'use strict';

// SynctMusic для Android. Ядро живёт в приложении, поэтому все действия —
// это прямые команды Tauri: без демона, без HTTP и без портов, кроме синка.

const invoke = (cmd, args) => {
  if (!window.__TAURI__ || !window.__TAURI__.core) {
    return Promise.reject(new Error('нет доступа к ядру приложения'));
  }
  return window.__TAURI__.core.invoke(cmd, args);
};

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

const num = (value) => Number(value || 0).toLocaleString('ru-RU');

function plural(n, one, few, many) {
  const abs = Math.abs(n) % 100;
  const last = abs % 10;
  if (abs > 10 && abs < 20) return many;
  if (last > 1 && last < 5) return few;
  if (last === 1) return one;
  return many;
}

function bytes(value) {
  const b = Number(value || 0);
  if (b < 1024) return `${b} Б`;
  const units = ['КБ', 'МБ', 'ГБ', 'ТБ'];
  let v = b / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i += 1; }
  return `${v >= 100 ? Math.round(v) : v.toFixed(1).replace('.', ',')} ${units[i]}`;
}

const toMillis = (ts) => {
  const n = Number(ts || 0);
  if (!n) return 0;
  return n > 1e12 ? n : n * 1000;
};

function ago(ts) {
  const ms = toMillis(ts);
  if (!ms) return 'только что';
  const diff = Math.max(0, Date.now() - ms);
  const minutes = Math.floor(diff / 60000);
  if (minutes < 1) return 'только что';
  if (minutes < 60) return `${minutes} ${plural(minutes, 'минуту', 'минуты', 'минут')} назад`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} ${plural(hours, 'час', 'часа', 'часов')} назад`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days} ${plural(days, 'день', 'дня', 'дней')} назад`;
  return new Date(ms).toLocaleDateString('ru-RU', { day: '2-digit', month: 'short' });
}

const errText = (error) => {
  if (!error) return 'неизвестная ошибка';
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
  const problems = snap.pending.length + snap.conflicts.length;
  led.className = 'led ' + (state.busy ? 'led--warn' : problems ? 'led--warn' : state.status ? 'led--ok' : '');
  stateText.textContent = state.busy
    ? 'работает'
    : !state.status
      ? (state.error ? 'ошибка' : 'не настроено')
      : problems
        ? 'нужны решения'
        : 'синхронно';

  const badge = $('#badge-pending');
  badge.textContent = String(snap.pending.length);
  badge.hidden = !snap.pending.length;

  for (const tab of document.querySelectorAll('.tab')) {
    tab.classList.toggle('is-on', tab.dataset.tab === state.tab);
  }

  const view = $('#view');
  if (!state.status) {
    view.innerHTML = state.error ? errorScreen(state.error) : setupScreen();
    return;
  }
  if (state.tab === 'pending') view.innerHTML = pendingView(snap);
  else if (state.tab === 'vault') view.innerHTML = vaultView(snap);
  else if (state.tab === 'more') view.innerHTML = moreView(snap);
  else view.innerHTML = overviewView(snap);
}

function errorScreen(message) {
  return `<section class="panel">
    <div class="empty">
      ${icon('alert', 'empty-ic')}
      <b>Не удалось открыть библиотеку</b>
      <p>${esc(message)}</p>
    </div>
    <button class="btn btn--block btn--soft" data-act="refresh">Попробовать снова</button>
  </section>`;
}

function setupScreen() {
  const info = state.info || {};
  const candidates = info.candidates || [];
  const picks = candidates.slice(0, 8).map((c) =>
    `<button class="pick" data-pick="${esc(c.path)}">${esc(c.name)}</button>`).join('');
  return `<section class="panel">
    <div class="kicker">первый запуск</div>
    <h1 class="hero-title">Где лежит <em>музыка?</em></h1>
    <p class="hero-hint">SynctMusic ничего не перекладывает: он запоминает папку и следит за ней.</p>
    <div class="field">
      <label for="root-input">Путь к папке</label>
      <input class="input" id="root-input" value="${esc(info.default_root || '')}" spellcheck="false" autocapitalize="off">
    </div>
    ${picks ? `<div class="picks">${picks}</div>` : ''}
    <div class="actions">
      <button class="btn btn--primary" data-act="init">${icon('folder')}Начать</button>
    </div>
    <p class="hint">Нужен доступ «Ко всем файлам» — без него Android не даст читать музыку вне папки приложения.</p>
  </section>`;
}

function overviewView(snap) {
  const pending = snap.pending.length;
  const conflicts = snap.conflicts.length;
  const peers = snap.known_peers.length;
  const tone = pending || conflicts ? 'var(--warn)' : 'var(--lime)';
  const progress = pending || conflicts ? 0.38 : 1;
  const title = pending || conflicts
    ? 'Нужно ваше <em>решение</em>'
    : 'Всё <em>синхронно</em>';
  const hint = pending
    ? `${pending} ${plural(pending, 'файл пропал', 'файла пропали', 'файлов пропали')} с диска — решите, что с ними делать.`
    : conflicts
      ? 'Есть конфликтные копии — проверьте, какая версия нужна.'
      : 'Файлы под наблюдением, удаления под контролем.';
  const root = snap.root || '';

  const pendingRow = pending
    ? `<div class="rows">${snap.pending.slice(0, 3).map((p) => `
        <div class="row">
          <div class="row-top"><span class="row-path">${esc(p.path)}</span></div>
          <div class="row-meta">пропал ${esc(ago(p.detected_at))}</div>
        </div>`).join('')}</div>
      ${pending > 3 ? `<p class="hint">И ещё ${pending - 3}.</p>` : ''}
      <div class="actions"><button class="btn btn--block btn--soft" data-tab-jump="pending">Разобрать очередь</button></div>`
    : `<div class="empty">${icon('check', 'empty-ic')}<b>Очередь пуста</b><p>Ни один файл не пропал без решения.</p></div>`;

  return `
  <section class="panel">
    <div class="hero">
      <div class="hero-text">
        <div class="kicker">библиотека</div>
        <h1 class="hero-title">${title}</h1>
        <p class="hero-hint">${esc(hint)}</p>
      </div>
      <div class="dial" style="--p:${progress};--tone:${tone}">
        <div class="dial-inner">
          <span class="dial-num">${pending || conflicts ? '!' : 'OK'}</span>
          <span class="dial-sub">${num(snap.files)} ф.</span>
        </div>
      </div>
    </div>
    <div class="actions">
      <button class="btn btn--primary" data-act="sync" ${state.busy ? 'disabled' : ''}>${icon('sync')}Синхронизировать</button>
      <button class="btn btn--soft" data-act="scan" ${state.busy ? 'disabled' : ''}>${icon('scan')}Скан</button>
    </div>
  </section>

  <div class="stats">
    <div class="stat">
      <div class="stat-k">файлов в индексе</div>
      <div class="stat-v">${num(snap.files)}</div>
      <div class="stat-note">${num(snap.local)} есть на телефоне</div>
    </div>
    <div class="stat">
      <div class="stat-k">объём</div>
      <div class="stat-v">${esc(bytes(snap.size))}</div>
      <div class="stat-note">без кодирования и копий</div>
    </div>
    <div class="stat">
      <div class="stat-k">записей в журнале</div>
      <div class="stat-v">${num(snap.ops)}</div>
      <div class="stat-note">дельта, а не весь заново</div>
    </div>
    <div class="stat">
      <div class="stat-k">в корзине</div>
      <div class="stat-v">${num(snap.vault.length)}</div>
      <div class="stat-note">удаления обратимы</div>
    </div>
  </div>

  <section class="panel panel--flat">
    <div class="panel-head">
      <span class="kicker">удаления</span>
      <span class="chip ${pending ? 'chip--warn' : 'chip--ok'}">${pending ? 'есть очередь' : 'чисто'}</span>
    </div>
    ${pendingRow}
  </section>

  <section class="panel panel--flat">
    <div class="panel-head">
      <span class="kicker">устройства</span>
      <span class="chip">${peers ? num(peers) : 'никого'}</span>
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
      : `<div class="empty"><b>Пока никого</b><p>Сопрягите телефон с ПК в разделе «Ещё».</p></div>`}
    <div class="actions"><button class="btn btn--block btn--ghost" data-tab-jump="more">Сопряжение и настройки</button></div>
  </section>

  <section class="panel panel--flat">
    <div class="panel-head"><span class="kicker">папка</span></div>
    <div class="row-meta">${esc(root || 'не выбрана')}</div>
    <div class="actions"><button class="btn btn--block btn--ghost" data-act="change-root">Сменить папку</button></div>
  </section>`;
}

function pendingView(snap) {
  const rows = snap.pending.map((p) => `
    <div class="row">
      <div class="row-top"><span class="row-path">${esc(p.path)}</span></div>
      <div class="row-meta">пропал ${esc(ago(p.detected_at))}</div>
      <div class="row-actions">
        <button class="btn btn--sm btn--danger" data-resolve="delete_everywhere" data-path="${esc(p.path)}">Удалить везде</button>
        <button class="btn btn--sm" data-resolve="only_here" data-path="${esc(p.path)}">Только здесь</button>
      </div>
      <div class="row-actions">
        <button class="btn btn--sm btn--ghost" data-forget="${esc(p.path)}">Забыть</button>
      </div>
    </div>`).join('');

  const conflicts = snap.conflicts.length ? `
    <section class="panel panel--flat">
      <div class="panel-head">
        <span class="kicker">конфликты</span>
        <span class="chip chip--warn">${snap.conflicts.length}</span>
      </div>
      <div class="rows">${snap.conflicts.map((path) => `
        <div class="row">
          <div class="row-top">${icon('split', '')}<span class="row-path">${esc(path)}</span></div>
          <div class="row-meta">рядом лежит вторая версия файла</div>
        </div>`).join('')}</div>
      <p class="hint">Конфликтные копии никогда не удаляются сами: выберите нужную версию в плеере и удалите лишнюю.</p>
    </section>` : '';

  return `<section class="panel">
      <div class="kicker">очередь удалений</div>
      <h1 class="hero-title">${snap.pending.length ? 'Что делать с <em>пропавшими?</em>' : 'Очередь <em>пуста</em>'}</h1>
      <p class="hero-hint">Файл исчез с диска, но библиотека ещё не решила его судьбу.</p>
    </section>
    <section class="panel panel--flat">
      ${snap.pending.length
        ? `<div class="rows">${rows}</div>
           <p class="hint">«Удалить везде» уберёт трек на всех устройствах (копия 30 дней лежит в корзине). «Только здесь» оставит его в общей библиотеке.</p>`
        : `<div class="empty">${icon('check', 'empty-ic')}<b>Ничего не ждёт</b><p>Удаления появятся здесь, когда файл пропадёт с диска.</p></div>`}
    </section>
    ${conflicts}`;
}

function vaultView(snap) {
  const rows = snap.vault.map((v) => `
    <div class="row">
      <div class="row-top"><span class="row-path">${esc(v.path)}</span></div>
      <div class="row-meta">${esc(bytes(v.size))} · удалён ${esc(ago(v.deleted_at))}</div>
      <div class="row-actions">
        <button class="btn btn--sm btn--lime" data-restore="${esc(v.path)}">Вернуть</button>
      </div>
    </div>`).join('');

  return `<section class="panel">
      <div class="kicker">корзина</div>
      <h1 class="hero-title">Удаления <em>обратимы</em></h1>
      <p class="hero-hint">Копии живут 30 дней: файл не занимает место дважды — на NTFS и APFS это hardlink.</p>
    </section>
    <section class="panel panel--flat">
      ${snap.vault.length
        ? `<div class="rows">${rows}</div>
           <div class="actions"><button class="btn btn--block btn--ghost" data-act="gc">${icon('trash')}Очистить просроченное</button></div>`
        : `<div class="empty">${icon('box', 'empty-ic')}<b>Корзина пуста</b><p>Здесь появятся удалённые файлы.</p></div>`}
    </section>`;
}

function moreView(snap) {
  const peers = snap.known_peers.map((p) => `
    <div class="row">
      <div class="row-top">${icon('pc', '')}<span class="row-path">${esc(p.dev)}</span></div>
      <div class="row-meta">${esc(p.addr)}</div>
    </div>`).join('');

  return `<section class="panel">
      <div class="kicker">сопряжение</div>
      <h1 class="hero-title">Одна строка — <em>и всё</em></h1>
      <p class="hero-hint">Покажите этот код на ПК в SynctMusic → «Сопряжение». Аккаунтов и серверов нет.</p>
      <div class="actions">
        <button class="btn btn--primary" data-act="pair-code">${icon('qr')}Мой код</button>
      </div>
      ${state.code ? `
        <div class="field">
          <label>код этого телефона</label>
          <input class="input" id="my-code" value="${esc(state.code)}" readonly>
        </div>
        <div class="actions">
          <button class="btn btn--block btn--soft" data-act="copy-code">${icon('copy')}Скопировать</button>
        </div>` : ''}
      <div class="field">
        <label for="pair-input">Код с другого устройства</label>
        <input class="input" id="pair-input" placeholder="syncth://pair?…" spellcheck="false" autocapitalize="off">
      </div>
      <div class="actions">
        <button class="btn btn--block btn--soft" data-act="pair-accept">Сопрячь</button>
      </div>
    </section>

    <section class="panel panel--flat">
      <div class="panel-head">
        <span class="kicker">устройства</span>
        <span class="chip">${snap.known_peers.length || 'никого'}</span>
      </div>
      ${peers ? `<div class="rows">${peers}</div>` : `<div class="empty"><b>Пока никого</b><p>Сопряжённые устройства появятся здесь.</p></div>`}
    </section>

    <section class="panel panel--flat">
      <div class="panel-head"><span class="kicker">библиотека</span></div>
      <div class="row-meta">${esc(snap.root || '')}</div>
      <div class="row-meta">${num(snap.files)} ${plural(snap.files, 'файл', 'файла', 'файлов')} · ${esc(bytes(snap.size))}</div>
      <div class="actions">
        <button class="btn btn--block btn--ghost" data-act="change-root">Сменить папку</button>
      </div>
    </section>

    <section class="panel panel--flat">
      <div class="panel-head"><span class="kicker">о приложении</span></div>
      <div class="row-meta">SynctMusic ${esc((state.info && state.info.version) || '')} · AGPL-3.0</div>
      <div class="row-meta">устройство: ${esc(snap.device || '')}</div>
      <div class="row-meta">порт синхронизации: ${esc((state.info && state.info.sync_port) || 47887)}</div>
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
  if (!root) { toast('Укажите папку', true); return; }
  await run('Библиотека', async () => {
    const result = await invoke('init_library', { root });
    state.status = result.status;
    if (state.info && state.info.info) {
      state.info.info.root = state.status.root;
      state.info.info.device = state.status.device;
    }
    toast(`Готово: ${result.added} ${plural(result.added, 'файл', 'файла', 'файлов')} в индексе`);
  });
}

async function action(name, payload) {
  switch (name) {
    case 'init':
      await initLibrary();
      break;
    case 'refresh':
      await run('Обновление', async () => { await loadStatus(); });
      break;
    case 'scan':
      await run('Скан', async () => {
        const result = await invoke('scan_now');
        state.status = result.status;
        toast(`Скан: +${result.added}, изменено ${result.changed}, пропало ${result.missing}`);
      });
      break;
    case 'sync':
      await run('Синхронизация', async () => {
        const result = await invoke('sync_now');
        state.status = result.status;
        if (result.errors && result.errors.length) {
          toast(`Частично: ${result.errors[0]}`, true);
        } else {
          toast(`Обмен: отдано ${result.sent}, принято ${result.received}`);
        }
      });
      break;
    case 'gc':
      await run('Корзина', async () => {
        const result = await invoke('vault_gc');
        state.status = result.status;
        toast(`Убрано из корзины: ${result.removed}`);
      });
      break;
    case 'pair-code':
      await run('Код', async () => {
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
    case 'change-root':
      // Смена папки — это новая библиотека: старый индекс остаётся в журнале,
      // поэтому сначала предупреждаем, а не молча сканируем другое дерево.
      if (state.status && state.status.root) {
        const ok = window.confirm(
          'Выбрать другую папку?\n\nИндекс прежней папки останется в журнале, а её файлы попадут в очередь удалений. Для чистой библиотеки лучше начать с нового каталога данных.',
        );
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
  if (!code) { toast('Вставьте код', true); return; }
  await run('Сопряжение', async () => {
    const result = await invoke('pair_accept', { code });
    await loadStatus();
    toast(`Сопряжено: ${result.dev}`);
  });
}

async function copy(text) {
  if (!text) return;
  try {
    await navigator.clipboard.writeText(text);
    toast('Скопировано');
  } catch {
    const input = $('#my-code');
    if (input) { input.select(); document.execCommand('copy'); toast('Скопировано'); }
  }
}

async function resolvePending(path, decision) {
  if (decision === 'delete_everywhere') {
    const ok = window.confirm(`Удалить «${path}» на всех устройствах?\n\nКопия 30 дней останется в корзине.`);
    if (!ok) return;
  }
  await run('Решение', async () => {
    const result = await invoke('resolve_pending', { path, action: decision });
    state.status = result.status;
    if (result.failed && result.failed.length) {
      toast(`Не удалось: ${result.failed[0][0]}`, true);
    } else {
      toast(decision === 'delete_everywhere' ? 'Удалено везде' : 'Оставлено в библиотеке');
    }
  });
}

async function forgetPending(path) {
  await run('Забыть', async () => {
    state.status = await invoke('forget_pending', { path });
    toast('Запись убрана из очереди');
  });
}

async function restorePath(path) {
  await run('Восстановление', async () => {
    state.status = await invoke('restore', { path });
    toast(`Возвращено: ${path}`);
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
    action(act.dataset.act);
  }
});

window.__syncth = { state, boot, render, loadStatus, action };

boot();
