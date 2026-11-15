<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'

const { t } = useI18n()
import { listExchangeFiles, fileShareLink, type FileManifest } from '@/lib/bridge'
import { useAuthStore } from '@/store/auth'
import { canSeeFileExchange } from '@/lib/access'
import { fmtBytes, fmtAgo } from '@/lib/format'
import FileQrModal from '@/components/FileQrModal.vue'
import FileTypeIcon from '@/components/FileTypeIcon.vue'

const router = useRouter()
const auth = useAuthStore()
const files = ref<FileManifest[]>([])
const sort = ref<'new' | 'paid' | 'free'>('new')
const copied = ref('')
const qrFile = ref<FileManifest | null>(null)
let timer: number | undefined

async function load() {
  files.value = await listExchangeFiles()
}

onMounted(() => {
  // page-visibility gate: non-allowed users are bounced to their own files
  if (!canSeeFileExchange(auth.address)) {
    router.replace({ name: 'myfiles' })
    return
  }
  load()
  // live refresh so new files from the network/local add appear without reload
  timer = window.setInterval(() => {
    if (!document.hidden) load()
  }, 5000)
})
onUnmounted(() => timer && clearInterval(timer))

const view = computed(() => {
  // hide unavailable files (no live seeders) — they can't be downloaded
  let list = files.value.filter((f) => (f.seeds ?? 0) > 0)
  if (sort.value === 'paid') list = list.filter((f) => f.isPaid)
  else if (sort.value === 'free') list = list.filter((f) => !f.isPaid)
  return list.sort((a, b) => b.created - a.created)
})

async function copyLink(f: FileManifest) {
  try {
    await navigator.clipboard.writeText(fileShareLink(f))
    copied.value = f.cid
    setTimeout(() => (copied.value = ''), 1500)
  } catch {
    /* ignore */
  }
}

function open(f: FileManifest) {
  router.push({ name: 'file', params: { cid: f.cid } })
}
</script>

<template>
  <div class="fx" data-testid="file-exchange">
    <header class="fx-head">
      <div>
        <h1>FILE EXCHANGE</h1>
        <p class="sub">{{ t('fx.sub') }}</p>
      </div>
    </header>

    <div class="tabs" data-testid="fx-sort-tabs">
      <button :class="{ on: sort === 'new' }" @click="sort = 'new'" data-testid="fx-sort-new">
        {{ t('fx.filterNew') }}
      </button>
      <button :class="{ on: sort === 'paid' }" @click="sort = 'paid'" data-testid="fx-sort-paid">
        {{ t('fx.filterPaid') }}
      </button>
      <button :class="{ on: sort === 'free' }" @click="sort = 'free'" data-testid="fx-sort-free">
        {{ t('fx.filterFree') }}
      </button>
    </div>

    <div v-if="!view.length" class="empty" data-testid="fx-empty">
      <span class="empty-ico">⌹</span>
      <p>{{ t('fx.emptyTitle') }}</p>
      <p class="muted">{{ t('fx.emptyHint') }}</p>
    </div>

    <div v-else class="grid" data-testid="fx-grid">
      <article
        v-for="f in view"
        :key="f.cid"
        class="card"
        :data-testid="`fx-card-${f.cid}`"
      >
        <div class="card-top">
          <FileTypeIcon :name="f.name" />
          <span class="ftype">{{ (f.name.split('.').pop() || 'BIN').toUpperCase() }}</span>
          <span v-if="f.isPaid" class="badge paid">{{ f.price }} STH</span>
          <span v-else class="badge free">FREE</span>
        </div>
        <h3 class="fname" :title="f.name">{{ f.name }}</h3>
        <div class="meta">
          <span>{{ fmtBytes(f.size) }}</span>
          <span class="dot">·</span>
          <span>{{ fmtAgo(f.created) }}</span>
        </div>
        <div class="seed-row">
          <span class="seed-dot" :class="{ on: f.seeding }"></span>
          <span class="seed-label">{{ f.seeding ? t('fx.seeding') : t('fx.notSeeding') }}</span>
          <span v-if="f.seeds" class="seed-count" :data-testid="`fx-seeds-${f.cid}`">
            · {{ f.seeds }} {{ t('fx.seedsShort') }}
          </span>
        </div>
        <div class="card-actions">
          <button class="btn ghost" @click="copyLink(f)" :data-testid="`fx-copy-${f.cid}`">
            {{ copied === f.cid ? t('fx.copied') : t('fx.snLink') }}
          </button>
          <button class="btn ghost icon" @click="qrFile = f" :data-testid="`fx-qr-${f.cid}`" :title="t('fx.qrTitle')">
            ⊞
          </button>
          <button class="btn primary" @click="open(f)" :data-testid="`fx-open-${f.cid}`">
            {{ t('fx.open') }}
          </button>
        </div>
      </article>
    </div>

    <FileQrModal
      v-if="qrFile"
      :link="fileShareLink(qrFile)"
      :name="qrFile.name"
      @close="qrFile = null"
    />
  </div>
</template>

<style scoped>
.fx {
  padding: 28px 32px;
  max-width: 1200px;
}
.fx-head h1 {
  font-size: 28px;
  letter-spacing: 3px;
  color: #e2e8f0;
}
.sub {
  margin-top: 6px;
  color: #64748b;
  font-size: 13px;
  letter-spacing: 1px;
}
.tabs {
  display: flex;
  gap: 8px;
  margin: 26px 0 22px;
}
.tabs button {
  background: rgba(15, 23, 32, 0.6);
  border: 1px solid rgba(34, 211, 238, 0.15);
  color: #94a3b8;
  padding: 8px 18px;
  border-radius: 999px;
  cursor: pointer;
  font-size: 12px;
  letter-spacing: 1px;
  transition: color 0.2s, border-color 0.2s, background-color 0.2s;
}
.tabs button.on {
  color: #03121a;
  background: #22d3ee;
  border-color: #22d3ee;
  font-weight: 700;
}
.grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
  gap: 18px;
}
.card {
  border: 1px solid rgba(34, 211, 238, 0.16);
  border-radius: 14px;
  background: linear-gradient(160deg, rgba(13, 20, 28, 0.9), rgba(8, 12, 18, 0.9));
  padding: 18px;
  transition: transform 0.2s, border-color 0.2s, box-shadow 0.2s;
}
.card:hover {
  transform: translateY(-3px);
  border-color: rgba(34, 211, 238, 0.5);
  box-shadow: 0 12px 40px -16px rgba(34, 211, 238, 0.4);
}
.card-top {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
}
.ftype {
  font-size: 11px;
  letter-spacing: 2px;
  color: #22d3ee;
  font-weight: 700;
  margin-right: auto;
}
.badge {
  font-size: 10px;
  padding: 3px 9px;
  border-radius: 999px;
  letter-spacing: 1px;
  font-weight: 700;
}
.badge.paid {
  background: rgba(217, 70, 239, 0.16);
  color: #e879f9;
}
.badge.free {
  background: rgba(34, 211, 238, 0.14);
  color: #22d3ee;
}
.fname {
  margin: 14px 0 8px;
  font-size: 15px;
  color: #e2e8f0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.meta {
  font-size: 12px;
  color: #64748b;
  display: flex;
  gap: 6px;
}
.dot {
  opacity: 0.5;
}
.seed-row {
  display: flex;
  align-items: center;
  gap: 7px;
  margin: 12px 0;
}
.seed-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #475569;
}
.seed-dot.on {
  background: #34d399;
  box-shadow: 0 0 8px #34d399;
}
.seed-label {
  font-size: 11px;
  color: #94a3b8;
}
.seed-count {
  font-size: 11px;
  color: #34d399;
}
.btn.icon {
  flex: 0 0 auto;
  padding: 8px 12px;
  font-size: 14px;
}
.card-actions {
  display: flex;
  gap: 8px;
  margin-top: 6px;
}
.btn {
  flex: 1;
  padding: 8px;
  border-radius: 8px;
  font-size: 12px;
  cursor: pointer;
  border: 1px solid transparent;
  transition: background-color 0.2s, color 0.2s;
}
.btn.ghost {
  background: transparent;
  border-color: rgba(34, 211, 238, 0.3);
  color: #94a3b8;
}
.btn.ghost:hover {
  color: #22d3ee;
}
.btn.primary {
  background: #22d3ee;
  color: #03121a;
  font-weight: 700;
}
.empty {
  text-align: center;
  padding: 80px 0;
  color: #64748b;
}
.empty-ico {
  font-size: 48px;
  color: rgba(34, 211, 238, 0.3);
}
.empty p {
  margin-top: 12px;
}
.muted {
  font-size: 12px;
  color: #475569;
}
</style>
