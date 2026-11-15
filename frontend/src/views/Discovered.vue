<script setup lang="ts">
import { onMounted, computed, ref, reactive, watch } from 'vue'
import { useRoute } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useAuthStore } from '@/store/auth'
import { useAppsStore } from '@/store/apps'
import { useTabsStore } from '@/store/tabs'
import { useDiscoveryStore } from '@/store/discovery'
import { useUiStore } from '@/store/ui'
import { coverStyle } from '@/lib/coverArt'
import { shortAddr } from '@/lib/crypto'
import { appLogo, appPreview, discoveryLogo, discoveryPreview, uiLog } from '@/lib/bridge'
import Identicon from '@/components/Identicon.vue'
import StorageQuotaModal from '@/components/StorageQuotaModal.vue'
import FirstVisitHint from '@/components/FirstVisitHint.vue'

const { t } = useI18n()
const route = useRoute()
const auth = useAuthStore()
const apps = useAppsStore()
const tabs = useTabsStore()
const discovery = useDiscoveryStore()
const ui = useUiStore()

const activeTag = ref('')
const query = ref('')
// «Не показывать установленные» — тумблер в шапке DISCOVER APPS_.
// Состояние сохраняется в localStorage (проще, чем через sled) и мгновенно
// перезаписывается при каждом переключении, чтобы после перезапуска клиента
// пользователь получал прежнюю раскладку каталога.
const HIDE_INSTALLED_KEY = 'ntfry.discover.hideInstalled'
const hideInstalled = ref<boolean>((() => {
  try { return localStorage.getItem(HIDE_INSTALLED_KEY) === '1' } catch { return false }
})())
// «Подробно» — id приложения, для которого открыта модалка (null = закрыта).
const detailsFor = ref<string | null>(null)
const detailApp = computed(() =>
  detailsFor.value ? discovery.apps.find((a) => a.id === detailsFor.value) ?? null : null,
)
// Приём поискового запроса с дашборда (?q=…): подставляем и сразу применяем.
function applyRouteQuery() {
  const q = route.query.q
  if (typeof q === 'string' && q) query.value = q
}
applyRouteQuery()
watch(() => route.query.q, applyRouteQuery)
// logo cache: installed apps expose a real logo on disk; non-installed apps
// with an announced logoHash lazily fetch the PNG blob over P2P (cached in
// data/discovery_cache); everything else falls back to a deterministic
// neon identicon so the card icon never blocks the UI.
const logos = reactive<Record<string, string>>({})
// DAPP Preview 440×180 — обложка карточки вместо градиента (fallback — градиент)
const previews = reactive<Record<string, string>>({})

// одноразовые диагностические записи в SYSLOG (dedup по ключу, без спама)
const uiLogged = new Set<string>()
function logOnce(key: string, msg: string) {
  if (uiLogged.has(key)) return
  uiLogged.add(key)
  uiLog('ui', msg)
}

// Ключ последней попытки сетевой загрузки (hash + «есть ли провайдеры»):
// повторяем fetch ТОЛЬКО когда изменились вводные, а уже показанную картинку
// никогда не перетираем пустотой — кэш «сбрасывается» лишь успешной новой
// загрузкой (v1.135.18). Rust-сторона при пустом хэше/офлайне отдаёт
// последний закэшированный файл.
const logoTry: Record<string, string> = {}
const previewTry: Record<string, string> = {}

async function loadLogos() {
  // ВАЖНО: все загрузки параллельны и независимы — локальные чтения (installed)
  // не ждут медленные P2P-попытки по офлайн-пирам (до ~10с на приложение).
  const jobs: Promise<void>[] = []
  for (const a of paged.value) {
    const ids = (a.providers || []).map((p) => p.nodeId).filter(Boolean)
    // ── logo ──
    if (a.installed) {
      if (logos[a.id] === undefined) {
        // локальный файл; у старых установок его может не быть — fallback на P2P-блоб
        jobs.push(
          appLogo(a.id).then(async (url) => {
            if (url) {
              logos[a.id] = url
              return
            }
            if (!a.private) {
              logos[a.id] = ''
              const u = await discoveryLogo(a.id, a.logoHash, ids)
              if (u) logos[a.id] = u
            }
          }),
        )
      }
    } else if (!a.private) {
      const key = `${a.logoHash}|${ids.length ? 1 : 0}`
      if (logoTry[a.id] !== key) {
        logoTry[a.id] = key
        if (logos[a.id] === undefined) logos[a.id] = ''
        jobs.push(
          discoveryLogo(a.id, a.logoHash, ids).then((url) => {
            if (url) logos[a.id] = url
          }),
        )
      }
    }
    // ── preview 440×180 ──
    if (a.installed) {
      if (previews[a.id] === undefined) {
        // установлено старой версией без файла превью → тянем блоб по анонсу
        jobs.push(
          appPreview(a.id).then(async (url) => {
            logOnce(
              `pv:${a.id}:${url ? url.length : 0}`,
              `preview ${a.id.slice(0, 12)}… — invoke вернул ${url ? url.length + ' симв.' : 'null'}`,
            )
            if (url) {
              previews[a.id] = url
              return
            }
            if (!a.private) {
              previews[a.id] = ''
              const u = await discoveryPreview(a.id, a.previewHash, ids)
              if (u) previews[a.id] = u
            }
          }),
        )
      }
    } else if (!a.private) {
      const key = `${a.previewHash}|${ids.length ? 1 : 0}`
      if (previewTry[a.id] !== key) {
        previewTry[a.id] = key
        if (previews[a.id] === undefined) previews[a.id] = ''
        jobs.push(
          discoveryPreview(a.id, a.previewHash, ids).then((url) => {
            if (url) previews[a.id] = url
          }),
        )
      }
    }
  }
  await Promise.all(jobs)
}

onMounted(() => {
  if (!apps.loaded) apps.init()
  if (auth.isTauri) discovery.scan()
})

// all tags across discovered apps (for the filter bar)
const allTags = computed(() => {
  const s = new Set<string>()
  discovery.searchApps.forEach((a) => a.tags.forEach((tg) => s.add(tg)))
  return Array.from(s).sort()
})
// Ограничиваем панель фильтра тегов: максимум 40 (иначе при тысячах dApp она разрастается).
const MAX_TAGS = 40
const visibleTags = computed(() => allTags.value.slice(0, MAX_TAGS))

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase()
  return discovery.searchApps.filter((a) => {
    // Тумблер «Не показывать установленные» — фильтруем локально, без
    // пересканирования сети. Приватные dApps не скрываем даже установленными:
    // владелец должен видеть их в каталоге для запуска и переустановки.
    if (hideInstalled.value && a.installed && !(a as any).private) return false
    if (activeTag.value && !a.tags.includes(activeTag.value)) return false
    if (!q) return true
    return (
      a.title.toLowerCase().includes(q) ||
      a.description.toLowerCase().includes(q) ||
      a.id.toLowerCase().includes(q) ||
      a.tags.some((tg) => tg.toLowerCase().includes(q))
    )
  })
})

// pagination — there can be thousands of apps, show a page at a time
const page = ref(1)
const pageSize = 12
const totalPages = computed(() => Math.max(1, Math.ceil(filtered.value.length / pageSize)))
const paged = computed(() => {
  if (page.value > totalPages.value) page.value = totalPages.value
  const start = (page.value - 1) * pageSize
  return filtered.value.slice(start, start + pageSize)
})
function goPage(p: number) {
  page.value = Math.min(Math.max(1, p), totalPages.value)
}

// reset to page 1 whenever the search query changes
watch(query, () => {
  page.value = 1
})

// «Не показывать установленные» — сохраняем состояние и сбрасываем пагинацию.
watch(hideInstalled, (v) => {
  try { localStorage.setItem(HIDE_INSTALLED_KEY, v ? '1' : '0') } catch { /* приватный режим — молча */ }
  page.value = 1
})

/** Pretty "last updated" date for the card pill ("" => no pill). Accepts an
 *  epoch-seconds string (manifest/announce format) or an ISO date. */
function fmtDate(v: string): string {
  if (!v) return ''
  let ms: number
  if (/^\d+$/.test(v)) {
    ms = Number(v) * (v.length <= 11 ? 1000 : 1) // 10-digit epoch secs => ms
  } else {
    const parsed = new Date(v)
    if (isNaN(parsed.getTime())) return ''
    ms = parsed.getTime()
  }
  const d = new Date(ms)
  if (isNaN(d.getTime()) || ms <= 0) return ''
  return d.toLocaleDateString(undefined, { day: '2-digit', month: 'short', year: 'numeric' })
}

function toggleTag(tg: string) {
  activeTag.value = activeTag.value === tg ? '' : tg
  page.value = 1
}

watch(paged, loadLogos, { immediate: true })

async function install(id: string) {
  const ok = await discovery.install(id)
  if (ok) tabs.open(id, id) // open the freshly installed (now verified) app
}

function launch(id: string) {
  tabs.open(id, id)
}

const copiedToast = ref(false)
async function copyLink(id: string) {
  try {
    await navigator.clipboard.writeText(`sth://${id}`)
  } catch {
    // fallback for restricted clipboard contexts
    const ta = document.createElement('textarea')
    ta.value = `sth://${id}`
    document.body.appendChild(ta)
    ta.select()
    document.execCommand('copy')
    document.body.removeChild(ta)
  }
  copiedToast.value = true
  setTimeout(() => (copiedToast.value = false), 1800)
}

// ── Соглашение о непроверенных dApps: модалка рисков + ончейн-оплата ──
const riskModal = ref(false)
const acceptToast = ref(false)
// Полная стоимость разблокировки: FEE_ALLOW_ALL_DAPPS + перевод 1 STH
const unvTotalFmt = computed(() => (discovery.unverifiedFeeSth + 1).toLocaleString())

function openRiskModal() {
  discovery.acceptError = ''
  riskModal.value = true
}

async function confirmRisk() {
  const ok = await discovery.acceptUnverified()
  if (ok) {
    riskModal.value = false
    acceptToast.value = true
    setTimeout(() => (acceptToast.value = false), 3500)
  }
}

function riskErrText(e: string): string {
  if (e.includes('INSUFFICIENT_FUNDS')) return t('discover.riskNoFunds', { n: unvTotalFmt.value })
  return `${t('discover.riskError')}: ${e}`
}

// ── Золотой бейдж: приватные dApps, доступные пользователю ──
const privListOpen = ref(false)
function openPrivate(p: { id: string; installed: boolean }) {
  privListOpen.value = false
  if (p.installed) launch(p.id)
  else detailsFor.value = p.id
}
</script>

<template>
  <div class="discover" data-testid="discover-view">
    <div class="head">
      <h1 class="page-title">// {{ t('discover.title') }}_</h1>
      <div class="head-actions">
        <!-- Золотой бейдж: показывается ТОЛЬКО когда пользователю доступны
             приватные dApps (владелец или адрес из списка доступа). Список
             не зависит от фильтра Official Repository. -->
        <div v-if="auth.isTauri && discovery.privateApps.length" class="priv-wrap">
          <button
            class="priv-gold"
            @click="privListOpen = !privListOpen"
            data-testid="discover-private-badge"
          >
            <span class="priv-lock">🔒</span>
            {{ t('discover.privFound', { n: discovery.privateApps.length }) }}
          </button>
          <div v-if="privListOpen" class="priv-overlay" @click="privListOpen = false"></div>
          <div v-if="privListOpen" class="priv-drop" data-testid="discover-private-list">
            <div class="priv-drop-title">{{ t('discover.privListTitle') }}</div>
            <button
              v-for="p in discovery.privateApps"
              :key="p.id"
              class="priv-item"
              @click="openPrivate(p)"
              :data-testid="`priv-item-${p.id}`"
            >
              <Identicon :seed="p.id" :size="22" />
              <span class="priv-item-name">{{ p.title || t('discover.noTitle') }}</span>
              <span v-if="p.installed" class="priv-item-inst">✔ {{ t('discover.open') }}</span>
              <span v-else class="priv-item-seeds">● {{ p.seeds }} {{ t('discover.seeds') }}</span>
            </button>
          </div>
        </div>
        <!-- «Не показывать установленные» — тумблер запоминается в
             localStorage и мгновенно перерисовывает каталог. Приватные
             dApps остаются видимыми, даже если установлены. -->
        <label
          v-if="auth.isTauri"
          class="hide-inst-toggle"
          :title="t('discover.hideInstalledHint')"
          data-testid="discover-hide-installed-toggle"
        >
          <input
            type="checkbox"
            v-model="hideInstalled"
            data-testid="discover-hide-installed-checkbox"
          />
          <span class="hide-inst-track"><span class="hide-inst-thumb"></span></span>
          <span class="hide-inst-label">{{ t('discover.hideInstalled') }}</span>
        </label>
        <button
          v-if="auth.isTauri"
          class="scan-pill"
          :class="{ scanning: discovery.scanning }"
          :disabled="discovery.scanning"
          @click="discovery.scan()"
          data-testid="discover-rescan-btn"
        >
          <span v-if="discovery.scanning" class="scan-pill-spin">⟳</span>
          {{ discovery.scanning ? t('discover.scanning') : t('discover.rescan') }}
        </button>
      </div>
    </div>

    <!-- Верхняя подсказка о mDNS/iroh-blobs/подписи убрана: техно-жаргон
         пугает обычного пользователя. Всё, что нужно знать — «Установи и
         запусти». Технические детали доступны в модалке «Подробно». -->

    <!-- search bar (sticky — как в Google, всегда перед глазами) -->
    <div class="search-sticky" data-testid="discover-search-sticky">
      <div class="search-row" data-testid="discover-search-row">
        <svg class="search-ico" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="11" cy="11" r="8"></circle>
          <line x1="21" y1="21" x2="16.65" y2="16.65"></line>
        </svg>
        <input
          v-model="query"
          class="search-input"
          type="text"
          :placeholder="t('discover.searchPlaceholder')"
          data-testid="discover-search-input"
        />
        <button
          v-if="query"
          class="search-clear"
          @click="query = ''"
          data-testid="discover-search-clear"
          aria-label="clear"
        >✕</button>
        <span
          v-if="auth.isTauri"
          class="search-count"
          data-testid="discover-search-count"
        >{{ t('discover.found', { n: filtered.length }) }}</span>
      </div>
    </div>

    <!-- Подсказка новичку при первом открытии магазина -->
    <FirstVisitHint storage-key="sth_hint_store_seen" title-key="hints.storeTitle" text-key="hints.storeText" />

    <!-- Official Repository: DEV/GEEK могут показать непроверенные приложения -->
    <div v-if="ui.mode !== 'user'" class="official-row" data-testid="discover-official-row">
      <span class="official-note">
        🛡 {{ discovery.showUnverified && discovery.unverifiedAccepted ? t('discover.unverifiedShown') : t('discover.officialOnly') }}
      </span>
      <label v-if="discovery.unverifiedAccepted" class="unv-toggle" data-testid="discover-unverified-toggle">
        <input
          type="checkbox"
          :checked="discovery.showUnverified"
          @change="discovery.toggleShowUnverified(($event.target as HTMLInputElement).checked)"
          data-testid="discover-unverified-checkbox"
        />
        {{ t('discover.showUnverified') }}
      </label>
      <button
        v-else
        class="unv-unlock"
        @click="openRiskModal"
        data-testid="discover-show-unknown-btn"
      >
        ⚠ {{ t('discover.riskBtn') }}
      </button>
    </div>


    <!-- tag filter bar (макс. 40 тегов) -->
    <div v-if="ui.mode !== 'user' && allTags.length" class="tag-filter" data-testid="discover-tag-filter">
      <button
        class="filter-tag"
        :class="{ active: !activeTag }"
        @click="activeTag = ''; page = 1"
        data-testid="filter-tag-all"
      >
        {{ t('discover.allTags') }}
      </button>
      <button
        v-for="tg in visibleTags"
        :key="tg"
        class="filter-tag"
        :class="{ active: activeTag === tg }"
        @click="toggleTag(tg)"
        :data-testid="`filter-tag-${tg}`"
      >
        #{{ tg }}
      </button>
      <span
        v-if="allTags.length > visibleTags.length"
        class="filter-more dim"
        data-testid="filter-tag-more"
      >+{{ allTags.length - visibleTags.length }}</span>
    </div>

    <p v-if="discovery.lastError" class="install-error" data-testid="discover-error">
      ⚠ {{ discovery.lastError === 'no-provider' ? t('discover.noProvider') : t('discover.installError') + ': ' + discovery.lastError }}
    </p>

    <div v-if="discovery.installingId" class="downloading" data-testid="discover-downloading">
      <span class="spinner">⟳</span>
      {{ discovery.installingSeeds > 0 ? t('discover.downloadingFrom', { n: discovery.installingSeeds }) : t('discover.downloading') }}
    </div>

    <!-- Fallback-скан: обычные P2P-сидеры недоступны → читаем облачные диски
         известных Cloud Seeder-ов (dApps появятся с бейджем «☁ через сидер»). -->
    <div v-if="discovery.cloudScanning" class="downloading cloud-scan" data-testid="discover-cloud-scanning">
      <span class="spinner">⟳</span>
      ☁ {{ t('discover.cloudScanning') }}
    </div>
    <div
      v-else-if="auth.isTauri && !discovery.hasLiveP2P && discovery.apps.length && discovery.apps.every((a) => a.viaSeeder)"
      class="cloud-note"
      data-testid="discover-cloud-note"
    >
      ☁ {{ t('discover.cloudSourced') }}
    </div>

    <!-- non-native notice -->
    <div v-if="!auth.isTauri" class="empty panel" data-testid="discover-web-note">
      <span class="empty-mark">[ web ]</span>
      <span class="dim">{{ t('discover.webOnly') }}</span>
    </div>

    <!-- discovered apps -->
    <div v-else-if="filtered.length" class="grid" data-testid="discover-list">
      <div
        v-for="a in paged"
        :key="a.id"
        class="disc-card panel"
        :class="{ installed: a.installed }"
        :data-testid="`disc-${a.id}`"
      >
        <!-- Цветная обложка-витрина: клик открывает приложение (или детали) -->
        <!-- Шапка-обложка: иконка слева + крупный заголовок. Клик открывает приложение (или детали). -->
        <div
          class="disc-cover"
          :style="coverStyle(a.id)"
          :data-testid="`disc-icon-${a.id}`"
          role="button"
          @click="a.installed ? launch(a.id) : (detailsFor = a.id)"
        >
          <!-- красная точка: непроверенное приложение (без текста, тултип) -->
          <span
            v-if="!a.official"
            class="disc-unv-dot"
            :title="t('discover.unverifiedDot')"
            :data-testid="`disc-unverified-${a.id}`"
          />
          <span class="disc-cover-plate">
            <img v-if="logos[a.id]" :src="logos[a.id]" alt="" class="disc-cover-logo" />
            <Identicon v-else :seed="a.id" :size="28" />
          </span>
          <span
            class="disc-cover-title"
            :class="{ untitled: !a.title }"
            :title="a.installed ? t('discover.open') : t('discover.details')"
            :data-testid="`disc-title-${a.id}`"
          >
            {{ a.title || t('discover.noTitle') }}
            <span
              v-if="a.installedVersion || a.netVersion"
              class="disc-ver"
              :data-testid="`disc-ver-${a.id}`"
            >[v{{ a.installedVersion || a.netVersion }}]</span>
          </span>
        </div>
        <!-- DAPP Preview под шапкой: картинка 440×180 или градиент-fallback.
             Статусы приложения (official/private/installed/online) — поверх превью. -->
        <div
          class="disc-preview"
          :style="previews[a.id] ? undefined : coverStyle(a.id)"
          role="button"
          :data-testid="`disc-preview-${a.id}`"
          @click="a.installed ? launch(a.id) : (detailsFor = a.id)"
        >
          <img
            v-if="previews[a.id]"
            :src="previews[a.id]"
            alt=""
            class="disc-cover-preview"
            @load="logOnce(`pvload:${a.id}`, `preview ${a.id.slice(0, 12)}… — <img> декодирован и отрисован`)"
            @error="logOnce(`pverr:${a.id}`, `preview ${a.id.slice(0, 12)}… — ОШИБКА декодирования <img>`)"
          />
          <div class="disc-top disc-top-overlay">
            <span
              v-if="a.official"
              class="disc-official"
              :title="t('discover.officialHint')"
              :data-testid="`disc-official-${a.id}`"
            >✔</span>
            <!-- Приватный dApp: виден только адресам из списка доступа. -->
            <span
              v-if="(a as any).private"
              class="disc-private"
              :title="t('apps.privBadge')"
              :data-testid="`disc-private-${a.id}`"
            >🔒</span>
            <!-- Крохотная зелёная точка вместо «✓ УСТАНОВЛЕНО»-подписи. -->
            <span
              v-if="a.installed"
              class="disc-installed-dot"
              :title="t('discover.installed')"
              :data-testid="`disc-installed-${a.id}`"
            />
            <!-- USER: просто зелёная точка Онлайн/Офлайн; DEV/GEEK — счётчик сидов. -->
            <span
              v-if="ui.mode === 'user'"
              class="disc-online-dot"
              :class="{ off: !a.seeds }"
              :title="a.seeds ? t('discover.online') : t('discover.offline')"
              :data-testid="`disc-seeds-${a.id}`"
            />
            <span v-else class="disc-seeds hl" :data-testid="`disc-seeds-${a.id}`">
              ● {{ a.seeds }} {{ t('discover.seeds') }}
            </span>
          </div>
          <!-- Fallback без превью: тип (free|paid|private) и теги поверх
               градиента — карточка не выглядит пустой (v1.135.18) -->
          <div
            v-if="!previews[a.id]"
            class="disc-nopreview"
            :data-testid="`disc-nopreview-${a.id}`"
          >
            <span
              class="np-type"
              :class="a.private ? 'private' : a.paid ? 'paid' : 'free'"
              :data-testid="`disc-type-${a.id}`"
            >{{ a.private ? t('discover.typePrivate') : a.paid ? t('discover.typePaid') : t('discover.typeFree') }}</span>
            <div v-if="a.tags.length" class="np-tags" :data-testid="`disc-nptags-${a.id}`">
              <span v-for="tg in a.tags.slice(0, 5)" :key="tg" class="np-tag">#{{ tg }}</span>
            </div>
          </div>
          <!-- Описание — при наличии превью выезжает при наведении; без превью
               видно постоянно (та же полоска, класс static) -->
          <div
            v-if="a.description"
            class="disc-desc-strip"
            :class="{ static: !previews[a.id] }"
            :data-testid="`disc-desc-${a.id}`"
          >
            {{ a.description }}
          </div>
          <!-- «via seeder» — пилюля в правом нижнем углу превью (как seeds) -->
          <div
            v-if="a.viaSeeder"
            class="disc-top disc-top-overlay disc-seeder-overlay"
            :data-testid="`disc-via-seeder-${a.id}`"
          >
            <span class="disc-seeds hl">☁ {{ t('discover.viaSeeder') }}</span>
          </div>
        </div>

        <div class="disc-actions disc-actions-clean">
          <!-- ①  Кнопки OPEN больше нет: открытие — кликом по title/preview.
               Для неустановленных остаётся INSTALL. -->
          <button
            v-if="!a.installed"
            class="btn btn-install disc-primary"
            :disabled="discovery.installingId === a.id"
            @click="install(a.id)"
            :data-testid="`disc-install-${a.id}`"
          >
            {{ discovery.installingId === a.id ? t('discover.installing') : t('discover.install') }} ↓
          </button>
          <!-- размер dApp — в одну строку перед иконкой details -->
          <span v-if="a.sizeBytes > 0" class="disc-badge weight" :data-testid="`disc-weight-${a.id}`">
            ⛁ {{ (a.sizeBytes / 1048576).toFixed(1) }} {{ t('discover.mb') }}
          </span>
          <!-- ②  Подробно — иконка ⓘ во всех режимах. -->
          <button
            class="disc-copy-square disc-details-icon"
            :title="t('discover.details')"
            @click="detailsFor = a.id"
            :data-testid="`disc-details-${a.id}`"
            aria-label="Details"
          >
            <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <circle cx="12" cy="12" r="10"></circle>
              <line x1="12" y1="16" x2="12" y2="12"></line>
              <line x1="12" y1="8" x2="12.01" y2="8"></line>
            </svg>
          </button>
          <!-- ③  Квадратная кнопка «Копировать ссылку». -->
          <button
            class="disc-copy-square"
            :title="t('discover.copyLink')"
            @click="copyLink(a.id)"
            :data-testid="`disc-copy-${a.id}`"
            aria-label="Copy link"
          >
            <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect>
              <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
            </svg>
          </button>
        </div>
      </div>
    </div>

    <!-- empty -->
    <div v-else class="empty panel" data-testid="discover-empty">
      <span class="empty-mark">[ 0x00 ]</span>
      <span class="dim">{{ discovery.cloudScanning ? t('discover.cloudScanning') : t('discover.none') }}</span>
      <span v-if="!discovery.hasLiveP2P && !discovery.cloudScanning" class="dim cloud-empty-hint" data-testid="discover-cloud-empty-hint">
        {{ t('discover.cloudEmptyHint') }}
      </span>
    </div>

    <!-- pagination -->
    <div v-if="filtered.length > pageSize" class="pager" data-testid="discover-pager">
      <button class="pager-btn" :disabled="page <= 1" @click="goPage(page - 1)" data-testid="pager-prev">
        ← {{ t('discover.prev') }}
      </button>
      <span class="pager-info dim" data-testid="pager-info">
        {{ t('discover.pageOf', { page, total: totalPages }) }}
      </span>
      <button class="pager-btn" :disabled="page >= totalPages" @click="goPage(page + 1)" data-testid="pager-next">
        {{ t('discover.next') }} →
      </button>
    </div>

    <StorageQuotaModal />

    <transition name="toast">
      <div v-if="copiedToast" class="copy-toast" data-testid="link-copied-toast">
        ✓ {{ t('discover.linkCopied') }}
      </div>
    </transition>

    <!-- Installed-toast: показывает источник инсталла (P2P vs Cloud Seeder),
         чтобы пользователь наглядно видел работу анти-цензурного fallback'а. -->
    <transition name="toast">
      <div
        v-if="discovery.installedFlash"
        class="install-toast"
        :class="{ 'via-seeder': discovery.lastInstallSource === 'seeder' }"
        data-testid="install-source-toast"
      >
        <span class="it-src" data-testid="install-source-badge">
          {{ discovery.lastInstallSource === 'seeder' ? t('discover.srcSeeder') : t('discover.srcP2P') }}
        </span>
        <span class="it-msg">
          {{ t('discover.installedVia', { name: discovery.installedFlash }) }}
        </span>
      </div>
    </transition>

    <!-- «Подробно» — модалка с полной технической информацией о приложении.
         Именно сюда переехали paid / boost / republish / updated / sth://-адрес
         и прочие тех-плашки, чтобы карточка в каталоге оставалась чистой. -->
    <transition name="fade">
      <div
        v-if="detailApp"
        class="disc-modal-back"
        @click.self="detailsFor = null"
        data-testid="disc-details-modal"
      >
        <div class="disc-modal panel" role="dialog" aria-modal="true">
          <button class="disc-modal-x" @click="detailsFor = null" aria-label="Close">✕</button>
          <div class="disc-modal-head">
            <span class="disc-modal-icon">
              <img v-if="logos[detailApp.id]" :src="logos[detailApp.id]" alt="" />
              <Identicon v-else :seed="detailApp.id" :size="36" />
            </span>
            <div class="disc-modal-titlewrap">
              <h2 class="disc-modal-title">{{ detailApp.title || t('discover.noTitle') }}</h2>
              <span v-if="detailApp.installedVersion || detailApp.netVersion" class="disc-modal-ver">
                v{{ detailApp.installedVersion || detailApp.netVersion }}
              </span>
            </div>
          </div>
          <p class="disc-modal-desc">{{ detailApp.description || t('discover.noDescription') }}</p>
          <div class="disc-modal-rows">
            <div class="disc-modal-row">
              <span class="dim">{{ t('discover.seedsRow') }}</span>
              <b class="hl">● {{ detailApp.seeds }}</b>
            </div>
            <div v-if="detailApp.sizeBytes > 0" class="disc-modal-row">
              <span class="dim">{{ t('discover.sizeRow') }}</span>
              <b>{{ (detailApp.sizeBytes / 1048576).toFixed(1) }} {{ t('discover.mb') }}</b>
            </div>
            <div v-if="detailApp.paid" class="disc-modal-row">
              <span class="dim">{{ t('discover.verifiedRow') }}</span>
              <b class="hl">✔ {{ t('discover.verified') }}</b>
            </div>
            <!-- Official/Private Repository: статус проверки + разработчик -->
            <div class="disc-modal-row" data-testid="disc-modal-official">
              <span class="dim">{{ t('discover.officialRow') }}</span>
              <b v-if="detailApp.private" class="priv-repo" data-testid="disc-modal-private-repo">🔒 Private Repository</b>
              <b v-else-if="detailApp.official" class="official-yes">✔ Official Repository</b>
              <b v-else class="official-no">⚠ {{ t('discover.notVerified') }}</b>
            </div>
            <div v-if="detailApp.owner" class="disc-modal-row" data-testid="disc-modal-owner">
              <span class="dim">{{ t('discover.developerRow') }}</span>
              <router-link
                class="disc-modal-dev"
                :to="`/profile/${detailApp.owner}`"
                :title="detailApp.owner"
                @click="detailsFor = null"
                data-testid="disc-modal-dev-link"
              >{{ shortAddr(detailApp.owner, 10, 6) }}</router-link>
            </div>
            <div v-if="detailApp.boostPool > 0" class="disc-modal-row">
              <span class="dim">{{ t('discover.boostRow') }}</span>
              <b>⚡ {{ (detailApp.boostPool / 1e8).toFixed(0) }} STH</b>
            </div>
            <div v-if="detailApp.cost > 0" class="disc-modal-row">
              <span class="dim">{{ t('discover.republishRow') }}</span>
              <b>↻ {{ detailApp.cost }} STH</b>
            </div>
            <div v-if="fmtDate(detailApp.updated)" class="disc-modal-row">
              <span class="dim">{{ t('discover.lastUpdated') }}</span>
              <b>🕒 {{ fmtDate(detailApp.updated) }}</b>
            </div>
            <div class="disc-modal-row disc-modal-url">
              <span class="dim">sth://</span>
              <button
                class="disc-modal-copyurl"
                @click="copyLink(detailApp.id)"
                :title="t('discover.copyLink')"
                :data-testid="`disc-modal-copy-${detailApp.id}`"
              >
                {{ shortAddr(detailApp.id, 14, 10) }}
                <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                  <rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect>
                  <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
                </svg>
              </button>
            </div>
          </div>
          <div v-if="detailApp.tags.length" class="disc-modal-tags">
            <span
              v-for="tg in detailApp.tags"
              :key="tg"
              class="card-tag"
            >#{{ tg }}</span>
          </div>
        </div>
      </div>
    </transition>
    <!-- Соглашение о непроверенных dApps: модалка «зона повышенного риска».
         Разблокировка платная: ончейн-tx fee=FEE_ALLOW_ALL_DAPPS + 1 STH на
         эскроу с vendorField=NETFORY_UNVERIFIED_ACCEPT — цифровое согласие
         навсегда фиксируется в блокчейне SmartHoldem. -->
    <transition name="fade">
      <div
        v-if="riskModal"
        class="disc-modal-back"
        @click.self="!discovery.acceptBusy && (riskModal = false)"
        data-testid="risk-modal"
      >
        <div class="risk-modal panel" role="dialog" aria-modal="true">
          <button
            class="disc-modal-x"
            :disabled="discovery.acceptBusy"
            @click="riskModal = false"
            aria-label="Close"
            data-testid="risk-close-btn"
          >✕</button>
          <div class="risk-head">
            <span class="risk-sign">⚠</span>
            <h2 class="risk-title" data-testid="risk-title">{{ t('discover.riskTitle') }}</h2>
          </div>
          <p class="risk-intro">{{ t('discover.riskIntro') }}</p>
          <p class="risk-sub">{{ t('discover.riskWhatTitle') }}</p>
          <ul class="risk-list">
            <li>{{ t('discover.riskItem1') }}</li>
            <li>{{ t('discover.riskItem2') }}</li>
            <li>{{ t('discover.riskItem3') }}</li>
          </ul>
          <div class="risk-disclaimer">
            <b>{{ t('discover.riskDisclaimerTitle') }}</b>
            <p>{{ t('discover.riskDisclaimer') }}</p>
          </div>
          <p class="risk-onchain">{{ t('discover.riskOnchain') }}</p>
          <p v-if="discovery.acceptError" class="risk-error" data-testid="risk-error">
            ⚠ {{ riskErrText(discovery.acceptError) }}
          </p>
          <div class="risk-actions">
            <button
              class="risk-pay"
              :disabled="discovery.acceptBusy"
              @click="confirmRisk"
              data-testid="risk-pay-btn"
            >
              <span v-if="discovery.acceptBusy" class="spinner">⟳</span>
              {{ discovery.acceptBusy ? t('discover.riskPaying') : t('discover.riskPay', { n: unvTotalFmt }) }}
            </button>
            <button
              class="risk-cancel"
              :disabled="discovery.acceptBusy"
              @click="riskModal = false"
              data-testid="risk-cancel-btn"
            >
              {{ t('discover.riskCancel') }}
            </button>
          </div>
          <p class="risk-feenote" data-testid="risk-fee-note">
            {{ t('discover.riskFeeNote', { fee: discovery.unverifiedFeeSth.toLocaleString() }) }}
          </p>
        </div>
      </div>
    </transition>

    <transition name="toast">
      <div v-if="acceptToast" class="copy-toast risk-toast" data-testid="risk-accepted-toast">
        ✓ {{ t('discover.riskAccepted') }}
      </div>
    </transition>
  </div>
</template>

<style scoped>
.discover {
  max-width: none;
  width: 100%;
  display: flex;
  flex-direction: column;
  gap: 18px;
}
.head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}
.page-title {
  font-size: 15px;
  letter-spacing: 0.14em;
  color: var(--accent);
  font-weight: var(--fw-700);
}
.head-actions {
  display: flex;
  align-items: center;
  gap: 12px;
}

/* ── «SCAN» — округлённая pill-кнопка (в паре с золотым «PRIVATE FOUND») ── */
.scan-pill {
  position: relative;
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 8px 18px;
  font-size: 11.5px;
  font-weight: 800;
  letter-spacing: 0.09em;
  text-transform: uppercase;
  color: #06121e;
  background: linear-gradient(160deg, var(--accent) 0%, color-mix(in srgb, var(--accent) 72%, #0d1b26) 100%);
  border: 1px solid color-mix(in srgb, var(--accent) 70%, #0d1b26);
  border-radius: 999px;
  cursor: pointer;
  box-shadow:
    0 0 14px -3px color-mix(in srgb, var(--accent) 60%, transparent),
    inset 0 1px 0 rgba(255, 255, 255, 0.35),
    inset 0 -2px 4px rgba(0, 0, 0, 0.18);
  transition: transform 0.15s ease, box-shadow 0.18s ease, filter 0.18s ease;
}
.scan-pill:hover:not(:disabled) {
  transform: translateY(-1px);
  box-shadow:
    0 0 22px -2px color-mix(in srgb, var(--accent) 80%, transparent),
    inset 0 1px 0 rgba(255, 255, 255, 0.5),
    inset 0 -2px 4px rgba(0, 0, 0, 0.18);
  filter: brightness(1.06);
}
.scan-pill:active:not(:disabled) { transform: translateY(0); }
.scan-pill:disabled { opacity: 0.75; cursor: default; }
.scan-pill.scanning { animation: scan-pill-pulse 1.4s ease-in-out infinite; }
.scan-pill-spin { display: inline-block; animation: spin 1s linear infinite; }
@keyframes scan-pill-pulse {
  0%, 100% { box-shadow: 0 0 14px -3px color-mix(in srgb, var(--accent) 60%, transparent), inset 0 1px 0 rgba(255,255,255,0.35), inset 0 -2px 4px rgba(0,0,0,0.18); }
  50%      { box-shadow: 0 0 26px -1px color-mix(in srgb, var(--accent) 90%, transparent), inset 0 1px 0 rgba(255,255,255,0.55), inset 0 -2px 4px rgba(0,0,0,0.18); }
}

/* ── Тумблер «Не показывать установленные» ── */
.hide-inst-toggle {
  display: inline-flex;
  align-items: center;
  gap: 9px;
  cursor: pointer;
  user-select: none;
  padding: 5px 12px 5px 8px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: var(--bg-elevated);
  transition: border-color 0.15s ease, background-color 0.15s ease;
}
.hide-inst-toggle:hover { border-color: var(--accent); }
.hide-inst-toggle input {
  position: absolute;
  opacity: 0;
  pointer-events: none;
  width: 0;
  height: 0;
}
.hide-inst-track {
  position: relative;
  width: 34px;
  height: 18px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--text-dim) 35%, transparent);
  border: 1px solid rgba(255, 255, 255, 0.08);
  transition: background-color 0.18s ease, border-color 0.18s ease, box-shadow 0.18s ease;
  flex: none;
}
.hide-inst-thumb {
  position: absolute;
  top: 1px;
  left: 1px;
  width: 14px;
  height: 14px;
  border-radius: 50%;
  background: #0b1218;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.55), inset 0 1px 0 rgba(255, 255, 255, 0.15);
  transition: transform 0.18s ease, background-color 0.18s ease;
}
.hide-inst-toggle input:checked + .hide-inst-track {
  background: var(--accent);
  border-color: color-mix(in srgb, var(--accent) 70%, transparent);
  box-shadow: 0 0 12px -3px color-mix(in srgb, var(--accent) 80%, transparent);
}
.hide-inst-toggle input:checked + .hide-inst-track .hide-inst-thumb {
  transform: translateX(16px);
  background: #06121e;
}
.hide-inst-toggle input:focus-visible + .hide-inst-track {
  outline: 2px solid color-mix(in srgb, var(--accent) 70%, transparent);
  outline-offset: 2px;
}
.hide-inst-label {
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--text-dim);
  white-space: nowrap;
}
.hide-inst-toggle:hover .hide-inst-label,
.hide-inst-toggle input:checked ~ .hide-inst-label {
  color: var(--text);
}
@media (max-width: 640px) {
  .hide-inst-label { display: none; }
}
/* ── Золотой металлический бейдж приватных dApps ── */
.priv-wrap {
  position: relative;
}
.priv-gold {
  position: relative;
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 8px 16px;
  font-size: 11.5px;
  font-weight: 800;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: #3a2a05;
  background: linear-gradient(160deg, #f9e7a0 0%, #e8c25a 26%, #c89a2e 50%, #f2d783 72%, #b98a24 100%);
  border: 1px solid rgba(255, 223, 128, 0.85);
  border-radius: 999px;
  cursor: pointer;
  overflow: hidden;
  box-shadow:
    0 0 14px rgba(232, 194, 90, 0.35),
    inset 0 1px 0 rgba(255, 255, 255, 0.6),
    inset 0 -2px 4px rgba(122, 84, 10, 0.35);
  transition: transform 0.15s, box-shadow 0.15s;
}
.priv-gold:hover {
  transform: translateY(-1px);
  box-shadow:
    0 0 24px rgba(232, 194, 90, 0.55),
    inset 0 1px 0 rgba(255, 255, 255, 0.7),
    inset 0 -2px 4px rgba(122, 84, 10, 0.35);
}
.priv-gold::after {
  content: '';
  position: absolute;
  top: 0;
  left: -80%;
  width: 55%;
  height: 100%;
  background: linear-gradient(105deg, transparent, rgba(255, 255, 255, 0.75), transparent);
  transform: skewX(-22deg);
  animation: priv-shine 3.4s ease-in-out infinite;
  pointer-events: none;
}
@keyframes priv-shine {
  0% { left: -80%; }
  45% { left: 130%; }
  100% { left: 130%; }
}
.priv-lock {
  font-size: 13px;
  filter: drop-shadow(0 1px 1px rgba(122, 84, 10, 0.4));
}
.priv-overlay {
  position: fixed;
  inset: 0;
  z-index: 40;
}
.priv-drop {
  position: absolute;
  top: calc(100% + 10px);
  right: 0;
  z-index: 41;
  min-width: 300px;
  max-width: 380px;
  max-height: 340px;
  overflow-y: auto;
  padding: 12px;
  background: var(--bg-elevated, rgba(13, 18, 26, 0.97));
  border: 1px solid rgba(232, 194, 90, 0.45);
  border-radius: 12px;
  box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5), 0 0 18px rgba(232, 194, 90, 0.15);
}
.priv-drop-title {
  font-size: 10.5px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: #e8c25a;
  padding: 2px 6px 10px;
}
.priv-item {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 9px 10px;
  background: transparent;
  border: 1px solid transparent;
  border-radius: 9px;
  cursor: pointer;
  text-align: left;
  transition: background-color 0.15s, border-color 0.15s;
}
.priv-item:hover {
  background: rgba(232, 194, 90, 0.08);
  border-color: rgba(232, 194, 90, 0.35);
}
.priv-item-name {
  flex: 1;
  min-width: 0;
  font-size: 12.5px;
  font-weight: 600;
  color: var(--text, #e2e8f0);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.priv-item-inst {
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.06em;
  color: #e8c25a;
  white-space: nowrap;
}
.priv-item-seeds {
  font-size: 10px;
  color: var(--text-dim, #94a3b8);
  white-space: nowrap;
}
.hint {
  font-size: 12px;
  line-height: 1.6;
}
.tag-filter {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}
.filter-tag {
  font-size: 11px;
  padding: 5px 11px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--text-dim);
  cursor: pointer;
  transition: border-color 0.15s, color 0.15s;
}
.filter-tag:hover {
  color: var(--accent);
}
.filter-tag.active {
  color: var(--accent);
  border-color: var(--accent);
}
.card-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.card-tag {
  font-size: 10px;
  padding: 2px 7px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--text-dim);
}
.card-tag.clickable {
  cursor: pointer;
}
.card-tag.clickable:hover {
  color: var(--accent);
  border-color: var(--accent);
}
.grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 16px;
}
@media (max-width: 1280px) {
  .grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}
@media (max-width: 900px) {
  .grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
@media (max-width: 580px) {
  .grid {
    grid-template-columns: 1fr;
  }
}
.disc-card {
  padding: 16px 18px 16px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  border-left: 2px solid var(--secondary);
  transition: border-color 0.18s ease, transform 0.18s ease, box-shadow 0.18s ease;
  position: relative;
  overflow: hidden;
}
.disc-card:hover {
  border-left-color: var(--accent);
  transform: translateY(-3px);
  box-shadow: 0 14px 34px -18px rgba(0, 0, 0, 0.85);
}
.disc-card:hover .disc-cover {
  filter: saturate(1.15) brightness(1.08);
}
.disc-card:hover .disc-cover-plate {
  transform: translateY(-1px) scale(1.06);
}
.disc-date-pill {
  position: absolute;
  right: 12px;
  bottom: 10px;
  font-size: 9px;
  font-weight: var(--fw-700);
  letter-spacing: 0.04em;
  padding: 3px 9px;
  border-radius: 999px;
  color: var(--text-dim);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  white-space: nowrap;
  pointer-events: none;
}
.search-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 9px 12px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 999px;
  transition: border-color 0.15s ease;
}
/* sticky-обёртка: строка поиска всегда перед глазами при прокрутке (как в Google) */
.search-sticky {
  position: sticky;
  top: 0;
  z-index: 20;
  background: var(--bg);
  padding: 10px 0;
  margin: -6px 0 -2px;
}
.search-sticky .search-row {
  box-shadow: 0 6px 20px -12px rgba(0, 0, 0, 0.8);
}
.filter-more {
  align-self: center;
  font-size: 11px;
  padding: 5px 4px;
  letter-spacing: 0.04em;
}
.search-row:focus-within {
  border-color: var(--accent);
}
.search-ico {
  flex: none;
  color: var(--text-dim);
}
.search-input {
  flex: 1;
  background: transparent;
  border: none;
  outline: none;
  color: var(--text);
  font-size: 13px;
  font-family: inherit;
}
.search-input::placeholder {
  color: var(--text-dim);
}
.search-clear {
  flex: none;
  background: transparent;
  border: none;
  color: var(--text-dim);
  cursor: pointer;
  font-size: 13px;
  transition: color 0.15s ease;
}
.search-clear:hover {
  color: var(--accent);
}
.search-count {
  flex: none;
  font-size: 11px;
  font-weight: var(--fw-700);
  letter-spacing: 0.03em;
  color: var(--text-dim);
  padding-left: 10px;
  margin-left: 2px;
  border-left: 1px solid var(--border);
  white-space: nowrap;
  font-variant-numeric: tabular-nums;
}
.disc-top {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 10px;
}
.disc-icon {
  flex: none;
  width: 24px;
  height: 24px;
  border-radius: 6px;
  overflow: hidden;
  line-height: 0;
}
.disc-icon img {
  width: 24px;
  height: 24px;
  object-fit: cover;
  border-radius: 6px;
}
.disc-name {
  flex: 1;
  font-size: 14px;
  font-weight: var(--fw-700);
  color: var(--accent);
  word-break: break-word;
}
.tags-bottom {
  margin-top: 2px;
  padding-top: 10px;
  border-top: 1px solid var(--border);
}
.disc-name.untitled,
.disc-desc.untitled {
  color: var(--text-dim);
  font-style: italic;
  font-weight: var(--fw-500);
}
.disc-ver {
  font-size: 10px;
  font-weight: var(--fw-700);
  color: var(--secondary);
  letter-spacing: 0.04em;
  margin-left: 4px;
}
/* Цветная обложка карточки (флаш к краям, скругление сверху). Кликабельна. */
.disc-unv-dot {
  position: absolute;
  top: 8px;
  right: 8px;
  width: 9px;
  height: 9px;
  border-radius: 50%;
  background: #ef4444;
  box-shadow: 0 0 7px rgba(239, 68, 68, 0.9);
  z-index: 2;
  cursor: help;
}
.disc-cover {
  height: 56px;
  margin: -16px -18px -10px;
  padding: 0 16px;
  border-radius: inherit;
  border-bottom-left-radius: 0;
  border-bottom-right-radius: 0;
  display: flex;
  align-items: center;
  justify-content: flex-start;
  gap: 12px;
  position: relative;
  cursor: pointer;
  overflow: hidden;
  transition: filter 0.2s ease;
}
/* DAPP Preview 440×180 под шапкой — full-bleed блок; градиент — fallback */
.disc-preview {
  position: relative;
  aspect-ratio: 440 / 180;
  margin: 0 -18px 0;
  overflow: hidden;
  cursor: pointer;
  border-top: 1px solid var(--border);
  border-bottom: 1px solid var(--border);
}
/* Картинка превью поверх градиента-fallback */
.disc-cover-preview {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  object-fit: cover;
  z-index: 0;
  pointer-events: none;
}
/* Статусы приложения — стеклянная пилюля поверх превью, справа сверху */
.disc-top-overlay {
  position: absolute;
  top: 8px;
  right: 10px;
  z-index: 2;
  padding: 5px 9px;
  border-radius: 999px;
  background: rgba(5, 12, 17, 0.55);
  border: 1px solid rgba(255, 255, 255, 0.1);
  backdrop-filter: blur(6px);
}
/* Описание — выезжающая полоска снизу превью (фон как у шапки, белый текст) */
.disc-desc-strip {
  position: absolute;
  left: 0;
  right: 0;
  bottom: 0;
  z-index: 1;
  padding: 8px 16px;
  background: rgba(8, 14, 18, 0.92);
  backdrop-filter: blur(4px);
  color: #fff;
  font-size: 12px;
  line-height: 1.4;
  max-height: 62%;
  overflow: hidden;
  transform: translateY(101%);
  transition: transform 0.25s ease;
  pointer-events: none;
}
.disc-preview:hover .disc-desc-strip {
  transform: translateY(0);
}
/* Без превью полоска описания видна постоянно (тот же стиль, что при hover) */
.disc-desc-strip.static {
  transform: translateY(0);
}
/* Fallback-панель без превью: тип и теги на градиенте */
.disc-nopreview {
  position: absolute;
  inset: 0;
  z-index: 1;
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 7px;
  padding: 10px 12px;
  pointer-events: none;
}
.np-type {
  font-size: 10px;
  font-weight: var(--fw-800, 800);
  letter-spacing: 0.14em;
  padding: 3px 10px;
  border-radius: 999px;
  border: 1px solid;
  backdrop-filter: blur(4px);
  text-transform: uppercase;
}
.np-type.free {
  color: #4ade80;
  border-color: rgba(74, 222, 128, 0.45);
  background: rgba(74, 222, 128, 0.1);
}
.np-type.paid {
  color: #fbbf24;
  border-color: rgba(251, 191, 36, 0.45);
  background: rgba(251, 191, 36, 0.1);
}
.np-type.private {
  color: #f0abfc;
  border-color: rgba(240, 171, 252, 0.45);
  background: rgba(240, 171, 252, 0.12);
}
.np-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 5px;
  max-height: 44px;
  overflow: hidden;
}
.np-tag {
  font-size: 10.5px;
  color: rgba(255, 255, 255, 0.82);
  background: rgba(5, 12, 17, 0.5);
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 999px;
  padding: 2px 8px;
  backdrop-filter: blur(4px);
}
/* «via seeder» — та же пилюля, но в правом нижнем углу превью */
.disc-seeder-overlay {
  top: auto;
  bottom: 8px;
}
/* Полоса действий: [INSTALL] размер · ⓘ · копия (без кнопки OPEN) */
.disc-actions-clean {
  display: flex;
  align-items: center;
  gap: 10px;
}
.disc-actions-clean .btn-install.disc-primary {
  flex: 1;
}
.disc-actions-clean .disc-details-icon {
  margin-left: auto;
}
/* Крупный заголовок приложения в шапке карточки */
.disc-cover-title {
  position: relative;
  z-index: 1;
  min-width: 0;
  font-size: 17px;
  font-weight: var(--fw-800, 800);
  letter-spacing: 0.02em;
  color: #fff;
  text-shadow: 0 1px 6px rgba(0, 0, 0, 0.55);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.disc-cover-title.untitled {
  font-style: italic;
  font-weight: var(--fw-500);
  opacity: 0.8;
}
.disc-cover-title .disc-ver {
  font-size: 11px;
  color: rgba(255, 255, 255, 0.75);
  font-weight: var(--fw-700);
  margin-left: 5px;
}
.disc-card:hover .disc-cover-title {
  text-decoration: none;
}
/* мягкое затемнение к низу — иконка и переход к телу карточки читаются лучше */
.disc-cover::after {
  content: '';
  position: absolute;
  inset: 0;
  background: linear-gradient(180deg, rgba(255, 255, 255, 0.08), rgba(0, 0, 0, 0.28));
  pointer-events: none;
}
/* Тёмная стеклянная подложка — прозрачные логотипы всегда видны на градиенте */
.disc-cover-plate {
  position: relative;
  z-index: 1;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 40px;
  height: 40px;
  border-radius: 11px;
  background: rgba(8, 12, 16, 0.55);
  border: 1px solid rgba(255, 255, 255, 0.22);
  backdrop-filter: blur(6px);
  -webkit-backdrop-filter: blur(6px);
  box-shadow: 0 4px 14px rgba(0, 0, 0, 0.45), inset 0 1px 0 rgba(255, 255, 255, 0.12);
  overflow: hidden;
  transition: transform 0.2s ease;
}
.disc-cover-logo {
  width: 30px;
  height: 30px;
  border-radius: 7px;
  object-fit: cover;
}
.disc-seeds {
  font-size: 10px;
  letter-spacing: 0.06em;
  white-space: nowrap;
}
/* USER-режим: маленькая зелёная точка Онлайн/Офлайн вместо счётчика сидов. */
.disc-online-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--success, var(--accent));
  box-shadow: 0 0 6px var(--success, var(--accent));
  flex-shrink: 0;
}
.disc-online-dot.off {
  background: var(--text-dim);
  box-shadow: none;
  opacity: 0.45;
}
.disc-desc {
  font-size: 12px;
  line-height: 1.5;
  color: var(--text);
}
.disc-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.disc-badge {
  font-size: 9px;
  font-weight: var(--fw-800);
  letter-spacing: 0.06em;
  padding: 3px 8px;
  border-radius: 999px;
  white-space: nowrap;
}
.disc-badge.paid {
  color: #34d399;
  background: rgba(52, 211, 153, 0.1);
  border: 1px solid rgba(52, 211, 153, 0.3);
}
.disc-badge.boost {
  color: #fbbf24;
  background: rgba(251, 191, 36, 0.1);
  border: 1px solid rgba(251, 191, 36, 0.3);
  box-shadow: 0 0 12px rgba(251, 191, 36, 0.15);
}
.disc-badge.weight {
  color: var(--text-dim);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
}
.disc-badge.cost {
  color: #60a5fa;
  background: rgba(96, 165, 250, 0.1);
  border: 1px solid rgba(96, 165, 250, 0.3);
}
.disc-badge.seeder {
  color: #26d0ce;
  background: rgba(38, 208, 206, 0.1);
  border: 1px solid rgba(38, 208, 206, 0.3);
}
.disc-url {
  font-size: 11px;
  padding: 6px 8px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  word-break: break-all;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  width: 100%;
  cursor: pointer;
  color: var(--text);
  font-family: var(--font-mono, monospace);
  text-align: left;
  transition: border-color 0.15s ease, color 0.15s ease;
}
.disc-url:hover {
  border-color: var(--accent);
  color: var(--accent);
}
.disc-url .url-text {
  word-break: break-all;
}
.disc-url .copy-ico {
  flex: 0 0 auto;
  opacity: 0.6;
  transition: opacity 0.15s ease;
}
.disc-url:hover .copy-ico {
  opacity: 1;
}
.copy-toast {
  position: fixed;
  left: 50%;
  bottom: 28px;
  transform: translateX(-50%);
  background: var(--panel);
  border: 1px solid var(--success, #4a9e7f);
  color: var(--success, #4a9e7f);
  font-size: 13px;
  font-weight: var(--fw-700);
  letter-spacing: 0.04em;
  padding: 10px 18px;
  border-radius: 999px;
  box-shadow: var(--shadow);
  z-index: 200;
}
/* «Installed» toast — источник (P2P / cloud seeder) слева от имени dApp.
   Через сидер подсвечивается фиолетовым (SEEDER-цвет), P2P — зелёным. */
.install-toast {
  position: fixed;
  left: 50%;
  bottom: 78px;
  transform: translateX(-50%);
  display: inline-flex;
  align-items: center;
  gap: 10px;
  background: var(--panel);
  border: 1px solid var(--success, #4a9e7f);
  color: var(--text);
  font-size: 13px;
  letter-spacing: 0.03em;
  padding: 10px 16px 10px 12px;
  border-radius: 999px;
  box-shadow: var(--shadow);
  z-index: 200;
}
.install-toast.via-seeder {
  border-color: #a855f7;
}
.install-toast .it-src {
  font-weight: var(--fw-800);
  letter-spacing: 0.05em;
  color: var(--success, #4a9e7f);
  padding: 2px 10px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--success, #4a9e7f) 14%, transparent);
  border: 1px solid color-mix(in srgb, var(--success, #4a9e7f) 55%, transparent);
  font-size: 11px;
}
.install-toast.via-seeder .it-src {
  color: #a855f7;
  background: color-mix(in srgb, #a855f7 14%, transparent);
  border-color: color-mix(in srgb, #a855f7 55%, transparent);
}
.install-toast .it-msg {
  white-space: nowrap;
  max-width: 60vw;
  overflow: hidden;
  text-overflow: ellipsis;
}
.toast-enter-active,
.toast-leave-active {
  transition: opacity 0.2s ease, transform 0.2s ease;
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateX(-50%) translateY(8px);
}
.disc-actions {
  display: flex;
  align-items: center;
  gap: 10px;
  padding-top: 6px;
  border-top: 1px solid var(--border);
}
.disc-actions .btn {
  padding: 8px 12px;
}
/* install CTA — blue, distinct from the teal "open" button (same matte tone) */
.btn-install {
  background: linear-gradient(180deg, #2f6fb0, #225488);
  border: 1px solid #3f82c8;
  color: #eaf4ff;
}
.btn-install:hover:not(:disabled) {
  background: linear-gradient(180deg, #387bc0, #285f97);
  border-color: #5aa0e0;
  box-shadow: 0 0 14px -4px rgba(79, 130, 200, 0.7);
}
.btn-install:disabled {
  opacity: 0.55;
  cursor: default;
}
.disc-verified {
  font-size: 11px;
  color: var(--success);
  font-weight: var(--fw-700);
}
.install-error {
  font-size: 12px;
  color: var(--danger);
  padding: 10px 12px;
  border: 1px solid var(--danger);
  border-radius: var(--radius);
  word-break: break-all;
}
.downloading {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 12px;
  color: var(--accent);
  padding: 10px 12px;
  border: 1px solid var(--accent);
  border-radius: var(--radius);
}
/* Скан облачных дисков сидеров — матово-синий, как «Cloud mode» в статус-баре */
.downloading.cloud-scan {
  color: #6ea8d8;
  border-color: rgba(110, 168, 216, 0.55);
}
.cloud-note {
  font-size: 11px;
  color: #6ea8d8;
  padding: 8px 12px;
  border: 1px dashed rgba(110, 168, 216, 0.4);
  border-radius: var(--radius);
}
.cloud-empty-hint {
  font-size: 11px;
  color: #6ea8d8;
}
.spinner {
  display: inline-block;
  animation: spin 1s linear infinite;
  font-size: 14px;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
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
.pager {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 16px;
  padding: 8px 0;
}
.pager-btn {
  font-size: 12px;
  padding: 8px 14px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  color: var(--text);
  cursor: pointer;
  transition: border-color 0.15s ease, color 0.15s ease;
}
.pager-btn:hover:not(:disabled) {
  border-color: var(--accent);
  color: var(--accent);
}
.pager-info {
  font-size: 12px;
  letter-spacing: 0.04em;
  min-width: 90px;
  text-align: center;
}
button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

/* ────────────────────────────────────────────────────────────────
   Clean discover card (v1.59) — Apple/Google-style progressive
   disclosure: card face shows only essentials; full technical
   detail lives inside the "Подробно" modal. */
.disc-card.installed { border-color: rgba(34,197,94,0.28); }

/* Green «installed» dot replacing the loud «✓ УСТАНОВЛЕНО» label. */
.disc-installed-dot {
  width: 8px; height: 8px; border-radius: 50%;
  background: #22c55e;
  box-shadow: 0 0 8px rgba(34,197,94,0.7);
  flex-shrink: 0;
}

/* Three-slot action row: [primary (install/open)] [size] [details] [copy-square]
   Ширина install-кнопки = ширине лейбла (grid-column auto), чтобы бейдж «⛁ MB»,
   ⓘ и «скопировать» всегда помещались в одну строку. */
.disc-actions-clean {
  display: grid;
  grid-template-columns: minmax(0, auto) minmax(0, 1fr) 36px 36px;
  gap: 8px;
  align-items: stretch;
}
.disc-primary { min-width: 0; }
.disc-actions-clean .btn-install.disc-primary {
  padding: 8px 14px;
  white-space: nowrap;
  font-size: 11.5px;
  letter-spacing: 0.07em;
  justify-self: start;
}
.disc-actions-clean .disc-badge.weight {
  justify-self: end;
  align-self: center;
}
/* «Открыть» — стеклянная кнопка с неоновой кромкой вместо сплошной зелёной плашки */
.disc-open-btn {
  position: relative;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 9px 14px;
  border-radius: 10px;
  background: linear-gradient(
    135deg,
    color-mix(in srgb, var(--accent) 16%, transparent),
    color-mix(in srgb, var(--accent) 4%, transparent)
  );
  border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent);
  color: var(--accent);
  font-size: 12px;
  font-weight: var(--fw-800, 800);
  letter-spacing: 0.09em;
  text-transform: uppercase;
  cursor: pointer;
  overflow: hidden;
  transition: border-color 0.18s ease, box-shadow 0.18s ease, background-color 0.18s ease;
}
/* бегущий блик при наведении */
.disc-open-btn::before {
  content: '';
  position: absolute;
  top: 0;
  bottom: 0;
  left: -60%;
  width: 40%;
  background: linear-gradient(105deg, transparent, rgba(255, 255, 255, 0.16), transparent);
  transform: skewX(-18deg);
  transition: left 0.45s ease;
  pointer-events: none;
}
.disc-open-btn:hover {
  border-color: var(--accent);
  box-shadow: 0 0 20px -5px color-mix(in srgb, var(--accent) 70%, transparent),
    inset 0 1px 0 rgba(255, 255, 255, 0.08);
}
.disc-open-btn:hover::before {
  left: 120%;
}
.disc-open-btn:active {
  transform: translateY(1px);
}
.disc-open-arrow {
  display: inline-block;
  transition: transform 0.18s ease;
}
.disc-open-btn:hover .disc-open-arrow {
  transform: translateX(4px);
}
.disc-details {
  background: transparent;
  border: 1px solid var(--border);
  color: var(--text-dim);
  padding: 0 14px;
  border-radius: 10px;
  font-size: 12px;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease;
}
.disc-details:hover { color: var(--text); border-color: var(--accent); background: rgba(255,255,255,0.04); }
.disc-copy-square {
  width: 36px;
  height: 100%;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 10px;
  color: var(--text-dim);
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease, transform 0.15s ease;
}
.disc-copy-square:hover { color: var(--accent); border-color: var(--accent); background: rgba(176,133,255,0.08); transform: translateY(-1px); }

/* ── Details modal ── */
.disc-modal-back {
  position: fixed; inset: 0; z-index: 2000;
  background: rgba(0, 0, 0, 0.72);
  backdrop-filter: blur(10px);
  -webkit-backdrop-filter: blur(10px);
  display: flex; align-items: center; justify-content: center;
  padding: 24px;
}
.disc-modal {
  position: relative;
  width: 100%;
  max-width: 520px;
  border-radius: 16px;
  padding: 28px 28px 24px;
  /* Solid opaque surface — раньше использовался несуществующий var(--surface),
     из-за чего модалка просвечивала фоновую страницу. */
  background: var(--panel, #14171c);
  border: 1px solid var(--border);
  box-shadow: 0 24px 64px rgba(0, 0, 0, 0.7);
}
.disc-modal-x {
  position: absolute; top: 12px; right: 14px;
  width: 30px; height: 30px;
  border: 0; background: transparent; color: var(--text-dim);
  font-size: 18px; cursor: pointer; border-radius: 8px;
  transition: color 0.15s ease, background-color 0.15s ease;
}
.disc-modal-x:hover { color: var(--text); background: rgba(255,255,255,0.06); }
.disc-modal-head { display: flex; align-items: center; gap: 14px; margin-bottom: 16px; }
.disc-modal-icon { width: 44px; height: 44px; display: inline-flex; align-items: center; justify-content: center; border-radius: 10px; background: rgba(255,255,255,0.04); overflow: hidden; }
.disc-modal-icon img { width: 100%; height: 100%; object-fit: cover; }
.disc-modal-titlewrap { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.disc-modal-title { font-size: 18px; font-weight: var(--fw-700); margin: 0; color: var(--text); overflow: hidden; text-overflow: ellipsis; }
.disc-modal-ver { font-size: 11px; color: var(--text-dim); font-family: var(--font-mono); letter-spacing: 0.06em; }
.disc-modal-desc { font-size: 13px; color: var(--text-dim); line-height: 1.6; margin: 0 0 16px; }
.disc-modal-rows { display: flex; flex-direction: column; gap: 8px; padding: 12px 0; border-top: 1px dashed var(--border); border-bottom: 1px dashed var(--border); margin-bottom: 12px; }
.disc-modal-row { display: flex; justify-content: space-between; align-items: center; gap: 12px; font-size: 12px; }
.disc-modal-row .dim { color: var(--text-dim); }
.disc-modal-row b { color: var(--text); font-family: var(--font-mono); font-weight: var(--fw-700); }
.disc-modal-row b.hl { color: var(--accent); }
.disc-modal-url { border-top: 1px dashed var(--border); padding-top: 8px; margin-top: 4px; }
.disc-modal-copyurl {
  display: inline-flex; align-items: center; gap: 8px;
  background: transparent; border: 1px dashed var(--border);
  padding: 5px 10px; border-radius: 8px;
  color: var(--text-dim); font-family: var(--font-mono); font-size: 11px;
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease;
}
.disc-modal-copyurl:hover { color: var(--accent); border-color: var(--accent); }
.disc-modal-tags { display: flex; flex-wrap: wrap; gap: 6px; }

/* Fade transition для модалки. */
.fade-enter-active, .fade-leave-active { transition: opacity 0.15s ease; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
/* --- Official Repository --- */
.official-row {
  display: flex; align-items: center; justify-content: space-between; gap: 12px;
  flex-wrap: wrap; padding: 8px 12px; border-radius: 10px;
  border: 1px solid var(--border, rgba(255,255,255,.08));
  background: rgba(74, 222, 128, .05); font-size: 12px;
}
.official-note { color: var(--text-dim, #9fb0c0); }
.official-yes { color: #4ade80; }
.official-no { color: #fbbf24; }
.priv-repo { color: #e8c25a; text-shadow: 0 0 10px rgba(232, 194, 90, 0.35); }
.disc-modal-dev {
  color: var(--accent, #64b5ef); text-decoration: none; font-weight: 700;
  font-family: inherit; font-size: 12px;
}
.disc-modal-dev:hover { text-decoration: underline; }
.unv-toggle {
  display: inline-flex; align-items: center; gap: 6px; cursor: pointer;
  color: var(--text-dim, #9fb0c0); user-select: none;
}
.unv-toggle input { accent-color: #fbbf24; cursor: pointer; }
/* Кнопка разблокировки непроверенных dApps (ончейн-согласие ещё не оплачено) */
.unv-unlock {
  display: inline-flex; align-items: center; gap: 7px;
  padding: 6px 14px; border-radius: 999px;
  background: rgba(251, 191, 36, 0.08);
  border: 1px solid rgba(251, 191, 36, 0.45);
  color: #fbbf24; font-size: 11px; font-weight: var(--fw-700);
  letter-spacing: 0.05em; cursor: pointer;
  transition: border-color 0.15s ease, background-color 0.15s ease, box-shadow 0.15s ease;
}
.unv-unlock:hover {
  border-color: #fbbf24;
  background: rgba(251, 191, 36, 0.14);
  box-shadow: 0 0 16px -6px rgba(251, 191, 36, 0.7);
}

/* ── Модалка «зона повышенного риска» (NETFORY_UNVERIFIED_ACCEPT) ── */
.risk-modal {
  position: relative;
  width: 100%;
  max-width: 560px;
  max-height: 90vh;
  overflow-y: auto;
  border-radius: 16px;
  padding: 26px 28px 20px;
  background: var(--panel, #14171c);
  border: 1px solid rgba(239, 68, 68, 0.5);
  box-shadow: 0 24px 64px rgba(0, 0, 0, 0.75), 0 0 40px -18px rgba(239, 68, 68, 0.55);
}
.risk-head { display: flex; align-items: center; gap: 12px; margin-bottom: 14px; }
.risk-sign {
  flex: none; display: inline-flex; align-items: center; justify-content: center;
  width: 42px; height: 42px; border-radius: 12px; font-size: 22px; color: #ef4444;
  background: rgba(239, 68, 68, 0.12); border: 1px solid rgba(239, 68, 68, 0.45);
}
.risk-title {
  margin: 0; font-size: 15px; letter-spacing: 0.08em;
  color: #ef4444; font-weight: var(--fw-800, 800);
}
.risk-intro { font-size: 13px; line-height: 1.6; color: var(--text); margin: 0 0 12px; }
.risk-sub { font-size: 12px; font-weight: var(--fw-700); color: #fbbf24; margin: 0 0 6px; }
.risk-list {
  margin: 0 0 14px; padding-left: 18px;
  display: flex; flex-direction: column; gap: 4px;
  font-size: 12px; line-height: 1.55; color: var(--text-dim);
}
.risk-list li::marker { color: #ef4444; }
.risk-disclaimer {
  padding: 10px 12px; border-radius: 10px; margin-bottom: 12px;
  background: rgba(239, 68, 68, 0.07); border: 1px dashed rgba(239, 68, 68, 0.45);
  font-size: 12px; line-height: 1.55;
}
.risk-disclaimer b { color: #ef4444; letter-spacing: 0.05em; }
.risk-disclaimer p { margin: 4px 0 0; color: var(--text); }
.risk-onchain {
  font-size: 12px; line-height: 1.55; color: var(--text-dim); margin: 0 0 14px;
}
.risk-error {
  font-size: 12px; color: var(--danger, #ef4444);
  padding: 8px 10px; margin: 0 0 12px;
  border: 1px solid var(--danger, #ef4444); border-radius: var(--radius);
  word-break: break-word;
}
.risk-actions { display: flex; flex-direction: column; gap: 8px; }
.risk-pay {
  display: inline-flex; align-items: center; justify-content: center; gap: 10px;
  padding: 13px 16px; border-radius: 12px;
  background: linear-gradient(180deg, #b91c1c, #7f1d1d);
  border: 1px solid #ef4444; color: #fff;
  font-size: 13px; font-weight: var(--fw-800, 800); letter-spacing: 0.07em;
  text-transform: uppercase; cursor: pointer;
  transition: box-shadow 0.18s ease, border-color 0.18s ease, transform 0.12s ease;
}
.risk-pay:hover:not(:disabled) {
  border-color: #f87171;
  box-shadow: 0 0 24px -6px rgba(239, 68, 68, 0.85);
}
.risk-pay:active:not(:disabled) { transform: translateY(1px); }
.risk-pay:disabled { opacity: 0.6; cursor: default; }
.risk-cancel {
  padding: 10px 16px; border-radius: 12px;
  background: transparent; border: 1px solid var(--border);
  color: var(--text-dim); font-size: 12px; font-weight: var(--fw-700);
  letter-spacing: 0.06em; cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease;
}
.risk-cancel:hover:not(:disabled) { color: var(--text); border-color: var(--accent); }
.risk-feenote {
  margin: 12px 0 0; font-size: 10px; letter-spacing: 0.04em;
  color: var(--text-dim); text-align: center;
  font-family: var(--font-mono, monospace);
}
.risk-toast { border-color: #fbbf24; color: #fbbf24; }
.disc-official {
  flex: 0 0 auto; display: inline-flex; align-items: center; justify-content: center;
  width: 16px; height: 16px; border-radius: 50%; font-size: 10px; font-weight: 800;
  color: #06121e; background: #4ade80; cursor: help;
}
/* Приватный dApp (E2EE): виден только адресам из списка доступа */
.disc-private {
  flex: 0 0 auto; display: inline-flex; align-items: center; justify-content: center;
  font-size: 11px; cursor: help; filter: drop-shadow(0 0 4px rgba(217, 70, 239, 0.6));
}
</style>
