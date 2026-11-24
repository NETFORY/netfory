<script setup lang="ts">
import { computed, defineEmits, defineProps, onMounted, ref } from 'vue'
import { openExplorerTx } from '@/lib/explorer'
import { errKey, fetchOrders, fetchTrades, fromUnits, orderCost, perToken, toUnits, tokenOp, tradeMs, type TokenConfig, type TokenInfo, type TokenOrder, type TokenTrade } from './api'
import { useTokT } from './t'
import HoldButton from './HoldButton.vue'
import TokenSummary from './TokenSummary.vue'

// Рынок токена: дневная цена, открытые ордера на продажу, последние сделки; продать / купить / отменить свой ордер.
const props = defineProps<{ info: TokenInfo; cfg: TokenConfig; address: string }>()
const emit = defineEmits<{ (e: 'done', id: string): void; (e: 'mode', title: string): void }>()
const tt = useTokT()
const m = computed(() => (props.cfg.market?.enabled ? props.cfg.market : null))
const dec = computed(() => props.info.decimals)
const mode = ref<'list' | 'sell' | 'buy' | 'cancel'>('list')
const orders = ref<TokenOrder[] | null>(null)
const trades = ref<TokenTrade[]>([])
const mine = ref(false)
const sel = ref<TokenOrder | null>(null)
const amount = ref('')
const price = ref('')
const busy = ref(false)
const err = ref('')

async function load() {
  orders.value = null
  const [o, t] = await Promise.all([fetchOrders(props.info.id).catch(() => []), fetchTrades(props.info.id).catch(() => [])])
  orders.value = o.sort((a, b) => (perToken(a, dec.value) < perToken(b, dec.value) ? -1 : 1))
  trades.value = t.sort((a, b) => tradeMs(b) - tradeMs(a))
}
onMounted(load)

// Дневная цена: последняя цена дня + объём; линия по дням.
const days = computed(() => {
  const by = new Map<string, { price: bigint; vol: bigint }>()
  for (const t of [...trades.value].reverse()) {
    const d = new Date(tradeMs(t)).toISOString().slice(0, 10)
    const cur = by.get(d) ?? { price: 0n, vol: 0n }
    by.set(d, { price: BigInt(t.pricePerToken || '0'), vol: cur.vol + BigInt(t.amount || '0') })
  }
  return [...by.entries()].map(([day, v]) => ({ day, ...v }))
})
const last = computed(() => days.value[days.value.length - 1])
const spark = computed(() => {
  const d = days.value.slice(-30)
  if (d.length < 2) return ''
  const ps = d.map((x) => Number(x.price))
  const lo = Math.min(...ps), hi = Math.max(...ps), span = hi - lo || 1
  return ps.map((p, i) => `${((i / (ps.length - 1)) * 300).toFixed(1)},${(52 - ((p - lo) / span) * 46).toFixed(1)}`).join(' ')
})
const shown = computed(() => (orders.value ?? []).filter((o) => !mine.value || o.seller === props.address))
const myOpen = computed(() => (orders.value ?? []).filter((o) => o.seller === props.address).length)

function go(next: 'sell' | 'buy' | 'cancel', o: TokenOrder | null = null) {
  sel.value = o
  amount.value = next === 'buy' && o ? fromUnits(o.remaining, dec.value).replace(/,/g, '') : ''
  price.value = ''
  err.value = ''
  mode.value = next
  emit('mode', tt(next === 'sell' ? 'mSellTitle' : next === 'buy' ? 'mBuyTitle' : 'mCancelTitle'))
}
function back() { mode.value = 'list'; err.value = ''; emit('mode', tt('mTitle')) }

const units = computed(() => toUnits(amount.value || '0', dec.value) || null)
const priceU = computed(() => toUnits(price.value || '0', 8) || null)
const step = computed(() => (m.value && !m.value.pricePerToken ? 10n ** BigInt(dec.value) : 1n))
const scale = computed(() => 10n ** BigInt(dec.value))
const sellTotal = computed(() => (units.value && priceU.value ? (units.value * priceU.value + scale.value - 1n) / scale.value : 0n))
const sellErr = computed(() => {
  if (units.value && units.value > BigInt(props.info.balance)) return tt('errBalance')
  if (priceU.value && priceU.value % step.value !== 0n) return tt('mErrStep', { s: fromUnits(step.value, 8) })
  if (m.value && myOpen.value >= m.value.maxOpenOrders) return tt('mErrTooMany')
  return ''
})
const buyCost = computed(() => (sel.value && units.value ? orderCost(sel.value, units.value) : 0n))
const buyErr = computed(() => (sel.value && units.value && units.value > BigInt(sel.value.remaining) ? tt('mErrAmount') : ''))

async function fire(kind: 'sell' | 'sell_v2' | 'buy' | 'cancel', asset: Record<string, unknown>, fee: string) {
  busy.value = true
  err.value = ''
  try {
    emit('done', await tokenOp(kind, asset, fee))
  } catch (e) {
    const k = errKey(e)
    err.value = k.key ? tt(k.key) : k.raw
  } finally {
    busy.value = false
  }
}
function doSell() {
  if (!m.value || !units.value || !priceU.value) return
  const v2 = m.value.pricePerToken
  fire(v2 ? 'sell_v2' : 'sell', { id: props.info.id, amount: units.value.toString(), price: (v2 ? priceU.value : priceU.value / scale.value).toString() }, m.value.fees.sell)
}
const doBuy = () => m.value && sel.value && units.value && fire('buy', { id: props.info.id, orderId: sel.value.orderId, amount: units.value.toString() }, m.value.fees.buy)
const doCancel = () => m.value && sel.value && fire('cancel', { id: props.info.id, orderId: sel.value.orderId }, m.value.fees.cancel)
const sym = computed(() => props.info.symbol)
const short = (a: string) => (a.length > 14 ? `${a.slice(0, 5)}…${a.slice(-5)}` : a)
function ago(ms: number) {
  const s = Math.max(1, Math.round((Date.now() - ms) / 1000))
  const r = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' })
  return s < 3600 ? r.format(-Math.round(s / 60), 'minute') : s < 86400 ? r.format(-Math.round(s / 3600), 'hour') : r.format(-Math.round(s / 86400), 'day')
}
</script>

<template>
  <div class="mk" data-testid="token-market">
    <p v-if="!m" class="tk-warn" data-testid="market-disabled">{{ tt('mDisabled') }}</p>

    <template v-else-if="mode === 'list'">
      <button class="mk-sell" :disabled="info.balance === '0' || !cfg.enabled" @click="go('sell')" data-testid="market-sell-btn">+ {{ tt('mPlaceSell') }}</button>

      <div class="mk-day" data-testid="market-daily">
        <div class="mk-day-top">
          <div><small>{{ tt('mDaily') }}</small><b class="mono">{{ last ? fromUnits(last.price, 8) : '0' }} <span>STH/{{ sym }}</span></b></div>
          <div class="r"><small class="mono">{{ last?.day ?? '—' }}</small><span>Vol <b class="mono">{{ last ? fromUnits(last.vol, dec) : '0' }}</b></span></div>
        </div>
        <svg v-if="spark" viewBox="0 0 300 56" preserveAspectRatio="none" class="mk-spark"><polyline :points="spark" /></svg>
        <p v-else class="mk-note">{{ tt(days.length ? 'mOneDay' : 'mNoTradesYet') }}</p>
      </div>

      <div class="mk-hd">
        <span class="tk-h">{{ tt('mOrders') }}</span>
        <span><a :class="{ on: mine }" @click="mine = !mine" data-testid="market-my-orders">{{ tt('mMyOrders') }} →</a><a class="mk-rf" @click="load" data-testid="market-refresh">↻</a></span>
      </div>
      <p v-if="!orders" class="tk-dim">{{ tt('loading') }}</p>
      <div v-else class="mk-orders" data-testid="market-orders">
        <div v-for="o in shown" :key="o.orderId" class="mk-order" data-testid="market-order-row">
          <div>
            <b class="mono">{{ fromUnits(perToken(o, dec), 8) }} <span>STH/{{ sym }}</span></b>
            <small>{{ tt('mRemaining') }} <span class="mono">{{ fromUnits(o.remaining, dec) }}</span> · <span :class="{ me: o.seller === address }">{{ o.seller === address ? tt('mYou') : short(o.seller) }}</span></small>
          </div>
          <button v-if="o.seller === address" class="mk-btn danger" @click="go('cancel', o)" data-testid="market-cancel-btn">{{ tt('cancel') }}</button>
          <button v-else class="mk-btn" :disabled="!cfg.enabled" @click="go('buy', o)" data-testid="market-buy-btn">{{ tt('mBuy') }}</button>
        </div>
        <p v-if="!shown.length" class="mk-empty" data-testid="market-empty">{{ tt(mine ? 'mNoMine' : 'mNoOrders') }}</p>
      </div>

      <span class="tk-h">{{ tt('mTrades') }}</span>
      <div class="mk-trades" data-testid="market-trades">
        <div v-for="t in trades.slice(0, 10)" :key="t.txId" @click="openExplorerTx(t.txId)">
          <span class="mono">{{ fromUnits(t.amount, dec) }} {{ sym }} × {{ t.pricePerToken && t.pricePerToken !== '0' ? fromUnits(t.pricePerToken, 8) : '—' }} STH</span>
          <span class="tk-dim">{{ ago(tradeMs(t)) }}</span>
        </div>
        <p v-if="!trades.length" class="mk-empty">{{ tt('mNoTrades') }}</p>
      </div>
      <a class="tk-explorer" @click="openExplorerTx(info.id)" data-testid="market-explorer">{{ tt('mExplorer') }} ↗</a>
    </template>

    <template v-else>
      <a class="mk-back" @click="back" data-testid="market-back">← {{ tt('mOrders') }}</a>

      <template v-if="mode === 'sell'">
        <label class="tk-field"><span class="tk-lbl">{{ tt('amount') }} ({{ sym }}) <a class="taf-max" @click.prevent="amount = fromUnits(info.balance, dec).replace(/,/g, '')">MAX</a></span>
          <input v-model="amount" inputmode="decimal" placeholder="0" data-testid="market-sell-amount" />
          <small class="tk-dim">{{ tt('available', { n: fromUnits(info.balance, dec) }) }}</small>
        </label>
        <label class="tk-field"><span class="tk-lbl">{{ tt('mPrice') }} (STH / 1 {{ sym }})</span>
          <input v-model="price" inputmode="decimal" placeholder="0" data-testid="market-sell-price" />
          <small v-if="step > 1n" class="tk-dim">{{ tt('mStep', { s: fromUnits(step, 8) }) }}</small>
        </label>
        <p class="tk-dim">{{ tt('mLocked') }}</p>
        <TokenSummary :rows="[[tt('amount'), `${amount || '0'} ${sym}`], [tt('mPrice'), `${price || '0'} STH`], [tt('total'), `${fromUnits(sellTotal, 8)} STH`]]" :fee="m.fees.sell" />
        <p v-if="sellErr || err" class="taf-err" data-testid="market-error">{{ err || sellErr }}</p>
        <HoldButton :label="tt('mHoldSell')" :disabled="!units || !priceU || !!sellErr || !cfg.enabled" :busy="busy" @confirm="doSell" data-testid="market-sell-hold" />
      </template>

      <template v-else-if="mode === 'buy' && sel">
        <div class="mk-kv"><span>{{ tt('mSeller') }}</span><b class="mono">{{ short(sel.seller) }}</b></div>
        <label class="tk-field"><span class="tk-lbl">{{ tt('amount') }} ({{ sym }}) <a class="taf-max" @click.prevent="amount = fromUnits(sel.remaining, dec).replace(/,/g, '')">MAX</a></span>
          <input v-model="amount" inputmode="decimal" data-testid="market-buy-amount" />
          <small class="tk-dim">{{ tt('mRemaining') }}: {{ fromUnits(sel.remaining, dec) }} · {{ fromUnits(perToken(sel, dec), 8) }} STH/{{ sym }}</small>
        </label>
        <TokenSummary :rows="[[tt('mYouGet'), `${amount || '0'} ${sym}`], [tt('mYouPay'), `${fromUnits(buyCost, 8)} STH`]]" :fee="m.fees.buy" :spend="buyCost" />
        <p v-if="buyErr || err" class="taf-err" data-testid="market-error">{{ err || buyErr }}</p>
        <HoldButton :label="tt('mHoldBuy')" :disabled="!units || !!buyErr || !cfg.enabled" :busy="busy" @confirm="doBuy" data-testid="market-buy-hold" />
      </template>

      <template v-else-if="mode === 'cancel' && sel">
        <p class="tk-dim">{{ tt('mCancelHint') }}</p>
        <TokenSummary :rows="[[tt('mRemaining'), `${fromUnits(sel.remaining, dec)} ${sym}`], [tt('mPrice'), `${fromUnits(perToken(sel, dec), 8)} STH`]]" :fee="m.fees.cancel" />
        <p v-if="err" class="taf-err" data-testid="market-error">{{ err }}</p>
        <HoldButton :label="tt('mHoldCancel')" :busy="busy" @confirm="doCancel" data-testid="market-cancel-hold" />
      </template>
    </template>
  </div>
</template>

<style scoped>
.mk { display: flex; flex-direction: column; gap: 12px; }
.mk-sell { padding: 13px; border-radius: 10px; border: 1.5px solid var(--accent); background: color-mix(in srgb, var(--accent) 14%, transparent); color: var(--accent); font: inherit; font-size: 12px; font-weight: 800; letter-spacing: 0.08em; text-transform: uppercase; cursor: pointer; transition: background-color 0.15s; }
.mk-sell:hover:not(:disabled) { background: color-mix(in srgb, var(--accent) 22%, transparent); }
.mk-sell:disabled { opacity: 0.4; cursor: default; }
.mk-day { padding: 12px 14px; border-radius: 12px; border: 1px solid var(--border); display: flex; flex-direction: column; gap: 10px; }
.mk-day-top { display: flex; justify-content: space-between; gap: 10px; }
.mk-day-top > div { display: flex; flex-direction: column; gap: 3px; }
.mk-day-top .r { align-items: flex-end; font-size: 11px; color: var(--text-dim); }
.mk-day small { font-size: 10.5px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--text-dim); }
.mk-day b { font-size: 15px; }
.mk-day b span { font-size: 10.5px; color: var(--text-dim); }
.mk-spark { width: 100%; height: 56px; }
.mk-spark polyline { fill: none; stroke: var(--accent); stroke-width: 2; vector-effect: non-scaling-stroke; }
.mk-note { margin: 0; padding-top: 10px; border-top: 1px solid var(--border); font-size: 11px; color: var(--text-dim); text-align: center; line-height: 1.5; }
.mk-hd { display: flex; justify-content: space-between; align-items: center; }
.mk-hd a { font-size: 10.5px; font-weight: 800; letter-spacing: 0.08em; text-transform: uppercase; color: var(--accent); cursor: pointer; opacity: 0.75; }
.mk-hd a.on { opacity: 1; text-decoration: underline; }
.mk-rf { margin-left: 12px; }
.mk-orders { display: flex; flex-direction: column; gap: 6px; }
.mk-order { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 10px 12px; border-radius: 10px; border: 1px solid var(--border); }
.mk-order > div { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
.mk-order b { font-size: 13px; }
.mk-order b span { font-size: 10px; color: var(--text-dim); }
.mk-order small { font-size: 10.5px; color: var(--text-dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.mk-order .me { color: var(--accent); }
.mk-btn { flex-shrink: 0; padding: 8px 14px; border-radius: 8px; border: 1px solid var(--accent); background: color-mix(in srgb, var(--accent) 12%, transparent); color: var(--accent); font: inherit; font-size: 10.5px; font-weight: 800; letter-spacing: 0.08em; text-transform: uppercase; cursor: pointer; }
.mk-btn.danger { border-color: #f87171; color: #f87171; background: transparent; }
.mk-btn:disabled { opacity: 0.4; }
.mk-empty { margin: 0; padding: 6px 0; font-size: 11.5px; color: var(--text-dim); text-align: center; }
.mk-trades { display: flex; flex-direction: column; }
.mk-trades > div { display: flex; justify-content: space-between; gap: 10px; padding: 7px 0; font-size: 11.5px; border-bottom: 1px solid var(--border); cursor: pointer; }
.mk-trades > div:last-of-type { border-bottom: none; }
.mk-back { align-self: flex-start; font-size: 10.5px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--text-dim); cursor: pointer; }
.mk-back:hover { color: var(--text); }
.mk-kv { display: flex; justify-content: space-between; padding: 10px 12px; border-radius: 10px; border: 1px solid var(--border); font-size: 11.5px; }
.mk-kv span { color: var(--text-dim); }
.taf-max { cursor: pointer; color: var(--accent); font-weight: 800; }
</style>
