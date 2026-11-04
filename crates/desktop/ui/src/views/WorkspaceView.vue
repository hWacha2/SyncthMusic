<script setup>
import ChangesTab from '../components/workspace/ChangesTab.vue';
import OverviewTab from '../components/workspace/OverviewTab.vue';
import SettingsTab from '../components/workspace/SettingsTab.vue';
import VaultTab from '../components/workspace/VaultTab.vue';
import WorkspaceEmpty from '../components/workspace/WorkspaceEmpty.vue';
import WorkspaceHead from '../components/workspace/WorkspaceHead.vue';
import WorkspaceNotices from '../components/workspace/WorkspaceNotices.vue';
import WorkspaceRail from '../components/workspace/WorkspaceRail.vue';
import { computed, onMounted } from 'vue';
import { state } from '../store.js';
import { refreshWorkspace } from '../store.js';

const data = computed(() => state.workspace.data);
const tab = computed(() => state.workspace.tab);

onMounted(() => {
  // Вкладка открывается уже с данными после boot(), но после «Проверить»
  // или возврата из устройств состояние могло быть сброшено.
  if (!data.value && !state.workspace.refreshing) refreshWorkspace();
});
</script>

<template>
  <section class="workspace">
    <WorkspaceRail :data="data" />
    <div class="workspace-main">
      <WorkspaceHead :data="data" :tab="tab" />
      <WorkspaceNotices :data="data" />
      <div class="workspace-body">
        <WorkspaceEmpty v-if="!data" />
        <OverviewTab v-else-if="tab === 'overview'" :data="data" />
        <ChangesTab v-else-if="tab === 'changes'" :data="data" />
        <VaultTab v-else-if="tab === 'vault'" :data="data" />
        <SettingsTab v-else-if="tab === 'settings'" :data="data" />
      </div>
    </div>
  </section>
</template>
