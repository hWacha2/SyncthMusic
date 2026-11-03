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
    if let Err(error) = session(&app, transport.as_mut()) {
        if let Some(runtime) = app.try_state::<AgentRuntime>() {
            runtime.status.lock().unwrap().error = Some(err(error));
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
    {
        *runtime.pending.lock().unwrap() = Some(PendingPair {
            dev: dev.clone(),
            name: name.clone(),
            pk: pk.clone(),
        });
        *runtime.pair_tx.lock().unwrap() = Some(tx);
    }

    let accepted = rx.recv_timeout(Duration::from_secs(120)).unwrap_or(false);
    *runtime.pending.lock().unwrap() = None;
    *runtime.pair_tx.lock().unwrap() = None;

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
    agent::send(transport, &AgentMsg::PairOk { token })?;
    Ok(())
}

fn command_loop(
    _app: &tauri::AppHandle,
    runtime: &AgentRuntime,
    transport: &mut dyn Transport,
) -> anyhow::Result<()> {
    // Сессия считается активной, пока ПК держит соединение и шлёт команды.
    // Во время сопряжения счётчик ещё не растёт: телефон показывает запрос.
    let _session = SessionGuard::new(&runtime.connected);
    loop {
        match agent::recv(transport)? {
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
                    Ok(path) => {
                        agent::send(transport, &AgentMsg::RootChanged { path })?;
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
                let entries = agent::manifest(std::path::Path::new(&root))?;
                agent::send(transport, &AgentMsg::Manifest { root, entries })?;
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
pub fn agent_set_root(app: tauri::AppHandle, path: String) -> Result<serde_json::Value, String> {
    let root = agent::ensure_root(&path).map_err(err)?;
    let root = root.to_string_lossy().to_string();
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
