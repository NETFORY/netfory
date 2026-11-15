<script setup lang="ts">
import { ref, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useWalletStore } from '@/store/wallet'
import { useDappIdentity } from '@/wallet/useDappIdentity'
import { shortAddr } from '@/lib/crypto'

const { t } = useI18n()
const wallet = useWalletStore()
const remember = ref(true)

const conn = computed(() => wallet.pendingConnect)
const { name: dappName, verified: dappVerified, host: dappHost } = useDappIdentity(
  computed(() => conn.value?.origin),
)
</script>

<template>
  <div class="connect" data-testid="wallet-connect">
    <div v-if="!conn" class="empty dim">{{ t('wallet.connect.none') }}</div>

    <template v-else>
      <div class="hero">
        <div class="dapp-ava">{{ dappName.charAt(0).toUpperCase() }}</div>
        <h3 class="title">{{ t('wallet.connect.title') }}</h3>
        <p class="name" data-testid="connect-dapp-name">
          {{ dappName }}
          <span
            class="trust"
            :class="dappVerified === true ? 'ok' : dappVerified === false ? 'warn' : 'unk'"
            data-testid="connect-dapp-trust"
          >{{ dappVerified === true ? '✓ verified' : dappVerified === false ? '⚠ unverified' : '? unknown' }}</span>
        </p>
        <p class="origin mono" data-testid="connect-origin">sth://{{ dappHost }}</p>
      </div>

      <p class="desc dim">{{ t('wallet.connect.desc') }}</p>

      <div class="grant">
        <span class="dim">{{ t('wallet.connect.shares') }}</span>
        <code class="mono">{{ shortAddr(wallet.address) }}</code>
      </div>

      <label class="remember" data-testid="connect-remember">
        <input v-model="remember" type="checkbox" />
        <span>{{ t('wallet.connect.remember') }}</span>
      </label>

      <div class="acts">
        <button class="btn btn-ghost danger" @click="wallet.rejectConnect()" data-testid="connect-reject-btn">
          {{ t('wallet.connect.reject') }}
        </button>
        <button
          class="btn btn-primary"
          :disabled="wallet.busy"
          @click="wallet.approveConnect(remember)"
          data-testid="connect-approve-btn"
        >
          {{ t('wallet.connect.approve') }}
        </button>
      </div>
    </template>
  </div>
</template>

<style scoped>
.connect {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.empty {
  text-align: center;
  padding: 40px 0;
  font-size: 12px;
}
.hero {
  text-align: center;
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 8px 0;
}
.plug {
  font-size: 26px;
}
.dapp-ava {
  width: 48px;
  height: 48px;
  border-radius: 14px;
  display: grid;
  place-items: center;
  font-weight: var(--fw-800);
  font-size: 22px;
  color: var(--bg);
  background: var(--accent);
  margin: 0 auto;
}
.name {
  font-size: 15px;
  font-weight: var(--fw-700);
  color: var(--text);
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: 4px;
  align-items: center;
}
.trust {
  font-size: 10px;
  font-weight: var(--fw-700);
  padding: 2px 8px;
  border-radius: 999px;
}
.trust.ok {
  background: color-mix(in srgb, #2ecc71 18%, transparent);
  color: #2ecc71;
}
.trust.warn {
  background: color-mix(in srgb, var(--danger) 18%, transparent);
  color: var(--danger);
}
.trust.unk {
  background: var(--panel);
  color: var(--text-dim);
  border: 1px solid var(--border);
}
.title {
  font-size: 13px;
  font-weight: var(--fw-800);
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--accent);
  margin: 0;
}
.origin {
  font-size: 13px;
  word-break: break-all;
  color: var(--text);
}
.desc {
  font-size: 12px;
  line-height: 1.5;
  text-align: center;
}
.grant {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--panel);
  font-size: 12px;
}
.remember {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--text-dim);
  cursor: pointer;
}
.acts {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
  margin-top: 4px;
}
.btn {
  padding: 12px;
  font-weight: var(--fw-700);
  font-size: 12px;
  letter-spacing: 0.08em;
}
.danger {
  color: var(--danger);
  border-color: color-mix(in srgb, var(--danger) 50%, transparent);
}
</style>
