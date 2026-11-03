<script setup>
import { computed, nextTick, onMounted, ref, watch } from 'vue';
import ModalLayer from './components/ModalLayer.vue';
import TitleBar from './components/TitleBar.vue';
import ErrorView from './views/ErrorView.vue';
import LanguageView from './views/LanguageView.vue';
import LibraryView from './views/LibraryView.vue';
import DevicesView from './views/DevicesView.vue';
import WorkspaceView from './views/WorkspaceView.vue';
import { boot, state } from './store.js';

const views = {
  language: LanguageView,
  library: LibraryView,
  devices: DevicesView,
  workspace: WorkspaceView,
  error: ErrorView,
};

const current = computed(() => views[state.view] || null);
const screen = ref(null);
const enter = ref(false);

// Анимация входа — только при настоящей смене экрана. Перерисовка тех же
// данных (опрос статуса) не должна дёргать её и сбрасывать прокрутку.
watch(
  () => state.view,
  async () => {
    enter.value = false;
    await nextTick();
    if (screen.value) screen.value.scrollTop = 0;
    requestAnimationFrame(() => {
      enter.value = true;
    });
  },
);

onMounted(boot);
</script>

<template>
  <div class="app">
    <TitleBar />
    <main ref="screen" class="screen" :class="{ 'view-enter': enter }">
      <component :is="current" v-if="current" />
    </main>
    <ModalLayer />
  </div>
</template>
