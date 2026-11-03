// Единственная точка, где интерфейс общается с Tauri. В обычном браузере
// (например, при просмотре собранного dist) моста нет — это не ошибка,
// а повод показать экран «нет связи с приложением».
const core = globalThis.__TAURI__?.core;

export const hasBridge = Boolean(core?.invoke);

export function invoke(command, args) {
  if (!core?.invoke) return Promise.reject(new Error('bridge'));
  return core.invoke(command, args);
}

const WINDOW_COMMANDS = {
  minimize: 'window_minimize',
  maximize: 'window_toggle_maximize',
  close: 'window_close',
};

export async function windowAction(action) {
  const command = WINDOW_COMMANDS[action];
  if (!command || !hasBridge) return;
  try {
    await invoke(command);
  } catch {
    // Окно могло уже закрыться; отдельного сообщения не нужно.
  }
}
