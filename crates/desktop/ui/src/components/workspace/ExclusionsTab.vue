<script setup>
import IconExclude from '../icons/IconExclude.vue';
import Spinner from '../Spinner.vue';
import { computed } from 'vue';
import { t } from '../../i18n/index.js';
import { ago, formatSize } from '../../format.js';
import { state } from '../../store.js';
import { undoExclusion } from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
});

const rows = computed(() => (props.data?.states || []).filter((row) => row.excluded));
const busy = computed(() => Boolean(state.workspace.action || state.workspace.refreshing));

// `ExcludedRemote` — уцелевшая копия осталась на телефоне, `ExcludedLocal` —
// на этом ПК. Группируем по состоянию, а не по старому полю `side`.
const groups = computed(() => [
  {
    side: 'remote',
    title: t('workspace.exclusions.phone_group'),
    hint: t('workspace.exclusions.phone_hint'),
    items: rows.value.filter((row) => row.state === 'excluded_remote'),
  },
  {
    side: 'local',
    title: t('workspace.exclusions.pc_group'),
    hint: t('workspace.exclusions.pc_hint'),
    items: rows.value.filter((row) => row.state === 'excluded_local'),
  },
]);
</script>

<template>
  <div class="ws-exclusions">
    <section v-if="!rows.length" class="ws-card ws-empty-card">
      <span class="ws-empty-icon is-calm"><IconExclude /></span>
      <b>{{ t('workspace.exclusions.empty_title') }}</b>
      <p>{{ t('workspace.exclusions.empty_text') }}</p>
    </section>

    <template v-else>
      <section
        v-for="group in groups"
        v-show="group.items.length"
        :key="group.side"
        class="ws-card"
      >
        <div class="ws-card-head">
          <h2 class="ws-card-title">{{ group.title }}</h2>
          <span class="ws-tag">
            {{ t('workspace.exclusions.count', { count: group.items.length }) }}
          </span>
        </div>
        <p class="ws-setting-hint">{{ group.hint }}</p>
        <div class="ws-rows">
          <div v-for="row in group.items" :key="row.path" class="ws-row">
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
      </section>
    </template>
  </div>
</template>
