<script setup>
import IconList from '../icons/IconList.vue';
import IconOverview from '../icons/IconOverview.vue';
import IconSettings from '../icons/IconSettings.vue';
import IconVault from '../icons/IconVault.vue';
import IconExclude from '../icons/IconExclude.vue';
import { computed } from 'vue';
import { t } from '../../i18n/index.js';
import { state } from '../../store.js';
import { basename, connectionState, excludedCount, pendingCount, vaultTotals } from '../../workspace/model.js';
import { selectTab } from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
});

const conn = computed(() => {
  // Пока идёт проверка или сброс истории, «Нет связи» вводит в заблуждение:
  // сеанс как раз используется для сканирования.
  if (state.workspace.refreshing || state.workspace.action === 'clear-history') {
    return { tone: 'warn', label: t('workspace.status.checking') };
  }
  return connectionState(props.data);
});

const tabs = [
  { id: 'overview', icon: IconOverview, label: 'workspace.tab.overview' },
  { id: 'changes', icon: IconList, label: 'workspace.tab.changes', hot: true },
  { id: 'exclusions', icon: IconExclude, label: 'workspace.tab.exclusions' },
  { id: 'vault', icon: IconVault, label: 'workspace.tab.vault' },
  { id: 'settings', icon: IconSettings, label: 'workspace.tab.settings' },
];

function countFor(id, data) {
  // После сброса истории счётчики ещё старые — не показываем их.
  if (state.workspace.stale) return 0;
  if (id === 'changes') return pendingCount(data);
  if (id === 'exclusions') return excludedCount(data);
  if (id === 'vault') return vaultTotals(data).count;
  return 0;
}
</script>

<template>
  <aside class="ws-rail">
    <div class="ws-device-card">
      <span class="ws-kicker">{{ t('workspace.this_pc') }}</span>
      <div class="ws-device-name" :title="data?.pc?.name || state.info?.device_name || t('workspace.this_pc')">
        {{ data?.pc?.name || state.info?.device_name || t('workspace.this_pc') }}
      </div>
      <div class="ws-device-root" :title="data?.pc?.root || state.root || ''">
        {{ (data?.pc?.root || state.root) ? basename(data?.pc?.root || state.root) : t('workspace.no_folder') }}
      </div>
    </div>

    <nav class="ws-nav" :aria-label="t('workspace.nav')">
      <button
        v-for="tab in tabs"
        :key="tab.id"
        class="ws-nav-item"
        :class="{ 'is-active': state.workspace.tab === tab.id }"
        @click="selectTab(tab.id)"
      >
        <span class="ws-nav-icon"><component :is="tab.icon" /></span>
        <span class="ws-nav-label">{{ t(tab.label) }}</span>
        <span
          v-if="countFor(tab.id, data)"
          class="ws-nav-count"
          :class="{ 'is-hot': tab.hot }"
        >{{ countFor(tab.id, data) }}</span>
      </button>
    </nav>

    <div class="ws-rail-foot">
      <div class="ws-rail-state">
        <span
          class="status-dot"
          :class="{
            'is-on': conn.tone === 'on',
            'is-warn': conn.tone === 'warn',
          }"
          aria-hidden="true"
        ></span>
        <span>{{ conn.label }}</span>
      </div>
      <span v-if="state.info?.version" class="ws-version">v{{ state.info.version }} · AGPL-3.0</span>
    </div>
  </aside>
</template>
