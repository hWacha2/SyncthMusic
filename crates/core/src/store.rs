//! Хранилище реплики: SQLite + LWW-материализация дерева.
//!
//! Одна БД на устройство. `ops` — append-only лог репликации, `entries` —
//! материализованное состояние (путь → блоб + метаданные).

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::path::{Path, PathBuf};

use crate::hlc::Hlc;
use crate::op::{Op, OpKind};

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS ops (
    id          TEXT PRIMARY KEY,
    op_json     TEXT NOT NULL,
    hlc_ms      INTEGER NOT NULL,
    hlc_counter INTEGER NOT NULL,
    dev         TEXT NOT NULL,
    kind        TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS entries (
    path        TEXT PRIMARY KEY,
    blob        TEXT NOT NULL,
    size        INTEGER NOT NULL,
    mtime_ns    INTEGER NOT NULL,
    added_at    INTEGER NOT NULL,
    deleted     INTEGER NOT NULL DEFAULT 0,
    hlc_ms      INTEGER NOT NULL,
    hlc_counter INTEGER NOT NULL,
    dev         TEXT NOT NULL,
    -- Локальное (не реплицируемое) знание: файл реально лежит здесь.
    -- Нужно, чтобы «ещё не скачано» не путалось с «удалено».
    local_present INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_entries_blob ON entries(blob);
CREATE INDEX IF NOT EXISTS idx_entries_live ON entries(deleted) WHERE deleted = 0;

CREATE TABLE IF NOT EXISTS blobs (
    blob TEXT PRIMARY KEY,
    size INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_deletes (
    path        TEXT PRIMARY KEY,
    blob        TEXT NOT NULL,
    detected_at INTEGER NOT NULL
);
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRow {
    pub path: String,
    pub blob: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub added_at: u64,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDelete {
    pub path: String,
    pub blob: String,
    pub detected_at: u64,
}

pub struct Replica {
    conn: Connection,
    dev: String,
    data_dir: Option<PathBuf>,
}

impl Replica {
    /// Реплика в памяти — для тестов и `syncth doctor`.
    pub fn in_memory(dev: &str) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let mut replica = Self {
            conn,
            dev: dev.to_string(),
            data_dir: None,
        };
        replica.migrate()?;
        Ok(replica)
    }

    /// Реплика на диске. Каталог создаётся при необходимости.
    pub fn open(data_dir: &Path, dev: &str) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("не удалось создать {}", data_dir.display()))?;
        let conn = Connection::open(data_dir.join("library.db"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let mut replica = Self {
            conn,
            dev: dev.to_string(),
            data_dir: Some(data_dir.to_path_buf()),
        };
        replica.migrate()?;
        Ok(replica)
    }

    fn migrate(&mut self) -> Result<()> {
        self.conn.execute_batch(SCHEMA)?;
        self.set_meta("schema_version", "1")?;
        match self.get_meta("device_id")? {
            Some(stored) => {
                // Идентичность устройства стабильна: она важнее аргумента CLI.
                self.dev = stored;
            }
            None => self.set_meta("device_id", &self.dev)?,
        }
        Ok(())
    }

    pub fn device_id(&self) -> &str {
        &self.dev
    }

    pub fn data_dir(&self) -> Option<&Path> {
        self.data_dir.as_deref()
    }

    // ---------------------------------------------------------------- meta

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_root(&self, root: &Path) -> Result<()> {
        self.set_meta("root", &root.to_string_lossy())
    }

    pub fn root(&self) -> Result<Option<PathBuf>> {
        Ok(self.get_meta("root")?.map(PathBuf::from))
    }

    // ---------------------------------------------------------------- apply

    /// Применяет операцию. Возвращает `true`, если состояние изменилось.
    /// Идемпотентно: повторное применение того же `id` — no-op.
    pub fn apply(&mut self, op: &Op) -> Result<bool> {
        anyhow::ensure!(op.is_intact(), "операция повреждена: {}", op.id);

        let tx = self.conn.transaction()?;
        let known: Option<i64> = tx
            .query_row("SELECT 1 FROM ops WHERE id = ?1", params![op.id], |r| {
                r.get(0)
            })
            .optional()?;
        if known.is_some() {
            return Ok(false);
        }

        tx.execute(
            "INSERT INTO ops(id, op_json, hlc_ms, hlc_counter, dev, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                op.id,
                op.to_json(),
                op.hlc.ms as i64,
                op.hlc.counter as i64,
                op.dev,
                op.kind.name()
            ],
        )?;

        let changed = match &op.kind {
            OpKind::EntryPut {
                path,
                blob,
                size,
                mtime_ns,
                added_at,
            } => upsert_entry(&tx, op, path, blob, *size, *mtime_ns, *added_at)?,
            OpKind::EntryDel { path } => delete_entry(&tx, op, path)?,
            OpKind::EntryMove {
                from,
                to,
                blob,
                size,
                mtime_ns,
                added_at,
            } => {
                let a = delete_entry(&tx, op, from)?;
                let b = upsert_entry(&tx, op, to, blob, *size, *mtime_ns, *added_at)?;
                a || b
            }
            OpKind::BlobHave { blob, size } => {
                tx.execute(
                    "INSERT INTO blobs(blob, size) VALUES (?1, ?2)
                     ON CONFLICT(blob) DO NOTHING",
                    params![blob, *size as i64],
                )?;
                false
            }
        };

        tx.commit()?;
        Ok(changed)
    }

    pub fn apply_all(&mut self, ops: &[Op]) -> Result<usize> {
        let mut applied = 0;
        for op in ops {
            if self.apply(op)? {
                applied += 1;
            }
        }
        Ok(applied)
    }

    // ---------------------------------------------------------------- read

    pub fn ops_json(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT op_json FROM ops ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn ops(&self) -> Result<Vec<Op>> {
        self.ops_json()?.iter().map(|s| Op::from_json(s)).collect()
    }

    pub fn op_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM ops", [], |r| r.get::<_, i64>(0))? as u64)
    }

    /// Все записи, включая tombstone'ы (нужно для сверки реплик).
    ///
    /// Удалённая запись нормализуется (payload обнуляется): иначе «сначала put,
    /// потом del» и «сначала del, потом put» давали бы разные состояния при
    /// одинаковом наборе операций, и реплики не сходились бы.
    pub fn entries(&self) -> Result<Vec<EntryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, blob, size, mtime_ns, added_at, deleted
             FROM entries ORDER BY path",
        )?;
        let rows = stmt.query_map([], |r| {
            let deleted = r.get::<_, i64>(5)? != 0;
            Ok(normalize_row(EntryRow {
                path: r.get(0)?,
                blob: r.get(1)?,
                size: r.get(2)?,
                mtime_ns: r.get(3)?,
                added_at: r.get(4)?,
                deleted,
            }))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn live_entries(&self) -> Result<Vec<EntryRow>> {
        Ok(self.entries()?.into_iter().filter(|e| !e.deleted).collect())
    }

    /// Записи, которые физически есть на этом устройстве.
    pub fn local_entries(&self) -> Result<Vec<EntryRow>> {
        Ok(self
            .entries()?
            .into_iter()
            .filter(|e| !e.deleted && self.is_present(&e.path).unwrap_or(false))
            .collect())
    }

    pub fn is_present(&self, path: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT local_present FROM entries WHERE path = ?1",
                params![path],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|v| v != 0)
            .unwrap_or(false))
    }

    pub fn mark_present(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE entries SET local_present = 1 WHERE path = ?1 AND local_present = 0",
            params![path],
        )?;
        Ok(())
    }

    pub fn mark_absent(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE entries SET local_present = 0 WHERE path = ?1",
            params![path],
        )?;
        Ok(())
    }

    pub fn entry(&self, path: &str) -> Result<Option<EntryRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT path, blob, size, mtime_ns, added_at, deleted
                 FROM entries WHERE path = ?1",
                params![path],
                |r| {
                    let deleted = r.get::<_, i64>(5)? != 0;
                    Ok(normalize_row(EntryRow {
                        path: r.get(0)?,
                        blob: r.get(1)?,
                        size: r.get(2)?,
                        mtime_ns: r.get(3)?,
                        added_at: r.get(4)?,
                        deleted,
                    }))
                },
            )
            .optional()?)
    }

    pub fn total_live_bytes(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(size), 0) FROM entries WHERE deleted = 0",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Детерминированный отпечаток состояния. Главный инвариант: после обмена
    /// операциями у всех реплик одинаковый `state_hash`.
    pub fn state_hash(&self) -> Result<String> {
        let mut hasher = blake3::Hasher::new();
        for e in self.entries()? {
            hasher.update(
                format!(
                    "{}\0{}\0{}\0{}\0{}\0{}\n",
                    e.path, e.blob, e.size, e.mtime_ns, e.added_at, e.deleted as u8
                )
                .as_bytes(),
            );
        }
        Ok(format!("b3:{}", hasher.finalize().to_hex()))
    }

    // ---------------------------------------------------------------- pending deletes

    pub fn record_pending_delete(&self, path: &str, blob: &str, detected_at: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO pending_deletes(path, blob, detected_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(path) DO NOTHING",
            params![path, blob, detected_at as i64],
        )?;
        Ok(())
    }

    pub fn pending_deletes(&self) -> Result<Vec<PendingDelete>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, blob, detected_at FROM pending_deletes ORDER BY path")?;
        let rows = stmt.query_map([], |r| {
            Ok(PendingDelete {
                path: r.get(0)?,
                blob: r.get(1)?,
                detected_at: r.get::<_, i64>(2)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn clear_pending_delete(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM pending_deletes WHERE path = ?1", params![path])?;
        Ok(())
    }
}

/// Удалённая запись не хранит payload: он не определён однозначно.
fn normalize_row(mut e: EntryRow) -> EntryRow {
    if e.deleted {
        e.blob.clear();
        e.size = 0;
        e.mtime_ns = 0;
        e.added_at = 0;
    }
    e
}

/// LWW: побеждает строго больший `(hlc_ms, hlc_counter, device_id)`.
fn is_newer(tx: &Transaction<'_>, path: &str, op: &Op) -> Result<bool> {
    let current: Option<(i64, i64, String)> = tx
        .query_row(
            "SELECT hlc_ms, hlc_counter, dev FROM entries WHERE path = ?1",
            params![path],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;

    let incoming = (op.hlc.ms as i64, op.hlc.counter as i64, op.dev.clone());
    Ok(match current {
        Some((ms, c, dev)) => incoming > (ms, c, dev),
        None => true,
    })
}

fn upsert_entry(
    tx: &Transaction<'_>,
    op: &Op,
    path: &str,
    blob: &str,
    size: u64,
    mtime_ns: i64,
    added_at: u64,
) -> Result<bool> {
    if !is_newer(tx, path, op)? {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO entries(path, blob, size, mtime_ns, added_at, deleted, hlc_ms, hlc_counter, dev)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8)
         ON CONFLICT(path) DO UPDATE SET
             blob = excluded.blob,
             size = excluded.size,
             mtime_ns = excluded.mtime_ns,
             added_at = excluded.added_at,
             deleted = 0,
             hlc_ms = excluded.hlc_ms,
             hlc_counter = excluded.hlc_counter,
             dev = excluded.dev",
        params![
            path,
            blob,
            size as i64,
            mtime_ns,
            added_at as i64,
            op.hlc.ms as i64,
            op.hlc.counter as i64,
            op.dev
        ],
    )?;
    tx.execute(
        "INSERT INTO blobs(blob, size) VALUES (?1, ?2) ON CONFLICT(blob) DO NOTHING",
        params![blob, size as i64],
    )?;
    Ok(true)
}

fn delete_entry(tx: &Transaction<'_>, op: &Op, path: &str) -> Result<bool> {
    if !is_newer(tx, path, op)? {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO entries(path, blob, size, mtime_ns, added_at, deleted, hlc_ms, hlc_counter, dev)
         VALUES (?1, '', 0, 0, 0, 1, ?2, ?3, ?4)
         ON CONFLICT(path) DO UPDATE SET
             deleted = 1,
             hlc_ms = excluded.hlc_ms,
             hlc_counter = excluded.hlc_counter,
             dev = excluded.dev",
        params![path, op.hlc.ms as i64, op.hlc.counter as i64, op.dev],
    )?;
    Ok(true)
}

/// Обмен операциями между двумя репликами (для тестов и `syncth sync`).
pub fn exchange(a: &mut Replica, b: &mut Replica) -> Result<(usize, usize)> {
    let from_a = a.ops()?;
    let from_b = b.ops()?;
    let applied_in_b = b.apply_all(&from_a)?;
    let applied_in_a = a.apply_all(&from_b)?;
    Ok((applied_in_a, applied_in_b))
}

pub fn now_ms() -> u64 {
    Hlc::wall_ms()
}
