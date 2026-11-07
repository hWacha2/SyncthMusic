<script setup>
import Spinner from '../Spinner.vue';
import { computed } from 'vue';
import { basename } from '../../format.js';

const props = defineProps({
  path: { type: String, required: true },
  kind: { type: String, required: true },
  kindClass: { type: String, default: '' },
  meta: { type: String, default: '' },
  actionLabel: { type: String, required: true },
  actionBusy: { type: Boolean, default: false },
  busy: { type: Boolean, default: false },
  selection: { type: Boolean, default: false },
  selected: { type: Boolean, default: false },
  // В группе по папкам заголовок уже показывает папку, поэтому оставляем
  // только имя файла. В плоском списке виден полный путь.
  showFolder: { type: Boolean, default: false },
});

const emit = defineEmits(['toggle', 'action']);

const title = computed(() => (props.showFolder ? basename(props.path) : props.path));

// Клик по строке включает/выключает её в выделении. Кнопка действия живёт
// своей жизнью и не должна переключать строку.
function onClick(event) {
  if (event.target.closest('button, a, input, label')) return;
  emit('toggle');
}
</script>

<template>
  <div
    class="ws-row is-selectable"
    :class="{ 'is-selected': selected }"
    @click="onClick"
  >
    <button
      v-if="selection"
      type="button"
      class="ws-check"
      :class="{ 'is-on': selected }"
      role="checkbox"
      :aria-checked="selected ? 'true' : 'false'"
      :aria-label="path"
      @click.stop="$emit('toggle')"
    ></button>
    <span class="ws-row-kind" :class="kindClass">{{ kind }}</span>
    <div class="ws-row-main">
      <div class="ws-row-path" :title="path">{{ title }}</div>
      <div class="ws-row-meta">{{ meta }}</div>
    </div>
    <div class="ws-row-actions">
      <button
        type="button"
        class="btn btn--quiet btn--small"
        :disabled="busy"
        @click.stop="$emit('action')"
      >
        <Spinner v-if="actionBusy" />{{ actionLabel }}
      </button>
    </div>
  </div>
</template>
