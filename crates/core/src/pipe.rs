//! «Глупая труба»: обмен операциями через обычный каталог.
//!
//! Работает поверх Syncthing, Nextcloud, WebDAV, git, флешки — чего угодно,
//! что умеет доставлять файлы. Идемпотентность обеспечивает CRDT.

use anyhow::{Context, Result};
use std::path::Path;

use crate::op::Op;
use crate::store::Replica;

/// Пишет по одному файлу на операцию. Имя детерминировано → повторная
/// публикация не создаёт дублей.
pub fn export_ops(replica: &Replica, dir: &Path) -> Result<usize> {
    std::fs::create_dir_all(dir)?;
    let mut written = 0;
    for json in replica.ops_json()? {
        let op = Op::from_json(&json)?;
        let name = format!("{}.op.json", op.id.trim_start_matches("b3:"));
        let path = dir.join(name);
        if path.exists() {
            continue;
        }
        std::fs::write(&path, &json).with_context(|| format!("запись {}", path.display()))?;
        written += 1;
    }
    Ok(written)
}

/// Читает все операции из каталога и применяет их.
pub fn import_ops(replica: &mut Replica, dir: &Path) -> Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut applied = 0;
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    entries.sort();

    for path in entries {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("не прочитан {}: {e}", path.display());
                continue;
            }
        };
        let op = match Op::from_json(&text) {
            Ok(op) => op,
            Err(e) => {
                tracing::warn!("битая операция {}: {e}", path.display());
                continue;
            }
        };
        if replica.apply(&op)? {
            applied += 1;
        }
    }
    Ok(applied)
}
