<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useAppsStore } from '@/store/apps'
import { useMailBadgeStore } from '@/store/mailBadge'
import { useTabsStore } from '@/store/tabs'
import { useBookmarksStore, bookmarkHost } from '@/store/bookmarks'
import { useUiStore, type UiMode } from '@/store/ui'
import { networkStorage, getMyUsername, listUsernames, dhtStats, dhtProvidersSummary, type NetworkStorage, type DhtStats, type DhtProvidersSummary } from '@/lib/bridge'
import Sparkline from '@/components/Sparkline.vue'
import MyceliumTile from '@/components/MyceliumTile.vue'
import Identicon from '@/components/Identicon.vue'
import { useDiscoveryStore } from '@/store/discovery'
import { appLogo, appPreview, discoveryLogo, discoveryPreview } from '@/lib/bridge'
import { coverStyle } from '@/lib/coverArt'
import { getDhtHistory, pushDht } from '@/lib/dhtHistory'
import { useNetStore } from '@/store/net'

const { t } = useI18n()
const appsStore = useAppsStore()
const mailBadge = useMailBadgeStore()
const tabs = useTabsStore()
const bookmarks = useBookmarksStore()
// Закладки https://-сайтов (v1.135.185): плитки → web-вкладка в один клик.
const bmInput = ref('')
const bmError = ref('')
function bmFavicon(url: string): string {
  try { return new URL(url).origin + '/favicon.ico' } catch { return '' }
}
function addBookmark() {
  bmError.value = ''
  const raw = bmInput.value.trim()
  if (!raw) return
  if (!bookmarks.add(raw)) {
    bmError.value = t('dashboard.bookmarkHttps')
    return
  }
  bmInput.value = ''
}
function openBookmark(url: string) {
  tabs.openWeb(url)
}
const router = useRouter()
const ui = useUiStore()
const net = useNetStore()

/** USER | DEV | GEEK — badge-переключатель UI-режима над «# WELCOME». */
const modes: Array<{ id: UiMode; label: string; hint: string }> = [
  { id: 'user', label: 'USER', hint: 'Простой режим для обычных пользователей' },
  { id: 'dev',  label: 'DEV',  hint: 'Режим разработчика: My Applications + DevHub' },
  { id: 'geek', label: 'GEEK', hint: 'Полный техно-режим: все хеши, CID, MeshMeter' },
]

// Правая колонка баннера, когда MeshMeter свёрнут → «Основные возможности
// сети». Каждый пункт — крохотный feature-row с юникод-глифом, коротким
// заголовком и одной строкой описания. `route` — необязательный клик:
// если задан, строка ведёт на соответствующую страницу. Соблюдаем
// цветовые акценты каждой подсистемы (шифрование → фиолетовый и т.д.).
const features = computed(() => [
  // USER-режим: «Абсолютная приватность» вместо технического E2EE-жаргона.
  { id: 'crypto',    glyph: '⚿',  hue: '#a78bfa',
    titleKey: ui.mode === 'user' ? 'dashboard.featureCryptoTitleUser' : 'dashboard.featureCryptoTitle',
    descKey:  ui.mode === 'user' ? 'dashboard.featureCryptoDescUser'  : 'dashboard.featureCryptoDesc' },
  { id: 'social',    glyph: '❈',  hue: '#f5b301', titleKey: 'dashboard.featureSocialTitle',
    descKey: ui.mode === 'user' ? 'dashboard.featureSocialDescUser' : 'dashboard.featureSocialDesc',    route: 'profile' },
  { id: 'messenger', glyph: '✉',  hue: '#22c55e', titleKey: 'dashboard.featureMessengerTitle',
    descKey: ui.mode === 'user' ? 'dashboard.featureMessengerDescUser' : 'dashboard.featureMessengerDesc' },
  { id: 'storage',   glyph: '▤',  hue: '#cc4141', titleKey: 'dashboard.featureStorageTitle',
    descKey: ui.mode === 'user' ? 'dashboard.featureStorageDescUser' : 'dashboard.featureStorageDesc',   route: 'storages' },
  { id: 'dev',       glyph: '⎇',  hue: '#60a5fa', titleKey: 'dashboard.featureDevTitle',       descKey: 'dashboard.featureDevDesc',       route: 'dev' },
])

const myUsername = ref<string | null>(null)
const nameCount = ref(0)

/* ---- global mesh storage meter ---- */
const NS_CACHE_KEY = 'sth_mesh_ns'
const ns = ref<NetworkStorage | null>(null)
const nsPending = ref(true)
const scanning = ref(false)
const lastUpdated = ref(0)
const nowTick = ref(Date.now())
let nsTimer: ReturnType<typeof setInterval> | undefined
let agoTimer: ReturnType<typeof setInterval> | undefined
let dhtTimer: ReturnType<typeof setInterval> | undefined

// Show last-known mesh data instantly from cache, then rescan on open.
try {
  const cached = localStorage.getItem(NS_CACHE_KEY)
  if (cached) {
    ns.value = JSON.parse(cached)
    nsPending.value = false
  }
} catch {
  /* ignore corrupt cache */
}

async function refreshNs() {
  scanning.value = true
  const r = await networkStorage()
  if (r) {
    ns.value = r
    lastUpdated.value = Date.now()
    try {
      localStorage.setItem(NS_CACHE_KEY, JSON.stringify(r))
      localStorage.setItem(NS_CACHE_KEY + '_t', String(lastUpdated.value))
    } catch {
      /* quota — keep in-memory only */
    }
  }
  nsPending.value = false
  scanning.value = false
}

/* ---- Mainline DHT node counter (serverless discovery health) ---- */
const DHT_CACHE_KEY = 'sth_dht_stats'
function loadDhtCache(): DhtStats {
  try {
    const raw = localStorage.getItem(DHT_CACHE_KEY)
    if (raw) {
      const d = JSON.parse(raw)
      if (d && typeof d.nodes === 'number') return d
    }
  } catch { /* ignore */ }
  return { enabled: true, nodes: 0 }
}
const dht = ref<DhtStats>(loadDhtCache())
const dhtHist = ref<number[]>(getDhtHistory())
async function refreshDht() {
  const d = await dhtStats()
  // Last-known-good: не сбрасываем счётчик до 0 при «дыхании» таблицы DHT.
  if (d.nodes > 0 || !dht.value.nodes) {
    dht.value = d
    try { localStorage.setItem(DHT_CACHE_KEY, JSON.stringify(d)) } catch { /* ignore */ }
  } else {
    dht.value = { ...d, nodes: dht.value.nodes }
  }
  if (d.nodes > 0) dhtHist.value = pushDht(d.nodes)
}

// Трекерлесс-режим: включён ли + сколько сидеров найдено через DHT.
const trackerless = ref<DhtProvidersSummary>({ enabled: false, seeders: 0, apps: 0, announcedApps: 0, beaconPort: null, dhtNodes: 0 })
async function refreshTrackerless() {
  trackerless.value = await dhtProvidersSummary()
}
// Инфо-панель индикатора: раскрывается по наведению, закрепляется по клику.
const tlPinned = ref(false)
const tlHover = ref(false)
const tlShow = computed(() => tlPinned.value || tlHover.value)

// Seconds since the mesh meter last refreshed (drives "Обновлено N сек назад").
const secondsAgo = computed(() => {
  if (!lastUpdated.value) return -1
  return Math.max(0, Math.floor((nowTick.value - lastUpdated.value) / 1000))
})

onMounted(() => {
  if (!appsStore.loaded) appsStore.init()
  try {
    const t = Number(localStorage.getItem(NS_CACHE_KEY + '_t'))
    if (t > 0) lastUpdated.value = t
  } catch {
    /* ignore */
  }
  refreshNs()
  refreshDht()
  refreshTrackerless()
  nsTimer = setInterval(() => {
    if (!document.hidden && net.online) refreshNs()
  }, 15000)
  agoTimer = setInterval(() => (nowTick.value = Date.now()), 1000)
  dhtTimer = setInterval(() => {
    if (!document.hidden) {
      refreshDht()
      refreshTrackerless()
    }
  }, 12000)
  getMyUsername().then((u) => (myUsername.value = u))
  listUsernames().then((list) => (nameCount.value = list.length))
})
onUnmounted(() => {
  if (nsTimer) clearInterval(nsTimer)
  if (agoTimer) clearInterval(agoTimer)
  if (dhtTimer) clearInterval(dhtTimer)
})

const NS_SEGMENTS = 30
const usedPct = computed(() => {
  const c = ns.value?.totalCapacityGb ?? 0
  const u = ns.value?.totalUsedGb ?? 0
  return c > 0 ? Math.min(100, (u / c) * 100) : 0
})
const filledSegments = computed(() => Math.round((usedPct.value / 100) * NS_SEGMENTS))
const nsReady = computed(() => ns.value !== null)
function gb(v: number | undefined): string {
  return (v ?? 0).toFixed(2)
}

/* 24h seeded-GB sparkline */
const hasSpark = computed(() => (ns.value?.history?.length ?? 0) >= 2)
const seeded24h = computed(() => {
  const h = ns.value?.history ?? []
  if (h.length) return h[h.length - 1].usedGb
  return ns.value?.totalUsedGb ?? 0
})
const sparkPoints = computed(() => {
  const h = ns.value?.history ?? []
  if (h.length < 2) return ''
  const vals = h.map((p) => p.usedGb)
  const min = Math.min(...vals)
  const span = Math.max(Math.max(...vals) - min, 0.0001)
  return h
    .map((p, i) => {
      const x = (i / (h.length - 1)) * 100
      const y = 100 - ((p.usedGb - min) / span) * 100
      return `${x.toFixed(2)},${y.toFixed(2)}`
    })
    .join(' ')
})

interface DApp {
  id: string
  name: string
  tag: string
  desc: string
  hue: string
  route?: string
}

const apps = computed<DApp[]>(() => [
  { id: 'microblog', name: 'Micro Blog', tag: '// MICRO BLOG', desc: 'P2P Blog · uncensored discussions', hue: '#d9822b', route: 'profile' },
  { id: 'domarket', name: 'Domains Market', tag: '// DNS', desc: 'Buy and sell .sth domains', hue: '#5c7894', route: 'domarket' },
  { id: 'mail', name: 'Mail', tag: '// MAIL', desc: 'E2E-encrypted mail', hue: '#4a9e7f' },
  { id: 'storage', name: 'Storage', tag: '// FILES', desc: 'Distributed file storage', hue: '#cc4141', route: 'storages' },
])

// Быстрый поиск приложений прямо с дашборда: отправляем запрос на страницу
// «Найти приложения» (Discover) через query-параметр — там он сразу применяется.
const searchQuery = ref('')
function doSearch() {
  const q = searchQuery.value.trim()
  router.push({ name: 'discover', query: q ? { q } : {} })
}

// ТОП-3 приложений сети по числу сидеров — быстрый запуск прямо под поиском.
const disc = useDiscoveryStore()
const topApps = computed(() =>
  [...disc.apps]
    .filter((a) => (a.seeds ?? 0) > 0)
    .sort((x, y) => (y.seeds ?? 0) - (x.seeds ?? 0))
    .slice(0, 3),
)
const topLogos = ref<Record<string, string>>({})
// DAPP Preview 440×180 — обложка вместо градиента (fallback — градиент)
const topPreviews = ref<Record<string, string>>({})
watch(
  topApps,
  (list) => {
    list.forEach((a) => {
      if (topLogos.value[a.id] === undefined) {
        if (a.installed) {
          // локальный файл; у старых установок может отсутствовать — P2P-fallback
          appLogo(a.id)
            .then(async (u) => {
              if (u) {
                topLogos.value[a.id] = u
                return
              }
              if (a.logoHash && !a.private) {
                const ids = (a.providers || []).map((p) => p.nodeId).filter(Boolean)
                topLogos.value[a.id] = ''
                const v = await discoveryLogo(a.id, a.logoHash, ids)
                if (v) topLogos.value[a.id] = v
                else if (!ids.length) delete topLogos.value[a.id]
              }
            })
            .catch(() => {})
        } else if (a.logoHash && !a.private) {
          // неустановленный dApp: PNG-лого по iroh-hash из анонса (кэш + P2P)
          const ids = (a.providers || []).map((p) => p.nodeId).filter(Boolean)
          topLogos.value[a.id] = ''
          discoveryLogo(a.id, a.logoHash, ids)
            .then((u) => {
              if (u) topLogos.value[a.id] = u
              else if (!ids.length) delete topLogos.value[a.id] // повторим с провайдерами
            })
            .catch(() => {})
        }
      }
      if (topPreviews.value[a.id] === undefined) {
        if (a.installed) {
          // установлено старой версией без файла превью → блоб по анонсу
          appPreview(a.id)
            .then(async (u) => {
              if (u) {
                topPreviews.value[a.id] = u
                return
              }
              if (a.previewHash && !a.private) {
                const pids = (a.providers || []).map((p) => p.nodeId).filter(Boolean)
                topPreviews.value[a.id] = ''
                const v = await discoveryPreview(a.id, a.previewHash, pids)
                if (v) topPreviews.value[a.id] = v
                else if (!pids.length) delete topPreviews.value[a.id]
              }
            })
            .catch(() => {})
        } else if (a.previewHash && !a.private) {
          const pids = (a.providers || []).map((p) => p.nodeId).filter(Boolean)
          topPreviews.value[a.id] = ''
          discoveryPreview(a.id, a.previewHash, pids)
            .then((u) => {
              if (u) topPreviews.value[a.id] = u
              else if (!pids.length) delete topPreviews.value[a.id]
            })
            .catch(() => {})
        }
      }
    })
  },
  { immediate: true },
)
function openTopApp(a: { id: string; title?: string; installed?: boolean }) {
  if (a.installed) tabs.open(a.id, a.title || a.id)
  else router.push({ name: 'discover', query: { q: a.title || a.id } })
}

async function launch(app: DApp) {
  if (app.route) {
    router.push({ name: app.route })
    return
  }
  // ✉ Mail → запуск нативного Oxid Mail отдельным процессом (mail_launcher.rs)
  if (app.id === 'mail') {
    try {
      const core = (window as any).__TAURI__?.core
      if (!core?.invoke) throw new Error('Oxid Mail доступен только в нативном клиенте SmartNet')
      await core.invoke('launch_oxid_mail')
    } catch (e: any) {
      window.alert(typeof e === 'string' ? e : (e?.message || String(e)))
    }
    return
  }
  tabs.open(app.id, app.name)
}
</script>

<template>
  <div class="dash" data-testid="dashboard-view">
    <!-- Верхняя строка Dashboard: слева — USER|DEV|GEEK badge-переключатель
         UI-режима, справа — пилюля-тумблер MeshMeter одного вертикального
         размера с бейджами. Оба элемента на одной горизонтали, чтобы верх
         страницы читался как единая control-полоса. -->
    <div class="dash-topbar">
      <div class="dash-modes" data-testid="dashboard-mode-switch">
        <button
          v-for="m in modes"
          :key="m.id"
          class="dm-badge"
          :class="{ active: ui.mode === m.id, [`dm-${m.id}`]: true }"
          :data-testid="`dashboard-mode-${m.id}`"
          :title="m.hint"
          @click="ui.setMode(m.id)"
        >
          <span class="dm-dot" />
          {{ m.label }}
        </button>
      </div>
      <!-- Правый угол: пилюля MeshMeter-toggle. Клик по MeshMeter
           сворачивает / разворачивает крупный блок Global Capacity. Состояние
           persist в localStorage через `ui.setMeshMeterOpen`. -->
      <div class="dash-topbar-right">
        <button
          class="mesh-toggle"
          :class="{ open: ui.meshMeterOpen }"
          :title="ui.meshMeterOpen ? 'Свернуть MeshMeter' : 'Показать MeshMeter'"
          :aria-expanded="ui.meshMeterOpen"
          @click="ui.setMeshMeterOpen(!ui.meshMeterOpen)"
          data-testid="dashboard-mesh-toggle"
        >
          <span class="mesh-toggle-ico" aria-hidden="true">⌬</span>
          <span class="mesh-toggle-text">MeshMeter</span>
          <span class="mesh-toggle-caret" :class="{ up: ui.meshMeterOpen }">▾</span>
        </button>
      </div>
    </div>

    <!-- banner -->
    <section class="banner rise" data-testid="dashboard-banner">
      <div class="banner-grid" />
      <div class="banner-cols">
        <div class="banner-text">
          <span class="banner-small mono-label">{{ t('dashboard.bannerSmall') }}</span>
          <div
            v-if="trackerless.enabled && ui.mode !== 'user'"
            class="trackerless-wrap"
            @mouseenter="tlHover = true"
            @mouseleave="tlHover = false"
          >
            <div
              class="trackerless-pill"
              :class="{ live: trackerless.seeders > 0, open: tlShow }"
              role="button"
              tabindex="0"
              :aria-expanded="tlShow"
              :title="t('dashboard.trackerlessInfoHint')"
              @click="tlPinned = !tlPinned"
              @keydown.enter="tlPinned = !tlPinned"
              data-testid="trackerless-indicator"
            >
              <span class="tl-dot" />
              <span class="tl-text">{{
                trackerless.seeders > 0
                  ? t('dashboard.trackerlessActive', { n: trackerless.seeders })
                  : t('dashboard.trackerlessOn')
              }}</span>
              <span class="tl-caret" :class="{ up: tlShow }">▾</span>
            </div>

            <!-- Инфо-панель: детали трекерлесс-DHT (без перехода в настройки) -->
            <div v-if="tlShow" class="tl-pop" data-testid="trackerless-popover">
              <div class="tl-pop-head">{{ t('peers.dhtDiagTitle') }}</div>
              <ul class="tl-pop-rows">
                <li class="tl-pop-row">
                  <span class="dim">{{ t('peers.dhtDiagSeeders') }}</span>
                  <b class="hl" data-testid="tl-pop-seeders">{{ trackerless.seeders }}</b>
                </li>
                <li class="tl-pop-row">
                  <span class="dim">{{ t('peers.dhtDiagApps') }}</span>
                  <b data-testid="tl-pop-apps">{{ trackerless.apps }}</b>
                </li>
                <li class="tl-pop-row">
                  <span class="dim">{{ t('peers.dhtDiagAnnounced') }}</span>
                  <b data-testid="tl-pop-announced">{{ trackerless.announcedApps }}</b>
                </li>
                <li class="tl-pop-row">
                  <span class="dim">{{ t('peers.dhtDiagNodes') }}</span>
                  <b data-testid="tl-pop-nodes">{{ trackerless.dhtNodes }}</b>
                </li>
                <li class="tl-pop-row">
                  <span class="dim">{{ t('peers.dhtDiagBeacon') }}</span>
                  <b :class="{ dim: !trackerless.beaconPort }" data-testid="tl-pop-beacon">{{
                    trackerless.beaconPort ? `UDP :${trackerless.beaconPort}` : t('peers.dhtDiagBeaconOff')
                  }}</b>
                </li>
              </ul>
            </div>
          </div>
          <h1 class="banner-big">{{ t('dashboard.bannerBig') }}</h1>
          <p class="banner-desc">{{ t(ui.mode === 'user' ? 'dashboard.descriptionUser' : 'dashboard.description') }}</p>

          <!-- Быстрый поиск приложений → отправляет запрос на страницу Discover -->
          <div class="dash-search" data-testid="dashboard-search-row">
            <svg class="dash-search-ico" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <circle cx="11" cy="11" r="8"></circle>
              <line x1="21" y1="21" x2="16.65" y2="16.65"></line>
            </svg>
            <input
              v-model="searchQuery"
              class="dash-search-input"
              type="text"
              :placeholder="t('discover.searchPlaceholder')"
              @keyup.enter="doSearch"
              data-testid="dashboard-search-input"
            />
            <button
              v-if="searchQuery"
              class="dash-search-clear"
              @click="searchQuery = ''"
              aria-label="clear"
              data-testid="dashboard-search-clear"
            >✕</button>
            <button class="dash-search-go" @click="doSearch" data-testid="dashboard-search-go" :aria-label="t('discover.title')">→</button>
          </div>

          <!-- Популярно в сети: топ-3 приложений по числу сидеров (витрина) -->
          <div v-if="topApps.length" class="top-apps" data-testid="dashboard-top-apps">
            <span class="top-apps-label mono-label">[ {{ t('dashboard.topApps') }} ]</span>
            <div class="top-apps-row">
              <button
                v-for="a in topApps"
                :key="a.id"
                class="top-app"
                @click="openTopApp(a)"
                :data-testid="`top-app-${a.id}`"
              >
                <span class="top-app-cover" :style="coverStyle(a.id)">
                  <img v-if="topPreviews[a.id]" :src="topPreviews[a.id]" alt="" class="top-app-preview" />
                  <!-- лого-чип — только fallback, когда нет preview-обложки -->
                  <span v-if="!topPreviews[a.id]" class="top-app-icon-chip">
                    <img v-if="topLogos[a.id]" :src="topLogos[a.id]" alt="" class="top-app-logo" />
                    <Identicon v-else :seed="a.id" :size="34" />
                  </span>
                </span>
                <span class="top-app-body">
                  <span class="top-app-name">{{ a.title || a.id }}</span>
                  <span class="top-app-seeds" :title="t('discover.online')">
                    <span class="top-app-dot" />{{ ui.mode !== 'user' ? a.seeds : '' }}
                  </span>
                </span>
              </button>
            </div>
          </div>
        </div>

        <!-- global mesh capacity meter — schлопывается через ui.meshMeterOpen
             (тумблер справа от «# WELCOME TO NETFORY», USER-режим по умолчанию
             стартует свёрнутым; GEEK — развёрнутым; состояние запоминается). -->
        <div v-if="ui.meshMeterOpen" class="cap" data-testid="network-storage">
          <div class="cap-head">
            <span class="cap-title mono-label">[ {{ t('dashboard.globalCapacity') }} ]</span>
            <span class="cap-router" :class="{ live: nsReady && (ns?.onlineNodes ?? 0) > 0, scanning }" data-testid="network-router-status">
              <span class="cap-dot" />
              {{ nsReady ? t('dashboard.meshOnline', { n: ns?.onlineNodes ?? 0 }) : t('dashboard.meshScan') }}
            </span>
          </div>
          <div v-if="secondsAgo >= 0" class="cap-updated" :class="{ scanning }" data-testid="network-updated-ago">
            <span class="cap-updated-dot" />
            {{ scanning ? t('dashboard.scanningMeter') : (secondsAgo < 1 ? t('dashboard.meshUpdatedNow') : t('dashboard.meshUpdatedAgo', { n: secondsAgo })) }}
          </div>

          <template v-if="nsReady">
            <div class="cap-row">
              <span class="cap-k">{{ t('dashboard.totalCapacity') }}</span>
              <span class="cap-v" data-testid="network-total-capacity">{{ gb(ns?.totalCapacityGb) }} GB</span>
            </div>
            <div
              class="cap-bar"
              :class="{ scanning }"
              :title="`${usedPct.toFixed(1)}%`"
              data-testid="network-capacity-bar"
            >
              <span
                v-for="i in NS_SEGMENTS"
                :key="i"
                class="seg"
                :class="{ on: i <= filledSegments }"
              />
            </div>
            <div class="cap-row">
              <span class="cap-k">{{ t('dashboard.networkLoad') }}</span>
              <span class="cap-v load" data-testid="network-total-used">{{ gb(ns?.totalUsedGb) }} GB</span>
            </div>
            <div class="cap-row reward" data-testid="network-reward-pool">
              <span class="cap-k">{{ t('dashboard.rewardPool') }}</span>
              <span class="cap-v reward-v" data-testid="network-reward-value">{{ gb(ns?.paymentPoolBalanceSth) }} STH</span>
            </div>
            <div class="cap-free" data-testid="network-free">
              {{ gb(ns?.totalFreeGb) }} GB {{ t('dashboard.free') }}
            </div>
            <div class="cap-row dht-row" data-testid="network-dht">
              <span class="cap-k">{{ t('dashboard.dhtNodes') }}</span>
              <span class="dht-right">
                <Sparkline
                  v-if="dht.enabled && dhtHist.length > 1"
                  :values="dhtHist"
                  color="#f59e0b"
                  :width="80"
                  :height="20"
                  data-testid="dashboard-dht-spark"
                />
                <span class="cap-v" :class="{ live: dht.enabled && dht.nodes > 0 }" data-testid="network-dht-nodes">
                  {{ dht.enabled ? dht.nodes : t('dashboard.dhtOff') }}
                </span>
              </span>
            </div>
            <div v-if="hasSpark" class="cap-spark" data-testid="network-spark">
              <span class="cap-spark-label"><b class="cap-spark-num" data-testid="network-spark-value">{{ gb(seeded24h) }}</b> {{ t('dashboard.gbSeeded24h') }}</span>
              <svg class="cap-spark-svg" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
                <polyline
                  :points="sparkPoints"
                  fill="none"
                  stroke="#34d399"
                  stroke-width="1.6"
                  vector-effect="non-scaling-stroke"
                  stroke-linejoin="round"
                  stroke-linecap="round"
                />
              </svg>
            </div>
          </template>

          <div v-else class="cap-scan" data-testid="network-scanning">
            <div class="cap-scanline" />
            <span class="cap-scan-text">{{ t('dashboard.scanningMeter') }}</span>
          </div>
        </div>

        <!-- Companion-блок: заполняет ту же правую колонку, что и MeshMeter,
             когда пользователь свернул технический счётчик. Размеры / бордер
             / паддинги точно те же (класс .cap + .cap-info), чтобы правая
             колонка не «прыгала» при переключении. Содержит краткое описание
             основных возможностей сети — то, что «нормис» ждёт увидеть в
             первые 30 секунд знакомства с приложением. -->
        <div v-else class="cap cap-info" data-testid="network-info">
          <div class="cap-head">
            <span class="cap-title mono-label">[ {{ t('dashboard.featuresHead') }} ]</span>
            <span class="cap-router live">
              <span class="cap-dot" />
              {{ t('dashboard.featuresBadge') }}
            </span>
          </div>
          <p class="cap-info-lede">{{ t('dashboard.featuresLede') }}</p>
          <ul class="cap-info-list">
            <li
              v-for="f in features"
              :key="f.id"
              class="cap-info-row"
              :data-hue="f.hue"
              :data-testid="`feature-${f.id}`"
              :class="{ clickable: !!f.route }"
              :role="f.route ? 'button' : undefined"
              :tabindex="f.route ? 0 : undefined"
              @click="f.route && router.push({ name: f.route })"
              @keydown.enter="f.route && router.push({ name: f.route })"
            >
              <span class="cap-info-glyph" aria-hidden="true">{{ f.glyph }}</span>
              <div class="cap-info-text">
                <span class="cap-info-title">{{ t(f.titleKey) }}</span>
                <span class="cap-info-desc">{{ t(f.descKey) }}</span>
              </div>
              <span v-if="f.route" class="cap-info-arrow" aria-hidden="true">→</span>
            </li>
          </ul>
        </div>
      </div>
    </section>

    <section class="seed-section">
      <div class="seed-head">
        <h2 class="seed-title">// {{ t('dashboard.sitesYouSeed') }}_</h2>
        <button v-if="ui.mode !== 'user'" class="btn btn-primary" @click="router.push({ name: 'apps' })" data-testid="publish-app-btn">
          {{ t('dashboard.publishApp') }}
        </button>
      </div>

      <div class="tiles" data-testid="dapp-tiles">
        <!-- NameService — первый блок среди раздаваемых сервисов -->
        <button
          class="tile rise"
          :style="{ animationDelay: '390ms', '--tile-hue': '#2ee3c6' }"
          @click="router.push({ name: 'nameservice' })"
          data-testid="dashboard-nameservice"
        >
          <div class="tile-glow" />
          <div class="tile-top">
            <span class="tile-tag">// NAMES</span>
            <span v-if="nameCount > 0" class="tile-status" data-testid="dashboard-ns-count">● {{ t('dashboard.namesRegistered', { n: nameCount }) }}</span>
          </div>
          <h3 class="tile-name">NameService</h3>
          <p class="tile-desc">{{ t('nameservice.dashHint') }}</p>
          <span v-if="myUsername" class="tile-launch" data-testid="dashboard-ns-mine">u://{{ myUsername }}</span>
          <span v-else class="tile-launch" data-testid="dashboard-ns-register">⬡ {{ t('naming.registerName') }} →</span>
        </button>
        <button
          v-for="(app, i) in apps"          :key="app.id"
          class="tile rise"
          :style="{ animationDelay: `${460 + i * 70}ms`, '--tile-hue': app.hue }"
          @click="launch(app)"
          :data-testid="`dapp-tile-${app.id}`"
        >
          <div class="tile-glow" />
          <div class="tile-top">
            <span class="tile-tag">{{ app.tag }}</span>
            <span
              v-if="app.id === 'mail' && mailBadge.unread > 0"
              class="tile-badge"
              data-testid="mail-unread-badge"
            >✉ {{ mailBadge.unread > 99 ? '99+' : mailBadge.unread }}</span>
            <span v-else class="tile-status">● {{ t('dashboard.seeding') }}</span>
          </div>
          <h3 class="tile-name">{{ app.name }}</h3>
          <p class="tile-desc">{{ app.desc }}</p>
          <span class="tile-launch">{{ t('dashboard.launch') }} →</span>
        </button>
        <!-- Mycelium Protocol — шестой блок витрины (вместо Stealth Mode = relay-only) -->
        <MyceliumTile />
      </div>
    </section>

    <!-- закладки https://-сайтов (web-вкладки); добавление — звёздочкой в адресной строке -->
    <section v-if="bookmarks.items.length" class="seed-section" data-testid="dashboard-bookmarks">
      <div class="seed-head">
        <h2 class="seed-title">// {{ t('dashboard.bookmarks') }}_</h2>
      </div>
      <div class="tiles bm-tiles">
        <div
          v-for="(b, i) in bookmarks.items"
          :key="b.url"
          class="tile bm-tile rise"
          :style="{ animationDelay: `${i * 60}ms`, '--tile-hue': '#38bdf8' }"
          role="button"
          tabindex="0"
          @click="openBookmark(b.url)"
          @keyup.enter="openBookmark(b.url)"
          :data-testid="`bookmark-tile-${bookmarkHost(b.url)}`"
        >
          <div class="tile-top">
            <span class="bm-host">
              <img class="bm-favicon" :src="bmFavicon(b.url)" alt="" @error="($event.target as HTMLImageElement).style.visibility = 'hidden'" />
              {{ bookmarkHost(b.url) }}
            </span>
            <button
              class="bm-del"
              :title="t('dashboard.bookmarkRemove')"
              @click.stop="bookmarks.remove(b.url)"
              :data-testid="`bookmark-remove-${bookmarkHost(b.url)}`"
            >✕</button>
          </div>
          <h3 class="tile-name bm-title">{{ b.title }}</h3>
          <p class="tile-desc bm-url">{{ b.url }}</p>
          <span class="tile-launch">{{ t('dashboard.bookmarkOpen') }}</span>
        </div>
      </div>
    </section>

    <!-- favorites (starred installed apps) -->
    <section v-if="appsStore.favoriteApps.length" class="seed-section" data-testid="dashboard-favorites">
      <div class="seed-head">
        <h2 class="seed-title">// {{ t('dashboard.favorites') }}_</h2>
      </div>
      <div class="tiles">
        <button
          v-for="(m, i) in appsStore.favoriteApps"
          :key="m.id"
          class="tile rise"
          :style="{ animationDelay: `${i * 70}ms`, '--tile-hue': '#f5b301' }"
          @click="tabs.open(m.id, m.name || m.id)"
          :data-testid="`fav-tile-${m.id}`"
        >
          <div class="tile-glow" />
          <div class="tile-top">
            <span class="tile-tag">★ {{ t('dashboard.favorite') }}</span>
            <span class="tile-status">● {{ t('dashboard.seeding') }}</span>
          </div>
          <h3 class="tile-name">{{ m.name || t('discover.noTitle') }}</h3>
          <p class="tile-desc">{{ m.description || t('discover.noDescription') }}</p>
          <span class="tile-launch">{{ t('dashboard.launch') }} →</span>
        </button>
      </div>
    </section>
  </div>
</template>

<style scoped>
.dash {
  display: flex;
  flex-direction: column;
  gap: 28px;
  max-width: none;
  width: 100%;
}

/* ---- Верхняя control-полоса Dashboard ----
   Слева — USER|DEV|GEEK badge-переключатель, справа — пилюля MeshMeter.
   Обе группы одной высоты, на одной горизонтали. */
.dash-topbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  flex-wrap: wrap;
}

.dash-modes {
  display: inline-flex;
  gap: 8px;
  padding: 6px;
  border-radius: 999px;
  background: rgba(0,0,0,0.28);
  border: 1px solid var(--border);
  backdrop-filter: blur(8px);
  -webkit-backdrop-filter: blur(8px);
}
.dm-badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 14px;
  font-family: var(--font-mono);
  font-size: 11px;
  font-weight: var(--fw-700);
  letter-spacing: 0.16em;
  color: var(--text-dim);
  background: transparent;
  border: 1px solid transparent;
  border-radius: 999px;
  cursor: pointer;
  transition: color 0.15s ease, background-color 0.15s ease, border-color 0.15s ease, box-shadow 0.2s ease;
}
.dm-badge:hover { color: var(--text); background: rgba(255,255,255,0.04); }
.dm-badge .dm-dot {
  width: 6px; height: 6px; border-radius: 50%;
  background: currentColor; opacity: 0.6;
  box-shadow: 0 0 0 0 currentColor;
  transition: box-shadow 0.25s ease, opacity 0.15s ease;
}
.dm-badge.active .dm-dot { opacity: 1; box-shadow: 0 0 10px currentColor; }
/* Свечение активного бейджа под цвет режима. */
.dm-badge.dm-user.active { color: #22c55e; border-color: rgba(34,197,94,0.55); background: rgba(34,197,94,0.10); box-shadow: 0 0 16px rgba(34,197,94,0.35); }
.dm-badge.dm-dev.active  { color: #60a5fa; border-color: rgba(96,165,250,0.55); background: rgba(96,165,250,0.10); box-shadow: 0 0 16px rgba(96,165,250,0.35); }
.dm-badge.dm-geek.active { color: #f59e0b; border-color: rgba(245,158,11,0.55); background: rgba(245,158,11,0.10); box-shadow: 0 0 16px rgba(245,158,11,0.35); }

/* Пилюля MeshMeter — размеры точь-в-точь под dm-badge (высота = ~26px),
   чтобы обе группы верхней полосы визуально совпадали по вертикали. */
.dash-topbar-right {
  display: flex;
  align-items: center;
  gap: 10px;
}
.mesh-toggle {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 6px 14px;
  font-family: var(--font-mono);
  font-size: 11px;
  font-weight: var(--fw-700);
  letter-spacing: 0.14em;
  color: var(--text-dim);
  background: rgba(0,0,0,0.28);
  border: 1px solid var(--border);
  border-radius: 999px;
  cursor: pointer;
  backdrop-filter: blur(8px);
  -webkit-backdrop-filter: blur(8px);
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease, box-shadow 0.2s ease;
}
.mesh-toggle:hover { color: var(--text); border-color: var(--accent); background: rgba(255,255,255,0.05); }
.mesh-toggle.open {
  color: var(--accent);
  border-color: var(--accent);
  box-shadow: 0 0 14px rgba(176,133,255,0.28);
}
.mesh-toggle-ico { font-size: 13px; line-height: 1; }
.mesh-toggle-caret { font-size: 9px; transition: transform 0.15s ease; opacity: 0.75; }
.mesh-toggle-caret.up { transform: rotate(180deg); }

/* banner */
.banner {
  position: relative;
  overflow: hidden;
  padding: 40px 36px;
  background: var(--panel);
  border: 1px solid var(--border-subtle);
  border-radius: var(--radius);
  box-shadow: var(--shadow-sm);
}
.banner-grid {
  display: none;
}
.banner-cols {
  position: relative;
  display: grid;
  grid-template-columns: 1fr;
  gap: 24px;
  align-items: center;
}
@media (min-width: 768px) {
  .banner-cols {
    grid-template-columns: 1.1fr 1fr;
    align-items: start;
  }
}
.banner-text {
  position: relative;
}

/* global mesh capacity meter */
.cap {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 20px;
  background: var(--panel-2);
  border: 1px solid var(--border-subtle);
  border-radius: var(--radius);
}

/* ── Companion right-column: короткое описание «что умеет сеть», когда
   MeshMeter свёрнут. Размерами полностью совпадает с .cap, поэтому
   правая колонка не «дёргается» при переключении. ── */
.cap-info-lede {
  font-size: 12.5px;
  color: var(--text-dim);
  line-height: 1.55;
  margin: 0 0 4px;
}
.cap-info-list {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.cap-info-row {
  display: grid;
  grid-template-columns: 32px 1fr auto;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  border: 1px solid var(--border-subtle);
  border-radius: 10px;
  background: rgba(0,0,0,0.18);
  transition: border-color 0.15s ease, background-color 0.15s ease, transform 0.15s ease;
}
.cap-info-row.clickable { cursor: pointer; }
.cap-info-row.clickable:hover {
  border-color: var(--_hue, var(--accent));
  background: rgba(255,255,255,0.03);
  transform: translateX(2px);
}
.cap-info-row[data-hue='#a78bfa'] { --_hue: #a78bfa; }
.cap-info-row[data-hue='#f5b301'] { --_hue: #f5b301; }
.cap-info-row[data-hue='#22c55e'] { --_hue: #22c55e; }
.cap-info-row[data-hue='#cc4141'] { --_hue: #cc4141; }
.cap-info-row[data-hue='#60a5fa'] { --_hue: #60a5fa; }

.cap-info-glyph {
  width: 32px; height: 32px;
  display: inline-flex; align-items: center; justify-content: center;
  font-size: 18px; line-height: 1;
  color: var(--_hue, var(--text));
  background: color-mix(in srgb, var(--_hue, var(--accent)) 12%, transparent);
  border: 1px solid color-mix(in srgb, var(--_hue, var(--accent)) 32%, transparent);
  border-radius: 8px;
  text-shadow: 0 0 8px color-mix(in srgb, var(--_hue, var(--accent)) 45%, transparent);
}
.cap-info-text { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.cap-info-title {
  font-size: 12.5px;
  font-weight: var(--fw-700);
  color: var(--text);
  letter-spacing: 0.01em;
}
.cap-info-desc {
  font-size: 11.5px;
  color: var(--text-dim);
  line-height: 1.4;
}
.cap-info-arrow {
  font-size: 14px;
  color: var(--_hue, var(--text-dim));
  opacity: 0.55;
  transition: opacity 0.15s ease, transform 0.15s ease;
}
.cap-info-row.clickable:hover .cap-info-arrow { opacity: 1; transform: translateX(3px); }
.cap-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}
.cap-title {
  color: var(--text-dim);
  letter-spacing: 0.12em;
}
.cap-router {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 10px;
  letter-spacing: 0.08em;
  color: var(--text-dim);
  text-transform: uppercase;
}
.cap-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--text-dim);
}
.cap-router.live .cap-dot {
  background: #34d399;
  box-shadow: 0 0 8px #34d399;
  animation: cap-pulse 1.6s ease-in-out infinite;
}
.cap-router.live {
  color: #34d399;
}
/* mesh "updated N sec ago" line + scanning pulse */
.cap-updated {
  display: flex;
  align-items: center;
  gap: 7px;
  font-size: 10px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--text-dim);
  margin-top: -4px;
}
.cap-updated-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--text-dim);
}
.cap-updated.scanning {
  color: #34d399;
}
.cap-updated.scanning .cap-updated-dot {
  background: #34d399;
  box-shadow: 0 0 8px #34d399;
  animation: cap-pulse 1s ease-in-out infinite;
}
.cap-router.scanning .cap-dot {
  background: #fbbf24;
  box-shadow: 0 0 8px #fbbf24;
  animation: cap-pulse 0.9s ease-in-out infinite;
}
.cap-router.scanning {
  color: #fbbf24;
}
.cap-row {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 10px;
}
.cap-k {
  font-size: 10.5px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.cap-v {
  font-family: var(--font-mono, monospace);
  font-size: 15px;
  font-weight: var(--fw-700);
  color: var(--text);
}
.cap-v.load {
  color: var(--text);
}
.cap-row.reward {
  padding-top: 8px;
  border-top: 1px solid var(--border-subtle);
}
.dht-right {
  display: inline-flex;
  align-items: center;
  gap: 10px;
}
.dht-right :deep(.sparkline) {
  opacity: 0.85;
}
.cap-v.reward-v {
  color: var(--text);
}
.cap-v.live {
  color: #34d399;
}
.cap-bar {
  display: flex;
  gap: 3px;
  height: 22px;
  align-items: stretch;
  position: relative;
  overflow: hidden;
  border-radius: 2px;
}
.cap-bar.scanning::after {
  content: '';
  position: absolute;
  inset: 0;
  background: linear-gradient(
    90deg,
    transparent 0%,
    var(--accent-soft) 42%,
    var(--accent-glow) 50%,
    var(--accent-soft) 58%,
    transparent 100%
  );
  transform: translateX(-100%);
  animation: cap-sweep 1.1s ease-in-out infinite;
  pointer-events: none;
}
@keyframes cap-sweep {
  to {
    transform: translateX(100%);
  }
}
.cap-bar .seg {
  flex: 1;
  border-radius: 1px;
  background: var(--ui-accent);
  opacity: 0.6;
  transition: background-color 0.4s ease, box-shadow 0.4s ease, opacity 0.4s ease;
}
.cap-bar .seg.on {
  background: linear-gradient(180deg, #34d399, #0f9b6c);
  box-shadow: 0 0 8px rgba(52, 211, 153, 0.55);
  opacity: 1;
  animation: cap-pulse 2.4s ease-in-out infinite;
}
.cap-free {
  font-size: 11px;
  letter-spacing: 0.06em;
  color: #34d399;
  text-align: right;
  text-shadow: 0 0 10px rgba(52, 211, 153, 0.4);
}
.cap-spark {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-top: 2px;
}
.cap-spark-label {
  font-size: 9px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.cap-spark-num {
  color: #34d399;
  font-weight: var(--fw-800);
  font-size: 11px;
}
.cap-spark-svg {
  width: 100%;
  height: 34px;
  opacity: 0.9;
  filter: drop-shadow(0 0 4px rgba(52, 211, 153, 0.4));
}
.cap-scan {
  position: relative;
  height: 76px;
  border: 1px solid var(--border-subtle);
  border-radius: var(--radius);
  overflow: hidden;
  display: flex;
  align-items: center;
  justify-content: center;
}
.cap-scanline {
  position: absolute;
  left: -40%;
  top: 0;
  bottom: 0;
  width: 40%;
  background: linear-gradient(90deg, transparent, rgba(255, 255, 255, 0.04), transparent);
  animation: cap-scan 1.8s linear infinite;
}
.cap-scan-text {
  position: relative;
  font-size: 11px;
  letter-spacing: 0.14em;
  color: var(--text-dim);
  text-transform: uppercase;
}
@keyframes cap-pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.55; }
}
@keyframes cap-scan {
  0% { left: -40%; }
  100% { left: 100%; }
}
.banner-small {
  position: relative;
  display: block;
}
/* Трекерлесс-индикатор (над заголовком WELCOME) */
.trackerless-pill {
  position: relative;
  display: inline-flex;
  align-items: center;
  gap: 8px;
  margin-top: 10px;
  padding: 5px 12px;
  border-radius: 999px;
  background: rgba(120, 130, 150, 0.1);
  border: 1px solid rgba(120, 130, 150, 0.28);
  font-size: 11px;
  letter-spacing: 0.05em;
  color: var(--text-dim);
  cursor: pointer;
  transition: background-color 0.15s ease, border-color 0.15s ease, transform 0.15s ease;
}
.trackerless-pill:hover {
  transform: translateY(-1px);
  border-color: rgba(120, 130, 150, 0.5);
}
.trackerless-pill.live {
  background: rgba(34, 197, 94, 0.12);
  border-color: rgba(34, 197, 94, 0.4);
  color: #22c55e;
}
.tl-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: currentColor;
  flex: 0 0 auto;
}
.trackerless-pill.live .tl-dot {
  box-shadow: 0 0 0 0 rgba(34, 197, 94, 0.6);
  animation: tl-pulse 2s infinite;
}
@keyframes tl-pulse {
  0% { box-shadow: 0 0 0 0 rgba(34, 197, 94, 0.55); }
  70% { box-shadow: 0 0 0 7px rgba(34, 197, 94, 0); }
  100% { box-shadow: 0 0 0 0 rgba(34, 197, 94, 0); }
}
.tl-text {
  font-weight: var(--fw-700);
}
/* --- Инфо-панель индикатора трекерлесс-DHT (hover/click) --- */
.trackerless-wrap {
  position: relative;
  display: inline-block;
  margin-top: 10px;
}
.trackerless-wrap .trackerless-pill {
  margin-top: 0;
}
.tl-caret {
  font-size: 9px;
  opacity: 0.7;
  transition: transform 0.18s ease;
}
.tl-caret.up {
  transform: rotate(180deg);
}
.trackerless-pill.open {
  border-color: rgba(120, 130, 150, 0.6);
}
.trackerless-pill.live.open {
  border-color: rgba(34, 197, 94, 0.7);
}
.tl-pop {
  position: absolute;
  top: calc(100% + 8px);
  left: 0;
  z-index: 40;
  min-width: 260px;
  padding: 12px 14px;
  border-radius: 12px;
  background: rgba(16, 18, 22, 0.97);
  border: 1px solid var(--border);
  box-shadow: 0 14px 40px -12px rgba(0, 0, 0, 0.7);
  backdrop-filter: blur(14px);
  animation: tl-pop-in 0.16s ease;
}
@keyframes tl-pop-in {
  from { opacity: 0; transform: translateY(-4px); }
  to { opacity: 1; transform: translateY(0); }
}
.tl-pop-head {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--secondary);
  font-weight: var(--fw-700);
  margin-bottom: 8px;
}
.tl-pop-rows {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.tl-pop-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  font-size: 12px;
}
.tl-pop-row b {
  font-variant-numeric: tabular-nums;
}
.banner-big {
  position: relative;
  font-size: 34px;
  line-height: 1.1;
  font-weight: var(--fw-800);
  letter-spacing: 0.02em;
  margin: 10px 0 14px;
  color: var(--text);
  text-shadow: none;
}
.banner-desc {
  position: relative;
  max-width: 720px;
  font-size: 13px;
  line-height: 1.7;
  color: var(--text-dim);
}

/* Быстрый поиск приложений в шапке */
.dash-search {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 20px;
  padding: 9px 8px 9px 14px;
  background: var(--panel-2);
  border: 1px solid var(--border-subtle);
  border-radius: 999px;
  transition: border-color 0.15s ease, box-shadow 0.15s ease;
}
.dash-search:focus-within {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-soft);
}
.dash-search-ico {
  flex: none;
  color: var(--text-dim);
}
.dash-search-input {
  flex: 1;
  min-width: 0;
  background: transparent;
  border: none;
  outline: none;
  color: var(--text);
  font-size: 13px;
  font-family: inherit;
}
.dash-search-input::placeholder {
  color: var(--text-dim);
}
.dash-search-clear {
  flex: none;
  background: transparent;
  border: none;
  color: var(--text-dim);
  cursor: pointer;
  font-size: 13px;
  transition: color 0.15s ease;
}
.dash-search-clear:hover {
  color: var(--accent);
}
.dash-search-go {
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 30px;
  height: 30px;
  border-radius: 50%;
  background: var(--accent-soft);
  border: 1px solid var(--border-accent);
  color: var(--accent);
  cursor: pointer;
  font-size: 16px;
  line-height: 1;
  transition: background-color 0.15s ease, color 0.15s ease, transform 0.15s ease;
}
.dash-search-go:hover {
  background: var(--accent);
  color: #04121a;
  transform: translateX(1px);
}

/* ТОП-3 приложений по сидерам — быстрый запуск под поиском. */
.top-apps {
  margin-top: 18px;
}
.top-apps-label {
  display: block;
  font-size: 10px;
  margin-bottom: 10px;
  color: var(--secondary);
}
.top-apps-row {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 14px;
}
.top-app {
  display: flex;
  flex-direction: column;
  padding: 0;
  overflow: hidden;
  background: var(--panel-2);
  border: 1px solid var(--border-subtle);
  border-radius: 14px;
  cursor: pointer;
  color: var(--text);
  text-align: left;
  transition: border-color 0.18s ease, transform 0.18s ease, box-shadow 0.18s ease;
}
.top-app:hover {
  border-color: var(--border-accent);
  transform: translateY(-3px);
  box-shadow: 0 12px 28px -10px rgba(0, 0, 0, 0.35), var(--shadow-card);
}
/* Цветная обложка-витрина (градиент из хэша id) с логотипом по центру. */
.top-app-cover {
  position: relative;
  aspect-ratio: 440 / 180; /* полное превью без обрезаний, масштабируется по ширине */
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  border: 1px solid var(--border);
  border-radius: 10px 10px 0 0;
}
/* DAPP Preview 440×180 как обложка; градиент под ним — fallback */
.top-app-preview {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  object-fit: cover;
  z-index: 0;
  pointer-events: none;
}
/* Блик сверху + лёгкое затемнение снизу — придаёт обложке глубину. */
.top-app-cover::before {
  content: '';
  position: absolute;
  inset: 0;
  background:
    radial-gradient(120% 90% at 18% 0%, rgba(255, 255, 255, 0.08), transparent 55%),
    linear-gradient(to top, rgba(0, 0, 0, 0.35), transparent 45%);
  pointer-events: none;
}
/* Полоса блика: при наведении пролетает по обложке слева направо. */
.top-app-cover::after {
  content: '';
  position: absolute;
  top: -20%;
  bottom: -20%;
  left: -70%;
  width: 55%;
  background: linear-gradient(
    105deg,
    transparent 0%,
    rgba(255, 255, 255, 0.02) 35%,
    rgba(255, 255, 255, 0.14) 50%,
    rgba(255, 255, 255, 0.02) 65%,
    transparent 100%
  );
  transform: skewX(-18deg);
  pointer-events: none;
}
.top-app:hover .top-app-cover::after {
  animation: top-app-shine 0.7s ease-out;
}
@keyframes top-app-shine {
  from { left: -70%; }
  to { left: 130%; }
}
/* Тёмный чип-подложка: прозрачные логотипы больше не сливаются с градиентом. */
.top-app-icon-chip {
  position: relative;
  z-index: 1;
  width: 58px;
  height: 58px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 15px;
  background: rgba(5, 12, 17, 0.66);
  border: 1px solid rgba(255, 255, 255, 0.16);
  box-shadow: 0 4px 14px rgba(0, 0, 0, 0.35), inset 0 1px 0 rgba(255, 255, 255, 0.08);
  backdrop-filter: blur(6px);
  transition: transform 0.18s ease, box-shadow 0.18s ease;
}
.top-app:hover .top-app-icon-chip {
  transform: scale(1.08);
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.45), inset 0 1px 0 rgba(255, 255, 255, 0.1);
}
.top-app-logo {
  width: 38px;
  height: 38px;
  border-radius: 9px;
  object-fit: cover;
}
.top-app-body {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 11px 14px;
  min-width: 0;
}
.top-app-name {
  flex: 1;
  font-size: 13.5px;
  font-weight: var(--fw-600);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.top-app-seeds {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 11px;
  font-weight: var(--fw-600);
  color: var(--success, #22c55e);
  background: rgba(34, 197, 94, 0.12);
  background: color-mix(in srgb, var(--success, #22c55e) 12%, transparent);
  padding: 3px 8px;
  border-radius: 999px;
}
.top-app-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--success, #22c55e);
  box-shadow: 0 0 5px var(--success, #22c55e);
}

/* NameService + Стелс-режим под заголовком */
.banner-blocks {
  display: grid;
  grid-template-columns: 1fr;
  gap: 12px;
  margin-top: 16px;
}
@media (min-width: 1100px) {
  .banner-blocks {
    grid-template-columns: 1fr 1fr;
    align-items: stretch;
  }
}

/* stats */
.stats {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 14px;
}
.stat-card {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 18px 16px;
  background: var(--panel);
  border: 1px solid var(--border-subtle);
  border-radius: var(--radius);
  box-shadow: var(--shadow-sm);
  overflow: hidden;
  transition: background-color 0.2s ease, transform 0.2s ease;
}
.stat-card:hover {
  background: var(--panel-hover);
  transform: translateY(-1px);
}
.stat-value {
  font-size: 17px;
  font-weight: var(--fw-700);
  color: var(--text);
  word-break: break-all;
}
.stat-value.mono {
  font-size: 13px;
  line-height: 1.45;
  letter-spacing: -0.02em;
  word-break: normal;
  white-space: nowrap;
}
.stat-corner {
  display: none;
}

/* seed section */
.seed-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 16px;
}
.seed-title {
  font-size: 14px;
  letter-spacing: 0.14em;
  color: var(--text-dim);
  font-weight: var(--fw-700);
}
.bm-add { display: flex; gap: 8px; align-items: center; margin-left: auto; }
.bm-input { width: min(360px, 50vw); padding: 7px 10px; background: rgba(0,0,0,0.28); border: 1px solid var(--border); color: var(--text); font-size: 12px; font-family: inherit; }
.bm-input:focus { outline: none; border-color: var(--border-accent); }
.bm-btn { padding: 7px 12px; background: var(--accent); color: #04110f; border: none; font-size: 11px; font-weight: var(--fw-800); cursor: pointer; }
.bm-btn:disabled { opacity: 0.5; cursor: default; }
.bm-err { color: #f59e0b; font-size: 11px; }
.bm-empty { font-size: 12px; }
.bm-tile { cursor: pointer; }
.bm-host { display: inline-flex; align-items: center; gap: 6px; font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--tile-hue); font-weight: var(--fw-800); }
.bm-favicon { width: 14px; height: 14px; object-fit: contain; }
.bm-del { background: transparent; border: none; color: var(--text-dim); cursor: pointer; font-size: 12px; padding: 2px 6px; opacity: 0; transition: opacity 0.15s, color 0.15s; }
.bm-tile:hover .bm-del { opacity: 1; }
.bm-del:hover { color: #ff5f56; }
.bm-title { margin-top: 10px; }
.bm-url { word-break: break-all; font-size: 11px; }
.tiles {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
  gap: 16px;
}
.tile {
  position: relative;
  text-align: left;
  padding: 20px 18px 18px;
  background: var(--panel);
  border: 1px solid var(--border-subtle);
  border-top: 2px solid var(--tile-hue, var(--secondary));
  border-radius: var(--radius);
  box-shadow: var(--shadow-sm);
  overflow: hidden;
  transition: background-color 0.2s ease, transform 0.2s ease, box-shadow 0.2s ease;
}
.tile:hover {
  background: var(--panel-hover);
  transform: translateY(-1px);
  box-shadow: var(--shadow);
}
.tile-glow {
  display: none;
}
.tile-top {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.tile-badge {
  min-width: 22px;
  padding: 2px 8px;
  border-radius: 999px;
  background: var(--tile-hue, var(--secondary));
  color: #0c0e11;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.02em;
  text-align: center;
  box-shadow: 0 0 12px color-mix(in srgb, var(--tile-hue, var(--secondary)) 55%, transparent);
}
.tile-tag {
  font-size: 10px;
  letter-spacing: 0.14em;
  color: var(--text-dim);
}
.tile-status {
  font-size: 9px;
  color: var(--secondary);
  letter-spacing: 0.08em;
}
.tile-name {
  font-size: 22px;
  font-weight: var(--fw-800);
  margin: 16px 0 8px;
  letter-spacing: 0.01em;
  color: var(--text);
}
.tile-desc {
  font-size: 11.5px;
  color: var(--text-dim);
  line-height: 1.5;
  min-height: 34px;
}
.tile-launch {
  display: inline-flex;
  align-items: center;
  margin-top: 16px;
  padding: 8px 14px;
  font-size: 11px;
  font-weight: var(--fw-700);
  letter-spacing: 0.12em;
  color: var(--accent);
  text-transform: uppercase;
  border: 1px solid var(--border-accent);
  border-radius: 2px;
  background: linear-gradient(180deg, rgba(255, 255, 255, 0.04) 0%, rgba(0, 0, 0, 0.15) 100%);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.04);
  transition: color 0.16s ease, border-color 0.16s ease, box-shadow 0.2s ease,
    background-color 0.2s ease;
}
.tile:hover .tile-launch {
  color: var(--accent-hover, var(--accent));
  border-color: var(--accent);
  background: linear-gradient(180deg, rgba(46, 227, 198, 0.1) 0%, rgba(0, 0, 0, 0.2) 100%);
  box-shadow: 0 0 18px -4px var(--accent-glow), inset 0 1px 0 rgba(255, 255, 255, 0.07);
  text-shadow: 0 0 8px var(--accent-glow);
}

/* e-ink: kill all hues to grayscale */
html[data-theme='eink'] .tile,
html[data-theme='eink'] .tile-tag,
html[data-theme='eink'] .tile-launch {
  --tile-hue: #000 !important;
  color: inherit;
}
html[data-theme='eink'] .tile-glow {
  display: none;
}
html[data-theme='eink'] .banner-big,
html[data-theme='eink'] .stat-value {
  text-shadow: none;
}

@media (max-width: 900px) {
  .stats {
    grid-template-columns: repeat(2, 1fr);
  }
}
.ns-block {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  grid-column: span 1;
  height: 100%;
  padding: 16px;
  background: var(--panel);
  border: 1px solid var(--border-subtle);
  border-radius: var(--radius);
  box-shadow: var(--shadow-sm);
  cursor: pointer;
  transition: background-color 0.18s ease, transform 0.18s ease;
}
.ns-block:hover {
  background: var(--panel-hover);
  transform: translateY(-1px);
}
.ns-left {
  display: flex;
  align-items: center;
  gap: 12px;
}
.ns-glyph {
  font-size: 22px;
  color: var(--secondary);
  line-height: 1;
}
.ns-title {
  font-size: 13px;
  letter-spacing: 0.08em;
  color: var(--text);
  font-weight: var(--fw-700);
}
.ns-count {
  display: inline-block;
  margin-left: 8px;
  padding: 2px 8px;
  font-size: 10px;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
  color: var(--secondary);
  background: var(--accent-soft);
  border: 1px solid var(--border-accent);
  border-radius: 999px;
  vertical-align: middle;
}
.ns-sub {
  font-size: 11px;
  color: var(--text-dim);
  margin-top: 2px;
}
.ns-right {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  width: 100%;
}
.ns-right .ns-reg {
  width: 100%;
}
.ns-mine {
  font-family: var(--font-mono, monospace);
  font-size: 12px;
  color: var(--secondary);
  font-weight: var(--fw-700);
  word-break: break-all;
}
.ns-arrow {
  display: none;
}

</style>
