const bridge = window.__TAURI__?.core;
const invoke = bridge?.invoke;

const state = {
  view: 'loading',
  info: null,
  root: null,
  busy: false,
  note: '',
  error: false,
  pickerOpen: false,
  browser: {
    path: null,
    parent: null,
    entries: [],
    loading: false,
    error: '',
  },
};

let browseSeq = 0;

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

function iconFolder() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true">
    <path d="M3.5 7.4A1.4 1.4 0 0 1 4.9 6h4.2l1.8 2.1h8.2a1.4 1.4 0 0 1 1.4 1.4v7.1a1.4 1.4 0 0 1-1.4 1.4H4.9a1.4 1.4 0 0 1-1.4-1.4z"/>
  </svg>`;
}

function iconMonitor() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true">
    <rect x="3.5" y="4.5" width="17" height="12" rx="2"/>
    <path d="M9 20h6M12 16.5V20"/>
  </svg>`;
}

function iconPhone() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true">
    <rect x="7" y="3" width="10" height="18" rx="2.4"/>
    <path d="M10.5 18h3"/>
  </svg>`;
}

function iconUp() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 19V5M6.5 10.5 12 5l5.5 5.5"/></svg>`;
}

function iconClose() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 6l12 12M18 6 6 18"/></svg>`;
}

function topbar() {
  return `<header class="topbar">
    <span class="brand-mark" aria-hidden="true"></span>
    <span class="brand-name">SynctMusic</span>
  </header>`;
}

function devicesView() {
  return `<aside class="devices" aria-hidden="true">
    <div class="device">
      <span class="device-icon">${iconMonitor()}</span>
      <span class="device-label">${esc(t('map.this'))}</span>
    </div>
    <span class="device-line"></span>
    <div class="device">
      <span class="device-icon">${iconPhone()}</span>
      <span class="device-label">${esc(t('map.peer'))}</span>
    </div>
    <p class="devices-note">${esc(t('map.note'))}</p>
  </aside>`;
}

function welcomeView() {
  const note = state.error
    ? `<p class="welcome-note is-error" role="alert">${esc(state.note)}</p>`
    : `<p class="welcome-note">${esc(state.note)}</p>`;

  return `<section class="welcome">
    ${topbar()}
    <div class="welcome-copy">
      <h1 class="welcome-title">${esc(t('start.title'))}</h1>
      <p class="welcome-text">${esc(t('start.text'))}</p>
      ${devicesView()}
    </div>
    <div class="welcome-actions">
      ${note}
      <button class="choose" id="choose" ${state.busy ? 'disabled' : ''}>
        ${iconFolder()}
        <span>${esc(state.busy ? t('start.scanning') : t('start.action'))}</span>
      </button>
    </div>
  </section>`;
}

function workspaceView() {
  return `<section class="workspace">
    ${topbar()}
    <div class="workspace-head">
      <span class="workspace-label">${esc(t('workspace.label'))}</span>
      <span class="workspace-path">${esc(state.root)}</span>
    </div>
    <div class="workspace-body"></div>
  </section>`;
}

function pickerView() {
  const browser = state.browser;
  const rows = browser.entries.map((entry) => `<button class="picker-row" data-dir="${esc(entry.path)}">
    ${iconFolder()}
    <span>${esc(entry.name)}</span>
  </button>`).join('');

  let body = '';
  if (browser.loading) {
    body = `<div class="picker-state">${esc(t('picker.loading'))}</div>`;
  } else if (browser.error) {
    body = `<div class="picker-state is-error">
      <div>${esc(browser.error)}</div>
      <button class="picker-retry" data-retry>${esc(t('picker.retry'))}</button>
    </div>`;
  } else if (!browser.entries.length) {
    body = `<div class="picker-state">${esc(t('picker.empty'))}</div>`;
  } else {
    body = rows;
  }

  const up = browser.parent
    ? `<button class="picker-row picker-row--up" data-dir="${esc(browser.parent)}">
        ${iconUp()}
        <span>${esc(t('picker.up'))}</span>
      </button>`
    : '';

  return `<section class="picker">
    <header class="picker-head">
      <button class="picker-close" data-picker-close aria-label="${esc(t('picker.cancel'))}">${iconClose()}</button>
      <span class="picker-title">${esc(t('picker.title'))}</span>
      <span></span>
    </header>
    <div class="picker-path">${esc(browser.path || '')}</div>
    <div class="picker-list">
      ${up}
      ${body}
    </div>
    <footer class="picker-foot">
      <button class="picker-use" data-picker-use ${!browser.path || browser.loading ? 'disabled' : ''}>
        ${esc(t('picker.use'))}
      </button>
    </footer>
  </section>`;
}

function render() {
  const screen = document.getElementById('screen');
  const picker = document.getElementById('picker');
  if (!screen || !picker) return;

  if (state.view === 'welcome') screen.innerHTML = welcomeView();
  else if (state.view === 'workspace') screen.innerHTML = workspaceView();
  else if (state.view === 'error') screen.innerHTML = `<div class="fatal">${esc(state.note)}</div>`;
  else screen.innerHTML = '';

  picker.hidden = !state.pickerOpen;
  picker.innerHTML = state.pickerOpen ? pickerView() : '';
}

async function browse(path) {
  const seq = ++browseSeq;
  state.browser.loading = true;
  state.browser.error = '';
  render();

  try {
    const result = await invoke('browse_dirs', { path: path || null });
    if (seq !== browseSeq) return;
    state.browser = {
      path: result.path,
      parent: result.parent || null,
      entries: result.entries || [],
      loading: false,
      error: '',
    };
  } catch (error) {
    if (seq !== browseSeq) return;
    state.browser.loading = false;
    state.browser.error = t('error.browse', { error: message(error) });
  }
  render();
}

function openPicker() {
  if (state.busy || !invoke) return;
  state.pickerOpen = true;
  state.browser = { path: null, parent: null, entries: [], loading: true, error: '' };
  if (!window.history.state?.syncthPicker) window.history.pushState({ syncthPicker: true }, '');
  render();
  browse(null);
}

function closePicker() {
  state.pickerOpen = false;
  if (window.history.state?.syncthPicker) window.history.back();
  render();
}

async function useFolder() {
  const path = state.browser.path;
  if (!path || state.busy) return;

  closePicker();
  state.busy = true;
  state.error = false;
  state.note = path;
  state.view = 'welcome';
  render();

  try {
    const result = await invoke('init_library', { root: path });
    state.root = result?.status?.root || path;
    state.view = 'workspace';
    state.note = '';
  } catch (error) {
    state.error = true;
    state.note = t('error.init', { error: message(error) });
    state.view = 'welcome';
  } finally {
    state.busy = false;
    render();
  }
}

async function boot() {
  window.I18N.apply();

  if (!invoke) {
    state.view = 'error';
    state.note = t('error.bridge');
    render();
    return;
  }

  try {
    state.info = await invoke('app_info');
    state.root = state.info?.info?.root || null;
    state.view = state.root ? 'workspace' : 'welcome';
  } catch (error) {
    state.view = 'error';
    state.note = message(error);
  }
  render();
}

document.addEventListener('click', (event) => {
  if (event.target.closest('#choose')) {
    openPicker();
    return;
  }

  if (event.target.closest('[data-picker-close]')) {
    closePicker();
    return;
  }

  const dir = event.target.closest('[data-dir]');
  if (dir && state.pickerOpen) {
    browse(dir.dataset.dir);
    return;
  }

  if (event.target.closest('[data-retry]')) {
    browse(state.browser.path);
    return;
  }

  if (event.target.closest('[data-picker-use]')) {
    useFolder();
  }
});

window.addEventListener('keydown', (event) => {
  if (event.key === 'Escape' && state.pickerOpen) closePicker();
});

window.addEventListener('popstate', () => {
  if (state.pickerOpen) {
    state.pickerOpen = false;
    render();
  }
});

window.__syncth = { state, boot, render, openPicker, closePicker, browse, useFolder };

boot();
