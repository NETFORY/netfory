<script setup>
import { ref, onMounted, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { store, saveSettings } from "../store";

const savedMsg = ref(false);
const autostart = ref(false);

onMounted(async () => {
  try { autostart.value = await invoke("get_autostart"); } catch {}
});
async function toggleAutostart() {
  try {
    await invoke("set_autostart", { enable: autostart.value });
  } catch (e) {
    autostart.value = !autostart.value;
  }
}

async function browseOut() {
  const sel = await open({ directory: true });
  if (sel) store.settings.outDir = sel;
}
// Android: выбор папки не поддерживается, файлы идут в Download/netdrop
const isAndroid = /android/i.test(navigator.userAgent);
function save() {
  saveSettings();
  saveDeviceCfg();
  savedMsg.value = true;
  setTimeout(() => (savedMsg.value = false), 1500);
}

// ---------- Доверенные устройства ----------
const devName = ref("");
const listenMode = ref(false);
const pairCode = ref("");
const pairQrSvg = ref("");
const addCode = ref("");
const devErr = ref("");
const statuses = ref({});
const pinging = ref(false);

// онлайн-статус устройств: пинг по постоянному NodeId
async function pingAll() {
  if (pinging.value || !store.devices.length) return;
  pinging.value = true;
  try {
    statuses.value = await invoke("ping_devices");
  } catch {} finally {
    pinging.value = false;
  }
}

async function loadDevices() {
  try {
    const info = await invoke("device_info");
    devName.value = info.name || "";
    listenMode.value = !!info.listen;
    store.devices = info.devices || [];
  } catch {}
}
onMounted(async () => {
  await loadDevices();
  pingAll();
});
watch(() => store.devicesVersion, loadDevices);

async function saveDeviceCfg() {
  try {
    await invoke("set_device_cfg", {
      name: devName.value || "NetDrop",
      listen: listenMode.value,
      outDir: store.settings.outDir || "",
    });
  } catch (e) {
    devErr.value = String(e);
  }
}

async function showPairQr() {
  devErr.value = "";
  try {
    await saveDeviceCfg();
    const r = await invoke("pair_qr");
    pairCode.value = r.code;
    pairQrSvg.value = r.qrSvg || "";
  } catch (e) {
    devErr.value = String(e);
  }
}

async function copyPairCode() {
  try { await navigator.clipboard.writeText(pairCode.value); } catch {}
}

async function addDevice() {
  devErr.value = "";
  const code = addCode.value.trim();
  if (!code) return;
  try {
    await invoke("add_device_by_code", { code });
    addCode.value = "";
    await loadDevices();
  } catch (e) {
    devErr.value = String(e);
  }
}

async function delDevice(id) {
  try {
    await invoke("remove_device", { id });
    await loadDevices();
  } catch {}
}
</script>

<template>
  <section class="panel">
    <label class="set-label">{{ $t("setTheme") }}</label>
    <select v-model="store.settings.theme" @change="save">
      <option value="dark">{{ $t("themeDark") }}</option>
      <option value="light">{{ $t("themeLight") }}</option>
    </select>

    <label class="set-label">{{ $t("setRelayMode") }}</label>
    <select v-model="store.settings.useN1">
      <option :value="true">{{ $t("relayN0N1") }}</option>
      <option :value="false">{{ $t("relayN0") }}</option>
    </select>

    <label class="set-label">{{ $t("setExtraRelays") }}</label>
    <textarea v-model="store.settings.relays" spellcheck="false" placeholder="https://relay.example.com"></textarea>

    <label class="chk" v-if="!isAndroid">
      <input type="checkbox" v-model="autostart" @change="toggleAutostart" />
      <span>{{ $t("setAutostart") }}</span>
    </label>
    <label class="chk">
      <input type="checkbox" v-model="store.settings.encrypt" />
      <span>{{ $t("setEncrypt") }}</span>
    </label>
    <label class="chk">
      <input type="checkbox" v-model="store.settings.compress" />
      <span>{{ $t("setCompress") }}</span>
    </label>
    <label class="chk">
      <input type="checkbox" v-model="store.settings.multiRecv" />
      <span>{{ $t("setMultiRecv") }}</span>
    </label>

    <label class="set-label">{{ $t("setLimit") }}</label>
    <input type="text" v-model.number="store.settings.speedLimit" inputmode="numeric" spellcheck="false" />

    <label class="set-label">{{ $t("setTrusted") }}</label>
    <textarea v-model="store.settings.trusted" spellcheck="false" placeholder="a1b2c3… (64 hex)"></textarea>

    <label class="set-label">{{ $t("setOut") }}</label>
    <!-- Android: папка фиксирована (MediaStore → Download/netdrop), выбор скрыт -->
    <div v-if="isAndroid" class="status">{{ $t("outDirAndroid") }}</div>
    <div v-else class="row">
      <input type="text" v-model="store.settings.outDir" spellcheck="false" />
      <button class="btn ghost" @click="browseOut">{{ $t("browse") }}</button>
    </div>

    <label class="set-label">{{ $t("setDevices") }}</label>
    <div class="row">
      <input type="text" v-model="devName" :placeholder="$t('devNamePh')" spellcheck="false" data-testid="device-name-input" />
      <label class="chk" style="margin: 0">
        <input type="checkbox" v-model="listenMode" @change="saveDeviceCfg" data-testid="listen-toggle" />
        <span>{{ $t("setListen") }}</span>
      </label>
    </div>
    <div class="row">
      <button class="btn ghost" @click="showPairQr" data-testid="pair-qr-btn">{{ $t("btnPairQr") }}</button>
      <input type="text" v-model="addCode" :placeholder="$t('pairCodePh')" spellcheck="false" data-testid="pair-code-input" />
      <button class="btn primary" :disabled="!addCode.trim()" @click="addDevice" data-testid="add-device-btn">{{ $t("btnAddDevice") }}</button>
    </div>
    <div v-if="isAndroid" class="status">{{ $t("pairScanHint") }}</div>
    <div v-if="pairCode" class="tk-box">
      <div v-if="pairQrSvg" class="qr-wrap" v-html="pairQrSvg"></div>
      <div class="tk-label">{{ $t("pairCodeLabel") }}</div>
      <div class="row">
        <input type="text" :value="pairCode" readonly spellcheck="false" />
        <button class="btn ghost" @click="copyPairCode">{{ $t("btnCopy") }}</button>
      </div>
    </div>
    <div v-for="d in store.devices" :key="d.id" class="row" data-testid="paired-device-row">
      <span>
        <span class="dot" :class="{ on: statuses[d.id], checking: pinging }" :title="statuses[d.id] ? $t('devOnline') : $t('devOffline')"></span>
        <b>{{ d.name }}</b> <span class="hist-dim">{{ d.id.slice(0, 10) }}…</span>
      </span>
      <span style="flex: 1"></span>
      <button class="btn danger btn-sm" @click="delDevice(d.id)">✕</button>
    </div>
    <div v-if="store.devices.length" class="row">
      <button class="btn ghost btn-sm" :disabled="pinging" @click="pingAll" data-testid="ping-devices-btn">
        {{ pinging ? $t("devPinging") : $t("btnPing") }}
      </button>
    </div>
    <div class="status err" v-if="devErr">{{ devErr }}</div>

    <div class="row">
      <span style="flex: 1"></span>
      <button class="btn primary" @click="save">{{ $t("btnSave") }}</button>
    </div>
    <div class="status ok" v-if="savedMsg">{{ $t("saved") }}</div>
  </section>
</template>
