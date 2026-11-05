//! Регрессия на гонку «скан — план/commit».
//!
//! Раньше план мог быть построен между транзакциями скана: перенос уже лежал
//! в `entries`, но ещё не в `ops`. Новый путь классифицировался как
//! добавление, commit отправлял `entry_put`, watermark пира уходил вперёд — и
//! на ПК навсегда оставались оба имени.
//!
//! Здесь проверяются две вещи:
//! 1. два одновременных скана одного переименования дают ровно один `entry_move`
//!    и ни одного `entry_put` на новый путь;
//! 2. план, запрошенный во время скана, не строится на полусостоянии, а ждёт
//!    завершения скана и видит переименование.

use std::fs;
use std::net::TcpListener;
use std::sync::{mpsc, Barrier};
use std::time::Duration;

use syncth_core::net::{serve_stream, PreviewSession};
use syncth_core::{scan, ChangeKind, OpKind, Replica};
use tempfile::TempDir;

const OLD: &str = "Album/01. Chu♡Chu♡Chu (new).flac";
const NEW: &str = "Album/01. Chu♡Chu♡Chu (new) 2.flac";

fn write_track(root: &std::path::Path, rel: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"same track bytes").unwrap();
}

fn rename_track(root: &std::path::Path) {
    fs::rename(root.join(OLD), root.join(NEW)).unwrap();
}

#[test]
fn two_scans_of_one_rename_emit_one_move() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("music");
    let state = tmp.path().join("state");
    write_track(&root, OLD);

    let mut first = Replica::open(&state, "dev-phone").unwrap();
    first.set_root(&root).unwrap();
    scan::scan(&root, &mut first).unwrap();

    rename_track(&root);

    // Второе соединение — как у агентского скана и sync_now на телефоне.
    let mut second = Replica::open(&state, "dev-phone").unwrap();
    let barrier = Barrier::new(2);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            scan::scan(&root, &mut first).unwrap();
        });
        let b = scope.spawn(|| {
            barrier.wait();
            scan::scan(&root, &mut second).unwrap();
        });
        a.join().unwrap();
        b.join().unwrap();
    });

    let replica = Replica::open(&state, "dev-phone").unwrap();
    let mut moves = 0;
    let mut puts_to_new = 0;
    for op in replica.ops().unwrap() {
        match &op.kind {
            OpKind::EntryMove { from, to, .. } if from == OLD && to == NEW => moves += 1,
            OpKind::EntryPut { path, .. } if path == NEW => puts_to_new += 1,
            _ => {}
        }
    }
    assert_eq!(moves, 1, "переименование должно записаться один раз");
    assert_eq!(
        puts_to_new, 0,
        "новый путь нельзя записывать как добавление при действующем переносе"
    );

    let plan = syncth_core::plan::build(&replica, "dev-pc", "PC", &[]).unwrap();
    let change = plan
        .local
        .iter()
        .find(|change| change.path == NEW)
        .expect("новый путь должен быть в плане");
    assert_eq!(change.kind, ChangeKind::Renamed);
    assert_eq!(change.from.as_deref(), Some(OLD));
}

struct Pair {
    _tmp: TempDir,
    root_a: std::path::PathBuf,
    root_b: std::path::PathBuf,
    a: Replica,
    b: Replica,
    token: String,
}

fn setup_pair() -> Pair {
    let tmp = TempDir::new().unwrap();
    let root_a = tmp.path().join("music_a");
    let root_b = tmp.path().join("music_b");
    write_track(&root_a, OLD);
    write_track(&root_b, OLD);

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

/// Начальная синхронизация: обе стороны фиксируют общую базу.
fn commit_initial(a: &mut Replica, b: Replica, token: &str) -> Replica {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server_token = token.to_string();
    let mut b = b;
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let result = serve_stream(stream, &mut b, &server_token);
        (result, b)
    });

    let mut session = PreviewSession::open(a, &addr, token).unwrap();
    session.commit(a).unwrap();

    let (result, b) = handle.join().unwrap();
    result.unwrap();
    b
}

#[test]
fn plan_waits_for_running_scan_and_sees_the_rename() {
    let Pair {
        _tmp,
        root_a,
        root_b,
        mut a,
        b,
        token,
    } = setup_pair();
    let b = commit_initial(&mut a, b, &token);

    rename_track(&root_a);

    // Скан отдельным соединением, как агентский скан на телефоне.
    let mut scanner = Replica::open(&_tmp.path().join("state_a"), "dev-a").unwrap();

    // Пауза внутри скана: он уже взял блокировку, но ещё не прочитал БД.
    // Именно в этот момент раньше мог строиться план и не видеть перенос.
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let scan_root = root_a.clone();
    let scan_handle = std::thread::spawn(move || {
        let mut paused = false;
        let report = scan::scan_with_progress(&scan_root, &mut scanner, &mut |progress| {
            if !paused
                && progress.stage == scan::ScanStage::Hashing
                && progress.total > 0
            {
                paused = true;
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
        })
        .unwrap();
        (report, scanner)
    });
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("скан должен дойти до этапа хеширования");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server_token = token.clone();
    let server_handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let result = serve_stream(stream, &mut a, &server_token);
        (result, a)
    });

    let client_token = token.clone();
    let (plan_tx, plan_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let mut b = b;
    let plan_handle = std::thread::spawn(move || {
        let mut session = PreviewSession::open(&mut b, &addr, &client_token).unwrap();
        plan_tx.send(session.plan.clone()).unwrap();
        go_rx.recv().unwrap();
        session.commit(&mut b).unwrap();
        b
    });

    // Даём клиенту дойти до построения плана. Под блокировкой он обязан
    // ждать, поэтому план появится только после release.
    std::thread::sleep(Duration::from_millis(200));
    release_tx.send(()).unwrap();

    let plan = plan_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("план должен построиться после завершения скана");
    // Клиент видит переименование со стороны сервера как remote; главное —
    // что оно вообще попало в план и осталось переносом.
    let change = plan
        .local
        .iter()
        .chain(plan.remote.iter())
        .find(|change| change.path == NEW)
        .expect("переименование должно попасть в план, а не потеряться");
    assert_eq!(change.kind, ChangeKind::Renamed);
    assert_eq!(change.from.as_deref(), Some(OLD));

    go_tx.send(()).unwrap();
    let _b = plan_handle.join().unwrap();
    let (server_result, _a) = server_handle.join().unwrap();
    server_result.unwrap();
    let (report, _scanner) = scan_handle.join().unwrap();
    assert_eq!(report.renamed, 1);

    for root in [&root_a, &root_b] {
        let names: Vec<String> = fs::read_dir(root.join("Album"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            vec!["01. Chu♡Chu♡Chu (new) 2.flac".to_string()],
            "после commit на обеих сторонах должен остаться только новый путь"
        );
    }
}
