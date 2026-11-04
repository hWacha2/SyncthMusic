<script setup>
import Spinner from '../Spinner.vue';
import { computed } from 'vue';
import { t } from '../../i18n/index.js';
import { state } from '../../store.js';
import { askClearHistory, toggleManual } from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
});

const manual = computed(() => Boolean(props.data?.manual_additions));
const busy = computed(() => Boolean(state.workspace.action || state.workspace.refreshing));
const clearing = computed(() => state.workspace.action === 'clear-history');
</script>

<template>
  <div class="ws-settings">
    <section class="ws-card ws-setting-card">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.settings.sync') }}</h2>
        <span class="ws-tag" :class="{ 'is-on': manual }">
          {{ manual ? t('workspace.settings.manual_on') : t('workspace.settings.manual_off') }}
        </span>
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
    </section>

    <section class="ws-card ws-setting-card">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.settings.history_title') }}</h2>
        <span class="ws-tag is-warn">{{ t('workspace.settings.history_tag') }}</span>
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
