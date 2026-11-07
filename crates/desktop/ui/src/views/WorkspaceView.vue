<script setup>
import ChangesTab from '../components/workspace/ChangesTab.vue';
import DevicesTab from '../components/workspace/DevicesTab.vue';
import ExclusionsTab from '../components/workspace/ExclusionsTab.vue';
import OverviewTab from '../components/workspace/OverviewTab.vue';
import SettingsTab from '../components/workspace/SettingsTab.vue';
import VaultTab from '../components/workspace/VaultTab.vue';
import WorkspaceEmpty from '../components/workspace/WorkspaceEmpty.vue';
import WorkspaceHead from '../components/workspace/WorkspaceHead.vue';
import WorkspaceNotices from '../components/workspace/WorkspaceNotices.vue';
import WorkspaceProgress from '../components/workspace/WorkspaceProgress.vue';
import WorkspaceRail from '../components/workspace/WorkspaceRail.vue';
import { computed, onMounted, watch } from 'vue';
import { state } from '../store.js';
import { refreshWorkspace } from '../store.js';
import { loadPhoneVault } from '../workspace/actions.js';

const data = computed(() => state.workspace.data);
// Пока данные устарели после сброса, нижняя панель продолжает показывать
// связь, а содержимое вкладок ждёт свежей проверки.
const body = computed(() => (state.workspace.stale ? null : state.workspace.data));
const tab = computed(() => state.workspace.tab);

onMounted(() => {
  // Вкладка открывается уже с данными после boot(), но после «Проверить»
  // или возврата из устройств состояние могло быть сброшено.
  if (!data.value && !state.workspace.refreshing) refreshWorkspace();
});

// Корзина телефона нужна не только на своей вкладке: без неё счётчик
// «Хранилища» показывает только файлы ПК. Загружаем её один раз при входе
// в рабочую область и заново — после переподключения телефона.
watch(
  () => state.workspace.data?.phone?.connected,
  () => loadPhoneVault(),
  { immediate: true },
);
</script>

<template>
  <section class="workspace">
    <WorkspaceRail :data="data" />
    <div class="workspace-main">
      <WorkspaceHead :data="body" :tab="tab" />
      <WorkspaceNotices :data="body" />
      <WorkspaceProgress />
      <div class="workspace-body">
        <DevicesTab v-if="tab === 'devices'" :data="body" />
        <WorkspaceEmpty v-else-if="!body" />
        <OverviewTab v-else-if="tab === 'overview'" :data="body" />
        <ChangesTab v-else-if="tab === 'changes'" :data="body" />
        <ExclusionsTab v-else-if="tab === 'exclusions'" :data="body" />
        <VaultTab v-else-if="tab === 'vault'" :data="body" />
        <SettingsTab v-else-if="tab === 'settings'" :data="body" />
      </div>
    </div>
  </section>
</template>
