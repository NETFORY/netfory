<script setup lang="ts">
/* eslint-disable no-undef */
// Карточка альбома: обложка, тайтл/артист, список треков, «Слушать всё».
// Воспроизведение — через глобальный нижний плеер.
import { ref, computed, onMounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { fetchWallMedia, albumShareLink, albumAccess, albumDelete, playlistAdd, type Album, type AlbumTrack } from '@/lib/bridge'
import { usePlayerStore, type PlayerTrack } from '@/store/player'
import { useAuthStore } from '@/store/auth'
import { useToastsStore } from '@/store/toasts'
import { shortAddr } from '@/lib/crypto'
import FileQrModal from '@/components/FileQrModal.vue'

const props = defineProps<{ album: Album }>()
const emit = defineEmits<{ (e: 'edit', album: Album): void; (e: 'deleted', id: string): void }>()
const { t } = useI18n()
const player = usePlayerStore()
const auth = useAuthStore()
const router = useRouter()
const toasts = useToastsStore()

const coverUrl = ref('')
const expanded = ref(false)
const qrLink = ref('')
const owned = ref(false)
const addingLib = ref(false)
const confirmDel = ref(false)
const deleting = ref(false)

async function doDelete() {
  deleting.value = true
  try {
    await albumDelete(props.album.id)
    toasts.push(t('music.albumDeleted'), '', 'info')
    emit('deleted', props.album.id)
  } catch (e: any) {
    toasts.push(t('music.deleteFailed'), `${e?.message || e}`, 'error')
  } finally {
    deleting.value = false
    confirmDel.value = false
  }
}

async function share() {
  try {
    qrLink.value = await albumShareLink(props.album.id)
  } catch {
    /* манифест ещё не готов */
  }
}

const isOwner = computed(() => props.album.owner === auth.address)
const isPaid = computed(() => !props.album.free && (props.album.price || 0) > 0)
const locked = computed(() => isPaid.value && !isOwner.value && !owned.value)

function openPage() {
  router.push({ name: 'album', params: { id: props.album.id } })
}

// Добавить бесплатный альбом «к себе» — все треки в личный плейлист.
async function addToLibrary() {
  addingLib.value = true
  try {
    for (const tk of props.album.tracks) {
      await playlistAdd({
        cid: tk.cid,
        name: tk.title,
        title: tk.title,
        artist: tk.artist || props.album.artist,
        size: tk.size,
        mime: tk.mime,
        node: tk.node,
        cover: tk.cover || props.album.cover,
      })
    }
    window.dispatchEvent(new Event('playlist-changed'))
    toasts.push(t('music.addedToLibrary'), '', 'info')
  } catch (e: any) {
    toasts.push(t('music.addFailed'), `${e?.message || e}`, 'error')
  } finally {
    addingLib.value = false
  }
}

async function loadCover() {
  coverUrl.value = ''
  if (!props.album.cover) return
  try {
    const url = await fetchWallMedia(props.album.cover, props.album.node || '')
    if (url) coverUrl.value = url
  } catch {
    /* обложка недоступна — плейсхолдер */
  }
}

function toTracks(): PlayerTrack[] {
  return props.album.tracks.map((tk: AlbumTrack) => ({
    cid: tk.cid,
    name: tk.title,
    title: tk.title,
    artist: tk.artist || props.album.artist,
    mime: tk.mime,
    node: tk.node,
    cover: tk.cover || props.album.cover,
  }))
}

function playAll() {
  if (locked.value) return openPage()
  player.playTracks(toTracks(), 0)
}
function playFrom(i: number) {
  if (locked.value) return openPage()
  player.playTracks(toTracks(), i)
}
function isCurrent(tk: AlbumTrack): boolean {
  return player.current?.cid === tk.cid && player.playing
}

async function checkOwned() {
  if (!isPaid.value || isOwner.value) {
    owned.value = true
    return
  }
  owned.value = await albumAccess(props.album.id)
}

onMounted(() => {
  loadCover()
  checkOwned()
})
watch(
  () => props.album.id,
  () => {
    loadCover()
    checkOwned()
  },
)
</script>

<template>
  <div class="album-card" data-testid="album-card">
    <div class="ac-head">
      <div class="ac-cover" @click="playAll" :title="t('music.playAll')">
        <img v-if="coverUrl" :src="coverUrl" alt="" />
        <svg v-else viewBox="0 0 24 24" width="40" height="40" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M9 18V5l10-2v13" />
          <circle cx="6" cy="18" r="3" />
          <circle cx="16" cy="16" r="3" />
        </svg>
        <span class="ac-play-overlay">▶</span>
      </div>
      <div class="ac-meta">
        <span class="ac-title" :title="album.title" @click="openPage">{{ album.title }}</span>
        <span class="ac-artist">{{ album.artist ? (album.artist.length > 20 ? shortAddr(album.artist) : album.artist) : shortAddr(album.owner) }}</span>
        <span class="ac-sub">
          {{ album.tracks.length }} {{ t('music.tracks') }}<template v-if="album.year"> · {{ album.year }}</template> ·
          <span v-if="isPaid" class="price-tag">{{ album.price }} STH</span>
          <span v-else>{{ t('music.free') }}</span>
          <span v-if="isPaid && owned && !isOwner" class="owned-tag"> · ✓ {{ t('music.owned') }}</span>
        </span>
        <div class="ac-actions">
          <button v-if="locked" class="ac-btn primary" @click="openPage" data-testid="album-open-paid">{{ t('music.getFor', { price: album.price }) }}</button>
          <button v-else class="ac-btn primary" @click="playAll" data-testid="album-play-all">▶ {{ t('music.playAll') }}</button>
          <button
            v-if="album.free && !isOwner"
            class="ac-btn"
            @click="addToLibrary"
            :disabled="addingLib"
            data-testid="album-add-library"
          >{{ addingLib ? '…' : `＋ ${t('music.addToMe')}` }}</button>
          <button class="ac-btn" @click="expanded = !expanded" data-testid="album-toggle-tracks">{{ expanded ? t('music.hide') : t('music.tracklist') }}</button>
          <button class="ac-btn icon" @click="share" :title="t('music.share')" data-testid="album-share-card">🔗</button>
          <template v-if="isOwner">
            <button class="ac-btn" @click="emit('edit', album)" data-testid="album-edit">✎ {{ t('music.edit') }}</button>
            <button v-if="!confirmDel" class="ac-btn danger" @click="confirmDel = true" data-testid="album-delete">🗑 {{ t('music.delete') }}</button>
            <template v-else>
              <button class="ac-btn danger" :disabled="deleting" @click="doDelete" data-testid="album-delete-confirm">{{ deleting ? '…' : t('music.confirmDelete') }}</button>
              <button class="ac-btn" @click="confirmDel = false" data-testid="album-delete-cancel">{{ t('music.cancel') }}</button>
            </template>
          </template>
        </div>
      </div>
    </div>

    <ul v-if="expanded" class="ac-tracks" data-testid="album-tracklist">
      <li
        v-for="(tk, i) in album.tracks"
        :key="tk.cid"
        class="ac-track"
        :class="{ on: isCurrent(tk) }"
        @click="playFrom(i)"
        :data-testid="`album-track-${tk.cid}`"
      >
        <span class="ac-tn">{{ i + 1 }}</span>
        <span class="ac-play">{{ isCurrent(tk) ? '❚❚' : '▶' }}</span>
        <span class="ac-tt" :title="tk.title">{{ tk.title }}</span>
      </li>
    </ul>

    <FileQrModal v-if="qrLink" :link="qrLink" :name="album.title" @close="qrLink = ''" />
  </div>
</template>

<style scoped>
.album-card {
  background: light-dark(var(--panel), rgba(255, 255, 255, 0.03));
  border: 1px solid rgba(0, 229, 255, 0.16);
  border-radius: 12px;
  padding: 14px;
  transition: border-color 0.15s ease, transform 0.15s ease;
}
.album-card:hover {
  border-color: rgba(0, 229, 255, 0.4);
}
.ac-head {
  display: flex;
  gap: 14px;
}
.ac-cover {
  position: relative;
  width: 96px;
  height: 96px;
  flex: 0 0 auto;
  border-radius: 10px;
  overflow: hidden;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  color: light-dark(#ffffff, #06121e);
  background: linear-gradient(135deg, light-dark(#0891b2, #00e5ff), light-dark(#7e22ce, #a855f7));
  box-shadow: 0 0 16px rgba(0, 229, 255, 0.3);
}
.ac-cover img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}
.ac-play-overlay {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 26px;
  color: #fff;
  background: rgba(0, 0, 0, 0.35);
  opacity: 0;
  transition: opacity 0.15s ease;
}
.ac-cover:hover .ac-play-overlay {
  opacity: 1;
}
.ac-meta {
  display: flex;
  flex-direction: column;
  min-width: 0;
  gap: 3px;
}
.ac-title {
  font-size: 15px;
  font-weight: 700;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  cursor: pointer;
}
.ac-title:hover {
  color: light-dark(#0891b2, #00e5ff);
}
.price-tag {
  color: light-dark(#a21caf, #e879f9);
  font-weight: 700;
}
.owned-tag {
  color: light-dark(#15803d, #4ade80);
  font-weight: 700;
}
.ac-artist {
  font-size: 12px;
  color: light-dark(#0891b2, #00e5ff);
}
.ac-sub {
  font-size: 11px;
  color: var(--text-dim);
}
.ac-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 8px;
}
.ac-btn {
  padding: 6px 12px;
  font-size: 11px;
  border-radius: 999px;
  border: 1px solid rgba(0, 229, 255, 0.3);
  background: transparent;
  color: var(--text-dim);
  cursor: pointer;
  white-space: nowrap;
  transition: all 0.15s ease;
}
.ac-btn:hover {
  border-color: light-dark(#0891b2, #00e5ff);
  color: light-dark(#0891b2, #00e5ff);
}
.ac-btn.primary {
  background: linear-gradient(135deg, light-dark(#0891b2, #00e5ff), light-dark(#7e22ce, #a855f7));
  color: light-dark(#ffffff, #06121e);
  border: none;
  font-weight: 700;
}
.ac-btn.icon {
  padding: 6px 10px;
}
.ac-btn.danger {
  border-color: rgba(255, 107, 107, 0.4);
  color: light-dark(#dc2626, #ff9d9d);
}
.ac-btn.danger:hover {
  border-color: light-dark(#dc2626, #ff6b6b);
  color: light-dark(#dc2626, #ff6b6b);
}
.ac-tracks {
  list-style: none;
  margin: 12px 0 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.ac-track {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 7px 9px;
  border-radius: 8px;
  cursor: pointer;
  transition: background-color 0.15s ease;
}
.ac-track:hover {
  background: light-dark(rgba(0, 0, 0, 0.05), rgba(255, 255, 255, 0.05));
}
.ac-track.on {
  background: rgba(0, 229, 255, 0.1);
}
.ac-tn {
  width: 18px;
  font-size: 11px;
  color: var(--text-dim);
  text-align: right;
}
.ac-play {
  width: 16px;
  font-size: 10px;
  color: light-dark(#0891b2, #00e5ff);
}
.ac-tt {
  flex: 1;
  font-size: 12px;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
