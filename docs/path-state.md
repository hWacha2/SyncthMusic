# path-state.md — состояния пути и переходы

Это ТЗ для рефакторинга. Оно описывает **одно состояние пути**, его поля,
инварианты, переходы и то, как каждый модуль читает это состояние. Код не
меняется по этому документу вслепую: каждый шаг — отдельный коммит с тестами.

## 1. Зачем

Сейчас состояние одного пути хранится в восьми местах:

| Источник | Поле | Смысл |
|---|---|---|
| `entries` | `deleted` | tombstone |
| `entries` | `local_present` | файл физически здесь |
| `entries` | `local_ignored` | legacy «не хочу здесь» |
| `entries` | `synced_blob` | последняя общая база |
| `entries` | `blob/size/mtime/added_at/hlc/dev` | текущее содержимое (LWW) |
| `pending_deletes` | `(blob, detected_at, counter)` | мягкое удаление, ждёт решения |
| `exclusions` | `(side, blob, size, mtime)` | путь вне синхронизации |
| `resurrections` | `(side, blob, size, mtime)` | возврат из исключения |
| `plan_decisions` | `(action, blob, hlc, dev)` | намерение человека (per-peer) |

Баги рождаются на пересечениях: `deleted=1` + непустой `synced_blob`;
`pending` + `resurrection`; `exclusions` без `entries`; `local_ignored` +
`exclusions`. Цель — **одно состояние на путь**, остальное выводится из него.

`DecisionSet` (per-peer решения) остаётся отдельной таблицей: это
реплицируемое намерение, а не локальное состояние. `OnlyHere`/`Unset` — это
события, которые двигают `path_state`, но сами решения живут по своим HLC.

## 2. Модель

Одна таблица, одна строка на путь **на каждой реплике**:

```sql
path_state(
  path        TEXT PRIMARY KEY,
  state       TEXT NOT NULL,   -- дискриминатор
  base        TEXT,            -- последняя общая база; NULL = базы нет
  blob        TEXT NOT NULL DEFAULT '', -- последнее известное содержимое;
                                        -- для missing/tombstone — удалённое
  size        INTEGER NOT NULL DEFAULT 0,
  mtime_ns    INTEGER NOT NULL DEFAULT 0,
  added_at    INTEGER NOT NULL DEFAULT 0,
  hlc_ms      INTEGER NOT NULL,
  hlc_counter INTEGER NOT NULL,
  dev         TEXT NOT NULL,
  present     INTEGER NOT NULL DEFAULT 0, -- локальное, не реплицируется
  pending_hlc_ms      INTEGER,            -- только для missing
  pending_hlc_counter INTEGER,            -- только для missing
  created_at  INTEGER                     -- только для excluded/resurrecting
)
```

Индексы: по `state`, по `blob`, по `(hlc_ms, hlc_counter)`.

`base`, `present`, `pending_hlc`, `created_at` — локальные поля. В `state_hash`
и в операции журнала они не входят. `blob/size/mtime/added_at/hlc/dev` — это
содержимое, которое реплицируется через `EntryPut`/`EntryDel`/`EntryMove`.

`path_state` — локальная проекция журнала `ops` плюс локальные знания
(`present`, `base`). Журнал `ops` остаётся источником для обмена с пиром; при
сбросе истории он очищается вместе с `path_state`.

## 3. Состояния

```rust
enum PathState {
    /// Файл (или его отсутствие) согласован с базой.
    Live {
        base: Option<Blob>,   // None = добавление, ещё не в базе
        blob: Blob,
        size: u64, mtime_ns: i64, added_at: u64,
        present: bool,
        hlc: Hlc, dev: String,
    },

    /// Файл пропал, решение ещё не принято.
    Missing {
        base: Option<Blob>,   // None = черновик, который никогда не был общим
        blob: Blob,           // что именно исчезло
        size: u64, mtime_ns: i64, added_at: u64,
        hlc: Hlc, dev: String, // последнее содержимое
        pending_hlc: Hlc,      // когда обнаружили пропажу
    },

    /// Удалён везде и это согласовано.
    Tombstone {
        base: Option<Blob>,   // последняя общая база (для «пир ещё на базе»)
        blob: Blob,           // что удалили; может быть пустым
        size: u64, mtime_ns: i64, added_at: u64,
        hlc: Hlc, dev: String, // операция удаления
    },

    /// Путь вне синхронизации; файл уцелел здесь.
    ExcludedLocal {
        base: Option<Blob>,
        blob: Blob, size: u64, mtime_ns: i64, added_at: u64,
        hlc: Hlc, dev: String,
        created_at: u64,
    },

    /// Путь вне синхронизации; файл уцелел на пире.
    ExcludedRemote {
        base: Option<Blob>,
        blob: Blob, size: u64, mtime_ns: i64, added_at: u64,
        hlc: Hlc, dev: String,
        created_at: u64,
    },

    /// Отмена исключения: локальная копия возвращается.
    ResurrectingLocal {
        base: Option<Blob>,   // NULL: новая жизнь, локальное добавление
        blob: Blob, size: u64, mtime_ns: i64, added_at: u64,
        hlc: Hlc, dev: String,
        created_at: u64,
    },

    /// Отмена исключения: копия должна приехать с пира.
    ResurrectingRemote {
        base: Option<Blob>,
        blob: Blob, size: u64, mtime_ns: i64, added_at: u64,
        hlc: Hlc, dev: String,
        created_at: u64,
    },
}
```

`Live` покрывает несколько наблюдаемых случаев:

| Ситуация | Как выглядит в `Live` |
|---|---|
| В синхронизации | `base=Some(b)`, `blob=b`, `present=true` |
| Локальное добавление | `base=None`, `present=true`, `dev=me` |
| Локальная правка | `base=Some(b)`, `blob!=b`, `present=true`, `dev=me` |
| Файл только на пире, ещё не скачан | `base=None` или `Some(b)`, `present=false`, `dev=peer` |
| Добавление с пира, ещё не скачано | `base=None`, `blob`, `present=false`, `dev=peer` |

Последняя строка важна: `Live { base=None, present=false, dev=peer }` — валидное
транзиентное состояние «чужое добавление принято, файл ещё не приехал».
`missing_blobs` должен его скачивать.

## 4. Инварианты

1. `Tombstone` не является живым содержимым: в проекции `entries` у него
   `blob=""`, `size=0`, `mtime=0`, `added_at=0`, `deleted=1`. В самой
   `path_state` `blob/size/mtime/added_at` хранят удалённое содержимое — они
   нужны корзине и детекту «пир ещё на базе».
2. `Missing.base` может быть `None` только для черновика, который никогда не
   был общим. Скан/`prune_ghost_deletes` такой путь забывает.
3. `Missing.blob` — содержимое, которое было у пути до исчезновения.
4. `Excluded*`, `Resurrecting*`, `Missing`, `Tombstone` взаимоисключающие.
5. `ExcludedLocal` и `ResurrectingLocal` требуют `present=1`.
6. `ExcludedRemote` и `ResurrectingRemote` требуют `present=0`.
7. `pending_hlc` есть только у `Missing`.
8. `created_at` есть только у `Excluded*`/`Resurrecting*`.
9. `base` не очищается при переходе в `Tombstone`; при оживлении
   (`EntryPut` на tombstone, vault restore, `ResurrectingLocal`) `base=NULL` —
   это новая жизнь.
10. `Tombstone.present=1` допустим только как транзиентное состояние: файл
    ещё лежит на диске и ждёт `prune_deleted_files`. После prune — `present=0`.
11. Никакой путь не может быть одновременно в `Missing`, `Tombstone`,
    `Excluded*` и `Resurrecting*`: это одна строка с одним `state`.
12. `Excluded*` — локальное решение. Оно не реплицируется и не входит в
    `state_hash` как отдельный флаг; в хеш входит только содержимое пути.

## 5. Переходы

### 5.1 События сканера (локальный диск)

| Событие | Из | В | Побочные эффекты |
|---|---|---|---|
| Файл найден, строки нет | — | `Live { base=None, present=1 }` | `EntryPut`, свежий HLC, `dev=me` |
| Файл найден, `Live { present=0 }` | `Live` | `Live { present=1 }` | если `blob==base` — просто `mark_present`; иначе локальная правка |
| Файл найден, содержимое изменилось | `Live` | `Live { blob=new, present=1 }` | `EntryPut`, свежий HLC, `dev=me`, `base` не меняется |
| Файл найден, mtime изменился, блоб тот же | `Live` | `Live` | `EntryPut` (mtime), свежий HLC |
| Файл найден, `Missing`, тот же блоб | `Missing` | `Live { base, blob, present=1 }` | pending снимается |
| Файл найден, `Missing`, другой блоб | `Missing` | `Live { base, blob=new, present=1 }` | `EntryPut`, свежий HLC, `dev=me`, pending снимается |
| Файл найден, `Tombstone` | `Tombstone` | `Live { base=None, present=1 }` | «восстановил из проводника»: `EntryPut`, план покажет Added |
| Файл найден, `ExcludedLocal` | `ExcludedLocal` | `ExcludedLocal` | обновить `blob/size/mtime`, ops не эмитить |
| Файл найден, `ExcludedRemote` | `ExcludedRemote` | `ExcludedLocal` | файл появился локально; **исключение сохраняется** до явной отмены |
| Файл найден, `ResurrectingLocal` | `ResurrectingLocal` | `ResurrectingLocal` | обновить содержимое, `EntryPut` |
| Файл найден, `ResurrectingRemote` | `ResurrectingRemote` | `Live { base, present=1 }` | маркер снимается, файл уже здесь |
| Файл пропал, `Live { base=Some }` | `Live` | `Missing` | `pending_hlc`, `present=0`; op не эмитится |
| Файл пропал, `Live { base=None, dev=me }` | `Live` | — (строка удаляется) | `forget_never_shared` |
| Файл пропал, `Live { base=None, dev=peer }` | `Live` | `Live { present=0 }` | чужое добавление ещё не скачано; это не удаление |
| Файл пропал, уже `Missing` | `Missing` | `Missing` | pending не перезаписывается |
| Файл пропал, `Tombstone` | `Tombstone` | `Tombstone` | — |
| Файл пропал, `ExcludedLocal` | `ExcludedLocal` | `ExcludedRemote` | уцелевшая копия исчезла; на undo проверить, есть ли блоб у пира |
| Файл пропал, `ExcludedRemote` | `ExcludedRemote` | `ExcludedRemote` | — |
| Файл пропал, `ResurrectingLocal` | `ResurrectingLocal` | `ExcludedRemote` | локальная копия не вернулась; на undo проверить пира |
| Файл пропал, `ResurrectingRemote` | `ResurrectingRemote` | `ResurrectingRemote` | ждём пира |

Сканер **не трогает** `Excluded*`/`Resurrecting*`, кроме явных случаев выше.
`ExcludedRemote` + файл появился не возвращает путь в синхронизацию сам:
исключение — решение человека.

### 5.2 Применение удалённых операций

Важно: **план строится по неприменённым операциям пира**, `apply_all`
вызывается только в `finalize` после решений. Поэтому таблица ниже — это
переходы `finalize`, а не то, что план видит во время preview. План видит
`path_state` + операции пира отдельно.

| Операция | Из | В | Побочные эффекты |
|---|---|---|---|
| `EntryPut`, строки нет | — | `Live { base=None, blob, present=0, hlc, dev=peer }` | чужое добавление, ждёт скачивания |
| `EntryPut`, `Live`, HLC новее | `Live` | `Live { blob=new, hlc, dev=peer }` | `base` не меняется, `present` не меняется |
| `EntryPut`, `Tombstone`, HLC новее | `Tombstone` | `Live { base=None, blob, present=0, hlc, dev=peer }` | оживление: `base=None` (новая жизнь) |
| `EntryPut`, `Missing`, тот же блоб, HLC > pending | `Missing` | `Live { base, blob, present=0, hlc, dev=peer }` | pending снимается; файл скачается |
| `EntryPut`, `Missing`, другой блоб или HLC <= pending | `Missing` | `Missing` | сохранить pending и base: план даст `ChangedVsDeleted`/stale |
| `EntryDel`, `Live`/`Missing` | `*` | `Tombstone { base, blob, hlc, dev=peer }` | pending снимается; файл удалит `prune_deleted_files` |
| `EntryDel`, `Tombstone` | `Tombstone` | `Tombstone` | идемпотентно |
| `EntryMove(from,to)` | `Live(from)` | `Tombstone(from)`, `Live(to)` | `from` получает `base`/`blob` старого пути; `to` — как `EntryPut` |
| Любая операция по `Excluded*` | `Excluded*` | без изменений | `finalize` фильтрует `peer_ops` по исключениям |
| `EntryPut` по `ResurrectingRemote` | `ResurrectingRemote` | `Live { base=Some(blob), present=0 }` | маркер снимается |
| `EntryDel` по `Resurrecting*` новее маркера | `Resurrecting*` | `Tombstone` | маркер снимается |

### 5.3 Commit: решения пользователя

| Решение | Из | В | Побочные эффекты |
|---|---|---|---|
| local `Added` + Accept | `Live { base=None }` | `Live { base=Some(blob) }` | `EntryPut`, `confirm_synced_paths` |
| local `Added` + Revert/DeleteEverywhere | `Live { base=None }` | `Tombstone` | `EntryDel`, файл в vault |
| local `Added` + OnlyHere | `Live { base=None }` | `ExcludedLocal` | exclusion, `present=1` |
| local `Updated` + Accept | `Live` | `Live { base=blob }` | `EntryPut`, confirm |
| local `Updated` + Revert | `Live` | `Live { blob=base, base=base }` | `EntryPut` с `base_blob`, confirm |
| local `Renamed/Moved` + Accept | `Live` | `Live { base=blob }` + `Tombstone(from)` | `EntryMove`, confirm target |
| local `Renamed/Moved` + Revert | `Live` | `Tombstone(to)` + `Live(from)` | обратный `EntryMove`; confirm source только если он уже был в общей базе, иначе путь снова `Added` |
| local `Deleted` + DeleteEverywhere | `Missing`/`Live` | `Tombstone` | `EntryDel`, pending снимается |
| local `Deleted` + KeepInLibrary | `Missing` | `Live { base=blob, present=1 }` | `EntryPut`, restore из vault/блоба, confirm |
| local `Deleted` + OnlyHere | `Missing` | `ExcludedRemote` | exclusion; pending снимается |
| remote `Added` + Accept | `Live { present=0 }` или — | `Live { base=Some(blob), present=1 }` | `place`, confirm |
| remote `Added` + DeleteEverywhere | — | `Tombstone` | локальный `EntryDel` (автор — я), peer применяет |
| remote `Added` + OnlyHere | — | `ExcludedRemote` | exclusion, peer op не применяется |
| remote `Updated` + Accept | `Live` | `Live { base=blob, present=1 }` | `place`, confirm |
| remote `Updated` + Revert | `Live` | `Live { base=old }` | peer создаёт revert-put, локально применяются обе операции, confirm |
| remote `Renamed/Moved` + Accept | `Live` | `Live { base=blob, present=1 }` + `Tombstone(from)` | `place`, `prune_deleted_files` |
| remote `Renamed/Moved` + Revert | `Live` | `Tombstone(to)` + `Live(from)` | peer создаёт обратный move |
| remote `Deleted` + DeleteEverywhere | `Live` | `Tombstone` | peer создаёт `EntryDel` |
| remote `Deleted` + KeepInLibrary | `Live` | `Live` (без изменений) | peer создаёт `EntryPut`; локальный файл уже есть |
| remote `Deleted` + OnlyHere | `Live` | `ExcludedLocal` | exclusion |

### 5.4 Конфликты

Конфликты не являются отдельным состоянием: это комбинация `path_state` и
неприменённых операций пира. План строит `Conflict`, commit применяет решение.

| Конфликт | Решение | Переход |
|---|---|---|
| `ChangedVsDeleted` (локально изменён, у пира удалён) | `Keep` | `Live` (локальная версия) → `Live { base=blob }`, peer получает revert-put |
| `ChangedVsDeleted` | `Accept` | `Tombstone`, локальная версия в vault/conflict-copy |
| `ChangedVsDeleted` (локально удалён, у пира изменён) | `Keep` | `Tombstone`, peer получает revert-del |
| `ChangedVsDeleted` | `Accept` | `Live { base=remote_blob, present=1 }`, `place` |
| `NoBase` (оба добавили разное) | `Keep` | `Live { base=local_blob }`, peer получает revert |
| `NoBase` | `Accept` | `Live { base=remote_blob, present=1 }`, `place` |
| `NoBase` | `KeepBoth` | `Live { base=local_blob }` + `Live { base=remote_blob }` на conflict-copy |
| `BothChanged` | `Keep` | `Live { base=local_blob }`, peer получает revert |
| `BothChanged` | `Accept` | `Live { base=remote_blob, present=1 }`, `place` |
| `BothChanged` | `KeepBoth` | обе версии: оригинал и conflict-copy |

`KeepBoth` всегда создаёт второй путь (`*.sync-conflict-*`) и подтверждает обе
версии. `Accept` на стороне, где файла нет, ставит `present=1` после `place`.

### 5.5 Отмена исключения (undo)

| Условие | Из | В | Побочные эффекты |
|---|---|---|---|
| `ExcludedLocal`, файл на диске есть | `ExcludedLocal` | `ResurrectingLocal` | свежий `EntryPut`, `base=None`, `present=1`, pending снять, `Unset` всем пирам |
| `ExcludedLocal`, файла нет, у пира блоб есть | `ExcludedLocal` | `ResurrectingRemote` | `present=0`, маркер remote, `Unset` |
| `ExcludedLocal`, файла нет, у пира блоба нет | `ExcludedLocal` | `Tombstone`/`ExcludedRemote` | сообщение «копия потеряна», не висеть в ResurrectingRemote |
| `ExcludedRemote`, у пира блоб есть | `ExcludedRemote` | `ResurrectingRemote` | `present=0`, маркер remote, `Unset` |
| `ExcludedRemote`, у пира блоба нет | `ExcludedRemote` | `Tombstone`/`ExcludedRemote` | сообщение «копия потеряна» |
| `ResurrectingLocal` + Accept | `ResurrectingLocal` | `Live { base=blob, present=1 }` | confirm, маркер снять |
| `ResurrectingLocal` + Revert | `ResurrectingLocal` | `Tombstone` | `EntryDel`, маркер снять |
| `ResurrectingRemote` + Accept | `ResurrectingRemote` | `Live { base=blob, present=1 }` | `place`, confirm, маркер снять |
| `ResurrectingRemote` + Keep/Reject | `ResurrectingRemote` | `ExcludedRemote` | маркер снять, путь снова исключён |

`Unset` пишется **всем известным пирам**, а не только текущему: exclusions
глобальны, `DecisionSet` — per-peer. Если пиров несколько, иначе другой пир
снова пришлёт `OnlyHere`.

### 5.6 Vault

| Операция | Эффект на `path_state` |
|---|---|
| `preserve_file` | не меняет состояние (только читает `entry_raw`) |
| `vault::restore` | `Tombstone` → `Live { base=None, present=1 }`, `EntryPut` с исходными `added_at/mtime_ns`, vault-row удаляется |
| `gc` / `clear` | не меняют `path_state` |

## 6. Как `plan.rs` читает состояние

План читает `path_state` и **не применяет** операции пира. `remote_ops` —
отдельный вход. `apply_all` вызывается в `finalize` после решений.

| Состояние | `plan.local` | `plan.remote` | `conflicts` |
|---|---|---|---|
| `Live { base=None, present=1 }` | `Added` (`base=""`) | — | — |
| `Live { base=Some(b), blob!=b, present=1 }` | `Updated` (`base=b`) или `Renamed/Moved` по журналу переносов (цепочка начинается с базы или не замыкается) | — | — |
| `Live { base=Some(b), blob==b, present=1 }` | — (aligned, если пир с тем же блобом) | — | — |
| `Live { base=Some(b), present=0 }` | — | если пир прислал новее/другое — `Updated`/`Added` | — |
| `Live { base=None, present=0, dev=peer }` | — | `Added` (файл ждёт скачивания) | — |
| `Missing` | `Deleted` (`base`) — если пир не восстановил тот же блоб новее | если пир восстановил тот же блоб новее — `Added` (`base=""`) | если пир прислал другой блоб — `ChangedVsDeleted` |
| `Tombstone` | `Deleted` — если пир ещё на базе (`base`) и не восстановил | если пир восстановил (`hlc > tombstone.hlc`) — `Added` (`base=""`) | — |
| `Excluded*` | — (скрыт) | — (скрыт) | — |
| `ResurrectingLocal` | `Added` (`base=""`) | подавить stale remote `Deleted` | — |
| `ResurrectingRemote` | — | синтетический `Added` (`base=""`, `present=0`) | — |

Правила подавления — свойства состояний, а не набор `if`:

- `ResurrectingLocal` + remote `Deleted` старше `hlc` → игнор.
- `Missing` + remote `EntryPut` того же блоба новее `pending_hlc` → возврат.
- `Tombstone` + remote `EntryPut` новее `tombstone.hlc` → `Added`; если блоб
  равен `deleted_blob` и HLC не новее — stale, игнор.
- Замкнутая цепочка `entry_move` без базы — возврат отклонённого переноса:
  файл вернулся на прежнее место и остаётся `Added`, а не `Moved`.

`PlanRow` (совместимая проекция) для `Tombstone` отдаёт `deleted=1`, но
сохраняет `blob/size/mtime/added_at` удалённого содержимого: плану нужны
`size/mtime` для решения «пир ещё на базе».

## 7. Сброс

Миграции нет. Полный сброс истории синхронизации на **обоих** устройствах:

1. Удалить `ops`, `path_state`, `plan_decisions`, `sync_plans`, `blobs`,
   старые `entries`, `pending_deletes`, `exclusions`, `resurrections`.
2. Создать пустой `path_state`.
3. Сохранить идентичность устройства, root, token, `vault`, `peers`, `meta`,
   `content_cache`.
4. Первый скан создаёт `Live { base=None }` для каждого локального файла.
   Первый обмен покажет добавления; одинаковые блобы схлопнутся в aligned,
   разные — в `NoBase`-конфликт.

Exclusions не переносятся: у пользователя их нет, а у приложения пока нет
других пользователей. Это сознательное решение, а не миграция.

## 8. Определения

### `state_hash`

Хеш только реплицируемого содержимого. Для каждого пути:

- `Live`, `Missing`, `Excluded*`, `Resurrecting*` — `path`, `blob`, `size`,
  `mtime_ns`, `added_at`, `deleted=0`;
- `Tombstone` — `path`, `deleted=1`, остальные поля пустые (как старый
  `normalize_row`).

`base`, `present`, `pending_hlc`, `created_at`, `dev`, `hlc` в хеш не входят.
Тест: ПК с `Missing` и телефон с `Live` того же блоба дают одинаковый хеш.

### `local_is_current`

Снимок `path_state` в момент построения плана. Для каждого локального
изменения:

- `Added`/`Updated` — состояние всё ещё `Live`/`ResurrectingLocal` с тем же
  `blob` и `present`;
- `Deleted` — состояние `Missing` (или `Tombstone`, если удаление уже
  применилось) с тем же `blob`/`base`;
- `Renamed/Moved` — обе строки (`from`/`to`) всё ещё соответствуют переносу.

Проверки `ignored/pending/exclusions` заменяются проверкой `state`.

### `states_disagree`

Сравнивает итоговые действия плана по общим путям. Путь, которого нет в одной
из карт (исключённый, `ResurrectingRemote` до применения op), не считается
расхождением. Расхождение — только разные действия по одному пути.

### `missing_blobs`

Только `Live { present=0 }` и `ResurrectingRemote`, у которых есть блоб.
Не `Missing`, не `Excluded*`, не `Tombstone`. `ExcludedRemote` не докачивается.

## 9. UI read model

UI получает одно поле `state` плюс вычисленные флаги:

```json
{
  "path": "Album/01.flac",
  "state": "live|missing|tombstone|excluded_local|excluded_remote|resurrecting_local|resurrecting_remote",
  "base": "b3:...",
  "blob": "b3:...",
  "size": 123,
  "mtime_ns": 0,
  "added_at": 0,
  "present": true,
  "pending": false,
  "excluded": false,
  "resurrecting": false,
  "created_at": 0
}
```

Старые поля `deleted/present/ignored/pending/excluded/resurrecting` остаются
только как совместимая проекция на время рефакторинга.

## 10. Тестовая матрица

Для каждого перехода — отдельный тест. Минимум:

1. `scan`: файл появился → `Live { base=None }`; изменился → `Live
   { base!=blob }`; пропал → `Missing`.
2. `scan`: `Missing` + файл вернулся с тем же блобом → `Live { base }`;
   с другим → `Live { base, blob!=base }`.
3. `scan`: `Tombstone` + файл появился → `Live { base=None }`.
4. `apply`: `EntryPut` на `Tombstone` → `Live { base=None }`; `EntryDel` на
   `Live` → `Tombstone`; `EntryPut` на `Missing` с тем же/другим блобом.
5. `commit`: `Added Accept` → `Live { base=blob }`; `Added Revert` →
   `Tombstone`.
6. `commit`: `Updated Accept/Revert`; `Renamed/Moved Accept/Revert`.
7. `commit`: `Deleted DeleteEverywhere/KeepInLibrary/OnlyHere`.
8. `commit`: конфликты `Keep/Accept/KeepBoth` для `ChangedVsDeleted`,
   `NoBase`, `BothChanged`.
9. `undo`: `ExcludedLocal` с файлом → `ResurrectingLocal`; без файла →
   `ResurrectingRemote`; `ExcludedRemote` → `ResurrectingRemote`.
10. `undo`: `ResurrectingLocal` + commit → `Live`; `ResurrectingRemote` +
    commit → `Live`; Reject → `ExcludedRemote`.
11. `vault::restore` → `Live { base=None }`.
12. Инварианты: после каждого commit нет `Tombstone` с живым payload в
    проекции; нет пути одновременно в `Missing` и `Excluded*`; `present`
    соответствует состоянию.
13. `state_hash`: `Missing` на одной стороне и `Live` на другой дают
    одинаковый хеш; `Tombstone` не зависит от сохранённого `blob`.
14. `missing_blobs`: не запрашивает `Missing`, `Excluded*`, `Tombstone`.
15. `reset`: после полного сброса на обоих устройствах первый скан создаёт
    `Live { base=None }`, старые ops/decisions не воскрешают пути.

## 11. Порядок работ

1. `path_state` + `store` (без миграции, БД обнуляется).
2. `plan.rs` — чтение состояний вместо комбинаций флагов.
3. `scan.rs` — переходы сканера.
4. `commit.rs` — решения и конфликты.
5. `resolve.rs`/`vault.rs` — undo/restore.
6. UI — одно поле `state`.

Каждый шаг — отдельный коммит с тестами из раздела 10.
