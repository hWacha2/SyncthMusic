'use strict';

// Тексты Android-клиента. Отдельно от отрисовки, по той же схеме, что и на ПК.

const I18N = (() => {
  const RU = {
    'common.copied': 'Скопировано',
    'common.copyFailed': 'Не удалось скопировать',
    'common.unknownError': 'Неизвестная ошибка',
    'common.dash': '—',
    'common.none': 'нет',
    'attention.more': 'и ещё {n}',
    'attention.count.one': '{n} пункт',
    'attention.count.few': '{n} пункта',
    'attention.count.many': '{n} пунктов',
    'attention.clean': 'нет',
    'attention.empty': 'Нет задач.',

    'panel.attention': 'Требует решения',
    'panel.devices': 'Устройства',
    'panel.folder': 'Папка',
    'alert.pending.title.one': '{n} удаление без решения',
    'alert.pending.title.few': '{n} удаления без решения',
    'alert.pending.title.many': '{n} удалений без решения',
    'alert.pending.text': 'Файл исчез с диска. Укажите, что с ним делать.',
    'alert.pending.action': 'Открыть',
    'alert.conflicts.title.one': '{n} конфликт',
    'alert.conflicts.title.few': '{n} конфликта',
    'alert.conflicts.title.many': '{n} конфликтов',
    'alert.conflicts.text': 'Один путь изменён на двух устройствах. Обе версии на диске.',
    'alert.conflicts.action': 'Открыть',
    'devices.tag': 'всего {n}',
    'devices.tagEmpty': 'нет',
    'device.unknown': 'неизвестное устройство',

    'word.files': ['файл', 'файла', 'файлов'],

    'time.justNow': 'только что',
    'time.minutesAgo': '{n} мин назад',
    'time.hoursAgo': '{n} ч назад',
    'time.daysAgo': '{n} дн назад',

    'nav.overview': 'Обзор',
    'tab.overview': 'Обзор',
    'tab.plan': 'План',
    'tab.pending': 'Удаления',
    'tab.conflicts': 'Конфликты',
    'tab.vault': 'Хранилище',
    'tab.more': 'Ещё',

    'appbar.loading': 'проверка',
    'appbar.busy': 'выполняется',
    'appbar.error': 'ошибка',
    'appbar.notConfigured': 'не настроено',
    'appbar.attention': 'есть изменения',
    'appbar.synced': 'синхронизировано',
    'appbar.refresh': 'Обновить',

    'boot.loadingTitle': 'Загрузка',
    'boot.loadingText': 'Читаем библиотеку…',

    'error.title': 'Не удалось открыть библиотеку',
    'error.retry': 'Повторить',

    'setup.kicker': 'Первый запуск',
    'setup.title': 'Папка с музыкой',
    'setup.hint': 'SynctMusic читает выбранную папку и не меняет её содержимое.',
    'setup.label': 'Путь к папке',
    'setup.start': 'Начать',
    'setup.permission': 'Приложению нужен доступ «Ко всем файлам», иначе Android не даст читать музыку вне его папки.',

    'overview.kicker': 'Библиотека',
    'overview.title.attention': 'Есть файлы без решения',
    'overview.title.ok': 'Синхронизировано',
    'overview.hint.plan.one': '{n} изменение ждёт решения.',
    'overview.hint.plan.few': '{n} изменения ждут решения.',
    'overview.hint.plan.many': '{n} изменений ждут решения.',
    'overview.hint.ok': 'Файлы под наблюдением.',

    'act.sync': 'Синхронизировать',
    'act.scan': 'Пересканировать',
    'act.connect': 'Подключиться',
    'act.disconnect': 'Отключиться',
    'act.forget': 'Забыть',
    'conn.none': 'Нет соединения',
    'conn.connected': 'подключено',
    'conn.connectedTo': 'Подключено: {name}',
    'toast.connected': 'Подключено: {name}',
    'toast.disconnected': 'Соединение закрыто',
    'toast.peerForgotten': 'Устройство забыто',
    'run.connect': 'Подключение',
    'run.disconnect': 'Отключение',
    'run.forgetPeer': 'Забывание устройства',
    'confirm.forgetPeer': 'Забыть устройство? Сопряжение придётся выполнить заново.',

    'stat.files': 'Файлов в индексе',
    'stat.filesNote': '{n} на телефоне',
    'stat.size': 'Объём',
    'stat.ops': 'Записей в журнале',
    'stat.opsNote': 'скан {ago}',
    'stat.vault': 'В хранилище',

    'section.deletions': 'Удаления',
    'chip.hasPending': 'есть',
    'chip.clean': 'нет',
    'section.devices': 'Устройства',
    'section.folder': 'Папка',
    'pending.emptyTitle': 'Ничего не ждёт',
    'pending.emptyText': 'Файлов без решения нет.',
    'pending.rowMeta': 'исчез {ago}',
    'pending.toQueue': 'К удалениям',
    'devices.emptyTitle': 'Устройств нет',
    'devices.emptyText': 'Добавьте компьютер в разделе «Ещё».',
    'devices.pair': 'Сопряжение',
    'folder.notSelected': 'не выбрана',
    'folder.change': 'Сменить папку',

    'queue.kicker': 'Очередь',
    'queue.title': 'Удаления',
    'queue.titleEmpty': 'Очередь пуста',
    'queue.hint': 'Файл исчез с диска. Решение принимается для каждого файла.',
    'queue.keep': 'Оставить здесь',
    'queue.delete': 'Удалить везде',
    'queue.dismiss': 'Убрать из очереди',
    'queue.explain': '«Оставить здесь» исключает файл из библиотеки, «Удалить везде» удаляет его на всех устройствах.',
    'queue.emptyText': 'Файлы, исчезнувшие с диска, появятся здесь.',

    'conflicts.kicker': 'Конфликты',
    'conflicts.title': 'Конфликты',
    'conflicts.sub': 'Один путь изменён на двух устройствах. Обе версии остаются на диске.',
    'conflicts.rowMeta': 'две версии файла',
    'conflicts.hint': 'Версию выбирает пользователь, приложение файлы не перезаписывает.',
    'conflicts.emptyTitle': 'Конфликтов нет',
    'conflicts.emptyText': 'Нет файлов с одним путём и разным содержимым.',

    'plan.title': 'План синхронизации',
    'plan.sub': 'Изменения с обеих сторон. Решение применяется к выбранной группе целиком.',
    'plan.changes': 'Изменения',
    'plan.thisDevice': 'Это устройство',
    'plan.otherDevice': 'Другое устройство',
    'plan.bothDevices': 'Оба устройства',
    'plan.filter.all': 'Все',
    'plan.filter.added': 'Добавленные',
    'plan.filter.updated': 'Изменённые',
    'plan.filter.moved': 'Перенесённые',
    'plan.filter.deleted': 'Удалённые',
    'plan.filter.conflicts': 'Конфликты',
    'plan.offline': 'Нет соединения',
    'plan.offlineTitle': 'Нет соединения',
    'plan.offlineText': 'Видны только изменения этого устройства. Подключитесь к пиру, чтобы сравнить обе стороны.',
    'plan.connect': 'Устройства',
    'plan.offlineActions': 'Решения недоступны без соединения.',
    'plan.staleTitle': 'Есть новые изменения',
    'plan.staleText': 'Файлы изменились после построения плана. План нужно собрать заново.',
    'plan.staleAction': 'Обновить план',
    'plan.staleActions': 'Обновите план, чтобы менять решения.',
    'plan.divergedTitle': 'План разошёлся',
    'plan.divergedText': 'Наборы решений на устройствах не совпадают. Переподключитесь и повторите.',
    'plan.readonlyTitle': 'Идёт передача',
    'plan.readonlyText': 'Файлы передаются. Решения временно недоступны.',
    'plan.readonlyActions': 'Решения недоступны во время передачи.',
    'plan.errorTitle': 'План не построен',
    'plan.selectAll': 'Выбрать все',
    'plan.clear': 'Снять',
    'plan.selected.one': 'Выбрано: {n} файл',
    'plan.selected.few': 'Выбрано: {n} файла',
    'plan.selected.many': 'Выбрано: {n} файлов',
    'plan.unresolved.one': 'Требует решения: {n}',
    'plan.unresolved.few': 'Требуют решения: {n}',
    'plan.unresolved.many': 'Требуют решения: {n}',
    'plan.needsDecision': 'Требует решения',
    'plan.decisionSet': 'Решение принято',
    'plan.commit': 'Применить план',
    'plan.emptyTitle': 'Изменений нет',
    'plan.emptyText': 'Обе стороны совпадают.',
    'plan.emptyLocalText': 'Локальных изменений нет. Подключите устройство, чтобы сравнить библиотеки.',
    'plan.action.added.accept': 'Добавить на второе устройство',
    'plan.action.added.revert': 'Убрать у источника',
    'plan.action.added.onlyHere': 'Оставить только у источника',
    'plan.action.updated.accept': 'Применить на второе устройство',
    'plan.action.updated.revert': 'Вернуть версию со второго устройства',
    'plan.action.moved.accept': 'Перенести на второе устройство',
    'plan.action.moved.revert': 'Вернуть старый путь у источника',
    'plan.action.deleted.accept': 'Удалить и на втором устройстве',
    'plan.action.deleted.revert': 'Вернуть файл у источника',
    'plan.action.deleted.onlyHere': 'Оставить только на втором устройстве',
    'plan.action.conflict.keep': 'Оставить эту версию',
    'plan.action.conflict.accept': 'Взять версию другого устройства',
    'plan.action.conflict.keepBoth': 'Сохранить обе версии',
    'alert.plan.title.one': '{n} изменение ждёт решения',
    'alert.plan.title.few': '{n} изменения ждут решения',
    'alert.plan.title.many': '{n} изменений ждут решения',
    'alert.plan.text': 'Проверьте план и выберите, что применить.',
    'alert.plan.conflicts': 'Есть конфликты: без решения план не применится.',
    'alert.plan.action': 'Открыть план',
    'toast.planRefreshed': 'План обновлён',
    'toast.planStale': 'План не обновлён: переподключитесь с ПК',
    'toast.decisionsSaved.one': 'Сохранено решение для {n} файла',
    'toast.decisionsSaved.few': 'Сохранены решения для {n} файлов',
    'toast.decisionsSaved.many': 'Сохранены решения для {n} файлов',
    'run.planRefresh': 'Обновление плана',
    'run.commit': 'Применение плана',
    'run.decision': 'Решение',
    'confirm.commit': 'Применить план?\n\nВыбранные решения будут выполнены на обоих устройствах. Невыбранные удаления не применяются.',

    'vault.kicker': 'Хранилище',
    'vault.title': 'Хранилище',
    'vault.hint': 'Удалённые файлы хранятся 30 дней.',
    'vault.rowMeta': '{size} · удалён {ago}',
    'vault.restore': 'Вернуть',
    'vault.gc': 'Очистить просроченное',
    'vault.emptyTitle': 'Хранилище пусто',
    'vault.emptyText': 'Здесь появятся удалённые файлы.',

    'pair.kicker': 'Сопряжение',
    'pair.title': 'Сопряжение',
    'pair.hint': 'Покажите свой код на ПК или отсканируйте QR-код ПК.',
    'pair.myCode': 'Мой код',
    'pair.scan': 'Сканировать QR',
    'pair.scanHint': 'Наведите камеру на QR-код в ПК-версии.',
    'pair.scanCancel': 'Отмена',
    'pair.scanUnsupported': 'Сканирование QR не поддерживается. Вставьте код вручную.',
    'pair.scanDenied': 'Нет доступа к камере. Разрешите его в настройках Android.',
    'pair.codeLabel': 'Код телефона',
    'pair.copy': 'Скопировать',
    'pair.inputLabel': 'Код с другого устройства',
    'pair.submit': 'Сопрячь',
    'pair.devices': 'Устройства',
    'pair.noDevices': 'Сопряжённых устройств нет.',
    'pair.library': 'Библиотека',
    'pair.about': 'О приложении',
    'pair.version': 'SynctMusic {v} · AGPL-3.0',
    'pair.device': 'устройство: {dev}',
    'pair.port': 'порт синхронизации: {port}',
    'pair.language': 'Язык',

    'toast.needFolder': 'Укажите папку',
    'toast.initDone': 'Готово: {n} {word} в индексе',
    'toast.scan': 'Скан: +{added}, изменено {changed}, пропало {missing}',
    'toast.sync': 'Обмен: отдано {sent}, принято {received}',
    'toast.commit': 'Синхронизация: принято {accepted}, отклонено {rejected}',
    'toast.syncPartial': 'Частично: {error}',
    'toast.gc': 'Убрано из хранилища: {n}',
    'toast.paired': 'Сопряжено: {dev}',
    'toast.deleted': 'Удалено везде',
    'toast.kept': 'Файл исключён из библиотеки',
    'toast.failed': 'Не удалось: {path}',
    'toast.forgotten': 'Запись убрана из очереди',
    'toast.restored': 'Возвращено: {path}',
    'toast.needCode': 'Вставьте код',

    'run.init': 'Библиотека',
    'run.refresh': 'Обновление',
    'run.scan': 'Скан',
    'run.sync': 'Синхронизация',
    'run.gc': 'Хранилище',
    'run.code': 'Код',
    'run.pair': 'Сопряжение',
    'run.resolve': 'Решение',
    'run.forget': 'Очередь',
    'run.restore': 'Восстановление',

    'confirm.changeRoot': 'Выбрать другую папку?\n\nИндекс прежней папки останется в журнале, а её файлы попадут в очередь удалений.',
    'confirm.delete': 'Удалить «{path}» на всех устройствах?\n\nКопия останется в хранилище 30 дней.',
  };

  const EN = {
    'common.copied': 'Copied',
    'common.copyFailed': 'Copy failed',
    'common.unknownError': 'Unknown error',
    'common.dash': '—',
    'common.none': 'none',
    'attention.more': 'and {n} more',
    'attention.count.one': '{n} item',
    'attention.count.other': '{n} items',
    'attention.clean': 'none',
    'attention.empty': 'Nothing to do.',

    'panel.attention': 'Needs attention',
    'panel.devices': 'Devices',
    'panel.folder': 'Folder',
    'alert.pending.title.one': '{n} deletion without a decision',
    'alert.pending.title.other': '{n} deletions without a decision',
    'alert.pending.text': 'A file disappeared from disk. Choose what to do with it.',
    'alert.pending.action': 'Open',
    'alert.conflicts.title.one': '{n} conflict',
    'alert.conflicts.title.other': '{n} conflicts',
    'alert.conflicts.text': 'One path changed on two devices. Both versions are on disk.',
    'alert.conflicts.action': 'Open',
    'devices.tag': '{n} total',
    'devices.tagEmpty': 'none',
    'device.unknown': 'Unknown device',

    'word.files': ['file', 'files'],

    'time.justNow': 'just now',
    'time.minutesAgo': '{n} min ago',
    'time.hoursAgo': '{n} h ago',
    'time.daysAgo': '{n} d ago',

    'nav.overview': 'Overview',
    'tab.overview': 'Overview',
    'tab.plan': 'Plan',
    'tab.pending': 'Deletions',
    'tab.conflicts': 'Conflicts',
    'tab.vault': 'Vault',
    'tab.more': 'More',

    'appbar.loading': 'checking',
    'appbar.busy': 'working',
    'appbar.error': 'error',
    'appbar.notConfigured': 'not configured',
    'appbar.attention': 'changes pending',
    'appbar.synced': 'up to date',
    'appbar.refresh': 'Refresh',

    'boot.loadingTitle': 'Loading',
    'boot.loadingText': 'Reading the library…',

    'error.title': 'Could not open the library',
    'error.retry': 'Retry',

    'setup.kicker': 'First run',
    'setup.title': 'Music folder',
    'setup.hint': 'SynctMusic reads the selected folder and does not change its contents.',
    'setup.label': 'Folder path',
    'setup.start': 'Start',
    'setup.permission': 'The app needs “All files access”; otherwise Android blocks reading music outside its own folder.',

    'overview.kicker': 'Library',
    'overview.title.attention': 'Files waiting for a decision',
    'overview.title.ok': 'Up to date',
    'overview.hint.plan.one': '{n} change needs a decision.',
    'overview.hint.plan.other': '{n} changes need a decision.',
    'overview.hint.ok': 'Files are being watched.',

    'act.sync': 'Sync',
    'act.scan': 'Rescan',
    'act.connect': 'Connect',
    'act.disconnect': 'Disconnect',
    'act.forget': 'Forget',
    'conn.none': 'No connection',
    'conn.connected': 'connected',
    'conn.connectedTo': 'Connected: {name}',
    'toast.connected': 'Connected: {name}',
    'toast.disconnected': 'Connection closed',
    'toast.peerForgotten': 'Device forgotten',
    'run.connect': 'Connect',
    'run.disconnect': 'Disconnect',
    'run.forgetPeer': 'Forget device',
    'confirm.forgetPeer': 'Forget this device? You will need to pair again.',

    'stat.files': 'Files indexed',
    'stat.filesNote': '{n} on the phone',
    'stat.size': 'Size',
    'stat.ops': 'Journal entries',
    'stat.opsNote': 'scan {ago}',
    'stat.vault': 'In vault',

    'section.deletions': 'Deletions',
    'chip.hasPending': 'pending',
    'chip.clean': 'none',
    'section.devices': 'Devices',
    'section.folder': 'Folder',
    'pending.emptyTitle': 'Nothing waiting',
    'pending.emptyText': 'No files without a decision.',
    'pending.rowMeta': 'disappeared {ago}',
    'pending.toQueue': 'Open deletions',
    'devices.emptyTitle': 'No devices',
    'devices.emptyText': 'Add a computer in the “More” section.',
    'devices.pair': 'Pairing',
    'folder.notSelected': 'not selected',
    'folder.change': 'Change folder',

    'queue.kicker': 'Queue',
    'queue.title': 'Deletions',
    'queue.titleEmpty': 'Queue is empty',
    'queue.hint': 'A file disappeared from disk. Each one needs a decision.',
    'queue.keep': 'Keep here',
    'queue.delete': 'Delete everywhere',
    'queue.dismiss': 'Dismiss',
    'queue.explain': '“Keep here” removes the file from the library; “Delete everywhere” deletes it on all devices.',
    'queue.emptyText': 'Files that disappear from disk will appear here.',

    'conflicts.kicker': 'Conflicts',
    'conflicts.title': 'Conflicts',
    'conflicts.sub': 'One path changed on two devices. Both versions stay on disk.',
    'conflicts.rowMeta': 'two versions of the file',
    'conflicts.hint': 'The user chooses the version; the app does not overwrite files.',
    'conflicts.emptyTitle': 'No conflicts',
    'conflicts.emptyText': 'No files with the same path and different content.',

    'plan.title': 'Sync plan',
    'plan.sub': 'Changes from both sides. A decision applies to the whole selected group.',
    'plan.changes': 'Changes',
    'plan.thisDevice': 'This device',
    'plan.otherDevice': 'Other device',
    'plan.bothDevices': 'Both devices',
    'plan.filter.all': 'All',
    'plan.filter.added': 'Added',
    'plan.filter.updated': 'Updated',
    'plan.filter.moved': 'Moved',
    'plan.filter.deleted': 'Deleted',
    'plan.filter.conflicts': 'Conflicts',
    'plan.offline': 'No connection',
    'plan.offlineTitle': 'No connection',
    'plan.offlineText': 'Only this device’s changes are visible. Connect to a peer to compare both sides.',
    'plan.connect': 'Devices',
    'plan.offlineActions': 'Decisions are unavailable without a connection.',
    'plan.staleTitle': 'New changes',
    'plan.staleText': 'Files changed after the plan was built. The plan must be rebuilt.',
    'plan.staleAction': 'Refresh plan',
    'plan.staleActions': 'Refresh the plan to change decisions.',
    'plan.divergedTitle': 'Plan diverged',
    'plan.divergedText': 'The decision sets on the devices do not match. Reconnect and try again.',
    'plan.readonlyTitle': 'Transfer in progress',
    'plan.readonlyText': 'Files are being transferred. Decisions are temporarily unavailable.',
    'plan.readonlyActions': 'Decisions are unavailable during transfer.',
    'plan.errorTitle': 'Plan not built',
    'plan.selectAll': 'Select all',
    'plan.clear': 'Clear',
    'plan.selected.one': 'Selected: {n} file',
    'plan.selected.other': 'Selected: {n} files',
    'plan.unresolved.one': 'Needs a decision: {n}',
    'plan.unresolved.other': 'Need a decision: {n}',
    'plan.needsDecision': 'Needs a decision',
    'plan.decisionSet': 'Decision set',
    'plan.commit': 'Apply plan',
    'plan.emptyTitle': 'No changes',
    'plan.emptyText': 'Both sides match.',
    'plan.emptyLocalText': 'No local changes. Connect a device to compare libraries.',
    'plan.action.added.accept': 'Add to the other device',
    'plan.action.added.revert': 'Remove at the source',
    'plan.action.added.onlyHere': 'Keep at the source only',
    'plan.action.updated.accept': 'Apply on the other device',
    'plan.action.updated.revert': 'Restore the other device’s version',
    'plan.action.moved.accept': 'Move on the other device',
    'plan.action.moved.revert': 'Restore the old path at the source',
    'plan.action.deleted.accept': 'Delete on the other device too',
    'plan.action.deleted.revert': 'Restore the file at the source',
    'plan.action.deleted.onlyHere': 'Keep on the other device only',
    'plan.action.conflict.keep': 'Keep this version',
    'plan.action.conflict.accept': 'Take the other device’s version',
    'plan.action.conflict.keepBoth': 'Keep both versions',
    'alert.plan.title.one': '{n} change needs a decision',
    'alert.plan.title.other': '{n} changes need a decision',
    'alert.plan.text': 'Review the plan and choose what to apply.',
    'alert.plan.conflicts': 'There are conflicts: the plan cannot be applied without a decision.',
    'alert.plan.action': 'Open plan',
    'toast.planRefreshed': 'Plan refreshed',
    'toast.planStale': 'Plan not refreshed: reconnect from the PC',
    'toast.decisionsSaved.one': 'Saved a decision for {n} file',
    'toast.decisionsSaved.other': 'Saved decisions for {n} files',
    'run.planRefresh': 'Plan refresh',
    'run.commit': 'Apply plan',
    'run.decision': 'Decision',
    'confirm.commit': 'Apply the plan?\n\nThe selected decisions will run on both devices. Undecided deletions are not applied.',

    'vault.kicker': 'Vault',
    'vault.title': 'Vault',
    'vault.hint': 'Deleted files are kept for 30 days.',
    'vault.rowMeta': '{size} · deleted {ago}',
    'vault.restore': 'Restore',
    'vault.gc': 'Remove expired',
    'vault.emptyTitle': 'Vault is empty',
    'vault.emptyText': 'Deleted files will appear here.',

    'pair.kicker': 'Pairing',
    'pair.title': 'Pairing',
    'pair.hint': 'Show your code in the desktop app or scan the desktop QR code.',
    'pair.myCode': 'My code',
    'pair.scan': 'Scan QR',
    'pair.scanHint': 'Point the camera at the QR code in the desktop app.',
    'pair.scanCancel': 'Cancel',
    'pair.scanUnsupported': 'QR scanning is not supported. Paste the code manually.',
    'pair.scanDenied': 'No camera access. Allow it in Android settings.',
    'pair.codeLabel': 'Phone code',
    'pair.copy': 'Copy',
    'pair.inputLabel': 'Code from another device',
    'pair.submit': 'Pair',
    'pair.devices': 'Devices',
    'pair.noDevices': 'No paired devices.',
    'pair.library': 'Library',
    'pair.about': 'About',
    'pair.version': 'SynctMusic {v} · AGPL-3.0',
    'pair.device': 'device: {dev}',
    'pair.port': 'sync port: {port}',
    'pair.language': 'Language',

    'toast.needFolder': 'Choose a folder',
    'toast.initDone': 'Done: {n} {word} indexed',
    'toast.scan': 'Scan: +{added}, changed {changed}, missing {missing}',
    'toast.sync': 'Exchange: sent {sent}, received {received}',
    'toast.commit': 'Synced: accepted {accepted}, rejected {rejected}',
    'toast.syncPartial': 'Partial: {error}',
    'toast.gc': 'Removed from vault: {n}',
    'toast.paired': 'Paired: {dev}',
    'toast.deleted': 'Deleted everywhere',
    'toast.kept': 'File removed from the library',
    'toast.failed': 'Failed: {path}',
    'toast.forgotten': 'Entry removed from the queue',
    'toast.restored': 'Restored: {path}',
    'toast.needCode': 'Paste a code',

    'run.init': 'Library',
    'run.refresh': 'Refresh',
    'run.scan': 'Scan',
    'run.sync': 'Sync',
    'run.gc': 'Vault',
    'run.code': 'Code',
    'run.pair': 'Pairing',
    'run.resolve': 'Decision',
    'run.forget': 'Queue',
    'run.restore': 'Restore',

    'confirm.changeRoot': 'Choose another folder?\n\nThe index of the previous folder stays in the journal and its files go to the deletion queue.',
    'confirm.delete': 'Delete “{path}” on all devices?\n\nA copy stays in the vault for 30 days.',
  };

  const DICTS = { ru: RU, en: EN };
  const STORAGE_KEY = 'syncth.locale';
  const listeners = [];

  function detect() {
    try {
      const saved = localStorage.getItem(STORAGE_KEY);
      if (saved && DICTS[saved]) return saved;
    } catch {
      // хранилище недоступно — берём язык системы
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

  function plural(key, n) {
    const forms = raw(key);
    if (!Array.isArray(forms)) return String(forms);
    const form = formOf(n);
    if (locale === 'ru') {
      return forms[form === 'few' ? 1 : form === 'one' ? 0 : 2];
    }
    return forms[form === 'one' ? 0 : 1];
  }

  function tp(base, n, vars) {
    return t(`${base}.${formOf(n)}`, { n, ...(vars || {}) });
  }

  function num(value) {
    return Number(value || 0).toLocaleString(dateLocale());
  }

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
    if (!ms) return t('time.justNow');
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
    size,
    ago,
    apply,
    setLocale,
    onChange,
    dictFor(loc) { return DICTS[loc] || RU; },
    get locale() { return locale; },
    dateLocale,
  };
})();

if (typeof window !== 'undefined') window.I18N = I18N;
if (typeof globalThis !== 'undefined') globalThis.I18N = I18N;
