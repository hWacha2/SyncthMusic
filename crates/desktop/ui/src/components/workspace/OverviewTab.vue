<script setup>
import DecisionButtons from './DecisionButtons.vue';
import IconCheck from '../icons/IconCheck.vue';
import { computed } from 'vue';
import { t } from '../../i18n/index.js';
import { formatSize, number } from '../../format.js';
import {
  attentionKind,
  decisionItems,
  decisionLabel,
  decisionMeta,
  pendingCount,
  planData,
  vaultTotals,
} from '../../workspace/model.js';
import { selectTab } from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
});

const pc = computed(() => props.data?.pc || {});
const plan = computed(() => planData(props.data));
const pending = computed(() => pendingCount(props.data));
const paths = computed(() => Number(plan.value?.paths) || 0);
const aligned = computed(() => Number(plan.value?.aligned) || 0);
const phoneScan = computed(() => props.data?.phone_scan);
const items = computed(() => decisionItems(props.data));
const vault = computed(() => vaultTotals(props.data));

// Карточка «Требует решения» не должна расти вместе со списком: показываем
// первые несколько нерешённых пунктов. Решённые здесь не нужны — они остаются
// во вкладке «Изменения», где решение можно отменить.
const ATTENTION_LIMIT = 3;
const pendingItems = computed(() => items.value.filter((item) => !item.decided));
const shownItems = computed(() => pendingItems.value.slice(0, ATTENTION_LIMIT));
const hiddenCount = computed(() => Math.max(0, pendingItems.value.length - ATTENTION_LIMIT));

const planNote = computed(() => {
  if (pending.value) return t('workspace.stats.plan_pending', { count: number(pending.value) });
  if (paths.value > 0) {
    return aligned.value > 0
      ? t('workspace.stats.plan_ready_aligned', { count: aligned.value })
      : t('workspace.stats.plan_ready');
  }
  if (aligned.value > 0) return t('workspace.stats.plan_aligned', { count: aligned.value });
  return t('workspace.stats.plan_empty');
});

function attentionMeta(item) {
  const meta = item.decided
    ? `${decisionMeta(item)} · ${decisionLabel(item.decision)}`
    : decisionMeta(item);
  return meta;
}
</script>

<template>
  <div class="ws-stats">
    <div class="ws-stat">
      <span class="ws-stat-k">{{ t('workspace.stats.pc') }}</span>
      <div class="ws-stat-v">{{ number(pc.local || 0) }}</div>
      <span class="ws-stat-n">{{ formatSize(pc.local_size || 0) }} · {{ t('workspace.stats.pc_note') }}</span>
    </div>
    <div class="ws-stat">
      <span class="ws-stat-k">{{ t('workspace.stats.phone') }}</span>
      <div class="ws-stat-v">{{ phoneScan ? number(phoneScan.count || 0) : '—' }}</div>
      <span class="ws-stat-n">
        {{ phoneScan
          ? `${formatSize(phoneScan.total || 0)} · ${t('workspace.stats.phone_note')}`
          : t('workspace.stats.phone_empty') }}
      </span>
    </div>
    <div class="ws-stat">
      <span class="ws-stat-k">{{ t('workspace.stats.plan') }}</span>
      <div class="ws-stat-v">{{ number(paths) }}</div>
      <span class="ws-stat-n">{{ planNote }}</span>
    </div>
    <div class="ws-stat">
      <span class="ws-stat-k">{{ t('workspace.stats.vault') }}</span>
      <div class="ws-stat-v">{{ number(vault.count) }}</div>
      <span class="ws-stat-n">
        {{ vault.count > 0 ? formatSize(vault.size) : t('workspace.stats.vault_empty') }}
      </span>
    </div>
  </div>

  <section class="ws-card ws-attention">
    <div class="ws-card-head">
      <h2 class="ws-card-title">{{ t('workspace.attention.title') }}</h2>
      <span class="ws-tag" :class="pending ? 'is-warn' : 'is-on'">
        {{ pending
          ? t('workspace.attention.count', { count: pending })
          : items.length ? t('workspace.attention.done') : t('workspace.attention.none') }}
      </span>
    </div>
    <div v-if="shownItems.length" class="ws-decisions">
      <div
        v-for="item in shownItems"
        :key="item.path"
        class="ws-decision"
        :class="{ 'is-decided': item.decided }"
      >
        <div class="ws-decision-main">
          <span class="ws-kind" :class="attentionKind(item).css">{{ attentionKind(item).label }}</span>
          <span class="ws-decision-path" :title="item.path">{{ item.path }}</span>
          <span class="ws-decision-meta">{{ attentionMeta(item) }}</span>
        </div>
        <div class="ws-decision-actions">
          <DecisionButtons :item="item" />
        </div>
      </div>
      <button v-if="hiddenCount" class="ws-more" @click="selectTab('changes')">
        <span>{{ t('workspace.attention.more', { count: hiddenCount }) }}</span>
        <span class="ws-more-arrow" aria-hidden="true">→</span>
      </button>
    </div>
    <div v-else class="ws-calm">
      <span class="ws-calm-icon"><IconCheck /></span>
      <div>
        <b>{{ t('workspace.attention.empty_title') }}</b>
        <p>{{ data?.manual_additions || data?.manual_changes
          ? t('workspace.attention.empty_manual')
          : t('workspace.attention.empty_text') }}</p>
      </div>
    </div>
  </section>
</template>
