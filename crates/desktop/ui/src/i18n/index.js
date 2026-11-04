import { computed, ref } from 'vue';
import en from './en.js';
import ru from './ru.js';

const STORAGE_KEY = 'syncth.lang';

// Список языков живёт отдельно от словарей: его читает и экран выбора, и
// маленькая переключалка в углу. Новый перевод достаточно добавить сюда и
// положить рядом словарь.
export const LANGUAGES = [
  { code: 'ru', label: 'Русский', short: 'RU' },
  { code: 'en', label: 'English', short: 'EN' },
];

const dicts = { ru, en };
const codes = new Set(LANGUAGES.map((item) => item.code));

function normalize(value) {
  return codes.has(value) ? value : null;
}

function stored() {
  try {
    return normalize(localStorage.getItem(STORAGE_KEY));
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
export const langShort = computed(
  () => LANGUAGES.find((item) => item.code === lang.value)?.short || lang.value.toUpperCase(),
);

const dict = computed(() => dicts[lang.value] || en);

export function t(key, vars = {}) {
  const text = dict.value[key] ?? key;
  return text.replace(/\{(\w+)\}/g, (_, name) => vars[name] ?? '');
}

export function setLang(next) {
  lang.value = normalize(next) || 'en';
  chosen.value = true;
  try {
    localStorage.setItem(STORAGE_KEY, lang.value);
  } catch {
    // Выбор всё равно остаётся в памяти до перезапуска.
  }
  document.documentElement.lang = lang.value;
}

document.documentElement.lang = lang.value;
