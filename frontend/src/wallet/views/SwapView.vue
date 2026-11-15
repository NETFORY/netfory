<script setup lang="ts">
import { ref, computed, watch, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { useWalletStore } from '@/store/wallet'
import { walletInvoke, type SwapQuote } from '@/wallet/ipc'
import HoldButton from '@/wallet/components/HoldButton.vue'

const { t } = useI18n()
const wallet = useWalletStore()

const tab = ref<'buy' | 'sell'>('buy')
const FEE = 0.25
const copiedGw = ref(false)

/* ---- live gateway addresses (fetched from the exchange) ---- */
const buyDeposit = ref('') // per-user BEP20 (BSC) address to send USDT for BUY
const sellGate = ref('') // STH gateway address to send STH for SELL
const gwLoading = ref(false)

async function loadGateways() {
  gwLoading.value = true
  try {
    const [bd, sg] = await Promise.all([
      walletInvoke<string>('get_buy_deposit_address').catch(() => ''),
      walletInvoke<string>('get_sell_gate_address').catch(() => ''),
    ])
    buyDeposit.value = bd
    sellGate.value = sg
  } finally {
    gwLoading.value = false
  }
}
onMounted(loadGateways)
// re-fetch the per-account BEP20 deposit address when the active account changes
watch(() => wallet.address, loadGateways)

// the address surfaced for the active tab (copy target).
const activeGateway = computed(() => (tab.value === 'buy' ? buyDeposit.value : sellGate.value))

/* ---- live pool quote (slippage-aware, volume-dependent) ---- */
const quote = ref<SwapQuote | null>(null)
const quoting = ref(false)
let quoteTimer: ReturnType<typeof setTimeout> | null = null

/* ---- BUY ---- */
const buyAmount = ref('')
const buyNum = computed(() => Number(buyAmount.value) || 0)

/* ---- SELL ---- */
const sellAmount = ref('')
const usdtAddr = ref('')
const sellNum = computed(() => Number(sellAmount.value) || 0)
const sellValid = computed(() => sellNum.value > 0 && /^0x[0-9a-fA-F]{40}$/.test(usdtAddr.value.trim()))

// The pool endpoint is priced by USDT volume; estimate it from the active side.
const activeSth = computed(() => (tab.value === 'buy' ? buyNum.value : sellNum.value))
const estimatedUsdt = computed(() => activeSth.value * (wallet.rate || 0))

// pool slippage ratio (min / estimated) — falls back to a flat 5% buffer.
const slipRatio = computed(() =>
  quote.value && quote.value.estimatedSthToReceive > 0
    ? quote.value.minSthToReceive / quote.value.estimatedSthToReceive
    : 0.95,
)
// effective USDT-per-STH for the current volume (incl. price impact).
const effectivePrice = computed(() => quote.value?.effectivePrice || wallet.rate)
// ideal pool rate (USDT received per STH sold) before slippage.
const idealRate = computed(() =>
  quote.value && quote.value.estimatedSthToReceive > 0
    ? quote.value.usdtAmount / quote.value.estimatedSthToReceive
    : wallet.rate,
)
const priceImpact = computed(() => {
  const spot = wallet.rate || 0
  if (!spot || !quote.value) return 0
  return Math.abs((effectivePrice.value - spot) / spot) * 100
})

/* BUY readouts */
const toPay = computed(() => (buyNum.value * effectivePrice.value).toFixed(2))
const buyReceive = computed(() => buyNum.value.toFixed(4))
const buyMin = computed(() => (buyNum.value * slipRatio.value).toFixed(4))

/* SELL readouts */
const sellReceive = computed(() => (sellNum.value * idealRate.value).toFixed(2))
const sellMin = computed(() => (sellNum.value * idealRate.value * slipRatio.value).toFixed(2))

const rateStr = computed(() => (wallet.rate || 0).toFixed(6))
const effStr = computed(() => effectivePrice.value.toFixed(6))

watch(estimatedUsdt, (usdt) => {
  if (quoteTimer) clearTimeout(quoteTimer)
  if (!usdt || usdt <= 0) {
    quote.value = null
    return
  }
  quoteTimer = setTimeout(async () => {
    quoting.value = true
    try {
      quote.value = await walletInvoke<SwapQuote>('get_swap_quote', { usdtAmount: usdt })
    } catch {
      quote.value = null
    } finally {
      quoting.value = false
    }
  }, 350)
})

async function copyGateway() {
  if (!activeGateway.value) return
  try {
    await navigator.clipboard.writeText(activeGateway.value)
  } catch {
    /* noop */
  }
  copiedGw.value = true
  setTimeout(() => (copiedGw.value = false), 1500)
}

async function iHavePaid() {
  if (buyNum.value <= 0) return
  await wallet.notifyExchangeDeposit(buyAmount.value)
}
async function sell() {
  if (!sellValid.value) return
  await wallet.executeExchangeSell(sellAmount.value, usdtAddr.value.trim())
}

/* ---- bridge settlement status (driven by the store, survives remounts) ---- */
const MAX_BRIDGE_TRIES = 30

</script>

<template>
  <div class="swap" data-testid="wallet-swap">
    <div class="tabs">
      <button class="tab" :class="{ on: tab === 'buy' }" @click="tab = 'buy'" data-testid="swap-tab-buy">
        {{ t('wallet.swap.buy') }}
      </button>
      <button class="tab" :class="{ on: tab === 'sell' }" @click="tab = 'sell'" data-testid="swap-tab-sell">
        {{ t('wallet.swap.sell') }}
      </button>
    </div>

    <div class="rate">
      <span class="dim">{{ t('wallet.swap.liveRate') }}</span>
      <span class="mono hl">1 STH = ${{ rateStr }}</span>
    </div>

    <div
      v-if="quote && estimatedUsdt > 0"
      class="rate sub pool"
      data-testid="swap-pool-price"
    >
      <span class="dim">{{ t('wallet.swap.poolPrice') }}</span>
      <span class="mono" :class="{ impacted: priceImpact > 2 }">
        ${{ effStr }} · {{ t('wallet.swap.impact') }} {{ priceImpact.toFixed(2) }}%
      </span>
    </div>

    <div
      v-if="wallet.bridge !== 'idle'"
      class="rate sub bridge"
      data-testid="swap-bridge-status"
    >
      <span class="dim">{{ t('wallet.swap.bridge') }}</span>
      <span class="mono" :class="{ ok: wallet.bridge === 'confirmed' }">
        {{
          wallet.bridge === 'confirmed'
            ? t('wallet.swap.bridgeConfirmed')
            : t('wallet.swap.bridgePending', { n: wallet.bridgeTries, max: MAX_BRIDGE_TRIES })
        }}
      </span>
    </div>

    <!-- BUY -->
    <template v-if="tab === 'buy'">
      <label class="fld">
        <span class="lbl">{{ t('wallet.swap.amountBuy') }}</span>
        <input v-model="buyAmount" class="inp mono" type="number" min="0" placeholder="0.0000" data-testid="swap-buy-amount" />
      </label>
      <div class="readouts">
        <div class="ro"><span class="dim">{{ t('wallet.swap.toPay') }}</span><span class="mono">${{ toPay }}</span></div>
        <div class="ro"><span class="dim">{{ t('wallet.swap.youReceive') }}</span><span class="mono">{{ buyReceive }} STH</span></div>
        <div class="ro"><span class="dim">{{ t('wallet.swap.minGuaranteed') }}</span><span class="mono">{{ buyMin }} STH</span></div>
      </div>
      <div class="notice">
        <p class="notice-txt">{{ t('wallet.swap.notice') }}</p>
        <button class="gw mono" @click="copyGateway" data-testid="swap-gateway-copy">
          {{ copiedGw ? t('wallet.receive.copied') : buyDeposit || t('wallet.swap.loadingGateway') }}
        </button>
        <span class="min-dep dim">Min. deposit: 5.00 USDT (BEP20)</span>
      </div>
      <p v-if="wallet.error" class="err">⚠ {{ wallet.error }}</p>
      <button class="btn btn-primary cta" :disabled="buyNum <= 0 || wallet.busy" @click="iHavePaid" data-testid="swap-ipaid-btn">
        {{ t('wallet.swap.iPaid') }}
      </button>
    </template>

    <!-- SELL -->
    <template v-else>
      <div class="rate sub">
        <span class="dim">{{ t('wallet.swap.yourBalance') }}</span>
        <span class="mono">{{ wallet.balance }} STH</span>
      </div>
      <label class="fld">
        <span class="lbl">{{ t('wallet.swap.amountSell') }}</span>
        <input v-model="sellAmount" class="inp mono" type="number" min="0" placeholder="0.0000" data-testid="swap-sell-amount" />
      </label>
      <label class="fld">
        <span class="lbl">{{ t('wallet.swap.usdtAddress') }}</span>
        <input v-model="usdtAddr" class="inp mono" placeholder="0x..." spellcheck="false" data-testid="swap-usdt-address" />
      </label>
      <div class="readouts">
        <div class="ro"><span class="dim">{{ t('wallet.swap.youReceiveUsdt') }}</span><span class="mono">${{ sellReceive }}</span></div>
        <div class="ro"><span class="dim">{{ t('wallet.swap.minGuaranteed') }}</span><span class="mono">${{ sellMin }}</span></div>
        <div class="ro"><span class="dim">{{ t('wallet.swap.networkFee') }}</span><span class="mono">{{ FEE.toFixed(2) }} STH</span></div>
        <div class="ro"><span class="dim">{{ t('wallet.swap.gateway') }}</span><span class="mono ellipsis" data-testid="swap-sell-gateway">{{ sellGate || t('wallet.swap.loadingGateway') }}</span></div>
      </div>
      <p v-if="wallet.error" class="err">⚠ {{ wallet.error }}</p>
      <HoldButton
        :label="wallet.busy ? t('wallet.swap.selling') : t('wallet.swap.holdSell')"
        :holding-label="t('wallet.send.holding')"
        :disabled="!sellValid || wallet.busy"
        danger
        testid="swap-sell-hold-btn"
        @complete="sell"
      />
    </template>
  </div>
</template>

<style scoped>
.swap {
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.tabs {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 6px;
  background: var(--panel);
  padding: 4px;
  border-radius: var(--radius);
  border: 1px solid var(--border);
}
.tab {
  padding: 9px;
  font-size: 11px;
  font-weight: var(--fw-800);
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text-dim);
  border-radius: var(--radius);
}
.tab.on {
  color: var(--accent);
  background: var(--accent-soft);
}
.rate {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 12px;
  padding: 10px 12px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.rate.sub {
  background: transparent;
}
.rate.pool {
  padding: 6px 12px;
  font-size: 11px;
}
.impacted {
  color: var(--danger);
}
.bridge .ok {
  color: #2ecc71;
}
.fld {
  display: flex;
  flex-direction: column;
  gap: 7px;
}
.lbl {
  font-size: 10px;
  letter-spacing: 0.12em;
  color: var(--text-dim);
  text-transform: uppercase;
  font-weight: var(--fw-700);
}
.inp {
  width: 100%;
  padding: 12px;
  font-size: 13px;
  color: var(--text);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.inp:focus {
  outline: none;
  border-color: var(--accent);
}
.readouts {
  display: flex;
  flex-direction: column;
  gap: 7px;
  padding: 12px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.ro {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 12px;
  gap: 10px;
}
.ellipsis {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 180px;
}
.notice {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px;
  border: 1px dashed var(--border-accent);
  border-radius: var(--radius);
  background: var(--accent-soft);
}
.notice-txt {
  font-size: 11px;
  letter-spacing: 0.06em;
  color: var(--accent);
  font-weight: var(--fw-700);
}
.gw {
  font-size: 11px;
  word-break: break-all;
  text-align: left;
  padding: 8px;
  color: var(--text);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.min-dep {
  font-size: 10px;
}
.mono {
  font-family: var(--font-mono, monospace);
}
.cta {
  width: 100%;
  padding: 13px;
}
.err {
  color: var(--danger);
  font-size: 12px;
}
</style>
