<script setup>
import LanguageSelect from '../components/LanguageSelect.vue';
import { t } from '../i18n/index.js';
import { chooseLanguage } from '../store.js';
</script>

<template>
  <section class="setup setup--center">
    <div class="library-card language-card">
      <h1 class="setup-title">{{ t('app.name') }}</h1>
      <p class="setup-hint">Выберите язык · Choose your language</p>
      <LanguageSelect @pick="chooseLanguage" />
    </div>
  </section>
</template>
