<script setup lang="ts">
/* eslint-disable no-undef */
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useEarningsStore } from '@/store/earnings'

const props = defineProps<{ appId?: string; compact?: boolean }>()
const { t } = useI18n()
const earnings = useEarningsStore()

// Живой тик (раз в секунду) для обратного отсчёта до закрытия текущих суток.
const nowMs = ref(Date.now())
let timer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  earnings.refresh()
  timer = setInterval(() => (nowMs.value = Date.now()), 1000)
})
onUnmounted(() => timer && clearInterval(timer))

const app = computed(() => (props.appId ? earnings.byApp(props.appId) : null))
const list = computed(() => earnings.activeApps)

// Секунд прошло с момента снимка с трекеров — чтобы отсчёт «жил» без перезапроса.
const elapsed = computed(() =>
  earnings.loadedAt > 0 ? Math.floor((nowMs.value - earnings.loadedAt) / 1000) : 0,
)
function liveNext(secs: number): number {
  return Math.max(0, secs - elapsed.value)
}

// Прогноз показываем ТОЛЬКО когда фактическая оценка ещё нулевая.
function showForecast(estSth: number, forecastSth: number): boolean {
  return estSth <= 0 && forecastSth > 0
}
const forecastTotal = computed(() => list.value.reduce((s, a) => s + (a.forecastSth || 0), 0))
const allZero = computed(() => earnings.totalEstSth <= 0)
// Ближайшее закрытие суток среди активных dApp (для подсказки-отсчёта).
const nextPayout = computed(() => {
  const xs = list.value.map((a) => liveNext(a.nextPayoutSecs || 0)).filter((s) => s > 0)
  return xs.length ? Math.min(...xs) : 0
})

function hrs(secs: number): string {
  const h = Math.floor(secs / 3600)
  const m = Math.floor((secs % 3600) / 60)
  if (h > 0) return `${h}ч ${m}м`
  return `${m}м`
}
function left(secs: number): string {
  if (secs <= 0) return '—'
  const d = Math.floor(secs / 86400)
  const h = Math.floor((secs % 86400) / 3600)
  return d > 0 ? `${d}д ${h}ч` : `${h}ч`
}
// Обратный отсчёт с секундами (для «первая оценка через …»).
function countdown(secs: number): string {
  if (secs <= 0) return '—'
  const d = Math.floor(secs / 86400)
  const h = Math.floor((secs % 86400) / 3600)
  const m = Math.floor((secs % 3600) / 60)
  const s = secs % 60
  if (d > 0) return `${d}д ${h}ч ${m}м`
  if (h > 0) return `${h}ч ${m}м ${String(s).padStart(2, '0')}с`
  return `${m}м ${String(s).padStart(2, '0')}с`
}
</script>

<template>
  <!-- Per-dApp compact widget (on an app page/card) — скрываем при нулевом начислении -->
  <template v-if="props.appId">
    <div v-if="app && app.estSth > 0" class="earn-widget" data-testid="dapp-earn-widget">
      <div class="ew-head">
        <span class="ew-ico">⬡</span>
        <span class="ew-title">{{ t('earnings.myAccrual') }}</span>
        <span v-if="app.online" class="ew-live" :title="t('earnings.online')">●</span>
      </div>
      <div class="ew-est" data-testid="dapp-earn-est">≈ {{ app.estSth.toFixed(4) }} STH</div>
      <div class="ew-rows">
        <div class="ew-row"><span class="dim">{{ t('earnings.bank') }}</span><b>{{ app.bankSth }} STH</b></div>
        <div class="ew-row"><span class="dim">{{ t('earnings.myUptime') }}</span><b>{{ hrs(app.myOnlineSecs) }}</b></div>
        <div class="ew-row"><span class="dim">{{ t('earnings.seeders') }}</span><b>{{ app.seeders }}</b></div>
        <div class="ew-row"><span class="dim">{{ t('earnings.left') }}</span><b>{{ left(app.secondsRemaining) }}</b></div>
      </div>
    </div>
  </template>

  <!-- Aggregate list (Earn tab) — только без appId -->
  <div v-else class="earn-net" data-testid="network-earn-widget">
    <div class="en-head">
      <span class="en-title">{{ t('earnings.networkTitle') }}</span>
      <span
        v-if="allZero && forecastTotal > 0"
        class="en-total forecast"
        data-testid="network-earn-total"
      >
        ~ {{ forecastTotal.toFixed(4) }} STH
        <span class="fc-tag">{{ t('earnings.forecastTag') }}</span>
      </span>
      <span v-else class="en-total" data-testid="network-earn-total">≈ {{ earnings.totalEstSth.toFixed(4) }} STH</span>
    </div>
    <p class="en-sub dim">{{ t('earnings.networkSub') }}</p>
    <!-- Подсказка-отсчёт: почему всё в нулях и когда появится первая оценка -->
    <p v-if="allZero && list.length" class="fc-note dim" data-testid="network-earn-note">
      {{ t('earnings.forecastHint') }}
      <template v-if="nextPayout > 0">
        <br />
        <b class="fc-clock" data-testid="network-earn-first-in">{{ t('earnings.firstIn', { t: countdown(nextPayout) }) }}</b>
      </template>
    </p>
    <div v-if="list.length" class="en-list">
      <div v-for="a in list" :key="a.id" class="en-item" :data-testid="`network-earn-${a.id}`">
        <span class="en-name">
          <span class="en-dot" :class="{ on: a.online }" />
          {{ a.name || 'sth://' + a.id.slice(0, 8) }}
        </span>
        <span class="en-meta dim">{{ a.seeders }} 👥 · {{ left(a.secondsRemaining) }}</span>
        <span
          v-if="showForecast(a.estSth, a.forecastSth)"
          class="en-amt forecast"
          :title="t('earnings.forecastTag')"
          :data-testid="`network-earn-amt-${a.id}`"
        >~ {{ a.forecastSth.toFixed(4) }}</span>
        <span v-else class="en-amt" :data-testid="`network-earn-amt-${a.id}`">≈ {{ a.estSth.toFixed(4) }}</span>
      </div>
    </div>
    <p v-else class="ew-empty dim" data-testid="network-earn-empty">{{ t('earnings.noActive') }}</p>
  </div>
</template>

<style scoped>
.earn-widget {
  padding: 12px 14px;
  border: 1px solid rgba(52, 211, 153, 0.3);
  border-radius: var(--radius);
  background: rgba(52, 211, 153, 0.06);
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.ew-head {
  display: flex;
  align-items: center;
  gap: 7px;
  font-size: 11px;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.ew-ico { color: #34d399; }
.ew-live { color: #34d399; margin-left: auto; animation: epulse 1.6s ease-in-out infinite; }
.ew-est {
  font-size: 18px;
  font-weight: var(--fw-800);
  color: #34d399;
  text-shadow: 0 0 14px rgba(52, 211, 153, 0.4);
}
.ew-rows { display: grid; grid-template-columns: 1fr 1fr; gap: 4px 14px; }
.ew-row { display: flex; justify-content: space-between; font-size: 12px; }
.ew-empty { font-size: 12px; padding: 4px 0; }

/* --- Прогноз (показывается только при нулевой фактической оценке) --- */
.forecast { color: #fbbf24 !important; text-shadow: 0 0 14px rgba(251, 191, 36, 0.35); }
.fc-tag {
  font-size: 9px;
  font-weight: var(--fw-700);
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: #0e0f12;
  background: #fbbf24;
  border-radius: 999px;
  padding: 2px 7px;
  margin-left: 7px;
  vertical-align: middle;
  text-shadow: none;
}
.fc-hint { font-size: 11px; margin-top: 2px; }
.fc-note {
  font-size: 11px;
  line-height: 1.6;
  padding: 8px 10px;
  border: 1px dashed rgba(251, 191, 36, 0.4);
  border-radius: var(--radius);
  background: rgba(251, 191, 36, 0.05);
  margin: 2px 0 4px;
}
.fc-clock { color: #fbbf24; font-variant-numeric: tabular-nums; letter-spacing: 0.02em; }

.earn-net {
  padding: 16px 18px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--bg-elevated);
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.en-head { display: flex; align-items: baseline; justify-content: space-between; }
.en-title {
  font-size: 11px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.en-total {
  font-size: 18px;
  font-weight: var(--fw-800);
  color: #34d399;
  text-shadow: 0 0 14px rgba(52, 211, 153, 0.4);
}
.en-sub { font-size: 11px; margin-bottom: 4px; }
.en-list { display: flex; flex-direction: column; gap: 6px; }
.en-item {
  display: grid;
  grid-template-columns: 1fr auto auto;
  align-items: center;
  gap: 10px;
  font-size: 12px;
  padding: 6px 0;
  border-top: 1px solid var(--border);
}
.en-name { display: flex; align-items: center; gap: 7px; font-weight: var(--fw-700); }
.en-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--text-dim); }
.en-dot.on { background: #34d399; box-shadow: 0 0 7px #34d399; }
.en-meta { font-size: 11px; }
.en-amt { color: #34d399; font-weight: var(--fw-700); }
@keyframes epulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.35; } }
</style>
