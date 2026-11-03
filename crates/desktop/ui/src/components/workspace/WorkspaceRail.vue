<script setup>
import IconList from '../icons/IconList.vue';
import IconOverview from '../icons/IconOverview.vue';
import IconVault from '../icons/IconVault.vue';
import { t } from '../../i18n/index.js';
import { openDevices, state } from '../../store.js';
import { basename, connectionState, pendingCount } from '../../workspace/model.js';
import { selectTab, toggleManual } from '../../workspace/actions.js';

defineProps({
  data: { type: Object, default: null },
});

const tabs = [
  { id: 'overview', icon: IconOverview, label: 'workspace.tab.overview' },
  { id: 'changes', icon: IconList, label: 'workspace.tab.changes', hot: true },
  { id: 'vault', icon: IconVault, label: 'workspace.tab.vault' },
];

function countFor(id, data) {
  if (id === 'changes') return pendingCount(data);
  if (id === 'vault') return Number(data?.pc?.vault_files) || 0;
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
      <button
        class="ws-manual"
        role="switch"
        :aria-checked="data?.manual_additions ? 'true' : 'false'"
        :title="t('workspace.manual.hint')"
        :disabled="Boolean(state.workspace.action || state.workspace.refreshing)"
        @click="toggleManual"
      >
        <span class="ws-manual-text">{{ t('workspace.manual.label') }}</span>
        <span class="ws-switch" :class="{ 'is-on': data?.manual_additions }" aria-hidden="true">
          <span class="ws-switch-dot"></span>
        </span>
      </button>
      <div class="ws-rail-state">
        <span
          class="status-dot"
          :class="{
            'is-on': connectionState(data).tone === 'on',
            'is-warn': connectionState(data).tone === 'warn',
          }"
          aria-hidden="true"
        ></span>
        <span>{{ connectionState(data).label }}</span>
      </div>
      <button class="ws-link" @click="openDevices">{{ t('workspace.devices') }}</button>
      <span v-if="state.info?.version" class="ws-version">v{{ state.info.version }} · AGPL-3.0</span>
    </div>
  </aside>
</template>
