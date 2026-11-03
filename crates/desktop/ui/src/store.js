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

export function errorMessage(error) {
  if (!error) return t('error.unknown');
  if (typeof error === 'string') return error;
  return error.message || String(error);
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
  }
}

export function chooseLanguage(next) {
  setLang(next);
  boot();
}

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
