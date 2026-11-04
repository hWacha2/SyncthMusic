<script setup>
import Spinner from '../Spinner.vue';
import { t } from '../../i18n/index.js';
import { state } from '../../store.js';
import { askDelete, decide } from '../../workspace/actions.js';

const props = defineProps({
  item: { type: Object, required: true },
});

const busy = () => Boolean(state.workspace.action || state.workspace.refreshing);
const decisionBusy = (action) => state.workspace.action === `decide:${props.item.path}:${action}`;

function conflictReason() {
  const reason = props.item.conflict?.reason;
  if (reason === 'changed_vs_deleted') return t('workspace.conflict.changed_deleted');
  if (reason === 'no_base') return t('workspace.conflict.no_base');
  return t('workspace.conflict.both_changed');
}
</script>

<template>
  <template v-if="item.type === 'addition'">
    <button
      class="btn btn--quiet btn--small"
      :disabled="busy()"
      @click="decide(item.path, 'accept')"
    >
      <Spinner v-if="decisionBusy('accept')" />{{ t('workspace.attention.transfer') }}
    </button>
    <button
      class="btn btn--quiet btn--small"
      :disabled="busy()"
      @click="decide(item.path, 'only_here')"
    >
      <Spinner v-if="decisionBusy('only_here')" />{{ item.change?.side === 'remote'
        ? t('workspace.attention.keep_phone_only')
        : t('workspace.attention.keep_pc_only') }}
    </button>
    <button
      class="btn btn--danger btn--small"
      :disabled="busy()"
      @click="askDelete(item.path, 'addition')"
    >
      <Spinner v-if="decisionBusy('delete_everywhere')" />{{ t('workspace.attention.delete_everywhere') }}
    </button>
  </template>

  <template v-else-if="item.type === 'deletion'">
    <button
      class="btn btn--quiet btn--small"
      :disabled="busy()"
      @click="decide(item.path, 'keep_in_library')"
    >
      <Spinner v-if="decisionBusy('keep_in_library')" />{{ item.change?.side === 'local'
        ? t('workspace.attention.restore_from_phone')
        : t('workspace.attention.keep_pc') }}
    </button>
    <button
      class="btn btn--danger btn--small"
      :disabled="busy()"
      @click="askDelete(item.path, 'deletion')"
    >
      <Spinner v-if="decisionBusy('delete_everywhere')" />{{ t('workspace.attention.delete_everywhere') }}
    </button>
  </template>

  <template v-else>
    <span class="ws-decision-reason">{{ conflictReason() }}</span>
    <button
      class="btn btn--quiet btn--small"
      :disabled="busy()"
      @click="decide(item.path, 'keep')"
    >
      <Spinner v-if="decisionBusy('keep')" />{{ t('workspace.attention.keep_pc') }}
    </button>
    <button
      class="btn btn--quiet btn--small"
      :disabled="busy()"
      @click="decide(item.path, 'accept')"
    >
      <Spinner v-if="decisionBusy('accept')" />{{ t('workspace.attention.keep_phone') }}
    </button>
    <button
      class="btn btn--quiet btn--small"
      :disabled="busy()"
      @click="decide(item.path, 'keep_both')"
    >
      <Spinner v-if="decisionBusy('keep_both')" />{{ t('workspace.attention.keep_both') }}
    </button>
  </template>
</template>
