const I18N = (() => {
  const ru = {
    'start.title': 'Папка с музыкой',
    'start.text': 'Выберите папку на этом устройстве. Файлы передаются напрямую между устройствами — без облака и серверов.',
    'start.action': 'Выбрать папку',
    'start.scanning': 'Сканируем…',
    'start.local': 'Файлы остаются на устройстве',
    'start.version': 'версия {version}',
    'map.this': 'Это устройство',
    'map.peer': 'Другое устройство',
    'map.note': 'Напрямую, без облака',
    'dialog.pick': 'Выберите папку с музыкой',
    'workspace.label': 'Библиотека',
    'error.bridge': 'Нет доступа к приложению',
    'error.init': 'Не удалось открыть папку: {error}',
    'error.unknown': 'Неизвестная ошибка',
    'window.minimize': 'Свернуть',
    'window.maximize': 'Развернуть',
    'window.close': 'Закрыть',
  };

  const en = {
    'start.title': 'Your music folder',
    'start.text': 'Choose a folder on this device. Files move directly between devices — no cloud, no servers.',
    'start.action': 'Choose folder',
    'start.scanning': 'Scanning…',
    'start.local': 'Files stay on this device',
    'start.version': 'version {version}',
    'map.this': 'This device',
    'map.peer': 'Other device',
    'map.note': 'Direct, no cloud',
    'dialog.pick': 'Choose the music folder',
    'workspace.label': 'Library',
    'error.bridge': 'The application bridge is unavailable',
    'error.init': 'Could not open the folder: {error}',
    'error.unknown': 'Unknown error',
    'window.minimize': 'Minimize',
    'window.maximize': 'Maximize',
    'window.close': 'Close',
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
