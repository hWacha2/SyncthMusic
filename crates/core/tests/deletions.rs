//! Разрешение удалений: решение принимает человек, а не устройство.
//!
//! Проверяем три исхода и один опасный случай (перенос на другом устройстве).

use std::fs;
use std::path::{Path, PathBuf};
use syncth_core::{pipe, plan, resolve, scan, vault, Action, Hlc, Replica};
use tempfile::TempDir;

struct Ctx {
    tmp: TempDir,
    root_a: PathBuf,
    root_b: PathBuf,
    pipe: PathBuf,
}

fn setup(files: &[(&str, &[u8])]) -> Ctx {
    let tmp = TempDir::new().unwrap();
    let root_a = tmp.path().join("music_a");
    let root_b = tmp.path().join("music_b");
    let pipe = tmp.path().join("pipe");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();
    fs::create_dir_all(&pipe).unwrap();
    for (rel, bytes) in files {
        let p = root_a.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }
    Ctx {
        tmp,
        root_a,
        root_b,
        pipe,
    }
}

fn open(ctx: &Ctx, name: &str, root: &Path) -> Replica {
    let r = Replica::open(&ctx.tmp.path().join(name), name).unwrap();
    r.set_root(root).unwrap();
    r
}

fn sync(a: &mut Replica, b: &mut Replica, pipe: &Path) {
    pipe::export_ops(a, pipe).unwrap();
    pipe::import_ops(b, pipe).unwrap();
    pipe::export_ops(b, pipe).unwrap();
    pipe::import_ops(a, pipe).unwrap();
}

fn copy_tree(from: &Path, to: &Path) {
    for e in walkdir::WalkDir::new(from) {
        let e = e.unwrap();
        if !e.file_type().is_file() {
            continue;
        }
        let rel = e.path().strip_prefix(from).unwrap();
        let dst = to.join(rel);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::copy(e.path(), &dst).unwrap();
    }
}

/// A и B с одинаковым деревом и файлами.
fn pair() -> (Ctx, Replica, Replica) {
    let ctx = setup(&[("Album/01.flac", b"one"), ("Album/02.flac", b"two")]);
    let mut a = open(&ctx, "data_a", &ctx.root_a);
    scan::scan(&ctx.root_a, &mut a).unwrap();
    let mut b = open(&ctx, "data_b", &ctx.root_b);
    sync(&mut a, &mut b, &ctx.pipe);
    copy_tree(&ctx.root_a, &ctx.root_b);
    scan::scan(&ctx.root_b, &mut b).unwrap();
    sync(&mut a, &mut b, &ctx.pipe);
    // Обе стороны видят одно дерево: это и есть общая база. Без неё удаление
    // неотличимо от исчезнувшего черновика.
    a.confirm_synced().unwrap();
    b.confirm_synced().unwrap();
    assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
    (ctx, a, b)
}

#[test]
fn resolving_delete_everywhere_removes_it_on_all_devices() {
    let (ctx, mut a, mut b) = pair();

    // Пользователь удалил трек на телефоне (A).
    fs::remove_file(ctx.root_a.join("Album/01.flac")).unwrap();
    let report = scan::scan(&ctx.root_a, &mut a).unwrap();
    assert_eq!(report.missing.len(), 1);

    // Пока не решили — ничего не реплицируется.
    sync(&mut a, &mut b, &ctx.pipe);
    assert!(!b.entry("Album/01.flac").unwrap().unwrap().deleted);
    assert!(ctx.root_b.join("Album/01.flac").is_file());

    let out = resolve::resolve(&mut a, "Album/01.flac", Action::DeleteEverywhere).unwrap();
    assert_eq!(out.deleted_paths, vec!["Album/01.flac"]);
    assert_eq!(out.ops_emitted, 1);
    assert!(!ctx.root_a.join("Album/01.flac").exists());

    // Решение доехало: запись удалена и файл на B действительно убран.
    sync(&mut a, &mut b, &ctx.pipe);
    resolve::prune_deleted_files(&mut b, &ctx.root_b).unwrap();
    assert!(b.entry("Album/01.flac").unwrap().unwrap().deleted);
    assert!(!ctx.root_b.join("Album/01.flac").exists());
    assert!(ctx.root_b.join("Album/02.flac").is_file());
    assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
}

#[test]
fn delete_everywhere_follows_the_track_after_a_move_elsewhere() {
    let (ctx, mut a, mut b) = pair();

    // На ПК (B) трек перенесли в другую папку, а на телефоне (A) в это же
    // время удалили старый путь. Обмен ещё не прошёл — намерения конкурентны.
    fs::create_dir_all(ctx.root_b.join("Compilations")).unwrap();
    fs::rename(
        ctx.root_b.join("Album/01.flac"),
        ctx.root_b.join("Compilations/01.flac"),
    )
    .unwrap();
    scan::scan(&ctx.root_b, &mut b).unwrap();

    fs::remove_file(ctx.root_a.join("Album/01.flac")).unwrap();
    scan::scan(&ctx.root_a, &mut a).unwrap();

    sync(&mut a, &mut b, &ctx.pipe);

    // Удаление по блобу, а не по пути: «удалить везде» находит и перенесённую
    // копию. Иначе трек остался бы жить под другим именем.
    let out = resolve::resolve(&mut a, "Album/01.flac", Action::DeleteEverywhere).unwrap();
    assert_eq!(out.deleted_paths, vec!["Compilations/01.flac"]);

    sync(&mut a, &mut b, &ctx.pipe);
    resolve::prune_deleted_files(&mut b, &ctx.root_b).unwrap();
    assert!(!ctx.root_b.join("Compilations/01.flac").exists());
    assert!(b.entry("Compilations/01.flac").unwrap().unwrap().deleted);
    assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
}

#[test]
fn only_here_keeps_the_global_library_intact() {
    let (ctx, mut a, mut b) = pair();

    fs::remove_file(ctx.root_a.join("Album/02.flac")).unwrap();
    scan::scan(&ctx.root_a, &mut a).unwrap();
    let out = resolve::resolve(&mut a, "Album/02.flac", Action::OnlyHere).unwrap();
    assert_eq!(out.ignored_paths, vec!["Album/02.flac"]);

    assert!(a.is_ignored("Album/02.flac").unwrap());
    assert!(!a.is_present("Album/02.flac").unwrap());
    // Запись жива в общей библиотеке: другие устройства её не потеряют.
    assert!(!a.entry("Album/02.flac").unwrap().unwrap().deleted);
    assert_eq!(a.live_entries().unwrap().len(), 2);
    assert!(a.pending_deletes().unwrap().is_empty());

    // Синхронизация не должна вернуть файл обратно на A.
    sync(&mut a, &mut b, &ctx.pipe);
    assert!(!ctx.root_a.join("Album/02.flac").exists());
    assert!(ctx.root_b.join("Album/02.flac").is_file());
    assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());

    // Передумал — файл снова нужен здесь и докачается.
    resolve::restore(&mut a, "Album/02.flac").unwrap();
    assert!(!a.is_ignored("Album/02.flac").unwrap());
}

#[test]
fn forget_clears_the_queue_without_deleting_anything() {
    let (ctx, mut a, _b) = pair();
    fs::remove_file(ctx.root_a.join("Album/01.flac")).unwrap();
    scan::scan(&ctx.root_a, &mut a).unwrap();
    assert_eq!(a.pending_deletes().unwrap().len(), 1);

    let ops = a.op_count().unwrap();
    a.clear_pending_delete("Album/01.flac").unwrap();
    assert!(a.pending_deletes().unwrap().is_empty());
    assert_eq!(
        a.op_count().unwrap(),
        ops,
        "снятие ожидания не создаёт операций"
    );
    assert!(!a.entry("Album/01.flac").unwrap().unwrap().deleted);
}

#[test]
fn restore_refuses_to_resurrect_a_deleted_track() {
    let (ctx, mut a, _b) = pair();
    fs::remove_file(ctx.root_a.join("Album/01.flac")).unwrap();
    scan::scan(&ctx.root_a, &mut a).unwrap();
    resolve::resolve(&mut a, "Album/01.flac", Action::DeleteEverywhere).unwrap();

    // Файла на A уже не было, когда принималось решение, — в корзине пусто.
    let err = resolve::restore(&mut a, "Album/01.flac")
        .unwrap_err()
        .to_string();
    assert!(err.contains("корзине"), "{err}");
}

/// Регрессия: после «удалить везде» файл вернули из корзины. Удаление уже
/// согласовано с телефоном, поэтому у пути нет общей базы — восстановление
/// обязано снова попасть в очередь как локальное добавление.
#[test]
fn restored_track_returns_to_the_change_queue() {
    let (ctx, mut a, mut b) = pair();

    let blob = a.entry("Album/01.flac").unwrap().unwrap().blob;
    a.record_pending_delete("Album/01.flac", &blob, Hlc::new(1, 0))
        .unwrap();
    resolve::resolve(&mut a, "Album/01.flac", Action::DeleteEverywhere).unwrap();
    // Файл лёг в корзину A, телефон получил удаление.
    assert!(!ctx.root_a.join("Album/01.flac").exists());
    sync(&mut a, &mut b, &ctx.pipe);
    resolve::prune_deleted_files(&mut b, &ctx.root_b).unwrap();

    // Обе стороны согласны: удаление не висит в плане.
    let peer_have = b.watermarks().unwrap();
    let plan_before =
        plan::build_with_peer_have(&a, &b.device_id(), "Phone", &[], Some(&peer_have)).unwrap();
    assert!(!plan_before.local.iter().any(|c| c.path == "Album/01.flac"));

    vault::restore(&mut a, "Album/01.flac").unwrap();
    assert!(ctx.root_a.join("Album/01.flac").is_file());

    let restored =
        plan::build_with_peer_have(&a, &b.device_id(), "Phone", &[], Some(&peer_have)).unwrap();
    let change = restored
        .local
        .iter()
        .find(|c| c.path == "Album/01.flac")
        .expect("восстановленный трек должен вернуться в очередь изменений");
    assert_eq!(change.kind, plan::ChangeKind::Added);
    assert!(change.base_blob.is_empty());

    // Решение «принять» снова отправляет трек на телефон.
    sync(&mut a, &mut b, &ctx.pipe);
    assert!(!b.entry("Album/01.flac").unwrap().unwrap().deleted);
}

#[test]
fn pending_delete_is_not_created_for_files_that_never_arrived() {
    let ctx = setup(&[("Album/01.flac", b"one")]);
    let mut a = open(&ctx, "data_a", &ctx.root_a);
    scan::scan(&ctx.root_a, &mut a).unwrap();
    pipe::export_ops(&a, &ctx.pipe).unwrap();

    // Манифест приехал, файлов на B нет — это «ещё не скачано», не удаление.
    let mut b = open(&ctx, "data_b", &ctx.root_b);
    pipe::import_ops(&mut b, &ctx.pipe).unwrap();
    let _ = a;
    let report = scan::scan(&ctx.root_b, &mut b).unwrap();
    assert!(report.missing.is_empty());
    assert!(b.pending_deletes().unwrap().is_empty());
    assert_eq!(b.live_entries().unwrap().len(), 1);
}
