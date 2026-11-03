//! Входящие сеансы на телефоне.
//!
//! Пока приложение открыто, оно слушает тот же порт, что и демон на ПК.
//! Второе устройство может подключиться первым и провести preview-сессию, а
//! затем commit: телефон примет операции и файлы так же, как демон.
//!
//! У каждого соединения своя реплика (своё SQLite-соединение): UI продолжает
//! читать статус, пока сеанс идёт. Слушатель живёт до закрытия приложения —
//! foreground service появится позже, вместе с работой в фоне.

use serde_json::{json, Value};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::Duration;
use syncth_core::{net, Hlc};

const BIND: &str = "0.0.0.0:47887";

/// Снимок живого сеанса для статуса в UI.
#[derive(Debug, Clone)]
struct SessionView {
    dev: String,
    name: String,
    addr: String,
    since: u64,
    paths: usize,
    unresolved: usize,
    read_only: bool,
    diverged: bool,
}

#[derive(Default)]
struct State {
    session: Option<SessionView>,
    commit: Option<mpsc::Sender<Result<(usize, usize), String>>>,
    disconnect: bool,
    error: Option<String>,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}

/// Запустить слушатель один раз на всё время жизни процесса.
pub fn ensure_started(app: tauri::AppHandle) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || listen(app));
}

fn listen(app: tauri::AppHandle) {
    loop {
        let listener = match TcpListener::bind(BIND) {
            Ok(listener) => listener,
            Err(e) => {
                state().lock().unwrap().error = Some(format!("порт {BIND}: {e}"));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        state().lock().unwrap().error = None;

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => handle(stream, &app),
                Err(e) => {
                    state().lock().unwrap().error = Some(format!("приём соединения: {e}"));
                }
            }
        }
    }
}

fn handle(stream: std::net::TcpStream, app: &tauri::AppHandle) {
    // Новое соединение означает, что слушатель жив. Ошибка прошлого сеанса
    // не должна навсегда блокировать следующие попытки.
    state().lock().unwrap().error = None;
    let mut replica = match crate::open(app) {
        Ok(replica) => replica,
        Err(e) => {
            state().lock().unwrap().error = Some(e);
            return;
        }
    };
    let token = match replica.ensure_token() {
        Ok(token) => token,
        Err(e) => {
            state().lock().unwrap().error = Some(format!("{e:#}"));
            return;
        }
    };
    let addr = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let since = Hlc::wall_ms();
    let mut committed = false;
    let mut failure: Option<String> = None;

    let result = net::serve_stream_with(stream, &mut replica, &token, &mut |session, replica| {
        let view = SessionView {
            dev: session.peer_dev.clone(),
            name: session.peer_name.clone(),
            addr: addr.clone(),
            since,
            paths: session.paths(),
            unresolved: session.unresolved(),
            read_only: session.read_only,
            diverged: session.diverged,
        };
        let (commit, disconnect) = {
            let mut st = state().lock().unwrap();
            st.session = Some(view);
            (st.commit.take(), std::mem::take(&mut st.disconnect))
        };

        if disconnect {
            session.close();
        }

        if let Some(reply) = commit {
            match session.commit(replica) {
                Ok(outcome) => {
                    committed = true;
                    let _ = reply.send(Ok((outcome.accepted, outcome.rejected)));
                }
                Err(e) => {
                    failure = Some(format!("{e:#}"));
                    let _ = reply.send(Err(format!("{e:#}")));
                }
            }
        }

        if session.finished {
            committed = true;
        }
    });

    match result {
        Ok(_) => {
            if !committed {
                // Сеанс мог закрыться до commit — это не ошибка.
                let mut st = state().lock().unwrap();
                st.error = None;
            }
        }
        Err(e) => {
            let message = format!("{e:#}");
            // Закрытый сокет — штатное завершение сеанса, а не ошибка.
            let benign =
                message.contains("пир закрыл сеанс") || message.contains("соединение закрыто");
            if !benign && failure.as_deref() != Some(message.as_str()) {
                state().lock().unwrap().error = Some(message);
            }
        }
    }

    state().lock().unwrap().session = None;

    if committed {
        if let Ok(Some(root)) = replica.root() {
            let _ = crate::media::scan(&root);
        }
    }
}

/// Статус входящего сеанса для `status_json`.
pub fn session_json() -> Option<Value> {
    let st = state().lock().unwrap();
    st.session.as_ref().map(|s| {
        json!({
            "connected": true,
            "incoming": true,
            "dev": s.dev,
            "name": s.name,
            "addr": s.addr,
            "since": s.since,
            "paths": s.paths,
            "unresolved": s.unresolved,
            "read_only": s.read_only,
            "diverged": s.diverged,
        })
    })
}

pub fn has_session() -> bool {
    state().lock().unwrap().session.is_some()
}

/// Слушатель и последняя ошибка — для диагностики в UI.
pub fn info_json() -> Value {
    let st = state().lock().unwrap();
    json!({
        "port": net::SYNC_PORT,
        "error": st.error,
    })
}

/// Попросить поток входящего сеанса выполнить commit. `None` — сеанса нет.
pub fn request_commit() -> Option<Result<(usize, usize), String>> {
    let (tx, rx) = mpsc::channel();
    {
        let mut st = state().lock().unwrap();
        if st.session.is_none() {
            return None;
        }
        st.commit = Some(tx);
    }
    Some(
        rx.recv_timeout(Duration::from_secs(180))
            .unwrap_or_else(|_| Err("Передача не завершилась за 3 минуты".into())),
    )
}

/// Закрыть входящий сеанс, не забывая сопряжение.
pub fn request_disconnect() -> bool {
    let mut st = state().lock().unwrap();
    if st.session.is_none() {
        return false;
    }
    st.session = None;
    st.disconnect = true;
    true
}
