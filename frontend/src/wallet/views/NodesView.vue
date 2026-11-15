<script setup lang="ts">
/*
 * SmartNet Wallet // NodesView
 *
 *   Быстрый переключатель активного узла кошелька, аналог "Settings → Network"
 *   но встроен прямо в drawer. Показывает два табa (Clearnet / P2P), пинг
 *   каждого узла, выделяет активный. Данные общие с Settings.vue — при смене
 *   узла кошелёк начнёт использовать его немедленно (все walletInvoke идут
 *   через sth_node, который резолвит активный узел).
 */
import { computed, onMounted, ref } from 'vue'
import { builtinP2pNodes } from '@/lib/sthNodes'
import { useI18n } from 'vue-i18n'
import { useWalletStore, type SavedNetwork } from '@/store/wallet'
import { walletInvoke } from '@/wallet/ipc'
import {
  isTauri,
  refreshNodePool,
  autoSelectNode,
  selectNode,
  currentNode,
  listCustomNodes,
  pingCustomNode,
  type NodePing,
  type CustomPing,
} from '@/lib/bridge'

const { t } = useI18n()
const wallet = useWalletStore()

type Tab = 'clear' | 'p2p' | 'networks'
// SHIP-44: iroh-узлы sth-core — основной транспорт, поэтому вкладка P2P первая.
const tab = ref<Tab>('p2p')

/* ---- Networks: сохранённые сети SmartHoldem (store) + «Добавить сеть» по API-ссылке ---- */
const netAdding = ref(false)
const netUrl = ref('')
const netProbing = ref(false)
const netErr = ref('')
const netProbe = ref<Omit<SavedNetwork, 'id'> | null>(null)
function netChain(n: SavedNetwork) {
  const parts = [`network ${n.version}`, n.wif ? `wif ${n.wif}` : '', n.token || n.symbol]
  return parts.filter(Boolean).join(' · ')
}
async function probeNetwork() {
  const url = netUrl.value.trim()
  if (!url || netProbing.value) return
  netProbing.value = true
  netErr.value = ''
  netProbe.value = null
  try {
    netProbe.value = await walletInvoke<Omit<SavedNetwork, 'id'>>('probe_network_api', { url })
  } catch (e: any) {
    netErr.value = typeof e === 'string' ? e : e?.message || String(e)
  } finally {
    netProbing.value = false
  }
}
function saveNetwork() {
  if (!netProbe.value) return
  wallet.addNetwork(netProbe.value)
  netProbe.value = null
  netUrl.value = ''
  netAdding.value = false
}

const clearNodes = ref<NodePing[]>([])
/** Встроенный пул iroh-узлов sth-core (из Rust `DEFAULT_IROH_NODES`, с пингом). */
const irohPool = ref<NodePing[]>([])
const customNodes = ref<string[]>([])
const customPings = ref<Record<string, CustomPing>>({})
const activeHost = ref('')
const busy = ref(false)

/** Встроенный пул P2P-узлов (lib/sthNodes.ts) + env `SMARTHOLDEM_NODES_P2P`. */
const p2pEnvNodes = computed<string[]>(() => builtinP2pNodes())

/** Итоговый список P2P-узлов: iroh-пул sth-core + env-встроенные + custom api://. */
const p2pNodes = computed<string[]>(() => {
  const all = new Set<string>(irohPool.value.map((n) => n.host))
  p2pEnvNodes.value.forEach((h) => all.add(h))
  customNodes.value.filter((h) => h.startsWith('api://')).forEach((h) => all.add(h))
  return [...all]
})

/** Свои clearnet-узлы (напр. локальный SmartHoldem http://localhost:4003). */
const customClearNodes = computed<string[]>(() => customNodes.value.filter((h) => !h.startsWith('api://')))

/** Пинг P2P-узла: из iroh-пула (refreshNodePool) либо из ручного pingCustomNode. */
function p2pPing(h: string): { online?: boolean; latencyMs?: number | null; height?: number | null } | undefined {
  return irohPool.value.find((n) => n.host === h) ?? customPings.value[h]
}

/** `api://1692c5f3…e723b790/sth-core` — короткая форма EndpointId для строки. */
function shortApi(h: string): string {
  const rest = h.replace(/^api:\/\//, '')
  const [id, ...name] = rest.split('/')
  if (id.length <= 20) return rest
  return `${id.slice(0, 8)}…${id.slice(-8)}/${name.join('/')}`
}

function isLocalHost(h: string): boolean {
  const host = h.replace(/^https?:\/\//, '').split(/[/:]/)[0]
  return host === 'localhost' || /^127\./.test(host) || /^10\./.test(host) || /^192\.168\./.test(host) || /^172\.(1[6-9]|2\d|3[01])\./.test(host)
}

async function loadAll() {
  if (!isTauri) {
    // web-preview mock — 3 clear + 2 p2p узла, чтобы UI был работоспособен.
    clearNodes.value = [
      { host: 'node0.smartholdem.io', latencyMs: 160, online: true, height: 11136384, active: false, p2p: false },
      { host: 'node1.smartholdem.io', latencyMs: 93, online: true, height: 11136384, active: false, p2p: false },
      { host: 'node2.smartholdem.io', latencyMs: 231, online: true, height: 11136384, active: false, p2p: false },
    ]
    irohPool.value = [
      { host: 'api://1692c5f342542465819ff31c31432e933455ef3daa788d7297065869e723b790/sth-core', latencyMs: 74, online: true, height: 11136384, active: true, p2p: true },
      { host: 'api://d8c32e15974a4197f16d61b7cc03e4be4338cde2deca6310589584adbc465bf6/sth-core', latencyMs: 118, online: true, height: 11136384, active: false, p2p: true },
    ]
    customNodes.value = ['api://SmockP2pNode1example000000000000000', 'api://SmockP2pNode2example000000000000000']
    for (const h of customNodes.value) {
      customPings.value[h] = { host: h, online: true, latencyMs: 210, height: 11136384, verified: true, p2p: true }
    }
    activeHost.value = irohPool.value[0].host
    return
  }
  busy.value = true
  try {
    const [pool, custom, cur] = await Promise.all([
      refreshNodePool(),
      listCustomNodes(),
      currentNode(),
    ])
    clearNodes.value = pool.filter((n) => !n.p2p)
    irohPool.value = pool.filter((n) => !!n.p2p)
    customNodes.value = custom
    activeHost.value = cur
    // пингуем P2P-узлы параллельно (custom и env; iroh-пул уже пропингован в Rust)
    const pooled = new Set(irohPool.value.map((n) => n.host))
    const pingTargets = p2pNodes.value.filter((h) => !pooled.has(h)).concat(custom.filter((h) => !h.startsWith('api://')))
    await Promise.all(
      pingTargets.map(async (h) => {
        try {
          customPings.value[h] = await pingCustomNode(h)
        } catch {
          /* оставляем без пинга — покажется как unknown */
        }
      }),
    )
  } finally {
    busy.value = false
  }
}

async function pickAuto() {
  if (!isTauri) {
    activeHost.value = clearNodes.value.reduce(
      (best, n) => (n.online && (!best || (n.latencyMs ?? 1e9) < (best.latencyMs ?? 1e9)) ? n : best),
      null as NodePing | null,
    )?.host || activeHost.value
    wallet.flash(t('wallet.nodes.autoDone'))
    return
  }
  busy.value = true
  try {
    const pool = await autoSelectNode()
    clearNodes.value = pool.filter((n) => !n.p2p)
    irohPool.value = pool.filter((n) => !!n.p2p)
    activeHost.value = (await currentNode()) || activeHost.value
    wallet.flash(t('wallet.nodes.autoDone'))
  } finally {
    busy.value = false
  }
}

async function pick(host: string) {
  if (host === activeHost.value) return
  if (!isTauri) {
    activeHost.value = host
    wallet.flash(t('wallet.nodes.switched'))
    return
  }
  busy.value = true
  try {
    await selectNode(host)
    activeHost.value = host
    wallet.flash(t('wallet.nodes.switched'))
  } catch (e: any) {
    wallet.error = String(e?.message ?? e ?? 'select failed')
  } finally {
    busy.value = false
  }
}

function fmtLatency(ms: number | null): string {
  if (ms == null) return '—'
  return `${Math.round(ms)}ms`
}

onMounted(loadAll)
</script>

<template>
  <div class="nodes" data-testid="wallet-nodes">
    <div class="hdr">
      <p class="hdr-title">{{ t('wallet.nodes.title') }}</p>
      <div class="hdr-acts">
        <button
          class="hdr-btn primary"
          :disabled="busy"
          @click="pickAuto"
          data-testid="nodes-auto-btn"
        >
          ⚡ {{ t('wallet.nodes.auto') }}
        </button>
        <button
          class="hdr-btn"
          :disabled="busy"
          @click="loadAll"
          data-testid="nodes-rescan-btn"
        >
          ↻ {{ t('wallet.nodes.rescan') }}
        </button>
      </div>
    </div>

    <!-- Табы: Clear Nodes / P2P Nodes -->
    <div class="tabs" data-testid="nodes-tabs">
      <button
        class="tab"
        :class="{ on: tab === 'clear' }"
        @click="tab = 'clear'"
        data-testid="nodes-tab-clear"
      >
        {{ t('wallet.nodes.clearNodes') }} <span class="cnt">{{ clearNodes.length + customClearNodes.length }}</span>
      </button>
      <button
        class="tab"
        :class="{ on: tab === 'p2p' }"
        @click="tab = 'p2p'"
        data-testid="nodes-tab-p2p"
      >
        🔗 {{ t('wallet.nodes.p2pNodes') }} <span class="cnt">{{ p2pNodes.length }}</span>
      </button>
      <button
        class="tab"
        :class="{ on: tab === 'networks' }"
        @click="tab = 'networks'"
        data-testid="nodes-tab-networks"
      >
        {{ t('wallet.nodes.networks') }} <span class="cnt">{{ wallet.networks.length }}</span>
      </button>
    </div>

    <!-- Networks: mainnet + сохранённые сети SmartHoldem (по API-ссылке) -->
    <div v-if="tab === 'networks'" class="list" data-testid="nodes-list-networks">
      <div
        v-for="nw in wallet.networks"
        :key="nw.id"
        class="node-row net-row"
        :class="{ active: nw.id === wallet.activeNetworkId }"
        role="button"
        tabindex="0"
        :data-testid="`network-row-${nw.id}`"
        @click="wallet.switchNetwork(nw.id)"
        @keydown.enter="wallet.switchNetwork(nw.id)"
      >
        <span class="dot online" />
        <span class="net-main">
          <span class="host">{{ nw.name || nw.token }}</span>
          <span class="net-chain dim mono">{{ netChain(nw) }}</span>
          <span v-if="nw.api" class="net-api dim mono">{{ nw.api.replace(/^https?:\/\//, '') }}</span>
        </span>
        <span v-if="nw.id === wallet.activeNetworkId" class="active-tag">{{ t('wallet.nodes.connected') }}</span>
        <span v-else-if="!nw.builtin" class="soon-tag dim">{{ t('wallet.viewOnly.title') }}</span>
        <button
          v-if="!nw.builtin"
          class="net-del"
          :title="t('wallet.nodes.removeNetwork')"
          @click.stop="wallet.removeNetwork(nw.id)"
          :data-testid="`network-remove-${nw.id}`"
        >✕</button>
      </div>

      <!-- добавить сеть -->
      <button v-if="!netAdding" class="net-add" @click="netAdding = true" data-testid="network-add-btn">
        + {{ t('wallet.nodes.addNetwork') }}
      </button>
      <div v-else class="net-form" data-testid="network-add-form">
        <span class="lbl">{{ t('wallet.nodes.addNetworkUrl') }}</span>
        <div class="net-form-row">
          <input
            v-model="netUrl"
            class="net-inp mono"
            placeholder="https://api.testnet.smartholdem.io"
            spellcheck="false"
            @keydown.enter="probeNetwork"
            data-testid="network-url-input"
          />
          <button class="net-go" :disabled="netProbing || !netUrl.trim()" @click="probeNetwork" data-testid="network-probe-btn">
            {{ netProbing ? '…' : t('wallet.nodes.checkNet') }}
          </button>
        </div>
        <p v-if="netErr" class="net-err" data-testid="network-probe-error">{{ netErr }}</p>
        <div v-if="netProbe" class="net-preview" data-testid="network-probe-result">
          <div class="np-row"><span class="dim">{{ t('wallet.nodes.npName') }}</span><b>{{ netProbe.name || '—' }}</b></div>
          <div class="np-row"><span class="dim">{{ t('wallet.nodes.npToken') }}</span><b class="mono">{{ netProbe.token }} / {{ netProbe.symbol }}</b></div>
          <div class="np-row"><span class="dim">{{ t('wallet.nodes.npVersion') }}</span><b class="mono">{{ netProbe.version }} · wif {{ netProbe.wif }}</b></div>
          <div class="np-row"><span class="dim">nethash</span><b class="mono np-hash" :title="netProbe.nethash">{{ netProbe.nethash.slice(0, 10) }}…{{ netProbe.nethash.slice(-8) }}</b></div>
          <div v-if="netProbe.height" class="np-row"><span class="dim">{{ t('wallet.nodes.npHeight') }}</span><b class="mono">#{{ netProbe.height.toLocaleString() }}</b></div>
          <div v-if="netProbe.coreVersion" class="np-row"><span class="dim">core</span><b class="mono">{{ netProbe.coreVersion }}</b></div>
          <div class="net-form-actions">
            <button class="net-cancel" @click="netAdding = false; netProbe = null" data-testid="network-cancel-btn">{{ t('wallet.nodes.cancel') }}</button>
            <button class="net-save" @click="saveNetwork" data-testid="network-save-btn">{{ t('wallet.nodes.saveNetwork') }}</button>
          </div>
        </div>
        <button v-else class="net-cancel" @click="netAdding = false" data-testid="network-cancel-btn">{{ t('wallet.nodes.cancel') }}</button>
      </div>
      <p class="footnote dim">{{ t('wallet.nodes.networksHint') }}</p>
    </div>

    <!-- Clear Nodes list -->
    <div v-else-if="tab === 'clear'" class="list" data-testid="nodes-list-clear">
      <div v-if="!clearNodes.length" class="empty dim">
        {{ t('wallet.nodes.empty') }}
      </div>
      <button
        v-for="n in clearNodes"
        :key="n.host"
        class="node-row"
        :class="{ active: n.host === activeHost, offline: !n.online }"
        :disabled="!n.online || busy"
        @click="pick(n.host)"
        :data-testid="`node-row-${n.host}`"
      >
        <span class="dot" :class="{ online: n.online }" />
        <span class="host">{{ n.host }}</span>
        <span v-if="n.height" class="height dim mono">#{{ n.height }}</span>
        <span class="latency mono">{{ fmtLatency(n.latencyMs) }}</span>
        <span v-if="n.host === activeHost" class="active-tag">{{ t('wallet.nodes.connected') }}</span>
      </button>

      <!-- Свои clearnet-узлы (локальный SmartHoldem и т.п.) -->
      <template v-if="customClearNodes.length">
        <p class="sub-title dim" data-testid="nodes-custom-clear-title">{{ t('wallet.nodes.customClear') }}</p>
        <button
          v-for="h in customClearNodes"
          :key="h"
          class="node-row"
          :class="{ active: h === activeHost, offline: customPings[h] && !customPings[h].online }"
          :disabled="busy || (customPings[h] ? !customPings[h].online : false)"
          @click="pick(h)"
          :data-testid="`node-row-${h}`"
        >
          <span class="dot" :class="{ online: customPings[h]?.online }" />
          <span class="host mono">{{ h.replace(/^https?:\/\//, '') }}</span>
          <span v-if="isLocalHost(h)" class="local-tag" :title="t('wallet.nodes.localTxHint')">{{ t('wallet.nodes.localTag') }}</span>
          <span v-if="customPings[h]?.height" class="height dim mono">#{{ customPings[h].height }}</span>
          <span class="latency mono">{{ fmtLatency(customPings[h]?.latencyMs ?? null) }}</span>
          <span v-if="h === activeHost" class="active-tag">{{ t('wallet.nodes.connected') }}</span>
        </button>
      </template>
    </div>

    <!-- P2P Nodes list -->
    <div v-else class="list" data-testid="nodes-list-p2p">
      <!-- SHIP-44: состояние push-стрима sth/events/1 -->
      <div class="stream" :class="{ live: wallet.streamLive }" data-testid="nodes-stream-status">
        <span class="dot" :class="{ online: wallet.streamLive, pulse: wallet.streamLive }" />
        <span class="stream-txt">
          {{ wallet.streamLive ? t('wallet.nodes.streamLive') : activeHost.startsWith('api://') ? t('wallet.nodes.streamOff') : t('wallet.nodes.streamLegacy') }}
        </span>
        <span v-if="wallet.netHeight" class="height mono" :title="t('wallet.nodes.netHeight')" data-testid="nodes-net-height">#{{ wallet.netHeight }}</span>
      </div>
      <div v-if="!p2pNodes.length" class="empty dim">
        {{ t('wallet.nodes.emptyP2p') }}
      </div>
      <button
        v-for="h in p2pNodes"
        :key="h"
        class="node-row"
        :class="{ active: h === activeHost, offline: p2pPing(h) && !p2pPing(h)?.online }"
        :disabled="busy || (!!p2pPing(h) && !p2pPing(h)?.online)"
        @click="pick(h)"
        :data-testid="`node-row-${h}`"
      >
        <span class="dot" :class="{ online: p2pPing(h)?.online }" />
        <span class="host mono" :title="h">{{ shortApi(h) }}</span>
        <span v-if="irohPool.some((n) => n.host === h)" class="iroh-tag" title="Post-Quantum P2P (iroh)">PQ</span>
        <span v-if="p2pPing(h)?.height" class="height dim mono">#{{ p2pPing(h)?.height }}</span>
        <span class="latency mono">{{ fmtLatency(p2pPing(h)?.latencyMs ?? null) }}</span>
        <span v-if="h === activeHost" class="active-tag">{{ t('wallet.nodes.connected') }}</span>
      </button>
    </div>

    <p class="footnote dim">{{ t('wallet.nodes.footnote') }}</p>
  </div>
</template>

<style scoped>
.stream {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px;
  border: 1px dashed var(--border, rgba(255, 255, 255, 0.12));
  border-radius: 6px;
  font-size: 10px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.stream.live {
  color: var(--accent);
  border-style: solid;
}
.stream .height {
  margin-left: auto;
}
.dot.pulse {
  animation: stream-pulse 1.6s ease-in-out infinite;
}
@keyframes stream-pulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.45; transform: scale(0.8); }
}
.net-main {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.net-chain {
  font-size: 10px;
  letter-spacing: 0.04em;
}
.net-api {
  font-size: 10px;
}
.net-del {
  margin-left: 6px;
  width: 22px;
  height: 22px;
  border-radius: 50%;
  color: var(--text-dim);
  cursor: pointer;
  flex: none;
}
.net-del:hover {
  color: #f0b429;
  background: rgba(240, 180, 41, 0.12);
}
.net-add {
  padding: 10px;
  border: 1px dashed var(--border);
  border-radius: var(--radius);
  font-size: 11px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  font-weight: var(--fw-800);
  color: var(--accent);
  cursor: pointer;
  transition: border-color 0.15s ease, background-color 0.15s ease;
}
.net-add:hover {
  border-color: var(--accent);
  background: color-mix(in srgb, var(--accent) 8%, transparent);
}
.net-form {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px;
  border: 1px solid var(--border-accent, var(--border));
  border-radius: var(--radius);
  background: var(--panel);
}
.net-form .lbl {
  font-size: 10px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--text-dim);
  font-weight: var(--fw-700);
}
.net-form-row {
  display: flex;
  gap: 6px;
}
.net-inp {
  flex: 1;
  min-width: 0;
  padding: 9px 10px;
  font-size: 12px;
  color: var(--text);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.net-inp:focus {
  outline: none;
  border-color: var(--accent);
}
.net-go,
.net-save,
.net-cancel {
  padding: 8px 12px;
  border-radius: var(--radius);
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  font-weight: var(--fw-800);
  cursor: pointer;
  border: 1px solid var(--border);
  color: var(--text-dim);
}
.net-go,
.net-save {
  color: var(--accent);
  border-color: var(--border-accent, var(--border));
}
.net-go:disabled {
  opacity: 0.5;
  cursor: default;
}
.net-save {
  background: color-mix(in srgb, var(--accent) 14%, transparent);
}
.net-err {
  font-size: 11px;
  color: #f0b429;
}
.net-preview {
  display: flex;
  flex-direction: column;
  gap: 5px;
  padding: 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--bg-elevated);
  font-size: 11px;
}
.np-row {
  display: flex;
  justify-content: space-between;
  gap: 10px;
}
.np-hash {
  max-width: 60%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.net-form-actions {
  display: flex;
  justify-content: flex-end;
  gap: 6px;
  margin-top: 6px;
}
.soon-tag {
  margin-left: auto;
  font-size: 9px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  border: 1px dashed var(--border);
  border-radius: 3px;
  padding: 1px 6px;
}
.iroh-tag {
  font-size: 9px;
  letter-spacing: 0.1em;
  padding: 1px 5px;
  border-radius: 3px;
  border: 1px solid var(--accent);
  color: var(--accent);
  text-transform: uppercase;
}
.nodes {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.hdr {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
}
.hdr-title {
  margin: 0;
  font-size: 11px;
  font-weight: var(--fw-800);
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--accent);
}
.hdr-acts {
  display: flex;
  gap: 6px;
}
.hdr-btn {
  font-size: 10px;
  letter-spacing: 0.08em;
  color: var(--text-dim);
  padding: 5px 9px;
  border-radius: 4px;
  border: 1px solid var(--border);
  background: transparent;
  font-weight: var(--fw-700);
}
.hdr-btn:hover:not(:disabled) {
  color: var(--text);
  border-color: var(--border-accent);
}
.hdr-btn.primary {
  color: var(--accent);
  border-color: color-mix(in srgb, var(--accent) 55%, transparent);
  background: color-mix(in srgb, var(--accent) 12%, transparent);
}
.hdr-btn:disabled {
  opacity: 0.5;
  cursor: wait;
}
.tabs {
  display: flex;
  gap: 4px;
  border-bottom: 1px solid var(--border);
}
.tab {
  padding: 8px 12px;
  font-size: 11px;
  letter-spacing: 0.06em;
  color: var(--text-dim);
  background: transparent;
  border: none;
  border-bottom: 2px solid transparent;
  cursor: pointer;
  font-weight: var(--fw-700);
}
.tab:hover {
  color: var(--text);
}
.tab.on {
  color: var(--accent);
  border-bottom-color: var(--accent);
}
.tab .cnt {
  margin-left: 4px;
  font-size: 10px;
  color: var(--text-dim);
}
.list {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.empty {
  text-align: center;
  padding: 30px 10px;
  font-size: 11.5px;
}
.node-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--panel);
  cursor: pointer;
  text-align: left;
  transition: border-color 0.15s ease, background-color 0.15s ease;
}
.node-row:hover:not(:disabled) {
  border-color: var(--border-accent);
}
.node-row.active {
  border-color: var(--accent);
  background: color-mix(in srgb, var(--accent) 12%, transparent);
}
.node-row.offline {
  opacity: 0.55;
}
.node-row:disabled {
  cursor: not-allowed;
}
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--text-dim);
  flex-shrink: 0;
}
.dot.online {
  background: #2ecc71;
  box-shadow: 0 0 6px #2ecc71;
}
.host {
  flex: 1;
  font-size: 12px;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.height {
  font-size: 10px;
}
.latency {
  font-size: 10.5px;
  color: var(--text);
  font-weight: var(--fw-700);
  min-width: 46px;
  text-align: right;
}
.active-tag {
  font-size: 9px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--accent);
  font-weight: var(--fw-800);
  padding: 2px 6px;
  border-radius: 4px;
  background: color-mix(in srgb, var(--accent) 22%, transparent);
}
.local-tag {
  font-size: 9px;
  letter-spacing: 0.14em;
  font-weight: var(--fw-800);
  padding: 2px 6px;
  border-radius: 4px;
  color: #f0b429;
  background: color-mix(in srgb, #f0b429 20%, transparent);
  cursor: help;
}
.sub-title {
  margin: 10px 0 2px;
  font-size: 10px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
}
.footnote {
  font-size: 10.5px;
  margin-top: 4px;
  line-height: 1.5;
}
.mono {
  font-family: var(--font-mono, monospace);
}
</style>
