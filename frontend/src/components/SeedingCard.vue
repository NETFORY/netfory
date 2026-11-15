<script setup lang="ts">
import { onMounted, computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useSeedingStore } from '@/store/seeding'
import SeederEarnings from '@/components/SeederEarnings.vue'

const { t } = useI18n()
const seeding = useSeedingStore()

onMounted(() => seeding.init())

const active = computed(() => seeding.isSeedingEnabled && seeding.metrics.status === 'ACTIVE')

/* ---- 7-day history chart (P1) ---- */
const history = computed(() => seeding.history)
const maxEarn = computed(() =>
  Math.max(0.0001, ...history.value.map((d) => d.estimatedEarningsSth)),
)
const hasHistory = computed(() =>
  history.value.some((d) => d.estimatedEarningsSth > 0 || d.avgUptimePercent > 0),
)
/* SVG polyline points (viewBox 0..100) mapping uptime% across the 7 days */
const uptimePoints = computed(() => {
  const h = history.value
  if (!h.length) return ''
  return h
    .map((d, i) => {
      const x = h.length > 1 ? (i / (h.length - 1)) * 100 : 50
      const y = 100 - Math.max(0, Math.min(100, d.avgUptimePercent))
      return `${x.toFixed(2)},${y.toFixed(2)}`
    })
    .join(' ')
})
function barH(earn: number): number {
  return Math.round((earn / maxEarn.value) * 100)
}
const hover = ref(-1)

// Earnings model mirror (matches seeding.rs). The ACTUAL estimate comes from the
// backend (it depends on seeded GB + active apps). Only the POTENTIAL max is
// re-scaled live here so dragging the disk slider updates the ceiling instantly.
const BASE_REWARD = 5.0
const MAX_K_UP = 2.5
const PER_APP_REWARD = 0.75
const density = computed(() => seeding.metrics.densityPerGb || 0)
const liveEstimate = computed(() => {
  if (!seeding.isSeedingEnabled || seeding.metrics.maxDailyEarnings <= 0) return 0
  return seeding.metrics.estDailyEarnings
})
const liveMax = computed(() => {
  if (!seeding.isSeedingEnabled || seeding.metrics.maxDailyEarnings <= 0) return 0
  const appBonus = (seeding.metrics.activeApps || 0) * PER_APP_REWARD
  return (BASE_REWARD + seeding.storageLimitGb * density.value + appBonus) * MAX_K_UP
})
const belowPeak = computed(() => liveMax.value > 0 && (seeding.metrics.kUp || 1) < MAX_K_UP - 0.01)
// While Earn STH is ON but no calculation has arrived yet, show a forecasting
// placeholder instead of a misleading ≈ 0.0000 STH.
// "Calculating…" until the first metrics sample arrives AND yields a real
// (non-zero) forecast — avoids the scary "0.0000" flash right after enabling.
const computing = computed(
  () => seeding.isSeedingEnabled && (!seeding.received || liveEstimate.value <= 0),
)

function onToggle() {
  seeding.toggleSeeding(!seeding.isSeedingEnabled)
}
function onSlider(e: Event) {
  seeding.setLimit(Number((e.target as HTMLInputElement).value))
}
// Human-friendly disk size: TB above 1024 GB, else GB.
function diskFmt(gb: number): string {
  if (gb >= 1024) return (gb / 1024).toFixed(2) + ' TB'
  return Math.round(gb) + ' GB'
}
</script>

<template>
  <div class="seed-card panel" data-testid="seeding-card">
    <div class="seed-head">
      <div>
        <h3 class="seed-title">⬡ {{ t('seeding.title') }}</h3>
        <p class="seed-sub dim">{{ t('seeding.subtitle') }}</p>
      </div>
      <button
        class="switch"
        :class="{ on: seeding.isSeedingEnabled }"
        :disabled="seeding.busy || !seeding.available"
        @click="onToggle"
        data-testid="seeding-toggle"
        role="switch"
        :aria-checked="seeding.isSeedingEnabled"
      >
        <span class="knob" />
      </button>
    </div>

    <p class="enable-label" :class="{ on: seeding.isSeedingEnabled }">
      {{ t('seeding.enableLabel') }}
    </p>

    <div v-if="!seeding.available" class="native-only dim" data-testid="seeding-native-only">
      {{ t('seeding.nativeOnly') }}
    </div>

    <template v-else>
      <template v-if="seeding.isSeedingEnabled">
      <div class="slider-row" data-testid="seeding-slider-row">
        <div class="slider-top">
          <span class="dim">{{ t('seeding.diskLabel') }}</span>
          <span class="hl" data-testid="seeding-limit">{{ seeding.storageLimitGb }} GB</span>
        </div>
        <input
          type="range"
          min="1"
          :max="seeding.maxLimitGb"
          step="1"
          :value="seeding.storageLimitGb"
          @change="onSlider"
          class="slider"
          data-testid="seeding-slider"
        />
        <div v-if="seeding.diskTotalGb > 0" class="disk-info dim" data-testid="seeding-disk-info">
          {{ t('seeding.diskCapacity') }}: <b class="hl">{{ diskFmt(seeding.diskTotalGb) }}</b>
          · {{ t('seeding.diskFree') }}: {{ diskFmt(seeding.diskFreeGb) }}
        </div>
      </div>

      <div class="earn-box" data-testid="seeding-earnings">
        <div class="earn-main">
          <span class="earn-lbl dim">{{ t('seeding.earnForecast') }}</span>
          <span v-if="computing" class="earn-val computing" data-testid="seeding-est-computing">
            {{ t('seeding.forecasting') }}<span class="earn-dots"><i>.</i><i>.</i><i>.</i></span>
          </span>
          <span v-else class="earn-val" data-testid="seeding-est-earnings">≈ {{ liveEstimate.toFixed(4) }} STH / {{ t('seeding.perDay') }}</span>
        </div>
        <p v-if="!computing && belowPeak" class="earn-max" data-testid="seeding-max-earnings">
          {{ t('seeding.earnMax', { amount: liveMax.toFixed(4) }) }}
        </p>
      </div>

      <div class="diag" data-testid="seeding-diagnostics">
        <div class="diag-row">
          <span class="dim">{{ t('seeding.statusLabel') }}</span>
          <span class="status">
            <span class="dot" :class="{ live: active }" />
            <b :class="active ? 'hl' : 'dim'">{{ active ? t('seeding.active') : t('seeding.stopped') }}</b>
          </span>
        </div>
        <div class="diag-row">
          <span class="dim">{{ t('seeding.uptimeLabel') }}</span>
          <span class="mono" data-testid="seeding-uptime">
            {{ seeding.metrics.uptime24h.toFixed(1) }}% ·
            <span class="hl">K_up: {{ seeding.metrics.kUp.toFixed(2) }}</span>
          </span>
        </div>
        <div class="diag-row">
          <span class="dim">{{ t('seeding.seededLabel') }}</span>
          <span class="mono" data-testid="seeding-seeded">
            {{ seeding.metrics.seededGb.toFixed(2) }} GB / {{ seeding.metrics.limitGb }} GB
          </span>
        </div>
        <div class="diag-row">
          <span class="dim">{{ t('seeding.appsLabel') }}</span>
          <span class="mono hl" data-testid="seeding-apps">{{ seeding.metrics.activeApps }}</span>
        </div>
      </div>

      <!-- Phase G: реальная оценка начислений по сети (per-app payout model) -->
      <SeederEarnings />
      </template>

      <!-- 7-day earning + uptime history chart -->
      <div class="hist" data-testid="seeding-history">
        <div class="hist-head">
          <span class="hist-title">{{ t('seeding.historyTitle') }}</span>
          <div class="hist-legend">
            <span class="lg lg-earn">{{ t('seeding.chartEarnings') }}</span>
            <span class="lg lg-up">{{ t('seeding.chartUptime') }}</span>
          </div>
        </div>

        <div v-if="hasHistory" class="chart" data-testid="seeding-chart" @mouseleave="hover = -1">
          <!-- uptime overlay line (neon amber) across the day columns -->
          <svg class="chart-line" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
            <polyline :points="uptimePoints" fill="none" stroke="#fbbf24" stroke-width="1.4"
              vector-effect="non-scaling-stroke" stroke-linejoin="round" stroke-linecap="round" />
          </svg>
          <div
            v-for="(d, i) in history"
            :key="d.timestamp"
            class="col"
            :class="{ hot: hover === i }"
            @mouseenter="hover = i"
            :data-testid="`chart-col-${i}`"
          >
            <div class="bar-wrap">
              <div class="bar" :style="{ height: barH(d.estimatedEarningsSth) + '%' }" />
              <span class="up-dot" :style="{ bottom: Math.min(100, d.avgUptimePercent) + '%' }" />
            </div>
            <span class="col-date">{{ d.formattedDate }}</span>

            <div v-if="hover === i" class="tip" :data-testid="`chart-tip-${i}`">
              <div class="tip-date">{{ d.formattedDate }}</div>
              <div class="tip-row"><span class="tip-k earn">◆</span>{{ d.estimatedEarningsSth.toFixed(4) }} STH</div>
              <div class="tip-row"><span class="tip-k up">●</span>{{ d.avgUptimePercent.toFixed(1) }}% {{ t('seeding.chartUptime') }}</div>
            </div>
          </div>
        </div>
        <p v-else class="hist-empty dim" data-testid="seeding-history-empty">{{ t('seeding.noHistory') }}</p>
      </div>
    </template>
  </div>
</template>

<style scoped>
.seed-card {
  padding: 20px 22px;
  display: flex;
  flex-direction: column;
  gap: 14px;
  border-left: 2px solid var(--accent);
}
.seed-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}
.seed-title {
  font-size: 14px;
  font-weight: var(--fw-800);
  letter-spacing: 0.06em;
  color: var(--accent);
}
.seed-sub {
  font-size: 12px;
  margin-top: 4px;
  line-height: 1.5;
}
.switch {
  position: relative;
  width: 52px;
  height: 28px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--bg-elevated);
  cursor: pointer;
  flex: 0 0 52px;
  transition: background-color 0.2s ease, border-color 0.2s ease;
}
.switch.on {
  background: rgba(52, 211, 153, 0.2);
  border-color: rgba(52, 211, 153, 0.5);
}
.switch .knob {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 22px;
  height: 22px;
  border-radius: 50%;
  background: var(--text-dim);
  transition: transform 0.2s ease, background-color 0.2s ease;
}
.switch.on .knob {
  transform: translateX(24px);
  background: #34d399;
}
.enable-label {
  font-size: 12px;
  font-weight: var(--fw-700);
  letter-spacing: 0.04em;
  color: var(--text-dim);
}
.enable-label.on {
  color: #34d399;
}
.native-only {
  font-size: 12px;
  padding: 10px 12px;
  border: 1px dashed var(--border);
  border-radius: var(--radius);
}
.slider-row {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.slider-top {
  display: flex;
  justify-content: space-between;
  font-size: 12px;
}
.slider {
  width: 100%;
  accent-color: var(--accent);
  cursor: pointer;
}
.disk-info {
  font-size: 11px;
  letter-spacing: 0.02em;
  margin-top: 2px;
}
.diag {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 14px 16px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.earn-box {
  padding: 16px 18px;
  border: 1px solid rgba(52, 211, 153, 0.35);
  border-radius: var(--radius);
  background: rgba(52, 211, 153, 0.07);
  box-shadow: 0 0 24px rgba(52, 211, 153, 0.1);
}
.earn-main {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.earn-lbl {
  font-size: 11px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}
.earn-val {
  font-size: 22px;
  font-weight: var(--fw-800);
  color: #34d399;
  text-shadow: 0 0 16px rgba(52, 211, 153, 0.45);
  animation: earnpulse 2.4s ease-in-out infinite;
}
.earn-val.computing {
  font-size: 15px;
  color: #fbbf24;
  text-shadow: 0 0 14px rgba(251, 191, 36, 0.4);
  letter-spacing: 0.02em;
}
.earn-dots i {
  font-style: normal;
  animation: earndots 1.4s infinite;
}
.earn-dots i:nth-child(2) { animation-delay: 0.2s; }
.earn-dots i:nth-child(3) { animation-delay: 0.4s; }
@keyframes earndots {
  0%, 100% { opacity: 0.2; }
  50% { opacity: 1; }
}
@keyframes earnpulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.78; }
}
.earn-max {
  margin-top: 8px;
  font-size: 11.5px;
  line-height: 1.5;
  color: #fbbf24;
}
.diag-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-size: 12px;
}
.status {
  display: flex;
  align-items: center;
  gap: 8px;
}
.dot {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  background: var(--text-dim);
}
.dot.live {
  background: #34d399;
  box-shadow: 0 0 8px #34d399;
  animation: pulse 1.6s ease-in-out infinite;
}
@keyframes pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.4; }
}

/* ---- 7-day history chart ---- */
.hist {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 16px 16px 12px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.hist-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}
.hist-title {
  font-size: 11px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.hist-legend {
  display: flex;
  gap: 12px;
  font-size: 10px;
}
.lg {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  color: var(--text-dim);
}
.lg::before {
  content: '';
  width: 8px;
  height: 8px;
  border-radius: 2px;
}
.lg-earn::before { background: #34d399; }
.lg-up::before { background: #fbbf24; border-radius: 50%; }
.chart {
  position: relative;
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 6px;
  height: 110px;
  padding-top: 6px;
}
.chart-line {
  position: absolute;
  left: 0;
  right: 0;
  top: 6px;
  bottom: 22px;
  width: 100%;
  height: calc(100% - 28px);
  pointer-events: none;
  opacity: 0.85;
  z-index: 2;
}
.col {
  position: relative;
  flex: 1 1 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  height: 100%;
  cursor: pointer;
}
.bar-wrap {
  position: relative;
  width: 100%;
  flex: 1;
  display: flex;
  align-items: flex-end;
  justify-content: center;
}
.bar {
  width: 60%;
  max-width: 22px;
  min-height: 2px;
  border-radius: 3px 3px 0 0;
  background: linear-gradient(180deg, #34d399, rgba(52, 211, 153, 0.25));
  box-shadow: 0 0 10px rgba(52, 211, 153, 0.35);
  transition: height 0.3s ease, filter 0.15s ease;
}
.col.hot .bar {
  filter: brightness(1.3);
}
.up-dot {
  position: absolute;
  left: 50%;
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #fbbf24;
  box-shadow: 0 0 7px rgba(251, 191, 36, 0.7);
  transform: translate(-50%, 50%);
  z-index: 3;
}
.col-date {
  font-size: 9.5px;
  color: var(--text-dim);
  letter-spacing: 0.02em;
}
.col.hot .col-date {
  color: var(--accent);
}
.tip {
  position: absolute;
  bottom: 100%;
  left: 50%;
  transform: translateX(-50%);
  margin-bottom: 6px;
  background: var(--panel, #15181d);
  border: 1px solid var(--border-accent, var(--border));
  border-radius: 6px;
  padding: 7px 9px;
  white-space: nowrap;
  z-index: 5;
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.45);
  pointer-events: none;
}
.tip-date {
  font-size: 10px;
  color: var(--text-dim);
  margin-bottom: 4px;
  letter-spacing: 0.06em;
}
.tip-row {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11px;
  font-weight: var(--fw-700);
}
.tip-k.earn { color: #34d399; }
.tip-k.up { color: #fbbf24; }
.hist-empty {
  font-size: 11.5px;
  padding: 14px 0;
  text-align: center;
}

/* ---- NEON CYBER: glow follows accent ---- */
[data-theme='neon'] .switch.on {
  background: var(--accent-soft);
  border-color: var(--accent);
  box-shadow: 0 0 14px -4px var(--accent-glow);
}
[data-theme='neon'] .switch.on .knob {
  background: var(--accent);
}
[data-theme='neon'] .enable-label.on {
  color: var(--accent);
}
[data-theme='neon'] .earn-box {
  border-color: var(--border-accent);
  background: var(--accent-soft);
  box-shadow: 0 0 22px -8px var(--accent-glow);
}
[data-theme='neon'] .earn-val {
  color: var(--accent);
  text-shadow: 0 0 16px var(--accent-glow);
}

</style>
