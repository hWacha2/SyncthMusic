//! Минимальный сетевой агент Android.
//!
//! Агент не строит планов и не ведёт журнал. Он подтверждает сопряжение,
//! хранит выбранную папку и выполняет команды ПК.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;
use syncth_core::agent::{self, AgentIdentity, AgentMsg, AGENT_PORT, AGENT_PROTO};
use syncth_core::scan;
use syncth_core::transport::Transport;
use tauri::Manager;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pair {
    name: String,
    pk: String,
    token: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Config {
    root: Option<String>,
    #[serde(default)]
    pairs: HashMap<String, Pair>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingPair {
    /// Номер запроса. Нужен, чтобы старый таймаут не сбросил новый запрос.
    pub seq: usize,
    pub dev: String,
    pub name: String,
    pub pk: String,
}

#[derive(Debug, Default)]
struct Status {
    running: bool,
    addr: String,
    error: Option<String>,
}

#[derive(Default)]
pub struct AgentRuntime {
    identity: Mutex<Option<AgentIdentity>>,
    config: Mutex<Config>,
    status: Mutex<Status>,
    pending: Mutex<Option<PendingPair>>,
    pair_tx: Mutex<Option<mpsc::Sender<bool>>>,
    pair_seq: AtomicUsize,
    root_change: AtomicBool,
    root_tx: Mutex<Option<mpsc::Sender<String>>>,
    /// Сколько командных сессий с ПК сейчас открыто. Во время сопряжения
    /// счётчик ещё не растёт: телефон в этот момент показывает свой запрос.
    connected: AtomicUsize,
}

/// Учитывает активную сессию и уменьшает счётчик при любом выходе.
struct SessionGuard<'a>(&'a AtomicUsize);

impl<'a> SessionGuard<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for SessionGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl AgentRuntime {
    fn root(&self) -> Option<String> {
        self.config.lock().unwrap().root.clone()
    }

    fn set_root(&self, root: Option<String>) {
        let mut config = self.config.lock().unwrap();
        config.root = root;
    }

    fn is_paired(&self, dev: &str, token: &str) -> bool {
        self.config
            .lock()
            .unwrap()
            .pairs
            .get(dev)
            .map(|pair| pair.token == token)
            .unwrap_or(false)
    }

    fn save_pair(&self, dev: &str, pair: Pair) {
        self.config
            .lock()
            .unwrap()
            .pairs
            .insert(dev.to_string(), pair);
    }
}

fn config_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(err)?;
    std::fs::create_dir_all(&dir).map_err(err)?;
    Ok(dir.join("agent.json"))
}

fn load_config(app: &tauri::AppHandle) -> Config {
    std::fs::read_to_string(config_path(app).unwrap_or_default())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_config(app: &tauri::AppHandle, config: &Config) -> Result<(), String> {
    let text = serde_json::to_string_pretty(config).map_err(err)?;
    std::fs::write(config_path(app)?, text).map_err(err)
}

fn err(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}

fn new_token() -> Result<String, String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(err)?;
    Ok(buf.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Загружает конфиг, кэширует идентичность и запускает сервер.
pub fn init(app: tauri::AppHandle) {
    let replica = match crate::open(&app) {
        Ok(replica) => replica,
        Err(error) => {
            let runtime = app.state::<AgentRuntime>();
            runtime.status.lock().unwrap().error = Some(error);
            return;
        }
    };
    let identity = AgentIdentity {
        dev: replica.device_id().to_string(),
        name: replica.device_name().to_string(),
        pk: replica.public_key(),
    };
    let config = load_config(&app);
    {
        let runtime = app.state::<AgentRuntime>();
        *runtime.identity.lock().unwrap() = Some(identity.clone());
        *runtime.config.lock().unwrap() = config;
    }
    std::thread::spawn(move || listen(app, identity));
}

fn listen(app: tauri::AppHandle, identity: AgentIdentity) {
    let _ = agent::start_discovery(
        identity.dev.clone(),
        identity.name.clone(),
        AGENT_PORT,
    );

    loop {
        let listener = match TcpListener::bind(("0.0.0.0", AGENT_PORT)) {
            Ok(listener) => listener,
            Err(error) => {
                if let Some(runtime) = app.try_state::<AgentRuntime>() {
                    let mut status = runtime.status.lock().unwrap();
                    status.running = false;
                    status.error = Some(format!("порт {AGENT_PORT}: {error}"));
                }
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };

        if let Some(runtime) = app.try_state::<AgentRuntime>() {
            let mut status = runtime.status.lock().unwrap();
            status.running = true;
            status.addr = format!("0.0.0.0:{AGENT_PORT}");
            status.error = None;
        }

        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            let app = app.clone();
            std::thread::spawn(move || handle(app, stream));
        }
    }
}

fn handle(app: tauri::AppHandle, stream: TcpStream) {
    let mut transport: Box<dyn Transport> = Box::new(stream);
    let _ = transport.set_read_timeout(Some(Duration::from_secs(120)));
    let _ = transport.set_write_timeout(Some(Duration::from_secs(120)));
    // Новое соединение уже означает, что агент жив. Ошибка прошлого сеанса
    // не должна висеть на экране, пока идёт успешная работа.
    if let Some(runtime) = app.try_state::<AgentRuntime>() {
        runtime.status.lock().unwrap().error = None;
    }
    if let Err(error) = session(&app, transport.as_mut()) {
        if let Some(runtime) = app.try_state::<AgentRuntime>() {
            // Обрыв сокета — штатное завершение сеанса, а не ошибка. Если же
            // рядом есть другое живое соединение, его статус тоже не трогаем.
            let disconnect = is_disconnect(&error);
            let alone = runtime.connected.load(Ordering::SeqCst) == 0;
            if !disconnect && alone {
                runtime.status.lock().unwrap().error = Some(err(error));
            }
        }
    }
    transport.shutdown();
}

fn session(app: &tauri::AppHandle, transport: &mut dyn Transport) -> anyhow::Result<()> {
    let hello = agent::recv(transport)?;
    let (dev, name, pk, proto, token) = match hello {
        AgentMsg::Hello {
            dev,
            name,
            pk,
            proto,
            token,
        } => (dev, name, pk, proto, token),
        AgentMsg::Error { message } => anyhow::bail!(message),
        other => anyhow::bail!("ожидалось приветствие, получено {other:?}"),
    };
    anyhow::ensure!(
        proto == AGENT_PROTO,
        "версия протокола не совпадает: {proto} вместо {AGENT_PROTO}"
    );

    let runtime = app
        .try_state::<AgentRuntime>()
        .ok_or_else(|| anyhow::anyhow!("агент не запущен"))?;
    let identity = runtime
        .identity
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| anyhow::anyhow!("идентичность недоступна"))?;

    let paired = token
        .as_deref()
        .map(|value| runtime.is_paired(&dev, value))
        .unwrap_or(false);

    agent::send(
        transport,
        &AgentMsg::HelloAck {
            dev: identity.dev.clone(),
            name: identity.name.clone(),
            pk: identity.pk.clone(),
            proto: AGENT_PROTO,
            paired,
            root: runtime.root(),
        },
    )?;

    if !paired {
        let ask = agent::recv(transport)?;
        match ask {
            AgentMsg::PairAsk {
                dev: ask_dev,
                name: ask_name,
                pk: ask_pk,
            } if ask_dev == dev => {
                pair(app, &runtime, transport, dev, ask_name, ask_pk)?;
                return command_loop(app, &runtime, transport);
            }
            AgentMsg::PairAsk { .. } => anyhow::bail!("идентификатор устройства не совпал"),
            AgentMsg::Error { message } => anyhow::bail!(message),
            other => anyhow::bail!("ожидался запрос сопряжения, получено {other:?}"),
        }
    }

    let _ = name;
    let _ = pk;
    if let Some(value) = token.as_deref() {
        crate::set_sync_token(app, value).map_err(|error| anyhow::anyhow!(error))?;
    }
    command_loop(app, &runtime, transport)
}

fn pair(
    app: &tauri::AppHandle,
    runtime: &AgentRuntime,
    transport: &mut dyn Transport,
    dev: String,
    name: String,
    pk: String,
) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel::<bool>();
    let seq = runtime.pair_seq.fetch_add(1, Ordering::SeqCst) + 1;
    {
        *runtime.pending.lock().unwrap() = Some(PendingPair {
            seq,
            dev: dev.clone(),
            name: name.clone(),
            pk: pk.clone(),
        });
        *runtime.pair_tx.lock().unwrap() = Some(tx);
    }

    let accepted = rx.recv_timeout(agent::PAIR_TIMEOUT).unwrap_or(false);

    // Запрос мог быть заменён новым подключением, пока мы ждали. В этом случае
    // не трогаем чужое состояние и не шлём ответ в уже закрытый сокет.
    let still_mine = {
        let mut pending = runtime.pending.lock().unwrap();
        let mine = pending
            .as_ref()
            .map(|pair| pair.seq == seq)
            .unwrap_or(false);
        if mine {
            *pending = None;
            *runtime.pair_tx.lock().unwrap() = None;
        }
        mine
    };
    if !still_mine {
        anyhow::bail!("сопряжение прервано новым запросом");
    }

    if !accepted {
        agent::send(
            transport,
            &AgentMsg::PairNo {
                reason: "пользователь отклонил подключение".into(),
            },
        )?;
        anyhow::bail!("сопряжение отклонено");
    }

    let token = new_token().map_err(anyhow::Error::msg)?;
    runtime.save_pair(
        &dev,
        Pair {
            name: name.clone(),
            pk,
            token: token.clone(),
        },
    );
    {
        let config = runtime.config.lock().unwrap();
        save_config(app, &config).map_err(anyhow::Error::msg)?;
    }
    // Полная синхронизация на телефоне живёт в том же реплике, поэтому её
    // токен должен совпасть с токеном пары до того, как ПК начнёт сеанс.
    crate::set_sync_token(app, &token).map_err(anyhow::Error::msg)?;
    agent::send(transport, &AgentMsg::PairOk { token })?;
    Ok(())
}

/// Восстанавливает состояние после запуска приложения: сканирует уже
/// выбранную папку, подставляет токен пары в общий реплик и поднимает
/// приёмник полной синхронизации.
pub(crate) fn startup(app: tauri::AppHandle) {
    let config = load_config(&app);
    // Токен нужен раньше скана: приёмник на 47887 может принять ПК в любую
    // секунду после запуска, и он должен узнать тот же токен, что и агент.
    if let Some(pair) = config.pairs.values().next() {
        let _ = crate::set_sync_token(&app, &pair.token);
    }
    std::thread::spawn(move || {
        let root = config.root.clone().or_else(|| {
            crate::open(&app)
                .ok()
                .and_then(|replica| replica.root().ok().flatten())
                .map(|path| path.to_string_lossy().to_string())
        });
        if let Some(root) = root {
            let _ = crate::sync_root(&app, std::path::Path::new(&root));
        }
        crate::server::ensure_started(app);
    });
}

/// Проверяет, что ошибка чтения — это истёкший таймаут, а не разрыв связи.
fn is_timeout(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .map(|io| {
                matches!(
                    io.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                )
            })
            .unwrap_or(false)
    })
}

/// Ошибка означает, что соединение просто закрылось: это не сбой агента.
fn is_disconnect(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .map(|io| {
                matches!(
                    io.kind(),
                    std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                )
            })
            .unwrap_or(false)
            || cause.to_string().contains("пир закрыл сеанс")
    })
}

/// Пересылает ПК ход сканирования, не заваливая сокет сообщением на каждый
/// файл: первый пакет, смена этапа и дальше примерно каждые 32 файла.
struct ScanProgressRelay<'a> {
    transport: &'a mut dyn Transport,
    done: usize,
    total: usize,
    done_bytes: u64,
    total_bytes: u64,
    first: bool,
}

impl<'a> ScanProgressRelay<'a> {
    fn new(transport: &'a mut dyn Transport) -> Self {
        Self {
            transport,
            done: 0,
            total: 0,
            done_bytes: 0,
            total_bytes: 0,
            first: true,
        }
    }

    fn update(&mut self, progress: scan::ScanProgress) {
        let finished = progress.total > 0 && progress.done >= progress.total;
        // Шлём не на каждый файл, а при смене этапа, каждые 32 файла или
        // каждые 8 МиБ: на телефоне файлы мелкие, и пакет на каждый — лишний шум.
        let bytes_advanced = progress.done_bytes >= self.done_bytes.saturating_add(8 * 1024 * 1024);
        if self.first
            || progress.total != self.total
            || progress.total_bytes != self.total_bytes
            || finished
            || bytes_advanced
            || progress.done >= self.done.saturating_add(32)
        {
            self.first = false;
            self.done = progress.done;
            self.total = progress.total;
            self.done_bytes = progress.done_bytes;
            self.total_bytes = progress.total_bytes;
            let _ = agent::send(
                self.transport,
                &AgentMsg::ScanProgress {
                    done: progress.done,
                    total: progress.total,
                    done_bytes: progress.done_bytes,
                    total_bytes: progress.total_bytes,
                },
            );
        }
    }
}

fn command_loop(
    app: &tauri::AppHandle,
    runtime: &AgentRuntime,
    transport: &mut dyn Transport,
) -> anyhow::Result<()> {
    // Сессия считается активной, пока ПК держит соединение и шлёт команды.
    // Во время сопряжения счётчик ещё не растёт: телефон показывает запрос.
    let _session = SessionGuard::new(&runtime.connected);
    // Связь установлена — прошлая ошибка больше не актуальна.
    runtime.status.lock().unwrap().error = None;

    // ПК шлёт heartbeat (GetRoot) каждые несколько секунд. Если сообщений нет
    // дольше AGENT_IDLE_TIMEOUT, считаем, что связь потеряна, и возвращаемся
    // в ожидание. Таймаут ставится на весь кадр: сообщения маленькие и
    // приходят целиком, поэтому частичный кадр не останется в буфере.
    transport
        .set_read_timeout(Some(agent::AGENT_IDLE_TIMEOUT))
        .map_err(|error| anyhow::anyhow!("{error:#}"))?;

    loop {
        let message = match agent::recv(transport) {
            Ok(message) => message,
            Err(error) if is_timeout(&error) => break,
            Err(error) => return Err(error),
        };
        match message {
            AgentMsg::GetRoot => {
                agent::send(
                    transport,
                    &AgentMsg::RootInfo {
                        path: runtime.root(),
                    },
                )?;
            }
            AgentMsg::RequestRootChange => {
                let (tx, rx) = mpsc::channel::<String>();
                runtime.root_change.store(true, Ordering::SeqCst);
                *runtime.root_tx.lock().unwrap() = Some(tx);
                let result = rx.recv_timeout(Duration::from_secs(300));
                runtime.root_change.store(false, Ordering::SeqCst);
                *runtime.root_tx.lock().unwrap() = None;
                match result {
                    Ok(path) if !path.is_empty() => {
                        agent::send(transport, &AgentMsg::RootChanged { path })?;
                    }
                    Ok(_) => {
                        agent::send(
                            transport,
                            &AgentMsg::Error {
                                message: "выбор папки отменён".into(),
                            },
                        )?;
                    }
                    Err(_) => {
                        agent::send(
                            transport,
                            &AgentMsg::Error {
                                message: "папка на телефоне не выбрана".into(),
                            },
                        )?;
                    }
                }
            }
            AgentMsg::Scan => {
                let Some(root) = runtime.root() else {
                    agent::send(
                        transport,
                        &AgentMsg::Error {
                            message: "папка на телефоне не выбрана".into(),
                        },
                    )?;
                    continue;
                };
                // Скан не только отдаёт список файлов ПК, но и обновляет
                // общий реплик: именно из него читает сервер полной
                // синхронизации на 47887. Ошибку скана не скрываем.
                let result = {
                    let mut relay = ScanProgressRelay::new(transport);
                    crate::scan_phone_with_progress(app, &mut |progress| relay.update(progress))
                };
                match result {
                    Ok((root, _)) => {
                        let entries = agent::manifest(&root)?;
                        agent::send(
                            transport,
                            &AgentMsg::Manifest {
                                root: root.to_string_lossy().to_string(),
                                entries,
                            },
                        )?;
                    }
                    Err(error) => {
                        agent::send(
                            transport,
                            &AgentMsg::Error {
                                message: format!("{root}: {error}"),
                            },
                        )?;
                    }
                }
            }
            AgentMsg::ResetHistory => {
                let result = {
                    let mut relay = ScanProgressRelay::new(transport);
                    crate::reset_phone_history(app, &mut |progress| relay.update(progress))
                };
                match result {
                    Ok((root, _)) => {
                        let entries = agent::manifest(&root)?;
                        agent::send(
                            transport,
                            &AgentMsg::Manifest {
                                root: root.to_string_lossy().to_string(),
                                entries,
                            },
                        )?;
                    }
                    Err(error) => {
                        agent::send(
                            transport,
                            &AgentMsg::Error {
                                message: error,
                            },
                        )?;
                    }
                }
            }
            AgentMsg::GetSyncInfo => {
                crate::server::ensure_started(app.clone());
                let info = crate::server::info_json();
                let error = info
                    .get("error")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                agent::send(
                    transport,
                    &AgentMsg::SyncInfo {
                        port: crate::SYNC_PORT,
                        error,
                    },
                )?;
            }
            AgentMsg::Bye => break,
            AgentMsg::Error { message } => anyhow::bail!(message),
            other => {
                agent::send(
                    transport,
                    &AgentMsg::Error {
                        message: format!("неизвестная команда: {other:?}"),
                    },
                )?;
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ команды

#[tauri::command]
pub fn agent_info(app: tauri::AppHandle) -> serde_json::Value {
    let runtime = match app.try_state::<AgentRuntime>() {
        Some(runtime) => runtime,
        None => return serde_json::json!({ "running": false }),
    };
    let identity = runtime.identity.lock().unwrap().clone();
    let status = runtime.status.lock().unwrap();
    let pending = runtime.pending.lock().unwrap().clone();
    let pairs = runtime
        .config
        .lock()
        .unwrap()
        .pairs
        .iter()
        .map(|(dev, pair)| {
            serde_json::json!({
                "dev": dev,
                "name": pair.name,
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "running": status.running,
        "addr": status.addr,
        "error": status.error,
        "root": runtime.root(),
        "connected": runtime.connected.load(Ordering::SeqCst) > 0,
        "root_change": runtime.root_change.load(Ordering::SeqCst),
        "pending": pending.map(|pair| serde_json::json!({
            "dev": pair.dev,
            "name": pair.name,
            "pk": pair.pk,
        })),
        "pairs": pairs,
        "device": identity.as_ref().map(|id| id.dev.clone()),
        "name": identity.as_ref().map(|id| id.name.clone()),
        "pk": identity.as_ref().map(|id| id.pk.clone()),
    })
}

#[tauri::command]
pub async fn agent_set_root(
    app: tauri::AppHandle,
    path: String,
) -> Result<serde_json::Value, String> {
    let root = agent::ensure_root(&path).map_err(err)?;
    let root = root.to_string_lossy().to_string();

    // Сканируем до сохранения: если папка недоступна, агент остаётся
    // на прежнем корне, а не показывает выбранную, но пустую.
    let scan_app = app.clone();
    let scan_root = PathBuf::from(&root);
    crate::blocking(move || crate::sync_root(&scan_app, &scan_root).map(|_| ())).await?;

    let runtime = app
        .try_state::<AgentRuntime>()
        .ok_or_else(|| "агент не запущен".to_string())?;
    runtime.set_root(Some(root.clone()));
    {
        let config = runtime.config.lock().unwrap();
        save_config(&app, &config)?;
    }
    if let Some(tx) = runtime.root_tx.lock().unwrap().as_ref() {
        let _ = tx.send(root);
    }
    Ok(agent_info(app))
}

/// Закрывает выбор папки, если ПК его запросил. Пустой путь — это сигнал
/// «пользователь передумал», чтобы ПК не ждал пять минут.
#[tauri::command]
pub fn agent_cancel_root(app: tauri::AppHandle) {
    if let Some(runtime) = app.try_state::<AgentRuntime>() {
        if let Some(tx) = runtime.root_tx.lock().unwrap().as_ref() {
            let _ = tx.send(String::new());
        }
    }
}

#[tauri::command]
pub fn agent_pair_decision(app: tauri::AppHandle, accept: bool) -> Result<serde_json::Value, String> {
    let runtime = app
        .try_state::<AgentRuntime>()
        .ok_or_else(|| "агент не запущен".to_string())?;
    if let Some(tx) = runtime.pair_tx.lock().unwrap().as_ref() {
        let _ = tx.send(accept);
    }
    Ok(agent_info(app))
}

#[tauri::command]
pub fn agent_forget(app: tauri::AppHandle, dev: String) -> Result<serde_json::Value, String> {
    let runtime = app
        .try_state::<AgentRuntime>()
        .ok_or_else(|| "агент не запущен".to_string())?;
    runtime.config.lock().unwrap().pairs.remove(&dev);
    {
        let config = runtime.config.lock().unwrap();
        save_config(&app, &config)?;
    }
    Ok(agent_info(app))
}
