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
use crate::path;
use crate::store::{LocalMove, PathState, PathStateRow, Replica};

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
    /// Файл переименован в той же папке.
    Renamed,
    /// Файл перенесён в другую папку (возможно, с новым именем).
    Moved,
    Deleted,
}

impl ChangeKind {
    /// Действие по умолчанию. Это смысл операции, а не политика интерфейса:
    /// удаления никогда не включаются сами, остальные изменения применяются.
    pub fn default_decision(self) -> Decision {
        match self {
            ChangeKind::Added
            | ChangeKind::Updated
            | ChangeKind::Renamed
            | ChangeKind::Moved => Decision::Accept,
            ChangeKind::Deleted => Decision::Keep,
        }
    }

    /// Отличить переименование от переноса по родительским папкам. Регистр и
    /// NFC не должны превращать смену регистра имени в перенос.
    pub fn for_move(from: &str, to: &str) -> Self {
        if same_folder(from, to) {
            ChangeKind::Renamed
        } else {
            ChangeKind::Moved
        }
    }
}

fn parent(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn same_folder(from: &str, to: &str) -> bool {
    path::key(parent(from)) == path::key(parent(to))
}

/// Старый путь переноса, если он всё ещё описывает текущее содержимое строки.
///
/// Одна и та же проверка нужна при построении плана и при проверке его
/// актуальности перед commit. Иначе план мог сказать «добавление», а к моменту
/// commit в журнале уже лежал `entry_move` — и путь уехал бы пиру как `put`.
fn local_move_from(
    by_path: &HashMap<&str, &PathStateRow>,
    move_to: &HashMap<&str, &LocalMove>,
    row: &PathStateRow,
) -> Option<String> {
    move_to.get(row.path.as_str()).and_then(|mv| {
        let source_is_gone = by_path
            .get(mv.from.as_str())
            .map_or(true, |source| source.state == PathState::Tombstone);
        (mv.from != row.path
            && mv.blob == row.blob
            && source_is_gone
            && (mv.hlc == row.hlc || row.base.is_none())
            && move_chain_is_real(mv, &row.blob, by_path, move_to))
        .then(|| mv.from.clone())
    })
}

/// Настоящий ли перенос: цепочка либо начинается с согласованной базы, либо
/// не замыкается. Замкнутая цепочка без базы — это возврат отклонённого
/// переноса: файл вернулся на прежнее место и остаётся добавлением.
fn move_chain_is_real(
    mv: &LocalMove,
    blob: &str,
    by_path: &HashMap<&str, &PathStateRow>,
    move_to: &HashMap<&str, &LocalMove>,
) -> bool {
    let mut current = mv.from.as_str();
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
            return true;
        };
        if previous.blob != blob {
            return true;
        }
        current = previous.from.as_str();
    }
}

/// Согласованная база пути; пустая строка — базы ещё нет.
fn base_of(row: &PathStateRow) -> &str {
    row.base.as_deref().unwrap_or("")
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
    /// Снимок решений подмешивается при загрузке из `plan_decisions`.
    /// В JSON плана он не входит: решения живут отдельной таблицей.
    #[serde(skip)]
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

    /// Итоговое действие по каждому пути и список нерешённых конфликтов.
    /// Строится в момент commit или отображения, но не хранится в плане:
    /// план описывает факты, а решение — отдельный слой.
    pub fn resolve(&self) -> Resolution {
        Resolution::new(self)
    }

    /// Отпечаток набора решений для статуса сеанса. Для сверки сторон он не
    /// годится: планы строятся независимо, и локально игнорируемый путь есть
    /// только у одной стороны. Сверка идёт по [`states_disagree`].
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
        for conflict in &self.conflicts {
            if self.explicit_entry(&conflict.path).is_none() {
                hasher.update(b"unresolved\0");
                hasher.update(conflict.path.as_bytes());
                hasher.update(b"\n");
            }
        }
        format!("b3:{}", hasher.finalize().to_hex())
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
            hasher.update(&[change.kind as u8]);
            hasher.update(b"\0");
            hasher.update(change.from.as_deref().unwrap_or("").as_bytes());
            hasher.update(b"\0");
            hasher.update(change.base_blob.as_bytes());
            hasher.update(b"\0");
            hasher.update(&[u8::from(change.ignored), u8::from(change.aligned)]);
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
        // То же состояние БД, что увидит commit: состояния путей и переносы
        // читаются одним снимком.
        let me = replica.device_id().to_string();
        let (states, local_moves) =
            replica.read_snapshot(|| Ok((replica.path_states()?, replica.local_moves(&me)?)))?;
        let by_path: HashMap<&str, &PathStateRow> =
            states.iter().map(|row| (row.path.as_str(), row)).collect();
        let move_to: HashMap<&str, &LocalMove> = local_moves
            .iter()
            .map(|mv| (mv.to.as_str(), mv))
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
                match row.state {
                    PathState::Missing | PathState::Tombstone => {}
                    _ => return Ok(false),
                }
                let base = if base_of(row).is_empty() {
                    change.blob.as_str()
                } else {
                    base_of(row)
                };
                if base != change.blob {
                    return Ok(false);
                }
            } else {
                if row.state == PathState::Tombstone
                    || row.blob != change.blob
                    || row.state.is_excluded() != change.ignored
                    || row.present != change.present
                {
                    return Ok(false);
                }
                // За время между планом и commit скан мог распознать перенос
                // (или наоборот — путь переписали после переноса). План с
                // неверным `from`/видом отправлять нельзя: иначе перенос уйдёт
                // как `put`, а старый путь останется на пире навсегда.
                let current_from = local_move_from(&by_path, &move_to, row);
                if current_from != change.from {
                    return Ok(false);
                }
                let kind = if let Some(from) = &current_from {
                    ChangeKind::for_move(from, &row.path)
                } else if base_of(row).is_empty() {
                    ChangeKind::Added
                } else {
                    ChangeKind::Updated
                };
                if kind != change.kind {
                    return Ok(false);
                }
            }
        }

        // Каждый действующий перенос обязан быть строкой плана. Проверка выше
        // ловит неверный вид уже существующей строки; эта — полностью
        // потерянный перенос, когда цель по содержимому совпала с базой.
        for (to, mv) in &move_to {
            let Some(target) = by_path.get(to) else {
                continue;
            };
            if target.state == PathState::Tombstone
                || target.state.is_excluded()
                // Перенос уже вошёл в общую базу: отдельной строки плана нет
                // и не должно быть.
                || target.blob == base_of(target)
            {
                continue;
            }
            if local_move_from(&by_path, &move_to, target).as_deref() != Some(mv.from.as_str()) {
                continue;
            }
            let described = self.local.iter().any(|change| {
                change.path.as_str() == *to
                    && change.from.as_deref() == Some(mv.from.as_str())
                    && change.kind == ChangeKind::for_move(&mv.from, to)
            });
            if !described {
                return Ok(false);
            }
        }
        // Появившийся новый локальный путь тоже делает план неполным.
        // Мягкое удаление, которое пир уже перекрыл более новой операцией с
        // тем же блобом, живёт в плане строкой пира, а не локальной. Без этой
        // поправки телефон отказывал бы в commit после отмены исключения.
        let remote_paths: HashSet<&str> = self
            .remote
            .iter()
            .filter(|change| change.kind != ChangeKind::Deleted)
            .map(|change| change.path.as_str())
            .collect();
        for row in &states {
            if row.state.is_excluded() || row.state == PathState::Tombstone {
                continue;
            }
            if conflicts.contains(row.path.as_str()) {
                continue;
            }
            let changed = row.blob != base_of(row);
            let pending_delete = row.state == PathState::Missing;
            if pending_delete && base_of(row).is_empty() {
                // Черновик, удалённый до первой синхронизации, не изменение.
                continue;
            }
            let in_plan = self
                .local
                .iter()
                .any(|change| change.path == row.path);
            if (changed || pending_delete)
                && !in_plan
                && !(pending_delete && remote_paths.contains(row.path.as_str()))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Итоговые действия по плану: явное решение человека важнее базового смысла
/// операции. Строится в момент commit или отображения и нигде не хранится:
/// `Plan` описывает факты, `DecisionSet` — решения, а `Resolution` — их итог.
///
/// Конфликты не имеют умолчания: без явного решения путь остаётся нерешённым.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    actions: BTreeMap<String, Decision>,
    unresolved: Vec<String>,
}

impl Resolution {
    pub fn new(plan: &Plan) -> Self {
        let mut actions = BTreeMap::new();
        for change in plan.local.iter().chain(plan.remote.iter()) {
            let action = if change.ignored {
                // Локально исключённый путь не должен исчезнуть или уехать.
                Decision::Keep
            } else if change.aligned {
                // Содержимое уже совпало: остаётся только зафиксировать базу.
                Decision::Accept
            } else {
                change.kind.default_decision()
            };
            actions.insert(change.path.clone(), action);
        }

        let mut unresolved = Vec::new();
        for conflict in &plan.conflicts {
            match plan.explicit_decision(&conflict.path) {
                Some(action) => {
                    actions.insert(conflict.path.clone(), action);
                }
                None => {
                    actions.remove(&conflict.path);
                    unresolved.push(conflict.path.clone());
                }
            }
        }

        // Явное решение по обычному пути перекрывает базовое действие.
        // Конфликты уже разобраны выше: у них нет умолчания.
        for (path, decision) in &plan.decisions.entries {
            if decision.action == Decision::Unset || plan.conflict(path).is_some() {
                continue;
            }
            if plan.change(path).is_some() {
                actions.insert(path.clone(), decision.action);
            }
        }

        Self {
            actions,
            unresolved,
        }
    }

    /// Итоговое действие по пути. `None` — только нерешённый конфликт.
    pub fn action(&self, path: &str) -> Option<Decision> {
        self.actions.get(path).copied()
    }

    /// Конфликты без явного решения. Обычные изменения всегда разрешимы:
    /// у них есть базовое действие.
    pub fn unresolved(&self) -> &[String] {
        &self.unresolved
    }

    /// Итоговая карта для сверки сторон. Нерешённый конфликт тоже входит в
    /// состояние: иначе одна сторона с решением и вторая без него выглядели
    /// бы согласованными.
    pub fn state(&self) -> BTreeMap<String, String> {
        let mut state: BTreeMap<String, String> = self
            .actions
            .iter()
            .map(|(path, action)| (path.clone(), action.as_str().to_string()))
            .collect();
        for path in &self.unresolved {
            state.insert(path.clone(), "unresolved".to_string());
        }
        state
    }
}

/// Расходятся ли стороны: есть ли общий путь с разными итоговыми действиями.
///
/// Сравнивать карты целиком нельзя: план каждой стороны строится независимо,
/// и локально игнорируемый путь (`local_ignored`) есть только у одной. Такой
/// путь не общий, и commit всё равно его не тронет. Расхождением считается
/// только разное действие по одному и тому же пути.
pub fn states_disagree(
    mine: &BTreeMap<String, String>,
    theirs: &BTreeMap<String, String>,
) -> bool {
    mine.iter().any(|(path, action)| {
        theirs
            .get(path)
            .is_some_and(|other| other.as_str() != action.as_str())
    })
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

    // Все входы плана должны быть из одного состояния БД. Иначе перенос
    // может быть уже в `entries`, но ещё не в `ops` (скан пишет несколькими
    // транзакциями), и новый путь превратится в «добавление».
    let me = replica.device_id().to_string();
    let (states, local_moves, removals) = replica.read_snapshot(|| {
        Ok((
            replica.path_states()?,
            replica.local_moves(&me)?,
            replica.removal_hlcs(&me)?,
        ))
    })?;
    let by_path: HashMap<&str, &PathStateRow> =
        states.iter().map(|row| (row.path.as_str(), row)).collect();
    let remote = remote_state(remote_ops);

    // Последний перенос в каждый путь. В `path_state` связи «откуда» нет,
    // поэтому без журнала новый путь неотличим от добавления.
    let mut move_to: HashMap<&str, &LocalMove> = HashMap::new();
    for mv in &local_moves {
        move_to.insert(mv.to.as_str(), mv);
    }

    // Источники переносов, которые всё ещё описывают текущее содержимое цели.
    // Их удаление не показываем отдельной строкой: иначе «принять перенос» и
    // «оставить удаление» противоречили бы друг другу. Если после переноса
    // содержимое изменилось, это уже не перенос, и старый путь должен
    // остаться в плане обычным удалением — иначе на телефоне будет дубль.
    let mut local_move_sources: HashSet<&str> = HashSet::new();
    for mv in &local_moves {
        let Some(target) = by_path.get(mv.to.as_str()) else {
            continue;
        };
        if target.state == PathState::Tombstone || target.state.is_excluded() {
            continue;
        }
        let still_describes_target =
            mv.blob == target.blob && (mv.hlc == target.hlc || target.base.is_none());
        if !still_describes_target {
            continue;
        }
        // Цепочка a→b→c: цель у c, но старый файл на телефоне может называться
        // a. Пока блоб тот же, все промежуточные источники — часть переноса.
        // Журнал хранит и обратный перенос после «Отклонить», поэтому цепочка
        // может замкнуться; посещённые источники обрывают обход.
        let mut current = mv;
        let mut visited: HashSet<&str> = HashSet::new();
        loop {
            if !visited.insert(current.from.as_str()) {
                break;
            }
            local_move_sources.insert(current.from.as_str());
            let Some(previous) = move_to.get(current.from.as_str()).copied() else {
                break;
            };
            if previous.blob != current.blob {
                break;
            }
            current = previous;
        }
    }

    let mut local = Vec::new();
    let mut conflicts = Vec::new();
    let mut conflict_paths: HashSet<String> = HashSet::new();

    // ---------------------------------------------------------- локальные
    for row in &states {
        if row.state.is_excluded() {
            // «Только здесь» или явное исключение: путь не участвует в плане
            // ни с одной стороны, пока человек не вернёт его в синхронизацию.
            continue;
        }
        match row.state {
            PathState::ExcludedLocal | PathState::ExcludedRemote => unreachable!(),
            PathState::ResurrectingRemote => {
                // Ожидание копии от пира: синтетическую строку добавит
                // удалённый проход, локального изменения здесь нет.
                continue;
            }
            PathState::ResurrectingLocal => {
                // Возврат локальной копии — обычное добавление с чистой базой.
                let aligned = remote
                    .get(&row.path)
                    .is_some_and(|state| state.present && state.blob == row.blob);
                local.push(Change {
                    path: row.path.clone(),
                    from: None,
                    kind: ChangeKind::Added,
                    side: Side::Local,
                    blob: row.blob.clone(),
                    base_blob: String::new(),
                    size: row.size,
                    mtime_ns: row.mtime_ns,
                    present: row.present,
                    ignored: false,
                    aligned,
                });
                continue;
            }
            PathState::Missing => {
                // Путь ни разу не входил в общую базу: пир о нём не знает, и
                // «удаление» здесь — не изменение, а просто исчезнувший
                // черновик. Сканер такие записи забывает.
                if row.base.is_none() {
                    continue;
                }
                // Пир уже вернул тот же блоб более новой операцией. Наше «файл
                // пропал» устарело: файл придёт от пира, а не будет удалён.
                let restored_by_peer = remote.get(&row.path).is_some_and(|state| {
                    state.present
                        && state.blob == row.blob
                        && row.pending_hlc.is_some_and(|pending| state.hlc > pending)
                });
                if restored_by_peer {
                    continue;
                }
                local.push(Change {
                    path: row.path.clone(),
                    from: None,
                    kind: ChangeKind::Deleted,
                    side: Side::Local,
                    blob: row.blob.clone(),
                    base_blob: base_of(row).to_string(),
                    size: row.size,
                    mtime_ns: row.mtime_ns,
                    present: false,
                    ignored: false,
                    aligned: false,
                });
                continue;
            }
            PathState::Tombstone => {
                if local_move_sources.contains(row.path.as_str()) {
                    // Старый путь уже описан строкой переноса — отдельное
                    // удаление заставило бы выбирать между двумя решениями.
                    continue;
                }
                // Подтверждённое удаление несём тому пиру, у которого файл ещё
                // лежит (или который ещё не применил нашу операцию удаления).
                // Иначе после общего удаления путь всплывал бы в плане вечно,
                // хотя обе стороны уже согласны.
                let peer = remote.get(&row.path);
                // Пир уже вернул файл более новой операцией. Это добавление с
                // его стороны, а не наше удаление.
                let restored_by_peer =
                    peer.is_some_and(|state| state.present && state.hlc > row.hlc);
                let peer_deleted = peer.is_some_and(|state| !state.present);
                let peer_has_it = peer.is_some_and(|state| state.present)
                    || (!peer_deleted
                        && !peer_applied_delete(
                            peer_have,
                            &me,
                            removals.get(&row.path).copied(),
                        ));
                if peer_has_it && !base_of(row).is_empty() && !restored_by_peer {
                    local.push(Change {
                        path: row.path.clone(),
                        from: None,
                        kind: ChangeKind::Deleted,
                        side: Side::Local,
                        blob: base_of(row).to_string(),
                        base_blob: base_of(row).to_string(),
                        size: row.size,
                        mtime_ns: row.mtime_ns,
                        present: false,
                        ignored: false,
                        aligned: false,
                    });
                }
                continue;
            }
            PathState::Live => {}
        }

        // Live: файл есть здесь или ждёт скачивания.
        if row.blob == base_of(row) {
            continue;
        }
        if row.base.is_none() && !row.present && row.dev != me {
            // Чужое добавление ещё не скачано: это не локальное изменение,
            // его покажет удалённый проход.
            continue;
        }
        // Файл уже идентичен версии пира. Базы может не быть, но данные
        // передавать незачем: коммит лишь зафиксирует совпадение.
        let aligned = row.present
            && remote
                .get(&row.path)
                .is_some_and(|state| state.present && state.blob == row.blob);

        // Сканер уже распознал переименование и записал `entry_move`.
        // Проверяем, что перенос всё ещё относится к текущему содержимому:
        // если путь позже переписали другим блобом, это обычная правка.
        let moved_from = local_move_from(&by_path, &move_to, row);

        local.push(Change {
            path: row.path.clone(),
            from: moved_from.clone(),
            kind: if let Some(from) = &moved_from {
                ChangeKind::for_move(from, &row.path)
            } else if row.base.is_none() {
                ChangeKind::Added
            } else {
                ChangeKind::Updated
            },
            side: Side::Local,
            blob: row.blob.clone(),
            base_blob: base_of(row).to_string(),
            size: row.size,
            mtime_ns: row.mtime_ns,
            present: row.present,
            ignored: false,
            aligned,
        });
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
        let row = by_path.get(path.as_str()).copied();
        if row.is_some_and(|row| row.state.is_excluded()) {
            // Явное исключение: пир может продолжать слать свой журнал, но
            // этот путь не участвует в синхронизации.
            continue;
        }

        // Журнал пира может содержать давнюю операцию, которая до нас не
        // доехала: watermark двигается только по применённым операциям, а не
        // по всей истории. Если наше состояние пути уже новее (LWW: HLC, затем
        // автор) и пир по своему watermark уже видел эту нашу операцию, значит
        // присланная запись — хвост истории, а не текущее состояние пира.
        // Тянуть для неё блоб нельзя: у пира его может давно не быть.
        //
        // Без watermark пира судить нельзя: при одновременной правке обеих
        // сторон более старый HLC — всё ещё настоящее изменение, и его нужно
        // показать как конфликт.
        if let Some(row) = row {
            let mine = (row.hlc.ms, row.hlc.counter, row.dev.as_str());
            let theirs = (state.hlc.ms, state.hlc.counter, state.dev.as_str());
            let peer_has_ours = peer_have
                .and_then(|have| have.get(&row.dev))
                .is_some_and(|&(ms, counter)| {
                    (ms, counter) >= (row.hlc.ms as i64, row.hlc.counter as i64)
                });
            if mine > theirs && peer_has_ours {
                continue;
            }
        }

        if state.present {
            // Пир вернул тот же блоб, который у нас «пропал». Его операция
            // новее нашего мягкого удаления, значит файл нужно тянуть, а не
            // удалять: иначе отмена исключения оставляла бы удаление навсегда.
            let pending_superseded = row.is_some_and(|row| {
                row.state == PathState::Missing
                    && row
                        .pending_hlc
                        .is_some_and(|pending| state.blob == row.blob && state.hlc > pending)
            });
            // У удалённой строки общая база есть только до тех пор, пока пир
            // не прислал факт новее самого удаления. Если прислал — это
            // восстановление: прежней базы у пути больше нет, и файл должен
            // вернуться в очередь как добавление, а не потеряться в проверке
            // «у пира лежит та же база». Источник нашего переноса — исключение:
            // пир мог просто ещё не увидеть перенос и присылать старый путь.
            let base_blob = match row {
                Some(_) if pending_superseded => String::new(),
                Some(row)
                    if row.state == PathState::Tombstone
                        && state.hlc > row.hlc
                        && !local_move_sources.contains(row.path.as_str()) =>
                {
                    String::new()
                }
                Some(row) => base_of(row).to_string(),
                None => String::new(),
            };
            let local_changed = match row {
                Some(row) if row.state != PathState::Tombstone => {
                    !pending_superseded
                        && (row.blob != base_of(row) || row.state == PathState::Missing)
                }
                Some(_) => !base_blob.is_empty(),
                None => false,
            };
            let local_blob = row
                .filter(|row| row.state != PathState::Tombstone)
                .map(|row| row.blob.clone())
                .unwrap_or_default();
            let same_content = row
                .filter(|row| row.state != PathState::Tombstone)
                .is_some_and(|row| row.present && row.blob == state.blob);
            let move_changes_old_path = state.moved_from.as_ref().is_some_and(|from| {
                by_path
                    .get(from.as_str())
                    .is_some_and(|row| row.state != PathState::Tombstone && row.base.is_some())
            });
            // Пир мог прислать старую операцию, которая уже является общей
            // базой. Это не его изменение: конфликт только если обе стороны
            // действительно ушли от базы. Возврат файла после нашего мягкого
            // удаления — изменение, даже если блоб совпал с базой.
            let remote_changed = pending_superseded
                || state.moved_from.is_some()
                || row
                    .as_ref()
                    .map_or(true, |row| {
                        row.state == PathState::Tombstone || state.blob != base_of(row)
                    });

            if same_content && !move_changes_old_path {
                continue;
            }

            if row.is_some_and(|row| row.state == PathState::Tombstone)
                && base_blob == state.blob
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
                    // Удалённый путь против другого содержимого пира — это
                    // всегда «изменено против удалено», даже если это мягкое
                    // удаление (`Missing`), а не подтверждённое.
                    Some(row)
                        if row.state == PathState::Tombstone
                            || row.state == PathState::Missing =>
                    {
                        ConflictReason::ChangedVsDeleted
                    }
                    Some(row) if row.base.is_none() => ConflictReason::NoBase,
                    _ => ConflictReason::BothChanged,
                };
                let (local_path, local_size) = row
                    .map(|row| (row.path.clone(), row.size))
                    .unwrap_or_else(|| (path.clone(), 0));
                let local_blob = if let Some(from) = &state.moved_from {
                    by_path
                        .get(from.as_str())
                        .filter(|row| {
                            row.state != PathState::Tombstone && row.blob != base_of(row)
                        })
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
                    row.state != PathState::Tombstone
                        && (row.blob != base_of(row) || row.state == PathState::Missing)
                });
                if source_changed {
                    let source = by_path.get(from.as_str()).copied();
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
                kind: if let Some(from) = &state.moved_from {
                    ChangeKind::for_move(from, path)
                } else if base_blob.is_empty() {
                    ChangeKind::Added
                } else {
                    ChangeKind::Updated
                },
                side: Side::Remote,
                blob: state.blob.clone(),
                base_blob: base_blob.clone(),
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
        if row.state == PathState::Tombstone {
            continue;
        }
        if row.state == PathState::Missing {
            // Обе стороны удалили — это не конфликт.
            continue;
        }
        // Отмена исключения уже вернула локальную копию свежим `EntryPut`.
        // Удаление пира старше неё — хвост его `pending_deletes`, а не
        // намерение убрать общий файл. Иначе отмена снова показывала бы
        // удаление, хотя файл жив.
        if row.state == PathState::ResurrectingLocal && row.present && row.hlc > state.hlc {
            continue;
        }
        if row.blob != base_of(row) {
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
            blob: base_of(row).to_string(),
            base_blob: base_of(row).to_string(),
            size: row.size,
            mtime_ns: row.mtime_ns,
            present: false,
            ignored: false,
            aligned: false,
        });
    }

    // Отмена исключения на стороне пира: у нас файла нет, но он есть у него.
    // Синтетическая строка добавления заставляет commit запросить блоб. Обычный
    // проход её не увидит, если пир не прислал новую операцию.
    for marker in states
        .iter()
        .filter(|row| row.state == PathState::ResurrectingRemote)
    {
        if local.iter().any(|change| change.path == marker.path)
            || remote_changes
                .iter()
                .any(|change| change.path == marker.path)
            || conflict_paths.contains(&marker.path)
        {
            continue;
        }
        // Пир уже удалил файл: скачивать нечего, пусть план покажет удаление.
        if remote
            .get(&marker.path)
            .is_some_and(|state| !state.present)
        {
            continue;
        }
        remote_changes.push(Change {
            path: marker.path.clone(),
            from: None,
            kind: ChangeKind::Added,
            side: Side::Remote,
            blob: marker.blob.clone(),
            base_blob: String::new(),
            size: marker.size,
            mtime_ns: marker.mtime_ns,
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
    /// HLC и автор операции, сформировавшей состояние. Нужны, чтобы отличить
    /// свежее изменение пира от старой записи журнала: пир присылает всё, что
    /// выше нашего watermark, а туда могут попадать операции, которые уже
    /// перекрыты более новой версией с нашей стороны.
    hlc: Hlc,
    dev: String,
}

/// Последнее состояние каждого пути по присланным операциям. Порядок —
/// HLC и затем `dev`, чтобы результат не зависел от порядка доставки.
fn peer_applied_delete(
    peer_have: Option<&HashMap<String, (i64, i64)>>,
    me: &str,
    removal: Option<Hlc>,
) -> bool {
    let Some(have) = peer_have else {
        return false;
    };
    let Some(hlc) = removal else {
        // Своего удаления мы не отправляли: запись закрылась операцией пира,
        // а он про неё уже знает.
        return true;
    };
    have.get(me)
        .is_some_and(|&(ms, counter)| Hlc::new(ms as u64, counter as u32) >= hlc)
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
                        hlc: op.hlc,
                        dev: op.dev.clone(),
                    },
                );
            }
            OpKind::EntryDel { path } => {
                map.insert(
                    path.clone(),
                    RemoteState {
                        path: path.clone(),
                        present: false,
                        hlc: op.hlc,
                        dev: op.dev.clone(),
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
                        hlc: op.hlc,
                        dev: op.dev.clone(),
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
                        hlc: op.hlc,
                        dev: op.dev.clone(),
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

    /// Свежая операция пира: HLC заметно выше локальных (они идут по часам).
    fn remote(kind: OpKind, ms: u64) -> Op {
        Op::new("dev-peer", Hlc::new(Hlc::wall_ms() + 1000 + ms, 0), kind)
    }

    /// Давняя операция, которая до нас так и не доехала.
    fn stale_remote(kind: OpKind) -> Op {
        Op::new("dev-peer", Hlc::new(10, 0), kind)
    }

    /// Локальная запись, которую уже подтвердили как общую базу.
    fn synced(replica: &mut Replica, path: &str, blob: &str) {
        let op = replica.make_op(put(path, blob)).unwrap();
        replica.apply(&op).unwrap();
        replica.mark_present(path).unwrap();
        replica.confirm_synced().unwrap();
    }

    /// Локальное добавление, которое ещё не подтверждали как общую базу.
    fn added(replica: &mut Replica, path: &str, blob: &str) {
        let op = replica.make_op(put(path, blob)).unwrap();
        replica.apply(&op).unwrap();
        replica.mark_present(path).unwrap();
    }

    /// То, что делает сканер, увидев переименование: одна операция переноса
    /// и обновлённые флаги присутствия. Файл на диске здесь не нужен.
    fn apply_move(replica: &mut Replica, from: &str, to: &str, blob: &str) {
        let op = replica
            .make_op(OpKind::EntryMove {
                from: from.into(),
                to: to.into(),
                blob: blob.into(),
                size: blob.len() as u64,
                mtime_ns: 2,
                added_at: 1,
            })
            .unwrap();
        replica.apply(&op).unwrap();
        replica.mark_present(to).unwrap();
        replica.mark_absent(from).unwrap();
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
        assert_eq!(plan.resolve().action("new.flac"), Some(Decision::Accept));
    }

    #[test]
    fn local_rename_is_one_change_with_source() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "Telegram/old.flac", "b3:aa");
        apply_move(&mut a, "Telegram/old.flac", "Telegram/new.flac", "b3:aa");

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        assert_eq!(plan.local.len(), 1, "перенос не должен двоиться");
        assert_eq!(plan.local[0].path, "Telegram/new.flac");
        assert_eq!(plan.local[0].kind, ChangeKind::Renamed);
        assert_eq!(
            plan.local[0].from.as_deref(),
            Some("Telegram/old.flac"),
            "старый путь должен быть виден в плане"
        );
        assert_eq!(
            plan.resolve().action("Telegram/new.flac"),
            Some(Decision::Accept)
        );
    }

    #[test]
    fn local_rename_while_phone_still_has_the_old_path() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "old.flac", "b3:aa");
        apply_move(&mut a, "old.flac", "new.flac", "b3:aa");

        // Телефон присылает своё старое добавление: у него всё ещё old.flac.
        let plan = build(&a, "dev-b", "Phone", &[remote(put("old.flac", "b3:aa"), 10)])
            .unwrap();

        assert_eq!(plan.local.len(), 1);
        assert_eq!(plan.local[0].kind, ChangeKind::Renamed);
        assert_eq!(plan.local[0].from.as_deref(), Some("old.flac"));
        assert!(plan.remote.is_empty(), "старая версия телефона — не его правка");
    }

    #[test]
    fn local_move_to_another_folder_is_moved() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "Album/01.flac", "b3:aa");
        apply_move(&mut a, "Album/01.flac", "Compilations/01.flac", "b3:aa");

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        assert_eq!(plan.local.len(), 1);
        assert_eq!(plan.local[0].kind, ChangeKind::Moved);
        assert_eq!(plan.local[0].path, "Compilations/01.flac");
        assert_eq!(plan.local[0].from.as_deref(), Some("Album/01.flac"));
    }

    #[test]
    fn local_move_and_rename_is_moved() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "Album/01.flac", "b3:aa");
        apply_move(&mut a, "Album/01.flac", "Compilations/02.flac", "b3:aa");

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        assert_eq!(plan.local.len(), 1);
        assert_eq!(plan.local[0].kind, ChangeKind::Moved);
        assert_eq!(plan.local[0].path, "Compilations/02.flac");
        assert_eq!(plan.local[0].from.as_deref(), Some("Album/01.flac"));
    }

    #[test]
    fn local_case_only_rename_is_renamed() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "Album/Panda.flac", "b3:aa");
        apply_move(&mut a, "Album/Panda.flac", "album/panda.flac", "b3:aa");

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        assert_eq!(plan.local.len(), 1);
        assert_eq!(plan.local[0].kind, ChangeKind::Renamed);
        assert_eq!(plan.local[0].path, "album/panda.flac");
        assert_eq!(plan.local[0].from.as_deref(), Some("Album/Panda.flac"));
    }

    #[test]
    fn local_move_over_an_existing_target_is_still_a_move() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "source.flac", "b3:source");
        synced(&mut a, "target.flac", "b3:target");
        apply_move(&mut a, "source.flac", "target.flac", "b3:source");

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        assert_eq!(plan.local.len(), 1, "занятая цель не должна давать лишних строк");
        assert_eq!(plan.local[0].path, "target.flac");
        assert_eq!(plan.local[0].kind, ChangeKind::Renamed);
        assert_eq!(plan.local[0].from.as_deref(), Some("source.flac"));
        assert_eq!(plan.local[0].blob, "b3:source");
    }

    #[test]
    fn renamed_then_deleted_still_reports_the_old_path_as_deleted() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "old.flac", "b3:aa");
        apply_move(&mut a, "old.flac", "new.flac", "b3:aa");

        // Новый путь так и не попал в общую базу и был удалён. Телефон при
        // этом ещё не получил даже перенос, поэтому old.flac у него остался.
        let op = a.make_op(del("new.flac")).unwrap();
        a.apply(&op).unwrap();
        a.mark_absent("new.flac").unwrap();

        let peer_have = HashMap::from([("dev-a".to_string(), (0i64, 0i64))]);
        let plan = build_with_peer_have(&a, "dev-b", "Phone", &[], Some(&peer_have)).unwrap();

        let deleted: Vec<_> = plan
            .local
            .iter()
            .filter(|change| change.kind == ChangeKind::Deleted)
            .collect();
        assert_eq!(deleted.len(), 1, "удаление источника переноса не должно теряться");
        assert_eq!(deleted[0].path, "old.flac");
    }

    #[test]
    fn chained_local_moves_are_one_change_without_ghost_deletions() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "a.flac", "b3:aa");
        apply_move(&mut a, "a.flac", "b.flac", "b3:aa");
        apply_move(&mut a, "b.flac", "c.flac", "b3:aa");

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        assert_eq!(plan.local.len(), 1, "промежуточные имена — не отдельные изменения");
        assert_eq!(plan.local[0].path, "c.flac");
        assert_eq!(plan.local[0].kind, ChangeKind::Renamed);
        assert_eq!(plan.local[0].from.as_deref(), Some("b.flac"));
        assert!(plan
            .local
            .iter()
            .all(|change| change.kind != ChangeKind::Deleted));
    }

    #[test]
    fn move_of_an_unsynced_addition_is_still_a_move() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        added(&mut a, "Album/01.flac", "b3:aa");
        apply_move(&mut a, "Album/01.flac", "Compilations/01.flac", "b3:aa");

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        assert_eq!(plan.local.len(), 1);
        assert_eq!(plan.local[0].kind, ChangeKind::Moved);
        assert_eq!(plan.local[0].from.as_deref(), Some("Album/01.flac"));
    }

    #[test]
    fn rejected_move_of_an_unsynced_addition_becomes_an_addition() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        added(&mut a, "Album/01.flac", "b3:aa");
        apply_move(&mut a, "Album/01.flac", "Compilations/01.flac", "b3:aa");
        // «Отклонить»: файл вернулся, но источник так и не стал общей базой.
        apply_move(&mut a, "Compilations/01.flac", "Album/01.flac", "b3:aa");

        // Прямой и обратный переносы образуют цикл в журнале: обход цепочки
        // обязан его разорвать, а файл — снова стать добавлением.
        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        assert_eq!(plan.local.len(), 1);
        assert_eq!(plan.local[0].path, "Album/01.flac");
        assert_eq!(plan.local[0].kind, ChangeKind::Added);
        assert_eq!(plan.local[0].from, None);
    }

    #[test]
    fn rejected_move_of_a_synced_file_leaves_no_changes() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "Album/01.flac", "b3:aa");
        apply_move(&mut a, "Album/01.flac", "Compilations/01.flac", "b3:aa");
        apply_move(&mut a, "Compilations/01.flac", "Album/01.flac", "b3:aa");
        a.confirm_synced_paths(&["Album/01.flac".to_string()])
            .unwrap();

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        assert!(
            plan.local.is_empty(),
            "согласованный перенос после отказа не должен возвращаться в план"
        );
    }

    #[test]
    fn local_edit_after_move_is_add_plus_delete_not_a_false_move() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "old.flac", "b3:aa");
        apply_move(&mut a, "old.flac", "new.flac", "b3:aa");

        // После переноса содержимое изменилось. Сканер в такой ситуации увидел
        // бы удаление + добавление, поэтому и план не должен притворяться, что
        // старый путь исчез вместе с переносом.
        let op = a.make_op(put("new.flac", "b3:bb")).unwrap();
        a.apply(&op).unwrap();
        a.mark_present("new.flac").unwrap();

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        assert_eq!(plan.local.len(), 2);
        let added = plan
            .local
            .iter()
            .find(|change| change.path == "new.flac")
            .unwrap();
        assert_eq!(added.kind, ChangeKind::Added);
        assert_eq!(added.from, None);
        let deleted = plan
            .local
            .iter()
            .find(|change| change.path == "old.flac")
            .unwrap();
        assert_eq!(deleted.kind, ChangeKind::Deleted);
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
        assert!(plan.resolve().unresolved().is_empty());
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
    fn excluded_local_path_stays_out_of_the_plan() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "track.flac", "b3:aa");

        // Локальная правка, которую человек решил не синхронизировать.
        let op = a.make_op(put("track.flac", "b3:bb")).unwrap();
        a.apply(&op).unwrap();
        a.mark_present("track.flac").unwrap();
        a.add_exclusion("track.flac", "local", "b3:bb", 2, 1)
            .unwrap();

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();
        assert!(plan.local.is_empty(), "исключённый путь не должен попасть в план");
        assert!(plan.remote.is_empty());
        assert!(plan.conflicts.is_empty());
        assert!(plan.local_is_current(&a).unwrap());
    }

    #[test]
    fn excluded_remote_path_does_not_request_a_blob() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "track.flac", "b3:aa");
        a.add_exclusion("track.flac", "remote", "b3:bb", 2, 1)
            .unwrap();

        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(put("track.flac", "b3:bb"), 10)],
        )
        .unwrap();

        assert!(plan.local.is_empty());
        assert!(plan.remote.is_empty(), "исключённый путь не должен проситься с пира");
        assert!(plan.conflicts.is_empty());
    }

    #[test]
    fn stale_peer_op_older_than_local_state_is_ignored() {
        // ПК уже принял версию с обложкой, а на телефоне в журнале осталась
        // старая операция без обложки, которую ПК так и не получил.
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "track.flac", "b3:with-cover");

        // Пир уже применил всё, что мы ему отправили.
        let peer_have = HashMap::from([("dev-a".to_string(), (i64::MAX, i64::MAX))]);
        let plan = build_with_peer_have(
            &a,
            "dev-b",
            "Phone",
            &[stale_remote(put("track.flac", "b3:no-cover"))],
            Some(&peer_have),
        )
        .unwrap();

        assert!(plan.remote.is_empty(), "устаревшая запись не должна тянуться");
        assert!(plan.local.is_empty());
        assert!(plan.conflicts.is_empty());
    }

    #[test]
    fn stale_peer_delete_older_than_local_state_is_ignored() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "track.flac", "b3:with-cover");

        let peer_have = HashMap::from([("dev-a".to_string(), (i64::MAX, i64::MAX))]);
        let plan = build_with_peer_have(
            &a,
            "dev-b",
            "Phone",
            &[stale_remote(del("track.flac"))],
            Some(&peer_have),
        )
        .unwrap();

        assert!(plan.remote.is_empty());
        assert!(plan.local.is_empty());
        assert!(plan.conflicts.is_empty());
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
        assert_eq!(plan.resolve().action("gone.flac"), Some(Decision::Keep));
        assert!(plan.resolve().unresolved().is_empty());
    }

    #[test]
    fn remote_deletion_is_offered_not_applied() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "old.flac", "b3:aa");

        let plan = build(&a, "dev-b", "Phone", &[remote(del("old.flac"), 10)]).unwrap();
        assert_eq!(plan.remote.len(), 1);
        assert_eq!(plan.remote[0].kind, ChangeKind::Deleted);
        assert_eq!(plan.resolve().action("old.flac"), Some(Decision::Keep));
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
        assert_eq!(plan.resolve().action("fresh.flac"), Some(Decision::Accept));
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
        assert_eq!(
            plan.resolve().unresolved().to_vec(),
            vec!["song.flac".to_string()]
        );
        assert_eq!(plan.resolve().action("song.flac"), None);
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
        assert_eq!(plan.resolve().action("song.flac"), Some(Decision::Accept));
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
        assert_eq!(plan.resolve().action("new/name.flac"), Some(Decision::Accept));
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
    fn resolution_applies_baseline_and_explicit_decisions() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        let op = a.make_op(put("new.flac", "b3:one")).unwrap();
        a.apply(&op).unwrap();
        let mut plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        let baseline = plan.resolve();
        assert_eq!(baseline.action("new.flac"), Some(Decision::Accept));
        assert!(baseline.unresolved().is_empty());

        plan.decisions.entries.insert(
            "new.flac".into(),
            PathDecision {
                path: "new.flac".into(),
                action: Decision::Revert,
                blob: "b3:one".into(),
                hlc: Hlc::new(10, 0),
                dev: "dev-a".into(),
            },
        );
        assert_eq!(
            plan.resolve().action("new.flac"),
            Some(Decision::Revert)
        );
    }

    #[test]
    fn resolution_keeps_conflicts_unresolved_without_an_explicit_decision() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "song.flac", "b3:base");
        let op = a.make_op(put("song.flac", "b3:local")).unwrap();
        a.apply(&op).unwrap();
        let mut plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(put("song.flac", "b3:remote"), 10)],
        )
        .unwrap();

        let unresolved = plan.resolve();
        assert_eq!(unresolved.action("song.flac"), None);
        assert_eq!(unresolved.unresolved(), ["song.flac".to_string()]);
        assert_eq!(
            unresolved.state().get("song.flac").map(String::as_str),
            Some("unresolved")
        );

        plan.decisions.entries.insert(
            "song.flac".into(),
            PathDecision {
                path: "song.flac".into(),
                action: Decision::Keep,
                blob: "b3:local|b3:remote".into(),
                hlc: Hlc::new(10, 0),
                dev: "dev-a".into(),
            },
        );
        let resolved = plan.resolve();
        assert_eq!(resolved.action("song.flac"), Some(Decision::Keep));
        assert!(resolved.unresolved().is_empty());
        assert_eq!(
            resolved.state().get("song.flac").map(String::as_str),
            Some("keep")
        );
    }

    #[test]
    fn states_disagree_only_on_shared_paths() {
        let mut mine = std::collections::BTreeMap::new();
        mine.insert("shared.flac".to_string(), "delete_everywhere".to_string());
        mine.insert("only-mine.flac".to_string(), "keep".to_string());

        let mut theirs = std::collections::BTreeMap::new();
        theirs.insert("shared.flac".to_string(), "delete_everywhere".to_string());
        theirs.insert("only-theirs.flac".to_string(), "accept".to_string());

        // Лишние пути у любой стороны — не расхождение: у другой стороны
        // такого файла просто нет.
        assert!(!states_disagree(&mine, &theirs));

        theirs.insert("shared.flac".to_string(), "accept".to_string());
        assert!(states_disagree(&mine, &theirs));
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
            plan.resolve().action("new.flac"),
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
        assert!(plan.resolve().unresolved().is_empty());

        // Скан чистит такой призрак целиком, а не оставляет вечное удаление.
        assert_eq!(a.prune_ghost_deletes().unwrap(), 1);
        assert!(a.pending_deletes().unwrap().is_empty());
        assert!(a.path_state("draft.flac").unwrap().is_none());
    }

    #[test]
    fn missing_path_returns_when_the_peer_has_the_same_blob_newer() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "track.flac", "b3:aa");
        a.record_pending_delete("track.flac", "b3:aa", Hlc::new(10, 0))
            .unwrap();

        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(put("track.flac", "b3:aa"), 1)],
        )
        .unwrap();

        assert!(
            plan.local.is_empty(),
            "возврат пира отменяет мягкое удаление"
        );
        assert_eq!(plan.remote.len(), 1);
        assert_eq!(plan.remote[0].kind, ChangeKind::Added);
        assert_eq!(plan.remote[0].base_blob, "");
    }

    #[test]
    fn missing_path_with_a_different_peer_blob_is_a_conflict() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "track.flac", "b3:aa");
        a.record_pending_delete("track.flac", "b3:aa", Hlc::new(10, 0))
            .unwrap();

        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(put("track.flac", "b3:bb"), 1)],
        )
        .unwrap();

        assert!(plan.local.is_empty());
        assert!(plan.remote.is_empty());
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(
            plan.conflicts[0].reason,
            ConflictReason::ChangedVsDeleted
        );
    }

    #[test]
    fn tombstone_accepts_a_newer_peer_restore_as_an_addition() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "track.flac", "b3:aa");
        a.apply(&remote(del("track.flac"), 10)).unwrap();
        assert_eq!(
            a.path_state("track.flac").unwrap().unwrap().state,
            PathState::Tombstone
        );

        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[remote(put("track.flac", "b3:aa"), 20)],
        )
        .unwrap();

        assert!(plan.local.is_empty(), "пир восстановил файл новее удаления");
        assert_eq!(plan.remote.len(), 1);
        assert_eq!(plan.remote[0].kind, ChangeKind::Added);
        assert_eq!(plan.remote[0].base_blob, "");
    }

    #[test]
    fn stale_peer_restore_of_the_deleted_blob_keeps_the_deletion() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "track.flac", "b3:aa");
        a.apply(&remote(del("track.flac"), 10)).unwrap();

        // Старый put с тем же блобом не новее tombstone: это хвост журнала
        // пира, а не восстановление. Удаление всё ещё нужно ему отправить.
        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[stale_remote(put("track.flac", "b3:aa"))],
        )
        .unwrap();

        assert_eq!(plan.local.len(), 1);
        assert_eq!(plan.local[0].kind, ChangeKind::Deleted);
        assert!(plan.remote.is_empty());
        assert!(plan.conflicts.is_empty());
    }

    #[test]
    fn resurrecting_remote_asks_for_the_peer_copy() {
        let a = Replica::in_memory("dev-a").unwrap();
        a.add_exclusion("track.flac", "remote", "b3:aa", 5, 1)
            .unwrap();
        a.add_resurrection("track.flac", "remote", "b3:aa", 5, 1)
            .unwrap();

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        assert!(plan.local.is_empty());
        assert_eq!(plan.remote.len(), 1);
        assert_eq!(plan.remote[0].kind, ChangeKind::Added);
        assert_eq!(plan.remote[0].base_blob, "");
        assert!(!plan.remote[0].present);
    }

    #[test]
    fn resurrecting_local_is_an_addition_and_ignores_a_stale_peer_delete() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        synced(&mut a, "track.flac", "b3:aa");
        a.add_resurrection("track.flac", "local", "b3:aa", 5, 1)
            .unwrap();

        let plan = build(
            &a,
            "dev-b",
            "Phone",
            &[stale_remote(del("track.flac"))],
        )
        .unwrap();

        assert_eq!(plan.local.len(), 1);
        assert_eq!(plan.local[0].kind, ChangeKind::Added);
        assert_eq!(plan.local[0].base_blob, "");
        assert!(
            plan.remote.is_empty(),
            "старое удаление пира не должно победить возврат"
        );
    }

    #[test]
    fn undownloaded_remote_addition_is_not_a_local_change() {
        let mut a = Replica::in_memory("dev-a").unwrap();
        a.apply(&remote(put("new.flac", "b3:aa"), 1)).unwrap();
        let row = a.path_state("new.flac").unwrap().unwrap();
        assert_eq!(row.state, PathState::Live);
        assert!(!row.present);
        assert_eq!(row.dev, "dev-peer");

        let plan = build(&a, "dev-b", "Phone", &[]).unwrap();

        assert!(plan.local.is_empty());
        assert!(plan.remote.is_empty());
        assert!(plan.conflicts.is_empty());
    }
}
