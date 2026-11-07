//! Сканирование корня библиотеки: хеширование, diff с БД, порождение операций.
//!
//! Файлы не перемещаются и не переименовываются (ADR-002). Сканер только
//! наблюдает и создаёт операции.

use crate::hlc::Hlc;
use crate::op::{Op, OpKind};
use crate::path;
use crate::store::{PathState, PathStateRow, Replica};
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
    /// только в `Missing`.
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
    // Скан пишет журнал несколькими транзакциями и не должен пересекаться с
    // другим сканом, построением плана или commit: план, прочитанный между
    // транзакциями, примет перенос за добавление (см. `core::lock`).
    let lock = replica.library_lock();
    let _guard = lock.lock();

    anyhow::ensure!(
        root.is_dir(),
        "корень библиотеки не найден: {}",
        root.display()
    );

    let mut report = ScanReport::default();

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

    // Один снимок состояний на весь скан. Сканер сам реализует переходы из
    // `docs/path-state.md`; совместимые проекции вроде `scan_rows` больше не
    // нужны.
    let mut states: HashMap<String, PathStateRow> = replica
        .path_states()?
        .into_iter()
        .map(|row| (row.path.clone(), row))
        .collect();
    let cache = replica.content_cache()?;
    let my_dev = replica.device_id().to_string();

    // Живые локальные записи по блобу — кандидаты в «старое имя» переименования.
    let mut by_blob: HashMap<String, Vec<PathStateRow>> = HashMap::new();
    let mut by_key: HashMap<String, Vec<PathStateRow>> = HashMap::new();
    for row in states.values() {
        if row.state != PathState::Live || !row.present {
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
    let mut dirty: HashSet<String> = HashSet::new();
    let mut removed: Vec<String> = Vec::new();
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

        let existing = states.get(&rel).cloned();

        // Быстрый путь 1: запись уже есть и размер с mtime не менялись.
        // Блоб при этом кладём в кэш содержимого — для будущего сброса истории.
        //
        // Для `Live { present=0 }` и `ExcludedRemote` доверять mtime нельзя:
        // локального файла здесь ещё не было, и совпадение с метаданными пира
        // ничего не говорит о содержимом. Такие пути читаем и хешируем.
        if let Some(e) = &existing {
            let content_known = e.state != PathState::Tombstone
                && !e.blob.is_empty()
                && (e.present || e.state == PathState::Missing);
            if content_known && e.size == size && e.mtime_ns == mtime_before {
                if mark_seen(&mut states, &rel) {
                    dirty.insert(rel.clone());
                }
                if !e.state.is_excluded() {
                    report.unchanged += 1;
                }
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

        let is_new = existing
            .as_ref()
            .map_or(true, |e| e.state == PathState::Tombstone);

        // Переименование/перенос: блоб тот же, старого пути больше нет на диске.
        // Проверяем до добавления записи и только для нового/удалённого пути —
        // иначе это была бы обычная правка содержимого.
        if is_new {
            if let Some((from, added_at)) =
                rename_candidate(&rel, &blob, &seen, &by_blob, &by_key, &moved_from)
            {
                let op = replica.make_op(OpKind::EntryMove {
                    from: from.clone(),
                    to: rel.clone(),
                    blob: blob.clone(),
                    size,
                    mtime_ns: mtime_before,
                    added_at,
                })?;
                let hlc = op.hlc;
                ops.push(op);
                if let Some(source) = states.get_mut(&from) {
                    source.state = PathState::Tombstone;
                    source.present = false;
                    source.pending_hlc = None;
                    source.created_at = None;
                    source.hlc = hlc;
                    source.dev = my_dev.clone();
                    dirty.insert(from.clone());
                }
                states.insert(
                    rel.clone(),
                    PathStateRow {
                        path: rel.clone(),
                        state: PathState::Live,
                        base: None,
                        blob: blob.clone(),
                        size,
                        mtime_ns: mtime_before,
                        added_at,
                        hlc,
                        dev: my_dev.clone(),
                        present: true,
                        pending_hlc: None,
                        created_at: None,
                    },
                );
                dirty.insert(rel.clone());
                moved_from.insert(from);
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

        // Переход по таблице 5.1: откуда → куда и нужен ли `EntryPut`.
        let mut row = existing.clone().unwrap_or_else(|| PathStateRow {
            path: rel.clone(),
            state: PathState::Live,
            base: None,
            blob: String::new(),
            size: 0,
            mtime_ns: 0,
            added_at: 0,
            hlc: Hlc::default(),
            dev: String::new(),
            present: false,
            pending_hlc: None,
            created_at: None,
        });
        let previous = existing.as_ref().map(|e| e.state);
        let emit_put;
        let added_at;
        match previous {
            None => {
                emit_put = true;
                added_at = Hlc::wall_ms();
                row.state = PathState::Live;
                row.base = None;
            }
            Some(PathState::Live) => {
                let e = existing.as_ref().unwrap();
                // Файл уже совпадает с известным содержимым (например, это
                // принятое, но ещё не подтверждённое добавление пира) —
                // достаточно отметить присутствие. Иначе это локальная правка
                // или смена только mtime.
                emit_put = e.present || e.blob != blob;
                added_at = e.added_at;
                if !e.present && e.blob == blob && e.base.is_none() {
                    // Блоб пришёл от пира и теперь есть у обеих сторон —
                    // это и есть общая база, даже если сеанс ещё не
                    // подтвердил её целиком.
                    row.base = Some(blob.clone());
                }
            }
            Some(PathState::Missing) => {
                // Файл вернулся: ожидание удаления снимается. Если блоб тот
                // же, но mtime изменился, отправим `EntryPut` ради mtime.
                emit_put = true;
                added_at = row.added_at;
                row.state = PathState::Live;
                row.pending_hlc = None;
            }
            Some(PathState::Tombstone) => {
                // Восстановил из проводника: это новая жизнь, прежняя база
                // больше не база.
                emit_put = true;
                added_at = Hlc::wall_ms();
                row.state = PathState::Live;
                row.base = None;
            }
            Some(PathState::ExcludedLocal) => {
                // Исключение — решение человека: содержимое обновляем, ops не шлём.
                emit_put = false;
                added_at = row.added_at;
            }
            Some(PathState::ExcludedRemote) => {
                // Файл появился локально; исключение сохраняется до явной отмены.
                emit_put = false;
                added_at = row.added_at;
                row.state = PathState::ExcludedLocal;
            }
            Some(PathState::ResurrectingLocal) => {
                emit_put = true;
                added_at = row.added_at;
            }
            Some(PathState::ResurrectingRemote) => {
                // Копия уже здесь: маркер снимается. Если содержимое отличается
                // от ожидаемого, это локальная правка.
                emit_put = existing.as_ref().is_some_and(|e| e.blob != blob);
                added_at = row.added_at;
                row.state = PathState::Live;
                row.created_at = None;
            }
        }
        row.path = rel.clone();
        row.blob = blob.clone();
        row.size = size;
        row.mtime_ns = mtime_before;
        row.added_at = added_at;
        row.present = true;
        if emit_put {
            let op = replica.make_op(OpKind::EntryPut {
                path: rel.clone(),
                blob: blob.clone(),
                size,
                mtime_ns: mtime_before,
                added_at,
            })?;
            row.hlc = op.hlc;
            row.dev = my_dev.clone();
            ops.push(op);
            report.ops_emitted += 1;
        }
        states.insert(rel.clone(), row);
        dirty.insert(rel.clone());
        match previous {
            Some(PathState::ExcludedLocal | PathState::ExcludedRemote) => {}
            Some(PathState::Tombstone) | None => report.added += 1,
            _ if emit_put => report.changed += 1,
            _ => report.unchanged += 1,
        }
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

    // Пропавшие файлы: только те, что раньше были здесь. «Ещё не скачано» и
    // «намеренно не держим здесь» удалением не считается. Ничего не удаляем.
    //
    // Без сопряжённых устройств мягкое удаление всё равно нужно: его можно
    // подтвердить («удалить везде») или вернуть из корзины. Поэтому черновики
    // забываются только когда есть с кем сверяться.
    let has_peers = replica.has_peers()?;
    let pending_hlc = if states.values().any(|row| {
        !seen.contains(&row.path)
            && row.state == PathState::Live
            && row.present
            && (row.base.is_some() || !has_peers)
    }) {
        Some(replica.next_hlc()?)
    } else {
        None
    };

    for row in states.values_mut() {
        if seen.contains(&row.path) {
            continue;
        }
        match row.state {
            PathState::Live => {
                if !row.present {
                    // Чужое добавление ещё не скачано: его отсутствие — не
                    // удаление, а ожидание файла от пира.
                    continue;
                }
                if row.base.is_some() || !has_peers {
                    row.state = PathState::Missing;
                    row.present = false;
                    row.pending_hlc = pending_hlc;
                    dirty.insert(row.path.clone());
                    report.missing.push(row.path.clone());
                } else if row.dev == my_dev {
                    // Собственное добавление, которое ни разу не входило в
                    // общую базу, при удалении забывается целиком: пир о пути
                    // не знает.
                    removed.push(row.path.clone());
                } else {
                    // Файл пира, ещё не подтверждённый как база: просто нет здесь.
                    row.present = false;
                    dirty.insert(row.path.clone());
                }
            }
            PathState::Missing => {
                if row.base.is_none() && has_peers {
                    // Черновик, удалённый до первой синхронизации.
                    removed.push(row.path.clone());
                }
            }
            PathState::Tombstone => {}
            PathState::ExcludedLocal => {
                // Уцелевшая копия исчезла: на отмене исключения проверим пира.
                row.state = PathState::ExcludedRemote;
                row.present = false;
                dirty.insert(row.path.clone());
            }
            PathState::ExcludedRemote => {}
            PathState::ResurrectingLocal => {
                // Локальная копия не вернулась: пусть отмена ищет её у пира.
                row.state = PathState::ExcludedRemote;
                row.present = false;
                dirty.insert(row.path.clone());
            }
            PathState::ResurrectingRemote => {}
        }
    }

    let removed_set: HashSet<&str> = removed.iter().map(String::as_str).collect();
    let changed_rows: Vec<PathStateRow> = dirty
        .iter()
        .filter(|path| !removed_set.contains(path.as_str()))
        .filter_map(|path| states.get(path).cloned())
        .collect();
    replica.save_scan_states(&changed_rows, &removed)?;
    replica.flush_hlc()?;

    Ok(report)
}

/// Отметить, что файл снова на диске. Возвращает `true`, если состояние
/// действительно изменилось. Для `Tombstone` этот путь не вызывается: файл,
/// вернувшийся после подтверждённого удаления, — новая жизнь через `EntryPut`.
fn mark_seen(states: &mut HashMap<String, PathStateRow>, path: &str) -> bool {
    let Some(row) = states.get_mut(path) else {
        return false;
    };
    match row.state {
        PathState::Live => {
            if row.present {
                false
            } else {
                row.present = true;
                true
            }
        }
        PathState::Missing => {
            row.state = PathState::Live;
            row.present = true;
            row.pending_hlc = None;
            true
        }
        PathState::ExcludedRemote => {
            row.state = PathState::ExcludedLocal;
            row.present = true;
            true
        }
        PathState::ResurrectingRemote => {
            row.state = PathState::Live;
            row.present = true;
            row.created_at = None;
            true
        }
        PathState::ExcludedLocal | PathState::ResurrectingLocal => {
            if row.present {
                false
            } else {
                row.present = true;
                true
            }
        }
        PathState::Tombstone => false,
    }
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
fn rename_candidate(
    new_path: &str,
    blob: &str,
    seen: &BTreeSet<String>,
    by_blob: &HashMap<String, Vec<PathStateRow>>,
    by_key: &HashMap<String, Vec<PathStateRow>>,
    moved_from: &HashSet<String>,
) -> Option<(String, u64)> {
    let new_key = path::key(new_path);

    // Сначала путь, совпадающий с точностью до регистра/NFC. Он не должен
    // лежать в `seen`: иначе на case-sensitive ФС это два разных файла.
    if let Some(cand) = by_key.get(&new_key).into_iter().flatten().find(|c| {
        c.path != new_path && !moved_from.contains(&c.path) && !seen.contains(&c.path)
    }) {
        return Some((cand.path.clone(), cand.added_at));
    }

    // Затем перенос в другую папку: содержимое обязано совпасть, иначе это
    // скорее удаление + добавление, и решение должен принять человек.
    let mut fallback = None;
    for cand in by_blob.get(blob).into_iter().flatten() {
        if cand.path == new_path || moved_from.contains(&cand.path) || seen.contains(&cand.path) {
            continue;
        }
        if path::key(&cand.path) == new_key {
            return Some((cand.path.clone(), cand.added_at));
        }
        if fallback.is_none() {
            fallback = Some((cand.path.clone(), cand.added_at));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn state(replica: &Replica, path: &str) -> PathStateRow {
        replica
            .path_state(path)
            .unwrap()
            .unwrap_or_else(|| panic!("нет состояния для {path}"))
    }

    #[test]
    fn scan_transitions_new_changed_and_missing() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut replica = Replica::in_memory("dev-a").unwrap();

        fs::write(root.join("track.flac"), b"one").unwrap();
        let report = scan(root, &mut replica).unwrap();
        assert_eq!(report.added, 1);

        let row = state(&replica, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.base, None);
        assert!(row.present);

        // Успешный сеанс подтверждает текущее содержимое как общую базу.
        replica.confirm_synced().unwrap();
        let base = state(&replica, "track.flac").base.unwrap();

        fs::write(root.join("track.flac"), b"two").unwrap();
        scan(root, &mut replica).unwrap();
        let row = state(&replica, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.base.as_deref(), Some(base.as_str()));
        assert_ne!(row.blob, base, "правка не должна менять базу");
        assert!(row.present);

        let blob = row.blob.clone();
        fs::remove_file(root.join("track.flac")).unwrap();
        let report = scan(root, &mut replica).unwrap();
        assert_eq!(report.missing, vec!["track.flac".to_string()]);

        let row = state(&replica, "track.flac");
        assert_eq!(row.state, PathState::Missing);
        assert!(!row.present);
        assert!(row.pending_hlc.is_some());
        assert_eq!(row.base.as_deref(), Some(base.as_str()));
        assert_eq!(row.blob, blob);
    }

    #[test]
    fn scan_restores_missing_with_the_same_and_a_different_blob() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut replica = Replica::in_memory("dev-a").unwrap();

        fs::write(root.join("track.flac"), b"one").unwrap();
        scan(root, &mut replica).unwrap();
        replica.confirm_synced().unwrap();
        let base = state(&replica, "track.flac").base.unwrap();

        fs::remove_file(root.join("track.flac")).unwrap();
        scan(root, &mut replica).unwrap();
        assert_eq!(state(&replica, "track.flac").state, PathState::Missing);

        // Файл вернулся с тем же содержимым: база сохраняется, ожидание снимается.
        fs::write(root.join("track.flac"), b"one").unwrap();
        scan(root, &mut replica).unwrap();
        let row = state(&replica, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.base.as_deref(), Some(base.as_str()));
        assert_eq!(row.blob, base);
        assert!(row.present);
        assert!(row.pending_hlc.is_none());

        // Пропал снова и вернулся уже другим содержимым: это локальная правка.
        fs::remove_file(root.join("track.flac")).unwrap();
        scan(root, &mut replica).unwrap();
        fs::write(root.join("track.flac"), b"three").unwrap();
        scan(root, &mut replica).unwrap();

        let row = state(&replica, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.base.as_deref(), Some(base.as_str()));
        assert_ne!(row.blob, base);
        assert_eq!(row.dev, "dev-a");
        assert!(row.present);
        assert!(row.pending_hlc.is_none());
    }

    #[test]
    fn scan_revives_tombstone_as_a_new_life() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut replica = Replica::in_memory("dev-a").unwrap();

        fs::write(root.join("track.flac"), b"one").unwrap();
        scan(root, &mut replica).unwrap();
        replica.confirm_synced().unwrap();

        let op = replica
            .make_op(OpKind::EntryDel {
                path: "track.flac".into(),
            })
            .unwrap();
        replica.apply(&op).unwrap();
        assert_eq!(state(&replica, "track.flac").state, PathState::Tombstone);

        fs::write(root.join("track.flac"), b"two").unwrap();
        scan(root, &mut replica).unwrap();

        let row = state(&replica, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.base, None, "после tombstone это новая жизнь");
        assert!(row.present);
        assert_eq!(row.dev, "dev-a");
        assert_eq!(row.blob, hash_file(&root.join("track.flac")).unwrap());
    }

    #[test]
    fn scan_keeps_exclusions_on_their_side() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut replica = Replica::in_memory("dev-a").unwrap();

        fs::write(root.join("track.flac"), b"one").unwrap();
        scan(root, &mut replica).unwrap();
        replica.confirm_synced().unwrap();
        let row = state(&replica, "track.flac");
        replica
            .add_exclusion("track.flac", "local", &row.blob, row.size, row.mtime_ns)
            .unwrap();
        assert_eq!(state(&replica, "track.flac").state, PathState::ExcludedLocal);

        // Содержимое исключённого файла обновляется, но операций не появляется.
        let ops_before = replica.op_count().unwrap();
        fs::write(root.join("track.flac"), b"two").unwrap();
        scan(root, &mut replica).unwrap();
        let row = state(&replica, "track.flac");
        assert_eq!(row.state, PathState::ExcludedLocal);
        assert_eq!(row.blob, hash_file(&root.join("track.flac")).unwrap());
        assert_eq!(replica.op_count().unwrap(), ops_before);

        // Уцелевшая копия исчезла: исключение остаётся, но сторона меняется.
        fs::remove_file(root.join("track.flac")).unwrap();
        scan(root, &mut replica).unwrap();
        let row = state(&replica, "track.flac");
        assert_eq!(row.state, PathState::ExcludedRemote);
        assert!(!row.present);

        // Файл вернули: исключение сохраняется до явной отмены.
        fs::write(root.join("track.flac"), b"three").unwrap();
        scan(root, &mut replica).unwrap();
        let row = state(&replica, "track.flac");
        assert_eq!(row.state, PathState::ExcludedLocal);
        assert!(row.present);
    }

    #[test]
    fn scan_finishes_resurrections() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut replica = Replica::in_memory("dev-a").unwrap();

        fs::write(root.join("track.flac"), b"one").unwrap();
        scan(root, &mut replica).unwrap();
        replica.confirm_synced().unwrap();

        // Ожидание копии от пира: файл появился здесь — маркер снимается.
        let row = state(&replica, "track.flac");
        replica
            .add_exclusion("remote.flac", "remote", &row.blob, row.size, row.mtime_ns)
            .unwrap();
        replica
            .add_resurrection("remote.flac", "remote", &row.blob, row.size, row.mtime_ns)
            .unwrap();
        assert_eq!(
            state(&replica, "remote.flac").state,
            PathState::ResurrectingRemote
        );
        fs::write(root.join("remote.flac"), b"one").unwrap();
        scan(root, &mut replica).unwrap();
        let restored = state(&replica, "remote.flac");
        assert_eq!(restored.state, PathState::Live);
        assert!(restored.present);

        // Локальная копия не вернулась: путь снова уходит на сторону пира.
        let row = state(&replica, "track.flac");
        replica
            .add_resurrection("track.flac", "local", &row.blob, row.size, row.mtime_ns)
            .unwrap();
        assert_eq!(
            state(&replica, "track.flac").state,
            PathState::ResurrectingLocal
        );
        fs::remove_file(root.join("track.flac")).unwrap();
        scan(root, &mut replica).unwrap();
        let lost = state(&replica, "track.flac");
        assert_eq!(lost.state, PathState::ExcludedRemote);
        assert!(!lost.present);
    }

    /// Пункт 15 раздела 10: после полного сброса истории первый скан
    /// начинает путь заново (`base=None`), а старые операции и решения
    /// не воскрешают исчезнувшие пути.
    #[test]
    fn first_scan_after_full_reset_starts_a_new_life() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut replica = Replica::in_memory("dev-a").unwrap();

        fs::write(root.join("track.flac"), b"one").unwrap();
        scan(root, &mut replica).unwrap();
        replica.confirm_synced().unwrap();
        let blob = state(&replica, "track.flac").blob;
        assert_eq!(state(&replica, "track.flac").base.as_deref(), Some(blob.as_str()));

        // Старое решение по пути, которого уже нет на диске.
        let del = replica
            .make_op(OpKind::EntryDel {
                path: "old.flac".into(),
            })
            .unwrap();
        replica.apply(&del).unwrap();
        assert_eq!(state(&replica, "old.flac").state, PathState::Tombstone);
        assert!(replica.op_count().unwrap() > 0);

        replica.reset_history().unwrap();
        assert_eq!(replica.op_count().unwrap(), 0);
        assert!(replica.path_states().unwrap().is_empty());

        scan(root, &mut replica).unwrap();
        let row = state(&replica, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.base, None, "после сброса это новая жизнь");
        assert!(row.present);
        assert_eq!(row.blob, blob);
        assert!(
            replica.path_state("old.flac").unwrap().is_none(),
            "старое решение не должно воскрешать путь"
        );
    }

    #[test]
    fn downloaded_peer_addition_becomes_a_common_base() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut replica = Replica::in_memory("dev-a").unwrap();

        fs::write(root.join("track.flac"), b"one").unwrap();
        let blob = hash_file(&root.join("track.flac")).unwrap();
        let mut peer = Replica::in_memory("dev-peer").unwrap();
        let op = peer
            .make_op(OpKind::EntryPut {
                path: "track.flac".into(),
                blob: blob.clone(),
                size: 3,
                mtime_ns: 1,
                added_at: 1,
            })
            .unwrap();
        replica.apply(&op).unwrap();

        let waiting = state(&replica, "track.flac");
        assert_eq!(waiting.state, PathState::Live);
        assert!(!waiting.present);
        assert_eq!(waiting.dev, "dev-peer");

        scan(root, &mut replica).unwrap();
        let row = state(&replica, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert!(row.present);
        assert_eq!(row.base.as_deref(), Some(blob.as_str()));
    }
}

