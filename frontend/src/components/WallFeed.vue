<script setup lang="ts">
/* eslint-disable no-undef */
// Децентрализованная стена (микроблог): лента постов автора, комментарии,
// модерация владельцем. Медиа/аватары тянутся лениво через iroh-blobs.
import { ref, reactive, onMounted, onUnmounted, watch, nextTick, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import EmojiPicker from 'vue3-emoji-picker'
import 'vue3-emoji-picker/css'
import { marked } from 'marked'
import DOMPurify from 'dompurify'
import { shortAddr } from '@/lib/crypto'
import UserAvatar from '@/components/UserAvatar.vue'
import { usePlayerStore, type PlayerTrack } from '@/store/player'
import { useWallCacheStore } from '@/store/wallCache'
import {
  listWall, createPost, deletePost, addComment, listComments,
  hideComment, banCommenter, pickImageFile, attachImageToPost,
  followWall, unfollowWall, isFollowingWall, openWall, onSocialEvent,
  postReactions, reactToPost, usernameOfAddress,
  pickFile, socialAttachFile, socialSaveFile, markWallRead, playlistAdd,
  albumOpenLink, albumList,
  translatorStatus, translatorDownload, translatorBootstrap, onTranslatorProgress, translatorProgress, detectLang, translateText, translatePrewarm,
  type TranslatorStatus, type TranslatorProgress,
  type WallPost, type WallComment, type ReactionSummary, type PostFile, type AlbumRef, type Album,
} from '@/lib/bridge'
import { useRouter } from 'vue-router'

const props = defineProps<{ author: string; isOwn: boolean; myAddress: string }>()
const { t, locale } = useI18n()
const player = usePlayerStore()
const cache = useWallCacheStore()
const router = useRouter()

// --- ЛОКАЛЬНЫЙ нейро-переводчик ---
const autoTranslate = ref(localStorage.getItem('sn.translateWall') === '1')
const trStatus = ref<TranslatorStatus | null>(null)
// Реальный размер пакета из конфига пресета (Bonsai-4B ≈ 0.6 ГБ, Qwen 1.5B ≈ 1.9 ГБ)
const trPackSize = computed(() => {
  const mb = trStatus.value?.model_size_mb || 0
  if (!mb) return '~1.8 GB'
  return mb >= 1000 ? `~${(mb / 1000).toFixed(1)} GB` : `~${mb} MB`
})
const trProgress = ref<TranslatorProgress | null>(null)
const trWanted = ref(false) // пользователь хочет перевод, но пакет может отсутствовать
let trUnlisten: (() => void) | null = null
// состояние перевода по id (поста/комментария)
const tr = reactive<Record<string, { text: string; loading: boolean; shown: boolean; err?: string }>>({})

function uiLang(): string {
  return (locale.value || 'en').toString().slice(0, 2)
}
async function refreshTrStatus() {
  trStatus.value = await translatorStatus()
}
async function startTrDownload() {
  trWanted.value = true
  try {
    await translatorBootstrap()
    await refreshTrStatus()
  } catch (e: any) {
    flash(String(e?.message || e))
  }
}
async function doTranslate(id: string, text: string) {
  if (!text || tr[id]?.loading) return
  if (!trStatus.value?.installed) {
    // пакета нет — запускаем фоновую загрузку модели и выходим (прогресс покажется)
    await startTrDownload()
    if (!trStatus.value?.installed) return
  }
  tr[id] = { text: tr[id]?.text || '', loading: true, shown: true }
  try {
    const out = await translateText(props.author, id, text, uiLang())
    tr[id] = { text: out, loading: false, shown: true }
  } catch (e: any) {
    tr[id] = { text: '', loading: false, shown: false, err: String(e?.message || e) }
    flash(tr[id].err || 'translate error')
  }
}
function hideTranslation(id: string) {
  if (tr[id]) tr[id].shown = false
}
function showTranslation(id: string) {
  if (tr[id]?.text) tr[id].shown = true
}
async function toggleAutoTranslate() {
  autoTranslate.value = !autoTranslate.value
  localStorage.setItem('sn.translateWall', autoTranslate.value ? '1' : '0')
  if (autoTranslate.value) {
    trWanted.value = true
    await refreshTrStatus()
    // пакета нет — запускаем загрузку модели (прогресс покажется в блоке ниже)
    if (!trStatus.value?.installed) {
      await startTrDownload()
    }
    await autoTranslateVisible()
    translatePrewarm(uiLang())
  }
}
// Автоперевод постов, язык которых отличается от языка интерфейса.
async function autoTranslateVisible() {
  await refreshTrStatus()
  if (!trStatus.value?.installed) return
  for (const p of posts.value) {
    if (!p.text || tr[p.id]?.shown) continue
    const lang = await detectLang(p.text)
    if (lang && lang !== uiLang()) {
      await doTranslate(p.id, p.text)
    }
  }
}

// Открыть страницу альбома по баннеру-анонсу (добор манифеста по ссылке при нужде).
const openingAlbum = ref('')
async function openAlbum(a: AlbumRef) {
  if (!a || openingAlbum.value) return
  openingAlbum.value = a.id
  try {
    const link = `sn://album/${a.id}~${a.manifest || ''}~${a.node || ''}`
    const id = await albumOpenLink(link)
    router.push({ name: 'album', params: { id } })
  } catch {
    router.push({ name: 'album', params: { id: a.id } })
  } finally {
    openingAlbum.value = ''
  }
}

// Воспроизвести аудио-вложения поста в глобальном нижнем плеере (очередь).
function playInGlobal(p: WallPost, f: PostFile) {
  const auds = p.files.filter(isAudio)
  const list: PlayerTrack[] = auds.map((a) => ({
    cid: a.cid,
    name: a.name,
    title: a.title,
    artist: a.artist,
    mime: a.mime,
    node: a.node,
    cover: a.cover,
  }))
  const start = auds.findIndex((a) => a.cid === f.cid)
  player.playTracks(list, Math.max(0, start))
}

const posts = ref<WallPost[]>([])
// Локально известные альбомы автора (из снапшота стены) — чтобы «оживить»
// старые посты-анонсы без AlbumRef: сопоставляем по обложке/названию.
const authorAlbums = ref<Album[]>([])
function albumRefFromAlbum(a: Album): AlbumRef {
  return {
    id: a.id,
    manifest: a.manifest || '',
    node: a.node || '',
    title: a.title,
    cover: a.cover || '',
    price: a.price || 0,
    free: a.free ?? true,
    tracks: a.tracks?.length || 0,
  }
}
function matchAlbumForPost(p: WallPost): AlbumRef | null {
  if (!authorAlbums.value.length) return null
  // 1) по обложке (album_create кладёт cover в images поста)
  const byCover = authorAlbums.value.find((a) => a.cover && p.images.includes(a.cover))
  if (byCover) return albumRefFromAlbum(byCover)
  // 2) по названию в тексте анонса
  const txt = (p.text || '').toLowerCase()
  const byTitle = authorAlbums.value.find((a) => a.title && txt.includes(a.title.toLowerCase()))
  if (byTitle) return albumRefFromAlbum(byTitle)
  return null
}
const loading = ref(false)
const composing = ref('')
const posting = ref(false)
const pendingImages = ref<string[]>([]) // CID прикреплённых картинок
const pendingFiles = ref<PostFile[]>([]) // прикреплённые файлы (mp3/mp4/…)
const attaching = ref(false)
const attachingFile = ref(false)
// Сессионный кэш медиа (Pinia): переживает навигацию между профилями
const fileUrls = cache.fileUrls // cid -> objectURL (audio/video/img-файлы)
const err = ref('')
const bgRefreshing = ref(false) // тихое фоновое обновление (кэш уже показан)
const loadingMore = ref(false)
const endReached = ref(false)
const sentinel = ref<HTMLElement | null>(null)
let feedObserver: IntersectionObserver | null = null

function humanSize(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`
}
function fileExt(name: string): string {
  const i = name.lastIndexOf('.')
  return i >= 0 ? name.slice(i + 1).toUpperCase() : 'FILE'
}
function isAudio(f: PostFile): boolean {
  return f.mime.startsWith('audio') || /\.mp3$/i.test(f.name)
}
function isVideo(f: PostFile): boolean {
  return f.mime.startsWith('video') || /\.(mp4|webm|mov)$/i.test(f.name)
}
function isImageFile(f: PostFile): boolean {
  return f.mime.startsWith('image') || /\.(png|jpe?g|webp|gif)$/i.test(f.name)
}
// Ленивая загрузка байтов файла → objectURL (кэшируется в Pinia на сессию).
async function ensureFileUrl(f: PostFile) {
  await cache.ensureFileUrl(f)
}
async function downloadFile(f: PostFile) {
  try {
    await socialSaveFile(f.cid, f.node, f.name)
  } catch (e: any) {
    flash(String(e?.message || e))
  }
}

// Отображаемое имя аудио: «Исполнитель — Название» из ID3, иначе имя файла.
function trackLabel(f: PostFile): string {
  const title = (f.title || '').trim()
  const artist = (f.artist || '').trim()
  if (title && artist) return `${artist} — ${title}`
  if (title) return title
  return f.name
}
// Добавить аудио в мой персональный плейлист (виден на моём профиле другим).
async function addToPlaylist(f: PostFile) {
  try {
    await playlistAdd({
      cid: f.cid,
      name: f.name,
      title: f.title || '',
      artist: f.artist || '',
      size: f.size,
      mime: f.mime,
      node: f.node,
      cover: f.cover || '',
    })
    window.dispatchEvent(new Event('playlist-changed'))
    flash(t('wall.addedToPlaylist'))
  } catch (e: any) {
    flash(String(e?.message || e))
  }
}

// --- Аудио-плейлист: проигрывание вложений поста подряд ---
const audioEls: Record<string, HTMLAudioElement> = {}
let autoplayCid: string | null = null
function registerAudio(cid: string, el: any) {
  if (el instanceof HTMLAudioElement) {
    audioEls[cid] = el
    if (autoplayCid === cid) {
      autoplayCid = null
      el.play().catch(() => {})
    }
  } else {
    delete audioEls[cid]
  }
}
async function onAudioEnded(p: WallPost, f: PostFile) {
  const auds = p.files.filter(isAudio)
  const idx = auds.findIndex((x) => x.cid === f.cid)
  const next = auds[idx + 1]
  if (!next) return
  if (fileUrls[next.cid] && audioEls[next.cid]) {
    audioEls[next.cid].play().catch(() => {})
  } else {
    autoplayCid = next.cid // registerAudio проиграет, как только элемент смонтируется
    await ensureFileUrl(next)
  }
}
// Открыть картинку-файл в новом окне в оригинале (загрузив байты при необходимости).
async function openFileImage(f: PostFile) {
  await ensureFileUrl(f)
  const url = fileUrls[f.cid]
  if (url) window.open(url, '_blank')
}

const following = ref(false)
const media = cache.media // cid -> data-URL (сессионный кэш в Pinia)
const openComments = reactive<Record<string, boolean>>({})
const comments = reactive<Record<string, WallComment[]>>({})
const draftComment = reactive<Record<string, string>>({})
const reactions = reactive<Record<string, ReactionSummary>>({}) // postId -> summary
const pickerFor = ref<string | null>(null) // postId whose emoji picker is open
const composerTab = ref<'write' | 'preview'>('write')
const MAX_FILES = 10
const QUICK = ['👍', '❤️', '😂', '🔥', '🎉']
const names = reactive<Record<string, string>>({}) // address -> @username (or '')
const postHtml = reactive<Record<string, string>>({}) // postId -> rendered markdown HTML

// Рендер markdown поста в безопасный HTML (заголовки, жирный, курсив, списки, ссылки).
function renderMd(text: string): string {
  if (!text) return ''
  const raw = marked.parse(text, { breaks: true, async: false }) as string
  return DOMPurify.sanitize(raw)
}

function displayName(addr: string): string {
  const n = names[addr]
  return n ? '@' + n : shortAddr(addr)
}
async function resolveName(addr: string) {
  if (!addr || addr in names) return
  names[addr] = '' // mark in-flight to avoid dupes
  try {
    const n = await usernameOfAddress(addr)
    names[addr] = n || ''
  } catch {
    names[addr] = ''
  }
}
function reactorNames(id: string, emoji: string): string[] {
  return (reactions[id]?.reactors?.[emoji] || []).map(displayName)
}

// Открыть картинку в новом окне в оригинальном размере (но не шире экрана).
function openImage(cid: string) {
  const url = media[cid]
  if (!url) return
  const w = window.open('', '_blank')
  if (!w) return
  w.document.write(
    `<!doctype html><html><head><meta charset="utf-8"><title>SmartNet</title>` +
      `<style>html,body{margin:0;height:100%;background:#0b1017;display:flex;align-items:center;justify-content:center}` +
      `img{max-width:100vw;max-height:100vh;object-fit:contain}</style></head>` +
      `<body><img src="${url}"></body></html>`,
  )
  w.document.close()
}

// --- Markdown-панель форматирования композера ---
const composerRef = ref<HTMLTextAreaElement | null>(null)

function surround(before: string, after: string) {
  const ta = composerRef.value
  if (!ta) return
  const s = ta.selectionStart
  const e = ta.selectionEnd
  const val = composing.value
  const sel = val.slice(s, e)
  composing.value = val.slice(0, s) + before + sel + after + val.slice(e)
  nextTick(() => {
    ta.focus()
    ta.setSelectionRange(s + before.length, s + before.length + sel.length)
  })
}
function prefixLine(prefix: string) {
  const ta = composerRef.value
  if (!ta) return
  const s = ta.selectionStart
  const val = composing.value
  const lineStart = val.lastIndexOf('\n', s - 1) + 1
  composing.value = val.slice(0, lineStart) + prefix + val.slice(lineStart)
  nextTick(() => {
    ta.focus()
    ta.setSelectionRange(s + prefix.length, s + prefix.length)
  })
}
function insertLink() {
  const ta = composerRef.value
  if (!ta) return
  const s = ta.selectionStart
  const e = ta.selectionEnd
  const val = composing.value
  const label = val.slice(s, e) || 'текст'
  const snippet = `[${label}](https://)`
  composing.value = val.slice(0, s) + snippet + val.slice(e)
  nextTick(() => {
    ta.focus()
    // выделить плейсхолдер URL для быстрой замены
    const urlStart = s + label.length + 3
    ta.setSelectionRange(urlStart, urlStart + 8)
  })
}

let unlisten: (() => void) | null = null

function flash(m: string) {
  err.value = m
  setTimeout(() => (err.value = ''), 4000)
}

function relTime(ms: number): string {
  const s = Math.floor((Date.now() - ms) / 1000)
  if (s < 60) return t('wall.now')
  if (s < 3600) return `${Math.floor(s / 60)}m`
  if (s < 86400) return `${Math.floor(s / 3600)}h`
  return new Date(ms).toLocaleDateString()
}

async function loadMedia(cid: string, node: string) {
  await cache.loadMedia(cid, node)
}

// Побочные догрузки для поста: markdown, медиа, реакции, имена, файлы-плееры.
function hydratePost(p: WallPost) {
  // старый анонс без AlbumRef → пробуем сопоставить с локальным альбомом автора
  if (!p.album) {
    const m = matchAlbumForPost(p)
    if (m) p.album = m
  }
  postHtml[p.id] = renderMd(p.text)
  p.images.forEach((cid) => loadMedia(cid, p.node))
  if (p.album?.cover) loadMedia(p.album.cover, p.album.node || p.node)
  p.files.forEach((f) => {
    if (isAudio(f) || isVideo(f) || isImageFile(f)) ensureFileUrl(f)
  })
  loadReactions(p)
  resolveName(p.author)
}

// Полное обновление первой страницы. silent=true — кэш уже на экране,
// показываем только тонкую полоску, без спиннера/очистки ленты.
async function refresh(silent = false) {
  if (!props.author) return
  if (silent) bgRefreshing.value = true
  else loading.value = true
  try {
    const fresh = await listWall(props.author, 50)
    authorAlbums.value = await albumList(props.author)
    posts.value = fresh
    endReached.value = fresh.length < 50
    fresh.forEach(hydratePost)
    cache.save(props.author, fresh)
  } finally {
    loading.value = false
    bgRefreshing.value = false
  }
  // авто-перевод ленты, если включён тумблер
  if (autoTranslate.value && !props.isOwn) {
    autoTranslateVisible()
  }
}

// Тихая дозагрузка новинок (live-события, свой пост): вливаем новые посты
// сверху, не трогая уже догруженные пагинацией старые.
async function mergeNew() {
  if (!props.author) return
  const fresh = await listWall(props.author, 50)
  if (!fresh.length) return
  const freshIds = new Set(fresh.map((p) => p.id))
  const oldestFresh = fresh[fresh.length - 1].ts
  // удалённые автором посты в пределах свежего окна убираем
  const kept = posts.value.filter((p) => p.ts < oldestFresh || freshIds.has(p.id))
  const known = new Set(kept.map((p) => p.id))
  const added = fresh.filter((p) => !known.has(p.id))
  added.forEach(hydratePost)
  posts.value = [...added, ...kept.filter((p) => !freshIds.has(p.id)), ...kept.filter((p) => freshIds.has(p.id))]
    .sort((a, b) => b.ts - a.ts)
  cache.save(props.author, posts.value)
}

// Бесконечная прокрутка: догрузка постов старше последнего на экране.
async function loadMore() {
  if (loadingMore.value || loading.value || endReached.value || !posts.value.length) return
  loadingMore.value = true
  try {
    const before = posts.value[posts.value.length - 1].ts
    const older = await listWall(props.author, 30, before)
    if (older.length) {
      const known = new Set(posts.value.map((p) => p.id))
      const add = older.filter((p) => !known.has(p.id))
      add.forEach(hydratePost)
      posts.value = [...posts.value, ...add]
    }
    if (older.length < 30) endReached.value = true
  } finally {
    loadingMore.value = false
  }
}

async function loadReactions(p: WallPost) {
  reactions[p.id] = await postReactions(p.author, p.id)
  Object.values(reactions[p.id].reactors || {}).forEach((list) =>
    list.forEach((a) => resolveName(a)),
  )
}

function reactionList(id: string): [string, number][] {
  const c = reactions[id]?.counts || {}
  return Object.entries(c).sort((a, b) => b[1] - a[1])
}

async function toggleReaction(p: WallPost, emoji: string) {
  pickerFor.value = null
  try {
    reactions[p.id] = await reactToPost(p.author, p.id, emoji)
  } catch (e: any) {
    flash(String(e?.message || e))
  }
}

function onPickReaction(p: WallPost, e: { i?: string }) {
  const ch = e?.i || ''
  if (ch) toggleReaction(p, ch)
}

async function submitPost() {
  const text = composing.value.trim()
  if ((!text && !pendingImages.value.length && !pendingFiles.value.length) || posting.value) return
  posting.value = true
  try {
    await createPost(text, [...pendingImages.value], [...pendingFiles.value])
    composing.value = ''
    pendingImages.value = []
    pendingFiles.value = []
    await mergeNew()
  } catch (e: any) {
    flash(String(e?.message || e))
  } finally {
    posting.value = false
  }
}

async function attach() {
  if (attaching.value) return
  const path = await pickImageFile()
  if (!path) return
  attaching.value = true
  try {
    const cid = await attachImageToPost(path)
    pendingImages.value.push(cid)
    loadMedia(cid, '')
  } catch (e: any) {
    flash(String(e?.message || e))
  } finally {
    attaching.value = false
  }
}

// Прикрепить файл (mp3/mp4/png/jpg/…): нативный выбор → сохранение в filesdata → сид.
async function attachFile() {
  if (attachingFile.value) return
  if (pendingFiles.value.length >= MAX_FILES) {
    flash(t('wall.maxFiles', { n: MAX_FILES }))
    return
  }
  const path = await pickFile()
  if (!path) return
  attachingFile.value = true
  try {
    const f = await socialAttachFile(path)
    pendingFiles.value.push(f)
  } catch (e: any) {
    flash(String(e?.message || e))
  } finally {
    attachingFile.value = false
  }
}
function removePendingFile(cid: string) {
  pendingFiles.value = pendingFiles.value.filter((f) => f.cid !== cid)
}

async function removePost(id: string) {
  if (!confirm(t('wall.deleteConfirm'))) return
  try {
    await deletePost(id)
    posts.value = posts.value.filter((p) => p.id !== id)
    cache.save(props.author, posts.value)
  } catch (e: any) {
    flash(String(e?.message || e))
  }
}

async function toggleComments(p: WallPost) {
  const open = !openComments[p.id]
  openComments[p.id] = open
  if (!open) return
  // 1) мгновенно из кэша (повторное раскрытие ветки — без ожидания сети)
  const cached = cache.getComments(p.author, p.id)
  if (cached) {
    comments[p.id] = [...cached]
    cached.forEach((c) => resolveName(c.author))
  }
  // 2) фоновая актуализация + сохранение в кэш
  const fresh = await listComments(p.author, p.id)
  comments[p.id] = fresh
  fresh.forEach((c) => resolveName(c.author))
  cache.saveComments(p.author, p.id, fresh)
  // авто-перевод комментариев, если включён тумблер (чужая стена)
  if (autoTranslate.value && !props.isOwn) autoTranslateComments(p.id)
}

// Автоперевод комментариев поста, язык которых отличается от языка интерфейса.
async function autoTranslateComments(postId: string) {
  await refreshTrStatus()
  if (!trStatus.value?.installed) return
  for (const c of comments[postId] || []) {
    if (!c.text || tr[c.id]?.shown) continue
    const lang = await detectLang(c.text)
    if (lang && lang !== uiLang()) {
      await doTranslate(c.id, c.text)
    }
  }
}

async function submitComment(p: WallPost) {
  const text = (draftComment[p.id] || '').trim()
  if (!text) return
  try {
    const c = await addComment(p.author, p.id, text)
    comments[p.id] = [...(comments[p.id] || []), c]
    cache.saveComments(p.author, p.id, comments[p.id])
    resolveName(c.author)
    draftComment[p.id] = ''
    p.comments += 1
  } catch (e: any) {
    flash(String(e?.message || e))
  }
}

async function doHide(p: WallPost, c: WallComment) {
  await hideComment(c.id, c.author)
  comments[p.id] = (comments[p.id] || []).filter((x) => x.id !== c.id)
}
async function doBan(c: WallComment) {
  await banCommenter(c.author)
  flash(t('wall.banned') + ': ' + shortAddr(c.author))
}

async function toggleFollow() {
  if (following.value) {
    await unfollowWall(props.author)
    following.value = false
  } else {
    await followWall(props.author)
    following.value = true
  }
}

async function init() {
  endReached.value = false
  if (!props.author) {
    posts.value = []
    return
  }
  // 1) мгновенный рендер из кэша (посты + медиа из сессионного кэша по CID)
  const cached = cache.get(props.author)
  posts.value = cached ? [...cached] : []
  if (cached) cached.forEach(hydratePost)
  await openWall(props.author)
  if (!props.isOwn) following.value = await isFollowingWall(props.author)
  // 2) фоновая проверка обновлений (тихая, если кэш уже на экране)
  await refresh(!!cached)
  // просмотр стены помечает её прочитанной (сбрасывает бейдж)
  await markWallRead(props.author)
  window.dispatchEvent(new Event('wall-read'))
}

onMounted(async () => {
  await init()
  unlisten = await onSocialEvent((e) => {
    if (e.author === props.author) {
      mergeNew().then(() => markWallRead(props.author))
    }
  })
  // бесконечная прокрутка: сторож внизу ленты → подгрузка следующей пачки
  feedObserver = new IntersectionObserver(
    (entries) => {
      if (entries.some((en) => en.isIntersecting)) loadMore()
    },
    { rootMargin: '600px' },
  )
  if (sentinel.value) feedObserver.observe(sentinel.value)
  // переводчик: статус пакета (только на чужих стенах) + подписка на прогресс
  if (!props.isOwn) {
    refreshTrStatus().then(() => {
      if (trStatus.value?.installed && autoTranslate.value) {
        translatePrewarm(uiLang())
      }
    })
    // восстанавливаем индикатор уже идущей загрузки (после навигации на др. страницу)
    translatorProgress().then((p) => {
      if (p && p.phase !== 'done') trProgress.value = p
    })
    trUnlisten = await onTranslatorProgress((p) => {
      trProgress.value = p
      if (p.phase === 'done') {
        refreshTrStatus().then(() => {
          if (autoTranslate.value) {
            autoTranslateVisible()
            translatePrewarm(uiLang())
          }
        })
      }
    })
  }
})
onUnmounted(() => {
  unlisten?.()
  trUnlisten?.()
  feedObserver?.disconnect()
})
watch(() => props.author, () => init())

// Надёжный триггер автоперевода: как только список постов меняется (новый пост
// пришёл вживую через gossip, обновление ленты, пагинация) — в авторежиме
// переводим все новые «чужие» посты в фоне на текущей странице автора.
watch(
  () => posts.value.map((p) => p.id).join(','),
  () => {
    if (autoTranslate.value && !props.isOwn) autoTranslateVisible()
  },
)
</script>

<template>
  <section class="panel wall" data-testid="wall-feed">
    <!-- ненавязчивый индикатор фонового обновления кэшированной ленты -->
    <div v-if="bgRefreshing" class="bg-refresh" data-testid="wall-bg-refresh">
      <div class="bg-refresh-fill" />
    </div>
    <div class="wall-head">
      <span class="block-title">{{ t('profile.microblog') }}</span>
      <div class="wall-head-actions">
        <span
          v-if="!isOwn && author && trStatus?.installed && trStatus.accelerator && trStatus.accelerator !== 'cpu'"
          class="tr-accel"
          :title="t('wall.accelHint')"
          data-testid="wall-translate-accel"
        >⚡ {{ trStatus.accelerator.toUpperCase() }}</span>
        <button
          v-if="!isOwn && author"
          class="tr-toggle"
          :class="{ on: autoTranslate }"
          @click="toggleAutoTranslate"
          :title="t('wall.autoTranslateHint')"
          data-testid="wall-translate-toggle"
        >🌐 {{ t('wall.autoTranslate') }}<span class="tr-sw" :class="{ on: autoTranslate }" /></button>
        <button
          v-if="!isOwn && author"
          class="follow-btn"
          :class="{ on: following }"
          @click="toggleFollow"
          data-testid="wall-follow-btn"
        >{{ following ? t('wall.following') : t('wall.follow') }}</button>
      </div>
    </div>

    <!-- Языковой пакет качается автоматически (торрентом) — статус на Стене -->
    <div
      v-if="!isOwn && trStatus && !trStatus.installed"
      class="tr-pack"
      data-testid="wall-translate-pack"
    >
      <div class="tr-pack-glow" />
      <div class="tr-pack-body">
        <span class="tr-pack-title">🧠 {{ t('wall.trPackNeeded') }}</span>
        <template v-if="trProgress && (trProgress.phase === 'torrent' || trProgress.phase === 'model' || trProgress.phase === 'tokenizer')">
          <p class="tr-pack-sub">
            {{ t('wall.trDownloading') }} · {{ trProgress.downloaded_mb }} / {{ trProgress.total_mb }} MB · {{ trProgress.pct.toFixed(1) }}%
          </p>
          <div class="tr-pack-bar"><div class="tr-pack-fill" :style="{ width: trProgress.pct + '%' }" /></div>
        </template>
        <template v-else-if="trProgress?.phase === 'error'">
          <p class="tr-pack-sub err">{{ trProgress.error }}</p>
          <button class="tr-pack-btn" @click="startTrDownload" data-testid="wall-translate-retry">{{ t('wall.trRetry') }}</button>
        </template>
        <template v-else-if="trStatus.downloading">
          <p class="tr-pack-sub">{{ t('wall.trDownloading') }}…</p>
          <div class="tr-pack-bar"><div class="tr-pack-fill indeterminate" /></div>
        </template>
        <template v-else>
          <p class="tr-pack-sub" data-testid="wall-tr-pack-size">{{ t('wall.trAutoSoon', { size: trPackSize }) }}</p>
          <p class="tr-pack-sub warn" v-if="trStatus.ram_low">{{ t('wall.trRamLow') }}</p>
        </template>
      </div>
    </div>

    <!-- composer (own wall only) -->
    <div v-if="isOwn" class="composer" data-testid="wall-composer">
      <div class="composer-tabs" data-testid="composer-tabs">
        <button
          type="button"
          class="ct-tab"
          :class="{ active: composerTab === 'write' }"
          @click="composerTab = 'write'"
          data-testid="composer-tab-write"
        >{{ t('wall.write') }}</button>
        <button
          type="button"
          class="ct-tab"
          :class="{ active: composerTab === 'preview' }"
          @click="composerTab = 'preview'"
          data-testid="composer-tab-preview"
        >{{ t('wall.preview') }}</button>
      </div>

      <template v-if="composerTab === 'write'">
        <div class="md-toolbar" data-testid="wall-md-toolbar">
          <button type="button" class="md-btn" title="Bold" @click="surround('**', '**')" data-testid="md-bold"><b>B</b></button>
          <button type="button" class="md-btn" title="Italic" @click="surround('*', '*')" data-testid="md-italic"><i>I</i></button>
          <button type="button" class="md-btn" title="Заголовок" @click="prefixLine('### ')" data-testid="md-heading">H</button>
          <button type="button" class="md-btn" title="Список" @click="prefixLine('- ')" data-testid="md-list">☰</button>
          <button type="button" class="md-btn" title="Ссылка" @click="insertLink" data-testid="md-link">🔗</button>
          <button type="button" class="md-btn" title="Код" @click="surround('`', '`')" data-testid="md-code">&lt;/&gt;</button>
        </div>
        <textarea
          ref="composerRef"
          v-model="composing"
          :placeholder="t('wall.placeholder')"
          rows="3"
          maxlength="5000"
          data-testid="wall-composer-text"
        />
      </template>
      <div
        v-else
        class="composer-preview post-text md"
        :class="{ empty: !composing.trim() }"
        v-html="composing.trim() ? renderMd(composing) : t('wall.previewEmpty')"
        data-testid="wall-composer-preview"
      />
      <div v-if="pendingImages.length" class="attach-row">
        <div v-for="cid in pendingImages" :key="cid" class="attach-thumb">
          <img v-if="media[cid]" :src="media[cid]" alt="" />
        </div>
      </div>
      <div v-if="pendingFiles.length" class="pending-files">
        <div v-for="f in pendingFiles" :key="f.cid" class="pending-file" data-testid="wall-pending-file">
          <span class="att-ico">📎</span>
          <span class="pf-name">{{ f.name }}</span>
          <span class="pf-size">{{ humanSize(f.size) }}</span>
          <button class="pf-del" @click="removePendingFile(f.cid)" data-testid="wall-pending-file-remove">✕</button>
        </div>
      </div>
      <div class="composer-actions">
        <div class="composer-attach">
          <button class="ghost" :disabled="attaching" @click="attach" data-testid="wall-attach-btn">
            📷 {{ t('wall.attach') }}
          </button>
          <button class="ghost" :disabled="attachingFile" @click="attachFile" data-testid="wall-attach-file-btn">
            📎 {{ attachingFile ? '…' : t('wall.attachFile') }}
          </button>
        </div>
        <button
          class="primary"
          :disabled="posting || (!composing.trim() && !pendingImages.length && !pendingFiles.length)"
          @click="submitPost"
          data-testid="wall-post-btn"
        >{{ posting ? '…' : t('wall.post') }}</button>
      </div>
    </div>

    <p v-if="err" class="wall-err" data-testid="wall-error">{{ err }}</p>

    <!-- feed -->
    <p v-if="loading && !posts.length" class="hint dim">{{ t('wall.loading') }}</p>
    <p v-else-if="!posts.length" class="hint dim" data-testid="wall-empty">
      {{ isOwn ? t('wall.empty') : t('wall.emptyOther') }}
    </p>

    <article v-for="p in posts" :key="p.id" class="post" data-testid="wall-post">
      <div class="post-head">
        <UserAvatar :address="p.author" :size="34" />
        <div class="pa-meta">
          <span class="pa-name">{{ displayName(p.author) }}</span>
          <span class="pa-time">{{ relTime(p.ts) }}</span>
        </div>
        <button
          v-if="isOwn"
          class="del-btn"
          :title="t('wall.delete')"
          @click="removePost(p.id)"
          data-testid="wall-post-delete"
        >✕</button>
      </div>

      <div class="post-body">
        <Transition name="tr-fade" mode="out-in">
          <div
            v-if="p.text && tr[p.id]?.shown && tr[p.id]?.text"
            :key="'tr'"
            class="tr-block"
            data-testid="post-translation"
          >
            <span class="tr-badge">🌐 {{ t('wall.translated') }}</span>
            <div class="post-text tr-text">{{ tr[p.id].text }}</div>
            <button class="tr-orig-btn" @click="hideTranslation(p.id)" data-testid="post-show-original">
              {{ t('wall.showOriginal') }}
            </button>
          </div>
          <div v-else-if="p.text" :key="'orig'" class="post-text md" v-html="postHtml[p.id]" data-testid="wall-post-text" />
        </Transition>

        <!-- ручной режим: кнопка «Перевести» под текстом (чужая стена, автоперевод ВЫКЛ) -->
        <button
          v-if="p.text && !isOwn && !autoTranslate && !(tr[p.id]?.shown && tr[p.id]?.text)"
          class="tr-btn"
          :disabled="tr[p.id]?.loading"
          @click="tr[p.id]?.text ? showTranslation(p.id) : doTranslate(p.id, p.text)"
          data-testid="post-translate-btn"
        >
          <span v-if="tr[p.id]?.loading" class="tr-spin" />
          <span v-else>🌐</span>
          {{ tr[p.id]?.text ? t('wall.showTranslation') : t('wall.translate') }}
        </button>

        <!-- Кликабельный баннер альбома (анонс) → страница альбома -->
        <button
          v-if="p.album"
          class="album-banner"
          @click="openAlbum(p.album)"
          :disabled="openingAlbum === p.album.id"
          :data-testid="`wall-album-banner-${p.album.id}`"
        >
          <span class="ab-cover">
            <img v-if="p.album.cover && media[p.album.cover]" :src="media[p.album.cover]" alt="" />
            <svg v-else viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
              <path d="M9 18V5l10-2v13" /><circle cx="6" cy="18" r="3" /><circle cx="16" cy="16" r="3" />
            </svg>
          </span>
          <span class="ab-meta">
            <span class="ab-title">{{ p.album.title }}</span>
            <span class="ab-sub">{{ p.album.tracks }} {{ t('music.tracks') }} · <b v-if="!p.album.free" class="ab-price">{{ p.album.price }} STH</b><b v-else class="ab-free">FREE</b></span>
          </span>
          <span class="ab-cta">{{ openingAlbum === p.album.id ? '…' : (p.album.free ? t('music.playAll') : t('music.getFor', { price: p.album.price })) }} ›</span>
        </button>

        <div v-if="p.images.length" class="post-media">
          <button
            v-for="cid in p.images"
            :key="cid"
            class="media-item"
            @dblclick="openImage(cid)"
            :title="t('wall.dblclickOpen')"
            data-testid="wall-post-image"
          >
            <img v-if="media[cid]" :src="media[cid]" alt="" />
            <span v-else class="media-ph" />
          </button>
        </div>
      </div>

      <!-- attachments (mp3 player / video / image / file) -->
      <div v-if="p.files.length" class="attachments" data-testid="wall-attachments">
        <div v-if="p.files.filter(isAudio).length > 1" class="playlist-head" data-testid="wall-playlist-head">
          🎧 {{ t('wall.playlist') }} · {{ p.files.filter(isAudio).length }}
        </div>
        <template v-for="f in p.files" :key="f.cid">
          <!-- mp3 → компактный плеер -->
          <div v-if="isAudio(f)" class="att att-audio" data-testid="wall-attachment-audio">
            <div class="att-info">
              <span class="att-ico">🎵</span>
              <span class="att-name">{{ trackLabel(f) }}</span>
              <span class="att-size">{{ humanSize(f.size) }}</span>
              <button class="att-dl" :title="t('player.playInPlayer')" @click="playInGlobal(p, f)" data-testid="wall-attachment-play-global">▶</button>
              <button class="att-dl" :title="t('wall.addToPlaylist')" @click="addToPlaylist(f)" data-testid="wall-attachment-playlist-add">＋</button>
              <button class="att-dl" :title="t('wall.download')" @click="downloadFile(f)" data-testid="wall-attachment-download">⬇</button>
            </div>
            <audio
              v-if="fileUrls[f.cid]"
              :ref="(el) => registerAudio(f.cid, el)"
              :src="fileUrls[f.cid]"
              controls
              preload="none"
              @ended="onAudioEnded(p, f)"
            />
            <button v-else class="att-load" @click="ensureFileUrl(f)">▶ {{ t('wall.loadMedia') }}</button>
          </div>
          <!-- mp4 → видео -->
          <div v-else-if="isVideo(f)" class="att att-video" data-testid="wall-attachment-video">
            <video v-if="fileUrls[f.cid]" :src="fileUrls[f.cid]" controls preload="none" />
            <button v-else class="att-load" @click="ensureFileUrl(f)">▶ {{ f.name }}</button>
            <div class="att-info">
              <span class="att-name">{{ f.name }}</span>
              <span class="att-size">{{ humanSize(f.size) }}</span>
              <button class="att-dl" :title="t('wall.download')" @click="downloadFile(f)">⬇</button>
            </div>
          </div>
          <!-- image-файл → миниатюра -->
          <button
            v-else-if="isImageFile(f)"
            class="att att-img"
            @click="openFileImage(f)"
            data-testid="wall-attachment-image"
          >
            <img v-if="fileUrls[f.cid]" :src="fileUrls[f.cid]" :alt="f.name" />
            <span v-else class="att-ph" />
          </button>
          <!-- прочие файлы → строка со скачиванием -->
          <div v-else class="att att-file" data-testid="wall-attachment-file">
            <span class="att-ext">{{ fileExt(f.name) }}</span>
            <span class="att-name">{{ f.name }}</span>
            <span class="att-size">{{ humanSize(f.size) }}</span>
            <button class="att-dl" :title="t('wall.download')" @click="downloadFile(f)">⬇</button>
          </div>
        </template>
      </div>

      <div class="post-foot">
        <div class="reactions" data-testid="wall-reactions">
          <span
            v-for="[emoji, n] in reactionList(p.id)"
            :key="emoji"
            class="chip-wrap"
          >
            <button
              class="react-chip"
              :class="{ mine: reactions[p.id]?.mine === emoji }"
              @click="toggleReaction(p, emoji)"
              data-testid="wall-reaction-chip"
            >{{ emoji }} {{ n }}</button>
            <span class="react-tip" data-testid="wall-reaction-tip">
              <b>{{ emoji }}</b>
              <span v-for="nm in reactorNames(p.id, emoji)" :key="nm" class="tip-name">{{ nm }}</span>
            </span>
          </span>
          <div class="react-add-wrap">
            <button
              class="react-add"
              :class="{ on: pickerFor === p.id }"
              @click="pickerFor = pickerFor === p.id ? null : p.id"
              :title="t('wall.react')"
              data-testid="wall-react-add"
            >☺+</button>
            <div v-if="pickerFor === p.id" class="react-pop" data-testid="wall-react-pop">
              <div class="quick-row">
                <button
                  v-for="q in QUICK"
                  :key="q"
                  class="quick-emoji"
                  @click="toggleReaction(p, q)"
                  data-testid="wall-quick-emoji"
                >{{ q }}</button>
              </div>
              <EmojiPicker
                :native="true"
                :hide-search="false"
                theme="dark"
                @select="(e:any) => onPickReaction(p, e)"
              />
            </div>
          </div>
        </div>
        <button class="cmt-toggle" @click="toggleComments(p)" data-testid="wall-comments-toggle">
          💬 {{ t('wall.comments') }} · {{ p.comments }}
        </button>
      </div>

      <!-- comments -->
      <div v-if="openComments[p.id]" class="comments" data-testid="wall-comments">
        <div v-for="c in comments[p.id] || []" :key="c.id" class="comment" data-testid="wall-comment">
          <UserAvatar :address="c.author" :size="24" />
          <div class="c-body">
            <span class="c-name">{{ displayName(c.author) }}</span>
            <span v-if="tr[c.id]?.shown && tr[c.id]?.text" class="c-text tr-text">{{ tr[c.id].text }}</span>
            <span v-else class="c-text">{{ c.text }}</span>
            <button
              v-if="!isOwn && !autoTranslate && c.text"
              class="c-tr-btn"
              :disabled="tr[c.id]?.loading"
              @click="tr[c.id]?.shown ? hideTranslation(c.id) : (tr[c.id]?.text ? showTranslation(c.id) : doTranslate(c.id, c.text))"
              data-testid="comment-translate-btn"
            >
              <span v-if="tr[c.id]?.loading" class="tr-spin sm" />
              <span v-else>🌐</span>
              {{ tr[c.id]?.shown ? t('wall.showOriginal') : t('wall.translate') }}
            </button>
          </div>
          <div v-if="isOwn && c.author !== myAddress" class="c-mod">
            <button @click="doHide(p, c)" data-testid="wall-comment-hide">{{ t('wall.hide') }}</button>
            <button class="danger" @click="doBan(c)" data-testid="wall-comment-ban">{{ t('wall.ban') }}</button>
          </div>
        </div>
        <div class="comment-add">
          <input
            v-model="draftComment[p.id]"
            :placeholder="t('wall.addComment')"
            maxlength="2000"
            @keyup.enter="submitComment(p)"
            data-testid="wall-comment-input"
          />
          <button class="primary" @click="submitComment(p)" data-testid="wall-comment-send">
            {{ t('wall.send') }}
          </button>
        </div>
      </div>
    </article>

    <!-- сторож бесконечной прокрутки + индикатор подгрузки -->
    <div ref="sentinel" class="feed-sentinel" aria-hidden="true" />
    <p v-if="loadingMore" class="hint dim feed-more" data-testid="wall-loading-more">
      <span class="spin">⟳</span> {{ t('wall.loading') }}
    </p>
  </section>
</template>

<style scoped>
.wall { display: flex; flex-direction: column; gap: 14px; position: relative; }
.bg-refresh {
  position: absolute; top: 0; left: 0; right: 0; height: 2px;
  border-radius: 999px; overflow: hidden; background: transparent; pointer-events: none;
}
.bg-refresh-fill {
  height: 100%; width: 35%; border-radius: 999px;
  background: var(--accent, #64b5ef); opacity: .7;
  animation: bgref 1.1s ease-in-out infinite;
}
@keyframes bgref { 0% { margin-left: -35%; } 100% { margin-left: 100%; } }
.feed-sentinel { height: 1px; }
.feed-more { text-align: center; }
.feed-more .spin { display: inline-block; animation: fm-spin 1s linear infinite; }
@keyframes fm-spin { to { transform: rotate(360deg); } }
.wall-head { display: flex; align-items: center; justify-content: space-between; }
.wall-head-actions { display: flex; align-items: center; gap: 8px; }

/* --- Переводчик: тумблер в шапке --- */
.tr-toggle {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 5px 10px; border-radius: 999px; font-size: 11px;
  border: 1px solid var(--border); background: rgba(255,255,255,.04);
  color: #9fb8c8; cursor: pointer;
  transition: color .15s ease, border-color .15s ease, background-color .15s ease;
}
.tr-toggle:hover { color: #eaf6ff; }
.tr-toggle.on { color: #00e5ff; border-color: rgba(0,229,255,.5); background: rgba(0,229,255,.08); }
.tr-accel {
  display: inline-flex; align-items: center; gap: 3px;
  padding: 4px 9px; border-radius: 999px; font-size: 10px; font-weight: 700;
  letter-spacing: .04em; color: #4ade80;
  border: 1px solid rgba(74,222,128,.45); background: rgba(74,222,128,.1);
}
.tr-sw { width: 22px; height: 12px; border-radius: 999px; background: rgba(255,255,255,.15); position: relative; transition: background-color .15s ease; }
.tr-sw::after { content: ''; position: absolute; top: 1px; left: 1px; width: 10px; height: 10px; border-radius: 50%; background: #6f96ad; transition: transform .15s ease, background-color .15s ease; }
.tr-sw.on { background: rgba(0,229,255,.35); }
.tr-sw.on::after { transform: translateX(10px); background: #00e5ff; }

/* --- Киберпанк-блок «нужен языковой пакет» --- */
.tr-pack {
  position: relative; overflow: hidden;
  border: 1px solid rgba(0,229,255,.3); border-radius: 14px;
  background: linear-gradient(150deg, rgba(0,229,255,.06), rgba(168,85,247,.06));
  padding: 16px 18px;
}
.tr-pack-glow { position: absolute; inset: -40% -20% auto; height: 120px; background: radial-gradient(circle, rgba(0,229,255,.25), transparent 70%); filter: blur(20px); pointer-events: none; }
.tr-pack-body { position: relative; display: flex; flex-direction: column; gap: 10px; }
.tr-pack-title { font-size: 14px; font-weight: 800; color: #eaf6ff; }
.tr-pack-sub { margin: 0; font-size: 12px; color: #9fb8c8; }
.tr-pack-sub.err { color: #ff9d9d; }
.tr-pack-sub.warn { color: #fcd34d; }
.tr-pack-bar { height: 8px; border-radius: 999px; background: rgba(255,255,255,.08); overflow: hidden; }
.tr-pack-fill { height: 100%; border-radius: 999px; background: linear-gradient(90deg, #a855f7, #00e5ff); box-shadow: 0 0 12px rgba(0,229,255,.5); transition: width .3s ease; }
.tr-pack-fill.indeterminate { width: 35%; animation: tr-indet 1.2s ease-in-out infinite; }
@keyframes tr-indet { 0% { margin-left: -35%; } 100% { margin-left: 100%; } }
.tr-pack-btn {
  align-self: flex-start; padding: 8px 16px; border-radius: 999px; cursor: pointer;
  border: 1px solid transparent; font-weight: 700; font-size: 12px; color: #06121e;
  background: linear-gradient(135deg, #00e5ff, #a855f7);
}
.tr-pack-btn:disabled { opacity: .5; cursor: not-allowed; }

/* --- Перевод под постом/комментарием --- */
.tr-block { display: flex; flex-direction: column; gap: 6px; }
.tr-badge { font-size: 10px; letter-spacing: .05em; color: #00e5ff; opacity: .8; }
.tr-text { color: #dbe9f4; }
.tr-orig-btn, .tr-btn, .c-tr-btn {
  align-self: flex-start; display: inline-flex; align-items: center; gap: 6px;
  margin-top: 4px; padding: 3px 10px; border-radius: 999px; font-size: 11px;
  border: 1px solid var(--border); background: rgba(255,255,255,.03);
  color: #7fb4cf; cursor: pointer; transition: color .15s ease, border-color .15s ease;
}
.tr-orig-btn:hover, .tr-btn:hover, .c-tr-btn:hover { color: #00e5ff; border-color: rgba(0,229,255,.4); }
.c-tr-btn { margin-top: 2px; padding: 2px 8px; font-size: 10px; }
.tr-spin {
  width: 11px; height: 11px; border-radius: 50%;
  border: 2px solid rgba(0,229,255,.25); border-top-color: #00e5ff;
  animation: tr-spin .7s linear infinite; display: inline-block;
}
.tr-spin.sm { width: 9px; height: 9px; border-width: 1.5px; }
@keyframes tr-spin { to { transform: rotate(360deg); } }
.tr-fade-enter-active, .tr-fade-leave-active { transition: opacity .28s ease; }
.tr-fade-enter-from, .tr-fade-leave-to { opacity: 0; }
.block-title { font-size: 12px; letter-spacing: .08em; text-transform: uppercase; opacity: .7; }
.follow-btn {
  padding: 5px 14px; border-radius: 999px; border: 1px solid var(--accent, #64b5ef);
  background: transparent; color: var(--accent, #64b5ef); cursor: pointer; font-size: 12px;
  transition: background-color .2s ease, color .2s ease;
}
.follow-btn.on { background: var(--accent, #64b5ef); color: #06121e; }
.follow-btn:hover { background: rgba(100,181,239,.15); }

.composer {
  display: flex; flex-direction: column; gap: 8px;
  background: rgba(255,255,255,.03); border: 1px solid rgba(255,255,255,.07);
  border-radius: 12px; padding: 12px;
}
.composer textarea {
  resize: vertical; width: 100%; background: transparent; border: none; outline: none;
  color: inherit; font: inherit; min-height: 56px;
}
.md-toolbar { display: flex; gap: 4px; margin-bottom: 6px; flex-wrap: wrap; }
.composer-tabs { display: flex; gap: 4px; margin-bottom: 8px; }
.ct-tab {
  padding: 4px 14px; font-size: 12px; letter-spacing: .04em; cursor: pointer;
  background: transparent; border: 1px solid rgba(255,255,255,.1); border-radius: 999px;
  color: var(--text-dim, #9fb0c0); transition: background-color .15s ease, color .15s ease;
}
.ct-tab.active { background: var(--accent, #64b5ef); color: #06121e; border-color: var(--accent, #64b5ef); font-weight: 600; }
.composer-preview {
  min-height: 56px; padding: 4px 2px; border-bottom: 1px dashed rgba(255,255,255,.12);
}
.composer-preview.empty { opacity: .5; font-style: italic; }
.md-btn {
  min-width: 28px; height: 28px; padding: 0 8px; cursor: pointer; font-size: 13px;
  background: rgba(255,255,255,.05); border: 1px solid rgba(255,255,255,.1);
  border-radius: 7px; color: inherit; line-height: 1;
  transition: background-color .15s ease, border-color .15s ease;
}
.md-btn:hover { background: rgba(255,255,255,.12); border-color: var(--accent, #64b5ef); }
.composer-actions { display: flex; justify-content: space-between; align-items: center; }
.composer-attach { display: flex; gap: 8px; }
.pending-files { display: flex; flex-direction: column; gap: 6px; }
.pending-file {
  display: flex; align-items: center; gap: 8px; font-size: 13px;
  background: rgba(255,255,255,.04); border: 1px solid rgba(255,255,255,.08);
  border-radius: 8px; padding: 6px 10px;
}
.pending-file .pf-name { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.pending-file .pf-size { opacity: .55; font-size: 12px; }
.pending-file .pf-del { background: none; border: none; color: inherit; opacity: .5; cursor: pointer; }
.pending-file .pf-del:hover { opacity: 1; color: #ff6b6b; }

.attachments { display: flex; flex-direction: column; gap: 10px; }
.playlist-head { font-size: 12px; font-weight: 600; opacity: .8; letter-spacing: .03em; }
.att-audio, .att-file, .att-video {
  background: linear-gradient(145deg, rgba(255,255,255,.06), rgba(255,255,255,.02));
  border: 1px solid rgba(255,255,255,.12); border-radius: 12px; padding: 10px 12px;
  display: flex; flex-direction: column; gap: 8px;
}
.att-info { display: flex; align-items: center; gap: 10px; font-size: 13px; }
.att-info .att-name { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }
.att-info .att-size { opacity: .55; font-size: 12px; }
.att-audio audio { width: 100%; height: 36px; }
.att-video video { width: 100%; max-height: 360px; border-radius: 8px; background: #000; }
.att-file { flex-direction: row; align-items: center; gap: 10px; }
.att-file .att-name { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.att-file .att-size { opacity: .55; font-size: 12px; }
.att-ext {
  font-size: 11px; font-weight: 700; letter-spacing: .04em; padding: 4px 8px;
  border-radius: 6px; background: var(--accent, #64b5ef); color: #06121e;
}
.att-dl, .att-load {
  background: transparent; border: 1px solid rgba(255,255,255,.16); color: inherit;
  border-radius: 8px; padding: 5px 12px; cursor: pointer; font-size: 13px;
  transition: background-color .2s ease;
}
.att-dl:hover, .att-load:hover { background: rgba(255,255,255,.1); }
.att-img { width: 160px; height: 120px; padding: 0; border: none; cursor: pointer; border-radius: 8px; overflow: hidden; background: rgba(0,0,0,.25); }
.att-img img { width: 100%; height: 100%; object-fit: cover; border-radius: 8px; }
.att-ph { display: block; width: 100%; height: 100%; background: rgba(0,0,0,.3); }
.attach-row { display: flex; gap: 8px; flex-wrap: wrap; }
.attach-thumb { width: 64px; height: 48px; border-radius: 6px; overflow: hidden; background: rgba(0,0,0,.3); }
.attach-thumb img { width: 100%; height: 100%; object-fit: cover; }

button.primary {
  background: var(--accent, #64b5ef); color: #06121e; border: none; padding: 7px 18px;
  border-radius: 999px; cursor: pointer; font-weight: 600; font-size: 13px;
  transition: opacity .2s ease, transform .1s ease;
}
button.primary:disabled { opacity: .45; cursor: default; }
button.primary:not(:disabled):active { transform: scale(.97); }
button.ghost {
  background: transparent; border: 1px solid rgba(255,255,255,.14); color: inherit;
  padding: 6px 14px; border-radius: 999px; cursor: pointer; font-size: 13px;
  transition: background-color .2s ease;
}
button.ghost:hover { background: rgba(255,255,255,.06); }

.wall-err { color: #ff6b6b; font-size: 13px; margin: 0; }
.hint.dim { opacity: .55; font-size: 13px; }

.post {
  background: rgba(255,255,255,.02); border: 1px solid rgba(255,255,255,.06);
  border-radius: 12px; padding: 14px; display: flex; flex-direction: column; gap: 10px;
}
.post-head { display: flex; align-items: center; gap: 10px; }
.pa-meta { display: flex; flex-direction: column; line-height: 1.2; flex: 1; }
.pa-name { font-weight: 600; font-size: 13px; }
.pa-time { font-size: 11px; opacity: .5; }
.del-btn { background: none; border: none; color: inherit; opacity: .4; cursor: pointer; font-size: 15px; }
.del-btn:hover { opacity: 1; color: #ff6b6b; }
.post-text { margin: 0; font-size: 14px; line-height: 1.5; white-space: pre-wrap; word-break: break-word; }
.post-text.md { white-space: normal; }
.post-text.md :first-child { margin-top: 0; }
.post-text.md :last-child { margin-bottom: 0; }
.post-text.md h1 { font-size: 20px; margin: 8px 0; }
.post-text.md h2 { font-size: 17px; margin: 8px 0; }
.post-text.md h3 { font-size: 15px; margin: 6px 0; }
.post-text.md p { margin: 6px 0; }
.post-text.md ul, .post-text.md ol { margin: 6px 0; padding-left: 22px; }
.post-text.md a { color: var(--accent, #64b5ef); text-decoration: underline; }
.post-text.md code {
  background: rgba(255,255,255,.08); padding: 1px 5px; border-radius: 4px;
  font-family: ui-monospace, monospace; font-size: 13px;
}
.post-text.md pre {
  background: rgba(0,0,0,.35); padding: 10px 12px; border-radius: 8px; overflow-x: auto;
}
.post-text.md pre code { background: none; padding: 0; }
.post-text.md blockquote {
  margin: 6px 0; padding: 2px 12px; border-left: 3px solid rgba(100,181,239,.5);
  color: rgba(255,255,255,.82);
}
.post-text.md img { max-width: 100%; border-radius: 8px; }
.post-body { display: flex; flex-direction: column; gap: 10px; }
.album-banner {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  margin: 10px 0 4px;
  padding: 10px 12px;
  border-radius: 12px;
  border: 1px solid rgba(0, 229, 255, 0.28);
  background: linear-gradient(90deg, rgba(0, 229, 255, 0.08), rgba(168, 85, 247, 0.08));
  cursor: pointer;
  text-align: left;
  transition: border-color 0.15s ease, transform 0.12s ease;
}
.album-banner:hover {
  border-color: #00e5ff;
  transform: translateY(-1px);
}
.album-banner:disabled {
  opacity: 0.7;
  cursor: default;
}
.ab-cover {
  width: 48px;
  height: 48px;
  flex: 0 0 auto;
  border-radius: 8px;
  overflow: hidden;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #06121e;
  background: linear-gradient(135deg, #00e5ff, #a855f7);
}
.ab-cover img { width: 100%; height: 100%; object-fit: cover; display: block; }
.ab-meta { display: flex; flex-direction: column; min-width: 0; flex: 1; }
.ab-title { font-size: 14px; font-weight: 700; color: #eaf6ff; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.ab-sub { font-size: 11px; color: #7fb4cf; }
.ab-price { color: #e879f9; }
.ab-free { color: #4ade80; }
.ab-cta { flex: 0 0 auto; font-size: 12px; font-weight: 700; color: #00e5ff; white-space: nowrap; }

.post-media { display: flex; flex-direction: column; gap: 8px; }
.media-item {
  display: block; width: 100%; padding: 0; border: none; cursor: pointer;
  background: rgba(0,0,0,.25); border-radius: 10px; overflow: hidden;
  transition: opacity .15s ease;
}
.media-item:hover { opacity: .94; }
.media-item img { width: 100%; height: auto; display: block; border-radius: 10px; }
.media-ph { display: block; width: 100%; height: 220px; background: rgba(0,0,0,.3); border-radius: 10px; }
.post-foot { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; }
.reactions { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
.chip-wrap { position: relative; display: inline-flex; }
.react-tip {
  position: absolute; bottom: calc(100% + 6px); left: 50%; transform: translateX(-50%);
  display: none; flex-direction: column; gap: 2px; z-index: 60; white-space: nowrap;
  background: #0e1621; border: 1px solid rgba(255,255,255,.12); border-radius: 10px;
  padding: 8px 12px; box-shadow: 0 10px 34px rgba(0,0,0,.55); font-size: 12px; min-width: 90px;
}
.chip-wrap:hover .react-tip { display: flex; }
.react-tip b { font-size: 14px; margin-bottom: 2px; }
.tip-name { opacity: .85; }
.react-chip {
  background: rgba(255,255,255,.05); border: 1px solid rgba(255,255,255,.08);
  border-radius: 999px; padding: 2px 10px; font-size: 13px; cursor: pointer; color: inherit;
  transition: background-color .2s ease, border-color .2s ease;
}
.react-chip:hover { background: rgba(255,255,255,.1); }
.react-chip.mine { border-color: var(--accent, #64b5ef); background: rgba(100,181,239,.16); }
.react-add-wrap { position: relative; }
.react-add {
  background: transparent; border: 1px solid rgba(255,255,255,.12); color: inherit;
  border-radius: 999px; padding: 2px 9px; font-size: 13px; cursor: pointer; opacity: .7;
  transition: opacity .2s ease, background-color .2s ease;
}
.react-add:hover, .react-add.on { opacity: 1; background: rgba(255,255,255,.08); }
.react-pop {
  position: absolute; bottom: calc(100% + 6px); left: 0; z-index: 50;
  background: #17212b; border: 1px solid rgba(255,255,255,.1); border-radius: 12px;
  padding: 8px; box-shadow: 0 12px 40px rgba(0,0,0,.5);
}
.quick-row { display: flex; gap: 4px; margin-bottom: 6px; }
.quick-emoji {
  background: rgba(255,255,255,.05); border: none; border-radius: 8px; font-size: 18px;
  padding: 4px 7px; cursor: pointer; transition: transform .1s ease, background-color .2s ease;
}
.quick-emoji:hover { background: rgba(255,255,255,.14); transform: scale(1.15); }
.cmt-toggle { background: none; border: none; color: inherit; opacity: .65; cursor: pointer; font-size: 12px; }
.cmt-toggle:hover { opacity: 1; }

.comments { border-top: 1px solid rgba(255,255,255,.06); margin-left: 44px; padding-top: 10px; padding-left: 14px; border-left: 2px solid rgba(100,181,239,.25); display: flex; flex-direction: column; gap: 8px; }
.comment { display: flex; align-items: flex-start; gap: 8px; }
.c-body { flex: 1; display: flex; flex-direction: column; }
.c-name { font-size: 12px; font-weight: 600; opacity: .8; }
.c-text { font-size: 13px; line-height: 1.4; word-break: break-word; }
.c-mod { display: flex; gap: 6px; }
.c-mod button { background: none; border: 1px solid rgba(255,255,255,.14); color: inherit; border-radius: 6px; font-size: 11px; padding: 2px 8px; cursor: pointer; }
.c-mod button.danger { border-color: rgba(255,107,107,.5); color: #ff8080; }
.comment-add { display: flex; gap: 8px; margin-top: 4px; }
.comment-add input {
  flex: 1; background: rgba(255,255,255,.04); border: 1px solid rgba(255,255,255,.08);
  border-radius: 999px; padding: 7px 14px; color: inherit; outline: none; font-size: 13px;
}
</style>
