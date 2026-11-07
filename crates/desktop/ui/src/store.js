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
    // Раскрытые вручную строки автоматического режима. Это состояние окна,
    // а не решение: если человек ничего не выбрал, действует авто-правило.
    manualOpen: [],
    // Корзина телефона: null — ещё не запрашивали, [] — пусто.
    phoneVault: null,
    phoneVaultError: '',
    phoneVaultLoading: false,
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

/// Ошибка настройки внутри рабочей области не должна уводить на отдельный
/// экран: показываем её тем же тостом, что и остальные действия.
function reportSetupError(text) {
  if (state.view === 'workspace') {
    state.workspace.note = text;
    state.workspace.noteError = true;
    return;
  }
  state.error = true;
  state.note = text;
}

export function resetWorkspace() {
  nextWorkspaceGeneration();
  workspacePendingRefresh = false;
  state.workspace.data = null;
  state.workspace.stale = false;
  state.workspace.error = '';
  state.workspace.statusError = '';
  state.workspace.manualOpen = [];
  state.workspace.phoneVault = null;
  state.workspace.phoneVaultError = '';
  state.workspace.phoneVaultLoading = false;
}

// Порядок экранов: язык → папка → рабочая область. Подключение телефона
// необязательно: оно живёт внутри рабочей области и не прерывает работу,
// если связи нет.
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
    state.phone = link?.linked ? { ...link, connected: false } : { linked: false };
    state.view = state.root ? 'workspace' : 'library';
  } catch (error) {
    state.view = 'error';
    state.note = errorMessage(error);
    return;
  }
  startPolling();
  if (state.view !== 'workspace') return;
  let connected = false;
  if (state.phone.linked) connected = await reconnect(true);
  if (state.view === 'workspace' && !connected) await refreshWorkspace();
}

export function chooseLanguage(next) {
  setLang(next);
  boot();
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
  // Переподключение из рабочей области не должно перебрасывать на «Обзор»:
  // вкладку сбрасываем только при входе с другого экрана или когда телефон
  // подключён со вкладки «Устройства».
  const wasWorkspace = state.view === 'workspace';
  const fromDevices = wasWorkspace && state.workspace.tab === 'devices';
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
  if (!wasWorkspace || fromDevices) state.workspace.tab = 'overview';
  state.view = state.root ? 'workspace' : 'library';
  if (result.needs_root) {
    state.note = t('library.waiting_phone');
    await requestRootChange(true);
    return;
  }
  if (state.view === 'workspace') await refreshWorkspace();
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
    state.note = '';
    reportSetupError(t('phone.error', { error: errorMessage(error) }));
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
    state.note = '';
    reportSetupError(t('phone.error', { error: errorMessage(error) }));
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
  state.workspace.note = '';
  state.workspace.noteError = false;

  let folder;
  try {
    folder = await invoke('pick_folder', { title: t('library.dialog') });
  } catch (error) {
    reportSetupError(t('error.init', { error: errorMessage(error) }));
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
    reportSetupError(t('error.init', { error: errorMessage(error) }));
  } finally {
    state.busy = '';
  }
  if (!opened) return;

  await closeWorkspace();
  // Новая папка сразу открывается в рабочей области. Телефон подключается
  // отдельно и не является условием входа.
  state.view = 'workspace';
  await refreshWorkspace();
}

/// Подключение живёт на отдельной вкладке рабочей области. Выбор вкладки
/// не закрывает сеанс: устройства можно смотреть, не выходя из библиотеки.
export function openDevices() {
  if (state.busy) return;
  state.error = false;
  state.note = '';
  if (state.view !== 'workspace' && state.root) state.view = 'workspace';
  state.workspace.tab = 'devices';
}

export async function openWorkspace() {
  if (state.busy) return;
  state.view = 'workspace';
  state.error = false;
  state.note = '';
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
  if (!quiet && state.view !== 'workspace') state.note = t('phone.wait_root');
  try {
    const result = await invoke('agent_request_root_change');
    if (state.phone.linked) state.phone.root = result?.root || state.phone.root;
    state.note = '';
    state.error = false;
    await closeWorkspace();
    if (state.view === 'workspace') await refreshWorkspace();
  } catch (error) {
    reportSetupError(t('phone.error', { error: errorMessage(error) }));
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
      // Полная проверка пересобирает план: старые раскрытые строки к нему
      // уже не относятся. Явные решения при этом остаются в базе.
      state.workspace.manualOpen = [];
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

/// Закрывает текущий сеанс, не трогая сохранённую пару. Рабочая область
/// остаётся открытой: телефон можно подключить снова из «Обзора» или
/// «Настроек», а следующий сеанс пройдёт без подтверждения.
export async function disconnectPhone() {
  if (state.phoneBusy || !hasBridge) return;
  state.phoneBusy = 'disconnect';
  state.note = '';
  state.error = false;
  try {
    const link = await invoke('agent_disconnect');
    await closeWorkspace();
    state.phone = link?.linked
      ? { ...link, connected: false }
      : { linked: false };
    state.view = state.root ? 'workspace' : 'library';
    if (state.view === 'workspace') {
      await refreshWorkspace();
      state.workspace.note = t('phone.disconnected');
      state.workspace.noteError = false;
    } else {
      state.note = t('phone.disconnected');
    }
  } catch (error) {
    reportSetupError(t('phone.error', { error: errorMessage(error) }));
  } finally {
    state.phoneBusy = '';
  }
}

/// Переподключение к сохранённому телефону. При неудаче рабочая область
/// остаётся на месте: связь — необязательная часть, а не условие входа.
export async function reconnect(quiet = false) {
  if (!hasBridge || state.busy || !state.phone.linked) return false;
  state.busy = 'reconnect';
  state.error = false;
  if (!quiet) state.note = t('setup.connecting');
  try {
    const result = await invoke('agent_reconnect');
    state.busy = '';
    await applyLink(result);
    return true;
  } catch (error) {
    state.busy = '';
    state.phone = { ...state.phone, linked: true, connected: false };
    if (!quiet) reportSetupError(t('phone.error', { error: errorMessage(error) }));
    return false;
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
    const wasConnected = state.phone.connected === true;
    state.phone = { ...state.phone, linked: true, connected: false };
    if (wasConnected) {
      state.workspace.note = t('phone.lost');
      state.workspace.noteError = true;
      await refreshWorkspace({ keepNote: true });
    }
    return;
  }

  const wasDisconnected = state.phone.connected !== true;
  state.phone = { ...state.phone, connected: true };
  if (wasDisconnected && state.view === 'workspace') {
    // Сеанс появился без нашей команды (например, после ручного
    // переподключения) — пересобираем данные рабочей области.
    await refreshWorkspace();
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
