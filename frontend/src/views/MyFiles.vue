<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { parseFileTag, type ParsedTag } from '@/lib/fileTags'
import {
  listMyFiles,
  fileDiskStats,
  deleteFile,
  deleteDownloadedFile,
  setFileSeeding,
  publishFile,
  fileShareLink,
  fileSalesCounts,
  openFileNative,
  appListen,
  fileDownloadProgress,
  notifyPrivateRecipients,
  listPrivateInbox,
  startFileDownload,
  saveFileAs,
  type FileManifest,
  type FileDiskStats,
} from '@/lib/bridge'
import { useAuthStore } from '@/store/auth'
import { useNetStore } from '@/store/net'
import { fmtBytes, fmtDateTime } from '@/lib/format'
import AddFileModal from '@/components/AddFileModal.vue'
import FileQrModal from '@/components/FileQrModal.vue'
import FileTypeIcon from '@/components/FileTypeIcon.vue'

const { t } = useI18n()
const auth = useAuthStore()
const files = ref<FileManifest[]>([])
const privateFiles = ref<FileManifest[]>([])
const tab = ref<'mine' | 'downloading' | 'downloaded' | 'private'>('mine')
const stats = ref<FileDiskStats>({
  totalBytes: 0,
  freeBytes: 0,
  usedBytes: 0,
  filesBytes: 0,
  fileCount: 0,
})
const showAdd = ref(false)
const droppedPath = ref('')
const dragActive = ref(false)
let dragUnlisten: Array<() => void> = []
const busy = ref('')
const pendingDelete = ref<FileManifest | null>(null)
const sales = ref<Record<string, number>>({})
const dlProgress = ref<Record<string, number>>({})
const expandedRecipients = ref<Record<string, boolean>>({})
const notifyBusy = ref('')
const notifyMsg = ref('')
let timer: number | undefined

async function load() {
  files.value = await listMyFiles()
  privateFiles.value = await listPrivateInbox()
  stats.value = await fileDiskStats()
  const paidCids = files.value
    .filter((f) => f.isPaid && f.owner === auth.address)
    .map((f) => f.cid)
  // сеть: подсчёт продаж идёт к блокчейн-ноде — в offline пропускаем,
  // иначе запрос виснет в таймаутах каждые 5 секунд и давит на IPC
  if (paidCids.length && useNetStore().online) {
    try {
      sales.value = await fileSalesCounts(paidCids)
    } catch {
      /* ignore */
    }
  }
  // live progress for files currently downloading in the background
  const dling = [...files.value, ...privateFiles.value].filter((x) => x.downloading)
  for (const f of dling) {
    try {
      const p = await fileDownloadProgress(f.cid)
      dlProgress.value[f.cid] =
        p.total > 0 ? Math.min(100, Math.round((p.downloaded / p.total) * 100)) : 0
    } catch {
      /* ignore */
    }
  }
}

function openAdd() {
  droppedPath.value = ''
  showAdd.value = true
}

onMounted(async () => {
  load()
  if (auth.isTauri) {
    timer = window.setInterval(load, 5000)
    // Tauri drag&drop: overlay при перетаскивании + приём файла →
    // открываем модалку публикации с уже выбранным файлом.
    dragUnlisten.push(await appListen('tauri://drag-enter', () => (dragActive.value = true)))
    dragUnlisten.push(await appListen('tauri://drag-over', () => (dragActive.value = true)))
    dragUnlisten.push(await appListen('tauri://drag-leave', () => (dragActive.value = false)))
    dragUnlisten.push(
      await appListen<{ paths: string[] }>('tauri://drag-drop', (p) => {
        dragActive.value = false
        const dropped = p?.paths?.[0]
        if (dropped) {
          droppedPath.value = dropped
          showAdd.value = true
        }
      }),
    )
    // refresh when a new private file arrives via the on-chain scanner
    dragUnlisten.push(await appListen('private-file-received', () => load()))
  }
})
onUnmounted(() => {
  if (timer) clearInterval(timer)
  dragUnlisten.forEach((u) => u())
  dragUnlisten = []
})

const usedPct = computed(() => {
  if (!stats.value.totalBytes) return 0
  return Math.min(100, (stats.value.usedBytes / stats.value.totalBytes) * 100)
})

const myFiles = computed(() => files.value.filter((f) => f.owner === auth.address))
const downloadingFiles = computed(() => files.value.filter((f) => f.downloading))
const downloadedFiles = computed(
  () => files.value.filter((f) => f.owner !== auth.address && !f.downloading),
)

// ── Категоризация «Мои файлы» (A + B из плана) ───────────────────────
// Опираемся на `FileManifest.tag` (JSON): фронт разбирает его в ParsedTag,
// раскладывает файлы по 5 категориям (all/dev/music/image/other) и
// строит иерархию «Repo → Version» ВНУТРИ вкладки Dev через
// специальные строки-заголовки. Поиск ищет и по имени, и по repo,
// чтобы «git-remote» находил все связанные бинарники.
type MineCat = 'all' | 'dev' | 'music' | 'image' | 'other'
const mineCat = ref<MineCat>('all')
const mineSearch = ref('')

interface TaggedFile { f: FileManifest; parsed: ParsedTag }
const myFilesTagged = computed<TaggedFile[]>(() =>
  myFiles.value.map((f) => ({ f, parsed: parseFileTag(f.tag) })),
)

const mineCatStats = computed(() => {
  const box: Record<MineCat, { count: number; size: number }> = {
    all: { count: 0, size: 0 },
    dev: { count: 0, size: 0 },
    music: { count: 0, size: 0 },
    image: { count: 0, size: 0 },
    other: { count: 0, size: 0 },
  }
  for (const { f, parsed } of myFilesTagged.value) {
    box.all.count++
    box.all.size += f.size
    // «private» файлы попадают в свой собственный таб верхнего уровня,
    // здесь не показываются, но если tag=private — учитываем как other.
    const c: MineCat = parsed.cat === 'private' ? 'other' : (parsed.cat as MineCat)
    if (box[c]) {
      box[c].count++
      box[c].size += f.size
    }
  }
  return box
})

/** Плоский отфильтрованный список для категорий кроме `dev`. */
const mineFilteredFlat = computed<TaggedFile[]>(() => {
  let list = myFilesTagged.value
  if (mineCat.value !== 'all') list = list.filter((x) => x.parsed.cat === mineCat.value)
  const q = mineSearch.value.trim().toLowerCase()
  if (q) {
    list = list.filter(
      (x) =>
        (x.f.name || '').toLowerCase().includes(q) ||
        (x.parsed.repo || '').toLowerCase().includes(q) ||
        (x.parsed.ver || '').toLowerCase().includes(q),
    )
  }
  return list
})

// ── Sort (D из плана) ─────────────────────────────────────────────
// Клик по заголовку колонки → сортировка. Повторный клик → инверсия
// направления. Для строковых полей (name) стартует с `asc`, для
// числовых (size/date/seeds) — с `desc`. В Dev-режиме, если пользователь
// НЕ выбрал явную сортировку (`sortKey === 'default'`), сохраняется
// иерархия «repo · vX.Y» из этапа B; при клике на любую колонку
// временно раскрывается плоский список.
type SortKey = 'default' | 'name' | 'size' | 'date' | 'seeds'
type SortDir = 'asc' | 'desc'
const sortKey = ref<SortKey>('default')
const sortDir = ref<SortDir>('desc')

function toggleSort(k: Exclude<SortKey, 'default'>) {
  if (sortKey.value === k) {
    sortDir.value = sortDir.value === 'asc' ? 'desc' : 'asc'
  } else {
    sortKey.value = k
    sortDir.value = k === 'name' ? 'asc' : 'desc'
  }
  currentPage.value = 1
}
function clearSort() {
  sortKey.value = 'default'
  sortDir.value = 'desc'
  currentPage.value = 1
}
function sortIndicator(k: Exclude<SortKey, 'default'>): string {
  if (sortKey.value !== k) return ''
  return sortDir.value === 'asc' ? '▲' : '▼'
}

function compareTagged(a: TaggedFile, b: TaggedFile): number {
  const mul = sortDir.value === 'asc' ? 1 : -1
  switch (sortKey.value) {
    case 'name':  return mul * (a.f.name || '').localeCompare(b.f.name || '')
    case 'size':  return mul * ((a.f.size || 0) - (b.f.size || 0))
    case 'date':  return mul * ((a.f.publishedAt || 0) - (b.f.publishedAt || 0))
    case 'seeds': return mul * ((a.f.seeds || 0) - (b.f.seeds || 0))
    default:      return 0
  }
}

/** Пользователь явно выбрал сортировку — временно распускаем группировку Dev. */
const userSortActive = computed(() => sortKey.value !== 'default')

// ── Pagination ────────────────────────────────────────────────────
// 10 / 25 / 50 файлов на странице. Только для «Мои файлы»; другие
// вкладки (Downloading / Downloaded / Private) сохраняют плоский вид.
type PageSize = 10 | 25 | 50
const PAGE_SIZE_KEY = 'sth_myfiles_page_size'
const pageSize = ref<PageSize>(((): PageSize => {
  const raw = Number(localStorage.getItem(PAGE_SIZE_KEY) || 25)
  return raw === 10 || raw === 25 || raw === 50 ? (raw as PageSize) : 25
})())
const currentPage = ref(1)

function setPageSize(sz: PageSize) {
  pageSize.value = sz
  currentPage.value = 1
  try { localStorage.setItem(PAGE_SIZE_KEY, String(sz)) } catch { /* noop */ }
}

// Сброс страницы при смене таба/категории/поиска — чтобы не «уехать»
// в 5-ю страницу пустоты после переключения фильтра.
watch([tab, mineCat, mineSearch], () => { currentPage.value = 1; selected.value.clear() })

// ── Bulk-выбор (C из плана) ────────────────────────────────────────
// Мульти-выбор через чекбоксы + плавающий toolbar с двумя массовыми
// операциями: «📋 Copy links» (одной кнопкой в буфер) и «🗑 Delete».
// Delete проходит по каждому файлу через уже существующие
// `deleteFile` (мои) / `deleteDownloadedFile` (чужие) — так не нужно
// новых Rust-команд, и правила seed-разделения остаются на месте.
const selected = ref(new Set<string>())

/** Список файлов на текущей странице (без строк-заголовков) — для «select all». */
const currentPageFileCids = computed<string[]>(() =>
  paginatedRows.value.filter((r) => r.kind === 'file').map((r) => (r as { f: FileManifest }).f.cid),
)
const allOnPageSelected = computed(() =>
  currentPageFileCids.value.length > 0 &&
  currentPageFileCids.value.every((cid) => selected.value.has(cid)),
)

function toggleSelect(cid: string) {
  if (selected.value.has(cid)) selected.value.delete(cid)
  else selected.value.add(cid)
  // ⚠ Vue не реагирует на мутации Set — форсим reactive update через новую ссылку.
  selected.value = new Set(selected.value)
}
function toggleAllOnPage() {
  const s = new Set(selected.value)
  if (allOnPageSelected.value) {
    for (const cid of currentPageFileCids.value) s.delete(cid)
  } else {
    for (const cid of currentPageFileCids.value) s.add(cid)
  }
  selected.value = s
}
function clearSelection() { selected.value = new Set() }

/** Скопировать sn://f/{cid} для всех выбранных файлов, по одной ссылке в строке. */
async function bulkCopyLinks() {
  const links = Array.from(selected.value).map((cid) => `sn://f/${cid}`).join('\n')
  try {
    await navigator.clipboard.writeText(links)
    notifyMsg.value = `Copied ${selected.value.size} link${selected.value.size === 1 ? '' : 's'}`
    setTimeout(() => { notifyMsg.value = '' }, 2200)
  } catch {
    notifyMsg.value = 'Copy failed'
    setTimeout(() => { notifyMsg.value = '' }, 2200)
  }
}

const bulkDeleting = ref(false)
const bulkDeleteConfirm = ref(false)
async function bulkDeleteRun() {
  if (bulkDeleting.value) return
  bulkDeleting.value = true
  const cids = Array.from(selected.value)
  let ok = 0
  let fail = 0
  for (const cid of cids) {
    const f = files.value.find((x) => x.cid === cid)
    if (!f) continue
    try {
      if (f.owner === auth.address) await deleteFile(cid)
      else await deleteDownloadedFile(cid)
      ok++
    } catch {
      fail++
    }
  }
  bulkDeleting.value = false
  bulkDeleteConfirm.value = false
  selected.value = new Set()
  await load()
  notifyMsg.value = fail === 0 ? `Deleted ${ok} file${ok === 1 ? '' : 's'}` : `Deleted ${ok}, failed ${fail}`
  setTimeout(() => { notifyMsg.value = '' }, 2500)
}

/** Строки таблицы. В Dev-вкладке вставляем «Repo · Version» заголовки. */
type MineRow =
  | { kind: 'header'; label: string; count: number; size: number }
  | { kind: 'file'; f: FileManifest; parsed: ParsedTag }

const mineRows = computed<MineRow[]>(() => {
  const items = mineFilteredFlat.value
  // (a) Если пользователь явно кликнул сортировку — плоский вид с сортировкой
  //     во всех категориях, включая Dev (иерархия Repo→Ver временно опущена).
  if (userSortActive.value) {
    return [...items].sort(compareTagged).map(({ f, parsed }) => ({ kind: 'file' as const, f, parsed }))
  }
  // (b) Все категории кроме Dev — плоский вид в естественном порядке.
  if (mineCat.value !== 'dev') {
    return items.map(({ f, parsed }) => ({ kind: 'file' as const, f, parsed }))
  }
  // (c) Dev-вкладка без явной сортировки — иерархия «repo · vX.Y» (этап B).
  const sorted = [...items].sort((a, b) => {
    const ra = a.parsed.repo || '~'
    const rb = b.parsed.repo || '~'
    if (ra !== rb) return ra.localeCompare(rb)
    const va = a.parsed.ver || ''
    const vb = b.parsed.ver || ''
    if (va !== vb) return vb.localeCompare(va, undefined, { numeric: true })
    return (a.f.name || '').localeCompare(b.f.name || '')
  })
  const groups = new Map<string, { count: number; size: number }>()
  for (const x of sorted) {
    const g = `${x.parsed.repo || '—'} · v${x.parsed.ver || '—'}`
    const cur = groups.get(g) || { count: 0, size: 0 }
    cur.count++
    cur.size += x.f.size
    groups.set(g, cur)
  }
  const rows: MineRow[] = []
  let currentGroup = ''
  for (const x of sorted) {
    const g = `${x.parsed.repo || '—'} · v${x.parsed.ver || '—'}`
    if (g !== currentGroup) {
      const meta = groups.get(g)!
      rows.push({ kind: 'header', label: g, count: meta.count, size: meta.size })
      currentGroup = g
    }
    rows.push({ kind: 'file', f: x.f, parsed: x.parsed })
  }
  return rows
})

/** Компактное форматирование байтов для бейджей на табах и группах. */
function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`
}
const shown = computed(() =>
  tab.value === 'mine'
    ? myFiles.value
    : tab.value === 'downloading'
      ? downloadingFiles.value
      : tab.value === 'private'
        ? privateFiles.value
        : downloadedFiles.value,
)

/**
 * Единая шкала строк таблицы: для 'mine' — mineRows (с групп-заголовками
 * в Dev-режиме и фильтрацией по категории/поиску), для остальных вкладок —
 * shown, обёрнутый в тот же MineRow тип, чтобы шаблон различал `kind` и
 * не дублировался. На всех вкладках применяется активная сортировка
 * (клик по колонке); группировка Dev автоматически отключается, когда
 * пользователь явно выбрал сортировку.
 */
const displayRows = computed<MineRow[]>(() => {
  if (tab.value === 'mine') return mineRows.value
  const items = shown.value.map((f) => ({ f, parsed: parseFileTag(f.tag) }))
  const sorted = userSortActive.value ? [...items].sort(compareTagged) : items
  return sorted.map(({ f, parsed }) => ({ kind: 'file' as const, f, parsed }))
})

/** Общее число файловых строк (без учёта заголовков групп) — для пагинации. */
const totalFileRows = computed(() => displayRows.value.filter((r) => r.kind === 'file').length)
const totalPages = computed(() => Math.max(1, Math.ceil(totalFileRows.value / pageSize.value)))

/**
 * Строки текущей страницы. Считаем «страницу» по числу файлов, а не строк:
 * заголовки групп идут «в довесок» и не тратят слот. Если в первый чанк
 * попадают файлы одной группы, но заголовок группы остался на предыдущей
 * странице — переносим заголовок с собой.
 */
const paginatedRows = computed<MineRow[]>(() => {
  const rows = displayRows.value
  const startIdx = (currentPage.value - 1) * pageSize.value
  const endIdx = startIdx + pageSize.value
  let fileSeen = 0
  const out: MineRow[] = []
  let pendingHeader: MineRow | null = null
  for (const r of rows) {
    if (r.kind === 'header') {
      pendingHeader = r
      continue
    }
    if (fileSeen >= startIdx && fileSeen < endIdx) {
      if (pendingHeader) {
        out.push(pendingHeader)
        pendingHeader = null
      }
      out.push(r)
    } else {
      // всё ещё до нашей страницы — сбрасываем висящий заголовок
      if (fileSeen < startIdx) pendingHeader = pendingHeader
      // после нашей страницы — заканчиваем
      if (fileSeen >= endIdx) break
    }
    fileSeen++
  }
  return out
})

/** Есть ли что показать в текущей вкладке (учитывая фильтр категории). */
const hasRows = computed(() => totalFileRows.value > 0)


/** Recipient addresses of a private file (owner excluded), from the manifest. */
function recipientsOf(f: FileManifest): string[] {
  const set = new Set<string>([...(f.recipients || []), ...Object.keys(f.wrappedKeys || {})])
  set.delete(f.owner)
  return Array.from(set)
}

/** Display title: real name for private files (when known), else generic. */
function fileTitle(f: FileManifest): string {
  if (f.encrypted) return '🔒 ' + (f.displayName || t('fx.privateFile'))
  return f.name
}

// комиссия за одну ончейн-транзакцию оповещения — 1 STH (антиспам-защита,
// совпадает с fee, передаваемым в notify_private_recipients)
const NOTIFY_FEE = 1
const notifyConfirm = ref<FileManifest | null>(null)

function askNotify(f: FileManifest) {
  notifyConfirm.value = f
}

async function notify(f: FileManifest) {
  notifyConfirm.value = null
  notifyBusy.value = f.cid
  notifyMsg.value = ''
  try {
    const n = await notifyPrivateRecipients(f.cid)
    notifyMsg.value = t('fx.notifySent', { n })
    setTimeout(() => (notifyMsg.value = ''), 5000)
    load()
  } catch (e: any) {
    pubError.value = `${f.name}: ${String(e?.message || e)}`
    setTimeout(() => (pubError.value = ''), 6000)
  } finally {
    notifyBusy.value = ''
  }
}

async function downloadPrivate(f: FileManifest) {
  busy.value = f.cid
  try {
    const updated = await startFileDownload(f.cid)
    const i = privateFiles.value.findIndex((x) => x.cid === f.cid)
    if (i >= 0) privateFiles.value[i] = updated
  } catch (e: any) {
    pubError.value = `${f.name}: ${String(e?.message || e)}`
    setTimeout(() => (pubError.value = ''), 6000)
  } finally {
    busy.value = ''
  }
}

async function savePrivate(f: FileManifest) {
  try {
    await saveFileAs(f.cid)
  } catch (e: any) {
    pubError.value = `${f.name}: ${String(e?.message || e)}`
    setTimeout(() => (pubError.value = ''), 4000)
  }
}

// «Сохранить как…»: открывает системный диалог, приватные файлы расшифровываются
// на бэкенде (save_file_as → decrypt_stored) и сохраняются в открытом виде.
const ctxMenu = ref<{ x: number; y: number; file: FileManifest } | null>(null)

function openCtx(e: MouseEvent, f: FileManifest) {
  ctxMenu.value = { x: e.clientX, y: e.clientY, file: f }
}
function closeCtx() {
  ctxMenu.value = null
}
async function saveAs(f: FileManifest) {
  closeCtx()
  try {
    await saveFileAs(f.cid)
  } catch (e: any) {
    pubError.value = `${f.name}: ${String(e?.message || e)}`
    setTimeout(() => (pubError.value = ''), 4000)
  }
}

async function onAdded(m: FileManifest) {
  // reactive insert so the new file shows instantly
  if (!files.value.find((f) => f.cid === m.cid)) files.value.unshift(m)
  load()
}

const pubError = ref('')

const copied = ref('')
const qrFile = ref<FileManifest | null>(null)
async function copyLink(f: FileManifest) {
  try {
    await navigator.clipboard.writeText(fileShareLink(f))
    copied.value = f.cid
    setTimeout(() => {
      if (copied.value === f.cid) copied.value = ''
    }, 1600)
  } catch {
    /* clipboard unavailable */
  }
}

async function publish(f: FileManifest) {
  busy.value = f.cid
  pubError.value = ''
  try {
    const updated = await publishFile(f.cid)
    const i = files.value.findIndex((x) => x.cid === f.cid)
    if (i >= 0) files.value[i] = updated
  } catch (e: any) {
    pubError.value = `${f.name}: ${String(e?.message || e)}`
    setTimeout(() => (pubError.value = ''), 6000)
  } finally {
    busy.value = ''
  }
}

async function toggleSeed(f: FileManifest) {
  busy.value = f.cid
  try {
    const updated = await setFileSeeding(f.cid, !f.seeding)
    const i = files.value.findIndex((x) => x.cid === f.cid)
    if (i >= 0) files.value[i] = updated
  } catch (e) {
    /* noop */
  } finally {
    busy.value = ''
  }
}

async function remove(f: FileManifest) {
  // native confirm() is blocked inside the Tauri webview → in-app modal
  pendingDelete.value = f
}

async function openNative(f: FileManifest) {
  try {
    await openFileNative(f.cid)
  } catch (e: any) {
    pubError.value = `${f.name}: ${String(e?.message || e)}`
    setTimeout(() => (pubError.value = ''), 4000)
  }
}

async function confirmDelete() {
  const f = pendingDelete.value
  if (!f) return
  pendingDelete.value = null
  busy.value = f.cid
  try {
    if (f.owner === auth.address) await deleteFile(f.cid)
    else await deleteDownloadedFile(f.cid)
    files.value = files.value.filter((x) => x.cid !== f.cid)
    load()
  } catch (e) {
    /* noop */
  } finally {
    busy.value = ''
  }
}
</script>

<template>
  <div class="mf" data-testid="my-files">
    <header class="mf-head">
      <div>
        <h1>{{ t('fx.myTitle') }}</h1>
        <p class="sub">{{ t('fx.mySub') }}</p>
      </div>
      <button class="add-btn" @click="openAdd" data-testid="my-files-add">
        {{ t('fx.addFile') }}
      </button>
    </header>

    <div class="stats" data-testid="my-files-stats">
      <div class="stat">
        <span class="stat-val">{{ stats.fileCount }}</span>
        <span class="stat-label">{{ t('fx.statFiles') }}</span>
      </div>
      <div class="stat">
        <span class="stat-val">{{ fmtBytes(stats.filesBytes) }}</span>
        <span class="stat-label">{{ t('fx.statInExchange') }}</span>
      </div>
      <div class="stat wide">
        <div class="disk-row">
          <span class="stat-label">{{ t('fx.disk', { used: fmtBytes(stats.usedBytes), total: fmtBytes(stats.totalBytes) }) }}</span>
          <span class="stat-label">{{ t('fx.diskFree', { free: fmtBytes(stats.freeBytes) }) }}</span>
        </div>
        <div class="bar"><div class="bar-fill" :style="{ width: usedPct + '%' }"></div></div>
      </div>
    </div>

    <div v-if="pubError" class="pub-error" data-testid="my-files-pub-error">{{ pubError }}</div>

    <div v-if="!auth.isTauri" class="notice" data-testid="my-files-web-notice">
      {{ t('fx.webNotice') }}
    </div>

    <template v-else>
      <div class="tabs" data-testid="my-files-tabs">
        <button :class="{ on: tab === 'mine' }" @click="tab = 'mine'" data-testid="my-files-tab-mine">
          {{ t('fx.tabMine') }} <span class="cnt">{{ myFiles.length }}</span>
        </button>
        <button
          :class="{ on: tab === 'downloading' }"
          @click="tab = 'downloading'"
          data-testid="my-files-tab-downloading"
        >
          Downloading <span class="cnt">{{ downloadingFiles.length }}</span>
        </button>
        <button
          :class="{ on: tab === 'downloaded' }"
          @click="tab = 'downloaded'"
          data-testid="my-files-tab-downloaded"
        >
          {{ t('fx.tabDownloaded') }} <span class="cnt">{{ downloadedFiles.length }}</span>
        </button>
        <button
          :class="{ on: tab === 'private' }"
          @click="tab = 'private'"
          data-testid="my-files-tab-private"
        >
          🔒 {{ t('fx.tabPrivate') }} <span class="cnt">{{ privateFiles.length }}</span>
        </button>
      </div>

      <div v-if="notifyMsg" class="notify-msg" data-testid="my-files-notify-msg">{{ notifyMsg }}</div>

      <!-- ── Sub-tabs категорий + поиск (только для «Мои файлы») ──
           A + B из плана. Категория выводится из FileManifest.tag,
           количество и общий вес показаны прямо на бейдже — так сразу
           понятно, где сидят гигабайты. Внутри Dev-вкладки строки
           файлов группируются заголовками «repo · vX.Y» (см. mineRows). -->
      <div v-if="tab === 'mine'" class="mine-cats" data-testid="my-files-cats">
        <button
          v-for="c in ['all', 'dev', 'music', 'image', 'other'] as const"
          :key="c"
          class="mc-pill"
          :class="{ on: mineCat === c, [`mc-${c}`]: true }"
          :data-testid="`my-files-cat-${c}`"
          @click="mineCat = c"
        >
          <span class="mc-lbl">{{
            c === 'all' ? 'All'
            : c === 'dev' ? 'Dev releases'
            : c === 'music' ? 'Music'
            : c === 'image' ? 'Images'
            : 'Other'
          }}</span>
          <span class="mc-cnt">{{ mineCatStats[c].count }}</span>
          <span v-if="mineCatStats[c].size > 0" class="mc-sz dim">· {{ fmtBytes(mineCatStats[c].size) }}</span>
        </button>
        <input
          v-model="mineSearch"
          class="mc-search"
          :placeholder="'Search by name, repo, version…'"
          data-testid="my-files-search"
        />
      </div>

      <div v-if="!hasRows" class="empty" data-testid="my-files-empty">
        <span class="empty-ico">⌸</span>
        <p v-if="tab === 'mine'">{{ t('fx.emptyMine') }}</p>
        <p v-else-if="tab === 'downloading'">No active downloads</p>
        <p v-else-if="tab === 'private'">{{ t('fx.emptyPrivate') }}</p>
        <p v-else>{{ t('fx.emptyDownloaded') }}</p>
        <button v-if="tab === 'mine'" class="add-btn ghost" @click="openAdd">
          {{ t('fx.addFirst') }}
        </button>
      </div>

      <table v-else class="files" data-testid="my-files-table">
        <thead>
          <tr>
            <!-- ☐ Bulk-select — чекбокс «выделить всё на текущей странице». -->
            <th class="th-check">
              <input
                type="checkbox"
                :checked="allOnPageSelected"
                @change="toggleAllOnPage"
                :title="allOnPageSelected ? 'Deselect page' : 'Select page'"
                data-testid="mf-select-all"
              />
            </th>
            <th class="th-sort" :class="{ active: sortKey === 'name' }" @click="toggleSort('name')" data-testid="mf-sort-name">
              {{ t('fx.colName') }} <span class="th-sort-ind">{{ sortIndicator('name') }}</span>
            </th>
            <th class="th-sort" :class="{ active: sortKey === 'size' }" @click="toggleSort('size')" data-testid="mf-sort-size">
              {{ t('fx.colSize') }} <span class="th-sort-ind">{{ sortIndicator('size') }}</span>
            </th>
            <th>{{ t('fx.colPrice') }}</th>
            <th class="th-sort" :class="{ active: sortKey === 'seeds' }" @click="toggleSort('seeds')" data-testid="mf-sort-seeds">
              {{ t('fx.colStatus') }} <span class="th-sort-ind">{{ sortIndicator('seeds') }}</span>
            </th>
            <th>
              <button
                v-if="sortKey !== 'default'"
                class="th-clear"
                @click="clearSort"
                title="Reset sorting"
                data-testid="mf-sort-clear"
              >✕</button>
            </th>
          </tr>
        </thead>
        <tbody>
          <template v-for="(row, i) in paginatedRows" :key="row.kind === 'header' ? `h-${i}-${row.label}` : row.f.cid">
            <!-- Групповой заголовок «repo · vX.Y» — рендерим только в Dev-вкладке. -->
            <tr v-if="row.kind === 'header'" class="mf-group-hdr" :data-testid="`my-files-group-${row.label}`">
              <td :colspan="6">
                <span class="mfg-title mono">⎇ {{ row.label }}</span>
                <span class="mfg-meta dim">{{ row.count }} · {{ fmtBytes(row.size) }}</span>
              </td>
            </tr>
            <tr
              v-else
              :data-testid="`my-file-${row.f.cid}`"
              :class="{ 'mf-selected': selected.has(row.f.cid) }"
              @contextmenu.prevent="openCtx($event, row.f)"
            >
              <!-- Bulk-select чекбокс. Клик по нему не должен триггерить
                   контекстное меню строки — событие останавливаем. -->
              <td class="td-check" @click.stop>
                <input
                  type="checkbox"
                  :checked="selected.has(row.f.cid)"
                  @change="toggleSelect(row.f.cid)"
                  :data-testid="`mf-select-${row.f.cid}`"
                />
              </td>
            <td class="name-cell">
              <FileTypeIcon :name="row.f.displayName || row.f.name" />
              <div class="name-col">
                <span
                  class="fname"
                  @dblclick="openNative(row.f)"
                  :title="t('fx.openNativeTitle')"
                  :data-testid="`my-file-name-${row.f.cid}`"
                >{{ fileTitle(row.f) }}</span>
                <span class="cid">{{ row.f.cid }}</span>
                <span
                  v-if="tab === 'mine' && row.f.published && row.f.publishedAt"
                  class="meta-date pub"
                  :data-testid="`my-file-pubdate-${row.f.cid}`"
                >
                  {{ t('fx.publishedAt', { date: fmtDateTime(row.f.publishedAt) }) }}
                </span>
                <span
                  v-if="tab === 'downloaded' && row.f.downloadedAt"
                  class="meta-date dl"
                  :data-testid="`my-file-dldate-${row.f.cid}`"
                >
                  {{ t('fx.downloadedAt', { date: fmtDateTime(row.f.downloadedAt) }) }}
                </span>
                <template v-if="tab === 'mine' && row.f.encrypted && recipientsOf(row.f).length">
                  <button
                    class="rcp-toggle"
                    @click="expandedRecipients[row.f.cid] = !expandedRecipients[row.f.cid]"
                    :data-testid="`my-file-recipients-toggle-${row.f.cid}`"
                  >
                    🔒 {{ t('fx.recipients', { n: recipientsOf(row.f).length }) }}
                    {{ expandedRecipients[row.f.cid] ? '▲' : '▼' }}
                  </button>
                  <ul
                    v-if="expandedRecipients[row.f.cid]"
                    class="rcp-list"
                    :data-testid="`my-file-recipients-${row.f.cid}`"
                  >
                    <li v-for="a in recipientsOf(row.f)" :key="a">{{ a }}</li>
                  </ul>
                </template>
              </div>
            </td>
            <td>{{ fmtBytes(row.f.size) }}</td>
            <td>
              <span v-if="row.f.isPaid" class="price">{{ row.f.price }} STH</span>
              <span v-else class="free">{{ t('fx.free') }}</span>
              <span
                v-if="tab === 'mine' && row.f.isPaid"
                class="sales"
                :data-testid="`my-file-sales-${row.f.cid}`"
                :title="t('fx.salesTitle')"
              >
                · {{ sales[row.f.cid] || 0 }} {{ t('fx.salesShort') }}
              </span>
              <span
                v-if="tab === 'mine' && row.f.isPaid && (sales[row.f.cid] || 0) > 0"
                class="revenue"
                :data-testid="`my-file-revenue-${row.f.cid}`"
                :title="t('fx.revenueTitle')"
              >
                · {{ +(row.f.price * (sales[row.f.cid] || 0)).toFixed(4) }} STH
              </span>
            </td>
            <td>
              <template v-if="row.f.downloading">
                <div class="dl-wrap" :data-testid="`my-file-dl-${row.f.cid}`">
                  <div class="dl-bar">
                    <div class="dl-fill" :style="{ width: (dlProgress[row.f.cid] || 0) + '%' }" />
                  </div>
                  <span class="dl-pct">Downloading… {{ dlProgress[row.f.cid] || 0 }}%</span>
                  <span v-if="row.f.seeds" class="seedcount">· {{ row.f.seeds }} {{ t('fx.seedsShort') }}</span>
                </div>
              </template>
              <template v-else-if="tab === 'private'">
                <span v-if="row.f.downloadedAt" class="seed on">{{ t('fx.readyToOpen') }}</span>
                <span v-else class="seed">{{ t('fx.notDownloaded') }}</span>
                <span v-if="row.f.seeds" class="seedcount">· {{ row.f.seeds }} {{ t('fx.seedsShort') }}</span>
              </template>
              <template v-else>
                <span class="seed" :class="{ on: row.f.seeding }">
                  {{ row.f.seeding ? t('fx.seedingDot') : t('fx.stopDot') }}
                </span>
                <span v-if="row.f.seeds" class="seedcount" :data-testid="`my-file-seeds-${row.f.cid}`">
                  · {{ row.f.seeds }} {{ t('fx.seedsShort') }}
                </span>
                <span v-if="row.f.published" class="chain on" :title="t('fx.onChainTitle')">⛓ on-chain</span>
              </template>
            </td>
            <td class="actions">
              <template v-if="tab === 'private'">
                <button
                  v-if="!row.f.downloadedAt && !row.f.downloading"
                  class="mini publish"
                  :disabled="busy === row.f.cid"
                  @click="downloadPrivate(row.f)"
                  :data-testid="`private-download-${row.f.cid}`"
                >
                  {{ busy === row.f.cid ? '…' : t('fx.download') }}
                </button>
                <template v-else-if="row.f.downloadedAt">
                  <button
                    class="mini"
                    @click="openNative(row.f)"
                    :data-testid="`private-open-${row.f.cid}`"
                  >
                    {{ t('fx.open') }}
                  </button>
                  <button
                    class="mini copy"
                    @click="savePrivate(row.f)"
                    :data-testid="`private-save-${row.f.cid}`"
                  >
                    {{ t('fx.save') }}
                  </button>
                </template>
                <button
                  class="mini danger"
                  :disabled="busy === row.f.cid"
                  @click="remove(row.f)"
                  :data-testid="`my-file-delete-${row.f.cid}`"
                >
                  {{ t('fx.delete') }}
                </button>
              </template>
              <template v-else>
                <button
                  v-if="tab === 'downloaded'"
                  class="mini folder"
                  @click="saveAs(row.f)"
                  :data-testid="`my-file-saveas-${row.f.cid}`"
                  :title="t('fx.saveAsTitle')"
                >
                  🗁
                </button>
                <button
                  v-if="!row.f.encrypted"
                  class="mini copy"
                  @click="copyLink(row.f)"
                  :data-testid="`my-file-copy-${row.f.cid}`"
                  :title="row.f.isPaid ? t('fx.linkTitlePaid') : t('fx.linkTitleFree')"
                >
                  {{ copied === row.f.cid ? t('fx.copied') : t('fx.link') }}
                </button>
                <button
                  v-if="!row.f.encrypted"
                  class="mini qr"
                  @click="qrFile = row.f"
                  :data-testid="`my-file-qr-${row.f.cid}`"
                  :title="t('fx.qrBtnTitle')"
                >
                  ⊞ QR
                </button>
                <template v-if="tab === 'mine'">
                  <button
                    v-if="row.f.encrypted"
                    class="mini publish"
                    :disabled="notifyBusy === row.f.cid || !recipientsOf(row.f).length"
                    @click="askNotify(row.f)"
                    :data-testid="`my-file-notify-${row.f.cid}`"
                    :title="t('fx.notifyTitle')"
                  >
                    {{ notifyBusy === row.f.cid ? '…' : t('fx.notify') }}
                  </button>
                  <button
                    v-if="row.f.isPaid && !row.f.published"
                    class="mini publish"
                    :disabled="busy === row.f.cid"
                    @click="publish(row.f)"
                    :data-testid="`my-file-publish-${row.f.cid}`"
                  >
                    {{ busy === row.f.cid ? '…' : t('fx.toMarket') }}
                  </button>
                  <button
                    class="mini"
                    :disabled="busy === row.f.cid"
                    @click="toggleSeed(row.f)"
                    :data-testid="`my-file-seed-${row.f.cid}`"
                  >
                    {{ row.f.seeding ? t('fx.stopSeeding') : t('fx.seed') }}
                  </button>
                </template>
                <button
                  class="mini danger"
                  :disabled="busy === row.f.cid"
                  @click="remove(row.f)"
                  :data-testid="`my-file-delete-${row.f.cid}`"
                >
                  {{ t('fx.delete') }}
                </button>
              </template>
            </td>
          </tr>
          </template>
        </tbody>
      </table>

      <!-- ── Bulk-toolbar (C из плана) ─────────────────────────────
           Плавающая полоса снизу, появляется когда выбрано ≥1 файл.
           Действия: копировать N sn://f-ссылок в буфер + массовое
           удаление (проходит через уже существующие delete-эндпоинты). -->
      <transition name="mf-bulk-slide">
        <div
          v-if="selected.size > 0"
          class="mf-bulk-bar"
          data-testid="mf-bulk-bar"
        >
          <span class="mf-bulk-count"><b>{{ selected.size }}</b> selected</span>
          <button
            class="mf-bulk-btn"
            @click="bulkCopyLinks"
            data-testid="mf-bulk-copy"
          >📋 Copy links</button>
          <button
            class="mf-bulk-btn danger"
            @click="bulkDeleteConfirm = true"
            data-testid="mf-bulk-delete"
          >🗑 Delete</button>
          <button
            class="mf-bulk-btn ghost"
            @click="clearSelection"
            data-testid="mf-bulk-clear"
          >✕ Clear</button>
        </div>
      </transition>

      <!-- Bulk delete confirmation modal -->
      <transition name="fade">
        <div
          v-if="bulkDeleteConfirm"
          class="mf-bulk-conf-back"
          @click.self="bulkDeleteConfirm = false"
          data-testid="mf-bulk-confirm"
        >
          <div class="mf-bulk-conf panel">
            <h3 class="mf-bulk-conf-title">Delete {{ selected.size }} file{{ selected.size === 1 ? '' : 's' }}?</h3>
            <p class="dim mf-bulk-conf-hint">
              This removes them from your file exchange and stops seeding. For your own files
              the content is deleted from disk. This action cannot be undone.
            </p>
            <div class="mf-bulk-conf-btns">
              <button
                class="btn"
                @click="bulkDeleteConfirm = false"
                :disabled="bulkDeleting"
                data-testid="mf-bulk-confirm-cancel"
              >Cancel</button>
              <button
                class="btn btn-danger"
                @click="bulkDeleteRun"
                :disabled="bulkDeleting"
                data-testid="mf-bulk-confirm-ok"
              >{{ bulkDeleting ? 'Deleting…' : 'Delete' }}</button>
            </div>
          </div>
        </div>
      </transition>

      <!-- ── Pagination footer (Мои файлы) ─────────────────────────
           Двойной ряд: слева — компактный переключатель размера страницы
           (10/25/50), справа — стрелки/номера страниц. Показываем только
           если общее число файлов больше самой мелкой страницы (10). -->
      <div
        v-if="totalFileRows > 10"
        class="mf-pager"
        data-testid="my-files-pager"
      >
        <div class="mf-pager-size">
          <span class="dim">Rows per page:</span>
          <button
            v-for="sz in [10, 25, 50] as const"
            :key="sz"
            class="mf-pager-sz"
            :class="{ on: pageSize === sz }"
            :data-testid="`my-files-page-size-${sz}`"
            @click="setPageSize(sz)"
          >{{ sz }}</button>
        </div>
        <div class="mf-pager-nav">
          <span class="dim mf-pager-info" data-testid="my-files-page-info">
            {{ (currentPage - 1) * pageSize + 1 }}–{{ Math.min(currentPage * pageSize, totalFileRows) }}
            / {{ totalFileRows }}
          </span>
          <button
            class="mf-pager-btn"
            :disabled="currentPage === 1"
            @click="currentPage = 1"
            title="First page"
            data-testid="my-files-page-first"
          >⇤</button>
          <button
            class="mf-pager-btn"
            :disabled="currentPage === 1"
            @click="currentPage--"
            title="Previous page"
            data-testid="my-files-page-prev"
          >◂</button>
          <span class="mf-pager-current" data-testid="my-files-page-current">
            {{ currentPage }} / {{ totalPages }}
          </span>
          <button
            class="mf-pager-btn"
            :disabled="currentPage >= totalPages"
            @click="currentPage++"
            title="Next page"
            data-testid="my-files-page-next"
          >▸</button>
          <button
            class="mf-pager-btn"
            :disabled="currentPage >= totalPages"
            @click="currentPage = totalPages"
            title="Last page"
            data-testid="my-files-page-last"
          >⇥</button>
        </div>
      </div>
    </template>

    <AddFileModal
      v-if="showAdd"
      :initial-path="droppedPath"
      @close="showAdd = false"
      @added="onAdded"
    />

    <!-- Оверлей drag&drop файла (как в Торрентах) -->
    <div v-if="dragActive" class="mf-drop-overlay" data-testid="my-files-drop-overlay">
      <div class="mf-drop-box">
        <span class="mf-drop-big">⇩</span>
        <span class="mf-drop-title">Drop to publish</span>
        <span class="mf-drop-sub">file to SmartNet</span>
      </div>
    </div>

    <FileQrModal
      v-if="qrFile"
      :link="fileShareLink(qrFile)"
      :name="qrFile.name"
      @close="qrFile = null"
    />

    <div
      v-if="pendingDelete"
      class="confirm-back"
      @click.self="pendingDelete = null"
      data-testid="delete-confirm-modal"
    >
      <div class="confirm-box">
        <h3>{{ t('fx.deleteTitle') }}</h3>
        <p class="confirm-name">«{{ pendingDelete.name }}»</p>
        <p class="confirm-warn">{{ t('fx.deleteWarn') }}</p>
        <div class="confirm-actions">
          <button class="mini" @click="pendingDelete = null" data-testid="delete-cancel">
            {{ t('fx.cancel') }}
          </button>
          <button class="mini danger solid" @click="confirmDelete" data-testid="delete-confirm">
            {{ t('fx.deleteConfirm') }}
          </button>
        </div>
      </div>
    </div>

    <!-- подтверждение ончейн-оповещения получателей -->
    <div
      v-if="notifyConfirm"
      class="confirm-back"
      @click.self="notifyConfirm = null"
      data-testid="notify-confirm-modal"
    >
      <div class="confirm-box">
        <h3>{{ t('fx.notifyCostTitle') }}</h3>
        <p class="confirm-name">🔒 {{ notifyConfirm.displayName || t('fx.privateFile') }}</p>
        <p class="confirm-warn">
          {{
            t('fx.notifyCostBody', {
              n: recipientsOf(notifyConfirm).length,
              fee: NOTIFY_FEE,
              total: +(recipientsOf(notifyConfirm).length * NOTIFY_FEE).toFixed(8),
            })
          }}
        </p>
        <div class="confirm-actions">
          <button class="mini" @click="notifyConfirm = null" data-testid="notify-cancel">
            {{ t('fx.cancel') }}
          </button>
          <button
            class="mini publish solid"
            @click="notify(notifyConfirm)"
            data-testid="notify-confirm"
          >
            {{ t('fx.notifyConfirmBtn') }}
          </button>
        </div>
      </div>
    </div>

    <!-- контекстное меню по правому клику -->
    <div v-if="ctxMenu" class="ctx-backdrop" @click="closeCtx" @contextmenu.prevent="closeCtx">
      <div
        class="ctx-menu"
        :style="{ left: ctxMenu.x + 'px', top: ctxMenu.y + 'px' }"
        data-testid="file-context-menu"
      >
        <button class="ctx-item" @click="saveAs(ctxMenu.file)" data-testid="ctx-save-as">
          🗁 {{ t('fx.saveAs') }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.mf {
  padding: 28px 32px;
  width: 100%;
}
.mf-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
}
.mf-head h1 {
  font-size: 28px;
  letter-spacing: 3px;
  color: var(--text);
}
.sub {
  margin-top: 6px;
  color: var(--text-dim);
  font-size: 13px;
}
.add-btn {
  background: linear-gradient(90deg, light-dark(#0e7490, #22d3ee), #0891b2);
  color: light-dark(#ffffff, #03121a);
  border: none;
  border-radius: 10px;
  padding: 11px 20px;
  font-weight: 700;
  letter-spacing: 1px;
  cursor: pointer;
  transition: transform 0.15s;
}
.add-btn:hover {
  transform: translateY(-1px);
}
.add-btn.ghost {
  margin-top: 16px;
  background: transparent;
  border: 1px solid rgba(34, 211, 238, 0.4);
  color: light-dark(#0e7490, #22d3ee);
}
.stats {
  display: flex;
  gap: 16px;
  margin: 26px 0;
}
.stat {
  border: 1px solid rgba(34, 211, 238, 0.14);
  border-radius: 12px;
  padding: 16px 20px;
  background: light-dark(var(--panel), rgba(13, 20, 28, 0.6));
  min-width: 130px;
}
.stat.wide {
  flex: 1;
}
.stat-val {
  display: block;
  font-size: 24px;
  color: light-dark(#0e7490, #22d3ee);
  font-weight: 700;
}
.stat-label {
  font-size: 11px;
  color: var(--text-dim);
  letter-spacing: 1px;
}
.disk-row {
  display: flex;
  justify-content: space-between;
  margin-bottom: 10px;
}
.bar {
  height: 8px;
  background: rgba(34, 211, 238, 0.1);
  border-radius: 999px;
  overflow: hidden;
}
.bar-fill {
  height: 100%;
  background: linear-gradient(90deg, light-dark(#0e7490, #22d3ee), #d946ef);
  transition: width 0.4s;
}
.notice,
.empty {
  text-align: center;
  padding: 60px 0;
  color: var(--text-dim);
}

/* ── Category sub-tabs + search (my-files) ─────────────────────────
   A из плана: под основным tab-баром показываем 5 pill-кнопок
   (All / Dev / Music / Images / Other) с точечным счётчиком и общим
   весом. Активная категория подсвечивается своим цветом, что помогает
   быстро понять «где сидят гигабайты» без чтения ярлыков. Правее —
   узкий search-input по имени файла / repo / версии. */
.mine-cats {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
  margin: 14px 0 10px;
}
.mc-pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 5px 12px;
  background: light-dark(rgba(0,0,0,0.05), rgba(0,0,0,0.28));
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--text-dim);
  font-family: var(--font-mono);
  font-size: 11px;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
  cursor: pointer;
  transition: color 0.15s ease, background-color 0.15s ease, border-color 0.15s ease, box-shadow 0.2s ease;
}
.mc-pill:hover { color: var(--text); border-color: var(--accent); }
.mc-pill.on { color: var(--text); border-color: var(--accent); background: rgba(176,133,255,0.10); box-shadow: 0 0 12px rgba(176,133,255,0.28); }
.mc-pill.mc-dev.on   { color: light-dark(#2563eb, #60a5fa); border-color: rgba(96,165,250,0.55);  background: rgba(96,165,250,0.10);  box-shadow: 0 0 14px rgba(96,165,250,0.35); }
.mc-pill.mc-music.on { color: light-dark(#b45309, #f5b301); border-color: rgba(245,158,11,0.55);  background: rgba(245,158,11,0.10);  box-shadow: 0 0 14px rgba(245,158,11,0.35); }
.mc-pill.mc-image.on { color: light-dark(#15803d, #22c55e); border-color: rgba(34,197,94,0.55);   background: rgba(34,197,94,0.10);   box-shadow: 0 0 14px rgba(34,197,94,0.35); }
.mc-pill.mc-other.on { color: light-dark(#7c3aed, #a78bfa); border-color: rgba(167,139,250,0.55); background: rgba(167,139,250,0.10); box-shadow: 0 0 14px rgba(167,139,250,0.35); }
.mc-cnt { color: var(--text); }
.mc-sz  { font-weight: var(--fw-500); font-size: 10.5px; }
.mc-search {
  flex: 1;
  min-width: 180px;
  max-width: 320px;
  margin-left: auto;
  padding: 6px 12px;
  background: light-dark(rgba(0,0,0,0.04), rgba(0,0,0,0.24));
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--text);
  font-size: 12px;
  outline: none;
  transition: border-color 0.15s ease;
}
.mc-search:focus { border-color: var(--accent); }
.mc-search::placeholder { color: var(--text-dim); opacity: 0.7; }

/* Групповой заголовок внутри таблицы (Dev-вкладка: «repo · vX.Y»). */
.mf-group-hdr td {
  padding: 12px 12px 8px !important;
  background: transparent !important;
  border-top: 1px dashed var(--border) !important;
  border-bottom: 0 !important;
}
.mf-group-hdr td .mfg-title {
  color: light-dark(#2563eb, #60a5fa);
  font-size: 11.5px;
  font-weight: var(--fw-700);
  letter-spacing: 0.04em;
}
.mf-group-hdr td .mfg-meta {
  margin-left: 10px;
  font-size: 11px;
}

/* ── Сортировка кликом по заголовку колонки (этап D) ────────────
   Показываем индикатор направления ▲/▼ рядом с активным заголовком,
   при hover бордер снизу подсвечивается акцентом. `.th-clear`
   позволяет сбросить сортировку и вернуться к дефолтному порядку. */
.th-sort {
  cursor: pointer;
  user-select: none;
  transition: color 0.15s ease, background-color 0.15s ease;
}
.th-sort:hover { color: var(--text) !important; background: light-dark(rgba(0,0,0,0.04), rgba(255,255,255,0.03)); }
.th-sort.active { color: var(--accent) !important; }
.th-sort-ind {
  display: inline-block;
  width: 10px;
  margin-left: 4px;
  font-size: 10px;
  color: var(--accent);
}
.th-clear {
  width: 22px; height: 22px;
  display: inline-flex; align-items: center; justify-content: center;
  padding: 0;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--text-dim);
  cursor: pointer;
  font-size: 11px;
  transition: color 0.15s ease, border-color 0.15s ease;
}
.th-clear:hover { color: var(--text); border-color: var(--accent); }

/* ── Pagination footer ──────────────────────────────────────────
   Компактная нижняя полоса: слева — переключатель размера страницы
   (10/25/50, persist в localStorage), справа — навигация «⇤ ◂ N/M ▸ ⇥». */
.mf-pager {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 14px 4px 6px;
  flex-wrap: wrap;
  border-top: 1px solid var(--border-subtle);
}
.mf-pager-size, .mf-pager-nav {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.05em;
}
.mf-pager-sz {
  padding: 4px 10px;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--text-dim);
  font-family: inherit;
  font-size: 11px;
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease;
}
.mf-pager-sz:hover { color: var(--text); border-color: var(--accent); }
.mf-pager-sz.on { color: var(--accent); border-color: var(--accent); background: rgba(176,133,255,0.10); }
.mf-pager-btn {
  width: 26px; height: 26px;
  display: inline-flex; align-items: center; justify-content: center;
  padding: 0;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--text-dim);
  cursor: pointer;
  font-size: 11px;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease;
}
.mf-pager-btn:hover:not(:disabled) { color: var(--text); border-color: var(--accent); background: light-dark(rgba(0,0,0,0.04), rgba(255,255,255,0.04)); }
.mf-pager-btn:disabled { opacity: 0.35; cursor: not-allowed; }
.mf-pager-current { color: var(--text); padding: 0 6px; font-weight: var(--fw-700); }
.mf-pager-info { padding: 0 8px 0 4px; }

/* ── C: Bulk-выбор + плавающий toolbar ──────────────────────────── */
.th-check, .td-check {
  width: 32px;
  padding-left: 12px !important;
  padding-right: 0 !important;
}
.th-check input, .td-check input {
  width: 15px;
  height: 15px;
  cursor: pointer;
  accent-color: var(--accent);
}
tr.mf-selected {
  background: rgba(176,133,255,0.06) !important;
}
tr.mf-selected td { border-color: rgba(176,133,255,0.28); }

.mf-bulk-bar {
  position: fixed;
  left: 50%;
  bottom: 28px;
  transform: translateX(-50%);
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 8px 14px;
  border-radius: 999px;
  background: light-dark(rgba(255,255,255,0.97), rgba(20,22,28,0.94));
  border: 1px solid var(--accent);
  box-shadow: 0 12px 40px rgba(0,0,0,0.55), 0 0 24px rgba(176,133,255,0.25);
  backdrop-filter: blur(10px);
  -webkit-backdrop-filter: blur(10px);
  z-index: 1500;
  font-family: var(--font-mono);
  font-size: 12px;
}
.mf-bulk-count { color: var(--text); padding: 0 6px; letter-spacing: 0.04em; }
.mf-bulk-count b { color: var(--accent); }
.mf-bulk-btn {
  padding: 6px 12px;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--text);
  cursor: pointer;
  font-size: 11.5px;
  font-family: inherit;
  font-weight: var(--fw-700);
  letter-spacing: 0.04em;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease;
}
.mf-bulk-btn:hover { border-color: var(--accent); background: rgba(176,133,255,0.10); }
.mf-bulk-btn.danger { color: light-dark(#dc2626, #f87171); border-color: rgba(248,113,113,0.4); }
.mf-bulk-btn.danger:hover { background: rgba(248,113,113,0.10); border-color: light-dark(#dc2626, #f87171); }
.mf-bulk-btn.ghost { color: var(--text-dim); }

.mf-bulk-slide-enter-active, .mf-bulk-slide-leave-active {
  transition: transform 0.2s ease, opacity 0.2s ease;
}
.mf-bulk-slide-enter-from, .mf-bulk-slide-leave-to {
  transform: translate(-50%, 20px);
  opacity: 0;
}

.mf-bulk-conf-back {
  position: fixed; inset: 0; z-index: 2000;
  background: rgba(0,0,0,0.55);
  backdrop-filter: blur(6px);
  -webkit-backdrop-filter: blur(6px);
  display: flex; align-items: center; justify-content: center;
  padding: 24px;
}
.mf-bulk-conf {
  width: 100%;
  max-width: 440px;
  border-radius: 16px;
  padding: 24px 26px 22px;
  background: var(--surface);
  border: 1px solid var(--border);
  box-shadow: 0 24px 64px rgba(0,0,0,0.55);
}
.mf-bulk-conf-title { font-size: 16px; font-weight: var(--fw-700); margin: 0 0 10px; color: light-dark(#dc2626, #f87171); }
.mf-bulk-conf-hint  { font-size: 12px; line-height: 1.55; margin: 0 0 18px; }
.mf-bulk-conf-btns  { display: flex; justify-content: flex-end; gap: 10px; }
.fade-enter-active, .fade-leave-active { transition: opacity 0.15s ease; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
.tabs {
  display: flex;
  gap: 8px;
  margin: 8px 0 18px;
}
.tabs button {
  background: light-dark(rgba(15, 23, 32, 0.06), rgba(15, 23, 32, 0.6));
  border: 1px solid rgba(34, 211, 238, 0.15);
  color: var(--text-dim);
  padding: 8px 18px;
  border-radius: 999px;
  cursor: pointer;
  font-size: 12px;
  letter-spacing: 1px;
  transition: color 0.2s, border-color 0.2s, background-color 0.2s;
}
.tabs button.on {
  color: light-dark(#ffffff, #03121a);
  background: light-dark(#0e7490, #22d3ee);
  border-color: light-dark(#0e7490, #22d3ee);
  font-weight: 700;
}
.tabs .cnt {
  display: inline-block;
  margin-left: 6px;
  padding: 1px 7px;
  border-radius: 999px;
  background: light-dark(rgba(0, 0, 0, 0.08), rgba(0, 0, 0, 0.18));
  font-size: 11px;
}
.meta-date {
  font-size: 10px;
  font-family: ui-monospace, monospace;
}
.meta-date.pub {
  color: light-dark(#059669, #34d399);
}
.meta-date.dl {
  color: light-dark(#0284c7, #38bdf8);
}
.empty-ico {
  font-size: 46px;
  color: rgba(34, 211, 238, 0.3);
}
.empty p {
  margin-top: 12px;
}
.files {
  width: 100%;
  border-collapse: collapse;
}
.files th {
  text-align: left;
  font-size: 11px;
  letter-spacing: 1px;
  color: var(--text-dim);
  padding: 10px 12px;
  border-bottom: 1px solid rgba(34, 211, 238, 0.12);
}
.files td {
  padding: 14px 12px;
  border-bottom: 1px solid light-dark(rgba(0, 0, 0, 0.07), rgba(255, 255, 255, 0.04));
  color: var(--text);
  font-size: 13px;
  white-space: nowrap;
  vertical-align: middle;
}
.name-cell {
  display: flex;
  flex-direction: row;
  align-items: center;
  gap: 12px;
  width: 99%;
}
.name-col {
  display: flex;
  flex-direction: column;
  gap: 3px;
  min-width: 0;
}
.fname {
  color: var(--text);
  font-weight: 600;
  cursor: pointer;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.fname:hover {
  color: light-dark(#0e7490, #22d3ee);
}
.cid {
  font-size: 10px;
  color: var(--text-dim);
  font-family: ui-monospace, monospace;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 340px;
}
.price {
  color: light-dark(#a21caf, #e879f9);
}
.sales {
  font-size: 11px;
  color: light-dark(#b45309, #fbbf24);
  margin-left: 4px;
}
.revenue {
  font-size: 11px;
  color: light-dark(#059669, #34d399);
  margin-left: 4px;
}
.free {
  color: light-dark(#0e7490, #22d3ee);
}
.seed {
  font-size: 12px;
  color: var(--text-dim);
}
.seed.on {
  color: light-dark(#059669, #34d399);
}
.actions {
  display: flex;
  gap: 6px;
  justify-content: flex-end;
  flex-wrap: nowrap;
}
.mini {
  background: transparent;
  border: 1px solid rgba(34, 211, 238, 0.3);
  color: var(--text-dim);
  border-radius: 7px;
  padding: 6px 10px;
  font-size: 11px;
  cursor: pointer;
  white-space: nowrap;
  transition: color 0.2s, border-color 0.2s;
}
.mini:hover {
  color: light-dark(#0e7490, #22d3ee);
  border-color: light-dark(#0e7490, #22d3ee);
}
.mini.danger:hover {
  color: light-dark(#dc2626, #f87171);
  border-color: light-dark(#dc2626, #f87171);
}
.mini:disabled {
  opacity: 0.5;
  cursor: wait;
}
.confirm-back {
  position: fixed;
  inset: 0;
  z-index: 1000;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(3, 6, 10, 0.78);
  backdrop-filter: blur(6px);
}
.confirm-box {
  width: 380px;
  max-width: 90vw;
  border: 1px solid rgba(248, 113, 113, 0.3);
  border-radius: 14px;
  background: light-dark(var(--panel), #0a0f16);
  padding: 24px;
  text-align: center;
}
.confirm-box h3 {
  font-size: 14px;
  letter-spacing: 2px;
  color: light-dark(#dc2626, #f87171);
}
.confirm-name {
  margin: 14px 0 6px;
  color: var(--text);
  font-size: 14px;
  word-break: break-all;
}
.confirm-warn {
  font-size: 12px;
  color: var(--text-dim);
  margin-bottom: 18px;
}
.confirm-actions {
  display: flex;
  gap: 10px;
  justify-content: center;
}
.mini.danger.solid {
  background: light-dark(#dc2626, #f87171);
  color: #1a0606;
  border-color: light-dark(#dc2626, #f87171);
  font-weight: 700;
}
.mini.danger.solid:hover {
  background: #ef4444;
  color: #fff;
}
.mini.publish {
  border-color: rgba(34, 211, 238, 0.5);
  color: light-dark(#0e7490, #22d3ee);
}
.mini.publish:hover {
  background: light-dark(#0e7490, #22d3ee);
  color: light-dark(#ffffff, #03121a);
}
.mini.copy {
  border-color: rgba(52, 211, 153, 0.35);
  color: light-dark(#059669, #34d399);
}
.mini.copy:hover {
  background: rgba(52, 211, 153, 0.12);
  border-color: light-dark(#059669, #34d399);
  color: light-dark(#059669, #34d399);
}
.mini.qr {
  border-color: rgba(148, 163, 184, 0.3);
  color: var(--text-dim);
}
.mini.qr:hover {
  background: rgba(148, 163, 184, 0.12);
  border-color: var(--text);
  color: var(--text);
}
.mini.folder {
  border-color: rgba(251, 191, 36, 0.4);
  color: light-dark(#b45309, #fbbf24);
  font-size: 13px;
  padding: 6px 9px;
}
.mini.publish.solid {
  background: linear-gradient(90deg, light-dark(#0e7490, #22d3ee), #0891b2);
  color: light-dark(#ffffff, #03121a);
  border: none;
  font-weight: 700;
}
.mini.folder:hover {
  background: rgba(251, 191, 36, 0.12);
  border-color: light-dark(#b45309, #fbbf24);
  color: light-dark(#b45309, #fbbf24);
}
.ctx-backdrop {
  position: fixed;
  inset: 0;
  z-index: 1100;
}
.ctx-menu {
  position: fixed;
  min-width: 180px;
  background: light-dark(var(--panel), #0a0f16);
  border: 1px solid rgba(34, 211, 238, 0.3);
  border-radius: 10px;
  padding: 6px;
  box-shadow: 0 12px 40px rgba(3, 6, 10, 0.7);
}
.ctx-item {
  display: block;
  width: 100%;
  text-align: left;
  background: transparent;
  border: none;
  color: var(--text);
  padding: 9px 12px;
  border-radius: 7px;
  font-size: 13px;
  cursor: pointer;
  transition: background-color 0.15s, color 0.15s;
}
.ctx-item:hover {
  background: rgba(34, 211, 238, 0.12);
  color: light-dark(#0e7490, #22d3ee);
}
.seedcount {
  font-size: 11px;
  color: light-dark(#059669, #34d399);
  margin-left: 4px;
}
.chain {
  display: inline-block;
  margin-left: 8px;
  font-size: 10px;
  color: light-dark(#059669, #34d399);
}
.downloaded {
  font-size: 11px;
  color: var(--text-dim);
}
.pub-error {
  margin: 12px 0;
  padding: 10px 14px;
  border: 1px solid rgba(248, 113, 113, 0.4);
  border-radius: 8px;
  background: rgba(248, 113, 113, 0.08);
  color: light-dark(#dc2626, #fca5a5);
  font-size: 12px;
}
.notify-msg {
  margin: 12px 0;
  padding: 10px 14px;
  border: 1px solid rgba(52, 211, 153, 0.4);
  border-radius: 8px;
  background: rgba(52, 211, 153, 0.08);
  color: light-dark(#047857, #6ee7b7);
  font-size: 12px;
}
.rcp-toggle {
  align-self: flex-start;
  margin-top: 3px;
  background: transparent;
  border: none;
  color: light-dark(#0e7490, #22d3ee);
  font-size: 10px;
  letter-spacing: 0.5px;
  cursor: pointer;
  padding: 0;
}
.rcp-toggle:hover {
  color: light-dark(#155e75, #67e8f9);
}
.rcp-list {
  margin: 4px 0 0;
  padding: 6px 10px;
  list-style: none;
  border-left: 2px solid rgba(34, 211, 238, 0.3);
  background: rgba(34, 211, 238, 0.04);
  border-radius: 0 6px 6px 0;
  max-width: 360px;
}
.rcp-list li {
  font-size: 10px;
  font-family: ui-monospace, monospace;
  color: var(--text-dim);
  word-break: break-all;
  padding: 1px 0;
}
.dl-wrap {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 140px;
}
.dl-bar {
  height: 6px;
  border-radius: 999px;
  background: rgba(148, 163, 184, 0.22);
  overflow: hidden;
}
.dl-fill {
  height: 100%;
  border-radius: 999px;
  background: linear-gradient(90deg, light-dark(#0e7490, #22d3ee), light-dark(#059669, #34d399));
  transition: width 0.4s ease;
}
.dl-pct {
  font-size: 11px;
  font-weight: 700;
  color: light-dark(#0e7490, #22d3ee);
}
.mf-drop-overlay {
  position: fixed;
  inset: 0;
  z-index: 900;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(4, 12, 18, 0.82);
  backdrop-filter: blur(4px);
  pointer-events: none;
}
.mf-drop-box {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 56px 80px;
  border: 2px dashed light-dark(#0e7490, #22d3ee);
  border-radius: 18px;
  background: rgba(34, 211, 238, 0.06);
  box-shadow: 0 0 40px rgba(34, 211, 238, 0.25);
}
.mf-drop-big {
  font-size: 56px;
  color: #22d3ee;
  animation: mf-bounce 1.1s ease-in-out infinite;
}
@keyframes mf-bounce {
  0%, 100% { transform: translateY(0); }
  50% { transform: translateY(8px); }
}
.mf-drop-title {
  font-size: 18px;
  font-weight: 800;
  letter-spacing: 0.08em;
  color: #e2e8f0;
}
.mf-drop-sub {
  font-size: 13px;
  color: #22d3ee;
  font-weight: 700;
  letter-spacing: 0.14em;
}
</style>
>
