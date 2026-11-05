<script setup>
import Spinner from '../Spinner.vue';
import { computed } from 'vue';
import { t } from '../../i18n/index.js';
import { state } from '../../store.js';
import { askDelete, decide } from '../../workspace/actions.js';

const props = defineProps({
  item: { type: Object, required: true },
});

const busy = () => Boolean(state.workspace.action || state.workspace.refreshing);
const decisionBusy = (action) => state.workspace.action === `decide:${props.item.path}:${action}`;

// Сторона изменения: `remote` — телефон, `local` — этот ПК. Подписи называют
// не внутреннее решение, а то, где файл окажется или откуда исчезнет.
const side = computed(() => props.item.change?.side);

const additionAccept = computed(() => (
  side.value === 'remote' ? t('workspace.attention.copy_to_pc') : t('workspace.attention.copy_to_phone')
));
const additionOnly = computed(() => (
  side.value === 'remote' ? t('workspace.attention.keep_phone_only') : t('workspace.attention.keep_pc_only')
));
const additionDelete = computed(() => (
  side.value === 'remote' ? t('workspace.attention.delete_from_phone') : t('workspace.attention.delete_from_pc')
));

const deletionRestore = computed(() => (
  side.value === 'local' ? t('workspace.attention.restore_to_pc') : t('workspace.attention.restore_to_phone')
));
const deletionOnly = computed(() => (
  side.value === 'local' ? t('workspace.attention.keep_phone_only') : t('workspace.attention.keep_pc_only')
));
const deletionDelete = computed(() => (
  side.value === 'local' ? t('workspace.attention.delete_from_phone') : t('workspace.attention.delete_from_pc')
));

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
      <Spinner v-if="decisionBusy('accept')" />{{ additionAccept }}
    </button>
    <button
      class="btn btn--quiet btn--small"
      :disabled="busy()"
      @click="decide(item.path, 'only_here')"
    >
      <Spinner v-if="decisionBusy('only_here')" />{{ additionOnly }}
    </button>
    <button
      class="btn btn--danger btn--small"
      :disabled="busy()"
      @click="askDelete(item.path, 'addition', additionDelete)"
    >
      <Spinner v-if="decisionBusy('delete_everywhere')" />{{ additionDelete }}
    </button>
  </template>

  <template v-else-if="item.type === 'deletion'">
    <button
      class="btn btn--quiet btn--small"
      :disabled="busy()"
      @click="decide(item.path, 'keep_in_library')"
    >
      <Spinner v-if="decisionBusy('keep_in_library')" />{{ deletionRestore }}
    </button>
    <button
      class="btn btn--quiet btn--small"
      :disabled="busy()"
      @click="decide(item.path, 'only_here')"
    >
      <Spinner v-if="decisionBusy('only_here')" />{{ deletionOnly }}
    </button>
    <button
      class="btn btn--danger btn--small"
      :disabled="busy()"
      @click="askDelete(item.path, 'deletion', deletionDelete)"
    >
      <Spinner v-if="decisionBusy('delete_everywhere')" />{{ deletionDelete }}
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
