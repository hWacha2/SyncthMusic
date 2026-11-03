<script setup>
import { nextTick, ref, watch } from 'vue';
import { t } from '../i18n/index.js';
import { closeModal, confirmModal, state } from '../store.js';

const confirmButton = ref(null);

watch(
  () => state.modal,
  async (modal) => {
    if (!modal) return;
    await nextTick();
    confirmButton.value?.focus();
  },
);
</script>

<template>
  <div class="modal-layer" :hidden="!state.modal">
    <template v-if="state.modal">
      <div class="modal-backdrop" @click="closeModal"></div>
      <div class="modal" role="dialog" aria-modal="true" aria-labelledby="modal-title">
        <span class="modal-kicker">{{ state.modal.kicker || '' }}</span>
        <h2 id="modal-title" class="modal-title">{{ state.modal.title }}</h2>
        <p class="modal-text">{{ state.modal.text || '' }}</p>
        <div v-if="state.modal.path" class="modal-path">{{ state.modal.path }}</div>
        <div class="modal-actions">
          <button class="btn btn--quiet" @click="closeModal">{{ t('workspace.cancel') }}</button>
          <button
            ref="confirmButton"
            class="btn"
            :class="{ 'btn--danger': state.modal.danger }"
            @click="confirmModal"
          >
            {{ state.modal.label || t('workspace.confirm') }}
          </button>
        </div>
      </div>
    </template>
  </div>
</template>
