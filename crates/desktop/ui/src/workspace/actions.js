import { hasBridge, invoke } from '../bridge.js';
import { t } from '../i18n/index.js';
import { number } from '../format.js';
import {
  closeWorkspace,
  discoverAll,
  errorMessage,
  openModal,
  refreshWorkspace,
  startProgressWatch,
  state,
  stopProgressWatch,
} from '../store.js';
import { canCommit } from './model.js';
import { isWorkspaceGeneration, nextWorkspaceGeneration } from './generation.js';

// Действия рабочей области повторяют app.js из ui-archive, но без ручной
// перерисовки: состояние реактивно, а окно подтверждения — общий ModalLayer.

export function selectTab(tab) {
  if (state.workspace.tab === tab) return;
  state.workspace.tab = tab;
}

export async function decide(path, action) {
  if (!hasBridge || state.workspace.action || state.workspace.refreshing) return;
  const gen = nextWorkspaceGeneration();
  state.workspace.action = `decide:${path}:${action}`;
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  try {
    const data = await invoke('workspace_decide', { path, action });
    if (isWorkspaceGeneration(gen)) state.workspace.data = data;
  } catch (error) {
    state.workspace.note = t('workspace.error.decision', { error: errorMessage(error) });
    state.workspace.noteError = true;
  } finally {
    state.workspace.action = '';
  }
}

export async function undecide(path) {
  if (!hasBridge || state.workspace.action || state.workspace.refreshing) return;
  const gen = nextWorkspaceGeneration();
  state.workspace.action = `undecide:${path}`;
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  try {
    const data = await invoke('workspace_undecide', { path });
    if (isWorkspaceGeneration(gen)) state.workspace.data = data;
  } catch (error) {
    state.workspace.note = t('workspace.error.decision', { error: errorMessage(error) });
    state.workspace.noteError = true;
  } finally {
    state.workspace.action = '';
  }
}

export async function decideMany(paths, action) {
  if (!hasBridge || !paths.length || state.workspace.action || state.workspace.refreshing) {
    return false;
  }
  const gen = nextWorkspaceGeneration();
  state.workspace.action = `decide-many:${action}`;
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  try {
    const data = await invoke('workspace_decide_many', { paths, action });
    if (isWorkspaceGeneration(gen)) state.workspace.data = data;
    return true;
  } catch (error) {
    state.workspace.note = t('workspace.error.decision', { error: errorMessage(error) });
    state.workspace.noteError = true;
    return false;
  } finally {
    state.workspace.action = '';
  }
}

export function askDelete(path, type = 'deletion', label = '') {
  const kicker = type === 'addition'
    ? t('workspace.attention.addition')
    : type === 'conflict'
      ? t('workspace.attention.conflict')
      : t('workspace.attention.deletion');
  openModal({
    kicker,
    title: t('workspace.modal.delete_title'),
    text: t('workspace.modal.delete_text'),
    path,
    label: label || t('workspace.attention.delete_everywhere'),
    danger: true,
    onConfirm: () => decide(path, 'delete_everywhere'),
  });
}

export function askForget() {
  openModal({
    title: t('workspace.modal.forget_title'),
    text: t('workspace.modal.forget_text'),
    label: t('workspace.forget'),
    danger: true,
    onConfirm: () => forgetPhone(),
  });
}

export function askBulkDelete(paths, label = '') {
  openModal({
    title: t('workspace.selection.delete_title'),
    text: t('workspace.selection.delete_text', { count: paths.length }),
    label: label || t('workspace.attention.delete_files'),
    danger: true,
    onConfirm: () => decideMany(paths, 'delete_everywhere'),
  });
}

export function askGc() {
  openModal({
    kicker: t('workspace.tab.vault'),
    title: t('workspace.modal.gc_title'),
    text: t('workspace.modal.gc_text'),
    label: t('workspace.vault.gc'),
    danger: true,
    onConfirm: () => gcVault(),
  });
}

export function askClearVault() {
  openModal({
    kicker: t('workspace.tab.vault'),
    title: t('workspace.modal.clear_vault_title'),
    text: t('workspace.modal.clear_vault_text'),
    label: t('workspace.modal.clear_vault_label'),
    danger: true,
    onConfirm: () => clearVault(),
  });
}

export function askClearHistory() {
  openModal({
    kicker: t('workspace.tab.settings'),
    title: t('workspace.modal.clear_history_title'),
    text: t('workspace.modal.clear_history_text'),
    label: t('workspace.settings.clear_history'),
    danger: true,
    onConfirm: () => clearHistory(),
  });
}

/// Сбрасывает журнал и планы на ПК и телефоне. Файлы и «Хранилище» остаются.
export async function clearHistory() {
  if (!hasBridge || state.workspace.action) return;
  const gen = nextWorkspaceGeneration();
  state.workspace.action = 'clear-history';
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  // Старые цифры относятся к прежней истории: пока идёт сброс и проверка,
  // показываем прогресс, а не устаревшую статистику. Сам объект данных
  // оставляем для нижнего индикатора связи.
  state.workspace.stale = true;
  startProgressWatch();
  try {
    await invoke('workspace_clear_history');
    if (!isWorkspaceGeneration(gen)) return;
    state.workspace.note = t('workspace.history_cleared');
    state.workspace.noteError = false;
  } catch (error) {
    state.workspace.stale = false;
    state.workspace.note = t('workspace.error.clear_history', { error: errorMessage(error) });
    state.workspace.noteError = true;
    return;
  } finally {
    state.workspace.action = '';
  }
  // Проверка — отдельный шаг: кнопка очистки успевает вернуться в обычное
  // состояние, а прогресс и новые цифры строит уже «Проверить».
  await refreshWorkspace({ keepNote: true });
  stopProgressWatch();
}

export async function commitWorkspace() {
  if (!hasBridge || !canCommit()) return;
  nextWorkspaceGeneration();
  state.workspace.action = 'commit';
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  let outcome = null;
  try {
    outcome = await invoke('workspace_commit');
  } catch (error) {
    state.workspace.action = '';
    state.workspace.note = t('workspace.error.commit', { error: errorMessage(error) });
    state.workspace.noteError = true;
    return;
  }
  state.workspace.action = '';
  state.workspace.data = null;
  state.workspace.note = t('workspace.commit_done', {
    accepted: number(outcome?.accepted || 0),
    rejected: number(outcome?.rejected || 0),
  });
  state.workspace.noteError = false;
  state.workspace.tab = 'overview';
  await refreshWorkspace({ keepNote: true });
}

/// Возвращает путь в синхронизацию. Ничего не решает автоматически: путь
/// снова появится в «Изменениях» после следующей проверки.
export async function undoExclusion(path) {
  if (!hasBridge || state.workspace.action || state.workspace.refreshing) return;
  const gen = nextWorkspaceGeneration();
  state.workspace.action = `exclusion:${path}`;
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  try {
    const data = await invoke('workspace_exclusion_undo', { path });
    if (isWorkspaceGeneration(gen)) state.workspace.data = data;
    state.workspace.note = t('workspace.exclusion_undone', { path });
    state.workspace.noteError = false;
  } catch (error) {
    state.workspace.note = t('workspace.error.exclusion', { error: errorMessage(error) });
    state.workspace.noteError = true;
  } finally {
    state.workspace.action = '';
  }
}

export async function restoreVault(path) {
  if (!hasBridge || state.workspace.action) return;  state.workspace.action = `restore:${path}`;
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  try {
    await invoke('workspace_restore', { path });
    state.workspace.note = t('workspace.restore_done', { path });
    state.workspace.noteError = false;
  } catch (error) {
    state.workspace.note = t('workspace.error.restore', { error: errorMessage(error) });
    state.workspace.noteError = true;
  } finally {
    state.workspace.action = '';
  }
  await refreshWorkspace({ keepNote: true });
}

export async function gcVault() {
  if (!hasBridge || state.workspace.action) return;
  state.workspace.action = 'gc';
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  const gen = nextWorkspaceGeneration();
  try {
    const result = await invoke('workspace_gc', { ttlDays: 30 });
    const rows = number(result?.rows || 0);
    const blobs = number(result?.blobs || 0);
    state.workspace.note = rows || blobs
      ? t('workspace.gc_done', { rows, blobs })
      : t('workspace.gc_empty');
    state.workspace.noteError = false;
    const data = await invoke('workspace_status');
    if (isWorkspaceGeneration(gen)) state.workspace.data = data;
  } catch (error) {
    state.workspace.note = t('workspace.error.gc', { error: errorMessage(error) });
    state.workspace.noteError = true;
  } finally {
    state.workspace.action = '';
  }
}

/// Полностью очищает корзину. В отличие от `gcVault`, не смотрит на возраст.
export async function clearVault() {
  if (!hasBridge || state.workspace.action) return;
  state.workspace.action = 'clear-vault';
  state.workspace.note = '';
  state.workspace.noteError = false;
  state.workspace.error = '';
  const gen = nextWorkspaceGeneration();
  try {
    const result = await invoke('workspace_clear_vault');
    state.workspace.note = t('workspace.clear_vault_done', {
      blobs: number(result?.blobs || 0),
    });
    state.workspace.noteError = false;
    const data = await invoke('workspace_status');
    if (isWorkspaceGeneration(gen)) state.workspace.data = data;
  } catch (error) {
    state.workspace.note = t('workspace.error.clear_vault', { error: errorMessage(error) });
    state.workspace.noteError = true;
  } finally {
    state.workspace.action = '';
  }
}

export async function toggleManual() {
  if (!hasBridge || state.workspace.action || state.workspace.refreshing) return;
  const enabled = !(state.workspace.data && state.workspace.data.manual_additions);
  const gen = nextWorkspaceGeneration();
  state.workspace.action = 'manual';
  state.workspace.note = '';
  state.workspace.noteError = false;
  try {
    await invoke('workspace_set_manual', { enabled });
    if (!isWorkspaceGeneration(gen)) return;
    if (state.workspace.data) state.workspace.data.manual_additions = enabled;
    state.workspace.note = t(enabled ? 'workspace.manual.on' : 'workspace.manual.off');
  } catch (error) {
    state.workspace.note = t('workspace.error.manual', { error: errorMessage(error) });
    state.workspace.noteError = true;
  } finally {
    state.workspace.action = '';
  }
}

export async function forgetPhone() {
  if (state.phoneBusy || !hasBridge) return;
  state.phoneBusy = 'forget';
  state.note = '';
  state.error = false;
  try {
    await invoke('agent_forget');
    await closeWorkspace();
    state.phone = { linked: false };
    state.view = 'devices';
    await discoverAll();
  } catch (error) {
    state.error = true;
    state.note = t('phone.error', { error: errorMessage(error) });
  } finally {
    state.phoneBusy = '';
  }
}

