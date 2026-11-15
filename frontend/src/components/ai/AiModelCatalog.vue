<script setup lang="ts">
/* eslint-disable no-undef */
// Каталог опубликованных моделей сети: alias'ы, зарегистрированные DEV-нодами
// через `publish-model --alias`. Кнопки: скачать модель (Save As + прогресс)
// или скопировать команду скачивания для CLI.
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { aiModelDownload, appListen, type AiModelTicket, type AiNetStats } from '@/lib/bridge'

defineProps<{ net: AiNetStats | null }>()

const copied = ref('')
let timer: ReturnType<typeof setTimeout> | null = null

// --- скачивание с прогрессом (событие "ai-model-dl" из Tauri) ---
interface DlState {
  status: 'run' | 'done' | 'error'
  bytes: number
  err?: string
}
const dl = ref<Record<string, DlState>>({})
let unlisten: (() => void) | null = null
onMounted(async () => {
  unlisten = await appListen<{ alias: string; bytes?: number }>('ai-model-dl', (m) => {
    if (!m?.alias) return
    const cur = dl.value[m.alias]
    if (cur?.status === 'run' && typeof m.bytes === 'number') cur.bytes = m.bytes
  })
})
onBeforeUnmount(() => unlisten?.())

async function download(m: AiModelTicket) {
  if (dl.value[m.alias]?.status === 'run') return
  dl.value[m.alias] = { status: 'run', bytes: 0 }
  try {
    const path = await aiModelDownload(m.alias, m.ticket)
    if (path) dl.value[m.alias] = { status: 'done', bytes: 0 }
    else delete dl.value[m.alias] // диалог отменён
  } catch (e: any) {
    dl.value[m.alias] = { status: 'error', bytes: 0, err: String(e?.message ?? e) }
  }
}

function fmtBytes(b: number): string {
  return b >= 1e9 ? (b / 1e9).toFixed(2) + ' GB' : (b / 1e6).toFixed(0) + ' MB'
}

async function copyCmd(alias: string) {
  const cmd = `netfory-ai-worker fetch-model ${alias}`
  try {
    await navigator.clipboard.writeText(cmd)
    copied.value = alias
    if (timer) clearTimeout(timer)
    timer = setTimeout(() => (copied.value = ''), 2000)
  } catch {
    /* clipboard недоступен — молча */
  }
}

function shortTicket(t: string): string {
  const [hash, node] = t.split('@')
  if (!hash || !node) return t.slice(0, 20) + '…'
  return `${hash.slice(0, 10)}…@${node.slice(0, 8)}…`
}

function fmtAt(ts: number): string {
  return new Date(ts * 1000).toLocaleString([], {
    day: '2-digit',
    month: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

/** источник жив, если переанонсировал тикет за последние 3 мин (анонс каждые 60с) */
function seederAlive(m: AiModelTicket, net: AiNetStats): boolean {
  return net.updatedAt - m.registeredAt < 180
}

/** живые метрики маркетплейса по alias (tok/s, цена, слоты) из pipelines */
function pinfo(alias: string, net: AiNetStats) {
  return net.pipelines?.find((p) => p.model === alias)
}
</script>

<template>
  <section class="catalog-card" data-testid="ai-model-catalog">
    <div class="card-head">
      <h2>{{ $t('ai.catalogTitle') }}</h2>
      <span v-if="net?.modelTickets?.length" class="count">{{ net!.modelTickets!.length }}</span>
    </div>

    <div v-if="!net?.modelTickets?.length" class="empty" data-testid="ai-catalog-empty">
      {{ $t('ai.catalogEmpty') }}
    </div>

    <div v-else class="rows">
      <div
        v-for="(m, i) in net!.modelTickets!"
        :key="m.alias"
        class="row"
        :data-testid="`ai-catalog-row-${i}`"
      >
        <span class="alias">{{ m.alias }}</span>
        <span
          class="health"
          :class="seederAlive(m, net!) ? 'on' : 'off'"
          :title="$t(seederAlive(m, net!) ? 'ai.catalogLiveTitle' : 'ai.catalogDeadTitle')"
          :data-testid="`ai-catalog-health-${i}`"
        >
          ● {{ $t(seederAlive(m, net!) ? 'ai.catalogLive' : 'ai.catalogDead') }}
        </span>
        <span
          v-if="net!.models?.[m.alias]"
          class="nodes-badge"
          :title="$t('ai.catalogNodesTitle')"
          :data-testid="`ai-catalog-nodes-${i}`"
        >
          {{ net!.models![m.alias] }} {{ $t('ai.catalogNodes') }}
        </span>
        <span
          v-if="pinfo(m.alias, net!)?.tokPerSec"
          class="tok-badge"
          :title="$t('ai.catalogTokTitle')"
          :data-testid="`ai-catalog-tok-${i}`"
        >
          ⚡ {{ pinfo(m.alias, net!)!.tokPerSec }} tok/s
        </span>
        <span
          v-if="pinfo(m.alias, net!)?.fullNodes || pinfo(m.alias, net!)?.gateways"
          class="price-badge"
          :class="{ free: !pinfo(m.alias, net!)?.pricePerMtok }"
          :title="$t('ai.catalogPriceTitle')"
          :data-testid="`ai-catalog-price-${i}`"
        >
          {{ pinfo(m.alias, net!)!.pricePerMtok ? `${pinfo(m.alias, net!)!.pricePerMtok} STH/1M` : $t('ai.catalogFree') }}
        </span>
        <span class="ticket" :title="m.ticket">{{ shortTicket(m.ticket) }}</span>
        <span class="at">{{ fmtAt(m.registeredAt) }}</span>
        <button
          class="dl-btn"
          :class="{ run: dl[m.alias]?.status === 'run', ok: dl[m.alias]?.status === 'done', err: dl[m.alias]?.status === 'error' }"
          :data-testid="`ai-catalog-download-${i}`"
          :title="dl[m.alias]?.err || $t('ai.catalogDownloadTitle')"
          :disabled="dl[m.alias]?.status === 'run'"
          @click="download(m)"
        >
          <template v-if="dl[m.alias]?.status === 'run'">
            {{ dl[m.alias].bytes ? fmtBytes(dl[m.alias].bytes) + '…' : $t('ai.catalogStarting') }}
          </template>
          <template v-else-if="dl[m.alias]?.status === 'done'">{{ $t('ai.catalogDlDone') }}</template>
          <template v-else-if="dl[m.alias]?.status === 'error'">{{ $t('ai.catalogDlRetry') }}</template>
          <template v-else>{{ $t('ai.catalogDownload') }}</template>
        </button>
        <button
          class="copy-btn"
          :class="{ ok: copied === m.alias }"
          :data-testid="`ai-catalog-copy-${i}`"
          :title="`netfory-ai-worker fetch-model ${m.alias}`"
          @click="copyCmd(m.alias)"
        >
          {{ copied === m.alias ? $t('ai.catalogCopied') : $t('ai.catalogCopy') }}
        </button>
      </div>
    </div>
  </section>
</template>

<style scoped>
.catalog-card {
  margin-top: 14px;
  border: 1px solid rgba(148, 163, 184, 0.14);
  border-radius: 13px;
  background: rgba(10, 16, 24, 0.55);
  padding: 18px 20px;
}
.card-head {
  display: flex;
  align-items: center;
  gap: 10px;
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
  font-size: 11.5px;
  color: #64748b;
  line-height: 1.6;
}
.rows {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.row {
  display: flex;
  align-items: center;
  gap: 12px;
  border: 1px solid rgba(148, 163, 184, 0.1);
  border-radius: 9px;
  padding: 7px 12px;
  transition: border-color 0.15s;
}
.row:hover {
  border-color: rgba(34, 211, 238, 0.25);
}
.alias {
  font-family: ui-monospace, monospace;
  font-size: 12.5px;
  font-weight: 700;
  color: #22d3ee;
  min-width: 120px;
}
.health {
  flex-shrink: 0;
  font-size: 10px;
  font-family: ui-monospace, monospace;
  letter-spacing: 0.5px;
}
.health.on {
  color: #34d399;
}
.health.off {
  color: #f87171;
}
.nodes-badge {
  flex-shrink: 0;
  font-family: ui-monospace, monospace;
  font-size: 10px;
  color: #a5b4fc;
  border: 1px solid rgba(165, 180, 252, 0.3);
  border-radius: 999px;
  padding: 1px 8px;
}
.tok-badge {
  flex-shrink: 0;
  font-family: ui-monospace, monospace;
  font-size: 10px;
  color: #fbbf24;
  border: 1px solid rgba(251, 191, 36, 0.3);
  border-radius: 999px;
  padding: 1px 8px;
}
.price-badge {
  flex-shrink: 0;
  font-family: ui-monospace, monospace;
  font-size: 10px;
  color: #f0abfc;
  border: 1px solid rgba(240, 171, 252, 0.3);
  border-radius: 999px;
  padding: 1px 8px;
}
.price-badge.free {
  color: #34d399;
  border-color: rgba(52, 211, 153, 0.3);
}
.ticket {
  flex: 1;
  font-family: ui-monospace, monospace;
  font-size: 10.5px;
  color: #64748b;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.at {
  font-family: ui-monospace, monospace;
  font-size: 10px;
  color: #475569;
  flex-shrink: 0;
}
.copy-btn {
  flex-shrink: 0;
  background: none;
  border: 1px solid rgba(34, 211, 238, 0.35);
  border-radius: 999px;
  color: #22d3ee;
  font-size: 10.5px;
  padding: 4px 12px;
  cursor: pointer;
  transition: background-color 0.15s, color 0.15s, border-color 0.15s;
}
.copy-btn:hover {
  background: rgba(34, 211, 238, 0.1);
}
.copy-btn.ok {
  color: #34d399;
  border-color: rgba(52, 211, 153, 0.45);
}
.dl-btn {
  flex-shrink: 0;
  background: #22d3ee;
  border: 1px solid #22d3ee;
  border-radius: 999px;
  color: #0a1018;
  font-size: 10.5px;
  font-weight: 700;
  padding: 4px 12px;
  cursor: pointer;
  transition: opacity 0.15s, background-color 0.15s, color 0.15s;
}
.dl-btn:hover:not(:disabled) {
  opacity: 0.85;
}
.dl-btn.run {
  background: none;
  color: #22d3ee;
  cursor: default;
  font-family: ui-monospace, monospace;
}
.dl-btn.ok {
  background: none;
  color: #34d399;
  border-color: rgba(52, 211, 153, 0.45);
}
.dl-btn.err {
  background: none;
  color: #f87171;
  border-color: rgba(248, 113, 113, 0.45);
}
</style>
