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
  pickerRequested: false,
  rootChangeSeen: false,
  pairBusy: false,
  browser: {
    path: null,
    parent: null,
    entries: [],
    loading: false,
    error: '',
  },
};

/// Что уже нарисовано. Нужно, чтобы секундный опрос не пересоздавал DOM
/// и не вызывал мерцание, если данные не изменились.
const painted = { screen: null, view: null, picker: null, pickerOpen: false, pair: null, pairOpen: false };

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

function iconPhone() {
  return `<svg viewBox="0 0 24 24" aria-hidden="true">
    <rect x="7.5" y="2.5" width="9" height="19" rx="2.6"/>
    <path d="M10.5 18.5h3"/>
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
    <span class="brand-name">${esc(t('app.name'))}</span>
  </header>`;
}

function languageView() {
  return `<section class="wait language">
    ${topbar()}
    <div class="wait-center">
      <div class="wait-copy">
        <h1 class="wait-title">${esc(t('app.name'))}</h1>
        <p class="wait-hint">Выберите язык · Choose your language</p>
      </div>
      <div class="language-choices">
        <button class="ghost language-choice" data-lang="ru">Русский</button>
        <button class="ghost language-choice" data-lang="en">English</button>
      </div>
    </div>
  </section>`;
}

function waitingView() {
  const info = state.info || {};
  // Пока связь установлена, ошибка прошлого сеанса не должна висеть рядом
  // со статусом «подключено».
  const statusError = info.error && !info.connected
    ? `<p class="agent-error" role="alert">${esc(t('agent.error', { error: info.error }))}</p>`
    : '';

  return `<section class="wait">
    ${topbar()}
    <div class="wait-center">
      <div class="wait-orb">
        <span class="wait-ring" aria-hidden="true"></span>
        <span class="wait-ring wait-ring--late" aria-hidden="true"></span>
        ${iconPhone()}
      </div>
      <div class="wait-copy">
        <h1 class="wait-title">${esc(t('wait.title'))}</h1>
        <p class="wait-hint">${esc(t('wait.hint'))}</p>
        ${state.root
          ? `<button class="ghost" id="change-root" ${state.busy ? 'disabled' : ''}>
              ${esc(t('agent.change'))}
            </button>
            <p class="wait-path">${esc(state.root)}</p>`
          : ''}
      </div>
      ${statusError}
    </div>
    <div class="wait-foot">${esc(info.name || '')}</div>
  </section>`;
}

function readyView() {
  const info = state.info || {};

  // Пока связь установлена, ошибка прошлого сеанса не должна висеть рядом
  // со статусом «подключено».
  const statusError = info.error && !info.connected
    ? `<p class="agent-error" role="alert">${esc(t('agent.error', { error: info.error }))}</p>`
    : '';

  return `<section class="ready">
    ${topbar()}
    <div class="ready-head">
      <span class="status-dot is-on" aria-hidden="true"></span>
      <h1 class="ready-title">${esc(t('ready.title'))}</h1>
    </div>
    ${statusError}
    <div class="ready-field">
      <div class="field-label">${esc(t('agent.folder'))}</div>
      <div class="folder-path">${esc(state.root || '')}</div>
      <button class="ghost" id="change-root" ${state.busy ? 'disabled' : ''}>
        ${esc(t('agent.change'))}
      </button>
    </div>
  </section>`;
}

function pickerView() {
  const browser = state.browser;
  const rows = browser.entries.map((entry) => `<button class="picker-row" data-dir="${esc(entry.path)}">
    ${iconFolder()}
    <span>${esc(entry.name)}</span>
  </button>`).join('');

  let body = '';
  if (browser.loading && !browser.entries.length) {
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

  const banner = `<div class="picker-banner">${esc(t(state.pickerRequested ? 'picker.request' : 'picker.hint'))}</div>`;
  const canClose = Boolean(state.root) || state.pickerRequested;

  return `<section class="picker">
    <header class="picker-head">
      ${canClose
        ? `<button class="picker-close" data-picker-close aria-label="${esc(t('picker.cancel'))}">${iconClose()}</button>`
        : '<span></span>'}
      <span class="picker-title">${esc(t('picker.change'))}</span>
      <span></span>
    </header>
    ${banner}
    <div class="picker-path">${esc(browser.path || '')}</div>
    <div class="picker-list${browser.loading ? ' is-loading' : ''}"${browser.loading ? ' aria-busy="true"' : ''}>
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

function pairView() {
  const pending = state.info?.pending;
  if (!pending) return '';
  return `<div class="pair">
    <div class="pair-card">
      <div class="pair-label">${esc(t('pair.title'))}</div>
      <div class="pair-title">${esc(pending.name || pending.dev)}</div>
      <div class="pair-actions">
        <button class="choose" data-pair-accept ${state.pairBusy ? 'disabled' : ''}>${esc(t('pair.accept'))}</button>
        <button class="ghost" data-pair-reject ${state.pairBusy ? 'disabled' : ''}>${esc(t('pair.reject'))}</button>
      </div>
    </div>
  </div>`;
}

function markEnter(host) {
  host.classList.remove('view-enter');
  void host.offsetWidth;
  host.classList.add('view-enter');
}

function render() {
  const screen = document.getElementById('screen');
  const picker = document.getElementById('picker');
  const pair = document.getElementById('pair');
  if (!screen || !picker || !pair) return;

  const viewChanged = state.view !== painted.view;
  let main = '';
  if (state.view === 'language') main = languageView();
  else if (state.view === 'ready') main = readyView();
  else if (state.view === 'waiting') main = waitingView();
  else if (state.view === 'error') main = `<div class="fatal">${esc(state.note)}</div>`;
  if (main !== painted.screen) {
    const scrollTop = screen.scrollTop;
    screen.innerHTML = main;
    painted.screen = main;
    screen.scrollTop = viewChanged ? 0 : scrollTop;
  } else if (viewChanged) {
    screen.scrollTop = 0;
  }
  if (viewChanged) markEnter(screen);
  painted.view = state.view;

  const pickerOpen = state.pickerOpen;
  const pickerChanged = pickerOpen !== painted.pickerOpen;
  const pickerHtml = pickerOpen ? pickerView() : '';
  if (picker.hidden !== !pickerOpen) picker.hidden = !pickerOpen;
  if (pickerHtml !== painted.picker) {
    picker.innerHTML = pickerHtml;
    painted.picker = pickerHtml;
  }
  if (pickerChanged && pickerOpen) markEnter(picker);
  painted.pickerOpen = pickerOpen;

  const pending = state.info?.pending;
  const pairOpen = Boolean(pending);
  const pairChanged = pairOpen !== painted.pairOpen;
  const pairHtml = pairOpen ? pairView() : '';
  if (pair.hidden !== !pairOpen) pair.hidden = !pairOpen;
  if (pairHtml !== painted.pair) {
    pair.innerHTML = pairHtml;
    painted.pair = pairHtml;
  }
  if (pairChanged && pairOpen) markEnter(pair);
  painted.pairOpen = pairOpen;
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

/// Сообщает ПК, что выбор папки отменён, чтобы он не ждал ответа.
function cancelRootRequest() {
  if (!state.pickerRequested) return;
  state.pickerRequested = false;
  if (invoke) invoke('agent_cancel_root').catch(() => {});
}

function openPicker(fromPc = false) {
  if (state.busy || !invoke || state.pickerOpen) return;
  state.pickerOpen = true;
  state.pickerRequested = fromPc;
  state.browser = { path: null, parent: null, entries: [], loading: true, error: '' };
  if (!window.history.state?.syncthPicker) window.history.pushState({ syncthPicker: true }, '');
  render();
  browse(null);
}

function closePicker(cancelRequest = true) {
  // Первый выбор папки — обязательный шаг: закрыть его нельзя. Отменить
  // можно только запрос ПК (тогда ПК получит понятный отказ).
  if (cancelRequest && !state.root && !state.pickerRequested) return;
  if (cancelRequest) cancelRootRequest();
  state.pickerRequested = false;
  state.pickerOpen = false;
  if (window.history.state?.syncthPicker) window.history.back();
  render();
}

async function useFolder() {
  const path = state.browser.path;
  if (!path || state.busy) return;

  closePicker(false);
  state.busy = true;
  state.error = false;
  render();

  try {
    state.info = await invoke('agent_set_root', { path });
    state.root = state.info?.root || path;
    state.view = state.info?.connected ? 'ready' : 'waiting';
    state.note = '';
  } catch (error) {
    state.error = true;
    state.note = t('error.root', { error: message(error) });
    state.view = state.root ? 'ready' : 'waiting';
  } finally {
    state.busy = false;
    render();
  }
}

async function pairDecision(accept) {
  if (state.pairBusy) return;
  state.pairBusy = true;
  render();
  try {
    state.info = await invoke('agent_pair_decision', { accept });
  } catch (error) {
    state.error = true;
    state.note = message(error);
  } finally {
    state.pairBusy = false;
    render();
  }
}

async function refresh() {
  if (!invoke) return;
  try {
    const info = await invoke('agent_info');

    // Проверяем, изменились ли данные, чтобы не дергать render зря
    const infoStr = JSON.stringify(info);
    if (state.info && JSON.stringify(state.info) === infoStr && !info.root_change) {
      return; // Данные те же, ничего не делаем
    }

    state.info = info;
    state.root = info.root || null;

    // Логика переключения экранов: сначала папка на телефоне, потом связь.
    if (state.view !== 'loading' && state.view !== 'error') {
      if (!info.root) {
        state.view = 'waiting';
        if (!state.pickerOpen && !info.pending) openPicker(info.connected);
      } else if (!info.connected) {
        if (state.view !== 'waiting') {
          state.view = 'waiting';
          if (state.pickerOpen) closePicker();
        }
      } else {
        state.view = 'ready';
      }
    }

    // Обработка принудительной смены папки с ПК. Если пикер уже открыт
    // (например, это первый выбор папки), помечаем запрос, чтобы отмена
    // корректно дошла до ПК.
    if (info.root_change && !state.rootChangeSeen && !state.busy && info.connected) {
      state.rootChangeSeen = true;
      if (state.pickerOpen) state.pickerRequested = true;
      else openPicker(true);
    }
    if (!info.root_change) state.rootChangeSeen = false;

    render();
  } catch (error) {
    if (state.view !== 'error') {
      state.view = 'error';
      state.note = message(error);
      render();
    }
  }
}

async function boot() {
  window.I18N.apply();

  // Первый запуск начинается с выбора языка, а не с папки.
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
    state.info = await invoke('agent_info');
    state.root = state.info?.root || null;

    // Сначала папка на телефоне, потом ожидание подключения.
    state.view = state.info?.connected && state.root ? 'ready' : 'waiting';
    if (!state.root && !state.info?.pending) openPicker(Boolean(state.info?.connected));
  } catch (error) {
    state.view = 'error';
    state.note = message(error);
  }
  render();
  setInterval(refresh, 1000);
}
document.addEventListener('click', (event) => {
  const langButton = event.target.closest('[data-lang]');
  if (langButton && !langButton.disabled) {
    langButton.disabled = true;
    window.I18N.setLang(langButton.dataset.lang);
    boot();
    return;
  }

  if (event.target.closest('#change-root')) {
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
    return;
  }

  if (event.target.closest('[data-pair-accept]')) {
    pairDecision(true);
    return;
  }

  if (event.target.closest('[data-pair-reject]')) {
    pairDecision(false);
  }
});

window.addEventListener('keydown', (event) => {
  if (event.key === 'Escape' && state.pickerOpen) closePicker();
});

window.addEventListener('popstate', () => {
  if (!state.pickerOpen) return;
  // Первый выбор папки нельзя отменить: возвращаем запись истории назад.
  if (!state.root && !state.pickerRequested) {
    if (!window.history.state?.syncthPicker) window.history.pushState({ syncthPicker: true }, '');
    return;
  }
  state.pickerOpen = false;
  cancelRootRequest();
  render();
});

window.__syncth = { state, boot, render, openPicker, closePicker, browse, useFolder, pairDecision, refresh };

boot();
