const I18N = (() => {
  const ru = {
    'wait.title': 'Ожидание подключения',
    'wait.hint': 'Откройте SynctMusic на компьютере.',
    'ready.title': 'Подключено',
    'agent.folder': 'Папка',
    'agent.change': 'Сменить папку',
    'agent.error': 'Ошибка: {error}',
    'pair.title': 'Подключить компьютер?',
    'pair.accept': 'Разрешить',
    'pair.reject': 'Отклонить',
    'picker.change': 'Папка',
    'picker.request': 'Компьютер просит выбрать папку.',
    'picker.hint': 'Выберите папку с музыкой на телефоне.',
    'picker.up': 'Наверх',
    'picker.use': 'Выбрать папку',
    'picker.empty': 'Внутри нет папок.',
    'picker.loading': 'Читаем…',
    'picker.retry': 'Повторить',
    'picker.cancel': 'Отмена',
    'error.bridge': 'Нет доступа к приложению',
    'error.browse': 'Не удалось прочитать папку: {error}',
    'error.root': 'Не удалось сохранить папку: {error}',
    'error.unknown': 'Неизвестная ошибка',
  };

  const en = {
    'wait.title': 'Waiting for connection',
    'wait.hint': 'Open SynctMusic on the computer.',
    'ready.title': 'Connected',
    'agent.folder': 'Folder',
    'agent.change': 'Change folder',
    'agent.error': 'Error: {error}',
    'pair.title': 'Connect this computer?',
    'pair.accept': 'Allow',
    'pair.reject': 'Decline',
    'picker.change': 'Folder',
    'picker.request': 'The computer asks you to choose a folder.',
    'picker.hint': 'Choose the music folder on this phone.',
    'picker.up': 'Up',
    'picker.use': 'Use folder',
    'picker.empty': 'No folders inside.',
    'picker.loading': 'Reading…',
    'picker.retry': 'Try again',
    'picker.cancel': 'Cancel',
    'error.bridge': 'The application bridge is unavailable',
    'error.browse': 'Could not read the folder: {error}',
    'error.root': 'Could not save the folder: {error}',
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
