<script setup lang="ts">
// Глобальный киберпанк-плеер (низ экрана, 70px, glassmorphism, неон cyan/purple).
// <audio> + AudioContext живут здесь один раз (компонент смонтирован в корне
// AppLayout) → музыка не прерывается при навигации между роутами.
// Плюс: мини-обложки из ID3 APIC и Milkdrop-визуализатор (butterchurn) по кнопке.
import { ref, reactive, computed, watch, onMounted, onBeforeUnmount } from 'vue'
import { useI18n } from 'vue-i18n'
import { usePlayerStore, trackLabel, type PlayerTrack } from '@/store/player'
import { socialFileBytes, fetchWallMedia, playlistList } from '@/lib/bridge'
import { useAuthStore } from '@/store/auth'

const player = usePlayerStore()
const auth = useAuthStore()
const { t } = useI18n()

const audioEl = ref<HTMLAudioElement | null>(null)
const canvasEl = ref<HTMLCanvasElement | null>(null)

// Кэш objectURL по CID, чтобы не тянуть байты повторно.
const urlCache = new Map<string, string>()
const loadingTrack = ref(false)

// Обложки: data-URL по CID обложки (дедуп по альбому — один запрос на альбом).
const covers = reactive<Record<string, string>>({})
async function ensureCover(tk: PlayerTrack | null) {
  if (!tk || !tk.cover || covers[tk.cover]) return
  try {
    const url = await fetchWallMedia(tk.cover, tk.node || '')
    if (url) covers[tk.cover] = url
  } catch {
    /* обложка недоступна — покажем плейсхолдер */
  }
}
function coverUrl(tk: PlayerTrack | null): string {
  return tk && tk.cover ? covers[tk.cover] || '' : ''
}

// --- Web Audio: анализатор спектра (fftSize 128 → 64 бина) ---
let audioCtx: AudioContext | null = null
let analyser: AnalyserNode | null = null
let sourceNode: MediaElementAudioSourceNode | null = null
let freqData: Uint8Array | null = null
let rafId = 0

function ensureAudioGraph() {
  if (audioCtx || !audioEl.value) return
  try {
    const Ctx = window.AudioContext || (window as any).webkitAudioContext
    if (!Ctx) return
    audioCtx = new Ctx()
    analyser = audioCtx.createAnalyser()
    analyser.fftSize = 128
    analyser.smoothingTimeConstant = 0.8
    freqData = new Uint8Array(analyser.frequencyBinCount)
    sourceNode = audioCtx.createMediaElementSource(audioEl.value)
    sourceNode.connect(analyser)
    analyser.connect(audioCtx.destination)
  } catch {
    /* анализатор недоступен — плеер продолжит играть без визуализации */
  }
}

// Autoplay-политика webkit: AudioContext стартует в состоянии `suspended` и
// «жест пользователя» действует только синхронно. При ПЕРВОМ клике по треку
// play() вызывается уже ПОСЛЕ await-загрузки байтов → жест потерян, звука нет.
// Поэтому разблокируем граф один раз на самом раннем жесте (pointerdown/keydown),
// пока он ещё действителен — после этого play() работает и с первого клика.
let audioUnlocked = false
async function unlockAudio() {
  if (audioUnlocked) return
  ensureAudioGraph()
  if (audioCtx) {
    try {
      if (audioCtx.state === 'suspended') await audioCtx.resume()
      audioUnlocked = true
      window.removeEventListener('pointerdown', unlockAudio)
      window.removeEventListener('keydown', unlockAudio)
    } catch {
      /* повторим на следующем жесте */
    }
  }
}

function drawSpectrum() {
  rafId = requestAnimationFrame(drawSpectrum)
  const canvas = canvasEl.value
  if (!canvas) return
  const ctx = canvas.getContext('2d')
  if (!ctx) return
  const w = canvas.width
  const h = canvas.height
  ctx.clearRect(0, 0, w, h)

  const count = 64
  let bins: number[]
  if (player.playing) {
    // Играет: реальный спектр, а если анализатор в webview молчит (нули) —
    // живая синтетическая анимация, чтобы визуализатор был «включён».
    let real: number[] | null = null
    if (analyser && freqData) {
      analyser.getByteFrequencyData(freqData)
      let sum = 0
      for (let i = 0; i < freqData.length; i++) sum += freqData[i]
      if (sum > 0) real = Array.from(freqData)
    }
    if (real) {
      bins = real
    } else {
      const tsec = Date.now() / 220
      bins = Array.from({ length: count }, (_, i) =>
        44 + 70 * Math.abs(Math.sin(i * 0.5 + tsec) * Math.cos(i * 0.17 - tsec * 0.7)),
      )
    }
  } else {
    // Пауза/стоп: спокойная ровная линия.
    bins = Array.from({ length: count }, () => 3)
  }

  const cnt = bins.length
  const gap = 2
  const barW = Math.max(1, (w - gap * (cnt - 1)) / cnt)
  for (let i = 0; i < cnt; i++) {
    const v = bins[i] / 255
    const barH = Math.max(2, v * h)
    const x = i * (barW + gap)
    const grad = ctx.createLinearGradient(0, h, 0, h - barH)
    grad.addColorStop(0, 'rgba(0, 229, 255, 0.95)')
    grad.addColorStop(1, 'rgba(168, 85, 247, 0.95)')
    ctx.fillStyle = grad
    ctx.fillRect(x, h - barH, barW, barH)
  }
}

function sizeCanvas() {
  const canvas = canvasEl.value
  if (!canvas) return
  const rect = canvas.getBoundingClientRect()
  const dpr = Math.min(window.devicePixelRatio || 1, 2)
  canvas.width = Math.max(1, Math.floor(rect.width * dpr))
  canvas.height = Math.max(1, Math.floor(rect.height * dpr))
}

// --- Milkdrop-визуализатор (butterchurn) — грузится лениво по кнопке ---
const vizOpen = ref(false)
const vizCanvas = ref<HTMLCanvasElement | null>(null)
const presetName = ref('')
let visualizer: any = null
let presetKeys: string[] = []
let presetsMap: Record<string, any> = {}
let vizRaf = 0

async function initViz() {
  if (visualizer || !vizCanvas.value) return
  ensureAudioGraph()
  if (audioCtx && audioCtx.state === 'suspended') await audioCtx.resume()
  if (!audioCtx || !sourceNode) return
  const bcMod: any = await import('butterchurn')
  const bpMod: any = await import('butterchurn-presets')
  const butterchurn = bcMod.default || bcMod
  const butterchurnPresets = bpMod.default || bpMod
  presetsMap = butterchurnPresets.getPresets()
  presetKeys = Object.keys(presetsMap)
  const canvas = vizCanvas.value
  const dpr = Math.min(window.devicePixelRatio || 1, 2)
  const w = canvas.clientWidth || window.innerWidth
  const h = canvas.clientHeight || window.innerHeight - 70
  // КРИТИЧНО: явно задаём буфер отрисовки canvas (иначе WebGL рисует в 300×150 → чёрный экран).
  canvas.width = Math.floor(w * dpr)
  canvas.height = Math.floor(h * dpr)
  visualizer = butterchurn.createVisualizer(audioCtx, canvas, {
    width: w,
    height: h,
    pixelRatio: dpr,
  })
  visualizer.connectAudio(sourceNode)
  randomPreset()
}

function vizRenderLoop() {
  vizRaf = requestAnimationFrame(vizRenderLoop)
  if (visualizer && vizOpen.value) {
    try {
      visualizer.render()
    } catch {
      /* игнор кадра */
    }
  }
}

function randomPreset() {
  if (!visualizer || !presetKeys.length) return
  const key = presetKeys[Math.floor(Math.random() * presetKeys.length)]
  presetName.value = key
  visualizer.loadPreset(presetsMap[key], 1.5)
}

function resizeViz() {
  if (!visualizer || !vizCanvas.value) return
  const canvas = vizCanvas.value
  const dpr = Math.min(window.devicePixelRatio || 1, 2)
  const w = canvas.clientWidth || window.innerWidth
  const h = canvas.clientHeight || window.innerHeight - 70
  canvas.width = Math.floor(w * dpr)
  canvas.height = Math.floor(h * dpr)
  visualizer.setRendererSize(w, h)
}

async function toggleViz() {
  vizOpen.value = !vizOpen.value
  if (vizOpen.value) {
    // ждём отрисовки overlay → инициализируем на реальном размере canvas
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)))
    await initViz()
    resizeViz()
    if (!vizRaf) vizRenderLoop()
  } else {
    // закрыли: canvas удаляется из DOM (v-if) → рвём ссылку на старый визуализатор,
    // чтобы при повторном открытии он пересоздался на НОВОМ canvas (иначе чёрный экран).
    cancelAnimationFrame(vizRaf)
    vizRaf = 0
    visualizer = null
  }
}

// --- Плавающее окно визуализатора: стандартный ↔ полноэкранный размер + перетаскивание ---
const vizFull = ref(false)
const vizPos = reactive({ x: 0, y: 0 })
let vizDragging = false
let vizMoved = false
let vizDragStart = { x: 0, y: 0, px: 0, py: 0 }

async function toggleVizFull() {
  vizFull.value = !vizFull.value
  if (vizFull.value) {
    vizPos.x = 0
    vizPos.y = 0
  }
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)))
  resizeViz()
}
// Клик по canvas: если это был drag (двигали) — не переключаем размер.
function onVizCanvasClick() {
  if (vizMoved) {
    vizMoved = false
    return
  }
  toggleVizFull()
}
function onVizDragStart(e: PointerEvent) {
  if (vizFull.value) return
  vizDragging = true
  vizMoved = false
  vizDragStart = { x: e.clientX, y: e.clientY, px: vizPos.x, py: vizPos.y }
  window.addEventListener('pointermove', onVizDragMove)
  window.addEventListener('pointerup', onVizDragEnd)
}
function onVizDragMove(e: PointerEvent) {
  if (!vizDragging) return
  const dx = e.clientX - vizDragStart.x
  const dy = e.clientY - vizDragStart.y
  if (Math.abs(dx) + Math.abs(dy) > 4) vizMoved = true
  vizPos.x = vizDragStart.px + dx
  vizPos.y = vizDragStart.py + dy
}
function onVizDragEnd() {
  vizDragging = false
  window.removeEventListener('pointermove', onVizDragMove)
  window.removeEventListener('pointerup', onVizDragEnd)
}

// Достать байты аудио: сначала по сохранённому узлу; при неудаче — повтор
// (холодный старт P2P), затем — обнаружение провайдеров без фикс. узла
// (узел трека мог устареть/быть недоступен, но контент раздают другие).
async function fetchAudioBytes(tk: PlayerTrack): Promise<string> {
  let b64 = await socialFileBytes(tk.cid, tk.node || '').catch(() => '')
  if (!b64) {
    await new Promise((r) => setTimeout(r, 800))
    b64 = await socialFileBytes(tk.cid, tk.node || '').catch(() => '')
  }
  if (!b64 && (tk.node || '').trim()) {
    b64 = await socialFileBytes(tk.cid, '').catch(() => '')
  }
  return b64
}

// --- Загрузка текущего трека ---
async function loadCurrent() {
  const tk = player.current
  const audio = audioEl.value
  if (!tk || !audio) return
  ensureCover(tk)
  loadingTrack.value = true
  try {
    let url = urlCache.get(tk.cid)
    if (!url) {
      const b64 = await fetchAudioBytes(tk)
      if (!b64) {
        loadingTrack.value = false
        return
      }
      const bin = atob(b64)
      const bytes = new Uint8Array(bin.length)
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
      url = URL.createObjectURL(new Blob([bytes], { type: tk.mime || 'audio/mpeg' }))
      urlCache.set(tk.cid, url)
    }
    audio.src = url
    audio.volume = player.volume
    if (player.playing) {
      ensureAudioGraph()
      if (audioCtx && audioCtx.state === 'suspended') await audioCtx.resume()
      await audio.play().catch(() => {})
    }
  } finally {
    loadingTrack.value = false
  }
}

watch(() => player.loadTick, loadCurrent)
// Пустой плеер + открыли очередь → подгружаем персональный плейлист (без автозапуска).
watch(
  () => player.queueOpen,
  async (open) => {
    if (open && !player.tracks.length && auth.address) {
      const list = await playlistList(auth.address)
      if (list.length) {
        player.setQueue(
          list.map((tk) => ({
            cid: tk.cid,
            name: tk.name,
            title: tk.title,
            artist: tk.artist,
            mime: tk.mime,
            node: tk.node,
            cover: tk.cover,
          })),
        )
      }
    }
  },
)
watch(
  () => player.tracks.map((x) => x.cover).join(','),
  () => player.tracks.forEach((tk) => ensureCover(tk)),
)
watch(
  () => player.playing,
  async (on) => {
    const audio = audioEl.value
    if (!audio) return
    if (on) {
      ensureAudioGraph()
      if (audioCtx && audioCtx.state === 'suspended') await audioCtx.resume()
      await audio.play().catch(() => {})
    } else {
      audio.pause()
    }
  },
)
watch(
  () => player.volume,
  (v) => {
    if (audioEl.value) audioEl.value.volume = v
  },
)

function onTime() {
  if (audioEl.value) player.setTime(audioEl.value.currentTime || 0)
}
function onMeta() {
  if (audioEl.value) player.setDuration(audioEl.value.duration || 0)
}
function onEnded() {
  player.next()
}

function togglePlay() {
  player.toggle()
}
function seek(e: Event) {
  const val = Number((e.target as HTMLInputElement).value)
  if (audioEl.value) audioEl.value.currentTime = val
  player.setTime(val)
}
function changeVolume(e: Event) {
  player.setVolume(Number((e.target as HTMLInputElement).value))
}

function fmt(sec: number): string {
  if (!isFinite(sec) || sec < 0) return '0:00'
  const m = Math.floor(sec / 60)
  const s = Math.floor(sec % 60)
  return `${m}:${s.toString().padStart(2, '0')}`
}

const nowLabel = computed(() => (player.current ? trackLabel(player.current) : ''))
const nowSub = computed(() => {
  const tk = player.current
  if (!tk) return ''
  return (tk.artist || '').trim() || t('player.unknownArtist')
})

function pick(i: number) {
  // Клик по уже играющему треку — пауза/возобновление; иначе — переход к треку.
  if (i === player.index) {
    player.toggle()
  } else {
    player.jumpTo(i)
  }
}
function removeTrack(i: number) {
  player.removeAt(i)
}
function labelOf(tk: PlayerTrack): string {
  return trackLabel(tk)
}

onMounted(() => {
  sizeCanvas()
  window.addEventListener('resize', sizeCanvas)
  window.addEventListener('resize', resizeViz)
  window.addEventListener('pointerdown', unlockAudio)
  window.addEventListener('keydown', unlockAudio)
  drawSpectrum()
})
onBeforeUnmount(() => {
  window.removeEventListener('resize', sizeCanvas)
  window.removeEventListener('resize', resizeViz)
  window.removeEventListener('pointerdown', unlockAudio)
  window.removeEventListener('keydown', unlockAudio)
  cancelAnimationFrame(rafId)
  cancelAnimationFrame(vizRaf)
  urlCache.forEach((u) => URL.revokeObjectURL(u))
  urlCache.clear()
})
</script>

<template>
  <div class="bap" :class="{ 'no-bar': !player.hasTrack, 'bap-idle': !player.hasTrack && !player.queueOpen }" data-testid="bottom-audio-player">
    <!-- Milkdrop-визуализатор (butterchurn) — плавающее окно (клик = стандарт/полный экран) -->
    <Transition name="viz-fade">
      <div
        v-if="vizOpen"
        class="viz-win"
        :class="{ full: vizFull, dragging: vizDragging }"
        :style="!vizFull ? { transform: `translate(${vizPos.x}px, ${vizPos.y}px)` } : {}"
        data-testid="player-visualizer"
        @pointerdown="onVizDragStart"
      >
        <canvas ref="vizCanvas" class="viz-canvas" @click="onVizCanvasClick" :title="t('player.toggleSize')"></canvas>
        <div class="viz-toolbar">
          <span class="viz-drag" :title="t('player.drag')">⠿</span>
          <span class="viz-preset" :title="presetName">{{ presetName || 'Milkdrop' }}</span>
          <button class="viz-btn" @click.stop="randomPreset" @pointerdown.stop :title="t('player.nextPreset')" data-testid="player-viz-next">⤳</button>
          <button class="viz-btn" @click.stop="toggleVizFull" @pointerdown.stop :title="t('player.toggleSize')" data-testid="player-viz-full">{{ vizFull ? '🗗' : '⛶' }}</button>
          <button class="viz-btn viz-close" @click.stop="toggleViz" @pointerdown.stop :title="t('player.close')" data-testid="player-viz-close">✕</button>
        </div>
      </div>
    </Transition>

    <!-- Fly-out очередь (slide-up) -->
    <Transition name="queue-slide">
      <div v-if="player.queueOpen" class="queue" :style="{ bottom: player.hasTrack ? '70px' : '0px' }" data-testid="player-queue-panel">
        <div class="queue-head">
          <span class="queue-title">{{ t('player.queue') }} · {{ player.tracks.length }}</span>
          <button class="q-close" @click="player.closeQueue()" data-testid="player-queue-close">✕</button>
        </div>
        <p v-if="!player.tracks.length" class="queue-empty" data-testid="player-queue-empty">{{ t('player.emptyQueue') }}</p>
        <ul v-else class="queue-list">
          <li
            v-for="(tk, i) in player.tracks"
            :key="tk.cid + '-' + i"
            class="q-row"
            :class="{ on: i === player.index }"
            :data-testid="`player-queue-item-${tk.cid}`"
          >
            <span class="q-cover">
              <img v-if="coverUrl(tk)" :src="coverUrl(tk)" alt="" />
              <svg v-else viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true">
                <path d="M9 18V5l10-2v13" />
                <circle cx="6" cy="18" r="3" />
                <circle cx="16" cy="16" r="3" />
              </svg>
            </span>
            <button class="q-play" @click="pick(i)" :title="t('player.play')">
              {{ i === player.index && player.playing ? '❚❚' : '▶' }}
            </button>
            <span class="q-name" :title="labelOf(tk)">{{ labelOf(tk) }}</span>
            <button
              class="q-del"
              @click="removeTrack(i)"
              :data-testid="`player-queue-remove-${tk.cid}`"
              :title="t('player.remove')"
            >✕</button>
          </li>
        </ul>
      </div>
    </Transition>

    <!-- Нижняя панель 70px -->
    <div v-if="player.hasTrack" class="bar">
      <div class="now">
        <div class="cover">
          <img v-if="coverUrl(player.current)" :src="coverUrl(player.current)" alt="" data-testid="player-now-cover" />
          <svg v-else viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="12" cy="12" r="9" />
            <circle cx="12" cy="12" r="2.4" fill="currentColor" stroke="none" />
          </svg>
        </div>
        <div class="meta">
          <span class="title" :title="nowLabel" data-testid="player-now-title">{{ nowLabel }}</span>
          <span class="sub">{{ loadingTrack ? t('player.loading') : nowSub }}</span>
        </div>
      </div>

      <div class="center">
        <div class="controls">
          <button class="ctl" @click="player.prev()" :title="t('player.prev')" data-testid="player-prev">⏮</button>
          <button class="ctl play" @click="togglePlay" :title="t('player.play')" data-testid="player-play-toggle">
            {{ player.playing ? '❚❚' : '▶' }}
          </button>
          <button class="ctl" @click="player.next()" :title="t('player.next')" data-testid="player-next">⏭</button>
        </div>
        <div class="progress">
          <span class="time">{{ fmt(player.currentTime) }}</span>
          <input
            class="seek"
            type="range"
            min="0"
            :max="player.duration || 0"
            step="0.1"
            :value="player.currentTime"
            @input="seek"
            data-testid="player-seek"
          />
          <span class="time">{{ fmt(player.duration) }}</span>
        </div>
      </div>

      <div class="right">
        <canvas ref="canvasEl" class="spectrum" data-testid="player-spectrum-canvas"></canvas>
        <button
          class="ctl viz-toggle"
          :class="{ active: vizOpen }"
          @click="toggleViz"
          :title="t('player.visualizer')"
          data-testid="player-viz-toggle"
        >◉</button>
        <div class="vol">
          <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M11 5 6 9H3v6h3l5 4V5z" />
            <path d="M15.5 8.5a5 5 0 0 1 0 7" />
          </svg>
          <input
            class="vol-slider"
            type="range"
            min="0"
            max="1"
            step="0.01"
            :value="player.volume"
            @input="changeVolume"
            data-testid="player-volume"
          />
        </div>
        <button
          class="ctl queue-toggle"
          :class="{ active: player.queueOpen }"
          @click="player.toggleQueue()"
          :title="t('player.queue')"
          data-testid="player-queue-toggle"
        >☰</button>
        <button class="ctl close-btn" @click="player.clear()" :title="t('player.close')" data-testid="player-close">✕</button>
      </div>
    </div>

    <audio
      ref="audioEl"
      @timeupdate="onTime"
      @loadedmetadata="onMeta"
      @ended="onEnded"
      style="display: none"
    />
  </div>
</template>

<style scoped>
.bap {
  position: relative;
  flex: 0 0 auto;
  z-index: 40;
}
.bap.no-bar {
  height: 0;
}
.bap.bap-idle {
  height: 0;
  overflow: hidden;
}
.queue-empty {
  margin: 0;
  padding: 22px 16px;
  font-size: 12px;
  color: #6f96ad;
  text-align: center;
}
.bar {
  height: 70px;
  display: grid;
  grid-template-columns: minmax(180px, 1fr) minmax(280px, 2fr) minmax(220px, 1fr);
  align-items: center;
  gap: 18px;
  padding: 0 20px;
  background: rgba(10, 14, 24, 0.72);
  backdrop-filter: blur(14px);
  -webkit-backdrop-filter: blur(14px);
  border-top: 1px solid rgba(0, 229, 255, 0.28);
  box-shadow: 0 -8px 30px rgba(0, 0, 0, 0.45);
}
.now {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}
.cover {
  width: 46px;
  height: 46px;
  flex: 0 0 auto;
  border-radius: 10px;
  overflow: hidden;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #06121e;
  background: linear-gradient(135deg, #00e5ff, #a855f7);
  box-shadow: 0 0 14px rgba(0, 229, 255, 0.4);
}
.cover img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}
.meta {
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.title {
  font-size: 13px;
  font-weight: 700;
  color: #eaf6ff;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.sub {
  font-size: 11px;
  color: #7fb4cf;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.center {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  min-width: 0;
}
.controls {
  display: flex;
  align-items: center;
  gap: 14px;
}
.ctl {
  background: none;
  border: none;
  color: #b9d6e6;
  cursor: pointer;
  font-size: 15px;
  line-height: 1;
  transition: color 0.15s ease, transform 0.12s ease, text-shadow 0.15s ease;
}
.ctl:hover {
  color: #00e5ff;
  transform: scale(1.12);
  text-shadow: 0 0 10px rgba(0, 229, 255, 0.6);
}
.ctl.play {
  width: 38px;
  height: 38px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 14px;
  color: #06121e;
  background: linear-gradient(135deg, #00e5ff, #a855f7);
  box-shadow: 0 0 16px rgba(0, 229, 255, 0.45);
}
.ctl.play:hover {
  color: #06121e;
  transform: scale(1.08);
}
.progress {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  max-width: 460px;
}
.time {
  font-size: 10px;
  color: #6f96ad;
  font-variant-numeric: tabular-nums;
  width: 34px;
  text-align: center;
  flex: 0 0 auto;
}
.seek {
  flex: 1;
  min-width: 0;
}
.right {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 12px;
}
.spectrum {
  width: 90px;
  height: 34px;
  border-radius: 6px;
}
.viz-toggle.active {
  color: #22c55e;
  text-shadow: 0 0 10px rgba(34, 197, 94, 0.7);
}
.vol {
  display: flex;
  align-items: center;
  gap: 6px;
  color: #8fb6cc;
}
.vol-slider {
  width: 64px;
}
.queue-toggle.active {
  color: #a855f7;
  text-shadow: 0 0 10px rgba(168, 85, 247, 0.6);
}
.close-btn:hover {
  color: #ff6b6b;
  text-shadow: 0 0 10px rgba(255, 107, 107, 0.6);
}
input[type='range'] {
  -webkit-appearance: none;
  appearance: none;
  height: 4px;
  border-radius: 3px;
  background: rgba(255, 255, 255, 0.14);
  cursor: pointer;
  outline: none;
}
input[type='range']::-webkit-slider-thumb {
  -webkit-appearance: none;
  width: 12px;
  height: 12px;
  border-radius: 50%;
  background: #00e5ff;
  box-shadow: 0 0 8px rgba(0, 229, 255, 0.8);
  border: none;
}
input[type='range']::-moz-range-thumb {
  width: 12px;
  height: 12px;
  border-radius: 50%;
  background: #00e5ff;
  border: none;
  box-shadow: 0 0 8px rgba(0, 229, 255, 0.8);
}
/* fly-out queue */
.queue {
  position: absolute;
  bottom: 70px;
  right: 16px;
  width: 340px;
  max-height: 44vh;
  display: flex;
  flex-direction: column;
  background: rgba(10, 14, 24, 0.92);
  backdrop-filter: blur(16px);
  -webkit-backdrop-filter: blur(16px);
  border: 1px solid rgba(0, 229, 255, 0.28);
  border-radius: 12px 12px 0 0;
  box-shadow: 0 -12px 34px rgba(0, 0, 0, 0.55);
  overflow: hidden;
  z-index: 2;
}
.queue-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 14px;
  border-bottom: 1px solid rgba(0, 229, 255, 0.16);
}
.queue-title {
  font-size: 11px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: #00e5ff;
  font-weight: 700;
}
.q-close {
  background: none;
  border: none;
  color: #7fb4cf;
  cursor: pointer;
  font-size: 13px;
}
.q-close:hover {
  color: #ff6b6b;
}
.queue-list {
  list-style: none;
  margin: 0;
  padding: 6px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.q-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 7px 9px;
  border-radius: 8px;
  transition: background-color 0.15s ease;
}
.q-row:hover {
  background: rgba(255, 255, 255, 0.05);
}
.q-row.on {
  background: rgba(0, 229, 255, 0.1);
}
.q-cover {
  width: 30px;
  height: 30px;
  flex: 0 0 auto;
  border-radius: 6px;
  overflow: hidden;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #06121e;
  background: linear-gradient(135deg, #00e5ff, #a855f7);
}
.q-cover img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}
.q-play {
  width: 26px;
  height: 26px;
  flex: 0 0 auto;
  border-radius: 50%;
  border: none;
  cursor: pointer;
  font-size: 10px;
  color: #06121e;
  background: linear-gradient(135deg, #00e5ff, #a855f7);
}
.q-name {
  flex: 1;
  font-size: 12px;
  color: #d7ecf7;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.q-del {
  background: none;
  border: none;
  color: #5f7d90;
  cursor: pointer;
  font-size: 11px;
}
.q-del:hover {
  color: #ff6b6b;
}
.queue-slide-enter-active,
.queue-slide-leave-active {
  transition: transform 0.28s cubic-bezier(0.16, 1, 0.3, 1), opacity 0.28s ease;
}
.queue-slide-enter-from,
.queue-slide-leave-to {
  transform: translateY(18px);
  opacity: 0;
}
/* Milkdrop — плавающее окно */
.viz-win {
  position: fixed;
  right: 16px;
  bottom: 86px;
  width: 640px;
  height: 400px;
  max-width: calc(100vw - 32px);
  max-height: calc(100vh - 120px);
  z-index: 41;
  background: #000;
  border: 1px solid rgba(0, 229, 255, 0.35);
  border-radius: 12px;
  overflow: hidden;
  box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6), 0 0 24px rgba(0, 229, 255, 0.22);
  cursor: grab;
}
.viz-win.dragging {
  cursor: grabbing;
}
.viz-win.full {
  top: 0;
  left: 0;
  right: 0;
  bottom: 70px;
  width: auto;
  height: auto;
  max-width: none;
  max-height: none;
  border: none;
  border-radius: 0;
  box-shadow: none;
  cursor: default;
  transform: none !important;
}
.viz-canvas {
  width: 100%;
  height: 100%;
  display: block;
  cursor: inherit;
}
.viz-toolbar {
  position: absolute;
  top: 10px;
  right: 12px;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 5px 10px;
  border-radius: 999px;
  background: rgba(10, 14, 24, 0.6);
  backdrop-filter: blur(10px);
  -webkit-backdrop-filter: blur(10px);
  border: 1px solid rgba(0, 229, 255, 0.3);
  user-select: none;
}
.viz-drag {
  color: #6f96ad;
  font-size: 13px;
  cursor: grab;
}
.viz-preset {
  max-width: 260px;
  font-size: 11px;
  color: #b9d6e6;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.viz-btn {
  background: none;
  border: none;
  color: #cfe8f5;
  cursor: pointer;
  font-size: 14px;
  line-height: 1;
  transition: color 0.15s ease, transform 0.12s ease;
}
.viz-btn:hover {
  color: #00e5ff;
  transform: scale(1.14);
}
.viz-close:hover {
  color: #ff6b6b;
}
.viz-fade-enter-active,
.viz-fade-leave-active {
  transition: opacity 0.3s ease;
}
.viz-fade-enter-from,
.viz-fade-leave-to {
  opacity: 0;
}
</style>
