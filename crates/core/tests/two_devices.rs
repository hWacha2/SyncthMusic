//! Сквозной сценарий двух устройств: обмен деревом через «глупую трубу»,
//! изменение файла и — главное — что удаление НЕ распространяется само.

use std::fs;
use std::path::{Path, PathBuf};
use syncth_core::{pipe, scan, Hlc, Replica};
use tempfile::TempDir;

struct Ctx {
    tmp: TempDir,
    root_a: PathBuf,
    root_b: PathBuf,
    pipe_dir: PathBuf,
}

fn setup() -> Ctx {
    let tmp = TempDir::new().unwrap();
    let root_a = tmp.path().join("music_a");
    let root_b = tmp.path().join("music_b");
    let pipe_dir = tmp.path().join("pipe");
    fs::create_dir_all(root_a.join("Album")).unwrap();
    fs::create_dir_all(&root_b).unwrap();
    fs::create_dir_all(&pipe_dir).unwrap();
    fs::write(root_a.join("Album/01.flac"), b"first-track-bytes").unwrap();
    fs::write(root_a.join("Album/02.flac"), b"second-track-bytes").unwrap();
    Ctx {
        tmp,
        root_a,
        root_b,
        pipe_dir,
    }
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(from).unwrap();
        let dst = to.join(rel);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::copy(entry.path(), &dst).unwrap();
    }
}

fn sync_via_pipe(a: &mut Replica, b: &mut Replica, dir: &Path) {
    pipe::export_ops(a, dir).unwrap();
    pipe::import_ops(b, dir).unwrap();
    pipe::export_ops(b, dir).unwrap();
    pipe::import_ops(a, dir).unwrap();
}

#[test]
fn two_devices_converge_and_deletions_stay_local() {
    let ctx = setup();
    let data_a = ctx.tmp.path().join("data_a");
    let data_b = ctx.tmp.path().join("data_b");

    let mut a = Replica::open(&data_a, "dev-a").unwrap();
    a.set_root(&ctx.root_a).unwrap();
    let mut clock_a = Hlc::default();
    let report = scan::scan(&ctx.root_a, &mut a, &mut clock_a).unwrap();
    assert_eq!(report.added, 2);
    assert_eq!(a.live_entries().unwrap().len(), 2);

    let mut b = Replica::open(&data_b, "dev-b").unwrap();
    b.set_root(&ctx.root_b).unwrap();
    let mut clock_b = Hlc::default();

    // Манифест приезжает раньше файлов: «ещё не скачано» ≠ «удалено».
    sync_via_pipe(&mut a, &mut b, &ctx.pipe_dir);
    assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
    let empty_scan = scan::scan(&ctx.root_b, &mut b, &mut clock_b).unwrap();
    assert_eq!(
        empty_scan.missing.len(),
        0,
        "не скачанные файлы не должны считаться удалёнными"
    );
    assert_eq!(b.pending_deletes().unwrap().len(), 0);

    // Файлы доехали.
    copy_tree(&ctx.root_a, &ctx.root_b);
    scan::scan(&ctx.root_b, &mut b, &mut clock_b).unwrap();
    sync_via_pipe(&mut a, &mut b, &ctx.pipe_dir);

    assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
    assert_eq!(b.live_entries().unwrap().len(), 2);
    for e in b.live_entries().unwrap() {
        assert!(
            b.is_present(&e.path).unwrap(),
            "{} должен быть помечен как локальный",
            e.path
        );
    }

    // Правка файла на телефоне (B) уезжает на ПК (A).
    fs::write(
        ctx.root_b.join("Album/02.flac"),
        b"second-track-bytes-edited",
    )
    .unwrap();
    let changed = scan::scan(&ctx.root_b, &mut b, &mut clock_b).unwrap();
    assert_eq!(changed.changed, 1);

    sync_via_pipe(&mut a, &mut b, &ctx.pipe_dir);
    assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
    let ea = a.entry("Album/02.flac").unwrap().unwrap();
    let eb = b.entry("Album/02.flac").unwrap().unwrap();
    assert_eq!(ea.blob, eb.blob);
    assert_ne!(ea.blob, a.entry("Album/01.flac").unwrap().unwrap().blob);

    // Удаление на B: не создаёт операций и не трогает A.
    let ops_before = b.op_count().unwrap();
    fs::remove_file(ctx.root_b.join("Album/01.flac")).unwrap();
    let report = scan::scan(&ctx.root_b, &mut b, &mut clock_b).unwrap();
    assert_eq!(report.missing.len(), 1);
    assert_eq!(
        b.op_count().unwrap(),
        ops_before,
        "удаление не должно порождать операции"
    );
    assert_eq!(b.pending_deletes().unwrap().len(), 1);

    sync_via_pipe(&mut a, &mut b, &ctx.pipe_dir);
    let on_a = a.entry("Album/01.flac").unwrap().unwrap();
    assert!(!on_a.deleted, "файл на ПК должен остаться");
}

#[test]
fn rescan_is_cheap_and_stable() {
    let ctx = setup();
    let mut a = Replica::open(&ctx.tmp.path().join("data"), "dev-a").unwrap();
    let mut clock = Hlc::default();

    let first = scan::scan(&ctx.root_a, &mut a, &mut clock).unwrap();
    assert_eq!(first.added, 2);
    let ops_after_first = a.op_count().unwrap();

    let second = scan::scan(&ctx.root_a, &mut a, &mut clock).unwrap();
    assert_eq!(second.unchanged, 2);
    assert_eq!(second.ops_emitted, 0);
    assert_eq!(
        a.op_count().unwrap(),
        ops_after_first,
        "повторный скан не должен порождать операции"
    );
}

#[test]
fn mtime_is_restored_after_write() {
    let ctx = setup();
    let src = ctx.root_a.join("Album/01.flac");
    let dst = ctx.root_b.join("copy.flac");
    fs::copy(&src, &dst).unwrap();

    let meta = fs::metadata(&src).unwrap();
    let mtime_ns = meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;

    scan::restore_mtime(&dst, mtime_ns).unwrap();
    let restored = fs::metadata(&dst).unwrap();
    let restored_ns = restored
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;

    // NTFS хранит время с точностью 100 нс.
    assert!(
        (restored_ns - mtime_ns).abs() <= 100,
        "mtime должен сохраняться"
    );
}
