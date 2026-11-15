<script setup lang="ts">
// Полноэкранный просмотр изображения мессенджера (отдельное окно ?w=imgview).
// Премиальный оверлей: прозрачное окно + виньеточное затенение. Управление:
// колесо — зум (к курсору), drag — перемещение, двойной клик — сброс,
// Esc / клик по фону / ✕ — закрыть.
import { ref, onMounted, onBeforeUnmount } from 'vue'
import { useI18n } from 'vue-i18n'
import { appListen, loadChatImage, chatMediaProgress, closeImageWindow } from '@/lib/bridge'

const { t } = useI18n()
const src = ref('')
const error = ref(false)
const loading = ref(true)
let unlisten: (() => void) | null = null

// живой прогресс докачки: «34% · 1.2 MB/s» (поллинг раз в 600мс)
const prog = ref('')
let progTimer: number | null = null
function fmtSpeed(bps: number): string {
  return bps >= 1048576 ? (bps / 1048576).toFixed(1) + ' MB/s' : Math.max(1, Math.round(bps / 1024)) + ' KB/s'
}
function startProg(hash: string, total: number) {
  stopProg()
  let lastB = 0
  let lastT = Date.now()
  progTimer = window.setInterval(async () => {
    try {
      const p = await chatMediaProgress(hash)
      const now = Date.now()
      const speed = Math.max(0, ((p.bytes - lastB) * 1000) / Math.max(1, now - lastT))
      lastB = p.bytes
      lastT = now
      const pct = total > 0 ? Math.min(99, Math.floor((p.bytes * 100) / total)) : 0
      prog.value = total > 0 ? `${pct}% · ${fmtSpeed(speed)}` : fmtSpeed(speed)
    } catch {}
  }, 600)
}
function stopProg() {
  if (progTimer) {
    window.clearInterval(progTimer)
    progTimer = null
  }
  prog.value = ''
}

// ---- зум/пан ----
const scale = ref(1)
const tx = ref(0)
const ty = ref(0)
const dragging = ref(false)
let dragStart = { x: 0, y: 0, tx: 0, ty: 0 }

function resetView() {
  scale.value = 1
  tx.value = 0
  ty.value = 0
}

function onWheel(e: WheelEvent) {
  e.preventDefault()
  if (!src.value) return
  const factor = e.deltaY < 0 ? 1.2 : 1 / 1.2
  const next = Math.min(8, Math.max(1, scale.value * factor))
  if (next === scale.value) return
  if (next === 1) {
    resetView()
    return
  }
  // якорь на курсор: точка под мышью остаётся на месте
  const cx = window.innerWidth / 2
  const cy = window.innerHeight / 2
  const px = e.clientX - cx
  const py = e.clientY - cy
  const k = next / scale.value
  tx.value = px - (px - tx.value) * k
  ty.value = py - (py - ty.value) * k
  scale.value = next
}

function onDragStart(e: MouseEvent) {
  if (e.button !== 0 || scale.value <= 1) return
  e.preventDefault()
  dragging.value = true
  dragStart = { x: e.clientX, y: e.clientY, tx: tx.value, ty: ty.value }
  window.addEventListener('mousemove', onDragMove)
  window.addEventListener('mouseup', onDragEnd)
}
function onDragMove(e: MouseEvent) {
  if (!dragging.value) return
  tx.value = dragStart.tx + (e.clientX - dragStart.x)
  ty.value = dragStart.ty + (e.clientY - dragStart.y)
}
function onDragEnd() {
  dragging.value = false
  window.removeEventListener('mousemove', onDragMove)
  window.removeEventListener('mouseup', onDragEnd)
}

async function load(room: string, hash: string, node: string, size = 0) {
  loading.value = true
  error.value = false
  src.value = ''
  resetView()
  startProg(hash, size)
  try {
    const b64 = await loadChatImage(room, hash, node)
    src.value = 'data:image/jpeg;base64,' + b64
  } catch {
    error.value = true
  } finally {
    loading.value = false
    stopProg()
  }
}

function boot() {
  // Прозрачный фон окна + без скроллбаров (белые полосы справа/снизу)
  document.documentElement.classList.add('iv-mode')
  const q = new URLSearchParams(window.location.search)
  const hash = q.get('hash') || ''
  if (hash) load(q.get('room') || '', hash, q.get('node') || '', Number(q.get('s') || 0))
}

function close() {
  closeImageWindow()
}
function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') close()
}

onMounted(async () => {
  boot()
  window.addEventListener('keydown', onKey)
  unlisten = await appListen<{ room?: string; hash?: string; node?: string; size?: number }>('imgview-open', (p) => {
    if (p?.hash) load(p.room || '', p.hash, p.node || '', p.size || 0)
  })
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKey)
  window.removeEventListener('mousemove', onDragMove)
  window.removeEventListener('mouseup', onDragEnd)
  document.documentElement.classList.remove('iv-mode')
  stopProg()
  if (unlisten) unlisten()
})
</script>

<template>
  <div class="iv-root" @click.self="close" @wheel="onWheel" data-testid="imgview-root">
    <div class="iv-hint" data-testid="imgview-hint">
      <span><b>Scroll</b> zoom</span>
      <span class="iv-hint-sep">·</span>
      <span><b>Drag</b> pan</span>
      <span class="iv-hint-sep">·</span>
      <span><b>Double-click</b> reset</span>
      <span class="iv-hint-sep">·</span>
      <span><b>Esc</b> close</span>
    </div>
    <button class="iv-close" data-testid="imgview-close-btn" title="Close (Esc)" @click.stop="close">
      <svg width="15" height="15" viewBox="0 0 15 15" fill="none"><path d="M2 2l11 11M13 2L2 13" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>
    </button>
    <img
      v-if="src"
      :src="src"
      class="iv-img"
      :class="{ 'iv-zoomed': scale > 1, 'iv-dragging': dragging }"
      :style="{ transform: `translate(${tx}px, ${ty}px) scale(${scale})` }"
      alt=""
      draggable="false"
      data-testid="imgview-img"
      @mousedown="onDragStart"
      @dblclick.stop="resetView"
      @click.stop
    />
    <div v-else-if="error" class="iv-state" data-testid="imgview-error">{{ t('messenger.imgLoadFailed') }}</div>
    <div v-else class="iv-state" data-testid="imgview-loading"><span class="iv-spinner" />{{ t('messenger.imgLoading') }}<span v-if="prog" class="iv-prog" data-testid="imgview-progress">{{ prog }}</span></div>
  </div>
</template>

<style>
/* Режим окна-просмотрщика: прозрачный webview, ноль скроллбаров */
html.iv-mode,
html.iv-mode body,
html.iv-mode #app {
  background: transparent !important;
  overflow: hidden !important;
  margin: 0;
  height: 100%;
}
html.iv-mode ::-webkit-scrollbar {
  display: none;
}
</style>

<style scoped>
.iv-root {
  position: fixed;
  inset: 0;
  /* полупрозрачное затенение с виньеткой: рабочий стол мягко темнеет,
     без глухой чёрной заливки */
  background: radial-gradient(
    ellipse at center,
    rgba(5, 8, 13, 0.78) 0%,
    rgba(3, 5, 9, 0.88) 62%,
    rgba(1, 2, 4, 0.94) 100%
  );
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: zoom-out;
  user-select: none;
  overflow: hidden;
  animation: iv-fade 0.18s ease-out;
}
.iv-img {
  max-width: 94vw;
  max-height: 94vh;
  object-fit: contain;
  border-radius: 12px;
  box-shadow:
    0 32px 90px rgba(0, 0, 0, 0.68),
    0 6px 24px rgba(0, 0, 0, 0.5),
    0 0 0 1px rgba(255, 255, 255, 0.07);
  animation: iv-pop 0.22s cubic-bezier(0.22, 1, 0.36, 1);
  cursor: default;
  transition: transform 0.12s ease-out;
  will-change: transform;
}
.iv-img.iv-zoomed {
  cursor: grab;
}
.iv-img.iv-dragging {
  cursor: grabbing;
  transition: none;
}
.iv-hint {
  position: fixed;
  top: 22px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 5;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 16px;
  border-radius: 999px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  background: rgba(12, 16, 24, 0.55);
  backdrop-filter: blur(14px);
  color: #a1a1aa;
  font-size: 11px;
  letter-spacing: 0.02em;
  font-family: 'Inter', system-ui, sans-serif;
  pointer-events: none;
  white-space: nowrap;
  animation: iv-hint-dim 0.4s ease 5s forwards;
}
.iv-hint b {
  color: #e4e4e7;
  font-weight: 600;
}
.iv-hint-sep {
  opacity: 0.4;
}
@keyframes iv-hint-dim {
  to { opacity: 0.28; }
}
.iv-close {
  position: fixed;
  top: 22px;
  right: 26px;
  z-index: 5;
  width: 40px;
  height: 40px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 50%;
  border: 1px solid rgba(255, 255, 255, 0.12);
  background: rgba(14, 18, 26, 0.55);
  backdrop-filter: blur(14px);
  color: #d4d4d8;
  cursor: pointer;
  transition: background-color 0.15s ease, color 0.15s ease, transform 0.15s ease;
}
.iv-close:hover {
  background: rgba(30, 36, 48, 0.75);
  color: #ffffff;
  transform: rotate(90deg);
}
.iv-state {
  color: #d4d4d8;
  font-size: 13px;
  display: flex;
  align-items: center;
  gap: 12px;
  font-family: 'Inter', system-ui, sans-serif;
  padding: 12px 22px;
  border-radius: 999px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  background: rgba(12, 16, 24, 0.6);
  backdrop-filter: blur(14px);
}
.iv-spinner {
  width: 18px;
  height: 18px;
  border: 2px solid rgba(255, 255, 255, 0.14);
  border-top-color: #38bdf8;
  border-radius: 50%;
  animation: iv-spin 0.8s linear infinite;
}
.iv-prog {
  color: #38bdf8;
  font-family: 'JetBrains Mono', monospace;
  font-size: 12px;
  white-space: nowrap;
}
@keyframes iv-spin {
  to { transform: rotate(360deg); }
}
@keyframes iv-fade {
  from { opacity: 0; }
  to { opacity: 1; }
}
@keyframes iv-pop {
  from { opacity: 0; transform: scale(0.965); }
  to { opacity: 1; transform: scale(1); }
}
</style>
