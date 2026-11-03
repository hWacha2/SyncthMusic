<script setup>
import { state } from '../store.js';
</script>

<template>
  <p
    v-if="state.note || state.error"
    class="setup-note"
    :class="{ 'is-error': state.error }"
    :role="state.error ? 'alert' : 'status'"
  >
    {{ state.note }}
  </p>
</template>
