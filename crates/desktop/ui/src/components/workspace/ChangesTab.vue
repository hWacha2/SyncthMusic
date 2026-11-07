<script setup>
import ChangeRow from './ChangeRow.vue';
import IconCheck from '../icons/IconCheck.vue';
import IconList from '../icons/IconList.vue';
import Spinner from '../Spinner.vue';
import { computed, onMounted, ref, watch } from 'vue';
import { t } from '../../i18n/index.js';
import { number } from '../../format.js';
import { state } from '../../store.js';
import { openDevices, reconnect, refreshWorkspace } from '../../store.js';
import {
  changeEntries,
  groupByFolder,
  groupByType,
  kindLabel,
  manualOpenSet,
  planData,
  sortEntries,
} from '../../workspace/model.js';
import { askBulkDelete, decideMany, toggleManualOpen } from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
});

const plan = computed(() => planData(props.data));
const entries = computed(() => changeEntries(props.data));
const openSet = computed(() => manualOpenSet(props.data));
const aligned = computed(() => Number(plan.value?.aligned) || 0);
const phoneLinked = computed(() => Boolean(props.data?.phone?.linked));
const phoneReady = computed(() => Boolean(props.data?.phone?.connected));

const remote = computed(() => entries.value.filter(
  (entry) => entry.type !== 'conflict' && entry.side === 'remote',
));
const local = computed(() => entries.value.filter(
  (entry) => entry.type !== 'conflict' && entry.side === 'local',
));
const conflicts = computed(() => entries.value.filter((entry) => entry.type === 'conflict'));

const counts = computed(() => ({
  remote: remote.value.length,
  local: local.value.length,
  conflicts: conflicts.value.length,
}));
const pendingCounts = computed(() => ({
  remote: remote.value.filter((entry) => entry.pending).length,
  local: local.value.filter((entry) => entry.pending).length,
  conflicts: conflicts.value.filter((entry) => entry.pending).length,
}));

const hasAny = computed(() => Boolean(entries.value.length || aligned.value));

const side = computed(() => {
  const value = state.workspace.changesSide;
  return value === 'local' || value === 'conflicts' ? value : 'remote';
});
const sort = computed(() => {
  const value = state.workspace.changesSort;
  return value === 'alpha' || value === 'type' ? value : 'folders';
});

const activeEntries = computed(() => {
  if (side.value === 'local') return local.value;
  if (side.value === 'conflicts') return conflicts.value;
  return remote.value;
});

const activePending = computed(() => activeEntries.value.filter((entry) => entry.pending).length);
const folderGroups = computed(() => groupByFolder(activeEntries.value));
const typeGroups = computed(() => groupByType(activeEntries.value));
const alphaEntries = computed(() => sortEntries(activeEntries.value));

const sides = [
  { id: 'remote', label: 'workspace.changes.remote' },
  { id: 'local', label: 'workspace.changes.local' },
  { id: 'conflicts', label: 'workspace.changes.conflicts' },
];
const sorts = [
  { id: 'folders', label: 'workspace.changes.sort_folders' },
  { id: 'alpha', label: 'workspace.changes.sort_alpha' },
  { id: 'type', label: 'workspace.changes.sort_type' },
];

// При входе открываем первое непустое устройство, но выбор человека не
// перебиваем, если он уже переключался.
onMounted(() => {
  if (activeEntries.value.length) return;
  if (remote.value.length) state.workspace.changesSide = 'remote';
  else if (local.value.length) state.workspace.changesSide = 'local';
  else if (conflicts.value.length) state.workspace.changesSide = 'conflicts';
});

// ---------------------------------------------------------------- выделение

const selected = ref([]);
const selectedSet = computed(() => new Set(selected.value));
const selectionActive = computed(() => selected.value.length > 0);
const selectedEntries = computed(() => activeEntries.value.filter(
  (entry) => entry.pending && selectedSet.value.has(entry.path),
));

function setSide(next) {
  if (side.value === next) return;
  state.workspace.changesSide = next;
  // Скрытые строки не должны попасть под массовое решение.
  selected.value = [];
}

function setSort(next) {
  state.workspace.changesSort = next;
}

function toggleEntry(entry) {
  const next = new Set(selected.value);
  if (next.has(entry.path)) next.delete(entry.path);
  else next.add(entry.path);
  selected.value = [...next];
}

function clearSelection() {
  selected.value = [];
}

function selectAll() {
  selected.value = activeEntries.value
    .filter((entry) => entry.pending)
    .map((entry) => entry.path);
}

function folderSelection(folder) {
  const items = folder.pendingItems;
  const chosen = items.filter((entry) => selectedSet.value.has(entry.path)).length;
  return {
    total: items.length,
    checked: items.length > 0 && chosen === items.length,
    partial: chosen > 0 && chosen < items.length,
  };
}

function toggleFolder(folder) {
  const next = new Set(selected.value);
  const items = folder.pendingItems;
  if (!items.length) return;
  const all = items.every((entry) => next.has(entry.path));
  for (const entry of items) {
    if (all) next.delete(entry.path);
    else next.add(entry.path);
  }
  selected.value = [...next];
}

function typeSelection(group) {
  const items = group.items.filter((entry) => entry.pending);
  const chosen = items.filter((entry) => selectedSet.value.has(entry.path)).length;
  return {
    total: items.length,
    checked: items.length > 0 && chosen === items.length,
    partial: chosen > 0 && chosen < items.length,
  };
}

function toggleTypeSelection(group) {
  const next = new Set(selected.value);
  const items = group.items.filter((entry) => entry.pending);
  if (!items.length) return;
  const all = items.every((entry) => next.has(entry.path));
  for (const entry of items) {
    if (all) next.delete(entry.path);
    else next.add(entry.path);
  }
  selected.value = [...next];
}

// После решения пути перестают быть pending — убираем их из выделения.
watch(activeEntries, (list) => {
  if (!selected.value.length) return;
  const alive = new Set(list.filter((entry) => entry.pending).map((entry) => entry.path));
  const next = selected.value.filter((path) => alive.has(path));
  if (next.length !== selected.value.length) selected.value = next;
});

// ---------------------------------------------------------------- массовые решения

const ACTION_SETS = {
  deletion: ['keep_in_library', 'only_here', 'delete_everywhere'],
  addition: ['accept', 'only_here', 'delete_everywhere'],
  change: ['accept', 'revert'],
  conflict: ['keep', 'accept', 'keep_both'],
};

const bulkActions = computed(() => {
  const list = selectedEntries.value;
  if (!list.length) return [];
  const sets = list.map((entry) => ACTION_SETS[entry.type] || []);
  return sets[0].filter((action) => sets.every((set) => set.includes(action)));
});

function bulkLabel(action) {
  if (action === 'accept') {
    if (selectedEntries.value.some((entry) => entry.type === 'conflict')) {
      return t('workspace.attention.keep_phone');
    }
    const kinds = new Set(selectedEntries.value.map((entry) => entry.type));
    if (kinds.size === 1 && kinds.has('change')) return t('workspace.attention.accept_change');
    if (kinds.size > 1) return t('decision.accept');
    const sides = new Set(selectedEntries.value.map((entry) => entry.change?.side));
    if (sides.size === 1 && sides.has('remote')) return t('workspace.attention.copy_to_pc');
    if (sides.size === 1 && sides.has('local')) return t('workspace.attention.copy_to_phone');
    return t('workspace.attention.copy_to_phone');
  }
  if (action === 'revert') return t('workspace.attention.reject_change');
  if (action === 'only_here') {
    const kinds = new Set(selectedEntries.value.map((entry) => entry.type));
    if (kinds.size > 1) return t('workspace.attention.only_one_side');
    const sides = new Set(selectedEntries.value.map((entry) => entry.change?.side));
    if (sides.size === 1) {
      // Для удаления «только здесь» — сторона, где файл остался, то есть
      // противоположная месту удаления.
      const addition = kinds.has('addition');
      const remote = sides.has('remote');
      const keepsPhone = addition ? remote : !remote;
      return keepsPhone
        ? t('workspace.attention.keep_phone_only')
        : t('workspace.attention.keep_pc_only');
    }
    return t('workspace.attention.only_one_side');
  }
  if (action === 'keep_in_library') {
    const sides = new Set(selectedEntries.value.map((entry) => entry.change?.side));
    if (sides.size === 1) {
      return sides.has('local')
        ? t('workspace.attention.restore_to_pc')
        : t('workspace.attention.restore_to_phone');
    }
    return t('decision.keep_in_library');
  }
  if (action === 'keep') return t('workspace.attention.keep_pc');
  if (action === 'keep_both') return t('workspace.attention.keep_both');
  if (action === 'delete_everywhere') {
    const kinds = new Set(selectedEntries.value.map((entry) => entry.type));
    if (kinds.size > 1) return t('workspace.attention.delete_files');
    const sides = new Set(selectedEntries.value.map((entry) => entry.change?.side));
    if (sides.size === 1) {
      const addition = kinds.has('addition');
      const remote = sides.has('remote');
      // Добавление удаляется там, где появилось; удаление — там, где файл
      // ещё лежит, то есть на противоположной стороне.
      const deletesPhone = addition ? remote : !remote;
      return deletesPhone
        ? t('workspace.attention.delete_from_phone')
        : t('workspace.attention.delete_from_pc');
    }
    return t('workspace.attention.delete_files');
  }
  return action;
}

async function applyBulk(action) {
  if (state.workspace.action || state.workspace.refreshing) return;
  const paths = selectedEntries.value.map((entry) => entry.path);
  if (!paths.length) return;
  if (action === 'delete_everywhere') {
    askBulkDelete(paths, bulkLabel(action));
    return;
  }
  if (await decideMany(paths, action)) selected.value = [];
}

// ---------------------------------------------------------------- папки

const expanded = ref(new Set());
const expandedAll = computed(
  () => folderGroups.value.length > 0
    && folderGroups.value.every((folder) => expanded.value.has(folder.folder)),
);

function isExpanded(folder) {
  return expanded.value.has(folder.folder);
}

function toggleExpanded(folder) {
  const next = new Set(expanded.value);
  if (next.has(folder.folder)) next.delete(folder.folder);
  else next.add(folder.folder);
  expanded.value = next;
}

function toggleExpandAll() {
  if (sort.value === 'type') {
    collapsedTypes.value = typesExpandedAll.value
      ? new Set(typeGroups.value.map((group) => group.type))
      : new Set();
    return;
  }
  expanded.value = expandedAll.value
    ? new Set()
    : new Set(folderGroups.value.map((folder) => folder.folder));
}

// ---------------------------------------------------------------- типы

const collapsedTypes = ref(new Set());
const typesExpandedAll = computed(
  () => typeGroups.value.length > 0
    && typeGroups.value.every((group) => !collapsedTypes.value.has(group.type)),
);
const groupsExpandedAll = computed(
  () => (sort.value === 'type' ? typesExpandedAll.value : expandedAll.value),
);

function isTypeOpen(type) {
  return !collapsedTypes.value.has(type);
}

function toggleType(type) {
  const next = new Set(collapsedTypes.value);
  if (next.has(type)) next.delete(type);
  else next.add(type);
  collapsedTypes.value = next;
}

function folderKinds(folder) {
  return folder.kinds.map((kind) => kindLabel(kind)).join(', ');
}

function typeTitle(type) {
  if (type === 'deletion') return t('workspace.changes.group_deleted');
  if (type === 'addition') return t('workspace.changes.group_added');
  return t('workspace.changes.group_changed');
}
</script>

<template>
  <div class="changes-tab">
    <section v-if="!plan" class="ws-card ws-empty-card">
      <span class="ws-empty-icon"><IconList /></span>
      <template v-if="!phoneReady">
        <b>{{ t(phoneLinked
          ? 'workspace.changes.offline_title'
          : 'workspace.changes.no_phone_title') }}</b>
        <p>{{ t(phoneLinked
          ? 'workspace.changes.offline_text'
          : 'workspace.changes.no_phone_text') }}</p>
        <button
          v-if="phoneLinked"
          class="btn btn--small"
          :disabled="Boolean(state.busy)"
          @click="reconnect()"
        >
          <Spinner v-if="state.busy === 'reconnect'" />{{ t('workspace.reconnect') }}
        </button>
        <button v-else class="btn btn--small" @click="openDevices">
          {{ t('workspace.connect') }}
        </button>
      </template>
      <template v-else>
        <b>{{ t('workspace.changes.empty_title') }}</b>
        <p>{{ t('workspace.changes.empty_text') }}</p>
        <button class="btn btn--small" @click="refreshWorkspace()">{{ t('workspace.check') }}</button>
      </template>
    </section>

    <section v-else-if="!hasAny" class="ws-card ws-empty-card">
      <span class="ws-empty-icon is-calm"><IconCheck /></span>
      <b>{{ t('workspace.changes.empty_title') }}</b>
      <p>{{ aligned > 0
        ? t('workspace.changes.aligned_text', { count: aligned })
        : t('workspace.changes.empty_text') }}</p>
      <button class="btn btn--small" @click="refreshWorkspace()">{{ t('workspace.check') }}</button>
    </section>

    <div v-else class="ws-groups" :class="{ 'has-selection': selectionActive }">
      <p v-if="aligned > 0" class="ws-aligned-note">
        {{ t('workspace.changes.aligned_note', { count: aligned }) }}
      </p>

      <div class="ws-sides">
        <button
          v-for="item in sides"
          :key="item.id"
          type="button"
          class="ws-side"
          :class="{ 'is-active': side === item.id }"
          @click="setSide(item.id)"
        >
          <span class="ws-side-top">
            <span class="ws-side-name">{{ t(item.label) }}</span>
            <span class="ws-side-count">{{ number(counts[item.id]) }}</span>
          </span>
          <span class="ws-side-sub">
            {{ pendingCounts[item.id]
              ? t('workspace.changes.pending_short', { count: pendingCounts[item.id] })
              : t('workspace.changes.no_pending') }}
          </span>
        </button>
      </div>

      <div v-if="side !== 'conflicts' && activeEntries.length" class="ws-sort-row">
        <div class="ws-sort-tabs">
          <button
            v-for="item in sorts"
            :key="item.id"
            type="button"
            class="ws-sort"
            :class="{ 'is-active': sort === item.id }"
            @click="setSort(item.id)"
          >{{ t(item.label) }}</button>
        </div>
        <div class="ws-sort-tools">
          <button
            v-if="sort !== 'alpha' && (sort === 'folders' ? folderGroups.length : typeGroups.length)"
            class="ws-link"
            @click="toggleExpandAll"
          >{{ groupsExpandedAll ? t('workspace.changes.collapse_all') : t('workspace.changes.expand_all') }}</button>
          <button
            v-if="activePending"
            class="ws-link"
            @click="selectAll"
          >{{ t('workspace.changes.select_all') }}</button>
        </div>
      </div>

      <p v-if="!activeEntries.length" class="ws-side-empty">
        {{ t('workspace.changes.side_empty') }}
      </p>

      <section v-else-if="side === 'conflicts'" class="ws-card ws-group">
        <div class="ws-rows">
          <ChangeRow
            v-for="entry in alphaEntries"
            :key="entry.path"
            :entry="entry"
            :open="openSet.has(entry.path)"
            :selection="selectionActive"
            :selected="selectedSet.has(entry.path)"
            :selectable="entry.pending"
            @toggle="toggleEntry(entry)"
            @manual="toggleManualOpen(entry.path)"
          />
        </div>
      </section>

      <section v-else-if="sort === 'folders'" class="ws-card ws-folders">
        <div
          v-for="folder in folderGroups"
          :key="folder.folder"
          class="ws-folder"
          :class="{ 'is-open': isExpanded(folder) }"
        >
          <div class="ws-folder-head">
            <button
              v-if="folderSelection(folder).total"
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
            <span v-else class="ws-check-space" aria-hidden="true"></span>
            <button type="button" class="ws-folder-toggle" @click="toggleExpanded(folder)">
              <span class="ws-folder-arrow" aria-hidden="true">›</span>
              <span class="ws-folder-name" :title="folder.folder || ''">
                {{ folder.folder || t('workspace.changes.root') }}
              </span>
              <span class="ws-folder-meta">
                {{ t('workspace.changes.folder_meta', {
                  count: number(folder.items.length),
                  kinds: folderKinds(folder),
                }) }}
              </span>
              <span class="ws-folder-count">{{ number(folder.items.length) }}</span>
            </button>
          </div>
          <div v-if="isExpanded(folder)" class="ws-rows ws-folder-rows">
            <ChangeRow
              v-for="entry in folder.items"
              :key="entry.path"
              :entry="entry"
              :open="openSet.has(entry.path)"
              :selection="selectionActive"
              :selected="selectedSet.has(entry.path)"
              :selectable="entry.pending"
              @toggle="toggleEntry(entry)"
            @manual="toggleManualOpen(entry.path)"
            />
          </div>
        </div>
      </section>

      <section v-else-if="sort === 'alpha'" class="ws-card ws-group">
        <div class="ws-rows">
          <ChangeRow
            v-for="entry in alphaEntries"
            :key="entry.path"
            :entry="entry"
            :open="openSet.has(entry.path)"
            :show-folder="true"
            :selection="selectionActive"
            :selected="selectedSet.has(entry.path)"
            :selectable="entry.pending"
            @toggle="toggleEntry(entry)"
            @manual="toggleManualOpen(entry.path)"
          />
        </div>
      </section>

      <template v-else>
        <section
          v-for="group in typeGroups"
          :key="group.type"
          class="ws-card ws-group ws-type"
          :class="{ 'is-open': isTypeOpen(group.type) }"
        >
          <div class="ws-type-head">
            <button
              v-if="typeSelection(group).total"
              type="button"
              class="ws-check"
              :class="{
                'is-on': typeSelection(group).checked,
                'is-partial': typeSelection(group).partial,
              }"
              role="checkbox"
              :aria-checked="typeSelection(group).checked ? 'true' : 'false'"
              :aria-label="typeTitle(group.type)"
              @click="toggleTypeSelection(group)"
            ></button>
            <span v-else class="ws-check-space" aria-hidden="true"></span>
            <button type="button" class="ws-group-toggle" @click="toggleType(group.type)">
              <span class="ws-folder-arrow" aria-hidden="true">›</span>
              <span class="ws-card-title">{{ typeTitle(group.type) }}</span>
              <span class="ws-tag">{{ number(group.items.length) }}</span>
            </button>
          </div>
          <div v-if="isTypeOpen(group.type)" class="ws-rows">
            <ChangeRow
              v-for="entry in group.items"
              :key="entry.path"
              :entry="entry"
              :open="openSet.has(entry.path)"
              :show-folder="true"
              :selection="selectionActive"
              :selected="selectedSet.has(entry.path)"
              :selectable="entry.pending"
              @toggle="toggleEntry(entry)"
            @manual="toggleManualOpen(entry.path)"
            />
          </div>
        </section>
      </template>
    </div>
    <Teleport to="body">
      <div v-if="selectedEntries.length" class="ws-bulk">
        <div class="ws-bulk-info">
          <span class="ws-bulk-count">{{ t('workspace.selection.count', { count: selectedEntries.length }) }}</span>
          <button type="button" class="ws-bulk-clear" @click="clearSelection">
            {{ t('workspace.selection.clear') }}
          </button>
        </div>
        <div class="ws-bulk-actions">
          <span v-if="!bulkActions.length" class="ws-bulk-hint">
            {{ t('workspace.selection.mixed') }}
          </span>
          <button
            v-for="action in bulkActions"
            :key="action"
            type="button"
            class="btn btn--small"
            :class="{ 'btn--danger': action === 'delete_everywhere' }"
            :disabled="Boolean(state.workspace.action || state.workspace.refreshing)"
            @click="applyBulk(action)"
          >
            <Spinner v-if="state.workspace.action === `decide-many:${action}`" />
            {{ bulkLabel(action) }}
          </button>
        </div>
      </div>
    </Teleport>
  </div>
</template>
