<script setup lang="ts">
/*
 * SmartNet Wallet // AuthorizeMessageView
 *
 *   Approval-gated `signMessage` prompt — the SmartNet equivalent of the
 *   Core-Wallet browser extension `AuthorizeMessage.vue`. Displays the exact
 *   plain-text payload the dApp asked to sign, so the user can spot phishing
 *   (arbitrary text that could be a hidden transaction, delegation authority,
 *   drainer nonce, etc.) before granting the signature.
 *
 *   Signing is delegated to `wallet.approveSignMessage()`, which invokes the
 *   Rust `resolve_sign_message` command; the real bip-schnorr signature is
 *   produced inside Rust and returned to the dApp via the `dapp-response`
 *   event bus. Reject → `reject_sign_message` (dApp gets an error).
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useWalletStore } from '@/store/wallet'
import { useDappIdentity } from '@/wallet/useDappIdentity'
import { shortAddr } from '@/lib/crypto'

const { t } = useI18n()
const wallet = useWalletStore()

const req = computed(() => wallet.pendingSignMessage)
const { name: dappName, verified: dappVerified, host: dappHost } = useDappIdentity(
  computed(() => req.value?.origin),
)

// Плоская длина сообщения → предупреждаем о слишком больших payload'ах
// (потенциальный DoS / скрытие вредоносного контента ниже экрана).
const messageLength = computed(() => (req.value?.message ?? '').length)
const oversized = computed(() => messageLength.value > 4000)
</script>

<template>
  <div class="authorize" data-testid="wallet-authorize">
    <div v-if="!req" class="empty dim">{{ t('wallet.authorize.none') }}</div>

    <template v-else>
      <!-- dApp identity chip (host, verified badge) -->
      <div class="dapp-id" data-testid="authorize-dapp-identity">
        <div class="dapp-ava">{{ dappName.charAt(0).toUpperCase() }}</div>
        <div class="dapp-meta">
          <span class="dapp-name" data-testid="authorize-dapp-name">{{ dappName }}</span>
          <span class="dapp-sub mono">sth://{{ dappHost }}</span>
        </div>
        <span
          class="dapp-badge"
          :class="dappVerified === true ? 'ok' : dappVerified === false ? 'warn' : 'unk'"
          data-testid="authorize-dapp-trust"
        >
          {{ dappVerified === true ? '✓' : dappVerified === false ? '⚠' : '?' }}
        </span>
      </div>

      <!-- Красная плашка: подпись — это авторизация, никогда не подписывай, если не понимаешь -->
      <div class="alert">
        <span class="alert-mark">⚠</span>
        <span class="alert-txt">{{ t('wallet.authorize.title') }}</span>
      </div>
      <p class="hint dim">{{ t('wallet.authorize.hint') }}</p>

      <!-- Кто подписывает (адрес пользователя) -->
      <div class="signer-row">
        <span class="dim">{{ t('wallet.authorize.signer') }}</span>
        <code class="mono" data-testid="authorize-signer">{{ shortAddr(wallet.address, 8, 6) }}</code>
      </div>

      <!-- Точный plaintext, который dApp просит подписать -->
      <div class="msg-box" data-testid="authorize-message-box">
        <div class="msg-head">
          <span class="msg-title">{{ t('wallet.authorize.message') }}</span>
          <span class="msg-meta dim">{{ messageLength }} {{ t('wallet.authorize.chars') }}</span>
        </div>
        <pre class="msg-body mono" data-testid="authorize-message-text">{{ req.message }}</pre>
        <p v-if="oversized" class="oversize warn">⚠ {{ t('wallet.authorize.oversized') }}</p>
      </div>

      <p v-if="wallet.error" class="err" data-testid="authorize-error">⚠ {{ wallet.error }}</p>

      <div class="actions">
        <button
          class="btn btn-danger act"
          :disabled="wallet.busy"
          @click="wallet.rejectSignMessage()"
          data-testid="authorize-reject-btn"
        >
          {{ t('wallet.authorize.reject') }}
        </button>
        <button
          class="btn btn-primary act"
          :disabled="wallet.busy"
          @click="wallet.approveSignMessage()"
          data-testid="authorize-approve-btn"
        >
          {{ wallet.busy ? t('wallet.authorize.signing') : t('wallet.authorize.approve') }}
        </button>
      </div>
    </template>
  </div>
</template>

<style scoped>
.authorize {
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.empty {
  padding: 28px 0;
  text-align: center;
  font-size: 12px;
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
.hint {
  font-size: 11.5px;
  line-height: 1.5;
  margin: -6px 2px 0;
}
.signer-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--panel);
  font-size: 12px;
}
.msg-box {
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--panel);
  overflow: hidden;
}
.msg-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 12px;
  border-bottom: 1px solid var(--border);
  font-size: 10px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
}
.msg-title {
  font-weight: var(--fw-700);
  color: var(--accent);
}
.msg-meta {
  font-size: 10px;
}
.msg-body {
  margin: 0;
  padding: 12px;
  font-size: 12px;
  line-height: 1.55;
  color: var(--text);
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 220px;
  overflow-y: auto;
  background: color-mix(in srgb, var(--bg) 60%, transparent);
}
.oversize {
  margin: 8px 12px 12px;
  font-size: 11px;
  color: var(--danger);
}
.warn {
  color: var(--danger);
}
.err {
  color: var(--danger);
  font-size: 12px;
}
.mono {
  font-family: var(--font-mono, monospace);
}
.actions {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
}
.act {
  padding: 13px;
  font-weight: var(--fw-700);
  font-size: 12px;
  letter-spacing: 0.08em;
}
</style>
