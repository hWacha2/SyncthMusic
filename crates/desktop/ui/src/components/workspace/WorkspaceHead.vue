<script setup>
import { computed } from 'vue';
import Spinner from '../Spinner.vue';
import { t } from '../../i18n/index.js';
import { formatSize, number } from '../../format.js';
import { refreshWorkspace, state } from '../../store.js';
import { canCommit, headline, pendingCount, planData } from '../../workspace/model.js';
import { askGc, commitWorkspace } from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
  tab: { type: String, default: 'overview' },
});

const busy = computed(() => Boolean(state.workspace.refreshing || state.workspace.action));
const refreshing = computed(() => state.workspace.refreshing);

const head = computed(() => {
  if (props.tab === 'changes') {
    const plan = planData(props.data);
    const count = plan ? Number(plan.paths) || 0 : 0;
    const pending = pendingCount(props.data);
    return {
      title: t('workspace.tab.changes'),
      text: !props.data
        ? t('workspace.sub.unknown')
        : pending
          ? t('workspace.changes.sub_pending', { count, pending })
          : t('workspace.changes.sub', { count }),
    };
  }
  if (props.tab === 'settings') {
    return {
      title: t('workspace.tab.settings'),
      text: t('workspace.settings.sub'),
    };
  }
  if (props.tab === 'vault') {
    return {
      title: t('workspace.tab.vault'),
      text: t('workspace.vault.sub', {
        count: number(props.data?.pc?.vault_files || 0),
        size: formatSize(props.data?.pc?.vault_size || 0),
      }),
    };
  }
  return headline(props.data);
});

const hasVault = computed(() => Number(props.data?.pc?.vault_files) > 0);

const fixing = computed(() => {
  const plan = planData(props.data);
  return (Number(plan?.paths) || 0) === 0 && (Number(plan?.aligned) || 0) > 0;
});

const commitLabel = computed(() => {
  if (state.workspace.action === 'commit') {
    return fixing.value ? t('workspace.fixing') : t('workspace.syncing');
  }
  return fixing.value ? t('workspace.fix') : t('workspace.sync');
});
</script>

<template>
  <header class="ws-head">
    <div class="ws-head-copy">
      <p class="ws-kicker">{{ t('workspace.kicker') }}</p>
      <h1 class="ws-title">{{ head.title }}</h1>
      <p class="ws-sub">{{ head.text }}</p>
    </div>
    <div v-if="tab !== 'settings'" class="ws-head-actions">
      <button class="btn btn--quiet" :disabled="busy" @click="refreshWorkspace()">
        <Spinner v-if="refreshing" />
        {{ refreshing ? t('workspace.checking') : t('workspace.check') }}
      </button>
      <template v-if="tab === 'vault'">
        <button class="btn" :disabled="!hasVault || busy" @click="askGc">
          <Spinner v-if="state.workspace.action === 'gc'" />
          {{ t('workspace.vault.gc') }}
        </button>
      </template>
      <template v-else>
        <button class="btn" :disabled="!canCommit(data)" @click="commitWorkspace">
          <Spinner v-if="state.workspace.action === 'commit'" />
          {{ commitLabel }}
        </button>
      </template>
    </div>
  </header>
</template>
