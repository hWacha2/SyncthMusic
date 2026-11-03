<script setup>
import { chooseLanguage } from '../store.js';
</script>

<template>
  <section class="setup setup--center">
    <div class="library-card language-card">
      <h1 class="setup-title">SynctMusic</h1>
      <p class="setup-hint">Выберите язык · Choose your language</p>
      <div class="language-actions">
        <button class="btn lang-choice" @click="chooseLanguage('ru')">Русский</button>
        <button class="btn btn--quiet lang-choice" @click="chooseLanguage('en')">English</button>
      </div>
    </div>
  </section>
</template>
