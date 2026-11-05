<script setup>
import IconVault from '../icons/IconVault.vue';
import Spinner from '../Spinner.vue';
import { computed } from 'vue';
import { t } from '../../i18n/index.js';
import { ago, formatSize } from '../../format.js';
import { state } from '../../store.js';
import { askClearVault, askGc, restoreVault } from '../../workspace/actions.js';
const props = defineProps({
  data: { type: Object, default: null },
});

const rows = computed(() => props.data?.vault || []);
const busy = computed(() => Boolean(state.workspace.action || state.workspace.refreshing));
const gc = computed(() => state.workspace.action === 'gc');
const clearing = computed(() => state.workspace.action === 'clear-vault');
</script>

<template>
  <div class="ws-vault">
    <section class="ws-card ws-vault-actions">
      <div class="ws-vault-action">
        <div class="ws-setting-copy">
          <span class="ws-setting-title">{{ t('workspace.vault.old_title') }}</span>
          <span class="ws-setting-hint">{{ t('workspace.vault.old_text') }}</span>
        </div>
        <button class="btn btn--quiet btn--small" :disabled="busy || !rows.length" @click="askGc">
          <Spinner v-if="gc" />{{ t('workspace.vault.gc') }}
        </button>
      </div>
      <div class="ws-vault-action">
        <div class="ws-setting-copy">
          <span class="ws-setting-title">{{ t('workspace.vault.all_title') }}</span>
          <span class="ws-setting-hint">{{ t('workspace.vault.all_text') }}</span>
        </div>
        <button
          class="btn btn--quiet btn--danger btn--small"
          :disabled="busy || !rows.length"
          @click="askClearVault"
        >
          <Spinner v-if="clearing" />{{ t('workspace.vault.clear_all') }}
        </button>
      </div>
    </section>

    <section v-if="!rows.length" class="ws-card ws-empty-card">
      <span class="ws-empty-icon is-calm"><IconVault /></span>
      <b>{{ t('workspace.vault.empty_title') }}</b>
      <p>{{ t('workspace.vault.empty_text') }}</p>
    </section>

    <section v-else class="ws-card">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.vault.panel') }}</h2>
        <span class="ws-tag">{{ t('workspace.vault.count', { count: rows.length }) }}</span>
      </div>
      <div class="ws-rows">
        <div v-for="row in rows" :key="row.path" class="ws-row">
          <span class="ws-row-kind is-vault">{{ formatSize(row.size) }}</span>
          <div class="ws-row-main">
            <div class="ws-row-path" :title="row.path">{{ row.path }}</div>
            <div class="ws-row-meta">{{ t('workspace.vault.deleted', { ago: ago(row.deleted_at) }) }}</div>
          </div>
          <div class="ws-row-actions">
            <button
              class="btn btn--quiet btn--small"
              :disabled="busy"
              @click="restoreVault(row.path)"
            >
              <Spinner v-if="state.workspace.action === `restore:${row.path}`" />{{ t('workspace.vault.restore') }}
            </button>
          </div>
        </div>
      </div>
    </section>
  </div>
</template>
