<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, computed, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { type Manifest, recentDonations, type Donation, type TopDonor } from '@/lib/bridge'
import NodePlanet from '@/components/NodePlanet.vue'
import AppEditForm from '@/components/AppEditForm.vue'
import DomainManager from '@/components/DomainManager.vue'
import { listMyDomains, verifyPeersStorage } from '@/lib/bridge'
import { useWalletStore } from '@/store/wallet'
import { useNetStore } from '@/store/net'
import { useAppsStore } from '@/store/apps'

const { t } = useI18n()
const wallet = useWalletStore()
const apps = useAppsStore()
const router = useRouter()

const props = defineProps<{
  open: boolean
  manifest: Manifest | null
  address: string
  seeds: number
  peersTotal: number
  peerIds: string[]
  peerAddrs: string[]
  transferActive: boolean
}>()
const emit = defineEmits<{ (e: 'close'): void }>()

// Storage-proof colours for the globe: peerId -> 'verified'|'partial'|'unverified'.
const peerProofs = ref<Record<string, string>>({})
let proofTimer: ReturnType<typeof setInterval> | null = null
async function refreshPeerProofs() {
  if (!props.open || !props.peerIds.length) {
    peerProofs.value = {}
    return
  }
  peerProofs.value = await verifyPeersStorage(props.peerIds)
}
watch(
  () => [props.open, props.peerIds.join(',')] as const,
  () => {
    refreshPeerProofs()
    if (props.open && !proofTimer) {
      proofTimer = setInterval(() => {
        if (useNetStore().online) refreshPeerProofs()
      }, 30000)
    } else if (!props.open && proofTimer) {
      clearInterval(proofTimer)
      proofTimer = null
    }
  },
)

const copied = ref(false)
const owner = computed(() => props.manifest?.owner || '')
// Owner-only sub-tabs inside the info panel: home (stats) | edit | domain.
type PanelTab = 'home' | 'edit' | 'domain'
const panelTab = ref<PanelTab>('home')
const activeTab = computed<PanelTab>(() => (isMine.value ? panelTab.value : 'home'))
// Soonest-expiring (≤30d) domain owned for this app — drives the warning badge.
const expiringDomain = ref<{ name: string; days: number } | null>(null)
async function loadExpiringDomain() {
  if (!isMine.value) {
    expiringDomain.value = null
    return
  }
  const mine = await listMyDomains()
  let soon: { name: string; days: number } | null = null
  for (const d of mine) {
    if (d.appId !== props.address) continue
    const days = Math.ceil((d.expires * 1000 - Date.now()) / 86400000)
    if (days <= 30 && (!soon || days < soon.days)) {
      soon = { name: `${d.name}.${d.zone}`, days }
    }
  }
  expiringDomain.value = soon
}
// reset to the stats view whenever the panel opens onto a different dApp
watch(
  () => [props.address, props.open] as const,
  () => {
    panelTab.value = 'home'
    if (props.open) loadExpiringDomain()
  },
)
const files = computed(() => props.manifest?.files || [])
const sizeBytes = computed(() => files.value.reduce((a, f) => a + (f.size || 0), 0))
function fmtSize(b: number): string {
  if (b >= 1024 * 1024) return (b / 1024 / 1024).toFixed(1) + ' МБ'
  if (b >= 1024) return (b / 1024).toFixed(0) + ' КБ'
  return b + ' B'
}

// ── Donation modal (tip the dApp owner via the shell wallet) ──────────────
const isMine = computed(() => !!owner.value && owner.value === wallet.mainAddress)
const showDonate = ref(false)
const donateAmount = ref('')
const donateMemo = computed(() => `donate:${props.address}`)
const sending = ref(false)
const panelToast = ref('')
const panelToastOk = ref(true)
const QUICK_AMOUNTS = [1, 5, 25]

// incoming donations feed + all-time totals (public chain, newest-first)
const donations = ref<Donation[]>([])
const donTotal = ref('0')
const donDonors = ref(0)
const donCount = ref(0)
const donTop = ref<TopDonor | null>(null)
const loadingDonations = ref(false)

async function loadDonations() {
  if (!owner.value) {
    donations.value = []
    donTotal.value = '0'
    donDonors.value = 0
    donCount.value = 0
    donTop.value = null
    return
  }
  loadingDonations.value = true
  try {
    const r = await recentDonations(owner.value, props.address)
    donations.value = r.items
    donTotal.value = r.totalSth
    donDonors.value = r.donors
    donCount.value = r.count
    donTop.value = r.topDonor
  } catch {
    donations.value = []
  } finally {
    loadingDonations.value = false
  }
}

function topDonorLabel(t: TopDonor): string {
  return t.name ? '@' + t.name : t.addr
}

function fmtAddr(a: string): string {
  return a.length > 14 ? `${a.slice(0, 7)}…${a.slice(-5)}` : a
}
function fmtAgo(ms: number): string {
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000))
  if (s < 60) return `${s}с`
  if (s < 3600) return `${Math.floor(s / 60)}м`
  if (s < 86400) return `${Math.floor(s / 3600)}ч`
  return `${Math.floor(s / 86400)}д`
}

// (re)load the feed whenever the panel opens onto a new dApp owner
watch(
  () => [props.open, owner.value] as const,
  ([open]) => {
    if (open) loadDonations()
  },
  { immediate: true },
)

function setQuick(v: number) {
  donateAmount.value = String(v)
}

function flashToast(msg: string, ok = true) {
  panelToast.value = msg
  panelToastOk.value = ok
  setTimeout(() => {
    if (panelToast.value === msg) panelToast.value = ''
  }, 2600)
}

function openDonate() {
  if (!owner.value) return
  donateAmount.value = ''
  showDonate.value = true
}
function closeDonate() {
  showDonate.value = false
}
async function copyOwner() {
  if (!owner.value) return
  try {
    await navigator.clipboard.writeText(owner.value)
    copied.value = true
    setTimeout(() => (copied.value = false), 1600)
  } catch {
    /* clipboard blocked */
  }
}
async function submitDonate() {
  const amt = Number(donateAmount.value)
  if (!owner.value || !(amt > 0)) return
  sending.value = true
  const ok = await wallet.sendFunds(owner.value, String(amt), donateMemo.value)
  sending.value = false
  showDonate.value = false
  if (ok) {
    flashToast(t('nodePanel.donateSent', { amount: amt }), true)
    setTimeout(loadDonations, 8000)
  } else flashToast(wallet.error || t('nodePanel.donateFailed'), false)
}

// ── App management (only for the owner of the active dApp) ─────────────────
function manageApp() {
  apps.requestEdit(props.address)
  emit('close')
  router.push('/apps')
}

function onEdited() {
  // metadata persisted in the apps store; return the owner to the stats view
  panelTab.value = 'home'
}

// ── Peers (real, from the discovery store) ────────────────────────────────
// connected = peers serving THIS dApp (seeds); available = other peers known on
// the network; mushroom not supported yet (Tor transport TBD).
const connectedPeers = computed(() => props.seeds || 0)
const availablePeers = computed(() => Math.max(0, (props.peersTotal || 0) - connectedPeers.value))
const mushroomPeers = 0
const totalPeers = computed(() => Math.max(props.peersTotal || 0, connectedPeers.value))
const peerBar = computed(() => {
  const tot = Math.max(1, totalPeers.value)
  return {
    on: (connectedPeers.value / tot) * 100,
    avail: (availablePeers.value / tot) * 100,
  }
})

// ── Files / CDN content-addressing breakdown (real, from manifest.files) ──
const CDN_CATS = [
  { key: 'scripts', label: 'Scripts', color: '#c44ad0', ext: ['js', 'mjs', 'cjs', 'ts', 'wasm'] },
  { key: 'styles', label: 'Styles', color: '#e08a2b', ext: ['css', 'scss', 'less'] },
  { key: 'fonts', label: 'Fonts', color: '#34d399', ext: ['ttf', 'otf', 'woff', 'woff2', 'eot'] },
  { key: 'media', label: 'Media', color: '#3b82f6', ext: ['png', 'jpg', 'jpeg', 'gif', 'svg', 'webp', 'ico', 'mp4', 'webm', 'mp3', 'wav', 'avif'] },
]
function extOf(path: string): string {
  const m = /\.([a-z0-9]+)$/i.exec(path)
  return m ? m[1].toLowerCase() : ''
}
const cdnBreakdown = computed(() => {
  const totals: Record<string, number> = { scripts: 0, styles: 0, fonts: 0, media: 0 }
  let other = 0
  for (const f of files.value) {
    const e = extOf(f.path)
    const cat = CDN_CATS.find((c) => c.ext.includes(e))
    if (cat) totals[cat.key] += f.size || 0
    else other += f.size || 0
  }
  const sum = sizeBytes.value || 1
  return CDN_CATS.map((c) => ({
    ...c,
    bytes: totals[c.key],
    pct: (totals[c.key] / sum) * 100,
    has: totals[c.key] > 0,
  })).concat([
    { key: 'other', label: 'Other', color: '#3a4450', ext: [], bytes: other, pct: (other / sum) * 100, has: other > 0 },
  ])
})

// ── Deduplication ratio (BLAKE3/SHA-256 content-addressing) ───────────────
// Bytes saved by storing each unique content hash once across the dApp's files.
const dedup = computed(() => {
  const seen = new Map<string, number>()
  let total = 0
  for (const f of files.value) {
    total += f.size || 0
    if (f.sha256 && !seen.has(f.sha256)) seen.set(f.sha256, f.size || 0)
  }
  let unique = 0
  for (const v of seen.values()) unique += v
  if (total <= 0) return { pct: 0, saved: 0 }
  return { pct: (1 - unique / total) * 100, saved: total - unique }
})
</script>

<template>
  <transition name="slide">
    <aside v-if="open" class="ntp" data-testid="node-telemetry-panel">
      <!-- mechanical seam: thin metallic joint line with a light-sweep on open -->
      <i class="ntp-seam" aria-hidden="true" />
      <div class="ntp-head">
        <div class="ntp-titles">
          <h2 class="ntp-name" data-testid="ntp-name">{{ manifest?.name || address }}</h2>
          <code class="ntp-addr">sth://{{ address }}</code>
          <div v-if="isMine" class="ntp-tabs" data-testid="ntp-owner-tabs">
            <button
              class="ntp-tab"
              :class="{ on: panelTab === 'home' }"
              @click="panelTab = 'home'"
              data-testid="ntp-tab-home"
              :title="t('nodePanel.tabHome')"
            >
              ⌂
            </button>
            <button
              class="ntp-tab"
              :class="{ on: panelTab === 'edit' }"
              @click="panelTab = 'edit'"
              data-testid="ntp-tab-edit"
            >
              ✎ {{ t('nodePanel.tabEdit') }}
            </button>
            <button
              class="ntp-tab"
              :class="{ on: panelTab === 'domain' }"
              @click="panelTab = 'domain'"
              data-testid="ntp-tab-domain"
            >
              .sth
            </button>
          </div>
        </div>
        <button class="ntp-close" @click="emit('close')" data-testid="ntp-close" aria-label="close">✕</button>
      </div>

      <!-- 3D planet — Stage 2 visual A (neon continents + content bars) -->
      <template v-if="activeTab === 'home'">
      <div class="ntp-planet" data-testid="ntp-planet-slot">
        <NodePlanet v-if="open" :peer-ids="peerIds" :peer-addrs="peerAddrs" :seed="address" :transfer-active="transferActive" :active="open" :proofs="peerProofs" />
      </div>
      <div v-if="peerIds.length" class="ntp-proof-legend" data-testid="ntp-proof-legend">
        <span class="ntp-pl-item"><i class="ntp-pl-dot verified"></i>{{ t('nodePanel.proofVerified') }}</span>
        <span class="ntp-pl-item"><i class="ntp-pl-dot partial"></i>{{ t('nodePanel.proofPartial') }}</span>
        <span class="ntp-pl-item"><i class="ntp-pl-dot unverified"></i>{{ t('nodePanel.proofUnverified') }}</span>
      </div>

      <!-- Domain expiry warning (owner only): yellow ≤30d, red ≤7d -->
      <button
        v-if="isMine && expiringDomain"
        class="ntp-dom-warn"
        :class="{ danger: expiringDomain.days <= 7 }"
        @click="panelTab = 'domain'"
        data-testid="ntp-domain-expiry-badge"
      >
        ⚠ {{ t('nodePanel.domExpSoon', { name: 'sth://' + expiringDomain.name.replace(/\.sth$/, ''), days: expiringDomain.days }) }}
        <span class="ntp-dom-warn-cta">{{ t('nodePanel.domRenew') }} →</span>
      </button>

      <!-- PEERS (real — from discovery store) -->
      <section class="ntp-sect">
        <span class="ntp-sect-h">{{ t('nodePanel.peers') }}</span>
        <div class="ntp-bar">
          <i class="seg c-on" :style="{ width: peerBar.on + '%' }" />
          <i class="seg c-avail" :style="{ width: peerBar.avail + '%' }" />
        </div>
        <div class="ntp-grid">
          <div class="kv"><span class="k"><i class="dot c-on" /> {{ t('nodePanel.connected') }}</span><b data-testid="ntp-connected">{{ connectedPeers }}</b></div>
          <div class="kv"><span class="k"><i class="dot c-avail" /> {{ t('nodePanel.available') }}</span><b>{{ availablePeers }}</b></div>
          <div class="kv"><span class="k"><i class="dot c-mushroom" /> {{ t('nodePanel.mushroom') }}</span><b>{{ mushroomPeers }}</b></div>
          <div class="kv"><span class="k"><i class="dot c-rest" /> {{ t('nodePanel.total') }}</span><b data-testid="ntp-total-peers">{{ totalPeers }}</b></div>
        </div>
      </section>

      <!-- DATA TRANSFER (deferred — needs iroh byte counters) -->
      <section class="ntp-sect">
        <span class="ntp-sect-h">{{ t('nodePanel.transfer') }}</span>
        <div class="ntp-grid">
          <div class="kv"><span class="k"><i class="dot c-recv" /> {{ t('nodePanel.received') }}</span><b>—</b></div>
          <div class="kv"><span class="k"><i class="dot c-sent" /> {{ t('nodePanel.sent') }}</span><b>—</b></div>
        </div>
      </section>

      <!-- FILES / CDN content-addressing breakdown (real — from manifest.files) -->
      <section class="ntp-sect">
        <span class="ntp-sect-h">{{ t('nodePanel.files') }}</span>
        <div class="ntp-bar">
          <i
            v-for="c in cdnBreakdown"
            v-show="c.has"
            :key="c.key"
            class="seg"
            :style="{ width: c.pct + '%', background: c.color }"
          />
        </div>
        <div class="ntp-grid">
          <div v-for="c in cdnBreakdown.slice(0, 4)" :key="c.key" class="kv">
            <span class="k"><i class="dot" :style="{ background: c.color }" /> {{ c.label }}</span>
            <b>{{ c.has ? fmtSize(c.bytes) : '—' }}</b>
          </div>
          <div class="kv full"><span class="k">{{ t('nodePanel.dedup') }}</span><b class="hl" data-testid="ntp-dedup">{{ dedup.pct.toFixed(dedup.pct > 0 ? 1 : 0) }} %</b></div>
          <div class="kv full"><span class="k">{{ t('nodePanel.size') }}</span><b data-testid="ntp-size">{{ fmtSize(sizeBytes) }}</b></div>
        </div>
      </section>

      <!-- DONATE — app-bound wallet (manifest owner) -->
      <section class="ntp-sect" v-if="owner">
        <span class="ntp-sect-h">{{ t('nodePanel.donate') }}</span>
        <button class="ntp-wallet ntp-wallet-btn" @click="copyOwner" data-testid="ntp-wallet" :title="t('nodePanel.copyOwner')">
          {{ copied ? t('nodePanel.copied') : owner }}
        </button>
        <div v-if="isMine" class="ntp-owner-badge" data-testid="ntp-owner-badge">
          <span class="ntp-owner-check">✓</span> {{ t('nodePanel.youOwner') }}
        </div>
        <button v-else class="ntp-donate" @click="openDonate" data-testid="ntp-donate-btn">
          ♥ {{ t('nodePanel.donate') }}
        </button>

        <!-- incoming donations feed + all-time total (social proof) -->
        <div v-if="donCount > 0" class="ntp-don-feed" data-testid="ntp-donations">
          <div class="ntp-don-total" data-testid="ntp-donations-total">
            <span class="ntp-don-total-amt">{{ donTotal }} STH</span>
            <span class="ntp-don-total-sub">{{ t('nodePanel.donTotal', { donors: donDonors }) }}</span>
          </div>

          <!-- top donor of the month (social-proof leaderboard) -->
          <div v-if="donTop" class="ntp-top-donor" data-testid="ntp-top-donor">
            <span class="ntp-top-crown">👑</span>
            <div class="ntp-top-meta">
              <span class="ntp-top-l">{{ t('nodePanel.topDonor') }}</span>
              <span class="ntp-top-name" :title="donTop.addr">{{ topDonorLabel(donTop) }}</span>
            </div>
            <span class="ntp-top-amt">{{ donTop.totalSth }} STH</span>
          </div>

          <span class="ntp-don-feed-h">{{ t('nodePanel.recentDonations') }}</span>
          <div
            v-for="(d, i) in donations.slice(0, 6)"
            :key="d.sender + d.ts + i"
            class="ntp-don-row"
            :data-testid="`ntp-donation-${i}`"
          >
            <span class="ntp-don-from">{{ fmtAddr(d.sender) }}</span>
            <span class="ntp-don-amt">+{{ d.amount }} STH</span>
            <span class="ntp-don-ago">{{ fmtAgo(d.ts) }}</span>
          </div>
        </div>
      </section>
      </template>

      <!-- EDIT tab (owner) — free metadata edit, inline (avoids the native
           webview z-order overlap of the old modal) -->
      <section v-else-if="activeTab === 'edit'" class="ntp-sect" data-testid="ntp-edit-tab">
        <AppEditForm :app-id="address" @saved="onEdited" @request-replace="manageApp" />
      </section>

      <!-- DOMAIN .sth tab (owner) — decentralized name registration -->
      <section v-else-if="activeTab === 'domain'" class="ntp-sect ntp-domain" data-testid="ntp-domain-tab">
        <DomainManager :app-id="address" />
      </section>

      <!-- DONATE MODAL (in-panel overlay) -->
      <transition name="fade">
        <div v-if="showDonate" class="ntp-modal-back" @click.self="closeDonate" data-testid="donate-modal">
          <div class="ntp-modal">
            <div class="ntp-modal-head">
              <span class="ntp-modal-title">♥ {{ t('nodePanel.donate') }}</span>
              <button class="ntp-close sm" @click="closeDonate" data-testid="donate-modal-close" aria-label="close">✕</button>
            </div>
            <p class="ntp-modal-app">// {{ manifest?.name || address }}</p>

            <div class="ntp-balance" data-testid="donate-balance">
              <span class="dim">{{ t('nodePanel.yourBalance') }}</span>
              <b>{{ wallet.mainBalance }} STH</b>
            </div>

            <label class="ntp-fld">
              <span class="ntp-fld-l">{{ t('nodePanel.amount') }} (STH)</span>
              <input
                v-model="donateAmount"
                class="ntp-input"
                type="number"
                min="0"
                step="any"
                inputmode="decimal"
                placeholder="0.0"
                data-testid="donate-amount-input"
              />
            </label>

            <div class="ntp-quick" data-testid="donate-quick">
              <button
                v-for="q in QUICK_AMOUNTS"
                :key="q"
                class="ntp-quick-btn"
                :class="{ active: Number(donateAmount) === q }"
                @click="setQuick(q)"
                :data-testid="`donate-quick-${q}`"
              >
                {{ q }} STH
              </button>
            </div>

            <div class="ntp-fld">
              <span class="ntp-fld-l">{{ t('nodePanel.memo') }}</span>
              <code class="ntp-memo-fixed" data-testid="donate-memo-fixed">{{ donateMemo }}</code>
            </div>

            <code class="ntp-modal-to" data-testid="donate-recipient">→ {{ owner }}</code>

            <div class="ntp-modal-actions">
              <button class="ntp-btn" @click="closeDonate" data-testid="donate-cancel-btn">{{ t('common.cancel') }}</button>
              <button
                class="ntp-btn primary"
                :disabled="sending || !(Number(donateAmount) > 0)"
                @click="submitDonate"
                data-testid="donate-send-btn"
              >
                {{ sending ? t('nodePanel.sending') : t('nodePanel.send') }}
              </button>
            </div>
          </div>
        </div>
      </transition>

      <!-- in-panel toast -->
      <transition name="fade">
        <div
          v-if="panelToast"
          class="ntp-toast"
          :class="{ err: !panelToastOk }"
          data-testid="donate-toast"
        >
          {{ panelToastOk ? '✓' : '⚠' }} {{ panelToast }}
        </div>
      </transition>
    </aside>
  </transition>
</template>

<style scoped>
.ntp {
  position: absolute;
  top: 0;
  right: 0;
  bottom: 0;
  width: 500px;
  max-width: 92vw;
  z-index: 30;
  overflow-y: auto;
  border-radius: 0;
  border-left: 1px solid #2c3138;
  /* Full-opaque surface — раньше градиент выглядел полупрозрачным
     поверх dApp-контента и «сквозь» карточки app details. */
  background: #0d1014;
  box-shadow: -8px 0 22px -18px rgba(0, 0, 0, 0.85);
  padding: 16px 16px 28px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  color: #c9d1d9;
}
.ntp-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 10px;
}
.ntp-name {
  font-size: 16px;
  font-weight: var(--fw-700);
  letter-spacing: 0.02em;
  color: #fff;
}
.ntp-addr {
  display: block;
  margin-top: 4px;
  font-size: 11px;
  color: var(--accent);
  word-break: break-all;
}
.ntp-close {
  background: linear-gradient(180deg, #1c2026, #14171b);
  border: 1px solid #2c3138;
  color: #8a93a0;
  width: 28px;
  height: 28px;
  border-radius: 0;
  cursor: pointer;
  flex-shrink: 0;
  transition: color 0.15s, border-color 0.15s;
}
.ntp-close:hover {
  color: #fff;
  border-color: #4a525c;
}
.ntp-proof-legend {
  display: flex;
  justify-content: center;
  gap: 16px;
  margin: 2px 0 10px;
  flex-wrap: wrap;
}
.ntp-pl-item {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 10px;
  letter-spacing: 0.04em;
  color: var(--text-dim);
}
.ntp-pl-dot {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  display: inline-block;
}
.ntp-pl-dot.verified { background: #34d399; box-shadow: 0 0 8px #34d399; }
.ntp-pl-dot.partial { background: #f59e0b; box-shadow: 0 0 8px #f59e0b; }
.ntp-pl-dot.unverified { background: #38bdf8; box-shadow: 0 0 8px #38bdf8; }

.ntp-planet {
  height: 360px;
  min-height: 250px;
  border: 1px solid #232830;
  background:
    radial-gradient(circle at 50% 45%, #141a22 0%, #0a0c0f 70%);
  display: flex;
  align-items: center;
  justify-content: center;
}
.planet-stub {
  font-size: 11px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: #4a525c;
}
.ntp-sect {
  display: flex;
  flex-direction: column;
  gap: 9px;
}
.ntp-sect-h {
  font-size: 10px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  color: #6b7480;
}
.ntp-bar {
  display: flex;
  height: 8px;
  border-radius: 0;
  overflow: hidden;
  background: #14171b;
}
.ntp-bar .seg {
  height: 100%;
}
.c-on { background: #34d399; }
.c-mushroom { background: #c44ad0; }
.c-avail { background: #3aa0d8; }
.c-rest { background: #2f3a45; }
.c-recv { background: #e2b53e; }
.c-sent { background: #34d399; }
.ntp-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 6px 16px;
}
.kv {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 12px;
}
.kv.full { grid-column: 1 / -1; }
.kv .k {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  color: #8a93a0;
}
.kv b { font-variant-numeric: tabular-nums; }
.kv b.hl { color: var(--accent); }
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  display: inline-block;
}
.ntp-wallet {
  font-size: 11px;
  color: #8a93a0;
  word-break: break-all;
  padding: 8px 10px;
  border: 1px solid #232830;
  background: #0c0e11;
}
.ntp-donate {
  margin-top: 2px;
  padding: 9px 14px;
  border-radius: 0;
  border: 1px solid #3a4450;
  background: linear-gradient(180deg, #2a3038, #1b2026);
  color: #d7dce2;
  font-size: 12px;
  letter-spacing: 0.06em;
  cursor: pointer;
  transition: border-color 0.15s, color 0.15s, background 0.15s;
}
.ntp-donate:hover {
  border-color: var(--accent);
  color: #fff;
}
/* owner sub-tabs (Home / Edit / Domain) — 3 equal-width buttons */
.ntp-tabs {
  display: flex;
  gap: 6px;
  margin-top: 8px;
}
.ntp-tab {
  flex: 1;
  padding: 7px 8px;
  border-radius: 0;
  border: 1px solid #3a4450;
  background: linear-gradient(180deg, #1c2733, #141b24);
  color: #8a96a4;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.05em;
  cursor: pointer;
  transition: border-color 0.15s, color 0.15s, background 0.15s;
  white-space: nowrap;
}
.ntp-tab:hover {
  color: #fff;
  border-color: var(--accent);
}
.ntp-tab.on {
  color: #04130d;
  background: var(--accent);
  border-color: var(--accent);
}
/* owner self badge (replaces donate button on the owner's own dApp) */
.ntp-owner-badge {
  margin-top: 2px;
  padding: 9px 14px;
  border: 1px solid rgba(127, 217, 207, 0.45);
  background: rgba(127, 217, 207, 0.1);
  color: #7fd9cf;
  font-size: 12px;
  letter-spacing: 0.05em;
  display: flex;
  align-items: center;
  gap: 8px;
}
.ntp-owner-check {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  border-radius: 999px;
  background: var(--accent);
  color: #04130d;
  font-size: 11px;
  font-weight: 900;
}
/* domain expiry warning badge (owner, under planet) */
.ntp-dom-warn {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  margin: -4px 0 6px;
  padding: 8px 11px;
  border: 1px solid rgba(231, 193, 75, 0.5);
  background: rgba(231, 193, 75, 0.1);
  color: #e7c14b;
  font-size: 11px;
  text-align: left;
  cursor: pointer;
  border-radius: 0;
  transition: background 0.15s, border-color 0.15s;
}
.ntp-dom-warn:hover {
  background: rgba(231, 193, 75, 0.18);
}
.ntp-dom-warn.danger {
  border-color: rgba(255, 106, 106, 0.6);
  background: rgba(255, 106, 106, 0.12);
  color: #ff6a6a;
}
.ntp-dom-warn-cta {
  margin-left: auto;
  font-weight: 700;
  white-space: nowrap;
}
/* .sth domain tab placeholder */
.ntp-domain-box {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 26px 14px;
  text-align: center;
}
.ntp-domain-glyph {
  font-size: 34px;
  font-weight: 900;
  letter-spacing: 0.04em;
  color: var(--accent);
}
.ntp-domain-title {
  font-size: 13px;
  color: var(--text);
  font-weight: 700;
}
.ntp-domain-sub {
  font-size: 11px;
  color: var(--text-dim);
  max-width: 240px;
}
.ntp-domain-cur {
  margin-top: 4px;
  font-size: 11px;
  color: #7fd9cf;
  border: 1px dashed #3a4450;
  padding: 5px 10px;
}
.ntp-wallet-btn {
  display: block;
  width: 100%;
  text-align: left;
  cursor: pointer;
  font-family: inherit;
  transition: border-color 0.15s, color 0.15s;
}
.ntp-wallet-btn:hover {
  border-color: var(--accent);
  color: #c9d1d9;
}
/* ── Donate modal ─────────────────────────────────────────────────────── */
.ntp-modal-back {
  position: absolute;
  inset: 0;
  z-index: 40;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 16px;
  background: rgba(6, 8, 10, 0.78);
}
.ntp-modal {
  width: 100%;
  max-width: 380px;
  border: 1px solid #2c3138;
  background: linear-gradient(180deg, #161a1f 0%, #0e1114 100%);
  box-shadow: 0 18px 50px -20px rgba(0, 0, 0, 0.9);
  padding: 18px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.ntp-modal-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.ntp-modal-title {
  font-size: 13px;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
  color: #fff;
}
.ntp-close.sm {
  width: 24px;
  height: 24px;
  font-size: 11px;
}
.ntp-modal-app {
  font-size: 11px;
  color: #6b7480;
  letter-spacing: 0.04em;
}
.ntp-balance {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 12px;
  border: 1px solid #232830;
  background: #0c0e11;
  font-size: 13px;
}
.ntp-balance .dim {
  color: #8a93a0;
  font-size: 11px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}
.ntp-balance b {
  color: var(--accent);
  font-variant-numeric: tabular-nums;
}
.ntp-fld {
  display: flex;
  flex-direction: column;
  gap: 5px;
}
.ntp-fld-l {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: #6b7480;
}
.ntp-input {
  background: #0c0e11;
  border: 1px solid #2c3138;
  border-radius: 0;
  color: #e5eaf0;
  font-size: 13px;
  padding: 9px 11px;
  font-family: inherit;
  outline: none;
  transition: border-color 0.15s;
}
.ntp-input:focus {
  border-color: var(--accent);
}
.ntp-quick {
  display: flex;
  gap: 8px;
}
.ntp-quick-btn {
  flex: 1;
  padding: 7px 0;
  border-radius: 0;
  border: 1px solid #2c3138;
  background: #0c0e11;
  color: #9aa4b0;
  font-size: 11px;
  letter-spacing: 0.04em;
  cursor: pointer;
  transition: border-color 0.15s, color 0.15s, background 0.15s;
}
.ntp-quick-btn:hover {
  border-color: #4a525c;
  color: #fff;
}
.ntp-quick-btn.active {
  border-color: var(--accent);
  color: #fff;
  background: linear-gradient(180deg, #143a32, #0e2a25);
}
.ntp-don-feed {
  display: flex;
  flex-direction: column;
  gap: 5px;
  margin-top: 4px;
  padding-top: 10px;
  border-top: 1px solid #1d2228;
}
.ntp-don-feed-h {
  font-size: 9px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: #6b7480;
  margin-bottom: 2px;
}
.ntp-don-total {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 8px 10px;
  margin-bottom: 6px;
  border: 1px solid #1d3a33;
  background: linear-gradient(180deg, #0e1f1b, #0b1714);
}
.ntp-don-total-amt {
  font-size: 16px;
  font-weight: var(--fw-700);
  color: #34d399;
  font-variant-numeric: tabular-nums;
}
.ntp-don-total-sub {
  font-size: 10px;
  color: #6b7480;
  letter-spacing: 0.04em;
}
.ntp-top-donor {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 10px;
  margin-bottom: 6px;
  border: 1px solid #4a3c12;
  background: linear-gradient(110deg, #221a08 0%, #1a1407 60%, #2a2009 100%);
  box-shadow: inset 0 0 18px -10px rgba(255, 200, 60, 0.6);
}
.ntp-top-crown {
  font-size: 16px;
  filter: drop-shadow(0 0 6px rgba(255, 200, 60, 0.7));
}
.ntp-top-meta {
  display: flex;
  flex-direction: column;
  gap: 1px;
  flex: 1;
  min-width: 0;
}
.ntp-top-l {
  font-size: 9px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: #b89544;
}
.ntp-top-name {
  font-size: 12px;
  font-weight: var(--fw-700);
  color: #ffd66b;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.ntp-top-amt {
  font-size: 13px;
  font-weight: var(--fw-700);
  color: #ffd66b;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}
.ntp-memo-fixed {
  font-size: 11px;
  color: #8a93a0;
  padding: 9px 11px;
  border: 1px solid #232830;
  background: #0c0e11;
  word-break: break-all;
}
.ntp-don-row {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 11px;
}
.ntp-don-from {
  color: #8a93a0;
  font-family: var(--font-mono, monospace);
  flex: 1;
  min-width: 0;
}
.ntp-don-amt {
  color: #34d399;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}
.ntp-don-ago {
  color: #5a626d;
  white-space: nowrap;
  min-width: 26px;
  text-align: right;
}
.ntp-modal-to {
  font-size: 10px;
  color: #6b7480;
  word-break: break-all;
}
.ntp-modal-actions {
  display: flex;
  gap: 10px;
  margin-top: 2px;
}
.ntp-btn {
  flex: 1;
  padding: 10px;
  border-radius: 0;
  border: 1px solid #3a4450;
  background: linear-gradient(180deg, #1c2026, #14171b);
  color: #c9d1d9;
  font-size: 12px;
  letter-spacing: 0.05em;
  cursor: pointer;
  transition: border-color 0.15s, color 0.15s, opacity 0.15s;
}
.ntp-btn:hover:not(:disabled) {
  border-color: #4a525c;
  color: #fff;
}
.ntp-btn.primary {
  border-color: #2f6f5f;
  background: linear-gradient(180deg, #1d4a40, #143a32);
  color: #c6f3e8;
}
.ntp-btn.primary:hover:not(:disabled) {
  border-color: var(--accent);
  color: #fff;
}
.ntp-btn:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}
.ntp-toast {
  position: absolute;
  left: 16px;
  right: 16px;
  bottom: 20px;
  z-index: 45;
  text-align: center;
  font-size: 12px;
  font-weight: var(--fw-700);
  letter-spacing: 0.03em;
  padding: 10px 14px;
  border: 1px solid #2f6f5f;
  background: rgba(20, 58, 50, 0.95);
  color: #c6f3e8;
  box-shadow: 0 10px 26px -14px rgba(0, 0, 0, 0.9);
}
.ntp-toast.err {
  border-color: #7a3030;
  background: rgba(58, 22, 22, 0.95);
  color: #f3c6c6;
}
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.18s ease;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
/* "Heavy door" mechanics: fast unlatch + weighty inertial stop (no flat slide).
   scaleX gives a pressure feel; the panel darkens on approach then settles. */
.slide-enter-active {
  transition:
    transform 0.5s cubic-bezier(0.22, 1, 0.36, 1),
    opacity 0.4s ease-out,
    filter 0.5s ease-out;
  transform-origin: right center;
}
.slide-leave-active {
  transition:
    transform 0.4s cubic-bezier(0.22, 1, 0.36, 1),
    opacity 0.3s ease-out,
    filter 0.3s ease-out;
  transform-origin: right center;
}
.slide-enter-from,
.slide-leave-to {
  transform: translateX(100%) scaleX(0.96);
  opacity: 0;
  filter: brightness(0.5);
}

/* thin steel seam at the panel's left edge + one light sweep as the door seats */
.ntp-seam {
  position: absolute;
  top: 0;
  bottom: 0;
  left: -1px;
  width: 2px;
  pointer-events: none;
  background: linear-gradient(
    180deg,
    rgba(120, 200, 190, 0) 0%,
    rgba(120, 200, 190, 0.35) 50%,
    rgba(120, 200, 190, 0) 100%
  );
  overflow: hidden;
}
.ntp-seam::after {
  content: '';
  position: absolute;
  left: 0;
  width: 100%;
  height: 38%;
  background: linear-gradient(
    180deg,
    rgba(0, 255, 200, 0) 0%,
    rgba(0, 255, 200, 0.85) 50%,
    rgba(0, 255, 200, 0) 100%
  );
  filter: blur(0.5px);
}
.slide-enter-active .ntp-seam::after {
  animation: seam-sweep 0.55s cubic-bezier(0.22, 1, 0.36, 1);
}
@keyframes seam-sweep {
  from {
    top: -40%;
    opacity: 0;
  }
  40% {
    opacity: 1;
  }
  to {
    top: 100%;
    opacity: 0;
  }
}
</style>
