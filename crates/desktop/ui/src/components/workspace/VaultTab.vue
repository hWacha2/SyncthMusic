<script setup>
import IconVault from '../icons/IconVault.vue';
import SortTabs from './SortTabs.vue';
import Spinner from '../Spinner.vue';
import { computed, ref } from 'vue';
import { t } from '../../i18n/index.js';
import { ago, formatSize } from '../../format.js';
import { state } from '../../store.js';
import { sortByFolder, sortByTime, sortEntries } from '../../workspace/model.js';
import {
  askClearVault,
  askGc,
  askPhoneVaultClear,
  askPhoneVaultGc,
  loadPhoneVault,
  restorePhoneVault,
  restoreVault,
} from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
});

// Хранилище выглядит одинаково для обоих устройств: переключатель меняет
// только источник строк и адресата команд очистки.
const device = ref('pc');
const onPhone = computed(() => device.value === 'phone');

const sort = ref('time');
const sortOptions = [
  { id: 'time', label: 'workspace.sort.time' },
  { id: 'folders', label: 'workspace.changes.sort_folders' },
  { id: 'alpha', label: 'workspace.changes.sort_alpha' },
];

const pcRows = computed(() => props.data?.vault || []);
const phoneRows = computed(() => state.workspace.phoneVault || []);
const rows = computed(() => (onPhone.value ? phoneRows.value : pcRows.value));
const sortedRows = computed(() => {
  if (sort.value === 'alpha') return sortEntries(rows.value);
  if (sort.value === 'folders') return sortByFolder(rows.value);
  return sortByTime(rows.value, 'deleted_at');
});

const busy = computed(() => Boolean(state.workspace.action || state.workspace.refreshing));
const gc = computed(() => state.workspace.action === (onPhone.value ? 'phone-vault-gc' : 'gc'));
const clearing = computed(
  () => state.workspace.action === (onPhone.value ? 'phone-vault-clear' : 'clear-vault'),
);

const phoneLinked = computed(() => Boolean(props.data?.phone?.linked));
const phoneConnected = computed(() => Boolean(props.data?.phone?.connected));
const phoneLoading = computed(() => state.workspace.phoneVaultLoading);
const phoneError = computed(() => state.workspace.phoneVaultError);

// Очистка доступна только когда список этого устройства реально прочитан.
const canClear = computed(() => {
  if (!rows.value.length) return false;
  if (!onPhone.value) return true;
  return phoneConnected.value && !phoneLoading.value && !phoneError.value;
});

const hint = computed(() =>
  onPhone.value ? t('workspace.phone_vault.hint') : t('workspace.vault.hint'),
);
const panel = computed(() =>
  onPhone.value ? t('workspace.phone_vault.panel') : t('workspace.vault.panel'),
);
const restoreLabel = computed(() =>
  onPhone.value ? t('workspace.phone_vault.restore') : t('workspace.vault.restore'),
);

function selectDevice(next) {
  if (next === 'phone' && !phoneLinked.value) return;
  device.value = next;
  // Список телефона уже загружен при входе в рабочую область. Здесь только
  // добираем его, если кэша ещё нет; после удалений и восстановлений его
  // обновляют сами действия, а не каждое переключение вкладки.
  if (next === 'phone') loadPhoneVault();
}

function askGcDevice() {
  if (onPhone.value) askPhoneVaultGc();
  else askGc();
}

function askClearDevice() {
  if (onPhone.value) askPhoneVaultClear();
  else askClearVault();
}

function restore(path) {
  if (onPhone.value) restorePhoneVault(path);
  else restoreVault(path);
}

function restoring(path) {
  const prefix = onPhone.value ? 'restore-phone' : 'restore';
  return state.workspace.action === `${prefix}:${path}`;
}
</script>

<template>
  <div class="ws-vault">
    <section class="ws-card ws-vault-device">
      <div class="ws-segmented" role="group" :aria-label="t('workspace.vault.device')">
        <button
          type="button"
          class="ws-segment"
          :class="{ 'is-on': !onPhone }"
          :aria-pressed="onPhone ? 'false' : 'true'"
          @click="selectDevice('pc')"
        >
          {{ t('workspace.vault.device_pc') }}
        </button>
        <button
          type="button"
          class="ws-segment"
          :class="{ 'is-on': onPhone }"
          :aria-pressed="onPhone ? 'true' : 'false'"
          :disabled="!phoneLinked"
          @click="selectDevice('phone')"
        >
          {{ t('workspace.vault.device_phone') }}
        </button>
      </div>
      <p class="ws-vault-hint">{{ hint }}</p>
    </section>

    <section class="ws-card ws-vault-actions">
      <div class="ws-vault-action">
        <div class="ws-setting-copy">
          <span class="ws-setting-title">{{ t('workspace.vault.old_title') }}</span>
          <span class="ws-setting-hint">{{ t('workspace.vault.old_text') }}</span>
        </div>
        <button
          class="btn btn--quiet btn--small"
          :disabled="busy || !canClear"
          @click="askGcDevice"
        >
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
          :disabled="busy || !canClear"
          @click="askClearDevice"
        >
          <Spinner v-if="clearing" />{{ t('workspace.vault.clear_all') }}
        </button>
      </div>
    </section>

    <template v-if="onPhone && !phoneConnected">
      <section class="ws-card ws-empty-card">
        <span class="ws-empty-icon is-calm"><IconVault /></span>
        <b>{{ t('workspace.phone_vault.offline_title') }}</b>
        <p>{{ t('workspace.phone_vault.offline_text') }}</p>
      </section>
    </template>

    <template v-else-if="onPhone && phoneError && !rows.length">
      <section class="ws-card ws-empty-card">
        <span class="ws-empty-icon is-calm"><IconVault /></span>
        <b>{{ panel }}</b>
        <p>{{ t('workspace.error.phone_vault', { error: phoneError }) }}</p>
        <button class="btn btn--quiet btn--small" @click="loadPhoneVault(true)">
          {{ t('workspace.phone_vault.retry') }}
        </button>
      </section>
    </template>

    <template v-else-if="onPhone && phoneLoading && !rows.length">
      <section class="ws-card ws-empty-card">
        <span class="ws-empty-icon is-calm"><IconVault /></span>
        <b>{{ panel }}</b>
        <p>{{ t('workspace.phone_vault.loading') }}</p>
      </section>
    </template>

    <template v-else-if="!rows.length">
      <section class="ws-card ws-empty-card">
        <span class="ws-empty-icon is-calm"><IconVault /></span>
        <b>{{ onPhone ? t('workspace.phone_vault.empty_title') : t('workspace.vault.empty_title') }}</b>
        <p>{{ onPhone ? t('workspace.phone_vault.empty_text') : t('workspace.vault.empty_text') }}</p>
      </section>
    </template>

    <section v-else class="ws-card">
      <div class="ws-card-head">
        <h2 class="ws-card-title">{{ panel }}</h2>
        <span class="ws-tag">{{ t('workspace.vault.count', { count: rows.length }) }}</span>
      </div>
      <!-- Пока список обновляется, уже прочитанные строки остаются на месте:
           иначе при каждом входе на вкладку телефона мелькает пустой экран. -->
      <p v-if="onPhone && phoneError" class="ws-vault-hint">
        {{ t('workspace.error.phone_vault', { error: phoneError }) }}
      </p>
      <div class="ws-sort-row ws-card-sort">
        <SortTabs v-model="sort" :options="sortOptions" />
      </div>
      <div class="ws-rows">
        <div v-for="row in sortedRows" :key="row.path" class="ws-row">
          <span class="ws-row-kind is-vault">{{ formatSize(row.size) }}</span>
          <div class="ws-row-main">
            <div class="ws-row-path" :title="row.path">{{ row.path }}</div>
            <div class="ws-row-meta">{{ t('workspace.vault.deleted', { ago: ago(row.deleted_at) }) }}</div>
          </div>
          <div class="ws-row-actions">
            <button class="btn btn--quiet btn--small" :disabled="busy" @click="restore(row.path)">
              <Spinner v-if="restoring(row.path)" />{{ restoreLabel }}
            </button>
          </div>
        </div>
      </div>
    </section>
  </div>
</template>
