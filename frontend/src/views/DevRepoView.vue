<script setup lang="ts">
// Страница репозитория — аналог GitHub: файлы + README (вкладка «Код»),
// релизы с ассетами из f:// (скачивание ТОЛЬКО по явному клику), ишью/вики.
import { computed, onMounted, onBeforeUnmount, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { marked } from 'marked'
import DOMPurify from 'dompurify'
import {
  addFile,
  appListen,
  devhubCreateRelease,
  devhubDeleteRelease,
  devhubEditRelease,
  devhubFilesLocal,
  devhubForkRepo,
  devhubListFiles,
  devhubReadFile,
  devhubStarredByMe,
  devhubSyncFiles,
  devhubSyncFork,
  devhubToggleStar,
  getMyUsername,
  getRepository,
  isTauri,
  listMyFiles,
  onTorrentProgress,
  pickFile,
  quickAddMagnet,
  startFileDownload,
  type DevAssetInfo,
  type DevFileContent,
  type DevFileEntry,
  type DevReleaseInfo,
  type DevRepository,
  type FileManifest,
  type TorrentStatus,
} from '@/lib/bridge'
import { useToastsStore } from '@/store/toasts'
import DevPrPanel from '@/components/DevPrPanel.vue'
import UserAvatar from '@/components/UserAvatar.vue'
import { buildFileTag, osFromFilename, osLabel } from '@/lib/fileTags'

const route = useRoute()
const router = useRouter()
const toasts = useToastsStore()
const repo = ref<DevRepository | null>(null)
const loading = ref(true)
const error = ref('')

const username = computed(() => String(route.params.username || ''))
const repoName = computed(() => String(route.params.repo || ''))

// SHA последнего коммита default-ветки (из подписанных git-рефов индекса)
const headSha = computed(() => {
  const refs = repo.value?.refs || []
  const def = `refs/heads/${repo.value?.defaultBranch || 'main'}`
  const hit = refs.find((r) => r.name === def) || refs.find((r) => r.name.startsWith('refs/heads/'))
  return hit?.sha || ''
})
async function copySha() {
  if (!headSha.value) return
  try {
    await navigator.clipboard.writeText(headSha.value)
    toasts.push('dev://', 'Commit SHA copied', 'in')
  } catch { /* clipboard недоступен */ }
}

// ---- вкладка «Коммиты»: история из подписанного индекса ----
function fmtCommitDate(secs: number): string {
  if (!secs) return ''
  return new Date(secs * 1000).toLocaleDateString(undefined, {
    year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
  })
}
async function copyCommitSha(sha: string) {
  try {
    await navigator.clipboard.writeText(sha)
    toasts.push('dev://', 'Commit SHA copied', 'in')
  } catch { /* clipboard недоступен */ }
}

// ---- дифф коммита (клик по строке → git show из бандла) ----
const openDiff = ref<Record<string, boolean>>({})
const diffCache = ref<Record<string, string[]>>({})
const diffErr = ref<Record<string, string>>({})
const diffLoading = ref<Record<string, boolean>>({})

async function toggleDiff(sha: string) {
  openDiff.value[sha] = !openDiff.value[sha]
  if (!openDiff.value[sha] || diffCache.value[sha] || diffLoading.value[sha]) return
  diffLoading.value[sha] = true
  try {
    const { devhubCommitDiff } = await import('@/lib/bridge')
    const text = await devhubCommitDiff(username.value, repoName.value, sha)
    diffCache.value[sha] = text.split('\n')
  } catch (e: any) {
    diffErr.value[sha] = typeof e === 'string' ? e : 'Failed to load diff'
  } finally {
    diffLoading.value[sha] = false
  }
}

function diffLineClass(l: string): string {
  if (l.startsWith('+++') || l.startsWith('---')) return 'dl-file'
  if (l.startsWith('diff --git')) return 'dl-head'
  if (l.startsWith('@@')) return 'dl-hunk'
  if (l.startsWith('+')) return 'dl-add'
  if (l.startsWith('-')) return 'dl-del'
  return ''
}

const TABS = [
  { id: 'code', label: 'Code' },
  { id: 'commits', label: 'Commits' },
  { id: 'releases', label: 'Releases' },
  { id: 'prs', label: 'Pull Requests' },
  { id: 'issues', label: 'Issues' },
  { id: 'wiki', label: 'Wiki' },
]

// вкладка «Настройки» видна только владельцу (добавляется динамически)
const visibleTabs = computed(() =>
  isOwn.value ? [...TABS, { id: 'settings', label: '⚙ Settings' }] : TABS,
)

// ---- настройки: удаление репозитория (danger zone, как на GitHub) ----
const delConfirm = ref('')
const deleting = ref(false)
async function deleteRepo() {
  if (delConfirm.value !== repoName.value) {
    toasts.push('dev://', 'Type the exact repository name to confirm', 'error')
    return
  }
  deleting.value = true
  try {
    const { devhubDeleteRepo } = await import('@/lib/bridge')
    await devhubDeleteRepo(repoName.value)
    toasts.push('dev://', `Repository «${repoName.value}» deleted`, 'in')
    router.push('/dev')
  } catch (e: any) {
    toasts.push('dev://', typeof e === 'string' ? e : 'Failed to delete repository', 'error')
  } finally {
    deleting.value = false
  }
}
const tab = ref<string>((route.query.tab as string) || 'code')
function setTab(id: string) {
  tab.value = id
  router.replace({ query: id === 'code' ? {} : { tab: id } })
}

// ---- вкладка «Код»: файловый браузер + README ----
const files = ref<DevFileEntry[]>([])
const filesErr = ref('')
const curPath = ref('')
const readmeHtml = ref('')
const fileView = ref<DevFileContent | null>(null)
const fileViewName = ref('')
/// Локальна ли копия файлов (у чужого репо — false до P2P-синка).
const filesLocal = ref(true)
const syncing = ref(false)

const crumbs = computed(() => {
  const parts = curPath.value.split('/').filter(Boolean)
  return parts.map((p, i) => ({ name: p, path: parts.slice(0, i + 1).join('/') }))
})

function renderMd(text: string): string {
  const raw = marked.parse(text, { breaks: true, async: false }) as string
  return DOMPurify.sanitize(raw)
}

function fmtSize(b: number): string {
  if (b < 1024) return `${b} B`
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`
  return `${(b / 1024 / 1024).toFixed(1)} MB`
}

async function loadFiles() {
  filesErr.value = ''
  fileView.value = null
  readmeHtml.value = ''
  try {
    files.value = await devhubListFiles(username.value, repoName.value, curPath.value)
    const rd = files.value.find((f) => !f.isDir && f.name.toLowerCase() === 'readme.md')
    if (rd) {
      const c = await devhubReadFile(username.value, repoName.value, rd.path)
      if (!c.binary) readmeHtml.value = renderMd(c.content)
    }
  } catch (e: any) {
    files.value = []
    filesErr.value = typeof e === 'string' ? e : e?.message || 'Failed to load file list'
  }
}

async function openEntry(f: DevFileEntry) {
  if (f.isDir) {
    curPath.value = f.path
    await loadFiles()
    return
  }
  // Чужой репо без локальной копии: контент тянем от сидеров по требованию.
  if (!filesLocal.value) {
    const ok = await syncFiles()
    if (!ok) return
  }
  try {
    fileViewName.value = f.name
    fileView.value = await devhubReadFile(username.value, repoName.value, f.path)
  } catch (e: any) {
    toasts.push('dev://', typeof e === 'string' ? e : 'Failed to open file', 'error')
  }
}

async function goCrumb(path: string) {
  curPath.value = path
  await loadFiles()
}

// ---- P2P-синк файлов чужого репо (bundle от сидеров → checkout на диск) ----
async function syncFiles(): Promise<boolean> {
  if (syncing.value) return false
  syncing.value = true
  try {
    await devhubSyncFiles(username.value, repoName.value)
    filesLocal.value = true
    toasts.push('dev://', 'Repository files downloaded from seeders', 'in')
    await loadFiles()
    return true
  } catch (e: any) {
    toasts.push('dev://', typeof e === 'string' ? e : 'Failed to download files (seeders offline?)', 'error')
    return false
  } finally {
    syncing.value = false
  }
}

// ---- ⑂ Fork в свой аккаунт (как на GitHub) ----
const forking = ref(false)
async function forkRepo() {
  if (forking.value) return
  forking.value = true
  try {
    const myName = await devhubForkRepo(username.value, repoName.value)
    toasts.push('dev://', `⑂ Fork created: dev://${myName}/${repoName.value}`, 'in')
    router.push({ name: 'dev-repo', params: { username: myName, repo: repoName.value } })
  } catch (e: any) {
    toasts.push('dev://', typeof e === 'string' ? e : 'Failed to create fork', 'error')
  } finally {
    forking.value = false
  }
}

function openForkSource() {
  const [u, r] = (repo.value?.forkedFrom || '').split('/')
  if (u && r) router.push({ name: 'dev-repo', params: { username: u, repo: r } })
}

// ---- ⇅ Sync fork: подтянуть свежие коммиты оригинала (только владелец форка) ----
const syncingFork = ref(false)
async function syncFork() {
  if (syncingFork.value) return
  syncingFork.value = true
  try {
    const res = await devhubSyncFork(repoName.value)
    if (!res.changed) {
      toasts.push('dev://', 'Fork is already up to date — no new commits', 'in')
    } else {
      const n = res.aheadBy
      const suffix = n === 1 ? 'commit' : 'commits'
      toasts.push('dev://', `⇅ Pulled ${n} ${suffix} from ${res.source}`, 'in')
      repo.value = await getRepository(username.value, repoName.value)
      await loadFiles()
    }
  } catch (e: any) {
    toasts.push('dev://', typeof e === 'string' ? e : 'Failed to sync fork', 'error')
  } finally {
    syncingFork.value = false
  }
}

// ---- вкладка «Релизы»: ассеты качаются ТОЛЬКО по клику (через f://) ----
const downloading = ref<Record<string, boolean>>({})

// ---- создание релиза (только свой репозиторий) ----
const myUsername = ref('')
const isOwn = computed(() => !!myUsername.value && myUsername.value === username.value)
const showReleaseForm = ref(false)
const relTag = ref('')
const relTitle = ref('')
const relNotes = ref('')
const relAssets = ref<DevAssetInfo[]>([])
const myFiles = ref<FileManifest[]>([])
const relSaving = ref(false)
const relError = ref('')
// DropZone-состояния: активное перетаскивание над зоной, список активных
// загрузок с прогрессом (пока Rust `add_file` не возвращает промежуточные
// events, показываем «indeterminate» pill).
const relUploading = ref<Array<{ name: string; size: number; done: boolean }>>([])
const relDragActive = ref(false)

async function openReleaseForm() {
  showReleaseForm.value = !showReleaseForm.value
  if (showReleaseForm.value && !myFiles.value.length) {
    try {
      myFiles.value = await listMyFiles()
    } catch {
      myFiles.value = []
    }
  }
}

/**
 * Загрузить релизный бинарник в файловое хранилище с тегом
 * `{"cat":"dev","repo":"<repo>","ver":"<tag>","os":"<win|macos|…>"}`
 * и добавить его в список ассетов создаваемого релиза. Плата/приватность
 * форсированно `0/false` — релизы всегда бесплатны и публичны.
 */
async function uploadReleaseAsset(srcPath: string) {
  const name = srcPath.split(/[\\/]/).pop() || srcPath
  const size = 0
  const slot: { name: string; size: number; done: boolean } = { name, size, done: false }
  relUploading.value.push(slot)
  try {
    const tagStr = buildFileTag({
      cat: 'dev',
      repo: repoName.value,
      ver: (relTag.value || '').trim() || 'unreleased',
      os: osFromFilename(name),
    })
    const m = await addFile(srcPath, 0, false, [], tagStr)
    // Добавляем в ассеты релиза (если ещё не там — dedupe по CID)
    if (!relAssets.value.some((a) => a.cid === m.cid)) {
      relAssets.value.push({ name: m.name, size: m.size, cid: m.cid, mirror: '' })
    }
    slot.done = true
    // Убираем строчку через секунду, чтобы UI не рос.
    setTimeout(() => {
      const i = relUploading.value.indexOf(slot)
      if (i >= 0) relUploading.value.splice(i, 1)
    }, 1200)
    toasts.push('dev://', `✓ ${m.name} added to release`, 'in')
  } catch (e: any) {
    slot.done = true
    toasts.push('dev://', typeof e === 'string' ? e : e?.message || 'Upload failed', 'error')
    setTimeout(() => {
      const i = relUploading.value.indexOf(slot)
      if (i >= 0) relUploading.value.splice(i, 1)
    }, 3000)
  }
}

/** Клик по DropZone → системный picker → загрузка. */
async function pickAndUploadAsset() {
  try {
    const p = await pickFile()
    if (p) await uploadReleaseAsset(p)
  } catch (e: any) {
    toasts.push('dev://', typeof e === 'string' ? e : 'File picker unavailable', 'error')
  }
}

/** Drag-drop реального файла в DropZone (Tauri v2 хочет получать file path). */
function onRelDragEnter(e: DragEvent) { e.preventDefault(); relDragActive.value = true }
function onRelDragOver(e: DragEvent)  { e.preventDefault(); relDragActive.value = true }
function onRelDragLeave()             { relDragActive.value = false }
async function onRelDrop(e: DragEvent) {
  e.preventDefault()
  relDragActive.value = false
  const files = Array.from(e.dataTransfer?.files || [])
  // В web-preview у File нет `.path` — работает только внутри Tauri.
  for (const f of files) {
    const path = (f as unknown as { path?: string }).path
    if (path) {
      await uploadReleaseAsset(path)
    } else {
      toasts.push('dev://', 'DropZone доступен только в десктоп-приложении', 'info')
    }
  }
}

function removeAsset(cid: string) {
  const i = relAssets.value.findIndex((a) => a.cid === cid)
  if (i >= 0) relAssets.value.splice(i, 1)
}

async function submitRelease() {
  if (!relTag.value.trim() || relSaving.value) return
  relSaving.value = true
  relError.value = ''
  try {
    await devhubCreateRelease(repoName.value, relTag.value, relTitle.value, relNotes.value, relAssets.value)
    toasts.push('dev://', `Release ${relTag.value} published and signed`, 'in')
    showReleaseForm.value = false
    relTag.value = ''
    relTitle.value = ''
    relNotes.value = ''
    relAssets.value = []
    repo.value = await getRepository(username.value, repoName.value)
  } catch (e: any) {
    relError.value = typeof e === 'string' ? e : e?.message || 'Failed to create release'
  } finally {
    relSaving.value = false
  }
}

async function downloadAsset(asset: DevAssetInfo) {
  if (downloading.value[asset.cid]) return
  downloading.value[asset.cid] = true
  try {
    // v1.63.2 БЫСТРЫЙ путь: `quickAddMagnet` возвращает info_hash за
    // миллисекунды (regex по magnet-URI, без ожидания метаданных из DHT).
    // Торрент сразу появляется в списке активных, метаданные и файлы
    // подтянутся в фоне. Ранее `parseMagnetLink` использовал
    // list_only:true и висел до 10+ минут на холодном DHT — пользователь
    // видел «клик не сработал».
    if (asset.magnet) {
      const { infoHash } = await quickAddMagnet(asset.magnet)
      assetInfoHash.value[asset.cid] = (infoHash || '').toUpperCase()
      toasts.push('dev://', `⇩ «${asset.name}» added — download in progress`, 'in', 4500)
    } else {
      await startFileDownload(asset.cid)
      toasts.push('dev://', `Download of «${asset.name}» started (f://) — see File Exchange`, 'in')
    }
  } catch (e: any) {
    toasts.push('dev://', typeof e === 'string' ? e : 'Failed to start download', 'error')
  } finally {
    downloading.value[asset.cid] = false
  }
}

// v1.63.3 Copy Magnet: копирует полный подписанный magnet-URI ассета
// в буфер обмена. Тик через 1.5 сек возвращает надпись обратно.
const copiedMagnet = ref<Record<string, boolean>>({})
async function copyMagnet(asset: DevAssetInfo) {
  if (!asset.magnet) return
  try {
    await navigator.clipboard.writeText(asset.magnet)
    copiedMagnet.value[asset.cid] = true
    setTimeout(() => {
      copiedMagnet.value[asset.cid] = false
    }, 1500)
    toasts.push('dev://', `Magnet copied to clipboard`, 'in', 2500)
  } catch {
    toasts.push('dev://', 'Failed to copy — clipboard blocked', 'error')
  }
}

// ---- Live progress bars for release assets (torrent-progress subscription) ----
// Держим два мапа:
//  1) magnet → infoHash (для быстрого lookup из шаблона)
//  2) infoHash → TorrentStatus (обновляется раз в секунду)
// Извлекаем btih параметр из magnet-URI без сетевого вызова, чтобы прогресс
// работал даже для тех торрентов, которые СИД уже раздаёт (у автора релиза),
// и для ранее скачанных (которые librqbit-сессия помнит после перезапуска).
const assetInfoHash = ref<Record<string, string>>({})
const torrentByHash = ref<Record<string, TorrentStatus>>({})
let unlistenTorrent: (() => void) | null = null
let unlistenSync: (() => void) | null = null
// первый сетевой поиск ещё идёт → показываем «Поиск в сети…», не «not found»
const searching = ref(false)

function infoHashFromMagnet(magnet: string): string {
  const m = /xt=urn:btih:([a-fA-F0-9]{40})/i.exec(magnet || '')
  return m ? m[1].toUpperCase() : ''
}

function assetStatus(asset: DevAssetInfo): TorrentStatus | null {
  const cached = assetInfoHash.value[asset.cid]
  const ih = (cached || infoHashFromMagnet(asset.magnet || '')).toUpperCase()
  if (!ih) return null
  if (!cached) assetInfoHash.value[asset.cid] = ih
  return torrentByHash.value[ih] || null
}

function fmtSpeed(bps: number): string {
  if (!bps) return ''
  if (bps < 1024) return `${bps} B/s`
  if (bps < 1024 * 1024) return `${(bps / 1024).toFixed(1)} KB/s`
  return `${(bps / 1024 / 1024).toFixed(1)} MB/s`
}

// ---- Edit / Delete release (owner-only) ----------------------------------
const editingTag = ref<string | null>(null)
const editTitle = ref('')
const editNotes = ref('')
const editSaving = ref(false)
const editError = ref('')

function startEditRelease(rl: DevReleaseInfo) {
  editingTag.value = rl.tag
  editTitle.value = rl.title || ''
  editNotes.value = rl.notes || ''
  editError.value = ''
}

function cancelEditRelease() {
  editingTag.value = null
  editTitle.value = ''
  editNotes.value = ''
  editError.value = ''
}

async function saveEditRelease() {
  if (!editingTag.value || editSaving.value) return
  editSaving.value = true
  editError.value = ''
  try {
    await devhubEditRelease(repoName.value, editingTag.value, editTitle.value, editNotes.value)
    toasts.push('dev://', `Release ${editingTag.value} updated`, 'in')
    cancelEditRelease()
    repo.value = await getRepository(username.value, repoName.value)
  } catch (e: any) {
    editError.value = typeof e === 'string' ? e : e?.message || 'Failed to update release'
  } finally {
    editSaving.value = false
  }
}

// Двухступенчатое удаление: клик на «✕ Delete» → появляется inline-confirm
// «Really delete? [Yes / Cancel]». Без модалок, GitHub-style.
const confirmDeleteTag = ref<string | null>(null)
const deletingRelease = ref(false)
function askDeleteRelease(tag: string) {
  confirmDeleteTag.value = tag
}
function cancelDeleteRelease() {
  confirmDeleteTag.value = null
}
async function confirmDelete(tag: string) {
  if (deletingRelease.value) return
  deletingRelease.value = true
  try {
    await devhubDeleteRelease(repoName.value, tag)
    toasts.push('dev://', `Release ${tag} deleted`, 'in')
    confirmDeleteTag.value = null
    repo.value = await getRepository(username.value, repoName.value)
  } catch (e: any) {
    toasts.push('dev://', typeof e === 'string' ? e : 'Failed to delete release', 'error')
  } finally {
    deletingRelease.value = false
  }
}

function fmtDate(secs: number): string {
  return new Date(secs * 1000).toLocaleDateString()
}

// ---- копирование dev://-ссылки и команды git clone ----
const devUrl = computed(() => `dev://${username.value}/${repoName.value}`)
async function copyText(text: string) {
  let ok = false
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text)
      ok = true
    }
  } catch {
    ok = false
  }
  if (!ok) {
    try {
      const ta = document.createElement('textarea')
      ta.value = text
      ta.style.position = 'fixed'
      ta.style.opacity = '0'
      document.body.appendChild(ta)
      ta.focus()
      ta.select()
      ok = document.execCommand('copy')
      document.body.removeChild(ta)
    } catch {
      ok = false
    }
  }
  toasts.push('dev://', ok ? `Copied: ${text}` : 'Copy failed', ok ? 'in' : 'error')
}
const copyLink = () => copyText(devUrl.value)
const copyClone = () => copyText(`git clone ${devUrl.value}`)

// ---- ⭐ подписанные стары ----
const starred = ref(false)
const starBusy = ref(false)
async function toggleStar() {
  if (starBusy.value || !repo.value) return
  starBusy.value = true
  try {
    starred.value = await devhubToggleStar(username.value, repoName.value)
    repo.value.stars = Math.max(0, repo.value.stars + (starred.value ? 1 : -1))
    toasts.push('dev://', starred.value ? '⭐ Star signed and published' : 'Star removed', 'in')
  } catch (e: any) {
    toasts.push('dev://', typeof e === 'string' ? e : 'Failed to star', 'error')
  } finally {
    starBusy.value = false
  }
}

async function load() {
  loading.value = true
  error.value = ''
  curPath.value = ''
  try {
    repo.value = await getRepository(username.value, repoName.value)
    searching.value = false
    devhubStarredByMe()
      .then((list) => (starred.value = list.includes(`${username.value}/${repoName.value}`)))
      .catch(() => {})
    filesLocal.value = await devhubFilesLocal(username.value, repoName.value)
    await loadFiles()
  } catch (e: any) {
    repo.value = null
    // Промах локального кэша: бэкенд параллельно синкает автора из сети
    // (DHT → биконы → swarm) и эмитит `devhub-synced`. Пока идёт первый
    // поиск — показываем «Поиск в сети…» вместо «not found».
    if (e?.kind === 'not_found' && isTauri && !searchedOnce) {
      searchedOnce = true
      searching.value = true
      // страховка: если синк так и не пришёл — через 12 c показываем ошибку
      window.setTimeout(() => {
        if (searching.value && !repo.value) {
          searching.value = false
          error.value = `Repository ${username.value}/${repoName.value} not found in the network.`
        }
      }, 12000)
    } else {
      searching.value = false
      error.value = e?.kind === 'not_found'
        ? `Repository ${username.value}/${repoName.value} not found in the network.`
        : 'Repository not found in the network.'
    }
  } finally {
    loading.value = false
  }
}
// один «мягкий» проход «Поиск в сети…» на каждую пару username/repo
let searchedOnce = false
onMounted(async () => {
  myUsername.value = (await getMyUsername()) || ''
  await load()
  // Live-прогресс релизных ассетов: слушаем общее событие `torrent-progress`
  // и индексируем по infoHash. Раз в секунду обновляем `torrentByHash`,
  // шаблон читает статус через `assetStatus(asset)`.
  try {
    unlistenTorrent = await onTorrentProgress((list) => {
      const next: Record<string, TorrentStatus> = {}
      for (const s of list) next[(s.infoHash || '').toUpperCase()] = s
      torrentByHash.value = next
    })
  } catch {
    // web-fallback: события нет → карточки просто без прогресса.
  }
  if (isTauri) {
    // Фоновый P2P-синк чужого индекса завершился → перезагружаем репозиторий.
    try {
      unlistenSync = await appListen<{ username: string }>('devhub-synced', (p) => {
        if (p.username === username.value && !repo.value) load()
      })
    } catch {
      // событие недоступно — не критично
    }
  }
})
onBeforeUnmount(() => {
  try { unlistenTorrent?.() } catch { /* ignore */ }
  unlistenTorrent = null
  try { unlistenSync?.() } catch { /* ignore */ }
  unlistenSync = null
})
watch([username, repoName], () => {
  searchedOnce = false
  load()
})
</script>

<template>
  <div class="dev-repo" data-testid="dev-repo-view">
    <button class="back" @click="router.push({ name: 'dev-user', params: { username } })" data-testid="dev-repo-back">
      ‹ dev://{{ username }}
    </button>

    <p v-if="loading" class="dim" data-testid="dev-repo-loading">Resolving repository…</p>
    <p v-else-if="searching" class="dim" data-testid="dev-repo-searching">
      <span class="dr-spin" aria-hidden="true"></span>
      Searching the network for {{ username }}/{{ repoName }}…
    </p>
    <p v-else-if="error" class="err" data-testid="dev-repo-error">{{ error }}</p>

    <template v-else-if="repo">
      <header class="dr-head" data-testid="dev-repo-header">
        <div class="dr-head-top">
          <h1 class="dr-title mono">
            <UserAvatar v-if="repo.address" :address="repo.address" :size="26" class="dr-uav" />
            <span class="at" @click="router.push({ name: 'dev-user', params: { username } })">@{{ username }}</span>/{{ repo.name }}
            <span class="dr-pub">Public</span>
          </h1>
          <div class="dr-actions">
            <button
              class="dr-star"
              :class="{ on: starred }"
              :disabled="starBusy"
              @click="toggleStar"
              :title="starred ? 'Remove star' : 'Sign star with wallet key'"
              data-testid="dev-repo-star-btn"
            >
              {{ starred ? '★' : '☆' }} {{ starred ? 'Starred' : 'Star' }}
              <span class="dr-star-count">{{ repo.stars }}</span>
            </button>
            <button
              v-if="!isOwn && myUsername"
              class="dr-fork"
              :disabled="forking"
              @click="forkRepo"
              title="Copy this repository to your account (fork) — then edit it as your own"
              data-testid="dev-repo-fork-btn"
            >⑂ {{ forking ? 'Forking…' : 'Fork' }}</button>
            <button class="dr-copy" :title="`git clone ${devUrl}`" @click="copyClone" data-testid="dev-repo-copy-clone">
              <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <polyline points="4 17 10 11 4 5"></polyline>
                <line x1="12" y1="19" x2="20" y2="19"></line>
              </svg>
              git clone
            </button>
            <button class="dr-copy" @click="copyLink" :title="devUrl" data-testid="dev-repo-copy-link">
              <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect>
                <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
              </svg>
              dev:// link
            </button>
          </div>
        </div>
        <p v-if="repo.forkedFrom" class="dr-forked" data-testid="dev-repo-forked-from">
          ⑂ forked from <span class="dr-forked-link mono" @click="openForkSource">{{ repo.forkedFrom }}</span>
          <button
            v-if="isOwn"
            class="dr-sync-fork"
            :disabled="syncingFork"
            @click="syncFork"
            title="Pull fresh commits from the original repository"
            data-testid="dev-repo-sync-fork-btn"
          >{{ syncingFork ? 'Syncing…' : '⇅ Sync fork' }}</button>
        </p>
        <p v-if="repo.description" class="dr-desc dim">{{ repo.description }}</p>
        <div class="dr-hash-row">
          <code class="dr-hash" :title="repo.hash">⬡ {{ repo.hash.slice(0, 16) }}…</code>
          <code
            v-if="headSha"
            class="dr-hash dr-sha"
            :title="`Latest commit: ${headSha} (click to copy)`"
            @click="copySha"
            data-testid="dev-repo-head-sha"
          >⑂ {{ repo.defaultBranch }} · {{ headSha.slice(0, 7) }}</code>
        </div>
      </header>

      <div class="dr-stats" data-testid="dev-repo-stats">
        <div class="dr-stat"><span class="v">★ {{ repo.stars }}</span><span class="k dim">stars</span></div>
        <div class="dr-stat"><span class="v">{{ repo.commits }}</span><span class="k dim">commits</span></div>
        <div class="dr-stat"><span class="v">{{ repo.branches.length }}</span><span class="k dim">branches</span></div>
        <div class="dr-stat"><span class="v seed">◉ {{ repo.seeders }}</span><span class="k dim">seeders</span></div>
        <div class="dr-stat" v-if="repo.language"><span class="v lang">{{ repo.language }}</span><span class="k dim">language</span></div>
        <div class="dr-stat" v-if="repo.license"><span class="v">{{ repo.license }}</span><span class="k dim">license</span></div>
      </div>

      <nav class="dr-tabs" data-testid="dev-repo-tabs">
        <button
          v-for="tb in visibleTabs"
          :key="tb.id"
          class="dr-tab"
          :class="{ active: tab === tb.id }"
          @click="setTab(tb.id)"
          :data-testid="`dev-repo-tab-${tb.id}`"
        >
          {{ tb.label }}
          <span v-if="tb.id === 'releases' && repo.releases.length" class="dr-tab-count">{{ repo.releases.length }}</span>
        </button>
      </nav>

      <!-- ======== Код ======== -->
      <section v-if="tab === 'code'" class="dr-panel" data-testid="dev-repo-panel-code">
        <div v-if="filesErr" class="stub">
          <p class="stub-title mono">// Files not synchronized</p>
          <p class="dim">{{ filesErr }}</p>
          <button class="dr-sync" :disabled="syncing" @click="syncFiles" data-testid="dev-repo-sync-btn-stub">
            {{ syncing ? 'Downloading from seeders…' : '⇩ Download files from seeders (P2P)' }}
          </button>
        </div>

        <template v-else>
          <!-- чужой репо: список файлов из подписанного анонса, контент — по клику -->
          <div v-if="!filesLocal" class="dr-remote-note" data-testid="dev-repo-remote-note">
            <span class="dim">File list is served by the author's signed P2P announcement. Content streams from seeders when you open a file.</span>
            <button class="dr-sync" :disabled="syncing" @click="syncFiles" data-testid="dev-repo-sync-btn">
              {{ syncing ? 'Downloading…' : '⇩ Sync (P2P)' }}
            </button>
          </div>
          <!-- хлебные крошки -->
          <div class="dr-crumbs" data-testid="dev-repo-crumbs">
            <span class="crumb mono" :class="{ link: curPath }" @click="goCrumb('')">{{ repo.name }}</span>
            <template v-for="c in crumbs" :key="c.path">
              <span class="dim">/</span>
              <span class="crumb mono" :class="{ link: c.path !== curPath }" @click="goCrumb(c.path)">{{ c.name }}</span>
            </template>
            <span class="dr-branch mono" :title="headSha ? `HEAD: ${headSha}` : ''">
              ⑂ {{ repo.defaultBranch }}<template v-if="headSha"> @ {{ headSha.slice(0, 7) }}</template>
            </span>
          </div>

          <!-- просмотр файла -->
          <div v-if="fileView" class="dr-file-view" data-testid="dev-repo-file-view">
            <div class="dr-file-head">
              <span class="mono">{{ fileViewName }}</span>
              <span class="dim">{{ fmtSize(fileView.size) }}<template v-if="fileView.truncated"> · showing first 2 MB</template></span>
              <button class="dr-file-close" @click="fileView = null" data-testid="dev-repo-file-close">✕</button>
            </div>
            <p v-if="fileView.binary" class="dim pad">Binary file — preview unavailable.</p>
            <div v-else-if="fileViewName.toLowerCase().endsWith('.md')" class="md pad" v-html="renderMd(fileView.content)" />
            <pre v-else class="dr-code mono">{{ fileView.content }}</pre>
          </div>

          <!-- список файлов -->
          <div v-else class="dr-files" data-testid="dev-repo-files">
            <button
              v-for="f in files"
              :key="f.path"
              class="dr-file-row"
              @click="openEntry(f)"
              :data-testid="`dev-repo-file-${f.name}`"
            >
              <span class="dr-file-icon">{{ f.isDir ? '📁' : '📄' }}</span>
              <span class="dr-file-name mono">{{ f.name }}</span>
              <span class="dr-file-size dim">{{ f.isDir ? '—' : fmtSize(f.size) }}</span>
            </button>
            <p v-if="!files.length" class="dim pad">Empty directory.</p>
          </div>

          <!-- README как на GitHub -->
          <div v-if="!fileView && readmeHtml" class="dr-readme" data-testid="dev-repo-readme">
            <div class="dr-readme-head mono dim">README.md</div>
            <div class="md pad" v-html="readmeHtml" />
          </div>
        </template>
      </section>

      <!-- ======== Релизы ======== -->
      <section v-else-if="tab === 'releases'" class="dr-panel" data-testid="dev-repo-panel-releases">
        <div v-if="isOwn" class="dr-rel-toolbar">
          <button class="dr-rel-new" @click="openReleaseForm" data-testid="dev-repo-new-release-btn">
            {{ showReleaseForm ? '✕ Cancel' : '+ Create release' }}
          </button>
        </div>

        <!-- черновик релиза (GitHub-like «Draft a new release») -->
        <div v-if="showReleaseForm" class="dr-rel-form" data-testid="dev-repo-release-form">
          <div class="dr-rel-grid">
            <div class="dr-rel-field">
              <label class="dr-rel-label">Tag *</label>
              <input v-model="relTag" class="dr-rel-input mono" placeholder="v1.0.0" maxlength="64" data-testid="dev-release-tag-input" />
            </div>
            <div class="dr-rel-field grow">
              <label class="dr-rel-label">Title</label>
              <input v-model="relTitle" class="dr-rel-input" placeholder="Release name…" maxlength="120" data-testid="dev-release-title-input" />
            </div>
          </div>
          <div class="dr-rel-field">
            <label class="dr-rel-label">Notes (markdown)</label>
            <textarea v-model="relNotes" class="dr-rel-input dr-rel-notes-input" rows="4" placeholder="What's new in this release…" data-testid="dev-release-notes-input" />
          </div>
          <div class="dr-rel-field">
            <label class="dr-rel-label">Release assets — drop binaries here</label>
            <!-- DropZone: перетащите или кликните, чтобы выбрать. Загруженные
                 файлы автоматически тегируются `dev/<repo>@<ver>/<os>` и
                 публикуются как бесплатные (без платы/приватности). Заведомая
                 привязка к текущему релиз-тегу — в момент загрузки, поэтому
                 сначала укажите Tag выше, потом бросайте бинарники. -->
            <div
              class="dr-rel-drop"
              :class="{ active: relDragActive }"
              @click="pickAndUploadAsset"
              @dragenter="onRelDragEnter"
              @dragover="onRelDragOver"
              @dragleave="onRelDragLeave"
              @drop="onRelDrop"
              data-testid="dev-release-dropzone"
            >
              <span class="dr-rel-drop-ico" aria-hidden="true">⇩</span>
              <div class="dr-rel-drop-text">
                <b>Drop binaries here</b>
                <span class="dim">or click to browse · exe · dmg · deb · apk · zip…</span>
              </div>
            </div>

            <!-- Активные загрузки (progress placeholder — Rust ещё не шлёт
                 промежуточные события в add_file, пока «indeterminate»). -->
            <div v-if="relUploading.length" class="dr-rel-uploading">
              <div
                v-for="(u, idx) in relUploading"
                :key="idx"
                class="dr-rel-uprow"
                :class="{ done: u.done }"
                :data-testid="`dev-release-uploading-${idx}`"
              >
                <span class="mono">{{ u.name }}</span>
                <span class="dr-rel-upstate dim">{{ u.done ? '✓ done' : '⟳ uploading…' }}</span>
              </div>
            </div>

            <!-- Уже добавленные в релиз ассеты (только те, что мы залили этой формой). -->
            <div v-if="relAssets.length" class="dr-rel-assets" data-testid="dev-release-assets">
              <div
                v-for="a in relAssets"
                :key="a.cid"
                class="dr-rel-asset-row"
                :data-testid="`dev-release-asset-${a.name}`"
              >
                <div class="dr-rel-asset">
                  <span class="mono dr-rel-aname">{{ a.name }}</span>
                  <span
                    v-if="osLabel(osFromFilename(a.name))"
                    class="dr-rel-osbadge"
                    :data-os="osFromFilename(a.name)"
                  >{{ osLabel(osFromFilename(a.name)) }}</span>
                  <span class="dim">{{ fmtSize(a.size) }}</span>
                  <button
                    class="dr-rel-rm"
                    @click.stop="removeAsset(a.cid)"
                    :title="'Remove from release'"
                    :data-testid="`dev-release-remove-${a.cid}`"
                  >✕</button>
                </div>
                <!-- v1.63.2 optional WebSeed mirror URL: HTTP(S) fallback source
                     when swarm has 0 peers. Signed inside the release manifest,
                     so trust is anchored in the author key. -->
                <div class="dr-rel-mirror">
                  <label class="dr-rel-mirror-lbl">
                    <span class="ic" title="WebSeed BEP-19 — HTTP mirror for zero-peer availability">🌐</span>
                    <input
                      v-model="a.mirror"
                      class="dr-rel-mirror-input"
                      placeholder="Optional mirror URL — https://github.com/…/releases/…"
                      spellcheck="false"
                      :data-testid="`dev-release-mirror-${a.cid}`"
                    />
                  </label>
                </div>
              </div>
            </div>
            <p v-else-if="!relUploading.length" class="dim dr-rel-empty">No assets yet — drop the binaries you want shipped with this release.</p>
          </div>
          <p v-if="relError" class="dr-rel-err" data-testid="dev-release-error">{{ relError }}</p>
          <button class="dr-rel-publish" :disabled="!relTag.trim() || relSaving" @click="submitRelease" data-testid="dev-release-publish-btn">
            {{ relSaving ? 'Signing…' : `Publish release${relAssets.length ? ` (${relAssets.length} asset${relAssets.length === 1 ? '' : 's'})` : ''}` }}
          </button>
        </div>

        <div v-if="!repo.releases.length && !showReleaseForm" class="stub" data-testid="dev-repo-releases-empty">
          <p class="stub-title mono">// Releases</p>
          <p class="dim">
            No releases yet. Release assets (exe, dmg, apk, deb…) are published to the file exchange and
            served via <b>f://</b> — they are <b>never downloaded automatically</b>, only on click.
          </p>
        </div>
        <div v-else class="dr-releases">
          <article v-for="rl in repo.releases" :key="rl.tag" class="dr-release" :data-testid="`dev-repo-release-${rl.tag}`">
            <header class="dr-rel-head">
              <h3 class="dr-rel-title">{{ rl.title || rl.tag }}</h3>
              <span class="dr-rel-tag mono">🏷 {{ rl.tag }}</span>
              <span class="dim">{{ fmtDate(rl.created) }}</span>
              <!-- Owner controls: Edit / Delete (двухступенчатое подтверждение) -->
              <div v-if="isOwn" class="dr-rel-owner">
                <button
                  v-if="editingTag !== rl.tag && confirmDeleteTag !== rl.tag"
                  class="dr-rel-edit"
                  @click="startEditRelease(rl)"
                  :data-testid="`dev-repo-release-edit-${rl.tag}`"
                  title="Edit release metadata (title, notes)"
                >✎ Edit</button>
                <button
                  v-if="editingTag !== rl.tag && confirmDeleteTag !== rl.tag"
                  class="dr-rel-del"
                  @click="askDeleteRelease(rl.tag)"
                  :data-testid="`dev-repo-release-delete-${rl.tag}`"
                  title="Delete this release from the signed index"
                >✕ Delete</button>
                <template v-if="confirmDeleteTag === rl.tag">
                  <span class="dr-rel-confirm">Delete «{{ rl.tag }}»?</span>
                  <button
                    class="dr-rel-del-yes"
                    :disabled="deletingRelease"
                    @click="confirmDelete(rl.tag)"
                    :data-testid="`dev-repo-release-delete-yes-${rl.tag}`"
                  >{{ deletingRelease ? '…' : 'Yes, delete' }}</button>
                  <button
                    class="dr-rel-del-no"
                    :disabled="deletingRelease"
                    @click="cancelDeleteRelease"
                    :data-testid="`dev-repo-release-delete-no-${rl.tag}`"
                  >Cancel</button>
                </template>
              </div>
            </header>

            <!-- Inline edit form (заменяет notes, когда открыт редактор) -->
            <div v-if="editingTag === rl.tag" class="dr-rel-edit-form" :data-testid="`dev-repo-release-edit-form-${rl.tag}`">
              <label class="dim">Title</label>
              <input
                v-model="editTitle"
                class="dr-rel-input"
                maxlength="120"
                placeholder="Release name…"
                :data-testid="`dev-release-edit-title-${rl.tag}`"
              />
              <label class="dim">Release notes (Markdown)</label>
              <textarea
                v-model="editNotes"
                class="dr-rel-input dr-rel-notes-input"
                rows="5"
                placeholder="What's new in this release…"
                :data-testid="`dev-release-edit-notes-${rl.tag}`"
              />
              <p v-if="editError" class="dr-rel-err">{{ editError }}</p>
              <div class="dr-rel-edit-actions">
                <button
                  class="dr-rel-publish"
                  :disabled="editSaving"
                  @click="saveEditRelease"
                  :data-testid="`dev-release-edit-save-${rl.tag}`"
                >{{ editSaving ? 'Signing…' : 'Save changes' }}</button>
                <button
                  class="dr-rel-edit-cancel"
                  :disabled="editSaving"
                  @click="cancelEditRelease"
                  :data-testid="`dev-release-edit-cancel-${rl.tag}`"
                >Cancel</button>
              </div>
            </div>
            <div v-else-if="rl.notes" class="md dr-rel-notes" v-html="renderMd(rl.notes)" />

            <div v-if="rl.assets.length" class="dr-assets">
              <div class="dr-assets-head dim">Assets · {{ rl.assets.length }}</div>
              <div v-for="a in rl.assets" :key="a.cid" class="dr-asset">
                <span class="mono dr-asset-name">{{ a.name }}</span>
                <span class="dim">{{ fmtSize(a.size) }}</span>
                <!-- v1.63.2 mirror badge (BEP-19 WebSeed): показываем, когда
                     автор указал HTTP-mirror для 100% availability. -->
                <a
                  v-if="a.mirror"
                  :href="a.mirror"
                  target="_blank"
                  rel="noopener noreferrer"
                  class="dr-asset-mirror"
                  :data-testid="`dev-repo-asset-mirror-${a.name}`"
                  :title="`WebSeed mirror: ${a.mirror}`"
                >🌐 mirror</a>
                <!-- Live progress: если торрент активен, показываем прогресс-бар вместо кнопки -->
                <template v-if="assetStatus(a) as any">
                  <div
                    class="dr-asset-progress"
                    :class="{ done: (assetStatus(a) as any).state === 'Seeding' || (assetStatus(a) as any).progress >= 1 }"
                    :data-testid="`dev-repo-asset-progress-${a.name}`"
                  >
                    <div class="dr-asset-bar">
                      <div class="dr-asset-bar-fill" :style="{ width: (((assetStatus(a) as any).progress || 0) * 100).toFixed(1) + '%' }" />
                    </div>
                    <span class="dr-asset-pct mono">
                      {{ ((((assetStatus(a) as any).progress || 0) * 100).toFixed(0)) }}%<template v-if="(assetStatus(a) as any).downloadSpeedBytes > 0"> · {{ fmtSpeed((assetStatus(a) as any).downloadSpeedBytes) }}</template>
                    </span>
                    <span v-if="(assetStatus(a) as any).state === 'Seeding'" class="dr-asset-seed dim" title="You are seeding this asset back to the swarm">↑ seeding</span>
                    <span v-else-if="(assetStatus(a) as any).state === 'Paused'" class="dr-asset-seed dim">⏸ paused</span>
                    <span v-else-if="(assetStatus(a) as any).state === 'Error'" class="dr-asset-seed err">! error</span>
                  </div>
                </template>
                <button
                  v-else
                  class="dr-asset-dl"
                  :disabled="downloading[a.cid]"
                  @click="downloadAsset(a)"
                  :data-testid="`dev-repo-asset-dl-${a.name}`"
                  :title="a.magnet ? 'Download via BitTorrent (peer-to-peer swarm)' : 'Download via f:// (iroh, legacy)'"
                >{{ downloading[a.cid] ? '…' : (a.magnet ? '⇩ Download (torrent)' : '⇩ Download (f://)') }}</button>
                <!-- v1.63.3: Copy Magnet — быстрый способ поделиться торрентом
                     (в чат, в другой BT-клиент, для мониторинга). Показываем
                     только для v1.62+ ассетов, у которых есть подписанный
                     magnet-URI. -->
                <button
                  v-if="a.magnet"
                  class="dr-asset-magnet"
                  :class="{ done: copiedMagnet[a.cid] }"
                  @click="copyMagnet(a)"
                  :data-testid="`dev-repo-asset-copy-magnet-${a.name}`"
                  :title="'Copy magnet URI to clipboard'"
                >{{ copiedMagnet[a.cid] ? '✓ Copied' : '⌘ Copy Magnet' }}</button>
              </div>
            </div>
          </article>
        </div>
      </section>

      <!-- ======== Ишью / Вики (следующие шаги) ======== -->
      <!-- ======== Коммиты ======== -->
      <section v-else-if="tab === 'commits'" class="dr-panel" data-testid="dev-repo-panel-commits">
        <div v-if="!repo.log || !repo.log.length" class="stub">
          <p class="stub-title mono">// Commit history</p>
          <p class="dim">History appears after the first git push into this repository (the log ships inside the author's signed index).</p>
        </div>
        <div v-else class="dr-commits" data-testid="dev-repo-commits-list">
          <div v-for="(c, i) in repo.log" :key="c.sha">
            <div
              class="dr-commit"
              :data-testid="`dev-repo-commit-${i}`"
              @click="toggleDiff(c.sha)"
            >
              <div class="dr-commit-main">
                <span class="dr-commit-msg">{{ c.message || '(no message)' }}</span>
                <span class="dr-commit-meta dim">
                  @{{ c.author }}<template v-if="c.date"> · {{ fmtCommitDate(c.date) }}</template>
                </span>
              </div>
              <button
                class="dr-commit-sha mono"
                :title="`${c.sha} (click to copy)`"
                @click.stop="copyCommitSha(c.sha)"
                :data-testid="`dev-repo-commit-sha-${i}`"
              >{{ c.sha.slice(0, 7) }}</button>
            </div>
            <div v-if="openDiff[c.sha]" class="dr-diff" :data-testid="`dev-repo-commit-diff-${i}`">
              <p v-if="diffLoading[c.sha]" class="dim dr-diff-status">Loading diff from bundle…</p>
              <p v-else-if="diffErr[c.sha]" class="dr-diff-err">{{ diffErr[c.sha] }}</p>
              <pre v-else class="dr-diff-pre mono"><span
                v-for="(l, li) in diffCache[c.sha]"
                :key="li"
                class="dl"
                :class="diffLineClass(l)"
              >{{ l }}
</span></pre>
            </div>
          </div>
        </div>
      </section>

      <!-- ======== Pull Requests ======== -->
      <section v-else-if="tab === 'prs'" class="dr-panel" data-testid="dev-repo-panel-prs">
        <DevPrPanel :username="username" :repo="repoName" :is-own="isOwn" :head-sha="headSha" />
      </section>

      <section v-else-if="tab === 'issues'" class="dr-panel" data-testid="dev-repo-panel-issues">
        <div class="stub">
          <p class="stub-title mono">// Decentralized issues</p>
          <p class="dim">Replicated issue tracker over signed P2P records — in development.</p>
        </div>
      </section>
      <section v-else-if="tab === 'wiki'" class="dr-panel" data-testid="dev-repo-panel-wiki">
        <div class="stub">
          <p class="stub-title mono">// Project wiki</p>
          <p class="dim">dev://-served documentation with local AI translation — in development.</p>
        </div>
      </section>

      <!-- ======== Настройки (только владелец) ======== -->
      <section
        v-else-if="tab === 'settings' && isOwn"
        class="dr-panel"
        data-testid="dev-repo-panel-settings"
      >
        <div class="dr-danger">
          <p class="dr-danger-title">Danger zone</p>
          <p class="dim dr-danger-text">
            Deleting the repository wipes it from disk and from your signed index — at the next
            sync it disappears from every peer. This action is irreversible.
          </p>
          <div class="dr-danger-row">
            <input
              v-model="delConfirm"
              class="dr-danger-inp mono"
              :placeholder="`Type «${repoName}» to confirm`"
              data-testid="dev-repo-delete-confirm-input"
            />
            <button
              class="dr-danger-btn"
              :disabled="deleting || delConfirm !== repoName"
              @click="deleteRepo"
              data-testid="dev-repo-delete-btn"
            >{{ deleting ? 'Deleting…' : '🗑 Delete repository' }}</button>
          </div>
        </div>
      </section>
    </template>
  </div>
</template>

<style scoped>
.dev-repo { padding: 28px 32px; max-width: 1000px; margin: 0 auto; height: 100%; overflow-y: auto; }
.mono { font-family: var(--font-mono, monospace); }
.back { font-size: 12px; color: var(--text-dim); margin-bottom: 18px; cursor: pointer; background: none; border: none; }
.back:hover { color: var(--accent); }
.err { color: #f87171; }
.dim { color: var(--text-dim); }
.dr-spin {
  display: inline-block;
  width: 12px;
  height: 12px;
  margin-right: 8px;
  border: 2px solid var(--text-dim);
  border-top-color: var(--accent, #2dd4a7);
  border-radius: 50%;
  vertical-align: -1px;
  animation: dr-spin 0.7s linear infinite;
}
@keyframes dr-spin { to { transform: rotate(360deg); } }
.pad { padding: 14px 16px; }
.dr-head { margin-bottom: 20px; }
.dr-head-top { display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-wrap: wrap; }
.dr-actions { display: flex; gap: 8px; flex-wrap: wrap; }
.dr-star {
  display: inline-flex; align-items: center; gap: 7px;
  padding: 7px 14px; border-radius: 999px; font-size: 12px; font-weight: var(--fw-800);
  color: #fbbf24; background: transparent; border: 1px solid rgba(251,191,36,0.4);
  cursor: pointer; white-space: nowrap;
  transition: background-color 0.15s ease, color 0.15s ease, transform 0.15s ease;
}
.dr-star:hover:not(:disabled) { background: rgba(251,191,36,0.12); transform: translateY(-1px); }
.dr-star.on { background: rgba(251,191,36,0.16); }
.dr-star:disabled { opacity: 0.5; }
.dr-star-count { padding: 1px 8px; border-radius: 999px; font-size: 10px; background: rgba(251,191,36,0.15); }
.dr-copy {
  display: inline-flex; align-items: center; gap: 7px;
  padding: 7px 14px; border-radius: 999px; font-size: 12px; font-weight: var(--fw-700);
  color: var(--accent); background: transparent; border: 1px solid var(--border-accent);
  cursor: pointer; white-space: nowrap;
  transition: color 0.15s ease, background-color 0.15s ease, transform 0.15s ease;
}
.dr-copy:hover { color: var(--bg); background: var(--accent); transform: translateY(-1px); }
.dr-fork {
  display: inline-flex; align-items: center; gap: 7px;
  padding: 7px 14px; border-radius: 999px; font-size: 12px; font-weight: var(--fw-800);
  color: #34d399; background: transparent; border: 1px solid rgba(52,211,153,0.45);
  cursor: pointer; white-space: nowrap;
  transition: background-color 0.15s ease, transform 0.15s ease;
}
.dr-fork:hover:not(:disabled) { background: rgba(52,211,153,0.12); transform: translateY(-1px); }
.dr-fork:disabled { opacity: 0.5; }
.dr-forked { font-size: 12px; color: var(--text-dim); margin: 2px 0 4px; display: inline-flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.dr-forked-link { color: var(--secondary); cursor: pointer; }
.dr-forked-link:hover { color: var(--accent); text-decoration: underline; }
.dr-sync-fork {
  padding: 3px 10px; border-radius: 999px; font-size: 11px; font-weight: var(--fw-700);
  color: #60a5fa; background: transparent; border: 1px solid rgba(96,165,250,0.45);
  cursor: pointer; white-space: nowrap; transition: background-color 0.15s ease, transform 0.15s ease;
}
.dr-sync-fork:hover:not(:disabled) { background: rgba(96,165,250,0.12); transform: translateY(-1px); }
.dr-sync-fork:disabled { opacity: 0.5; cursor: default; }
.dr-remote-note {
  display: flex; align-items: center; justify-content: space-between; gap: 14px; flex-wrap: wrap;
  padding: 10px 14px; margin-bottom: 12px; border-radius: 10px;
  border: 1px dashed var(--border-accent); background: rgba(255,255,255,0.015); font-size: 12px;
}
.dr-sync {
  padding: 6px 14px; border-radius: 999px; font-size: 11px; font-weight: var(--fw-700);
  color: var(--accent); background: transparent; border: 1px solid var(--border-accent);
  cursor: pointer; white-space: nowrap; transition: background-color 0.15s ease, color 0.15s ease;
}
.dr-sync:hover:not(:disabled) { color: var(--bg); background: var(--accent); }
.dr-sync:disabled { opacity: 0.5; }
.stub .dr-sync { margin-top: 12px; }
.dr-title { font-size: 24px; font-weight: var(--fw-800); color: var(--accent); display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
.dr-title .at { color: var(--secondary); cursor: pointer; }
.dr-title .at:hover { text-decoration: underline; }
.dr-pub { padding: 3px 12px; border-radius: 999px; font-size: 11px; font-weight: var(--fw-700); color: #34d399; border: 1px solid rgba(52,211,153,0.4); }
.dr-desc { font-size: 13px; margin: 6px 0; }
.dr-hash-row { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
.dr-hash { font-size: 11px; color: var(--secondary); }
.dr-sha { cursor: pointer; transition: color 0.12s ease; }
.dr-sha:hover { color: var(--accent); }
.dr-commits { display: flex; flex-direction: column; }
.dr-commit {
  display: flex; align-items: center; justify-content: space-between; gap: 14px;
  padding: 10px 14px; border-bottom: 1px solid var(--border);
}
.dr-commit:last-child { border-bottom: none; }
.dr-commit:hover { background: rgba(255, 255, 255, 0.02); }
.dr-commit-main { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
.dr-commit-msg { font-size: 13px; font-weight: var(--fw-700); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dr-commit-meta { font-size: 11px; }
.dr-commit-sha {
  font-size: 11px; padding: 4px 10px; border-radius: 7px; white-space: nowrap;
  color: var(--secondary); background: rgba(255, 255, 255, 0.03); border: 1px solid var(--border);
  cursor: pointer; transition: color 0.12s ease, border-color 0.12s ease;
}
.dr-commit-sha:hover { color: var(--accent); border-color: var(--border-accent); }
.dr-commit { cursor: pointer; }
.dr-diff { border-bottom: 1px solid var(--border); background: rgba(0, 0, 0, 0.22); }
.dr-diff-status, .dr-diff-err { font-size: 12px; padding: 10px 16px; }
.dr-diff-err { color: var(--danger, #e5534b); }
.dr-diff-pre { margin: 0; padding: 10px 16px; font-size: 11.5px; line-height: 1.55; overflow-x: auto; max-height: 480px; overflow-y: auto; }
.dl { display: block; white-space: pre; }
.dl-add { color: #4cc38a; background: rgba(76, 195, 138, 0.08); }
.dl-del { color: #e5534b; background: rgba(229, 83, 75, 0.08); }
.dl-hunk { color: #58a6ff; }
.dl-head { color: var(--accent); font-weight: var(--fw-700); }
.dl-file { color: var(--secondary); }
.dr-danger { border: 1px solid rgba(229, 83, 75, 0.45); border-radius: 12px; padding: 18px; display: flex; flex-direction: column; gap: 10px; }
.dr-danger-title { font-size: 13px; font-weight: var(--fw-700); color: var(--danger, #e5534b); }
.dr-danger-text { font-size: 12px; max-width: 640px; }
.dr-danger-row { display: flex; gap: 10px; flex-wrap: wrap; }
.dr-danger-inp {
  flex: 1; min-width: 240px; padding: 9px 12px; font-size: 12px; color: inherit; border-radius: 8px;
  background: rgba(255, 255, 255, 0.03); border: 1px solid var(--border); outline: none;
}
.dr-danger-inp:focus { border-color: rgba(229, 83, 75, 0.6); }
.dr-danger-btn {
  padding: 9px 16px; font-size: 12px; font-weight: var(--fw-700); border-radius: 8px; cursor: pointer;
  color: #fff; background: rgba(229, 83, 75, 0.85); border: none; transition: opacity 0.12s ease;
}
.dr-danger-btn:disabled { opacity: 0.35; cursor: default; }
.dr-stats { display: flex; flex-wrap: wrap; gap: 22px; padding: 16px 18px; border-radius: 12px; background: rgba(255,255,255,0.02); border: 1px solid var(--border); margin-bottom: 16px; }
.dr-stat { display: flex; flex-direction: column; gap: 2px; }
.dr-stat .v { font-size: 18px; font-weight: var(--fw-800); font-variant-numeric: tabular-nums; }
.dr-stat .k { font-size: 10px; letter-spacing: 0.05em; text-transform: uppercase; }
.seed { color: #34d399; }
.lang { color: var(--secondary); }
.dr-tabs { display: flex; gap: 4px; border-bottom: 1px solid var(--border); margin-bottom: 18px; }
.dr-tab { display: inline-flex; align-items: center; gap: 7px; padding: 9px 16px; font-size: 13px; color: var(--text-dim); cursor: pointer; background: none; border: none; border-bottom: 2px solid transparent; margin-bottom: -1px; transition: color 0.15s ease, border-color 0.15s ease; }
.dr-tab:hover { color: var(--text); }
.dr-tab.active { color: var(--accent); border-bottom-color: var(--accent); font-weight: var(--fw-700); }
.dr-tab-count { padding: 1px 8px; border-radius: 999px; font-size: 10px; background: rgba(255,255,255,0.06); }
.dr-panel { min-height: 160px; }
.stub { padding: 24px; border-radius: 12px; border: 1px dashed var(--border); background: rgba(255,255,255,0.015); }
.stub-title { color: var(--accent); font-size: 13px; margin-bottom: 8px; }
.stub p { font-size: 13px; line-height: 1.6; }
.dr-crumbs { display: flex; align-items: center; gap: 8px; font-size: 13px; margin-bottom: 12px; flex-wrap: wrap; }
.crumb { color: var(--text); }
.crumb.link { color: var(--accent); cursor: pointer; }
.crumb.link:hover { text-decoration: underline; }
.dr-branch { margin-left: auto; padding: 3px 12px; border-radius: 999px; border: 1px solid var(--border); font-size: 11px; color: var(--secondary); }
.dr-files { border: 1px solid var(--border); border-radius: 12px; overflow: hidden; }
.dr-file-row {
  display: grid; grid-template-columns: 26px 1fr auto; align-items: center; gap: 10px;
  width: 100%; padding: 9px 14px; text-align: left;
  background: rgba(255,255,255,0.01); border: none; border-bottom: 1px solid var(--border);
  cursor: pointer; transition: background-color 0.12s ease;
}
.dr-file-row:last-child { border-bottom: none; }
.dr-file-row:hover { background: rgba(255,255,255,0.045); }
.dr-file-name { font-size: 13px; color: var(--text); }
.dr-file-row:hover .dr-file-name { color: var(--accent); }
.dr-file-size { font-size: 11px; }
.dr-file-icon { font-size: 13px; }
.dr-readme { margin-top: 18px; border: 1px solid var(--border); border-radius: 12px; overflow: hidden; }
.dr-readme-head { padding: 9px 16px; font-size: 12px; border-bottom: 1px solid var(--border); background: rgba(255,255,255,0.02); }
.dr-file-view { border: 1px solid var(--border); border-radius: 12px; overflow: hidden; }
.dr-file-head { display: flex; align-items: center; gap: 14px; padding: 9px 16px; font-size: 12px; border-bottom: 1px solid var(--border); background: rgba(255,255,255,0.02); }
.dr-file-close { margin-left: auto; background: none; border: none; color: var(--text-dim); cursor: pointer; font-size: 13px; }
.dr-file-close:hover { color: #f87171; }
.dr-code { padding: 14px 16px; font-size: 12px; line-height: 1.6; overflow-x: auto; white-space: pre; }
.md { font-size: 13px; line-height: 1.7; }
.md :deep(h1), .md :deep(h2), .md :deep(h3) { margin: 12px 0 6px; }
.md :deep(p) { margin: 6px 0; }
.md :deep(code) { background: rgba(255,255,255,0.06); padding: 1px 6px; border-radius: 6px; font-size: 12px; }
.md :deep(pre) { background: rgba(255,255,255,0.04); padding: 12px; border-radius: 10px; overflow-x: auto; }
.dr-releases { display: flex; flex-direction: column; gap: 16px; }
.dr-rel-toolbar { display: flex; justify-content: flex-end; margin-bottom: 14px; }
.dr-rel-new {
  padding: 8px 16px; border-radius: 10px; font-size: 13px; font-weight: var(--fw-800);
  color: #04140f; background: #34d399; border: none; cursor: pointer;
  transition: transform 0.15s ease;
}
.dr-rel-new:hover { transform: translateY(-1px); }
.dr-rel-form { padding: 18px 20px; border-radius: 12px; border: 1px solid var(--border-accent); background: rgba(255,255,255,0.02); margin-bottom: 18px; display: flex; flex-direction: column; gap: 12px; }
.dr-rel-grid { display: flex; gap: 12px; flex-wrap: wrap; }
.dr-rel-field { display: flex; flex-direction: column; gap: 6px; }
.dr-rel-field.grow { flex: 1; min-width: 220px; }
.dr-rel-label { font-size: 11px; font-weight: var(--fw-700); color: var(--secondary); }
.dr-rel-input { padding: 8px 12px; border-radius: 10px; font-size: 13px; background: var(--bg-elevated); border: 1px solid var(--border); color: var(--text); outline: none; }
.dr-rel-input:focus { border-color: var(--border-accent); }
.dr-rel-notes-input { resize: vertical; font-family: inherit; }
.dr-rel-files { display: flex; flex-direction: column; gap: 6px; max-height: 200px; overflow-y: auto; }
.dr-rel-file { display: grid; grid-template-columns: 1fr auto 22px; gap: 12px; align-items: center; padding: 7px 12px; border-radius: 8px; font-size: 12px; text-align: left; background: rgba(255,255,255,0.02); border: 1px solid var(--border); cursor: pointer; transition: border-color 0.12s ease; }
.dr-rel-file:hover { border-color: var(--border-accent); }
.dr-rel-file.picked { border-color: #34d399; }
.dr-rel-file .tick { color: #34d399; font-weight: var(--fw-800); }

/* DropZone для загрузки релизных бинарников (заменяет прежний список
   «выберите из уже опубликованных»). Клик открывает системный picker,
   drag&drop работает в десктоп-Tauri (у File есть `.path`). */
.dr-rel-drop {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 18px 20px;
  border: 1.5px dashed var(--border);
  border-radius: 12px;
  background: rgba(255,255,255,0.02);
  cursor: pointer;
  transition: border-color 0.15s ease, background-color 0.15s ease, transform 0.15s ease;
}
.dr-rel-drop:hover { border-color: #60a5fa; background: rgba(96,165,250,0.06); }
.dr-rel-drop.active { border-color: #34d399; background: rgba(52,211,153,0.08); border-style: solid; transform: scale(1.01); }
.dr-rel-drop-ico { font-size: 26px; color: #60a5fa; line-height: 1; }
.dr-rel-drop-text { display: flex; flex-direction: column; gap: 2px; }
.dr-rel-drop-text b { font-size: 13px; color: var(--text); }
.dr-rel-drop-text span { font-size: 11.5px; font-family: var(--font-mono); letter-spacing: 0.04em; }

.dr-rel-uploading { display: flex; flex-direction: column; gap: 4px; margin-top: 6px; }
.dr-rel-uprow {
  display: flex; justify-content: space-between; align-items: center;
  padding: 6px 10px; border-radius: 8px;
  background: rgba(96,165,250,0.06);
  border: 1px dashed rgba(96,165,250,0.32);
  font-size: 12px;
  transition: background-color 0.2s ease, border-color 0.2s ease;
}
.dr-rel-uprow.done {
  background: rgba(52,211,153,0.06);
  border-color: rgba(52,211,153,0.32);
}
.dr-rel-upstate { font-size: 11px; }

.dr-rel-assets { display: flex; flex-direction: column; gap: 4px; margin-top: 6px; }
.dr-rel-asset {
  display: grid;
  grid-template-columns: 1fr auto auto 24px;
  align-items: center;
  gap: 10px;
  padding: 7px 10px;
  background: rgba(52,211,153,0.05);
  border: 1px solid rgba(52,211,153,0.28);
  border-radius: 8px;
  font-size: 12px;
}
.dr-rel-aname { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dr-rel-osbadge {
  padding: 2px 8px;
  border-radius: 999px;
  font-size: 9.5px;
  font-weight: var(--fw-800);
  letter-spacing: 0.08em;
  background: rgba(0,0,0,0.28);
  border: 1px solid var(--border);
  color: var(--text-dim);
}
.dr-rel-osbadge[data-os="win"]     { color: #60a5fa; border-color: rgba(96,165,250,0.55); }
.dr-rel-osbadge[data-os="macos"]   { color: #e2e8f0; border-color: rgba(226,232,240,0.4); }
.dr-rel-osbadge[data-os="linux"]   { color: #f5b301; border-color: rgba(245,158,11,0.55); }
.dr-rel-osbadge[data-os="android"] { color: #22c55e; border-color: rgba(34,197,94,0.55); }
.dr-rel-osbadge[data-os="archive"] { color: #a78bfa; border-color: rgba(167,139,250,0.55); }
.dr-rel-rm {
  width: 22px; height: 22px;
  display: inline-flex; align-items: center; justify-content: center;
  padding: 0;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--text-dim);
  cursor: pointer;
  font-size: 12px;
  transition: color 0.15s ease, border-color 0.15s ease;
}
.dr-rel-rm:hover { color: #f87171; border-color: #f87171; }

.dr-rel-empty { font-size: 11.5px; margin: 4px 0 0; }
.dr-rel-err { color: #f87171; font-size: 12px; }
.dr-rel-publish { align-self: flex-start; padding: 9px 18px; border-radius: 10px; font-size: 13px; font-weight: var(--fw-800); color: #04140f; background: #34d399; border: none; cursor: pointer; }
.dr-rel-publish:disabled { opacity: 0.4; cursor: not-allowed; }
.dr-release { padding: 18px 20px; border-radius: 12px; border: 1px solid var(--border); background: rgba(255,255,255,0.02); }
.dr-rel-head { display: flex; align-items: center; gap: 14px; flex-wrap: wrap; margin-bottom: 8px; }
.dr-rel-title { font-size: 17px; font-weight: var(--fw-800); }
.dr-rel-tag { font-size: 12px; color: var(--secondary); }
.dr-rel-notes { margin: 8px 0; }
.dr-assets { margin-top: 12px; border: 1px solid var(--border); border-radius: 10px; overflow: hidden; }
.dr-assets-head { padding: 7px 14px; font-size: 11px; border-bottom: 1px solid var(--border); background: rgba(255,255,255,0.02); }
.dr-asset { display: grid; grid-template-columns: 1fr auto auto auto auto; align-items: center; gap: 10px; padding: 8px 14px; font-size: 12px; border-bottom: 1px solid var(--border); }
.dr-asset:last-child { border-bottom: none; }
.dr-asset-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dr-asset-mirror {
  font-size: 10px;
  color: var(--secondary);
  text-decoration: none;
  padding: 3px 9px;
  border-radius: 999px;
  border: 1px dashed var(--border);
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease;
  white-space: nowrap;
}
.dr-asset-mirror:hover { color: var(--fg); border-color: var(--border-accent); background: rgba(255,255,255,0.03); }
.dr-asset-dl { padding: 5px 13px; border-radius: 999px; font-size: 11px; font-weight: var(--fw-700); color: var(--accent); background: transparent; border: 1px solid var(--border-accent); cursor: pointer; transition: background-color 0.15s ease, color 0.15s ease; }
.dr-asset-dl:hover:not(:disabled) { color: var(--bg); background: var(--accent); }
.dr-asset-dl:disabled { opacity: 0.5; }
.dr-asset-magnet {
  padding: 5px 12px;
  border-radius: 999px;
  font-size: 11px;
  font-weight: var(--fw-600);
  color: var(--secondary);
  background: transparent;
  border: 1px solid var(--border);
  cursor: pointer;
  white-space: nowrap;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease;
}
.dr-asset-magnet:hover { color: var(--fg); border-color: var(--border-accent); }
.dr-asset-magnet.done { color: #22c55e; border-color: #22c55e; }

/* v1.63.2 mirror-URL input в форме релиза */
.dr-rel-asset-row {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding-bottom: 10px;
  border-bottom: 1px dashed var(--border);
}
.dr-rel-asset-row:last-child { border-bottom: none; padding-bottom: 0; }
.dr-rel-mirror { padding-left: 22px; }
.dr-rel-mirror-lbl {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 11px;
  color: var(--secondary);
}
.dr-rel-mirror-lbl .ic { font-size: 12px; opacity: 0.7; cursor: help; }
.dr-rel-mirror-input {
  flex: 1;
  padding: 5px 10px;
  border-radius: 6px;
  border: 1px solid var(--border);
  background: rgba(255,255,255,0.02);
  color: var(--fg);
  font-family: var(--font-mono);
  font-size: 11px;
  min-width: 0;
}
.dr-rel-mirror-input:focus {
  outline: none;
  border-color: var(--border-accent);
  background: rgba(255,255,255,0.04);
}

/* ── Live progress bar for release assets ─────────────────────────── */
.dr-asset-progress {
  display: flex; align-items: center; gap: 10px;
  min-width: 220px;
}
.dr-asset-bar {
  flex: 1;
  height: 6px;
  border-radius: 999px;
  background: rgba(255,255,255,0.06);
  border: 1px solid var(--border);
  overflow: hidden;
}
.dr-asset-bar-fill {
  height: 100%;
  background: var(--accent);
  transition: width 0.4s ease;
}
.dr-asset-progress.done .dr-asset-bar-fill {
  background: #22c55e; /* seeded — green */
}
.dr-asset-pct {
  font-size: 11px;
  color: var(--secondary);
  min-width: 90px;
  text-align: right;
  white-space: nowrap;
}
.dr-asset-seed { font-size: 10px; text-transform: uppercase; letter-spacing: 0.05em; }
.dr-asset-seed.err { color: #ef4444; }

/* ── Edit / Delete controls on release header ─────────────────────── */
.dr-rel-owner {
  margin-left: auto;
  display: flex; align-items: center; gap: 6px;
  flex-wrap: wrap;
}
.dr-rel-edit,
.dr-rel-del,
.dr-rel-del-yes,
.dr-rel-del-no,
.dr-rel-edit-cancel {
  padding: 4px 10px;
  border-radius: 999px;
  font-size: 10px;
  font-weight: var(--fw-600);
  border: 1px solid var(--border);
  background: transparent;
  color: var(--secondary);
  cursor: pointer;
  transition: background-color 0.15s ease, color 0.15s ease, border-color 0.15s ease;
}
.dr-rel-edit:hover { color: var(--accent); border-color: var(--border-accent); }
.dr-rel-del:hover  { color: #ef4444; border-color: #ef4444; }
.dr-rel-confirm { font-size: 11px; color: #ef4444; font-weight: var(--fw-700); }
.dr-rel-del-yes {
  color: #fff; background: #ef4444; border-color: #ef4444;
}
.dr-rel-del-yes:hover:not(:disabled) { background: #dc2626; }
.dr-rel-del-yes:disabled { opacity: 0.6; }
.dr-rel-del-no:hover { color: var(--fg); border-color: var(--border-accent); }

.dr-rel-edit-form {
  display: flex; flex-direction: column; gap: 6px;
  padding: 12px 0;
  border-top: 1px dashed var(--border);
  margin-top: 10px;
}
.dr-rel-edit-form label { font-size: 10px; text-transform: uppercase; letter-spacing: 0.06em; margin-top: 4px; }
.dr-rel-edit-actions { display: flex; gap: 8px; margin-top: 8px; }
</style>
