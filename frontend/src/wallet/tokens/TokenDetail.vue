<script setup lang="ts">
import { computed, defineEmits, defineProps, onMounted, ref } from 'vue'
import { openExplorerTx, openExplorerWallet } from '@/lib/explorer'
import { cachedInfo, fromUnits, lastPrice, tokenInfo, tokenTxs, type TokenConfig, type TokenInfo, type TokenRow, type TokenTx } from './api'
import { useTokT } from './t'
import TokenLogo from './TokenLogo.vue'
import TokenActionForm from './TokenActionForm.vue'
import TokenMetaForm from './TokenMetaForm.vue'
import TokenSheet from './TokenSheet.vue'
import TokenMarketSheet from './TokenMarketSheet.vue'
import TokenHoldersSheet from './TokenHoldersSheet.vue'

// Экран токена: держатель — отправить / multi pay / сжечь; владелец — + довыпуск и витрина. 5 последних операций.
const props = defineProps<{ row: TokenRow; cfg: TokenConfig; address: string }>()
const emit = defineEmits<{ (e: 'back'): void; (e: 'changed'): void }>()
const tt = useTokT()
const info = ref<TokenInfo | null>(cachedInfo(props.row.id))
const txs = ref<TokenTx[] | null>(null)
const mode = ref<'' | 'send' | 'multi' | 'mint' | 'burn' | 'meta' | 'market' | 'holders'>('')
const marketTitle = ref('')
const note = ref('')
const loading = ref(false)
const price = ref<bigint | null>(null)
const value = computed(() => (price.value ? (BigInt(props.row.balance) * price.value) / 10n ** BigInt(props.row.decimals) : null))

const isOwner = computed(() => !!(info.value?.owner || props.row.owner))
const pct = computed(() => { const i = info.value; if (!i || i.supplyCap === '0') return 100; return Number((BigInt(i.supply) * 1000n) / BigInt(i.supplyCap)) / 10 })
const canMint = computed(() => isOwner.value && !!info.value?.mintable && BigInt(info.value.supply) < BigInt(info.value.supplyCap))
const hasBal = computed(() => props.row.balance !== '0')
const sheetTitle = computed(() => (mode.value === 'meta' ? tt('step3') : mode.value === 'holders' ? `${tt('holders')} · ${props.row.symbol}` : mode.value === 'market' ? `${marketTitle.value || tt('mTitle')} · ${props.row.symbol}` : mode.value === 'multi' ? `Multi pay · ${props.row.symbol}` : `${tt(mode.value || 'send')} ${props.row.symbol}`))

async function load() {
  loading.value = true
  const [i, t] = await Promise.allSettled([tokenInfo(props.row.id, props.address, props.row.balance), tokenTxs(props.row.id, props.address, props.row.decimals)])
  if (i.status === 'fulfilled') info.value = { ...i.value, balance: props.row.balance, owner: i.value.owner || props.row.owner }
  txs.value = t.status === 'fulfilled' ? t.value : []
  loading.value = false
}
function done(id: string) { mode.value = ''; note.value = `${tt('sent')}: ${id.slice(0, 10)}…`; emit('changed'); setTimeout(load, 4000) }
onMounted(() => { load(); lastPrice(props.row.id).then((p) => (price.value = p)) })
function ago(ms: number) {
  if (!ms) return ''
  const s = Math.max(1, Math.round((Date.now() - ms) / 1000))
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' })
  if (s < 3600) return rtf.format(-Math.round(s / 60), 'minute')
  if (s < 86400) return rtf.format(-Math.round(s / 3600), 'hour')
  return rtf.format(-Math.round(s / 86400), 'day')
}
const short = (a: string) => (a.length > 16 ? `${a.slice(0, 6)}…${a.slice(-6)}` : a)
</script>

<template>
  <div class="tk-detail" data-testid="token-detail">
    <div class="tk-bar-head">
      <button @click="emit('back')" data-testid="token-detail-back" aria-label="back">‹</button>
      <b>{{ tt('tokenWord') }}</b>
      <button :class="{ spin: loading }" @click="load" data-testid="token-detail-refresh" aria-label="refresh">↻</button>
    </div>

    <div class="tk-hero">
      <TokenLogo :id="row.id" :symbol="row.symbol" :info="info" :size="64" square />
      <div class="tk-hero-txt">
        <h3 data-testid="token-detail-name">{{ info?.meta?.name || row.symbol }} <em v-if="isOwner" class="tk-badge">{{ tt('owner') }}</em></h3>
        <span class="tk-sym">{{ row.symbol }}</span>
        <div class="tk-bal"><b data-testid="token-detail-balance">{{ fromUnits(row.balance, row.decimals) }}</b> <span>{{ row.symbol }}</span></div>
        <small v-if="price" class="tk-px" data-testid="token-detail-value">≈ {{ fromUnits(value ?? 0n, 8) }} STH · {{ fromUnits(price, 8) }} STH/{{ row.symbol }}</small>
      </div>
    </div>

    <div class="tk-acts three">
      <button class="pri" :disabled="!cfg.enabled || !hasBal" @click="mode = 'send'" data-testid="token-action-send">{{ tt('send') }}</button>
      <button :disabled="!cfg.enabled || !hasBal" @click="mode = 'multi'" data-testid="token-action-multi">Multi pay</button>
      <button class="mkt" @click="marketTitle = ''; mode = 'market'" data-testid="token-action-market">{{ tt('mTitle') }}</button>
    </div>
    <button v-if="info?.burnable" class="tk-burn" :disabled="!cfg.enabled || !hasBal" @click="mode = 'burn'" data-testid="token-action-burn">{{ tt('burn') }}</button>
    <div v-if="isOwner" class="tk-owner" data-testid="token-owner-actions">
      <span class="tk-h">{{ tt('ownerActions') }}</span>
      <div class="tk-acts">
        <button v-if="canMint" :disabled="!cfg.enabled" @click="mode = 'mint'" data-testid="token-action-mint">{{ tt('mintMore') }}</button>
        <button :disabled="!cfg.enabled" @click="mode = 'meta'" data-testid="token-action-meta">{{ tt('step3') }}</button>
        <button @click="mode = 'holders'" data-testid="token-action-holders">{{ tt('holders') }}</button>
      </div>
    </div>
    <p v-if="!cfg.enabled" class="tk-warn" data-testid="token-inactive">{{ tt('inactive') }}</p>
    <p v-if="note" class="tk-ok" data-testid="token-detail-note">{{ note }}</p>

    <div v-if="info" class="tk-info">
      <p v-if="info.meta?.description" class="tk-desc">{{ info.meta.description }}</p>
      <a v-if="info.meta?.website" class="tk-site" :href="info.meta.website" target="_blank" rel="noopener">{{ info.meta.website }}</a>
      <div class="tk-kv"><span>{{ tt('decimals') }}</span><b>{{ info.decimals }}</b></div>
      <div class="tk-kv"><span>{{ tt('supplyCap') }}</span><b>{{ fromUnits(info.supply, info.decimals) }} / {{ fromUnits(info.supplyCap, info.decimals) }}</b></div>
      <i class="tk-prog"><s :style="{ width: `${pct}%` }" /></i>
      <div class="tk-chips">
        <span :class="{ off: !info.mintable }">{{ tt('l_mint') }}</span>
        <span :class="{ off: !info.burnable }">{{ tt('l_burn') }}</span>
      </div>
      <div class="tk-kv"><span>{{ tt('owner') }}</span><a @click="openExplorerWallet(info.ownerAddress)" :title="info.ownerAddress">{{ short(info.ownerAddress) }} ↗</a></div>
      <div v-if="info.initHeight" class="tk-kv"><span>{{ tt('initBlock') }}</span><b>#{{ info.initHeight.toLocaleString() }}</b></div>
      <div v-if="info.holders != null" class="tk-kv"><span>{{ tt('holders') }}</span><b>{{ info.holders }}</b></div>
      <div class="tk-kv"><span>ID</span><b class="tk-id" :title="row.id">{{ row.id }}</b></div>
      <a class="tk-explorer" @click="openExplorerTx(row.id)" data-testid="token-detail-explorer">{{ tt('explorer') }} ↗</a>
    </div>

    <h4 class="tk-h">{{ tt('txs') }}</h4>
    <p v-if="txs === null" class="tk-dim">{{ tt('loading') }}</p>
    <p v-else-if="!txs.length" class="tk-dim" data-testid="token-txs-empty">{{ tt('noTxs') }}</p>
    <ul v-else class="tk-txs" data-testid="token-txs">
      <li v-for="x in txs" :key="x.id" @click="openExplorerTx(x.id)">
        <div><b>{{ x.type === 1 ? tt(x.dir === 'in' ? 'dirIn' : 'dirOut') : tt(`l_${x.label}`) }} <code v-if="x.counterparty">{{ short(x.counterparty) }}</code></b><small>{{ ago(x.timestamp) }}<em v-if="!x.confirmed"> · {{ tt('pending') }}</em></small></div>
        <span class="tk-txam" :class="x.dir">{{ x.dir === 'in' ? '+' : x.dir === 'out' ? '-' : '' }}{{ x.amount }} {{ row.symbol }}</span>
      </li>
    </ul>

    <TokenSheet v-if="mode && info" :title="sheetTitle" testid="token-sheet" @close="mode = ''">
      <TokenHoldersSheet v-if="mode === 'holders'" :info="info" :address="address" />
      <TokenMarketSheet v-else-if="mode === 'market'" :info="{ ...info, balance: row.balance }" :cfg="cfg" :address="address" @done="done" @mode="marketTitle = $event" />
      <TokenMetaForm v-else-if="mode === 'meta'" :id="row.id" :symbol="row.symbol" :cfg="cfg" :initial="info.meta" @done="done" />
      <TokenActionForm v-else :mode="mode" :info="{ ...info, balance: row.balance }" :cfg="cfg" :address="address" @done="done" />
    </TokenSheet>
  </div>
</template>
