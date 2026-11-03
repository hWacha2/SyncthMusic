<script setup>
import { t } from '../i18n/index.js';
import { windowAction } from '../bridge.js';
</script>

<template>
  <header class="titlebar" data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <span class="brand-mark" aria-hidden="true"></span>
      <span class="brand-name">SynctMusic</span>
    </div>
    <div class="window-controls">
      <button
        class="window-btn"
        :aria-label="t('window.minimize')"
        :title="t('window.minimize')"
        @click="windowAction('minimize')"
      >
        <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M2 6h8"/></svg>
      </button>
      <button
        class="window-btn"
        :aria-label="t('window.maximize')"
        :title="t('window.maximize')"
        @click="windowAction('maximize')"
      >
        <svg viewBox="0 0 12 12" aria-hidden="true"><rect x="2.5" y="2.5" width="7" height="7"/></svg>
      </button>
      <button
        class="window-btn window-btn--close"
        :aria-label="t('window.close')"
        :title="t('window.close')"
        @click="windowAction('close')"
      >
        <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M3 3l6 6M9 3L3 9"/></svg>
      </button>
    </div>
  </header>
</template>
