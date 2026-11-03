import { reactive } from 'vue';
import { hasBridge, invoke } from './bridge.js';
import { chosen, setLang, t } from './i18n/index.js';

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
  busy: '',
  note: '',
  error: false,
  phoneBusy: '',
  workspace: {
    data: null,
    refreshing: false,
    error: '',
    statusError: '',
    action: '',
    tab: 'overview',
    note: '',
    noteError: false,
  },
  modal: null,
});

// Номер поколения данных рабочей области. Ответы старых запросов не должны
// перетирать состояние нового устройства или новой папки.
let workspaceGen = 0;
let workspacePendingRefresh = false;
let polling = false;

export function errorMessage(error) {
  if (!error) return t('error.unknown');
  if (typeof error === 'string') return error;
  return error.message || String(error);
}

export function resetWorkspace() {
  workspaceGen += 1;
  workspacePendingRefresh = false;
  state.workspace.data = null;
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
  const gen = ++workspaceGen;
  try {
    const data = await invoke('workspace_refresh');
    if (gen === workspaceGen) state.workspace.data = data;
  } catch (error) {
    if (gen === workspaceGen) state.workspace.error = errorMessage(error);
  } finally {
    state.workspace.refreshing = false;
    if (workspacePendingRefresh) {
      workspacePendingRefresh = false;
      refreshWorkspace(options);
    }
  }
}

// ---------------------------------------------------------------- status

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
  if (!status || status.busy) return;

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

function startPolling() {
  if (polling) return;
  polling = true;
  setInterval(pollStatus, 2500);
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
