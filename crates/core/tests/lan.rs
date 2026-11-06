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
fn preview_builds_plan_without_applying_ops_and_shares_decisions() {
    use syncth_core::plan::Decision;

    let ctx = setup();
    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    let handle = net::spawn_one(a, "127.0.0.1:0", ctx.token.clone()).unwrap();

    let mut session = net::PreviewSession::open(&mut b, &handle.addr, &ctx.token).unwrap();
    assert!(session.paths() >= 2, "план должен содержать оба трека");
    assert_eq!(
        b.live_entries().unwrap().len(),
        0,
        "preview не применяет операции"
    );

    let path = session
        .plan
        .remote
        .first()
        .expect("у пира есть добавленные пути")
        .path
        .clone();
    session
        .record_decision(&mut b, &path, Decision::Accept)
        .unwrap();
    session.poll(&mut b).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    session.close();
    drop(session);
    let _ = handle.finish();

    // Решение доехало до сервера, но операции так и не применились.
    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let plan = a
        .load_plan("dev-b", syncth_core::Hlc::wall_ms())
        .unwrap()
        .expect("план сохранён на сервере");
    assert_eq!(
        plan.decisions.get(&path).map(|d| d.action),
        Some(Decision::Accept)
    );
    assert_eq!(
        b.live_entries().unwrap().len(),
        0,
        "решение не должно применяться до commit"
    );
}

#[test]
fn server_side_db_decisions_reach_the_live_channel() {
    use syncth_core::plan::{Decision, PathDecision};

    let ctx = setup();
    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    let handle = net::spawn_one(a, "127.0.0.1:0", ctx.token.clone()).unwrap();
    let mut session = net::PreviewSession::open(&mut b, &handle.addr, &ctx.token).unwrap();
    let path = session.plan.remote[0].path.clone();

    // Как будто решение пришло из UI сервера в его отдельном соединении.
    let mut admin = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let mut plan = admin
        .load_plan("dev-b", syncth_core::Hlc::wall_ms())
        .unwrap()
        .unwrap();
    let blob = plan.blob_for(&path).unwrap();
    let hlc = admin.next_hlc().unwrap();
    let mut set = admin.load_decisions("dev-b").unwrap();
    set.entries.insert(
        path.clone(),
        PathDecision {
            path: path.clone(),
            action: Decision::Accept,
            blob,
            hlc,
            dev: "dev-a".into(),
        },
    );
    set.retain_valid(&plan);
    admin.save_decisions("dev-b", &set).unwrap();
    plan.decisions = set;
    admin.save_plan(&plan).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while session.plan.decisions.get(&path).is_none() && std::time::Instant::now() < deadline {
        session.poll(&mut b).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        session.plan.decisions.get(&path).is_some(),
        "решение из БД сервера должно приехать по живому каналу"
    );
    session.close();
    drop(session);
    let _ = handle.finish();
}

#[test]
fn old_protocol_is_rejected() {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let ctx = setup();
    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let handle = net::spawn_one(a, "127.0.0.1:0", ctx.token.clone()).unwrap();

    let mut stream = TcpStream::connect(&handle.addr).unwrap();
    let body = format!(
        r#"{{"t":"hello","dev":"old","token":"{}","proto":2,"have":{{}}}}"#,
        ctx.token
    )
    .into_bytes();
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&body).unwrap();
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).unwrap();
    let mut reply = vec![0u8; u32::from_be_bytes(len) as usize];
    stream.read_exact(&mut reply).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&reply).unwrap();
    assert_eq!(value["t"], "error");
    assert!(value["message"].as_str().unwrap().contains("версия"));
    drop(stream);
    let _ = handle.finish();
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
fn commit_applies_accepted_additions_and_clears_the_plan() {
    let ctx = setup();
    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    let handle = net::spawn_one(a, "127.0.0.1:0", ctx.token.clone()).unwrap();
    let mut session = net::PreviewSession::open(&mut b, &handle.addr, &ctx.token).unwrap();

    let outcome = session.commit(&mut b).unwrap();
    assert!(outcome.accepted >= 2, "{outcome:?}");
    assert!(session.finished);
    assert!(b
        .load_plan("dev-a", syncth_core::Hlc::wall_ms())
        .unwrap()
        .is_none());

    drop(session);
    let _ = handle.finish();

    assert_eq!(
        fs::read(ctx.root_b.join("Album/01.flac")).unwrap(),
        b"alpha-bytes"
    );
    assert_eq!(
        fs::read(ctx.root_b.join("Album/02.flac")).unwrap(),
        b"beta-bytes"
    );
    assert_eq!(state(&ctx.data_a), state(&ctx.data_b));
}

#[test]
fn commit_with_a_different_decision_is_rejected() {
    use syncth_core::plan::Decision;

    let ctx = setup();
    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    let handle = net::spawn_one(a, "127.0.0.1:0", ctx.token.clone()).unwrap();
    let mut session = net::PreviewSession::open(&mut b, &handle.addr, &ctx.token).unwrap();

    // Решение записано локально, но пиру ещё не отправлено: хэши разошлись.
    let path = session.plan.remote[0].path.clone();
    session
        .record_decision(&mut b, &path, Decision::Keep)
        .unwrap();
    let err = session.commit(&mut b).unwrap_err().to_string();
    assert!(err.contains("разошёлся"), "{err}");
    assert!(
        session.finished,
        "неудачный commit завершает сессию, чтобы её можно было открыть заново"
    );
    drop(session);
    let _ = handle.finish();
}

#[test]
fn commit_resumes_a_partial_download_from_the_same_offset() {
    let ctx = setup();
    let blob = syncth_core::scan::hash_file(&ctx.root_a.join("Album/01.flac")).unwrap();
    let key = blob.trim_start_matches("b3:");
    let incoming = ctx.data_b.join("incoming");
    fs::create_dir_all(&incoming).unwrap();
    // Телефон успел скачать только половину файла.
    fs::write(incoming.join(format!("{key}.part")), b"alpha").unwrap();

    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    let handle = net::spawn_one(a, "127.0.0.1:0", ctx.token.clone()).unwrap();
    let mut session = net::PreviewSession::open(&mut b, &handle.addr, &ctx.token).unwrap();
    session.commit(&mut b).unwrap();
    drop(session);
    let _ = handle.finish();

    assert_eq!(
        fs::read(ctx.root_b.join("Album/01.flac")).unwrap(),
        b"alpha-bytes",
        "файл должен докачаться с пятого байта"
    );
    assert!(
        !incoming.join(format!("{key}.part")).exists(),
        ".part исчезает только после проверки"
    );
    assert!(
        !incoming.join(format!("{key}.blob")).exists(),
        "проверенная загрузка убрана после раскладки"
    );
}

#[test]
fn keep_both_puts_both_versions_on_both_devices() {
    use syncth_core::plan::Decision;

    let ctx = setup();
    session(&ctx);

    let rel = "Album/01.flac";
    fs::write(ctx.root_a.join(rel), b"pc-version").unwrap();
    fs::write(ctx.root_b.join(rel), b"phone-version").unwrap();
    let mut a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    scan::scan(&ctx.root_a, &mut a).unwrap();
    drop(a);
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    scan::scan(&ctx.root_b, &mut b).unwrap();

    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let handle = net::spawn_one(a, "127.0.0.1:0", ctx.token.clone()).unwrap();
    let mut session = net::PreviewSession::open(&mut b, &handle.addr, &ctx.token).unwrap();
    assert_eq!(session.plan.conflicts.len(), 1);
    let path = session.plan.conflicts[0].path.clone();
    session
        .record_decision(&mut b, &path, Decision::KeepBoth)
        .unwrap();
    // Решение должно доехать до сервера до commit.
    session.poll(&mut b).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    session.poll(&mut b).unwrap();

    session.commit(&mut b).unwrap();
    drop(session);
    let _ = handle.finish();

    for root in [&ctx.root_a, &ctx.root_b] {
        let mut contents = Vec::new();
        for e in walkdir::WalkDir::new(root) {
            let e = e.unwrap();
            if e.file_type().is_file() {
                contents.push(fs::read(e.path()).unwrap());
            }
        }
        assert!(
            contents.iter().any(|c| c == b"pc-version"),
            "{}",
            root.display()
        );
        assert!(
            contents.iter().any(|c| c == b"phone-version"),
            "{}",
            root.display()
        );
    }
    assert_eq!(state(&ctx.data_a), state(&ctx.data_b));
}

#[test]
fn keep_in_library_restores_from_the_peer_vault() {
    use syncth_core::plan::Decision;

    let ctx = setup();
    session(&ctx);

    // ПК удалил трек навсегда: файл ушёл в корзину, операция удаления уехала.
    let mut a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    fs::remove_file(ctx.root_a.join("Album/02.flac")).unwrap();
    scan::scan(&ctx.root_a, &mut a).unwrap();
    syncth_core::resolve::resolve(
        &mut a,
        "Album/02.flac",
        syncth_core::Action::DeleteEverywhere,
    )
    .unwrap();
    drop(a);

    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    let handle = net::spawn_one(a, "127.0.0.1:0", ctx.token.clone()).unwrap();
    let mut session = net::PreviewSession::open(&mut b, &handle.addr, &ctx.token).unwrap();
    let path = session
        .plan
        .remote
        .iter()
        .find(|c| c.path == "Album/02.flac")
        .expect("удаление должно быть в плане")
        .path
        .clone();
    session
        .record_decision(&mut b, &path, Decision::KeepInLibrary)
        .unwrap();
    session.poll(&mut b).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    session.poll(&mut b).unwrap();

    session.commit(&mut b).unwrap();
    drop(session);
    let _ = handle.finish();

    assert_eq!(
        fs::read(ctx.root_b.join("Album/02.flac")).unwrap(),
        b"beta-bytes"
    );
    assert_eq!(
        fs::read(ctx.root_a.join("Album/02.flac")).unwrap(),
        b"beta-bytes",
        "решение возвращает файл и на источнике удаления"
    );
    assert_eq!(state(&ctx.data_a), state(&ctx.data_b));
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

/// Регрессия по второму пользовательскому сценарию: файл удалили на ПК, и
/// копии в корзине ПК нет. «Удалить везде» отправило удаление на телефон —
/// именно там файл лёг в корзину. Телефон возвращает трек из своей корзины,
/// и ПК обязан увидеть добавление и скачать блоб.
#[test]
fn track_restored_from_the_phone_vault_reaches_the_pc() {
    use syncth_core::plan::ChangeKind;

    let ctx = setup();
    session(&ctx);

    let path = "Album/01.flac";
    // Пользователь удалил файл на ПК: в корзину ПК копия не попала.
    fs::remove_file(ctx.root_a.join(path)).unwrap();
    {
        let mut a = Replica::open(&ctx.data_a, "dev-a").unwrap();
        scan::scan(&ctx.root_a, &mut a).unwrap();
        syncth_core::resolve::resolve(&mut a, path, syncth_core::Action::DeleteEverywhere).unwrap();
        assert!(a.vault_for_path(path).unwrap().is_none());
    }

    // Телефон получает удаление: его копия уходит в корзину телефона.
    session(&ctx);
    {
        let b = Replica::open(&ctx.data_b, "dev-b").unwrap();
        assert!(b.entry(path).unwrap().unwrap().deleted);
        assert!(b.vault_for_path(path).unwrap().is_some());
    }
    assert!(!ctx.root_b.join(path).exists());

    // Телефон возвращает трек из своей корзины.
    {
        let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
        syncth_core::vault::restore(&mut b, path).unwrap();
    }
    assert!(ctx.root_b.join(path).is_file());

    // Следующий сеанс: ПК видит чужое добавление и скачивает блоб.
    let b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    let handle = net::spawn_one(b, "127.0.0.1:0", ctx.token.clone()).unwrap();
    let mut a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let mut session = net::PreviewSession::open(&mut a, &handle.addr, &ctx.token).unwrap();
    let remote = session
        .plan
        .remote
        .iter()
        .find(|change| change.path == path)
        .expect("восстановление с телефона должно вернуться в план ПК");
    assert_eq!(remote.kind, ChangeKind::Added);
    assert!(remote.base_blob.is_empty());
    session.commit(&mut a).unwrap();
    drop(session);
    let _ = handle.finish();

    assert_eq!(
        fs::read(ctx.root_a.join(path)).unwrap(),
        b"alpha-bytes",
        "файл должен приехать с телефона"
    );
    assert!(!Replica::open(&ctx.data_a, "dev-a")
        .unwrap()
        .entry(path)
        .unwrap()
        .unwrap()
        .deleted);
    assert_eq!(state(&ctx.data_a), state(&ctx.data_b));
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

/// Регрессия ровно по пользовательскому сценарию: файл удалили на телефоне,
/// на ПК подтвердили «удалить везде», а потом вернули его из корзины ПК.
/// Телефон исполняет входящий сеанс и строит свой план сам. Его томбстоун
/// хранит прежнюю базу, но файла у него нет — восстановление обязано приехать
/// как чужое добавление, иначе блоб никто не запросит.
#[test]
fn track_restored_from_the_vault_reaches_the_phone_again() {
    use syncth_core::plan::{ChangeKind, Decision};

    let ctx = setup();
    session(&ctx);

    let path = "Album/01.flac";
    // Пользователь удалил файл на телефоне.
    fs::remove_file(ctx.root_b.join(path)).unwrap();
    {
        let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
        scan::scan(&ctx.root_b, &mut b).unwrap();
        assert!(!b.pending_deletes().unwrap().is_empty());
    }

    // Телефон — исполнитель: план строит он, решение «удалить везде» принято
    // на ПК и доехало до него.
    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let handle = net::spawn_one(a, "127.0.0.1:0", ctx.token.clone()).unwrap();
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    let mut session = net::PreviewSession::open(&mut b, &handle.addr, &ctx.token).unwrap();
    let local = session
        .plan
        .local
        .iter()
        .find(|change| change.path == path)
        .expect("телефон должен видеть своё удаление");
    assert_eq!(local.kind, ChangeKind::Deleted);
    session
        .record_decision(&mut b, path, Decision::DeleteEverywhere)
        .unwrap();
    session.poll(&mut b).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    session.poll(&mut b).unwrap();
    session.commit(&mut b).unwrap();
    drop(session);
    let _ = handle.finish();

    // На ПК файл лёг в корзину, запись стала томбстоуном.
    {
        let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
        assert!(a.entry(path).unwrap().unwrap().deleted);
        assert!(a.vault_for_path(path).unwrap().is_some());
    }

    // ПК восстановил трек из корзины: локальное добавление с новой базой.
    {
        let mut a = Replica::open(&ctx.data_a, "dev-a").unwrap();
        syncth_core::vault::restore(&mut a, path).unwrap();
    }
    assert!(ctx.root_a.join(path).is_file());

    // Следующий сеанс: телефон обязан увидеть добавление и попросить блоб.
    let a = Replica::open(&ctx.data_a, "dev-a").unwrap();
    let handle = net::spawn_one(a, "127.0.0.1:0", ctx.token.clone()).unwrap();
    let mut b = Replica::open(&ctx.data_b, "dev-b").unwrap();
    let mut session = net::PreviewSession::open(&mut b, &handle.addr, &ctx.token).unwrap();
    let remote = session
        .plan
        .remote
        .iter()
        .find(|change| change.path == path)
        .expect("восстановление должно вернуться в план телефона");
    assert_eq!(remote.kind, ChangeKind::Added);
    assert!(remote.base_blob.is_empty());
    session.commit(&mut b).unwrap();
    drop(session);
    let _ = handle.finish();

    assert_eq!(
        fs::read(ctx.root_b.join(path)).unwrap(),
        b"alpha-bytes",
        "файл должен снова приехать на телефон"
    );
    assert!(!Replica::open(&ctx.data_b, "dev-b")
        .unwrap()
        .entry(path)
        .unwrap()
        .unwrap()
        .deleted);
    assert_eq!(state(&ctx.data_a), state(&ctx.data_b));
}
