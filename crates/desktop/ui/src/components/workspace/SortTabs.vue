<script setup>
import { t } from '../../i18n/index.js';

defineProps({
  modelValue: { type: String, default: 'time' },
  options: { type: Array, required: true },
});

const emit = defineEmits(['update:modelValue']);
</script>

<template>
  <div class="ws-sort-tabs">
    <button
      v-for="item in options"
      :key="item.id"
      type="button"
      class="ws-sort"
      :class="{ 'is-active': modelValue === item.id }"
      @click="emit('update:modelValue', item.id)"
    >{{ t(item.label) }}</button>
  </div>
</template>
