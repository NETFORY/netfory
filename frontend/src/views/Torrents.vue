<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import {
  getActiveTorrents,
  onTorrentProgress,
  pauseTorrent,
  resumeTorrent,
  removeTorrent,
  openTorrentFolder,
  openTorrentFile,
  getTorrentMagnet,
  getTorrentFileBytes,
  saveFile,
  getTorrentDhtNodes,
  getProfileStats,
  parseTorrentPath,
  appListen,
  type TorrentMetadata,
  type TorrentStatus,
} from '@/lib/bridge'
import TorrentMagnetModal from '@/components/TorrentMagnetModal.vue'
import TorrentLimitsModal from '@/components/TorrentLimitsModal.vue'
import TorrentCreateModal from '@/components/TorrentCreateModal.vue'
import TorrentStreamModal from '@/components/TorrentStreamModal.vue'

const route = useRoute()
const router = useRouter()
const { t } = useI18n()

// Стриминг-плеер: смотреть/слушать во время скачивания.
const streamTorrent = ref<TorrentStatus | null>(null)
function openStream(t: TorrentStatus) {
  streamTorrent.value = t
}

const torrents = ref<TorrentStatus[]>([])
const magnetInput = ref('')
const modalMagnet = ref<string | null>(null)
const modalPreloaded = ref<TorrentMetadata | null>(null)
const showModal = ref(false)
const limitsOpen = ref(false)
const dhtNodes = ref(0)
const dragActive = ref(false)
let unlisten: (() => void) | null = null
let dragUnlisten: Array<() => void> = []
let dhtTimer: ReturnType<typeof setInterval> | undefined

// Фильтр записей таблицы.
type Filter = 'all' | 'downloading' | 'done' | 'paused' | 'error'
const filter = ref<Filter>('all')
const filters: { id: Filter; label: string }[] = [
  { id: 'all', label: 'All' },
  { id: 'downloading', label: 'Downloading' },
  { id: 'done', label: 'Completed' },
  { id: 'paused', label: 'Paused' },
  { id: 'error', label: 'Error' },
]
const filteredTorrents = computed(() => {
  switch (filter.value) {
    case 'downloading':
      return torrents.value.filter((t) => t.state === 'Downloading' || t.state === 'Checking')
    case 'done':
      return torrents.value.filter((t) => t.state === 'Seeding' || t.progress >= 1)
    case 'paused':
      return torrents.value.filter((t) => t.state === 'Paused')
    case 'error':
      return torrents.value.filter((t) => t.state === 'Error')
    default:
      return torrents.value
  }
})
function countFor(id: Filter): number {
  if (id === 'all') return torrents.value.length
  if (id === 'downloading')
    return torrents.value.filter((t) => t.state === 'Downloading' || t.state === 'Checking').length
  if (id === 'done')
    return torrents.value.filter((t) => t.state === 'Seeding' || t.progress >= 1).length
  if (id === 'paused') return torrents.value.filter((t) => t.state === 'Paused').length
  return torrents.value.filter((t) => t.state === 'Error').length
}

function humanBytes(n: number): string {
  if (!n) return '0 B'
  const u = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.min(Math.floor(Math.log(n) / Math.log(1024)), u.length - 1)
  return `${(n / Math.pow(1024, i)).toFixed(i ? 1 : 0)} ${u[i]}`
}
function speed(n: number): string {
  return n > 0 ? `${humanBytes(n)}/s` : '—'
}

const stateLabel: Record<string, string> = {
  Downloading: 'Downloading',
  Seeding: 'Seeding',
  Paused: 'Paused',
  Checking: 'Checking',
  Error: 'Error',
}

const totalDown = computed(() =>
  torrents.value.reduce((s, t) => s + (t.downloadSpeedBytes || 0), 0),
)
const totalUp = computed(() =>
  torrents.value.reduce((s, t) => s + (t.uploadSpeedBytes || 0), 0),
)
// Накопительная отдача ноды (из profile_stats) — не сбрасывается при
// перезапуске, в отличие от per-session счётчика librqbit. Совпадает с
// «Всего отдано» на Карте сети. Фолбэк — сумма по текущей сессии.
const nodeUploaded = ref(0)
const totalUploaded = computed(() => {
  const session = torrents.value.reduce((s, t) => s + (t.uploadedBytes || 0), 0)
  return Math.max(nodeUploaded.value, session)
})
// Накопительное скачивание ноды (из profile_stats) — не сбрасывается при
// перезапуске. Фолбэк — сумма скачанного по текущей сессии.
const nodeDownloaded = ref(0)
const totalDownloaded = computed(() => {
  const session = torrents.value.reduce((s, t) => s + (t.downloadedBytes || 0), 0)
  return Math.max(nodeDownloaded.value, session)
})

function openMagnet(m: string) {
  const v = (m || '').trim()
  if (/^magnet:\?/i.test(v)) {
    modalPreloaded.value = null
    modalMagnet.value = v
    showModal.value = true
  }
}
function openPreloaded(meta: TorrentMetadata) {
  modalMagnet.value = null
  modalPreloaded.value = meta
  showModal.value = true
}
function closeModal() {
  showModal.value = false
  modalMagnet.value = null
  modalPreloaded.value = null
}
function addFromInput() {
  if (/^magnet:\?/i.test(magnetInput.value.trim())) {
    openMagnet(magnetInput.value.trim())
    magnetInput.value = ''
  }
}
async function handleDroppedPaths(paths: string[]) {
  const torrentPath = paths.find((p) => p.toLowerCase().endsWith('.torrent'))
  if (!torrentPath) return
  try {
    const meta = await parseTorrentPath(torrentPath)
    openPreloaded(meta)
  } catch {
    /* ignore parse errors */
  }
}
// Пиковая скорость отдачи по каждому торренту (клиентский трекинг максимума).
const peakUp = ref<Record<string, number>>({})
function trackPeaks(list: TorrentStatus[]) {
  for (const t of list) {
    const cur = peakUp.value[t.infoHash] || 0
    if ((t.uploadSpeedBytes || 0) > cur) {
      peakUp.value[t.infoHash] = t.uploadSpeedBytes
    }
  }
}
async function refresh() {
  const list = await getActiveTorrents()
  trackPeaks(list)
  torrents.value = list
}
async function onStarted() {
  await refresh()
}
async function toggle(t: TorrentStatus) {
  if (t.state === 'Paused') await resumeTorrent(t.infoHash)
  else await pauseTorrent(t.infoHash)
  await refresh()
}
const deleteTarget = ref<TorrentStatus | null>(null)
const deleteWithFiles = ref(false)
const deleting = ref(false)

function askRemove(t: TorrentStatus) {
  deleteTarget.value = t
  deleteWithFiles.value = false // по умолчанию файлы НЕ трогаем
}
function cancelRemove() {
  deleteTarget.value = null
}
async function confirmRemove() {
  const t = deleteTarget.value
  if (!t || deleting.value) return
  deleting.value = true
  try {
    await removeTorrent(t.infoHash, deleteWithFiles.value)
    await refresh()
  } finally {
    deleting.value = false
    deleteTarget.value = null
  }
}

/* ---------- Контекстное меню (ПКМ по строке торрента) ---------- */
const ctxMenu = ref<{ x: number; y: number; t: TorrentStatus } | null>(null)

function openContextMenu(e: MouseEvent, t: TorrentStatus) {
  // Ограничиваем координаты, чтобы меню не вылезло за край окна.
  const x = Math.min(e.clientX, window.innerWidth - 220)
  const y = Math.min(e.clientY, window.innerHeight - 100)
  ctxMenu.value = { x, y, t }
}
function closeContextMenu() {
  ctxMenu.value = null
}

async function copyMagnet() {
  const t = ctxMenu.value?.t
  closeContextMenu()
  if (!t) return
  try {
    const magnet = await getTorrentMagnet(t.infoHash)
    if (magnet) await navigator.clipboard.writeText(magnet)
  } catch {
    /* clipboard/bridge недоступен — игнорируем */
  }
}

// Info-hash v1 (40 hex, SHA-1) — v2/hybrid раздачи librqbit-сессия не создаёт.
async function copyInfoHash() {
  const t = ctxMenu.value?.t
  closeContextMenu()
  if (!t?.infoHash) return
  try {
    await navigator.clipboard.writeText(t.infoHash)
  } catch {
    /* clipboard недоступен — игнорируем */
  }
}

async function exportTorrentFile() {
  const t = ctxMenu.value?.t
  closeContextMenu()
  if (!t) return
  try {
    const b64 = await getTorrentFileBytes(t.infoHash)
    if (!b64) return
    // Base64 → Uint8Array для нативного диалога «Сохранить как».
    const bin = atob(b64)
    const bytes = new Uint8Array(bin.length)
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
    const safeName = (t.name || t.infoHash).replace(/[/\\:*?"<>|]/g, '_')
    await saveFile(`${safeName}.torrent`, bytes, 'application/x-bittorrent')
  } catch {
    /* экспорт недоступен — метаданные ещё не загружены */
  }
}

onMounted(async () => {
  await refresh()
  unlisten = await onTorrentProgress((list) => {
    trackPeaks(list)
    torrents.value = list
  })
  const pollDht = async () => {
    try {
      dhtNodes.value = await getTorrentDhtNodes()
      const st = await getProfileStats()
      nodeUploaded.value = st.uploadedBytes
      nodeDownloaded.value = st.downloadedBytes
    } catch {
      /* noop */
    }
  }
  await pollDht()
  dhtTimer = setInterval(pollDht, 5000)
  // Tauri drag&drop: overlay + приём .torrent-файлов.
  dragUnlisten.push(await appListen('tauri://drag-enter', () => (dragActive.value = true)))
  dragUnlisten.push(await appListen('tauri://drag-over', () => (dragActive.value = true)))
  dragUnlisten.push(await appListen('tauri://drag-leave', () => (dragActive.value = false)))
  dragUnlisten.push(
    await appListen<{ paths: string[] }>('tauri://drag-drop', (p) => {
      dragActive.value = false
      if (p?.paths?.length) handleDroppedPaths(p.paths)
    }),
  )
})

// Magnet-ссылка из адресной строки (query). Реактивно — срабатывает и при
// первом заходе, и когда страница уже открыта (router меняет только query).
watch(
  () => route.query.magnet,
  (q) => {
    if (typeof q === 'string' && q) {
      openMagnet(q)
      router.replace({ name: 'torrents' })
    }
  },
  { immediate: true },
)
onBeforeUnmount(() => {
  if (unlisten) unlisten()
  if (dhtTimer) clearInterval(dhtTimer)
  dragUnlisten.forEach((u) => u())
  dragUnlisten = []
})
</script>

<template>
  <div class="torrents" data-testid="torrents-page">
    <header class="tr-header">
      <div>
        <h1 class="tr-h1">TORRENTS</h1>
        <p class="tr-sub">{{ t('torrents.subtitle') }}</p>
      </div>
      <div class="tr-header-right">
        <div class="tr-dht" data-testid="tr-dht-nodes" title="BitTorrent DHT nodes visible to the torrent client (like qBittorrent)">
          <span class="tr-dht-dot" :class="{ live: dhtNodes > 0 }"></span>
          DHT: <b>{{ dhtNodes }}</b> nodes
        </div>
        <button
          class="tr-create-btn"
          @click="createOpen = true"
          title="Create a torrent from a file or folder"
          data-testid="tr-create-btn"
        >
          <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <line x1="12" y1="5" x2="12" y2="19" />
            <line x1="5" y1="12" x2="19" y2="12" />
          </svg>
          {{ t('torrents.createTorrent') }}
        </button>
        <button
          class="tr-settings-btn"
          @click="limitsOpen = true"
          title="Torrent client settings"
          data-testid="tr-settings-btn"
        >
          <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
      </div>
    </header>

    <!-- Общие статы (клик по загрузке/отдаче → ограничения скорости) -->
    <div class="tr-stats">
      <button
        class="tr-stat clickable"
        @click="limitsOpen = true"
        title="Configure rate limits"
        data-testid="tr-stat-down"
      >
        <span class="tr-stat-lbl">{{ t('torrents.totalDownload') }} ⚙</span>
        <span class="tr-stat-val down">▼ {{ speed(totalDown) }}</span>
      </button>
      <button
        class="tr-stat clickable"
        @click="limitsOpen = true"
        title="Configure rate limits"
        data-testid="tr-stat-up"
      >
        <span class="tr-stat-lbl">{{ t('torrents.totalUpload') }} ⚙</span>
        <span class="tr-stat-val up">▲ {{ speed(totalUp) }}</span>
      </button>
      <div class="tr-stat" data-testid="tr-stat-downloaded">
        <span class="tr-stat-lbl">{{ t('torrents.totalDownloaded') }}</span>
        <span class="tr-stat-val downloaded">{{ humanBytes(totalDownloaded) }}</span>
      </div>
      <div class="tr-stat" data-testid="tr-stat-contrib">
        <span class="tr-stat-lbl">{{ t('torrents.totalUploaded') }}</span>
        <span class="tr-stat-val contrib">{{ humanBytes(totalUploaded) }}</span>
      </div>
    </div>

    <!-- Добавить магнет / .torrent -->
    <div class="tr-add">
      <input
        v-model="magnetInput"
        class="tr-add-input"
        placeholder="magnet:?xt=urn:btih:…  — paste magnet link"
        spellcheck="false"
        @keyup.enter="addFromInput"
        data-testid="torrent-magnet-input"
      />
      <button class="tr-add-btn" @click="addFromInput" data-testid="torrent-add-btn">
        + {{ t('torrents.add') }}
      </button>
      <label class="tr-drop-hint" title="Drop a .torrent file anywhere in the window">
        <span class="tr-drop-ic">⇩</span>
        <span>drop<br /><b>.torrent</b> file</span>
      </label>
    </div>

    <!-- Фильтры записей -->
    <div class="tr-filters" role="radiogroup" data-testid="torrents-filters">
      <button
        v-for="f in filters"
        :key="f.id"
        class="tr-filter"
        :class="{ active: filter === f.id }"
        role="radio"
        :aria-checked="filter === f.id"
        @click="filter = f.id"
        :data-testid="`torrents-filter-${f.id}`"
      >
        <span class="tr-radio"></span>
        {{ f.label }}
        <span class="tr-filter-count">{{ countFor(f.id) }}</span>
      </button>
    </div>

    <!-- Таблица -->
    <div class="tr-table" data-testid="torrents-table">
      <div class="tr-row tr-head-row">
        <span class="c-name">{{ t('torrents.colName') }}</span>
        <span class="c-size">{{ t('torrents.colSize') }}</span>
        <span class="c-prog">{{ t('torrents.colProgress') }}</span>
        <span class="c-state">{{ t('torrents.colStatus') }}</span>
        <span class="c-peers">Seeds/Leechers</span>
        <span class="c-speed">{{ t('torrents.colSpeed') }}</span>
        <span class="c-act"></span>
      </div>

      <div v-if="!filteredTorrents.length" class="tr-empty" data-testid="torrents-empty">
        {{
          torrents.length
            ? 'No torrents in this category.'
            : 'No active torrents. Paste a magnet link above or into the address bar.'
        }}
      </div>

      <div
        v-for="t in filteredTorrents"
        :key="t.infoHash"
        class="tr-row"
        :data-testid="`torrent-row-${t.infoHash}`"
        @contextmenu.prevent="openContextMenu($event, t)"
        @dblclick="openTorrentFile(t.infoHash)"
        title="Double-click to open the file with the system app"
      >
        <span class="c-name" :title="t.name">
          <span v-if="t.isUpdate" class="upd-badge" title="SmartNet update bundle — seeded to other nodes" data-testid="torrent-update-badge">UPDATE</span>
          {{ t.name }}
        </span>
        <span class="c-size">{{ humanBytes(t.totalBytes) }}</span>
        <span class="c-prog">
          <span class="pbar">
            <span
              class="pbar-fill"
              :class="{ done: t.progress >= 1 }"
              :style="{ width: `${Math.round((t.progress || 0) * 100)}%` }"
            ></span>
          </span>
          <span class="pbar-pct">{{ Math.round((t.progress || 0) * 100) }}%</span>
        </span>
        <span class="c-state">
          <span class="badge" :class="`st-${t.state.toLowerCase()}`">
            {{ stateLabel[t.state] || t.state }}
          </span>
        </span>
        <span class="c-peers" title="Seeds (visible in the swarm) / Leechers (connected, downloading from us)">
          <span class="seed">{{ t.seedersConnected }}</span> /
          <span class="peer">{{ t.peersConnected }}</span>
          <span
            v-if="t.liveTcp + t.liveUtp > 0"
            class="conn-badges"
            :title="`Live connections: TCP ${t.liveTcp}, µTP ${t.liveUtp}`"
          >
            <span v-if="t.liveTcp > 0" class="conn tcp">TCP {{ t.liveTcp }}</span>
            <span v-if="t.liveUtp > 0" class="conn utp">µTP {{ t.liveUtp }}</span>
          </span>
        </span>
        <span class="c-speed">
          <span class="down">▼ {{ speed(t.downloadSpeedBytes) }}</span>
          <span class="up">▲ {{ speed(t.uploadSpeedBytes) }}</span>
          <span v-if="peakUp[t.infoHash]" class="up-peak" title="Session peak upload speed">
            peak ▲ {{ speed(peakUp[t.infoHash]) }}
          </span>
        </span>
        <span class="c-act">
          <button
            class="ic play"
            title="Watch/listen while downloading (streaming)"
            @click="openStream(t)"
            :data-testid="`torrent-stream-${t.infoHash}`"
          >▶</button>
          <button
            class="ic"
            :title="t.state === 'Paused' ? 'Resume' : 'Pause'"
            @click="toggle(t)"
            :data-testid="`torrent-toggle-${t.infoHash}`"
          >{{ t.state === 'Paused' ? '▸' : '‖' }}</button>
          <button
            class="ic"
            title="Open folder"
            @click="openTorrentFolder(t.infoHash)"
            :data-testid="`torrent-folder-${t.infoHash}`"
          >📁</button>
          <button
            class="ic danger"
            title="Remove from list"
            @click="askRemove(t)"
            :data-testid="`torrent-remove-${t.infoHash}`"
          >✕</button>
        </span>
      </div>
    </div>

    <TorrentMagnetModal
      v-if="showModal"
      :magnet="modalMagnet"
      :preloaded="modalPreloaded"
      @close="closeModal"
      @started="onStarted"
    />
    <TorrentLimitsModal v-if="limitsOpen" @close="limitsOpen = false" />

    <TorrentStreamModal
      v-if="streamTorrent"
      :info-hash="streamTorrent.infoHash"
      :name="streamTorrent.name"
      @close="streamTorrent = null"
    />

    <TorrentCreateModal
      v-if="createOpen"
      @close="createOpen = false"
      @created="onStarted"
    />

    <!-- Диалог удаления торрента -->
    <div v-if="deleteTarget" class="del-overlay" data-testid="delete-torrent-modal">
      <div class="del-card">
        <header class="del-head">
          <span class="del-warn-ico">⚠</span>
          <h3 class="del-title">Delete torrent</h3>
          <button class="del-x" @click="cancelRemove" data-testid="delete-close">✕</button>
        </header>
        <div class="del-body">
          <p class="del-q">
            Are you sure you want to remove «<b>{{ deleteTarget.name }}</b>» from the torrent list?
          </p>
          <label class="del-check" data-testid="delete-with-files-label">
            <input
              type="checkbox"
              v-model="deleteWithFiles"
              data-testid="delete-with-files"
            />
            <span>Also delete the content files from disk</span>
          </label>
          <p class="del-hint">
            By default only the torrent and its swarm entry are removed — downloaded files stay on disk.
          </p>
        </div>
        <footer class="del-foot">
          <button class="del-cancel" @click="cancelRemove" data-testid="delete-cancel">
            Cancel
          </button>
          <button
            class="del-confirm"
            :class="{ danger: deleteWithFiles }"
            :disabled="deleting"
            @click="confirmRemove"
            data-testid="delete-confirm"
          >
            {{ deleting ? 'DELETING…' : 'Delete' }}
          </button>
        </footer>
      </div>
    </div>

    <!-- Контекстное меню (ПКМ по строке торрента) -->
    <template v-if="ctxMenu">
      <div class="ctx-backdrop" @click="closeContextMenu" @contextmenu.prevent="closeContextMenu"></div>
      <div
        class="tr-ctx-menu"
        :style="{ left: `${ctxMenu.x}px`, top: `${ctxMenu.y}px` }"
        data-testid="torrent-context-menu"
      >
        <button class="ctx-item" @click="copyMagnet" data-testid="ctx-copy-magnet">
          🔗 Copy magnet link
        </button>
        <button class="ctx-item" @click="copyInfoHash" data-testid="ctx-copy-infohash">
          # Copy Info-Hash v1
        </button>
        <button class="ctx-item" @click="exportTorrentFile" data-testid="ctx-export-torrent">
          💾 Export as .torrent
        </button>
      </div>
    </template>

    <!-- Оверлей drag&drop .torrent -->
    <div v-if="dragActive" class="tr-drop-overlay" data-testid="torrent-drop-overlay">
      <div class="tr-drop-box">
        <span class="tr-drop-big">⇩</span>
        <span class="tr-drop-title">Release to add</span>
        <span class="tr-drop-sub">.torrent file</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.torrents {
  width: 100%;
}
.tr-header {
  margin-bottom: 22px;
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}
.tr-header-right {
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  gap: 10px;
}
.tr-settings-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 34px;
  height: 34px;
  border-radius: 999px;
  border: 1px solid var(--border);
  color: var(--text-dim);
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease, transform 0.3s ease;
}
.tr-settings-btn:hover {
  color: #00e5ff;
  border-color: var(--border-accent);
  transform: rotate(60deg);
}
.tr-create-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 34px;
  padding: 0 14px;
  border-radius: 999px;
  border: 1px solid var(--border-accent, #1e6f8f);
  background: rgba(0, 229, 255, 0.06);
  color: #00e5ff;
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.02em;
  cursor: pointer;
  transition:
    background-color 0.15s ease,
    border-color 0.15s ease,
    transform 0.12s ease;
}
.tr-create-btn:hover {
  background: rgba(0, 229, 255, 0.14);
  border-color: #00e5ff;
}
.tr-create-btn:active {
  transform: translateY(1px);
}

.tr-dht {
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--text-dim);
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 7px 14px;
  letter-spacing: 0.04em;
}
.conn-badges {
  display: inline-flex;
  gap: 5px;
  margin-left: 8px;
}
.conn {
  font-size: 9px;
  font-weight: 700;
  letter-spacing: 0.04em;
  padding: 1px 6px;
  border-radius: 4px;
  font-variant-numeric: tabular-nums;
}
.conn.tcp {
  color: #38bdf8;
  background: rgba(56, 189, 248, 0.12);
  border: 1px solid rgba(56, 189, 248, 0.4);
}
.conn.utp {
  color: #34d399;
  background: rgba(52, 211, 153, 0.12);
  border: 1px solid rgba(52, 211, 153, 0.4);
}
.tr-dht b {
  color: #f59e0b;
  font-variant-numeric: tabular-nums;
}
.tr-dht-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #64748b;
}
.tr-dht-dot.live {
  background: #f59e0b;
  box-shadow: 0 0 7px rgba(245, 158, 11, 0.7);
}
.tr-h1 {
  font-size: 26px;
  font-weight: 800;
  letter-spacing: 0.12em;
  color: var(--text);
}
.tr-sub {
  margin-top: 4px;
  font-size: 12px;
  color: var(--text-dim);
}
.tr-stats {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 14px;
  margin-bottom: 20px;
}
.tr-stat {
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 16px 18px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.tr-stat-lbl {
  font-size: 10px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--secondary);
}
.tr-stat-val {
  font-size: 22px;
  font-weight: 800;
  font-variant-numeric: tabular-nums;
}
.tr-stat-val.down {
  color: #38bdf8;
}
.tr-stat-val.up {
  color: #34d399;
}
.tr-stat-val.contrib {
  color: #a855f7;
}
.tr-stat-val.downloaded {
  color: #38bdf8;
}
.upd-badge {
  display: inline-block;
  margin-right: 8px;
  padding: 2px 7px;
  border-radius: 5px;
  font-size: 9px;
  font-weight: 800;
  letter-spacing: 0.08em;
  vertical-align: middle;
  color: #052e2a;
  background: linear-gradient(135deg, #34d399, #2ee3c6);
  box-shadow: 0 0 0 1px rgba(46, 227, 198, 0.35);
}
.tr-stat.clickable {
  cursor: pointer;
  text-align: left;
  transition: border-color 0.16s ease, box-shadow 0.16s ease;
}
.tr-stat.clickable:hover {
  border-color: var(--border-accent);
  box-shadow: 0 0 0 1px rgba(0, 229, 255, 0.15);
}
.tr-filters {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 14px;
}
.tr-filter {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 7px 13px;
  font-size: 11px;
  letter-spacing: 0.04em;
  color: var(--text-dim);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 999px;
  cursor: pointer;
  transition: color 0.14s ease, border-color 0.14s ease;
}
.tr-filter:hover {
  color: var(--text);
}
.tr-filter.active {
  color: #06b6d4;
  border-color: #06b6d4;
}
.tr-radio {
  width: 11px;
  height: 11px;
  border-radius: 50%;
  border: 1.5px solid currentColor;
  position: relative;
}
.tr-filter.active .tr-radio::after {
  content: '';
  position: absolute;
  inset: 2px;
  border-radius: 50%;
  background: #06b6d4;
}
.tr-filter-count {
  font-size: 10px;
  font-variant-numeric: tabular-nums;
  color: var(--secondary);
  background: var(--bg);
  border-radius: 999px;
  padding: 1px 7px;
}
.tr-filter.active .tr-filter-count {
  color: #06b6d4;
}
.tr-add {
  display: flex;
  gap: 10px;
  margin-bottom: 18px;
}
.tr-add-input {
  flex: 1;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 11px 14px;
  font-size: 12px;
  color: var(--text);
  outline: none;
}
.tr-add-input:focus {
  border-color: var(--border-accent);
}
.tr-add-btn {
  padding: 0 20px;
  background: linear-gradient(135deg, #0891b2, #06b6d4);
  color: #041016;
  font-weight: 800;
  letter-spacing: 0.08em;
  border-radius: 8px;
  border: none;
  cursor: pointer;
  transition: filter 0.16s ease;
}
.tr-add-btn:hover {
  filter: brightness(1.12);
}
.tr-drop-hint {
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  gap: 9px;
  padding: 8px 14px;
  font-size: 10px;
  line-height: 1.2;
  letter-spacing: 0.04em;
  color: var(--text-dim);
  border: 1px dashed var(--border);
  border-radius: 8px;
}
.tr-drop-hint b {
  color: var(--accent);
}
.tr-drop-ic {
  font-size: 18px;
  color: #06b6d4;
}
.tr-drop-overlay {
  position: fixed;
  inset: 0;
  z-index: 250;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(4, 12, 18, 0.82);
  backdrop-filter: blur(4px);
  pointer-events: none;
}
.tr-drop-box {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 56px 80px;
  border: 2px dashed #06b6d4;
  border-radius: 18px;
  background: rgba(6, 182, 212, 0.06);
  box-shadow: 0 0 40px rgba(6, 182, 212, 0.25);
}
.tr-drop-big {
  font-size: 56px;
  color: #06b6d4;
  animation: tr-bounce 1.1s ease-in-out infinite;
}
@keyframes tr-bounce {
  0%, 100% {
    transform: translateY(0);
  }
  50% {
    transform: translateY(8px);
  }
}
.tr-drop-title {
  font-size: 18px;
  font-weight: 800;
  letter-spacing: 0.08em;
  color: var(--text);
}
.tr-drop-sub {
  font-size: 13px;
  color: #06b6d4;
  font-weight: 700;
  letter-spacing: 0.14em;
}
.tr-table {
  border: 1px solid var(--border);
  border-radius: 10px;
  overflow: hidden;
  background: var(--bg-elevated);
}
.tr-row {
  display: grid;
  grid-template-columns: 2.4fr 0.8fr 1.6fr 1fr 0.9fr 1.4fr 1fr;
  align-items: center;
  gap: 10px;
  padding: 11px 16px;
  font-size: 12px;
  border-bottom: 1px solid var(--border-subtle);
}
.tr-row:last-child {
  border-bottom: none;
}
.tr-head-row {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--secondary);
  background: var(--bg);
}
.tr-empty {
  padding: 40px 16px;
  text-align: center;
  color: var(--text-dim);
  font-size: 12px;
}
.c-name {
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.c-size {
  color: var(--text-dim);
  font-variant-numeric: tabular-nums;
}
.c-prog {
  display: flex;
  align-items: center;
  gap: 8px;
}
.pbar {
  flex: 1;
  height: 7px;
  background: var(--bg);
  border-radius: 4px;
  overflow: hidden;
}
.pbar-fill {
  display: block;
  height: 100%;
  background: linear-gradient(90deg, #0891b2, #38bdf8);
  transition: width 0.4s ease;
}
.pbar-fill.done {
  background: linear-gradient(90deg, #059669, #34d399);
}
.pbar-pct {
  font-size: 10px;
  color: var(--text-dim);
  min-width: 34px;
  text-align: right;
  font-variant-numeric: tabular-nums;
}
.badge {
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.05em;
  padding: 3px 8px;
  border-radius: 999px;
  border: 1px solid currentColor;
}
.st-downloading {
  color: #38bdf8;
}
.st-seeding {
  color: #34d399;
}
.st-paused {
  color: #94a3b8;
}
.st-checking {
  color: #fbbf24;
}
.st-error {
  color: #f87171;
}
.c-peers .seed {
  color: #a855f7;
  font-weight: 700;
}
.c-peers .peer {
  color: var(--text-dim);
}
.c-speed {
  display: flex;
  flex-direction: column;
  gap: 2px;
  font-variant-numeric: tabular-nums;
}
.c-speed .down {
  color: #38bdf8;
}
.c-speed .up {
  color: #34d399;
}
.c-speed .up-peak {
  color: var(--text-dim);
  font-size: 10px;
  opacity: 0.75;
}
.c-act {
  display: flex;
  gap: 6px;
  justify-content: flex-end;
}
.ic {
  color: var(--text-dim);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 3px 8px;
  font-size: 12px;
  cursor: pointer;
  transition: color 0.14s ease, border-color 0.14s ease;
}
.ic:hover {
  color: var(--accent);
  border-color: var(--border-accent);
}
.ic.danger:hover {
  color: var(--danger);
  border-color: var(--danger);
}
.ic.play:hover {
  color: var(--accent);
  border-color: var(--accent);
}

/* --- Контекстное меню --- */
.ctx-backdrop {
  position: fixed;
  inset: 0;
  z-index: 90;
}
.tr-ctx-menu {
  position: fixed;
  z-index: 91;
  min-width: 220px;
  padding: 6px;
  background: var(--panel, #16181d);
  border: 1px solid var(--border, #2a2d34);
  border-radius: 10px;
  box-shadow: 0 12px 32px rgba(0, 0, 0, 0.45);
  display: flex;
  flex-direction: column;
  gap: 2px;
  animation: ctx-pop 0.12s ease;
}
@keyframes ctx-pop {
  from {
    opacity: 0;
    transform: translateY(-4px) scale(0.98);
  }
  to {
    opacity: 1;
    transform: none;
  }
}
.ctx-item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 9px 12px;
  background: transparent;
  border: none;
  border-radius: 7px;
  color: var(--text, #e6e8ec);
  font-size: 13px;
  text-align: left;
  cursor: pointer;
  transition:
    background-color 0.12s ease,
    color 0.12s ease;
}
.ctx-item:hover {
  background: var(--accent-soft, rgba(90, 130, 255, 0.14));
  color: var(--accent, #6ea0ff);
}

/* ---------- Диалог удаления торрента ---------- */
.del-overlay {
  position: fixed;
  inset: 0;
  z-index: 120;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  background: rgba(6, 8, 12, 0.62);
  backdrop-filter: blur(6px);
  animation: del-fade 0.16s ease;
}
@keyframes del-fade {
  from {
    opacity: 0;
  }
  to {
    opacity: 1;
  }
}
.del-card {
  width: 100%;
  max-width: 460px;
  background: var(--panel, #16181d);
  border: 1px solid var(--border, #2a2d34);
  border-radius: 14px;
  box-shadow: 0 24px 60px rgba(0, 0, 0, 0.55);
  overflow: hidden;
  animation: del-pop 0.18s cubic-bezier(0.16, 1, 0.3, 1);
}
@keyframes del-pop {
  from {
    opacity: 0;
    transform: translateY(10px) scale(0.97);
  }
  to {
    opacity: 1;
    transform: none;
  }
}
.del-head {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 16px 18px;
  border-bottom: 1px solid var(--border, #2a2d34);
}
.del-warn-ico {
  font-size: 18px;
  color: #fbbf24;
  line-height: 1;
}
.del-title {
  flex: 1;
  margin: 0;
  font-size: 16px;
  font-weight: 700;
  color: var(--text, #e6e8ec);
}
.del-x {
  width: 28px;
  height: 28px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: transparent;
  border: none;
  border-radius: 8px;
  color: var(--secondary, #8b8f99);
  font-size: 14px;
  cursor: pointer;
  transition: background-color 0.14s ease, color 0.14s ease;
}
.del-x:hover {
  background: rgba(255, 255, 255, 0.06);
  color: var(--text, #e6e8ec);
}
.del-body {
  padding: 18px;
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.del-q {
  margin: 0;
  font-size: 14px;
  line-height: 1.5;
  color: var(--text, #e6e8ec);
}
.del-q b {
  color: #fff;
  word-break: break-word;
}
.del-check {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 12px;
  border: 1px solid var(--border, #2a2d34);
  border-radius: 10px;
  cursor: pointer;
  font-size: 13px;
  color: var(--text, #e6e8ec);
  transition: border-color 0.14s ease, background-color 0.14s ease;
}
.del-check:hover {
  border-color: rgba(239, 68, 68, 0.5);
  background: rgba(239, 68, 68, 0.06);
}
.del-check input {
  margin-top: 1px;
  width: 16px;
  height: 16px;
  accent-color: #ef4444;
  cursor: pointer;
}
.del-hint {
  margin: 0;
  font-size: 12px;
  line-height: 1.45;
  color: var(--secondary, #8b8f99);
}
.del-foot {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  padding: 14px 18px;
  border-top: 1px solid var(--border, #2a2d34);
  background: rgba(255, 255, 255, 0.015);
}
.del-cancel,
.del-confirm {
  padding: 9px 18px;
  border-radius: 9px;
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  transition: background-color 0.14s ease, border-color 0.14s ease, opacity 0.14s ease;
}
.del-cancel {
  background: transparent;
  border: 1px solid var(--border, #2a2d34);
  color: var(--text, #e6e8ec);
}
.del-cancel:hover {
  border-color: var(--secondary, #8b8f99);
}
.del-confirm {
  background: #ef4444;
  border: 1px solid #ef4444;
  color: #fff;
}
.del-confirm:hover {
  background: #dc2626;
}
.del-confirm.danger {
  background: #b91c1c;
  border-color: #b91c1c;
}
.del-confirm:disabled {
  opacity: 0.6;
  cursor: default;
}
</style>
