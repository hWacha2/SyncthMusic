//! Применение выбранных решений после commit.
//!
//! Модуль не трогает сеть: он строит исходящие операции, список нужных блобов
//! и выполняет действия только после того, как все блобы скачаны и проверены.
//! Так оборванная передача не оставляет библиотеку в половинчатом состоянии.
//!
//! Порядок на каждой стороне:
//! 1. `prepare` — операции, которые надо отправить пиру, и блобы, которых нет.
//! 2. Обмен блобами (см. `net::PreviewSession`), `.part` переживает обрыв.
//! 3. `apply_local` — локальные отмены, конфликтные копии, освобождение путей.
//! 4. Применение операций пира и `finalize` — раскладка блобов и фиксация базы.

use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::hlc::Hlc;
use crate::op::{Op, OpKind};
use crate::path;
use crate::plan::{Change, ChangeKind, Decision, PathDecision, Plan, Resolution, Side};
use crate::resolve;
use crate::scan;
use crate::store::{LocalMove, PathState, PathStateRow, Replica};
use crate::vault;

#[derive(Debug, Default, Clone)]
pub struct Prepared {
    /// Операции, которые надо отправить пиру и (для новых) применить у себя.
    pub ops: Vec<Op>,
    /// Блобы, которых у нас нет: их надо скачать до применения.
    pub needs: Vec<String>,
    /// Ожидаемые размеры блобов из `needs`. По ним интерфейс показывает,
    /// сколько байт осталось принять; сам протокол размеры не передаёт.
    pub need_sizes: HashMap<String, u64>,
    /// Источники переносов, которые до отказа уже были согласованы с пиром.
    /// Только их можно снова подтвердить как общую базу: добавление, которое
    /// ещё не синхронизировали, после отказа остаётся добавлением.
    pub move_sources_in_base: HashSet<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub accepted: usize,
    pub rejected: usize,
}

/// Проверить, что план можно применять.
pub fn validate(replica: &Replica, plan: &Plan, resolution: &Resolution) -> Result<()> {
    anyhow::ensure!(
        !plan.is_expired(Hlc::wall_ms()),
        "план устарел — обновите его"
    );
    anyhow::ensure!(
        plan.local_is_current(replica)?,
        "есть новые локальные изменения — обновите план"
    );
    let unresolved = resolution.unresolved();
    anyhow::ensure!(
        unresolved.is_empty(),
        "требуют решения: {}",
        unresolved.join(", ")
    );
    Ok(())
}

/// Каталог временных загрузок. Он живёт рядом с БД, а не в библиотеке.
pub fn incoming_dir(replica: &Replica) -> Result<PathBuf> {
    let data_dir = replica
        .data_dir()
        .context("докачка доступна только для реплики на диске")?;
    let dir = data_dir.join("incoming");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn blob_key(blob: &str) -> &str {
    blob.trim_start_matches("b3:")
}

pub fn part_path(replica: &Replica, blob: &str) -> Result<PathBuf> {
    Ok(incoming_dir(replica)?.join(format!("{}.part", blob_key(blob))))
}

pub fn staged_path(replica: &Replica, blob: &str) -> Result<PathBuf> {
    Ok(incoming_dir(replica)?.join(format!("{}.blob", blob_key(blob))))
}

/// Сколько байт уже лежит в `.part`. Именно с этого места попросим пира
/// продолжить передачу.
pub fn partial_offset(replica: &Replica, blob: &str) -> u64 {
    part_path(replica, blob)
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Полностью скачанный и проверенный блоб.
pub fn verified(replica: &Replica, blob: &str) -> Option<PathBuf> {
    let p = staged_path(replica, blob).ok()?;
    p.is_file().then_some(p)
}

/// Блоб есть локально: живой файл, корзина или проверенная загрузка.
pub fn blob_source(replica: &Replica, root: &Path, blob: &str) -> Result<Option<PathBuf>> {
    if blob.is_empty() {
        return Ok(None);
    }
    if let Some(p) = verified(replica, blob) {
        return Ok(Some(p));
    }
    if let Some(data_dir) = replica.data_dir() {
        let p = vault::vault_blob_path(data_dir, blob);
        if p.is_file() {
            return Ok(Some(p));
        }
    }
    for row in replica.path_states()? {
        if row.present && !row.state.is_tombstone() && row.blob == blob {
            let p = resolve::disk_path(root, &row.path);
            if p.is_file() {
                return Ok(Some(p));
            }
        }
    }
    Ok(None)
}

pub fn has_blob(replica: &Replica, root: &Path, blob: &str) -> bool {
    blob_source(replica, root, blob)
        .map(|p| p.is_some())
        .unwrap_or(false)
}

/// Удалить проверенные загрузки после успешного commit: содержимое уже
/// разложено по библиотеке, держать вторую копию незачем.
pub fn cleanup_staged(replica: &Replica, blobs: &[String]) {
    for blob in blobs {
        if let Ok(p) = staged_path(replica, blob) {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn need(state: &mut Prepared, seen: &mut HashSet<String>, blob: &str, size: u64) {
    if !blob.is_empty() && seen.insert(blob.to_string()) {
        state.needs.push(blob.to_string());
        state.need_sizes.insert(blob.to_string(), size);
    }
}

fn entry_meta(replica: &Replica, path: &str) -> Result<Option<(u64, i64, u64)>> {
    Ok(replica
        .path_state(path)?
        .map(|row| (row.size, row.mtime_ns, row.added_at)))
}

/// Размер уже доступного блоба (живой файл, корзина, проверенная загрузка).
fn known_blob_size(replica: &Replica, root: &Path, blob: &str) -> Option<u64> {
    blob_source(replica, root, blob)
        .ok()
        .flatten()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
}

fn put_op(
    replica: &mut Replica,
    path: &str,
    blob: &str,
    size: u64,
    mtime_ns: i64,
    added_at: u64,
) -> Result<Op> {
    replica.make_op(OpKind::EntryPut {
        path: path.to_string(),
        blob: blob.to_string(),
        size,
        mtime_ns,
        added_at,
    })
}

/// Хвостовая копия для `KeepBoth`: имя должно совпасть на обеих сторонах.
/// Поэтому берём устройство-проигравшего и HLC общего решения, а не локальное
/// время и не «пира с моей точки зрения».
pub fn keep_both_copy(plan: &Plan, decision: &PathDecision, me: &str) -> String {
    let loser = if decision.dev == me {
        plan.peer_dev.as_str()
    } else {
        me
    };
    path::conflict_name(&decision.path, loser, decision.hlc.ms)
}

fn conflict_local_path(conflict: &crate::plan::Conflict) -> &str {
    if conflict.local_path.is_empty() {
        &conflict.path
    } else {
        &conflict.local_path
    }
}

/// Что останется на исходном пути после решения по конфликту.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictKeep {
    Local,
    Remote,
    Delete,
}

/// `Keep` — версия автора решения, `Accept` — версия второй стороны.
/// `KeepBoth` сохраняет обе версии, а при удалённой стороне вырождается в
/// `Keep`/`Accept` (второй версии просто нет).
fn conflict_keep(
    action: Decision,
    chooser: bool,
    local_deleted: bool,
    remote_deleted: bool,
) -> ConflictKeep {
    if action == Decision::KeepBoth {
        if local_deleted {
            return ConflictKeep::Remote;
        }
        if remote_deleted {
            return ConflictKeep::Local;
        }
        return if chooser {
            ConflictKeep::Local
        } else {
            ConflictKeep::Remote
        };
    }
    let keep_local =
        (action == Decision::Keep && chooser) || (action == Decision::Accept && !chooser);
    if keep_local {
        if local_deleted {
            ConflictKeep::Delete
        } else {
            ConflictKeep::Local
        }
    } else if remote_deleted {
        ConflictKeep::Delete
    } else {
        ConflictKeep::Remote
    }
}

/// Локальная версия конфликта уже удалена (мягко или подтверждённо).
fn conflict_local_deleted(replica: &Replica, conflict: &crate::plan::Conflict) -> Result<bool> {
    let path = conflict_local_path(conflict);
    Ok(replica.path_state(path)?.is_none_or(|row| {
        matches!(row.state, PathState::Missing | PathState::Tombstone)
    }))
}

/// Построить исходящие операции и список блобов, которые надо докачать.
pub fn prepare(replica: &mut Replica, plan: &Plan, resolution: &Resolution) -> Result<Prepared> {
    let root = replica.root()?.context("корень библиотеки не задан")?;
    let me = replica.device_id().to_string();

    // Для отказа от переноса нужно знать, был ли источник согласован с пиром
    // ещё до цепочки переносов. Скан сохраняет базу в tombstone источника,
    // поэтому снимок до применения операций даёт ответ.
    let (states, local_moves) =
        replica.read_snapshot(|| Ok((replica.path_states()?, replica.local_moves(&me)?)))?;
    let by_path: HashMap<&str, &PathStateRow> =
        states.iter().map(|row| (row.path.as_str(), row)).collect();
    let move_to: HashMap<&str, &LocalMove> = local_moves
        .iter()
        .map(|mv| (mv.to.as_str(), mv))
        .collect();

    // Свежие операции должны побеждать по HLC всё, что пришло от пира,
    // включая версии, из-за которых возник конфликт.
    replica.observe_hlc(plan.version)?;

    let mut prepared = Prepared::default();
    let mut seen = HashSet::new();

    for change in &plan.local {
        let decision = resolution.action(&change.path).unwrap_or(Decision::Keep);
        match change.kind {
            ChangeKind::Added => match decision {
                Decision::Accept => {
                    let (size, mtime_ns, added_at) = entry_meta(replica, &change.path)?
                        .unwrap_or((change.size, change.mtime_ns, Hlc::wall_ms()));
                    prepared.ops.push(put_op(
                        replica,
                        &change.path,
                        &change.blob,
                        size,
                        mtime_ns,
                        added_at,
                    )?);
                }
                Decision::Revert | Decision::DeleteEverywhere => {
                    // Добавление отменяется у источника: файл уходит в корзину,
                    // а запись закрывается собственной операцией удаления.
                    prepared.ops.push(replica.make_op(OpKind::EntryDel {
                        path: change.path.clone(),
                    })?);
                }
                _ => {}
            },
            ChangeKind::Updated => {
                match decision {
                    Decision::Accept => {
                        let (size, mtime_ns, added_at) = entry_meta(replica, &change.path)?
                            .unwrap_or((change.size, change.mtime_ns, Hlc::wall_ms()));
                        prepared.ops.push(put_op(
                            replica,
                            &change.path,
                            &change.blob,
                            size,
                            mtime_ns,
                            added_at,
                        )?);
                    }
                    Decision::Revert => {
                        anyhow::ensure!(
                            !change.base_blob.is_empty(),
                            "у «{}» нет базовой версии — откатить нельзя",
                            change.path
                        );
                        if !has_blob(replica, &root, &change.base_blob) {
                            // Точный размер базовой версии неизвестен: в записи
                            // пути хранится только текущий. Для полосы прогресса
                            // берём текущий размер как оценку.
                            need(&mut prepared, &mut seen, &change.base_blob, change.size);
                        }
                        let (_, mtime_ns, added_at) = entry_meta(replica, &change.path)?
                            .unwrap_or((change.size, change.mtime_ns, Hlc::wall_ms()));
                        let size = known_blob_size(replica, &root, &change.base_blob)
                            .unwrap_or(change.size);
                        prepared.ops.push(put_op(
                            replica,
                            &change.path,
                            &change.base_blob,
                            size,
                            mtime_ns,
                            added_at,
                        )?);
                    }
                    _ => {}
                }
            }
            ChangeKind::Renamed | ChangeKind::Moved => {
                let Some(from) = &change.from else {
                    bail!("перенос без старого пути: {}", change.path);
                };
                let (size, mtime_ns, added_at) = entry_meta(replica, &change.path)?.unwrap_or((
                    change.size,
                    change.mtime_ns,
                    Hlc::wall_ms(),
                ));
                let (op_from, op_to) = match decision {
                    Decision::Accept => (from.clone(), change.path.clone()),
                    Decision::Revert => {
                        if move_source_in_base(from, &change.blob, &by_path, &move_to) {
                            prepared.move_sources_in_base.insert(from.clone());
                        }
                        (change.path.clone(), from.clone())
                    }
                    _ => continue,
                };
                prepared.ops.push(replica.make_op(OpKind::EntryMove {
                    from: op_from,
                    to: op_to,
                    blob: change.blob.clone(),
                    size,
                    mtime_ns,
                    added_at,
                })?);
            }
            ChangeKind::Deleted => match decision {
                Decision::DeleteEverywhere => {
                    prepared.ops.push(replica.make_op(OpKind::EntryDel {
                        path: change.path.clone(),
                    })?);
                }
                Decision::KeepInLibrary => {
                    if !has_blob(replica, &root, &change.blob) {
                        need(&mut prepared, &mut seen, &change.blob, change.size);
                    }
                    prepared.ops.push(put_op(
                        replica,
                        &change.path,
                        &change.blob,
                        change.size,
                        change.mtime_ns,
                        Hlc::wall_ms(),
                    )?);
                }
                _ => {}
            },
        }
    }

    for conflict in &plan.conflicts {
        let decision = plan
            .explicit_entry(&conflict.path)
            .with_context(|| format!("конфликт без решения: {}", conflict.path))?;
        let action = resolution
            .action(&conflict.path)
            .with_context(|| format!("конфликт без решения: {}", conflict.path))?;
        let chooser = decision.dev == me;
        let local_deleted = conflict_local_deleted(replica, conflict)?;
        let remote_deleted = conflict.remote_blob.is_empty();
        let keep = conflict_keep(action, chooser, local_deleted, remote_deleted);
        let both_versions = action == Decision::KeepBoth && !local_deleted && !remote_deleted;

        // Блоб пира нужен, если он остаётся на исходном пути или уходит в
        // конфликтную копию.
        if (keep == ConflictKeep::Remote || both_versions)
            && !has_blob(replica, &root, &conflict.remote_blob)
        {
            need(
                &mut prepared,
                &mut seen,
                &conflict.remote_blob,
                conflict.remote_size,
            );
        }

        // Решение разрешает конфликт для обеих сторон; операцию создаёт
        // только автор решения, иначе HLC-гонка.
        if chooser {
            match keep {
                ConflictKeep::Delete => {
                    prepared.ops.push(replica.make_op(OpKind::EntryDel {
                        path: conflict.path.clone(),
                    })?);
                }
                ConflictKeep::Local => {
                    let (size, mtime_ns, added_at) = entry_meta(replica, conflict_local_path(conflict))?
                        .or(entry_meta(replica, &conflict.path)?)
                        .unwrap_or((conflict.local_size, Hlc::wall_ms() as i64, Hlc::wall_ms()));
                    prepared.ops.push(put_op(
                        replica,
                        &conflict.path,
                        &conflict.local_blob,
                        size,
                        mtime_ns,
                        added_at,
                    )?);
                }
                ConflictKeep::Remote => {
                    prepared.ops.push(put_op(
                        replica,
                        &conflict.path,
                        &conflict.remote_blob,
                        conflict.remote_size,
                        Hlc::wall_ms() as i64,
                        Hlc::wall_ms(),
                    )?);
                }
            }
        } else if both_versions {
            // Проигравшая версия сохраняется рядом и уезжает пиру.
            let copy = keep_both_copy(plan, decision, &me);
            let meta = entry_meta(replica, &conflict.path)?.unwrap_or((
                conflict.local_size,
                Hlc::wall_ms() as i64,
                Hlc::wall_ms(),
            ));
            prepared.ops.push(put_op(
                replica,
                &copy,
                &conflict.local_blob,
                meta.0,
                meta.1,
                meta.2,
            )?);
        }
    }

    // Блобы, которые нужны для принятых чужих изменений.
    for change in &plan.remote {
        let decision = resolution.action(&change.path).unwrap_or(Decision::Keep);
        let wanted = match change.kind {
            ChangeKind::Added
            | ChangeKind::Updated
            | ChangeKind::Renamed
            | ChangeKind::Moved => {
                decision == Decision::Accept
            }
            ChangeKind::Deleted => decision == Decision::KeepInLibrary,
        };
        if wanted && !has_blob(replica, &root, &change.blob) {
            need(&mut prepared, &mut seen, &change.blob, change.size);
        }
        // Чужое добавление можно не забирать, а удалить везде. Решение
        // исполняет только его автор: источник удалит файл у себя, а мы
        // отправим операцию удаления, чтобы он не вернулся с другой стороны.
        if change.kind == ChangeKind::Added
            && decision == Decision::DeleteEverywhere
            && plan
                .explicit_entry(&change.path)
                .is_some_and(|decision| decision.dev == me)
        {
            prepared.ops.push(replica.make_op(OpKind::EntryDel {
                path: change.path.clone(),
            })?);
        }
    }

    Ok(prepared)
}

/// Было ли содержимое переноса согласовано с пиром до всей цепочки переносов.
///
/// Скан оставляет базу в tombstone источника, а промежуточные имена цепочки
/// базы не имеют. Поэтому идём от `from` назад по журналу, пока не найдём
/// путь с базой. Цикл прямого и обратного переноса обрывается посещёнными.
fn move_source_in_base(
    from: &str,
    blob: &str,
    by_path: &HashMap<&str, &PathStateRow>,
    move_to: &HashMap<&str, &LocalMove>,
) -> bool {
    let mut current = from;
    let mut visited: HashSet<&str> = HashSet::new();
    loop {
        if !visited.insert(current) {
            return false;
        }
        if let Some(row) = by_path.get(current) {
            if let Some(base) = &row.base {
                return base == blob;
            }
        }
        let Some(previous) = move_to.get(current) else {
            return false;
        };
        if previous.blob != blob {
            return false;
        }
        current = previous.from.as_str();
    }
}

fn write_blob(root: &Path, rel: &str, src: &Path, mtime_ns: i64) -> Result<()> {
    let dst = resolve::disk_path(root, rel);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, &dst).with_context(|| format!("запись {}", dst.display()))?;
    crate::scan::restore_mtime(&dst, mtime_ns)?;
    Ok(())
}

/// Убирает занятый путь в conflict-copy и публикует её как обычный файл,
/// чтобы она не потерялась и уехала на другие устройства.
pub(crate) fn save_conflict_copy(
    replica: &mut Replica,
    root: &Path,
    existing: &Path,
    rel: &str,
) -> Result<()> {
    let old_blob = crate::scan::hash_file(existing)?;
    let meta = std::fs::metadata(existing)?;
    let old_mtime = crate::scan::mtime_ns_public(&meta);
    let old_size = meta.len();

    let conflict_rel = path::conflict_name(rel, replica.device_id(), Hlc::wall_ms());
    let conflict_abs = resolve::disk_path(root, &conflict_rel);
    if let Some(parent) = conflict_abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(existing, &conflict_abs)
        .with_context(|| format!("сохранение {}", conflict_abs.display()))?;

    let kind = OpKind::EntryPut {
        path: conflict_rel.clone(),
        blob: old_blob,
        size: old_size,
        mtime_ns: old_mtime,
        added_at: replica.next_hlc()?.ms,
    };
    let op = replica.make_op(kind)?;
    replica.apply(&op)?;
    // Файл уже лежит на диске под новым именем.
    set_present(replica, &conflict_rel, true)?;
    Ok(())
}

fn remove_to_vault(
    replica: &mut Replica,
    root: &Path,
    rel: &str,
    expected_blob: &str,
) -> Result<()> {
    let abs = resolve::disk_path(root, rel);
    // Файл мог измениться после построения плана (или это чужая правка под тем
    // же путём). Такую версию не удаляем: сохраняем рядом как conflict-copy.
    if abs.is_file() && scan::hash_file(&abs).ok().as_deref() != Some(expected_blob) {
        save_conflict_copy(replica, root, &abs, rel)?;
        return Ok(());
    }
    if replica.data_dir().is_some() {
        let _ = vault::preserve_file(replica, root, rel)?;
    }
    match std::fs::remove_file(&abs) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("удаление {}", abs.display())),
    }
}

/// Достать файл из корзины без создания собственной операции: операция для
/// пира уже лежит в `Prepared` и будет применена ниже.
fn restore_from_vault(replica: &mut Replica, root: &Path, rel: &str) -> Result<bool> {
    let Some(row) = replica.vault_for_path(rel)? else {
        return Ok(false);
    };
    let Some(data_dir) = replica.data_dir().map(Path::to_path_buf) else {
        return Ok(false);
    };
    let src = vault::vault_blob_path(&data_dir, &row.blob);
    if !src.is_file() {
        replica.remove_vault_row(&row.blob, &row.path)?;
        return Ok(false);
    }
    let dst = resolve::disk_path(root, rel);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Не перезаписываем версию, появившуюся на этом пути после удаления.
    if dst.is_file() && scan::hash_file(&dst).ok().as_deref() != Some(row.blob.as_str()) {
        save_conflict_copy(replica, root, &dst, rel)?;
    }
    std::fs::copy(&src, &dst).with_context(|| format!("восстановление {}", dst.display()))?;
    crate::scan::restore_mtime(&dst, row.mtime_ns)?;
    replica.remove_vault_row(&row.blob, &row.path)?;
    Ok(true)
}

fn staged_or_known(replica: &Replica, root: &Path, blob: &str) -> Option<PathBuf> {
    blob_source(replica, root, blob).ok().flatten()
}

/// Запомнить путь вне синхронизации. Сторона говорит, где файл остаётся:
/// при добавлении — там, где он появился; при удалении — там, где он ещё есть.
fn remember_exclusion(replica: &Replica, change: &Change) -> Result<()> {
    if !replica.exclusions_enabled() {
        return Ok(());
    }
    let side = match change.kind {
        ChangeKind::Added => change.side,
        ChangeKind::Deleted => match change.side {
            Side::Local => Side::Remote,
            Side::Remote => Side::Local,
        },
        _ => return Ok(()),
    };
    exclude_change(replica, change, side)
}

/// Перевести путь в `ExcludedLocal`/`ExcludedRemote` (раздел 5.3 спецификации).
/// Исключение — решение человека, поэтому операций не порождает, а ожидание
/// удаления снимается: путь больше не участвует в синхронизации.
fn exclude_change(replica: &Replica, change: &Change, side: Side) -> Result<()> {
    let now = Hlc::wall_ms();
    let mut row = match replica.path_state(&change.path)? {
        Some(row) => row,
        None => PathStateRow {
            path: change.path.clone(),
            state: PathState::Live,
            base: None,
            blob: String::new(),
            size: 0,
            mtime_ns: 0,
            added_at: now,
            hlc: Hlc::default(),
            dev: String::new(),
            present: false,
            pending_hlc: None,
            created_at: None,
        },
    };
    row.state = match side {
        Side::Local => PathState::ExcludedLocal,
        Side::Remote => PathState::ExcludedRemote,
    };
    row.blob = change.blob.clone();
    row.size = change.size;
    row.mtime_ns = change.mtime_ns;
    if row.added_at == 0 {
        row.added_at = now;
    }
    // Сторона исключения совпадает с тем, где физически лежит копия.
    row.present = side == Side::Local;
    row.pending_hlc = None;
    row.created_at = Some(now);
    replica.save_path_state(&row)
}

/// Поставить/снять `present` после действия на диске. Состояние пути задаёт
/// решение или применённая операция, поэтому здесь меняется только флаг.
fn set_present(replica: &Replica, path: &str, present: bool) -> Result<()> {
    let Some(mut row) = replica.path_state(path)? else {
        return Ok(());
    };
    if row.present != present {
        row.present = present;
        replica.save_path_state(&row)?;
    }
    Ok(())
}

/// Снять маркер возврата: файл уже на месте и подтверждён.
fn clear_resurrection_marker(replica: &Replica, path: &str) -> Result<()> {
    let Some(mut row) = replica.path_state(path)? else {
        return Ok(());
    };
    if !row.state.is_resurrecting() {
        return Ok(());
    }
    row.state = PathState::Live;
    row.created_at = None;
    replica.save_path_state(&row)
}

fn op_hits_exclusion(kind: &OpKind, excluded: &HashSet<String>) -> bool {
    match kind {
        OpKind::EntryPut { path, .. } | OpKind::EntryDel { path } => excluded.contains(path),
        OpKind::EntryMove { from, to, .. } => {
            excluded.contains(from) || excluded.contains(to)
        }
        OpKind::BlobHave { .. } => false,
    }
}

/// Выполнить локальную часть решений до применения чужих операций. Блобы уже
/// должны быть скачаны. Здесь освобождаются пути, которые заменяются версией
/// пира, отменяются изменения и раскладываются конфликтные копии.
pub fn apply_local(
    replica: &mut Replica,
    plan: &Plan,
    resolution: &Resolution,
    prepared: &Prepared,
) -> Result<()> {
    let root = replica.root()?.context("корень библиотеки не задан")?;
    let me = replica.device_id().to_string();

    for change in &plan.local {
        let decision = resolution.action(&change.path).unwrap_or(Decision::Keep);
        match change.kind {
            ChangeKind::Added => match decision {
                Decision::Revert | Decision::DeleteEverywhere => {
                    remove_to_vault(replica, &root, &change.path, &change.blob)?;
                }
                Decision::OnlyHere => {
                    remember_exclusion(replica, change)?;
                }
                _ => {}
            },
            ChangeKind::Updated => {
                if decision == Decision::Revert {
                    let src = staged_or_known(replica, &root, &change.base_blob)
                        .with_context(|| format!("нет базовой версии для {}", change.path))?;
                    write_blob(&root, &change.path, &src, change.mtime_ns)?;
                }
            }
            ChangeKind::Renamed | ChangeKind::Moved => {
                if decision == Decision::Revert {
                    let Some(from) = &change.from else { continue };
                    let src = resolve::disk_path(&root, &change.path);
                    let dst = resolve::disk_path(&root, from);
                    if src.is_file() && !dst.exists() {
                        if let Some(parent) = dst.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::rename(&src, &dst)?;
                        crate::scan::restore_mtime(&dst, change.mtime_ns)?;
                    }
                }
            }
            ChangeKind::Deleted => match decision {
                Decision::DeleteEverywhere => {
                    remove_to_vault(replica, &root, &change.path, &change.blob)?
                }
                Decision::KeepInLibrary => {
                    let restored = restore_from_vault(replica, &root, &change.path)?;
                    if !restored {
                        if let Some(src) = staged_or_known(replica, &root, &change.blob) {
                            write_blob(&root, &change.path, &src, change.mtime_ns)?;
                        } else {
                            bail!(
                                "«{}» не восстанавливается: нет ни копии в корзине, \
                                 ни блоба у пира",
                                change.path
                            );
                        }
                    }
                    // `EntryPut` из `prepared` переведёт путь в `Live`; файл
                    // уже на диске.
                    set_present(replica, &change.path, true)?;
                }
                Decision::OnlyHere => {
                    // Оставляем ожидание удаления: если человек вернёт путь в
                    // синхронизацию, скан уже будет знать, что файл исчез, и
                    // покажет удаление снова. Пока действует исключение, план
                    // этот путь не видит.
                    remember_exclusion(replica, change)?;
                }
                _ => {}
            },
        }
    }

    for conflict in &plan.conflicts {
        let decision = plan
            .explicit_entry(&conflict.path)
            .with_context(|| format!("конфликт без решения: {}", conflict.path))?;
        let action = resolution
            .action(&conflict.path)
            .with_context(|| format!("конфликт без решения: {}", conflict.path))?;
        let chooser = decision.dev == me;
        let local_deleted = conflict_local_deleted(replica, conflict)?;
        let remote_deleted = conflict.remote_blob.is_empty();
        let keep = conflict_keep(action, chooser, local_deleted, remote_deleted);
        let both_versions = action == Decision::KeepBoth && !local_deleted && !remote_deleted;

        if !both_versions && matches!(keep, ConflictKeep::Delete | ConflictKeep::Remote) {
            // Локальная версия уступает удалению или версии пира.
            let local = conflict_local_path(conflict);
            remove_to_vault(replica, &root, local, &conflict.local_blob)?;
            set_present(replica, local, false)?;
            if local != conflict.path {
                set_present(replica, &conflict.path, false)?;
            }
        }

        if both_versions && !chooser {
            // Проигравшая версия остаётся рядом под своим путём.
            let copy = keep_both_copy(plan, decision, &me);
            let local = conflict_local_path(conflict);
            let src = resolve::disk_path(&root, local);
            let dst = resolve::disk_path(&root, &copy);
            if src.is_file() && !dst.exists() {
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(&src, &dst)?;
            }
            set_present(replica, local, false)?;
            if local != conflict.path {
                set_present(replica, &conflict.path, false)?;
            }
        }
    }

    for change in &plan.remote {
        let decision = resolution.action(&change.path).unwrap_or(Decision::Keep);
        if decision != Decision::OnlyHere {
            continue;
        }
        if replica.exclusions_enabled() {
            // ПК ведёт список исключений сам. Чужую операцию по этому пути
            // не применяем (см. `finalize`), поэтому запись не появляется.
            remember_exclusion(replica, change)?;
        } else if change.kind == ChangeKind::Added {
            // Телефон не ведёт список исключений, но состояние пути общее:
            // путь просто перестаёт участвовать в синхронизации.
            exclude_change(replica, change, Side::Remote)?;
        }
    }

    // Новые операции (отмены, удаления, восстановления, конфликтные копии)
    // применяем у себя; у принятых добавлений/правок операция лишь повторяет
    // уже известное содержимое.
    replica.apply_all(&prepared.ops)?;

    // Флаги присутствия после применения операций. Сами состояния уже
    // переведены операциями (`EntryPut`/`EntryDel`/`EntryMove`).
    for change in &plan.local {
        let decision = resolution.action(&change.path).unwrap_or(Decision::Keep);
        match (change.kind, decision) {
            (ChangeKind::Updated, Decision::Revert) => {
                set_present(replica, &change.path, true)?;
            }
            (ChangeKind::Renamed | ChangeKind::Moved, Decision::Revert) => {
                if let Some(from) = &change.from {
                    if resolve::disk_path(&root, from).is_file() {
                        set_present(replica, from, true)?;
                    }
                    set_present(replica, &change.path, false)?;
                }
            }
            _ => {}
        }
    }

    for conflict in &plan.conflicts {
        let Some(decision) = plan.explicit_entry(&conflict.path) else {
            continue;
        };
        if decision.action == Decision::KeepBoth && decision.dev != me {
            let copy = keep_both_copy(plan, decision, &me);
            if resolve::disk_path(&root, &copy).is_file() {
                set_present(replica, &copy, true)?;
            }
        }
    }

    Ok(())
}

/// Применить операции пира, разложить проверенные блобы и зафиксировать
/// согласованную базу только для путей, которые действительно приняты.
pub fn finalize(
    replica: &mut Replica,
    plan: &Plan,
    resolution: &Resolution,
    prepared: &Prepared,
    peer_ops: &[Op],
) -> Result<Outcome> {
    let root = replica.root()?.context("корень библиотеки не задан")?;
    let me = replica.device_id().to_string();

    // Возврат, который пользователь отклонил, снова становится исключением.
    // Запоминаем до применения операций пира: `EntryPut` может превратить
    // `ResurrectingRemote` в `Live` раньше, чем мы посмотрим на решение.
    let resurrecting_remote: HashSet<String> = replica
        .path_states()?
        .into_iter()
        .filter(|row| row.state == PathState::ResurrectingRemote)
        .map(|row| row.path)
        .collect();

    // Операции по исключённым путям не применяем: иначе чужое добавление
    // создало бы у нас запись, а чужое удаление убрало бы файл, который
    // человек решил оставить. Watermark по ним не двигается, поэтому после
    // отмены исключения пир пришлёт их снова.
    let excluded: HashSet<String> = replica
        .path_states()?
        .into_iter()
        .filter(|row| row.state.is_excluded())
        .map(|row| row.path)
        .collect();
    let peer_ops: Vec<Op> = peer_ops
        .iter()
        .filter(|op| !op_hits_exclusion(&op.kind, &excluded))
        .cloned()
        .collect();
    replica.apply_all(&peer_ops)?;
    // Удаления приходят операцией, но файл на диске убирает эта функция
    // (с копией в корзину). Заодно спасает локальные правки.
    resolve::prune_deleted_files(replica, &root)?;

    let mut confirmed: HashSet<String> = HashSet::new();
    let mut outcome = Outcome::default();

    for change in &plan.local {
        let decision = resolution.action(&change.path).unwrap_or(Decision::Keep);
        match (change.kind, decision) {
            (ChangeKind::Added, Decision::Accept) => {
                confirmed.insert(change.path.clone());
                outcome.accepted += 1;
            }
            (ChangeKind::Added, Decision::OnlyHere) => {
                // Файл остаётся только здесь и в общую базу не входит:
                // подтверждать его как согласованный нельзя.
                outcome.accepted += 1;
            }
            (ChangeKind::Added, Decision::Revert) => outcome.accepted += 1,
            (ChangeKind::Added, Decision::DeleteEverywhere) => outcome.accepted += 1,
            (ChangeKind::Updated, Decision::Accept | Decision::Revert) => {
                confirmed.insert(change.path.clone());
                outcome.accepted += 1;
            }
            (ChangeKind::Renamed | ChangeKind::Moved, Decision::Accept) => {
                confirmed.insert(change.path.clone());
                outcome.accepted += 1;
            }
            (ChangeKind::Renamed | ChangeKind::Moved, Decision::Revert) => {
                if let Some(from) = &change.from {
                    // Несогласованное добавление после возврата остаётся
                    // добавлением: подтверждать его как базу нельзя.
                    if prepared.move_sources_in_base.contains(from) {
                        confirmed.insert(from.clone());
                    }
                }
                outcome.accepted += 1;
            }
            (ChangeKind::Deleted, Decision::DeleteEverywhere) => outcome.accepted += 1,
            (ChangeKind::Deleted, Decision::KeepInLibrary) => {
                confirmed.insert(change.path.clone());
                outcome.accepted += 1;
            }
            (ChangeKind::Deleted, Decision::OnlyHere) => outcome.accepted += 1,
            _ => outcome.rejected += 1,
        }
    }

    for change in &plan.remote {
        let decision = resolution.action(&change.path).unwrap_or(Decision::Keep);
        match (change.kind, decision) {
            (ChangeKind::Deleted, Decision::DeleteEverywhere | Decision::KeepInLibrary) => {
                confirmed.insert(change.path.clone());
                outcome.accepted += 1;
            }
            (ChangeKind::Deleted, Decision::OnlyHere) => outcome.accepted += 1,
            (ChangeKind::Deleted, _) => outcome.rejected += 1,
            (ChangeKind::Added, Decision::DeleteEverywhere) => outcome.accepted += 1,
            (_, Decision::OnlyHere) => outcome.accepted += 1,
            (_, Decision::Accept) => {
                confirmed.insert(change.path.clone());
                outcome.accepted += 1;
            }
            _ => outcome.rejected += 1,
        }
    }

    // Отказ от возврата: путь снова исключён на стороне пира, а не висит в
    // `ResurrectingRemote`. `Accept` раскладывает копию и снимает маркер,
    // `OnlyHere` уже исключил путь в `apply_local`, `DeleteEverywhere` удаляет.
    for change in &plan.remote {
        if !resurrecting_remote.contains(&change.path) {
            continue;
        }
        let decision = resolution.action(&change.path).unwrap_or(Decision::Keep);
        if matches!(
            decision,
            Decision::Accept | Decision::OnlyHere | Decision::DeleteEverywhere
        ) {
            continue;
        }
        let Some(mut row) = replica.path_state(&change.path)? else {
            continue;
        };
        if matches!(row.state, PathState::Live | PathState::ResurrectingRemote) {
            row.state = PathState::ExcludedRemote;
            row.present = false;
            row.pending_hlc = None;
            row.created_at = None;
            replica.save_path_state(&row)?;
        }
    }

    for conflict in &plan.conflicts {
        let action = resolution
            .action(&conflict.path)
            .with_context(|| format!("конфликт без решения: {}", conflict.path))?;
        match action {
            Decision::Keep | Decision::Accept => {
                confirmed.insert(conflict.path.clone());
                outcome.accepted += 1;
            }
            Decision::KeepBoth => {
                let decision = plan
                    .explicit_entry(&conflict.path)
                    .with_context(|| format!("конфликт без решения: {}", conflict.path))?;
                confirmed.insert(conflict.path.clone());
                // Копия есть на обеих сторонах, только если сохранять есть
                // что с обеих: удалённая сторона вырождает решение в Keep или
                // Accept.
                let local_deleted = conflict_local_deleted(replica, conflict)?;
                if !local_deleted && !conflict.remote_blob.is_empty() {
                    confirmed.insert(keep_both_copy(plan, decision, &me));
                }
                outcome.accepted += 1;
            }
            other => bail!(
                "решение {other:?} не поддерживается для конфликта {}",
                conflict.path
            ),
        }
    }

    // Раскладываем только то, что действительно могло измениться на диске:
    // принятые чужие добавления/правки/переносы и выбранные версии конфликтов.
    // Совпавшие файлы не хэшируем повторно — иначе первый коммит заново
    // перечитывал бы всю библиотеку.
    let mut place: HashSet<String> = HashSet::new();
    for change in &plan.remote {
        let decision = resolution.action(&change.path).unwrap_or(Decision::Keep);
        if matches!(
            change.kind,
            ChangeKind::Added
                | ChangeKind::Updated
                | ChangeKind::Renamed
                | ChangeKind::Moved
        ) && decision == Decision::Accept
        {
            place.insert(change.path.clone());
        }
    }
    for conflict in &plan.conflicts {
        let Some(action) = resolution.action(&conflict.path) else {
            continue;
        };
        place.insert(conflict.path.clone());
        if action == Decision::KeepBoth {
            let local_deleted = conflict_local_deleted(replica, conflict)?;
            if !local_deleted && !conflict.remote_blob.is_empty() {
                if let Some(decision) = plan.explicit_entry(&conflict.path) {
                    place.insert(keep_both_copy(plan, decision, &me));
                }
            }
        }
    }

    // Конфликт — это комбинация `path_state` и неприменённых операций пира,
    // а не отдельное состояние. После применения операций фиксируем
    // выбранную версию явно: LWW не применяет put к `Missing` с другим
    // блобом, хотя решение человека этот конфликт уже разрешило.
    for conflict in &plan.conflicts {
        let decision = plan
            .explicit_entry(&conflict.path)
            .with_context(|| format!("конфликт без решения: {}", conflict.path))?;
        let action = resolution
            .action(&conflict.path)
            .with_context(|| format!("конфликт без решения: {}", conflict.path))?;
        let chooser = decision.dev == me;
        let local_deleted = conflict_local_deleted(replica, conflict)?;
        let remote_deleted = conflict.remote_blob.is_empty();
        let keep = conflict_keep(action, chooser, local_deleted, remote_deleted);
        let path = &conflict.path;
        let mut row = match replica.path_state(path)? {
            Some(row) => row,
            None => PathStateRow {
                path: path.clone(),
                state: PathState::Live,
                base: None,
                blob: String::new(),
                size: 0,
                mtime_ns: 0,
                added_at: Hlc::wall_ms(),
                hlc: decision.hlc,
                dev: decision.dev.clone(),
                present: false,
                pending_hlc: None,
                created_at: None,
            },
        };
        match keep {
            ConflictKeep::Delete => {
                row.state = PathState::Tombstone;
                row.present = false;
                row.pending_hlc = None;
                row.created_at = None;
            }
            ConflictKeep::Local => {
                if row.blob != conflict.local_blob {
                    row.mtime_ns = Hlc::wall_ms() as i64;
                }
                row.state = PathState::Live;
                row.blob = conflict.local_blob.clone();
                row.size = conflict.local_size;
                row.base = Some(conflict.local_blob.clone());
                row.pending_hlc = None;
                row.created_at = None;
            }
            ConflictKeep::Remote => {
                if row.blob != conflict.remote_blob {
                    row.mtime_ns = Hlc::wall_ms() as i64;
                }
                row.state = PathState::Live;
                row.blob = conflict.remote_blob.clone();
                row.size = conflict.remote_size;
                row.base = Some(conflict.remote_blob.clone());
                row.present = false;
                row.pending_hlc = None;
                row.created_at = None;
            }
        }
        replica.save_path_state(&row)?;
    }

    for rel in place {
        if !confirmed.contains(&rel) {
            continue;
        }
        let Some(mut row) = replica.path_state(&rel)? else {
            continue;
        };
        if row.state.is_excluded() || row.state.is_tombstone() {
            continue;
        }
        let dst = resolve::disk_path(&root, &rel);
        let disk_blob = dst
            .is_file()
            .then(|| crate::scan::hash_file(&dst).ok())
            .flatten();
        if disk_blob.as_deref() == Some(row.blob.as_str()) {
            // Содержимое уже на месте: путь больше не ждёт удаления и не
            // нуждается в возврате с другой стороны.
            row.present = true;
            row.pending_hlc = None;
            if row.state.is_resurrecting() {
                row.state = PathState::Live;
                row.created_at = None;
            }
            replica.save_path_state(&row)?;
            continue;
        }
        let Some(src) = blob_source(replica, &root, &row.blob)? else {
            bail!("не удалось получить содержимое «{rel}» ({})", row.blob);
        };
        // Файл на диске не совпадает с согласованной базой — значит, его
        // изменили уже после скана. Не теряем такую правку.
        if let Some(existing_blob) = disk_blob {
            if row.base.as_deref() != Some(existing_blob.as_str()) {
                save_conflict_copy(replica, &root, &dst, &rel)?;
            }
        }
        write_blob(&root, &rel, &src, row.mtime_ns)?;
        row.present = true;
        row.pending_hlc = None;
        if row.state.is_resurrecting() {
            row.state = PathState::Live;
            row.created_at = None;
        }
        replica.save_path_state(&row)?;
    }

    // Принятое содержимое уже разложено по диску: маркер возврата для таких
    // путей больше не нужен.
    for path in &confirmed {
        clear_resurrection_marker(replica, path)?;
    }

    let paths: Vec<String> = confirmed.into_iter().collect();
    replica.confirm_synced_paths(&paths)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{self, PathDecision};
    use crate::store::PathState;
    use std::collections::HashSet;
    use std::fs;
    use tempfile::TempDir;

    struct Fixture {
        _tmp: TempDir,
        root: PathBuf,
        replica: Replica,
    }

    fn fixture(files: &[(&str, &[u8])]) -> Fixture {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("music");
        fs::create_dir_all(&root).unwrap();
        for (rel, bytes) in files {
            let p = root.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(p, bytes).unwrap();
        }
        let mut replica = Replica::open(&tmp.path().join("state"), "dev-a").unwrap();
        replica.set_root(&root).unwrap();
        replica.enable_exclusions();
        crate::scan::scan(&root, &mut replica).unwrap();
        Fixture {
            _tmp: tmp,
            root,
            replica,
        }
    }

    fn blob_of(bytes: &[u8]) -> String {
        format!("b3:{}", blake3::hash(bytes).to_hex())
    }

    fn peer_put(path: &str, bytes: &[u8], offset_ms: u64) -> Op {
        let mut peer = Replica::in_memory("dev-b").unwrap();
        peer.make_op_at(
            Hlc::new(Hlc::wall_ms() + offset_ms, 0),
            OpKind::EntryPut {
                path: path.into(),
                blob: blob_of(bytes),
                size: bytes.len() as u64,
                mtime_ns: 1,
                added_at: 1,
            },
        )
        .unwrap()
    }

    fn peer_del(path: &str, offset_ms: u64) -> Op {
        let mut peer = Replica::in_memory("dev-b").unwrap();
        peer.make_op_at(
            Hlc::new(Hlc::wall_ms() + offset_ms, 0),
            OpKind::EntryDel { path: path.into() },
        )
        .unwrap()
    }

    /// Положить содержимое в `incoming` так, будто блоб уже скачан.
    fn stage(replica: &Replica, bytes: &[u8]) -> String {
        let blob = blob_of(bytes);
        let p = staged_path(replica, &blob).unwrap();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
        blob
    }

    fn state(f: &Fixture, path: &str) -> PathStateRow {
        f.replica
            .path_state(path)
            .unwrap()
            .unwrap_or_else(|| panic!("нет состояния для {path}"))
    }

    /// Инварианты раздела 4, которые должны держаться после каждого commit.
    fn assert_invariants(replica: &Replica) {
        let mut paths = HashSet::new();
        for row in replica.path_states().unwrap() {
            assert!(
                paths.insert(row.path.clone()),
                "путь повторяется в path_state: {}",
                row.path
            );
            if row.pending_hlc.is_some() {
                assert_eq!(
                    row.state,
                    PathState::Missing,
                    "pending_hlc допустим только у Missing: {}",
                    row.path
                );
            }
            match row.state {
                PathState::Missing => {
                    assert!(!row.present, "Missing не может быть present: {}", row.path);
                }
                PathState::ExcludedLocal | PathState::ResurrectingLocal => {
                    assert!(row.present, "локальное исключение требует present: {}", row.path);
                }
                PathState::ExcludedRemote | PathState::ResurrectingRemote => {
                    assert!(
                        !row.present,
                        "удалённое исключение требует present=0: {}",
                        row.path
                    );
                }
                PathState::Live | PathState::Tombstone => {
                    assert!(
                        row.created_at.is_none(),
                        "created_at только у исключений: {}",
                        row.path
                    );
                }
            }
        }

        // Tombstone не показывает удалённый payload в совместимой проекции.
        for entry in replica.entries().unwrap() {
            if entry.deleted {
                assert!(entry.blob.is_empty(), "tombstone с блобом: {}", entry.path);
                assert_eq!(entry.size, 0, "tombstone с размером: {}", entry.path);
                assert_eq!(entry.mtime_ns, 0, "tombstone с mtime: {}", entry.path);
                assert_eq!(entry.added_at, 0, "tombstone с added_at: {}", entry.path);
            }
        }

        // Проекции состояний взаимоисключающие.
        let pending: HashSet<String> = replica
            .pending_deletes()
            .unwrap()
            .into_iter()
            .map(|row| row.path)
            .collect();
        let excluded = replica.exclusion_paths().unwrap();
        let resurrecting = replica.resurrection_paths().unwrap();
        for path in &pending {
            assert!(
                !excluded.contains(path),
                "путь одновременно Missing и Excluded: {path}"
            );
            assert!(
                !resurrecting.contains(path),
                "путь одновременно Missing и Resurrecting: {path}"
            );
        }
        for path in &excluded {
            assert!(
                !resurrecting.contains(path),
                "путь одновременно Excluded и Resurrecting: {path}"
            );
        }
    }

    /// Прогнать commit: план строится по `preview_ops`, а в `finalize`
    /// приходят операции, которые пир подготовил после решения.
    fn run(
        f: &mut Fixture,
        preview_ops: &[Op],
        decision_ops: &[Op],
        decisions: &[(&str, Decision, &str)],
    ) -> Plan {
        let mut plan = plan::build(&f.replica, "dev-b", "Phone", preview_ops).unwrap();
        for (path, action, dev) in decisions {
            let blob = plan.blob_for(path).unwrap();
            plan.decisions.entries.insert(
                path.to_string(),
                PathDecision {
                    path: path.to_string(),
                    action: *action,
                    blob,
                    hlc: f.replica.next_hlc().unwrap(),
                    dev: dev.to_string(),
                },
            );
        }
        let resolution = plan.resolve();
        assert!(resolution.unresolved().is_empty());
        let prepared = prepare(&mut f.replica, &plan, &resolution).unwrap();
        apply_local(&mut f.replica, &plan, &resolution, &prepared).unwrap();
        finalize(&mut f.replica, &plan, &resolution, &prepared, decision_ops).unwrap();
        assert_invariants(&f.replica);
        plan
    }

    fn base(f: &mut Fixture, path: &str) {
        let blob = state(f, path).blob;
        let mut row = state(f, path);
        row.base = Some(blob);
        f.replica.save_path_state(&row).unwrap();
    }

    // ------------------------------------------------ решения по изменениям

    #[test]
    fn commit_added_accept_fixes_base() {
        let mut f = fixture(&[("track.flac", b"one")]);
        run(&mut f, &[], &[], &[("track.flac", Decision::Accept, "dev-a")]);

        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.base.as_deref(), Some(blob_of(b"one").as_str()));
        assert!(row.present);
    }

    #[test]
    fn commit_added_revert_tombstones_and_vaults() {
        let mut f = fixture(&[("track.flac", b"one")]);
        run(&mut f, &[], &[], &[("track.flac", Decision::Revert, "dev-a")]);

        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Tombstone);
        assert!(!row.present);
        assert!(!f.root.join("track.flac").exists());
        assert!(f.replica.vault_for_path("track.flac").unwrap().is_some());
    }

    #[test]
    fn commit_updated_accept_and_revert() {
        let mut f = fixture(&[("track.flac", b"one")]);
        base(&mut f, "track.flac");
        fs::write(f.root.join("track.flac"), b"two").unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();

        let plan = run(
            &mut f,
            &[],
            &[],
            &[("track.flac", Decision::Accept, "dev-a")],
        );
        assert_eq!(plan.local[0].kind, ChangeKind::Updated);
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.blob, blob_of(b"two"));
        assert_eq!(row.base, Some(blob_of(b"two")));

        let mut f = fixture(&[("track.flac", b"one")]);
        base(&mut f, "track.flac");
        fs::write(f.root.join("track.flac"), b"two").unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();
        stage(&f.replica, b"one");
        run(
            &mut f,
            &[],
            &[],
            &[("track.flac", Decision::Revert, "dev-a")],
        );

        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.blob, blob_of(b"one"));
        assert_eq!(row.base, Some(blob_of(b"one")));
        assert_eq!(fs::read(f.root.join("track.flac")).unwrap(), b"one");
    }

    #[test]
    fn commit_renamed_accept_and_revert() {
        let mut f = fixture(&[("old.flac", b"one")]);
        base(&mut f, "old.flac");
        fs::rename(f.root.join("old.flac"), f.root.join("new.flac")).unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();

        run(&mut f, &[], &[], &[("new.flac", Decision::Accept, "dev-a")]);
        let target = state(&f, "new.flac");
        assert_eq!(target.state, PathState::Live);
        assert_eq!(target.base, Some(blob_of(b"one")));
        assert!(target.present);
        assert_eq!(state(&f, "old.flac").state, PathState::Tombstone);
        assert!(f.root.join("new.flac").is_file());

        let mut f = fixture(&[("old.flac", b"one")]);
        base(&mut f, "old.flac");
        fs::rename(f.root.join("old.flac"), f.root.join("new.flac")).unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();

        run(&mut f, &[], &[], &[("new.flac", Decision::Revert, "dev-a")]);
        let source = state(&f, "old.flac");
        assert_eq!(source.state, PathState::Live);
        assert_eq!(source.base, Some(blob_of(b"one")));
        assert!(source.present);
        assert_eq!(state(&f, "new.flac").state, PathState::Tombstone);
        assert!(f.root.join("old.flac").is_file());
        assert!(!f.root.join("new.flac").exists());
    }

    #[test]
    fn rejecting_a_move_of_an_unsynced_addition_keeps_it_pending() {
        let mut f = fixture(&[("old.flac", b"one")]);
        // Базы нет: добавление ещё не синхронизировали.
        fs::rename(f.root.join("old.flac"), f.root.join("new.flac")).unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();

        let plan = run(
            &mut f,
            &[],
            &[],
            &[("new.flac", Decision::Revert, "dev-a")],
        );
        assert_eq!(plan.local[0].kind, ChangeKind::Renamed);

        let source = state(&f, "old.flac");
        assert_eq!(source.state, PathState::Live);
        assert_eq!(
            source.base, None,
            "несогласованное добавление не должно стать общей базой"
        );
        assert!(source.present);
        assert_eq!(state(&f, "new.flac").state, PathState::Tombstone);
        assert!(f.root.join("old.flac").is_file());

        // Следующая проверка снова показывает файл как добавление, а не
        // обратный перенос из отклонённого пути.
        let next = plan::build(&f.replica, "dev-b", "Phone", &[]).unwrap();
        assert_eq!(next.local.len(), 1);
        assert_eq!(next.local[0].path, "old.flac");
        assert_eq!(next.local[0].kind, ChangeKind::Added);
        assert_eq!(next.local[0].from, None);
    }

    #[test]
    fn commit_deleted_decisions() {
        // Удалить везде.
        let mut f = fixture(&[("track.flac", b"one")]);
        base(&mut f, "track.flac");
        fs::remove_file(f.root.join("track.flac")).unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();
        run(
            &mut f,
            &[],
            &[],
            &[("track.flac", Decision::DeleteEverywhere, "dev-a")],
        );
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Tombstone);
        assert!(!row.present);
        // Локальный файл уже удалён человеком: в корзину здесь класть нечего,
        // копию сохранит сторона, у которой файл ещё есть.

        // Оставить в библиотеке.
        let mut f = fixture(&[("track.flac", b"one")]);
        base(&mut f, "track.flac");
        fs::remove_file(f.root.join("track.flac")).unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();
        stage(&f.replica, b"one");
        run(
            &mut f,
            &[],
            &[],
            &[("track.flac", Decision::KeepInLibrary, "dev-a")],
        );
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.base, Some(blob_of(b"one")));
        assert!(row.present);
        assert_eq!(fs::read(f.root.join("track.flac")).unwrap(), b"one");

        // Только здесь (файл остался на пире).
        let mut f = fixture(&[("track.flac", b"one")]);
        base(&mut f, "track.flac");
        fs::remove_file(f.root.join("track.flac")).unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();
        run(
            &mut f,
            &[],
            &[],
            &[("track.flac", Decision::OnlyHere, "dev-a")],
        );
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::ExcludedRemote);
        assert!(!row.present);
        assert!(row.pending_hlc.is_none());
    }

    // ------------------------------------------------------------ конфликты

    #[test]
    fn commit_no_base_keep_accept_and_keep_both() {
        // Keep: остаётся локальная версия, пир получает revert-put.
        let mut f = fixture(&[("track.flac", b"local")]);
        let remote = peer_put("track.flac", b"remote", 1000);
        run(
            &mut f,
            &[remote.clone()],
            &[],
            &[("track.flac", Decision::Keep, "dev-a")],
        );
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.blob, blob_of(b"local"));
        assert_eq!(row.base, Some(blob_of(b"local")));
        assert_eq!(fs::read(f.root.join("track.flac")).unwrap(), b"local");

        // Accept: локальная версия уступает версии пира.
        let mut f = fixture(&[("track.flac", b"local")]);
        let remote = peer_put("track.flac", b"remote", 1000);
        stage(&f.replica, b"remote");
        run(
            &mut f,
            &[remote.clone()],
            &[],
            &[("track.flac", Decision::Accept, "dev-a")],
        );
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.blob, blob_of(b"remote"));
        assert_eq!(row.base, Some(blob_of(b"remote")));
        assert!(row.present);
        assert_eq!(fs::read(f.root.join("track.flac")).unwrap(), b"remote");

        // KeepBoth: решение принял пир, локальная версия уходит в копию.
        let mut f = fixture(&[("track.flac", b"local")]);
        let remote = peer_put("track.flac", b"remote", 1000);
        stage(&f.replica, b"remote");
        let plan = run(
            &mut f,
            &[remote.clone()],
            &[remote],
            &[("track.flac", Decision::KeepBoth, "dev-b")],
        );
        let decision = plan.explicit_entry("track.flac").unwrap();
        let copy = keep_both_copy(&plan, decision, "dev-a");
        let original = state(&f, "track.flac");
        assert_eq!(original.state, PathState::Live);
        assert_eq!(original.blob, blob_of(b"remote"));
        assert_eq!(original.base, Some(blob_of(b"remote")));
        let copy_row = state(&f, &copy);
        assert_eq!(copy_row.state, PathState::Live);
        assert_eq!(copy_row.blob, blob_of(b"local"));
        assert_eq!(copy_row.base, Some(blob_of(b"local")));
        assert_eq!(fs::read(f.root.join("track.flac")).unwrap(), b"remote");
        assert_eq!(fs::read(f.root.join(&copy)).unwrap(), b"local");
    }

    #[test]
    fn commit_both_changed_keep_accept_and_keep_both() {
        let mut f = fixture(&[("track.flac", b"base")]);
        base(&mut f, "track.flac");
        fs::write(f.root.join("track.flac"), b"local").unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();
        let remote = peer_put("track.flac", b"remote", 1000);
        let plan = run(
            &mut f,
            &[remote.clone()],
            &[],
            &[("track.flac", Decision::Keep, "dev-a")],
        );
        assert_eq!(plan.conflicts[0].reason, plan::ConflictReason::BothChanged);
        let row = state(&f, "track.flac");
        assert_eq!(row.blob, blob_of(b"local"));
        assert_eq!(row.base, Some(blob_of(b"local")));

        let mut f = fixture(&[("track.flac", b"base")]);
        base(&mut f, "track.flac");
        fs::write(f.root.join("track.flac"), b"local").unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();
        let remote = peer_put("track.flac", b"remote", 1000);
        stage(&f.replica, b"remote");
        run(
            &mut f,
            &[remote.clone()],
            &[],
            &[("track.flac", Decision::Accept, "dev-a")],
        );
        let row = state(&f, "track.flac");
        assert_eq!(row.blob, blob_of(b"remote"));
        assert_eq!(row.base, Some(blob_of(b"remote")));
        assert!(row.present);
        assert_eq!(fs::read(f.root.join("track.flac")).unwrap(), b"remote");

        let mut f = fixture(&[("track.flac", b"base")]);
        base(&mut f, "track.flac");
        fs::write(f.root.join("track.flac"), b"local").unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();
        let remote = peer_put("track.flac", b"remote", 1000);
        stage(&f.replica, b"remote");
        let plan = run(
            &mut f,
            &[remote.clone()],
            &[remote],
            &[("track.flac", Decision::KeepBoth, "dev-b")],
        );
        let decision = plan.explicit_entry("track.flac").unwrap();
        let copy = keep_both_copy(&plan, decision, "dev-a");
        assert_eq!(state(&f, "track.flac").blob, blob_of(b"remote"));
        assert_eq!(state(&f, &copy).blob, blob_of(b"local"));
        assert_eq!(fs::read(f.root.join("track.flac")).unwrap(), b"remote");
        assert_eq!(fs::read(f.root.join(&copy)).unwrap(), b"local");
    }

    #[test]
    fn commit_changed_vs_deleted_local_changed_keep_and_accept() {
        // Keep: локальная правка побеждает удаление пира.
        let mut f = fixture(&[("track.flac", b"base")]);
        base(&mut f, "track.flac");
        fs::write(f.root.join("track.flac"), b"local").unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();
        let del = peer_del("track.flac", 1000);
        let plan = run(
            &mut f,
            &[del.clone()],
            &[],
            &[("track.flac", Decision::Keep, "dev-a")],
        );
        assert_eq!(
            plan.conflicts[0].reason,
            plan::ConflictReason::ChangedVsDeleted
        );
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.blob, blob_of(b"local"));
        assert_eq!(row.base, Some(blob_of(b"local")));
        assert!(f.root.join("track.flac").is_file());

        // Accept: принимаем удаление, локальная версия в корзине.
        let mut f = fixture(&[("track.flac", b"base")]);
        base(&mut f, "track.flac");
        fs::write(f.root.join("track.flac"), b"local").unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();
        let del = peer_del("track.flac", 1000);
        run(
            &mut f,
            &[del.clone()],
            &[],
            &[("track.flac", Decision::Accept, "dev-a")],
        );
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Tombstone);
        assert!(!row.present);
        assert!(!f.root.join("track.flac").exists());
        assert!(f.replica.vault_for_path("track.flac").unwrap().is_some());
    }

    #[test]
    fn commit_changed_vs_deleted_local_deleted_keep_and_accept() {
        // Keep: локальное удаление побеждает правку пира.
        let mut f = fixture(&[("track.flac", b"base")]);
        base(&mut f, "track.flac");
        fs::remove_file(f.root.join("track.flac")).unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();
        let remote = peer_put("track.flac", b"remote", 1000);
        let plan = run(
            &mut f,
            &[remote.clone()],
            &[],
            &[("track.flac", Decision::Keep, "dev-a")],
        );
        assert_eq!(
            plan.conflicts[0].reason,
            plan::ConflictReason::ChangedVsDeleted
        );
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Tombstone);
        assert!(!row.present);
        assert!(!f.root.join("track.flac").exists());

        // Accept: берём версию пира, файл возвращается.
        let mut f = fixture(&[("track.flac", b"base")]);
        base(&mut f, "track.flac");
        fs::remove_file(f.root.join("track.flac")).unwrap();
        crate::scan::scan(&f.root, &mut f.replica).unwrap();
        let remote = peer_put("track.flac", b"remote", 1000);
        stage(&f.replica, b"remote");
        run(
            &mut f,
            &[remote.clone()],
            &[],
            &[("track.flac", Decision::Accept, "dev-a")],
        );
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.blob, blob_of(b"remote"));
        assert_eq!(row.base, Some(blob_of(b"remote")));
        assert!(row.present);
        assert_eq!(fs::read(f.root.join("track.flac")).unwrap(), b"remote");
    }

    // ------------------------------------------------------- возврат (раздел 5.5)

    #[test]
    fn commit_resurrecting_local_accept_and_revert() {
        // Accept: возвращённая копия становится согласованной базой.
        let mut f = fixture(&[("track.flac", b"one")]);
        let blob = blob_of(b"one");
        let row = state(&f, "track.flac");
        f.replica
            .add_resurrection("track.flac", "local", &blob, row.size, row.mtime_ns)
            .unwrap();
        let plan = run(&mut f, &[], &[], &[("track.flac", Decision::Accept, "dev-a")]);
        assert_eq!(plan.local[0].kind, ChangeKind::Added);
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.base.as_deref(), Some(blob.as_str()));
        assert!(row.present);
        assert!(f.root.join("track.flac").is_file());

        // Revert: файл снова удаляется и попадает в корзину.
        let mut f = fixture(&[("track.flac", b"one")]);
        let blob = blob_of(b"one");
        let row = state(&f, "track.flac");
        f.replica
            .add_resurrection("track.flac", "local", &blob, row.size, row.mtime_ns)
            .unwrap();
        run(&mut f, &[], &[], &[("track.flac", Decision::Revert, "dev-a")]);
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Tombstone);
        assert!(!row.present);
        assert!(!f.root.join("track.flac").exists());
        assert!(f.replica.vault_for_path("track.flac").unwrap().is_some());
    }

    #[test]
    fn commit_resurrecting_remote_accept_and_reject() {
        // Accept: копия скачивается, путь снова живой.
        let mut f = fixture(&[]);
        let blob = blob_of(b"one");
        f.replica
            .add_exclusion("track.flac", "remote", &blob, 3, 1)
            .unwrap();
        let out = resolve::undo_exclusion(&mut f.replica, "track.flac", None).unwrap();
        assert!(out.waiting_remote);
        stage(&f.replica, b"one");
        let plan = run(&mut f, &[], &[], &[("track.flac", Decision::Accept, "dev-a")]);
        assert_eq!(plan.remote[0].kind, ChangeKind::Added);
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.base.as_deref(), Some(blob.as_str()));
        assert!(row.present);
        assert_eq!(fs::read(f.root.join("track.flac")).unwrap(), b"one");

        // Keep: возврат отклонён, путь снова исключён и не висит в ожидании.
        let mut f = fixture(&[]);
        let blob = blob_of(b"one");
        f.replica
            .add_exclusion("track.flac", "remote", &blob, 3, 1)
            .unwrap();
        resolve::undo_exclusion(&mut f.replica, "track.flac", None).unwrap();
        run(&mut f, &[], &[], &[("track.flac", Decision::Keep, "dev-a")]);
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::ExcludedRemote);
        assert!(!row.present);
        assert!(row.created_at.is_none());
        assert!(!f.root.join("track.flac").exists());
    }
}
