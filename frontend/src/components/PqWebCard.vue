<script setup lang="ts">
/* eslint-disable no-undef */
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  EMPTY_WEBPROXY_STATUS,
  appListen,
  webproxyRecheck,
  webproxySetEnabled,
  webproxySetMode,
  webproxySetNodes,
  webproxySetTotal,
  webproxySetV6,
  webproxyStatus,
  type WebproxyStatus,
} from '@/lib/bridge'

// PQ Web (v1.135.192): группы доменов с режимом, узлы pq-web (с трекера — подписанный
// список, кэш при недоступных трекерах, ✓ verified из allowlist, + ручные) с RTT, тумблер роутера.
// v1.135.202: prop advanced (ручные узлы — только DEV/GEEK), событие status → статус-строка Settings.
const props = withDefaults(defineProps<{ advanced?: boolean }>(), { advanced: true })
const emit = defineEmits<{ (e: 'status', s: WebproxyStatus): void }>()
const { t } = useI18n()
const st = ref<WebproxyStatus>(EMPTY_WEBPROXY_STATUS)
const nodesText = ref('')
const busy = ref(false)
let unlisten: (() => void) | null = null
let unlistenTotal: (() => void) | null = null

const trackerNodes = computed(() => st.value.nodes.filter((n) => n.source === 'tracker'))
const manualHealth = computed(() => st.value.nodes.filter((n) => n.source !== 'tracker'))
const autoAge = computed(() => {
  const at = st.value.auto?.at || 0
  if (!at) return ''
  const m = Math.max(0, Math.round((Date.now() / 1000 - at) / 60))
  return m < 60 ? t('web.pq.minAgo', { n: m }) : t('web.pq.hAgo', { n: Math.round(m / 60) })
})

async function load() {
  st.value = await webproxyStatus()
  emit('status', st.value)
  // IP-адреса узлов в UI не показываем: в поле — только id [регион]; ip:port/relay,
  // введённые ранее, бэкенд сохраняет сам (webproxy_set_nodes мержит по id)
  if (!nodesText.value) nodesText.value = (st.value.manualNodes || []).map((n) => [n.id, n.region].filter(Boolean).join(' ')).join('\n')
}
async function setMode(name: string, mode: string) {
  await webproxySetMode(name, mode).catch(() => {})
  await load()
}
async function saveNodes() {
  busy.value = true
  const nodes = nodesText.value
    .split('\n')
    .map((l) => l.trim())
    .filter(Boolean)
    .map((l) => {
      // id [region] [ip:port …] [https://relay] — как печатает `pq-web serve` в строке "dial:"
      const [id, ...rest] = l.split(/\s+/)
      const addrs = rest.filter((t) => /^\[?[0-9a-f.:]+\]?:\d+$/i.test(t))
      const relay = rest.find((t) => /^https?:\/\//i.test(t)) || ''
      const region = rest.find((t) => !addrs.includes(t) && t !== relay && t !== '-') || ''
      return { id, region, addrs, relay }
    })
  await webproxySetNodes(nodes).catch(() => {})
  await load()
  busy.value = false
}
async function recheck() {
  busy.value = true
  await webproxyRecheck().catch(() => {})
  await load()
  busy.value = false
}
async function toggle() {
  await webproxySetEnabled(!st.value.enabled).catch(() => {})
  await load()
}
async function toggleTotal() {
  await webproxySetTotal(!st.value.total).catch(() => {})
  await load()
}
// v1.135.207: фильтр v6-узлов — off | prefer (по умолчанию) | only
const v6Mode = computed(() => st.value.v6Mode || 'prefer')
const v6Nodes = computed(() => st.value.nodes.filter((n) => n.ipv6 && n.ok).length)
async function setV6(mode: 'off' | 'prefer' | 'only') {
  await webproxySetV6(mode).catch(() => {})
  await load()
}
const openNodes = computed(() => st.value.nodes.filter((n) => n.open).length)
function shortId(id: string) {
  return id.length > 14 ? id.slice(0, 8) + '…' + id.slice(-4) : id
}
onMounted(async () => {
  await load()
  unlisten = await appListen('webproxy-relays', () => void load())
  unlistenTotal = await appListen('webproxy-total', () => void load())
})
onUnmounted(() => {
  unlisten?.()
  unlistenTotal?.()
})
</script>

<template>
  <section class="panel block pq" data-testid="settings-pqweb">
    <div class="pq-head">
      <span class="block-title">{{ t('web.pq.title') }}</span>
      <label class="pq-toggle" data-testid="pqweb-enabled">
        <input type="checkbox" :checked="st.enabled" @change="toggle" />
        <span>{{ t('web.pq.enabled') }}</span>
      </label>
    </div>
    <p class="hint dim">{{ t('web.pq.hint') }}</p>
    <p v-if="st.port" class="pq-port dim" data-testid="pqweb-port">{{ t('web.pq.port', { port: st.port }) }}</p>

    <div class="pq-grid">
    <div class="pq-col">
    <label class="pq-total" :class="{ on: st.total }" data-testid="pqweb-total">
      <input type="checkbox" :checked="st.total" @change="toggleTotal" />
      <span class="pq-total-title">{{ t('web.pq.total') }}</span>
      <span class="dim pq-total-hint">{{ t('web.pq.totalHint', { n: openNodes }) }}</span>
    </label>

    <div class="pq-v6box" data-testid="pqweb-v6">
      <label class="pq-v6-row" data-testid="pqweb-v6-prefer-row">
        <input type="checkbox" :checked="v6Mode !== 'off'" @change="setV6(v6Mode === 'off' ? 'prefer' : 'off')" data-testid="pqweb-v6-prefer" />
        <span class="pq-v6-title">{{ t('web.pq.v6Prefer') }}</span>
      </label>
      <label class="pq-v6-row" :class="{ dimmed: v6Mode === 'off' }" data-testid="pqweb-v6-only-row">
        <input type="checkbox" :disabled="v6Mode === 'off'" :checked="v6Mode === 'only'" @change="setV6(v6Mode === 'only' ? 'prefer' : 'only')" data-testid="pqweb-v6-only" />
        <span>{{ t('web.pq.v6Only') }}</span>
      </label>
      <span class="dim pq-v6-hint" data-testid="pqweb-v6-hint">
        {{ v6Mode === 'only' ? t('web.pq.v6OnlyHint', { n: v6Nodes }) : v6Mode === 'off' ? t('web.pq.v6OffHint') : t('web.pq.v6PreferHint', { n: v6Nodes }) }}
      </span>
    </div>

    <h4 class="pq-sub">{{ t('web.pq.bundles') }}</h4>
    <div v-for="b in st.bundles" :key="b.name" class="pq-row" :data-testid="`pqweb-bundle-${b.name}`">
      <div class="pq-name">
        <b>{{ b.name }}</b>
        <span class="dim pq-hosts" :title="b.hosts.join(', ')">{{ b.hosts.slice(0, 3).join(', ') }}<span v-if="b.hosts.length > 3">…</span></span>
      </div>
      <span class="pq-route" :class="b.route || 'none'" :title="b.reason">
        {{ b.route === 'relay' ? t('web.pq.routeRelay') : b.route === 'direct' ? t('web.pq.routeDirect') : t('web.pq.routeUnknown') }}
      </span>
      <select class="pq-select" :value="b.mode" @change="setMode(b.name, ($event.target as HTMLSelectElement).value)" :data-testid="`pqweb-mode-${b.name}`">
        <option value="auto">{{ t('web.pq.modeAuto') }}</option>
        <option value="relay">{{ t('web.pq.modeRelay') }}</option>
        <option value="direct">{{ t('web.pq.modeDirect') }}</option>
      </select>
    </div>
    </div>

    <div class="pq-col">
    <h4 class="pq-sub tight">{{ t('web.pq.fromTracker') }}</h4>
    <div class="pq-tracker" data-testid="pqweb-tracker">
      <template v-if="st.auto?.nodes?.length">
        <span class="pq-badge" :class="st.auto.signed ? 'signed' : 'unsigned'" data-testid="pqweb-signed-badge">
          {{ st.auto.signed ? '✓ ' + t('web.pq.signed') : t('web.pq.unsigned') }}
        </span>
        <span class="dim">{{ t('web.pq.nodesCount', { n: st.auto.nodes.length }) }} · {{ autoAge }}</span>
        <span v-if="st.auto.stale" class="pq-badge stale" :title="t('web.pq.cachedHint')" data-testid="pqweb-cached-badge">{{ t('web.pq.cached') }}</span>
      </template>
      <span v-else class="dim" data-testid="pqweb-no-tracker-nodes">{{ t('web.pq.noTrackerNodes') }}</span>
    </div>
    <p v-if="st.auto?.error" class="hint warn pq-auto-err" data-testid="pqweb-tracker-error">{{ st.auto.error }}</p>
    <ul v-if="trackerNodes.length" class="pq-health">
      <li v-for="n in trackerNodes" :key="n.id" :class="{ ok: n.ok }" :data-testid="`pqweb-node-${n.id}`">
        <i class="dot" />{{ shortId(n.id) }} <span class="dim">{{ n.region }}</span>
        <span v-if="n.verified" class="pq-verified" :title="t('web.pq.verifiedHint')" :data-testid="`pqweb-node-verified-${n.id}`">✓ {{ t('web.pq.verified') }}</span>
        <span v-if="n.open" class="pq-open" :title="t('web.pq.openHint')">{{ t('web.pq.open') }}</span>
        <span v-if="n.ipv6" class="pq-open v6" :title="t('web.pq.v6Hint')" :data-testid="`pqweb-node-v6-${n.id}`">{{ t('web.pq.v6') }}</span>
        <span class="rtt">{{ n.ok ? n.rttMs + ' ms' : n.error || '—' }}</span>
      </li>
    </ul>

    <template v-if="props.advanced">
    <h4 class="pq-sub">{{ t('web.pq.nodes') }}</h4>
    <p class="hint dim">{{ t('web.pq.nodesHint') }}</p>
    <textarea v-model="nodesText" class="pq-nodes" rows="3" spellcheck="false" placeholder="EndpointId NL" data-testid="pqweb-nodes-input" />
    <div class="pq-actions">
      <button class="pq-btn" :disabled="busy" @click="saveNodes" data-testid="pqweb-nodes-save">{{ t('web.pq.save') }}</button>
      <button class="pq-btn ghost" :disabled="busy" @click="recheck" data-testid="pqweb-recheck">{{ t('web.pq.recheck') }}</button>
    </div>
    </template>
    <div v-else class="pq-actions">
      <button class="pq-btn ghost" :disabled="busy" @click="recheck" data-testid="pqweb-recheck">{{ t('web.pq.recheck') }}</button>
    </div>
    <p v-if="!st.nodes.length" class="hint warn" data-testid="pqweb-no-nodes">{{ t('web.pq.noNodes') }}</p>
    <ul v-else-if="manualHealth.length" class="pq-health">
      <li v-for="n in manualHealth" :key="n.id" :class="{ ok: n.ok }" :data-testid="`pqweb-node-${n.id}`">
        <i class="dot" />{{ shortId(n.id) }} <span class="dim">{{ n.region }}</span>
        <span v-if="n.open" class="pq-open" :title="t('web.pq.openHint')">{{ t('web.pq.open') }}</span>
        <span v-if="n.ipv6" class="pq-open v6" :title="t('web.pq.v6Hint')" :data-testid="`pqweb-node-v6-${n.id}`">{{ t('web.pq.v6') }}</span>
        <span class="rtt">{{ n.ok ? n.rttMs + ' ms' : n.error || '—' }}</span>
      </li>
    </ul>
    </div>
    </div>
    <!-- /pq-grid -->
  </section>
</template>

<style scoped>
.pq-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.pq-grid { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 0 28px; }
.pq-col { min-width: 0; }
.pq-sub.tight { margin-top: 14px; }
@media (max-width: 1100px) { .pq-grid { grid-template-columns: 1fr; } .pq-sub.tight { margin-top: 16px; } }
.pq-toggle { display: inline-flex; align-items: center; gap: 8px; font-size: 11px; color: var(--text-dim); cursor: pointer; }
.pq-port { font-size: 11px; font-family: var(--mono, monospace); }
.pq-total { display: grid; grid-template-columns: auto 1fr; gap: 4px 10px; align-items: center; margin-top: 14px; padding: 12px 14px; border: 1px solid var(--border); cursor: pointer; transition: border-color 0.16s ease, box-shadow 0.16s ease; }
.pq-total.on { border-color: #2ee6a6; box-shadow: 0 0 14px rgba(46, 230, 166, 0.2); }
.pq-total-title { font-size: 12px; font-weight: var(--fw-800); letter-spacing: 0.06em; }
.pq-total.on .pq-total-title { color: #2ee6a6; }
.pq-total-hint { grid-column: 2; font-size: 11px; line-height: 1.4; }
.pq-v6box { display: flex; flex-direction: column; gap: 6px; margin-top: 10px; padding: 10px 14px; border: 1px solid var(--border); }
.pq-v6-row { display: flex; align-items: center; gap: 8px; font-size: 11px; cursor: pointer; }
.pq-v6-row.dimmed { opacity: 0.45; cursor: default; }
.pq-v6-title { font-size: 12px; font-weight: var(--fw-800); letter-spacing: 0.04em; }
.pq-v6-hint { font-size: 11px; line-height: 1.4; }
.pq-open { font-size: 9px; letter-spacing: 0.1em; text-transform: uppercase; color: #2ee6a6; border: 1px solid #2ee6a6; padding: 0 5px; }
.pq-open.v6 { color: #7dd3fc; border-color: #7dd3fc; text-transform: none; }
.pq-sub { margin: 16px 0 8px; font-size: 10px; letter-spacing: 0.16em; color: var(--accent); font-weight: var(--fw-800); }
.pq-row { display: grid; grid-template-columns: 1fr auto auto; gap: 10px; align-items: center; padding: 8px 0; border-top: 1px solid var(--border); }
.pq-name { display: flex; flex-direction: column; gap: 2px; font-size: 12px; min-width: 0; }
.pq-hosts { font-size: 10px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.pq-route { font-size: 10px; letter-spacing: 0.1em; text-transform: uppercase; padding: 2px 8px; border: 1px solid var(--border); color: var(--text-dim); }
.pq-route.relay { color: #c084fc; border-color: #c084fc; }
.pq-route.direct { color: var(--accent); border-color: var(--accent); }
.pq-select { background: rgba(0, 0, 0, 0.3); color: var(--text); border: 1px solid var(--border); font: inherit; font-size: 11px; padding: 5px 8px; }
.pq-tracker { display: flex; align-items: center; gap: 8px; font-size: 11px; flex-wrap: wrap; }
.pq-badge { font-size: 10px; letter-spacing: 0.12em; text-transform: uppercase; padding: 2px 8px; border: 1px solid; }
.pq-badge.signed { color: var(--accent); border-color: var(--accent); box-shadow: 0 0 10px rgba(23, 233, 199, 0.25); }
.pq-badge.unsigned { color: #f59e0b; border-color: #f59e0b; }
.pq-auto-err { font-family: var(--mono, monospace); font-size: 10px; word-break: break-all; }
.pq-badge.stale { color: var(--text-dim); border-color: var(--border); border-style: dashed; }
.pq-verified { font-size: 9px; letter-spacing: 0.1em; text-transform: uppercase; color: var(--accent); }
.pq-nodes { width: 100%; background: rgba(0, 0, 0, 0.3); color: var(--text); border: 1px solid var(--border); font: inherit; font-size: 11px; padding: 8px; resize: vertical; }
.pq-actions { display: flex; gap: 8px; margin-top: 8px; }
.pq-btn { padding: 7px 14px; background: var(--accent); color: #04110f; border: 1px solid var(--accent); font-size: 11px; font-weight: var(--fw-800); cursor: pointer; }
.pq-btn.ghost { background: transparent; color: var(--accent); }
.pq-btn:disabled { opacity: 0.5; cursor: default; }
.hint.warn { color: #f59e0b; }
.pq-health { list-style: none; margin: 10px 0 0; padding: 0; font-size: 11px; font-family: var(--mono, monospace); }
.pq-health li { display: flex; align-items: center; gap: 8px; padding: 4px 0; }
.pq-health .dot { width: 7px; height: 7px; border-radius: 50%; background: #ff5f56; display: inline-block; }
.pq-health li.ok .dot { background: var(--accent); box-shadow: 0 0 8px var(--accent); }
.pq-health .rtt { margin-left: auto; color: var(--text-dim); }
</style>
