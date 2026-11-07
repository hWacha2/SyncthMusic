<script setup>
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { LANGUAGES, lang, t } from '../i18n/index.js';
import { chooseLanguage } from '../store.js';

const open = ref(false);
const root = ref(null);
const current = computed(
  () => LANGUAGES.find((item) => item.code === lang.value) || LANGUAGES[0],
);

function pick(code) {
  open.value = false;
  chooseLanguage(code);
}

function onPointerDown(event) {
  if (root.value && !root.value.contains(event.target)) open.value = false;
}

function onKeyDown(event) {
  if (event.key === 'Escape') open.value = false;
}

onMounted(() => {
  document.addEventListener('pointerdown', onPointerDown);
  document.addEventListener('keydown', onKeyDown);
});

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', onPointerDown);
  document.removeEventListener('keydown', onKeyDown);
});
</script>

<template>
  <section class="setup setup--center">
    <div class="library-card language-card">
      <h1 class="setup-title">{{ t('app.name') }}</h1>
      <p class="setup-hint">Выберите язык · Choose your language</p>

      <div ref="root" class="lang-select" :class="{ 'is-open': open }">
        <button
          type="button"
          class="lang-trigger"
          aria-haspopup="listbox"
          :aria-expanded="open"
          @click="open = !open"
        >
          <span>{{ current.label }}</span>
          <svg class="lang-chevron" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M4 6l4 4 4-4" />
          </svg>
        </button>

        <div v-if="open" class="lang-menu" role="listbox">
          <button
            v-for="item in LANGUAGES"
            :key="item.code"
            type="button"
            role="option"
            :aria-selected="item.code === lang"
            class="lang-option"
            :class="{ 'is-current': item.code === lang }"
            @click="pick(item.code)"
          >
            <span>{{ item.label }}</span>
            <span class="lang-code">{{ item.short }}</span>
          </button>
        </div>
      </div>
    </div>
  </section>
</template>
