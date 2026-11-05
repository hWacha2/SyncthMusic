//! Android-клиент SynctMusic.
//!
//! В отличие от настольной версии здесь нет отдельного демона: ядро
//! (`syncth-core`) живёт прямо в приложении. Так проще с точки зрения Android —
//! нет второго процесса, которым надо управлять, и нет HTTP-прослойки.
//!
//! Что уже работает: инициализация библиотеки на выбранной папке, скан,
//! исходящая синхронизация с сопряжённым пиром, входящие сеансы и commit, пока
//! приложение открыто, очередь удалений, корзина, конфликты, список устройств.
//! После записи файлов Android просят перечитать библиотеку. Foreground-сервис
//! для работы в фоне появится позже.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use syncth_core::plan::{self, Decision, PathDecision};
use syncth_core::{net, resolve, scan, store::Replica, Action, Hlc};
use tauri::Manager;

mod agent;
mod media;
mod server;

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
    // Имя пишем только при изменении: иначе каждый опрос статуса открывал бы
    // запись в SQLite и мешал сканированию.
    let name = phone_name();
    if replica.device_name() != name.as_str() {
        replica.set_device_name(&name).map_err(err)?;
    }
    Ok(replica)
}

/// Записывает корень в общий журнал и сразу сканирует папку.
///
/// Это нужно не только интерфейсу: полная синхронизация на телефоне живёт
/// в том же реплике, поэтому выбранная папка должна быть видна и серверу
/// `47887`, и агентскому соединению.
pub(crate) fn sync_root(app: &tauri::AppHandle, root: &Path) -> Result<scan::ScanReport, String> {
    if !root.is_dir() {
        return Err(format!("Папка не найдена: {}", root.display()));
    }
    let mut replica = open(app)?;
    replica.set_root(root).map_err(err)?;
    let report = scan::scan(root, &mut replica).map_err(err)?;
    let _ = replica.set_meta("last_scan", &Hlc::wall_ms().to_string());
    Ok(report)
}

/// Сканирует текущую папку телефона в реплику и сообщает о ходе: агент
/// пересылает его ПК, чтобы там был виден прогресс долгой проверки.
pub(crate) fn scan_phone_with_progress(
    app: &tauri::AppHandle,
    progress: &mut dyn FnMut(scan::ScanProgress),
) -> Result<(PathBuf, scan::ScanReport), String> {
    let mut replica = open(app)?;
    let root = replica
        .root()
        .map_err(err)?
        .ok_or("Папка на телефоне не выбрана")?;
    let report = scan::scan_with_progress(&root, &mut replica, progress).map_err(err)?;
    let _ = replica.set_meta("last_scan", &Hlc::wall_ms().to_string());
    Ok((root, report))
}

/// Забывает историю на телефоне и сразу пересобирает индекс из файлов.
/// Сопряжение, токен и выбранная папка не трогаются.
pub(crate) fn reset_phone_history(
    app: &tauri::AppHandle,
    progress: &mut dyn FnMut(scan::ScanProgress),
) -> Result<(PathBuf, scan::ScanReport), String> {
    let mut replica = open(app)?;
    let root = replica
        .root()
        .map_err(err)?
        .ok_or("Папка на телефоне не выбрана")?;
    replica.reset_history().map_err(err)?;
    let report = scan::scan_with_progress(&root, &mut replica, progress).map_err(err)?;
    let _ = replica.set_meta("last_scan", &Hlc::wall_ms().to_string());
    Ok((root, report))
}

/// Привязывает токен полной синхронизации к токену агентской пары.
/// Без этого `PreviewSession` с ПК не пройдёт проверку токена.
pub(crate) fn set_sync_token(app: &tauri::AppHandle, token: &str) -> Result<(), String> {
    let replica = open(app)?;
    replica.set_token(token).map_err(err)
}

/// Читает системное свойство Android. На других платформах возвращает пустую строку.
fn android_prop(name: &str) -> String {
    if !cfg!(target_os = "android") {
        return String::new();
    }
    std::process::Command::new("getprop")
        .arg(name)
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Имя телефона для интерфейса и для второго устройства. В отличие от
/// `device_id`, имя ничего не идентифицирует и может быть любым.
///
/// Берём маркетинговое название, а модель добавляем в скобках: на телефоне
/// оно обычно понятнее, чем артикул вроде `2511FPC34G`.
fn phone_name() -> String {
    use std::sync::OnceLock;
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        let model = android_prop("ro.product.model");
        let market = ["ro.product.marketname", "ro.product.odm.marketname", "ro.product.vendor.marketname"]
            .iter()
            .map(|prop| android_prop(prop))
            .find(|value| !value.is_empty())
            .unwrap_or_default();

        let same = market.eq_ignore_ascii_case(&model)
            || (!model.is_empty() && market.to_lowercase().contains(&model.to_lowercase()));

        if !market.is_empty() && !model.is_empty() && !same {
            format!("{market} ({model})")
        } else if !market.is_empty() {
            market
        } else if !model.is_empty() {
            model
        } else {
            syncth_core::device_name()
        }
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

fn browse_start() -> PathBuf {
    if cfg!(target_os = "android") {
        PathBuf::from("/storage/emulated/0")
    } else {
        PathBuf::from(default_root())
    }
}

/// Обычный список папок для выбора библиотеки. Android SAF отдаёт URI, а ядру
/// нужен путь, поэтому телефон использует собственный браузер каталогов.
#[tauri::command]
async fn browse_dirs(path: Option<String>) -> Result<Value, String> {
    blocking(move || {
        let dir = path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(browse_start);

        if !dir.is_dir() {
            return Err(format!("папка не найдена: {}", dir.display()));
        }

        let parent = dir.parent().and_then(|candidate| {
            if candidate.is_dir() && candidate != dir.as_path() {
                Some(candidate.to_string_lossy().to_string())
            } else {
                None
            }
        });

        let mut entries: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
            .map_err(|e| format!("не удалось прочитать {}: {e}", dir.display()))?
            .flatten()
            .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                (!name.starts_with('.')).then(|| (name, entry.path()))
            })
            .collect();
        entries.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

        let entries: Vec<Value> = entries
            .into_iter()
            .map(|(name, path)| json!({ "name": name, "path": path.to_string_lossy() }))
            .collect();

        Ok(json!({
            "path": dir.to_string_lossy(),
            "parent": parent,
            "entries": entries,
        }))
    })
    .await
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
    let connection = match server::session_json() {
        Some(incoming) => incoming,
        None => match known_peers.iter().find(|p| p.dev == connected) {
            Some(peer) => json!({
                "connected": true,
                "dev": peer.dev,
                "name": peer.name,
                "addr": peer.addr,
            }),
            None => json!({ "connected": false }),
        },
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
        "listen": server::info_json(),
        "last_scan": last_scan,
        "last_sync": last_sync,
        "ignored": replica.ignored_paths().unwrap_or_default(),
        "ready": root.is_some(),
    })
}

/// Устройство, с которым открыт живой сеанс. На телефоне это входящее
/// соединение: исходящее пока только проверяет доступность пира.
fn session_dev() -> Option<String> {
    server::session_json()
        .and_then(|session| session.get("dev").and_then(Value::as_str).map(str::to_string))
}

fn plan_payload(replica: &Replica) -> Value {
    let Some(dev) = session_dev() else {
        // Без сеанса план всё равно нужен: человек видит локальные изменения,
        // но решения и commit недоступны.
        return match plan::build(replica, "", &replica.device_name(), &[]) {
            Ok(plan) => json!({
                "connected": false,
                "local_only": true,
                "stale": false,
                "plan": plan,
                "hash": plan.decisions_hash(),
                "unresolved": plan.unresolved().len(),
                "paths": plan.path_count(),
                "session": Value::Null,
            }),
            Err(e) => json!({
                "connected": false,
                "local_only": true,
                "stale": false,
                "plan": Value::Null,
                "error": format!("{e:#}"),
                "session": Value::Null,
            }),
        };
    };

    match replica.load_plan(&dev, Hlc::wall_ms()) {
        Ok(Some(plan)) => {
            let stale = !plan.local_is_current(replica).unwrap_or(false);
            json!({
                "connected": true,
                "local_only": false,
                "stale": stale,
                "plan": plan,
                "hash": plan.decisions_hash(),
                "unresolved": plan.unresolved().len(),
                "paths": plan.path_count(),
                "session": server::session_json(),
            })
        }
        Ok(None) => json!({
            "connected": true,
            "local_only": false,
            "stale": false,
            "plan": Value::Null,
            "session": server::session_json(),
        }),
        Err(e) => json!({
            "connected": true,
            "local_only": false,
            "stale": false,
            "plan": Value::Null,
            "error": format!("{e:#}"),
            "session": server::session_json(),
        }),
    }
}

/// Записать решение в план сеанса. Отправка пиру происходит в потоке сеанса:
/// он периодически забирает новые решения из базы.
fn save_decision(
    replica: &mut Replica,
    peer_dev: &str,
    path: &str,
    action: Decision,
) -> Result<(), String> {
    let mut plan = replica
        .load_plan(peer_dev, Hlc::wall_ms())
        .map_err(err)?
        .ok_or_else(|| "План не найден: обновите соединение".to_string())?;
    let blob = plan
        .blob_for(path)
        .ok_or_else(|| format!("Путь отсутствует в плане: {path}"))?;
    let hlc = replica.next_hlc().map_err(err)?;
    let decision = PathDecision {
        path: path.to_string(),
        action,
        blob,
        hlc,
        dev: replica.device_id().to_string(),
    };
    let mut set = replica.load_decisions(peer_dev).map_err(err)?;
    set.entries.insert(path.to_string(), decision);
    set.retain_valid(&plan);
    replica.save_decisions(peer_dev, &set).map_err(err)?;
    plan.decisions = set;
    replica.save_plan(&plan).map_err(err)?;
    Ok(())
}

pub(crate) async fn blocking<T, F>(f: F) -> Result<T, String>
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
async fn sync_plan(app: tauri::AppHandle) -> Result<Value, String> {
    blocking(move || {
        let replica = open(&app)?;
        Ok(plan_payload(&replica))
    })
    .await
}

#[tauri::command]
async fn set_plan_decision(
    app: tauri::AppHandle,
    path: String,
    action: String,
) -> Result<Value, String> {
    blocking(move || {
        let peer_dev = session_dev()
            .ok_or_else(|| "Нет соединения: решения недоступны".to_string())?;
        let action = Decision::parse(&action)
            .ok_or_else(|| format!("Неизвестное решение: {action}"))?;
        let mut replica = open(&app)?;
        save_decision(&mut replica, &peer_dev, &path, action)?;
        Ok(plan_payload(&replica))
    })
    .await
}

#[tauri::command]
async fn commit_plan(app: tauri::AppHandle) -> Result<Value, String> {
    blocking(move || {
        let Some(result) = server::request_commit() else {
            return Err("Нет сеанса: попросите ПК подключиться к телефону".into());
        };
        let (accepted, rejected) = result?;
        let replica = open(&app)?;
        let _ = replica.set_meta("last_sync", &Hlc::wall_ms().to_string());
        Ok(json!({
            "accepted": accepted,
            "rejected": rejected,
            "plan": plan_payload(&replica),
            "status": status_json(&replica),
        }))
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
        // Входящий сеанс обслуживает свой поток: commit надо попросить у него,
        // а не открывать второе соединение к тому же пиру.
        if server::has_session() {
            let (accepted, rejected) = server::request_commit()
                .unwrap_or_else(|| Err("Входящий сеанс уже закрыт".into()))?;
            let replica = open(&app)?;
            let _ = replica.set_meta("last_sync", &Hlc::wall_ms().to_string());
            return Ok(json!({
                "accepted": accepted,
                "rejected": rejected,
                "status": status_json(&replica),
            }));
        }

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
        // Новые файлы появились через std::fs — Android узнает о них только
        // после явного сканирования.
        let _ = media::scan(&root);
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
    // Входящий сеанс закрывает его собственный поток.
    server::request_disconnect();
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
        .manage(agent::AgentRuntime::default())
        .setup(|app| {
            // Сессия живёт только пока приложение открыто: сокет не переживает
            // перезапуск, поэтому «подключено» из прошлого запуска не сохраняем.
            if let Ok(replica) = open(app.handle()) {
                let _ = replica.set_meta("session_peer", "");
            }
            // Восстанавливаем выбранную папку и подставляем токен пары
            // в общий реплик — без этого сеанс на 47887 не пройдёт.
            agent::startup(app.handle().clone());
            // Приёмник полной синхронизации живёт всё время работы окна:
            // ПК подключается к нему сразу после агентского сеанса.
            server::ensure_started(app.handle().clone());
            // Запускаем минимальный агент: он слушает ПК и отдаёт выбранную папку.
            agent::init(app.handle().clone());
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            browse_dirs,
            agent::agent_info,
            agent::agent_set_root,
            agent::agent_cancel_root,
            agent::agent_pair_decision,
            agent::agent_forget,
            status,
            sync_plan,
            set_plan_decision,
            commit_plan,
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
