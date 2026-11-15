<script setup lang="ts">
import { defineEmits, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useAuthStore } from '@/store/auth'
import { useWalletStore } from '@/store/wallet'
import { shortAddr } from '@/lib/crypto'
import { p2pStatus } from '@/lib/bridge'

const { t } = useI18n()
const auth = useAuthStore()
const wallet = useWalletStore()
const revealed = ref(false)
const seed = ref('')
const busy = ref(false)
const copiedIdx = ref<number | null>(null)
const emit = defineEmits<{ (e: 'show-messenger-qr'): void }>()
// Node ID (iroh EndpointId) этого устройства — копируется по клику
const nodeId = ref('')
const nodeCopied = ref(false)
onMounted(async () => {
  nodeId.value = (await p2pStatus()).nodeId
})
async function copyNodeId() {
  if (!nodeId.value) return
  try {
    await navigator.clipboard.writeText(nodeId.value)
    nodeCopied.value = true
    setTimeout(() => (nodeCopied.value = false), 1300)
  } catch {
    /* noop */
  }
}

async function toggleReveal() {
  if (!revealed.value) {
    seed.value = (await auth.revealSeed()) || ''
    revealed.value = true
  } else {
    revealed.value = false
    seed.value = ''
  }
}

async function derive() {
  busy.value = true
  try {
    await auth.deriveNext()
  } finally {
    busy.value = false
  }
}

async function copy(text: string, idx: number) {
  try {
    await navigator.clipboard.writeText(text)
    copiedIdx.value = idx
    setTimeout(() => (copiedIdx.value = null), 1300)
  } catch {
    /* noop */
  }
}
</script>

<template>
  <div class="keys" data-testid="keys-view">
    <h1 class="page-title">// {{ t('keys.title') }}_</h1>

    <!-- seed phrase -->
    <section class="panel block" data-testid="keys-seed-block">
      <div class="block-head">
        <span class="block-title">{{ t('keys.seedSection') }}</span>
        <button class="btn" @click="toggleReveal" data-testid="reveal-seed-btn">
          {{ revealed ? t('keys.hide') : t('keys.reveal') }}
        </button>
      </div>
      <div class="seed-box" :class="{ blurred: !revealed }" data-testid="seed-display">
        <span v-if="revealed" class="seed-text">{{ seed }}</span>
        <span v-else class="seed-mask">•••• •••• •••• •••• •••• •••• •••• •••• •••• •••• •••• ••••</span>
      </div>
    </section>

    <!-- NETFORY Messenger: QR для входа на телефоне/другом ПК (модалка — в Settings.vue) -->
    <section class="panel block" data-testid="keys-messenger-block">
      <div class="block-head">
        <span class="block-title">{{ t('keys.msgSection') }}</span>
        <button class="btn btn-primary" @click="emit('show-messenger-qr')" data-testid="keys-show-messenger-qr-btn">
          {{ t('settings.showMsgQr') }}
        </button>
      </div>
      <p class="hint dim">{{ t('keys.msgSectionHint') }}</p>
    </section>

    <!-- Node ID этого устройства (iroh) -->
    <section class="panel block" data-testid="keys-node-block">
      <div class="block-head">
        <span class="block-title">{{ t('keys.nodeId') }}</span>
        <button class="btn copy-btn" :disabled="!nodeId" @click="copyNodeId" data-testid="copy-node-id-btn">
          {{ nodeCopied ? t('common.copied') : t('common.copy') }}
        </button>
      </div>
      <button class="node-id mono" :class="{ copied: nodeCopied }" :disabled="!nodeId" :title="t('keys.nodeIdClick')" @click="copyNodeId" data-testid="keys-node-id">
        {{ nodeId || '—' }}
      </button>
      <p class="hint dim">{{ nodeId ? t('keys.nodeIdHint') : t('keys.nodeIdOff') }}</p>
    </section>

    <!-- HD accounts -->
    <section class="block">
      <div class="block-head">
        <span class="block-title">{{ t('keys.accounts') }}</span>
        <button class="btn btn-primary" :disabled="busy" @click="derive" data-testid="derive-account-btn">
          {{ t('keys.derive') }}
        </button>
      </div>

      <div class="acc-list" data-testid="accounts-list">
        <div
          v-for="acc in auth.accounts"
          :key="acc.index"
          class="acc panel"
          :data-testid="`account-${acc.index}`"
        >
          <div class="acc-left">
            <span class="acc-badge">{{ t('keys.account') }} #{{ acc.index }}</span>
            <span class="acc-path dim">{{ acc.path }}</span>
          </div>
          <div class="acc-right">
            <code class="acc-addr hl">{{ acc.address }}</code>
            <span class="acc-pub dim">{{ t('keys.pubkey') }}: {{ shortAddr(acc.publicKey, 10, 6) }}</span>
          </div>
          <button
            class="btn copy-btn"
            @click="copy(acc.address, acc.index)"
            :data-testid="`copy-account-${acc.index}`"
          >
            {{ copiedIdx === acc.index ? t('common.copied') : t('common.copy') }}
          </button>
        </div>
      </div>
    </section>

    <!-- CONNECTED DAPPS (moved here from Settings) -->
    <section class="panel block" data-testid="keys-dapps-block">
      <div class="block-head">
        <span class="block-title">{{ t('dapps.title') }}</span>
      </div>
      <p class="hint dim">{{ t('dapps.hint') }}</p>
      <div v-if="wallet.approvedOrigins.length" class="dapp-list" data-testid="dapp-origin-list">
        <div
          v-for="o in wallet.approvedOrigins"
          :key="o"
          class="dapp-row"
          :data-testid="`dapp-origin-${o}`"
        >
          <code class="dapp-origin">{{ o }}</code>
          <button
            class="btn btn-danger"
            @click="wallet.revokeOrigin(o)"
            :data-testid="`dapp-revoke-${o}`"
          >
            {{ t('dapps.revoke') }}
          </button>
        </div>
      </div>
      <p v-else class="hint dim" data-testid="dapp-empty">{{ t('dapps.none') }}</p>
    </section>
  </div>
</template>

<style scoped>
.keys {
  max-width: 980px;
  display: flex;
  flex-direction: column;
  gap: 24px;
}
.page-title {
  font-size: 15px;
  letter-spacing: 0.14em;
  color: var(--accent);
  font-weight: var(--fw-700);
}
.block {
  display: flex;
  flex-direction: column;
}
.panel.block {
  padding: 18px;
  gap: 14px;
}
.block-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 14px;
}
.block-title {
  font-size: 12px;
  letter-spacing: 0.12em;
  color: var(--secondary);
  text-transform: uppercase;
  font-weight: var(--fw-600);
}
.seed-box {
  padding: 16px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  transition: filter 0.2s ease;
}
.seed-box.blurred {
  filter: blur(5px);
  user-select: none;
}
.seed-text {
  font-size: 14px;
  line-height: 1.9;
  word-spacing: 4px;
  color: var(--accent);
}
.seed-mask {
  letter-spacing: 0.15em;
  color: var(--text-dim);
}
.acc-list {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.acc {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 14px 16px;
  transition: border-color 0.18s ease;
}
.acc:hover {
  border-color: var(--border-accent);
}
.acc-left {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 150px;
}
.acc-badge {
  font-size: 12px;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
}
.acc-path {
  font-size: 10px;
}
.acc-right {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}
.acc-addr {
  font-size: 12px;
  word-break: break-all;
}
.acc-pub {
  font-size: 10px;
}
.node-id {
  display: block;
  width: 100%;
  text-align: left;
  padding: 10px 12px;
  margin: 10px 0 8px;
  font-size: 12px;
  word-break: break-all;
  line-height: 1.5;
  color: var(--text);
  background: var(--bg-elev, rgba(255, 255, 255, 0.03));
  border: 1px dashed var(--border);
  cursor: copy;
  transition: border-color 0.15s ease, background-color 0.15s ease;
}
.node-id:hover:not(:disabled) { border-color: var(--accent); }
.node-id.copied { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 12%, transparent); }
.node-id:disabled { cursor: default; opacity: 0.6; }
.copy-btn {
  padding: 7px 12px;
  white-space: nowrap;
}
.hint {
  font-size: 12px;
  line-height: 1.6;
}
.dapp-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.dapp-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.dapp-origin {
  flex: 1;
  font-size: 12px;
  word-break: break-all;
}
button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
