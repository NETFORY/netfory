<script setup lang="ts">
/* eslint-disable no-undef */
import { computed, ref, reactive, watch, nextTick, onMounted, onBeforeUnmount } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useUiStore, NARROW_PX } from '@/store/ui'
import SitePanel from '@/components/SitePanel.vue'
import { useAuthStore } from '@/store/auth'
import { canSeeFileExchange } from '@/lib/access'
import { useTabsStore, type Tab } from '@/store/tabs'
import { useAppsStore } from '@/store/apps'
import { useWalletStore } from '@/store/wallet'
import { useDiscoveryStore } from '@/store/discovery'
import { useDevhubStore } from '@/store/devhub'
import { useToastsStore } from '@/store/toasts'
import { useMailBadgeStore } from '@/store/mailBadge'
import { useSeedersStatsStore } from '@/store/seedersStats'
import { useTerminalStore } from '@/store/terminal'
import { usePlayerStore } from '@/store/player'
import { shortAddr } from '@/lib/crypto'
import { startSeederReplication, stopSeederReplication, startProfileReplication } from '@/lib/seederRep'
import {
  isTauri,
  openAppWindow,
  openAppWindowNew,
  openEmbeddedWebview,
  setEmbeddedBounds,
  hideEmbeddedWebview,
  closeEmbeddedWebview,
  dappTranslateToggle,
  translatorDappCapable,
  dappTranslateConfig,
  dappHistoryBack,
  dappHistoryForward,
  dappReload,
  openSiteWebview,
  siteNavigate,
  hideSiteWebview,
  closeSiteWebview,
  siteHistoryBack,
  siteHistoryForward,
  siteReload,
  siteRevealDownload,
  appListen,
  openExternal,
  resolveUsername,
  resolveDomain,
  domainOfApp,
  isDeveloper,
  toggleChatWindow,
  messengerAvailable,
  launchMessenger,
  messengerOpen,
  toggleConsoleWindow,
  nodeStatus,
  getProfileAvatar,
  getMyUsername,
  recoverMyUsername,
  albumOpenLink,
  uiLog,
  webproxyStatus,
  webproxySetTotal,
  myceliumStatus,
  type MyceliumCircuit,
  type NodeStatus,
} from '@/lib/bridge'
import LanguageSwitcher from '@/components/LanguageSwitcher.vue'
import TerminalDrawer from '@/components/TerminalDrawer.vue'
import Identicon from '@/components/Identicon.vue'
import WalletDrawer from '@/wallet/WalletDrawer.vue'
import BottomAudioPlayer from '@/components/BottomAudioPlayer.vue'
import LinkContextMenu from '@/components/LinkContextMenu.vue'
import StartPage from '@/components/StartPage.vue'
import OnboardingIntro from '@/components/OnboardingIntro.vue'
import NodeTelemetryPanel from '@/components/NodeTelemetryPanel.vue'
import DonateModal from '@/components/DonateModal.vue'
import UserProfile from '@/views/UserProfile.vue'
import { useBookmarksStore } from '@/store/bookmarks'
import { installWindowOpenGuard, isExternalHttp, linkTarget, NEWTAB_PREFIX, OPEN_LINK_EVENT } from '@/lib/clearnet'
import { NAV_ITEMS, type NavBadgeSource, type NavGate } from '@/config/nav.config'

const ui = useUiStore()
const auth = useAuthStore()
// Онбординг первого запуска (3 шага): true, пока пользователь не закрыл его.
const showOnboarding = ref(false)
const tabs = useTabsStore()
const apps = useAppsStore()
const wallet = useWalletStore()
const discovery = useDiscoveryStore()
const devhub = useDevhubStore()
const toastsStore = useToastsStore()
const mailBadge = useMailBadgeStore()
const seedersStats = useSeedersStatsStore()
const terminal = useTerminalStore()
const player = usePlayerStore()
const route = useRoute()
const router = useRouter()
const { t, locale } = useI18n()

const addressInput = ref('')
// Перевод страниц DApp: состояние вкл/выкл + прогресс на каждый активный адрес.
const dappTr = reactive<Record<string, boolean>>({})
const dappTrActive = computed(() => !!tabs.activeId && !!dappTr[tabs.activeId])
interface DappTrProgress { total: number; done: number; cache: number; running: boolean; elapsedMs: number }
const dappTrProgress = ref<DappTrProgress | null>(null)
// Перевод DApp: GPU-сборки И CPU-сборки с bonsai переводят локально; обычная
// CPU-сборка работает в режиме «только кэш» (Фаза 3) — переводы приходят из
// P2P-топиков `tr:<lang>`.
const dappTrCacheOnly = ref(false)
translatorDappCapable().then((cap) => { dappTrCacheOnly.value = !cap }).catch(() => {})
const dappTrBadge = computed(() => {
  const p = dappTrProgress.value
  if (!dappTrActive.value || !p || p.total === 0) return ''
  if (p.running) return `${Math.min(99, Math.round((p.done / Math.max(1, p.total)) * 100))}%`
  const cachePct = p.done ? Math.round((p.cache / p.done) * 100) : 0
  return `${cachePct}% · ${(p.elapsedMs / 1000).toFixed(1)}s`
})
const dappTrRunning = computed(() => dappTrActive.value && !!dappTrProgress.value?.running)
// Запоминание авто-перевода на конкретный DApp (localStorage).
const AUTO_TR_KEY = 'sn.dappAutoTranslate'
// v1.135.76: перевод dApp отключён глобально — кнопка скрыта, а сохранённый
// авто-перевод принудительно сбрасывается у всех, у кого был включён.
try { localStorage.removeItem(AUTO_TR_KEY) } catch { /* приватный режим */ }
function loadAutoTr(): Set<string> {
  return new Set() // всегда пусто: авто-перевод выключен навсегда
}
function saveAutoTr(s: Set<string>) { localStorage.setItem(AUTO_TR_KEY, JSON.stringify([...s])) }
async function toggleDappTranslate() {
  const addr = tabs.activeId
  if (!addr) return
  const on = !dappTr[addr]
  dappTr[addr] = on
  if (on) dappTrProgress.value = null
  const set = loadAutoTr()
  if (on) set.add(addr); else set.delete(addr)
  saveAutoTr(set)
  try {
    await dappTranslateToggle(addr, on, String(locale.value || 'en'))
  } catch {
    dappTr[addr] = !on
  }
}
const launchingChat = ref(false)
// ♥ DONATE — модалка поддержки проекта (кнопка в сайдбаре под Мессенджером)
const showDonate = ref(false)
// System Console теперь по умолчанию открывается в отдельном native-окне
// (`?w=syscon`). Плавающая панель убрана — толковее иметь всегда доступную
// консоль в отдельном окне, чтобы её не перекрывал dApp и не тратили ресурс
// на дублирование подписчика `sys-log` в главном окне.
async function openSysConsoleWindow() {
  await toggleConsoleWindow()
}
// security-shield popover (hover or click-pinned)
const shieldHover = ref(false)
const shieldPinned = ref(false)
// active dApp crypto self-test report → drives the STH:// prefix color + shield
const activeReport = computed(() => terminal.reportFor(tabs.activeId))
const prefixClass = computed(() => {
  const r = activeReport.value
  if (!tabs.activeId || !r) return ''
  return r.polyfilled ? 'prefix-polyfill' : 'prefix-native'
})
const shieldSecure = computed(() => !!activeReport.value?.integrity)

// version of an installed/owned app for the [vN] tab badge
function tabVersion(id: string): number | null {
  return apps.apps.find((m) => m.id === id)?.version || null
}

// Тултип бейджа источника установки (☁ cloud seeder / ⇄ P2P)
function sourceTooltip(tab: Tab): string {
  const s = tab.source
  if (!s) return ''
  const when = s.at ? formatInstalledAt(s.at) : ''
  const ver = s.version ? ` · v${s.version}` : ''
  const base =
    s.kind === 'seeder'
      ? t('tabsrc.seeder', { provider: s.provider || 'cloud' })
      : t('tabsrc.p2p')
  return `${base}${ver}${when ? ' · ' + when : ''}`
}

// Backend пишет `at` как строку с Unix-секундами (см. `crypto.rs::now_iso`),
// а раньше вкладка вслепую делала `new Date(s.at)` → «Invalid Date».
// Поддерживаем три формы: number, «число как строка» (секунды или ms),
// либо валидный ISO/RFC-2822 — иначе просто пропускаем часть с датой.
function formatInstalledAt(raw: string | number): string {
  const asNumber = typeof raw === 'number' ? raw : Number(raw)
  let d: Date
  if (Number.isFinite(asNumber) && String(raw).trim() !== '') {
    // >10^12 → уже миллисекунды, иначе секунды
    d = new Date(asNumber > 1e12 ? asNumber : asNumber * 1000)
  } else {
    d = new Date(String(raw))
  }
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleString()
}
const hostRef = ref<HTMLElement | null>(null)
let knownIds: string[] = []
const createdIds = new Set<string>()
let syncing = false
let syncPending = false
let lastEink = false
const loadTimers: Record<string, ReturnType<typeof setTimeout>> = {}
const unlisteners: Array<() => void> = []

// The active dApp shows a "loading from the decentralized network" overlay until
// its injected bridge fires `dapp-loaded` (native) or the iframe loads (web).
const activeLoading = computed(() => !!tabs.activeId && !terminal.isLoaded(tabs.activeId))

function armLoadTimeout(id: string) {
  clearLoadTimeout(id)
  // Safety: reveal the webview even if a dApp never signals load (e.g. no bridge).
  loadTimers[id] = setTimeout(() => {
    terminal.setLoaded(id)
    if (id === tabs.activeId) syncEmbedded()
  }, 12000)
}
function clearLoadTimeout(id: string) {
  if (loadTimers[id]) {
    clearTimeout(loadTimers[id])
    delete loadTimers[id]
  }
}

const nav = computed(() => {
  // Резолверы бейджей/гейтов держатся здесь, а не в конфиге, чтобы конфиг не
  // тянул store-зависимости — так его можно юнит-тестировать и переиспользовать
  // из любых компонентов (напр. мобильного меню в будущем).
  const badgeFor = (src?: NavBadgeSource): number => {
    switch (src) {
      case 'apps.myApps':
        return apps.myApps.length
      case 'discovery.pendingUpdates':
        return discovery.pendingUpdates.length
      case 'apps.installedApps':
        return apps.installedApps.length
      case 'devhub.openPrs':
        return devhub.openPrs
      case 'seeders.sessionReplications':
        return seedersStats.sessionReplications
      default:
        return 0
    }
  }
  const gatePasses = (gate?: NavGate): boolean => {
    if (!gate) return true
    if (gate === 'file-exchange') return canSeeFileExchange(auth.address)
    if (gate === 'developers') return isDeveloper(auth.address)
    return true
  }
  // Пункт виден, только если его `modes` содержит активный UI-режим (по
  // умолчанию USER). Если `modes` не задан — пункт виден во всех режимах.
  const modePasses = (modes?: readonly string[]): boolean => {
    if (!modes || modes.length === 0) return true
    return modes.includes(ui.mode)
  }
  return NAV_ITEMS.filter((n) => gatePasses(n.gate) && modePasses(n.modes)).map((n) => ({
    id: n.id,
    icon: n.icon,
    label:
      ui.mode === 'user' && n.userI18nKey
        ? t(`nav.${n.userI18nKey}`)
        : (n.staticLabel ?? (n.i18nKey ? t(`nav.${n.i18nKey}`) : n.id)),
    badge: badgeFor(n.badgeSource),
    // ⟳-значок доступных обновлений (облако/P2P раздают версию новее локальной)
    updBadge: n.id === 'installed' ? discovery.pendingUpdates.length : 0,
  }))
})

function go(name: string) {
  tabs.goHome()
  router.push({ name })
}

// v1.135.217: если рядом с Netfory лежит ntfry-messenger — чат живёт в отдельном
// приложении (сайдкар, свой endpoint/база); иначе — встроенное окно, как раньше.
const extMessenger = ref<boolean | null>(null)
const extUnread = ref(0)
async function openMessenger(target?: string) {
  if (launchingChat.value) return
  launchingChat.value = true
  // Safety timeout so the spinner never sticks if the window call stalls.
  const to = window.setTimeout(() => { launchingChat.value = false }, 8000)
  try {
    if (extMessenger.value === null) extMessenger.value = await messengerAvailable()
    if (extMessenger.value) {
      try {
        if (target) await messengerOpen(target)
        else await launchMessenger()
        return
      } catch (e) {
        console.warn('ntfry-messenger', e)
        extMessenger.value = false // бинарь не стартовал — встроенный чат
      }
    }
    await toggleChatWindow(target)
  } finally {
    window.clearTimeout(to)
    // brief grace so the new window has time to paint before re-enabling.
    window.setTimeout(() => { launchingChat.value = false }, 600)
  }
}

// ✉ Oxid Mail — запуск отдельного нативного приложения (см. mail_launcher.rs)
const launchingMail = ref(false)
async function openOxidMail() {
  if (launchingMail.value) return
  launchingMail.value = true
  try {
    const core = (window as any).__TAURI__?.core
    if (!core?.invoke) throw new Error('Oxid Mail доступен только в нативном клиенте SmartNet')
    await core.invoke('launch_oxid_mail')
  } catch (e: any) {
    window.alert(typeof e === 'string' ? e : (e?.message || String(e)))
  } finally {
    window.setTimeout(() => { launchingMail.value = false }, 800)
  }
}

async function openAddress() {
  const input = addressInput.value.trim()
  if (!input) return
  // Ввод БЕЗ протокола (просто имя/фраза) — кандидат на встроенный поиск
  // магазина, как омнибокс Chrome. Протокольные ссылки идут по своим веткам.
  const hasProto = /^[a-z][a-z0-9+.-]*:/i.test(input)
  const goStoreSearch = () => {
    addressInput.value = ''
    tabs.goHome()
    router.push({ name: 'discover', query: { q: input } })
  }
  // magnet:?xt=… → перехват торрент-ссылки: открываем страницу Torrents с модалкой.
  const magnet = input.match(/magnet:\?[^\s"']+/i)
  if (magnet) {
    addressInput.value = ''
    router.push({ name: 'torrents', query: { magnet: magnet[0] } })
    tabs.goHome()
    return
  }
  // v1.135.183: https:// → обычный сайт в нативной web-вкладке (websites.rs).
  // Активная web-вкладка навигируется на месте, иначе — новая вкладка.
  // Только https: http:// не открываем (нет TLS — сайт может быть подменён).
  if (/^https:\/\//i.test(input)) {
    addressInput.value = ''
    void openWebsite(input)
    return
  }
  if (/^http:\/\//i.test(input)) {
    toastsStore.push('https://', t('web.httpsOnly'), 'error')
    return
  }
  // api:// → P2P-прокси к узлу через netfory-provider (новая вкладка-страница).
  if (/^api:\/\//i.test(input)) {
    addressInput.value = ''
    tabs.goHome()
    router.push({ name: 'api-view', query: { url: input } })
    return
  }
  // f://<cid> → страница файла (скачивание/покупка), эквивалент sn://file/<cid>.
  if (/^f:\/\//i.test(input)) {
    const cid = input.replace(/^f:\/\//i, '').split(/[/?#]/)[0]
    addressInput.value = ''
    if (cid) {
      tabs.goHome()
      router.push({ name: 'file', params: { cid } })
    }
    return
  }
  // dev:// → децентрализованный DevHub: хаб / профиль автора / репозиторий.
  if (/^dev:\/\//i.test(input)) {
    const rest = input.replace(/^dev:\/\//i, '').replace(/\/+$/, '')
    addressInput.value = ''
    const [user, repo] = rest.split('/')
    tabs.goHome()
    if (user && repo) router.push({ name: 'dev-repo', params: { username: user, repo } })
    else if (user) router.push({ name: 'dev-user', params: { username: user } })
    else router.push({ name: 'dev' })
    return
  }
  // sn:// → native SmartNet system pages (one word = one page). Legacy `u://`
  // aliases (nameservice/domarket/files/myfiles) остаются для обратной
  // совместимости, но НОВЫЕ пути только под sn://, чтобы `u://dashboard`
  // (условное имя пользователя) не перехватывалось системой.
  const lower = input.toLowerCase().replace(/\/+$/, '')
  const snAlias = lower.match(/^sn:\/\/([a-z0-9-]+)$/)?.[1]
  const uAlias = lower.match(/^u:\/\/([a-z0-9-]+)$/)?.[1]
  // devhub → dev-хаб (совместимо с dev://). files ветвится по правам ниже.
  const SN_ROUTES: Record<string, string> = {
    // legacy (sn:// + u://)
    nameservice: 'nameservice',
    domarket: 'domarket',
    myfiles: 'myfiles',
    // системные страницы (sn:// only)
    dashboard: 'dashboard',
    apps: 'apps',
    discover: 'discover',
    installed: 'installed',
    network: 'network',
    keys: 'keys',
    settings: 'settings',
    nodes: 'nodes',
    'ai-computer': 'ai-computer',
    ai: 'ai-computer',
    profile: 'profile',
    torrents: 'torrents',
    music: 'music',
    dev: 'dev',
    devhub: 'dev',
    // Хранилища & Медиа (зонтичная страница: Torrents / MyFiles / Music).
    storages: 'storages',
    storage: 'storages',
  }
  const LEGACY_U_ALIASES = new Set(['nameservice', 'domarket', 'files', 'myfiles'])
  const activeAlias = snAlias || (uAlias && LEGACY_U_ALIASES.has(uAlias) ? uAlias : undefined)
  // sn://wallet → выдвинуть кошелёк на «Получить» (из окна мессенджера: пополнение баланса).
  if (activeAlias === 'wallet') {
    addressInput.value = ''
    wallet.openWallet()
    wallet.setView('receive')
    return
  }
  if (activeAlias === 'files') {
    addressInput.value = ''
    router.push({ name: canSeeFileExchange(auth.address) ? 'files' : 'myfiles' })
    return
  }
  if (activeAlias && SN_ROUTES[activeAlias]) {
    addressInput.value = ''
    tabs.goHome()
    router.push({ name: SN_ROUTES[activeAlias] })
    return
  }
  // sn://file/<cid> — file download / purchase page (service page, not f://)
  const fileMatch = lower.match(/^(?:sn|u):\/\/file\/(.+)$/)
  if (fileMatch) {
    const rawCid = addressInput.value.trim().replace(/^(?:sn|u):\/\/file\//i, '').replace(/\/+$/, '')
    addressInput.value = ''
    tabs.goHome()
    router.push({ name: 'file', params: { cid: rawCid } })
    return
  }
  // sn://album/<id>~<cid>~<node> — album page (fetch manifest by CID if needed).
  if (/^(?:sn|u):\/\/album\//i.test(lower)) {
    const link = input.trim()
    addressInput.value = ''
    try {
      const aid = await albumOpenLink(link)
      router.push({ name: 'album', params: { id: aid } })
    } catch {
      const idPart = link.replace(/^(?:sn|u):\/\/album\//i, '').split('~')[0]
      if (idPart) router.push({ name: 'album', params: { id: idPart } })
    }
    return
  }
  // u://<identity-or-username> → open the user's PUBLIC profile as its own tab
  // (visitor mode), so it doesn't get hidden behind a currently-open dApp webview.
  if (/^u:\/\//i.test(input)) {
    const target = input.replace(/^u:\/\//i, '').trim()
    if (!target) return
    addressInput.value = ''
    // A lowercase [a-z0-9.] token (≤24) is a USERNAME → resolve to its owner.
    if (/^[a-z0-9.]{2,24}$/.test(target)) {
      const addr = await resolveUsername(target.toLowerCase())
      if (!addr) {
        toastsStore.push('u://', `Пользователь с именем «${target}» не найден в реестре имён`, 'error')
        return
      }
      tabs.openProfile(addr, `u://${target}`)
      return
    }
    // Иначе допустимо только S-адресный формат (Base58Check, 26..40 символов).
    if (!/^S[1-9A-HJ-NP-Za-km-z]{25,40}$/.test(target)) {
      toastsStore.push('u://', `Некорректный u://-адрес: ${target.slice(0, 24)}…`, 'error')
      return
    }
    tabs.openProfile(target, `u://${target.slice(0, 10)}…`)
    return
  }
  // Фраза без протокола, не похожая ни на S-адрес приложения, ни на домен
  // (пробелы, произвольные символы) → сразу встроенный поиск магазина.
  if (
    !hasProto &&
    !/^S[1-9A-HJ-NP-Za-km-z]{25,40}$/.test(input) &&
    !/^[a-z0-9-]{1,63}(\.[a-z]{2,16})?$/i.test(input)
  ) {
    goStoreSearch()
    return
  }
  const raw = input.replace(/^sth:\/\//i, '')
  if (!raw) return
  addressInput.value = ''
  // v1.63.6: sth://<APP_ID>/<sub/path>?q → отделяем appId (authority) от
  // deep-link path'а. Раньше весь `raw` шёл на apps.some/resolve —
  // содержавшие «/» ссылки типа sth://addr/trading/PAIR никогда не
  // совпадали ни с одним appId и всегда падали в "APP NOT FOUND".
  const slashAt = raw.search(/[\/?#]/)
  const authority = slashAt === -1 ? raw : raw.slice(0, slashAt)
  const suffix = slashAt === -1 ? '' : raw.slice(slashAt) // includes leading /, ?, #
  // v1.129: прямая ссылка sth://<id>?n=<nodeId>&h=<hash> — параметры установки
  // отделяются от deep-link path'а и уходят в Resolve отдельными query.
  const { deepPath, n: directN, h: directH } = splitShareSuffix(suffix)
  const rawLower = authority.toLowerCase()
  const isExplicitDomain = /^[a-z0-9-]{1,63}\.[a-z]{2,16}$/i.test(authority)
  const looksLikeAppId = /^S[1-9A-HJ-NP-Za-km-z]{25,40}$/.test(authority)
  // bare label (e.g. sth://explorer) → default .sth zone, resolve to its appId.
  const isBareDomain =
    !authority.includes('.') &&
    /^[a-z0-9-]{1,63}$/.test(authority) &&
    !looksLikeAppId &&
    !apps.apps.some((m) => m.id === authority)
  if (isExplicitDomain || isBareDomain) {
    const appId = await resolveDomain(rawLower)
    if (appId) {
      const title = rawLower.replace(/\.sth$/, '')
      if (apps.apps.some((m) => m.id === appId)) {
        tabs.open(appId, title, deepPath || undefined)
      } else {
        tabs.goHome()
        router.push({ name: 'resolve', params: { id: appId }, query: deepPath ? { p: deepPath } : {} })
      }
    } else if (!hasProto) {
      // Имени нет в .sth-реестре и это не протокольная ссылка → поиск магазина.
      goStoreSearch()
    } else {
      // Unknown domain → keep the query visible instead of silently clearing it.
      addressInput.value = input
    }
    return
  }
  // Already have it locally → open directly (с pending deep-link path).
  // Otherwise try to FIND & install it from the decentralized network
  // (share-by-address UX). Deep-link path сохраняется как ?p= query,
  // Resolve.vue после установки открывает вкладку сразу на нужной странице.
  if (apps.apps.some((m) => m.id === authority)) {
    tabs.open(authority, authority, deepPath || undefined)
  } else {
    tabs.goHome()
    router.push({
      name: 'resolve',
      params: { id: authority },
      query: {
        ...(deepPath ? { p: deepPath } : {}),
        ...(directN && directH ? { n: directN, h: directH } : {}),
      },
    })
  }
}

// Авто-перехват: вставка magnet-ссылки в адресную строку сразу открывает модалку.
watch(addressInput, (v) => {
  if (/magnet:\?xt=/i.test(v)) openAddress()
})

async function openAddressWindow() {
  // «В окне»: пустой ввод → открыть АКТИВНУЮ dApp-вкладку отдельным окном.
  let raw = addressInput.value.trim().replace(/^sth:\/\//i, '')
  if (!raw && tabs.activeId) raw = tabs.activeId
  if (!raw) return
  const slashAt = raw.search(/[\/?#]/)
  const authority = slashAt === -1 ? raw : raw.slice(0, slashAt)
  const suffix = slashAt === -1 ? '' : raw.slice(slashAt)
  let appId = authority
  let title = authority
  // sth://<имя> или <имя.зона> → резолвим в appId, как при открытии вкладкой.
  // Раньше имя уходило в окно как есть → dApp-loader не находил такой id → 404.
  const looksLikeAppId = /^S[1-9A-HJ-NP-Za-km-z]{25,40}$/.test(authority)
  const isDomainish =
    !looksLikeAppId &&
    /^[a-z0-9-]{1,63}(\.[a-z]{2,16})?$/i.test(authority) &&
    !apps.apps.some((m) => m.id === authority)
  if (isDomainish) {
    const resolved = await resolveDomain(authority.toLowerCase())
    if (!resolved) {
      addressInput.value = raw // имя не найдено — оставляем ввод видимым
      return
    }
    appId = resolved
    title = authority.toLowerCase().replace(/\.sth$/, '')
  }
  const native = await openAppWindow(appId, title, suffix || undefined)
  if (!native) tabs.open(appId, title, suffix || undefined)
  addressInput.value = ''
}

// ---- Browser-like nav (v1.89.5) --------------------------------------
// Кнопки ← → ↻ слева от адресной строки. Для активной dApp-вкладки
// листаем историю САМОГО dApp (SPA route stack внутри child webview) —
// это ожидаемое поведение «как в браузере». Для не-dApp вкладок работаем
// с историей host-shell (vue-router).
async function navBack() {
  if (isTauri && tabs.activeId && isWebTab.value) {
    try { await siteHistoryBack(tabs.activeId) } catch {}
    return
  }
  if (isTauri && tabs.activeId && isAppTab.value) {
    try { await dappHistoryBack(tabs.activeId) } catch {}
    return
  }
  try { router.back() } catch {}
}
async function navForward() {
  if (isTauri && tabs.activeId && isWebTab.value) {
    try { await siteHistoryForward(tabs.activeId) } catch {}
    return
  }
  if (isTauri && tabs.activeId && isAppTab.value) {
    try { await dappHistoryForward(tabs.activeId) } catch {}
    return
  }
  try { router.forward() } catch {}
}
async function navReload() {
  if (isTauri && tabs.activeId && isWebTab.value) {
    try { await siteReload(tabs.activeId) } catch {}
    return
  }
  if (isTauri && tabs.activeId && isAppTab.value) {
    try { await dappReload(tabs.activeId) } catch {}
    return
  }
  try { window.location.reload() } catch {}
}

// Browser-like URL bar: reflect the active tab's sth:// address, and let the
// user copy the full address with one click (+ transient confirmation).
const addrCopied = ref(false)
// Node telemetry panel (ZeroNet-style) for the active dApp tab. In Tauri the
// native child webview floats above the DOM, so when the panel opens we shrink
// the webview by its width (same trick as the wallet drawer) to reveal it.
const panelOpen = ref(false)
const NTP_WIDTH = 500
const activeManifest = computed(() =>
  tabs.activeId ? apps.apps.find((m) => m.id === tabs.activeId) || null : null,
)
// Profile (u://) tabs render a Vue page instead of a native dApp webview.
const activeTabKind = computed(() => tabs.activeTab?.kind || 'app')
const isAppTab = computed(() => !!tabs.activeId && activeTabKind.value === 'app')
// v1.135.183: web-вкладка (https://-сайт) — нативный webview без Web3-моста.
const isWebTab = computed(() => !!tabs.activeId && activeTabKind.value === 'web')
// v1.135.189: внутренний путь dApp (событие dapp-url из Rust) → sth://name/<path>
const dappBase: Record<string, string> = {}
const dappPath: Record<string, string> = {}
function dappInnerPath(raw: string): string {
  try {
    const u = new URL(raw)
    u.searchParams.delete('sthapp')
    let path = u.pathname.replace(/\/index\.html?$/i, '/')
    if (path === '/') path = ''
    const q = u.searchParams.toString()
    return path + (q ? `?${q}` : '') + (u.hash || '')
  } catch {
    return ''
  }
}
// PQ Web (v1.135.190): host → узел pq-web для активных relay-туннелей → бейдж в адресной строке
const tunnelHosts = reactive<Record<string, string>>({})
// host → 'v6' | 'v4' — семейство исходящего соединения узла (pq-web ≥0.5.0 отвечает «OK v6»)
const tunnelFam = reactive<Record<string, string>>({})
const activeTunnel = computed(() => {
  if (!isWebTab.value || !tabs.activeTab?.url) return null
  let host = ''
  try { host = new URL(tabs.activeTab.url).hostname.toLowerCase() } catch { return null }
  const parts = host.split('.')
  const base = parts.slice(-2).join('.')
  for (const [h, node] of Object.entries(tunnelHosts)) {
    if (h === host || h === base || h.endsWith('.' + base)) return { host: h, node, fam: tunnelFam[h] || '' }
  }
  return null
})
// Mycelium-режим вкладок (1.136.7): host → узел pq-web выхода цепочки → бейдж «Mycelium» + карта
const mushroomHosts = reactive<Record<string, string>>({})
const myceliumOn = ref(false)
const mycCircuits = ref<MyceliumCircuit[]>([])
const activeMushroom = computed(() => {
  if (!myceliumOn.value || !isWebTab.value || !tabs.activeTab?.url) return null
  let host = ''
  try { host = new URL(tabs.activeTab.url).hostname.toLowerCase() } catch { return null }
  const base = host.split('.').slice(-2).join('.')
  for (const [h, node] of Object.entries(mushroomHosts)) {
    if (h === host || h === base || h.endsWith('.' + base)) return { host: h, node }
  }
  return { host, node: '' }
})
// цепочка до узла выхода этого сайта (или любая готовая exit-цепочка пула)
const mycCircuit = computed(() => {
  const node = activeMushroom.value?.node
  const exits = mycCircuits.value.filter((c) => (c.kind ?? 'exit') === 'exit')
  return exits.find((c) => node && c.target === node) || exits.find((c) => !c.degraded) || exits[0] || null
})
async function refreshMycelium() {
  const r = await myceliumStatus()
  myceliumOn.value = !!r?.settings?.enabled
  mycCircuits.value = r?.circuits ?? []
}
let mycTimer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  void refreshMycelium()
  mycTimer = setInterval(() => void refreshMycelium(), 8000)
})
onBeforeUnmount(() => clearInterval(mycTimer))
// хост активной web-вкладки — для панели сайта
const activeWebHost = computed(() => {
  if (!isWebTab.value || !tabs.activeTab?.url) return ''
  try { return new URL(tabs.activeTab.url).hostname.toLowerCase() } catch { return '' }
})

// dVPN (v1.135.197): тумблер «все сайты через pq-web» в адресной строке
const dvpnOn = ref(false)
const dvpnEnabled = ref(false)
const dvpnNodes = ref(0)
async function loadDvpn() {
  try {
    const st = await webproxyStatus()
    dvpnEnabled.value = st.enabled && st.port > 0
    dvpnOn.value = !!st.total
    dvpnNodes.value = st.nodes.filter((n) => n.ok).length
  } catch { /* offline */ }
}
async function toggleDvpn() {
  const next = !dvpnOn.value
  dvpnOn.value = next
  try { await webproxySetTotal(next) } catch { dvpnOn.value = !next }
  if (next && dvpnNodes.value === 0) toastsStore.push('dVPN', t('web.pq.dvpnNoNodes'), 'error')
}
// ★ закладка активной web-вкладки (v1.135.185)
const bookmarks = useBookmarksStore()
const activeWebBookmarked = computed(() => !!(isWebTab.value && tabs.activeTab?.url && bookmarks.has(tabs.activeTab.url)))
function toggleBookmark() {
  const tab = tabs.activeTab
  if (!tab?.url) return
  const added = bookmarks.toggle(tab.url, tab.title)
  toastsStore.push('★ ' + webHost(tab), t(added ? 'web.bookmarkSaved' : 'web.bookmarkRemoved'), added ? 'in' : 'info', 3500)
}
const webIds = new Set<string>()
/** Новая вкладка — стартовая страница SmartNet (поиск dApps / u:// / сайтов, запасной Google). */
function openNewTab() {
  tabs.openStart(t('start.tabTitle'))
}
/** Выбор на стартовой странице: прогоняем через омнибокс (sth:// / u:// / https:// / фраза),
 *  затем закрываем стартовую вкладку — как переход со страницы новой вкладки в Chrome. */
async function onStartOpen(startId: string, input: string) {
  addressInput.value = input
  await openAddress()
  tabs.close(startId)
}
function onGlobalKeydown(e: KeyboardEvent) {
  if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 't') {
    e.preventDefault()
    openNewTab()
  }
}
onMounted(() => window.addEventListener('keydown', onGlobalKeydown))
onBeforeUnmount(() => window.removeEventListener('keydown', onGlobalKeydown))

// Узкий экран (телефон): боковое меню автоматически свёрнуто, раскрытое — поверх контента
const narrowMq = typeof window !== 'undefined' ? window.matchMedia(`(max-width: ${NARROW_PX}px)`) : null
const onNarrow = () => ui.setNarrow(!!narrowMq?.matches)
onMounted(() => {
  onNarrow()
  narrowMq?.addEventListener('change', onNarrow)
})
onBeforeUnmount(() => narrowMq?.removeEventListener('change', onNarrow))
watch(() => router.currentRoute.value.fullPath, () => ui.collapseIfNarrow())
/** Открыть https://-URL: в активной web-вкладке (на месте) или новой вкладкой. */
async function openWebsite(url: string, newTab = false) {
  let u: URL
  try { u = new URL(url) } catch { toastsStore.push('https://', t('web.badUrl'), 'error'); return }
  if (u.protocol !== 'https:') { toastsStore.push('https://', t('web.httpsOnly'), 'error'); return }
  const href = u.toString()
  if (!newTab && tabs.activeId && isWebTab.value) {
    // повторный Enter на тот же адрес во время загрузки — не дёргаем WebView2 ещё одной навигацией
    const cur = tabs.tabs.find((x) => x.id === tabs.activeId)
    if (cur?.loading && cur.url === href) return
    tabs.setWebNav(tabs.activeId, href, true)
    addressInput.value = href
    if (isTauri && createdIds.has(tabs.activeId)) {
      try { await siteNavigate(tabs.activeId, href) } catch (e) { console.warn('siteNavigate:', e) }
    }
    return
  }
  const id = tabs.openWeb(href)
  webIds.add(id)
  addressInput.value = href
  if (!isTauri) tabs.setWebNav(id, href, false) // web fallback: события site-nav не придут
}
/** Домен web-вкладки для favicon/подписи (без www.). */
function webHost(tab: { url?: string }): string {
  try { return new URL(tab.url || '').hostname.replace(/^www\./, '') } catch { return '' }
}
function webFavicon(tab: { url?: string }): string {
  try { return new URL(tab.url || '').origin + '/favicon.ico' } catch { return '' }
}
const activeSeeds = computed(
  () => discovery.apps.find((a) => a.id === tabs.activeId)?.seeds ?? 0,
)
const activePeerIds = computed(
  () =>
    discovery.apps
      .find((a) => a.id === tabs.activeId)
      ?.providers.map((p) => p.nodeId) ?? [],
)
// Index-aligned with activePeerIds — the socket addr of each provider (from the
// discovery peer list) so the planet can GeoLite2-locate it; "" => hash fallback.
const activePeerAddrs = computed(() => {
  const provs = discovery.apps.find((a) => a.id === tabs.activeId)?.providers ?? []
  return provs.map(
    (p) => discovery.peers.find((pe) => pe.nodeId === p.nodeId)?.addr ?? '',
  )
})
const activeTransfer = computed(() => discovery.installingId === tabs.activeId)

// Globe trigger: live stats tooltip + neon flicker when the peer/seed count rises.
const planetTooltip = computed(
  () =>
    `Сеть dApp\nСидов (хранят dApp): ${activeSeeds.value}\nПиров онлайн: ${discovery.peers.length}` +
    `\nУзел: ${nodeStat.value?.host || '—'}${nodeStat.value?.latencyMs != null ? ` (${nodeStat.value.latencyMs}ms)` : ''}` +
    `\nКлик — телеметрия узла`,
)

// Active-node connection-quality dot (mirrors the Node Pool ping colour).
const nodeStat = ref<NodeStatus | null>(null)
// вспышка в статус-строке после автопереключения узла (8 с)
const nodeSwitchFlash = ref('')
let nodeSwitchTimer: ReturnType<typeof setTimeout> | null = null
let nodeStatTimer: ReturnType<typeof setInterval> | undefined
async function refreshNodeStat() {
  nodeStat.value = await nodeStatus()
}
const nodePingClass = computed(() => {
  const s = nodeStat.value
  if (!s || !s.online || s.latencyMs == null) return 'off'
  if (s.latencyMs < 900) return 'fast'
  if (s.latencyMs < 1800) return 'mid'
  return 'slow'
})
// Passive transport indicator for the status line: only meaningful when the
// active node is a P2P (api://) one. "p2p" = private mesh, "fallback" = the
// P2P node is unreachable and calls are going over the public clearnet pool.
const transportMode = computed(() => nodeStat.value?.transport ?? 'clearnet')
const showTransport = computed(() => transportMode.value === 'p2p' || transportMode.value === 'fallback')
const transportLabel = computed(() =>
  transportMode.value === 'p2p' ? t('wallet.tpP2P') : t('wallet.tpFallback'),
)
const transportTitle = computed(() =>
  transportMode.value === 'p2p' ? t('wallet.tpP2PHint') : t('wallet.tpFallbackHint'),
)
/* Версии в заголовке окна (v1.135.19): Rust ставит "…v<core>" при старте, а
 * UI дописывает версию своего бандла. Совпадают → просто "v<core>"; разошлись
 * (в сборку попал устаревший dist) → "v<core> · UI v<ui> [STALE UI!]" + SYSLOG.
 * Несовпадение ядра и фронта теперь видно прямо в заголовке. */
async function syncTitleVersions() {
  if (!isTauri) return
  try {
    const { getVersion } = await import('@tauri-apps/api/app')
    const { getCurrentWindow } = await import('@tauri-apps/api/window')
    const core = await getVersion()
    const ui = typeof __UI_VERSION__ === 'string' ? __UI_VERSION__ : ''
    const base = `NETFORY // SmartHoldem WEB4 P2P network v${core}`
    if (!ui || ui === core) {
      await getCurrentWindow().setTitle(base)
    } else {
      await getCurrentWindow().setTitle(`${base} · UI v${ui} [STALE UI!]`)
      uiLog(`ВНИМАНИЕ: версия UI-бандла v${ui} не совпадает с ядром v${core} — в сборку попал устаревший dist`)
    }
  } catch {
    /* noop: web-режим или старое API */
  }
}


// Открытие ссылки из мессенджера/других виджетов через адресную строку
// (event 'netfory:open-address', detail = url: dev:// api:// sth:// u:// f://).
function onOpenAddressEvent(e: Event) {
  const url = (e as CustomEvent).detail
  if (typeof url !== 'string' || !url.trim()) return
  addressInput.value = url.trim()
  openAddress()
}

onMounted(() => {
  refreshNodeStat()
  nodeStatTimer = setInterval(refreshNodeStat, 30000)
  loadMyAvatar()
  loadMyUsername()
  window.addEventListener('name-confirmed', onNameConfirmedTopbar)
  syncTitleVersions()
  window.addEventListener('netfory:open-address', onOpenAddressEvent as EventListener)
  // Первый запуск: дружелюбный 3-шаговый онбординг (показывается один раз).
  try {
    if (!localStorage.getItem('sth_onboarding_seen')) showOnboarding.value = true
  } catch {
    /* noop */
  }
  startSeederReplication()
  startProfileReplication()
  window.addEventListener('avatar-updated', loadMyAvatar)
  window.addEventListener('seeder-replicated', onSeederReplicated as EventListener)
  // Обработчик OS-deep-link'ов sth:// — открывает dApp в отдельном окне.
  // Регистрируется в App.vue при получении события `deep-link` из Tauri.
  window.addEventListener('sth-open', onSthOpen as EventListener)
  window.addEventListener('api-open', onApiOpen as EventListener)
  // Deep link, пришедший ДО монтирования AppLayout (холодный старт /
  // заблокированный кошелёк) — App.vue буферизует его на window.
  const pendSth = (window as any).__sthPendingOpen
  if (typeof pendSth === 'string') {
    delete (window as any).__sthPendingOpen
    void onSthOpen(new CustomEvent('sth-open', { detail: pendSth }))
  }
  const pendApi = (window as any).__apiPendingOpen
  if (typeof pendApi === 'string') {
    delete (window as any).__apiPendingOpen
    onApiOpen(new CustomEvent('api-open', { detail: pendApi }))
  }
})
onBeforeUnmount(() => {
  if (nodeStatTimer) clearInterval(nodeStatTimer)
  stopSeederReplication()
  window.removeEventListener('avatar-updated', loadMyAvatar)
  window.removeEventListener('name-confirmed', onNameConfirmedTopbar)
  window.removeEventListener('seeder-replicated', onSeederReplicated as EventListener)
  window.removeEventListener('sth-open', onSthOpen as EventListener)
  window.removeEventListener('api-open', onApiOpen as EventListener)
  window.removeEventListener('netfory:open-address', onOpenAddressEvent as EventListener)
})

// Фоновая репликация Cloud Seeder завершилась — глобальный тост (иначе
// пользователь не видит, что обновлённый dApp уже улетел в /ntfryseed).
function onSeederReplicated(e: CustomEvent) {
  const d = e.detail || {}
  // Инкремент счётчика в сайдбаре: бейдж «⇈ N» = живой heartbeat фонового
  // сидера в этой сессии, даже если пользователь пропустил всплывающий тост.
  seedersStats.tickReplicated()
  const parts = [
    d.dapps != null ? `${d.dapps} dApps` : '',
    d.archives ? `${d.archives} архивов` : '',
  ].filter(Boolean)
  toastsStore.push(
    '⇈ SEEDER',
    `Реплицировано в облако${parts.length ? ': ' + parts.join(' · ') : ''} → /ntfryseed`,
    'info',
    5000,
    () => {
      // Клик по тосту → переход на страницу Cloud Seeders. Активная dApp-вкладка
      // (native webview) перекрывает Vue-роут, поэтому сначала goHome().
      tabs.goHome()
      router.push({ name: 'seeders' })
    },
  )
}

// Разбор хвоста sth://-ссылки: deep-link path + параметры прямой установки
// n (nodeId издателя) / h (хэш tar-блоба) из самодостаточной share-ссылки.
function splitShareSuffix(suffix: string): { deepPath: string; n: string; h: string } {
  let n = ''
  let h = ''
  let clean = suffix
  const qPos = suffix.indexOf('?')
  if (qPos !== -1) {
    const hashPos = suffix.indexOf('#', qPos)
    const qs = new URLSearchParams(hashPos === -1 ? suffix.slice(qPos + 1) : suffix.slice(qPos + 1, hashPos))
    n = qs.get('n') || ''
    h = qs.get('h') || ''
    if (n || h) {
      qs.delete('n')
      qs.delete('h')
      const rest = qs.toString()
      clean = suffix.slice(0, qPos) + (rest ? '?' + rest : '') + (hashPos === -1 ? '' : suffix.slice(hashPos))
    }
  }
  return { deepPath: clean.replace(/^\//, ''), n, h }
}

// sth://<APP_ID>[/path?query] пришёл от ОС (клик по ссылке в браузере /
// deep-link при повторном запуске). Открываем dApp в отдельном native-окне
// через уже существующую команду `open_app_webview`. Извлекаем только
// authority (APP_ID) — path/query dApp обработает сам, они попадут в URL
// webview'а через нашу sth:// scheme-регистрацию.
async function onSthOpen(e: Event) {
  delete (window as any).__sthPendingOpen
  const url = (e as CustomEvent).detail as string
  if (typeof url !== 'string' || !url.startsWith('sth://')) return
  const rest = url.slice(6)
  const slashAt = rest.search(/[\/?#]/)
  const appId = slashAt === -1 ? rest : rest.slice(0, slashAt)
  const suffix = slashAt === -1 ? '' : rest.slice(slashAt)
  if (!appId) return
  // v1.129: dApp не установлен → ведём на Resolve (с n/h из прямой ссылки),
  // а не в пустое 404-окно sth://-схемы.
  if (!apps.apps.some((m) => m.id === appId)) {
    const share = splitShareSuffix(suffix)
    tabs.goHome()
    router.push({
      name: 'resolve',
      params: { id: appId },
      query: {
        ...(share.deepPath ? { p: share.deepPath } : {}),
        ...(share.n && share.h ? { n: share.n, h: share.h } : {}),
      },
    })
    return
  }
  const deepPath = suffix.replace(/^\//, '')
  try {
    // v1.63.6: пробрасываем path (напр. `trading/XBTSX.STH_XBTSX.USDT`) в
    // окно dApp, чтобы deep-link открывался сразу на нужной странице,
    // а не на корне.
    await openAppWindow(appId, appId, deepPath || undefined)
  } catch (err) {
    console.error('sth:// deep-link open failed', err)
  }
}
// api://<nodeId>/<provider>[/path?query] пришёл от ОС → Netfory tool.
// TODO: интегрировать с Netfory address bar / SystemConsole. Пока просто
// логгируем — dApp'ы обычно ходят в api:// не через ОС-скимы, а
// внутренними fetch'ами, поэтому чаще нужен только sth://.
function onApiOpen(e: Event) {
  delete (window as any).__apiPendingOpen
  const url = (e as CustomEvent).detail as string
  console.info('api:// deep-link received:', url)
}

// Аватар пользователя в топбаре (у кнопки LOCK): загруженный вместо идентикона.
const myAvatar = ref<string | null>(null)
async function loadMyAvatar() {
  myAvatar.value = await getProfileAvatar()
}
watch(() => auth.address, () => loadMyAvatar())
// Зарегистрированное @имя в топбаре (обновляется по `name-confirmed`).
const myUsername = ref<string | null>(null)
async function loadMyUsername() {
  myUsername.value = auth.address ? await getMyUsername() : null
  // sled пуст (свежая установка) — чекаем регистрацию по исходящим tx через iroh
  if (!myUsername.value && auth.address) recoverMyUsername().then((n) => { if (n) myUsername.value = n }).catch(() => {})
}
watch(() => auth.address, () => loadMyUsername())
function onNameConfirmedTopbar(e: Event) {
  const n = (e as CustomEvent).detail
  if (typeof n === 'string' && n) myUsername.value = n
  else void loadMyUsername()
}
const planetFlicker = ref(false)
let flickerT: ReturnType<typeof setTimeout> | undefined
watch(activeSeeds, (n, o) => {
  if (n > (o ?? 0)) {
    planetFlicker.value = true
    if (flickerT) clearTimeout(flickerT)
    flickerT = setTimeout(() => (planetFlicker.value = false), 3500)
  }
})
watch(
  () => tabs.activeId,
  () => {
    panelOpen.value = false
  },
)
async function copyActiveAddress() {
  const v = addressInput.value.trim() || (tabs.activeId || '').trim()
  const link = /^sth:\/\//i.test(v) ? v : `sth://${v.replace(/^sth:\/\//i, '')}`
  if (!v) return
  try {
    await navigator.clipboard.writeText(link)
  } catch {
    /* noop */
  }
  addrCopied.value = true
  setTimeout(() => (addrCopied.value = false), 1500)
}
watch(
  () => tabs.activeId,
  async (id) => {
    if (!id) {
      addressInput.value = ''
      return
    }
    // Prefer the registered .sth domain over the raw appId in the URL bar.
    const kind = tabs.activeTab?.kind || 'app'
    if (kind === 'app') {
      const dom = await domainOfApp(id)
      dappBase[id] = dom ? `sth://${dom.replace(/\.sth$/, '')}` : id
      addressInput.value = dappBase[id] + (dappPath[id] || '')
    } else if (kind === 'web') {
      addressInput.value = tabs.activeTab?.url || ''
    } else if (kind === 'start') {
      addressInput.value = ''
    } else {
      addressInput.value = id
    }
  },
)

// Tauri serves the active app over the sth:// custom protocol via a native
// child webview; Web uses an in-page iframe placeholder.
function webPlaceholder(id: string): string {
  return `<!doctype html><html><head><meta charset="utf-8"><style>
    *{margin:0;font-family:'JetBrains Mono',monospace}
    body{background:#0d0f12;color:#5c7894;height:100vh;display:flex;flex-direction:column;
      align-items:center;justify-content:center;gap:16px;text-align:center}
    .a{color:#d9822b;font-weight: var(--fw-800);font-size:18px;letter-spacing:.08em;word-break:break-all;padding:0 20px}
    .b{font-size:11px;letter-spacing:.16em;text-transform:uppercase}
    .g{position:fixed;inset:0;background-image:linear-gradient(rgba(92,120,148,.06) 1px,transparent 1px),
      linear-gradient(90deg,rgba(92,120,148,.06) 1px,transparent 1px);background-size:34px 34px}
    .c{border:1px solid #232831;padding:28px 36px;background:#14171c;z-index:1}
  </style></head><body><div class="g"></div><div class="c">
    <div class="a">sth://${id}</div>
    <p class="b" style="margin-top:12px">${t('viewer.serving')}</p>
    <p class="b" style="margin-top:6px;color:#3a4250">${t('viewer.webMock')}</p>
  </div></body></html>`
}

async function lock() {
  if (isTauri) {
    for (const tab of tabs.tabs) {
      if (tab.kind === 'web') await closeSiteWebview(tab.id)
      else await closeEmbeddedWebview(tab.id)
    }
  }
  createdIds.clear()
  webIds.clear()
  tabs.closeAll()
  // Счётчик фоновых репликаций — heartbeat текущей сессии; при lock() сбрасываем.
  seedersStats.reset()
  await auth.lock()
}

/* --------- Native embedded child-webview lifecycle (Tauri only) ---------
 * In Tauri the active tab is rendered by a REAL native child webview attached
 * to the main window over `hostRef`. We keep its bounds in sync with the host
 * div, hide inactive tabs, and close webviews whose tab was removed. Web mode
 * keeps the <iframe> fallback (below).
 */
// v1.135.10 (Linux/WebKitGTK/RDP fix): после Vue-обновления DOM ещё не
// обязательно применил CSS/layout — на WebKitGTK через RDP это приводит к
// тому, что `getBoundingClientRect()` возвращает bounds ДО того как sidebar/
// tab-strip заняли свои места, и native child webview оказывается размером
// со всё main-окно. Ждём два rAF (первый — Vue commit, второй — стилевой
// проход/reflow), чтобы гарантировать финальные размеры.
function waitForLayoutStable(): Promise<void> {
  return new Promise((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(() => resolve()))
  })
}

async function syncEmbedded() {
  if (!isTauri) return
  // Serialize: rapid watcher/resize bursts must not create the same webview
  // twice (otherwise Tauri throws "a webview with label ... already exists").
  if (syncing) {
    syncPending = true
    return
  }
  syncing = true
  try {
    await nextTick()
    await waitForLayoutStable()
    const active = tabs.activeId
    const currentIds = tabs.tabs.map((t) => t.id)

    // E-Ink toggled: native webviews can't be re-filtered live, so drop them
    // and let the block below recreate the active one with grayscale injected.
    const eink = ui.theme === 'eink'
    if (eink !== lastEink) {
      lastEink = eink
      for (const id of Array.from(createdIds)) {
        await closeEmbeddedWebview(id)
        createdIds.delete(id)
        clearLoadTimeout(id)
      }
    }

    // close native webviews for tabs that no longer exist
    for (const id of knownIds) {
      if (!currentIds.includes(id)) {
        if (webIds.has(id)) {
          await closeSiteWebview(id)
          webIds.delete(id)
        } else {
          await closeEmbeddedWebview(id)
          terminal.reset(id)
        }
        createdIds.delete(id)
        clearLoadTimeout(id)
      }
    }
    knownIds = currentIds

    // hide every non-active tab webview
    for (const tab of tabs.tabs) {
      if (tab.id === active) continue
      if (tab.kind === 'web') await hideSiteWebview(tab.id)
      else await hideEmbeddedWebview(tab.id)
    }

    // v1.135.183: web-вкладка (https://-сайт) — свой набор команд (label web-*),
    // без загрузочного оверлея: сайт виден сразу, прогресс — в табе.
    if (active && isWebTab.value) {
      const webTab = tabs.activeTab
      if (hostRef.value && webTab?.url && !ui.overlayOpen) {
        const r = hostRef.value.getBoundingClientRect()
        const drawerW = wallet.isWalletOpen ? Math.min(400, r.width) : 0
        // панель сайта (глобус справа) сужает webview — сайт остаётся виден и кликабелен
        const panelW = panelOpen.value ? Math.min(NTP_WIDTH, r.width) : 0
        const w = Math.max(0, r.width - drawerW - panelW)
        const h = Math.max(0, r.height)
        // создаёт webview при первом показе, далее — только bounds + show
        createdIds.add(active)
        webIds.add(active)
        await openSiteWebview(active, webTab.url, r.left, r.top, w, h)
      } else {
        await hideSiteWebview(active)
      }
      return
    }

    // position / create the active tab webview over the host area — unless a
    // Vue overlay/modal is open (native webviews float above the DOM). When the
    // wallet drawer is open we shrink the webview by the drawer width; when the
    // terminal is open we shrink its height to dock the log panel below it.
    // Profile (u://) tabs are pure Vue pages → never spawn a native webview.
    if (active && isAppTab.value && hostRef.value && !ui.overlayOpen) {
      const r = hostRef.value.getBoundingClientRect()
      const drawerW = wallet.isWalletOpen ? Math.min(400, r.width) : 0
      const panelW = panelOpen.value ? Math.min(NTP_WIDTH, r.width) : 0
      const w = Math.max(0, r.width - drawerW - panelW)
      const termH = terminal.isOpen ? Math.round(r.height * 0.3) : 0
      const h = Math.max(0, r.height - termH)
      // v1.135.10: diagnostic log — bounds of the host div vs window size, чтобы
      // на Linux/RDP (WebKitGTK) видеть, если child webview кладётся не туда
      // и накрывает sidebar/tab-strip.
      try {
        uiLog(
          'sys',
          `sync-embed active=${String(active).slice(0, 12)} host=[${Math.round(r.left)},${Math.round(r.top)} ${Math.round(r.width)}x${Math.round(r.height)}] win=${window.innerWidth}x${window.innerHeight} dpr=${window.devicePixelRatio} draw=${drawerW} panel=${panelW} term=${termH} → wv=[${Math.round(r.left)},${Math.round(r.top)} ${Math.round(w)}x${Math.round(h)}]`,
        )
      } catch {}
      if (!createdIds.has(active)) {
        // Create HIDDEN: the Vue loading overlay stays visible until `dapp-loaded`.
        createdIds.add(active)
        // v1.63.6: если deep-link принёс pendingPath — открываем webview
        // сразу на конкретной подстранице dApp.
        const path = tabs.consumePendingPath(active)
        // v1.90.11: fresh=true — если нативный webview с этим label уцелел от
        // прошлой жизни вкладки (напр. 404 до установки dApp), Rust принудительно
        // перезагрузит его на корень вместо показа устаревшей страницы.
        await openEmbeddedWebview(active, r.left, r.top, w, h, false, ui.theme === 'eink', path, true)
        armLoadTimeout(active)
      } else if (terminal.isLoaded(active)) {
        // Если пришёл новый pendingPath (пользователь перешёл по shared-ссылке
        // на другую страницу того же dApp) — навигируем существующий webview.
        const path = tabs.consumePendingPath(active)
        await openEmbeddedWebview(active, r.left, r.top, w, h, true, ui.theme === 'eink', path)
      } else {
        // created but still loading → keep hidden, just track bounds
        await setEmbeddedBounds(active, r.left, r.top, w, h)
        await hideEmbeddedWebview(active)
      }
    } else if (active) {
      await hideEmbeddedWebview(active)
    }
  } catch (e) {
    // benign during fast tab churn (e.g. label already exists / not found)
    console.warn('syncEmbedded:', e)
  } finally {
    syncing = false
    if (syncPending) {
      syncPending = false
      syncEmbedded()
    }
  }
}

let ro: ResizeObserver | null = null
watch(
  () => [
    tabs.activeId,
    tabs.tabs.length,
    ui.sidebarCollapsed,
    ui.overlayOpen,
    wallet.isWalletOpen,
    panelOpen.value,
    terminal.isOpen,
    ui.theme,
    ui.fontSize,
  ],
  () => syncEmbedded(),
)
// Живая скорость сети (левый сайдбар): rx/tx всех интерфейсов из события
// `net-speed` (Rust-сэмплер, раз в секунду). null → web-fallback (подпись).
const netSpeed = ref<{ rx: number; tx: number } | null>(null)
function fmtBps(bps: number): string {
  if (bps < 1024) return `${bps} B/s`
  if (bps < 1024 * 1024) return `${(bps / 1024).toFixed(bps < 10240 ? 1 : 0)} KB/s`
  return `${(bps / 1048576).toFixed(1)} MB/s`
}
onMounted(() => {
  if (isTauri) {
    if (hostRef.value) {
      ro = new ResizeObserver(() => syncEmbedded())
      ro.observe(hostRef.value)
    }
    window.addEventListener('resize', syncEmbedded)
    // dApp → host event broker: terminal logs, crypto self-test reports, and the
    // page-loaded signal that reveals the native webview (hides the load overlay).
    ;(async () => {
      // Внешние ссылки из dApp-webview'ов (embed-*/app-*): у них нет
      // shell:allow-open, поэтому мост пересылает URL сюда событием.
      // v1.135.225: белого списка больше нет — открываем во web-вкладке NETFORY.
      // Ссылки из ДРУГИХ Tauri-окон (мессенджер): backend open_in_main шлёт
      // targeted-событие сюда — открываем через адресную строку (все схемы).
      unlisteners.push(
        await appListen<string>('netfory:open-address', (url) => {
          if (typeof url !== 'string' || !url.trim()) return
          if (url.startsWith(NEWTAB_PREFIX)) {
            openHttpByPref(url.slice(NEWTAB_PREFIX.length))
            return
          }
          addressInput.value = url.trim()
          openAddress()
        }),
      )
      unlisteners.push(
        await appListen<{ app_id: string; url: string }>('dapp-open-url', (p) => {
          openHttpByPref(String(p?.url || ''))
        }),
      )
      // диалоги сайтов (alert/confirm/prompt) — без нативных модалок WebView2, тостом
      unlisteners.push(
        await appListen<{ tab: string; kind: string; message: string }>('site-dialog', (p) => {
          const host = webHost(tabs.tabs.find((x) => x.id === p?.tab) || {}) || 'site'
          toastsStore.push(host, `${p?.kind === 'alert' ? '' : `[${p?.kind}] `}${p?.message || ''}`.slice(0, 300), 'info')
        }),
      )
      // ссылки http(s) из окна мессенджера (open_link_in_main) — всегда НОВАЯ вкладка
      unlisteners.push(
        await appListen<string>('netfory:open-link', (url) => {
          openHttpByPref(String(url || ''))
        }),
      )
      // ✉ События Oxid Mail (IPC stdin/stdout → mail_launcher.rs):
      // new_mail — тост с кликом «открыть почту»; unread — бейдж на карточке Mail.
      unlisteners.push(
        await appListen<{ type: string; count?: number; from?: string; subject?: string }>('oxid-mail-event', (p) => {
          if (p?.type === 'unread') {
            mailBadge.set(Number(p.count || 0))
            return
          }
          if (p?.type !== 'new_mail') return
          const n = Number(p.count || 0)
          const body = n > 1 ? `Новых писем: ${n}` : `${p.from || ''} — ${p.subject || 'Новое письмо'}`
          toastsStore.push('✉ Oxid Mail', body, 'in', 6000, () => { void openOxidMail() })
        }),
      )
      // v1.63.11: кастомные схемы (sth:// u:// sn:// api:// f:// dev://) из
      // dApp-webview (клик по <a href>, window.open, window.location.href) —
      // мост/on_navigation блокируют навигацию внутри webview и пересылают
      // URL сюда. Открываем через ту же логику, что и адресная строка.
      unlisteners.push(
        await appListen<{ app_id: string; url: string; new_window?: boolean }>('dapp-open-link', (p) => {
          const href = String(p?.url || '')
          if (!/^(sth|u|sn|api|f|dev):\/\//i.test(href)) return
          // Внутренняя ссылка dApp с target="_blank" (или Ctrl/Middle-click)
          // → открываем ВСЕГДА новое отдельное окно с тем же appId + path,
          // не трогая уже открытый инстанс dApp.
          if (p?.new_window && /^sth:\/\//i.test(href)) {
            const raw = href.replace(/^sth:\/\//i, '')
            const slash = raw.search(/[\/?#]/)
            const appId = slash === -1 ? raw : raw.slice(0, slash)
            const rest = slash === -1 ? '' : raw.slice(slash)
            const deep = rest.replace(/^\//, '')
            void openAppWindowNew(appId, appId, deep || undefined)
            return
          }
          addressInput.value = href
          void openAddress()
        }),
      )
      // v1.135.183: web-вкладки (websites.rs) — URL/загрузка, заголовок, ссылки наружу.
      unlisteners.push(
        await appListen<{ tab: string; url: string; loading: boolean }>('site-nav', (p) => {
          if (!p?.tab) return
          tabs.setWebNav(p.tab, String(p.url || ''), !!p.loading)
          if (p.tab === tabs.activeId && p.url) addressInput.value = String(p.url)
          if (!p.loading) terminal.setLoaded(p.tab)
        }),
      )
      unlisteners.push(
        await appListen<{ host: string; route: string; node: string; fam?: string }>('webproxy-route', (p) => {
          if (!p?.host) return
          const h = p.host.toLowerCase()
          if (p.route === 'mycelium') {
            mushroomHosts[h] = p.node
            return
          }
          if (p.route === 'relay' && p.node) {
            tunnelHosts[h] = p.node
            if (p.fam) tunnelFam[h] = p.fam
          } else if (p.route === 'direct') {
            delete tunnelHosts[h]
            delete tunnelFam[h]
          }
        }),
      )
      unlisteners.push(await appListen<{ on: boolean }>('webproxy-total', (p) => { dvpnOn.value = !!p?.on }))
      unlisteners.push(await appListen('webproxy-relays', () => void loadDvpn()))
      void loadDvpn()
      // dApp: внутренний путь (sth://id/path?sthapp=…) → sth://name/path
      unlisteners.push(
        await appListen<{ tab: string; url: string }>('dapp-url', (p) => {
          if (!p?.tab || !p.url) return
          dappPath[p.tab] = dappInnerPath(String(p.url))
          if (p.tab === tabs.activeId && dappBase[p.tab] !== undefined) {
            addressInput.value = dappBase[p.tab] + dappPath[p.tab]
          }
        }),
      )
      // загрузки со https-сайтов: сохраняем в «Загрузки», тост с открытием папки
      unlisteners.push(
        await appListen<{ tab: string; state: string; name: string; path: string; success?: boolean }>('site-download', (p) => {
          if (!p) return
          if (p.state === 'started') {
            toastsStore.push('⬇ ' + (p.name || ''), t('web.downloadStarted'), 'info', 4000)
          } else if (p.success) {
            toastsStore.push('✓ ' + (p.name || ''), t('web.downloadDone'), 'in', 9000, () => {
              void siteRevealDownload(p.path).catch(() => {})
            })
          } else {
            toastsStore.push('✕ ' + (p.name || ''), t('web.downloadFailed'), 'error', 6000)
          }
        }),
      )
      // SPA-переходы (pushState) и фрагменты внутри сайта — полный URL в адресной строке
      unlisteners.push(
        await appListen<{ tab: string; url: string }>('site-url', (p) => {
          if (!p?.tab || !p.url) return
          tabs.setWebUrl(p.tab, String(p.url))
          if (p.tab === tabs.activeId) addressInput.value = String(p.url)
        }),
      )
      unlisteners.push(
        await appListen<{ tab: string; title: string }>('site-title', (p) => {
          if (p?.tab) tabs.setWebTitle(p.tab, String(p.title || ''))
        }),
      )
      unlisteners.push(
        await appListen<{ tab: string; url: string; new_window?: boolean; upgraded?: boolean }>('site-open-link', (p) => {
          const href = String(p?.url || '')
          if (!href) return
          if (/^https:\/\//i.test(href)) {
            // http→https апгрейд идёт в ту же вкладку; window.open — новой вкладкой
            if (p?.upgraded && p.tab) {
              tabs.setWebNav(p.tab, href, true)
              if (p.tab === tabs.activeId) addressInput.value = href
              void siteNavigate(p.tab, href).catch((e) => console.warn('siteNavigate:', e))
            } else {
              void openWebsite(href, true)
            }
            return
          }
          // sth:// sn:// u:// api:// f:// dev:// → та же логика, что и адресная строка
          addressInput.value = href
          void openAddress()
        }),
      )
      unlisteners.push(await appListen<any>('dapp-terminal-log', (p) => terminal.addLog(p)))
      // Суммарная скорость сети для индикатора в сайдбаре.
      unlisteners.push(
        await appListen<{ rxBps: number; txBps: number }>('net-speed', (p) => {
          netSpeed.value = { rx: p?.rxBps || 0, tx: p?.txBps || 0 }
        }),
      )
      // dApp вызвал smartholdem.openWallet() → выезжает панель кошелька.
      unlisteners.push(
        await appListen<{ origin: string }>('dapp-open-wallet', () => {
          wallet.openWallet()
        }),
      )
      unlisteners.push(await appListen<any>('dapp-crypto-report', (p) => terminal.setReport(p)))
      // Wallet/blockchain call auto-fell-back from a stalled api:// P2P node to
      // the clearnet pool. Instead of a (spammy) toast, just refresh the passive
      // transport indicator in the status line — it shows the persistent state.
      // внешний мессенджер: непрочитанные → бейдж в сайдбаре
      unlisteners.push(
        await appListen<{ type: string; total?: number }>('messenger-event', (p) => {
          if (p?.type === 'unread') extUnread.value = p.total ?? 0
          if (p?.type === 'exited') extUnread.value = 0
        }),
      )
      unlisteners.push(
        await appListen<any>('node-p2p-fallback', () => {
          refreshNodeStat()
        }),
      )
      // v1.135.211: автопереключение узла SmartHoldem (Rust) → статус-строка + тост
      unlisteners.push(
        await appListen<{ from: string; to: string; reason: string; p2p: boolean }>('sth-node-switched', (p) => {
          if (!p?.to) return
          refreshNodeStat()
          const short = (h: string) => h.startsWith('api://') ? 'api://' + h.slice(6, 14) + '…' : h.replace(/^https?:\/\//, '')
          nodeSwitchFlash.value = `${short(p.from)} → ${short(p.to)}`
          if (nodeSwitchTimer) clearTimeout(nodeSwitchTimer)
          nodeSwitchTimer = setTimeout(() => (nodeSwitchFlash.value = ''), 8000)
          toastsStore.push(t('topbar.nodeSwitched'), `${short(p.to)} · ${p.reason}`, 'info', 4500)
        }),
      )
      unlisteners.push(
        await appListen<any>('dapp-loaded', (p) => {
          if (!p?.app_id) return
          terminal.setLoaded(p.app_id)
          clearLoadTimeout(p.app_id)
          if (p.app_id === tabs.activeId) syncEmbedded()
          // Настраиваем перевод по выделению (контекстное меню) + язык.
          // На CPU-сборке работает в режиме «только кэш».
          dappTranslateConfig(p.app_id, String(locale.value || 'en'), true, t('apps.translateSelection')).catch(() => {})
          // авто-перевод: если этот DApp был отмечен — включаем перевод сразу
          if (loadAutoTr().has(p.app_id)) {
            dappTr[p.app_id] = true
            dappTranslateToggle(p.app_id, true, String(locale.value || 'en')).catch(() => {})
          }
        }),
      )
      // прогресс перевода страницы DApp → пульсация иконки + бейдж
      unlisteners.push(
        await appListen<DappTrProgress>('dapp-translate-progress', (p) => {
          dappTrProgress.value = p
        }),
      )
      // Фаза 3: в кэш прилетел перевод из P2P-топика `tr:<lang>` → если перевод
      // активного DApp включён, перезапускаем проход, чтобы подтянуть свежие строки.
      unlisteners.push(
        await appListen<{ lang: string }>('translator-cache-updated', () => {
          const addr = tabs.activeId
          if (addr && dappTr[addr]) {
            dappTranslateToggle(addr, true, String(locale.value || 'en')).catch(() => {})
          }
        }),
      )
      // Входящие Pull Request (DevHub): бейдж на пункте DEV HUB + тост.
      devhub.refreshPrCount()
      unlisteners.push(
        await appListen<{ from: string; count: number }>('devhub-pr-received', (p) => {
          devhub.refreshPrCount()
          toastsStore.push('dev://', `Новый Pull Request от @${p?.from || '?'}`, 'in', 6000)
        }),
      )
      // Автор из подписок обновил репозиторий — тост, чтобы не пропустить релиз.
      unlisteners.push(
        await appListen<{ username: string; repo: string; message: string }>('devhub-author-push', (p) => {
          if (!p?.username || !p?.repo) return
          const msg = p.message ? ` — «${p.message}»` : ''
          toastsStore.push('dev://', `@${p.username} обновил ${p.repo}${msg}`, 'in', 6000)
        }),
      )
      // Новый альбом от артиста из подписок — тост с переходом в альбом.
      unlisteners.push(
        await appListen<{ id: string; owner: string; username: string; title: string; paid: boolean }>('music-new-album', (p) => {
          if (!p?.id) return
          const who = p.username ? `@${p.username}` : (p.owner || '').slice(0, 8) + '…'
          const badge = p.paid ? ' 💎' : ''
          toastsStore.push(
            '🎵 Новый альбом',
            `${who} выпустил${badge} «${p.title || 'без названия'}»`,
            'in',
            8000,
            () => router.push({ name: 'album', params: { id: p.id } }),
          )
        }),
      )
    })()
    // Start P2P discovery for the whole authenticated session so published
    // dApp updates are detected (and auto-applied) in the background regardless
    // of which view is open. apps must be loaded first so installed versions
    // are known to the update detector.
    ;(async () => {
      if (!apps.loaded) await apps.init()
      discovery.scan()
    })()
  }
})
onBeforeUnmount(() => {
  ro?.disconnect()
  if (isTauri) window.removeEventListener('resize', syncEmbedded)
  document.removeEventListener('click', onGlobalLinkClick, true)
  document.removeEventListener('contextmenu', onGlobalLinkContext, true)
  window.removeEventListener(OPEN_LINK_EVENT, onOpenLinkEvent as EventListener)
  unlisteners.forEach((u) => u())
})

// v1.135.225: внешние http(s)-ссылки (Vue-страницы, dApp, чат, window.open)
// открываются молча во web-вкладке NETFORY — без белого списка и модалки.
// http:// поднимаем до https:// (web-вкладки — только TLS).
function openExternalHttp(href: string) {
  if (!isExternalHttp(href)) return
  void openWebsite(href.replace(/^http:\/\//i, 'https://'), true)
}
/** Клик/событие по стороннему сайту — по настройке «Browser»: новая web-вкладка или системный браузер. */
function openHttpByPref(href: string) {
  if (!isExternalHttp(href)) return
  if (linkTarget() === 'system') {
    openExternal(href).catch((err) => toastsStore.push('ClearNet', String(err?.message || err), 'error'))
    return
  }
  openExternalHttp(href)
}
function onOpenLinkEvent(e: CustomEvent<string>) {
  openHttpByPref(String(e.detail || ''))
}
window.addEventListener(OPEN_LINK_EVENT, onOpenLinkEvent as EventListener)

function onGlobalLinkClick(e: MouseEvent) {
  const target = e.target as HTMLElement | null
  const a = target?.closest?.('a[href]') as HTMLAnchorElement | null
  if (!a) return
  const href = a.getAttribute('href') || ''
  if (!isExternalHttp(href)) return
  e.preventDefault()
  e.stopPropagation()
  openHttpByPref(href)
}
document.addEventListener('click', onGlobalLinkClick, true)

// ПКМ по внешней ссылке → меню: «во вкладке NETFORY» / «в системном браузере».
const linkCtx = ref<{ x: number; y: number; url: string } | null>(null)
function onGlobalLinkContext(e: MouseEvent) {
  const target = e.target as HTMLElement | null
  const a = target?.closest?.('a[href]') as HTMLAnchorElement | null
  const href = a?.getAttribute('href') || ''
  if (!a || !isExternalHttp(href)) return
  e.preventDefault()
  e.stopPropagation()
  if (!linkCtx.value) ui.openOverlay()
  linkCtx.value = { x: Math.min(e.clientX, window.innerWidth - 380), y: Math.min(e.clientY, window.innerHeight - 120), url: href }
}
function closeLinkCtx() {
  if (!linkCtx.value) return
  linkCtx.value = null
  ui.closeOverlay()
}
function linkCtxOpenTab() {
  const url = linkCtx.value?.url || ''
  closeLinkCtx()
  openExternalHttp(url)
}
function linkCtxOpenSystem() {
  const url = linkCtx.value?.url || ''
  closeLinkCtx()
  openExternal(url).catch((err) => toastsStore.push('ClearNet', String(err?.message || err), 'error'))
}
document.addEventListener('contextmenu', onGlobalLinkContext, true)
// Программный window.open(внешний URL) — туда же, во web-вкладку.
installWindowOpenGuard()
</script>

<template>
  <div class="layout" data-testid="layout-shell">
    <!-- TOP BAR -->
    <header class="topbar" data-testid="topbar">
      <div class="tb-left">
        <span class="prefix" :class="prefixClass" data-testid="sth-prefix">STH://</span>
      </div>
      <div class="tb-center">
        <div class="nav-controls" data-testid="nav-controls">
          <button
            class="nav-btn"
            :title="t('topbar.back') || 'Назад'"
            @click="navBack"
            data-testid="nav-back-btn"
            aria-label="Back"
          >
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M15 18l-6-6 6-6" />
            </svg>
          </button>
          <button
            class="nav-btn"
            :title="t('topbar.forward') || 'Вперёд'"
            @click="navForward"
            data-testid="nav-forward-btn"
            aria-label="Forward"
          >
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M9 6l6 6-6 6" />
            </svg>
          </button>
          <button
            class="nav-btn"
            :title="t('topbar.reload') || 'Обновить'"
            @click="navReload"
            data-testid="nav-reload-btn"
            aria-label="Reload"
          >
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M3 12a9 9 0 0 1 15.5-6.3L21 8" />
              <path d="M21 3v5h-5" />
              <path d="M21 12a9 9 0 0 1-15.5 6.3L3 16" />
              <path d="M3 21v-5h5" />
            </svg>
          </button>
        </div>
        <span
          class="sec-shield"
          :class="{ secure: shieldSecure }"
          @mouseenter="shieldHover = true"
          @mouseleave="shieldHover = false"
          @click="shieldPinned = !shieldPinned"
          data-testid="security-shield"
        >
          <svg viewBox="0 0 24 24" width="15" height="15" aria-hidden="true">
            <path
              d="M12 2 4 5v6c0 5 3.4 8.5 8 11 4.6-2.5 8-6 8-11V5l-8-3z"
              fill="currentColor"
              fill-opacity="0.16"
              stroke="currentColor"
              stroke-width="1.4"
            />
            <path
              v-if="shieldSecure"
              d="M8.5 12.2l2.4 2.4 4.6-4.8"
              fill="none"
              stroke="currentColor"
              stroke-width="1.8"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
          <div
            v-if="shieldHover || shieldPinned"
            class="sec-popover"
            data-testid="security-popover"
          >
            {{
              shieldSecure
                ? t('security.secure')
                : tabs.activeId
                  ? t('security.checking')
                  : t('security.idle')
            }}
          </div>
        </span>
        <input
          v-model="addressInput"
          class="addr-input"
          :placeholder="t('topbar.placeholder')"
          @keyup.enter="openAddress"
          data-testid="address-bar-input"
        />
        <!-- Перевод dApp скрыт (v1.135.76): фича отключена глобально;
             авто-перевод принудительно сброшен у всех (localStorage чистится). -->
        <span v-if="activeMushroom" class="addr-myc-wrap">
          <button
            class="addr-myc"
            :class="{ wait: !mycCircuit }"
            :title="t('web.myc.hint', { host: activeMushroom.host })"
            @click="panelOpen = !panelOpen"
            data-testid="address-mycelium-badge"
          ><i class="myc-dot" />Mycelium</button>
        </span>
        <button
          v-if="isTauri && dvpnEnabled && !myceliumOn"
          class="addr-dvpn"
          :class="{ on: dvpnOn }"
          :title="dvpnOn ? t('web.pq.dvpnOnHint', { n: dvpnNodes }) : t('web.pq.dvpnOffHint')"
          @click="toggleDvpn"
          data-testid="address-dvpn-btn"
        ><i class="dvpn-dot" />dVPN</button>
        <span
          v-if="activeTunnel"
          class="addr-tunnel"
          :class="activeTunnel.fam"
          :title="t(activeTunnel.fam ? 'web.pq.tunnelHintFam' : 'web.pq.tunnelHint', { host: activeTunnel.host, node: activeTunnel.node.slice(0, 10) + '…', fam: activeTunnel.fam === 'v6' ? 'IPv6' : 'IPv4' })"
          data-testid="address-tunnel-badge"
        >⬡ {{ t('web.pq.tunnel') }}<b v-if="activeTunnel.fam" class="addr-tunnel-fam" data-testid="address-tunnel-fam">{{ activeTunnel.fam }}</b></span>
        <button
          v-if="isWebTab && tabs.activeTab?.url"
          class="addr-win bm-star"
          :class="{ on: activeWebBookmarked }"
          :title="activeWebBookmarked ? t('topbar.bookmarkRemove') : t('topbar.bookmarkAdd')"
          @click="toggleBookmark"
          data-testid="address-bookmark-btn"
        >{{ activeWebBookmarked ? '★' : '☆' }}</button>
        <button
          v-if="tabs.activeId"
          class="addr-win"
          :class="{ copied: addrCopied }"
          :title="t('apps.copyAddress')"
          @click="copyActiveAddress"
          data-testid="address-copy-btn"
        >{{ addrCopied ? '✓' : '❐' }}</button>
        <button
          v-if="tabs.activeId"
          class="addr-win term-toggle"
          :class="{ active: terminal.isOpen }"
          :title="t('terminal.open')"
          @click="terminal.toggle()"
          data-testid="terminal-toggle-btn"
        >›_</button>
        <button v-if="!isWebTab" class="addr-win" :title="t('apps.openWindow')" @click="openAddressWindow" data-testid="address-window-btn">⧉</button>
        <button
          v-if="isTauri"
          class="addr-win sysconsole-toggle"
          title="Системная консоль (P2P · DHT) — откроется в отдельном окне"
          @click="openSysConsoleWindow"
          data-testid="sysconsole-toggle-btn"
        >
          <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect x="3" y="4" width="18" height="16" rx="2" />
            <path d="M7 9l3 3-3 3" />
            <path d="M13 15h4" />
          </svg>
        </button>
      </div>
      <div class="tb-right">
        <LanguageSwitcher />
        <!-- ♫ Music — быстрый доступ к плееру из топ-бара во всех режимах. -->
        <button
          class="tb-iconbtn"
          :class="{ active: !tabs.activeId && route.name === 'music' }"
          :title="t('nav.music')"
          @click="go('music')"
          data-testid="topbar-music"
          aria-label="Music"
        >
          <span class="tb-glyph" aria-hidden="true">♫</span>
        </button>
        <!-- ☀/☾ — переключатель DEFAULT dark/light. Виден всегда: с кастомной
             темы клик тоже уводит на DEFAULT (пожелание пользователя v1.105). -->
        <button
          class="tb-iconbtn"
          :title="ui.theme === 'default-light' ? t('topbar.switchDark') : t('topbar.switchLight')"
          @click="ui.toggleDayNight()"
          data-testid="topbar-theme-toggle"
          aria-label="Toggle light/dark theme"
        >
          <span class="tb-glyph" aria-hidden="true">{{ ui.theme === 'default-light' ? '☾' : '☀' }}</span>
        </button>
        <!-- ✉ Oxid Mail — быстрый запуск почтового приложения из топ-бара
             (перед настройками). Бейдж показывает непрочитанные письма. -->
        <button
          class="tb-iconbtn tb-mail"
          :title="t('topbar.mail')"
          :disabled="launchingMail"
          @click="openOxidMail()"
          data-testid="topbar-mail"
          aria-label="Oxid Mail"
        >
          <svg class="tb-glyph-svg" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect x="2" y="4" width="20" height="16" rx="2" />
            <path d="m22 7-8.97 5.7a1.94 1.94 0 0 1-2.06 0L2 7" />
          </svg>
          <span
            v-if="mailBadge.unread > 0"
            class="tb-mail-unread"
            data-testid="topbar-mail-badge"
          >{{ mailBadge.unread > 99 ? '99+' : mailBadge.unread }}</span>
        </button>
        <!-- ⚙ Settings — единственная точка входа в настройки (sidebar-пункт
             удалён в v1.63.1 во всех режимах, включая GEEK). Иконка в
             топбаре обеспечивает быстрый доступ независимо от режима. -->
        <button
          class="tb-iconbtn"
          :class="{ active: !tabs.activeId && route.name === 'settings' }"
          :title="t('nav.settings')"
          @click="go('settings')"
          data-testid="topbar-settings"
          aria-label="Settings"
        >
          <span class="tb-glyph" aria-hidden="true">⚙</span>
        </button>
        <button
          class="node-addr"
          data-testid="topbar-node-address"
          :title="auth.address"
          @click="wallet.toggleWallet()"
        >
          <svg class="wallet-ic" viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M21 12V7H5a2 2 0 0 1 0-4h14v4" />
            <path d="M3 5v14a2 2 0 0 0 2 2h16v-5" />
            <path d="M18 12a2 2 0 0 0 0 4h4v-4Z" />
          </svg>
          {{ shortAddr(auth.address) }}
        </button>
        <button
          class="avatar-btn"
          :class="{ active: !tabs.activeId && route.name === 'profile' }"
          :title="myUsername ? `@${myUsername}` : t('nav.profile')"
          @click="go('profile')"
          data-testid="topbar-avatar"
        >
          <img v-if="myAvatar" :src="myAvatar" class="avatar-img" alt="" />
          <Identicon v-else :seed="auth.address" :size="30" />
        </button>
        <button
          class="btn btn-danger lock-btn"
          @click="lock"
          :title="t('nav.lock')"
          :aria-label="t('nav.lock')"
          data-testid="lock-button"
        >
          <svg
            viewBox="0 0 24 24"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <rect x="4" y="11" width="16" height="10" rx="2" />
            <path d="M8 11V7a4 4 0 0 1 8 0v4" />
          </svg>
        </button>
      </div>
    </header>

    <div class="body">
      <!-- SIDEBAR -->
      <aside class="sidebar" :class="{ collapsed: ui.sidebarCollapsed, narrow: ui.narrow }" data-testid="sidebar">
        <div class="sidebar-top">
          <div class="nf-brand">
            <img
              class="nf-logo"
              src="/netfory_logo.png"
              alt="NETFORY"
              title="NETFORY"
              width="24"
              height="24"
              data-testid="sidebar-logo"
            />
            <span class="nf-wordmark" data-testid="sidebar-wordmark"><span class="nf-net">NET</span><span class="nf-fory">FORY</span></span>
          </div>
          <button
            class="collapse-btn"
            :class="{ collapsed: ui.sidebarCollapsed }"
            @click="ui.toggleSidebar()"
            :title="ui.sidebarCollapsed ? 'Развернуть' : 'Свернуть'"
            :aria-label="ui.sidebarCollapsed ? 'Развернуть меню' : 'Свернуть меню'"
            data-testid="sidebar-toggle"
          >
            <svg class="collapse-ico" viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <polyline points="15 18 9 12 15 6" />
            </svg>
          </button>
        </div>

        <nav class="nav">
          <button
            v-for="item in nav"
            :key="item.id"
            class="nav-item"
            :class="{ active: !tabs.activeId && route.name === item.id }"
            @click="go(item.id)"
            :title="item.label"
            :data-testid="`nav-${item.id}`"
          >
            <span class="nav-icon">{{ item.icon }}</span>
            <span v-if="!ui.sidebarCollapsed" class="nav-label">{{ item.label }}</span>
            <span
              v-if="item.updBadge > 0"
              class="nav-badge upd"
              :class="{ collapsed: ui.sidebarCollapsed }"
              :title="t('installed.updatesAvailable', { n: item.updBadge })"
              :data-testid="`nav-upd-badge-${item.id}`"
            >⟳{{ item.updBadge > 99 ? '99+' : item.updBadge }}</span>
            <span
              v-else-if="item.badge > 0"
              class="nav-badge"
              :class="{ collapsed: ui.sidebarCollapsed }"
              :data-testid="`nav-badge-${item.id}`"
            >{{ item.badge > 99 ? '99+' : item.badge }}</span>
          </button>
        </nav>

        <!-- 🎵 PLAYER — открывает/сворачивает очередь глобального аудио-плеера.
             Docked в нижнем кластере, прямо над блоком МЕССЕНДЖЕР. -->
        <button
          class="player-launch"
          :class="{ collapsed: ui.sidebarCollapsed, active: player.queueOpen }"
          @click="player.toggleQueue()"
          title="Player"
          data-testid="nav-player"
        >
          <span class="player-launch-icon">
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M9 18V5l10-2v13" />
              <circle cx="6" cy="18" r="3" />
              <circle cx="16" cy="16" r="3" />
            </svg>
          </span>
          <span v-if="!ui.sidebarCollapsed" class="player-launch-label">Player</span>
        </button>

        <!-- P2P MESSENGER — opens the native borderless chat window. Docked in
             the lower cluster, between SETTINGS and the bottom status string. -->
        <button
          class="chat-launch"
          :class="{ collapsed: ui.sidebarCollapsed }"
          @click="openMessenger()"
          :disabled="launchingChat"
          :title="t('chat.launch')"
          data-testid="nav-chat"
        >
          <span class="chat-launch-icon" :class="{ spinning: launchingChat }">{{ launchingChat ? '◠' : '▰▰' }}</span>
          <span v-if="!ui.sidebarCollapsed" class="chat-launch-label">{{ launchingChat ? 'Loading...' : t('chat.launch') }}</span>
          <span v-if="extUnread > 0" class="tb-mail-unread chat-ext-unread" data-testid="nav-chat-unread">{{ extUnread > 99 ? '99+' : extUnread }}</span>
        </button>

        <!-- ♥ DONATE — поддержка SmartNet. Docked под блоком МЕССЕНДЖЕР. -->
        <button
          class="donate-launch"
          :class="{ collapsed: ui.sidebarCollapsed }"
          @click="showDonate = true"
          title="Поддержать SmartNet"
          data-testid="nav-donate"
        >
          <span class="donate-launch-icon" aria-hidden="true">♥</span>
          <span v-if="!ui.sidebarCollapsed" class="donate-launch-label">DONATE</span>
        </button>

        <!-- ✉ OXID MAIL — пункт скрыт из сайдбара (v1.122.2): почта запускается
             из Dashboard-карточки Mail либо через api-команду напрямую. -->

        <div v-if="!ui.sidebarCollapsed" class="sidebar-meta" data-testid="sidebar-meta">
          <span
            v-if="showTransport"
            class="sb-transport"
            :class="`tr-${transportMode}`"
            :title="transportTitle"
            data-testid="sidebar-transport"
          >
            <span class="sb-tr-dot"></span>{{ transportLabel }}
          </span>
          <span
            v-if="nodeSwitchFlash"
            class="sb-nodeswitch"
            :title="t('topbar.nodeSwitched')"
            data-testid="sidebar-node-switch"
          >⇄ {{ nodeSwitchFlash }}</span>
          <span v-if="netSpeed" class="sb-netspeed" :title="t('topbar.meta')" data-testid="sidebar-netspeed">
            <span class="ns-down">▼ {{ fmtBps(netSpeed.rx) }}</span>
            <span class="ns-up">▲ {{ fmtBps(netSpeed.tx) }}</span>
          </span>
          <template v-else>{{ t('topbar.meta') }}</template>
        </div>
      </aside>
      <div v-if="ui.narrow && !ui.sidebarCollapsed" class="sb-scrim" @click="ui.collapseIfNarrow()" data-testid="sidebar-scrim" />

      <!-- CONTENT -->
      <main class="content-wrap" data-testid="content-area">
        <!-- TAB STRIP -->
        <div v-if="tabs.tabs.length" class="tab-strip" data-testid="tab-strip">
          <button
            class="tab home-tab"
            :class="{ active: !tabs.activeId }"
            @click="tabs.goHome()"
            data-testid="tab-home"
          >
            ⌂ {{ t('nav.dashboard') }}
          </button>
          <button
            v-for="tab in tabs.tabs"
            :key="tab.id"
            class="tab"
            :class="{ active: tabs.activeId === tab.id }"
            @click="tabs.setActive(tab.id)"
            :data-testid="`tab-${tab.id}`"
          >
            <span v-if="tab.kind === 'start'" class="tab-start-ic" :data-testid="`tab-start-ic-${tab.id}`">⌕</span>
            <template v-else-if="tab.kind === 'web'">
              <span v-if="tab.loading" class="tab-dot tab-web-loading" :title="t('web.loading')" :data-testid="`tab-web-loading-${tab.id}`">●</span>
              <img
                v-else-if="webFavicon(tab)"
                class="tab-favicon"
                :src="webFavicon(tab)"
                :alt="webHost(tab)"
                :title="tab.url"
                :data-testid="`tab-web-favicon-${tab.id}`"
                @error="($event.target as HTMLImageElement).style.visibility = 'hidden'"
              />
            </template>
            <span
              v-else-if="tab.verified === true"
              class="tab-verified"
              :title="t('verify.verified')"
              :data-testid="`tab-verified-${tab.id}`"
              >✓</span
            >
            <span
              v-else-if="tab.verified === false"
              class="tab-unverified"
              :title="tab.verifyReason ? `${t('verify.unverified')} · ${tab.verifyReason}` : t('verify.unverified')"
              :data-testid="`tab-unverified-${tab.id}`"
              >⚠</span
            >
            <span v-else class="tab-dot" :title="t('verify.checking')">●</span>
            <span class="tab-title">{{ tab.title }}</span>
            <span
              v-if="tab.kind === 'app' && tab.source"
              class="tab-src"
              :class="tab.source.kind === 'seeder' ? 'src-cloud' : 'src-p2p'"
              :title="sourceTooltip(tab)"
              :data-testid="`tab-source-${tab.id}`"
              >{{ tab.source.kind === 'seeder' ? '☁' : '⇄' }}</span
            >
            <span
              v-if="tabVersion(tab.id)"
              class="tab-ver"
              :data-testid="`tab-ver-${tab.id}`"
            >[v{{ tabVersion(tab.id) }}]</span>
            <span class="tab-close" @click.stop="tabs.close(tab.id)" :data-testid="`tab-close-${tab.id}`">✕</span>
          </button>
          <!-- «+» как в Chrome: новая web-вкладка со стартовой страницей поиска (Ctrl+T) -->
          <button
            class="tab-new"
            :title="t('web.newTab')"
            @click="openNewTab"
            data-testid="tab-new"
          >+</button>

          <!-- Node telemetry panel trigger: far-right of the tab line, under LOCK -->
          <button
            v-if="tabs.activeId"
            class="panel-trigger"
            :class="{ active: panelOpen, flicker: planetFlicker }"
            :title="isWebTab ? t('site.panelHint') : planetTooltip"
            @click="panelOpen = !panelOpen"
            data-testid="node-panel-toggle-btn"
          >
            <svg class="globe-ic" viewBox="0 0 24 24" width="26" height="26" fill="none" aria-hidden="true">
              <circle cx="12" cy="12" r="9.2" fill="#1e9bf0" stroke="currentColor" stroke-width="1.6" />
              <path d="M3.2 10.5 C6 11 7 9 8.4 9.2 C9.6 9.4 9.4 11 10.6 11.4 C11.8 11.8 12 13.6 11 14.2 C10 14.8 10.4 16.6 9.4 17.2 C8.7 17.6 7.6 20 6.2 19.4" fill="#3ad07a" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" />
              <path d="M13 4.4 C12 5.4 13.4 6.6 14.6 6.4 C16 6.2 16.4 8.4 15.4 9.2 C14.4 10 15.4 11.6 16.8 11.4 C18.4 11.2 19.4 9.6 20.6 10" fill="#3ad07a" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" />
              <path d="M16.4 13 C17.6 13.4 18.2 15.2 17.4 16.4 C16.8 17.4 15 17.4 14.8 18.8" fill="#3ad07a" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" />
            </svg>
            <span class="node-dot" :class="nodePingClass" data-testid="globe-node-dot"></span>
          </button>
        </div>

        <!-- internal client views -->
        <section v-show="!tabs.activeId" class="content" data-testid="router-content">
          <!-- ВАЖНО: без mode="out-in" — при падении/зависании leave-перехода
               тяжёлой страницы (например, Профиль) out-in блокировал монтирование
               следующего view → «пустой Dashboard». -->
          <router-view v-slot="{ Component }">
            <transition name="view">
              <component :is="Component" />
            </transition>
          </router-view>
        </section>

        <!-- Tauri: single host for the active tab's NATIVE child webview (sth://) -->
        <div
          v-if="isTauri"
          v-show="isAppTab || isWebTab"
          ref="hostRef"
          class="tab-host"
          data-testid="native-webview-host"
        >
          <div v-if="activeLoading && isAppTab" class="app-loading" data-testid="app-loading">
            <span class="spinner">⟳</span>
            <span class="app-loading-txt">{{ t('common.loadingApp') }}</span>
            <span class="app-loading-sub">{{ t('common.loadingAppSub') }}</span>
          </div>
          <TerminalDrawer
            v-if="terminal.isOpen && isAppTab"
            :app-id="tabs.activeId"
          />
        </div>

        <!-- Web fallback: per-tab isolated iframe surface -->
        <template v-else>
          <section
            v-for="tab in tabs.tabs.filter((t) => t.kind === 'app')"
            v-show="tabs.activeId === tab.id"
            :key="tab.id"
            class="tab-host"
            :data-testid="`tab-host-${tab.id}`"
          >
            <div v-if="activeLoading && tabs.activeId === tab.id" class="app-loading" data-testid="app-loading">
              <span class="spinner">⟳</span>
              <span class="app-loading-txt">{{ t('common.loadingApp') }}</span>
              <span class="app-loading-sub">{{ t('common.loadingAppSub') }}</span>
            </div>
            <iframe
              :srcdoc="webPlaceholder(tab.id)"
              class="tab-frame"
              sandbox="allow-scripts"
              :title="tab.title"
              @load="terminal.setLoaded(tab.id)"
              :data-testid="`tab-frame-${tab.id}`"
            />
            <TerminalDrawer
              v-if="terminal.isOpen && tabs.activeId === tab.id"
              :app-id="tab.id"
            />
          </section>
        </template>

        <!-- Web fallback (browser mode): https://-сайты доступны только в десктопном приложении -->
        <template v-if="!isTauri">
          <section
            v-for="tab in tabs.tabs.filter((t) => t.kind === 'web')"
            v-show="tabs.activeId === tab.id"
            :key="tab.id"
            class="tab-host web-tab-host"
            :data-testid="`web-tab-${tab.id}`"
          >
            <div class="web-mock">
              <div class="web-mock-url">{{ tab.url }}</div>
              <p class="web-mock-txt">{{ t('web.desktopOnly') }}</p>
            </div>
          </section>
        </template>

        <!-- Start (new tab) pages: SmartNet search, Vue-only -->
        <section
          v-for="tab in tabs.tabs.filter((t) => t.kind === 'start')"
          v-show="tabs.activeId === tab.id"
          :key="tab.id"
          class="tab-host profile-tab-host"
          :data-testid="`start-tab-${tab.id}`"
        >
          <StartPage @open="onStartOpen(tab.id, $event)" />
        </section>

        <!-- Profile (u://) tabs: rendered Vue page in read-only visitor mode -->
        <section
          v-for="tab in tabs.tabs.filter((t) => t.kind === 'profile')"
          v-show="tabs.activeId === tab.id"
          :key="tab.id"
          class="tab-host profile-tab-host"
          :data-testid="`profile-tab-${tab.id}`"
        >
          <UserProfile :identity-override="tab.identity" force-visitor />
        </section>
      </main>

      <!-- Node telemetry panel (heavy-door slide) for the active dApp tab -->
      <NodeTelemetryPanel
        v-if="isAppTab"
        :open="panelOpen"
        :manifest="activeManifest"
        :address="tabs.activeId"
        :seeds="activeSeeds"
        :peers-total="discovery.peers.length"
        :peer-ids="activePeerIds"
        :peer-addrs="activePeerAddrs"
        :transfer-active="activeTransfer"
        @close="panelOpen = false"
      />
      <SitePanel
        v-if="isWebTab"
        :open="panelOpen"
        :host="activeWebHost"
        :node="activeMushroom?.node || ''"
        @close="panelOpen = false"
        @reload="navReload"
      />
    </div>

    <!-- Global bottom audio player (Web3 music) — persistent across routes -->
    <BottomAudioPlayer />

    <!-- Global right-side wallet drawer (thin-client widget) -->
    <WalletDrawer />

    <!-- Global ClearNet egress warning (CRT/glitch) -->
    <LinkContextMenu v-if="linkCtx" :x="linkCtx.x" :y="linkCtx.y" :url="linkCtx.url" @tab="linkCtxOpenTab" @system="linkCtxOpenSystem" @close="closeLinkCtx" />

    <DonateModal v-if="showDonate" @close="showDonate = false" />

    <!-- Первый запуск: экран знакомства для новичков (3 шага) -->
    <OnboardingIntro v-if="showOnboarding" @close="showOnboarding = false" />
  </div>
</template>

<style scoped>
.layout {
  display: flex;
  flex-direction: column;
  height: 100vh;
  overflow: hidden;
}
.topbar {
  height: 50px;
  min-height: 50px;
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 0 16px;
  background: var(--bg);
  border-bottom: 1px solid var(--border-subtle);
  z-index: 30;
}
.prefix {
  color: var(--accent);
  font-weight: var(--fw-800);
  font-size: 15px;
  letter-spacing: 0.06em;
  transition: color 0.2s ease, text-shadow 0.2s ease;
}
.prefix.prefix-native {
  color: #22c55e;
  text-shadow: 0 0 10px rgba(34, 197, 94, 0.45);
}
.prefix.prefix-polyfill {
  color: #00e5ff;
  text-shadow: 0 0 10px rgba(0, 229, 255, 0.45);
}
.sec-shield {
  position: relative;
  display: flex;
  align-items: center;
  color: var(--text-dim);
  cursor: pointer;
  transition: color 0.18s ease;
}
.sec-shield.secure {
  color: #22c55e;
}
.sec-shield:hover {
  color: var(--text);
}
.sec-shield.secure:hover {
  color: #34d399;
}
.sec-popover {
  position: absolute;
  top: calc(100% + 10px);
  left: -6px;
  width: 280px;
  z-index: 50;
  padding: 10px 12px;
  font-size: 11px;
  line-height: 1.5;
  letter-spacing: 0.02em;
  color: var(--text);
  background: rgba(10, 15, 29, 0.97);
  border: 1px solid rgba(34, 197, 94, 0.4);
  border-radius: 8px;
  box-shadow: 0 14px 30px rgba(0, 0, 0, 0.5);
}
.term-toggle.active {
  color: #00e5ff;
  border-color: rgba(0, 229, 255, 0.5);
  background: rgba(0, 229, 255, 0.08);
}
.sysconsole-toggle {
  display: inline-flex;
  align-items: center;
  line-height: 0;
}
.sysconsole-toggle.active {
  color: #00e5ff;
  border-color: rgba(0, 229, 255, 0.5);
  background: rgba(0, 229, 255, 0.08);
}
.info-toggle {
  font-size: 13px;
  line-height: 1;
}
.info-toggle.active {
  color: #00ffc8;
  border-color: rgba(0, 255, 200, 0.5);
  background: rgba(0, 255, 200, 0.1);
}
/* Telemetry panel trigger — pinned to the far right of the tab line (under LOCK) */
.panel-trigger {
  margin-left: auto;
  align-self: center;
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  line-height: 0;
  color: var(--text-dim);
  padding: 2px;
  border: none;
  background: transparent;
  position: relative;
  border-radius: 50%;
  transition: color 0.16s ease, transform 0.2s ease;
}
.globe-ic {
  display: block;
  width: 26px;
  height: 26px;
  filter: drop-shadow(0 0 3px rgba(0, 255, 200, 0.25));
  transition: transform 0.5s cubic-bezier(0.22, 1, 0.36, 1), filter 0.3s ease;
}
/* metallic shine sweep on hover */
.panel-trigger::after {
  content: '';
  position: absolute;
  inset: -3px;
  border-radius: 50%;
  background: linear-gradient(
    120deg,
    transparent 32%,
    rgba(255, 255, 255, 0.65) 48%,
    rgba(180, 255, 235, 0.4) 54%,
    transparent 70%
  );
  background-size: 230% 230%;
  background-position: 160% 0;
  opacity: 0;
  pointer-events: none;
  mix-blend-mode: screen;
}
.panel-trigger:hover::after {
  opacity: 1;
  animation: globe-shine 0.85s ease;
}
@keyframes globe-shine {
  from {
    background-position: 160% 0;
  }
  to {
    background-position: -60% 0;
  }
}
.panel-trigger:hover .globe-ic {
  transform: rotate(-18deg) scale(1.14);
  filter: drop-shadow(0 0 7px rgba(0, 255, 200, 0.75));
}
.panel-trigger.active .globe-ic {
  transform: rotate(8deg) scale(1.08);
  filter: drop-shadow(0 0 6px rgba(0, 255, 200, 0.6));
}
.panel-trigger:hover {
  color: #00ffc8;
}
.node-dot {
  position: absolute;
  right: -1px;
  bottom: -1px;
  width: 9px;
  height: 9px;
  border-radius: 50%;
  border: 1.5px solid var(--bg, #060a10);
  background: #64748b;
  pointer-events: none;
}
.node-dot.fast {
  background: #34d399;
  box-shadow: 0 0 6px rgba(52, 211, 153, 0.8);
}
.node-dot.mid {
  background: #fbbf24;
  box-shadow: 0 0 6px rgba(251, 191, 36, 0.7);
}
.node-dot.slow {
  background: #fb923c;
  box-shadow: 0 0 6px rgba(251, 146, 60, 0.7);
}
.node-dot.off {
  background: #64748b;
}
.panel-trigger.active {
  color: #00ffc8;
}
/* neon flicker for ~3.5s when the dApp gains a new storage peer */
.panel-trigger.flicker .globe-ic {
  animation: globe-neon 0.45s steps(2, end) 7;
}
@keyframes globe-neon {
  0%,
  100% {
    filter: drop-shadow(0 0 4px rgba(0, 255, 200, 0.6)) brightness(1);
  }
  30% {
    filter: drop-shadow(0 0 13px rgba(0, 255, 200, 1)) brightness(1.55);
  }
  50% {
    filter: drop-shadow(0 0 2px rgba(0, 255, 200, 0.3)) brightness(0.8);
  }
  70% {
    filter: drop-shadow(0 0 15px rgba(90, 255, 220, 1)) brightness(1.6);
  }
}
.tb-center {
  flex: 1;
  display: flex;
  align-items: center;
  gap: 8px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  padding: 0 8px 0 12px;
  height: 32px;
}
.tb-center:focus-within {
  border-color: var(--border-accent);
}
.addr-arrow {
  font-weight: var(--fw-700);
}
.addr-input {
  flex: 1;
  background: none;
  border: none;
  outline: none;
  font-size: 12px;
  color: var(--text);
}
.addr-input::placeholder {
  color: var(--text-dim);
}
.addr-win {
  color: var(--text-dim);
  padding: 2px 6px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  transition: color 0.16s ease, border-color 0.16s ease;
}
.addr-win:hover {
  color: var(--accent);
  border-color: var(--border-accent);
}
/* Browser-like Back / Forward / Reload — leftmost inside address bar */
.nav-controls {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  margin-right: 8px;
  padding-right: 8px;
  border-right: 1px solid var(--border);
}
.nav-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  padding: 0;
  color: var(--text-dim);
  background: transparent;
  border: none;
  border-radius: 999px;
  cursor: pointer;
  transition: color 0.14s ease, background 0.14s ease;
}
.nav-btn:hover {
  color: var(--text);
  background: rgba(255, 255, 255, 0.06);
}
.nav-btn:active {
  color: var(--accent);
  background: rgba(217, 130, 43, 0.14);
}
.addr-win.bm-star.on { color: #f5b301; border-color: #f5b301; }
.addr-dvpn { display: inline-flex; align-items: center; gap: 6px; padding: 3px 9px; font-size: 9px; letter-spacing: 0.16em; font-weight: var(--fw-800); color: var(--text-dim); border: 1px solid var(--border); border-radius: var(--radius); background: transparent; cursor: pointer; white-space: nowrap; transition: color 0.16s ease, border-color 0.16s ease, box-shadow 0.16s ease; }
.addr-dvpn .dvpn-dot { width: 7px; height: 7px; border-radius: 50%; background: #55606d; display: inline-block; transition: background 0.16s ease, box-shadow 0.16s ease; }
.addr-dvpn:hover { color: var(--text); border-color: var(--text-dim); }
.addr-dvpn.on { color: #2ee6a6; border-color: #2ee6a6; box-shadow: 0 0 12px rgba(46, 230, 166, 0.35); }
.addr-dvpn.on .dvpn-dot { background: #2ee6a6; box-shadow: 0 0 8px #2ee6a6; animation: dvpn-pulse 1.8s ease-in-out infinite; }
@keyframes dvpn-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.45; } }
.addr-myc-wrap { position: relative; display: inline-flex; }
.addr-myc { display: inline-flex; align-items: center; gap: 6px; padding: 3px 9px; font: inherit; font-size: 9px; letter-spacing: 0.14em; text-transform: uppercase; font-weight: var(--fw-800); color: #2ee6a6; background: transparent; border: 1px solid #2ee6a6; box-shadow: 0 0 10px rgba(46, 230, 166, 0.3); cursor: pointer; white-space: nowrap; transition: background-color 0.15s ease, box-shadow 0.15s ease; }
.addr-myc:hover { background: rgba(46, 230, 166, 0.1); }
.addr-myc.wait { color: #f5c451; border-color: #f5c451; box-shadow: 0 0 10px rgba(245, 196, 81, 0.3); }
.myc-dot { width: 6px; height: 6px; border-radius: 50%; background: currentColor; animation: mycPulse 1.6s ease-in-out infinite; }
@keyframes mycPulse { 50% { opacity: 0.35; } }
.addr-tunnel { display: inline-flex; align-items: center; gap: 4px; padding: 3px 8px; font-size: 9px; letter-spacing: 0.14em; font-weight: var(--fw-800); color: #c084fc; border: 1px solid #c084fc; box-shadow: 0 0 10px rgba(192, 132, 252, 0.35); white-space: nowrap; }
.addr-tunnel-fam { margin-left: 2px; padding: 0 4px; font-size: 8px; letter-spacing: 0.08em; border-radius: 2px; background: rgba(192, 132, 252, 0.18); color: #e9d5ff; }
.addr-tunnel.v6 .addr-tunnel-fam { background: rgba(45, 212, 191, 0.2); color: #5eead4; }
.addr-win.copied {
  color: #2ecc71;
  border-color: #2ecc71;
}
.addr-tr.active {
  color: #00e5ff;
  border-color: rgba(0, 229, 255, 0.55);
  background: rgba(0, 229, 255, 0.1);
  box-shadow: 0 0 8px rgba(0, 229, 255, 0.35);
}
.addr-tr {
  display: inline-flex;
  align-items: center;
  gap: 5px;
}
.addr-tr.pulsing {
  animation: addr-tr-pulse 1.1s ease-in-out infinite;
}
@keyframes addr-tr-pulse {
  0%, 100% { box-shadow: 0 0 4px rgba(0, 229, 255, 0.3); opacity: 0.75; }
  50% { box-shadow: 0 0 14px rgba(0, 229, 255, 0.7); opacity: 1; }
}
.addr-tr-badge {
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.02em;
  color: #00e5ff;
  font-variant-numeric: tabular-nums;
}
.addr-tr.disabled {
  color: #555b66;
  opacity: 0.5;
  cursor: not-allowed;
  filter: grayscale(1);
  box-shadow: none;
}
/* Фаза 3: режим «только кэш» на CPU-сборке — приглушённый фиолетовый оттенок. */
.addr-tr.cache-only.active {
  color: #b085ff;
  border-color: rgba(176, 133, 255, 0.5);
  background: rgba(176, 133, 255, 0.1);
  box-shadow: 0 0 8px rgba(176, 133, 255, 0.3);
}
.tb-right {
  display: flex;
  align-items: center;
  gap: 12px;
}
/* Топ-бар: единый стиль icon-кнопок (Music, Settings). Не отвлекают
   свечением до наведения — важнее чтобы взгляд оставался на адрес-баре. */
.tb-iconbtn {
  width: 34px;
  height: 34px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: transparent;
  color: var(--text-dim);
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease, transform 0.15s ease;
}
.tb-iconbtn:hover { color: var(--text); border-color: var(--accent); background: light-dark(rgba(0,0,0,0.04), rgba(255,255,255,0.04)); transform: translateY(-1px); }
.tb-iconbtn.active { color: var(--accent); border-color: var(--accent); background: rgba(176,133,255,0.10); }
.tb-iconbtn:disabled { opacity: 0.55; cursor: wait; }
.tb-glyph { font-size: 16px; line-height: 1; }
/* ✉ Почта в топ-баре: относительное позиционирование под бейдж непрочитанных */
.tb-iconbtn.tb-mail { position: relative; }
.tb-glyph-svg { display: block; }
.chat-ext-unread { margin-left: auto; }
.tb-mail-unread {
  position: absolute;
  top: -5px;
  right: -6px;
  min-width: 15px;
  height: 15px;
  padding: 0 4px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 999px;
  background: var(--accent);
  color: var(--bg);
  font-size: 9px;
  font-weight: 700;
  line-height: 1;
}
.node-addr {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  color: var(--secondary);
  letter-spacing: 0.05em;
  padding: 5px 9px;
  border: 1px dashed var(--border);
  border-radius: var(--radius);
  cursor: pointer;
  transition: color 0.16s ease, border-color 0.16s ease;
}
.node-addr .wallet-ic {
  flex-shrink: 0;
  color: var(--accent);
  opacity: 0.95;
}
.node-addr:hover {
  color: var(--accent);
  border-color: var(--border-accent);
}
.lock-btn {
  padding: 7px 10px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  line-height: 0;
}
.avatar-btn {
  padding: 2px;
  border: 1px solid transparent;
  border-radius: 9px;
  background: transparent;
  cursor: pointer;
  line-height: 0;
  transition: border-color 0.2s ease, box-shadow 0.2s ease;
}
.avatar-btn:hover {
  border-color: var(--accent);
  box-shadow: 0 0 10px -2px var(--accent);
}
.avatar-btn.active {
  border-color: var(--accent);
}
.avatar-img {
  width: 30px;
  height: 30px;
  border-radius: 7px;
  object-fit: cover;
  display: block;
}
.body {
  flex: 1;
  display: flex;
  overflow: hidden;
  position: relative;
}
.sidebar {
  width: 200px;
  background: var(--bg);
  border-right: 1px solid var(--border-subtle);
  display: flex;
  flex-direction: column;
  padding: 12px 10px;
  transition: width 0.22s cubic-bezier(0.16, 1, 0.3, 1);
}
.sidebar.collapsed {
  width: 62px;
}
/* узкий экран: раскрытое меню не сжимает контент, а ложится поверх (в потоке остаются 62px) */
.sidebar.narrow:not(.collapsed) {
  position: relative;
  z-index: 60;
  margin-right: -138px;
  box-shadow: 12px 0 32px rgba(0, 0, 0, 0.45);
}
.sb-scrim {
  position: absolute;
  inset: 0;
  z-index: 59;
  background: rgba(0, 0, 0, 0.35);
  animation: sb-scrim-in 0.18s ease;
}
@keyframes sb-scrim-in { from { opacity: 0; } }
.sidebar-top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-bottom: 14px;
}
.sidebar.collapsed .sidebar-top {
  justify-content: center;
}
/* NETFORY logo: 24×24 raster mark (public/netfory_logo.png) with a 10px
 * gap to the wordmark; hover state gently lifts the icon. */
.nf-brand {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}
.sidebar.collapsed .nf-brand {
  display: none;
}
.nf-wordmark {
  font-size: 17px;
  font-weight: 800;
  letter-spacing: 0.08em;
  line-height: 1;
  white-space: nowrap;
}
/* В light-темах белый «NET» не читается на белом фоне — переключаем на
   тёмный текст и затемнённый бирюзовый через light-dark() (color-scheme). */
.nf-net {
  color: light-dark(#1b1e26, #ffffff);
}
.nf-fory {
  color: light-dark(#0e7490, #00f0ff);
}
.nf-logo {
  display: block;
  width: 24px;
  height: 24px;
  flex: none;
  object-fit: contain;
  cursor: pointer;
  filter: drop-shadow(0 0 4px rgba(0, 240, 255, 0.25));
  transition: transform 0.25s ease, filter 0.25s ease;
}
.nf-logo:hover {
  transform: translateY(-1px) scale(1.04);
  filter: drop-shadow(0 0 8px rgba(0, 240, 255, 0.55));
}
.sidebar.collapsed .nf-logo {
  display: none;
}
.collapse-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 19px;
  height: 19px;
  flex: none;
  color: var(--accent);
  border: 1px solid var(--border-accent);
  border-radius: 6px;
  background: linear-gradient(180deg, rgba(47, 214, 192, 0.12), rgba(47, 214, 192, 0.04));
  cursor: pointer;
  box-shadow: 0 0 0 0 var(--accent-glow, rgba(47, 214, 192, 0.4));
  transition: color 0.16s ease, border-color 0.16s ease, background-color 0.16s ease, transform 0.16s ease, box-shadow 0.16s ease;
}
.collapse-btn:hover {
  color: var(--bg);
  background: var(--accent);
  border-color: var(--accent);
  transform: translateY(-1px);
  box-shadow: 0 4px 18px -4px var(--accent-glow, rgba(47, 214, 192, 0.6));
}
.collapse-btn:active {
  transform: translateY(0);
}
.collapse-ico {
  transition: transform 0.22s ease;
}
.collapse-btn.collapsed {
  align-self: center;
}
.collapse-btn.collapsed .collapse-ico {
  transform: rotate(180deg);
}
.nav {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.nav-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 11px 12px;
  border: 1px solid transparent;
  border-radius: var(--radius);
  color: var(--text-dim);
  font-size: 12px;
  font-weight: var(--fw-600);
  letter-spacing: 0.05em;
  transition: color 0.16s ease, background-color 0.16s ease, border-color 0.16s ease;
  position: relative;
}
.nav-label {
  white-space: nowrap;
}
.nav-item:hover {
  color: var(--text);
  background: var(--panel-2);
}
.nav-item.active {
  color: var(--text);
  background: transparent;
  border-color: transparent;
}
.nav-item.active::before {
  content: '';
  position: absolute;
  left: 0;
  top: 6px;
  bottom: 6px;
  width: 3px;
  border-radius: 2px;
  background: var(--secondary);
}
.nav-icon {
  font-size: 14px;
  width: 18px;
  text-align: center;
}
/* Счётчик-бейдж DApps в пунктах меню (published / installed / updates) */
.nav-badge {
  margin-left: auto;
  min-width: 18px;
  height: 18px;
  padding: 0 5px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  font-size: 10px;
  font-weight: var(--fw-700);
  line-height: 1;
  color: var(--secondary);
  background: var(--accent-soft);
  border: 1px solid var(--border-accent);
  border-radius: 999px;
}
/* ⟳-значок обновлений: тёплый акцент, чтобы отличался от обычного счётчика */
.nav-badge.upd {
  color: #1a1305;
  background: #fbbf24;
  border-color: #fbbf24;
}
.nav-badge.collapsed {
  position: absolute;
  top: 4px;
  right: 4px;
  margin-left: 0;
  min-width: 15px;
  height: 15px;
  padding: 0 3px;
  font-size: 8.5px;
}
.collapsed .nav-item {
  justify-content: center;
}
.sidebar-meta {
  margin-top: 0;
  padding: 12px 8px 4px;
  font-size: 9px;
  line-height: 1.5;
  color: var(--secondary);
  letter-spacing: 0.06em;
  border-top: 1px solid var(--border);
}
/* Живая скорость сети ↓/↑ вместо статической подписи */
.sb-netspeed {
  display: flex;
  gap: 10px;
  font-family: var(--font-mono, ui-monospace, monospace);
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.04em;
  white-space: nowrap;
}
.sb-netspeed .ns-down {
  color: #34d399;
}
.sb-netspeed .ns-up {
  color: #fbbf24;
}
.sb-nodeswitch { display: inline-flex; align-items: center; gap: 4px; padding: 2px 6px; font-size: 9px; letter-spacing: 0.08em; color: #fbbf24; border: 1px solid rgba(251, 191, 36, 0.5); border-radius: 3px; white-space: nowrap; max-width: 100%; overflow: hidden; text-overflow: ellipsis; animation: sb-flash 0.6s ease-out; }
@keyframes sb-flash { from { opacity: 0; transform: translateY(2px); } to { opacity: 1; transform: none; } }
.sb-transport {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  margin-bottom: 6px;
  padding: 3px 8px;
  border-radius: 999px;
  border: 1px solid currentColor;
  font-size: 9px;
  font-weight: 800;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  cursor: help;
}
.sb-transport .sb-tr-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
  box-shadow: 0 0 6px currentColor;
}
.sb-transport.tr-p2p {
  color: #818cf8;
}
.sb-transport.tr-fallback {
  color: #fbbf24;
  animation: sb-tr-pulse 1.8s ease-in-out infinite;
}
@keyframes sb-tr-pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.55; }
}
.chat-launch {
  margin-top: 0;
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 11px 12px;
  background: transparent;
  color: var(--secondary);
  border: none;
  border-top: 1px solid var(--border);
  border-bottom: 1px solid var(--border);
  border-radius: 0;
  cursor: pointer;
  font-size: 12px;
  letter-spacing: 0.08em;
  transition: background-color 0.15s ease, color 0.15s ease;
}
.player-launch {
  margin-top: auto;
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 11px 12px;
  background: transparent;
  color: var(--secondary);
  border: none;
  border-top: 1px solid var(--border);
  border-radius: 0;
  cursor: pointer;
  font-size: 12px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  transition: background-color 0.15s ease, color 0.15s ease;
}
.player-launch:hover,
.player-launch.active {
  background: rgba(0, 229, 255, 0.08);
  color: #00e5ff;
}
.player-launch.collapsed {
  justify-content: center;
  gap: 0;
}
.player-launch-icon {
  color: #00e5ff;
  line-height: 0;
  display: inline-flex;
}
.player-launch-label {
  font-weight: var(--fw-600);
}
.chat-launch:hover {
  background: rgba(56, 249, 215, 0.08);
  color: #38f9d7;
}
/* ♥ DONATE — под Мессенджером, в стилистике нижнего кластера сайдбара */
.donate-launch {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 11px 12px;
  background: transparent;
  color: var(--secondary);
  border: none;
  border-bottom: 1px solid var(--border);
  border-radius: 0;
  cursor: pointer;
  font-size: 12px;
  letter-spacing: 0.08em;
  transition: background-color 0.15s ease, color 0.15s ease;
}
.donate-launch:hover {
  background: rgba(67, 233, 123, 0.08);
  color: #43e97b;
}
.donate-launch.collapsed {
  justify-content: center;
  gap: 0;
}
.donate-launch-icon {
  color: #43e97b;
  font-size: 12px;
  line-height: 1;
  animation: donate-pulse 1.5s ease-in-out infinite;
}
@keyframes donate-pulse {
  0%, 100% { transform: scale(1); }
  50% { transform: scale(1.25); }
}
.donate-launch-label {
  font-weight: var(--fw-600);
}
/* ✉ ПОЧТА — запуск Oxid Mail, в стилистике нижнего кластера сайдбара */
.mail-launch {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 11px 12px;
  background: transparent;
  color: var(--secondary);
  border: none;
  border-bottom: 1px solid var(--border);
  border-radius: 0;
  cursor: pointer;
  font-size: 12px;
  letter-spacing: 0.08em;
  transition: background-color 0.15s ease, color 0.15s ease;
}
.mail-launch:hover {
  background: rgba(226, 88, 34, 0.1);
  color: #e25822;
}
.mail-launch.collapsed {
  justify-content: center;
  gap: 0;
}
.mail-launch:disabled {
  opacity: 0.7;
  cursor: default;
}
.mail-launch-icon {
  color: #e25822;
  font-size: 12px;
  line-height: 1;
}
.mail-launch-label {
  font-weight: var(--fw-600);
}
.chat-launch.collapsed {
  justify-content: center;
  gap: 0;
}
.chat-launch-icon {
  font-size: 11px;
  letter-spacing: -1px;
  color: #38f9d7;
}
.chat-launch-icon.spinning {
  display: inline-block;
  animation: chat-spin 0.8s linear infinite;
}
@keyframes chat-spin { to { transform: rotate(360deg); } }
.chat-launch:disabled { opacity: 0.7; cursor: default; }
.chat-launch-label {
  font-weight: var(--fw-600);
  text-transform: uppercase;
}
.content-wrap {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.tab-strip {
  display: flex;
  align-items: stretch;
  gap: 2px;
  padding: 6px 10px 0;
  background: var(--bg);
  border-bottom: 1px solid var(--border-subtle);
  overflow-x: auto;
}
.tab-start-ic { color: var(--accent, #2dd4bf); font-size: 13px; line-height: 1; }
.tab-new {
  flex: none;
  align-self: center;
  width: 26px;
  height: 26px;
  margin-left: 2px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  background: transparent;
  border: 1px solid transparent;
  border-radius: 50%;
  color: var(--text-dim);
  font-size: 18px;
  line-height: 1;
  cursor: pointer;
  transition: color 0.16s ease, background-color 0.16s ease, border-color 0.16s ease;
}
.tab-new:hover {
  color: var(--text);
  background: var(--bg-elevated);
  border-color: var(--border);
}
.tab {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  max-width: 220px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-bottom: none;
  border-radius: var(--radius) var(--radius) 0 0;
  color: var(--text-dim);
  font-size: 11px;
  letter-spacing: 0.04em;
  transition: color 0.16s ease, background-color 0.16s ease;
}
.tab:hover {
  color: var(--text);
}
.tab.active {
  color: var(--accent);
  background: var(--bg);
  border-color: var(--border-accent);
}
.home-tab {
  font-weight: var(--fw-700);
}
.tab-dot {
  font-size: 7px;
  color: var(--success);
}
.tab-verified {
  font-size: 11px;
  font-weight: var(--fw-800);
  color: var(--success);
}
.tab-unverified {
  font-size: 11px;
  font-weight: var(--fw-800);
  color: var(--danger);
}
.tab-title {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 130px;
}
.tab-close {
  font-size: 10px;
  opacity: 0.6;
  padding: 0 2px;
}
.tab-ver {
  font-size: 9px;
  font-weight: var(--fw-700);
  color: var(--secondary);
  letter-spacing: 0.03em;
  flex-shrink: 0;
}
.tab-src {
  font-size: 10px;
  flex-shrink: 0;
  cursor: help;
}
.src-cloud {
  color: var(--accent);
}
.src-p2p {
  color: var(--success);
  opacity: 0.75;
}
.tab-close:hover {
  opacity: 1;
  color: var(--danger);
}
.content {
  flex: 1;
  overflow-y: auto;
  padding: 28px 32px;
  position: relative;
}
.tab-host {
  flex: 1;
  overflow: hidden;
  background: var(--bg);
  position: relative;
}
.web-tab-host { display: flex; align-items: center; justify-content: center; }
.web-mock { text-align: center; padding: 28px 36px; border: 1px solid var(--border, #232831); background: rgba(20, 23, 28, 0.6); max-width: 560px; }
.web-mock-url { color: var(--accent); font-weight: var(--fw-800); font-size: 14px; word-break: break-all; }
.web-mock-txt { margin-top: 10px; font-size: 11px; letter-spacing: 0.12em; text-transform: uppercase; color: #5c7894; }
.tab-favicon { width: 13px; height: 13px; flex: none; object-fit: contain; }
.tab-web-loading { animation: web-pulse 0.9s ease-in-out infinite; color: var(--accent); }
@keyframes web-pulse { 0%, 100% { opacity: 0.25; } 50% { opacity: 1; } }
.profile-tab-host {
  overflow-y: auto;
}
.app-loading {
  position: absolute;
  inset: 0;
  z-index: 5;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 14px;
  background: var(--bg);
  color: var(--accent);
}
.app-loading .spinner {
  font-size: 28px;
  animation: app-spin 1s linear infinite;
}
.app-loading-txt {
  font-size: 11px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.app-loading-sub {
  font-size: 10px;
  line-height: 1.5;
  letter-spacing: 0.04em;
  color: var(--secondary);
  max-width: 320px;
  text-align: center;
  text-transform: none;
}
@keyframes app-spin {
  to {
    transform: rotate(360deg);
  }
}
.tab-frame {
  width: 100%;
  height: 100%;
  border: none;
  display: block;
}
.view-enter-active {
  transition: opacity 0.18s ease, transform 0.18s ease;
}
/* Уходящий view снимается с потока (absolute) — новый монтируется сразу,
   без ожидания завершения leave (см. комментарий у router-view). */
.view-leave-active {
  transition: opacity 0.14s ease;
  position: absolute;
  inset: 28px 32px;
  pointer-events: none;
}
.view-enter-from {
  opacity: 0;
  transform: translateY(8px);
}
.view-leave-to {
  opacity: 0;
}
</style>
