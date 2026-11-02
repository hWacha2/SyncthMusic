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

pub mod commit;
pub mod hlc;
pub mod net;
pub mod op;
pub mod path;
pub mod pipe;
pub mod plan;
pub mod resolve;
pub mod scan;
pub mod store;
pub mod vault;

pub use hlc::Hlc;
pub use op::{Op, OpKind};
pub use plan::{
    build as build_plan, Change, ChangeKind, Conflict, ConflictReason, Decision, DecisionSet, Plan,
    PathDecision, Side, PLAN_TTL_MS,
};
pub use resolve::{resolve, Action, Outcome};
pub use scan::{scan, ScanReport};
pub use store::{EntryRow, Peer, PendingDelete, PlanRow, Replica, VaultRow};

/// Понятное человеку имя устройства. Не заменяет `device_id`: тот остаётся
/// стабильным, а имя можно менять (переименование ПК, `SYNCTH_DEVICE_NAME`).
/// Символы, ломающие строку сопряжения, вычищаются.
pub fn device_name() -> String {
    let raw = std::env::var("SYNCTH_DEVICE_NAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok().filter(|s| !s.trim().is_empty()))
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "device".into());
    sanitize_name(&raw)
}

/// Привести имя устройства к виду, безопасному для строки сопряжения.
pub fn sanitize_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = false;
    for ch in raw.trim().chars() {
        let keep = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') || !ch.is_ascii();
        let ch = if keep { ch } else { '-' };
        if ch == '-' {
            if last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        out.push(ch);
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "device".into()
    } else {
        out
    }
}
