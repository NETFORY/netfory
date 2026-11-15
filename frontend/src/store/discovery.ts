import { defineStore } from 'pinia'
import { useTabsStore } from './tabs'
import {
  isTauri,
  startDiscovery,
  discoveredPeers,
  installApp,
  cloudSeederInstallAny,
  cloudSeederScanApps,
  prefetchApp,
  getAutoUpdate,
  setAutoUpdate,
  verifyApp,
  p2pStatus,
  getStorageLimit,
  setStorageLimit,
  officialLists,
  notifySystem,
  checkUnverifiedAccept,
  payUnverifiedAccept,
  privateReveal,
  PRIVATE_DAPP_PLACEHOLDER,
  PRIVATE_REVEAL_MARKER,
  type PeerInfo,
} from '@/lib/bridge'
import { useAppsStore } from '@/store/apps'
import { useNetStore } from '@/store/net'
import { useUiStore } from '@/store/ui'
import { i18n } from '@/i18n'

/**
 * Anti-spam store policy: the decentralized store search shows ONLY paid apps by
 * default. Flip to `true` to surface every discovered app. Laid in code now but
 * intentionally NOT user-exposed yet — Phase B (tracker payment verification)
 * will populate each app's `paid` flag that `searchApps` filters on.
 */
const SHOW_ALL_APPS = true

export interface DiscoveredApp {
  id: string
  title: string // "" => UI shows "no title"
  description: string // "" => UI shows "no description"
  tags: string[]
  seeds: number // number of distinct peers serving this app
  installed: boolean
  paid: boolean // on-chain publish payment confirmed (tracker-reported)
  boostPool: number // accumulated seeding boost (smarthoshi)
  sizeBytes: number // tracker-reported payload weight (bytes)
  cost: number // dynamic republish cost in whole STH (base + weight)
  providers: { nodeId: string; hash: string }[]
  // P2P update sync: highest version advertised on the network for this app,
  // and the provider that serves it (set when newer than the installed copy).
  netVersion: number
  installedVersion: number
  updateAvailable: boolean
  updateProvider: { nodeId: string; hash: string } | null
  updated: string // ISO date of the installed copy's last publish/update ("" if unknown)
  viaSeeder: boolean // все провайдеры пришли из Cloud Seeder fallback, не из живого P2P
  owner: string // адрес разработчика (owner подписанного манифеста), '' у старых анонсов
  official: boolean // проверено: owner ∈ DEVELOPERS или id ∈ OFFICIAL_DAPPS
  private: boolean // приватный dApp (E2EE) — виден только адресам из списка доступа
  logoHash: string // iroh-hash PNG-логотипа из анонса ('' — нет; лого качается лениво)
  previewHash: string // iroh-hash DAPP Preview 440×180 из анонса ('' — нет)
}

interface DiscoveryState {
  peers: PeerInfo[]
  apps: DiscoveredApp[]
  scanning: boolean
  p2pEnabled: boolean
  nodeId: string
  installingId: string
  installingSeeds: number
  lastError: string
  autoUpdate: boolean
  updatingId: string
  updatedFlash: string
  // Скан облачных дисков Cloud Seeder-ов идёт прямо сейчас (fallback-каталог)
  cloudScanning: boolean
  // Тост «Установлено» после успешной инсталляции dApp: имя приложения плюс
  // источник (`p2p` — прямая P2P-раздача, `seeder` — Cloud Seeder-архив).
  // Пользователь сразу видит, когда сработал анти-цензурный fallback.
  installedFlash: string
  lastInstallSource: '' | 'p2p' | 'seeder'
  // set when an install is blocked by the storage limit (native quota)
  storagePrompt: { id: string; neededBytes: number } | null
  // приватные dApps: результат reveal-проверки по анонсу (id → метаданные/отказ)
  privReveal: Record<string, { allowed: boolean; name: string; description: string; tags: string[]; p: string }>
  // Official Repository: списки доверенных разработчиков и одобренных dApps
  officialDevs: string[]
  officialDapps: string[]
  // DEV/GEEK: показать в каталоге и непроверенные приложения
  showUnverified: boolean
  // Соглашение о непроверенных dApps (NETFORY_UNVERIFIED_ACCEPT): оплачено ли
  // ончейн-согласие текущим адресом, комиссия из .env и статус оплаты.
  unverifiedAccepted: boolean
  unverifiedFeeSth: number
  acceptBusy: boolean
  acceptError: string
}

// ids we've already auto-applied / pre-fetched this session (avoid loops)
const handledUpdates = new Set<string>()
const prefetched = new Set<string>()

// троттлинг фонового скана облачных сидеров (не долбим API Яндекса/Mail.ru)
let lastCloudScanMs = 0
const CLOUD_SCAN_INTERVAL_MS = 120_000


let pollTimer: ReturnType<typeof setInterval> | null = null
// приватные dApps: reveal-проверки в полёте (защита от дублей за цикл опроса)
const revealInflight = new Set<string>()

// ── Last-known-good кэш каталога (как на Dashboard/NetworkMap) ──────────────
// При старте показываем последние найденные приложения из localStorage сразу,
// а живой скан обновляет список в фоне — пользователь не видит пустую страницу,
// пока mDNS/трекеры/облака ищут сидеров.
const APPS_CACHE_KEY = 'sth_discovery_apps'
const APPS_CACHE_MAX = 200
const UNVERIFIED_KEY = 'sth_show_unverified'

function loadAppsCache(): DiscoveredApp[] {
  try {
    const raw = localStorage.getItem(APPS_CACHE_KEY)
    if (!raw) return []
    const list = JSON.parse(raw)
    return Array.isArray(list) ? list : []
  } catch {
    return []
  }
}
function saveAppsCache(apps: DiscoveredApp[]) {
  try {
    localStorage.setItem(APPS_CACHE_KEY, JSON.stringify(apps.slice(0, APPS_CACHE_MAX)))
  } catch {
    /* quota — кэш не критичен */
  }
}

export const useDiscoveryStore = defineStore('discovery', {
  state: (): DiscoveryState => ({
    peers: [],
    apps: [],
    scanning: false,
    p2pEnabled: false,
    nodeId: '',
    installingId: '',
  installingSeeds: 0,
    lastError: '',
    autoUpdate: true,
    updatingId: '',
    updatedFlash: '',
    cloudScanning: false,
    installedFlash: '',
    lastInstallSource: '',
    storagePrompt: null,
    privReveal: {},
    officialDevs: [],
    officialDapps: [],
    showUnverified: (() => {
      try {
        return localStorage.getItem(UNVERIFIED_KEY) === '1'
      } catch {
        return false
      }
    })(),
    unverifiedAccepted: false,
    unverifiedFeeSth: Number((import.meta as any).env?.FEE_ALLOW_ALL_DAPPS ?? 9999) || 9999,
    acceptBusy: false,
    acceptError: '',
  }),

  getters: {
    // installed apps that have a newer signed version available on the network
    pendingUpdates: (s): DiscoveredApp[] => s.apps.filter((a) => a.updateAvailable),
    // Store search results. By design only PAID apps are surfaced in the public
    // store (anti-spam). `SHOW_ALL_APPS` is the override flag — laid in code for
    // a future "show everything" mode but NOT yet wired (Phase B: trackers will
    // stamp each discovered app's `paid` flag, then this getter filters on it).
    searchApps(s): DiscoveredApp[] {
      const showUnv = s.showUnverified && s.unverifiedAccepted && useUiStore().mode !== 'user'
      return s.apps.filter((a) => {
        // Приватные dApps: в s.apps попадают только установленные/участники
        // списка доступа (aggregate отфильтровал) — явное приглашение
        // владельца обходит фильтр Official Repository.
        if (a.private) return true
        if (!SHOW_ALL_APPS && (a as any).paid === false) return false
        // Official Repository: по умолчанию только проверенные dApps.
        // «Показать непроверенные» доступно только в режимах DEV/GEEK и ТОЛЬКО
        // после оплаченного ончейн-согласия NETFORY_UNVERIFIED_ACCEPT;
        // обмен по прямым sn:// ссылкам работает без фильтра.
        return showUnv || a.official
      })
    },
    updateProviderFor:
      (s) =>
      (id: string): { nodeId: string; hash: string } | null =>
        s.apps.find((a) => a.id === id)?.updateProvider || null,
    // number of distinct peer nodes (seeds) we are currently connected to /
    // can reach — NOT the app list (there can be thousands of apps).
    uniqueSeeds: (s): number => {
      const set = new Set<string>()
      for (const p of s.peers) {
        const key = p.nodeId || p.node || p.addr
        if (key) set.add(key)
      }
      return set.size
    },
    // total distinct apps available across all reachable peers
    availableAppCount: (s): number => s.apps.length,
    // приватные dApps, доступные пользователю: fail-closed фильтр aggregate()
    // уже отсеял чужие — остаются установленные или подтверждённые reveal.
    privateApps: (s): DiscoveredApp[] => s.apps.filter((a) => a.private),
    // есть ли пиры из живого P2P (mDNS/трекеры), а не из cloud-fallback
    hasLiveP2P: (s): boolean => s.peers.some((p) => !p.viaSeeder),
  },

  actions: {
    /** Start mDNS discovery + poll peers, aggregating apps (deduplicated). */
    async scan() {
      if (!isTauri) return
      this.hydrateFromCache()
      this.scanning = true
      this.lastError = '' // dismiss any stale install/verify error on manual rescan
      this.autoUpdate = await getAutoUpdate()
      const st = await p2pStatus()
      this.p2pEnabled = st.enabled
      this.nodeId = st.nodeId
      void this.loadOfficial()
      void this.loadUnverifiedAccept()
      await startDiscovery()
      await this.refresh()
      // обычные сидеры недоступны → сразу сканируем облачные диски сидеров
      if (!this.hasLiveP2P) void this.scanCloudSeeders(true)
      if (pollTimer) clearInterval(pollTimer)
      // Idle-friendly: skip polling while the window is hidden (prevents the
      // background async-runtime / WebView2 IPC pressure that froze the UI on
      // low-core Windows/RDP after long idle). Resumes on the next visible tick.
      pollTimer = setInterval(() => {
        const hidden = typeof document !== 'undefined' && document.hidden
        // Rust TCP-probe надёжнее navigator.onLine (Windows/WebKitGTK после
        // сна). В offline пропускаем тик — иначе IPC-вызовы виснут в
        // таймаутах, копятся и морозят весь UI.
        if (!hidden && useNetStore().online) this.refresh()
      }, 4000)
      setTimeout(() => (this.scanning = false), 1500)
    },

    async refresh() {
      this.peers = await discoveredPeers()
      this.aggregate()
      // фоновый цикл: живого P2P нет → периодически опрашиваем облака сидеров
      if (!this.hasLiveP2P) void this.scanCloudSeeders()
      // act on any updates discovered this cycle (auto-apply or pre-stage)
      this.handleUpdates()
    },

    /** Скан облачных дисков всех известных Cloud Seeder-ов: найденные dApps
     *  вливаются в каталог как via-seeder пиры. Вызывается ТОЛЬКО когда
     *  обычные (живые P2P) сидеры недоступны. `force` — ручной SCAN без
     *  троттлинга; фоновый опрос — не чаще раза в 2 минуты. */
    async scanCloudSeeders(force = false) {
      if (!isTauri || this.cloudScanning) return
      const now = Date.now()
      if (!force && now - lastCloudScanMs < CLOUD_SCAN_INTERVAL_MS) return
      lastCloudScanMs = now
      this.cloudScanning = true
      try {
        await cloudSeederScanApps()
        this.peers = await discoveredPeers()
        this.aggregate()
      } catch {
        // облака недоступны — молча ждём следующего цикла
      } finally {
        this.cloudScanning = false
      }
    },

    /** Загрузка списков Official Repository (Sled + .env) и разметка каталога. */
    async loadOfficial() {
      try {
        const o = await officialLists()
        this.officialDevs = o.developers
        this.officialDapps = o.dapps
        this.markOfficial()
      } catch {
        // нативный API недоступен (web) — фильтр не размечается
      }
    },

    /** Reveal-проверка приватного dApp по payload из анонса (кэш по id+payload).
     *  Разрешён → метаданные подставятся на следующем aggregate(). */
    async revealFetch(id: string, payload: string) {
      const cached = this.privReveal[id]
      if ((cached && cached.p === payload) || revealInflight.has(id)) return
      revealInflight.add(id)
      try {
        const r = await privateReveal(id, payload)
        this.privReveal[id] = { ...r, p: payload }
        if (r.allowed) this.aggregate()
      } catch {
        // нативный API недоступен / сессия закрыта — попробуем в следующем цикле
      } finally {
        revealInflight.delete(id)
      }
    },

    /** Пересчитать флаг official у всех приложений каталога. */
    markOfficial() {
      const devs = new Set(this.officialDevs)
      const ids = new Set(this.officialDapps)
      for (const a of this.apps) {
        a.official = ids.has(a.id) || (!!a.owner && devs.has(a.owner))
      }
    },

    toggleShowUnverified(v: boolean) {
      this.showUnverified = v
      try {
        localStorage.setItem(UNVERIFIED_KEY, v ? '1' : '0')
      } catch { /* quota */ }
    },

    /** Проверка ончейн/локального согласия NETFORY_UNVERIFIED_ACCEPT. */
    async loadUnverifiedAccept() {
      try {
        const r = await checkUnverifiedAccept()
        this.unverifiedAccepted = r.accepted
        if (r.feeSth > 0) this.unverifiedFeeSth = r.feeSth
      } catch {
        this.unverifiedAccepted = false
      }
    },

    /** Оплата ончейн-соглашения → разблокировка непроверенных dApps. */
    async acceptUnverified(): Promise<boolean> {
      this.acceptBusy = true
      this.acceptError = ''
      try {
        await payUnverifiedAccept()
        this.unverifiedAccepted = true
        this.toggleShowUnverified(true)
        return true
      } catch (e: any) {
        this.acceptError = typeof e === 'string' ? e : e?.message || 'tx-failed'
        return false
      } finally {
        this.acceptBusy = false
      }
    },

    /** Системное уведомление о НОВЫХ найденных обновлениях (раз на id@version). */
    notifyNewUpdates() {
      const KEY = 'sth_notified_updates'
      const upd = this.pendingUpdates
      if (!upd.length) return
      let seen: string[] = []
      try {
        seen = JSON.parse(localStorage.getItem(KEY) || '[]')
      } catch { /* повреждено — уведомим заново */ }
      const freshKeys = upd
        .map((a) => `${a.id}@${a.netVersion}`)
        .filter((k) => !seen.includes(k))
      if (!freshKeys.length) return
      const names = upd
        .filter((a) => freshKeys.includes(`${a.id}@${a.netVersion}`))
        .map((a) => a.title || a.id)
      const t = i18n.global.t
      void notifySystem(
        t('installed.updateNotifTitle', { n: freshKeys.length }),
        names.join(', '),
      )
      try {
        localStorage.setItem(KEY, JSON.stringify([...seen, ...freshKeys].slice(-100)))
      } catch { /* quota */ }
    },

    /** Мгновенная гидрация каталога из localStorage (до первого живого скана).
     *  Волатильные поля сбрасываются: авто-обновления из устаревшего кэша не
     *  запускаются, `installed` пересчитывается по локальному реестру. */
    hydrateFromCache() {
      if (this.apps.length) return
      const cached = loadAppsCache()
      if (!cached.length) return
      const local = useAppsStore()
      for (const a of cached) {
        a.installed = local.apps.some((m) => m.id === a.id)
        a.updateAvailable = false
        a.updateProvider = null
        a.logoHash = a.logoHash || '' // старый кэш без поля — нормализуем
        a.previewHash = a.previewHash || ''
      }
      this.apps = cached
      this.markOfficial()
    },

    /** Collapse all peers into a unique app list with seed counts + metadata. */
    aggregate() {
      const apps = useAppsStore()
      const byId = new Map<string, DiscoveredApp>()
      for (const peer of this.peers) {
        for (const a of peer.apps) {
          let entry = byId.get(a.id)
          if (!entry) {
            const local = apps.apps.find((m) => m.id === a.id)
            entry = {
              id: a.id,
              title: local?.name || '',
              description: local?.description || '',
              tags: local?.tags || [],
              seeds: 0,
              installed: !!local,
              paid: false,
              boostPool: 0,
              sizeBytes: 0,
              cost: 0,
              providers: [],
              netVersion: 0,
              installedVersion: local?.version || 0,
              updateAvailable: false,
              updateProvider: null,
              updated: local?.updated || '',
              viaSeeder: true,
              owner: (local as any)?.owner || '',
              official: false,
              private: !!(local as any)?.private,
              logoHash: '',
              previewHash: '',
            }
            byId.set(a.id, entry)
          }
          // Приватный dApp: плейсхолдер-имя / reveal-payload в description —
          // метаданные НЕ копируем (зашифрованы), payload проверяем асинхронно.
          const isPriv =
            a.title === PRIVATE_DAPP_PLACEHOLDER ||
            (a.description || '').startsWith(PRIVATE_REVEAL_MARKER)
          if (isPriv) {
            entry.private = true
            if ((a.description || '').startsWith(PRIVATE_REVEAL_MARKER)) {
              void this.revealFetch(a.id, a.description)
            }
          }
          // fill metadata announced over the network if still unknown
          if (!isPriv) {
            if (!entry.title && a.title) entry.title = a.title
            if (!entry.description && a.description) entry.description = a.description
            if (!entry.tags.length && a.tags?.length) entry.tags = a.tags
            if (!entry.logoHash && a.logo) entry.logoHash = a.logo
            if (!entry.previewHash && a.preview) entry.previewHash = a.preview
          }
          if (!entry.updated && a.updated) entry.updated = a.updated
          if (!entry.owner && a.owner) entry.owner = a.owner
          entry.seeds += 1
          if (!peer.viaSeeder) entry.viaSeeder = false
          if (a.paid) entry.paid = true
          if (a.boostPool && a.boostPool > entry.boostPool) entry.boostPool = a.boostPool
          if (a.sizeBytes && a.sizeBytes > entry.sizeBytes) entry.sizeBytes = a.sizeBytes
          if (a.cost && a.cost > entry.cost) entry.cost = a.cost
          if (a.hash && !entry.providers.some((p) => p.nodeId === peer.nodeId)) {
            entry.providers.push({ nodeId: peer.nodeId, hash: a.hash })
          }
          // P2P update detection: a peer serving a strictly newer version than
          // the copy we have installed means the owner published an update.
          // (mDNS omits the merkle for TXT size reasons, so version is the LAN
          // signal — authenticity is gated by verify_app after the fetch. When
          // a merkle IS advertised, e.g. via trackers, require it to differ.)
          //
          // Cloud Seeder fallback: peers advertised via seeders may omit the
          // iroh blob hash (client has no P2P — install runs through the
          // encrypted cloud archive). We MUST still flag such updates and let
          // applyUpdate route them through cloudSeederInstallAny.
          const local = apps.apps.find((m) => m.id === a.id)
          const peerVer = a.version || 0
          if (peerVer > entry.netVersion) entry.netVersion = peerVer
          if (
            local &&
            peerVer > (local.version || 0) &&
            (!a.merkle || a.merkle !== local.merkle)
          ) {
            // prefer the highest-version provider
            if (peerVer >= entry.netVersion) {
              entry.updateAvailable = true
              // Live P2P provider takes precedence; keep null for seeder-only
              // updates so applyUpdate falls back to the cloud archive path.
              if (a.hash && peer.nodeId && !peer.viaSeeder) {
                entry.updateProvider = { nodeId: peer.nodeId, hash: a.hash }
              } else if (!entry.updateProvider) {
                entry.updateProvider = null
              }
            }
          }
        }
      }
      // Приватные dApps: fail-closed — не установлен и не подтверждён
      // reveal-проверкой → скрываем из каталога; участнику подставляем
      // расшифрованные имя/описание/теги.
      const all = Array.from(byId.values()).filter((e) => {
        if (!e.private || e.installed) return true
        const r = this.privReveal[e.id]
        if (!r?.allowed) return false
        if (r.name) e.title = r.name
        if (r.description) e.description = r.description
        if (r.tags.length && !e.tags.length) e.tags = r.tags
        return true
      })
      const fresh = all.sort((a, b) => b.seeds - a.seeds)
      // Сеть ещё ищет сидеров (0 пиров) — держим закэшированный список,
      // не затираем его пустым результатом.
      if (!fresh.length && !this.peers.length && this.apps.length) return
      this.apps = fresh
      this.markOfficial()
      this.notifyNewUpdates()
      if (fresh.length) saveAppsCache(fresh)
    },

    /** Auto-apply (auto mode) or pre-stage (hybrid mode) pending updates. */
    async handleUpdates() {
      for (const a of this.apps) {
        if (!a.updateAvailable) continue
        const key = `${a.id}@${a.netVersion}`
        if (this.autoUpdate) {
          if (handledUpdates.has(key) || this.updatingId) continue
          handledUpdates.add(key)
          const ok = await this.applyUpdate(a.id)
          // on failure (peer offline, download error) drop the marker so the
          // next discovery cycle retries instead of giving up for the session
          if (!ok) handledUpdates.delete(key)
        } else {
          // hybrid: pre-fetch the new blob in the background so an explicit
          // user "Update" is instant; do not apply automatically.
          // Skip prefetch for seeder-only updates (no iroh provider to pull
          // from — the archive path fetches at apply time instead).
          if (!a.updateProvider) continue
          if (prefetched.has(key)) continue
          prefetched.add(key)
          void prefetchApp(a.updateProvider.nodeId, a.updateProvider.hash)
        }
      }
    },

    /** Fetch + apply a published update for an installed app, then verify.
     *  Tries live P2P provider first (if any); falls back to Cloud Seeder
     *  archive so clients with no direct P2P still receive published updates. */
    async applyUpdate(id: string): Promise<boolean> {
      const entry = this.apps.find((a) => a.id === id)
      if (!entry || !entry.updateAvailable) {
        this.lastError = 'no-provider'
        return false
      }
      this.updatingId = id
      this.lastError = ''
      try {
        const provider = entry.updateProvider
        let m: Awaited<ReturnType<typeof installApp>> | null = null
        // 1) Live P2P provider takes precedence when available.
        if (provider && provider.nodeId && provider.hash) {
          try {
            m = await installApp(id, provider.nodeId, provider.hash)
          } catch (e: any) {
            const msg = e?.message || String(e) || 'update-failed'
            const sm = /STORAGE_LIMIT:(\d+)/.exec(msg)
            if (sm) {
              this.storagePrompt = { id, neededBytes: parseInt(sm[1], 10) }
              return false
            }
            // fall through to seeder fallback
            m = null
          }
        }
        // 2) Cloud Seeder archive fallback (works with zero P2P connectivity).
        if (!m) {
          try {
            m = await cloudSeederInstallAny(id)
          } catch (e: any) {
            const msg = e?.message || String(e) || 'update-failed'
            const sm = /STORAGE_LIMIT:(\d+)/.exec(msg)
            if (sm) {
              this.storagePrompt = { id, neededBytes: parseInt(sm[1], 10) }
              return false
            }
            this.lastError = msg
            return false
          }
        }
        if (!m) {
          this.lastError = 'update-failed'
          return false
        }
        // integrity gate: signed merkle + id↔pubkey binding must hold
        const v = await verifyApp(id)
        if (!v.verified) {
          this.lastError = `verify-failed:${v.reason}`
          return false
        }
        const apps = useAppsStore()
        apps.replaceLocal(m)
        // источник мог смениться (P2P ↔ ☁ seeder) — обновляем бейдж вкладки
        useTabsStore().loadSource(id)
        entry.installed = true
        entry.installedVersion = m.version || entry.installedVersion
        entry.updateAvailable = false
        entry.updateProvider = null
        entry.title = m.name || entry.title
        entry.description = m.description || entry.description
        this.updatedFlash = `${m.name || id}::v${m.version || ''}`
        setTimeout(() => (this.updatedFlash = ''), 3000)
        return true
      } finally {
        this.updatingId = ''
      }
    },

    async toggleAutoUpdate(enabled: boolean) {
      this.autoUpdate = enabled
      await setAutoUpdate(enabled)
      // turning auto back on: re-evaluate pending updates immediately
      if (enabled) this.handleUpdates()
    },

    /** Pull a discovered app over iroh-blobs, trying every provider in turn. */
    async install(id: string): Promise<boolean> {
      this.lastError = ''
      this.lastInstallSource = ''
      const entry = this.apps.find((a) => a.id === id)
      const providers = (entry?.providers || []).filter((p) => p.nodeId && p.hash)
      if (!providers.length) {
        // нет живых P2P-провайдеров → пробуем облачные архивы известных сидеров
        return this.installViaSeeder(id)
      }
      this.installingId = id
      this.installingSeeds = providers.length
      try {
        let lastErr = 'install-failed'
        // Resilience: a single seeder with a corrupted/missing blob must not
        // sink the install — iterate over every distinct provider until one
        // serves a verifiable copy. A storage-limit hit is terminal (prompt).
        for (const provider of providers) {
          try {
            const m = await installApp(id, provider.nodeId, provider.hash)
            if (m) {
              const apps = useAppsStore()
              apps.replaceLocal(m)
              if (entry) {
                entry.installed = true
                entry.title = m.name || entry.title
                entry.description = m.description || entry.description
              }
              this.lastInstallSource = 'p2p'
              this.installedFlash = m.name || id
              setTimeout(() => (this.installedFlash = ''), 4000)
              return true
            }
            lastErr = 'install-failed'
          } catch (e: any) {
            const msg = e?.message || String(e) || 'install-failed'
            const sm = /STORAGE_LIMIT:(\d+)/.exec(msg)
            if (sm) {
              this.storagePrompt = { id, neededBytes: parseInt(sm[1], 10) }
              return false
            }
            // broken seeder (NOTFOUND / encode error / timeout) → try the next
            lastErr = msg
          }
        }
        // все P2P-провайдеры мертвы → последний шанс: облачные архивы сидеров
        if (await this.installViaSeeder(id)) return true
        this.lastError = lastErr
        return false
      } finally {
        this.installingId = ''
        this.installingSeeds = 0
      }
    },

    /** Прямая установка по (providers, hash) из direct-ссылки `sth://<id>?n=&h=`
     *  или трекерлесс-DHT-резолва — каталог (mDNS/трекеры) не требуется. */
    async installDirect(id: string, providers: string[], hash: string): Promise<boolean> {
      this.lastError = ''
      this.lastInstallSource = ''
      const list = providers.filter((p) => !!p)
      if (!list.length || !hash) {
        this.lastError = 'install-failed'
        return false
      }
      this.installingId = id
      this.installingSeeds = list.length
      try {
        let lastErr = 'install-failed'
        for (const nodeId of list) {
          try {
            const m = await installApp(id, nodeId, hash)
            if (m) {
              // тот же гейт целостности, что и после установки из облака
              const v = await verifyApp(id)
              if (!v.verified) {
                this.lastError = `verify-failed:${v.reason}`
                return false
              }
              useAppsStore().replaceLocal(m)
              const entry = this.apps.find((a) => a.id === id)
              if (entry) {
                entry.installed = true
                entry.title = m.name || entry.title
                entry.description = m.description || entry.description
              }
              this.lastInstallSource = 'p2p'
              this.installedFlash = m.name || id
              setTimeout(() => (this.installedFlash = ''), 4000)
              return true
            }
          } catch (e: any) {
            const msg = e?.message || String(e) || 'install-failed'
            const sm = /STORAGE_LIMIT:(\d+)/.exec(msg)
            if (sm) {
              this.storagePrompt = { id, neededBytes: parseInt(sm[1], 10) }
              return false
            }
            // мёртвый провайдер → пробуем следующий из списка
            lastErr = msg
          }
        }
        this.lastError = lastErr
        return false
      } finally {
        this.installingId = ''
        this.installingSeeds = 0
      }
    },

    /** Установка из облачных архивов известных Cloud Seeder-ов (без P2P). */
    async installViaSeeder(id: string): Promise<boolean> {
      this.installingId = id
      try {
        const m = await cloudSeederInstallAny(id)
        // тот же гейт целостности, что и после P2P-обновления
        const v = await verifyApp(id)
        if (!v.verified) {
          this.lastError = `verify-failed:${v.reason}`
          return false
        }
        const apps = useAppsStore()
        apps.replaceLocal(m)
        const entry = this.apps.find((a) => a.id === id)
        if (entry) {
          entry.installed = true
          entry.title = m.name || entry.title
          entry.description = m.description || entry.description
        }
        this.lastInstallSource = 'seeder'
        this.installedFlash = m.name || id
        setTimeout(() => (this.installedFlash = ''), 4000)
        return true
      } catch (e: any) {
        const msg = e?.message || String(e) || 'install-failed'
        const sm = /STORAGE_LIMIT:(\d+)/.exec(msg)
        if (sm) {
          this.storagePrompt = { id, neededBytes: parseInt(sm[1], 10) }
          return false
        }
        this.lastError = msg
        return false
      } finally {
        if (this.installingId === id) this.installingId = ''
      }
    },

    /** Raise the storage limit by exactly what the blocked install needs, then retry. */
    async confirmStorageIncrease(): Promise<boolean> {
      const p = this.storagePrompt
      if (!p) return false
      const current = await getStorageLimit()
      const neededMb = Math.ceil(p.neededBytes / (1024 * 1024))
      await setStorageLimit(current + neededMb * 1024 * 1024)
      const id = p.id
      this.storagePrompt = null
      return this.install(id)
    },

    cancelStorageIncrease() {
      this.storagePrompt = null
    },

    stop() {
      if (pollTimer) {
        clearInterval(pollTimer)
        pollTimer = null
      }
    },
  },
})
