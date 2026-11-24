<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useWalletStore } from '@/store/wallet'
import { openExplorerWallet } from '@/lib/explorer'
import { cachedInfo, fromUnits, lastPrice, listTokens, tokenConfig, type TokenConfig, type TokenRow } from '@/wallet/tokens/api'
import { useTokT } from '@/wallet/tokens/t'
import TokenLogo from '@/wallet/tokens/TokenLogo.vue'
import TokenDetail from '@/wallet/tokens/TokenDetail.vue'
import TokenWizard from '@/wallet/tokens/TokenWizard.vue'

// Раздел «Токены» кошелька: список → экран токена | мастер создания.
const wallet = useWalletStore()
const tt = useTokT()
const address = computed(() => wallet.address)
const cfg = ref<TokenConfig | null>(null)
const rows = ref<TokenRow[] | null>(null)
const err = ref('')
const screen = ref<'list' | 'detail' | 'create'>('list')
const sel = ref<TokenRow | null>(null)
const pendingSym = computed(() => { try { return JSON.parse(localStorage.getItem(`sth_tok_wizard:${address.value}`) || 'null')?.symbol || '' } catch { return '' } })

// Последняя цена рынка по каждому токену → стоимость баланса в STH.
const prices = ref<Record<string, bigint | null>>({})
function loadPrices(r: TokenRow[]) { for (const x of r) lastPrice(x.id).then((p) => (prices.value = { ...prices.value, [x.id]: p })) }
const value = (r: TokenRow) => { const p = prices.value[r.id]; return p ? (BigInt(r.balance) * p) / 10n ** BigInt(r.decimals) : null }
const totalSth = computed(() => (rows.value ?? []).reduce((a, r) => a + (value(r) ?? 0n), 0n))
async function load() {
  if (!address.value) return
  err.value = ''
  try {
    const [c, r] = await Promise.all([cfg.value ? Promise.resolve(cfg.value) : tokenConfig(), listTokens(address.value)])
    cfg.value = c
    rows.value = r
    loadPrices(r)
    if (sel.value) sel.value = r.find((x) => x.id === sel.value!.id) ?? sel.value
  } catch (e) {
    err.value = String((e as any)?.message ?? e)
    rows.value ??= []
  }
}
onMounted(load)
watch(address, () => { screen.value = 'list'; sel.value = null; rows.value = null; load() })
function open(r: TokenRow) { sel.value = r; screen.value = 'detail' }
function created(id: string, symbol: string) {
  sel.value = { id, symbol, decimals: 8, balance: '0', owner: true }
  screen.value = 'list'
  load().then(() => { const r = rows.value?.find((x) => x.id === id); if (r) open(r) })
}
const metaName = (r: TokenRow) => cachedInfo(r.id)?.meta?.name || ''
</script>

<template>
  <div class="tk-root" data-testid="wallet-tokens-view">
    <TokenDetail v-if="screen === 'detail' && sel && cfg" :row="sel" :cfg="cfg" :address="address" @back="screen = 'list'" @changed="load" />
    <TokenWizard v-else-if="screen === 'create' && cfg" :cfg="cfg" :address="address" @close="screen = 'list'; load()" @created="created" />
    <template v-else>
      <div class="tk-top">
        <div><h3>{{ tt('title') }}</h3><small>{{ wallet.network }} · {{ tt('count', { n: rows?.length ?? 0 }) }}</small></div>
        <button class="tk-create" :disabled="!cfg" @click="screen = 'create'" data-testid="tokens-create-btn">＋ {{ pendingSym ? tt('resume', { s: pendingSym }) : tt('create') }}</button>
      </div>
      <p v-if="cfg && !cfg.enabled" class="tk-warn" data-testid="token-inactive">{{ tt('inactive') }}</p>
      <p v-if="err" class="taf-err" data-testid="tokens-error">{{ err }}</p>
      <p v-if="rows === null" class="tk-dim" data-testid="tokens-loading">{{ tt('loading') }}</p>
      <div v-else-if="!rows.length" class="tk-empty" data-testid="tokens-empty">
        <b>{{ tt('empty') }}</b><small>{{ tt('emptyHint') }}</small>
      </div>
      <h4 v-else class="tk-h tk-h-row">{{ tt('myTokens') }}<span v-if="totalSth" data-testid="tokens-total-value">≈ {{ fromUnits(totalSth, 8) }} STH</span></h4>
      <ul v-if="rows?.length" class="tk-list" data-testid="tokens-list">
        <li v-for="(r, i) in rows" :key="r.id" :style="{ animationDelay: `${i * 40}ms` }" @click="open(r)" :data-testid="`token-row-${r.symbol}`">
          <TokenLogo :id="r.id" :symbol="r.symbol" :info="cachedInfo(r.id)" />
          <div class="tk-li-main"><b>{{ r.symbol }} <em v-if="r.owner" class="tk-badge">{{ tt('owner') }}</em></b><small>{{ metaName(r) }}</small></div>
          <div class="tk-li-bal"><b>{{ fromUnits(r.balance, r.decimals) }}</b><small v-if="prices[r.id]" :data-testid="`token-row-value-${r.symbol}`">≈ {{ fromUnits(value(r) ?? 0n, 8) }} STH</small><small v-if="prices[r.id]" class="px">@ {{ fromUnits(prices[r.id]!, 8) }}</small></div>
        </li>
      </ul>
      <a class="tk-explorer" @click="openExplorerWallet(address)" data-testid="tokens-explorer">{{ tt('allInExplorer') }} ↗</a>
    </template>
  </div>
</template>

<style>
.tk-root { display: flex; flex-direction: column; gap: 12px; padding: 4px 2px 12px; }
.tk-top { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
.tk-top h3 { margin: 0; font-size: 18px; }
.tk-top small { font-size: 10.5px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--text-dim); }
.tk-create { padding: 11px 14px; border-radius: 10px; border: 1.5px solid var(--accent); background: color-mix(in srgb, var(--accent) 10%, transparent); color: var(--accent); font: inherit; font-size: 11.5px; font-weight: 800; letter-spacing: 0.06em; text-transform: uppercase; cursor: pointer; white-space: nowrap; max-width: 60%; overflow: hidden; text-overflow: ellipsis; transition: background-color 0.15s, transform 0.15s; }
.tk-create:hover:not(:disabled) { background: color-mix(in srgb, var(--accent) 20%, transparent); transform: translateY(-1px); }
.tk-create:disabled { opacity: 0.4; }
.tk-h { margin: 6px 0 0; font-size: 11px; font-weight: 800; letter-spacing: 0.1em; text-transform: uppercase; color: var(--accent); }
.tk-list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 8px; }
.tk-list li { display: flex; align-items: center; gap: 12px; padding: 12px; border-radius: 10px; border: 1px solid var(--border); background: var(--surface); cursor: pointer; transition: border-color 0.15s, transform 0.15s; animation: tk-in 0.25s ease-out both; }
.tk-list li:hover { border-color: var(--accent); transform: translateX(2px); }
.tk-li-main, .tk-li-bal { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
.tk-li-main { flex: 1; }
.tk-li-main b { font-size: 14px; font-family: var(--font-mono); display: flex; align-items: center; gap: 6px; }
.tk-li-main small, .tk-li-bal small { font-size: 11px; color: var(--text-dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tk-li-bal { align-items: flex-end; font-family: var(--font-mono); font-size: 14px; }
.tk-li-bal small { font-size: 10.5px; }
.tk-li-bal small.px { opacity: 0.7; }
.tk-h-row { display: flex; justify-content: space-between; }
.tk-h-row span { color: var(--text-dim); font-family: var(--font-mono); letter-spacing: 0; text-transform: none; }
.tk-px { font-size: 11px; color: var(--text-dim); font-family: var(--font-mono); }
.tk-badge { font-style: normal; font-size: 9.5px; font-weight: 800; letter-spacing: 0.08em; text-transform: uppercase; padding: 2px 7px; border-radius: 4px; border: 1px solid var(--accent); background: color-mix(in srgb, var(--accent) 15%, transparent); color: var(--accent); }
.tk-empty { display: flex; flex-direction: column; gap: 6px; padding: 28px 16px; text-align: center; border-radius: 12px; border: 1px dashed var(--border); }
.tk-empty small, .tk-dim, .tk-hint { font-size: 11.5px; color: var(--text-dim); margin: 0; }
.tk-warn { margin: 0; font-size: 11.5px; color: #f5c451; padding: 8px 10px; border-radius: 9px; background: rgba(245, 196, 81, 0.08); }
.tk-ok { margin: 0; font-size: 11.5px; color: #4ade80; }
.tk-explorer { align-self: center; font-size: 11px; font-weight: 700; letter-spacing: 0.08em; text-transform: uppercase; color: #f5c451; cursor: pointer; }
.tk-detail, .tk-wizard { display: flex; flex-direction: column; gap: 12px; animation: tk-in 0.2s ease-out; }
.tk-bar-head { display: grid; grid-template-columns: 40px 1fr auto; align-items: center; min-height: 34px; }
.tk-bar-head b { text-align: center; font-size: 12px; letter-spacing: 0.14em; text-transform: uppercase; }
.tk-bar-head button { background: none; border: none; color: var(--text); font-size: 22px; cursor: pointer; justify-self: start; padding: 0 6px; }
.tk-bar-head button:last-child { justify-self: end; font-size: 16px; color: var(--accent); }
.tk-bar-head button.spin { animation: tk-rot 0.8s linear infinite; }
.tk-restart { font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--text-dim); cursor: pointer; }
.tk-restart:hover { color: var(--text); }
.tk-hero { display: flex; align-items: center; gap: 16px; }
.tk-hero-txt { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.tk-hero h3 { margin: 0; font-size: 18px; display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.tk-sym { font-size: 11px; color: var(--text-dim); font-family: var(--font-mono); letter-spacing: 0.06em; }
.tk-bal b { font-size: 22px; font-family: var(--font-mono); }
.tk-bal span { font-size: 12px; color: var(--text-dim); font-family: var(--font-mono); }
.tk-acts { display: grid; grid-template-columns: repeat(auto-fit, minmax(90px, 1fr)); gap: 8px; }
.tk-acts button { padding: 12px 6px; border-radius: 9px; border: 1px solid var(--border); background: var(--surface); color: var(--text); font: inherit; font-size: 11.5px; font-weight: 800; letter-spacing: 0.06em; text-transform: uppercase; cursor: pointer; transition: border-color 0.15s, transform 0.15s; }
.tk-acts button:hover:not(:disabled) { border-color: var(--accent); transform: translateY(-1px); }
.tk-acts button:disabled { opacity: 0.4; cursor: default; }
.tk-acts .pri { border-color: var(--accent); color: var(--accent); background: color-mix(in srgb, var(--accent) 10%, transparent); }
.tk-acts .gold { border-color: color-mix(in srgb, #f5c451 50%, transparent); color: #f5c451; }
.tk-acts.three { grid-template-columns: repeat(3, 1fr); }
.tk-acts .mkt { border-color: color-mix(in srgb, var(--accent) 55%, transparent); color: var(--accent); }
.tk-burn { padding: 12px; border-radius: 9px; border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent); background: transparent; color: var(--accent); font: inherit; font-size: 11.5px; font-weight: 800; letter-spacing: 0.08em; text-transform: uppercase; cursor: pointer; transition: border-color 0.15s; }
.tk-burn:hover:not(:disabled) { border-color: var(--accent); }
.tk-burn:disabled { opacity: 0.4; cursor: default; }
.tk-owner { display: flex; flex-direction: column; gap: 10px; padding: 12px; border-radius: 12px; border: 1px solid color-mix(in srgb, var(--accent) 35%, transparent); }
.tk-owner .tk-h { margin: 0; }
.tk-info { display: flex; flex-direction: column; gap: 10px; padding: 14px; border-radius: 12px; border: 1px solid var(--border); background: var(--surface); }
.tk-desc { margin: 0; font-size: 12.5px; line-height: 1.55; }
.tk-site { font-size: 12px; color: var(--accent); word-break: break-all; }
.tk-kv { display: flex; justify-content: space-between; align-items: baseline; gap: 10px; }
.tk-kv span { font-size: 10.5px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--text-dim); white-space: nowrap; }
.tk-kv b, .tk-kv a { font-size: 12.5px; font-family: var(--font-mono); font-weight: 500; text-align: right; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tk-kv a { color: var(--text); cursor: pointer; }
.tk-id { font-size: 11px !important; color: var(--text-dim); max-width: 75%; }
.tk-prog { display: block; height: 5px; border-radius: 3px; background: var(--border); overflow: hidden; }
.tk-prog s { display: block; height: 100%; background: var(--accent); transition: width 0.4s; }
.tk-chips { display: flex; gap: 6px; }
.tk-chips span { font-size: 10px; font-weight: 800; letter-spacing: 0.08em; text-transform: uppercase; padding: 4px 8px; border-radius: 4px; border: 1px solid var(--accent); color: var(--accent); }
.tk-chips span.off { border-color: var(--border); color: var(--text-dim); text-decoration: line-through; }
.tk-txs { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 6px; }
.tk-txs li { display: flex; align-items: center; gap: 10px; padding: 11px 12px; border-radius: 10px; border: 1px solid var(--border); background: var(--surface); cursor: pointer; transition: border-color 0.15s; }
.tk-txs li:hover { border-color: var(--accent); }
.tk-txs li > div { flex: 1; display: flex; flex-direction: column; gap: 3px; min-width: 0; }
.tk-txs b { font-size: 12.5px; font-weight: 600; }
.tk-txs code { font-size: 10.5px; color: var(--text-dim); margin-left: 4px; }
.tk-txs small { font-size: 10.5px; color: var(--text-dim); }
.tk-txam { font-family: var(--font-mono); font-size: 12.5px; font-weight: 700; color: var(--accent); white-space: nowrap; }
.tk-steps { list-style: none; margin: 0; padding: 0; display: grid; grid-template-columns: repeat(4, 1fr); gap: 4px; }
.tk-steps li { display: flex; flex-direction: column; gap: 6px; font-size: 10px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--text-dim); }
.tk-steps li i { display: block; height: 4px; border-radius: 2px; background: var(--border); transition: background-color 0.3s; }
.tk-steps li.on, .tk-steps li.done { color: var(--text); }
.tk-steps li.on i, .tk-steps li.done i { background: var(--accent); }
.tk-card { display: flex; flex-direction: column; gap: 12px; }
.tk-field { display: flex; flex-direction: column; gap: 6px; }
.tk-lbl { display: flex; justify-content: space-between; font-size: 11px; font-weight: 800; letter-spacing: 0.08em; text-transform: uppercase; color: var(--accent); }
.tk-field input:not([type='range']):not([type='checkbox']) { padding: 12px; border-radius: 10px; border: 1px solid var(--border); background: var(--bg); color: var(--text); font: inherit; font-family: var(--font-mono); font-size: 13px; transition: border-color 0.15s; }
.tk-field input:focus { outline: none; border-color: var(--accent); }
.tk-field input[type='range'] { accent-color: var(--accent); }
.tk-ticker { font-size: 18px !important; letter-spacing: 0.12em; }
.tk-agree { display: flex; gap: 10px; align-items: flex-start; font-size: 12px; line-height: 1.45; cursor: pointer; }
.tk-agree input { accent-color: var(--accent); margin-top: 2px; }
.tk-flags { display: flex; gap: 16px; font-size: 12px; }
.tk-flags input { accent-color: var(--accent); }
.tk-wait { display: flex; align-items: center; gap: 8px; font-size: 12px; padding: 12px; border-radius: 10px; border: 1px solid var(--border); }
.tk-spin { width: 14px; height: 14px; border-radius: 50%; border: 2px solid var(--border); border-top-color: var(--accent); animation: tk-rot 0.8s linear infinite; }
.tk-primary { padding: 13px; border-radius: 12px; border: 1.5px solid var(--accent); background: color-mix(in srgb, var(--accent) 12%, transparent); color: var(--accent); font: inherit; font-size: 12.5px; font-weight: 800; letter-spacing: 0.06em; text-transform: uppercase; cursor: pointer; transition: transform 0.15s; }
.tk-primary:hover { transform: translateY(-1px); }
.tk-skip { padding: 12px; border-radius: 10px; border: 1px solid var(--border); background: transparent; color: var(--text); font: inherit; font-size: 11px; font-weight: 800; letter-spacing: 0.06em; text-transform: uppercase; cursor: pointer; }
.tk-done { align-items: center; text-align: center; padding: 26px 10px; }
.tk-done-ic { width: 52px; height: 52px; border-radius: 50%; display: inline-flex; align-items: center; justify-content: center; border: 2px solid var(--accent); color: var(--accent); font-size: 24px; animation: tk-pop 0.35s cubic-bezier(0.3, 1.6, 0.5, 1); }
.mono { font-family: var(--font-mono); }
.taf-err { margin: 0; font-size: 11.5px; color: #f87171; }
@keyframes tk-in { from { opacity: 0; transform: translateY(4px); } }
@keyframes tk-rot { to { transform: rotate(360deg); } }
@keyframes tk-pop { from { transform: scale(0.4); opacity: 0; } }
</style>
