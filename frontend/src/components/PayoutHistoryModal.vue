<script setup lang="ts">
/* eslint-disable no-undef */
/*
 * SmartNet // PayoutHistoryModal
 *
 *   Модалка "История выплат" для страницы MyApplications. Показывает
 *   фактические зачисления от payout-сервера (memo `payout:<appId>`), сумму
 *   и разбивку по dApp'ам. В отличие от `SeederEarnings` (это прогноз/оценка
 *   за текущий цикл выплаты), здесь — уже подтверждённые сетью транзакции.
 */
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useWalletStore } from '@/store/wallet'
import { payoutHistory, type PayoutItem } from '@/lib/bridge'
import { openExplorerTx } from '@/lib/explorer'

// Обозреватель сети STH (dApp): клик по выплате открывает транзакцию
function openTx(id: string) {
  openExplorerTx(id)
}
import { shortAddr } from '@/lib/crypto'

const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()
const wallet = useWalletStore()

const loading = ref(false)
const items = ref<PayoutItem[]>([])
const totalSth = ref('0.0000')
const appsCount = ref(0)
const filter = ref<'all' | string>('all')

// Группировка по dApp'у: суммарно и количество выплат — для быстрого summary.
interface AppAgg { appId: string; appName: string; totalSth: number; count: number }
const perApp = computed<AppAgg[]>(() => {
  const map = new Map<string, AppAgg>()
  for (const it of items.value) {
    const key = it.appId
    const cur = map.get(key) || { appId: key, appName: it.appName, totalSth: 0, count: 0 }
    cur.totalSth += Number(it.amountSth || 0)
    cur.count += 1
    map.set(key, cur)
  }
  return [...map.values()].sort((a, b) => b.totalSth - a.totalSth)
})

const filteredItems = computed(() =>
  filter.value === 'all' ? items.value : items.value.filter((i) => i.appId === filter.value),
)

async function load() {
  if (!wallet.mainAddress) return
  loading.value = true
  try {
    const res = await payoutHistory(wallet.mainAddress)
    items.value = res.items
    totalSth.value = res.totalSth
    appsCount.value = res.apps
  } catch {
    items.value = []
    totalSth.value = '0.0000'
    appsCount.value = 0
  } finally {
    loading.value = false
  }
}

// Форматирование дат: DD.MM.YYYY HH:MM (локально-независимо, ISO-like).
function fmtDate(ts: number): string {
  const d = new Date(ts)
  const pad = (n: number) => n.toString().padStart(2, '0')
  return `${pad(d.getDate())}.${pad(d.getMonth() + 1)}.${d.getFullYear()} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}

onMounted(() => {
  if (props.open) load()
})
watch(
  () => props.open,
  (v) => {
    if (v) load()
  },
)
</script>

<template>
  <teleport to="body">
    <transition name="ph-fade">
      <div v-if="open" class="ph-back" @click.self="emit('close')" data-testid="payout-history-modal">
        <div class="ph-modal">
          <header class="ph-head">
            <div class="ph-titles">
              <h2 class="ph-title" data-testid="payout-history-title">// {{ t('payoutHistory.title') }}_</h2>
              <p class="ph-sub dim">{{ t('payoutHistory.sub') }}</p>
            </div>
            <button class="ph-close" @click="emit('close')" data-testid="payout-history-close">✕</button>
          </header>

          <!-- Summary chips: total / apps / count -->
          <div class="ph-summary" data-testid="payout-history-summary">
            <div class="chip">
              <span class="chip-label dim">{{ t('payoutHistory.total') }}</span>
              <span class="chip-value hl" data-testid="payout-history-total">{{ totalSth }} STH</span>
            </div>
            <div class="chip">
              <span class="chip-label dim">{{ t('payoutHistory.apps') }}</span>
              <span class="chip-value">{{ appsCount }}</span>
            </div>
            <div class="chip">
              <span class="chip-label dim">{{ t('payoutHistory.count') }}</span>
              <span class="chip-value">{{ items.length }}</span>
            </div>
          </div>

          <!-- Per-app filter chips -->
          <div v-if="perApp.length" class="ph-filters" data-testid="payout-history-filters">
            <button
              class="ph-filter"
              :class="{ on: filter === 'all' }"
              @click="filter = 'all'"
              data-testid="payout-filter-all"
            >
              {{ t('payoutHistory.filterAll') }} · {{ items.length }}
            </button>
            <button
              v-for="a in perApp"
              :key="a.appId"
              class="ph-filter"
              :class="{ on: filter === a.appId }"
              @click="filter = a.appId"
              :data-testid="`payout-filter-${a.appId}`"
              :title="a.appId"
            >
              {{ a.appName }} · {{ a.totalSth.toFixed(4) }} STH
            </button>
          </div>

          <!-- Feed of payouts -->
          <div v-if="loading" class="ph-loading dim" data-testid="payout-history-loading">
            {{ t('payoutHistory.loading') }}
          </div>
          <div v-else-if="!filteredItems.length" class="ph-empty" data-testid="payout-history-empty">
            <span class="empty-mark">[ 0.00 STH ]</span>
            <p class="dim">{{ t('payoutHistory.empty') }}</p>
          </div>
          <div v-else class="ph-list" data-testid="payout-history-list">
            <div
              v-for="it in filteredItems"
              :key="it.id"
              class="ph-row ph-click"
              :title="t('payoutHistory.openExplorer')"
              @click="openTx(it.id)"
              :data-testid="`payout-row-${it.id}`"
            >
              <div class="row-main">
                <span class="row-app">{{ it.appName }}</span>
                <span class="row-amount hl">+{{ it.amountSth }} STH</span>
              </div>
              <div class="row-meta">
                <span class="row-date dim">{{ fmtDate(it.timestamp) }}</span>
                <code class="row-payer mono" :title="it.payer">{{ shortAddr(it.payer, 6, 4) }}</code>
              </div>
              <code class="row-memo mono dim">{{ it.memo }}</code>
            </div>
          </div>
        </div>
      </div>
    </transition>
  </teleport>
</template>

<style scoped>
.ph-back {
  position: fixed;
  inset: 0;
  z-index: 200;
  background: rgba(0, 0, 0, 0.6);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 20px;
}
.ph-modal {
  width: 100%;
  max-width: 720px;
  max-height: 82vh;
  background: var(--bg);
  border: 1px solid var(--border-accent);
  border-radius: var(--radius);
  box-shadow: 0 24px 80px rgba(0, 0, 0, 0.55);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.ph-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  padding: 18px 22px;
  background: var(--panel);
  border-bottom: 1px solid var(--border);
}
.ph-titles {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.ph-title {
  margin: 0;
  font-size: 15px;
  color: var(--accent);
  font-weight: var(--fw-800);
  letter-spacing: 0.08em;
}
.ph-sub {
  font-size: 11px;
  letter-spacing: 0.06em;
  line-height: 1.4;
  max-width: 520px;
}
.ph-close {
  font-size: 15px;
  color: var(--text-dim);
  padding: 4px 8px;
}
.ph-close:hover {
  color: var(--danger);
}
.ph-summary {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 10px;
  padding: 16px 22px;
  border-bottom: 1px dashed var(--border);
}
.chip {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 12px 14px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--panel);
}
.chip-label {
  font-size: 10px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
}
.chip-value {
  font-size: 15px;
  font-weight: var(--fw-800);
  color: var(--text);
  font-family: var(--font-mono, monospace);
}
.ph-filters {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  padding: 12px 22px;
  border-bottom: 1px solid var(--border);
}
.ph-filter {
  font-size: 11px;
  padding: 6px 12px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: transparent;
  color: var(--text-dim);
  cursor: pointer;
  transition: border-color 0.15s ease, color 0.15s ease, background-color 0.15s ease;
}
.ph-filter:hover {
  border-color: var(--border-accent);
  color: var(--text);
}
.ph-filter.on {
  border-color: var(--accent);
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  color: var(--accent);
}
.ph-loading,
.ph-empty {
  padding: 60px 22px;
  text-align: center;
  font-size: 13px;
}
.empty-mark {
  display: block;
  font-family: var(--font-mono, monospace);
  color: var(--accent);
  font-weight: var(--fw-700);
  margin-bottom: 8px;
}
.ph-list {
  flex: 1;
  overflow-y: auto;
  padding: 8px 22px 20px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.ph-click { cursor: pointer; }
.ph-row {
  display: flex;
  flex-direction: column;
  gap: 5px;
  padding: 12px 14px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--panel);
  transition: border-color 0.15s ease;
}
.ph-row:hover {
  border-color: var(--border-accent);
}
.row-main {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
}
.row-app {
  font-weight: var(--fw-700);
  color: var(--text);
  font-size: 13px;
}
.row-amount {
  font-family: var(--font-mono, monospace);
  font-size: 13px;
  font-weight: var(--fw-800);
  color: #2ecc71;
}
.row-meta {
  display: flex;
  justify-content: space-between;
  gap: 10px;
  font-size: 11px;
}
.row-payer {
  color: var(--text-dim);
}
.row-memo {
  font-size: 10px;
  color: var(--text-dim);
  word-break: break-all;
  opacity: 0.7;
}
.mono {
  font-family: var(--font-mono, monospace);
}

/* modal fade transition */
.ph-fade-enter-active,
.ph-fade-leave-active {
  transition: opacity 0.2s ease;
}
.ph-fade-enter-from,
.ph-fade-leave-to {
  opacity: 0;
}
</style>
