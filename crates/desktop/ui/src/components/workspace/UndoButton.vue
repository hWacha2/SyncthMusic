<script setup>
import Spinner from '../Spinner.vue';
import { t } from '../../i18n/index.js';
import { state } from '../../store.js';
import { undecide } from '../../workspace/actions.js';

const props = defineProps({
  path: { type: String, required: true },
});

const busy = () => Boolean(state.workspace.action || state.workspace.refreshing);
</script>

<template>
  <button
    class="btn btn--quiet btn--small"
    :title="t('workspace.decision.undo_hint')"
    :disabled="busy()"
    @click="undecide(props.path)"
  >
    <Spinner v-if="state.workspace.action === `undecide:${props.path}`" />{{ t('workspace.decision.undo') }}
  </button>
</template>
