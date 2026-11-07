//! Хранилище реплики: SQLite + LWW-материализация дерева.
//!
//! Одна БД на устройство. `ops` — append-only лог репликации, `entries` —
//! материализованное состояние (путь → блоб + метаданные).

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
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
    kind        TEXT NOT NULL,
    -- Путь, которого касается операция. Нужен, чтобы забыть собственное
    -- добавление, не разбирая JSON всего журнала.
    path        TEXT NOT NULL DEFAULT ''
);

-- Для дельта-синка: «что у пира уже есть» по каждому устройству-автору.
-- Индекс по пути создаётся в migrate(): в старых БД колонки ops.path ещё нет,
-- и CREATE INDEX здесь падал бы раньше, чем успевала пройти миграция.
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

-- Кэш содержимого: путь + размер + mtime_ns → блоб. Это не история
-- синхронизации, а локальное знание о файлах: таблица переживает
-- `reset_history`, чтобы после сброса не читать всю библиотеку заново.
CREATE TABLE IF NOT EXISTS content_cache (
    path     TEXT PRIMARY KEY,
    size     INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    blob     TEXT NOT NULL,
    seen_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_deletes (
    path        TEXT PRIMARY KEY,
    blob        TEXT NOT NULL,
    detected_at INTEGER NOT NULL,
    -- Полный HLC момента обнаружения: без счётчика синтетическое удаление
    -- в preview могло оказаться не новее собственной операции за ту же мс.
    hlc_counter INTEGER NOT NULL DEFAULT 0
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

-- Пути, которые человек вывел из синхронизации кнопкой «оставить только на
-- одной стороне». Это намерение, а не журнал: таблица не реплицируется и
-- переживает сброс истории. `side` — где файл остаётся относительно этого
-- устройства: `local` или `remote`.
CREATE TABLE IF NOT EXISTS exclusions (
    path       TEXT PRIMARY KEY,
    side       TEXT NOT NULL,
    blob       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    mtime_ns   INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

-- Путь, который после отмены исключения возвращается в синхронизацию.
-- `side` говорит, где лежит уцелевшая копия: `local` — на этом устройстве,
-- `remote` — у пира. Пока файла здесь нет, скан не должен записывать его в
-- `pending_deletes`, а план обязан показать добавление с той стороны.
CREATE TABLE IF NOT EXISTS resurrections (
    path       TEXT PRIMARY KEY,
    side       TEXT NOT NULL,
    blob       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    mtime_ns   INTEGER NOT NULL,
    created_at INTEGER NOT NULL
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

/// Состояние пути, нужное сканеру: метаданные записи плюс локальные флаги.
/// Читается один раз на скан, чтобы не делать SELECT на каждый файл.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRow {
    pub path: String,
    pub blob: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub added_at: u64,
    pub deleted: bool,
    pub present: bool,
    pub ignored: bool,
    pub dev: String,
    pub synced_blob: String,
}

/// Перенос, уже записанный в локальный журнал. Нужен плану, чтобы отличить
/// переименование от добавления: в `entries` от переноса остаются только
/// «старый путь удалён» и «новый путь жив», а связь между ними хранится в
/// операции `entry_move`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMove {
    pub from: String,
    pub to: String,
    pub blob: String,
    pub hlc: Hlc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDelete {
    pub path: String,
    pub blob: String,
    pub detected_at: u64,
    pub counter: u32,
}

impl PendingDelete {
    /// Мягкое удаление как HLC: та же миллисекунда, но сохранённый счётчик.
    pub fn hlc(&self) -> Hlc {
        Hlc::new(self.detected_at, self.counter)
    }
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

/// Путь, выведенный из синхронизации по решению человека. `side` говорит, на
/// какой стороне файл остаётся: `local` — на этом устройстве, `remote` — на
/// сопряжённом. Значение не зависит от того, кто именно принял решение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExclusionRow {
    pub path: String,
    pub side: String,
    pub blob: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub created_at: u64,
}

/// Путь, возвращаемый в синхронизацию после отмены исключения. `side` —
/// сторона с уцелевшей копией: `local` или `remote`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResurrectionRow {
    pub path: String,
    pub side: String,
    pub blob: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub created_at: u64,
}

pub struct Replica {
    conn: Connection,
    dev: String,
    name: String,
    data_dir: Option<PathBuf>,
    /// Общая с другими соединениями к этой же библиотеке блокировка.
    /// См. `crate::lock`.
    lock: crate::lock::LibraryLock,
    key: SigningKey,
    /// Часы в памяти. Читать и писать `meta` на каждый файл слишком дорого:
    /// при долгом сканировании это тысячи лишних транзакций.
    hlc: Cell<Option<Hlc>>,
    hlc_dirty: Cell<bool>,
    /// Вести ли таблицу исключений. Включается только на ПК: именно он
    /// показывает список исключений и умеет их отменять. Телефон остаётся
    /// минимальным агентом и продолжает слать свой журнал как раньше.
    exclusions: bool,
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
    /// HLC операции, которая сформировала текущее состояние пути. По нему
    /// видно, устарела ли операция пира: журнал пира может содержать давние
    /// записи, которые до нас так и не доехали.
    pub hlc: Hlc,
    /// Автор этой операции. Нужен для разрешения ничьей по HLC так же, как
    /// в `is_newer` при применении операций.
    pub dev: String,
}

impl Drop for Replica {
    /// Часы живут в памяти, поэтому при закрытии реплики дописываем их в БД.
    /// Ошибку здесь вернуть некому: при следующем открытии часы всё равно
    /// поднимутся по системному времени.
    fn drop(&mut self) {
        let _ = self.flush_hlc();
    }
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
            lock: crate::lock::ephemeral(),
            key: load_or_create_key(None)?,
            hlc: Cell::new(None),
            hlc_dirty: Cell::new(false),
            exclusions: false,
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
            lock: crate::lock::for_data_dir(data_dir),
            key: load_or_create_key(Some(data_dir))?,
            hlc: Cell::new(None),
            hlc_dirty: Cell::new(false),
            exclusions: false,
        };
        // Открытие и миграция схемы тоже не должны пересекаться с другой
        // репликой того же каталога (агент, входящий сеанс, UI).
        let lock = replica.lock.clone();
        {
            let _guard = lock.lock();
            replica.migrate()?;
        }
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
        if !self.has_column("pending_deletes", "hlc_counter")? {
            self.conn.execute(
                "ALTER TABLE pending_deletes ADD COLUMN hlc_counter INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !self.has_column("ops", "path")? {
            self.conn.execute(
                "ALTER TABLE ops ADD COLUMN path TEXT NOT NULL DEFAULT ''",
                [],
            )?;
            self.backfill_op_paths()?;
        }
        // Только после появления колонки: на старых БД её ещё нет.
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS ops_by_path ON ops(dev, path)",
            [],
        )?;
        if self.get_meta("schema_version")?.as_deref() != Some("6") {
            self.set_meta("schema_version", "6")?;
        }
        match self.get_meta("device_id")? {
            Some(stored) => {
                // Идентичность устройства стабильна: она важнее аргумента CLI.
                self.dev = stored;
            }
            None => self.set_meta("device_id", &self.dev)?,
        }
        // Имя не входит в идентичность: его можно менять без миграции данных.
        // Записанное имя не перезаписываем при каждом открытии: лишние
        // записи в SQLite мешают долгому сканированию на телефоне.
        match self.get_meta("device_name")? {
            Some(stored) if !stored.is_empty() => self.name = stored,
            _ => {
                self.name = crate::device_name();
                self.set_meta("device_name", &self.name)?;
            }
        }
        Ok(())
    }

    pub fn device_id(&self) -> &str {
        &self.dev
    }

    /// Блокировка библиотеки, общая для всех соединений к тому же каталогу.
    /// Клонируется, чтобы держать guard, пока `Replica` используется mutably.
    pub fn library_lock(&self) -> crate::lock::LibraryLock {
        self.lock.clone()
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

    /// Заполнить `ops.path` для журнала, созданного до появления колонки.
    fn backfill_op_paths(&mut self) -> Result<()> {
        let rows: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id, op_json FROM ops WHERE path = ''")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for (id, json) in rows {
            let Ok(op) = Op::from_json(&json) else {
                continue;
            };
            tx.execute(
                "UPDATE ops SET path = ?2 WHERE id = ?1",
                params![id, op_path(&op.kind)],
            )?;
        }
        tx.commit()?;
        Ok(())
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
        let value = root.to_string_lossy();
        // Кэш содержимого привязан к путям относительно корня. При смене
        // библиотеки старые пути к новой не относятся — начинаем кэш заново.
        if self.get_meta("root")?.as_deref() != Some(value.as_ref()) {
            self.conn.execute("DELETE FROM content_cache", [])?;
        }
        self.set_meta("root", &value)
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

    /// Есть ли вообще сопряжённые устройства. Без них «мягкое удаление»
    /// никому не нужно: расходиться не с кем.
    pub fn has_peers(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM peers LIMIT 1", [], |r| r.get::<_, i64>(0))
            .optional()?
            .is_some())
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

    /// Прочитать несколько таблиц как одно состояние.
    ///
    /// SQLite в режиме autocommit даёт каждому `SELECT` свой снимок. Плану
    /// нужны `entries`, `pending_deletes`, `exclusions`, `ops` и `removal_hlcs`
    /// из одного момента: иначе перенос может попасть в `entries`, но ещё не
    /// попасть в `ops`, и новый путь станет «добавлением». `BEGIN DEFERRED`
    /// фиксирует снимок на первом чтении; запись внутри замыкания запрещена.
    pub fn read_snapshot<T>(&self, read: impl FnOnce() -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN DEFERRED")?;

        /// Закрывает читающую транзакцию на любом выходе, включая ошибку.
        struct Guard<'a>(&'a rusqlite::Connection);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                // Транзакция только читающая: commit не меняет данные.
                let _ = self.0.execute_batch("COMMIT");
            }
        }

        let guard = Guard(&self.conn);
        let result = read();
        drop(guard);
        result
    }

    /// Все пути с локальными флагами — вход для построения плана.
    pub fn plan_rows(&self) -> Result<Vec<PlanRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, blob, size, mtime_ns, added_at, deleted,
                    local_present, local_ignored, synced_blob, hlc_ms, hlc_counter, dev
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
                hlc: Hlc::new(r.get::<_, i64>(9)? as u64, r.get::<_, u32>(10)?),
                dev: r.get(11)?,
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
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
    ///
    /// Внутри скана часы держим в памяти и пишем один раз в конце: запись в
    /// `meta` на каждый файл — это отдельная транзакция на ровном месте.
    fn load_hlc(&self) -> Result<Hlc> {
        if let Some(cached) = self.hlc.get() {
            return Ok(cached);
        }
        let clock = match self.get_meta("hlc")? {
            Some(s) => match s.split_once('.') {
                Some((ms, c)) => Hlc::new(ms.parse().unwrap_or(0), c.parse().unwrap_or(0)),
                None => Hlc::default(),
            },
            None => Hlc::default(),
        };
        self.hlc.set(Some(clock));
        Ok(clock)
    }

    fn save_hlc(&self, hlc: &Hlc) {
        self.hlc.set(Some(*hlc));
        self.hlc_dirty.set(true);
    }

    /// Сохранить часы в БД, если с прошлого раза они менялись.
    pub fn flush_hlc(&self) -> Result<()> {
        if !self.hlc_dirty.get() {
            return Ok(());
        }
        let Some(clock) = self.hlc.get() else {
            return Ok(());
        };
        self.set_meta("hlc", &format!("{}.{}", clock.ms, clock.counter))?;
        self.hlc_dirty.set(false);
        Ok(())
    }

    /// Следующая метка для локальной операции.
    pub fn next_hlc(&mut self) -> Result<Hlc> {
        let mut clock = self.load_hlc()?;
        let ticked = clock.tick();
        self.save_hlc(&ticked);
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

    /// Операция с заданным HLC. Нужна для синтетических preview-удалений:
    /// их метка — момент обнаружения, а не текущие часы.
    pub fn make_op_at(&mut self, hlc: Hlc, kind: OpKind) -> Result<Op> {
        let kind = Op::canonicalize(kind)?;
        let mut op = Op::new(self.dev.clone(), hlc, kind);
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
        self.save_hlc(&clock);
        // Чужие операции приходят пачками в сеансе, но часы после них должны
        // быть видны другим соединениям сразу: иначе два сеанса могут выдать
        // операции с одинаковым HLC.
        self.flush_hlc()
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

        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = apply_in_tx(&tx, op)?;
        tx.commit()?;
        Ok(changed)
    }

    /// Применяет пачку собственных операций одной транзакцией. Скан порождает
    /// сотни `EntryPut`, и отдельная транзакция на каждый — заметная доля
    /// времени на телефоне.
    ///
    /// Часы здесь не наблюдают: все операции уже созданы `make_op`, значит
    /// HLC монотонен. Подписи всё равно проверяем — это дешёвая страховка от
    /// повреждённой операции.
    pub fn apply_local_batch(&mut self, ops: &[Op]) -> Result<usize> {
        if ops.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut changed = 0;
        for op in ops {
            anyhow::ensure!(op.is_intact(), "операция повреждена: {}", op.id);
            anyhow::ensure!(op.verify_signature(), "неверная подпись операции {}", op.id);
            anyhow::ensure!(
                op.dev == self.dev,
                "чужая операция в локальной пачке: {}",
                op.dev
            );
            if apply_in_tx(&tx, op)? {
                changed += 1;
            }
        }
        tx.commit()?;
        self.flush_hlc()?;
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

    /// Операции для preview. Собственные «призраки» — добавления, удалённые
    /// до первой синхронизации, — пиру не показываем: файла больше нет, а в
    /// общей базе его никогда не было. Иначе у пира появится строка плана,
    /// которой у нас нет, и стороны зря разойдутся.
    pub fn preview_ops(&self, known: &HashMap<String, (i64, i64)>) -> Result<Vec<Op>> {
        let mut ops = self.ops_since(known)?;
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM entries WHERE deleted = 0 OR synced_blob <> ''")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let live: std::collections::HashSet<String> = rows.collect::<rusqlite::Result<_>>()?;
        ops.retain(|op| {
            let path = op_path(&op.kind);
            op.dev != self.dev || path.is_empty() || live.contains(path)
        });
        // Исключённые пути пиру не показываем вовсе: иначе у него появится
        // строка плана, которую мы никогда не подтвердим. Журнал при этом не
        // трогается, поэтому после отмены исключения операции уйдут как обычно.
        if self.exclusions {
            let excluded = self.exclusion_paths()?;
            ops.retain(|op| {
                let path = op_path(&op.kind);
                path.is_empty() || !excluded.contains(path)
            });
        }
        Ok(ops)
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

    /// Переносы, которые это устройство записало в журнал. Порядок — по HLC и
    /// затем `id`, поэтому при нескольких переносах в один и тот же путь
    /// побеждает последний.
    pub fn local_moves(&self, dev: &str) -> Result<Vec<LocalMove>> {
        let mut stmt = self.conn.prepare(
            "SELECT op_json, hlc_ms, hlc_counter FROM ops
             WHERE dev = ?1 AND kind = 'entry_move'
             ORDER BY hlc_ms, hlc_counter, id",
        )?;
        let rows = stmt.query_map(params![dev], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut moves = Vec::new();
        for row in rows {
            let (json, ms, counter) = row?;
            let Ok(op) = Op::from_json(&json) else {
                continue;
            };
            if let OpKind::EntryMove { from, to, blob, .. } = op.kind {
                moves.push(LocalMove {
                    from,
                    to,
                    blob,
                    hlc: Hlc::new(ms as u64, counter as u32),
                });
            }
        }
        Ok(moves)
    }

    /// HLC последней операции, которая убрала путь: удаления или источника
    /// переноса. У `entry_move` колонка `path` указывает на цель, поэтому
    /// источник приходится доставать из `op_json`. По этой метке видно,
    /// успел ли пир применить наше удаление/перенос: его watermark должен
    /// покрывать HLC.
    pub fn removal_hlcs(&self, dev: &str) -> Result<HashMap<String, Hlc>> {
        let mut stmt = self.conn.prepare(
            "SELECT op_json, hlc_ms, hlc_counter FROM ops
             WHERE dev = ?1 AND kind IN ('entry_del', 'entry_move')",
        )?;
        let rows = stmt.query_map(params![dev], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut map: HashMap<String, Hlc> = HashMap::new();
        for row in rows {
            let (json, ms, counter) = row?;
            let Ok(op) = Op::from_json(&json) else {
                continue;
            };
            let path = match op.kind {
                OpKind::EntryDel { path } => path,
                OpKind::EntryMove { from, .. } => from,
                _ => continue,
            };
            let hlc = Hlc::new(ms as u64, counter as u32);
            map.entry(path)
                .and_modify(|current| {
                    if hlc > *current {
                        *current = hlc;
                    }
                })
                .or_insert(hlc);
        }
        Ok(map)
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
        let mut stmt = self.conn.prepare(
            "SELECT path, blob, size, mtime_ns, added_at
             FROM entries WHERE deleted = 0 AND local_present = 1 ORDER BY path",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(EntryRow {
                path: r.get(0)?,
                blob: r.get(1)?,
                size: r.get::<_, i64>(2)? as u64,
                mtime_ns: r.get(3)?,
                added_at: r.get::<_, i64>(4)? as u64,
                deleted: false,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Всё состояние путей одним запросом: скан использует его вместо
    /// `entry()`/`is_present()`/`is_ignored()` на каждый файл.
    pub fn scan_rows(&self) -> Result<Vec<ScanRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, blob, size, mtime_ns, added_at, deleted,
                    local_present, local_ignored, dev, synced_blob
             FROM entries ORDER BY path",
        )?;
        let rows = stmt.query_map([], |r| {
            let deleted = r.get::<_, i64>(5)? != 0;
            let row = ScanRow {
                path: r.get(0)?,
                blob: r.get(1)?,
                size: r.get::<_, i64>(2)? as u64,
                mtime_ns: r.get(3)?,
                added_at: r.get::<_, i64>(4)? as u64,
                deleted,
                present: r.get::<_, i64>(6)? != 0,
                ignored: r.get::<_, i64>(7)? != 0,
                dev: r.get(8)?,
                synced_blob: r.get(9)?,
            };
            Ok(if deleted {
                ScanRow {
                    blob: String::new(),
                    size: 0,
                    mtime_ns: 0,
                    added_at: 0,
                    present: false,
                    ..row
                }
            } else {
                row
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // -------------------------------------------------------- content cache

    /// Кэш содержимого: `path → (size, mtime_ns, blob)`. Переживает
    /// `reset_history`, поэтому после сброса файлы не читаются заново.
    pub fn content_cache(&self) -> Result<HashMap<String, (u64, i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, size, mtime_ns, blob FROM content_cache")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (
                    r.get::<_, i64>(1)? as u64,
                    r.get(2)?,
                    r.get::<_, String>(3)?,
                ),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    }

    /// Записать в кэш пачку посчитанных блобов одной транзакцией.
    pub fn save_content_cache(&mut self, rows: &[(String, u64, i64, String)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let now = Hlc::wall_ms() as i64;
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO content_cache(path, size, mtime_ns, blob, seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(path) DO UPDATE SET
                     size = excluded.size,
                     mtime_ns = excluded.mtime_ns,
                     blob = excluded.blob,
                     seen_at = excluded.seen_at",
            )?;
            for (path, size, mtime_ns, blob) in rows {
                stmt.execute(params![path, *size as i64, mtime_ns, blob, now])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Убрать из кэша пути, которых больше нет на диске.
    pub fn prune_content_cache(&mut self, keep: &HashSet<String>) -> Result<usize> {
        let stale: Vec<String> = {
            let mut stmt = self.conn.prepare("SELECT path FROM content_cache")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(|row| match row {
                Ok(path) if !keep.contains(&path) => Some(Ok(path)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if stale.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        {
            let mut stmt = tx.prepare("DELETE FROM content_cache WHERE path = ?1")?;
            for path in &stale {
                stmt.execute(params![path])?;
            }
        }
        tx.commit()?;
        Ok(stale.len())
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

    /// Пакетное обновление локальных флагов после скана. Сканер передаёт
    /// только те пути, где состояние действительно меняется.
    pub fn mark_presence_batch(
        &mut self,
        present: &[String],
        unignore: &[String],
        clear_pending: &[String],
    ) -> Result<()> {
        if present.is_empty() && unignore.is_empty() && clear_pending.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        {
            let mut stmt = tx.prepare(
                "UPDATE entries SET local_present = 1
                 WHERE path = ?1 AND local_present = 0",
            )?;
            for path in present {
                stmt.execute(params![path])?;
            }
            let mut stmt = tx.prepare(
                "UPDATE entries SET local_ignored = 0
                 WHERE path = ?1 AND local_ignored != 0",
            )?;
            for path in unignore {
                stmt.execute(params![path])?;
            }
            let mut stmt = tx.prepare("DELETE FROM pending_deletes WHERE path = ?1")?;
            for path in clear_pending {
                stmt.execute(params![path])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Пакетная фиксация пропавших файлов: `record` — новые ожидающие
    /// удаления, `forget` — собственные добавления, которые ещё никому не
    /// показывались. Одной транзакцией вместо одной на путь.
    pub fn apply_missing_batch(
        &mut self,
        hlc: Hlc,
        record: &[(String, String)],
        forget: &[String],
    ) -> Result<()> {
        if record.is_empty() && forget.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO pending_deletes(path, blob, detected_at, hlc_counter)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(path) DO NOTHING",
            )?;
            let mut absent = tx.prepare("UPDATE entries SET local_present = 0 WHERE path = ?1")?;
            for (path, blob) in record {
                insert.execute(params![path, blob, hlc.ms as i64, hlc.counter as i64])?;
                absent.execute(params![path])?;
            }

            let mut del_entry = tx.prepare(
                "DELETE FROM entries
                 WHERE path = ?1 AND dev = ?2 AND deleted = 0 AND synced_blob = ''",
            )?;
            let mut del_pending = tx.prepare("DELETE FROM pending_deletes WHERE path = ?1")?;
            let mut del_decision = tx.prepare("DELETE FROM plan_decisions WHERE path = ?1")?;
            for path in forget {
                if del_entry.execute(params![path, self.dev])? > 0 {
                    del_pending.execute(params![path])?;
                    del_decision.execute(params![path])?;
                }
            }
        }
        tx.commit()?;
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

    /// Локальное исключение для пути, которого ещё нет в таблице: это чужое
    /// добавление, которое не хотят забирать. Создаём запись без локального
    /// файла, чтобы план больше не предлагал этот путь.
    pub fn ignore_missing(
        &self,
        path: &str,
        blob: &str,
        size: u64,
        mtime_ns: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO entries(path, blob, size, mtime_ns, added_at, deleted, \
             hlc_ms, hlc_counter, dev, local_present, local_ignored) \
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, 0, '', 0, 1) \
             ON CONFLICT(path) DO UPDATE SET \
                 local_ignored = 1, \
                 local_present = 0",
            params![
                path,
                blob,
                size as i64,
                mtime_ns,
                Hlc::wall_ms() as i64
            ],
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

    /// Забыть согласованную с пиром базу пути.
    ///
    /// Нужно при возврате файла из корзины: удаление уже стало общим фактом,
    /// поэтому прежний `synced_blob` больше не база, а восстановление —
    /// локальное добавление. Без сброса план видел бы `blob == synced_blob`
    /// и не показывал бы вернувшийся трек в очереди.
    pub fn forget_synced_base(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE entries SET synced_blob = '' WHERE path = ?1",
            params![path],
        )?;
        Ok(())
    }

    pub fn ignored_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM entries WHERE local_ignored = 1 ORDER BY path")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Снять только флаг `local_ignored`, не трогая присутствие файла.
    /// Нужно при переходе на явные исключения: запись может лежать на диске.
    pub fn clear_ignored(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE entries SET local_ignored = 0 WHERE path = ?1",
            params![path],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------ exclusions

    /// Включить ведение исключений (только ПК).
    pub fn enable_exclusions(&mut self) {
        self.exclusions = true;
    }

    pub fn exclusions_enabled(&self) -> bool {
        self.exclusions
    }

    pub fn exclusions(&self) -> Result<Vec<ExclusionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, side, blob, size, mtime_ns, created_at
             FROM exclusions ORDER BY side, path",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ExclusionRow {
                path: r.get(0)?,
                side: r.get(1)?,
                blob: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                mtime_ns: r.get(4)?,
                created_at: r.get::<_, i64>(5)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn exclusion_paths(&self) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM exclusions")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn exclusion(&self, path: &str) -> Result<Option<ExclusionRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT path, side, blob, size, mtime_ns, created_at
                 FROM exclusions WHERE path = ?1",
                params![path],
                |r| {
                    Ok(ExclusionRow {
                        path: r.get(0)?,
                        side: r.get(1)?,
                        blob: r.get(2)?,
                        size: r.get::<_, i64>(3)? as u64,
                        mtime_ns: r.get(4)?,
                        created_at: r.get::<_, i64>(5)? as u64,
                    })
                },
            )
            .optional()?)
    }

    /// Запомнить путь вне синхронизации. Повторное решение по тому же пути
    /// обновляет сторону и содержимое, но не создаёт дубликат.
    pub fn add_exclusion(
        &self,
        path: &str,
        side: &str,
        blob: &str,
        size: u64,
        mtime_ns: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO exclusions(path, side, blob, size, mtime_ns, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
                 side = excluded.side,
                 blob = excluded.blob,
                 size = excluded.size,
                 mtime_ns = excluded.mtime_ns,
                 created_at = excluded.created_at",
            params![path, side, blob, size as i64, mtime_ns, Hlc::wall_ms() as i64],
        )?;
        // Новое исключение отменяет прежнее намерение вернуть путь: иначе
        // маркер продолжит подавлять удаление и показывать добавление.
        self.conn.execute(
            "DELETE FROM resurrections WHERE path = ?1",
            params![path],
        )?;
        Ok(())
    }

    pub fn remove_exclusion(&self, path: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM exclusions WHERE path = ?1", params![path])?
            > 0)
    }

    // ------------------------------------------------------- resurrections

    pub fn add_resurrection(
        &self,
        path: &str,
        side: &str,
        blob: &str,
        size: u64,
        mtime_ns: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO resurrections(path, side, blob, size, mtime_ns, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
                 side = excluded.side,
                 blob = excluded.blob,
                 size = excluded.size,
                 mtime_ns = excluded.mtime_ns,
                 created_at = excluded.created_at",
            params![path, side, blob, size as i64, mtime_ns, Hlc::wall_ms() as i64],
        )?;
        Ok(())
    }

    pub fn remove_resurrection(&self, path: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM resurrections WHERE path = ?1", params![path])?
            > 0)
    }

    pub fn clear_resurrections(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM resurrections", [])?)
    }

    pub fn resurrections(&self) -> Result<Vec<ResurrectionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, side, blob, size, mtime_ns, created_at
             FROM resurrections ORDER BY path",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ResurrectionRow {
                path: r.get(0)?,
                side: r.get(1)?,
                blob: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                mtime_ns: r.get(4)?,
                created_at: r.get::<_, i64>(5)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn resurrection_paths(&self) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM resurrections")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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

    pub fn record_pending_delete(&self, path: &str, blob: &str, hlc: Hlc) -> Result<()> {
        self.conn.execute(
            "INSERT INTO pending_deletes(path, blob, detected_at, hlc_counter)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO NOTHING",
            params![
                path,
                blob,
                hlc.ms as i64,
                hlc.counter as i64
            ],
        )?;
        Ok(())
    }

    pub fn pending_deletes(&self) -> Result<Vec<PendingDelete>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, blob, detected_at, hlc_counter FROM pending_deletes ORDER BY path",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(PendingDelete {
                path: r.get(0)?,
                blob: r.get(1)?,
                detected_at: r.get::<_, i64>(2)? as u64,
                counter: r.get::<_, i64>(3)? as u32,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Забыть журнал и материализованное состояние, но оставить идентичность
    /// устройства, выбранную папку, токен и корзину. Нужно для проверки
    /// синхронизации с чистого листа: следующий скан заново построит индекс.
    pub fn reset_history(&mut self) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // `content_cache` намеренно не в списке: это локальное знание о файлах,
        // а не история синхронизации. Благодаря ему «Проверить» после сброса
        // не перечитывает всю библиотеку заново.
        for table in [
            "ops",
            "entries",
            "blobs",
            "pending_deletes",
            "sync_plans",
            "plan_decisions",
            "resurrections",
        ] {
            tx.execute(&format!("DELETE FROM {table}"), [])?;
        }
        tx.commit()?;
        // Недокачанные блобы больше не нужны: решение по ним потеряно вместе
        // с планом.
        if let Some(dir) = self.data_dir() {
            let incoming = dir.join("incoming");
            if let Ok(entries) = std::fs::read_dir(incoming) {
                for entry in entries.flatten() {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }

    pub fn clear_pending_delete(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM pending_deletes WHERE path = ?1", params![path])?;
        Ok(())
    }

    /// Собственное добавление, которое ещё ни разу не входило в общую базу.
    pub fn is_local_only(&self, path: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM entries
                 WHERE path = ?1 AND dev = ?2 AND deleted = 0 AND synced_blob = ''",
                params![path, self.dev],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some())
    }

    /// Забыть локальное добавление, удалённое до первой синхронизации.
    ///
    /// Возвращает `false`, если путь чужой или уже был в общей базе: такие
    /// удаления обязаны пройти через решение человека, иначе можно стереть
    /// чужой файл. Операции из журнала не удаляем: `preview_ops` просто не
    /// показывает пиру пути, которых здесь больше нет.
    pub fn forget_never_shared(&mut self, path: &str) -> Result<bool> {
        if !self.is_local_only(path)? {
            return Ok(false);
        }
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM entries WHERE path = ?1", params![path])?;
        tx.execute("DELETE FROM pending_deletes WHERE path = ?1", params![path])?;
        tx.execute("DELETE FROM plan_decisions WHERE path = ?1", params![path])?;
        tx.commit()?;
        Ok(true)
    }

    /// Почистить «призраки» от старых сканов: ожидающие удаления, которые
    /// относятся к собственным добавлениям без общей базы или к записям,
    /// которых в дереве уже нет. Возвращает число забытых путей.
    pub fn prune_ghost_deletes(&mut self) -> Result<usize> {
        let paths: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT p.path FROM pending_deletes p
                 LEFT JOIN entries e ON e.path = p.path
                 WHERE e.path IS NULL
                    OR e.deleted != 0
                    OR (e.dev = ?1 AND e.synced_blob = '')",
            )?;
            let rows = stmt.query_map(params![self.dev], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if paths.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        {
            let mut del_pending = tx.prepare("DELETE FROM pending_deletes WHERE path = ?1")?;
            let mut del_entry = tx.prepare(
                "DELETE FROM entries WHERE path = ?1 AND dev = ?2 AND deleted = 0 AND synced_blob = ''",
            )?;
            let mut del_decision = tx.prepare("DELETE FROM plan_decisions WHERE path = ?1")?;
            for path in &paths {
                del_pending.execute(params![path])?;
                del_entry.execute(params![path, self.dev])?;
                del_decision.execute(params![path])?;
            }
        }
        tx.commit()?;
        Ok(paths.len())
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

    /// Убирает все строки корзины. Файлы блобов чистит `vault::clear`,
    /// потому что путь к каталогу знает только он.
    pub fn clear_vault_rows(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM vault", [])?)
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

/// Общая часть `apply` и `apply_local_batch`: записать операцию в журнал и
/// материализовать её в `entries`. Транзакцию открывает вызывающий.
fn apply_in_tx(tx: &Transaction<'_>, op: &Op) -> Result<bool> {
    let known: Option<i64> = tx
        .query_row("SELECT 1 FROM ops WHERE id = ?1", params![op.id], |r| {
            r.get(0)
        })
        .optional()?;
    if known.is_some() {
        return Ok(false);
    }

    tx.execute(
        "INSERT INTO ops(id, op_json, hlc_ms, hlc_counter, dev, kind, path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            op.id,
            op.to_json(),
            op.hlc.ms as i64,
            op.hlc.counter as i64,
            op.dev,
            op.kind.name(),
            op_path(&op.kind)
        ],
    )?;

    let changed = match &op.kind {
        OpKind::EntryPut {
            path,
            blob,
            size,
            mtime_ns,
            added_at,
        } => upsert_entry(tx, op, path, blob, *size, *mtime_ns, *added_at)?,
        OpKind::EntryDel { path } => delete_entry(tx, op, path, true)?,
        OpKind::EntryMove {
            from,
            to,
            blob,
            size,
            mtime_ns,
            added_at,
        } => {
            // Перенос не отменяет наше «файл пропал»: решение по старому
            // пути ещё должно найти трек по блобу уже под новым именем.
            let a = delete_entry(tx, op, from, false)?;
            let b = upsert_entry(tx, op, to, blob, *size, *mtime_ns, *added_at)?;
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
    Ok(changed)
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
             -- Оживление томбстоуна: удаление уже стало общим фактом, поэтому
             -- прежний `synced_blob` больше не база. Без сброса план видел бы
             -- blob == synced_blob и не показывал вернувшийся файл в очереди.
             -- RHS видит старую строку, поэтому `entries.deleted` — прежнее
             -- значение; обычная правка живой записи базу сохраняет.
             synced_blob = CASE
                 WHEN entries.deleted = 1 THEN ''
                 ELSE entries.synced_blob
             END,
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

fn delete_entry(tx: &Transaction<'_>, op: &Op, path: &str, clear_pending: bool) -> Result<bool> {
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
    // Подтверждённое удаление отменяет локальное «файл пропал»: иначе оно
    // осталось бы в pending_deletes и всплывало в каждом следующем плане.
    if clear_pending {
        tx.execute(
            "DELETE FROM pending_deletes WHERE path = ?1",
            params![path],
        )?;
    }
    Ok(true)
}

/// Путь, которого касается операция. У переноса — целевой: исходный путь
/// после применения уже удалён, и забывать нужно только цель.
fn op_path(kind: &OpKind) -> &str {
    match kind {
        OpKind::EntryPut { path, .. } | OpKind::EntryDel { path } => path,
        OpKind::EntryMove { to, .. } => to,
        OpKind::BlobHave { .. } => "",
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Регрессия: в старых БД на `ops` ещё нет колонки `path`. Схема пыталась
    /// создать индекс по этой колонке до миграции и падала с
    /// `no such column: path`, после чего приложение не открывало библиотеку.
    #[test]
    fn migrates_old_ops_without_path_column() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("library.db");

        let op = Op::new(
            "old-dev",
            Hlc::new(1, 0),
            OpKind::EntryPut {
                path: "music/a.mp3".into(),
                blob: "b3:old".into(),
                size: 7,
                mtime_ns: 1,
                added_at: 1,
            },
        );

        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE ops (
                     id          TEXT PRIMARY KEY,
                     op_json     TEXT NOT NULL,
                     hlc_ms      INTEGER NOT NULL,
                     hlc_counter INTEGER NOT NULL,
                     dev         TEXT NOT NULL,
                     kind        TEXT NOT NULL
                 );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ops(id, op_json, hlc_ms, hlc_counter, dev, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![op.id, op.to_json(), 1_i64, 0_i64, "old-dev", "entry_put"],
            )
            .unwrap();
        }

        let replica = Replica::open(tmp.path(), "dev-a").unwrap();

        let path: String = replica
            .conn
            .query_row("SELECT path FROM ops WHERE id = ?1", params![op.id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(path, "music/a.mp3");

        let index: i64 = replica
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'ops_by_path'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(index, 1);
    }

    #[test]
    fn preview_ops_skip_excluded_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let mut replica = Replica::open(tmp.path(), "dev-a").unwrap();
        replica.enable_exclusions();
        let op = replica
            .make_op(OpKind::EntryPut {
                path: "a.mp3".into(),
                blob: "b3:a".into(),
                size: 1,
                mtime_ns: 1,
                added_at: 1,
            })
            .unwrap();
        replica.apply(&op).unwrap();
        assert_eq!(replica.preview_ops(&HashMap::new()).unwrap().len(), 1);

        replica
            .add_exclusion("a.mp3", "local", "b3:a", 1, 1)
            .unwrap();
        // Пиру не показываем операцию по исключённому пути: иначе у него
        // появится строка плана, которую мы никогда не подтвердим.
        assert!(replica.preview_ops(&HashMap::new()).unwrap().is_empty());
        // Но из журнала операция не исчезла: после отмены исключения уйдёт.
        assert_eq!(replica.op_count().unwrap(), 1);
    }

    #[test]
    fn reset_history_keeps_identity_root_and_token() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("music");
        fs::create_dir_all(&root).unwrap();
        let mut replica = Replica::open(tmp.path(), "dev-a").unwrap();
        replica.set_root(&root).unwrap();
        let token = replica.ensure_token().unwrap();

        let op = replica
            .make_op(OpKind::EntryPut {
                path: "a.mp3".into(),
                blob: "b3:a".into(),
                size: 1,
                mtime_ns: 1,
                added_at: 1,
            })
            .unwrap();
        replica.apply(&op).unwrap();
        replica
            .add_exclusion("a.mp3", "local", "b3:a", 1, 1)
            .unwrap();
        assert_eq!(replica.op_count().unwrap(), 1);
        assert!(!replica.entries().unwrap().is_empty());

        replica.reset_history().unwrap();

        assert_eq!(replica.op_count().unwrap(), 0);
        assert!(replica.entries().unwrap().is_empty());
        // Исключение — намерение человека, а не история: сброс его не трогает.
        assert_eq!(replica.exclusions().unwrap().len(), 1);
        assert!(replica.exclusion_paths().unwrap().contains("a.mp3"));
        assert_eq!(replica.device_id(), "dev-a");
        assert_eq!(replica.root().unwrap().as_deref(), Some(root.as_path()));
        assert_eq!(replica.ensure_token().unwrap(), token);
    }
}
