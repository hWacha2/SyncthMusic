//! Десктопный клиент SynctMusic.
//!
//! Приложение — тонкая оболочка над уже существующим демоном `syncthd`:
//! состояние и действия берутся из его локального HTTP API (`127.0.0.1:47889`),
//! а установка/запуск демона и выбор библиотеки выполняются здесь.
//!
//! Запросы к демону идут из Rust, а не из webview: у API нет CORS-заголовков,
//! и так frontend не зависит от сетевых разрешений webview.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use syncth_core::Replica;

const UI_ADDR: &str = "127.0.0.1:47889";
const SYNC_BIND: &str = "0.0.0.0:47887";

#[derive(Default)]
struct Desktop {
    /// Демон, запущенный именно этим окном. Нужен, чтобы не плодить процессы.
    child: Mutex<Option<std::process::Child>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    root: Option<String>,
    data_dir: Option<String>,
    daemon_bin: Option<String>,
}

// ------------------------------------------------------------------ paths

fn app_data_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        return PathBuf::from(appdata).join("SyncthMusic");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config").join("syncth-music")
}

fn config_path() -> PathBuf {
    app_data_dir().join("desktop.json")
}

fn default_data_dir() -> PathBuf {
    app_data_dir().join("data")
}

fn load_config() -> Config {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(config: &Config) -> Result<(), String> {
    std::fs::create_dir_all(app_data_dir()).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(config_path(), text).map_err(|e| e.to_string())
}

fn effective_data_dir(config: &Config) -> PathBuf {
    config
        .data_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_data_dir)
}

fn effective_root(config: &Config) -> Option<PathBuf> {
    config.root.as_ref().map(PathBuf::from)
}

/// Локальное имя устройства до первого запуска. Демон дальше хранит его в БД.
fn default_device_id(data_dir: &Path) -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "device".into());
    let seed = format!("{host}|{}", data_dir.display());
    let mut acc: u64 = 1469598103934665603;
    for b in seed.as_bytes() {
        acc ^= *b as u64;
        acc = acc.wrapping_mul(1099511628211);
    }
    format!("dev-{:08x}", (acc >> 32) as u32)
}

fn daemon_name() -> &'static str {
    if cfg!(windows) {
        "syncthd.exe"
    } else {
        "syncthd"
    }
}

fn resolve_daemon_bin(config: &Config) -> Option<PathBuf> {
    if let Some(path) = &config.daemon_bin {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut candidates = vec![dir.join(daemon_name())];
    if let Some(target) = dir.parent() {
        candidates.push(target.join("release").join(daemon_name()));
        candidates.push(target.join("debug").join(daemon_name()));
    }
    for candidate in candidates {
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(daemon_name()))
        .find(|path| path.is_file())
}

// ------------------------------------------------------------------ http to daemon

fn http(method: &str, path: &str, body: Option<&str>) -> Result<Value, String> {
    let addr: SocketAddr = UI_ADDR.parse().map_err(|e| format!("{e}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(1200))
        .map_err(|e| format!("Служба не отвечает: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(190)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

    let body = body.unwrap_or("");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {UI_ADDR}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body.as_bytes()))
        .and_then(|_| stream.flush())
        .map_err(|e| format!("Не удалось отправить запрос службе: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("Не удалось прочитать ответ службы: {e}"))?;

    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "Служба вернула некорректный ответ".to_string())?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| "Служба вернула ответ без статуса".to_string())?;
    let payload = String::from_utf8_lossy(&raw[split + 4..]);
    let value: Value = serde_json::from_str(&payload).unwrap_or_else(|_| json!({ "raw": payload }));

    if !(200..300).contains(&status) {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Служба вернула ошибку");
        return Err(message.to_string());
    }
    Ok(value)
}

fn daemon_running() -> bool {
    http("GET", "/api/status", None).is_ok()
}

async fn blocking<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("Фоновая задача упала: {e}"))?
}

// ------------------------------------------------------------------ commands

#[tauri::command]
fn app_info() -> Value {
    let config = load_config();
    let data_dir = effective_data_dir(&config);
    let daemon = resolve_daemon_bin(&config);
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "config_path": config_path().to_string_lossy(),
        "config": config,
        "data_dir": data_dir.to_string_lossy(),
        "device_name": syncth_core::device_name(),
        "daemon_bin": daemon.map(|p| p.to_string_lossy().to_string()),
        "app_data_dir": app_data_dir().to_string_lossy(),
        "platform": if cfg!(windows) { "windows" } else if cfg!(target_os = "macos") { "macos" } else { "linux" },
        "sync_addr": SYNC_BIND,
    })
}

#[tauri::command]
async fn daemon_status() -> Value {
    match blocking(|| http("GET", "/api/status", None)).await {
        Ok(status) => json!({ "running": true, "status": status }),
        Err(error) => json!({ "running": false, "error": error }),
    }
}

#[tauri::command]
async fn daemon_pair_code() -> Result<Value, String> {
    blocking(|| http("GET", "/api/pair-code", None)).await
}

#[tauri::command]
async fn daemon_action(name: String, payload: Option<Value>) -> Result<Value, String> {
    let payload = payload.unwrap_or(Value::Null);
    let field = |key: &str| -> Result<String, String> {
        payload
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("не передано поле «{key}»"))
    };

    let (path, body) = match name.as_str() {
        "scan" => ("/api/scan", None),
        "sync" => ("/api/sync", None),
        "gc" => ("/api/gc", None),
        "disk_check" => ("/api/disk/check", None),
        "pause" => (
            "/api/pause",
            Some(json!({
                "paused": payload.get("paused").and_then(Value::as_bool).unwrap_or(false)
            })),
        ),
        "pair" => ("/api/pair", Some(json!({ "code": field("code")? }))),
        "connect" => ("/api/connect", Some(json!({ "dev": field("dev")? }))),
        "disconnect" => ("/api/disconnect", None),
        "peer_forget" => (
            "/api/peers/forget",
            Some(json!({ "dev": field("dev")? })),
        ),
        "pending_resolve" => (
            "/api/pending/resolve",
            Some(json!({ "path": field("path")?, "action": field("action")? })),
        ),
        "pending_forget" => (
            "/api/pending/forget",
            Some(json!({ "path": field("path")? })),
        ),
        "restore" => ("/api/restore", Some(json!({ "path": field("path")? }))),
        "ignore" => (
            "/api/ignore",
            Some(json!({
                "path": field("path")?,
                "ignored": payload.get("ignored").and_then(Value::as_bool).unwrap_or(true)
            })),
        ),
        other => return Err(format!("неизвестное действие: {other}")),
    };

    let body = body.map(|value| value.to_string());
    blocking(move || http("POST", path, body.as_deref())).await
}

#[tauri::command]
fn start_daemon(state: tauri::State<'_, Desktop>) -> Result<Value, String> {
    if daemon_running() {
        return Ok(json!({ "started": false, "message": "Служба уже запущена" }));
    }

    let config = load_config();
    let root =
        effective_root(&config).ok_or_else(|| "сначала выберите папку с музыкой".to_string())?;
    if !root.is_dir() {
        return Err(format!("папка библиотеки не найдена: {}", root.display()));
    }
    let bin = resolve_daemon_bin(&config).ok_or_else(|| {
        "не найден syncthd: соберите его командой `cargo build --release -p syncth-daemon`"
            .to_string()
    })?;
    let data_dir = effective_data_dir(&config);
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(app_data_dir()).map_err(|e| e.to_string())?;

    // Лог демона рядом с настройками — пригодится, если что-то не поднялось.
    let log_path = app_data_dir().join("daemon.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| e.to_string())?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;

    let mut command = Command::new(&bin);
    command
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("--root")
        .arg(&root)
        .arg("--ui")
        .arg(UI_ADDR)
        .arg("--bind")
        .arg(SYNC_BIND)
        // У десктопного клиента свой интерфейс: второй значок в трее не нужен.
        .arg("--no-tray")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let child = command
        .spawn()
        .map_err(|e| format!("не удалось запустить {}: {e}", bin.display()))?;
    if let Ok(mut slot) = state.child.lock() {
        *slot = Some(child);
    }

    Ok(json!({
        "started": true,
        "bin": bin.to_string_lossy(),
        "data_dir": data_dir.to_string_lossy(),
        "log": log_path.to_string_lossy(),
    }))
}

#[tauri::command]
async fn stop_daemon(app: tauri::AppHandle) -> Result<Value, String> {
    use tauri::Manager;

    blocking(move || {
        let _ = http("POST", "/api/quit", None);
        let mut stopped = false;
        for _ in 0..30 {
            if !daemon_running() {
                stopped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        // Если демон не ответил на /api/quit, останавливаем тот процесс,
        // который запустили сами.
        if let Some(desktop) = app.try_state::<Desktop>() {
            if let Ok(mut slot) = desktop.child.lock() {
                if let Some(mut child) = slot.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                    stopped = !daemon_running();
                }
            }
        }

        if stopped {
            Ok(json!({ "stopped": true, "message": "Служба остановлена" }))
        } else {
            Err("Не удалось остановить службу: процесс не отвечает".to_string())
        }
    })
    .await
}

#[tauri::command]
async fn initialize_library(root: String, data_dir: Option<String>) -> Result<Value, String> {
    blocking(move || {
        if daemon_running() {
            return Err("Остановите службу перед созданием библиотеки".to_string());
        }
        let root_path = PathBuf::from(&root);
        if !root_path.is_dir() {
            return Err(format!("папка не найдена: {}", root_path.display()));
        }
        let data_dir = data_dir
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(default_data_dir);
        std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;

        let dev = default_device_id(&data_dir);
        let mut replica = Replica::open(&data_dir, &dev).map_err(|e| format!("{e:#}"))?;
        replica.set_root(&root_path).map_err(|e| format!("{e:#}"))?;
        let report =
            syncth_core::scan::scan(&root_path, &mut replica).map_err(|e| format!("{e:#}"))?;
        let device = replica.device_id().to_string();
        drop(replica);

        let mut config = load_config();
        config.root = Some(root_path.to_string_lossy().into());
        config.data_dir = Some(data_dir.to_string_lossy().into());
        save_config(&config)?;

        Ok(json!({
            "device": device,
            "root": root_path.to_string_lossy(),
            "data_dir": data_dir.to_string_lossy(),
            "added": report.added,
            "changed": report.changed,
            "renamed": report.renamed,
            "missing": report.missing.len(),
            "ops": report.ops_emitted,
            "files": report.added + report.changed + report.unchanged + report.renamed,
        }))
    })
    .await
}

#[tauri::command]
async fn change_library(folder: String) -> Result<Value, String> {
    blocking(move || {
        if daemon_running() {
            return Err("Сначала остановите службу (она держит библиотеку открытой)".to_string());
        }
        let root = PathBuf::from(&folder);
        if !root.is_dir() {
            return Err(format!("папка не найдена: {}", root.display()));
        }
        let mut config = load_config();
        let data_dir = effective_data_dir(&config);
        std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
        let replica = Replica::open(&data_dir, &default_device_id(&data_dir))
            .map_err(|e| format!("{e:#}"))?;
        replica.set_root(&root).map_err(|e| format!("{e:#}"))?;
        config.root = Some(root.to_string_lossy().into());
        config.data_dir = Some(data_dir.to_string_lossy().into());
        save_config(&config)?;
        Ok(json!({ "root": root.to_string_lossy() }))
    })
    .await
}

#[tauri::command]
fn pick_folder(title: Option<String>) -> Option<String> {
    let mut dialog = rfd::FileDialog::new();
    if let Some(title) = title {
        dialog = dialog.set_title(&title);
    }
    dialog
        .pick_folder()
        .map(|p| p.to_string_lossy().to_string())
}

#[tauri::command]
fn pick_data_folder() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Куда складывать служебные данные SynctMusic")
        .pick_folder()
        .map(|p| p.to_string_lossy().to_string())
}

/// Проверка пути, введённого вручную. Диалог выбора — удобство, но путь
/// всегда можно напечатать: на ПК это обычный `F:\Music`.
#[tauri::command]
fn probe_folder(path: String) -> Value {
    let path = PathBuf::from(path.trim());
    json!({
        "exists": path.is_dir(),
        "path": path.to_string_lossy(),
    })
}

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    open_target(&path)
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    open_target(&url)
}

fn open_target(target: &str) -> Result<(), String> {
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", target]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(target);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(target);
        command
    };

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("не удалось открыть: {e}"))
}

#[tauri::command]
fn daemon_log_tail(lines: Option<usize>) -> Value {
    let path = app_data_dir().join("daemon.log");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let lines = lines.unwrap_or(60).clamp(10, 400);
    let tail: Vec<&str> = text.lines().rev().take(lines).collect();
    json!({
        "path": path.to_string_lossy(),
        "text": tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
    })
}

#[tauri::command]
fn window_minimize(window: tauri::WebviewWindow) {
    let _ = window.minimize();
}

#[tauri::command]
fn window_toggle_maximize(window: tauri::WebviewWindow) {
    if window.is_maximized().unwrap_or(false) {
        let _ = window.unmaximize();
    } else {
        let _ = window.maximize();
    }
}

#[tauri::command]
fn window_close(window: tauri::WebviewWindow) {
    let _ = window.close();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Desktop::default())
        // Отладочный канал: позволяет прогнать сценарий интерфейса без кликов.
        // В релизе отсутствует.
        .setup(|app| {
            #[cfg(debug_assertions)]
            if let Ok(script) = std::env::var("SYNCTH_UI_EVAL") {
                use tauri::Manager;
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(2500));
                    if let Some(window) = handle.get_webview_window("main") {
                        let _ = window.eval(&script);
                    }
                });
            }
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            daemon_status,
            daemon_pair_code,
            daemon_action,
            start_daemon,
            stop_daemon,
            initialize_library,
            change_library,
            pick_folder,
            pick_data_folder,
            probe_folder,
            open_path,
            open_url,
            daemon_log_tail,
            window_minimize,
            window_toggle_maximize,
            window_close,
        ])
        .run(tauri::generate_context!())
        .expect("не удалось запустить SynctMusic");
}
