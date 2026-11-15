<script setup lang="ts">
import { ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import QRCode from 'qrcode'
import { useWalletStore } from '@/store/wallet'

const { t } = useI18n()
const wallet = useWalletStore()
const qr = ref('')
const copied = ref(false)

watch(
  () => wallet.address,
  async (addr) => {
    if (!addr) return
    qr.value = await QRCode.toDataURL(addr, {
      margin: 1,
      width: 220,
      color: { dark: '#e6edf3', light: '#0000' },
    })
  },
  { immediate: true },
)

async function copy() {
  const addr = wallet.address
  if (!addr) return
  try {
    await navigator.clipboard.writeText(addr)
  } catch {
    const ta = document.createElement('textarea')
    ta.value = addr
    document.body.appendChild(ta)
    ta.select()
    document.execCommand('copy')
    document.body.removeChild(ta)
  }
  copied.value = true
  setTimeout(() => (copied.value = false), 1600)
}
</script>

<template>
  <div class="receive" data-testid="wallet-receive">
    <p class="intro dim">{{ t('wallet.receive.header') }}</p>

    <div class="qr-wrap">
      <img v-if="qr" :src="qr" alt="address QR" class="qr" data-testid="receive-qr" />
    </div>

    <span class="lbl">{{ t('wallet.receive.yourAddress') }}</span>
    <button class="addr-box mono" @click="copy" data-testid="receive-copy">
      {{ wallet.address || '—' }}
    </button>

    <button class="tap-copy" @click="copy" data-testid="receive-tap-copy">
      {{ copied ? t('wallet.receive.copied') : t('wallet.receive.tapCopy') }}
    </button>
  </div>
</template>

<style scoped>
.receive {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 16px;
}
.intro {
  font-size: 12px;
  text-align: center;
  line-height: 1.6;
}
.qr-wrap {
  padding: 16px;
  background: var(--bg-elevated);
  border: 1px solid var(--border-accent);
  border-radius: var(--radius);
}
.qr {
  display: block;
  width: 220px;
  height: 220px;
}
.lbl {
  align-self: flex-start;
  font-size: 10px;
  letter-spacing: 0.12em;
  color: var(--text-dim);
  text-transform: uppercase;
  font-weight: var(--fw-700);
}
.addr-box {
  width: 100%;
  padding: 14px;
  font-size: 12px;
  word-break: break-all;
  text-align: left;
  color: var(--text);
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  cursor: pointer;
}
.addr-box:hover {
  border-color: var(--border-accent);
}
.mono {
  font-family: var(--font-mono, monospace);
}
.tap-copy {
  width: 100%;
  padding: 12px;
  font-size: 11px;
  font-weight: var(--fw-800);
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--accent);
  background: var(--accent-soft);
  border: 1px solid var(--accent);
  border-radius: var(--radius);
}
</style>
