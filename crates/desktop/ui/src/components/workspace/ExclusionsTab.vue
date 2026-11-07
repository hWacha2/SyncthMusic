<script setup>
import IconExclude from '../icons/IconExclude.vue';
import SortTabs from './SortTabs.vue';
import Spinner from '../Spinner.vue';
import { computed, ref, watch } from 'vue';
import { t } from '../../i18n/index.js';
import { ago, formatSize } from '../../format.js';
import { state } from '../../store.js';
import { sortByFolder, sortByTime, sortEntries } from '../../workspace/model.js';
import { undoExclusion } from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
});

const sort = ref('time');
const sortOptions = [
  { id: 'time', label: 'workspace.sort.time' },
  { id: 'folders', label: 'workspace.changes.sort_folders' },
  { id: 'alpha', label: 'workspace.changes.sort_alpha' },
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

// Если на ПК исключений нет, а на телефоне есть, сразу открываем телефон.
watch(rows, (list) => {
  if (picked.value || !list.length) return;
  if (!list.some((row) => row.state === 'excluded_local')) side.value = 'remote';
}, { immediate: true });

function selectSide(next) {
  picked.value = true;
  side.value = next;
}

const active = computed(
  () => groups.value.find((group) => group.side === side.value) || groups.value[0],
);

const sortedItems = computed(() => {
  if (sort.value === 'alpha') return sortEntries(active.value.items);
  if (sort.value === 'folders') return sortByFolder(active.value.items);
  return sortByTime(active.value.items, 'created_at');
});
</script>

<template>
  <div class="ws-exclusions">
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

      <section class="ws-card">
        <div class="ws-card-head">
          <h2 class="ws-card-title">{{ active.title }}</h2>
          <span class="ws-tag">
            {{ t('workspace.exclusions.count', { count: active.items.length }) }}
          </span>
        </div>
        <p v-if="!active.items.length" class="ws-empty-line">
          {{ t('workspace.exclusions.side_empty') }}
        </p>
        <template v-else>
          <div class="ws-sort-row ws-card-sort">
            <SortTabs v-model="sort" :options="sortOptions" />
          </div>
          <div class="ws-rows">
            <div v-for="row in sortedItems" :key="row.path" class="ws-row">
              <span class="ws-row-kind is-excluded">{{ formatSize(row.size) }}</span>
              <div class="ws-row-main">
                <div class="ws-row-path" :title="row.path">{{ row.path }}</div>
                <div class="ws-row-meta">
                  {{ t('workspace.exclusions.excluded', { ago: ago(row.created_at) }) }}
                </div>
              </div>
              <div class="ws-row-actions">
                <button
                  class="btn btn--quiet btn--small"
                  :disabled="busy"
                  @click="undoExclusion(row.path)"
                >
                  <Spinner v-if="state.workspace.action === `exclusion:${row.path}`" />
                  {{ t('workspace.exclusions.undo') }}
                </button>
              </div>
            </div>
          </div>
        </template>
      </section>
    </template>
  </div>
</template>
