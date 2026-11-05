<script setup>
import IconOverview from '../icons/IconOverview.vue';
import Spinner from '../Spinner.vue';
import { t } from '../../i18n/index.js';
import { refreshWorkspace, state } from '../../store.js';
</script>

<template>
  <section v-if="state.workspace.error" class="ws-card ws-empty-card">
    <b>{{ t('workspace.status.error') }}</b>
    <p>{{ state.workspace.error }}</p>
    <button class="btn btn--small" @click="refreshWorkspace()">{{ t('workspace.retry') }}</button>
  </section>

  <section v-else-if="state.workspace.refreshing || state.workspace.stale" class="ws-card ws-empty-card ws-loading">
    <Spinner />
    <b>{{ t('workspace.loading.title') }}</b>
    <p>{{ t('workspace.loading.text') }}</p>
  </section>

  <section v-else class="ws-card ws-empty-card">
    <span class="ws-empty-icon"><IconOverview /></span>
    <b>{{ t('workspace.loading.title') }}</b>
    <p>{{ t('workspace.loading.text') }}</p>
  </section>
</template>
