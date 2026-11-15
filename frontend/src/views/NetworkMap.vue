<script setup lang="ts">
import { onMounted, onBeforeUnmount, ref, computed, reactive } from 'vue'
import { useI18n } from 'vue-i18n'
import { useAuthStore } from '@/store/auth'
import { heartbeatStatus, type HeartbeatStatus } from '@/lib/bridge'
import SeedingCard from '@/components/SeedingCard.vue'
import { useSeedingStore } from '@/store/seeding'
import { useUiStore } from '@/store/ui'
import { networkMap, usernameOfAddress, peerSources, dhtStats, dhtNodesList, getSeedersEffective, getSeedersAuto, getProfileStats, swarmTopSeeders, cloudSeederRegistryGet, cloudSeederStatusGet, cloudSeederRescuesGet, type NetworkSeeder, type DhtStats, type ProfileStats, type SwarmSeeder, type SeederStatus } from '@/lib/bridge'
import { shortAddr } from '@/lib/crypto'
import NodePlanet from '@/components/NodePlanet.vue'
import Sparkline from '@/components/Sparkline.vue'
import { getDhtHistory, pushDht } from '@/lib/dhtHistory'
import { useNetStore } from '@/store/net'

const { t } = useI18n()
const auth = useAuthStore()
const net = useNetStore()

const seeders = ref<NetworkSeeder[]>([])
const loading = ref(false)
// ---- МОЯ НОДА (v1.135.185): всегда видна в таблице; если трекеры меня не
// вернули — локальная строка с причиной (раздача выключена / заблокировано /
// heartbeat отклонён). Клик по своей строке → модалка Earn STH (SeedingCard).
const seedingStore = useSeedingStore()
const ui = useUiStore()
// Вкладки таблицы (v1.135.188): пользователи+сидеры / Cloud Seeders
const boardTab = ref<'nodes' | 'cloud'>('nodes')
const hb = ref<HeartbeatStatus | null>(null)
const showSeedModal = ref(false)
const myAddr = computed(() => (auth.address || '').trim())
function isMe(s: NetworkSeeder): boolean {
  return !!myAddr.value && (s.node || '').trim() === myAddr.value
}
const myIdx = computed(() => seeders.value.findIndex(isMe))
const meOnMap = computed(() => myIdx.value >= 0)
// закреплённая строка нужна, если меня нет в таблице ИЛИ я за пределами топ-10
const mePinned = computed(() => !!myAddr.value && auth.isTauri && (myIdx.value < 0 || myIdx.value >= MAX_ROWS))
const meRow = computed<NetworkSeeder>(() =>
  meOnMap.value
    ? seeders.value[myIdx.value]
    : {
        endpointId: 'me-local',
        node: myAddr.value,
        metricsUrl: '',
        appsSeeded: seedingStore.metrics.activeApps || 0,
        gbSeeded: seedingStore.metrics.seededGb || 0,
        uptimeSecs: 0,
        score: nodeStats.value.byteHours || 0,
        online: false,
        lastSeen: 0,
      },
)
const meReason = computed(() => {
  const h = hb.value
  if (!h) return t('network.meNotOnMap')
  if (!h.enabled) return t('network.meSeedingOff')
  if (!h.unlocked) return t('network.meLocked')
  if (!h.lastAttempt) return t('network.meHbPending')
  if (!h.anyOk) {
    const r = h.trackers.map(([, v]) => v).filter((v) => v !== 'ok').join('; ').slice(0, 160)
    return t('network.meHbFailed', { r })
  }
  return t('network.meWaiting')
})
const sources = ref<Record<string, string>>({})
// «Личная» скорость каждого узла по замерам swarm-бандита (endpointId → метрика).
const mySeeders = ref<Record<string, SwarmSeeder>>({})
const myMaxSpeed = computed(() =>
  Object.values(mySeeders.value).reduce((m, s) => Math.max(m, s.speed), 0),
)
function mine(s: NetworkSeeder): SwarmSeeder | undefined {
  return mySeeders.value[s.endpointId]
}
function mineTitle(m: SwarmSeeder): string {
  return `✓${m.successes} · ✗${m.errors}`
}
// Текст hover-тултипа для точки узла на глобусе: метка + личная скорость + ✓/✗.
const globeTooltips = computed<Record<string, string>>(() => {
  const m: Record<string, string> = {}
  for (const s of seeders.value) {
    const label = nodeLabel(s)
    const mm = mine(s)
    if (mm && mm.successes > 0) {
      const pct = myMaxSpeed.value ? Math.round((mm.speed / myMaxSpeed.value) * 100) : 0
      const errs = mm.errors > 0 ? ` · ✗${mm.errors}` : ''
      m[s.endpointId] = `${label} · ${t('network.mySpeed')} ${pct}% · ✓${mm.successes}${errs}`
    } else {
      m[s.endpointId] = `${label} · ${t('network.noData')}`
    }
  }
  return m
})
const DHT_CACHE_KEY = 'sth_dht_stats'
function loadDhtCache(): DhtStats {
  try {
    const raw = localStorage.getItem(DHT_CACHE_KEY)
    if (raw) {
      const d = JSON.parse(raw)
      if (d && typeof d.nodes === 'number') return d
    }
  } catch { /* ignore */ }
  return { enabled: true, nodes: 0, via_smartnet: false }
}
const dht = ref<DhtStats>(loadDhtCache())
const dhtHist = ref<number[]>(getDhtHistory())
const dhtNodes = ref<string[]>([])
let timer: ReturnType<typeof setInterval> | null = null

// Персональная экономика Byte-Hours (ранг ноды по объёму раздачи × время).
const NODE_STATS_KEY = 'sth_node_stats'
function loadNodeStatsCache(): ProfileStats {
  try {
    const raw = localStorage.getItem(NODE_STATS_KEY)
    if (raw) {
      const d = JSON.parse(raw)
      if (d && typeof d.byteHours === 'number') return d
    }
  } catch { /* ignore */ }
  return { byteHours: 0, rank: 'Leech', uploadedBytes: 0, nextRankAt: 1, progressToNext: 0 }
}
const nodeStats = ref<ProfileStats>(loadNodeStatsCache())
const RANK_META: Record<string, { label: string; color: string }> = {
  Leech: { label: 'LEECH', color: '#64748b' },
  Peer: { label: 'PEER', color: '#38bdf8' },
  Seeder: { label: 'SEEDER', color: '#34d399' },
  SuperSeed: { label: 'SUPERSEED', color: '#a855f7' },
  Anchor: { label: 'ANCHOR', color: '#f59e0b' },
}
const rankMeta = computed(() => RANK_META[nodeStats.value.rank] || RANK_META.Leech)
const byteHoursLabel = computed(() => {
  const v = nodeStats.value.byteHours
  if (v >= 1000) return (v / 1000).toFixed(2) + 'K'
  if (v >= 1) return v.toFixed(2)
  return v.toFixed(3)
})
const progressPct = computed(() => Math.round((nodeStats.value.progressToNext || 0) * 100))

// Награды за раздачу торрентов (по суммарному объёму отданных GB → бонус к репутации).
const SEED_AWARDS = [
  { id: 'superseed', label: 'SUPERSEED', gb: 1000, rep: 100, color: '#a855f7', icon: '⬢' },
  { id: 'anchor', label: 'ANCHOR', gb: 5000, rep: 500, color: '#f59e0b', icon: '⚓' },
]
const seededGb = computed(() => nodeStats.value.uploadedBytes / (1024 ** 3))
function awardEarned(gb: number): boolean {
  return seededGb.value >= gb
}
function awardPct(gb: number): number {
  return Math.min(100, Math.round((seededGb.value / gb) * 100))
}
const awardRepTotal = computed(() =>
  SEED_AWARDS.filter((a) => seededGb.value >= a.gb).reduce((s, a) => s + a.rep, 0),
)
// Предыдущие достижения — ранги по объёму раздачи (GB):
// LEECH <10 → PEER ≥10 → SEEDER ≥500 → SUPERSEED ≥1000 → ANCHOR ≥5000.
const RANK_ORDER = ['Leech', 'Peer', 'Seeder', 'SuperSeed', 'Anchor']
const RANK_TIERS = [
  { id: 'Leech', label: 'LEECH', at: 0, color: '#64748b' },
  { id: 'Peer', label: 'PEER', at: 10, color: '#38bdf8' },
  { id: 'Seeder', label: 'SEEDER', at: 500, color: '#34d399' },
  { id: 'SuperSeed', label: 'SUPERSEED', at: 1000, color: '#a855f7' },
  { id: 'Anchor', label: 'ANCHOR', at: 5000, color: '#f59e0b' },
]
const currentRankIdx = computed(() => Math.max(0, RANK_ORDER.indexOf(nodeStats.value.rank)))
function fmtBytes(n: number): string {
  if (!n) return '0 B'
  const u = ['B', 'KB', 'MB', 'GB', 'TB']
  let i = 0
  let v = n
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++ }
  return `${v.toFixed(v >= 100 || i === 0 ? 0 : 1)} ${u[i]}`
}

// Per-peer discovery/transport source → badge label + colour.
//   direct = live P2P (holepunch/LAN)    relay = via relay server
//   mdns   = found on the local network  wan   = from a tracker directory
const SOURCE_META: Record<string, { label: string; color: string }> = {
  direct: { label: 'DIRECT', color: '#34d399' },
  relay: { label: 'RELAY', color: '#38bdf8' },
  mdns: { label: 'mDNS', color: '#14c9b8' },
  wan: { label: 'WAN', color: '#64748b' },
  dht: { label: 'DHT', color: '#f59e0b' },
  seeder: { label: 'SEEDER', color: '#a855f7' },
  client: { label: 'CLIENT', color: '#60a5fa' },
}
function sourceOf(s: NetworkSeeder): string {
  return sources.value[s.endpointId] || 'wan'
}
function sourceMeta(s: NetworkSeeder) {
  return SOURCE_META[sourceOf(s)] || SOURCE_META.wan
}
// Known SmartNet bootstrap seeder addrs (manual + auto + env + default, ip:port).
const seederSet = ref<Set<string>>(new Set())
const seederList = computed(() => [...seederSet.value])
const seederNodeCount = computed(() => seederList.value.length)

// Cloud Seeders (публичный анти-цензурный слой на Яндекс Диске). URI формата
// `sn://seed/y@<folder>@<pubkey>`. В таблицу выводим только «userId» автора
// (короткий pubkey) плюс метку CLOUD — без хеша папки и без OAuth-идентификации.
const cloudSeederRegistry = ref<string[]>([])
const cloudSeederStatuses = ref<Record<string, SeederStatus>>({})
// Сколько раз за сессию сидер «спас» установку dApp (P2P был недоступен).
const cloudSeederRescues = ref<Record<string, number>>({})
interface CloudSeederRow {
  uri: string
  pubkey: string // 66-hex compressed secp256k1 — идентификатор автора
  userId: string // короткая форма для UI (без полной пары ключей)
  provider: string // бейдж провайдера: YA / GD / MAIL (различает дубли pubkey)
  online: boolean
  checkedAt: string
  rescues: number // «спасённых» установок за сессию из этого сидера
}
const CS_PROVIDER_LABEL: Record<string, string> = { y: 'YA', g: 'GD', m: 'MAIL' }
const cloudSeederRows = computed<CloudSeederRow[]>(() => {
  const rows: CloudSeederRow[] = []
  for (const uri of cloudSeederRegistry.value) {
    // sn://seed/<y|g|m>@<folder_token>@<pubkey_hex_66>
    const m = /^sn:\/\/seed\/([ygm])@.+@([0-9a-f]{66})$/i.exec(uri)
    if (!m) continue
    const pubkey = m[2].toLowerCase()
    const st = cloudSeederStatuses.value[uri]
    rows.push({
      uri,
      pubkey,
      userId: `${pubkey.slice(0, 8)}…${pubkey.slice(-6)}`,
      provider: CS_PROVIDER_LABEL[m[1].toLowerCase()] || '?',
      online: !!st?.ok,
      checkedAt: st?.checkedAt || '',
      rescues: cloudSeederRescues.value[uri] || 0,
    })
  }
  return rows
})
// Globe point colours keyed by id (peers by transport/source; DHT nodes amber;
// bootstrap SEEDER nodes always highlighted in violet).
// Режим раскраски: по источнику/транспорту (по умолчанию) ЛИБО по «моей»
// скорости (замеры swarm-бандита) — сеть визуально подсветит лучшие маршруты.
const globeColorMode = ref<'source' | 'speed'>('source')
function speedColor(m?: SwarmSeeder): string {
  if (!m || m.successes === 0) return '#3a4656' // нет личных данных → тускло-серый
  if (m.errors > 0) return '#ff8a3d' // были сбои → оранжевый
  const r = myMaxSpeed.value ? m.speed / myMaxSpeed.value : 0
  if (r >= 0.66) return '#22c55e' // быстрый → яркий зелёный
  if (r >= 0.33) return '#00e5ff' // средний → циан
  return '#5b7aa8' // медленный → приглушённый синий
}
const globeColors = computed<Record<string, string>>(() => {
  const m: Record<string, string> = {}
  if (globeColorMode.value === 'speed') {
    // По моей скорости: узлы-источники окрашены по замерам бандита, остальные тускло.
    for (const s of seeders.value) m[s.endpointId] = speedColor(mine(s))
    for (const n of dhtNodes.value) if (!(n in m)) m[n] = '#3a4656'
    for (const a of seederList.value) if (!(a in m)) m[a] = '#3a4656'
    return m
  }
  // Клиентские (UI) узлы — голубым, серверы (headless) — по транспорту.
  for (const s of seeders.value) m[s.endpointId] = isHeadless(s) ? sourceMeta(s).color : SOURCE_META.client.color
  for (const n of dhtNodes.value) m[n] = seederSet.value.has(n) ? SOURCE_META.seeder.color : SOURCE_META.dht.color
  for (const a of seederList.value) m[a] = SOURCE_META.seeder.color
  return m
})
// Seeders first (always shown as violet anchors), then peers, then DHT nodes
// (excluding any already shown as seeders) so the leaderboard always renders.
const globePeerIds = computed(() => [
  ...seederList.value,
  ...seeders.value.map((s) => s.endpointId),
  ...dhtNodes.value.filter((n) => !seederSet.value.has(n)),
])
const CACHE_KEY = 'sth_netmap_cache'
// v1.135.188: снимок всех входов карты (глобус + таблица + cloud seeders), чтобы
// после перезапуска показать последнее состояние мгновенно, а не пустой глобус.
const SNAP_KEY = 'sth_netmap_snapshot'
function loadCache() {
  try {
    const raw = localStorage.getItem(CACHE_KEY)
    if (raw) {
      const arr = JSON.parse(raw)
      if (Array.isArray(arr) && arr.length) seeders.value = arr
    }
    const snap = JSON.parse(localStorage.getItem(SNAP_KEY) || 'null')
    if (snap && typeof snap === 'object') {
      if (Array.isArray(snap.dhtNodes)) dhtNodes.value = snap.dhtNodes
      if (Array.isArray(snap.seederList)) seederSet.value = new Set(snap.seederList)
      if (snap.sources && typeof snap.sources === 'object') sources.value = snap.sources
      if (Array.isArray(snap.cloudRegistry)) cloudSeederRegistry.value = snap.cloudRegistry
      if (snap.cloudStatuses && typeof snap.cloudStatuses === 'object') cloudSeederStatuses.value = snap.cloudStatuses
    }
  } catch { /* ignore */ }
}
function saveCache(list: NetworkSeeder[]) {
  try { localStorage.setItem(CACHE_KEY, JSON.stringify(list)) } catch { /* ignore */ }
}
function saveSnapshot() {
  try {
    localStorage.setItem(SNAP_KEY, JSON.stringify({
      dhtNodes: dhtNodes.value,
      seederList: seederList.value,
      sources: sources.value,
      cloudRegistry: cloudSeederRegistry.value,
      cloudStatuses: cloudSeederStatuses.value,
    }))
  } catch { /* ignore */ }
}

const onlineCount = computed(() => seeders.value.filter((s) => s.online).length)
const totalGb = computed(() =>
  seeders.value.reduce((a, s) => a + (s.gbSeeded || 0), 0),
)
// Decentralisation split: dedicated headless servers (have /metrics) vs home
// "client" UI nodes (no /metrics). Mirrors the tracker's /stats split.
const serverCount = computed(
  () => seeders.value.filter((s) => s.metricsUrl && s.metricsUrl.trim()).length,
)
const clientCount = computed(() => seeders.value.length - serverCount.value)

// Show at most 10 rows in the leaderboard; everyone else is reflected only in
// the "Сидеров" counter (the full list can be hundreds of UI-client peers).
const MAX_ROWS = 10
const visibleSeeders = computed(() => seeders.value.slice(0, MAX_ROWS))
const overflowCount = computed(() => Math.max(0, seeders.value.length - MAX_ROWS))

function fmtUptime(s: number): string {
  if (!s) return '—'
  const d = Math.floor(s / 86400)
  const h = Math.floor((s % 86400) / 3600)
  const m = Math.floor((s % 3600) / 60)
  if (d > 0) return `${d}d ${h}h`
  if (h > 0) return `${h}h ${m}m`
  if (m > 0) return `${m}m`
  return `${s}s`
}

// Headless seeders expose a metrics endpoint; UI-client peers don't — use that
// as the discriminator for the node-type icon (server vs user).
function isHeadless(s: NetworkSeeder): boolean {
  return !!(s.metricsUrl && s.metricsUrl.trim())
}

// Reward-address → registered username (resolved client-side off the on-chain
// name registry; the tracker never needs to know names). '' = looked-up, none.
const names = reactive<Record<string, string>>({})
const pending = new Set<string>()
async function resolveNames(list: NetworkSeeder[]) {
  for (const s of list) {
    const addr = (s.node || '').trim()
    if (!addr || addr in names || pending.has(addr)) continue
    pending.add(addr)
    try {
      const u = await usernameOfAddress(addr)
      names[addr] = u || ''
    } catch {
      names[addr] = ''
    } finally {
      pending.delete(addr)
    }
  }
}
// Display label for a node: "@username" when registered, else shortened address.
function nodeLabel(s: NetworkSeeder): string {
  const addr = (s.node || '').trim()
  if (addr && names[addr]) return '@' + names[addr]
  return addr ? shortAddr(addr, 8, 6) : shortAddr(s.endpointId, 8, 6)
}
function hasName(s: NetworkSeeder): boolean {
  const addr = (s.node || '').trim()
  return !!(addr && names[addr])
}

async function refresh() {
  if (!auth.isTauri || loading.value) return
  loading.value = true
  try {
    const r = await networkMap()
    // Keep the last known map if a poll comes back empty (trackers not yet
    // reached) so the view never flashes back to 0 after it had data.
    if (r.length) {
      seeders.value = r
      saveCache(r)
      resolveNames(r.slice(0, MAX_ROWS))
      peerSources(r.map((s) => s.endpointId)).then((m) => (sources.value = m))
    }
    heartbeatStatus().then((h) => (hb.value = h)).catch(() => {})
    swarmTopSeeders().then((list) => {
      const m: Record<string, SwarmSeeder> = {}
      for (const x of list) m[x.peer] = x
      mySeeders.value = m
    })
    dhtStats().then((d) => {
      // Не понижаем кэшированное число до 0 при кратковременной просадке —
      // Карта сети должна оставаться согласованной с Dashboard.
      if (d.nodes > 0 || !dht.value.nodes) {
        dht.value = d
        try { localStorage.setItem(DHT_CACHE_KEY, JSON.stringify(d)) } catch { /* ignore */ }
      } else {
        dht.value = { ...d, nodes: dht.value.nodes }
      }
      if (d.nodes > 0) dhtHist.value = pushDht(d.nodes)
    })
    dhtNodesList().then((n) => (dhtNodes.value = n.slice(0, 48)))
    Promise.all([getSeedersEffective(), getSeedersAuto()]).then(([eff, auto]) => {
      seederSet.value = new Set([...eff, ...auto])
    })
    getProfileStats().then((s) => {
      nodeStats.value = s
      try { localStorage.setItem(NODE_STATS_KEY, JSON.stringify(s)) } catch { /* ignore */ }
    })
    // Cloud Seeders (реестр + статусы) — параллельно с P2P-запросами.
    cloudSeederRegistryGet().then((list) => (cloudSeederRegistry.value = list)).catch(() => {})
    cloudSeederStatusGet().then((m) => (cloudSeederStatuses.value = m)).catch(() => {})
    cloudSeederRescuesGet().then((m) => (cloudSeederRescues.value = m)).catch(() => {})
    // снимок входов глобуса/таблицы — после того как параллельные запросы отработают
    setTimeout(saveSnapshot, 6000)
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  loadCache() // show last-known seeders instantly instead of 0 while refreshing
  if (seeders.value.length) resolveNames(seeders.value.slice(0, MAX_ROWS))
  if (auth.isTauri) {
    seedingStore.init().catch(() => {})
    refresh()
    // фоновое обновление каждые 30 с, пока страница открыта (кнопки «Обновить» нет)
    timer = setInterval(() => { if (!document.hidden && net.online) refresh() }, 30000)
  }
})
onBeforeUnmount(() => {
  if (timer) clearInterval(timer)
})
</script>

<template>
  <div class="netmap" data-testid="network-view">
    <!-- Earn STH: настройки раздачи прямо с карты сети (клик по своей ноде) -->
    <div v-if="showSeedModal" class="seed-modal-back" @click.self="showSeedModal = false" data-testid="network-seed-modal">
      <div class="seed-modal">
        <header class="seed-modal-head">
          <h3 class="seed-modal-title">{{ t('network.seedModalTitle') }}</h3>
          <button class="seed-modal-x" @click="showSeedModal = false" data-testid="network-seed-modal-close">✕</button>
        </header>
        <SeedingCard />
      </div>
    </div>
    <div class="head">
      <h1 class="page-title">// {{ t('network.title') }}_</h1>
      <span class="auto-upd dim" :class="{ live: loading }" :title="t('network.autoRefreshHint')" data-testid="network-auto-refresh">
        <i class="auto-dot" />{{ loading ? t('network.refreshing') : t('network.autoRefresh') }}
      </span>
    </div>

    <p class="hint dim">{{ t('network.hint') }}</p>

    <!-- non-native notice -->
    <div v-if="!auth.isTauri" class="empty panel" data-testid="network-web-note">
      <span class="empty-mark">[ web ]</span>
      <span class="dim">{{ t('network.webOnly') }}</span>
    </div>

    <template v-else>
      <!-- summary stats -->
      <div class="stats" data-testid="network-stats">
        <div class="stat panel">
          <span class="stat-k">{{ t('network.seeders') }}</span>
          <span class="stat-v accent" data-testid="network-count">{{ seeders.length }}</span>
        </div>
        <div class="stat panel">
          <span class="stat-k">{{ t('network.online') }}</span>
          <span class="stat-v" data-testid="network-online">{{ onlineCount }}</span>
        </div>
        <div class="stat panel">
          <span class="stat-k">{{ t('network.totalSeeded') }}</span>
          <span class="stat-v" data-testid="network-total-gb">{{ totalGb.toFixed(2) }} GB</span>
        </div>
        <div class="stat panel">
          <span class="stat-k">{{ t('network.split') }}</span>
          <span class="stat-v split" data-testid="network-split">
            <span class="srv">{{ serverCount }}</span><span class="sep">/</span><span class="cli">{{ clientCount }}</span>
          </span>
        </div>
        <div class="stat panel" data-testid="network-dht-stat">
          <span class="stat-k">{{ t('network.dhtNodes') }}</span>
          <span class="stat-v" :class="{ accent: dht.enabled && dht.nodes > 0 }" data-testid="network-dht-nodes">
            {{ dht.enabled ? dht.nodes : t('network.dhtOff') }}
          </span>
          <span
            v-if="dht.enabled && dht.nodes > 0"
            class="dht-via"
            :class="dht.via_smartnet ? 'smartnet' : 'public'"
            :title="dht.via_smartnet ? t('network.dhtViaSmartnetHint') : t('network.dhtViaPublicHint')"
            data-testid="network-dht-via"
          >
            <i class="via-dot" />
            {{ dht.via_smartnet ? t('network.dhtViaSmartnet') : t('network.dhtViaPublic') }}
          </span>
          <Sparkline
            v-if="dht.enabled && dhtHist.length > 1"
            :values="dhtHist"
            color="#f59e0b"
            class="dht-spark"
            data-testid="network-dht-spark"
          />
        </div>
      </div>

      <!-- personal node economy: Byte-Hours + rank + progress -->
      <div v-if="ui.mode === 'geek'" class="node-card panel" data-testid="node-economy-card">
        <div class="nc-head">
          <span class="nc-title">// {{ t('network.myNode') }}</span>
          <span
            class="rank-badge"
            :style="{ '--rank-color': rankMeta.color }"
            :title="t('network.rankHint')"
            data-testid="node-rank-badge"
          >
            {{ rankMeta.label }}
          </span>
        </div>
        <div class="nc-grid">
          <div class="nc-metric">
            <span class="nc-k">{{ t('network.byteHours') }}</span>
            <span class="nc-v accent" data-testid="node-byte-hours">
              {{ byteHoursLabel }} <small>{{ t('network.gibH') }}</small>
            </span>
          </div>
          <div class="nc-metric">
            <span class="nc-k">{{ t('network.totalUploaded') }}</span>
            <span class="nc-v" data-testid="node-uploaded">{{ fmtBytes(nodeStats.uploadedBytes) }}</span>
          </div>
        </div>
        <div class="nc-progress" data-testid="node-rank-progress">
          <div class="ncp-labels">
            <span class="dim">{{ progressPct }}%</span>
            <span v-if="nodeStats.nextRankAt != null" class="dim">
              {{ t('network.nextRankAt', { v: nodeStats.nextRankAt?.toFixed(0) }) }}
            </span>
            <span v-else class="dim accent">{{ t('network.maxRank') }}</span>
          </div>
          <div class="ncp-track">
            <div
              class="ncp-fill"
              :style="{ width: progressPct + '%', '--rank-color': rankMeta.color }"
            />
          </div>
        </div>

        <!-- Награды за раздачу — компактно: ранги (Byte-Hours) + награды по GB -->
        <div class="seed-awards" data-testid="seed-awards">
          <div class="sa-head">
            <span class="sa-title">{{ t('network.awardsTitle') }}</span>
            <div class="rt-strip" data-testid="rank-tiers">
              <span
                v-for="(tier, i) in RANK_TIERS"
                :key="tier.id"
                class="rt-pill"
                :class="{ reached: currentRankIdx >= i }"
                :style="{ '--t-color': tier.color }"
                :data-testid="`rank-tier-${tier.id}`"
              >{{ tier.label }}</span>
            </div>
            <span v-if="awardRepTotal > 0" class="sa-rep" data-testid="seed-awards-rep">
              +{{ awardRepTotal }} {{ t('network.reputation') }}
            </span>
          </div>
          <div class="sa-rows">
            <div
              v-for="a in SEED_AWARDS"
              :key="a.id"
              class="sa-row"
              :class="{ earned: awardEarned(a.gb) }"
              :style="{ '--a-color': a.color }"
              :data-testid="`seed-award-${a.id}`"
            >
              <span class="sa-icon">{{ a.icon }}</span>
              <span class="sa-name">{{ a.label }}</span>
              <span class="sa-req dim" :data-testid="`seed-award-${a.id}-progress`">
                {{ seededGb.toFixed(seededGb < 10 ? 1 : 0) }}/{{ a.gb.toLocaleString() }} GB · <b class="sa-rep-val">+{{ a.rep }}</b>
              </span>
              <div class="sa-track"><div class="sa-fill" :style="{ width: awardPct(a.gb) + '%' }" /></div>
              <span class="sa-pct" :class="{ done: awardEarned(a.gb) }" :data-testid="`seed-award-${a.id}-state`">{{ awardEarned(a.gb) ? '✓' : awardPct(a.gb) + '%' }}</span>
            </div>
          </div>
        </div>
      </div>

      <!-- leaderboard (65%) + globe (35%) -->
      <div v-if="seeders.length" class="map-split" data-testid="network-map-split">
      <div class="board panel" data-testid="network-board">
        <div class="board-tabs" role="tablist" data-testid="network-board-tabs">
          <button
            class="bt"
            :class="{ on: boardTab === 'nodes' }"
            role="tab"
            @click="boardTab = 'nodes'"
            data-testid="network-tab-nodes"
          >{{ t('network.tabNodes') }} <span class="bt-n">{{ seeders.length }}</span></button>
          <button
            class="bt"
            :class="{ on: boardTab === 'cloud' }"
            role="tab"
            @click="boardTab = 'cloud'"
            data-testid="network-tab-cloud"
          >☁ {{ t('network.tabCloud') }} <span class="bt-n">{{ cloudSeederRows.length }}</span></button>
          <span v-if="boardTab === 'cloud'" class="bt-hint dim" :title="t('network.cloudSeedersHint')">{{ t('network.cloudSeedersHint') }}</span>
        </div>
        <table class="lb">
          <thead>
            <tr>
              <th class="c-rank">{{ t('network.rank') }}</th>
              <th class="c-node">{{ t('network.node') }}</th>
              <th class="c-type">Type</th>
              <th class="c-src">{{ t('network.source') }}</th>
              <th class="c-num">{{ t('network.apps') }}</th>
              <th class="c-num">{{ t('network.gb') }}</th>
              <th class="c-num">{{ t('network.uptime') }}</th>
              <th class="c-num">{{ t('network.score') }}</th>
              <th class="c-mine" :title="t('network.mySpeedHint')">{{ t('network.mySpeed') }}</th>
            </tr>
          </thead>
          <tbody v-if="boardTab === 'nodes'">
            <tr
              v-for="(s, i) in visibleSeeders"
              :key="s.endpointId"
              :class="{ me: isMe(s) }"
              :title="isMe(s) ? t('network.meClick') : undefined"
              @click="isMe(s) && (showSeedModal = true)"
              :data-testid="`network-row-${s.endpointId}`"
            >
              <td class="c-rank" :class="{ top: i < 3 }">{{ i + 1 }}</td>
              <td class="c-node">
                <span
                  class="badge"
                  :class="s.online ? 'on' : 'off'"
                  :title="s.online ? t('network.statusOn') : t('network.statusOff')"
                ></span>
                <span class="node-id" :class="{ named: hasName(s) }" :title="s.node || s.endpointId">{{ nodeLabel(s) }}</span>
                <span v-if="hasName(s)" class="node-addr">{{ shortAddr(s.node, 6, 4) }}</span>
              </td>
              <td class="c-type">
                <span
                  class="kind-chip"
                  :class="isHeadless(s) ? 'headless' : 'client'"
                  :title="isHeadless(s) ? 'Headless server (seeder)' : 'User (UI client)'"
                  :data-testid="`network-kind-${s.endpointId}`"
                >
                  <!-- server icon (headless) -->
                  <svg v-if="isHeadless(s)" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <rect x="2" y="3" width="20" height="7" rx="1.5" />
                    <rect x="2" y="14" width="20" height="7" rx="1.5" />
                    <line x1="6" y1="6.5" x2="6" y2="6.5" />
                    <line x1="6" y1="17.5" x2="6" y2="17.5" />
                  </svg>
                  <!-- user icon (UI client) -->
                  <svg v-else width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <circle cx="12" cy="8" r="4" />
                    <path d="M4 21v-1a6 6 0 0 1 6-6h4a6 6 0 0 1 6 6v1" />
                  </svg>
                  <span class="kind-label">{{ isHeadless(s) ? 'SEED' : 'USEED' }}</span>
                </span>
              </td>
              <td class="c-src">
                <span
                  class="src-chip"
                  :style="{ color: sourceMeta(s).color, borderColor: sourceMeta(s).color }"
                  :data-testid="`network-source-${s.endpointId}`"
                  :title="t('network.sourceHint')"
                >
                  <i class="src-dot" :style="{ background: sourceMeta(s).color }" />
                  {{ sourceMeta(s).label }}
                </span>
              </td>
              <td class="c-num">{{ s.appsSeeded }}</td>
              <td class="c-num">{{ s.gbSeeded.toFixed(3) }}</td>
              <td class="c-num dim">{{ fmtUptime(s.uptimeSecs) }}</td>
              <td class="c-num score">{{ s.score.toFixed(2) }}</td>
              <td class="c-mine" :data-testid="`network-myspeed-${s.endpointId}`">
                <span v-if="mine(s)" class="mine-wrap" :title="mineTitle(mine(s)!)">
                  <span class="mine-bar">
                    <span
                      class="mine-fill"
                      :class="{ bad: mine(s)!.errors > 0 }"
                      :style="{ width: (myMaxSpeed ? Math.max(10, (mine(s)!.speed / myMaxSpeed) * 100) : 10) + '%' }"
                    />
                  </span>
                  <span class="mine-ok">✓{{ mine(s)!.successes }}</span>
                </span>
                <span v-else class="dim mine-none">—</span>
              </td>
            </tr>
            <!-- МОЯ НОДА: закреплённая строка (вне топ-10 или ещё не подтверждена трекерами) -->
            <tr
              v-if="mePinned"
              class="me me-pinned"
              :class="{ local: !meOnMap }"
              :title="t('network.meClick')"
              @click="showSeedModal = true"
              data-testid="network-me-row"
            >
              <td class="c-rank">{{ meOnMap ? myIdx + 1 : '—' }}</td>
              <td class="c-node">
                <span class="badge" :class="meRow.online ? 'on' : 'off'"></span>
                <span class="node-id" :class="{ named: hasName(meRow) }" :title="myAddr">{{ nodeLabel(meRow) }}</span>
                <span class="me-tag">{{ t('network.meRow') }}</span>
              </td>
              <td class="c-type">
                <span class="kind-chip client">
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <circle cx="12" cy="8" r="4" />
                    <path d="M4 21v-1a6 6 0 0 1 6-6h4a6 6 0 0 1 6 6v1" />
                  </svg>
                  <span class="kind-label">USEED</span>
                </span>
              </td>
              <td class="c-src">
                <span
                  v-if="meOnMap"
                  class="src-chip"
                  :style="{ color: sourceMeta(meRow).color, borderColor: sourceMeta(meRow).color }"
                ><i class="src-dot" :style="{ background: sourceMeta(meRow).color }" />{{ sourceMeta(meRow).label }}</span>
                <span v-else class="src-chip" style="color: #f59e0b; border-color: #f59e0b" :title="meReason" data-testid="network-me-local">
                  <i class="src-dot" style="background: #f59e0b" />{{ t('network.meLocal') }}
                </span>
              </td>
              <td class="c-num">{{ meRow.appsSeeded }}</td>
              <td class="c-num">{{ meRow.gbSeeded.toFixed(3) }}</td>
              <td class="c-num dim">{{ fmtUptime(meRow.uptimeSecs) }}</td>
              <td class="c-num score">{{ meRow.score.toFixed(2) }}</td>
              <td class="c-mine">
                <span v-if="!meOnMap" class="me-reason" data-testid="network-me-reason">{{ meReason }}</span>
                <span v-else class="dim mine-none">—</span>
              </td>
            </tr>
          </tbody>
          <!-- Cloud Seeders — публичный анти-цензурный слой (Yandex-архивы).
               В таблицу выводим строго userId автора (короткий pubkey) + метку
               CLOUD; никаких хешей папок или OAuth-адресов не раскрываем. -->
          <tbody v-if="boardTab === 'cloud'" data-testid="network-cloud-seeders">
            <tr v-if="!cloudSeederRows.length" class="cs-sep">
              <td colspan="9"><span class="dim">{{ t('network.cloudEmpty') }}</span></td>
            </tr>
            <tr
              v-for="(r, i) in cloudSeederRows"
              :key="`cs-${r.uri}`"
              class="cs-row"
              :data-testid="`network-cloud-row-${r.pubkey}`"
            >
              <td class="c-rank">☁</td>
              <td class="c-node">
                <span
                  class="badge"
                  :class="r.online ? 'on' : 'off'"
                  :title="r.online ? t('network.statusOn') : t('network.statusOff')"
                ></span>
                <span
                  class="cs-prov"
                  :title="r.provider === 'YA' ? 'Yandex Disk' : r.provider === 'GD' ? 'Google Drive' : 'Mail.ru Cloud'"
                  :data-testid="`network-cloud-provider-${i}`"
                >{{ r.provider }}</span>
                <span class="node-id cs-id" :title="r.pubkey" :data-testid="`network-cloud-userid-${i}`">{{ r.userId }}</span>
              </td>
              <td class="c-type">
                <span class="kind-chip cs-kind" :data-testid="`network-cloud-kind-${i}`">
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <path d="M17.5 19a4.5 4.5 0 1 0-.8-8.94A6 6 0 0 0 5 12a4 4 0 0 0 0 8h12.5z" />
                  </svg>
                  <span class="kind-label">CLOUD</span>
                </span>
                <span
                  v-if="r.rescues > 0"
                  class="cs-rescue"
                  :title="t('network.cloudRescues', { n: r.rescues })"
                  :data-testid="`network-cloud-rescues-${i}`"
                >⛑ {{ r.rescues }}</span>
              </td>
              <td class="c-src">
                <span
                  class="src-chip"
                  :style="{ color: SOURCE_META.seeder.color, borderColor: SOURCE_META.seeder.color }"
                  :data-testid="`network-cloud-source-${i}`"
                  :title="t('network.cloudSeedersHint')"
                >
                  <i class="src-dot" :style="{ background: SOURCE_META.seeder.color }" />
                  {{ SOURCE_META.seeder.label }}
                </span>
              </td>
              <td class="c-num dim">—</td>
              <td class="c-num dim">—</td>
              <td class="c-num dim">—</td>
              <td class="c-num dim">—</td>
              <td class="c-mine dim">—</td>
            </tr>
          </tbody>
        </table>
        <p v-if="overflowCount > 0" class="overflow-note dim" data-testid="network-overflow-note">
          {{ t('network.andMore', { n: overflowCount }) }}
        </p>
      </div>

      <!-- globe: all peers/clients positioned + coloured by transport source -->
      <aside class="globe-col panel" data-testid="network-globe">
        <div class="globe-host">
          <NodePlanet
            :peer-ids="globePeerIds"
            :seed="'smartnet-map'"
            :transfer-active="false"
            :active="true"
            :color-map="globeColors"
            :tooltips="globeTooltips"
            :max-points="72"
          />
        </div>
        <div class="globe-mode" data-testid="globe-color-mode">
          <button
            class="gm-btn"
            :class="{ on: globeColorMode === 'source' }"
            @click="globeColorMode = 'source'"
            data-testid="globe-mode-source"
          >{{ t('network.colorBySource') }}</button>
          <button
            class="gm-btn"
            :class="{ on: globeColorMode === 'speed' }"
            @click="globeColorMode = 'speed'"
            data-testid="globe-mode-speed"
          >{{ t('network.colorBySpeed') }}</button>
        </div>
        <div v-if="globeColorMode === 'speed'" class="globe-legend" data-testid="network-globe-legend-speed">
          <span class="gl-item"><i class="gl-dot" style="background:#22c55e" />{{ t('network.fast') }}</span>
          <span class="gl-item"><i class="gl-dot" style="background:#00e5ff" />{{ t('network.medium') }}</span>
          <span class="gl-item"><i class="gl-dot" style="background:#5b7aa8" />{{ t('network.slow') }}</span>
          <span class="gl-item"><i class="gl-dot" style="background:#ff8a3d" />{{ t('network.errors') }}</span>
          <span class="gl-item"><i class="gl-dot" style="background:#3a4656" />{{ t('network.noData') }}</span>
        </div>
        <div v-else class="globe-legend" data-testid="network-globe-legend">
          <span class="gl-item"><i class="gl-dot" :style="{ background: SOURCE_META.direct.color }" />{{ SOURCE_META.direct.label }}</span>
          <span class="gl-item"><i class="gl-dot" :style="{ background: SOURCE_META.client.color }" />{{ SOURCE_META.client.label }}</span>
          <span class="gl-item"><i class="gl-dot" :style="{ background: SOURCE_META.relay.color }" />{{ SOURCE_META.relay.label }}</span>
          <span class="gl-item"><i class="gl-dot" :style="{ background: SOURCE_META.mdns.color }" />{{ SOURCE_META.mdns.label }}</span>
          <span class="gl-item"><i class="gl-dot" :style="{ background: SOURCE_META.wan.color }" />{{ SOURCE_META.wan.label }}</span>
          <span class="gl-item"><i class="gl-dot" :style="{ background: SOURCE_META.dht.color }" />{{ SOURCE_META.dht.label }}</span>
          <span
            v-if="seederNodeCount > 0"
            class="gl-item"
            :title="t('network.seederHint')"
            data-testid="network-globe-seeder"
          ><i class="gl-dot" :style="{ background: SOURCE_META.seeder.color }" />{{ SOURCE_META.seeder.label }} · {{ seederNodeCount }}</span>
        </div>
      </aside>
      </div>

      <!-- empty -->
      <div v-else class="empty panel" data-testid="network-empty">
        <span class="empty-mark">[ 0x00 ]</span>
        <span class="dim">{{ t('network.none') }}</span>
      </div>
    </template>
  </div>
</template>

<style scoped>
.netmap {
  width: 100%;
  display: flex;
  flex-direction: column;
  gap: 18px;
}
.dht-spark {
  margin-top: 6px;
}
.head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}
.auto-upd { display: inline-flex; align-items: center; gap: 8px; font-size: 10px; letter-spacing: 0.16em; text-transform: uppercase; }
.auto-dot { width: 7px; height: 7px; border-radius: 50%; background: #3a4656; display: inline-block; transition: background-color 0.3s; }
.auto-upd.live .auto-dot { background: var(--accent); box-shadow: 0 0 10px var(--accent); animation: auto-pulse 0.9s ease-in-out infinite; }
@keyframes auto-pulse { 0%, 100% { opacity: 0.35; } 50% { opacity: 1; } }
.board-tabs { display: flex; align-items: center; gap: 4px; padding: 8px 10px 0; border-bottom: 1px solid var(--border); }
.bt { background: transparent; border: 1px solid transparent; border-bottom: none; color: var(--text-dim); font: inherit; font-size: 11px; letter-spacing: 0.12em; text-transform: uppercase; font-weight: var(--fw-800); padding: 8px 14px; cursor: pointer; transition: color 0.15s, background-color 0.15s; }
.bt:hover { color: var(--text); }
.bt.on { color: var(--accent); border-color: var(--border); background: rgba(23, 233, 199, 0.06); box-shadow: inset 0 -2px 0 var(--accent); }
.bt-n { margin-left: 6px; font-weight: var(--fw-600); opacity: 0.7; }
.bt-hint { margin-left: auto; font-size: 10px; max-width: 55%; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.page-title {
  font-size: 15px;
  letter-spacing: 0.14em;
  color: var(--accent);
  font-weight: var(--fw-700);
}
.hint {
  font-size: 12px;
  line-height: 1.6;
}
.stats {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
  gap: 12px;
}
.stat {
  padding: 14px 16px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.stat-k {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.stat-v {
  font-size: 22px;
  font-weight: var(--fw-700);
  color: var(--text);
}
.stat-v.accent {
  color: var(--accent);
}
/* personal node economy card */
.node-card {
  padding: 16px 18px;
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.nc-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.nc-title {
  font-size: 11px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.rank-badge {
  font-size: 11px;
  font-weight: var(--fw-700);
  letter-spacing: 0.16em;
  padding: 4px 12px;
  border-radius: 999px;
  color: var(--rank-color);
  border: 1px solid color-mix(in srgb, var(--rank-color) 55%, transparent);
  background: color-mix(in srgb, var(--rank-color) 12%, transparent);
  box-shadow: 0 0 12px color-mix(in srgb, var(--rank-color) 30%, transparent);
}
.nc-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
  gap: 12px;
}
.nc-metric {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.nc-k {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.nc-v {
  font-size: 22px;
  font-weight: var(--fw-700);
  color: var(--text);
}
.nc-v small {
  font-size: 12px;
  font-weight: var(--fw-400);
  color: var(--text-dim);
}
.nc-v.accent {
  color: var(--accent);
}
.nc-progress {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.ncp-labels {
  display: flex;
  justify-content: space-between;
  font-size: 11px;
}
.ncp-track {
  height: 6px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--text-dim) 22%, transparent);
  overflow: hidden;
}
.ncp-fill {
  height: 100%;
  border-radius: 999px;
  background: var(--rank-color);
  box-shadow: 0 0 10px color-mix(in srgb, var(--rank-color) 55%, transparent);
  transition: width 0.6s ease;
}
.seed-awards {
  margin-top: 10px;
  padding-top: 9px;
  border-top: 1px solid var(--border);
}
.sa-head {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px 12px;
  margin-bottom: 7px;
}
.sa-title {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--secondary);
  font-weight: var(--fw-700);
}
.rt-strip {
  display: flex;
  align-items: center;
  gap: 5px;
  flex-wrap: wrap;
}
.rt-pill {
  font-size: 9px;
  font-weight: var(--fw-700);
  letter-spacing: 0.05em;
  padding: 2px 8px;
  border-radius: 999px;
  border: 1px solid var(--border);
  color: var(--text-dim);
  opacity: 0.55;
  transition: opacity 0.2s ease;
}
.rt-pill.reached {
  opacity: 1;
  color: var(--t-color);
  border-color: var(--t-color);
  background: color-mix(in srgb, var(--t-color) 12%, transparent);
}
.sa-rep {
  margin-left: auto;
  font-size: 10px;
  font-weight: var(--fw-800);
  color: #34d399;
  background: rgba(52, 211, 153, 0.1);
  border: 1px solid rgba(52, 211, 153, 0.3);
  border-radius: 999px;
  padding: 2px 9px;
}
.sa-rows {
  display: flex;
  flex-direction: column;
  gap: 5px;
}
.sa-row {
  display: flex;
  align-items: center;
  gap: 9px;
  font-size: 11px;
  opacity: 0.72;
  transition: opacity 0.2s ease;
}
.sa-row.earned {
  opacity: 1;
}
.sa-icon {
  color: var(--a-color);
  font-size: 12px;
  width: 14px;
  text-align: center;
}
.sa-name {
  font-weight: var(--fw-800);
  letter-spacing: 0.05em;
  color: var(--a-color);
  width: 82px;
  flex: none;
}
.sa-req {
  font-size: 10px;
  white-space: nowrap;
  flex: none;
}
.sa-rep-val {
  color: #34d399;
}
.sa-track {
  flex: 1;
  height: 4px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--text-dim) 22%, transparent);
  overflow: hidden;
}
.sa-fill {
  height: 100%;
  border-radius: 999px;
  background: var(--a-color);
  box-shadow: 0 0 8px color-mix(in srgb, var(--a-color) 55%, transparent);
  transition: width 0.6s ease;
}
.sa-pct {
  font-size: 10px;
  color: var(--text-dim);
  font-variant-numeric: tabular-nums;
  width: 34px;
  text-align: right;
  flex: none;
}
.sa-pct.done {
  color: #34d399;
  font-weight: var(--fw-800);
}

.board {
  padding: 6px 4px;
  overflow-x: auto;
}
/* table 65% / globe 35% split; globe drops below the table on narrow screens */
.map-split {
  display: grid;
  grid-template-columns: minmax(0, 2.1fr) minmax(0, 1fr);
  gap: 14px;
  align-items: start;
}
.map-split .board {
  min-width: 0;
}
.globe-col {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 10px;
  position: sticky;
  top: 8px;
}
.globe-host {
  width: 100%;
  /* Прямоугольный кадр 3:2 (шире, чем выше); планета вписывается целиком с
     небольшим отступом и масштабируется под размер окна. Зум — колесом мыши. */
  aspect-ratio: 3 / 2;
  border: 1px solid var(--border);
  background: radial-gradient(circle at 50% 45%, #141a22 0%, #0a0c0f 70%);
}
.globe-legend {
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  gap: 12px;
}
.globe-mode {
  display: flex;
  justify-content: center;
  gap: 6px;
}
.gm-btn {
  background: rgba(255, 255, 255, 0.04);
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--text-dim);
  padding: 4px 12px;
  font-size: 10px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  cursor: pointer;
  transition: color 0.15s ease, background-color 0.15s ease, border-color 0.15s ease;
}
.gm-btn:hover {
  color: var(--text);
}
.gm-btn.on {
  color: #06121e;
  background: var(--accent);
  border-color: transparent;
  font-weight: var(--fw-700);
}
.gl-item {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 10px;
  letter-spacing: 0.05em;
  color: var(--text-dim);
}
.gl-dot {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  display: inline-block;
}
@media (max-width: 1080px) {
  .map-split {
    grid-template-columns: 1fr;
  }
  .globe-col {
    position: static;
  }
}
.c-src {
  width: 92px;
}
.src-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 3px 8px;
  border: 1px solid;
  border-radius: 999px;
  font-size: 9.5px;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
  white-space: nowrap;
}
.src-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  display: inline-block;
}
.dht-via {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  margin-top: 4px;
  font-size: 9.5px;
  font-weight: var(--fw-700);
  letter-spacing: 0.05em;
  text-transform: uppercase;
}
.dht-via.smartnet {
  color: #34d399;
}
.dht-via.public {
  color: #64748b;
}
.via-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  display: inline-block;
  background: currentColor;
}
.lb {
  width: 100%;
  table-layout: fixed;
  border-collapse: collapse;
}
.lb tbody tr.me { cursor: pointer; background: rgba(56, 189, 248, 0.06); box-shadow: inset 3px 0 0 #38bdf8; }
.lb tbody tr.me:hover { background: rgba(56, 189, 248, 0.12); }
.lb tbody tr.me.local { box-shadow: inset 3px 0 0 #f59e0b; background: rgba(245, 158, 11, 0.06); }
.lb tbody tr.me.local:hover { background: rgba(245, 158, 11, 0.12); }
.me-tag { margin-left: 8px; font-size: 9px; letter-spacing: 0.14em; padding: 1px 6px; border: 1px solid #38bdf8; color: #38bdf8; font-weight: var(--fw-800); }
tr.local .me-tag { border-color: #f59e0b; color: #f59e0b; }
.me-reason { font-size: 10px; color: #f59e0b; white-space: normal; line-height: 1.3; display: inline-block; max-width: 220px; }
.seed-modal-back { position: fixed; inset: 0; background: rgba(8, 8, 10, 0.65); backdrop-filter: blur(8px); -webkit-backdrop-filter: blur(8px); display: flex; align-items: center; justify-content: center; z-index: 200; padding: 24px; }
.seed-modal { width: min(920px, 100%); max-height: 90vh; overflow: auto; background: var(--bg, #0d0f12); border: 1px solid var(--border-accent, #1e9bf0); box-shadow: 0 24px 80px rgba(0, 0, 0, 0.6); }
.seed-modal-head { display: flex; align-items: center; justify-content: space-between; padding: 14px 18px; border-bottom: 1px solid var(--border); }
.seed-modal-title { margin: 0; font-size: 13px; letter-spacing: 0.12em; color: var(--accent); font-weight: var(--fw-800); }
.seed-modal-x { background: transparent; border: none; color: var(--text-dim); font-size: 14px; cursor: pointer; }
.seed-modal-x:hover { color: var(--text); }
.lb td {
  padding: 10px 12px;
  text-align: left;
  font-size: 12px;
  border-bottom: 1px solid var(--border);
}
/* заголовки таблицы (v1.135.200): без «полосок» (раньше th случайно получал стиль tr.me),
   читаемые — акцентный цвет, жирный трекинг, тонкая подсветка снизу */
.lb thead th {
  padding: 12px 12px 10px;
  text-align: left;
  font-size: 10px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  font-weight: var(--fw-800);
  color: var(--accent);
  background: rgba(23, 233, 199, 0.04);
  border-bottom: 1px solid rgba(23, 233, 199, 0.28);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.lb thead th.c-num,
.lb thead th.c-mine {
  text-align: right;
}
.lb thead th.c-rank {
  color: var(--text-dim);
}
.lb tr:last-child td {
  border-bottom: none;
}
.c-rank {
  width: 40px;
  color: var(--text-dim);
  font-weight: var(--fw-700);
}
.c-rank.top {
  color: var(--accent);
}
.c-num {
  width: 74px;
  text-align: right;
  font-variant-numeric: tabular-nums;
}
.c-node .node-id {
  color: var(--accent);
  word-break: break-all;
}
.c-node .node-id.named {
  font-weight: var(--fw-700);
}
.stat-v.split {
  display: inline-flex;
  align-items: baseline;
  gap: 4px;
}
.stat-v.split .srv {
  color: var(--accent);
}
.stat-v.split .cli {
  color: #34d399;
}
.stat-v.split .sep {
  color: var(--text-dim);
  font-weight: 400;
}
.c-node .node-addr {
  display: block;
  margin-top: 2px;
  font-size: 10.5px;
  color: var(--text-dim);
  letter-spacing: 0.02em;
}
.c-type {
  width: 104px;
}
.kind-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 3px 9px;
  border: 1px solid var(--border);
  border-radius: 999px;
  font-size: 10px;
  letter-spacing: 0.04em;
}
.kind-chip.client {
  color: #a5b4fc;
  border-color: #3730a3;
}
.kind-chip.headless {
  color: var(--accent);
  border-color: var(--border-accent);
}
/* Cloud Seeder rows — визуально отделены от P2P-пиров: отдельная секция с
   заголовком, приглушённые «—» в числовых колонках, фиолетовый акцент. */
.cs-sep td {
  padding: 12px 12px 6px;
  border-bottom: 1px dashed color-mix(in srgb, #a855f7 45%, transparent);
  background: color-mix(in srgb, #a855f7 6%, transparent);
}
.cs-sep-wrap {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.cs-sep-mark {
  font-size: 10px;
  font-weight: var(--fw-800);
  letter-spacing: 0.14em;
  color: #a855f7;
}
.cs-sep-count {
  font-size: 10px;
  font-variant-numeric: tabular-nums;
}
.cs-sep-hint {
  font-size: 10px;
  letter-spacing: 0.03em;
  flex: 1 1 240px;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.cs-row td {
  background: color-mix(in srgb, #a855f7 3%, transparent);
}
.cs-id {
  font-family: var(--font-mono, ui-monospace, monospace);
  font-size: 11px;
  letter-spacing: 0.02em;
}
.kind-chip.cs-kind {
  color: #a855f7;
  border-color: color-mix(in srgb, #a855f7 55%, transparent);
  background: color-mix(in srgb, #a855f7 10%, transparent);
}
/* бейдж провайдера облака (YA / GD / MAIL) — различает дубли pubkey автора.
   Стоит В ОДНОЙ строке ПЕРЕД коротким pubkey (ячейка c-node), не переносится */
.cs-prov {
  display: inline-flex;
  align-items: center;
  flex: none;
  white-space: nowrap;
  margin-right: 6px;
  padding: 1px 5px;
  border-radius: 8px;
  font-size: 9px;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
  color: #38bdf8;
  border: 1px solid color-mix(in srgb, #38bdf8 45%, transparent);
  background: color-mix(in srgb, #38bdf8 10%, transparent);
}
/* крохотный счётчик «спасённых» установок за сессию рядом с бейджем CLOUD */
.cs-rescue {
  display: inline-flex;
  align-items: center;
  margin-left: 5px;
  padding: 1px 5px;
  border-radius: 8px;
  font-size: 9px;
  font-weight: var(--fw-700);
  letter-spacing: 0.04em;
  color: var(--success, #34d399);
  border: 1px solid color-mix(in srgb, var(--success, #34d399) 45%, transparent);
  background: color-mix(in srgb, var(--success, #34d399) 10%, transparent);
  cursor: help;
}
.kind-label {
  text-transform: uppercase;
  font-weight: var(--fw-600);
}
.score {
  color: var(--success);
  font-weight: var(--fw-700);
}
/* personal (bandit-measured) speed column */
.c-mine {
  width: 96px;
  text-align: right;
}
.mine-wrap {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  justify-content: flex-end;
}
.mine-bar {
  width: 46px;
  height: 7px;
  border-radius: 4px;
  background: rgba(255, 255, 255, 0.08);
  overflow: hidden;
  flex: 0 0 auto;
}
.mine-fill {
  display: block;
  height: 100%;
  border-radius: 4px;
  background: linear-gradient(90deg, #00e5ff, #22c55e);
  transition: width 0.3s ease;
}
.mine-fill.bad {
  background: linear-gradient(90deg, #ff8a3d, #ff6b6b);
}
.mine-ok {
  font-size: 10px;
  color: var(--success);
  font-variant-numeric: tabular-nums;
}
.mine-none {
  font-size: 12px;
}
.badge {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  margin-right: 8px;
}
.badge.on {
  background: var(--success);
  box-shadow: 0 0 6px var(--success);
}
.badge.off {
  background: var(--danger);
}
.empty {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 18px;
  font-size: 12px;
}
.empty-mark {
  color: var(--secondary);
  letter-spacing: 0.1em;
}
.overflow-note {
  padding: 12px 14px 4px;
  font-size: 11.5px;
  text-align: center;
  letter-spacing: 0.04em;
}
button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
