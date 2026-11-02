//! Хранилище реплики: SQLite + LWW-материализация дерева.
//!
//! Одна БД на устройство. `ops` — append-only лог репликации, `entries` —
//! материализованное состояние (путь → блоб + метаданные).

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::hlc::Hlc;
use crate::op::{Op, OpKind};
use crate::plan::{Decision, DecisionSet, PathDecision, Plan};

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

-- Для дельта-синка: «что у пира уже есть» по каждому устройству-автору.
CREATE INDEX IF NOT EXISTS ops_by_dev_hlc ON ops(dev, hlc_ms, hlc_counter);

-- TOFU: первый подписанный ключ устройства запоминается. Смена ключа у того же
-- `dev` отклоняется — иначе подпись не защищает от подмены автора.
CREATE TABLE IF NOT EXISTS dev_keys (
    dev TEXT PRIMARY KEY,
    pk  TEXT NOT NULL
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
    local_present INTEGER NOT NULL DEFAULT 0,
    -- Локальное исключение: «не хочу это здесь, но в общей библиотеке пусть
    -- останется». Не реплицируется, не влияет на state_hash.
    local_ignored INTEGER NOT NULL DEFAULT 0,
    -- Последний блоб, про который это устройство знает, что он согласован
    -- с пиром. Нужен, чтобы отличить последовательную правку тега от
    -- одновременной правки на двух устройствах. Локальное, не реплицируется.
    synced_blob TEXT NOT NULL DEFAULT ''
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

-- Корзина: hardlink на блоб в data_dir/vault (0 байт на NTFS).
CREATE TABLE IF NOT EXISTS vault (
    blob       TEXT NOT NULL,
    path       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    mtime_ns   INTEGER NOT NULL,
    added_at   INTEGER NOT NULL,
    deleted_at INTEGER NOT NULL,
    PRIMARY KEY (blob, path)
);

CREATE INDEX IF NOT EXISTS idx_vault_path ON vault(path);

-- Сопряжённые устройства: адрес запоминается, чтобы не искать друг друга
-- по UDP каждый раз и чтобы работал запуск без discovery.
CREATE TABLE IF NOT EXISTS peers (
    dev      TEXT PRIMARY KEY,
    addr     TEXT NOT NULL,
    added_at INTEGER NOT NULL
);

-- Замороженный план сеанса: один на пару устройств. Живёт 15 минут и
-- перестраивается при новом скане или изменении блоба.
CREATE TABLE IF NOT EXISTS sync_plans (
    peer_dev   TEXT PRIMARY KEY,
    plan_json  TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

-- Решения человека по путям. Не операции журнала: своя таблица, своя жизнь.
-- `blob` фиксирует содержимое, к которому относится решение: если блоб
-- изменился или путь исчез, решение больше не применяется.
CREATE TABLE IF NOT EXISTS plan_decisions (
    peer_dev    TEXT NOT NULL,
    path        TEXT NOT NULL,
    action      TEXT NOT NULL,
    blob        TEXT NOT NULL,
    hlc_ms      INTEGER NOT NULL,
    hlc_counter INTEGER NOT NULL,
    dev         TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (peer_dev, path)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultRow {
    pub blob: String,
    pub path: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub added_at: u64,
    pub deleted_at: u64,
}

pub struct Replica {
    conn: Connection,
    dev: String,
    name: String,
    data_dir: Option<PathBuf>,
    key: SigningKey,
}

/// Сопряжённое устройство. `name` — понятное имя, если пир его сообщил.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub dev: String,
    pub addr: String,
    pub name: String,
}

/// Полное состояние пути для построения плана. В отличие от `EntryRow`
/// здесь видны локальные флаги и согласованная база `synced_blob`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRow {
    pub path: String,
    pub blob: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub added_at: u64,
    pub deleted: bool,
    pub present: bool,
    pub ignored: bool,
    pub synced_blob: String,
}

impl Replica {
    /// Реплика в памяти — для тестов и `syncth doctor`.
    pub fn in_memory(dev: &str) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut replica = Self {
            conn,
            dev: dev.to_string(),
            name: crate::device_name(),
            data_dir: None,
            key: load_or_create_key(None)?,
        };
        replica.migrate()?;
        Ok(replica)
    }

    /// Реплика на диске. Каталог создаётся при необходимости.
    pub fn open(data_dir: &Path, dev: &str) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("не удалось создать {}", data_dir.display()))?;
        let conn = Connection::open(data_dir.join("library.db"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let mut replica = Self {
            conn,
            dev: dev.to_string(),
            name: crate::device_name(),
            data_dir: Some(data_dir.to_path_buf()),
            key: load_or_create_key(Some(data_dir))?,
        };
        replica.migrate()?;
        Ok(replica)
    }

    fn migrate(&mut self) -> Result<()> {
        self.conn.execute_batch(SCHEMA)?;
        // БД, созданные до появления локальных исключений.
        if !self.has_column("entries", "local_ignored")? {
            self.conn.execute(
                "ALTER TABLE entries ADD COLUMN local_ignored INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !self.has_column("entries", "synced_blob")? {
            self.conn.execute(
                "ALTER TABLE entries ADD COLUMN synced_blob TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        if !self.has_column("peers", "name")? {
            self.conn.execute(
                "ALTER TABLE peers ADD COLUMN name TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        self.set_meta("schema_version", "5")?;
        match self.get_meta("device_id")? {
            Some(stored) => {
                // Идентичность устройства стабильна: она важнее аргумента CLI.
                self.dev = stored;
            }
            None => self.set_meta("device_id", &self.dev)?,
        }
        // Имя не входит в идентичность: его можно менять без миграции данных.
        // Всегда берём текущее системное имя, чтобы переименование ПК было видно.
        self.name = crate::device_name();
        self.set_meta("device_name", &self.name)?;
        Ok(())
    }

    pub fn device_id(&self) -> &str {
        &self.dev
    }

    /// Понятное имя этого устройства. Отличается от `device_id`.
    pub fn device_name(&self) -> &str {
        &self.name
    }

    /// Задать имя явно (например, модель телефона вместо пустого hostname).
    pub fn set_device_name(&mut self, name: &str) -> Result<()> {
        let name = crate::sanitize_name(name);
        self.name = name.clone();
        self.set_meta("device_name", &name)?;
        Ok(())
    }

    pub fn data_dir(&self) -> Option<&Path> {
        self.data_dir.as_deref()
    }

    fn has_column(&self, table: &str, column: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
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

    // ---------------------------------------------------------------- token

    /// Общий секрет библиотеки. Хранится локально, передаётся второму
    /// устройству один раз при сопряжении.
    pub fn ensure_token(&self) -> Result<String> {
        match self.get_meta("token")? {
            Some(t) if !t.is_empty() => Ok(t),
            _ => self.rotate_token(),
        }
    }

    pub fn rotate_token(&self) -> Result<String> {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("нет источника случайности: {e}"))?;
        let token: String = buf.iter().map(|b| format!("{b:02x}")).collect();
        self.set_meta("token", &token)?;
        Ok(token)
    }

    /// Принять токен другого устройства при пейринге. Не генерируем новый:
    /// библиотека — это общий секрет, а не две независимые.
    pub fn set_token(&self, token: &str) -> Result<()> {
        anyhow::ensure!(!token.is_empty(), "пустой токен библиотеки");
        self.set_meta("token", token)
    }

    // ---------------------------------------------------------------- peers

    pub fn add_peer(&self, dev: &str, addr: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO peers(dev, addr, added_at, name) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(dev) DO UPDATE SET addr = excluded.addr,
                 name = CASE WHEN excluded.name <> '' THEN excluded.name ELSE peers.name END",
            params![dev, addr, Hlc::wall_ms() as i64, name],
        )?;
        Ok(())
    }

    pub fn known_peers(&self) -> Result<Vec<Peer>> {
        let mut stmt = self
            .conn
            .prepare("SELECT dev, addr, name FROM peers ORDER BY dev")?;
        let rows = stmt.query_map([], |r| {
            Ok(Peer {
                dev: r.get(0)?,
                addr: r.get(1)?,
                name: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Забыть устройство. Токен библиотеки не меняется: «забыть» — это убрать
    /// сохранённый адрес, а не сменить общий секрет для остальных пиров.
    pub fn remove_peer(&self, dev: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM peers WHERE dev = ?1", params![dev])?
            > 0)
    }

    // ---------------------------------------------------------------- plan

    /// Все пути с локальными флагами — вход для построения плана.
    pub fn plan_rows(&self) -> Result<Vec<PlanRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, blob, size, mtime_ns, added_at, deleted,
                    local_present, local_ignored, synced_blob
             FROM entries ORDER BY path",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(PlanRow {
                path: r.get(0)?,
                blob: r.get(1)?,
                size: r.get::<_, i64>(2)? as u64,
                mtime_ns: r.get(3)?,
                added_at: r.get::<_, i64>(4)? as u64,
                deleted: r.get::<_, i64>(5)? != 0,
                present: r.get::<_, i64>(6)? != 0,
                ignored: r.get::<_, i64>(7)? != 0,
                synced_blob: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Сохранить замороженный план. Решения в JSON не входят: они живут
    /// отдельно и подмешиваются при загрузке.
    pub fn save_plan(&self, plan: &Plan) -> Result<()> {
        let json = serde_json::to_string(plan)?;
        self.conn.execute(
            "INSERT INTO sync_plans(peer_dev, plan_json, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(peer_dev) DO UPDATE SET
                 plan_json = excluded.plan_json,
                 created_at = excluded.created_at,
                 expires_at = excluded.expires_at",
            params![
                plan.peer_dev,
                json,
                plan.created_at as i64,
                plan.expires_at as i64
            ],
        )?;
        Ok(())
    }

    /// Загрузить план, если он ещё не истёк. Просроченный план удаляется и
    /// не возвращается: применять его нельзя.
    pub fn load_plan(&self, peer_dev: &str, now: u64) -> Result<Option<Plan>> {
        let row: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT plan_json, expires_at FROM sync_plans WHERE peer_dev = ?1",
                params![peer_dev],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((json, expires_at)) = row else {
            return Ok(None);
        };
        if now >= expires_at as u64 {
            self.clear_plan(peer_dev)?;
            return Ok(None);
        }
        let mut plan: Plan = serde_json::from_str(&json)?;
        let mut decisions = self.load_decisions(peer_dev)?;
        decisions.retain_valid(&plan);
        plan.decisions = decisions;
        Ok(Some(plan))
    }

    pub fn clear_plan(&self, peer_dev: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM sync_plans WHERE peer_dev = ?1", params![peer_dev])?
            > 0)
    }

    pub fn clear_all_plans(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM sync_plans", [])?)
    }

    pub fn load_decisions(&self, peer_dev: &str) -> Result<DecisionSet> {
        let mut stmt = self.conn.prepare(
            "SELECT path, action, blob, hlc_ms, hlc_counter, dev
             FROM plan_decisions WHERE peer_dev = ?1 ORDER BY path",
        )?;
        let rows = stmt.query_map(params![peer_dev], |r| {
            let action: String = r.get(1)?;
            Ok(PathDecision {
                path: r.get(0)?,
                action: Decision::parse(&action).unwrap_or(Decision::Keep),
                blob: r.get(2)?,
                hlc: Hlc::new(r.get::<_, i64>(3)? as u64, r.get::<_, i64>(4)? as u32),
                dev: r.get(5)?,
            })
        })?;
        let mut set = DecisionSet::default();
        for decision in rows {
            let decision = decision?;
            set.entries.insert(decision.path.clone(), decision);
        }
        Ok(set)
    }

    /// Записать набор решений целиком. Вызывается после ручного изменения
    /// или после слияния с пиром.
    pub fn save_decisions(&mut self, peer_dev: &str, set: &DecisionSet) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM plan_decisions WHERE peer_dev = ?1",
            params![peer_dev],
        )?;
        for decision in set.entries.values() {
            tx.execute(
                "INSERT INTO plan_decisions(peer_dev, path, action, blob, hlc_ms, hlc_counter, dev, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    peer_dev,
                    decision.path,
                    decision.action.as_str(),
                    decision.blob,
                    decision.hlc.ms as i64,
                    decision.hlc.counter as i64,
                    decision.dev,
                    Hlc::wall_ms() as i64
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Слить решения пира с локальными по HLC и сохранить результат.
    /// Возвращает итоговый набор и число изменённых путей.
    pub fn merge_decisions(
        &mut self,
        peer_dev: &str,
        incoming: &DecisionSet,
    ) -> Result<(DecisionSet, usize)> {
        let mut merged = self.load_decisions(peer_dev)?;
        let changed = merged.merge(incoming);
        if changed > 0 {
            self.save_decisions(peer_dev, &merged)?;
        }
        Ok((merged, changed))
    }

    pub fn clear_decisions(&self, peer_dev: &str) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM plan_decisions WHERE peer_dev = ?1", params![peer_dev])?)
    }

    // ---------------------------------------------------------------- clock

    /// Часы реплики. Хранятся в БД, иначе перезапуск процесса с «уехавшими
    /// назад» системными часами выпустил бы операции со старым HLC.
    fn load_hlc(&self) -> Result<Hlc> {
        Ok(match self.get_meta("hlc")? {
            Some(s) => match s.split_once('.') {
                Some((ms, c)) => Hlc::new(ms.parse().unwrap_or(0), c.parse().unwrap_or(0)),
                None => Hlc::default(),
            },
            None => Hlc::default(),
        })
    }

    fn save_hlc(&self, hlc: &Hlc) -> Result<()> {
        self.set_meta("hlc", &format!("{}.{}", hlc.ms, hlc.counter))
    }

    /// Следующая метка для локальной операции.
    pub fn next_hlc(&mut self) -> Result<Hlc> {
        let mut clock = self.load_hlc()?;
        let ticked = clock.tick();
        self.save_hlc(&ticked)?;
        Ok(ticked)
    }

    /// Новая локальная операция: канонический путь, свежий HLC, подпись.
    /// Единственный правильный способ порождать `entry_*` на этом устройстве.
    pub fn make_op(&mut self, kind: OpKind) -> Result<Op> {
        let kind = Op::canonicalize(kind)?;
        let mut op = Op::new(self.dev.clone(), self.next_hlc()?, kind);
        op.sign(&self.key);
        Ok(op)
    }

    /// Публичный ключ этого устройства (hex).
    pub fn public_key(&self) -> String {
        hex::encode(self.key.verifying_key().to_bytes())
    }

    /// Подтянуть часы после чужой операции: причинность важнее настенных часов.
    pub fn observe_hlc(&mut self, remote: Hlc) -> Result<()> {
        let mut clock = self.load_hlc()?;
        clock.observe(remote);
        self.save_hlc(&clock)
    }

    // ---------------------------------------------------------------- apply

    /// TOFU-проверка автора: у одного `dev` не может появиться второй ключ.
    fn check_author_key(&self, op: &Op) -> Result<()> {
        let Some(pk) = &op.pk else {
            return Ok(());
        };
        let known: Option<String> = self
            .conn
            .query_row(
                "SELECT pk FROM dev_keys WHERE dev = ?1",
                params![op.dev],
                |r| r.get(0),
            )
            .optional()?;
        match known {
            Some(known) => anyhow::ensure!(
                known == *pk,
                "устройство {} уже подписано другим ключом",
                op.dev
            ),
            None => {
                self.conn.execute(
                    "INSERT INTO dev_keys(dev, pk) VALUES (?1, ?2)",
                    params![op.dev, pk],
                )?;
            }
        }
        Ok(())
    }

    /// Применяет операцию. Возвращает `true`, если состояние изменилось.
    /// Идемпотентно: повторное применение того же `id` — no-op.
    pub fn apply(&mut self, op: &Op) -> Result<bool> {
        anyhow::ensure!(op.is_intact(), "операция повреждена: {}", op.id);
        anyhow::ensure!(op.verify_signature(), "неверная подпись операции {}", op.id);
        self.check_author_key(op)?;
        self.observe_hlc(op.hlc)?;

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

    /// Верхняя граница журнала по каждому устройству-автору.
    ///
    /// Журнал одного автора строго упорядочен его HLC, поэтому пары
    /// `(dev → последний HLC)` достаточно, чтобы описать «что у меня уже есть»
    /// и попросить только хвост. Глобального максимума мало: устройство может
    /// создать операцию с большим HLC, не увидев чужую с меньшим.
    pub fn watermarks(&self) -> Result<HashMap<String, (i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT dev, hlc_ms, hlc_counter FROM ops
             ORDER BY dev, hlc_ms, hlc_counter",
        )?;
        let mut rows = stmt.query([])?;
        let mut map = HashMap::new();
        while let Some(row) = rows.next()? {
            map.insert(row.get::<_, String>(0)?, (row.get(1)?, row.get(2)?));
        }
        Ok(map)
    }

    /// Операции, которых нет у пира согласно его `watermarks`.
    pub fn ops_since(&self, known: &HashMap<String, (i64, i64)>) -> Result<Vec<Op>> {
        if known.is_empty() {
            return self.ops();
        }

        // `dev NOT IN (...)` — авторы, о которых пир вообще не знает;
        // дальше по каждому известному автору берём строго больший HLC.
        let mut clauses = Vec::with_capacity(known.len() + 1);
        clauses.push(format!("dev NOT IN ({})", vec!["?"; known.len()].join(",")));
        let mut args: Vec<Value> = known.keys().cloned().map(Value::Text).collect();
        for (dev, (ms, counter)) in known {
            clauses.push("(dev = ? AND (hlc_ms > ? OR (hlc_ms = ? AND hlc_counter > ?)))".into());
            args.push(Value::Text(dev.clone()));
            args.push(Value::Integer(*ms));
            args.push(Value::Integer(*ms));
            args.push(Value::Integer(*counter));
        }

        let sql = format!(
            "SELECT op_json FROM ops WHERE {} ORDER BY rowid",
            clauses.join(" OR ")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |r| {
            r.get::<_, String>(0)
        })?;
        let mut ops = Vec::new();
        for json in rows {
            ops.push(Op::from_json(&json?)?);
        }
        Ok(ops)
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

    /// Локальное исключение: запись остаётся в общей библиотеке, но на этом
    /// устройстве её не хотят видеть и не будут докачивать.
    pub fn set_ignored(&self, path: &str, ignored: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE entries SET local_ignored = ?2, local_present = 0 WHERE path = ?1",
            params![path, ignored as i64],
        )?;
        Ok(())
    }

    pub fn is_ignored(&self, path: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT local_ignored FROM entries WHERE path = ?1",
                params![path],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|v| v != 0)
            .unwrap_or(false))
    }

    /// Последнее состояние, про которое известно, что оно есть у пира.
    /// Пустая строка — базы ещё нет (файл никогда не синхронизировался).
    pub fn synced_blob(&self, path: &str) -> Result<String> {
        Ok(self
            .conn
            .query_row(
                "SELECT synced_blob FROM entries WHERE path = ?1",
                params![path],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_default())
    }

    /// Вызывается в конце успешного сеанса: обе стороны обменялись операциями
    /// и всеми нужными блобами, значит текущее содержимое — согласованная база.
    pub fn confirm_synced(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE entries SET synced_blob = blob
             WHERE deleted = 0 AND local_present = 1 AND synced_blob <> blob",
            [],
        )?)
    }

    /// То же, но только для выбранных путей: commit не должен объявлять
    /// согласованными отклонённые изменения и отложенные удаления.
    pub fn confirm_synced_paths(&self, paths: &[String]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut changed = 0;
        for path in paths {
            changed += tx.execute(
                "UPDATE entries SET synced_blob = blob
                 WHERE path = ?1 AND deleted = 0 AND local_present = 1 AND synced_blob <> blob",
                params![path],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn ignored_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM entries WHERE local_ignored = 1 ORDER BY path")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Все живые пути, ссылающиеся на блоб. Нужно, чтобы «удалить навсегда»
    /// не оставило копию под другим именем после переноса.
    pub fn live_paths_by_blob(&self, blob: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM entries WHERE blob = ?1 AND deleted = 0 ORDER BY path")?;
        let rows = stmt.query_map(params![blob], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Пары (путь, блоб) для подтверждённых удалений, которые ещё лежат на
    /// диске. Блоб здесь не нормализуется: по нему проверяется локальная копия.
    pub fn deleted_present(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, blob FROM entries
             WHERE deleted = 1 AND local_present = 1 AND blob <> ''
             ORDER BY path",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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

    /// Полные метаданные записи, даже если она удалена. Нужно корзине:
    /// после tombstone `entry()` намеренно прячет payload.
    pub fn entry_raw(&self, path: &str) -> Result<Option<(String, u64, i64, u64)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT blob, size, mtime_ns, added_at FROM entries WHERE path = ?1",
                params![path],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)? as u64,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)? as u64,
                    ))
                },
            )
            .optional()?)
    }

    /// Пути с конфликтными копиями — для UI.
    pub fn conflict_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT path FROM entries
             WHERE deleted = 0 AND path LIKE '%.sync-conflict-%'
             ORDER BY path",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ---------------------------------------------------------------- vault

    pub fn add_vault_row(&self, row: &VaultRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO vault(blob, path, size, mtime_ns, added_at, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(blob, path) DO UPDATE SET
                 size = excluded.size,
                 mtime_ns = excluded.mtime_ns,
                 added_at = excluded.added_at,
                 deleted_at = excluded.deleted_at",
            params![
                row.blob,
                row.path,
                row.size as i64,
                row.mtime_ns,
                row.added_at as i64,
                row.deleted_at as i64
            ],
        )?;
        Ok(())
    }

    pub fn vault_rows(&self) -> Result<Vec<VaultRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT blob, path, size, mtime_ns, added_at, deleted_at
             FROM vault ORDER BY deleted_at DESC, path",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(VaultRow {
                blob: r.get(0)?,
                path: r.get(1)?,
                size: r.get::<_, i64>(2)? as u64,
                mtime_ns: r.get(3)?,
                added_at: r.get::<_, i64>(4)? as u64,
                deleted_at: r.get::<_, i64>(5)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn vault_for_path(&self, path: &str) -> Result<Option<VaultRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT blob, path, size, mtime_ns, added_at, deleted_at
             FROM vault WHERE path = ?1 ORDER BY deleted_at DESC LIMIT 1",
        )?;
        Ok(stmt
            .query_row(params![path], |r| {
                Ok(VaultRow {
                    blob: r.get(0)?,
                    path: r.get(1)?,
                    size: r.get::<_, i64>(2)? as u64,
                    mtime_ns: r.get(3)?,
                    added_at: r.get::<_, i64>(4)? as u64,
                    deleted_at: r.get::<_, i64>(5)? as u64,
                })
            })
            .optional()?)
    }

    pub fn remove_vault_row(&self, blob: &str, path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM vault WHERE blob = ?1 AND path = ?2",
            params![blob, path],
        )?;
        Ok(())
    }

    pub fn vault_refs(&self, blob: &str) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM vault WHERE blob = ?1",
            params![blob],
            |r| r.get::<_, i64>(0),
        )? as usize)
    }

    pub fn vault_rows_before(&self, ts: u64) -> Result<Vec<VaultRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT blob, path, size, mtime_ns, added_at, deleted_at
             FROM vault WHERE deleted_at < ?1 ORDER BY deleted_at",
        )?;
        let rows = stmt.query_map(params![ts as i64], |r| {
            Ok(VaultRow {
                blob: r.get(0)?,
                path: r.get(1)?,
                size: r.get::<_, i64>(2)? as u64,
                mtime_ns: r.get(3)?,
                added_at: r.get::<_, i64>(4)? as u64,
                deleted_at: r.get::<_, i64>(5)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
        "INSERT INTO entries(path, blob, size, mtime_ns, added_at, deleted, hlc_ms, hlc_counter, dev, local_present, local_ignored)
         VALUES (?1, '', 0, 0, 0, 1, ?2, ?3, ?4, 0, 0)
         ON CONFLICT(path) DO UPDATE SET
             deleted = 1,
             -- local_present не трогаем: файл ещё лежит на диске и должен быть
             -- удалён в prune_deleted_files (или спасён как conflict-copy).
             local_ignored = 0,
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

/// Ключ устройства: `data_dir/device.key` (32 байта). Создаётся один раз.
/// Для тестовых реплик в памяти ключ эфемерный.
fn load_or_create_key(data_dir: Option<&Path>) -> Result<SigningKey> {
    let Some(dir) = data_dir else {
        return random_key();
    };
    let path = dir.join("device.key");
    if path.is_file() {
        let raw = fs::read(&path).with_context(|| format!("чтение {}", path.display()))?;
        let bytes = <[u8; 32]>::try_from(raw.as_slice())
            .map_err(|_| anyhow::anyhow!("повреждён ключ устройства: {}", path.display()))?;
        return Ok(SigningKey::from_bytes(&bytes));
    }
    let key = random_key()?;
    fs::write(&path, key.to_bytes()).with_context(|| format!("запись {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("права на {}", path.display()))?;
    }
    Ok(key)
}

fn random_key() -> Result<SigningKey> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("нет доступа к генератору: {e}"))?;
    Ok(SigningKey::from_bytes(&buf))
}

pub fn now_ms() -> u64 {
    Hlc::wall_ms()
}
