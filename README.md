# SynctMusic

Синхронизация музыкальной библиотеки между ПК и Android-телефоном по локальной сети (или USB).

Приложение поддерживает одинаковую структуру папок на обоих устройствах, сохраняет имена и даты файлов и ничего не удаляет без подтверждения.

## Как работает

- Синхронизацию запускает ПК, телефон принимает входящий сеанс.
- Подключение по LAN или USB (`adb`), первое подключение подтверждается на
  телефоне.
- Скан инкрементальный: размер и `mtime`, содержимое проверяется по BLAKE3.
- Удалённый файл сначала попадает в корзину устройства на 30 дней.
- Конфликты не перезаписываются: вторая версия остаётся рядом копией
  `*.sync-conflict-*`.
- Файл можно оставить только на одном устройстве — через «Исключения».

## Сборка

Нужны Rust 1.75+, Node.js, Tauri CLI 2. Для Android дополнительно JDK, Android
SDK и NDK.

### ПК

```powershell
npm --prefix crates\desktop\ui run build
cargo build -p syncth-desktop --release
.\target\release\syncth-desktop.exe
```

`ui/dist` встраивается в бинарник, поэтому интерфейс нужно собирать до
`cargo build`.

### Android

Окружение ставится скриптом:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\android-setup.ps1
```

Сборка APK под arm64:

```powershell
. "$env:LOCALAPPDATA\SyncthMusic\android\android-env.ps1"
cd crates\mobile
cargo tauri android build --apk --split-per-abi --target aarch64 --ci
```

APK появится в
`crates\mobile\src-tauri\gen\android\app\build\outputs\apk\arm64\release\`.

### CLI

```powershell
cargo build --release
.\target\release\syncth.exe --help
```

CLI нужен для диагностики и подготовки библиотеки, полный обмен запускает
только ПК-клиент.

## Как пользоваться

1. Установить ПК-клиент и APK.
2. Выбрать папку с музыкой на обоих устройствах, которая должна быть одинаковой.
3. Подключить телефон по LAN или USB и подтвердить подключение на телефоне.
4. Нажать "Проверить", посмотреть план и применить решения.
5. Нажать "Синхронизировать"

Новые файлы и правки можно принимать автоматически в настройках ПК. Удаления и
конфликты всегда требуют решения человека.

## Структура

- `crates/core` — ядро: скан, операции, план, commit, сеть, корзина.
- `crates/desktop` — Tauri-клиент для ПК.
- `crates/mobile` — Tauri-клиент для Android.
- `crates/cli` — диагностика и подготовка библиотеки.
- `docs` — архитектура, план и пограничные случаи.

## Тесты

```powershell
cargo test --workspace
```

## Лицензия

AGPL-3.0-or-later.
