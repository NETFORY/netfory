<script setup lang="ts">
// Отдельная страница альбома: обложка, треклист, для платного — кнопка
// «Получить альбом за N STH» (оплата напрямую владельцу, как у платных файлов).
import { ref, computed, onMounted, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { albumGet, albumList, albumBuy, albumAccess, albumSales, albumRevenue, albumSalesSeries, albumShareLink, albumExportTorrent, albumSyncKeys, albumFetchKey, fetchWallMedia, type Album } from '@/lib/bridge'
import { useAuthStore } from '@/store/auth'
import { usePlayerStore, type PlayerTrack } from '@/store/player'
import { shortAddr } from '@/lib/crypto'
import FileQrModal from '@/components/FileQrModal.vue'
import AlbumSalesSparkline from '@/components/AlbumSalesSparkline.vue'

const route = useRoute()
const router = useRouter()
const { t } = useI18n()
const auth = useAuthStore()
const player = usePlayerStore()

const album = ref<Album | null>(null)
const coverUrl = ref('')
const loading = ref(true)
const hasAccess = ref(false)
const phase = ref<'idle' | 'paying' | 'polling' | 'error'>('idle')
const message = ref('')
const sales = ref(0)
const revenue = ref(0)
const salesSeries = ref<{ ts: number; amount: number }[]>([])
const qrLink = ref('')
const qrName = ref('')
const magnetBusy = ref(false)
const confirmOpen = ref(false)
const otherAlbums = ref<Album[]>([])
const otherCovers = ref<Record<string, string>>({})

const id = computed(() => (route.params.id as string) || '')
const isOwner = computed(() => !!album.value && album.value.owner === auth.address)
const isPaid = computed(() => !!album.value && !album.value.free && (album.value.price || 0) > 0)
// E2EE: для зашифрованного альбома покупателю нужен распечатанный ключ.
const keyReady = ref(false)
const keyMsg = ref('')
const isEncrypted = computed(() => !!album.value?.encrypted)
const canPlay = computed(
  () =>
    !!album.value &&
    (!isPaid.value || isOwner.value || (hasAccess.value && (!isEncrypted.value || keyReady.value))),
)

/** Покупатель зашифрованного альбома: получить свой ECIES-ключ (ретраи UI). */
async function ensureKey() {
  if (!isEncrypted.value || isOwner.value || !hasAccess.value || keyReady.value) return
  keyMsg.value = ''
  try {
    keyReady.value = await albumFetchKey(id.value)
  } catch (e: any) {
    keyMsg.value = typeof e === 'string' ? e : e?.message || 'Ключ пока недоступен'
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

async function load() {
  loading.value = true
  album.value = await albumGet(id.value)
  if (album.value?.cover) {
    try {
      coverUrl.value = (await fetchWallMedia(album.value.cover, album.value.node || '')) || ''
    } catch {
      /* плейсхолдер */
    }
  }
  hasAccess.value = await albumAccess(id.value)
  if (album.value && !album.value.free) {
    sales.value = await albumSales(id.value)
    // доход показываем только автору
    if (album.value.owner === auth.address) {
      revenue.value = await albumRevenue(id.value)
      salesSeries.value = await albumSalesSeries(id.value)
    }
  }
  // Другие альбомы автора (для левой колонки).
  if (album.value) {
    const list = await albumList(album.value.owner)
    otherAlbums.value = list.filter((a) => a.id !== id.value)
    for (const a of otherAlbums.value) {
      if (a.cover && !otherCovers.value[a.cover]) {
        try {
          const u = await fetchWallMedia(a.cover, a.node || '')
          if (u) otherCovers.value[a.cover] = u
        } catch {
          /* плейсхолдер */
        }
      }
    }
  }
  loading.value = false
}

function openOther(a: Album) {
  router.push({ name: 'album', params: { id: a.id } })
}

async function share() {
  try {
    const link = await albumShareLink(id.value)
    qrName.value = album.value?.title || ''
    qrLink.value = link
  } catch (e: any) {
    message.value = `${e?.message || e}`
  }
}

async function exportMagnet() {
  magnetBusy.value = true
  message.value = ''
  try {
    const magnet = await albumExportTorrent(id.value)
    qrName.value = album.value?.title || ''
    qrLink.value = magnet
  } catch (e: any) {
    message.value = `${t('music.magnetFailed')}: ${e?.message || e}`
  } finally {
    magnetBusy.value = false
  }
}

function toTracks(): PlayerTrack[] {
  if (!album.value) return []
  return album.value.tracks.map((tk) => ({
    cid: tk.cid,
    name: tk.title,
    title: tk.title,
    artist: tk.artist || album.value!.artist,
    mime: tk.mime,
    node: tk.node,
    cover: tk.cover || album.value!.cover,
  }))
}
// Бесплатные ~30-сек превью платных треков (играются до покупки).
const hasPreviews = computed(() => !!album.value?.tracks.some((tk) => tk.preview))
function toPreviewTracks(): PlayerTrack[] {
  if (!album.value) return []
  return album.value.tracks
    .filter((tk) => tk.preview)
    .map((tk) => ({
      cid: tk.preview as string,
      name: `${tk.title} (превью)`,
      title: `${tk.title} (превью)`,
      artist: tk.artist || album.value!.artist,
      mime: tk.mime,
      node: tk.node,
      cover: tk.cover || album.value!.cover,
    }))
}
function playPreviews() {
  player.playTracks(toPreviewTracks(), 0)
}
function playAll() {
  player.playTracks(toTracks(), 0)
}
function playFrom(i: number) {
  const tk = album.value?.tracks[i]
  if (tk && player.current?.cid === tk.cid) {
    player.toggle()
    return
  }
  player.playTracks(toTracks(), i)
}
function isCurrent(tk: { cid: string }): boolean {
  return player.current?.cid === tk.cid
}
function isPlayingTrack(tk: { cid: string }): boolean {
  return player.current?.cid === tk.cid && player.playing
}

async function buy() {
  confirmOpen.value = true
}
async function confirmBuy() {
  confirmOpen.value = false
  phase.value = 'paying'
  message.value = ''
  try {
    await albumBuy(id.value)
  } catch (e: any) {
    phase.value = 'error'
    message.value = `${t('music.payFailed')}: ${e?.message || e}`
    return
  }
  // Оплата ушла напрямую владельцу → разблокируем сразу и оптимистично +1 к продажам.
  hasAccess.value = true
  sales.value += 1
  phase.value = 'idle'
  message.value = t('music.paid')
  // E2EE: сразу пробуем получить ключ (владелец мог ещё не выпустить — ретрай кнопкой).
  if (isEncrypted.value) ensureKey()
}

onMounted(load)
watch(() => id.value, () => {
  otherAlbums.value = []
  load()
})
</script>

<template>
  <div class="album-view" data-testid="album-view">
    <button class="back" @click="router.back()" data-testid="album-back">‹ {{ t('music.back') }}</button>

    <div v-if="loading" class="state">{{ t('music.loading') }}</div>
    <div v-else-if="!album" class="state" data-testid="album-not-found">{{ t('music.notFound') }}</div>

    <div v-else class="album-body">
      <aside v-if="otherAlbums.length" class="other-col" data-testid="album-other">
        <span class="oc-title">{{ t('music.otherAlbums') }}</span>
        <button
          v-for="a in otherAlbums"
          :key="a.id"
          class="oc-item"
          @click="openOther(a)"
          :data-testid="`album-other-${a.id}`"
        >
          <span class="oc-cover">
            <img v-if="a.cover && otherCovers[a.cover]" :src="otherCovers[a.cover]" alt="" />
            <svg v-else viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true"><path d="M9 18V5l10-2v13" /><circle cx="6" cy="18" r="3" /><circle cx="16" cy="16" r="3" /></svg>
          </span>
          <span class="oc-meta">
            <span class="oc-name">{{ a.title }}</span>
            <span class="oc-year">{{ a.year || '—' }}<template v-if="!a.free"> · {{ a.price }} STH</template></span>
          </span>
        </button>
      </aside>

      <div class="album-main">
        <div class="album-hero">
      <div class="cover">
        <img v-if="coverUrl" :src="coverUrl" alt="" />
        <svg v-else viewBox="0 0 24 24" width="64" height="64" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M9 18V5l10-2v13" />
          <circle cx="6" cy="18" r="3" />
          <circle cx="16" cy="16" r="3" />
        </svg>
      </div>

      <div class="info">
        <span v-if="isPaid" class="badge paid" data-testid="album-price-badge">{{ album.price }} STH</span>
        <span v-else class="badge free">FREE</span>
        <h1 class="a-title" data-testid="album-title">{{ album.title }}</h1>
        <p class="a-artist">{{ album.artist && album.artist.length > 22 ? shortAddr(album.artist) : album.artist }}</p>
        <p class="a-sub">{{ album.tracks.length }} {{ t('music.tracks') }}<template v-if="album.year"> · {{ album.year }}</template> · {{ t('music.by') }} {{ shortAddr(album.owner) }}</p>
        <p v-if="isPaid" class="a-sales" data-testid="album-sales">{{ t('music.sold', { n: sales }) }}</p>
        <p v-if="isPaid && isOwner" class="a-revenue" data-testid="album-revenue">💰 {{ t('music.revenue', { n: revenue }) }}</p>
        <p v-if="album.note" class="a-note" data-testid="album-note">{{ album.note }}</p>

        <div class="a-actions">
          <button v-if="canPlay" class="cta play" @click="playAll" data-testid="album-listen">▶ {{ t('music.playAll') }}</button>
          <button
            v-if="!canPlay && hasPreviews"
            class="cta preview"
            @click="playPreviews"
            data-testid="album-preview"
          >▶ Слушать превью (30 сек)</button>
          <template v-if="!canPlay && hasAccess && isEncrypted && !keyReady">
            <button class="cta buy" @click="ensureKey" data-testid="album-fetch-key">
              ⚿ Получить ключ E2EE
            </button>
          </template>
          <template v-else-if="!canPlay && !hasAccess">
            <button
              v-if="phase === 'idle' || phase === 'error'"
              class="cta buy"
              @click="buy"
              data-testid="album-buy"
            >{{ t('music.getFor', { price: album.price }) }}</button>
            <button v-else class="cta busy" disabled data-testid="album-buying">
              <span class="spin"></span>
              {{ phase === 'paying' ? t('music.sendingPayment') : t('music.checkingPayment') }}
            </button>
          </template>
        </div>
        <p v-if="keyMsg" class="a-msg" data-testid="album-key-message">{{ keyMsg }}</p>
        <p v-if="isEncrypted" class="a-e2ee dim" data-testid="album-e2ee-badge">
          🔐 Строгое E2EE: треки зашифрованы AES-256, ключ выдаётся персонально каждому покупателю
        </p>

        <div class="a-share">
          <button class="share-btn" @click="share" data-testid="album-share">🔗 {{ t('music.share') }}</button>
          <button
            v-if="album.free"
            class="share-btn"
            @click="exportMagnet"
            :disabled="magnetBusy"
            data-testid="album-magnet"
          >🧲 {{ magnetBusy ? t('music.magnetBusy') : t('music.magnet') }}</button>
        </div>
        <p v-if="message" class="a-msg" :class="{ ok: phase === 'idle' && hasAccess }" data-testid="album-message">{{ message }}</p>

        <AlbumSalesSparkline v-if="isPaid && isOwner" :points="salesSeries" />
      </div>
    </div>

    <ul v-if="album" class="a-tracks" data-testid="album-view-tracks">
      <li
        v-for="(tk, i) in album.tracks"
        :key="tk.cid"
        class="a-track"
        :class="{ locked: !canPlay, on: isCurrent(tk) }"
        @click="canPlay && playFrom(i)"
        :data-testid="`album-view-track-${i}`"
      >
        <span class="tn">{{ i + 1 }}</span>
        <span class="tp">{{ !canPlay ? '🔒' : (isPlayingTrack(tk) ? '❚❚' : '▶') }}</span>
        <span class="tt">{{ tk.title }}</span>
      </li>
    </ul>
      </div>
    </div>

    <FileQrModal v-if="qrLink" :link="qrLink" :name="qrName" @close="qrLink = ''" />

    <div v-if="confirmOpen" class="confirm-backdrop" @click.self="confirmOpen = false" data-testid="album-confirm">
      <div class="confirm-box">
        <h3 class="cf-title">{{ t('music.confirmTitle') }}</h3>
        <p class="cf-msg">{{ t('music.confirmMsg', { price: album?.price, title: album?.title }) }}</p>
        <div class="cf-actions">
          <button class="cf-btn ghost" @click="confirmOpen = false" data-testid="album-confirm-cancel">{{ t('music.cancel') }}</button>
          <button class="cf-btn primary" @click="confirmBuy" data-testid="album-confirm-ok">{{ t('music.confirmPay') }}</button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.album-view {
  padding: 28px 32px;
  max-width: 1080px;
  margin: 0 auto;
  height: 100%;
  overflow-y: auto;
}
.album-body {
  display: flex;
  gap: 28px;
  align-items: flex-start;
}
.other-col {
  flex: 0 0 220px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  position: sticky;
  top: 0;
}
.oc-title {
  font-size: 11px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: #00e5ff;
  font-weight: 700;
  margin-bottom: 4px;
}
.oc-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 7px;
  border-radius: 10px;
  border: 1px solid transparent;
  background: rgba(255, 255, 255, 0.03);
  cursor: pointer;
  text-align: left;
  transition: border-color 0.15s ease, background-color 0.15s ease;
}
.oc-item:hover {
  border-color: rgba(0, 229, 255, 0.35);
  background: rgba(0, 229, 255, 0.06);
}
.oc-cover {
  width: 42px;
  height: 42px;
  flex: 0 0 auto;
  border-radius: 7px;
  overflow: hidden;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #06121e;
  background: linear-gradient(135deg, #00e5ff, #a855f7);
}
.oc-cover img { width: 100%; height: 100%; object-fit: cover; display: block; }
.oc-meta { display: flex; flex-direction: column; min-width: 0; }
.oc-name { font-size: 12px; font-weight: 700; color: #eaf6ff; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.oc-year { font-size: 11px; color: #6f96ad; }
.album-main { flex: 1; min-width: 0; }
.back {
  background: none;
  border: none;
  color: #7fb4cf;
  cursor: pointer;
  font-size: 13px;
  margin-bottom: 20px;
}
.back:hover {
  color: #00e5ff;
}
.state {
  color: #6f96ad;
  text-align: center;
  padding: 60px 0;
}
.album-hero {
  display: flex;
  gap: 26px;
  align-items: flex-start;
  margin-bottom: 26px;
}
.cover {
  width: 220px;
  height: 220px;
  flex: 0 0 auto;
  border-radius: 14px;
  overflow: hidden;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #06121e;
  background: linear-gradient(135deg, #00e5ff, #a855f7);
  box-shadow: 0 0 30px rgba(0, 229, 255, 0.3);
}
.cover img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}
.info {
  display: flex;
  flex-direction: column;
  min-width: 0;
  padding-top: 6px;
}
.badge {
  align-self: flex-start;
  font-size: 11px;
  font-weight: 700;
  padding: 4px 10px;
  border-radius: 999px;
  margin-bottom: 10px;
}
.badge.paid {
  background: rgba(217, 70, 239, 0.16);
  color: #e879f9;
}
.badge.free {
  background: rgba(34, 197, 94, 0.16);
  color: #4ade80;
}
.a-title {
  font-size: 30px;
  font-weight: 800;
  color: #eaf6ff;
  margin: 0;
}
.a-artist {
  font-size: 15px;
  color: #00e5ff;
  margin: 6px 0 0;
}
.a-sub {
  font-size: 12px;
  color: #6f96ad;
  margin: 4px 0 0;
}
.a-sales {
  font-size: 12px;
  color: #4ade80;
  margin: 6px 0 0;
  font-weight: 600;
}
.a-revenue {
  font-size: 13px;
  color: #e879f9;
  margin: 4px 0 0;
  font-weight: 700;
  text-shadow: 0 0 10px rgba(232, 121, 249, 0.35);
}
.a-note {
  font-size: 13px;
  color: #b9d6e6;
  line-height: 1.55;
  margin: 12px 0 0;
  white-space: pre-wrap;
  max-width: 520px;
}
.a-share {
  display: flex;
  gap: 10px;
  margin-top: 14px;
}
.share-btn {
  padding: 8px 16px;
  border-radius: 999px;
  border: 1px solid rgba(0, 229, 255, 0.3);
  background: transparent;
  color: #b9d6e6;
  font-size: 12px;
  cursor: pointer;
  transition: all 0.15s ease;
}
.share-btn:hover {
  border-color: #00e5ff;
  color: #00e5ff;
}
.share-btn:disabled {
  opacity: 0.6;
  cursor: default;
}
.a-actions {
  margin-top: 20px;
}
.cta {
  padding: 12px 26px;
  border-radius: 999px;
  border: none;
  font-size: 13px;
  font-weight: 700;
  cursor: pointer;
  color: #06121e;
  background: linear-gradient(135deg, #00e5ff, #a855f7);
  display: inline-flex;
  align-items: center;
  gap: 10px;
  box-shadow: 0 0 18px rgba(0, 229, 255, 0.3);
}
.cta.buy {
  background: linear-gradient(135deg, #e879f9, #a855f7);
}
.cta.preview {
  background: transparent;
  color: #00e5ff;
  border: 1px solid rgba(0, 229, 255, 0.45);
  box-shadow: none;
}
.cta.busy {
  opacity: 0.7;
  cursor: default;
}
.spin {
  width: 13px;
  height: 13px;
  border: 2px solid rgba(6, 18, 30, 0.4);
  border-top-color: #06121e;
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}
.a-msg {
  margin-top: 12px;
  font-size: 12px;
  color: #ff9d9d;
}
.a-msg.ok {
  color: #4ade80;
}
.a-e2ee {
  margin-top: 10px;
  font-size: 11px;
}
.confirm-backdrop {
  position: fixed;
  inset: 0;
  z-index: 120;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(4, 8, 16, 0.72);
  backdrop-filter: blur(6px);
}
.confirm-box {
  width: 380px;
  max-width: 92vw;
  padding: 22px;
  border-radius: 14px;
  background: rgba(12, 17, 28, 0.98);
  border: 1px solid rgba(0, 229, 255, 0.3);
  box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6);
}
.cf-title {
  margin: 0 0 10px;
  font-size: 17px;
  color: #eaf6ff;
}
.cf-msg {
  margin: 0 0 20px;
  font-size: 13px;
  color: #b9d6e6;
  line-height: 1.5;
}
.cf-actions {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
}
.cf-btn {
  padding: 9px 18px;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 700;
  cursor: pointer;
  border: 1px solid transparent;
}
.cf-btn.ghost {
  background: transparent;
  border-color: rgba(255, 255, 255, 0.16);
  color: #b9d6e6;
}
.cf-btn.primary {
  background: linear-gradient(135deg, #e879f9, #a855f7);
  color: #06121e;
}
.a-tracks {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.a-track {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 11px 14px;
  border-radius: 10px;
  cursor: pointer;
  transition: background-color 0.15s ease;
}
.a-track:hover {
  background: rgba(255, 255, 255, 0.05);
}
.a-track.locked {
  cursor: default;
  opacity: 0.7;
}
.a-track.on {
  background: rgba(0, 229, 255, 0.1);
  box-shadow: inset 0 0 0 1px rgba(0, 229, 255, 0.35);
}
.a-track.on .tt {
  color: #00e5ff;
  font-weight: 700;
  text-shadow: 0 0 10px rgba(0, 229, 255, 0.55);
}
.a-track.on .tp,
.a-track.on .tn {
  color: #00e5ff;
  text-shadow: 0 0 8px rgba(0, 229, 255, 0.6);
}
.tn {
  width: 22px;
  font-size: 12px;
  color: #5f7d90;
  text-align: right;
}
.tp {
  width: 18px;
  font-size: 12px;
  color: #00e5ff;
}
.tt {
  flex: 1;
  font-size: 13px;
  color: #d7ecf7;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
