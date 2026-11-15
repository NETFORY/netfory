<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { useTabsStore } from '@/store/tabs'
import { useWalletStore } from '@/store/wallet'
import { useAuthStore } from '@/store/auth'
import {
  listMarketOrders,
  atomicBuy,
  createDomainDraft,
  resolveDomain,
  usernameOfAddress,
  type SaleOrder,
} from '@/lib/bridge'
import { shortAddr } from '@/lib/crypto'

const { t } = useI18n()
const router = useRouter()
const tabs = useTabsStore()
const wallet = useWalletStore()
const auth = useAuthStore()

const orders = ref<SaleOrder[]>([])
const names = ref<Record<string, string>>({})
const search = ref('')
const sortBy = ref<'priceAsc' | 'priceDesc' | 'name' | 'expiry'>('priceAsc')
const loading = ref(true)
const selected = ref<SaleOrder | null>(null)
const busy = ref(false)
const flash = ref('')
const flashOk = ref(true)

// Phase 3c — post-purchase "path of the hero" modal.
const modalPhase = ref<'' | 'processing' | 'success'>('')
const boughtName = ref('')
const boughtAppId = ref('')

function isMine(o: SaleOrder): boolean {
  return !!auth.address && o.seller === auth.address
}

// Short 3-4 char labels are rare/high-value → flag them PREMIUM.
function isPremium(o: SaleOrder): boolean {
  return o.name.length <= 4
}

const balanceSth = computed(() => Number(wallet.mainBalance) || 0)
const filtered = computed(() => {
  const q = search.value.trim().toLowerCase()
  const base = q ? orders.value.filter((o) => o.name.includes(q)) : orders.value.slice()
  const arr = base.slice()
  switch (sortBy.value) {
    case 'priceDesc':
      arr.sort((a, b) => b.priceSth - a.priceSth)
      break
    case 'name':
      arr.sort((a, b) => a.name.localeCompare(b.name))
      break
    case 'expiry':
      arr.sort((a, b) => a.expires - b.expires)
      break
    default:
      arr.sort((a, b) => a.priceSth - b.priceSth)
  }
  return arr
})

function notify(msg: string, ok = true) {
  flash.value = msg
  flashOk.value = ok
  setTimeout(() => (flash.value = ''), 3000)
}

async function load() {
  loading.value = true
  orders.value = await listMarketOrders()
  for (const o of orders.value) {
    if (!(o.seller in names.value)) {
      names.value[o.seller] = (await usernameOfAddress(o.seller)) || ''
    }
  }
  loading.value = false
}
onMounted(load)

// Auto-refresh: re-read the (local) market index periodically so a seller's
// listing disappears as soon as the chain indexer marks it sold — no manual
// refresh needed. `listMarketOrders` is local DB only, so this is cheap.
let pollTimer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  pollTimer = setInterval(() => {
    if (!document.hidden && !busy.value && !loading.value) load()
  }, 20000)
})
onUnmounted(() => {
  if (pollTimer) clearInterval(pollTimer)
})

function sellerLabel(o: SaleOrder): string {
  return names.value[o.seller] ? `u://${names.value[o.seller]}` : shortAddr(o.seller, 6, 5)
}

async function openApp(o: SaleOrder) {
  const appId = await resolveDomain(o.name)
  if (appId) {
    tabs.open(appId, o.name)
    router.push('/dashboard')
  }
}

async function confirmBuy() {
  if (!selected.value || busy.value) return
  busy.value = true
  const order = selected.value
  modalPhase.value = 'processing'
  try {
    // Phase 3c: auto-mint a draft app (cyberpunk stub) for the bought domain,
    // then atomically buy it — binding the domain to the new draft appId.
    const draft = await createDomainDraft(order.name)
    const newAppId = draft?.id || ''
    await atomicBuy(order.name, order.priceSth, newAppId)
    boughtName.value = order.name
    boughtAppId.value = newAppId
    modalPhase.value = 'success'
    selected.value = null
    await load()
  } catch (e: any) {
    modalPhase.value = ''
    notify(typeof e === 'string' ? e : e?.message || 'error', false)
  } finally {
    busy.value = false
  }
}

function goToBought() {
  const appId = boughtAppId.value
  modalPhase.value = ''
  if (appId) {
    tabs.open(appId, `${boughtName.value}.sth`)
    router.push('/dashboard')
  }
}
function closeModal() {
  modalPhase.value = ''
}
</script>

<template>
  <div class="dmk" data-testid="domains-market">
    <header class="dmk-head">
      <h1 class="dmk-title">// DomainsMarket_</h1>
      <p class="dmk-sub">{{ t('domarket.subtitle') }}</p>
      <div class="dmk-search-wrap">
        <span class="dmk-search-glyph">⌕</span>
        <input
          v-model="search"
          class="dmk-search"
          :placeholder="t('domarket.searchPh')"
          spellcheck="false"
          data-testid="domarket-search"
        />
      </div>
      <div class="dmk-sort" data-testid="domarket-sort-wrap">
        <span class="dmk-sort-label">{{ t('domarket.sort') }}</span>
        <select v-model="sortBy" class="dmk-sort-sel" data-testid="domarket-sort">
          <option value="priceAsc">{{ t('domarket.sortPriceAsc') }}</option>
          <option value="priceDesc">{{ t('domarket.sortPriceDesc') }}</option>
          <option value="name">{{ t('domarket.sortName') }}</option>
          <option value="expiry">{{ t('domarket.sortExpiry') }}</option>
        </select>
      </div>
    </header>

    <p v-if="loading" class="dmk-empty">{{ t('domarket.loading') }}</p>
    <p v-else-if="!filtered.length" class="dmk-empty" data-testid="domarket-empty">{{ t('domarket.empty') }}</p>

    <div v-else class="dmk-grid" data-testid="domarket-grid">
      <div v-for="o in filtered" :key="o.name" class="dmk-card" :class="{ premium: isPremium(o) }" :data-testid="`domarket-card-${o.name}`">
        <button class="dmk-open" :title="`sth://${o.name}`" @click="openApp(o)" :data-testid="`domarket-open-${o.name}`">↗</button>
        <span v-if="isPremium(o)" class="dmk-premium-badge" :data-testid="`domarket-premium-${o.name}`">★ {{ t('domarket.premium') }}</span>
        <span class="dmk-price">{{ o.priceSth }} <i>STH</i></span>
        <span class="dmk-name">sth://{{ o.name }}</span>
        <span class="dmk-seller">{{ sellerLabel(o) }}</span>
        <span v-if="isMine(o)" class="dmk-own" :data-testid="`domarket-own-${o.name}`">
          ★ {{ t('domarket.yourDomain') }}
        </span>
        <button
          v-else
          class="dmk-buy"
          @click="selected = o"
          :data-testid="`domarket-buy-${o.name}`"
        >
          {{ t('domarket.buy') }}
        </button>
      </div>
    </div>

    <!-- buy confirmation bar -->
    <transition name="slide">
      <div v-if="selected" class="dmk-confirm" data-testid="domarket-confirm">
        <div class="dmk-confirm-info">
          <span class="dmk-confirm-name">sth://{{ selected.name }}</span>
          <span class="dmk-confirm-price">{{ selected.priceSth }} STH</span>
          <span class="dmk-confirm-bal" :class="{ low: balanceSth < selected.priceSth }">
            {{ t('nodePanel.domBalance') }}: {{ balanceSth }} STH
          </span>
        </div>
        <div class="dmk-confirm-actions">
          <button class="dmk-btn" :disabled="busy" @click="selected = null" data-testid="domarket-cancel">
            {{ t('common.cancel') }}
          </button>
          <button
            class="dmk-btn primary"
            :disabled="busy || balanceSth < selected.priceSth"
            @click="confirmBuy"
            data-testid="domarket-confirm-buy"
          >
            {{ busy ? '…' : t('domarket.confirmBuy', { price: selected.priceSth }) }}
          </button>
        </div>
      </div>
    </transition>

    <transition name="fade">
      <div v-if="flash" class="dmk-flash" :class="{ err: !flashOk }" data-testid="domarket-flash">
        {{ flashOk ? '✓' : '⚠' }} {{ flash }}
      </div>
    </transition>

    <!-- Phase 3c — post-purchase "path of the hero" overlay -->
    <transition name="fade">
      <div v-if="modalPhase" class="dmk-overlay" data-testid="domarket-buy-modal">
        <div class="dmk-modal">
          <template v-if="modalPhase === 'processing'">
            <div class="dmk-spinner">⟳</div>
            <h2 class="dmk-modal-title">{{ t('domarket.buying') }}</h2>
            <p class="dmk-modal-sub">{{ t('domarket.buyingSub') }}</p>
            <div class="dmk-steps">
              <span class="dmk-step on">1 · {{ t('domarket.stepFee') }}</span>
              <span class="dmk-step on">2 · {{ t('domarket.stepSeller') }}</span>
              <span class="dmk-step on">3 · {{ t('domarket.stepClaim') }}</span>
            </div>
          </template>
          <template v-else>
            <div class="dmk-trophy">🏆</div>
            <h2 class="dmk-modal-title">{{ t('domarket.congrats') }}</h2>
            <p class="dmk-bought">sth://{{ boughtName }}</p>
            <p class="dmk-modal-sub">{{ t('domarket.draftReady') }}</p>
            <div class="dmk-modal-actions">
              <button class="dmk-btn" @click="closeModal" data-testid="domarket-modal-close">
                {{ t('domarket.stayHere') }}
              </button>
              <button class="dmk-btn primary" @click="goToBought" data-testid="domarket-modal-go">
                {{ t('domarket.goToApp') }}
              </button>
            </div>
          </template>
        </div>
      </div>
    </transition>
  </div>
</template>

<style scoped>
.dmk {
  padding: 28px 32px 120px;
  max-width: 1100px;
  margin: 0 auto;
}
.dmk-head {
  margin-bottom: 22px;
}
.dmk-title {
  font-size: 26px;
  font-weight: 800;
  letter-spacing: 0.04em;
  color: var(--accent);
}
.dmk-sub {
  font-size: 13px;
  color: var(--text-dim);
  margin: 4px 0 16px;
}
.dmk-search-wrap {
  display: flex;
  align-items: center;
  gap: 8px;
  border: 1px solid var(--border);
  background: rgba(0, 0, 0, 0.3);
  border-radius: 10px;
  padding: 0 14px;
  max-width: 460px;
}
.dmk-search-wrap:focus-within {
  border-color: var(--accent);
}
.dmk-search-glyph {
  color: var(--text-dim);
  font-size: 16px;
}
.dmk-search {
  flex: 1;
  background: transparent;
  border: none;
  outline: none;
  color: var(--text);
  font-size: 14px;
  padding: 12px 0;
  font-family: inherit;
}
.dmk-sort {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 12px;
}
.dmk-sort-label {
  font-size: 10px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.dmk-sort-sel {
  background: rgba(0, 0, 0, 0.3);
  border: 1px solid var(--border);
  border-radius: 8px;
  color: var(--text);
  font-size: 12px;
  font-family: inherit;
  padding: 7px 10px;
  cursor: pointer;
  outline: none;
  transition: border-color 0.15s;
}
.dmk-sort-sel:focus {
  border-color: var(--accent);
}
.dmk-card.premium {
  border-color: rgba(231, 193, 75, 0.45);
  background: linear-gradient(160deg, rgba(231, 193, 75, 0.08), rgba(0, 0, 0, 0.28));
}
.dmk-card.premium:hover {
  border-color: #e7c14b;
}
.dmk-premium-badge {
  position: absolute;
  top: 12px;
  left: 18px;
  font-size: 9px;
  font-weight: 800;
  letter-spacing: 0.1em;
  color: #04130d;
  background: #e7c14b;
  padding: 2px 8px;
  border-radius: 999px;
  box-shadow: 0 0 14px -2px rgba(231, 193, 75, 0.7);
}
.dmk-card.premium .dmk-price {
  margin-top: 14px;
}

.dmk-empty {
  color: var(--text-dim);
  font-size: 13px;
  padding: 40px 0;
  text-align: center;
}
.dmk-grid {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 14px;
}
.dmk-card {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 4px;
  border: 1px solid var(--border);
  border-radius: 12px;
  padding: 16px 18px;
  background: linear-gradient(160deg, rgba(127, 217, 207, 0.04), rgba(0, 0, 0, 0.25));
  transition: border-color 0.15s, transform 0.15s;
}
.dmk-card:hover {
  border-color: var(--accent);
  transform: translateY(-2px);
}
.dmk-open {
  position: absolute;
  top: 12px;
  right: 12px;
  width: 26px;
  height: 26px;
  border-radius: 6px;
  border: 1px solid var(--border);
  background: rgba(0, 0, 0, 0.3);
  color: var(--accent);
  cursor: pointer;
  font-size: 14px;
  transition: border-color 0.15s, background 0.15s;
}
.dmk-open:hover {
  border-color: var(--accent);
  background: rgba(127, 217, 207, 0.12);
}
.dmk-price {
  font-size: 22px;
  font-weight: 800;
  color: var(--accent);
}
.dmk-price i {
  font-style: normal;
  font-size: 12px;
  color: var(--text-dim);
}
.dmk-name {
  font-size: 15px;
  font-weight: 700;
  color: var(--text);
  font-family: var(--font-mono, monospace);
}
.dmk-seller {
  font-size: 11px;
  color: var(--text-dim);
}
.dmk-buy {
  margin-top: 10px;
  align-self: flex-start;
  padding: 7px 18px;
  border-radius: 999px;
  border: 1px solid var(--accent);
  background: var(--accent);
  color: #04130d;
  font-size: 12px;
  font-weight: 800;
  letter-spacing: 0.05em;
  cursor: pointer;
  transition: opacity 0.15s;
}
.dmk-buy:hover {
  opacity: 0.85;
}
.dmk-own {
  margin-top: 10px;
  align-self: flex-start;
  padding: 7px 14px;
  border-radius: 999px;
  border: 1px solid rgba(231, 193, 75, 0.5);
  color: #e7c14b;
  font-size: 11px;
  font-weight: 800;
  letter-spacing: 0.05em;
}
.dmk-overlay {
  position: fixed;
  inset: 0;
  z-index: 80;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(2, 6, 10, 0.78);
  backdrop-filter: blur(8px);
}
.dmk-modal {
  width: min(440px, 92vw);
  border: 1px solid var(--accent);
  border-radius: 16px;
  padding: 36px 30px;
  text-align: center;
  background: rgba(8, 18, 22, 0.97);
  box-shadow: 0 0 60px -18px var(--accent);
}
.dmk-spinner {
  font-size: 40px;
  color: var(--accent);
  display: inline-block;
  animation: dmk-spin 1s linear infinite;
}
@keyframes dmk-spin {
  to { transform: rotate(360deg); }
}
.dmk-trophy {
  font-size: 46px;
}
.dmk-modal-title {
  margin: 14px 0 6px;
  font-size: 22px;
  font-weight: 800;
  color: var(--text);
}
.dmk-bought {
  font-size: 18px;
  font-weight: 800;
  color: var(--accent);
  font-family: var(--font-mono, monospace);
  word-break: break-all;
  text-shadow: 0 0 18px rgba(127, 217, 207, 0.4);
}
.dmk-modal-sub {
  margin-top: 10px;
  font-size: 12px;
  line-height: 1.6;
  color: var(--text-dim);
}
.dmk-steps {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-top: 18px;
  text-align: left;
}
.dmk-step {
  font-size: 11px;
  letter-spacing: 0.04em;
  color: var(--text-dim);
  padding: 6px 10px;
  border-radius: 6px;
  border: 1px solid var(--border);
  background: rgba(0, 0, 0, 0.25);
}
.dmk-step.on {
  color: var(--accent);
  border-color: rgba(127, 217, 207, 0.4);
}
.dmk-modal-actions {
  display: flex;
  gap: 8px;
  margin-top: 22px;
}
.dmk-modal-actions .dmk-btn {
  flex: 1;
}

.dmk-confirm {
  position: fixed;
  left: 50%;
  bottom: 24px;
  transform: translateX(-50%);
  width: min(680px, 92vw);
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  border: 1px solid var(--accent);
  border-radius: 12px;
  padding: 14px 18px;
  background: rgba(8, 18, 22, 0.96);
  backdrop-filter: blur(14px);
  z-index: 50;
}
.dmk-confirm-info {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.dmk-confirm-name {
  font-size: 14px;
  font-weight: 700;
  color: var(--text);
}
.dmk-confirm-price {
  font-size: 13px;
  color: var(--accent);
}
.dmk-confirm-bal {
  font-size: 11px;
  color: var(--text-dim);
}
.dmk-confirm-bal.low {
  color: #ff8a8a;
}
.dmk-confirm-actions {
  display: flex;
  gap: 8px;
}
.dmk-btn {
  padding: 9px 16px;
  border-radius: 8px;
  border: 1px solid var(--border);
  background: transparent;
  color: var(--text);
  font-size: 12px;
  font-weight: 700;
  cursor: pointer;
  transition: border-color 0.15s, opacity 0.15s;
}
.dmk-btn.primary {
  background: var(--accent);
  color: #04130d;
  border-color: var(--accent);
}
.dmk-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.dmk-flash {
  position: fixed;
  bottom: 96px;
  left: 50%;
  transform: translateX(-50%);
  padding: 10px 16px;
  border-radius: 8px;
  font-size: 13px;
  background: rgba(0, 200, 120, 0.14);
  border: 1px solid rgba(0, 200, 120, 0.4);
  color: var(--accent);
  z-index: 60;
}
.dmk-flash.err {
  background: rgba(255, 90, 90, 0.14);
  border-color: rgba(255, 90, 90, 0.4);
  color: #ff8a8a;
}
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.2s ease;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
.slide-enter-active,
.slide-leave-active {
  transition: transform 0.2s ease, opacity 0.2s ease;
}
.slide-enter-from,
.slide-leave-to {
  transform: translate(-50%, 20px);
  opacity: 0;
}
@media (max-width: 720px) {
  .dmk-grid {
    grid-template-columns: 1fr;
  }
}
</style>
