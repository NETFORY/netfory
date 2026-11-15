<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useWalletStore } from '@/store/wallet'
import { useDappIdentity } from '@/wallet/useDappIdentity'
import { shortAddr } from '@/lib/crypto'

const { t } = useI18n()
const wallet = useWalletStore()

const req = computed(() => wallet.pendingRequest)
const { name: dappName, verified: dappVerified, host: dappHost } = useDappIdentity(
  computed(() => req.value?.origin),
)
const totalDebit = computed(() => {
  if (!req.value) return '0'
  return (Number(req.value.amount) + Number(req.value.fee)).toFixed(4)
})
function origin(): string {
  try {
    return new URL(req.value!.origin).host
  } catch {
    return req.value?.origin ?? 'unknown'
  }
}

const LARGE_AMOUNT = 1000 // STH

interface Risk {
  id: string
  level: 'high' | 'med' | 'low'
  label: string
}

const knownRecipients = computed(() => new Set(wallet.transactions.map((tx) => tx.counterparty)))

/** Heuristic safety checks surfaced to the user before they approve. */
const risks = computed<Risk[]>(() => {
  const r: Risk[] = []
  const p = req.value
  if (!p) return r

  // 1. Large amount — absolute threshold OR > half of the wallet balance.
  const amount = Number(p.amount) || 0
  const balance = Number(wallet.balance) || 0
  if (amount >= LARGE_AMOUNT || (balance > 0 && amount > balance * 0.5)) {
    r.push({ id: 'amount', level: 'med', label: t('wallet.verify.risk.amount') })
  }
  // 2. New recipient — address never seen in this wallet's history.
  if (p.recipient && !knownRecipients.value.has(p.recipient)) {
    r.push({ id: 'recipient', level: 'low', label: t('wallet.verify.risk.recipient') })
  }
  return r
})
</script>

<template>
  <div class="verify" data-testid="wallet-verify">
    <div v-if="!req" class="empty dim">{{ t('wallet.verify.none') }}</div>

    <template v-else>
      <div class="dapp-id" data-testid="verify-dapp-identity">
        <div class="dapp-ava">{{ dappName.charAt(0).toUpperCase() }}</div>
        <div class="dapp-meta">
          <span class="dapp-name" data-testid="verify-dapp-name">{{ dappName }}</span>
          <span class="dapp-sub mono">sth://{{ dappHost }}</span>
        </div>
        <span
          class="dapp-badge"
          :class="dappVerified === true ? 'ok' : dappVerified === false ? 'warn' : 'unk'"
          data-testid="verify-dapp-trust"
        >
          {{ dappVerified === true ? '✓' : dappVerified === false ? '⚠' : '?' }}
        </span>
      </div>

      <div class="alert">
        <span class="alert-mark">⚠</span>
        <span class="alert-txt">{{ t('wallet.verify.title') }}</span>
      </div>

      <div class="risks" data-testid="verify-risk-badges">
        <span class="risks-title dim">{{ t('wallet.verify.risk.title') }}</span>
        <div
          v-for="r in risks"
          :key="r.id"
          class="badge"
          :class="`lvl-${r.level}`"
          data-testid="verify-risk-badge"
          :data-risk="r.id"
        >
          <span class="badge-dot" />
          <span class="badge-lbl">{{ r.label }}</span>
        </div>
        <div v-if="risks.length === 0" class="badge lvl-ok" data-testid="verify-risk-ok">
          <span class="badge-dot" />
          <span class="badge-lbl">{{ t('wallet.verify.risk.safe') }}</span>
        </div>
      </div>

      <div class="origin" data-testid="verify-origin">
        <span class="dim">{{ t('wallet.verify.origin') }}</span>
        <span class="host">{{ origin() }}</span>
      </div>

      <div class="payload">
        <div class="row">
          <span class="dim">{{ t('wallet.verify.method') }}</span>
          <span class="mono">{{ req.method }}</span>
        </div>
        <div class="row">
          <span class="dim">{{ t('wallet.verify.to') }}</span>
          <span class="mono" :title="req.recipient">{{ shortAddr(req.recipient, 8, 6) }}</span>
        </div>
        <div class="row">
          <span class="dim">{{ t('wallet.verify.amount') }}</span>
          <span class="mono hl" data-testid="verify-amount">{{ req.amount }} STH</span>
        </div>
        <div class="row">
          <span class="dim">{{ t('wallet.verify.fee') }}</span>
          <span class="mono">{{ req.fee }} STH</span>
        </div>
        <div v-if="req.memo" class="row">
          <span class="dim">{{ t('wallet.verify.memo') }}</span>
          <span class="mono">{{ req.memo }}</span>
        </div>
        <div class="row total">
          <span>{{ t('wallet.verify.totalDebit') }}</span>
          <span class="mono hl">{{ totalDebit }} STH</span>
        </div>
      </div>

      <p v-if="wallet.error" class="err" data-testid="verify-error">⚠ {{ wallet.error }}</p>

      <div class="actions">
        <button
          class="btn btn-danger act"
          :disabled="wallet.busy"
          @click="wallet.reject()"
          data-testid="verify-reject-btn"
        >
          {{ t('wallet.verify.reject') }}
        </button>
        <button
          class="btn btn-primary act"
          :disabled="wallet.busy"
          @click="wallet.approve()"
          data-testid="verify-approve-btn"
        >
          {{ wallet.busy ? t('wallet.verify.broadcasting') : t('wallet.verify.approve') }}
        </button>
      </div>
    </template>
  </div>
</template>

<style scoped>
.verify {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.empty {
  padding: 28px 0;
  text-align: center;
  font-size: 12px;
}
.alert {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 14px;
  background: var(--accent-soft);
  border: 1px solid var(--border-accent);
  border-radius: var(--radius);
}
.alert-mark {
  font-size: 18px;
  color: var(--accent);
}
.alert-txt {
  font-size: 11px;
  font-weight: var(--fw-800);
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--accent);
}
.dapp-id {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--panel);
}
.dapp-ava {
  width: 36px;
  height: 36px;
  border-radius: 10px;
  display: grid;
  place-items: center;
  font-weight: var(--fw-800);
  font-size: 16px;
  color: var(--bg);
  background: var(--accent);
  flex-shrink: 0;
}
.dapp-meta {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
  flex: 1;
}
.dapp-name {
  font-size: 13px;
  font-weight: var(--fw-700);
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.dapp-sub {
  font-size: 10px;
  color: var(--text-dim);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.dapp-badge {
  width: 22px;
  height: 22px;
  border-radius: 50%;
  display: grid;
  place-items: center;
  font-size: 12px;
  font-weight: var(--fw-800);
  flex-shrink: 0;
}
.dapp-badge.ok {
  background: color-mix(in srgb, #2ecc71 20%, transparent);
  color: #2ecc71;
}
.dapp-badge.warn {
  background: color-mix(in srgb, var(--danger) 20%, transparent);
  color: var(--danger);
}
.dapp-badge.unk {
  background: var(--panel);
  color: var(--text-dim);
  border: 1px solid var(--border);
}
.risks {
  display: flex;
  flex-direction: column;
  gap: 7px;
}
.risks-title {
  font-size: 10px;
  font-weight: var(--fw-700);
  letter-spacing: 0.12em;
  text-transform: uppercase;
}
.badge {
  display: flex;
  align-items: center;
  gap: 9px;
  padding: 9px 11px;
  border-radius: var(--radius);
  border: 1px solid var(--border);
  font-size: 11.5px;
  line-height: 1.35;
  transition: border-color 0.2s ease, background-color 0.2s ease;
}
.badge-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}
.badge-lbl {
  color: var(--text);
}
.lvl-high {
  background: color-mix(in srgb, var(--danger) 14%, transparent);
  border-color: color-mix(in srgb, var(--danger) 55%, transparent);
}
.lvl-high .badge-dot {
  background: var(--danger);
}
.lvl-med {
  background: color-mix(in srgb, var(--accent) 12%, transparent);
  border-color: color-mix(in srgb, var(--accent) 50%, transparent);
}
.lvl-med .badge-dot {
  background: var(--accent);
}
.lvl-low {
  background: var(--panel);
  border-color: var(--border);
}
.lvl-low .badge-dot {
  background: var(--text-dim);
}
.lvl-ok {
  background: color-mix(in srgb, #2ecc71 12%, transparent);
  border-color: color-mix(in srgb, #2ecc71 45%, transparent);
}
.lvl-ok .badge-dot {
  background: #2ecc71;
}
.origin {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.1em;
}
.host {
  font-size: 14px;
  font-weight: var(--fw-700);
  color: var(--text);
  text-transform: none;
  letter-spacing: 0;
  word-break: break-all;
}
.payload {
  display: flex;
  flex-direction: column;
  gap: 9px;
  padding: 14px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 12px;
  gap: 10px;
}
.row.total {
  border-top: 1px solid var(--border);
  padding-top: 9px;
  font-weight: var(--fw-700);
}
.mono {
  font-family: var(--font-mono, monospace);
}
.err {
  color: var(--danger);
  font-size: 12px;
}
.actions {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
}
.act {
  padding: 13px;
}
</style>
