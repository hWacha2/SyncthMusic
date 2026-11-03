<script setup>
import IconVault from '../icons/IconVault.vue';
import Spinner from '../Spinner.vue';
import { computed } from 'vue';
import { t } from '../../i18n/index.js';
import { ago, formatSize } from '../../format.js';
import { state } from '../../store.js';
import { restoreVault } from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
});

const rows = computed(() => props.data?.vault || []);
</script>

<template>
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
            :disabled="Boolean(state.workspace.action)"
            @click="restoreVault(row.path)"
          >
            <Spinner v-if="state.workspace.action === `restore:${row.path}`" />{{ t('workspace.vault.restore') }}
          </button>
        </div>
      </div>
    </div>
  </section>
</template>
