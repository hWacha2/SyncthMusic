//! Подписи Ed25519: новая операция всегда подписана, подделка отклоняется.
//!
//! Старые операции без подписи пока принимаются — иначе нельзя обновить уже
//! работающую библиотеку. Но как только у `dev` появился ключ, второй ключ под
//! тем же именем запрещён (TOFU).

use syncth_core::{Op, OpKind, Replica};

fn kind(path: &str) -> OpKind {
    OpKind::EntryPut {
        path: path.into(),
        blob: "b3:00".into(),
        size: 1,
        mtime_ns: 2,
        added_at: 3,
    }
}

#[test]
fn local_ops_are_signed_and_survive_json() {
    let mut a = Replica::in_memory("dev-a").unwrap();
    let op = a.make_op(kind("Album/01.flac")).unwrap();

    assert!(op.pk.is_some(), "публичный ключ должен ехать с операцией");
    assert!(op.sig.is_some());
    assert!(op.verify_signature());
    assert_eq!(op.pk.as_deref(), Some(a.public_key().as_str()));

    let back = Op::from_json(&op.to_json()).unwrap();
    assert_eq!(back, op);
}

#[test]
fn tampered_signature_is_rejected() {
    let mut a = Replica::in_memory("dev-a").unwrap();
    let mut op = a.make_op(kind("Album/01.flac")).unwrap();

    let mut sig: Vec<char> = op.sig.clone().unwrap().chars().collect();
    sig[0] = if sig[0] == '0' { '1' } else { '0' };
    op.sig = Some(sig.into_iter().collect());

    assert!(!op.verify_signature());
    // id остался прежним, поэтому это ловится именно проверкой подписи.
    assert!(op.is_intact());
    assert!(Op::from_json(&op.to_json()).is_err());

    let mut b = Replica::in_memory("dev-b").unwrap();
    assert!(b.apply(&op).is_err());
}

#[test]
fn a_second_key_cannot_take_over_the_same_device_id() {
    let mut a = Replica::in_memory("dev-a").unwrap();
    let first = a.make_op(kind("one.flac")).unwrap();

    // Второй экземпляр с тем же именем, но другим случайным ключом.
    let mut impostor = Replica::in_memory("dev-a").unwrap();
    let second = impostor.make_op(kind("two.flac")).unwrap();
    assert_ne!(first.pk, second.pk);

    let mut b = Replica::in_memory("dev-b").unwrap();
    b.apply(&first).unwrap();
    assert!(b.apply(&second).is_err(), "смена ключа должна отклоняться");
}

#[test]
fn unsigned_legacy_ops_are_still_accepted() {
    let mut b = Replica::in_memory("dev-b").unwrap();
    let mut op = Op::new(
        "dev-old",
        syncth_core::Hlc { ms: 10, counter: 0 },
        Op::canonicalize(kind("old.flac")).unwrap(),
    );
    assert!(op.sig.is_none());
    assert!(b.apply(&op).unwrap());
    // Повторное применение идемпотентно.
    op.sig = None;
    assert!(!b.apply(&op).unwrap());
}
