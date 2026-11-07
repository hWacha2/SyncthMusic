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

-- Состояние пути. Одна строка на путь; дискриминатор `state` описывает
-- жизненный цикл, остальные поля — реплицируемое содержимое и локальные
-- знания (`base`, `present`, `pending_hlc`, `created_at`).
CREATE TABLE IF NOT EXISTS path_state (
    path        TEXT PRIMARY KEY,
    state       TEXT NOT NULL,
    -- Последняя общая база. NULL — файл ещё ни разу не был согласован.
    base        TEXT,
    -- Последнее известное содержимое. У tombstone здесь хранится удалённое
    -- содержимое: корзине и плану нужны размер/mtime, а проекция `entries`
    -- их прячет.
    blob        TEXT NOT NULL DEFAULT '',
    size        INTEGER NOT NULL DEFAULT 0,
    mtime_ns    INTEGER NOT NULL DEFAULT 0,
    added_at    INTEGER NOT NULL DEFAULT 0,
    hlc_ms      INTEGER NOT NULL DEFAULT 0,
    hlc_counter INTEGER NOT NULL DEFAULT 0,
    dev         TEXT NOT NULL DEFAULT '',
    -- Локальное знание: файл реально лежит здесь. Не реплицируется.
    present     INTEGER NOT NULL DEFAULT 0,
    -- Только для missing: HLC момента, когда файл пропал.
    pending_hlc_ms      INTEGER,
    pending_hlc_counter INTEGER,
    -- Только для excluded/resurrecting: когда принято решение.
    created_at  INTEGER
);

CREATE INDEX IF NOT EXISTS idx_path_state_state ON path_state(state);
CREATE INDEX IF NOT EXISTS idx_path_state_blob ON path_state(blob);
CREATE INDEX IF NOT EXISTS idx_path_state_hlc ON path_state(hlc_ms, hlc_counter);

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

/// Дискриминатор состояния пути. Это единственный источник истины о том,
/// что происходит с путём; старые таблицы `entries`/`pending_deletes`/
/// `exclusions`/`resurrections` больше не существуют.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathState {
    /// Содержимое согласовано с базой или является локальным изменением.
    Live,
    /// Файл пропал, человек ещё не решил, что с ним делать.
    Missing,
    /// Удалён везде; `base` помнит последнюю общую версию.
    Tombstone,
    /// Путь вне синхронизации, уцелевшая копия лежит здесь.
    ExcludedLocal,
    /// Путь вне синхронизации, уцелевшая копия лежит у пира.
    ExcludedRemote,
    /// Отмена исключения: локальная копия возвращается в синхронизацию.
    ResurrectingLocal,
    /// Отмена исключения: копия должна приехать от пира.
    ResurrectingRemote,
}

impl PathState {
    pub fn as_str(self) -> &'static str {
        match self {
            PathState::Live => "live",
            PathState::Missing => "missing",
            PathState::Tombstone => "tombstone",
            PathState::ExcludedLocal => "excluded_local",
            PathState::ExcludedRemote => "excluded_remote",
            PathState::ResurrectingLocal => "resurrecting_local",
            PathState::ResurrectingRemote => "resurrecting_remote",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "live" => PathState::Live,
            "missing" => PathState::Missing,
            "tombstone" => PathState::Tombstone,
            "excluded_local" => PathState::ExcludedLocal,
            "excluded_remote" => PathState::ExcludedRemote,
            "resurrecting_local" => PathState::ResurrectingLocal,
            "resurrecting_remote" => PathState::ResurrectingRemote,
            _ => return None,
        })
    }

    pub fn is_excluded(self) -> bool {
        matches!(self, PathState::ExcludedLocal | PathState::ExcludedRemote)
    }

    pub fn is_resurrecting(self) -> bool {
        matches!(
            self,
            PathState::ResurrectingLocal | PathState::ResurrectingRemote
        )
    }

    pub fn is_tombstone(self) -> bool {
        self == PathState::Tombstone
    }
}

/// Одна строка `path_state`. Поля `base`, `present`, `pending_hlc` и
/// `created_at` локальные: они не реплицируются и не входят в `state_hash`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathStateRow {
    pub path: String,
    pub state: PathState,
    pub base: Option<String>,
    pub blob: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub added_at: u64,
    pub hlc: Hlc,
    pub dev: String,
    pub present: bool,
    pub pending_hlc: Option<Hlc>,
    pub created_at: Option<u64>,
}

impl PathStateRow {
    /// Содержимое, которое увидит совместимая проекция `entries`.
    fn entry_row(&self) -> EntryRow {
        if self.state.is_tombstone() {
            EntryRow {
                path: self.path.clone(),
                blob: String::new(),
                size: 0,
                mtime_ns: 0,
                added_at: 0,
                deleted: true,
            }
        } else {
            EntryRow {
                path: self.path.clone(),
                blob: self.blob.clone(),
                size: self.size,
                mtime_ns: self.mtime_ns,
                added_at: self.added_at,
                deleted: false,
            }
        }
    }
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
        // Миграции с прежних версий нет: `path_state` заменяет четыре таблицы,
        // и переносить их полу-состояния нечем. При первом открытии старой БД
        // история синхронизации стирается, а идентичность, root, token,
        // корзина, пиры и кэш содержимого остаются.
        if self.get_meta("schema_version")?.as_deref() != Some("7") {
            self.reset_sync_schema()?;
            self.set_meta("schema_version", "7")?;
        }
        if !self.has_column("peers", "name")? {
            self.conn.execute(
                "ALTER TABLE peers ADD COLUMN name TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS ops_by_path ON ops(dev, path)",
            [],
        )?;
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

    /// Стереть историю синхронизации и пересоздать её таблицы. Вызывается
    /// один раз при переходе на схему 7; данные, не относящиеся к истории
    /// (meta, peers, vault, content_cache, dev_keys), не трогаются.
    fn reset_sync_schema(&mut self) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for table in [
            "entries",
            "pending_deletes",
            "exclusions",
            "resurrections",
            "path_state",
            "ops",
            "plan_decisions",
            "sync_plans",
            "blobs",
        ] {
            tx.execute(&format!("DROP TABLE IF EXISTS {table}"), [])?;
        }
        tx.commit()?;
        self.conn.execute_batch(SCHEMA)?;
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

    /// Полное состояние пути. Основное чтение для нового кода.
    pub fn path_state(&self, path: &str) -> Result<Option<PathStateRow>> {
        self.conn
            .query_row(
                "SELECT path, state, base, blob, size, mtime_ns, added_at,
                        hlc_ms, hlc_counter, dev, present,
                        pending_hlc_ms, pending_hlc_counter, created_at
                 FROM path_state WHERE path = ?1",
                params![path],
                read_path_state_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Все состояния путей — одним запросом.
    pub fn path_states(&self) -> Result<Vec<PathStateRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, state, base, blob, size, mtime_ns, added_at,
                    hlc_ms, hlc_counter, dev, present,
                    pending_hlc_ms, pending_hlc_counter, created_at
             FROM path_state ORDER BY path",
        )?;
        let rows = stmt.query_map([], read_path_state_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Сохранить итоговые состояния после скана. Скан сам реализует переходы
    /// из `docs/path-state.md`, поэтому пишет строки целиком, а не отдельные
    /// флаги. Удалённые пути забываются вместе с решениями по ним.
    pub fn save_scan_states(
        &mut self,
        rows: &[PathStateRow],
        removed: &[String],
    ) -> Result<()> {
        if rows.is_empty() && removed.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for row in rows {
            write_path_state(&tx, row)?;
        }
        for path in removed {
            tx.execute("DELETE FROM path_state WHERE path = ?1", params![path])?;
            tx.execute("DELETE FROM plan_decisions WHERE path = ?1", params![path])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Записать одно состояние пути. Commit и undo сами реализуют переходы
    /// из `docs/path-state.md`, поэтому им нужна запись строки целиком.
    pub fn save_path_state(&self, row: &PathStateRow) -> Result<()> {
        write_path_state(&self.conn, row)
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

        let local = op.dev == self.dev;
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = apply_in_tx(&tx, op, local)?;
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
            if apply_in_tx(&tx, op, true)? {
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
        let mut stmt = self.conn.prepare(
            "SELECT path FROM path_state WHERE state <> 'tombstone' OR base IS NOT NULL",
        )?;
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

    /// Уносил ли журнал этот блоб с пути переносом. Нужно, чтобы «удалить
    /// везде» не считало старый путь отдельной удалённой копией: трек уже
    /// живёт под новым именем, и отчёт должен назвать только его.
    pub fn moved_from(&self, path: &str, blob: &str) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare("SELECT op_json FROM ops WHERE kind = 'entry_move'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for raw in rows {
            let Ok(op) = Op::from_json(&raw?) else {
                continue;
            };
            if let OpKind::EntryMove {
                from,
                blob: moved,
                ..
            } = op.kind
            {
                if from == path && moved == blob {
                    return Ok(true);
                }
            }
        }
        Ok(false)
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
        Ok(self
            .path_states()?
            .into_iter()
            .map(|row| row.entry_row())
            .collect())
    }

    pub fn live_entries(&self) -> Result<Vec<EntryRow>> {
        Ok(self.entries()?.into_iter().filter(|e| !e.deleted).collect())
    }

    /// Записи, которые физически есть на этом устройстве.
    pub fn local_entries(&self) -> Result<Vec<EntryRow>> {
        Ok(self
            .path_states()?
            .into_iter()
            .filter(|row| !row.state.is_tombstone() && row.present)
            .map(|row| row.entry_row())
            .collect())
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
            .path_state(path)?
            .map(|row| row.present)
            .unwrap_or(false))
    }

    pub fn mark_present(&self, path: &str) -> Result<()> {
        mark_present_in(&self.conn, path)
    }

    pub fn mark_absent(&self, path: &str) -> Result<()> {
        mark_absent_in(&self.conn, path)
    }

    /// Локальное исключение: запись остаётся в общей библиотеке, но на этом
    /// устройстве её не хотят видеть и не будут докачивать.
    pub fn set_ignored(&self, path: &str, ignored: bool) -> Result<()> {
        let Some(mut row) = self.path_state(path)? else {
            return Ok(());
        };
        if ignored {
            row.state = PathState::ExcludedRemote;
            row.present = false;
            row.pending_hlc = None;
            row.created_at = Some(Hlc::wall_ms());
        } else if row.state.is_excluded() {
            row.state = PathState::Live;
            row.created_at = None;
        }
        write_path_state(&self.conn, &row)
    }

    /// Локальное исключение для пути, которого ещё нет в таблице: это чужое
    /// добавление, которое не хотят забирать.
    pub fn ignore_missing(
        &self,
        path: &str,
        blob: &str,
        size: u64,
        mtime_ns: i64,
    ) -> Result<()> {
        let now = Hlc::wall_ms();
        let mut row = match self.path_state(path)? {
            Some(row) => row,
            None => PathStateRow {
                path: path.to_string(),
                state: PathState::ExcludedRemote,
                base: None,
                blob: String::new(),
                size: 0,
                mtime_ns: 0,
                added_at: now,
                hlc: Hlc::default(),
                dev: String::new(),
                present: false,
                pending_hlc: None,
                created_at: None,
            },
        };
        row.state = PathState::ExcludedRemote;
        row.blob = blob.to_string();
        row.size = size;
        row.mtime_ns = mtime_ns;
        if row.added_at == 0 {
            row.added_at = now;
        }
        row.present = false;
        row.pending_hlc = None;
        row.created_at = Some(now);
        write_path_state(&self.conn, &row)
    }

    pub fn is_ignored(&self, path: &str) -> Result<bool> {
        Ok(self
            .path_state(path)?
            .map(|row| row.state.is_excluded())
            .unwrap_or(false))
    }

    /// Последнее состояние, про которое известно, что оно есть у пира.
    /// Пустая строка — базы ещё нет (файл никогда не синхронизировался).
    pub fn synced_blob(&self, path: &str) -> Result<String> {
        Ok(self
            .path_state(path)?
            .and_then(|row| row.base)
            .unwrap_or_default())
    }

    /// Вызывается в конце успешного сеанса: обе стороны обменялись операциями
    /// и всеми нужными блобами, значит текущее содержимое — согласованная база.
    pub fn confirm_synced(&self) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let paths: Vec<String> = {
            let mut stmt = tx.prepare("SELECT path FROM path_state")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut changed = 0;
        for path in paths {
            if confirm_synced_in(&tx, &path)? {
                changed += 1;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// То же, но только для выбранных путей: commit не должен объявлять
    /// согласованными отклонённые изменения и отложенные удаления.
    pub fn confirm_synced_paths(&self, paths: &[String]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut changed = 0;
        for path in paths {
            if confirm_synced_in(&tx, path)? {
                changed += 1;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Забыть согласованную с пиром базу пути.
    ///
    /// Нужно при возврате файла из корзины: удаление уже стало общим фактом,
    /// поэтому прежняя база больше не база, а восстановление — локальное
    /// добавление. У tombstone база сохраняется: по ней план отличает
    /// «пир ещё на базе» от восстановления.
    pub fn forget_synced_base(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE path_state SET base = NULL
             WHERE path = ?1 AND state <> 'tombstone'",
            params![path],
        )?;
        Ok(())
    }

    pub fn ignored_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT path FROM path_state
             WHERE state IN ('excluded_local', 'excluded_remote') ORDER BY path",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Старый флаг `local_ignored` больше не существует: исключение — это
    /// состояние пути, и снимать его может только явная отмена. Вызов оставлен
    /// для совместимости с кодом, который чистил legacy-флаг.
    pub fn clear_ignored(&self, _path: &str) -> Result<()> {
        Ok(())
    }

    // ------------------------------------------------------------ exclusions

    /// Включить ведение исключений (только ПК). Флаг остаётся ради
    /// совместимости: сами исключения теперь состояния `path_state`.
    pub fn enable_exclusions(&mut self) {
        self.exclusions = true;
    }

    pub fn exclusions_enabled(&self) -> bool {
        self.exclusions
    }

    /// Совместимая проекция исключений.
    pub fn exclusions(&self) -> Result<Vec<ExclusionRow>> {
        let mut rows: Vec<ExclusionRow> = self
            .path_states()?
            .into_iter()
            .filter(|row| row.state.is_excluded())
            .map(|row| ExclusionRow {
                side: exclusion_side(row.state).to_string(),
                path: row.path,
                blob: row.blob,
                size: row.size,
                mtime_ns: row.mtime_ns,
                created_at: row.created_at.unwrap_or(0),
            })
            .collect();
        rows.sort_by(|a, b| (&a.side, &a.path).cmp(&(&b.side, &b.path)));
        Ok(rows)
    }

    pub fn exclusion_paths(&self) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT path FROM path_state
             WHERE state IN ('excluded_local', 'excluded_remote')",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn exclusion(&self, path: &str) -> Result<Option<ExclusionRow>> {
        Ok(self.path_state(path)?.and_then(|row| {
            row.state.is_excluded().then(|| ExclusionRow {
                side: exclusion_side(row.state).to_string(),
                path: row.path,
                blob: row.blob,
                size: row.size,
                mtime_ns: row.mtime_ns,
                created_at: row.created_at.unwrap_or(0),
            })
        }))
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
        let now = Hlc::wall_ms();
        let state = match side {
            "local" => PathState::ExcludedLocal,
            _ => PathState::ExcludedRemote,
        };
        let mut row = self.path_state(path)?.unwrap_or_else(|| PathStateRow {
            path: path.to_string(),
            state,
            base: None,
            blob: String::new(),
            size: 0,
            mtime_ns: 0,
            added_at: now,
            hlc: Hlc::default(),
            dev: String::new(),
            present: false,
            pending_hlc: None,
            created_at: None,
        });
        row.state = state;
        row.blob = blob.to_string();
        row.size = size;
        row.mtime_ns = mtime_ns;
        if row.added_at == 0 {
            row.added_at = now;
        }
        // Сторона исключения совпадает с тем, где физически лежит копия.
        row.present = state == PathState::ExcludedLocal;
        row.pending_hlc = None;
        row.created_at = Some(now);
        write_path_state(&self.conn, &row)
    }

    /// Снять исключение, вернув путь в `Live` с тем же содержимым и
    /// присутствием. Операцию возврата в синхронизацию создаёт вызывающий.
    pub fn remove_exclusion(&self, path: &str) -> Result<bool> {
        let Some(mut row) = self.path_state(path)? else {
            return Ok(false);
        };
        if !row.state.is_excluded() {
            return Ok(false);
        }
        row.state = PathState::Live;
        row.created_at = None;
        write_path_state(&self.conn, &row)?;
        Ok(true)
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
        let now = Hlc::wall_ms();
        let mut row = self.path_state(path)?.unwrap_or_else(|| PathStateRow {
            path: path.to_string(),
            state: PathState::Live,
            base: None,
            blob: String::new(),
            size: 0,
            mtime_ns: 0,
            added_at: now,
            hlc: Hlc::default(),
            dev: String::new(),
            present: false,
            pending_hlc: None,
            created_at: None,
        });
        row.blob = blob.to_string();
        row.size = size;
        row.mtime_ns = mtime_ns;
        if row.added_at == 0 {
            row.added_at = now;
        }
        row.pending_hlc = None;
        row.created_at = Some(now);
        if side == "local" {
            // Новая жизнь: прежняя база больше не база, иначе план счёл бы
            // вернувшийся файл уже согласованным.
            row.state = PathState::ResurrectingLocal;
            row.base = None;
            row.present = true;
        } else {
            row.state = PathState::ResurrectingRemote;
            row.present = false;
        }
        write_path_state(&self.conn, &row)
    }

    pub fn remove_resurrection(&self, path: &str) -> Result<bool> {
        let Some(mut row) = self.path_state(path)? else {
            return Ok(false);
        };
        if !row.state.is_resurrecting() {
            return Ok(false);
        }
        row.state = PathState::Live;
        row.created_at = None;
        write_path_state(&self.conn, &row)?;
        Ok(true)
    }

    pub fn clear_resurrections(&self) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let paths: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT path FROM path_state
                 WHERE state IN ('resurrecting_local', 'resurrecting_remote')",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for path in &paths {
            if let Some(mut row) = read_path_state(&tx, path)? {
                row.state = PathState::Live;
                row.created_at = None;
                write_path_state(&tx, &row)?;
            }
        }
        tx.commit()?;
        Ok(paths.len())
    }

    pub fn resurrections(&self) -> Result<Vec<ResurrectionRow>> {
        Ok(self
            .path_states()?
            .into_iter()
            .filter(|row| row.state.is_resurrecting())
            .map(|row| ResurrectionRow {
                side: resurrection_side(row.state).to_string(),
                path: row.path,
                blob: row.blob,
                size: row.size,
                mtime_ns: row.mtime_ns,
                created_at: row.created_at.unwrap_or(0),
            })
            .collect())
    }

    pub fn resurrection_paths(&self) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT path FROM path_state
             WHERE state IN ('resurrecting_local', 'resurrecting_remote')",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Все живые пути, ссылающиеся на блоб. Нужно, чтобы «удалить навсегда»
    /// не оставило копию под другим именем после переноса.
    pub fn live_paths_by_blob(&self, blob: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT path FROM path_state
             WHERE blob = ?1 AND state <> 'tombstone' ORDER BY path",
        )?;
        let rows = stmt.query_map(params![blob], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Пары (путь, блоб) для подтверждённых удалений, которые ещё лежат на
    /// диске. Блоб здесь не нормализуется: по нему проверяется локальная копия.
    pub fn deleted_present(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, blob FROM path_state
             WHERE state = 'tombstone' AND present = 1 AND blob <> ''
             ORDER BY path",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn entry(&self, path: &str) -> Result<Option<EntryRow>> {
        Ok(self.path_state(path)?.map(|row| row.entry_row()))
    }

    pub fn total_live_bytes(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(size), 0) FROM path_state WHERE state <> 'tombstone'",
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
        let mut row = match self.path_state(path)? {
            Some(row) => row,
            None => PathStateRow {
                path: path.to_string(),
                state: PathState::Missing,
                base: None,
                blob: String::new(),
                size: 0,
                mtime_ns: 0,
                added_at: 0,
                hlc: Hlc::default(),
                dev: String::new(),
                present: false,
                pending_hlc: None,
                created_at: None,
            },
        };
        if row.state == PathState::Tombstone {
            return Ok(());
        }
        // Повторное обнаружение не переписывает отметку: иначе HLC мягкого
        // удаления уезжал бы вперёд на каждом скане.
        if row.state == PathState::Missing && row.pending_hlc.is_some() {
            return Ok(());
        }
        if row.blob.is_empty() {
            row.blob = blob.to_string();
        }
        row.state = PathState::Missing;
        row.present = false;
        row.pending_hlc = Some(hlc);
        row.created_at = None;
        write_path_state(&self.conn, &row)
    }

    pub fn pending_deletes(&self) -> Result<Vec<PendingDelete>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, blob, pending_hlc_ms, pending_hlc_counter
             FROM path_state
             WHERE state = 'missing' AND pending_hlc_ms IS NOT NULL
             ORDER BY path",
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
            "path_state",
            "blobs",
            "sync_plans",
            "plan_decisions",
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

    /// Снять ожидание удаления, не удаляя файл. `Missing` снова становится
    /// живым путём (файла здесь по-прежнему нет — его докачает обмен).
    pub fn clear_pending_delete(&self, path: &str) -> Result<()> {
        let Some(mut row) = self.path_state(path)? else {
            return Ok(());
        };
        row.pending_hlc = None;
        if row.state == PathState::Missing {
            row.state = PathState::Live;
            row.created_at = None;
        }
        write_path_state(&self.conn, &row)
    }

    /// Собственное добавление, которое ещё ни разу не входило в общую базу.
    pub fn is_local_only(&self, path: &str) -> Result<bool> {
        Ok(self.path_state(path)?.is_some_and(|row| {
            !row.state.is_tombstone() && row.base.is_none() && row.dev == self.dev
        }))
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
        tx.execute("DELETE FROM path_state WHERE path = ?1", params![path])?;
        tx.execute("DELETE FROM plan_decisions WHERE path = ?1", params![path])?;
        tx.commit()?;
        Ok(true)
    }

    /// Почистить «призраки» от старых сканов: `Missing` без общей базы —
    /// это черновик, который никогда никому не показывали. Возвращает число
    /// забытых путей.
    pub fn prune_ghost_deletes(&mut self) -> Result<usize> {
        let paths: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT path FROM path_state WHERE state = 'missing' AND base IS NULL")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if paths.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        {
            let mut del_state = tx.prepare("DELETE FROM path_state WHERE path = ?1")?;
            let mut del_decision = tx.prepare("DELETE FROM plan_decisions WHERE path = ?1")?;
            for path in &paths {
                del_state.execute(params![path])?;
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
            .path_state(path)?
            .map(|row| (row.blob, row.size, row.mtime_ns, row.added_at)))
    }

    /// Пути с конфликтными копиями — для UI.
    pub fn conflict_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT path FROM path_state
             WHERE state <> 'tombstone' AND path LIKE '%.sync-conflict-%'
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

fn exclusion_side(state: PathState) -> &'static str {
    if state == PathState::ExcludedLocal {
        "local"
    } else {
        "remote"
    }
}

fn resurrection_side(state: PathState) -> &'static str {
    if state == PathState::ResurrectingLocal {
        "local"
    } else {
        "remote"
    }
}

fn read_path_state_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PathStateRow> {
    let raw: String = row.get(1)?;
    let state = PathState::parse(&raw).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("неизвестное состояние пути: {raw}"),
            )),
        )
    })?;
    let pending_ms: Option<i64> = row.get(11)?;
    let pending_counter: Option<i64> = row.get(12)?;
    Ok(PathStateRow {
        path: row.get(0)?,
        state,
        base: row.get(2)?,
        blob: row.get(3)?,
        size: row.get::<_, i64>(4)? as u64,
        mtime_ns: row.get(5)?,
        added_at: row.get::<_, i64>(6)? as u64,
        hlc: Hlc::new(row.get::<_, i64>(7)? as u64, row.get::<_, i64>(8)? as u32),
        dev: row.get(9)?,
        present: row.get::<_, i64>(10)? != 0,
        pending_hlc: pending_ms
            .zip(pending_counter)
            .map(|(ms, counter)| Hlc::new(ms as u64, counter as u32)),
        created_at: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
    })
}

fn read_path_state(conn: &Connection, path: &str) -> Result<Option<PathStateRow>> {
    conn.query_row(
        "SELECT path, state, base, blob, size, mtime_ns, added_at,
                hlc_ms, hlc_counter, dev, present,
                pending_hlc_ms, pending_hlc_counter, created_at
         FROM path_state WHERE path = ?1",
        params![path],
        read_path_state_row,
    )
    .optional()
    .map_err(Into::into)
}

fn write_path_state(conn: &Connection, row: &PathStateRow) -> Result<()> {
    conn.execute(
        "INSERT INTO path_state(
             path, state, base, blob, size, mtime_ns, added_at,
             hlc_ms, hlc_counter, dev, present,
             pending_hlc_ms, pending_hlc_counter, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(path) DO UPDATE SET
             state = excluded.state,
             base = excluded.base,
             blob = excluded.blob,
             size = excluded.size,
             mtime_ns = excluded.mtime_ns,
             added_at = excluded.added_at,
             hlc_ms = excluded.hlc_ms,
             hlc_counter = excluded.hlc_counter,
             dev = excluded.dev,
             present = excluded.present,
             pending_hlc_ms = excluded.pending_hlc_ms,
             pending_hlc_counter = excluded.pending_hlc_counter,
             created_at = excluded.created_at",
        params![
            row.path,
            row.state.as_str(),
            row.base,
            row.blob,
            row.size as i64,
            row.mtime_ns,
            row.added_at as i64,
            row.hlc.ms as i64,
            row.hlc.counter as i64,
            row.dev,
            row.present as i64,
            row.pending_hlc.map(|h| h.ms as i64),
            row.pending_hlc.map(|h| h.counter as i64),
            row.created_at.map(|v| v as i64),
        ],
    )?;
    Ok(())
}

fn mark_present_in(conn: &Connection, path: &str) -> Result<()> {
    conn.execute(
        "UPDATE path_state SET present = 1 WHERE path = ?1 AND present = 0",
        params![path],
    )?;
    Ok(())
}

fn mark_absent_in(conn: &Connection, path: &str) -> Result<()> {
    conn.execute(
        "UPDATE path_state SET present = 0 WHERE path = ?1",
        params![path],
    )?;
    Ok(())
}

/// Отметить путь согласованным: `base = blob`. Маркер возврата снимается:
/// файл уже здесь и подтверждён.
fn confirm_synced_in(conn: &Connection, path: &str) -> Result<bool> {
    let Some(mut row) = read_path_state(conn, path)? else {
        return Ok(false);
    };
    if !row.present || row.state.is_tombstone() || row.state == PathState::Missing
        || row.state.is_excluded()
    {
        return Ok(false);
    }
    let mut changed = false;
    if row.base.as_deref() != Some(row.blob.as_str()) {
        row.base = Some(row.blob.clone());
        changed = true;
    }
    if row.state.is_resurrecting() {
        row.state = PathState::Live;
        row.created_at = None;
        changed = true;
    }
    if changed {
        write_path_state(conn, &row)?;
    }
    Ok(changed)
}

fn remember_blob(tx: &Transaction<'_>, blob: &str, size: u64) -> Result<()> {
    tx.execute(
        "INSERT INTO blobs(blob, size) VALUES (?1, ?2)
         ON CONFLICT(blob) DO NOTHING",
        params![blob, size as i64],
    )?;
    Ok(())
}

/// Общая часть `apply` и `apply_local_batch`: записать операцию в журнал и
/// материализовать её в `path_state`. Транзакцию открывает вызывающий.
/// `local` — операция создана этим устройством, а не пришла от пира.
fn apply_in_tx(tx: &Transaction<'_>, op: &Op, local: bool) -> Result<bool> {
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
        } => put_path(tx, op, local, path, blob, *size, *mtime_ns, *added_at)?,
        OpKind::EntryDel { path } => del_path(tx, op, path, None)?,
        OpKind::EntryMove {
            from,
            to,
            blob,
            size,
            mtime_ns,
            added_at,
        } => {
            // Перенос не отменяет наше «файл пропал», если он несёт тот же
            // блоб: решение по старому пути ещё должно найти трек под новым
            // именем. В остальных случаях источник становится томбстоуном.
            let a = del_path(tx, op, from, Some(blob))?;
            let b = put_path(tx, op, local, to, blob, *size, *mtime_ns, *added_at)?;
            a || b
        }
        OpKind::BlobHave { blob, size } => {
            remember_blob(tx, blob, *size)?;
            false
        }
    };
    Ok(changed)
}

/// LWW: побеждает строго больший `(hlc_ms, hlc_counter, device_id)`.
fn is_newer(tx: &Transaction<'_>, path: &str, op: &Op) -> Result<bool> {
    let Some(row) = read_path_state(tx, path)? else {
        return Ok(true);
    };
    Ok((op.hlc.ms, op.hlc.counter, op.dev.as_str())
        > (row.hlc.ms, row.hlc.counter, row.dev.as_str()))
}

fn put_path(
    tx: &Transaction<'_>,
    op: &Op,
    local: bool,
    path: &str,
    blob: &str,
    size: u64,
    mtime_ns: i64,
    added_at: u64,
) -> Result<bool> {
    let Some(mut row) = read_path_state(tx, path)? else {
        let row = PathStateRow {
            path: path.to_string(),
            state: PathState::Live,
            base: None,
            blob: blob.to_string(),
            size,
            mtime_ns,
            added_at,
            hlc: op.hlc,
            dev: op.dev.clone(),
            present: false,
            pending_hlc: None,
            created_at: None,
        };
        write_path_state(tx, &row)?;
        remember_blob(tx, blob, size)?;
        return Ok(true);
    };
    if row.state.is_excluded() {
        // Исключение — решение человека; операции по нему не применяются.
        return Ok(false);
    }
    let newer = is_newer(tx, path, op)?;
    match row.state {
        PathState::Missing => {
            let same_blob = row.blob == blob;
            let after_pending = row.pending_hlc.is_none_or(|pending| op.hlc > pending);
            // Чужой put по пропавшему пути возвращает его только с тем же
            // блобом и более новым HLC. Иначе решение остаётся за человеком:
            // план покажет конфликт «изменено против удалено».
            if !local && !(same_blob && after_pending) {
                return Ok(false);
            }
            row.state = PathState::Live;
            row.blob = blob.to_string();
            row.size = size;
            row.mtime_ns = mtime_ns;
            row.added_at = added_at;
            row.hlc = op.hlc;
            row.dev = op.dev.clone();
            row.pending_hlc = None;
            row.created_at = None;
        }
        PathState::Tombstone => {
            if !newer {
                return Ok(false);
            }
            // Оживление: удаление уже стало общим фактом, прежняя база
            // больше не база.
            row.state = PathState::Live;
            row.base = None;
            row.blob = blob.to_string();
            row.size = size;
            row.mtime_ns = mtime_ns;
            row.added_at = added_at;
            row.hlc = op.hlc;
            row.dev = op.dev.clone();
            row.pending_hlc = None;
            row.created_at = None;
        }
        PathState::ResurrectingRemote => {
            if !local && !newer {
                return Ok(false);
            }
            // Дождались копии от пира: путь снова в синхронизации.
            row.state = PathState::Live;
            row.base = Some(blob.to_string());
            row.blob = blob.to_string();
            row.size = size;
            row.mtime_ns = mtime_ns;
            row.added_at = added_at;
            row.hlc = op.hlc;
            row.dev = op.dev.clone();
            row.present = false;
            row.pending_hlc = None;
            row.created_at = None;
        }
        PathState::ResurrectingLocal => {
            if !local && !newer {
                return Ok(false);
            }
            row.blob = blob.to_string();
            row.size = size;
            row.mtime_ns = mtime_ns;
            row.added_at = added_at;
            row.hlc = op.hlc;
            row.dev = op.dev.clone();
        }
        PathState::Live => {
            if !newer {
                return Ok(false);
            }
            row.blob = blob.to_string();
            row.size = size;
            row.mtime_ns = mtime_ns;
            row.added_at = added_at;
            row.hlc = op.hlc;
            row.dev = op.dev.clone();
        }
        PathState::ExcludedLocal | PathState::ExcludedRemote => unreachable!(),
    }
    write_path_state(tx, &row)?;
    remember_blob(tx, blob, size)?;
    Ok(true)
}

fn del_path(
    tx: &Transaction<'_>,
    op: &Op,
    path: &str,
    keep_missing_blob: Option<&str>,
) -> Result<bool> {
    let Some(mut row) = read_path_state(tx, path)? else {
        let row = PathStateRow {
            path: path.to_string(),
            state: PathState::Tombstone,
            base: None,
            blob: String::new(),
            size: 0,
            mtime_ns: 0,
            added_at: 0,
            hlc: op.hlc,
            dev: op.dev.clone(),
            present: false,
            pending_hlc: None,
            created_at: None,
        };
        write_path_state(tx, &row)?;
        return Ok(true);
    };
    if row.state.is_excluded() {
        return Ok(false);
    }
    // Перенос с тем же блобом не отменяет ожидание удаления: решение по
    // старому пути ещё должно найти трек под новым именем.
    if let Some(blob) = keep_missing_blob {
        if row.state == PathState::Missing && row.blob == blob {
            return Ok(false);
        }
    }
    if !is_newer(tx, path, op)? {
        return Ok(false);
    }
    row.state = PathState::Tombstone;
    row.pending_hlc = None;
    row.created_at = None;
    row.hlc = op.hlc;
    row.dev = op.dev.clone();
    write_path_state(tx, &row)?;
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

    fn put(path: &str, blob: &str) -> OpKind {
        OpKind::EntryPut {
            path: path.into(),
            blob: blob.into(),
            size: blob.len() as u64,
            mtime_ns: 1,
            added_at: 1,
        }
    }

    fn local_put(replica: &mut Replica, path: &str, blob: &str) {
        let op = replica.make_op(put(path, blob)).unwrap();
        replica.apply(&op).unwrap();
    }

    fn synced(replica: &mut Replica, path: &str, blob: &str) {
        local_put(replica, path, blob);
        replica.mark_present(path).unwrap();
        replica.confirm_synced().unwrap();
    }

    /// Схема 7 стирает историю синхронизации целиком, но не трогает
    /// идентичность, корень, токен, пиров, корзину и кэш содержимого.
    #[test]
    fn destructive_reset_keeps_identity_and_vault() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("library.db");
        let root = tmp.path().join("music");
        fs::create_dir_all(&root).unwrap();

        let op = Op::new(
            "old-dev",
            Hlc::new(1, 0),
            put("music/a.mp3", "b3:old"),
        );

        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE ops (
                     id          TEXT PRIMARY KEY,
                     op_json     TEXT NOT NULL,
                     hlc_ms      INTEGER NOT NULL,
                     hlc_counter INTEGER NOT NULL,
                     dev         TEXT NOT NULL,
                     kind        TEXT NOT NULL
                 );
                 CREATE TABLE entries (
                     path TEXT PRIMARY KEY, blob TEXT NOT NULL, size INTEGER NOT NULL,
                     mtime_ns INTEGER NOT NULL, added_at INTEGER NOT NULL,
                     deleted INTEGER NOT NULL, hlc_ms INTEGER NOT NULL,
                     hlc_counter INTEGER NOT NULL, dev TEXT NOT NULL,
                     local_present INTEGER NOT NULL, local_ignored INTEGER NOT NULL,
                     synced_blob TEXT NOT NULL
                 );
                 CREATE TABLE pending_deletes (
                     path TEXT PRIMARY KEY, blob TEXT NOT NULL,
                     detected_at INTEGER NOT NULL, hlc_counter INTEGER NOT NULL
                 );
                 CREATE TABLE exclusions (
                     path TEXT PRIMARY KEY, side TEXT NOT NULL, blob TEXT NOT NULL,
                     size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL,
                     created_at INTEGER NOT NULL
                 );
                 CREATE TABLE resurrections (
                     path TEXT PRIMARY KEY, side TEXT NOT NULL, blob TEXT NOT NULL,
                     size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL,
                     created_at INTEGER NOT NULL
                 );
                 CREATE TABLE peers (
                     dev TEXT PRIMARY KEY, addr TEXT NOT NULL, added_at INTEGER NOT NULL
                 );
                 CREATE TABLE vault (
                     blob TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL,
                     mtime_ns INTEGER NOT NULL, added_at INTEGER NOT NULL,
                     deleted_at INTEGER NOT NULL, PRIMARY KEY (blob, path)
                 );
                 CREATE TABLE content_cache (
                     path TEXT PRIMARY KEY, size INTEGER NOT NULL,
                     mtime_ns INTEGER NOT NULL, blob TEXT NOT NULL,
                     seen_at INTEGER NOT NULL
                 );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO meta(key, value) VALUES
                     ('schema_version', '6'), ('device_id', 'old-dev'), ('token', 'tok')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO meta(key, value) VALUES ('root', ?1)",
                params![root.to_string_lossy()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ops(id, op_json, hlc_ms, hlc_counter, dev, kind)
                 VALUES (?1, ?2, 1, 0, 'old-dev', 'entry_put')",
                params![op.id, op.to_json()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO entries(path, blob, size, mtime_ns, added_at, deleted,
                     hlc_ms, hlc_counter, dev, local_present, local_ignored, synced_blob)
                 VALUES ('music/a.mp3', 'b3:old', 1, 1, 1, 0, 1, 0, 'old-dev', 1, 0, '')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO pending_deletes(path, blob, detected_at, hlc_counter)
                 VALUES ('music/b.mp3', 'b3:b', 5, 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO exclusions(path, side, blob, size, mtime_ns, created_at)
                 VALUES ('music/c.mp3', 'local', 'b3:c', 1, 1, 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO resurrections(path, side, blob, size, mtime_ns, created_at)
                 VALUES ('music/d.mp3', 'remote', 'b3:d', 1, 1, 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO peers(dev, addr, added_at) VALUES ('peer-1', '1.2.3.4:7', 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO vault(blob, path, size, mtime_ns, added_at, deleted_at)
                 VALUES ('b3:v', 'music/v.mp3', 1, 1, 1, 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO content_cache(path, size, mtime_ns, blob, seen_at)
                 VALUES ('music/cached.mp3', 1, 1, 'b3:cached', 1)",
                [],
            )
            .unwrap();
        }

        let replica = Replica::open(tmp.path(), "dev-a").unwrap();

        assert_eq!(replica.op_count().unwrap(), 0);
        assert!(replica.entries().unwrap().is_empty());
        assert!(replica.path_states().unwrap().is_empty());
        assert!(replica.pending_deletes().unwrap().is_empty());
        assert!(replica.exclusions().unwrap().is_empty());
        assert!(replica.resurrections().unwrap().is_empty());

        let old_tables: i64 = replica
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN
                     ('entries', 'pending_deletes', 'exclusions', 'resurrections')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_tables, 0, "старые таблицы должны быть удалены");

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

        assert_eq!(
            replica.get_meta("schema_version").unwrap().as_deref(),
            Some("7")
        );
        assert_eq!(replica.device_id(), "old-dev");
        assert_eq!(replica.root().unwrap().as_deref(), Some(root.as_path()));
        assert_eq!(replica.ensure_token().unwrap(), "tok");
        assert_eq!(replica.known_peers().unwrap().len(), 1);
        assert_eq!(replica.vault_rows().unwrap().len(), 1);
        assert_eq!(replica.content_cache().unwrap().len(), 1);
    }

    #[test]
    fn preview_ops_skip_excluded_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let mut replica = Replica::open(tmp.path(), "dev-a").unwrap();
        replica.enable_exclusions();
        let op = replica.make_op(put("a.mp3", "b3:a")).unwrap();
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
    fn reset_history_keeps_identity_root_and_token_but_wipes_state() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("music");
        fs::create_dir_all(&root).unwrap();
        let mut replica = Replica::open(tmp.path(), "dev-a").unwrap();
        replica.set_root(&root).unwrap();
        let token = replica.ensure_token().unwrap();

        synced(&mut replica, "a.mp3", "b3:a");
        replica
            .add_exclusion("a.mp3", "local", "b3:a", 1, 1)
            .unwrap();
        assert_eq!(replica.op_count().unwrap(), 1);
        assert!(!replica.entries().unwrap().is_empty());

        replica.reset_history().unwrap();

        assert_eq!(replica.op_count().unwrap(), 0);
        assert!(replica.entries().unwrap().is_empty());
        // Полный сброс не переносит исключения: это часть истории путей.
        assert!(replica.exclusions().unwrap().is_empty());
        assert!(replica.exclusion_paths().unwrap().is_empty());
        assert_eq!(replica.device_id(), "dev-a");
        assert_eq!(replica.root().unwrap().as_deref(), Some(root.as_path()));
        assert_eq!(replica.ensure_token().unwrap(), token);
    }

    #[test]
    fn entry_put_on_tombstone_starts_a_new_life() {
        let mut replica = Replica::in_memory("dev-a").unwrap();
        synced(&mut replica, "a.mp3", "b3:a");
        assert_eq!(
            replica.path_state("a.mp3").unwrap().unwrap().base.as_deref(),
            Some("b3:a")
        );

        let del = replica.make_op(OpKind::EntryDel { path: "a.mp3".into() }).unwrap();
        replica.apply(&del).unwrap();
        let tombstone = replica.path_state("a.mp3").unwrap().unwrap();
        assert_eq!(tombstone.state, PathState::Tombstone);
        assert_eq!(tombstone.base.as_deref(), Some("b3:a"));
        assert_eq!(tombstone.blob, "b3:a");

        local_put(&mut replica, "a.mp3", "b3:a");
        let revived = replica.path_state("a.mp3").unwrap().unwrap();
        assert_eq!(revived.state, PathState::Live);
        assert_eq!(revived.base, None, "оживление начинает новую жизнь");
    }

    #[test]
    fn entry_del_keeps_content_in_path_state_but_hides_it_in_projection() {
        let mut replica = Replica::in_memory("dev-a").unwrap();
        synced(&mut replica, "a.mp3", "b3:a");
        let del = replica.make_op(OpKind::EntryDel { path: "a.mp3".into() }).unwrap();
        replica.apply(&del).unwrap();

        let raw = replica.entry_raw("a.mp3").unwrap().unwrap();
        assert_eq!(raw.0, "b3:a");
        let projected = replica.entry("a.mp3").unwrap().unwrap();
        assert!(projected.deleted);
        assert!(projected.blob.is_empty());
        assert_eq!(projected.size, 0);
    }

    #[test]
    fn remote_put_on_missing_follows_the_pending_hlc() {
        let mut replica = Replica::in_memory("dev-a").unwrap();
        synced(&mut replica, "a.mp3", "b3:a");
        replica
            .record_pending_delete("a.mp3", "b3:a", Hlc::new(100, 0))
            .unwrap();

        // Тот же блоб, но HLC не новее отметки о пропаже — путь остаётся
        // пропавшим, решение принимает человек.
        let stale = Op::new(
            "dev-peer",
            Hlc::new(100, 0),
            put("a.mp3", "b3:a"),
        );
        replica.apply(&stale).unwrap();
        assert_eq!(
            replica.path_state("a.mp3").unwrap().unwrap().state,
            PathState::Missing
        );

        let other = Op::new("dev-peer", Hlc::new(200, 0), put("a.mp3", "b3:b"));
        replica.apply(&other).unwrap();
        let still = replica.path_state("a.mp3").unwrap().unwrap();
        assert_eq!(still.state, PathState::Missing);
        assert_eq!(still.blob, "b3:a");
        assert_eq!(still.pending_hlc, Some(Hlc::new(100, 0)));

        let restored = Op::new("dev-peer", Hlc::new(201, 0), put("a.mp3", "b3:a"));
        replica.apply(&restored).unwrap();
        let live = replica.path_state("a.mp3").unwrap().unwrap();
        assert_eq!(live.state, PathState::Live);
        assert_eq!(live.base.as_deref(), Some("b3:a"));
        assert_eq!(live.pending_hlc, None);
    }

    #[test]
    fn move_keeps_missing_source_with_the_matching_blob() {
        let mut replica = Replica::in_memory("dev-a").unwrap();
        synced(&mut replica, "old.mp3", "b3:a");
        replica
            .record_pending_delete("old.mp3", "b3:a", Hlc::new(100, 0))
            .unwrap();

        let mv = Op::new(
            "dev-peer",
            Hlc::new(Hlc::wall_ms() + 101, 0),
            OpKind::EntryMove {
                from: "old.mp3".into(),
                to: "new.mp3".into(),
                blob: "b3:a".into(),
                size: 1,
                mtime_ns: 1,
                added_at: 1,
            },
        );
        replica.apply(&mv).unwrap();
        let source = replica.path_state("old.mp3").unwrap().unwrap();
        assert_eq!(source.state, PathState::Missing);
        assert_eq!(source.pending_hlc, Some(Hlc::new(100, 0)));
        let target = replica.path_state("new.mp3").unwrap().unwrap();
        assert_eq!(target.state, PathState::Live);
        assert_eq!(target.blob, "b3:a");

        // Перенос другого содержимого источник не щадит.
        let mv = Op::new(
            "dev-peer",
            Hlc::new(Hlc::wall_ms() + 102, 0),
            OpKind::EntryMove {
                from: "old.mp3".into(),
                to: "other.mp3".into(),
                blob: "b3:b".into(),
                size: 1,
                mtime_ns: 1,
                added_at: 1,
            },
        );
        replica.apply(&mv).unwrap();
        assert_eq!(
            replica.path_state("old.mp3").unwrap().unwrap().state,
            PathState::Tombstone
        );
    }

    #[test]
    fn exclusion_and_resurrection_round_trip() {
        let mut replica = Replica::in_memory("dev-a").unwrap();
        synced(&mut replica, "a.mp3", "b3:a");

        replica
            .add_exclusion("a.mp3", "local", "b3:a", 1, 1)
            .unwrap();
        let excluded = replica.path_state("a.mp3").unwrap().unwrap();
        assert_eq!(excluded.state, PathState::ExcludedLocal);
        assert!(excluded.present);
        assert_eq!(excluded.base.as_deref(), Some("b3:a"));

        assert!(replica.remove_exclusion("a.mp3").unwrap());
        let live = replica.path_state("a.mp3").unwrap().unwrap();
        assert_eq!(live.state, PathState::Live);
        assert_eq!(live.created_at, None);

        // Возврат копии от пира.
        replica
            .add_exclusion("a.mp3", "remote", "b3:a", 1, 1)
            .unwrap();
        replica
            .add_resurrection("a.mp3", "remote", "b3:a", 1, 1)
            .unwrap();
        let waiting = replica.path_state("a.mp3").unwrap().unwrap();
        assert_eq!(waiting.state, PathState::ResurrectingRemote);
        assert!(!waiting.present);
        assert_eq!(waiting.base.as_deref(), Some("b3:a"));
        assert_eq!(replica.resurrection_paths().unwrap().len(), 1);

        // Возврат локальной копии начинает новую жизнь и подтверждается.
        replica
            .add_resurrection("a.mp3", "local", "b3:a", 1, 1)
            .unwrap();
        let local = replica.path_state("a.mp3").unwrap().unwrap();
        assert_eq!(local.state, PathState::ResurrectingLocal);
        assert_eq!(local.base, None);
        assert!(local.present);

        assert_eq!(replica.confirm_synced().unwrap(), 1);
        let confirmed = replica.path_state("a.mp3").unwrap().unwrap();
        assert_eq!(confirmed.state, PathState::Live);
        assert_eq!(confirmed.base.as_deref(), Some("b3:a"));
    }

    #[test]
    fn state_hash_ignores_local_flags_and_tombstone_payload() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let mut b = Replica::in_memory("dev-b").unwrap();
        synced(&mut a, "a.mp3", "b3:a");
        synced(&mut b, "a.mp3", "b3:a");
        a.record_pending_delete("a.mp3", "b3:a", Hlc::new(10, 0))
            .unwrap();
        assert_eq!(
            a.path_state("a.mp3").unwrap().unwrap().state,
            PathState::Missing
        );
        assert_eq!(
            b.path_state("a.mp3").unwrap().unwrap().state,
            PathState::Live
        );
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());

        let mut c = Replica::in_memory("dev-c").unwrap();
        synced(&mut c, "a.mp3", "b3:other");
        for replica in [&mut b, &mut c] {
            let del = replica
                .make_op(OpKind::EntryDel { path: "a.mp3".into() })
                .unwrap();
            replica.apply(&del).unwrap();
        }
        assert_eq!(b.state_hash().unwrap(), c.state_hash().unwrap());
    }

    #[test]
    fn pending_deletes_projection_is_only_missing() {
        let mut replica = Replica::in_memory("dev-a").unwrap();
        synced(&mut replica, "a.mp3", "b3:a");
        replica
            .record_pending_delete("a.mp3", "b3:a", Hlc::new(42, 7))
            .unwrap();
        let pending = replica.pending_deletes().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].hlc(), Hlc::new(42, 7));

        replica.clear_pending_delete("a.mp3").unwrap();
        assert!(replica.pending_deletes().unwrap().is_empty());
        let live = replica.path_state("a.mp3").unwrap().unwrap();
        assert_eq!(live.state, PathState::Live);
        assert_eq!(live.blob, "b3:a");
    }

    #[test]
    fn prune_ghost_deletes_only_drops_missing_without_base() {
        let mut replica = Replica::in_memory("dev-a").unwrap();
        synced(&mut replica, "kept.mp3", "b3:kept");
        replica
            .record_pending_delete("kept.mp3", "b3:kept", Hlc::new(10, 0))
            .unwrap();
        replica
            .record_pending_delete("ghost.mp3", "b3:ghost", Hlc::new(10, 0))
            .unwrap();

        assert_eq!(replica.prune_ghost_deletes().unwrap(), 1);
        assert!(replica.path_state("ghost.mp3").unwrap().is_none());
        assert_eq!(
            replica.path_state("kept.mp3").unwrap().unwrap().state,
            PathState::Missing
        );
    }
}
