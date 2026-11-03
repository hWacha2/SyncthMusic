const bridge = window.__TAURI__?.core;
const invoke = bridge?.invoke;

const state = {
  view: 'loading',
  info: null,
  root: null,
  phone: { linked: false },
  lan: [],
  adb: [],
  adbError: '',
  discovering: false,
  busy: '',
  note: '',
  error: false,
  phoneBusy: '',
  workspace: {
    data: null,
    refreshing: false,
    error: '',
    statusError: '',
    action: '',
    tab: 'overview',
    note: '',
    noteError: false,
  },
  modal: null,
};

// Что уже нарисовано. Как и на телефоне, не трогаем DOM, если HTML тот же:
// иначе опрос статуса будет вызывать мерцание и сбрасывать фокус.
const painted = { screen: null, view: null };

// Номер поколения данных рабочей области. Каждый новый запрос получает
// номер, и только ответ с актуальным номером попадает в состояние. Иначе
// запоздавший ответ старого устройства или старого опроса перетирает новое.
let workspaceGen = 0;
let workspacePendingRefresh = false;

function t(key, vars) {
  return window.I18N.t(key, vars);
}

function esc(value) {
  return String(value ?? '').replace(/[&<>"']/g, (char) => ({
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#39;',
  }[char]));
}

function message(error) {
  if (!error) return t('error.unknown');
  if (typeof error === 'string') return error;
  return error.message || String(error);
}

function sizeParts(bytes) {
  const value = Number(bytes) || 0;
  const units = window.I18N.lang === 'ru'
    ? ['Б', 'КБ', 'МБ', 'ГБ', 'ТБ']
    : ['B', 'KB', 'MB', 'GB', 'TB'];
  if (value < 1024) return { value: String(value), unit: units[0] };
  let size = value / 1024;
  let unit = 1;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return {
    value: size >= 10 ? String(Math.round(size)) : size.toFixed(1),
    unit: units[unit],
  };
}

function formatSize(bytes) {
  const parts = sizeParts(bytes);
  const value = window.I18N.lang === 'ru' ? parts.value.replace('.', ',') : parts.value;
  return `${value} ${parts.unit}`;
}

function number(value) {
  return (Number(value) || 0).toLocaleString(window.I18N.lang === 'ru' ? 'ru-RU' : 'en-US');
}

function ago(ms) {
  const value = Number(ms) || 0;
  if (!value) return t('time.never');
  const seconds = Math.max(0, (Date.now() - value) / 1000);
  if (seconds < 45) return t('time.now');
  if (seconds < 3600) return t('time.minutes', { count: Math.max(1, Math.round(seconds / 60)) });
  if (seconds < 86400) return t('time.hours', { count: Math.max(1, Math.round(seconds / 3600)) });
  return t('time.days', { count: Math.max(1, Math.round(seconds / 86400)) });
}

function basename(path) {
  const text = String(path || '');
  const parts = text.split(/[\\/]/).filter(Boolean);
  return parts.length ? parts[parts.length - 1] : text;
}

function iconFolder() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true">
    <path d="M3.5 7.4A1.4 1.4 0 0 1 4.9 6h4.2l1.8 2.1h8.2a1.4 1.4 0 0 1 1.4 1.4v7.1a1.4 1.4 0 0 1-1.4 1.4H4.9a1.4 1.4 0 0 1-1.4-1.4z"/>
  </svg>`;
}

function iconPhone() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true">
    <rect x="7" y="3" width="10" height="18" rx="2.4"/>
    <path d="M10.5 18h3"/>
  </svg>`;
}

function iconPc() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true">
    <rect x="3" y="4.5" width="18" height="12" rx="2"/>
    <path d="M9 20h6M12 16.5V20"/>
  </svg>`;
}

function iconOverview() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true">
    <rect x="3.5" y="3.5" width="7" height="7" rx="2"/>
    <rect x="13.5" y="3.5" width="7" height="7" rx="2"/>
    <rect x="3.5" y="13.5" width="7" height="7" rx="2"/>
    <rect x="13.5" y="13.5" width="7" height="7" rx="2"/>
  </svg>`;
}

function iconList() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true">
    <path d="M4 6.5h16M4 12h16M4 17.5h10"/>
  </svg>`;
}

function iconVault() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true">
    <path d="M4 7.5 12 3l8 4.5v9L12 21l-8-4.5z"/>
    <path d="M4.4 7.6 12 12l7.6-4.4M12 12v9"/>
  </svg>`;
}

function iconCheck() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12.5 4.5 4.5L19 7"/></svg>`;
}

function iconUsb() {
  return `
<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 256 256">

\t<path fill="currentColor" fill-rule="evenodd" d="M129.042 230.413c14.807.56 24.14-9.993 24.14-20.644c0-10.652-10.127-19.1-15.966-19.1c0-6.462 0-34.093-.022-39.406c5.368-1.812 8.29-1.81 17.39-5.088c9.097-3.278 8.624-2.793 15.03-8.938c6.407-6.144 7.441-24.457 7.441-30.875c3.484-.18 13.856.036 13.856.036l-.422-33.11l-37.406.393v32.078h11.25c0 3.828.86 12.614-1.518 18.645c-2.377 6.031-2.992 5.485-9.732 8.245s-15.94 4.323-15.94 4.323s-.36-73.992-.144-73.992l13.224.001c1.102 0 1.52-.767.942-1.7l-22.217-35.878l-22.728 36.321c-.586.936-.17 1.676.943 1.651l15.465-.339s.75 97.485.732 101.874c-4.272 0-13.788-.045-20.224-5.307s-7.105-8.643-8.932-14.428c-1.828-5.785-2.512-12.533-2.512-17.572c6.495 0 17.021-10.184 17.021-19.98c0-9.798-11.035-22.247-21.027-22.247s-23.23 8.282-23.23 20.986s10.022 21.24 15.59 21.24c0 3.907.606 13.36 2.102 19.03c1.496 5.673 2.405 5.92 3.447 8.371c2.277 5.357 2.599 9.327 13.277 15.643c10.678 6.315 20.318 6.101 24.488 6.101c.25 2.888 0 10.361 0 13.922c-7.875 0-17.2 7.9-17.2 19.29c0 11.387 8.075 19.894 22.882 20.454" />
</svg>


`;
}

function spinner() {
  return `<span class="spinner" aria-hidden="true"></span>`;
}

function noteView() {
  if (!state.note && !state.error) return '';
  const cls = state.error ? 'setup-note is-error' : 'setup-note';
  return `<p class="${cls}" role="${state.error ? 'alert' : 'status'}">${esc(state.note)}</p>`;
}

function lanRow(device) {
  const key = `lan:${device.dev}`;
  const busy = state.busy === key;
  return `<div class="device-item ${busy ? 'is-busy' : ''}">
    <span class="device-icon device-icon--lan">${iconPhone()}</span>
    <div class="device-copy">
      <div class="device-name">${esc(device.name || device.dev)}</div>
      <div class="device-addr">${esc(device.addr)}</div>
    </div>
    ${busy
      ? `<button class="btn btn--quiet" data-cancel>${esc(t('setup.cancel'))}</button>`
      : `<button class="btn" data-connect-lan data-dev="${esc(device.dev)}" data-addr="${esc(device.addr)}"
          ${state.busy ? 'disabled' : ''}>${esc(t('setup.connect'))}</button>`}
  </div>`;
}

function adbRow(device) {
  const key = `adb:${device.serial}`;
  const busy = state.busy === key;
  const ready = device.state === 'device';
  const label = ready
    ? (busy ? t('setup.connecting') : t('setup.connect'))
    : (device.state === 'unauthorized' ? t('setup.unauthorized') : t('setup.offline'));
  return `<div class="device-item ${busy ? 'is-busy' : ''}">
    <span class="device-icon device-icon--usb">${iconUsb()}</span>
    <div class="device-copy">
      <div class="device-name">${esc(device.model || device.serial)}</div>
      <div class="device-addr">${esc(device.serial)}</div>
    </div>
    ${busy
      ? `<button class="btn btn--quiet" data-cancel>${esc(t('setup.cancel'))}</button>`
      : `<button class="btn" data-connect-adb data-serial="${esc(device.serial)}"
          ${!ready || state.busy ? 'disabled' : ''}>${esc(label)}</button>`}
  </div>`;
}

function devicesView() {
  const lan = state.lan.length
    ? state.lan.map(lanRow).join('')
    : `<p class="group-empty">${esc(state.discovering ? t('setup.searching') : t('setup.empty_lan'))}</p>`;
  const adb = state.adb.length
    ? state.adb.map(adbRow).join('')
    : `<p class="group-empty">${esc(state.discovering ? t('setup.searching') : t('setup.empty_adb'))}</p>`;

  const adbError = state.adbError
    ? `<p class="group-error">${esc(state.adbError)}</p>`
    : '';

  return `<section class="setup">
    <div class="setup-head">
      <h1 class="setup-title">${esc(t('setup.title'))}</h1>
      <div class="setup-actions">
        ${state.phone.linked
          ? `<button class="btn" id="open-workspace">${esc(t('workspace.back'))}</button>`
          : ''}
        ${state.root
          ? `<button class="btn btn--quiet" id="open-library">${esc(t('setup.change_folder'))}</button>`
          : ''}
        <button class="btn btn--quiet" id="refresh" ${state.discovering ? 'disabled' : ''}>
          ${state.discovering ? spinner() : ''}${esc(t('setup.refresh'))}
        </button>
      </div>
    </div>
    <div class="setup-body">
      <section class="group">
        <h2 class="group-title">${esc(t('setup.lan'))}</h2>
        <div class="device-list">${lan}</div>
      </section>
      <section class="group">
        <h2 class="group-title">${esc(t('setup.adb'))}</h2>
        <div class="device-list">${adb}</div>
        ${adbError}
      </section>
    </div>
    ${noteView()}
  </section>`;
}

function languageView() {
  return `<section class="setup setup--center">
    <div class="library-card language-card">
      <h1 class="setup-title">SynctMusic</h1>
      <p class="setup-hint">Выберите язык · Choose your language</p>
      <div class="language-actions">
        <button class="btn lang-choice" data-lang="ru">Русский</button>
        <button class="btn btn--quiet lang-choice" data-lang="en">English</button>
      </div>
    </div>
  </section>`;
}

function libraryView() {
  return `<section class="setup setup--center">
    <div class="library-card">
      <span class="library-icon">${iconFolder()}</span>
      <h1 class="setup-title">${esc(t('library.title'))}</h1>
      <p class="setup-hint">${esc(t('library.hint'))}</p>
      <div class="library-actions">
        <button class="btn btn--large" id="choose" ${state.busy === 'folder' ? 'disabled' : ''}>
          ${state.busy === 'folder' ? spinner() : ''}${esc(state.busy === 'folder' ? t('library.choosing') : t('library.action'))}
        </button>
        ${state.root
          ? `<button class="btn btn--quiet" id="open-devices">${esc(t('library.back'))}</button>`
          : ''}
      </div>
      ${noteView()}
    </div>
  </section>`;
}

// ------------------------------------------------------------------ workspace

function workspaceData() {
  return state.workspace.data;
}

function planData(data = workspaceData()) {
  return data && data.plan ? data.plan : null;
}

function decisionItems(data = workspaceData()) {
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

function pendingDecisions(data = workspaceData()) {
  return decisionItems(data).filter((item) => !item.decided);
}

function pendingCount(data = workspaceData()) {
  return pendingDecisions(data).length;
}

function workspaceConnected(data) {
  return Boolean(data && data.phone && data.phone.connected);
}

function sessionReady(data) {
  return Boolean(data && data.session && data.session.open && !data.session.finished);
}

function canCommit(data = workspaceData()) {
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

function headline(data) {
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

function connectionState(data) {
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

function checkedText(data) {
  if (!data || !data.checked_at) return t('workspace.checked_never');
  return ago(data.checked_at);
}

function railItem(id, icon, label, count, hot) {
  const active = state.workspace.tab === id ? ' is-active' : '';
  const badge = count
    ? `<span class="ws-nav-count${hot ? ' is-hot' : ''}">${number(count)}</span>`
    : '';
  return `<button class="ws-nav-item${active}" data-ws-tab="${id}">
    <span class="ws-nav-icon">${icon}</span>
    <span class="ws-nav-label">${esc(label)}</span>
    ${badge}
  </button>`;
}

function workspaceRail(data) {
  const pending = pendingCount(data);
  const manual = Boolean(data && data.manual_additions);
  const vault = Number(data && data.pc && data.pc.vault_files) || 0;
  const connection = connectionState(data);
  const name = (data && data.pc && data.pc.name)
    || state.info?.device_name
    || t('workspace.this_pc');
  const root = (data && data.pc && data.pc.root) || state.root || '';
  const version = state.info?.version || '';

  return `<aside class="ws-rail">
    <div class="ws-device-card">
      <span class="ws-kicker">${esc(t('workspace.this_pc'))}</span>
      <div class="ws-device-name" title="${esc(name)}">${esc(name)}</div>
      <div class="ws-device-root" title="${esc(root)}">${esc(root ? basename(root) : t('workspace.no_folder'))}</div>
    </div>
    <nav class="ws-nav" aria-label="${esc(t('workspace.nav'))}">
      ${railItem('overview', iconOverview(), t('workspace.tab.overview'), 0, false)}
      ${railItem('changes', iconList(), t('workspace.tab.changes'), pending, true)}
      ${railItem('vault', iconVault(), t('workspace.tab.vault'), vault, false)}
    </nav>
    <div class="ws-rail-foot">
      <button class="ws-manual" id="ws-manual" role="switch" aria-checked="${manual ? 'true' : 'false'}" title="${esc(t('workspace.manual.hint'))}" ${state.workspace.action || state.workspace.refreshing ? 'disabled' : ''}>
        <span class="ws-manual-text">${esc(t('workspace.manual.label'))}</span>
        <span class="ws-switch${manual ? ' is-on' : ''}" aria-hidden="true"><span class="ws-switch-dot"></span></span>
      </button>
      <div class="ws-rail-state">
        <span class="status-dot ${connection.tone === 'on' ? 'is-on' : connection.tone === 'warn' ? 'is-warn' : ''}" aria-hidden="true"></span>
        <span>${esc(connection.label)}</span>
      </div>
      <button class="ws-link" id="open-devices">${esc(t('workspace.devices'))}</button>
      ${version ? `<span class="ws-version">v${esc(version)} · AGPL-3.0</span>` : ''}
    </div>
  </aside>`;
}

function workspaceActions(data, tab) {
  const busy = state.workspace.refreshing || Boolean(state.workspace.action);
  const refreshLabel = state.workspace.refreshing ? t('workspace.checking') : t('workspace.check');
  if (tab === 'vault') {
    const hasVault = data && Number(data.pc && data.pc.vault_files) > 0;
    return `<button class="btn btn--quiet" id="ws-refresh" ${busy ? 'disabled' : ''}>
        ${state.workspace.refreshing ? spinner() : ''}${esc(refreshLabel)}
      </button>
      <button class="btn" id="ws-gc" ${!hasVault || busy ? 'disabled' : ''}>
        ${state.workspace.action === 'gc' ? spinner() : ''}${esc(t('workspace.vault.gc'))}
      </button>`;
  }
  const plan = planData(data);
  const paths = Number(plan?.paths) || 0;
  const aligned = Number(plan?.aligned) || 0;
  // Если различий нет, а совпадения ещё не зафиксированы, коммит нужен
  // только чтобы записать общую базу.
  const fixing = paths === 0 && aligned > 0;
  const commitLabel = state.workspace.action === 'commit'
    ? (fixing ? t('workspace.fixing') : t('workspace.syncing'))
    : (fixing ? t('workspace.fix') : t('workspace.sync'));
  const enabled = canCommit(data);
  return `<button class="btn btn--quiet" id="ws-refresh" ${busy ? 'disabled' : ''}>
      ${state.workspace.refreshing ? spinner() : ''}${esc(refreshLabel)}
    </button>
    <button class="btn" id="ws-commit" ${enabled ? '' : 'disabled'}>
      ${state.workspace.action === 'commit' ? spinner() : ''}${esc(commitLabel)}
    </button>`;
}

function workspaceHead(data, tab) {
  let kicker = t('workspace.kicker');
  let title;
  let text;
  if (tab === 'changes') {
    const plan = planData(data);
    const count = plan ? Number(plan.paths) || 0 : 0;
    const pending = pendingCount(data);
    title = t('workspace.tab.changes');
    text = !data
      ? t('workspace.sub.unknown')
      : pending
        ? t('workspace.changes.sub_pending', { count, pending })
        : t('workspace.changes.sub', { count });
  } else if (tab === 'vault') {
    title = t('workspace.tab.vault');
    text = t('workspace.vault.sub', {
      count: number(data?.pc?.vault_files || 0),
      size: formatSize(data?.pc?.vault_size || 0),
    });
  } else {
    const head = headline(data);
    title = head.title;
    text = head.text;
  }

  return `<header class="ws-head">
    <div class="ws-head-copy">
      <p class="ws-kicker">${esc(kicker)}</p>
      <h1 class="ws-title">${esc(title)}</h1>
      <p class="ws-sub">${esc(text)}</p>
    </div>
    <div class="ws-head-actions">${workspaceActions(data, tab)}</div>
  </header>`;
}

function workspaceNotice(data) {
  const rows = [];
  if (state.workspace.error) {
    rows.push({ tone: 'bad', text: t('workspace.error.refresh', { error: state.workspace.error }) });
  }
  if (data && data.session_error) {
    rows.push({ tone: 'warn', text: t('workspace.error.session', { error: data.session_error }) });
  }
  if (data && data.phone_error) {
    rows.push({ tone: 'warn', text: t('workspace.error.phone', { error: data.phone_error }) });
  }
  if (state.workspace.statusError) {
    rows.push({ tone: 'warn', text: t('workspace.error.status', { error: state.workspace.statusError }) });
  }
  if (state.workspace.note) {
    rows.push({ tone: state.workspace.noteError ? 'bad' : 'ok', text: state.workspace.note });
  }
  if (!rows.length) return '';
  return `<div class="ws-notices">${rows.map((row) => `
    <p class="ws-notice is-${row.tone}" role="${row.tone === 'ok' ? 'status' : 'alert'}">
      <span class="ws-notice-dot" aria-hidden="true"></span>${esc(row.text)}
    </p>`).join('')}</div>`;
}

function statCard(label, value, note, unit = '') {
  return `<div class="ws-stat">
    <span class="ws-stat-k">${esc(label)}</span>
    <div class="ws-stat-v">${esc(value)}${unit ? `<small>${esc(unit)}</small>` : ''}</div>
    <span class="ws-stat-n">${esc(note)}</span>
  </div>`;
}

function statsView(data) {
  const pc = data?.pc || {};
  const plan = planData(data);
  const pending = pendingCount(data);
  const paths = Number(plan?.paths) || 0;
  const aligned = Number(plan?.aligned) || 0;
  const phoneScan = data?.phone_scan;
  const phoneSize = sizeParts(phoneScan?.total || 0);

  return `<div class="ws-stats">
    ${statCard(
      t('workspace.stats.pc'),
      number(pc.files || 0),
      t('workspace.stats.pc_note', { count: number(pc.local || 0) }),
    )}
    ${statCard(
      t('workspace.stats.phone'),
      phoneScan ? number(phoneScan.count || 0) : '—',
      phoneScan
        ? `${formatSize(phoneScan.total || 0)} · ${t('workspace.stats.phone_note')}`
        : t('workspace.stats.phone_empty'),
    )}
    ${statCard(
      t('workspace.stats.plan'),
      number(paths),
      pending
        ? t('workspace.stats.plan_pending', { count: number(pending) })
        : paths > 0
          ? aligned > 0
            ? t('workspace.stats.plan_ready_aligned', { count: aligned })
            : t('workspace.stats.plan_ready')
          : aligned > 0
            ? t('workspace.stats.plan_aligned', { count: aligned })
            : t('workspace.stats.plan_empty'),
    )}
    ${statCard(
      t('workspace.stats.vault'),
      number(pc.vault_files || 0),
      Number(pc.vault_files) > 0
        ? formatSize(pc.vault_size || 0)
        : t('workspace.stats.vault_empty'),
    )}
  </div>`;
}

function connectionNode(kind, title, sub, on) {
  return `<div class="ws-node">
    <span class="ws-node-icon ${on ? 'is-on' : ''}">${kind === 'pc' ? iconPc() : iconPhone()}</span>
    <div class="ws-node-copy">
      <span class="ws-node-name">${esc(title)}</span>
      <span class="ws-node-sub">${esc(sub)}</span>
    </div>
  </div>`;
}

function connectionCard(data) {
  const phone = data?.phone || {};
  const stateInfo = connectionState(data);
  const connected = stateInfo.tone === 'on';
  const phoneTitle = phone.name || phone.dev || t('workspace.connection.phone');
  const phoneSub = phone.root ? basename(phone.root) : t('workspace.connection.no_folder');
  const pcName = data?.pc?.name || t('workspace.this_pc');
  const folder = phone.root
    ? `<span class="ws-kv-value is-mono" title="${esc(phone.root)}">${esc(phone.root)}</span>`
    : `<span class="ws-kv-value is-empty">${esc(t('workspace.connection.no_folder'))}</span>`;

  const folderAction = phone.linked
    ? `<button class="btn btn--quiet btn--small" id="ws-change-root" ${state.phoneBusy ? 'disabled' : ''}>
        ${state.phoneBusy === 'root' ? spinner() : ''}${esc(t('workspace.change_root'))}
      </button>`
    : '';

  return `<section class="ws-card ws-connection">
    <div class="ws-card-head">
      <h2 class="ws-card-title">${esc(t('workspace.connection.title'))}</h2>
      <span class="ws-tag ${connected ? 'is-on' : stateInfo.tone === 'warn' ? 'is-warn' : ''}">${esc(stateInfo.label)}</span>
    </div>
    <div class="ws-signal ${connected ? 'is-live' : 'is-idle'}">
      ${connectionNode('pc', pcName, t('workspace.connection.this_pc'), true)}
      <div class="ws-wire" aria-hidden="true"><span></span><span></span><span></span></div>
      ${connectionNode(
        'phone',
        phone.linked ? phoneTitle : t('workspace.connection.phone'),
        phone.linked ? phoneSub : t('workspace.connection.not_linked'),
        connected,
      )}
    </div>
    <div class="ws-kv-list">
      <div class="ws-kv">
        <span class="ws-kv-label">${esc(t('workspace.connection.folder'))}</span>
        <span class="ws-kv-main">${folder}</span>
        ${folderAction}
      </div>
      <div class="ws-kv">
        <span class="ws-kv-label">${esc(t('workspace.connection.address'))}</span>
        <span class="ws-kv-main"><span class="ws-kv-value is-mono">${esc(phone.addr || '—')}</span></span>
      </div>
      <div class="ws-kv">
        <span class="ws-kv-label">${esc(t('workspace.connection.checked'))}</span>
        <span class="ws-kv-main"><span class="ws-kv-value">${esc(checkedText(data))}</span></span>
      </div>
    </div>
    <div class="ws-card-actions">
      ${phone.linked
        ? `<button class="btn btn--quiet btn--small" id="ws-forget" ${state.phoneBusy ? 'disabled' : ''}>${esc(t('workspace.disconnect'))}</button>`
        : `<button class="btn btn--small" id="open-devices">${esc(t('workspace.connect'))}</button>`}
    </div>
  </section>`;
}

function decisionLabel(decision) {
  return decision ? t(`decision.${decision}`) : t('workspace.changes.undecided');
}

function kindLabel(kind) {
  return t(`kind.${kind}`);
}

function decisionBusy(path, action) {
  return state.workspace.action === `decide:${path}:${action}`;
}

function undoButton(path, hint) {
  const busy = Boolean(state.workspace.action) || state.workspace.refreshing;
  return `<button class="btn btn--quiet btn--small" data-undecide="${esc(path)}" title="${esc(hint)}" ${busy ? 'disabled' : ''}>
    ${state.workspace.action === `undecide:${path}` ? spinner() : ''}${esc(t('workspace.decision.undo'))}
  </button>`;
}

function rejectButton(path) {
  const busy = Boolean(state.workspace.action) || state.workspace.refreshing;
  return `<button class="btn btn--quiet btn--small" data-decide="revert" data-path="${esc(path)}" title="${esc(t('workspace.decision.reject_hint'))}" ${busy ? 'disabled' : ''}>
    ${decisionBusy(path, 'revert') ? spinner() : ''}${esc(t('workspace.decision.reject'))}
  </button>`;
}

function decisionButtons(item) {
  const busy = Boolean(state.workspace.action) || state.workspace.refreshing;
  if (item.type === 'addition') {
    const remote = item.change?.side === 'remote';
    return `<button class="btn btn--quiet btn--small" data-decide="accept" data-path="${esc(item.path)}" ${busy ? 'disabled' : ''}>
        ${decisionBusy(item.path, 'accept') ? spinner() : ''}${esc(t('workspace.attention.transfer'))}
      </button>
      <button class="btn btn--quiet btn--small" data-decide="only_here" data-path="${esc(item.path)}" ${busy ? 'disabled' : ''}>
        ${decisionBusy(item.path, 'only_here') ? spinner() : ''}${esc(remote ? t('workspace.attention.keep_phone_only') : t('workspace.attention.keep_pc_only'))}
      </button>
      <button class="btn btn--danger btn--small" data-decide="delete_everywhere" data-kind="addition" data-path="${esc(item.path)}" ${busy ? 'disabled' : ''}>
        ${decisionBusy(item.path, 'delete_everywhere') ? spinner() : ''}${esc(t('workspace.attention.delete_everywhere'))}
      </button>`;
  }
  if (item.type === 'deletion') {
    return `<button class="btn btn--quiet btn--small" data-decide="keep_in_library" data-path="${esc(item.path)}" ${busy ? 'disabled' : ''}>
        ${decisionBusy(item.path, 'keep_in_library') ? spinner() : ''}${esc(t('workspace.attention.keep'))}
      </button>
      <button class="btn btn--danger btn--small" data-decide="delete_everywhere" data-path="${esc(item.path)}" ${busy ? 'disabled' : ''}>
        ${decisionBusy(item.path, 'delete_everywhere') ? spinner() : ''}${esc(t('workspace.attention.delete_everywhere'))}
      </button>`;
  }
  const conflict = item.conflict || {};
  let reason = t('workspace.conflict.both_changed');
  if (conflict.reason === 'changed_vs_deleted') reason = t('workspace.conflict.changed_deleted');
  if (conflict.reason === 'no_base') reason = t('workspace.conflict.no_base');
  return `<span class="ws-decision-reason">${esc(reason)}</span>
    <button class="btn btn--quiet btn--small" data-decide="keep" data-path="${esc(item.path)}" ${busy ? 'disabled' : ''}>
      ${decisionBusy(item.path, 'keep') ? spinner() : ''}${esc(t('workspace.attention.keep_pc'))}
    </button>
    <button class="btn btn--quiet btn--small" data-decide="accept" data-path="${esc(item.path)}" ${busy ? 'disabled' : ''}>
      ${decisionBusy(item.path, 'accept') ? spinner() : ''}${esc(t('workspace.attention.keep_phone'))}
    </button>
    <button class="btn btn--quiet btn--small" data-decide="keep_both" data-path="${esc(item.path)}" ${busy ? 'disabled' : ''}>
      ${decisionBusy(item.path, 'keep_both') ? spinner() : ''}${esc(t('workspace.attention.keep_both'))}
    </button>`;
}

function decisionMeta(item) {
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

function attentionKind(item) {
  if (item.type === 'conflict') return { css: 'is-conflict', label: t('workspace.attention.conflict') };
  if (item.type === 'addition') return { css: 'is-add', label: t('workspace.attention.addition') };
  return { css: 'is-delete', label: t('workspace.attention.deletion') };
}

function attentionRow(item) {
  const kind = attentionKind(item);
  const meta = item.decided
    ? `${decisionMeta(item)} · ${decisionLabel(item.decision)}`
    : decisionMeta(item);
  return `<div class="ws-decision${item.decided ? ' is-decided' : ''}">
    <div class="ws-decision-main">
      <span class="ws-kind ${kind.css}">${esc(kind.label)}</span>
      <span class="ws-decision-path" title="${esc(item.path)}">${esc(item.path)}</span>
      <span class="ws-decision-meta">${esc(meta)}</span>
    </div>
    <div class="ws-decision-actions">${item.decided
      ? undoButton(item.path, t('workspace.decision.undo_hint'))
      : decisionButtons(item)}</div>
  </div>`;
}

function attentionCard(data) {
  const items = decisionItems(data);
  const pending = items.filter((item) => !item.decided).length;
  const manual = Boolean(data && data.manual_additions);
  return `<section class="ws-card ws-attention">
    <div class="ws-card-head">
      <h2 class="ws-card-title">${esc(t('workspace.attention.title'))}</h2>
      <span class="ws-tag ${pending ? 'is-warn' : 'is-on'}">${esc(pending
        ? t('workspace.attention.count', { count: pending })
        : items.length ? t('workspace.attention.done') : t('workspace.attention.none'))}</span>
    </div>
    ${items.length
      ? `<div class="ws-decisions">${items.map(attentionRow).join('')}</div>`
      : `<div class="ws-calm">
          <span class="ws-calm-icon">${iconCheck()}</span>
          <div>
            <b>${esc(t('workspace.attention.empty_title'))}</b>
            <p>${esc(manual ? t('workspace.attention.empty_manual') : t('workspace.attention.empty_text'))}</p>
          </div>
        </div>`}
  </section>`;
}

function overviewView(data) {
  if (!data) return workspaceEmptyView();
  return `${statsView(data)}
    <div class="ws-grid">
      ${connectionCard(data)}
      ${attentionCard(data)}
    </div>`;
}

function changeMeta(change, manual = false) {
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

function changeRow(change, manual = false) {
  const addition = manual && change.kind === 'added' && !change.aligned && !change.decided;
  const pending = (change.kind === 'deleted' || (manual && change.kind === 'added'))
    && !change.decided
    && !change.aligned;
  const auto = !change.aligned && !change.decided && !pending;
  let actions = '';
  if (pending) {
    actions = `<div class="ws-row-actions">${decisionButtons({
      type: addition ? 'addition' : 'deletion',
      path: change.path,
      change,
      size: change.size,
    })}</div>`;
  } else if (change.decided) {
    actions = `<div class="ws-row-actions">${undoButton(change.path, t('workspace.decision.undo_hint'))}</div>`;
  } else if (auto) {
    // Файл уже принят автоматически. Отмена — это явный отказ, после
    // которого решение снова можно снять обычной кнопкой.
    actions = `<div class="ws-row-actions">${rejectButton(change.path)}</div>`;
  }
  return `<div class="ws-row">
    <span class="ws-row-kind is-${esc(change.kind)}">${esc(kindLabel(change.kind))}</span>
    <div class="ws-row-main">
      <div class="ws-row-path" title="${esc(change.path)}">${esc(change.path)}</div>
      <div class="ws-row-meta">${esc(changeMeta(change, manual))}</div>
    </div>
    ${actions}
  </div>`;
}

function changeGroup(title, rows, count) {
  if (!rows.length) return '';
  return `<section class="ws-card ws-group">
    <div class="ws-card-head">
      <h2 class="ws-card-title">${esc(title)}</h2>
      <span class="ws-tag">${number(count)}</span>
    </div>
    <div class="ws-rows">${rows.join('')}</div>
  </section>`;
}

function changesView(data) {
  if (!data) return workspaceEmptyView();
  const plan = planData(data);
  if (!plan) {
    return `<section class="ws-card ws-empty-card">
      <span class="ws-empty-icon">${iconList()}</span>
      <b>${esc(t('workspace.changes.empty_title'))}</b>
      <p>${esc(t('workspace.changes.empty_text'))}</p>
      <button class="btn btn--small" id="ws-refresh">${esc(t('workspace.check'))}</button>
    </section>`;
  }

  const changes = (plan.changes || []).filter((change) => !change.aligned);
  const manual = Boolean(data.manual_additions);
  const aligned = Number(plan.aligned) || 0;
  const local = changes.filter((change) => change.side === 'local');
  const remote = changes.filter((change) => change.side === 'remote');
  const conflicts = plan.conflicts || [];
  const hasAny = local.length || remote.length || conflicts.length;

  if (!hasAny) {
    return `<section class="ws-card ws-empty-card">
      <span class="ws-empty-icon is-calm">${iconCheck()}</span>
      <b>${esc(t('workspace.changes.empty_title'))}</b>
      <p>${esc(aligned > 0
        ? t('workspace.changes.aligned_text', { count: aligned })
        : t('workspace.changes.empty_text'))}</p>
      <button class="btn btn--small" id="ws-refresh">${esc(t('workspace.check'))}</button>
    </section>`;
  }

  const conflictRows = conflicts.map((conflict) => {
    const pending = Boolean(conflict.unresolved);
    return `<div class="ws-row">
      <span class="ws-row-kind is-conflict">${esc(kindLabel('conflict'))}</span>
      <div class="ws-row-main">
        <div class="ws-row-path" title="${esc(conflict.path)}">${esc(conflict.path)}</div>
        <div class="ws-row-meta">${esc(conflict.unresolved ? t('workspace.changes.undecided') : decisionLabel(conflict.decision))}</div>
      </div>
      ${pending
        ? `<div class="ws-row-actions">${decisionButtons({ type: 'conflict', path: conflict.path, conflict, size: 0 })}</div>`
        : `<div class="ws-row-actions">${undoButton(conflict.path, t('workspace.decision.undo_hint'))}</div>`}
    </div>`;
  });

  return `<div class="ws-groups">
    ${aligned > 0
      ? `<p class="ws-aligned-note">${esc(t('workspace.changes.aligned_note', { count: aligned }))}</p>`
      : ''}
    ${changeGroup(t('workspace.changes.local'), local.map((change) => changeRow(change, manual)), local.length)}
    ${changeGroup(t('workspace.changes.remote'), remote.map((change) => changeRow(change, manual)), remote.length)}
    ${changeGroup(t('workspace.changes.conflicts'), conflictRows, conflicts.length)}
  </div>`;
}

function vaultView(data) {
  if (!data) return workspaceEmptyView();
  const rows = data.vault || [];
  if (!rows.length) {
    return `<section class="ws-card ws-empty-card">
      <span class="ws-empty-icon is-calm">${iconVault()}</span>
      <b>${esc(t('workspace.vault.empty_title'))}</b>
      <p>${esc(t('workspace.vault.empty_text'))}</p>
    </section>`;
  }

  return `<section class="ws-card">
    <div class="ws-card-head">
      <h2 class="ws-card-title">${esc(t('workspace.vault.panel'))}</h2>
      <span class="ws-tag">${esc(t('workspace.vault.count', { count: rows.length }))}</span>
    </div>
    <div class="ws-rows">
      ${rows.map((row) => `<div class="ws-row">
        <span class="ws-row-kind is-vault">${esc(formatSize(row.size))}</span>
        <div class="ws-row-main">
          <div class="ws-row-path" title="${esc(row.path)}">${esc(row.path)}</div>
          <div class="ws-row-meta">${esc(t('workspace.vault.deleted', { ago: ago(row.deleted_at) }))}</div>
        </div>
        <div class="ws-row-actions">
          <button class="btn btn--quiet btn--small" data-restore="${esc(row.path)}" ${state.workspace.action ? 'disabled' : ''}>
            ${state.workspace.action === `restore:${row.path}` ? spinner() : ''}${esc(t('workspace.vault.restore'))}
          </button>
        </div>
      </div>`).join('')}
    </div>
  </section>`;
}

function workspaceEmptyView() {
  if (state.workspace.refreshing) {
    return `<section class="ws-card ws-empty-card ws-loading">
      ${spinner()}
      <b>${esc(t('workspace.loading.title'))}</b>
      <p>${esc(t('workspace.loading.text'))}</p>
    </section>`;
  }
  if (state.workspace.error) {
    return `<section class="ws-card ws-empty-card">
      <b>${esc(t('workspace.status.error'))}</b>
      <p>${esc(state.workspace.error)}</p>
      <button class="btn btn--small" id="ws-refresh">${esc(t('workspace.retry'))}</button>
    </section>`;
  }
  return `<section class="ws-card ws-empty-card">
    <span class="ws-empty-icon">${iconOverview()}</span>
    <b>${esc(t('workspace.loading.title'))}</b>
    <p>${esc(t('workspace.loading.text'))}</p>
  </section>`;
}

function workspaceBody(data) {
  const tab = state.workspace.tab;
  if (tab === 'changes') return changesView(data);
  if (tab === 'vault') return vaultView(data);
  return overviewView(data);
}

function workspaceView() {
  const data = workspaceData();
  return `<section class="workspace">
    ${workspaceRail(data)}
    <div class="workspace-main">
      ${workspaceHead(data, state.workspace.tab)}
      ${workspaceNotice(data)}
      <div class="workspace-body">${workspaceBody(data)}</div>
    </div>
  </section>`;
}

// ------------------------------------------------------------------ модальное окно

function modalView() {
  const modal = state.modal;
  if (!modal) return '';
  return `<div class="modal-backdrop" data-modal="cancel"></div>
    <div class="modal" role="dialog" aria-modal="true" aria-labelledby="modal-title">
      <span class="modal-kicker">${esc(modal.kicker || '')}</span>
      <h2 class="modal-title" id="modal-title">${esc(modal.title)}</h2>
      <p class="modal-text">${esc(modal.text || '')}</p>
      ${modal.path ? `<div class="modal-path">${esc(modal.path)}</div>` : ''}
      <div class="modal-actions">
        <button class="btn btn--quiet" data-modal="cancel">${esc(t('workspace.cancel'))}</button>
        <button class="btn ${modal.danger ? 'btn--danger' : ''}" data-modal="confirm">${esc(modal.label || t('workspace.confirm'))}</button>
      </div>
    </div>`;
}

function openModal(options) {
  state.modal = options;
  renderModal();
}

function closeModal() {
  state.modal = null;
  renderModal();
}

function renderModal() {
  const host = document.getElementById('modal-layer');
  if (!host) return;
  const html = modalView();
  host.hidden = !state.modal;
  if (host.innerHTML !== html) host.innerHTML = html;
  if (state.modal) {
    const confirm = host.querySelector('[data-modal="confirm"]');
    if (confirm) confirm.focus();
  }
}

function markEnter(host) {
  host.classList.remove('view-enter');
  void host.offsetWidth;
  host.classList.add('view-enter');
}

function render() {
  const screen = document.getElementById('screen');
  if (!screen) return;
  const viewChanged = state.view !== painted.view;
  let html = '';
  if (state.view === 'language') html = languageView();
  else if (state.view === 'devices') html = devicesView();
  else if (state.view === 'library') html = libraryView();
  else if (state.view === 'workspace') html = workspaceView();
  else if (state.view === 'error') html = `<div class="fatal">${esc(state.note)}</div>`;
  if (html !== painted.screen) {
    const scrollTop = screen.scrollTop;
    screen.innerHTML = html;
    painted.screen = html;
    screen.scrollTop = viewChanged ? 0 : scrollTop;
  } else if (viewChanged) {
    screen.scrollTop = 0;
  }
  if (viewChanged) markEnter(screen);
  painted.view = state.view;
  renderModal();

  document.querySelectorAll('[data-window]').forEach((button) => {
    const title = t(`window.${button.dataset.window}`);
    button.setAttribute('aria-label', title);
    button.setAttribute('title', title);
  });
}

// Сбрасывает рабочую область и делает ответы уже начатых запросов
// недействительными: они относятся к прошлому устройству или прошлой папке.
function resetWorkspace() {
  workspaceGen += 1;
  workspacePendingRefresh = false;
  state.workspace.data = null;
  state.workspace.error = '';
  state.workspace.statusError = '';
}

async function applyLink(result) {
  state.phone = {
    linked: true,
    connected: true,
    dev: result.dev,
    name: result.name,
    addr: result.addr,
    root: result.root,
  };
  state.note = '';
  state.error = false;
  resetWorkspace();
  state.workspace.tab = 'overview';
  state.view = state.root ? 'workspace' : 'library';
  if (result.needs_root) {
    state.note = t('library.waiting_phone');
  }
  render();
  if (result.needs_root) {
    await requestRootChange(true);
  } else if (state.view === 'workspace') {
    await refreshWorkspace();
  }
}

async function connectLan(dev, addr) {
  if (state.busy || !invoke) return;
  state.busy = `lan:${dev}`;
  state.error = false;
  state.note = t('setup.wait_pair');
  render();
  try {
    const result = await invoke('agent_connect', { addr, dev });
    state.busy = '';
    await applyLink(result);
  } catch (error) {
    state.busy = '';
    state.error = true;
    state.note = t('phone.error', { error: message(error) });
    render();
  }
}

async function connectAdb(serial) {
  if (state.busy || !invoke) return;
  state.busy = `adb:${serial}`;
  state.error = false;
  state.note = t('setup.wait_pair');
  render();
  try {
    const result = await invoke('agent_connect_adb', { serial });
    state.busy = '';
    await applyLink(result);
  } catch (error) {
    state.busy = '';
    state.error = true;
    state.note = t('phone.error', { error: message(error) });
    render();
  }
}

async function discoverAll(keepNote = false) {
  if (state.discovering || !invoke) return;
  state.discovering = true;
  if (!keepNote) {
    state.error = false;
    state.note = '';
  }
  render();
  const [lan, adb] = await Promise.allSettled([
    invoke('discover_agents'),
    invoke('discover_adb'),
  ]);
  state.lan = lan.status === 'fulfilled' ? (lan.value?.devices || []) : [];
  state.adb = adb.status === 'fulfilled' ? (adb.value?.devices || []) : [];
  state.adbError = adb.status === 'rejected' ? t('setup.adb_error', { error: message(adb.reason) }) : '';
  state.discovering = false;
  render();
}

async function chooseFolder() {
  if (state.busy || !invoke) return;
  state.error = false;
  state.note = '';

  let folder;
  try {
    folder = await invoke('pick_folder', { title: t('library.dialog') });
  } catch (error) {
    state.error = true;
    state.note = t('error.init', { error: message(error) });
    render();
    return;
  }
  if (!folder) return;

  state.busy = 'folder';
  render();
  let opened = false;
  try {
    const result = await invoke('initialize_library', {
      root: folder,
      dataDir: state.info?.config?.data_dir || null,
    });
    state.root = result?.root || folder;
    state.note = '';
    opened = true;
  } catch (error) {
    state.error = true;
    state.note = t('error.init', { error: message(error) });
  } finally {
    state.busy = '';
    render();
  }
  if (!opened) return;

  await closeWorkspace();
  // Сначала локальная папка, потом телефон: после выбора папки показываем
  // устройства. Если телефон уже был сопряжён, пробуем вернуться к нему.
  if (state.phone.linked) {
    state.view = 'workspace';
    render();
    await reconnect();
  } else {
    state.view = 'devices';
    render();
    await discoverAll();
  }
}

async function closeWorkspace() {
  resetWorkspace();
  state.workspace.note = '';
  state.workspace.action = '';
  if (invoke) {
    try {
      await invoke('workspace_close');
    } catch {
      // Сеанс мог быть уже закрыт вместе с телефоном.
    }
  }
}

function openLibrary() {
  if (state.busy) return;
  closeWorkspace();
  state.view = 'library';
  state.error = false;
  state.note = '';
  render();
}

async function openDevices() {
  if (state.busy) return;
  await closeWorkspace();
  state.view = 'devices';
  state.error = false;
  state.note = '';
  render();
  await discoverAll();
}

async function openWorkspace() {
  if (state.busy) return;
  state.view = 'workspace';
  state.error = false;
  state.note = '';
  state.workspace.tab = 'overview';
  render();
  await refreshWorkspace();
}

async function requestRootChange(quiet = false) {
  if (state.phoneBusy || !invoke) return;
  state.phoneBusy = 'root';
  if (!quiet) state.note = t('phone.wait_root');
  render();
  try {
    const result = await invoke('agent_request_root_change');
    if (state.phone.linked) state.phone.root = result?.root || state.phone.root;
    state.note = '';
    state.error = false;
    await closeWorkspace();
    if (state.view === 'workspace') await refreshWorkspace();
  } catch (error) {
    state.error = true;
    state.note = t('phone.error', { error: message(error) });
  } finally {
    state.phoneBusy = '';
    render();
  }
}

async function refreshWorkspace(options = {}) {
  if (!invoke) return;
  if (state.workspace.refreshing) {
    // Перезапустим после текущего запроса, но только с данными нового
    // поколения: старый ответ не должен победить по времени прихода.
    workspacePendingRefresh = true;
    return;
  }
  if (!options.keepNote) {
    state.workspace.note = '';
    state.workspace.noteError = false;
  }
  state.workspace.refreshing = true;
  state.workspace.error = '';
  state.workspace.statusError = '';
  const gen = ++workspaceGen;
  render();
  try {
    const data = await invoke('workspace_refresh');
    if (gen === workspaceGen) state.workspace.data = data;
  } catch (error) {
    if (gen === workspaceGen) state.workspace.error = message(error);
  } finally {
    state.workspace.refreshing = false;
    if (workspacePendingRefresh) {
      workspacePendingRefresh = false;
      render();
      refreshWorkspace(options);
      return;
    }
    render();
  }
}

async function pollWorkspace() {
  if (!invoke || state.view !== 'workspace') return;
  if (!state.workspace.data || state.workspace.refreshing || state.workspace.action || state.modal) return;
  const gen = ++workspaceGen;
  try {
    const data = await invoke('workspace_status');
    if (gen !== workspaceGen) return;
    state.workspace.data = data;
    state.workspace.statusError = '';
    render();
  } catch (error) {
    if (gen !== workspaceGen) return;
    state.workspace.statusError = message(error);
    render();
  }
}

async function decide(path, action) {
  if (!invoke || state.workspace.action || state.workspace.refreshing) return;
  const gen = ++workspaceGen;
  state.workspace.action = `decide:${path}:${action}`;
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  render();
  try {
    const data = await invoke('workspace_decide', { path, action });
    if (gen === workspaceGen) state.workspace.data = data;
  } catch (error) {
    state.workspace.note = t('workspace.error.decision', { error: message(error) });
    state.workspace.noteError = true;
  } finally {
    state.workspace.action = '';
    render();
  }
}

async function undecide(path) {
  if (!invoke || state.workspace.action || state.workspace.refreshing) return;
  const gen = ++workspaceGen;
  state.workspace.action = `undecide:${path}`;
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  render();
  try {
    const data = await invoke('workspace_undecide', { path });
    if (gen === workspaceGen) state.workspace.data = data;
  } catch (error) {
    state.workspace.note = t('workspace.error.decision', { error: message(error) });
    state.workspace.noteError = true;
  } finally {
    state.workspace.action = '';
    render();
  }
}

function askDelete(item) {
  openModal({
    kicker: attentionKind({ type: item.type || 'deletion' }).label,
    title: t('workspace.modal.delete_title'),
    text: t('workspace.modal.delete_text'),
    path: item.path,
    label: t('workspace.attention.delete_everywhere'),
    danger: true,
    onConfirm: () => decide(item.path, 'delete_everywhere'),
  });
}

function askForget() {
  openModal({
    title: t('workspace.modal.forget_title'),
    text: t('workspace.modal.forget_text'),
    label: t('workspace.disconnect'),
    danger: true,
    onConfirm: () => forgetPhone(),
  });
}

function askGc() {
  openModal({
    kicker: t('workspace.tab.vault'),
    title: t('workspace.modal.gc_title'),
    text: t('workspace.modal.gc_text'),
    label: t('workspace.vault.gc'),
    danger: true,
    onConfirm: () => gcVault(),
  });
}

async function commitWorkspace() {
  if (!invoke || !canCommit()) return;
  ++workspaceGen;
  state.workspace.action = 'commit';
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  render();
  let outcome = null;
  try {
    outcome = await invoke('workspace_commit');
  } catch (error) {
    state.workspace.action = '';
    state.workspace.note = t('workspace.error.commit', { error: message(error) });
    state.workspace.noteError = true;
    render();
    return;
  }
  state.workspace.action = '';
  state.workspace.data = null;
  state.workspace.note = t('workspace.commit_done', {
    accepted: number(outcome?.accepted || 0),
    rejected: number(outcome?.rejected || 0),
  });
  state.workspace.noteError = false;
  state.workspace.tab = 'overview';
  render();
  await refreshWorkspace({ keepNote: true });
}

async function restoreVault(path) {
  if (!invoke || state.workspace.action) return;
  state.workspace.action = `restore:${path}`;
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  render();
  try {
    await invoke('workspace_restore', { path });
    state.workspace.note = t('workspace.restore_done', { path });
    state.workspace.noteError = false;
  } catch (error) {
    state.workspace.note = t('workspace.error.restore', { error: message(error) });
    state.workspace.noteError = true;
  } finally {
    state.workspace.action = '';
  }
  render();
  await refreshWorkspace({ keepNote: true });
}

async function gcVault() {
  if (!invoke || state.workspace.action) return;
  state.workspace.action = 'gc';
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  render();
  const gen = ++workspaceGen;
  try {
    const result = await invoke('workspace_gc', { ttlDays: 30 });
    state.workspace.note = t('workspace.gc_done', {
      rows: number(result?.rows || 0),
      blobs: number(result?.blobs || 0),
    });
    state.workspace.noteError = false;
    const data = await invoke('workspace_status');
    if (gen === workspaceGen) state.workspace.data = data;
  } catch (error) {
    state.workspace.note = t('workspace.error.gc', { error: message(error) });
    state.workspace.noteError = true;
  } finally {
    state.workspace.action = '';
    render();
  }
}

async function toggleManual() {
  if (!invoke || state.workspace.action || state.workspace.refreshing) return;
  const enabled = !(state.workspace.data && state.workspace.data.manual_additions);
  const gen = ++workspaceGen;
  state.workspace.action = 'manual';
  state.workspace.note = '';
  state.workspace.noteError = false;
  render();
  try {
    await invoke('workspace_set_manual', { enabled });
    if (gen !== workspaceGen) return;
    if (state.workspace.data) state.workspace.data.manual_additions = enabled;
    state.workspace.note = t(enabled ? 'workspace.manual.on' : 'workspace.manual.off');
  } catch (error) {
    state.workspace.note = t('workspace.error.manual', { error: message(error) });
    state.workspace.noteError = true;
  } finally {
    state.workspace.action = '';
    render();
  }
}

async function scanPhone() {
  await refreshWorkspace();
}

async function forgetPhone() {
  if (state.phoneBusy || !invoke) return;
  state.phoneBusy = 'forget';
  state.note = '';
  state.error = false;
  try {
    await invoke('agent_forget');
    await closeWorkspace();
    state.phone = { linked: false };
    state.view = 'devices';
    render();
    await discoverAll();
  } catch (error) {
    state.error = true;
    state.note = t('phone.error', { error: message(error) });
  } finally {
    state.phoneBusy = '';
    render();
  }
}

async function reconnect() {
  if (!invoke || state.busy || !state.phone.linked) return;
  state.busy = 'reconnect';
  state.error = false;
  state.note = t('setup.connecting');
  render();
  try {
    const result = await invoke('agent_reconnect');
    state.busy = '';
    await applyLink(result);
  } catch (error) {
    state.busy = '';
    state.error = true;
    state.note = t('phone.error', { error: message(error) });
    state.phone = { linked: false };
    state.view = 'devices';
    render();
    await discoverAll(true);
  }
}

async function pollStatus() {
  if (!invoke || !state.phone.linked) return;
  if (state.busy || state.phoneBusy) return;
  if (state.view !== 'workspace' && state.view !== 'library') return;

  let status;
  try {
    status = await invoke('agent_status');
  } catch (error) {
    // Статус опрашивается постоянно: без этого сообщения устойчивый сбой
    // бэкенда выглядел бы как «замерший» интерфейс без причины.
    state.workspace.statusError = message(error);
    render();
    return;
  }
  if (!status || status.busy) return;

  if (!status.connected) {
    state.phone = { linked: false };
    resetWorkspace();
    state.view = 'devices';
    state.error = true;
    state.note = t('phone.lost');
    render();
    await closeWorkspace();
    await discoverAll(true);
    return;
  }

  let changed = false;
  if (status.root !== undefined && status.root !== state.phone.root) {
    state.phone.root = status.root;
    resetWorkspace();
    changed = true;
    if (state.view === 'workspace') refreshWorkspace();
  }
  if (status.name && status.name !== state.phone.name) {
    state.phone.name = status.name;
    changed = true;
  }
  if (changed) render();
}

async function cancelConnect() {
  if (!invoke) return;
  try {
    await invoke('agent_cancel');
  } catch {
    // Соединение могло уже закрыться само.
  }
}

async function windowAction(action) {
  if (!invoke) return;
  const command = {
    minimize: 'window_minimize',
    maximize: 'window_toggle_maximize',
    close: 'window_close',
  }[action];
  if (!command) return;
  try {
    await invoke(command);
  } catch {
    // Окно могло уже закрыться; отдельного сообщения не нужно.
  }
}

async function boot() {
  window.I18N.apply();

  // Самый первый экран: язык. Пока выбор не сохранён, даже не трогаем
  // библиотеку и телефон — человек должен понимать, что происходит.
  if (!window.I18N.chosen) {
    state.view = 'language';
    render();
    return;
  }

  if (!invoke) {
    state.view = 'error';
    state.note = t('error.bridge');
    render();
    return;
  }

  try {
    state.info = await invoke('app_info');
    state.root = state.info?.config?.root || null;
    const link = await invoke('agent_link');
    state.phone = link?.linked ? link : { linked: false };

    if (!state.root) state.view = 'library';
    else if (!state.phone.linked) state.view = 'devices';
    else state.view = 'workspace';
  } catch (error) {
    state.view = 'error';
    state.note = message(error);
  }
  render();
  if (state.view === 'devices') await discoverAll();
  if (state.view === 'workspace') {
    await reconnect();
    if (state.view === 'workspace') await refreshWorkspace();
  }
  setInterval(pollStatus, 2500);
  setInterval(pollWorkspace, 3200);
}

document.addEventListener('click', (event) => {
  const langButton = event.target.closest('[data-lang]');
  if (langButton && !langButton.disabled) {
    langButton.disabled = true;
    window.I18N.setLang(langButton.dataset.lang);
    boot();
    return;
  }

  if (event.target.closest('[data-modal="cancel"]')) {
    closeModal();
    return;
  }

  if (event.target.closest('[data-modal="confirm"]')) {
    const modal = state.modal;
    closeModal();
    if (modal?.onConfirm) modal.onConfirm();
    return;
  }

  if (event.target.closest('#refresh')) {
    discoverAll();
    return;
  }

  if (event.target.closest('[data-cancel]')) {
    cancelConnect();
    return;
  }

  if (event.target.closest('#choose')) {
    chooseFolder();
    return;
  }

  const lan = event.target.closest('[data-connect-lan]');
  if (lan && !lan.disabled) {
    connectLan(lan.dataset.dev, lan.dataset.addr);
    return;
  }

  const adb = event.target.closest('[data-connect-adb]');
  if (adb && !adb.disabled) {
    connectAdb(adb.dataset.serial);
    return;
  }

  const tab = event.target.closest('[data-ws-tab]');
  if (tab) {
    if (state.workspace.tab !== tab.dataset.wsTab) {
      state.workspace.tab = tab.dataset.wsTab;
      render();
    }
    return;
  }

  if (event.target.closest('#ws-refresh')) {
    refreshWorkspace();
    return;
  }

  if (event.target.closest('#ws-manual')) {
    toggleManual();
    return;
  }

  if (event.target.closest('#ws-commit')) {
    commitWorkspace();
    return;
  }

  if (event.target.closest('#ws-change-root')) {
    requestRootChange();
    return;
  }

  if (event.target.closest('#ws-forget')) {
    askForget();
    return;
  }

  if (event.target.closest('#ws-gc')) {
    askGc();
    return;
  }

  const undecideButton = event.target.closest('[data-undecide]');
  if (undecideButton && !undecideButton.disabled) {
    undecide(undecideButton.dataset.undecide);
    return;
  }

  const decideButton = event.target.closest('[data-decide]');
  if (decideButton && !decideButton.disabled) {
    const action = decideButton.dataset.decide;
    const path = decideButton.dataset.path;
    if (action === 'delete_everywhere') {
      askDelete({ path, type: decideButton.dataset.kind || 'deletion' });
    } else {
      decide(path, action);
    }
    return;
  }

  const restore = event.target.closest('[data-restore]');
  if (restore && !restore.disabled) {
    restoreVault(restore.dataset.restore);
    return;
  }

  if (event.target.closest('#open-library')) {
    openLibrary();
    return;
  }

  if (event.target.closest('#open-workspace')) {
    openWorkspace();
    return;
  }

  if (event.target.closest('#open-devices')) {
    openDevices();
    return;
  }

  const windowButton = event.target.closest('[data-window]');
  if (windowButton) windowAction(windowButton.dataset.window);
});

document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape' && state.modal) closeModal();
});

window.__syncth = {
  state,
  boot,
  render,
  discoverAll,
  connectLan,
  connectAdb,
  reconnect,
  pollStatus,
  pollWorkspace,
  cancelConnect,
  chooseFolder,
  openLibrary,
  openDevices,
  openWorkspace,
  requestRootChange,
  refreshWorkspace,
  decide,
  commitWorkspace,
  restoreVault,
  gcVault,
  closeWorkspace,
  scanPhone,
  forgetPhone,
  closeModal,
};

boot().catch((error) => {
  state.view = 'error';
  state.note = message(error);
  render();
});
