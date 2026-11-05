use serde::{Deserialize, Serialize};

pub const DEFAULT_IDLE_MINUTES: u32 = 25;

fn default_idle_minutes() -> u32 {
    DEFAULT_IDLE_MINUTES
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRequest {
    /// Через сколько минут без подключений ПК сервис останавливается сам.
    #[serde(default = "default_idle_minutes")]
    pub idle_minutes: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TouchRequest {
    /// `true` — сеанс с ПК начался, `false` — закончился.
    pub active: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForegroundRequest {
    pub foreground: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub running: bool,
    pub foreground: bool,
    pub active_sessions: u32,
    pub idle_minutes: u32,
}
