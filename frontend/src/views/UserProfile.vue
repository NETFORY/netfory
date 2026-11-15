<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import QRCode from 'qrcode'
import { useAuthStore } from '@/store/auth'
import { useWalletStore } from '@/store/wallet'
import { useAppsStore } from '@/store/apps'
import { useTabsStore } from '@/store/tabs'
import { loadProfileManifest, saveProfileConfig, appLogo, saveFile, listMyDomains, domainsByOwner, renewDomain, renewalPrice, domainDurationDays, type ProfileManifest, type Manifest, type DomainRecord } from '@/lib/bridge'
import { shortAddr } from '@/lib/crypto'
import Identicon from '@/components/Identicon.vue'
import UserAvatar from '@/components/UserAvatar.vue'
import { useUsersStore } from '@/store/users'
import IdentityRegistration from '@/components/IdentityRegistration.vue'
import TxConfirmModal from '@/components/TxConfirmModal.vue'
import WallFeed from '@/components/WallFeed.vue'
import WallPlaylist from '@/components/WallPlaylist.vue'
import AlbumCard from '@/components/AlbumCard.vue'
import MusicStudioModal from '@/components/MusicStudioModal.vue'
import { albumList, albumFeed, type Album } from '@/lib/bridge'
import { getMyUsername, recoverMyUsername, resyncRegistry, usernameOfAddress, followStats, followStatsCached, subscribe as apiSubscribe, getSubscribePrice, toggleChatWindow, donorTier, pickImageFile, setProfileAvatar, getProfileAvatar, authorAvatar, fetchWallMedia, wallUnreadCounts, onSocialEvent, onSocialGraphUpdated, getProfileStats, getActiveTorrents, getReputation, dislikeUser, type FollowStats, type FollowEntry, type ProfileStats, type TorrentStatus } from '@/lib/bridge'

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const props = defineProps<{
  // When rendered inside a dApp-style tab (u://), the identity is passed directly
  // and the profile is always shown in read-only VISITOR mode.
  identityOverride?: string
  forceVisitor?: boolean
}>()
const auth = useAuthStore()
const wallet = useWalletStore()
const apps = useAppsStore()
const tabs = useTabsStore()

const manifest = ref<ProfileManifest | null>(null)
const saving = ref(false)
const flash = ref('')
const rewardEdit = ref('')
const qrDataUrl = ref('')
const logos = ref<Record<string, string>>({})
const myUsername = ref<string | null>(null)
const viewedUsername = ref<string | null>(null)
const showRegister = ref(false)
const username = computed(() => viewedUsername.value)

/* ---- subscriptions (follow) ---- */
const emptyStats: FollowStats = { subscribers: 0, following: 0, recentSubscribers: [], followingList: [] }
const stats = ref<FollowStats>(emptyStats)
const myFollowing = ref<FollowEntry[]>([])
const subPrice = ref(1)
const subscribing = ref(false)
const subConfirm = ref(false)
// Right-column tabs: Wall (social graph + microblog) | DAPPs (showcase + stats) | Torrent
const rightTab = ref<'wall' | 'dapps' | 'torrent' | 'music'>('wall')
const profileAlbums = ref<Album[]>([])
const acquiredAlbums = ref<Album[]>([])
const showStudio = ref(false)
const editAlbum = ref<Album | null>(null)
async function loadAlbums() {
  profileAlbums.value = await albumList(wallAddr.value)
  // «Альбомы других авторов» — только на своём профиле: все локально известные
  // альбомы, купленные/добавленные у ДРУГИХ авторов (owner !== я).
  if (isOwn.value) {
    const feed = await albumFeed()
    acquiredAlbums.value = feed.filter((a) => a.owner !== wallAddr.value)
  } else {
    acquiredAlbums.value = []
  }
}
function onAlbumCreated(a: Album) {
  profileAlbums.value = [a, ...profileAlbums.value.filter((x) => x.id !== a.id)]
  showStudio.value = false
  editAlbum.value = null
}
function onEditAlbum(a: Album) {
  editAlbum.value = a
  showStudio.value = true
}
function onAlbumDeleted(id: string) {
  profileAlbums.value = profileAlbums.value.filter((x) => x.id !== id)
}
function closeStudio() {
  showStudio.value = false
  editAlbum.value = null
}
watch(rightTab, (tab) => {
  if (tab === 'music') loadAlbums()
  if (tab === 'torrent') loadTorrentStats()
})

/* ---- Torrent tab: seeding + network-contribution stats (own node only) ---- */
const torrentStats = ref<ProfileStats>({ byteHours: 0, rank: 'Leech', uploadedBytes: 0, downloadedBytes: 0, nextRankAt: 1, progressToNext: 0 })
const activeTorrents = ref<TorrentStatus[]>([])
let torrentTimer: ReturnType<typeof setInterval> | null = null
const RANK_META: Record<string, { label: string; color: string }> = {
  Leech: { label: 'LEECH', color: '#64748b' },
  Peer: { label: 'PEER', color: '#38bdf8' },
  Seeder: { label: 'SEEDER', color: '#34d399' },
  SuperSeed: { label: 'SUPERSEED', color: '#a855f7' },
  Anchor: { label: 'ANCHOR', color: '#f59e0b' },
}
const rankMeta = computed(() => RANK_META[torrentStats.value.rank] || RANK_META.Leech)
const byteHoursLabel = computed(() => {
  const v = torrentStats.value.byteHours
  if (v >= 1000) return (v / 1000).toFixed(2) + 'K'
  if (v >= 1) return v.toFixed(2)
  return v.toFixed(3)
})
const rankProgressPct = computed(() => Math.round((torrentStats.value.progressToNext || 0) * 100))
const seedingCount = computed(() => activeTorrents.value.filter((t) => t.progress >= 0.999).length)
const upSpeedTotal = computed(() => activeTorrents.value.reduce((s, t) => s + (t.uploadSpeedBytes || 0), 0))
const downSpeedTotal = computed(() => activeTorrents.value.reduce((s, t) => s + (t.downloadSpeedBytes || 0), 0))
function fmtBytes(n: number): string {
  if (!n) return '0 B'
  const u = ['B', 'KB', 'MB', 'GB', 'TB']
  let i = 0
  let v = n
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++ }
  return `${v.toFixed(v >= 100 || i === 0 ? 0 : 1)} ${u[i]}`
}
async function loadTorrentStats() {
  if (!isOwn.value) return
  torrentStats.value = await getProfileStats()
  activeTorrents.value = await getActiveTorrents()
  if (!torrentTimer) {
    torrentTimer = setInterval(async () => {
      if (rightTab.value !== 'torrent') return
      torrentStats.value = await getProfileStats()
      activeTorrents.value = await getActiveTorrents()
    }, 4000)
  }
}
// Wall author address (wallet address of the viewed profile).
const wallAddr = computed(() => (isOwn.value ? auth.address || '' : identity.value || ''))
const amIFollowing = computed(
  () => !!viewedUsername.value && myFollowing.value.some((e) => e.name === viewedUsername.value),
)
const canSubscribe = computed(
  () => !isOwn.value && !!viewedUsername.value && !!myUsername.value && !amIFollowing.value,
)
// Nameless viewer looking at a named profile: must register a name before they
// can subscribe (keeps the social graph free of raw burner-wallet addresses).
const needsName = computed(
  () => !isOwn.value && !!viewedUsername.value && !myUsername.value && !amIFollowing.value,
)
const headerTitle = computed(() => {
  if (viewedUsername.value) return t('profile.titleNamed', { name: viewedUsername.value })
  return isOwn.value ? t('profile.citizen') : t('profile.citizenForeign')
})

async function loadSocial() {
  const id = identity.value
  // Мгновенно: последний успешный снимок из кэша (профиль не ждёт сеть).
  const cached = await followStatsCached(id)
  if (cached && identity.value === id) {
    stats.value = cached
    if (isOwn.value) myFollowing.value = cached.followingList
  }
  if (!isOwn.value && auth.address) {
    const mc = await followStatsCached(auth.address)
    if (mc) myFollowing.value = mc.followingList
  }
  loadReputation()
  // Тихое фоновое обновление с блокчейна (подменяет цифры без мерцания).
  const fresh = await followStats(id)
  if (identity.value !== id) return
  stats.value = fresh
  if (isOwn.value) {
    myFollowing.value = fresh.followingList
  } else if (auth.address) {
    myFollowing.value = (await followStats(auth.address)).followingList
  }
}

/* ---- Reputation + dislike ---- */
const reputation = ref<number>(0)
const dislikeConfirm = ref(false)
const disliking = ref(false)
async function loadReputation() {
  if (!wallAddr.value) { reputation.value = 0; return }
  reputation.value = await getReputation(wallAddr.value)
}
async function confirmDislike() {
  if (disliking.value) return
  disliking.value = true
  try {
    await dislikeUser(identity.value)
    dislikeConfirm.value = false
    notify(t('profile.dislikeOk'))
    setTimeout(loadReputation, 2500)
  } catch (e: any) {
    notify(typeof e === 'string' ? e : e?.message || t('profile.dislikeError'))
    dislikeConfirm.value = false
  } finally {
    disliking.value = false
  }
}

async function doSubscribe() {
  if (!canSubscribe.value || subscribing.value) return
  // explicit confirmation before broadcasting the follow payment
  subConfirm.value = true
}

async function confirmSubscribe() {
  if (!canSubscribe.value || subscribing.value) return
  subscribing.value = true
  try {
    await apiSubscribe(viewedUsername.value as string)
    subConfirm.value = false
    notify(t('profile.subscribedOk'))
    // optimistic: reflect the follow immediately; chain confirms shortly after
    myFollowing.value = [...myFollowing.value, { name: viewedUsername.value, address: identity.value }]
    setTimeout(loadSocial, 2500)
  } catch (e: any) {
    notify(typeof e === 'string' ? e : e?.message || t('profile.subscribeError'))
    subConfirm.value = false
  } finally {
    subscribing.value = false
  }
}

function openEntry(e: FollowEntry) {
  const target = e.name || e.address || ''
  if (!target) return
  graphModal.value = null
  if (props.identityOverride) {
    // Профиль открыт внутри u://-вкладки: router.push менял бы скрытый за
    // вкладкой route и "ничего не происходило" — открываем через адресную
    // строку оболочки (новая u://-вкладка).
    window.dispatchEvent(new CustomEvent('netfory:open-address', { detail: `u://${target}` }))
    return
  }
  router.push({ name: 'profile-view', params: { identity: e.address || e.name || '' } })
}

// Pipe this identity into the native P2P messenger (spawns the chat window if
// closed, then auto-focuses the dialogue). Prefer u://name over the raw address.
function messageUser() {
  const target = viewedUsername.value ? `u://${viewedUsername.value}` : identity.value
  if (target) toggleChatWindow(target)
}

const regPending = ref(false)
async function onRegistered() {
  // Show a "registration in progress" state until the on-chain indexer confirms
  // the name (poll up to 3 min, then fall back to the register CTA).
  showRegister.value = false
  regPending.value = true
  const start = Date.now()
  const poll = async () => {
    const u = await getMyUsername()
    if (u) {
      myUsername.value = u
      viewedUsername.value = u
      regPending.value = false
      loadSocial()
      return
    }
    if (Date.now() - start > 180_000) {
      regPending.value = false
      return
    }
    setTimeout(poll, 10_000)
  }
  setTimeout(poll, 3_000)
}

// When opened via u://<identity> (route param), render that person's PUBLIC
// profile read-only; otherwise it's the logged-in user's own editable profile.
const routeIdentity = computed(
  () => (props.identityOverride || (route.params.identity as string | undefined))?.trim() || '',
)
const isOwn = computed(
  () => !props.forceVisitor && (!routeIdentity.value || routeIdentity.value === auth.address),
)
const identity = computed(() =>
  isOwn.value ? manifest.value?.public.identity || auth.address || '' : routeIdentity.value,
)
// u:// is the USER/developer protocol (sth:// is reserved for dApps). Prefer the
// human-readable username over the raw address once a name is registered.
const shareLink = computed(() => {
  if (viewedUsername.value) return `u://${viewedUsername.value}`
  return identity.value ? `u://${identity.value}` : ''
})
const isPublic = computed(() => manifest.value?.public.isPublic ?? false)
const publishedApps = computed<Manifest[]>(() => {
  if (isOwn.value) return (apps as any).myPublished || []
  // best-effort: surface any locally-known apps owned by this identity.
  return apps.apps.filter(
    (m: any) => m.status === 'published' && (m.owner === routeIdentity.value),
  )
})
// total published count drives whether the contribution/showcase blocks render
const publishedCount = computed(() =>
  isOwn.value
    ? (manifest.value?.statsCache?.appsPublished ?? 0)
    : publishedApps.value.length,
)
// DON donor medal (Steam-style, 3 tiers: bronze/silver/gold)
const medalTier = ref(0)
const medalCount = ref(0)
const medalTotal = ref('0')
async function loadMedals() {
  const addr = isOwn.value ? auth.address || identity.value : identity.value
  if (!addr) {
    medalTier.value = 0
    return
  }
  const r = await donorTier(addr)
  medalTier.value = r.tier
  medalCount.value = r.count
  medalTotal.value = r.totalSth
}
const medalRoman = computed(() => ['', 'I', 'II', 'III'][medalTier.value] || '')
const medalClass = computed(() => ['', 'bronze', 'silver', 'gold'][medalTier.value] || '')
// QR is hidden behind an icon → modal (keeps the sidebar clean)
const showQr = ref(false)
const graphModal = ref<null | 'subscribers' | 'following'>(null)

function notify(msg: string) {
  flash.value = msg
  setTimeout(() => (flash.value = ''), 1800)
}

async function load() {
  manifest.value = await loadProfileManifest()
  rewardEdit.value = manifest.value?.public.rewardAddress || auth.address || ''
  await loadDomains()
  // Стоимость продления не меняется в рамках сессии, тянем один раз.
  try { renewPrice.value = await renewalPrice() } catch { /* keep default */ }
}

const showDomains = computed(() => manifest.value?.public.showDomains ?? false)
const myDomains = ref<DomainRecord[]>([])
// Домены-секция:
//   • на моём профиле — всегда видна (даже если пусто → CTA «Купить домен»);
//   • у визитёра — только если у автора реально есть домены в реестре.
// `showDomains` (privacy-тумблер) применяется ТОЛЬКО к своему профилю — на
// чужом профиле мы не читаем чужой manifest, поэтому просто показываем
// то, что вернул on-chain `domainsByOwner(visited)`. Стрелять чужим тумблером
// из своего manifest — было багом (мои домены светились на u://europa).
const showDomainSection = computed(
  () => isOwn.value || myDomains.value.length > 0,
)

async function loadDomains() {
  if (isOwn.value) {
    myDomains.value = await listMyDomains()
  } else if (identity.value) {
    // Публичная on-chain запись — доступна всем; вернёт домены посещаемого
    // пользователя, а не мои. Именно так «мои домены» больше не лезут в
    // чужой профиль при переходе с /profile → /profile/:identity.
    myDomains.value = await domainsByOwner(identity.value)
  } else {
    myDomains.value = []
  }
}

async function toggleShowDomains() {
  saving.value = true
  try {
    const next = await saveProfileConfig(rewardEdit.value, isPublic.value, !showDomains.value)
    if (next) manifest.value = next
    notify(t('profile.saved'))
  } finally {
    saving.value = false
  }
}

/** Open the dApp bound to a domain in a new app tab. */
function openDomain(d: DomainRecord) {
  if (!d.appId) return
  tabs.open(d.appId, `${d.name}.${d.zone}`)
}

/* ---- domain expiration & renewal ---- */
// Стоимость продления (сеть возвращает 250 STH дефолтом). Загружается один
// раз при монтировании — используется и для gating кнопки по балансу, и в
// подписи «Продлить · N STH».
const renewPrice = ref(250)
// Инлайн-разворачиваемая форма подтверждения (переключается по клику на
// «Продлить»; повторный клик закрывает).
const renewOpen = ref('')
const renewBusy = ref(false)
const balanceSth = computed(() => Number(wallet.mainBalance) || 0)
const domDurationDays = domainDurationDays()

/** Whole days remaining until `rec.expires`. Negative means grace-period. */
function daysLeft(rec: DomainRecord): number {
  return Math.ceil((rec.expires * 1000 - Date.now()) / 86400000)
}
/** Renewable window matches the backend (`dns_manager::renew_domain`):
 *  30 days before expiry and up to 30 days grace after. */
function isRenewable(rec: DomainRecord): boolean {
  const d = daysLeft(rec)
  return d <= 30 && d >= -30
}
/** Localized `DD.MM.YYYY` for the expiration date. */
function fmtExpDate(unix: number): string {
  const d = new Date(unix * 1000)
  return d.toLocaleDateString()
}
/** Color class for the compact "· X d." expiration indicator. */
function expTone(rec: DomainRecord): string {
  const d = daysLeft(rec)
  if (d <= 7) return 'danger'
  if (d <= 30) return 'warn'
  return ''
}

async function doRenew(rec: DomainRecord) {
  if (renewBusy.value || balanceSth.value < renewPrice.value) return
  renewBusy.value = true
  try {
    await renewDomain(`${rec.name}.${rec.zone}`, rec.appId)
    notify(t('nodePanel.domRenewOk', { name: `${rec.name}.${rec.zone}` }))
    renewOpen.value = ''
    await loadDomains()
  } catch (e: any) {
    notify(typeof e === 'string' ? e : e?.message || 'error')
  } finally {
    renewBusy.value = false
  }
}

async function togglePrivacy() {
  saving.value = true
  try {
    const next = await saveProfileConfig(rewardEdit.value, !isPublic.value, showDomains.value)
    if (next) manifest.value = next
    notify(t('profile.saved'))
  } finally {
    saving.value = false
  }
}

async function saveReward() {
  saving.value = true
  try {
    const next = await saveProfileConfig(rewardEdit.value, isPublic.value, showDomains.value)
    if (next) manifest.value = next
    notify(t('profile.saved'))
  } finally {
    saving.value = false
  }
}

async function copy(text: string, label: string) {
  if (!text) return
  try {
    await navigator.clipboard.writeText(text)
    notify(label)
  } catch {
    /* clipboard blocked — ignore */
  }
}

async function exportKeypair() {
  const json = await auth.exportVaultJson()
  if (!json) return
  const saved = await saveFile(
    `smartnet-keypair-${(identity.value || 'vault').slice(0, 8)}.json`,
    json,
    'application/json',
  )
  if (saved) notify(t('profile.exported'))
}

/** Свежая установка: sled пуст, но имя может быть в цепи — быстрый чек через iroh. */
async function recoverOwnName() {
  const n = await recoverMyUsername().catch(() => null)
  if (!n || myUsername.value) return
  myUsername.value = n
  if (isOwn.value) viewedUsername.value = n
  loadSocial()
}

const resyncing = ref(false)
async function resyncRegistryNow() {
  if (resyncing.value) return
  resyncing.value = true
  try {
    const r = await resyncRegistry()
    myUsername.value = r.name ?? (await getMyUsername())
    if (isOwn.value) viewedUsername.value = myUsername.value
    await loadSocial()
    window.dispatchEvent(new CustomEvent('name-confirmed', { detail: myUsername.value }))
    notify(t('profile.resyncDone', { n: r.subscribers }))
  } catch {
    notify(t('profile.resyncFailed'))
  } finally {
    resyncing.value = false
  }
}

function launchApp(m: Manifest) {
  tabs.open(m.id, m.name)
}

// FNV-1a (matches Identicon.vue) so the shared card avatar equals the on-screen one.
function fnv(str: string): number {
  let h = 0x811c9dc5
  for (let i = 0; i < str.length; i++) {
    h ^= str.charCodeAt(i)
    h = Math.imul(h, 0x01000193)
  }
  return h >>> 0
}

function drawIdenticon(ctx: CanvasRenderingContext2D, seed: string, x: number, y: number, size: number) {
  const u = size / 5
  const hue = fnv(seed) % 360
  const hue2 = (hue + 140 + (fnv(seed + '::accent') % 80)) % 360
  const fg = `hsl(${hue},95%,62%)`
  const fg2 = `hsl(${hue2},95%,60%)`
  ctx.fillStyle = '#0b0e13'
  ctx.fillRect(x, y, size, size)
  for (let col = 0; col < 3; col++) {
    for (let row = 0; row < 5; row++) {
      const bit = fnv(`${seed}:${col}:${row}`)
      if (bit % 2 === 0) {
        ctx.fillStyle = bit % 3 === 0 ? fg2 : fg
        ctx.fillRect(x + col * u, y + row * u, u, u)
        if (col < 2) ctx.fillRect(x + (4 - col) * u, y + row * u, u, u)
      }
    }
  }
}

function loadImg(src: string): Promise<HTMLImageElement> {
  return new Promise((res, rej) => {
    const im = new Image()
    im.onload = () => res(im)
    im.onerror = rej
    im.src = src
  })
}

async function buildShareCard(): Promise<Blob | null> {
  const canvas = document.createElement('canvas')
  canvas.width = 1200
  canvas.height = 630
  const ctx = canvas.getContext('2d')
  if (!ctx) return null
  // background + subtle grid
  ctx.fillStyle = '#0b0e13'
  ctx.fillRect(0, 0, 1200, 630)
  ctx.strokeStyle = 'rgba(0,229,255,0.05)'
  ctx.lineWidth = 1
  for (let gx = 0; gx <= 1200; gx += 40) {
    ctx.beginPath(); ctx.moveTo(gx, 0); ctx.lineTo(gx, 630); ctx.stroke()
  }
  for (let gy = 0; gy <= 630; gy += 40) {
    ctx.beginPath(); ctx.moveTo(0, gy); ctx.lineTo(1200, gy); ctx.stroke()
  }
  ctx.strokeStyle = 'rgba(0,229,255,0.4)'
  ctx.lineWidth = 2
  ctx.strokeRect(24, 24, 1152, 582)

  // avatar
  ctx.save()
  ctx.shadowColor = 'rgba(0,229,255,0.5)'
  ctx.shadowBlur = 30
  drawIdenticon(ctx, identity.value || 'sth', 72, 80, 180)
  ctx.restore()

  // headings
  ctx.fillStyle = '#e8fbff'
  ctx.font = 'bold 50px monospace'
  ctx.fillText('SmartNet', 296, 130)
  ctx.fillStyle = '#00E5FF'
  ctx.font = '600 24px monospace'
  ctx.fillText('DEVELOPER PROFILE', 298, 168)
  ctx.fillStyle = '#9fb2c0'
  ctx.font = '22px monospace'
  ctx.fillText(`u://${shortAddr(identity.value)}`, 298, 212)

  // stats line
  ctx.fillStyle = '#cbd5e1'
  ctx.font = '600 22px monospace'
  ctx.fillText(
    `${manifest.value?.statsCache.appsPublished ?? 0} published   ·   ${manifest.value?.statsCache.appsSeeded ?? 0} seeded`,
    72,
    320,
  )

  // top dApps
  ctx.fillStyle = '#00E5FF'
  ctx.font = '600 20px monospace'
  ctx.fillText('TOP dAPPS', 72, 372)
  ctx.fillStyle = '#e8fbff'
  ctx.font = '24px monospace'
  publishedApps.value.slice(0, 4).forEach((m, i) => {
    const label = (m.name || m.id).slice(0, 28)
    ctx.fillText(`▸ ${label}`, 72, 412 + i * 40)
  })

  // QR bottom-right on a light plate
  if (qrDataUrl.value) {
    try {
      const qr = await loadImg(qrDataUrl.value)
      ctx.fillStyle = '#e8fbff'
      ctx.fillRect(872, 332, 264, 248)
      ctx.drawImage(qr, 892, 340, 224, 224)
      ctx.fillStyle = '#04121a'
      ctx.font = '600 16px monospace'
      ctx.fillText('SCAN TO VISIT', 924, 576)
    } catch {
      /* qr failed — skip */
    }
  }

  return await new Promise((res) => canvas.toBlob((b) => res(b), 'image/png'))
}

async function shareProfile() {
  const blob = await buildShareCard()
  if (!blob) return
  const bytes = new Uint8Array(await blob.arrayBuffer())
  const fname = `smartnet-${(identity.value || 'profile').slice(0, 8)}.png`
  // Browser/mobile with the Web Share sheet → share directly (NOT in Tauri).
  const nav = navigator as any
  if (!isTauriShare() && nav.canShare) {
    const file = new File([bytes], fname, { type: 'image/png' })
    if (nav.canShare({ files: [file] })) {
      try {
        await nav.share({ files: [file], title: 'SmartNet', text: shareLink.value })
        return
      } catch {
        /* cancelled — fall through to save */
      }
    }
  }
  // Tauri: native "Save As" dialog; browser: download.
  const saved = await saveFile(fname, bytes, 'image/png')
  if (saved) notify(t('profile.cardSaved'))
}

function isTauriShare(): boolean {
  return !!(window as any).__TAURI__
}

async function renderQr() {
  if (!shareLink.value) {
    qrDataUrl.value = ''
    return
  }
  try {
    qrDataUrl.value = await QRCode.toDataURL(shareLink.value, {
      errorCorrectionLevel: 'M',
      margin: 2,
      width: 320,
      color: { dark: '#04121a', light: '#e8fbff' },
    })
  } catch {
    qrDataUrl.value = ''
  }
}

async function loadLogos() {
  for (const m of publishedApps.value) {
    if (logos.value[m.id] === undefined) {
      const url = await appLogo(m.id)
      if (url) logos.value[m.id] = url
    }
  }
}

watch(shareLink, renderQr)
watch(publishedApps, loadLogos, { deep: false })
// New incoming `sub:` transfer to me → refresh my live subscriber count at once.
watch(
  () => wallet.transactions.filter((t) => t.type === 'in' && t.memo?.startsWith('sub:')).length,
  (n, prev) => {
    if (isOwn.value && n !== prev) loadSocial()
  },
)
watch(identity, async () => {
  // Смена identity (переход с /profile → /profile/:visitor или наоборот) —
  // сбрасываем состояние, специфичное для владельца, чтобы данные с прошлой
  // страницы не мигали визитёру. Ранее `myDomains` оставался от ownera → мои
  // домены светились на чужом профиле, пока `loadDomains` не звался повторно.
  myDomains.value = []
  // Аватар — ПЕРВЫМ и без await: первые строки loadAvatar синхронно ставят
  // локальный кэш users store или дефолт. Раньше он обновлялся ПОСЛЕ цепочки
  // await (getMyUsername/loadSocial — секунды сети), и на чужой странице
  // несколько секунд висел аватар предыдущего профиля.
  loadAvatar()
  myUsername.value = await getMyUsername()
  viewedUsername.value = isOwn.value ? myUsername.value : await usernameOfAddress(identity.value)
  await loadDomains()
  await loadSocial()
  loadMedals()
})

onMounted(async () => {
  // Аватар грузим первым и НЕ ждём — он локальный, должен появляться мгновенно,
  // а не после тяжёлой цепочки (loadSocial/кошелёк/сеть).
  loadAvatar()
  await load()
  await apps.init()
  await renderQr()
  await loadLogos()
  myUsername.value = await getMyUsername()
  viewedUsername.value = isOwn.value ? myUsername.value : await usernameOfAddress(identity.value)
  if (!myUsername.value) recoverOwnName()
  subPrice.value = await getSubscribePrice()
  await loadSocial()
  wallet.refresh().catch(() => {})
  loadMedals()
  // визитёр: CID аватара автора может подтянуться из снапшота/постов чуть позже
  if (!isOwn.value && !avatarUrl.value) {
    setTimeout(() => loadAvatar(), 3000)
  }
})

// --- Profile avatar (own profile: click the avatar → pick → 128×128 → seed) ---
const avatarUrl = ref<string | null>(null)
const avatarBusy = ref(false)

// Load the shown avatar: own → local file; visitor → author's avatar over P2P.
async function loadAvatar() {
  const target = identity.value
  if (isOwn.value) {
    avatarUrl.value = await getProfileAvatar()
    return
  }
  // мгновенно (синхронно, до всех await): кэш из users store или дефолт —
  // чтобы при переходе между профилями не мигал аватар предыдущего пользователя
  const users = useUsersStore()
  avatarUrl.value = users.avatars[target] || null
  users.ensure(target)
  const info = await authorAvatar(target)
  if (identity.value !== target) return // уже ушли на другой профиль
  const url = info?.cid ? await fetchWallMedia(info.cid, info.node) : null
  if (identity.value !== target) return
  if (url) avatarUrl.value = url
  else if (!users.avatars[target]) avatarUrl.value = null
}

async function changeAvatar() {
  if (!isOwn.value || avatarBusy.value) return
  const path = await pickImageFile()
  if (!path) return
  avatarBusy.value = true
  try {
    avatarUrl.value = await setProfileAvatar(path)
    // синхронизировать аватар в топбаре (у кнопки LOCK)
    window.dispatchEvent(new Event('avatar-updated'))
  } catch (e: any) {
    avatarError.value = String(e?.message || e)
    setTimeout(() => (avatarError.value = ''), 5000)
  } finally {
    avatarBusy.value = false
  }
}
const avatarError = ref('')

// --- Unread badge (followed walls) ---
const wallUnreadTotal = ref(0)
let unreadUnlisten: (() => void) | null = null
async function refreshUnread() {
  const m = await wallUnreadCounts()
  wallUnreadTotal.value = Object.values(m).reduce((a, b) => a + b, 0)
}
let graphUnlisten: (() => void) | null = null
onMounted(async () => {
  await refreshUnread()
  unreadUnlisten = await onSocialEvent(() => refreshUnread())
  graphUnlisten = await onSocialGraphUpdated((e) => {
    if (e.address === identity.value || e.address === auth.address) void loadSocial()
  })
  window.addEventListener('wall-read', refreshUnread)
})
onUnmounted(() => {
  unreadUnlisten?.()
  graphUnlisten?.()
  window.removeEventListener('wall-read', refreshUnread)
  if (torrentTimer) clearInterval(torrentTimer)
})
</script>

<template>
  <div class="profile-view" data-testid="user-profile">
    <div class="grid" :class="{ 'grid-wall': rightTab === 'wall' }">
      <!-- LEFT: identity + privacy + share + balance -->
      <section class="panel id-col" data-testid="profile-identity">
        <!-- Столбец 1: аватар → адрес → имя → репутация → лайки -->
        <div class="id-sect id-c1">
          <div
            class="avatar-wrap"
            :class="{ editable: isOwn }"
            @click="changeAvatar"
            :title="isOwn ? t('profile.changeAvatar') : ''"
            data-testid="profile-avatar"
          >
            <img
              v-if="avatarUrl"
              :src="avatarUrl"
              class="avatar-img"
              alt="avatar"
              data-testid="profile-avatar-img"
            />
            <Identicon v-else :seed="identity" :size="120" />
            <span v-if="isOwn" class="avatar-edit-badge" data-testid="profile-avatar-edit">
              {{ avatarBusy ? '…' : '✎' }}
            </span>
            <span class="status-dot" :class="{ live: isPublic }" />
          </div>
          <p v-if="avatarError" class="avatar-error" data-testid="profile-avatar-error">{{ avatarError }}</p>
          <code
            class="addr addr-clickable"
            data-testid="profile-address"
            :title="`${identity} — ${t('profile.clickToCopy')}`"
            role="button"
            tabindex="0"
            @click="copy(identity, t('profile.addressCopied'))"
            @keydown.enter.prevent="copy(identity, t('profile.addressCopied'))"
            @keydown.space.prevent="copy(identity, t('profile.addressCopied'))"
          >{{ shortAddr(identity) }}</code>
          <span v-if="username" class="username-label" data-testid="profile-username">@{{ username }}</span>

          <button
            v-if="!isOwn"
            class="msg-btn"
            @click="messageUser"
            data-testid="profile-message-btn"
          >
            <span class="msg-btn-icon">▰▰</span>{{ t('chat.sendMessage') }}
          </button>

          <div v-if="canSubscribe" class="sub-cta" data-testid="profile-subscribe-cta">
            <button
              class="btn btn-primary sub-btn"
              :disabled="subscribing"
              @click="doSubscribe"
              data-testid="profile-subscribe-btn"
            >
              {{ subscribing ? t('profile.subscribing') : `${t('profile.subscribe')} (${subPrice} STH)` }}
            </button>
            <span class="sub-note dim">{{ t('profile.subscribeNote', { amount: subPrice }) }}</span>
          </div>
          <div v-else-if="!isOwn && amIFollowing" class="subscribed-badge" data-testid="profile-subscribed-badge">
            ✓ {{ t('profile.subscribed') }}
          </div>
          <div v-else-if="needsName" class="sub-cta" data-testid="profile-needname-cta">
            <button
              class="btn btn-primary sub-btn"
              @click="showRegister = true"
              data-testid="profile-needname-btn"
            >
              ⬡ {{ t('profile.needName') }}
            </button>
            <span class="sub-note dim">{{ t('profile.needNameNote') }}</span>
          </div>

          <span v-if="viewedUsername" class="reputation-badge" :title="t('profile.reputationHint')" data-testid="profile-reputation">
            ⚡ {{ t('profile.reputation') }}: <b>{{ reputation }}</b>
          </span>

          <div v-if="viewedUsername" class="social-counts" data-testid="profile-social-counts">
            <span class="sc-pill heart" :title="t('profile.subscribers')">❤ {{ stats.subscribers }}</span>
            <span class="sc-pill like" :title="t('profile.following')">👍 {{ stats.following }}</span>
            <button
              v-if="!isOwn"
              class="sc-pill dislike"
              :title="t('profile.dislikeHint')"
              @click="dislikeConfirm = true"
              data-testid="profile-dislike-btn"
            >👎</button>
          </div>

          <!-- MEDALS (Steam-style badges). В вертикальном режиме — под лайками. -->
          <div v-if="medalTier > 0" class="medals medals-c1" data-testid="profile-medals">
            <div
              class="medal"
              :class="medalClass"
              :title="t('profile.medalDonDesc', { count: medalCount, total: medalTotal })"
              :data-testid="`medal-don-${medalTier}`"
            >
              <span class="medal-coin">
                <span class="medal-shine"></span>
                <span class="medal-coin-txt">DON</span>
              </span>
              <span class="medal-label">DON {{ medalRoman }}</span>
            </div>
          </div>
        </div>

        <!-- Столбец 2: баланс кошелька + ссылка + QR -->
        <div class="id-sect id-c2">
          <!-- Мой баланс (только на собственном профиле). -->
          <div v-if="isOwn" class="bal-card" data-testid="profile-balance">
            <span class="bal-label">{{ t('profile.balance') }}</span>
            <span class="bal-amt">{{ wallet.mainBalance }} <small>STH</small></span>
            <span class="bal-fiat">≈ ${{ wallet.mainFiat }} USDT</span>
          </div>

          <button
            class="share-btn"
            :disabled="!shareLink"
            @click="copy(shareLink, t('profile.linkCopied'))"
            data-testid="profile-copy-link"
          >
            <span class="sb-icon">⧉</span>
            <span class="sb-link">{{ shareLink || '—' }}</span>
          </button>

          <button
            v-if="shareLink"
            class="qr-icon-btn"
            @click="showQr = true"
            data-testid="profile-qr-btn"
            :title="t('profile.scanToVisit')"
          >
            <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true">
              <rect x="3" y="3" width="7" height="7" rx="1" />
              <rect x="14" y="3" width="7" height="7" rx="1" />
              <rect x="3" y="14" width="7" height="7" rx="1" />
              <path d="M14 14h3v3M21 14v7M17 21h4M14 21h0" stroke-linecap="round" />
            </svg>
            <span>QR</span>
          </button>
        </div>

        <!-- Столбец 3: регистрация имени + видимость профиля -->
        <div class="id-sect id-c3">
          <div v-if="isOwn && !username && regPending" class="reg-pending" data-testid="profile-register-pending">
            <span class="spin">⟳</span> {{ t('naming.registering') }}
          </div>
          <div v-else-if="isOwn && !username" class="reg-cta" data-testid="profile-register-cta">
            <button class="btn btn-primary reg-cta-btn" @click="showRegister = true" data-testid="profile-register-name">
              ⬡ {{ t('naming.registerName') }}
            </button>
            <span class="reg-cta-hint">{{ t('naming.ctaHint') }}</span>
          </div>

          <!-- Видимость профиля (тумблер) -->
          <div v-if="isOwn" class="privacy-row" data-testid="profile-privacy">
            <div class="pr-text">
              <span class="pr-label">{{ t('profile.visibility') }}</span>
              <span class="pr-state" :class="{ on: isPublic }">
                {{ isPublic ? t('profile.public') : t('profile.private') }}
              </span>
            </div>
            <button
              class="toggle"
              :class="{ on: isPublic }"
              :disabled="saving"
              @click="togglePrivacy"
              data-testid="profile-privacy-toggle"
            >
              <span class="knob" />
            </button>
          </div>

          <!-- MEDALS: в горизонтальном режиме — под тумблером видимости -->
          <div v-if="medalTier > 0" class="medals medals-c3" data-testid="profile-medals-horiz">
            <div
              class="medal"
              :class="medalClass"
              :title="t('profile.medalDonDesc', { count: medalCount, total: medalTotal })"
            >
              <span class="medal-coin">
                <span class="medal-shine"></span>
                <span class="medal-coin-txt">DON</span>
              </span>
              <span class="medal-label">DON {{ medalRoman }}</span>
            </div>
          </div>
        </div>
      </section>

      <!-- RIGHT COLUMN: tabbed — Wall | DAPPs | Torrent -->
      <div class="right-col">
      <div class="rc-tabs" data-testid="profile-right-tabs">
        <button
          class="rc-tab"
          :class="{ active: rightTab === 'wall' }"
          @click="rightTab = 'wall'"
          data-testid="profile-tab-wall"
        >{{ t('profile.tabWall') }}<span v-if="wallUnreadTotal > 0" class="tab-badge" data-testid="wall-unread-badge">{{ wallUnreadTotal }}</span></button>
        <button
          class="rc-tab"
          :class="{ active: rightTab === 'dapps' }"
          @click="rightTab = 'dapps'"
          data-testid="profile-tab-dapps"
        >{{ t('profile.tabDapps') }}</button>
        <button
          class="rc-tab"
          :class="{ active: rightTab === 'torrent' }"
          @click="rightTab = 'torrent'"
          data-testid="profile-tab-torrent"
        >{{ t('profile.tabTorrent') }}</button>
        <button
          class="rc-tab"
          :class="{ active: rightTab === 'music' }"
          @click="rightTab = 'music'"
          data-testid="profile-tab-music"
        >{{ t('nav.music') }}</button>
      </div>

      <!-- RIGHT: stats + reward address (DAPPs tab) -->
      <section
        v-if="rightTab === 'dapps' && (isOwn || publishedCount > 0)"
        class="panel stat-col"
        data-testid="profile-stats"
      >
        <template v-if="publishedCount > 0">
        <span class="block-title">{{ t('profile.stats') }}</span>
        <div class="stat-grid">
          <div class="stat" data-testid="stat-published">
            <span class="stat-num">{{ isOwn ? (manifest?.statsCache.appsPublished ?? 0) : publishedApps.length }}</span>
            <span class="stat-cap">{{ t('profile.published') }}</span>
          </div>
          <div v-if="isOwn" class="stat" data-testid="stat-seeded">
            <span class="stat-num">{{ manifest?.statsCache.appsSeeded ?? 0 }}</span>
            <span class="stat-cap">{{ t('profile.seeded') }}</span>
          </div>
        </div>
        </template>

        <div v-if="isOwn" class="reward">
          <label class="block-title sm">{{ t('profile.rewardAddress') }}</label>
          <p class="hint dim">{{ t('profile.rewardHint') }}</p>
          <div class="reward-row">
            <input
              v-model="rewardEdit"
              class="reward-input"
              :placeholder="auth.address"
              data-testid="profile-reward-input"
            />
            <button class="btn btn-primary" :disabled="saving" @click="saveReward" data-testid="profile-reward-save">
              {{ t('profile.save') }}
            </button>
          </div>
        </div>

        <!-- action tray -->
        <div v-if="isOwn" class="tray" data-testid="profile-action-tray">
          <span class="block-title sm">{{ t('profile.security') }}</span>
          <button class="btn btn-ghost" @click="exportKeypair" data-testid="profile-export-keypair">
            ⭳ {{ t('profile.exportKeypair') }}
          </button>
          <p class="hint dim">{{ t('profile.exportHint') }}</p>
          <button class="btn btn-ghost" :disabled="resyncing" @click="resyncRegistryNow" data-testid="profile-resync-registry">
            <span :class="{ spin: resyncing }" aria-hidden="true">↻</span> {{ resyncing ? t('profile.resyncing') : t('profile.resync') }}
          </button>
          <p class="hint dim">{{ t('profile.resyncHint') }}</p>
        </div>
      </section>

      <!-- WALL LAYOUT (VK-style): center posts + right social graph -->
      <div v-if="rightTab === 'wall'" class="wall-layout" data-testid="wall-layout">
        <WallFeed
          class="wall-center"
          :author="wallAddr"
          :is-own="isOwn"
          :my-address="auth.address || ''"
          data-testid="profile-microblog"
        />

        <aside v-if="isOwn || viewedUsername" class="wall-social" data-testid="profile-social">
          <!-- Подписчики + Друзья/Подписки в ОДНОМ блоке (макс 4 аватара в ряд) -->
          <section class="panel social-block" data-testid="social-block">
            <!-- Подписчики (кто подписан на меня) -->
            <div class="sg-group" data-testid="social-subscribers-block">
              <div class="sg-head">
                <span class="block-title">{{ t('profile.mySubscribers') }} <b class="sg-count">{{ stats.subscribers }}</b></span>
                <button
                  v-if="stats.recentSubscribers.length"
                  class="sg-all"
                  @click="graphModal = 'subscribers'"
                  data-testid="show-all-subscribers"
                >{{ t('profile.showAll') }}</button>
              </div>
              <div v-if="stats.recentSubscribers.length" class="people-grid" data-testid="subscribers-grid">
                <button
                  v-for="e in stats.recentSubscribers.slice(0, 4)"
                  :key="e.address"
                  class="person"
                  @click="openEntry(e)"
                  :data-testid="`subscriber-${e.address}`"
                >
                  <UserAvatar :address="e.address" :size="52" />
                  <span class="person-name">{{ e.name ? '@' + e.name : shortAddr(e.address, 5, 4) }}</span>
                </button>
              </div>
              <p v-else class="dim people-empty" data-testid="subscribers-empty">{{ t('profile.noSubscribers') }}</p>
            </div>

            <!-- Друзья / Подписки (на кого я подписан) -->
            <div class="sg-group" data-testid="social-following-block">
              <div class="sg-head">
                <span class="block-title">{{ t('profile.mySubscriptions') }} <b class="sg-count">{{ stats.following }}</b></span>
                <button
                  v-if="stats.followingList.length"
                  class="sg-all"
                  @click="graphModal = 'following'"
                  data-testid="show-all-following"
                >{{ t('profile.showAll') }}</button>
              </div>
              <div v-if="stats.followingList.length" class="people-grid" data-testid="following-grid">
                <button
                  v-for="e in stats.followingList.slice(0, 4)"
                  :key="e.address"
                  class="person"
                  @click="openEntry(e)"
                  :data-testid="`following-${e.address}`"
                >
                  <UserAvatar :address="e.address" :size="52" />
                  <span class="person-name">{{ e.name ? '@' + e.name : shortAddr(e.address, 5, 4) }}</span>
                </button>
              </div>
              <p v-else class="dim people-empty" data-testid="following-empty">{{ t('profile.noFollowing') }}</p>
            </div>
          </section>

          <!-- Block 3: персональный музыкальный плейлист (виден всем) -->
          <WallPlaylist :owner="wallAddr" :is-own="isOwn" />

          <!-- На чужом профиле дополнительно показываем МОЙ плейлист -->
          <WallPlaylist
            v-if="!isOwn && auth.address"
            :owner="auth.address"
            :is-own="true"
            :title="t('music.myPlaylist')"
          />
        </aside>
      </div>

      <!-- PUBLIC SHOWCASE: developer's published dApps (DAPPs tab) -->
      <section
        v-if="rightTab === 'dapps' && publishedCount > 0"
        class="panel showcase"
        data-testid="profile-showcase"
      >
        <div class="sc-head">
        <span class="block-title">{{ t('profile.showcase') }}</span>
        <span class="sc-count" data-testid="showcase-count">{{ publishedApps.length }}</span>
        <button class="btn btn-primary sm share-profile" @click="shareProfile" data-testid="profile-share-btn">
          ⤴ {{ t('profile.shareProfile') }}
        </button>
      </div>
      <p class="hint dim">{{ t('profile.showcaseHint') }}</p>

      <div v-if="publishedApps.length" class="sc-grid" data-testid="showcase-grid">
        <article
          v-for="m in publishedApps"
          :key="m.id"
          class="sc-card"
          :data-testid="`showcase-card-${m.id}`"
        >
          <div class="sc-logo">
            <img v-if="logos[m.id]" :src="logos[m.id]" :alt="m.name" />
            <Identicon v-else :seed="m.id" :size="56" />
          </div>
          <div class="sc-body">
            <div class="sc-title">
              <span class="sc-name">{{ m.name }}</span>
              <span v-if="m.version" class="sc-ver">v{{ m.version }}</span>
            </div>
            <p class="sc-desc">{{ m.description || t('profile.noDescription') }}</p>
            <div v-if="m.tags && m.tags.length" class="sc-tags">
              <span v-for="tg in m.tags.slice(0, 3)" :key="tg" class="sc-tag">{{ tg }}</span>
            </div>
          </div>
          <div class="sc-actions">
            <button class="btn btn-primary sm" @click="launchApp(m)" :data-testid="`showcase-launch-${m.id}`">
              {{ t('profile.launch') }}
            </button>
            <button
              class="btn btn-ghost sm"
              @click="copy(`sth://${m.id}`, t('profile.linkCopied'))"
              :data-testid="`showcase-copy-${m.id}`"
            >
              ⧉
            </button>
          </div>
        </article>
      </div>
      <p v-else class="empty" data-testid="showcase-empty">{{ t('profile.showcaseEmpty') }}</p>
    </section>

      <!-- «Мои домены» — отдельный горизонтальный блок во вкладке DAPPS,
           между «Опубликованными dApp» (order 1) и «Статистикой вклада»
           (order 3). Показывается только на вкладке DAPPS. -->
      <div
        v-if="rightTab === 'dapps' && showDomainSection"
        class="domains-block domains-dapps"
        data-testid="profile-domains"
      >
        <div class="db-head">
          <span class="db-title">{{ isOwn ? t('profile.myDomains') : t('profile.domains') }}</span>
          <label v-if="isOwn" class="db-show" data-testid="profile-domains-show-all">
            <input
              type="checkbox"
              :checked="showDomains"
              :disabled="saving"
              @change="toggleShowDomains"
              data-testid="profile-domains-show-all-cb"
            />
            <span>{{ t('profile.showToAll') }}</span>
          </label>
        </div>
        <p v-if="isOwn && !myDomains.length" class="db-empty">
          {{ t('profile.noDomains') }}
        </p>
        <ul v-else class="db-list">
          <li
            v-for="d in myDomains"
            :key="`${d.name}.${d.zone}`"
            class="db-item"
            :class="{ link: !!d.appId, renewing: renewOpen === `${d.name}.${d.zone}` }"
            :data-testid="`profile-domain-${d.name}`"
          >
            <div class="db-row" @click="openDomain(d)"
              :title="d.appId ? t('profile.openBoundApp') : t('profile.domainNoApp')">
              <span class="db-dot" :class="{ on: !!d.appId }"></span>
              <span class="db-name">{{ d.name }}.{{ d.zone }}</span>
              <span class="db-exp" :class="expTone(d)">
                {{ fmtExpDate(d.expires) }} · {{ daysLeft(d) }} {{ t('nodePanel.domDays') }}
              </span>
              <button
                v-if="isOwn && isRenewable(d)"
                class="db-renew"
                @click.stop="renewOpen = renewOpen === `${d.name}.${d.zone}` ? '' : `${d.name}.${d.zone}`"
                :data-testid="`profile-domain-renew-${d.name}`"
              >
                {{ t('nodePanel.domRenew') }}
              </button>
              <span v-else-if="d.appId" class="db-open">↗</span>
            </div>
            <div
              v-if="isOwn && renewOpen === `${d.name}.${d.zone}`"
              class="db-renew-form"
              :data-testid="`profile-domain-renew-form-${d.name}`"
            >
              <p class="db-renew-q">
                {{ t('nodePanel.domRenewQ', { name: `${d.name}.${d.zone}`, price: renewPrice }) }}
              </p>
              <p class="db-renew-bal" :class="{ low: balanceSth < renewPrice }">
                {{ t('nodePanel.domBalance') }}: {{ balanceSth }} STH
                <span v-if="balanceSth < renewPrice" class="db-renew-warn">
                  · {{ t('nodePanel.domInsufficient') }}
                </span>
              </p>
              <button
                class="db-renew-confirm"
                :disabled="renewBusy || balanceSth < renewPrice"
                @click.stop="doRenew(d)"
                :data-testid="`profile-domain-renew-confirm-${d.name}`"
              >
                {{ renewBusy ? '…' : t('nodePanel.domRenewBtn', { price: renewPrice }) }}
              </button>
            </div>
          </li>
        </ul>
      </div>

      <!-- DAPPs tab: nothing published (viewing someone else) -->
      <section
        v-if="rightTab === 'dapps' && !isOwn && publishedCount === 0"
        class="panel"
        data-testid="profile-dapps-empty"
      >
        <span class="block-title">{{ t('profile.tabDapps') }}</span>
        <p class="hint dim">{{ t('profile.showcaseEmpty') }}</p>
      </section>

      <!-- TORRENT tab: seeding / network-contribution stats -->
      <section v-if="rightTab === 'torrent'" class="panel torrent-stats" data-testid="profile-torrent">
        <span class="block-title">🌱 {{ t('profile.tabTorrent') }}</span>

        <template v-if="isOwn">
          <!-- counters: seeds / downloaded / uploaded -->
          <div class="ts-grid">
            <div class="ts-card" data-testid="ts-seeds">
              <span class="ts-k">{{ t('profile.mySeeds') }}</span>
              <span class="ts-v seeds">{{ seedingCount }}<small>/ {{ activeTorrents.length }}</small></span>
              <span class="ts-sub">{{ t('profile.activeTorrents') }}</span>
            </div>
            <div class="ts-card" data-testid="ts-downloaded">
              <span class="ts-k">⬇ {{ t('profile.totalDownloaded') }}</span>
              <span class="ts-v dl">{{ fmtBytes(torrentStats.downloadedBytes) }}</span>
              <span class="ts-sub">{{ fmtBytes(downSpeedTotal) }}/s</span>
            </div>
            <div class="ts-card" data-testid="ts-uploaded">
              <span class="ts-k">⬆ {{ t('profile.totalUploaded') }}</span>
              <span class="ts-v ul">{{ fmtBytes(torrentStats.uploadedBytes) }}</span>
              <span class="ts-sub">{{ fmtBytes(upSpeedTotal) }}/s</span>
            </div>
          </div>

          <!-- My Node (byte-hours + rank + progress), mirrored from Network Map -->
          <div class="node-card" data-testid="profile-my-node">
            <div class="nc-head">
              <span class="nc-title">// {{ t('network.myNode') }}</span>
              <span class="rank-badge" :style="{ '--rank-color': rankMeta.color }" data-testid="profile-node-rank">
                {{ rankMeta.label }}
              </span>
            </div>
            <div class="nc-row">
              <div class="nc-metric">
                <span class="nc-k">{{ t('network.byteHours') }}</span>
                <span class="nc-v accent" data-testid="profile-node-bh">{{ byteHoursLabel }} <small>{{ t('network.gibH') }}</small></span>
              </div>
              <div class="nc-metric">
                <span class="nc-k">⬆ {{ t('network.totalUploaded') }}</span>
                <span class="nc-v" data-testid="profile-node-uploaded">{{ fmtBytes(torrentStats.uploadedBytes) }}</span>
              </div>
            </div>
            <div class="nc-progress">
              <div class="nc-progress-head">
                <span v-if="torrentStats.nextRankAt != null" class="dim">{{ t('network.nextRankAt', { v: torrentStats.nextRankAt?.toFixed(0) }) }}</span>
                <span v-else class="dim">{{ t('network.maxRank') }}</span>
                <span class="nc-pct">{{ rankProgressPct }}%</span>
              </div>
              <div class="nc-progress-bar">
                <div class="nc-progress-fill" :style="{ width: rankProgressPct + '%', '--rank-color': rankMeta.color }"></div>
              </div>
            </div>
          </div>
        </template>
        <p v-else class="hint dim">{{ t('profile.torrentLocalOnly') }}</p>
      </section>

      <!-- MUSIC tab: albums of this profile + Studio (own only) -->
      <section v-if="rightTab === 'music'" class="panel" data-testid="profile-music">
        <div class="music-tab-head">
          <span class="block-title">♫ {{ t('nav.music') }} <b class="sg-count">{{ profileAlbums.length }}</b></span>
          <button v-if="isOwn" class="studio-open-btn" @click="editAlbum = null; showStudio = true" data-testid="open-studio-profile">
            🎛 {{ t('music.studio') }}
          </button>
        </div>
        <p v-if="!profileAlbums.length" class="hint dim">{{ t('music.emptyProfile') }}</p>
        <div v-else class="music-tab-list">
          <AlbumCard
            v-for="a in profileAlbums"
            :key="a.id"
            :album="a"
            @edit="onEditAlbum"
            @deleted="onAlbumDeleted"
          />
        </div>

        <!-- Альбомы других авторов (купленные / добавленные) — только на своём профиле -->
        <template v-if="isOwn && acquiredAlbums.length">
          <div class="music-tab-head acquired-head">
            <span class="block-title">🎧 {{ t('music.otherAuthorsAlbums') }} <b class="sg-count">{{ acquiredAlbums.length }}</b></span>
          </div>
          <div class="music-tab-list" data-testid="profile-music-acquired">
            <AlbumCard v-for="a in acquiredAlbums" :key="a.id" :album="a" />
          </div>
        </template>
      </section>
    </div>
    </div>

    <MusicStudioModal v-if="showStudio" :edit="editAlbum" @close="closeStudio" @created="onAlbumCreated" />

    <IdentityRegistration v-if="showRegister" @close="showRegister = false" @registered="onRegistered" />
    <TxConfirmModal
      v-if="subConfirm && viewedUsername"
      :title="t('profile.subscribeConfirm', { name: viewedUsername })"
      :recipient="identity"
      :amount="subPrice"
      :vendor="`sub:${viewedUsername}`"
      :busy="subscribing"
      @confirm="confirmSubscribe"
      @cancel="subConfirm = false"
    />

    <TxConfirmModal
      v-if="dislikeConfirm && !isOwn"
      :title="t('profile.dislikeConfirm', { name: viewedUsername || shortAddr(identity) })"
      :recipient="identity"
      :amount="1"
      vendor="reputacion-off"
      :busy="disliking"
      @confirm="confirmDislike"
      @cancel="dislikeConfirm = false"
    />

    <transition name="fade">
      <div v-if="flash" class="flash" data-testid="profile-flash">{{ flash }}</div>
    </transition>

    <!-- QR modal (opened from the QR icon button) -->
    <transition name="fade">
      <div v-if="showQr" class="qr-modal-back" @click.self="showQr = false" data-testid="profile-qr-modal">
        <div class="qr-modal">
          <button class="qr-modal-x" @click="showQr = false" data-testid="profile-qr-modal-close" aria-label="close">✕</button>
          <img v-if="qrDataUrl" :src="qrDataUrl" alt="QR u://" class="qr-modal-img" />
          <code class="qr-modal-link">{{ shareLink }}</code>
          <span class="qr-modal-cap">{{ t('profile.scanToVisit') }}</span>
        </div>
      </div>
    </transition>

    <!-- Social graph modal: full list of subscribers / following -->
    <div
      v-if="graphModal"
      class="graph-modal-back"
      @click.self="graphModal = null"
      data-testid="graph-modal"
    >
      <div class="graph-modal">
        <div class="gm-head">
          <span class="gm-title">
            {{ graphModal === 'subscribers' ? t('profile.mySubscribers') : t('profile.mySubscriptions') }}
            <b class="sg-count">{{ graphModal === 'subscribers' ? stats.subscribers : stats.following }}</b>
          </span>
          <button class="gm-x" @click="graphModal = null" data-testid="graph-modal-close" aria-label="close">✕</button>
        </div>
        <div class="gm-list">
          <button
            v-for="e in (graphModal === 'subscribers' ? stats.recentSubscribers : stats.followingList)"
            :key="e.address"
            class="gm-row"
            @click="openEntry(e); graphModal = null"
            :data-testid="`graph-row-${e.address}`"
          >
            <UserAvatar :address="e.address" :size="40" />
            <span class="gm-name">{{ e.name ? '@' + e.name : shortAddr(e.address, 6, 5) }}</span>
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.profile-view {
  padding: 28px 32px;
  max-width: none;
  width: 100%;
  position: relative;
}
.ph {
  margin-bottom: 28px;
}
.ph-title {
  font-size: 1.9rem;
  letter-spacing: 0.04em;
  margin: 0;
}
.ph-sub {
  color: var(--text-dim);
  margin-top: 6px;
  font-size: 0.9rem;
}
.grid {
  display: grid;
  grid-template-columns: 320px 1fr;
  gap: 22px;
  align-items: start;
}
/* Вкладка СТЕНА: контент фиксированный (962px), поэтому ограничиваем всю
   сетку 320+22+962 и центрируем — левый профиль прилегает к центру,
   пустого зазора между ними нет. */
.grid-wall {
  max-width: 1304px;
  margin: 0 auto;
}
.right-col {
  display: flex;
  flex-direction: column;
  gap: 22px;
  min-width: 0;
}
.right-col .showcase {
  margin-top: 0;
}
/* DAPPs tab: showcase → мои домены → contribution stats (сверху вниз) */
.right-col .rc-tabs {
  order: -1;
}
.right-col .showcase {
  order: 1;
}
.right-col .domains-dapps {
  order: 2;
  margin-top: 0;
}
.right-col .stat-col {
  order: 3;
}
/* --- Right-column tab bar --- */
.rc-tabs {
  display: flex;
  gap: 8px;
}
.rc-tab {
  flex: 1;
  padding: 9px 12px;
  border-radius: 10px;
  border: 1px solid var(--border);
  background: transparent;
  color: var(--text-dim);
  font-size: 12px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  cursor: pointer;
  transition: background-color 0.15s, color 0.15s, border-color 0.15s;
}
.tab-badge {
  display: inline-block;
  min-width: 16px;
  margin-left: 6px;
  padding: 1px 6px;
  border-radius: 999px;
  background: #ff4d4f;
  color: #fff;
  font-size: 10px;
  font-weight: 700;
  line-height: 1.5;
  vertical-align: middle;
}
.rc-tab:hover {
  color: var(--accent);
  border-color: var(--border-accent);
}
.rc-tab.active {
  background: var(--accent);
  color: #03121a;
  border-color: var(--accent);
  font-weight: 700;
}
/* --- VK-style Wall layout: ФИКСИРОВАННЫЕ ширины, контент по центру ---
   Центр (микроблог) = 580px, правый сайдбар = 360px. Ничего не тянется. */
.wall-layout {
  display: flex;
  gap: 22px;
  align-items: flex-start;
  justify-content: center;
}
.wall-center {
  flex: 0 1 580px;
  width: 580px;
  min-width: 550px;
}
.wall-social {
  flex: 0 0 360px;
  width: 360px;
  max-width: 360px;
  display: grid;
  /* minmax(0,1fr): дети НЕ могут распирать колонку шире 360px */
  grid-template-columns: minmax(0, 1fr);
  gap: 22px;
  align-content: start;
  position: sticky;
  top: 16px;
}
.wall-social > * {
  min-width: 0;
}
.social-block {
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.social-block .block-title {
  display: flex;
  align-items: center;
  gap: 8px;
}
/* Две группы (подписчики / друзья) внутри одного блока — с разделителем */
.social-block .sg-group + .sg-group {
  margin-top: 4px;
  padding-top: 18px;
  border-top: 1px solid var(--border);
}
.sg-count {
  color: var(--text-dim);
  font-weight: 600;
}
.music-tab-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 14px;
}
.acquired-head {
  margin-top: 26px;
  padding-top: 18px;
  border-top: 1px solid rgba(0, 229, 255, 0.14);
}
.studio-open-btn {
  padding: 8px 16px;
  border-radius: 999px;
  border: none;
  background: linear-gradient(135deg, #00e5ff, #a855f7);
  color: #06121e;
  font-weight: 700;
  font-size: 11px;
  cursor: pointer;
  white-space: nowrap;
}
.music-tab-list {
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.sg-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.sg-all {
  background: transparent;
  border: none;
  color: var(--accent);
  font-size: 12px;
  cursor: pointer;
  padding: 2px 4px;
  white-space: nowrap;
  transition: opacity 0.15s;
}
.sg-all:hover {
  opacity: 0.75;
  text-decoration: underline;
}
.graph-modal-back {
  position: fixed;
  inset: 0;
  z-index: 200;
  background: rgba(3, 8, 14, 0.72);
  backdrop-filter: blur(6px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
}
.graph-modal {
  width: 420px;
  max-width: 100%;
  max-height: 74vh;
  display: flex;
  flex-direction: column;
  background: var(--panel, #111c26);
  border: 1px solid var(--border);
  border-radius: 16px;
  overflow: hidden;
  box-shadow: 0 24px 70px rgba(0, 0, 0, 0.55);
}
.gm-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px 18px;
  border-bottom: 1px solid var(--border);
}
.gm-title {
  font-size: 13px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}
.gm-x {
  background: none;
  border: none;
  color: var(--text-dim);
  font-size: 18px;
  cursor: pointer;
}
.gm-x:hover {
  color: var(--text);
}
.gm-list {
  overflow-y: auto;
  padding: 8px;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.gm-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px 12px;
  border-radius: 10px;
  background: transparent;
  border: none;
  color: inherit;
  cursor: pointer;
  text-align: left;
  transition: background-color 0.15s;
}
.gm-row:hover {
  background: rgba(255, 255, 255, 0.05);
}
.gm-name {
  font-size: 14px;
  font-weight: 600;
}
.wall-social .people-grid {
  grid-template-columns: repeat(4, 1fr);
  gap: 10px;
}
/* В узком сайдбаре аватары без карточной рамки — чище, как в примере */
.wall-social .person {
  background: transparent;
  border: none;
  padding: 4px 2px;
}
.wall-social .person:hover {
  border-color: transparent;
  transform: translateY(-2px);
}
.wall-social .person-name {
  font-size: 11px;
}

/* Профиль-колонка: 3 логических столбца (id-c1 / id-c2 / id-c3).
   По умолчанию (широкий экран) — профиль СЛЕВА, вертикальный центрированный стек. */
.id-col {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 16px;
}
.id-sect {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 14px;
  width: 100%;
}

/* Медали: дубликат в c3 показывается только в горизонтальном режиме */
.medals.medals-c3 {
  display: none;
}
@media (max-width: 1420px) and (min-width: 761px) {
  .medals.medals-c1 {
    display: none;
  }
  .medals.medals-c3 {
    display: flex;
  }
}

/* Адаптив: когда ширины не хватает — профиль превращается в
   ГОРИЗОНТАЛЬНУЮ карточку из 3 столбцов (~33% каждый):
   1) аватар/имя/репутация/лайки/медали  2) баланс + ссылка + QR  3) тумблер видимости.
   Брейкпоинт 1420px: ниже него двухколоночная сетка (320px профиль +
   962px фикс. контент) уже не помещается. */
@media (max-width: 1420px) {
  .grid {
    grid-template-columns: 1fr;
  }
  .id-col {
    flex-direction: row;
    align-items: flex-start;
    gap: 0;
  }
  .id-sect {
    flex: 1 1 0;
    min-width: 0;
    max-width: 33.33%;
    padding: 0 24px;
    gap: 12px;
  }
  .id-sect:first-child {
    padding-left: 0;
  }
  .id-sect:last-child {
    padding-right: 0;
  }
  .id-sect + .id-sect {
    border-left: 1px solid var(--border);
  }
  /* Столбец 1: аватар/имя — по центру своего столбца */
  .id-c1 {
    align-items: center;
  }
  /* Столбцы 2-3 — выравнивание влево */
  .id-c2,
  .id-c3 {
    align-items: flex-start;
  }
  .id-c2 > *,
  .id-c3 > * {
    text-align: left;
  }
  .id-c2 .bal-card,
  .id-c2 .share-btn,
  .id-c3 .privacy-row,
  .id-c3 .reg-cta {
    width: 100%;
  }
}

/* Совсем узко — обратно в вертикальный стек */
@media (max-width: 760px) {
  .id-col {
    flex-direction: column;
  }
  .id-sect {
    padding: 0;
    width: 100%;
    max-width: 100%;
    align-items: center;
  }
  .id-sect + .id-sect {
    border-left: none;
    border-top: 1px solid var(--border);
    padding-top: 16px;
  }
  .id-c2 > *,
  .id-c3 > * {
    text-align: center;
  }
}

@media (max-width: 1060px) {
  .wall-layout {
    flex-direction: column;
    align-items: stretch;
  }
  .wall-center {
    flex: 1 1 auto;
    width: 100%;
    min-width: 0;
    max-width: none;
  }
  .wall-social {
    flex: 1 1 auto;
    width: 100%;
    position: static;
  }
}
@media (max-width: 920px) {
  .grid {
    grid-template-columns: 1fr;
  }
}
.username-label {
  font-size: 1rem;
  font-weight: var(--fw-700);
  color: #00E5FF;
  text-shadow: 0 0 10px rgba(0, 229, 255, 0.5);
  letter-spacing: 0.02em;
}
.reg-cta {
  width: 100%;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
}
.reg-cta-btn {
  width: 100%;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
}
.reg-cta-hint {
  font-size: 0.72rem;
  color: var(--text-dim);
  text-align: center;
}
.panel {
  border: 1px solid var(--border);
  border-radius: 14px;
  background: var(--bg-elevated);
  padding: 24px;
}
.avatar-wrap {
  position: relative;
  width: 120px;
  height: 120px;
  border-radius: 50%;
  overflow: hidden;
}
.avatar-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}
.avatar-wrap :deep(svg) {
  width: 100%;
  height: 100%;
  display: block;
}
.status-dot {
  position: absolute;
  right: 10px;
  bottom: 10px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: #4b5563;
  border: 3px solid var(--bg-elevated);
  z-index: 2;
}
.status-dot.live {
  background: #22c55e;
  box-shadow: 0 0 10px #22c55e;
}
.addr {
  font-family: monospace;
  font-size: 0.95rem;
  color: var(--accent);
  letter-spacing: 0.03em;
}
.addr-clickable {
  cursor: pointer;
  user-select: none;
  padding: 2px 8px;
  border-radius: 6px;
  transition: background-color 0.15s ease, transform 0.1s ease;
}
.addr-clickable:hover {
  background: rgba(var(--accent-rgb, 100, 200, 150), 0.12);
}
.addr-clickable:active {
  transform: scale(0.97);
}
.addr-clickable:focus-visible {
  outline: 1px solid var(--accent);
  outline-offset: 2px;
}
.privacy-row {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 0;
  border-top: 1px solid var(--border);
  border-bottom: 1px solid var(--border);
}
.pr-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.pr-label {
  font-size: 0.7rem;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: var(--text-dim);
}
.pr-state {
  font-size: 0.95rem;
  color: var(--text-dim);
}
.pr-state.on {
  color: #22c55e;
}
.toggle {
  width: 48px;
  height: 26px;
  border-radius: 999px;
  background: var(--bg);
  border: 1px solid var(--border);
  position: relative;
  cursor: pointer;
  transition: background-color 0.25s ease;
}
.toggle.on {
  background: rgba(34, 197, 94, 0.25);
  border-color: #22c55e;
}
.knob {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 20px;
  height: 20px;
  border-radius: 50%;
  background: var(--text-dim);
  transition: transform 0.25s ease, background-color 0.25s ease;
}
.toggle.on .knob {
  transform: translateX(22px);
  background: #22c55e;
}
.share-btn {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 12px;
  border: 1px dashed var(--accent);
  border-radius: 10px;
  background: transparent;
  color: var(--accent);
  cursor: pointer;
  transition: background-color 0.2s ease;
}
.share-btn:hover {
  background: rgba(0, 229, 255, 0.06);
}
.sb-icon {
  flex: none;
}
.sb-link {
  font-family: monospace;
  font-size: 0.8rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.msg-btn {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  margin-top: 4px;
  padding: 9px 18px;
  border: 1px solid #1f5a52;
  border-radius: 999px;
  background: #0e2e2a;
  color: #38f9d7;
  font-size: 0.78rem;
  font-weight: var(--fw-600);
  letter-spacing: 0.06em;
  text-transform: uppercase;
  cursor: pointer;
  transition: background-color 0.18s ease, color 0.18s ease;
}
.msg-btn:hover {
  background: #38f9d7;
  color: #04110f;
}
.msg-btn-icon {
  font-size: 0.62rem;
  letter-spacing: -1px;
}

.qr-icon-btn {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 8px 14px;
  border-radius: 999px;
  border: 1px solid var(--border, #2a3340);
  background: var(--panel-2, #11161d);
  color: var(--accent);
  font-size: 0.72rem;
  font-weight: var(--fw-700);
  letter-spacing: 0.08em;
  cursor: pointer;
  transition: border-color 0.16s ease, color 0.16s ease, background-color 0.16s ease;
}
.qr-icon-btn:hover {
  border-color: var(--accent);
  background: var(--bg-elevated, #161b22);
}
.qr-modal-back {
  position: fixed;
  inset: 0;
  z-index: 120;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(4, 8, 12, 0.82);
}
.qr-modal {
  position: relative;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 26px 26px 20px;
  border-radius: 16px;
  background: #e8fbff;
  box-shadow: 0 24px 60px -20px #000;
}
.qr-modal-x {
  position: absolute;
  top: 8px;
  right: 10px;
  background: transparent;
  border: none;
  font-size: 14px;
  color: #04121a;
  cursor: pointer;
}
.qr-modal-img {
  width: 240px;
  height: 240px;
  display: block;
  border-radius: 8px;
}
.qr-modal-link {
  font-size: 0.78rem;
  color: #04121a;
  font-weight: var(--fw-700);
}
.qr-modal-cap {
  font-size: 0.66rem;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: #2a4654;
}

/* DON donor medal — Steam-style shiny coin badge */
.medals {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
  justify-content: center;
}
.medal {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 5px;
}
.medal-coin {
  position: relative;
  width: 46px;
  height: 46px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
}
/* Bronze — DON I */
.medal.bronze .medal-coin {
  background: radial-gradient(circle at 32% 28%, #f4c9a0 0%, #d98e54 34%, #a85f2a 64%, #6e3a13 100%);
  box-shadow: 0 0 0 2px #5e3110, 0 0 12px -3px rgba(217, 142, 84, 0.8),
    inset 0 2px 4px rgba(255, 255, 255, 0.45), inset 0 -3px 6px rgba(80, 40, 12, 0.6);
}
.medal.bronze .medal-coin-txt { color: #5a3406; }
.medal.bronze .medal-label { color: #d98e54; }
/* Silver — DON II */
.medal.silver .medal-coin {
  background: radial-gradient(circle at 32% 28%, #ffffff 0%, #d4dbe2 34%, #9aa6b2 64%, #5e6b78 100%);
  box-shadow: 0 0 0 2px #4a5560, 0 0 16px -2px rgba(200, 215, 230, 0.85),
    inset 0 2px 4px rgba(255, 255, 255, 0.7), inset 0 -3px 6px rgba(70, 85, 100, 0.6);
}
.medal.silver .medal-coin-txt { color: #3a4450; }
.medal.silver .medal-label { color: #c2cdd9; }
/* Gold — DON III */
.medal.gold .medal-coin {
  background: radial-gradient(circle at 32% 28%, #ffe9a8 0%, #ffc94d 34%, #e89c1c 64%, #9c5e0c 100%);
  box-shadow: 0 0 0 2px #7a4a08, 0 0 20px 0 rgba(255, 190, 70, 0.95),
    inset 0 2px 4px rgba(255, 255, 255, 0.55), inset 0 -3px 6px rgba(120, 70, 6, 0.6);
}
.medal.gold .medal-coin-txt { color: #5a3406; }
.medal.gold .medal-label { color: #ffd66b; }
.medal.gold .medal-coin { animation: medal-pulse 2.4s ease-in-out infinite; }
@keyframes medal-pulse {
  0%, 100% { box-shadow: 0 0 0 2px #7a4a08, 0 0 18px -2px rgba(255, 190, 70, 0.9), inset 0 2px 4px rgba(255, 255, 255, 0.55), inset 0 -3px 6px rgba(120, 70, 6, 0.6); }
  50% { box-shadow: 0 0 0 2px #7a4a08, 0 0 28px 4px rgba(255, 200, 80, 1), inset 0 2px 4px rgba(255, 255, 255, 0.6), inset 0 -3px 6px rgba(120, 70, 6, 0.6); }
}
.medal-coin-txt {
  font-size: 0.62rem;
  font-weight: 900;
  letter-spacing: 0.04em;
  color: #5a3406;
  text-shadow: 0 1px 0 rgba(255, 255, 255, 0.5);
  z-index: 1;
}
.medal-shine {
  position: absolute;
  top: 0;
  left: -120%;
  width: 60%;
  height: 100%;
  transform: skewX(-22deg);
  background: linear-gradient(90deg, transparent, rgba(255, 255, 255, 0.85), transparent);
  animation: medal-shine 2.6s cubic-bezier(0.22, 1, 0.36, 1) infinite;
}
@keyframes medal-shine {
  0% { left: -120%; }
  55%, 100% { left: 160%; }
}
.medal-label {
  font-size: 0.58rem;
  font-weight: var(--fw-700);
  letter-spacing: 0.12em;
  color: #e0a93a;
}
.bal-card {
  width: 100%;
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 16px;
  border-radius: 12px;
  background: linear-gradient(135deg, rgba(0, 229, 255, 0.08), transparent);
  border: 1px solid var(--border);
}
.bal-label {
  font-size: 0.68rem;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: var(--text-dim);
}
.bal-amt {
  font-size: 1.55rem;
  font-weight: var(--fw-600);
}
.bal-amt small {
  font-size: 0.8rem;
  color: var(--text-dim);
}
.bal-fiat {
  font-size: 0.85rem;
  color: var(--text-dim);
}
/* right column */
.block-title {
  display: block;
  font-size: 0.72rem;
  text-transform: uppercase;
  letter-spacing: 0.1em;
  color: var(--text-dim);
  margin-bottom: 14px;
}
.block-title.sm {
  margin-bottom: 6px;
}
/* --- Torrent tab stats --- */
.ts-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 12px;
  margin-bottom: 20px;
}
.ts-card {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 14px;
  border: 1px solid rgba(0, 229, 255, 0.16);
  border-radius: 12px;
  background: rgba(255, 255, 255, 0.03);
}
.ts-k {
  font-size: 11px;
  color: var(--text-dim);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}
.ts-v {
  font-size: 22px;
  font-weight: 800;
  color: #eaf6ff;
}
.ts-v small {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-dim);
  margin-left: 2px;
}
.ts-v.seeds { color: #4ade80; }
.ts-v.dl { color: #38bdf8; }
.ts-v.ul { color: #a855f7; }
.ts-sub {
  font-size: 11px;
  color: #6f96ad;
}
.node-card {
  padding: 16px;
  border: 1px solid rgba(168, 85, 247, 0.22);
  border-radius: 14px;
  background: linear-gradient(160deg, rgba(168, 85, 247, 0.06), rgba(0, 229, 255, 0.04));
}
.nc-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 14px;
}
.nc-title {
  font-size: 12px;
  letter-spacing: 0.06em;
  color: #9fb8c8;
  font-weight: 700;
}
.rank-badge {
  font-size: 11px;
  font-weight: 800;
  letter-spacing: 0.08em;
  padding: 3px 12px;
  border-radius: 999px;
  color: var(--rank-color);
  border: 1px solid color-mix(in srgb, var(--rank-color) 55%, transparent);
  background: color-mix(in srgb, var(--rank-color) 12%, transparent);
  box-shadow: 0 0 12px color-mix(in srgb, var(--rank-color) 30%, transparent);
}
.nc-row {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 14px;
  margin-bottom: 16px;
}
.nc-metric {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.nc-k {
  font-size: 11px;
  color: var(--text-dim);
}
.nc-v {
  font-size: 18px;
  font-weight: 700;
  color: #eaf6ff;
}
.nc-v.accent { color: #00e5ff; }
.nc-v small { font-size: 11px; color: var(--text-dim); font-weight: 600; }
.nc-progress-head {
  display: flex;
  justify-content: space-between;
  font-size: 11px;
  margin-bottom: 6px;
}
.nc-pct { color: #eaf6ff; font-weight: 700; }
.nc-progress-bar {
  height: 8px;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.06);
  overflow: hidden;
}
.nc-progress-fill {
  height: 100%;
  border-radius: 999px;
  background: var(--rank-color);
  box-shadow: 0 0 10px color-mix(in srgb, var(--rank-color) 55%, transparent);
  transition: width 0.4s ease;
}
.stat-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 14px;
  margin-bottom: 26px;
}
.stat {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 18px;
  border-radius: 12px;
  background: var(--bg);
  border: 1px solid var(--border);
}
.stat-num {
  font-size: 2rem;
  font-weight: var(--fw-700);
  color: var(--accent);
}
.stat-cap {
  font-size: 0.75rem;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--text-dim);
}
.reward {
  margin-bottom: 26px;
}
.hint {
  font-size: 0.8rem;
  margin: 4px 0 10px;
}
.hint.dim {
  color: var(--text-dim);
}
.reward-row {
  display: flex;
  gap: 10px;
}
.reward-input {
  flex: 1;
  padding: 9px 12px;
  border-radius: 8px;
  border: 1px solid var(--border);
  background: var(--bg);
  color: var(--text);
  font-family: monospace;
  font-size: 0.85rem;
}
.tray {
  border-top: 1px solid var(--border);
  padding-top: 18px;
}
.showcase {
  margin-top: 22px;
}
.sc-head {
  display: flex;
  align-items: center;
  gap: 10px;
}
.sc-count {
  font-size: 0.8rem;
  font-weight: var(--fw-700);
  color: var(--accent);
  background: rgba(0, 229, 255, 0.1);
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 1px 9px;
}
.share-profile {
  margin-left: auto;
}
.sc-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
  gap: 14px;
  margin-top: 14px;
}
.sc-card {
  display: flex;
  gap: 12px;
  padding: 14px;
  border-radius: 12px;
  background: var(--bg);
  border: 1px solid var(--border);
  transition: border-color 0.2s ease, transform 0.2s ease;
}
.sc-card:hover {
  border-color: var(--accent);
  transform: translateY(-2px);
}
.sc-logo {
  flex: none;
  width: 56px;
  height: 56px;
  border-radius: 12px;
  overflow: hidden;
}
.sc-logo img {
  width: 56px;
  height: 56px;
  object-fit: cover;
  border-radius: 12px;
}
.sc-body {
  flex: 1;
  min-width: 0;
}
.sc-title {
  display: flex;
  align-items: baseline;
  gap: 8px;
}
.sc-name {
  font-weight: var(--fw-600);
  font-size: 0.98rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.sc-ver {
  font-size: 0.68rem;
  color: var(--accent);
  flex: none;
}
.sc-desc {
  font-size: 0.8rem;
  color: var(--text-dim);
  margin: 4px 0 8px;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.sc-tags {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
}
.sc-tag {
  font-size: 0.66rem;
  color: var(--text-dim);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 1px 7px;
}
.sc-actions {
  display: flex;
  flex-direction: column;
  gap: 6px;
  flex: none;
}
.btn.sm {
  padding: 5px 12px;
  font-size: 0.78rem;
}
.empty {
  margin-top: 14px;
  color: var(--text-dim);
  font-size: 0.88rem;
}
.flash {
  position: fixed;
  bottom: 28px;
  left: 50%;
  transform: translateX(-50%);
  background: var(--accent);
  color: #04121a;
  padding: 9px 20px;
  border-radius: 999px;
  font-size: 0.85rem;
  font-weight: var(--fw-600);
  z-index: 200;
}
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.25s ease;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
.social-counts {
  display: flex;
  gap: 10px;
  margin-top: 4px;
}
.sc-pill {
  font-size: 12px;
  font-weight: var(--fw-700);
  padding: 4px 10px;
  border-radius: 999px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
}
.sc-pill.heart { color: #f472b6; }
.sc-pill.like { color: #60a5fa; }
.sc-pill.dislike {
  color: #f87171;
  cursor: pointer;
  transition: background-color 0.15s ease, border-color 0.15s ease, transform 0.1s ease;
}
.sc-pill.dislike:hover {
  background: rgba(248, 113, 113, 0.12);
  border-color: rgba(248, 113, 113, 0.5);
  transform: translateY(-1px);
}
.reputation-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-top: 8px;
  padding: 3px 12px;
  border-radius: 999px;
  font-size: 12px;
  color: #fcd34d;
  background: rgba(252, 211, 77, 0.08);
  border: 1px solid rgba(252, 211, 77, 0.3);
  box-shadow: 0 0 12px rgba(252, 211, 77, 0.12);
}
.reputation-badge b {
  color: #fde68a;
  font-weight: 800;
}
.sub-cta {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  width: 100%;
  margin-top: 6px;
}
.sub-btn {
  width: 100%;
  padding: 11px 14px;
}
.sub-note {
  font-size: 10px;
  text-align: center;
}
.subscribed-badge {
  font-size: 12px;
  font-weight: var(--fw-700);
  color: var(--success, #4a9e7f);
  padding: 8px 12px;
  border: 1px solid var(--success, #4a9e7f);
  border-radius: 999px;
  margin-top: 6px;
}
.reg-pending {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  font-size: 12px;
  color: var(--accent);
  padding: 11px 14px;
  border: 1px solid var(--accent);
  border-radius: var(--radius);
  width: 100%;
}
.reg-pending .spin,
.tray .spin {
  display: inline-block;
  animation: spin 1s linear infinite;
}
.tray .btn + .hint + .btn {
  margin-top: 10px;
}
@keyframes spin {
  to { transform: rotate(360deg); }
}
.social-col {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.social-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  flex-wrap: wrap;
}
.social-tabs {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
}
.st-tab {
  font-size: 11px;
  padding: 5px 10px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--text-dim);
  cursor: pointer;
  transition: border-color 0.15s ease, color 0.15s ease;
}
.st-tab:hover { color: var(--accent); }
.st-tab.active {
  color: var(--accent);
  border-color: var(--accent);
}
.people-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(86px, 1fr));
  gap: 10px;
}
.person {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  padding: 10px 6px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  cursor: pointer;
  transition: border-color 0.15s ease, transform 0.15s ease;
}
.person:hover {
  border-color: var(--accent);
  transform: translateY(-2px);
}
.person-name {
  font-size: 10px;
  color: var(--text);
  word-break: break-all;
  text-align: center;
}
.people-empty {
  font-size: 12px;
  padding: 14px;
  text-align: center;
}


.domains-block {
  align-self: stretch;
  width: 100%;
  margin-top: 14px;
  padding: 14px 12px;
  border: 1px solid rgba(34, 211, 238, 0.16);
  border-radius: 12px;
  background: rgba(15, 23, 32, 0.45);
}
.db-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 10px;
}
.db-title {
  font-size: 11px;
  letter-spacing: 2px;
  color: #22d3ee;
  font-weight: 700;
}
.db-show {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11px;
  color: #94a3b8;
  cursor: pointer;
}
.db-show input {
  accent-color: #22d3ee;
  cursor: pointer;
}
.db-empty {
  font-size: 12px;
  color: #64748b;
}
.db-list {
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 0;
  padding: 0;
}
.db-item {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 8px 10px;
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.02);
  border: 1px solid rgba(255, 255, 255, 0.04);
}
.db-item.link .db-row {
  cursor: pointer;
}
.db-item.link:hover {
  border-color: rgba(34, 211, 238, 0.45);
  background: rgba(34, 211, 238, 0.07);
}
.db-item.renewing {
  border-color: rgba(34, 211, 238, 0.55);
  background: rgba(34, 211, 238, 0.06);
}
.db-row {
  display: flex;
  align-items: center;
  gap: 9px;
}
.db-exp {
  margin-left: auto;
  font-family: ui-monospace, monospace;
  font-size: 11px;
  color: #64748b;
  white-space: nowrap;
}
.db-exp.warn {
  color: #e7c14b;
}
.db-exp.danger {
  color: #ff6a6a;
}
.db-renew {
  padding: 3px 10px;
  border-radius: 999px;
  border: 1px solid #22d3ee;
  background: transparent;
  color: #22d3ee;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.04em;
  cursor: pointer;
  transition: background-color 0.15s, color 0.15s;
}
.db-renew:hover {
  background: #22d3ee;
  color: #04131a;
}
.db-renew-form {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px 12px;
  border: 1px solid rgba(34, 211, 238, 0.45);
  border-radius: 8px;
  background: rgba(34, 211, 238, 0.05);
}
.db-renew-q {
  margin: 0;
  font-size: 12px;
  color: #e2e8f0;
}
.db-renew-bal {
  margin: 0;
  font-size: 11px;
  color: #94a3b8;
}
.db-renew-bal.low {
  color: #ff8a8a;
}
.db-renew-warn {
  color: #ff6a6a;
  font-weight: 700;
}
.db-renew-confirm {
  padding: 9px 14px;
  border-radius: 8px;
  border: 1px solid #22d3ee;
  background: #22d3ee;
  color: #04131a;
  font-size: 12px;
  font-weight: 800;
  letter-spacing: 0.05em;
  cursor: pointer;
  transition: opacity 0.15s;
}
.db-renew-confirm:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.db-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #475569;
  flex-shrink: 0;
}
.db-dot.on {
  background: #34d399;
  box-shadow: 0 0 6px rgba(52, 211, 153, 0.6);
}
.db-name {
  font-family: ui-monospace, monospace;
  font-size: 13px;
  color: #e2e8f0;
}
.db-open {
  margin-left: auto;
  color: #22d3ee;
  font-size: 14px;
}

</style>
