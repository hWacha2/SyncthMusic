//! Трей Windows: пауза, синхронизация, открыть UI, выход.
//!
//! `tray-icon` создаёт скрытое окно и ждёт сообщений Windows, поэтому в этом
//! потоке крутится маленький `PeekMessage`-насос. Если трей не удался
//! (например, нет графической сессии), демон продолжает работать без него.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::{Cmd, Reply, Runtime};

/// Запустить трей в отдельном потоке. Ошибки только логируются.
pub fn spawn(commands: Sender<Cmd>, runtime: Arc<Mutex<Runtime>>, ui_url: String) {
    std::thread::spawn(move || {
        if let Err(e) = run(commands, runtime, ui_url) {
            tracing::warn!("трей недоступен: {e:#}");
        }
    });
}

fn run(commands: Sender<Cmd>, runtime: Arc<Mutex<Runtime>>, ui_url: String) -> anyhow::Result<()> {
    let menu = Menu::new();
    let open = MenuItem::new(format!("Открыть {}", syncth_core::APP_NAME), true, None);
    let sync = MenuItem::new("Синхронизировать сейчас", true, None);
    let pause = MenuItem::new("Пауза / продолжить", true, None);
    let check = MenuItem::new("Проверить место на диске", true, None);
    let quit = MenuItem::new("Выход", true, None);
    menu.append_items(&[
        &open,
        &sync,
        &pause,
        &check,
        &PredefinedMenuItem::separator(),
        &quit,
    ])?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(syncth_core::APP_NAME)
        .with_icon(make_icon()?)
        .build()?;

    let mut last_tooltip = String::new();
    let mut last_update = Instant::now() - Duration::from_secs(10);

    loop {
        pump_messages();

        while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            if event.id == open.id() {
                open_url(&ui_url);
            } else if event.id == sync.id() {
                fire(&commands, Cmd::Sync);
            } else if event.id == pause.id() {
                let paused = runtime.lock().unwrap().paused;
                let _ = commands.send(Cmd::Pause {
                    paused: !paused,
                    reply: sink(),
                });
            } else if event.id == check.id() {
                fire(&commands, Cmd::CheckDisk);
            } else if event.id == quit.id() {
                // WAL переживёт обычное завершение процесса; отдельный
                // graceful shutdown тут ничего не добавит.
                std::process::exit(0);
            }
        }

        if last_update.elapsed() >= Duration::from_secs(2) {
            last_update = Instant::now();
            let text = tooltip(&runtime);
            if text != last_tooltip {
                let _ = set_tooltip(&tray, &text);
                last_tooltip = text;
            }
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn tooltip(runtime: &Arc<Mutex<Runtime>>) -> String {
    let name = syncth_core::APP_NAME;
    let rt = runtime.lock().unwrap();
    if rt.disk_low {
        return format!("{name} — мало места, синхронизация на паузе");
    }
    if rt.paused {
        return format!("{name} — автосинк на паузе");
    }
    if rt.syncing {
        return format!("{name} — синхронизация…");
    }
    if rt.last_error.is_some() {
        return format!("{name} — есть ошибка, откройте UI");
    }
    format!("{name} — работает")
}

fn fire(commands: &Sender<Cmd>, make: impl FnOnce(Reply) -> Cmd + Send + 'static) {
    // Ответ трею не нужен: результат виден в UI и тултипе.
    let _ = commands.send(make(sink()));
}

fn sink() -> Reply {
    let (tx, _rx) = mpsc::channel();
    tx
}

#[cfg(windows)]
fn open_url(url: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
}

#[cfg(not(windows))]
fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

#[cfg(windows)]
fn set_tooltip(tray: &TrayIcon, text: &str) -> anyhow::Result<()> {
    tray.set_tooltip(Some(text))?;
    Ok(())
}

#[cfg(not(windows))]
fn set_tooltip(_tray: &TrayIcon, _text: &str) -> anyhow::Result<()> {
    Ok(())
}

/// Насос сообщений Windows: без него клики по меню не доставляются.
#[cfg(windows)]
fn pump_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let has = unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) };
        if has == 0 {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(not(windows))]
fn pump_messages() {}

/// Простая иконка-нота, нарисованная в коде: не нужен бинарный ресурс.
fn make_icon() -> anyhow::Result<Icon> {
    const W: u32 = 32;
    const H: u32 = 32;
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let inside = dx * dx + dy * dy <= 15.0 * 15.0;
            if !inside {
                continue;
            }
            let i = ((y * W + x) * 4) as usize;
            let shade = (y as f32 / H as f32 * 40.0) as u8;
            rgba[i] = 70 + shade;
            rgba[i + 1] = 130 + shade;
            rgba[i + 2] = 220;
            rgba[i + 3] = 255;
        }
    }
    // Белая вертикальная «нота» и точка.
    for y in 7..23 {
        for x in 14..18 {
            let i = ((y * W + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[240, 245, 255, 255]);
        }
    }
    for y in 20..25 {
        for x in 10..17 {
            let i = ((y * W + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[240, 245, 255, 255]);
        }
    }
    Icon::from_rgba(rgba, W, H).map_err(|e| anyhow::anyhow!("иконка трея: {e}"))
}
