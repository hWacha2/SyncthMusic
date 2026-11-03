<script setup>
defineProps({
  kind: { type: String, default: 'lan' },
  name: { type: String, default: '' },
  sub: { type: String, default: '' },
  busy: { type: Boolean, default: false },
});
</script>

<template>
  <div class="device-item" :class="{ 'is-busy': busy }">
    <span class="device-icon" :class="`device-icon--${kind}`">
      <slot name="icon" />
    </span>
    <div class="device-copy">
      <div class="device-name">{{ name }}</div>
      <div class="device-addr">{{ sub }}</div>
    </div>
    <slot />
  </div>
</template>
