<script setup lang="ts">
/* eslint-disable no-undef */
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'

const emit = defineEmits<{ (e: 'close'): void }>()
const { t } = useI18n()

interface Coin {
  code: string
  name: string
  addr: string
}

const coins: Coin[] = [
  { code: 'BTC', name: 'Bitcoin', addr: 'bc1qzhxutw8ldh72lw25klmgh4t8dymvasjh2sn3cs' },
  { code: 'ETH', name: 'Ethereum', addr: '0x6654a8ff07cad230af46124620aaa7890a2df1ae' },
  { code: 'BTS', name: 'Bitshares', addr: 'smartnet' },
  { code: 'STH', name: 'SmartHoldem', addr: 'ShTNoUJJMBcJJZrsut8spEG3MeUjxX5Zep' },
  { code: 'GRAM', name: 'TON', addr: 'UQC9kwYlI0qGrox2heB5SOm9_cpCeLg9QcEEoJYU9QEJNaae' },
  { code: 'DOGE', name: 'Dogecoin', addr: 'DFjfANz2UhFmF4Nm63pC7XuojzoQfrtguM' },
  { code: 'LTC', name: 'Litecoin', addr: 'ltc1q55ppyt5vxvlhlwnh4pekehkt9vyh4tv6qafgvm' },
]

const copied = ref('')
let timer: ReturnType<typeof setTimeout> | null = null

async function copy(coin: Coin) {
  try {
    await navigator.clipboard?.writeText(coin.addr)
    copied.value = coin.code
    if (timer) clearTimeout(timer)
    timer = setTimeout(() => { copied.value = '' }, 1600)
  } catch {
    /* clipboard недоступен — тихо игнорируем */
  }
}
</script>

<template>
  <div class="dn-overlay" @click.self="emit('close')" data-testid="donate-modal">
    <div class="dn-card">
      <button class="dn-x" @click="emit('close')" data-testid="donate-close" :aria-label="t('donateModal.close')">✕</button>

      <div class="dn-scan" aria-hidden="true" />

      <h2 class="dn-title" data-testid="donate-title">{{ t('donateModal.title') }}</h2>
      <p class="dn-sub">{{ t('donateModal.subtitle') }}</p>

      <div class="dn-body">
        <p>
          <b>SmartNet</b>{{ t('donateModal.intro1') }}
        </p>
        <p>{{ t('donateModal.intro2') }}</p>
        <p class="dn-cta">{{ t('donateModal.cta') }}</p>
      </div>

      <ul class="dn-coins">
        <li v-for="c in coins" :key="c.code" class="dn-coin" :data-testid="`donate-coin-${c.code}`">
          <div class="dn-coin-head">
            <span class="dn-coin-code">{{ c.code }}</span>
            <span class="dn-coin-name">{{ c.name }}</span>
          </div>
          <div class="dn-coin-row">
            <code class="dn-addr" :data-testid="`donate-addr-${c.code}`">{{ c.addr }}</code>
            <button
              class="dn-copy"
              :class="{ done: copied === c.code }"
              @click="copy(c)"
              :data-testid="`donate-copy-${c.code}`"
              :title="copied === c.code ? t('donateModal.copiedTitle') : t('donateModal.copyTitle')"
            >
              {{ copied === c.code ? t('donateModal.copied') : t('donateModal.copy') }}
            </button>
          </div>
        </li>
      </ul>
    </div>
  </div>
</template>

<style scoped>
.dn-overlay {
  position: fixed;
  inset: 0;
  z-index: 9999;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  background: rgba(3, 6, 5, 0.82);
  backdrop-filter: blur(6px);
  animation: dn-fade 0.2s ease-out;
}
@keyframes dn-fade { from { opacity: 0; } to { opacity: 1; } }

.dn-card {
  position: relative;
  width: 560px;
  max-width: 100%;
  max-height: 88vh;
  overflow-y: auto;
  padding: 28px 30px 26px;
  background: #060b0a;
  border: 1px solid #1c3b34;
  border-radius: 14px;
  box-shadow: 0 0 0 1px rgba(56, 249, 215, 0.12), 0 24px 70px rgba(0, 0, 0, 0.7),
    inset 0 0 40px rgba(56, 249, 215, 0.05);
  font-family: 'JetBrains Mono', 'Fira Code', 'Courier New', monospace;
  color: #cdeee6;
  animation: dn-rise 0.28s cubic-bezier(0.2, 0.9, 0.25, 1);
}
@keyframes dn-rise {
  from { transform: translateY(14px) scale(0.98); opacity: 0; }
  to { transform: translateY(0) scale(1); opacity: 1; }
}

/* лёгкие CRT-сканлайны поверх карточки */
.dn-scan {
  pointer-events: none;
  position: absolute;
  inset: 0;
  border-radius: 14px;
  background: repeating-linear-gradient(
    rgba(56, 249, 215, 0) 0,
    rgba(56, 249, 215, 0) 2px,
    rgba(0, 0, 0, 0.14) 3px
  );
  mix-blend-mode: overlay;
  opacity: 0.5;
}

.dn-x {
  position: absolute;
  top: 12px;
  right: 14px;
  z-index: 3;
  background: transparent;
  border: none;
  color: #5f8f86;
  font-size: 16px;
  cursor: pointer;
  transition: color 0.15s ease, transform 0.15s ease;
}
.dn-x:hover { color: #7ef0c0; transform: scale(1.15); }

.dn-title {
  position: relative;
  z-index: 2;
  margin: 0;
  font-size: 22px;
  font-weight: 800;
  letter-spacing: 0.04em;
  color: #38f9d7;
  text-shadow: 0 0 14px rgba(56, 249, 215, 0.5);
}
.dn-sub {
  position: relative;
  z-index: 2;
  margin: 6px 0 0;
  font-size: 13px;
  color: #7fb8ad;
  letter-spacing: 0.02em;
}

.dn-body {
  position: relative;
  z-index: 2;
  margin-top: 18px;
  display: flex;
  flex-direction: column;
  gap: 11px;
  font-size: 12.5px;
  line-height: 1.65;
  color: #b6d8cf;
}
.dn-body b { color: #eafff9; }
.dn-cta {
  color: #7ef0c0;
  font-weight: 700;
}

.dn-coins {
  position: relative;
  z-index: 2;
  list-style: none;
  margin: 18px 0 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.dn-coin {
  padding: 10px 12px;
  border: 1px solid #16302b;
  border-radius: 10px;
  background: rgba(11, 24, 21, 0.7);
  transition: border-color 0.15s ease, background 0.15s ease;
}
.dn-coin:hover {
  border-color: #2c5b51;
  background: rgba(14, 32, 28, 0.9);
}
.dn-coin-head {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin-bottom: 6px;
}
.dn-coin-code {
  font-size: 12px;
  font-weight: 800;
  letter-spacing: 0.08em;
  color: #38f9d7;
}
.dn-coin-name {
  font-size: 10.5px;
  color: #6f9a91;
  text-transform: uppercase;
  letter-spacing: 0.06em;
}
.dn-coin-row {
  display: flex;
  align-items: center;
  gap: 10px;
}
.dn-addr {
  flex: 1;
  min-width: 0;
  font-size: 11.5px;
  color: #dff5ef;
  word-break: break-all;
  line-height: 1.4;
}
.dn-copy {
  flex-shrink: 0;
  font-family: inherit;
  font-size: 10px;
  font-weight: 800;
  letter-spacing: 0.06em;
  padding: 7px 11px;
  border-radius: 7px;
  cursor: pointer;
  color: #06110e;
  background: #38f9d7;
  border: 1px solid #38f9d7;
  transition: background 0.15s ease, box-shadow 0.15s ease, color 0.15s ease;
  white-space: nowrap;
}
.dn-copy:hover { box-shadow: 0 0 16px rgba(56, 249, 215, 0.6); }
.dn-copy.done {
  background: transparent;
  color: #38f9d7;
}
</style>
