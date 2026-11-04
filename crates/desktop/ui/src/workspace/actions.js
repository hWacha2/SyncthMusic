import { hasBridge, invoke } from '../bridge.js';
import { t } from '../i18n/index.js';
import { number } from '../format.js';
import {
  closeWorkspace,
  discoverAll,
  errorMessage,
  openModal,
  refreshWorkspace,
  state,
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

export function askDelete(path, type = 'deletion') {
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
    label: t('workspace.attention.delete_everywhere'),
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

export async function restoreVault(path) {
  if (!hasBridge || state.workspace.action) return;
  state.workspace.action = `restore:${path}`;
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
    state.workspace.note = t('workspace.gc_done', {
      rows: number(result?.rows || 0),
      blobs: number(result?.blobs || 0),
    });
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

