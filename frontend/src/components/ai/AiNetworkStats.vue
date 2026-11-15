<script setup lang="ts">
/* eslint-disable no-undef */
// Сетевой дашборд AI Stats: суммарные мощности всех активных AI-нод.
import type { AiNetStats } from '@/lib/bridge'
import { fmtCapacityGb, fmtCompute } from '@/lib/aiFormat'
import AiStatTile from './AiStatTile.vue'

defineProps<{
  net: AiNetStats | null
  failed?: boolean
}>()

function fmtTime(ts: number): string {
  return new Date(ts * 1000).toLocaleTimeString()
}
</script>

<template>
  <section class="net-card" data-testid="ai-network-stats">
    <div class="card-head">
      <h2>{{ $t('ai.netTitle') }}</h2>
      <span v-if="net" class="updated">{{ $t('ai.updated') }}: {{ fmtTime(net.updatedAt) }}</span>
    </div>

    <div v-if="failed && !net" class="net-fail" data-testid="ai-net-fail">
      {{ $t('ai.noNet') }}
    </div>

    <div v-else class="tiles">
      <AiStatTile
        :label="$t('ai.activeNodes')"
        :value="net ? String(net.activeNodes) : '—'"
        accent
        testid="ai-net-active-nodes"
      />
      <AiStatTile
        :label="$t('ai.gpuNodes')"
        :value="net ? String(net.gpuNodes) : '—'"
        testid="ai-net-gpu-nodes"
      />
      <AiStatTile
        :label="$t('ai.totalVram')"
        :value="net ? fmtCapacityGb(net.totalVramGb) : '—'"
        testid="ai-net-total-vram"
      />
      <AiStatTile
        :label="$t('ai.totalVramFree')"
        :value="net ? fmtCapacityGb(net.totalVramFreeGb) : '—'"
        testid="ai-net-free-vram"
      />
      <AiStatTile
        :label="$t('ai.totalCompute')"
        :value="net ? fmtCompute(net.totalTflopsFp16) : '—'"
        accent
        testid="ai-net-compute"
      />
      <AiStatTile
        :label="$t('ai.totalRam')"
        :value="net ? fmtCapacityGb(net.totalRamGb) : '—'"
        testid="ai-net-total-ram"
      />
      <AiStatTile
        :label="$t('ai.totalRamFree')"
        :value="net ? fmtCapacityGb(net.totalRamFreeGb) : '—'"
        testid="ai-net-free-ram"
      />
      <AiStatTile
        :label="$t('ai.activeShards')"
        :value="net ? String(net.activeShards ?? 0) : '—'"
        accent
        testid="ai-net-active-shards"
      />
      <AiStatTile
        :label="$t('ai.readyNodes')"
        :value="net ? String(net.shardReadyNodes ?? 0) : '—'"
        testid="ai-net-ready-nodes"
      />
    </div>

    <div v-if="net && net.models && Object.keys(net.models).length" class="models-row" data-testid="ai-net-models">
      <span class="models-label">{{ $t('ai.netModels') }}:</span>
      <span v-for="(cnt, name) in net.models" :key="name" class="model-chip">
        {{ name }} <b>×{{ cnt }}</b>
      </span>
    </div>
  </section>
</template>

<style scoped>
.net-card {
  border: 1px solid rgba(148, 163, 184, 0.14);
  border-radius: 13px;
  background: rgba(10, 16, 24, 0.55);
  padding: 18px 20px;
}
.card-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  margin-bottom: 14px;
}
h2 {
  font-size: 13px;
  letter-spacing: 2px;
  color: #94a3b8;
}
.updated {
  font-size: 11px;
  color: #475569;
  font-family: ui-monospace, monospace;
}
.net-fail {
  padding: 26px 0;
  text-align: center;
  font-size: 12.5px;
  color: #64748b;
}
.tiles {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
  gap: 10px;
}
.models-row {
  margin-top: 14px;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}
.models-label {
  font-size: 11px;
  letter-spacing: 0.8px;
  text-transform: uppercase;
  color: #64748b;
}
.model-chip {
  font-family: ui-monospace, monospace;
  font-size: 11.5px;
  color: #e2e8f0;
  border: 1px solid rgba(34, 211, 238, 0.3);
  border-radius: 999px;
  padding: 3px 10px;
}
.model-chip b {
  color: #22d3ee;
}
</style>
