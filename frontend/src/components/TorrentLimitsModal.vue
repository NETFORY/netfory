<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, onMounted, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { getTorrentLimits, setTorrentLimits, getRutrackerKey, setRutrackerKey, getTorrentNetSettings, setTorrentNetSettings } from '@/lib/bridge'

const emit = defineEmits<{ (e: 'close'): void }>()
const { t } = useI18n()

// KiB/s, 0 = безлимит. Слайдер до 102400 KiB/s (100 МиБ/с).
const MAX = 102400
const download = ref(0)
const upload = ref(0)
const rutrackerKey = ref('')
const saving = ref(false)

type Tab = 'conn' | 'speed' | 'bt'
const tab = ref<Tab>('conn')

// --- Настройки соединений (порт / UPnP / µTP / лимит пиров) ---
const listenPort = ref(0)
const randomPort = ref(true)
const upnp = ref(true)
const utp = ref(true)
const peerLimit = ref(0)
// --- BitTorrent (DHT / LSD) ---
const dht = ref(true)
const lsd = ref(true)

const unitKib = computed(() => t('torrentLimits.unitKibps'))
const unitMib = computed(() => t('torrentLimits.unitMibps'))

function label(kib: number): string {
  if (!kib) return '∞'
  if (kib >= 1024) return `${(kib / 1024).toFixed(1)} ${unitMib.value}`
  return `${kib} ${unitKib.value}`
}

function rollRandomPort() {
  // Случайный высокий порт (как qBittorrent): 49152–65535.
  listenPort.value = 49152 + Math.floor(Math.random() * (65535 - 49152))
  randomPort.value = false
}

onMounted(async () => {
  const l = await getTorrentLimits()
  download.value = l.downloadKib
  upload.value = l.uploadKib
  rutrackerKey.value = await getRutrackerKey()
  const net = await getTorrentNetSettings()
  listenPort.value = net.listenPort
  randomPort.value = net.listenPort === 0
  upnp.value = net.upnp
  utp.value = net.utp
  peerLimit.value = net.peerLimit
  dht.value = net.dht
  lsd.value = net.lsd
})

async function save() {
  saving.value = true
  try {
    await setTorrentLimits(Math.max(0, download.value | 0), Math.max(0, upload.value | 0))
    await setRutrackerKey(rutrackerKey.value.trim())
    await setTorrentNetSettings({
      listenPort: randomPort.value ? 0 : Math.min(65535, Math.max(1, listenPort.value | 0)),
      upnp: upnp.value,
      utp: utp.value,
      peerLimit: Math.max(0, peerLimit.value | 0),
      dht: dht.value,
      lsd: lsd.value,
    })
    emit('close')
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <div class="lm-overlay" data-testid="torrent-limits-modal">
    <div class="lm-card">
      <header class="lm-head">
        <span class="lm-ic">⚡</span>
        <h3 class="lm-title">{{ t('torrentLimits.title') }}</h3>
        <button class="lm-x" @click="emit('close')" data-testid="torrent-limits-close">✕</button>
      </header>

      <div class="lm-tabs" role="tablist">
        <button
          class="lm-tab"
          :class="{ active: tab === 'conn' }"
          @click="tab = 'conn'"
          data-testid="tab-conn"
        >{{ t('torrentLimits.tabConn') }}</button>
        <button
          class="lm-tab"
          :class="{ active: tab === 'speed' }"
          @click="tab = 'speed'"
          data-testid="tab-speed"
        >{{ t('torrentLimits.tabSpeed') }}</button>
        <button
          class="lm-tab"
          :class="{ active: tab === 'bt' }"
          @click="tab = 'bt'"
          data-testid="tab-bt"
        >{{ t('torrentLimits.tabBt') }}</button>
      </div>

      <div class="lm-body">
        <!-- ВКЛАДКА: СОЕДИНЕНИЕ -->
        <div v-show="tab === 'conn'" class="lm-pane" data-testid="pane-conn">
          <div class="lm-conn-row">
            <span class="lm-conn-lbl">{{ t('torrentLimits.protoLabel') }}</span>
            <select v-model="utp" class="lm-select" data-testid="torrent-proto">
              <option :value="true">{{ t('torrentLimits.protoBoth') }}</option>
              <option :value="false">{{ t('torrentLimits.protoTcpOnly') }}</option>
            </select>
          </div>
          <span class="lm-field-hint">
            {{ t('torrentLimits.protoHint') }}
          </span>

          <div class="lm-conn-row">
            <label class="lm-chk">
              <input type="checkbox" v-model="randomPort" data-testid="torrent-random-port" />
              <span>{{ t('torrentLimits.randomPort') }}</span>
            </label>
            <div class="lm-num">
              <input
                type="number"
                min="1"
                max="65535"
                :disabled="randomPort"
                v-model.number="listenPort"
                :placeholder="randomPort ? t('torrentLimits.portAuto') : ''"
                data-testid="torrent-listen-port"
              />
              <button class="lm-inf" @click="rollRandomPort" data-testid="torrent-roll-port" :title="t('torrentLimits.rollPortTitle')">↻</button>
            </div>
          </div>

          <label class="lm-chk wide">
            <input type="checkbox" v-model="upnp" data-testid="torrent-upnp" />
            <span>{{ t('torrentLimits.upnp') }}</span>
          </label>

          <div class="lm-conn-row">
            <span class="lm-conn-lbl">{{ t('torrentLimits.peersMax') }}</span>
            <div class="lm-num">
              <input
                type="number"
                min="0"
                v-model.number="peerLimit"
                :placeholder="t('torrentLimits.peersPlaceholder')"
                data-testid="torrent-peer-limit"
              />
            </div>
          </div>

          <span class="lm-field-hint">
            {{ t('torrentLimits.connHint') }}
          </span>
        </div>

        <!-- ВКЛАДКА: СКОРОСТЬ -->
        <div v-show="tab === 'speed'" class="lm-pane" data-testid="pane-speed">
          <div class="lm-row">
            <div class="lm-row-top">
              <span class="lm-lbl down">{{ t('torrentLimits.speedDown') }}</span>
              <span class="lm-val">{{ label(download) }}</span>
            </div>
            <div class="lm-controls">
              <input
                type="range"
                class="lm-slider down"
                min="0"
                :max="MAX"
                step="128"
                v-model.number="download"
                data-testid="torrent-limit-download-slider"
              />
              <div class="lm-num">
                <input type="number" min="0" v-model.number="download" data-testid="torrent-limit-download-input" />
                <span class="lm-unit">{{ t('torrentLimits.unitKibps') }}</span>
              </div>
            </div>
            <button class="lm-inf" @click="download = 0">{{ t('torrentLimits.unlimited') }}</button>
          </div>

          <div class="lm-row">
            <div class="lm-row-top">
              <span class="lm-lbl up">{{ t('torrentLimits.speedUp') }}</span>
              <span class="lm-val">{{ label(upload) }}</span>
            </div>
            <div class="lm-controls">
              <input
                type="range"
                class="lm-slider up"
                min="0"
                :max="MAX"
                step="128"
                v-model.number="upload"
                data-testid="torrent-limit-upload-slider"
              />
              <div class="lm-num">
                <input type="number" min="0" v-model.number="upload" data-testid="torrent-limit-upload-input" />
                <span class="lm-unit">{{ t('torrentLimits.unitKibps') }}</span>
              </div>
            </div>
            <button class="lm-inf" @click="upload = 0">{{ t('torrentLimits.unlimited') }}</button>
          </div>

          <p class="lm-note">{{ t('torrentLimits.speedNote') }}</p>
        </div>

        <!-- ВКЛАДКА: BITTORRENT -->
        <div v-show="tab === 'bt'" class="lm-pane" data-testid="pane-bt">
          <label class="lm-chk wide">
            <input type="checkbox" v-model="dht" data-testid="torrent-dht" />
            <span>{{ t('torrentLimits.dht') }}</span>
          </label>
          <label class="lm-chk wide">
            <input type="checkbox" v-model="lsd" data-testid="torrent-lsd" />
            <span>{{ t('torrentLimits.lsd') }}</span>
          </label>
          <span class="lm-field-hint">
            {{ t('torrentLimits.btHint') }}
          </span>

          <div class="lm-tracker">
            <label class="lm-field">
              <span class="lm-field-lbl">{{ t('torrentLimits.rutrackerLabel') }}</span>
              <input
                v-model="rutrackerKey"
                class="lm-input"
                :placeholder="t('torrentLimits.rutrackerPlaceholder')"
                spellcheck="false"
                data-testid="rutracker-key-input"
              />
            </label>
            <span class="lm-field-hint">
              {{ t('torrentLimits.rutrackerHint') }}
            </span>
          </div>
        </div>
      </div>

      <footer class="lm-foot">
        <button class="lm-btn ghost" @click="emit('close')" data-testid="torrent-limits-cancel">
          {{ t('torrentLimits.cancel') }}
        </button>
        <button class="lm-btn ok" :disabled="saving" @click="save" data-testid="torrent-limits-save">
          {{ saving ? t('torrentLimits.saving') : t('torrentLimits.ok') }}
        </button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
.lm-overlay {
  position: fixed;
  inset: 0;
  z-index: 200;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(4, 7, 12, 0.78);
  backdrop-filter: blur(6px);
}
.lm-card {
  width: min(560px, 94vw);
  background: var(--bg-elevated, #0e1218);
  border: 1px solid var(--border-accent, #1e6f8f);
  border-radius: 12px;
  box-shadow: 0 24px 60px rgba(0, 0, 0, 0.6), 0 0 0 1px rgba(0, 229, 255, 0.06);
  overflow: hidden;
}
.lm-head {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px 18px;
  border-bottom: 1px solid var(--border-subtle);
}
.lm-ic {
  color: #00e5ff;
  font-size: 16px;
}
.lm-title {
  flex: 1;
  font-size: 15px;
  color: var(--text);
  letter-spacing: 0.04em;
}
.lm-x {
  color: var(--text-dim);
  font-size: 14px;
  padding: 4px 8px;
}
.lm-x:hover {
  color: var(--danger);
}
.lm-tabs {
  display: flex;
  gap: 4px;
  padding: 12px 18px 0;
  border-bottom: 1px solid var(--border-subtle);
}
.lm-tab {
  padding: 9px 16px;
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.05em;
  color: var(--text-dim);
  border: none;
  border-bottom: 2px solid transparent;
  background: transparent;
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease;
}
.lm-tab:hover {
  color: var(--text);
}
.lm-tab.active {
  color: #00e5ff;
  border-bottom-color: #00e5ff;
}
.lm-pane {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.lm-select {
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 7px 10px;
  font-size: 12px;
  color: var(--text);
  outline: none;
  cursor: pointer;
}
.lm-select:focus {
  border-color: var(--border-accent);
}
.lm-body {
  padding: 20px 18px 8px;
  display: flex;
  flex-direction: column;
  gap: 22px;
  min-height: 260px;
}
.lm-row {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.lm-row-top {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
}
.lm-lbl {
  font-size: 12px;
  letter-spacing: 0.06em;
  font-weight: 700;
}
.lm-lbl.down {
  color: #38bdf8;
}
.lm-lbl.up {
  color: #34d399;
}
.lm-val {
  font-size: 15px;
  font-weight: 800;
  color: var(--accent);
  font-variant-numeric: tabular-nums;
}
.lm-controls {
  display: flex;
  align-items: center;
  gap: 14px;
}
.lm-slider {
  flex: 1;
  -webkit-appearance: none;
  appearance: none;
  height: 5px;
  border-radius: 3px;
  background: var(--bg);
  outline: none;
}
.lm-slider.down::-webkit-slider-thumb,
.lm-slider.up::-webkit-slider-thumb {
  -webkit-appearance: none;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  cursor: pointer;
}
.lm-slider.down::-webkit-slider-thumb {
  background: #38bdf8;
  box-shadow: 0 0 8px rgba(56, 189, 248, 0.6);
}
.lm-slider.up::-webkit-slider-thumb {
  background: #34d399;
  box-shadow: 0 0 8px rgba(52, 211, 153, 0.6);
}
.lm-num {
  display: flex;
  align-items: center;
  gap: 6px;
}
.lm-num input {
  width: 92px;
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 7px 9px;
  font-size: 12px;
  color: var(--text);
  outline: none;
  font-variant-numeric: tabular-nums;
}
.lm-num input:focus {
  border-color: var(--border-accent);
}
.lm-unit {
  font-size: 10px;
  color: var(--text-dim);
}
.lm-inf {
  align-self: flex-start;
  font-size: 10px;
  letter-spacing: 0.04em;
  color: var(--text-dim);
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 3px 10px;
  cursor: pointer;
  transition: color 0.14s ease, border-color 0.14s ease;
}
.lm-inf:hover {
  color: var(--accent);
  border-color: var(--border-accent);
}
.lm-note {
  font-size: 10px;
  color: var(--text-dim);
  line-height: 1.5;
}
.lm-tracker {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding-top: 16px;
  border-top: 1px solid var(--border-subtle);
}
.lm-field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.lm-field-lbl {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--secondary);
}
.lm-input {
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 8px 10px;
  font-size: 12px;
  color: var(--text);
  outline: none;
}
.lm-input:focus {
  border-color: var(--border-accent);
}
.lm-field-hint {
  font-size: 10px;
  color: var(--text-dim);
  line-height: 1.5;
}
.lm-conn {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding-top: 16px;
  border-top: 1px solid var(--border-subtle);
}
.lm-conn-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.lm-conn-lbl {
  font-size: 12px;
  color: var(--text);
}
.lm-chk {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--text);
  cursor: pointer;
}
.lm-chk.wide span {
  line-height: 1.4;
}
.lm-chk input[type='checkbox'] {
  width: 15px;
  height: 15px;
  accent-color: #06b6d4;
  cursor: pointer;
}
.lm-num input:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}
.lm-foot {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  padding: 14px 18px;
  border-top: 1px solid var(--border-subtle);
}
.lm-btn {
  padding: 9px 22px;
  border-radius: 8px;
  font-weight: 800;
  letter-spacing: 0.06em;
  cursor: pointer;
  border: 1px solid var(--border);
}
.lm-btn.ghost {
  background: transparent;
  color: var(--text-dim);
}
.lm-btn.ghost:hover {
  color: var(--text);
  border-color: var(--border-accent);
}
.lm-btn.ok {
  background: linear-gradient(135deg, #0891b2, #06b6d4);
  color: #041016;
  border: none;
}
.lm-btn.ok:hover:not(:disabled) {
  filter: brightness(1.1);
}
.lm-btn.ok:disabled {
  opacity: 0.5;
  cursor: default;
}
</style>
