import { computed, ref } from 'vue';
import en from './en.js';
import ru from './ru.js';

const STORAGE_KEY = 'syncth.lang';

function stored() {
  try {
    const value = localStorage.getItem(STORAGE_KEY);
    return value === 'ru' || value === 'en' ? value : null;
  } catch {
    // Приватный режим или заблокированное хранилище: выбор живёт до перезапуска.
    return null;
  }
}

function systemLang() {
  const tag = (navigator.language || 'en').toLowerCase();
  return tag.startsWith('ru') ? 'ru' : 'en';
}

export const lang = ref(stored() || systemLang());
export const chosen = ref(stored() !== null);

const dict = computed(() => (lang.value === 'ru' ? ru : en));

export function t(key, vars = {}) {
  const text = dict.value[key] ?? key;
  return text.replace(/\{(\w+)\}/g, (_, name) => vars[name] ?? '');
}

export function setLang(next) {
  lang.value = next === 'ru' ? 'ru' : 'en';
  chosen.value = true;
  try {
    localStorage.setItem(STORAGE_KEY, lang.value);
  } catch {
    // Выбор всё равно остаётся в памяти до перезапуска.
  }
  document.documentElement.lang = lang.value;
}

document.documentElement.lang = lang.value;
