<script setup>
import Spinner from '../Spinner.vue';
import { computed } from 'vue';
import { LANGUAGES, lang, setLang, t } from '../../i18n/index.js';
import {
  chooseFolder,
  disconnectPhone,
  openDevices,
  reconnect,
  setToastSeconds,
  state,
} from '../../store.js';
import {
  askClearHistory,
  askForget,
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
const linked = computed(() => Boolean(phone.value.linked));
const connected = computed(() => Boolean(phone.value.connected));
</script>

<template>
  <div class="ws-settings">
    <section class="ws-card ws-setting-card">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.settings.library') }}</h2>
      </div>
      <p class="ws-setting-hint">{{ t('workspace.settings.language_text') }}</p>
      <div class="ws-segmented" role="group" :aria-label="t('workspace.settings.language')">
        <button
          v-for="item in LANGUAGES"
          :key="item.code"
          type="button"
          class="ws-segment"
          :class="{ 'is-on': item.code === lang }"
          :aria-pressed="item.code === lang ? 'true' : 'false'"
          @click="setLang(item.code)"
        >
          {{ item.label }}
        </button>
      </div>
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
        <h2 class="ws-card-title">{{ t('workspace.settings.device') }}</h2>
        <span class="ws-tag" :class="{ 'is-on': connected, 'is-warn': linked && !connected }">
          {{ !linked
            ? t('workspace.settings.not_linked')
            : connected ? t('workspace.status.connected') : t('workspace.status.offline') }}
        </span>
      </div>
      <p class="ws-setting-hint">{{ t('workspace.settings.device_text') }}</p>
      <p v-if="linked" class="ws-setting-note">
        {{ phone.name || phone.dev }}<span v-if="phone.addr"> · {{ phone.addr }}</span>
        <span v-if="phone.root"> · {{ phone.root }}</span>
      </p>
      <div class="ws-card-actions">
        <button
          v-if="!linked"
          type="button"
          class="btn btn--small"
          @click="openDevices"
        >
          {{ t('workspace.connect') }}
        </button>
        <button
          v-else-if="!connected"
          type="button"
          class="btn btn--small"
          :disabled="Boolean(state.busy)"
          @click="reconnect()"
        >
          <Spinner v-if="state.busy === 'reconnect'" />
          {{ t('workspace.reconnect') }}
        </button>
        <button
          v-if="linked && connected"
          type="button"
          class="btn btn--quiet btn--small"
          :disabled="Boolean(state.phoneBusy)"
          @click="disconnectPhone"
        >
          <Spinner v-if="state.phoneBusy === 'disconnect'" />
          {{ t('workspace.disconnect') }}
        </button>
        <button
          v-if="linked"
          type="button"
          class="btn btn--quiet btn--small btn--danger"
          :disabled="Boolean(state.phoneBusy)"
          @click="askForget"
        >
          <Spinner v-if="state.phoneBusy === 'forget'" />
          {{ t('workspace.forget') }}
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
      <div class="ws-card-actions">
        <button
          type="button"
          class="btn btn--quiet btn--danger btn--small"
          :disabled="busy"
          @click="askClearHistory"
        >
          <Spinner v-if="clearing" />
          {{ clearing ? t('workspace.settings.clearing') : t('workspace.settings.clear_history') }}
        </button>
      </div>
    </section>
  </div>
</template>
