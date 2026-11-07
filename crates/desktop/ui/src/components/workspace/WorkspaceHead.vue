<script setup>
import { computed } from 'vue';
import Spinner from '../Spinner.vue';
import { t } from '../../i18n/index.js';
import { formatSize, number } from '../../format.js';
import { refreshWorkspace, state } from '../../store.js';
import { canCommit, headline, pendingCount, planData, vaultTotals } from '../../workspace/model.js';
import { commitWorkspace } from '../../workspace/actions.js';

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
    const vault = vaultTotals(props.data);
    return {
      title: t('workspace.tab.vault'),
      text: t('workspace.vault.sub', {
        count: number(vault.count),
        size: formatSize(vault.size),
      }),
    };
  }
  if (props.tab === 'exclusions') {
    return {
      title: t('workspace.tab.exclusions'),
      text: t('workspace.exclusions.sub', {
        count: number(props.data?.exclusions?.length || 0),
      }),
    };
  }
  return headline(props.data);
});

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
    <div v-if="tab !== 'settings' && tab !== 'vault' && tab !== 'exclusions'" class="ws-head-actions">
      <button class="btn btn--quiet" :disabled="busy" @click="refreshWorkspace()">
        <Spinner v-if="refreshing" />
        {{ refreshing ? t('workspace.checking') : t('workspace.check') }}
      </button>
      <button class="btn" :disabled="!canCommit(data)" @click="commitWorkspace">
        <Spinner v-if="state.workspace.action === 'commit'" />
        {{ commitLabel }}
      </button>
    </div>
  </header>
</template>
