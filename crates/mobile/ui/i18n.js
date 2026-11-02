const I18N = (() => {
  const ru = {
    'start.title': 'Папка с музыкой',
    'start.text': 'Выберите папку на этом устройстве. Файлы передаются напрямую между устройствами — без облака и серверов.',
    'start.action': 'Выбрать папку',
    'start.scanning': 'Сканируем…',
    'map.this': 'Это устройство',
    'map.peer': 'Другое устройство',
    'map.note': 'Напрямую, без облака',
    'workspace.label': 'Библиотека',
    'picker.title': 'Выберите папку',
    'picker.up': 'Наверх',
    'picker.use': 'Выбрать эту папку',
    'picker.empty': 'В этой папке нет вложенных папок.',
    'picker.loading': 'Читаем папки…',
    'picker.retry': 'Повторить',
    'picker.cancel': 'Отмена',
    'error.bridge': 'Нет доступа к приложению',
    'error.browse': 'Не удалось прочитать папку: {error}',
    'error.init': 'Не удалось открыть папку: {error}',
    'error.unknown': 'Неизвестная ошибка',
  };

  const en = {
    'start.title': 'Your music folder',
    'start.text': 'Choose a folder on this device. Files move directly between devices — no cloud, no servers.',
    'start.action': 'Choose folder',
    'start.scanning': 'Scanning…',
    'map.this': 'This device',
    'map.peer': 'Other device',
    'map.note': 'Direct, no cloud',
    'workspace.label': 'Library',
    'picker.title': 'Choose a folder',
    'picker.up': 'Up',
    'picker.use': 'Use this folder',
    'picker.empty': 'There are no folders inside.',
    'picker.loading': 'Reading folders…',
    'picker.retry': 'Try again',
    'picker.cancel': 'Cancel',
    'error.bridge': 'The application bridge is unavailable',
    'error.browse': 'Could not read the folder: {error}',
    'error.init': 'Could not open the folder: {error}',
    'error.unknown': 'Unknown error',
  };

  const lang = (navigator.language || 'en').toLowerCase().startsWith('ru') ? 'ru' : 'en';
  const dict = lang === 'ru' ? ru : en;

  function t(key, vars = {}) {
    const text = dict[key] ?? key;
    return text.replace(/\{(\w+)\}/g, (_, name) => (vars[name] ?? ''));
  }

  function apply() {
    document.documentElement.lang = lang;
    document.querySelectorAll('[data-i18n]').forEach((node) => {
      node.textContent = t(node.dataset.i18n);
    });
    document.querySelectorAll('[data-i18n-aria]').forEach((node) => {
      node.setAttribute('aria-label', t(node.dataset.i18nAria));
    });
  }

  return { lang, t, apply };
})();

window.I18N = I18N;
