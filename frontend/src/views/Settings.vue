<script setup lang="ts">
import { ref, onBeforeUnmount, watch, computed, nextTick } from 'vue'
import { builtinP2pNodes } from '@/lib/sthNodes'
import { onMounted } from 'vue'
import { useRoute } from 'vue-router'
import { useI18n } from 'vue-i18n'
import QRCode from 'qrcode'
import { encodeSeedQr, randomPin } from '@/lib/seedqr'
import { useUiStore, type ThemeId, type LocaleId, type FontId, type DensityId, type FontSizeId } from '@/store/ui'
import { useAuthStore } from '@/store/auth'
import { getDataDir, setDataDir, pickFolder, getTrackers, setTrackers, getSeeders, setSeeders, getSeedersAuto, getRelayOnly, setRelayOnly, getMobileFallbacks, setMobileFallbacks, getSwarmEnabled, setSwarmEnabled, swarmTopSeeders, getDhtProvidersEnabled, setDhtProvidersEnabled, dhtProvidersSummary, getDhtEnabled, setDhtEnabled, getRelayConfig, setRelayConfig, getAutoUpdate, setAutoUpdate, storageUsage, setStorageLimit, blobStoreUsage, optimizeBlobStore, dedupStats, appListen, saveFile, refreshNodePool, autoSelectNode, selectNode, currentNode, listCustomNodes, addCustomNode, removeCustomNode, pingCustomNode, translationCacheStats, clearTranslationCache, translatorEngineLabel, netDoctorRun, netDoctorCheckRelays, discoveryProbe, type DiscoveryReport, aiModelsCacheStats, aiModelsCacheClear, type StorageUsage, type DedupStats, type NodePing, type CustomPing, type TranslationCacheStats, type SwarmSeeder, type DhtProvidersSummary, type NetDoctorCheck, type MobileFallbacksMode } from '@/lib/bridge'
import { useDiscoveryStore } from '@/store/discovery'
import { officialLists, officialRefresh, openCrashLog, EMPTY_WEBPROXY_STATUS, p2pStatus, type OfficialLists, type WebproxyStatus, type MyceliumInfo, webproxyStatus } from '@/lib/bridge'
import { COIN_TYPE, WIF, CURVE, SUB_UNIT } from '@/lib/crypto'
import StorageQuotaModal from '@/components/StorageQuotaModal.vue'
import SeedingCard from '@/components/SeedingCard.vue'
import PqWebCard from '@/components/PqWebCard.vue'
import MyceliumCard from '@/components/MyceliumCard.vue'
import ConnStatsCard from '@/components/ConnStatsCard.vue'
import { linkTarget, setLinkTarget, type LinkTarget } from '@/lib/clearnet'
import KeysView from '@/views/Keys.vue'
import { shortAddr } from '@/lib/crypto'
import { useUpdater } from '@/lib/updater'
import { getClientVersion, getMyUsername, autostartGet, autostartSet, type AutostartSettings } from '@/lib/bridge'

const { t, locale } = useI18n()
const ui = useUiStore()
const auth = useAuthStore()
const confirming = ref(false)
const confirmingWipe = ref(false)

// P2P-обновления: ручная проверка.
const updater = useUpdater()
const clientVersion = ref('')
const checkingUpdate = ref(false)
const updateMsg = ref('')
const crashLogMsg = ref('')
async function openCrashLogClick() {
  crashLogMsg.value = ''
  try {
    await openCrashLog()
  } catch (e: any) {
    const code = typeof e === 'string' ? e : e?.message || ''
    crashLogMsg.value = code.includes('crash-log-empty') ? t('updates.crashLogEmpty') : code
  }
}

// Official Repository: списки доверенных разработчиков/dApps + ручной рескан
const official = ref<OfficialLists | null>(null)
const officialBusy = ref(false)
const officialMsg = ref('')
async function loadOfficialInfo() {
  try {
    official.value = await officialLists()
  } catch { /* web-превью: нативный API недоступен */ }
}
async function rescanOfficial() {
  if (officialBusy.value) return
  officialBusy.value = true
  officialMsg.value = ''
  try {
    official.value = await officialRefresh()
    officialMsg.value = t('settings.officialDone')
    // сразу переразмечаем каталог «Найти приложения»
    void useDiscoveryStore().loadOfficial()
  } catch (e) {
    officialMsg.value = String(e)
  } finally {
    officialBusy.value = false
  }
}
onMounted(loadOfficialInfo)
const officialScanLabel = computed(() => {
  const ts = official.value?.lastScan || 0
  return ts ? new Date(ts * 1000).toLocaleString() : '—'
})

onMounted(async () => {
  try {
    clientVersion.value = await getClientVersion()
  } catch {
    /* web fallback */
  }
})
async function manualUpdateCheck() {
  if (checkingUpdate.value) return
  checkingUpdate.value = true
  updateMsg.value = ''
  try {
    await updater.checkNow()
    const s = updater.state
    if (s.phase === 'downloading' || s.phase === 'ready') {
      updateMsg.value = t('updates.foundVersion', { version: s.info?.version ?? '' })
    } else if (s.error) {
      updateMsg.value = t('updates.checkError', { error: s.error })
    } else {
      updateMsg.value = t('updates.upToDate')
    }
  } finally {
    checkingUpdate.value = false
  }
}

type SettingsTab = 'earn' | 'interface' | 'keys' | 'storage' | 'network' | 'danger'
// «Сеть» — первая вкладка и вкладка по умолчанию (в веб-сборке её нет — тогда «Интерфейс»)
const tab = ref<SettingsTab>(auth.isTauri ? 'network' : 'interface')
const tabs: { id: SettingsTab; label: string }[] = [
  { id: 'network', label: 'tabNetwork' },
  { id: 'interface', label: 'tabInterface' },
  { id: 'earn', label: 'tabEarn' },
  { id: 'keys', label: 'tabKeys' },
  { id: 'storage', label: 'tabStorage' },
  { id: 'danger', label: 'tabDanger' },
]

/* ---- Вкладка «Интерфейс»: слева меню подразделов, справа выбранный блок (как в «Сеть») ---- */
type IfaceSection = 'language' | 'appearance' | 'font' | 'browser' | 'network' | 'official' | 'autostart'
const IFACE_SECTION_KEY = 'sth_settings_iface_section'
const ifaceSections: IfaceSection[] = ['language', 'appearance', 'font', 'browser', 'network', 'official', 'autostart']
// Browser: где открывать сторонние сайты (общий localStorage — окно мессенджера видит тот же выбор)
const linkTargetSel = ref<LinkTarget>(linkTarget())
function onLinkTarget() {
  setLinkTarget(linkTargetSel.value)
}
const ifaceSection = ref<IfaceSection>((() => {
  try {
    const v = localStorage.getItem(IFACE_SECTION_KEY) as IfaceSection | null
    return v && ifaceSections.includes(v) ? v : 'language'
  } catch {
    return 'language'
  }
})())
function goIface(id: IfaceSection) {
  ifaceSection.value = id
  try { localStorage.setItem(IFACE_SECTION_KEY, id) } catch { /* noop */ }
}
// Автозагрузка: крестик → трей, запуск при входе в систему (свёрнутым в трей)
const autoSt = ref<AutostartSettings | null>(null)
const autoBusy = ref(false)
const autoErr = ref('')
void autostartGet().then((v) => (autoSt.value = v))
async function setAutostart(patch: Partial<AutostartSettings>) {
  if (!autoSt.value || autoBusy.value) return
  autoBusy.value = true
  autoErr.value = ''
  try {
    autoSt.value = await autostartSet({ ...autoSt.value, ...patch })
  } catch (e) {
    autoErr.value = String(e)
  } finally {
    autoBusy.value = false
  }
}

const themes: { id: ThemeId; label: string }[] = [
  { id: 'default-dark', label: 'defaultDark' },
  { id: 'default-light', label: 'defaultLight' },
  { id: 'focus', label: 'focus' },
  { id: 'matte-dark', label: 'matteDark' },
  { id: 'readable-light', label: 'readableLight' },
  { id: 'eink', label: 'eink' },
  { id: 'neon', label: 'neon' },
]

const langs: { id: LocaleId; label: string }[] = [
  { id: 'ru', label: 'РУССКИЙ' },
  { id: 'en', label: 'ENGLISH' },
  { id: 'id', label: 'INDONESIA' },
]

const fonts: { id: FontId; label: string; stack: string }[] = [
  { id: 'jetbrains', label: 'JetBrains Mono', stack: "'JetBrains Mono', monospace" },
  { id: 'plex', label: 'IBM Plex Mono', stack: "'IBM Plex Mono', monospace" },
  { id: 'fira', label: 'Fira Code', stack: "'Fira Code', monospace" },
  { id: 'inter', label: 'Inter', stack: "'Inter', system-ui, sans-serif" },
  { id: 'tenor', label: 'Tenor Sans', stack: "'Tenor Sans', sans-serif" },
  { id: 'saira', label: 'Saira', stack: "'Saira', sans-serif" },
]

const densities: { id: DensityId; label: string }[] = [
  { id: 'default', label: 'default' },
  { id: 'compact', label: 'compact' },
]

const SIZES: FontSizeId[] = ['s', 'm', 'l']
const sizeIndex = computed<number>({
  get: () => Math.max(0, SIZES.indexOf(ui.fontSize)),
  set: (v: number) => ui.setFontSize(SIZES[v] || 'm'),
})

const netRows = [
  { key: 'addressVersion', value: '63 (0x3F)' },
  { key: 'coinType', value: String(COIN_TYPE) },
  { key: 'wif', value: String(WIF) },
  { key: 'curve', value: CURVE },
  { key: 'subUnit', value: SUB_UNIT },
]

function pickLang(id: LocaleId) {
  ui.setLocale(id)
  locale.value = id
}

async function reset() {
  await auth.reset()
}

async function wipe() {
  await auth.wipeAll()
}

/* ---- Data folder (Tauri) ---- */
const dataDir = ref('')
const storage = ref<StorageUsage | null>(null)
const limitGb = ref<number>(8)
const transfer = ref<{ active: boolean; phase: string; moved: number; total: number; item: string }>({
  active: false,
  phase: '',
  moved: 0,
  total: 0,
  item: '',
})
const transferError = ref('')
const transferPct = computed(() =>
  transfer.value.total > 0 ? Math.min(100, Math.round((transfer.value.moved / transfer.value.total) * 100)) : 0,
)

const GB = 1024 * 1024 * 1024
function toGb(bytes: number): string {
  return (bytes / GB).toFixed(2)
}
function humanBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  if (n < GB) return `${(n / 1024 / 1024).toFixed(1)} MB`
  return `${(n / GB).toFixed(2)} GB`
}

// --- Кэш переводов (AI Translation Layer) ---
const trCache = ref<TranslationCacheStats | null>(null)
const clearingTrCache = ref(false)
const trEngine = ref('')
async function loadTrCache() {
  trCache.value = await translationCacheStats()
  trEngine.value = await translatorEngineLabel()
}
async function clearTrCache() {
  if (clearingTrCache.value) return
  clearingTrCache.value = true
  try {
    await clearTranslationCache()
    await loadTrCache()
  } finally {
    clearingTrCache.value = false
  }
}

// --- Кеш скачанных AI-моделей (app-data/ai-models-store) ---
const aiCacheBytes = ref<number | null>(null)
const clearingAiCache = ref(false)
async function loadAiCache() {
  aiCacheBytes.value = await aiModelsCacheStats()
}
async function clearAiCache() {
  if (clearingAiCache.value) return
  clearingAiCache.value = true
  try {
    await aiModelsCacheClear()
    await loadAiCache()
  } finally {
    clearingAiCache.value = false
  }
}
const usedPct = computed(() => {
  const s = storage.value
  if (!s || s.limit <= 0) return 0
  return Math.min(100, Math.round((s.dappsUsed / s.limit) * 100))
})

async function loadStorage() {
  const s = await storageUsage()
  storage.value = s
  if (s) limitGb.value = Math.max(8, Math.round(s.limit / GB))
}

const blobBytes = ref<number>(0)
const optimizing = ref(false)
const optimizedAt = ref(false)
const dedup = ref<DedupStats | null>(null)
async function loadBlobUsage() {
  blobBytes.value = await blobStoreUsage()
  dedup.value = await dedupStats()
}
async function optimizeBlobs() {
  if (optimizing.value) return
  optimizing.value = true
  optimizedAt.value = false
  try {
    blobBytes.value = await optimizeBlobStore()
    dedup.value = await dedupStats()
    optimizedAt.value = true
  } finally {
    optimizing.value = false
  }
}

onMounted(async () => {
  if (auth.isTauri) {
    dataDir.value = (await getDataDir()) || ''
    await loadStorage()
    await loadTrCache()
    await loadAiCache()
  }
  await loadBlobUsage()
})
async function changeDataDir() {
  const picked = await pickFolder()
  if (!picked) return
  // Subscribe to native transfer-progress events before kicking off the move.
  transfer.value = { active: true, phase: 'scanning', moved: 0, total: 0, item: '' }
  const un = await appListen<{ phase: string; movedBytes: number; totalBytes: number; currentItem: string }>(
    'data-transfer-progress',
    (p) => {
      transfer.value = {
        active: p.phase !== 'done',
        phase: p.phase,
        moved: p.movedBytes,
        total: p.totalBytes,
        item: p.currentItem,
      }
    },
  )
  try {
    const applied = await setDataDir(picked)
    if (applied) dataDir.value = applied
    await loadStorage()
  } catch (e: any) {
    transferError.value = typeof e === 'string' ? e : (e?.message || 'Не удалось перенести данные')
  } finally {
    un()
    transfer.value.active = false
    setTimeout(() => { transfer.value.phase = '' }, 2500)
  }
}
async function applyLimit() {
  const gb = Math.max(8, Math.floor(Number(limitGb.value) || 8))
  await setStorageLimit(gb * GB)
  await loadStorage()
}

/* ---- P2P LAN discovery (mDNS + iroh-blobs, native only) ---- */
const discovery = useDiscoveryStore()

async function scanNetwork() {
  await discovery.scan()
}

/* ---- SmartHoldem Node Pool (mainnet REST nodes, native only) ---- */
// Результаты пинга живут на уровне модуля: при каждом открытии настроек список не
// обнуляется и не перепингуется (один скан в Rust раз в 2 мин или по кнопке «Пинг»).
const pingCache: { pool: NodePing[]; results: Record<string, CustomPing>; at: number } = { pool: [], results: {}, at: 0 }
const PING_CACHE_TTL_MS = 120_000
const nodes = ref<NodePing[]>(pingCache.pool.filter((n) => !n.p2p))
const nodesLoading = ref(false)
const nodeBusy = ref('')
const activeHost = computed(() => nodes.value.find((n) => n.active)?.host || '')
const onlineCount = computed(() => nodes.value.filter((n) => n.online).length)

/** Пул из Rust = iroh-узлы sth-core (p2p) + HTTPS: iroh уходят во вкладку P2P с готовым пингом. */
function applyPool(pool: NodePing[]) {
  nodes.value = pool.filter((n) => !n.p2p)
  for (const n of pool.filter((n) => !!n.p2p)) {
    if (!p2pNodes.value.includes(n.host)) p2pNodes.value = [n.host, ...p2pNodes.value]
    pingResults.value[n.host] = { host: n.host, online: n.online, latencyMs: n.latencyMs, height: n.height, verified: n.online, p2p: true, error: n.error }
  }
  pingCache.pool = pool
  pingCache.results = { ...pingResults.value }
  pingCache.at = Date.now()
}
async function rescanNodes() {
  if (!auth.isTauri) return
  nodesLoading.value = true
  try {
    applyPool(await refreshNodePool())
  } finally {
    nodesLoading.value = false
  }
}
async function autoNode() {
  if (!auth.isTauri) return
  nodesLoading.value = true
  try {
    applyPool(await autoSelectNode())
  } finally {
    nodesLoading.value = false
  }
}
async function pickNode(n: NodePing) {
  if (!auth.isTauri || !n.online || n.active) return
  nodeBusy.value = n.host
  try {
    await selectNode(n.host)
    nodes.value = nodes.value.map((x) => ({ ...x, active: x.host === n.host }))
    activeNode.value = n.host
  } finally {
    nodeBusy.value = ''
  }
}
function nodePingClass(ms: number | null) {
  if (ms == null) return 'off'
  if (ms < 900) return 'fast'
  if (ms < 1800) return 'mid'
  return 'slow'
}

/* ---- Custom nodes (clearnet hostnames or api:// P2P addresses) ---- */
const customNodes = ref<string[]>([])
const activeNode = ref('')
const newNode = ref('')
const nodeError = ref('')
const addingNode = ref(false)

async function loadCustomNodes() {
  if (!auth.isTauri) return
  customNodes.value = await listCustomNodes()
  activeNode.value = await currentNode()
  // detectLocalNode() ОТКЛЮЧЁН (SHIP-44): работаем через iroh-пул sth-core.
}

/* ---- Local node autodetect (SmartHoldem on localhost:4003) ---- */
const LOCAL_NODE_CANDIDATES = ['http://127.0.0.1:4003', 'http://localhost:4003']
const localNode = ref<CustomPing | null>(null)
const localNodeBusy = ref(false)
/** Кнопка «использовать локальный узел»: узел отвечает и ещё не выбран. */
const showLocalNode = computed(
  () => !!localNode.value?.online && !LOCAL_NODE_CANDIDATES.includes(activeNode.value),
)
async function detectLocalNode() {
  if (!auth.isTauri) return
  for (const host of LOCAL_NODE_CANDIDATES) {
    try {
      const p = await pingCustomNode(host)
      if (p.online) {
        localNode.value = { ...p, host }
        return
      }
    } catch {
      /* узла нет — пробуем следующий адрес */
    }
  }
  localNode.value = null
}
async function useLocalNode() {
  const host = localNode.value?.host
  if (!host || !auth.isTauri) return
  localNodeBusy.value = true
  nodeError.value = ''
  try {
    if (!customNodes.value.includes(host)) customNodes.value = await addCustomNode(host)
    await selectNode(host)
    activeNode.value = host
    nodes.value = nodes.value.map((x) => ({ ...x, active: false }))
  } catch (e: any) {
    nodeError.value = String(e?.message || e)
  } finally {
    localNodeBusy.value = false
  }
}
async function addNode() {
  const host = newNode.value.trim()
  if (!host || !auth.isTauri) return
  addingNode.value = true
  nodeError.value = ''
  try {
    customNodes.value = await addCustomNode(host)
    newNode.value = ''
  } catch (e: any) {
    nodeError.value = String(e?.message || e)
  } finally {
    addingNode.value = false
  }
}
async function removeNode(host: string) {
  if (!auth.isTauri) return
  customNodes.value = await removeCustomNode(host)
  activeNode.value = await currentNode()
}
/** Узел проверен и не отвечает — серый, выбор запрещён (непроверенный считаем доступным). */
function isDown(host: string): boolean {
  const p = pingResults.value[host]
  return !!p && !p.online
}
async function pickCustomNode(host: string) {
  if (!auth.isTauri || activeNode.value === host || isDown(host)) return
  nodeBusy.value = host
  try {
    await selectNode(host)
    activeNode.value = host
    nodes.value = nodes.value.map((x) => ({ ...x, active: false }))
  } finally {
    nodeBusy.value = ''
  }
}
const pingResults = ref<Record<string, CustomPing>>({ ...pingCache.results })
const pingBusy = ref('')
async function checkNode(host: string) {
  if (!auth.isTauri) return
  pingBusy.value = host
  try {
    pingResults.value = { ...pingResults.value, [host]: await pingCustomNode(host) }
  } catch {
    pingResults.value = { ...pingResults.value, [host]: { host, online: false, latencyMs: null, height: null, verified: false, p2p: host.startsWith('api://') } }
  } finally {
    pingBusy.value = ''
    pingCache.results = { ...pingResults.value }
  }
}
/** Best (lowest) latency among online clearnet pool nodes — the speed baseline. */
const bestPoolLatency = computed(() => {
  const ls = nodes.value.filter((n) => n.online && n.latencyMs != null).map((n) => n.latencyMs as number)
  return ls.length ? Math.min(...ls) : null
})
/** P2P latency relative to the fastest clearnet pool node (privacy-vs-speed). */
function poolDelta(host: string): { slow: boolean; ratio: number } | null {
  const p = pingResults.value[host]
  const best = bestPoolLatency.value
  if (!p || !p.online || p.latencyMs == null || best == null || best <= 0) return null
  const ratio = Math.round((p.latencyMs / best) * 10) / 10
  return { slow: ratio >= 2, ratio }
}
let unNodeSwitch: (() => void) | null = null
onMounted(async () => {
  if (auth.isTauri) {
    // один скан пула (iroh + clearnet) в Rust; свежий кэш (<2 мин) показываем сразу без перепинга
    if (Date.now() - pingCache.at > PING_CACHE_TTL_MS) rescanNodes()
    loadCustomNodes()
    // автопереключение узла в Rust → подсветить новый активный без ручного rescan
    unNodeSwitch = await appListen<{ to: string }>('sth-node-switched', (p) => {
      if (!p?.to) return
      activeNode.value = p.to
      nodes.value = nodes.value.map((x) => ({ ...x, active: x.host === p.to }))
    })
  }
})
onBeforeUnmount(() => { if (unNodeSwitch) unNodeSwitch() })

/* ---- Встроенные P2P-узлы (lib/sthNodes.ts + env SMARTHOLDEM_NODES_P2P) ---- */
const p2pNodes = ref<string[]>(builtinP2pNodes())
const nodeTab = ref<'clear' | 'p2p'>('p2p')
// онлайн P2P-узлов (api://) по результатам пинга
const p2pOnlineCount = computed(() => p2pNodes.value.filter((h) => pingResults.value[h]?.online).length)

// Пинг/АВТО для обеих вкладок — один параллельный проход по всему пулу в Rust
// (`ping_all`: iroh + clearnet); отдельных 10 параллельных invoke на P2P-узлы больше нет —
// вместе с фоновым сканом они давали шторм соединений и «сбрасывали» результаты.
async function rescanP2p() {
  await rescanNodes()
  activeNode.value = await currentNode()
}
async function autoP2p() {
  await autoNode()
  activeNode.value = await currentNode()
}
function tabAuto() {
  nodeTab.value === 'p2p' ? autoP2p() : autoNode()
}
function tabRescan() {
  nodeTab.value === 'p2p' ? rescanP2p() : rescanNodes()
}
const tabLoading = computed(() => nodesLoading.value)

/* ---- Вкладка «Сеть» (v1.135.202): под-разделы без прокрутки + статус-строка ---- */
type NetSection = 'pqweb' | 'mycelium' | 'discovery' | 'relays' | 'seeding' | 'nodes' | 'diag'
const NET_SECTION_KEY = 'sth_settings_net_section'
const netSections: { id: NetSection; label: string }[] = [
  { id: 'pqweb', label: 'secPqweb' },
  { id: 'mycelium', label: 'secMycelium' },
  { id: 'nodes', label: 'secNodes' },
  { id: 'discovery', label: 'secDiscovery' },
  { id: 'relays', label: 'secRelays' },
  { id: 'seeding', label: 'secSeeding' },
  { id: 'diag', label: 'secDiag' },
]
const netSection = ref<NetSection>((() => {
  try {
    const v = localStorage.getItem(NET_SECTION_KEY) as NetSection | null
    return v && netSections.some((s) => s.id === v) ? v : 'pqweb'
  } catch {
    return 'pqweb'
  }
})())
function goNet(id: NetSection) {
  netSection.value = id
  try { localStorage.setItem(NET_SECTION_KEY, id) } catch { /* noop */ }
}
// Расширенные параметры (трекеры, сидеры, n1-реле, диагностика, ручные узлы pq-web) — только DEV/GEEK.
const advanced = computed(() => ui.mode !== 'user')
// «Диагностика» целиком расширенная: при переходе в USER уводим на PQ Web.
watch(advanced, (adv) => {
  if (!adv && netSection.value === 'diag') goNet('pqweb')
}, { immediate: true })
// Статус-класс пилюли версии в шапке: checking / found / error / ok.
const updStatusClass = computed(() => {
  if (checkingUpdate.value) return 'checking'
  const ph = updater.state.phase
  if (ph === 'downloading' || ph === 'ready' || ph === 'installing') return 'found'
  if (updater.state.error) return 'error'
  return 'ok'
})
// Статус PQ Web приходит из PqWebCard (событие status) — для чипов и сводки.
const pqSt = ref<WebproxyStatus>(EMPTY_WEBPROXY_STATUS)
function onPqStatus(s: WebproxyStatus) {
  pqSt.value = s
}
const pqOkNodes = computed(() => pqSt.value.nodes.filter((n) => n.ok).length)
// Mycelium (NIP-1): статус приходит из MyceliumCard — для чипа и сводки раздела
const mySt = ref<MyceliumInfo | null>(null)
// Node ID этого устройства (iroh): чип в статус-строке, клик — копия в буфер
const myNodeId = ref('')
const myNodeCopied = ref(false)
void p2pStatus().then((s) => (myNodeId.value = s.nodeId))
async function copyMyNodeId() {
  if (!myNodeId.value) return
  try {
    await navigator.clipboard.writeText(myNodeId.value)
    myNodeCopied.value = true
    setTimeout(() => (myNodeCopied.value = false), 1300)
  } catch {
    /* noop */
  }
}
function onMyStatus(s: MyceliumInfo | null) {
  const changed = mySt.value?.settings?.enabled !== s?.settings?.enabled
  mySt.value = s
  // автопауза/возврат dVPN меняет pq-статус — перечитать для чипа и сводки
  if (changed) void webproxyStatus().then((p) => (pqSt.value = p))
}
const myLevel = computed(() => {
  const s = mySt.value
  if (!s?.settings?.enabled) return 'off'
  return (s.status?.circuitsReady ?? 0) > 0 ? 'on' : 'warn'
})
const trackersCount = computed(() => trackersText.value.split('\n').map((s) => s.trim()).filter(Boolean).length)
/** Короткая подпись активного узла кошелька: host пула или api://<8>… */
const netActiveLabel = computed(() => {
  if (activeHost.value) return activeHost.value
  const a = activeNode.value
  if (!a) return '—'
  if (a.startsWith('api://')) return 'api://' + a.slice(6, 14) + '…'
  return a.replace(/^https?:\/\//, '')
})
function ifaceSummary(id: IfaceSection): string {
  const on = t('settings.net.stOn'), off = t('settings.net.stOff')
  switch (id) {
    case 'language': return langs.find((l) => l.id === ui.locale)?.label ?? ui.locale
    case 'appearance': return t(`settings.${themes.find((x) => x.id === ui.theme)?.label ?? 'defaultDark'}`)
    case 'browser': return linkTargetSel.value === 'system' ? 'System Browser' : 'NETFORY Browser'
    case 'font': return `${fonts.find((f) => f.id === ui.font)?.label ?? ui.font} · ${ui.fontSize.toUpperCase()}`
    case 'network': return `ver 63 · coin ${COIN_TYPE}`
    case 'official': return official.value ? `${official.value.developers.length} devs · ${official.value.dapps.length} dApps` : '—'
    case 'autostart': return autoSt.value ? `${t('settings.autoTrayShort')} ${autoSt.value.closeToTray ? on : off} · ${t('settings.autoBootShort')} ${autoSt.value.launchAtLogin ? on : off}` : '—'
  }
}
function netSummary(id: NetSection): string {
  const on = t('settings.net.stOn'), off = t('settings.net.stOff')
  switch (id) {
    case 'pqweb': return `dVPN ${pqSt.value.total ? on : off} · ${pqOkNodes.value}/${pqSt.value.nodes.length}`
    case 'mycelium': return mySt.value?.settings?.enabled ? `${on} · ${mySt.value.status?.circuitsReady ?? 0} / ${mySt.value.status?.relaysKnown ?? 0}` : off
    case 'nodes': return `${netActiveLabel.value} · P2P ${p2pOnlineCount.value} / Clear ${onlineCount.value}`
    case 'discovery': return `DHT ${dhtEnabled.value ? on : off} · ${discovery.uniqueSeeds} LAN`
    case 'relays': return relayOnly.value ? t('settings.net.stRelayOnly') : `n1 ${n1RelayOn.value ? on : off} · ${t('peers.mobile_' + mobileFb.value)}`
    case 'seeding': return `swarm ${swarmEnabled.value ? on : off} · DHT ${dhtProviders.value ? on : off}`
    case 'diag': return clientVersion.value ? 'v' + clientVersion.value : '—'
  }
}

/* Deep-link support: /settings?tab=network&focus=custom-nodes (e.g. from the
   wallet transport badge) opens the right tab and scrolls to the block. */
const route = useRoute()
const focusCustom = ref(false)
// ?tab=… при уже открытых настройках (переход по ссылке) тоже переключает вкладку
watch(() => route.query.tab, (v) => {
  if (typeof v === 'string' && tabs.some((x) => x.id === v) && (v !== 'network' || auth.isTauri)) tab.value = v as SettingsTab
})
onMounted(async () => {
  const q = route.query
  if (typeof q.tab === 'string' && ['earn', 'interface', 'keys', 'storage', 'network', 'danger'].includes(q.tab) && (q.tab !== 'network' || auth.isTauri)) {
    tab.value = q.tab as SettingsTab
  }
  if (q.focus === 'custom-nodes') {
    goNet('nodes')
    await nextTick()
    const el = document.querySelector('[data-testid="custom-nodes"]')
    if (el) {
      el.scrollIntoView({ behavior: 'smooth', block: 'center' })
      focusCustom.value = true
      setTimeout(() => (focusCustom.value = false), 2200)
    }
  }
})

/* ---- WAN trackers + relay-only privacy (native only) ---- */
const trackersText = ref('')
const relayOnly = ref(false)
// Мобильные фолбэки (DoH + pkarr HTTPS-discovery): auto | on | off
const mobileFb = ref<MobileFallbacksMode>('auto')
const mobileFbFlash = ref('')
const mobileModes: MobileFallbacksMode[] = ['auto', 'on', 'off']
const swarmEnabled = ref(true)
const dhtProviders = ref(true)
const dhtEnabled = ref(true)
const autoUpdate = ref(true)
const trackerFlash = ref('')
const seedersText = ref('')
const seedersFlash = ref('')
const seedersAuto = ref<string[]>([])

onMounted(async () => {
  if (!auth.isTauri) return
  trackersText.value = (await getTrackers()).join('\n')
  seedersText.value = (await getSeeders()).join('\n')
  seedersAuto.value = await getSeedersAuto()
  relayOnly.value = await getRelayOnly()
  mobileFb.value = await getMobileFallbacks()
  swarmEnabled.value = await getSwarmEnabled()
  dhtProviders.value = await getDhtProvidersEnabled()
  dhtEnabled.value = await getDhtEnabled()
  autoUpdate.value = await getAutoUpdate()
  await loadSwarmSeeders()
  await loadDhtDiag()
  // Переход с Dashboard-индикатора: подсветить и прокрутить к тумблеру DHT.
  if (route.query.section === 'p2p') {
    goNet('seeding')
    await nextTick()
    const row = document.querySelector('[data-testid="dht-providers-row"]') as HTMLElement | null
    if (row) {
      row.scrollIntoView({ behavior: 'smooth', block: 'center' })
      row.classList.add('row-flash')
      setTimeout(() => row.classList.remove('row-flash'), 2400)
    }
  }
  const rc = await getRelayConfig()
  n1RelayOn.value = rc.enabled
  n1RelayText.value = rc.urls.join('\n')
})

async function saveTrackers() {
  const list = trackersText.value
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean)
  await setTrackers(list)
  trackersText.value = list.join('\n')
  trackerFlash.value = t('peers.trackersSaved')
  setTimeout(() => (trackerFlash.value = ''), 1800)
}

async function saveSeeders() {
  const list = seedersText.value
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean)
  await setSeeders(list)
  seedersText.value = (await getSeeders()).join('\n')
  seedersFlash.value = t('peers.seedersSaved')
  setTimeout(() => (seedersFlash.value = ''), 1800)
}

async function toggleRelay() {
  relayOnly.value = !relayOnly.value
  await setRelayOnly(relayOnly.value)
}

async function setMobileFbMode(mode: MobileFallbacksMode) {
  if (mobileFb.value === mode) return
  mobileFb.value = mode
  await setMobileFallbacks(mode)
  mobileFbFlash.value = t('peers.dhtRestart')
  setTimeout(() => (mobileFbFlash.value = ''), 3000)
}

async function toggleSwarm() {
  swarmEnabled.value = !swarmEnabled.value
  await setSwarmEnabled(swarmEnabled.value)
}

async function toggleDhtProviders() {
  dhtProviders.value = !dhtProviders.value
  await setDhtProvidersEnabled(dhtProviders.value)
  await loadDhtDiag()
}

// P1: диагностика трекерлесс-DHT (сколько узлов найдено/анонсируем).
/* ---- discovery probe (v1.135.210) ---- */
const discNode = ref('')
const discRunning = ref(false)
const discReport = ref<DiscoveryReport | null>(null)
watch(p2pNodes, (list) => { if (!discNode.value && list.length) discNode.value = list.includes(activeNode.value) ? activeNode.value : list[0] }, { immediate: true })
async function runDiscProbe() {
  if (!auth.isTauri || !discNode.value) return
  discRunning.value = true
  discReport.value = null
  try {
    discReport.value = await discoveryProbe(discNode.value)
  } catch (e) {
    discReport.value = { id: discNode.value, hits: [], errors: [String(e)], lookupMs: 0, connectMs: null, via: '' }
  } finally {
    discRunning.value = false
  }
}
function discSourceLabel(src: string): string {
  const map: Record<string, string> = { dns: 'DNS (n0)', pkarr: 'pkarr HTTPS', mdns: 'mDNS (LAN)', mainline: 'Mainline DHT', 'sled-peerbook': 'peerbook (sled)' }
  const k = Object.keys(map).find((k) => src.toLowerCase().includes(k))
  return k ? map[k] : src
}
const dhtDiag = ref<DhtProvidersSummary>({ enabled: false, seeders: 0, apps: 0, announcedApps: 0, beaconPort: null, dhtNodes: 0 })
const dhtDiagLoading = ref(false)
async function loadDhtDiag() {
  if (!auth.isTauri) return
  dhtDiagLoading.value = true
  try {
    dhtDiag.value = await dhtProvidersSummary()
  } finally {
    dhtDiagLoading.value = false
  }
}

const swarmSeeders = ref<SwarmSeeder[]>([])
const swarmMaxSpeed = computed(() =>
  swarmSeeders.value.reduce((m, s) => Math.max(m, s.speed), 0),
)
function swarmSeederShort(peer: string): string {
  return peer.length > 14 ? `${peer.slice(0, 8)}…${peer.slice(-4)}` : peer
}
async function loadSwarmSeeders() {
  swarmSeeders.value = await swarmTopSeeders()
}

const dhtFlash = ref('')
async function toggleDht() {
  dhtEnabled.value = !dhtEnabled.value
  await setDhtEnabled(dhtEnabled.value)
  dhtFlash.value = t('peers.dhtRestart')
  setTimeout(() => (dhtFlash.value = ''), 3000)
}

/* ---- «Сетевой доктор» (v1.63.13): диагностика мобильных сетей / DPI ---- */
const docRunning = ref(false)
const docSteps = ref<NetDoctorCheck[]>([])
const docCopyFlash = ref('')
function docTitle(id: string): string {
  return t('peers.doc_' + id)
}
async function runNetDoctor() {
  if (docRunning.value || !auth.isTauri) return
  docRunning.value = true
  docSteps.value = []
  let unlisten: (() => void) | null = null
  try {
    // Живой прогресс: шаги прилетают событиями по мере выполнения проб.
    unlisten = await appListen<NetDoctorCheck>('net-doctor-step', (c) => {
      const i = docSteps.value.findIndex((s) => s.id === c.id)
      if (i >= 0) docSteps.value[i] = c
      else docSteps.value.push(c)
    })
    docSteps.value = await netDoctorRun()
  } finally {
    docRunning.value = false
    if (unlisten) unlisten()
  }
}
/** v1.63.14: копирует plain-text отчёт диагностики в буфер обмена для передачи
 *  саппорту/оператору. Формат: одна строка на пробу — [STATUS] TITLE — detail. */
async function copyDoctorReport() {
  if (!docSteps.value.length) return
  const lines: string[] = []
  lines.push(`SmartNet Network Doctor — ${new Date().toISOString()}`)
  lines.push(`client v${clientVersion.value || ''}`.trim())
  lines.push('')
  for (const s of docSteps.value) {
    const tag = s.status === 'ok' ? 'OK  ' : s.status === 'warn' ? 'WARN' : s.status === 'fail' ? 'FAIL' : '... '
    lines.push(`[${tag}] ${docTitle(s.id).padEnd(18, ' ')} — ${s.detail}`)
  }
  const text = lines.join('\n')
  try {
    await navigator.clipboard.writeText(text)
    docCopyFlash.value = t('peers.docCopied')
  } catch {
    docCopyFlash.value = t('peers.docCopyFail')
  }
  setTimeout(() => (docCopyFlash.value = ''), 2200)
}

/* ---- SmartNet custom relays (n1-relay) ---- */
const n1RelayOn = ref(false)
const n1RelayText = ref('')
const n1RelayFlash = ref('')
async function saveN1Relays() {
  const urls = n1RelayText.value
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean)
  const rc = await setRelayConfig(n1RelayOn.value, urls)
  n1RelayOn.value = rc.enabled
  n1RelayText.value = rc.urls.join('\n')
  n1RelayFlash.value = t('peers.n1RelaySaved')
  setTimeout(() => (n1RelayFlash.value = ''), 2200)
}
async function toggleN1Relay() {
  n1RelayOn.value = !n1RelayOn.value
  await saveN1Relays()
}

/* v1.63.17: точечная диагностика n1-реле (TCP/TLS 443, dual-stack) — кнопка
 * «Проверить мои реле» рядом с полем ввода. Прогресс — по событиям
 * `net-doctor-step` (те же, что у полного Доктора). */
const n1RelayCheckRunning = ref(false)
const n1RelayCheckResults = ref<NetDoctorCheck[]>([])
async function checkN1Relays() {
  if (n1RelayCheckRunning.value || !auth.isTauri) return
  const urls = n1RelayText.value
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean)
  n1RelayCheckRunning.value = true
  n1RelayCheckResults.value = []
  let unlisten: (() => void) | null = null
  try {
    unlisten = await appListen<NetDoctorCheck>('net-doctor-step', (c) => {
      if (!c.id.startsWith('myrelay:')) return
      const i = n1RelayCheckResults.value.findIndex((s) => s.id === c.id)
      if (i >= 0) n1RelayCheckResults.value[i] = c
      else n1RelayCheckResults.value.push(c)
    })
    n1RelayCheckResults.value = await netDoctorCheckRelays(urls)
  } finally {
    n1RelayCheckRunning.value = false
    if (unlisten) unlisten()
  }
}

async function toggleAutoUpdate() {
  autoUpdate.value = !autoUpdate.value
  await setAutoUpdate(autoUpdate.value)
  discovery.autoUpdate = autoUpdate.value
}

onBeforeUnmount(() => {
  // keep P2P discovery running in the background (started in AppLayout) so
  // auto-update keeps working after leaving Settings — do not stop it here.
})

/* ---- Backup & Restore ---- */
const flash = ref('')
const qrOpen = ref(false)
const qrDataUrl = ref('')
const importInput = ref<HTMLInputElement | null>(null)

// Track the QR modal as a Vue overlay so AppLayout hides native child webviews
// behind it (z-order fix on the built desktop app).
watch(qrOpen, (open) => (open ? ui.openOverlay() : ui.closeOverlay()))

function notify(msg: string) {
  flash.value = msg
  setTimeout(() => (flash.value = ''), 1800)
}

async function exportVault() {
  const json = await auth.exportVaultJson()
  if (!json) return
  const saved = await saveFile(`smartnet-vault-${auth.address.slice(0, 8)}.json`, json, 'application/json')
  if (saved) notify(t('settings.exported'))
}

async function showQr() {
  const json = await auth.exportVaultJson()
  if (!json) return
  qrDataUrl.value = await QRCode.toDataURL(json, {
    errorCorrectionLevel: 'L',
    margin: 2,
    width: 360,
    color: { dark: '#0d0f12', light: '#ffffff' },
  })
  qrOpen.value = true
}

/* ---- QR для входа в standalone NETFORY Messenger (сид под PIN) ---- */
const msgQrOpen = ref(false)
const msgQrDataUrl = ref('')
const msgQrPin = ref('')
watch(msgQrOpen, (open) => (open ? ui.openOverlay() : ui.closeOverlay()))
async function showMessengerQr() {
  const seed = await auth.revealSeed()
  if (!seed) { notify(t('settings.msgQrLocked')); return }
  msgQrPin.value = randomPin()
  // имя u:// публично (на цепи) — кладём в QR открыто, чтобы на телефоне оно было известно сразу
  const name = await getMyUsername().catch(() => null)
  const payload = await encodeSeedQr(seed, msgQrPin.value, name || '')
  msgQrDataUrl.value = await QRCode.toDataURL(payload, { errorCorrectionLevel: 'M', margin: 2, width: 360, color: { dark: '#0d0f12', light: '#ffffff' } })
  msgQrOpen.value = true
}
function closeMessengerQr() {
  msgQrOpen.value = false
  msgQrDataUrl.value = ''
  msgQrPin.value = ''
}

function triggerImport() {
  importInput.value?.click()
}

async function onImportFile(e: Event) {
  const file = (e.target as HTMLInputElement).files?.[0]
  if (!file) return
  const text = await file.text()
  const ok = await auth.importVaultJson(text)
  if (!ok) notify(t('settings.importError'))
}
</script>

<template>
  <div class="settings" data-testid="settings-view">
    <!-- Шапка (v1.135.203): справа — глобальный тулбар обновлений/crash-лога, виден на любой вкладке -->
    <div class="page-head" data-testid="settings-head">
      <h1 class="page-title">// {{ t('settings.title') }}_</h1>
      <div v-if="auth.isTauri" class="hdr-tools" data-testid="settings-header-tools">
        <span class="ver-pill" :class="updStatusClass" :title="t('updates.hintSuffix')" data-testid="settings-version-pill">
          <i class="vdot" />v{{ clientVersion || '—' }}
        </span>
        <button
          class="btn btn-primary hdr-btn"
          :class="{ busy: checkingUpdate }"
          :disabled="checkingUpdate"
          @click="manualUpdateCheck"
          data-testid="check-update-btn"
        >
          <span class="hdr-ico" :class="{ spin: checkingUpdate }">↻</span>
          {{ checkingUpdate ? t('updates.checking') : t('updates.checkShort') }}
        </button>
        <button class="btn hdr-btn hdr-crash" @click="openCrashLogClick" data-testid="open-crash-log-btn">
          <span class="hdr-ico">⚠</span>{{ t('updates.crashShort') }}
        </button>
      </div>
    </div>
    <transition name="hdr-msg">
      <p v-if="updateMsg || crashLogMsg" class="hdr-msg" :class="updStatusClass" data-testid="settings-header-msg">
        <span v-if="updateMsg" data-testid="update-check-msg">{{ updateMsg }}</span>
        <span v-if="updateMsg && crashLogMsg" class="dim"> · </span>
        <span v-if="crashLogMsg" data-testid="crash-log-msg">{{ crashLogMsg }}</span>
        <button class="hdr-msg-close" @click="updateMsg = ''; crashLogMsg = ''" :title="t('common.close')" data-testid="settings-header-msg-close">✕</button>
      </p>
    </transition>

    <!-- section tabs (metallic headers) -->
    <div class="seg-tabs" data-testid="settings-tabs">
      <button
        v-for="tb in tabs"
        :key="tb.id"
        class="seg-btn"
        :class="{ active: tab === tb.id, danger: tb.id === 'danger' }"
        @click="tab = tb.id"
        :data-testid="`settings-tab-${tb.id}`"
      >{{ t(`settings.${tb.label}`) }}</button>
    </div>

    <div v-show="tab === 'earn'" data-testid="settings-pane-earn">
      <SeedingCard />
    </div>

    <!-- Keys pane: полноценный компонент из /keys, интегрированный в
         Settings как вкладка. USER/DEV режимы не показывают отдельный
         пункт «Ключи» в sidebar — путь один: Settings → Ключи. -->
    <div v-show="tab === 'keys'" data-testid="settings-pane-keys">
      <KeysView @show-messenger-qr="showMessengerQr" />
    </div>

    <!-- INTERFACE: слева меню подразделов, справа выбранный блок (как во вкладке «Сеть») -->
    <div v-show="tab === 'interface'" class="net-body iface-body" data-testid="settings-interface-grid">
    <nav class="net-nav" data-testid="iface-nav">
      <button
        v-for="id in ifaceSections"
        :key="id"
        class="net-nav-btn"
        :class="{ on: ifaceSection === id }"
        @click="goIface(id)"
        :data-testid="`iface-nav-${id}`"
      >
        <span class="nn-label">{{ id === 'official' ? 'Official Repository' : t(`settings.ifaceSec.${id}`) }}</span>
        <span class="nn-sub mono" :data-testid="`iface-nav-sub-${id}`">{{ ifaceSummary(id) }}</span>
      </button>
    </nav>
    <div class="net-pane">
    <!-- APPEARANCE -->
    <section v-show="ifaceSection === 'appearance'" class="panel block" data-testid="settings-appearance">
      <div class="block-head">
        <span class="block-title">{{ t('settings.appearance') }}</span>
        <button class="reset-btn" @click="ui.resetAppearance()" data-testid="reset-appearance-btn">
          ↺ {{ t('settings.resetAppearance') }}
        </button>
      </div>
      <div class="theme-grid">
        <button
          v-for="th in themes"
          :key="th.id"
          class="theme-btn"
          :class="{ active: ui.theme === th.id }"
          @click="ui.setTheme(th.id)"
          :data-testid="`theme-${th.id}`"
        >
          <span class="swatch" :class="th.id" />
          <span class="theme-name">{{ t(`settings.${th.label}`) }}</span>
          <span v-if="ui.theme === th.id" class="theme-check hl">●</span>
        </button>
      </div>
      <span class="block-title density-title">
        {{ t('settings.fontWeight') }} <span class="fw-val hl">{{ ui.fontWeight }}</span>
      </span>
      <div class="size-slider">
        <input
          type="range"
          min="100"
          max="600"
          step="100"
          :value="ui.fontWeight"
          @input="ui.setFontWeight(Number(($event.target as HTMLInputElement).value))"
          class="size-range"
          data-testid="font-weight-slider"
        />
        <div class="size-ticks">
          <span :class="{ on: ui.fontWeight === 100 }">100</span>
          <span :class="{ on: ui.fontWeight === 200 }">200</span>
          <span :class="{ on: ui.fontWeight === 300 }">300</span>
          <span :class="{ on: ui.fontWeight === 400 }">400</span>
          <span :class="{ on: ui.fontWeight === 500 }">500</span>
          <span :class="{ on: ui.fontWeight === 600 }">600</span>
        </div>
      </div>
      <div class="opt-row" data-testid="opt-noblur">
        <div class="opt-text">
          <span class="opt-label">{{ t('settings.disableBlur') }}</span>
          <span class="opt-hint dim">{{ t('settings.disableBlurHint') }}</span>
        </div>
        <button
          class="toggle"
          :class="{ on: ui.noBlur }"
          role="switch"
          :aria-checked="ui.noBlur"
          @click="ui.setNoBlur(!ui.noBlur)"
          data-testid="toggle-noblur"
        >
          <span class="toggle-knob"></span>
        </button>
      </div>
    </section>

    <!-- FONTS -->
    <section v-show="ifaceSection === 'font'" class="panel block" data-testid="settings-fonts">
      <span class="block-title">{{ t('settings.fonts') }}</span>
      <div class="font-grid">
        <button
          v-for="f in fonts"
          :key="f.id"
          class="font-btn"
          :class="{ active: ui.font === f.id }"
          :style="{ fontFamily: f.stack }"
          @click="ui.setFont(f.id)"
          :data-testid="`font-${f.id}`"
        >
          <span class="font-name">{{ f.label }}</span>
          <span class="font-sample">AaBb 0123 {}</span>
          <span v-if="ui.font === f.id" class="theme-check hl">●</span>
        </button>
      </div>
      <span class="block-title density-title">{{ t('settings.fontSize') }}</span>
      <div class="size-slider">
        <input
          type="range"
          min="0"
          max="2"
          step="1"
          v-model.number="sizeIndex"
          class="size-range"
          data-testid="font-size-slider"
        />
        <div class="size-ticks">
          <span :class="{ on: ui.fontSize === 's' }">S</span>
          <span :class="{ on: ui.fontSize === 'm' }">M</span>
          <span :class="{ on: ui.fontSize === 'l' }">L</span>
        </div>
      </div>
      <span class="block-title density-title">{{ t('settings.density') }}</span>
      <div class="lang-row">
        <button
          v-for="d in densities"
          :key="d.id"
          class="btn"
          :class="{ 'btn-primary': ui.density === d.id }"
          @click="ui.setDensity(d.id)"
          :data-testid="`density-${d.id}`"
        >
          {{ t(`settings.${d.label}`) }}
        </button>
      </div>
    </section>

    <!-- LANGUAGE -->
    <section v-show="ifaceSection === 'language'" class="panel block" data-testid="settings-language">
      <span class="block-title">{{ t('settings.language') }}</span>
      <div class="lang-row">
        <button
          v-for="l in langs"
          :key="l.id"
          class="btn"
          :class="{ 'btn-primary': ui.locale === l.id }"
          @click="pickLang(l.id)"
          :data-testid="`settings-lang-${l.id}`"
        >
          {{ l.label }}
        </button>
      </div>
    </section>

    <!-- NETWORK -->
    <section v-show="ifaceSection === 'network'" class="panel block" data-testid="settings-network">
      <span class="block-title">{{ t('settings.network') }}</span>
      <table class="net-table">
        <tbody>
          <tr v-for="row in netRows" :key="row.key" :data-testid="`net-${row.key}`">
            <td class="net-key mono-label">{{ t(`settings.${row.key}`) }}</td>
            <td class="net-val hl">{{ row.value }}</td>
          </tr>
        </tbody>
      </table>
    </section>

    <!-- OFFICIAL REPOSITORY -->
    <section v-show="ifaceSection === 'official'" class="panel block" data-testid="settings-official">
      <span class="block-title">Official Repository</span>
      <p class="dim official-hint">{{ t('settings.officialHint') }}</p>
      <div class="official-info">
        <span class="dim">{{ t('settings.officialDevs') }}: <b class="hl" data-testid="official-devs-count">{{ official ? official.developers.length : '—' }}</b></span>
        <span class="dim">{{ t('settings.officialDapps') }}: <b class="hl" data-testid="official-dapps-count">{{ official ? official.dapps.length : '—' }}</b></span>
        <span class="dim">{{ t('settings.officialLastScan') }}: <b data-testid="official-last-scan">{{ officialScanLabel }}</b></span>
      </div>
      <button
        class="btn btn-primary"
        :disabled="officialBusy || !auth.isTauri"
        @click="rescanOfficial"
        data-testid="official-rescan-btn"
      >
        {{ officialBusy ? '…' : t('settings.officialRescan') }}
      </button>
      <p v-if="officialMsg" class="dim official-msg" data-testid="official-rescan-msg">{{ officialMsg }}</p>
    </section>

    <!-- BROWSER: где открывать сторонние сайты (ссылки в мессенджере, dApp, страницах) -->
    <section v-show="ifaceSection === 'browser'" class="panel block" data-testid="settings-browser">
      <span class="block-title">BROWSER</span>
      <div class="opt-row browser-row">
        <div class="opt-text">
          <span class="opt-label">{{ t('settings.browserOpen') }}</span>
          <span class="opt-hint dim">{{ t('settings.browserOpenHint') }}</span>
        </div>
        <select v-model="linkTargetSel" class="disc-select mono browser-select" @change="onLinkTarget" data-testid="browser-link-target-select">
          <option value="netfory" data-testid="browser-opt-netfory">NETFORY Browser</option>
          <option value="system" data-testid="browser-opt-system">System Browser</option>
        </select>
      </div>
    </section>

    <!-- AUTOSTART: трей и запуск при входе в систему -->
    <section v-show="ifaceSection === 'autostart'" class="panel block" data-testid="settings-autostart">
      <span class="block-title">{{ t('settings.ifaceSec.autostart') }}</span>
      <p class="hint dim">{{ t('settings.autoHint') }}</p>
      <p v-if="!auth.isTauri" class="hint dim" data-testid="autostart-web-only">{{ t('settings.autoDesktopOnly') }}</p>
      <div class="opt-row" data-testid="opt-close-to-tray">
        <div class="opt-text">
          <span class="opt-label">{{ t('settings.autoTray') }}</span>
          <span class="opt-hint dim">{{ t('settings.autoTrayHint') }}</span>
        </div>
        <button class="toggle" :class="{ on: autoSt?.closeToTray }" role="switch" :aria-checked="!!autoSt?.closeToTray" :disabled="!autoSt || autoBusy" @click="setAutostart({ closeToTray: !autoSt?.closeToTray })" data-testid="toggle-close-to-tray">
          <span class="toggle-knob"></span>
        </button>
      </div>
      <div class="opt-row" data-testid="opt-launch-at-login">
        <div class="opt-text">
          <span class="opt-label">{{ t('settings.autoBoot') }}</span>
          <span class="opt-hint dim">{{ t('settings.autoBootHint') }}</span>
        </div>
        <button class="toggle" :class="{ on: autoSt?.launchAtLogin }" role="switch" :aria-checked="!!autoSt?.launchAtLogin" :disabled="!autoSt || autoBusy" @click="setAutostart({ launchAtLogin: !autoSt?.launchAtLogin })" data-testid="toggle-launch-at-login">
          <span class="toggle-knob"></span>
        </button>
      </div>
      <p v-if="autoErr" class="hint err-text" data-testid="autostart-error">{{ autoErr }}</p>
    </section>
    </div>
    </div>
    <!-- /INTERFACE -->

    <!-- STORAGE / DATA FOLDER (native only) -->
    <section v-show="tab === 'storage'" v-if="auth.isTauri" class="panel block" data-testid="settings-storage">
      <span class="block-title">{{ t('settings.storage') }}</span>
      <p class="hint dim">{{ t('settings.storageHint') }}</p>
      <div class="data-dir-row">
        <code class="data-dir" data-testid="data-dir-path">{{ dataDir || '—' }}</code>
        <button class="btn btn-primary" @click="changeDataDir" :disabled="transfer.active" data-testid="change-data-dir-btn">
          {{ transfer.active ? t('storage.transferring') : t('settings.changeFolder') }}
        </button>
      </div>

      <!-- data-directory transfer progress -->
      <div v-if="transfer.active || transfer.phase === 'done'" class="transfer-box" data-testid="data-transfer-progress">
        <div class="cap-line">
          <span class="mono-label">
            {{ transfer.phase === 'done' ? t('storage.transferDone') : t('storage.transferring') }}
          </span>
          <span class="hl" data-testid="transfer-pct">{{ transferPct }}%</span>
        </div>
        <div class="cap-bar">
          <div
            class="cap-fill transfer-fill"
            :class="{ done: transfer.phase === 'done' }"
            :style="{ width: transferPct + '%' }"
            data-testid="transfer-bar-fill"
          />
        </div>
        <p class="hint dim mono" data-testid="transfer-item">
          {{ toGb(transfer.moved) }} / {{ toGb(transfer.total) }} GB
          <template v-if="transfer.item"> · {{ transfer.item }}</template>
        </p>
      </div>
      <p v-if="transferError" class="hint danger" data-testid="transfer-error">{{ transferError }}</p>

      <!-- capacity bar + limit control -->
      <div v-if="storage" class="storage-cap" data-testid="storage-capacity">
        <div class="cap-line">
          <span class="mono-label">{{ t('storage.dappsUsed') }}</span>
          <span class="hl" data-testid="storage-used-of">
            {{ toGb(storage.dappsUsed) }} / {{ toGb(storage.limit) }} GB
          </span>
        </div>
        <div class="cap-bar" data-testid="storage-bar">
          <div
            class="cap-fill"
            :class="{ warn: usedPct >= 90 }"
            :style="{ width: usedPct + '%' }"
            data-testid="storage-bar-fill"
          />
        </div>
        <p class="hint dim">{{ t('storage.dataTotal') }}: {{ toGb(storage.dataTotal) }} GB</p>

        <div class="limit-row">
          <label class="mono-label">{{ t('storage.limitLabel') }}</label>
          <input
            v-model.number="limitGb"
            type="number"
            min="8"
            step="1"
            class="limit-input"
            data-testid="storage-limit-input"
          />
          <span class="dim unit">GB</span>
          <button class="btn btn-primary" @click="applyLimit" data-testid="storage-limit-apply">
            {{ t('storage.apply') }}
          </button>
        </div>
        <p class="hint dim">{{ t('storage.minNote') }}</p>

        <div class="blob-gc" data-testid="blob-gc">
          <div class="limit-row">
            <span class="mono-label">{{ t('storage.blobStore') }}</span>
            <span class="hl" data-testid="blob-store-size">{{ toGb(blobBytes) }} GB</span>
            <button class="btn btn-primary" :disabled="optimizing" @click="optimizeBlobs" data-testid="blob-optimize-btn">
              {{ optimizing ? t('storage.optimizing') : t('storage.optimize') }}
            </button>
          </div>
          <p class="hint dim">{{ t('storage.blobGcHint') }}</p>
          <p v-if="optimizedAt" class="hint" style="color: var(--success)" data-testid="blob-optimized-note">{{ t('storage.optimizeDone') }}</p>

          <div v-if="dedup" class="dedup-box" data-testid="dedup-stats">
            <span class="mono-label">{{ t('storage.dedupTitle') }}</span>
            <div class="dedup-row">
              <div class="dedup-stat">
                <span class="dedup-val hl" data-testid="dedup-unique">{{ dedup.uniqueBlobs }}</span>
                <span class="dedup-key dim">{{ t('storage.dedupUnique') }}</span>
              </div>
              <span class="seed-sep dim">·</span>
              <div class="dedup-stat">
                <span class="dedup-val" :class="{ ok: dedup.dedupedApps > 0 }" data-testid="dedup-saved">{{ dedup.dedupedApps }}</span>
                <span class="dedup-key dim">{{ t('storage.dedupSaved') }}</span>
              </div>
              <span class="seed-sep dim">·</span>
              <div class="dedup-stat">
                <span class="dedup-val" data-testid="dedup-shared">{{ dedup.sharedBlobs }}</span>
                <span class="dedup-key dim">{{ t('storage.dedupShared') }}</span>
              </div>
              <span v-if="dedup.savedBytes > 0" class="seed-sep dim">·</span>
              <div v-if="dedup.savedBytes > 0" class="dedup-stat">
                <span class="dedup-val ok" data-testid="dedup-saved-bytes">{{ (dedup.savedBytes / 1048576).toFixed(1) }} MB</span>
                <span class="dedup-key dim">{{ t('storage.dedupSavedBytes') }}</span>
              </div>
            </div>
            <p class="hint dim">{{ t('storage.dedupHint') }}</p>
          </div>
        </div>
      </div>

      <!-- Кэш переводов (AI Translation Layer) -->
      <div class="tr-cache-box" data-testid="tr-cache">
        <div class="limit-row">
          <span class="mono-label">{{ t('storage.trCacheTitle') }}</span>
          <span class="hl" data-testid="tr-cache-size">
            {{ trCache ? humanBytes(trCache.bytes) : '—' }}
          </span>
          <span
            v-if="trEngine"
            class="tr-engine-badge"
            :class="{ gpu: trEngine.startsWith('GPU'), bonsai: trEngine.includes('BONSAI') }"
            :title="t('storage.trCacheEngineHint')"
            data-testid="tr-cache-engine"
          >{{ trEngine }}</span>
          <button
            class="btn btn-primary"
            :disabled="clearingTrCache || !trCache || trCache.count === 0"
            @click="clearTrCache"
            data-testid="tr-cache-clear-btn"
          >
            {{ clearingTrCache ? t('storage.trCacheClearing') : t('storage.trCacheClear') }}
          </button>
        </div>
        <p class="hint dim" data-testid="tr-cache-detail">
          {{ t('storage.trCacheRecords') }}: <b>{{ trCache?.count ?? 0 }}</b>
          <template v-if="trCache && trCache.count > 0">
            · {{ t('storage.trCacheAvg') }}: {{ humanBytes(trCache.avg_bytes) }}
          </template>
        </p>
        <p class="hint dim">{{ t('storage.trCacheHint') }}</p>
      </div>

      <!-- Кеш скачанных AI-моделей (каталог sn://ai-computer) -->
      <div class="tr-cache-box" data-testid="ai-models-cache">
        <div class="limit-row">
          <span class="mono-label">{{ t('storage.aiCacheTitle') }}</span>
          <span class="hl" data-testid="ai-models-cache-size">
            {{ aiCacheBytes === null ? '—' : humanBytes(aiCacheBytes) }}
          </span>
          <button
            class="btn btn-primary"
            :disabled="clearingAiCache || !aiCacheBytes"
            @click="clearAiCache"
            data-testid="ai-models-cache-clear-btn"
          >
            {{ clearingAiCache ? t('storage.aiCacheClearing') : t('storage.aiCacheClear') }}
          </button>
        </div>
        <p class="hint dim">{{ t('storage.aiCacheHint') }}</p>
      </div>
    </section>

    <!-- NETWORK TAB (v1.135.202): sticky статус-строка + под-разделы (без прокрутки) -->
    <div v-show="tab === 'network'" v-if="auth.isTauri" class="net-wrap" data-testid="settings-network-split">
    <!-- статус-строка: клик по индикатору открывает нужный раздел -->
    <div class="net-status" data-testid="net-status-strip">
      <button class="chip" :class="pqSt.total ? 'on' : 'off'" @click="goNet('pqweb')" data-testid="net-chip-dvpn">
        <i class="cdot" />{{ t('settings.net.stDvpn') }} <b>{{ pqSt.total ? t('settings.net.stOn') : t('settings.net.stOff') }}</b>
      </button>
      <button class="chip" :class="pqSt.enabled ? 'on' : 'off'" @click="goNet('pqweb')" data-testid="net-chip-socks">
        <i class="cdot" />{{ t('settings.net.stSocks') }} <b>{{ pqSt.enabled && pqSt.port ? ':' + pqSt.port : t('settings.net.stOff') }}</b>
      </button>
      <button class="chip" :class="pqOkNodes ? 'on' : 'warn'" @click="goNet('pqweb')" data-testid="net-chip-pqnodes">
        <i class="cdot" />{{ t('settings.net.stPqNodes') }} <b>{{ pqOkNodes }}/{{ pqSt.nodes.length }}</b>
      </button>
      <button class="chip" :class="myLevel" @click="goNet('mycelium')" data-testid="net-chip-mycelium">
        <i class="cdot" />{{ t('mycelium.chip') }} <b>{{ myLevel === 'off' ? t('settings.net.stOff') : (mySt?.status?.circuitsReady ?? 0) }}</b>
      </button>
      <span class="chip-sep" />
      <button class="chip" :class="myNodeId ? 'on' : 'off'" :title="myNodeId ? `${myNodeId}\n${t('settings.net.stMyNodeHint')}` : ''" @click="copyMyNodeId" data-testid="net-chip-my-node">
        <i class="cdot" />{{ t('settings.net.stMyNode') }} <b class="mono">{{ myNodeCopied ? t('common.copied') : (myNodeId ? shortAddr(myNodeId, 6, 4) : '—') }}</b>
      </button>
      <button class="chip" :class="netActiveLabel !== '—' ? 'on' : 'warn'" @click="goNet('nodes')" data-testid="net-chip-node">
        <i class="cdot" />{{ t('settings.net.stNode') }} <b class="mono">{{ netActiveLabel }}</b>
      </button>
      <button class="chip" :class="p2pOnlineCount || onlineCount ? 'on' : 'warn'" @click="goNet('nodes')" :title="`P2P Nodes ${p2pOnlineCount} / Clear Nodes ${onlineCount}`" data-testid="net-chip-online">
        <i class="cdot" />{{ t('settings.net.stOnline') }} <b>{{ p2pOnlineCount }}/{{ onlineCount }}</b>
      </button>
      <span class="chip-sep" />
      <button class="chip" :class="dhtEnabled ? (dhtDiag.dhtNodes ? 'on' : 'warn') : 'off'" @click="goNet(advanced ? 'diag' : 'discovery')" data-testid="net-chip-dht">
        <i class="cdot" />{{ t('settings.net.stDht') }} <b>{{ dhtEnabled ? dhtDiag.dhtNodes : t('settings.net.stOff') }}</b>
      </button>
      <button v-if="advanced" class="chip" :class="trackersCount ? 'on' : 'warn'" @click="goNet('discovery')" data-testid="net-chip-trackers">
        <i class="cdot" />{{ t('settings.net.stTrackers') }} <b>{{ trackersCount }}</b>
      </button>
      <button v-if="relayOnly" class="chip on violet" @click="goNet('relays')" data-testid="net-chip-relayonly">
        <i class="cdot" />{{ t('settings.net.stRelayOnly') }}
      </button>
    </div>

    <div class="net-body">
    <!-- левое sticky-меню разделов -->
    <nav class="net-nav" data-testid="net-nav">
      <button
        v-for="s in netSections.filter((x) => advanced || x.id !== 'diag')"
        :key="s.id"
        class="net-nav-btn"
        :class="{ on: netSection === s.id }"
        @click="goNet(s.id)"
        :data-testid="`net-nav-${s.id}`"
      >
        <span class="nn-label">{{ t(`settings.net.${s.label}`) }}</span>
        <span class="nn-sub mono" :data-testid="`net-nav-sub-${s.id}`">{{ netSummary(s.id) }}</span>
      </button>
      <div v-if="!advanced" class="net-nav-foot" data-testid="net-advanced-hidden">
        <p class="hint dim">{{ t('settings.net.advancedHidden') }}</p>
        <button class="btn ghost-btn tiny" @click="ui.setMode('dev')" data-testid="net-advanced-enable-btn">{{ t('settings.net.advancedShow') }}</button>
      </div>
    </nav>

    <div class="net-pane">
    <!-- PQ Web: туннель для заблокированных сайтов через узлы pq-web (v1.135.190) -->
    <PqWebCard v-show="netSection === 'pqweb'" :advanced="advanced" @status="onPqStatus" />
    <!-- Mycelium: mushroom-маршрутизация (NIP-1, experimental) -->
    <MyceliumCard v-show="netSection === 'mycelium'" :active="tab === 'network' && netSection === 'mycelium'" @status="onMyStatus" />

    <!-- ОБНАРУЖЕНИЕ: DHT, скан LAN, трекеры, сидеры -->
    <section v-show="netSection === 'discovery'" class="panel block" data-testid="settings-peers">
      <span class="block-title">{{ t('peers.title') }}</span>
      <p class="hint dim">{{ t('peers.hint') }}</p>

      <!-- Mainline DHT (pkarr) — бессерверное обнаружение, по умолчанию ВКЛ -->
      <div class="relay-row dht-row" data-testid="dht-row">
        <button
          class="toggle"
          :class="{ on: dhtEnabled }"
          role="switch"
          :aria-checked="dhtEnabled"
          @click="toggleDht"
          data-testid="dht-toggle"
        >
          <span class="knob" />
        </button>
        <div class="relay-text">
          <span class="mono-label">{{ t('peers.dhtTitle') }}</span>
          <p class="hint dim">{{ t('peers.dhtHint') }}</p>
          <span v-if="dhtFlash" class="flash hl" data-testid="dht-flash">{{ dhtFlash }}</span>
        </div>
      </div>

      <button class="btn btn-primary" :disabled="discovery.scanning" @click="scanNetwork" data-testid="scan-network-btn">
        {{ discovery.scanning ? t('peers.scanning') : t('peers.scan') }}
      </button>

      <div class="seed-summary" data-testid="peer-seed-summary">
        <div class="seed-stat">
          <span class="seed-val hl" data-testid="peer-seed-count">{{ discovery.uniqueSeeds }}</span>
          <span class="seed-key dim">{{ t('peers.seedsConnected') }}</span>
        </div>
        <span class="seed-sep dim">·</span>
        <div class="seed-stat">
          <span class="seed-val" data-testid="peer-app-count">{{ discovery.availableAppCount }}</span>
          <span class="seed-key dim">{{ t('peers.appsAvailable') }}</span>
        </div>
      </div>
      <p v-if="!discovery.uniqueSeeds" class="hint dim" data-testid="peer-empty">{{ t('peers.none') }}</p>
      <p v-if="discovery.lastError" class="hint" style="color: var(--danger)" data-testid="peer-error">
        ⚠ {{ discovery.lastError === 'no-provider' ? t('discover.noProvider') : t('discover.installError') + ': ' + discovery.lastError }}
      </p>

      <!-- WAN trackers + DHT seeders (расширенное: DEV/GEEK) -->
      <div v-if="advanced" class="cfg-grid">
      <div class="tracker-cfg" data-testid="trackers-config">
        <span class="sub-title mono-label">{{ t('peers.trackersTitle') }}</span>
        <p class="hint dim">{{ t('peers.trackersHint') }}</p>
        <textarea
          v-model="trackersText"
          class="tracker-input"
          rows="3"
          spellcheck="false"
          placeholder="159.69.188.60:7374"
          data-testid="trackers-input"
        />
        <div class="tracker-actions">
          <button class="btn btn-primary" @click="saveTrackers" data-testid="save-trackers-btn">
            {{ t('peers.saveTrackers') }}
          </button>
          <span v-if="trackerFlash" class="flash hl" data-testid="trackers-flash">{{ trackerFlash }}</span>
        </div>
      </div>

      <!-- SmartNet DHT seeders (decentralized bootstrap; multiple supported) -->
      <div class="tracker-cfg" data-testid="seeders-config">
        <span class="sub-title mono-label">
          {{ t('peers.seedersTitle') }}
          <span
            v-if="seedersAuto.length"
            class="auto-badge"
            :title="seedersAuto.join('\n')"
            data-testid="seeders-auto-badge"
          >+{{ seedersAuto.length }} {{ t('peers.seedersAuto') }}</span>
        </span>
        <p class="hint dim">{{ t('peers.seedersHint') }}</p>
        <textarea
          v-model="seedersText"
          class="tracker-input"
          rows="3"
          spellcheck="false"
          placeholder="116.202.32.250:6881"
          data-testid="seeders-input"
        />
        <div class="tracker-actions">
          <button class="btn btn-primary" @click="saveSeeders" data-testid="save-seeders-btn">
            {{ t('peers.saveSeeders') }}
          </button>
          <span v-if="seedersFlash" class="flash hl" data-testid="seeders-flash">{{ seedersFlash }}</span>
        </div>
      </div>
      </div>
      <!-- /cfg-grid -->
    </section>

    <!-- РЕЛЕ И ПРИВАТНОСТЬ: relay-only, мобильные фолбэки, n1-реле -->
    <section v-show="netSection === 'relays'" class="panel block" data-testid="settings-relays">
      <span class="block-title">{{ t('settings.net.relaysTitle') }}</span>
      <p class="hint dim">{{ t('settings.net.relaysHint') }}</p>

      <!-- Relay-only privacy toggle -->
      <div class="relay-row first" data-testid="relay-only-row">
        <button
          class="toggle"
          :class="{ on: relayOnly }"
          role="switch"
          :aria-checked="relayOnly"
          @click="toggleRelay"
          data-testid="relay-only-toggle"
        >
          <span class="knob" />
        </button>
        <div class="relay-text">
          <span class="mono-label">{{ t('peers.relayTitle') }}</span>
          <p class="hint dim">{{ t('peers.relayHint') }}</p>
        </div>
      </div>

      <!-- Мобильные фолбэки: DoH + pkarr HTTPS-discovery (авто / вкл / выкл) -->
      <div class="relay-row" data-testid="mobile-fb-row">
        <div class="mob-seg" role="radiogroup" data-testid="mobile-fb-seg">
          <button
            v-for="m in mobileModes"
            :key="m"
            class="mob-btn"
            :class="{ active: mobileFb === m }"
            role="radio"
            :aria-checked="mobileFb === m"
            @click="setMobileFbMode(m)"
            :data-testid="`mobile-fb-${m}`"
          >
            {{ t(`peers.mobile_${m}`) }}
          </button>
        </div>
        <div class="relay-text">
          <span class="mono-label">{{ t('peers.mobileTitle') }}</span>
          <p class="hint dim">{{ t('peers.mobileHint') }}</p>
          <span v-if="mobileFbFlash" class="flash hl" data-testid="mobile-fb-flash">{{ mobileFbFlash }}</span>
        </div>
      </div>

      <!-- SmartNet custom relays (n1-relay) — расширенное -->
      <template v-if="advanced">
      <div class="relay-row" data-testid="n1-relay-row">
        <button
          class="toggle"
          :class="{ on: n1RelayOn }"
          role="switch"
          :aria-checked="n1RelayOn"
          @click="toggleN1Relay"
          data-testid="n1-relay-toggle"
        >
          <span class="knob" />
        </button>
        <div class="relay-text">
          <span class="mono-label">{{ t('peers.n1RelayTitle') }}</span>
          <p class="hint dim">{{ t('peers.n1RelayHint') }}</p>
        </div>
      </div>
      <div v-if="n1RelayOn" class="tracker-cfg" data-testid="n1-relay-config">
        <textarea
          v-model="n1RelayText"
          class="tracker-input"
          rows="3"
          spellcheck="false"
          placeholder="https://relay1.smartnet.example&#10;https://relay2.smartnet.example"
          data-testid="n1-relay-input"
        />
        <div class="tracker-actions">
          <button class="btn btn-primary" @click="saveN1Relays" data-testid="save-n1-relay-btn">
            {{ t('peers.saveN1Relays') }}
          </button>
          <button
            class="btn"
            :disabled="n1RelayCheckRunning"
            @click="checkN1Relays"
            data-testid="check-n1-relay-btn"
          >
            {{ n1RelayCheckRunning ? t('peers.checkingN1Relays') : t('peers.checkN1Relays') }}
          </button>
          <span v-if="n1RelayFlash" class="flash hl" data-testid="n1-relay-flash">{{ n1RelayFlash }}</span>
        </div>
        <ul
          v-if="n1RelayCheckResults.length"
          class="doc-steps"
          data-testid="n1-relay-check-results"
        >
          <li
            v-for="c in n1RelayCheckResults"
            :key="c.id"
            class="doc-step"
            :class="`st-${c.status}`"
            :data-testid="`n1-relay-check-${c.id}`"
          >
            <span class="doc-dot"></span>
            <span class="doc-detail">{{ c.detail }}</span>
          </li>
        </ul>
      </div>
      </template>
    </section>

    <!-- РАЗДАЧА И ЗАГРУЗКА: автообновление, swarm, DHT-провайдеры -->
    <section v-show="netSection === 'seeding'" class="panel block" data-testid="settings-net-seeding">
      <span class="block-title">{{ t('settings.net.seedingTitle') }}</span>
      <p class="hint dim">{{ t('settings.net.seedingHint') }}</p>

      <!-- Auto-update apps toggle -->
      <div class="relay-row first" data-testid="auto-update-row">
        <button
          class="toggle"
          :class="{ on: autoUpdate }"
          role="switch"
          :aria-checked="autoUpdate"
          @click="toggleAutoUpdate"
          data-testid="auto-update-toggle"
        >
          <span class="knob" />
        </button>
        <div class="relay-text">
          <span class="mono-label">{{ t('peers.autoUpdateTitle') }}</span>
          <p class="hint dim">{{ t('peers.autoUpdateHint') }}</p>
        </div>
      </div>

      <div class="relay-row" data-testid="swarm-row">
        <button
          class="toggle"
          :class="{ on: swarmEnabled }"
          role="switch"
          :aria-checked="swarmEnabled"
          @click="toggleSwarm"
          data-testid="swarm-toggle"
        >
          <span class="knob" />
        </button>
        <div class="relay-text">
          <span class="mono-label">{{ t('peers.swarmTitle') }}</span>
          <p class="hint dim">{{ t('peers.swarmHint') }}</p>
        </div>
      </div>

      <!-- Мини-виджет: топ сидеров по скорости (из весов бандита) -->
      <div v-if="swarmEnabled" class="swarm-top" data-testid="swarm-top-seeders">
        <div class="swarm-top-head">
          <span class="mono-label">{{ t('peers.swarmTopTitle') }}</span>
          <button class="mini-refresh" @click="loadSwarmSeeders" :title="t('network.refresh')" data-testid="swarm-top-refresh">↻</button>
        </div>
        <p v-if="!swarmSeeders.length" class="hint dim" data-testid="swarm-top-empty">{{ t('peers.swarmTopEmpty') }}</p>
        <ul v-else class="swarm-list">
          <li
            v-for="(s, i) in swarmSeeders"
            :key="s.peer"
            class="swarm-row"
            :data-testid="`swarm-seed-${i}`"
          >
            <span class="swarm-rank">{{ i + 1 }}</span>
            <span class="swarm-peer mono" :title="s.peer">{{ swarmSeederShort(s.peer) }}</span>
            <span class="swarm-bar">
              <span
                class="swarm-bar-fill"
                :class="{ bad: s.errors > 0 }"
                :style="{ width: (swarmMaxSpeed ? Math.max(6, (s.speed / swarmMaxSpeed) * 100) : 6) + '%' }"
              />
            </span>
            <span class="swarm-meta mono">
              <span class="ok">✓{{ s.successes }}</span>
              <span v-if="s.errors > 0" class="err">✗{{ s.errors }}</span>
            </span>
          </li>
        </ul>
      </div>

      <div class="relay-row" data-testid="dht-providers-row">
        <button
          class="toggle"
          :class="{ on: dhtProviders }"
          role="switch"
          :aria-checked="dhtProviders"
          @click="toggleDhtProviders"
          data-testid="dht-providers-toggle"
        >
          <span class="knob" />
        </button>
        <div class="relay-text">
          <span class="mono-label">{{ t('peers.dhtProvidersTitle') }}</span>
          <p class="hint dim">{{ t('peers.dhtProvidersHint') }}</p>
        </div>
      </div>
    </section>

    <!-- ДИАГНОСТИКА (расширенное): DHT-диагностика и Сетевой доктор в две колонки -->
    <section v-show="netSection === 'diag'" v-if="advanced" class="panel block" data-testid="settings-diag">
      <span class="block-title">{{ t('settings.net.diagTitle') }}</span>
      <p class="hint dim">{{ t('settings.net.diagHint') }}</p>
      <div class="cfg-grid" data-testid="settings-diag-tools">
      <!-- P1: диагностика трекерлесс-DHT -->
      <div class="dht-diag" data-testid="dht-diag">
        <div class="dht-diag-head">
          <span class="mono-label">{{ t('peers.dhtDiagTitle') }}</span>
          <button class="mini-refresh" @click="loadDhtDiag" :disabled="dhtDiagLoading" :title="t('peers.dhtDiagRefresh')" data-testid="dht-diag-refresh">↻</button>
        </div>
        <p v-if="!dhtProviders" class="hint dim" data-testid="dht-diag-off">{{ t('peers.dhtDiagOff') }}</p>
        <div v-else class="dht-diag-grid" data-testid="dht-diag-grid">
          <div class="dht-diag-stat">
            <span class="dht-diag-val hl" data-testid="dht-diag-seeders">{{ dhtDiag.seeders }}</span>
            <span class="dht-diag-key dim">{{ t('peers.dhtDiagSeeders') }}</span>
          </div>
          <div class="dht-diag-stat">
            <span class="dht-diag-val" data-testid="dht-diag-apps">{{ dhtDiag.apps }}</span>
            <span class="dht-diag-key dim">{{ t('peers.dhtDiagApps') }}</span>
          </div>
          <div class="dht-diag-stat">
            <span class="dht-diag-val" data-testid="dht-diag-announced">{{ dhtDiag.announcedApps }}</span>
            <span class="dht-diag-key dim">{{ t('peers.dhtDiagAnnounced') }}</span>
          </div>
          <div class="dht-diag-stat">
            <span class="dht-diag-val" data-testid="dht-diag-nodes">{{ dhtDiag.dhtNodes }}</span>
            <span class="dht-diag-key dim">{{ t('peers.dhtDiagNodes') }}</span>
          </div>
          <div class="dht-diag-stat wide">
            <span class="dht-diag-val" :class="{ dim: !dhtDiag.beaconPort }" data-testid="dht-diag-beacon">{{
              dhtDiag.beaconPort ? `UDP :${dhtDiag.beaconPort}` : t('peers.dhtDiagBeaconOff')
            }}</span>
            <span class="dht-diag-key dim">{{ t('peers.dhtDiagBeacon') }}</span>
          </div>
        </div>
      </div>

      <ConnStatsCard v-if="netSection === 'diag'" />

      <!-- v1.63.13: «Сетевой доктор» — диагностика мобильных сетей / DPI -->
      <div class="net-doctor" data-testid="net-doctor">
        <div class="dht-diag-head">
          <span class="mono-label">{{ t('peers.docTitle') }}</span>
          <div class="doc-head-actions">
            <button
              v-if="docSteps.length && !docRunning"
              class="mini-refresh"
              @click="copyDoctorReport"
              :title="t('peers.docCopy')"
              data-testid="net-doctor-copy-btn"
            >⧉</button>
            <button
              class="btn ghost-btn doc-run-btn"
              :disabled="docRunning || !auth.isTauri"
              @click="runNetDoctor"
              data-testid="net-doctor-run-btn"
            >{{ docRunning ? t('peers.docRunning') : t('peers.docRun') }}</button>
          </div>
        </div>
        <p class="hint dim" data-testid="net-doctor-hint">{{ t('peers.docHint') }}</p>
        <ul v-if="docSteps.length" class="doc-steps" data-testid="net-doctor-steps">
          <li
            v-for="s in docSteps"
            :key="s.id"
            class="doc-step"
            :class="[`st-${s.status}`, { 'doc-verdict': s.id === 'verdict' }]"
            :data-testid="`net-doctor-step-${s.id}`"
          >
            <span class="doc-dot"></span>
            <span class="doc-name">{{ docTitle(s.id) }}</span>
            <span class="doc-detail">{{ s.detail }}</span>
          </li>
        </ul>
        <p v-if="docCopyFlash" class="hint hl" data-testid="net-doctor-copy-flash">{{ docCopyFlash }}</p>
      </div>

      <!-- v1.135.210: discovery EndpointId — источник (dns / pkarr / mdns / mainline / peerbook) и мс -->
      <div class="net-doctor disc-diag" data-testid="disc-diag">
        <div class="dht-diag-head">
          <span class="mono-label">{{ t('peers.discTitle') }}</span>
          <button
            class="btn ghost-btn doc-run-btn"
            :disabled="discRunning || !auth.isTauri || !discNode"
            @click="runDiscProbe"
            data-testid="disc-diag-run-btn"
          >{{ discRunning ? t('peers.discRunning') : t('peers.discRun') }}</button>
        </div>
        <p class="hint dim">{{ t('peers.discHint') }}</p>
        <select v-model="discNode" class="disc-select mono" data-testid="disc-diag-node">
          <option v-for="h in p2pNodes" :key="h" :value="h">{{ h.replace('api://', '').slice(0, 16) }}… {{ h.split('/').pop() }}</option>
        </select>
        <ul v-if="discReport" class="doc-steps" data-testid="disc-diag-steps">
          <li
            v-for="(h, i) in discReport.hits"
            :key="i"
            class="doc-step st-ok"
            :data-testid="`disc-diag-hit-${h.source}`"
          >
            <span class="doc-dot"></span>
            <span class="doc-name">{{ discSourceLabel(h.source) }}</span>
            <span class="doc-detail">{{ h.ms }} ms · relay {{ h.relays }}{{ h.relay ? ` (${h.relay})` : '' }} · ip {{ h.addrs }}</span>
          </li>
          <li v-if="!discReport.hits.length" class="doc-step st-fail" data-testid="disc-diag-none">
            <span class="doc-dot"></span>
            <span class="doc-name">{{ t('peers.discNone') }}</span>
            <span class="doc-detail">{{ discReport.lookupMs }} ms</span>
          </li>
          <li class="doc-step doc-verdict" :class="discReport.connectMs != null ? 'st-ok' : 'st-fail'" data-testid="disc-diag-connect">
            <span class="doc-dot"></span>
            <span class="doc-name">{{ t('peers.discConnect') }}</span>
            <span class="doc-detail">{{ discReport.connectMs != null ? `${discReport.connectMs} ms · ${discReport.via}` : t('nodes.down') }}</span>
          </li>
          <li v-for="(e, i) in discReport.errors" :key="'e' + i" class="doc-step st-warn" :data-testid="`disc-diag-err-${i}`">
            <span class="doc-dot"></span>
            <span class="doc-name">{{ t('peers.discError') }}</span>
            <span class="doc-detail">{{ e }}</span>
          </li>
        </ul>
      </div>
      </div>
      <!-- /cfg-grid -->
    </section>

    <!-- SMARTHOLDEM NODE POOL (native only) -->
    <section v-show="netSection === 'nodes'" class="panel block" data-testid="settings-nodes">
      <div class="block-head">
        <span class="block-title">{{ t('nodes.title') }}</span>
        <div class="node-actions">
          <button class="btn ghost-btn" :disabled="tabLoading" @click="tabAuto" data-testid="node-auto-btn">
            ⚡ {{ t('nodes.auto') }}
          </button>
          <button class="btn btn-primary" :disabled="tabLoading" @click="tabRescan" data-testid="node-rescan-btn">
            {{ tabLoading ? t('nodes.pinging') : t('nodes.rescan') }}
          </button>
        </div>
      </div>

      <!-- Node source tabs: clearnet pool vs our P2P protocol nodes (api://) -->
      <div class="node-tabs" data-testid="node-tabs">
        <button
          class="node-tab"
          :class="{ on: nodeTab === 'p2p' }"
          @click="nodeTab = 'p2p'"
          data-testid="node-tab-p2p"
        >
          🔗 P2P Nodes <span class="ntcount">{{ p2pNodes.length }}</span>
        </button>
        <button
          class="node-tab"
          :class="{ on: nodeTab === 'clear' }"
          @click="nodeTab = 'clear'"
          data-testid="node-tab-clear"
        >
          Clear Nodes <span class="ntcount">{{ nodes.length }}</span>
        </button>
      </div>

      <!-- CLEAR NODES (clearnet mainnet pool) -->
      <template v-if="nodeTab === 'clear'">
        <p class="hint dim">{{ t('nodes.hint') }}</p>

        <div class="node-summary" data-testid="node-summary">
          <span class="dim">{{ t('nodes.active') }}: <b class="active-host hl">{{ activeHost || '—' }}</b></span>
          <span class="dim">{{ t('nodes.online') }}: <b class="hl">{{ onlineCount }}/{{ nodes.length }}</b></span>
        </div>

        <ul class="node-list" data-testid="node-list">
          <li
            v-for="n in nodes"
            :key="n.host"
            class="node-item"
            :class="{ active: n.active, offline: !n.online }"
            :aria-disabled="!n.online"
            :title="n.online ? '' : t('nodes.offlineNoSelect')"
            @click="pickNode(n)"
            :data-testid="`node-${n.host}`"
          >
            <span class="ndot" :class="n.online ? 'on' : 'off'"></span>
            <span class="nhost">{{ n.host }}</span>
            <span v-if="n.height" class="nheight">#{{ n.height }}</span>
            <span class="nping" :class="nodePingClass(n.latencyMs)">
              {{ n.latencyMs != null ? `${n.latencyMs}ms` : t('nodes.offline') }}
            </span>
            <span v-if="n.active" class="nbadge">● {{ t('nodes.connected') }}</span>
            <span v-else-if="nodeBusy === n.host" class="nbadge dim">…</span>
          </li>
        </ul>
        <p v-if="!nodes.length && !nodesLoading" class="hint dim" data-testid="node-empty">{{ t('nodes.empty') }}</p>
      </template>

      <!-- P2P NODES (our protocol, api://) -->
      <template v-else>
        <p class="hint dim">{{ t('nodes.p2pHint') }}</p>
        <ul v-if="p2pNodes.length" class="node-list" data-testid="p2p-node-list">
          <li
            v-for="h in p2pNodes"
            :key="h"
            class="node-item custom-item"
            :class="{ active: activeNode === h, offline: isDown(h) }"
            :data-testid="`p2p-node-${h}`"
          >
            <div class="custom-item-main">
              <span class="ndot" :class="pingResults[h] ? (pingResults[h].online ? 'on' : 'off') : 'on'"></span>
              <span class="nhost">{{ h }}</span>
              <span class="nbadge p2p-badge">{{ t('nodes.p2p') }}</span>
              <template v-if="pingResults[h]">
                <span v-if="pingResults[h].online" class="nping fast" :data-testid="`p2p-node-ping-${h}`">
                  {{ pingResults[h].latencyMs != null ? `${pingResults[h].latencyMs}ms` : 'ok' }}
                </span>
                <span v-else class="nping off" :title="pingResults[h].error || ''" :data-testid="`p2p-node-ping-${h}`">{{ t('nodes.down') }}</span>
                <span v-if="!pingResults[h].online && pingResults[h].error" class="nerr" :title="pingResults[h].error" :data-testid="`p2p-node-error-${h}`">{{ pingResults[h].error }}</span>
                <span v-if="pingResults[h].height" class="nheight">#{{ pingResults[h].height }}</span>
                <span
                  v-if="pingResults[h].online"
                  class="nbadge"
                  :class="pingResults[h].verified ? 'sig-ok' : 'sig-bad'"
                >{{ pingResults[h].verified ? t('nodes.verified') : t('nodes.unverified') }}</span>
              </template>
              <button
                class="btn ghost-btn tiny"
                :disabled="pingBusy === h"
                @click="checkNode(h)"
                :data-testid="`p2p-node-check-${h}`"
              >
                {{ pingBusy === h ? t('nodes.checking') : t('nodes.check') }}
              </button>
              <span v-if="activeNode === h" class="nbadge">● {{ t('nodes.connected') }}</span>
              <button
                v-else
                class="btn ghost-btn tiny"
                :disabled="nodeBusy === h || isDown(h)"
                :title="isDown(h) ? t('nodes.offlineNoSelect') : ''"
                @click="pickCustomNode(h)"
                :data-testid="`p2p-node-select-${h}`"
              >
                {{ t('nodes.select') }}
              </button>
            </div>
          </li>
        </ul>
        <p v-else class="hint dim" data-testid="p2p-node-empty">{{ t('nodes.p2pEmpty') }}</p>
      </template>

      <!-- Custom nodes (clearnet or api:// P2P) -->
      <div class="custom-nodes" :class="{ 'focus-flash': focusCustom }" data-testid="custom-nodes">
        <span class="block-subtitle">{{ t('nodes.custom') }}</span>
        <p class="hint dim">{{ t('nodes.customHint') }}</p>

        <!-- Local node autodetect: SmartHoldem answers on localhost:4003 -->
        <div v-if="showLocalNode" class="local-node-banner" data-testid="local-node-banner">
          <span class="ndot on"></span>
          <div class="local-node-text">
            <b>{{ t('nodes.localDetected') }}</b>
            <span class="hint dim" data-testid="local-node-info">
              {{ localNode!.host }}
              <template v-if="localNode!.latencyMs != null"> · {{ localNode!.latencyMs }}ms</template>
              <template v-if="localNode!.height"> · #{{ localNode!.height }}</template>
            </span>
          </div>
          <button
            class="btn btn-primary tiny"
            :disabled="localNodeBusy"
            @click="useLocalNode"
            data-testid="local-node-use-btn"
          >
            {{ t('nodes.useLocal') }}
          </button>
        </div>

        <div class="add-node-row">
          <input
            v-model="newNode"
            class="node-input"
            :placeholder="t('nodes.addPlaceholder')"
            data-testid="custom-node-input"
            @keyup.enter="addNode"
          />
          <button
            class="btn btn-primary"
            :disabled="addingNode || !newNode.trim()"
            @click="addNode"
            data-testid="custom-node-add-btn"
          >
            {{ t('nodes.add') }}
          </button>
        </div>
        <p v-if="nodeError" class="hint err" data-testid="custom-node-error">{{ nodeError }}</p>

        <ul v-if="customNodes.length" class="node-list" data-testid="custom-node-list">
          <li
            v-for="h in customNodes"
            :key="h"
            class="node-item custom-item"
            :class="{ active: activeNode === h, offline: isDown(h) }"
            :data-testid="`custom-node-${h}`"
          >
            <div class="custom-item-main">
              <span class="ndot" :class="pingResults[h] ? (pingResults[h].online ? 'on' : 'off') : 'on'"></span>
              <span class="nhost">{{ h }}</span>
              <span v-if="h.startsWith('api://')" class="nbadge p2p-badge">{{ t('nodes.p2p') }}</span>
              <span v-if="h.startsWith('http://')" class="nbadge" data-testid="custom-node-local-badge">{{ t('nodes.local') }}</span>

              <!-- ping result -->
              <template v-if="pingResults[h]">
                <span
                  v-if="pingResults[h].online"
                  class="nping fast"
                  :data-testid="`custom-node-ping-${h}`"
                >{{ pingResults[h].latencyMs != null ? `${pingResults[h].latencyMs}ms` : 'ok' }}</span>
                <span v-else class="nping off" :data-testid="`custom-node-ping-${h}`">{{ t('nodes.down') }}</span>
                <span v-if="pingResults[h].height" class="nheight">#{{ pingResults[h].height }}</span>
                <span
                  v-if="h.startsWith('api://') && pingResults[h].online"
                  class="nbadge"
                  :class="pingResults[h].verified ? 'sig-ok' : 'sig-bad'"
                >{{ pingResults[h].verified ? t('nodes.verified') : t('nodes.unverified') }}</span>
                <span
                  v-if="poolDelta(h)"
                  class="nping"
                  :class="poolDelta(h)!.slow ? 'mid' : 'fast'"
                  :title="t('nodes.vsPoolHint')"
                  :data-testid="`custom-node-vspool-${h}`"
                >{{ t('nodes.vsPool', { x: poolDelta(h)!.ratio }) }}</span>
              </template>

              <button
                class="btn ghost-btn tiny"
                :disabled="pingBusy === h"
                @click="checkNode(h)"
                :data-testid="`custom-node-check-${h}`"
              >
                {{ pingBusy === h ? t('nodes.checking') : t('nodes.check') }}
              </button>
              <span v-if="activeNode === h" class="nbadge">● {{ t('nodes.connected') }}</span>
              <button
                v-else
                class="btn ghost-btn tiny"
                :disabled="nodeBusy === h || isDown(h)"
                :title="isDown(h) ? t('nodes.offlineNoSelect') : ''"
                @click="pickCustomNode(h)"
                :data-testid="`custom-node-select-${h}`"
              >
                {{ t('nodes.select') }}
              </button>
              <button
                class="btn ghost-btn tiny danger"
                @click="removeNode(h)"
                :data-testid="`custom-node-remove-${h}`"
              >
                {{ t('nodes.remove') }}
              </button>
            </div>
          </li>
        </ul>
        <p v-else class="hint dim" data-testid="custom-node-empty">{{ t('nodes.noCustom') }}</p>
      </div>
    </section>
    </div>
    <!-- /net-pane -->
    </div>
    <!-- /net-body -->
    </div>
    <!-- /NETWORK -->


    <!-- BACKUP & RESTORE -->
    <section v-show="tab === 'storage'" class="panel block" data-testid="settings-backup">
      <span class="block-title">{{ t('settings.backup') }}</span>
      <p class="hint dim">{{ t('settings.backupHint') }}</p>
      <div class="backup-row">
        <button class="btn btn-primary" @click="exportVault" data-testid="export-vault-btn">
          {{ t('settings.exportVault') }}
        </button>
        <button class="btn" @click="showQr" data-testid="show-qr-btn">
          {{ t('settings.showQr') }}
        </button>
        <button class="btn" @click="showMessengerQr" data-testid="show-messenger-qr-btn">
          {{ t('settings.showMsgQr') }}
        </button>
        <button class="btn" @click="triggerImport" data-testid="import-vault-btn">
          {{ t('settings.importVault') }}
        </button>
        <input
          ref="importInput"
          type="file"
          accept="application/json,.json"
          class="hidden-input"
          @change="onImportFile"
          data-testid="import-vault-input"
        />
      </div>
      <p v-if="flash" class="flash hl" data-testid="backup-flash">{{ flash }}</p>
    </section>

    <!-- DANGER ZONE -->
    <section v-show="tab === 'danger'" class="panel block danger" data-testid="settings-danger">
      <span class="block-title danger-title">{{ t('settings.dangerZone') }}</span>
      <p class="hint dim">{{ t('settings.dangerHint') }}</p>

      <button v-if="!confirming" class="btn btn-danger" @click="confirming = true" data-testid="reset-identity-btn">
        {{ t('settings.resetIdentity') }}
      </button>
      <div v-else class="confirm-row" data-testid="reset-confirm">
        <span class="confirm-text">{{ t('settings.resetConfirm') }}</span>
        <div class="confirm-btns">
          <button class="btn" @click="confirming = false" data-testid="reset-cancel-btn">
            {{ t('common.cancel') }}
          </button>
          <button class="btn btn-danger" @click="reset" data-testid="reset-confirm-btn">
            {{ t('common.confirm') }}
          </button>
        </div>
      </div>

      <div class="wipe-block">
        <p class="hint dim">{{ t('settings.wipeHint') }}</p>
        <button v-if="!confirmingWipe" class="btn btn-danger" @click="confirmingWipe = true" data-testid="wipe-all-btn">
          {{ t('settings.wipeAll') }}
        </button>
        <div v-else class="confirm-row" data-testid="wipe-confirm">
          <span class="confirm-text">{{ t('settings.wipeConfirm') }}</span>
          <div class="confirm-btns">
            <button class="btn" @click="confirmingWipe = false" data-testid="wipe-cancel-btn">
              {{ t('common.cancel') }}
            </button>
            <button class="btn btn-danger" @click="wipe" data-testid="wipe-confirm-btn">
              {{ t('common.confirm') }}
            </button>
          </div>
        </div>
      </div>
    </section>

    <!-- QR modal -->
    <div v-if="qrOpen" class="qr-overlay" @click.self="qrOpen = false" data-testid="qr-modal">
      <div class="qr-card panel">
        <div class="qr-head">
          <span class="block-title">{{ t('settings.qrTitle') }}</span>
          <button class="btn close-x" @click="qrOpen = false" data-testid="qr-close-btn">✕</button>
        </div>
        <img :src="qrDataUrl" class="qr-img" alt="vault qr" data-testid="qr-image" />
        <p class="hint dim">{{ t('settings.qrHint') }}</p>
      </div>
    </div>

    <!-- QR для standalone-мессенджера: сид зашифрован PIN'ом, PIN вводится на телефоне -->
    <div v-if="msgQrOpen" class="qr-overlay" @click.self="closeMessengerQr" data-testid="messenger-qr-modal">
      <div class="qr-card panel">
        <div class="qr-head">
          <span class="block-title">{{ t('settings.msgQrTitle') }}</span>
          <button class="btn close-x" @click="closeMessengerQr" data-testid="messenger-qr-close-btn">✕</button>
        </div>
        <img :src="msgQrDataUrl" class="qr-img" alt="messenger seed qr" data-testid="messenger-qr-image" />
        <p class="msg-qr-pin mono" data-testid="messenger-qr-pin">PIN <b>{{ msgQrPin }}</b></p>
        <p class="hint dim">{{ t('settings.msgQrHint') }}</p>
      </div>
    </div>

    <StorageQuotaModal />
  </div>
</template>

<style scoped>
.settings {
  width: 100%;
  display: flex;
  flex-direction: column;
  gap: 18px;
}
.page-title {
  font-size: 15px;
  letter-spacing: 0.14em;
  color: var(--accent);
  font-weight: var(--fw-700);
}
/* --- шапка с тулбаром обновлений (v1.135.203) --- */
.page-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  flex-wrap: wrap;
}
.hdr-tools {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-left: auto;
}
.ver-pill {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 7px 12px;
  font-family: var(--font-mono, monospace);
  font-size: 11px;
  letter-spacing: 0.06em;
  color: var(--text-dim);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 999px;
  cursor: help;
}
.ver-pill .vdot { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); box-shadow: 0 0 8px var(--accent); flex: none; }
.ver-pill.checking .vdot { animation: vdot-pulse 0.9s ease-in-out infinite; }
.ver-pill.found { color: #f59e0b; border-color: rgba(245, 158, 11, 0.5); }
.ver-pill.found .vdot { background: #f59e0b; box-shadow: 0 0 8px rgba(245, 158, 11, 0.7); }
.ver-pill.error .vdot { background: var(--danger); box-shadow: 0 0 8px var(--danger); }
@keyframes vdot-pulse { 0%, 100% { opacity: 0.25; } 50% { opacity: 1; } }
.hdr-btn {
  padding: 9px 14px;
  font-size: 11px;
  gap: 8px;
  white-space: nowrap;
}
.hdr-btn.btn-primary { box-shadow: 0 0 22px -6px var(--accent-glow); }
.hdr-btn.busy { opacity: 0.85; }
.hdr-ico { display: inline-block; font-size: 13px; line-height: 1; }
.hdr-ico.spin { animation: hdr-spin 0.9s linear infinite; }
@keyframes hdr-spin { to { transform: rotate(360deg); } }
.hdr-crash { color: #f59e0b; border-color: rgba(245, 158, 11, 0.45); background: rgba(245, 158, 11, 0.06); }
.hdr-crash:hover { color: #fbbf24; border-color: #f59e0b; box-shadow: 0 0 0 1px rgba(245, 158, 11, 0.4), 0 0 18px -6px rgba(245, 158, 11, 0.8); }
.hdr-msg {
  display: flex;
  align-items: center;
  gap: 10px;
  margin: -6px 0 0;
  padding: 10px 14px;
  font-size: 12px;
  line-height: 1.45;
  color: var(--text);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-left: 3px solid var(--accent);
  border-radius: var(--radius);
}
.hdr-msg.found { border-left-color: #f59e0b; }
.hdr-msg.error { border-left-color: var(--danger); }
.hdr-msg-close {
  margin-left: auto;
  padding: 2px 6px;
  color: var(--text-dim);
  background: transparent;
  border: 0;
  cursor: pointer;
  font-size: 12px;
  transition: color 0.16s ease;
}
.hdr-msg-close:hover { color: var(--text); }
.hdr-msg-enter-active, .hdr-msg-leave-active { transition: opacity 0.2s ease, transform 0.2s ease; }
.hdr-msg-enter-from, .hdr-msg-leave-to { opacity: 0; transform: translateY(-4px); }
@media (max-width: 720px) {
  .hdr-tools { width: 100%; margin-left: 0; flex-wrap: wrap; }
}
/* --- metallic section tabs --- */
.seg-tabs {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  padding: 6px;
  background: linear-gradient(180deg, #1c1d21, #131316);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.seg-btn {
  flex: 1 1 auto;
  min-width: 96px;
  padding: 10px 14px;
  font-size: 11px;
  font-weight: var(--fw-700);
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text-dim);
  background: linear-gradient(180deg, #2a2b30, #1a1b1f);
  border: 1px solid #34353b;
  border-top-color: #44454c;
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.05);
  cursor: pointer;
  transition: color 0.18s ease, border-color 0.18s ease, background-color 0.18s ease, transform 0.12s ease;
}
.seg-btn:hover {
  color: var(--text);
  border-color: var(--border-accent);
  transform: translateY(-1px);
}
.seg-btn.active {
  color: var(--accent);
  border-color: var(--accent);
  background: linear-gradient(180deg, #23262b, #16181c);
  box-shadow: inset 0 0 0 1px var(--accent), inset 0 1px 0 rgba(255, 255, 255, 0.06);
}
.seg-btn.danger.active {
  color: var(--danger);
  border-color: var(--danger);
  box-shadow: inset 0 0 0 1px var(--danger);
}
.panel.block {
  padding: 20px;
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.block-title {
  font-size: 12px;
  letter-spacing: 0.12em;
  color: var(--secondary);
  text-transform: uppercase;
  font-weight: var(--fw-600);
}
.theme-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 12px;
}
.theme-btn {
  position: relative;
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 12px;
  padding: 16px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--bg-elevated);
  transition: border-color 0.18s ease, transform 0.18s ease;
}
.theme-btn:hover {
  border-color: var(--border-accent);
  transform: translateY(-2px);
}
.theme-btn.active {
  border-color: var(--accent);
  box-shadow: 0 0 0 1px var(--accent);
}
.swatch {
  width: 100%;
  height: 42px;
  border: 1px solid var(--border-subtle);
  border-radius: 4px;
  overflow: hidden;
}
.swatch.default-dark {
  background: linear-gradient(135deg, #202124 0 46%, #2d2e31 46% 72%, #2ee3c6 72%);
}
.swatch.default-light {
  background: linear-gradient(135deg, #f8f9fa 0 46%, #ffffff 46% 72%, #0e9384 72%);
}
.swatch.focus {
  background: linear-gradient(135deg, #161719 0 60%, #2e333b 60% 78%, #2fd6c0 78%);
}
.swatch.matte-dark {
  background: linear-gradient(135deg, #0d0f12 0 62%, #2ee3c6 62%);
}
.swatch.readable-light {
  background: linear-gradient(135deg, #ffffff 0 62%, #d9822b 62%);
}
.swatch.eink {
  background: linear-gradient(135deg, #ffffff 0 62%, #000000 62%);
}
.swatch.neon {
  background: linear-gradient(135deg, #0e1419 0 46%, #ff7d6b 46% 64%, #2fe0c8 64% 82%, #e85cf0 82%);
  box-shadow: 0 0 8px rgba(47, 224, 200, 0.5);
}
.theme-name {
  font-size: 11px;
  font-weight: var(--fw-700);
  letter-spacing: 0.08em;
}
.theme-check {
  position: absolute;
  top: 10px;
  right: 12px;
  font-size: 10px;
}
.lang-row {
  display: flex;
  gap: 12px;
}
.font-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: 12px;
}
.font-btn {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 14px 16px;
  text-align: left;
  border: 1px solid var(--border-subtle);
  border-radius: var(--radius);
  background: var(--panel-2);
  color: var(--text);
  transition: border-color 0.16s ease, box-shadow 0.18s ease, background-color 0.18s ease;
}
.font-btn:hover {
  border-color: var(--border-accent);
}
.font-btn.active {
  border-color: var(--accent);
  box-shadow: 0 0 0 1px var(--border-accent), 0 0 18px -8px var(--accent-glow);
}
.font-name {
  font-size: 12px;
  font-weight: var(--fw-700);
  letter-spacing: 0.04em;
}
.font-sample {
  font-size: 15px;
  color: var(--text-dim);
  letter-spacing: 0.02em;
}
.density-title {
  margin-top: 22px;
}
.opt-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  margin-top: 22px;
  padding-top: 18px;
  border-top: 1px solid var(--border);
}
.opt-text {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.opt-label {
  font-size: 11px;
  font-weight: var(--fw-700);
  letter-spacing: 0.08em;
  text-transform: uppercase;
}
.opt-hint {
  font-size: 11px;
}
.toggle {
  flex: none;
  width: 46px;
  height: 24px;
  border-radius: 999px;
  background: var(--ui-accent);
  border: 1px solid var(--border-subtle);
  position: relative;
  cursor: pointer;
  transition: background-color 0.18s ease, border-color 0.18s ease;
}
.toggle.on {
  background: var(--accent);
  border-color: var(--accent);
}
.toggle-knob {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: var(--bg);
  transition: transform 0.18s ease;
}
.toggle.on .toggle-knob {
  transform: translateX(22px);
}
.block-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.reset-btn {
  font-size: 10px;
  font-weight: var(--fw-700);
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--text-dim);
  padding: 5px 10px;
  border: 1px solid var(--border-subtle);
  border-radius: var(--radius);
  background: var(--panel-2);
  transition: color 0.16s ease, border-color 0.16s ease;
}
.reset-btn:hover {
  color: var(--accent);
  border-color: var(--border-accent);
}

/* Two-column masonry for the Interface tab (fills the empty right half) */
.iface-grid {
  columns: 2;
  column-gap: 18px;
}
.iface-grid > .block {
  break-inside: avoid;
  margin-bottom: 18px;
  display: inline-block;
  width: 100%;
}
@media (max-width: 900px) {
  .iface-grid {
    columns: 1;
  }
}

/* Font-size slider (S / M / L) */
.size-slider {
  padding: 4px 2px 0;
}
.size-range {
  width: 100%;
  appearance: none;
  -webkit-appearance: none;
  height: 4px;
  border-radius: 2px;
  background: var(--ui-accent);
  outline: none;
  cursor: pointer;
}
.size-range::-webkit-slider-thumb {
  -webkit-appearance: none;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: var(--accent);
  border: 2px solid var(--bg);
  box-shadow: 0 0 10px -2px var(--accent-glow);
}
.size-range::-moz-range-thumb {
  width: 14px;
  height: 14px;
  border-radius: 50%;
  background: var(--accent);
  border: 2px solid var(--bg);
}
.size-ticks {
  display: flex;
  justify-content: space-between;
  margin-top: 8px;
  font-size: 11px;
  font-weight: var(--fw-700);
  color: var(--text-dim);
}
.size-ticks .on {
  color: var(--accent);
}

.net-table {
  width: 100%;
  border-collapse: collapse;
}
.net-table tr {
  border-bottom: 1px solid var(--border);
}
.net-table tr:last-child {
  border-bottom: none;
}
.net-key {
  padding: 12px 0;
}
.net-val {
  text-align: right;
  font-size: 13px;
  font-weight: var(--fw-600);
}
.hint {
  font-size: 12px;
  line-height: 1.6;
}
.backup-row {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
}
.data-dir-row {
  display: flex;
  align-items: center;
  gap: 12px;
}
/* Official Repository panel */
.official-hint { font-size: 12px; margin: 6px 0 10px; }
.official-info {
  display: flex; flex-wrap: wrap; gap: 14px;
  font-size: 12px; margin-bottom: 12px;
}
.official-msg { font-size: 12px; margin-top: 8px; }
.data-dir {
  flex: 1;
  font-size: 12px;
  padding: 10px 12px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  word-break: break-all;
}
.peer-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

/* --- Network tab (v1.135.202): sticky status strip + side nav + single pane --- */
.net-wrap {
  display: flex;
  flex-direction: column;
  gap: 14px;
  min-width: 0;
}
.net-status {
  position: sticky;
  top: 0;
  z-index: 6;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  padding: 8px 10px;
  background: color-mix(in srgb, var(--bg) 96%, transparent);
  backdrop-filter: blur(14px);
  -webkit-backdrop-filter: blur(14px);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.chip {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 5px 10px;
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text-dim);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 999px;
  cursor: pointer;
  white-space: nowrap;
  transition: border-color 0.16s ease, color 0.16s ease, transform 0.12s ease;
}
.chip b { color: var(--text); font-weight: var(--fw-700); letter-spacing: 0.04em; text-transform: none; }
.chip b.mono { font-family: var(--font-mono, monospace); max-width: 220px; overflow: hidden; text-overflow: ellipsis; }
.chip:hover { border-color: var(--border-accent); color: var(--text); transform: translateY(-1px); }
.chip .cdot { width: 6px; height: 6px; border-radius: 50%; background: var(--text-dim); flex: none; }
.chip.on .cdot { background: var(--accent); box-shadow: 0 0 8px var(--accent); }
.chip.on b { color: var(--accent); }
.chip.warn .cdot { background: #f59e0b; box-shadow: 0 0 8px rgba(245, 158, 11, 0.6); }
.chip.warn b { color: #f59e0b; }
.chip.off .cdot { background: #ff5f56; }
.chip.violet .cdot { background: #c084fc; box-shadow: 0 0 8px rgba(192, 132, 252, 0.6); }
.chip.violet { color: #c084fc; border-color: rgba(192, 132, 252, 0.45); }
.chip-sep { width: 1px; height: 18px; background: var(--border); margin: 0 4px; }
.net-body {
  display: grid;
  grid-template-columns: 224px minmax(0, 1fr);
  gap: 18px;
  align-items: start;
}
.net-nav {
  position: sticky;
  top: 54px;
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 6px;
  background: linear-gradient(180deg, #1c1d21, #131316);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.net-nav-btn {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 4px;
  width: 100%;
  padding: 10px 12px;
  text-align: left;
  color: var(--text-dim);
  background: transparent;
  border: 1px solid transparent;
  border-left: 2px solid transparent;
  border-radius: calc(var(--radius) - 2px);
  cursor: pointer;
  transition: color 0.16s ease, background-color 0.16s ease, border-color 0.16s ease;
}
.net-nav-btn:hover { color: var(--text); background: rgba(255, 255, 255, 0.03); }
.net-nav-btn.on {
  color: var(--accent);
  background: linear-gradient(180deg, #23262b, #16181c);
  border-color: #34353b;
  border-left-color: var(--accent);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.05);
}
.nn-label { font-size: 11px; font-weight: var(--fw-700); letter-spacing: 0.1em; text-transform: uppercase; }
.nn-sub { font-size: 10px; color: var(--text-dim); font-family: var(--font-mono, monospace); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 100%; }
.net-nav-btn.on .nn-sub { color: color-mix(in srgb, var(--accent) 70%, var(--text-dim)); }
.net-nav-foot { display: flex; flex-direction: column; gap: 8px; padding: 10px 12px 8px; border-top: 1px solid var(--border); margin-top: 4px; }
.net-nav-foot .hint { font-size: 10.5px; line-height: 1.45; }
.net-pane { min-width: 0; display: flex; flex-direction: column; gap: 18px; }
.iface-body .net-pane > .panel { margin: 0; }
.iface-body .toggle:not(.on) { border-color: var(--border); }
.iface-body .toggle:not(.on) .toggle-knob { background: var(--text-dim); }
.err-text { color: #f87171; }
.browser-row { align-items: center; gap: 16px; flex-wrap: wrap; margin-top: 10px; padding-top: 10px; }
.browser-row .browser-select { flex: 0 0 auto; width: auto; min-width: 190px; margin: 0; padding: 8px 10px; font-size: 12px; cursor: pointer; }
.browser-select option { background: var(--bg); color: var(--text); }
.cfg-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 18px; align-items: start; }
.cfg-grid > .tracker-cfg { margin-top: 0; }
.relay-row.first { margin-top: 0; padding-top: 0; border-top: 0; }
@media (max-width: 1000px) {
  .net-body { grid-template-columns: 1fr; }
  .net-nav { position: static; flex-direction: row; flex-wrap: wrap; }
  .net-nav-btn { width: auto; flex: 1 1 140px; }
  .net-nav-foot { flex-basis: 100%; }
  .cfg-grid { grid-template-columns: 1fr; }
}
.node-actions {
  display: flex;
  gap: 8px;
  flex-shrink: 0;
}
.node-tabs {
  display: flex;
  gap: 8px;
  margin: 10px 0 4px;
}
.node-tab {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 14px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: transparent;
  color: var(--text-dim);
  font-size: 12px;
  letter-spacing: 0.03em;
  cursor: pointer;
  transition: background-color 0.15s, color 0.15s, border-color 0.15s;
}
.node-tab:hover {
  color: var(--accent);
  border-color: var(--border-accent);
}
.node-tab.on {
  background: var(--accent);
  color: #03121a;
  border-color: var(--accent);
  font-weight: 700;
}
.ntcount {
  font-size: 10px;
  opacity: 0.75;
}

.ghost-btn {
  border-color: var(--border-accent) !important;
  color: var(--accent) !important;
  background: transparent !important;
}
.node-summary {
  display: flex;
  gap: 22px;
  flex-wrap: wrap;
  font-size: 12px;
  padding: 12px 14px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-left: 2px solid var(--accent);
  border-radius: var(--radius);
}
.active-host {
  font-family: var(--font-mono, monospace);
}
.node-list {
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin: 0;
  padding: 0;
}
.node-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 14px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--bg-elevated);
  cursor: pointer;
  transition: border-color 0.18s ease, background-color 0.18s ease;
}
.node-item:hover {
  border-color: var(--border-accent);
}
.node-item.active {
  border-color: #34d399;
  background: rgba(52, 211, 153, 0.08);
}
.node-item.offline {
  opacity: 0.55;
  cursor: not-allowed;
  filter: grayscale(0.6);
}
.node-item.offline .nhost { color: var(--text-dim, #8a94a6); }
.msg-qr-pin { margin: 10px 0 4px; font-size: 13px; letter-spacing: 0.2em; color: var(--text-dim); }
.msg-qr-pin b { color: var(--accent); font-size: 20px; letter-spacing: 0.3em; margin-left: 8px; }
.disc-select { width: 100%; margin: 6px 0 8px; padding: 6px 8px; font-size: 11px; background: rgba(255, 255, 255, 0.04); color: inherit; border: 1px solid rgba(255, 255, 255, 0.12); border-radius: var(--radius); }
.ndot {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  flex-shrink: 0;
}
.ndot.on {
  background: #34d399;
  box-shadow: 0 0 8px rgba(52, 211, 153, 0.7);
}
.ndot.off {
  background: #64748b;
}
.nhost {
  font-family: var(--font-mono, monospace);
  font-size: 13px;
  color: var(--text);
  word-break: break-all;
}
.nheight {
  font-size: 11px;
  color: var(--text-dim);
  font-family: var(--font-mono, monospace);
}
.nping {
  margin-left: auto;
  font-size: 12px;
  font-family: var(--font-mono, monospace);
  white-space: nowrap;
}
.nping.fast {
  color: #34d399;
}
.nping.mid {
  color: #fbbf24;
}
.nping.slow {
  color: #fb923c;
}
.nping.off {
  color: #64748b;
}
.nerr { font-size: 10px; color: var(--text-dim, #8a94a6); max-width: 34ch; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; letter-spacing: 0; }
.nbadge {
  font-size: 11px;
  color: #34d399;
  letter-spacing: 0.5px;
  white-space: nowrap;
}
.nbadge.dim {
  color: var(--text-dim);
}
.custom-nodes {
  margin-top: 18px;
  padding-top: 16px;
  border-top: 1px solid var(--border);
}
.custom-nodes.focus-flash {
  animation: custom-focus 2.2s ease;
  border-radius: 10px;
}
.local-node-banner {
  display: flex;
  align-items: center;
  gap: 12px;
  margin: 10px 0 12px;
  padding: 10px 12px;
  background: rgba(52, 211, 153, 0.07);
  border: 1px solid rgba(52, 211, 153, 0.35);
  border-radius: 10px;
}
.local-node-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
  flex: 1;
  min-width: 0;
  font-size: 12px;
}
.local-node-text .hint {
  margin: 0;
  font-family: 'JetBrains Mono', monospace;
}
@keyframes custom-focus {
  0%, 100% { box-shadow: 0 0 0 0 transparent; }
  20%, 60% { box-shadow: 0 0 0 2px var(--border-accent), 0 0 22px rgba(129, 140, 248, 0.4); }
}
.block-subtitle {
  display: block;
  font-size: 12px;
  letter-spacing: 1px;
  text-transform: uppercase;
  color: var(--accent);
  margin-bottom: 6px;
}
.add-node-row {
  display: flex;
  gap: 8px;
  margin: 10px 0;
}
.node-input {
  flex: 1;
  padding: 10px 12px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 8px;
  color: var(--text);
  font-family: var(--font-mono, monospace);
  font-size: 12px;
}
.node-input:focus {
  outline: none;
  border-color: var(--border-accent);
}
.btn.tiny {
  padding: 5px 10px;
  font-size: 11px;
}
.btn.danger {
  border-color: #f8717155 !important;
  color: #f87171 !important;
}
.p2p-badge {
  color: #818cf8;
  border: 1px solid #818cf855;
  border-radius: 6px;
  padding: 1px 6px;
}
.hint.err {
  color: #f87171;
}
.custom-item {
  cursor: default;
}
.custom-item-main {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  width: 100%;
}
.custom-item-main .nhost {
  flex: 1;
  min-width: 160px;
}
.nbadge.sig-ok {
  color: #34d399;
  border: 1px solid #34d39955;
  border-radius: 6px;
  padding: 1px 6px;
}
.nbadge.sig-bad {
  color: #f87171;
  border: 1px solid #f8717155;
  border-radius: 6px;
  padding: 1px 6px;
}

.dedup-box {
  margin-top: 14px;
  padding: 12px 14px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: rgba(0, 0, 0, 0.22);
}
.dedup-row {
  display: flex;
  align-items: center;
  gap: 14px;
  margin: 8px 0;
}
.dedup-stat {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.dedup-val {
  font-size: 20px;
  font-weight: 800;
  font-family: var(--font-mono, monospace);
}
.dedup-val.ok {
  color: var(--success);
}
.dedup-key {
  font-size: 10px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.seed-summary {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 14px 16px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-left: 2px solid var(--accent);
  border-radius: var(--radius);
  margin-top: 12px;
}
.seed-stat {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.seed-val {
  font-size: 22px;
  font-weight: var(--fw-800);
  line-height: 1;
}
.seed-key {
  font-size: 10px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}
.seed-sep {
  font-size: 18px;
}
.peer-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  font-size: 12px;
}
.peer-dot {
  font-size: 8px;
}
.peer-node {
  flex: 1;
  word-break: break-all;
}
.peer-app-name {
  flex: 1;
  font-size: 12px;
  font-weight: var(--fw-600);
  color: var(--accent);
  word-break: break-word;
}
.peer-app-name.untitled {
  color: var(--text-dim);
  font-style: italic;
  font-weight: var(--fw-500);
}
.peer-addr,
.peer-apps {
  font-size: 11px;
  white-space: nowrap;
}
.hidden-input {
  display: none;
}
.flash {
  font-size: 12px;
  letter-spacing: 0.06em;
}
.danger {
  border-color: var(--danger);
}
.danger-title {
  color: var(--danger);
}
.confirm-row {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 14px;
  background: var(--danger-soft);
  border: 1px solid var(--danger);
  border-radius: var(--radius);
}
.confirm-text {
  font-size: 12px;
  color: var(--danger);
  line-height: 1.6;
}
.confirm-btns {
  display: flex;
  gap: 10px;
}
.wipe-block {
  margin-top: 8px;
  padding-top: 14px;
  border-top: 1px dashed var(--danger);
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.qr-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.75);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 100;
  animation: fade 0.15s ease both;
}
.qr-card {
  width: 100%;
  max-width: 420px;
  padding: 20px;
  display: flex;
  flex-direction: column;
  gap: 14px;
  align-items: center;
}
.qr-head {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.close-x {
  padding: 6px 10px;
}
.qr-img {
  width: 320px;
  height: 320px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: #fff;
}

/* --- WAN trackers + relay-only --- */
.tracker-cfg {
  display: flex;
  flex-direction: column;
  gap: 10px;
  margin-top: 6px;
  padding-top: 16px;
  border-top: 1px solid var(--border);
}
.sub-title {
  font-size: 11px;
  letter-spacing: 0.12em;
  color: var(--secondary);
  text-transform: uppercase;
  font-weight: var(--fw-600);
}
.auto-badge {
  margin-left: 8px;
  padding: 2px 8px;
  border-radius: 10px;
  font-size: 9.5px;
  letter-spacing: 0.04em;
  text-transform: none;
  color: #34d399;
  background: rgba(52, 211, 153, 0.12);
  border: 1px solid rgba(52, 211, 153, 0.32);
  cursor: help;
  white-space: nowrap;
}
.tracker-input {
  width: 100%;
  resize: vertical;
  padding: 10px 12px;
  font-family: var(--font-mono, monospace);
  font-size: 12px;
  line-height: 1.7;
  color: var(--text);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  transition: border-color 0.18s ease;
}
.tracker-input:focus {
  outline: none;
  border-color: var(--accent);
}
.tracker-actions {
  display: flex;
  align-items: center;
  gap: 14px;
}
.relay-row {
  display: flex;
  align-items: flex-start;
  gap: 14px;
  margin-top: 4px;
  padding-top: 16px;
  border-top: 1px solid var(--border);
}
.relay-text {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
/* сегмент-переключатель мобильных фолбэков (АВТО / ВКЛ / ВЫКЛ) */
.mob-seg {
  display: flex;
  border: 1px solid var(--border);
  border-radius: 8px;
  overflow: hidden;
  flex-shrink: 0;
}
.mob-btn {
  padding: 5px 10px;
  font-size: 10px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  font-family: inherit;
  background: transparent;
  color: var(--text-dim);
  border: none;
  cursor: pointer;
  transition: background-color 0.15s ease, color 0.15s ease;
}
.mob-btn + .mob-btn {
  border-left: 1px solid var(--border);
}
.mob-btn:hover {
  color: var(--text);
}
.mob-btn.active {
  background: rgba(34, 197, 94, 0.14);
  color: #22c55e;
}
/* подсветка строки при переходе с Dashboard-индикатора */
.row-flash {
  animation: rowFlash 2.2s ease;
  border-radius: 10px;
}
@keyframes rowFlash {
  0%, 100% { background: transparent; box-shadow: none; }
  15%, 55% { background: rgba(34, 197, 94, 0.12); box-shadow: 0 0 0 1px rgba(34, 197, 94, 0.4); }
}
/* --- swarm top seeders mini-widget --- */
.swarm-top {
  margin-top: 12px;
  padding: 12px;
  border-radius: 10px;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid var(--border);
}
.dht-diag {
  margin-top: 12px;
  padding: 12px;
  border-radius: 10px;
  background: rgba(52, 211, 153, 0.04);
  border: 1px solid rgba(52, 211, 153, 0.22);
}
.dht-diag-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 10px;
}
.dht-diag-grid {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 10px 14px;
}
.dht-diag-stat {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 8px 10px;
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid var(--border);
}
.dht-diag-stat.wide {
  grid-column: 1 / -1;
}
.dht-diag-val {
  font-size: 18px;
  font-weight: var(--fw-800);
  font-variant-numeric: tabular-nums;
  color: #34d399;
}
.dht-diag-key {
  font-size: 11px;
  letter-spacing: 0.03em;
}

.swarm-top-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 8px;
}
.mini-refresh {
  background: none;
  border: 1px solid var(--border);
  border-radius: 7px;
  color: var(--accent, #64b5ef);
  width: 26px;
  height: 24px;
  cursor: pointer;
  font-size: 13px;
  line-height: 1;
  transition: background-color 0.15s ease, transform 0.15s ease;
}
.mini-refresh:hover {
  background: rgba(100, 181, 239, 0.12);
  transform: rotate(90deg);
}
.swarm-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.swarm-row {
  display: grid;
  grid-template-columns: 18px 120px 1fr auto;
  align-items: center;
  gap: 10px;
}
.swarm-rank {
  font-size: 11px;
  color: #7fb4cf;
  text-align: center;
}
.swarm-peer {
  font-size: 11px;
  color: #d7ecf7;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.swarm-bar {
  height: 7px;
  border-radius: 4px;
  background: rgba(255, 255, 255, 0.07);
  overflow: hidden;
}
.swarm-bar-fill {
  display: block;
  height: 100%;
  border-radius: 4px;
  background: linear-gradient(90deg, #00e5ff, #22c55e);
  transition: width 0.3s ease;
}
.swarm-bar-fill.bad {
  background: linear-gradient(90deg, #ff8a3d, #ff6b6b);
}
.swarm-meta {
  display: flex;
  gap: 8px;
  font-size: 11px;
}
.swarm-meta .ok {
  color: #22c55e;
}
.swarm-meta .err {
  color: #ff6b6b;
}
.toggle {
  flex: 0 0 auto;
  width: 44px;
  height: 24px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--bg-elevated);
  position: relative;
  transition: background-color 0.2s ease, border-color 0.2s ease;
  cursor: pointer;
}
.toggle.on {
  background: var(--accent);
  border-color: var(--accent);
}
.toggle .knob {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: var(--text);
  transition: transform 0.2s ease;
}
.toggle.on .knob {
  transform: translateX(20px);
  background: #0d0f12;
}

/* --- storage capacity --- */
.storage-cap {
  display: flex;
  flex-direction: column;
  gap: 10px;
  margin-top: 6px;
  padding-top: 16px;
  border-top: 1px solid var(--border);
}
.cap-line {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 12px;
}
.cap-bar {
  width: 100%;
  height: 10px;
  border-radius: 999px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  overflow: hidden;
}
.cap-fill {
  height: 100%;
  background: var(--accent);
  transition: width 0.3s ease, background-color 0.3s ease;
}
.cap-fill.warn {
  background: var(--danger);
}
.transfer-box {
  margin-top: 14px;
  padding: 14px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--bg-elevated);
}
.transfer-fill {
  background: linear-gradient(90deg, var(--accent), #00E5FF);
  box-shadow: 0 0 12px rgba(0, 229, 255, 0.5);
}
.transfer-fill.done {
  background: var(--success, #22c55e);
  box-shadow: 0 0 12px rgba(34, 197, 94, 0.5);
}
.limit-row {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.tr-engine-badge {
  font-family: var(--font-mono, monospace);
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.06em;
  padding: 2px 8px;
  border-radius: 999px;
  color: var(--text-dim);
  background: color-mix(in srgb, var(--text-dim) 12%, transparent);
  border: 1px solid color-mix(in srgb, var(--text-dim) 30%, transparent);
  white-space: nowrap;
}
.tr-engine-badge.bonsai {
  color: #7ef0c0;
  background: color-mix(in srgb, #7ef0c0 12%, transparent);
  border-color: color-mix(in srgb, #7ef0c0 40%, transparent);
}
.tr-engine-badge.gpu {
  color: #8fb8ff;
  background: color-mix(in srgb, #8fb8ff 12%, transparent);
  border-color: color-mix(in srgb, #8fb8ff 40%, transparent);
}

.limit-input {
  width: 90px;
  padding: 8px 10px;
  font-size: 13px;
  color: var(--text);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.limit-input:focus {
  outline: none;
  border-color: var(--accent);
}
.unit {
  font-size: 12px;
}

/* ============================================================
   READABLE LIGHT — section tabs were stuck dark/black (hardcoded slate
   gradients); restyle to a light brushed-metal so they follow the theme.
   Orange accent fills (toggles) become calm metallic slate.
   ============================================================ */
[data-theme='readable-light'] .seg-tabs {
  background: linear-gradient(180deg, #eceef1, #dfe2e7);
  border: 1px solid var(--border);
}
[data-theme='readable-light'] .seg-btn {
  background: linear-gradient(180deg, #ffffff, #e9ebef);
  border: 1px solid #cdd2da;
  border-top-color: #ffffff;
  color: var(--secondary);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.85);
}
[data-theme='readable-light'] .seg-btn:hover {
  color: #374151;
  border-color: #9aa3af;
  transform: translateY(-1px);
}
[data-theme='readable-light'] .seg-btn.active {
  color: #1f2937;
  background: linear-gradient(180deg, #ccd1d9, #b4bbc6);
  border-color: #8b94a1;
  box-shadow: inset 0 0 0 1px #8b94a1, inset 0 1px 0 rgba(255, 255, 255, 0.6);
}
[data-theme='readable-light'] .seg-btn.danger.active {
  color: var(--danger);
  border-color: var(--danger);
  box-shadow: inset 0 0 0 1px var(--danger);
}
[data-theme='readable-light'] .toggle.on {
  background: linear-gradient(180deg, #6b7280, #4b5563);
  border-color: #4b5563;
}

/* ============================================================
   DEFAULT LIGHT — section tabs: тёмные «металлические» градиенты
   заменяем на плоский Google-стиль (белая подложка + мягкие пилюли).
   ============================================================ */
[data-theme='default-light'] .seg-tabs {
  background: #ffffff;
  border: 1px solid var(--border-subtle);
  box-shadow: var(--shadow-card);
}
[data-theme='default-light'] .seg-btn {
  background: #f4f6fa;
  border: 1px solid transparent;
  color: var(--text-dim);
  box-shadow: none;
}
[data-theme='default-light'] .seg-btn:hover {
  color: var(--text);
  background: #eef1f7;
  border-color: var(--border-subtle);
  transform: translateY(-1px);
}
[data-theme='default-light'] .seg-btn.active {
  color: var(--accent);
  background: var(--accent-soft);
  border-color: transparent;
  box-shadow: inset 0 0 0 1px var(--border-accent);
}
[data-theme='default-light'] .seg-btn.danger.active {
  color: var(--danger);
  background: var(--danger-soft);
  border-color: transparent;
  box-shadow: inset 0 0 0 1px var(--danger);
}

/* ---- NEON CYBER settings controls (glowing tabs/sliders) ---- */
[data-theme='neon'] .seg-tabs {  background: #0d161c;
  border: 1px solid var(--border);
}
[data-theme='neon'] .seg-btn {
  background: var(--panel-2);
  border: 1px solid rgba(47, 224, 200, 0.25);
  color: var(--text-dim);
  box-shadow: none;
}
[data-theme='neon'] .seg-btn:hover {
  color: var(--status);
  border-color: rgba(47, 224, 200, 0.5);
  transform: none;
}
[data-theme='neon'] .seg-btn.active {
  background: linear-gradient(180deg, #1c3b3a, #123029);
  border-color: var(--status);
  color: #5cf0dc;
  text-shadow: 0 0 8px var(--teal-glow);
  box-shadow: 0 0 20px -6px var(--teal-glow), inset 0 0 0 1px rgba(47, 224, 200, 0.15);
}
[data-theme='neon'] .size-range {
  background: #0c1318;
  height: 6px;
  box-shadow: inset 0 0 10px -4px var(--teal-glow);
}
[data-theme='neon'] .size-range::-webkit-slider-thumb {
  background: var(--accent);
  border: none;
  box-shadow: 0 0 12px -2px var(--accent-glow);
}
[data-theme='neon'] .size-range::-moz-range-thumb {
  background: var(--accent);
  border: none;
  box-shadow: 0 0 12px -2px var(--accent-glow);
}
[data-theme='neon'] .theme-btn,
[data-theme='neon'] .font-btn,
[data-theme='neon'] .reset-btn {
  background: var(--panel-2);
  border: 1px solid var(--border-subtle);
}
[data-theme='neon'] .theme-btn.active,
[data-theme='neon'] .font-btn.active {
  border-color: var(--accent);
  box-shadow: 0 0 18px -6px var(--accent-glow);
}
[data-theme='neon'] .toggle {
  background: #0c1318;
  border: 1px solid rgba(47, 224, 200, 0.3);
  box-shadow: inset 0 0 8px -4px var(--teal-glow);
}
[data-theme='neon'] .toggle.on {
  background: var(--accent);
  border-color: var(--accent);
  box-shadow: 0 0 16px -4px var(--accent-glow);
}
[data-theme='neon'] .toggle-knob {
  background: #f4eae8;
}

/* v1.63.13: «Сетевой доктор» */
.net-doctor {
  margin-top: 10px;
}
.doc-head-actions {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.doc-run-btn {
  font-size: 11px;
  padding: 4px 12px;
}
.doc-steps {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-top: 10px;
}
.doc-step {
  display: flex;
  align-items: baseline;
  gap: 8px;
  font-size: 12px;
  line-height: 1.45;
}
.doc-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex: none;
  align-self: center;
  background: #5b6470;
}
.doc-step.st-ok .doc-dot {
  background: #22c55e;
}
.doc-step.st-warn .doc-dot {
  background: #f59e0b;
}
.doc-step.st-fail .doc-dot {
  background: #ef4444;
}
.doc-name {
  font-family: 'JetBrains Mono', ui-monospace, monospace;
  font-size: 11px;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  flex: none;
  min-width: 150px;
}
.doc-detail {
  flex: 1;
}
.doc-step.doc-verdict {
  margin-top: 6px;
  padding-top: 8px;
  border-top: 1px dashed rgba(128, 128, 128, 0.3);
}
.doc-step.doc-verdict .doc-name,
.doc-step.doc-verdict .doc-detail {
  font-weight: 600;
}
.doc-step.doc-verdict.st-ok .doc-detail {
  color: #22c55e;
}
.doc-step.doc-verdict.st-warn .doc-detail {
  color: #f59e0b;
}
.doc-step.doc-verdict.st-fail .doc-detail {
  color: #ef4444;
}

</style>
