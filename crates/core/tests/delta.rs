//! Дельта-синк: каждый сеанс передаёт только хвост журнала.
//!
//! Верхние HLC по каждому автору — достаточный и точный «чекпоинт»: журнал
//! одного устройства упорядочен, поэтому всё, что ниже его отметки, у пира уже
//! есть. Глобальный максимум для этого не годится.

use std::collections::HashMap;
use syncth_core::{Hlc, Op, OpKind, Replica};

fn path_of(op: &Op) -> &str {
    match &op.kind {
        OpKind::EntryPut { path, .. } => path,
        _ => "",
    }
}

fn put(dev: &str, ms: u64, path: &str, blob: &str) -> Op {
    let kind = OpKind::EntryPut {
        path: path.into(),
        blob: blob.into(),
        size: 3,
        mtime_ns: ms as i64,
        added_at: ms,
    };
    Op::new(
        dev.to_string(),
        Hlc { ms, counter: 0 },
        Op::canonicalize(kind).unwrap(),
    )
}

#[test]
fn watermark_is_the_last_hlc_of_each_author() {
    let mut a = Replica::in_memory("dev-a").unwrap();
    a.apply(&put("dev-a", 100, "a.flac", "b3:a")).unwrap();
    a.apply(&put("dev-a", 200, "b.flac", "b3:b")).unwrap();
    a.apply(&put("dev-b", 150, "c.flac", "b3:c")).unwrap();

    let w = a.watermarks().unwrap();
    assert_eq!(w.get("dev-a"), Some(&(200, 0)));
    assert_eq!(w.get("dev-b"), Some(&(150, 0)));
    assert_eq!(w.len(), 2);
}

#[test]
fn delta_contains_only_ops_the_peer_does_not_have() {
    let mut a = Replica::in_memory("dev-a").unwrap();
    let mut b = Replica::in_memory("dev-b").unwrap();
    for (ms, path, blob) in [(100, "one.flac", "b3:1"), (200, "two.flac", "b3:2")] {
        let op = put("dev-a", ms, path, blob);
        a.apply(&op).unwrap();
        b.apply(&op).unwrap();
    }
    // b ещё не видел это.
    a.apply(&put("dev-a", 300, "three.flac", "b3:3")).unwrap();
    // И не знает об отдельном авторе.
    a.apply(&put("dev-c", 50, "old.flac", "b3:4")).unwrap();

    let want = b.watermarks().unwrap();
    let delta = a.ops_since(&want).unwrap();
    let paths: Vec<&str> = delta.iter().map(path_of).collect();
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert!(paths.contains(&"three.flac"));
    assert!(paths.contains(&"old.flac"));
}

#[test]
fn empty_watermarks_means_full_log() {
    let mut a = Replica::in_memory("dev-a").unwrap();
    a.apply(&put("dev-a", 100, "one.flac", "b3:1")).unwrap();
    a.apply(&put("dev-a", 200, "two.flac", "b3:2")).unwrap();

    let all = a.ops_since(&HashMap::new()).unwrap();
    assert_eq!(all.len(), 2);
    // И повторный запрос с полным чекпоинтом не отдаёт ничего.
    assert!(a.ops_since(&a.watermarks().unwrap()).unwrap().is_empty());
}
