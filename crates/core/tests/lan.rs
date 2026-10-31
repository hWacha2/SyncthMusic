//! LAN-транспорт: реальные сокеты, реальные байты, без моков.

use std::fs;
use std::path::{Path, PathBuf};
use syncth_core::{net, scan, Replica};
use tempfile::TempDir;

struct Ctx {
    _tmp: TempDir,
    root_a: PathBuf,
    root_b: PathBuf,
    data_a: PathBuf,
    data_b: PathBuf,
    token: String,
}

fn setup() -> Ctx {
    let tmp = TempDir::new().unwrap();
    let ctx = Ctx {
        root_a: tmp.path().join("music_a"),
        root_b: tmp.path().join("music_b"),
        data_a: tmp.path().join("data_a"),
        data_b: tmp.path().join("data_b"),
        token: String::new(),
        _tmp: tmp,
    };
    fs::create_dir_all(ctx.root_a.join("Album")).unwrap();
    fs::create_dir_all(&ctx.root_b).unwrap();
    fs::write(ctx.root_a.join("Album/01.flac"), b"alpha-bytes").unwrap();
    fs::write(ctx.root_a.join("Album/02.flac"), b"beta-bytes").unwrap();

    let mut a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    a.set_root(&ctx.root_a).unwrap();
    scan::scan(&ctx.root_a, &mut a).unwrap();
    let b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    b.set_root(&ctx.root_b).unwrap();
    let token = a.ensure_token().unwrap();
    Ctx { token, ..ctx }
}

/// Один сеанс: A слушает, B подключается (затем оба реплики переоткрываются).
fn session(ctx: &Ctx) -> net::SyncStats {
    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    let handle = net::spawn_one(a, "127.0.0.1:0", ctx.token.clone()).unwrap();
    let stats = net::sync_with_peer(&mut b, &handle.addr, &ctx.token).unwrap();
    handle.finish().unwrap();
    stats
}

fn state(data: &Path) -> String {
    Replica::open(data, "probe").unwrap().state_hash().unwrap()
}

#[test]
fn transfers_manifest_and_blobs_over_loopback() {
    let ctx = setup();
    let stats = session(&ctx);

    assert_eq!(stats.received_blobs, 2, "{}", stats.summary());
    assert_eq!(
        fs::read(ctx.root_b.join("Album/01.flac")).unwrap(),
        b"alpha-bytes"
    );
    assert_eq!(
        fs::read(ctx.root_b.join("Album/02.flac")).unwrap(),
        b"beta-bytes"
    );
    assert_eq!(state(&ctx.data_a), state(&ctx.data_b));

    // mtime восстановлен — иначе сортировка по дате на новом устройстве врёт.
    let ma = fs::metadata(ctx.root_a.join("Album/01.flac")).unwrap();
    let mb = fs::metadata(ctx.root_b.join("Album/01.flac")).unwrap();
    let diff = ma
        .modified()
        .unwrap()
        .duration_since(mb.modified().unwrap())
        .unwrap_or_default();
    assert!(diff.as_millis() < 2, "mtime разъехался на {diff:?}");

    // Повторный сеанс не гоняет байты и не плодит операции.
    let again = session(&ctx);
    assert_eq!(again.received_blobs, 0);
    assert_eq!(again.sent_blobs, 0);
    assert_eq!(again.applied_remote_ops, 0, "{}", again.summary());
    // Дельта-синк: журнал уже синхронизирован, значит в сокет не ушло ни одной
    // операции — даже если в логе их тысячи.
    assert_eq!(again.sent_remote_ops, 0, "{}", again.summary());
    assert_eq!(again.received_remote_ops, 0, "{}", again.summary());
}

#[test]
fn conflicting_edits_never_overwrite_each_other() {
    let ctx = setup();
    session(&ctx);

    let rel = "Album/01.flac";
    fs::write(ctx.root_a.join(rel), b"edited-on-pc").unwrap();
    fs::write(ctx.root_b.join(rel), b"edited-on-phone").unwrap();

    let mut a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    scan::scan(&ctx.root_a, &mut a).unwrap();
    drop(a);
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    scan::scan(&ctx.root_b, &mut b).unwrap();
    drop(b);

    // Один сеанс определяет победителя и сохраняет проигравшего рядом.
    session(&ctx);
    // Ещё сеансы развозят conflict-copy на обе стороны.
    session(&ctx);
    session(&ctx);

    for root in [&ctx.root_a, &ctx.root_b] {
        let mut contents = Vec::new();
        for e in walkdir::WalkDir::new(root) {
            let e = e.unwrap();
            if e.file_type().is_file() {
                contents.push(fs::read(e.path()).unwrap());
            }
        }
        assert!(
            contents.iter().any(|c| c == b"edited-on-pc"),
            "потеряна правка ПК в {}",
            root.display()
        );
        assert!(
            contents.iter().any(|c| c == b"edited-on-phone"),
            "потеряна правка телефона в {}",
            root.display()
        );
        assert_eq!(
            contents
                .iter()
                .filter(|c| **c == b"edited-on-phone")
                .count(),
            1,
            "дубликаты правки телефона в {}",
            root.display()
        );
    }

    assert_eq!(state(&ctx.data_a), state(&ctx.data_b));
}

#[test]
fn sequential_edits_are_updates_not_conflicts() {
    let ctx = setup();
    session(&ctx); // база согласована

    // Правка тегов на ПК, обмен, потом правка на телефоне.
    fs::write(ctx.root_a.join("Album/01.flac"), b"pc-tag-edit").unwrap();
    let mut a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    scan::scan(&ctx.root_a, &mut a).unwrap();
    drop(a);
    session(&ctx);

    fs::write(ctx.root_b.join("Album/01.flac"), b"phone-tag-edit").unwrap();
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    scan::scan(&ctx.root_b, &mut b).unwrap();
    drop(b);
    session(&ctx);

    for root in [&ctx.root_a, &ctx.root_b] {
        let conflicts: Vec<_> = walkdir::WalkDir::new(root)
            .into_iter()
            .map(|e| e.unwrap())
            .filter(|e| e.file_name().to_string_lossy().contains("sync-conflict"))
            .collect();
        assert!(
            conflicts.is_empty(),
            "обычная правка не должна плодить копии: {conflicts:?}"
        );
        assert_eq!(
            fs::read(root.join("Album/01.flac")).unwrap(),
            b"phone-tag-edit"
        );
    }
    assert_eq!(state(&ctx.data_a), state(&ctx.data_b));
}

#[test]
fn first_contact_with_same_path_keeps_both_versions() {
    let ctx = setup();
    // До первого обмена на B уже есть свой файл по тому же пути.
    fs::create_dir_all(ctx.root_b.join("Album")).unwrap();
    fs::write(ctx.root_b.join("Album/01.flac"), b"b-side-original").unwrap();
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    scan::scan(&ctx.root_b, &mut b).unwrap();
    drop(b);

    session(&ctx);
    session(&ctx);

    for root in [&ctx.root_a, &ctx.root_b] {
        let mut contents = Vec::new();
        for e in walkdir::WalkDir::new(root) {
            let e = e.unwrap();
            if e.file_type().is_file() {
                contents.push(fs::read(e.path()).unwrap());
            }
        }
        assert!(
            contents.iter().any(|c| c == b"alpha-bytes"),
            "{}",
            root.display()
        );
        assert!(
            contents.iter().any(|c| c == b"b-side-original"),
            "{}",
            root.display()
        );
    }
}

#[test]
fn wrong_token_is_rejected() {
    let ctx = setup();
    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let handle = net::spawn_one(a, "127.0.0.1:0", "correct-token".into()).unwrap();
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    let err = net::sync_with_peer(&mut b, &handle.addr, "wrong-token")
        .unwrap_err()
        .to_string();
    assert!(err.contains("токен"), "{err}");
    assert!(handle.finish().is_err());
    assert!(b.live_entries().unwrap().is_empty());
}

#[test]
fn deleted_track_disappears_on_the_peer_after_resolution() {
    let ctx = setup();
    session(&ctx);

    // Телефон удалил трек и подтвердил «удалить везде».
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    fs::remove_file(ctx.root_b.join("Album/02.flac")).unwrap();
    scan::scan(&ctx.root_b, &mut b).unwrap();
    syncth_core::resolve::resolve(
        &mut b,
        "Album/02.flac",
        syncth_core::Action::DeleteEverywhere,
    )
    .unwrap();
    drop(b);

    session(&ctx);

    assert!(
        !ctx.root_a.join("Album/02.flac").exists(),
        "ПК должен удалить трек"
    );
    assert!(ctx.root_a.join("Album/01.flac").is_file());
    assert_eq!(state(&ctx.data_a), state(&ctx.data_b));
}
