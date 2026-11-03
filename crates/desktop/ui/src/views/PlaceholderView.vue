<script setup>
import { computed } from 'vue';
import { t } from '../i18n/index.js';
import { state } from '../store.js';

// Временный экран для разделов, которые ещё не перенесены. Каждый такой
// раздел заменяется отдельным компонентом в следующем куске миграции.
const title = computed(() => {
  if (state.view === 'library') return t('library.title');
  if (state.view === 'devices') return t('setup.title');
  if (state.view === 'workspace') return t('workspace.label');
  return 'SynctMusic';
});
</script>

<template>
  <section class="setup setup--center">
    <div class="library-card">
      <h1 class="setup-title">{{ title }}</h1>
      <p class="setup-hint">
        Раздел ещё не перенесён в Vue.<br>
        This section is not ported to Vue yet.
      </p>
    </div>
  </section>
</template>
