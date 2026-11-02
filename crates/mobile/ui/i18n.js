const I18N = (() => {
  const ru = {
    'start.title': 'Где лежит музыка?',
    'start.text': 'Укажите папку с библиотекой. SynctMusic ничего не переносит, не переименовывает и не отправляет в облако.',
    'start.action': 'Выбрать папку',
    'start.scanning': 'Сканируем папку…',
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
    'start.title': 'Where does the music live?',
    'start.text': 'Choose the folder that holds your library. SynctMusic never moves, renames, or uploads anything.',
    'start.action': 'Choose folder',
    'start.scanning': 'Scanning folder…',
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
