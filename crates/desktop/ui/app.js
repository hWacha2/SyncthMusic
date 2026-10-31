'use strict';

// Ошибку загрузки интерфейса видно без консоли: это десктоп, а не вкладка.
window.addEventListener('error', (event) => {
  const view = document.getElementById('view');
  if (view && !view.dataset.errorShown) {
    view.dataset.errorShown = '1';
    view.innerHTML = `<section class="panel"><div class="empty"><b>Ошибка интерфейса</b><p>${String(event.message || event.error || '')}</p></div></section>`;
  }
});

// ------------------------------------------------------------------ основа

const tauri = window.__TAURI__;

// Мост ищем в момент вызова: это позволяет открыть интерфейс в браузере
// с подставным мостом для проверки вёрстки.
function invoke(command, args) {
  const bridge = window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke;
  if (!bridge) return Promise.reject(new Error('мост Tauri недоступен'));
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

const num = (value) => Number(value || 0).toLocaleString('ru-RU');

function plural(n, one, few, many) {
  const abs = Math.abs(n) % 100;
  const tail = abs % 10;
  if (abs > 10 && abs < 20) return many;
  if (tail > 1 && tail < 5) return few;
  if (tail === 1) return one;
  return many;
}

function sizeParts(bytes) {
  const units = ['Б', 'КБ', 'МБ', 'ГБ', 'ТБ'];
  let value = Number(bytes || 0);
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  const shown = index === 0
    ? String(Math.round(value))
    : value.toLocaleString('ru-RU', { maximumFractionDigits: 1 });
  return { value: shown, unit: units[index] };
}

const sizeText = (bytes) => {
  const parts = sizeParts(bytes);
  return `${parts.value} ${parts.unit}`;
};

function toMillis(ts) {
  if (!ts) return 0;
  const value = Number(ts);
  return value > 1e12 ? value : value * 1000;
}

function ago(ts) {
  const ms = toMillis(ts);
  if (!ms) return '—';
  const diff = Math.max(0, Date.now() - ms);
  const minutes = Math.floor(diff / 60000);
  if (minutes < 1) return 'только что';
  if (minutes < 60) return `${minutes} мин назад`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} ч назад`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days} дн назад`;
  return new Date(ms).toLocaleDateString('ru-RU', { day: '2-digit', month: 'short' });
}

function clock(ts) {
  const ms = toMillis(ts);
  if (!ms) return '—';
  return new Date(ms).toLocaleString('ru-RU', {
    day: '2-digit',
    month: 'short',
    hour: '2-digit',
    minute: '2-digit',
  });
}

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

// В лог демона попадают ANSI-цвета из tracing; в интерфейсе они не нужны.
const stripAnsi = (text) => String(text || '').replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, '');

function errText(error) {
  if (!error) return 'Неизвестная ошибка';
  if (typeof error === 'string') return error;
  return error.message || String(error);
}

async function copyText(text) {
  if (!text) return;
  try {
    await navigator.clipboard.writeText(text);
    toast('Скопировано');
  } catch {
    const area = document.createElement('textarea');
    area.value = text;
    area.style.position = 'fixed';
    area.style.opacity = '0';
    document.body.appendChild(area);
    area.select();
    try {
      document.execCommand('copy');
      toast('Скопировано');
    } catch {
      toast('Не удалось скопировать', 'bad');
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
      discSub: 'демон',
      cap: 'ожидает запуска',
      title: snap.cfg.root ? 'Демон <em>остановлен</em>' : 'Библиотека <em>не выбрана</em>',
      sub: snap.cfg.root
        ? 'Файлы на месте и ничего не потеряно. Запустите демон, чтобы продолжить обмен с телефоном.'
        : 'Выберите папку с музыкой, чтобы начать.',
    };
  }

  const pending = snap.pending.length;
  const pendingNote = pending
    ? ` ${pending} ${plural(pending, 'удаление ждёт', 'удаления ждут', 'удалений ждут')} вашего решения.`
    : '';

  switch (status.state) {
    case 'syncing':
      return {
        tone: 'busy',
        disc: 'SYNC',
        discSub: 'сверяем',
        cap: 'идёт обмен',
        title: 'Сверяем <em>библиотеку</em>',
        sub: 'Смотрим, что изменилось, и переносим только разницу — без полной перекачки.',
      };
    case 'paused_with_warning':
      return {
        tone: 'bad',
        disc: '!',
        discSub: 'мало места',
        cap: 'синхронизация на паузе',
        title: 'Мало <em>места</em>',
        sub: 'Чтобы не забить диск, синхронизация приостановлена. Освободите немного места и запустите проверку.',
      };
    case 'paused':
      return {
        tone: 'idle',
        disc: 'PAUSE',
        discSub: 'пауза',
        cap: 'фоновая работа остановлена',
        title: 'Пауза',
        sub: `Сканирование и сеть остановлены. Файлы не меняются.${pendingNote}`,
      };
    case 'error':
      return {
        tone: 'bad',
        disc: 'ERR',
        discSub: 'ошибка',
        cap: 'нужно внимание',
        title: 'Нужно <em>внимание</em>',
        sub: status.last_error ? String(status.last_error) : 'Демон сообщил об ошибке.',
      };
    default:
      return {
        tone: 'good',
        disc: 'OK',
        discSub: `${num(status.files)} ф.`,
        cap: status.last_sync ? `связь ${ago(status.last_sync)}` : 'готов к обмену',
        title: 'Всё <em>синхронно</em>',
        sub: status.last_sync
          ? `Последняя связь ${ago(status.last_sync)}.${pendingNote}`
          : `Демон работает, файлы под наблюдением.${pendingNote}`,
      };
  }
}

// ------------------------------------------------------------------ каркас

function navHtml(snap) {
  const items = [
    { id: 'overview', label: 'Обзор', icon: 'disc' },
    { id: 'pending', label: 'Удаления', icon: 'clock', count: snap.pending.length },
    { id: 'conflicts', label: 'Конфликты', icon: 'split', count: snap.conflicts.length },
    { id: 'vault', label: 'Хранилище', icon: 'box', count: snap.vault.length },
    { id: 'devices', label: 'Устройства', icon: 'devices', count: snap.known.length },
    { id: 'pair', label: 'Сопряжение', icon: 'qr' },
    { id: 'settings', label: 'Настройки', icon: 'sliders' },
  ];

  return items.map((item) => {
    const active = state.view === item.id ? ' active' : '';
    const count = item.count
      ? `<span class="nav-count${item.id === 'pending' || item.id === 'conflicts' ? ' hot' : ''}">${item.count}</span>`
      : '';
    return `<button class="nav-item${active}" data-view="${item.id}">
      ${icon(item.icon, 'nav-ico')}<span class="nav-label">${item.label}</span>${count}
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
    ? 'первый запуск'
    : !state.running
      ? 'демон остановлен'
      : snap.status && snap.status.state === 'syncing'
        ? 'синхронизация…'
        : snap.status && snap.status.state === 'paused'
          ? 'пауза'
          : snap.status && snap.status.state === 'error'
            ? 'ошибка'
            : 'на связи';

  $('#tb-ver').textContent = info ? `v${info.version}` : '';
  $('#tb-state').textContent = stateText;
  $('#tb-dev').textContent = snap.device || '';

  $('#rail-dev').textContent = snap.device || '—';
  $('#rail-dev').title = snap.device || '';
  $('#rail-root').textContent = snap.root ? basename(snap.root) : 'Библиотека не выбрана';
  $('#rail-root').title = snap.root || '';
  $('#rail-daemon').textContent = state.running ? 'демон работает' : 'демон остановлен';
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
  const rootName = snap.root ? basename(snap.root) : 'библиотека не выбрана';

  const size = sizeParts(status ? status.size : 0);
  const vaultSize = snap.vault.reduce((sum, row) => sum + Number(row.size || 0), 0);

  const primaryAction = !snap.running
    ? `<button class="btn btn--primary" data-act="start-daemon">${icon('play')}Запустить демон</button>`
    : `<button class="btn btn--primary" data-act="sync" ${status.state === 'syncing' ? 'disabled' : ''}>${icon('sync')}Синхронизировать</button>`;

  const pauseAction = !snap.running
    ? ''
    : status.paused
      ? `<button class="btn" data-act="resume">${icon('play')}Продолжить</button>`
      : `<button class="btn btn--ghost" data-act="pause">${icon('pause')}Пауза</button>`;

  const stats = `<div class="stats">
    <div class="stat">
      <div class="stat-k">Файлов в индексе</div>
      <div class="stat-v">${num(status ? status.files : 0)}</div>
      <div class="stat-n">${num(status ? status.local : 0)} есть на этом диске</div>
    </div>
    <div class="stat">
      <div class="stat-k">Объём библиотеки</div>
      <div class="stat-v">${size.value}<small>${size.unit}</small></div>
      <div class="stat-n">Без перекодирования и копий</div>
    </div>
    <div class="stat">
      <div class="stat-k">Записей в журнале</div>
      <div class="stat-v">${num(status ? status.ops : 0)}</div>
      <div class="stat-n">${status && status.last_scan ? `скан ${ago(status.last_scan)}` : 'журнал изменений'}</div>
    </div>
    <div class="stat">
      <div class="stat-k">В хранилище</div>
      <div class="stat-v">${snap.vault.length}</div>
      <div class="stat-n">${vaultSize ? sizeText(vaultSize) + ' можно вернуть' : 'удаления обратимы'}</div>
    </div>
  </div>`;

  const peerTitle = peerCount
    ? `${peerCount} ${plural(peerCount, 'устройство', 'устройства', 'устройств')}`
    : 'Телефон';
  const peerMeta = connected
    ? `на связи ${ago((snap.peers.find((peer) => !peer.error) || {}).last_seen)}`
    : snap.known.length
      ? 'пока не отвечает'
      : 'ещё не сопряжён';

  const signal = `<section class="panel">
    <div class="panel-head">
      <div class="panel-title">${icon('sync')}Сигнал</div>
      <span class="tag ${connected ? 'tag--good' : ''}">${connected ? 'LAN активен' : 'локальная сеть'}</span>
    </div>
    <div class="signal">
      <div class="node">
        <div class="node-ico hot">${icon('pc')}</div>
        <div><b>Этот компьютер</b><span>${esc(snap.device || '—')}</span></div>
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
        <span>${status && status.last_sync ? `Связь ${ago(status.last_sync)}` : 'Ещё не синхронизировались'}</span>
      </div>
      <span class="mono">${esc((status && status.pair_addr) || snap.info.sync_addr || '')}</span>
      <button class="btn btn--sm" data-view="pair">Код сопряжения</button>
    </div>
  </section>`;

  const alerts = [];
  if (!snap.running) {
    alerts.push({
      icon: 'warn',
      tone: 'warn',
      title: 'Демон остановлен',
      text: 'Запустите его, чтобы возобновить наблюдение за папкой и обмен с телефоном.',
      action: `<button class="btn btn--sm" data-act="start-daemon">Запустить</button>`,
    });
  }
  if (status && status.disk && status.disk.low) {
    const percent = status.disk.free_percent != null
      ? `${status.disk.free_percent.toFixed(1)}%`
      : sizeText(status.disk.free);
    alerts.push({
      icon: 'warn',
      tone: 'bad',
      title: 'Мало свободного места',
      text: `Осталось ${percent} — синхронизация на паузе, чтобы не забить диск.`,
      action: `<button class="btn btn--sm" data-act="disk-check">Проверить</button>`,
    });
  }
  if (snap.pending.length) {
    alerts.push({
      icon: 'clock',
      tone: '',
      title: `${snap.pending.length} ${plural(snap.pending.length, 'удаление', 'удаления', 'удалений')} без решения`,
      text: 'Файл исчез с диска. Удалить его везде, оставить только здесь или забыть — выбор за вами.',
      action: `<button class="btn btn--sm" data-view="pending">Открыть</button>`,
    });
  }
  if (snap.conflicts.length) {
    alerts.push({
      icon: 'split',
      tone: 'bad',
      title: `${snap.conflicts.length} ${plural(snap.conflicts.length, 'конфликт', 'конфликта', 'конфликтов')}`,
      text: 'Один и тот же трек изменён с двух сторон. Ничего не перезаписано молча.',
      action: `<button class="btn btn--sm" data-view="conflicts">Разобрать</button>`,
    });
  }
  if (snap.running && status.last_error) {
    alerts.push({
      icon: 'alert',
      tone: 'bad',
      title: 'Последняя ошибка',
      text: String(status.last_error).slice(0, 180),
      action: `<button class="btn btn--sm" data-view="settings">Лог</button>`,
    });
  }
  if (!snap.daemonBin) {
    alerts.push({
      icon: 'alert',
      tone: 'warn',
      title: 'Не найден syncthd',
      text: 'Соберите демон командой cargo build --release -p syncth-daemon.',
      action: `<button class="btn btn--sm" data-view="settings">Подробнее</button>`,
    });
  }

  const attentionRows = alerts.slice(0, 4).map((alert) => `<div class="alert">
      <div class="alert-ico ${alert.tone}">${icon(alert.icon)}</div>
      <div class="alert-body"><b>${esc(alert.title)}</b><p>${esc(alert.text)}</p></div>
      ${alert.action}
    </div>`).join('');

  const attention = `<section class="panel">
    <div class="panel-head">
      <div class="panel-title">${icon('alert')}Требует решения</div>
      <span class="tag ${alerts.length ? 'tag--warn' : 'tag--good'}">${alerts.length ? alerts.length + ' ' + plural(alerts.length, 'пункт', 'пункта', 'пунктов') : 'чисто'}</span>
    </div>
    ${alerts.length
      ? `<div class="alert-list">${attentionRows}</div>${alerts.length > 4 ? `<div class="row-meta" style="margin-top:12px">и ещё ${alerts.length - 4}</div>` : ''}`
      : `<div class="calm">${icon('check')}<span>Все решения приняты. SynctMusic не трогает файлы без вас.</span></div>`}
  </section>`;

  const peerRows = [];
  const seen = new Map(snap.peers.map((peer) => [peer.addr, peer]));
  snap.known.slice(0, 3).forEach((known) => {
    const live = seen.get(known.addr);
    if (live) seen.delete(known.addr);
    peerRows.push(`<div class="row">
      <span class="dot ${live ? (live.error ? 'bad' : 'good') : ''}"></span>
      <div class="row-body">
        <div class="row-path" style="font-family:var(--body);font-size:12.5px">${esc(known.dev || 'устройство')}</div>
        <div class="row-meta mono">${esc(known.addr)}</div>
      </div>
      <div class="row-meta" style="margin:0">${live ? (live.error ? 'ошибка' : ago(live.last_seen)) : 'нет ответа'}</div>
    </div>`);
  });
  [...seen.values()].slice(0, 2).forEach((live) => {
    peerRows.push(`<div class="row">
      <span class="dot ${live.error ? 'bad' : 'good'}"></span>
      <div class="row-body">
        <div class="row-path" style="font-family:var(--body);font-size:12.5px">новое устройство</div>
        <div class="row-meta mono">${esc(live.addr)}</div>
      </div>
      <div class="row-meta" style="margin:0">${live.error ? 'ошибка' : ago(live.last_seen)}</div>
    </div>`);
  });

  const peers = `<section class="panel">
    <div class="panel-head">
      <div class="panel-title">${icon('devices')}Устройства</div>
      <span class="tag">${snap.known.length + snap.peers.length ? 'знакомые и рядом' : 'пока никого'}</span>
    </div>
    ${peerRows.length
      ? `<div class="rows">${peerRows.join('')}</div>
         <div class="settings-actions"><button class="btn btn--sm" data-view="pair">Подключить телефон</button><button class="btn btn--sm btn--ghost" data-view="devices">Все устройства</button></div>`
      : emptyState('phone', 'Телефон ещё не подключён', 'Покажите код сопряжения на телефоне — устройства найдут друг друга в локальной сети.', `<button class="btn btn--sm" data-view="pair">Сопряжение</button>`)}
  </section>`;

  const rules = `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('check')}Правила игры</div><span class="tag">без облака</span></div>
    <div class="principles" style="border-top:0;max-width:none">
      <div class="principle" style="padding-top:0"><span class="num">01</span><div><b>Файлы остаются на месте</b>Индексация только читает папку. Никаких переименований и «правильной структуры».</div></div>
      <div class="principle"><span class="num">02</span><div><b>Удаления обратимы</b>Копия живёт в хранилище 30 дней. Восстановить можно в один клик.</div></div>
      <div class="principle" style="padding-bottom:0"><span class="num">03</span><div><b>Никаких аккаунтов</b>Только ваши устройства в локальной сети. Нет сервера — нет подписки.</div></div>
    </div>
  </section>`;

  return `<div class="hero">
      <div>
        <div class="eyebrow">Библиотека <span class="path">${esc(rootName)}</span></div>
        <h1 class="state-title">${copy.title}</h1>
        <p class="state-sub">${esc(copy.sub)}</p>
        <div class="hero-actions">
          ${primaryAction}
          <button class="btn" data-act="scan" ${!snap.running || (status && status.state === 'syncing') ? 'disabled' : ''}>${icon('scan')}Сканировать</button>
          ${pauseAction}
          <button class="icon-btn" data-act="open-root" title="Открыть папку с музыкой" ${snap.root ? '' : 'disabled'}>${icon('folder')}</button>
        </div>
      </div>
      <div class="disc-block">
        <div class="disc ${status && status.state === 'syncing' ? 'is-spinning' : ''}" data-tone="${copy.tone}">
          <div class="disc-label">
            <small>статус</small>
            <b>${esc(copy.disc)}</b>
            <span>${esc(copy.discSub)}</span>
          </div>
        </div>
        <div class="disc-cap">${esc(copy.cap)}</div>
      </div>
    </div>
    ${stats}
    <div class="grid grid-2">${signal}${attention}</div>
    <div class="grid grid-2">${peers}${rules}</div>`;
}

// ------------------------------------------------------------------ удаления

function renderPending() {
  const snap = snapshot();
  const rows = snap.pending.map((pending) => `<div class="row">
      <div class="row-body">
        <div class="row-path">${esc(pending.path)}</div>
        <div class="row-meta"><span class="dot warn"></span>Исчез с диска ${ago(pending.detected_at)} · решение за вами</div>
      </div>
      <div class="row-side">
        <button class="btn btn--sm" data-act="pending-only" data-path="${esc(pending.path)}">Только здесь</button>
        <button class="btn btn--sm btn--danger" data-act="pending-everywhere" data-path="${esc(pending.path)}">Удалить везде</button>
        <button class="btn btn--sm btn--ghost" data-act="pending-forget" data-path="${esc(pending.path)}">Забыть</button>
      </div>
    </div>`).join('');

  const content = snap.pending.length
    ? `<div class="rows">${rows}</div>`
    : emptyState('check', 'Очередь пуста', 'Ни один файл не исчез незаметно. Когда удаление появится, оно будет ждать вашего решения здесь.');

  return viewHead(
    'Очередь решений',
    'Удаления',
    'Файл исчез с диска — SynctMusic не удаляет его на других устройствах сам. Выберите, что сделать: удалить везде (копия останется в хранилище 30 дней), оставить только здесь или забыть запись.',
    snap.pending.length ? `<button class="btn btn--sm" data-view="vault">${icon('box')}Хранилище</button>` : '',
  ) + `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('clock')}Ждут решения</div><span class="tag ${snap.pending.length ? 'tag--warn' : 'tag--good'}">${snap.pending.length}</span></div>
    ${content}
  </section>`;
}

// ------------------------------------------------------------------ конфликты

function renderConflicts() {
  const snap = snapshot();
  const rows = snap.conflicts.map((path) => `<div class="row">
      <div class="row-body">
        <div class="row-path">${esc(path)}</div>
        <div class="row-meta"><span class="dot bad"></span>Изменён с двух сторон — автоматически не перезаписываем</div>
      </div>
      <div class="row-side">
        <button class="btn btn--sm" data-act="open-conflict" data-path="${esc(path)}">${icon('folder')}Открыть папку</button>
      </div>
    </div>`).join('');

  const content = snap.conflicts.length
    ? `<div class="rows">${rows}</div>`
    : emptyState('check', 'Конфликтов нет', 'Одинаковых путей с разным содержимым не обнаружено.');

  return viewHead(
    'Одинаковые имена',
    'Конфликты',
    'Один и тот же путь изменён на двух устройствах. SynctMusic не выбирает победителя молча: проверьте файлы в плеере и оставьте нужную версию. Лишнее можно убрать вручную — копия попадёт в хранилище.',
    `<button class="btn btn--sm" data-act="open-root" ${snap.root ? '' : 'disabled'}>${icon('folder')}Открыть библиотеку</button>`,
  ) + `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('split')}Требуют разбора</div><span class="tag ${snap.conflicts.length ? 'tag--bad' : 'tag--good'}">${snap.conflicts.length}</span></div>
    ${content}
  </section>`;
}

// ------------------------------------------------------------------ хранилище

function renderVault() {
  const snap = snapshot();
  const rows = snap.vault.map((row) => `<div class="row">
      <div class="row-body">
        <div class="row-path">${esc(row.path)}</div>
        <div class="row-meta"><span class="dot"></span>${sizeText(row.size)} · удалён ${ago(row.deleted_at)}</div>
      </div>
      <div class="row-side">
        <button class="btn btn--sm" data-act="restore" data-path="${esc(row.path)}">${icon('refresh')}Вернуть</button>
      </div>
    </div>`).join('');

  const content = snap.vault.length
    ? `<div class="rows">${rows}</div>`
    : emptyState('box', 'Хранилище пусто', 'Сюда попадают удалённые файлы и заменённые версии. Они живут 30 дней — этого хватает, чтобы передумать.');

  return viewHead(
    'Обратимые удаления',
    'Хранилище',
    'Каждый удалённый файл сохраняется на 30 дней. Верните его в библиотеку или дождитесь автоматической очистки.',
    `<button class="btn btn--sm" data-act="gc">${icon('refresh')}Очистить просроченное</button>`,
  ) + `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('box')}Можно вернуть</div><span class="tag ${snap.vault.length ? 'tag--warn' : ''}">${snap.vault.length} ${plural(snap.vault.length, 'файл', 'файла', 'файлов')}</span></div>
    ${content}
  </section>`;
}

// ------------------------------------------------------------------ устройства

function renderDevices() {
  const snap = snapshot();
  const status = snap.status;

  const current = `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('pc')}Этот компьютер</div><span class="tag ${snap.running ? 'tag--good' : ''}">${snap.running ? 'в сети' : 'остановлен'}</span></div>
    <div class="kv"><span class="kv-k">Идентификатор</span><span class="kv-v">${esc(snap.device || '—')}</span></div>
    <div class="kv"><span class="kv-k">Библиотека</span><span class="kv-v">${esc(snap.root || 'не выбрана')}</span></div>
    <div class="kv"><span class="kv-k">Адрес для синхронизации</span><span class="kv-v">${esc((status && status.pair_addr) || snap.info.sync_addr || '—')}</span></div>
    <div class="kv"><span class="kv-k">Файлов на диске</span><span class="kv-v">${num(status ? status.local : 0)}</span></div>
    <div class="settings-actions">
      <button class="btn btn--sm" data-act="copy-device" ${snap.device ? '' : 'disabled'}>${icon('copy')}Копировать ID</button>
      <button class="btn btn--sm btn--ghost" data-act="open-root" ${snap.root ? '' : 'disabled'}>${icon('folder')}Папка</button>
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
        <div class="row-path" style="font-family:var(--body);font-size:12.5px">${esc(known.dev || 'устройство')}</div>
        <div class="row-meta mono">${esc(known.addr)}${live && live.error ? ' · ' + esc(live.error) : ''}</div>
      </div>
      <div class="row-meta" style="margin:0">${live ? (live.error ? 'ошибка' : ago(live.last_seen)) : 'нет ответа'}</div>
    </div>`);
  });
  [...seen.values()].forEach((live) => {
    rows.push(`<div class="row">
      <span class="dot ${live.error ? 'bad' : 'good'}"></span>
      <div class="row-body">
        <div class="row-path" style="font-family:var(--body);font-size:12.5px">Устройство рядом</div>
        <div class="row-meta mono">${esc(live.addr)}${live.error ? ' · ' + esc(live.error) : ''}</div>
      </div>
      <div class="row-meta" style="margin:0">${live.error ? 'ошибка' : ago(live.last_seen)}</div>
    </div>`);
  });

  const known = `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('devices')}Знакомые устройства</div><span class="tag">${snap.known.length}</span></div>
    ${rows.length
      ? `<div class="rows">${rows.join('')}</div>`
      : emptyState('phone', 'Пока только этот компьютер', 'Подключите телефон по коду сопряжения — после этого он появится здесь и будет находиться сам в локальной сети.')}
    <div class="settings-actions"><button class="btn btn--sm" data-view="pair">${icon('qr')}Сопряжение</button></div>
  </section>`;

  return viewHead(
    'Локальная сеть',
    'Устройства',
    'Никакого облака: устройства обмениваются изменениями напрямую, когда оказываются в одной сети. Идентификатор — имя, а ключ подписи закрепляется при первом контакте.',
  ) + `<div class="grid grid-2">${current}${known}</div>`;
}

// ------------------------------------------------------------------ сопряжение

function renderPair() {
  const snap = snapshot();
  const head = viewHead(
    'Один код',
    'Сопряжение',
    'Код содержит адрес, идентификатор и общий секрет библиотеки. Покажите QR на телефоне или вставьте код с другого устройства — сеть и аккаунты не нужны.',
    `<button class="btn btn--sm" data-act="pair-refresh" ${!snap.running ? 'disabled' : ''}>${icon('refresh')}Новый код</button>`,
  );

  if (!snap.running) {
    return head + `<section class="panel">${emptyState('warn', 'Демон остановлен', 'Сопряжение доступно, когда демон запущен: именно он держит секрет библиотеки.', `<button class="btn btn--sm" data-act="start-daemon">Запустить демон</button>`)}</section>`;
  }

  if (state.pairError) {
    return head + `<section class="panel"><div class="alert"><div class="alert-ico bad">${icon('alert')}</div><div class="alert-body"><b>Не удалось получить код</b><p>${esc(state.pairError)}</p></div></div></section>`;
  }

  if (!state.pair) {
    return head + `<section class="panel"><div class="empty"><span class="spinner"></span><b>Готовим код…</b><p>Демон создаёт QR и строку сопряжения.</p></div></section>`;
  }

  const pair = state.pair;
  const qr = pair.svg
    ? pair.svg
    : '<div class="qr-empty">QR недоступен<br>используйте код</div>';

  return head + `<div class="pair-grid">
    <section class="panel">
      <div class="panel-head"><div class="panel-title">${icon('qr')}Код библиотеки</div><span class="tag">${esc(pair.dev || snap.device || '')}</span></div>
      <div class="code-box">${esc(pair.code)}</div>
      <div class="settings-actions">
        <button class="btn btn--sm" data-act="copy-code">${icon('copy')}Копировать код</button>
        <span class="tag">${esc(pair.addr || '')}</span>
      </div>
      <div class="steps">
        <div class="step">Откройте SynctMusic на телефоне и выберите «Сопряжение».</div>
        <div class="step">Отсканируйте QR или вставьте код строкой — устройства обменяются ключами.</div>
        <div class="step">После первого контакта телефон запомнится и будет находиться сам.</div>
      </div>
      <form class="field" data-form="pair">
        <input class="input" name="code" placeholder="Вставить код с другого устройства" autocomplete="off" spellcheck="false">
        <button class="btn" type="submit">Сопрячь</button>
      </form>
    </section>
    <section class="panel">
      <div class="panel-head"><div class="panel-title">${icon('qr')}QR-код</div><span class="tag">наведите камеру</span></div>
      <div class="qr-card">${qr}</div>
      <p class="view-sub" style="margin-top:12px">Код действует, пока не создан новый. Он не покидает вашу сеть.</p>
    </section>
  </div>`;
}

// ------------------------------------------------------------------ настройки

function renderSettings() {
  const snap = snapshot();
  const info = snap.info;
  const log = state.log;
  const labels = {
    idle: 'на связи',
    syncing: 'синхронизация',
    paused: 'пауза',
    paused_with_warning: 'пауза: мало места',
    error: 'ошибка',
  };
  const rawState = snap.status ? snap.status.state : null;
  const stateLabel = state.running ? (labels[rawState] || rawState || 'работает') : 'остановлен';

  const paths = `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('folder')}Папки</div><span class="tag">на месте</span></div>
    <div class="kv"><span class="kv-k">Музыка</span><span class="kv-v">${esc(snap.root || 'не выбрана')}</span></div>
    <div class="kv"><span class="kv-k">Служебные данные</span><span class="kv-v">${esc(info.data_dir)}</span></div>
    <div class="kv"><span class="kv-k">Настройки клиента</span><span class="kv-v">${esc(info.config_path)}</span></div>
    <div class="settings-actions">
      <button class="btn btn--sm" data-act="open-root" ${snap.root ? '' : 'disabled'}>${icon('folder')}Открыть музыку</button>
      <button class="btn btn--sm" data-act="open-data">${icon('folder')}Данные</button>
      <button class="btn btn--sm btn--ghost" data-act="change-library">Сменить папку</button>
    </div>
    <p class="view-sub" style="margin-top:12px">SynctMusic не переименовывает и не перемещает треки. Индекс и хранилище лежат отдельно от библиотеки.</p>
  </section>`;

  const daemon = `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('sync')}Демон</div><span class="tag ${state.running ? 'tag--good' : ''}">${state.running ? 'работает' : 'остановлен'}</span></div>
    <div class="kv"><span class="kv-k">Состояние</span><span class="kv-v">${esc(stateLabel)}</span></div>
    <div class="kv"><span class="kv-k">Бинарник</span><span class="kv-v">${esc(snap.daemonBin || 'не найден')}</span></div>
    <div class="kv"><span class="kv-k">Порт синхронизации</span><span class="kv-v">${esc(snap.info.sync_addr || '')}</span></div>
    <div class="settings-actions">
      ${state.running
        ? `<button class="btn btn--sm btn--danger" data-act="stop-daemon">Остановить</button>`
        : `<button class="btn btn--sm btn--primary" data-act="start-daemon">Запустить</button>`}
      <button class="btn btn--sm" data-act="open-web" ${state.running ? '' : 'disabled'}>${icon('external')}Веб-интерфейс</button>
      <button class="btn btn--sm btn--ghost" data-act="log-refresh">${icon('refresh')}Обновить лог</button>
    </div>
    <div class="log" id="daemon-log">${esc(log ? (stripAnsi(log.text) || 'Лог пуст.') : 'Нажмите «Обновить лог», чтобы прочитать daemon.log.')}</div>
    <div class="row-meta mono" style="margin-top:8px">${esc(log ? log.path : info.app_data_dir + '\\daemon.log')}</div>
  </section>`;

  const about = `<section class="panel">
    <div class="panel-head"><div class="panel-title">${icon('disc')}О программе</div><span class="tag">AGPL-3.0</span></div>
    <div class="grid grid-2b" style="margin:0;gap:0">
      <div>
        <div class="kv"><span class="kv-k">Версия</span><span class="kv-v">${esc(info.version)}</span></div>
        <div class="kv"><span class="kv-k">Платформа</span><span class="kv-v">${esc(info.platform)}</span></div>
      </div>
      <div>
        <div class="kv"><span class="kv-k">Устройство</span><span class="kv-v">${esc(snap.device || '—')}</span></div>
        <div class="kv"><span class="kv-k">Протокол</span><span class="kv-v">LAN · HLC · Ed25519</span></div>
      </div>
    </div>
    <p class="view-sub" style="margin-top:14px">Свободное ПО без серверов, аккаунтов и подписок. Ваша музыка остаётся вашей: локальные файлы — источник истины, удаления проходят через хранилище.</p>
  </section>`;

  return viewHead(
    'Пульт',
    'Настройки',
    'Клиент — тонкая оболочка над демоном syncthd. Здесь видно, где лежат данные и что происходит внутри.',
  ) + `<div class="settings-grid">${paths}${daemon}</div><div style="margin-top:18px">${about}</div>`;
}

// ------------------------------------------------------------------ первый запуск

function renderOnboarding() {
  const info = state.info;
  const root = state.onboarding.root;
  const dataDir = state.onboarding.dataDir || (info && info.data_dir);
  const busy = state.busy;

  return `<div class="onboard">
    <div>
      <div class="onboard-kicker">SynctMusic · локальная синхронизация</div>
      <h1 class="onboard-title">Музыка живёт<br>там, где вы её<br><em>оставили</em>.</h1>
      <p class="onboard-lead">Одна и та же библиотека на телефоне и компьютере. Без аккаунтов, облака и подписок — только ваши файлы и локальная сеть.</p>
      <div class="principles">
        <div class="principle"><span class="num">01</span><div><b>Файлы не переезжают</b>SynctMusic читает папку на месте и не меняет структуру коллекции.</div></div>
        <div class="principle"><span class="num">02</span><div><b>Удаления обратимы</b>Копия живёт в хранилище 30 дней, решение всегда за вами.</div></div>
        <div class="principle"><span class="num">03</span><div><b>Обложки и теги — вместе с файлами</b>Правки из AIMP и Poweramp доезжают до второго устройства.</div></div>
      </div>
    </div>
    <div class="setup-card">
      <h2>Начнём с папки</h2>
      <p class="hint">Выберите каталог, где уже лежит музыка. Первый проход считает хеши и ничего не записывает в файлы.</p>
      <div class="picked ${root ? '' : 'is-empty'}">${icon('folder')}<span>${esc(root || 'Папка не выбрана')}</span></div>
      <button class="btn" data-act="pick-root" ${busy ? 'disabled' : ''}>Выбрать папку</button>

      <div class="picked" style="margin-top:12px">${icon('box')}<span>${esc(dataDir || 'Папка данных по умолчанию')}</span></div>
      <button class="btn btn--ghost btn--sm" data-act="pick-data" ${busy ? 'disabled' : ''}>Изменить служебную папку</button>

      <div class="setup-foot">
        <div class="setup-note">Здесь будут индекс и хранилище удалённого. Музыку не трогаем.</div>
        <button class="btn btn--primary" data-act="init" ${!root || busy ? 'disabled' : ''}>${busy ? 'Индексация…' : 'Начать'}</button>
      </div>
      ${busy ? `<div class="setup-progress"><span class="spinner"></span>Читаем файлы и считаем хеши. Большая библиотека может занять несколько минут.</div>` : `<div class="setup-progress">${icon('phone')}<span>Телефон подключите следующим шагом — код появится в разделе «Сопряжение».</span></div>`}
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
      <button class="btn btn--ghost" data-modal="cancel">Отмена</button>
      <button class="btn ${modal.danger ? 'btn--danger' : 'btn--primary'}" data-modal="confirm">${esc(modal.label || 'Подтвердить')}</button>
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

async function runBusy(task, fallback = 'Готово') {
  state.busy = true;
  render();
  try {
    const message = await task();
    await refreshStatus();
    toast(message || fallback);
  } catch (error) {
    toast(errText(error), 'bad');
  } finally {
    state.busy = false;
    render();
  }
}

const daemonCommand = (name, payload, fallback) => runBusy(async () => {
  const result = await invoke('daemon_action', { name, payload: payload || null });
  return (result && result.message) || fallback;
}, fallback);

async function startAndWait() {
  const started = await invoke('start_daemon');
  for (let attempt = 0; attempt < 12; attempt += 1) {
    await sleep(450);
    await refreshStatus();
    if (state.running) return started;
  }
  throw new Error('демон не ответил за 5 секунд — посмотрите лог в настройках');
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
    toast(`Библиотека проиндексирована: ${num(result.files)} ${plural(result.files, 'файл', 'файла', 'файлов')}`);
    try {
      await startAndWait();
      toast('Демон запущен, папка под наблюдением');
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
      title: 'Остановить демон?',
      text: 'Чтобы сменить папку, демон нужно остановить — он держит библиотеку открытой. Текущая папка и журнал останутся на месте.',
      label: 'Остановить и выбрать',
      danger: false,
      run: async () => {
        await runBusy(async () => {
          const result = await invoke('stop_daemon');
          return (result && result.message) || 'Демон остановлен';
        });
        await pickNewLibrary();
      },
    });
    return;
  }
  await pickNewLibrary();
}

async function pickNewLibrary() {
  const folder = await invoke('pick_folder', { title: 'Новая папка с музыкой' });
  if (!folder) return;
  await runBusy(async () => {
    await invoke('change_library', { folder });
    await refreshInfo();
    state.lastHtml = '';
    return 'Библиотека переключена';
  });
  try {
    await startAndWait();
    toast('Демон запущен с новой папкой');
  } catch (error) {
    toast(errText(error), 'warn');
  }
}

async function handleAction(action, dataset) {
  const path = dataset.path;
  switch (action) {
    case 'sync':
      await daemonCommand('sync', null, 'Синхронизация завершена');
      break;
    case 'scan':
      await daemonCommand('scan', null, 'Сканирование завершено');
      break;
    case 'pause':
      await daemonCommand('pause', { paused: true }, 'Пауза включена');
      break;
    case 'resume':
      await daemonCommand('pause', { paused: false }, 'Синхронизация возобновлена');
      break;
    case 'disk-check':
      await daemonCommand('disk_check', null, 'Проверка диска завершена');
      break;
    case 'gc':
      openConfirm({
        title: 'Очистить хранилище?',
        text: 'Будут удалены только копии, срок хранения которых истёк. Недавние удаления останутся доступными.',
        label: 'Очистить',
        danger: true,
        run: () => daemonCommand('gc', null, 'Хранилище очищено'),
      });
      break;
    case 'pending-only':
      await daemonCommand('pending_resolve', { path, action: 'only_here' }, 'Файл останется только здесь');
      break;
    case 'pending-forget':
      await daemonCommand('pending_forget', { path }, 'Запись убрана из очереди');
      break;
    case 'pending-everywhere':
      openConfirm({
        title: 'Удалить на всех устройствах?',
        text: 'Файл уже исчез с этого диска. Подтвердите удаление везде — копия останется в хранилище 30 дней и её можно будет вернуть.',
        path,
        label: 'Удалить везде',
        danger: true,
        run: () => daemonCommand('pending_resolve', { path, action: 'delete_everywhere' }, 'Удаление подтверждено'),
      });
      break;
    case 'restore':
      await daemonCommand('restore', { path }, 'Файл восстановлен');
      break;
    case 'start-daemon':
      await runBusy(async () => {
        const result = await startAndWait();
        return (result && result.started) ? 'Демон запущен' : ((result && result.message) || 'Демон уже работает');
      });
      break;
    case 'stop-daemon':
      openConfirm({
        title: 'Остановить демон?',
        text: 'Наблюдение за папкой и синхронизация прекратятся, пока вы снова не запустите SynctMusic. Файлы не пострадают.',
        label: 'Остановить',
        danger: true,
        run: async () => {
          await runBusy(async () => {
            const result = await invoke('stop_daemon');
            return (result && result.message) || 'Демон остановлен';
          });
        },
      });
      break;
    case 'pair-refresh':
      state.pair = null;
      await loadPair(true);
      render();
      if (!state.pairError) toast('Код обновлён');
      break;
    case 'copy-code':
      if (state.pair) await copyText(state.pair.code);
      break;
    case 'copy-device': {
      const snap = snapshot();
      if (snap.device) await copyText(snap.device);
      break;
    }
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
      const folder = await invoke('pick_folder', { title: 'Папка с музыкой' });
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
  await daemonCommand('pair', { code }, 'Сопряжение запущено');
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
  if (!hasBridge()) {
    $('#view').innerHTML = `<section class="panel">${emptyState('alert', 'Нет доступа к ядру Tauri', 'Запустите приложение через собранный бинарник SynctMusic, а не как обычную веб-страницу.')}</section>`;
    return;
  }
  state.started = true;

  try {
    await refreshInfo();
  } catch (error) {
    $('#view').innerHTML = `<section class="panel">${emptyState('alert', 'Не удалось прочитать настройки', errText(error))}</section>`;
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
      // Ничего страшного: покажем состояние «демон остановлен» и кнопку запуска.
    }
  }

  if (state.running) await loadPair();
  render();
  setInterval(poll, 3000);
}

// Небольшой внутренний хук: через него удобно проверять состояние и
// переключать разделы при отладке (в том числе из scripts/).
window.__syncth = { state, boot, render, navigate, refreshStatus, handleAction };

boot();
