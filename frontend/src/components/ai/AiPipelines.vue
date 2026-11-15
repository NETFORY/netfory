<script setup lang="ts">
/* eslint-disable no-undef */
// Активные конвейеры AI-сети: какие модели нарезаны на шарды по нодам
// (pipeline parallelism), какие слои уже загружены и где модель целиком.
import { computed } from 'vue'
import type { AiNetStats } from '@/lib/bridge'

const props = defineProps<{
  net: AiNetStats | null
}>()

const pipelines = computed(() => props.net?.pipelines ?? [])

/** живые метрики шлюзов конвейера (TTFT, токены/с) + граф цепочки */
const gateways = computed(() =>
  (props.net?.nodes ?? []).filter(
    (n) => n.pipe && (n.pipe.requests > 0 || (n.pipe.chain?.length ?? 0) > 0),
  ),
)

/** время шага декода шарда-хопа (из heartbeat ноды, матч по диапазону слоёв) */
function hopStep(h: { from: number; to: number }): number | null {
  const tag = `@${h.from}..${h.to}`
  const n = (props.net?.nodes ?? []).find(
    (x) => x.pipe?.stepMs != null && x.models?.some((m) => m.endsWith(tag)),
  )
  return n?.pipe?.stepMs ?? null
}

function shortAddr(a: string): string {
  return a.length > 14 ? `${a.slice(0, 8)}…${a.slice(-4)}` : a
}
</script>

<template>
  <section class="pipes-card" data-testid="ai-pipelines">
    <div class="card-head">
      <h2>{{ $t('ai.pipesTitle') }}</h2>
    </div>

    <div v-if="!pipelines.length" class="empty" data-testid="ai-pipelines-empty">
      {{ $t('ai.noPipes') }}
    </div>

    <div v-else class="pipe-list">
      <div
        v-for="p in pipelines"
        :key="p.model"
        class="pipe-row"
        :data-testid="`ai-pipeline-${p.model}`"
      >
        <div class="pipe-top">
          <span class="model-name">{{ p.model }}</span>
          <span class="counts">
            <b>{{ p.shards }}</b> {{ $t('ai.pipeShards') }}
            <template v-if="p.fullNodes">
              · <b>{{ p.fullNodes }}</b> {{ $t('ai.pipeFullNodes') }}
            </template>
          </span>
        </div>
        <div v-if="p.ranges.length" class="ranges">
          <span class="ranges-label">{{ $t('ai.pipeLayers') }}:</span>
          <span v-for="(r, i) in p.ranges" :key="`${r}-${i}`" class="range-chip">{{ r }}</span>
        </div>
      </div>
    </div>
    <div v-if="gateways.length" class="gw-metrics" data-testid="ai-pipe-metrics">
      <div v-for="g in gateways" :key="g.node" class="gw-row">
        <span class="gw-addr" :title="g.node">{{ shortAddr(g.node) }}</span>
        <template v-if="g.pipe!.requests > 0">
        <span class="gw-stat">TTFT <b>{{ g.pipe!.ttftMs }}</b> ms</span>
        <span class="gw-stat"><b>{{ g.pipe!.tokPerSec }}</b> tok/s</span>
        <span class="gw-stat">{{ $t('ai.pipeReq') }}: <b>{{ g.pipe!.requests }}</b></span>
        <span v-if="g.pipe!.batch > 1" class="gw-stat">batch <b>{{ g.pipe!.batch }}</b></span>
        <span
          v-if="g.pipe!.maxSlots"
          class="gw-stat slots-stat"
          data-testid="ai-pipe-slots-stat"
          :title="$t('ai.pipeSlotsTitle')"
        >
          {{ $t('ai.pipeSlots') }}: <b>{{ g.pipe!.slots ?? 0 }}/{{ g.pipe!.maxSlots }}</b>
        </span>
        <span
          v-if="g.pipe!.specRounds"
          class="gw-stat spec-stat"
          data-testid="ai-pipe-spec-stat"
          :title="$t('ai.pipeSpecTitle')"
        >
          spec ×{{ ((g.pipe!.specAccepted ?? 0) / g.pipe!.specRounds!).toFixed(1) }}
        </span>
        <span
          v-if="g.pipe!.prefixHits"
          class="gw-stat cache-stat"
          data-testid="ai-pipe-cache-stat"
        >
          {{ $t('ai.pipeCacheHits') }}: <b>{{ g.pipe!.prefixHits }}</b>
          <template v-if="g.pipe!.ttftCacheMs && g.pipe!.ttftFullMs">
            · TTFT <b>{{ g.pipe!.ttftCacheMs }}</b> ms
            ({{ $t('ai.pipeNoCacheTtft') }} <b>{{ g.pipe!.ttftFullMs }}</b> ms)
          </template>
        </span>
        </template>
        <!-- живой граф роя: шлюз → шарды с типами слоёв и RTT хопов -->
        <div
          v-if="g.pipe!.chain?.length"
          class="chain-graph"
          data-testid="ai-pipe-chain"
          :title="$t('ai.pipeChainTitle')"
        >
          <span class="hop gw-hop">
            <b class="hop-name">GW</b>
          </span>
          <template v-for="(h, i) in g.pipe!.chain" :key="`${h.id}-${i}`">
            <span class="hop-arrow">
              <i>→</i>
              <em v-if="h.rttMs != null" data-testid="ai-pipe-hop-rtt">{{ h.rttMs }}ms</em>
            </span>
            <span
              class="hop"
              :class="{ 'hop-hybrid': h.rec != null }"
              :title="`ai://${h.id}`"
              :data-testid="`ai-pipe-hop-${i}`"
            >
              <b class="hop-name">S{{ i }}</b>
              <span class="hop-layers">{{ h.from }}–{{ h.to }}</span>
              <span class="hop-kinds">
                <b
                  v-if="h.rec != null"
                  class="kind-rec"
                  :title="$t('ai.pipeRecTitle')"
                >{{ h.rec }}Δ</b>
                <b class="kind-attn" :title="$t('ai.pipeAttnTitle')">{{
                  h.attn ?? h.to - h.from
                }}A</b>
                <b
                  v-if="hopStep(h) != null"
                  class="hop-step"
                  :title="$t('ai.pipeStepTitle')"
                  :data-testid="`ai-pipe-hop-step-${i}`"
                >⏱{{ hopStep(h) }}ms</b>
              </span>
            </span>
          </template>
          <span class="hop-arrow"><i>→</i></span>
          <span class="hop gw-hop"><b class="hop-name">GW</b></span>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
.pipes-card {
  margin-top: 14px;
  border: 1px solid rgba(148, 163, 184, 0.14);
  border-radius: 13px;
  background: rgba(10, 16, 24, 0.55);
  padding: 18px 20px;
}
.card-head {
  margin-bottom: 12px;
}
h2 {
  font-size: 13px;
  letter-spacing: 2px;
  color: #94a3b8;
}
.empty {
  padding: 18px 0;
  text-align: center;
  font-size: 12px;
  color: #64748b;
  line-height: 1.5;
}
.pipe-list {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.pipe-row {
  border: 1px solid rgba(52, 211, 153, 0.18);
  border-radius: 10px;
  background: rgba(52, 211, 153, 0.04);
  padding: 10px 14px;
}
.pipe-top {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}
.model-name {
  font-family: ui-monospace, monospace;
  font-size: 13px;
  font-weight: 700;
  color: #34d399;
}
.counts {
  font-size: 11.5px;
  color: #94a3b8;
}
.counts b {
  color: #e2e8f0;
}
.ranges {
  margin-top: 8px;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}
.ranges-label {
  font-size: 10.5px;
  letter-spacing: 0.8px;
  text-transform: uppercase;
  color: #64748b;
}
.range-chip {
  font-family: ui-monospace, monospace;
  font-size: 11px;
  color: #e2e8f0;
  border: 1px solid rgba(34, 211, 238, 0.3);
  border-radius: 999px;
  padding: 2px 9px;
}
.gw-metrics {
  margin-top: 12px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.gw-row {
  display: flex;
  align-items: center;
  gap: 14px;
  flex-wrap: wrap;
  border: 1px solid rgba(34, 211, 238, 0.16);
  border-radius: 9px;
  padding: 7px 12px;
  font-size: 11.5px;
}
.gw-addr {
  font-family: ui-monospace, monospace;
  color: #22d3ee;
}
.gw-stat {
  color: #94a3b8;
}
.gw-stat b {
  color: #e2e8f0;
  font-family: ui-monospace, monospace;
}
.cache-stat {
  color: #34d399;
}
.cache-stat b {
  color: #34d399;
}
.spec-stat {
  color: #fbbf24;
  font-family: ui-monospace, monospace;
}
.slots-stat {
  color: #a78bfa;
}
.slots-stat b {
  color: #a78bfa;
}
.chain-graph {
  width: 100%;
  margin-top: 6px;
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 4px 2px;
}
.hop {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  border: 1px solid rgba(34, 211, 238, 0.28);
  border-radius: 8px;
  background: rgba(34, 211, 238, 0.05);
  padding: 4px 9px;
  font-size: 10.5px;
}
.hop-hybrid {
  border-color: rgba(167, 139, 250, 0.4);
  background: rgba(167, 139, 250, 0.06);
}
.gw-hop {
  border-color: rgba(52, 211, 153, 0.35);
  background: rgba(52, 211, 153, 0.06);
}
.hop-name {
  font-family: ui-monospace, monospace;
  font-size: 10.5px;
  color: #e2e8f0;
}
.gw-hop .hop-name {
  color: #34d399;
}
.hop-layers {
  font-family: ui-monospace, monospace;
  color: #94a3b8;
}
.hop-kinds {
  display: inline-flex;
  gap: 5px;
}
.kind-rec {
  font-family: ui-monospace, monospace;
  color: #a78bfa;
}
.kind-attn {
  font-family: ui-monospace, monospace;
  color: #22d3ee;
}
.hop-step {
  font-family: ui-monospace, monospace;
  color: #fbbf24;
}
.hop-arrow {
  display: inline-flex;
  flex-direction: column;
  align-items: center;
  min-width: 30px;
  line-height: 1.1;
}
.hop-arrow i {
  font-style: normal;
  color: #475569;
  font-size: 12px;
}
.hop-arrow em {
  font-style: normal;
  font-family: ui-monospace, monospace;
  font-size: 9px;
  color: #64748b;
}
</style>
