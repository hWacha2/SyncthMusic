<script setup>
import { computed } from 'vue';
import { t } from '../../i18n/index.js';
import { formatDuration, formatSize } from '../../format.js';
import { state } from '../../store.js';

const progress = computed(() => state.workspace.progress);
const active = computed(() => Boolean(progress.value.stage));
const determined = computed(
  () => progress.value.total_bytes > 0 || progress.value.total > 0,
);

function clamp(value) {
  return Math.min(100, Math.max(0, Math.round(value)));
}

// Процент считаем по байтам, когда размер известен: файлы разного размера,
// и «384 из 926» может стоять на месте, пока байты уже идут.
const percent = computed(() => {
  const { done, total, done_bytes: doneBytes, total_bytes: totalBytes } =
    progress.value;
  if (totalBytes > 0) return clamp((doneBytes / totalBytes) * 100);
  if (total > 0) return clamp((done / total) * 100);
  return 0;
});

const bytes = computed(() => {
  const { done_bytes: done, total_bytes: total } = progress.value;
  if (!done && !total) return '';
  return total ? `${formatSize(done)} / ${formatSize(total)}` : formatSize(done);
});

const speed = computed(() => {
  const value = progress.value.speed;
  if (!(value > 0)) return '';
  return t('workspace.progress.speed', { speed: formatSize(value) });
});

const eta = computed(() => {
  const { done_bytes: done, total_bytes: total, speed: value } = progress.value;
  if (!(value > 0) || !(total > done)) return '';
  return t('workspace.progress.eta', {
    time: formatDuration((total - done) / value),
  });
});

const label = computed(() => {
  const { stage, done, total } = progress.value;
  if (stage === 'reset') return t('workspace.progress.reset');
  if (stage === 'pc_walk') {
    return done
      ? t('workspace.progress.pc_walk', { count: done })
      : t('workspace.progress.pc_start');
  }
  if (stage === 'pc_files') {
    return total
      ? t('workspace.progress.pc_files', { done, total })
      : t('workspace.progress.pc_walk', { count: done });
  }
  if (stage === 'phone') {
    if (total) return t('workspace.progress.phone', { done, total });
    return done
      ? t('workspace.progress.phone_walk', { count: done })
      : t('workspace.progress.phone_start');
  }
  if (stage === 'plan') return t('workspace.progress.plan');
  return '';
});
</script>

<template>
  <div v-if="active" class="ws-progress" role="status" aria-live="polite">
    <div class="ws-progress-head">
      <span class="ws-progress-label">{{ label }}</span>
      <span v-if="determined" class="ws-progress-percent">{{ percent }}%</span>
    </div>
    <div class="ws-progress-track" :class="{ 'is-indeterminate': !determined }">
      <span
        class="ws-progress-fill"
        :style="determined ? { width: `${percent}%` } : null"
      ></span>
    </div>
    <div v-if="bytes || speed || eta" class="ws-progress-meta">
      <span v-if="bytes" class="ws-progress-bytes">{{ bytes }}</span>
      <span v-if="speed">{{ speed }}</span>
      <span v-if="eta">{{ eta }}</span>
    </div>
  </div>
</template>
