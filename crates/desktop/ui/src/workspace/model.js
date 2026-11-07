import { lang, t } from '../i18n/index.js';
import { ago, basename, dirname, formatSize, number, sizeParts } from '../format.js';
import { state } from '../store.js';

// Производные значения рабочей области. Здесь нет побочных эффектов: те же
// правила, что в старом app.js, но без ручной сборки HTML.

export function workspaceData() {
  return state.workspace.data;
}

export function planData(data = workspaceData()) {
  return data && data.plan ? data.plan : null;
}

export function decisionItems(data = workspaceData()) {
  const plan = planData(data);
  if (!plan) return [];
  const items = [];
  const paths = new Set();

  for (const conflict of plan.conflicts || []) {
    paths.add(conflict.path);
    items.push({
      type: 'conflict',
      path: conflict.path,
      conflict,
      decided: Boolean(conflict.decided),
      pending: Boolean(conflict.pending),
      decision: conflict.decision,
      size: Math.max(Number(conflict.local_size) || 0, Number(conflict.remote_size) || 0),
    });
  }

  for (const change of plan.changes || []) {
    if (change.aligned || change.ignored || paths.has(change.path)) continue;
    // В карточку внимания попадают только те пути, которым действительно
    // нужно решение, и уже решённые удаления: их решение можно сбросить.
    const relevant = change.pending || change.decided || change.kind === 'deleted';
    if (!relevant) continue;
    paths.add(change.path);
    items.push({
      type: change.kind === 'added'
        ? 'addition'
        : change.kind === 'deleted' ? 'deletion' : 'change',
      path: change.path,
      change,
      decided: Boolean(change.decided),
      pending: Boolean(change.pending),
      decision: change.decision,
      size: change.size,
    });
  }

  return items;
}

export function pendingDecisions(data = workspaceData()) {
  return decisionItems(data).filter((item) => item.pending);
}

export function pendingCount(data = workspaceData()) {
  const plan = planData(data);
  if (plan && Array.isArray(plan.pending)) return plan.pending.length;
  return pendingDecisions(data).length;
}

/// Число путей вне синхронизации. Флаг `excluded` вычислен на бэкенде,
/// поэтому старые `exclusions` для счётчиков больше не нужны.
export function excludedCount(data = workspaceData()) {
  return (data?.states || []).filter((row) => row.excluded).length;
}

/// Суммарная корзина: файлы ПК плюс уже прочитанная корзина подключённого
/// телефона. Пока список телефона не загружен, показываем хотя бы корзину ПК.
export function vaultTotals(data = workspaceData()) {
  const pcCount = Number(data?.pc?.vault_files) || 0;
  const pcSize = Number(data?.pc?.vault_size) || 0;
  const phoneRows = workspaceConnected(data) ? state.workspace.phoneVault || [] : [];
  const phoneSize = phoneRows.reduce((sum, row) => sum + (Number(row.size) || 0), 0);
  return { count: pcCount + phoneRows.length, size: pcSize + phoneSize };
}

// Единый список изменений для вкладки «Изменения». Конфликты идут первыми и
// вытесняют одноимённую строку из обычных изменений, чтобы путь не задвоился.
export function changeEntries(data = workspaceData()) {
  const plan = planData(data);
  if (!plan) return [];
  const entries = [];
  const seen = new Set();

  for (const conflict of plan.conflicts || []) {
    seen.add(conflict.path);
    entries.push({
      type: 'conflict',
      path: conflict.path,
      conflict,
      change: null,
      side: null,
      folder: dirname(conflict.path),
      decided: Boolean(conflict.decided),
      pending: Boolean(conflict.pending),
      decision: conflict.decision,
      size: Math.max(Number(conflict.local_size) || 0, Number(conflict.remote_size) || 0),
    });
  }

  for (const change of plan.changes || []) {
    if (change.aligned || change.ignored || seen.has(change.path)) continue;
    seen.add(change.path);
    entries.push({
      // Тип строки — это вид факта, а не режим: действия в ручном режиме
      // зависят от вида, даже если путь сейчас принят автоматически.
      type: change.kind === 'added'
        ? 'addition'
        : change.kind === 'deleted' ? 'deletion' : 'change',
      path: change.path,
      conflict: null,
      change,
      side: change.side,
      folder: dirname(change.path),
      decided: Boolean(change.decided),
      pending: Boolean(change.pending),
      decision: change.decision,
      size: change.size,
    });
  }

  return entries;
}

/// Строки, которые сейчас приняты автоматически и ещё допускают ручной выбор.
/// Раскрытие такой строки — временное состояние окна, а не решение.
export function autoEntries(data = workspaceData()) {
  return changeEntries(data).filter(
    (entry) => !entry.pending && !entry.decided && entry.type !== 'conflict',
  );
}

export function manualOpenSet(data = workspaceData()) {
  const available = new Set(autoEntries(data).map((entry) => entry.path));
  return new Set(state.workspace.manualOpen.filter((path) => available.has(path)));
}

export function manualOpenCount(data = workspaceData()) {
  return manualOpenSet(data).size;
}

function locale() {
  return lang.value === 'ru' ? 'ru' : 'en';
}

function comparePaths(a, b) {
  return String(a.path).localeCompare(String(b.path), locale(), {
    numeric: true,
    sensitivity: 'base',
  });
}

export function sortEntries(entries) {
  return [...entries].sort(comparePaths);
}

/// Папки по алфавиту; корень библиотеки — в конце. Пустая строка означает
/// корень, поэтому её нельзя сравнивать как обычное имя.
function compareFolders(a, b) {
  if (!a) return b ? 1 : 0;
  if (!b) return -1;
  return a.localeCompare(b, locale(), { numeric: true, sensitivity: 'base' });
}

/// Плоская сортировка «по папкам»: файлы одной папки идут рядом, внутри —
/// по алфавиту. Для «Хранилища» и «Исключений» этого достаточно: полный путь
/// уже виден в строке.
export function sortByFolder(items) {
  return [...items].sort((a, b) => {
    const af = dirname(a.path);
    const bf = dirname(b.path);
    if (af !== bf) return compareFolders(af, bf);
    return comparePaths(a, b);
  });
}

/// Группы по папкам для плоских списков «Хранилища» и «Исключений»: у их
/// строк нет готового поля `folder`, поэтому путь раскладываем здесь.
export function groupPathsByFolder(items) {
  const groups = new Map();
  for (const item of items) {
    const folder = dirname(item.path);
    if (!groups.has(folder)) groups.set(folder, []);
    groups.get(folder).push(item);
  }

  return [...groups.entries()]
    .map(([folder, list]) => ({
      folder,
      items: sortEntries(list),
      size: list.reduce((sum, item) => sum + (Number(item.size) || 0), 0),
    }))
    .sort((a, b) => compareFolders(a.folder, b.folder));
}

/// Свежие записи сверху. Поле времени у разных списков своё: `created_at`
/// у исключений, `deleted_at` у хранилища.
export function sortByTime(items, field) {
  return [...items].sort((a, b) => (Number(b[field]) || 0) - (Number(a[field]) || 0));
}

export function groupByFolder(entries) {
  const groups = new Map();
  for (const entry of entries) {
    const folder = entry.folder || '';
    if (!groups.has(folder)) groups.set(folder, []);
    groups.get(folder).push(entry);
  }

  return [...groups.entries()]
    .map(([folder, items]) => {
      const pendingItems = items.filter((item) => item.pending);
      return {
        folder,
        items: sortEntries(items),
        pendingItems,
        pending: pendingItems.length,
        kinds: [...new Set(items.map((item) => item.change?.kind || 'conflict'))],
      };
    })
    .sort((a, b) => compareFolders(a.folder, b.folder));
}

const TYPE_ORDER = ['deletion', 'addition', 'change'];

export function groupByType(entries) {
  return TYPE_ORDER
    .map((type) => ({
      type,
      items: sortEntries(entries.filter((entry) => entry.type === type)),
    }))
    .filter((group) => group.items.length);
}

export function workspaceConnected(data) {
  return Boolean(data && data.phone && data.phone.connected);
}

export function sessionReady(data) {
  return Boolean(data && data.session && data.session.open && !data.session.finished);
}

export function canCommit(data = workspaceData()) {
  const plan = planData(data);
  const paths = Number(plan?.paths) || 0;
  const aligned = Number(plan?.aligned) || 0;
  return Boolean(
    data
    && sessionReady(data)
    && !data.session.read_only
    && !data.session.diverged
    && !data.session_error
    && !state.workspace.refreshing
    && !state.workspace.action
    && plan
    && (paths > 0 || aligned > 0)
    && pendingCount(data) === 0
    // Раскрытая строка ещё не значит решение. Пока человек не выбрал,
    // авто-правило не должно применяться незаметно.
    && manualOpenCount(data) === 0,
  );
}

export function headline(data = workspaceData()) {
  if (!data && (state.workspace.refreshing || state.workspace.stale)) {
    return { title: t('workspace.status.checking'), text: t('workspace.sub.checking') };
  }
  if (!data) {
    if (state.workspace.error) {
      return { title: t('workspace.status.error'), text: t('workspace.sub.error') };
    }
    return { title: t('workspace.status.unknown'), text: t('workspace.sub.unknown') };
  }
  const plan = planData(data);
  const pending = pendingCount(data);
  const paths = Number(plan?.paths) || 0;
  const aligned = Number(plan?.aligned) || 0;
  if (!data.phone?.linked) {
    return { title: t('workspace.status.local'), text: t('workspace.sub.local') };
  }
  if (data.session && data.session.diverged) {
    return { title: t('workspace.status.diverged'), text: t('workspace.sub.diverged') };
  }
  if (data.session_error || (data.phone && !data.phone.connected)) {
    return { title: t('workspace.status.offline'), text: t('workspace.sub.offline') };
  }
  if (pending > 0) {
    return { title: t('workspace.status.attention'), text: t('workspace.sub.attention', { count: pending }) };
  }
  if (paths > 0) {
    return {
      title: t('workspace.status.ready'),
      text: aligned > 0
        ? t('workspace.sub.ready_aligned', { count: paths, aligned })
        : t('workspace.sub.ready', { count: paths }),
    };
  }
  if (aligned > 0) {
    return { title: t('workspace.status.ready'), text: t('workspace.sub.aligned', { count: aligned }) };
  }
  if (sessionReady(data)) {
    return { title: t('workspace.status.synced'), text: t('workspace.sub.synced') };
  }
  return { title: t('workspace.status.unknown'), text: t('workspace.sub.unknown') };
}

export function connectionState(data = workspaceData()) {
  if (!data || !data.phone || !data.phone.linked) {
    return { tone: 'off', label: t('workspace.settings.not_linked') };
  }
  if (!data.phone.connected) {
    return { tone: 'off', label: t('workspace.status.offline') };
  }
  if (sessionReady(data)) {
    return { tone: 'on', label: t('workspace.status.connected') };
  }
  return { tone: 'warn', label: t('workspace.status.preparing') };
}

export function checkedText(data = workspaceData()) {
  if (!data || !data.checked_at) return t('workspace.checked_never');
  return ago(data.checked_at);
}

export function decisionLabel(decision) {
  return decision ? t(`decision.${decision}`) : t('workspace.changes.undecided');
}

export function kindLabel(kind) {
  return t(`kind.${kind}`);
}

export function decisionMeta(item) {
  const parts = [];
  if (item.type === 'conflict') {
    parts.push(kindLabel('conflict'));
    const local = Number(item.conflict?.local_size) || 0;
    const remote = Number(item.conflict?.remote_size) || 0;
    if (local && remote && local !== remote) {
      parts.push(`${formatSize(local)} / ${formatSize(remote)}`);
    } else if (local || remote) {
      parts.push(formatSize(local || remote));
    }
  } else if (item.type === 'addition') {
    parts.push(item.change?.side === 'remote'
      ? t('workspace.attention.added_on_phone')
      : t('workspace.attention.added_on_pc'));
    if (item.size) parts.push(formatSize(item.size));
  } else if (item.type === 'change') {
    parts.push(item.change?.side === 'remote'
      ? t('workspace.attention.changed_on_phone')
      : t('workspace.attention.changed_on_pc'));
    if (item.size) parts.push(formatSize(item.size));
  } else {
    parts.push(item.change?.side === 'remote'
      ? t('workspace.attention.deleted_on_phone')
      : t('workspace.attention.deleted_on_pc'));
    if (item.size) parts.push(formatSize(item.size));
  }
  return parts.join(' · ');
}

export function attentionKind(item) {
  if (item.type === 'conflict') return { css: 'is-conflict', label: t('workspace.attention.conflict') };
  if (item.type === 'addition') return { css: 'is-add', label: t('workspace.attention.addition') };
  if (item.type === 'change') return { css: 'is-change', label: kindLabel(item.change?.kind) };
  return { css: 'is-delete', label: t('workspace.attention.deletion') };
}

export function changeMeta(change, showFolder = false, pending = false) {
  const parts = [];
  if (showFolder) {
    parts.push(dirname(change.path) || t('workspace.changes.root'));
  }
  if (change.kind === 'renamed' && change.from) {
    parts.push(t('workspace.changes.renamed_meta', { name: basename(change.from) }));
  } else if (change.kind === 'moved' && change.from) {
    if (basename(change.from) === basename(change.path)) {
      const folder = dirname(change.from) || t('workspace.changes.root');
      parts.push(t('workspace.changes.moved_meta', { folder }));
    } else {
      parts.push(t('workspace.changes.moved_from', { path: change.from }));
    }
  }
  if (change.size) parts.push(formatSize(change.size));
  if (change.kind === 'deleted') {
    parts.push(change.decided ? decisionLabel(change.decision) : t('workspace.changes.undecided'));
  } else if (change.decided) {
    parts.push(decisionLabel(change.decision));
  } else if (pending) {
    parts.push(t('workspace.changes.undecided'));
  } else {
    parts.push(t('workspace.changes.auto'));
  }
  return parts.join(' · ');
}

export { ago, basename, formatSize, number, sizeParts };
