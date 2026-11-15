<script setup lang="ts">
/*
 * SmartNet Wallet // DashboardView
 *
 *   Главная кошелька в стиле браузерного расширения SmartHoldem: строка
 *   аккаунта, карточка баланса со штриховкой, спарклайн курса за 7 дней
 *   (сэмплы XBTS-пула из Rust `get_rate_history`), тап по балансу
 *   STH ⇄ USDT ⇄ скрыт, цветные быстрые действия, pending-карточки с кольцом
 *   до подтверждения, токены SHIP-14 и последние операции.
 */
import { computed, ref, nextTick, watch, onBeforeUnmount } from 'vue'
import { useI18n } from 'vue-i18n'
import { useWalletStore } from '@/store/wallet'
import { shortAddr } from '@/lib/crypto'
import { openExplorerTx, openExplorerWallet } from '@/lib/explorer'

const { t, locale } = useI18n()
const wallet = useWalletStore()

const openTx = openExplorerTx

async function copyAddr() {
  const addr = wallet.address
  if (!addr) return
  try {
    await navigator.clipboard.writeText(addr)
  } catch {
    const ta = document.createElement('textarea')
    ta.value = addr
    document.body.appendChild(ta)
    ta.select()
    document.execCommand('copy')
    document.body.removeChild(ta)
  }
  wallet.flash(t('wallet.receive.copied'))
}

/* ---- BIP-44 аккаунты: выпадающий список под строкой аккаунта ---- */
const accOpen = ref(false)
const accBusy = ref(false)
const editIdx = ref<number | null>(null)
const editLabel = ref('')
const activeAcc = computed(() => wallet.activeAccount)
const accNumber = computed(() => (activeAcc.value?.index ?? 0) + 1)
const accLabel = computed(() => activeAcc.value?.label ?? t('wallet.accountN', { n: 1 }))
async function toggleAccounts() {
  accOpen.value = !accOpen.value
  if (accOpen.value && !wallet.accounts.length) void wallet.loadAccounts()
}
function pickAccount(i: number) {
  accOpen.value = false
  void wallet.switchAccount(i)
}
async function addAccount() {
  if (accBusy.value) return
  accBusy.value = true
  try {
    const a = await wallet.addAccount()
    startEdit(a.index, a.label)
  } catch (e: any) {
    wallet.error = typeof e === 'string' ? e : e?.message || String(e)
  } finally {
    accBusy.value = false
  }
}
function startEdit(i: number, label: string) {
  editIdx.value = i
  editLabel.value = label
  nextTick(() => (document.querySelector<HTMLInputElement>('.acc-edit'))?.select())
}
async function commitEdit() {
  if (editIdx.value === null) return
  const i = editIdx.value
  editIdx.value = null
  await wallet.renameAccount(i, editLabel.value)
}
function fmtAcc(b: string) {
  if (!b) return '…'
  const n = Number(b)
  if (!Number.isFinite(n)) return b
  return n.toLocaleString('ru-RU', { minimumFractionDigits: 2, maximumFractionDigits: n < 1 ? 8 : 4 }).replace(/,/g, '.').replace(/\s/g, ' ')
}

/* ---- баланс: STH / USDT / скрыт ---- */
const balanceParts = computed(() => {
  const [int, frac = ''] = String(wallet.balance).split('.')
  return { int: Number(int).toLocaleString('ru-RU').replace(/,/g, ' '), frac }
})
const fiatParts = computed(() => {
  const [int, frac = ''] = Number(wallet.fiat).toFixed(2).split('.')
  return { int: Number(int).toLocaleString('ru-RU').replace(/,/g, ' '), frac }
})
const rateStr = computed(() => (wallet.rate > 0 ? `1 STH = $${wallet.rate.toFixed(4)}` : ''))

/* ---- спарклайн 7д ---- */
const spark = computed(() => {
  const h = wallet.rateHistory
  if (h.length < 2) return null
  const W = 100
  const H = 28
  const min = Math.min(...h.map((p) => p.r))
  const max = Math.max(...h.map((p) => p.r))
  const span = max - min || 1
  const pts = h.map((p, i) => {
    const x = (i / (h.length - 1)) * W
    const y = H - 2 - ((p.r - min) / span) * (H - 4)
    return [x, y] as const
  })
  const line = pts.map(([x, y], i) => `${i ? 'L' : 'M'}${x.toFixed(2)},${y.toFixed(2)}`).join(' ')
  const area = `${line} L${W},${H} L0,${H} Z`
  const first = h[0].r
  const last = h[h.length - 1].r
  const change = first > 0 ? ((last - first) / first) * 100 : 0
  return { line, area, change, up: change >= 0, lastX: pts[pts.length - 1][0], lastY: pts[pts.length - 1][1] }
})

/* ---- быстрые действия ---- */
const quick = [
  { id: 'send', path: 'M5 12h14M13 6l6 6-6 6', label: () => t('wallet.quick.send'), tone: 'accent' },
  { id: 'receive', path: 'M19 12H5M11 18l-6-6 6-6', label: () => t('wallet.quick.receive'), tone: 'secondary' },
  { id: 'swap', path: 'M16 3h5v5M21 3l-7 7M8 21H3v-5M3 21l7-7', label: () => t('wallet.quick.swap'), tone: 'accent' },
  { id: 'history', path: 'M12 8v4l3 2M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z', label: () => t('wallet.quick.history'), tone: 'plain' },
] as const

function quickDisabled(id: string) {
  return wallet.viewOnly && (id === 'send' || id === 'swap')
}

/* ---- время ---- */
const rtf = computed(() => new Intl.RelativeTimeFormat(locale.value, { numeric: 'always', style: 'narrow' }))
function timeAgo(ts: number): string {
  const s = Math.round((ts - Date.now()) / 1000)
  const a = Math.abs(s)
  if (a < 60) return rtf.value.format(s, 'second')
  if (a < 3600) return rtf.value.format(Math.round(s / 60), 'minute')
  if (a < 86400) return rtf.value.format(Math.round(s / 3600), 'hour')
  if (a < 86400 * 30) return rtf.value.format(Math.round(s / 86400), 'day')
  return new Date(ts).toLocaleDateString(locale.value, { month: 'short', day: '2-digit' })
}

const confirmedRecent = computed(() => wallet.transactions.filter((x) => x.confirmed).slice(0, 3))

/* ---- pending: реальный обратный отсчёт до блока (~8 с) по первому появлению tx ---- */
const BLOCK_MS = 8000
const firstSeen = new Map<string, number>()
const nowMs = ref(Date.now())
let tick: ReturnType<typeof setInterval> | undefined
watch(
  () => wallet.pendingTxs.map((x) => x.id),
  (ids) => {
    const now = Date.now()
    for (const id of ids) if (!firstSeen.has(id)) firstSeen.set(id, now)
    for (const id of [...firstSeen.keys()]) if (!ids.includes(id)) firstSeen.delete(id)
    if (ids.length && !tick) tick = setInterval(() => (nowMs.value = Date.now()), 200)
    if (!ids.length && tick) { clearInterval(tick); tick = undefined }
  },
  { immediate: true },
)
onBeforeUnmount(() => { if (tick) clearInterval(tick) })
/** Прошедшая доля блока 0..1 (после 8 с остаётся заполненным — ждём следующий блок). */
function pendProgress(id: string): number {
  const t0 = firstSeen.get(id) ?? nowMs.value
  return Math.min(1, (nowMs.value - t0) / BLOCK_MS)
}
function pendEta(id: string): string {
  const left = Math.ceil((BLOCK_MS - (nowMs.value - (firstSeen.get(id) ?? nowMs.value))) / 1000)
  return left > 0 ? t('wallet.pendingEtaSec', { n: left }) : t('wallet.pendingNextBlock')
}
const RING_LEN = 81.7
const idxPct = computed(() => {
  const p = wallet.txIndex
  if (!p) return 0
  return p.done || !p.total ? 100 : Math.min(100, Math.round((p.indexed / p.total) * 100))
})
</script>

<template>
  <div class="dash" data-testid="wallet-dashboard">
    <!-- режим только просмотра (не mainnet) -->
    <div v-if="wallet.viewOnly" class="view-only" data-testid="wallet-view-only">
      <span class="vo-dot" />
      <div class="vo-main">
        <span class="vo-title">{{ wallet.activeNetwork.name || wallet.activeNetwork.token }} · {{ t('wallet.viewOnly.title') }}</span>
        <span class="vo-body dim">{{ t('wallet.viewOnly.body') }}</span>
      </div>
      <button class="vo-back" @click="wallet.switchNetwork('mainnet')" data-testid="wallet-view-only-back">{{ t('wallet.viewOnly.back') }}</button>
    </div>

    <!-- аккаунт (BIP-44: main + суб-адреса одной сид-фразы) -->
    <div class="account" :class="{ open: accOpen }" data-testid="wallet-account-row">
      <span class="acc-badge">{{ accNumber }}</span>
      <button class="acc-main" @click="toggleAccounts" :title="wallet.address" data-testid="wallet-short-address">
        <span class="acc-name">{{ accLabel }} <span v-if="accNumber === 1" class="acc-main-tag">MAIN</span></span>
        <span class="acc-addr mono">{{ wallet.address ? shortAddr(wallet.address, 6, 6) : '—' }}</span>
      </button>
      <button class="acc-ic" @click="copyAddr" :title="t('wallet.receive.tapCopy')" data-testid="wallet-copy-address">
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
      </button>
      <button class="acc-ic" :disabled="!wallet.address" @click="openExplorerWallet(wallet.address)" :title="t('wallet.openInExplorer')" data-testid="wallet-account-explorer">
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 3h7v7M21 3l-9 9M10 5H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-5" /></svg>
      </button>
      <button class="acc-ic acc-chev" :class="{ up: accOpen }" @click="toggleAccounts" :title="t('wallet.accounts.title')" data-testid="wallet-account-more">
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6" /></svg>
      </button>

      <transition name="acc-drop">
        <div v-if="accOpen" class="acc-list" data-testid="wallet-accounts-list">
          <div v-if="wallet.accountsLoading && !wallet.accounts.length" class="acc-loading dim">…</div>
          <div
            v-for="a in wallet.accounts"
            :key="a.index"
            class="acc-item"
            :class="{ on: a.active }"
            role="button"
            tabindex="0"
            :title="a.path"
            @click="pickAccount(a.index)"
            @keydown.enter="pickAccount(a.index)"
            :data-testid="`wallet-account-${a.index}`"
          >
            <span class="acc-badge sm">{{ a.index + 1 }}</span>
            <span class="acc-item-main">
              <input
                v-if="editIdx === a.index"
                v-model="editLabel"
                class="acc-edit mono"
                maxlength="24"
                @click.stop
                @keydown.enter.stop.prevent="commitEdit"
                @keydown.esc.stop="editIdx = null"
                @blur="commitEdit"
                :data-testid="`wallet-account-edit-${a.index}`"
              />
              <span v-else class="acc-item-name">{{ a.label }} <span v-if="a.main" class="acc-main-tag">MAIN</span></span>
              <span class="acc-item-addr mono dim">{{ shortAddr(a.address, 6, 6) }}</span>
            </span>
            <span class="acc-item-bal mono">{{ wallet.balanceMode === 'hidden' ? '••••' : fmtAcc(a.balance) }}</span>
            <button class="acc-pen" @click.stop="startEdit(a.index, a.label)" :title="t('wallet.accounts.rename')" :data-testid="`wallet-account-rename-${a.index}`">
              <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z" /></svg>
            </button>
          </div>
          <button class="acc-add" :disabled="accBusy" @click="addAccount" data-testid="wallet-account-add">+ {{ t('wallet.accounts.add') }}</button>
          <p class="acc-hint dim">{{ t('wallet.accounts.hint') }}</p>
        </div>
      </transition>
    </div>

    <!-- карточка баланса -->
    <div class="balance-card" :class="{ hidden: wallet.balanceMode === 'hidden' }">
      <div class="bc-top">
        <span class="bc-label">{{ t('wallet.totalBalance') }}</span>
        <button class="bc-refresh" :class="{ spin: wallet.loading }" @click="wallet.refresh(true)" data-testid="wallet-refresh">
          <svg viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-2.64-6.36M21 3v6h-6" /></svg>
          {{ t('wallet.refreshLower') }}
        </button>
      </div>

      <button
        class="bc-balance"
        :class="wallet.balanceFlash && `flash-${wallet.balanceFlash}`"
        :title="t('wallet.balanceTapHint')"
        @click="wallet.cycleBalanceMode()"
        data-testid="wallet-balance"
      >
        <template v-if="wallet.balanceMode === 'hidden'">
          <span class="bc-int">••••••</span>
          <span class="bc-unit">STH</span>
        </template>
        <template v-else-if="wallet.balanceMode === 'usdt'">
          <span class="bc-int">${{ fiatParts.int }}</span><span class="bc-frac">,{{ fiatParts.frac }}</span>
          <span class="bc-unit">USDT</span>
        </template>
        <template v-else>
          <span class="bc-int">{{ balanceParts.int }}</span><span v-if="balanceParts.frac" class="bc-frac">,{{ balanceParts.frac }}</span>
          <span class="bc-unit">STH</span>
        </template>
      </button>

      <div class="bc-sub">
        <span v-if="wallet.balanceMode === 'hidden'" class="bc-fiat dim" data-testid="wallet-fiat">••••</span>
        <span v-else-if="wallet.balanceMode === 'usdt'" class="bc-fiat" data-testid="wallet-fiat">≈ {{ balanceParts.int }}<template v-if="balanceParts.frac">,{{ balanceParts.frac }}</template> <b>STH</b></span>
        <span v-else class="bc-fiat" data-testid="wallet-fiat">≈ ${{ fiatParts.int }},{{ fiatParts.frac }} <b>USDT</b></span>
        <span class="bc-rate mono" data-testid="wallet-rate">
          {{ rateStr }}
          <span v-if="spark" class="bc-chg" :class="spark.up ? 'up' : 'down'" :title="t('wallet.rate7d')">{{ spark.up ? '▲' : '▼' }} {{ Math.abs(spark.change).toFixed(1) }}%</span>
        </span>
      </div>

      <!-- спарклайн курса за 7 дней -->
      <svg v-if="spark" class="bc-spark" viewBox="0 0 100 28" preserveAspectRatio="none" aria-hidden="true" data-testid="wallet-sparkline">
        <defs>
          <linearGradient id="sparkFill" x1="0" x2="0" y1="0" y2="1">
            <stop offset="0" :stop-color="spark.up ? 'var(--success)' : '#f0b429'" stop-opacity="0.35" />
            <stop offset="1" :stop-color="spark.up ? 'var(--success)' : '#f0b429'" stop-opacity="0" />
          </linearGradient>
        </defs>
        <path :d="spark.area" fill="url(#sparkFill)" />
        <path :d="spark.line" fill="none" :stroke="spark.up ? 'var(--success)' : '#f0b429'" stroke-width="1.2" vector-effect="non-scaling-stroke" stroke-linejoin="round" />
        <circle :cx="spark.lastX" :cy="spark.lastY" r="1.6" :fill="spark.up ? 'var(--success)' : '#f0b429'" class="spark-dot" />
      </svg>

      <!-- SHIP-44: живая высота сети -->
      <div
        class="bc-live"
        :class="{ live: wallet.streamLive && !wallet.streamStale, stale: wallet.streamStale }"
        :title="wallet.streamStale ? t('wallet.nodes.staleHint') : ''"
        :role="wallet.streamStale ? 'button' : undefined"
        @click="wallet.streamStale && wallet.reconnectStream()"
        data-testid="wallet-live-badge"
      >
        <span class="live-dot" />
        <span class="live-txt">{{ wallet.streamStale ? 'STALE' : wallet.streamLive ? 'LIVE' : 'REST' }}</span>
        <span v-if="wallet.netHeight" class="live-h mono" :title="t('wallet.nodes.netHeight')" data-testid="wallet-net-height">#{{ wallet.netHeight.toLocaleString('ru-RU').replace(/,/g, ' ') }}</span>
        <span v-if="wallet.totalBalance" class="live-total mono" :title="t('wallet.accounts.totalHint')" data-testid="wallet-total-balance">
          Σ {{ wallet.balanceMode === 'hidden' ? '••••' : wallet.totalBalance }} <b>STH</b>
        </span>
      </div>
      <div v-if="wallet.txIndex" class="idx-line mono" :class="{ done: wallet.txIndex.done }" data-testid="wallet-txindex-progress">
        <span class="idx-bar"><i :style="{ width: idxPct + '%' }" /></span>
        <span>{{ wallet.txIndex.done ? t('wallet.indexDone', { n: wallet.txIndex.indexed }) : t('wallet.indexing', { n: wallet.txIndex.indexed, total: wallet.txIndex.total }) }}</span>
      </div>
    </div>

    <!-- быстрые действия -->
    <div class="quick">
      <button
        v-for="q in quick"
        :key="q.id"
        class="qbtn"
        :class="[q.tone, { off: quickDisabled(q.id) }]"
        :disabled="quickDisabled(q.id)"
        @click="wallet.setView(q.id as any)"
        :data-testid="`wallet-quick-${q.id}`"
      >
        <svg class="qicon" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path :d="q.path" />
        </svg>
        <span class="qlabel">{{ q.label() }}</span>
      </button>
    </div>

    <!-- pending: кольцо ~8 с до подтверждения -->
    <div v-if="wallet.droppedTx" class="dropped" data-testid="wallet-tx-dropped">
      <div class="dropped-txt">
        <b>{{ t('wallet.dropped.title') }}</b>
        <span>{{ t('wallet.dropped.body', { id: wallet.droppedTx.id.slice(0, 10) }) }}</span>
        <span v-if="wallet.droppedTx.reason" class="dropped-reason">{{ wallet.droppedTx.reason }}</span>
      </div>
      <button class="dropped-x" @click="wallet.droppedTx = null" data-testid="wallet-tx-dropped-close">✕</button>
    </div>
    <div v-if="wallet.pendingTxs.length" class="pending" data-testid="wallet-pending">
      <div
        v-for="tx in wallet.pendingTxs"
        :key="tx.id"
        class="pend"
        role="button"
        tabindex="0"
        :title="t('wallet.pendingOpenHint')"
        @click="openTx(tx.id)"
        @keydown.enter.prevent="openTx(tx.id)"
        :data-testid="`wallet-pending-${tx.id}`"
      >
        <span class="ring" :class="{ over: pendProgress(tx.id) >= 1 }" aria-hidden="true">
          <svg viewBox="0 0 32 32" width="32" height="32">
            <circle cx="16" cy="16" r="13" class="ring-bg" />
            <circle cx="16" cy="16" r="13" class="ring-fg" :style="{ strokeDashoffset: RING_LEN * (1 - pendProgress(tx.id)) }" />
          </svg>
          <span class="ring-ic">{{ tx.type === 'in' ? '↙' : '↗' }}</span>
        </span>
        <div class="pend-main">
          <span class="pend-title">{{ t('wallet.pendingCard') }} <span class="dim" :data-testid="`wallet-pending-eta-${tx.id}`">· {{ pendEta(tx.id) }}</span></span>
          <span class="pend-addr mono dim">{{ shortAddr(tx.counterparty, 6, 6) }}</span>
        </div>
        <span class="pend-amt mono" :class="tx.type">{{ tx.type === 'in' ? '+' : '−' }}{{ tx.amount }}</span>
        <span class="pend-open" aria-hidden="true" :data-testid="`wallet-pending-open-${tx.id}`">↗</span>
      </div>
    </div>

    <!-- SHIP-14 токены -->
    <div v-if="wallet.tokens.length" class="tokens" data-testid="wallet-tokens">
      <div class="sect-head">
        <span class="sect">{{ t('wallet.tokens.title') }}</span>
        <span class="sect dim">{{ wallet.tokens.length }}</span>
      </div>
      <ul class="tok-list">
        <li v-for="tk in wallet.tokens" :key="tk.id" class="tok" :title="tk.id" :data-testid="`wallet-token-${tk.symbol}`">
          <span class="tok-logo">
            <img v-if="tk.logoUrl" :src="tk.logoUrl" :alt="tk.symbol" @error="($event.target as HTMLImageElement).style.display = 'none'" />
            <span v-else class="tok-glyph">{{ tk.symbol.slice(0, 2) }}</span>
          </span>
          <div class="tok-main">
            <span class="tok-sym mono">{{ tk.symbol }}</span>
            <span v-if="tk.name" class="tok-name dim">{{ tk.name }}</span>
          </div>
          <span class="tok-amt mono">{{ wallet.balanceMode === 'hidden' ? '••••' : tk.amount }}</span>
        </li>
      </ul>
    </div>

    <!-- последние операции -->
    <div class="recent">
      <div class="sect-head">
        <span class="sect">{{ t('wallet.recentActivity') }}</span>
        <button class="view-all" @click="wallet.setView('history')" data-testid="wallet-view-all">{{ t('wallet.viewAll') }}</button>
      </div>

      <div v-if="!confirmedRecent.length && !wallet.pendingTxs.length" class="empty dim" data-testid="wallet-no-tx">
        {{ t('wallet.noTx') }}
      </div>

      <ul v-else class="tx-list">
        <li
          v-for="tx in confirmedRecent"
          :key="tx.id"
          class="tx"
          :class="tx.type"
          @click="openTx(tx.id)"
          :title="t('payoutHistory.openExplorer')"
          :data-testid="`wallet-tx-${tx.id}`"
        >
          <span class="tx-ic" :class="tx.type">
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">
              <path v-if="tx.type === 'in'" d="M17 7 7 17M16 17H7V8" />
              <path v-else d="M7 17 17 7M8 7h9v9" />
            </svg>
          </span>
          <div class="tx-main">
            <span class="tx-type">{{ tx.type === 'in' ? t('wallet.received') : t('wallet.sent') }}</span>
            <span class="tx-addr mono">{{ shortAddr(tx.counterparty, 6, 6) }}</span>
          </div>
          <div class="tx-right">
            <span class="tx-amt mono" :class="tx.type">{{ wallet.balanceMode === 'hidden' ? '••••' : (tx.type === 'in' ? '+' : '−') + tx.amount }}</span>
            <span class="tx-date dim">{{ timeAgo(tx.timestamp) }}</span>
          </div>
        </li>
      </ul>
    </div>
  </div>
</template>

<style scoped>
.dropped {
  display: flex; align-items: flex-start; gap: 10px; margin: 0 0 12px;
  padding: 12px 14px; border: 1px dashed rgba(248, 113, 113, 0.6); border-radius: 12px;
  background: rgba(248, 113, 113, 0.08); color: #fca5a5; font-size: 12px;
}
.dropped-txt { display: flex; flex-direction: column; gap: 3px; flex: 1; min-width: 0; }
.dropped-txt b { letter-spacing: 0.06em; text-transform: uppercase; font-size: 11px; }
.dropped-reason { opacity: 0.75; font-family: 'JetBrains Mono', monospace; font-size: 11px; word-break: break-word; }
.dropped-x { background: none; border: none; color: inherit; cursor: pointer; font-size: 14px; padding: 0 2px; }

.dash {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.mono {
  font-family: var(--font-mono, monospace);
}
.dim {
  color: var(--text-dim);
}

/* ---- view-only ---- */
.view-only {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 12px;
  border: 1px solid #f0b429;
  border-radius: var(--radius);
  background: rgba(240, 180, 41, 0.08);
}
.vo-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #f0b429;
  flex: none;
}
.vo-main {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.vo-title {
  font-size: 11px;
  font-weight: var(--fw-800);
  letter-spacing: 0.08em;
  text-transform: uppercase;
}
.vo-body {
  font-size: 11px;
}
.vo-back {
  margin-left: auto;
  flex: none;
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  font-weight: var(--fw-800);
  color: var(--accent);
  border: 1px solid var(--border-accent);
  border-radius: 999px;
  padding: 5px 10px;
  cursor: pointer;
  background: transparent;
}

/* ---- account ---- */
.account {
  position: relative;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 10px;
  padding: 10px 12px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  transition: border-color 0.15s ease;
}
.account.open {
  border-color: var(--border-accent);
}
.acc-main-tag {
  margin-left: 6px;
  font-size: 8px;
  letter-spacing: 0.14em;
  padding: 1px 5px;
  border-radius: 3px;
  color: var(--accent);
  border: 1px solid var(--border-accent);
  vertical-align: middle;
}
.acc-chev svg {
  transition: transform 0.2s ease;
}
.acc-chev.up svg {
  transform: rotate(180deg);
}
.acc-list {
  flex-basis: 100%;
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding-top: 8px;
  border-top: 1px dashed var(--border);
}
.acc-drop-enter-active,
.acc-drop-leave-active {
  transition: opacity 0.16s ease, transform 0.16s ease;
}
.acc-drop-enter-from,
.acc-drop-leave-to {
  opacity: 0;
  transform: translateY(-4px);
}
.acc-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--bg-elevated);
  cursor: pointer;
  transition: border-color 0.15s ease, background-color 0.15s ease;
}
.acc-item:hover {
  border-color: var(--border-accent);
}
.acc-item.on {
  border-color: var(--accent);
  background: color-mix(in srgb, var(--accent) 10%, var(--bg-elevated));
}
.acc-badge.sm {
  width: 22px;
  height: 22px;
  font-size: 11px;
}
.acc-item-main {
  display: flex;
  flex-direction: column;
  gap: 1px;
  min-width: 0;
  flex: 1;
}
.acc-item-name {
  font-size: 12px;
  font-weight: var(--fw-800);
}
.acc-item-addr {
  font-size: 10px;
}
.acc-item-bal {
  font-size: 12px;
  font-weight: var(--fw-800);
  color: var(--accent);
  white-space: nowrap;
}
.acc-pen {
  width: 22px;
  height: 22px;
  display: grid;
  place-items: center;
  border-radius: 50%;
  color: var(--text-dim);
  cursor: pointer;
}
.acc-pen:hover {
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 12%, transparent);
}
.acc-edit {
  width: 100%;
  font-size: 12px;
  padding: 2px 6px;
  color: var(--text);
  background: var(--bg);
  border: 1px solid var(--accent);
  border-radius: 4px;
}
.acc-edit:focus {
  outline: none;
}
.acc-add {
  padding: 9px;
  border: 1px dashed var(--border-accent);
  border-radius: var(--radius);
  font-size: 10px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  font-weight: var(--fw-800);
  color: var(--accent);
  cursor: pointer;
  transition: background-color 0.15s ease;
}
.acc-add:hover:not(:disabled) {
  background: color-mix(in srgb, var(--accent) 10%, transparent);
}
.acc-add:disabled {
  opacity: 0.5;
}
.acc-hint,
.acc-loading {
  font-size: 10px;
  padding: 0 2px;
}
.acc-badge {
  width: 26px;
  height: 26px;
  display: grid;
  place-items: center;
  border-radius: 6px;
  font-size: 12px;
  font-weight: var(--fw-800);
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  border: 1px solid var(--border-accent);
}
.acc-main {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 1px;
  flex: 1;
  min-width: 0;
  cursor: pointer;
  text-align: left;
}
.acc-name {
  font-size: 11px;
  font-weight: var(--fw-800);
  letter-spacing: 0.1em;
  text-transform: uppercase;
}
.acc-addr {
  font-size: 11px;
  color: var(--text-dim);
}
.acc-ic {
  width: 28px;
  height: 28px;
  display: grid;
  place-items: center;
  border-radius: 6px;
  color: var(--text-dim);
  cursor: pointer;
  transition: color 0.15s ease, background-color 0.15s ease;
}
.acc-ic:hover {
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 10%, transparent);
}

/* ---- balance card ---- */
.balance-card {
  position: relative;
  overflow: hidden;
  padding: 16px 18px 14px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background:
    repeating-linear-gradient(135deg, color-mix(in srgb, var(--text) 4%, transparent) 0 1px, transparent 1px 9px),
    var(--bg-elevated);
}
.balance-card::after {
  content: '';
  position: absolute;
  inset: 0;
  pointer-events: none;
  background: radial-gradient(120% 90% at 100% 0%, color-mix(in srgb, var(--accent) 12%, transparent), transparent 55%);
}
.balance-card > * {
  position: relative;
  z-index: 1;
}
.bc-top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 6px;
}
.bc-label {
  font-size: 10px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  font-weight: var(--fw-700);
  color: var(--text-dim);
}
.bc-refresh {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 10px;
  letter-spacing: 0.04em;
  color: var(--text-dim);
  cursor: pointer;
  transition: color 0.15s ease;
}
.bc-refresh:hover {
  color: var(--accent);
}
.bc-refresh.spin svg {
  animation: spin 0.9s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}
.bc-balance {
  display: flex;
  align-items: baseline;
  gap: 2px;
  cursor: pointer;
  text-align: left;
  transform-origin: left center;
  color: var(--text);
  line-height: 1.05;
  transition: opacity 0.15s ease;
}
.bc-balance:hover {
  opacity: 0.86;
}
.bc-int {
  font-size: 34px;
  font-weight: var(--fw-800);
  letter-spacing: -0.02em;
  font-family: var(--font-mono, monospace);
}
.bc-frac {
  font-size: 22px;
  font-weight: var(--fw-700);
  color: var(--secondary);
  font-family: var(--font-mono, monospace);
}
.bc-unit {
  margin-left: 8px;
  font-size: 13px;
  font-weight: var(--fw-700);
  letter-spacing: 0.08em;
  color: var(--text-dim);
}
.balance-card.hidden .bc-int {
  letter-spacing: 0.1em;
}
.bc-balance.flash-up {
  animation: balance-flash-up 1.4s ease-out;
}
.bc-balance.flash-down {
  animation: balance-flash-down 1.4s ease-out;
}
@keyframes balance-flash-up {
  0% {
    color: var(--accent);
    text-shadow: 0 0 18px color-mix(in srgb, var(--accent) 85%, transparent);
    transform: scale(1.04);
  }
  100% {
    color: var(--text);
    text-shadow: none;
    transform: scale(1);
  }
}
@keyframes balance-flash-down {
  0% {
    color: #f0b429;
    text-shadow: 0 0 18px rgba(240, 180, 41, 0.8);
    transform: scale(1.04);
  }
  100% {
    color: var(--text);
    text-shadow: none;
    transform: scale(1);
  }
}
.bc-sub {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 10px;
  margin-top: 8px;
}
.bc-fiat {
  font-size: 13px;
  font-weight: var(--fw-700);
  color: var(--text);
}
.bc-fiat b {
  font-size: 10px;
  letter-spacing: 0.1em;
  color: var(--text-dim);
}
.bc-rate {
  font-size: 10px;
  color: var(--text-dim);
  display: inline-flex;
  gap: 6px;
  align-items: baseline;
}
.bc-chg {
  font-weight: var(--fw-800);
}
.bc-chg.up {
  color: var(--success);
}
.bc-chg.down {
  color: #f0b429;
}
.bc-spark {
  display: block;
  width: calc(100% + 36px);
  height: 34px;
  margin: 10px -18px -6px;
  opacity: 0.9;
}
.spark-dot {
  animation: live-pulse 2.2s ease-in-out infinite;
  transform-box: fill-box;
  transform-origin: center;
}
.idx-line {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 6px;
  font-size: 10px;
  letter-spacing: 0.04em;
  color: var(--text-dim);
}
.idx-line.done {
  color: #22c55e;
}
.idx-bar {
  flex: 0 0 72px;
  height: 3px;
  border-radius: 2px;
  background: rgba(255, 255, 255, 0.08);
  overflow: hidden;
}
.idx-bar i {
  display: block;
  height: 100%;
  background: var(--accent);
  transition: width 0.3s ease;
}
.idx-line.done .idx-bar i {
  background: #22c55e;
}
.live-total {
  margin-left: auto;
  color: var(--text-dim);
  letter-spacing: 0.04em;
  font-weight: var(--fw-700);
}
.live-total b {
  font-size: 8px;
  letter-spacing: 0.12em;
  opacity: 0.8;
}
.bc-live {
  margin-top: 10px;
  display: flex;
  width: 100%;
  align-items: center;
  align-items: center;
  gap: 6px;
  font-size: 9px;
  letter-spacing: 0.14em;
  font-weight: var(--fw-700);
  color: var(--text-dim);
}
.bc-live .live-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--success);
  box-shadow: 0 0 6px var(--success);
  animation: live-pulse 2.2s ease-in-out infinite;
}
.bc-live.live {
  color: var(--success);
}
.bc-live.live .live-dot {
  animation-duration: 1.4s;
}
.bc-live.stale {
  color: #f0b429;
  cursor: pointer;
}
.bc-live.stale .live-dot {
  background: #f0b429;
  box-shadow: 0 0 6px #f0b429;
  animation: none;
}
.bc-live.stale:hover .live-txt {
  text-decoration: underline;
}
.bc-live .live-h {
  color: var(--secondary);
  letter-spacing: 0.04em;
}
.bc-live.live .live-h {
  color: var(--text);
}
@keyframes live-pulse {
  0%,
  100% {
    opacity: 1;
    transform: scale(1);
  }
  50% {
    opacity: 0.55;
    transform: scale(0.8);
  }
}

/* ---- quick actions (цветные, как в расширении) ---- */
.quick {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 8px;
}
.qbtn {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 7px;
  padding: 12px 4px 10px;
  border-radius: var(--radius);
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text-dim);
  cursor: pointer;
  transition: transform 0.15s ease, border-color 0.15s ease, background-color 0.15s ease, color 0.15s ease, box-shadow 0.15s ease;
}
.qbtn.accent {
  color: var(--accent);
  border-color: color-mix(in srgb, var(--accent) 55%, transparent);
  background: color-mix(in srgb, var(--accent) 10%, var(--panel));
}
.qbtn.secondary {
  color: var(--secondary);
  border-color: color-mix(in srgb, var(--secondary) 55%, transparent);
  background: color-mix(in srgb, var(--secondary) 10%, var(--panel));
}
.qbtn:hover:not(:disabled) {
  transform: translateY(-2px);
  box-shadow: 0 8px 20px -12px currentColor;
}
.qbtn.accent:hover:not(:disabled) {
  background: color-mix(in srgb, var(--accent) 18%, var(--panel));
  border-color: var(--accent);
}
.qbtn.secondary:hover:not(:disabled) {
  background: color-mix(in srgb, var(--secondary) 18%, var(--panel));
  border-color: var(--secondary);
}
.qbtn.plain:hover:not(:disabled) {
  color: var(--text);
  border-color: var(--text-dim);
}
.qbtn.off {
  opacity: 0.4;
  cursor: not-allowed;
}
.qicon {
  transition: transform 0.2s ease;
}
.qbtn:hover:not(:disabled) .qicon {
  transform: translateX(2px);
}
.qbtn.secondary:hover:not(:disabled) .qicon {
  transform: translateX(-2px);
}
.qlabel {
  font-size: 9px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  font-weight: var(--fw-800);
}

/* ---- pending ---- */
.pending {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.pend {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 9px 12px;
  border: 1px dashed #f0b429;
  border-radius: var(--radius);
  background: rgba(240, 180, 41, 0.06);
  cursor: pointer;
  transition: background-color 0.15s, border-color 0.15s;
}
.pend:hover,
.pend:focus-visible {
  background: rgba(240, 180, 41, 0.14);
  border-color: #ffd166;
  outline: none;
}
.pend-open {
  flex: none;
  font-size: 12px;
  color: #f0b429;
  opacity: 0.55;
  transition: opacity 0.15s, transform 0.15s;
}
.pend:hover .pend-open {
  opacity: 1;
  transform: translate(1px, -1px);
}
.ring {
  position: relative;
  width: 32px;
  height: 32px;
  flex: none;
}
.ring svg {
  transform: rotate(-90deg);
}
.ring-bg {
  fill: none;
  stroke: var(--border);
  stroke-width: 2.5;
}
.ring-fg {
  fill: none;
  stroke: #f0b429;
  stroke-width: 2.5;
  stroke-linecap: round;
  stroke-dasharray: 81.7;
  transition: stroke-dashoffset 0.2s linear;
}
.ring.over .ring-fg {
  animation: ring-pulse 1.2s ease-in-out infinite;
}
@keyframes ring-pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.35; }
}
.ring-ic {
  position: absolute;
  inset: 0;
  display: grid;
  place-items: center;
  font-size: 13px;
  color: #f0b429;
}
.pend-main {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.pend-title {
  font-size: 11px;
  font-weight: var(--fw-800);
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: #f0b429;
}
.pend-addr {
  font-size: 11px;
}
.pend-amt {
  margin-left: auto;
  font-size: 13px;
  font-weight: var(--fw-800);
}
.pend-amt.in {
  color: var(--success);
}

/* ---- sections ---- */
.sect-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 8px;
}
.sect {
  font-size: 10px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  font-weight: var(--fw-800);
}
.view-all {
  font-size: 10px;
  letter-spacing: 0.12em;
  color: var(--accent);
  font-weight: var(--fw-800);
  cursor: pointer;
}
.view-all:hover {
  text-decoration: underline;
}
.empty {
  font-size: 12px;
  padding: 14px 0;
}

/* ---- tokens ---- */
.tok-list {
  list-style: none;
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 6px;
}
.tok {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 12px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  min-width: 0;
  transition: border-color 0.2s ease, transform 0.2s ease;
}
.tok:hover {
  border-color: var(--accent);
  transform: translateY(-1px);
}
.tok-logo {
  width: 28px;
  height: 28px;
  flex: none;
  border-radius: 50%;
  overflow: hidden;
  display: grid;
  place-items: center;
  background: var(--bg);
  border: 1px solid var(--border);
}
.tok-logo img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}
.tok-glyph {
  font-size: 10px;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
  color: var(--accent);
}
.tok-main {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.tok-sym {
  font-size: 12px;
  font-weight: var(--fw-700);
}
.tok-name {
  font-size: 10px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.tok-amt {
  margin-left: auto;
  font-size: 13px;
  font-weight: var(--fw-700);
  white-space: nowrap;
}

/* ---- recent tx cards ---- */
.tx-list {
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.tx {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 14px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  cursor: pointer;
  transition: border-color 0.15s ease, transform 0.15s ease, box-shadow 0.15s ease;
}
.tx:hover {
  transform: translateY(-1px);
  border-color: var(--border-accent);
  box-shadow: 0 10px 24px -18px var(--accent);
}
.tx-ic {
  width: 34px;
  height: 34px;
  flex: none;
  display: grid;
  place-items: center;
  border-radius: 8px;
  border: 1px solid;
}
.tx-ic.in {
  color: var(--accent);
  border-color: color-mix(in srgb, var(--accent) 50%, transparent);
  background: color-mix(in srgb, var(--accent) 12%, transparent);
}
.tx-ic.out {
  color: var(--secondary);
  border-color: color-mix(in srgb, var(--secondary) 50%, transparent);
  background: color-mix(in srgb, var(--secondary) 12%, transparent);
}
.tx-main {
  display: flex;
  flex-direction: column;
  gap: 3px;
  min-width: 0;
}
.tx-type {
  font-size: 11px;
  font-weight: var(--fw-800);
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text);
}
.tx-addr {
  font-size: 11px;
  color: var(--text-dim);
}
.tx-right {
  margin-left: auto;
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 3px;
}
.tx-amt {
  font-size: 14px;
  font-weight: var(--fw-800);
}
.tx-amt.in {
  color: var(--accent);
}
.tx-amt.out {
  color: var(--text);
}
.tx-date {
  font-size: 10px;
}
</style>
