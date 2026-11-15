<script setup lang="ts">
/* eslint-disable no-undef */
import { useI18n } from 'vue-i18n'

const { t } = useI18n()
defineProps<{
  title: string
  recipient: string
  amount: number | string
  vendor: string
  busy?: boolean
}>()
const emit = defineEmits<{ (e: 'confirm'): void; (e: 'cancel'): void }>()
</script>

<template>
  <div class="tx-back" @click.self="!busy && emit('cancel')" data-testid="tx-confirm-modal">
    <div class="tx-card">
      <div class="tx-head">
        <span class="tx-dot" /><span class="tx-dot amber" /><span class="tx-dot green" />
        <span class="tx-title">{{ t('tx.confirmTitle') }}</span>
      </div>

      <p class="tx-purpose" data-testid="tx-purpose">{{ title }}</p>

      <div class="tx-row">
        <span class="tx-k">{{ t('tx.recipient') }}</span>
        <code class="tx-v" data-testid="tx-recipient">{{ recipient }}</code>
      </div>
      <div class="tx-row">
        <span class="tx-k">{{ t('tx.amount') }}</span>
        <span class="tx-v amount" data-testid="tx-amount">{{ amount }} STH</span>
      </div>
      <div class="tx-row">
        <span class="tx-k">{{ t('tx.memo') }}</span>
        <code class="tx-v memo" data-testid="tx-vendor">{{ vendor }}</code>
      </div>

      <div class="tx-actions">
        <button class="btn" :disabled="busy" @click="emit('cancel')" data-testid="tx-cancel">
          {{ t('tx.cancel') }}
        </button>
        <button class="btn btn-primary" :disabled="busy" @click="emit('confirm')" data-testid="tx-sign">
          {{ busy ? t('tx.signing') : t('tx.sign') }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.tx-back {
  position: fixed;
  inset: 0;
  background: rgba(2, 6, 12, 0.8);
  backdrop-filter: blur(6px);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 400;
}
.tx-card {
  width: 420px;
  max-width: 92vw;
  background: #070b11;
  border: 1px solid var(--accent);
  border-radius: 14px;
  box-shadow: 0 0 40px -8px var(--accent);
  padding: 18px;
  font-family: var(--font-mono, monospace);
}
.tx-head {
  display: flex;
  align-items: center;
  gap: 7px;
  margin-bottom: 14px;
}
.tx-dot {
  width: 11px;
  height: 11px;
  border-radius: 50%;
  background: #ef4444;
}
.tx-dot.amber { background: #f59e0b; }
.tx-dot.green { background: #22c55e; }
.tx-title {
  margin-left: 10px;
  font-size: 12px;
  letter-spacing: 0.1em;
  color: var(--accent);
}
.tx-purpose {
  font-size: 14px;
  color: #e8fbff;
  margin-bottom: 14px;
  line-height: 1.5;
}
.tx-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 9px 0;
  border-top: 1px solid var(--border);
}
.tx-k {
  font-size: 11px;
  color: var(--text-dim);
  letter-spacing: 0.04em;
  white-space: nowrap;
}
.tx-v {
  font-size: 12px;
  color: var(--text);
  word-break: break-all;
  text-align: right;
}
.tx-v.amount {
  font-size: 16px;
  font-weight: var(--fw-800);
  color: var(--accent);
}
.tx-v.memo {
  color: #fbbf24;
}
.tx-actions {
  display: flex;
  gap: 10px;
  justify-content: flex-end;
  margin-top: 16px;
}
</style>
