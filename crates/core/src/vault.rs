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
use crate::store::{Replica, VaultRow};

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
    let data_dir = replica
        .data_dir()
        .map(Path::to_path_buf)
        .context("корзина доступна только для реплики на диске")?;
    let root = replica.root()?.context("корень библиотеки не задан")?;

    let row = match replica.vault_for_path(path)? {
        Some(r) => r,
        None => bail!("«{path}» удалён, и копии в корзине нет — восстановить нечем"),
    };

    let src = vault_blob_path(&data_dir, &row.blob);
    if !src.is_file() {
        replica.remove_vault_row(&row.blob, &row.path)?;
        bail!("«{path}» удалён, но файл корзины потерян — восстановить нечем");
    }

    let dst = disk_path(&root, path);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("создание {}", parent.display()))?;
    }
    // Копия, а не hardlink: восстановленный файл будут править тегами, и он не
    // должен менять содержимое корзины (там может лежать тот же блоб).
    std::fs::copy(&src, &dst).with_context(|| format!("восстановление {}", dst.display()))?;
    scan::restore_mtime(&dst, row.mtime_ns)?;

    let kind = OpKind::EntryPut {
        path: path.to_string(),
        blob: row.blob.clone(),
        size: row.size,
        mtime_ns: row.mtime_ns,
        added_at: row.added_at,
    };
    let op = replica.make_op(kind)?;
    replica.apply(&op)?;
    replica.set_ignored(path, false)?;
    replica.mark_present(path)?;
    replica.clear_pending_delete(path)?;
    replica.remove_vault_row(&row.blob, &row.path)?;
    Ok(())
}

/// Чистит корзину: строки старше `ttl_days`, блобы без ссылок и файлы-сироты
/// (например, оставшиеся после ручного удаления БД).
pub fn gc(replica: &Replica, ttl_days: u64) -> Result<GcReport> {
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
