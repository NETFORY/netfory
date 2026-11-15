<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import QRCode from 'qrcode'

const { t } = useI18n()
// title/hint опциональны: по умолчанию строки File Exchange (fx.*),
// переопределяются вызывающим (например, QR прямой ссылки на dApp).
const props = defineProps<{ link: string; name?: string; title?: string; hint?: string }>()
const emit = defineEmits<{ (e: 'close'): void }>()

const qrDataUrl = ref('')
const copied = ref(false)

onMounted(async () => {
  try {
    qrDataUrl.value = await QRCode.toDataURL(props.link, {
      errorCorrectionLevel: 'M',
      margin: 2,
      width: 320,
      color: { dark: '#03121a', light: '#ffffff' },
    })
  } catch {
    /* ignore */
  }
})

async function copyLink() {
  try {
    await navigator.clipboard.writeText(props.link)
    copied.value = true
    setTimeout(() => (copied.value = false), 1500)
  } catch {
    /* ignore */
  }
}
</script>

<template>
  <div class="qr-back" @click.self="emit('close')" data-testid="file-qr-modal">
    <div class="qr-box">
      <header class="qr-head">
        <h3>{{ props.title || t('fx.qrModalTitle') }}</h3>
        <button class="x" @click="emit('close')" data-testid="file-qr-close">✕</button>
      </header>
      <p v-if="name" class="qr-name">{{ name }}</p>
      <div class="qr-img-wrap">
        <img v-if="qrDataUrl" :src="qrDataUrl" alt="QR" class="qr-img" data-testid="file-qr-img" />
        <div v-else class="qr-loading">{{ t('fx.qrGenerating') }}</div>
      </div>
      <p class="qr-link" data-testid="file-qr-link">{{ link }}</p>
      <button class="qr-copy" @click="copyLink" data-testid="file-qr-copy">
        {{ copied ? t('fx.copied') : t('fx.qrCopyLink') }}
      </button>
      <p class="qr-hint">{{ props.hint || t('fx.qrHint') }}</p>
    </div>
  </div>
</template>

<style scoped>
.qr-back {
  position: fixed;
  inset: 0;
  z-index: 1100;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(3, 6, 10, 0.82);
  backdrop-filter: blur(6px);
}
.qr-box {
  width: 380px;
  max-width: 92vw;
  border: 1px solid rgba(34, 211, 238, 0.3);
  border-radius: 16px;
  background: #0a0f16;
  padding: 22px;
  text-align: center;
  box-shadow: 0 0 60px rgba(34, 211, 238, 0.14);
}
.qr-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 14px;
}
.qr-head h3 {
  font-size: 13px;
  letter-spacing: 2px;
  color: #22d3ee;
}
.x {
  background: none;
  border: none;
  color: #64748b;
  cursor: pointer;
  font-size: 16px;
}
.qr-name {
  color: #e2e8f0;
  font-size: 13px;
  margin-bottom: 14px;
  word-break: break-word;
}
.qr-img-wrap {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 320px;
}
.qr-img {
  width: 320px;
  height: 320px;
  border-radius: 12px;
  background: #fff;
}
.qr-loading {
  color: #475569;
  font-size: 13px;
}
.qr-link {
  margin: 16px 0 12px;
  font-family: ui-monospace, monospace;
  font-size: 11px;
  color: #64748b;
  word-break: break-all;
}
.qr-copy {
  width: 100%;
  padding: 11px;
  border: 1px solid rgba(34, 211, 238, 0.4);
  border-radius: 10px;
  background: transparent;
  color: #22d3ee;
  font-size: 13px;
  cursor: pointer;
  transition: background-color 0.2s;
}
.qr-copy:hover {
  background: rgba(34, 211, 238, 0.1);
}
.qr-hint {
  margin-top: 12px;
  font-size: 11px;
  color: #475569;
  line-height: 1.5;
}
</style>
