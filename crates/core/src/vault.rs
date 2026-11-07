//! Корзина: удалённые треки остаются на диске как hardlink'и.
//!
//! На NTFS hardlink не занимает места: файл и его «копия» в корзине — это два
//! имени одного и того же inode. Поэтому «удалить везде» становится обратимым
//! без резервного копирования всей библиотеки.
//!
//! Корзина — локальная, как и `pending_deletes`: она не реплицируется. На другом
//! устройстве при получении удаления файл тоже попадает в его собственную
//! корзину (см. `resolve::prune_deleted_files`), поэтому восстановить трек можно
//! с любого устройства, у которого он был.
//!
//! Если hardlink невозможен (другой том), используется обычная копия.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::hlc::Hlc;
use crate::op::OpKind;
use crate::resolve::{disk_path, prune_empty_dirs};
use crate::scan;
use crate::store::{PathState, Replica, VaultRow};

/// Сколько дней хранить удалённое по умолчанию.
pub const DEFAULT_TTL_DAYS: u64 = 30;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcReport {
    pub rows_removed: usize,
    pub blobs_removed: usize,
    pub orphans_removed: usize,
}

/// Имя файла в корзине: `b3:ab12…` → `ab12…`. Двоеточие в имени файла
/// недопустимо на Windows, поэтому префикс отбрасываем.
fn blob_key(blob: &str) -> String {
    let hex = blob.strip_prefix("b3:").unwrap_or(blob);
    hex.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn vault_root(data_dir: &Path) -> PathBuf {
    data_dir.join("vault").join("blobs")
}

/// Путь, где лежит содержимое блоба в корзине.
pub fn vault_blob_path(data_dir: &Path, blob: &str) -> PathBuf {
    let key = blob_key(blob);
    let prefix = if key.len() >= 2 { &key[..2] } else { "00" };
    vault_root(data_dir).join(prefix).join(key)
}

/// Кладёт файл в корзину. Возвращает `None`, если корзины нет (реплика
/// в памяти) или файла уже нет на диске — удалять тогда нечего.
///
/// Ошибка означает, что сохранить копию не удалось, и удалять файл нельзя.
pub fn preserve_file(replica: &Replica, root: &Path, rel: &str) -> Result<Option<VaultRow>> {
    let Some(data_dir) = replica.data_dir() else {
        return Ok(None);
    };

    let abs = disk_path(root, rel);
    if !abs.is_file() {
        return Ok(None);
    }

    let meta =
        std::fs::metadata(&abs).with_context(|| format!("чтение метаданных {}", abs.display()))?;
    let blob = scan::hash_file(&abs)
        .with_context(|| format!("хеширование перед удалением {}", abs.display()))?;

    let dst = vault_blob_path(data_dir, &blob);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("создание {}", parent.display()))?;
    }
    if !dst.is_file() && std::fs::hard_link(&abs, &dst).is_err() {
        // Другой том или ФС без hardlink'ов — не теряем трек из-за этого.
        std::fs::copy(&abs, &dst)
            .with_context(|| format!("копирование в корзину {}", dst.display()))?;
    }

    // Метаданные записи нужны для восстановления; после tombstone они стираются,
    // поэтому по возможности берём их до применения `entry_del`.
    let (size, mtime_ns, added_at) = match replica.entry_raw(rel).ok().flatten() {
        Some((_blob, size, mtime_ns, added_at)) => (size, mtime_ns, added_at),
        None => (meta.len(), scan::mtime_ns_public(&meta), Hlc::wall_ms()),
    };

    let row = VaultRow {
        blob,
        path: rel.to_string(),
        size,
        mtime_ns,
        added_at,
        deleted_at: Hlc::wall_ms(),
    };
    replica.add_vault_row(&row)?;
    Ok(Some(row))
}

/// Возвращает удалённый трек из корзины и снова делает его частью библиотеки.
///
/// Новая `entry_put` уедет к пирам: удаление было таким же обычным LWW-фактом,
/// значит восстановление — законная победа более новой операции.
pub fn restore(replica: &mut Replica, path: &str) -> Result<()> {
    // Восстановление пишет файл и журнал; скан или commit в другом соединении
    // не должны увидеть библиотеку посередине.
    let lock = replica.library_lock();
    let _guard = lock.lock();
    let data_dir = replica
        .data_dir()
        .map(Path::to_path_buf)
        .context("корзина доступна только для реплики на диске")?;
    let root = replica.root()?.context("корень библиотеки не задан")?;

    let vault_row = match replica.vault_for_path(path)? {
        Some(r) => r,
        None => bail!("«{path}» удалён, и копии в корзине нет — восстановить нечем"),
    };

    let src = vault_blob_path(&data_dir, &vault_row.blob);
    if !src.is_file() {
        replica.remove_vault_row(&vault_row.blob, &vault_row.path)?;
        bail!("«{path}» удалён, но файл корзины потерян — восстановить нечем");
    }

    let dst = disk_path(&root, path);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("создание {}", parent.display()))?;
    }
    // На пути уже может лежать новый файл с тем же именем: скачали заново или
    // создали вручную. Восстановление не имеет права его перезаписать, поэтому
    // занятую версию сохраняем рядом как conflict-copy.
    if dst.is_file() && scan::hash_file(&dst).ok().as_deref() != Some(vault_row.blob.as_str()) {
        crate::commit::save_conflict_copy(replica, &root, &dst, path)?;
    }
    // Копия, а не hardlink: восстановленный файл будут править тегами, и он не
    // должен менять содержимое корзины (там может лежать тот же блоб).
    std::fs::copy(&src, &dst).with_context(|| format!("восстановление {}", dst.display()))?;
    scan::restore_mtime(&dst, vault_row.mtime_ns)?;

    let kind = OpKind::EntryPut {
        path: path.to_string(),
        blob: vault_row.blob.clone(),
        size: vault_row.size,
        mtime_ns: vault_row.mtime_ns,
        added_at: vault_row.added_at,
    };
    let op = replica.make_op(kind)?;
    replica.apply(&op)?;
    // `Tombstone` → `Live { base=None, present=1 }`: удаление уже было общим
    // фактом, поэтому возвращённый файл — новая жизнь, а не правка старой
    // базы. План должен снова показать его как добавление.
    if let Some(mut row) = replica.path_state(path)? {
        row.state = PathState::Live;
        row.base = None;
        row.present = true;
        row.pending_hlc = None;
        row.created_at = None;
        replica.save_path_state(&row)?;
    }
    replica.remove_vault_row(&vault_row.blob, &vault_row.path)?;
    Ok(())
}

/// Полностью очищает корзину: все строки и все файлы блобов.
///
/// Восстановить что-либо после этого нельзя — вызывающий код обязан спросить
/// пользователя. Файлы библиотеки не затрагиваются: hardlink в корзине — лишь
/// второе имя, его удаление не трогает оригинал.
pub fn clear(replica: &Replica) -> Result<GcReport> {
    // Копии нельзя удалять, пока скан или commit читает те же блобы.
    let lock = replica.library_lock();
    let _guard = lock.lock();
    let Some(data_dir) = replica.data_dir() else {
        return Ok(GcReport::default());
    };

    let mut report = GcReport {
        rows_removed: replica.clear_vault_rows()?,
        ..GcReport::default()
    };

    let root = vault_root(data_dir);
    if root.is_dir() {
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
        {
            if entry.file_type().is_file() && std::fs::remove_file(entry.path()).is_ok() {
                report.blobs_removed += 1;
            }
        }
        // Каталог `vault` тоже убираем, если он опустел.
        let _ = std::fs::remove_dir(&root);
        if let Some(parent) = root.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    Ok(report)
}

/// Чистит корзину: строки старше `ttl_days`, блобы без ссылок и файлы-сироты
/// (например, оставшиеся после ручного удаления БД).
pub fn gc(replica: &Replica, ttl_days: u64) -> Result<GcReport> {
    // Как и `clear`: не трогаем блобы, которые прямо сейчас читает сеанс.
    let lock = replica.library_lock();
    let _guard = lock.lock();
    let Some(data_dir) = replica.data_dir() else {
        return Ok(GcReport::default());
    };

    let mut report = GcReport::default();
    let now = Hlc::wall_ms();
    let ttl_ms = ttl_days.saturating_mul(24 * 60 * 60 * 1000);
    let cutoff = now.saturating_sub(ttl_ms);

    for row in replica.vault_rows_before(cutoff)? {
        replica.remove_vault_row(&row.blob, &row.path)?;
        report.rows_removed += 1;

        if replica.vault_refs(&row.blob)? == 0 {
            let file = vault_blob_path(data_dir, &row.blob);
            if std::fs::remove_file(&file).is_ok() {
                report.blobs_removed += 1;
            }
        }
    }

    // Сироты: файлы корзины, на которые не ссылается ни одна строка.
    let live: std::collections::BTreeSet<PathBuf> = replica
        .vault_rows()?
        .into_iter()
        .map(|r| vault_blob_path(data_dir, &r.blob))
        .collect();
    let root = vault_root(data_dir);
    if root.is_dir() {
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            if !live.contains(entry.path()) && std::fs::remove_file(entry.path()).is_ok() {
                report.orphans_removed += 1;
                prune_empty_dirs(&root, entry.path().parent());
            }
        }
        let _ = std::fs::remove_dir(&root);
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn restore_brings_tombstone_back_as_a_new_life() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("music");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("track.flac"), b"one").unwrap();
        let mut replica = Replica::open(&tmp.path().join("state"), "dev-a").unwrap();
        replica.set_root(&root).unwrap();
        scan::scan(&root, &mut replica).unwrap();
        replica.confirm_synced().unwrap();

        // Подтверждённое удаление: копия в корзине, состояние Tombstone.
        preserve_file(&replica, &root, "track.flac").unwrap();
        let op = replica
            .make_op(OpKind::EntryDel {
                path: "track.flac".into(),
            })
            .unwrap();
        replica.apply(&op).unwrap();
        fs::remove_file(root.join("track.flac")).unwrap();
        assert_eq!(
            replica.path_state("track.flac").unwrap().unwrap().state,
            PathState::Tombstone
        );

        restore(&mut replica, "track.flac").unwrap();

        let row = replica.path_state("track.flac").unwrap().unwrap();
        assert_eq!(row.state, PathState::Live);
        assert_eq!(row.base, None, "возврат — новая жизнь, а не правка базы");
        assert!(row.present);
        assert!(row.pending_hlc.is_none());
        assert_eq!(fs::read(root.join("track.flac")).unwrap(), b"one");
        assert!(replica.vault_rows().unwrap().is_empty());
    }

    #[test]
    fn blob_key_is_windows_safe() {
        assert_eq!(blob_key("b3:ABCD"), "abcd");
        assert_eq!(blob_key("raw"), "a");
    }

    #[test]
    fn vault_path_uses_two_char_prefix() {
        let p = vault_blob_path(Path::new("D:/data"), "b3:abcdef");
        assert!(p.ends_with("ab/abcdef"), "{}", p.display());
    }
}
