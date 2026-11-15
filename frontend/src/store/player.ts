// Глобальный музыкальный плеер (Web3 Spotify/Bandcamp).
// Состояние вынесено в Pinia, чтобы воспроизведение НЕ прерывалось при
// навигации между роутами — сам <audio> живёт в BottomAudioPlayer.vue,
// смонтированном один раз в корне AppLayout.
import { defineStore } from 'pinia'

export interface PlayerTrack {
  cid: string
  name: string
  title?: string
  artist?: string
  mime?: string
  node?: string
  cover?: string
}

export function trackLabel(tk: PlayerTrack): string {
  const title = (tk.title || '').trim()
  const artist = (tk.artist || '').trim()
  if (title && artist) return `${artist} — ${title}`
  if (title) return title
  return tk.name
}

export const usePlayerStore = defineStore('player', {
  state: () => ({
    tracks: [] as PlayerTrack[],
    index: -1,
    playing: false,
    currentTime: 0,
    duration: 0,
    volume: 0.9,
    queueOpen: false,
    shuffle: false,
    repeat: 'off' as 'off' | 'all' | 'one',
    // счётчик — увеличивается всякий раз, когда нужно (пере)загрузить текущий трек
    loadTick: 0,
  }),
  getters: {
    current(state): PlayerTrack | null {
      return state.index >= 0 ? state.tracks[state.index] || null : null
    },
    hasTrack(state): boolean {
      return state.index >= 0 && !!state.tracks[state.index]
    },
  },
  actions: {
    // Загрузить очередь и начать воспроизведение с указанного индекса.
    playTracks(list: PlayerTrack[], start = 0) {
      if (!list.length) return
      this.tracks = list.slice()
      this.index = Math.min(Math.max(0, start), list.length - 1)
      this.playing = true
      this.loadTick++
    },
    // Загрузить очередь БЕЗ автозапуска (предзагрузка первого трека, пауза).
    setQueue(list: PlayerTrack[]) {
      if (!list.length) return
      this.tracks = list.slice()
      this.index = 0
      this.playing = false
      this.loadTick++
    },
    playTrack(tk: PlayerTrack) {
      // тот же трек уже играет → переключаем play/pause
      if (this.current && this.current.cid === tk.cid) {
        this.toggle()
        return
      }
      this.playTracks([tk], 0)
    },
    enqueue(list: PlayerTrack[]) {
      const wasEmpty = this.tracks.length === 0
      this.tracks.push(...list)
      if (wasEmpty && this.tracks.length) {
        this.index = 0
        this.playing = true
        this.loadTick++
      }
    },
    toggle() {
      if (this.hasTrack) this.playing = !this.playing
    },
    next() {
      // повтор одного трека → проигрываем заново
      if (this.repeat === 'one') {
        this.playing = true
        this.loadTick++
        return
      }
      // перемешивание → случайный трек (не текущий)
      if (this.shuffle && this.tracks.length > 1) {
        let n = Math.floor(Math.random() * this.tracks.length)
        if (n === this.index) n = (n + 1) % this.tracks.length
        this.index = n
        this.playing = true
        this.loadTick++
        return
      }
      if (this.index < this.tracks.length - 1) {
        this.index++
        this.playing = true
        this.loadTick++
      } else if (this.repeat === 'all' && this.tracks.length) {
        // повтор всего плейлиста → с начала
        this.index = 0
        this.playing = true
        this.loadTick++
      } else {
        this.playing = false
      }
    },
    toggleShuffle() {
      this.shuffle = !this.shuffle
    },
    cycleRepeat() {
      this.repeat = this.repeat === 'off' ? 'all' : this.repeat === 'all' ? 'one' : 'off'
    },
    prev() {
      // <3с → в начало трека, иначе предыдущий
      if (this.currentTime > 3) {
        this.loadTick++
        return
      }
      if (this.index > 0) {
        this.index--
        this.playing = true
        this.loadTick++
      }
    },
    jumpTo(i: number) {
      if (i >= 0 && i < this.tracks.length) {
        this.index = i
        this.playing = true
        this.loadTick++
      }
    },
    removeAt(i: number) {
      if (i < 0 || i >= this.tracks.length) return
      this.tracks.splice(i, 1)
      if (i < this.index) {
        this.index--
      } else if (i === this.index) {
        if (this.index >= this.tracks.length) this.index = this.tracks.length - 1
        this.loadTick++
      }
      if (!this.tracks.length) {
        this.index = -1
        this.playing = false
      }
    },
    setVolume(v: number) {
      this.volume = Math.min(1, Math.max(0, v))
    },
    setTime(t: number) {
      this.currentTime = t
    },
    setDuration(d: number) {
      this.duration = d
    },
    toggleQueue() {
      this.queueOpen = !this.queueOpen
    },
    closeQueue() {
      this.queueOpen = false
    },
    clear() {
      this.tracks = []
      this.index = -1
      this.playing = false
      this.currentTime = 0
      this.duration = 0
      this.queueOpen = false
    },
  },
})
