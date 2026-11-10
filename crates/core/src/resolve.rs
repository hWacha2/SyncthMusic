//! Разрешение ожидающих удалений — единственное место, где рождается `entry_del`.
//!
//! Удаление файла на устройстве ничего не реплицирует: сканер переводит путь в
//! `Missing` и хранит `pending_hlc`. Операция удаления появляется только после
//! явного решения пользователя. Поэтому «главное устройство» не нужно:
//! приоритет у решения человека, а не у железа.
//!
//! Решение принимается по **блобам**, а не по путям. Если трек на другом
//! устройстве успели перенести в другую папку, «удалить везде» всё равно
//! удалит все его копии, а «только здесь» не тронет общую библиотеку.
//!
//! Здесь же живёт отмена исключения: она переводит `Excluded*` в
//! `ResurrectingLocal`/`ResurrectingRemote` (см. `docs/path-state.md`, 5.5).

use anyhow::{bail, Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use crate::hlc::Hlc;
use crate::op::{Op, OpKind};
use crate::plan::{Decision, PathDecision};
use crate::scan;
use crate::store::{PathState, Replica};

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
        .path_state(path)?
        .filter(|row| row.state == PathState::Missing && row.pending_hlc.is_some())
        .with_context(|| format!("нет ожидающего удаления для {path}"))?;

    let root = replica.root()?.context("корень библиотеки не задан")?;

    match action {
        Action::DeleteEverywhere => delete_everywhere(replica, &root, &pending.blob, path),
        Action::OnlyHere => only_here(replica, path, &root),
    }
}

fn delete_everywhere(
    replica: &mut Replica,
    root: &Path,
    blob: &str,
    origin: &str,
) -> Result<Outcome> {
    let mut out = Outcome::default();
    let targets = replica.live_paths_by_blob(blob)?;
    // Если трек успели перенести, среди целей есть и старый путь, по которому
    // человек принял решение. Он уже исчез здесь; в отчёте оставляем только
    // реально найденную живую копию, но операцию удаления шлём и по нему.
    let moved_elsewhere = targets.iter().any(|path| path != origin)
        && replica.moved_from(origin, blob)?;
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
        let op = replica.make_op(kind)?;
        replica.apply(&op)?;
        if path != origin || !moved_elsewhere {
            out.deleted_paths.push(path.clone());
        }
        out.ops_emitted += 1;

        match remove_file(root, &path) {
            Ok(()) => out.files_removed += 1,
            Err(e) => out.failed.push((path, format!("{e:#}"))),
        }
    }

    // Ожидания, указывавшие на этот блоб под другими путями, больше не нужны.
    // Неудачные пути оставляем в очереди — их можно повторить.
    for mut row in replica.path_states()? {
        if row.blob == blob && !failed.contains(&row.path) && row.pending_hlc.is_some() {
            row.pending_hlc = None;
            if row.state == PathState::Missing {
                row.state = PathState::Live;
                row.created_at = None;
            }
            replica.save_path_state(&row)?;
        }
    }

    Ok(out)
}

fn only_here(replica: &mut Replica, path: &str, root: &Path) -> Result<Outcome> {
    let mut out = Outcome::default();
    let Some(mut row) = replica.path_state(path)? else {
        bail!("путь не найден в библиотеке: {path}");
    };
    // «Только здесь» — исключение на стороне пира: файл остался в общей
    // библиотеке, но на это устройство больше не вернётся.
    row.state = PathState::ExcludedRemote;
    row.present = false;
    row.pending_hlc = None;
    row.created_at = Some(Hlc::wall_ms());
    replica.save_path_state(&row)?;
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
    let Some(row) = replica.path_state(path)? else {
        bail!("путь не найден в библиотеке: {path}");
    };
    if row.state.is_tombstone() {
        return crate::vault::restore(replica, path);
    }
    if row.state.is_excluded() {
        let out = undo_exclusion(replica, path, None)?;
        if out.lost {
            bail!("«{path}» потерян: копии нет ни здесь, ни у пира");
        }
        return Ok(());
    }
    // Путь и так в синхронизации: снимаем только ожидание удаления.
    if row.pending_hlc.is_some() {
        let mut row = row;
        row.pending_hlc = None;
        if row.state == PathState::Missing {
            row.state = PathState::Live;
            row.created_at = None;
        }
        replica.save_path_state(&row)?;
    }
    Ok(())
}

/// Что произошло при отмене исключения. Нужно интерфейсу и тестам, чтобы
/// отличить «файл остался здесь» от «файл придёт от пира» и от потери.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UndoOutcome {
    /// Уцелевшая копия лежит на этом устройстве и возвращена как добавление.
    pub kept_local: bool,
    /// Здесь файла нет: копию нужно забрать у пира.
    pub waiting_remote: bool,
    /// Копии нет ни здесь, ни у пира: путь переведён в `Tombstone`.
    pub lost: bool,
}

/// Снимает исключение и возвращает путь в синхронизацию (раздел 5.5).
///
/// Сторона, где файл уцелел, сообщает об этом пиру свежим `EntryPut`, а
/// сторона без файла ждёт копию от пира. Старое `OnlyHere` перебивается
/// `Unset` у всех известных пиров: иначе при следующем слиянии решений
/// исключение вернулось бы.
pub fn undo_exclusion(
    replica: &mut Replica,
    path: &str,
    peer_dev: Option<&str>,
) -> Result<UndoOutcome> {
    let mut out = UndoOutcome::default();
    let Some(mut row) = replica.path_state(path)? else {
        return Ok(out);
    };
    if !row.state.is_excluded() {
        // Повторная отмена — не ошибка: исключение уже снято.
        return Ok(out);
    }

    let root = replica.root()?.context("корень библиотеки не задан")?;
    let disk = disk_path(&root, path);
    let now = Hlc::wall_ms();

    if disk.is_file() {
        // Копия уцелела здесь. Сначала выводим путь из исключения: операции
        // по исключённым путям не применяются. Затем свежий `EntryPut` делает
        // файл добавлением для пира — старый `pending_delete` получит более
        // новый HLC и перестанет считаться удалением.
        let blob = scan::hash_file(&disk)?;
        let meta = std::fs::metadata(&disk)?;
        let size = meta.len();
        let mtime_ns = scan::mtime_ns_public(&meta);

        row.state = PathState::ResurrectingLocal;
        row.base = None;
        row.blob = blob.clone();
        row.size = size;
        row.mtime_ns = mtime_ns;
        row.present = true;
        row.pending_hlc = None;
        row.created_at = Some(now);
        replica.save_path_state(&row)?;

        let op = replica.make_op(OpKind::EntryPut {
            path: path.to_string(),
            blob,
            size,
            mtime_ns,
            added_at: now,
        })?;
        replica.apply(&op)?;
        out.kept_local = true;
    } else if !row.blob.is_empty() {
        // Здесь файла нет, но блоб известен: копию должен прислать пир.
        // Маркер заставляет план показать добавление с его стороны.
        row.state = PathState::ResurrectingRemote;
        row.present = false;
        row.pending_hlc = None;
        row.created_at = Some(now);
        replica.save_path_state(&row)?;
        out.waiting_remote = true;
    } else {
        // Ни здесь, ни у пира копии нет: не висим в ожидании, а честно
        // фиксируем потерю.
        row.state = PathState::Tombstone;
        row.present = false;
        row.pending_hlc = None;
        row.created_at = None;
        replica.save_path_state(&row)?;
        out.lost = true;
    }

    unset_decisions(replica, path, &row.blob, peer_dev)?;
    Ok(out)
}

/// Записать `Unset` всем известным пирам: исключения глобальны, а решения
/// хранятся per-peer. Иначе другой пир снова прислал бы `OnlyHere`.
fn unset_decisions(
    replica: &mut Replica,
    path: &str,
    blob: &str,
    peer_dev: Option<&str>,
) -> Result<()> {
    let mut peers: BTreeSet<String> = replica
        .known_peers()?
        .into_iter()
        .map(|peer| peer.dev)
        .collect();
    if let Some(dev) = peer_dev {
        peers.insert(dev.to_string());
    }
    for peer in peers {
        let mut set = replica.load_decisions(&peer)?;
        if set
            .get(path)
            .is_some_and(|decision| decision.action == Decision::Unset)
        {
            continue;
        }
        let blob = set
            .get(path)
            .map(|decision| decision.blob.clone())
            .filter(|blob| !blob.is_empty())
            .unwrap_or_else(|| blob.to_string());
        let decision = PathDecision {
            path: path.to_string(),
            action: Decision::Unset,
            blob,
            hlc: replica.next_hlc()?,
            dev: replica.device_id().to_string(),
        };
        set.entries.insert(path.to_string(), decision);
        replica.save_decisions(&peer, &set)?;
    }
    Ok(())
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

    // Пути, которые сейчас реально лежат здесь. Нужны, чтобы не удалить файл,
    // переименованный только по регистру/Unicode: на Windows/macOS старый путь
    // указывает на тот же самый файл, что и новый.
    let live_keys: HashMap<String, String> = replica
        .path_states()?
        .into_iter()
        .filter(|row| !row.state.is_tombstone() && row.present)
        .map(|row| (crate::path::key(&row.path), row.path))
        .collect();

    let rows = replica.deleted_present()?;

    for (path, blob) in rows {
        if let Some(other) = live_keys.get(&crate::path::key(&path)) {
            if other != &path {
                tracing::warn!(
                    "не удаляю «{path}»: это тот же файл, что живой «{other}» \
                     (различие только в регистре/Unicode)"
                );
                set_present(replica, &path, false)?;
                continue;
            }
        }
        let abs = disk_path(root, &path);
        if !abs.is_file() {
            set_present(replica, &path, false)?;
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
                set_present(replica, &path, false)?;
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
                let op = replica.make_op(kind)?;
                replica.apply(&op)?;
                set_present(replica, &path, false)?;
                saved.push(conflict);
            }
            Err(_) => {
                // Не читается (занят плеером) — не трогаем, попробуем позже.
            }
        }
    }

    Ok(saved)
}

/// Физически переносит локальный файл вслед за `entry_move`.
///
/// Без этого принимающая сторона сначала отправила бы старый файл в корзину,
/// а потом скачала тот же блоб по новому пути. Если локальная версия отличается
/// (её правили здесь), ничего не трогаем: конфликт разберёт `prune_deleted_files`.
pub fn apply_moves(replica: &mut Replica, root: &Path, ops: &[Op]) -> Result<usize> {
    let mut moved = 0;
    for op in ops {
        let OpKind::EntryMove {
            from,
            to,
            blob,
            mtime_ns,
            ..
        } = &op.kind
        else {
            continue;
        };
        let src = disk_path(root, from);
        let dst = disk_path(root, to);
        let same_key = crate::path::key(from) == crate::path::key(to);

        if !src.is_file() {
            // Перенос уже сделан (или файл только предстоит скачать).
            if dst.is_file() && scan::hash_file(&dst).ok().as_deref() == Some(blob.as_str()) {
                replica.mark_present(to)?;
                replica.mark_absent(from)?;
            }
            continue;
        }
        // Занятый destination не перезаписываем: это может быть чужая правка.
        // Для совпадающего ключа нужно убедиться, что это действительно тот же
        // файл (смена регистра/NFC), а не два разных файла на регистрозависимой ФС.
        if dst.exists() {
            if !same_key {
                continue;
            }
            if !same_existing_file(&src, &dst) {
                tracing::warn!(
                    "не переношу {from} → {to}: на диске уже есть другой файл"
                );
                continue;
            }
        }
        match scan::hash_file(&src) {
            Ok(h) if &h == blob => {
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let result = if from == to {
                    Ok(())
                } else if same_key {
                    // Windows/macOS считают `A.flac` и `a.flac` одним файлом и
                    // могут отказать в прямом rename. Идём через временное имя.
                    rename_case_safe(&src, &dst)
                } else {
                    std::fs::rename(&src, &dst)
                };
                match result {
                    Ok(()) => {
                        scan::restore_mtime(&dst, *mtime_ns)?;
                        replica.mark_present(to)?;
                        replica.mark_absent(from)?;
                        moved += 1;
                    }
                    Err(e) if same_key => {
                        // На регистронезависимой ФС src и dst — один и тот же файл;
                        // rename мог не понадобиться. Главное — проверить содержимое.
                        if dst.is_file()
                            && scan::hash_file(&dst).ok().as_deref() == Some(blob.as_str())
                        {
                            replica.mark_present(to)?;
                            replica.mark_absent(from)?;
                        } else {
                            tracing::warn!("не удалось перенести {from} → {to}: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("не удалось перенести {from} → {to}: {e}"),
                }
            }
            Ok(_) => {
                tracing::debug!("локальная версия {from} отличается — оставляю для конфликта");
            }
            Err(e) => tracing::warn!("не читается {from} при переносе: {e:#}"),
        }
    }
    Ok(moved)
}

/// Один и тот же файл на диске? Нужно для смены только регистра/NFC, когда
/// `dst.exists()` — это не занятый путь, а тот же самый inode.
fn same_existing_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(windows)]
    {
        // Метаданные Windows не дают стабильного file index в std; канонизация
        // разрешает регистр через саму ФС и возвращает одно и то же имя.
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

/// Переименование, меняющее только регистр/NFC. Прямой `rename` на Windows и
/// macOS может отказать, потому что целевое имя «уже занято» тем же файлом.
/// Поэтому сначала уводим файл во временное служебное имя, затем — в целевое.
fn rename_case_safe(src: &Path, dst: &Path) -> std::io::Result<()> {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let pid = std::process::id();
    let mut tmp = None;
    for index in 0..1024u32 {
        let candidate = parent.join(format!(".syncth-tmp-{pid}-{index}"));
        if !candidate.exists() {
            tmp = Some(candidate);
            break;
        }
    }
    let Some(tmp) = tmp else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "не удалось подобрать временное имя",
        ));
    };
    std::fs::rename(src, &tmp)?;
    match std::fs::rename(&tmp, dst) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Возвращаем файл на место, чтобы не потерять его под temp-именем.
            let _ = std::fs::rename(&tmp, src);
            Err(e)
        }
    }
}

/// Поставить/снять `present`, не меняя состояние пути.
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

pub fn disk_path(root: &Path, rel: &str) -> std::path::PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split('/') {
        p.push(part);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::PathStateRow;
    use std::fs;
    use std::path::PathBuf;
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
        scan::scan(&root, &mut replica).unwrap();
        replica.confirm_synced().unwrap();
        Fixture {
            _tmp: tmp,
            root,
            replica,
        }
    }

    fn blob_of(bytes: &[u8]) -> String {
        format!("b3:{}", blake3::hash(bytes).to_hex())
    }

    fn state(f: &Fixture, path: &str) -> PathStateRow {
        f.replica
            .path_state(path)
            .unwrap()
            .unwrap_or_else(|| panic!("нет состояния для {path}"))
    }

    fn only_here_decision(blob: &str) -> PathDecision {
        PathDecision {
            path: "track.flac".into(),
            action: Decision::OnlyHere,
            blob: blob.into(),
            hlc: Hlc::new(1, 0),
            dev: "dev-a".into(),
        }
    }

    // ------------------------------------------------------- отмена (раздел 5.5)

    #[test]
    fn undo_excluded_local_with_file_resurrects_local() {
        let mut f = fixture(&[("track.flac", b"one")]);
        let blob = blob_of(b"one");
        f.replica
            .add_exclusion("track.flac", "local", &blob, 3, 1)
            .unwrap();
        assert_eq!(state(&f, "track.flac").state, PathState::ExcludedLocal);

        let out = undo_exclusion(&mut f.replica, "track.flac", None).unwrap();

        assert!(out.kept_local);
        assert!(!out.waiting_remote);
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::ResurrectingLocal);
        assert_eq!(row.base, None);
        assert!(row.present);
        assert_eq!(row.blob, blob);
        assert_eq!(fs::read(f.root.join("track.flac")).unwrap(), b"one");
        // Свежий EntryPut должен уехать пиру.
        assert!(f
            .replica
            .ops()
            .unwrap()
            .iter()
            .any(|op| matches!(&op.kind, OpKind::EntryPut { path, .. } if path == "track.flac")));
    }

    #[test]
    fn undo_excluded_local_without_file_waits_for_remote() {
        let mut f = fixture(&[("track.flac", b"one")]);
        let blob = blob_of(b"one");
        f.replica
            .add_exclusion("track.flac", "local", &blob, 3, 1)
            .unwrap();
        fs::remove_file(f.root.join("track.flac")).unwrap();
        // Скан ещё не видел пропажу: состояние остаётся ExcludedLocal.
        assert_eq!(state(&f, "track.flac").state, PathState::ExcludedLocal);

        let out = undo_exclusion(&mut f.replica, "track.flac", None).unwrap();

        assert!(out.waiting_remote);
        assert!(!out.kept_local);
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::ResurrectingRemote);
        assert!(!row.present);
        assert_eq!(row.blob, blob);
    }

    #[test]
    fn undo_excluded_remote_waits_for_remote() {
        let mut f = fixture(&[("track.flac", b"one")]);
        let blob = blob_of(b"one");
        f.replica
            .add_exclusion("track.flac", "remote", &blob, 3, 1)
            .unwrap();
        fs::remove_file(f.root.join("track.flac")).unwrap();

        let out = undo_exclusion(&mut f.replica, "track.flac", None).unwrap();

        assert!(out.waiting_remote);
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::ResurrectingRemote);
        assert!(!row.present);
        assert_eq!(row.blob, blob);
    }

    #[test]
    fn undo_without_any_known_copy_reports_loss() {
        let mut f = fixture(&[("track.flac", b"one")]);
        fs::remove_file(f.root.join("track.flac")).unwrap();
        f.replica
            .add_exclusion("track.flac", "remote", "", 0, 0)
            .unwrap();

        let out = undo_exclusion(&mut f.replica, "track.flac", None).unwrap();

        assert!(out.lost);
        assert!(!out.kept_local && !out.waiting_remote);
        let row = state(&f, "track.flac");
        assert_eq!(row.state, PathState::Tombstone);
        assert!(!row.present);
    }

    #[test]
    fn undo_writes_unset_to_every_known_peer() {
        let mut f = fixture(&[("track.flac", b"one")]);
        let blob = blob_of(b"one");
        f.replica.add_peer("dev-b", "127.0.0.1:1", "Phone").unwrap();
        f.replica.add_peer("dev-c", "127.0.0.1:2", "Tablet").unwrap();
        for peer in ["dev-b", "dev-c"] {
            let mut set = f.replica.load_decisions(peer).unwrap();
            set.entries
                .insert("track.flac".into(), only_here_decision(&blob));
            f.replica.save_decisions(peer, &set).unwrap();
        }
        f.replica
            .add_exclusion("track.flac", "local", &blob, 3, 1)
            .unwrap();

        undo_exclusion(&mut f.replica, "track.flac", None).unwrap();

        for peer in ["dev-b", "dev-c"] {
            let set = f.replica.load_decisions(peer).unwrap();
            assert_eq!(
                set.get("track.flac").unwrap().action,
                Decision::Unset,
                "Unset должен уехать пиру {peer}"
            );
        }
    }

    #[test]
    fn repeated_undo_is_a_no_op() {
        let mut f = fixture(&[("track.flac", b"one")]);
        let blob = blob_of(b"one");
        f.replica
            .add_exclusion("track.flac", "local", &blob, 3, 1)
            .unwrap();
        undo_exclusion(&mut f.replica, "track.flac", None).unwrap();

        let out = undo_exclusion(&mut f.replica, "track.flac", None).unwrap();

        assert!(!out.kept_local && !out.waiting_remote && !out.lost);
        assert_eq!(state(&f, "track.flac").state, PathState::ResurrectingLocal);
    }
}
