<script setup>
import DeviceRow from '../components/DeviceRow.vue';
import IconPhone from '../components/icons/IconPhone.vue';
import IconUsb from '../components/icons/IconUsb.vue';
import NoteLine from '../components/NoteLine.vue';
import Spinner from '../components/Spinner.vue';
import { t } from '../i18n/index.js';
import {
  cancelConnect,
  connectAdb,
  connectLan,
  discoverAll,
  openLibrary,
  openWorkspace,
  state,
} from '../store.js';

const lanKey = (device) => `lan:${device.dev}`;
const adbKey = (device) => `adb:${device.serial}`;

function adbLabel(device) {
  if (device.state !== 'device') {
    return device.state === 'unauthorized' ? t('setup.unauthorized') : t('setup.offline');
  }
  return state.busy === adbKey(device) ? t('setup.connecting') : t('setup.connect');
}
</script>

<template>
  <section class="setup">
    <div class="setup-head">
      <h1 class="setup-title">{{ t('setup.title') }}</h1>
      <div class="setup-actions">
        <button v-if="state.phone.linked" class="btn" @click="openWorkspace">
          {{ t('workspace.back') }}
        </button>
        <button v-if="state.root" class="btn btn--quiet" @click="openLibrary">
          {{ t('setup.change_folder') }}
        </button>
        <button class="btn btn--quiet" :disabled="state.discovering" @click="discoverAll()">
          <Spinner v-if="state.discovering" />{{ t('setup.refresh') }}
        </button>
      </div>
    </div>

    <div class="setup-body">
      <section class="group">
        <h2 class="group-title">{{ t('setup.lan') }}</h2>
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
                class="btn btn--quiet"
                @click="cancelConnect"
              >
                {{ t('setup.cancel') }}
              </button>
              <button
                v-else
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

      <section class="group">
        <h2 class="group-title">{{ t('setup.adb') }}</h2>
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
                class="btn btn--quiet"
                @click="cancelConnect"
              >
                {{ t('setup.cancel') }}
              </button>
              <button
                v-else
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
    </div>

    <NoteLine />
  </section>
</template>
