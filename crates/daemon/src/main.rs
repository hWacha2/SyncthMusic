//! `syncthd` — фоновый демон для ПК.
//!
//! Задачи, которые он забирает у пользователя:
//! * следит за папкой библиотеки и сам запускает скан после паузы;
//! * периодически ищет пиров в LAN и синхронизируется с ними;
//! * принимает входящие сеансы;
//! * отдаёт локальную веб-страницу на `127.0.0.1` — очередь удалений,
//!   конфликты, корзина, состояние;
//! * умеет прописаться в автозапуск Windows.
//!
//! Вся мутация реплики идёт через один рабочий поток: у `Replica` один
//! SQLite-коннектор, а наблюдатель, сеть и веб-UI лишь шлют ему команды.
//! UI читает состояние через отдельное соединение (WAL это позволяет).

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::Watcher;
use syncth_core::{net, resolve, scan, vault, Action, Hlc, Replica};

#[derive(Parser, Debug)]
#[command(
    name = "syncthd",
    version,
    about = "Фоновый демон синхронизации музыкальной библиотеки"
)]
struct Cli {
    /// Каталог состояния (БД, корзина). Не должен лежать внутри библиотеки.
    #[arg(long, default_value = ".syncth-data")]
    data_dir: PathBuf,

    /// Корень библиотеки. Если не указан — берётся из `syncth init`.
    #[arg(long)]
    root: Option<PathBuf>,

    /// Адрес для входящих сеансов синхронизации.
    #[arg(long, default_value = "0.0.0.0:47887")]
    bind: String,

    /// Адрес локального веб-UI.
    #[arg(long, default_value = "127.0.0.1:47889")]
    ui: String,

    /// Общий токен библиотеки. По умолчанию — из БД.
    #[arg(long)]
    token: Option<String>,

    /// Как часто автоматически искать пиров, секунды.
    #[arg(long, default_value_t = 30)]
    sync_interval: u64,

    /// Сколько дней хранить удалённое в корзине.
    #[arg(long, default_value_t = vault::DEFAULT_TTL_DAYS)]
    vault_days: u64,

    /// Синхронизироваться с этим адресом, даже если discovery молчит.
    /// Можно указать несколько раз.
    #[arg(long = "peer")]
    peers: Vec<String>,

    /// Не следить за папкой (скан только по кнопке).
    #[arg(long)]
    no_watch: bool,

    /// Прописать демон в автозапуск Windows и выйти.
    #[arg(long)]
    install_autostart: bool,

    /// Убрать демон из автозапуска и выйти.
    #[arg(long)]
    uninstall_autostart: bool,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    if let Err(e) = run() {
        eprintln!("ошибка: {e:#}");
        std::process::exit(1);
    }
}

// ------------------------------------------------------------------ runtime state

#[derive(Debug, Clone, Default, Serialize)]
struct PeerStatus {
    addr: String,
    last_seen: Option<u64>,
    error: Option<String>,
}

#[derive(Debug, Default)]
struct Runtime {
    paused: bool,
    syncing: bool,
    last_scan: Option<u64>,
    last_sync: Option<u64>,
    last_error: Option<String>,
    peers: BTreeMap<String, PeerStatus>,
}

type Shared = Arc<Mutex<Runtime>>;

impl Runtime {
    fn error(&mut self, message: impl Into<String>) {
        let message = message.into();
        tracing::warn!("{message}");
        self.last_error = Some(message);
    }
}

// ------------------------------------------------------------------ commands

type Reply = Sender<Result<String, String>>;

enum Cmd {
    Scan(Reply),
    Sync(Reply),
    Resolve {
        path: String,
        action: Action,
        reply: Reply,
    },
    Restore {
        path: String,
        reply: Reply,
    },
    SetIgnored {
        path: String,
        ignored: bool,
        reply: Reply,
    },
    Forget {
        path: String,
        reply: Reply,
    },
    Gc(Reply),
    Pause {
        paused: bool,
        reply: Reply,
    },
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    if cli.install_autostart {
        return install_autostart(&cli.data_dir, cli.root.as_deref());
    }
    if cli.uninstall_autostart {
        return uninstall_autostart();
    }

    let dev = std::env::var("SYNCTH_DEV").ok();
    let dev = dev.unwrap_or_else(|| default_device_id(&cli.data_dir));
    let replica = Replica::open(&cli.data_dir, &dev)?;
    let dev = replica.device_id().to_string();

    let root = match cli.root.clone() {
        Some(r) => {
            let r = absolute(&r)?;
            replica.set_root(&r)?;
            r
        }
        None => replica
            .root()?
            .context("корень не задан: укажите --root или выполните syncth init")?,
    };
    if !root.is_dir() {
        bail!("корень библиотеки не найден: {}", root.display());
    }
    if absolute(&cli.data_dir)?.starts_with(&root) {
        bail!("data-dir не должен находиться внутри библиотеки");
    }

    let token = match cli
        .token
        .clone()
        .or_else(|| std::env::var("SYNCTH_TOKEN").ok())
    {
        Some(t) => t,
        None => replica.ensure_token()?,
    };

    let listener =
        std::net::TcpListener::bind(&cli.bind).with_context(|| format!("bind {}", cli.bind))?;
    let port = listener.local_addr()?.port();
    net::start_discovery(dev.clone(), port)?;

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (stream_tx, stream_rx) = mpsc::channel::<TcpStream>();
    let (fs_tx, fs_rx) = mpsc::sync_channel::<()>(1);

    // Входящие соединения складываем в очередь серверному потоку.
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    if stream_tx.send(s).is_err() {
                        break;
                    }
                }
                Err(e) => tracing::warn!("accept: {e}"),
            }
        }
    });

    let _watcher = if cli.no_watch {
        None
    } else {
        Some(watch_root(&root, fs_tx.clone())?)
    };

    let runtime: Shared = Arc::new(Mutex::new(Runtime::default()));
    let cfg = WorkerConfig {
        root: root.clone(),
        token: token.clone(),
        sync_interval: Duration::from_secs(cli.sync_interval.max(5)),
        vault_days: cli.vault_days,
        explicit_peers: cli.peers.clone(),
    };

    let rt = runtime.clone();
    std::thread::spawn(move || worker(replica, cfg, cmd_rx, fs_rx, rt));

    // Входящие сеансы обслуживает отдельный поток со своим соединением к БД.
    // Иначе демон, который сам ждёт ответа от пира, не смог бы принять
    // встречное подключение — два устройства зависли бы друг на друге.
    {
        let data_dir = cli.data_dir.clone();
        let dev = dev.clone();
        let token = token.clone();
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            let mut replica = match Replica::open(&data_dir, &dev) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("сервер синхронизации не стартовал: {e:#}");
                    return;
                }
            };
            while let Ok(stream) = stream_rx.recv() {
                runtime.lock().unwrap().syncing = true;
                finish_sync(
                    &runtime,
                    net::serve_stream(stream, &mut replica, &token).map(|s| s.summary()),
                );
            }
        });
    }

    // UI-поток читает БД отдельным соединением: WAL допускает читателей
    // параллельно с записью рабочего потока.
    let read = Arc::new(Mutex::new(Replica::open(&cli.data_dir, &dev)?));
    let ui_ctx = UiCtx {
        commands: cmd_tx,
        read,
        runtime,
        root,
        dev: dev.clone(),
    };
    println!("syncthd запущен");
    println!("  устройство: {dev}");
    println!("  корень:     {}", ui_ctx.root.display());
    println!("  синк:       {}", cli.bind);
    println!("  UI:         http://{}", cli.ui);
    println!("  данные:     {}", absolute(&cli.data_dir)?.display());
    serve_ui(&cli.ui, ui_ctx)
}

// ------------------------------------------------------------------ worker

struct WorkerConfig {
    root: PathBuf,
    token: String,
    sync_interval: Duration,
    vault_days: u64,
    explicit_peers: Vec<String>,
}

fn worker(
    mut replica: Replica,
    cfg: WorkerConfig,
    commands: Receiver<Cmd>,
    fs_events: Receiver<()>,
    runtime: Shared,
) {
    let root = cfg.root.clone();
    let mut pending_scan = false;
    let mut scan_at = Instant::now();
    let mut next_sync = Instant::now() + cfg.sync_interval;

    // Первичный скан: догоняем изменения, сделанные при выключенном демоне.
    if let Err(e) = do_scan(&mut replica, &root, &runtime) {
        runtime.lock().unwrap().error(format!("скан: {e:#}"));
    }
    if let Err(e) = vault::gc(&replica, cfg.vault_days) {
        tracing::warn!("gc корзины: {e:#}");
    }
    let mut last_gc = Instant::now();

    loop {
        tracing::trace!("worker loop");
        match commands.recv_timeout(Duration::from_millis(200)) {
            Ok(Cmd::Scan(reply)) => {
                let res =
                    do_scan(&mut replica, &root, &runtime).map(|_| "скан завершён".to_string());
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Sync(reply)) => {
                let res = do_sync(&mut replica, &cfg, &runtime, true);
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Resolve {
                path,
                action,
                reply,
            }) => {
                let res = resolve::resolve(&mut replica, &path, action).map(|out| match action {
                    Action::DeleteEverywhere => format!(
                        "удалено путей: {}; копия в корзине",
                        out.deleted_paths.len()
                    ),
                    Action::OnlyHere => {
                        format!("«{path}» убрано только с этого устройства")
                    }
                });
                if res.is_ok() {
                    let _ = resolve::prune_deleted_files(&mut replica, &root);
                }
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Restore { path, reply }) => {
                let res = resolve::restore(&mut replica, &path)
                    .map(|_| format!("«{path}» снова в библиотеке"));
                respond(reply, res, &runtime);
            }
            Ok(Cmd::SetIgnored {
                path,
                ignored,
                reply,
            }) => {
                let res = (|| -> Result<String> {
                    if replica.entry(&path)?.is_none() {
                        bail!("путь не найден в библиотеке: {path}");
                    }
                    replica.set_ignored(&path, ignored)?;
                    if ignored {
                        replica.clear_pending_delete(&path)?;
                        Ok(format!("«{path}» больше не держим здесь"))
                    } else {
                        replica.mark_absent(&path)?;
                        Ok(format!("«{path}» снова ожидается здесь"))
                    }
                })();
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Forget { path, reply }) => {
                let res = replica
                    .clear_pending_delete(&path)
                    .map(|_| format!("ожидание для «{path}» снято"));
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Gc(reply)) => {
                let res = vault::gc(&replica, cfg.vault_days).map(|r| {
                    format!(
                        "корзина: удалено записей {}, файлов {}, сирот {}",
                        r.rows_removed, r.blobs_removed, r.orphans_removed
                    )
                });
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Pause { paused, reply }) => {
                runtime.lock().unwrap().paused = paused;
                let _ = reply.send(Ok(if paused {
                    "автосинк на паузе".into()
                } else {
                    "автосинк включён".into()
                }));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        // Входящие сеансы обслуживает отдельный поток (см. `run`), чтобы
        // встречные подключения не зависели от нашего исходящего синка.
        while fs_events.try_recv().is_ok() {
            pending_scan = true;
            scan_at = Instant::now() + Duration::from_secs(2);
        }

        if pending_scan && Instant::now() >= scan_at {
            pending_scan = false;
            if let Err(e) = do_scan(&mut replica, &root, &runtime) {
                runtime.lock().unwrap().error(format!("скан: {e:#}"));
            }
        }

        let now = Instant::now();
        if now >= next_sync {
            tracing::debug!("пора автосинка");
            next_sync = now + cfg.sync_interval;
            if !runtime.lock().unwrap().paused {
                if let Err(e) = do_sync(&mut replica, &cfg, &runtime, false) {
                    tracing::debug!("автосинк: {e:#}");
                }
            }
        }

        if last_gc.elapsed() >= Duration::from_secs(24 * 60 * 60) {
            last_gc = Instant::now();
            match vault::gc(&replica, cfg.vault_days) {
                Ok(r) if r.rows_removed + r.blobs_removed + r.orphans_removed > 0 => {
                    tracing::info!(
                        "корзина: удалено записей {}, файлов {}",
                        r.rows_removed,
                        r.blobs_removed
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("gc корзины: {e:#}"),
            }
        }
    }
}

fn respond(reply: Reply, res: Result<String>, runtime: &Shared) {
    if let Err(e) = &res {
        runtime.lock().unwrap().error(format!("{e:#}"));
    }
    let _ = reply.send(res.map_err(|e| format!("{e:#}")));
}

fn do_scan(replica: &mut Replica, root: &Path, runtime: &Shared) -> Result<()> {
    let report = scan::scan(root, replica)?;
    let saved = resolve::prune_deleted_files(replica, root)?;
    let mut rt = runtime.lock().unwrap();
    rt.last_scan = Some(Hlc::wall_ms());
    rt.last_error = None;
    drop(rt);
    tracing::info!(
        "скан: +{} новых, ~{} изменённых, {} без изменений, пропало {}",
        report.added,
        report.changed,
        report.unchanged,
        report.missing.len()
    );
    if !report.missing.is_empty() {
        tracing::warn!(
            "пропало с диска: {} — решение в веб-UI",
            report.missing.len()
        );
    }
    if !saved.is_empty() {
        tracing::warn!("сохранено конфликтных копий: {}", saved.len());
    }
    Ok(())
}

fn do_sync(
    replica: &mut Replica,
    cfg: &WorkerConfig,
    runtime: &Shared,
    manual: bool,
) -> Result<String> {
    let me = replica.device_id().to_string();
    let mut targets: Vec<(String, String)> = cfg
        .explicit_peers
        .iter()
        .map(|a| (a.clone(), a.clone()))
        .collect();

    match net::discover(Duration::from_secs(2)) {
        Ok(found) => {
            for (dev, addr) in found {
                // Себя пропускаем; при автоматическом синке инициатор только
                // один (меньший device id), чтобы не открывать две сессии.
                if dev == me {
                    continue;
                }
                if !manual && me > dev {
                    continue;
                }
                if !targets.iter().any(|(_, a)| *a == addr) {
                    targets.push((dev, addr));
                }
            }
        }
        Err(e) => tracing::debug!("discovery: {e:#}"),
    }

    if targets.is_empty() {
        if manual {
            bail!("никого не нашли: включён ли демон на другом устройстве и та же ли это сеть?");
        }
        return Ok(String::new());
    }

    runtime.lock().unwrap().syncing = true;
    let mut done = Vec::new();
    let mut errors = Vec::new();
    for (dev, addr) in targets {
        match net::sync_with_peer(replica, &addr, &cfg.token) {
            Ok(stats) => {
                done.push(format!("{dev} ({addr}): {}", stats.summary()));
                let mut rt = runtime.lock().unwrap();
                rt.peers.insert(
                    dev,
                    PeerStatus {
                        addr,
                        last_seen: Some(Hlc::wall_ms()),
                        error: None,
                    },
                );
                rt.last_error = None;
            }
            Err(e) => {
                let message = format!("{e:#}");
                errors.push(format!("{dev} ({addr}): {message}"));
                let mut rt = runtime.lock().unwrap();
                rt.peers.insert(
                    dev,
                    PeerStatus {
                        addr,
                        last_seen: None,
                        error: Some(message),
                    },
                );
            }
        }
    }
    {
        let mut rt = runtime.lock().unwrap();
        rt.syncing = false;
        if !done.is_empty() {
            rt.last_sync = Some(Hlc::wall_ms());
        }
    }

    if done.is_empty() {
        let all = errors.join("; ");
        runtime.lock().unwrap().last_error = Some(all.clone());
        bail!("синхронизация не удалась: {all}");
    }
    let mut msg = done.join("; ");
    if !errors.is_empty() {
        msg.push_str(&format!(" | ошибки: {}", errors.join("; ")));
        runtime.lock().unwrap().last_error = Some(errors.join("; "));
    }
    Ok(msg)
}

fn finish_sync(runtime: &Shared, result: Result<String>) {
    let mut rt = runtime.lock().unwrap();
    rt.syncing = false;
    match result {
        Ok(summary) => {
            rt.last_sync = Some(Hlc::wall_ms());
            rt.last_error = None;
            tracing::info!("входящий сеанс: {summary}");
        }
        Err(e) => rt.error(format!("входящий сеанс: {e:#}")),
    }
}

// ------------------------------------------------------------------ watcher

fn watch_root(root: &Path, events: mpsc::SyncSender<()>) -> Result<notify::RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if event.paths.iter().any(|p| interesting(p)) {
            let _ = events.try_send(());
        }
    })?;
    watcher.watch(root, notify::RecursiveMode::Recursive)?;
    Ok(watcher)
}

/// Служебные файлы (`*.syncth-part`, `.syncth*`) не повод для скана.
fn interesting(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    !(name.starts_with(".syncth") || name.ends_with(".syncth-part"))
}

// ------------------------------------------------------------------ web ui

struct UiCtx {
    commands: Sender<Cmd>,
    read: Arc<Mutex<Replica>>,
    runtime: Shared,
    root: PathBuf,
    dev: String,
}

fn serve_ui(addr: &str, ctx: UiCtx) -> Result<()> {
    let server = tiny_http::Server::http(addr).map_err(|e| anyhow::anyhow!("{e}"))?;
    let ctx = Arc::new(ctx);
    for request in server.incoming_requests() {
        let ctx = ctx.clone();
        // Долгие команды не должны блокировать опрос состояния.
        std::thread::spawn(move || handle_request(request, &ctx));
    }
    Ok(())
}

fn handle_request(mut request: tiny_http::Request, ctx: &UiCtx) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("/");

    let (code, mime, body) = match (&method, path) {
        (tiny_http::Method::Get, "/") | (tiny_http::Method::Get, "/index.html") => {
            (200, "text/html; charset=utf-8", UI.to_string())
        }
        (tiny_http::Method::Get, "/api/status") => (
            200,
            "application/json; charset=utf-8",
            api_status(ctx).to_string(),
        ),
        (tiny_http::Method::Post, p) => match api_action(ctx, p, &mut request) {
            Ok(msg) => (
                200,
                "application/json; charset=utf-8",
                json!({ "ok": true, "message": msg }).to_string(),
            ),
            Err(e) => (
                400,
                "application/json; charset=utf-8",
                json!({ "ok": false, "error": e }).to_string(),
            ),
        },
        _ => (404, "text/plain; charset=utf-8", "not found".to_string()),
    };

    let response = tiny_http::Response::from_string(body)
        .with_status_code(code)
        .with_header(
            tiny_http::Header::from_bytes("Content-Type", mime).expect("валидный заголовок"),
        )
        .with_header(
            tiny_http::Header::from_bytes("Cache-Control", "no-store").expect("валидный заголовок"),
        );
    let _ = request.respond(response);
}

fn body_json(request: &mut tiny_http::Request) -> Value {
    let mut s = String::new();
    let _ = request.as_reader().take(1 << 20).read_to_string(&mut s);
    serde_json::from_str(&s).unwrap_or(Value::Null)
}

fn str_field(v: &Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("в запросе нет поля «{key}»"))
}

fn api_action(ctx: &UiCtx, path: &str, request: &mut tiny_http::Request) -> Result<String, String> {
    let body = if path == "/api/pause" {
        body_json(request)
    } else {
        Value::Null
    };

    match path {
        "/api/scan" => run_cmd(&ctx.commands, Cmd::Scan),
        "/api/sync" => run_cmd(&ctx.commands, Cmd::Sync),
        "/api/gc" => run_cmd(&ctx.commands, Cmd::Gc),
        "/api/pause" => {
            let paused = body.get("paused").and_then(Value::as_bool).unwrap_or(false);
            run_cmd(&ctx.commands, move |reply| Cmd::Pause { paused, reply })
        }
        "/api/pending/resolve" => {
            let body = body_json(request);
            let path = str_field(&body, "path")?;
            let action = match body.get("action").and_then(Value::as_str) {
                Some("delete_everywhere") => Action::DeleteEverywhere,
                Some("only_here") => Action::OnlyHere,
                _ => return Err("action: delete_everywhere | only_here".into()),
            };
            run_cmd(&ctx.commands, move |reply| Cmd::Resolve {
                path,
                action,
                reply,
            })
        }
        "/api/pending/forget" => {
            let body = body_json(request);
            let path = str_field(&body, "path")?;
            run_cmd(&ctx.commands, move |reply| Cmd::Forget { path, reply })
        }
        "/api/restore" => {
            let body = body_json(request);
            let path = str_field(&body, "path")?;
            run_cmd(&ctx.commands, move |reply| Cmd::Restore { path, reply })
        }
        "/api/ignore" => {
            let body = body_json(request);
            let path = str_field(&body, "path")?;
            let ignored = body.get("ignored").and_then(Value::as_bool).unwrap_or(true);
            run_cmd(&ctx.commands, move |reply| Cmd::SetIgnored {
                path,
                ignored,
                reply,
            })
        }
        other => Err(format!("неизвестный запрос: {other}")),
    }
}

fn run_cmd(tx: &Sender<Cmd>, make: impl FnOnce(Reply) -> Cmd) -> Result<String, String> {
    let (reply, rx) = mpsc::channel();
    tx.send(make(reply))
        .map_err(|_| "демон остановлен".to_string())?;
    rx.recv_timeout(Duration::from_secs(180))
        .map_err(|_| "операция не завершилась за 3 минуты".to_string())?
}

fn api_status(ctx: &UiCtx) -> Value {
    let rt = ctx.runtime.lock().unwrap();
    let r = ctx.read.lock().unwrap();

    let pending: Vec<Value> = r
        .pending_deletes()
        .unwrap_or_default()
        .into_iter()
        .map(|p| json!({ "path": p.path, "detected_at": p.detected_at }))
        .collect();
    let ignored: Vec<String> = r.ignored_paths().unwrap_or_default();
    let conflicts: Vec<String> = r.conflict_paths().unwrap_or_default();
    let vault: Vec<Value> = r
        .vault_rows()
        .unwrap_or_default()
        .into_iter()
        .map(|v| {
            json!({
                "path": v.path,
                "size": v.size,
                "deleted_at": v.deleted_at,
            })
        })
        .collect();

    json!({
        "device": ctx.dev,
        "root": ctx.root.to_string_lossy(),
        "paused": rt.paused,
        "syncing": rt.syncing,
        "last_scan": rt.last_scan,
        "last_sync": rt.last_sync,
        "last_error": rt.last_error,
        "ops": r.op_count().unwrap_or(0),
        "files": r.live_entries().map(|v| v.len()).unwrap_or(0),
        "local": r.local_entries().map(|v| v.len()).unwrap_or(0),
        "size": r.total_live_bytes().unwrap_or(0),
        "pending": pending,
        "ignored": ignored,
        "conflicts": conflicts,
        "vault": vault,
        "peers": rt.peers.values().collect::<Vec<_>>(),
    })
}

// ------------------------------------------------------------------ autostart

#[cfg(windows)]
fn autostart_path() -> Result<PathBuf> {
    let appdata = std::env::var("APPDATA").context("нет APPDATA")?;
    Ok(PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join("syncthd.vbs"))
}

#[cfg(not(windows))]
fn autostart_path() -> Result<PathBuf> {
    bail!("автозапуск реализован только для Windows")
}

fn install_autostart(data_dir: &Path, root: Option<&Path>) -> Result<()> {
    let path = autostart_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let exe = std::env::current_exe()?;
    let mut cmd = format!(
        "\"{}\" --data-dir \"{}\"",
        exe.display(),
        data_dir.display()
    );
    if let Some(root) = root {
        cmd.push_str(&format!(" --root \"{}\"", root.display()));
    }
    // Скрытое окно: демон не должен висеть консолью поверх плеера.
    let script = format!(
        "' SynctMusic: автозапуск syncthd\r\nSet sh = CreateObject(\"WScript.Shell\")\r\nsh.Run \"{}\", 0, False\r\n",
        cmd.replace('"', "\"\"")
    );
    std::fs::write(&path, script).with_context(|| format!("запись {}", path.display()))?;
    println!("демон добавлен в автозапуск: {}", path.display());
    println!("команда: {cmd}");
    Ok(())
}

fn uninstall_autostart() -> Result<()> {
    let path = autostart_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => println!("демон убран из автозапуска"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("в автозапуске ничего не было")
        }
        Err(e) => return Err(e).with_context(|| format!("удаление {}", path.display())),
    }
    Ok(())
}

// ------------------------------------------------------------------ misc

fn absolute(p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(p))
    }
}

fn default_device_id(data_dir: &Path) -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "device".into());
    let seed = format!("{host}|{}", data_dir.display());
    format!("dev-{}", hash8(&seed))
}

fn hash8(s: &str) -> String {
    let mut acc: u64 = 1469598103934665603;
    for b in s.as_bytes() {
        acc ^= *b as u64;
        acc = acc.wrapping_mul(1099511628211);
    }
    format!("{acc:016x}")[..8].to_string()
}

const UI: &str = include_str!("ui.html");
