<script setup>
import IconExclude from '../icons/IconExclude.vue';
import PathRow from './PathRow.vue';
import SortTabs from './SortTabs.vue';
import Spinner from '../Spinner.vue';
import { computed, ref, watch } from 'vue';
import { t } from '../../i18n/index.js';
import { ago, formatSize, number } from '../../format.js';
import { state } from '../../store.js';
import { groupPathsByFolder, sortByTime, sortEntries } from '../../workspace/model.js';
import { undoExclusion, undoExclusionMany } from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
});

const sort = ref('folders');
const sortOptions = [
  { id: 'folders', label: 'workspace.changes.sort_folders' },
  { id: 'alpha', label: 'workspace.changes.sort_alpha' },
  { id: 'time', label: 'workspace.sort.time' },
];

const rows = computed(() => (props.data?.states || []).filter((row) => row.excluded));
const busy = computed(() => Boolean(state.workspace.action || state.workspace.refreshing));

// `ExcludedLocal` — уцелевшая копия осталась на этом ПК, `ExcludedRemote` —
// на телефоне. Переключатель выбирает одну сторону, как в «Хранилище».
const groups = computed(() => [
  {
    side: 'local',
    title: t('workspace.exclusions.pc_group'),
    hint: t('workspace.exclusions.pc_hint'),
    items: rows.value.filter((row) => row.state === 'excluded_local'),
  },
  {
    side: 'remote',
    title: t('workspace.exclusions.phone_group'),
    hint: t('workspace.exclusions.phone_hint'),
    items: rows.value.filter((row) => row.state === 'excluded_remote'),
  },
]);

const side = ref('local');
const picked = ref(false);

// Если текущая сторона опустела, а на другой ещё есть исключения, сразу
// открываем непустую. Ручной выбор человека не перебиваем.
watch(rows, (list) => {
  if (picked.value || !list.length) return;
  const local = list.some((row) => row.state === 'excluded_local');
  const remote = list.some((row) => row.state === 'excluded_remote');
  if (side.value === 'local' && !local && remote) side.value = 'remote';
  else if (side.value === 'remote' && !remote && local) side.value = 'local';
}, { immediate: true });

function selectSide(next) {
  if (side.value === next) return;
  picked.value = true;
  side.value = next;
  // Скрытые строки не должны попасть под массовый возврат.
  selected.value = [];
  collapsed.value = new Set();
}

const active = computed(
  () => groups.value.find((group) => group.side === side.value) || groups.value[0],
);
const activeRows = computed(() => active.value.items);
const folderGroups = computed(() => groupPathsByFolder(activeRows.value));
const flatRows = computed(() => {
  if (sort.value === 'alpha') return sortEntries(activeRows.value);
  return sortByTime(activeRows.value, 'created_at');
});

function rowMeta(row) {
  return t('workspace.exclusions.excluded', { ago: ago(row.created_at) });
}

function undoing(path) {
  return state.workspace.action === `exclusion:${path}`;
}

// ---------------------------------------------------------------- выделение

const selected = ref([]);
const selectedSet = computed(() => new Set(selected.value));
const selectionActive = computed(() => selected.value.length > 0);
const selectedRows = computed(
  () => activeRows.value.filter((row) => selectedSet.value.has(row.path)),
);

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
  selected.value = activeRows.value.map((row) => row.path);
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

// После возврата строки исчезают из исключений — чистим и выделение.
watch(activeRows, (list) => {
  if (!selected.value.length) return;
  const alive = new Set(list.map((row) => row.path));
  const next = selected.value.filter((path) => alive.has(path));
  if (next.length !== selected.value.length) selected.value = next;
});

// ---------------------------------------------------------------- папки

// Как и в «Хранилище», папки сразу раскрыты: список обычно короткий.
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

// ---------------------------------------------------------------- массовый возврат

const bulkUndoing = computed(() => state.workspace.action === 'exclusion-many');

async function applyBulk() {
  const paths = selectedRows.value.map((row) => row.path);
  if (!paths.length) return;
  if (await undoExclusionMany(paths)) selected.value = [];
}
</script>

<template>
  <div class="ws-exclusions" :class="{ 'has-selection': selectionActive }">
    <section v-if="!rows.length" class="ws-card ws-empty-card">
      <span class="ws-empty-icon is-calm"><IconExclude /></span>
      <b>{{ t('workspace.exclusions.empty_title') }}</b>
      <p>{{ t('workspace.exclusions.empty_text') }}</p>
    </section>

    <template v-else>
      <section class="ws-card ws-vault-device">
        <div class="ws-segmented" role="group" :aria-label="t('workspace.exclusions.device')">
          <button
            type="button"
            class="ws-segment"
            :class="{ 'is-on': side === 'local' }"
            :aria-pressed="side === 'local' ? 'true' : 'false'"
            @click="selectSide('local')"
          >{{ t('workspace.vault.device_pc') }}</button>
          <button
            type="button"
            class="ws-segment"
            :class="{ 'is-on': side === 'remote' }"
            :aria-pressed="side === 'remote' ? 'true' : 'false'"
            @click="selectSide('remote')"
          >{{ t('workspace.vault.device_phone') }}</button>
        </div>
        <p class="ws-vault-hint">{{ active.hint }}</p>
      </section>

      <section class="ws-groups">
       <!-- <div class="ws-card-head">
          <h2 class="ws-card-title">{{ active.title }}</h2>
          <span class="ws-tag">
            {{ t('workspace.exclusions.count', { count: activeRows.length }) }}
          </span>
        </div> -->

        <p v-if="!activeRows.length" class="ws-empty-line">
          {{ t('workspace.exclusions.side_empty') }}
        </p>

        <template v-else>
          <div class="ws-sort-row">
            <SortTabs v-model="sort" :options="sortOptions" />
            <div class="ws-sort-tools">
              <button
                v-if="sort === 'folders' && folderGroups.length"
                class="ws-link"
                @click="toggleExpandAll"
              >{{ allExpanded ? t('workspace.changes.collapse_all') : t('workspace.changes.expand_all') }}</button>
              <button
                v-if="activeRows.length"
                class="ws-link"
                @click="selectAll"
              >{{ t('workspace.changes.select_all') }}</button>
            </div>
          </div>

          <div v-if="sort === 'folders'" class="ws-card ws-folders ">
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
                  kind-class="is-excluded"
                  :meta="rowMeta(row)"
                  :action-label="t('workspace.exclusions.undo')"
                  :action-busy="undoing(row.path)"
                  :busy="busy"
                  :selection="selectionActive"
                  :selected="selectedSet.has(row.path)"
                  show-folder
                  @toggle="toggleRow(row)"
                  @action="undoExclusion(row.path)"
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
              kind-class="is-excluded"
              :meta="rowMeta(row)"
              :action-label="t('workspace.exclusions.undo')"
              :action-busy="undoing(row.path)"
              :busy="busy"
              :selection="selectionActive"
              :selected="selectedSet.has(row.path)"
              @toggle="toggleRow(row)"
              @action="undoExclusion(row.path)"
            />
          </div>
        </template>
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
            <Spinner v-if="bulkUndoing" />{{ t('workspace.exclusions.undo_selected') }}
          </button>
        </div>
      </div>
    </Teleport>
  </div>
</template>
