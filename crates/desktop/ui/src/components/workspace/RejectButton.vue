<script setup>
import Spinner from '../Spinner.vue';
import { t } from '../../i18n/index.js';
import { state } from '../../store.js';
import { decide } from '../../workspace/actions.js';

const props = defineProps({
  path: { type: String, required: true },
});

const busy = () => Boolean(state.workspace.action || state.workspace.refreshing);
</script>

<template>
  <button
    class="btn btn--quiet btn--small"
    :title="t('workspace.decision.reject_hint')"
    :disabled="busy()"
    @click="decide(props.path, 'revert')"
  >
    <Spinner v-if="state.workspace.action === `decide:${props.path}:revert`" />{{ t('workspace.decision.reject') }}
  </button>
</template>
