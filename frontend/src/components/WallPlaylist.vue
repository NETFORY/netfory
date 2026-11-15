<script setup lang="ts">
/* eslint-disable no-undef */
// Персональный музыкальный плейлист пользователя (виден другим на его профиле).
// Воспроизведение идёт через глобальный нижний плеер (store/player).
import { ref, onMounted, onUnmounted, watch, nextTick } from 'vue'
import { useI18n } from 'vue-i18n'
import { playlistList, playlistRemove, playlistReorder, type PlaylistTrack } from '@/lib/bridge'
import { usePlayerStore, type PlayerTrack } from '@/store/player'

const props = defineProps<{ owner: string; isOwn: boolean; title?: string }>()
const { t } = useI18n()
const player = usePlayerStore()

const tracks = ref<PlaylistTrack[]>([])
const listEl = ref<HTMLUListElement | null>(null)
const dragIndex = ref<number | null>(null)
const overIndex = ref<number | null>(null)

// Авто-прокрутка списка к текущему играющему треку (чтобы активная песня
// всегда была видна при длинном плейлисте со внутренним скроллом).
async function scrollToCurrent() {
  const cid = player.current?.cid
  if (!cid || !listEl.value) return
  await nextTick()
  const row = listEl.value.querySelector(`[data-cid="${CSS.escape(cid)}"]`) as HTMLElement | null
  row?.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
}

function label(tk: PlaylistTrack): string {
  const title = (tk.title || '').trim()
  const artist = (tk.artist || '').trim()
  if (title && artist) return `${artist} — ${title}`
  if (title) return title
  return tk.name
}

function toPlayerTrack(tk: PlaylistTrack): PlayerTrack {
  return { cid: tk.cid, name: tk.name, title: tk.title, artist: tk.artist, mime: tk.mime, node: tk.node, cover: tk.cover }
}

async function load() {
  if (!props.owner) return
  tracks.value = await playlistList(props.owner)
}

// Загрузить весь плейлист в глобальный плеер, начиная с выбранного трека.
function play(tk: PlaylistTrack) {
  if (player.current?.cid === tk.cid) {
    player.toggle()
    return
  }
  const list = tracks.value.map(toPlayerTrack)
  const start = tracks.value.findIndex((x) => x.cid === tk.cid)
  player.playTracks(list, Math.max(0, start))
}

function isCurrent(tk: PlaylistTrack): boolean {
  return player.current?.cid === tk.cid && player.playing
}

async function remove(tk: PlaylistTrack) {
  await playlistRemove(tk.cid)
  await load()
}

/* ---- drag-and-drop reorder (own playlist only) ---- */
function onDragStart(i: number) {
  if (!props.isOwn) return
  dragIndex.value = i
}
function onDragOver(e: DragEvent, i: number) {
  if (!props.isOwn || dragIndex.value === null) return
  e.preventDefault()
  overIndex.value = i
}
async function onDrop(i: number) {
  const from = dragIndex.value
  dragIndex.value = null
  overIndex.value = null
  if (!props.isOwn || from === null || from === i) return
  const arr = [...tracks.value]
  const [moved] = arr.splice(from, 1)
  arr.splice(i, 0, moved)
  tracks.value = arr
  try {
    await playlistReorder(arr.map((t) => t.cid))
  } catch {
    /* порядок сохранится при следующем действии */
  }
}
function onDragEnd() {
  dragIndex.value = null
  overIndex.value = null
}

onMounted(() => {
  load()
  window.addEventListener('playlist-changed', load)
})
onUnmounted(() => window.removeEventListener('playlist-changed', load))
watch(() => props.owner, load)
// при смене трека (клик, next/prev, авто-переход) — подкрутить список к нему
watch(() => player.current?.cid, scrollToCurrent)
// когда список только что загрузился — сразу показать активный трек
watch(tracks, scrollToCurrent)
</script>

<template>
  <section v-if="tracks.length || isOwn" class="panel social-block playlist" data-testid="wall-playlist">
    <div class="pl-head">
      <span class="block-title">🎵 {{ title || t('wall.playlistTitle') }} <b class="sg-count">{{ tracks.length }}</b></span>
      <div class="pl-modes">
        <button
          class="pl-mode"
          :class="{ on: player.shuffle }"
          @click="player.toggleShuffle()"
          :title="t('wall.shuffle')"
          data-testid="playlist-shuffle"
        >🔀</button>
        <button
          class="pl-mode"
          :class="{ on: player.repeat !== 'off' }"
          @click="player.cycleRepeat()"
          :title="player.repeat === 'one' ? t('wall.repeatOne') : player.repeat === 'all' ? t('wall.repeatAll') : t('wall.repeatOff')"
          data-testid="playlist-repeat"
        >{{ player.repeat === 'one' ? '🔂' : '🔁' }}</button>
      </div>
    </div>

    <p v-if="!tracks.length" class="dim pl-empty">{{ t('wall.playlistEmpty') }}</p>

    <ul v-else ref="listEl" class="pl-list">
      <li
        v-for="(tk, idx) in tracks"
        :key="tk.cid"
        class="pl-row"
        :class="{ 'drag-src': dragIndex === idx, 'drag-over': overIndex === idx && dragIndex !== idx }"
        :draggable="isOwn"
        @dragstart="onDragStart(idx)"
        @dragover="onDragOver($event, idx)"
        @drop="onDrop(idx)"
        @dragend="onDragEnd"
        :data-cid="tk.cid"
        data-testid="playlist-track"
      >
        <span v-if="isOwn" class="pl-grip" :title="t('wall.dragReorder')" data-testid="playlist-drag-handle">⠿</span>
        <button
          class="pl-play"
          :class="{ on: isCurrent(tk) }"
          @click="play(tk)"
          :data-testid="`playlist-play-${tk.cid}`"
        >{{ isCurrent(tk) ? '⏸' : '▶' }}</button>
        <span class="pl-name" :title="label(tk)">{{ label(tk) }}</span>
        <button
          v-if="isOwn"
          class="pl-del"
          @click="remove(tk)"
          :data-testid="`playlist-remove-${tk.cid}`"
        >✕</button>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.playlist { display: flex; flex-direction: column; gap: 12px; }
.pl-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
.pl-modes { display: flex; gap: 6px; }
.pl-mode {
  background: rgba(255,255,255,.04); border: 1px solid var(--border); border-radius: 8px;
  width: 30px; height: 26px; font-size: 13px; cursor: pointer; opacity: .5;
  transition: opacity .15s ease, background-color .15s ease, border-color .15s ease;
}
.pl-mode:hover { opacity: .85; }
.pl-mode.on {
  opacity: 1; color: #06121e; background: var(--accent, #64b5ef);
  border-color: transparent; box-shadow: 0 0 10px rgba(100,181,239,.4);
}
.pl-empty { font-size: 13px; opacity: .55; }
.pl-list {
  list-style: none; margin: 0; padding: 0 6px 0 0; display: flex; flex-direction: column; gap: 4px;
  max-height: 460px; overflow-y: auto; overscroll-behavior: contain;
}
/* тонкий неоновый скроллбар внутри плейлиста */
.pl-list::-webkit-scrollbar { width: 8px; }
.pl-list::-webkit-scrollbar-track { background: transparent; }
.pl-list::-webkit-scrollbar-thumb {
  background: rgba(100,181,239,.28); border-radius: 8px; border: 2px solid transparent; background-clip: padding-box;
}
.pl-list::-webkit-scrollbar-thumb:hover { background: rgba(100,181,239,.5); background-clip: padding-box; }
.pl-list { scrollbar-width: thin; scrollbar-color: rgba(100,181,239,.35) transparent; }
.pl-row {
  display: flex; align-items: center; gap: 10px; padding: 6px 8px; border-radius: 9px;
  background: rgba(255,255,255,.03); transition: background-color .15s ease;
}
.pl-row:hover { background: rgba(255,255,255,.07); }
.pl-row[draggable="true"] { cursor: grab; }
.pl-row.drag-src { opacity: .45; }
.pl-row.drag-over { box-shadow: inset 0 2px 0 0 var(--accent, #64b5ef); background: rgba(100,181,239,.1); }
.pl-grip { flex: 0 0 auto; color: #6f96ad; font-size: 13px; cursor: grab; user-select: none; }
.pl-grip:active { cursor: grabbing; }
.pl-play {
  width: 30px; height: 30px; flex: 0 0 auto; border-radius: 50%; cursor: pointer;
  background: var(--accent, #64b5ef); color: #06121e; border: none; font-size: 12px;
  display: flex; align-items: center; justify-content: center;
  transition: transform .1s ease;
}
.pl-play:hover { transform: scale(1.08); }
.pl-play.on { background: #ff8a3d; }
.pl-name { flex: 1; min-width: 0; font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.pl-del { background: none; border: none; color: inherit; opacity: .4; cursor: pointer; font-size: 13px; }
.pl-del:hover { opacity: 1; color: #ff6b6b; }
</style>
