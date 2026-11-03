//! Живой commit через настоящий TCP: обновление существующего файла должно
//! не только записать новую базу, но и заменить содержимое на диске.

use std::fs;
use std::net::TcpListener;
use std::time::Duration;

use syncth_core::net::{serve_stream, PreviewSession};
use syncth_core::{scan, ChangeKind, Decision, Replica};
use tempfile::TempDir;

struct Pair {
    tmp: TempDir,
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
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();
    fs::write(root_a.join("song.txt"), b"version one").unwrap();
    fs::write(root_b.join("song.txt"), b"version one").unwrap();

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
        tmp,
        root_a,
        root_b,
        a,
        b,
        token,
    }
}

/// Открыть сессию с B, дать тесту посмотреть план и выполнить commit.
fn commit_with_b<F>(a: &mut Replica, b: Replica, token: &str, check: F) -> Replica
where
    F: FnOnce(&mut PreviewSession, &mut Replica),
{
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
    check(&mut session, a);
    session.commit(a).unwrap();

    let (result, b) = handle.join().unwrap();
    result.unwrap();
    b
}

#[test]
fn commit_replaces_existing_file_with_peer_update() {
    let Pair {
        tmp,
        root_a,
        root_b,
        mut a,
        b,
        token,
    } = setup();
    let _keep_tmp = &tmp;

    // Первый коммит только фиксирует совпадающие версии как базу.
    let b = commit_with_b(&mut a, b, &token, |session, _| {
        assert_eq!(session.plan.aligned_count(), 1);
        assert_eq!(session.plan.path_count(), 0);
    });

    // Телефон правит файл. ПК должен получить и содержимое, и новую базу.
    fs::write(root_b.join("song.txt"), b"version two").unwrap();
    let mut b = b;
    scan::scan(&root_b, &mut b).unwrap();

    let _b = commit_with_b(&mut a, b, &token, |session, _| {
        assert_eq!(session.plan.local.len(), 0);
        assert_eq!(session.plan.remote.len(), 1);
        assert_eq!(session.plan.path_count(), 1);
    });

    assert_eq!(fs::read(root_a.join("song.txt")).unwrap(), b"version two");
    let entry = a.entry("song.txt").unwrap().unwrap();
    assert_eq!(
        a.synced_blob("song.txt").unwrap(),
        entry.blob,
        "после коммита база должна указывать на новую версию"
    );
}

#[test]
fn deleted_addition_is_removed_on_both_sides_and_kept_in_vault() {
    let Pair {
        tmp,
        root_a,
        root_b,
        mut a,
        b,
        token,
    } = setup();
    let _keep_tmp = &tmp;

    // Сначала фиксируем общий файл, чтобы у плана была нормальная база.
    let b = commit_with_b(&mut a, b, &token, |session, _| {
        assert_eq!(session.plan.path_count(), 0);
    });

    // Новый файл на ПК. Решение «удалить везде» должно убрать его с обоих
    // устройств, сохранив копию в хранилище ПК.
    fs::write(root_a.join("new.txt"), b"new file").unwrap();
    scan::scan(&root_a, &mut a).unwrap();

    let b = commit_with_b(&mut a, b, &token, |session, replica| {
        assert_eq!(session.plan.local.len(), 1);
        let path = session.plan.local[0].path.clone();
        session
            .record_decision(replica, &path, Decision::DeleteEverywhere)
            .unwrap();
        session.poll(replica).unwrap();
    });

    assert!(!root_a.join("new.txt").exists());
    assert!(!root_b.join("new.txt").exists());
    assert!(a.live_entries().unwrap().iter().all(|e| e.path != "new.txt"));
    assert!(a.vault_rows().unwrap().iter().any(|row| row.path == "new.txt"));
    assert!(b.live_entries().unwrap().iter().all(|e| e.path != "new.txt"));
}

#[test]
fn remote_addition_can_be_deleted_everywhere_without_downloading_it() {
    let Pair {
        tmp,
        root_a,
        root_b,
        mut a,
        b,
        token,
    } = setup();
    let _keep_tmp = &tmp;

    let mut b = commit_with_b(&mut a, b, &token, |session, _| {
        assert_eq!(session.plan.path_count(), 0);
    });

    // Файл появился только на телефоне. ПК решает, что он не нужен нигде.
    fs::write(root_b.join("phone.txt"), b"phone only").unwrap();
    scan::scan(&root_b, &mut b).unwrap();

    commit_with_b(&mut a, b, &token, |session, replica| {
        assert_eq!(session.plan.remote.len(), 1);
        let path = session.plan.remote[0].path.clone();
        session
            .record_decision(replica, &path, Decision::DeleteEverywhere)
            .unwrap();
        session.poll(replica).unwrap();
    });

    assert!(!root_a.join("phone.txt").exists());
    assert!(!root_b.join("phone.txt").exists());
    assert!(a.live_entries().unwrap().iter().all(|e| e.path != "phone.txt"));
}

#[test]
fn phone_pending_delete_is_visible_to_pc_and_can_be_kept() {
    let Pair {
        tmp,
        root_a,
        root_b,
        mut a,
        b,
        token,
    } = setup();
    let _keep_tmp = &tmp;

    let mut b = commit_with_b(&mut a, b, &token, |session, _| {
        assert_eq!(session.plan.path_count(), 0);
    });

    // Телефон удаляет файл и сканирует: это ещё не операция, а «мягкое»
    // удаление, которое ПК обязан увидеть в плане и подтвердить.
    fs::remove_file(root_b.join("song.txt")).unwrap();
    scan::scan(&root_b, &mut b).unwrap();

    let b = commit_with_b(&mut a, b, &token, |session, replica| {
        assert_eq!(session.plan.remote.len(), 1);
        assert_eq!(session.plan.remote[0].kind, ChangeKind::Deleted);
        let path = session.plan.remote[0].path.clone();
        session
            .record_decision(replica, &path, Decision::KeepInLibrary)
            .unwrap();
        session.poll(replica).unwrap();
    });

    assert!(root_a.join("song.txt").is_file());
    assert!(root_b.join("song.txt").is_file());
    assert_eq!(fs::read(root_b.join("song.txt")).unwrap(), b"version one");
    assert_eq!(b.live_entries().unwrap().len(), 1);
}

#[test]
fn deleted_everywhere_leaves_no_phantom_plan() {
    let Pair {
        tmp,
        root_a,
        root_b,
        mut a,
        b,
        token,
    } = setup();
    let _keep_tmp = &tmp;

    let mut b = commit_with_b(&mut a, b, &token, |session, _| {
        assert_eq!(session.plan.path_count(), 0);
    });

    fs::remove_file(root_b.join("song.txt")).unwrap();
    scan::scan(&root_b, &mut b).unwrap();

    let b = commit_with_b(&mut a, b, &token, |session, replica| {
        assert_eq!(session.plan.remote.len(), 1);
        assert_eq!(session.plan.remote[0].kind, ChangeKind::Deleted);
        let path = session.plan.remote[0].path.clone();
        session
            .record_decision(replica, &path, Decision::DeleteEverywhere)
            .unwrap();
        session.poll(replica).unwrap();
    });

    assert!(!root_a.join("song.txt").exists());
    assert!(!root_b.join("song.txt").exists());

    // После общего удаления путь не должен возвращаться в план ни на одной
    // стороне: решение уже принято, решения больше нет.
    let b = commit_with_b(&mut a, b, &token, |session, _| {
        assert_eq!(session.plan.path_count(), 0);
        assert!(session.plan.local.is_empty());
        assert!(session.plan.remote.is_empty());
    });
    assert!(b.pending_deletes().unwrap().is_empty());
}

#[test]
fn decision_can_be_undone_and_chosen_again() {
    let Pair {
        tmp,
        root_a,
        root_b,
        mut a,
        b,
        token,
    } = setup();
    let _keep_tmp = &tmp;

    let mut b = commit_with_b(&mut a, b, &token, |session, _| {
        assert_eq!(session.plan.path_count(), 0);
    });

    fs::write(root_b.join("new.txt"), b"new").unwrap();
    scan::scan(&root_b, &mut b).unwrap();

    let b = commit_with_b(&mut a, b, &token, |session, replica| {
        assert_eq!(session.plan.remote.len(), 1);
        let path = session.plan.remote[0].path.clone();

        session
            .record_decision(replica, &path, Decision::OnlyHere)
            .unwrap();
        session.poll(replica).unwrap();
        assert_eq!(
            session.plan.decisions.get(&path).map(|d| d.action),
            Some(Decision::OnlyHere)
        );

        // Отмена должна дойти до пира: старый выбор не возвращается.
        session.forget_decision(replica, &path).unwrap();
        assert!(session.plan.explicit_decision(&path).is_none());
        for _ in 0..3 {
            session.poll(replica).unwrap();
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(session.plan.explicit_decision(&path).is_none());

        // После отмены можно выбрать другой вариант — им и завершаем.
        session
            .record_decision(replica, &path, Decision::Accept)
            .unwrap();
        session.poll(replica).unwrap();
        assert_eq!(
            session.plan.decisions.get(&path).map(|d| d.action),
            Some(Decision::Accept)
        );
    });

    assert!(root_a.join("new.txt").is_file());
    assert!(root_b.join("new.txt").is_file());
    assert!(b.live_entries().unwrap().iter().any(|e| e.path == "new.txt"));
}

#[test]
fn deleted_never_shared_file_does_not_block_commit() {
    let Pair {
        tmp,
        root_a,
        root_b,
        mut a,
        b,
        token,
    } = setup();
    let _keep_tmp = &tmp;

    // Черновик создан и удалён до первой синхронизации: пир о нём не знает.
    // Пир уже сопряжён — иначе мягкое удаление никому не нужно.
    a.add_peer("dev-b", "127.0.0.1:1", "Phone").unwrap();
    fs::write(root_a.join("draft.txt"), b"draft").unwrap();
    scan::scan(&root_a, &mut a).unwrap();
    fs::remove_file(root_a.join("draft.txt")).unwrap();
    scan::scan(&root_a, &mut a).unwrap();

    assert!(
        a.entry("draft.txt").unwrap().is_none(),
        "удалённый черновик не должен остаться в дереве"
    );
    assert!(a.pending_deletes().unwrap().is_empty());

    let b = commit_with_b(&mut a, b, &token, |session, _| {
        assert!(
            session.plan.local.iter().all(|c| c.path != "draft.txt"),
            "призрак не должен быть локальным удалением"
        );
        assert!(session.plan.remote.iter().all(|c| c.path != "draft.txt"));
        assert_eq!(session.plan.path_count(), 0);
    });

    assert!(!root_b.join("draft.txt").exists());
    assert!(b.entry("draft.txt").unwrap().is_none());
    assert!(a.entry("draft.txt").unwrap().is_none());
}
