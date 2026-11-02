//! `syncthd` — служба синхронизации для ПК.
//!
//! По умолчанию работает только по кнопке: без watcher и без автосинка.
//! Задачи, которые она забирает у пользователя:
//! * отдаёт локальную веб-страницу на `127.0.0.1` — состояние, план изменений,
//!   устройства;
//! * держит соединение с выбранным устройством и обслуживает входящие сеансы;
//! * умеет прописаться в автозапуск Windows.
//!
//! Вся мутация реплики идёт через один рабочий поток: у `Replica` один
//! SQLite-коннектор, а сеть и веб-UI лишь шлют ему команды.
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
use syncth_core::plan::{Decision, PathDecision};
use syncth_core::{net, resolve, scan, vault, Action, Hlc, Replica};

mod tray;

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

    /// Автоматически искать пиров и синхронизироваться каждые N секунд.
    /// Работает только вместе со скрытым `--auto-sync`.
    #[arg(long, default_value_t = 30)]
    sync_interval: u64,

    /// Включить автосинк (скрытый флаг для отладки). По умолчанию выключен.
    #[arg(long, hide = true)]
    auto_sync: bool,

    /// Сколько дней хранить удалённое в корзине.
    #[arg(long, default_value_t = vault::DEFAULT_TTL_DAYS)]
    vault_days: u64,

    /// Синхронизироваться с этим адресом, даже если discovery молчит.
    /// Можно указать несколько раз.
    #[arg(long = "peer")]
    peers: Vec<String>,

    /// Следить за папкой и сканировать после изменений. По умолчанию
    /// выключено: скан запускает только пользователь.
    #[arg(long, hide = true)]
    watch: bool,

    /// Порог свободного места, %. Ниже — синхронизация приостанавливается.
    #[arg(long, default_value_t = 5.0)]
    min_free_percent: f64,

    /// Не показывать иконку в трее.
    #[arg(long)]
    no_tray: bool,

    /// Не показывать уведомления Windows об удалениях.
    #[arg(long)]
    no_notify: bool,

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
    name: String,
    addr: String,
    last_seen: Option<u64>,
    error: Option<String>,
}

/// Логическая сессия с выбранным устройством.
///
/// С этапа 3 сокет живёт всё время сессии: стороны обмениваются манифестом,
/// планом и push-решениями. Состояние «подключено» разрешает синхронизацию.
#[derive(Debug, Clone, Default, Serialize)]
struct ActivePeer {
    dev: String,
    name: String,
    addr: String,
    since: u64,
}

/// Сводка живого плана для статуса. Сам план читается из БД через `/api/plan`.
#[derive(Debug, Clone, Default, Serialize)]
struct SessionStatus {
    peer_dev: String,
    peer_name: String,
    addr: String,
    since: u64,
    paths: usize,
    unresolved: usize,
    decisions_hash: String,
    diverged: bool,
    read_only: bool,
    /// `outgoing` — мы подключились, `incoming` — подключились к нам.
    source: String,
}

fn session_snapshot(
    session: &net::PreviewSession,
    addr: String,
    since: u64,
    source: &str,
) -> SessionStatus {
    SessionStatus {
        peer_dev: session.peer_dev.clone(),
        peer_name: session.peer_name.clone(),
        addr,
        since,
        paths: session.paths(),
        unresolved: session.unresolved(),
        decisions_hash: session.decisions_hash(),
        diverged: session.diverged,
        read_only: session.read_only,
        source: source.to_string(),
    }
}

#[derive(Debug, Default)]
struct Runtime {
    paused: bool,
    syncing: bool,
    last_scan: Option<u64>,
    last_sync: Option<u64>,
    last_error: Option<String>,
    disk_low: bool,
    disk_free_percent: Option<f64>,
    disk_free: u64,
    disk_total: u64,
    disk_threshold: f64,
    peers: BTreeMap<String, PeerStatus>,
    active_peer: Option<ActivePeer>,
    session: Option<SessionStatus>,
    /// Запрос commit для входящей сессии: её поток заберёт его на ближайшем шаге.
    pending_commit: Option<Sender<Result<String, String>>>,
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
    Commit(Reply),
    Connect {
        dev: String,
        reply: Reply,
    },
    Disconnect(Reply),
    ForgetPeer {
        dev: String,
        reply: Reply,
    },
    SetDecision {
        path: String,
        action: Decision,
        reply: Reply,
    },
    RefreshPlan(Reply),
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
    CheckDisk(Reply),
    Pair {
        code: String,
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

    if let Some(t) = cli
        .token
        .clone()
        .or_else(|| std::env::var("SYNCTH_TOKEN").ok())
    {
        replica.set_token(&t)?;
    }
    let token = replica.ensure_token()?;

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

    let _watcher = if cli.watch {
        Some(watch_root(&root, fs_tx.clone())?)
    } else {
        None
    };

    let runtime: Shared = Arc::new(Mutex::new(Runtime::default()));
    let disk = DiskProbe {
        paths: vec![root.clone(), absolute(&cli.data_dir)?],
        min_percent: cli.min_free_percent.clamp(0.0, 100.0),
    };
    let cfg = WorkerConfig {
        root: root.clone(),
        auto_sync: cli.auto_sync,
        sync_interval: Duration::from_secs(cli.sync_interval.max(5)),
        vault_days: cli.vault_days,
        explicit_peers: cli.peers.clone(),
        notify: !cli.no_notify,
        disk: disk.clone(),
    };

    let rt = runtime.clone();
    std::thread::spawn(move || worker(replica, cfg, cmd_rx, fs_rx, rt));

    // Входящие сеансы обслуживает отдельный поток со своим соединением к БД.
    // Иначе демон, который сам ждёт ответа от пира, не смог бы принять
    // встречное подключение — два устройства зависли бы друг на друге.
    {
        let data_dir = cli.data_dir.clone();
        let dev = dev.clone();
        let runtime = runtime.clone();
        let disk = disk.clone();
        std::thread::spawn(move || {
            while let Ok(stream) = stream_rx.recv() {
                if refresh_disk(&runtime, &disk) {
                    let _ = net::reject_stream(
                        stream,
                        "на этом устройстве мало места — синхронизация приостановлена",
                    );
                    continue;
                }
                // Соединение обслуживается своим потоком и своим SQLite-
                // соединением: живая preview-сессия может висеть долго и не
                // должна блокировать встречные подключения.
                let data_dir = data_dir.clone();
                let dev = dev.clone();
                let runtime = runtime.clone();
                std::thread::spawn(move || {
                    let mut replica = match Replica::open(&data_dir, &dev) {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::error!("входящий сеанс: {e:#}");
                            return;
                        }
                    };
                    let token = match replica.ensure_token() {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::warn!("токен: {e:#}");
                            return;
                        }
                    };
                    let peer_addr = stream
                        .peer_addr()
                        .map(|a| a.to_string())
                        .unwrap_or_default();
                    let since = Hlc::wall_ms();
                    let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
                    let seen_in_cb = seen.clone();
                    let runtime_in_cb = runtime.clone();
                    let result = net::serve_stream_with(
                        stream,
                        &mut replica,
                        &token,
                        &mut |session, replica| {
                            *seen_in_cb.lock().unwrap() = Some(session.peer_dev.clone());
                            let snapshot =
                                session_snapshot(session, peer_addr.clone(), since, "incoming");
                            let mut rt = runtime_in_cb.lock().unwrap();
                            rt.peers.insert(
                                snapshot.peer_dev.clone(),
                                PeerStatus {
                                    name: snapshot.peer_name.clone(),
                                    addr: snapshot.addr.clone(),
                                    last_seen: Some(Hlc::wall_ms()),
                                    error: None,
                                },
                            );
                            rt.active_peer = Some(ActivePeer {
                                dev: snapshot.peer_dev.clone(),
                                name: snapshot.peer_name.clone(),
                                addr: snapshot.addr.clone(),
                                since,
                            });
                            rt.session = Some(snapshot);
                            // На этой стороне сессия входящая: commit запускает
                            // её же поток по запросу из UI.
                            let request = rt.pending_commit.take();
                            drop(rt);

                            if let Some(reply) = request {
                                match session.commit(replica) {
                                    Ok(o) => {
                                        let mut rt = runtime_in_cb.lock().unwrap();
                                        rt.last_sync = Some(Hlc::wall_ms());
                                        if !rt.disk_low {
                                            rt.last_error = None;
                                        }
                                        if let Some(info) = rt.session.as_mut() {
                                            info.read_only = true;
                                        }
                                        drop(rt);
                                        let _ = reply.send(Ok(format!(
                                            "Синхронизация завершена: принято {}, отклонено {}",
                                            o.accepted, o.rejected
                                        )));
                                    }
                                    Err(e) => {
                                        runtime_in_cb
                                            .lock()
                                            .unwrap()
                                            .error(format!("commit: {e:#}"));
                                        let _ = reply.send(Err(format!("{e:#}")));
                                    }
                                }
                            }
                        },
                    );
                    let peer = seen.lock().unwrap().clone();
                    match result {
                        Ok(stats) => {
                            let mut rt = runtime.lock().unwrap();
                            rt.last_sync = Some(Hlc::wall_ms());
                            if !rt.disk_low {
                                rt.last_error = None;
                            }
                            tracing::info!("входящий сеанс: {}", stats.summary());
                        }
                        Err(e) => {
                            let message = format!("{e:#}");
                            if !is_unreachable(&e) && !message.contains("закрыл сеанс") {
                                runtime
                                    .lock()
                                    .unwrap()
                                    .error(format!("входящий сеанс: {message}"));
                            }
                        }
                    }
                    // Сессия закончилась: не оставляем «подключено» висеть.
                    let mut rt = runtime.lock().unwrap();
                    if let Some(reply) = rt.pending_commit.take() {
                        let _ = reply.send(Err("сессия завершилась до commit".into()));
                    }
                    if let Some(peer) = peer {
                        let same = rt.active_peer.as_ref().is_some_and(|p| p.dev == peer);
                        let incoming = rt.session.as_ref().is_some_and(|s| s.source == "incoming");
                        if same && incoming {
                            rt.active_peer = None;
                            rt.session = None;
                        }
                    }
                });
            }
        });
    }

    // UI-поток читает БД отдельным соединением: WAL допускает читателей
    // параллельно с записью рабочего потока.
    let read = Arc::new(Mutex::new(Replica::open(&cli.data_dir, &dev)?));
    let name = read
        .lock()
        .map(|r| r.device_name().to_string())
        .unwrap_or_default();
    let ui_ctx = UiCtx {
        commands: cmd_tx.clone(),
        read,
        runtime: runtime.clone(),
        root,
        dev: dev.clone(),
        name,
        sync_addr: net::advertised_addr(port),
    };
    if !cli.no_tray {
        tray::spawn(cmd_tx, runtime.clone(), format!("http://{}", cli.ui));
    }
    println!("syncthd запущен");
    println!("  устройство: {dev}");
    println!("  корень:     {}", ui_ctx.root.display());
    println!("  синк:       {}", cli.bind);
    println!("  токен:      {}…", &token[..8.min(token.len())]);
    println!("  UI:         http://{}", cli.ui);
    println!("  порог места: {:.0}%", disk.min_percent);
    println!("  данные:     {}", absolute(&cli.data_dir)?.display());
    serve_ui(&cli.ui, ui_ctx)
}

// ------------------------------------------------------------------ worker

struct WorkerConfig {
    root: PathBuf,
    auto_sync: bool,
    sync_interval: Duration,
    vault_days: u64,
    explicit_peers: Vec<String>,
    notify: bool,
    disk: DiskProbe,
}

fn worker(
    mut replica: Replica,
    cfg: WorkerConfig,
    commands: Receiver<Cmd>,
    fs_events: Receiver<()>,
    runtime: Shared,
) {
    let root = cfg.root.clone();
    let mut session: Option<net::PreviewSession> = None;
    let mut pending_scan = false;
    let mut scan_at = Instant::now();
    let mut next_sync = Instant::now() + cfg.sync_interval;
    let mut last_disk_check = Instant::now() - Duration::from_secs(60);

    // Первичный скан: догоняем изменения, сделанные при выключенном демоне.
    refresh_disk(&runtime, &cfg.disk);
    if let Err(e) = do_scan(&mut replica, &root, &runtime, cfg.notify) {
        runtime.lock().unwrap().error(format!("скан: {e:#}"));
    }
    if let Err(e) = vault::gc(&replica, cfg.vault_days) {
        tracing::warn!("gc корзины: {e:#}");
    }
    let mut last_gc = Instant::now();

    loop {
        match commands.recv_timeout(Duration::from_millis(200)) {
            Ok(Cmd::Scan(reply)) => {
                let res = do_scan(&mut replica, &root, &runtime, cfg.notify)
                    .map(|_| "Сканирование завершено".to_string());
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Sync(reply)) => {
                let res = do_sync(&mut replica, &cfg, &runtime, true);
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Commit(reply)) => {
                if session.is_none() {
                    let mut rt = runtime.lock().unwrap();
                    let incoming = rt.session.as_ref().is_some_and(|s| s.source == "incoming");
                    if incoming {
                        // Входящую сессию обслуживает другой поток; передаём
                        // ему запрос и не отвечаем здесь.
                        rt.pending_commit = Some(reply);
                        continue;
                    }
                    drop(rt);
                    respond(
                        reply,
                        Err(anyhow::anyhow!(
                            "Нет соединения: сначала подключитесь к устройству"
                        )),
                        &runtime,
                    );
                    continue;
                }
                let (res, used) = match session.as_mut() {
                    Some(active) => {
                        let outcome = active.commit(&mut replica);
                        match &outcome {
                            Ok(o) => (
                                Ok(format!(
                                    "Синхронизация завершена: принято {}, отклонено {}",
                                    o.accepted, o.rejected
                                )),
                                true,
                            ),
                            Err(e) => (Err(anyhow::anyhow!("{e:#}")), active.read_only),
                        }
                    }
                    None => (Ok(String::new()), false),
                };
                if used {
                    if let Some(mut old) = session.take() {
                        old.close();
                    }
                    let mut rt = runtime.lock().unwrap();
                    rt.session = None;
                    rt.active_peer = None;
                    if res.is_ok() {
                        rt.last_sync = Some(Hlc::wall_ms());
                        if !rt.disk_low {
                            rt.last_error = None;
                        }
                    }
                }
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Connect { dev, reply }) => {
                let res = do_connect(&mut replica, &runtime, &dev, &mut session);
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Disconnect(reply)) => {
                if let Some(mut old) = session.take() {
                    old.close();
                }
                let mut rt = runtime.lock().unwrap();
                rt.active_peer = None;
                rt.session = None;
                drop(rt);
                let _ = reply.send(Ok("Соединение закрыто".into()));
            }
            Ok(Cmd::SetDecision {
                path,
                action,
                reply,
            }) => {
                let res = match session.as_mut() {
                    Some(s) => s.record_decision(&mut replica, &path, action),
                    // Входящая сессия живёт в своём потоке: решение пишем в БД,
                    // оттуда его заберёт и отправит живой канал.
                    None => (|| -> Result<()> {
                        let dev = runtime
                            .lock()
                            .unwrap()
                            .active_peer
                            .as_ref()
                            .map(|p| p.dev.clone())
                            .context("Нет соединения: сначала подключитесь к устройству")?;
                        save_decision(&mut replica, &dev, &path, action)
                    })(),
                };
                respond(
                    reply,
                    res.map(|_| format!("Решение по «{path}» сохранено")),
                    &runtime,
                );
            }
            Ok(Cmd::RefreshPlan(reply)) => {
                let dev = runtime
                    .lock()
                    .unwrap()
                    .active_peer
                    .as_ref()
                    .map(|p| p.dev.clone());
                let res = match dev {
                    Some(dev) => do_connect(&mut replica, &runtime, &dev, &mut session),
                    None => Err(anyhow::anyhow!(
                        "Нет соединения: сначала подключитесь к устройству"
                    )),
                };
                respond(reply, res.map(|_| "План обновлён".to_string()), &runtime);
            }
            Ok(Cmd::ForgetPeer { dev, reply }) => {
                let was_active = runtime
                    .lock()
                    .unwrap()
                    .active_peer
                    .as_ref()
                    .is_some_and(|p| p.dev == dev);
                if was_active {
                    if let Some(mut old) = session.take() {
                        old.close();
                    }
                }
                let res = (|| -> Result<String> {
                    let removed = replica.remove_peer(&dev)?;
                    let mut rt = runtime.lock().unwrap();
                    if was_active {
                        rt.active_peer = None;
                        rt.session = None;
                    }
                    rt.peers.remove(&dev);
                    if removed {
                        Ok(format!("Устройство {dev} забыто"))
                    } else {
                        Ok(format!("Устройство {dev} и так не сохранено"))
                    }
                })();
                respond(reply, res, &runtime);
            }
            Ok(Cmd::CheckDisk(reply)) => {
                let low = refresh_disk(&runtime, &cfg.disk);
                let free = runtime
                    .lock()
                    .unwrap()
                    .disk_free_percent
                    .map(|p| format!("{p:.1}%"))
                    .unwrap_or_else(|| "неизвестно".into());
                let msg: Result<String> = if low {
                    Err(anyhow::anyhow!(
                        "Мало места: свободно {free} — синхронизация приостановлена"
                    ))
                } else {
                    Ok(format!("Места достаточно: свободно {free}"))
                };
                respond(reply, msg, &runtime);
            }
            Ok(Cmd::Pair { code, reply }) => {
                let res = (|| -> Result<String> {
                    let info = net::parse_pair_code(&code)?;
                    replica.set_token(&info.token)?;
                    replica.add_peer(&info.dev, &info.addr, &info.name)?;
                    // Сообщаем второй стороне, чтобы она увидела сопряжение,
                    // даже если код вводили только здесь. Сеть не обязательна.
                    let _ = net::announce(&mut replica, &info.addr, &info.token);
                    let shown = if info.name.is_empty() {
                        info.dev.clone()
                    } else {
                        info.name.clone()
                    };
                    Ok(format!("Сопряжено с {shown} ({})", info.addr))
                })();
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Resolve {
                path,
                action,
                reply,
            }) => {
                let res = resolve::resolve(&mut replica, &path, action).map(|out| match action {
                    Action::DeleteEverywhere => format!(
                        "Удалено путей: {}; копия в корзине",
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
                    .map(|_| format!("Ожидание для «{path}» снято"));
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Gc(reply)) => {
                let res = vault::gc(&replica, cfg.vault_days).map(|r| {
                    format!(
                        "Корзина: удалено записей {}, файлов {}, сирот {}",
                        r.rows_removed, r.blobs_removed, r.orphans_removed
                    )
                });
                respond(reply, res, &runtime);
            }
            Ok(Cmd::Pause { paused, reply }) => {
                runtime.lock().unwrap().paused = paused;
                let _ = reply.send(Ok(if paused {
                    "Автосинк на паузе".into()
                } else {
                    "Автосинк включён".into()
                }));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        // Живая сессия: отправляем новые решения и принимаем входящие.
        if let Some(active) = session.as_mut() {
            match active.poll(&mut replica) {
                Ok(()) => {
                    let mut rt = runtime.lock().unwrap();
                    if let Some(info) = rt.session.as_mut() {
                        info.paths = active.paths();
                        info.unresolved = active.unresolved();
                        info.decisions_hash = active.decisions_hash();
                        info.diverged = active.diverged;
                        info.read_only = active.read_only;
                    }
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    active.close();
                    session = None;
                    let mut rt = runtime.lock().unwrap();
                    rt.session = None;
                    rt.active_peer = None;
                    if is_unreachable(&e) || message.contains("закрыл сеанс") {
                        tracing::info!("сеанс завершён: {message}");
                        rt.last_error = None;
                    } else {
                        rt.error(format!("Сеанс прерван: {message}"));
                    }
                }
            }
        }

        // Входящие сеансы обслуживает отдельный поток (см. `run`), чтобы
        // встречные подключения не зависели от нашего исходящего синка.
        while fs_events.try_recv().is_ok() {
            pending_scan = true;
            scan_at = Instant::now() + Duration::from_secs(2);
        }

        if pending_scan && Instant::now() >= scan_at {
            pending_scan = false;
            if let Err(e) = do_scan(&mut replica, &root, &runtime, cfg.notify) {
                runtime.lock().unwrap().error(format!("скан: {e:#}"));
            }
        }

        if last_disk_check.elapsed() >= Duration::from_secs(5) {
            last_disk_check = Instant::now();
            refresh_disk(&runtime, &cfg.disk);
        }

        let now = Instant::now();
        if cfg.auto_sync && now >= next_sync {
            next_sync = now + cfg.sync_interval;
            let low = refresh_disk(&runtime, &cfg.disk);
            let paused = runtime.lock().unwrap().paused;
            if !paused && !low {
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

fn do_scan(replica: &mut Replica, root: &Path, runtime: &Shared, notify: bool) -> Result<()> {
    let report = scan::scan(root, replica)?;
    let saved = resolve::prune_deleted_files(replica, root)?;
    let mut rt = runtime.lock().unwrap();
    rt.last_scan = Some(Hlc::wall_ms());
    if !rt.disk_low {
        rt.last_error = None;
    }
    drop(rt);
    tracing::info!(
        "скан: +{} новых, ~{} изменённых, {} перенесено, {} без изменений, пропало {}",
        report.added,
        report.changed,
        report.renamed,
        report.unchanged,
        report.missing.len()
    );
    if !report.missing.is_empty() {
        tracing::warn!(
            "пропало с диска: {} — решение в веб-UI",
            report.missing.len()
        );
        if notify {
            notify_deletions(&report.missing);
        }
    }
    if !saved.is_empty() {
        tracing::warn!("сохранено конфликтных копий: {}", saved.len());
    }
    Ok(())
}

/// Уведомление Windows: удаление не должно проходить незамеченным.
fn notify_deletions(missing: &[String]) {
    let count = missing.len();
    let first = missing.first().cloned().unwrap_or_default();
    std::thread::spawn(move || {
        let mut body = first;
        if count > 1 {
            body.push_str(&format!("\nи ещё {} файл(ов)", count - 1));
        }
        body.push_str("\nОткройте SynctMusic: удалить везде или оставить только здесь.");
        let result = notify_rust::Notification::new()
            .appname("SynctMusic")
            .summary(&format!("Пропало файлов: {count}"))
            .body(&body)
            .timeout(notify_rust::Timeout::Milliseconds(10_000))
            .show();
        if let Err(e) = result {
            tracing::debug!("уведомление не показано: {e:#}");
        }
    });
}

/// Отказ соединения на уровне сети: пир выключен, спит или ещё не слушает.
/// Такие сбои не должны переводить весь интерфейс в состояние «ошибка».
fn is_unreachable(error: &anyhow::Error) -> bool {
    use std::io::ErrorKind;
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .map(|io| {
                matches!(
                    io.kind(),
                    ErrorKind::ConnectionRefused
                        | ErrorKind::TimedOut
                        | ErrorKind::NotFound
                        | ErrorKind::AddrNotAvailable
                        | ErrorKind::NetworkUnreachable
                        | ErrorKind::HostUnreachable
                        | ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionAborted
                )
            })
            .unwrap_or(false)
    })
}

/// «Подключиться» к сохранённому устройству.
///
/// Открывает живую preview-сессию: рукопожатие, обмен манифестами и планом.
/// Сокет остаётся открытым до «Отключиться»; по нему идут решения и heartbeat.
fn do_connect(
    replica: &mut Replica,
    runtime: &Shared,
    dev: &str,
    session: &mut Option<net::PreviewSession>,
) -> Result<String> {
    if let Some(mut old) = session.take() {
        old.close();
    }
    let peer = replica
        .known_peers()?
        .into_iter()
        .find(|p| p.dev == dev)
        .with_context(|| format!("устройство не сохранено: {dev}"))?;
    let token = replica.ensure_token()?;
    let mut addr = peer.addr.clone();

    let opened = match net::PreviewSession::open(replica, &addr, &token) {
        Ok(session) => session,
        Err(first) => {
            // Адрес мог смениться по DHCP: пробуем найти пира в локальной сети.
            let found = net::discover(Duration::from_secs(2))
                .unwrap_or_default()
                .into_iter()
                .find(|(found_dev, _)| *found_dev == dev);
            match found {
                Some((_, new_addr)) => {
                    addr = new_addr;
                    net::PreviewSession::open(replica, &addr, &token)
                        .with_context(|| format!("первая попытка: {first:#}"))?
                }
                None => return Err(first),
            }
        }
    };

    let name = if opened.peer_name.is_empty() {
        peer.name
    } else {
        opened.peer_name.clone()
    };
    let label = if name.is_empty() {
        opened.peer_dev.clone()
    } else {
        name.clone()
    };
    let now = Hlc::wall_ms();
    let status = session_snapshot(&opened, addr.clone(), now, "outgoing");
    let mut rt = runtime.lock().unwrap();
    rt.peers.insert(
        opened.peer_dev.clone(),
        PeerStatus {
            name: name.clone(),
            addr: addr.clone(),
            last_seen: Some(now),
            error: None,
        },
    );
    rt.active_peer = Some(ActivePeer {
        dev: opened.peer_dev.clone(),
        name,
        addr: addr.clone(),
        since: now,
    });
    rt.session = Some(status);
    rt.last_error = None;
    *session = Some(opened);
    Ok(format!("Подключено: {label} ({addr})"))
}

/// Записать решение без живого канала в этом потоке. Так работают входящие
/// сессии и будущий офлайн-план: канал заберёт решение из БД.
fn save_decision(
    replica: &mut Replica,
    peer_dev: &str,
    path: &str,
    action: Decision,
) -> Result<()> {
    let mut plan = replica
        .load_plan(peer_dev, Hlc::wall_ms())?
        .context("план не найден или истёк")?;
    let blob = plan
        .blob_for(path)
        .with_context(|| format!("путь отсутствует в плане: {path}"))?;
    let hlc = replica.next_hlc()?;
    let decision = PathDecision {
        path: path.to_string(),
        action,
        blob,
        hlc,
        dev: replica.device_id().to_string(),
    };
    let mut set = replica.load_decisions(peer_dev)?;
    set.entries.insert(path.to_string(), decision);
    set.retain_valid(&plan);
    replica.save_decisions(peer_dev, &set)?;
    plan.decisions = set;
    replica.save_plan(&plan)?;
    Ok(())
}

fn do_sync(
    replica: &mut Replica,
    cfg: &WorkerConfig,
    runtime: &Shared,
    manual: bool,
) -> Result<String> {
    if refresh_disk(runtime, &cfg.disk) {
        bail!("мало места на диске — синхронизация приостановлена до освобождения места");
    }
    let token = replica.ensure_token()?;
    let me = replica.device_id().to_string();
    let active = runtime.lock().unwrap().active_peer.clone();

    // Ручной режим: обмен только с тем устройством, к которому пользователь
    // явно подключился. Скрытый `--auto-sync` сохраняет прежний поиск всех.
    if active.is_none() && !cfg.auto_sync {
        bail!("Нет соединения: сначала подключитесь к устройству");
    }

    // Тройка: device id, адрес и понятное имя (если известно). В сообщениях
    // показываем имя, иначе пользователь видит только dev-xxxxxxxx.
    let mut targets: Vec<(String, String, String)> = Vec::new();
    if let Some(peer) = &active {
        targets.push((peer.dev.clone(), peer.addr.clone(), peer.name.clone()));
    } else {
        targets.extend(
            cfg.explicit_peers
                .iter()
                .map(|a| (a.clone(), a.clone(), String::new())),
        );
        for peer in replica.known_peers()? {
            if peer.dev != me && !targets.iter().any(|(_, a, _)| *a == peer.addr) {
                targets.push((peer.dev, peer.addr, peer.name));
            }
        }

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
                    if !targets.iter().any(|(_, a, _)| *a == addr) {
                        targets.push((dev, addr, String::new()));
                    }
                }
            }
            Err(e) => tracing::debug!("discovery: {e:#}"),
        }
    }

    if targets.is_empty() {
        if manual {
            bail!("Нет соединения: сначала подключитесь к устройству");
        }
        return Ok(String::new());
    }

    runtime.lock().unwrap().syncing = true;
    let mut done = Vec::new();
    let mut errors = Vec::new();
    let mut no_answer = Vec::new();
    for (dev, addr, name) in targets {
        let label = if name.is_empty() {
            dev.clone()
        } else {
            name.clone()
        };
        match net::sync_with_peer(replica, &addr, &token) {
            Ok(stats) => {
                done.push(format!("{label} ({addr}): {}", stats.summary()));
                let mut rt = runtime.lock().unwrap();
                rt.peers.insert(
                    dev,
                    PeerStatus {
                        name,
                        addr,
                        last_seen: Some(Hlc::wall_ms()),
                        error: None,
                    },
                );
                rt.last_error = None;
            }
            Err(e) => {
                let message = format!("{e:#}");
                if is_unreachable(&e) {
                    // Устройство просто не в сети или ещё не слушает порт —
                    // это не ошибка синхронизации, а «нет ответа».
                    tracing::debug!("{label} ({addr}): {message}");
                    no_answer.push(format!("{label} ({addr})"));
                    let mut rt = runtime.lock().unwrap();
                    rt.peers.insert(
                        dev,
                        PeerStatus {
                            name,
                            addr,
                            last_seen: None,
                            error: None,
                        },
                    );
                    continue;
                }
                errors.push(format!("{label} ({addr}): {message}"));
                let mut rt = runtime.lock().unwrap();
                rt.peers.insert(
                    dev,
                    PeerStatus {
                        name,
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

    if done.is_empty() && errors.is_empty() {
        if manual && !no_answer.is_empty() {
            // Пользователь нажал «синхронизировать» и ждёт ответа.
            return Ok(format!("Нет ответа: {}", no_answer.join(", ")));
        }
        return Ok(String::new());
    }
    if done.is_empty() {
        let all = errors.join("; ");
        runtime.lock().unwrap().last_error = Some(all.clone());
        bail!("Синхронизация не удалась: {all}");
    }
    let mut msg = done.join("; ");
    if !errors.is_empty() {
        msg.push_str(&format!(" | Ошибки: {}", errors.join("; ")));
        runtime.lock().unwrap().last_error = Some(errors.join("; "));
    }
    Ok(msg)
}

// ------------------------------------------------------------------ disk guard

#[derive(Debug, Clone)]
struct DiskProbe {
    paths: Vec<PathBuf>,
    min_percent: f64,
}

impl DiskProbe {
    /// Возвращает (свободно, всего, процент) по самому тесному тому.
    fn snapshot(&self) -> Option<(u64, u64, f64)> {
        let mut worst: Option<(u64, u64, f64)> = None;
        for path in &self.paths {
            let (Ok(total), Ok(free)) = (fs4::total_space(path), fs4::available_space(path)) else {
                continue;
            };
            if total == 0 {
                continue;
            }
            let percent = free as f64 / total as f64 * 100.0;
            if worst.map(|(_, _, p)| percent < p).unwrap_or(true) {
                worst = Some((free, total, percent));
            }
        }
        worst
    }
}

/// Обновляет состояние и возвращает `true`, если места мало.
///
/// Предупреждение ставится только при переходе через порог: иначе каждые
/// 5 секунд в лог и UI уходила бы одна и та же строка.
fn refresh_disk(runtime: &Shared, probe: &DiskProbe) -> bool {
    let Some((free, total, percent)) = probe.snapshot() else {
        return runtime.lock().unwrap().disk_low;
    };
    let mut rt = runtime.lock().unwrap();
    rt.disk_threshold = probe.min_percent;
    rt.disk_free = free;
    rt.disk_total = total;
    rt.disk_free_percent = Some(percent);

    let low = percent < probe.min_percent;
    if low && !rt.disk_low {
        let msg = format!(
            "мало места на диске: свободно {percent:.1}% (порог {:.0}%) — синхронизация приостановлена",
            probe.min_percent
        );
        tracing::warn!("{msg}");
        rt.last_error = Some(msg);
    } else if !low && rt.disk_low {
        tracing::info!("места достаточно ({percent:.1}%) — синхронизация возобновлена");
        if rt
            .last_error
            .as_deref()
            .map(|s| s.starts_with("мало места"))
            .unwrap_or(false)
        {
            rt.last_error = None;
        }
    }
    rt.disk_low = low;
    low
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
    /// Понятное имя устройства (COMPUTERNAME и т.п.).
    name: String,
    /// Адрес, который попадёт в строку сопряжения.
    sync_addr: String,
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
        (tiny_http::Method::Get, "/api/pair-code") => (
            200,
            "application/json; charset=utf-8",
            api_pair_code(ctx).to_string(),
        ),
        (tiny_http::Method::Get, "/api/plan") => (
            200,
            "application/json; charset=utf-8",
            api_plan(ctx).to_string(),
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
    let body = if matches!(path, "/api/pause" | "/api/decision") {
        body_json(request)
    } else {
        Value::Null
    };

    match path {
        "/api/scan" => run_cmd(&ctx.commands, Cmd::Scan),
        "/api/sync" => run_cmd(&ctx.commands, Cmd::Sync),
        "/api/commit" => run_cmd(&ctx.commands, Cmd::Commit),
        "/api/plan/refresh" => run_cmd(&ctx.commands, Cmd::RefreshPlan),
        "/api/decision" => {
            let path = str_field(&body, "path")?;
            let action = match body.get("action").and_then(Value::as_str) {
                Some(raw) => {
                    Decision::parse(raw).ok_or_else(|| format!("неизвестное решение: {raw}"))?
                }
                None => return Err("в запросе нет поля «action»".into()),
            };
            run_cmd(&ctx.commands, move |reply| Cmd::SetDecision {
                path,
                action,
                reply,
            })
        }
        "/api/connect" => {
            let body = body_json(request);
            let dev = str_field(&body, "dev")?;
            run_cmd(&ctx.commands, move |reply| Cmd::Connect { dev, reply })
        }
        "/api/disconnect" => run_cmd(&ctx.commands, Cmd::Disconnect),
        "/api/peers/forget" => {
            let body = body_json(request);
            let dev = str_field(&body, "dev")?;
            run_cmd(&ctx.commands, move |reply| Cmd::ForgetPeer { dev, reply })
        }
        "/api/gc" => run_cmd(&ctx.commands, Cmd::Gc),
        "/api/disk/check" => run_cmd(&ctx.commands, Cmd::CheckDisk),
        "/api/pair" => {
            let body = body_json(request);
            let code = str_field(&body, "code")?;
            run_cmd(&ctx.commands, move |reply| Cmd::Pair { code, reply })
        }
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
        // Аккуратная остановка из десктопного клиента: ответ успевает уйти,
        // после чего процесс завершается. WAL это переживает.
        "/api/quit" => {
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                std::process::exit(0);
            });
            Ok("Служба остановлена".to_string())
        }
        other => Err(format!("Неизвестный запрос: {other}")),
    }
}

fn run_cmd(tx: &Sender<Cmd>, make: impl FnOnce(Reply) -> Cmd) -> Result<String, String> {
    let (reply, rx) = mpsc::channel();
    tx.send(make(reply))
        .map_err(|_| "Служба остановлена".to_string())?;
    rx.recv_timeout(Duration::from_secs(180))
        .map_err(|_| "Операция не завершилась за 3 минуты".to_string())?
}

fn api_plan(ctx: &UiCtx) -> Value {
    let dev = ctx
        .runtime
        .lock()
        .unwrap()
        .active_peer
        .as_ref()
        .map(|p| p.dev.clone());
    let Some(dev) = dev else {
        return json!({ "connected": false, "plan": Value::Null });
    };
    let r = ctx.read.lock().unwrap();
    match r.load_plan(&dev, Hlc::wall_ms()) {
        Ok(Some(plan)) => json!({
            "connected": true,
            "plan": plan,
            "hash": plan.decisions_hash(),
            "unresolved": plan.unresolved().len(),
            "paths": plan.path_count(),
        }),
        Ok(None) => json!({ "connected": true, "plan": Value::Null }),
        Err(e) => json!({ "connected": true, "plan": Value::Null, "error": format!("{e:#}") }),
    }
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
    let known_peers: Vec<Value> = r
        .known_peers()
        .unwrap_or_default()
        .into_iter()
        .map(|p| json!({ "dev": p.dev, "addr": p.addr, "name": p.name }))
        .collect();
    let peers: Vec<Value> = rt
        .peers
        .iter()
        .map(|(dev, p)| {
            json!({
                "dev": dev,
                "name": p.name,
                "addr": p.addr,
                "last_seen": p.last_seen,
                "error": p.error,
            })
        })
        .collect();
    let connection = match &rt.active_peer {
        Some(peer) => json!({
            "connected": true,
            "dev": peer.dev,
            "name": peer.name,
            "addr": peer.addr,
            "since": peer.since,
        }),
        None => json!({ "connected": false }),
    };

    let state = if rt.disk_low {
        "paused_with_warning"
    } else if rt.paused {
        "paused"
    } else if rt.syncing {
        "syncing"
    } else if rt.last_error.is_some() {
        "error"
    } else {
        "running"
    };

    json!({
        "device": ctx.dev,
        "name": ctx.name,
        "root": ctx.root.to_string_lossy(),
        "state": state,
        "paused": rt.paused,
        "syncing": rt.syncing,
        "last_scan": rt.last_scan,
        "last_sync": rt.last_sync,
        "last_error": rt.last_error,
        "disk": {
            "low": rt.disk_low,
            "free_percent": rt.disk_free_percent,
            "free": rt.disk_free,
            "total": rt.disk_total,
            "threshold": rt.disk_threshold,
        },
        "ops": r.op_count().unwrap_or(0),
        "files": r.live_entries().map(|v| v.len()).unwrap_or(0),
        "local": r.local_entries().map(|v| v.len()).unwrap_or(0),
        "size": r.total_live_bytes().unwrap_or(0),
        "pending": pending,
        "ignored": ignored,
        "conflicts": conflicts,
        "vault": vault,
        "peers": peers,
        "connection": connection,
        "session": rt.session,
        "known_peers": known_peers,
        "pair_addr": ctx.sync_addr,
    })
}

fn api_pair_code(ctx: &UiCtx) -> Value {
    let token = ctx.read.lock().unwrap().ensure_token().unwrap_or_default();
    let code = net::pair_code(&ctx.dev, &ctx.name, &token, &ctx.sync_addr);
    let svg = qrcode::QrCode::new(code.as_bytes())
        .map(|qr| {
            qr.render::<qrcode::render::svg::Color>()
                .min_dimensions(190, 190)
                .build()
        })
        .unwrap_or_default();
    json!({
        "code": code,
        "svg": svg,
        "addr": ctx.sync_addr,
        "dev": ctx.dev,
        "name": ctx.name,
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
