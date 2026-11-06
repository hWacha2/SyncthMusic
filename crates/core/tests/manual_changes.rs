//! Ручной режим изменений: правки, переименования и переносы ждут решения.
//!
//! Политика задаётся инициатором сеанса и уезжает серверу в `Preview`, чтобы
//! обе стороны строили одинаковый план. Проверяем не только решения, но и то,
//! что до commit пути действительно не применяются, а после «Отклонить»
//! восстанавливается исходная версия.

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use syncth_core::net::{serve_stream, PreviewSession, SyncStats};
use syncth_core::plan::{ChangeKind, Decision};
use syncth_core::{commit, scan, Hlc, Replica};
use tempfile::TempDir;

const SONG: &str = "Album/01. Song.flac";
const RENAMED: &str = "Album/01. Song (new).flac";
const MOVED: &str = "Other/01. Song.flac";

struct Pair {
    _tmp: TempDir,
    root_a: PathBuf,
    root_b: PathBuf,
    data_a: PathBuf,
    data_b: PathBuf,
    token: String,
}

fn setup() -> Pair {
    let tmp = TempDir::new().unwrap();
    let root_a = tmp.path().join("pc");
    let root_b = tmp.path().join("phone");
    fs::create_dir_all(root_a.join("Album")).unwrap();
    fs::create_dir_all(&root_b).unwrap();
    fs::write(root_a.join(SONG), b"base-bytes").unwrap();

    let token = "m".repeat(64);
    let data_a = tmp.path().join("data_a");
    let data_b = tmp.path().join("data_b");
    let mut a = Replica::open(&data_a, "dev-pc").unwrap();
    let mut b = Replica::open(&data_b, "dev-phone").unwrap();
    a.set_token(&token).unwrap();
    b.set_token(&token).unwrap();
    a.set_root(&root_a).unwrap();
    b.set_root(&root_b).unwrap();
    scan::scan(&root_a, &mut a).unwrap();
    scan::scan(&root_b, &mut b).unwrap();

    // Первый сеанс записывает общую базу: трек с ПК появляется на телефоне.
    let (addr, handle) = spawn_server(a, token.clone());
    let mut session = PreviewSession::open(&mut b, &addr, &token).unwrap();
    session.commit(&mut b).unwrap();
    handle.join().unwrap().0.unwrap();

    assert_eq!(fs::read(root_b.join(SONG)).unwrap(), b"base-bytes");
    Pair {
        _tmp: tmp,
        root_a,
        root_b,
        data_a,
        data_b,
        token,
    }
}

fn open_a(pair: &Pair) -> Replica {
    Replica::open(&pair.data_a, "dev-pc").unwrap()
}

fn open_b(pair: &Pair) -> Replica {
    Replica::open(&pair.data_b, "dev-phone").unwrap()
}

fn spawn_server(
    server: Replica,
    token: String,
) -> (
    String,
    std::thread::JoinHandle<(Result<SyncStats, String>, Replica)>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut server = server;
        let result = serve_stream(stream, &mut server, &token).map_err(|error| format!("{error:#}"));
        (result, server)
    });
    (addr, handle)
}

/// Открыть сеанс, дать тесту посмотреть план и выполнить commit.
fn commit_session<F>(pair: &Pair, mut client: Replica, manual: bool, check: F) -> Replica
where
    F: FnOnce(&mut PreviewSession, &mut Replica),
{
    let server = open_a(pair);
    let (addr, handle) = spawn_server(server, pair.token.clone());
    let mut session = PreviewSession::open_with_policy(&mut client, &addr, &pair.token, manual)
        .expect("сеанс должен открыться");
    check(&mut session, &mut client);
    session.commit(&mut client).unwrap();
    let (result, server) = handle.join().unwrap();
    result.unwrap();
    server
}

fn album_files(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for folder in ["Album", "Other"] {
        let dir = root.join(folder);
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries {
            names.push(format!("{folder}/{}", entry.unwrap().file_name().to_string_lossy()));
        }
    }
    names.sort();
    names
}

fn state_hash(replica: &Replica) -> String {
    replica.state_hash().unwrap()
}

#[test]
fn default_mode_accepts_an_update_without_questions() {
    let pair = setup();
    fs::write(pair.root_a.join(SONG), b"changed-bytes").unwrap();
    let mut a = open_a(&pair);
    scan::scan(&pair.root_a, &mut a).unwrap();
    drop(a);

    let b = open_b(&pair);
    let server = commit_session(&pair, b, false, |session, _| {
        assert!(!session.plan.manual_changes);
        let change = session
            .plan
            .remote
            .iter()
            .find(|change| change.path == SONG)
            .expect("правка должна быть в плане");
        assert_eq!(change.kind, ChangeKind::Updated);
        assert_eq!(
            session.plan.effective_decision(SONG),
            Some(Decision::Accept)
        );
        assert!(session.plan.unresolved().is_empty());
    });
    drop(server);

    assert_eq!(
        fs::read(pair.root_b.join(SONG)).unwrap(),
        b"changed-bytes"
    );
    assert_eq!(state_hash(&open_a(&pair)), state_hash(&open_b(&pair)));
}

#[test]
fn manual_mode_blocks_commit_until_the_change_is_accepted() {
    let pair = setup();
    fs::write(pair.root_a.join(SONG), b"changed-bytes").unwrap();
    let mut a = open_a(&pair);
    scan::scan(&pair.root_a, &mut a).unwrap();
    drop(a);

    let mut b = open_b(&pair);
    let server = open_a(&pair);
    let (addr, handle) = spawn_server(server, pair.token.clone());
    let mut session = PreviewSession::open_with_policy(&mut b, &addr, &pair.token, true).unwrap();

    assert!(session.plan.manual_changes);
    let change = session
        .plan
        .remote
        .iter()
        .find(|change| change.path == SONG)
        .expect("правка должна быть в плане");
    assert_eq!(change.kind, ChangeKind::Updated);
    assert_eq!(session.plan.effective_decision(SONG), None);
    assert!(session.plan.unresolved().contains(&SONG.to_string()));

    let blocked = commit::validate(&b, &session.plan).unwrap_err().to_string();
    assert!(blocked.contains("требуют решения"), "{blocked}");

    // Сервер получил ту же политику и тоже видит путь нерешённым.
    let server_plan = open_a(&pair)
        .load_plan("dev-phone", Hlc::wall_ms())
        .unwrap()
        .expect("сервер сохраняет план");
    assert!(server_plan.manual_changes);
    assert!(server_plan.unresolved().contains(&SONG.to_string()));
    assert_eq!(server_plan.effective_decision(SONG), None);

    session
        .record_decision(&mut b, SONG, Decision::Accept)
        .unwrap();
    session.poll(&mut b).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    session.poll(&mut b).unwrap();
    session.commit(&mut b).unwrap();

    let (result, _server) = handle.join().unwrap();
    result.unwrap();

    assert!(!session.plan.unresolved().contains(&SONG.to_string()));
    assert_eq!(
        fs::read(pair.root_b.join(SONG)).unwrap(),
        b"changed-bytes"
    );
    assert_eq!(state_hash(&open_a(&pair)), state_hash(&open_b(&pair)));
}

#[test]
fn rejecting_a_local_rename_restores_the_old_name() {
    let pair = setup();
    fs::rename(pair.root_b.join(SONG), pair.root_b.join(RENAMED)).unwrap();
    let mut b = open_b(&pair);
    scan::scan(&pair.root_b, &mut b).unwrap();

    let server = commit_session(&pair, b, true, |session, client| {
        let change = session
            .plan
            .local
            .iter()
            .find(|change| change.path == RENAMED)
            .expect("переименование должно быть в плане");
        assert_eq!(change.kind, ChangeKind::Renamed);
        assert_eq!(change.from.as_deref(), Some(SONG));
        assert_eq!(session.plan.effective_decision(RENAMED), None);
        assert!(session.plan.unresolved().contains(&RENAMED.to_string()));

        session
            .record_decision(client, RENAMED, Decision::Revert)
            .unwrap();
        session.poll(client).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        session.poll(client).unwrap();
    });
    drop(server);

    assert_eq!(album_files(&pair.root_a), vec![SONG.to_string()]);
    assert_eq!(album_files(&pair.root_b), vec![SONG.to_string()]);
    assert_eq!(fs::read(pair.root_b.join(SONG)).unwrap(), b"base-bytes");
    assert_eq!(state_hash(&open_a(&pair)), state_hash(&open_b(&pair)));
}

#[test]
fn rejecting_a_remote_update_keeps_the_base_version_on_both_sides() {
    let pair = setup();
    fs::write(pair.root_a.join(SONG), b"changed-bytes").unwrap();
    let mut a = open_a(&pair);
    scan::scan(&pair.root_a, &mut a).unwrap();
    drop(a);

    let server = commit_session(&pair, open_b(&pair), true, |session, client| {
        let change = session
            .plan
            .remote
            .iter()
            .find(|change| change.path == SONG)
            .expect("правка с ПК должна быть в плане");
        assert_eq!(change.kind, ChangeKind::Updated);
        assert_eq!(session.plan.effective_decision(SONG), None);

        session
            .record_decision(client, SONG, Decision::Revert)
            .unwrap();
        session.poll(client).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        session.poll(client).unwrap();
    });
    drop(server);

    assert_eq!(fs::read(pair.root_a.join(SONG)).unwrap(), b"base-bytes");
    assert_eq!(fs::read(pair.root_b.join(SONG)).unwrap(), b"base-bytes");
    assert_eq!(state_hash(&open_a(&pair)), state_hash(&open_b(&pair)));
}

#[test]
fn accepting_a_remote_move_moves_the_file_on_the_other_side() {
    let pair = setup();
    fs::create_dir_all(pair.root_a.join("Other")).unwrap();
    fs::rename(pair.root_a.join(SONG), pair.root_a.join(MOVED)).unwrap();
    let mut a = open_a(&pair);
    scan::scan(&pair.root_a, &mut a).unwrap();
    drop(a);

    let server = commit_session(&pair, open_b(&pair), true, |session, client| {
        let change = session
            .plan
            .remote
            .iter()
            .find(|change| change.path == MOVED)
            .expect("перенос с ПК должен быть в плане");
        assert_eq!(change.kind, ChangeKind::Moved);
        assert_eq!(change.from.as_deref(), Some(SONG));
        assert_eq!(session.plan.effective_decision(MOVED), None);

        session
            .record_decision(client, MOVED, Decision::Accept)
            .unwrap();
        session.poll(client).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        session.poll(client).unwrap();
    });
    drop(server);

    assert_eq!(album_files(&pair.root_a), vec![MOVED.to_string()]);
    assert_eq!(album_files(&pair.root_b), vec![MOVED.to_string()]);
    assert_eq!(fs::read(pair.root_b.join(MOVED)).unwrap(), b"base-bytes");
    assert_eq!(state_hash(&open_a(&pair)), state_hash(&open_b(&pair)));
}
