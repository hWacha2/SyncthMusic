use tauri::{command, AppHandle, Runtime};

use crate::models::*;
use crate::{ForegroundServiceExt, Result};

#[command]
pub(crate) async fn start<R: Runtime>(
    app: AppHandle<R>,
    idle_minutes: Option<u32>,
) -> Result<ServiceStatus> {
    app.foreground_service()
        .start(idle_minutes.unwrap_or(DEFAULT_IDLE_MINUTES))
}

#[command]
pub(crate) async fn stop<R: Runtime>(app: AppHandle<R>) -> Result<ServiceStatus> {
    app.foreground_service().stop()
}

#[command]
pub(crate) async fn touch<R: Runtime>(app: AppHandle<R>, active: bool) -> Result<ServiceStatus> {
    app.foreground_service().touch(active)
}

#[command]
pub(crate) async fn set_foreground<R: Runtime>(
    app: AppHandle<R>,
    foreground: bool,
) -> Result<ServiceStatus> {
    app.foreground_service().set_foreground(foreground)
}

#[command]
pub(crate) async fn status<R: Runtime>(app: AppHandle<R>) -> Result<ServiceStatus> {
    app.foreground_service().status()
}
