use serde::de::DeserializeOwned;
use tauri::{plugin::PluginApi, AppHandle, Runtime};

use crate::models::*;

pub fn init<R: Runtime, C: DeserializeOwned>(
    app: &AppHandle<R>,
    _api: PluginApi<R, C>,
) -> crate::Result<ForegroundService<R>> {
    Ok(ForegroundService(app.clone()))
}

/// Заглушка: на настольных платформах приложение не засыпает.
pub struct ForegroundService<R: Runtime>(AppHandle<R>);

impl<R: Runtime> ForegroundService<R> {
    pub fn start(&self, idle_minutes: u32) -> crate::Result<ServiceStatus> {
        Ok(ServiceStatus {
            running: false,
            foreground: false,
            active_sessions: 0,
            idle_minutes,
        })
    }

    pub fn stop(&self) -> crate::Result<ServiceStatus> {
        self.status()
    }

    pub fn touch(&self, _active: bool) -> crate::Result<ServiceStatus> {
        self.status()
    }

    pub fn set_foreground(&self, _foreground: bool) -> crate::Result<ServiceStatus> {
        self.status()
    }

    pub fn status(&self) -> crate::Result<ServiceStatus> {
        Ok(ServiceStatus::default())
    }
}
