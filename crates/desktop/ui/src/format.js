import { lang, t } from './i18n/index.js';

// Числа и размеры показываем в привычном для языка виде: 1,4 ГБ / 1.4 GB.
export function sizeParts(bytes) {
  const value = Number(bytes) || 0;
  const units = lang.value === 'ru'
    ? ['Б', 'КБ', 'МБ', 'ГБ', 'ТБ']
    : ['B', 'KB', 'MB', 'GB', 'TB'];
  if (value < 1024) return { value: String(value), unit: units[0] };
  let size = value / 1024;
  let unit = 1;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return {
    value: size >= 10 ? String(Math.round(size)) : size.toFixed(1),
    unit: units[unit],
  };
}

export function formatSize(bytes) {
  const parts = sizeParts(bytes);
  const value = lang.value === 'ru' ? parts.value.replace('.', ',') : parts.value;
  return `${value} ${parts.unit}`;
}

export function number(value) {
  return (Number(value) || 0).toLocaleString(lang.value === 'ru' ? 'ru-RU' : 'en-US');
}

export function ago(ms) {
  const value = Number(ms) || 0;
  if (!value) return t('time.never');
  const seconds = Math.max(0, (Date.now() - value) / 1000);
  if (seconds < 45) return t('time.now');
  if (seconds < 3600) return t('time.minutes', { count: Math.max(1, Math.round(seconds / 60)) });
  if (seconds < 86400) return t('time.hours', { count: Math.max(1, Math.round(seconds / 3600)) });
  return t('time.days', { count: Math.max(1, Math.round(seconds / 86400)) });
}

export function basename(path) {
  const text = String(path || '');
  const parts = text.split(/[\\/]/).filter(Boolean);
  return parts.length ? parts[parts.length - 1] : text;
}

// Папка, в которой лежит путь. Для файла в корне библиотеки — пустая строка.
export function dirname(path) {
  const text = String(path || '');
  const index = Math.max(text.lastIndexOf('/'), text.lastIndexOf('\\'));
  return index > 0 ? text.slice(0, index) : '';
}
