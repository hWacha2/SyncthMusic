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
  pending: [], conflicts: [], vault: [], known_peers: [], device: '',
};

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
    if (state.info.info && state.info.info.device) {
      $('#app-dev').textContent = state.info.info.device;
    }
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

// ------------------------------------------------------------------ отрисовка

function render() {
  const snap = status();
  const device = snap.device || (state.info && state.info.info && state.info.info.device) || '';
  if (device) $('#app-dev').textContent = device;

  const led = $('#app-led');
  const stateText = $('#app-state');
  const progress = $('#progress');
  if (progress) progress.hidden = !state.busy;
  const problems = snap.pending.length + snap.conflicts.length;
  led.className = 'led ' + (state.busy ? 'led--warn' : problems ? 'led--warn' : state.status ? 'led--ok' : '');
  stateText.textContent = state.busy
    ? t('appbar.busy')
    : !state.status
      ? (state.error ? t('appbar.error') : t('appbar.notConfigured'))
      : problems
        ? t('appbar.attention')
        : t('appbar.synced');

  const badge = $('#badge-pending');
  badge.textContent = String(snap.pending.length);
  badge.hidden = !snap.pending.length;

  const conflictBadge = $('#badge-conflicts');
  conflictBadge.textContent = String(snap.conflicts.length);
  conflictBadge.hidden = !snap.conflicts.length;

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
  if (state.tab === 'pending') view.innerHTML = pendingView(snap);
  else if (state.tab === 'conflicts') view.innerHTML = conflictsView(snap);
  else if (state.tab === 'vault') view.innerHTML = vaultView(snap);
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
    <div class="kicker">${esc(t('setup.kicker'))}</div>
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
  const pending = snap.pending.length;
  const conflicts = snap.conflicts.length;
  const peers = snap.known_peers.length;
  const title = t('overview.title.ok');
  const hint = pending
    ? tp('overview.hint.pending', pending)
    : conflicts
      ? t('overview.hint.conflicts')
      : t('overview.hint.ok');
  const root = snap.root || '';

  const alerts = [];
  if (pending) {
    alerts.push({
      icon: 'alert',
      tone: 'bad',
      title: tp('alert.pending.title', pending),
      text: t('alert.pending.text'),
      action: `<button class="btn btn--sm" data-tab-jump="pending">${esc(t('alert.pending.action'))}</button>`,
    });
  }
  if (conflicts) {
    alerts.push({
      icon: 'split',
      tone: 'bad',
      title: tp('alert.conflicts.title', conflicts),
      text: t('alert.conflicts.text'),
      action: `<button class="btn btn--sm" data-tab-jump="conflicts">${esc(t('alert.conflicts.action'))}</button>`,
    });
  }

  return `
  <section class="panel">
    <div class="hero">
      <div class="hero-text">
        <div class="kicker">${esc(t('overview.kicker'))}</div>
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
      <button class="btn btn--primary" data-act="sync" ${state.busy ? 'disabled' : ''}>${icon('sync')}${esc(t('act.sync'))}</button>
      <button class="btn btn--soft" data-act="scan" ${state.busy ? 'disabled' : ''}>${icon('rescan')}${esc(t('act.scan'))}</button>
    </div>
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
              <span class="row-path">${esc(p.dev)}</span>
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

function pendingView(snap) {
  const rows = snap.pending.map((p) => `
    <div class="row">
      <div class="row-top"><span class="row-path">${esc(p.path)}</span></div>
      <div class="row-meta">${esc(t('pending.rowMeta', { ago: ago(p.detected_at) }))}</div>
      <div class="row-actions">
        <button class="btn btn--sm btn--danger" data-resolve="delete_everywhere" data-path="${esc(p.path)}">${esc(t('queue.delete'))}</button>
        <button class="btn btn--sm" data-resolve="only_here" data-path="${esc(p.path)}">${esc(t('queue.keep'))}</button>
      </div>
      <div class="row-actions">
        <button class="btn btn--sm btn--ghost" data-forget="${esc(p.path)}">${esc(t('queue.dismiss'))}</button>
      </div>
    </div>`).join('');

  return `<section class="panel">
      <div class="kicker">${esc(t('queue.kicker'))}</div>
      <h1 class="hero-title">${esc(snap.pending.length ? t('queue.title') : t('queue.titleEmpty'))}</h1>
      <p class="hero-hint">${esc(t('queue.hint'))}</p>
    </section>
    <section class="panel panel--flat">
      ${snap.pending.length
        ? `<div class="rows">${rows}</div>
           <p class="hint">${esc(t('queue.explain'))}</p>`
        : `<div class="empty">${icon('check', 'empty-ic')}<b>${esc(t('pending.emptyTitle'))}</b><p>${esc(t('queue.emptyText'))}</p></div>`}
    </section>`;
}

function conflictsView(snap) {
  const rows = snap.conflicts.map((path) => `
    <div class="row">
      <div class="row-top">${icon('split', '')}<span class="row-path">${esc(path)}</span></div>
      <div class="row-meta">${esc(t('conflicts.rowMeta'))}</div>
    </div>`).join('');

  return `<section class="panel">
      <h1 class="hero-title">${esc(t('conflicts.title'))}</h1>
      <p class="hero-hint">${esc(t('conflicts.sub'))}</p>
    </section>
    <section class="panel panel--flat">
      ${snap.conflicts.length
        ? `<div class="rows">${rows}</div>
           <p class="hint">${esc(t('conflicts.hint'))}</p>`
        : `<div class="empty">${icon('check', 'empty-ic')}<b>${esc(t('conflicts.emptyTitle'))}</b><p>${esc(t('conflicts.emptyText'))}</p></div>`}
    </section>`;
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
  const peers = snap.known_peers.map((p) => `
    <div class="row">
      <div class="row-top">${icon('pc', '')}<span class="row-path">${esc(p.dev)}</span></div>
      <div class="row-meta">${esc(p.addr)}</div>
    </div>`).join('');

  return `<section class="panel">
      <h1 class="hero-title">${esc(t('pair.title'))}</h1>
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
      <div class="panel-head"><span class="kicker">${esc(t('pair.about'))}</span></div>
      <div class="row-meta">${esc(t('pair.version', { v: (state.info && state.info.version) || '' }))}</div>
      <div class="row-meta">${esc(t('pair.device', { dev: snap.device || '' }))}</div>
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
    }
    toast(t('toast.initDone', { n: num(result.added), word: plural('word.files', result.added) }));
  });
}

async function action(name, payload) {
  switch (name) {
    case 'init':
      await initLibrary();
      break;
    case 'refresh':
      await run(t('run.refresh'), async () => { await loadStatus(); });
      break;
    case 'scan':
      await run(t('run.scan'), async () => {
        const result = await invoke('scan_now');
        state.status = result.status;
        toast(t('toast.scan', { added: result.added, changed: result.changed, missing: result.missing }));
      });
      break;
    case 'sync':
      await run(t('run.sync'), async () => {
        const result = await invoke('sync_now');
        state.status = result.status;
        if (result.errors && result.errors.length) {
          toast(t('toast.syncPartial', { error: result.errors[0] }), true);
        } else {
          toast(t('toast.sync', { sent: result.sent, received: result.received }));
        }
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
    toast(t('toast.paired', { dev: result.dev }));
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

document.addEventListener('click', (event) => {
  const tab = event.target.closest('.tab');
  if (tab) {
    state.tab = tab.dataset.tab;
    render();
    $('#view').scrollTop = 0;
    return;
  }

  const jump = event.target.closest('[data-tab-jump]');
  if (jump) {
    state.tab = jump.dataset.tabJump;
    render();
    $('#view').scrollTop = 0;
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
    action(act.dataset.act, act.dataset.act === 'lang' ? act.dataset.lang : undefined);
  }
});

window.__syncth = { state, boot, render, loadStatus, action };

I18N.onChange(() => {
  render();
});

boot();
