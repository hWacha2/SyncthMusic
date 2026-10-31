//! Главный инвариант проекта: реплики сходятся при любом порядке доставки,
//! и повторное применение операции ничего не меняет.

use proptest::prelude::*;
use syncth_core::{Hlc, Op, OpKind, Replica};

fn path_of(i: u8) -> String {
    format!("Artist {}/Album/{:02}.flac", i % 3, i % 5)
}

fn blob_of(i: u8) -> String {
    format!("b3:{i:064x}")
}

fn make_ops(raws: &[(u8, u8, u8, u8, u8)]) -> Vec<Op> {
    raws.iter()
        .enumerate()
        .map(|(i, &(dev, action, a, b, blob))| {
            let hlc = Hlc::new(1_000 + (i as u64 / 3), i as u32);
            let dev = format!("dev-{dev}");
            let kind = match action % 3 {
                0 => OpKind::EntryPut {
                    path: path_of(a),
                    blob: blob_of(blob),
                    size: 100 + blob as u64,
                    mtime_ns: 1_700_000_000_000_000_000 + i as i64,
                    added_at: 1_700_000_000_000 + i as u64,
                },
                1 => OpKind::EntryDel { path: path_of(a) },
                _ => OpKind::EntryMove {
                    from: path_of(a),
                    to: path_of(b),
                    blob: blob_of(blob),
                    size: 100 + blob as u64,
                    mtime_ns: 1_700_000_000_000_000_000 + i as i64,
                    added_at: 1_700_000_000_000 + i as u64,
                },
            };
            Op::new(dev, hlc, Op::canonicalize(kind).unwrap())
        })
        .collect()
}

fn build(ops: &[Op], dev: &str, order: impl Iterator<Item = usize>) -> Replica {
    let mut r = Replica::in_memory(dev).unwrap();
    for i in order {
        r.apply(&ops[i]).unwrap();
    }
    r
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn replicas_converge_under_any_order(
        raws in prop::collection::vec((0u8..3, 0u8..3, 0u8..6, 0u8..6, 0u8..4), 0..40)
    ) {
        let ops = make_ops(&raws);
        let n = ops.len();

        let forward = build(&ops, "acc-1", 0..n);
        let backward = build(&ops, "acc-2", (0..n).rev());
        let even_first = build(&ops, "acc-3", (0..n).step_by(2).chain((1..n).step_by(2)));

        prop_assert_eq!(forward.state_hash().unwrap(), backward.state_hash().unwrap());
        prop_assert_eq!(forward.state_hash().unwrap(), even_first.state_hash().unwrap());
        prop_assert_eq!(forward.entries().unwrap(), backward.entries().unwrap());
        prop_assert_eq!(forward.entries().unwrap(), even_first.entries().unwrap());
    }

    #[test]
    fn applying_twice_is_a_noop(
        raws in prop::collection::vec((0u8..2, 0u8..3, 0u8..4, 0u8..4, 0u8..3), 0..30)
    ) {
        let ops = make_ops(&raws);
        let once = build(&ops, "once", 0..ops.len());

        let mut twice = Replica::in_memory("twice").unwrap();
        for _ in 0..2 {
            for op in &ops {
                twice.apply(op).unwrap();
            }
        }

        prop_assert_eq!(once.state_hash().unwrap(), twice.state_hash().unwrap());
        prop_assert_eq!(once.op_count().unwrap(), twice.op_count().unwrap());
    }
}

#[test]
fn newer_write_wins_across_devices() {
    let old = Op::new(
        "dev-a",
        Hlc::new(10, 0),
        OpKind::EntryPut {
            path: "a.flac".into(),
            blob: blob_of(1),
            size: 1,
            mtime_ns: 1,
            added_at: 1,
        },
    );
    let new = Op::new(
        "dev-b",
        Hlc::new(20, 0),
        OpKind::EntryPut {
            path: "a.flac".into(),
            blob: blob_of(2),
            size: 2,
            mtime_ns: 2,
            added_at: 2,
        },
    );

    let mut r = Replica::in_memory("tie").unwrap();
    r.apply(&new).unwrap();
    r.apply(&old).unwrap(); // пришло позже, но старше по HLC
    let e = r.entry("a.flac").unwrap().unwrap();
    assert_eq!(
        e.blob,
        blob_of(2),
        "побеждает больший HLC, а не порядок прихода"
    );
}
