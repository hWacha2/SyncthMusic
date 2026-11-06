//! Десктопный клиент SynctMusic.
//!
//! Ядро (`syncth-core`) живёт прямо в приложении: сканирование, план и commit
//! выполняются здесь, без отдельной службы и HTTP-прослойки. Телефон только
//! принимает входящий сеанс и исполняет решения, принятые на ПК.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use syncth_core::agent::{self, AgentIdentity, AgentMsg, AGENT_PROTO};
use syncth_core::net::{PreviewSession, SYNC_PORT};
use syncth_core::plan::{ChangeKind, Plan};
use syncth_core::transport::Transport;
use syncth_core::{scan, vault, Decision, Hlc, Replica};
use tauri::Manager;

const SYNC_BIND: &str = "0.0.0.0:47887";

/// Как часто ПК проверяет живой сеанс и заодно забирает актуальную папку.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);

/// Сколько ждём ответ на heartbeat, прежде чем считать связь потерянной.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(8);

/// Прощание не должно задерживать закрытие окна, если сокет уже завис.
const BYE_TIMEOUT: Duration = Duration::from_secs(2);

/// Токен сохранённой пары принадлежит другому телефону. По этой ошибке USB-
/// подключение повторяется без токена, чтобы новый телефон спросил согласие.
const OTHER_DEVICE_ERROR: &str = "подключилось другое устройство";

/// Сокет текущего подключения, который можно аварийно закрыть кнопкой
/// «Отмена». Хранится отдельно, чтобы команда отмены не ждала мьютекс сессии.
static CANCEL_STREAM: Mutex<Option<TcpStream>> = Mutex::new(None);
static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct Desktop {
    agent: Mutex<AgentState>,
    workspace: Mutex<WorkspaceState>,
    /// Ход долгой проверки. Отдельный мьютекс: интерфейс опрашивает его,
    /// пока `workspace` занят сканированием.
    progress: Mutex<ProgressState>,
}

#[derive(Default)]
struct ProgressState {
    stage: String,
    done: usize,
    total: usize,
    done_bytes: u64,
    total_bytes: u64,
}

#[derive(Default)]
struct AgentState {
    session: Option<LiveSession>,
    last_error: Option<String>,
}

/// Полный синк ПК↔телефон идёт поверх TCP-протокола `syncth-core`
/// и живёт прямо в приложении: отдельного процесса не нужно.
#[derive(Default)]
struct WorkspaceState {
    session: Option<PreviewSession>,
    /// ADB-туннель к приёмнику `47887`: серийник и локальный порт.
    adb_sync: Option<(String, u16)>,
    phone_scan: Option<PhoneScanView>,
    pc_scan: Option<PcScanView>,
    phone_error: Option<String>,
    session_error: Option<String>,
    checked_at: Option<u64>,
}

impl WorkspaceState {
    fn close(&mut self) {
        if let Some(mut session) = self.session.take() {
            session.close();
        }
        if let Some((serial, port)) = self.adb_sync.take() {
            let _ = run_adb(&[
                "-s",
                &serial,
                "forward",
                "--remove",
                &format!("tcp:{port}"),
            ]);
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct PhoneScanView {
    root: String,
    count: usize,
    total: u64,
    at: u64,
}

#[derive(Clone, Debug, Serialize)]
struct PcScanView {
    added: usize,
    changed: usize,
    renamed: usize,
    missing: usize,
    at: u64,
}

/// Постоянное соединение с телефоном. Живёт, пока открыт ПК-клиент.
struct LiveSession {
    transport: Box<dyn Transport>,
    link: AgentLink,
    root: Option<String>,
    last_beat: Instant,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct AgentLink {
    dev: String,
    name: String,
    addr: String,
    token: String,
    root: Option<String>,
    /// Серийный номер ADB, если связь идёт по USB. Тогда адрес получается
    /// через `adb forward` и обновляется при каждом подключении.
    serial: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    root: Option<String>,
    data_dir: Option<String>,
    /// Сопряжённый Android-агент. Путь к папке хранится здесь, на ПК.
    agent: Option<AgentLink>,
    /// Новые файлы не переносятся автоматически: человек выбирает
    /// «перенести» или «оставить только на другой стороне».
    manual_additions: bool,
    /// Правки, переименования и переносы ждут решения человека. Политика
    /// живёт только здесь, в план она не входит: план описывает факты.
    manual_changes: bool,
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

async fn blocking<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("Фоновая задача упала: {e}"))?
}

// ------------------------------------------------------------------ android agent

#[derive(Debug, Clone)]
struct HelloInfo {
    dev: String,
    name: String,
    root: Option<String>,
    token: String,
    /// `true`, если соединение уже было сопряжено и токен пришёл из конфига.
    remembered: bool,
}

fn pc_identity() -> Result<AgentIdentity, String> {
    let config = load_config();
    let data_dir = effective_data_dir(&config);
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let replica = Replica::open(&data_dir, &default_device_id(&data_dir))
        .map_err(|e| format!("{e:#}"))?;
    Ok(AgentIdentity {
        dev: replica.device_id().to_string(),
        name: replica.device_name().to_string(),
        pk: replica.public_key(),
    })
}

fn agent_stream(addr: &str, timeout: Duration) -> Result<TcpStream, String> {
    let address: SocketAddr = addr
        .parse()
        .map_err(|_| format!("некорректный адрес: {addr}"))?;
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(4))
        .map_err(|e| format!("не удалось подключиться к {addr}: {e}"))?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    Ok(stream)
}

/// Открывает соединение, здоровается и при необходимости проводит сопряжение.
///
/// ПК всегда инициатор. Телефон только подтверждает запрос, поэтому вызов
/// может ждать ответа пользователя на телефоне.
fn agent_open(
    addr: &str,
    link: Option<&AgentLink>,
    timeout: Duration,
) -> Result<(Box<dyn Transport>, HelloInfo), String> {
    let identity = pc_identity()?;
    CANCEL_REQUESTED.store(false, Ordering::SeqCst);
    let stream = agent_stream(addr, timeout)?;
    if CANCEL_REQUESTED.load(Ordering::SeqCst) {
        return Err("сопряжение отменено".to_string());
    }

    // Пока идёт рукопожатие, кнопка «Отмена» может закрыть сокет снаружи.
    *CANCEL_STREAM.lock().unwrap() = stream.try_clone().ok();

    let result = agent_handshake(stream, link, identity);
    *CANCEL_STREAM.lock().unwrap() = None;

    if CANCEL_REQUESTED.load(Ordering::SeqCst) {
        return Err("сопряжение отменено".to_string());
    }
    result
}

fn agent_handshake(
    stream: TcpStream,
    link: Option<&AgentLink>,
    identity: AgentIdentity,
) -> Result<(Box<dyn Transport>, HelloInfo), String> {
    let mut transport: Box<dyn Transport> = Box::new(stream);

    agent::send(
        transport.as_mut(),
        &AgentMsg::Hello {
            dev: identity.dev.clone(),
            name: identity.name.clone(),
            pk: identity.pk.clone(),
            proto: AGENT_PROTO,
            token: link.map(|link| link.token.clone()),
        },
    )
    .map_err(|e| format!("{e:#}"))?;

    let ack = agent::recv(transport.as_mut()).map_err(|e| format!("{e:#}"))?;
    let (remote_dev, remote_name, paired, remote_root) = match ack {
        AgentMsg::HelloAck {
            dev,
            name,
            proto,
            paired,
            root,
            ..
        } => {
            if proto != AGENT_PROTO {
                return Err(format!(
                    "версия протокола не совпадает: {proto} вместо {AGENT_PROTO}"
                ));
            }
            (dev, name, paired, root)
        }
        AgentMsg::Error { message } => return Err(message),
        other => return Err(format!("телефон ответил неожиданно: {other:?}")),
    };

    if let Some(link) = link {
        if link.dev != remote_dev {
            return Err(OTHER_DEVICE_ERROR.to_string());
        }
    }

    if !paired {
        agent::send(
            transport.as_mut(),
            &AgentMsg::PairAsk {
                dev: identity.dev.clone(),
                name: identity.name.clone(),
                pk: identity.pk.clone(),
            },
        )
        .map_err(|e| format!("{e:#}"))?;

        let token = match agent::recv(transport.as_mut()).map_err(|e| format!("{e:#}"))? {
            AgentMsg::PairOk { token } => token,
            AgentMsg::PairNo { reason } => return Err(reason),
            AgentMsg::Error { message } => return Err(message),
            other => return Err(format!("телефон ответил неожиданно: {other:?}")),
        };

        return Ok((
            transport,
            HelloInfo {
                dev: remote_dev,
                name: remote_name,
                root: remote_root,
                token,
                remembered: false,
            },
        ));
    }

    let token = link
        .map(|link| link.token.clone())
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "телефон считает ПК сопряжённым, но токена нет".to_string())?;

    Ok((
        transport,
        HelloInfo {
            dev: remote_dev,
            name: remote_name,
            root: remote_root,
            token,
            remembered: true,
        },
    ))
}

fn save_agent_link(link: AgentLink) -> Result<(), String> {
    let mut config = load_config();
    config.agent = Some(link);
    save_config(&config)
}

/// Консольные дочерние процессы (`adb`, `netsh`, `powershell`) при запуске из
/// GUI-приложения получают собственное окно консоли. На Windows его скрывает
/// `CREATE_NO_WINDOW`; на других платформах флаг не нужен.
fn hidden_command(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn run_adb(args: &[&str]) -> Result<String, String> {
    let output = hidden_command(Command::new("adb").args(args))
        .output()
        .map_err(|e| format!("adb не найден: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "adb вернул ошибку".to_string()
        } else {
            stderr
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn adb_forward(serial: &str) -> Result<String, String> {
    let text = run_adb(&["-s", serial, "forward", "tcp:0", "tcp:47890"])?;
    let port = text
        .trim()
        .lines()
        .last()
        .unwrap_or_default()
        .trim()
        .parse::<u16>()
        .map_err(|_| format!("adb не выдал порт: {}", text.trim()))?;
    Ok(format!("127.0.0.1:{port}"))
}

fn link_addr(link: &AgentLink) -> Result<String, String> {
    if let Some(serial) = link.serial.as_deref().filter(|value| !value.is_empty()) {
        // Убираем прошлое правило forward, иначе они копятся при каждом запуске.
        if let Some(port) = link
            .addr
            .rsplit(':')
            .next()
            .and_then(|port| port.parse::<u16>().ok())
        {
            let _ = run_adb(&["-s", serial, "forward", "--remove", &format!("tcp:{port}")]);
        }
        return adb_forward(serial);
    }
    if link.addr.is_empty() {
        return Err("адрес устройства не сохранён".to_string());
    }
    Ok(link.addr.clone())
}

#[tauri::command]
fn agent_cancel() {
    CANCEL_REQUESTED.store(true, Ordering::SeqCst);
    if let Some(stream) = CANCEL_STREAM.lock().unwrap().take() {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

/// Ставит постоянный сеанс и закрывает предыдущий, если он был.
fn install_session(app: &tauri::AppHandle, transport: Box<dyn Transport>, link: AgentLink) {
    let state = app.state::<Desktop>();
    let mut guard = state.agent.lock().unwrap();
    if let Some(mut old) = guard.session.take() {
        let _ = old.transport.set_write_timeout(Some(BYE_TIMEOUT));
        let _ = agent::send(old.transport.as_mut(), &AgentMsg::Bye);
        old.transport.shutdown();
    }
    guard.last_error = None;
    guard.session = Some(LiveSession {
        transport,
        root: link.root.clone(),
        link,
        last_beat: Instant::now(),
    });
}

fn close_session(app: &tauri::AppHandle) {
    let state = app.state::<Desktop>();
    let mut guard = state.agent.lock().unwrap();
    if let Some(mut session) = guard.session.take() {
        let _ = session.transport.set_write_timeout(Some(BYE_TIMEOUT));
        let _ = agent::send(session.transport.as_mut(), &AgentMsg::Bye);
        session.transport.shutdown();
    }
}

/// Прощание при закрытии окна. Если сеанс занят длинной командой, не ждём:
/// процесс всё равно закроет сокет, и телефон увидит разрыв.
fn close_session_now(app: &tauri::AppHandle) {
    let state = app.state::<Desktop>();
    {
        let Ok(mut guard) = state.agent.try_lock() else {
            return;
        };
        if let Some(mut session) = guard.session.take() {
            let _ = session.transport.set_write_timeout(Some(BYE_TIMEOUT));
            let _ = agent::send(session.transport.as_mut(), &AgentMsg::Bye);
            session.transport.shutdown();
        }
    }
    // Полный сеанс тоже закрываем: иначе телефон будет ждать commit,
    // пока ОС не прибьёт сокет, а ADB-туннель останется висеть.
    if let Ok(mut workspace) = state.workspace.try_lock() {
        workspace.close();
    };
}

/// Выполняет команду на уже открытом сеансе.
///
/// Внешняя ошибка означает разрыв или рассинхронизацию протокола — сеанс
/// сбрасывается, и следующий статус покажет «нет связи». Внутренняя ошибка —
/// это обычный отказ телефона (например, отмена выбора папки): сеанс остаётся
/// живым, и heartbeat продолжает идти.
fn with_session<T>(
    app: &tauri::AppHandle,
    action: impl FnOnce(&mut LiveSession) -> Result<Result<T, String>, String>,
) -> Result<T, String> {
    let state = app.state::<Desktop>();
    let mut guard = state
        .agent
        .lock()
        .map_err(|_| "состояние связи недоступно".to_string())?;
    let Some(session) = guard.session.as_mut() else {
        return Err(guard
            .last_error
            .clone()
            .unwrap_or_else(|| "телефон не подключён".to_string()));
    };

    match action(session) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(message)) => Err(message),
        Err(error) => {
            if let Some(mut dead) = guard.session.take() {
                dead.transport.shutdown();
            }
            guard.last_error = Some(error.clone());
            Err(error)
        }
    }
}

/// Heartbeat: заодно забирает актуальную папку, если её изменили на телефоне.
fn query_root(session: &mut LiveSession) -> Result<Option<String>, String> {
    session
        .transport
        .set_read_timeout(Some(HEARTBEAT_TIMEOUT))
        .map_err(|error| format!("{error:#}"))?;
    agent::send(session.transport.as_mut(), &AgentMsg::GetRoot)
        .map_err(|error| format!("{error:#}"))?;
    loop {
        match agent::recv(session.transport.as_mut()).map_err(|error| format!("{error:#}"))? {
            AgentMsg::RootInfo { path } => return Ok(path),
            AgentMsg::Error { message } => return Err(message),
            _ => continue,
        }
    }
}

fn start_heartbeat(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(HEARTBEAT_EVERY);

        // Если сеанс занят длинной командой (выбор папки, скан), пропускаем
        // удар: телефон в это время тоже не читает сокет.
        let state = app.state::<Desktop>();
        let Ok(mut guard) = state.agent.try_lock() else {
            continue;
        };
        let Some(session) = guard.session.as_mut() else {
            continue;
        };

        match query_root(session) {
            Ok(root) => {
                session.last_beat = Instant::now();
                if session.root != root {
                    session.root = root.clone();
                    session.link.root = root;
                    let _ = save_agent_link(session.link.clone());
                }
            }
            Err(error) => {
                if let Some(mut dead) = guard.session.take() {
                    dead.transport.shutdown();
                }
                guard.last_error = Some(error);
            }
        }
    });
}

/// Человеческое имя телефона, подключённого по USB.
///
/// `adb devices -l` отдаёт только кодовое обозначение модели, например
/// `2511FPC34G`. Маркетинговое название лежит в системных свойствах, поэтому
/// для авторизованного устройства читаем их одним вызовом. Если прочитать не
/// удалось, остаётся модель из списка или серийный номер.
fn adb_device_name(serial: &str, model: &str) -> String {
    let props = run_adb(&[
        "-s",
        serial,
        "shell",
        "getprop ro.product.marketname; getprop ro.product.model",
    ])
    .unwrap_or_default();
    let mut lines = props.lines().map(str::trim);
    let market = lines.next().unwrap_or_default();
    let system_model = lines.next().unwrap_or_default();
    let model = if system_model.is_empty() { model } else { system_model };

    // Как и на телефоне: не дублируем модель, если она уже входит в название.
    let duplicate = market.eq_ignore_ascii_case(model)
        || (!model.is_empty() && market.to_lowercase().contains(&model.to_lowercase()));
    if !market.is_empty() && !model.is_empty() && !duplicate {
        format!("{market} ({model})")
    } else if !market.is_empty() {
        market.to_string()
    } else if !model.is_empty() {
        model.to_string()
    } else {
        serial.to_string()
    }
}

#[tauri::command]
async fn discover_adb() -> Result<Value, String> {
    blocking(|| {
        let text = run_adb(&["devices", "-l"])?;
        let mut devices = Vec::new();
        for line in text.lines().skip(1) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(serial) = parts.next() else {
                continue;
            };
            let state = parts.next().unwrap_or_default();
            if !matches!(state, "device" | "unauthorized" | "offline") {
                continue;
            }
            let mut model = String::new();
            for field in parts {
                if let Some(value) = field.strip_prefix("model:") {
                    model = value.replace('_', " ");
                }
            }
            let model = if model.is_empty() {
                serial.to_string()
            } else {
                model
            };
            let name = if state == "device" {
                adb_device_name(serial, &model)
            } else {
                model.clone()
            };
            devices.push(json!({
                "serial": serial,
                "model": model,
                "name": name,
                "state": state,
            }));
        }
        Ok(json!({ "devices": devices }))
    })
    .await
}

#[tauri::command]
async fn discover_agents() -> Result<Value, String> {
    blocking(|| {
        let devices = agent::discover(Duration::from_millis(1400))
            .map_err(|e| format!("{e:#}"))?;
        let linked = load_config().agent.map(|link| link.dev);
        Ok(json!({ "devices": devices, "linked": linked }))
    })
    .await
}

const FIREWALL_RULE: &str = "SynctMusic LAN discovery";

/// Добавляет постоянное правило для входящих UDP-ответов discovery.
///
/// Приложение обычно запущено без прав администратора, поэтому правило
/// добавляет этот же exe, перезапущенный через UAC с флагом
/// `--allow-firewall`. Профиль `any` нужен потому, что Windows часто
/// считает домашнюю сеть доменной.
#[tauri::command]
async fn allow_lan_discovery() -> Result<Value, String> {
    #[cfg(not(windows))]
    {
        return Ok(json!({ "allowed": false, "reason": "unsupported" }));
    }

    #[cfg(windows)]
    {
        blocking(|| {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let exe_text = exe.to_string_lossy().replace('\'', "''");
            let script = format!(
                "$p = Start-Process -FilePath '{exe_text}' -ArgumentList '--allow-firewall' -Verb RunAs -Wait -PassThru; exit $p.ExitCode"
            );
            let status = hidden_command(Command::new("powershell").args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &script,
            ]))
            .status()
            .map_err(|e| format!("Не удалось запустить PowerShell: {e}"))?;
            if !status.success() {
                return Err(
                    "Правило не добавлено: запрос администратора отменён или netsh вернул ошибку."
                        .to_string(),
                );
            }
            Ok(json!({ "allowed": true }))
        })
        .await
    }
}

/// Выполняется в elevated-процессе. Возвращает код выхода.
pub fn firewall_helper() -> i32 {
    match add_firewall_rule() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn add_firewall_rule() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let program = format!("program={}", exe.to_string_lossy());

    let _ = hidden_command(
        Command::new("netsh")
            .args([
                "advfirewall",
                "firewall",
                "delete",
                "rule",
                &format!("name={FIREWALL_RULE}"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .status();

    let output = hidden_command(
        Command::new("netsh")
            .args([
                "advfirewall",
                "firewall",
                "add",
                "rule",
                &format!("name={FIREWALL_RULE}"),
                "dir=in",
                "action=allow",
                &program,
                "protocol=udp",
                "profile=any",
                "enable=yes",
            ])
            .stdin(Stdio::null()),
    )
    .output()
    .map_err(|e| format!("netsh: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[tauri::command]
async fn agent_connect(
    app: tauri::AppHandle,
    addr: String,
    dev: Option<String>,
) -> Result<Value, String> {
    blocking(move || {
        let config = load_config();
        let link = config.agent.as_ref();
        let link = match (link, dev.as_deref()) {
            (Some(link), Some(dev)) if link.dev == dev => Some(link.clone()),
            (Some(_), None) => link.cloned(),
            _ => None,
        };

        let (transport, info) =
            agent_open(addr.trim(), link.as_ref(), agent::PAIR_TIMEOUT + Duration::from_secs(5))?;

        let full = AgentLink {
            dev: info.dev.clone(),
            name: info.name.clone(),
            addr: addr.trim().to_string(),
            token: info.token.clone(),
            root: info.root.clone(),
            serial: None,
        };
        save_agent_link(full.clone())?;
        install_session(&app, transport, full);

        Ok(json!({
            "dev": info.dev,
            "name": info.name,
            "addr": addr.trim(),
            "serial": null,
            "root": info.root,
            "remembered": info.remembered,
            "needs_root": info.root.is_none(),
        }))
    })
    .await
}

#[tauri::command]
async fn agent_connect_adb(app: tauri::AppHandle, serial: String) -> Result<Value, String> {
    blocking(move || {
        // Токен пары не привязан к ADB-серийнику: тот же телефон может
        // подключиться по USB после LAN или получить новый серийник. Поэтому
        // сохранённую пару пробуем всегда, а не только при совпадении serial.
        let saved = load_config().agent;
        let addr = adb_forward(&serial)?;
        match adb_connect_once(&app, &serial, &addr, saved.as_ref()) {
            // Ответил другой телефон: повторяем без токена, иначе вместо
            // запроса на сопряжение человек увидел бы ошибку.
            Err(error) if saved.is_some() && error == OTHER_DEVICE_ERROR => {
                adb_connect_once(&app, &serial, &addr, None)
            }
            result => result,
        }
    })
    .await
}

fn adb_connect_once(
    app: &tauri::AppHandle,
    serial: &str,
    addr: &str,
    link: Option<&AgentLink>,
) -> Result<Value, String> {
    let (transport, info) =
        agent_open(addr, link, agent::PAIR_TIMEOUT + Duration::from_secs(5))?;

    let full = AgentLink {
        dev: info.dev.clone(),
        name: info.name.clone(),
        addr: addr.to_string(),
        token: info.token.clone(),
        root: info.root.clone(),
        serial: Some(serial.to_string()),
    };
    save_agent_link(full.clone())?;
    install_session(app, transport, full);

    Ok(json!({
        "dev": info.dev,
        "name": info.name,
        "addr": addr,
        "serial": serial,
        "root": info.root,
        "remembered": info.remembered,
        "needs_root": info.root.is_none(),
    }))
}

/// Переподключается к сохранённому телефону при запуске ПК-клиента.
#[tauri::command]
async fn agent_reconnect(app: tauri::AppHandle) -> Result<Value, String> {
    blocking(move || {
        let link = load_config()
            .agent
            .ok_or_else(|| "телефон ещё не подключён".to_string())?;
        let addr = link_addr(&link)?;
        let (transport, info) =
            agent_open(&addr, Some(&link), agent::PAIR_TIMEOUT + Duration::from_secs(5))?;

        let full = AgentLink {
            dev: info.dev.clone(),
            name: info.name.clone(),
            addr: addr.clone(),
            token: info.token.clone(),
            root: info.root.clone(),
            serial: link.serial.clone(),
        };
        save_agent_link(full.clone())?;
        install_session(&app, transport, full);

        Ok(json!({
            "dev": info.dev,
            "name": info.name,
            "addr": addr,
            "serial": link.serial,
            "root": info.root,
            "remembered": info.remembered,
            "needs_root": info.root.is_none(),
        }))
    })
    .await
}

#[tauri::command]
async fn agent_request_root_change(app: tauri::AppHandle) -> Result<Value, String> {
    blocking(move || {
        let root = with_session(&app, |session| {
            session
                .transport
                .set_read_timeout(Some(Duration::from_secs(310)))
                .map_err(|error| format!("{error:#}"))?;
            agent::send(session.transport.as_mut(), &AgentMsg::RequestRootChange)
                .map_err(|e| format!("{e:#}"))?;

            match agent::recv(session.transport.as_mut()).map_err(|e| format!("{e:#}"))? {
                AgentMsg::RootChanged { path } => {
                    session.root = Some(path.clone());
                    session.link.root = Some(path.clone());
                    Ok(Ok(path))
                }
                AgentMsg::Error { message } => Ok(Err(message)),
                other => Err(format!("телефон ответил неожиданно: {other:?}")),
            }
        })?;

        let mut updated = load_config()
            .agent
            .ok_or_else(|| "телефон ещё не подключён".to_string())?;
        updated.root = Some(root.clone());
        save_agent_link(updated)?;
        Ok(json!({ "root": root }))
    })
    .await
}

#[tauri::command]
async fn agent_scan(app: tauri::AppHandle) -> Result<Value, String> {
    blocking(move || scan_phone_now(&app)).await
}

#[tauri::command]
fn agent_status(app: tauri::AppHandle) -> Value {
    let state = app.state::<Desktop>();
    let guard = match state.agent.try_lock() {
        Ok(guard) => guard,
        // Сеанс занят длинной командой; пусть UI сохранит прошлое состояние.
        Err(_) => return json!({ "busy": true }),
    };

    if let Some(session) = &guard.session {
        return json!({
            "busy": false,
            "connected": true,
            "dev": session.link.dev,
            "name": session.link.name,
            "addr": session.link.addr,
            "serial": session.link.serial,
            "root": session.root,
            "idle_secs": session.last_beat.elapsed().as_secs(),
        });
    }

    json!({
        "busy": false,
        "connected": false,
        "error": guard.last_error,
        "root": load_config().agent.and_then(|link| link.root),
    })
}

#[tauri::command]
fn agent_link() -> Value {
    match load_config().agent {
        Some(link) => json!({
            "linked": true,
            "dev": link.dev,
            "name": link.name,
            "addr": link.addr,
            "root": link.root,
            "serial": link.serial,
        }),
        None => json!({ "linked": false }),
    }
}

#[tauri::command]
fn agent_forget(app: tauri::AppHandle) -> Result<Value, String> {
    close_session(&app);
    let mut config = load_config();
    if let Some(link) = config.agent.take() {
        if let Some(serial) = link.serial.as_deref().filter(|value| !value.is_empty()) {
            if let Some(port) = link
                .addr
                .rsplit(':')
                .next()
                .and_then(|port| port.parse::<u16>().ok())
            {
                let _ = run_adb(&["-s", serial, "forward", "--remove", &format!("tcp:{port}")]);
            }
        }
    }
    save_config(&config)?;
    Ok(json!({ "linked": false }))
}

/// Разрывает текущий сеанс, но оставляет сохранённую пару: следующее
/// подключение к тому же телефону пройдёт без подтверждения.
#[tauri::command]
fn agent_disconnect(app: tauri::AppHandle) -> Value {
    close_session(&app);
    {
        let desktop = app.state::<Desktop>();
        if let Ok(mut ws) = desktop.workspace.lock() {
            ws.close();
            ws.session_error = None;
        };
    }
    agent_link()
}

// ------------------------------------------------------------------ workspace

/// Реплика библиотеки на ПК. Соединение с телефоном живёт в памяти окна,
/// поэтому каждая команда открывает свежий SQLite-хэндл: WAL это допускает.
fn workspace_replica() -> Result<Replica, String> {
    let config = load_config();
    let root =
        effective_root(&config).ok_or_else(|| "Сначала выберите папку с музыкой".to_string())?;
    if !root.is_dir() {
        return Err(format!("Папка библиотеки не найдена: {}", root.display()));
    }
    let data_dir = effective_data_dir(&config);
    std::fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
    let mut replica = Replica::open(&data_dir, &default_device_id(&data_dir))
        .map_err(|error| format!("{error:#}"))?;
    // Список «Исключения» и отмену ведёт только ПК.
    replica.enable_exclusions();
    let known = replica.root().map_err(|error| format!("{error:#}"))?;
    if known.as_deref() != Some(root.as_path()) {
        replica
            .set_root(&root)
            .map_err(|error| format!("{error:#}"))?;
    }
    Ok(replica)
}

fn parse_decision(action: &str) -> Result<Decision, String> {
    Ok(match action {
        "accept" => Decision::Accept,
        "revert" => Decision::Revert,
        "keep" => Decision::Keep,
        "delete_everywhere" => Decision::DeleteEverywhere,
        "keep_in_library" => Decision::KeepInLibrary,
        "only_here" => Decision::OnlyHere,
        "keep_both" => Decision::KeepBoth,
        other => return Err(format!("Неизвестное решение: {other}")),
    })
}

/// Единый предикат «путь ждёт решения». Его используют и интерфейс, и guard
/// перед commit: если список пуст, передавать можно. Ручная политика живёт
/// только здесь, в настройке ПК: план описывает факты, а какие из них требуют
/// человека, решает приложение.
fn pending_paths(plan: &Plan, config: &Config) -> BTreeSet<String> {
    let mut pending = BTreeSet::new();

    for conflict in &plan.conflicts {
        if plan.explicit_entry(&conflict.path).is_none() {
            pending.insert(conflict.path.clone());
        }
    }

    for change in plan.local.iter().chain(plan.remote.iter()) {
        if change.aligned || change.ignored {
            continue;
        }
        // Конфликт уже учтён выше и решается своим набором действий.
        if plan.conflict(&change.path).is_some() {
            continue;
        }
        let manual = match change.kind {
            ChangeKind::Deleted => true,
            ChangeKind::Added => config.manual_additions,
            ChangeKind::Updated | ChangeKind::Renamed | ChangeKind::Moved => {
                config.manual_changes
            }
        };
        if manual && plan.explicit_entry(&change.path).is_none() {
            pending.insert(change.path.clone());
        }
    }

    pending
}

fn plan_payload(plan: &Plan, config: &Config) -> Value {
    let resolution = plan.resolve();
    let pending = pending_paths(plan, config);
    let unresolved: BTreeSet<&str> = resolution
        .unresolved()
        .iter()
        .map(String::as_str)
        .collect();
    let changes = plan
        .local
        .iter()
        .chain(plan.remote.iter())
        .map(|change| {
            json!({
                "path": &change.path,
                "from": &change.from,
                "kind": change.kind,
                "side": change.side,
                "size": change.size,
                "present": change.present,
                "ignored": change.ignored,
                // Итог может быть базовым (авто) или явным решением человека.
                "decision": resolution.action(&change.path).map(Decision::as_str),
                // Удаление по умолчанию «оставить», поэтому одного decision
                // мало: интерфейсу нужно знать, подтвердил ли это человек.
                "decided": plan.explicit_entry(&change.path).is_some(),
                // Совпавшие файлы нужны коммиту для фиксации базы, но в
                // списке изменений и в счётчике их быть не должно.
                "aligned": change.aligned,
                "pending": pending.contains(&change.path),
                "unresolved": unresolved.contains(change.path.as_str()),
            })
        })
        .collect::<Vec<_>>();

    let conflicts = plan
        .conflicts
        .iter()
        .map(|conflict| {
            json!({
                "path": &conflict.path,
                "reason": conflict.reason,
                "local_path": &conflict.local_path,
                "remote_path": &conflict.remote_path,
                "local_size": conflict.local_size,
                "remote_size": conflict.remote_size,
                "local_blob": &conflict.local_blob,
                "remote_blob": &conflict.remote_blob,
                "decision": resolution.action(&conflict.path).map(Decision::as_str),
                "decided": plan.explicit_entry(&conflict.path).is_some(),
                "pending": pending.contains(&conflict.path),
                "unresolved": unresolved.contains(conflict.path.as_str()),
            })
        })
        .collect::<Vec<_>>();

    json!({
        "peer": { "dev": &plan.peer_dev, "name": &plan.peer_name },
        "created_at": plan.created_at,
        "expires_at": plan.expires_at,
        "version": plan.version,
        "paths": plan.path_count(),
        "aligned": plan.aligned_count(),
        "unresolved": resolution.unresolved(),
        "pending": pending.into_iter().collect::<Vec<_>>(),
        "changes": changes,
        "conflicts": conflicts,
    })
}

fn workspace_payload(app: &tauri::AppHandle, replica: &Replica, ws: &WorkspaceState) -> Value {
    let config = load_config();
    let link = config.agent.clone();
    let live = replica.live_entries().unwrap_or_default();
    let files = live.len();
    let total = live.iter().map(|entry| entry.size).sum::<u64>();
    let local_rows = replica.local_entries().unwrap_or_default();
    let local = local_rows.len();
    let local_total = local_rows.iter().map(|entry| entry.size).sum::<u64>();
    let vault_rows = replica.vault_rows().unwrap_or_default();
    let vault_size = vault_rows.iter().map(|row| row.size).sum::<u64>();
    let exclusions = replica
        .exclusions()
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            json!({
                "path": row.path,
                "side": row.side,
                "size": row.size,
                "created_at": row.created_at,
            })
        })
        .collect::<Vec<_>>();
    let pending = replica
        .pending_deletes()
        .unwrap_or_default()
        .into_iter()
        .map(|row| json!({ "path": row.path, "detected_at": row.detected_at }))
        .collect::<Vec<_>>();
    let peers = replica
        .known_peers()
        .unwrap_or_default()
        .into_iter()
        .map(|peer| json!({ "dev": peer.dev, "name": peer.name, "addr": peer.addr }))
        .collect::<Vec<_>>();

    let (agent_connected, agent_error) = {
        let state = app.state::<Desktop>();
        let values = match state.agent.try_lock() {
            Ok(guard) => (guard.session.is_some(), guard.last_error.clone()),
            // Команда уже держит сеанс — значит, связь есть.
            Err(_) => (true, None),
        };
        values
    };

    let session = ws.session.as_ref().map(|session| {
        json!({
            "open": true,
            "peer": { "dev": &session.peer_dev, "name": &session.peer_name },
            "paths": session.paths(),
            "unresolved": session.unresolved(),
            "read_only": session.read_only,
            "finished": session.finished,
            "diverged": session.diverged,
            "hash": session.decisions_hash(),
        })
    });

    json!({
        "pc": {
            "root": replica.root().ok().flatten().map(|path| path.to_string_lossy().to_string()),
            "device": replica.device_id(),
            "name": replica.device_name(),
            "files": files,
            "local": local,
            "local_size": local_total,
            "total": total,
            "vault_files": vault_rows.len(),
            "vault_size": vault_size,
            "pending": pending,
            "peers": peers,
        },
        "manual_additions": config.manual_additions,
        "manual_changes": config.manual_changes,
        "phone": link.map(|link| json!({
            "linked": true,
            "connected": agent_connected,
            "error": agent_error,
            "dev": link.dev,
            "name": link.name,
            "addr": link.addr,
            "root": link.root,
            "serial": link.serial,
        })).unwrap_or_else(|| json!({ "linked": false, "connected": false })),
        "phone_scan": ws.phone_scan.as_ref(),
        "pc_scan": ws.pc_scan.as_ref(),
        "phone_error": ws.phone_error,
        "session_error": ws.session_error,
        "checked_at": ws.checked_at,
        "session": session,
        "plan": ws.session.as_ref().map(|session| plan_payload(&session.plan, &config)),
        "vault": vault_rows.into_iter().map(|row| json!({
            "path": row.path,
            "size": row.size,
            "deleted_at": row.deleted_at,
        })).collect::<Vec<_>>(),
        "exclusions": exclusions,
    })
}

/// Спрашивает телефон, поднят ли у него приём полной синхронизации.
fn query_sync_info(app: &tauri::AppHandle) -> Result<(u16, Option<String>), String> {
    with_session(app, |session| {
        session
            .transport
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|error| format!("{error:#}"))?;
        agent::send(session.transport.as_mut(), &AgentMsg::GetSyncInfo)
            .map_err(|error| format!("{error:#}"))?;
        loop {
            match agent::recv(session.transport.as_mut()).map_err(|error| format!("{error:#}"))? {
                AgentMsg::SyncInfo { port, error } => return Ok(Ok((port, error))),
                AgentMsg::Error { message } => return Ok(Err(message)),
                _ => continue,
            }
        }
    })
}

/// Адрес приёмника `47887` на телефоне. По Wi-Fi это тот же IP, что у агента;
/// по USB — отдельный `adb forward`, который снимается при закрытии сеанса.
fn phone_sync_addr(ws: &mut WorkspaceState) -> Result<String, String> {
    let link = load_config()
        .agent
        .ok_or_else(|| "Телефон не подключён".to_string())?;
    if let Some(serial) = link.serial.clone().filter(|value| !value.is_empty()) {
        // Сначала создаём новый туннель и только потом убираем старый. Если
        // снять порт заранее, adb может выдать тот же номер, но слушатель
        // ещё не будет готов, и первый коннект получит отказ.
        let previous = ws.adb_sync.clone();
        let text = run_adb(&[
            "-s",
            &serial,
            "forward",
            "tcp:0",
            &format!("tcp:{SYNC_PORT}"),
        ])?;
        let port = text
            .trim()
            .lines()
            .last()
            .unwrap_or_default()
            .trim()
            .parse::<u16>()
            .map_err(|_| format!("adb не выдал порт: {}", text.trim()))?;
        if let Some((old_serial, old_port)) = previous {
            if old_port != port {
                let _ = run_adb(&[
                    "-s",
                    &old_serial,
                    "forward",
                    "--remove",
                    &format!("tcp:{old_port}"),
                ]);
            }
        }
        ws.adb_sync = Some((serial, port));
        return Ok(format!("127.0.0.1:{port}"));
    }
    let ip = link
        .addr
        .rsplit_once(':')
        .map(|(ip, _)| ip)
        .filter(|ip| !ip.is_empty())
        .ok_or_else(|| "Адрес телефона не сохранён".to_string())?;
    Ok(format!("{ip}:{SYNC_PORT}"))
}

/// Открывает preview-сеанс, если его нет, и забирает решения телефона.
fn ensure_preview(
    app: &tauri::AppHandle,
    ws: &mut WorkspaceState,
    replica: &mut Replica,
) -> Result<(), String> {
    let config = load_config();
    let link = config
        .agent
        .clone()
        .ok_or_else(|| "Телефон не подключён".to_string())?;

    if let Some(session) = ws.session.as_mut() {
        if !session.finished && session.peer_dev == link.dev {
            if let Err(error) = session.poll(replica) {
                ws.close();
                return Err(format!("{error:#}"));
            }
            return Ok(());
        }
        ws.close();
    }

    // Ошибка в статусе приёмника может быть устаревшей: слушатель уже поднят,
    // а прошлый сеанс оборвался. Пробуем подключиться в любом случае, а
    // подсказку телефона показываем только если все попытки не удались.
    let sync_hint = match query_sync_info(app) {
        Ok((_port, Some(error))) => Some(error),
        _ => None,
    };

    let addr = phone_sync_addr(ws)?;
    let mut last = String::new();
    for attempt in 0..4 {
        match PreviewSession::open(replica, &addr, &link.token) {
            Ok(mut session) => {
                let _ = session.poll(replica);
                ws.session = Some(session);
                return Ok(());
            }
            Err(error) => {
                last = format!("{error:#}");
                if attempt < 3 {
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
    }
    match sync_hint {
        Some(hint) => Err(format!(
            "Не удалось открыть сеанс с телефоном: {last}. Приём на телефоне сообщает: {hint}"
        )),
        None => Err(format!("Не удалось открыть сеанс с телефоном: {last}")),
    }
}

/// Обновить ход проверки для интерфейса. Мьютекс отдельный от `workspace`,
/// поэтому опрос не ждёт конца сканирования.
fn set_progress(
    app: &tauri::AppHandle,
    stage: &str,
    done: usize,
    total: usize,
    done_bytes: u64,
    total_bytes: u64,
) {
    if let Some(state) = app.try_state::<Desktop>() {
        if let Ok(mut progress) = state.progress.lock() {
            progress.stage = stage.to_string();
            progress.done = done;
            progress.total = total;
            progress.done_bytes = done_bytes;
            progress.total_bytes = total_bytes;
        }
    }
}

fn clear_progress(app: &tauri::AppHandle) {
    set_progress(app, "", 0, 0, 0, 0);
}

/// Скан телефона через агентский сеанс. Заодно обновляет сохранённый корень.
fn scan_phone_now(app: &tauri::AppHandle) -> Result<Value, String> {
    let (root, entries) = with_session(app, |session| {
        session
            .transport
            .set_read_timeout(Some(Duration::from_secs(610)))
            .map_err(|error| format!("{error:#}"))?;
        agent::send(session.transport.as_mut(), &AgentMsg::Scan)
            .map_err(|error| format!("{error:#}"))?;

        loop {
            match agent::recv(session.transport.as_mut())
                .map_err(|error| format!("{error:#}"))?
            {
                AgentMsg::Manifest { root, entries } => {
                    session.root = Some(root.clone());
                    session.link.root = Some(root.clone());
                    return Ok(Ok((root, entries)));
                }
                AgentMsg::Error { message } => return Ok(Err(message)),
                AgentMsg::ScanProgress {
                    done,
                    total,
                    done_bytes,
                    total_bytes,
                } => {
                    set_progress(app, "phone", done, total, done_bytes, total_bytes);
                }
                // В очереди мог остаться ответ на heartbeat `GetRoot`.
                _ => {}
            }
        }
    })?;

    let total = entries.iter().map(|entry| entry.size).sum::<u64>();
    let mut config = load_config();
    if let Some(link) = config.agent.as_mut() {
        link.root = Some(root.clone());
        save_config(&config)?;
    }
    Ok(json!({
        "root": root,
        "count": entries.len(),
        "total": total,
    }))
}

/// Сбрасывает историю на телефоне: агент забывает журнал, заново сканирует
/// папку и возвращает свежий список файлов.
fn clear_phone_history(app: &tauri::AppHandle) -> Result<Value, String> {
    let (root, entries) = with_session(app, |session| {
        session
            .transport
            .set_read_timeout(Some(Duration::from_secs(610)))
            .map_err(|error| format!("{error:#}"))?;
        agent::send(session.transport.as_mut(), &AgentMsg::ResetHistory)
            .map_err(|error| format!("{error:#}"))?;

        loop {
            match agent::recv(session.transport.as_mut())
                .map_err(|error| format!("{error:#}"))?
            {
                AgentMsg::Manifest { root, entries } => {
                    session.root = Some(root.clone());
                    session.link.root = Some(root.clone());
                    return Ok(Ok((root, entries)));
                }
                AgentMsg::Error { message } => return Ok(Err(message)),
                AgentMsg::ScanProgress {
                    done,
                    total,
                    done_bytes,
                    total_bytes,
                } => {
                    set_progress(app, "phone", done, total, done_bytes, total_bytes);
                }
                // В очереди мог остаться ответ на heartbeat `GetRoot`.
                _ => {}
            }
        }
    })?;

    let total = entries.iter().map(|entry| entry.size).sum::<u64>();
    let mut config = load_config();
    if let Some(link) = config.agent.as_mut() {
        link.root = Some(root.clone());
        save_config(&config)?;
    }
    Ok(json!({
        "root": root,
        "count": entries.len(),
        "total": total,
    }))
}

fn refresh_workspace(app: &tauri::AppHandle) -> Result<Value, String> {
    set_progress(app, "pc_walk", 0, 0, 0, 0);
    let mut replica = workspace_replica()?;
    let root = replica
        .root()
        .map_err(|error| format!("{error:#}"))?
        .ok_or_else(|| "Корень библиотеки не задан".to_string())?;
    let report = scan::scan_with_progress(&root, &mut replica, &mut |progress| {
        let stage = match progress.stage {
            scan::ScanStage::Walking => "pc_walk",
            scan::ScanStage::Hashing => "pc_files",
        };
        set_progress(
            app,
            stage,
            progress.done,
            progress.total,
            progress.done_bytes,
            progress.total_bytes,
        );
    })
    .map_err(|error| format!("{error:#}"))?;

    set_progress(app, "phone", 0, 0, 0, 0);
    let phone = scan_phone_now(app);
    set_progress(app, "plan", 0, 0, 0, 0);
    let desktop = app.state::<Desktop>();
    let mut ws = desktop
        .workspace
        .lock()
        .map_err(|_| "состояние синхронизации недоступно".to_string())?;

    ws.pc_scan = Some(PcScanView {
        added: report.added,
        changed: report.changed,
        renamed: report.renamed,
        missing: report.missing.len(),
        at: Hlc::wall_ms(),
    });

    match phone {
        Ok(value) => {
            ws.phone_error = None;
            ws.phone_scan = Some(PhoneScanView {
                root: value
                    .get("root")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                count: value.get("count").and_then(Value::as_u64).unwrap_or(0) as usize,
                total: value.get("total").and_then(Value::as_u64).unwrap_or(0),
                at: Hlc::wall_ms(),
            });
        }
        Err(error) => {
            ws.phone_scan = None;
            ws.phone_error = Some(error);
        }
    }

    ws.checked_at = Some(Hlc::wall_ms());
    // «Проверить» пересобирает план по свежим сканам. Старый сеанс
    // держал бы план, построенный до сканирования, поэтому закрываем его.
    ws.close();
    ws.session_error = match ensure_preview(app, &mut ws, &mut replica) {
        Ok(()) => None,
        Err(error) => Some(error),
    };
    let payload = workspace_payload(app, &replica, &ws);
    clear_progress(app);
    Ok(payload)
}

#[tauri::command]
async fn workspace_refresh(app: tauri::AppHandle) -> Result<Value, String> {
    blocking(move || refresh_workspace(&app)).await
}

#[tauri::command]
async fn workspace_status(app: tauri::AppHandle) -> Result<Value, String> {
    blocking(move || {
        let mut replica = workspace_replica()?;
        let desktop = app.state::<Desktop>();
        let mut ws = desktop
            .workspace
            .lock()
            .map_err(|_| "состояние синхронизации недоступно".to_string())?;
        if let Some(session) = ws.session.as_mut() {
            if !session.finished {
                if let Err(error) = session.poll(&mut replica) {
                    ws.close();
                    ws.session_error = Some(format!("{error:#}"));
                }
            }
        }
        Ok(workspace_payload(&app, &replica, &ws))
    })
    .await
}

#[tauri::command]
async fn workspace_decide(
    app: tauri::AppHandle,
    path: String,
    action: String,
) -> Result<Value, String> {
    blocking(move || {
        let decision = parse_decision(&action)?;
        let mut replica = workspace_replica()?;
        let desktop = app.state::<Desktop>();
        let mut ws = desktop
            .workspace
            .lock()
            .map_err(|_| "состояние синхронизации недоступно".to_string())?;
        let Some(session) = ws.session.as_mut() else {
            return Err("Сначала проверьте папки: сеанс ещё не открыт".to_string());
        };
        session
            .record_decision(&mut replica, &path, decision)
            .map_err(|error| format!("{error:#}"))?;
        session
            .poll(&mut replica)
            .map_err(|error| format!("{error:#}"))?;
        Ok(workspace_payload(&app, &replica, &ws))
    })
    .await
}

#[tauri::command]
async fn workspace_decide_many(
    app: tauri::AppHandle,
    paths: Vec<String>,
    action: String,
) -> Result<Value, String> {
    blocking(move || {
        let decision = parse_decision(&action)?;
        let mut replica = workspace_replica()?;
        let desktop = app.state::<Desktop>();
        let mut ws = desktop
            .workspace
            .lock()
            .map_err(|_| "состояние синхронизации недоступно".to_string())?;
        let Some(session) = ws.session.as_mut() else {
            return Err("Сначала проверьте папки: сеанс ещё не открыт".to_string());
        };
        session
            .record_decisions(&mut replica, &paths, decision)
            .map_err(|error| format!("{error:#}"))?;
        session
            .poll(&mut replica)
            .map_err(|error| format!("{error:#}"))?;
        Ok(workspace_payload(&app, &replica, &ws))
    })
    .await
}

#[tauri::command]
async fn workspace_undecide(app: tauri::AppHandle, path: String) -> Result<Value, String> {
    blocking(move || {
        let mut replica = workspace_replica()?;
        let desktop = app.state::<Desktop>();
        let mut ws = desktop
            .workspace
            .lock()
            .map_err(|_| "состояние синхронизации недоступно".to_string())?;
        let Some(session) = ws.session.as_mut() else {
            return Err("Сначала проверьте папки: сеанс ещё не открыт".to_string());
        };
        session
            .forget_decision(&mut replica, &path)
            .map_err(|error| format!("{error:#}"))?;
        session
            .poll(&mut replica)
            .map_err(|error| format!("{error:#}"))?;
        Ok(workspace_payload(&app, &replica, &ws))
    })
    .await
}

/// Ручной режим добавлений. Настройка общая для всех сеансов, поэтому живёт
/// в конфиге, а не в состоянии окна. Возвращаем готовый payload: план не
/// меняется, меняется только список путей, которым нужно решение, поэтому
/// переоткрывать сеанс или сканировать папки не нужно.
#[tauri::command]
async fn workspace_set_manual(app: tauri::AppHandle, enabled: bool) -> Result<Value, String> {
    blocking(move || {
        let mut config = load_config();
        config.manual_additions = enabled;
        save_config(&config)?;
        let replica = workspace_replica()?;
        let desktop = app.state::<Desktop>();
        let ws = desktop
            .workspace
            .lock()
            .map_err(|_| "состояние синхронизации недоступно".to_string())?;
        Ok(workspace_payload(&app, &replica, &ws))
    })
    .await
}

/// Ручной режим изменений. Политика не входит в план: план остаётся набором
/// фактов, а необходимость решения вычисляется при отображении и перед commit.
/// Поэтому переключатель мгновенный — без переоткрытия сеанса.
#[tauri::command]
async fn workspace_set_manual_changes(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<Value, String> {
    blocking(move || {
        let mut config = load_config();
        config.manual_changes = enabled;
        save_config(&config)?;
        let replica = workspace_replica()?;
        let desktop = app.state::<Desktop>();
        let ws = desktop
            .workspace
            .lock()
            .map_err(|_| "состояние синхронизации недоступно".to_string())?;
        Ok(workspace_payload(&app, &replica, &ws))
    })
    .await
}

#[tauri::command]
async fn workspace_commit(app: tauri::AppHandle) -> Result<Value, String> {
    blocking(move || {
        let mut replica = workspace_replica()?;
        let desktop = app.state::<Desktop>();
        let mut ws = desktop
            .workspace
            .lock()
            .map_err(|_| "состояние синхронизации недоступно".to_string())?;
        let Some(session) = ws.session.as_mut() else {
            return Err("Сеанс ещё не открыт".to_string());
        };

        // Тот же предикат, что отдаёт интерфейс. Раскрытая строка в авто-режиме
        // ещё не решение: интерфейс блокирует commit, пока она открыта.
        let config = load_config();
        let pending = pending_paths(&session.plan, &config);
        if !pending.is_empty() {
            let preview = pending
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let more = if pending.len() > 3 { "…" } else { "" };
            return Err(format!(
                "Без решения: {}. Подтвердите изменения в списке: {preview}{more}",
                pending.len()
            ));
        }

        let outcome = match session.commit(&mut replica) {
            Ok(outcome) => outcome,
            Err(error) => {
                let message = format!("{error:#}");
                ws.close();
                ws.session_error = Some(message.clone());
                return Err(message);
            }
        };
        ws.close();
        ws.session_error = None;

        let mut scanned = None;
        if let Ok(Some(root)) = replica.root() {
            if let Ok(report) = scan::scan(&root, &mut replica) {
                scanned = Some(PcScanView {
                    added: report.added,
                    changed: report.changed,
                    renamed: report.renamed,
                    missing: report.missing.len(),
                    at: Hlc::wall_ms(),
                });
            }
        }
        ws.pc_scan = scanned;

        Ok(json!({
            "accepted": outcome.accepted,
            "rejected": outcome.rejected,
            "scan": ws.pc_scan.as_ref(),
        }))
    })
    .await
}

#[tauri::command]
async fn workspace_restore(path: String) -> Result<Value, String> {
    blocking(move || {
        let mut replica = workspace_replica()?;
        vault::restore(&mut replica, &path).map_err(|error| format!("{error:#}"))?;
        let mut report = None;
        if let Ok(Some(root)) = replica.root() {
            report = scan::scan(&root, &mut replica).ok();
        }
        Ok(json!({
            "path": path,
            "added": report.as_ref().map(|report| report.added).unwrap_or(0),
        }))
    })
    .await
}

/// Корзина телефона: треки, удалённые из общей библиотеки, но ещё лежащие
/// на телефоне. Блоб не запрашиваем — только список, чтобы показать его в UI.
#[tauri::command]
async fn workspace_phone_vault(app: tauri::AppHandle) -> Result<Value, String> {
    blocking(move || {
        let rows = with_session(&app, |session| {
            session
                .transport
                .set_read_timeout(Some(Duration::from_secs(30)))
                .map_err(|error| format!("{error:#}"))?;
            agent::send(session.transport.as_mut(), &AgentMsg::GetVault)
                .map_err(|error| format!("{error:#}"))?;
            loop {
                match agent::recv(session.transport.as_mut())
                    .map_err(|error| format!("{error:#}"))?
                {
                    AgentMsg::VaultInfo { rows } => return Ok(Ok(rows)),
                    AgentMsg::Error { message } => return Ok(Err(message)),
                    // В очереди мог остаться ответ на heartbeat `GetRoot`.
                    _ => {}
                }
            }
        })?;
        // На ПК путь мог уже вернуться другим способом: предлагать его к
        // восстановлению незачем, иначе телефон перезапишет живую версию.
        // Исключённые пути тоже скрываем: их операции ПК не применяет.
        let replica = workspace_replica()?;
        let excluded = replica.exclusion_paths().unwrap_or_default();
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|row| !excluded.contains(&row.path))
            .filter(|row| {
                replica
                    .entry(&row.path)
                    .ok()
                    .flatten()
                    .map(|entry| entry.deleted)
                    .unwrap_or(true)
            })
            .collect();
        Ok(json!({ "rows": rows }))
    })
    .await
}

/// Вернуть трек из корзины телефона. Телефон сам достаёт копию и создаёт
/// `entry_put`; ПК затем видит путь как чужое добавление и скачивает блоб
/// обычным сеансом. Поэтому после команды закрываем старый план и строим
/// новый — иначе восстановление не появилось бы в «Изменениях».
#[tauri::command]
async fn workspace_restore_phone_vault(app: tauri::AppHandle, path: String) -> Result<Value, String> {
    blocking(move || {
        with_session(&app, |session| {
            session
                .transport
                .set_read_timeout(Some(Duration::from_secs(120)))
                .map_err(|error| format!("{error:#}"))?;
            agent::send(
                session.transport.as_mut(),
                &AgentMsg::RestoreVault { path: path.clone() },
            )
            .map_err(|error| format!("{error:#}"))?;
            loop {
                match agent::recv(session.transport.as_mut())
                    .map_err(|error| format!("{error:#}"))?
                {
                    AgentMsg::VaultRestored { .. } => return Ok(Ok(())),
                    AgentMsg::Error { message } => return Ok(Err(message)),
                    _ => {}
                }
            }
        })?;

        let mut replica = workspace_replica()?;
        let desktop = app.state::<Desktop>();
        let mut ws = desktop
            .workspace
            .lock()
            .map_err(|_| "состояние синхронизации недоступно".to_string())?;
        ws.close();
        ws.session_error = match ensure_preview(&app, &mut ws, &mut replica) {
            Ok(()) => None,
            Err(error) => Some(error),
        };
        Ok(workspace_payload(&app, &replica, &ws))
    })
    .await
}

/// Попросить телефон почистить корзину. Возвращает, сколько строк и файлов
/// удалено; на ПК это не влияет на план, поэтому сеанс не пересобираем.
fn phone_vault_prune(app: &tauri::AppHandle, message: AgentMsg) -> Result<Value, String> {
    let (rows, blobs) = with_session(app, |session| {
        session
            .transport
            .set_read_timeout(Some(Duration::from_secs(60)))
            .map_err(|error| format!("{error:#}"))?;
        agent::send(session.transport.as_mut(), &message)
            .map_err(|error| format!("{error:#}"))?;
        loop {
            match agent::recv(session.transport.as_mut())
                .map_err(|error| format!("{error:#}"))?
            {
                AgentMsg::VaultPruned {
                    rows_removed,
                    blobs_removed,
                } => return Ok(Ok((rows_removed, blobs_removed))),
                AgentMsg::Error { message } => return Ok(Err(message)),
                // В очереди мог остаться ответ на heartbeat `GetRoot`.
                _ => {}
            }
        }
    })?;
    Ok(json!({ "rows": rows, "blobs": blobs }))
}

/// Удалить из корзины телефона копии старше срока хранения.
#[tauri::command]
async fn workspace_phone_vault_gc(app: tauri::AppHandle, ttl_days: Option<u64>) -> Result<Value, String> {
    blocking(move || {
        phone_vault_prune(
            &app,
            AgentMsg::GcVault {
                ttl_days: ttl_days.unwrap_or(vault::DEFAULT_TTL_DAYS),
            },
        )
    })
    .await
}

/// Полностью очистить корзину телефона. Вернуть треки после этого нельзя.
#[tauri::command]
async fn workspace_phone_vault_clear(app: tauri::AppHandle) -> Result<Value, String> {
    blocking(move || phone_vault_prune(&app, AgentMsg::ClearVault)).await
}

/// Вернуть путь в синхронизацию. Старый план построен без этого пути, поэтому
/// закрываем сеанс и сразу открываем новый: путь возвращается в «Изменения» без
/// ручной проверки. Если телефон недоступен, исключение всё равно снято —
/// ошибка связи показывается в статусе, а план построится при следующей проверке.
#[tauri::command]
async fn workspace_exclusion_undo(app: tauri::AppHandle, path: String) -> Result<Value, String> {
    blocking(move || {
        let mut replica = workspace_replica()?;
        replica
            .remove_exclusion(&path)
            .map_err(|error| format!("{error:#}"))?;
        let desktop = app.state::<Desktop>();
        let mut ws = desktop
            .workspace
            .lock()
            .map_err(|_| "состояние синхронизации недоступно".to_string())?;
        ws.close();
        ws.session_error = match ensure_preview(&app, &mut ws, &mut replica) {
            Ok(()) => None,
            Err(error) => Some(error),
        };
        Ok(workspace_payload(&app, &replica, &ws))
    })
    .await
}

#[tauri::command]
async fn workspace_gc(ttl_days: Option<u64>) -> Result<Value, String> {
    blocking(move || {
        let replica = workspace_replica()?;
        let report =
            vault::gc(&replica, ttl_days.unwrap_or(30)).map_err(|error| format!("{error:#}"))?;
        Ok(json!({
            "rows": report.rows_removed,
            "blobs": report.blobs_removed,
            "orphans": report.orphans_removed,
        }))
    })
    .await
}

#[tauri::command]
async fn workspace_clear_vault() -> Result<Value, String> {
    blocking(move || {
        let replica = workspace_replica()?;
        let report = vault::clear(&replica).map_err(|error| format!("{error:#}"))?;
        Ok(json!({
            "rows": report.rows_removed,
            "blobs": report.blobs_removed,
        }))
    })
    .await
}

/// Полный сброс истории синхронизации. Файлы остаются на месте; на телефоне
/// стирается его журнал, чтобы обе стороны начали с чистого листа.
#[tauri::command]
async fn workspace_clear_history(app: tauri::AppHandle) -> Result<Value, String> {
    blocking(move || {
        // Сеанс держит старый план, поэтому закрываем его до сброса.
        {
            let desktop = app.state::<Desktop>();
            let mut ws = desktop
                .workspace
                .lock()
                .map_err(|_| "состояние синхронизации недоступно".to_string())?;
            ws.close();
        }

        // Проверяем корень до телефона: иначе телефон остался бы без истории,
        // хотя на ПК сбрасывать нечего.
        let mut replica = workspace_replica()?;
        replica
            .root()
            .map_err(|error| format!("{error:#}"))?
            .ok_or_else(|| "Корень библиотеки не задан".to_string())?;

        set_progress(&app, "reset", 0, 0, 0, 0);
        // Сбрасываем телефон первым и без «половинчатого» сброса: если он
        // не ответил, историю ПК не трогаем, иначе стороны разойдутся.
        if let Err(error) = clear_phone_history(&app) {
            clear_progress(&app);
            return Err(error);
        }

        replica
            .reset_history()
            .map_err(|error| format!("{error:#}"))?;
        drop(replica);
        clear_progress(&app);

        // Скан и план строит отдельный шаг «Проверить»: так кнопка очистки
        // не висит в «Очищаем…», пока идёт проверка, а старые цифры уже
        // не показываются. Индекс ПК пересоберётся при следующем скане.
        Ok(json!({ "pc": true, "phone": true }))
    })
    .await
}

#[tauri::command]
fn workspace_progress(app: tauri::AppHandle) -> Value {
    let state = app.state::<Desktop>();
    let payload = match state.progress.lock() {
        Ok(progress) => json!({
            "stage": progress.stage,
            "done": progress.done,
            "total": progress.total,
            "done_bytes": progress.done_bytes,
            "total_bytes": progress.total_bytes,
        }),
        Err(_) => json!({
            "stage": "",
            "done": 0,
            "total": 0,
            "done_bytes": 0,
            "total_bytes": 0,
        }),
    };
    payload
}

#[tauri::command]
fn workspace_close(app: tauri::AppHandle) -> Value {
    let desktop = app.state::<Desktop>();
    if let Ok(mut ws) = desktop.workspace.lock() {
        ws.close();
        ws.session_error = None;
    }
    json!({ "closed": true })
}

// ------------------------------------------------------------------ commands

#[tauri::command]
fn app_info() -> Value {
    let config = load_config();
    let data_dir = effective_data_dir(&config);
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "config_path": config_path().to_string_lossy(),
        "config": config,
        "data_dir": data_dir.to_string_lossy(),
        "device_name": syncth_core::device_name(),
        "app_data_dir": app_data_dir().to_string_lossy(),
        "platform": if cfg!(windows) { "windows" } else if cfg!(target_os = "macos") { "macos" } else { "linux" },
        "sync_addr": SYNC_BIND,
    })
}

#[tauri::command]
async fn initialize_library(root: String, data_dir: Option<String>) -> Result<Value, String> {
    blocking(move || {
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
        .set_title(format!(
            "Куда складывать служебные данные {}",
            syncth_core::APP_NAME
        ))
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

    hidden_command(&mut command);

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("не удалось открыть: {e}"))
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
        .on_window_event(|window, event| {
            if matches!(
                event,
                tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
            ) {
                close_session_now(window.app_handle());
            }
        })
        .setup(|app| {
            start_heartbeat(app.handle().clone());
            // Название окна берём из одного Rust-константы, чтобы смена
            // продукта не требовала правок в разных местах.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title(syncth_core::APP_NAME);
            }
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            discover_agents,
            allow_lan_discovery,
            discover_adb,
            agent_connect,
            agent_connect_adb,
            agent_reconnect,
            agent_status,
            agent_cancel,
            agent_request_root_change,
            agent_scan,
            agent_link,
            agent_disconnect,
            agent_forget,
            workspace_refresh,
            workspace_status,
            workspace_decide,
            workspace_decide_many,
            workspace_undecide,
            workspace_set_manual,
            workspace_set_manual_changes,
            workspace_clear_history,
            workspace_progress,
            workspace_commit,
            workspace_restore,
            workspace_phone_vault,
            workspace_restore_phone_vault,
            workspace_phone_vault_gc,
            workspace_phone_vault_clear,
            workspace_exclusion_undo,
            workspace_gc,
            workspace_clear_vault,
            workspace_close,
            initialize_library,
            change_library,
            pick_folder,
            pick_data_folder,
            probe_folder,
            open_path,
            open_url,
            window_minimize,
            window_toggle_maximize,
            window_close,
        ])
        .run(tauri::generate_context!())
        .expect("не удалось запустить приложение");
}

#[cfg(test)]
mod tests {
    use super::*;
    use syncth_core::plan::{
        Change, Conflict, ConflictReason, DecisionSet, PathDecision, Side,
    };

    fn change(path: &str, kind: ChangeKind) -> Change {
        Change {
            path: path.to_string(),
            from: None,
            kind,
            side: Side::Local,
            blob: format!("blob:{path}"),
            base_blob: String::new(),
            size: 1,
            mtime_ns: 0,
            present: true,
            ignored: false,
            aligned: false,
        }
    }

    fn conflict(path: &str) -> Conflict {
        Conflict {
            path: path.to_string(),
            local_path: path.to_string(),
            remote_path: path.to_string(),
            local_blob: "blob:local".to_string(),
            remote_blob: "blob:remote".to_string(),
            local_size: 1,
            remote_size: 1,
            reason: ConflictReason::BothChanged,
        }
    }

    fn plan(changes: Vec<Change>, conflicts: Vec<Conflict>) -> Plan {
        Plan {
            peer_dev: "peer".to_string(),
            peer_name: "Peer".to_string(),
            created_at: 0,
            expires_at: u64::MAX,
            local: changes,
            remote: Vec::new(),
            conflicts,
            decisions: DecisionSet::default(),
            version: Hlc::new(0, 0),
        }
    }

    fn decide(plan: &mut Plan, path: &str, action: Decision) {
        plan.decisions.entries.insert(
            path.to_string(),
            PathDecision {
                path: path.to_string(),
                action,
                blob: plan.blob_for(path).unwrap_or_default(),
                hlc: Hlc::new(1, 0),
                dev: "dev-pc".to_string(),
            },
        );
    }

    #[test]
    fn auto_policy_asks_only_for_deletions_and_conflicts() {
        let plan = plan(
            vec![
                change("added.flac", ChangeKind::Added),
                change("updated.flac", ChangeKind::Updated),
                change("deleted.flac", ChangeKind::Deleted),
            ],
            vec![conflict("conflict.flac")],
        );
        let pending = pending_paths(&plan, &Config::default());
        assert_eq!(
            pending,
            BTreeSet::from([
                "deleted.flac".to_string(),
                "conflict.flac".to_string()
            ])
        );
    }

    #[test]
    fn manual_flags_ask_for_additions_and_changes() {
        let config = Config {
            manual_additions: true,
            manual_changes: true,
            ..Config::default()
        };
        let mut aligned = change("aligned.flac", ChangeKind::Updated);
        aligned.aligned = true;
        let mut ignored = change("ignored.flac", ChangeKind::Updated);
        ignored.ignored = true;
        let plan = plan(
            vec![
                change("added.flac", ChangeKind::Added),
                change("updated.flac", ChangeKind::Updated),
                change("renamed.flac", ChangeKind::Renamed),
                change("moved.flac", ChangeKind::Moved),
                change("deleted.flac", ChangeKind::Deleted),
                aligned,
                ignored,
            ],
            Vec::new(),
        );

        let pending = pending_paths(&plan, &config);
        assert_eq!(
            pending,
            BTreeSet::from([
                "added.flac".to_string(),
                "updated.flac".to_string(),
                "renamed.flac".to_string(),
                "moved.flac".to_string(),
                "deleted.flac".to_string()
            ])
        );
    }

    #[test]
    fn explicit_decisions_clear_pending_and_unset_restores_it() {
        let mut plan = plan(
            vec![
                change("updated.flac", ChangeKind::Updated),
                change("deleted.flac", ChangeKind::Deleted),
            ],
            vec![conflict("conflict.flac")],
        );
        let config = Config {
            manual_changes: true,
            ..Config::default()
        };
        assert_eq!(pending_paths(&plan, &config).len(), 3);

        decide(&mut plan, "updated.flac", Decision::Revert);
        decide(&mut plan, "deleted.flac", Decision::Keep);
        decide(&mut plan, "conflict.flac", Decision::Accept);
        assert!(pending_paths(&plan, &config).is_empty());

        // Сброс решения — это `Unset`, то есть путь снова ждёт человека.
        decide(&mut plan, "updated.flac", Decision::Unset);
        assert_eq!(
            pending_paths(&plan, &config),
            BTreeSet::from(["updated.flac".to_string()])
        );
    }
}
