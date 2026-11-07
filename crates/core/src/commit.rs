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
use crate::store::Replica;
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
    for entry in replica.local_entries()? {
        if entry.blob == blob {
            let p = resolve::disk_path(root, &entry.path);
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
        .entry(path)?
        .map(|e| (e.size, e.mtime_ns, e.added_at)))
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

/// Построить исходящие операции и список блобов, которые надо докачать.
pub fn prepare(replica: &mut Replica, plan: &Plan, resolution: &Resolution) -> Result<Prepared> {
    let root = replica.root()?.context("корень библиотеки не задан")?;
    let me = replica.device_id().to_string();

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
                    Decision::Revert => (change.path.clone(), from.clone()),
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
        match action {
            Decision::Keep | Decision::Accept => {
                let keep_local = (action == Decision::Keep && chooser)
                    || (action == Decision::Accept && !chooser);
                if !keep_local && !has_blob(replica, &root, &conflict.remote_blob) {
                    need(
                        &mut prepared,
                        &mut seen,
                        &conflict.remote_blob,
                        conflict.remote_size,
                    );
                }
                // Решение разрешает конфликт для обеих сторон; операцию
                // создаёт только автор решения, иначе HLC-гонка.
                if chooser {
                    let (blob, size, mtime_ns) = if keep_local {
                        let meta = entry_meta(replica, conflict_local_path(conflict))?
                            .or(entry_meta(replica, &conflict.path)?)
                            .unwrap_or((
                                conflict.local_size,
                                Hlc::wall_ms() as i64,
                                Hlc::wall_ms(),
                            ));
                        (conflict.local_blob.clone(), meta.0, meta.1)
                    } else {
                        (
                            conflict.remote_blob.clone(),
                            conflict.remote_size,
                            Hlc::wall_ms() as i64,
                        )
                    };
                    anyhow::ensure!(
                        !blob.is_empty(),
                        "у конфликта «{}» нет выбранной версии",
                        conflict.path
                    );
                    prepared.ops.push(put_op(
                        replica,
                        &conflict.path,
                        &blob,
                        size,
                        mtime_ns,
                        Hlc::wall_ms(),
                    )?);
                }
            }
            Decision::KeepBoth => {
                if !has_blob(replica, &root, &conflict.remote_blob) {
                    need(
                        &mut prepared,
                        &mut seen,
                        &conflict.remote_blob,
                        conflict.remote_size,
                    );
                }
                let meta = entry_meta(replica, &conflict.path)?.unwrap_or((
                    conflict.local_size,
                    Hlc::wall_ms() as i64,
                    Hlc::wall_ms(),
                ));
                if chooser {
                    // Победитель остаётся на исходном пути.
                    prepared.ops.push(put_op(
                        replica,
                        &conflict.path,
                        &conflict.local_blob,
                        meta.0,
                        meta.1,
                        meta.2,
                    )?);
                } else {
                    // Проигравшая версия сохраняется рядом и уезжает пиру.
                    let copy = keep_both_copy(plan, decision, &me);
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
            other => bail!(
                "решение {other:?} не поддерживается для конфликта {}",
                conflict.path
            ),
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
    replica.mark_present(&conflict_rel)?;
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
fn remember_exclusion(replica: &mut Replica, change: &Change) -> Result<()> {
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
    let side = match side {
        Side::Local => "local",
        Side::Remote => "remote",
    };
    replica.add_exclusion(
        &change.path,
        side,
        &change.blob,
        change.size,
        change.mtime_ns,
    )?;
    // Прежнее локальное исключение больше не должно пережить отмену нового:
    // план теперь смотрит на таблицу исключений, а не на флаг записи.
    replica.clear_ignored(&change.path)?;
    Ok(())
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
                    replica.clear_pending_delete(&change.path)?;
                }
                Decision::OnlyHere => {
                    remember_exclusion(replica, change)?;
                    replica.mark_present(&change.path)?;
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
                    replica.set_ignored(&change.path, false)?;
                    replica.mark_present(&change.path)?;
                    replica.clear_pending_delete(&change.path)?;
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
        match action {
            Decision::Keep | Decision::Accept => {
                let keep_local = (action == Decision::Keep && chooser)
                    || (action == Decision::Accept && !chooser);
                if !keep_local {
                    // Чужую версию поставит операция автора решения.
                    let local = conflict_local_path(conflict);
                    remove_to_vault(replica, &root, local, &conflict.local_blob)?;
                    replica.mark_absent(local)?;
                    if local != conflict.path {
                        replica.mark_absent(&conflict.path)?;
                    }
                }
            }
            Decision::KeepBoth => {
                if !chooser {
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
                    replica.mark_absent(local)?;
                    if local != conflict.path {
                        replica.mark_absent(&conflict.path)?;
                    }
                }
            }
            other => bail!(
                "решение {other:?} не поддерживается для конфликта {}",
                conflict.path
            ),
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
            // Телефон остаётся минимальным агентом: старый механизм с
            // локальным флагом ему достаточно.
            replica.ignore_missing(&change.path, &change.blob, change.size, change.mtime_ns)?;
            replica.clear_pending_delete(&change.path)?;
        }
    }

    // Новые операции (отмены, удаления, восстановления, конфликтные копии)
    // применяем у себя; у принятых добавлений/правок операция лишь повторяет
    // уже известное содержимое.
    replica.apply_all(&prepared.ops)?;

    // Флаги присутствия после применения операций.
    for change in &plan.local {
        let decision = resolution.action(&change.path).unwrap_or(Decision::Keep);
        match (change.kind, decision) {
            (ChangeKind::Added, Decision::Revert) => {
                replica.clear_pending_delete(&change.path)?;
            }
            (ChangeKind::Updated, Decision::Revert) => {
                replica.mark_present(&change.path)?;
            }
            (ChangeKind::Renamed | ChangeKind::Moved, Decision::Revert) => {
                if let Some(from) = &change.from {
                    if resolve::disk_path(&root, from).is_file() {
                        replica.mark_present(from)?;
                    }
                    replica.mark_absent(&change.path)?;
                }
            }
            (ChangeKind::Deleted, Decision::DeleteEverywhere) => {
                replica.clear_pending_delete(&change.path)?;
            }
            (ChangeKind::Deleted, Decision::KeepInLibrary) => {
                replica.mark_present(&change.path)?;
                replica.set_ignored(&change.path, false)?;
                replica.clear_pending_delete(&change.path)?;
            }
            (ChangeKind::Deleted, Decision::OnlyHere) => {
                // Ожидание удаления намеренно остаётся: оно понадобится,
                // если человек вернёт путь в синхронизацию.
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
                replica.mark_present(&copy)?;
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
    peer_ops: &[Op],
) -> Result<Outcome> {
    let root = replica.root()?.context("корень библиотеки не задан")?;
    let me = replica.device_id().to_string();

    // Операции по исключённым путям не применяем: иначе чужое добавление
    // создало бы у нас запись, а чужое удаление убрало бы файл, который
    // человек решил оставить. Watermark по ним не двигается, поэтому после
    // отмены исключения пир пришлёт их снова.
    let excluded = replica.exclusion_paths()?;
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
                    confirmed.insert(from.clone());
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
                // Копия есть на обеих сторонах: у выбравшего это его версия,
                // у второго — версия пира.
                confirmed.insert(keep_both_copy(plan, decision, &me));
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
            if let Some(decision) = plan.explicit_entry(&conflict.path) {
                place.insert(keep_both_copy(plan, decision, &me));
            }
        }
    }

    for rel in place {
        if !confirmed.contains(&rel) || replica.is_ignored(&rel)? {
            continue;
        }
        let Some(entry) = replica.entry(&rel)? else {
            continue;
        };
        if entry.deleted {
            continue;
        }
        let dst = resolve::disk_path(&root, &rel);
        let disk_blob = dst
            .is_file()
            .then(|| crate::scan::hash_file(&dst).ok())
            .flatten();
        if disk_blob.as_deref() == Some(entry.blob.as_str()) {
            if !replica.is_present(&rel)? {
                replica.mark_present(&rel)?;
            }
            // Содержимое уже на месте: путь больше не ждёт удаления и не
            // нуждается в возврате с другой стороны.
            replica.clear_pending_delete(&rel)?;
            replica.remove_resurrection(&rel)?;
            continue;
        }
        let Some(src) = blob_source(replica, &root, &entry.blob)? else {
            bail!("не удалось получить содержимое «{rel}» ({})", entry.blob);
        };
        // Файл на диске не совпадает с согласованной базой — значит, его
        // изменили уже после скана. Не теряем такую правку.
        if let Some(existing_blob) = disk_blob {
            let base = replica.synced_blob(&rel)?;
            if base.is_empty() || existing_blob != base {
                save_conflict_copy(replica, &root, &dst, &rel)?;
            }
        }
        write_blob(&root, &rel, &src, entry.mtime_ns)?;
        replica.mark_present(&rel)?;
        replica.clear_pending_delete(&rel)?;
        replica.remove_resurrection(&rel)?;
    }

    // Принятое содержимое уже разложено по диску: маркер возврата для таких
    // путей больше не нужен.
    for path in &confirmed {
        replica.remove_resurrection(path)?;
    }

    let paths: Vec<String> = confirmed.into_iter().collect();
    replica.confirm_synced_paths(&paths)?;
    Ok(outcome)
}
