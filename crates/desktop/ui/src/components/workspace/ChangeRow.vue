<script setup>
import DecisionButtons from './DecisionButtons.vue';
import RejectButton from './RejectButton.vue';
import UndoButton from './UndoButton.vue';
import { computed } from 'vue';
import { t } from '../../i18n/index.js';
import { basename } from '../../format.js';
import { changeMeta, decisionLabel, kindLabel } from '../../workspace/model.js';

const props = defineProps({
  entry: { type: Object, required: true },
  manual: { type: Boolean, default: false },
  showFolder: { type: Boolean, default: false },
  selection: { type: Boolean, default: false },
  selected: { type: Boolean, default: false },
  selectable: { type: Boolean, default: false },
});

const emit = defineEmits(['toggle']);

const kind = computed(() => props.entry.change?.kind || 'conflict');

// Переименование и перенос показываем именем файла: папка уже видна в
// заголовке группы или в мете, а человеку важен именно новый файл.
const title = computed(() => {
  if (kind.value === 'renamed' || kind.value === 'moved' || props.showFolder) {
    return basename(props.entry.path);
  }
  return props.entry.path;
});

const item = computed(() => ({
  type: props.entry.type === 'conflict'
    ? 'conflict'
    : props.entry.type === 'addition' ? 'addition' : 'deletion',
  path: props.entry.path,
  change: props.entry.change,
  conflict: props.entry.conflict,
  size: props.entry.size,
}));

const meta = computed(() => {
  if (props.entry.type === 'conflict') {
    return props.entry.pending
      ? t('workspace.changes.undecided')
      : decisionLabel(props.entry.conflict?.decision);
  }
  return changeMeta(props.entry.change, props.manual, props.showFolder);
});

// Клик по строке включает/выключает её в выделении. Кнопки решений и чекбокс
// живут своей жизнью и не должны переключать строку.
function onClick(event) {
  if (!props.selectable) return;
  if (event.target.closest('button, a, input, label')) return;
  emit('toggle');
}
</script>

<template>
  <div
    class="ws-row"
    :class="{
      'is-selected': selected,
      'is-selectable': selectable,
    }"
    @click="onClick"
  >
    <template v-if="selection">
      <button
        v-if="selectable"
        type="button"
        class="ws-check"
        :class="{ 'is-on': selected }"
        role="checkbox"
        :aria-checked="selected ? 'true' : 'false'"
        :aria-label="entry.path"
        @click.stop="$emit('toggle')"
      ></button>
      <span v-else class="ws-check-space" aria-hidden="true"></span>
    </template>

    <span class="ws-row-kind" :class="`is-${kind}`">{{ kindLabel(kind) }}</span>
    <div class="ws-row-main">
      <div class="ws-row-path" :title="entry.path">{{ title }}</div>
      <div class="ws-row-meta">{{ meta }}</div>
    </div>
    <div v-if="entry.pending" class="ws-row-actions">
      <DecisionButtons :item="item" />
    </div>
    <div v-else-if="entry.decided" class="ws-row-actions">
      <UndoButton :path="entry.path" />
    </div>
    <div v-else-if="entry.type === 'change'" class="ws-row-actions">
      <RejectButton :path="entry.path" />
    </div>
  </div>
</template>
