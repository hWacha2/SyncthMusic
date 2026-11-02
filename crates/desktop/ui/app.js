const bridge = window.__TAURI__?.core;
const invoke = bridge?.invoke;

const state = {
  view: 'loading',
  info: null,
  root: null,
  busy: false,
  note: '',
  error: false,
};

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

function welcomeView() {
  const version = state.info?.version || '0.1.0';
  const note = state.error
    ? `<p class="welcome-note is-error" role="alert">${esc(state.note)}</p>`
    : `<p class="welcome-note">${esc(state.note)}</p>`;

  return `<section class="welcome">
    <div class="welcome-content">
      <div class="welcome-copy">
        <h1 class="welcome-title">${esc(t('start.title'))}</h1>
        <p class="welcome-text">${esc(t('start.text'))}</p>
        <div class="welcome-action">
          <button class="choose" id="choose" ${state.busy ? 'disabled' : ''}>
            <span class="choose-icon">${iconFolder()}</span>
            <span class="choose-label">${esc(state.busy ? t('start.scanning') : t('start.action'))}</span>
          </button>
        </div>
        ${note}
      </div>
      <footer class="welcome-foot">
        <span>${esc(t('start.local'))}</span>
        <span>${esc(t('start.version', { version }))}</span>
      </footer>
    </div>
    <div class="orbit" aria-hidden="true"></div>
  </section>`;
}

function workspaceView() {
  return `<section class="workspace">
    <div class="workspace-head">
      <span class="workspace-label">${esc(t('workspace.label'))}</span>
      <span class="workspace-path" title="${esc(state.root)}">${esc(state.root)}</span>
    </div>
    <div class="workspace-body"></div>
  </section>`;
}

function render() {
  const screen = document.getElementById('screen');
  if (!screen) return;
  if (state.view === 'welcome') screen.innerHTML = welcomeView();
  else if (state.view === 'workspace') screen.innerHTML = workspaceView();
  else if (state.view === 'error') screen.innerHTML = `<div class="fatal">${esc(state.note)}</div>`;
  else screen.innerHTML = '';

  document.querySelectorAll('[data-window]').forEach((button) => {
    const title = t(`window.${button.dataset.window}`);
    button.setAttribute('aria-label', title);
    button.setAttribute('title', title);
  });
}

async function chooseFolder() {
  if (state.busy || !invoke) return;
  state.error = false;
  state.note = '';

  let folder;
  try {
    folder = await invoke('pick_folder', { title: t('dialog.pick') });
  } catch (error) {
    state.error = true;
    state.note = t('error.init', { error: message(error) });
    render();
    return;
  }
  if (!folder) return;

  state.busy = true;
  state.note = folder;
  render();

  try {
    const result = await invoke('initialize_library', {
      root: folder,
      dataDir: state.info?.config?.data_dir || null,
    });
    state.root = result?.root || folder;
    state.view = 'workspace';
    state.note = '';
  } catch (error) {
    state.error = true;
    state.note = t('error.init', { error: message(error) });
  } finally {
    state.busy = false;
    render();
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
    state.view = state.root ? 'workspace' : 'welcome';
  } catch (error) {
    state.view = 'error';
    state.note = message(error);
  }
  render();
}

document.addEventListener('click', (event) => {
  const choose = event.target.closest('#choose');
  if (choose && !choose.disabled) {
    chooseFolder();
    return;
  }
  const windowButton = event.target.closest('[data-window]');
  if (windowButton) windowAction(windowButton.dataset.window);
});

window.__syncth = { state, boot, render, chooseFolder };

boot();
