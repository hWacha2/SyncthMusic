//! Android-клиент SynctMusic.
//!
//! В отличие от настольной версии здесь нет отдельного демона: ядро
//! (`syncth-core`) живёт прямо в приложении. Так проще с точки зрения Android —
//! нет второго процесса, которым надо управлять, и нет HTTP-прослойки.
//!
//! Что уже работает: инициализация библиотеки на выбранной папке, скан,
//! исходящая синхронизация с сопряжённым пиром, очередь удалений, корзина,
//! конфликты, список устройств. Входящие сеансы (когда ПК сам стучится на
//! телефон) появятся вместе с foreground-сервисом.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use syncth_core::{net, resolve, scan, store::Replica, Action, Hlc};
use tauri::Manager;

/// Порт синхронизации: тот же, что у демона на ПК.
const SYNC_PORT: u16 = 47887;

/// Имя устройства до первого открытия базы. Дальше ядро берёт сохранённое.
/// Считаем из машины и каталога данных: без аккаунтов и без сети.
fn boot_dev(data_dir: &Path) -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "phone".into());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = format!("{host}|{}|{nanos}", data_dir.display());
    let mut acc: u64 = 1469598103934665603;
    for b in seed.as_bytes() {
        acc ^= *b as u64;
        acc = acc.wrapping_mul(1099511628211);
    }
    format!("dev-{:08x}", (acc >> 32) as u32)
}

#[derive(Default)]
struct Mobile {
    /// Один и тот же корень нельзя сканировать дважды одновременно.
    busy: Mutex<bool>,
}

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(err)?.join("library");
    std::fs::create_dir_all(&dir).map_err(err)?;
    Ok(dir)
}

fn open(app: &tauri::AppHandle) -> Result<Replica, String> {
    let dir = data_dir(app)?;
    let dev = boot_dev(&dir);
    let mut replica = Replica::open(&dir, &dev).map_err(err)?;
    replica.set_device_name(&phone_name()).map_err(err)?;
    Ok(replica)
}

/// Модель телефона для интерфейса и для второго устройства. В отличие от
/// `device_id`, имя ничего не идентифицирует и может быть любым.
fn phone_name() -> String {
    use std::sync::OnceLock;
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        if cfg!(target_os = "android") {
            if let Ok(out) = std::process::Command::new("getprop")
                .arg("ro.product.model")
                .output()
            {
                let model = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !model.is_empty() {
                    return model;
                }
            }
        }
        syncth_core::device_name()
    })
    .clone()
}

fn default_root() -> String {
    if cfg!(target_os = "android") {
        "/storage/emulated/0/Music".to_string()
    } else {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".into());
        PathBuf::from(home)
            .join("Music")
            .to_string_lossy()
            .to_string()
    }
}

/// Папки, которые на телефоне обычно и есть музыка. Нужны вместо диалога
/// выбора: системный пикер на Android отдаёт URI, а нам нужен обычный путь.
fn root_candidates() -> Vec<Value> {
    let base = if cfg!(target_os = "android") {
        PathBuf::from("/storage/emulated/0")
    } else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&base) else {
        return out;
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    dirs.sort();
    for path in dirs {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if name.starts_with('.') || name == "android" || name == "dcim" || name == "documents" {
            continue;
        }
        out.push(json!({
            "path": path.to_string_lossy(),
            "name": path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        }));
    }
    out
}

fn status_json(replica: &Replica) -> Value {
    let root = replica.root().ok().flatten();
    let root_text = root.as_ref().map(|p| p.to_string_lossy().to_string());
    let entries = replica.live_entries().unwrap_or_default();
    let local = entries
        .iter()
        .filter(|e| replica.is_present(&e.path).unwrap_or(false))
        .count();
    let pending: Vec<Value> = replica
        .pending_deletes()
        .unwrap_or_default()
        .into_iter()
        .map(|p| json!({ "path": p.path, "detected_at": p.detected_at }))
        .collect();
    let vault: Vec<Value> = replica
        .vault_rows()
        .unwrap_or_default()
        .into_iter()
        .map(|v| json!({ "path": v.path, "size": v.size, "deleted_at": v.deleted_at }))
        .collect();
    let known_peers = replica.known_peers().unwrap_or_default();
    let connected = replica
        .get_meta("session_peer")
        .ok()
        .flatten()
        .unwrap_or_default();
    let connection = match known_peers.iter().find(|p| p.dev == connected) {
        Some(peer) => json!({
            "connected": true,
            "dev": peer.dev,
            "name": peer.name,
            "addr": peer.addr,
        }),
        None => json!({ "connected": false }),
    };
    let known: Vec<Value> = known_peers
        .into_iter()
        .map(|p| json!({ "dev": p.dev, "addr": p.addr, "name": p.name }))
        .collect();
    let last_scan = replica
        .get_meta("last_scan")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok());
    let last_sync = replica
        .get_meta("last_sync")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok());

    json!({
        "device": replica.device_id(),
        "name": replica.device_name(),
        "root": root_text,
        "files": entries.len(),
        "local": local,
        "size": replica.total_live_bytes().unwrap_or(0),
        "ops": replica.op_count().unwrap_or(0),
        "pending": pending,
        "conflicts": replica.conflict_paths().unwrap_or_default(),
        "vault": vault,
        "known_peers": known,
        "connection": connection,
        "last_scan": last_scan,
        "last_sync": last_sync,
        "ignored": replica.ignored_paths().unwrap_or_default(),
        "ready": root.is_some(),
    })
}

async fn blocking<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("фоновая задача упала: {e}"))?
}

// ------------------------------------------------------------------ команды

#[tauri::command]
fn app_info(app: tauri::AppHandle) -> Value {
    let info = {
        let replica = open(&app);
        match replica {
            Ok(r) => json!({
                "data_dir": data_dir(&app).map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
                "device": r.device_id(),
                "name": r.device_name(),
                "root": r.root().ok().flatten().map(|p| p.to_string_lossy().to_string()),
            }),
            Err(e) => json!({ "error": e }),
        }
    };
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "platform": if cfg!(target_os = "android") { "android" } else if cfg!(windows) { "windows" } else { "desktop" },
        "default_root": default_root(),
        "candidates": root_candidates(),
        "sync_port": SYNC_PORT,
        "info": info,
    })
}

#[tauri::command]
async fn status(app: tauri::AppHandle) -> Result<Value, String> {
    blocking(move || {
        let replica = open(&app)?;
        Ok(status_json(&replica))
    })
    .await
}

#[tauri::command]
async fn init_library(app: tauri::AppHandle, root: String) -> Result<Value, String> {
    blocking(move || {
        let path = PathBuf::from(root.trim());
        if !path.is_dir() {
            return Err(format!("папка не найдена: {}", path.display()));
        }
        let mut replica = open(&app)?;
        replica.set_root(&path).map_err(err)?;
        let report = scan::scan(&path, &mut replica).map_err(err)?;
        let _ = replica.set_meta("last_scan", &Hlc::wall_ms().to_string());
        Ok(json!({
            "added": report.added,
            "status": status_json(&replica),
        }))
    })
    .await
}

#[tauri::command]
async fn change_root(app: tauri::AppHandle, root: String) -> Result<Value, String> {
    init_library(app, root).await
}

#[tauri::command]
async fn scan_now(app: tauri::AppHandle, state: tauri::State<'_, Mobile>) -> Result<Value, String> {
    {
        let mut busy = state.busy.lock().unwrap();
        if *busy {
            return Err("скан уже идёт".into());
        }
        *busy = true;
    }
    let result = blocking(move || {
        let mut replica = open(&app)?;
        let root = replica
            .root()
            .map_err(err)?
            .ok_or("библиотека не настроена")?;
        let report = scan::scan(&root, &mut replica).map_err(err)?;
        let _ = replica.set_meta("last_scan", &Hlc::wall_ms().to_string());
        Ok(json!({
            "added": report.added,
            "changed": report.changed,
            "renamed": report.renamed,
            "missing": report.missing.len(),
            "busy": report.skipped_busy,
            "status": status_json(&replica),
        }))
    })
    .await;
    *state.busy.lock().unwrap() = false;
    result
}

#[tauri::command]
async fn sync_now(app: tauri::AppHandle, state: tauri::State<'_, Mobile>) -> Result<Value, String> {
    {
        let mut busy = state.busy.lock().unwrap();
        if *busy {
            return Err("синхронизация уже идёт".into());
        }
        *busy = true;
    }
    let result = blocking(move || {
        let mut replica = open(&app)?;
        let root = replica
            .root()
            .map_err(err)?
            .ok_or("библиотека не настроена")?;
        scan::scan(&root, &mut replica).map_err(err)?;

        let connected = replica
            .get_meta("session_peer")
            .map_err(err)?
            .unwrap_or_default();
        if connected.is_empty() {
            return Err("Нет соединения: сначала подключитесь к устройству".into());
        }
        let peer = replica
            .known_peers()
            .map_err(err)?
            .into_iter()
            .find(|p| p.dev == connected)
            .ok_or("сохранённое устройство не найдено")?;

        let token = replica.ensure_token().map_err(err)?;
        let stats = net::sync_with_peer(&mut replica, &peer.addr, &token).map_err(err)?;
        let _ = replica.set_meta("last_sync", &Hlc::wall_ms().to_string());
        let saved = resolve::prune_deleted_files(&mut replica, &root).map_err(err)?;
        Ok(json!({
            "sent": stats.sent_remote_ops,
            "received": stats.applied_remote_ops,
            "errors": Vec::<String>::new(),
            "rescued": saved,
            "status": status_json(&replica),
        }))
    })
    .await;
    *state.busy.lock().unwrap() = false;
    result
}

#[tauri::command]
async fn connect_peer(app: tauri::AppHandle, dev: String) -> Result<Value, String> {
    blocking(move || {
        let mut replica = open(&app)?;
        let peer = replica
            .known_peers()
            .map_err(err)?
            .into_iter()
            .find(|p| p.dev == dev)
            .ok_or_else(|| format!("устройство не сохранено: {dev}"))?;
        let token = replica.ensure_token().map_err(err)?;
        let mut addr = peer.addr.clone();
        let hello = match net::probe_peer(&mut replica, &addr, &token) {
            Ok(hello) => hello,
            Err(first) => {
                let found = net::discover(Duration::from_secs(2))
                    .unwrap_or_default()
                    .into_iter()
                    .find(|(found_dev, _)| *found_dev == dev);
                match found {
                    Some((_, new_addr)) => {
                        addr = new_addr;
                        net::probe_peer(&mut replica, &addr, &token)
                            .map_err(|e| format!("{first:#}; повтор по {addr}: {e:#}"))?
                    }
                    None => return Err(err(first)),
                }
            }
        };
        let name = if hello.name.is_empty() {
            peer.name
        } else {
            hello.name
        };
        replica
            .set_meta("session_peer", &hello.dev)
            .map_err(err)?;
        Ok(json!({
            "dev": hello.dev,
            "name": name,
            "addr": addr,
            "status": status_json(&replica),
        }))
    })
    .await
}

#[tauri::command]
fn disconnect_peer(app: tauri::AppHandle) -> Result<Value, String> {
    let replica = open(&app)?;
    replica.set_meta("session_peer", "").map_err(err)?;
    Ok(status_json(&replica))
}

#[tauri::command]
fn forget_peer(app: tauri::AppHandle, dev: String) -> Result<Value, String> {
    let replica = open(&app)?;
    replica.remove_peer(&dev).map_err(err)?;
    let session = replica
        .get_meta("session_peer")
        .map_err(err)?
        .unwrap_or_default();
    if session == dev {
        replica.set_meta("session_peer", "").map_err(err)?;
    }
    Ok(status_json(&replica))
}

#[tauri::command]
fn pair_code(app: tauri::AppHandle) -> Result<Value, String> {
    let replica = open(&app)?;
    let token = replica.ensure_token().map_err(err)?;
    let addr = net::advertised_addr(SYNC_PORT);
    let code = net::pair_code(replica.device_id(), replica.device_name(), &token, &addr);
    Ok(json!({
        "code": code,
        "addr": addr,
        "dev": replica.device_id(),
        "name": replica.device_name(),
    }))
}

#[tauri::command]
async fn pair_accept(app: tauri::AppHandle, code: String) -> Result<Value, String> {
    blocking(move || {
        let info = net::parse_pair_code(&code).map_err(err)?;
        let mut replica = open(&app)?;
        replica.set_token(&info.token).map_err(err)?;
        replica.add_peer(&info.dev, &info.addr, &info.name).map_err(err)?;
        // Сразу сообщаем второй стороне: иначе она увидит сопряжение только
        // после первой синхронизации (или не увидит вовсе, если её не запускали).
        let announced = net::announce(&mut replica, &info.addr, &info.token).is_ok();
        Ok(json!({
            "dev": info.dev,
            "name": info.name,
            "addr": info.addr,
            "announced": announced,
        }))
    })
    .await
}

#[tauri::command]
async fn resolve_pending(
    app: tauri::AppHandle,
    path: String,
    action: String,
) -> Result<Value, String> {
    blocking(move || {
        let mut replica = open(&app)?;
        let action = match action.as_str() {
            "delete_everywhere" => Action::DeleteEverywhere,
            "only_here" => Action::OnlyHere,
            other => return Err(format!("неизвестное действие: {other}")),
        };
        let outcome = resolve::resolve(&mut replica, &path, action).map_err(err)?;
        if let Some(root) = replica.root().map_err(err)? {
            resolve::prune_deleted_files(&mut replica, &root).map_err(err)?;
        }
        Ok(json!({
            "deleted": outcome.deleted_paths,
            "ignored": outcome.ignored_paths,
            "failed": outcome.failed,
            "status": status_json(&replica),
        }))
    })
    .await
}

#[tauri::command]
fn forget_pending(app: tauri::AppHandle, path: String) -> Result<Value, String> {
    let replica = open(&app)?;
    replica.clear_pending_delete(&path).map_err(err)?;
    Ok(status_json(&replica))
}

#[tauri::command]
async fn restore(app: tauri::AppHandle, path: String) -> Result<Value, String> {
    blocking(move || {
        let mut replica = open(&app)?;
        resolve::restore(&mut replica, &path).map_err(err)?;
        Ok(status_json(&replica))
    })
    .await
}

#[tauri::command]
fn set_ignored(app: tauri::AppHandle, path: String, ignored: bool) -> Result<Value, String> {
    let replica = open(&app)?;
    replica.set_ignored(&path, ignored).map_err(err)?;
    Ok(status_json(&replica))
}

#[tauri::command]
fn vault_gc(app: tauri::AppHandle) -> Result<Value, String> {
    let replica = open(&app)?;
    let report = syncth_core::vault::gc(&replica, 30).map_err(err)?;
    Ok(json!({ "removed": report.rows_removed, "status": status_json(&replica) }))
}

/// Проверка «эта папка вообще существует и читается» до инициализации.
#[tauri::command]
fn probe_root(path: String) -> Value {
    let path = Path::new(path.trim());
    json!({
        "exists": path.is_dir(),
        "path": path.to_string_lossy(),
        "candidates": root_candidates(),
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Mobile::default())
        // Отладочный канал для прогона интерфейса без кликов; в релизе нет.
        .setup(|app| {
            // Сессия живёт только пока приложение открыто: сокет не переживает
            // перезапуск, поэтому «подключено» из прошлого запуска не сохраняем.
            if let Ok(replica) = open(app.handle()) {
                let _ = replica.set_meta("session_peer", "");
            }
            #[cfg(debug_assertions)]
            if let Ok(script) = std::env::var("SYNCTH_UI_EVAL") {
                use tauri::Manager;
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(2500));
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
            status,
            init_library,
            change_root,
            scan_now,
            sync_now,
            pair_code,
            pair_accept,
            connect_peer,
            disconnect_peer,
            forget_peer,
            resolve_pending,
            forget_pending,
            restore,
            set_ignored,
            vault_gc,
            probe_root,
        ])
        .run(tauri::generate_context!())
        .expect("не удалось запустить приложение");
}
