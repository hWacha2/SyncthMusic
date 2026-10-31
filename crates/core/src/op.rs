//! Операции — атомы репликации.
//!
//! Свойства: иммутабельны, идемпотентны (по `id`), детерминированы,
//! сериализуются в JSON/CBOR размером 200–800 байт.

use crate::hlc::Hlc;
use crate::path;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum OpKind {
    /// Файл появился или изменился.
    EntryPut {
        path: String,
        blob: String,
        size: u64,
        mtime_ns: i64,
        added_at: u64,
    },
    /// Переименование/перенос. Данные едут внутри, чтобы применение не зависело
    /// от порядка доставки.
    EntryMove {
        from: String,
        to: String,
        blob: String,
        size: u64,
        mtime_ns: i64,
        added_at: u64,
    },
    /// Файл удалён из дерева. Всегда результат подтверждённого решения
    /// пользователя, никогда — молчаливой синхронизации.
    EntryDel { path: String },
    /// «У меня есть этот блоб» — для планирования передач.
    BlobHave { blob: String, size: u64 },
}

impl OpKind {
    pub fn name(&self) -> &'static str {
        match self {
            OpKind::EntryPut { .. } => "entry_put",
            OpKind::EntryMove { .. } => "entry_move",
            OpKind::EntryDel { .. } => "entry_del",
            OpKind::BlobHave { .. } => "blob_have",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Op {
    pub id: String,
    pub dev: String,
    pub hlc: Hlc,
    pub kind: OpKind,
    /// Публичный ключ автора (hex Ed25519), если операция подписана.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pk: Option<String>,
    /// Подпись канонического `id`. Старые операции без подписи принимаются
    /// ради совместимости; новые всегда подписываются.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
}

impl Op {
    /// Создаёт операцию и считает её `id` по каноническому телу.
    pub fn new(dev: impl Into<String>, hlc: Hlc, kind: OpKind) -> Self {
        let dev = dev.into();
        let id = compute_id(&dev, hlc, &kind);
        Self {
            id,
            dev,
            hlc,
            kind,
            pk: None,
            sig: None,
        }
    }

    /// Подписывает операцию ключом устройства. Публичный ключ едет вместе с
    /// операцией: проверяющая сторона не обязана знать пира заранее.
    pub fn sign(&mut self, key: &SigningKey) {
        let sig = key.sign(self.id.as_bytes());
        self.pk = Some(hex::encode(key.verifying_key().to_bytes()));
        self.sig = Some(hex::encode(sig.to_bytes()));
    }

    /// Криптографическая проверка подписи. Операции без подписи считаются
    /// совместимыми со старым форматом (но не «доверенными» — это решает
    /// хранилище через TOFU-таблицу ключей).
    pub fn verify_signature(&self) -> bool {
        match (&self.pk, &self.sig) {
            (None, None) => true,
            (Some(pk), Some(sig)) => {
                let Ok(pk_bytes) = hex::decode(pk) else {
                    return false;
                };
                let Ok(pk_bytes) = <[u8; 32]>::try_from(pk_bytes.as_slice()) else {
                    return false;
                };
                let Ok(vk) = VerifyingKey::from_bytes(&pk_bytes) else {
                    return false;
                };
                let Ok(sig_bytes) = hex::decode(sig) else {
                    return false;
                };
                let Ok(sig_bytes) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
                    return false;
                };
                let sig = Signature::from_bytes(&sig_bytes);
                vk.verify_strict(self.id.as_bytes(), &sig).is_ok()
            }
            _ => false,
        }
    }

    /// Нормализует пути внутри операции; вызывается до создания `Op`.
    pub fn canonicalize(mut kind: OpKind) -> anyhow::Result<OpKind> {
        match &mut kind {
            OpKind::EntryPut { path: p, .. } | OpKind::EntryDel { path: p } => {
                *p = path::normalize(p)?;
            }
            OpKind::EntryMove { from, to, .. } => {
                *from = path::normalize(from)?;
                *to = path::normalize(to)?;
            }
            OpKind::BlobHave { .. } => {}
        }
        Ok(kind)
    }

    /// Проверка целостности: `id` должен соответствовать телу.
    pub fn is_intact(&self) -> bool {
        compute_id(&self.dev, self.hlc, &self.kind) == self.id
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("Op всегда сериализуется")
    }

    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        let op: Op = serde_json::from_str(s)?;
        anyhow::ensure!(
            op.is_intact(),
            "операция повреждена: id не совпадает с телом"
        );
        anyhow::ensure!(op.verify_signature(), "неверная подпись операции {}", op.id);
        Ok(op)
    }
}

fn compute_id(dev: &str, hlc: Hlc, kind: &OpKind) -> String {
    #[derive(Serialize)]
    struct Body<'a> {
        dev: &'a str,
        hlc: Hlc,
        kind: &'a OpKind,
    }
    let bytes = serde_json::to_vec(&Body { dev, hlc, kind }).expect("сериализация не падает");
    format!("b3:{}", blake3::hash(&bytes).to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_and_detects_tampering() {
        let kind = OpKind::EntryPut {
            path: "a/b.flac".into(),
            blob: "b3:00".into(),
            size: 1,
            mtime_ns: 2,
            added_at: 3,
        };
        let a = Op::new("dev-a", Hlc::new(10, 0), kind.clone());
        let b = Op::new("dev-a", Hlc::new(10, 0), kind);
        assert_eq!(a.id, b.id);
        assert!(a.is_intact());

        let mut tampered = a.clone();
        tampered.dev = "dev-b".into();
        assert!(!tampered.is_intact());
    }

    #[test]
    fn json_roundtrip() {
        let op = Op::new(
            "dev-a",
            Hlc::new(10, 1),
            OpKind::EntryDel {
                path: "x/y.flac".into(),
            },
        );
        let back = Op::from_json(&op.to_json()).unwrap();
        assert_eq!(op, back);
    }
}
