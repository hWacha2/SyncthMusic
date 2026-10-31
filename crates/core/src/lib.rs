//! syncth-core — движок синхронизации музыкальной библиотеки.
//!
//! Модель (ADR-002): библиотека = зеркало одной папки.
//! Путь (`relpath`) реплицируется, содержимое адресуется BLAKE3.
//! Состояние каждого пути — LWW-регистр с HLC и детерминированным тай-брейком
//! по `device_id`, поэтому слияние сходится при любом порядке доставки.

pub mod hlc;
pub mod op;
pub mod path;
pub mod pipe;
pub mod scan;
pub mod store;

pub use hlc::Hlc;
pub use op::{Op, OpKind};
pub use scan::{scan, ScanReport};
pub use store::{EntryRow, Replica};
