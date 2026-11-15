<script setup lang="ts">
/* eslint-disable no-undef */
// Ноды AI-сети с версиями бинарников: устаревшие (старее максимальной
// версии своей группы воркер/клиент) подсвечиваются жёлтым.
import { computed } from 'vue'
import type { AiNetStats, AiNodeInfo } from '@/lib/bridge'

const props = defineProps<{
  net: AiNetStats | null
}>()

const nodes = computed(() => props.net?.nodes ?? [])

function cmpVer(a: string, b: string): number {
  const pa = a.split('.').map((n) => parseInt(n, 10) || 0)
  const pb = b.split('.').map((n) => parseInt(n, 10) || 0)
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0)
    if (d) return d
  }
  return 0
}

/** максимальная версия отдельно среди воркеров и клиентов */
const latest = computed(() => {
  const m: Record<'worker' | 'client', string> = { worker: '', client: '' }
  for (const n of nodes.value) {
    if (!n.version) continue
    const k = n.worker ? 'worker' : 'client'
    if (!m[k] || cmpVer(n.version, m[k]) > 0) m[k] = n.version
  }
  return m
})

function isOutdated(n: AiNodeInfo): boolean {
  if (!n.version) return true
  const top = n.worker ? latest.value.worker : latest.value.client
  return !!top && cmpVer(n.version, top) < 0
}

function shortAddr(a: string): string {
  return a.length > 14 ? `${a.slice(0, 8)}…${a.slice(-4)}` : a
}

function fmtVram(n: AiNodeInfo): string {
  if (!n.gpu) return '—'
  return `${(n.vramFreeMb / 1024).toFixed(1)}/${(n.vramTotalMb / 1024).toFixed(1)} GB`
}

/** роль ноды: шлюз (собранный конвейер) / шард (диапазон слоёв) / воркер / клиент */
type Role = 'gateway' | 'shard' | 'worker' | 'client'
function role(n: AiNodeInfo): Role {
  if (!n.worker) return 'client'
  if (n.pipe?.chain?.length) return 'gateway'
  if (n.models.some((m) => m.includes('@'))) return 'shard'
  return 'worker'
}
const roleKey: Record<Role, string> = {
  gateway: 'ai.nodeGateway',
  shard: 'ai.nodeShard',
  worker: 'ai.nodeWorker',
  client: 'ai.nodeClient',
}
</script>

<template>
  <section class="nodes-card" data-testid="ai-nodes">
    <div class="card-head">
      <h2>{{ $t('ai.nodesTitle') }}</h2>
      <span v-if="nodes.length" class="count">{{ nodes.length }}</span>
    </div>

    <div v-if="!nodes.length" class="empty" data-testid="ai-nodes-empty">
      {{ $t('ai.noNodes') }}
    </div>

    <div v-else class="node-list">
      <div
        v-for="n in nodes"
        :key="n.node"
        class="node-row"
        :data-testid="`ai-node-${n.node}`"
      >
        <span class="addr" :title="n.node">{{ shortAddr(n.node) }}</span>
        <span class="kind" :class="`k-${role(n)}`" :data-testid="`ai-node-role-${n.node}`">
          {{ $t(roleKey[role(n)]) }}
        </span>
        <span
          class="ver"
          :class="{ outdated: isOutdated(n) }"
          :title="isOutdated(n) ? $t('ai.nodeOutdated') : ''"
          :data-testid="`ai-node-ver-${n.node}`"
        >
          v{{ n.version || '?' }}<template v-if="isOutdated(n)"> ⚠</template>
        </span>
        <span class="vram" :class="{ 'no-gpu': !n.gpu }">{{ fmtVram(n) }}</span>
        <span class="models">
          <span v-for="m in n.models.slice(0, 2)" :key="m" class="m-chip">{{ m }}</span>
          <span v-if="n.models.length > 2" class="m-more">+{{ n.models.length - 2 }}</span>
          <span v-if="n.shardReady" class="ready-dot" title="shard-ready">●</span>
          <span v-if="n.throttled" class="throttle" title="throttled">▲</span>
        </span>
      </div>
    </div>
  </section>
</template>

<style scoped>
.nodes-card {
  margin-top: 14px;
  border: 1px solid rgba(148, 163, 184, 0.14);
  border-radius: 13px;
  background: rgba(10, 16, 24, 0.55);
  padding: 18px 20px;
}
.card-head {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
}
h2 {
  font-size: 13px;
  letter-spacing: 2px;
  color: #94a3b8;
}
.count {
  font-family: ui-monospace, monospace;
  font-size: 11px;
  color: #22d3ee;
  border: 1px solid rgba(34, 211, 238, 0.3);
  border-radius: 999px;
  padding: 1px 8px;
}
.empty {
  padding: 16px 0;
  text-align: center;
  font-size: 12px;
  color: #64748b;
}
.node-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
  max-height: 320px;
  overflow-y: auto;
}
.node-row {
  display: grid;
  grid-template-columns: 130px 62px 86px 100px 1fr;
  align-items: center;
  gap: 10px;
  border: 1px solid rgba(148, 163, 184, 0.1);
  border-radius: 9px;
  padding: 7px 12px;
  font-size: 11.5px;
}
.addr {
  font-family: ui-monospace, monospace;
  color: #e2e8f0;
}
.kind {
  font-size: 9.5px;
  font-weight: 700;
  letter-spacing: 0.8px;
  text-transform: uppercase;
  text-align: center;
  border-radius: 4px;
  padding: 2px 6px;
}
.k-worker {
  color: #34d399;
  border: 1px solid rgba(52, 211, 153, 0.35);
}
.k-gateway {
  color: #22d3ee;
  border: 1px solid rgba(34, 211, 238, 0.4);
}
.k-shard {
  color: #a78bfa;
  border: 1px solid rgba(167, 139, 250, 0.4);
}
.k-client {
  color: #94a3b8;
  border: 1px solid rgba(148, 163, 184, 0.25);
}
.ver {
  font-family: ui-monospace, monospace;
  color: #22d3ee;
}
.ver.outdated {
  color: #fbbf24;
}
.vram {
  font-family: ui-monospace, monospace;
  color: #e2e8f0;
}
.vram.no-gpu {
  color: #475569;
}
.models {
  display: flex;
  align-items: center;
  gap: 5px;
  flex-wrap: wrap;
}
.m-chip {
  font-family: ui-monospace, monospace;
  font-size: 10.5px;
  color: #e2e8f0;
  border: 1px solid rgba(34, 211, 238, 0.25);
  border-radius: 999px;
  padding: 1px 8px;
}
.m-more {
  font-size: 10.5px;
  color: #64748b;
}
.ready-dot {
  color: #34d399;
  font-size: 9px;
}
.throttle {
  font-size: 10px;
  color: #f87171;
}
</style>
