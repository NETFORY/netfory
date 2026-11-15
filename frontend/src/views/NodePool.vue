<script setup lang="ts">
import { ref, onMounted, computed } from 'vue'
import { useAuthStore } from '@/store/auth'
import {
  refreshNodePool,
  autoSelectNode,
  selectNode,
  type NodePing,
} from '@/lib/bridge'

const auth = useAuthStore()
const nodes = ref<NodePing[]>([])
const loading = ref(false)
const busy = ref('')

const activeHost = computed(() => nodes.value.find((n) => n.active)?.host || '')
const onlineCount = computed(() => nodes.value.filter((n) => n.online).length)

async function rescan() {
  if (!auth.isTauri) return
  loading.value = true
  try {
    nodes.value = (await refreshNodePool()).filter((n) => !n.p2p)
  } finally {
    loading.value = false
  }
}

async function auto() {
  if (!auth.isTauri) return
  loading.value = true
  try {
    nodes.value = (await autoSelectNode()).filter((n) => !n.p2p)
  } finally {
    loading.value = false
  }
}

async function pick(n: NodePing) {
  if (!auth.isTauri || !n.online || n.active) return
  busy.value = n.host
  try {
    await selectNode(n.host)
    nodes.value = nodes.value.map((x) => ({ ...x, active: x.host === n.host }))
  } finally {
    busy.value = ''
  }
}

function pingClass(ms: number | null) {
  if (ms == null) return 'off'
  if (ms < 900) return 'fast'
  if (ms < 1800) return 'mid'
  return 'slow'
}

onMounted(rescan)
</script>

<template>
  <div class="nodes-page" data-testid="node-pool-page">
    <header class="head">
      <div>
        <h1>УЗЛЫ SMARTHOLDEM</h1>
        <p class="sub">
          Пул mainnet-узлов. Клиент подключён к самому быстрому; при перегрузке —
          автопереключение на резервный.
        </p>
      </div>
      <div class="actions">
        <button class="btn ghost" :disabled="loading || !auth.isTauri" @click="auto" data-testid="node-auto-btn">
          ⚡ Авто (лучший)
        </button>
        <button class="btn" :disabled="loading || !auth.isTauri" @click="rescan" data-testid="node-rescan-btn">
          {{ loading ? '⟳ Пинг…' : '↻ Rescan' }}
        </button>
      </div>
    </header>

    <div v-if="!auth.isTauri" class="notice">
      Пул узлов доступен только в десктоп-приложении SmartNet.
    </div>

    <template v-else>
      <div class="summary" data-testid="node-summary">
        <span>Активный: <b class="active-host">{{ activeHost || '—' }}</b></span>
        <span>Онлайн: <b>{{ onlineCount }}/{{ nodes.length }}</b></span>
      </div>

      <ul class="list">
        <li
          v-for="n in nodes"
          :key="n.host"
          class="node"
          :class="{ active: n.active, offline: !n.online }"
          @click="pick(n)"
          :data-testid="`node-${n.host}`"
        >
          <span class="dot" :class="n.online ? 'on' : 'off'"></span>
          <span class="host">{{ n.host }}</span>
          <span v-if="n.height" class="height">#{{ n.height }}</span>
          <span class="ping" :class="pingClass(n.latencyMs)">
            {{ n.latencyMs != null ? `${n.latencyMs}ms` : 'offline' }}
          </span>
          <span v-if="n.active" class="badge">● подключён</span>
          <span v-else-if="busy === n.host" class="badge dim">…</span>
        </li>
      </ul>
    </template>
  </div>
</template>

<style scoped>
.nodes-page {
  max-width: 820px;
  margin: 0 auto;
  padding: 28px 24px;
}
.head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
  margin-bottom: 22px;
}
h1 {
  font-size: 22px;
  letter-spacing: 3px;
  color: #22d3ee;
}
.sub {
  margin-top: 8px;
  font-size: 13px;
  color: #64748b;
  max-width: 460px;
  line-height: 1.5;
}
.actions {
  display: flex;
  gap: 8px;
  flex-shrink: 0;
}
.btn {
  background: #22d3ee;
  color: #03121a;
  border: none;
  border-radius: 9px;
  padding: 9px 16px;
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 1px;
  cursor: pointer;
  transition: filter 0.2s;
}
.btn:hover {
  filter: brightness(1.1);
}
.btn.ghost {
  background: transparent;
  color: #22d3ee;
  border: 1px solid rgba(34, 211, 238, 0.4);
}
.btn:disabled {
  opacity: 0.5;
  cursor: default;
}
.notice {
  text-align: center;
  padding: 50px 0;
  color: #64748b;
}
.summary {
  display: flex;
  gap: 24px;
  margin-bottom: 14px;
  font-size: 13px;
  color: #94a3b8;
}
.active-host {
  color: #34d399;
  font-family: ui-monospace, monospace;
}
.list {
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.node {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px 16px;
  border: 1px solid rgba(148, 163, 184, 0.14);
  border-radius: 11px;
  background: rgba(15, 23, 32, 0.5);
  cursor: pointer;
  transition: border-color 0.2s, background-color 0.2s;
}
.node:hover {
  border-color: rgba(34, 211, 238, 0.4);
}
.node.active {
  border-color: #34d399;
  background: rgba(52, 211, 153, 0.08);
}
.node.offline {
  opacity: 0.55;
  cursor: default;
}
.dot {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  flex-shrink: 0;
}
.dot.on {
  background: #34d399;
  box-shadow: 0 0 8px rgba(52, 211, 153, 0.7);
}
.dot.off {
  background: #64748b;
}
.host {
  font-family: ui-monospace, monospace;
  font-size: 14px;
  color: #e2e8f0;
}
.height {
  font-size: 11px;
  color: #475569;
  font-family: ui-monospace, monospace;
}
.ping {
  margin-left: auto;
  font-size: 13px;
  font-family: ui-monospace, monospace;
}
.ping.fast {
  color: #34d399;
}
.ping.mid {
  color: #fbbf24;
}
.ping.slow {
  color: #fb923c;
}
.ping.off {
  color: #64748b;
}
.badge {
  font-size: 11px;
  color: #34d399;
  letter-spacing: 0.5px;
}
.badge.dim {
  color: #64748b;
}
</style>
