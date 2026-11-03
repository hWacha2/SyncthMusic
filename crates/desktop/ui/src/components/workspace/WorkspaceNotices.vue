<script setup>
import { computed } from 'vue';
import { t } from '../../i18n/index.js';
import { state } from '../../store.js';

const props = defineProps({
  data: { type: Object, default: null },
});

const rows = computed(() => {
  const list = [];
  if (state.workspace.error) {
    list.push({ tone: 'bad', text: t('workspace.error.refresh', { error: state.workspace.error }) });
  }
  if (props.data?.session_error) {
    list.push({ tone: 'warn', text: t('workspace.error.session', { error: props.data.session_error }) });
  }
  if (props.data?.phone_error) {
    list.push({ tone: 'warn', text: t('workspace.error.phone', { error: props.data.phone_error }) });
  }
  if (state.workspace.statusError) {
    list.push({ tone: 'warn', text: t('workspace.error.status', { error: state.workspace.statusError }) });
  }
  if (state.workspace.note) {
    list.push({ tone: state.workspace.noteError ? 'bad' : 'ok', text: state.workspace.note });
  }
  return list;
});
</script>

<template>
  <div v-if="rows.length" class="ws-notices">
    <p
      v-for="(row, index) in rows"
      :key="index"
      class="ws-notice"
      :class="`is-${row.tone}`"
      :role="row.tone === 'ok' ? 'status' : 'alert'"
    >
      <span class="ws-notice-dot" aria-hidden="true"></span>{{ row.text }}
    </p>
  </div>
</template>
