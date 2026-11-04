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
      decided: !conflict.unresolved,
      decision: conflict.decision,
      size: Math.max(Number(conflict.local_size) || 0, Number(conflict.remote_size) || 0),
    });
  }

  const manual = Boolean(data && data.manual_additions);
  for (const change of plan.changes || []) {
    if (change.aligned) continue;
    const addition = manual && change.kind === 'added';
    const deletion = change.kind === 'deleted';
    if (!addition && !deletion) continue;
    if (paths.has(change.path)) continue;
    paths.add(change.path);
    items.push({
      type: addition ? 'addition' : 'deletion',
      path: change.path,
      change,
      decided: Boolean(change.decided),
      decision: change.decision,
      size: change.size,
    });
  }

  return items;
}

export function pendingDecisions(data = workspaceData()) {
  return decisionItems(data).filter((item) => !item.decided);
}

export function pendingCount(data = workspaceData()) {
  return pendingDecisions(data).length;
}

// Единый список изменений для вкладки «Изменения». Конфликты идут первыми и
// вытесняют одноимённую строку из обычных изменений, чтобы путь не задвоился.
export function changeEntries(data = workspaceData()) {
  const plan = planData(data);
  if (!plan) return [];
  const manual = Boolean(data && data.manual_additions);
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
      decided: !conflict.unresolved,
      pending: Boolean(conflict.unresolved),
      size: Math.max(Number(conflict.local_size) || 0, Number(conflict.remote_size) || 0),
    });
  }

  for (const change of plan.changes || []) {
    if (change.aligned || seen.has(change.path)) continue;
    seen.add(change.path);
    const addition = manual && change.kind === 'added';
    const deletion = change.kind === 'deleted';
    entries.push({
      type: addition ? 'addition' : deletion ? 'deletion' : 'change',
      path: change.path,
      conflict: null,
      change,
      side: change.side,
      folder: dirname(change.path),
      decided: Boolean(change.decided),
      pending: Boolean(!change.decided && (addition || deletion)),
      size: change.size,
    });
  }

  return entries;
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
    .sort((a, b) => {
      // Файлы из корня библиотеки — в конце, остальные папки по алфавиту.
      if (!a.folder) return b.folder ? 1 : 0;
      if (!b.folder) return -1;
      return a.folder.localeCompare(b.folder, locale(), { numeric: true, sensitivity: 'base' });
    });
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
    && pendingCount(data) === 0,
  );
}

export function headline(data = workspaceData()) {
  if (!data && state.workspace.refreshing) {
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
    return { tone: 'off', label: t('workspace.status.offline') };
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
  return { css: 'is-delete', label: t('workspace.attention.deletion') };
}

export function changeMeta(change, manual = false) {
  const parts = [];
  if (change.kind === 'moved' && change.from) {
    parts.push(t('workspace.changes.from', { path: change.from }));
  }
  if (change.size) parts.push(formatSize(change.size));
  if (change.kind === 'deleted') {
    parts.push(change.decided ? decisionLabel(change.decision) : t('workspace.changes.undecided'));
  } else if (change.decided) {
    parts.push(decisionLabel(change.decision));
  } else if (manual && change.kind === 'added') {
    parts.push(t('workspace.changes.undecided'));
  } else {
    parts.push(t('workspace.changes.auto'));
  }
  return parts.join(' · ');
}

export { ago, basename, formatSize, number, sizeParts };
