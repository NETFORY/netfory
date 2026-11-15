<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, computed, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  pickFile,
  pickFolder,
  getBtTrackers,
  refreshBtTrackers,
  createTorrentFromPath,
} from '@/lib/bridge'

const emit = defineEmits<{ (e: 'close'): void; (e: 'created'): void }>()
const { t } = useI18n()

const sourcePath = ref('')
const trackersText = ref('')
const comment = ref('')
// опциональный web-seed (BEP19): HTTP(S)-зеркало с теми же файлами
const webSeed = ref('')
const pieceLength = ref<number | null>(null) // null = Авто
const isPrivate = ref(false)
const creating = ref(false)
const error = ref('')
// кнопка «Обновить трекеры»: свежий список с GitHub → в textarea и в БД
const updatingTrackers = ref(false)
const trackersMsg = ref('')
async function updateTrackers() {
  if (updatingTrackers.value) return
  updatingTrackers.value = true
  trackersMsg.value = ''
  try {
    const list = await refreshBtTrackers()
    trackersText.value = list.join('\n')
    trackersMsg.value = t('torrentCreate.trackersUpdated', { n: list.length })
  } catch (e: any) {
    trackersMsg.value = typeof e === 'string' ? e : e?.message || String(e)
  } finally {
    updatingTrackers.value = false
  }
}

// Варианты размера части (в байтах). null = Авто (2 MiB по умолчанию в движке).
const pieceOptions = computed<{ label: string; value: number | null }[]>(() => [
  { label: t('torrentCreate.pieceAuto'), value: null },
  { label: `64 ${t('torrentCreate.kibShort')}`, value: 65536 },
  { label: `128 ${t('torrentCreate.kibShort')}`, value: 131072 },
  { label: `256 ${t('torrentCreate.kibShort')}`, value: 262144 },
  { label: `512 ${t('torrentCreate.kibShort')}`, value: 524288 },
  { label: `1 ${t('torrentCreate.mibShort')}`, value: 1048576 },
  { label: `2 ${t('torrentCreate.mibShort')}`, value: 2097152 },
  { label: `4 ${t('torrentCreate.mibShort')}`, value: 4194304 },
  { label: `8 ${t('torrentCreate.mibShort')}`, value: 8388608 },
  { label: `16 ${t('torrentCreate.mibShort')}`, value: 16777216 },
])

onMounted(async () => {
  try {
    const list = await getBtTrackers()
    trackersText.value = list.join('\n')
  } catch {
    /* трекеры недоступны — оставляем пустым */
  }
})

async function chooseFile() {
  const p = await pickFile()
  if (p) sourcePath.value = p
}
async function chooseFolder() {
  const p = await pickFolder()
  if (p) sourcePath.value = p
}

async function create() {
  if (creating.value) return
  error.value = ''
  if (!sourcePath.value.trim()) {
    error.value = t('torrentCreate.errNoPath')
    return
  }
  creating.value = true
  try {
    const trackers = trackersText.value
      .split('\n')
      .map((l) => l.trim())
      .filter((l) => l.length > 0)
    await createTorrentFromPath({
      sourcePath: sourcePath.value.trim(),
      pieceLength: pieceLength.value,
      trackers,
      comment: comment.value.trim() || undefined,
      private: isPrivate.value,
      webSeeds: webSeed.value.trim() ? [webSeed.value.trim()] : [],
    })
    emit('created')
    emit('close')
  } catch (e: any) {
    error.value = e?.message || String(e) || t('torrentCreate.errGeneric')
  } finally {
    creating.value = false
  }
}
</script>

<template>
  <div class="tm-overlay" data-testid="torrent-create-modal">
    <div class="tm-card tc-card">
      <header class="tm-head">
        <span class="tm-tag">{{ t('torrentCreate.tag') }}</span>
        <h3 class="tm-title">{{ t('torrentCreate.title') }}</h3>
        <button class="tm-x" @click="emit('close')" data-testid="torrent-create-close">✕</button>
      </header>

      <div class="tc-body">
        <!-- Источник -->
        <div class="tc-section">
          <span class="tc-section-lbl">{{ t('torrentCreate.pickSection') }}</span>
          <div class="tc-path-row">
            <input
              v-model="sourcePath"
              class="tm-input tc-path"
              spellcheck="false"
              :placeholder="t('torrentCreate.pathPlaceholder')"
              data-testid="torrent-create-path"
            />
            <button class="tc-pick" @click="chooseFile" data-testid="torrent-create-pick-file">
              {{ t('torrentCreate.pickFile') }}
            </button>
            <button class="tc-pick" @click="chooseFolder" data-testid="torrent-create-pick-folder">
              {{ t('torrentCreate.pickFolder') }}
            </button>
          </div>
        </div>

        <!-- Настройки -->
        <div class="tc-section">
          <span class="tc-section-lbl">{{ t('torrentCreate.settings') }}</span>
          <label class="tm-field tc-inline">
            <span class="tm-field-lbl">{{ t('torrentCreate.pieceLen') }}</span>
            <select
              v-model="pieceLength"
              class="tm-input tc-select"
              data-testid="torrent-create-piece"
            >
              <option v-for="o in pieceOptions" :key="o.label" :value="o.value">
                {{ o.label }}
              </option>
            </select>
          </label>
          <label class="tc-check" data-testid="torrent-create-private-lbl">
            <input
              type="checkbox"
              v-model="isPrivate"
              data-testid="torrent-create-private"
            />
            <span>{{ t('torrentCreate.priv') }} <em>{{ t('torrentCreate.privHint') }}</em></span>
          </label>
          <p class="tc-hint">
            {{ t('torrentCreate.startHint') }}
          </p>
        </div>

        <!-- Поля -->
        <div class="tc-section">
          <div class="tc-lbl-row">
            <span class="tc-section-lbl">{{ t('torrentCreate.trackers') }}</span>
            <button
              class="tc-upd-btn"
              :disabled="updatingTrackers"
              @click="updateTrackers"
              data-testid="torrent-create-update-trackers"
            >{{ updatingTrackers ? t('torrentCreate.trackersUpdating') : t('torrentCreate.updateTrackers') }}</button>
          </div>
          <textarea
            v-model="trackersText"
            class="tm-input tc-area"
            spellcheck="false"
            rows="5"
            data-testid="torrent-create-trackers"
          ></textarea>
          <span v-if="trackersMsg" class="tc-trk-msg" data-testid="torrent-create-trackers-msg">{{ trackersMsg }}</span>
        </div>

        <div class="tc-section">
          <span class="tc-section-lbl">{{ t('torrentCreate.webSeed') }}</span>
          <input
            v-model="webSeed"
            class="tm-input"
            spellcheck="false"
            :placeholder="t('torrentCreate.webSeedPlaceholder')"
            data-testid="torrent-create-webseed"
          />
          <p class="tc-hint">{{ t('torrentCreate.webSeedHint') }}</p>
        </div>

        <div class="tc-section">
          <span class="tc-section-lbl">{{ t('torrentCreate.comment') }}</span>
          <input
            v-model="comment"
            class="tm-input"
            spellcheck="false"
            :placeholder="t('torrentCreate.commentPlaceholder')"
            data-testid="torrent-create-comment"
          />
        </div>

        <div v-if="error" class="tm-error tc-error" data-testid="torrent-create-error">
          <span>⚠ {{ error }}</span>
        </div>
      </div>

      <footer class="tc-foot">
        <button class="tc-cancel" @click="emit('close')" data-testid="torrent-create-cancel">
          {{ t('torrentCreate.cancel') }}
        </button>
        <button
          class="tm-start tc-create"
          :disabled="creating || !sourcePath.trim()"
          @click="create"
          data-testid="torrent-create-submit"
        >
          <span v-if="creating" class="tm-spin tc-spin">⟳</span>
          {{ creating ? t('torrentCreate.creating') : t('torrentCreate.submit') }}
        </button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
.tm-overlay {
  position: fixed;
  inset: 0;
  z-index: 200;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(4, 7, 12, 0.78);
  backdrop-filter: blur(6px);
}
.tm-card {
  display: flex;
  flex-direction: column;
  background: var(--bg-elevated, #0e1218);
  border: 1px solid var(--border-accent, #1e6f8f);
  border-radius: 12px;
  box-shadow:
    0 24px 60px rgba(0, 0, 0, 0.6),
    0 0 0 1px rgba(0, 229, 255, 0.06);
  overflow: hidden;
}
.tc-card {
  width: min(680px, 94vw);
  max-height: 90vh;
}
.tm-head {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px 18px;
  border-bottom: 1px solid var(--border-subtle);
}
.tm-tag {
  font-size: 10px;
  font-weight: 800;
  letter-spacing: 0.16em;
  color: #00e5ff;
  border: 1px solid rgba(0, 229, 255, 0.4);
  border-radius: 6px;
  padding: 3px 8px;
}
.tm-title {
  flex: 1;
  font-size: 15px;
  color: var(--text);
}
.tm-x {
  color: var(--text-dim);
  font-size: 14px;
  padding: 4px 8px;
}
.tm-x:hover {
  color: var(--danger);
}
.tc-body {
  display: flex;
  flex-direction: column;
  gap: 18px;
  padding: 18px;
  overflow-y: auto;
}
.tc-section {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.tc-section-lbl {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--secondary);
}
.tc-lbl-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.tc-upd-btn {
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--accent, #22d3ee);
  font-size: 10px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  padding: 3px 10px;
  cursor: pointer;
  transition: border-color 0.12s ease, background-color 0.12s ease;
}
.tc-upd-btn:hover { border-color: var(--accent, #22d3ee); background: rgba(34, 211, 238, 0.08); }
.tc-upd-btn:disabled { opacity: 0.55; cursor: default; }
.tc-trk-msg { font-size: 11px; color: var(--secondary); }
.tc-path-row {
  display: flex;
  gap: 8px;
}
.tm-input {
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 9px 10px;
  font-size: 12px;
  color: var(--text);
  outline: none;
  width: 100%;
  font-family: inherit;
}
.tm-input:focus {
  border-color: var(--border-accent);
}
.tc-path {
  flex: 1;
}
.tc-pick {
  flex-shrink: 0;
  padding: 0 14px;
  background: var(--panel-2, rgba(255, 255, 255, 0.05));
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--text);
  font-size: 12px;
  cursor: pointer;
  transition:
    border-color 0.14s ease,
    color 0.14s ease;
}
.tc-pick:hover {
  border-color: var(--border-accent);
  color: #00e5ff;
}
.tm-field {
  display: flex;
  gap: 6px;
}
.tc-inline {
  flex-direction: row;
  align-items: center;
  gap: 12px;
}
.tm-field-lbl {
  font-size: 12px;
  color: var(--text);
}
.tc-select {
  width: auto;
  min-width: 140px;
  cursor: pointer;
}
.tc-area {
  resize: vertical;
  line-height: 1.5;
  font-size: 11px;
}
.tc-hint {
  margin: 0;
  font-size: 11px;
  color: var(--text-dim);
  line-height: 1.4;
}
.tc-check {
  display: flex;
  align-items: center;
  gap: 9px;
  font-size: 12px;
  color: var(--text);
  cursor: pointer;
}
.tc-check em {
  color: var(--text-dim);
  font-style: normal;
  font-size: 11px;
}
.tc-error {
  padding: 10px 12px;
  border-radius: 6px;
  background: rgba(255, 80, 80, 0.08);
  color: var(--danger);
  font-size: 12px;
}
.tm-spin {
  display: inline-block;
  animation: tm-rot 1s linear infinite;
}
.tc-spin {
  margin-right: 6px;
}
@keyframes tm-rot {
  to {
    transform: rotate(360deg);
  }
}
.tc-foot {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  padding: 14px 18px;
  border-top: 1px solid var(--border-subtle);
}
.tc-cancel {
  padding: 10px 18px;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 8px;
  color: var(--text-dim);
  font-size: 12px;
  cursor: pointer;
  transition: color 0.14s ease;
}
.tc-cancel:hover {
  color: var(--text);
}
.tm-start {
  padding: 10px 18px;
  background: linear-gradient(135deg, #0891b2, #06b6d4);
  color: #041016;
  font-weight: 800;
  letter-spacing: 0.08em;
  font-size: 12px;
  border-radius: 8px;
  border: none;
  cursor: pointer;
  transition:
    filter 0.16s ease,
    transform 0.12s ease;
}
.tm-start:hover:not(:disabled) {
  filter: brightness(1.1);
}
.tm-start:active:not(:disabled) {
  transform: translateY(1px);
}
.tm-start:disabled {
  opacity: 0.5;
  cursor: default;
}
</style>
