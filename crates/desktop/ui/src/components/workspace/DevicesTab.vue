<script setup>
import DeviceRow from '../DeviceRow.vue';
import NoteLine from '../NoteLine.vue';
import IconPc from '../icons/IconPc.vue';
import IconPhone from '../icons/IconPhone.vue';
import IconUsb from '../icons/IconUsb.vue';
import Spinner from '../Spinner.vue';
import { computed, onMounted } from 'vue';
import { t } from '../../i18n/index.js';
import { basename } from '../../format.js';
import {
  cancelConnect,
  connectAdb,
  connectLan,
  discoverAll,
  disconnectPhone,
  reconnect,
  requestRootChange,
  state,
} from '../../store.js';
import { checkedText } from '../../workspace/model.js';
import { askForget } from '../../workspace/actions.js';

const props = defineProps({
  data: { type: Object, default: null },
});

const phone = computed(() => props.data?.phone || state.phone || {});
const pc = computed(() => props.data?.pc || {});
const linked = computed(() => Boolean(phone.value.linked));
const connected = computed(() => Boolean(phone.value.connected));

const conn = computed(() => {
  // Пока идёт проверка, «Нет связи» вводит в заблуждение: сеанс как раз
  // используется для сканирования.
  if (state.workspace.refreshing) {
    return { tone: 'warn', label: t('workspace.status.checking') };
  }
  if (!linked.value) return { tone: 'off', label: t('workspace.settings.not_linked') };
  if (!connected.value) return { tone: 'off', label: t('workspace.status.offline') };
  return { tone: 'on', label: t('workspace.status.connected') };
});

const lanKey = (device) => `lan:${device.dev}`;
const adbKey = (device) => `adb:${device.serial}`;

function adbLabel(device) {
  if (device.state !== 'device') {
    return device.state === 'unauthorized' ? t('setup.unauthorized') : t('setup.offline');
  }
  return state.busy === adbKey(device) ? t('setup.connecting') : t('setup.connect');
}

onMounted(() => {
  discoverAll();
});
</script>

<template>
  <div class="ws-devices">
    <section v-if="linked" class="ws-card ws-connection">
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
            <span class="ws-node-name">{{ phone.name || phone.dev || t('workspace.connection.phone') }}</span>
            <span class="ws-node-sub">{{ phone.root
              ? basename(phone.root)
              : t('workspace.connection.no_folder') }}</span>
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
            v-if="connected"
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
          v-if="!connected"
          type="button"
          class="btn btn--small"
          :disabled="Boolean(state.busy)"
          @click="reconnect()"
        >
          <Spinner v-if="state.busy === 'reconnect'" />{{ t('workspace.reconnect') }}
        </button>
        <button
          v-if="connected"
          type="button"
          class="btn btn--quiet btn--small"
          :disabled="Boolean(state.phoneBusy)"
          @click="disconnectPhone"
        >
          <Spinner v-if="state.phoneBusy === 'disconnect'" />{{ t('workspace.disconnect') }}
        </button>
        <button
          type="button"
          class="btn btn--quiet btn--small btn--danger"
          :disabled="Boolean(state.phoneBusy)"
          @click="askForget"
        >
          <Spinner v-if="state.phoneBusy === 'forget'" />{{ t('workspace.forget') }}
        </button>
      </div>
    </section>

    <template v-if="!connected">
      <section class="ws-card ws-setting-card">
        <div class="ws-card-head">
          <h2 class="ws-card-title">{{ t('setup.lan') }}</h2>
        </div>
        <div class="device-list">
          <template v-if="state.lan.length">
            <DeviceRow
              v-for="device in state.lan"
              :key="device.dev"
              kind="lan"
              :name="device.name || device.dev"
              :sub="device.addr"
              :busy="state.busy === lanKey(device)"
            >
              <template #icon><IconPhone /></template>
              <button
                v-if="state.busy === lanKey(device)"
                type="button"
                class="btn btn--quiet"
                @click="cancelConnect"
              >
                {{ t('setup.cancel') }}
              </button>
              <button
                v-else
                type="button"
                class="btn"
                :disabled="Boolean(state.busy)"
                @click="connectLan(device.dev, device.addr)"
              >
                {{ t('setup.connect') }}
              </button>
            </DeviceRow>
          </template>
          <div v-else>
            <p class="group-empty">
              {{ state.discovering ? t('setup.searching') : t('setup.empty_lan') }}
            </p>
            <span v-if="!state.discovering && state.discovered" class="group-hint">
              {{ t('setup.lan_hint') }}
            </span>
          </div>
        </div>
      </section>

      <section class="ws-card ws-setting-card">
        <div class="ws-card-head">
          <h2 class="ws-card-title">{{ t('setup.adb') }}</h2>
        </div>
        <div class="device-list">
          <template v-if="state.adb.length">
            <DeviceRow
              v-for="device in state.adb"
              :key="device.serial"
              kind="usb"
              :name="device.name || device.model || device.serial"
              :sub="device.serial"
              :busy="state.busy === adbKey(device)"
            >
              <template #icon><IconUsb /></template>
              <button
                v-if="state.busy === adbKey(device)"
                type="button"
                class="btn btn--quiet"
                @click="cancelConnect"
              >
                {{ t('setup.cancel') }}
              </button>
              <button
                v-else
                type="button"
                class="btn"
                :disabled="device.state !== 'device' || Boolean(state.busy)"
                @click="connectAdb(device.serial)"
              >
                {{ adbLabel(device) }}
              </button>
            </DeviceRow>
          </template>
          <p v-else class="group-empty">
            {{ state.discovering ? t('setup.searching') : t('setup.empty_adb') }}
          </p>
        </div>
        <p v-if="state.adbError" class="group-error">{{ state.adbError }}</p>
      </section>
    </template>

    <NoteLine />
  </div>
</template>
