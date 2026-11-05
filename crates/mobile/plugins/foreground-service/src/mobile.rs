use serde::de::DeserializeOwned;
use serde_json::json;
use tauri::{
    plugin::{PluginApi, PluginHandle},
    AppHandle, Runtime,
};

use crate::models::*;

pub fn init<R: Runtime, C: DeserializeOwned>(
    _app: &AppHandle<R>,
    api: PluginApi<R, C>,
) -> crate::Result<ForegroundService<R>> {
    let handle =
        api.register_android_plugin("org.syncth.music.foreground", "ForegroundServicePlugin")?;
    Ok(ForegroundService(handle))
}

/// Обёртка над Kotlin-плагином.
pub struct ForegroundService<R: Runtime>(PluginHandle<R>);

impl<R: Runtime> ForegroundService<R> {
    pub fn start(&self, idle_minutes: u32) -> crate::Result<ServiceStatus> {
        self.0
            .run_mobile_plugin("start", StartRequest { idle_minutes })
            .map_err(Into::into)
    }

    pub fn stop(&self) -> crate::Result<ServiceStatus> {
        self.0
            .run_mobile_plugin("stop", json!({}))
            .map_err(Into::into)
    }

    /// Сообщает сервису, что сеанс с ПК начался или закончился.
    pub fn touch(&self, active: bool) -> crate::Result<ServiceStatus> {
        self.0
            .run_mobile_plugin("touch", TouchRequest { active })
            .map_err(Into::into)
    }

    /// Приложение на переднем плане или ушло в фон.
    pub fn set_foreground(&self, foreground: bool) -> crate::Result<ServiceStatus> {
        self.0
            .run_mobile_plugin("setForeground", ForegroundRequest { foreground })
            .map_err(Into::into)
    }

    pub fn status(&self) -> crate::Result<ServiceStatus> {
        self.0
            .run_mobile_plugin("status", json!({}))
            .map_err(Into::into)
    }
}
