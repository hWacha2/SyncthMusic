<script setup>
import DecisionButtons from './DecisionButtons.vue';
import UndoButton from './UndoButton.vue';
import IconCheck from '../icons/IconCheck.vue';
import IconPc from '../icons/IconPc.vue';
import IconPhone from '../icons/IconPhone.vue';
import Spinner from '../Spinner.vue';
import { computed } from 'vue';
import { t } from '../../i18n/index.js';
import { basename, formatSize, number } from '../../format.js';
import { openDevices, requestRootChange, state } from '../../store.js';
import {
  attentionKind,
  checkedText,
  connectionState,
  decisionItems,
  decisionLabel,
  decisionMeta,
  pendingCount,
  planData,
} from '../../workspace/model.js';
import { askForget } from '../../workspace/actions.js';

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
const conn = computed(() => connectionState(props.data));
const connected = computed(() => conn.value.tone === 'on');
const phone = computed(() => props.data?.phone || {});

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
      <div class="ws-stat-v">{{ number(pc.files || 0) }}</div>
      <span class="ws-stat-n">{{ t('workspace.stats.pc_note', { count: number(pc.local || 0) }) }}</span>
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
      <div class="ws-stat-v">{{ number(pc.vault_files || 0) }}</div>
      <span class="ws-stat-n">
        {{ Number(pc.vault_files) > 0 ? formatSize(pc.vault_size || 0) : t('workspace.stats.vault_empty') }}
      </span>
    </div>
  </div>

  <div class="ws-grid">
    <section class="ws-card ws-connection">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.connection.title') }}</h2>
        <span
          class="ws-tag"
          :class="{ 'is-on': connected, 'is-warn': conn.tone === 'warn' }"
        >{{ conn.label }}</span>
      </div>
      <div class="ws-signal" :class="connected ? 'is-live' : 'is-idle'">
        <div class="ws-node">
          <span class="ws-node-icon is-on"><IconPc /></span>
          <div class="ws-node-copy">
            <span class="ws-node-name">{{ pc.name || t('workspace.this_pc') }}</span>
            <span class="ws-node-sub">{{ t('workspace.connection.this_pc') }}</span>
          </div>
        </div>
        <div class="ws-wire" aria-hidden="true"><span></span><span></span><span></span></div>
        <div class="ws-node">
          <span class="ws-node-icon" :class="{ 'is-on': connected }"><IconPhone /></span>
          <div class="ws-node-copy">
            <span class="ws-node-name">{{ phone.linked ? (phone.name || phone.dev || t('workspace.connection.phone')) : t('workspace.connection.phone') }}</span>
            <span class="ws-node-sub">{{ phone.linked
              ? (phone.root ? basename(phone.root) : t('workspace.connection.no_folder'))
              : t('workspace.connection.not_linked') }}</span>
          </div>
        </div>
      </div>
      <div class="ws-kv-list">
        <div class="ws-kv">
          <span class="ws-kv-label">{{ t('workspace.connection.folder') }}</span>
          <span class="ws-kv-main">
            <span v-if="phone.root" class="ws-kv-value is-mono" :title="phone.root">{{ phone.root }}</span>
            <span v-else class="ws-kv-value is-empty">{{ t('workspace.connection.no_folder') }}</span>
          </span>
          <button
            v-if="phone.linked"
            class="btn btn--quiet btn--small"
            :disabled="Boolean(state.phoneBusy)"
            @click="requestRootChange()"
          >
            <Spinner v-if="state.phoneBusy === 'root'" />{{ t('workspace.change_root') }}
          </button>
        </div>
        <div class="ws-kv">
          <span class="ws-kv-label">{{ t('workspace.connection.address') }}</span>
          <span class="ws-kv-main"><span class="ws-kv-value is-mono">{{ phone.addr || '—' }}</span></span>
        </div>
        <div class="ws-kv">
          <span class="ws-kv-label">{{ t('workspace.connection.checked') }}</span>
          <span class="ws-kv-main"><span class="ws-kv-value">{{ checkedText(data) }}</span></span>
        </div>
      </div>
      <div class="ws-card-actions">
        <button
          v-if="phone.linked"
          class="btn btn--quiet btn--small"
          :disabled="Boolean(state.phoneBusy)"
          @click="askForget"
        >{{ t('workspace.disconnect') }}</button>
        <button v-else class="btn btn--small" @click="openDevices">{{ t('workspace.connect') }}</button>
      </div>
    </section>

    <section class="ws-card ws-attention">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.attention.title') }}</h2>
        <span class="ws-tag" :class="pending ? 'is-warn' : 'is-on'">
          {{ pending
            ? t('workspace.attention.count', { count: pending })
            : items.length ? t('workspace.attention.done') : t('workspace.attention.none') }}
        </span>
      </div>
      <div v-if="items.length" class="ws-decisions">
        <div
          v-for="item in items"
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
            <UndoButton v-if="item.decided" :path="item.path" />
            <DecisionButtons v-else :item="item" />
          </div>
        </div>
      </div>
      <div v-else class="ws-calm">
        <span class="ws-calm-icon"><IconCheck /></span>
        <div>
          <b>{{ t('workspace.attention.empty_title') }}</b>
          <p>{{ data?.manual_additions
            ? t('workspace.attention.empty_manual')
            : t('workspace.attention.empty_text') }}</p>
        </div>
      </div>
    </section>
  </div>
</template>
