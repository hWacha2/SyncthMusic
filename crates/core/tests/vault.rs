//! Корзина: «удалить везде» должно быть обратимым.
//!
//! Сценарий жизни: трек удалили на телефоне, решение доехало до ПК, ПК убрал
//! файл из библиотеки — но оставил hardlink в `data_dir/vault`. Пока TTL не
//! истёк, трек можно вернуть, и возвращение уедет обратно на телефон.

use std::fs;
use std::path::{Path, PathBuf};
use syncth_core::{pipe, resolve, scan, vault, Action, Replica};
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

fn pair() -> (Ctx, Replica, Replica) {
    let ctx = setup(&[("Album/01.flac", b"one"), ("Album/02.flac", b"two")]);
    let mut a = open(&ctx, "data_a", &ctx.root_a);
    scan::scan(&ctx.root_a, &mut a).unwrap();
    let mut b = open(&ctx, "data_b", &ctx.root_b);
    sync(&mut a, &mut b, &ctx.pipe);
    copy_tree(&ctx.root_a, &ctx.root_b);
    scan::scan(&ctx.root_b, &mut b).unwrap();
    sync(&mut a, &mut b, &ctx.pipe);
    a.confirm_synced().unwrap();
    b.confirm_synced().unwrap();
    assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
    (ctx, a, b)
}

#[test]
fn deleted_track_can_be_restored_from_the_vault_and_it_propagates() {
    let (ctx, mut a, mut b) = pair();

    // Удалили на телефоне (A): файла там уже нет, корзина A пуста.
    fs::remove_file(ctx.root_a.join("Album/01.flac")).unwrap();
    scan::scan(&ctx.root_a, &mut a).unwrap();
    resolve::resolve(&mut a, "Album/01.flac", Action::DeleteEverywhere).unwrap();

    // Решение доехало до ПК (B). Файл ещё лежал на диске — он уходит в корзину B.
    sync(&mut a, &mut b, &ctx.pipe);
    resolve::prune_deleted_files(&mut b, &ctx.root_b).unwrap();
    assert!(!ctx.root_b.join("Album/01.flac").exists());
    assert!(b
        .vault_rows()
        .unwrap()
        .iter()
        .any(|r| r.path == "Album/01.flac"));

    // Передумали на ПК: трек возвращается из корзины.
    resolve::restore(&mut b, "Album/01.flac").unwrap();
    assert!(ctx.root_b.join("Album/01.flac").is_file());
    assert_eq!(
        fs::read(ctx.root_b.join("Album/01.flac")).unwrap(),
        b"one".to_vec()
    );
    assert!(!b.entry("Album/01.flac").unwrap().unwrap().deleted);
    assert!(b.vault_rows().unwrap().is_empty());

    // Восстановление — обычная более новая операция, поэтому оно доезжает до A.
    sync(&mut a, &mut b, &ctx.pipe);
    assert!(!a.entry("Album/01.flac").unwrap().unwrap().deleted);
    assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
}

#[test]
fn delete_everywhere_vaults_a_second_copy_of_the_same_track() {
    let ctx = setup(&[("Album/01.flac", b"one"), ("Copy/01.flac", b"one")]);
    let mut a = open(&ctx, "data_a", &ctx.root_a);
    scan::scan(&ctx.root_a, &mut a).unwrap();

    // На телефоне удалили одну копию и решили «удалить везде»: по блобу
    // убирается и вторая.
    fs::remove_file(ctx.root_a.join("Album/01.flac")).unwrap();
    scan::scan(&ctx.root_a, &mut a).unwrap();
    let out = resolve::resolve(&mut a, "Album/01.flac", Action::DeleteEverywhere).unwrap();
    assert_eq!(out.deleted_paths.len(), 2);
    assert!(!ctx.root_a.join("Copy/01.flac").exists());

    // Корзина сохранила содержимое, хотя файлов в библиотеке больше нет.
    let row = a
        .vault_for_path("Copy/01.flac")
        .unwrap()
        .expect("вторая копия должна быть в корзине");
    let data_dir = a.data_dir().unwrap();
    assert!(vault::vault_blob_path(data_dir, &row.blob).is_file());

    resolve::restore(&mut a, "Copy/01.flac").unwrap();
    assert_eq!(
        fs::read(ctx.root_a.join("Copy/01.flac")).unwrap(),
        b"one".to_vec()
    );
}

#[test]
fn vault_gc_removes_expired_rows_and_blobs() {
    let ctx = setup(&[("Album/01.flac", b"one")]);
    let mut a = open(&ctx, "data_a", &ctx.root_a);
    scan::scan(&ctx.root_a, &mut a).unwrap();

    let mut row = vault::preserve_file(&a, &ctx.root_a, "Album/01.flac")
        .unwrap()
        .unwrap();
    row.deleted_at = 1; // как будто удалено очень давно
    a.add_vault_row(&row.clone()).unwrap();
    let data_dir = ctx.tmp.path().join("data_a");
    let blob_path = vault::vault_blob_path(&data_dir, &row.blob);
    assert!(blob_path.is_file());

    let report = vault::gc(&a, vault::DEFAULT_TTL_DAYS).unwrap();
    assert_eq!(report.rows_removed, 1);
    assert_eq!(report.blobs_removed, 1);
    assert!(a.vault_rows().unwrap().is_empty());
    assert!(!blob_path.exists());
}

#[test]
fn vault_gc_removes_orphan_files() {
    let ctx = setup(&[("Album/01.flac", b"one")]);
    let mut a = open(&ctx, "data_a", &ctx.root_a);
    scan::scan(&ctx.root_a, &mut a).unwrap();

    let data_dir = ctx.tmp.path().join("data_a");
    let orphan = data_dir
        .join("vault")
        .join("blobs")
        .join("aa")
        .join("deadbeef");
    fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    fs::write(&orphan, b"stray").unwrap();

    let report = vault::gc(&a, vault::DEFAULT_TTL_DAYS).unwrap();
    assert_eq!(report.orphans_removed, 1);
    assert!(!orphan.exists());
}

#[test]
fn restore_fails_when_the_vault_file_was_lost() {
    let (ctx, mut a, _b) = pair();
    fs::remove_file(ctx.root_a.join("Album/01.flac")).unwrap();
    scan::scan(&ctx.root_a, &mut a).unwrap();
    resolve::resolve(&mut a, "Album/01.flac", Action::DeleteEverywhere).unwrap();

    // Здесь файла на диске уже не было, поэтому корзина пуста.
    let err = resolve::restore(&mut a, "Album/01.flac")
        .unwrap_err()
        .to_string();
    assert!(err.contains("корзине"), "{err}");
}
