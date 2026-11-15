<script setup lang="ts">
/* eslint-disable no-undef */
// Мини-график динамики продаж альбома (бары по дням/неделям) из истории albumbuy:.
// Показывается автору на странице альбома. Только фронтенд-агрегация точек.
import { ref, computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{ points: { ts: number; amount: number }[] }>()
const { t } = useI18n()

const mode = ref<'day' | 'week'>('day')
const DAY = 86400000
const WEEK = DAY * 7

// Начало суток (UTC) для ts.
function dayStart(ts: number): number {
  return Math.floor(ts / DAY) * DAY
}
// Начало ISO-недели (понедельник, UTC).
function weekStart(ts: number): number {
  const d = new Date(dayStart(ts))
  const dow = (d.getUTCDay() + 6) % 7 // 0 = понедельник
  return dayStart(ts) - dow * DAY
}

interface Bucket {
  key: number
  count: number
  revenue: number
  label: string
}

const buckets = computed<Bucket[]>(() => {
  if (!props.points.length) return []
  const step = mode.value === 'day' ? DAY : WEEK
  const startOf = mode.value === 'day' ? dayStart : weekStart
  const maxBuckets = mode.value === 'day' ? 30 : 12
  // диапазон: от первой продажи (или maxBuckets назад) до сегодня
  const now = Date.now()
  const first = startOf(props.points[0].ts)
  const earliest = startOf(now) - (maxBuckets - 1) * step
  const from = Math.max(first, earliest)
  const to = startOf(now)
  const map = new Map<number, Bucket>()
  for (let k = from; k <= to; k += step) {
    map.set(k, { key: k, count: 0, revenue: 0, label: fmtLabel(k) })
  }
  for (const p of props.points) {
    const k = startOf(p.ts)
    const b = map.get(k)
    if (b) {
      b.count += 1
      b.revenue += p.amount
    }
  }
  return Array.from(map.values())
})

function fmtLabel(ts: number): string {
  const d = new Date(ts)
  const dd = String(d.getUTCDate()).padStart(2, '0')
  const mm = String(d.getUTCMonth() + 1).padStart(2, '0')
  return `${dd}.${mm}`
}

const maxCount = computed(() => Math.max(1, ...buckets.value.map((b) => b.count)))
const totalSales = computed(() => buckets.value.reduce((s, b) => s + b.count, 0))
const peak = computed(() => Math.max(0, ...buckets.value.map((b) => b.count)))
</script>

<template>
  <div class="spark" data-testid="album-sales-chart">
    <div class="spark-head">
      <span class="spark-title">📈 {{ t('music.salesDynamics') }}</span>
      <div class="spark-modes">
        <button
          class="sm-btn"
          :class="{ on: mode === 'day' }"
          @click="mode = 'day'"
          data-testid="chart-mode-day"
        >{{ t('music.byDay') }}</button>
        <button
          class="sm-btn"
          :class="{ on: mode === 'week' }"
          @click="mode = 'week'"
          data-testid="chart-mode-week"
        >{{ t('music.byWeek') }}</button>
      </div>
    </div>

    <div v-if="buckets.length" class="bars" :style="{ '--n': buckets.length }">
      <div
        v-for="b in buckets"
        :key="b.key"
        class="bar-col"
        :title="`${b.label} · ${b.count} · ${b.revenue} STH`"
        :data-testid="`chart-bar-${b.key}`"
      >
        <div class="bar-track">
          <div class="bar-fill" :style="{ height: (b.count / maxCount) * 100 + '%' }"></div>
        </div>
      </div>
    </div>
    <p v-else class="spark-empty">{{ t('music.noSalesYet') }}</p>

    <div class="spark-foot">
      <span>{{ t('music.chartTotal', { n: totalSales }) }}</span>
      <span>{{ t('music.chartPeak', { n: peak }) }}</span>
    </div>
  </div>
</template>

<style scoped>
.spark {
  margin-top: 14px;
  padding: 14px;
  border: 1px solid rgba(0, 229, 255, 0.18);
  border-radius: 12px;
  background: rgba(255, 255, 255, 0.03);
  max-width: 520px;
}
.spark-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
}
.spark-title {
  font-size: 12px;
  font-weight: 700;
  color: #eaf6ff;
  letter-spacing: 0.04em;
}
.spark-modes {
  display: flex;
  gap: 6px;
}
.sm-btn {
  padding: 4px 10px;
  font-size: 10px;
  border-radius: 999px;
  border: 1px solid rgba(0, 229, 255, 0.25);
  background: transparent;
  color: #7fb4cf;
  cursor: pointer;
  transition: all 0.15s ease;
}
.sm-btn.on {
  background: linear-gradient(135deg, #00e5ff, #a855f7);
  color: #06121e;
  border-color: transparent;
  font-weight: 700;
}
.bars {
  display: grid;
  grid-template-columns: repeat(var(--n), 1fr);
  align-items: end;
  gap: 3px;
  height: 90px;
}
.bar-col {
  height: 100%;
  display: flex;
  align-items: flex-end;
}
.bar-track {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: flex-end;
  border-radius: 3px;
  background: rgba(255, 255, 255, 0.04);
}
.bar-fill {
  width: 100%;
  min-height: 2px;
  border-radius: 3px 3px 0 0;
  background: linear-gradient(180deg, #a855f7, #00e5ff);
  box-shadow: 0 0 6px rgba(0, 229, 255, 0.4);
  transition: height 0.2s ease;
}
.spark-empty {
  margin: 0;
  font-size: 11px;
  color: #6f96ad;
  text-align: center;
  padding: 18px 0;
}
.spark-foot {
  display: flex;
  justify-content: space-between;
  margin-top: 10px;
  font-size: 11px;
  color: #7fb4cf;
}
</style>
