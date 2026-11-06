<script setup>
import { computed, onUnmounted, ref, watch } from 'vue';
import { t } from '../../i18n/index.js';
import { state } from '../../store.js';

const props = defineProps({
  data: { type: Object, default: null },
});

// Источники уведомлений: результаты действий и текущие ошибки связи.
// Один и тот же текст не повторяем, пока он не сменится или не исчезнет.
const rows = computed(() => {
  const list = [];
  if (state.workspace.error) {
    list.push({
      key: 'refresh',
      tone: 'bad',
      text: t('workspace.error.refresh', { error: state.workspace.error }),
    });
  }
  if (props.data?.session_error) {
    list.push({
      key: 'session',
      tone: 'warn',
      text: t('workspace.error.session', { error: props.data.session_error }),
    });
  }
  if (props.data?.phone_error) {
    list.push({
      key: 'phone',
      tone: 'warn',
      text: t('workspace.error.phone', { error: props.data.phone_error }),
    });
  }
  if (state.workspace.statusError) {
    list.push({
      key: 'status',
      tone: 'warn',
      text: t('workspace.error.status', { error: state.workspace.statusError }),
    });
  }
  if (state.workspace.note) {
    // Результат операции — событие, а не постоянное состояние: у каждого
    // сообщения свой ключ, чтобы новые тосты не вытесняли предыдущие.
    const tone = state.workspace.noteError ? 'bad' : 'ok';
    list.push({
      key: `note:${tone}|${state.workspace.note}`,
      tone,
      text: state.workspace.note,
    });
  }
  return list;
});

const toasts = ref([]);
const timers = new Map();
const seen = new Map();
let nextId = 1;

function dismiss(id) {
  const timer = timers.get(id);
  if (timer) {
    clearTimeout(timer);
    timers.delete(id);
  }
  toasts.value = toasts.value.filter((toast) => toast.id !== id);
}

function show(row) {
  // Один постоянный источник — один тост: новый текст заменяет старый.
  // Сообщения операций приходят с разными ключами и копятся до четырёх.
  for (const toast of toasts.value.filter((toast) => toast.key === row.key)) {
    dismiss(toast.id);
  }
  // Больше четырёх сообщений одновременно читать всё равно невозможно.
  while (toasts.value.length >= 4) dismiss(toasts.value[0].id);

  const id = nextId++;
  toasts.value.push({ id, key: row.key, tone: row.tone, text: row.text });
  const seconds = Number(state.toastSeconds) || 0;
  if (seconds > 0) {
    timers.set(
      id,
      setTimeout(() => dismiss(id), seconds * 1000),
    );
  }
}

watch(
  rows,
  (list) => {
    const present = new Set(list.map((row) => row.key));
    for (const key of [...seen.keys()]) {
      if (!present.has(key)) seen.delete(key);
    }
    for (const row of list) {
      const signature = `${row.tone}|${row.text}`;
      if (seen.get(row.key) === signature) continue;
      seen.set(row.key, signature);
      show(row);
    }
  },
  { immediate: true },
);

onUnmounted(() => {
  for (const timer of timers.values()) clearTimeout(timer);
  timers.clear();
});
</script>

<template>
  <!-- Телепорт в body: иначе `position: fixed` привязан к анимированному
       предку экрана и тосты уезжают вместе с прокруткой. -->
  <Teleport to="body">
    <TransitionGroup name="ws-toast" tag="div" class="ws-toasts">
      <div
        v-for="toast in toasts"
        :key="toast.id"
        class="ws-toast"
        :class="`is-${toast.tone}`"
        :role="toast.tone === 'bad' ? 'alert' : 'status'"
      >
        <span class="ws-toast-dot" aria-hidden="true"></span>
        <span class="ws-toast-text">{{ toast.text }}</span>
        <button
          type="button"
          class="ws-toast-close"
          :aria-label="t('workspace.toast.dismiss')"
          @click="dismiss(toast.id)"
        >
          ×
        </button>
      </div>
    </TransitionGroup>
  </Teleport>
</template>
