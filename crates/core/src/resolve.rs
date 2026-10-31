//! Разрешение ожидающих удалений — единственное место, где рождается `entry_del`.
//!
//! Удаление файла на устройстве ничего не реплицирует: сканер кладёт запись в
//! `pending_deletes`. Операция удаления появляется только после явного решения
//! пользователя. Поэтому «главное устройство» не нужно: приоритет у решения
//! человека, а не у железа.
//!
//! Решение принимается по **блобам**, а не по путям. Если трек на другом
//! устройстве успели перенести в другую папку, «удалить везде» всё равно
//! удалит все его копии, а «только здесь» не тронет общую библиотеку.

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::op::{Op, OpKind};
use crate::scan;
use crate::store::Replica;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Удалить трек из библиотеки на всех устройствах. Обратимо, пока копия
    /// жива в локальной корзине (`vault`).
    DeleteEverywhere,
    /// Убрать с этого устройства, но оставить в общей библиотеке.
    /// Обратимо через `restore`.
    OnlyHere,
}

#[derive(Debug, Default, Clone)]
pub struct Outcome {
    pub deleted_paths: Vec<String>,
    pub ignored_paths: Vec<String>,
    pub ops_emitted: usize,
    pub files_removed: usize,
    /// Копии, которые удалить не удалось (нет прав, файл занят).
    pub failed: Vec<(String, String)>,
}

/// Применяет решение к ожидающей записи.
pub fn resolve(replica: &mut Replica, path: &str, action: Action) -> Result<Outcome> {
    let pending = replica
        .pending_deletes()?
        .into_iter()
        .find(|p| p.path == path)
        .with_context(|| format!("нет ожидающего удаления для {path}"))?;

    let root = replica.root()?.context("корень библиотеки не задан")?;

    match action {
        Action::DeleteEverywhere => delete_everywhere(replica, &root, &pending.blob),
        Action::OnlyHere => only_here(replica, path, &root),
    }
}

fn delete_everywhere(replica: &mut Replica, root: &Path, blob: &str) -> Result<Outcome> {
    let mut out = Outcome::default();
    let targets = replica.live_paths_by_blob(blob)?;
    let mut failed: Vec<String> = Vec::new();

    for path in targets {
        // Сначала корзина: удаление должно быть обратимым. Если сохранить
        // копию не удалось, файл не трогаем — лучше ошибка, чем потеря.
        if let Err(e) = crate::vault::preserve_file(replica, root, &path) {
            out.failed.push((path.clone(), format!("корзина: {e:#}")));
            failed.push(path);
            continue;
        }

        let kind = OpKind::EntryDel { path: path.clone() };
        let op = Op::new(
            replica.device_id().to_string(),
            replica.next_hlc()?,
            Op::canonicalize(kind)?,
        );
        replica.apply(&op)?;
        replica.clear_pending_delete(&path)?;
        out.deleted_paths.push(path.clone());
        out.ops_emitted += 1;

        match remove_file(root, &path) {
            Ok(()) => out.files_removed += 1,
            Err(e) => out.failed.push((path, format!("{e:#}"))),
        }
    }

    // Ожидания, указывавшие на этот блоб под другими путями, больше не нужны.
    // Неудачные пути оставляем в очереди — их можно повторить.
    for p in replica.pending_deletes()? {
        if p.blob == blob && !failed.contains(&p.path) {
            replica.clear_pending_delete(&p.path)?;
        }
    }

    Ok(out)
}

fn only_here(replica: &mut Replica, path: &str, root: &Path) -> Result<Outcome> {
    let mut out = Outcome::default();
    replica.set_ignored(path, true)?;
    replica.clear_pending_delete(path)?;
    out.ignored_paths.push(path.to_string());

    // Файла уже нет (иначе он не был бы в ожидании), но если он здесь —
    // сначала в корзину, потом убираем из библиотеки.
    crate::vault::preserve_file(replica, root, path)?;
    if remove_file(root, path).is_ok() {
        out.files_removed += 1;
    }
    Ok(out)
}

/// Возвращает запись в локальную библиотеку. Файл докачается при следующем
/// обмене с тем, у кого он есть; если запись удалена везде — берём копию
/// из корзины.
pub fn restore(replica: &mut Replica, path: &str) -> Result<()> {
    match replica.entry(path)? {
        Some(e) if e.deleted => crate::vault::restore(replica, path),
        Some(_) => {
            replica.set_ignored(path, false)?;
            replica.mark_absent(path)?;
            replica.clear_pending_delete(path)?;
            Ok(())
        }
        None => bail!("путь не найден в библиотеке: {path}"),
    }
}

/// Удаляет файл и опустевшие каталоги. Отсутствие файла — не ошибка.
fn remove_file(root: &Path, rel: &str) -> Result<()> {
    let abs = disk_path(root, rel);
    match std::fs::remove_file(&abs) {
        Ok(()) => {
            prune_empty_dirs(root, abs.parent());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("удаление {}", abs.display())),
    }
}

/// Не оставляем пустые папки от удалённых альбомов, но не трогаем корень.
pub(crate) fn prune_empty_dirs(root: &Path, mut dir: Option<&Path>) {
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) {
            return;
        }
        match std::fs::read_dir(d) {
            Ok(mut it) => {
                if it.next().is_some() {
                    return;
                }
            }
            Err(_) => return,
        }
        if std::fs::remove_dir(d).is_err() {
            return;
        }
        dir = d.parent();
    }
}

/// Доводит подтверждённые удаления до диска и спасает локальные правки.
///
/// Вызывается после применения чужих операций. Если файл на диске не совпадает
/// с удаляемым блобом (его правили здесь и ещё не просканировали), он не
/// удаляется, а сохраняется как conflict-copy — «никогда не терять аудио».
pub fn prune_deleted_files(replica: &mut Replica, root: &Path) -> Result<Vec<String>> {
    let mut saved = Vec::new();

    let rows = replica.deleted_present()?;

    for (path, blob) in rows {
        let abs = disk_path(root, &path);
        if !abs.is_file() {
            replica.mark_absent(&path)?;
            continue;
        }
        let on_disk = scan::hash_file(&abs);
        match on_disk {
            Ok(h) if h == blob => {
                // Удаление пришло от другого устройства — сохраняем копию в
                // локальную корзину, чтобы его можно было отменить.
                if let Err(e) = crate::vault::preserve_file(replica, root, &path) {
                    tracing::warn!("не удалось положить {path} в корзину: {e:#}");
                    continue;
                }
                std::fs::remove_file(&abs)
                    .with_context(|| format!("удаление {}", abs.display()))?;
                replica.mark_absent(&path)?;
                prune_empty_dirs(root, abs.parent());
            }
            Ok(_) => {
                // Локальная версия отличается — сохраняем её отдельным путём.
                let conflict =
                    crate::path::conflict_name(&path, replica.device_id(), crate::Hlc::wall_ms());
                let dst = disk_path(root, &conflict);
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(&abs, &dst)
                    .with_context(|| format!("сохранение конфликтной копии {}", dst.display()))?;
                let meta = std::fs::metadata(&dst)?;
                let kind = OpKind::EntryPut {
                    path: conflict.clone(),
                    blob: scan::hash_file(&dst)?,
                    size: meta.len(),
                    mtime_ns: scan::mtime_ns_public(&meta),
                    added_at: replica.next_hlc()?.ms,
                };
                let op = Op::new(
                    replica.device_id().to_string(),
                    replica.next_hlc()?,
                    Op::canonicalize(kind)?,
                );
                replica.apply(&op)?;
                replica.mark_absent(&path)?;
                saved.push(conflict);
            }
            Err(_) => {
                // Не читается (занят плеером) — не трогаем, попробуем позже.
            }
        }
    }

    Ok(saved)
}

pub fn disk_path(root: &Path, rel: &str) -> std::path::PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split('/') {
        p.push(part);
    }
    p
}
