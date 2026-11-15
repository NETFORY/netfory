<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import {
  fileMeta,
  fileSeedCount,
  checkFileAccess,
  startFileDownload,
  saveFileAs,
  buyFile,
  registerSharedFile,
  fileDownloadProgress,
  fileShareLink,
  type FileManifest,
} from '@/lib/bridge'
import { useAuthStore } from '@/store/auth'
import { fmtBytes } from '@/lib/format'
import FileQrModal from '@/components/FileQrModal.vue'

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const auth = useAuthStore()

// A share link may be self-contained:
// `sn://file/<cid>~<node>~<size>~<price>~<owner>~<name>` (works for free & paid
// without on-chain indexing). The iroh transit hash equals the cid.
const parts = computed(() => String(route.params.cid || '').split('~'))
const cid = computed(() => parts.value[0] || '')
const linkNode = computed(() => parts.value[1] || '')
const linkSize = computed(() => Number(parts.value[2] || 0) || 0)
const linkPrice = computed(() => Number(parts.value[3] || 0) || 0)
const linkOwner = computed(() => parts.value[4] || '')
const linkName = computed(() => {
  const raw = parts.value.slice(5).join('~')
  try {
    return decodeURIComponent(raw)
  } catch {
    return raw
  }
})
const hasLinkMeta = computed(() => parts.value.length > 1 && !!linkNode.value)
const meta = ref<FileManifest | null>(null)
const seeds = ref(0)
const available = ref(false)
const hasAccess = ref(false)
const loading = ref(true)

// phase: 'idle' | 'paying' | 'polling' | 'downloading' | 'ready' | 'error'
const phase = ref<'idle' | 'paying' | 'polling' | 'downloading' | 'ready' | 'error'>('idle')
const message = ref('')
let seedTimer: number | undefined
let progTimer: number | undefined
const progress = ref({ downloaded: 0, total: 0, pct: 0 })
const qrOpen = ref(false)

const isOwner = computed(() => !!meta.value && meta.value.owner === auth.address)
const anonymous = computed(() => !meta.value || !meta.value.owner)

async function refresh() {
  let m = await fileMeta(cid.value)
  // Self-contained share link (free / Private P2P mode): the file isn't known
  // locally nor on-chain, so register a remote manifest from the link payload
  // (blob + seeder node) to enable a direct P2P fetch.
  if (!m && hasLinkMeta.value && auth.isTauri) {
    try {
      // the iroh transit hash == the Blake3 cid, so blob = cid
      m = await registerSharedFile(
        cid.value,
        cid.value,
        linkNode.value,
        linkName.value,
        linkSize.value,
        linkPrice.value,
        linkOwner.value,
      )
    } catch {
      /* ignore — UI will show the unavailable state */
    }
  }
  meta.value = m
  const info = await fileSeedCount(cid.value)
  seeds.value = info.seeds
  available.value = info.available
  hasAccess.value = await checkFileAccess(cid.value)
  if (available.value) phase.value = 'ready'
  loading.value = false
}

onMounted(() => {
  refresh()
  seedTimer = window.setInterval(async () => {
    const info = await fileSeedCount(cid.value)
    seeds.value = info.seeds
  }, 8000)
})
onUnmounted(() => {
  if (seedTimer) clearInterval(seedTimer)
  if (progTimer) clearInterval(progTimer)
})

async function pollProgress() {
  const p = await fileDownloadProgress(cid.value)
  seeds.value = p.seeds
  progress.value = {
    downloaded: p.downloaded,
    total: p.total,
    pct: p.total > 0 ? Math.min(100, Math.round((p.downloaded / p.total) * 100)) : 0,
  }
  if (p.complete) {
    available.value = true
    progress.value.pct = 100
    phase.value = 'ready'
    if (progTimer) {
      clearInterval(progTimer)
      progTimer = undefined
    }
  }
}

async function doDownload() {
  phase.value = 'downloading'
  message.value = ''
  progress.value = { downloaded: 0, total: meta.value?.size || 0, pct: 0 }
  if (progTimer) clearInterval(progTimer)
  try {
    // Фоновая докачка: возвращается сразу и продолжается независимо от страницы
    // (состояние пишется в БД, файл виден в «Мои файлы» → «Скачиваются»).
    await startFileDownload(cid.value)
    progTimer = window.setInterval(pollProgress, 700)
  } catch (e: any) {
    phase.value = 'error'
    message.value = t('fx.downloadError', { err: String(e?.message || e) })
    if (progTimer) {
      clearInterval(progTimer)
      progTimer = undefined
    }
  }
}

function sleep(ms: number) {
  return new Promise((r) => setTimeout(r, ms))
}

async function payAndDownload() {
  phase.value = 'paying'
  message.value = ''
  try {
    await buyFile(cid.value)
  } catch (e: any) {
    phase.value = 'error'
    message.value = t('fx.paymentNotSent', { err: String(e?.message || e) })
    return
  }
  // polling: first check after 10s, second 8s later
  phase.value = 'polling'
  message.value = t('fx.checkingPayment')
  await sleep(10000)
  if (await checkFileAccess(cid.value)) {
    hasAccess.value = true
    return doDownload()
  }
  await sleep(8000)
  if (await checkFileAccess(cid.value)) {
    hasAccess.value = true
    return doDownload()
  }
  phase.value = 'error'
  message.value = t('fx.paymentNotFound')
}

async function save() {
  try {
    await saveFileAs(cid.value)
  } catch (e: any) {
    message.value = t('fx.saveError', { err: String(e?.message || e) })
  }
}

const canFreeDownload = computed(
  () => meta.value && (!meta.value.isPaid || isOwner.value || hasAccess.value),
)
</script>

<template>
  <div class="fd" data-testid="file-download">
    <button class="back" @click="router.back()" data-testid="fd-back">{{ t('fx.back') }}</button>

    <div v-if="loading" class="state" data-testid="fd-loading">{{ t('fx.loadingMeta') }}</div>

    <div v-else-if="!meta && !auth.isTauri" class="state" data-testid="fd-web-notice">
      {{ t('fx.downloadWebNotice') }}
    </div>

    <div v-else class="card" data-testid="fd-card">
      <div class="head">
        <span class="ext">{{ ((meta?.name || cid).split('.').pop() || 'BIN').toUpperCase() }}</span>
        <span v-if="meta?.isPaid" class="badge paid" data-testid="fd-price">{{ meta?.price }} STH</span>
        <span v-else class="badge free">FREE</span>
        <span v-if="anonymous" class="badge anon" data-testid="fd-anon">{{ t('fx.anon') }}</span>
      </div>

      <h1 class="name" data-testid="fd-name">{{ meta?.name || cid }}</h1>
      <p class="cid">{{ cid }}</p>

      <button v-if="meta" class="share-qr" @click="qrOpen = true" data-testid="fd-qr">
        {{ t('fx.shareQr') }}
      </button>

      <div class="rows">
        <div v-if="meta?.size" class="row"><span>{{ t('fx.rowSize') }}</span><b>{{ fmtBytes(meta!.size) }}</b></div>
        <div v-if="!anonymous && meta?.owner" class="row">
          <span>{{ t('fx.rowOwner') }}</span><b class="mono">{{ meta?.owner }}</b>
        </div>
        <div class="row">
          <span>{{ t('fx.rowSeeds') }}</span>
          <b data-testid="fd-seeds"><span class="seed-dot" :class="{ on: seeds > 0 }"></span> {{ seeds }}</b>
        </div>
      </div>

      <p v-if="message" class="msg" :class="{ err: phase === 'error' }" data-testid="fd-message">
        {{ message }}
      </p>

      <!-- already downloaded → save to disk -->
      <button v-if="phase === 'ready' || available" class="cta save" @click="save" data-testid="fd-save">
        {{ t('fx.saveToDisk') }}
      </button>

      <!-- in-flight -->
      <div v-else-if="phase === 'downloading'" class="dl-progress" data-testid="fd-downloading">
        <div class="dl-bar"><div class="dl-fill" :style="{ width: progress.pct + '%' }"></div></div>
        <div class="dl-meta">
          <span data-testid="fd-progress-pct">
            {{ progress.pct }}% · {{ fmtBytes(progress.downloaded) }}<template v-if="progress.total"> / {{ fmtBytes(progress.total) }}</template>
          </span>
          <span data-testid="fd-progress-seeds">
            <span class="seed-dot" :class="{ on: seeds > 0 }"></span> {{ seeds }} {{ t('fx.seedsShort') }}
          </span>
        </div>
      </div>
      <button v-else-if="phase === 'polling' || phase === 'paying'" class="cta busy" disabled data-testid="fd-polling">
        <span class="spin"></span> {{ phase === 'paying' ? t('fx.sendingPayment') : t('fx.checkingPayment') }}
      </button>

      <!-- free / owner / purchased → direct download -->
      <button v-else-if="canFreeDownload" class="cta dl" @click="doDownload" data-testid="fd-download">
        {{ t('fx.download') }}
      </button>

      <!-- paid, not purchased → pay then download -->
      <button v-else class="cta buy" @click="payAndDownload" data-testid="fd-buy">
        {{ t('fx.payAndDownload', { price: meta?.price }) }}
      </button>
    </div>

    <FileQrModal
      v-if="qrOpen && meta"
      :link="fileShareLink(meta)"
      :name="meta.name"
      @close="qrOpen = false"
    />
  </div>
</template>

<style scoped>
.fd {
  padding: 28px 32px;
  max-width: 640px;
}
.back {
  background: none;
  border: none;
  color: #64748b;
  cursor: pointer;
  font-size: 13px;
  margin-bottom: 18px;
}
.back:hover {
  color: #22d3ee;
}
.state {
  padding: 60px 0;
  text-align: center;
  color: #64748b;
}
.card {
  border: 1px solid rgba(34, 211, 238, 0.18);
  border-radius: 16px;
  background: linear-gradient(160deg, rgba(13, 20, 28, 0.92), rgba(8, 12, 18, 0.92));
  padding: 28px;
}
.head {
  display: flex;
  align-items: center;
  gap: 10px;
}
.ext {
  font-size: 12px;
  letter-spacing: 2px;
  color: #22d3ee;
  font-weight: 700;
}
.badge {
  font-size: 10px;
  padding: 3px 10px;
  border-radius: 999px;
  letter-spacing: 1px;
  font-weight: 700;
}
.badge.paid { background: rgba(217, 70, 239, 0.16); color: #e879f9; }
.badge.free { background: rgba(34, 211, 238, 0.14); color: #22d3ee; }
.badge.anon { background: rgba(148, 163, 184, 0.14); color: #94a3b8; }
.name {
  margin: 18px 0 6px;
  font-size: 22px;
  color: #e2e8f0;
  word-break: break-word;
}
.cid {
  font-family: ui-monospace, monospace;
  font-size: 11px;
  color: #475569;
  word-break: break-all;
}
.rows {
  margin: 22px 0;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.row {
  display: flex;
  justify-content: space-between;
  font-size: 13px;
  color: #94a3b8;
  border-bottom: 1px solid rgba(255, 255, 255, 0.04);
  padding-bottom: 10px;
}
.row b { color: #e2e8f0; }
.mono { font-family: ui-monospace, monospace; font-size: 11px; }
.seed-dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #475569;
  margin-right: 4px;
}
.seed-dot.on { background: #34d399; box-shadow: 0 0 8px #34d399; }
.msg { font-size: 13px; color: #94a3b8; margin: 8px 0 16px; }
.msg.err { color: #f87171; }
.cta {
  width: 100%;
  padding: 14px;
  border: none;
  border-radius: 12px;
  font-weight: 700;
  letter-spacing: 1px;
  cursor: pointer;
  font-size: 14px;
  transition: transform 0.15s, opacity 0.2s;
}
.cta:hover:not(:disabled) { transform: translateY(-1px); }
.cta.dl, .cta.save { background: linear-gradient(90deg, #22d3ee, #0891b2); color: #03121a; }
.cta.buy { background: linear-gradient(90deg, #d946ef, #a21caf); color: #fff; }
.cta.busy { background: rgba(34, 211, 238, 0.12); color: #22d3ee; cursor: wait; }
.dl-progress {
  margin-top: 4px;
}
.dl-bar {
  height: 12px;
  border-radius: 999px;
  background: rgba(34, 211, 238, 0.1);
  overflow: hidden;
}
.dl-fill {
  height: 100%;
  background: linear-gradient(90deg, #22d3ee, #0891b2);
  border-radius: 999px;
  transition: width 0.5s ease;
}
.dl-meta {
  display: flex;
  justify-content: space-between;
  margin-top: 10px;
  font-size: 12px;
  color: #94a3b8;
}
.share-qr {
  margin: 10px 0 4px;
  background: transparent;
  border: 1px solid rgba(148, 163, 184, 0.3);
  color: #94a3b8;
  border-radius: 8px;
  padding: 7px 12px;
  font-size: 12px;
  cursor: pointer;
  transition: color 0.2s, border-color 0.2s;
}
.share-qr:hover {
  color: #22d3ee;
  border-color: rgba(34, 211, 238, 0.5);
}
.spin {
  display: inline-block;
  width: 12px;
  height: 12px;
  border: 2px solid rgba(34, 211, 238, 0.3);
  border-top-color: #22d3ee;
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
  vertical-align: middle;
  margin-right: 6px;
}
@keyframes spin { to { transform: rotate(360deg); } }
</style>
