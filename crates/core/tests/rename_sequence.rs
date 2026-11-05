//! Реальный сценарий двух последовательных переименований через TCP.
//!
//! Сначала файл переименовывается на ПК, затем — на телефоне. Второе
//! переименование раньше могло уйти пиру как `entry_put`: план строился между
//! транзакциями скана и не видел `entry_move`. На ПК оставались оба имени.
//!
//! Тест проверяет и план (перенос должен остаться `Renamed`), и физические
//! файлы на обеих сторонах.

use std::fs;
use std::net::TcpListener;
use std::path::Path;

use syncth_core::net::{serve_stream, PreviewSession};
use syncth_core::{scan, ChangeKind, Replica};
use tempfile::TempDir;

const OLD: &str = "Album/01. Chu.flac";
const MID: &str = "Album/01. Chu (new).flac";
const NEW: &str = "Album/01. Chu (new) 2.flac";

struct Pair {
    _tmp: TempDir,
    root_a: std::path::PathBuf,
    root_b: std::path::PathBuf,
    a: Replica,
    b: Replica,
    token: String,
}

fn setup() -> Pair {
    let tmp = TempDir::new().unwrap();
    let root_a = tmp.path().join("music_a");
    let root_b = tmp.path().join("music_b");
    fs::create_dir_all(root_a.join("Album")).unwrap();
    fs::create_dir_all(root_b.join("Album")).unwrap();
    fs::write(root_a.join(OLD), b"track-bytes").unwrap();
    fs::write(root_b.join(OLD), b"track-bytes").unwrap();

    let token = "a".repeat(64);
    let mut a = Replica::open(&tmp.path().join("state_a"), "dev-a").unwrap();
    let mut b = Replica::open(&tmp.path().join("state_b"), "dev-b").unwrap();
    a.set_token(&token).unwrap();
    b.set_token(&token).unwrap();
    a.set_root(&root_a).unwrap();
    b.set_root(&root_b).unwrap();
    scan::scan(&root_a, &mut a).unwrap();
    scan::scan(&root_b, &mut b).unwrap();
    Pair {
        _tmp: tmp,
        root_a,
        root_b,
        a,
        b,
        token,
    }
}

/// Клиент `client` подключается к серверу `server`, даёт тесту посмотреть план
/// и выполняет commit. Возвращает обновлённого сервера.
fn sync<F>(client: &mut Replica, server: Replica, token: &str, check: F) -> Replica
where
    F: FnOnce(&mut PreviewSession, &mut Replica),
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server_token = token.to_string();
    let mut server = server;
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let result = serve_stream(stream, &mut server, &server_token);
        (result, server)
    });

    let mut session = PreviewSession::open(client, &addr, token).unwrap();
    check(&mut session, client);
    session.commit(client).unwrap();

    let (result, server) = handle.join().unwrap();
    result.unwrap();
    server
}

fn album_files(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root.join("Album"))
        .unwrap()
        .map(|entry| {
            format!(
                "Album/{}",
                entry.unwrap().file_name().to_string_lossy()
            )
        })
        .collect();
    names.sort();
    names
}

fn assert_single_file(root: &Path) {
    assert_eq!(
        album_files(root),
        vec![NEW.to_string()],
        "после двух переименований должен остаться только последний путь"
    );
}

#[test]
fn pc_then_phone_rename_keeps_a_single_file() {
    let Pair {
        _tmp,
        root_a,
        root_b,
        mut a,
        b,
        token,
    } = setup();

    let b = sync(&mut a, b, &token, |_, _| {});

    // ПК: old -> mid.
    fs::rename(root_a.join(OLD), root_a.join(MID)).unwrap();
    scan::scan(&root_a, &mut a).unwrap();
    let mut b = sync(&mut a, b, &token, |session, _| {
        let change = session
            .plan
            .local
            .iter()
            .find(|change| change.path == MID)
            .expect("первое переименование должно быть в плане");
        assert_eq!(change.kind, ChangeKind::Renamed);
        assert_eq!(change.from.as_deref(), Some(OLD));
    });

    // Телефон: mid -> new. Это то самое второе переименование.
    fs::rename(root_b.join(MID), root_b.join(NEW)).unwrap();
    scan::scan(&root_b, &mut b).unwrap();
    let a = sync(&mut b, a, &token, |session, _| {
        let change = session
            .plan
            .local
            .iter()
            .find(|change| change.path == NEW)
            .expect("второе переименование должно быть в плане");
        assert_eq!(change.kind, ChangeKind::Renamed);
        assert_eq!(change.from.as_deref(), Some(MID));
    });

    assert_single_file(&root_a);
    assert_single_file(&root_b);

    let live_a: Vec<String> = a
        .live_entries()
        .unwrap()
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    assert_eq!(live_a, vec![NEW.to_string()]);
}

#[test]
fn two_quick_phone_renames_leave_no_duplicates() {
    let Pair {
        _tmp,
        root_a,
        root_b,
        mut a,
        b,
        token,
    } = setup();

    let mut b = sync(&mut a, b, &token, |_, _| {});

    // Телефон переименовывает дважды подряд, синхронизация — после каждого.
    fs::rename(root_b.join(OLD), root_b.join(MID)).unwrap();
    scan::scan(&root_b, &mut b).unwrap();
    let a = sync(&mut b, a, &token, |_, _| {
        // PC получает перенос как удалённое/добавленное со стороны пира,
        // и это должно быть одно изменение, а не два независимых файла.
    });

    fs::rename(root_b.join(MID), root_b.join(NEW)).unwrap();
    scan::scan(&root_b, &mut b).unwrap();
    let _a = sync(&mut b, a, &token, |session, _| {
        let change = session
            .plan
            .local
            .iter()
            .find(|change| change.path == NEW)
            .expect("второе переименование должно быть в плане");
        assert_eq!(change.kind, ChangeKind::Renamed);
        assert_eq!(change.from.as_deref(), Some(MID));
    });

    assert_single_file(&root_a);
    assert_single_file(&root_b);
}
