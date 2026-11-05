//! Удержание приёмника на телефоне в фоне.
//!
//! Android замораживает обычный процесс, как только гаснет экран или
//! открывается другое приложение. Слушатели агента и синхронизации живут в
//! процессе приложения, поэтому без foreground-сервиса связь обрывается.
//! Плагин держит процесс и блокировки Wi-Fi/CPU, а вся сеть остаётся в
//! `syncth-mobile`: здесь нет бизнес-логики, только жизненный цикл.
//!
//! На остальных платформах плагин ничего не делает — он нужен только Android.

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Runtime,
};

pub use models::*;

#[cfg(not(target_os = "android"))]
mod desktop;
#[cfg(target_os = "android")]
mod mobile;

mod commands;
mod error;
mod models;

pub use error::{Error, Result};

#[cfg(not(target_os = "android"))]
use desktop::ForegroundService;
#[cfg(target_os = "android")]
use mobile::ForegroundService;

/// Доступ к сервису из любого места приложения.
pub trait ForegroundServiceExt<R: Runtime> {
    fn foreground_service(&self) -> &ForegroundService<R>;
}

impl<R: Runtime, T: Manager<R>> ForegroundServiceExt<R> for T {
    fn foreground_service(&self) -> &ForegroundService<R> {
        self.state::<ForegroundService<R>>().inner()
    }
}

/// Подключает плагин к приложению.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("foreground-service")
        .invoke_handler(tauri::generate_handler![
            commands::start,
            commands::stop,
            commands::touch,
            commands::set_foreground,
            commands::status,
        ])
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            let service = mobile::init(app, api)?;
            #[cfg(not(target_os = "android"))]
            let service = desktop::init(app, api)?;
            app.manage(service);
            Ok(())
        })
        .build()
}
