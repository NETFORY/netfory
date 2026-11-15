<script setup lang="ts">
/*
 * SmartNet // views/CryptoVault.vue
 *
 * Crypto Vault (ntfry) — Zero-Knowledge шифрованное облачное хранилище.
 * МУЛЬТИ-АККАУНТ: любое число дисков (2×Yandex, 2×Google, Local Safe Disk…) —
 * каждый диск = независимый сейф со своим /ntfry и своим Sled-кэшем.
 * Переключатель дисков в шапке, менеджер дисков в Settings (авто-логин
 * из API + редактируемая метка + квота каждого диска). Токены и список
 * аккаунтов хранятся в localStorage ТОЛЬКО зашифрованными мастер-ключом.
 */
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useNetStore } from '@/store/net'
import {
  isTauri,
  pickFile,
  pickFolder,
  detectOneDriveFolder,
  appListen,
  onNativeFileDrop,
  cloudVaultOpen,
  cloudVaultOpenRaid,
  cloudVaultClose,
  cloudVaultStatus,
  cloudVaultLs,
  cloudVaultPush,
  cloudVaultPull,
  cloudVaultMkdir,
  cloudVaultDelete,
  cloudVaultReset,
  cloudVaultResetRaid,
  cloudVaultRaidResync,
  cloudVaultQuota,
  cloudVaultDictStatus,
  cloudVaultDictTrain,
  cloudVaultSeal,
  cloudVaultUnseal,
  cloudVaultAccountInfo,
  cloudVaultCachedStored,
  cloudVaultScanPaths,
  cloudVaultYandexOauth,
  cloudVaultYandexRefresh,
  cloudVaultGoogleOauth,
  cloudVaultGoogleRefresh,
  type CloudVaultStatus,
  type CloudVaultRow,
  type CloudVaultProgress,
  type CloudVaultQuota,
  type CloudVaultDictStatus,
  type RaidMember,
  type VaultScanFile,
} from '@/lib/bridge'

type Provider = 'local' | 'yandex' | 'gdrive' | 'onedrive' | 'mailru'
type DriveAccount = {
  id: string
  provider: Provider
  label: string   // редактируемая метка
  login: string   // авто из API провайдера
  token: string
  refresh: string
  expiresAt: number
  raid?: boolean  // участвует ли диск в RAID (по умолчанию — да)
}

const LS_PLAIN = 'ntfry.settings'      // только не-секретное (activeId)
const LS_SEALED = 'ntfry.settings.sec' // аккаунты+токены под шифром мастер-ключа

const accounts = ref<DriveAccount[]>([])
const activeId = ref('')
/** RAID-режим: выбранные облачные диски объединены в одно хранилище. */
const raidMode = ref(false)
/** Тип RAID: stripe = объединение объёма (RAID-0), mirror = зеркало (RAID-1). */
const raidType = ref<'stripe' | 'mirror'>('stripe')
/** Квоты по id аккаунта (runtime, не персистится). */
const quotas = ref<Record<string, { total: number; used: number }>>({})
/** «Занято сейфом» по id аккаунта — сколько байт vault держит на этом диске. */
const vaultUsage = ref<Record<string, number>>({})
/** Отметки sync по id аккаунта: prev — предыдущее подключение, last — текущее.
 *  Фильтр «только новое» показывает файлы, загруженные после prev. */
const syncMarks = ref<Record<string, { prev: number; last: number }>>({})

const active = computed(() => accounts.value.find((a) => a.id === activeId.value) || null)
/** RAID-пригодные диски: облачные + Local Safe Disk (с путём). Legacy mock без пути — нет. */
const cloudAccounts = computed(() => accounts.value.filter((a) => a.provider !== 'local' || !!a.token))
/** Диски, отмеченные для участия в RAID (галка ⛓ в списке). */
const raidSelected = computed(() => cloudAccounts.value.filter((a) => a.raid !== false))
const raidEligible = computed(() => raidSelected.value.length >= 2)
const raidActive = computed(() => raidMode.value && raidEligible.value)
const raidKey = computed(() => `${raidType.value}:${raidSelected.value.map((a) => a.id).sort().join('+')}`)
const raidMembers = (): RaidMember[] =>
  raidSelected.value.map((a) => ({ provider: a.provider, token: a.token, id: a.id }))

const showSettings = ref(false)
const drivesOpen = ref(false)
const savedFlash = ref(false)

const status = ref<CloudVaultStatus | null>(null)
const quota = ref<CloudVaultQuota | null>(null)
const dictInfo = ref<CloudVaultDictStatus | null>(null)
const rows = ref<CloudVaultRow[]>([])
const cwd = ref('/')
const busy = ref(false)
const resyncBusy = ref(false)

/** Активный диск/RAID пишет свежий stats в status → обновляем «занято сейфом»
 *  сразу же, без ожидания следующего открытия dropdown. Для RAID приписываем
 *  всем членам одинаковое суммарное значение (это индикатор масштаба, а не
 *  точная раскладка чанков по членам). */
watch(
  () => status.value?.stats.storedBytes ?? 0,
  (n) => {
    if (!n) return
    if (raidActive.value) {
      for (const a of raidSelected.value) vaultUsage.value[a.id] = n
    } else if (activeId.value) {
      vaultUsage.value[activeId.value] = n
    }
  },
)
/** Подключение висит дольше 15с → чип «offline · retry?». */
const connectTimedOut = ref(false)
let connectTimer: ReturnType<typeof setTimeout> | null = null
const errorMsg = ref('')
const progress = ref<CloudVaultProgress | null>(null)
const toast = ref('')
const lastSpeed = ref<{ mbps: number; op: 'push' | 'pull' } | null>(null)
const dropActive = ref(false)
let unlisten: () => void = () => {}
let unlistenDrop: () => void = () => {}
let toastTimer: ReturnType<typeof setTimeout> | null = null

function genId(): string {
  try {
    return crypto.randomUUID().slice(0, 8)
  } catch {
    return Math.random().toString(36).slice(2, 10)
  }
}

const PROV_NAME: Record<Provider, string> = { local: 'Local', yandex: 'Yandex', gdrive: 'Google', onedrive: 'OneDrive', mailru: 'Mail.ru' }
const PROV_SHORT: Record<Provider, string> = { local: 'LO', yandex: 'YA', gdrive: 'GD', onedrive: 'OD', mailru: 'MR' }

function defaultLabel(provider: Provider, login: string): string {
  const same = accounts.value.filter((a) => a.provider === provider).length
  return login || (same > 0 ? `${PROV_NAME[provider]} ${same + 1}` : PROV_NAME[provider])
}

/* ── настройки: activeId открыто, аккаунты+токены — только под шифром ─── */
async function loadSettings() {
  try {
    let legacy: any = null
    const plain = localStorage.getItem(LS_PLAIN)
    if (plain) {
      const p = JSON.parse(plain)
      if (p.activeId) activeId.value = p.activeId
      if (p.raid) raidMode.value = true
      if (p.raidType === 'mirror') raidType.value = 'mirror'
      if (p.syncMarks) syncMarks.value = p.syncMarks
      // старый plaintext-формат (provider+tokens) — мигрируем под шифр
      if (p.yandexToken || p.gdriveToken || p.provider) legacy = p
    }
    if (isTauri) {
      const sealed = localStorage.getItem(LS_SEALED)
      if (sealed) {
        const sec = JSON.parse(await cloudVaultUnseal(sealed))
        if (Array.isArray(sec.accounts)) {
          accounts.value = sec.accounts
        } else if (sec.yandexToken || sec.gdriveToken) {
          legacy = { ...legacy, ...sec } // старый sealed-формат одиночного диска
        }
      }
    }
    if (!accounts.value.length && legacy) {
      migrateLegacy(legacy)
      await persistSettings()
    }
    if (!active.value && accounts.value.length) activeId.value = accounts.value[0].id
  } catch { /* повреждённые/недешифруемые данные — остаёмся на дефолтах */ }
}

/** Одиночные настройки прошлых версий → массив аккаунтов. */
function migrateLegacy(p: any) {
  if (p.yandexToken) {
    accounts.value.push({
      id: genId(), provider: 'yandex', label: 'Yandex', login: '',
      token: p.yandexToken, refresh: p.yandexRefresh || '', expiresAt: p.yandexExpiresAt || 0,
    })
  }
  if (p.gdriveToken) {
    accounts.value.push({
      id: genId(), provider: 'gdrive', label: 'Google', login: '',
      token: p.gdriveToken, refresh: p.gdriveRefresh || '', expiresAt: p.gdriveExpiresAt || 0,
    })
  }
  if (p.provider === 'local') {
    accounts.value.push({ id: genId(), provider: 'local', label: 'Local mock', login: '', token: '', refresh: '', expiresAt: 0 })
  }
  const prefer = accounts.value.find((a) => a.provider === p.provider)
  activeId.value = (prefer || accounts.value[0])?.id || ''
}

async function persistSettings() {
  localStorage.setItem(
    LS_PLAIN,
    JSON.stringify({ v: 2, activeId: activeId.value, raid: raidMode.value, raidType: raidType.value, syncMarks: syncMarks.value }),
  )
  if (isTauri) {
    try {
      localStorage.setItem(LS_SEALED, await cloudVaultSeal(JSON.stringify({ accounts: accounts.value })))
    } catch { /* кошелёк заблокирован — аккаунты не сохраняем */ }
  }
}

onMounted(async () => {
  await loadSettings()
  unlisten = await appListen<CloudVaultProgress>('vault-progress', (p) => {
    progress.value = p
    if (p.done >= p.total) setTimeout(() => { if (progress.value === p) progress.value = null }, 1200)
  })
  // нативный OS drag&drop: папки/файлы → очередь загрузки в текущий каталог
  unlistenDrop = await onNativeFileDrop((p) => {
    if (!status.value) return
    if (p.type === 'enter' || p.type === 'over') dropActive.value = true
    else if (p.type === 'leave') dropActive.value = false
    else if (p.type === 'drop') {
      dropActive.value = false
      if (p.paths?.length) void enqueuePaths(p.paths)
    }
  })
  // периодическая проверка здоровья RAID-набора (в offline облака недоступны)
  healthTimer = setInterval(() => {
    if (useNetStore().online) void checkRaidHealth()
  }, 60_000)
  if (isTauri && (active.value || raidActive.value)) await connect()
})
onUnmounted(() => {
  unlisten()
  unlistenDrop()
  if (healthTimer) clearInterval(healthTimer)
})

/* ── авто-рефреш OAuth-токена аккаунта (за 60с до истечения) ──────────── */
async function ensureFreshToken(acc: DriveAccount) {
  const soon = Date.now() + 60_000
  if (!acc.refresh || !acc.expiresAt || acc.expiresAt >= soon) return
  try {
    const r = acc.provider === 'yandex'
      ? await cloudVaultYandexRefresh(acc.refresh)
      : acc.provider === 'gdrive'
        ? await cloudVaultGoogleRefresh(acc.refresh)
        : null
    if (r) {
      acc.token = r.accessToken
      if (r.refreshToken) acc.refresh = r.refreshToken
      acc.expiresAt = Date.now() + r.expiresIn * 1000
      await persistSettings()
    }
  } catch { /* refresh не удался — пробуем текущим токеном */ }
}

/* ── здоровье RAID-набора (доступность каждого члена) ─────────────────── */
const memberHealth = ref<Record<string, boolean>>({})
let healthTimer: ReturnType<typeof setInterval> | null = null

/** Пингует все диски набора (account_info) и обновляет их квоты. */
async function checkRaidHealth() {
  if (!isTauri || !raidActive.value) return
  await Promise.all(
    raidSelected.value.map(async (a) => {
      try {
        const i = await cloudVaultAccountInfo(a.provider, a.token)
        quotas.value[a.id] = { total: i.totalBytes, used: i.usedBytes }
        memberHealth.value[a.id] = true
      } catch {
        memberHealth.value[a.id] = false
      }
    }),
  )
}

const raidHealth = computed(() => {
  if (!raidActive.value) return null
  const total = raidSelected.value.length
  if (!raidSelected.value.some((a) => a.id in memberHealth.value)) return null
  const ok = raidSelected.value.filter((a) => memberHealth.value[a.id]).length
  return { ok, total }
})

const healthTitle = computed(() => {
  const down = raidSelected.value.filter((a) => memberHealth.value[a.id] === false)
  return down.length
    ? `Недоступны: ${down.map((a) => a.label).join(', ')} — ${raidType.value === 'mirror' ? 'зеркало деградировало' : 'их чанки недоступны'}`
    : 'Все диски набора доступны'
})

/** RAID-1: докатка недостающих чанков на доступные диски зеркала. */
async function resyncMirror() {
  if (resyncBusy.value || !isTauri) return
  resyncBusy.value = true
  errorMsg.value = ''
  try {
    const r = await cloudVaultRaidResync()
    showToast(
      r.copied || r.failed || r.lost
        ? `Re-sync: ${r.copied} восстановлено · ${r.failed} ошибок · ${r.lost} потеряно`
        : `Re-sync: зеркало консистентно (${r.checked} чанков) ✓`,
    )
    await checkRaidHealth()
  } catch (e) {
    errorMsg.value = String(e)
  } finally {
    resyncBusy.value = false
  }
}

/* ── 15-секундный таймаут подключения ─────────────────────────────────── */
function armConnectTimeout() {
  connectTimedOut.value = false
  if (connectTimer) clearTimeout(connectTimer)
  connectTimer = setTimeout(() => {
    if (busy.value && !status.value) connectTimedOut.value = true
  }, 15_000)
}

function disarmConnectTimeout() {
  if (connectTimer) {
    clearTimeout(connectTimer)
    connectTimer = null
  }
  if (status.value) connectTimedOut.value = false
}

async function retryConnect() {
  connectTimedOut.value = false
  await reconnect()
}

async function afterConnect() {
  await refresh()
  try { quota.value = await cloudVaultQuota() } catch { quota.value = null }
  try { dictInfo.value = await cloudVaultDictStatus() } catch { dictInfo.value = null }
  if (raidActive.value) {
    void checkRaidHealth() // в RAID общая квота — сумма; подиск-квоты и здоровье отдельно
  } else {
    const a = active.value
    if (a && quota.value) quotas.value[a.id] = { total: quota.value.totalBytes, used: quota.value.usedBytes }
  }
}

async function connect() {
  errorMsg.value = ''
  // RAID: объединяем все облачные диски в один vault
  if (raidActive.value) {
    busy.value = true
    armConnectTimeout()
    try {
      const existing = await cloudVaultStatus()
      if (existing && existing.provider === 'raid' && existing.account === raidKey.value) {
        status.value = existing
        await afterConnect()
        return
      }
      for (const a of raidSelected.value) await ensureFreshToken(a)
      status.value = await cloudVaultOpenRaid(raidMembers(), raidType.value)
      await afterConnect()
    } catch (e) {
      status.value = null
      errorMsg.value = String(e)
    } finally {
      busy.value = false
      disarmConnectTimeout()
    }
    return
  }
  const acc = active.value
  if (!acc) {
    showSettings.value = true
    return
  }
  busy.value = true
  armConnectTimeout()
  try {
    // сессия могла пережить навигацию по страницам — переиспользуем её
    const existing = await cloudVaultStatus()
    if (existing && existing.provider === acc.provider && existing.account === acc.id) {
      status.value = existing
      await afterConnect()
      return
    }
    await ensureFreshToken(acc)
    // мнемоника не передаётся: Rust берёт секрет кошелька (SESSION_SEED)
    status.value = await cloudVaultOpen(acc.provider, '', acc.token, acc.id)
    markSynced(acc.id)
    await afterConnect()
  } catch (e) {
    status.value = null
    errorMsg.value = String(e)
  } finally {
    busy.value = false
    disarmConnectTimeout()
  }
}

/* ── RAID-тумблер / тип / выбор членов ────────────────────────────────── */
async function toggleRaid() {
  if (!raidEligible.value && !raidMode.value) return
  raidMode.value = !raidMode.value
  await persistSettings()
  await reconnect()
}

async function setRaidType(t: 'stripe' | 'mirror') {
  if (raidType.value === t) return
  raidType.value = t
  await persistSettings()
  if (raidActive.value) await reconnect()
}

/** Галка ⛓: включить/исключить диск из RAID-набора. */
async function toggleRaidMember(a: DriveAccount) {
  a.raid = a.raid === false
  await persistSettings()
  if (raidMode.value) await reconnect()
}

/* ── переключение дисков ──────────────────────────────────────────────── */
async function switchDrive(id: string) {
  drivesOpen.value = false
  const raidWasOn = raidMode.value
  raidMode.value = false // явный выбор одного диска выключает RAID
  if (id === activeId.value && status.value && !raidWasOn) return
  activeId.value = id
  await persistSettings()
  await reconnect()
}

async function reconnect() {
  if (!isTauri) return
  busy.value = true // сразу помечаем «loading», чтобы чип не мигал «offline»
  armConnectTimeout()
  await cloudVaultClose()
  status.value = null
  rows.value = []
  quota.value = null
  dictInfo.value = null
  lastSpeed.value = null
  cwd.value = '/'
  clearRecentFilter()
  await connect()
}

/** Отметка sync: prev — предыдущее подключение диска, last — текущее. */
function markSynced(id: string) {
  const m = syncMarks.value[id]
  syncMarks.value[id] = { prev: m?.last ?? 0, last: Date.now() }
  void persistSettings()
}

/* ── фильтр «только новое после последнего sync» (клик по чипу ⛁) ─────── */
type RecentRow = { path: string; name: string; size: number; modified: number }
const recentFilter = ref(false)
const recentSince = ref(0)
const recentRows = ref<RecentRow[]>([])
const recentLoading = ref(false)

/** Клик по чипу «⛁ … in vault»: переключиться на диск + показать только
 *  файлы, загруженные после последнего sync этого диска. */
async function jumpToVaultUsage(id: string) {
  drivesOpen.value = false
  const since = syncMarks.value[id]?.prev ?? syncMarks.value[id]?.last ?? 0
  await switchDrive(id)
  if (!status.value) return // подключение не удалось — ошибка уже на экране
  recentSince.value = raidActive.value ? 0 : (syncMarks.value[activeId.value]?.prev ?? since)
  recentFilter.value = true
  await loadRecentRows()
}

/** Рекурсивный обход VFS (in-memory индекс — мгновенно, без сети):
 *  плоский список файлов новее recentSince, свежие сверху. */
async function loadRecentRows() {
  recentLoading.value = true
  const out: RecentRow[] = []
  const dirs = ['/']
  while (dirs.length) {
    const d = dirs.shift()!
    let ls: CloudVaultRow[] = []
    try { ls = await cloudVaultLs(d) } catch { continue }
    for (const r of ls) {
      const p = (d === '/' ? '' : d) + '/' + r.name
      if (r.isDir) dirs.push(p)
      else if (r.modified * 1000 > recentSince.value) out.push({ path: p, name: r.name, size: r.size, modified: r.modified })
    }
  }
  out.sort((a, b) => b.modified - a.modified)
  recentRows.value = out
  recentLoading.value = false
}

function clearRecentFilter() {
  recentFilter.value = false
  recentRows.value = []
}

/** Клик по пути в фильтре — открыть папку файла (фильтр сбрасывается). */
async function openRecentDir(r: RecentRow) {
  clearRecentFilter()
  const slash = r.path.lastIndexOf('/')
  cwd.value = slash > 0 ? r.path.slice(0, slash) : '/'
  await refresh()
}

async function downloadRecent(r: RecentRow) {
  errorMsg.value = ''
  try {
    const ok = await cloudVaultPull(r.path)
    if (ok) showToast(`Decrypted & saved · ${r.name} ✓`)
  } catch (e) {
    errorMsg.value = String(e)
  }
}

async function removeRecent(r: RecentRow) {
  if (armDelete.value !== r.path) {
    armDelete.value = r.path
    setTimeout(() => { if (armDelete.value === r.path) armDelete.value = '' }, 4000)
    return
  }
  armDelete.value = ''
  busy.value = true
  try {
    await cloudVaultDelete(r.path)
    showToast(`Deleted ${r.name} — orphan chunks GC'd ✓`)
    status.value = (await cloudVaultStatus()) ?? status.value
    recentRows.value = recentRows.value.filter((x) => x.path !== r.path)
    await refresh()
  } catch (e) {
    if (isPartialDelete(e)) {
      showToast(`⚠ ${r.name} удалён, но не все чанки убраны из облака — GC доделается позже`)
      status.value = (await cloudVaultStatus()) ?? status.value
      recentRows.value = recentRows.value.filter((x) => x.path !== r.path)
      await refresh()
    } else {
      errorMsg.value = String(e)
    }
  } finally {
    busy.value = false
  }
}

/** Квоты всех подключённых дисков (для списка) — параллельно, без блокировки UI. */
async function refreshAllQuotas() {
  if (!isTauri) return
  await Promise.all(
    accounts.value
      .filter((a) => a.provider !== 'local' || !!a.token)
      .map(async (a) => {
        try {
          const i = await cloudVaultAccountInfo(a.provider, a.token)
          quotas.value[a.id] = { total: i.totalBytes, used: i.usedBytes }
          if (i.login && !a.login) {
            a.login = i.login
            await persistSettings()
          }
        } catch { /* просроченный токен и т.п. — квоту не показываем */ }
      }),
  )
  // «Занято сейфом» тянем отдельно и для локальных без токена — там кэш может
  // уже существовать по id, даже если провайдер сейчас не смонтирован.
  await Promise.all(
    accounts.value.map(async (a) => {
      try {
        vaultUsage.value[a.id] = await cloudVaultCachedStored(a.provider, a.id)
      } catch { /* нет кэша — просто не показываем */ }
    }),
  )
}

function toggleDrives() {
  drivesOpen.value = !drivesOpen.value
  if (drivesOpen.value) void refreshAllQuotas()
}

async function refresh() {
  try { rows.value = await cloudVaultLs(cwd.value) } catch { rows.value = [] }
}

function showToast(msg: string) {
  toast.value = msg
  if (toastTimer) clearTimeout(toastTimer)
  toastTimer = setTimeout(() => { toast.value = '' }, 3200)
}

/* ── upload / download с замером скорости ─────────────────────────────── */
async function upload() {
  const src = await pickFile()
  if (!src) return
  errorMsg.value = ''
  busy.value = true
  const t0 = performance.now()
  try {
    const r = await cloudVaultPush(src, cwd.value)
    const secs = Math.max(0.001, (performance.now() - t0) / 1000)
    lastSpeed.value = { mbps: r.size / 1048576 / secs, op: 'push' }
    if (status.value) status.value = { ...status.value, files: status.value.files + 1, stats: r.stats }
    await refresh()
    const savedPct = r.size > 0 ? Math.max(0, Math.round((1 - r.compressedBytes / r.size) * 100)) : 0
    showToast(
      r.deduped === r.chunks && r.chunks > 0
        ? 'Encrypted & pushed · 100% dedup — 0 B uploaded ✓'
        : `Encrypted & pushed · ${fmtBytes(r.size)} → ${fmtBytes(r.compressedBytes)} (${savedPct}% saved, ${r.deduped} dedup) ✓`,
    )
  } catch (e) {
    errorMsg.value = String(e)
  } finally {
    busy.value = false
  }
}

async function download(r: CloudVaultRow) {
  if (r.isDir) return
  errorMsg.value = ''
  const t0 = performance.now()
  try {
    const path = (cwd.value === '/' ? '' : cwd.value) + '/' + r.name
    const ok = await cloudVaultPull(path)
    if (ok) {
      const secs = Math.max(0.001, (performance.now() - t0) / 1000)
      lastSpeed.value = { mbps: r.size / 1048576 / secs, op: 'pull' }
      showToast(`Decrypted & saved · ${r.name} ✓`)
    }
  } catch (e) {
    errorMsg.value = String(e)
  }
}

/* ── очередь загрузки (drag&drop папок/файлов) ────────────────────────── */
type QueueItem = VaultScanFile & { dstDir: string }
const queue = ref<QueueItem[]>([])
const queueRunning = ref(false)
const queueTotal = ref(0)
const queueDone = ref(0)
const queueFailed = ref<string[]>([])
const queueBytesTotal = ref(0)
const queueBytesDone = ref(0)
const currentRel = ref('')
let queueCancel = false

/** Скан брошенных путей → mkdir пустых папок → постановка файлов в очередь. */
async function enqueuePaths(paths: string[]) {
  errorMsg.value = ''
  try {
    const scan = await cloudVaultScanPaths(paths)
    const basePrefix = cwd.value === '/' ? '' : cwd.value
    for (const d of scan.emptyDirs) {
      try { await cloudVaultMkdir(`${basePrefix}/${d}`) } catch { /* уже есть */ }
    }
    if (!scan.files.length && scan.emptyDirs.length) {
      await refresh()
      showToast(`Created ${scan.emptyDirs.length} folder(s) ✓`)
      return
    }
    for (const f of scan.files) {
      const slash = f.rel.lastIndexOf('/')
      const dstDir = slash > 0 ? `${basePrefix}/${f.rel.slice(0, slash)}` : basePrefix || '/'
      queue.value.push({ ...f, dstDir })
      queueBytesTotal.value += f.size
    }
    queueTotal.value += scan.files.length
    if (!queueRunning.value) void runQueue()
  } catch (e) {
    errorMsg.value = String(e)
  }
}

/** Последовательный прогон очереди (каждый файл — до 6 параллельных чанков). */
async function runQueue() {
  queueRunning.value = true
  queueCancel = false
  busy.value = true
  const t0 = performance.now()
  const bytesAtStart = queueBytesDone.value
  while (queue.value.length && !queueCancel) {
    const item = queue.value.shift()!
    currentRel.value = item.rel
    try {
      const r = await cloudVaultPush(item.src, item.dstDir)
      queueDone.value += 1
      queueBytesDone.value += item.size
      if (status.value) status.value = { ...status.value, files: status.value.files + 1, stats: r.stats }
    } catch (e) {
      queueFailed.value.push(`${item.rel}: ${String(e)}`)
    }
  }
  const secs = Math.max(0.001, (performance.now() - t0) / 1000)
  const pushed = queueBytesDone.value - bytesAtStart
  if (pushed > 0) lastSpeed.value = { mbps: pushed / 1048576 / secs, op: 'push' }
  currentRel.value = ''
  queueRunning.value = false
  busy.value = false
  await refresh()
  showToast(
    queueCancel
      ? `Queue cancelled · ${queueDone.value}/${queueTotal.value} uploaded`
      : queueFailed.value.length
        ? `Done with errors · ${queueDone.value} ok, ${queueFailed.value.length} failed`
        : `Encrypted & uploaded ${queueDone.value} file(s) ✓`,
  )
  if (!queue.value.length) {
    setTimeout(() => {
      if (!queueRunning.value && !queue.value.length) {
        queueTotal.value = 0
        queueDone.value = 0
        queueBytesTotal.value = 0
        queueBytesDone.value = 0
        queueFailed.value = []
      }
    }, 6000)
  }
}

function cancelQueue() {
  queueCancel = true
  queue.value = []
}

const queuePct = computed(() =>
  queueBytesTotal.value > 0 ? Math.min(100, Math.round((queueBytesDone.value / queueBytesTotal.value) * 100)) : 0,
)

/* ── навигация / mkdir / delete ───────────────────────────────────────── */
const navBusy = ref(false)
async function openRow(r: CloudVaultRow) {
  if (!r.isDir || navBusy.value) return
  navBusy.value = true
  cwd.value = (cwd.value === '/' ? '' : cwd.value) + '/' + r.name
  try { await refresh() } finally { navBusy.value = false }
}

async function goCrumb(path: string) {
  if (navBusy.value) return
  navBusy.value = true
  cwd.value = path
  try { await refresh() } finally { navBusy.value = false }
}

const newFolderMode = ref(false)
const newFolderName = ref('')
async function createFolder() {
  const name = newFolderName.value.trim().replace(/[/\\]/g, '-')
  if (!name) return
  try {
    await cloudVaultMkdir((cwd.value === '/' ? '' : cwd.value) + '/' + name)
    newFolderMode.value = false
    newFolderName.value = ''
    await refresh()
  } catch (e) {
    errorMsg.value = String(e)
  }
}

const armDelete = ref('')
function rowPath(r: CloudVaultRow) {
  return (cwd.value === '/' ? '' : cwd.value) + '/' + r.name
}

/** Частичный успех: файл УЖЕ удалён из индекса, но GC чанков в облаке не
 *  завершился (Rust: VaultError::GcFailed). Откатывать оптимистичное
 *  удаление нельзя — файла в сейфе больше нет; показываем предупреждение. */
function isPartialDelete(e: unknown): boolean {
  return String(e).includes('GC чанков в облаке не завершён')
}

async function removeRow(r: CloudVaultRow) {
  const path = rowPath(r)
  if (armDelete.value !== path) {
    armDelete.value = path
    setTimeout(() => { if (armDelete.value === path) armDelete.value = '' }, 4000)
    return
  }
  armDelete.value = ''
  // оптимистичное удаление: строка исчезает сразу, GC чанков идёт в фоне
  const prevRows = rows.value
  rows.value = rows.value.filter((x) => x !== r)
  try {
    const n = await cloudVaultDelete(path)
    showToast(`Deleted ${r.name} (${n} files) — orphan chunks GC'd ✓`)
    status.value = (await cloudVaultStatus()) ?? status.value
    await refresh()
  } catch (e) {
    if (isPartialDelete(e)) {
      // файл удалён — оставляем строку убранной, но честно предупреждаем о чанках
      showToast(`⚠ ${r.name} удалён, но не все чанки убраны из облака — GC доделается позже`)
      status.value = (await cloudVaultStatus()) ?? status.value
      await refresh()
    } else {
      errorMsg.value = String(e)
      rows.value = prevRows
      await refresh()
    }
  }
}

/* ── добавление дисков: OAuth / local / токен вручную ─────────────────── */
const oauthBusy = ref(false)

/** Добавить аккаунт (дедуп по provider+login: повторный OAuth обновляет токены). */
async function addAccount(provider: Provider, token: string, refresh: string, expiresIn: number) {
  let login = ''
  if (provider !== 'local') {
    try { login = (await cloudVaultAccountInfo(provider, token)).login } catch { /* без логина */ }
  }
  const dup = accounts.value.find((a) => a.provider === provider && login && a.login === login)
  if (dup) {
    dup.token = token
    if (refresh) dup.refresh = refresh
    dup.expiresAt = expiresIn ? Date.now() + expiresIn * 1000 : 0
    activeId.value = dup.id
    await persistSettings()
    showToast(`${PROV_NAME[provider]} · ${login}: токены обновлены ✓`)
  } else {
    const acc: DriveAccount = {
      id: genId(), provider, label: defaultLabel(provider, login), login,
      token, refresh, expiresAt: expiresIn ? Date.now() + expiresIn * 1000 : 0,
    }
    accounts.value.push(acc)
    activeId.value = acc.id
    await persistSettings()
    showToast(`Drive added: ${acc.label} ✓`)
  }
  await reconnect()
}

async function signInYandex() {
  errorMsg.value = ''
  oauthBusy.value = true
  try {
    const r = await cloudVaultYandexOauth()
    await addAccount('yandex', r.accessToken, r.refreshToken || '', r.expiresIn || 0)
  } catch (e) {
    errorMsg.value = String(e)
  } finally {
    oauthBusy.value = false
  }
}

async function signInGoogle() {
  errorMsg.value = ''
  oauthBusy.value = true
  try {
    const r = await cloudVaultGoogleOauth()
    await addAccount('gdrive', r.accessToken, r.refreshToken || '', r.expiresIn || 0)
  } catch (e) {
    errorMsg.value = String(e)
  } finally {
    oauthBusy.value = false
  }
}

/** OneDrive: десктоп-клиент Microsoft уже синхронизирует локальную папку.
 *  Сначала пытаемся авто-определить стандартную (`~/OneDrive`,
 *  `%OneDrive%`, `~/OneDrive - <tenant>`) — тогда добавляем её одним
 *  кликом; если не нашли, открываем диалог выбора папки. Внутри появится
 *  /ntfry с зашифрованными чанками, OneDrive сам поднимет их в облако. */
async function addOneDrive() {
  errorMsg.value = ''
  let dir = await detectOneDriveFolder()
  if (!dir) {
    dir = await pickFolder()
    if (!dir) return
  }
  const dup = accounts.value.find((a) => a.provider === 'onedrive' && a.token === dir)
  if (dup) {
    await switchDrive(dup.id)
    return
  }
  const name = dir.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || 'OneDrive'
  const acc: DriveAccount = {
    id: genId(), provider: 'onedrive', label: `OneDrive · ${name}`, login: dir,
    token: dir, refresh: '', expiresAt: 0,
  }
  accounts.value.push(acc)
  activeId.value = acc.id
  await persistSettings()
  showToast(`OneDrive folder: ${name} ✓`)
  await reconnect()
}

/** Local Safe Disk: выбранная папка = зашифрованный виртуальный диск (<папка>/ntfry). */
async function addLocal() {
  errorMsg.value = ''
  const dir = await pickFolder()
  if (!dir) return
  const dup = accounts.value.find((a) => a.provider === 'local' && a.token === dir)
  if (dup) {
    await switchDrive(dup.id)
    return
  }
  const name = dir.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || 'Local Safe Disk'
  const acc: DriveAccount = {
    id: genId(), provider: 'local', label: `Local · ${name}`, login: dir,
    token: dir, refresh: '', expiresAt: 0,
  }
  accounts.value.push(acc)
  activeId.value = acc.id
  await persistSettings()
  showToast(`Local Safe Disk: ${name} ✓`)
  await reconnect()
}

/** Облако Mail.ru: официальный WebDAV, OAuth у облака нет. Нужен «пароль для
 *  внешнего приложения» (account.mail.ru → Безопасность) и платный тариф
 *  Mail Space — WebDAV Mail.ru открыт только платным подписчикам. Креды
 *  валидируются PROPFIND-запросом ДО добавления аккаунта. */
const mailruOpen = ref(false)
const mailruEmail = ref('')
const mailruPass = ref('')

async function addMailru() {
  errorMsg.value = ''
  const email = mailruEmail.value.trim()
  const pass = mailruPass.value.trim()
  if (!email || !pass) return
  oauthBusy.value = true
  try {
    const creds = `${email}:${pass}`
    await cloudVaultAccountInfo('mailru', creds) // 401/403 → бросит до добавления
    await addAccount('mailru', creds, '', 0)
    mailruEmail.value = ''
    mailruPass.value = ''
    mailruOpen.value = false
  } catch (e) {
    errorMsg.value = String(e)
  } finally {
    oauthBusy.value = false
  }
}

const manualProvider = ref<Provider>('yandex')
const manualToken = ref('')
async function addManual() {
  const t = manualToken.value.trim()
  if (!t) return
  errorMsg.value = ''
  oauthBusy.value = true
  try {
    await addAccount(manualProvider.value, t, '', 0)
    manualToken.value = ''
  } catch (e) {
    errorMsg.value = String(e)
  } finally {
    oauthBusy.value = false
  }
}

/* ── удаление аккаунта из списка (двухшаговое) ────────────────────────── */
const armRemove = ref('')
async function removeAccount(id: string) {
  if (armRemove.value !== id) {
    armRemove.value = id
    setTimeout(() => { if (armRemove.value === id) armRemove.value = '' }, 4000)
    return
  }
  armRemove.value = ''
  const wasActive = id === activeId.value
  accounts.value = accounts.value.filter((a) => a.id !== id)
  delete quotas.value[id]
  if (wasActive) activeId.value = accounts.value[0]?.id || ''
  await persistSettings()
  if (wasActive) {
    if (active.value) await reconnect()
    else {
      await cloudVaultClose()
      status.value = null
      rows.value = []
      quota.value = null
    }
  }
  showToast('Drive removed (файлы в облаке не тронуты)')
}

async function saveSettings() {
  await persistSettings()
  savedFlash.value = true
  setTimeout(() => { savedFlash.value = false }, 1600)
  showSettings.value = false
  if (isTauri && !status.value && active.value) await connect()
}

function openSettings() {
  showSettings.value = true
  void refreshAllQuotas()
}

/* ── ZSTD-словарь: обучение COVER на пользовательской папке ───────────── */
const dictBusy = ref(false)
async function trainDict() {
  errorMsg.value = ''
  dictBusy.value = true
  try {
    const r = await cloudVaultDictTrain()
    if (r) {
      showToast(`ZSTD dictionary trained: ${r.samples} samples → ${fmtBytes(r.size)} ✓`)
      dictInfo.value = await cloudVaultDictStatus()
    }
  } catch (e) {
    errorMsg.value = String(e)
  } finally {
    dictBusy.value = false
  }
}

/* ── reset при «чужом кошельке» ───────────────────────────────────────── */
const keyMismatch = computed(() => errorMsg.value.includes('другим ключом') || errorMsg.value.includes('другому кошельку'))
const resetArmed = ref(false)
async function resetVault() {
  if (!resetArmed.value) {
    resetArmed.value = true
    setTimeout(() => { resetArmed.value = false }, 5000)
    return
  }
  resetArmed.value = false
  errorMsg.value = ''
  busy.value = true
  try {
    if (raidActive.value) {
      status.value = await cloudVaultResetRaid(raidMembers(), raidType.value)
    } else {
      const acc = active.value
      if (!acc) return
      status.value = await cloudVaultReset(acc.provider, acc.token, acc.id)
    }
    await afterConnect()
    showToast('Cloud vault reset & re-initialized for this wallet ✓')
  } catch (e) {
    errorMsg.value = String(e)
  } finally {
    busy.value = false
  }
}

/* ── computed ─────────────────────────────────────────────────────────── */
const crumbs = computed(() => {
  const parts = cwd.value.split('/').filter(Boolean)
  return [{ label: 'vault:', path: '/' }, ...parts.map((p, i) => ({ label: p, path: '/' + parts.slice(0, i + 1).join('/') }))]
})

const savings = computed(() => {
  const s = status.value?.stats
  if (!s || s.rawBytes <= 0) return null
  const saved = Math.max(0, s.rawBytes - s.storedBytes)
  return {
    raw: s.rawBytes,
    stored: s.storedBytes,
    saved,
    dedup: s.dedupSavedBytes,
    pct: Math.round((saved / s.rawBytes) * 100),
    barPct: Math.min(100, Math.round((s.storedBytes / s.rawBytes) * 100)),
  }
})

const quotaText = computed(() => {
  if (!quota.value || quota.value.totalBytes <= 0) return ''
  const free = Math.max(0, quota.value.totalBytes - quota.value.usedBytes)
  return `${fmtBytes(free)} free of ${fmtBytes(quota.value.totalBytes)}`
})

function quotaTextFor(id: string): string {
  const q = quotas.value[id]
  if (!q || q.total <= 0) return '—'
  return `${fmtBytes(Math.max(0, q.total - q.used))} free`
}

/** «Занято сейфом» на конкретном диске — пусто, если хинт ещё не подъехал
 *  или диск никогда не был подключён. */
function vaultUsageTextFor(id: string): string {
  const n = vaultUsage.value[id]
  if (!n || n <= 0) return ''
  return `⛁ ${fmtBytes(n)} in vault`
}

const activeTitle = computed(() => {
  if (raidActive.value) return `⛓ RAID-${raidType.value === 'mirror' ? '1' : '0'} · ${raidSelected.value.length} drives`
  const a = active.value
  if (!a) return 'NO DRIVE'
  return a.label || PROV_NAME[a.provider]
})

const progressPct = computed(() =>
  progress.value && progress.value.total > 0
    ? Math.min(100, Math.round((progress.value.done / progress.value.total) * 100))
    : 0,
)

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(2)} MB`
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`
}
</script>

<template>
  <div class="vault" data-testid="crypto-vault-view">
    <header class="v-head">
      <div>
        <h1 class="v-title">// CRYPTO VAULT_</h1>
        <p class="v-sub">
          Zero-Knowledge encrypted container on top of your personal cloud drives.
          Files are chunked (512 KB default, .env CHUNK_SIZE), ZSTD-compressed and ChaCha20-Poly1305 encrypted
          <b>before</b> they ever leave this device. The cloud only sees ciphertext.
        </p>
      </div>
      <div class="v-actions">
        <!-- Здоровье RAID-набора: все ли диски доступны -->
        <button
          v-if="raidActive && raidHealth"
          class="v-chip v-chip--health"
          :data-degraded="raidHealth.ok < raidHealth.total"
          :title="healthTitle + ' · клик — перепроверить'"
          data-testid="vault-raid-health-chip"
          @click="checkRaidHealth"
        >
          ⛨ {{ raidType === 'mirror' ? 'MIRROR' : 'SET' }} {{ raidHealth.ok }}/{{ raidHealth.total }}
        </button>
        <!-- Re-sync: только для ДЕГРАДИРОВАННОГО зеркала (при OK не показываем) -->
        <button
          v-if="raidActive && raidType === 'mirror' && raidHealth && raidHealth.ok < raidHealth.total"
          class="v-chip v-chip--resync"
          :disabled="resyncBusy"
          title="Докатать недостающие чанки на доступные диски зеркала"
          data-testid="vault-raid-resync-btn"
          @click="resyncMirror"
        >
          ⟲ {{ resyncBusy ? 'RE-SYNC…' : 'RE-SYNC' }}
        </button>
        <span v-if="quota && quotaText" class="v-chip v-chip--quota" data-testid="vault-quota-chip">
          ⛁ {{ raidActive ? 'TOTAL · ' : '' }}{{ quotaText }}
        </span>
        <!-- В RAID дополнительно — квота выбранного (активного) диска -->
        <span
          v-if="raidActive && active && quotas[active.id]"
          class="v-chip v-chip--quota"
          :title="`Свободное место на диске «${active.label}»`"
          data-testid="vault-active-drive-quota-chip"
        >
          ⛁ {{ active.label }} · {{ quotaTextFor(active.id) }}
        </span>

        <!-- Подключение зависло (>15с) — предлагаем повторить -->
        <button
          v-if="!status && connectTimedOut && (active || raidActive)"
          class="v-chip v-chip--retry"
          data-testid="vault-connect-retry-btn"
          @click="retryConnect"
        >
          ⟳ retry
        </button>

        <!-- Переключатель дисков (мульти-аккаунт) -->
        <div class="v-drive-wrap">
          <button
            class="v-chip v-chip--drive"
            :data-ok="!!status"
            :data-loading="!status && busy && !!active && !connectTimedOut"
            data-testid="vault-drive-switcher"
            @click="toggleDrives"
          >
            <span class="v-dot" />
            {{ status
                ? activeTitle
                : (active
                    ? activeTitle + (busy && !connectTimedOut ? ' · loading…' : connectTimedOut ? ' · offline · retry?' : ' · offline')
                    : 'NO DRIVE') }}
            <span class="v-caret">▾</span>
          </button>
          <div v-if="drivesOpen" class="v-drives-pop" data-testid="vault-drives-dropdown">
            <button
              v-if="raidEligible || raidMode"
              class="v-drive-row v-drive-row--raid"
              :class="{ 'v-drive-row--active': raidActive }"
              data-testid="vault-raid-toggle-row"
              @click="drivesOpen = false; toggleRaid()"
            >
              <span class="v-prov" data-prov="raid">⛓</span>
              <span class="v-drive-main">
                <span class="v-drive-label">RAID-{{ raidType === 'mirror' ? '1 · зеркало' : '0 · общий объём' }}</span>
                <span class="v-drive-login">{{ raidSelected.length }} из {{ cloudAccounts.length }} дисков в наборе</span>
              </span>
              <span class="v-drive-quota">{{ raidActive ? 'ON' : 'OFF' }}</span>
              <span v-if="raidActive" class="v-drive-check">●</span>
            </button>
            <button
              v-for="a in accounts"
              :key="a.id"
              class="v-drive-row"
              :class="{ 'v-drive-row--active': !raidActive && a.id === activeId }"
              :data-testid="`vault-drive-option-${a.id}`"
              @click="switchDrive(a.id)"
            >
              <span class="v-prov" :data-prov="a.provider">{{ PROV_SHORT[a.provider] }}</span>
              <span class="v-drive-main">
                <span class="v-drive-label">{{ a.label }}</span>
                <span class="v-drive-login">{{ a.login || (a.provider === 'local' ? 'testing sandbox' : '—') }}</span>
              </span>
              <span class="v-drive-quota">
                <span class="v-drive-quota-free">{{ quotaTextFor(a.id) }}</span>
                <span
                  v-if="vaultUsageTextFor(a.id)"
                  role="button"
                  class="v-drive-quota-used v-drive-quota-used--btn"
                  :title="`Занято сейфом на диске «${a.label}» — клик: только новое после последнего sync`"
                  :data-testid="`vault-drive-used-${a.id}`"
                  @click.stop="jumpToVaultUsage(a.id)"
                >{{ vaultUsageTextFor(a.id) }}</span>
              </span>
              <span v-if="!raidActive && a.id === activeId" class="v-drive-check">●</span>
            </button>
            <div v-if="!accounts.length" class="v-drives-empty">Нет подключённых дисков</div>
            <button class="v-drive-manage" data-testid="vault-manage-drives-btn" @click="drivesOpen = false; openSettings()">
              ＋ Manage drives…
            </button>
          </div>
        </div>

        <button class="v-btn" data-testid="vault-settings-btn" @click="openSettings">⚙ Settings</button>
      </div>
    </header>

    <div v-if="!isTauri" class="v-banner" data-testid="vault-web-banner">
      Crypto Vault is powered by the ntfry Rust core — available in the SmartNet desktop client.
    </div>

    <!-- Панель экономии + скорость -->
    <section v-if="savings" class="v-savings" data-testid="vault-savings-panel">
      <div class="v-savings-head">
        <span class="v-savings-title">[ ZERO-KNOWLEDGE SAVINGS ]</span>
        <span class="v-savings-pct" data-testid="vault-savings-pct">−{{ savings.pct }}%</span>
      </div>
      <div class="v-savings-bar" :title="`stored ${fmtBytes(savings.stored)} of raw ${fmtBytes(savings.raw)}`">
        <span class="v-savings-fill" :style="{ width: savings.barPct + '%' }" />
      </div>
      <div class="v-savings-chips">
        <span class="v-schip" data-testid="vault-savings-raw">RAW · <b>{{ fmtBytes(savings.raw) }}</b></span>
        <span class="v-schip" data-testid="vault-savings-stored">IN CLOUD · <b>{{ fmtBytes(savings.stored) }}</b></span>
        <span class="v-schip v-schip--good" data-testid="vault-savings-saved">SAVED · <b>{{ fmtBytes(savings.saved) }}</b></span>
        <span class="v-schip" data-testid="vault-savings-dedup">DEDUP · <b>{{ fmtBytes(savings.dedup) }}</b></span>
        <span v-if="lastSpeed" class="v-schip v-schip--speed" data-testid="vault-speed-chip">
          {{ lastSpeed.op === 'push' ? '⇈' : '⇊' }} SPEED · <b>{{ lastSpeed.mbps.toFixed(1) }} MB/s</b>
        </span>
      </div>
    </section>

    <!-- Панель инструментов VFS -->
    <div class="v-toolbar">
      <nav class="v-crumbs" data-testid="vault-breadcrumbs">
        <button v-for="(c, i) in crumbs" :key="c.path" class="v-crumb" @click="goCrumb(c.path)">
          {{ c.label }}<span v-if="i < crumbs.length - 1" class="v-sep">/</span>
        </button>
      </nav>
      <div class="v-tools">
        <button v-if="isTauri && !status" class="v-btn v-btn--accent" :disabled="busy" data-testid="vault-connect-btn" @click="connect">
          {{ busy ? '…' : '⚿ Connect' }}
        </button>
        <button class="v-btn" :disabled="!status || busy" data-testid="vault-new-folder-btn" @click="newFolderMode = !newFolderMode">＋ Folder</button>
        <button class="v-btn" :disabled="!status || busy" data-testid="vault-refresh-btn" @click="refresh">↻</button>
        <button class="v-btn v-btn--accent" :disabled="!status || busy || !isTauri" data-testid="vault-upload-btn" @click="upload">
          ↑ Upload
        </button>
      </div>
    </div>

    <div v-if="newFolderMode" class="v-newfolder" data-testid="vault-new-folder-row">
      <input
        v-model="newFolderName"
        class="v-input"
        placeholder="folder name…"
        data-testid="vault-new-folder-input"
        @keyup.enter="createFolder"
      />
      <button class="v-btn v-btn--accent" data-testid="vault-new-folder-create-btn" @click="createFolder">Create</button>
      <button class="v-btn v-btn--dim" @click="newFolderMode = false">✕</button>
    </div>

    <!-- Очередь загрузки (drag&drop) -->
    <section v-if="queueTotal > 0" class="v-queue" data-testid="vault-upload-queue">
      <div class="v-queue-head">
        <span class="v-queue-title">[ UPLOAD QUEUE ]</span>
        <span class="v-queue-num" data-testid="vault-queue-counter">
          {{ queueDone }}/{{ queueTotal }} files · {{ fmtBytes(queueBytesDone) }} / {{ fmtBytes(queueBytesTotal) }}
        </span>
        <button v-if="queueRunning" class="v-btn v-btn--sm v-btn--danger" data-testid="vault-queue-cancel-btn" @click="cancelQueue">✕ cancel</button>
      </div>
      <div class="v-progress-track">
        <span class="v-progress-fill" :style="{ width: queuePct + '%' }" />
      </div>
      <div v-if="currentRel" class="v-queue-current">⇈ {{ currentRel }}</div>
      <div v-if="queueFailed.length" class="v-queue-failed" data-testid="vault-queue-failed">
        ⚠ {{ queueFailed.length }} failed: {{ queueFailed[queueFailed.length - 1] }}
      </div>
    </section>

    <!-- Прогресс шифрования/загрузки чанков -->
    <div v-if="progress" class="v-progress" data-testid="vault-progress-bar">
      <div class="v-progress-info">
        <span class="v-progress-label">
          {{ progress.op === 'push' ? '⇈ encrypt & upload' : '⇊ download & decrypt' }} · {{ progress.file }}
        </span>
        <span class="v-progress-num">{{ fmtBytes(progress.done) }} / {{ fmtBytes(progress.total) }} · {{ progressPct }}%</span>
      </div>
      <div class="v-progress-track">
        <span class="v-progress-fill" :style="{ width: progressPct + '%' }" />
      </div>
    </div>

    <p v-if="errorMsg" class="v-error" data-testid="vault-error">{{ errorMsg }}</p>

    <div v-if="keyMismatch" class="v-mismatch" data-testid="vault-key-mismatch">
      <p class="v-mismatch-text">
        The cloud <code>/ntfry</code> container was encrypted with a different wallet.
        Either unlock SmartNet with that wallet — or wipe the remote container and
        start fresh with the current one.
      </p>
      <button class="v-btn v-btn--danger" :disabled="busy" data-testid="vault-reset-btn" @click="resetVault">
        {{ resetArmed ? '⚠ Click again to ERASE remote vault' : 'Reset cloud vault (erase /ntfry)' }}
      </button>
    </div>

    <!-- Фильтр «только новое после последнего sync» (клик по чипу ⛁) -->
    <div v-if="recentFilter" class="v-filter-bar" data-testid="vault-recent-filter-bar">
      <span class="v-filter-text">
        ⛁ NEW · загружено после последнего sync
        <b>{{ recentSince ? new Date(recentSince).toLocaleString() : '— первое подключение, показано всё' }}</b>
        · {{ recentLoading ? '…' : `${recentRows.length} файл(ов)` }}
      </span>
      <button class="v-filter-clear" data-testid="vault-recent-filter-clear" @click="clearRecentFilter">
        ✕ показать все
      </button>
    </div>

    <!-- Виртуальная файловая система -->
    <section class="v-fs" data-testid="vault-file-list">
      <div class="v-fs-head">
        <span>NAME</span><span>SIZE</span><span>MODIFIED</span><span></span><span></span>
      </div>
      <template v-if="recentFilter">
        <div v-for="r in recentRows" :key="r.path" class="v-row" :data-testid="`vault-recent-row-${r.path}`">
          <button class="v-name v-name--path" :title="`Открыть папку файла ${r.path}`" @click="openRecentDir(r)">
            {{ r.path }}
          </button>
          <span>{{ fmtBytes(r.size) }}</span>
          <span>{{ r.modified ? new Date(r.modified * 1000).toLocaleString() : '—' }}</span>
          <button class="v-dl" :title="`Decrypt & save ${r.name}`" :data-testid="`vault-recent-download-${r.path}`" @click="downloadRecent(r)">↓</button>
          <button
            class="v-del"
            :class="{ 'v-del--armed': armDelete === r.path }"
            :data-testid="`vault-recent-delete-${r.path}`"
            @click="removeRecent(r)"
          >
            {{ armDelete === r.path ? 'sure?' : '✕' }}
          </button>
        </div>
        <div v-if="!recentRows.length && !recentLoading" class="v-empty" data-testid="vault-recent-empty">
          <p class="v-empty-glyph">⛁</p>
          <p>Ничего нового после последнего sync — всё уже было в сейфе.</p>
        </div>
      </template>
      <template v-else-if="rows.length">
        <div v-for="r in rows" :key="r.name" class="v-row" :data-testid="`vault-row-${r.name}`">
          <button class="v-name" :class="{ 'v-name--dir': r.isDir }" @click="openRow(r)">
            {{ r.isDir ? '▸ ' + r.name + '/' : r.name }}
          </button>
          <span>{{ r.isDir ? '—' : fmtBytes(r.size) }}</span>
          <span>{{ r.modified ? new Date(r.modified * 1000).toLocaleString() : '—' }}</span>
          <button v-if="!r.isDir" class="v-dl" :title="`Decrypt & save ${r.name}`" :data-testid="`vault-download-${r.name}`" @click="download(r)">↓</button>
          <span v-else />
          <button
            class="v-del"
            :class="{ 'v-del--armed': armDelete === rowPath(r) }"
            :data-testid="`vault-delete-${r.name}`"
            @click="removeRow(r)"
          >
            {{ armDelete === rowPath(r) ? 'sure?' : '✕' }}
          </button>
        </div>
      </template>
      <div v-else class="v-empty" data-testid="vault-empty-state">
        <p class="v-empty-glyph">⛃</p>
        <p v-if="!isTauri">
          Open the desktop client to unlock the encrypted virtual file system.
        </p>
        <p v-else-if="!accounts.length">
          No drives connected yet. Open <b>⚙ Settings</b> and add your first cloud drive —
          you can connect several Yandex and Google accounts side by side.
        </p>
        <p v-else-if="!status">
          Vault is not connected. Press <b>Connect</b> — the encryption key is derived
          from your wallet phrase automatically.
        </p>
        <p v-else>
          This folder is empty. Press <b>↑ Upload</b> or just <b>drag & drop files
          and folders here</b> — the structure is recreated and every file is
          chunked, compressed and encrypted locally before reaching the cloud.
        </p>
      </div>
    </section>

    <!-- Оверлей drag&drop -->
    <div v-if="dropActive && status" class="v-dropzone" data-testid="vault-dropzone-overlay">
      <div class="v-dropzone-inner">
        <p class="v-dropzone-glyph">⇣</p>
        <p class="v-dropzone-text">Drop to encrypt & upload → <b>{{ cwd }}</b></p>
        <p class="v-dropzone-sub">Папки создадутся автоматически, файлы встанут в очередь</p>
      </div>
    </div>

    <!-- Модальное окно настроек -->
    <div v-if="showSettings" class="v-modal-back" data-testid="vault-settings-modal" @click.self="showSettings = false">
      <div class="v-modal">
        <h2 class="v-modal-title">[ DRIVES & KEYS ]</h2>
        <p class="v-modal-note">
          Keys never leave this device. The master encryption key (m/999'/0'/0'/0)
          is derived <b>automatically from your SmartNet wallet phrase</b>. Every drive
          below is an independent encrypted vault; OAuth tokens are stored sealed with
          that key — Zero-Knowledge all the way down.
        </p>

        <div class="v-keysource" data-testid="vault-key-source">
          <span class="v-keysource-dot" />
          KEY SOURCE · SmartNet wallet (seamless)
        </div>

        <label class="v-label">RAID mode</label>
        <div class="v-raid" :class="{ 'v-raid--on': raidActive }">
          <div class="v-raid-info">
            <span class="v-raid-title">⛓ Объединить выбранные диски в одно хранилище</span>
            <span class="v-raid-sub">
              {{ raidEligible
                ? `В наборе ${raidSelected.length} из ${cloudAccounts.length} дисков — отметьте ⛓ у дисков ниже`
                : 'Отметьте ⛓ минимум у 2 дисков ниже (облачных или локальных)' }}
            </span>
          </div>
          <button
            class="v-switch"
            :class="{ 'v-switch--on': raidMode }"
            :disabled="!raidEligible && !raidMode"
            data-testid="vault-raid-toggle"
            @click="toggleRaid"
          >
            <span class="v-switch-knob" />
          </button>
        </div>
        <div class="v-raid-types">
          <button
            class="v-raid-type"
            :class="{ 'v-raid-type--on': raidType === 'stripe' }"
            data-testid="vault-raid-type-stripe"
            @click="setRaidType('stripe')"
          >
            <span class="v-raid-type-name">RAID-0 · общий объём</span>
            <span class="v-raid-type-sub">Чанки распределяются по дискам, квота суммируется. Потеря диска = потеря его чанков</span>
          </button>
          <button
            class="v-raid-type"
            :class="{ 'v-raid-type--on': raidType === 'mirror' }"
            data-testid="vault-raid-type-mirror"
            @click="setRaidType('mirror')"
          >
            <span class="v-raid-type-name">RAID-1 · зеркало</span>
            <span class="v-raid-type-sub">Каждый чанк на всех дисках набора. Сейф переживает потерю любого диска</span>
          </button>
        </div>

        <label class="v-label">Connected drives ({{ accounts.length }})</label>
        <div v-if="!accounts.length" class="v-hint">Нет подключённых дисков — добавьте ниже (можно несколько аккаунтов одного провайдера).</div>
        <div
          v-for="a in accounts"
          :key="a.id"
          class="v-acc"
          :class="{ 'v-acc--active': a.id === activeId }"
          :data-testid="`vault-account-${a.id}`"
        >
          <span class="v-prov" :data-prov="a.provider">{{ PROV_SHORT[a.provider] }}</span>
          <div class="v-acc-main">
            <input
              v-model="a.label"
              class="v-acc-label"
              :data-testid="`vault-account-label-${a.id}`"
              title="Метка диска (редактируемая)"
              @change="persistSettings"
            />
            <span class="v-acc-login">{{ a.login || (a.provider === 'local' ? 'testing sandbox' : 'логин неизвестен') }}</span>
          </div>
          <span class="v-acc-quota" :data-testid="`vault-account-quota-${a.id}`">{{ quotaTextFor(a.id) }}</span>
          <button
            v-if="a.provider !== 'local' || !!a.token"
            class="v-raid-chip"
            :class="{ 'v-raid-chip--on': a.raid !== false }"
            :title="a.raid !== false ? 'В RAID-наборе — кликните чтобы исключить' : 'Не в RAID — кликните чтобы включить'"
            :data-testid="`vault-account-raid-${a.id}`"
            @click="toggleRaidMember(a)"
          >⛓</button>
          <span v-else class="v-raid-chip v-raid-chip--void" />
          <button
            v-if="a.id !== activeId"
            class="v-btn v-btn--sm"
            :data-testid="`vault-account-use-${a.id}`"
            @click="switchDrive(a.id)"
          >use</button>
          <span v-else class="v-acc-current">ACTIVE</span>
          <button
            class="v-del"
            :class="{ 'v-del--armed': armRemove === a.id }"
            :data-testid="`vault-account-remove-${a.id}`"
            @click="removeAccount(a.id)"
          >
            {{ armRemove === a.id ? 'sure?' : '✕' }}
          </button>
        </div>

        <label class="v-label">Add drive</label>
        <div class="v-acc-add">
          <button class="v-btn v-btn--ya" :disabled="oauthBusy || !isTauri" data-testid="vault-yandex-oauth-btn" @click="signInYandex">
            {{ oauthBusy ? 'waiting…' : '＋ Yandex Disk' }}
          </button>
          <button class="v-btn v-btn--g" :disabled="oauthBusy || !isTauri" data-testid="vault-google-oauth-btn" @click="signInGoogle">
            {{ oauthBusy ? 'waiting…' : '＋ Google Drive' }}
          </button>
          <button class="v-btn v-btn--od" :disabled="oauthBusy || !isTauri" data-testid="vault-onedrive-add-btn" @click="addOneDrive">
            ＋ OneDrive
          </button>
          <button
            class="v-btn v-btn--mr"
            :class="{ 'v-btn--mr-open': mailruOpen }"
            :disabled="oauthBusy || !isTauri"
            data-testid="vault-mailru-add-btn"
            @click="mailruOpen = !mailruOpen"
          >
            ＋ Mail.ru Облако
          </button>
          <button class="v-btn" :disabled="oauthBusy || !isTauri" data-testid="vault-add-local-btn" @click="addLocal">＋ Local Safe Disk</button>
        </div>
        <div v-if="mailruOpen" class="v-token-row v-mailru-form" data-testid="vault-mailru-form">
          <input
            v-model="mailruEmail"
            class="v-input"
            type="email"
            placeholder="email@mail.ru"
            data-testid="vault-mailru-email-input"
          />
          <input
            v-model="mailruPass"
            class="v-input"
            type="password"
            placeholder="пароль для внешнего приложения"
            data-testid="vault-mailru-pass-input"
          />
          <button
            class="v-btn v-btn--mr"
            :disabled="oauthBusy || !mailruEmail.trim() || !mailruPass.trim()"
            data-testid="vault-mailru-confirm-btn"
            @click="addMailru"
          >{{ oauthBusy ? 'checking…' : 'Add' }}</button>
        </div>
        <p v-if="mailruOpen" class="v-hint" data-testid="vault-mailru-hint">
          Mail.ru: у Облака нет OAuth/API — подключение по официальному WebDAV.
          Создайте <b>пароль для внешнего приложения</b> (account.mail.ru →
          Безопасность → Пароли для внешних приложений, доступ «Полный доступ
          к Облаку») и вводите его вместо обычного пароля. ⚠ WebDAV Mail.ru
          работает <b>только на платных тарифах</b> (подписка Mail Space).
        </p>
        <p class="v-hint">
          Yandex: окно согласия открывается прямо в клиенте, токен ловится автоматически.
          Google: откроется системный браузер (Google запрещает webview), токен придёт на
          локальный loopback. Повторный вход в тот же аккаунт просто обновит токены.
          OneDrive: никакого API — клиент сам найдёт стандартную папку OneDrive
          (<code>~/OneDrive</code>, <code>%OneDrive%</code>) и добавит её одним
          кликом; иначе откроется диалог выбора. Десктоп-клиент Microsoft сам
          синхронизирует папку с облаком; внутри появится <code>/ntfry</code>
          с зашифрованными чанками.
          Local Safe Disk: выберите папку на локальном диске — в ней появится
          <b>/ntfry</b> с зашифрованными чанками, ровно как на облаке; такие диски
          можно объединять с облачными в RAID.
        </p>

        <label class="v-label">Add by token (advanced)</label>
        <div class="v-token-row">
          <select v-model="manualProvider" class="v-input v-input--prov" data-testid="vault-manual-provider-select">
            <option value="yandex">Yandex</option>
            <option value="gdrive">Google</option>
            <option value="mailru">Mail.ru</option>
          </select>
          <input
            v-model="manualToken"
            class="v-input"
            type="password"
            :placeholder="manualProvider === 'mailru' ? 'email:пароль_приложения' : 'OAuth token (y0_… / ya29.…)'"
            data-testid="vault-manual-token-input"
          />
          <button class="v-btn" :disabled="oauthBusy || !isTauri || !manualToken.trim()" data-testid="vault-manual-add-btn" @click="addManual">Add</button>
        </div>

        <!-- ZSTD-словарь -->
        <template v-if="status">
          <label class="v-label">ZSTD compression dictionary (active drive)</label>
          <div class="v-token-row">
            <span class="v-dict-status" data-testid="vault-dict-status">
              {{ dictInfo?.present ? `active · ${dictInfo.filesUsingDict} files use it` : 'not set (standard ZSTD)' }}
            </span>
            <button class="v-btn" :disabled="dictBusy || !isTauri" data-testid="vault-dict-train-btn" @click="trainDict">
              {{ dictBusy ? 'training…' : '⚙ Train from folder…' }}
            </button>
          </div>
          <p class="v-hint">
            Pick a folder with ≥8 samples of your typical files — a personal COVER
            dictionary is trained locally and stored as <code>/ntfry/user.dict</code>.
            Boosts compression of small similar files.
          </p>
        </template>

        <div class="v-modal-actions">
          <button class="v-btn v-btn--dim" data-testid="vault-settings-cancel-btn" @click="showSettings = false">Cancel</button>
          <button class="v-btn v-btn--accent" data-testid="vault-save-settings-btn" @click="saveSettings">Save</button>
        </div>
      </div>
    </div>

    <transition name="fade">
      <div v-if="savedFlash" class="v-toast" data-testid="vault-saved-toast">Settings saved (tokens sealed) ✓</div>
    </transition>
    <transition name="fade">
      <div v-if="toast" class="v-toast" data-testid="vault-op-toast">{{ toast }}</div>
    </transition>
  </div>
</template>

<style scoped>
.vault { padding: 24px 32px 48px; max-width: 1100px; margin: 0 auto; }
.v-head { display: flex; justify-content: space-between; align-items: flex-start; gap: 24px; margin-bottom: 24px; }
.v-title { font-family: var(--font-mono); font-size: 22px; font-weight: var(--fw-700); letter-spacing: 0.08em; color: var(--text); margin: 0 0 8px; }
.v-sub { font-size: 13.5px; color: var(--text-dim); max-width: 560px; margin: 0; line-height: 1.6; }
.v-actions { display: flex; align-items: center; gap: 12px; flex-shrink: 0; flex-wrap: wrap; justify-content: flex-end; }

.v-chip {
  display: inline-flex; align-items: center; gap: 8px; padding: 6px 14px;
  border-radius: 999px; border: 1px solid var(--border); background: light-dark(rgba(0,0,0,0.05), rgba(0,0,0,0.28));
  font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.08em; color: var(--text-dim);
}
.v-chip--quota { color: var(--text); }
.v-chip--health { cursor: pointer; color: light-dark(#15803d, #22c55e); border-color: rgba(34,197,94,0.5); transition: border-color 0.15s ease; }
.v-chip--health[data-degraded='true'] { color: light-dark(#dc2626, #f87171); border-color: rgba(239,68,68,0.6); background: rgba(239,68,68,0.08); animation: v-health-pulse 1.6s ease-in-out infinite; }
@keyframes v-health-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.55; } }
.v-chip--resync { color: light-dark(#059669, #34d399); border-color: rgba(52,211,153,0.55); background: rgba(52,211,153,0.08); cursor: pointer; }
.v-chip--resync:hover:not(:disabled) { background: rgba(52,211,153,0.18); }
.v-chip--resync:disabled { opacity: 0.55; cursor: wait; }
.v-chip--retry { color: light-dark(#dc2626, #f87171); border-color: rgba(239,68,68,0.55); background: rgba(239,68,68,0.08); cursor: pointer; }
.v-chip--retry:hover { background: rgba(239,68,68,0.18); }
.v-dot { width: 8px; height: 8px; border-radius: 50%; background: #ef4444; box-shadow: 0 0 8px #ef4444; }
.v-chip[data-ok='true'] { color: var(--text); }
.v-chip[data-ok='true'] .v-dot { background: light-dark(#15803d, #22c55e); box-shadow: 0 0 8px light-dark(#15803d, #22c55e); }
.v-chip[data-loading='true'] { color: light-dark(#b45309, #fbbf24); }
.v-chip[data-loading='true'] .v-dot {
  background: light-dark(#b45309, #fbbf24); box-shadow: 0 0 8px light-dark(#b45309, #fbbf24);
  animation: v-dot-pulse 1s ease-in-out infinite;
}
@keyframes v-dot-pulse { 0%, 100% { opacity: 1; transform: scale(1); } 50% { opacity: 0.45; transform: scale(0.75); } }

/* ── переключатель дисков ── */
.v-drive-wrap { position: relative; }
.v-chip--drive { cursor: pointer; transition: border-color 0.15s ease; }
.v-chip--drive:hover { border-color: light-dark(#7c3aed, #a78bfa); }
.v-caret { color: var(--text-dim); font-size: 9px; }
.v-drives-pop {
  position: absolute; top: calc(100% + 8px); right: 0; z-index: 60; min-width: 340px;
  background: var(--surface, var(--panel)); border: 1px solid light-dark(#7c3aed66, light-dark(#7c3aed, #a78bfa)66); border-radius: 14px;
  box-shadow: 0 18px 60px rgba(0,0,0,0.55); overflow: hidden; padding: 6px;
}
.v-drive-row {
  display: grid; grid-template-columns: 34px 1fr auto 14px; gap: 10px; align-items: center;
  width: 100%; padding: 9px 10px; background: none; border: none; border-radius: 10px;
  cursor: pointer; text-align: left; transition: background-color 0.12s ease;
}
.v-drive-row:hover { background: rgba(167,139,250,0.08); }
.v-drive-row--active { background: rgba(167,139,250,0.1); }
.v-drive-main { display: flex; flex-direction: column; gap: 2px; overflow: hidden; }
.v-drive-label { font-family: var(--font-mono); font-size: 12.5px; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.v-drive-login { font-family: var(--font-mono); font-size: 10.5px; color: var(--text-dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.v-drive-quota { font-family: var(--font-mono); font-size: 10.5px; color: var(--text-dim); white-space: nowrap; display: flex; flex-direction: column; align-items: flex-end; gap: 2px; }
.v-drive-quota-free { color: var(--text-dim); }
.v-drive-quota-used { color: light-dark(#7c3aed, #a78bfa); font-size: 10px; letter-spacing: 0.02em; }
.v-drive-quota-used--btn {
  cursor: pointer; text-decoration: underline dotted rgba(167,139,250,0.55);
  text-underline-offset: 3px; transition: color 0.15s ease, text-decoration-color 0.15s ease;
}
.v-drive-quota-used--btn:hover { color: light-dark(#6d28d9, #c4b5fd); text-decoration-color: light-dark(#6d28d9, #c4b5fd); }
.v-drive-check { color: light-dark(#15803d, #22c55e); font-size: 10px; }
.v-drives-empty { padding: 14px; font-family: var(--font-mono); font-size: 11.5px; color: var(--text-dim); text-align: center; }
.v-drive-manage {
  display: block; width: 100%; margin-top: 4px; padding: 9px 10px; border: none;
  border-top: 1px solid var(--border); background: none; color: light-dark(#7c3aed, #a78bfa);
  font-family: var(--font-mono); font-size: 11.5px; cursor: pointer; text-align: left;
}
.v-drive-manage:hover { background: rgba(167,139,250,0.08); }

/* ── бейдж провайдера ── */
.v-prov {
  display: inline-flex; align-items: center; justify-content: center; width: 30px; height: 30px;
  border-radius: 9px; font-family: var(--font-mono); font-size: 10px; font-weight: var(--fw-700);
  letter-spacing: 0.06em; border: 1px solid var(--border); color: var(--text-dim); flex-shrink: 0;
}
.v-prov[data-prov='yandex'] { border-color: #fc3f1d88; color: light-dark(#c2410c, #fc6a4d); background: rgba(252,63,29,0.08); }
.v-prov[data-prov='gdrive'] { border-color: #4285f488; color: light-dark(#2563eb, #7aa9f7); background: rgba(66,133,244,0.08); }
.v-prov[data-prov='local'] { border-color: light-dark(#7c3aed66, light-dark(#7c3aed, #a78bfa)66); color: light-dark(#7c3aed, #a78bfa); background: rgba(167,139,250,0.08); }
.v-prov[data-prov='raid'] { border-color: light-dark(#15803d66, light-dark(#15803d, #22c55e)66); color: light-dark(#15803d, #22c55e); background: rgba(34,197,94,0.08); }
.v-drive-row--raid { border-bottom: 1px solid var(--border); border-radius: 10px 10px 0 0; }

/* ── RAID-тумблер в Settings ── */
.v-raid {
  display: flex; align-items: center; justify-content: space-between; gap: 14px;
  padding: 12px 14px; border: 1px dashed var(--border); border-radius: 12px;
  transition: border-color 0.2s ease;
}
.v-raid--on { border-color: rgba(34,197,94,0.55); background: rgba(34,197,94,0.05); }
.v-raid-info { display: flex; flex-direction: column; gap: 3px; }
.v-raid-title { font-family: var(--font-mono); font-size: 12.5px; color: var(--text); }
.v-raid-sub { font-size: 11px; color: var(--text-dim); }
.v-switch {
  position: relative; width: 44px; height: 24px; flex-shrink: 0; border-radius: 999px;
  border: 1px solid var(--border); background: light-dark(rgba(0,0,0,0.06), rgba(0,0,0,0.35)); cursor: pointer;
  transition: background-color 0.2s ease, border-color 0.2s ease; padding: 0;
}
.v-switch:disabled { opacity: 0.4; cursor: not-allowed; }
.v-switch--on { background: rgba(34,197,94,0.35); border-color: light-dark(#15803d88, light-dark(#15803d, #22c55e)88); }
.v-switch-knob {
  position: absolute; top: 2px; left: 2px; width: 18px; height: 18px; border-radius: 50%;
  background: var(--text-dim); transition: transform 0.2s ease, background-color 0.2s ease;
}
.v-switch--on .v-switch-knob { transform: translateX(20px); background: light-dark(#15803d, #22c55e); }

/* ── выбор типа RAID ── */
.v-raid-types { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; margin-top: 8px; }
.v-raid-type {
  display: flex; flex-direction: column; gap: 4px; padding: 10px 12px; text-align: left;
  border: 1px solid var(--border); border-radius: 12px; background: light-dark(rgba(0,0,0,0.04), rgba(0,0,0,0.2));
  cursor: pointer; transition: border-color 0.15s ease, background-color 0.15s ease;
}
.v-raid-type:hover { border-color: light-dark(#7c3aed88, light-dark(#7c3aed, #a78bfa)88); }
.v-raid-type--on { border-color: light-dark(#15803d88, light-dark(#15803d, #22c55e)88); background: rgba(34,197,94,0.06); }
.v-raid-type-name { font-family: var(--font-mono); font-size: 11.5px; color: var(--text); }
.v-raid-type--on .v-raid-type-name { color: light-dark(#15803d, #22c55e); }
.v-raid-type-sub { font-size: 10.5px; color: var(--text-dim); line-height: 1.45; }

/* ── галка участия диска в RAID ── */
.v-raid-chip {
  width: 26px; height: 26px; display: inline-flex; align-items: center; justify-content: center;
  border: 1px solid var(--border); border-radius: 8px; background: none; color: var(--text-dim);
  font-size: 12px; cursor: pointer; opacity: 0.55; transition: border-color 0.15s ease, opacity 0.15s ease;
}
.v-raid-chip:hover { border-color: light-dark(#15803d, #22c55e); opacity: 1; }
.v-raid-chip--on { border-color: light-dark(#15803d88, light-dark(#15803d, #22c55e)88); color: light-dark(#15803d, #22c55e); background: rgba(34,197,94,0.08); opacity: 1; }
.v-raid-chip--void { border: none; background: none; cursor: default; }

/* ── очередь загрузки ── */
.v-queue { margin-bottom: 12px; padding: 12px 16px; border: 1px solid rgba(167,139,250,0.4); border-radius: 12px; background: var(--surface); }
.v-queue-head { display: flex; align-items: center; gap: 12px; margin-bottom: 8px; }
.v-queue-title { font-family: var(--font-mono); font-size: 10.5px; letter-spacing: 0.14em; color: light-dark(#7c3aed, #a78bfa); }
.v-queue-num { flex: 1; font-family: var(--font-mono); font-size: 11.5px; color: var(--text); }
.v-queue-current { margin-top: 6px; font-family: var(--font-mono); font-size: 11px; color: var(--text-dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.v-queue-failed { margin-top: 6px; font-family: var(--font-mono); font-size: 11px; color: light-dark(#dc2626, #f87171); word-break: break-word; }

/* ── drag&drop оверлей ── */
.v-dropzone {
  position: fixed; inset: 0; z-index: 90; display: flex; align-items: center; justify-content: center;
  background: rgba(10, 8, 20, 0.72); backdrop-filter: blur(3px); pointer-events: none;
}
.v-dropzone-inner {
  padding: 44px 64px; border: 2px dashed light-dark(#7c3aed, #a78bfa); border-radius: 20px; text-align: center;
  background: rgba(167,139,250,0.06); box-shadow: 0 0 60px rgba(167,139,250,0.25);
}
.v-dropzone-glyph { font-size: 44px; color: #a78bfa; margin: 0 0 10px; }
.v-dropzone-text { font-family: var(--font-mono); font-size: 15px; color: var(--text); margin: 0 0 6px; }
.v-dropzone-text b { color: #a78bfa; }
.v-dropzone-sub { font-size: 12px; color: var(--text-dim); margin: 0; }

.v-banner { margin-bottom: 20px; padding: 12px 18px; border-radius: 12px; border: 1px dashed rgba(167,139,250,0.5); color: var(--text-dim); font-family: var(--font-mono); font-size: 12.5px; }

.v-savings { margin-bottom: 20px; padding: 16px 20px 14px; background: var(--surface); border: 1px solid var(--border); border-radius: 14px; }
.v-savings-head { display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 10px; }
.v-savings-title { font-family: var(--font-mono); font-size: 10.5px; letter-spacing: 0.14em; color: light-dark(#7c3aed, #a78bfa); }
.v-savings-pct { font-family: var(--font-mono); font-size: 18px; font-weight: var(--fw-700); color: light-dark(#15803d, #22c55e); text-shadow: 0 0 16px rgba(34,197,94,0.4); }
.v-savings-bar { height: 10px; border-radius: 999px; overflow: hidden; background: rgba(34,197,94,0.16); border: 1px solid var(--border); margin-bottom: 12px; }
.v-savings-fill { display: block; height: 100%; background: light-dark(#7c3aed, #a78bfa); transition: width 0.4s ease; }
.v-savings-chips { display: flex; flex-wrap: wrap; gap: 10px; }
.v-schip { padding: 4px 12px; border-radius: 999px; background: light-dark(rgba(0,0,0,0.05), rgba(0,0,0,0.28)); border: 1px solid var(--border); color: var(--text-dim); font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.03em; }
.v-schip b { color: var(--text); }
.v-schip--good { border-color: rgba(34,197,94,0.5); }
.v-schip--good b { color: light-dark(#15803d, #22c55e); }
.v-schip--speed { border-color: rgba(167,139,250,0.5); }
.v-schip--speed b { color: light-dark(#7c3aed, #a78bfa); }

.v-btn { padding: 8px 16px; border-radius: 10px; border: 1px solid var(--border); background: var(--surface); color: var(--text); font-family: var(--font-mono); font-size: 12.5px; cursor: pointer; transition: border-color 0.15s ease, background-color 0.15s ease; }
.v-btn:hover:not(:disabled) { border-color: light-dark(#7c3aed, #a78bfa); }
.v-btn:disabled { opacity: 0.5; cursor: not-allowed; }
.v-btn--dim { color: var(--text-dim); }
.v-btn--sm { padding: 4px 12px; font-size: 11px; }
.v-btn--accent { border-color: light-dark(#7c3aed, #a78bfa); background: rgba(167,139,250,0.12); }
.v-btn--danger { border-color: rgba(239,68,68,0.6); background: rgba(239,68,68,0.1); color: light-dark(#dc2626, #f87171); }
.v-btn--danger:hover:not(:disabled) { border-color: #ef4444; }
.v-btn--ya { border-color: #fc3f1d88; background: rgba(252,63,29,0.1); white-space: nowrap; }
.v-btn--ya:hover:not(:disabled) { border-color: #fc3f1d; }
.v-btn--g { border-color: #4285f488; background: rgba(66,133,244,0.1); white-space: nowrap; }
.v-btn--g:hover:not(:disabled) { border-color: #4285f4; }
.v-btn--od { border-color: #0078d488; background: rgba(0,120,212,0.1); white-space: nowrap; }
.v-btn--od:hover:not(:disabled) { border-color: #0078d4; }
.v-btn--mr { border-color: #005ff988; background: rgba(0,95,249,0.1); white-space: nowrap; }
.v-btn--mr:hover:not(:disabled), .v-btn--mr-open { border-color: #005ff9; }
.v-mailru-form { margin-top: 8px; }

.v-toolbar { display: flex; justify-content: space-between; align-items: center; margin-bottom: 12px; }
.v-tools { display: flex; gap: 8px; }
.v-crumbs { display: flex; align-items: center; gap: 2px; font-family: var(--font-mono); font-size: 13px; }
.v-crumb { background: none; border: none; color: light-dark(#7c3aed, #a78bfa); cursor: pointer; padding: 2px 4px; font: inherit; }
.v-crumb:hover { text-decoration: underline; }
.v-sep { color: var(--text-dim); margin-left: 4px; }

.v-newfolder { display: flex; gap: 8px; margin-bottom: 12px; }
.v-newfolder .v-input { flex: 1; max-width: 320px; }

.v-progress { margin-bottom: 12px; }
.v-progress-info { display: flex; justify-content: space-between; gap: 12px; margin-bottom: 6px; font-family: var(--font-mono); font-size: 11.5px; color: var(--text-dim); }
.v-progress-label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.v-progress-num { flex-shrink: 0; color: var(--text); }
.v-progress-track { height: 8px; border-radius: 999px; overflow: hidden; background: light-dark(rgba(0,0,0,0.06), rgba(255,255,255,0.05)); border: 1px solid var(--border); }
.v-progress-fill { display: block; height: 100%; background: linear-gradient(90deg, light-dark(#7c3aed, #a78bfa), light-dark(#15803d, #22c55e)); box-shadow: 0 0 12px rgba(167,139,250,0.5); transition: width 0.2s ease; }

.v-error { margin: 0 0 12px; padding: 10px 14px; border-radius: 10px; border: 1px solid rgba(239,68,68,0.5); color: light-dark(#dc2626, #f87171); font-family: var(--font-mono); font-size: 12px; word-break: break-word; }
.v-mismatch { margin: 0 0 12px; padding: 14px 16px; border-radius: 12px; border: 1px dashed rgba(245,158,11,0.55); background: rgba(245,158,11,0.05); }
.v-mismatch-text { font-size: 12.5px; color: var(--text-dim); line-height: 1.6; margin: 0 0 10px; }
.v-mismatch-text code { color: #f59e0b; }

/* ── фильтр «только новое после последнего sync» ── */
.v-filter-bar {
  display: flex; align-items: center; justify-content: space-between; gap: 12px;
  margin-bottom: 10px; padding: 8px 14px; border: 1px solid rgba(167,139,250,0.5);
  border-radius: 10px; background: rgba(167,139,250,0.07);
  font-family: var(--font-mono); font-size: 11.5px; color: var(--text);
}
.v-filter-text b { color: light-dark(#7c3aed, #a78bfa); }
.v-filter-clear {
  background: none; border: 1px solid var(--border); border-radius: 8px; color: var(--text-dim);
  font-family: var(--font-mono); font-size: 11px; padding: 3px 10px; cursor: pointer;
  transition: border-color 0.15s ease, color 0.15s ease; white-space: nowrap;
}
.v-filter-clear:hover { border-color: light-dark(#7c3aed, #a78bfa); color: light-dark(#7c3aed, #a78bfa); }

.v-fs { border: 1px solid var(--border); border-radius: 14px; background: var(--surface); overflow: hidden; }
.v-fs-head, .v-row { display: grid; grid-template-columns: 1fr 110px 180px 42px 46px; gap: 10px; align-items: center; padding: 10px 18px; font-family: var(--font-mono); font-size: 12.5px; text-align: left; }
.v-fs-head { color: var(--text-dim); font-size: 10.5px; letter-spacing: 0.14em; border-bottom: 1px solid var(--border); }
.v-row { border-bottom: 1px solid var(--border); color: var(--text-dim); }
.v-row:hover { background: rgba(167,139,250,0.06); }
.v-name { background: none; border: none; color: var(--text); font: inherit; text-align: left; cursor: default; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; padding: 0; }
.v-name--dir { color: light-dark(#7c3aed, #a78bfa); cursor: pointer; }
.v-name--dir:hover { text-decoration: underline; }
.v-name--path { color: var(--text); cursor: pointer; }
.v-name--path:hover { color: light-dark(#7c3aed, #a78bfa); text-decoration: underline; }
.v-dl { background: none; border: 1px solid var(--border); border-radius: 8px; color: var(--text); font: inherit; cursor: pointer; padding: 2px 10px; transition: border-color 0.15s ease; }
.v-dl:hover { border-color: light-dark(#15803d, #22c55e); color: light-dark(#15803d, #22c55e); }
.v-del { background: none; border: 1px solid var(--border); border-radius: 8px; color: var(--text-dim); font: inherit; font-size: 11px; cursor: pointer; padding: 2px 8px; transition: border-color 0.15s ease, color 0.15s ease; }
.v-del:hover { border-color: #ef4444; color: light-dark(#dc2626, #f87171); }
.v-del--armed { border-color: #ef4444; color: light-dark(#dc2626, #f87171); background: rgba(239,68,68,0.1); }

.v-empty { padding: 48px 32px; text-align: center; color: var(--text-dim); font-size: 13.5px; line-height: 1.7; }
.v-empty p { max-width: 520px; margin: 0 auto; }
.v-empty-glyph { font-size: 42px; color: light-dark(#7c3aed, #a78bfa); text-shadow: 0 0 24px rgba(167,139,250,0.5); margin-bottom: 12px !important; }

.v-modal-back { position: fixed; inset: 0; background: rgba(0,0,0,0.6); backdrop-filter: blur(4px); display: flex; align-items: center; justify-content: center; z-index: 100; }
.v-modal { width: min(600px, 92vw); max-height: 88vh; overflow-y: auto; padding: 26px 28px; border-radius: 16px; background: var(--surface, var(--panel)); border: 1px solid light-dark(#7c3aed66, light-dark(#7c3aed, #a78bfa)66); box-shadow: 0 24px 80px rgba(0,0,0,0.5); }
.v-modal-title { font-family: var(--font-mono); font-size: 13px; letter-spacing: 0.14em; color: light-dark(#7c3aed, #a78bfa); margin: 0 0 10px; }
.v-modal-note { font-size: 12.5px; color: var(--text-dim); line-height: 1.6; margin: 0 0 18px; }
.v-keysource { display: inline-flex; align-items: center; gap: 8px; padding: 8px 14px; border-radius: 10px; border: 1px solid rgba(34,197,94,0.45); background: rgba(34,197,94,0.07); font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.08em; color: var(--text); }
.v-keysource-dot { width: 8px; height: 8px; border-radius: 50%; background: light-dark(#15803d, #22c55e); box-shadow: 0 0 8px light-dark(#15803d, #22c55e); }
.v-label { display: block; font-family: var(--font-mono); font-size: 10.5px; letter-spacing: 0.12em; color: var(--text-dim); margin: 16px 0 6px; text-transform: uppercase; }
.v-input { width: 100%; padding: 10px 12px; border-radius: 10px; border: 1px solid var(--border); background: light-dark(rgba(0,0,0,0.05), rgba(0,0,0,0.3)); color: var(--text); font-family: var(--font-mono); font-size: 13px; }
.v-input:focus { outline: none; border-color: light-dark(#7c3aed, #a78bfa); }
.v-input--prov { max-width: 130px; }
.v-token-row { display: flex; gap: 8px; align-items: center; }
.v-token-row .v-input { flex: 1; }
.v-dict-status { flex: 1; font-family: var(--font-mono); font-size: 12px; color: var(--text-dim); }
.v-hint { font-size: 11px; color: var(--text-dim); line-height: 1.55; margin: 8px 0 0; }
.v-hint code { color: light-dark(#7c3aed, #a78bfa); }
.v-modal-actions { display: flex; justify-content: flex-end; gap: 10px; margin-top: 22px; }

/* ── строки аккаунтов в Settings ── */
.v-acc {
  display: grid; grid-template-columns: 34px 1fr auto auto auto auto; gap: 10px; align-items: center;
  padding: 9px 10px; border: 1px solid var(--border); border-radius: 12px; margin-bottom: 8px;
  transition: border-color 0.15s ease;
}
.v-acc--active { border-color: rgba(34,197,94,0.5); background: rgba(34,197,94,0.04); }
.v-acc-main { display: flex; flex-direction: column; gap: 2px; overflow: hidden; }
.v-acc-label {
  background: none; border: none; border-bottom: 1px dashed transparent; padding: 0;
  color: var(--text); font-family: var(--font-mono); font-size: 12.5px; width: 100%;
}
.v-acc-label:hover, .v-acc-label:focus { border-bottom-color: light-dark(#7c3aed88, light-dark(#7c3aed, #a78bfa)88); outline: none; }
.v-acc-login { font-family: var(--font-mono); font-size: 10.5px; color: var(--text-dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.v-acc-quota { font-family: var(--font-mono); font-size: 10.5px; color: var(--text-dim); white-space: nowrap; }
.v-acc-current { font-family: var(--font-mono); font-size: 9.5px; letter-spacing: 0.1em; color: light-dark(#15803d, #22c55e); }
.v-acc-add { display: flex; gap: 8px; flex-wrap: wrap; }

.v-toast { position: fixed; bottom: 28px; left: 50%; transform: translateX(-50%); padding: 10px 20px; border-radius: 10px; border: 1px solid #22c55e88; background: rgba(0, 40, 20, 0.9); color: #22c55e; font-family: var(--font-mono); font-size: 12.5px; z-index: 110; }
.fade-enter-active, .fade-leave-active { transition: opacity 0.25s ease; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
</style>
