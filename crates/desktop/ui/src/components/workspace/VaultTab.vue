<script setup>
import IconVault from '../icons/IconVault.vue';
import PathRow from './PathRow.vue';
import SortTabs from './SortTabs.vue';
import Spinner from '../Spinner.vue';
import { computed, ref, watch } from 'vue';
import { t } from '../../i18n/index.js';
import { ago, formatSize, number } from '../../format.js';
import { state } from '../../store.js';
import { groupPathsByFolder, sortByTime, sortEntries } from '../../workspace/model.js';
import {
  askClearVault,
  askGc,
  askPhoneVaultClear,
  askPhoneVaultGc,
  loadPhoneVault,
  restorePhoneVault,
  restorePhoneVaultMany,
  restoreVault,
  restoreVaultMany,
} from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
});

// Хранилище выглядит одинаково для обоих устройств: переключатель меняет
// только источник строк и адресата команд очистки.
const device = ref('pc');
const onPhone = computed(() => device.value === 'phone');

const sort = ref('folders');
const sortOptions = [
  { id: 'folders', label: 'workspace.changes.sort_folders' },
  { id: 'alpha', label: 'workspace.changes.sort_alpha' },
  { id: 'time', label: 'workspace.sort.time' },
];

const pcRows = computed(() => props.data?.vault || []);
const phoneRows = computed(() => state.workspace.phoneVault || []);
const rows = computed(() => (onPhone.value ? phoneRows.value : pcRows.value));
const folderGroups = computed(() => groupPathsByFolder(rows.value));
const flatRows = computed(() => {
  if (sort.value === 'alpha') return sortEntries(rows.value);
  return sortByTime(rows.value, 'deleted_at');
});

const busy = computed(() => Boolean(state.workspace.action || state.workspace.refreshing));
const gc = computed(() => state.workspace.action === (onPhone.value ? 'phone-vault-gc' : 'gc'));
const clearing = computed(
  () => state.workspace.action === (onPhone.value ? 'phone-vault-clear' : 'clear-vault'),
);

const phoneLinked = computed(() => Boolean(props.data?.phone?.linked));
const phoneConnected = computed(() => Boolean(props.data?.phone?.connected));
const phoneLoading = computed(() => state.workspace.phoneVaultLoading);
const phoneError = computed(() => state.workspace.phoneVaultError);

// Очистка доступна только когда список этого устройства реально прочитан.
const canClear = computed(() => {
  if (!rows.value.length) return false;
  if (!onPhone.value) return true;
  return phoneConnected.value && !phoneLoading.value && !phoneError.value;
});

const hint = computed(() =>
  onPhone.value ? t('workspace.phone_vault.hint') : t('workspace.vault.hint'),
);
const panel = computed(() =>
  onPhone.value ? t('workspace.phone_vault.panel') : t('workspace.vault.panel'),
);
const restoreLabel = computed(() =>
  onPhone.value ? t('workspace.phone_vault.restore') : t('workspace.vault.restore'),
);
const restoreSelectedLabel = computed(() =>
  onPhone.value
    ? t('workspace.phone_vault.restore_selected')
    : t('workspace.vault.restore_selected'),
);

function selectDevice(next) {
  if (next === 'phone' && !phoneLinked.value) return;
  device.value = next;
  // Скрытые строки не должны попасть под массовое восстановление.
  selected.value = [];
  collapsed.value = new Set();
  // Список телефона уже загружен при входе в рабочую область. Здесь только
  // добираем его, если кэша ещё нет; после удалений и восстановлений его
  // обновляют сами действия, а не каждое переключение вкладки.
  if (next === 'phone') loadPhoneVault();
}

function askGcDevice() {
  if (onPhone.value) askPhoneVaultGc();
  else askGc();
}

function askClearDevice() {
  if (onPhone.value) askPhoneVaultClear();
  else askClearVault();
}

function restore(path) {
  if (onPhone.value) restorePhoneVault(path);
  else restoreVault(path);
}

function restoring(path) {
  const prefix = onPhone.value ? 'restore-phone' : 'restore';
  return state.workspace.action === `${prefix}:${path}`;
}

function rowMeta(row) {
  return t('workspace.vault.deleted', { ago: ago(row.deleted_at) });
}

// ---------------------------------------------------------------- выделение

const selected = ref([]);
const selectedSet = computed(() => new Set(selected.value));
const selectionActive = computed(() => selected.value.length > 0);
const selectedRows = computed(() => rows.value.filter((row) => selectedSet.value.has(row.path)));

function toggleRow(row) {
  const next = new Set(selected.value);
  if (next.has(row.path)) next.delete(row.path);
  else next.add(row.path);
  selected.value = [...next];
}

function clearSelection() {
  selected.value = [];
}

function selectAll() {
  selected.value = rows.value.map((row) => row.path);
}

function folderSelection(folder) {
  const chosen = folder.items.filter((row) => selectedSet.value.has(row.path)).length;
  return {
    total: folder.items.length,
    checked: folder.items.length > 0 && chosen === folder.items.length,
    partial: chosen > 0 && chosen < folder.items.length,
  };
}

function toggleFolder(folder) {
  const next = new Set(selected.value);
  const items = folder.items;
  if (!items.length) return;
  const all = items.every((row) => next.has(row.path));
  for (const row of items) {
    if (all) next.delete(row.path);
    else next.add(row.path);
  }
  selected.value = [...next];
}

// После восстановления строки исчезают из корзины — чистим и выделение.
watch(rows, (list) => {
  if (!selected.value.length) return;
  const alive = new Set(list.map((row) => row.path));
  const next = selected.value.filter((path) => alive.has(path));
  if (next.length !== selected.value.length) selected.value = next;
});

// Если телефон отключился, список корзины скрыт: массовая панель не должна
// предлагать действия над невидимыми строками.
watch(phoneConnected, (connected) => {
  if (onPhone.value && !connected) selected.value = [];
});

// ---------------------------------------------------------------- папки

// В отличие от «Изменений» папки сразу раскрыты: корзина обычно короче, и
// человеку нужен список файлов, а не ещё один клик.
const collapsed = ref(new Set());
const allExpanded = computed(
  () => folderGroups.value.length > 0
    && folderGroups.value.every((folder) => !collapsed.value.has(folder.folder)),
);

function isExpanded(folder) {
  return !collapsed.value.has(folder.folder);
}

function toggleExpanded(folder) {
  const next = new Set(collapsed.value);
  if (next.has(folder.folder)) next.delete(folder.folder);
  else next.add(folder.folder);
  collapsed.value = next;
}

function toggleExpandAll() {
  collapsed.value = allExpanded.value
    ? new Set(folderGroups.value.map((folder) => folder.folder))
    : new Set();
}

// ---------------------------------------------------------------- массовое восстановление

const bulkRestoring = computed(
  () => state.workspace.action === (onPhone.value ? 'restore-phone-many' : 'restore-many'),
);

async function applyBulk() {
  const paths = selectedRows.value.map((row) => row.path);
  if (!paths.length) return;
  const ok = onPhone.value
    ? await restorePhoneVaultMany(paths)
    : await restoreVaultMany(paths);
  if (ok) selected.value = [];
}
</script>

<template>
  <div class="ws-vault" :class="{ 'has-selection': selectionActive }">
    <section class="ws-card ws-vault-device">
      <div class="ws-segmented" role="group" :aria-label="t('workspace.vault.device')">
        <button
          type="button"
          class="ws-segment"
          :class="{ 'is-on': !onPhone }"
          :aria-pressed="onPhone ? 'false' : 'true'"
          @click="selectDevice('pc')"
        >
          {{ t('workspace.vault.device_pc') }}
        </button>
        <button
          type="button"
          class="ws-segment"
          :class="{ 'is-on': onPhone }"
          :aria-pressed="onPhone ? 'true' : 'false'"
          :disabled="!phoneLinked"
          @click="selectDevice('phone')"
        >
          {{ t('workspace.vault.device_phone') }}
        </button>
      </div>
      <p class="ws-vault-hint">{{ hint }}</p>
    </section>

    <template v-if="onPhone && !phoneConnected">
      <section class="ws-card ws-empty-card">
        <span class="ws-empty-icon is-calm"><IconVault /></span>
        <b>{{ t('workspace.phone_vault.offline_title') }}</b>
        <p>{{ t('workspace.phone_vault.offline_text') }}</p>
      </section>
    </template>

    <template v-else-if="onPhone && phoneError && !rows.length">
      <section class="ws-card ws-empty-card">
        <span class="ws-empty-icon is-calm"><IconVault /></span>
        <b>{{ panel }}</b>
        <p>{{ t('workspace.error.phone_vault', { error: phoneError }) }}</p>
        <button class="btn btn--quiet btn--small" @click="loadPhoneVault(true)">
          {{ t('workspace.phone_vault.retry') }}
        </button>
      </section>
    </template>

    <template v-else-if="onPhone && phoneLoading && !rows.length">
      <section class="ws-card ws-empty-card">
        <span class="ws-empty-icon is-calm"><IconVault /></span>
        <b>{{ panel }}</b>
        <p>{{ t('workspace.phone_vault.loading') }}</p>
      </section>
    </template>

    <template v-else-if="!rows.length">
      <section class="ws-card ws-empty-card">
        <span class="ws-empty-icon is-calm"><IconVault /></span>
        <b>{{ onPhone ? t('workspace.phone_vault.empty_title') : t('workspace.vault.empty_title') }}</b>
        <p>{{ onPhone ? t('workspace.phone_vault.empty_text') : t('workspace.vault.empty_text') }}</p>
      </section>
    </template>

    <template v-else>
      <section class="ws-card">
        <div class="ws-card-head">
          <h2 class="ws-card-title">{{ panel }}</h2>
          <span class="ws-tag">{{ t('workspace.vault.count', { count: rows.length }) }}</span>
        </div>
        <!-- Пока список обновляется, уже прочитанные строки остаются на месте:
             иначе при каждом входе на вкладку телефона мелькает пустой экран. -->
        <p v-if="onPhone && phoneError" class="ws-card-hint">
          {{ t('workspace.error.phone_vault', { error: phoneError }) }}
        </p>
        <div class="ws-sort-row ws-card-sort">
          <SortTabs v-model="sort" :options="sortOptions" />
          <div class="ws-sort-tools">
            <button
              v-if="sort === 'folders' && folderGroups.length"
              class="ws-link"
              @click="toggleExpandAll"
            >{{ allExpanded ? t('workspace.changes.collapse_all') : t('workspace.changes.expand_all') }}</button>
            <button
              v-if="rows.length"
              class="ws-link"
              @click="selectAll"
            >{{ t('workspace.changes.select_all') }}</button>
          </div>
        </div>

        <div v-if="sort === 'folders'" class="ws-folders ws-path-folders">
          <div
            v-for="folder in folderGroups"
            :key="folder.folder"
            class="ws-folder"
            :class="{ 'is-open': isExpanded(folder) }"
          >
            <div class="ws-folder-head">
              <button
                type="button"
                class="ws-check"
                :class="{
                  'is-on': folderSelection(folder).checked,
                  'is-partial': folderSelection(folder).partial,
                }"
                role="checkbox"
                :aria-checked="folderSelection(folder).checked ? 'true' : 'false'"
                :aria-label="folder.folder || t('workspace.changes.root')"
                @click="toggleFolder(folder)"
              ></button>
              <button type="button" class="ws-folder-toggle" @click="toggleExpanded(folder)">
                <span class="ws-folder-arrow" aria-hidden="true">›</span>
                <span class="ws-folder-name" :title="folder.folder || ''">
                  {{ folder.folder || t('workspace.changes.root') }}
                </span>
                <span class="ws-folder-meta">{{ formatSize(folder.size) }}</span>
                <span class="ws-folder-count">{{ number(folder.items.length) }}</span>
              </button>
            </div>
            <div v-if="isExpanded(folder)" class="ws-rows ws-folder-rows">
              <PathRow
                v-for="row in folder.items"
                :key="row.path"
                :path="row.path"
                :kind="formatSize(row.size)"
                kind-class="is-vault"
                :meta="rowMeta(row)"
                :action-label="restoreLabel"
                :action-busy="restoring(row.path)"
                :busy="busy"
                :selection="selectionActive"
                :selected="selectedSet.has(row.path)"
                show-folder
                @toggle="toggleRow(row)"
                @action="restore(row.path)"
              />
            </div>
          </div>
        </div>

        <div v-else class="ws-rows">
          <PathRow
            v-for="row in flatRows"
            :key="row.path"
            :path="row.path"
            :kind="formatSize(row.size)"
            kind-class="is-vault"
            :meta="rowMeta(row)"
            :action-label="restoreLabel"
            :action-busy="restoring(row.path)"
            :busy="busy"
            :selection="selectionActive"
            :selected="selectedSet.has(row.path)"
            @toggle="toggleRow(row)"
            @action="restore(row.path)"
          />
        </div>
      </section>

      <section class="ws-card ws-vault-actions">
        <div class="ws-vault-action">
          <div class="ws-setting-copy">
            <span class="ws-setting-title">{{ t('workspace.vault.old_title') }}</span>
            <span class="ws-setting-hint">{{ t('workspace.vault.old_text') }}</span>
          </div>
          <button
            class="btn btn--quiet btn--small"
            :disabled="busy || !canClear"
            @click="askGcDevice"
          >
            <Spinner v-if="gc" />{{ t('workspace.vault.gc') }}
          </button>
        </div>
        <div class="ws-vault-action">
          <div class="ws-setting-copy">
            <span class="ws-setting-title">{{ t('workspace.vault.all_title') }}</span>
            <span class="ws-setting-hint">{{ t('workspace.vault.all_text') }}</span>
          </div>
          <button
            class="btn btn--quiet btn--danger btn--small"
            :disabled="busy || !canClear"
            @click="askClearDevice"
          >
            <Spinner v-if="clearing" />{{ t('workspace.vault.clear_all') }}
          </button>
        </div>
      </section>
    </template>

    <Teleport to="body">
      <div v-if="selectedRows.length" class="ws-bulk">
        <div class="ws-bulk-info">
          <span class="ws-bulk-count">
            {{ t('workspace.selection.count', { count: selectedRows.length }) }}
          </span>
          <button type="button" class="ws-bulk-clear" @click="clearSelection">
            {{ t('workspace.selection.clear') }}
          </button>
        </div>
        <div class="ws-bulk-actions">
          <button
            type="button"
            class="btn btn--small"
            :disabled="busy"
            @click="applyBulk"
          >
            <Spinner v-if="bulkRestoring" />{{ restoreSelectedLabel }}
          </button>
        </div>
      </div>
    </Teleport>
  </div>
</template>
