<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, computed, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  parseMagnetLink,
  startTorrentDownload,
  getDataDir,
  type TorrentMetadata,
} from '@/lib/bridge'

const { t } = useI18n()

const props = defineProps<{ magnet?: string; preloaded?: TorrentMetadata | null }>()
const emit = defineEmits<{ (e: 'close'): void; (e: 'started'): void }>()

const loading = ref(true)
const error = ref('')
const meta = ref<TorrentMetadata | null>(null)
const savePath = ref('')
const sequential = ref(false)
const starting = ref(false)
// индексы выбранных файлов (по умолчанию — все)
const selected = ref<Set<number>>(new Set())

function humanBytes(n: number): string {
  if (!n) return '0 B'
  const u = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.min(Math.floor(Math.log(n) / Math.log(1024)), u.length - 1)
  return `${(n / Math.pow(1024, i)).toFixed(i ? 1 : 0)} ${u[i]}`
}

const selectedSize = computed(() => {
  const files = meta.value?.files ?? []
  return files.reduce((s, f, i) => (selected.value.has(i) ? s + f.size : s), 0)
})

function toggle(i: number) {
  const set = new Set(selected.value)
  if (set.has(i)) set.delete(i)
  else set.add(i)
  selected.value = set
}

onMounted(async () => {
  try {
    const dir = await getDataDir()
    savePath.value = dir ? `${dir}/downloads` : 'data/downloads'
  } catch {
    savePath.value = 'data/downloads'
  }
  try {
    let m: TorrentMetadata
    if (props.preloaded) {
      m = props.preloaded
    } else if (props.magnet) {
      m = await parseMagnetLink(props.magnet)
    } else {
      throw new Error(t('torrents.noSource'))
    }
    meta.value = m
    selected.value = new Set(m.files.map((_, i) => i))
  } catch (e: any) {
    error.value = e?.message || String(e) || t('torrents.metaFailed')
  } finally {
    loading.value = false
  }
})

async function start() {
  if (!meta.value || starting.value) return
  starting.value = true
  try {
    const all = selected.value.size === meta.value.files.length
    await startTorrentDownload(
      meta.value.infoHash,
      savePath.value.trim() || null,
      sequential.value,
      all ? null : [...selected.value].sort((a, b) => a - b),
    )
    emit('started')
    emit('close')
  } catch (e: any) {
    error.value = e?.message || String(e) || t('torrents.startFailed')
  } finally {
    starting.value = false
  }
}
</script>

<template>
  <div class="tm-overlay" data-testid="torrent-magnet-modal">
    <div class="tm-card">
      <header class="tm-head">
        <span class="tm-tag">MAGNET</span>
        <h3 class="tm-title" data-testid="torrent-modal-title">
          {{ meta?.name || t('torrents.loadingMeta') }}
        </h3>
        <button class="tm-x" @click="emit('close')" data-testid="torrent-modal-close">✕</button>
      </header>

      <div v-if="loading" class="tm-loading" data-testid="torrent-modal-loading">
        <span class="tm-spin">⟳</span>
        <span>{{ t('torrents.fetchingMeta') }}</span>
      </div>

      <div v-else-if="error" class="tm-error" data-testid="torrent-modal-error">
        <span>⚠ {{ error }}</span>
      </div>

      <div v-else class="tm-body">
        <!-- Дерево файлов -->
        <div class="tm-files">
          <div class="tm-files-head">
            <span>{{ t('torrents.filesHead') }} ({{ meta?.files.length }})</span>
            <span class="tm-total">{{ humanBytes(meta?.totalBytes || 0) }}</span>
          </div>
          <ul class="tm-file-list">
            <li
              v-for="(f, i) in meta?.files"
              :key="i"
              class="tm-file"
              :data-testid="`torrent-file-${i}`"
            >
              <label class="tm-file-lbl">
                <input
                  type="checkbox"
                  :checked="selected.has(i)"
                  @change="toggle(i)"
                  :data-testid="`torrent-file-cb-${i}`"
                />
                <span class="tm-file-name" :title="f.path">{{ f.path }}</span>
              </label>
              <span class="tm-file-size">{{ humanBytes(f.size) }}</span>
            </li>
          </ul>
        </div>

        <!-- Настройки загрузки -->
        <div class="tm-settings">
          <label class="tm-field">
            <span class="tm-field-lbl">{{ t('torrents.savePath') }}</span>
            <input
              v-model="savePath"
              class="tm-input"
              spellcheck="false"
              data-testid="torrent-save-path"
            />
          </label>

          <label class="tm-check">
            <input
              type="checkbox"
              v-model="sequential"
              data-testid="torrent-sequential-cb"
            />
            <span>
              {{ t('torrents.sequential') }}
              <em>{{ t('torrents.sequentialHint') }}</em>
            </span>
          </label>

          <div class="tm-summary">
            <span>{{ t('torrents.selectedCount') }}: {{ selected.size }} / {{ meta?.files.length }}</span>
            <span class="tm-summary-size">{{ humanBytes(selectedSize) }}</span>
          </div>

          <button
            class="tm-start"
            :disabled="starting || selected.size === 0"
            @click="start"
            data-testid="torrent-start-btn"
          >
            {{ starting ? t('torrents.startingBtn') : t('torrents.startBtn') }}
          </button>
        </div>
      </div>
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
  width: min(860px, 94vw);
  max-height: 88vh;
  display: flex;
  flex-direction: column;
  background: var(--bg-elevated, #0e1218);
  border: 1px solid var(--border-accent, #1e6f8f);
  border-radius: 12px;
  box-shadow: 0 24px 60px rgba(0, 0, 0, 0.6), 0 0 0 1px rgba(0, 229, 255, 0.06);
  overflow: hidden;
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
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.tm-x {
  color: var(--text-dim);
  font-size: 14px;
  padding: 4px 8px;
}
.tm-x:hover {
  color: var(--danger);
}
.tm-loading,
.tm-error {
  padding: 48px 24px;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 16px;
  color: var(--text-dim);
  font-size: 12px;
  letter-spacing: 0.04em;
}
.tm-error {
  color: var(--danger);
}
.tm-spin {
  font-size: 28px;
  color: #00e5ff;
  animation: tm-rot 1s linear infinite;
}
@keyframes tm-rot {
  to {
    transform: rotate(360deg);
  }
}
.tm-body {
  display: grid;
  grid-template-columns: 1fr 300px;
  grid-template-rows: minmax(0, 1fr);
  gap: 0;
  flex: 1 1 auto;
  overflow: hidden;
  min-height: 0;
}
.tm-files {
  display: flex;
  flex-direction: column;
  min-height: 0;
  border-right: 1px solid var(--border-subtle);
}
.tm-files-head {
  display: flex;
  justify-content: space-between;
  padding: 12px 16px;
  font-size: 11px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--secondary);
  border-bottom: 1px solid var(--border-subtle);
}
.tm-total {
  color: var(--accent);
}
.tm-file-list {
  list-style: none;
  margin: 0;
  padding: 6px 0;
  overflow-y: auto;
}
.tm-file {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 6px 16px;
  font-size: 12px;
}
.tm-file:hover {
  background: var(--panel-2, rgba(255, 255, 255, 0.03));
}
.tm-file-lbl {
  flex: 1;
  display: flex;
  align-items: center;
  gap: 9px;
  min-width: 0;
  cursor: pointer;
}
.tm-file-name {
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.tm-file-size {
  color: var(--text-dim);
  font-variant-numeric: tabular-nums;
  flex-shrink: 0;
}
.tm-settings {
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 16px;
  min-height: 0;
  overflow-y: auto;
}
.tm-field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.tm-field-lbl {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--secondary);
}
.tm-input {
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 8px 10px;
  font-size: 11px;
  color: var(--text);
  outline: none;
}
.tm-input:focus {
  border-color: var(--border-accent);
}
.tm-check {
  display: flex;
  align-items: flex-start;
  gap: 9px;
  font-size: 12px;
  color: var(--text);
  cursor: pointer;
  line-height: 1.4;
}
.tm-check em {
  display: block;
  color: var(--text-dim);
  font-size: 10px;
  font-style: normal;
}
.tm-summary {
  display: flex;
  justify-content: space-between;
  font-size: 11px;
  color: var(--text-dim);
  padding-top: 6px;
  border-top: 1px solid var(--border-subtle);
}
.tm-summary-size {
  color: var(--accent);
  font-weight: 700;
}
.tm-start {
  margin-top: auto;
  padding: 12px;
  background: linear-gradient(135deg, #0891b2, #06b6d4);
  color: #041016;
  font-weight: 800;
  letter-spacing: 0.1em;
  border-radius: 8px;
  border: none;
  cursor: pointer;
  transition: filter 0.16s ease, transform 0.12s ease;
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
