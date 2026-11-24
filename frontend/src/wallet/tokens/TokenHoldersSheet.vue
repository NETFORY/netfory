<script setup lang="ts">
import { computed, defineProps, onMounted, ref } from 'vue'
import { openExplorerWallet } from '@/lib/explorer'
import { fetchHolders, fromUnits, type TokenHolder, type TokenInfo } from './api'
import { useTokT } from './t'

// Держатели токена: место, адрес, баланс и доля от выпуска; свой адрес подсвечен. Клик — кошелёк в эксплорере.
const props = defineProps<{ info: TokenInfo; address: string }>()
const tt = useTokT()
const list = ref<TokenHolder[] | null>(null)
const q = ref('')
onMounted(async () => { list.value = await fetchHolders(props.info.id).catch(() => []) })
const supply = computed(() => BigInt(props.info.supply || '0') || 1n)
const share = (b: string) => Number((BigInt(b) * 10000n) / supply.value) / 100
const shown = computed(() => (list.value ?? []).map((h, i) => ({ ...h, rank: i + 1 })).filter((h) => !q.value || h.address.toLowerCase().includes(q.value.trim().toLowerCase())))
const top10 = computed(() => (list.value ?? []).slice(0, 10).reduce((a, h) => a + share(h.balance), 0))
const short = (a: string) => `${a.slice(0, 8)}…${a.slice(-8)}`
</script>

<template>
  <div class="th" data-testid="token-holders">
    <p v-if="!list" class="tk-dim">{{ tt('loading') }}</p>
    <template v-else>
      <div class="th-stats">
        <div><small>{{ tt('holders') }}</small><b class="mono" data-testid="holders-count">{{ list.length }}{{ list.length >= 100 ? '+' : '' }}</b></div>
        <div><small>{{ tt('top10') }}</small><b class="mono">{{ top10.toFixed(1) }}%</b></div>
        <div><small>{{ tt('supply') }}</small><b class="mono">{{ fromUnits(info.supply, info.decimals) }}</b></div>
      </div>
      <input v-if="list.length > 8" v-model="q" class="th-q" :placeholder="tt('searchAddr')" spellcheck="false" data-testid="holders-search" />
      <div class="th-list" data-testid="holders-list">
        <a v-for="h in shown" :key="h.address" @click="openExplorerWallet(h.address)" :class="{ me: h.address === address }" data-testid="holder-row">
          <span class="th-rank mono">{{ h.rank }}</span>
          <span class="th-main">
            <span class="mono">{{ short(h.address) }}<em v-if="h.address === address"> · {{ tt('mYou') }}</em><em v-else-if="h.address === info.ownerAddress"> · {{ tt('owner') }}</em></span>
            <i class="th-bar"><s :style="{ width: `${Math.max(1, share(h.balance))}%` }" /></i>
          </span>
          <span class="th-bal"><b class="mono">{{ fromUnits(h.balance, info.decimals) }}</b><small class="mono">{{ share(h.balance).toFixed(2) }}%</small></span>
        </a>
        <p v-if="!shown.length" class="th-empty">{{ tt('noHolders') }}</p>
      </div>
    </template>
  </div>
</template>

<style scoped>
.th { display: flex; flex-direction: column; gap: 12px; }
.th-stats { display: grid; grid-template-columns: repeat(3, 1fr); gap: 6px; }
.th-stats div { display: flex; flex-direction: column; gap: 3px; padding: 10px; border-radius: 10px; border: 1px solid var(--border); min-width: 0; }
.th-stats small { font-size: 9.5px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--text-dim); }
.th-stats b { font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.th-q { padding: 10px 12px; border-radius: 10px; border: 1px solid var(--border); background: var(--bg); color: var(--text); font-family: var(--font-mono); font-size: 12px; }
.th-q:focus { outline: none; border-color: var(--accent); }
.th-list { display: flex; flex-direction: column; }
.th-list a { display: flex; align-items: center; gap: 10px; padding: 9px 4px; border-bottom: 1px solid var(--border); cursor: pointer; color: var(--text); transition: background-color 0.15s; }
.th-list a:last-of-type { border-bottom: none; }
.th-list a:hover { background: color-mix(in srgb, var(--accent) 6%, transparent); }
.th-list a.me .th-main > span { color: var(--accent); }
.th-rank { width: 22px; font-size: 10.5px; color: var(--text-dim); text-align: right; }
.th-main { flex: 1; display: flex; flex-direction: column; gap: 5px; min-width: 0; font-size: 11.5px; }
.th-main em { font-style: normal; color: var(--accent); }
.th-bar { display: block; height: 3px; border-radius: 2px; background: var(--border); overflow: hidden; }
.th-bar s { display: block; height: 100%; background: var(--accent); }
.th-bal { display: flex; flex-direction: column; align-items: flex-end; gap: 2px; }
.th-bal b { font-size: 12px; }
.th-bal small { font-size: 10px; color: var(--text-dim); }
.th-empty { margin: 0; padding: 10px 0; font-size: 11.5px; color: var(--text-dim); text-align: center; }
</style>
