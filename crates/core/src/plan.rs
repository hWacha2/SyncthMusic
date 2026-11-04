//! План сеанса: что изменилось с двух сторон и что с этим делать.
//!
//! Модель намеренно отделена от журнала и диска. Построение плана только
//! читает `entries`, `pending_deletes` и присланные операции пира. Ни одной
//! записи в БД или файловых операций здесь нет — их делает отдельная фаза
//! commit (см. `docs/sync-session.md`).
//!
//! Решение по пути — это не операция журнала, а отдельное намерение человека.
//! Оно живёт в своей таблице, привязано к паре устройств и сливается по HLC:
//! при равном времени побеждает больший `device_id`, затем устойчивый ранг
//! действия. Так две стороны могут менять решения, пока не начата передача.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::hlc::Hlc;
use crate::op::{Op, OpKind};
use crate::store::{PlanRow, Replica};

/// Сколько живёт план. Просроченный план не применяется: состояние могло
/// измениться, а решения относятся к конкретным блобам.
pub const PLAN_TTL_MS: u64 = 15 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Local,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Updated,
    Moved,
    Deleted,
}

impl ChangeKind {
    /// Действие по умолчанию. Удаления никогда не включаются сами: снятая
    /// галочка — это осознанное «Остаётся», а не «ещё не решил».
    pub fn default_decision(self) -> Decision {
        match self {
            ChangeKind::Added | ChangeKind::Updated | ChangeKind::Moved => Decision::Accept,
            ChangeKind::Deleted => Decision::Keep,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    /// Для переноса — старый путь. Применяется как «убрать старый / добавить
    /// новый», но в плане это одна строка, чтобы не терять атомарность.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub kind: ChangeKind,
    pub side: Side,
    pub blob: String,
    /// Последний блоб, согласованный с пиром. Пустая строка — базы ещё нет.
    #[serde(default)]
    pub base_blob: String,
    pub size: u64,
    pub mtime_ns: i64,
    /// Файл реально есть на этом устройстве.
    pub present: bool,
    /// Путь исключён локально («оставить только на другом устройстве»).
    pub ignored: bool,
    /// Содержимое уже совпадает с пиром. Базы ещё может не быть, но передавать
    /// файл не нужно: коммит только зафиксирует совпадение. Такие строки не
    /// показываются как изменения.
    #[serde(default)]
    pub aligned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictReason {
    /// Обе стороны изменили путь после общей базы.
    BothChanged,
    /// Общей базы не было, а блобы разные.
    NoBase,
    /// Одна сторона изменила, другая удалила.
    ChangedVsDeleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    /// Путь, по которому принимается решение.
    pub path: String,
    /// Локальный путь, если конфликт пришёл из переноса.
    #[serde(default)]
    pub local_path: String,
    /// Путь на пире, если конфликт пришёл из переноса.
    #[serde(default)]
    pub remote_path: String,
    pub local_blob: String,
    pub remote_blob: String,
    pub local_size: u64,
    pub remote_size: u64,
    pub reason: ConflictReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Принять изменение: добавление, правку, перенос или версию пира
    /// в конфликте.
    Accept,
    /// Отменить локальное изменение и вернуть версию со второго устройства.
    /// Для добавления/удаления не используется.
    Revert,
    /// Оставить как есть. Для удаления это «Остаётся» — решение по умолчанию,
    /// которое не применяет удаление и не фиксирует расхождение навсегда.
    Keep,
    /// Удалить путь во всей библиотеке.
    DeleteEverywhere,
    /// Отклонить удаление и вернуть файл: новый `EntryPut`.
    KeepInLibrary,
    /// Оставить файл только на одной стороне (`local_ignored` на другой).
    OnlyHere,
    /// Сохранить обе версии конфликта.
    KeepBoth,
    /// Отмена решения. Хранится с новым HLC, чтобы пережить переподключение
    /// и не дать пиру вернуть старое решение; для плана это «решения нет».
    Unset,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Accept => "accept",
            Decision::Revert => "revert",
            Decision::Keep => "keep",
            Decision::DeleteEverywhere => "delete_everywhere",
            Decision::KeepInLibrary => "keep_in_library",
            Decision::OnlyHere => "only_here",
            Decision::KeepBoth => "keep_both",
            Decision::Unset => "unset",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "accept" => Decision::Accept,
            "revert" => Decision::Revert,
            "keep" => Decision::Keep,
            "delete_everywhere" => Decision::DeleteEverywhere,
            "keep_in_library" => Decision::KeepInLibrary,
            "only_here" => Decision::OnlyHere,
            "keep_both" => Decision::KeepBoth,
            "unset" => Decision::Unset,
            _ => return None,
        })
    }

    /// Устойчивый ранг для тай-брейка, когда HLC и автор совпали.
    fn rank(self) -> u8 {
        match self {
            Decision::Keep => 0,
            Decision::Revert => 1,
            Decision::Accept => 2,
            Decision::DeleteEverywhere => 3,
            Decision::KeepInLibrary => 4,
            Decision::OnlyHere => 5,
            Decision::KeepBoth => 6,
            Decision::Unset => 7,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathDecision {
    pub path: String,
    pub action: Decision,
    /// Блоб, к которому относится решение. Если содержимое изменилось,
    /// решение считается устаревшим.
    pub blob: String,
    pub hlc: Hlc,
    pub dev: String,
}

impl PathDecision {
    fn order_key(&self) -> (u64, u32, &str, u8) {
        (self.hlc.ms, self.hlc.counter, self.dev.as_str(), self.action.rank())
    }
}

/// Явные решения человека. Отсутствие решения — не то же самое, что
/// «Остаётся»: для добавлений/правок работает действие по умолчанию, для
/// конфликтов — статус «Требует решения».
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionSet {
    #[serde(default)]
    pub entries: BTreeMap<String, PathDecision>,
}

impl DecisionSet {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, path: &str) -> Option<&PathDecision> {
        self.entries.get(path)
    }

    /// Слияние по HLC, last-write-wins на путь. Возвращает число изменений.
    pub fn merge(&mut self, incoming: &DecisionSet) -> usize {
        let mut changed = 0;
        for (path, candidate) in &incoming.entries {
            let replace = match self.entries.get(path) {
                None => true,
                Some(current) => candidate.order_key() > current.order_key(),
            };
            if replace {
                self.entries.insert(path.clone(), candidate.clone());
                changed += 1;
            }
        }
        changed
    }

    /// Оставить только решения, которые всё ещё относятся к текущему плану.
    /// Путь исчез или блоб изменился — решение больше не применимо.
    /// Исключение — `Unset`: это надгробие отмены, оно должно пережить
    /// переподключение и перебить устаревшее решение пира.
    pub fn retain_valid(&mut self, plan: &Plan) -> usize {
        let before = self.entries.len();
        self.entries.retain(|path, decision| {
            if decision.action == Decision::Unset {
                return true;
            }
            plan.blob_for(path)
                .map(|blob| blob == decision.blob)
                .unwrap_or(false)
        });
        before - self.entries.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub peer_dev: String,
    pub peer_name: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub local: Vec<Change>,
    pub remote: Vec<Change>,
    pub conflicts: Vec<Conflict>,
    #[serde(default)]
    pub decisions: DecisionSet,
    pub version: Hlc,
}

impl Plan {
    pub fn is_expired(&self, now: u64) -> bool {
        now >= self.expires_at
    }

    /// Пути, которые действительно отличаются. Совпавшие файлы не попадают
    /// в счётчик, иначе первый сеанс показывал бы всю библиотеку как «новую».
    pub fn path_count(&self) -> usize {
        let mut paths: HashSet<&str> = HashSet::new();
        for change in self.local.iter().chain(self.remote.iter()) {
            if !change.aligned {
                paths.insert(change.path.as_str());
            }
        }
        for conflict in &self.conflicts {
            paths.insert(conflict.path.as_str());
        }
        paths.len()
    }

    /// Сколько путей уже совпадает по содержимому и будет только зафиксировано.
    pub fn aligned_count(&self) -> usize {
        let mut paths: HashSet<&str> = HashSet::new();
        for change in self.local.iter().chain(self.remote.iter()) {
            if change.aligned {
                paths.insert(change.path.as_str());
            }
        }
        paths.len()
    }

    pub fn change(&self, path: &str) -> Option<&Change> {
        self.local
            .iter()
            .chain(self.remote.iter())
            .find(|change| change.path == path)
    }

    pub fn conflict(&self, path: &str) -> Option<&Conflict> {
        self.conflicts.iter().find(|conflict| conflict.path == path)
    }

    /// Блоб, к которому относится решение по пути. Для конфликта — оба
    /// блоба: если изменится любой, решение устареет.
    pub fn blob_for(&self, path: &str) -> Option<String> {
        if let Some(conflict) = self.conflict(path) {
            // Порядок блобов должен быть одинаковым на обеих сторонах:
            // у каждой свой local/remote, но пара — одна и та же.
            let (a, b) = if conflict.local_blob <= conflict.remote_blob {
                (&conflict.local_blob, &conflict.remote_blob)
            } else {
                (&conflict.remote_blob, &conflict.local_blob)
            };
            return Some(format!("{a}|{b}"));
        }
        self.change(path).map(|change| change.blob.clone())
    }

    /// Действие по умолчанию для пути, если человек ничего не выбрал.
    pub fn default_decision(&self, path: &str) -> Option<Decision> {
        if self.conflict(path).is_some() {
            return None;
        }
        self.change(path).and_then(|change| {
            if change.ignored {
                Some(Decision::Keep)
            } else {
                Some(change.kind.default_decision())
            }
        })
    }

    /// Явное решение человека, если оно есть и не отменено через `Unset`.
    pub fn explicit_entry(&self, path: &str) -> Option<&PathDecision> {
        self.decisions
            .get(path)
            .filter(|decision| decision.action != Decision::Unset)
    }

    /// Явное решение по пути. `None` и для отсутствующего решения, и для
    /// отменённого: после «Отменить» путь снова считается нерешённым.
    pub fn explicit_decision(&self, path: &str) -> Option<Decision> {
        self.explicit_entry(path).map(|decision| decision.action)
    }

    /// Итоговое действие: явное решение человека важнее умолчания.
    pub fn effective_decision(&self, path: &str) -> Option<Decision> {
        self.explicit_decision(path)
            .or_else(|| self.default_decision(path))
    }

    /// Конфликты без явного решения. Commit не разрешён, пока список не пуст.
    pub fn unresolved(&self) -> Vec<String> {
        self.conflicts
            .iter()
            .filter(|conflict| self.explicit_entry(&conflict.path).is_none())
            .map(|conflict| conflict.path.clone())
            .collect()
    }

    /// Хэш явных решений и нерешённых конфликтов: по нему commit проверяет,
    /// что обе стороны видят одно и то же.
    ///
    /// Сравнивать полный `effective_map` нельзя. `local_ignored` — локальное
    /// состояние и пиру не передаётся, поэтому один и тот же файл может быть
    /// строкой плана у одной стороны и отсутствовать у другой. Решение по
    /// такой строке никто не принимает, и расхождение планов здесь ложное:
    /// commit всё равно не тронет путь, который вторая сторона игнорирует.
    pub fn decisions_hash(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        for (path, decision) in &self.decisions.entries {
            hasher.update(path.as_bytes());
            hasher.update(b"\0");
            hasher.update(decision.action.as_str().as_bytes());
            hasher.update(b"\0");
            hasher.update(decision.blob.as_bytes());
            hasher.update(b"\n");
        }
        // Конфликт без решения — такая же часть общего состояния, как и
        // выбранное действие: обе стороны обязаны видеть его одинаково.
        for path in self.unresolved() {
            hasher.update(b"unresolved\0");
            hasher.update(path.as_bytes());
            hasher.update(b"\n");
        }
        format!("b3:{}", hasher.finalize().to_hex())
    }

    /// Все пути плана с итоговым действием. `unresolved` — не действие,
    /// а честная метка того, что решение ещё не принято.
    pub fn effective_map(&self) -> BTreeMap<String, &'static str> {
        let mut map = BTreeMap::new();
        for change in self.local.iter().chain(self.remote.iter()) {
            let action = self
                .effective_decision(&change.path)
                .map(Decision::as_str)
                .unwrap_or("unresolved");
            map.insert(change.path.clone(), action);
        }
        for conflict in &self.conflicts {
            let action = self
                .effective_decision(&conflict.path)
                .map(Decision::as_str)
                .unwrap_or("unresolved");
            map.insert(conflict.path.clone(), action);
        }
        map
    }

    /// Отпечаток самого плана (не решений). Нужен, чтобы отличить старый
    /// план от перестроенного при том же наборе решений.
    pub fn fingerprint(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.peer_dev.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.created_at.to_string().as_bytes());
        for change in self.local.iter().chain(self.remote.iter()) {
            hasher.update(b"\n");
            hasher.update(change.path.as_bytes());
            hasher.update(b"\0");
            hasher.update(change.blob.as_bytes());
            hasher.update(b"\0");
            hasher.update(change.kind.default_decision().as_str().as_bytes());
            hasher.update(b"\0");
            hasher.update(change.base_blob.as_bytes());
        }
        for conflict in &self.conflicts {
            hasher.update(b"\nconflict\0");
            hasher.update(conflict.path.as_bytes());
            hasher.update(b"\0");
            hasher.update(conflict.local_blob.as_bytes());
            hasher.update(b"\0");
            hasher.update(conflict.remote_blob.as_bytes());
        }
        format!("b3:{}", hasher.finalize().to_hex())
    }

    /// Локальная часть плана ещё соответствует БД? Скан или правка файла
    /// после построения плана делают его непригодным.
    pub fn local_is_current(&self, replica: &Replica) -> Result<bool> {
        let rows = replica.plan_rows()?;
        let by_path: HashMap<&str, &PlanRow> =
            rows.iter().map(|row| (row.path.as_str(), row)).collect();
        let pending: HashSet<String> = replica
            .pending_deletes()?
            .into_iter()
            .map(|row| row.path)
            .collect();
        let conflicts: HashSet<&str> = self
            .conflicts
            .iter()
            .map(|conflict| conflict.path.as_str())
            .collect();

        for change in &self.local {
            let Some(row) = by_path.get(change.path.as_str()) else {
                return Ok(false);
            };
            if change.kind == ChangeKind::Deleted {
                let still_pending = pending.contains(&change.path);
                if !row.deleted && !still_pending {
                    return Ok(false);
                }
                let base = if row.synced_blob.is_empty() {
                    change.blob.as_str()
                } else {
                    row.synced_blob.as_str()
                };
                if base != change.blob {
                    return Ok(false);
                }
            } else if row.deleted || row.blob != change.blob || row.ignored != change.ignored {
                return Ok(false);
            }
        }
        // Появившийся новый локальный путь тоже делает план неполным.
        for row in &rows {
            if row.ignored || row.deleted {
                continue;
            }
            if conflicts.contains(row.path.as_str()) {
                continue;
            }
            let changed = row.blob != row.synced_blob;
            let pending_delete = pending.contains(&row.path);
            if pending_delete && row.synced_blob.is_empty() {
                // Черновик, удалённый до первой синхронизации, не изменение.
                continue;
            }
            let in_plan = self
                .local
                .iter()
                .any(|change| change.path == row.path);
            if (changed || pending_delete) && !in_plan {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Построить план по локальному состоянию и операциям, которых у нас ещё нет.
///
/// `remote_ops` — операции пира, отобранные по watermarks (как в существующем
/// протоколе). Функция ничего не пишет и не проверяет подписи: это делает
/// `Replica::apply` на фазе commit, а план лишь показывает намерения.
pub fn build(replica: &Replica, peer_dev: &str, peer_name: &str, remote_ops: &[Op]) -> Result<Plan> {
    build_with_peer_have(replica, peer_dev, peer_name, remote_ops, None)
}

/// То же, но с watermarks пира: по ним видно, применил ли он уже наше
/// подтверждённое удаление. Без этого после commit удаление навсегда
/// оставалось бы в плане одной стороны («план разошёлся»).
pub fn build_with_peer_have(
    replica: &Replica,
    peer_dev: &str,
    peer_name: &str,
    remote_ops: &[Op],
    peer_have: Option<&HashMap<String, (i64, i64)>>,
) -> Result<Plan> {
    let created_at = Hlc::wall_ms();
    let mut version = Hlc::new(created_at, 0);
    for op in remote_ops {
        if op.hlc > version {
            version = op.hlc;
        }
    }

    let rows = replica.plan_rows()?;
    let by_path: HashMap<&str, &PlanRow> =
        rows.iter().map(|row| (row.path.as_str(), row)).collect();
    let pending: HashMap<String, (String, u64)> = replica
        .pending_deletes()?
        .into_iter()
        .map(|row| (row.path, (row.blob, row.detected_at)))
        .collect();
    let remote = remote_state(remote_ops);

    let mut local = Vec::new();
    let mut conflicts = Vec::new();
    let mut conflict_paths: HashSet<String> = HashSet::new();

    // ---------------------------------------------------------- локальные
    for row in &rows {
        if row.ignored {
            continue;
        }
        if let Some((blob, _)) = pending.get(&row.path) {
            // Путь ни разу не входил в общую базу: пир о нём не знает, и
            // «удаление» здесь — не изменение, а просто исчезнувший черновик.
            // Сканер такие записи забывает, это лишь страховка для старых БД.
            if row.synced_blob.is_empty() {
                continue;
            }
            local.push(Change {
                path: row.path.clone(),
                from: None,
                kind: ChangeKind::Deleted,
                side: Side::Local,
                blob: blob.clone(),
                base_blob: row.synced_blob.clone(),
                size: row.size,
                mtime_ns: row.mtime_ns,
                present: false,
                ignored: false,
                aligned: false,
            });
            continue;
        }
        if row.deleted {
            // Подтверждённое удаление несём тому пиру, у которого файл ещё
            // лежит (или который ещё не применил нашу операцию удаления).
            // Иначе после общего удаления путь всплывал бы в плане вечно,
            // хотя обе стороны уже согласны.
            let peer_deleted = remote
                .get(&row.path)
                .is_some_and(|state| !state.present);
            let peer_has_it = remote
                .get(&row.path)
                .is_some_and(|state| state.present)
                || (!peer_deleted
                    && !peer_applied_delete(replica, peer_have, &row.path)?);
            if peer_has_it && !row.synced_blob.is_empty() {
                local.push(Change {
                    path: row.path.clone(),
                    from: None,
                    kind: ChangeKind::Deleted,
                    side: Side::Local,
                    blob: row.synced_blob.clone(),
                    base_blob: row.synced_blob.clone(),
                    size: row.size,
                    mtime_ns: row.mtime_ns,
                    present: false,
                    ignored: false,
                    aligned: false,
                });
            }
            continue;
        }
        if row.blob != row.synced_blob {
            // Файл уже идентичен версии пира. Базы может не быть, но данные
            // передавать незачем: коммит лишь зафиксирует совпадение.
            let aligned = row.present
                && remote
                    .get(&row.path)
                    .is_some_and(|state| state.present && state.blob == row.blob);
            local.push(Change {
                path: row.path.clone(),
                from: None,
                kind: if row.synced_blob.is_empty() {
                    ChangeKind::Added
                } else {
                    ChangeKind::Updated
                },
                side: Side::Local,
                blob: row.blob.clone(),
                base_blob: row.synced_blob.clone(),
                size: row.size,
                mtime_ns: row.mtime_ns,
                present: row.present,
                ignored: row.ignored,
                aligned,
            });
        }
    }

    // ---------------------------------------------------------- удалённые
    let mut remote_changes = Vec::new();
    // Источники переноса отдельной строкой не показываем: иначе решение
    // «Остаётся» по источнику оставило бы старый файл при принятом переносе.
    let mut move_sources: HashSet<String> = HashSet::new();
    for state in remote.values() {
        if state.moved_from.is_some() {
            move_sources.extend(state.moved_from.iter().cloned());
        }
    }

    for state in remote.values() {
        let path = &state.path;
        if by_path.get(path.as_str()).is_some_and(|row| row.ignored) {
            // «Только здесь»: не тянем и не трогаем.
            continue;
        }
        let row = by_path.get(path.as_str()).cloned();

        if state.present {
            let local_changed = match row {
                Some(row) if !row.deleted => {
                    row.blob != row.synced_blob || pending.contains_key(&row.path)
                }
                Some(row) if row.deleted => !row.synced_blob.is_empty(),
                _ => false,
            };
            let local_blob = row
                .filter(|row| !row.deleted)
                .map(|row| row.blob.clone())
                .unwrap_or_default();
            let same_content = row
                .filter(|row| !row.deleted)
                .is_some_and(|row| row.blob == state.blob);
            let move_changes_old_path = state.moved_from.as_ref().is_some_and(|from| {
                by_path
                    .get(from.as_str())
                    .is_some_and(|row| !row.deleted && !row.synced_blob.is_empty())
            });
            // Пир мог прислать старую операцию, которая уже является общей
            // базой. Это не его изменение: конфликт только если обе стороны
            // действительно ушли от базы.
            let remote_changed = state.moved_from.is_some()
                || row
                    .as_ref()
                    .map_or(true, |row| row.deleted || state.blob != row.synced_blob);

            if same_content && !move_changes_old_path {
                continue;
            }

            if row.is_some_and(|row| row.deleted) && row.is_some_and(|row| row.synced_blob == state.blob)
            {
                // Мы удалили, а у пира лежит та же база: наше удаление —
                // единственное изменение, его и отправим.
                continue;
            }

            if !remote_changed {
                // Содержимое пира всё ещё совпадает с базой: изменение только
                // локальное, его уже добавил локальный проход.
                continue;
            }

            if local_changed && !same_content {
                let reason = match row {
                    Some(row) if row.deleted => ConflictReason::ChangedVsDeleted,
                    Some(row) if row.synced_blob.is_empty() => ConflictReason::NoBase,
                    _ => ConflictReason::BothChanged,
                };
                let (local_path, local_size) = row
                    .map(|row| (row.path.clone(), row.size))
                    .unwrap_or_else(|| (path.clone(), 0));
                let local_blob = if let Some(from) = &state.moved_from {
                    by_path
                        .get(from.as_str())
                        .filter(|row| !row.deleted && row.blob != row.synced_blob)
                        .map(|row| row.blob.clone())
                        .unwrap_or(local_blob)
                } else {
                    local_blob
                };
                conflicts.push(Conflict {
                    path: path.clone(),
                    local_path,
                    remote_path: path.clone(),
                    local_blob,
                    remote_blob: state.blob.clone(),
                    local_size,
                    remote_size: state.size,
                    reason,
                });
                conflict_paths.insert(path.clone());
                continue;
            }

            // Перенос, у которого локально изменён старый путь, — тоже
            // конфликт: принять его значило бы молча потерять правку.
            if let Some(from) = &state.moved_from {
                let source_changed = by_path.get(from.as_str()).is_some_and(|row| {
                    !row.deleted
                        && (row.blob != row.synced_blob || pending.contains_key(&row.path))
                });
                if source_changed {
                    let source = by_path.get(from.as_str()).cloned();
                    conflicts.push(Conflict {
                        path: path.clone(),
                        local_path: from.clone(),
                        remote_path: path.clone(),
                        local_blob: source.map(|row| row.blob.clone()).unwrap_or_default(),
                        remote_blob: state.blob.clone(),
                        local_size: source.map(|row| row.size).unwrap_or(0),
                        remote_size: state.size,
                        reason: ConflictReason::BothChanged,
                    });
                    conflict_paths.insert(path.clone());
                    continue;
                }
            }

            remote_changes.push(Change {
                path: path.clone(),
                from: state.moved_from.clone(),
                kind: if state.moved_from.is_some() {
                    ChangeKind::Moved
                } else if row.is_none() || row.is_some_and(|row| row.synced_blob.is_empty()) {
                    ChangeKind::Added
                } else {
                    ChangeKind::Updated
                },
                side: Side::Remote,
                blob: state.blob.clone(),
                base_blob: row.map(|row| row.synced_blob.clone()).unwrap_or_default(),
                size: state.size,
                mtime_ns: state.mtime_ns,
                present: row.is_some_and(|row| row.present),
                ignored: false,
                aligned: false,
            });
            continue;
        }

        // Удалён у пира.
        if move_sources.contains(path) {
            // Источник переноса уже обработан строкой переноса.
            continue;
        }
        let Some(row) = row else {
            // Путь нам вообще не известен — удалять нечего.
            continue;
        };
        if row.deleted {
            continue;
        }
        if pending.contains_key(&row.path) {
            // Обе стороны удалили — это не конфликт.
            continue;
        }
        if row.blob != row.synced_blob {
            conflicts.push(Conflict {
                path: row.path.clone(),
                local_path: row.path.clone(),
                remote_path: row.path.clone(),
                local_blob: row.blob.clone(),
                remote_blob: String::new(),
                local_size: row.size,
                remote_size: 0,
                reason: ConflictReason::ChangedVsDeleted,
            });
            conflict_paths.insert(row.path.clone());
            continue;
        }
        remote_changes.push(Change {
            path: row.path.clone(),
            from: None,
            kind: ChangeKind::Deleted,
            side: Side::Remote,
            blob: row.synced_blob.clone(),
            base_blob: row.synced_blob.clone(),
            size: row.size,
            mtime_ns: row.mtime_ns,
            present: false,
            ignored: false,
            aligned: false,
        });
    }

    // Пути конфликтов не дублируем в списках изменений.
    local.retain(|change| !conflict_paths.contains(&change.path));
    remote_changes.retain(|change| !conflict_paths.contains(&change.path));

    local.sort_by(|a, b| a.path.cmp(&b.path));
    remote_changes.sort_by(|a, b| a.path.cmp(&b.path));
    conflicts.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(Plan {
        peer_dev: peer_dev.to_string(),
        peer_name: peer_name.to_string(),
        created_at,
        expires_at: created_at + PLAN_TTL_MS,
        local,
        remote: remote_changes,
        conflicts,
        decisions: DecisionSet::default(),
        version,
    })
}

#[derive(Debug, Clone, Default)]
struct RemoteState {
    path: String,
    present: bool,
    blob: String,
    size: u64,
    mtime_ns: i64,
    moved_from: Option<String>,
}

/// Последнее состояние каждого пути по присланным операциям. Порядок —
/// HLC и затем `dev`, чтобы результат не зависел от порядка доставки.
fn peer_applied_delete(
    replica: &Replica,
    peer_have: Option<&HashMap<String, (i64, i64)>>,
    path: &str,
) -> Result<bool> {
    let Some(have) = peer_have else {
        return Ok(false);
    };
    let Some(hlc) = replica.last_delete_hlc(replica.device_id(), path)? else {
        // Своего удаления мы не отправляли: запись закрылась операцией пира,
        // а он про неё уже знает.
        return Ok(true);
    };
    Ok(have
        .get(replica.device_id())
        .is_some_and(|&(ms, counter)| Hlc::new(ms as u64, counter as u32) >= hlc))
}

fn remote_state(ops: &[Op]) -> BTreeMap<String, RemoteState> {
    let mut ordered: Vec<&Op> = ops.iter().collect();
    ordered.sort_by(|a, b| (a.hlc, a.dev.as_str()).cmp(&(b.hlc, b.dev.as_str())));

    let mut map: BTreeMap<String, RemoteState> = BTreeMap::new();
    for op in ordered {
        match &op.kind {
            OpKind::EntryPut {
                path,
                blob,
                size,
                mtime_ns,
                ..
            } => {
                map.insert(
                    path.clone(),
                    RemoteState {
                        path: path.clone(),
                        present: true,
                        blob: blob.clone(),
                        size: *size,
                        mtime_ns: *mtime_ns,
                        moved_from: None,
                    },
                );
            }
            OpKind::EntryDel { path } => {
                map.insert(
                    path.clone(),
                    RemoteState {
                        path: path.clone(),
                        present: false,
                        ..Default::default()
                    },
                );
            }
            OpKind::EntryMove {
                from,
                to,
                blob,
                size,
                mtime_ns,
                ..
            } => {
                map.insert(
                    from.clone(),
                    RemoteState {
                        path: from.clone(),
                        present: false,
                        ..Default::default()
                    },
                );
                map.insert(
                    to.clone(),
                    RemoteState {
                        path: to.clone(),
                        present: true,
                        blob: blob.clone(),
                        size: *size,
                        mtime_ns: *mtime_ns,
                        moved_from: Some(from.clone()),
                    },
                );
            }
            OpKind::BlobHave { .. } => {}
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::OpKind;

    fn put(path: &str, blob: &str) -> OpKind {
        OpKind::EntryPut {
            path: path.into(),
            blob: blob.into(),
            size: blob.len() as u64,
            mtime_ns: 1,
            added_at: 1,
        }
    }

    fn del(path: &str) -> OpKind {
        OpKind::EntryDel { path: path.into() }
    }

    fn remote(kind: OpKind, ms: u64) -> Op {
        Op::new("dev-peer", Hlc::new(ms, 0), kind)
    }

    /// Локальная запись, которую уже подтвердили как общую базу.
    fn synced(replica: &mut Replica, path: &str, blob: &str) {
        let op = replica.make_op(put(path, blob)).unwrap();
        replica.apply(&op).unwrap();
        replica.mark_present(path).unwrap();
        replica.confirm_synced().unwrap();
    }

    #[test]
    fn identical_replicas_produce_empty_plan() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "a.flac", "b3:aa");
        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();
        assert!(plan.local.is_empty());
        assert!(plan.remote.is_empty());
        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.path_count(), 0);
    }

    #[test]
    fn ignored_remote_addition_disappears_from_next_plan() {
        let a = Replica::in_memory("dev-a").unwrap();
        let addition = remote(put("skip.flac", "b3:skip"), 10);

        let plan = build(&a, "dev-b", "Phone", std::slice::from_ref(&addition)).unwrap();
        assert_eq!(plan.remote.len(), 1);
        assert_eq!(plan.remote[0].kind, ChangeKind::Added);

        // «Оставить только на телефоне»: запоминаем исключение, не создавая
        // локального файла и не забирая блоб.
        a.ignore_missing("skip.flac", "b3:skip", 3, 1).unwrap();

        let next = build(&a, "dev-b", "Phone", std::slice::from_ref(&addition)).unwrap();
        assert!(next.remote.is_empty());
        assert_eq!(next.path_count(), 0);
    }

    #[test]
    fn local_addition_is_accepted_by_default() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let op = a.make_op(put("new.flac", "b3:new")).unwrap();
        a.apply(&op).unwrap();
        a.mark_present("new.flac").unwrap();

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();
        assert_eq!(plan.local.len(), 1);
        assert_eq!(plan.local[0].kind, ChangeKind::Added);
        assert_eq!(plan.effective_decision("new.flac"), Some(Decision::Accept));
    }

    #[test]
    fn identical_file_without_base_is_aligned_not_a_change() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let op = a.make_op(put("same.flac", "b3:aa")).unwrap();
        a.apply(&op).unwrap();
        a.mark_present("same.flac").unwrap();

        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(put("same.flac", "b3:aa"), 10)],
        )
        .unwrap();
        assert_eq!(plan.local.len(), 1);
        assert!(plan.local[0].aligned);
        assert_eq!(plan.path_count(), 0);
        assert_eq!(plan.aligned_count(), 1);
        assert!(plan.unresolved().is_empty());
    }

    #[test]
    fn stale_peer_op_equal_to_base_is_not_a_conflict() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "track.flac", "b3:one");
        let update = a.make_op(put("track.flac", "b3:two")).unwrap();
        a.apply(&update).unwrap();
        a.mark_present("track.flac").unwrap();

        // Пир присылает операцию со старой базой: он ещё не менял файл.
        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(put("track.flac", "b3:one"), 10)],
        )
        .unwrap();
        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.local.len(), 1);
        assert_eq!(plan.local[0].kind, ChangeKind::Updated);
        assert!(plan.remote.is_empty());
    }

    #[test]
    fn plan_becomes_stale_after_a_local_change() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let op = a.make_op(put("track.flac", "b3:one")).unwrap();
        a.apply(&op).unwrap();
        a.mark_present("track.flac").unwrap();

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();
        assert!(plan.local_is_current(&a).unwrap());

        let changed = a.make_op(put("track.flac", "b3:two")).unwrap();
        a.apply(&changed).unwrap();
        a.mark_present("track.flac").unwrap();
        assert!(!plan.local_is_current(&a).unwrap());
    }

    #[test]
    fn local_deletion_is_kept_by_default() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "gone.flac", "b3:aa");
        a.record_pending_delete("gone.flac", "b3:aa", Hlc::new(10, 0))
            .unwrap();

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();
        assert_eq!(plan.local.len(), 1);
        assert_eq!(plan.local[0].kind, ChangeKind::Deleted);
        assert_eq!(plan.effective_decision("gone.flac"), Some(Decision::Keep));
        assert!(plan.unresolved().is_empty());
    }

    #[test]
    fn remote_deletion_is_offered_not_applied() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "old.flac", "b3:aa");

        let plan = build(&a, "dev-b", "Phone", &[remote(del("old.flac"), 10)]).unwrap();
        assert_eq!(plan.remote.len(), 1);
        assert_eq!(plan.remote[0].kind, ChangeKind::Deleted);
        assert_eq!(plan.effective_decision("old.flac"), Some(Decision::Keep));
        assert!(plan.conflicts.is_empty());
    }

    #[test]
    fn remote_addition_is_accepted_by_default() {
        let a = Replica::in_memory("dev-a").unwrap();
        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(put("fresh.flac", "b3:bb"), 10)],
        )
        .unwrap();
        assert_eq!(plan.remote.len(), 1);
        assert_eq!(plan.remote[0].kind, ChangeKind::Added);
        assert_eq!(plan.effective_decision("fresh.flac"), Some(Decision::Accept));
    }

    #[test]
    fn both_sides_change_different_blobs_is_a_conflict() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "song.flac", "b3:base");
        let op = a.make_op(put("song.flac", "b3:local")).unwrap();
        a.apply(&op).unwrap();

        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(put("song.flac", "b3:remote"), 10)],
        )
        .unwrap();
        assert!(plan.local.is_empty());
        assert!(plan.remote.is_empty());
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].local_blob, "b3:local");
        assert_eq!(plan.conflicts[0].remote_blob, "b3:remote");
        assert_eq!(plan.unresolved(), vec!["song.flac".to_string()]);
        assert_eq!(plan.effective_decision("song.flac"), None);
    }

    #[test]
    fn same_change_on_both_sides_is_not_a_conflict() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "song.flac", "b3:base");
        let op = a.make_op(put("song.flac", "b3:same")).unwrap();
        a.apply(&op).unwrap();

        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(put("song.flac", "b3:same"), 10)],
        )
        .unwrap();
        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.effective_decision("song.flac"), Some(Decision::Accept));
    }

    #[test]
    fn local_edit_vs_remote_delete_is_a_conflict() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "song.flac", "b3:base");
        let op = a.make_op(put("song.flac", "b3:local")).unwrap();
        a.apply(&op).unwrap();

        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(del("song.flac"), 10)],
        )
        .unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].reason, ConflictReason::ChangedVsDeleted);
    }

    #[test]
    fn remote_move_is_one_change_with_source() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "old/name.flac", "b3:aa");

        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(
                OpKind::EntryMove {
                    from: "old/name.flac".into(),
                    to: "new/name.flac".into(),
                    blob: "b3:aa".into(),
                    size: 5,
                    mtime_ns: 2,
                    added_at: 2,
                },
                10,
            )],
        )
        .unwrap();
        assert_eq!(plan.remote.len(), 1);
        assert_eq!(plan.remote[0].kind, ChangeKind::Moved);
        assert_eq!(plan.remote[0].from.as_deref(), Some("old/name.flac"));
        assert_eq!(plan.effective_decision("new/name.flac"), Some(Decision::Accept));
    }

    #[test]
    fn move_over_a_locally_changed_source_is_a_conflict() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "old.flac", "b3:base");
        let op = a.make_op(put("old.flac", "b3:edited")).unwrap();
        a.apply(&op).unwrap();

        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(
                OpKind::EntryMove {
                    from: "old.flac".into(),
                    to: "new.flac".into(),
                    blob: "b3:base".into(),
                    size: 8,
                    mtime_ns: 2,
                    added_at: 2,
                },
                10,
            )],
        )
        .unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].local_path, "old.flac");
        assert_eq!(plan.conflicts[0].remote_path, "new.flac");
    }

    #[test]
    fn decisions_merge_by_hlc_per_path() {
        let mut set = DecisionSet::default();
        set.entries.insert(
            "a.flac".into(),
            PathDecision {
                path: "a.flac".into(),
                action: Decision::Keep,
                blob: "b3:aa".into(),
                hlc: Hlc::new(10, 0),
                dev: "dev-a".into(),
            },
        );

        let mut newer = DecisionSet::default();
        newer.entries.insert(
            "a.flac".into(),
            PathDecision {
                path: "a.flac".into(),
                action: Decision::Accept,
                blob: "b3:aa".into(),
                hlc: Hlc::new(20, 0),
                dev: "dev-b".into(),
            },
        );
        assert_eq!(set.merge(&newer), 1);
        assert_eq!(set.get("a.flac").unwrap().action, Decision::Accept);

        // Старое решение не перетирает новое.
        assert_eq!(set.merge(&newer), 0);
    }

    #[test]
    fn decision_is_dropped_when_blob_changes() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let op = a.make_op(put("new.flac", "b3:one")).unwrap();
        a.apply(&op).unwrap();
        let mut plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        let mut decisions = DecisionSet::default();
        decisions.entries.insert(
            "new.flac".into(),
            PathDecision {
                path: "new.flac".into(),
                action: Decision::Keep,
                blob: "b3:one".into(),
                hlc: Hlc::new(10, 0),
                dev: "dev-a".into(),
            },
        );
        plan.decisions = decisions;
        let mut stored = plan.decisions.clone();
        assert_eq!(stored.retain_valid(&plan), 0);
        stored.entries.get_mut("new.flac").unwrap().blob = "b3:two".into();
        assert_eq!(stored.retain_valid(&plan), 1);
        assert!(stored.is_empty());
    }

    #[test]
    fn decisions_hash_reflects_explicit_decisions() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let op = a.make_op(put("new.flac", "b3:one")).unwrap();
        a.apply(&op).unwrap();
        let mut plan = build(&a, "dev-b", "Phone", &[]).unwrap();
        let before = plan.decisions_hash();

        plan.decisions.entries.insert(
            "new.flac".into(),
            PathDecision {
                path: "new.flac".into(),
                action: Decision::Keep,
                blob: "b3:one".into(),
                hlc: Hlc::new(10, 0),
                dev: "dev-a".into(),
            },
        );
        assert_ne!(before, plan.decisions_hash());
    }

    #[test]
    fn decisions_hash_ignores_paths_without_explicit_decisions() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let op = a.make_op(put("new.flac", "b3:one")).unwrap();
        a.apply(&op).unwrap();
        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();
        let before = plan.decisions_hash();

        // У второй стороны может быть лишняя строка плана: например, файл,
        // который здесь помечен `local_ignored` и потому в план не попал.
        // Решения по нему нет, поэтому набор решений остаётся тем же.
        let mut with_extra = plan.clone();
        let mut extra = with_extra.local[0].clone();
        extra.path = "ignored.flac".into();
        with_extra.local.push(extra);
        assert_eq!(before, with_extra.decisions_hash());

        // Явное решение по этому пути уже обязано изменить хэш.
        with_extra.decisions.entries.insert(
            "ignored.flac".into(),
            PathDecision {
                path: "ignored.flac".into(),
                action: Decision::Keep,
                blob: "b3:one".into(),
                hlc: Hlc::new(11, 0),
                dev: "dev-a".into(),
            },
        );
        assert_ne!(before, with_extra.decisions_hash());
    }

    #[test]
    fn building_a_plan_does_not_write_to_the_database() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "base.flac", "b3:base");
        a.record_pending_delete("base.flac", "b3:base", Hlc::new(10, 0))
            .unwrap();
        let hash_before = a.state_hash().unwrap();
        let ops_before = a.op_count().unwrap();

        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(put("other.flac", "b3:other"), 10)],
        )
        .unwrap();
        assert!(!plan.local.is_empty());

        assert_eq!(a.state_hash().unwrap(), hash_before);
        assert_eq!(a.op_count().unwrap(), ops_before);
        assert!(a.load_decisions("dev-b").unwrap().is_empty());
    }

    #[test]
    fn plan_expires_after_ttl() {
        let a = Replica::in_memory("dev-a").unwrap();
        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();
        assert!(!plan.is_expired(plan.created_at));
        assert!(plan.is_expired(plan.created_at + PLAN_TTL_MS));
    }

    #[test]
    fn plan_and_decisions_roundtrip_through_the_database() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let op = a.make_op(put("new.flac", "b3:one")).unwrap();
        a.apply(&op).unwrap();
        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();
        a.save_plan(&plan).unwrap();

        let loaded = a.load_plan("dev-b", Hlc::wall_ms()).unwrap().unwrap();
        assert_eq!(loaded.peer_dev, "dev-b");
        assert_eq!(loaded.peer_name, "Phone");
        assert_eq!(loaded.fingerprint(), plan.fingerprint());

        let mut set = DecisionSet::default();
        set.entries.insert(
            "new.flac".into(),
            PathDecision {
                path: "new.flac".into(),
                action: Decision::Keep,
                blob: "b3:one".into(),
                hlc: Hlc::new(10, 0),
                dev: "dev-a".into(),
            },
        );
        a.save_decisions("dev-b", &set).unwrap();
        let loaded = a.load_plan("dev-b", Hlc::wall_ms()).unwrap().unwrap();
        assert_eq!(loaded.decisions.get("new.flac").unwrap().action, Decision::Keep);

        assert!(a.clear_plan("dev-b").unwrap());
        assert!(a.load_plan("dev-b", Hlc::wall_ms()).unwrap().is_none());
    }

    #[test]
    fn expired_plan_is_not_loaded() {
        let a = Replica::in_memory("dev-a").unwrap();
        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();
        a.save_plan(&plan).unwrap();
        let later = plan.expires_at + 1;
        assert!(a.load_plan("dev-b", later).unwrap().is_none());
        // Просроченный план не должен оставаться в таблице.
        assert!(a.load_plan("dev-b", later).unwrap().is_none());
    }

    #[test]
    fn stale_decision_is_dropped_when_loading_the_plan() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let op = a.make_op(put("new.flac", "b3:one")).unwrap();
        a.apply(&op).unwrap();
        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();
        a.save_plan(&plan).unwrap();

        let mut set = DecisionSet::default();
        set.entries.insert(
            "new.flac".into(),
            PathDecision {
                path: "new.flac".into(),
                action: Decision::Keep,
                blob: "b3:other".into(),
                hlc: Hlc::new(10, 0),
                dev: "dev-a".into(),
            },
        );
        a.save_decisions("dev-b", &set).unwrap();

        let loaded = a.load_plan("dev-b", Hlc::wall_ms()).unwrap().unwrap();
        assert!(loaded.decisions.is_empty());
    }

    #[test]
    fn database_decisions_merge_by_hlc() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let mut first = DecisionSet::default();
        first.entries.insert(
            "x.flac".into(),
            PathDecision {
                path: "x.flac".into(),
                action: Decision::Keep,
                blob: "b3:x".into(),
                hlc: Hlc::new(10, 0),
                dev: "dev-a".into(),
            },
        );
        a.save_decisions("dev-b", &first).unwrap();

        let mut newer = DecisionSet::default();
        newer.entries.insert(
            "x.flac".into(),
            PathDecision {
                path: "x.flac".into(),
                action: Decision::Accept,
                blob: "b3:x".into(),
                hlc: Hlc::new(20, 0),
                dev: "dev-b".into(),
            },
        );
        let (merged, changed) = a.merge_decisions("dev-b", &newer).unwrap();
        assert_eq!(changed, 1);
        assert_eq!(merged.get("x.flac").unwrap().action, Decision::Accept);
        assert_eq!(a.load_decisions("dev-b").unwrap().get("x.flac").unwrap().action, Decision::Accept);

        // Повторное слияние ничего не меняет.
        let (_, changed) = a.merge_decisions("dev-b", &newer).unwrap();
        assert_eq!(changed, 0);
    }

    fn decision(path: &str, action: Decision, blob: &str, ms: u64, dev: &str) -> PathDecision {
        PathDecision {
            path: path.into(),
            action,
            blob: blob.into(),
            hlc: Hlc::new(ms, 0),
            dev: dev.into(),
        }
    }

    #[test]
    fn unset_beats_a_stale_peer_decision_and_stays_forgotten() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let op = a.make_op(put("new.flac", "b3:one")).unwrap();
        a.apply(&op).unwrap();
        let mut plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        let mut peer = DecisionSet::default();
        peer.entries.insert(
            "new.flac".into(),
            decision("new.flac", Decision::KeepInLibrary, "b3:one", 10, "dev-b"),
        );
        plan.decisions.merge(&peer);
        assert_eq!(
            plan.explicit_decision("new.flac"),
            Some(Decision::KeepInLibrary)
        );

        let mut mine = DecisionSet::default();
        mine.entries.insert(
            "new.flac".into(),
            decision("new.flac", Decision::Unset, "b3:one", 20, "dev-a"),
        );
        plan.decisions.merge(&mine);
        assert_eq!(plan.explicit_decision("new.flac"), None);
        // Для добавления возвращается действие по умолчанию, а не решение.
        assert_eq!(
            plan.effective_decision("new.flac"),
            Some(Decision::Accept)
        );

        // Отмена — надгробие: не выбрасывается, даже если путь исчез из плана.
        let mut stored = plan.decisions.clone();
        stored.retain_valid(&plan);
        assert_eq!(
            stored.get("new.flac").map(|d| d.action),
            Some(Decision::Unset)
        );

        // Устаревшее решение пира не воскресает при повторном слиянии.
        plan.decisions.merge(&peer);
        assert_eq!(plan.explicit_decision("new.flac"), None);

        // Новое осознанное решение с более свежим HLC снова работает.
        let mut again = DecisionSet::default();
        again.entries.insert(
            "new.flac".into(),
            decision("new.flac", Decision::OnlyHere, "b3:one", 30, "dev-a"),
        );
        plan.decisions.merge(&again);
        assert_eq!(plan.explicit_decision("new.flac"), Some(Decision::OnlyHere));
    }

    #[test]
    fn pending_delete_keeps_full_hlc_and_is_not_rewritten() {
        let a = Replica::in_memory("dev-a").unwrap();
        a.record_pending_delete("gone.flac", "b3:aa", Hlc::new(42, 7))
            .unwrap();
        a.record_pending_delete("gone.flac", "b3:aa", Hlc::new(99, 1))
            .unwrap();

        let pending = a.pending_deletes().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].hlc(), Hlc::new(42, 7));
        assert_eq!(pending[0].detected_at, 42);
        assert_eq!(pending[0].counter, 7);
    }

    #[test]
    fn deleted_never_shared_file_is_not_a_change() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let op = a.make_op(put("draft.flac", "b3:draft")).unwrap();
        a.apply(&op).unwrap();
        a.mark_present("draft.flac").unwrap();

        // Черновик ни разу не подтверждали как общую базу.
        a.record_pending_delete("draft.flac", "b3:draft", Hlc::new(10, 0))
            .unwrap();
        a.mark_absent("draft.flac").unwrap();

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();
        assert!(plan.local.is_empty(), "призрак не должен быть изменением");
        assert!(plan.conflicts.is_empty());
        assert!(plan.unresolved().is_empty());

        // Скан чистит такой призрак целиком, а не оставляет вечное удаление.
        assert_eq!(a.prune_ghost_deletes().unwrap(), 1);
        assert!(a.pending_deletes().unwrap().is_empty());
        assert!(a.plan_rows().unwrap().iter().all(|row| row.path != "draft.flac"));
    }
}
