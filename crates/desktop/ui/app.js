'use strict';

// Ошибку загрузки интерфейса видно без консоли: это десктоп, а не вкладка.
window.addEventListener('error', (event) => {
  const view = document.getElementById('view');
  if (view && !view.dataset.errorShown) {
    view.dataset.errorShown = '1';
    view.innerHTML = `<section class="panel"><div class="empty"><b>${I18N.t('err.interface')}</b><p>${String(event.message || event.error || '')}</p></div></section>`;
  }
});

// ------------------------------------------------------------------ основа

const tauri = window.__TAURI__;

const { t, tp, plural, num, ago, size: sizeParts } = I18N;

// Мост ищем в момент вызова: это позволяет открыть интерфейс в браузере
// с подставным мостом для проверки вёрстки.
function invoke(command, args) {
  const bridge = window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke;
  if (!bridge) return Promise.reject(new Error('Tauri bridge unavailable'));
  return bridge(command, args);
}

const hasBridge = () => !!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke);

const $ = (selector, root = document) => root.querySelector(selector);

const state = {
  info: null,
  running: false,
  status: null,
  lastDevice: null,
  lastError: null,
  view: 'overview',
  busy: false,
  pair: null,
  pairError: null,
  pairLoading: false,
  log: null,
  logLoading: false,
  onboarding: { root: null, dataDir: null },
  autoStartTried: false,
  started: false,
  modal: null,
  keepScroll: false,
  lastHtml: '',
  lastView: '',
};

const WEEKDAYS = ['вс', 'пн', 'вт', 'ср', 'чт', 'пт', 'сб'];

// ------------------------------------------------------------------ утилиты

const esc = (value) =>
  String(value == null ? '' : value).replace(/[&<>"']/g, (ch) => ({
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#39;',
  }[ch]));

const icon = (name, cls = '') => `<svg class="${cls}" aria-hidden="true"><use href="#i-${name}"></use></svg>`;

const sizeText = (bytes) => sizeParts(bytes).text;

const basename = (path) => {
  const parts = String(path || '').split(/[\\/]/).filter(Boolean);
  return parts.length ? parts[parts.length - 1] : String(path || '');
};

const dirname = (path) => {
  const value = String(path || '');
  const index = Math.max(value.lastIndexOf('\\'), value.lastIndexOf('/'));
  return index > 0 ? value.slice(0, index) : value;
};

const joinPath = (root, relative) => {
  if (!root) return relative;
  const base = String(root).replace(/[\\/]+$/, '');
  const tail = String(relative).replace(/^[\\/]+/, '');
  return `${base}\\${tail}`;
};

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// В лог службы попадают ANSI-цвета из tracing; в интерфейсе они не нужны.
const stripAnsi = (text) => String(text || '').replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, '');

function errText(error) {
  if (!error) return t('common.unknownError');
  if (typeof error === 'string') return error;
  return error.message || String(error);
}

async function copyText(text) {
  if (!text) return;
  try {
    await navigator.clipboard.writeText(text);
    toast(t('common.copied'));
  } catch {
    const area = document.createElement('textarea');
    area.value = text;
    area.style.position = 'fixed';
    area.style.opacity = '0';
    document.body.appendChild(area);
    area.select();
    try {
      document.execCommand('copy');
      toast(t('common.copied'));
    } catch {
      toast(t('common.copyFailed'), 'bad');
    }
    area.remove();
  }
}

// ------------------------------------------------------------------ снимок состояния

function snapshot() {
  const info = state.info || {};
  const cfg = info.config || {};
  const status = state.running ? state.status : null;
  const array = (value) => (Array.isArray(value) ? value : []);
  return {
    info,
    cfg,
    status,
    running: !!state.running,
    root: (status && status.root) || cfg.root || null,
    pending: array(status && status.pending),
    conflicts: array(status && status.conflicts),
    vault: array(status && status.vault),
    ignored: array(status && status.ignored),
    peers: array(status && status.peers),
    known: array(status && status.known_peers),
    device: status ? status.device : state.lastDevice,
    daemonBin: info.daemon_bin || null,
  };
}

function stateCopy(snap) {
  const status = snap.status;
  if (!snap.running) {
    return {
      tone: 'idle',
      disc: 'OFF',
      discSub: t('state.service'),
      cap: t('state.notRunning'),
      title: snap.cfg.root ? t('state.stopped.title') : t('state.noRoot.title'),
      sub: snap.cfg.root ? t('state.stopped.sub') : t('state.noRoot.sub'),
    };
  }

  const pending = snap.pending.length;
  const pendingNote = pending ? tp('state.pendingNote', pending) : '';

  switch (status.state) {
    case 'syncing':
      return {
        tone: 'busy',
        disc: 'SYNC',
        discSub: t('state.syncing.disc'),
        cap: t('state.syncing.cap'),
        title: t('state.syncing.title'),
        sub: t('state.syncing.sub'),
      };
    case 'paused_with_warning':
      return {
        tone: 'bad',
        disc: '!',
        discSub: t('state.lowDisk.disc'),
        cap: t('state.lowDisk.cap'),
        title: t('state.lowDisk.title'),
        sub: t('state.lowDisk.sub'),
      };
    case 'paused':
      return {
        tone: 'idle',
        disc: 'PAUSE',
        discSub: t('state.paused.disc'),
        cap: t('state.paused.cap'),
        title: t('state.paused.title'),
        sub: pending ? `${t('state.paused.sub')} ${pendingNote}` : t('state.paused.sub'),
      };
    case 'error':
      return {
        tone: 'bad',
        disc: 'ERR',
        discSub: t('state.error.disc'),
        cap: t('state.error.cap'),
        title: t('state.error.title'),
        sub: status.last_error && I18N.locale === 'ru' ? String(status.last_error) : t('state.error.sub'),
      };
    default:
      return {
        tone: 'good',
        disc: 'OK',
        discSub: t('state.ok.disc'),
        cap: t('state.ok.cap'),
        title: t('state.ok.title'),
        sub: status.last_sync
          ? `${t('state.ok.subSync', { ago: ago(status.last_sync) })}${pending ? ` ${pendingNote}` : ''}`
          : `${t('state.ok.sub')}${pending ? ` ${pendingNote}` : ''}`,
      };
  }
}

// ------------------------------------------------------------------ каркас

function navHtml(snap) {
  const items = [
    { id: 'overview', icon: 'disc' },
    { id: 'pending', icon: 'clock', count: snap.pending.length },
    { id: 'conflicts', icon: 'split', count: snap.conflicts.length },
    { id: 'vault', icon: 'box', count: snap.vault.length },
    { id: 'devices', icon: 'devices', count: snap.known.length },
    { id: 'pair', icon: 'qr' },
    { id: 'settings', icon: 'sliders' },
  ];

  return items.map((item) => {
    const active = state.view === item.id ? ' active' : '';
    const count = item.count
      ? `<span class="nav-count${item.id === 'pending' || item.id === 'conflicts' ? ' hot' : ''}">${item.count}</span>`
      : '';
    return `<button class="nav-item${active}" data-view="${item.id}">
      ${icon(item.icon, 'nav-ico')}<span class="nav-label">${esc(t(`nav.${item.id}`))}</span>${count}
    </button>`;
  }).join('');
}

function chrome(snap, copy) {
  const info = state.info;
  const onboard = !info.config.root;
  const daemonTone = state.busy
    ? 'busy'
    : state.running
      ? (snap.status && (snap.status.state === 'syncing') ? 'busy' : 'on')
      : (snap.cfg.root ? 'off' : 'off');
  document.body.dataset.daemon = daemonTone;
  document.body.classList.toggle('is-onboarding', onboard);

  const stateText = onboard
    ? t('titlebar.firstRun')
    : !state.running
      ? t('titlebar.serviceStopped')
      : snap.status && snap.status.state === 'syncing'
        ? t('titlebar.syncing')
        : snap.status && snap.status.state === 'paused'
          ? t('titlebar.paused')
          : snap.status && snap.status.state === 'error'
            ? t('titlebar.error')
            : t('titlebar.online');

  $('#tb-ver').textContent = info ? `v${info.version}` : '';
  $('#tb-state').textContent = stateText;
  $('#tb-dev').textContent = snap.device || '';

  $('#rail-dev').textContent = snap.device || t('common.dash');
  $('#rail-dev').title = snap.device || '';
  $('#rail-root').textContent = snap.root ? basename(snap.root) : t('rail.noLibrary');
  $('#rail-root').title = snap.root || '';
  $('#rail-daemon').textContent = state.running ? t('rail.serviceRunning') : t('rail.serviceStopped');
  $('#rail-locale').textContent = I18N.locale === 'ru' ? 'Русский' : 'English';
  $('#rail-ver').textContent = info ? `v${info.version} · AGPL-3.0` : '';
  $('#nav').innerHTML = onboard ? '' : navHtml(snap);
  $('#progress').hidden = !state.busy;
}

function viewHead(kicker, title, sub, actions = '') {
  return `<div class="view-head">
    <div>
      <div class="eyebrow">${esc(kicker)}</div>
      <h1 class="view-title">${title}</h1>
      ${sub ? `<p class="view-sub">${sub}</p>` : ''}
    </div>
    <div class="view-actions">${actions}</div>
  </div>`;
}

function emptyState(iconName, title, text, action = '') {
  return `<div class="empty">
    ${icon(iconName)}
    <b>${esc(title)}</b>
    <p>${esc(text)}</p>
    ${action}
  </div>`;
}

// ------------------------------------------------------------------ обзор

function renderOverview() {
  const snap = snapshot();
  const copy = stateCopy(snap);
  const status = snap.status;
  const connected = snap.peers.filter((peer) => !peer.error).length;
  const peerCount = Math.max(connected, snap.known.length);
  const rootName = snap.root ? basename(snap.root) : t('rail.noLibrary');

  const size = sizeParts(status ? status.size : 0);
  const vaultSize = snap.vault.reduce((sum, row) => sum + Number(row.size || 0), 0);

  const primaryAction = !snap.running
    ? `<button class="btn btn--primary" data-act="start-daemon">${icon('play')}${esc(t('act.startService'))}</button>`
    : `<button class="btn btn--primary" data-act="sync" ${status.state === 'syncing' ? 'disabled' : ''}>${icon('sync')}${esc(t('act.sync'))}</button>`;

  const pauseAction = !snap.running
    ? ''
    : status.paused
      ? `<button class="btn" data-act="resume">${icon('play')}${esc(t('act.resume'))}</button>`
      : `<button class="btn btn--ghost" data-act="pause">${icon('pause')}${esc(t('act.pause'))}</button>`;

  const stats = `<div class="stats">
    <div class="stat">
      <div class="stat-k">${esc(t('stat.files'))}</div>
      <div class="stat-v">${num(status ? status.files : 0)}</div>
      <div class="stat-n">${esc(t('stat.filesNote', { n: num(status ? status.local : 0) }))}</div>
    </div>
    <div class="stat">
      <div class="stat-k">${esc(t('stat.size'))}</div>
      <div class="stat-v">${size.value}<small>${size.unit}</small></div>
    </div>
    <div class="stat">
      <div class="stat-k">${esc(t('stat.ops'))}</div>
      <div class="stat-v">${num(status ? status.ops : 0)}</div>
      ${status && status.last_scan ? `<div class="stat-n">${esc(t('stat.opsNote', { ago: ago(status.last_scan) }))}</div>` : ''}
    </div>
    <div class="stat">
      <div class="stat-k">${esc(t('stat.vault'))}</div>
      <div class="stat-v">${snap.vault.length}</div>
      ${vaultSize ? `<div class="stat-n">${esc(sizeText(vaultSize))}</div>` : ''}
    </div>
  </div>`;

  const peerTitle = peerCount
    ? `${peerCount} ${plural('word.devices', peerCount)}`
    : t('peers.phone');
  const peerMeta = connected
    ? t('peers.online', { ago: ago((snap.peers.find((peer) => !peer.error) || {}).last_seen) })
    : snap.known.length
      ? t('peers.noResponse')
      : t('peers.notPaired');

  const signal = `<section class="panel">
    <div class="panel-head">
      <div class="panel-title">${icon('sync')}${esc(t('panel.connection'))}</div>
      <span class="tag ${connected ? 'tag--good' : ''}">${esc(connected ? t('tag.connected') : t('tag.disconnected'))}</span>
    </div>
    <div class="signal">
      <div class="node">
        <div class="node-ico hot">${icon('pc')}</div>
        <div><b>${esc(t('node.thisPc'))}</b><span>${esc(snap.device || t('common.dash'))}</span></div>
      </div>
      <div class="wire ${connected ? '' : 'is-idle'}">
        <span class="pulse"></span><span class="pulse"></span><span class="pulse"></span>
      </div>
      <div class="node">
        <div class="node-ico ${connected ? 'on' : ''}">${icon('phone')}</div>
        <div><b>${esc(peerTitle)}</b><span>${esc(peerMeta)}</span></div>
      </div>
    </div>
    <div class="signal-foot">
      <div class="meta">
        <span class="dot ${connected ? 'good' : 'warn'}"></span>
        <span>${esc(status && status.last_sync ? t('signal.lastContact', { ago: ago(status.last_sync) }) : t('signal.never'))}</span>
      </div>
      <span class="mono">${esc((status && status.pair_addr) || snap.info.sync_addr || '')}</span>
      <button class="btn btn--sm" data-view="pair">${esc(t('btn.pairCode'))}</button>
    </div>
  </section>`;

  const alerts = [];
  if (!snap.running) {
    alerts.push({
      icon: 'warn',
      tone: 'warn',
      title: t('alert.serviceStopped.title'),
      text: t('alert.serviceStopped.text'),
      action: `<button class="btn btn--sm" data-act="start-daemon">${esc(t('act.start'))}</button>`,
    });
  }
  if (status && status.disk && status.disk.low) {
    const percent = status.disk.free_percent != null
      ? `${status.disk.free_percent.toFixed(1)}%`
      : sizeText(status.disk.free);
    alerts.push({
      icon: 'warn',
      tone: 'bad',
      title: t('alert.lowDisk.title'),
      text: t('alert.lowDisk.text', { percent }),
      action: `<button class="btn btn--sm" data-act="disk-check">${esc(t('alert.lowDisk.action'))}</button>`,
    });
  }
  if (snap.pending.length) {
    alerts.push({
      icon: 'clock',
      tone: '',
      title: tp('alert.pending.title', snap.pending.length),
      text: t('alert.pending.text'),
      action: `<button class="btn btn--sm" data-view="pending">${esc(t('alert.pending.action'))}</button>`,
    });
  }
  if (snap.conflicts.length) {
    alerts.push({
      icon: 'split',
      tone: 'bad',
      title: tp('alert.conflicts.title', snap.conflicts.length),
      text: t('alert.conflicts.text'),
      action: `<button class="btn btn--sm" data-view="conflicts">${esc(t('alert.pending.action'))}</button>`,
    });
  }
  if (snap.running && status.last_error) {
    alerts.push({
      icon: 'alert',
      tone: 'bad',
      title: t('alert.error.title'),
      text: I18N.locale === 'ru' ? String(status.last_error).slice(0, 180) : t('alert.error.text'),
      action: `<button class="btn btn--sm" data-view="settings">${esc(t('alert.error.action'))}</button>`,
    });
  }
  if (!snap.daemonBin) {
    alerts.push({
      icon: 'alert',
      tone: 'warn',
      title: t('alert.noBinary.title'),
      text: t('alert.noBinary.text'),
      action: `<button class="btn btn--sm" data-view="settings">${esc(t('alert.noBinary.action'))}</button>`,
    });
  }

  const attentionRows = alerts.slice(0, 4).map((alert) => `<div class="alert">
      <div class="alert-ico ${alert.tone}">${icon(alert.icon)}</div>
      <div class="alert-body"><b>${esc(alert.title)}</b><p>${esc(alert.text)}</p></div>
      ${alert.action}
    </div>`).join('');

  const attention = `<section class="panel">
    <div class="panel-head">
      <div class="panel-title">${icon('alert')}${esc(t('panel.attention'))}</div>
      <span class="tag ${alerts.length ? 'tag--warn' : 'tag--good'}">${esc(alerts.length ? tp('attention.count', alerts.length) : t('attention.clean'))}</span>
    </div>
    ${alerts.length
      ? `<div class="alert-list">${attentionRows}</div>${alerts.length > 4 ? `<div class="row-meta" style="margin-top:12px">${esc(t('attention.more', { n: alerts.length - 4 }))}</div>` : ''}`
      : `<div class="calm">${icon('check')}<span>${esc(t('attention.empty'))}</span></div>`}
  </section>`;

  const peerRows = [];
  const seen = new Map(snap.peers.map((peer) => [peer.addr, peer]));
  snap.known.slice(0, 3).forEach((known) => {
    const live = seen.get(known.addr);
    if (live) seen.delete(known.addr);
    peerRows.push(`<div class="row">
      <span class="dot ${live ? (live.error ? 'bad' : 'good') : ''}"></span>
      <div class="row-body">
        <div class="row-path" style="font-family:var(--body);font-size:12.5px">${esc(known.dev || t('device.unknown'))}</div>
        <div class="row-meta mono">${esc(known.addr)}</div>
      </div>
      <div class="row-meta" style="margin:0">${esc(live ? (live.error ? t('common.error') : ago(live.last_seen)) : t('common.noAnswer'))}</div>
    </div>`);
  });
  [...seen.values()].slice(0, 2).forEach((live) => {
    peerRows.push(`<div class="row">
      <span class="dot ${live.error ? 'bad' : 'good'}"></span>
      <div class="row-body">
        <div class="row-path" style="font-family:var(--body);font-size:12.5px">${esc(t('device.nearby'))}</div>
        <div class="row-meta mono">${esc(live.addr)}</div>
      </div>
      <div class="row-meta" style="margin:0">${esc(live.error ? t('common.error') : ago(live.last_seen))}</div>
    </div>`);
  });

  const peers = `<section class="panel">
    <div class="panel-head">
      <div class="panel-title">${icon('devices')}${esc(t('panel.devices'))}</div>
      <span class="tag">${esc(snap.known.length + snap.peers.length ? t('devices.tag', { n: snap.known.length + snap.peers.length }) : t('devices.tagEmpty'))}</span>
    </div>
    ${peerRows.length
      ? `<div class="rows">${peerRows.join('')}</div>
         <div class="settings-actions"><button class="btn btn--sm" data-view="pair">${esc(t('devices.connect'))}</button><button class="btn btn--sm btn--ghost" data-view="devices">${esc(t('devices.all'))}</button></div>`
      : emptyState('phone', t('devices.emptyTitle'), t('devices.emptyText'), `<button class="btn btn--sm" data-view="pair">${esc(t('devices.emptyAction'))}</button>`)}
  </section>`;

  return `<div class="hero">
      <div>
        <div class="eyebrow">${esc(t('nav.overview'))} <span class="path">${esc(rootName)}</span></div>
        <h1 class="state-title">${copy.title}</h1>
        <p class="state-sub">${esc(copy.sub)}</p>
        <div class="hero-actions">
          ${primaryAction}
          <button class="btn" data-act="scan" ${!snap.running || (status && status.state === 'syncing') ? 'disabled' : ''}>${icon('rescan')}${esc(t('act.scan'))}</button>
          ${pauseAction}
          <button class="icon-btn" data-act="open-root" title="${esc(t('act.openFolder'))}" ${snap.root ? '' : 'disabled'}>${icon('folder')}</button>
        </div>
      </div>
      <div class="disc-block">
        <div class="disc ${status && status.state === 'syncing' ? 'is-spinning' : ''}" data-tone="${copy.tone}">
          <div class="disc-label">
            <small>${esc(t('state.service'))}</small>
            <b>${esc(copy.disc)}</b>
            <span>${esc(copy.discSub)}</span>
          </div>
        </div>
        <div class="disc-cap">${esc(copy.cap)}</div>
      </div>
    </div>
    ${stats}
    <div class="grid grid-2">${signal}${attention}</div>
    <div class="grid">${peers}</div>`;
}

// ------------------------------------------------------------------ удаления

function renderPending() {
  const snap = snapshot();
  const rows = snap.pending.map((pending) => `<div class="row">
      <div class="row-body">
        <div class="row-path">${esc(pending.path)}</div>
        <div class="row-meta"><span class="dot warn"></span>${esc(t('pending.rowMeta', { ago: ago(pending.detected_at) }))}</div>
      </div>
      <div class="row-side">
        <button class="btn btn--sm" data-act="pending-only" data-path="${esc(pending.path)}">${esc(t('pending.keep'))}</button>
        <button class="btn btn--sm btn--danger" data-act="pending-everywhere" data-path="${esc(pending.path)}">${esc(t('pending.delete'))}</button>
        <button class="btn btn--sm btn--ghost" data-act="pending-forget" data-path="${esc(pending.path)}">${esc(t('pending.dismiss'))}</button>
      </div>
    </div>`).join('');

  const content = snap.pending.length
    ? `<div class="rows">${rows}</div><p class="view-sub" style="margin-top:14px">${esc(t('pending.hint'))}</p>`
    : emptyState('check', t('pending.emptyTitle'), t('pending.emptyText'));

  return viewHead(
    t('pending.kicker'),
    t('pending.title'),
    t('pending.sub'),
    snap.pending.length ? `<button class="btn btn--sm" data-view="vault">${icon('box')}${esc(t('nav.vault'))}</button>` : '',
  ) + `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('clock')}${esc(t('panel.waiting'))}</div><span class="tag ${snap.pending.length ? 'tag--warn' : 'tag--good'}">${snap.pending.length}</span></div>
    ${content}
  </section>`;
}

// ------------------------------------------------------------------ конфликты

function renderConflicts() {
  const snap = snapshot();
  const rows = snap.conflicts.map((path) => `<div class="row">
      <div class="row-body">
        <div class="row-path">${esc(path)}</div>
        <div class="row-meta"><span class="dot bad"></span>${esc(t('conflicts.rowMeta'))}</div>
      </div>
      <div class="row-side">
        <button class="btn btn--sm" data-act="open-conflict" data-path="${esc(path)}">${icon('folder')}${esc(t('conflicts.open'))}</button>
      </div>
    </div>`).join('');

  const content = snap.conflicts.length
    ? `<div class="rows">${rows}</div>`
    : emptyState('check', t('conflicts.emptyTitle'), t('conflicts.emptyText'));

  return viewHead(
    t('conflicts.kicker'),
    t('nav.conflicts'),
    t('conflicts.sub'),
    `<button class="btn btn--sm" data-act="open-root" ${snap.root ? '' : 'disabled'}>${icon('folder')}${esc(t('conflicts.openLibrary'))}</button>`,
  ) + `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('split')}${esc(t('panel.conflicts'))}</div><span class="tag ${snap.conflicts.length ? 'tag--bad' : 'tag--good'}">${snap.conflicts.length}</span></div>
    ${content}
  </section>`;
}

// ------------------------------------------------------------------ хранилище

function renderVault() {
  const snap = snapshot();
  const rows = snap.vault.map((row) => `<div class="row">
      <div class="row-body">
        <div class="row-path">${esc(row.path)}</div>
        <div class="row-meta"><span class="dot"></span>${esc(t('vault.rowMeta', { size: sizeText(row.size), ago: ago(row.deleted_at) }))}</div>
      </div>
      <div class="row-side">
        <button class="btn btn--sm" data-act="restore" data-path="${esc(row.path)}">${icon('refresh')}${esc(t('vault.restore'))}</button>
      </div>
    </div>`).join('');

  const content = snap.vault.length
    ? `<div class="rows">${rows}</div>`
    : emptyState('box', t('vault.emptyTitle'), t('vault.emptyText'));

  return viewHead(
    t('vault.kicker'),
    t('nav.vault'),
    t('vault.sub'),
    `<button class="btn btn--sm" data-act="gc">${icon('refresh')}${esc(t('vault.gc'))}</button>`,
  ) + `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('box')}${esc(t('panel.restorable'))}</div><span class="tag ${snap.vault.length ? 'tag--warn' : ''}">${esc(tp('vault.count', snap.vault.length))}</span></div>
    ${content}
  </section>`;
}

// ------------------------------------------------------------------ устройства

function renderDevices() {
  const snap = snapshot();
  const status = snap.status;

  const current = `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('pc')}${esc(t('node.thisPc'))}</div><span class="tag ${snap.running ? 'tag--good' : ''}">${esc(snap.running ? t('devices.tagOnline') : t('devices.tagOffline'))}</span></div>
    <div class="kv"><span class="kv-k">${esc(t('kv.id'))}</span><span class="kv-v">${esc(snap.device || t('common.dash'))}</span></div>
    <div class="kv"><span class="kv-k">${esc(t('kv.library'))}</span><span class="kv-v">${esc(snap.root || t('common.notSelected'))}</span></div>
    <div class="kv"><span class="kv-k">${esc(t('kv.syncAddr'))}</span><span class="kv-v">${esc((status && status.pair_addr) || snap.info.sync_addr || t('common.dash'))}</span></div>
    <div class="kv"><span class="kv-k">${esc(t('kv.filesOnDisk'))}</span><span class="kv-v">${num(status ? status.local : 0)}</span></div>
    <div class="settings-actions">
      <button class="btn btn--sm" data-act="copy-device" ${snap.device ? '' : 'disabled'}>${icon('copy')}${esc(t('devices.copyId'))}</button>
      <button class="btn btn--sm btn--ghost" data-act="open-root" ${snap.root ? '' : 'disabled'}>${icon('folder')}${esc(t('devices.folder'))}</button>
    </div>
  </section>`;

  const seen = new Map(snap.peers.map((peer) => [peer.addr, peer]));
  const rows = [];
  snap.known.forEach((known) => {
    const live = seen.get(known.addr);
    if (live) seen.delete(known.addr);
    rows.push(`<div class="row">
      <span class="dot ${live ? (live.error ? 'bad' : 'good') : ''}"></span>
      <div class="row-body">
        <div class="row-path" style="font-family:var(--body);font-size:12.5px">${esc(known.dev || t('device.unknown'))}</div>
        <div class="row-meta mono">${esc(known.addr)}${live && live.error ? ' · ' + esc(live.error) : ''}</div>
      </div>
      <div class="row-meta" style="margin:0">${esc(live ? (live.error ? t('common.error') : ago(live.last_seen)) : t('common.noAnswer'))}</div>
    </div>`);
  });
  [...seen.values()].forEach((live) => {
    rows.push(`<div class="row">
      <span class="dot ${live.error ? 'bad' : 'good'}"></span>
      <div class="row-body">
        <div class="row-path" style="font-family:var(--body);font-size:12.5px">${esc(t('device.nearby'))}</div>
        <div class="row-meta mono">${esc(live.addr)}${live.error ? ' · ' + esc(live.error) : ''}</div>
      </div>
      <div class="row-meta" style="margin:0">${esc(live.error ? t('common.error') : ago(live.last_seen))}</div>
    </div>`);
  });

  const known = `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('devices')}${esc(t('devices.known'))}</div><span class="tag">${snap.known.length}</span></div>
    ${rows.length
      ? `<div class="rows">${rows.join('')}</div>`
      : emptyState('phone', t('devices.otherEmptyTitle'), t('devices.otherEmptyText'))}
    <div class="settings-actions"><button class="btn btn--sm" data-view="pair">${icon('qr')}${esc(t('nav.pair'))}</button></div>
  </section>`;

  return viewHead(
    t('devices.kicker'),
    t('nav.devices'),
    t('devices.sub'),
  ) + `<div class="grid grid-2">${current}${known}</div>`;
}

// ------------------------------------------------------------------ сопряжение

function renderPair() {
  const snap = snapshot();
  const head = viewHead(
    t('pair.kicker'),
    t('nav.pair'),
    t('pair.sub'),
    `<button class="btn btn--sm" data-act="pair-refresh" ${!snap.running ? 'disabled' : ''}>${icon('refresh')}${esc(t('pair.newCode'))}</button>`,
  );

  if (!snap.running) {
    return head + `<section class="panel">${emptyState('warn', t('pair.serviceStoppedTitle'), t('pair.serviceStoppedText'), `<button class="btn btn--sm" data-act="start-daemon">${esc(t('act.startService'))}</button>`)}</section>`;
  }

  if (state.pairError) {
    return head + `<section class="panel"><div class="alert"><div class="alert-ico bad">${icon('alert')}</div><div class="alert-body"><b>${esc(t('pair.codeError'))}</b><p>${esc(state.pairError)}</p></div></div></section>`;
  }

  if (!state.pair) {
    return head + `<section class="panel"><div class="empty"><span class="spinner"></span><b>${esc(t('pair.loading'))}</b><p>${esc(t('pair.loadingText'))}</p></div></section>`;
  }

  const pair = state.pair;
  const qr = pair.svg
    ? pair.svg
    : `<div class="qr-empty">${esc(t('pair.qrMissing'))}<br>${esc(t('pair.qrMissingHint'))}</div>`;

  return head + `<div class="pair-grid">
    <section class="panel">
      <div class="panel-head"><div class="panel-title">${icon('qr')}${esc(t('pair.panelCode'))}</div><span class="tag">${esc(pair.dev || snap.device || '')}</span></div>
      <div class="code-box">${esc(pair.code)}</div>
      <div class="settings-actions">
        <button class="btn btn--sm" data-act="copy-code">${icon('copy')}${esc(t('pair.copy'))}</button>
        <span class="tag">${esc(pair.addr || '')}</span>
      </div>
      <div class="steps">
        <div class="step">${esc(t('pair.step1'))}</div>
        <div class="step">${esc(t('pair.step2'))}</div>
        <div class="step">${esc(t('pair.step3'))}</div>
      </div>
      <form class="field" data-form="pair">
        <input class="input" name="code" placeholder="${esc(t('pair.placeholder'))}" autocomplete="off" spellcheck="false">
        <button class="btn" type="submit">${esc(t('pair.submit'))}</button>
      </form>
    </section>
    <section class="panel">
      <div class="panel-head"><div class="panel-title">${icon('qr')}${esc(t('pair.qrPanel'))}</div><span class="tag">${esc(t('pair.qrTag'))}</span></div>
      <div class="qr-card">${qr}</div>
      <p class="view-sub" style="margin-top:12px">${esc(t('pair.qrNote'))}</p>
    </section>
  </div>`;
}

// ------------------------------------------------------------------ настройки

function renderSettings() {
  const snap = snapshot();
  const info = snap.info;
  const log = state.log;
  const labels = {
    idle: t('stateLabel.idle'),
    syncing: t('stateLabel.syncing'),
    paused: t('stateLabel.paused'),
    paused_with_warning: t('stateLabel.pausedWarning'),
    error: t('stateLabel.error'),
  };
  const rawState = snap.status ? snap.status.state : null;
  const stateLabel = state.running ? (labels[rawState] || rawState || t('settings.running')) : t('stateLabel.stopped');

  const paths = `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('folder')}${esc(t('settings.paths'))}</div></div>
    <div class="kv"><span class="kv-k">${esc(t('kv.music'))}</span><span class="kv-v">${esc(snap.root || t('common.notSelected'))}</span></div>
    <div class="kv"><span class="kv-k">${esc(t('kv.data'))}</span><span class="kv-v">${esc(info.data_dir)}</span></div>
    <div class="kv"><span class="kv-k">${esc(t('kv.config'))}</span><span class="kv-v">${esc(info.config_path)}</span></div>
    <div class="settings-actions">
      <button class="btn btn--sm" data-act="open-root" ${snap.root ? '' : 'disabled'}>${icon('folder')}${esc(t('settings.openMusic'))}</button>
      <button class="btn btn--sm" data-act="open-data">${icon('folder')}${esc(t('settings.data'))}</button>
      <button class="btn btn--sm btn--ghost" data-act="change-library">${esc(t('settings.changeFolder'))}</button>
    </div>
  </section>`;

  const daemon = `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('sync')}${esc(t('settings.service'))}</div><span class="tag ${state.running ? 'tag--good' : ''}">${esc(state.running ? t('settings.running') : t('settings.stopped'))}</span></div>
    <div class="kv"><span class="kv-k">${esc(t('kv.state'))}</span><span class="kv-v">${esc(stateLabel)}</span></div>
    <div class="kv"><span class="kv-k">${esc(t('kv.binary'))}</span><span class="kv-v">${esc(snap.daemonBin || t('common.none'))}</span></div>
    <div class="kv"><span class="kv-k">${esc(t('kv.port'))}</span><span class="kv-v">${esc(snap.info.sync_addr || '')}</span></div>
    <div class="settings-actions">
      ${state.running
        ? `<button class="btn btn--sm btn--danger" data-act="stop-daemon">${esc(t('act.stop'))}</button>`
        : `<button class="btn btn--sm btn--primary" data-act="start-daemon">${esc(t('act.start'))}</button>`}
      <button class="btn btn--sm" data-act="open-web" ${state.running ? '' : 'disabled'}>${icon('external')}${esc(t('settings.web'))}</button>
      <button class="btn btn--sm btn--ghost" data-act="log-refresh">${icon('refresh')}${esc(t('settings.refreshLog'))}</button>
    </div>
    <div class="log" id="daemon-log">${esc(log ? (stripAnsi(log.text) || t('settings.logEmpty')) : t('settings.logHint'))}</div>
    <div class="row-meta mono" style="margin-top:8px">${esc(log ? log.path : info.app_data_dir + '\\daemon.log')}</div>
  </section>`;

  const about = `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('disc')}${esc(t('settings.about'))}</div><span class="tag">AGPL-3.0</span></div>
    <div class="grid grid-2b" style="margin:0;gap:0">
      <div>
        <div class="kv"><span class="kv-k">${esc(t('kv.version'))}</span><span class="kv-v">${esc(info.version)}</span></div>
        <div class="kv"><span class="kv-k">${esc(t('kv.platform'))}</span><span class="kv-v">${esc(info.platform)}</span></div>
      </div>
      <div>
        <div class="kv"><span class="kv-k">${esc(t('kv.device'))}</span><span class="kv-v">${esc(snap.device || t('common.dash'))}</span></div>
        <div class="kv"><span class="kv-k">${esc(t('kv.protocol'))}</span><span class="kv-v">LAN · HLC · Ed25519</span></div>
      </div>
    </div>
    <p class="view-sub" style="margin-top:14px">${esc(t('settings.aboutNote'))}</p>
  </section>`;

  const language = `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('sliders')}${esc(t('settings.language'))}</div></div>
    <div class="settings-actions">
      <button class="btn btn--sm${I18N.locale === 'ru' ? ' btn--primary' : ''}" data-act="lang" data-lang="ru">Русский</button>
      <button class="btn btn--sm${I18N.locale === 'en' ? ' btn--primary' : ''}" data-act="lang" data-lang="en">English</button>
    </div>
  </section>`;

  return viewHead(
    t('settings.kicker'),
    t('nav.settings'),
    t('settings.sub'),
  ) + `<div class="settings-grid">${paths}${daemon}${language}</div><div style="margin-top:18px">${about}</div>`;
}

// ------------------------------------------------------------------ первый запуск

function renderOnboarding() {
  const info = state.info;
  const root = state.onboarding.root;
  const dataDir = state.onboarding.dataDir || (info && info.data_dir);
  const busy = state.busy;

  return `<div class="onboard">
    <div>
      <div class="onboard-kicker">SynctMusic</div>
      <h1 class="onboard-title">${esc(t('onboard.title'))}</h1>
      <p class="onboard-lead">${esc(t('onboard.lead'))}</p>
      <div class="principles">
        <div class="principle"><span class="num">01</span><div><b>${esc(t('onboard.p1.title'))}</b>${esc(t('onboard.p1.text'))}</div></div>
        <div class="principle"><span class="num">02</span><div><b>${esc(t('onboard.p2.title'))}</b>${esc(t('onboard.p2.text'))}</div></div>
        <div class="principle"><span class="num">03</span><div><b>${esc(t('onboard.p3.title'))}</b>${esc(t('onboard.p3.text'))}</div></div>
      </div>
    </div>
    <div class="setup-card">
      <h2>${esc(t('onboard.card'))}</h2>
      <p class="hint">${esc(t('onboard.hint'))}</p>
      <div class="picked ${root ? '' : 'is-empty'}">${icon('folder')}<span>${esc(root || t('onboard.noFolder'))}</span></div>
      <button class="btn" data-act="pick-root" ${busy ? 'disabled' : ''}>${esc(t('onboard.pick'))}</button>

      <div class="picked" style="margin-top:12px">${icon('box')}<span>${esc(dataDir || t('onboard.dataDefault'))}</span></div>
      <button class="btn btn--ghost btn--sm" data-act="pick-data" ${busy ? 'disabled' : ''}>${esc(t('onboard.pickData'))}</button>

      <div class="setup-foot">
 
        <button class="btn btn--primary" data-act="init" ${!root || busy ? 'disabled' : ''}>${esc(busy ? t('onboard.indexing') : t('onboard.start'))}</button>
      </div>
      ${busy ? `<div class="setup-progress"><span class="spinner"></span>${esc(t('onboard.busy'))}</div>` : `<div class="setup-progress">${icon('phone')}<span>${esc(t('onboard.footer'))}</span></div>`}
    </div>
  </div>`;
}

// ------------------------------------------------------------------ рендер

function renderView() {
  switch (state.view) {
    case 'pending': return renderPending();
    case 'conflicts': return renderConflicts();
    case 'vault': return renderVault();
    case 'devices': return renderDevices();
    case 'pair': return renderPair();
    case 'settings': return renderSettings();
    default: return renderOverview();
  }
}

function render() {
  if (!state.info) return;
  const snap = snapshot();
  const copy = stateCopy(snap);
  const onboard = !state.info.config.root;
  if (onboard) state.view = 'overview';

  const main = $('#main');
  const previous = main.scrollTop;
  const viewChanged = state.lastView !== state.view;
  state.lastView = state.view;

  chrome(snap, copy);

  const html = onboard ? renderOnboarding() : renderView();
  if (html !== state.lastHtml) {
    $('#view').innerHTML = html;
    state.lastHtml = html;
  }

  main.scrollTop = viewChanged || !state.keepScroll ? 0 : previous;
  state.keepScroll = true;
}

// ------------------------------------------------------------------ модальное окно

function renderModal() {
  const host = $('#modal');
  if (!state.modal) {
    host.hidden = true;
    host.innerHTML = '';
    return;
  }
  const modal = state.modal;
  host.hidden = false;
  host.innerHTML = `<div class="modal" role="dialog" aria-modal="true">
    <h3>${esc(modal.title)}</h3>
    <p>${esc(modal.text)}</p>
    ${modal.path ? `<div class="modal-path">${esc(modal.path)}</div>` : ''}
    <div class="modal-actions">
      <button class="btn btn--ghost" data-modal="cancel">${esc(t('common.cancel'))}</button>
      <button class="btn ${modal.danger ? 'btn--danger' : 'btn--primary'}" data-modal="confirm">${esc(modal.label || t('common.confirm'))}</button>
    </div>
  </div>`;
}

function openConfirm(options) {
  state.modal = options;
  renderModal();
}

function closeModal() {
  state.modal = null;
  renderModal();
}

// ------------------------------------------------------------------ уведомления

function toast(message, kind = 'ok') {
  const host = $('#toasts');
  const node = document.createElement('div');
  node.className = `toast ${kind === 'bad' ? 'bad' : kind === 'warn' ? 'warn' : ''}`;
  node.textContent = String(message || '');
  host.appendChild(node);
  setTimeout(() => {
    node.style.transition = 'opacity .25s, transform .25s';
    node.style.opacity = '0';
    node.style.transform = 'translateY(6px)';
    setTimeout(() => node.remove(), 260);
  }, 4600);
}

// ------------------------------------------------------------------ действия

async function refreshStatus() {
  try {
    const result = await invoke('daemon_status');
    state.running = !!result.running;
    state.status = result.status || null;
    state.lastError = result.error || null;
    if (state.status && state.status.device) state.lastDevice = state.status.device;
  } catch (error) {
    state.running = false;
    state.status = null;
    state.lastError = errText(error);
  }
}

async function refreshInfo() {
  state.info = await invoke('app_info');
}

async function loadPair(force = false) {
  if (!state.running) return;
  if (state.pair && !force) return;
  if (state.pairLoading) return;
  state.pairLoading = true;
  state.pairError = null;
  try {
    state.pair = await invoke('daemon_pair_code');
  } catch (error) {
    state.pair = null;
    state.pairError = errText(error);
  } finally {
    state.pairLoading = false;
  }
}

async function loadLog(force = false) {
  if (state.log && !force) return;
  if (state.logLoading) return;
  state.logLoading = true;
  try {
    state.log = await invoke('daemon_log_tail', { lines: 80 });
  } catch (error) {
    state.log = { path: '', text: errText(error) };
  } finally {
    state.logLoading = false;
  }
}

async function runBusy(task, fallback = null) {
  state.busy = true;
  render();
  try {
    const message = await task();
    await refreshStatus();
    toast(message || fallback || t('toast.done'));
  } catch (error) {
    toast(errText(error), 'bad');
  } finally {
    state.busy = false;
    render();
  }
}

const daemonCommand = (name, payload, fallback) => runBusy(async () => {
  const result = await invoke('daemon_action', { name, payload: payload || null });
  // Сообщения службы — на русском; в английском интерфейсе берём свой текст.
  return (result && result.message && I18N.locale === 'ru') ? result.message : fallback;
}, fallback);

async function startAndWait() {
  const started = await invoke('start_daemon');
  for (let attempt = 0; attempt < 12; attempt += 1) {
    await sleep(450);
    await refreshStatus();
    if (state.running) return started;
  }
  throw new Error(t('toast.serviceNoAnswer'));
}

function navigate(view) {
  state.view = view;
  state.keepScroll = false;
  if (view === 'pair') {
    loadPair().then(render);
  }
  if (view === 'settings') {
    loadLog().then(render);
  }
  render();
}

async function initializeLibrary() {
  const root = state.onboarding.root;
  if (!root) return;
  state.busy = true;
  render();
  try {
    const result = await invoke('initialize_library', {
      root,
      dataDir: state.onboarding.dataDir || null,
    });
    await refreshInfo();
    state.busy = false;
    state.view = 'overview';
    state.lastHtml = '';
    render();
    toast(t('toast.indexed', { n: num(result.files), word: plural('word.files', result.files) }));
    try {
      await startAndWait();
      toast(t('toast.serviceStarted'));
    } catch (error) {
      toast(errText(error), 'warn');
    }
  } catch (error) {
    toast(errText(error), 'bad');
  } finally {
    state.busy = false;
    render();
  }
}

async function changeLibraryFlow() {
  if (state.running) {
    openConfirm({
      title: t('modal.changeRoot.title'),
      text: t('modal.changeRoot.text'),
      label: t('modal.changeRoot.label'),
      danger: false,
      run: async () => {
        await runBusy(async () => {
          const result = await invoke('stop_daemon');
          return (result && result.message) || t('toast.serviceStopped');
        });
        await pickNewLibrary();
      },
    });
    return;
  }
  await pickNewLibrary();
}

async function pickNewLibrary() {
  const folder = await invoke('pick_folder', { title: t('dialog.pickNewMusic') });
  if (!folder) return;
  await runBusy(async () => {
    await invoke('change_library', { folder });
    await refreshInfo();
    state.lastHtml = '';
    return t('toast.librarySwitched');
  });
  try {
    await startAndWait();
    toast(t('toast.serviceStarted'));
  } catch (error) {
    toast(errText(error), 'warn');
  }
}

async function handleAction(action, dataset) {
  const path = dataset.path;
  switch (action) {
    case 'sync':
      await daemonCommand('sync', null, t('toast.syncDone'));
      break;
    case 'scan':
      await daemonCommand('scan', null, t('toast.scanDone'));
      break;
    case 'pause':
      await daemonCommand('pause', { paused: true }, t('toast.paused'));
      break;
    case 'resume':
      await daemonCommand('pause', { paused: false }, t('toast.resumed'));
      break;
    case 'disk-check':
      await daemonCommand('disk_check', null, t('toast.diskChecked'));
      break;
    case 'gc':
      openConfirm({
        title: t('modal.gc.title'),
        text: t('modal.gc.text'),
        label: t('modal.gc.label'),
        danger: true,
        run: () => daemonCommand('gc', null, t('toast.vaultCleaned')),
      });
      break;
    case 'pending-only':
      await daemonCommand('pending_resolve', { path, action: 'only_here' }, t('toast.keptHere'));
      break;
    case 'pending-forget':
      await daemonCommand('pending_forget', { path }, t('toast.dismissed'));
      break;
    case 'pending-everywhere':
      openConfirm({
        title: t('modal.delete.title'),
        text: t('modal.delete.text'),
        path,
        label: t('modal.delete.label'),
        danger: true,
        run: () => daemonCommand('pending_resolve', { path, action: 'delete_everywhere' }, t('toast.deleteConfirmed')),
      });
      break;
    case 'restore':
      await daemonCommand('restore', { path }, t('toast.restored'));
      break;
    case 'start-daemon':
      await runBusy(async () => {
        const result = await startAndWait();
        return (result && result.started) ? t('toast.serviceStarted') : ((result && result.message) || t('toast.serviceAlready'));
      });
      break;
    case 'stop-daemon':
      openConfirm({
        title: t('modal.stop.title'),
        text: t('modal.stop.text'),
        label: t('act.stop'),
        danger: true,
        run: async () => {
          await runBusy(async () => {
            const result = await invoke('stop_daemon');
            return (result && result.message) || t('toast.serviceStopped');
          });
        },
      });
      break;
    case 'pair-refresh':
      state.pair = null;
      await loadPair(true);
      render();
      if (!state.pairError) toast(t('toast.codeUpdated'));
      break;
    case 'copy-code':
      if (state.pair) await copyText(state.pair.code);
      break;
    case 'copy-device': {
      const snap = snapshot();
      if (snap.device) await copyText(snap.device);
      break;
    }
    case 'lang':
      I18N.setLocale(dataset.lang);
      break;
    case 'open-web':
      await invoke('open_url', { url: 'http://127.0.0.1:47889' }).catch((error) => toast(errText(error), 'bad'));
      break;
    case 'open-root': {
      const snap = snapshot();
      if (snap.root) await invoke('open_path', { path: snap.root }).catch((error) => toast(errText(error), 'bad'));
      break;
    }
    case 'open-data':
      await invoke('open_path', { path: state.info.data_dir }).catch((error) => toast(errText(error), 'bad'));
      break;
    case 'open-conflict':
      if (path) {
        const snap = snapshot();
        const target = joinPath(snap.root, dirname(path));
        await invoke('open_path', { path: target }).catch((error) => toast(errText(error), 'bad'));
      }
      break;
    case 'log-refresh':
      await loadLog(true);
      render();
      break;
    case 'change-library':
      await changeLibraryFlow();
      break;
    case 'pick-root': {
      const folder = await invoke('pick_folder', { title: t('dialog.pickMusic') });
      if (folder) {
        state.onboarding.root = folder;
        render();
      }
      break;
    }
    case 'pick-data': {
      const folder = await invoke('pick_data_folder');
      if (folder) {
        state.onboarding.dataDir = folder;
        render();
      }
      break;
    }
    case 'init':
      await initializeLibrary();
      break;
    default:
      break;
  }
}

// ------------------------------------------------------------------ события

document.addEventListener('pointerdown', (event) => {
  const handle = event.target.closest('[data-resize]');
  if (!handle) return;
  const api = window.__TAURI__ && window.__TAURI__.window;
  if (!api || !api.getCurrentWindow) return;
  event.preventDefault();
  api.getCurrentWindow().startResizeDragging(handle.dataset.resize).catch(() => {});
});

document.addEventListener('dblclick', (event) => {
  if (event.target.closest('.titlebar') && !event.target.closest('[data-win]')) {
    invoke('window_toggle_maximize').catch(() => {});
  }
});

document.addEventListener('click', (event) => {
  const win = event.target.closest('[data-win]');
  if (win) {
    const action = win.dataset.win;
    const command = action === 'close' ? 'window_close' : action === 'max' ? 'window_toggle_maximize' : 'window_minimize';
    invoke(command).catch(() => {});
    return;
  }

  const modalButton = event.target.closest('[data-modal]');
  if (modalButton) {
    if (modalButton.dataset.modal === 'cancel') {
      closeModal();
    } else {
      const run = state.modal && state.modal.run;
      closeModal();
      if (run) run();
    }
    return;
  }

  const navButton = event.target.closest('[data-view]');
  if (navButton) {
    navigate(navButton.dataset.view);
    return;
  }

  const actionButton = event.target.closest('[data-act]');
  if (actionButton && !actionButton.disabled) {
    const dataset = { ...actionButton.dataset };
    handleAction(dataset.act, dataset);
  }
});

document.addEventListener('submit', async (event) => {
  const form = event.target.closest('[data-form="pair"]');
  if (!form) return;
  event.preventDefault();
  const data = new FormData(form);
  const code = String(data.get('code') || '').trim();
  if (!code) return;
  form.reset();
  await daemonCommand('pair', { code }, t('toast.paired'));
});

document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape' && state.modal) closeModal();
});

// ------------------------------------------------------------------ запуск

async function poll() {
  if (!state.info || state.busy) return;
  await refreshStatus();
  render();
}

async function boot() {
  if (state.started) return;
  I18N.apply();
  if (!hasBridge()) {
    $('#view').innerHTML = `<section class="panel">${emptyState('alert', t('err.bridge'), t('err.bridgeText'))}</section>`;
    return;
  }
  state.started = true;

  try {
    await refreshInfo();
  } catch (error) {
    $('#view').innerHTML = `<section class="panel">${emptyState('alert', t('err.settings'), errText(error))}</section>`;
    return;
  }

  await refreshStatus();

  if (!state.info.config.root) {
    state.view = 'overview';
  } else if (!state.running && !state.autoStartTried) {
    state.autoStartTried = true;
    try {
      await startAndWait();
    } catch {
      // Ничего страшного: покажем состояние «служба остановлена» и кнопку запуска.
    }
  }

  if (state.running) await loadPair();
  render();
  setInterval(poll, 3000);
}

// Небольшой внутренний хук: через него удобно проверять состояние и
// переключать разделы при отладке (в том числе из scripts/).
window.__syncth = { state, boot, render, navigate, refreshStatus, handleAction };

I18N.onChange(() => {
  state.lastHtml = '';
  render();
});

boot();
