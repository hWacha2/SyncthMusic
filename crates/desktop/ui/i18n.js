const I18N = (() => {
  const ru = {
    'setup.title': 'Устройства',
    'setup.lan': 'Локальная сеть',
    'setup.adb': 'USB (ADB)',
    'setup.refresh': 'Обновить',
    'setup.change_folder': 'Сменить папку на ПК',
    'setup.searching': 'Поиск…',
    'setup.empty_lan': 'Устройства не найдены.',
    'setup.empty_adb': 'Нет устройств.',
    'setup.connect': 'Подключить',
    'setup.connecting': 'Подключение…',
    'setup.cancel': 'Отмена',
    'setup.wait_pair': 'Подтвердите подключение на телефоне.',
    'setup.unauthorized': 'Разрешите отладку',
    'setup.offline': 'Недоступно',
    'setup.adb_error': 'ADB: {error}',
    'library.title': 'Папка с музыкой',
    'library.hint': 'Выберите папку на этом компьютере.',
    'library.action': 'Выбрать папку',
    'library.choosing': 'Открываем…',
    'library.dialog': 'Выберите папку с музыкой',
    'library.waiting_phone': 'Выберите папку на телефоне.',
    'library.back': 'Назад к устройствам',
    'workspace.label': 'Библиотека',
    'workspace.connect': 'Подключить телефон',
    'phone.linked': 'Подключён',
    'phone.folder': 'Папка на телефоне',
    'phone.no_folder': 'Папка не выбрана',
    'phone.change_root': 'Сменить папку',
    'phone.wait_root': 'Выберите папку на телефоне.',
    'phone.scan': 'Проверить',
    'phone.scanning': 'Читаем…',
    'phone.forget': 'Отключить',
    'phone.scanned': 'Файлов: {count}. Объём: {size}.',
    'phone.lost': 'Связь с телефоном потеряна.',
    'phone.error': 'Ошибка: {error}',
    'error.bridge': 'Нет доступа к приложению',
    'error.init': 'Не удалось открыть папку: {error}',
    'error.unknown': 'Неизвестная ошибка',
    'window.minimize': 'Свернуть',
    'window.maximize': 'Развернуть',
    'window.close': 'Закрыть',
  };

  const en = {
    'setup.title': 'Devices',
    'setup.lan': 'Local network',
    'setup.adb': 'USB (ADB)',
    'setup.refresh': 'Refresh',
    'setup.change_folder': 'Change PC folder',
    'setup.searching': 'Searching…',
    'setup.empty_lan': 'No devices found.',
    'setup.empty_adb': 'No devices.',
    'setup.connect': 'Connect',
    'setup.connecting': 'Connecting…',
    'setup.cancel': 'Cancel',
    'setup.wait_pair': 'Confirm the connection on the phone.',
    'setup.unauthorized': 'Allow debugging',
    'setup.offline': 'Unavailable',
    'setup.adb_error': 'ADB: {error}',
    'library.title': 'Music folder',
    'library.hint': 'Choose a folder on this computer.',
    'library.action': 'Choose folder',
    'library.choosing': 'Opening…',
    'library.dialog': 'Choose the music folder',
    'library.waiting_phone': 'Choose the folder on the phone.',
    'library.back': 'Back to devices',
    'workspace.label': 'Library',
    'workspace.connect': 'Connect a phone',
    'phone.linked': 'Connected',
    'phone.folder': 'Phone folder',
    'phone.no_folder': 'No folder chosen',
    'phone.change_root': 'Change folder',
    'phone.wait_root': 'Choose the folder on the phone.',
    'phone.scan': 'Check',
    'phone.scanning': 'Reading…',
    'phone.forget': 'Disconnect',
    'phone.scanned': 'Files: {count}. Size: {size}.',
    'phone.lost': 'The phone connection was lost.',
    'phone.error': 'Error: {error}',
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
