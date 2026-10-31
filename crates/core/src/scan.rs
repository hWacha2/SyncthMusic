//! Сканирование корня библиотеки: хеширование, diff с БД, порождение операций.
//!
//! Файлы не перемещаются и не переименовываются (ADR-002). Сканер только
//! наблюдает и создаёт операции.

use crate::hlc::Hlc;
use crate::op::{Op, OpKind};
use crate::path;
use crate::store::Replica;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Debug, Default, Clone)]
pub struct ScanReport {
    pub added: usize,
    pub changed: usize,
    pub unchanged: usize,
    /// Пути, которые были в БД, но пропали с диска. В операции не превращаются —
    /// только в `pending_deletes`.
    pub missing: Vec<String>,
    pub skipped_busy: usize,
    pub ops_emitted: usize,
}

pub fn scan(root: &Path, replica: &mut Replica, clock: &mut Hlc) -> Result<ScanReport> {
    anyhow::ensure!(
        root.is_dir(),
        "корень библиотеки не найден: {}",
        root.display()
    );

    let mut report = ScanReport::default();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let dev = replica.device_id().to_string();

    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_ignored(e.path()))
    {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("пропущено при обходе: {e}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }

        let abs = entry.path();
        let rel_raw = match abs.strip_prefix(root) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let rel = match path::normalize(&rel_raw) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("некорректный путь {rel_raw}: {e}");
                continue;
            }
        };
        seen.insert(rel.clone());

        let before = match abs.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let size = before.len();
        let mtime_before = mtime_ns(&before);

        let existing = replica.entry(&rel)?;
        if let Some(e) = &existing {
            if !e.deleted && e.size == size && e.mtime_ns == mtime_before {
                report.unchanged += 1;
                continue;
            }
        }

        let blob = match hash_file(abs) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("не удалось прочитать {}: {e}", abs.display());
                report.skipped_busy += 1;
                continue;
            }
        };

        // Файл мог измениться прямо во время хеширования (плеер пишет теги).
        let after = match abs.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if after.len() != size || mtime_ns(&after) != mtime_before {
            report.skipped_busy += 1;
            continue;
        }

        let is_new = existing.as_ref().map(|e| e.deleted).unwrap_or(true);
        if !is_new {
            if let Some(e) = &existing {
                if e.blob == blob {
                    // Изменился только mtime — это важно для порядка в плеерах.
                    let kind = OpKind::EntryPut {
                        path: rel.clone(),
                        blob: blob.clone(),
                        size,
                        mtime_ns: mtime_before,
                        added_at: e.added_at,
                    };
                    let op = Op::new(dev.clone(), clock.tick(), Op::canonicalize(kind)?);
                    replica.apply(&op)?;
                    report.changed += 1;
                    report.ops_emitted += 1;
                    continue;
                }
            }
        }

        let added_at = existing
            .as_ref()
            .map(|e| e.added_at)
            .unwrap_or_else(Hlc::wall_ms);
        let kind = OpKind::EntryPut {
            path: rel,
            blob,
            size,
            mtime_ns: mtime_before,
            added_at,
        };
        let op = Op::new(dev.clone(), clock.tick(), Op::canonicalize(kind)?);
        replica.apply(&op)?;
        if is_new {
            report.added += 1;
        } else {
            report.changed += 1;
        }
        report.ops_emitted += 1;
    }

    // Всё, что реально лежит на диске, помечаем как присутствующее локально.
    for rel in &seen {
        replica.mark_present(rel)?;
    }

    // Пропавшие файлы: только те, что раньше были здесь. «Ещё не скачано»
    // удалением не считается. Ничего не удаляем — только фиксируем.
    let now = Hlc::wall_ms();
    for e in replica.local_entries()? {
        if e.deleted || seen.contains(&e.path) {
            continue;
        }
        replica.record_pending_delete(&e.path, &e.blob, now)?;
        replica.mark_absent(&e.path)?;
        report.missing.push(e.path);
    }

    Ok(report)
}

fn is_ignored(p: &Path) -> bool {
    let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
    name.starts_with(".syncth")
        || name.eq_ignore_ascii_case("$recycle.bin")
        || name.eq_ignore_ascii_case("system volume information")
        || name == ".git"
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    match meta.modified() {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i64,
            Err(_) => 0,
        },
        Err(_) => 0,
    }
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("открытие {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("b3:{}", hasher.finalize().to_hex()))
}

/// Восстанавливает mtime файла после записи — чтобы «сортировка по дате»
/// не съезжала на новом устройстве.
pub fn restore_mtime(path: &Path, mtime_ns: i64) -> Result<()> {
    let secs = (mtime_ns / 1_000_000_000).max(0) as u64;
    let nanos = (mtime_ns % 1_000_000_000).max(0) as u32;
    let t = UNIX_EPOCH + std::time::Duration::new(secs, nanos);
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(t))?;
    Ok(())
}
