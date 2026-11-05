//! Сканирование корня библиотеки: хеширование, diff с БД, порождение операций.
//!
//! Файлы не перемещаются и не переименовываются (ADR-002). Сканер только
//! наблюдает и создаёт операции.

use crate::hlc::Hlc;
use crate::op::{Op, OpKind};
use crate::path;
use crate::store::{Replica, ScanRow};
use anyhow::{Context, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Этап сканирования — интерфейс показывает по нему подпись и полосу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStage {
    /// Обходим папку и считаем файлы; `total` ещё неизвестен.
    Walking,
    /// Сверяем содержимое; `total` — сколько файлов нашлось.
    Hashing,
}

/// Прогресс сканирования. `total == 0` на этапе `Walking`: список файлов
/// ещё не собран, поэтому полоса будет неопределённой.
///
/// Байты нужны интерфейсу: файлы сильно разного размера, и по штукам полоса
/// может стоять на месте, хотя чтение идёт. `total_bytes == 0` — размер ещё
/// неизвестен.
#[derive(Debug, Clone, Copy)]
pub struct ScanProgress {
    pub stage: ScanStage,
    pub done: usize,
    pub total: usize,
    pub done_bytes: u64,
    pub total_bytes: u64,
}

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
    scan_with_progress(root, replica, &mut |_| {})
}

/// Сколько собственных операций накапливаем до записи одной транзакцией.
const OP_BATCH: usize = 128;

/// То же, что [`scan`], но сообщает о ходе обхода и хеширования. Колбэк
/// вызывается на каждом файле; вызывающий сам решает, как часто обновлять UI.
pub fn scan_with_progress(
    root: &Path,
    replica: &mut Replica,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<ScanReport> {
    anyhow::ensure!(
        root.is_dir(),
        "корень библиотеки не найден: {}",
        root.display()
    );

    let mut report = ScanReport::default();
    // Пути вне синхронизации: их нельзя ни хешировать, ни превращать в
    // операции, ни считать пропавшими. Иначе исключение вернулось бы в план
    // при следующей же проверке.
    let excluded = replica.exclusion_paths()?;

    // Сначала собираем список файлов. Иначе нельзя отличить переименование
    // `Foo.flac → foo.flac` от двух реально существующих файлов, различающихся
    // только регистром: к моменту обработки второго первый мог быть ещё не
    // встречен обходом.
    let mut files: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut total_bytes: u64 = 0;
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
            Ok(rel) => {
                if excluded.contains(&rel) {
                    continue;
                }
                if let Ok(meta) = abs.metadata() {
                    total_bytes = total_bytes.saturating_add(meta.len());
                }
                files.push((abs, rel));
                progress(ScanProgress {
                    stage: ScanStage::Walking,
                    done: files.len(),
                    total: 0,
                    done_bytes: total_bytes,
                    total_bytes: 0,
                });
            }
            Err(e) => tracing::warn!("некорректный путь {rel_raw}: {e}"),
        }
    }

    let total = files.len();
    progress(ScanProgress {
        stage: ScanStage::Hashing,
        done: 0,
        total,
        done_bytes: 0,
        total_bytes,
    });

    let seen: BTreeSet<String> = files.iter().map(|(_, rel)| rel.clone()).collect();

    // Всё состояние путей читаем один раз. Раньше на каждый файл делались
    // отдельные SELECT'ы `entry`/`is_present`/`is_ignored`, а на телефоне это
    // заметная часть времени скана.
    let mut rows: HashMap<String, ScanRow> = replica
        .scan_rows()?
        .into_iter()
        .map(|row| (row.path.clone(), row))
        .collect();
    let pending: HashSet<String> = replica
        .pending_deletes()?
        .into_iter()
        .map(|delete| delete.path)
        .collect();
    let cache = replica.content_cache()?;
    let my_dev = replica.device_id().to_string();

    // Живые локальные записи по блобу — кандидаты в «старое имя» переименования.
    let mut by_blob: HashMap<String, Vec<ScanRow>> = HashMap::new();
    let mut by_key: HashMap<String, Vec<ScanRow>> = HashMap::new();
    for row in rows.values() {
        if row.deleted || !row.present || excluded.contains(&row.path) {
            continue;
        }
        by_blob.entry(row.blob.clone()).or_default().push(row.clone());
        by_key
            .entry(path::key(&row.path))
            .or_default()
            .push(row.clone());
    }
    let mut moved_from: HashSet<String> = HashSet::new();

    let mut hashed: Vec<(String, u64, i64, String)> = Vec::new();
    let mut ops: Vec<Op> = Vec::new();
    let mut hash_buf = vec![0u8; 1024 * 1024];
    let mut done_bytes: u64 = 0;

    for (index, (abs, rel)) in files.into_iter().enumerate() {
        progress(ScanProgress {
            stage: ScanStage::Hashing,
            done: index,
            total,
            done_bytes,
            total_bytes,
        });
        let before = match abs.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let size = before.len();
        let mtime_before = mtime_ns(&before);
        done_bytes = done_bytes.saturating_add(size);

        let existing = rows.get(&rel).cloned();

        // Быстрый путь 1: запись уже есть и размер с mtime не менялись.
        // Блоб при этом кладём в кэш содержимого — для будущего сброса истории.
        if let Some(e) = &existing {
            if !e.deleted && e.size == size && e.mtime_ns == mtime_before {
                report.unchanged += 1;
                hashed.push((rel, size, mtime_before, e.blob.clone()));
                continue;
            }
        }

        // Быстрый путь 2: тот же размер и mtime уже встречались. Файл не
        // читаем — берём блоб из кэша, который переживает `reset_history`.
        let cached = cache
            .get(&rel)
            .filter(|(cached_size, cached_mtime, _)| {
                *cached_size == size && *cached_mtime == mtime_before
            })
            .map(|(_, _, blob)| blob.clone());

        let blob = match cached {
            Some(blob) => {
                // Ничего не писали, но файл мог измениться параллельно.
                let after = match abs.metadata() {
                    Ok(m) => m,
                    Err(_) => {
                        report.skipped_busy += 1;
                        continue;
                    }
                };
                if after.len() != size || mtime_ns(&after) != mtime_before {
                    report.skipped_busy += 1;
                    continue;
                }
                blob
            }
            None => {
                let blob = match hash_file_with(&abs, &mut hash_buf) {
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
                blob
            }
        };

        hashed.push((rel.clone(), size, mtime_before, blob.clone()));

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
                ops.push(replica.make_op(kind)?);
                if let Some(from) = rows.get_mut(&cand.path) {
                    from.deleted = true;
                    from.present = false;
                }
                rows.insert(
                    rel.clone(),
                    ScanRow {
                        path: rel.clone(),
                        blob: blob.clone(),
                        size,
                        mtime_ns: mtime_before,
                        added_at: cand.added_at,
                        deleted: false,
                        present: false,
                        ignored: false,
                        dev: my_dev.clone(),
                        synced_blob: String::new(),
                    },
                );
                moved_from.insert(cand.path.clone());
                report.renamed += 1;
                report.ops_emitted += 1;
                flush_ops(replica, &mut ops)?;
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
                    ops.push(replica.make_op(kind)?);
                    if let Some(row) = rows.get_mut(&rel) {
                        row.mtime_ns = mtime_before;
                    }
                    report.changed += 1;
                    report.ops_emitted += 1;
                    flush_ops(replica, &mut ops)?;
                    continue;
                }
            }
        }

        let added_at = existing
            .as_ref()
            .map(|e| e.added_at)
            .unwrap_or_else(Hlc::wall_ms);
        let kind = OpKind::EntryPut {
            path: rel.clone(),
            blob: blob.clone(),
            size,
            mtime_ns: mtime_before,
            added_at,
        };
        ops.push(replica.make_op(kind)?);
        match rows.get_mut(&rel) {
            Some(row) => {
                row.blob = blob;
                row.size = size;
                row.mtime_ns = mtime_before;
                row.added_at = added_at;
                row.deleted = false;
                row.dev = my_dev.clone();
                row.synced_blob = String::new();
            }
            None => {
                rows.insert(
                    rel.clone(),
                    ScanRow {
                        path: rel.clone(),
                        blob,
                        size,
                        mtime_ns: mtime_before,
                        added_at,
                        deleted: false,
                        present: false,
                        ignored: false,
                        dev: my_dev.clone(),
                        synced_blob: String::new(),
                    },
                );
            }
        }
        if is_new {
            report.added += 1;
        } else {
            report.changed += 1;
        }
        report.ops_emitted += 1;
        flush_ops(replica, &mut ops)?;
    }

    finish_ops(replica, &mut ops)?;
    replica.flush_hlc()?;

    progress(ScanProgress {
        stage: ScanStage::Hashing,
        done: total,
        total,
        done_bytes: total_bytes.max(done_bytes),
        total_bytes,
    });

    // Кэш содержимого: всё, что реально посчитали или подтвердили по mtime.
    // Он не входит в историю синхронизации и не стирается при её сбросе.
    replica.save_content_cache(&hashed)?;
    let keep: HashSet<String> = seen.iter().cloned().collect();
    replica.prune_content_cache(&keep)?;

    // Всё, что реально лежит на диске: локально присутствует, ожидание
    // удаления снимается, ручное исключение снимается (файл вернули).
    let mut present = Vec::new();
    let mut unignore = Vec::new();
    let mut clear_pending = Vec::new();
    for rel in &seen {
        if excluded.contains(rel) {
            continue;
        }
        if let Some(row) = rows.get(rel) {
            if !row.deleted && !row.present {
                present.push(rel.clone());
            }
            if row.ignored {
                unignore.push(rel.clone());
            }
        }
        if pending.contains(rel) {
            clear_pending.push(rel.clone());
        }
    }
    replica.mark_presence_batch(&present, &unignore, &clear_pending)?;

    // Пропавшие файлы: только те, что раньше были здесь. «Ещё не скачано» и
    // «намеренно не держим здесь» удалением не считается. Ничего не удаляем.
    //
    // Собственное добавление, которое ни разу не входило в общую базу, при
    // удалении забывается целиком: пир о пути не знает, а мягкое удаление без
    // общей базы только расходится с его представлением и блокирует commit.
    let has_peers = replica.has_peers()?;
    if has_peers {
        replica.prune_ghost_deletes()?;
    }
    let hlc = replica.next_hlc()?;
    let mut record = Vec::new();
    let mut forget = Vec::new();
    for row in rows.values() {
        if row.deleted
            || !row.present
            || seen.contains(&row.path)
            || row.ignored
            || excluded.contains(&row.path)
        {
            continue;
        }
        if has_peers && row.dev == my_dev && row.synced_blob.is_empty() {
            forget.push(row.path.clone());
            continue;
        }
        record.push((row.path.clone(), row.blob.clone()));
        report.missing.push(row.path.clone());
    }
    replica.apply_missing_batch(hlc, &record, &forget)?;
    replica.flush_hlc()?;

    Ok(report)
}

/// Записать накопленные операции, когда их стало достаточно много.
fn flush_ops(replica: &mut Replica, ops: &mut Vec<Op>) -> Result<()> {
    if ops.len() >= OP_BATCH {
        replica.apply_local_batch(ops)?;
        ops.clear();
    }
    Ok(())
}

/// Дописать остаток перед концом скана.
fn finish_ops(replica: &mut Replica, ops: &mut Vec<Op>) -> Result<()> {
    if !ops.is_empty() {
        replica.apply_local_batch(ops)?;
        ops.clear();
    }
    Ok(())
}

/// Ищет живой локальный путь с тем же блобом, которого больше нет в `seen`.
/// Приоритет — путь, совпадающий по регистру/Unicode: на Windows/macOS это
/// типичное переименование `Foo.flac → foo.flac`, и старый путь физически
/// указывает на тот же файл.
fn rename_candidate<'a>(
    new_path: &str,
    blob: &str,
    seen: &BTreeSet<String>,
    by_blob: &'a HashMap<String, Vec<ScanRow>>,
    by_key: &'a HashMap<String, Vec<ScanRow>>,
    moved_from: &HashSet<String>,
) -> Option<&'a ScanRow> {
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
    let mut buf = vec![0u8; 1024 * 1024];
    hash_file_with(path, &mut buf)
}

/// Хеширование с переиспользуемым буфером: в скане он один на всю папку.
fn hash_file_with(path: &Path, buf: &mut [u8]) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("открытие {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    loop {
        let n = file.read(buf)?;
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
