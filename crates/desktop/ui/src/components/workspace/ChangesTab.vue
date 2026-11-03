<script setup>
import DecisionButtons from './DecisionButtons.vue';
import RejectButton from './RejectButton.vue';
import UndoButton from './UndoButton.vue';
import IconCheck from '../icons/IconCheck.vue';
import IconList from '../icons/IconList.vue';
import { computed } from 'vue';
import { t } from '../../i18n/index.js';
import { number } from '../../format.js';
import { refreshWorkspace } from '../../store.js';
import {
  changeMeta,
  decisionLabel,
  kindLabel,
  planData,
} from '../../workspace/model.js';

const props = defineProps({
  data: { type: Object, default: null },
});

const plan = computed(() => planData(props.data));
const manual = computed(() => Boolean(props.data?.manual_additions));
const changes = computed(() => (plan.value?.changes || []).filter((change) => !change.aligned));
const aligned = computed(() => Number(plan.value?.aligned) || 0);
const local = computed(() => changes.value.filter((change) => change.side === 'local'));
const remote = computed(() => changes.value.filter((change) => change.side === 'remote'));
const conflicts = computed(() => plan.value?.conflicts || []);
const hasAny = computed(() => Boolean(local.value.length || remote.value.length || conflicts.value.length));

function isPending(change) {
  return (change.kind === 'deleted' || (manual.value && change.kind === 'added'))
    && !change.decided
    && !change.aligned;
}

function isAddition(change) {
  return manual.value && change.kind === 'added' && !change.aligned && !change.decided;
}

function isAuto(change) {
  return !change.aligned && !change.decided && !isPending(change);
}

function itemFor(change) {
  return {
    type: isAddition(change) ? 'addition' : 'deletion',
    path: change.path,
    change,
    size: change.size,
  };
}

</script>

<template>
  <section v-if="!plan" class="ws-card ws-empty-card">
    <span class="ws-empty-icon"><IconList /></span>
    <b>{{ t('workspace.changes.empty_title') }}</b>
    <p>{{ t('workspace.changes.empty_text') }}</p>
    <button class="btn btn--small" @click="refreshWorkspace()">{{ t('workspace.check') }}</button>
  </section>

  <section v-else-if="!hasAny" class="ws-card ws-empty-card">
    <span class="ws-empty-icon is-calm"><IconCheck /></span>
    <b>{{ t('workspace.changes.empty_title') }}</b>
    <p>{{ aligned > 0
      ? t('workspace.changes.aligned_text', { count: aligned })
      : t('workspace.changes.empty_text') }}</p>
    <button class="btn btn--small" @click="refreshWorkspace()">{{ t('workspace.check') }}</button>
  </section>

  <div v-else class="ws-groups">
    <p v-if="aligned > 0" class="ws-aligned-note">
      {{ t('workspace.changes.aligned_note', { count: aligned }) }}
    </p>

    <section v-if="local.length" class="ws-card ws-group">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.changes.local') }}</h2>
        <span class="ws-tag">{{ number(local.length) }}</span>
      </div>
      <div class="ws-rows">
        <div v-for="change in local" :key="change.path" class="ws-row">
          <span class="ws-row-kind" :class="`is-${change.kind}`">{{ kindLabel(change.kind) }}</span>
          <div class="ws-row-main">
            <div class="ws-row-path" :title="change.path">{{ change.path }}</div>
            <div class="ws-row-meta">{{ changeMeta(change, manual) }}</div>
          </div>
          <div v-if="isPending(change)" class="ws-row-actions">
            <DecisionButtons :item="itemFor(change)" />
          </div>
          <div v-else-if="change.decided" class="ws-row-actions">
            <UndoButton :path="change.path" />
          </div>
          <div v-else-if="isAuto(change)" class="ws-row-actions">
            <RejectButton :path="change.path" />
          </div>
        </div>
      </div>
    </section>

    <section v-if="remote.length" class="ws-card ws-group">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.changes.remote') }}</h2>
        <span class="ws-tag">{{ number(remote.length) }}</span>
      </div>
      <div class="ws-rows">
        <div v-for="change in remote" :key="change.path" class="ws-row">
          <span class="ws-row-kind" :class="`is-${change.kind}`">{{ kindLabel(change.kind) }}</span>
          <div class="ws-row-main">
            <div class="ws-row-path" :title="change.path">{{ change.path }}</div>
            <div class="ws-row-meta">{{ changeMeta(change, manual) }}</div>
          </div>
          <div v-if="isPending(change)" class="ws-row-actions">
            <DecisionButtons :item="itemFor(change)" />
          </div>
          <div v-else-if="change.decided" class="ws-row-actions">
            <UndoButton :path="change.path" />
          </div>
          <div v-else-if="isAuto(change)" class="ws-row-actions">
            <RejectButton :path="change.path" />
          </div>
        </div>
      </div>
    </section>

    <section v-if="conflicts.length" class="ws-card ws-group">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ t('workspace.changes.conflicts') }}</h2>
        <span class="ws-tag">{{ number(conflicts.length) }}</span>
      </div>
      <div class="ws-rows">
        <div v-for="conflict in conflicts" :key="conflict.path" class="ws-row">
          <span class="ws-row-kind is-conflict">{{ kindLabel('conflict') }}</span>
          <div class="ws-row-main">
            <div class="ws-row-path" :title="conflict.path">{{ conflict.path }}</div>
            <div class="ws-row-meta">
              {{ conflict.unresolved ? t('workspace.changes.undecided') : decisionLabel(conflict.decision) }}
            </div>
          </div>
          <div v-if="conflict.unresolved" class="ws-row-actions">
            <DecisionButtons :item="{ type: 'conflict', path: conflict.path, conflict, size: 0 }" />
          </div>
          <div v-else class="ws-row-actions">
            <UndoButton :path="conflict.path" />
          </div>
        </div>
      </div>
    </section>
  </div>
</template>
