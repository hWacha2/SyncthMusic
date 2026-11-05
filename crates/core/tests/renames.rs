//! Переименования и переносы: это не удаление + добавление.
//!
//! Иначе на Windows/macOS переименование только по регистру выглядело бы как
//! «старый пропал, новый появился», а на телефоне остались бы две копии.

use std::fs;
use std::path::PathBuf;
use syncth_core::plan::{self, ChangeKind};
use syncth_core::{resolve, scan, Replica};
use tempfile::TempDir;

struct Ctx {
    _tmp: TempDir,
    root: PathBuf,
    data: PathBuf,
}

fn setup(files: &[(&str, &[u8])]) -> Ctx {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("music");
    let data = tmp.path().join("data");
    fs::create_dir_all(&root).unwrap();
    for (rel, bytes) in files {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }
    Ctx {
        _tmp: tmp,
        root,
        data,
    }
}

fn open(ctx: &Ctx) -> Replica {
    let r = Replica::open(&ctx.data, "dev-a").unwrap();
    r.set_root(&ctx.root).unwrap();
    r
}

fn rename(ctx: &Ctx, from: &str, to: &str) {
    let src = ctx.root.join(from);
    let dst = ctx.root.join(to);
    fs::create_dir_all(dst.parent().unwrap()).unwrap();
    fs::rename(src, dst).unwrap();
}

#[test]
fn moving_a_track_is_a_rename_not_delete_and_add() {
    let ctx = setup(&[("Album/01.flac", b"one"), ("Album/02.flac", b"two")]);
    let mut a = open(&ctx);
    scan::scan(&ctx.root, &mut a).unwrap();
    let before = a.entry("Album/01.flac").unwrap().unwrap();

    rename(&ctx, "Album/01.flac", "Compilations/01.flac");
    let report = scan::scan(&ctx.root, &mut a).unwrap();

    assert_eq!(report.renamed, 1, "{report:?}");
    assert_eq!(report.added, 0, "{report:?}");
    assert!(
        report.missing.is_empty(),
        "перенос не должен ждать удаления"
    );
    assert!(a.pending_deletes().unwrap().is_empty());

    let old = a.entry("Album/01.flac").unwrap().unwrap();
    let new = a.entry("Compilations/01.flac").unwrap().unwrap();
    assert!(old.deleted);
    assert!(!new.deleted);
    assert_eq!(new.blob, before.blob);
    assert_eq!(
        new.added_at, before.added_at,
        "порядок в плеере сохраняется"
    );

    let saved = resolve::prune_deleted_files(&mut a, &ctx.root).unwrap();
    assert!(saved.is_empty(), "перенос не создаёт conflict-copy");
    assert!(!ctx.root.join("Album/01.flac").exists());
    assert!(ctx.root.join("Compilations/01.flac").is_file());
}

#[test]
fn case_only_rename_does_not_delete_the_same_file() {
    let ctx = setup(&[("Album/01.flac", b"one")]);
    let mut a = open(&ctx);
    scan::scan(&ctx.root, &mut a).unwrap();

    // На Windows/macOS это тот же файл; путь в манифесте меняет регистр.
    rename(&ctx, "Album/01.flac", "Album/01.FLAC");
    let report = scan::scan(&ctx.root, &mut a).unwrap();
    assert_eq!(report.renamed, 1, "{report:?}");
    assert!(report.missing.is_empty());
    assert!(a.pending_deletes().unwrap().is_empty());

    // prune не должен принять «старый» путь за отдельный файл и удалить новый.
    resolve::prune_deleted_files(&mut a, &ctx.root).unwrap();
    assert!(ctx.root.join("Album/01.FLAC").is_file());
    assert_eq!(a.live_entries().unwrap().len(), 1);
}

#[test]
fn case_only_rename_with_a_tag_edit_is_still_a_move() {
    let ctx = setup(&[("Album/01.flac", b"one")]);
    let mut a = open(&ctx);
    let before = scan::scan(&ctx.root, &mut a).unwrap();
    assert_eq!(before.added, 1);
    let initial = a.entry("Album/01.flac").unwrap().unwrap();

    rename(&ctx, "Album/01.flac", "Album/01.FLAC");
    fs::write(ctx.root.join("Album/01.FLAC"), b"one-edited").unwrap();
    let report = scan::scan(&ctx.root, &mut a).unwrap();

    assert_eq!(report.renamed, 1, "{report:?}");
    assert_eq!(report.added, 0, "{report:?}");
    assert!(report.missing.is_empty());
    assert!(a.pending_deletes().unwrap().is_empty());

    let old = a.entry("Album/01.flac").unwrap().unwrap();
    let new = a.entry("Album/01.FLAC").unwrap().unwrap();
    assert!(old.deleted);
    assert_eq!(new.size, 10);
    assert_eq!(
        new.added_at, initial.added_at,
        "порядок сохраняется даже при правке тегов во время переименования"
    );
    resolve::prune_deleted_files(&mut a, &ctx.root).unwrap();
    assert!(ctx.root.join("Album/01.FLAC").is_file());
}

#[test]
fn scanned_rename_becomes_one_moved_change_in_the_plan() {
    let ctx = setup(&[("Telegram/10-Panda-Eyes-Ily.flac", b"panda")]);
    let mut a = open(&ctx);
    scan::scan(&ctx.root, &mut a).unwrap();
    // Первая синхронизация прошла: путь стал общей базой.
    a.confirm_synced().unwrap();

    rename(
        &ctx,
        "Telegram/10-Panda-Eyes-Ily.flac",
        "Telegram/10-Panda-Eyes-Ily - копия.flac",
    );
    let report = scan::scan(&ctx.root, &mut a).unwrap();
    assert_eq!(report.renamed, 1, "{report:?}");

    let plan = plan::build(&a, "dev-phone", "Phone", &[]).unwrap();
    assert_eq!(plan.local.len(), 1, "перенос не должен выглядеть добавлением");
    assert_eq!(plan.local[0].kind, ChangeKind::Moved);
    assert_eq!(
        plan.local[0].from.as_deref(),
        Some("Telegram/10-Panda-Eyes-Ily.flac"),
        "в плане должен быть старый путь"
    );
    assert_eq!(
        plan.local[0].path,
        "Telegram/10-Panda-Eyes-Ily - копия.flac"
    );
}

#[test]
fn a_copy_is_not_mistaken_for_a_rename() {
    let ctx = setup(&[("Album/01.flac", b"one")]);
    let mut a = open(&ctx);
    scan::scan(&ctx.root, &mut a).unwrap();

    fs::create_dir_all(ctx.root.join("Compilations")).unwrap();
    fs::copy(
        ctx.root.join("Album/01.flac"),
        ctx.root.join("Compilations/01.flac"),
    )
    .unwrap();

    let report = scan::scan(&ctx.root, &mut a).unwrap();
    assert_eq!(report.renamed, 0, "{report:?}");
    assert_eq!(report.added, 1, "{report:?}");
    assert!(report.missing.is_empty());
    assert_eq!(a.live_entries().unwrap().len(), 2);
    assert!(ctx.root.join("Album/01.flac").is_file());
}
