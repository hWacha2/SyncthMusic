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
  searched: false,
  busy: '',
  note: '',
  error: false,
  phoneBusy: '',
  manifest: null,
};

// Что уже нарисовано. Как и на телефоне, не трогаем DOM, если HTML тот же:
// иначе опрос статуса будет вызывать мерцание и сбрасывать фокус.
const painted = { screen: null };

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

function formatSize(bytes) {
  const value = Number(bytes) || 0;
  if (value < 1024) return `${value} Б`;
  const units = ['КБ', 'МБ', 'ГБ', 'ТБ'];
  let size = value / 1024;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size >= 10 ? Math.round(size) : size.toFixed(1)} ${units[unit]}`;
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

function iconUsb() {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="1em" height="1em" viewBox="0 0 24 24">
\t<path d="M0 0h24v24H0z" fill="none" />
\t<g fill="none">
\t\t<rect width="2.96" height="2.96" x="15.72" y="6.28" fill="currentColor" opacity=".2" rx="1" />
\t\t<rect width="2.86" height="2.86" x="10.53" y="19.37" fill="currentColor" opacity=".2" rx="1" />
\t\t<rect width="3.09" height="3.09" x="5.85" y="7.99" fill="currentColor" opacity=".2" rx="1" />
\t\t<path stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="M13.5 20.8a1.54 1.54 0 1 1-3.08 0a1.54 1.54 0 0 1 3.08 0M8.84 9.53a1.48 1.48 0 1 1-2.96 0a1.48 1.48 0 0 1 2.96 0" />
\t\t<path stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="M11.96 17.19L9.4 15.75c-1-.56-1.49-.84-1.76-1.3c-.28-.47-.28-1.05-.28-2.19v-1.24m4.6 3.4l3.14-1.38c.87-.39 1.3-.58 1.6-.91a2 2 0 0 0 .3-.47c.2-.4.2-.88.2-1.84m-5.24 9.44V1.99" />
\t\t<rect width="2.8" height="2.8" x="15.72" y="6.28" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" rx=".5" />
\t\t<path stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="m9.38 3.93l1.16-1.16c.67-.67 1-1 1.42-1s.75.33 1.41 1l1.17 1.16" />
\t</g>
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

  return `<section class="setup view-enter">
    <div class="setup-head">
      <h1 class="setup-title">${esc(t('setup.title'))}</h1>
      <div class="setup-actions">
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

function libraryView() {
  return `<section class="setup setup--center view-enter">
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

function phonePanel() {
  const phone = state.phone;
  const busy = Boolean(state.phoneBusy);
  const root = phone.root
    ? `<span class="meta-value">${esc(phone.root)}</span>`
    : `<span class="meta-value is-empty">${esc(t('phone.no_folder'))}</span>`;

  const stats = state.manifest
    ? `<p class="setup-note">${esc(t('phone.scanned', {
        count: state.manifest.count,
        size: formatSize(state.manifest.total),
      }))}</p>`
    : '';

  return `<section class="phone-card">
    <div class="phone-head">
      <span class="status-dot is-on" aria-hidden="true"></span>
      <span class="phone-name">${esc(phone.name || phone.dev)}</span>
      <span class="badge">${esc(t('phone.linked'))}</span>
    </div>
    <div class="phone-meta">
      <span class="meta-label">${esc(t('phone.folder'))}</span>
      ${root}
    </div>
    <div class="panel-actions">
      <button class="btn" id="scan" ${busy ? 'disabled' : ''}>
        ${state.phoneBusy === 'scan' ? spinner() : ''}${esc(state.phoneBusy === 'scan' ? t('phone.scanning') : t('phone.scan'))}
      </button>
      <button class="btn" id="change-root" ${busy ? 'disabled' : ''}>
        ${esc(state.phoneBusy === 'root' ? t('phone.wait_root') : t('phone.change_root'))}
      </button>
      <button class="btn btn--quiet" id="forget" ${busy ? 'disabled' : ''}>
        ${esc(t('phone.forget'))}
      </button>
    </div>
    ${stats}
  </section>`;
}

function workspaceView() {
  return `<section class="workspace view-enter">
    <div class="workspace-head">
      <span class="workspace-label">${esc(t('workspace.label'))}</span>
      <span class="workspace-path" title="${esc(state.root)}">${esc(state.root)}</span>
    </div>
    <div class="workspace-body">
      ${state.phone.linked ? phonePanel() : `<button class="btn" id="open-devices">${esc(t('workspace.connect'))}</button>`}
    </div>
    ${noteView()}
  </section>`;
}

function render() {
  const screen = document.getElementById('screen');
  if (!screen) return;
  let html = '';
  if (state.view === 'devices') html = devicesView();
  else if (state.view === 'library') html = libraryView();
  else if (state.view === 'workspace') html = workspaceView();
  else if (state.view === 'error') html = `<div class="fatal">${esc(state.note)}</div>`;
  if (html !== painted.screen) {
    screen.innerHTML = html;
    painted.screen = html;
  }

  document.querySelectorAll('[data-window]').forEach((button) => {
    const title = t(`window.${button.dataset.window}`);
    button.setAttribute('aria-label', title);
    button.setAttribute('title', title);
  });
}

function applyLink(result) {
  state.phone = {
    linked: true,
    dev: result.dev,
    name: result.name,
    addr: result.addr,
    root: result.root,
  };
  state.manifest = null;
  state.note = '';
  state.error = false;
  state.view = state.root ? 'workspace' : 'library';
  if (result.needs_root) {
    state.note = t('library.waiting_phone');
  }
  render();
  if (result.needs_root) requestRootChange(true);
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
    applyLink(result);
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
    applyLink(result);
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
  state.searched = true;
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

function openLibrary() {
  if (state.busy) return;
  state.view = 'library';
  state.error = false;
  state.note = '';
  render();
}

async function openDevices() {
  if (state.busy) return;
  state.view = 'devices';
  state.error = false;
  state.note = '';
  render();
  await discoverAll();
}

async function requestRootChange(quiet = false) {
  if (state.phoneBusy || !invoke) return;
  state.phoneBusy = 'root';
  if (!quiet) state.note = t('phone.wait_root');
  render();
  try {
    const result = await invoke('agent_request_root_change');
    if (state.phone.linked) state.phone.root = result?.root || state.phone.root;
    state.manifest = null;
    state.note = '';
    state.error = false;
  } catch (error) {
    state.error = true;
    state.note = t('phone.error', { error: message(error) });
  } finally {
    state.phoneBusy = '';
    render();
  }
}

async function scanPhone() {
  if (state.phoneBusy || !invoke) return;
  state.phoneBusy = 'scan';
  state.note = '';
  state.error = false;
  render();
  try {
    const result = await invoke('agent_scan');
    state.manifest = {
      count: result?.count || 0,
      total: result?.total || 0,
    };
    if (result?.root && state.phone.linked) state.phone.root = result.root;
  } catch (error) {
    state.error = true;
    state.note = t('phone.error', { error: message(error) });
  } finally {
    state.phoneBusy = '';
    render();
  }
}

async function forgetPhone() {
  if (state.phoneBusy || !invoke) return;
  state.phoneBusy = 'forget';
  state.note = '';
  state.error = false;
  try {
    await invoke('agent_forget');
    state.phone = { linked: false };
    state.manifest = null;
    state.view = 'devices';
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
    applyLink(result);
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
  } catch {
    return;
  }
  if (!status || status.busy) return;

  if (!status.connected) {
    state.phone = { linked: false };
    state.manifest = null;
    state.error = true;
    state.note = t('phone.lost');
    state.view = 'devices';
    render();
    await discoverAll(true);
    return;
  }

  let changed = false;
  if (status.root !== undefined && status.root !== state.phone.root) {
    state.phone.root = status.root;
    changed = true;
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
  else if (state.phone.linked) await reconnect();
  setInterval(pollStatus, 2500);
}

document.addEventListener('click', (event) => {
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

  if (event.target.closest('#scan')) {
    scanPhone();
    return;
  }

  if (event.target.closest('#change-root')) {
    requestRootChange();
    return;
  }

  if (event.target.closest('#forget')) {
    forgetPhone();
    return;
  }

  if (event.target.closest('#open-library')) {
    openLibrary();
    return;
  }

  if (event.target.closest('#open-devices')) {
    openDevices();
    return;
  }

  const windowButton = event.target.closest('[data-window]');
  if (windowButton) windowAction(windowButton.dataset.window);
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
  cancelConnect,
  chooseFolder,
  openLibrary,
  openDevices,
  requestRootChange,
  scanPhone,
  forgetPhone,
};

boot();
