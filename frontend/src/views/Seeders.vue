<script setup lang="ts">
/*
 * SmartNet // views/Seeders.vue
 *
 * Cloud Seeders (ntfryseed) — публичный анти-цензурный слой поверх облаков.
 * Пользователь публикует подписанный + зашифрованный стейт сети
 * (список dApps, ноды, известные сидеры) в открытую папку /ntfryseed
 * на СВОЁМ облаке (Яндекс Диск или Облако Mail.ru). Любой пир по URI
 * sn://seed/y@<folder>@<pubkey> или sn://seed/m@<weblink>@<pubkey>
 * читает эти данные без OAuth — публикатор известен по своему pubkey,
 * подпись автора проверяется секп256k1.
 *
 * Условие публикации: пользователь ДОЛЖЕН иметь подключённый Yandex- или
 * Mail.ru-аккаунт в Crypto Vault. Без него доступно только
 * ЧТЕНИЕ чужих сидеров и работа с реестром URI.
 */
import { computed, onMounted, onUnmounted, ref, watch, nextTick } from 'vue'
import { useRoute } from 'vue-router'
import { useI18n } from 'vue-i18n'
import QRCode from 'qrcode'
import {
  isTauri,
  openExternal,
  cloudSeederPublish,
  cloudSeederFetch,
  cloudSeederInstall,
  cloudSeederStatusGet,
  cloudSeederRegistryGet,
  cloudSeederRegistrySet,
  cloudSeederRegistryOriginsGet,
  onSeederPublishProgress,
  onSeederRegistryUpdated,
  verifyApp,
  cloudSeederPublishProfiles,
  cloudSeederFetchProfilesManifest,
  cloudSeederFetchProfile,
  cloudSeederFetchProfileFile,
  profileSeederAutoRepGet,
  profileSeederAutoRepSet,
  profileSeederLastReplicated,
  onProfilePublishProgress,
  type SeederPublishResult,
  type SeederProviderId,
  type SeederFetchResult,
  type SeederStatus,
  type SeederPublishProgress,
  type SeederOrigin,
  type ProfilesPublishOut,
  type ProfilesManifest,
  type ProfilePublishProgress,
  type CloudUserBundle,
} from '@/lib/bridge'
import { useAppsStore } from '@/store/apps'
import { vdappsStatus, type VdappsStatus } from '@/lib/bridge'

// vDapps: статус анонса official-списков на облачный диск
const vd = ref<VdappsStatus | null>(null)
onMounted(async () => {
  try {
    vd.value = await vdappsStatus()
  } catch { /* web-превью */ }
})
const vdTime = computed(() =>
  vd.value?.ts ? new Date(vd.value.ts * 1000).toLocaleString() : '—',
)
import {
  autoRepEnabled,
  setAutoRep,
  markOwnSeeder,
  lastReplication,
  ownSeeder,
  loadSeederAccounts,
  freshSeederToken,
  secondaryAccId,
  setSecondaryAccId,
  publishSecondary,
  publishProfilesSecondary,
  type SeederDriveAccount,
} from '@/lib/seederRep'

const busy = ref(false)
const errorMsg = ref('')
const toast = ref('')
let toastTimer: ReturnType<typeof setTimeout> | null = null

const { t } = useI18n()

// ── Аккаунты для публикации из Crypto Vault: Yandex + Mail.ru ───────
// Выбор primary-аккаунта переживает навигацию (localStorage), иначе при
// каждом заходе на страницу выставлялся первый аккаунт из списка (v1.135.21).
const LS_PUB_ACC = 'sth_seeder_pub_acc'
const seederAccounts = ref<SeederDriveAccount[]>([])
const activeAccId = ref(localStorage.getItem(LS_PUB_ACC) || '')
watch(activeAccId, (id) => {
  if (id) localStorage.setItem(LS_PUB_ACC, id)
  // primary и secondary не могут совпадать — зеркало на тот же диск бессмысленно
  if (id && secondaryId.value === id) secondaryId.value = ''
})
const activeAcc = computed(
  () => seederAccounts.value.find((a) => a.id === activeAccId.value) || null,
)
const canPublish = computed(() => isTauri && !!activeAcc.value)

function provName(a: SeederDriveAccount): string {
  if (a.provider === 'mailru') return 'Mail.ru'
  if (a.provider === 'gdrive') return 'Google'
  return 'Yandex'
}

// ── Secondary drive: зеркалирование /ntfryseed на второй диск ────────
const secondaryId = ref(secondaryAccId())
watch(secondaryId, (id) => setSecondaryAccId(id))
const secondaryOptions = computed(() =>
  seederAccounts.value.filter((a) => a.id !== activeAccId.value),
)
const secondaryError = ref('')
const secondaryResult = ref<SeederPublishResult | null>(null)

function onSecondaryError(e: Event) {
  secondaryError.value = t('pubSeeders.err.secondaryPrefix') + String((e as CustomEvent).detail || t('pubSeeders.err.generic'))
}
function onSecondaryReplicated() {
  secondaryError.value = ''
  void loadRegistry()
}

// ── Mail.ru: ручная публикация папки (фолбэк для VK ID-логинов) ─────
// Пользователь публикует /ntfryseed в веб-интерфейсе Облака и вставляет
// публичную ссылку — weblink извлекается и передаётся в publish, o2-токен
// не нужен. Запоминается на диске для авто-репликации.
const LS_MR_LINK = 'sth_seeder_mr_link'
const mrPublicLink = ref(localStorage.getItem(LS_MR_LINK) || '')
// Поле показывается ТОЛЬКО если автопубликация папки не удалась (o2/publish)
// или ссылка уже была введена ранее — обычный путь полностью автоматический.
const mrLinkNeeded = ref(false)
const showMrLink = computed(
  () => activeAcc.value?.provider === 'mailru' && (mrLinkNeeded.value || !!mrPublicLink.value),
)
function mrWeblink(): string | undefined {
  const raw = mrPublicLink.value.trim()
  if (!raw) return undefined
  const m = raw.match(/(?:cloud\.mail\.ru\/public\/)?([A-Za-z0-9_-]+\/[A-Za-z0-9_-]+)\/?(?:[?#].*)?$/)
  return m ? m[1] : undefined
}

/** Открыть папку ntfryseed в браузере — пользователю остаётся нажать
 *  «Настроить доступ» → «Скопировать» и вставить ссылку в поле. */
async function openMrFolder() {
  try {
    await openExternal('https://cloud.mail.ru/home/ntfryseed/')
  } catch {
    showToast(t('pubSeeders.toast.openMrFolder'))
  }
}

/** Вставили ссылку → сразу публикуем заново (без лишнего клика). */
async function useMrLink() {
  if (!mrWeblink()) {
    showToast(t('pubSeeders.toast.badLink'))
    return
  }
  errorMsg.value = ''
  await doPublish()
}

// ── Публикация ──────────────────────────────────────────────────────
const publishResult = ref<SeederPublishResult | null>(null)

// ── Live-прогресс публикации (метаданные + архивы dApp) ─────────────
// Событие seeder-publish-progress приходит и при ручной публикации, и при
// фоновой авто-репликации — блок показывается только пока busy.
const pubProg = ref<SeederPublishProgress | null>(null)
let unProg: (() => void) | null = null
let unReg: (() => void) | null = null

// ── Сводка по /ntfryseed за текущую сессию ──────────────────────────
// Копим по последнему успешному upload каждого appId (dedupe по appId):
// сколько архивов уже лежит в облаке, суммарный размер после zstd-сжатия
// и сколько байт сэкономлено благодаря компрессии.
type ArchiveStat = { size: number; raw: number; kbps: number }
const sessionArchives = ref<Map<string, ArchiveStat>>(new Map())

const cloudTotals = computed(() => {
  let size = 0
  let raw = 0
  let upBytes = 0
  let upSecs = 0
  const ratios: number[] = []
  for (const s of sessionArchives.value.values()) {
    size += s.size
    raw += s.raw
    if (s.raw > 0) ratios.push(1 - s.size / s.raw)
    if (s.kbps > 0) {
      upBytes += s.size
      upSecs += s.size / 1024 / s.kbps
    }
  }
  const saved = Math.max(0, raw - size)
  const avg = ratios.length ? ratios.reduce((a, b) => a + b, 0) / ratios.length : 0
  return {
    count: sessionArchives.value.size,
    size,
    saved,
    avgPct: Math.round(avg * 100),
    hasCompression: ratios.length > 0,
    // средняя скорость заливки за сессию (взвешенная по объёму), КБ/с
    avgKbps: upSecs > 0 ? upBytes / 1024 / upSecs : 0,
  }
})

function trackArchiveDone(p: SeederPublishProgress) {
  if (p.phase !== 'archive' || p.status !== 'done' || !p.appId) return
  if (p.sizeBytes <= 0) return
  sessionArchives.value.set(p.appId, {
    size: p.sizeBytes,
    raw: p.rawBytes > 0 ? p.rawBytes : p.sizeBytes,
    kbps: p.kbps,
  })
  // reassign to trigger reactivity for Map
  sessionArchives.value = new Map(sessionArchives.value)
}

const progPct = computed(() => {
  const p = pubProg.value
  if (!p) return 0
  if (p.phase === 'meta') return 6
  if (p.phase === 'done') return 100
  if (!p.total) return 50
  const done = p.status === 'done' || p.status === 'error' ? p.index : p.index - 1
  // внутри текущего архива двигаем бар по реально переданным байтам
  let frac = 0
  if (p.status === 'uploading' && p.sizeBytes > 0) frac = Math.min(1, p.uploadedBytes / p.sizeBytes)
  return Math.round(6 + ((done + frac) / p.total) * 92)
})

function fmtSize(b: number): string {
  if (b >= 1048576) return `${(b / 1048576).toFixed(1)} ${t('pubSeeders.units.mb')}`
  return `${Math.max(1, Math.round(b / 1024))} ${t('pubSeeders.units.kb')}`
}

function fmtSpeed(kbps: number): string {
  return kbps >= 1024
    ? `${(kbps / 1024).toFixed(1)} ${t('pubSeeders.units.mbps')}`
    : `${Math.round(kbps)} ${t('pubSeeders.units.kbps')}`
}

/** «12.4 МБ → 3.1 МБ (−75%)» — степень zstd-сжатия архива, либо просто размер. */
function fmtPacked(p: SeederPublishProgress): string {
  const saved = p.rawBytes > 0 ? 1 - p.sizeBytes / p.rawBytes : 0
  if (saved < 0.01) return fmtSize(p.sizeBytes)
  return `${fmtSize(p.rawBytes)} → ${fmtSize(p.sizeBytes)} (−${Math.round(saved * 100)}%)`
}

/** ETA по текущей скорости: « · осталось ~2 мин»; после последнего байта —
 *  « · сервер обрабатывает…» (финализация checksum/репликации у облака). */
function fmtEta(p: SeederPublishProgress): string {
  if (p.kbps <= 0 || p.sizeBytes <= 0 || !p.uploadedBytes) return ''
  const left = p.sizeBytes - p.uploadedBytes
  if (left <= 0) return t('pubSeeders.status.serverProcessing')
  const secs = Math.ceil(left / 1024 / p.kbps)
  if (secs < 60) return t('pubSeeders.status.etaSec', { n: secs })
  const mins = Math.ceil(secs / 60)
  if (mins < 60) return t('pubSeeders.status.etaMin', { n: mins })
  return t('pubSeeders.status.etaHour', { h: Math.floor(mins / 60), m: mins % 60 })
}

/** Live-строка заливки: «45.2 МБ / 93.3 МБ (−69%) · 2.4 МБ/с · осталось ~2 мин…». */
function fmtUpload(p: SeederPublishProgress): string {
  const savedPct = p.rawBytes > 0 ? 1 - p.sizeBytes / p.rawBytes : 0
  const packed = savedPct >= 0.01 ? ` (−${Math.round(savedPct * 100)}%)` : ''
  if (!p.uploadedBytes) return `${fmtPacked(p)}…`
  const speed = p.kbps > 0 ? ` · ${fmtSpeed(p.kbps)}` : ''
  return `${fmtSize(p.uploadedBytes)} / ${fmtSize(p.sizeBytes)}${packed}${speed}${fmtEta(p)}…`
}

function shortAppId(id: string): string {
  return id.length > 14 ? `${id.slice(0, 8)}…${id.slice(-4)}` : id
}

// ── Чтение чужого сидера ────────────────────────────────────────────
const fetchUri = ref('')
const fetchResult = ref<SeederFetchResult | null>(null)
const fetchBusy = ref(false)
const fetchError = ref('')

// ── Реестр известных сидер-URI ──────────────────────────────────────
const registry = ref<string[]>([])
// Источник добавления каждого URI (для бейджа "MANUAL / GOSSIP / …").
const origins = ref<Record<string, SeederOrigin>>({})

/** Локализованный лейбл + класс для origin-бейджа рядом с URI. */
function originBadge(uri: string): { label: string; cls: string; title: string } | null {
  const o = origins.value[uri]
  if (!o) return null
  const map: Record<SeederOrigin, { label: string; cls: string; title: string }> = {
    own: { label: t('pubSeeders.origin.own'), cls: 'own', title: t('pubSeeders.origin.ownTitle') },
    manual: { label: 'MANUAL', cls: 'manual', title: t('pubSeeders.origin.manualTitle') },
    gossip: { label: 'GOSSIP', cls: 'gossip', title: t('pubSeeders.origin.gossipTitle') },
    seed_file: { label: 'SEEDERS.ENC', cls: 'seed', title: t('pubSeeders.origin.seedFileTitle') },
    bootstrap: { label: 'BOOTSTRAP', cls: 'boot', title: t('pubSeeders.origin.bootstrapTitle') },
  }
  return map[o]
}

// ── Статус сидеров (жив/недоступен + время последнего fetch) ────────
const statuses = ref<Record<string, SeederStatus>>({})
const checkBusy = ref(false)

async function loadStatuses() {
  try {
    statuses.value = await cloudSeederStatusGet()
  } catch {
    statuses.value = {}
  }
}

async function checkAll() {
  if (checkBusy.value || !registry.value.length) return
  checkBusy.value = true
  try {
    for (const uri of registry.value) {
      try {
        await cloudSeederFetch(uri) // статус пишется на бэкенде
      } catch {
        // недоступен — статус уже записан
      }
    }
    await Promise.all([loadStatuses(), loadRegistry()])
    showToast(t('pubSeeders.toast.checkDone'))
  } finally {
    checkBusy.value = false
  }
}

function dotClass(uri: string): string {
  const s = statuses.value[uri]
  if (!s) return 'unknown'
  return s.ok ? 'ok' : 'bad'
}

/** Даты бэкенда — unix-секунды строкой (now_iso) либо ISO. Парсим гибко. */
function fmtWhen(iso?: string | null): string {
  if (!iso) return '—'
  const d = /^\d+$/.test(iso) ? new Date(Number(iso) * 1000) : new Date(iso)
  return isNaN(d.getTime()) ? '—' : d.toLocaleString()
}

function statusText(uri: string): string {
  const s = statuses.value[uri]
  if (!s) return t('pubSeeders.status.never')
  return s.ok
    ? t('pubSeeders.status.ok', { when: fmtWhen(s.lastOk) })
    : t('pubSeeders.status.bad', { when: fmtWhen(s.lastOk) })
}

/** «Держит сеть прямо сейчас» — сидер, у которого последний fetch успешен
 *  или lastOk свежее 15 минут. Именно эти URI видит Cloud Fallback как
 *  живые каналы, и именно их подсвечиваем при переходе с плашки Cloud mode. */
function isHoldingNetwork(uri: string): boolean {
  const s = statuses.value[uri]
  if (!s) return false
  if (s.ok) return true
  if (s.lastOk) {
    const t = /^\d+$/.test(s.lastOk) ? Number(s.lastOk) * 1000 : Date.parse(s.lastOk)
    if (Number.isFinite(t) && Date.now() - t < 15 * 60_000) return true
  }
  return false
}

// Deep-link из плашки Cloud mode: приходим с #active-seeders → скроллим к
// реестру и включаем импульс на «живых» сидерах на пару секунд.
const route = useRoute()
const highlightActive = ref(false)

/** Сколько сидеров сейчас держат сеть (для бейджа в шапке реестра). */
const holdingCount = computed(() => registry.value.filter(isHoldingNetwork).length)

/** Порядок в списке: живые сидеры — сверху (стабильно между рендерами),
 *  чтобы при клике из Cloud mode пользователь сразу видел, кто держит сеть. */
const orderedRegistry = computed(() => {
  const live: string[] = []
  const dead: string[] = []
  for (const uri of registry.value) (isHoldingNetwork(uri) ? live : dead).push(uri)
  return [...live, ...dead]
})

// ── Установка dApp из облака (без P2P) ──────────────────────────────
const installingId = ref('')
const fetchDapps = computed<any[]>(() => fetchResult.value?.dapps?.items || [])

async function installFromCloud(appId: string) {
  const uri = fetchUri.value.trim()
  if (!uri || installingId.value) return
  installingId.value = appId
  fetchError.value = ''
  try {
    const m = await cloudSeederInstall(uri, appId)
    const v = await verifyApp(appId)
    if (!v.verified) {
      fetchError.value = `verify-failed: ${v.reason || t('pubSeeders.err.verifyFallback')}`
      return
    }
    useAppsStore().replaceLocal(m)
    showToast(t('pubSeeders.toast.installedFromCloud', { name: m.name || appId }))
  } catch (e) {
    fetchError.value = errText(e)
  } finally {
    installingId.value = ''
  }
}

// ── Авто-репликация (Phase 3b) ──────────────────────────────────────
const autoRep = ref(autoRepEnabled())
const lastRep = ref(lastReplication())
const hasOwnSeeder = ref(!!ownSeeder())
const lastRepText = computed(() =>
  lastRep.value ? new Date(lastRep.value).toLocaleString() : '—',
)

function toggleAutoRep() {
  autoRep.value = !autoRep.value
  setAutoRep(autoRep.value)
}

function onReplicated() {
  lastRep.value = lastReplication()
  hasOwnSeeder.value = true
  void loadRegistry()
}

// ── Public Profile Seeders — репликация профилей (opt-in, раз в час) ─
const profileAutoRep = ref(false)
const profileLastRep = ref(0)
const profileBusy = ref(false)
const profileError = ref('')
const profileResult = ref<ProfilesPublishOut | null>(null)
const profileProg = ref<ProfilePublishProgress | null>(null)
let unProfileProg: (() => void) | null = null
const profileLastRepText = computed(() =>
  profileLastRep.value ? new Date(profileLastRep.value).toLocaleString() : '—',
)

async function loadProfileState() {
  try {
    profileAutoRep.value = await profileSeederAutoRepGet()
    profileLastRep.value = await profileSeederLastReplicated()
  } catch {
    /* ok, кошелёк закрыт */
  }
}

async function toggleProfileAutoRep() {
  const next = !profileAutoRep.value
  profileAutoRep.value = next
  try {
    await profileSeederAutoRepSet(next)
  } catch (e) {
    profileAutoRep.value = !next // откатываем
    profileError.value = errText(e)
  }
}

async function publishProfilesNow() {
  if (!activeAcc.value || profileBusy.value) return
  profileBusy.value = true
  profileError.value = ''
  profileResult.value = null
  profileProg.value = null
  try {
    const acc = activeAcc.value
    const token = await freshSeederToken(acc)
    const weblink = acc.provider === 'mailru' ? mrWeblink() : undefined
    const res = await cloudSeederPublishProfiles(
      token,
      acc.provider as SeederProviderId,
      weblink,
    )
    profileResult.value = res
    profileLastRep.value = res.updated
    if (secondaryId.value) {
      try {
        await publishProfilesSecondary(seederAccounts.value, acc.id)
      } catch (e2) {
        secondaryError.value = t('pubSeeders.err.secondaryProfiles') + errText(e2)
      }
    }
  } catch (e) {
    profileError.value = errText(e)
  } finally {
    profileBusy.value = false
  }
}

// Читалка чужих профилей: manifest + просмотр bundle
const profilesManifest = ref<ProfilesManifest | null>(null)
const profilesFetchBusy = ref(false)
const profilesFetchError = ref('')
const openedProfile = ref<CloudUserBundle | null>(null)
const openedProfileBusy = ref(false)

// Загруженные из облака файлы: cid → { url, mime, busy?, error? }
type CloudFileState = {
  url: string
  mime: string
  size: number
  name: string
}
const cloudFiles = ref<Record<string, CloudFileState>>({})
const cloudFilesBusy = ref<Record<string, boolean>>({})
const cloudFilesError = ref<Record<string, string>>({})

function fileKind(mime: string): 'image' | 'audio' | 'video' | 'other' {
  const m = (mime || '').toLowerCase()
  if (m.startsWith('image/')) return 'image'
  if (m.startsWith('audio/')) return 'audio'
  if (m.startsWith('video/')) return 'video'
  return 'other'
}

function b64ToBlob(b64: string, mime: string): Blob {
  const bin = atob(b64)
  const len = bin.length
  const arr = new Uint8Array(len)
  for (let i = 0; i < len; i += 1) arr[i] = bin.charCodeAt(i)
  return new Blob([arr], { type: mime || 'application/octet-stream' })
}

/**
 * Скачать один прикреплённый файл из облака и создать objectURL для превью.
 * Если файл уже был загружен в этой сессии — переиспользуем URL.
 */
async function loadCloudFile(cid: string, name: string, mime: string, size: number) {
  if (cloudFiles.value[cid] || cloudFilesBusy.value[cid]) return
  if (!openedProfile.value) return
  const uri = fetchUri.value.trim()
  if (!uri) return
  cloudFilesBusy.value = { ...cloudFilesBusy.value, [cid]: true }
  try {
    const b64 = await cloudSeederFetchProfileFile(
      uri,
      openedProfile.value.userId,
      cid,
    )
    const blob = b64ToBlob(b64, mime)
    const url = URL.createObjectURL(blob)
    cloudFiles.value = {
      ...cloudFiles.value,
      [cid]: { url, mime, size, name },
    }
    const nextErr = { ...cloudFilesError.value }
    delete nextErr[cid]
    cloudFilesError.value = nextErr
  } catch (e) {
    cloudFilesError.value = { ...cloudFilesError.value, [cid]: errText(e) }
  } finally {
    const nextBusy = { ...cloudFilesBusy.value }
    delete nextBusy[cid]
    cloudFilesBusy.value = nextBusy
  }
}

/** Скачать файл на диск (через <a download>). */
async function downloadCloudFile(cid: string, name: string, mime: string, size: number) {
  await loadCloudFile(cid, name, mime, size)
  const st = cloudFiles.value[cid]
  if (!st) return
  const a = document.createElement('a')
  a.href = st.url
  a.download = name || cid
  document.body.appendChild(a)
  a.click()
  a.remove()
}

/** Освободить все objectURL при закрытии модалки, чтобы не текла память. */
function revokeAllCloudFiles() {
  for (const st of Object.values(cloudFiles.value)) {
    try {
      URL.revokeObjectURL(st.url)
    } catch {
      /* noop */
    }
  }
  cloudFiles.value = {}
  cloudFilesBusy.value = {}
  cloudFilesError.value = {}
}

async function fetchProfilesFromCurrentUri() {
  if (!fetchUri.value.trim()) return
  profilesFetchBusy.value = true
  profilesFetchError.value = ''
  profilesManifest.value = null
  revokeAllCloudFiles()
  openedProfile.value = null
  try {
    profilesManifest.value = await cloudSeederFetchProfilesManifest(fetchUri.value.trim())
  } catch (e) {
    profilesFetchError.value = errText(e)
  } finally {
    profilesFetchBusy.value = false
  }
}

async function openCloudProfile(userId: string) {
  if (!fetchUri.value.trim()) return
  revokeAllCloudFiles()
  openedProfileBusy.value = true
  try {
    openedProfile.value = await cloudSeederFetchProfile(fetchUri.value.trim(), userId)
  } catch (e) {
    profilesFetchError.value = errText(e)
  } finally {
    openedProfileBusy.value = false
  }
}

function closeCloudProfile() {
  revokeAllCloudFiles()
  openedProfile.value = null
}

// ── QR-код для URI ──────────────────────────────────────────────────
const qrFor = ref('')
const qrData = ref('')

async function toggleQr(uri: string) {
  if (qrFor.value === uri) {
    qrFor.value = ''
    qrData.value = ''
    return
  }
  try {
    qrData.value = await QRCode.toDataURL(uri, {
      width: 220,
      margin: 1,
      color: { dark: '#062a2a', light: '#d9fbfa' },
    })
    qrFor.value = uri
  } catch {
    showToast(t('pubSeeders.toast.qrFailed'))
  }
}

async function loadAccounts() {
  seederAccounts.value = await loadSeederAccounts()
  // восстановленный из localStorage аккаунт мог быть отвязан — проверяем
  const exists = seederAccounts.value.some((a) => a.id === activeAccId.value)
  if ((!activeAccId.value || !exists) && seederAccounts.value.length) {
    activeAccId.value = seederAccounts.value[0].id
  }
}

async function loadRegistry() {
  try {
    const [list, orig] = await Promise.all([
      cloudSeederRegistryGet(),
      cloudSeederRegistryOriginsGet(),
    ])
    registry.value = list
    origins.value = orig
  } catch {
    registry.value = []
    origins.value = {}
  }
}

onMounted(async () => {
  window.addEventListener('seeder-replicated', onReplicated)
  window.addEventListener('seeder-secondary-error', onSecondaryError)
  window.addEventListener('seeder-secondary-replicated', onSecondaryReplicated)
  unProg = await onSeederPublishProgress((p) => {
    pubProg.value = p
    trackArchiveDone(p)
  })
  unProfileProg = await onProfilePublishProgress((p) => {
    profileProg.value = p
  })
  // gossip-анонсы и merge чужих seeders.enc пополняют реестр в фоне
  unReg = await onSeederRegistryUpdated(() => void loadRegistry())
  await Promise.all([loadAccounts(), loadRegistry(), loadStatuses(), loadProfileState()])
  // Пришли с плашки Cloud mode (route.hash === '#active-seeders')?
  // Скроллим к реестру и включаем импульс подсветки на «живых» сидерах.
  if (route.hash === '#active-seeders') {
    await nextTick()
    const el = document.getElementById('active-seeders')
    if (el) el.scrollIntoView({ behavior: 'smooth', block: 'start' })
    highlightActive.value = true
    setTimeout(() => (highlightActive.value = false), 4200)
  }
})

onUnmounted(() => {
  window.removeEventListener('seeder-replicated', onReplicated)
  window.removeEventListener('seeder-secondary-error', onSecondaryError)
  window.removeEventListener('seeder-secondary-replicated', onSecondaryReplicated)
  if (unProg) unProg()
  if (unReg) unReg()
  if (unProfileProg) unProfileProg()
  revokeAllCloudFiles()
})

function showToast(msg: string) {
  toast.value = msg
  if (toastTimer) clearTimeout(toastTimer)
  toastTimer = setTimeout(() => (toast.value = ''), 2400)
}

function errText(e: any): string {
  if (typeof e === 'string') return e
  return e?.message || String(e) || t('pubSeeders.err.generic')
}

async function doPublish() {
  const acc = activeAcc.value
  if (!acc) return
  busy.value = true
  errorMsg.value = ''
  pubProg.value = null
  try {
    const token = await freshSeederToken(acc)
    const weblink = acc.provider === 'mailru' ? mrWeblink() : undefined
    publishResult.value = await cloudSeederPublish(token, acc.provider as SeederProviderId, weblink)
    if (weblink) localStorage.setItem(LS_MR_LINK, mrPublicLink.value.trim())
    markOwnSeeder(publishResult.value.uri, publishResult.value.publicKey)
    lastRep.value = lastReplication()
    hasOwnSeeder.value = true
    // Автоматически добавим в реестр (свой URI = якорь для восстановления сети)
    if (!registry.value.includes(publishResult.value.uri)) {
      const next = [publishResult.value.uri, ...registry.value]
      await cloudSeederRegistrySet(next)
      registry.value = next
    }
    showToast(t('pubSeeders.toast.published'))
    // Secondary-зеркало: best-effort, ошибка НЕ отменяет успех primary
    secondaryError.value = ''
    secondaryResult.value = null
    if (secondaryId.value) {
      try {
        secondaryResult.value = await publishSecondary(seederAccounts.value, acc.id)
        if (secondaryResult.value) await loadRegistry()
      } catch (e2) {
        secondaryError.value = t('pubSeeders.err.secondaryPrefix') + errText(e2)
      }
    }
  } catch (e) {
    const raw = errText(e)
    // Автопубликация папки Mail.ru не удалась → раскрываем аварийный ввод ссылки
    if (acc.provider === 'mailru' && /MR_NEED_WEBLINK|o2\.mail\.ru|weblink|publish/i.test(raw)) {
      mrLinkNeeded.value = true
      errorMsg.value = t('pubSeeders.err.mrPublishFallback')
    } else {
      errorMsg.value = raw
    }
  } finally {
    busy.value = false
  }
}

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    showToast(t('pubSeeders.toast.copied'))
  } catch {
    showToast(t('pubSeeders.toast.copyFailed'))
  }
}

async function doFetch() {
  const uri = fetchUri.value.trim()
  if (!uri) return
  fetchBusy.value = true
  fetchError.value = ''
  fetchResult.value = null
  try {
    fetchResult.value = await cloudSeederFetch(uri)
  } catch (e) {
    fetchError.value = errText(e)
  } finally {
    fetchBusy.value = false
    void loadStatuses()
    void loadRegistry()
  }
}

async function addToRegistry(uri: string) {
  if (registry.value.includes(uri)) {
    showToast(t('pubSeeders.toast.alreadyInRegistry'))
    return
  }
  const next = [uri, ...registry.value]
  await cloudSeederRegistrySet(next)
  registry.value = next
  showToast(t('pubSeeders.toast.addedToRegistry'))
}

async function removeFromRegistry(uri: string) {
  const next = registry.value.filter((x) => x !== uri)
  await cloudSeederRegistrySet(next)
  registry.value = next
}

function shortUri(uri: string): string {
  // sn://seed/<y|g|m>@<folder>@<pubkey> — фолдер обычно длинный base64/weblink
  const m = uri.match(/^(sn:\/\/seed\/[ygm])@(.+)@([0-9a-f]{66})$/i)
  if (!m) return uri
  const folder = m[2]
  const fs = folder.length > 16 ? folder.slice(0, 8) + '…' + folder.slice(-6) : folder
  return `${m[1]}@${fs}@${m[3].slice(0, 8)}…${m[3].slice(-6)}`
}

function itemsCount(bucket: SeederFetchResult['dapps']): number {
  if (!bucket || !Array.isArray(bucket.items)) return 0
  return bucket.items.length
}
</script>

<template>
  <div class="seeders" data-testid="seeders-view">
    <header class="sd-head">
      <h1 class="sd-title">{{ t('pubSeeders.title') }}</h1>
      <p class="sd-sub" v-html="t('pubSeeders.subtitle')" />
    </header>

    <!-- ── Сводка по /ntfryseed за сессию ──────────────────────────── -->
    <section
      v-if="cloudTotals.count > 0"
      class="sd-cloud-summary"
      data-testid="seeders-cloud-summary"
    >
      <span class="sd-cs-tag">{{ t('pubSeeders.sessionTag') }}</span>
      <span class="sd-cs-item" data-testid="seeders-cs-count">
        <i18n-t keypath="pubSeeders.sessionArchives" :plural="cloudTotals.count">
          <template #count><b>{{ cloudTotals.count }}</b></template>
        </i18n-t>
      </span>
      <span class="sd-cs-sep">·</span>
      <span class="sd-cs-item" data-testid="seeders-cs-size">
        ~<b>{{ fmtSize(cloudTotals.size) }}</b>
      </span>
      <template v-if="cloudTotals.hasCompression && cloudTotals.saved > 0">
        <span class="sd-cs-sep">·</span>
        <span class="sd-cs-item sd-cs-saved" data-testid="seeders-cs-saved">
          {{ t('pubSeeders.sessionSaved') }} <b>{{ fmtSize(cloudTotals.saved) }}</b>
        </span>
        <span class="sd-cs-sep">·</span>
        <span class="sd-cs-item sd-cs-saved" data-testid="seeders-cs-avg">
          {{ t('pubSeeders.sessionAvg') }} <b>−{{ cloudTotals.avgPct }}%</b>
        </span>
      </template>
      <template v-if="cloudTotals.avgKbps > 0">
        <span class="sd-cs-sep">·</span>
        <span class="sd-cs-item sd-cs-speed" data-testid="seeders-cs-speed">
          ⇈ SPEED <b>{{ fmtSpeed(cloudTotals.avgKbps) }}</b>
        </span>
      </template>
    </section>

    <!-- ── Публикация ────────────────────────────────────────────── -->
    <section class="sd-card" data-testid="seeders-publish">
      <div class="sd-card-head">
        <span class="sd-tag">{{ t('pubSeeders.publishTag') }}</span>
        <span v-if="canPublish" class="sd-tag sd-tag--ok" data-testid="seeders-active-account">● {{ provName(activeAcc!) }} · {{ activeAcc!.label || activeAcc!.login || '' }}</span>
      </div>

      <!-- Нет облачного аккаунта → мягкий гейт с ссылкой на Vault -->
      <div v-if="!canPublish" class="sd-empty" data-testid="seeders-no-yandex">
        <p class="sd-empty-msg" v-html="t('pubSeeders.noAccountMsg')" />
        <router-link class="sd-btn sd-btn--ghost" :to="{ name: 'vault' }" data-testid="seeders-goto-vault">
          {{ t('pubSeeders.goToVault') }}
        </router-link>
      </div>

      <div v-else class="sd-pub-body">
        <p class="sd-desc">
          {{ t('pubSeeders.publishDesc') }}
        </p>
        <div class="sd-actions">
          <button
            class="sd-btn sd-btn--primary"
            :disabled="busy"
            data-testid="seeders-publish-btn"
            @click="doPublish"
          >
            {{ busy ? t('pubSeeders.publishing') : t('pubSeeders.publishBtn') }}
          </button>
          <select
            v-if="seederAccounts.length > 1"
            v-model="activeAccId"
            class="sd-select"
            data-testid="seeders-account-select"
          >
            <option v-for="a in seederAccounts" :key="a.id" :value="a.id">
              {{ provName(a) }} · {{ a.label || a.login || a.id }}
            </option>
          </select>
        </div>

        <!-- Secondary drive: зеркалирование /ntfryseed на второй диск -->
        <div
          v-if="seederAccounts.length > 1"
          class="sd-secondary-row"
          data-testid="seeders-secondary-row"
        >
          <span class="sd-secondary-label">{{ t('pubSeeders.secondaryLabel') }}</span>
          <select v-model="secondaryId" class="sd-select" data-testid="seeders-secondary-select">
            <option value="">{{ t('pubSeeders.secondaryOff') }}</option>
            <option v-for="a in secondaryOptions" :key="a.id" :value="a.id">
              {{ provName(a) }} · {{ a.label || a.login || a.id }}
            </option>
          </select>
          <span v-if="secondaryId" class="sd-secondary-hint">
            {{ t('pubSeeders.secondaryHint') }}
          </span>
        </div>

        <!-- Mail.ru: ручной ввод публичной ссылки папки — виден, если
             автопубликация не сработала (VK ID) или ссылка уже задана -->
        <div
          v-if="showMrLink"
          class="sd-mr-link"
          data-testid="seeders-mailru-weblink-block"
        >
          <div class="sd-mr-row">
            <button
              class="sd-btn sd-btn--ghost"
              data-testid="seeders-mailru-open-folder"
              @click="openMrFolder"
            >
              {{ t('pubSeeders.mrOpenFolder') }}
            </button>
            <input
              v-model="mrPublicLink"
              class="sd-input sd-mr-input"
              placeholder="https://cloud.mail.ru/public/…"
              data-testid="seeders-mailru-weblink-input"
              @keyup.enter="useMrLink"
            />
            <button
              class="sd-btn sd-btn--primary"
              :disabled="busy || !mrPublicLink.trim()"
              data-testid="seeders-mailru-use-link"
              @click="useMrLink"
            >
              {{ t('pubSeeders.mrApply') }}
            </button>
          </div>
          <p class="sd-mr-hint" v-html="t('pubSeeders.mrHint')" />
        </div>

        <div v-if="errorMsg" class="sd-err" data-testid="seeders-publish-error">{{ errorMsg }}</div>
        <div v-if="secondaryError" class="sd-err sd-err--warn" data-testid="seeders-secondary-error">
          {{ secondaryError }}
        </div>

        <!-- Live-прогресс публикации: что именно пакуется/заливается, размер, скорость -->
        <div v-if="busy && pubProg" class="sd-progress" data-testid="seeders-publish-progress">
          <div class="sd-prog-line" data-testid="seeders-publish-progress-line">
            <span v-if="pubProg.phase === 'meta'">{{ t('pubSeeders.progMeta') }}</span>
            <span v-else-if="pubProg.phase === 'archive'">
              {{ t('pubSeeders.progArchive') }} <b>{{ pubProg.index }}/{{ pubProg.total }}</b> ·
              <code class="sd-prog-id" :title="pubProg.appId">{{ shortAppId(pubProg.appId) }}</code>
              <template v-if="pubProg.status === 'packing'">{{ t('pubSeeders.progPacking') }}</template>
              <template v-else-if="pubProg.status === 'uploading'">
                {{ t('pubSeeders.progUploading') }} {{ fmtUpload(pubProg) }}
                <span
                  v-if="pubProg.attempt > 1"
                  class="sd-prog-retry"
                  data-testid="seeders-publish-retry-badge"
                  :title="t('pubSeeders.progRetryTitle')"
                >{{ t('pubSeeders.progAttempt', { n: pubProg.attempt }) }}</span>
              </template>
              <template v-else-if="pubProg.status === 'done'">
                — ✓ {{ fmtPacked(pubProg) }} · {{ fmtSpeed(pubProg.kbps) }}
              </template>
              <template v-else-if="pubProg.status === 'error'">
                — <span class="sd-prog-err">✗ {{ pubProg.error }}</span>
              </template>
            </span>
            <span v-else>{{ t('pubSeeders.progFinishing') }}</span>
          </div>
          <div class="sd-prog-bar">
            <div class="sd-prog-fill" :style="{ width: progPct + '%' }" />
          </div>
        </div>

        <div v-if="publishResult" class="sd-result" data-testid="seeders-publish-result">
          <div class="sd-uri">
            <span class="sd-uri-label">SEEDER URI</span>
            <code class="sd-uri-value" :title="publishResult.uri">{{ publishResult.uri }}</code>
            <button class="sd-copy" data-testid="seeders-copy-uri" @click="copyText(publishResult.uri)">
              [ COPY ]
            </button>
            <button class="sd-copy" data-testid="seeders-qr-uri" @click="toggleQr(publishResult.uri)">
              [ QR ]
            </button>
          </div>
          <div v-if="qrFor === publishResult.uri" class="sd-qr" data-testid="seeders-qr-own">
            <img :src="qrData" alt="Seeder URI QR" class="sd-qr-img" />
          </div>
          <div class="sd-stats">
            <span class="sd-stat"><b data-testid="seeders-stat-dapps">{{ publishResult.dapps }}</b> dApps</span>
            <span class="sd-stat"><b data-testid="seeders-stat-nodes">{{ publishResult.nodes }}</b> nodes</span>
            <span class="sd-stat"><b data-testid="seeders-stat-seeders">{{ publishResult.seeders }}</b> seeders</span>
            <span class="sd-stat"><b data-testid="seeders-stat-archives">{{ publishResult.archives }}</b> {{ t('pubSeeders.statArchives') }}</span>
            <span class="sd-stat sd-stat--dim">{{ t('pubSeeders.statUpdated', { when: fmtWhen(publishResult.updated) }) }}</span>
          </div>
          <div v-if="secondaryResult" class="sd-uri sd-uri--mirror" data-testid="seeders-secondary-result">
            <span class="sd-uri-label">{{ t('pubSeeders.mirrorUriLabel') }}</span>
            <code class="sd-uri-value" :title="secondaryResult.uri">{{ secondaryResult.uri }}</code>
            <button class="sd-copy" data-testid="seeders-copy-mirror-uri" @click="copyText(secondaryResult.uri)">
              [ COPY ]
            </button>
          </div>
        </div>

        <!-- Авто-репликация каждые 30 мин (Phase 3b) -->
        <div class="sd-autorep" data-testid="seeders-autorep-row">
          <button
            class="sd-switch"
            :class="{ on: autoRep }"
            role="switch"
            :aria-checked="autoRep"
            data-testid="seeders-autorep-toggle"
            @click="toggleAutoRep"
          >
            <span class="sd-switch-knob" />
          </button>
          <div class="sd-autorep-text">
            <span>{{ t('pubSeeders.autorepLabel', { state: autoRep ? t('pubSeeders.on') : t('pubSeeders.off') }) }}</span>
            <span class="sd-autorep-sub" data-testid="seeders-autorep-last">
              {{ hasOwnSeeder ? t('pubSeeders.autorepLast', { when: lastRepText }) : t('pubSeeders.autorepPending') }}
            </span>
          </div>
        </div>

        <!-- ── Public Profile Seeders: репликация профилей ─────────────── -->
        <div class="sd-autorep sd-autorep--profile" data-testid="profile-replicate-row">
          <button
            class="sd-switch"
            :class="{ on: profileAutoRep }"
            role="switch"
            :aria-checked="profileAutoRep"
            data-testid="profile-replicate-toggle"
            @click="toggleProfileAutoRep"
          >
            <span class="sd-switch-knob" />
          </button>
          <div class="sd-autorep-text">
            <span>
              <span v-html="t('pubSeeders.replProfilesLabel')" />
              {{ profileAutoRep ? t('pubSeeders.replProfilesOn') : t('pubSeeders.off') }}
            </span>
            <span class="sd-autorep-sub" data-testid="profile-replicate-last">
              {{ t('pubSeeders.replProfilesLastLabel', { when: profileLastRepText }) }}
            </span>
          </div>
          <button
            v-if="canPublish"
            class="sd-btn sd-btn--sm"
            :disabled="profileBusy"
            data-testid="profile-replicate-publish-now"
            @click="publishProfilesNow"
          >
            {{ profileBusy ? t('pubSeeders.publishing') : t('pubSeeders.publishNow') }}
          </button>
        </div>
        <div v-if="profileError" class="sd-err" data-testid="profile-replicate-error">
          {{ profileError }}
        </div>
        <div v-if="profileProg && profileBusy" class="sd-progress" data-testid="profile-replicate-progress">
          <div class="sd-prog-line">
            <span class="sd-prog-phase">[{{ profileProg.phase }}]</span>
            <span v-if="profileProg.userId" class="sd-prog-name">{{ profileProg.userId }}</span>
            <span v-if="profileProg.fileCid" class="sd-prog-sub">file {{ profileProg.fileCid.slice(0, 12) }}…</span>
            <span v-if="profileProg.total" class="sd-prog-idx">{{ profileProg.index }}/{{ profileProg.total }}</span>
            <span v-if="profileProg.kbps" class="sd-prog-kbps">{{ profileProg.kbps }} KB/s</span>
            <span
              v-if="profileProg.attempt > 1"
              class="sd-prog-retry"
              data-testid="profile-replicate-retry-badge"
            >{{ t('pubSeeders.progAttempt', { n: profileProg.attempt }) }}</span>
            <span class="sd-prog-status">{{ profileProg.status }}</span>
          </div>
        </div>
        <div v-if="profileResult" class="sd-result sd-result--profile" data-testid="profile-replicate-result">
          <div class="sd-stats">
            <span class="sd-stat"><b data-testid="profile-stat-users">{{ profileResult.usersPublished }}</b> {{ t('pubSeeders.profilesUploaded') }}</span>
            <span class="sd-stat"><b>{{ profileResult.usersSkipped }}</b> {{ t('pubSeeders.profilesUnchanged') }}</span>
            <span class="sd-stat"><b data-testid="profile-stat-files">{{ profileResult.filesUploaded }}</b> {{ t('pubSeeders.filesUploaded') }}</span>
            <span class="sd-stat"><b>{{ profileResult.filesSkipped }}</b> {{ t('pubSeeders.filesDedup') }}</span>
            <span v-if="profileResult.orphansDeleted" class="sd-stat">
              <b>{{ profileResult.orphansDeleted }}</b> {{ t('pubSeeders.orphansDeleted') }}
            </span>
          </div>
        </div>
      </div>
    </section>

    <!-- ── vDapps: official-анонс на облачный диск ─────────────────── -->
    <section v-if="vd?.eligible" class="sd-card" data-testid="seeders-vdapps">
      <div class="sd-card-head">
        <span class="sd-tag">{{ t('pubSeeders.vdappsTag') }}</span>
      </div>
      <p class="sd-hint">
        {{ t('pubSeeders.vdappsHint') }}
      </p>
      <div v-if="vd?.published" class="sd-stats" data-testid="vdapps-info">
        <span class="sd-stat">{{ t('pubSeeders.vdappsAnnounced') }} <b>{{ vdTime }}</b></span>
        <span class="sd-stat"><b>{{ vd.devs }}</b> {{ t('pubSeeders.vdappsDevs') }}</span>
        <span class="sd-stat"><b>{{ vd.dapps }}</b> {{ t('pubSeeders.vdappsOfficial') }}</span>
        <span class="sd-stat"><b>{{ vd.seeders }}</b> {{ t('pubSeeders.vdappsSeeders') }}</span>
      </div>
      <p v-else class="sd-hint" data-testid="vdapps-none">
        {{ t('pubSeeders.vdappsNone') }}
      </p>
    </section>

    <!-- ── Чтение чужого сидера ───────────────────────────────────── -->
    <section class="sd-card" data-testid="seeders-fetch">
      <div class="sd-card-head">
        <span class="sd-tag">{{ t('pubSeeders.fetchTag') }}</span>
      </div>
      <p class="sd-desc">
        {{ t('pubSeeders.fetchDesc') }}
      </p>
      <div class="sd-row">
        <input
          v-model="fetchUri"
          class="sd-input"
          placeholder="sn://seed/y@<folder>@<publicKey> · m@ — Mail.ru"
          data-testid="seeders-fetch-uri"
          @keyup.enter="doFetch"
        />
        <button
          class="sd-btn"
          :disabled="fetchBusy || !fetchUri.trim()"
          data-testid="seeders-fetch-btn"
          @click="doFetch"
        >
          {{ fetchBusy ? t('pubSeeders.fetching') : t('pubSeeders.fetchBtn') }}
        </button>
      </div>

      <div v-if="fetchError" class="sd-err" data-testid="seeders-fetch-error">{{ fetchError }}</div>

      <div v-if="fetchResult" class="sd-result" data-testid="seeders-fetch-result">
        <div class="sd-stats">
          <span class="sd-stat"><b>{{ itemsCount(fetchResult.dapps) }}</b> dApps</span>
          <span class="sd-stat"><b>{{ itemsCount(fetchResult.nodes) }}</b> nodes</span>
          <span class="sd-stat"><b>{{ itemsCount(fetchResult.seeders) }}</b> seeders</span>
          <span class="sd-stat sd-stat--dim">pubkey {{ fetchResult.publicKey.slice(0, 12) }}…</span>
        </div>
        <!-- dApps сидера: с архивом в облаке можно установить без P2P -->
        <ul v-if="fetchDapps.length" class="sd-fetch-apps" data-testid="seeders-fetch-apps">
          <li v-for="d in fetchDapps" :key="d.id" class="sd-fetch-app">
            <span class="sd-fa-name" :title="d.id">
              {{ d.name || d.id }}
              <em v-if="d.version" class="sd-fa-ver">v{{ d.version }}</em>
            </span>
            <span v-if="d.archive" class="sd-fa-badge" :title="t('pubSeeders.archiveTitle')">{{ t('pubSeeders.archiveBadge') }}</span>
            <button
              v-if="d.archive && isTauri"
              class="sd-copy"
              :disabled="!!installingId"
              :data-testid="`seeders-install-${d.id}`"
              @click="installFromCloud(d.id)"
            >
              {{ installingId === d.id ? t('pubSeeders.installing') : t('pubSeeders.installBtn') }}
            </button>
          </li>
        </ul>
        <button
          class="sd-btn sd-btn--ghost sd-btn--sm"
          data-testid="seeders-add-to-registry"
          @click="addToRegistry(fetchUri.trim())"
        >
          {{ t('pubSeeders.addToRegistryBtn') }}
        </button>

        <!-- Public Profile Seeders: чтение чужих профилей и микроблогов ─ -->
        <div class="sd-profiles-fetch" data-testid="profiles-fetch-block">
          <button
            class="sd-btn sd-btn--ghost sd-btn--sm"
            :disabled="profilesFetchBusy || !fetchUri.trim()"
            data-testid="profiles-fetch-btn"
            @click="fetchProfilesFromCurrentUri"
          >
            {{ profilesFetchBusy ? t('pubSeeders.fetchingProfiles') : t('pubSeeders.fetchProfilesBtn') }}
          </button>
          <div v-if="profilesFetchError" class="sd-err" data-testid="profiles-fetch-error">
            {{ profilesFetchError }}
          </div>
          <ul
            v-if="profilesManifest && profilesManifest.users.length"
            class="sd-fetch-apps"
            data-testid="profiles-fetch-list"
          >
            <li v-for="u in profilesManifest.users" :key="u.userId" class="sd-fetch-app">
              <span class="sd-fa-name" :title="u.userId">
                {{ u.name || u.userId }}
                <em class="sd-fa-ver">{{ t('pubSeeders.postsShort', { n: u.posts }) }}</em>
              </span>
              <button
                class="sd-copy"
                :disabled="openedProfileBusy"
                :data-testid="`profiles-open-${u.userId}`"
                @click="openCloudProfile(u.userId)"
              >
                {{ openedProfileBusy ? '…' : 'VIEW' }}
              </button>
            </li>
          </ul>
          <div
            v-if="profilesManifest && !profilesManifest.users.length"
            class="sd-empty"
            data-testid="profiles-fetch-empty"
          >
            {{ t('pubSeeders.profilesEmpty') }}
          </div>
        </div>
      </div>
    </section>

    <!-- Модалка просмотра чужого профиля из облака ─────────────────── -->
    <div
      v-if="openedProfile"
      class="sd-modal"
      data-testid="cloud-profile-modal"
      @click.self="closeCloudProfile"
    >
      <div class="sd-modal-body">
        <div class="sd-modal-head">
          <div class="sd-modal-title">
            <b>{{ openedProfile.profile.name || openedProfile.userId }}</b>
            <span class="sd-fa-ver">{{ t('pubSeeders.cloudFromCloud') }}</span>
          </div>
          <button
            class="sd-copy"
            data-testid="cloud-profile-close"
            @click="closeCloudProfile"
          >✕</button>
        </div>
        <div v-if="openedProfile.profile.identity" class="sd-modal-sub">
          {{ openedProfile.profile.identity }}
        </div>
        <ul class="sd-cloud-posts">
          <li
            v-for="pw in openedProfile.posts"
            :key="pw.post.id"
            class="sd-cloud-post"
            :data-testid="`cloud-post-${pw.post.id}`"
          >
            <div class="sd-cloud-post-head">
              <span class="sd-cloud-post-ts">{{ new Date(pw.post.ts).toLocaleString() }}</span>
              <span v-if="pw.comments.length" class="sd-fa-ver">
                {{ t('pubSeeders.commentsPlural', { n: pw.comments.length }) }}
              </span>
            </div>
            <div class="sd-cloud-post-text">{{ pw.post.text }}</div>
            <ul v-if="pw.post.files.length" class="sd-cloud-files">
              <li
                v-for="f in pw.post.files"
                :key="f.cid"
                class="sd-cloud-file"
                :data-testid="`cloud-file-${f.cid}`"
              >
                <div class="sd-cloud-file-row">
                  <span class="sd-cloud-file-meta">
                    📎 <b>{{ f.name }}</b>
                    <em>({{ Math.ceil(f.size / 1024) }} KB, {{ f.mime }})</em>
                  </span>
                  <span class="sd-cloud-file-actions">
                    <button
                      v-if="!cloudFiles[f.cid] && fileKind(f.mime) !== 'other'"
                      class="sd-copy"
                      :disabled="!!cloudFilesBusy[f.cid]"
                      :data-testid="`cloud-file-load-${f.cid}`"
                      @click="loadCloudFile(f.cid, f.name, f.mime, f.size)"
                    >
                      {{ cloudFilesBusy[f.cid] ? '…' : t('pubSeeders.load') }}
                    </button>
                    <button
                      class="sd-copy"
                      :disabled="!!cloudFilesBusy[f.cid]"
                      :data-testid="`cloud-file-download-${f.cid}`"
                      @click="downloadCloudFile(f.cid, f.name, f.mime, f.size)"
                    >
                      {{ cloudFilesBusy[f.cid] && !cloudFiles[f.cid] ? '…' : t('pubSeeders.download') }}
                    </button>
                  </span>
                </div>
                <div v-if="cloudFilesError[f.cid]" class="sd-err">
                  {{ cloudFilesError[f.cid] }}
                </div>
                <template v-if="cloudFiles[f.cid]">
                  <img
                    v-if="fileKind(f.mime) === 'image'"
                    :src="cloudFiles[f.cid].url"
                    :alt="f.name"
                    class="sd-cloud-media sd-cloud-media--img"
                    :data-testid="`cloud-file-preview-${f.cid}`"
                  />
                  <audio
                    v-else-if="fileKind(f.mime) === 'audio'"
                    :src="cloudFiles[f.cid].url"
                    controls
                    preload="none"
                    class="sd-cloud-media sd-cloud-media--audio"
                    :data-testid="`cloud-file-preview-${f.cid}`"
                  />
                  <video
                    v-else-if="fileKind(f.mime) === 'video'"
                    :src="cloudFiles[f.cid].url"
                    controls
                    preload="metadata"
                    class="sd-cloud-media sd-cloud-media--video"
                    :data-testid="`cloud-file-preview-${f.cid}`"
                  />
                </template>
              </li>
            </ul>
          </li>
        </ul>
        <div v-if="!openedProfile.posts.length" class="sd-empty">
          {{ t('pubSeeders.postsEmpty') }}
        </div>
      </div>
    </div>

    <!-- ── Реестр известных сидер-URI ─────────────────────────────── -->
    <section
      id="active-seeders"
      class="sd-card"
      :class="{ 'sd-card--focus': highlightActive }"
      data-testid="seeders-registry"
    >
      <div class="sd-card-head">
        <span class="sd-tag">{{ t('pubSeeders.registryTag', { n: registry.length }) }}</span>
        <span
          v-if="holdingCount > 0"
          class="sd-holding-badge"
          :class="{ pulse: highlightActive }"
          :title="t('pubSeeders.holdingCountTitle')"
          data-testid="seeders-holding-count"
        >{{ t('pubSeeders.holdingCountLabel', { n: holdingCount }) }}</span>
        <button
          v-if="registry.length && isTauri"
          class="sd-copy"
          :disabled="checkBusy"
          data-testid="seeders-check-all"
          @click="checkAll"
        >
          {{ checkBusy ? t('pubSeeders.checking') : t('pubSeeders.checkAll') }}
        </button>
      </div>
      <p class="sd-desc">
        {{ t('pubSeeders.registryDesc') }}
      </p>
      <div v-if="!registry.length" class="sd-empty-mini" data-testid="seeders-registry-empty">
        {{ t('pubSeeders.registryEmpty') }}
      </div>
      <ul v-else class="sd-list">
        <li
          v-for="uri in orderedRegistry"
          :key="uri"
          class="sd-list-item"
          :class="{ 'sd-list-item--holding': isHoldingNetwork(uri), 'sd-pulse': highlightActive && isHoldingNetwork(uri) }"
          :data-testid="`seeders-registry-item-${uri.slice(-8)}`"
        >
          <div class="sd-list-row">
            <code class="sd-list-uri" :title="uri">{{ shortUri(uri) }}</code>
            <span
              v-if="isHoldingNetwork(uri)"
              class="sd-holding"
              :title="t('pubSeeders.holdingBadgeTitle')"
              :data-testid="`seeders-holding-${uri.slice(-8)}`"
            >{{ t('pubSeeders.holdingBadge') }}</span>
            <span
              v-if="originBadge(uri)"
              class="sd-origin"
              :class="`org-${originBadge(uri)!.cls}`"
              :title="originBadge(uri)!.title"
              :data-testid="`seeders-origin-${uri.slice(-8)}`"
            >{{ originBadge(uri)!.label }}</span>
            <span class="sd-status" :data-testid="`seeders-status-${uri.slice(-8)}`">
              <span class="sd-dot" :class="dotClass(uri)" />{{ statusText(uri) }}
            </span>
            <div class="sd-list-actions">
              <button class="sd-copy" @click="copyText(uri)" :data-testid="`seeders-copy-${uri.slice(-8)}`">
                COPY
              </button>
              <button class="sd-copy" @click="toggleQr(uri)" :data-testid="`seeders-qr-${uri.slice(-8)}`">
                QR
              </button>
              <button
                class="sd-copy sd-copy--danger"
                @click="removeFromRegistry(uri)"
                :data-testid="`seeders-remove-${uri.slice(-8)}`"
              >
                {{ t('pubSeeders.remove') }}
              </button>
            </div>
          </div>
          <div v-if="qrFor === uri" class="sd-qr" :data-testid="`seeders-qr-panel-${uri.slice(-8)}`">
            <img :src="qrData" alt="Seeder URI QR" class="sd-qr-img" />
          </div>
        </li>
      </ul>
    </section>

    <Transition name="sd-toast">
      <div v-if="toast" class="sd-toast" data-testid="seeders-toast">{{ toast }}</div>
    </Transition>
  </div>
</template>

<style scoped>
.seeders {
  padding: 24px 32px 64px;
  max-width: 960px;
  margin: 0 auto;
  --seeder: light-dark(#0d9488, #26d0ce);
  --seeder-glow: light-dark(rgba(13, 148, 136, 0.18), rgba(38, 208, 206, 0.28));
  --seeder-dim: light-dark(rgba(13, 148, 136, 0.45), rgba(38, 208, 206, 0.5));
}

.sd-head { margin-bottom: 28px; }
.sd-title {
  font-family: var(--font-mono);
  font-size: 22px;
  font-weight: var(--fw-700);
  letter-spacing: 0.08em;
  color: var(--seeder);
  text-shadow: 0 0 22px var(--seeder-glow);
  margin: 0 0 10px;
}
.sd-sub {
  font-size: 13px;
  color: var(--text-dim);
  max-width: 720px;
  margin: 0;
  line-height: 1.65;
}
.sd-sub b { color: var(--text); font-weight: var(--fw-700); }

.sd-cloud-summary {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  padding: 10px 14px;
  margin-bottom: 20px;
  background: rgba(38, 208, 206, 0.06);
  border: 1px solid var(--seeder-dim);
  border-left: 2px solid var(--seeder);
  border-radius: 4px;
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--text-dim);
}
.sd-cs-tag {
  color: var(--seeder);
  font-size: 10.5px;
  letter-spacing: 0.14em;
  margin-right: 4px;
}
.sd-cs-item b {
  color: var(--seeder);
  font-weight: var(--fw-700);
  margin: 0 3px;
}
.sd-cs-saved b { color: light-dark(#15803d, #22c55e); }
.sd-cs-speed b { color: light-dark(#7c3aed, #a78bfa); }
.sd-cs-sep { opacity: 0.4; }

.sd-card {
  margin-bottom: 20px;
  padding: 20px 22px;
  background: light-dark(var(--panel), rgba(0, 0, 0, 0.34));
  border: 1px solid var(--seeder-dim);
  border-radius: 6px;
  box-shadow: 0 0 24px rgba(38, 208, 206, 0.06);
}

.sd-card-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
}
.sd-tag {
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.14em;
  color: var(--seeder);
}
.sd-tag--ok { color: light-dark(#15803d, #22c55e); }

.sd-desc {
  font-size: 13px;
  color: var(--text-dim);
  margin: 0 0 14px;
  line-height: 1.6;
}

.sd-empty {
  padding: 16px 0 4px;
  border-top: 1px dashed light-dark(rgba(0, 0, 0, 0.12), rgba(255, 255, 255, 0.08));
  display: flex;
  flex-direction: column;
  gap: 14px;
  align-items: flex-start;
}
.sd-empty-msg { margin: 0; font-size: 13.5px; color: var(--text-dim); line-height: 1.6; }
.sd-empty-msg b { color: var(--text); }

.sd-pub-body { display: flex; flex-direction: column; gap: 14px; }

/* live-прогресс публикации архивов */
.sd-progress {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px 12px;
  border: 1px solid color-mix(in srgb, light-dark(#0d9488, #26d0ce) 30%, transparent);
  border-radius: 6px;
  background: color-mix(in srgb, light-dark(#0d9488, #26d0ce) 5%, transparent);
}
.sd-prog-line {
  font-size: 12px;
  color: var(--text, #d9fbfa);
  letter-spacing: 0.02em;
}
.sd-prog-id {
  font-family: var(--font-mono, ui-monospace, monospace);
  font-size: 11px;
  color: light-dark(#0d9488, #26d0ce);
}
.sd-prog-err { color: var(--danger, light-dark(#dc2626, #f87171)); }
.sd-prog-retry {
  display: inline-block;
  margin-left: 6px;
  padding: 1px 7px;
  border-radius: 999px;
  font-size: 0.72rem;
  font-weight: 600;
  color: var(--warning, light-dark(#b45309, #fbbf24));
  border: 1px solid currentColor;
  animation: sd-retry-pulse 1.2s ease-in-out infinite;
}
@keyframes sd-retry-pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.45; }
}
.sd-prog-bar {
  height: 4px;
  border-radius: 2px;
  overflow: hidden;
  background: color-mix(in srgb, light-dark(#0d9488, #26d0ce) 14%, transparent);
}
.sd-prog-fill {
  height: 100%;
  background: light-dark(#0d9488, #26d0ce);
  transition: width 0.35s ease;
}
.sd-actions { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }

.sd-btn {
  padding: 9px 18px;
  background: transparent;
  border: 1px solid var(--seeder-dim);
  color: var(--seeder);
  font-family: var(--font-mono);
  font-size: 12px;
  font-weight: var(--fw-700);
  letter-spacing: 0.08em;
  border-radius: 4px;
  cursor: pointer;
  transition: background 0.15s ease, box-shadow 0.15s ease, transform 0.1s ease;
  text-decoration: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
}
.sd-btn:disabled { opacity: 0.45; cursor: not-allowed; }
.sd-btn:hover:not(:disabled) {
  background: rgba(38, 208, 206, 0.08);
  box-shadow: 0 0 18px var(--seeder-glow);
}
.sd-btn:active:not(:disabled) { transform: translateY(1px); }
.sd-btn--primary {
  background: rgba(38, 208, 206, 0.14);
  border-color: var(--seeder);
}
.sd-btn--ghost { color: var(--text-dim); border-color: var(--border); }
.sd-btn--ghost:hover:not(:disabled) { color: var(--seeder); border-color: var(--seeder-dim); }
.sd-btn--sm { padding: 6px 14px; font-size: 11px; }

.sd-select {
  padding: 9px 12px;
  background: light-dark(rgba(0, 0, 0, 0.06), rgba(0, 0, 0, 0.4));
  border: 1px solid var(--border);
  color: var(--text);
  font-family: var(--font-mono);
  font-size: 12px;
  border-radius: 4px;
}

.sd-err {
  padding: 10px 12px;
  background: rgba(239, 68, 68, 0.1);
  border: 1px solid rgba(239, 68, 68, 0.5);
  border-radius: 4px;
  color: light-dark(#dc2626, #fca5a5);
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.5;
  word-break: break-all;
}

.sd-result {
  margin-top: 4px;
  padding: 14px;
  background: rgba(38, 208, 206, 0.04);
  border: 1px solid var(--seeder-dim);
  border-radius: 4px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.sd-uri {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.sd-uri-label {
  font-family: var(--font-mono);
  font-size: 10px;
  letter-spacing: 0.14em;
  color: var(--text-dim);
}
.sd-uri-value {
  flex: 1;
  padding: 8px 12px;
  background: light-dark(rgba(0, 0, 0, 0.05), rgba(0, 0, 0, 0.5));
  border: 1px solid var(--border);
  border-radius: 4px;
  color: var(--seeder);
  font-family: var(--font-mono);
  font-size: 12px;
  word-break: break-all;
  min-width: 220px;
}

.sd-copy {
  padding: 6px 12px;
  background: transparent;
  border: 1px solid var(--border);
  color: var(--text-dim);
  font-family: var(--font-mono);
  font-size: 10.5px;
  letter-spacing: 0.08em;
  border-radius: 3px;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s;
}
.sd-copy:hover { color: var(--seeder); border-color: var(--seeder-dim); }
.sd-copy--danger:hover { color: light-dark(#dc2626, #fca5a5); border-color: rgba(239, 68, 68, 0.5); }

.sd-stats { display: flex; gap: 18px; flex-wrap: wrap; align-items: center; }
.sd-stat {
  font-family: var(--font-mono);
  font-size: 12.5px;
  color: var(--text-dim);
}
.sd-stat b { color: var(--seeder); font-weight: var(--fw-700); margin-right: 4px; font-size: 15px; }
.sd-stat--dim { font-size: 11px; color: var(--text-dim); opacity: 0.7; }
.sd-stat--dim b { font-size: 11px; color: var(--text-dim); }

.sd-row {
  display: flex;
  gap: 10px;
  margin-bottom: 12px;
  flex-wrap: wrap;
}
.sd-input {
  flex: 1;
  min-width: 260px;
  padding: 9px 12px;
  background: light-dark(rgba(0, 0, 0, 0.05), rgba(0, 0, 0, 0.5));
  border: 1px solid var(--border);
  border-radius: 4px;
  color: var(--text);
  font-family: var(--font-mono);
  font-size: 12.5px;
}
.sd-input:focus { outline: none; border-color: var(--seeder-dim); box-shadow: 0 0 12px var(--seeder-glow); }

.sd-mr-link { margin-top: 12px; display: flex; flex-direction: column; gap: 6px; }
.sd-mr-row { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
.sd-mr-input { flex: 1; min-width: 200px; }
.sd-mr-hint { margin: 0; font-size: 12px; color: var(--text-dim); line-height: 1.55; }
.sd-mr-hint b { color: var(--text); }

.sd-empty-mini {
  padding: 12px 14px;
  background: light-dark(rgba(0, 0, 0, 0.04), rgba(0, 0, 0, 0.25));
  border: 1px dashed var(--border);
  border-radius: 4px;
  color: var(--text-dim);
  font-size: 12.5px;
  font-family: var(--font-mono);
}

.sd-list {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.sd-list-item {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px 12px;
  background: light-dark(rgba(0, 0, 0, 0.05), rgba(0, 0, 0, 0.28));
  border: 1px solid var(--border);
  border-radius: 4px;
}
.sd-list-row {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
}
.sd-list-uri {
  flex: 1;
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--seeder);
  word-break: break-all;
}
.sd-list-actions { display: flex; gap: 6px; }

.sd-status {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-family: var(--font-mono);
  font-size: 10.5px;
  color: var(--text-dim);
  white-space: nowrap;
}
/* Origin-бейдж: откуда узнали URI (свой / вручную / gossip / seeders.enc / bootstrap) */
.sd-origin {
  font-family: var(--font-mono);
  font-size: 9.5px;
  font-weight: var(--fw-700);
  letter-spacing: 0.1em;
  padding: 2px 7px;
  border-radius: 999px;
  border: 1px solid;
  white-space: nowrap;
  flex-shrink: 0;
}
.sd-origin.org-own { color: var(--seeder); border-color: var(--seeder); background: rgba(38, 208, 206, 0.14); }
.sd-origin.org-manual { color: light-dark(#b45309, #fbbf24); border-color: rgba(251, 191, 36, 0.5); background: rgba(251, 191, 36, 0.08); }
.sd-origin.org-gossip { color: light-dark(#7c3aed, #a78bfa); border-color: rgba(167, 139, 250, 0.5); background: rgba(167, 139, 250, 0.08); }
.sd-origin.org-seed { color: light-dark(#2563eb, #60a5fa); border-color: rgba(96, 165, 250, 0.5); background: rgba(96, 165, 250, 0.08); }
.sd-origin.org-boot { color: var(--text-dim); border-color: var(--border); background: light-dark(rgba(0, 0, 0, 0.03), rgba(255, 255, 255, 0.03)); }
.sd-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}
.sd-dot.ok { background: light-dark(#15803d, #22c55e); box-shadow: 0 0 8px rgba(34, 197, 94, 0.6); }
.sd-dot.bad { background: #ef4444; box-shadow: 0 0 8px rgba(239, 68, 68, 0.6); }
.sd-dot.unknown { background: light-dark(rgba(0, 0, 0, 0.22), rgba(255, 255, 255, 0.22)); }

/* Deep-link из Cloud mode: пришли с #active-seeders — карточка реестра
 * получает мягкую подсветку рамки, а живые сидеры пульсируют. */
.sd-card--focus {
  border-color: rgba(96, 165, 250, 0.55);
  box-shadow: 0 0 0 1px rgba(96, 165, 250, 0.35), 0 0 24px rgba(96, 165, 250, 0.2);
  transition: border-color 0.3s ease, box-shadow 0.3s ease;
}
.sd-list-item--holding {
  border-color: rgba(96, 165, 250, 0.45);
  background: linear-gradient(180deg, light-dark(rgba(96, 165, 250, 0.14), rgba(20, 34, 54, 0.55)), light-dark(rgba(96, 165, 250, 0.07), rgba(11, 21, 36, 0.55)));
}
.sd-pulse {
  animation: sd-holding-pulse 1.4s ease-in-out 0s 3;
}
@keyframes sd-holding-pulse {
  0%, 100% { box-shadow: 0 0 0 0 rgba(96, 165, 250, 0); }
  50% { box-shadow: 0 0 0 3px rgba(96, 165, 250, 0.35), 0 0 18px rgba(96, 165, 250, 0.25); }
}
/* «HOLDING»-бейдж рядом с URI: подчеркивает, что этот канал держит сеть. */
.sd-holding {
  font-family: var(--font-mono);
  font-size: 9.5px;
  font-weight: var(--fw-700);
  letter-spacing: 0.12em;
  padding: 2px 7px;
  border-radius: 999px;
  color: light-dark(#2563eb, #93c5fd);
  border: 1px solid rgba(96, 165, 250, 0.55);
  background: rgba(96, 165, 250, 0.12);
  white-space: nowrap;
  flex-shrink: 0;
}
/* Компактный счётчик «N держат сеть» в шапке карточки KNOWN SEEDERS. */
.sd-holding-badge {
  font-family: var(--font-mono);
  font-size: 10px;
  font-weight: var(--fw-700);
  letter-spacing: 0.1em;
  color: light-dark(#2563eb, #93c5fd);
  border: 1px solid rgba(96, 165, 250, 0.5);
  background: rgba(96, 165, 250, 0.1);
  padding: 3px 9px;
  border-radius: 999px;
  margin-left: auto;
  margin-right: 8px;
}
.sd-holding-badge.pulse {
  animation: sd-holding-pulse 1.4s ease-in-out 0s 3;
}

.sd-fetch-apps {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
  max-height: 260px;
  overflow-y: auto;
}
.sd-fetch-app {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 7px 10px;
  background: light-dark(rgba(0, 0, 0, 0.05), rgba(0, 0, 0, 0.3));
  border: 1px solid var(--border);
  border-radius: 4px;
}
.sd-fa-name {
  flex: 1;
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.sd-fa-ver { color: var(--text-dim); font-style: normal; font-size: 10.5px; margin-left: 4px; }
.sd-fa-badge {
  font-family: var(--font-mono);
  font-size: 10px;
  color: var(--seeder);
  border: 1px solid var(--seeder-dim);
  border-radius: 999px;
  padding: 2px 8px;
  white-space: nowrap;
}

.sd-qr { padding: 6px 0 2px; }
.sd-qr-img {
  width: 180px;
  height: 180px;
  border-radius: 6px;
  border: 1px solid var(--seeder-dim);
  box-shadow: 0 0 18px var(--seeder-glow);
}

.sd-autorep {
  display: flex;
  gap: 12px;
  align-items: center;
  margin-top: 4px;
  padding-top: 12px;
  border-top: 1px dashed light-dark(rgba(0, 0, 0, 0.12), rgba(255, 255, 255, 0.08));
}
.sd-switch {
  position: relative;
  width: 40px;
  height: 22px;
  border-radius: 999px;
  border: 1px solid var(--seeder-dim);
  background: light-dark(rgba(0, 0, 0, 0.06), rgba(0, 0, 0, 0.4));
  cursor: pointer;
  padding: 0;
  flex-shrink: 0;
  transition: background 0.15s ease, border-color 0.15s ease;
}
.sd-switch.on {
  background: rgba(38, 208, 206, 0.25);
  border-color: var(--seeder);
}
.sd-switch-knob {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: var(--text-dim);
  display: block;
  transition: transform 0.15s ease, background 0.15s ease;
}
.sd-switch.on .sd-switch-knob {
  transform: translateX(18px);
  background: var(--seeder);
}
.sd-autorep-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--text);
}
.sd-autorep-sub { font-size: 11px; color: var(--text-dim); }

/* Public Profile Seeders — блок в шапке publish + модалка чужого профиля */
.sd-autorep--profile {
  border-top: 1px dashed rgba(38, 208, 206, 0.2);
  padding-top: 12px;
  margin-top: 4px;
}
.sd-btn--sm { padding: 6px 10px; font-size: 11px; }
.sd-profiles-fetch {
  margin-top: 14px;
  padding-top: 12px;
  border-top: 1px dashed rgba(38, 208, 206, 0.2);
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.sd-result--profile { margin-top: 10px; }

.sd-modal {
  position: fixed;
  inset: 0;
  background: rgba(4, 22, 22, 0.78);
  backdrop-filter: blur(6px);
  z-index: 240;
  display: flex;
  align-items: flex-start;
  justify-content: center;
  padding: 6vh 20px;
  overflow-y: auto;
}
.sd-modal-body {
  width: min(720px, 100%);
  background: var(--surface, var(--panel));
  border: 1px solid var(--seeder);
  border-radius: 6px;
  padding: 20px;
  box-shadow: 0 0 40px var(--seeder-glow);
}
.sd-modal-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 8px;
}
.sd-modal-title { font-family: var(--font-mono); font-size: 14px; }
.sd-modal-sub { font-family: var(--font-mono); font-size: 11px; color: var(--text-dim); margin-bottom: 14px; }
.sd-cloud-posts { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 12px; }
.sd-cloud-post {
  padding: 12px;
  background: rgba(38, 208, 206, 0.05);
  border-left: 2px solid var(--seeder);
  border-radius: 3px;
}
.sd-cloud-post-head {
  display: flex;
  justify-content: space-between;
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--text-dim);
  margin-bottom: 6px;
}
.sd-cloud-post-text { font-size: 13px; white-space: pre-wrap; word-break: break-word; }
.sd-cloud-files { list-style: none; padding: 8px 0 0; margin: 0; display: flex; flex-direction: column; gap: 8px; }
.sd-cloud-file { font-family: var(--font-mono); font-size: 11px; color: var(--text-dim); }
.sd-cloud-file-row { display: flex; justify-content: space-between; align-items: center; gap: 8px; flex-wrap: wrap; }
.sd-cloud-file-meta b { color: var(--text); font-weight: normal; }
.sd-cloud-file-actions { display: flex; gap: 6px; }
.sd-cloud-media { display: block; margin-top: 8px; border-radius: 4px; max-width: 100%; }
.sd-cloud-media--img { max-height: 320px; object-fit: contain; background: rgba(0, 0, 0, 0.35); }
.sd-cloud-media--audio { width: 100%; }
.sd-cloud-media--video { max-height: 360px; background: #000; }

.sd-toast {
  position: fixed;
  bottom: 24px;
  left: 50%;
  transform: translateX(-50%);
  padding: 10px 18px;
  background: rgba(38, 208, 206, 0.14);
  border: 1px solid var(--seeder);
  color: var(--seeder);
  font-family: var(--font-mono);
  font-size: 12px;
  letter-spacing: 0.08em;
  border-radius: 4px;
  box-shadow: 0 0 24px var(--seeder-glow);
  z-index: 200;
}

.sd-toast-enter-active, .sd-toast-leave-active { transition: opacity 0.18s, transform 0.18s; }
.sd-toast-enter-from, .sd-toast-leave-to { opacity: 0; transform: translate(-50%, 8px); }

/* Secondary drive (зеркало) */
.sd-secondary-row {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  margin-top: 10px;
}
.sd-secondary-label {
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.1em;
  color: var(--text-dim);
}
.sd-secondary-hint {
  flex-basis: 100%;
  font-size: 11px;
  color: var(--text-dim);
}
.sd-err--warn {
  border-color: #d8a03488;
  color: light-dark(#92400e, #e5b567);
  background: rgba(216, 160, 52, 0.08);
}
.sd-uri--mirror { margin-top: 8px; }
</style>
