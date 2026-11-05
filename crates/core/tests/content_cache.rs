//! Кэш содержимого: переживает сброс истории и избавляет от повторного
//! чтения всей библиотеки.

use std::fs;
use std::path::PathBuf;
use syncth_core::{scan, Replica};
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
    let mut replica = Replica::open(&ctx.data, "dev-a").unwrap();
    replica.set_root(&ctx.root).unwrap();
    replica
}

#[test]
fn content_cache_survives_reset_and_avoids_rehashing() {
    let ctx = setup(&[("Album/01.flac", b"one")]);
    let path = ctx.root.join("Album/01.flac");
    // Фиксированное время: после правки содержимого вернём его обратно,
    // чтобы проверка `size + mtime` совпала с кэшем.
    let mtime = 1_700_000_000_000_000_000i64;
    scan::restore_mtime(&path, mtime).unwrap();

    let mut replica = open(&ctx);
    let first = scan::scan(&ctx.root, &mut replica).unwrap();
    assert_eq!(first.added, 1);
    let blob = replica.entry("Album/01.flac").unwrap().unwrap().blob;
    assert!(replica.content_cache().unwrap().contains_key("Album/01.flac"));

    // История очищается, кэш содержимого — нет.
    replica.reset_history().unwrap();
    assert!(replica.entries().unwrap().is_empty());
    assert!(replica.content_cache().unwrap().contains_key("Album/01.flac"));

    // Меняем байты, сохраняя размер и mtime. Скан должен взять блоб из кэша:
    // ровно то же доверие к `size + mtime`, что и в быстром пути по entries.
    fs::write(&path, b"two").unwrap();
    scan::restore_mtime(&path, mtime).unwrap();

    let second = scan::scan(&ctx.root, &mut replica).unwrap();
    assert_eq!(second.added, 1);
    assert_eq!(
        replica.entry("Album/01.flac").unwrap().unwrap().blob,
        blob,
        "кэш должен вернуть прежний блоб, не перечитывая файл"
    );
}

#[test]
fn content_cache_forgets_deleted_paths() {
    let ctx = setup(&[("Album/01.flac", b"one"), ("Album/02.flac", b"two")]);
    let mut replica = open(&ctx);

    scan::scan(&ctx.root, &mut replica).unwrap();
    let cache = replica.content_cache().unwrap();
    assert_eq!(cache.len(), 2);

    fs::remove_file(ctx.root.join("Album/02.flac")).unwrap();
    scan::scan(&ctx.root, &mut replica).unwrap();

    let cache = replica.content_cache().unwrap();
    assert!(cache.contains_key("Album/01.flac"));
    assert!(!cache.contains_key("Album/02.flac"));
}

#[test]
fn changing_root_drops_content_cache() {
    let ctx = setup(&[("Album/01.flac", b"one")]);
    let mut replica = open(&ctx);
    scan::scan(&ctx.root, &mut replica).unwrap();
    assert!(!replica.content_cache().unwrap().is_empty());

    let other = ctx.root.parent().unwrap().join("other");
    fs::create_dir_all(&other).unwrap();
    replica.set_root(&other).unwrap();

    assert!(replica.content_cache().unwrap().is_empty());
}
