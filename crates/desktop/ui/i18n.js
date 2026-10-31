'use strict';

// Тексты интерфейса. Лежат отдельно от отрисовки, чтобы правки формулировок
// не расползались по app.js. Язык берётся из localStorage, иначе из системы.

const I18N = (() => {
  const RU = {
    'app.name': 'SynctMusic',

    'common.copied': 'Скопировано',
    'common.copyFailed': 'Не удалось скопировать',
    'common.unknownError': 'Неизвестная ошибка',
    'common.cancel': 'Отмена',
    'common.confirm': 'Подтвердить',
    'common.error': 'ошибка',
    'common.noAnswer': 'нет ответа',
    'common.none': 'нет',
    'common.notSelected': 'не выбрана',
    'common.dash': '—',

    'word.files': ['файл', 'файла', 'файлов'],
    'word.deletions': ['удаление', 'удаления', 'удалений'],
    'word.conflicts': ['конфликт', 'конфликта', 'конфликтов'],
    'word.devices': ['устройство', 'устройства', 'устройств'],
    'word.items': ['пункт', 'пункта', 'пунктов'],

    'time.justNow': 'только что',
    'time.minutesAgo': '{n} мин назад',
    'time.hoursAgo': '{n} ч назад',
    'time.daysAgo': '{n} дн назад',

    'err.interface': 'Ошибка интерфейса',
    'err.bridge': 'Нет доступа к ядру Tauri',
    'err.bridgeText': 'Запустите собранное приложение SynctMusic, а не страницу в браузере.',
    'err.settings': 'Не удалось прочитать настройки',

    'state.service': 'Служба',
    'state.notRunning': 'не запущена',
    'state.noRoot.title': 'Библиотека не выбрана',
    'state.noRoot.sub': 'Укажите папку с музыкой.',
    'state.stopped.title': 'Служба остановлена',
    'state.stopped.sub': 'Синхронизация и наблюдение за папкой не работают.',
    'state.syncing.cap': 'синхронизация',
    'state.syncing.disc': 'сверка',
    'state.syncing.title': 'Синхронизация',
    'state.syncing.sub': 'Сверяем список файлов с устройствами.',
    'state.lowDisk.cap': 'мало места',
    'state.lowDisk.disc': 'место',
    'state.lowDisk.title': 'Синхронизация на паузе',
    'state.lowDisk.sub': 'Свободного места на диске недостаточно. Освободите место и запустите проверку.',
    'state.paused.cap': 'пауза',
    'state.paused.disc': 'пауза',
    'state.paused.title': 'Пауза',
    'state.paused.sub': 'Сканирование и сеть остановлены. Файлы не меняются.',
    'state.error.cap': 'ошибка',
    'state.error.disc': 'ошибка',
    'state.error.title': 'Ошибка',
    'state.error.sub': 'Служба сообщила об ошибке.',
    'state.ok.cap': 'синхронизировано',
    'state.ok.disc': 'норма',
    'state.ok.title': 'Синхронизировано',
    'state.ok.sub': 'Синхронизаций ещё не было.',
    'state.ok.subSync': 'Последняя синхронизация: {ago}.',
    'state.pendingNote.one': '{n} удаление ждёт решения.',
    'state.pendingNote.few': '{n} удаления ждут решения.',
    'state.pendingNote.many': '{n} удалений ждут решения.',

    'titlebar.firstRun': 'первый запуск',
    'titlebar.serviceStopped': 'служба остановлена',
    'titlebar.syncing': 'синхронизация',
    'titlebar.paused': 'пауза',
    'titlebar.error': 'ошибка',
    'titlebar.online': 'на связи',
    'titlebar.minimize': 'Свернуть',
    'titlebar.maximize': 'Развернуть',
    'titlebar.close': 'Закрыть',

    'rail.thisDevice': 'это устройство',
    'rail.noLibrary': 'Библиотека не выбрана',
    'rail.serviceRunning': 'служба работает',
    'rail.serviceStopped': 'служба остановлена',
    'rail.web': 'Веб-интерфейс',
    'rail.sections': 'Разделы',

    'nav.overview': 'Обзор',
    'nav.pending': 'Удаления',
    'nav.conflicts': 'Конфликты',
    'nav.vault': 'Хранилище',
    'nav.devices': 'Устройства',
    'nav.pair': 'Сопряжение',
    'nav.settings': 'Настройки',

    'act.startService': 'Запустить службу',
    'act.start': 'Запустить',
    'act.stop': 'Остановить',
    'act.sync': 'Синхронизировать',
    'act.scan': 'Сканировать',
    'act.pause': 'Пауза',
    'act.resume': 'Продолжить',
    'act.openFolder': 'Открыть папку с музыкой',

    'stat.files': 'Файлов в индексе',
    'stat.filesNote': '{n} на этом диске',
    'stat.size': 'Объём библиотеки',
    'stat.ops': 'Записей в журнале',
    'stat.opsNote': 'скан {ago}',
    'stat.vault': 'В хранилище',

    'panel.connection': 'Связь',
    'tag.connected': 'Подключено',
    'tag.disconnected': 'Нет подключения',
    'node.thisPc': 'Этот компьютер',
    'peers.phone': 'Телефон',
    'peers.online': 'на связи {ago}',
    'peers.noResponse': 'не отвечает',
    'peers.notPaired': 'не сопряжён',
    'signal.lastContact': 'Связь: {ago}',
    'signal.never': 'Синхронизаций не было',
    'btn.pairCode': 'Код сопряжения',

    'alert.serviceStopped.title': 'Служба остановлена',
    'alert.serviceStopped.text': 'Синхронизация и наблюдение за папкой не работают.',
    'alert.lowDisk.title': 'Мало свободного места',
    'alert.lowDisk.text': 'Осталось {percent}. Синхронизация приостановлена.',
    'alert.lowDisk.action': 'Проверить',
    'alert.pending.title.one': '{n} удаление без решения',
    'alert.pending.title.few': '{n} удаления без решения',
    'alert.pending.title.many': '{n} удалений без решения',
    'alert.pending.text': 'Файл исчез с диска. Укажите, что с ним делать.',
    'alert.pending.action': 'Открыть',
    'alert.conflicts.title.one': '{n} конфликт',
    'alert.conflicts.title.few': '{n} конфликта',
    'alert.conflicts.title.many': '{n} конфликтов',
    'alert.conflicts.text': 'Один путь изменён на двух устройствах. Обе версии на диске.',
    'alert.error.title': 'Последняя ошибка',
    'alert.error.action': 'Журнал',
    'alert.noBinary.title': 'Не найден syncthd',
    'alert.noBinary.text': 'Соберите его: cargo build --release -p syncth-daemon.',
    'alert.noBinary.action': 'Подробнее',

    'panel.attention': 'Требует решения',
    'attention.count.one': '{n} пункт',
    'attention.count.few': '{n} пункта',
    'attention.count.many': '{n} пунктов',
    'attention.clean': 'нет',
    'attention.empty': 'Нет задач.',
    'attention.more': 'и ещё {n}',

    'panel.devices': 'Устройства',
    'devices.tag': 'всего {n}',
    'devices.tagEmpty': 'нет',
    'device.unknown': 'устройство',
    'device.nearby': 'Новое устройство',
    'devices.connect': 'Добавить устройство',
    'devices.all': 'Все устройства',
    'devices.emptyTitle': 'Устройств нет',
    'devices.emptyText': 'Добавьте устройство по коду сопряжения.',
    'devices.emptyAction': 'Сопряжение',

    'pending.kicker': 'Очередь',
    'pending.title': 'Удаления',
    'pending.sub': 'Файлы, исчезнувшие с диска. Решение принимается для каждого файла.',
    'pending.rowMeta': 'исчез {ago}',
    'pending.keep': 'Оставить здесь',
    'pending.delete': 'Удалить везде',
    'pending.dismiss': 'Убрать из очереди',
    'pending.emptyTitle': 'Очередь пуста',
    'pending.emptyText': 'Файлы, исчезнувшие с диска, появятся здесь.',
    'pending.hint': '«Оставить здесь» исключает файл из общей библиотеки, «Удалить везде» удаляет его на всех устройствах.',
    'panel.waiting': 'Ждут решения',

    'conflicts.kicker': 'Конфликты',
    'conflicts.sub': 'Один путь изменён на двух устройствах. Обе версии остаются на диске.',
    'conflicts.rowMeta': 'изменён на двух устройствах',
    'conflicts.open': 'Открыть папку',
    'conflicts.emptyTitle': 'Конфликтов нет',
    'conflicts.emptyText': 'Файлов с одинаковым путём и разным содержимым нет.',
    'conflicts.openLibrary': 'Открыть библиотеку',
    'panel.conflicts': 'Требуют разбора',

    'vault.kicker': 'Хранилище',
    'vault.sub': 'Удалённые файлы и заменённые версии хранятся 30 дней.',
    'vault.rowMeta': '{size} · удалён {ago}',
    'vault.restore': 'Вернуть',
    'vault.emptyTitle': 'Хранилище пусто',
    'vault.emptyText': 'Здесь появятся удалённые файлы.',
    'vault.gc': 'Очистить просроченное',
    'vault.count.one': '{n} файл',
    'vault.count.few': '{n} файла',
    'vault.count.many': '{n} файлов',
    'panel.restorable': 'Можно вернуть',

    'devices.kicker': 'Устройства',
    'devices.sub': 'Обмен идёт напрямую по локальной сети. Ключ подписи закрепляется при первом контакте.',
    'devices.tagOnline': 'служба работает',
    'devices.tagOffline': 'служба остановлена',
    'kv.id': 'Идентификатор',
    'kv.library': 'Библиотека',
    'kv.syncAddr': 'Адрес синхронизации',
    'kv.filesOnDisk': 'Файлов на диске',
    'devices.copyId': 'Копировать ID',
    'devices.folder': 'Папка',
    'devices.known': 'Известные устройства',
    'devices.otherEmptyTitle': 'Других устройств нет',
    'devices.otherEmptyText': 'Добавьте устройство по коду сопряжения.',

    'pair.kicker': 'Сопряжение',
    'pair.sub': 'Код содержит адрес, идентификатор и секрет библиотеки. Вставьте его на втором устройстве или отсканируйте QR.',
    'pair.newCode': 'Новый код',
    'pair.serviceStoppedTitle': 'Служба остановлена',
    'pair.serviceStoppedText': 'Код создаёт служба. Запустите её, чтобы продолжить.',
    'pair.codeError': 'Не удалось получить код',
    'pair.loading': 'Создание кода…',
    'pair.loadingText': 'Служба формирует QR и строку сопряжения.',
    'pair.panelCode': 'Код библиотеки',
    'pair.copy': 'Копировать код',
    'pair.step1': 'Откройте SynctMusic на телефоне и выберите «Сопряжение».',
    'pair.step2': 'Отсканируйте QR или вставьте код.',
    'pair.step3': 'После первого обмена устройство запомнится.',
    'pair.placeholder': 'Код с другого устройства',
    'pair.submit': 'Сопрячь',
    'pair.qrPanel': 'QR',
    'pair.qrTag': 'наведите камеру',
    'pair.qrNote': 'Код действует до создания нового.',
    'pair.qrMissing': 'QR недоступен',
    'pair.qrMissingHint': 'используйте код',

    'settings.kicker': 'Настройки',
    'settings.sub': 'Пути, состояние службы и журнал.',
    'settings.paths': 'Папки',
    'kv.music': 'Музыка',
    'kv.data': 'Служебные данные',
    'kv.config': 'Настройки клиента',
    'settings.openMusic': 'Открыть музыку',
    'settings.data': 'Данные',
    'settings.changeFolder': 'Сменить папку',
    'settings.service': 'Служба',
    'settings.running': 'работает',
    'settings.stopped': 'остановлена',
    'kv.state': 'Состояние',
    'kv.binary': 'Исполняемый файл',
    'kv.port': 'Порт синхронизации',
    'stateLabel.idle': 'на связи',
    'stateLabel.syncing': 'синхронизация',
    'stateLabel.paused': 'пауза',
    'stateLabel.pausedWarning': 'пауза: мало места',
    'stateLabel.error': 'ошибка',
    'stateLabel.stopped': 'остановлена',
    'settings.web': 'Веб-интерфейс',
    'settings.refreshLog': 'Обновить журнал',
    'settings.logEmpty': 'Журнал пуст.',
    'settings.logHint': 'Нажмите «Обновить журнал», чтобы прочитать daemon.log.',
    'settings.about': 'О программе',
    'kv.version': 'Версия',
    'kv.platform': 'Платформа',
    'kv.device': 'Устройство',
    'kv.protocol': 'Протокол',
    'settings.aboutNote': 'Без сервера и аккаунта. Локальные файлы — источник истины.',
    'settings.language': 'Язык',

    'onboard.kicker': 'Первый запуск',
    'onboard.title': 'Синхронизация музыкальной папки',
    'onboard.lead': 'Файлы на этом компьютере и на телефоне будут одинаковыми.',
    'onboard.p1.title': 'Файлы не меняются',
    'onboard.p1.text': 'Индексируются существующие файлы. Имена и структура папок остаются прежними.',
    'onboard.p2.title': 'Удаления хранятся 30 дней',
    'onboard.p2.text': 'Копия удалённого файла остаётся в хранилище.',
    'onboard.p3.title': 'Теги и обложки',
    'onboard.p3.text': 'Изменения из AIMP и Poweramp синхронизируются вместе с файлом.',
    'onboard.card': 'Папка с музыкой',
    'onboard.hint': 'Выберите существующую папку. Первый проход только читает файлы.',
    'onboard.noFolder': 'Папка не выбрана',
    'onboard.pick': 'Выбрать папку',
    'onboard.pickData': 'Изменить служебную папку',
    'onboard.dataDefault': 'Папка данных по умолчанию',
    'onboard.start': 'Начать',
    'onboard.indexing': 'Индексация…',
    'onboard.busy': 'Читаем файлы и считаем хеши. Большая библиотека может занять несколько минут.',
    'onboard.footer': 'Телефон добавляется в разделе «Сопряжение».',

    'modal.gc.title': 'Очистить хранилище?',
    'modal.gc.text': 'Будут удалены копии с истёкшим сроком хранения. Остальные останутся.',
    'modal.gc.label': 'Очистить',
    'modal.delete.title': 'Удалить на всех устройствах?',
    'modal.delete.text': 'Копия останется в хранилище 30 дней.',
    'modal.delete.label': 'Удалить везде',
    'modal.stop.title': 'Остановить службу?',
    'modal.stop.text': 'Синхронизация и наблюдение за папкой прекратятся. Файлы не изменятся.',
    'modal.changeRoot.title': 'Остановить службу и сменить папку?',
    'modal.changeRoot.text': 'Текущая папка и журнал сохранятся.',
    'modal.changeRoot.label': 'Остановить и выбрать',

    'toast.indexed': 'Библиотека проиндексирована: {n} {word}',
    'toast.serviceStarted': 'Служба запущена',
    'toast.serviceAlready': 'Служба уже работает',
    'toast.serviceStopped': 'Служба остановлена',
    'toast.serviceNoAnswer': 'Служба не ответила за 5 секунд. Проверьте журнал.',
    'toast.librarySwitched': 'Библиотека переключена',
    'toast.codeUpdated': 'Код обновлён',
    'toast.syncDone': 'Синхронизация завершена',
    'toast.scanDone': 'Сканирование завершено',
    'toast.paused': 'Пауза включена',
    'toast.resumed': 'Пауза снята',
    'toast.diskChecked': 'Проверка диска завершена',
    'toast.vaultCleaned': 'Хранилище очищено',
    'toast.keptHere': 'Файл исключён из библиотеки',
    'toast.dismissed': 'Запись убрана из очереди',
    'toast.deleteConfirmed': 'Удаление подтверждено',
    'toast.restored': 'Файл восстановлен',
    'toast.paired': 'Сопряжение запущено',
    'toast.done': 'Готово',

    'dialog.pickMusic': 'Папка с музыкой',
    'dialog.pickNewMusic': 'Новая папка с музыкой',
    'dialog.pickData': 'Папка служебных данных',
  };

  const EN = {
    'app.name': 'SynctMusic',

    'common.copied': 'Copied',
    'common.copyFailed': 'Copy failed',
    'common.unknownError': 'Unknown error',
    'common.cancel': 'Cancel',
    'common.confirm': 'Confirm',
    'common.error': 'error',
    'common.noAnswer': 'no response',
    'common.none': 'none',
    'common.notSelected': 'not selected',
    'common.dash': '—',

    'word.files': ['file', 'files'],
    'word.deletions': ['deletion', 'deletions'],
    'word.conflicts': ['conflict', 'conflicts'],
    'word.devices': ['device', 'devices'],
    'word.items': ['item', 'items'],

    'time.justNow': 'just now',
    'time.minutesAgo': '{n} min ago',
    'time.hoursAgo': '{n} h ago',
    'time.daysAgo': '{n} d ago',

    'err.interface': 'Interface error',
    'err.bridge': 'Tauri core unavailable',
    'err.bridgeText': 'Start the built SynctMusic app instead of the page in a browser.',
    'err.settings': 'Could not read settings',

    'state.service': 'Service',
    'state.notRunning': 'stopped',
    'state.noRoot.title': 'No library selected',
    'state.noRoot.sub': 'Choose a music folder.',
    'state.stopped.title': 'Service stopped',
    'state.stopped.sub': 'Sync and folder watching are not running.',
    'state.syncing.cap': 'syncing',
    'state.syncing.disc': 'sync',
    'state.syncing.title': 'Syncing',
    'state.syncing.sub': 'Comparing the file list with other devices.',
    'state.lowDisk.cap': 'low disk space',
    'state.lowDisk.disc': 'space',
    'state.lowDisk.title': 'Sync paused',
    'state.lowDisk.sub': 'Not enough free disk space. Free some space and run the check again.',
    'state.paused.cap': 'paused',
    'state.paused.disc': 'paused',
    'state.paused.title': 'Paused',
    'state.paused.sub': 'Scanning and network are stopped. Files are unchanged.',
    'state.error.cap': 'error',
    'state.error.disc': 'error',
    'state.error.title': 'Error',
    'state.error.sub': 'The service reported an error.',
    'state.ok.cap': 'up to date',
    'state.ok.disc': 'ok',
    'state.ok.title': 'Up to date',
    'state.ok.sub': 'No syncs yet.',
    'state.ok.subSync': 'Last sync: {ago}.',
    'state.pendingNote.one': '{n} deletion is waiting for a decision.',
    'state.pendingNote.other': '{n} deletions are waiting for a decision.',

    'titlebar.firstRun': 'first run',
    'titlebar.serviceStopped': 'service stopped',
    'titlebar.syncing': 'syncing',
    'titlebar.paused': 'paused',
    'titlebar.error': 'error',
    'titlebar.online': 'online',
    'titlebar.minimize': 'Minimize',
    'titlebar.maximize': 'Maximize',
    'titlebar.close': 'Close',

    'rail.thisDevice': 'this device',
    'rail.noLibrary': 'No library selected',
    'rail.serviceRunning': 'service running',
    'rail.serviceStopped': 'service stopped',
    'rail.web': 'Web UI',
    'rail.sections': 'Sections',

    'nav.overview': 'Overview',
    'nav.pending': 'Deletions',
    'nav.conflicts': 'Conflicts',
    'nav.vault': 'Vault',
    'nav.devices': 'Devices',
    'nav.pair': 'Pairing',
    'nav.settings': 'Settings',

    'act.startService': 'Start service',
    'act.start': 'Start',
    'act.stop': 'Stop',
    'act.sync': 'Sync',
    'act.scan': 'Scan',
    'act.pause': 'Pause',
    'act.resume': 'Resume',
    'act.openFolder': 'Open music folder',

    'stat.files': 'Files indexed',
    'stat.filesNote': '{n} on this disk',
    'stat.size': 'Library size',
    'stat.ops': 'Journal entries',
    'stat.opsNote': 'scan {ago}',
    'stat.vault': 'In vault',

    'panel.connection': 'Connection',
    'tag.connected': 'Connected',
    'tag.disconnected': 'Not connected',
    'node.thisPc': 'This computer',
    'peers.phone': 'Phone',
    'peers.online': 'online {ago}',
    'peers.noResponse': 'not responding',
    'peers.notPaired': 'not paired',
    'signal.lastContact': 'Last contact: {ago}',
    'signal.never': 'No syncs yet',
    'btn.pairCode': 'Pairing code',

    'alert.serviceStopped.title': 'Service stopped',
    'alert.serviceStopped.text': 'Sync and folder watching are not running.',
    'alert.lowDisk.title': 'Low disk space',
    'alert.lowDisk.text': '{percent} left. Sync is paused.',
    'alert.lowDisk.action': 'Check',
    'alert.pending.title.one': '{n} deletion without a decision',
    'alert.pending.title.other': '{n} deletions without a decision',
    'alert.pending.text': 'A file disappeared from disk. Choose what to do with it.',
    'alert.pending.action': 'Open',
    'alert.conflicts.title.one': '{n} conflict',
    'alert.conflicts.title.other': '{n} conflicts',
    'alert.conflicts.text': 'One path changed on two devices. Both versions are on disk.',
    'alert.error.title': 'Last error',
    'alert.error.action': 'Log',
    'alert.noBinary.title': 'syncthd not found',
    'alert.noBinary.text': 'Build it with cargo build --release -p syncth-daemon.',
    'alert.noBinary.action': 'Details',

    'panel.attention': 'Needs attention',
    'attention.count.one': '{n} item',
    'attention.count.other': '{n} items',
    'attention.clean': 'none',
    'attention.empty': 'Nothing to do.',
    'attention.more': 'and {n} more',

    'panel.devices': 'Devices',
    'devices.tag': '{n} total',
    'devices.tagEmpty': 'none',
    'device.unknown': 'device',
    'device.nearby': 'New device',
    'devices.connect': 'Add device',
    'devices.all': 'All devices',
    'devices.emptyTitle': 'No devices',
    'devices.emptyText': 'Add a device with a pairing code.',
    'devices.emptyAction': 'Pairing',

    'pending.kicker': 'Queue',
    'pending.title': 'Deletions',
    'pending.sub': 'Files that disappeared from disk. Each one needs a decision.',
    'pending.rowMeta': 'disappeared {ago}',
    'pending.keep': 'Keep here',
    'pending.delete': 'Delete everywhere',
    'pending.dismiss': 'Dismiss',
    'pending.emptyTitle': 'Queue is empty',
    'pending.emptyText': 'Files that disappear from disk will appear here.',
    'pending.hint': '“Keep here” removes the file from the shared library; “Delete everywhere” deletes it on all devices.',
    'panel.waiting': 'Waiting for a decision',

    'conflicts.kicker': 'Conflicts',
    'conflicts.sub': 'One path changed on two devices. Both versions stay on disk.',
    'conflicts.rowMeta': 'changed on two devices',
    'conflicts.open': 'Open folder',
    'conflicts.emptyTitle': 'No conflicts',
    'conflicts.emptyText': 'No files with the same path and different content.',
    'conflicts.openLibrary': 'Open library',
    'panel.conflicts': 'Needs review',

    'vault.kicker': 'Vault',
    'vault.sub': 'Deleted files and replaced versions are kept for 30 days.',
    'vault.rowMeta': '{size} · deleted {ago}',
    'vault.restore': 'Restore',
    'vault.emptyTitle': 'Vault is empty',
    'vault.emptyText': 'Deleted files will appear here.',
    'vault.gc': 'Remove expired',
    'vault.count.one': '{n} file',
    'vault.count.other': '{n} files',
    'panel.restorable': 'Restorable',

    'devices.kicker': 'Devices',
    'devices.sub': 'Devices exchange changes directly over the local network. The signing key is pinned on first contact.',
    'devices.tagOnline': 'service running',
    'devices.tagOffline': 'service stopped',
    'kv.id': 'Identifier',
    'kv.library': 'Library',
    'kv.syncAddr': 'Sync address',
    'kv.filesOnDisk': 'Files on disk',
    'devices.copyId': 'Copy ID',
    'devices.folder': 'Folder',
    'devices.known': 'Known devices',
    'devices.otherEmptyTitle': 'No other devices',
    'devices.otherEmptyText': 'Add a device with a pairing code.',

    'pair.kicker': 'Pairing',
    'pair.sub': 'The code contains the address, identifier and library secret. Enter it on the second device or scan the QR.',
    'pair.newCode': 'New code',
    'pair.serviceStoppedTitle': 'Service stopped',
    'pair.serviceStoppedText': 'The service creates the code. Start it to continue.',
    'pair.codeError': 'Could not get a code',
    'pair.loading': 'Creating a code…',
    'pair.loadingText': 'The service is generating the QR and pairing string.',
    'pair.panelCode': 'Library code',
    'pair.copy': 'Copy code',
    'pair.step1': 'Open SynctMusic on the phone and choose “Pairing”.',
    'pair.step2': 'Scan the QR or paste the code.',
    'pair.step3': 'The device is remembered after the first exchange.',
    'pair.placeholder': 'Code from another device',
    'pair.submit': 'Pair',
    'pair.qrPanel': 'QR',
    'pair.qrTag': 'point the camera',
    'pair.qrNote': 'The code stays valid until a new one is created.',
    'pair.qrMissing': 'QR unavailable',
    'pair.qrMissingHint': 'use the code',

    'settings.kicker': 'Settings',
    'settings.sub': 'Paths, service state and the log.',
    'settings.paths': 'Folders',
    'kv.music': 'Music',
    'kv.data': 'App data',
    'kv.config': 'Client config',
    'settings.openMusic': 'Open music',
    'settings.data': 'Data',
    'settings.changeFolder': 'Change folder',
    'settings.service': 'Service',
    'settings.running': 'running',
    'settings.stopped': 'stopped',
    'kv.state': 'State',
    'kv.binary': 'Executable',
    'kv.port': 'Sync port',
    'stateLabel.idle': 'online',
    'stateLabel.syncing': 'syncing',
    'stateLabel.paused': 'paused',
    'stateLabel.pausedWarning': 'paused: low disk space',
    'stateLabel.error': 'error',
    'stateLabel.stopped': 'stopped',
    'settings.web': 'Web UI',
    'settings.refreshLog': 'Refresh log',
    'settings.logEmpty': 'Log is empty.',
    'settings.logHint': 'Press “Refresh log” to read daemon.log.',
    'settings.about': 'About',
    'kv.version': 'Version',
    'kv.platform': 'Platform',
    'kv.device': 'Device',
    'kv.protocol': 'Protocol',
    'settings.aboutNote': 'No server, no account. Local files are the source of truth.',
    'settings.language': 'Language',

    'onboard.kicker': 'First run',
    'onboard.title': 'Music folder sync',
    'onboard.lead': 'The files on this computer and on the phone will be identical.',
    'onboard.p1.title': 'Files stay in place',
    'onboard.p1.text': 'Existing files are indexed. Names and folder structure stay unchanged.',
    'onboard.p2.title': 'Deletions kept for 30 days',
    'onboard.p2.text': 'A copy of a deleted file stays in the vault.',
    'onboard.p3.title': 'Tags and artwork',
    'onboard.p3.text': 'Changes made in AIMP and Poweramp sync together with the file.',
    'onboard.card': 'Music folder',
    'onboard.hint': 'Choose an existing folder. The first pass only reads files.',
    'onboard.noFolder': 'No folder selected',
    'onboard.pick': 'Choose folder',
    'onboard.pickData': 'Change app data folder',
    'onboard.dataDefault': 'Default app data folder',
    'onboard.start': 'Start',
    'onboard.indexing': 'Indexing…',
    'onboard.busy': 'Reading files and computing hashes. A large library can take several minutes.',
    'onboard.footer': 'Add the phone later in the “Pairing” section.',

    'modal.gc.title': 'Clean the vault?',
    'modal.gc.text': 'Only copies whose retention period has expired will be removed.',
    'modal.gc.label': 'Clean',
    'modal.delete.title': 'Delete on all devices?',
    'modal.delete.text': 'The copy stays in the vault for 30 days.',
    'modal.delete.label': 'Delete everywhere',
    'modal.stop.title': 'Stop the service?',
    'modal.stop.text': 'Sync and folder watching will stop. Files are not changed.',
    'modal.changeRoot.title': 'Stop the service and change folder?',
    'modal.changeRoot.text': 'The current folder and journal are kept.',
    'modal.changeRoot.label': 'Stop and choose',

    'toast.indexed': 'Library indexed: {n} {word}',
    'toast.serviceStarted': 'Service started',
    'toast.serviceAlready': 'Service is already running',
    'toast.serviceStopped': 'Service stopped',
    'toast.serviceNoAnswer': 'The service did not respond within 5 seconds. Check the log.',
    'toast.librarySwitched': 'Library switched',
    'toast.codeUpdated': 'Code updated',
    'toast.syncDone': 'Sync finished',
    'toast.scanDone': 'Scan finished',
    'toast.paused': 'Paused',
    'toast.resumed': 'Resumed',
    'toast.diskChecked': 'Disk check finished',
    'toast.vaultCleaned': 'Vault cleaned',
    'toast.keptHere': 'File removed from the library',
    'toast.dismissed': 'Entry removed from the queue',
    'toast.deleteConfirmed': 'Deletion confirmed',
    'toast.restored': 'File restored',
    'toast.paired': 'Pairing started',
    'toast.done': 'Done',

    'dialog.pickMusic': 'Music folder',
    'dialog.pickNewMusic': 'New music folder',
    'dialog.pickData': 'App data folder',
  };

  const DICTS = { ru: RU, en: EN };
  const STORAGE_KEY = 'syncth.locale';
  const listeners = [];

  function detect() {
    try {
      const saved = localStorage.getItem(STORAGE_KEY);
      if (saved && DICTS[saved]) return saved;
    } catch {
      // приватный режим или отключённое хранилище
    }
    const nav = (typeof navigator !== 'undefined' && navigator.language) || 'ru';
    return nav.toLowerCase().startsWith('ru') ? 'ru' : 'en';
  }

  let locale = detect();

  const dict = () => DICTS[locale] || DICTS.ru;
  const dateLocale = () => (locale === 'ru' ? 'ru-RU' : 'en-US');

  function raw(key) {
    const value = dict()[key];
    if (value === undefined) {
      const fallback = DICTS.ru[key];
      if (fallback === undefined) {
        if (typeof console !== 'undefined') console.warn('missing i18n key:', key);
        return key;
      }
      return fallback;
    }
    return value;
  }

  function fill(text, vars) {
    if (!vars) return text;
    return String(text).replace(/\{(\w+)\}/g, (match, name) =>
      (vars[name] === undefined ? match : String(vars[name])));
  }

  function t(key, vars) {
    const value = raw(key);
    if (Array.isArray(value)) return fill(value[0], vars);
    return fill(value, vars);
  }

  function formOf(n) {
    const abs = Math.abs(Number(n) || 0);
    if (locale === 'ru') {
      const tail = abs % 10;
      if (abs % 100 > 10 && abs % 100 < 20) return 'many';
      if (tail > 1 && tail < 5) return 'few';
      if (tail === 1) return 'one';
      return 'many';
    }
    return abs === 1 ? 'one' : 'other';
  }

  // plural('word.files', 3) -> «файла» / «files»
  function plural(key, n) {
    const forms = raw(key);
    if (!Array.isArray(forms)) return String(forms);
    const form = formOf(n);
    if (locale === 'ru') {
      return forms[form === 'few' ? 1 : form === 'one' ? 0 : 2];
    }
    return forms[form === 'one' ? 0 : 1];
  }

  // tp('state.pendingNote', 3, {n: 3}) -> форма целой фразы, не только слова
  function tp(base, n, vars) {
    return t(`${base}.${formOf(n)}`, { n, ...(vars || {}) });
  }

  function num(value) {
    return Number(value || 0).toLocaleString(dateLocale());
  }

  // size(1536) -> { value: '1,5', unit: 'КБ', text: '1,5 КБ' }
  function size(bytes) {
    const units = locale === 'ru'
      ? ['Б', 'КБ', 'МБ', 'ГБ', 'ТБ']
      : ['B', 'KB', 'MB', 'GB', 'TB'];
    let value = Number(bytes || 0);
    let index = 0;
    while (value >= 1024 && index < units.length - 1) {
      value /= 1024;
      index += 1;
    }
    const shown = index === 0
      ? String(Math.round(value))
      : value.toLocaleString(dateLocale(), { maximumFractionDigits: 1 });
    return { value: shown, unit: units[index], text: `${shown} ${units[index]}` };
  }

  function ago(ts) {
    const ms = Number(ts || 0);
    if (!ms) return t('common.dash');
    const millis = ms > 1e12 ? ms : ms * 1000;
    const diff = Math.max(0, Date.now() - millis);
    const minutes = Math.floor(diff / 60000);
    if (minutes < 1) return t('time.justNow');
    if (minutes < 60) return t('time.minutesAgo', { n: minutes });
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return t('time.hoursAgo', { n: hours });
    const days = Math.floor(hours / 24);
    if (days < 30) return t('time.daysAgo', { n: days });
    return new Date(millis).toLocaleDateString(dateLocale(), { day: '2-digit', month: 'short' });
  }

  function clock(ts) {
    const ms = Number(ts || 0);
    if (!ms) return t('common.dash');
    const millis = ms > 1e12 ? ms : ms * 1000;
    return new Date(millis).toLocaleString(dateLocale(), {
      day: '2-digit',
      month: 'short',
      hour: '2-digit',
      minute: '2-digit',
    });
  }

  function apply(root) {
    if (typeof document === 'undefined') return;
    const scope = root || document;
    scope.querySelectorAll('[data-i18n]').forEach((el) => {
      el.textContent = t(el.dataset.i18n);
    });
    scope.querySelectorAll('[data-i18n-title]').forEach((el) => {
      el.title = t(el.dataset.i18nTitle);
    });
    scope.querySelectorAll('[data-i18n-aria]').forEach((el) => {
      el.setAttribute('aria-label', t(el.dataset.i18nAria));
    });
    document.documentElement.lang = locale;
  }

  function setLocale(next) {
    if (!DICTS[next] || next === locale) return;
    locale = next;
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // сохранение не критично
    }
    apply();
    listeners.forEach((fn) => fn(locale));
  }

  function onChange(fn) {
    listeners.push(fn);
  }

  return {
    t,
    tp,
    plural,
    num,
    ago,
    clock,
    size,
    apply,
    setLocale,
    onChange,
    dictFor(loc) { return DICTS[loc] || RU; },
    get locale() { return locale; },
    dateLocale,
    other(loc) {
      return (loc || locale) === 'ru' ? 'en' : 'ru';
    },
  };
})();

if (typeof window !== 'undefined') window.I18N = I18N;
if (typeof globalThis !== 'undefined') globalThis.I18N = I18N;
