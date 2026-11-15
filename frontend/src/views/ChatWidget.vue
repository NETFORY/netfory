<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount, nextTick, watch } from 'vue'
import EmojiPicker from 'vue3-emoji-picker'
import 'vue3-emoji-picker/css'
import { useI18n } from 'vue-i18n'
import {
  appListen,
  toggleChatWindow,
  chatKeyStatus,
  publishChatKey,
  chatMyContactLink,
  chatAddContact,
  chatListContacts,
  chatRemoveContact,
  type ContactCard,
  openExternal,
  openInMain,
  uiLog,
  isTauri,
  estimatePublishFee,
  openRoom,
  createGroup,
  joinGroup,
  groupInvite,
  deleteRoom,
  getGroupPolicy,
  setGroupPolicy,
  chatGroupLogoSeed,
  banMember,
  unbanMember,
  listBans,
  syncIncomingBans,
  pruneBanTable,
  getRooms,
  getMessages, getMessagesPage,
  sendMessage,
  chatDeleteMessage,
  chatEditMessage,
  sendChatImage,
  sendChatAudio,
  sendChatFile,
  loadChatFile,
  chatVideoBegin,
  chatVideoChunk,
  chatVideoAbort,
  chatVideoSend,
  chatReadDropped,
  onNativeFileDrop,
  loadChatVideo,
  loadChatImage,
  saveChatMedia,
  loadChatAudio,
  chatCancelMedia,
  chatMediaProgress,
  openImageWindow,
  chatCacheInfo,
  chatCacheSet,
  chatCacheClear,
  chatTyping,
  chatAcks,
  chatReact,
  chatReactions,
  clearChatHistory,
  followStats,
  usernameOfAddress,
  loadVault,
  getChatSettings,
  setChatSettings,
  publishMsgCost,
  resolveMsgCost,
  gossipStatus,
  gossipRoomPeers,
  type ChatRoom,
  type ChatMessage,
  type ChatSettings,
  type ChatCacheInfo,
  type MsgReaction,
  type GossipStatus,
  type GroupPolicy,
  myceliumStatus,
  myceliumCircuit,
  type MyceliumCircuit,
} from '@/lib/bridge'
import { useWalletStore } from '@/store/wallet'
import { useToastsStore } from '@/store/toasts'
import { useNetStore } from '@/store/net'
import { useUiStore } from '@/store/ui'
import { openInNetfory, linkTarget, NEWTAB_PREFIX } from '@/lib/clearnet'
import { convertFileSrc, invoke } from '@tauri-apps/api/core'
import UserAvatar from '@/components/UserAvatar.vue'
import CircuitMap from '@/components/CircuitMap.vue'
import MdViewerModal from '@/components/MdViewerModal.vue'
import ChatOnboarding from '@/components/ChatOnboarding.vue'
import { getMyUsername, recoverMyUsername } from '@/lib/bridge'

const { t, locale } = useI18n()
const wallet = useWalletStore()
const ui = useUiStore()
const toasts = useToastsStore()

const rooms = ref<ChatRoom[]>([])
const following = ref<{ name: string | null; address: string }[]>([])
const active = ref<ChatRoom | null>(null)
const messages = ref<ChatMessage[]>([])
const search = ref('')
const draft = ref('')
const tab = ref<'all' | 'subs'>('all')
const keyPublished = ref(true)
// Запись в цепи устарела (сменился NodeId/ключ) — тогда и только тогда нужна кнопка «ОБНОВИТЬ».
const keyStale = ref(false)
const keyNode = ref('')
const keyNodeShort = computed(() => (keyNode.value ? keyNode.value.slice(0, 6) + '…' + keyNode.value.slice(-4) : ''))
async function refreshKeyStatus() {
  try {
    const s = await chatKeyStatus()
    keyPublished.value = s.published
    keyStale.value = s.stale
    keyNode.value = s.publishedNode
  } catch {}
}
// Клик по «✓ Ключ опубликован» → окно «Поделиться контактом» (поверх настроек).
function openShareFromKey() {
  showSettings.value = false
  openShareContact()
}
const publishFee = ref('') // STH fee a publish will use (for the UI preview)
const publishing = ref(false)
// Онбординг «вы в новом интернете»: имя u:// + ключ. Без имени писать нельзя.
const myUsername = ref<string | null>(null)
const showOnboarding = ref(false)
const LS_ONB_SNOOZE = 'sth_chat_onb_snoozed_at'
const setupIncomplete = computed(() => !myUsername.value || !keyPublished.value)
const canWrite = computed(() => !!myUsername.value)
// standalone-мессенджер (без Netfory): имени/баланса из цепи нет — онбординг прячем, показываем подсказку
const hostAvailable = ref(true)
function openOnboarding() {
  showOnboarding.value = true
}
function snoozeOnboarding() {
  showOnboarding.value = false
  try { localStorage.setItem(LS_ONB_SNOOZE, String(Date.now())) } catch {}
}
async function refreshSetupState() {
  try { myUsername.value = await getMyUsername() } catch { myUsername.value = null }
  await refreshKeyStatus()
}
// Standalone: принудительная проверка имени u:// в цепи (публичные узлы sth-core).
const chainCheckBusy = ref(false)
async function checkNameOnChain() {
  chainCheckBusy.value = true
  try {
    const n = await recoverMyUsername()
    if (n) { myUsername.value = n; showToast(t('notifications.nameConfirmed', { name: n })) }
    else showToast(t('messenger.standaloneNotRegistered'))
  } catch { showToast(t('messenger.standaloneNotRegistered')) }
  finally { chainCheckBusy.value = false }
}
let setupPoll: ReturnType<typeof setInterval> | null = null
// Плашка «Переподключение к mesh…» после обрыва сети (событие chat-mesh из Rust).
const meshState = ref<'' | 'reconnecting' | 'ready'>('')
const meshRooms = ref(0)
let meshTimer: ReturnType<typeof setTimeout> | null = null
let unlistenMesh: (() => void) | null = null
let unlistenName: (() => void) | null = null
function onMesh(p: { state?: string; rooms?: number }) {
  if (meshTimer) clearTimeout(meshTimer)
  if (p?.state === 'reconnecting') {
    meshState.value = 'reconnecting'
    meshRooms.value = p.rooms || 0
    // страховка: если ready так и не пришёл — убираем через 45с
    meshTimer = setTimeout(() => { meshState.value = '' }, 45_000)
  } else if (p?.state === 'ready') {
    meshState.value = meshState.value === 'reconnecting' ? 'ready' : ''
    meshTimer = setTimeout(() => { meshState.value = '' }, 2500)
  }
}
// Приветствие в общей группе SmartHoldem после завершения онбординга (один раз).
const LS_WELCOME = 'sth_chat_welcome_sent'
async function sendWelcome() {
  showOnboarding.value = false
  try {
    if (localStorage.getItem(LS_WELCOME)) return
    await refreshSetupState()
    if (!myUsername.value) return
    if (!rooms.value.length) await refreshRooms()
    const grp = rooms.value.find((r) => r.kind === 'group' && (r.peerName || '').toLowerCase() === 'smartholdem')
      || rooms.value.find((r) => r.kind === 'group')
    if (!grp) return
    localStorage.setItem(LS_WELCOME, '1')
    const msg = await sendMessage(grp.id, t('messenger.welcomeMsg', { name: myUsername.value }))
    if (active.value?.id === grp.id) messages.value = [...messages.value, msg]
    else selectRoom(grp)
    await refreshRooms()
  } catch {
    /* сеть/узел недоступны — не критично */
  }
}
let errorTimer: ReturnType<typeof setTimeout> | null = null
// Ошибки узла — дружелюбно и с автоскрытием (раньше красная полоса висела навсегда).
function fmtErr(e: unknown, fallback: string): string {
  const code = typeof e === 'string' ? e : (e as any)?.message || ''
  const m = code.match(/^insufficient-balance:([\d.]+):([\d.]+)/)
  if (m) return t('messenger.insufficientBalance', { need: Number(m[1]).toLocaleString(), have: Number(m[2]).toLocaleString() })
  return code || fallback
}
const wipeAsk = ref(false)
const leaveAsk = ref(false)
const error = ref('')
watch(error, (v) => {
  if (errorTimer) clearTimeout(errorTimer)
  if (v) errorTimer = setTimeout(() => { error.value = '' }, 12_000)
})
const feed = ref<HTMLElement | null>(null)
const inputRef = ref<HTMLTextAreaElement | null>(null)
const copied = ref(false)

// --- settings + economic state ---
const showSettings = ref(false)
const savingSettings = ref(false)
const publishingCost = ref(false)
// P2P transit HUD + open spinner
const loading = ref(true)
const p2p = ref<GossipStatus>({ online: false, topics: 0, p2p: false, resubscribing: 0, outbox: 0 })
// жёлтая точка в шапке: идёт переподписка топиков или копится outbox
const meshWarn = computed(() => p2p.value.online && (p2p.value.resubscribing > 0 || p2p.value.outbox > 0))
let statusTimer: number | null = null
let banTimer: number | null = null
async function refreshStatus() {
  p2p.value = await gossipStatus()
  // per-room presence for the contact list (grayscale + no glow when offline)
  if (p2p.value.online) {
    const next: Record<string, boolean> = {}
    for (const r of visibleRooms.value) {
      try {
        next[r.id] = (await gossipRoomPeers(r.id)) > 0
      } catch {
        next[r.id] = false
      }
    }
    roomOnline.value = next
  } else {
    roomOnline.value = {}
  }
}
const settings = ref<ChatSettings>({ onlyAllowTrusted: false, notifications: true, msgCostSth: 0, costPublished: false, autoSubscribe: true })
const activeCost = ref(0) // peer's per-message price (STH) for the open room
const peerOnline = ref(false) // ≥1 gossip neighbor on the active room's topic
const peerCount = ref(0) // live neighbor count (group member presence)
// «печатает…»: эфемерный gossip-сигнал активной комнаты (гаснет через 3.5 с)
const typingActive = ref(false)
let typingHideTimer: number | undefined
let typingSentAt = 0
// per-room online flag (contact list): offline avatars are grayed + un-glowed
const roomOnline = ref<Record<string, boolean>>({})
function isRoomOnline(r: ChatRoom): boolean {
  return !!roomOnline.value[r.id]
}

// --- group creation modal ---
const showGroupModal = ref(false)

// --- share/add contact (адресная книга — обмен ключами без блокчейна) ---
const showShareContact = ref(false)
const myContactLink = ref('')
const showAddContact = ref(false)
const addContactLink = ref('')
const addContactErr = ref('')
const addingContact = ref(false)
const contacts = ref<ContactCard[]>([])
async function openShareContact() {
  showShareContact.value = true
  myContactLink.value = ''
  try {
    myContactLink.value = await chatMyContactLink()
  } catch {
    /* нет аккаунта/ключа */
  }
}
async function openAddContact() {
  showAddContact.value = true
  addContactErr.value = ''
  addContactLink.value = ''
  try {
    contacts.value = await chatListContacts()
  } catch {
    contacts.value = []
  }
}
async function doAddContact() {
  const link = addContactLink.value.trim()
  if (!link || addingContact.value) return
  addingContact.value = true
  addContactErr.value = ''
  try {
    await chatAddContact(link)
    addContactLink.value = ''
    contacts.value = await chatListContacts()
  } catch (e: any) {
    const code = String(e?.message || e)
    addContactErr.value =
      code.includes('bad-link-format') ? t('messenger.contactBadLink')
      : code.includes('bad-address') ? t('messenger.contactBadAddress')
      : code.includes('bad-key') ? t('messenger.contactBadKey')
      : code.includes('own-contact') ? t('messenger.contactOwn')
      : code
  } finally {
    addingContact.value = false
  }
}
async function removeContact(address: string) {
  try {
    await chatRemoveContact(address)
    contacts.value = await chatListContacts()
  } catch {
    /* noop */
  }
}
function openContactChat(c: ContactCard) {
  showAddContact.value = false
  openConversation(c.username ? 'u://' + c.username : c.address)
}
const groupName = ref('')
const groupLink = ref('')
const creatingGroup = ref(false)

const isGroup = computed(() => active.value?.kind === 'group')
// Owner check needs the wallet address. The messenger runs in a SEPARATE Tauri
// window where the Pinia wallet store may be empty, so resolve it from the
// backend vault (with the store as a fallback).
const myAddress = ref('')
const isOwner = computed(() => isGroup.value && !!active.value?.owner && active.value?.owner === myAddress.value)

// --- group admin panel (owner-only) ---
const showAdmin = ref(false)
const savingPolicy = ref(false)
const adminError = ref('')
const policy = ref<GroupPolicy | null>(null)
const policyForm = ref({ entryFeeSth: 0, paidEntry: false, inviteOnly: false, channelMode: false, logoHash: '', logoNode: '' })
const policyBanned = ref<string[]>([])
// --- логотип группы (владелец грузит ≤128×128 PNG/JPG ≤128КБ; синк через policy) ---
const logoInputRef = ref<HTMLInputElement | null>(null)
const logoUploading = ref(false)
const logoPreview = ref('') // data-url свежезагруженного лого (до сохранения)
const groupLogos = ref<Record<string, string>>({}) // roomId -> data-url
const logoTried = new Set<string>() // hash'и, по которым P2P-докачка уже пробовалась
async function refreshGroupLogo(roomId: string) {
  try {
    const p = await getGroupPolicy(roomId)
    const hash = p?.logoHash || ''
    if (!hash) return
    if (groupLogos.value[roomId]?.length && logoTried.has(hash)) return
    if (logoTried.has(hash)) return
    logoTried.add(hash)
    const b64 = await loadChatImage(roomId, hash, p?.logoNode || '')
    groupLogos.value = { ...groupLogos.value, [roomId]: 'data:image/png;base64,' + b64 }
  } catch {
    /* владелец офлайн — попробуем в другой раз (при рестарте) */
  }
}
function refreshAllGroupLogos() {
  for (const r of rooms.value) if (r.kind === 'group') refreshGroupLogo(r.id)
}
async function onLogoPick(e: Event) {
  const f = (e.target as HTMLInputElement).files?.[0]
  ;(e.target as HTMLInputElement).value = ''
  if (!f || !active.value) return
  if (!/image\/(png|jpe?g)/i.test(f.type)) {
    showToast(t('messenger.logoBadType'))
    return
  }
  logoUploading.value = true
  try {
    const bmp = await createImageBitmap(f)
    const k = Math.min(1, 128 / bmp.width, 128 / bmp.height)
    const w = Math.max(1, Math.round(bmp.width * k))
    const h = Math.max(1, Math.round(bmp.height * k))
    const c = document.createElement('canvas')
    c.width = w
    c.height = h
    c.getContext('2d')!.drawImage(bmp, 0, 0, w, h)
    bmp.close()
    const dataUrl = c.toDataURL('image/png')
    const b64 = dataUrl.split(',')[1] || ''
    if (b64.length > 175_000) {
      showToast(t('messenger.logoTooBig'))
      return
    }
    const r = await chatGroupLogoSeed(active.value.id, b64)
    policyForm.value.logoHash = r.hash
    policyForm.value.logoNode = r.node
    logoPreview.value = dataUrl
  } catch (err: any) {
    showToast(typeof err === 'string' ? err : err?.message || t('messenger.sendFailed'))
  } finally {
    logoUploading.value = false
  }
}
function removeLogo() {
  policyForm.value.logoHash = ''
  policyForm.value.logoNode = ''
  logoPreview.value = ''
}
// ban duration presets (UX: segmented chips instead of a fuzzy slider)
const BAN_DURATIONS = computed(() => [
  { key: '1h', label: t('messenger.banDur1h'), secs: 3600 },
  { key: '1d', label: t('messenger.banDur1d'), secs: 86400 },
  { key: '1w', label: t('messenger.banDur1w'), secs: 604800 },
  { key: '1mo', label: t('messenger.banDur1mo'), secs: 2592000 },
  { key: '1y', label: t('messenger.banDur1y'), secs: 31536000 },
])
const banDuration = ref(86400) // default 1 day
const banningAddr = ref('')
// groups where *I* am banned (banned user's view): groupId -> ban-end (secs)
const myBans = ref<Record<string, number>>({})
let bansInitialized = false
async function refreshMyBans() {
  try {
    const bans = await syncIncomingBans()
    const map: Record<string, number> = {}
    for (const b of bans) {
      map[b.group] = b.until
      // toast only on a NEWLY-detected ban (skip the initial silent seed)
      if (bansInitialized && myBans.value[b.group] !== b.until) {
        const room = rooms.value.find((r) => r.id === b.group)
        const gname = room?.peerName || b.group.slice(0, 10) + '…'
        toasts.push(
          t('messenger.banToastTitle'),
          t('messenger.banToastBody', { group: gname, until: fmtBanUntil(b.until) }),
          'error',
          9000,
        )
      }
    }
    myBans.value = map
    bansInitialized = true
  } catch {
    /* offline */
  }
}
function bannedUntil(roomId: string): number {
  return myBans.value[roomId] || 0
}
function isBannedHere(roomId: string): boolean {
  const u = myBans.value[roomId]
  return !!u && u * 1000 > Date.now()
}
function fmtBanUntil(secs: number): string {
  if (!secs) return ''
  return new Date(secs * 1000).toLocaleString(undefined, {
    day: '2-digit',
    month: 'short',
    hour: '2-digit',
    minute: '2-digit',
  })
}

// Channel mode: only the owner posts to the main topic; members' messages are
// hidden from the main feed (client-side filter, toggleable via "Обсуждения").
const channelMode = computed(() => isGroup.value && !!policy.value?.channelMode)
const showDiscussions = ref(false)
const hiddenCount = computed(() => {
  if (!channelMode.value) return 0
  const owner = active.value?.owner
  return messages.value.filter((m) => !m.sys && m.sender !== owner).length
})
// Feed actually rendered: in channel mode, owner-only unless discussions are open.
const visibleMessages = computed(() => {
  if (!channelMode.value || showDiscussions.value) return messages.value
  const owner = active.value?.owner
  return messages.value.filter((m) => m.sys || m.sender === owner)
})

// Telegram-style grouping: flag the first/last message of each run of
// consecutive same-sender messages (a system line breaks the run). Used to
// show the avatar only on the last bubble and the sender name only on the first.
const groupedMessages = computed(() => {
  // строгая сортировка по времени: без неё история после перезахода хаотична
  const list = [...visibleMessages.value].sort((a, b) => (a.ts || 0) - (b.ts || 0))
  const dayKey = (ts: number) => new Date(ts).toDateString()
  return list.map((m, i) => {
    const prev = list[i - 1]
    const newDay = !prev || dayKey(prev.ts) !== dayKey(m.ts)
    if (m.sys) return { ...m, firstInGroup: true, lastInGroup: true, newDay }
    const next = list[i + 1]
    const samePrev = prev && !prev.sys && prev.sender === m.sender && prev.mine === m.mine
    const sameNext = next && !next.sys && next.sender === m.sender && next.mine === m.mine
    return { ...m, firstInGroup: !samePrev, lastInGroup: !sameNext, newDay }
  })
})

// «12 июня» / «12 июня 2025» — разделитель дней в ленте (как в Telegram)
function dayLabel(ts: number): string {
  const d = new Date(ts)
  const opts: Intl.DateTimeFormatOptions = { day: 'numeric', month: 'long' }
  if (d.getFullYear() !== new Date().getFullYear()) opts.year = 'numeric'
  return d.toLocaleDateString(locale.value, opts)
}

// Lightweight transient toast (this messenger runs in its own Tauri window,
// separate from the shell's ToastHost).
const toast = ref('')
let toastTimer: number | null = null
// ---- контекстное меню «Save File» для медиа-сообщений (ПКМ) ----
const mediaCtx = ref<{ x: number; y: number; id: string; hash: string; node: string; name: string; mime: string } | null>(null)
function openMediaCtx(e: MouseEvent, m: { id: string; text: string; sender: string; ts: number }) {
  const im = imgMeta(m.text)
  const am = im ? null : audMeta(m.text)
  const fm = im || am ? null : fileMeta(m.text)
  const vm = im || am || fm ? null : vidMeta(m.text)
  if (!im && !am && !fm && !vm) return
  let name = ''
  if (im) {
    name = `photo_${im.h.slice(0, 8)}.jpg`
  } else if (fm) {
    name = fm.f || 'file'
  } else if (vm) {
    name = vm.f || 'video.mp4'
  } else if (am!.v) {
    // голосовое: <username>-<дата>-voice.<ext> (реальный контейнер записи)
    const d = new Date(m.ts)
    const pad = (n: number) => String(n).padStart(2, '0')
    const date = `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}_${pad(d.getHours())}${pad(d.getMinutes())}`
    const who = (displayName(m.sender) || 'user').replace(/^@/, '').replace(/[^\w.-]+/g, '_')
    const ext = (am!.m || '').includes('webm') ? 'webm' : 'mp3'
    name = `${who}-${date}-voice.${ext}`
  } else {
    name = am!.f || 'audio.mp3'
  }
  mediaCtx.value = {
    x: Math.min(e.clientX, window.innerWidth - 130),
    y: Math.min(e.clientY, window.innerHeight - 48),
    id: m.id,
    hash: im ? im.h : fm ? fm.h : vm ? vm.h : am!.h,
    node: im ? im.n : fm ? fm.n : vm ? vm.n : am!.n,
    name,
    mime: im ? 'image/jpeg' : fm ? fm.m || 'application/pdf' : vm ? 'video/mp4' : am!.m || 'audio/mpeg',
  }
}
async function saveCtxFile() {
  const c = mediaCtx.value
  mediaCtx.value = null
  if (!c || !active.value) return
  try {
    const ok = await saveChatMedia(active.value.id, c.hash, c.node, c.name, c.mime)
    if (ok) showToast(t('messenger.fileSaved'))
  } catch (e: any) {
    showToast(typeof e === 'string' ? e : e?.message || t('messenger.sendFailed'))
  }
}

function showToast(msg: string) {
  toast.value = msg
  if (toastTimer) clearTimeout(toastTimer)
  toastTimer = window.setTimeout(() => (toast.value = ''), 3200)
}

// --- pinned post + owner right-click context menu (channel/broadcast UX) ---
const pinnedMsg = computed(() => {
  const id = policy.value?.pinned
  if (!id) return null
  return messages.value.find((m) => m.id === id && !m.sys) || null
})
// Subscriber count: distinct participants seen in history + the owner.
const subscriberCount = computed(() => {
  if (!isGroup.value) return 0
  const set = new Set<string>()
  for (const m of messages.value) if (m.sender) set.add(m.sender)
  if (active.value?.owner) set.add(active.value.owner)
  return set.size
})
const ctxMenu = ref<{ open: boolean; x: number; y: number; id: string }>({ open: false, x: 0, y: 0, id: '' })
const ctxEl = ref<HTMLElement | null>(null)
function onMsgContext(e: MouseEvent, id: string) {
  // Меню доступно во ВСЕХ чатах (копировать/удалить); пин — только владельцу группы.
  e.preventDefault()
  ctxMenu.value = { open: true, x: e.clientX, y: e.clientY, id }
  // после рендера меряем РЕАЛЬНУЮ высоту меню: у нижней/правой границы окна
  // открываем вверх/влево, чтобы меню не пряталось за краем
  nextTick(() => {
    const el = ctxEl.value
    if (!el) return
    const r = el.getBoundingClientRect()
    let x = e.clientX
    let y = e.clientY
    if (x + r.width > window.innerWidth - 8) x = Math.max(8, window.innerWidth - r.width - 8)
    if (y + r.height > window.innerHeight - 8) y = Math.max(8, e.clientY - r.height)
    ctxMenu.value = { open: true, x, y, id }
  })
}
function closeCtx() {
  ctxMenu.value.open = false
  ctxMoreReacts.value = false
}
/** Автор сообщения для пункта «Профиль» (нет у системных и своих сообщений). */
function ctxSender(id: string): string {
  const m = messages.value.find((x) => x.id === id)
  return m && !m.sys && !m.mine ? m.sender : ''
}
/** «▾» в ряду быстрых реакций: полный emoji-пикер для любой реакции. */
const ctxMoreReacts = ref(false)
// пикер (~300px) раскрылся — перемеряем меню и прижимаем к краям окна
watch(ctxMoreReacts, (on) => {
  if (!on) return
  nextTick(() => {
    const el = ctxEl.value
    if (!el) return
    const r = el.getBoundingClientRect()
    let { x, y } = ctxMenu.value
    if (x + r.width > window.innerWidth - 8) x = Math.max(8, window.innerWidth - r.width - 8)
    if (y + r.height > window.innerHeight - 8) y = Math.max(8, window.innerHeight - r.height - 8)
    ctxMenu.value = { ...ctxMenu.value, x, y }
  })
})
function pickReaction(e: { i: string }) {
  if (!e?.i) return
  reactTo(ctxMenu.value.id, e.i)
}
async function copyMsg(id: string) {
  closeCtx()
  const m = messages.value.find((x) => x.id === id)
  if (!m) return
  const rm = repMeta(m.text)
  const im = rm ? null : imgMeta(m.text)
  const am = rm || im ? null : audMeta(m.text)
  const text = rm ? rm.t : im ? im.c || '' : am ? am.f || '' : m.text
  try {
    await navigator.clipboard.writeText(text)
    showToast(t('messenger.msgCopied'))
  } catch {
    showToast(t('messenger.sendFailed'))
  }
}
async function deleteMsg(id: string) {
  closeCtx()
  if (!active.value) return
  try {
    await chatDeleteMessage(active.value.id, id)
    messages.value = messages.value.filter((x) => x.id !== id)
    showToast(t('messenger.msgDeleted'))
  } catch (e: any) {
    showToast(typeof e === 'string' ? e : e?.message || t('messenger.sendFailed'))
  }
}

// ===== Редактирование своего сообщения (единый id → замена у собеседников) =====
const editing = ref<{ id: string; original: string } | null>(null)
function canEdit(id: string): boolean {
  const m = messages.value.find((x) => x.id === id)
  if (!m || !m.mine || m.sys) return false
  return !imgMeta(m.text) && !audMeta(m.text) && !vidMeta(m.text) && !fileMeta(m.text)
}
function startEdit(id: string) {
  closeCtx()
  const m = messages.value.find((x) => x.id === id)
  if (!m || !canEdit(id)) return
  const rm = repMeta(m.text)
  const text = rm ? rm.t : m.text
  editing.value = { id, original: draft.value }
  replyTo.value = null
  draft.value = text
  nextTick(() => {
    autoGrow()
    inputRef.value?.focus()
  })
}
function cancelEdit() {
  if (!editing.value) return
  draft.value = editing.value.original
  editing.value = null
  nextTick(autoGrow)
}
async function commitEdit() {
  if (!active.value || !editing.value) return
  const id = editing.value.id
  const text = draft.value.trim()
  if (!text) return
  try {
    const updated = await chatEditMessage(active.value.id, id, text)
    messages.value = messages.value.map((x) => (x.id === id ? { ...x, text: updated.text, edited: updated.edited } : x))
    draft.value = ''
    editing.value = null
    nextTick(autoGrow)
    showToast(t('messenger.msgEdited'))
    await refreshRooms()
  } catch (e: any) {
    const code = typeof e === 'string' ? e : e?.message || ''
    error.value = code.includes('msg-too-long') ? t('messenger.msgTooLong') : code || t('messenger.sendFailed')
  }
}

// ===== Ответ на сообщение (Telegram-style): цитата над полем ввода =====
interface RepMeta { r: string; s: string; p: string; t: string }
function repMeta(text: string): RepMeta | null {
  if (!text || !text.startsWith('sn-rep:')) return null
  try {
    const m = JSON.parse(text.slice(7))
    return m && m.r && typeof m.t === 'string' ? m : null
  } catch { return null }
}
// «чистый» текст конверта: реплаи разворачиваются, медиа — метками
function msgPlain(text: string): string {
  const rm = repMeta(text)
  if (rm) return rm.t
  const im = imgMeta(text)
  if (im) return im.c || '[photo]'
  const am = audMeta(text)
  if (am) return am.f || '[audio]'
  const fm = fileMeta(text)
  if (fm) return fm.f || '[file]'
  const vm = vidMeta(text)
  if (vm) return vm.f || '[video]'
  return text
}
const replyTo = ref<{ id: string; sender: string; preview: string } | null>(null)
function startReply(id: string) {
  closeCtx()
  const m = messages.value.find((x) => x.id === id)
  if (!m) return
  replyTo.value = { id: m.id, sender: m.sender, preview: previewText(msgPlain(m.text)).slice(0, 100) }
  nextTick(() => inputRef.value?.focus())
}
// клик по цитате — прыжок к оригиналу с подсветкой (как в Telegram)
function scrollToMsg(id: string) {
  const el = document.querySelector(`[data-testid="chat-msg-${id}"]`)
  if (!el) return
  el.scrollIntoView({ behavior: 'smooth', block: 'center' })
  el.classList.add('flash')
  window.setTimeout(() => el.classList.remove('flash'), 1400)
}
// мини-превью в цитате: миниатюра фото / иконка аудио оригинального сообщения
function quotedThumb(id: string): string {
  const m = messages.value.find((x) => x.id === id)
  const im = m ? imgMeta(m.text) : null
  return im?.t ? 'data:image/jpeg;base64,' + im.t : ''
}
function quotedIsAudio(id: string): boolean {
  const m = messages.value.find((x) => x.id === id)
  return !!(m && audMeta(m.text))
}
async function setPinned(id: string | null) {
  closeCtx()
  if (!active.value) return
  try {
    const p = await setGroupPolicy(
      active.value.id,
      policyForm.value.entryFeeSth,
      policyForm.value.paidEntry,
      policyForm.value.inviteOnly,
      policyForm.value.channelMode,
      id,
      policyBanned.value,
      policy.value?.logoHash || null,
      policy.value?.logoNode || null,
    )
    policy.value = p
    showToast(id ? t('messenger.postPinned') : t('messenger.postUnpinned'))
  } catch (e: any) {
    showToast(typeof e === 'string' ? e : e?.message || t('messenger.pinFailed'))
  }
}

// --- channel post engagement: local view + repost counters (per-device) ---
const views = ref<Record<string, number>>({})
const reposts = ref<Record<string, number>>({})
const sessionViewed = new Set<string>()
function viewsKey(roomId: string) { return `sth_chat_views_${roomId}` }
function repostsKey(roomId: string) { return `sth_chat_reposts_${roomId}` }
function loadCounters(roomId: string) {
  try { views.value = JSON.parse(localStorage.getItem(viewsKey(roomId)) || '{}') } catch { views.value = {} }
  try { reposts.value = JSON.parse(localStorage.getItem(repostsKey(roomId)) || '{}') } catch { reposts.value = {} }
}
// One view per post per session (local-only metric the owner reads on-device).
function tallyViews() {
  if (!isGroup.value || !active.value) return
  let changed = false
  for (const m of messages.value) {
    if (m.sys) continue
    const k = `${active.value.id}:${m.id}`
    if (sessionViewed.has(k)) continue
    sessionViewed.add(k)
    views.value[m.id] = (views.value[m.id] || 0) + 1
    changed = true
  }
  if (changed) {
    views.value = { ...views.value }
    localStorage.setItem(viewsKey(active.value.id), JSON.stringify(views.value))
  }
}
function repostMsg(id: string) {
  closeCtx()
  const m = messages.value.find((x) => x.id === id)
  if (!m || !active.value) return
  reposts.value[id] = (reposts.value[id] || 0) + 1
  reposts.value = { ...reposts.value }
  try { localStorage.setItem(repostsKey(active.value.id), JSON.stringify(reposts.value)) } catch {}
  const who = displayName(m.sender)
  draft.value = `${t('messenger.repostPrefix', { who })}\n«${msgPlain(m.text)}»\n`
  nextTick(() => { autoGrow(); inputRef.value?.focus() })
  showToast(t('messenger.repostCopied'))
}

async function loadPolicy(roomId: string) {
  policy.value = await getGroupPolicy(roomId)
  const p = policy.value
  policyForm.value = {
    entryFeeSth: p ? p.entryFee / 1e8 : 0,
    paidEntry: p?.paidEntry || false,
    inviteOnly: p?.inviteOnly || false,
    channelMode: p?.channelMode || false,
    logoHash: p?.logoHash || '',
    logoNode: p?.logoNode || '',
  }
  logoPreview.value = ''
  policyBanned.value = p?.banned ? [...p.banned] : []
  showDiscussions.value = false
}
function openAdmin() {
  adminError.value = ''
  showAdmin.value = true
  loadGroupBans()
}
// ban-end (secs) per address for the ACTIVE group (owner moderation view)
const groupBans = ref<Record<string, number>>({})
async function loadGroupBans() {
  if (!active.value) return
  try {
    const all = await listBans()
    const map: Record<string, number> = {}
    for (const b of all) if (b.group === active.value.id) map[b.addr] = b.until
    groupBans.value = map
  } catch {
    /* offline */
  }
}
function memberBanUntil(addr: string): number {
  return groupBans.value[addr] || 0
}
async function savePolicy(extraBan?: string) {
  if (!active.value) return
  savingPolicy.value = true
  adminError.value = ''
  try {
    if (extraBan && !policyBanned.value.includes(extraBan)) policyBanned.value.push(extraBan)
    const p = await setGroupPolicy(
      active.value.id,
      policyForm.value.entryFeeSth,
      policyForm.value.paidEntry,
      policyForm.value.inviteOnly,
      policyForm.value.channelMode,
      policy.value?.pinned ?? null,
      policyBanned.value,
      policyForm.value.logoHash || null,
      policyForm.value.logoNode || null,
    )
    policy.value = p
    if (logoPreview.value && p.logoHash) {
      groupLogos.value = { ...groupLogos.value, [active.value.id]: logoPreview.value }
      logoTried.add(p.logoHash)
    } else if (!p.logoHash && groupLogos.value[active.value.id]) {
      const m = { ...groupLogos.value }
      delete m[active.value.id]
      groupLogos.value = m
    }
    await loadTail(active.value.id)
    scrollFeed()
  } catch (e: any) {
    adminError.value = typeof e === 'string' ? e : e?.message || t('messenger.savePolicyFailed')
  } finally {
    savingPolicy.value = false
  }
}
async function kickMember(addr: string) {
  if (!active.value) return
  banningAddr.value = addr
  adminError.value = ''
  try {
    await banMember(active.value.id, addr, banDuration.value)
    policy.value = await getGroupPolicy(active.value.id)
    policyBanned.value = policy.value?.banned ? [...policy.value.banned] : []
    await loadGroupBans()
  } catch (e: any) {
    adminError.value = typeof e === 'string' ? e : e?.message || t('messenger.banFailed')
  } finally {
    banningAddr.value = ''
  }
}
async function unban(addr: string) {
  if (!active.value) return
  banningAddr.value = addr
  adminError.value = ''
  try {
    await unbanMember(active.value.id, addr)
    policy.value = await getGroupPolicy(active.value.id)
    policyBanned.value = policy.value?.banned ? [...policy.value.banned] : []
    await loadGroupBans()
  } catch (e: any) {
    adminError.value = typeof e === 'string' ? e : e?.message || t('messenger.errGeneric')
  } finally {
    banningAddr.value = ''
  }
}
// Moderation roster: distinct senders seen, excluding the owner and ourselves.
const moderationMembers = computed(() =>
  groupMembers.value.filter((a) => a !== myAddress.value && a !== active.value?.owner),
)

// Font-size regulator (A- / A+), persisted; drives the --chat-text-size CSS var.
const fontSize = ref(14)
function adjustFont(delta: number) {
  fontSize.value = Math.min(20, Math.max(12, fontSize.value + delta))
  try { localStorage.setItem('sth_chat_fontsize', String(fontSize.value)) } catch {}
}
// Distinct senders seen in the open group's history → header identicon mosaic.
const groupMembers = computed(() => {
  if (!isGroup.value) return [] as string[]
  const set = new Set<string>()
  for (const m of messages.value) if (m.sender) set.add(m.sender)
  return Array.from(set).slice(0, 4)
})

// Resolve & cache u:// usernames for addresses (sender labels, mosaic, system lines).
const nameCache = ref<Record<string, string | null>>({})
async function ensureName(addr: string) {
  if (!addr || addr in nameCache.value) return
  nameCache.value = { ...nameCache.value, [addr]: null }
  try {
    const n = await usernameOfAddress(addr)
    nameCache.value = { ...nameCache.value, [addr]: n }
  } catch { /* keep short addr */ }
}
function displayName(addr: string) {
  const n = nameCache.value[addr]
  if (n) return '@' + n
  return addr.length > 16 ? addr.slice(0, 10) + '…' : addr
}
/** Открыть публичный профиль u://<username> пользователя (шапка личного чата,
 *  пункт контекстного меню). Имя — из комнаты/кэша, иначе резолвим по адресу;
 *  без имени открываем по адресу (страница профиля принимает и его). */
async function openUserProfile(addr: string, name?: string | null) {
  closeCtx()
  let n = name || nameCache.value[addr] || null
  if (!n && addr) {
    try { n = await usernameOfAddress(addr) } catch { /* без имени */ }
    if (n) nameCache.value = { ...nameCache.value, [addr]: n }
  }
  const target = n || addr
  if (!target) return
  const url = 'u://' + target.replace(/^@/, '')
  // мессенджер может жить в отдельном окне → через backend в главное окно
  if (isTauri) openInMain(url).catch(() => {})
  else window.dispatchEvent(new CustomEvent('netfory:open-address', { detail: url }))
}
/** Контекстное меню: профиль автора сообщения. */
function ctxOpenProfile(id: string) {
  const m = messages.value.find((x) => x.id === id)
  if (!m || m.sys) return
  const name = m.mine ? null : (!isGroup.value && m.sender === active.value?.peerAddress ? active.value?.peerName : null)
  void openUserProfile(m.sender, name)
}
/** Клик по аватару в группе → личный чат с автором (по имени, если известно). */
function openDmWith(addr: string) {
  if (!addr || addr === myAddress.value) return
  const n = nameCache.value[addr]
  void openConversation(n ? 'u://' + n : addr)
}

// ── @упоминания: подсказки имён из недавних авторов группы и моих контактов ──
const mentionIdx = ref(0)
const mentionQuery = computed<string | null>(() => {
  const el = inputRef.value
  const text = draft.value
  const caret = el ? el.selectionStart ?? text.length : text.length
  const before = text.slice(0, caret)
  const m = /(^|\s)@([\w.-]{0,32})$/.exec(before)
  return m ? m[2].toLowerCase() : null
})
const mentionList = computed<string[]>(() => {
  const q = mentionQuery.value
  if (q === null) return []
  const seen = new Set<string>()
  const out: string[] = []
  // авторы последних сообщений — от новых к старым
  for (let i = messages.value.length - 1; i >= 0 && out.length < 40; i--) {
    const m = messages.value[i]
    if (!m.sender || m.mine) continue
    const n = nameCache.value[m.sender]
    if (n && !seen.has(n.toLowerCase())) { seen.add(n.toLowerCase()); out.push(n) }
  }
  for (const c of contacts.value) {
    if (c.username && !seen.has(c.username.toLowerCase())) { seen.add(c.username.toLowerCase()); out.push(c.username) }
  }
  return out.filter((n) => n.toLowerCase().startsWith(q)).slice(0, 6)
})
watch(mentionList, () => (mentionIdx.value = 0))
function applyMention(name: string) {
  const el = inputRef.value
  const text = draft.value
  const caret = el ? el.selectionStart ?? text.length : text.length
  const before = text.slice(0, caret).replace(/@[\w.-]*$/, '@' + name + ' ')
  draft.value = before + text.slice(caret)
  nextTick(() => {
    if (!el) return
    el.focus()
    el.selectionStart = el.selectionEnd = before.length
  })
}
function onComposerEnter() {
  if (mentionList.value.length) applyMention(mentionList.value[mentionIdx.value] ?? mentionList.value[0])
  else void send()
}
function onMentionNav(e: KeyboardEvent, dir: 1 | -1) {
  if (!mentionList.value.length) return
  e.preventDefault()
  const n = mentionList.value.length
  mentionIdx.value = (mentionIdx.value + dir + n) % n
}
function sysLabel(s: string) {
  return s === 'join' ? t('messenger.joinedGroup') : s
}
watch(messages, (list) => {
  for (const m of list) if (m.sender) ensureName(m.sender)
  tallyViews()
})

async function loadSettings() {
  settings.value = await getChatSettings()
}
async function saveSettings() {
  savingSettings.value = true
  try {
    await setChatSettings(
      settings.value.onlyAllowTrusted,
      settings.value.notifications,
      settings.value.msgCostSth,
      settings.value.autoSubscribe,
    )
  } finally {
    savingSettings.value = false
  }
}
async function doPublishCost() {
  publishingCost.value = true
  try {
    await setChatSettings(
      settings.value.onlyAllowTrusted,
      settings.value.notifications,
      settings.value.msgCostSth,
      settings.value.autoSubscribe,
    )
    await publishMsgCost()
    settings.value.costPublished = true
  } catch (e: any) {
    error.value = typeof e === 'string' ? e : e?.message || t('messenger.ratePublishFailed')
  } finally {
    publishingCost.value = false
  }
}

// Стартовая группа сообщества: новичку без единой комнаты/контакта нечего
// делать в мессенджере — автоматически подключаем к общей группе SmartHoldem.
const DEFAULT_GROUP_LINK =
  'sn://group/71595ad6351b223c761f371760e89ba612b310752212c1d03102a04cae481740~a06678f058919e28073eada3ad7534a93f488a85ba12ab159286f382e5e95d92~U21hcnRIb2xkZW0~SSU6TvycebBMTvc8WHiKTfG9xmX1QSniZu'
let defaultGroupTried = false

async function refreshRooms() {
  rooms.value = await getRooms()
  // список пуст → вступаем в дефолтную группу (одна попытка за сессию)
  if (!rooms.value.length && !defaultGroupTried) {
    defaultGroupTried = true
    try {
      await joinGroup(DEFAULT_GROUP_LINK)
      rooms.value = await getRooms()
    } catch {}
  }
  refreshAllGroupLogos()
}

const visibleRooms = computed(() => {
  const q = search.value.trim().toLowerCase()
  let list = rooms.value
  if (tab.value === 'subs') {
    const subs = new Set(following.value.map((f) => f.address))
    list = list.filter((r) => r.kind === 'group' || subs.has(r.peerAddress))
  }
  if (!q) return list
  return list.filter(
    (r) => (r.peerName || '').toLowerCase().includes(q) || r.peerAddress.toLowerCase().includes(q),
  )
})

// ── Окно истории ─────────────────────────────────────────────────────────
// В памяти/DOM держим только хвост (PAGE сообщений) и подгружаем старше по PAGE
// при прокрутке к верхнему краю — открытие комнаты не зависит от длины истории,
// полная история остаётся в sled на диске.
const PAGE = 50
const hasMore = ref(false)
const loadingMore = ref(false)
let loadedRoom = ''

async function loadTail(roomId: string) {
  loadedRoom = roomId
  const page = await getMessagesPage(roomId, undefined, PAGE)
  if (loadedRoom !== roomId) return
  messages.value = page.messages
  hasMore.value = page.hasMore
}

/** Входящее/ресинк: подтянуть хвост и слить в окно (новые — в конец, правки — на место). */
async function refreshTail(roomId: string) {
  const page = await getMessagesPage(roomId, undefined, PAGE)
  if (loadedRoom !== roomId) return
  const byId = new Map(page.messages.map((m) => [m.id, m]))
  const merged = messages.value.map((m) => byId.get(m.id) ?? m)
  const known = new Set(merged.map((m) => m.id))
  const fresh = page.messages.filter((m) => !known.has(m.id))
  // Догнавшие сообщения несут время ОТПРАВКИ — ставим их на своё место, а не в конец.
  messages.value = fresh.length ? [...merged, ...fresh].sort((a, b) => a.ts - b.ts) : merged
  if (messages.value.length <= PAGE) hasMore.value = page.hasMore
}

async function loadOlder() {
  if (loadingMore.value || !hasMore.value || !active.value) return
  const roomId = active.value.id
  const oldest = messages.value[0]
  if (!oldest) return
  loadingMore.value = true
  const el = feed.value
  const prevHeight = el?.scrollHeight ?? 0
  const prevTop = el?.scrollTop ?? 0
  try {
    const page = await getMessagesPage(roomId, oldest.ts, PAGE)
    if (loadedRoom !== roomId) return
    hasMore.value = page.hasMore
    if (page.messages.length) {
      const known = new Set(messages.value.map((m) => m.id))
      messages.value = [...page.messages.filter((m) => !known.has(m.id)), ...messages.value]
      await nextTick()
      // сохранить позицию чтения: сдвигаем на высоту добавленного сверху
      if (el) el.scrollTop = prevTop + (el.scrollHeight - prevHeight)
    }
  } finally {
    loadingMore.value = false
  }
}

function onFeedScroll() {
  const el = feed.value
  if (el && el.scrollTop < 120 && hasMore.value && !loadingMore.value) void loadOlder()
}

async function scrollFeed(force = true) {
  // ДО перерисовки: насколько бегунок поднят от низа. Если пользователь читает
  // историю (поднят >200px) — входящие сообщения НЕ дёргают ленту вниз.
  const el0 = feed.value
  const preDist = el0 ? el0.scrollHeight - (el0.scrollTop + el0.clientHeight) : 0
  await nextTick()
  const el = feed.value
  if (!el) return
  if (!force && preDist > 200) return
  const target = el.scrollHeight
  const distance = target - (el.scrollTop + el.clientHeight)
  // Smooth for incremental new messages; instant for big jumps (open / switch).
  el.scrollTo({ top: target, behavior: distance > el.clientHeight * 1.5 ? 'auto' : 'smooth' })
}

async function openConversation(target: string) {
  error.value = ''
  try {
    const room = await openRoom(target)
    active.value = room
    await loadTail(room.id)
    activeCost.value = await resolveMsgCost(room.peerAddress)
    peerCount.value = await gossipRoomPeers(room.id)
    peerOnline.value = peerCount.value > 0
    await refreshRooms()
    scrollFeed()
  } catch (e: any) {
    error.value = typeof e === 'string' ? e : e?.message || t('messenger.openChatFailed')
  }
}

async function openGroupRoom(r: ChatRoom) {
  error.value = ''
  try {
    const room = await joinGroup(`sn://group/${r.groupSecret}`)
    active.value = room
    await loadTail(room.id)
    activeCost.value = 0
    await loadPolicy(room.id)
    loadCounters(room.id)
    tallyViews()
    peerCount.value = await gossipRoomPeers(room.id)
    peerOnline.value = peerCount.value > 0
    await refreshRooms()
    scrollFeed()
  } catch (e: any) {
    error.value = typeof e === 'string' ? e : e?.message || t('messenger.openGroupFailed')
  }
}

function selectRoom(r: ChatRoom) {
  if (r.kind === 'group' && isBannedHere(r.id)) {
    error.value = t('messenger.bannedInGroup', { until: fmtBanUntil(bannedUntil(r.id)) })
    return
  }
  // мобильный: комната уже активна (восстановлена при старте / после «назад») — watcher active.id не сработает
  if (active.value?.id === r.id) mobilePane.value = 'chat'
  if (r.kind === 'group') openGroupRoom(r)
  else openConversation(r.peerName ? `u://${r.peerName}` : r.peerAddress)
}
function openContact(f: { name?: string; address: string }) {
  const target = f.name ? 'u://' + f.name : f.address
  if (active.value && (active.value.peerAddress === f.address || (f.name && active.value.peerName === f.name))) mobilePane.value = 'chat'
  openConversation(target)
}

// ===== Парсинг ссылок в сообщениях (XSS-safe: только сегменты, без v-html) =
// Наши схемы (dev/api/sth/u/f) → адресная строка; sn://contact и sn://group →
// мессенджер; http(s) → системный браузер.
const MSG_LINK_RE = /(?:https?|sth|sn|dev|api|u|f):\/\/[^\s<>"'«»]+|magnet:\?[^\s<>"'«»]+/gi
interface MsgSeg {
  text: string
  link?: string
  magnet?: boolean
  contact?: boolean
  group?: boolean
}
// Заголовок magnet-ссылки из dn= (display name); без него — "Torrent Link".
function magnetTitle(link: string): string {
  const m = /[?&]dn=([^&]+)/i.exec(link)
  if (!m) return 'Torrent Link'
  try {
    return decodeURIComponent(m[1].replace(/\+/g, ' '))
  } catch {
    return m[1]
  }
}
// Заголовок контакт-ссылки: @username из u= (если есть), иначе адрес из a=.
function contactTitle(link: string): string {
  const u = /[?&]u=([^&]+)/i.exec(link)
  if (u) {
    try {
      return '@' + decodeURIComponent(u[1])
    } catch {
      return '@' + u[1]
    }
  }
  const a = /[?&]a=([^&]+)/i.exec(link)
  return a ? a[1] : 'Contact'
}
// Название группы из инвайт-ссылки sn://group/<secret>~<node>~<b64name>~<owner>.
function groupTitle(link: string): string {
  try {
    const parts = link.replace(/^sn:\/\/group\//i, '').split('~')
    const b = parts[2] || ''
    if (b) {
      const s = atob(b.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (b.length % 4)) % 4))
      const name = new TextDecoder().decode(Uint8Array.from(s, (c) => c.charCodeAt(0)))
      if (name.trim()) return name.trim()
    }
  } catch { /* битый b64 — фолбэк ниже */ }
  return 'Group'
}
const segCache = new Map<string, MsgSeg[]>()
function msgSegments(text: string): MsgSeg[] {
  const hit = segCache.get(text)
  if (hit) return hit
  const out: MsgSeg[] = []
  let last = 0
  for (const m of text.matchAll(MSG_LINK_RE)) {
    let tok = m[0]
    const i = m.index ?? 0
    // хвостовая пунктуация не входит в ссылку
    const trimmed = tok.replace(/[).,!?:;»"']+$/, '')
    tok = trimmed || tok
    if (i > last) out.push({ text: text.slice(last, i) })
    if (/^magnet:/i.test(tok)) {
      // magnet — компактный бейдж с именем раздачи вместо километровой ссылки
      out.push({ text: magnetTitle(tok), link: tok, magnet: true })
    } else if (/^sn:\/\/contact\?/i.test(tok)) {
      // контакт — компактный бейдж [CONTACT] @имя (или адрес) вместо полной ссылки
      out.push({ text: contactTitle(tok), link: tok, contact: true })
    } else if (/^sn:\/\/group\//i.test(tok)) {
      // группа — компактный бейдж [GROUP] с названием вместо ссылки-приглашения
      out.push({ text: groupTitle(tok), link: tok, group: true })
    } else {
      out.push({ text: tok, link: tok })
    }
    last = i + tok.length
  }
  if (last < text.length) out.push({ text: text.slice(last) })
  if (segCache.size > 500) segCache.clear()
  segCache.set(text, out)
  return out
}
function openMsgLink(url: string) {
  if (/^https?:\/\//i.test(url)) {
    // обычный интернет — по настройке: новая web-вкладка NETFORY или системный браузер.
    // Мессенджер — отдельное Tauri-окно: DOM-событие до главного окна не долетает,
    // поэтому вкладку открываем через backend (open_link_in_main).
    if (linkTarget() === 'system') {
      openExternal(url).catch(() => {})
    } else if (isTauri) {
      // через проверенный open_in_main (тот же путь, что sth:// u:// …) с меткой «новая вкладка»;
      // в системный браузер молча НЕ уходим — настройка «NETFORY Browser» должна соблюдаться
      openInMain(`${NEWTAB_PREFIX}${url}`).catch((e) => uiLog('web', `messenger link → main failed: ${e}`))
    } else {
      openInNetfory(url)
    }
    return
  }
  if (/^sn:\/\/contact\?/i.test(url)) {
    // контакт-ссылка — модалка добавления с предзаполненным полем
    addContactLink.value = url
    addContactErr.value = ''
    showAddContact.value = true
    chatListContacts().then((c) => (contacts.value = c)).catch(() => {})
    return
  }
  if (/^sn:\/\/group\//i.test(url)) {
    joinViaLink(url)
    return
  }
  // dev:// api:// sth:// u:// f:// sn://… — адресная строка оболочки.
  // Мессенджер может жить в ОТДЕЛЬНОМ Tauri-окне: DOM-событие туда не долетит,
  // поэтому через backend (open_in_main): фокус главного окна + targeted emit.
  if (isTauri) {
    openInMain(url).catch(() => {})
  } else {
    window.dispatchEvent(new CustomEvent('netfory:open-address', { detail: url }))
  }
}

async function joinViaLink(link: string) {
  error.value = ''
  try {
    const isLink = link.startsWith('sn://') || link.startsWith('u://')
    const room = await joinGroup(isLink ? link : `sn://group/${link}`)
    await refreshRooms()
    await openGroupRoom(room)
  } catch (e: any) {
    error.value = typeof e === 'string' ? e : e?.message || t('messenger.joinGroupFailed')
  }
}

async function doSearch() {
  const v = search.value.trim()
  if (!v) return
  if (/^(sn|u):\/\/group\//i.test(v) || /^[0-9a-f]{64}(~|$)/i.test(v)) {
    await joinViaLink(v)
  } else {
    await openConversation(v)
  }
  search.value = ''
}

// Полная (пригодная к вступлению) ссылка на группу?
// sn://group/<secretHex64>~<node>~<b64name>~<ownerAddr> — owner завершает формат.
function isCompleteGroupLink(v: string): boolean {
  const body = v.replace(/^(sn|u):\/\/group\//i, '')
  if (body === v && !/^[0-9a-f]{64}~/i.test(v)) return false
  const parts = (body === v ? v : body).split('~')
  return (
    parts.length >= 4 &&
    /^[0-9a-f]{64}$/i.test(parts[0]) &&
    /^S[1-9A-HJ-NP-Za-km-z]{20,40}$/.test(parts[3] || '')
  )
}
// Автовступление при ВСТАВКЕ полной групповой ссылки — без нажатия Enter.
watch(search, (v) => {
  const s = (v || '').trim()
  if (s && isCompleteGroupLink(s)) doSearch()
})

async function createGroupAction() {
  if (!groupName.value.trim()) return
  creatingGroup.value = true
  error.value = ''
  try {
    const [room, link] = await createGroup(groupName.value.trim())
    groupLink.value = link
    await refreshRooms()
    active.value = room
    messages.value = []
    activeCost.value = 0
    peerCount.value = 0
    peerOnline.value = false
  } catch (e: any) {
    error.value = typeof e === 'string' ? e : e?.message || t('messenger.createGroupFailed')
  } finally {
    creatingGroup.value = false
  }
}

function closeGroupModal() {
  showGroupModal.value = false
  groupName.value = ''
  groupLink.value = ''
}

function copyText(t: string) {
  if (!t) return
  navigator.clipboard?.writeText(t)
  copied.value = true
  setTimeout(() => (copied.value = false), 1400)
}

function autoGrow() {
  const el = inputRef.value
  if (!el) return
  el.style.height = 'auto'
  el.style.height = Math.min(el.scrollHeight, 300) + 'px'
}
watch(draft, () => nextTick(autoGrow))
// Сигналим «печатает…» собеседнику (не чаще раза в 2.5 с, только при вводе).
watch(draft, (v) => {
  if (!v || !active.value) return
  const now = Date.now()
  if (now - typingSentAt > 2500) {
    typingSentAt = now
    chatTyping(active.value.id)
  }
})

// --- Emoji picker (Telegram-style): вставка emoji в позицию курсора ---
const showEmoji = ref(false)
function toggleEmoji() {
  showEmoji.value = !showEmoji.value
}
function insertEmoji(e: { i: string }) {
  const ch = e?.i || ''
  if (!ch) return
  const el = inputRef.value
  if (el) {
    const start = el.selectionStart ?? draft.value.length
    const end = el.selectionEnd ?? draft.value.length
    draft.value = draft.value.slice(0, start) + ch + draft.value.slice(end)
    nextTick(() => {
      el.focus()
      const pos = start + ch.length
      el.setSelectionRange(pos, pos)
      autoGrow()
    })
  } else {
    draft.value += ch
  }
}

async function send() {
  if (!canWrite.value) {
    openOnboarding()
    return
  }
  if (!active.value || !draft.value.trim()) return
  showEmoji.value = false
  if (editing.value) {
    await commitEdit()
    return
  }
  // Channel mode: only the owner may publish to the main topic.
  if (channelMode.value && !isOwner.value) {
    showToast(t('messenger.channelOnlyOwner'))
    return
  }
  let text = draft.value
  const rep = replyTo.value
  if (rep) {
    text = 'sn-rep:' + JSON.stringify({ r: rep.id, s: rep.sender, p: rep.preview, t: text })
  }
  draft.value = ''
  replyTo.value = null
  nextTick(autoGrow)
  try {
    const msg = await sendMessage(active.value.id, text)
    messages.value = [...messages.value, msg]
    await refreshRooms()
    scrollFeed()
  } catch (e: any) {
    const code = typeof e === 'string' ? e : e?.message || ''
    error.value = code.includes('msg-too-long') ? t('messenger.msgTooLong') : code || t('messenger.sendFailed')
  }
}

// ===== P2P-изображения (как в Telegram): выбор/вставка → сжатие → отправка =====
const fileInput = ref<HTMLInputElement | null>(null)
const pendingImg = ref<{ full: string; thumb: string; w: number; h: number } | null>(null)
const imgCaption = ref('')
const sendingImg = ref(false)
const imgCache = new Map<string, string>()
const lightbox = ref<{ open: boolean; src: string; loading: boolean; error: boolean }>({ open: false, src: '', loading: false, error: false })

interface ImgMeta { h: string; n: string; w: number; ht: number; s: number; t: string; c?: string }
// Кэш разбора медиа-конвертов: шаблон вызывает *Meta(m.text) десятки раз на
// сообщение при каждом re-render — парсим JSON один раз на текст.
const metaCache = new Map<string, unknown>()
function parseMeta<T extends { h: string }>(text: string, prefix: string): T | null {
  if (!text || !text.startsWith(prefix)) return null
  if (metaCache.has(text)) return metaCache.get(text) as T | null
  let v: T | null = null
  try {
    const m = JSON.parse(text.slice(prefix.length))
    v = m && m.h ? (m as T) : null
  } catch { v = null }
  if (metaCache.size > 2000) metaCache.clear()
  metaCache.set(text, v)
  return v
}

function imgMeta(text: string): ImgMeta | null {
  return parseMeta<ImgMeta>(text, 'sn-img:')
}

function pickImage() { fileInput.value?.click() }
function onFilePicked(e: Event) {
  const inp = e.target as HTMLInputElement
  const f = inp.files?.[0]
  if (f) {
    const isAudio = f.type === 'audio/mpeg' || f.type === 'audio/mp3' || f.type === 'audio/mp4'
      || f.type === 'audio/x-m4a' || /\.(mp3|m4a)$/i.test(f.name)
    const isDoc = f.type === 'application/pdf' || f.type === 'text/markdown' || /\.(pdf|md|markdown)$/i.test(f.name)
    const isVideo = f.type === 'video/mp4' || /\.(mp4|m4v)$/i.test(f.name)
    if (f.type.startsWith('image/')) prepareImage(f)
    else if (isAudio) prepareAudio(f)
    else if (isVideo) prepareVideo(f)
    else if (isDoc) prepareFile(f)
    else showToast(t('messenger.mediaUnsupported'))
  }
  inp.value = ''
}
// Вставка изображения из буфера обмена (Ctrl+V) прямо в поле ввода.
function onPaste(e: ClipboardEvent) {
  const items = e.clipboardData?.items
  if (!items) return
  for (const it of Array.from(items)) {
    if (it.type.startsWith('image/')) {
      const f = it.getAsFile()
      if (f) { e.preventDefault(); prepareImage(f); return }
    }
  }
}

function encodeJpeg(src: ImageBitmap, w: number, h: number, q: number): string {
  const c = document.createElement('canvas')
  c.width = w
  c.height = h
  c.getContext('2d')!.drawImage(src, 0, 0, w, h)
  return c.toDataURL('image/jpeg', q).split(',')[1]
}

// Масштабируем до max 1920×1080 и сжимаем в JPEG; миниатюра ужимается до ≤1.7КБ
// base64 — конверт обязан влезть в 4КБ gossip-кадр (крупные кадры разваливали
// меш: регрессия v1.135.51-63). Полное фото едет по iroh-blobs отдельным каналом.
async function prepareImage(file: Blob) {
  try {
    // Крупные файлы (> 4 МБ ≈ 20+ Мп) декодируем сразу с даунскейлом до 1920 по
    // ширине: полный декод 50–100 Мп фото — сотни МБ RGBA в памяти WebView2 и
    // шанс уронить слабую машину. Мелкие — как есть (без апскейла).
    const bmp = file.size > 4 * 1024 * 1024
      ? await createImageBitmap(file, { resizeWidth: 1920, resizeQuality: 'high' })
      : await createImageBitmap(file)
    const k = Math.min(1, 1920 / bmp.width, 1080 / bmp.height)
    const w = Math.max(1, Math.round(bmp.width * k))
    const h = Math.max(1, Math.round(bmp.height * k))
    const full = encodeJpeg(bmp, w, h, 0.85)
    let side = 110
    let q = 0.45
    let tk = Math.min(1, side / w, side / h)
    let thumb = encodeJpeg(bmp, Math.max(1, Math.round(w * tk)), Math.max(1, Math.round(h * tk)), q)
    while (thumb.length > 1700 && (q > 0.25 || side > 56)) {
      if (q > 0.25) q -= 0.1
      else { side -= 16; tk = Math.min(1, side / w, side / h) }
      thumb = encodeJpeg(bmp, Math.max(1, Math.round(w * tk)), Math.max(1, Math.round(h * tk)), q)
    }
    bmp.close()
    pendingImg.value = { full, thumb, w, h }
  } catch {
    showToast(t('messenger.imgReadFailed'))
  }
}

async function sendImage() {
  if (!active.value || !pendingImg.value || sendingImg.value) return
  if (channelMode.value && !isOwner.value) { showToast(t('messenger.channelOnlyOwner')); return }
  const p = pendingImg.value
  sendingImg.value = true
  try {
    const msg = await sendChatImage(active.value.id, p.full, p.thumb, p.w, p.h, imgCaption.value.trim())
    const meta = imgMeta(msg.text)
    if (meta) imgCache.set(meta.h, p.full)
    pendingImg.value = null
    imgCaption.value = ''
    messages.value = [...messages.value, msg]
    await refreshRooms()
    scrollFeed()
  } catch (e: any) {
    const code = typeof e === 'string' ? e : e?.message || ''
    error.value = code.includes('img-paid-unsupported') ? t('messenger.imgPaidUnsupported')
      : code.includes('img-too-big') || code.includes('img-thumb') ? t('messenger.imgTooBig')
      : code || t('messenger.sendFailed')
  } finally {
    sendingImg.value = false
  }
}

function thumbSrcTxt(text: string): string {
  const m = imgMeta(text)
  if (!m) return ''
  return 'data:image/jpeg;base64,' + (imgCache.get(m.h) || m.t)
}

// Клик по миниатюре → полноэкранный просмотр: в Tauri открываем отдельное
// окно на весь экран (как в Telegram), в web-режиме — lightbox-оверлей.
async function openImageTxt(text: string) {
  const meta = imgMeta(text)
  if (!meta) return
  if (isTauri) {
    try {
      await openImageWindow(active.value?.id || '', meta.h, meta.n, meta.s || 0)
      return
    } catch {}
  }
  const cached = imgCache.get(meta.h)
  if (cached) {
    lightbox.value = { open: true, src: 'data:image/jpeg;base64,' + cached, loading: false, error: false }
    return
  }
  lightbox.value = { open: true, src: '', loading: true, error: false }
  try {
    const b64 = await loadChatImage(active.value?.id || '', meta.h, meta.n)
    imgCache.set(meta.h, b64)
    if (lightbox.value.open) lightbox.value = { open: true, src: 'data:image/jpeg;base64,' + b64, loading: false, error: false }
  } catch {
    if (lightbox.value.open) lightbox.value = { open: true, src: '', loading: false, error: true }
  }
}

// Полные изображения НЕ докачиваются автоматически (v1.135.58): одновременная
// фоновая загрузка всей группой у отправителя душила его gossip-линки — именно
// после медиа «пропадала связь». Миниатюра и так мгновенна (едет в конверте),
// полный файл качается только по клику (openImageTxt), под глобальным шлюзом
// «1 докачка за раз» на бэкенде.

// ===== Квитанции доставки: id сообщения -> скольким участникам доставлено =====
const acks = ref<Record<string, number>>({})
watch(() => active.value?.id, async (id) => {
  acks.value = {}
  reactions.value = {}
  if (id) {
    try { acks.value = await chatAcks(id) } catch {}
    try { reactions.value = await chatReactions(id) } catch {}
  }
})

// ===== Реакции на сообщения (Telegram-style): id -> [{e, n, mine}] =====
const reactions = ref<Record<string, MsgReaction[]>>({})
const quickReactions = ['❤️‍🔥', '🤝', '👍', '❤️', '😁', '😍', '😇', '🎉', '👎']
async function reactTo(id: string, emoji: string) {
  closeCtx()
  if (!active.value) return
  try {
    const list = await chatReact(active.value.id, id, emoji)
    reactions.value = { ...reactions.value, [id]: list }
  } catch {}
}

// Превью последнего сообщения в списке комнат: медиа — локализованной меткой.
function previewText(s: string): string {
  if (!s) return '—'
  const rm = repMeta(s)
  if (rm) s = rm.t
  if (s === '[photo]' || s.startsWith('sn-img:')) return t('messenger.photoPreview')
  if (s === '[audio]' || s.startsWith('sn-aud:')) return t('messenger.audioPreview')
  if (s === '[file]' || s.startsWith('sn-file:')) return t('messenger.filePreview')
  if (s === '[video]' || s.startsWith('sn-vid:')) return t('messenger.videoPreview')
  return s
}

// ===== P2P-аудио (mp3): отправка + мини-плеер как в Telegram =====
interface AudMeta { h: string; n: string; s: number; d: number; f: string; m?: string; v?: number; w?: string }
const pendingAud = ref<{ b64: string; name: string; durMs: number; size: number; mime?: string; voice?: boolean; wave?: string } | null>(null)
const sendingAud = ref(false)
const audCache = new Map<string, string>()
const audFetching = ref<Set<string>>(new Set())
const player = ref<{ hash: string; name: string; playing: boolean; cur: number; dur: number } | null>(null)
let audioEl: HTMLAudioElement | null = null

function audMeta(text: string): AudMeta | null {
  return parseMeta<AudMeta>(text, 'sn-aud:')
}

function bufToB64(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf)
  let bin = ''
  const CH = 0x8000
  for (let i = 0; i < bytes.length; i += CH) {
    bin += String.fromCharCode.apply(null, Array.from(bytes.subarray(i, i + CH)))
  }
  return btoa(bin)
}

function audioDuration(file: Blob): Promise<number> {
  return new Promise((res) => {
    const url = URL.createObjectURL(file)
    const a = new Audio()
    a.preload = 'metadata'
    a.onloadedmetadata = () => { URL.revokeObjectURL(url); res(Math.round((a.duration || 0) * 1000)) }
    a.onerror = () => { URL.revokeObjectURL(url); res(0) }
    a.src = url
  })
}

// Звуковая волна: 48 пиков (RMS по сегментам) → hex-строка 0-f для конверта.
async function computeWave(blob: Blob): Promise<string> {
  try {
    const buf = await blob.arrayBuffer()
    const ctx = new AudioContext()
    const audio = await ctx.decodeAudioData(buf)
    void ctx.close()
    const ch = audio.getChannelData(0)
    const BARS = 48
    const step = Math.floor(ch.length / BARS) || 1
    const peaks: number[] = []
    for (let i = 0; i < BARS; i++) {
      let sum = 0
      let n = 0
      const start = i * step
      const end = Math.min(start + step, ch.length)
      for (let j = start; j < end; j += 16) { sum += Math.abs(ch[j]); n++ }
      peaks.push(n ? sum / n : 0)
    }
    const max = Math.max(...peaks, 0.001)
    return peaks.map((p) => Math.min(15, Math.round((p / max) * 15)).toString(16)).join('')
  } catch {
    return ''
  }
}

async function prepareAudio(file: File) {
  if (file.size > 25 * 1024 * 1024) { showToast(t('messenger.audTooBig')); return }
  const isM4a = file.type === 'audio/mp4' || file.type === 'audio/x-m4a' || /\.m4a$/i.test(file.name)
  try {
    const [buf, durMs, wave] = await Promise.all([file.arrayBuffer(), audioDuration(file), computeWave(file)])
    pendingAud.value = { b64: bufToB64(buf), name: file.name, durMs, size: file.size, mime: isM4a ? 'audio/mp4' : 'audio/mpeg', wave }
  } catch {
    showToast(t('messenger.imgReadFailed'))
  }
}

async function sendAudio() {
  if (!active.value || !pendingAud.value || sendingAud.value) return
  if (channelMode.value && !isOwner.value) { showToast(t('messenger.channelOnlyOwner')); return }
  const p = pendingAud.value
  sendingAud.value = true
  try {
    const msg = await sendChatAudio(active.value.id, p.b64, p.name, p.durMs, p.mime || 'audio/mpeg', !!p.voice, p.wave || '')
    const meta = audMeta(msg.text)
    if (meta) audCache.set(meta.h, p.b64)
    pendingAud.value = null
    messages.value = [...messages.value, msg]
    await refreshRooms()
    scrollFeed()
  } catch (e: any) {
    const code = typeof e === 'string' ? e : e?.message || ''
    error.value = code.includes('paid-unsupported') ? t('messenger.imgPaidUnsupported')
      : code.includes('aud-too-big') ? t('messenger.audTooBig')
      : code || t('messenger.sendFailed')
  } finally {
    sendingAud.value = false
  }
}

// ===== P2P-документы (.pdf/.md): конверт sn-file, сохранение через chat_save_media =====
interface FileMeta { h: string; n: string; s: number; f: string; m?: string }
const pendingFile = ref<{ b64: string; name: string; size: number; mime: string } | null>(null)
const sendingFile = ref(false)
const fileSaving = ref<Set<string>>(new Set())

function fileMeta(text: string): FileMeta | null {
  return parseMeta<FileMeta>(text, 'sn-file:')
}

async function prepareFile(file: File) {
  if (file.size > 25 * 1024 * 1024) { showToast(t('messenger.fileTooBig')); return }
  try {
    const buf = await file.arrayBuffer()
    const mime = f_isPdf(file) ? 'application/pdf' : 'text/markdown'
    pendingFile.value = { b64: bufToB64(buf), name: file.name, size: file.size, mime }
  } catch {
    showToast(t('messenger.imgReadFailed'))
  }
}
function f_isPdf(file: File): boolean {
  return file.type === 'application/pdf' || /\.pdf$/i.test(file.name)
}

async function sendFile() {
  if (!active.value || !pendingFile.value || sendingFile.value) return
  if (channelMode.value && !isOwner.value) { showToast(t('messenger.channelOnlyOwner')); return }
  const p = pendingFile.value
  sendingFile.value = true
  try {
    const msg = await sendChatFile(active.value.id, p.b64, p.name, p.mime)
    pendingFile.value = null
    messages.value = [...messages.value, msg]
    await refreshRooms()
    scrollFeed()
  } catch (e: any) {
    const code = typeof e === 'string' ? e : e?.message || ''
    error.value = code.includes('paid-unsupported') ? t('messenger.imgPaidUnsupported')
      : code.includes('file-too-big') ? t('messenger.fileTooBig')
      : code || t('messenger.sendFailed')
  } finally {
    sendingFile.value = false
  }
}

// клик по карточке файла: P2P-докачка (если надо) + диалог «Сохранить как»
async function saveFileMsg(text: string) {
  const meta = fileMeta(text)
  if (!meta || !active.value || fileSaving.value.has(meta.h)) return
  fileSaving.value = new Set(fileSaving.value).add(meta.h)
  try {
    const ok = await saveChatMedia(active.value.id, meta.h, meta.n, meta.f || 'file', meta.m || 'application/pdf')
    if (ok) showToast(t('messenger.fileSaved'))
  } catch (e: any) {
    const code = typeof e === 'string' ? e : e?.message || ''
    showToast(code.includes('media-timeout') ? t('messenger.audTimeout') : code || t('messenger.audLoadFailed'))
  } finally {
    const s = new Set(fileSaving.value)
    s.delete(meta.h)
    fileSaving.value = s
  }
}
function fileExtBadge(text: string): string {
  const m = fileMeta(text)
  return (m?.m || '').includes('pdf') ? 'PDF' : 'MD'
}

// ===== Просмотр .md в чате (Telegram-style, без сохранения на диск) =====
const mdViewer = ref<{ name: string; text: string } | null>(null)
const fileTextCache = new Map<string, string>()

function b64ToUtf8(b64: string): string {
  const bin = atob(b64)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return new TextDecoder('utf-8').decode(bytes)
}

function isMdFile(meta: FileMeta): boolean {
  return (meta.m || '').includes('markdown') || /\.(md|markdown)$/i.test(meta.f || '')
}

// клик по карточке: .md — открыть просмотр, .pdf — «Сохранить как»
function onFileCardClick(text: string) {
  const meta = fileMeta(text)
  if (!meta) return
  if (isMdFile(meta)) openMdViewer(text)
  else saveFileMsg(text)
}

async function openMdViewer(text: string) {
  const meta = fileMeta(text)
  if (!meta || !active.value) return
  let content = fileTextCache.get(meta.h)
  if (content === undefined) {
    if (fileSaving.value.has(meta.h)) return
    fileSaving.value = new Set(fileSaving.value).add(meta.h)
    try {
      const b64 = await loadChatFile(active.value.id, meta.h, meta.n, meta.m || 'text/markdown')
      content = b64ToUtf8(b64)
      fileTextCache.set(meta.h, content)
    } catch (e: any) {
      const code = typeof e === 'string' ? e : e?.message || ''
      showToast(code.includes('media-timeout') ? t('messenger.audTimeout') : code || t('messenger.audLoadFailed'))
      return
    } finally {
      const s = new Set(fileSaving.value)
      s.delete(meta.h)
      fileSaving.value = s
    }
  }
  mdViewer.value = { name: meta.f || 'file.md', text: content }
}

// ===== Видео (.mp4 ≤500 МБ): чанк-аплоад + встроенный HTML5-плеер =====
interface VidMeta { h: string; n: string; s: number; f: string; m?: string; d?: number; t?: string; w?: number; ht?: number; ph?: string }
const pendingVid = ref<{ file: File; name: string; size: number; durMs: number; thumb: string; w: number; h: number; poster: string } | null>(null)
const sendingVid = ref(false)
const vidPct = ref(0)
const vidFetching = ref<Set<string>>(new Set())
const vidProg = ref<Record<string, { pct: number; label: string; hint?: string }>>({})

// Подсказка «почему ждём»: до 15 с — просто ищем; дальше объясняем по данным:
// автор онлайн в комнате → скорее NAT/реле; офлайн → нет живых источников.
function sourceHint(providers: number, elapsed: number): string {
  if (elapsed === 0) return t('messenger.vidQueued')
  if (elapsed < 15) return t('messenger.vidSources', { n: providers })
  if (providers <= 1 && !peerOnline.value) return t('messenger.vidSenderOffline')
  return t('messenger.vidSourcesSilent', { n: providers })
}
const vidSrc = ref<Record<string, string>>({}) // hash → asset:// URL (играет стримингом)

function vidMeta(text: string): VidMeta | null {
  return parseMeta<VidMeta>(text, 'sn-vid:')
}

// длительность + кадр-постер (~1с): тот же бюджет миниатюры, что у фото —
// конверт sn-vid обязан влезть в 4КБ gossip-кадр
function videoProbe(file: File): Promise<{ durMs: number; thumb: string; poster: string; w: number; h: number }> {
  return new Promise((resolve) => {
    const url = URL.createObjectURL(file)
    const v = document.createElement('video')
    v.preload = 'auto'
    v.muted = true
    let durMs = 0
    let done = false
    const finish = (r: { durMs: number; thumb: string; poster: string; w: number; h: number }) => {
      if (done) return
      done = true
      URL.revokeObjectURL(url)
      v.removeAttribute('src')
      resolve(r)
    }
    const fail = () => finish({ durMs, thumb: '', poster: '', w: 0, h: 0 })
    v.onloadedmetadata = () => {
      durMs = isFinite(v.duration) ? Math.round(v.duration * 1000) : 0
      try { v.currentTime = Math.min(1, (v.duration || 1) / 10) } catch { fail() }
    }
    v.onseeked = async () => {
      try {
        const vw = v.videoWidth
        const vh = v.videoHeight
        if (!vw || !vh) return fail()
        const bmp = await createImageBitmap(v)
        let side = 110
        let q = 0.45
        let tk = Math.min(1, side / vw, side / vh)
        let thumb = encodeJpeg(bmp, Math.max(1, Math.round(vw * tk)), Math.max(1, Math.round(vh * tk)), q)
        while (thumb.length > 1700 && (q > 0.25 || side > 56)) {
          if (q > 0.25) q -= 0.1
          else { side -= 16; tk = Math.min(1, side / vw, side / vh) }
          thumb = encodeJpeg(bmp, Math.max(1, Math.round(vw * tk)), Math.max(1, Math.round(vh * tk)), q)
        }
        // качественный постер 360px — уйдёт отдельным мини-блобом (hash в конверте)
        const pk = Math.min(1, 360 / vw)
        let poster = encodeJpeg(bmp, Math.max(1, Math.round(vw * pk)), Math.max(1, Math.round(vh * pk)), 0.8)
        if (poster.length > 400_000) poster = encodeJpeg(bmp, Math.max(1, Math.round(vw * pk)), Math.max(1, Math.round(vh * pk)), 0.6)
        bmp.close()
        finish({ durMs, thumb, poster, w: vw, h: vh })
      } catch { fail() }
    }
    v.onerror = fail
    window.setTimeout(fail, 15000)
    v.src = url
  })
}

async function prepareVideo(file: File) {
  if (file.size > 500 * 1024 * 1024) { showToast(t('messenger.vidTooBig')); return }
  const probe = await videoProbe(file)
  pendingVid.value = { file, name: file.name, size: file.size, durMs: probe.durMs, thumb: probe.thumb, w: probe.w, h: probe.h, poster: probe.poster }
}

async function sendVideo() {
  if (!active.value || !pendingVid.value || sendingVid.value) return
  if (channelMode.value && !isOwner.value) { showToast(t('messenger.channelOnlyOwner')); return }
  const p = pendingVid.value
  sendingVid.value = true
  vidPct.value = 0
  let uploadId = ''
  try {
    uploadId = await chatVideoBegin()
    const CHUNK = 8 * 1024 * 1024
    for (let off = 0; off < p.file.size; off += CHUNK) {
      const buf = await p.file.slice(off, off + CHUNK).arrayBuffer()
      await chatVideoChunk(uploadId, bufToB64(buf))
      vidPct.value = Math.min(99, Math.floor(((off + buf.byteLength) * 100) / p.file.size))
    }
    const msg = await chatVideoSend(active.value.id, uploadId, p.name, p.durMs, p.thumb, p.w, p.h, p.poster)
    pendingVid.value = null
    messages.value = [...messages.value, msg]
    await refreshRooms()
    scrollFeed()
  } catch (e: any) {
    if (uploadId) chatVideoAbort(uploadId).catch(() => {})
    const code = typeof e === 'string' ? e : e?.message || ''
    error.value = code.includes('paid-unsupported') ? t('messenger.imgPaidUnsupported')
      : code.includes('video-too-big') ? t('messenger.vidTooBig')
      : code || t('messenger.sendFailed')
  } finally {
    sendingVid.value = false
    vidPct.value = 0
  }
}

// качественные постеры (мини-блобы) + фоновый префетч видео
const vidPoster = ref<Record<string, string>>({}) // poster hash → data URL
const vidPath = ref<Record<string, string>>({}) // video hash → путь в кэше (готово)
const isAndroidUA = /android/i.test(navigator.userAgent)
let mediaQueueBusy = false

watch([messages, active], () => { void pumpMediaQueue() })

async function pumpMediaQueue() {
  if (mediaQueueBusy || !active.value) return
  mediaQueueBusy = true
  const roomId = active.value.id
  try {
    // 1) постеры (маленькие — качаем первыми, карточки становятся чёткими у всех)
    for (const m of messages.value) {
      const meta = vidMeta(m.text)
      if (!meta?.ph || vidPoster.value[meta.ph]) continue
      try {
        const b64 = await loadChatImage(roomId, meta.ph, meta.n)
        vidPoster.value = { ...vidPoster.value, [meta.ph]: 'data:image/jpeg;base64,' + b64 }
      } catch {}
      if (active.value?.id !== roomId) return
    }
    // 2) фоновая автозагрузка видео ≤200 МБ (десктоп): скачавший становится сидером
    if (isAndroidUA) return
    for (const m of messages.value) {
      const meta = vidMeta(m.text)
      if (!meta || meta.s > 200 * 1024 * 1024) continue
      if (vidPath.value[meta.h] || vidSrc.value[meta.h] || vidFetching.value.has(meta.h)) continue
      await fetchVideoToCache(m.text, false)
      if (active.value?.id !== roomId) return
    }
  } finally {
    mediaQueueBusy = false
  }
}

// плеер не смог декодировать/открыть файл (кодек не поддержан WebView,
// либо asset-протокол не пустил) → возврат к постеру + подсказка
function onVideoError(text: string) {
  const meta = vidMeta(text)
  if (!meta) return
  const { [meta.h]: _drop, ...rest } = vidSrc.value
  vidSrc.value = rest
  showToast(t('messenger.vidPlayError'))
}

// клик по карточке: мгновенно, если файл уже в кэше; иначе P2P-докачка
// (повторный клик — отмена) → asset:// стриминг
async function playVideo(text: string) {
  const meta = vidMeta(text)
  if (!meta || !active.value) return
  if (vidSrc.value[meta.h]) return
  if (vidPath.value[meta.h]) {
    vidSrc.value = { ...vidSrc.value, [meta.h]: convertFileSrc(vidPath.value[meta.h]) }
    return
  }
  if (vidFetching.value.has(meta.h)) {
    await chatCancelMedia(meta.h)
    return
  }
  await fetchVideoToCache(text, true)
}

async function fetchVideoToCache(text: string, openPlayer: boolean) {
  const meta = vidMeta(text)
  if (!meta || !active.value || vidFetching.value.has(meta.h)) return
  vidFetching.value = new Set(vidFetching.value).add(meta.h)
  const total = meta.s || 0
  let lastB = 0
  let lastT = Date.now()
  // пока байты не пошли (поиск пиров) — честная надпись вместо «0%»
  vidProg.value = { ...vidProg.value, [meta.h]: { pct: 0, label: t('messenger.vidSearching') } }
  const progTimer = window.setInterval(async () => {
    try {
      const pr = await chatMediaProgress(meta.h)
      const now = Date.now()
      const speed = Math.max(0, ((pr.bytes - lastB) * 1000) / Math.max(1, now - lastT))
      lastB = pr.bytes
      lastT = now
      const pct = total > 0 ? Math.min(99, Math.floor((pr.bytes * 100) / total)) : 0
      vidProg.value = {
        ...vidProg.value,
        [meta.h]: {
          pct,
          label: pr.bytes > 0 ? `${pct}% · ${fmtSize(speed)}/s` : t('messenger.vidSearching'),
          hint: pr.bytes > 0 ? '' : sourceHint(pr.providers || 0, pr.elapsedSecs || 0),
        },
      }
    } catch {}
  }, 600)
  try {
    const path = await loadChatVideo(active.value.id, meta.h, meta.n)
    vidPath.value = { ...vidPath.value, [meta.h]: path }
    if (openPlayer) vidSrc.value = { ...vidSrc.value, [meta.h]: convertFileSrc(path) }
  } catch (e: any) {
    const code = typeof e === 'string' ? e : e?.message || ''
    if (openPlayer) {
      showToast(
        code.includes('media-cancelled') ? t('messenger.audCancelled')
          : code.includes('media-timeout') ? t('messenger.audTimeout')
          : code || t('messenger.audLoadFailed'),
      )
    }
  } finally {
    window.clearInterval(progTimer)
    const { [meta.h]: _drop, ...rest } = vidProg.value
    vidProg.value = rest
    const s = new Set(vidFetching.value)
    s.delete(meta.h)
    vidFetching.value = s
  }
}

function fmtDur(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000))
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`
}
function fmtSize(b: number): string {
  return b >= 1024 * 1024 ? (b / (1024 * 1024)).toFixed(1) + ' MB' : Math.max(1, Math.round(b / 1024)) + ' KB'
}

function stopPlayer() {
  if (audioEl) {
    audioEl.pause()
    audioEl.src = ''
    audioEl = null
  }
  player.value = null
}

// Клик по кнопке play в сообщении: докачка mp3 по P2P при первом запуске,
// глобальный одиночный плеер (новый трек останавливает предыдущий).
async function toggleAudio(text: string) {
  const meta = audMeta(text)
  if (!meta) return
  if (player.value && player.value.hash === meta.h && audioEl) {
    if (audioEl.paused) { audioEl.play(); player.value.playing = true }
    else { audioEl.pause(); player.value.playing = false }
    return
  }
  stopPlayer()
  let b64 = audCache.get(meta.h)
  if (!b64) {
    // повторный клик по крутящейся загрузке = отмена (следующий клик — повтор)
    if (audFetching.value.has(meta.h)) {
      await chatCancelMedia(meta.h)
      return
    }
    audFetching.value.add(meta.h)
    audFetching.value = new Set(audFetching.value)
    // живой прогресс докачки: % и скорость на кружке (поллинг раз в 600мс)
    const total = meta.s || 0
    let lastB = 0
    let lastT = Date.now()
    const progTimer = window.setInterval(async () => {
      try {
        const p = await chatMediaProgress(meta.h)
        const now = Date.now()
        const speed = Math.max(0, ((p.bytes - lastB) * 1000) / Math.max(1, now - lastT))
        lastB = p.bytes
        lastT = now
        const pct = total > 0 ? Math.min(99, Math.floor((p.bytes * 100) / total)) : 0
        audProg.value = { ...audProg.value, [meta.h]: { pct, label: `${pct}% · ${fmtSize(speed)}/s` } }
      } catch {}
    }, 600)
    try {
      b64 = await loadChatAudio(active.value?.id || '', meta.h, meta.n, meta.m || 'audio/mpeg')
      audCache.set(meta.h, b64)
    } catch (e: any) {
      const code = typeof e === 'string' ? e : e?.message || ''
      showToast(
        code.includes('media-cancelled') ? t('messenger.audCancelled')
          : code.includes('media-timeout') ? t('messenger.audTimeout')
          : t('messenger.audLoadFailed'),
      )
      return
    } finally {
      window.clearInterval(progTimer)
      const { [meta.h]: _drop, ...rest } = audProg.value
      audProg.value = rest
      audFetching.value.delete(meta.h)
      audFetching.value = new Set(audFetching.value)
    }
  }
  const el = new Audio('data:' + (meta.m || 'audio/mpeg') + ';base64,' + b64)
  audioEl = el
  player.value = { hash: meta.h, name: meta.v ? t('messenger.voiceMsg') : (meta.f || 'audio.mp3'), playing: true, cur: 0, dur: meta.d || 0 }
  el.addEventListener('loadedmetadata', () => {
    if (audioEl !== el || !player.value) return
    if (isFinite(el.duration) && el.duration > 0) {
      player.value.dur = Math.round(el.duration * 1000)
    } else {
      // Chromium: WebM от MediaRecorder без cues даёт duration=Infinity и не
      // перематывается. Хак: прыжок в «бесконечность» заставляет браузер
      // вычислить реальную длительность (данные полностью в data:-URL).
      const onDur = () => {
        if (isFinite(el.duration) && el.duration > 0) {
          el.removeEventListener('durationchange', onDur)
          if (el.currentTime > 1e9) el.currentTime = 0
          if (audioEl === el && player.value) player.value.dur = Math.round(el.duration * 1000)
        }
      }
      el.addEventListener('durationchange', onDur)
      el.currentTime = 1e10
    }
  })
  el.addEventListener('timeupdate', () => {
    if (player.value && audioEl === el && el.currentTime < 1e9) player.value.cur = Math.round(el.currentTime * 1000)
  })
  el.addEventListener('ended', () => {
    if (player.value && audioEl === el) { player.value.playing = false; player.value.cur = player.value.dur }
  })
  el.play().catch(() => stopPlayer())
}

function togglePlayerPause() {
  if (!audioEl || !player.value) return
  if (audioEl.paused) { audioEl.play(); player.value.playing = true }
  else { audioEl.pause(); player.value.playing = false }
}

// Перемотка кликом по прогресс-полосе верхней панели плеера.
function seekPlayer(e: MouseEvent) {
  if (!audioEl || !player.value || !player.value.dur) return
  const el = e.currentTarget as HTMLElement
  const frac = Math.min(1, Math.max(0, (e.clientX - el.getBoundingClientRect().left) / el.clientWidth))
  audioEl.currentTime = (frac * player.value.dur) / 1000
  player.value.cur = Math.round(frac * player.value.dur)
}

function audState(text: string): 'playing' | 'paused' | 'loading' | 'idle' {
  const meta = audMeta(text)
  if (!meta) return 'idle'
  if (audFetching.value.has(meta.h)) return 'loading'
  if (player.value?.hash === meta.h) return player.value.playing ? 'playing' : 'paused'
  return 'idle'
}

// ===== Прогресс докачки медиа: hash -> {pct, label «34% · 1.2 MB/s»} =====
const audProg = ref<Record<string, { pct: number; label: string }>>({})
function audPct(text: string): number {
  const meta = audMeta(text)
  return (meta && audProg.value[meta.h]?.pct) || 0
}
function audProgLabel(text: string): string {
  const meta = audMeta(text)
  return (meta && audProg.value[meta.h]?.label) || ''
}

function audName(text: string): string {
  const meta = audMeta(text)
  if (!meta) return ''
  return meta.v ? t('messenger.voiceMsg') : (meta.f || 'audio.mp3')
}

// ===== Звуковая волна в бабле (Telegram-style) с перемоткой по клику =====
const waveCache = new Map<string, number[]>()
function waveBars(text: string): number[] {
  const meta = audMeta(text)
  if (!meta || !meta.w) return []
  let bars = waveCache.get(meta.h)
  if (!bars) {
    bars = meta.w.split('').map((c) => parseInt(c, 16) || 0)
    waveCache.set(meta.h, bars)
  }
  return bars
}
function wavePlayed(text: string): number {
  const meta = audMeta(text)
  if (!meta || player.value?.hash !== meta.h || !player.value.dur) return 0
  return Math.ceil((player.value.cur / player.value.dur) * waveBars(text).length)
}

function seekTo(frac: number) {
  if (!audioEl || !player.value) return
  const durSec = isFinite(audioEl.duration) && audioEl.duration > 0 ? audioEl.duration : player.value.dur / 1000
  if (durSec > 0) {
    audioEl.currentTime = frac * durSec
    player.value.cur = Math.round(frac * durSec * 1000)
  }
}

// Клик по волне: активный трек — перемотка (и play, если на паузе);
// неактивный — запуск с указанной позиции (как в Telegram).
async function onWaveClick(text: string, e: MouseEvent) {
  const meta = audMeta(text)
  if (!meta) return
  const box = e.currentTarget as HTMLElement
  const frac = Math.min(1, Math.max(0, (e.clientX - box.getBoundingClientRect().left) / box.clientWidth))
  if (player.value?.hash === meta.h && audioEl) {
    seekTo(frac)
    if (audioEl.paused) { void audioEl.play(); player.value.playing = true }
    return
  }
  await toggleAudio(text)
  if (player.value?.hash === meta.h && audioEl) seekTo(frac)
}

// ===== Голосовые сообщения (MediaRecorder → WebM/Opus). Запись НЕ уходит
// автоматически: стоп кладёт её в превью, отправка — только кнопкой. =====
const recording = ref(false)
const recElapsed = ref(0)
let recorder: MediaRecorder | null = null
let recChunks: Blob[] = []
let recTimer: number | null = null
let recCancelled = false
const REC_MAX_MS = 10 * 60 * 1000

async function toggleRecording() {
  if (recording.value) { stopRecording(false); return }
  if (pendingAud.value || pendingImg.value) return
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true })
    const mime = MediaRecorder.isTypeSupported('audio/webm;codecs=opus') ? 'audio/webm;codecs=opus' : 'audio/webm'
    const rec = new MediaRecorder(stream, { mimeType: mime, audioBitsPerSecond: 64000 })
    recorder = rec
    recChunks = []
    recCancelled = false
    rec.ondataavailable = (e) => { if (e.data.size) recChunks.push(e.data) }
    rec.onstop = async () => {
      stream.getTracks().forEach((tr) => tr.stop())
      const durMs = recElapsed.value
      recording.value = false
      if (recTimer) { window.clearInterval(recTimer); recTimer = null }
      if (recCancelled || !recChunks.length || durMs < 500) return
      const blob = new Blob(recChunks, { type: 'audio/webm' })
      if (blob.size > 25 * 1024 * 1024) { showToast(t('messenger.audTooBig')); return }
      const [buf, wave] = await Promise.all([blob.arrayBuffer(), computeWave(blob)])
      pendingAud.value = { b64: bufToB64(buf), name: 'voice.webm', durMs, size: blob.size, mime: 'audio/webm', voice: true, wave }
    }
    recElapsed.value = 0
    recording.value = true
    const started = Date.now()
    recTimer = window.setInterval(() => {
      recElapsed.value = Date.now() - started
      if (recElapsed.value >= REC_MAX_MS) stopRecording(false)
    }, 250)
    rec.start(250)
  } catch {
    showToast(t('messenger.micDenied'))
  }
}

function stopRecording(cancel: boolean) {
  recCancelled = cancel
  if (recorder && recorder.state !== 'inactive') recorder.stop()
  recorder = null
}

// ===== Кэш медиафайлов мессенджера (data/messenger) =====
const cacheInfo = ref<ChatCacheInfo>({ bytes: 0, files: 0, maxGb: 2, autoclean: true })
watch(showSettings, async (v) => {
  if (v) {
    try { cacheInfo.value = await chatCacheInfo() } catch {}
  }
})
async function saveCache() {
  try { await chatCacheSet(Number(cacheInfo.value.maxGb) || 2, cacheInfo.value.autoclean) } catch {}
}
async function clearCache() {
  try {
    await chatCacheClear()
    cacheInfo.value = await chatCacheInfo()
    showToast(t('messenger.cacheCleared'))
  } catch {}
}
function fmtMb(b: number): string { return (b / (1024 * 1024)).toFixed(1) }

async function confirmWipe() {
  if (!active.value) return
  await clearChatHistory(active.value.id)
  messages.value = []
  wipeAsk.value = false
  await refreshRooms()}

async function confirmLeave() {
  if (!active.value) return
  await deleteRoom(active.value.id)
  active.value = null
  messages.value = []
  leaveAsk.value = false
  await refreshRooms()
}

async function copyInvite() {
  if (!active.value) return
  try {
    const link = await groupInvite(active.value.id)
    copyText(link)
  } catch (e: any) {
    error.value = typeof e === 'string' ? e : e?.message || t('messenger.inviteFailed')
  }
}

async function doPublishKey() {
  publishing.value = true
  error.value = ''
  try {
    const res = await publishChatKey()
    await refreshKeyStatus()
    // явный результат — раньше «уже в цепи»/«debounce» выглядели как «ничего не произошло»
    if (res === 'already-on-chain') showToast(t('messenger.keyAlreadyOnChain'))
    else if (res === 'debounced') showToast(t('messenger.keyPublishDebounced'))
    else showToast(t('messenger.keyPublishedOk', { tx: String(res).slice(0, 10) }))
  } catch (e: any) {
    error.value = fmtErr(e, t('messenger.keyPublishFailed'))
  } finally {
    publishing.value = false
  }
}

function onMsgBodyClick(e: MouseEvent) {
  const a = (e.target as HTMLElement)?.closest?.('a[href]') as HTMLAnchorElement | null
  const href = a?.getAttribute('href') || ''
  if (!a || !/^https?:\/\//i.test(href)) return
  // v1.135.225: внешние ссылки — молча во web-вкладку NETFORY (без белого списка)
  e.preventDefault()
  openInNetfory(href)
}

function fmtTime(ts: number) {
  try {
    return new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  } catch {
    return ''
  }
}
// Contact-list stamp: time if <24h since last message, else date (dd.mm).
function fmtRoomTime(ts: number) {
  if (!ts) return ''
  try {
    const d = new Date(ts)
    if (Date.now() - ts > 86_400_000) {
      return d.toLocaleDateString([], { day: '2-digit', month: '2-digit' })
    }
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  } catch {
    return ''
  }
}
function closeWindow() {
  toggleChatWindow()
}
// «—»: в standalone-мессенджере сворачиваем окно; в Netfory команды нет → прежнее поведение (скрыть)
function minimizeWindow() {
  invoke('chat_window_minimize').catch(() => closeWindow())
}

let unlisten: (() => void) | null = null
let unlistenMsg: (() => void) | null = null
let unlistenPeers: (() => void) | null = null
let unlistenTyping: (() => void) | null = null
let unlistenAck: (() => void) | null = null
let unlistenVoice: (() => void) | null = null
let unlistenVoiceIn: (() => void) | null = null

// ---- v1.135.230: голосовой звонок 1-на-1 (Opus по QUIC-датаграммам iroh) ----
type VoiceState = 'idle' | 'calling' | 'ringing' | 'active'
const voice = ref<{ state: VoiceState; roomId: string; peerName: string; muted: boolean; since: number }>({
  state: 'idle', roomId: '', peerName: '', muted: false, since: 0,
})
const voiceElapsed = ref(0)
// v1.135.232: индикатор «кто говорит» (voice-speaking) — 1-на-1 и групповой канал
type SpeakingPeer = { id: string; addr?: string; speaking: boolean }
const speaking = ref<{ peers: SpeakingPeer[]; me: boolean }>({ peers: [], me: false })
let unlistenSpeaking: (() => void) | null = null
let unlistenVoiceInvite: (() => void) | null = null
function shortId(id: string): string { return id.length > 10 ? id.slice(0, 6) + '…' + id.slice(-4) : id }
/** Имя участника голосового канала: @имя по адресу (v1.135.235), иначе короткий EndpointId. */
function peerLabel(p: SpeakingPeer): string {
  if (p.addr) { ensureName(p.addr); return displayName(p.addr) }
  return shortId(p.id)
}
// v1.135.235: в группе кто-то вошёл в голосовой канал (voice-group-invite) → подсветка кнопки + тост
const liveVoice = ref<Record<string, number>>({})
const groupVoiceLive = computed(() => !!active.value && (liveVoice.value[active.value.id] || 0) > Date.now() - 90_000)
/** Android: системное разрешение на микрофон выдаётся WebView через getUserMedia (wry запрашивает
 *  RECORD_AUDIO/MODIFY_AUDIO_SETTINGS); без этого cpal/oboe не откроет захват. На десктопе — no-op. */
async function ensureMicPermission() {
  if (!/Android/i.test(navigator.userAgent) || !navigator.mediaDevices?.getUserMedia) return
  try {
    const st = await navigator.mediaDevices.getUserMedia({ audio: true })
    st.getTracks().forEach((tr) => tr.stop())
  } catch { /* откажет cpal → voice-state audio-error */ }
}
async function joinGroupVoice() {
  if (!active.value || !isGroup.value || voiceBusy.value) return
  voice.value = { state: 'calling', roomId: active.value.id, peerName: active.value.peerName || t('messenger.groupVoice'), muted: false, since: 0 }
  try {
    await ensureMicPermission()
    await invoke('voice_group_join', { roomId: active.value.id })
  } catch (e: any) {
    voiceReset()
    toasts.push(t('messenger.groupVoice'), String(e?.message || e), 'error')
  }
}
function isMissedCall(m: { text: string; mine: boolean }): boolean {
  return !m.mine && typeof m.text === 'string' && m.text.startsWith('☎ Пропущенный звонок')
}
let voiceTimer: number | null = null
const voiceBusy = computed(() => voice.value.state !== 'idle')
function voiceLabel(roomId: string): string {
  const r = rooms.value.find((x) => x.id === roomId)
  return r?.peerName ? '@' + r.peerName : t('messenger.citizen')
}
function voiceTick() {
  voiceElapsed.value = voice.value.since ? Math.floor((Date.now() - voice.value.since) / 1000) : 0
}
function voiceReset() {
  voice.value = { state: 'idle', roomId: '', peerName: '', muted: false, since: 0 }
  if (voiceTimer) { window.clearInterval(voiceTimer); voiceTimer = null }
  speaking.value = { peers: [], me: false }
}
async function startCall() {
  if (!active.value || isGroup.value || voiceBusy.value) return
  voice.value = { state: 'calling', roomId: active.value.id, peerName: voiceLabel(active.value.id), muted: false, since: 0 }
  try {
    await ensureMicPermission()
    await invoke('voice_call', { roomId: active.value.id })
  } catch (e: any) {
    voiceReset()
    toasts.push(t('messenger.callTitle'), String(e?.message || e), 'error')
  }
}
async function answerCall(accept: boolean) {
  if (accept) await ensureMicPermission()
  try { await invoke('voice_answer', { accept }) } catch {}
  if (!accept) voiceReset()
}
async function hangup() {
  try { await invoke('voice_hangup') } catch {}
  voiceReset()
}
async function toggleMute() {
  await setMute(!voice.value.muted)
}
async function setMute(on: boolean) {
  try { voice.value.muted = (await invoke<boolean>('voice_mute', { on })) } catch {}
}
// Android (Telecom): маршрут звука — разговорный динамик ↔ громкая связь
const isAndroid = typeof navigator !== 'undefined' && /Android/i.test(navigator.userAgent)
const speakerOn = ref(false)
async function toggleSpeaker() {
  const next = !speakerOn.value
  try {
    await invoke('voice_audio_route', { route: next ? 'speaker' : 'earpiece' })
    speakerOn.value = next
  } catch {}
}
// ---- v1.135.231: настройки аудио (устройства, уровень микрофона, RNNoise) ----
const audioOpen = ref(false)
const moreOpen = ref(false)
watch(() => active.value?.id, () => { moreOpen.value = false })
const audio = ref<{ inputs: string[]; outputs: string[]; input: string; output: string; denoise: boolean; aec: boolean; agc: boolean }>({
  inputs: [], outputs: [], input: '', output: '', denoise: true, aec: true, agc: true,
})
const micLevel = ref(0)
let unlistenLevel: (() => void) | null = null
async function openAudio() {
  if (audioOpen.value) { closeAudio(); return }
  try {
    audio.value = await invoke('voice_audio_settings')
  } catch (e: any) {
    toasts.push(t('messenger.audioTitle'), String(e?.message || e), 'error')
    return
  }
  audioOpen.value = true
  micLevel.value = 0
  unlistenLevel = await appListen<{ level?: number }>('voice-level', (p) => { micLevel.value = Math.max(0, Math.min(1, Number(p?.level || 0))) })
  invoke('voice_meter', { on: true }).catch(() => {})
}
function closeAudio() {
  audioOpen.value = false
  invoke('voice_meter', { on: false }).catch(() => {})
  if (unlistenLevel) { unlistenLevel(); unlistenLevel = null }
}
async function saveAudio(patch: { input?: string; output?: string; denoise?: boolean; aec?: boolean; agc?: boolean }) {
  Object.assign(audio.value, patch)
  try { await invoke('voice_set_audio', patch) } catch {}
  if (patch.input !== undefined) {
    // перезапуск индикатора на новом микрофоне
    await invoke('voice_meter', { on: false }).catch(() => {})
    setTimeout(() => invoke('voice_meter', { on: true }).catch(() => {}), 250)
  }
}

function onVoiceState(p: { roomId?: string; state?: string; reason?: string }) {
  const rid = p?.roomId || ''
  if (p?.state === 'active') {
    voice.value = { ...voice.value, state: 'active', roomId: rid, peerName: voiceLabel(rid), since: Date.now() }
    if (!voiceTimer) voiceTimer = window.setInterval(voiceTick, 1000)
  } else if (p?.state === 'ended') {
    const reasonKey: Record<string, string> = {
      busy: 'messenger.callBusy', declined: 'messenger.callDeclined', 'no-answer': 'messenger.callNoAnswer',
      unreachable: 'messenger.callUnreachable', missed: 'messenger.callMissed', 'audio-error': 'messenger.callAudioError',
    }
    const k = reasonKey[p.reason || '']
    if (k) toasts.push(t('messenger.callTitle'), t(k, { name: voiceLabel(rid) }), p.reason === 'missed' ? 'info' : 'error')
    voiceReset()
  } else if (p?.state === 'calling' || p?.state === 'ringing') {
    voice.value = { ...voice.value, state: p.state as VoiceState, roomId: rid, peerName: voiceLabel(rid) }
  }
}
let unlistenReact: (() => void) | null = null
let unlistenEdit: (() => void) | null = null
// ===== dropzone: перетаскивание файла в окно чата (как в Telegram) =====
// Tauri перехватывает OS-drop до DOM — слушаем нативные drag-события webview.
const dropActive = ref(false)
let unDrop: (() => void) | null = null

const DROP_MIME: Record<string, string> = {
  jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png', webp: 'image/webp', gif: 'image/gif',
  mp3: 'audio/mpeg', m4a: 'audio/mp4',
  mp4: 'video/mp4', m4v: 'video/mp4',
  pdf: 'application/pdf', md: 'text/markdown', markdown: 'text/markdown',
}

async function handleDroppedPaths(paths: string[]) {
  dropActive.value = false
  if (!active.value || !paths.length) return
  const path = paths[0]
  const name = path.split(/[\\/]/).pop() || 'file'
  const ext = (name.split('.').pop() || '').toLowerCase()
  const mime = DROP_MIME[ext]
  if (!mime) { showToast(t('messenger.mediaUnsupported')); return }
  try {
    const b64 = await chatReadDropped(path)
    const bin = atob(b64)
    const bytes = new Uint8Array(bin.length)
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
    const f = new File([bytes], name, { type: mime })
    if (mime.startsWith('image/')) prepareImage(f)
    else if (mime.startsWith('audio/')) prepareAudio(f)
    else if (mime.startsWith('video/')) prepareVideo(f)
    else prepareFile(f)
  } catch (e: any) {
    const code = typeof e === 'string' ? e : e?.message || ''
    showToast(code.includes('drop-too-big') ? t('messenger.dropTooBig') : code || t('messenger.sendFailed'))
  }
}

// Mycelium (NIP-1): личные сообщения идут через mushroom-цепочку — индикатор в шапке, подсказка у звонка
const viaMycelium = ref(false)
const myceliumReady = ref(false)
let myceliumTimer: ReturnType<typeof setInterval> | null = null
async function loadMycelium() {
  const s = await myceliumStatus()
  viaMycelium.value = !!s?.settings?.enabled
  myceliumReady.value = (s?.status?.circuitsReady ?? 0) > 0
}
onMounted(() => {
  void loadMycelium()
  window.addEventListener('focus', loadMycelium)
  myceliumTimer = setInterval(() => void loadMycelium(), 20000)
})
// карта цепочки собеседника: по клику на бейдж, живое обновление раз в 3 с
const showCircuit = ref(false)
const peerCircuit = ref<MyceliumCircuit | null>(null)
const circuitErr = ref('')
let circuitTimer: ReturnType<typeof setInterval> | null = null
async function loadPeerCircuit() {
  const addr = active.value?.peerAddress
  peerCircuit.value = addr ? await myceliumCircuit(addr) : null
}
watch(() => active.value?.id, () => void loadMycelium())
function openCircuit() {
  showCircuit.value = true
  circuitErr.value = ''
  void loadMycelium()
  void loadPeerCircuit()
  if (!circuitTimer) circuitTimer = setInterval(() => void loadPeerCircuit(), 3000)
}
watch(showCircuit, (v) => {
  if (!v && circuitTimer) {
    clearInterval(circuitTimer)
    circuitTimer = null
  }
})
onBeforeUnmount(() => {
  window.removeEventListener('focus', loadMycelium)
  if (myceliumTimer) clearInterval(myceliumTimer)
  if (circuitTimer) clearInterval(circuitTimer)
})

onMounted(async () => {
  try {
    onNativeFileDrop((p) => {
      if (p.type === 'enter' || p.type === 'over') dropActive.value = !!active.value
      else if (p.type === 'leave') dropActive.value = false
      else if (p.type === 'drop') void handleDroppedPaths((p as any).paths || [])
    }).then((un) => { unDrop = un }).catch(() => {})
    try { const fs = Number(localStorage.getItem('sth_chat_fontsize')); if (fs >= 12 && fs <= 20) fontSize.value = fs } catch {}
    try { const v = await loadVault(); myAddress.value = v?.address || wallet.account?.address || '' } catch { myAddress.value = wallet.account?.address || '' }
    try { hostAvailable.value = await invoke<boolean>('msg_host_available') } catch { hostAvailable.value = true }
    await refreshSetupState()
    // Онбординг: нет имени или ключа → показываем (если не отложен за последние 12ч).
    if (setupIncomplete.value && hostAvailable.value) {
      let snoozed = 0
      try { snoozed = Number(localStorage.getItem(LS_ONB_SNOOZE) || 0) } catch {}
      showOnboarding.value = Date.now() - snoozed > 12 * 60 * 60_000
      setupPoll = setInterval(async () => {
        await refreshSetupState()
        if (!setupIncomplete.value && setupPoll) { clearInterval(setupPoll); setupPoll = null }
      }, 15_000)
    }
    await loadSettings()
    estimatePublishFee().then((f) => { publishFee.value = f })
    await refreshRooms()
    await refreshStatus()
    refreshMyBans()
    pruneBanTable().catch(() => {})
    if (wallet.account?.address) {
      following.value = (await followStats(wallet.account.address)).followingList
    }
    if (rooms.value.length) selectRoom(rooms.value[0])
  } catch (e: any) {
    error.value = typeof e === 'string' ? e : e?.message || t('messenger.initFailed')
  } finally {
    loading.value = false
  }
  unlisten = await appListen<string>('chat-open-target', (target) => {
    if (target) {
      if (/^(sn|u):\/\/group\//i.test(target)) joinViaLink(target)
      else openConversation(target)
    }
  })
  // Live inbound (paid poller / gossip): refresh list + append to the open room.
  unlistenMesh = await appListen<{ state?: string; rooms?: number }>('chat-mesh', onMesh)
  unlistenName = await appListen<{ name?: string }>('name-confirmed', async (p) => {
    await refreshSetupState()
    if (p?.name) showToast(t('notifications.nameConfirmed', { name: p.name }))
  })
  unlistenMsg = await appListen<{ roomId?: string }>('chat-incoming', async (p) => {
    if (active.value && p?.roomId === active.value.id) {
      await refreshTail(active.value.id)
      scrollFeed(false) // не листаем вниз, если пользователь читает историю
    }
    await refreshRooms()
  })
  // Live peer presence (gossip neighbor up/down) on the active room's topic.
  unlistenPeers = await appListen<{ roomId?: string; neighbors?: number }>('chat-peers', (p) => {
    if (active.value && p?.roomId === active.value.id) {
      peerCount.value = p.neighbors || 0
      peerOnline.value = peerCount.value > 0
    }
  })
  // Живые квитанции доставки (E2EE sys="ack" от получателей).
  unlistenAck = await appListen<{ room?: string; id?: string; count?: number }>('chat-ack', (p) => {
    if (active.value && p?.room === active.value.id && p.id) {
      acks.value = { ...acks.value, [p.id]: p.count || 0 }
    }
  })
  // Живые реакции на сообщения (эфемерный gossip sys="react").
  unlistenReact = await appListen<{ room?: string; id?: string; reactions?: MsgReaction[] }>('chat-react', (p) => {
    if (active.value && p?.room === active.value.id && p.id) {
      reactions.value = { ...reactions.value, [p.id]: p.reactions || [] }
    }
  })
  // Автор отредактировал сообщение (sys="edit") — подменяем текст на месте.
  unlistenEdit = await appListen<{ room?: string; id?: string; text?: string; edited?: number }>('chat-edit', (p) => {
    if (active.value && p?.room === active.value.id && p.id) {
      messages.value = messages.value.map((x) => (x.id === p.id ? { ...x, text: p.text ?? x.text, edited: p.edited ?? Date.now() } : x))
    }
  })
  // Живой «печатает…» активной комнаты (эфемерный gossip-сигнал).
  unlistenVoice = await appListen<{ roomId?: string; state?: string; reason?: string }>('voice-state', onVoiceState)
  unlistenSpeaking = await appListen<{ peers?: SpeakingPeer[]; me?: boolean }>('voice-speaking', (p) => {
    speaking.value = { peers: p?.peers || [], me: !!p?.me }
  })
  unlistenVoiceInvite = await appListen<{ roomId?: string; from?: string }>('voice-group-invite', (p) => {
    const rid = p?.roomId || ''
    if (!rid) return
    liveVoice.value = { ...liveVoice.value, [rid]: Date.now() }
    const room = rooms.value.find((x) => x.id === rid)
    if (p?.from) ensureName(p.from)
    toasts.push(t('messenger.groupVoice'), t('messenger.groupVoiceStarted', { name: p?.from ? displayName(p.from) : t('messenger.citizen'), room: room?.peerName || '' }), 'info')
  })
  unlistenVoiceIn = await appListen<{ roomId?: string }>('voice-incoming', (p) => {
    const rid = p?.roomId || ''
    voice.value = { state: 'ringing', roomId: rid, peerName: voiceLabel(rid), muted: false, since: 0 }
  })
  unlistenTyping = await appListen<{ roomId?: string; sender?: string }>('chat-typing', (p) => {
    if (active.value && p?.roomId === active.value.id) {
      typingActive.value = true
      if (typingHideTimer) window.clearTimeout(typingHideTimer)
      typingHideTimer = window.setTimeout(() => (typingActive.value = false), 3500)
    }
  })
  // Poll the transit engine status for the HUD — but pause while the window is
  // hidden so an idle messenger doesn't keep waking the backend / WebView2 IPC
  // (root of the long-idle "Not Responding" freeze on low-core Windows/RDP).
  statusTimer = window.setInterval(() => {
    if (!document.hidden) refreshStatus()
  }, 5000)
  // ban self-detection runs on a slower cadence (hits the chain)
  banTimer = window.setInterval(() => {
    if (!document.hidden && useNetStore().online) refreshMyBans()
  }, 60000)
})
onBeforeUnmount(() => {
  stopRecording(true)
  stopPlayer()
  if (setupPoll) clearInterval(setupPoll)
  if (errorTimer) clearTimeout(errorTimer)
  if (meshTimer) clearTimeout(meshTimer)
  if (unlistenMesh) unlistenMesh()
  if (unlistenName) unlistenName()
  if (unDrop) unDrop()
  if (unlisten) unlisten()
  if (unlistenMsg) unlistenMsg()
  if (unlistenPeers) unlistenPeers()
  if (unlistenTyping) unlistenTyping()
  if (unlistenVoice) unlistenVoice()
  if (unlistenVoiceIn) unlistenVoiceIn()
  if (unlistenSpeaking) unlistenSpeaking()
  if (unlistenVoiceInvite) unlistenVoiceInvite()
  if (voiceTimer) window.clearInterval(voiceTimer)
  if (audioOpen.value) closeAudio()
  if (unlistenAck) unlistenAck()
  if (unlistenReact) unlistenReact()
  if (unlistenEdit) unlistenEdit()
  if (typingHideTimer) window.clearTimeout(typingHideTimer)
  if (statusTimer) window.clearInterval(statusTimer)
  if (banTimer) window.clearInterval(banTimer)
})
watch(active, () => {
  typingActive.value = false
  replyTo.value = null
  scrollFeed()
})

// ===== Мобильный layout (v1.135.236): ≤760px — список чатов и переписка на отдельных экранах
const isMobile = ref(false)
const mobilePane = ref<'list' | 'chat'>('list')
const mobileMq = typeof window !== 'undefined' && window.matchMedia ? window.matchMedia('(max-width: 760px)') : null
function onMobileMq() {
  isMobile.value = !!mobileMq?.matches
}
onMobileMq()
mobileMq?.addEventListener?.('change', onMobileMq)
onBeforeUnmount(() => mobileMq?.removeEventListener?.('change', onMobileMq))
watch(() => active.value?.id, (id) => {
  if (id) mobilePane.value = 'chat'
})
function mobileBack() {
  mobilePane.value = 'list'
}

// ===== Android-мост (MainActivity.kt): системная «назад» и действия экрана входящего звонка
declare global {
  interface Window {
    __ntfryBack?: () => boolean
    __ntfryCallAction?: { action: string; room: string } | null
  }
}
/** true — «назад» обработана в UI (закрыли модалку / вернулись к списку); false — свернуть приложение. */
function androidBack(): boolean {
  if (lightbox.value.open) { lightbox.value.open = false; return true }
  if (ctxMenu.value.open) { ctxMenu.value.open = false; return true }
  if (showEmoji.value) { showEmoji.value = false; return true }
  if (audioOpen.value) { audioOpen.value = false; return true }
  const modals = [showCircuit, showSettings, showGroupModal, showShareContact, showAddContact, showAdmin, showDiscussions, showOnboarding]
  for (const m of modals) if (m.value) { m.value = false; return true }
  if (isMobile.value && mobilePane.value === 'chat') { mobileBack(); return true }
  return false
}
/** Кнопки «Ответить/Отклонить» из уведомления или полноэкранного экрана входящего звонка. */
function consumeCallAction() {
  const a = window.__ntfryCallAction
  if (!a) return
  const st = voice.value.state
  switch (a.action) {
    case 'answer':
    case 'decline':
      if (st !== 'ringing') return // звонок ещё не восстановлен — дождёмся voice-incoming
      window.__ntfryCallAction = null
      void answerCall(a.action === 'answer')
      return
    case 'hangup': // гарнитура / Telecom (сотовый звонок прервал)
      window.__ntfryCallAction = null
      if (st === 'ringing') void answerCall(false)
      else if (st !== 'idle') void hangup()
      return
    case 'mute':
    case 'unmute': // удержание из Telecom → глушим/возвращаем микрофон
      window.__ntfryCallAction = null
      if (st === 'active') void setMute(a.action === 'mute')
      return
    default:
      window.__ntfryCallAction = null
  }
}
watch(() => voice.value.state, (st) => { if (st === 'idle') speakerOn.value = false })

// ===== Push-to-talk (шумное место): микрофон закрыт, говорим пока зажата клавиша / экранная кнопка
const PTT_KEY = 'ntfry.ptt'
const ptt = ref<{ enabled: boolean; key: string }>({ enabled: false, key: 'AltRight' })
try { Object.assign(ptt.value, JSON.parse(localStorage.getItem(PTT_KEY) || '{}')) } catch {}
const pttCapture = ref(false)
const pttHeld = ref(false)
function savePtt(patch: Partial<{ enabled: boolean; key: string }>) {
  ptt.value = { ...ptt.value, ...patch }
  localStorage.setItem(PTT_KEY, JSON.stringify(ptt.value))
  if (voice.value.state === 'active') void setMute(ptt.value.enabled && !pttHeld.value)
}
function pttKeyLabel(code: string): string {
  return code.replace(/^Key|^Digit/, '').replace('AltRight', 'Right Alt').replace('AltLeft', 'Left Alt').replace('ControlRight', 'Right Ctrl').replace('ControlLeft', 'Left Ctrl').replace('ShiftRight', 'Right Shift').replace('ShiftLeft', 'Left Shift')
}
function isTypingTarget(e: KeyboardEvent): boolean {
  const el = e.target as HTMLElement | null
  return !!el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable)
}
async function pttDown() {
  if (pttHeld.value) return
  pttHeld.value = true
  if (voice.value.state === 'active') await setMute(false)
}
async function pttUp() {
  if (!pttHeld.value) return
  pttHeld.value = false
  if (voice.value.state === 'active') await setMute(true)
}
function onPttKeyDown(e: KeyboardEvent) {
  if (pttCapture.value) {
    e.preventDefault()
    if (e.code !== 'Escape') savePtt({ key: e.code })
    pttCapture.value = false
    return
  }
  if (!ptt.value.enabled || e.code !== ptt.value.key || e.repeat) return
  if (isTypingTarget(e) && !e.code.startsWith('Alt') && !e.code.startsWith('Control') && !e.code.startsWith('Shift')) return
  e.preventDefault()
  void pttDown()
}
function onPttKeyUp(e: KeyboardEvent) {
  if (ptt.value.enabled && e.code === ptt.value.key) void pttUp()
}
onMounted(() => {
  window.addEventListener('keydown', onPttKeyDown)
  window.addEventListener('keyup', onPttKeyUp)
  window.addEventListener('blur', () => void pttUp())
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', onPttKeyDown)
  window.removeEventListener('keyup', onPttKeyUp)
})
// звонок стал активным → в режиме PTT сразу закрываем микрофон
watch(() => voice.value.state, (st) => {
  if (st === 'active' && ptt.value.enabled) void setMute(true)
  if (st === 'idle') pttHeld.value = false
})

// ===== Свайп вправо в переписке → список чатов (телефон)
let swipe: { x: number; y: number; t: number } | null = null
function onSwipeStart(e: TouchEvent) {
  if (!isMobile.value || e.touches.length !== 1) { swipe = null; return }
  const t = e.touches[0]
  swipe = { x: t.clientX, y: t.clientY, t: Date.now() }
}
function onSwipeEnd(e: TouchEvent) {
  if (!swipe) return
  const t = e.changedTouches[0]
  const dx = t.clientX - swipe.x
  const dy = Math.abs(t.clientY - swipe.y)
  const dt = Date.now() - swipe.t
  swipe = null
  if (dx > 90 && dy < 70 && dt < 700 && dx > dy * 1.8) mobileBack()
}
function onCallActionEvent() { consumeCallAction() }
onMounted(() => {
  window.__ntfryBack = androidBack
  window.addEventListener('ntfry-call-action', onCallActionEvent)
  // холодный старт из уведомления: действие уже записано, звонок восстановится через voice-incoming/voice_state
  setTimeout(consumeCallAction, 1500)
})
onBeforeUnmount(() => {
  if (window.__ntfryBack === androidBack) delete window.__ntfryBack
  window.removeEventListener('ntfry-call-action', onCallActionEvent)
})
watch(() => voice.value.state, (st) => { if (st === 'ringing') consumeCallAction() })
</script>

<template>
  <div class="cw noselect" :class="{ mobile: isMobile, 'm-list': isMobile && mobilePane === 'list', 'm-chat': isMobile && mobilePane === 'chat' }" :style="{ '--chat-text-size': fontSize + 'px' }" data-testid="chat-widget">
    <!-- custom borderless titlebar -->
    <div class="titlebar" data-tauri-drag-region>
      <div class="tb-left" data-tauri-drag-region>
        <span class="tb-title" data-tauri-drag-region>NETFORY // P2P MESSENGER</span>
        <div
          class="p2p-hud"
          :class="{ on: p2p.online }"
          :title="p2p.p2p ? (p2p.online ? t('messenger.transitActive', { n: p2p.topics }) : t('messenger.transitOffline')) : t('messenger.p2pDisabled')"
          data-tauri-drag-region
          data-testid="chat-p2p-status"
        >
          <span class="hud-dot" />
          <span class="hud-label">{{ p2p.online ? 'ONLINE' : 'OFFLINE' }}</span>
          <span class="hud-topics" data-testid="chat-p2p-topics">◇ {{ p2p.topics }}</span>
          <span
            v-if="meshWarn"
            class="hud-warn"
            :title="t('messenger.meshWarn', { r: p2p.resubscribing, q: p2p.outbox })"
            data-testid="chat-mesh-warn"
          />
        </div>
      </div>
      <div class="tb-ctrls">
        <button class="tb-btn font" @click="adjustFont(-1)" :title="t('messenger.fontDec')" data-testid="chat-font-dec">A-</button>
        <button class="tb-btn font" @click="adjustFont(1)" :title="t('messenger.fontInc')" data-testid="chat-font-inc">A+</button>
        <button class="tb-btn" @click="openShareContact" :title="t('messenger.shareContact')" data-testid="chat-share-contact-btn">⤴</button>
        <button class="tb-btn" @click="openAddContact" :title="t('messenger.addContact')" data-testid="chat-add-contact-btn">⊕</button>
        <button class="tb-btn" @click="showSettings = true" :title="t('messenger.settings')" data-testid="chat-settings-btn">⚙</button>
        <button class="tb-btn" @click="minimizeWindow" :title="t('messenger.minimize')" data-testid="chat-min">—</button>
        <button class="tb-btn close" @click="closeWindow" :title="t('messenger.closeT')" data-testid="chat-close">✕</button>
      </div>
    </div>

    <div v-if="setupIncomplete && !showOnboarding && hostAvailable" class="keybar" data-testid="chat-keybar">
      <span>{{ t('messenger.setupBar', { left: [!myUsername && t('messenger.setupName'), !keyPublished && t('messenger.setupKey')].filter(Boolean).join(' + ') }) }}</span>
      <button class="kb-btn" @click="openOnboarding" data-testid="chat-setup-open">{{ t('messenger.setupBtn') }}</button>
    </div>
    <div v-else-if="!myUsername && !hostAvailable" class="keybar" data-testid="chat-standalone-noname">
      <span>{{ t('messenger.standaloneNoName', { addr: myAddress ? myAddress.slice(0, 10) + '…' : '—' }) }}</span>
      <button class="kb-btn" :disabled="chainCheckBusy" @click="checkNameOnChain" data-testid="chat-standalone-check-chain">{{ chainCheckBusy ? t('messenger.standaloneChecking') : t('messenger.standaloneCheckChain') }}</button>
    </div>

    <ChatOnboarding
      v-if="showOnboarding"
      :key-published="keyPublished"
      :publishing="publishing"
      :publish-fee="publishFee"
      :address="myAddress"
      @publish-key="doPublishKey"
      @close="snoozeOnboarding"
      @done="sendWelcome"
    />

    <transition name="toast-pop">
      <div v-if="meshState" class="meshbar" :class="meshState" data-testid="chat-meshbar">
        <span v-if="meshState === 'reconnecting'" class="mesh-spin" />
        {{ meshState === 'reconnecting' ? t('messenger.meshReconnecting', { n: meshRooms }) : t('messenger.meshReady') }}
      </div>
    </transition>

    <div v-if="loading" class="cw-loading" data-testid="chat-loading">
      <span class="spinner" />
      <span class="ld-text">{{ t('messenger.connectingTransit') }}</span>
    </div>

    <div class="split">
      <!-- LEFT: rooms -->
      <aside class="left">
        <div class="search">
          <input
            v-model="search"
            :placeholder="t('messenger.searchPlaceholder')"
            @keydown.enter="doSearch"
            data-testid="chat-search"
          />
          <button class="new-group" @click="showGroupModal = true" :title="t('messenger.newGroupTitle')" data-testid="chat-new-group">{{ t('messenger.groupBtn') }}</button>
        </div>
        <div class="tabs">
          <button class="tab" :class="{ on: tab === 'all' }" @click="tab = 'all'" data-testid="chat-tab-all">{{ t('messenger.tabAll') }}</button>
          <button class="tab" :class="{ on: tab === 'subs' }" @click="tab = 'subs'" data-testid="chat-tab-subs">{{ t('messenger.tabSubs') }}</button>
        </div>

        <div v-if="tab === 'subs' && !rooms.length" class="contacts">
          <div
            v-for="f in following"
            :key="f.address"
            class="contact"
            role="button"
            tabindex="0"
            @click="openContact(f)"
            @keydown.enter="openConversation(f.name ? 'u://' + f.name : f.address)"
            :data-testid="`chat-contact-${f.address}`"
          >
            <UserAvatar :address="f.address" :size="32" />
            <span
              class="c-name c-name-link"
              :title="t('messenger.ctxProfile')"
              @click.stop="openUserProfile(f.address, f.name)"
              :data-testid="`chat-contact-profile-${f.address}`"
            >{{ f.name ? '@' + f.name : f.address.slice(0, 10) + '…' }}</span>
          </div>
        </div>

        <div class="rooms" data-testid="chat-rooms">
          <button
            v-for="r in visibleRooms"
            :key="r.id"
            class="room"
            :class="{ active: active && active.id === r.id, 'room-banned': r.kind === 'group' && isBannedHere(r.id) }"
            @click="selectRoom(r)"
            :data-testid="`chat-room-${r.peerAddress}`"
          >
            <span class="r-av" :class="{ offline: !isRoomOnline(r) }" :data-testid="`chat-room-av-${r.peerAddress}`">
              <img v-if="r.kind === 'group' && groupLogos[r.id]" class="r-logo" :src="groupLogos[r.id]" alt="" />
              <UserAvatar v-else :address="r.peerAddress" :size="34" />
            </span>
            <div class="r-meta">
              <div class="r-top">
                <span class="r-name">
                  <span v-if="r.kind === 'group'" class="r-group-ic" :title="t('messenger.group')">⛬</span>
                  {{ r.kind === 'group' ? (r.peerName || t('messenger.group')) : (r.peerName ? '@' + r.peerName : r.peerAddress.slice(0, 12) + '…') }}
                </span>
                <span class="r-time" :data-testid="`chat-room-time-${r.peerAddress}`">{{ fmtRoomTime(r.lastTs) }}</span>
              </div>
              <div class="r-bot">
                <span v-if="r.kind === 'group' && isBannedHere(r.id)" class="r-banned-tag" :data-testid="`chat-room-banned-${r.peerAddress}`">{{ t('messenger.roomBanUntil', { date: fmtBanUntil(bannedUntil(r.id)) }) }}</span>
                <span v-else class="r-last">{{ previewText(r.lastText) }}</span>
                <span v-if="r.unread > 0 && !isBannedHere(r.id)" class="r-badge" :data-testid="`chat-unread-${r.peerAddress}`">{{ r.unread > 99 ? '99+' : r.unread }}</span>
              </div>
            </div>
          </button>
          <p v-if="!visibleRooms.length && tab === 'all'" class="empty">{{ t('messenger.emptyRooms') }}</p>
        </div>
      </aside>

      <!-- RIGHT: conversation -->
      <section class="right" @touchstart.passive="onSwipeStart" @touchend.passive="onSwipeEnd">
        <template v-if="active">
          <header class="conv-head">
            <button v-if="isMobile" class="m-back" @click="mobileBack" :title="t('messenger.back')" data-testid="chat-mobile-back">‹</button>
            <div v-if="isGroup" class="gh-mosaic" :class="`n${Math.min((groupMembers.length || 1), 4)}`" data-testid="chat-group-mosaic">
              <span v-for="s in (groupMembers.length ? groupMembers : [active.peerAddress])" :key="s" class="gm-cell">
                <UserAvatar :address="s" :size="34" />
              </span>
            </div>
            <button
              v-else
              class="av av-lg av-btn"
              :title="t('messenger.ctxProfile')"
              @click="openUserProfile(active.peerAddress, active.peerName)"
              data-testid="chat-head-avatar"
            ><UserAvatar :address="active.peerAddress" :size="34" /></button>
            <div class="ch-meta">
              <span class="ch-name">
                <span class="ch-title" data-testid="chat-head-title"><span v-if="isGroup" class="r-group-ic">⛬</span>{{ active.peerName ? (isGroup ? active.peerName : '@' + active.peerName) : t('messenger.citizen') }}</span>
                <span class="ch-sub">
                <span v-if="isGroup" class="presence" :class="{ on: peerCount > 0 }" data-testid="chat-group-members">
                  <span class="pr-dot" />{{ typingActive ? t('messenger.typingSomeone') : t('messenger.membersOnline', { n: peerCount }) }}
                </span>
                <span v-if="isGroup" class="subs-count" data-testid="chat-subscriber-count" :title="t('messenger.subscribersTitle')">
                  {{ t('messenger.subscribersCount', { n: subscriberCount }) }}
                </span>
                <span v-else class="presence" :class="{ on: peerOnline || typingActive }" :title="peerOnline ? t('messenger.peerOnlineTitle') : t('messenger.peerOfflineTitle')" data-testid="chat-peer-presence">
                  <span class="pr-dot" :class="{ typing: typingActive }" />{{ typingActive ? t('messenger.typing') : (peerOnline ? t('messenger.peerOnline') : t('messenger.peerOffline')) }}
                </span>
                </span>
              </span>
              <code class="ch-addr">{{ isGroup ? t('messenger.groupE2ee') : active.peerAddress }}</code>
              <button v-if="viaMycelium && !isGroup" type="button" class="myc-badge" :class="{ wait: !myceliumReady }" :title="t('mycelium.showMap')" @click.stop="openCircuit" data-testid="chat-mycelium-badge"><i class="myc-dot" />{{ myceliumReady ? t('mycelium.badge') : t('mycelium.badgeWait') }}</button>
              <div v-if="isGroup && policy" class="ch-policy" data-testid="chat-group-policy">
                <span v-if="channelMode" class="pol-badge channel" data-testid="chat-channel-badge">{{ t('messenger.channelBadge') }}</span>
                <span v-if="policy.paidEntry" class="pol-badge fee">{{ t('messenger.entryBadge', { amount: (policy.entryFee / 1e8) }) }}</span>
                <span v-if="policy.inviteOnly" class="pol-badge">{{ t('messenger.inviteOnlyBadge') }}</span>
              </div>
            </div>
            <div class="conv-actions">
              <button
                v-if="!isGroup"
                class="call-btn"
                :class="{ busy: voiceBusy }"
                :disabled="voiceBusy"
                @click="startCall"
                :title="t('messenger.callTitle')"
                data-testid="chat-call-btn"
              >
                <svg viewBox="0 0 24 24" width="15" height="15" fill="currentColor"><path d="M6.6 10.8a15.1 15.1 0 0 0 6.6 6.6l2.2-2.2a1 1 0 0 1 1-.25c1.1.37 2.3.57 3.6.57a1 1 0 0 1 1 1V20a1 1 0 0 1-1 1A17 17 0 0 1 3 4a1 1 0 0 1 1-1h3.5a1 1 0 0 1 1 1c0 1.25.2 2.45.57 3.57a1 1 0 0 1-.25 1L6.6 10.8z" /></svg>
                <span class="cb-txt">{{ t('messenger.call') }}</span>
              </button>
              <button v-if="isGroup" class="call-btn" :class="{ live: groupVoiceLive && !voiceBusy }" :disabled="voiceBusy" @click="joinGroupVoice" :title="groupVoiceLive ? t('messenger.groupVoiceLive') : t('messenger.groupVoice')" data-testid="chat-group-voice-btn">
                <svg viewBox="0 0 24 24" width="15" height="15" fill="currentColor"><path d="M12 14a3 3 0 0 0 3-3V5a3 3 0 0 0-6 0v6a3 3 0 0 0 3 3zm5-3a5 5 0 0 1-10 0H5a7 7 0 0 0 6 6.92V21h2v-3.08A7 7 0 0 0 19 11h-2z" /></svg>
                <span class="cb-txt">{{ t('messenger.groupVoice') }}</span>
              </button>
              <!-- мобильный: остальные действия — в выпадающем меню «⋯» -->
              <button v-if="isMobile" class="admin-gear more-btn" :class="{ on: moreOpen }" @click="moreOpen = !moreOpen" :title="t('messenger.moreActions')" data-testid="chat-more-btn">⋯</button>
              <div class="acts" :class="{ open: moreOpen }" @click="moreOpen = false" data-testid="chat-head-actions">
              <button class="admin-gear" :class="{ on: audioOpen }" @click="openAudio" :title="t('messenger.audioTitle')" data-testid="chat-audio-btn">🎧<span class="act-txt">{{ t('messenger.audioTitle') }}</span></button>
              <button v-if="isGroup && isOwner" class="admin-gear" @click="openAdmin" :title="t('messenger.groupSettingsTitle')" data-testid="chat-admin-gear">⚙<span class="act-txt">{{ t('messenger.groupSettingsTitle') }}</span></button>
              <button v-if="isGroup" class="invite" @click.stop="copyInvite" data-testid="chat-copy-invite">
                {{ copied ? t('messenger.copied') : t('messenger.invite') }}
              </button>
              <button class="leave" @click="leaveAsk = true" :data-testid="isGroup ? 'chat-leave-group' : 'chat-delete-room'">
                {{ isGroup ? t('messenger.leaveGroup') : t('messenger.deleteChat') }}
              </button>
              <button class="wipe" @click="wipeAsk = true" data-testid="chat-wipe">{{ t(ui.mode === 'user' ? 'messenger.wipeLogsUser' : 'messenger.wipeLogs') }}</button>
              </div>
            </div>
          </header>

          <!-- v1.135.231: настройки аудио — микрофон/динамики, уровень, RNNoise -->
          <div v-if="audioOpen" class="audio-panel" data-testid="chat-audio-panel">
            <label class="ap-row">
              <span class="ap-l">{{ t('messenger.audioMic') }}</span>
              <select class="ap-sel" :value="audio.input" @change="saveAudio({ input: ($event.target as HTMLSelectElement).value })" data-testid="chat-audio-mic">
                <option value="">{{ t('messenger.audioDefault') }}</option>
                <option v-for="d in audio.inputs" :key="d" :value="d">{{ d }}</option>
              </select>
            </label>
            <div class="ap-row">
              <span class="ap-l">{{ t('messenger.audioLevel') }}</span>
              <div class="ap-meter" data-testid="chat-audio-level"><div class="ap-fill" :style="{ width: Math.round(micLevel * 100) + '%' }" :class="{ hot: micLevel > 0.85 }" /></div>
            </div>
            <label class="ap-row">
              <span class="ap-l">{{ t('messenger.audioSpk') }}</span>
              <select class="ap-sel" :value="audio.output" @change="saveAudio({ output: ($event.target as HTMLSelectElement).value })" data-testid="chat-audio-spk">
                <option value="">{{ t('messenger.audioDefault') }}</option>
                <option v-for="d in audio.outputs" :key="d" :value="d">{{ d }}</option>
              </select>
            </label>
            <label class="ap-row ap-toggle">
              <input type="checkbox" :checked="audio.denoise" @change="saveAudio({ denoise: ($event.target as HTMLInputElement).checked })" data-testid="chat-audio-denoise" />
              <span>{{ t('messenger.audioDenoise') }}</span>
              <span class="ap-hint">{{ t('messenger.audioDenoiseHint') }}</span>
            </label>
            <label class="ap-row ap-toggle">
              <input type="checkbox" :checked="audio.aec" @change="saveAudio({ aec: ($event.target as HTMLInputElement).checked })" data-testid="chat-audio-aec" />
              <span>{{ t('messenger.audioAec') }}</span>
              <span class="ap-hint">{{ t('messenger.audioAecHint') }}</span>
            </label>
            <label class="ap-row ap-toggle">
              <input type="checkbox" :checked="audio.agc" @change="saveAudio({ agc: ($event.target as HTMLInputElement).checked })" data-testid="chat-audio-agc" />
              <span>{{ t('messenger.audioAgc') }}</span>
              <span class="ap-hint">{{ t('messenger.audioAgcHint') }}</span>
            </label>
            <label class="ap-row ap-toggle">
              <input type="checkbox" :checked="ptt.enabled" @change="savePtt({ enabled: ($event.target as HTMLInputElement).checked })" data-testid="chat-audio-ptt" />
              <span>{{ t('messenger.pttToggle') }}</span>
              <span class="ap-hint">{{ t('messenger.pttToggleHint') }}</span>
            </label>
            <div v-if="ptt.enabled" class="ap-row ap-toggle" data-testid="chat-audio-ptt-key-row">
              <span>{{ t('messenger.pttKey') }}</span>
              <button class="call-act" :class="{ on: pttCapture }" @click="pttCapture = !pttCapture" data-testid="chat-audio-ptt-key">
                {{ pttCapture ? t('messenger.pttPressKey') : pttKeyLabel(ptt.key) }}
              </button>
              <span class="ap-hint">{{ t('messenger.pttKeyHint') }}</span>
            </div>
            <button class="call-act ap-close" @click="closeAudio" data-testid="chat-audio-close">{{ t('common.close') }}</button>
          </div>
          <!-- v1.135.230: панель голосового звонка (входящий / исходящий / активный) -->
          <div v-if="voiceBusy" class="call-bar" :class="voice.state" data-testid="chat-call-bar">
            <span class="call-ic" :class="{ pulse: voice.state !== 'active' }">●</span>
            <span class="call-who" :class="{ talking: !isGroup && speaking.peers.some((p) => p.speaking) }">{{ voice.peerName }}</span>
            <span v-if="voice.state === 'active' && speaking.me" class="spk-dot me" :title="t('messenger.youSpeaking')" data-testid="chat-call-me-speaking">●</span>
            <span v-if="isGroup && voice.state === 'active'" class="call-members" data-testid="chat-voice-members">
              <span v-for="p in speaking.peers" :key="p.id" class="member" :class="{ talking: p.speaking }" :title="p.id" :data-testid="`chat-voice-member-${p.id.slice(0, 8)}`">{{ peerLabel(p) }}</span>
              <span v-if="!speaking.peers.length" class="member empty">{{ t('messenger.groupVoiceAlone') }}</span>
            </span>
            <span class="call-st" data-testid="chat-call-state">
              {{ voice.state === 'ringing' ? t('messenger.callIncoming') : voice.state === 'calling' ? t('messenger.callCalling') : fmtDur(voiceElapsed * 1000) }}
            </span>
            <span v-if="viaMycelium && !isGroup" class="call-myc" data-testid="chat-call-mycelium-hint">{{ t('mycelium.callHint') }}</span>
            <span class="call-sp" />
            <template v-if="voice.state === 'ringing'">
              <button class="call-act accept" @click="answerCall(true)" data-testid="chat-call-accept">{{ t('messenger.callAccept') }}</button>
              <button class="call-act end" @click="answerCall(false)" data-testid="chat-call-decline">{{ t('messenger.callDecline') }}</button>
            </template>
            <template v-else>
              <button
                v-if="voice.state === 'active' && ptt.enabled"
                class="call-act ptt"
                :class="{ held: pttHeld }"
                @pointerdown.prevent="pttDown"
                @pointerup="pttUp"
                @pointerleave="pttUp"
                @pointercancel="pttUp"
                @contextmenu.prevent
                :title="t('messenger.pttHint', { key: pttKeyLabel(ptt.key) })"
                data-testid="chat-call-ptt"
              >
                {{ pttHeld ? t('messenger.pttTalking') : t('messenger.pttHold') }}
              </button>
              <button v-else-if="voice.state === 'active'" class="call-act" :class="{ on: voice.muted }" @click="toggleMute" data-testid="chat-call-mute">
                {{ voice.muted ? t('messenger.callUnmute') : t('messenger.callMute') }}
              </button>
              <button v-if="voice.state === 'active' && isAndroid" class="call-act" :class="{ on: speakerOn }" @click="toggleSpeaker" :title="t('messenger.callSpeakerHint')" data-testid="chat-call-speaker">
                {{ speakerOn ? t('messenger.callSpeakerOn') : t('messenger.callSpeaker') }}
              </button>
              <button class="call-act end" @click="hangup" data-testid="chat-call-hangup">{{ t('messenger.callHangup') }}</button>
            </template>
          </div>
          <!-- Панель воспроизведения mp3: в области сообщений, ПОД шапкой чата
               (раньше был fixed-оверлей сверху окна и перекрывал topbar) -->
          <div v-if="player" class="aud-topbar" data-testid="chat-aud-topbar">
            <div class="aud-track" @click="seekPlayer" data-testid="chat-aud-progress">
              <div class="aud-fill" :style="{ width: player.dur ? (player.cur / player.dur) * 100 + '%' : '0%' }" />
            </div>
            <div class="aud-topbar-row">
              <button class="aud-tb-btn" @click="togglePlayerPause" data-testid="chat-aud-topbar-toggle">
                <svg v-if="player.playing" viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
                  <rect x="6" y="5" width="4" height="14" rx="1" /><rect x="14" y="5" width="4" height="14" rx="1" />
                </svg>
                <svg v-else viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
                  <path d="M8 5.14v13.72c0 .8.87 1.3 1.56.9l11-6.86a1.05 1.05 0 0 0 0-1.8l-11-6.86c-.69-.4-1.56.1-1.56.9z" />
                </svg>
              </button>
              <span class="aud-tb-name">{{ player.name }}</span>
              <span class="aud-tb-time">{{ fmtDur(player.cur) }} / {{ fmtDur(player.dur) }}</span>
              <button class="aud-tb-close" @click="stopPlayer" data-testid="chat-aud-topbar-close">✕</button>
            </div>
          </div>
          <div ref="feed" class="feed" @click="onMsgBodyClick" @scroll.passive="onFeedScroll" data-testid="chat-feed">
            <div v-if="hasMore" class="feed-more" data-testid="chat-load-older">
              <button class="feed-more-btn" :disabled="loadingMore" @click.stop="loadOlder">
                {{ loadingMore ? t('messenger.loadingOlder') : t('messenger.loadOlder') }}
              </button>
            </div>
            <div v-if="pinnedMsg" class="pinned-banner" data-testid="chat-pinned-banner">
              <span class="pin-ic">📌</span>
              <div class="pin-body">
                <span class="pin-label">{{ t('messenger.pinnedLabel') }} · {{ displayName(pinnedMsg.sender) }}</span>
                <span class="pin-text">{{ previewText(pinnedMsg.text) }}</span>
              </div>
              <button v-if="isOwner" class="pin-unpin" @click="setPinned(null)" data-testid="chat-unpin-btn" :title="t('messenger.unpinTitle')">✕</button>
            </div>
            <template v-for="m in groupedMessages" :key="m.id">
              <div v-if="m.newDay" class="day-divider" :data-testid="`chat-day-${m.id}`"><span>{{ dayLabel(m.ts) }}</span></div>
              <div v-if="m.sys" class="sys-line" :data-testid="`chat-sys-${m.id}`">— {{ displayName(m.sender) }} {{ sysLabel(m.sys) }} —</div>
              <div
                v-else
                class="msg-row"
                :class="{ mine: m.mine, pinned: policy?.pinned === m.id, cont: !m.firstInGroup }"
                :data-testid="`chat-msg-${m.id}`"
                @contextmenu="onMsgContext($event, m.id)"
              >
                <span
                  v-if="m.lastInGroup"
                  class="av msg-av"
                  :class="{ 'av-link': isGroup && !m.mine }"
                  :title="isGroup && !m.mine ? t('messenger.openDmWith', { name: displayName(m.sender) }) : undefined"
                  :data-testid="isGroup && !m.mine ? `chat-avatar-dm-${m.id}` : undefined"
                  @click="isGroup && !m.mine && openDmWith(m.sender)"
                ><UserAvatar :address="m.mine ? myAddress : m.sender" :size="28" /></span>
                <span v-else class="av-spacer" aria-hidden="true"></span>
                <div class="bubble" :class="{ mine: m.mine, first: m.firstInGroup, last: m.lastInGroup }">
                  <span
                    v-if="isGroup && m.firstInGroup"
                    class="b-sender"
                    :class="{ 'b-sender-link': !m.mine }"
                    :title="m.mine ? undefined : t('messenger.ctxProfile')"
                    :data-testid="m.mine ? undefined : `chat-sender-profile-${m.id}`"
                    @click.stop="!m.mine && openUserProfile(m.sender)"
                  >{{ displayName(m.sender) }}</span>
                  <div
                    v-if="repMeta(m.text)"
                    class="b-quote"
                    @click.stop="scrollToMsg(repMeta(m.text)!.r)"
                    :data-testid="`chat-quote-${m.id}`"
                  >
                    <img v-if="quotedThumb(repMeta(m.text)!.r)" class="q-thumb" :src="quotedThumb(repMeta(m.text)!.r)" alt="" />
                    <svg v-else-if="quotedIsAudio(repMeta(m.text)!.r)" class="q-aud" viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/></svg>
                    <div class="q-col">
                      <span class="q-sender">{{ displayName(repMeta(m.text)!.s) }}</span>
                      <span class="q-text">{{ previewText(repMeta(m.text)!.p) }}</span>
                    </div>
                  </div>
                  <div v-if="imgMeta(m.text)" class="msg-img-wrap" :data-testid="`chat-msg-img-${m.id}`" @contextmenu.prevent.stop="openMediaCtx($event, m)">
                    <div class="imgcard" @click.stop="openImageTxt(m.text)" :data-testid="`chat-img-open-${m.id}`">
                      <img class="imgcard-thumb" :src="thumbSrcTxt(m.text)" alt="" />
                      <div class="imgcard-info">
                        <span class="imgcard-name">{{ t('messenger.photoPreview') }} {{ imgMeta(m.text)?.w }}×{{ imgMeta(m.text)?.ht }}</span>
                        <span class="imgcard-size">{{ fmtSize(imgMeta(m.text)?.s || 0) }}</span>
                        <span class="imgcard-open">{{ t('messenger.imgOpen') }}</span>
                      </div>
                    </div>
                    <div v-if="imgMeta(m.text)?.c" class="img-caption" :data-testid="`chat-img-caption-${m.id}`">{{ imgMeta(m.text)?.c }}</div>
                  </div>
                  <div v-else-if="audMeta(m.text)" class="msg-aud" :data-testid="`chat-msg-aud-${m.id}`" @click.stop @contextmenu.prevent.stop="openMediaCtx($event, m)">
                    <button class="aud-btn" :class="{ playing: audState(m.text) === 'playing' }" @click="toggleAudio(m.text)" :data-testid="`chat-aud-play-${m.id}`" :title="audState(m.text) === 'loading' ? t('messenger.audCancelTitle') : ''">
                      <span v-if="audState(m.text) === 'loading'" class="aud-load">
                        <svg class="aud-ring" :class="{ spin: !audPct(m.text) }" viewBox="0 0 36 36">
                          <circle class="ring-bg" cx="18" cy="18" r="15.5" />
                          <circle class="ring-fg" cx="18" cy="18" r="15.5" :stroke-dashoffset="97.4 - (97.4 * Math.max(4, audPct(m.text))) / 100" />
                        </svg>
                        <span class="aud-pct" :data-testid="`chat-aud-pct-${m.id}`">{{ audPct(m.text) }}</span>
                      </span>
                      <svg v-else-if="audState(m.text) === 'playing'" viewBox="0 0 24 24" width="22" height="22" fill="currentColor">
                        <rect x="6" y="5" width="4" height="14" rx="1" /><rect x="14" y="5" width="4" height="14" rx="1" />
                      </svg>
                      <svg v-else viewBox="0 0 24 24" width="22" height="22" fill="currentColor">
                        <path d="M8 5.14v13.72c0 .8.87 1.3 1.56.9l11-6.86a1.05 1.05 0 0 0 0-1.8l-11-6.86c-.69-.4-1.56.1-1.56.9z" />
                      </svg>
                    </button>
                    <div class="aud-info">
                      <span v-if="!audMeta(m.text)?.v || !waveBars(m.text).length" class="aud-name">{{ audName(m.text) }}</span>
                      <div
                        v-if="waveBars(m.text).length"
                        class="aud-wave"
                        @click.stop="onWaveClick(m.text, $event)"
                        :data-testid="`chat-aud-wave-${m.id}`"
                      >
                        <span
                          v-for="(v, wi) in waveBars(m.text)"
                          :key="wi"
                          class="wv"
                          :class="{ played: wi < wavePlayed(m.text) }"
                          :style="{ height: 18 + v * 5.4 + '%' }"
                        />
                      </div>
                      <span class="aud-sub">
                        <template v-if="audState(m.text) === 'loading' && audProgLabel(m.text)">↓ {{ audProgLabel(m.text) }}</template>
                        <template v-else-if="player && player.hash === audMeta(m.text)?.h">{{ fmtDur(player.cur) }} / {{ fmtDur(player.dur) }}</template>
                        <template v-else>{{ fmtDur(audMeta(m.text)?.d || 0) }}, {{ fmtSize(audMeta(m.text)?.s || 0) }}</template>
                      </span>
                    </div>
                  </div>
                  <div v-else-if="vidMeta(m.text)" class="msg-vid" :data-testid="`chat-msg-video-${m.id}`" @click.stop @contextmenu.prevent.stop="openMediaCtx($event, m)">
                    <video
                      v-if="vidSrc[vidMeta(m.text)!.h]"
                      class="vid-el"
                      :src="vidSrc[vidMeta(m.text)!.h]"
                      controls
                      preload="metadata"
                      playsinline
                      @error="onVideoError(m.text)"
                      :data-testid="`chat-video-player-${m.id}`"
                    />
                    <button
                      v-else
                      class="vid-poster"
                      :style="vidMeta(m.text)!.w && vidMeta(m.text)!.ht ? { aspectRatio: vidMeta(m.text)!.w + ' / ' + vidMeta(m.text)!.ht } : undefined"
                      @click="playVideo(m.text)"
                      :data-testid="`chat-video-play-${m.id}`"
                    >
                      <img
                        v-if="vidMeta(m.text)!.ph && vidPoster[vidMeta(m.text)!.ph!]"
                        class="vid-thumb"
                        :src="vidPoster[vidMeta(m.text)!.ph!]"
                        alt=""
                      />
                      <!-- постер ещё не докачан: фирменная плашка вместо размытой миниатюры -->
                      <span v-else class="vid-plate" :data-testid="`chat-video-plate-${m.id}`">
                        <svg class="vid-plate-ico" viewBox="0 0 24 24" width="30" height="30" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
                          <rect x="2" y="4" width="20" height="16" rx="2.5" /><path d="M2 9h20M2 15h20M7 4v16M17 4v16" />
                        </svg>
                        <span class="vid-plate-brand">NETFORY <b>VIDEO</b></span>
                        <span class="vid-plate-hint">{{ vidMeta(m.text)!.ph ? t('messenger.vidPosterLoading') : t('messenger.vidNoPoster') }}</span>
                      </span>
                      <span v-if="vidMeta(m.text)!.ph && vidPoster[vidMeta(m.text)!.ph!]" class="vid-shade"></span>
                      <span v-if="vidFetching.has(vidMeta(m.text)!.h)" class="vid-load" :data-testid="`chat-video-loading-${m.id}`">
                        <svg class="aud-ring spin" viewBox="0 0 36 36">
                          <circle class="ring-bg" cx="18" cy="18" r="15.5" />
                          <circle class="ring-fg" cx="18" cy="18" r="15.5" stroke-dashoffset="60" />
                        </svg>
                        <span class="vid-prog">{{ vidProg[vidMeta(m.text)!.h]?.label || '…' }}</span>
                        <span class="vid-prog-hint" :data-testid="`chat-video-source-hint-${m.id}`">{{ vidProg[vidMeta(m.text)!.h]?.hint || t('messenger.vidTapToCancel') }}</span>
                      </span>
                      <span v-else class="vid-play-ico">
                        <svg viewBox="0 0 24 24" width="26" height="26" fill="currentColor"><path d="M8 5v14l11-7z" /></svg>
                      </span>
                      <span class="vid-caption">
                        <span class="vid-name">{{ vidMeta(m.text)?.f }}</span>
                        <span class="vid-sub">{{ vidMeta(m.text)?.d ? fmtDur(vidMeta(m.text)!.d!) + ' · ' : '' }}{{ fmtSize(vidMeta(m.text)?.s || 0) }}</span>
                      </span>
                    </button>
                  </div>
                  <div v-else-if="fileMeta(m.text)" class="msg-aud msg-file" :data-testid="`chat-msg-file-${m.id}`" @click.stop @contextmenu.prevent.stop="openMediaCtx($event, m)">
                    <button class="aud-btn" @click="saveFileMsg(m.text)" :data-testid="`chat-file-save-${m.id}`" :title="t('messenger.fileSaveTitle')">
                      <span v-if="fileSaving.has(fileMeta(m.text)!.h)" class="aud-load">
                        <svg class="aud-ring spin" viewBox="0 0 36 36">
                          <circle class="ring-bg" cx="18" cy="18" r="15.5" />
                          <circle class="ring-fg" cx="18" cy="18" r="15.5" stroke-dashoffset="60" />
                        </svg>
                      </span>
                      <svg v-else viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                        <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" /><polyline points="14 2 14 8 20 8" />
                      </svg>
                    </button>
                    <div class="aud-info file-clickable" @click="onFileCardClick(m.text)" :data-testid="`chat-file-open-${m.id}`">
                      <span class="aud-name">{{ fileMeta(m.text)?.f }}</span>
                      <span class="aud-sub">{{ fileExtBadge(m.text) }} · {{ fmtSize(fileMeta(m.text)?.s || 0) }}<span v-if="fileExtBadge(m.text) === 'MD'" class="file-open-hint"> · {{ t('messenger.mdOpen') }}</span></span>
                    </div>
                  </div>
                  <span v-else class="b-text"><template v-for="(seg, si) in msgSegments(msgPlain(m.text))" :key="si"><a
                      v-if="seg.link"
                      class="msg-link"
                      :class="{ 'msg-magnet': seg.magnet, 'msg-contact': seg.contact, 'msg-group': seg.group }"
                      :title="seg.link"
                      :data-testid="`chat-msg-link-${m.id}-${si}`"
                      @click.prevent.stop="openMsgLink(seg.link)"
                    ><span v-if="seg.magnet" class="magnet-badge">MAGNET</span><span v-else-if="seg.contact" class="contact-badge">CONTACT</span><span v-else-if="seg.group" class="group-badge">GROUP</span>{{ seg.text }}</a><template v-else>{{ seg.text }}</template></template></span>
                  <span class="b-time">
                    <button v-if="isMissedCall(m)" class="recall-btn" @click="startCall" :disabled="voiceBusy" :data-testid="`chat-recall-${m.id}`">{{ t('messenger.callBack') }}</button>
                    <span v-if="m.edited" class="b-edited" :data-testid="`chat-edited-${m.id}`">{{ t('messenger.editedMark') }}</span>
                    {{ fmtTime(m.ts) }}
                    <span v-if="isGroup" class="b-stat" :data-testid="`chat-views-${m.id}`" :title="t('messenger.viewsTitle')">👁 {{ views[m.id] || 1 }}</span>
                    <span v-if="isGroup && (reposts[m.id] || 0) > 0" class="b-stat" :data-testid="`chat-reposts-${m.id}`" :title="t('messenger.repostsTitle')">↻ {{ reposts[m.id] }}</span>
                    <span v-if="m.mine" class="b-tick" :class="{ delivered: peerOnline || (acks[m.id] || 0) > 0 }" :title="t('messenger.deliveredCount', { n: acks[m.id] || 0 })" :data-testid="`chat-ack-${m.id}`">{{ (acks[m.id] || 0) > 0 ? '✓✓ ' + acks[m.id] : (peerOnline ? '✓✓' : '✓') }}</span>
                  </span>
                  <div v-if="(reactions[m.id] || []).length" class="b-reacts" :data-testid="`chat-reacts-${m.id}`">
                    <button
                      v-for="r in reactions[m.id]"
                      :key="r.e"
                      class="react-chip"
                      :class="{ mine: r.mine }"
                      @click.stop="reactTo(m.id, r.e)"
                      :data-testid="`chat-react-${m.id}-${r.e}`"
                    >{{ r.e }}<span class="react-n">{{ r.n }}</span></button>
                  </div>
                </div>
              </div>
            </template>
            <div v-if="channelMode && hiddenCount > 0" class="discuss-toggle">
              <button class="discuss-btn" @click="showDiscussions = !showDiscussions" data-testid="chat-toggle-discussions">
                {{ showDiscussions ? t('messenger.hideDiscussions') : t('messenger.showDiscussions', { n: hiddenCount }) }}
              </button>
            </div>
            <p v-if="!visibleMessages.length" class="feed-empty">
              {{ channelMode ? t('messenger.channelEmpty') : t('messenger.feedEmpty') }}
            </p>
          </div>

          <div class="composer" :class="{ locked: !canWrite }">
            <div v-if="channelMode && !isOwner" class="channel-note" data-testid="chat-channel-note">
              {{ t('messenger.channelNote') }}
            </div>
            <div v-if="activeCost > 0" class="paid-note" data-testid="chat-paid-note">
              {{ t('messenger.paidNote', { amount: activeCost }) }}
            </div>
            <div v-if="pendingImg" class="img-pending" data-testid="chat-img-pending">
              <img class="img-pending-thumb" :src="'data:image/jpeg;base64,' + pendingImg.thumb" alt="" />
              <div class="img-pending-body">
                <input
                  v-model="imgCaption"
                  class="img-caption-input"
                  maxlength="1000"
                  :placeholder="t('messenger.captionPlaceholder')"
                  @keydown.enter.exact.prevent="sendImage"
                  data-testid="chat-img-caption-input"
                />
                <div class="img-pending-info">
                  <span class="img-pending-dims">{{ pendingImg.w }}×{{ pendingImg.h }} · JPEG</span>
                  <div class="img-pending-actions">
                    <button class="img-send-btn" :disabled="sendingImg" @click="sendImage" data-testid="chat-img-send">
                      {{ sendingImg ? t('messenger.imgSending') : t('messenger.imgSend') }}
                    </button>
                    <button class="img-cancel-btn" :disabled="sendingImg" @click="pendingImg = null; imgCaption = ''" data-testid="chat-img-cancel">✕</button>
                  </div>
                </div>
              </div>
            </div>
            <div v-else-if="pendingAud" class="img-pending" data-testid="chat-aud-pending">
              <span class="aud-pending-icon">
                <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M9 18V5l12-2v13" /><circle cx="6" cy="18" r="3" /><circle cx="18" cy="16" r="3" />
                </svg>
              </span>
              <div class="img-pending-info">
                <span class="img-pending-dims">{{ pendingAud.voice ? t('messenger.voiceMsg') : pendingAud.name }} · {{ fmtDur(pendingAud.durMs) }} · {{ fmtSize(pendingAud.size) }}</span>
                <div class="img-pending-actions">
                  <button class="img-send-btn" :disabled="sendingAud" @click="sendAudio" data-testid="chat-aud-send">
                    {{ sendingAud ? t('messenger.imgSending') : t('messenger.imgSend') }}
                  </button>
                  <button class="img-cancel-btn" :disabled="sendingAud" @click="pendingAud = null" data-testid="chat-aud-cancel">✕</button>
                </div>
              </div>
            </div>
            <div v-else-if="pendingVid" class="img-pending" data-testid="chat-vid-pending">
              <img v-if="pendingVid.thumb" class="img-pending-thumb" :src="'data:image/jpeg;base64,' + pendingVid.thumb" alt="" />
              <span v-else class="aud-pending-icon">
                <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <rect x="2" y="5" width="14" height="14" rx="2" /><path d="m22 7-6 4 6 4z" />
                </svg>
              </span>
              <div class="img-pending-info">
                <span class="img-pending-dims">{{ pendingVid.name }} · {{ fmtSize(pendingVid.size) }}<template v-if="pendingVid.durMs"> · {{ fmtDur(pendingVid.durMs) }}</template></span>
                <div class="img-pending-actions">
                  <button class="img-send-btn" :disabled="sendingVid" @click="sendVideo" data-testid="chat-vid-send">
                    {{ sendingVid ? vidPct + '%' : t('messenger.imgSend') }}
                  </button>
                  <button class="img-cancel-btn" :disabled="sendingVid" @click="pendingVid = null" data-testid="chat-vid-cancel">✕</button>
                </div>
              </div>
            </div>
            <div v-else-if="pendingFile" class="img-pending" data-testid="chat-file-pending">
              <span class="aud-pending-icon">
                <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" /><polyline points="14 2 14 8 20 8" />
                </svg>
              </span>
              <div class="img-pending-info">
                <span class="img-pending-dims">{{ pendingFile.name }} · {{ fmtSize(pendingFile.size) }}</span>
                <div class="img-pending-actions">
                  <button class="img-send-btn" :disabled="sendingFile" @click="sendFile" data-testid="chat-file-send">
                    {{ sendingFile ? t('messenger.imgSending') : t('messenger.imgSend') }}
                  </button>
                  <button class="img-cancel-btn" :disabled="sendingFile" @click="pendingFile = null" data-testid="chat-file-cancel">✕</button>
                </div>
              </div>
            </div>
            <div v-else-if="recording" class="img-pending rec-strip" data-testid="chat-rec-strip">
              <span class="rec-dot" />
              <span class="rec-time">{{ fmtDur(recElapsed) }}</span>
              <span class="rec-hint">{{ t('messenger.recHint') }}</span>
              <button class="img-cancel-btn" @click="stopRecording(true)" data-testid="chat-rec-cancel">✕</button>
            </div>
            <div v-if="editing" class="reply-strip edit-strip" data-testid="chat-edit-strip">
              <svg class="reply-icn" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>
              <div class="reply-body">
                <span class="reply-name">{{ t('messenger.editingTitle') }}</span>
                <span class="reply-text">{{ previewText(msgPlain(messages.find((x) => x.id === editing!.id)?.text || '')) }}</span>
              </div>
              <button class="img-cancel-btn" @click="cancelEdit" data-testid="chat-edit-cancel">✕</button>
            </div>
            <div v-if="replyTo" class="reply-strip" data-testid="chat-reply-strip">
              <svg class="reply-icn" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 17 4 12 9 7"/><path d="M20 18v-2a4 4 0 0 0-4-4H4"/></svg>
              <img v-if="quotedThumb(replyTo.id)" class="q-thumb" :src="quotedThumb(replyTo.id)" alt="" />
              <svg v-else-if="quotedIsAudio(replyTo.id)" class="q-aud" viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/></svg>
              <div class="reply-body">
                <span class="reply-name">{{ displayName(replyTo.sender) }}</span>
                <span class="reply-text">{{ replyTo.preview }}</span>
              </div>
              <button class="img-cancel-btn" @click="replyTo = null" data-testid="chat-reply-cancel">✕</button>
            </div>
            <transition name="fade">
              <div v-if="mentionList.length" class="mention-pop" data-testid="chat-mention-pop">
                <button
                  v-for="(n, i) in mentionList"
                  :key="n"
                  class="mention-item"
                  :class="{ on: i === mentionIdx }"
                  @mousedown.prevent="applyMention(n)"
                  @mouseenter="mentionIdx = i"
                  :data-testid="`chat-mention-${n}`"
                >
                  <span class="mention-at">@</span>{{ n }}
                </button>
              </div>
            </transition>
            <div class="composer-row">
              <button
                class="emoji-btn"
                type="button"
                @click="toggleEmoji"
                :class="{ on: showEmoji }"
                title="Эмодзи"
                data-testid="chat-emoji-btn"
              >
                <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <circle cx="12" cy="12" r="10" />
                  <path d="M8 14s1.5 2 4 2 4-2 4-2" />
                  <line x1="9" y1="9" x2="9.01" y2="9" />
                  <line x1="15" y1="9" x2="15.01" y2="9" />
                </svg>
              </button>
              <button
                class="emoji-btn"
                type="button"
                @click="pickImage"
                :title="t('messenger.attachImage')"
                data-testid="chat-attach-btn"
              >
                <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
                </svg>
              </button>
              <input ref="fileInput" type="file" accept="image/*,audio/mpeg,.mp3,audio/mp4,audio/x-m4a,.m4a,video/mp4,.mp4,.m4v,application/pdf,.pdf,text/markdown,.md" style="display: none" @change="onFilePicked" data-testid="chat-file-input" />
              <textarea
                ref="inputRef"
                v-model="draft"
                rows="1"
                :readonly="!canWrite"
                :placeholder="!canWrite ? t('messenger.needNamePlaceholder') : activeCost > 0 ? t('messenger.paidPlaceholder') : isMobile ? t('messenger.msgPlaceholderShort') : t(ui.mode === 'user' ? 'messenger.msgPlaceholderUser' : 'messenger.msgPlaceholder')"
                @click="!canWrite && openOnboarding()"
                @keydown.enter.exact.prevent="onComposerEnter"
                @keydown.tab="mentionList.length && (($event.preventDefault(), applyMention(mentionList[mentionIdx])))"
                @keydown.up="onMentionNav($event, -1)"
                @keydown.down="onMentionNav($event, 1)"
                @paste="onPaste"
                data-testid="chat-input"
              />
              <button class="send" @click="send" data-testid="chat-send">
                <svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor"><path d="M3 20.5v-6l8-2.5-8-2.5v-6l19 8.5-19 8.5z"/></svg>
              </button>
              <button
                class="mic"
                :class="{ rec: recording }"
                @click="toggleRecording"
                :title="t('messenger.micTitle')"
                data-testid="chat-mic-btn"
              >
                <svg v-if="!recording" viewBox="0 0 24 24" width="19" height="19" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
                  <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
                  <line x1="12" y1="19" x2="12" y2="23" />
                  <line x1="8" y1="23" x2="16" y2="23" />
                </svg>
                <svg v-else viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
                  <rect x="6" y="6" width="12" height="12" rx="2" />
                </svg>
              </button>
            </div>
            <transition name="emoji-pop">
              <div v-if="showEmoji" class="emoji-panel" data-testid="chat-emoji-panel">
                <EmojiPicker
                  :native="true"
                  :hide-search="false"
                  :disable-skin-tones="false"
                  :disabled-groups="['animals_nature', 'food_drink', 'travel_places', 'objects', 'symbols', 'flags']"
                  theme="dark"
                  @select="insertEmoji"
                />
              </div>
            </transition>
            <template v-if="mediaCtx">
              <div class="ctx-backdrop" @click="mediaCtx = null" @contextmenu.prevent="mediaCtx = null" />
              <div class="ctx-menu" :style="{ left: mediaCtx.x + 'px', top: mediaCtx.y + 'px' }" data-testid="chat-media-ctx">
                <button class="ctx-item media-save-item" @click="startReply(mediaCtx!.id); mediaCtx = null" data-testid="chat-media-reply-btn">
                  <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 17 4 12 9 7"/><path d="M20 18v-2a4 4 0 0 0-4-4H4"/></svg>
                  {{ t('messenger.ctxReply').replace('↩ ', '') }}
                </button>
                <button class="ctx-item media-save-item" @click="saveCtxFile" data-testid="chat-media-save-btn">
                  <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>
                  Save File
                </button>
              </div>
            </template>
          </div>
        </template>

        <div v-else class="splash" data-testid="chat-splash">
          <span class="sp-logo">🔒</span>
          <span class="sp-title">{{ t('messenger.splashTitle') }}</span>
          <span class="sp-sub">{{ t('messenger.splashSub') }}</span>
        </div>
      </section>
    </div>

    <p v-if="error" class="cw-error" data-testid="chat-error">
      <span class="cw-error-text">{{ error }}</span>
      <button class="cw-error-x" @click="error = ''" :title="t('messenger.closeT')" data-testid="chat-error-close">✕</button>
    </p>

    <!-- transient toast (channel-mode blocks, etc.) -->
    <transition name="toast-pop">
      <div v-if="toast" class="cw-toast" data-testid="chat-toast">{{ toast }}</div>
    </transition>

    <!-- Полноэкранный просмотр изображения (как в Telegram): клик — закрыть -->
    <MdViewerModal v-if="mdViewer" :name="mdViewer.name" :text="mdViewer.text" @close="mdViewer = null" />
    <div v-if="dropActive" class="drop-overlay" data-testid="chat-drop-overlay">
      <div class="drop-card">
        <svg viewBox="0 0 24 24" width="42" height="42" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" /><polyline points="7 10 12 15 17 10" /><line x1="12" y1="15" x2="12" y2="3" />
        </svg>
        <span>{{ t('messenger.dropHere') }}</span>
      </div>
    </div>
    <div v-if="lightbox.open" class="img-lightbox" @click="lightbox.open = false" data-testid="chat-img-lightbox">
      <img v-if="lightbox.src" :src="lightbox.src" class="lb-img" alt="" />
      <div v-else-if="lightbox.error" class="lb-state" data-testid="chat-img-lightbox-error">{{ t('messenger.imgLoadFailed') }}</div>
      <div v-else class="lb-state"><span class="lb-spinner" />{{ t('messenger.imgLoading') }}</div>
    </div>

    <!-- right-click context menu (repost: everyone · pin/unpin: owner) -->
    <template v-if="ctxMenu.open">
      <div class="ctx-backdrop" @click="closeCtx" @contextmenu.prevent="closeCtx" data-testid="chat-ctx-backdrop" />
      <div class="ctx-menu" ref="ctxEl" :style="{ left: ctxMenu.x + 'px', top: ctxMenu.y + 'px' }" data-testid="chat-ctx-menu">
        <div class="ctx-reacts" data-testid="chat-ctx-reacts">
          <button
            v-for="e in quickReactions"
            :key="e"
            class="ctx-react"
            @click="reactTo(ctxMenu.id, e)"
            :data-testid="`chat-ctx-react-${e}`"
          >{{ e }}</button>
          <button
            class="ctx-react ctx-react-more"
            :class="{ on: ctxMoreReacts }"
            @click.stop="ctxMoreReacts = !ctxMoreReacts"
            data-testid="chat-ctx-react-more"
            :title="t('messenger.moreReactions')"
          >▾</button>
        </div>
        <div v-if="ctxMoreReacts" class="ctx-emoji" data-testid="chat-ctx-emoji-picker" @click.stop>
          <EmojiPicker
            :native="true"
            :hide-search="false"
            :disable-skin-tones="true"
            :disabled-groups="['flags']"
            theme="dark"
            @select="pickReaction"
          />
        </div>
        <button
          class="ctx-item"
          @click="startReply(ctxMenu.id)"
          data-testid="chat-ctx-reply"
        >{{ t('messenger.ctxReply') }}</button>
        <button
          class="ctx-item"
          @click="copyMsg(ctxMenu.id)"
          data-testid="chat-ctx-copy"
        >{{ t('messenger.ctxCopy') }}</button>
        <button
          v-if="ctxSender(ctxMenu.id)"
          class="ctx-item"
          @click="ctxOpenProfile(ctxMenu.id)"
          data-testid="chat-ctx-profile"
        >{{ t('messenger.ctxProfile') }}</button>
        <button
          v-if="canEdit(ctxMenu.id)"
          class="ctx-item"
          @click="startEdit(ctxMenu.id)"
          data-testid="chat-ctx-edit"
        >{{ t('messenger.ctxEdit') }}</button>
        <button
          class="ctx-item"
          @click="repostMsg(ctxMenu.id)"
          data-testid="chat-ctx-repost"
        >{{ t('messenger.ctxRepost') }}</button>
        <button
          class="ctx-item ctx-danger"
          @click="deleteMsg(ctxMenu.id)"
          data-testid="chat-ctx-delete"
        >{{ t('messenger.ctxDelete') }}</button>
        <button
          v-if="isOwner && policy?.pinned === ctxMenu.id"
          class="ctx-item"
          @click="setPinned(null)"
          data-testid="chat-ctx-unpin"
        >{{ t('messenger.ctxUnpin') }}</button>
        <button
          v-else-if="isOwner"
          class="ctx-item"
          @click="setPinned(ctxMenu.id)"
          data-testid="chat-ctx-pin"
        >{{ t('messenger.ctxPin') }}</button>
      </div>
    </template>

    <!-- annihilation glitch modal -->
    <div v-if="wipeAsk" class="wipe-back" @click.self="wipeAsk = false" data-testid="chat-wipe-modal">
      <div class="wipe-card">
        <h3 class="wipe-title">{{ t('messenger.wipeTitle') }}</h3>
        <p class="wipe-body">
          {{ t(ui.mode === 'user' ? 'messenger.wipeBodyUser' : 'messenger.wipeBody') }}
        </p>
        <div class="wipe-actions">
          <button class="wb cancel" @click="wipeAsk = false" data-testid="chat-wipe-cancel">{{ t('messenger.cancel') }}</button>
          <button class="wb burn" @click="confirmWipe" data-testid="chat-wipe-confirm">{{ t(ui.mode === 'user' ? 'messenger.annihilateUser' : 'messenger.annihilate') }}</button>
        </div>
      </div>
    </div>

    <!-- leave / delete room modal -->
    <div v-if="showCircuit" class="wipe-back" @click.self="showCircuit = false" data-testid="chat-circuit-modal">
      <div class="wipe-card circ-card">
        <h3 class="wipe-title">// {{ t('mycelium.mapChatTitle') }}_</h3>
        <CircuitMap v-if="peerCircuit" :circuit="peerCircuit" :peer-label="active ? displayName(active.peerAddress) : ''" testid="chat-circuit-map" @rebuilt="peerCircuit = $event" @error="circuitErr = $event" />
        <p v-if="circuitErr" class="circ-none circ-err" data-testid="chat-circuit-error">{{ circuitErr }}</p>
        <p v-else class="circ-none" data-testid="chat-circuit-none">{{ t('mycelium.mapChatNone') }}</p>
        <p class="circ-note">{{ t('mycelium.exitPeerHint') }}</p>
        <button class="circ-close" @click="showCircuit = false" data-testid="chat-circuit-close">{{ t('mycelium.close') }}</button>
      </div>
    </div>
    <div v-if="leaveAsk" class="wipe-back" @click.self="leaveAsk = false" data-testid="chat-leave-modal">
      <div class="wipe-card">
        <h3 class="wipe-title">// {{ isGroup ? t('messenger.leaveTitleGroup') : t('messenger.leaveTitleChat') }}_</h3>
        <p class="wipe-body">
          {{ isGroup ? t('messenger.leaveBodyGroup') : t('messenger.leaveBodyChat') }}
        </p>
        <div class="wipe-actions">
          <button class="wb cancel" @click="leaveAsk = false" data-testid="chat-leave-cancel">{{ t('messenger.cancel') }}</button>
          <button class="wb burn" @click="confirmLeave" data-testid="chat-leave-confirm">{{ isGroup ? t('messenger.leaveBtnGroup') : t('messenger.leaveBtnChat') }}</button>
        </div>
      </div>
    </div>

    <!-- group admin panel (owner only) -->
    <div v-if="showAdmin" class="set-back" @click.self="showAdmin = false" data-testid="chat-admin-modal">
      <div class="set-card">
        <header class="set-head">
          <h3 class="set-title">{{ t('messenger.adminTitle') }}</h3>
          <button class="set-x" @click="showAdmin = false" data-testid="chat-admin-close">✕</button>
        </header>

        <div class="set-row col" data-testid="admin-monetization">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.monetization') }}</span>
            <span class="set-sub">{{ t('messenger.monetizationSub') }}</span>
          </div>
          <label class="set-row inner">
            <span class="set-label">{{ t('messenger.enablePaidEntry') }}</span>
            <button class="sw" :class="{ on: policyForm.paidEntry }" @click="policyForm.paidEntry = !policyForm.paidEntry" data-testid="admin-paid-toggle"><span class="sw-knob" /></button>
          </label>
          <div class="cost-row">
            <input type="number" min="0" step="0.001" class="cost-input" v-model.number="policyForm.entryFeeSth" data-testid="admin-entry-fee" />
            <span class="cost-unit">STH</span>
          </div>
        </div>

        <label class="set-row" data-testid="admin-access">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.inviteOnly') }}</span>
            <span class="set-sub">{{ t('messenger.inviteOnlySub') }}</span>
          </div>
          <button class="sw" :class="{ on: policyForm.inviteOnly }" @click="policyForm.inviteOnly = !policyForm.inviteOnly" data-testid="admin-inviteonly-toggle"><span class="sw-knob" /></button>
        </label>

        <label class="set-row" data-testid="admin-channel">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.channelMode') }}</span>
            <span class="set-sub">{{ t('messenger.channelModeSub') }}</span>
          </div>
          <button class="sw" :class="{ on: policyForm.channelMode }" @click="policyForm.channelMode = !policyForm.channelMode" data-testid="admin-channel-toggle"><span class="sw-knob" /></button>
        </label>

        <div class="set-row col" data-testid="admin-logo">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.groupLogo') }}</span>
            <span class="set-sub">{{ t('messenger.groupLogoSub') }}</span>
          </div>
          <div class="logo-row">
            <img v-if="logoPreview || (active && groupLogos[active.id])" class="logo-prev" :src="logoPreview || groupLogos[active?.id || '']" alt="" data-testid="admin-logo-preview" />
            <span v-else class="logo-prev logo-empty">128</span>
            <input ref="logoInputRef" type="file" accept="image/png,image/jpeg" class="hidden-input" @change="onLogoPick" data-testid="admin-logo-input" />
            <button class="pill-btn" :disabled="logoUploading" @click="logoInputRef?.click()" data-testid="admin-logo-upload">
              {{ logoUploading ? '…' : t('messenger.logoUpload') }}
            </button>
            <button v-if="policyForm.logoHash" class="pill-btn danger" @click="removeLogo" data-testid="admin-logo-remove">✕</button>
          </div>
        </div>

        <div class="set-row col" data-testid="admin-moderation">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.moderation') }}</span>
            <span class="set-sub">{{ t('messenger.moderationSub') }}</span>
          </div>
          <div class="ban-dur" data-testid="ban-duration">
            <span class="ban-dur-l">{{ t('messenger.banDurationLabel') }}</span>
            <div class="ban-dur-chips">
              <button
                v-for="d in BAN_DURATIONS"
                :key="d.key"
                class="ban-chip"
                :class="{ active: banDuration === d.secs }"
                @click="banDuration = d.secs"
                :data-testid="`ban-dur-${d.key}`"
              >{{ d.label }}</button>
            </div>
          </div>
          <div class="mod-list">
            <div v-for="a in moderationMembers" :key="a" class="mod-item" :class="{ 'mod-banned': policyBanned.includes(a) }" :data-testid="`admin-member-${a}`">
              <span class="mod-av" :class="{ flat: policyBanned.includes(a) }">
                <UserAvatar :address="a" :size="26" />
              </span>
              <div class="mod-meta">
                <span class="mod-name">{{ displayName(a) }}</span>
                <span v-if="policyBanned.includes(a)" class="mod-ban-tag" :data-testid="`admin-banned-${a}`">
                  {{ t('messenger.bannedTag') }}{{ memberBanUntil(a) ? ' · ' + t('messenger.untilWord') + ' ' + fmtBanUntil(memberBanUntil(a)) : '' }}
                </span>
              </div>
              <button v-if="policyBanned.includes(a)" class="mod-unban" :disabled="banningAddr === a" @click="unban(a)" :data-testid="`admin-unban-${a}`">{{ banningAddr === a ? '…' : t('messenger.restore') }}</button>
              <button v-else class="mod-kick" :disabled="banningAddr === a" @click="kickMember(a)" :data-testid="`admin-kick-${a}`">{{ banningAddr === a ? t('messenger.banning') : t('messenger.banAction') }}</button>
            </div>
            <p v-if="!moderationMembers.length" class="mod-empty">{{ t('messenger.modEmpty') }}</p>
          </div>
        </div>

        <p v-if="adminError" class="admin-err" data-testid="admin-error">{{ adminError }}</p>
        <div class="set-row admin-actions">
          <button class="cost-pub wide" :disabled="savingPolicy" @click="savePolicy()" data-testid="admin-save">
            {{ savingPolicy ? t('messenger.savingPolicy') : t('messenger.savePolicy') }}
          </button>
        </div>
      </div>
    </div>

    <!-- group creation modal -->
    <div v-if="showGroupModal" class="set-back" @click.self="closeGroupModal" data-testid="chat-group-modal">
      <div class="set-card">
        <header class="set-head">
          <h3 class="set-title">{{ t('messenger.createGroupTitle') }}</h3>
          <button class="set-x" @click="closeGroupModal" data-testid="chat-group-close">✕</button>
        </header>
        <div class="set-row col">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.groupNameLabel') }}</span>
            <span class="set-sub">{{ t('messenger.groupNameSub') }}</span>
          </div>
          <div class="cost-row" v-if="!groupLink">
            <input
              v-model="groupName"
              class="cost-input wide"
              :placeholder="t('messenger.groupNamePlaceholder')"
              @keydown.enter="createGroupAction"
              data-testid="chat-group-name"
            />
            <button class="cost-pub" :disabled="creatingGroup || !groupName.trim()" @click="createGroupAction" data-testid="chat-group-create">
              {{ creatingGroup ? t('messenger.creating') : t('messenger.create') }}
            </button>
          </div>
        </div>
        <div v-if="groupLink" class="set-row col" data-testid="chat-group-link-row">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.inviteLinkLabel') }}</span>
            <span class="set-sub">{{ t('messenger.inviteLinkSub') }}</span>
          </div>
          <div class="cost-row">
            <code class="invite-link" data-testid="chat-group-link">{{ groupLink }}</code>
            <button class="cost-pub" @click="copyText(groupLink)" data-testid="chat-group-copy">{{ copied ? '✓' : t('messenger.copy') }}</button>
          </div>
          <button class="wb cancel done-btn" @click="closeGroupModal" data-testid="chat-group-done">{{ t('messenger.done') }}</button>
        </div>
      </div>
    </div>

    <!-- share contact modal: моя контакт-ссылка (ключи без блокчейна) -->
    <div v-if="showShareContact" class="set-back" @click.self="showShareContact = false" data-testid="chat-share-contact-modal">
      <div class="set-card">
        <header class="set-head">
          <h3 class="set-title">{{ t('messenger.shareContactTitle') }}</h3>
          <button class="set-x" @click="showShareContact = false" data-testid="chat-share-contact-close">✕</button>
        </header>
        <div class="set-row col">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.myContactLink') }}</span>
            <span class="set-sub">{{ t('messenger.shareContactSub') }}</span>
          </div>
          <div class="cost-row" v-if="myContactLink">
            <code class="invite-link" data-testid="chat-my-contact-link">{{ myContactLink }}</code>
            <button class="cost-pub" @click="copyText(myContactLink)" data-testid="chat-share-contact-copy">{{ copied ? '✓' : t('messenger.copy') }}</button>
          </div>
          <span v-else class="set-sub">{{ t('messenger.contactLinkLoading') }}</span>
        </div>
      </div>
    </div>

    <!-- add contact modal: адресная книга -->
    <div v-if="showAddContact" class="set-back" @click.self="showAddContact = false" data-testid="chat-add-contact-modal">
      <div class="set-card">
        <header class="set-head">
          <h3 class="set-title">{{ t('messenger.addContactTitle') }}</h3>
          <button class="set-x" @click="showAddContact = false" data-testid="chat-add-contact-close">✕</button>
        </header>
        <div class="set-row col">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.addContactLabel') }}</span>
            <span class="set-sub">{{ t('messenger.addContactSub') }}</span>
          </div>
          <div class="cost-row">
            <input
              v-model="addContactLink"
              class="cost-input wide"
              :placeholder="'sn://contact?a=…'"
              @keydown.enter="doAddContact"
              data-testid="chat-add-contact-input"
            />
            <button class="cost-pub" :disabled="addingContact || !addContactLink.trim()" @click="doAddContact" data-testid="chat-add-contact-submit">
              {{ addingContact ? '…' : t('messenger.addBtn') }}
            </button>
          </div>
          <span v-if="addContactErr" class="set-sub err" data-testid="chat-add-contact-error">{{ addContactErr }}</span>
        </div>
        <div v-if="contacts.length" class="set-row col" data-testid="chat-contacts-list">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.contactsSaved') }} ({{ contacts.length }})</span>
          </div>
          <div class="abook">
            <div v-for="c in contacts" :key="c.address" class="abook-row" :data-testid="`chat-contact-row-${c.address}`">
              <button class="abook-open" @click="openContactChat(c)" :title="t('messenger.openChat')" :data-testid="`chat-contact-open-${c.address}`">
                <span class="abook-name">{{ c.username ? '@' + c.username : c.address.slice(0, 12) + '…' }}</span>
                <span class="abook-addr">{{ c.address }}</span>
              </button>
              <button class="abook-del" @click="removeContact(c.address)" :title="t('messenger.deleteContact')" :data-testid="`chat-contact-del-${c.address}`">✕</button>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- settings overlay -->
    <div v-if="showSettings" class="set-back" @click.self="showSettings = false" data-testid="chat-settings-modal">
      <div class="set-card">
        <header class="set-head">
          <h3 class="set-title">{{ t('messenger.settingsTitle') }}</h3>
          <button class="set-x" @click="showSettings = false" data-testid="chat-settings-close">✕</button>
        </header>

        <div class="set-row col" data-testid="chat-set-key">
          <div class="set-info">
            <span class="set-label">
              {{ t('messenger.keyLabel') }}
              <span class="key-state" :class="{ on: keyPublished }">{{ keyPublished ? t('messenger.keyOnline') : t('messenger.keyOffline') }}</span>
            </span>
            <span class="set-sub">{{ t('messenger.keySub') }}</span>
          </div>
          <div class="cost-row" v-if="keyPublished && !keyStale">
            <button
              class="key-ok"
              @click="openShareFromKey"
              :title="t('messenger.keyShareHint')"
              data-testid="chat-key-published"
            >{{ t('messenger.keyPublishedRow') }}<span v-if="keyNodeShort" class="key-node">· {{ keyNodeShort }}</span></button>
          </div>
          <div class="cost-row" v-else>
            <span class="set-hint stale" v-if="keyStale" data-testid="chat-key-stale">{{ t('messenger.keyStaleHint') }}</span>
            <span class="set-hint" v-if="publishFee">{{ t('messenger.netFee', { fee: publishFee }) }}</span>
            <button
              class="cost-pub"
              :disabled="publishing"
              @click="doPublishKey"
              data-testid="chat-republish-key"
            >{{ publishing ? t('messenger.publishing') : (keyStale ? t('messenger.keyUpdate') : t('messenger.publish')) }}</button>
          </div>
        </div>

        <label class="set-row" data-testid="chat-set-notify">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.notifications') }}</span>
            <span class="set-sub">{{ t('messenger.notificationsSub') }}</span>
          </div>
          <button
            class="sw"
            :class="{ on: settings.notifications }"
            @click="settings.notifications = !settings.notifications; saveSettings()"
            data-testid="chat-set-notify-toggle"
          ><span class="sw-knob" /></button>
        </label>

        <label class="set-row" data-testid="chat-set-autosub">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.autoSub') }}</span>
            <span class="set-sub">{{ t('messenger.autoSubSub') }}</span>
          </div>
          <button
            class="sw"
            :class="{ on: settings.autoSubscribe }"
            @click="settings.autoSubscribe = !settings.autoSubscribe; saveSettings()"
            data-testid="chat-set-autosub-toggle"
          ><span class="sw-knob" /></button>
        </label>

        <label class="set-row" data-testid="chat-set-trusted">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.trusted') }}</span>
            <span class="set-sub">{{ t('messenger.trustedSub') }}</span>
          </div>
          <button
            class="sw"
            :class="{ on: settings.onlyAllowTrusted }"
            @click="settings.onlyAllowTrusted = !settings.onlyAllowTrusted; saveSettings()"
            data-testid="chat-set-trusted-toggle"
          ><span class="sw-knob" /></button>
        </label>

        <div class="set-row col">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.msgCost') }}</span>
            <span class="set-sub">{{ t('messenger.msgCostSub') }}</span>
          </div>
          <div class="cost-row">
            <input
              type="number"
              min="0"
              step="0.001"
              class="cost-input"
              v-model.number="settings.msgCostSth"
              @change="saveSettings"
              data-testid="chat-set-cost-input"
            />
            <span class="cost-unit">STH</span>
            <button
              class="cost-pub"
              :disabled="publishingCost"
              @click="doPublishCost"
              data-testid="chat-set-cost-publish"
            >{{ publishingCost ? t('messenger.publishing') : (settings.costPublished ? t('messenger.updateRate') : t('messenger.publish')) }}</button>
          </div>
          <span class="set-hint">{{ t('messenger.rateNote') }}</span>
        </div>

        <div class="set-row col" data-testid="chat-set-cache">
          <div class="set-info">
            <span class="set-label">{{ t('messenger.cacheTitle') }}</span>
            <span class="set-sub">{{ t('messenger.cacheSub') }}</span>
          </div>
          <div class="cost-row">
            <input
              type="number"
              min="0.1"
              step="0.5"
              class="cost-input"
              v-model.number="cacheInfo.maxGb"
              @change="saveCache"
              data-testid="chat-cache-max-input"
            />
            <span class="cost-unit">GB</span>
            <button
              class="sw"
              :class="{ on: cacheInfo.autoclean }"
              @click="cacheInfo.autoclean = !cacheInfo.autoclean; saveCache()"
              data-testid="chat-cache-autoclean-toggle"
            ><span class="sw-knob" /></button>
            <span class="set-hint">{{ t('messenger.cacheAutoclean') }}</span>
          </div>
          <div class="cost-row">
            <span class="set-hint" data-testid="chat-cache-usage">{{ t('messenger.cacheUsage', { mb: fmtMb(cacheInfo.bytes), files: cacheInfo.files }) }}</span>
            <button class="cost-pub" @click="clearCache" data-testid="chat-cache-clear">{{ t('messenger.cacheClear') }}</button>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.cw {
  --chat-text-size: 14px;
  height: 100vh;
  position: relative;
  display: flex;
  flex-direction: column;
  background: var(--app-bg);
  color: #e4e4e7;
  font-family: 'Inter', 'Roboto', system-ui, sans-serif;
  overflow: hidden;
}
.titlebar {
  height: 34px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 8px 0 12px;
  background: linear-gradient(180deg, var(--panel-2), var(--panel-solid));
  border-bottom: 1px solid var(--border);
  box-shadow: inset 0 1px 0 var(--border);
  user-select: none;
}
.tb-title {
  font-family: 'JetBrains Mono', 'Fira Code', monospace;
  font-size: 11px;
  letter-spacing: 0.14em;
  color: var(--accent);
}
.tb-ctrls { display: flex; gap: 4px; }
.tb-btn {
  width: 30px;
  height: 24px;
  background: transparent;
  color: #a1a1aa;
  border: 1px solid transparent;
  cursor: pointer;
  font-size: 13px;
}
.tb-btn:hover { background: var(--panel-2); color: #fff; }
.tb-btn.close:hover { background: #b91c1c; color: #fff; }
.tb-btn.font { font-family: 'JetBrains Mono', monospace; font-size: 11px; font-weight: var(--fw-700); }
.keybar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 12px;
  background: #1c1407;
  border-bottom: 1px solid #92610a;
  font-size: 12px;
  color: #fbbf24;
}
.kb-btn {
  background: #f59e0b;
  color: #1a1206;
  border: none;
  padding: 6px 12px;
  font-weight: var(--fw-800);
  font-size: 11px;
  cursor: pointer;
  white-space: nowrap;
}
.split { flex: 1; display: flex; min-height: 0; }
.m-back { display: none; }
/* ---- телефон (Android / узкое окно): один экран за раз ------------------------------ */
@media (max-width: 760px) {
  .cw { height: 100dvh; }
  /* edge-to-edge WebView без нативных отступов: не прячем шапку под статус-бар, а ввод — под навигацию */
  .cw.mobile .titlebar { padding: calc(6px + env(safe-area-inset-top, 0px)) 10px 6px; }
  .cw.mobile .composer { padding: 8px 10px calc(8px + env(safe-area-inset-bottom, 0px)); gap: 4px; }
  .cw.mobile .composer-row { align-items: flex-end; gap: 6px; }
  .cw.mobile .composer textarea { border-radius: 18px; padding: 10px 14px; min-height: 42px; }
  .cw.mobile .composer-row > button { min-width: 42px; min-height: 42px; border-radius: 50%; padding: 0; display: inline-flex; align-items: center; justify-content: center; }
  .cw.mobile .feed { padding-bottom: 12px; overscroll-behavior: contain; }
  .cw.mobile .tb-title { display: none; }
  .cw.mobile .tb-btn.font, .cw.mobile [data-testid='chat-min'], .cw.mobile [data-testid='chat-close'] { display: none; }
  .cw.mobile .tb-btn { width: 36px; height: 36px; font-size: 16px; }
  .cw.mobile .left { width: 100%; min-width: 0; border-right: none; }
  .cw.m-chat .left { display: none; }
  .cw.m-list .right { display: none; }
  .cw.mobile .m-back {
    display: inline-flex; align-items: center; justify-content: center;
    width: 36px; height: 36px; margin-right: 2px; border-radius: 10px; border: none;
    background: transparent; color: var(--accent); font-size: 26px; line-height: 1; cursor: pointer;
  }
  .cw.mobile .m-back:active { background: var(--panel-2); }
  .cw.mobile .conv-head { padding: 8px 10px; gap: 8px; position: relative; }
  .cw.mobile .ch-addr { display: none; }
  .cw.mobile .ch-meta { flex: 1; min-width: 0; }
  .cw.mobile .ch-name { display: flex; flex-direction: column; gap: 2px; min-width: 0; font-size: 14px; }
  .cw.mobile .ch-title { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .cw.mobile .ch-sub { display: flex; align-items: center; gap: 8px; min-width: 0; overflow: hidden; white-space: nowrap; }
  .cw.mobile .ch-sub .presence, .cw.mobile .ch-sub .subs-count { margin-left: 0; flex: none; }
  .cw.mobile .ch-sub .subs-count { overflow: hidden; text-overflow: ellipsis; flex: 0 1 auto; }
  .cw.mobile .ch-policy { margin-top: 2px; }
  .cw.mobile .conv-actions { gap: 6px; }
  .cw.mobile .call-btn { width: 36px; height: 36px; padding: 0; justify-content: center; border-radius: 10px; }
  .cw.mobile .call-btn svg { width: 18px; height: 18px; }
  .cw.mobile .cb-txt { display: none; }
  .cw.mobile .more-btn { width: 36px; height: 36px; font-size: 20px; line-height: 1; border-radius: 10px; }
  .cw.mobile .acts {
    display: none; position: absolute; right: 10px; top: calc(100% + 6px); z-index: 60;
    flex-direction: column; align-items: stretch; gap: 4px; min-width: 220px; padding: 6px;
    background: var(--panel-solid); border: 1px solid var(--border); border-radius: 12px;
    box-shadow: 0 14px 40px rgba(0, 0, 0, 0.55);
  }
  .cw.mobile .acts.open { display: flex; animation: acts-in 0.14s ease-out; }
  .cw.mobile .acts > button {
    width: auto; height: auto; display: flex; align-items: center; gap: 10px; text-align: left;
    padding: 11px 12px; font-size: 13px; border-radius: 8px; border: none;
  }
  .cw.mobile .acts > .admin-gear { color: #e4e4e7; }
  .cw.mobile .acts > .admin-gear.on { color: var(--accent); }
  .cw.mobile .act-txt { display: inline; }
  .cw.mobile .acts > .invite { color: var(--accent); }
  .cw.mobile .acts > .wipe { color: #ef4444; }
  @keyframes acts-in { from { opacity: 0; transform: translateY(-4px); } to { opacity: 1; transform: none; } }
  .cw.mobile .bubble { max-width: 86%; }
  .cw.mobile .composer-row textarea, .cw.mobile .composer-row input { font-size: 16px; } /* iOS/Android: без автозума */
  .cw.mobile .composer-row textarea::placeholder { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .cw.mobile .room { padding: 10px 12px; }
}
.left {
  width: 320px;
  min-width: 320px;
  background: var(--panel-solid);
  border-right: 1px solid var(--border);
  display: flex;
  flex-direction: column;
  min-height: 0;
}
.search { padding: 10px; border-bottom: 1px solid var(--border); display: flex; gap: 6px; }
.search input {
  flex: 1;
  min-width: 0;
  background: var(--panel-2);
  border: 1px solid var(--border);
  color: #e4e4e7;
  padding: 9px 11px;
  font-family: 'JetBrains Mono', monospace;
  font-size: 12px;
  outline: none;
}
.search input:focus { border-color: var(--accent); }
.new-group {
  flex: none;
  background: var(--accent-soft);
  border: 1px solid #1f5a52;
  color: var(--accent);
  font-size: 11px;
  font-weight: var(--fw-700);
  padding: 0 9px;
  cursor: pointer;
  white-space: nowrap;
}
.new-group:hover { background: var(--panel-hover); }
.tabs { display: flex; border-bottom: 1px solid var(--border); }
.tab {
  flex: 1;
  background: transparent;
  border: none;
  color: #71717a;
  padding: 9px;
  font-size: 12px;
  cursor: pointer;
  border-bottom: 2px solid transparent;
}
.tab.on { color: var(--accent); border-bottom-color: var(--accent); }
.rooms, .contacts { flex: 1; overflow-y: auto; min-height: 0; }
.contacts { border-bottom: 1px solid var(--border); }
.room, .contact {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 12px;
  background: transparent;
  border: none;
  border-bottom: 1px solid var(--panel-2);
  border-left: 2px solid transparent;
  cursor: pointer;
  text-align: left;
}
.room:hover, .contact:hover { background: var(--border); }
.r-av { flex: none; line-height: 0; transition: filter 0.25s ease; }
.r-av.offline :deep(.identicon) {
  box-shadow: none;
  filter: grayscale(1) brightness(0.82);
  border-color: rgba(255, 255, 255, 0.05);
}
.room.active { background: var(--border); border-left-color: var(--accent); }
.r-meta { display: flex; flex-direction: column; min-width: 0; gap: 2px; flex: 1; }
.r-top { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
.r-name, .c-name { font-size: 13px; font-weight: var(--fw-600); color: #e4e4e7; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.r-group-ic { color: var(--accent); margin-right: 2px; }
.r-time { flex: none; font-family: 'JetBrains Mono', monospace; font-size: 10px; color: #52525b; }
.r-last { font-size: 11px; color: #71717a; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; min-width: 0; }
.r-bot { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
.r-badge {
  flex: none;
  min-width: 18px;
  height: 18px;
  padding: 0 5px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  background: var(--accent);
  color: #04110f;
  font-family: 'JetBrains Mono', monospace;
  font-size: 10px;
  font-weight: var(--fw-800);
  border-radius: 9px !important;
  box-shadow: 0 0 8px rgba(0, 245, 255, 0.5);
}
.empty, .feed-empty { color: #52525b; font-size: 12px; padding: 16px; text-align: center; }
.right { flex: 1; display: flex; flex-direction: column; min-width: 0; background: transparent; }
.conv-head {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 14px;
  border-bottom: 1px solid var(--border);
  background: linear-gradient(180deg, var(--panel-2), var(--panel-solid));
  box-shadow: inset 0 1px 0 var(--panel-2);
}
.ch-meta { display: flex; flex-direction: column; min-width: 0; }
.ch-name { font-size: 14px; font-weight: var(--fw-700); }
.ch-addr {
  font-family: 'JetBrains Mono', monospace;
  font-size: 10px;
  color: #52525b;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 360px;
}
.conv-actions { margin-left: auto; display: flex; align-items: center; gap: 8px; flex: none; }
.acts { display: contents; }
.act-txt { display: none; }
.ch-sub { display: contents; }
.call-btn {
  display: inline-flex; align-items: center; gap: 6px;
  background: rgba(34, 197, 94, 0.12); border: 1px solid rgba(34, 197, 94, 0.45); color: #22c55e;
  height: 28px; padding: 0 10px; border-radius: 6px; font-size: 12px; cursor: pointer;
  transition: background-color 0.15s, transform 0.15s;
}
.call-btn:hover:not(:disabled) { background: rgba(34, 197, 94, 0.22); transform: translateY(-1px); }
.call-btn:disabled { opacity: 0.45; cursor: default; }
.call-btn.live { border-color: #22c55e; color: #22c55e; animation: call-pulse 1.4s ease-in-out infinite; }
.call-bar {
  display: flex; align-items: center; gap: 10px; padding: 8px 14px;
  background: light-dark(rgba(34, 197, 94, 0.08), rgba(34, 197, 94, 0.1));
  border-bottom: 1px solid rgba(34, 197, 94, 0.35); font-size: 13px;
}
.call-bar.ringing { background: light-dark(rgba(0, 245, 255, 0.08), rgba(0, 245, 255, 0.1)); border-bottom-color: rgba(0, 245, 255, 0.4); }
.call-ic { color: #22c55e; font-size: 10px; }
.call-bar.ringing .call-ic { color: var(--accent); }
.call-ic.pulse { animation: call-pulse 1s ease-in-out infinite; }
@keyframes call-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.2; } }
.call-who { font-weight: 600; }
.call-st { opacity: 0.8; font-variant-numeric: tabular-nums; }
.call-sp { flex: 1; }
.call-act {
  background: transparent; border: 1px solid rgba(255, 255, 255, 0.25); color: var(--text);
  height: 26px; padding: 0 10px; border-radius: 6px; font-size: 12px; cursor: pointer; transition: background-color 0.15s;
}
.call-act:hover { background: rgba(255, 255, 255, 0.08); }
.call-act.on { border-color: #f59e0b; color: #f59e0b; }
.call-act.ptt { min-width: 118px; touch-action: none; user-select: none; -webkit-user-select: none; border-color: var(--accent); color: var(--accent); }
.call-act.ptt.held { background: #22c55e; border-color: #22c55e; color: #fff; }
.call-act.accept { border-color: #22c55e; color: #22c55e; }
.call-act.end { border-color: #ef4444; color: #ef4444; }
.admin-gear.on { border-color: var(--accent); color: var(--accent); }
.call-who.talking { color: #22c55e; text-shadow: 0 0 8px rgba(34, 197, 94, 0.6); }
.spk-dot { font-size: 10px; color: #22c55e; animation: call-pulse 0.6s ease-in-out infinite; }
.call-members { display: inline-flex; gap: 6px; flex-wrap: wrap; }
.member { padding: 1px 8px; border-radius: 10px; border: 1px solid rgba(255, 255, 255, 0.2); font-size: 11px; font-family: monospace; opacity: 0.8; transition: border-color 0.15s, color 0.15s; }
.member.talking { border-color: #22c55e; color: #22c55e; opacity: 1; box-shadow: 0 0 8px rgba(34, 197, 94, 0.45); }
.member.empty { font-family: inherit; opacity: 0.55; border-style: dashed; }
.recall-btn { margin-left: 8px; background: rgba(34, 197, 94, 0.12); border: 1px solid rgba(34, 197, 94, 0.45); color: #22c55e; border-radius: 6px; padding: 1px 8px; font-size: 11px; cursor: pointer; }
.recall-btn:disabled { opacity: 0.45; cursor: default; }
.audio-panel {
  display: grid; grid-template-columns: 1fr 1fr; gap: 10px 18px; padding: 12px 14px;
  background: light-dark(rgba(0, 0, 0, 0.04), rgba(255, 255, 255, 0.04)); border-bottom: 1px solid var(--line); font-size: 12px;
}
.ap-row { display: flex; align-items: center; gap: 10px; min-width: 0; }
.ap-l { width: 90px; flex: none; opacity: 0.75; }
.ap-sel { flex: 1; min-width: 0; height: 28px; background: transparent; color: var(--text); border: 1px solid var(--line); border-radius: 6px; padding: 0 6px; font-size: 12px; }
.ap-meter { flex: 1; height: 10px; border-radius: 5px; background: rgba(255, 255, 255, 0.08); overflow: hidden; }
.ap-fill { height: 100%; background: #22c55e; transition: width 0.1s linear; }
.ap-fill.hot { background: #ef4444; }
.ap-toggle { grid-column: 1 / -1; cursor: pointer; }
.ap-hint { opacity: 0.55; font-size: 11px; }
.ap-close { grid-column: 1 / -1; justify-self: end; }
.admin-gear {
  background: transparent;
  border: 1px solid rgba(0, 245, 255, 0.4);
  color: var(--accent);
  width: 30px;
  height: 28px;
  font-size: 14px;
  cursor: pointer;
}
.admin-gear:hover { background: rgba(0, 245, 255, 0.12); }
.ch-policy { display: flex; gap: 6px; margin-top: 3px; flex-wrap: wrap; }
.pol-badge {
  font-family: 'JetBrains Mono', monospace;
  font-size: 9px;
  letter-spacing: 0.04em;
  color: #a1a1aa;
  border: 1px solid #3f3f46;
  padding: 1px 6px;
}
.pol-badge.fee { color: #fbbf24; border-color: #92610a; }
.pol-badge.channel { color: var(--status); border-color: var(--border-accent); }
.discuss-toggle { display: flex; justify-content: center; padding: 8px 0 4px; }
.discuss-btn {
  font-family: 'JetBrains Mono', monospace;
  font-size: 11px;
  letter-spacing: 0.04em;
  color: var(--status);
  background: rgba(8, 32, 38, 0.6);
  border: 1px solid var(--border-accent);
  padding: 5px 14px;
  cursor: pointer;
  transition: background-color 0.18s ease, color 0.18s ease;
}
.discuss-btn:hover { background: rgba(14, 116, 144, 0.25); color: #a5f3fc; }
.channel-note {
  font-family: 'JetBrains Mono', monospace;
  font-size: 11px;
  letter-spacing: 0.03em;
  color: var(--status);
  background: rgba(8, 32, 38, 0.55);
  border-top: 1px solid var(--border-accent);
  padding: 6px 14px;
}
.cw-toast {
  position: fixed;
  left: 50%;
  bottom: 56px;
  transform: translateX(-50%);
  z-index: 9999;
  max-width: 420px;
  font-family: 'JetBrains Mono', monospace;
  font-size: 12px;
  line-height: 1.4;
  color: #e4e4e7;
  background: linear-gradient(180deg, #1a1a1f, #0d0d10);
  border: 1px solid var(--border-accent);
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.5);
  padding: 10px 16px;
  text-align: center;
}
.toast-pop-enter-active, .toast-pop-leave-active { transition: opacity 0.24s ease, transform 0.24s ease; }
.toast-pop-enter-from, .toast-pop-leave-to { opacity: 0; transform: translateX(-50%) translateY(8px); }
.subs-count {
  font-size: 10px;
  letter-spacing: 0.03em;
  color: #a5b4fc;
  border: 1px solid #3730a3;
  padding: 1px 7px;
  margin-left: 8px;
}
.pinned-banner {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 12px;
  margin-bottom: 8px;
  background: rgba(8, 32, 38, 0.55);
  border: 1px solid var(--border-accent);
  border-left: 3px solid #22d3ee;
}
.pin-ic { font-size: 14px; flex: 0 0 auto; }
.pin-body { display: flex; flex-direction: column; gap: 2px; min-width: 0; flex: 1; }
.pin-label { font-size: 9px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--status); }
.pin-text {
  font-size: 12px;
  color: #d4d4d8;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.pin-unpin {
  flex: 0 0 auto;
  background: transparent;
  border: 1px solid #155e63;
  color: var(--status);
  font-size: 11px;
  padding: 2px 7px;
  cursor: pointer;
  transition: background-color 0.16s ease;
}
.pin-unpin:hover { background: rgba(14, 116, 144, 0.3); }
.msg-row.pinned .bubble { box-shadow: 0 0 0 1px #22d3ee; }
.ctx-backdrop { position: fixed; inset: 0; z-index: 9998; }
.ctx-menu {
  position: fixed;
  z-index: 9999;
  min-width: 180px;
  background: linear-gradient(180deg, #1a1a1f, #0d0d10);
  border: 1px solid #3f3f46;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.55);
  padding: 4px;
}
.ctx-item {
  display: block;
  width: 100%;
  text-align: left;
  font-family: 'JetBrains Mono', monospace;
  font-size: 12px;
  color: #e4e4e7;
  background: transparent;
  border: none;
  padding: 9px 12px;
  cursor: pointer;
  transition: background-color 0.14s ease;
}
.ctx-item:hover { background: rgba(14, 116, 144, 0.22); color: #a5f3fc; }
.ctx-reacts {
  display: flex;
  gap: 2px;
  padding: 4px 6px 8px;
  border-bottom: 1px solid #27272e;
  margin-bottom: 4px;
}
.ctx-react {
  background: transparent;
  border: none;
  font-size: 18px;
  line-height: 1;
  padding: 5px 6px;
  cursor: pointer;
  border-radius: 999px;
  transition: transform 0.12s ease, background-color 0.12s ease;
}
.ctx-react:hover { background: rgba(14, 116, 144, 0.25); transform: scale(1.25); }
.ctx-react-more { font-size: 14px; color: var(--text-dim); align-self: center; padding: 6px 8px; }
.ctx-react-more.on { background: rgba(14, 116, 144, 0.35); color: #e4e4e7; }
.ctx-react-more:hover { transform: none; }
.ctx-emoji { padding: 0 2px 6px; border-bottom: 1px solid #27272e; margin-bottom: 4px; }
.ctx-emoji :deep(.v3-emoji-picker) { width: 100%; min-width: 320px; max-width: 360px; height: 300px; box-shadow: none; background: transparent; }
.b-reacts { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 5px; }
.b-quote {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 8px;
  margin-bottom: 5px;
  border-left: 2px solid var(--accent);
  background: rgba(255, 255, 255, 0.06);
  border-radius: 4px;
  cursor: pointer;
  max-width: 100%;
  transition: background-color 0.12s ease;
}
.b-quote:hover { background: rgba(255, 255, 255, 0.11); }
.q-col { display: flex; flex-direction: column; gap: 1px; min-width: 0; }
.q-thumb {
  width: 34px;
  height: 34px;
  object-fit: cover;
  border-radius: 4px;
  flex: none;
}
.q-aud { color: var(--accent); flex: none; }
.q-sender { font-size: 11px; font-weight: 700; color: var(--accent); }
.q-text {
  font-size: 12px;
  color: #c7cbd1;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 320px;
}
.msg-row.flash .bubble { animation: msg-flash 1.2s ease; }
@keyframes msg-flash {
  0%, 100% { box-shadow: none; }
  25% { box-shadow: 0 0 0 2px var(--accent); }
}
.b-edited { font-style: italic; opacity: 0.75; margin-right: 4px; }
.edit-strip .reply-name { color: var(--accent, #6ab0f3); }
.reply-strip {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 6px 10px;
  margin-bottom: 6px;
  border-left: 2px solid var(--accent);
  background: rgba(255, 255, 255, 0.05);
  border-radius: 4px;
}
.reply-icn { color: var(--accent); flex: none; }
.reply-body { display: flex; flex-direction: column; gap: 1px; flex: 1; min-width: 0; }
.reply-name { font-size: 11px; font-weight: 700; color: var(--accent); }
.reply-text {
  font-size: 12px;
  color: #c7cbd1;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.react-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  background: rgba(255, 255, 255, 0.07);
  border: 1px solid transparent;
  border-radius: 999px;
  padding: 2px 8px;
  font-size: 13px;
  line-height: 1.3;
  color: #e4e4e7;
  cursor: pointer;
  transition: background-color 0.12s ease, border-color 0.12s ease;
}
.react-chip:hover { background: rgba(255, 255, 255, 0.14); }
.react-chip.mine { background: rgba(14, 116, 144, 0.35); border-color: rgba(0, 245, 255, 0.35); }
.react-n { font-size: 11px; font-family: 'JetBrains Mono', monospace; color: #a5f3fc; }
.set-row.inner { padding: 0; border: none; }
.ban-dur { display: flex; flex-direction: column; gap: 6px; margin-bottom: 4px; }
.ban-dur-l { font-size: 10px; letter-spacing: 0.08em; text-transform: uppercase; color: #8a8f99; }
.ban-dur-chips { display: flex; gap: 6px; flex-wrap: wrap; }
.ban-chip {
  padding: 5px 12px;
  border: 1px solid var(--panel-2);
  background: transparent;
  color: #9aa0ab;
  font-size: 11px;
  cursor: pointer;
  border-radius: 999px;
  transition: border-color 0.15s, color 0.15s, background 0.15s;
}
.ban-chip:hover { border-color: #3a4250; color: #e4e4e7; }
.ban-chip.active { border-color: #ef4444; color: #fecaca; background: rgba(127, 29, 29, 0.25); }
.mod-kick:disabled, .mod-unban:disabled { opacity: 0.5; cursor: not-allowed; }
.room-banned { filter: grayscale(1) brightness(0.7); opacity: 0.7; }
.room-banned:hover { background: transparent; }
.r-banned-tag { font-size: 11px; color: #ef4444; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.mod-meta { display: flex; flex-direction: column; gap: 1px; flex: 1; min-width: 0; }
.mod-ban-tag { font-size: 10px; color: #ef4444; font-weight: 600; letter-spacing: 0.02em; }
.mod-av { line-height: 0; flex: none; transition: filter 0.2s ease; }
.mod-av.flat :deep(.identicon) { box-shadow: none; filter: grayscale(1) brightness(0.85); }
.mod-banned .mod-name { color: var(--text-dim); }
.mod-list { display: flex; flex-direction: column; gap: 6px; max-height: 200px; overflow-y: auto; }
.mod-item { display: flex; align-items: center; gap: 8px; padding: 6px; border: 1px solid var(--panel-2); }
.mod-item :deep(.identicon) { border: 1px solid rgba(0, 245, 255, 0.3); }
.mod-name { flex: 1; min-width: 0; font-size: 12px; color: #e4e4e7; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.mod-kick { background: transparent; border: 1px solid #7f1d1d; color: #ef4444; font-size: 10px; padding: 4px 8px; cursor: pointer; }
.mod-kick:hover { background: #7f1d1d; color: #fff; }
.mod-unban { background: transparent; border: 1px solid #1f5a52; color: var(--accent); font-size: 10px; padding: 4px 8px; cursor: pointer; }
.mod-empty { font-size: 11px; color: #52525b; }
.admin-err { padding: 8px 14px; margin: 0; background: #450a0a; color: #fca5a5; font-size: 11px; }
.admin-actions { justify-content: stretch; }
.cost-pub.wide { width: 100%; margin-left: 0; }
.invite {
  background: transparent;
  border: 1px solid #1f5a52;
  color: var(--accent);
  padding: 6px 10px;
  font-size: 11px;
  cursor: pointer;
}
.invite:hover { background: var(--accent-soft); }
.leave {
  background: transparent;
  border: 1px solid #3f3f46;
  color: #a1a1aa;
  padding: 6px 10px;
  font-size: 11px;
  cursor: pointer;
}
.leave:hover { background: #27272a; color: #fff; }
.wipe {
  background: transparent;
  border: 1px solid #7f1d1d;
  color: #ef4444;
  padding: 6px 10px;
  font-size: 11px;
  cursor: pointer;
}
.wipe:hover { background: #7f1d1d; color: #fff; }
.sys-line {
  align-self: center;
  margin: 4px 0;
  padding: 3px 10px;
  font-family: 'JetBrains Mono', monospace;
  font-size: 10px;
  letter-spacing: 0.04em;
  color: #707070;
  background: var(--panel-2);
  border: 1px solid var(--border);
}
.feed { flex: 1; overflow-y: auto; padding: 16px; display: flex; flex-direction: column; gap: 10px; min-height: 0; }
.msg-row { display: flex; align-items: flex-end; gap: 8px; max-width: 82%; align-self: flex-start; }
.msg-row.mine { align-self: flex-end; flex-direction: row-reverse; }
.msg-row.cont { margin-top: -8px; }
.av-spacer { width: 28px; flex: none; }
.av {
  display: inline-flex;
  flex: none;
  border-radius: 50% !important;
  overflow: hidden;
  border: 1px solid var(--border-subtle);
  background: var(--panel-solid);
}
.av :deep(.identicon) { border-radius: 0 !important; border: none !important; box-shadow: none !important; }
.msg-av { width: 28px; height: 28px; }
.av-lg { width: 34px; height: 34px; }
.av-btn { padding: 0; border: 0; background: transparent; cursor: pointer; border-radius: 50%; transition: transform 0.15s, box-shadow 0.15s; }
.av-btn:hover { transform: scale(1.06); box-shadow: 0 0 0 2px rgba(45, 212, 191, 0.45); }
.gh-mosaic {
  width: 34px;
  height: 34px;
  flex: none;
  border-radius: 50% !important;
  overflow: hidden;
  border: 1px solid rgba(0, 245, 255, 0.3);
  display: grid;
  background: #0b0e13;
}
.gh-mosaic.n1 { grid-template-columns: 1fr; }
.gh-mosaic.n2 { grid-template-columns: 1fr 1fr; }
.gh-mosaic.n3, .gh-mosaic.n4 { grid-template-columns: 1fr 1fr; grid-template-rows: 1fr 1fr; }
.gm-cell { overflow: hidden; display: flex; align-items: center; justify-content: center; }
.gm-cell :deep(.identicon) { width: 100% !important; height: 100% !important; border-radius: 0 !important; border: none !important; box-shadow: none !important; }
.bubble {
  max-width: 100%;
  background: var(--panel-2);
  border: 1px solid var(--border);
  border-radius: 18px;
  padding: 7px 13px 6px;
  line-height: 1.45;
  white-space: pre-wrap;
  word-break: break-word;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.25);
}
.bubble.mine {
  background: var(--accent-soft);
  border-color: var(--border-accent);
}
/* Telegram-style tails: the pointed corner appears only on the LAST bubble of a
   run (next to the avatar). Joining corners flatten so stacked bubbles read as
   one group. */
.bubble.last:not(.mine) { border-bottom-left-radius: 6px; }
.bubble.mine.last { border-bottom-right-radius: 6px; }
.bubble:not(.first):not(.mine) { border-top-left-radius: 10px; }
.bubble.mine:not(.first) { border-top-right-radius: 10px; }
.bubble:not(.last):not(.mine) { border-bottom-left-radius: 10px; }
.bubble.mine:not(.last) { border-bottom-right-radius: 6px; }

/* Telegram-feed mode: EVERY message is left-aligned with the avatar on the
   left (own + others). Own bubbles keep a subtle accent tint for identity but
   mirror the incoming bubble geometry (tail on the left next to the avatar). */
.msg-row.mine { align-self: flex-start; flex-direction: row; }
.bubble.mine { border-radius: 16px; }
.bubble.mine.last { border-bottom-left-radius: 5px; border-bottom-right-radius: 16px; }
.bubble.mine:not(.first) { border-top-left-radius: 6px; border-top-right-radius: 16px; }
.bubble.mine:not(.last) { border-bottom-left-radius: 6px; border-bottom-right-radius: 16px; }
.b-time { text-align: left; }
.b-sender { display: block; font-family: var(--font-mono, monospace); font-size: 10px; color: var(--accent); margin-bottom: 2px; }
.b-sender-link { cursor: pointer; width: fit-content; }
.b-sender-link:hover { text-decoration: underline; }
.c-name-link { cursor: pointer; }
.c-name-link:hover { text-decoration: underline; }
.b-text {
  display: block;
  font-family: var(--font-mono, monospace);
  font-variant-ligatures: none;
  font-size: var(--chat-text-size, 14px);
  color: var(--text);
}
.b-time { display: block; margin-top: 3px; font-family: var(--font-mono, monospace); font-weight: var(--fw-400); font-size: 10px; color: var(--text-dim); text-align: right; }
.composer { position: relative; display: flex; flex-direction: column; gap: 6px; padding: 10px; border-top: 1px solid var(--border); background: var(--panel-solid); }
.av-link { cursor: pointer; transition: transform 0.15s ease, box-shadow 0.15s ease; }
.av-link:hover { transform: scale(1.08); box-shadow: 0 0 0 2px var(--accent); }
.mention-pop {
  position: absolute;
  left: 10px;
  right: 10px;
  bottom: calc(100% - 4px);
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  padding: 8px;
  background: var(--panel-solid);
  border: 1px solid var(--border-accent, var(--border));
  border-radius: 8px;
  box-shadow: 0 -8px 24px rgba(0, 0, 0, 0.35);
  z-index: 5;
}
.mention-item {
  font: inherit;
  font-size: 12px;
  padding: 4px 10px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--bg);
  color: var(--text);
  cursor: pointer;
  transition: border-color 0.12s ease, background-color 0.12s ease, color 0.12s ease;
}
.mention-item.on, .mention-item:hover { border-color: var(--accent); color: var(--accent); background: color-mix(in srgb, var(--accent) 12%, transparent); }
.mention-at { opacity: 0.6; margin-right: 1px; }
.composer-row { display: flex; gap: 8px; width: 100%; align-items: flex-end; }
.composer textarea {
  flex: 1;
  resize: none;
  background: var(--panel-2);
  border: 1px solid var(--border);
  color: #e4e4e7;
  padding: 11px;
  font-family: inherit;
  font-size: 14px;
  line-height: 1.4;
  outline: none;
  min-height: 42px;
  max-height: 300px;
  overflow-y: auto;
  scrollbar-width: thin;
}
.composer textarea::-webkit-scrollbar { width: 6px; }
.composer textarea::-webkit-scrollbar-thumb { background: #27272a; }
.composer textarea::-webkit-scrollbar-track { background: transparent; }
.composer textarea:focus { border-color: var(--accent); }
.send {
  flex: none;
  align-self: stretch;
  background: var(--accent);
  color: #04110f;
  border: none;
  padding: 0 18px;
  font-size: 15px;
  font-weight: var(--fw-800);
  cursor: pointer;
}
.splash { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 8px; color: #3f3f46; }
.sp-logo { font-size: 40px; }
.sp-title { font-size: 16px; font-weight: var(--fw-700); color: #71717a; }
.sp-sub { font-size: 12px; }
.cw-error { margin: 0; padding: 8px 14px; background: #450a0a; color: #fca5a5; font-size: 12px; display: flex; align-items: center; gap: 10px; }
.cw-error-text { flex: 1; min-width: 0; overflow-wrap: anywhere; }
.cw-error-x { flex: none; background: none; border: none; color: #fca5a5; cursor: pointer; font-size: 12px; padding: 2px 6px; opacity: 0.8; }
.cw-error-x:hover { opacity: 1; }
.composer.locked textarea { cursor: pointer; opacity: 0.7; }
.meshbar { display: flex; align-items: center; gap: 8px; padding: 6px 12px; font-size: 11.5px; letter-spacing: 0.04em; background: #1c1407; color: #f59e0b; border-bottom: 1px solid rgba(245, 158, 11, 0.25); }
.meshbar.ready { background: #062a20; color: var(--accent); border-bottom-color: color-mix(in srgb, var(--accent) 30%, transparent); }
.mesh-spin { width: 10px; height: 10px; border-radius: 50%; border: 2px solid rgba(245, 158, 11, 0.3); border-top-color: #f59e0b; animation: mesh-rot 0.8s linear infinite; }
@keyframes mesh-rot { to { transform: rotate(360deg); } }
.composer.locked .send { opacity: 0.35; pointer-events: none; }
.wipe-back { position: fixed; inset: 0; background: rgba(8, 8, 10, 0.6); backdrop-filter: blur(8px); -webkit-backdrop-filter: blur(8px); display: flex; align-items: center; justify-content: center; z-index: 200; }
.wipe-card { width: 420px; max-width: 92%; background: #0a0a0a; border: 2px solid #ff3333; box-shadow: 0 0 24px rgba(255, 51, 51, 0.4); padding: 22px; font-family: 'JetBrains Mono', monospace; }
.wipe-title { color: #ff3333; font-size: 16px; letter-spacing: 0.14em; }
.wipe-body { color: #ff6b6b; font-size: 13px; line-height: 1.55; margin: 14px 0 18px; }
.wipe-actions { display: flex; gap: 10px; justify-content: flex-end; }
.wb { padding: 10px 14px; font-weight: var(--fw-800); font-size: 12px; cursor: pointer; background: transparent; }
.wb.cancel { color: var(--accent); border: 1px solid var(--accent); }
.wb.cancel:hover { background: var(--accent); color: #04110f; }
.wb.burn { color: #ff3333; border: 1px solid #ff3333; }
.wb.burn:hover { background: #ff3333; color: #0a0a0a; }
.done-btn { align-self: flex-end; margin-top: 4px; }

/* composer paid-note */
.paid-note {
  font-family: 'JetBrains Mono', monospace;
  font-size: 11px;
  color: #fbbf24;
  background: #1c1407;
  border: 1px solid #92610a;
  padding: 6px 9px;
  width: 100%;
}

/* settings overlay */
.set-back { position: fixed; inset: 0; background: rgba(8, 8, 10, 0.6); backdrop-filter: blur(8px); -webkit-backdrop-filter: blur(8px); display: flex; align-items: center; justify-content: center; z-index: 200; }
.set-card { width: 460px; max-width: 94%; max-height: min(92vh, 760px); overflow-y: auto; background: var(--panel-solid); border: 1px solid var(--border-accent); box-shadow: 0 0 28px var(--accent-glow); }
.set-head { display: flex; align-items: center; justify-content: space-between; padding: 12px 14px; border-bottom: 1px solid var(--border); background: #020202; }
.set-title { margin: 0; font-family: 'JetBrains Mono', monospace; font-size: 13px; letter-spacing: 0.12em; color: var(--accent); }
.set-x { background: transparent; border: none; color: #a1a1aa; cursor: pointer; font-size: 14px; }
.set-x:hover { color: #fff; }
.set-row { display: flex; align-items: center; justify-content: space-between; gap: 14px; padding: 14px; border-bottom: 1px solid var(--panel-2); }
.set-row.col { flex-direction: column; align-items: stretch; gap: 10px; }
.set-info { display: flex; flex-direction: column; gap: 3px; }
.set-label { font-size: 13px; font-weight: var(--fw-600); color: #e4e4e7; }
.set-sub { font-size: 11px; color: #71717a; line-height: 1.4; }
.set-sub.err { color: #f87171; }
/* ссылки в сообщениях: свои схемы + http(s), подсветка при наведении */
.msg-link {
  color: var(--accent);
  text-decoration: underline dotted;
  text-underline-offset: 2px;
  cursor: pointer;
  word-break: break-all;
  transition: background-color 0.12s ease, color 0.12s ease;
}
.msg-link:hover {
  background: var(--accent-soft, rgba(34, 211, 238, 0.14));
  text-decoration-style: solid;
}
/* magnet/contact/group: бейдж + короткое имя вместо километровой ссылки */
.msg-magnet,
.msg-contact,
.msg-group {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  text-decoration: none;
  border: 1px solid color-mix(in srgb, var(--accent) 40%, transparent);
  border-radius: 4px;
  padding: 1px 7px 1px 2px;
  margin: 1px 0;
}
.msg-magnet:hover,
.msg-contact:hover,
.msg-group:hover {
  border-color: var(--accent);
  text-decoration: none;
}
.magnet-badge,
.contact-badge,
.group-badge {
  flex: none;
  font-size: 9px;
  font-weight: 700;
  letter-spacing: 0.08em;
  padding: 1px 5px;
  border-radius: 3px;
  background: var(--accent);
  color: #04141a;
}
/* контакт отличаем зелёным оттенком (профиль/личность) */
.msg-contact {
  border-color: color-mix(in srgb, #34d399 40%, transparent);
}
.msg-contact:hover {
  border-color: #34d399;
}
.contact-badge {
  background: #34d399;
  color: #04140d;
}
/* группу отличаем янтарным оттенком (сообщество) */
.msg-group {
  border-color: color-mix(in srgb, #fbbf24 40%, transparent);
}
.msg-group:hover {
  border-color: #fbbf24;
}
.group-badge {
  background: #fbbf24;
  color: #140d04;
}
/* адресная книга (add contact) */
.abook { display: flex; flex-direction: column; gap: 4px; max-height: 220px; overflow-y: auto; }
.abook-row { display: flex; align-items: stretch; gap: 6px; }
.abook-open { flex: 1; min-width: 0; display: flex; flex-direction: column; align-items: flex-start; gap: 2px; background: #060608; border: 1px solid var(--border); padding: 6px 10px; cursor: pointer; text-align: left; transition: border-color 0.15s ease; }
.abook-open:hover { border-color: var(--accent); }
.abook-name { color: var(--accent); font-size: 12px; font-weight: 600; }
.abook-addr { color: #71717a; font-size: 10px; font-family: 'JetBrains Mono', monospace; overflow: hidden; text-overflow: ellipsis; max-width: 100%; }
.abook-del { flex: none; width: 30px; background: transparent; border: 1px solid var(--border); color: #71717a; cursor: pointer; transition: color 0.15s ease, border-color 0.15s ease; }
.abook-del:hover { color: #f87171; border-color: #f87171; }
.set-hint { font-size: 10px; color: #52525b; }
.sw { position: relative; width: 42px; height: 22px; flex: none; background: #27272a; border: 1px solid #3f3f46; cursor: pointer; transition: background-color 0.18s ease; }
.sw.on { background: var(--accent-soft); border-color: #1f5a52; }
.sw-knob { position: absolute; top: 2px; left: 2px; width: 16px; height: 16px; background: #71717a; transition: transform 0.18s ease, background-color 0.18s ease; }
.sw.on .sw-knob { transform: translateX(20px); background: var(--accent); }
.cost-row { display: flex; align-items: center; gap: 8px; }
.cost-input { width: 110px; background: var(--panel-2); border: 1px solid var(--border); color: #e4e4e7; padding: 8px 10px; font-family: 'JetBrains Mono', monospace; font-size: 13px; outline: none; }
.cost-input.wide { width: 100%; flex: 1; }
.cost-input:focus { border-color: var(--accent); }
.cost-unit { font-size: 12px; color: #a1a1aa; }
.cost-pub { margin-left: auto; background: var(--accent); color: #04110f; border: none; padding: 8px 12px; font-size: 11px; font-weight: var(--fw-800); cursor: pointer; }
.cost-pub:disabled { opacity: 0.6; cursor: default; }
.invite-link { flex: 1; min-width: 0; overflow-x: auto; white-space: nowrap; background: #060608; border: 1px solid var(--border); color: var(--accent); padding: 8px 10px; font-family: 'JetBrains Mono', monospace; font-size: 11px; }
.kb-fee { color: #fbbf24; }
.key-state { margin-left: 8px; font-size: 11px; font-weight: var(--fw-600); color: #71717a; }
.key-state.on { color: var(--accent); }
/* «✓ Ключ опубликован» — кликабельный статус вместо кнопки republish (открывает share contact) */
.key-ok { margin-left: auto; background: transparent; border: 1px solid var(--accent); color: var(--accent); padding: 7px 12px; font-size: 11px; font-weight: var(--fw-800); cursor: pointer; transition: background-color 0.15s, color 0.15s; }
.key-ok:hover { background: var(--accent); color: #04110f; }
.key-ok .key-node { margin-left: 6px; font-weight: var(--fw-600); opacity: 0.75; font-family: var(--mono, monospace); }
.set-hint.stale { color: #f59e0b; }

/* presence */
.presence {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  margin-left: 10px;
  font-size: 10px;
  font-weight: var(--fw-500);
  letter-spacing: 0.06em;
  color: #71717a;
  text-transform: lowercase;
  vertical-align: middle;
}
.presence.on { color: var(--accent); }
.pr-dot { width: 6px; height: 6px; background: #52525b; flex: none; }
.presence.on .pr-dot { background: var(--accent); box-shadow: 0 0 6px var(--accent); }
.pr-dot.typing { animation: typing-pulse 0.9s ease-in-out infinite; }
@keyframes typing-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.25; } }
.b-tick { margin-left: 5px; color: #52e0c0; opacity: 0.55; }
.b-tick.delivered { opacity: 1; }
.b-stat { margin-left: 7px; color: #8a8a8a; letter-spacing: 0.02em; }
.tb-left { display: flex; align-items: center; gap: 12px; min-width: 0; }
.p2p-hud {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 3px 9px;
  border: 1px solid #3f3f46;
  background: #161618;
  font-family: 'JetBrains Mono', monospace;
  font-size: 10px;
  letter-spacing: 0.1em;
  color: #71717a;
  cursor: default;
  user-select: none;
}
.p2p-hud.on { border-color: #1f5a52; color: var(--accent); }
.hud-dot {
  width: 7px;
  height: 7px;
  background: #52525b;
  flex: none;
}
.p2p-hud.on .hud-dot {
  background: var(--accent);
  box-shadow: 0 0 6px var(--accent);
  animation: hud-pulse 1.6s ease-in-out infinite;
}
@keyframes hud-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.4; } }
.hud-topics { color: inherit; opacity: 0.75; }
.hud-warn {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #facc15;
  box-shadow: 0 0 7px #facc15;
  flex: none;
  animation: hud-pulse 0.9s ease-in-out infinite;
  pointer-events: auto;
  cursor: help;
}
.p2p-hud > * { pointer-events: none; }

/* open / connecting spinner overlay */
.cw-loading {
  position: absolute;
  inset: 34px 0 0 0;
  z-index: 50;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 16px;
  background: #09090b;
}
.spinner {
  width: 34px;
  height: 34px;
  border: 2px solid #27272a;
  border-top-color: var(--accent);
  border-radius: 0;
  animation: cw-spin 0.8s linear infinite;
}
@keyframes cw-spin { to { transform: rotate(360deg); } }
.ld-text {
  font-family: 'JetBrains Mono', monospace;
  font-size: 11px;
  letter-spacing: 0.12em;
  color: #52525b;
}

/* ---- Theme-aware messenger bubbles ---- */
[data-theme='neon'] .bubble {
  border-color: var(--border-accent);
  box-shadow: 0 0 14px -7px var(--accent-glow);
}
[data-theme='neon'] .bubble.mine {
  box-shadow: 0 0 16px -5px var(--accent-glow);
}
[data-theme='neon'] .send {
  box-shadow: 0 0 18px -4px var(--accent-glow);
}

/* ============================================================
   TELEGRAM SKIN — мессенджер всегда в привычной палитре Telegram
   (тёмная «Night»), независимо от глобальной темы приложения.
   ============================================================ */
.cw {
  --app-bg: #0e1621;
  --panel-solid: #17212b;
  --panel: #17212b;
  --panel-2: #232e3c;
  --panel-hover: #202b36;
  --accent: #64b5ef;
  --accent-hover: #7cc0f2;
  --accent-soft: #2b5278;
  --accent-glow: rgba(100, 181, 239, 0.35);
  --border: rgba(255, 255, 255, 0.07);
  --border-subtle: rgba(255, 255, 255, 0.06);
  --border-accent: #2b5278;
  --secondary: #7d8e9b;
  --text: #ffffff;
  --text-dim: #8b98a5;
  color: #ffffff;
  font-family: 'Inter', 'Segoe UI', system-ui, 'Apple Color Emoji', 'Noto Color Emoji', sans-serif;
}
/* sans-serif вместо моноширинного во всех «человеческих» текстах чата */
.cw .b-text,
.cw .b-sender,
.cw .b-time,
.cw .r-name,
.cw .r-last,
.cw .ch-name,
.cw .search input {
  font-family: 'Inter', 'Segoe UI', system-ui, 'Apple Color Emoji', 'Noto Color Emoji', sans-serif;
}
.cw .b-text { color: #fff; font-variant-ligatures: normal; }
/* список чатов: наведение и активный чат — как в Telegram */
.cw .room:hover,
.cw .contact:hover { background: #202b36; }
.cw .room.active { background: #2b5278; border-left-color: transparent; }
.cw .room.active .r-name,
.cw .room.active .r-last,
.cw .room.active .r-time,
.cw .room.active .r-group-ic { color: #fff; }
.cw .r-badge { background: var(--accent); color: #fff; box-shadow: none; }
.cw .new-group { background: rgba(100, 181, 239, 0.12); border-color: transparent; color: var(--accent); }
/* пузыри сообщений */
.cw .bubble { background: #182533; border: none; box-shadow: 0 1px 1px rgba(0, 0, 0, 0.2); }
.cw .bubble.mine { background: var(--accent-soft); border: none; }
.cw .bubble .b-time { color: rgba(255, 255, 255, 0.55); }
.cw .b-sender { color: #64b5ef; }
/* композер */
.cw .composer { position: relative; background: var(--panel-solid); border-top: 1px solid var(--border); }
.cw .composer-row { align-items: flex-end; gap: 6px; }
.cw .composer textarea {
  background: var(--panel-2);
  border: none;
  border-radius: 18px;
  color: #fff;
  padding: 11px 14px;
}
.cw .composer textarea:focus { border: none; box-shadow: none; }
.cw .emoji-btn {
  flex: none;
  align-self: flex-end;
  width: 42px;
  height: 42px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: transparent;
  border: none;
  color: #7d8e9b;
  border-radius: 50%;
  cursor: pointer;
  transition: color 0.15s ease, background-color 0.15s ease;
}
.cw .emoji-btn:hover,
.cw .emoji-btn.on { color: var(--accent); background: rgba(100, 181, 239, 0.12); }
.cw .send {
  align-self: flex-end;
  /* тот же размер, что у микрофона (.mic 38px) — ровная строка отправки */
  width: 38px;
  height: 38px;
  min-width: 38px;
  padding: 0;
  border-radius: 50%;
  background: var(--accent);
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: background-color 0.15s ease;
}
.cw .send svg {
  width: 18px;
  height: 18px;
}
.cw .mic {
  align-self: flex-end;
}
.cw .send:hover { background: var(--accent-hover); }
/* панель эмодзи над композером */
.cw .emoji-panel {
  position: absolute;
  bottom: 100%;
  left: 8px;
  margin-bottom: 10px;
  z-index: 60;
  border-radius: 12px;
  overflow: hidden;
  box-shadow: 0 10px 34px rgba(0, 0, 0, 0.55);
}
.cw .emoji-panel :deep(.v3-emoji-picker) {
  --v3-bg-color: #17212b;
  --v3-bg-color-hover: #202b36;
  --v3-text-color: #fff;
  --v3-border-color: rgba(255, 255, 255, 0.08);
  width: 340px;
}
.emoji-pop-enter-active,
.emoji-pop-leave-active { transition: opacity 0.15s ease, transform 0.15s ease; }
.emoji-pop-enter-from,
.emoji-pop-leave-to { opacity: 0; transform: translateY(8px); }

/* ===== P2P-изображения: компактная карточка как в Telegram ===== */
.msg-img-wrap {
  display: block;
  max-width: 320px;
}
.imgcard {
  display: flex;
  align-items: center;
  gap: 10px;
  cursor: pointer;
  padding: 2px 4px 2px 2px;
  border-radius: 10px;
  min-width: 190px;
  transition: background-color 0.15s ease;
}
.imgcard:hover {
  background: rgba(255, 255, 255, 0.05);
}
.imgcard-thumb {
  flex: none;
  width: 52px;
  height: 52px;
  object-fit: cover;
  border-radius: 8px;
  background: var(--panel-2);
  box-shadow: 0 0 0 1px rgba(255, 255, 255, 0.08);
}
.imgcard-info {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.imgcard-name {
  font-size: 12.5px;
  font-weight: 600;
  color: var(--text, #e4e4e7);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.imgcard-size {
  font-size: 11px;
  opacity: 0.65;
}
.imgcard-open {
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--accent);
}
.imgcard:hover .imgcard-open {
  text-decoration: underline;
}
/* пункт «Save File»: иконка + текст в строку (стили .ctx-* общие с меню модерации) */
.media-save-item {
  display: flex;
  align-items: center;
  gap: 8px;
}
.ctx-item.ctx-danger {
  color: #f87171;
}
/* логотип группы: превью в списке чатов и в админке */
.r-logo {
  width: 34px;
  height: 34px;
  border-radius: 50%;
  object-fit: cover;
  display: block;
}
.logo-row {
  display: flex;
  align-items: center;
  gap: 10px;
}
.logo-prev {
  width: 44px;
  height: 44px;
  border-radius: 10px;
  object-fit: cover;
  background: var(--panel-2);
  box-shadow: 0 0 0 1px rgba(255, 255, 255, 0.1);
}
.logo-empty {
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 10px;
  opacity: 0.45;
}
.hidden-input {
  display: none;
}
.pill-btn {
  padding: 7px 14px;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  background: var(--accent);
  color: #04141a;
  border: none;
  border-radius: 4px;
  cursor: pointer;
}
.pill-btn:disabled {
  opacity: 0.5;
  cursor: default;
}
.pill-btn.danger {
  background: transparent;
  border: 1px solid rgba(248, 113, 113, 0.5);
  color: #f87171;
}
.img-pending {
  display: flex;
  gap: 10px;
  align-items: center;
  padding: 8px 10px;
  margin-bottom: 6px;
  background: var(--panel-2);
  border: 1px solid var(--border);
  border-radius: 8px;
}
.img-pending-thumb { width: 64px; height: 48px; object-fit: cover; border-radius: 6px; }
.img-pending-body { display: flex; flex-direction: column; gap: 6px; flex: 1; }
.img-caption-input {
  width: 100%;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 6px;
  color: inherit;
  font-size: 12px;
  padding: 6px 8px;
  outline: none;
}
.img-caption-input:focus { border-color: var(--accent); }
.img-caption {
  padding: 6px 4px 2px;
  font-size: var(--chat-text-size, 13px);
  line-height: 1.35;
  white-space: pre-wrap;
  word-break: break-word;
}
.aud-pending-icon { color: var(--accent); display: flex; align-items: center; }
/* ===== Аудио-плеер (Telegram-style) ===== */
.msg-aud {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 200px;
  max-width: 320px;
  padding: 4px 2px;
}
.file-clickable {
  cursor: pointer;
}
.file-clickable:hover .aud-name {
  text-decoration: underline;
}
.file-open-hint {
  color: #4ea1f3;
}
.msg-vid {
  width: min(340px, 68vw);
  border-radius: 10px;
  overflow: hidden;
  background: #0d1218;
}
.vid-el {
  display: block;
  width: 100%;
  max-height: 380px;
  background: #000;
}
.vid-poster {
  position: relative;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  width: 100%;
  aspect-ratio: 16 / 9;
  max-height: 420px;
  overflow: hidden;
  border: 0;
  background: linear-gradient(160deg, #16202b, #0d1218);
  color: #dbe4ee;
  cursor: pointer;
}
.vid-thumb {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  object-fit: cover;
}
.vid-shade {
  position: absolute;
  inset: 0;
  background: linear-gradient(180deg, rgba(0, 0, 0, 0.12), rgba(0, 0, 0, 0.48));
}
.vid-load,
.vid-play-ico,
.vid-caption {
  position: relative;
  z-index: 1;
}
.vid-play-ico {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 52px;
  height: 52px;
  border-radius: 50%;
  background: rgba(78, 161, 243, 0.22);
  color: #7cc0ff;
  transition: background-color 0.15s ease, transform 0.15s ease;
}
.vid-poster:hover .vid-play-ico {
  background: rgba(78, 161, 243, 0.35);
  transform: scale(1.06);
}
.vid-load {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  padding: 12px 18px 10px;
  border-radius: 14px;
  background: rgba(6, 12, 20, 0.72);
  backdrop-filter: blur(8px);
  -webkit-backdrop-filter: blur(8px);
  border: 1px solid rgba(124, 192, 255, 0.18);
}
.vid-load .aud-ring {
  width: 44px;
  height: 44px;
}
.vid-prog {
  font-size: 0.78rem;
  font-weight: 600;
  color: #e8f2fc;
  white-space: nowrap;
}
.vid-prog-hint {
  font-size: 0.66rem;
  color: rgba(232, 242, 252, 0.55);
  white-space: nowrap;
}
/* Фирменная плашка NETFORY VIDEO — пока не докачан 360px-постер.
   Фон на всю карточку; бренд-бейдж в левом верхнем углу, центр свободен для ▶. */
.vid-plate {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  justify-content: flex-start;
  gap: 4px;
  padding: 12px 14px;
  background:
    radial-gradient(120% 90% at 20% 0%, rgba(78, 161, 243, 0.22), transparent 60%),
    radial-gradient(100% 80% at 90% 100%, rgba(52, 211, 153, 0.14), transparent 60%),
    linear-gradient(160deg, #0b1626, #071019);
}
.vid-plate::after {
  content: '';
  position: absolute;
  inset: 0;
  background-image: repeating-linear-gradient(90deg, rgba(255, 255, 255, 0.025) 0 1px, transparent 1px 14px);
  pointer-events: none;
}
.vid-plate-ico {
  color: #7cc0ff;
  opacity: 0.85;
  width: 22px;
  height: 22px;
}
.vid-plate-brand {
  font-size: 0.66rem;
  letter-spacing: 0.22em;
  color: #9cc9f5;
  font-weight: 500;
}
.vid-plate-brand b {
  color: #e8f2fc;
  font-weight: 800;
}
.vid-plate-hint {
  font-size: 0.62rem;
  letter-spacing: 0.06em;
  color: rgba(156, 201, 245, 0.55);
}
.vid-poster:has(.vid-plate) .vid-play-ico {
  background: rgba(78, 161, 243, 0.32);
  box-shadow: 0 0 0 6px rgba(78, 161, 243, 0.1), 0 6px 20px rgba(0, 0, 0, 0.45);
}
.feed-more {
  display: flex;
  justify-content: center;
  padding: 6px 0 10px;
}
.feed-more-btn {
  padding: 5px 14px;
  font: inherit;
  font-size: 0.72rem;
  letter-spacing: 0.06em;
  color: var(--text-dim);
  background: rgba(255, 255, 255, 0.04);
  border: 1px solid var(--border);
  border-radius: 999px;
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease;
}
.feed-more-btn:hover:not(:disabled) {
  color: #67e8f9;
  border-color: rgba(34, 211, 238, 0.5);
}
.day-divider {
  display: flex;
  justify-content: center;
  margin: 14px 0 6px;
}
.day-divider span {
  padding: 3px 12px;
  border-radius: 12px;
  background: rgba(20, 28, 38, 0.75);
  color: #9fb4c9;
  font-size: 0.74rem;
}
.drop-overlay {
  position: fixed;
  inset: 0;
  z-index: 300;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(8, 12, 18, 0.6);
  backdrop-filter: blur(4px);
  pointer-events: none;
}
.drop-card {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 36px 56px;
  border: 2px dashed #4ea1f3;
  border-radius: 16px;
  background: rgba(20, 30, 42, 0.85);
  color: #cfe3f7;
  font-size: 1rem;
  font-weight: 600;
}
.drop-card svg {
  color: #4ea1f3;
}
.vid-caption {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 2px;
  max-width: 90%;
}
.vid-name {
  font-size: 0.8rem;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 100%;
}
.vid-sub {
  font-size: 0.72rem;
  color: #8fa5bb;
}
.aud-btn {
  width: 46px;
  height: 46px;
  min-width: 46px;
  border-radius: 50%;
  border: none;
  background: var(--accent);
  color: #0b0f14;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: transform 0.12s ease, filter 0.15s ease;
}
.aud-btn:hover { filter: brightness(1.12); }
.aud-btn:active { transform: scale(0.94); }
.aud-spin {
  width: 18px;
  height: 18px;
  border: 2px solid rgba(11, 15, 20, 0.3);
  border-top-color: #0b0f14;
  border-radius: 50%;
  animation: lb-spin 0.8s linear infinite;
}
.aud-load {
  position: relative;
  width: 34px;
  height: 34px;
  display: flex;
  align-items: center;
  justify-content: center;
}
.aud-ring {
  position: absolute;
  inset: 0;
  transform: rotate(-90deg);
}
.aud-ring.spin { animation: lb-spin 1.1s linear infinite; }
.aud-ring .ring-bg {
  fill: none;
  stroke: rgba(11, 15, 20, 0.25);
  stroke-width: 3;
}
.aud-ring .ring-fg {
  fill: none;
  stroke: #0b0f14;
  stroke-width: 3;
  stroke-linecap: round;
  stroke-dasharray: 97.4;
  transition: stroke-dashoffset 0.5s ease;
}
.aud-pct {
  font-size: 10px;
  font-weight: 700;
  font-family: 'JetBrains Mono', monospace;
  color: #0b0f14;
  line-height: 1;
}
.aud-info { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.aud-name {
  font-size: 13px;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 230px;
}
.aud-sub { font-size: 11px; color: #a1a1aa; font-family: 'JetBrains Mono', monospace; }
.aud-wave {
  display: flex;
  align-items: center;
  gap: 2px;
  height: 24px;
  width: 176px;
  cursor: pointer;
  padding: 2px 0;
}
.wv {
  flex: 1;
  min-height: 3px;
  border-radius: 2px;
  background: rgba(161, 161, 170, 0.45);
  transition: background-color 0.15s ease;
  pointer-events: none;
}
.wv.played { background: var(--accent); }
.aud-wave:hover .wv:not(.played) { background: rgba(161, 161, 170, 0.7); }
.aud-topbar {
  /* в потоке колонки чата (под шапкой, над лентой), а не поверх topbar'а */
  flex: none;
  background: var(--panel-2);
  border-bottom: 1px solid var(--border);
  backdrop-filter: blur(12px);
}
.aud-track {
  height: 4px;
  background: var(--border);
  cursor: pointer;
}
.aud-fill {
  height: 100%;
  background: var(--accent);
  transition: width 0.2s linear;
}
.aud-topbar-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 5px 10px;
}
.aud-tb-btn {
  width: 26px;
  height: 26px;
  min-width: 26px;
  border-radius: 50%;
  border: none;
  background: var(--accent);
  color: #0b0f14;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
}
.aud-tb-name {
  flex: 1;
  font-size: 12px;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.aud-tb-time { font-size: 11px; color: #a1a1aa; font-family: 'JetBrains Mono', monospace; }
.aud-tb-close {
  background: transparent;
  border: none;
  color: #a1a1aa;
  cursor: pointer;
  font-size: 13px;
  padding: 2px 6px;
}
.aud-tb-close:hover { color: #fff; }
/* ===== Голосовые сообщения ===== */
.mic {
  width: 38px;
  height: 38px;
  min-width: 38px;
  border-radius: 50%;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: #a1a1aa;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: color 0.15s ease, background-color 0.15s ease, border-color 0.15s ease;
}
.mic:hover { color: var(--accent); border-color: var(--accent); }
.mic.rec {
  background: #b91c1c;
  border-color: #b91c1c;
  color: #fff;
  animation: rec-pulse 1.2s ease-in-out infinite;
}
@keyframes rec-pulse {
  0%, 100% { box-shadow: 0 0 0 0 rgba(185, 28, 28, 0.45); }
  50% { box-shadow: 0 0 0 7px rgba(185, 28, 28, 0); }
}
.rec-strip { border-color: #b91c1c; }
.rec-dot {
  width: 10px;
  height: 10px;
  min-width: 10px;
  border-radius: 50%;
  background: #ef4444;
  animation: rec-blink 1s ease-in-out infinite;
}
@keyframes rec-blink {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.25; }
}
.rec-time { font-size: 13px; font-weight: 600; font-family: 'JetBrains Mono', monospace; }
.rec-hint { flex: 1; font-size: 11px; color: #a1a1aa; }
.img-pending-info { display: flex; align-items: center; flex: 1; justify-content: space-between; gap: 12px; }
.img-pending-dims { font-size: 12px; color: #a1a1aa; font-family: 'JetBrains Mono', monospace; }
.img-pending-actions { display: flex; gap: 6px; }
.img-send-btn {
  background: var(--accent);
  color: #0b0f14;
  border: none;
  padding: 6px 14px;
  border-radius: 6px;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: opacity 0.15s ease;
}
.img-send-btn:hover { opacity: 0.85; }
.img-send-btn:disabled { opacity: 0.5; cursor: default; }
.img-cancel-btn {
  background: transparent;
  color: #a1a1aa;
  border: 1px solid var(--border);
  padding: 6px 10px;
  border-radius: 6px;
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease;
}
.img-cancel-btn:hover { color: #fff; border-color: #b91c1c; }
.img-lightbox {
  position: fixed;
  inset: 0;
  z-index: 300;
  background: rgba(4, 6, 10, 0.94);
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: zoom-out;
  animation: lb-fade 0.15s ease;
}
@keyframes lb-fade { from { opacity: 0; } to { opacity: 1; } }
.lb-img { max-width: 100vw; max-height: 100vh; object-fit: contain; }
.lb-state { color: #a1a1aa; font-size: 13px; display: flex; align-items: center; gap: 10px; }
.lb-spinner {
  width: 18px;
  height: 18px;
  border: 2px solid var(--border);
  border-top-color: var(--accent);
  border-radius: 50%;
  animation: lb-spin 0.8s linear infinite;
}
@keyframes lb-spin { to { transform: rotate(360deg); } }
.myc-badge { display: inline-flex; align-items: center; gap: 5px; margin-left: 8px; padding: 1px 7px; font-size: 9px; letter-spacing: 0.14em; text-transform: uppercase; color: #2ee6a6; border: 1px solid rgba(46, 230, 166, 0.5); vertical-align: middle; }
.myc-badge .myc-dot { width: 5px; height: 5px; border-radius: 50%; background: #2ee6a6; box-shadow: 0 0 6px #2ee6a6; }
.myc-badge { background: transparent; font-family: inherit; cursor: pointer; transition: box-shadow 0.16s ease; }
.myc-badge:hover { box-shadow: 0 0 10px rgba(46, 230, 166, 0.35); }
.circ-card { width: min(720px, 94vw); max-width: none; border-color: #2ee6a6; box-shadow: 0 0 24px rgba(46, 230, 166, 0.25); }
.circ-card .wipe-title { color: #2ee6a6; }
.circ-none { font-size: 12px; color: var(--text-dim); margin: 12px 0; }
.circ-err { color: #ff5d6c; }
.circ-note { font-size: 11px; color: var(--text-dim); margin: 12px 0 0; line-height: 1.45; }
.circ-close { margin-top: 14px; padding: 6px 16px; background: transparent; border: 1px solid var(--border); color: var(--text); cursor: pointer; font-size: 11px; letter-spacing: 0.12em; transition: border-color 0.16s ease; }
.circ-close:hover { border-color: #2ee6a6; }
.myc-badge.wait { color: #f5c451; border-color: rgba(245, 196, 81, 0.5); }
.myc-badge.wait .myc-dot { background: #f5c451; box-shadow: none; }
.call-myc { font-size: 10px; color: var(--text-dim); margin-right: 8px; }
</style>
