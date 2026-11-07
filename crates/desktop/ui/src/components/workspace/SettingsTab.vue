<script setup>
import LanguageSelect from '../LanguageSelect.vue';
import Spinner from '../Spinner.vue';
import { computed } from 'vue';
import { setLang, t } from '../../i18n/index.js';
import { chooseFolder, setToastSeconds, state } from '../../store.js';
import {
  askClearHistory,
  toggleManual,
  toggleManualChanges,
} from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
});

const toastChoices = [3, 5, 8, 12, 20];

const manual = computed(() => Boolean(props.data?.manual_additions));
const manualChanges = computed(() => Boolean(props.data?.manual_changes));
const busy = computed(() => Boolean(state.workspace.action || state.workspace.refreshing));
const clearing = computed(() => state.workspace.action === 'clear-history');
const folder = computed(() => props.data?.pc?.root || state.root || '');
const phone = computed(() => props.data?.phone || state.phone || {});
const connected = computed(() => Boolean(phone.value.connected));
</script>

<template>
  <div class="ws-settings">
    <section class="ws-card ws-setting-card">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.settings.language') }}</h2>
      </div>
      <p class="ws-setting-hint">{{ t('workspace.settings.language_text') }}</p>
      <LanguageSelect class="ws-lang-select" @pick="setLang" />
    </section>

    <section class="ws-card ws-setting-card">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.settings.library') }}</h2>
      </div>
      <p class="ws-setting-hint">{{ t('workspace.settings.folder_text') }}</p>
      <div class="ws-setting-row is-static">
        <span class="ws-setting-copy">
          <span class="ws-setting-title">{{ t('workspace.settings.folder') }}</span>
          <span class="ws-setting-hint" :title="folder">
            {{ folder || t('workspace.no_folder') }}
          </span>
        </span>
        <button
          type="button"
          class="btn btn--quiet btn--small"
          :disabled="Boolean(state.busy) || busy"
          @click="chooseFolder"
        >
          <Spinner v-if="state.busy === 'folder'" />
          {{ t('workspace.settings.change_folder') }}
        </button>
      </div>
    </section>

    <section class="ws-card ws-setting-card">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.settings.sync') }}</h2>
      </div>
      <button
        type="button"
        class="ws-setting-row"
        role="switch"
        :aria-checked="manual ? 'true' : 'false'"
        :disabled="busy"
        @click="toggleManual"
      >
        <span class="ws-setting-copy">
          <span class="ws-setting-title">{{ t('workspace.manual.label') }}</span>
          <span class="ws-setting-hint">{{ t('workspace.manual.hint') }}</span>
        </span>
        <span class="ws-switch" :class="{ 'is-on': manual }" aria-hidden="true">
          <span class="ws-switch-dot"></span>
        </span>
      </button>
      <button
        type="button"
        class="ws-setting-row"
        role="switch"
        :aria-checked="manualChanges ? 'true' : 'false'"
        :disabled="busy"
        @click="toggleManualChanges"
      >
        <span class="ws-setting-copy">
          <span class="ws-setting-title">{{ t('workspace.manual_changes.label') }}</span>
          <span class="ws-setting-hint">{{ t('workspace.manual_changes.hint') }}</span>
        </span>
        <span class="ws-switch" :class="{ 'is-on': manualChanges }" aria-hidden="true">
          <span class="ws-switch-dot"></span>
        </span>
      </button>
    </section>

    <section class="ws-card ws-setting-card">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.settings.toasts_title') }}</h2>
      </div>
      <p class="ws-setting-hint">{{ t('workspace.settings.toasts_text') }}</p>
      <div class="ws-segmented" role="group" :aria-label="t('workspace.settings.toasts_title')">
        <button
          v-for="value in toastChoices"
          :key="value"
          type="button"
          class="ws-segment"
          :class="{ 'is-on': value === state.toastSeconds }"
          :aria-pressed="value === state.toastSeconds ? 'true' : 'false'"
          @click="setToastSeconds(value)"
        >
          {{ t('workspace.settings.toasts_seconds', { count: value }) }}
        </button>
      </div>
    </section>

    <section class="ws-card ws-setting-card">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.settings.history_title') }}</h2>
      </div>
      <p class="ws-setting-hint">{{ t('workspace.settings.history_text') }}</p>
      <p class="ws-setting-note">{{ t('workspace.settings.history_note') }}</p>
      <p v-if="!connected" class="ws-setting-note">
        {{ t('workspace.settings.history_offline') }}
      </p>
      <div class="ws-card-actions">
        <button
          type="button"
          class="btn btn--quiet btn--danger btn--small"
          :disabled="busy || !connected"
          @click="askClearHistory"
        >
          <Spinner v-if="clearing" />
          {{ clearing ? t('workspace.settings.clearing') : t('workspace.settings.clear_history') }}
        </button>
      </div>
    </section>
  </div>
</template>
