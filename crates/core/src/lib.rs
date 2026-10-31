//! syncth-core — движок синхронизации музыкальной библиотеки.
//!
//! Модель (ADR-002): библиотека = зеркало одной папки.
//! Путь (`relpath`) реплицируется, содержимое адресуется BLAKE3.
//! Состояние каждого пути — LWW-регистр с HLC и детерминированным тай-брейком
//! по `device_id`, поэтому слияние сходится при любом порядке доставки.
//!
//! Удаления — отдельный класс: они никогда не рождаются автоматически.
//! Сканер только складывает пропажу в `pending_deletes`, а `entry_del`
//! появляется после явного решения пользователя (`resolve`).

pub mod hlc;
pub mod net;
pub mod op;
pub mod path;
pub mod pipe;
pub mod resolve;
pub mod scan;
pub mod store;
pub mod vault;

pub use hlc::Hlc;
pub use op::{Op, OpKind};
pub use resolve::{resolve, Action, Outcome};
pub use scan::{scan, ScanReport};
pub use store::{EntryRow, PendingDelete, Replica, VaultRow};
