<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { useWalletStore } from '@/store/wallet'
import { useToastsStore } from '@/store/toasts'
import { shortAddr } from '@/lib/crypto'
import { openExplorerTx } from '@/lib/explorer'

const { t } = useI18n()
const wallet = useWalletStore()
const toasts = useToastsStore()

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
    <div v-if="!wallet.transactions.length" class="empty dim" data-testid="history-empty">
      {{ t('wallet.noTx') }}
    </div>
    <ul v-else class="tx-list">
      <li v-for="tx in wallet.transactions" :key="tx.id" class="tx tx-click" @click="openTx(tx.id)" :title="t('payoutHistory.openExplorer')" :data-testid="`history-tx-${tx.id}`">
        <div class="tx-head">
          <span class="tx-type" :class="tx.type">
            {{ tx.type === 'in' ? t('wallet.received') : t('wallet.sent') }}
          </span>
          <span class="tx-amt mono" :class="tx.type">
            {{ tx.type === 'in' ? '+' : '−' }}{{ tx.amount }} STH
          </span>
        </div>
        <div class="tx-meta">
          <span class="mono">{{ shortAddr(tx.counterparty, 8, 6) }}</span>
          <span class="dim">{{ fmtTime(tx.timestamp) }}</span>
        </div>
        <div
          v-if="tx.memo"
          class="tx-memo mono"
          :title="t('wallet.memoCopyHint')"
          @click.stop="copyMemo(tx.memo)"
          :data-testid="`history-memo-${tx.id}`"
        >“{{ tx.memo }}”</div>
        <span class="status" :class="{ ok: tx.confirmed }">
          {{ tx.confirmed ? t('wallet.confirmed') : t('wallet.pending') }}
        </span>
      </li>
    </ul>
  </div>
</template>

<style scoped>
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
