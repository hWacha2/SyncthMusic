const I18N = (() => {
  const ru = {
    'start.title': 'Где лежит музыка?',
    'start.text': 'Укажите папку с библиотекой. SynctMusic ничего не переносит, не переименовывает и не отправляет в облако.',
    'start.action': 'Выбрать папку',
    'start.scanning': 'Сканируем папку…',
    'start.local': 'Папка остаётся на этом устройстве',
    'start.version': 'версия {version}',
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
    'start.title': 'Where does the music live?',
    'start.text': 'Choose the folder that holds your library. SynctMusic never moves, renames, or uploads anything.',
    'start.action': 'Choose folder',
    'start.scanning': 'Scanning folder…',
    'start.local': 'The folder stays on this device',
    'start.version': 'version {version}',
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
