<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { netConnStats, type NetConnStats } from '@/lib/bridge'

// Диагностика соединений iroh: открытые соединения и ошибки реле за минуту по endpoint'ам.
const { t } = useI18n()
const st = ref<NetConnStats | null>(null)
let timer: ReturnType<typeof setInterval> | null = null

// null после удачного ответа (команда на миг недоступна) — держим последние значения
async function load() {
  const r = await netConnStats()
  if (r || !st.value) st.value = r
}
const short = (id: string) => (id ? id.slice(0, 8) + '…' + id.slice(-4) : '—')
const host = (u: string) => u.replace(/^https?:\/\//, '').replace(/\/$/, '') || '—'
const totalOpen = computed(() => (st.value?.endpoints || []).reduce((a, e) => a + e.open, 0))
const totalOpened = computed(() => (st.value?.endpoints || []).reduce((a, e) => a + e.openedMin, 0))

onMounted(() => {
  void load()
  timer = setInterval(() => void load(), 5000)
})
onUnmounted(() => {
  if (timer) clearInterval(timer)
})
</script>

<template>
  <div class="dht-diag cs" :class="{ storm: st?.storm }" data-testid="conn-stats">
    <div class="dht-diag-head">
      <span class="mono-label">{{ t('connStats.title') }}</span>
      <button class="mini-refresh" @click="load" :title="t('connStats.refresh')" data-testid="conn-stats-refresh">↻</button>
    </div>
    <p v-if="!st" class="hint dim" data-testid="conn-stats-off">{{ t('connStats.off') }}</p>
    <template v-else>
      <p class="cs-state mono" :class="st.storm ? 'bad' : 'ok'" data-testid="conn-stats-state">
        <i class="cs-dot" />{{ st.storm ? t('connStats.storm') : t('connStats.calm') }}
        <em>· {{ t('connStats.window', { s: st.windowSecs }) }}</em>
      </p>
      <div class="dht-diag-grid">
        <div class="dht-diag-stat">
          <span class="dht-diag-val" data-testid="conn-stats-open">{{ totalOpen }}</span>
          <span class="dht-diag-key dim">{{ t('connStats.open') }}</span>
        </div>
        <div class="dht-diag-stat">
          <span class="dht-diag-val" :class="{ warn: totalOpened >= 40 }" data-testid="conn-stats-opened">{{ totalOpened }}</span>
          <span class="dht-diag-key dim">{{ t('connStats.openedMin') }}</span>
        </div>
        <div class="dht-diag-stat">
          <span class="dht-diag-val" :class="{ warn: st.connectErrorsMin >= 20 }" data-testid="conn-stats-errors">{{ st.connectErrorsMin }}</span>
          <span class="dht-diag-key dim">{{ t('connStats.errorsMin') }}</span>
        </div>
        <div class="dht-diag-stat">
          <span class="dht-diag-val" :class="{ warn: st.relayErrorsMin >= 10 }" data-testid="conn-stats-relay-errors">{{ st.relayErrorsMin }}</span>
          <span class="dht-diag-key dim">{{ t('connStats.relayErrorsMin') }}</span>
        </div>
      </div>
      <table class="cs-table mono" data-testid="conn-stats-table">
        <thead>
          <tr><th>{{ t('connStats.ep') }}</th><th>{{ t('connStats.colOpen') }}</th><th>+/−{{ t('connStats.perMin') }}</th><th>{{ t('connStats.colRelay') }}</th><th>{{ t('connStats.colHome') }}</th></tr>
        </thead>
        <tbody>
          <tr v-for="e in st.endpoints" :key="e.name" :class="{ down: !e.up }" :data-testid="`conn-stats-ep-${e.name}`">
            <td :title="e.nodeId"><b>{{ t(`connStats.ep_${e.name}`) }}</b><br /><span class="dim">{{ e.up ? short(e.nodeId) : t('connStats.down') }}</span></td>
            <td>{{ e.open }}</td>
            <td>+{{ e.openedMin }} / −{{ e.closedMin }}</td>
            <td :class="{ warnc: e.relayChangesMin >= 3 }">{{ e.relayChangesMin }}</td>
            <td class="dim">{{ host(e.homeRelay) }}</td>
          </tr>
        </tbody>
      </table>
      <p v-if="st.lastError" class="cs-err mono" data-testid="conn-stats-last-error">{{ t('connStats.lastError') }}: {{ st.lastError }}</p>
      <p class="hint dim">{{ t('connStats.hint') }}</p>
    </template>
  </div>
</template>

<style scoped>
/* стили плиток как у карточки DHT (те объявлены scoped в Settings.vue и сюда не доходят) */
.dht-diag-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 10px; }
.dht-diag-grid { display: grid; grid-template-columns: repeat(2, 1fr); gap: 10px 14px; }
.dht-diag-stat { display: flex; flex-direction: column; gap: 2px; padding: 8px 10px; border-radius: 8px; background: rgba(255, 255, 255, 0.03); border: 1px solid var(--border); }
.dht-diag-val { font-size: 18px; font-weight: var(--fw-800); font-variant-numeric: tabular-nums; color: #34d399; }
.dht-diag-key { font-size: 11px; letter-spacing: 0.03em; }
.mini-refresh { background: none; border: 1px solid var(--border); border-radius: 7px; color: var(--accent, #64b5ef); width: 26px; height: 24px; cursor: pointer; font-size: 13px; line-height: 1; transition: background-color 0.15s ease, transform 0.15s ease; }
.mini-refresh:hover { background: rgba(100, 181, 239, 0.12); transform: rotate(90deg); }
.cs.storm { border-color: rgba(248, 113, 113, 0.5); background: rgba(248, 113, 113, 0.05); }
.cs-state { display: flex; align-items: center; gap: 8px; font-size: 12px; margin: 0 0 10px; }
.cs-state em { color: var(--text-dim); font-style: normal; }
.cs-dot { width: 8px; height: 8px; border-radius: 50%; background: #34d399; box-shadow: 0 0 8px rgba(52, 211, 153, 0.6); }
.cs-state.bad { color: #f87171; }
.cs-state.bad .cs-dot { background: #f87171; box-shadow: 0 0 8px rgba(248, 113, 113, 0.7); animation: cs-pulse 1.2s ease-in-out infinite; }
.dht-diag-val.warn { color: #f87171; }
.cs-table { width: 100%; margin-top: 12px; border-collapse: collapse; font-size: 11px; }
.cs-table th { text-align: left; font-weight: 500; color: var(--text-dim); letter-spacing: 0.06em; padding: 4px 6px; border-bottom: 1px solid var(--border); }
.cs-table td { padding: 6px; border-bottom: 1px solid rgba(255, 255, 255, 0.04); vertical-align: top; word-break: break-all; }
.cs-table tr.down td { opacity: 0.5; }
.cs-table .warnc { color: #f87171; }
.cs-err { margin: 10px 0 0; font-size: 11px; color: #f87171; word-break: break-word; }
@keyframes cs-pulse { 50% { opacity: 0.4; } }
</style>
