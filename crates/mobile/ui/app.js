'use strict';

// SynctMusic для Android. Ядро живёт в приложении, поэтому все действия —
// это прямые команды Tauri: без службы, без HTTP и без портов, кроме синка.

const invoke = (cmd, args) => {
  if (!window.__TAURI__ || !window.__TAURI__.core) {
    return Promise.reject(new Error('app core unavailable'));
  }
  return window.__TAURI__.core.invoke(cmd, args);
};

const { t, tp, plural, num, size, ago } = I18N;

const state = {
  info: null,
  status: null,
  tab: 'overview',
  busy: false,
  error: null,
  code: null,
  peers: [],
  plan: null,
  planError: null,
  planLoading: false,
  planFilter: 'all',
  // Выделение строго одной группы «источник + тип»: { group, paths:Set }.
  planSelection: null,
  polling: false,
};

const $ = (sel) => document.querySelector(sel);

const esc = (value) =>
  String(value == null ? '' : value).replace(/[&<>"']/g, (ch) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  }[ch]));

const icon = (name, cls = '') => `<svg class="${cls}" aria-hidden="true"><use href="#i-${name}"></use></svg>`;

const errText = (error) => {
  if (!error) return t('common.unknownError');
  if (typeof error === 'string') return error;
  return error.message || String(error);
};

let toastTimer = null;
function toast(message, bad = false) {
  const host = $('#toast');
  host.textContent = message;
  host.classList.toggle('is-bad', bad);
  host.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { host.hidden = true; }, 4200);
}

const status = () => state.status || {
  ready: false, files: 0, local: 0, size: 0, ops: 0,
  pending: [], conflicts: [], vault: [], known_peers: [], device: '', name: '',
  connection: { connected: false },
};

// Понятное имя сопряжённого устройства; id остаётся запасным вариантом.
const peerName = (peer) => (peer && (peer.name || peer.dev)) || t('device.unknown');

// ------------------------------------------------------------------ загрузка

async function loadStatus() {
  state.status = await invoke('status');
}

async function boot() {
  I18N.apply();
  try {
    state.info = await invoke('app_info');
    state.status = state.info.info && state.info.info.root != null ? await invoke('status') : null;
    if (!state.status && state.info.info && state.info.info.error) {
      state.error = state.info.info.error;
    }
    if (state.info.info && (state.info.info.name || state.info.info.device)) {
      $('#app-dev').textContent = state.info.info.name || state.info.info.device;
    }
    if (state.status) await loadPlan();
  } catch (error) {
    state.error = errText(error);
  }
  render();
}

async function refresh(silent = false) {
  if (!state.status) return;
  try {
    await loadStatus();
    render();
  } catch (error) {
    if (!silent) toast(errText(error), true);
  }
}

async function loadPlan() {
  if (!state.status) {
    state.plan = null;
    state.planSelection = null;
    return;
  }
  if (state.planLoading) return;
  state.planLoading = true;
  try {
    state.plan = await invoke('sync_plan');
    state.planError = null;
    reconcilePlanSelection();
  } catch (error) {
    state.plan = { connected: false, local_only: true, stale: false, plan: null, error: errText(error) };
    state.planSelection = null;
    state.planError = errText(error);
  } finally {
    state.planLoading = false;
  }
}

function planSummary() {
  const data = state.plan;
  const plan = data && data.plan ? data.plan : null;
  return {
    connected: !!(data && data.connected),
    localOnly: !!(data && data.local_only),
    stale: !!(data && data.stale),
    plan,
    paths: plan && data.paths != null ? data.paths : 0,
    unresolved: data && data.unresolved != null ? data.unresolved : 0,
    error: (data && data.error) || state.planError,
  };
}

// ------------------------------------------------------------------ отрисовка

function render() {
  const snap = status();
  const device = snap.name || snap.device || (state.info && state.info.info && (state.info.info.name || state.info.info.device)) || '';
  if (device) $('#app-dev').textContent = device;

  const led = $('#app-led');
  const stateText = $('#app-state');
  const progress = $('#progress');
  if (progress) progress.hidden = !state.busy;
  const plan = planSummary();
  const planCount = plan.paths || snap.pending.length + snap.conflicts.length;
  const problems = planCount;
  led.className = 'led ' + (state.busy ? 'led--warn' : problems ? 'led--warn' : state.status ? 'led--ok' : '');
  stateText.textContent = state.busy
    ? t('appbar.busy')
    : !state.status
      ? (state.error ? t('appbar.error') : t('appbar.notConfigured'))
      : problems
        ? t('appbar.attention')
        : t('appbar.synced');

  const badge = $('#badge-plan');
  if (badge) {
    badge.textContent = String(planCount);
    badge.hidden = !planCount;
  }

  for (const tab of document.querySelectorAll('.tab')) {
    tab.classList.toggle('is-on', tab.dataset.tab === state.tab);
    const label = tab.querySelector('span[data-i18n]');
    if (label) label.textContent = t(label.dataset.i18n);
  }

  const view = $('#view');
  if (!state.status) {
    view.innerHTML = state.error ? errorScreen(state.error) : setupScreen();
    return;
  }
  if (state.tab === 'plan') {
    view.innerHTML = planView(snap);
    view.querySelectorAll('.plan-check[data-indeterminate]').forEach((box) => {
      box.indeterminate = true;
    });
  } else if (state.tab === 'vault') view.innerHTML = vaultView(snap);
  else if (state.tab === 'more') view.innerHTML = moreView(snap);
  else view.innerHTML = overviewView(snap);
}

function errorScreen(message) {
  return `<section class="panel">
    <div class="empty">
      ${icon('alert', 'empty-ic')}
      <b>${esc(t('error.title'))}</b>
      <p>${esc(message)}</p>
    </div>
    <button class="btn btn--block btn--soft" data-act="refresh">${esc(t('error.retry'))}</button>
  </section>`;
}

function setupScreen() {
  const info = state.info || {};
  const candidates = info.candidates || [];
  const picks = candidates.slice(0, 8).map((c) =>
    `<button class="pick" data-pick="${esc(c.path)}">${esc(c.name)}</button>`).join('');
  return `<section class="panel">
    <h1 class="hero-title">${esc(t('setup.title'))}</h1>
    <p class="hero-hint">${esc(t('setup.hint'))}</p>
    <div class="field">
      <label for="root-input">${esc(t('setup.label'))}</label>
      <input class="input" id="root-input" value="${esc(info.default_root || '')}" spellcheck="false" autocapitalize="off">
    </div>
    ${picks ? `<div class="picks">${picks}</div>` : ''}
    <div class="actions">
      <button class="btn btn--primary" data-act="init">${icon('folder')}${esc(t('setup.start'))}</button>
    </div>
    <p class="hint">${esc(t('setup.permission'))}</p>
  </section>`;
}

function overviewView(snap) {
  const plan = planSummary();
  const planPaths = plan.paths || snap.pending.length + snap.conflicts.length;
  const planUnresolved = plan.unresolved || snap.conflicts.length;
  const peers = snap.known_peers.length;
  const connection = snap.connection || { connected: false };
  const connected = !!connection.connected;
  const title = t('overview.title.ok');
  const hint = planPaths
    ? tp('overview.hint.plan', planPaths)
    : t('overview.hint.ok');
  const root = snap.root || '';

  const alerts = [];
  if (planPaths || planUnresolved) {
    alerts.push({
      icon: planUnresolved ? 'split' : 'clock',
      tone: planUnresolved ? 'bad' : '',
      title: tp('alert.plan.title', planPaths || planUnresolved),
      text: planUnresolved ? t('alert.plan.conflicts') : t('alert.plan.text'),
      action: `<button class="btn btn--sm" data-tab-jump="plan">${esc(t('alert.plan.action'))}</button>`,
    });
  }

  return `
  <section class="panel">
    <div class="hero">
      <div class="hero-text">
        <div class="kicker">${esc(t('nav.overview'))}${root ? ` <span class="kicker-path">${esc(root)}</span>` : ''}</div>
        <h1 class="hero-title">${esc(title)}</h1>
        <p class="hero-hint">${esc(hint)}</p>
      </div>
      <div class="dial" style="--p:1;--tone:var(--lime)">
        <div class="dial-inner">
          <span class="dial-num">OK</span>
          <span class="dial-sub">${num(snap.files)}</span>
        </div>
      </div>
    </div>
    <div class="actions">
      <button class="btn btn--primary" data-act="sync" ${state.busy || !connected ? 'disabled' : ''}>${icon('sync')}${esc(t('act.sync'))}</button>
      <button class="btn btn--soft" data-act="scan" ${state.busy ? 'disabled' : ''}>${icon('rescan')}${esc(t('act.scan'))}</button>
    </div>
    <div class="row-meta">
      ${connected
        ? `${esc(t('conn.connectedTo', { name: connection.name || connection.dev }))} · ${esc(connection.addr || '')}`
        : esc(t('conn.none'))}
    </div>
    ${connected ? `<div class="actions"><button class="btn btn--block btn--ghost" data-act="disconnect">${esc(t('act.disconnect'))}</button></div>` : ''}
  </section>

  <div class="stats">
    <div class="stat">
      <div class="stat-k">${esc(t('stat.files'))}</div>
      <div class="stat-v">${num(snap.files)}</div>
      <div class="stat-note">${esc(t('stat.filesNote', { n: num(snap.local) }))}</div>
    </div>
    <div class="stat">
      <div class="stat-k">${esc(t('stat.size'))}</div>
      <div class="stat-v">${esc(size(snap.size).text)}</div>
    </div>
    <div class="stat">
      <div class="stat-k">${esc(t('stat.ops'))}</div>
      <div class="stat-v">${num(snap.ops)}</div>
      ${snap.last_scan ? `<div class="stat-note">${esc(t('stat.opsNote', { ago: ago(snap.last_scan) }))}</div>` : ''}
    </div>
    <div class="stat">
      <div class="stat-k">${esc(t('stat.vault'))}</div>
      <div class="stat-v">${num(snap.vault.length)}</div>
    </div>
  </div>

  <section class="panel panel--flat">
    <div class="panel-head">
      <div class="panel-title">${esc(t('panel.attention'))}</div>
      <span class="tag ${alerts.length ? 'tag--warn' : 'tag--good'}">${esc(alerts.length ? tp('attention.count', alerts.length) : t('attention.clean'))}</span>
    </div>
    ${alerts.length
      ? `<div class="alert-list">${alerts.map((alert) => `
          <div class="alert">
            <div class="alert-ico ${alert.tone}">${icon(alert.icon)}</div>
            <div class="alert-body"><b>${esc(alert.title)}</b><p>${esc(alert.text)}</p></div>
            ${alert.action}
          </div>`).join('')}</div>`
      : `<div class="calm">${icon('check')}<span>${esc(t('attention.empty'))}</span></div>`}
  </section>

  <section class="panel panel--flat">
    <div class="panel-head">
      <div class="panel-title">${esc(t('panel.devices'))}</div>
      <span class="tag">${esc(peers ? t('devices.tag', { n: num(peers) }) : t('devices.tagEmpty'))}</span>
    </div>
    ${peers
      ? `<div class="rows">${snap.known_peers.map((p) => `
          <div class="row">
            <div class="row-top">
              ${icon('pc', '')}
              <span class="row-path">${esc(peerName(p))}</span>
            </div>
            <div class="row-meta">${esc(p.addr)}</div>
          </div>`).join('')}</div>`
      : `<div class="empty"><b>${esc(t('devices.emptyTitle'))}</b><p>${esc(t('devices.emptyText'))}</p></div>`}
    <div class="actions"><button class="btn btn--block btn--ghost" data-tab-jump="more">${esc(t('devices.pair'))}</button></div>
  </section>

  <section class="panel panel--flat">
    <div class="panel-head"><div class="panel-title">${esc(t('panel.folder'))}</div></div>
    <div class="row-meta">${esc(root || t('folder.notSelected'))}</div>
    <div class="actions"><button class="btn btn--block btn--ghost" data-act="change-root">${esc(t('folder.change'))}</button></div>
  </section>`;
}

const PLAN_KINDS = ['added', 'updated', 'moved', 'deleted'];
const PLAN_FILTERS = ['all', 'added', 'updated', 'moved', 'deleted', 'conflicts'];

function planSourceName(plan, side) {
  if (side === 'local') {
    return (state.status && state.status.name)
      || (state.info && state.info.info && state.info.info.name)
      || t('plan.thisDevice');
  }
  return (plan && plan.peer_name) || t('plan.otherDevice');
}

function planGroups(filter = state.planFilter) {
  const { plan } = planSummary();
  if (!plan) return [];
  const groups = [];
  if ((filter === 'all' || filter === 'conflicts') && (plan.conflicts || []).length) {
    groups.push({
      key: 'conflict',
      side: 'conflict',
      kind: 'conflict',
      title: t('plan.filter.conflicts'),
      source: t('plan.bothDevices'),
      items: plan.conflicts.map((conflict) => ({
        path: conflict.path,
        size: conflict.local_size || conflict.remote_size || 0,
        time: 0,
        conflict,
      })),
    });
  }
  for (const side of ['local', 'remote']) {
    for (const kind of PLAN_KINDS) {
      if (filter !== 'all' && filter !== kind) continue;
      const items = (plan[side] || [])
        .filter((change) => change.kind === kind)
        .map((change) => ({
          path: change.path,
          size: change.size || 0,
          time: change.mtime_ns ? Math.round(change.mtime_ns / 1e6) : 0,
          change,
        }));
      if (!items.length) continue;
      groups.push({
        key: `${side}:${kind}`,
        side,
        kind,
        title: t(`plan.filter.${kind}`),
        source: planSourceName(plan, side),
        items,
      });
    }
  }
  return groups;
}

function planGroupPaths() {
  const map = new Map();
  planGroups('all').forEach((group) => {
    map.set(group.key, new Set(group.items.map((item) => item.path)));
  });
  return map;
}

function reconcilePlanSelection() {
  const selection = state.planSelection;
  if (!selection) return;
  const allowed = planGroupPaths().get(selection.group);
  if (!allowed) {
    state.planSelection = null;
    return;
  }
  const kept = [...selection.paths].filter((path) => allowed.has(path));
  if (!kept.length) {
    state.planSelection = null;
    return;
  }
  selection.paths = new Set(kept);
}

function planDecision(group, action) {
  if (group === 'conflict') {
    return action === 'keep' ? 'keep' : action === 'keep_both' ? 'keep_both' : 'accept';
  }
  const kind = group.split(':')[1];
  if (kind === 'deleted') {
    return action === 'revert' ? 'keep_in_library' : action === 'only_here' ? 'only_here' : 'delete_everywhere';
  }
  if (action === 'revert') return 'revert';
  if (action === 'only_here') return 'only_here';
  return 'accept';
}

function planActions(group, kind) {
  if (group === 'conflict') {
    return [
      { action: 'keep', label: t('plan.action.conflict.keep') },
      { action: 'accept', label: t('plan.action.conflict.accept') },
      { action: 'keep_both', label: t('plan.action.conflict.keepBoth') },
    ];
  }
  const actions = [
    { action: 'accept', label: t(`plan.action.${kind}.accept`) },
    { action: 'revert', label: t(`plan.action.${kind}.revert`) },
  ];
  if (kind === 'added' || kind === 'deleted') {
    actions.push({ action: 'only_here', label: t(`plan.action.${kind}.onlyHere`) });
  }
  return actions;
}

function planItemMeta(item, plan) {
  const parts = [];
  if (item.size) parts.push(size(item.size).text);
  if (item.time) parts.push(ago(item.time));
  if (item.conflict) {
    const entries = plan && plan.decisions && plan.decisions.entries;
    parts.push(entries && entries[item.path] ? t('plan.decisionSet') : t('plan.needsDecision'));
  }
  return parts.join(' · ');
}

function planView(snap) {
  const summary = planSummary();
  const connection = snap.connection || { connected: false };
  const readonly = !!connection.read_only;
  const diverged = !!connection.diverged;
  const connected = summary.connected;
  const stale = summary.stale;
  const busy = state.busy;
  const canCommit = connected && !stale && !diverged && !readonly && !busy
    && summary.unresolved === 0 && summary.paths > 0;
  const peerName = (summary.plan && summary.plan.peer_name)
    || connection.name
    || connection.dev
    || '';

  const notices = [];
  if (!connected) {
    notices.push(`<div class="alert">
      <div class="alert-ico">${icon('devices')}</div>
      <div class="alert-body"><b>${esc(t('plan.offlineTitle'))}</b><p>${esc(t('plan.offlineText'))}</p></div>
      <button class="btn btn--sm" data-tab-jump="more">${esc(t('plan.connect'))}</button>
    </div>`);
  }
  if (stale) {
    notices.push(`<div class="alert">
      <div class="alert-ico">${icon('alert')}</div>
      <div class="alert-body"><b>${esc(t('plan.staleTitle'))}</b><p>${esc(t('plan.staleText'))}</p></div>
      <button class="btn btn--sm" data-act="plan-refresh">${esc(t('plan.staleAction'))}</button>
    </div>`);
  }
  if (diverged) {
    notices.push(`<div class="alert">
      <div class="alert-ico bad">${icon('alert')}</div>
      <div class="alert-body"><b>${esc(t('plan.divergedTitle'))}</b><p>${esc(t('plan.divergedText'))}</p></div>
    </div>`);
  }
  if (readonly) {
    notices.push(`<div class="alert">
      <div class="alert-ico">${icon('sync')}</div>
      <div class="alert-body"><b>${esc(t('plan.readonlyTitle'))}</b><p>${esc(t('plan.readonlyText'))}</p></div>
    </div>`);
  }
  if (summary.error) {
    notices.push(`<div class="alert">
      <div class="alert-ico bad">${icon('alert')}</div>
      <div class="alert-body"><b>${esc(t('plan.errorTitle'))}</b><p>${esc(summary.error)}</p></div>
    </div>`);
  }

  const filters = `<div class="plan-filters">${PLAN_FILTERS.map((filter) => {
    const on = state.planFilter === filter ? ' is-on' : '';
    return `<button class="plan-filter${on}" data-plan-filter="${filter}">${esc(t(`plan.filter.${filter}`))}</button>`;
  }).join('')}</div>`;

  const groups = planGroups();
  const selection = state.planSelection;
  const selectedPaths = selection ? selection.paths : new Set();
  const lockDecisions = readonly || busy || !connected || stale || diverged;

  const body = groups.length
    ? groups.map((group) => {
      const allSelected = group.items.length > 0
        && selection && selection.group === group.key
        && group.items.every((item) => selectedPaths.has(item.path));
      const someSelected = selection && selection.group === group.key
        && group.items.some((item) => selectedPaths.has(item.path));
      const items = group.items.map((item) => {
        const checked = !!(selection && selection.group === group.key && selectedPaths.has(item.path));
        return `<label class="plan-item${checked ? ' is-selected' : ''}">
          <input class="plan-check" type="checkbox" data-plan-path="${esc(item.path)}" data-plan-group="${esc(group.key)}" ${checked ? 'checked' : ''} ${lockDecisions ? 'disabled' : ''}>
          <span class="plan-item-body">
            <span class="plan-item-path">${esc(item.path)}</span>
            <span class="plan-item-meta">${esc(planItemMeta(item, summary.plan))}</span>
          </span>
        </label>`;
      }).join('');
      return `<section class="plan-group">
        <div class="plan-group-head">
          <input class="plan-check" type="checkbox" data-plan-group="${esc(group.key)}" aria-label="${esc(t('plan.selectAll'))}" ${allSelected ? 'checked' : ''} ${someSelected && !allSelected ? 'data-indeterminate="1"' : ''} ${lockDecisions ? 'disabled' : ''}>
          <div class="plan-group-title"><b>${esc(group.title)}</b><span>${esc(group.source)}</span></div>
          <span class="tag">${group.items.length}</span>
        </div>
        <div class="plan-items">${items}</div>
      </section>`;
    }).join('')
    : `<div class="empty">${icon('check', 'empty-ic')}<b>${esc(t('plan.emptyTitle'))}</b><p>${esc(connected ? t('plan.emptyText') : t('plan.emptyLocalText'))}</p></div>`;

  let plate = '';
  if (selection && selection.paths.size) {
    const group = groups.find((entry) => entry.key === selection.group)
      || planGroups('all').find((entry) => entry.key === selection.group);
    const actions = group ? planActions(group.key, group.kind) : [];
    plate = `<div class="plan-plate">
      <div class="plan-plate-head">
        <span class="plan-plate-count">${esc(tp('plan.selected', selection.paths.size))}</span>
        <button class="plan-plate-clear" data-plan-clear>${esc(t('plan.clear'))}</button>
      </div>
      <div class="plan-plate-actions">
        ${actions.map((entry) => `<button class="btn btn--sm${entry.action === 'accept' ? ' btn--primary' : ''}" data-plan-action="${entry.action}" ${lockDecisions ? 'disabled' : ''}>${esc(entry.label)}</button>`).join('')}
        <button class="btn btn--sm btn--ghost" data-plan-all="${esc(selection.group)}" ${lockDecisions ? 'disabled' : ''}>${esc(t('plan.selectAll'))}</button>
      </div>
      ${lockDecisions ? `<div class="plan-plate-note">${esc(!connected ? t('plan.offlineActions') : stale || diverged ? t('plan.staleActions') : t('plan.readonlyActions'))}</div>` : ''}
    </div>`;
  }

  return `<section class="panel">
      <div class="kicker">${esc(t('tab.plan'))}</div>
      <h1 class="hero-title">${esc(t('plan.title'))}</h1>
      <p class="hero-hint">${esc(t('plan.sub'))}</p>
      <div class="plan-status">
        <span class="tag ${connected ? 'tag--good' : 'tag--warn'}">${esc(connected && peerName ? t('conn.connectedTo', { name: peerName }) : t('plan.offline'))}</span>
        ${summary.unresolved ? `<span class="tag tag--warn">${esc(tp('plan.unresolved', summary.unresolved))}</span>` : ''}
      </div>
      <div class="actions">
        <button class="btn btn--primary" data-act="plan-commit" ${canCommit ? '' : 'disabled'}>${icon('sync')}${esc(t('plan.commit'))}</button>
      </div>
    </section>

    ${notices.length ? `<section class="panel panel--flat"><div class="alert-list">${notices.join('')}</div></section>` : ''}

    <section class="panel panel--flat">
      <div class="panel-head">
        <div class="panel-title">${esc(t('plan.changes'))}</div>
      </div>
      ${filters}
      <div class="plan-groups">${body}</div>
    </section>

    ${plate}`;
}

function vaultView(snap) {
  const rows = snap.vault.map((v) => `
    <div class="row">
      <div class="row-top"><span class="row-path">${esc(v.path)}</span></div>
      <div class="row-meta">${esc(t('vault.rowMeta', { size: size(v.size).text, ago: ago(v.deleted_at) }))}</div>
      <div class="row-actions">
        <button class="btn btn--sm btn--lime" data-restore="${esc(v.path)}">${esc(t('vault.restore'))}</button>
      </div>
    </div>`).join('');

  return `<section class="panel">
      <h1 class="hero-title">${esc(t('vault.title'))}</h1>
      <p class="hero-hint">${esc(t('vault.hint'))}</p>
    </section>
    <section class="panel panel--flat">
      ${snap.vault.length
        ? `<div class="rows">${rows}</div>
           <div class="actions"><button class="btn btn--block btn--ghost" data-act="gc">${icon('trash')}${esc(t('vault.gc'))}</button></div>`
        : `<div class="empty">${icon('box', 'empty-ic')}<b>${esc(t('vault.emptyTitle'))}</b><p>${esc(t('vault.emptyText'))}</p></div>`}
    </section>`;
}

function moreView(snap) {
  const connection = snap.connection || { connected: false };
  const peers = snap.known_peers.map((p) => {
    const isConnected = connection.connected && connection.dev === p.dev;
    return `
    <div class="row">
      <div class="row-top">${icon('pc', '')}<span class="row-path">${esc(peerName(p))}</span></div>
      <div class="row-meta">${esc(p.addr)}${isConnected ? ' · ' + esc(t('conn.connected')) : ''}</div>
      <div class="row-actions">
        ${isConnected
          ? `<button class="btn btn--sm btn--ghost" data-act="disconnect">${esc(t('act.disconnect'))}</button>`
          : `<button class="btn btn--sm btn--lime" data-act="connect" data-dev="${esc(p.dev)}">${esc(t('act.connect'))}</button>`}
        <button class="btn btn--sm btn--ghost" data-act="forget" data-dev="${esc(p.dev)}">${esc(t('act.forget'))}</button>
      </div>
    </div>`;
  }).join('');

  return `<section class="panel">
      <h1 class="hero-title">${esc(t('tab.more'))}</h1>
      <p class="hero-hint">${esc(t('pair.hint'))}</p>
      <div class="actions">
        <button class="btn btn--primary" data-act="pair-code">${icon('qr')}${esc(t('pair.myCode'))}</button>
        <button class="btn btn--soft" data-act="pair-scan">${icon('camera')}${esc(t('pair.scan'))}</button>
      </div>
      ${state.code ? `
        <div class="field">
          <label>${esc(t('pair.codeLabel'))}</label>
          <input class="input" id="my-code" value="${esc(state.code)}" readonly>
        </div>
        <div class="actions">
          <button class="btn btn--block btn--soft" data-act="copy-code">${icon('copy')}${esc(t('pair.copy'))}</button>
        </div>` : ''}
      <div class="field">
        <label for="pair-input">${esc(t('pair.inputLabel'))}</label>
        <input class="input" id="pair-input" placeholder="syncth://pair?…" spellcheck="false" autocapitalize="off">
      </div>
      <div class="actions">
        <button class="btn btn--block btn--soft" data-act="pair-accept">${esc(t('pair.submit'))}</button>
      </div>
    </section>

    <section class="panel panel--flat">
      <div class="panel-head">
        <div class="panel-title">${esc(t('panel.devices'))}</div>
        <span class="tag">${snap.known_peers.length ? esc(t('devices.tag', { n: num(snap.known_peers.length) })) : esc(t('devices.tagEmpty'))}</span>
      </div>
      ${peers ? `<div class="rows">${peers}</div>` : `<div class="empty"><b>${esc(t('devices.emptyTitle'))}</b><p>${esc(t('pair.noDevices'))}</p></div>`}
    </section>

    <section class="panel panel--flat">
      <div class="panel-head"><div class="panel-title">${esc(t('panel.folder'))}</div></div>
      <div class="row-meta">${esc(snap.root || '')}</div>
      <div class="row-meta">${num(snap.files)} ${plural('word.files', snap.files)} · ${esc(size(snap.size).text)}</div>
      <div class="actions">
        <button class="btn btn--block btn--ghost" data-act="change-root">${esc(t('folder.change'))}</button>
      </div>
    </section>

    <section class="panel panel--flat">
      <div class="panel-head"><div class="panel-title">${esc(t('pair.language'))}</div></div>
      <div class="actions">
        <button class="btn btn--sm${I18N.locale === 'ru' ? ' btn--lime' : ' btn--ghost'}" data-act="lang" data-lang="ru">Русский</button>
        <button class="btn btn--sm${I18N.locale === 'en' ? ' btn--lime' : ' btn--ghost'}" data-act="lang" data-lang="en">English</button>
      </div>
    </section>

    <section class="panel panel--flat">
      <div class="panel-head"><span class="panel-title">${esc(t('pair.about'))}</span></div>
      <div class="row-meta">${esc(t('pair.version', { v: (state.info && state.info.version) || '' }))}</div>
      <div class="row-meta">${esc(t('pair.device', { dev: snap.name || snap.device || '' }))}</div>
      <div class="row-meta">${esc(t('pair.port', { port: (state.info && state.info.sync_port) || 47887 }))}</div>
    </section>`;
}

// ------------------------------------------------------------------ действия

async function run(label, work) {
  if (state.busy) return;
  state.busy = true;
  render();
  try {
    await work();
  } catch (error) {
    toast(`${label}: ${errText(error)}`, true);
  } finally {
    state.busy = false;
    render();
  }
}

async function initLibrary() {
  const input = $('#root-input');
  const root = input ? input.value.trim() : '';
  if (!root) { toast(t('toast.needFolder'), true); return; }
  await run(t('run.init'), async () => {
    const result = await invoke('init_library', { root });
    state.status = result.status;
    if (state.info && state.info.info) {
      state.info.info.root = state.status.root;
      state.info.info.device = state.status.device;
      state.info.info.name = state.status.name;
    }
    await loadPlan();
    toast(t('toast.initDone', { n: num(result.added), word: plural('word.files', result.added) }));
  });
}

async function action(name, payload, data) {
  switch (name) {
    case 'init':
      await initLibrary();
      break;
    case 'refresh':
      await run(t('run.refresh'), async () => {
        await loadStatus();
        await loadPlan();
      });
      break;
    case 'scan':
      await run(t('run.scan'), async () => {
        const result = await invoke('scan_now');
        state.status = result.status;
        await loadPlan();
        toast(t('toast.scan', { added: result.added, changed: result.changed, missing: result.missing }));
      });
      break;
    case 'sync':
      await run(t('run.sync'), async () => {
        const result = await invoke('sync_now');
        state.status = result.status;
        await loadPlan();
        if (result.errors && result.errors.length) {
          toast(t('toast.syncPartial', { error: result.errors[0] }), true);
        } else if (typeof result.accepted === 'number') {
          toast(t('toast.commit', { accepted: result.accepted, rejected: result.rejected }));
        } else {
          toast(t('toast.sync', { sent: result.sent, received: result.received }));
        }
      });
      break;
    case 'plan-refresh':
      await run(t('run.planRefresh'), async () => {
        await loadPlan();
        if (planSummary().stale) toast(t('toast.planStale'), true);
        else toast(t('toast.planRefreshed'));
      });
      break;
    case 'plan-commit':
      if (!window.confirm(t('confirm.commit'))) return;
      await run(t('run.commit'), async () => {
        const result = await invoke('commit_plan');
        state.status = result.status;
        state.plan = result.plan;
        toast(t('toast.commit', { accepted: result.accepted, rejected: result.rejected }));
      });
      break;
    case 'connect':
      await run(t('run.connect'), async () => {
        const result = await invoke('connect_peer', { dev: data && data.dev });
        state.status = result.status;
        await loadPlan();
        toast(t('toast.connected', { name: result.name || result.dev }));
      });
      break;
    case 'disconnect':
      await run(t('run.disconnect'), async () => {
        state.status = await invoke('disconnect_peer');
        toast(t('toast.disconnected'));
      });
      break;
    case 'forget':
      if (data && data.dev && !window.confirm(t('confirm.forgetPeer'))) return;
      await run(t('run.forgetPeer'), async () => {
        state.status = await invoke('forget_peer', { dev: data.dev });
        toast(t('toast.peerForgotten'));
      });
      break;
    case 'gc':
      await run(t('run.gc'), async () => {
        const result = await invoke('vault_gc');
        state.status = result.status;
        toast(t('toast.gc', { n: result.removed }));
      });
      break;
    case 'pair-code':
      await run(t('run.code'), async () => {
        const result = await invoke('pair_code');
        state.code = result.code;
      });
      break;
    case 'copy-code':
      await copy(state.code || '');
      break;
    case 'pair-accept':
      await pairAccept();
      break;
    case 'pair-scan':
      await scanQr();
      break;
    case 'lang':
      I18N.setLocale(payload);
      break;
    case 'change-root':
      // Смена папки — это новая библиотека: старый индекс остаётся в журнале,
      // поэтому сначала предупреждаем, а не молча сканируем другое дерево.
      if (state.status && state.status.root) {
        const ok = window.confirm(t('confirm.changeRoot'));
        if (!ok) return;
        if (state.info) state.info.default_root = state.status.root;
      }
      state.status = null;
      state.error = null;
      state.tab = 'overview';
      render();
      break;
    default:
      if (payload) await payload();
  }
}

async function pairAccept() {
  const input = $('#pair-input');
  const code = input ? input.value.trim() : '';
  if (!code) { toast(t('toast.needCode'), true); return; }
  await pairAcceptCode(code);
}

async function pairAcceptCode(code) {
  await run(t('run.pair'), async () => {
    const result = await invoke('pair_accept', { code });
    await loadStatus();
    toast(t('toast.paired', { dev: result.name || result.dev }));
  });
}

// ------------------------------------------------------------------ QR-сканер

let qrScan = null;

async function scanQr() {
  if (qrScan) return;
  if (!('BarcodeDetector' in window) || !navigator.mediaDevices) {
    toast(t('pair.scanUnsupported'), true);
    return;
  }

  const host = document.createElement('div');
  host.className = 'qr-scan';
  host.innerHTML = `
    <video id="qr-video" playsinline muted autoplay></video>
    <div class="qr-frame" aria-hidden="true"></div>
    <div class="qr-scan-bar">
      <p class="qr-scan-hint">${esc(t('pair.scanHint'))}</p>
      <button class="btn btn--block btn--soft" data-qr-close>${esc(t('pair.scanCancel'))}</button>
    </div>`;
  document.body.append(host);

  const session = { host, stream: null, timer: null };
  qrScan = session;
  host.querySelector('[data-qr-close]').addEventListener('click', stopQrScan);

  try {
    const formats = await BarcodeDetector.getSupportedFormats().catch(() => []);
    if (!formats.includes('qr_code')) {
      stopQrScan();
      toast(t('pair.scanUnsupported'), true);
      return;
    }

    const stream = await navigator.mediaDevices.getUserMedia({
      audio: false,
      video: { facingMode: { ideal: 'environment' } },
    });
    if (qrScan !== session) {
      stream.getTracks().forEach((track) => track.stop());
      return;
    }
    session.stream = stream;

    const video = host.querySelector('video');
    video.srcObject = stream;
    await video.play();

    const detector = new BarcodeDetector({ formats: ['qr_code'] });
    const tick = async () => {
      if (qrScan !== session) return;
      try {
        const found = await detector.detect(video);
        const code = found
          .map((item) => item.rawValue)
          .find((value) => value && value.startsWith('syncth://pair'));
        if (code) {
          stopQrScan();
          await pairAcceptCode(code);
          return;
        }
      } catch {
        // Кадр без кода — просто пробуем следующий.
      }
      if (qrScan === session) session.timer = setTimeout(tick, 250);
    };
    session.timer = setTimeout(tick, 250);
  } catch (error) {
    stopQrScan();
    toast(error && error.name === 'NotAllowedError' ? t('pair.scanDenied') : errText(error), true);
  }
}

function stopQrScan() {
  if (!qrScan) return;
  const session = qrScan;
  qrScan = null;
  clearTimeout(session.timer);
  if (session.stream) session.stream.getTracks().forEach((track) => track.stop());
  const video = session.host.querySelector('video');
  if (video) video.srcObject = null;
  session.host.remove();
}

async function copy(text) {
  if (!text) return;
  try {
    await navigator.clipboard.writeText(text);
    toast(t('common.copied'));
  } catch {
    const input = $('#my-code');
    if (input) { input.select(); document.execCommand('copy'); toast(t('common.copied')); }
  }
}

async function applyPlanDecision(action) {
  const selection = state.planSelection;
  if (!selection || !selection.paths.size) return;
  const summary = planSummary();
  if (!summary.connected || summary.stale) return;
  const decision = planDecision(selection.group, action);
  const paths = [...selection.paths];
  await run(t('run.decision'), async () => {
    for (const path of paths) {
      await invoke('set_plan_decision', { path, action: decision });
    }
    state.planSelection = null;
    await loadStatus();
    await loadPlan();
    toast(tp('toast.decisionsSaved', paths.length));
  });
}

async function resolvePending(path, decision) {
  if (decision === 'delete_everywhere') {
    const ok = window.confirm(t('confirm.delete', { path }));
    if (!ok) return;
  }
  await run(t('run.resolve'), async () => {
    const result = await invoke('resolve_pending', { path, action: decision });
    state.status = result.status;
    if (result.failed && result.failed.length) {
      toast(t('toast.failed', { path: result.failed[0][0] }), true);
    } else {
      toast(decision === 'delete_everywhere' ? t('toast.deleted') : t('toast.kept'));
    }
  });
}

async function forgetPending(path) {
  await run(t('run.forget'), async () => {
    state.status = await invoke('forget_pending', { path });
    toast(t('toast.forgotten'));
  });
}

async function restorePath(path) {
  await run(t('run.restore'), async () => {
    state.status = await invoke('restore', { path });
    toast(t('toast.restored', { path }));
  });
}

// ------------------------------------------------------------------ события

document.addEventListener('change', (event) => {
  const box = event.target.closest('.plan-check');
  if (!box) return;
  const groupKey = box.dataset.planGroup;
  if (!groupKey) return;

  if (box.dataset.planPath) {
    let selection = state.planSelection;
    if (!selection || selection.group !== groupKey) {
      selection = { group: groupKey, paths: new Set() };
    }
    if (box.checked) selection.paths.add(box.dataset.planPath);
    else selection.paths.delete(box.dataset.planPath);
    state.planSelection = selection.paths.size ? selection : null;
    render();
    return;
  }

  const group = planGroups('all').find((entry) => entry.key === groupKey);
  if (box.checked && group) {
    state.planSelection = {
      group: groupKey,
      paths: new Set(group.items.map((item) => item.path)),
    };
  } else if (state.planSelection && state.planSelection.group === groupKey) {
    state.planSelection = null;
  }
  render();
});

document.addEventListener('click', (event) => {
  const tab = event.target.closest('.tab');
  if (tab) {
    state.tab = tab.dataset.tab;
    render();
    $('#view').scrollTop = 0;
    if (state.tab === 'plan') loadPlan().then(render);
    return;
  }

  const jump = event.target.closest('[data-tab-jump]');
  if (jump) {
    state.tab = jump.dataset.tabJump;
    render();
    $('#view').scrollTop = 0;
    if (state.tab === 'plan') loadPlan().then(render);
    return;
  }

  const planFilter = event.target.closest('[data-plan-filter]');
  if (planFilter) {
    state.planFilter = planFilter.dataset.planFilter;
    const visible = new Set(planGroups(state.planFilter).map((group) => group.key));
    if (state.planSelection && !visible.has(state.planSelection.group)) {
      state.planSelection = null;
    }
    render();
    return;
  }

  const planAll = event.target.closest('[data-plan-all]');
  if (planAll) {
    const group = planGroups('all').find((entry) => entry.key === planAll.dataset.planAll);
    if (group) {
      state.planSelection = {
        group: group.key,
        paths: new Set(group.items.map((item) => item.path)),
      };
      render();
    }
    return;
  }

  const planClear = event.target.closest('[data-plan-clear]');
  if (planClear) {
    state.planSelection = null;
    render();
    return;
  }

  const planAction = event.target.closest('[data-plan-action]');
  if (planAction && !planAction.disabled) {
    applyPlanDecision(planAction.dataset.planAction);
    return;
  }

  const pick = event.target.closest('[data-pick]');
  if (pick) {
    const input = $('#root-input');
    if (input) input.value = pick.dataset.pick;
    return;
  }

  const resolveBtn = event.target.closest('[data-resolve]');
  if (resolveBtn) {
    resolvePending(resolveBtn.dataset.path, resolveBtn.dataset.resolve);
    return;
  }

  const forgetBtn = event.target.closest('[data-forget]');
  if (forgetBtn) {
    forgetPending(forgetBtn.dataset.forget);
    return;
  }

  const restoreBtn = event.target.closest('[data-restore]');
  if (restoreBtn) {
    restorePath(restoreBtn.dataset.restore);
    return;
  }

  const act = event.target.closest('[data-act]');
  if (act) {
    action(
      act.dataset.act,
      act.dataset.act === 'lang' ? act.dataset.lang : undefined,
      { ...act.dataset },
    );
  }
});

window.__syncth = { state, boot, render, loadStatus, action };

I18N.onChange(() => {
  render();
});

// Пока приложение открыто, сеанс на другой стороне может менять решения и
// статус. Опрашиваем нечасто, чтобы не тратить батарею.
setInterval(async () => {
  if (!state.status || state.busy || state.polling || document.hidden) return;
  state.polling = true;
  try {
    await loadStatus();
    if (state.tab === 'plan') await loadPlan();
    render();
  } catch {
    // Молча: следующая попытка через несколько секунд.
  } finally {
    state.polling = false;
  }
}, 5000);

boot();
