import { reactive } from 'vue';
import { hasBridge, invoke } from './bridge.js';
import { chosen, setLang, t } from './i18n/index.js';
import { isWorkspaceGeneration, nextWorkspaceGeneration } from './workspace/generation.js';

const TOAST_KEY = 'syncth.toast_seconds';
const TOAST_CHOICES = [3, 5, 8, 12, 20];
const TOAST_DEFAULT = 8;

function loadToastSeconds() {
  try {
    const value = Number(localStorage.getItem(TOAST_KEY));
    return TOAST_CHOICES.includes(value) ? value : TOAST_DEFAULT;
  } catch {
    return TOAST_DEFAULT;
  }
}

/// Меняет, сколько живут всплывающие уведомления. Уже показанные тосты
/// доживают со своим таймером — настройка влияет на следующие.
export function setToastSeconds(value) {
  if (!TOAST_CHOICES.includes(value)) return;
  state.toastSeconds = value;
  try {
    localStorage.setItem(TOAST_KEY, String(value));
  } catch {
    // Приватный режим или запрет хранилища — просто не запоминаем.
  }
}

// Общее состояние приложения. Экраны читают его напрямую, а действия
// меняют через функции ниже — так проще переносить старую логику по кускам.
export const state = reactive({
  view: 'loading',
  langReturn: null,
  info: null,
  root: null,
  phone: { linked: false },
  lan: [],
  adb: [],
  adbError: '',
  discovering: false,
  discovered: false,
  busy: '',
  note: '',
  error: false,
  phoneBusy: '',
  workspace: {
    data: null,
    // Данные устарели после сброса истории: пока идёт проверка, показываем
    // прогресс вместо старых цифр, но нижний индикатор связи не сбрасываем.
    stale: false,
    refreshing: false,
    error: '',
    statusError: '',
    action: '',
    tab: 'overview',
    changesSide: 'remote',
    changesSort: 'folders',
    note: '',
    noteError: false,
    progress: {
      stage: '',
      done: 0,
      total: 0,
      done_bytes: 0,
      total_bytes: 0,
      speed: 0,
    },
  },
  modal: null,
  // Сколько секунд висят всплывающие уведомления. Настройка переживает
  // перезапуск: это личное предпочтение, а не часть библиотеки.
  toastSeconds: loadToastSeconds(),
});

// Ответы старых запросов не должны перетирать состояние нового устройства
// или новой папки: все они сверяются с общим номером поколения.
let workspacePendingRefresh = false;
let polling = false;

export function errorMessage(error) {
  if (!error) return t('error.unknown');
  if (typeof error === 'string') return error;
  return error.message || String(error);
}

export function resetWorkspace() {
  nextWorkspaceGeneration();
  workspacePendingRefresh = false;
  state.workspace.data = null;
  state.workspace.stale = false;
  state.workspace.error = '';
  state.workspace.statusError = '';
}

// Порядок экранов как в старом интерфейсе: язык → папка → устройства →
// рабочая область. Пока человек не выбрал язык, библиотеку не трогаем.
export async function boot() {
  if (!chosen.value) {
    state.view = 'language';
    return;
  }
  if (!hasBridge) {
    state.view = 'error';
    state.note = t('error.bridge');
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
    state.note = errorMessage(error);
    return;
  }
  startPolling();
  if (state.view === 'devices') await discoverAll();
  if (state.view === 'workspace') {
    await reconnect();
    if (state.view === 'workspace') await refreshWorkspace();
  }
}

/// Открывает выбор языка поверх текущего экрана. После выбора возвращаемся
/// туда же: перезапускать загрузку и рвать соединение с телефоном не нужно.
export function openLanguage() {
  if (state.view === 'language') return;
  state.langReturn = state.view;
  state.view = 'language';
}

export function chooseLanguage(next) {
  setLang(next);
  const back = state.langReturn;
  state.langReturn = null;
  if (back) {
    state.view = back;
    return;
  }
  boot();
}

export function closeLanguage() {
  if (!state.langReturn) return;
  state.view = state.langReturn;
  state.langReturn = null;
}

// ---------------------------------------------------------------- setup

export async function discoverAll(keepNote = false) {
  if (state.discovering || !hasBridge) return;
  state.discovering = true;
  if (!keepNote) {
    state.error = false;
    state.note = '';
  }
  const [lan, adb] = await Promise.allSettled([
    invoke('discover_agents'),
    invoke('discover_adb'),
  ]);
  state.lan = lan.status === 'fulfilled' ? (lan.value?.devices || []) : [];
  state.adb = adb.status === 'fulfilled' ? (adb.value?.devices || []) : [];
  state.adbError = adb.status === 'rejected'
    ? t('setup.adb_error', { error: errorMessage(adb.reason) })
    : '';
  state.discovered = true;
  state.discovering = false;
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
  if (result.needs_root) {
    await requestRootChange(true);
  } else if (state.view === 'workspace') {
    await refreshWorkspace();
  }
}

export async function connectLan(dev, addr) {
  if (state.busy || !hasBridge) return;
  state.busy = `lan:${dev}`;
  state.error = false;
  state.note = t('setup.wait_pair');
  try {
    const result = await invoke('agent_connect', { addr, dev });
    state.busy = '';
    await applyLink(result);
  } catch (error) {
    state.busy = '';
    state.error = true;
    state.note = t('phone.error', { error: errorMessage(error) });
  }
}

export async function connectAdb(serial) {
  if (state.busy || !hasBridge) return;
  state.busy = `adb:${serial}`;
  state.error = false;
  state.note = t('setup.wait_pair');
  try {
    const result = await invoke('agent_connect_adb', { serial });
    state.busy = '';
    await applyLink(result);
  } catch (error) {
    state.busy = '';
    state.error = true;
    state.note = t('phone.error', { error: errorMessage(error) });
  }
}

export async function cancelConnect() {
  if (!hasBridge) return;
  try {
    await invoke('agent_cancel');
  } catch {
    // Соединение могло уже закрыться само.
  }
}

export async function chooseFolder() {
  if (state.busy || !hasBridge) return;
  state.error = false;
  state.note = '';

  let folder;
  try {
    folder = await invoke('pick_folder', { title: t('library.dialog') });
  } catch (error) {
    state.error = true;
    state.note = t('error.init', { error: errorMessage(error) });
    return;
  }
  if (!folder) return;

  state.busy = 'folder';
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
    state.note = t('error.init', { error: errorMessage(error) });
  } finally {
    state.busy = '';
  }
  if (!opened) return;

  await closeWorkspace();
  // Сначала локальная папка, потом телефон: после выбора папки показываем
  // устройства. Если телефон уже был сопряжён, пробуем вернуться к нему.
  if (state.phone.linked) {
    state.view = 'workspace';
    await reconnect();
  } else {
    state.view = 'devices';
    await discoverAll();
  }
}

export function openLibrary() {
  if (state.busy) return;
  closeWorkspace();
  state.view = 'library';
  state.error = false;
  state.note = '';
}

export async function openDevices() {
  if (state.busy) return;
  await closeWorkspace();
  state.view = 'devices';
  state.error = false;
  state.note = '';
  await discoverAll();
}

export async function openWorkspace() {
  if (state.busy) return;
  state.view = 'workspace';
  state.error = false;
  state.note = '';
  state.workspace.tab = 'overview';
  await refreshWorkspace();
}

export async function closeWorkspace() {
  resetWorkspace();
  state.workspace.note = '';
  state.workspace.action = '';
  if (!hasBridge) return;
  try {
    await invoke('workspace_close');
  } catch {
    // Сеанс мог быть уже закрыт вместе с телефоном.
  }
}

export async function requestRootChange(quiet = false) {
  if (state.phoneBusy || !hasBridge) return;
  state.phoneBusy = 'root';
  if (!quiet) state.note = t('phone.wait_root');
  try {
    const result = await invoke('agent_request_root_change');
    if (state.phone.linked) state.phone.root = result?.root || state.phone.root;
    state.note = '';
    state.error = false;
    await closeWorkspace();
    if (state.view === 'workspace') await refreshWorkspace();
  } catch (error) {
    state.error = true;
    state.note = t('phone.error', { error: errorMessage(error) });
  } finally {
    state.phoneBusy = '';
  }
}

// ---------------------------------------------------------------- workspace data

export async function refreshWorkspace(options = {}) {
  if (!hasBridge) return;
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
  const gen = nextWorkspaceGeneration();
  startProgressWatch();
  try {
    const data = await invoke('workspace_refresh');
    if (isWorkspaceGeneration(gen)) {
      state.workspace.data = data;
      state.workspace.stale = false;
    }
  } catch (error) {
    if (isWorkspaceGeneration(gen)) state.workspace.error = errorMessage(error);
  } finally {
    state.workspace.refreshing = false;
    stopProgressWatch();
    if (workspacePendingRefresh) {
      workspacePendingRefresh = false;
      refreshWorkspace(options);
    }
  }
}

// ---------------------------------------------------------------- progress

let progressTimer = null;
let progressSample = null;

function applyProgress(value) {
  const stage = value && typeof value.stage === 'string' ? value.stage : '';
  const doneBytes = Number(value?.done_bytes) || 0;
  const totalBytes = Number(value?.total_bytes) || 0;
  const now = Date.now();

  // Скорость считаем по байтам: файлы разного размера, и по штукам она врёт.
  // Сглаживаем, чтобы цифра не дёргалась на каждом опросе. Уменьшение байтов
  // означает новый этап (обход → проверка) — начинаем замер заново.
  let speed = 0;
  if (
    stage &&
    progressSample &&
    progressSample.stage === stage &&
    doneBytes >= progressSample.bytes
  ) {
    const dt = (now - progressSample.at) / 1000;
    const delta = doneBytes - progressSample.bytes;
    if (dt >= 0.4) {
      const instant = delta / dt;
      speed = progressSample.speed
        ? progressSample.speed * 0.7 + instant * 0.3
        : instant;
      progressSample = { stage, bytes: doneBytes, at: now, speed };
    } else {
      speed = progressSample.speed || 0;
    }
  } else {
    progressSample = stage ? { stage, bytes: doneBytes, at: now, speed: 0 } : null;
  }

  state.workspace.progress = {
    stage,
    done: Number(value?.done) || 0,
    total: Number(value?.total) || 0,
    done_bytes: doneBytes,
    total_bytes: totalBytes,
    speed,
  };
}

/// Ход проверки живёт в отдельном мьютексе на бэкенде, поэтому его можно
/// опрашивать, пока `workspace_refresh` ещё считает файлы.
export function startProgressWatch() {
  if (progressTimer || !hasBridge) return;
  const tick = async () => {
    try {
      applyProgress(await invoke('workspace_progress'));
    } catch {
      // Проверка могла закончиться между тиками — полосу уберёт stop.
    }
  };
  tick();
  progressTimer = setInterval(tick, 300);
}

export function stopProgressWatch() {
  if (progressTimer) clearInterval(progressTimer);
  progressTimer = null;
  applyProgress(null);
}

// ---------------------------------------------------------------- status

/// Закрывает текущий сеанс, не трогая сохранённую пару. Телефон снова
/// появится в списке устройств, а подключение к нему пройдёт без
/// подтверждения на телефоне.
export async function disconnectPhone() {
  if (state.phoneBusy || !hasBridge) return;
  state.phoneBusy = 'disconnect';
  state.note = '';
  state.error = false;
  try {
    await invoke('agent_disconnect');
    await closeWorkspace();
    state.phone = { linked: false };
    state.view = 'devices';
    await discoverAll(true);
    state.note = t('phone.disconnected');
  } catch (error) {
    state.error = true;
    state.note = t('phone.error', { error: errorMessage(error) });
  } finally {
    state.phoneBusy = '';
  }
}

export async function reconnect() {
  if (!hasBridge || state.busy || !state.phone.linked) return;
  state.busy = 'reconnect';
  state.error = false;
  state.note = t('setup.connecting');
  try {
    const result = await invoke('agent_reconnect');
    state.busy = '';
    await applyLink(result);
  } catch (error) {
    state.busy = '';
    state.error = true;
    state.note = t('phone.error', { error: errorMessage(error) });
    state.phone = { linked: false };
    state.view = 'devices';
    await discoverAll(true);
  }
}

async function pollStatus() {
  if (!hasBridge || !state.phone.linked) return;
  if (state.busy || state.phoneBusy) return;
  if (state.view !== 'workspace' && state.view !== 'library') return;

  let status;
  try {
    status = await invoke('agent_status');
  } catch (error) {
    // Без этого сообщения устойчивый сбой бэкенда выглядел бы как
    // «замерший» интерфейс без причины.
    state.workspace.statusError = errorMessage(error);
    return;
  }
  if (!status || status.busy || state.phoneBusy) return;

  if (!status.connected) {
    state.phone = { linked: false };
    resetWorkspace();
    state.view = 'devices';
    state.error = true;
    state.note = t('phone.lost');
    await closeWorkspace();
    await discoverAll(true);
    return;
  }

  if (status.root !== undefined && status.root !== state.phone.root) {
    state.phone.root = status.root;
    resetWorkspace();
    if (state.view === 'workspace') refreshWorkspace();
  }
  if (status.name) state.phone.name = status.name;
}

async function pollWorkspace() {
  if (!hasBridge || state.view !== 'workspace') return;
  if (!state.workspace.data || state.workspace.refreshing || state.workspace.action || state.modal) return;
  const gen = nextWorkspaceGeneration();
  try {
    const data = await invoke('workspace_status');
    if (!isWorkspaceGeneration(gen)) return;
    state.workspace.data = data;
    state.workspace.statusError = '';
  } catch (error) {
    if (!isWorkspaceGeneration(gen)) return;
    state.workspace.statusError = errorMessage(error);
  }
}

function startPolling() {
  if (polling) return;
  polling = true;
  setInterval(pollStatus, 2500);
  setInterval(pollWorkspace, 3200);
}

// ---------------------------------------------------------------- modal

export function openModal(options) {
  state.modal = options;
}

export function closeModal() {
  state.modal = null;
}

export function confirmModal() {
  const modal = state.modal;
  state.modal = null;
  if (modal?.onConfirm) modal.onConfirm();
}
