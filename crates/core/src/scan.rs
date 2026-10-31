//! Сканирование корня библиотеки: хеширование, diff с БД, порождение операций.
//!
//! Файлы не перемещаются и не переименовываются (ADR-002). Сканер только
//! наблюдает и создаёт операции.

use crate::hlc::Hlc;
use crate::op::{Op, OpKind};
use crate::path;
use crate::store::{EntryRow, Replica};
use anyhow::{Context, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Debug, Default, Clone)]
pub struct ScanReport {
    pub added: usize,
    pub changed: usize,
    pub unchanged: usize,
    /// Переименования/переносы, распознанные по блобу.
    pub renamed: usize,
    /// Пути, которые были в БД, но пропали с диска. В операции не превращаются —
    /// только в `pending_deletes`.
    pub missing: Vec<String>,
    pub skipped_busy: usize,
    pub ops_emitted: usize,
}

pub fn scan(root: &Path, replica: &mut Replica) -> Result<ScanReport> {
    anyhow::ensure!(
        root.is_dir(),
        "корень библиотеки не найден: {}",
        root.display()
    );

    let mut report = ScanReport::default();
    let dev = replica.device_id().to_string();

    // Сначала собираем список файлов. Иначе нельзя отличить переименование
    // `Foo.flac → foo.flac` от двух реально существующих файлов, различающихся
    // только регистром: к моменту обработки второго первый мог быть ещё не
    // встречен обходом.
    let mut files: Vec<(std::path::PathBuf, String)> = Vec::new();
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
        let abs = entry.into_path();
        let rel_raw = match abs.strip_prefix(root) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        match path::normalize(&rel_raw) {
            Ok(rel) => files.push((abs, rel)),
            Err(e) => tracing::warn!("некорректный путь {rel_raw}: {e}"),
        }
    }

    let seen: BTreeSet<String> = files.iter().map(|(_, rel)| rel.clone()).collect();

    // Живые локальные записи по блобу — кандидаты в «старое имя» переименования.
    let mut by_blob: HashMap<String, Vec<EntryRow>> = HashMap::new();
    let mut by_key: HashMap<String, Vec<EntryRow>> = HashMap::new();
    for e in replica.local_entries()? {
        by_blob.entry(e.blob.clone()).or_default().push(e.clone());
        by_key.entry(path::key(&e.path)).or_default().push(e);
    }
    let mut moved_from: HashSet<String> = HashSet::new();

    for (abs, rel) in files {
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

        let blob = match hash_file(&abs) {
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

        // Переименование/перенос: блоб тот же, старого пути больше нет на диске.
        // Проверяем до добавления записи и только для нового/удалённого пути —
        // иначе это была бы обычная правка содержимого.
        if is_new {
            if let Some(cand) = rename_candidate(&rel, &blob, &seen, &by_blob, &by_key, &moved_from)
            {
                let kind = OpKind::EntryMove {
                    from: cand.path.clone(),
                    to: rel.clone(),
                    blob: blob.clone(),
                    size,
                    mtime_ns: mtime_before,
                    added_at: cand.added_at,
                };
                let op = Op::new(dev.clone(), replica.next_hlc()?, Op::canonicalize(kind)?);
                replica.apply(&op)?;
                moved_from.insert(cand.path.clone());
                report.renamed += 1;
                report.ops_emitted += 1;
                continue;
            }
            // Тот же ключ, но оба файла реально лежат на диске — это не перенос,
            // а коллизия регистра/Unicode. Не склеиваем и не теряем данные.
            if let Some(other) = by_blob
                .get(&blob)
                .into_iter()
                .flatten()
                .find(|e| e.path != rel && path::key(&e.path) == path::key(&rel))
            {
                if seen.contains(&other.path) {
                    tracing::warn!(
                        "коллизия путей: «{}» и «{}» различаются только регистром/Unicode; \
                         на Windows/macOS это один файл",
                        other.path,
                        rel
                    );
                }
            }
        }

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
                    let op = Op::new(dev.clone(), replica.next_hlc()?, Op::canonicalize(kind)?);
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
        let op = Op::new(dev.clone(), replica.next_hlc()?, Op::canonicalize(kind)?);
        replica.apply(&op)?;
        if is_new {
            report.added += 1;
        } else {
            report.changed += 1;
        }
        report.ops_emitted += 1;
    }

    // Всё, что реально лежит на диске: локально присутствует, ожидание
    // удаления снимается, ручное исключение снимается (файл вернули).
    for rel in &seen {
        if replica.is_ignored(rel)? {
            replica.set_ignored(rel, false)?;
        }
        replica.clear_pending_delete(rel)?;
        replica.mark_present(rel)?;
    }

    // Пропавшие файлы: только те, что раньше были здесь. «Ещё не скачано» и
    // «намеренно не держим здесь» удалением не считается. Ничего не удаляем.
    let now = replica.next_hlc()?.ms;
    for e in replica.local_entries()? {
        if e.deleted || seen.contains(&e.path) || replica.is_ignored(&e.path)? {
            continue;
        }
        replica.record_pending_delete(&e.path, &e.blob, now)?;
        replica.mark_absent(&e.path)?;
        report.missing.push(e.path);
    }

    Ok(report)
}

/// Ищет живой локальный путь с тем же блобом, которого больше нет в `seen`.
/// Приоритет — путь, совпадающий по регистру/Unicode: на Windows/macOS это
/// типичное переименование `Foo.flac → foo.flac`, и старый путь физически
/// указывает на тот же файл.
fn rename_candidate<'a>(
    new_path: &str,
    blob: &str,
    seen: &BTreeSet<String>,
    by_blob: &'a HashMap<String, Vec<EntryRow>>,
    by_key: &'a HashMap<String, Vec<EntryRow>>,
    moved_from: &HashSet<String>,
) -> Option<&'a EntryRow> {
    let new_key = path::key(new_path);

    // Сначала путь, совпадающий с точностью до регистра/NFC. Он не должен
    // лежать в `seen`: иначе на case-sensitive ФС это два разных файла.
    if let Some(cand) =
        by_key.get(&new_key).into_iter().flatten().find(|c| {
            c.path != new_path && !moved_from.contains(&c.path) && !seen.contains(&c.path)
        })
    {
        return Some(cand);
    }

    // Затем перенос в другую папку: содержимое обязано совпасть, иначе это
    // скорее удаление + добавление, и решение должен принять человек.
    let mut fallback = None;
    for cand in by_blob.get(blob).into_iter().flatten() {
        if cand.path == new_path || moved_from.contains(&cand.path) || seen.contains(&cand.path) {
            continue;
        }
        if path::key(&cand.path) == new_key {
            return Some(cand);
        }
        if fallback.is_none() {
            fallback = Some(cand);
        }
    }
    fallback
}

fn is_ignored(p: &Path) -> bool {
    let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
    name.starts_with(".syncth")
        || name.ends_with(".syncth-part")
        || name.ends_with(".syncth-conflict")
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

/// Для конфликтных копий и приёмника файлов.
pub fn mtime_ns_public(meta: &std::fs::Metadata) -> i64 {
    mtime_ns(meta)
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
