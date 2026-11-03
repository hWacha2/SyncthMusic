<script setup>
import IconFolder from '../components/icons/IconFolder.vue';
import NoteLine from '../components/NoteLine.vue';
import Spinner from '../components/Spinner.vue';
import { t } from '../i18n/index.js';
import { chooseFolder, openDevices, state } from '../store.js';
</script>

<template>
  <section class="setup setup--center">
    <div class="library-card">
      <span class="library-icon"><IconFolder /></span>
      <h1 class="setup-title">{{ t('library.title') }}</h1>
      <p class="setup-hint">{{ t('library.hint') }}</p>
      <div class="library-actions">
        <button class="btn btn--large" :disabled="state.busy === 'folder'" @click="chooseFolder">
          <Spinner v-if="state.busy === 'folder'" />
          {{ state.busy === 'folder' ? t('library.choosing') : t('library.action') }}
        </button>
        <button v-if="state.root" class="btn btn--quiet" @click="openDevices">
          {{ t('library.back') }}
        </button>
      </div>
      <NoteLine />
    </div>
  </section>
</template>
