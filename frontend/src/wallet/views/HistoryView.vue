<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { useWalletStore } from '@/store/wallet'
import { useToastsStore } from '@/store/toasts'
import { shortAddr } from '@/lib/crypto'
import { openExplorerTx } from '@/lib/explorer'
import { computed, onMounted, ref, watch } from 'vue'
import { historyTokenTxs, type HistTokenTx } from '@/wallet/tokens/api'
import { useTokT } from '@/wallet/tokens/t'
import TokenLogo from '@/wallet/tokens/TokenLogo.vue'

const { t } = useI18n()
const wallet = useWalletStore()
const toasts = useToastsStore()
const tt = useTokT()

// SHIP-14: операции по токенам показываем в общей ленте рядом с STH, по времени.
const tokTxs = ref<HistTokenTx[]>([])
const filter = ref<'all' | 'sth' | 'tok'>('all')
async function loadTok() { if (wallet.address) tokTxs.value = await historyTokenTxs(wallet.address).catch(() => []) }
onMounted(loadTok)
watch(() => [wallet.address, wallet.transactions.length], loadTok)
type Row = { k: 'sth'; ts: number; tx: (typeof wallet.transactions)[number] } | { k: 'tok'; ts: number; tx: HistTokenTx }
const rows = computed<Row[]>(() => {
  const a: Row[] = filter.value === 'tok' ? [] : wallet.transactions.map((tx) => ({ k: 'sth' as const, ts: tx.timestamp, tx }))
  const b: Row[] = filter.value === 'sth' ? [] : tokTxs.value.map((tx) => ({ k: 'tok' as const, ts: tx.timestamp, tx }))
  return [...a, ...b].sort((x, y) => (x.tx.confirmed === y.tx.confirmed ? y.ts - x.ts : x.tx.confirmed ? 1 : -1))
})
const tokLabel = (x: HistTokenTx) => (x.type === 1 ? tt(x.dir === 'in' ? 'dirIn' : 'dirOut') : tt(`l_${x.label}`))

// клик по memo — копируем в буфер (не открывая обозреватель)
async function copyMemo(memo: string) {
  try {
    await navigator.clipboard.writeText(memo)
    toasts.push(t('wallet.memoCopied'), memo.length > 60 ? memo.slice(0, 57) + '…' : memo, 'info')
  } catch {
    /* clipboard недоступен */
  }
}

// клик по транзакции — обозреватель сети STH
function openTx(id: string) {
  openExplorerTx(id)
}

function fmtTime(ts: number): string {
  return new Date(ts).toLocaleString(undefined, {
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}
</script>

<template>
  <div class="history" data-testid="wallet-history">
    <div v-if="tokTxs.length" class="h-filter" data-testid="history-filter">
      <button v-for="f in (['all', 'sth', 'tok'] as const)" :key="f" :class="{ on: filter === f }" @click="filter = f" :data-testid="`history-filter-${f}`">{{ tt(`hist_${f}`) }}</button>
    </div>
    <div v-if="!rows.length" class="empty dim" data-testid="history-empty">
      {{ t('wallet.noTx') }}
    </div>
    <ul v-else class="tx-list">
      <template v-for="r in rows" :key="r.tx.id">
      <li v-if="r.k === 'tok'" class="tx tx-click tx-tok" @click="openTx(r.tx.id)" :data-testid="`history-token-tx-${r.tx.id}`">
        <div class="tx-head">
          <span class="tx-type" :class="r.tx.dir"><TokenLogo :id="r.tx.tokenId" :symbol="r.tx.symbol" :size="16" /> {{ tokLabel(r.tx) }}</span>
          <span class="tx-amt mono" :class="r.tx.dir">{{ r.tx.dir === 'in' ? '+' : r.tx.dir === 'out' ? '−' : '' }}{{ r.tx.amount }} {{ r.tx.symbol }}</span>
        </div>
        <div class="tx-meta">
          <span class="mono">{{ r.tx.counterparty ? shortAddr(r.tx.counterparty, 8, 6) : '' }}</span>
          <span class="dim">{{ fmtTime(r.tx.timestamp) }}</span>
        </div>
        <div v-if="r.tx.memo" class="tx-memo mono" @click.stop="copyMemo(r.tx.memo)">“{{ r.tx.memo }}”</div>
        <span class="status" :class="{ ok: r.tx.confirmed }">{{ r.tx.confirmed ? t('wallet.confirmed') : t('wallet.pending') }}</span>
      </li>
      <li v-else class="tx tx-click" @click="openTx(r.tx.id)" :title="t('payoutHistory.openExplorer')" :data-testid="`history-tx-${r.tx.id}`">
        <div class="tx-head">
          <span class="tx-type" :class="(r.tx as any).type">
            {{ (r.tx as any).type === 'in' ? t('wallet.received') : t('wallet.sent') }}
          </span>
          <span class="tx-amt mono" :class="(r.tx as any).type">
            {{ (r.tx as any).type === 'in' ? '+' : '−' }}{{ r.tx.amount }} STH
          </span>
        </div>
        <div class="tx-meta">
          <span class="mono">{{ shortAddr(r.tx.counterparty, 8, 6) }}</span>
          <span class="dim">{{ fmtTime(r.tx.timestamp) }}</span>
        </div>
        <div
          v-if="r.tx.memo"
          class="tx-memo mono"
          :title="t('wallet.memoCopyHint')"
          @click.stop="copyMemo(r.tx.memo)"
          :data-testid="`history-memo-${r.tx.id}`"
        >“{{ r.tx.memo }}”</div>
        <span class="status" :class="{ ok: r.tx.confirmed }">
          {{ r.tx.confirmed ? t('wallet.confirmed') : t('wallet.pending') }}
        </span>
      </li>
      </template>
    </ul>
  </div>
</template>

<style scoped>
.h-filter { display: flex; gap: 6px; }
.h-filter button { flex: 1; padding: 7px; border-radius: 999px; border: 1px solid var(--border); background: transparent; color: var(--text-dim); font: inherit; font-size: 11px; font-weight: 700; letter-spacing: 0.06em; text-transform: uppercase; cursor: pointer; transition: border-color 0.15s, color 0.15s; }
.h-filter button.on { border-color: var(--accent); color: var(--accent); }
.tx-tok .tx-type { display: inline-flex; align-items: center; gap: 6px; }
.tx-type.self, .tx-amt.self { color: var(--accent); }
.history {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.empty {
  padding: 28px 0;
  text-align: center;
  font-size: 12px;
}
.tx-list {
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.tx-click { cursor: pointer; }
.tx-click:hover { filter: brightness(1.15); }
.tx {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 13px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  min-width: 0;
  overflow: hidden;
}
.tx-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.tx-type {
  font-size: 10px;
  letter-spacing: 0.1em;
  font-weight: var(--fw-700);
  text-transform: uppercase;
}
.tx-type.in,
.tx-amt.in {
  color: var(--success);
}
.tx-type.out,
.tx-amt.out {
  color: var(--accent);
}
.tx-amt {
  font-size: 13px;
  font-weight: var(--fw-700);
}
.tx-meta {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 11px;
  color: var(--text-dim);
}
.tx-memo:hover {
  color: var(--text);
}
.tx-memo {
  font-size: 11px;
  color: var(--secondary);
  cursor: copy;
  transition: color 0.15s;
  min-width: 0;
  max-width: 100%;
  overflow-wrap: anywhere;
  word-break: break-all;
  white-space: normal;
  line-height: 1.35;
}
.status {
  align-self: flex-start;
  font-size: 9px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text-dim);
  padding: 2px 7px;
  border: 1px solid var(--border);
  border-radius: 999px;
}
.status.ok {
  color: var(--success);
  border-color: var(--success);
}
.mono {
  font-family: var(--font-mono, monospace);
}
</style>
