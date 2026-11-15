<script setup lang="ts">
/* eslint-disable no-undef */
// Кроп/масштаб любой картинки в кадр DAPP Preview 440×180.
// Экспорт через canvas: PNG, при >256KB — JPEG (всегда Huffman — WebView-safe).
import { ref, computed, onBeforeUnmount, watch, nextTick } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{ file: File }>()
const emit = defineEmits<{
  (e: 'apply', bytes: number[], name: string): void
  (e: 'cancel'): void
  (e: 'decodefail'): void
}>()
const { t } = useI18n()

const W = 440
const H = 180
const img = new Image()
const loaded = ref(false)
const minScale = ref(1)
const scale = ref(1)
const offX = ref(0)
const offY = ref(0)
const canvasEl = ref<HTMLCanvasElement | null>(null)
const url = URL.createObjectURL(props.file)

img.onload = () => {
  minScale.value = Math.max(W / img.naturalWidth, H / img.naturalHeight)
  scale.value = minScale.value
  offX.value = (W - img.naturalWidth * scale.value) / 2
  offY.value = (H - img.naturalHeight * scale.value) / 2
  loaded.value = true
  nextTick(draw)
}
img.onerror = () => emit('decodefail')
img.src = url
onBeforeUnmount(() => URL.revokeObjectURL(url))

function clampOffsets() {
  const w = img.naturalWidth * scale.value
  const h = img.naturalHeight * scale.value
  offX.value = Math.min(0, Math.max(W - w, offX.value))
  offY.value = Math.min(0, Math.max(H - h, offY.value))
}

function draw() {
  const c = canvasEl.value
  if (!c || !loaded.value) return
  const ctx = c.getContext('2d')
  if (!ctx) return
  ctx.clearRect(0, 0, W, H)
  ctx.imageSmoothingEnabled = true
  ctx.imageSmoothingQuality = 'high'
  ctx.drawImage(img, offX.value, offY.value, img.naturalWidth * scale.value, img.naturalHeight * scale.value)
}
watch([scale, offX, offY, loaded], () => nextTick(draw))

// зум с сохранением центра кадра
function setScale(next: number) {
  const s = Math.min(minScale.value * 4, Math.max(minScale.value, next))
  const cx = (W / 2 - offX.value) / scale.value
  const cy = (H / 2 - offY.value) / scale.value
  scale.value = s
  offX.value = W / 2 - cx * s
  offY.value = H / 2 - cy * s
  clampOffsets()
}
const scaleModel = computed({
  get: () => scale.value,
  set: (v: number) => setScale(v),
})
function onWheel(e: WheelEvent) {
  setScale(scale.value * (e.deltaY < 0 ? 1.08 : 0.92))
}

let dragging = false
let lastX = 0
let lastY = 0
function onDown(e: PointerEvent) {
  dragging = true
  lastX = e.clientX
  lastY = e.clientY
  ;(e.target as HTMLElement).setPointerCapture(e.pointerId)
}
function onMove(e: PointerEvent) {
  if (!dragging) return
  offX.value += e.clientX - lastX
  offY.value += e.clientY - lastY
  lastX = e.clientX
  lastY = e.clientY
  clampOffsets()
}
function onUp() {
  dragging = false
}

const busy = ref(false)
async function apply() {
  const c = canvasEl.value
  if (!c || !loaded.value || busy.value) return
  busy.value = true
  try {
    const toBlob = (type: string, q?: number) =>
      new Promise<Blob | null>((res) => c.toBlob(res, type, q))
    // качество прежде всего: PNG, при превышении лимита сети — JPEG
    let blob = await toBlob('image/png')
    if (!blob || blob.size > 256 * 1024) blob = await toBlob('image/jpeg', 0.9)
    if (!blob || blob.size > 256 * 1024) blob = await toBlob('image/jpeg', 0.72)
    if (!blob) return
    const bytes = Array.from(new Uint8Array(await blob.arrayBuffer()))
    const base = props.file.name.replace(/\.[^.]+$/, '') || 'preview'
    const ext = blob.type === 'image/png' ? 'png' : 'jpg'
    emit('apply', bytes, `${base}-440x180.${ext}`)
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="crop-back" data-testid="preview-crop-modal" @click.self="emit('cancel')">
    <div class="crop-box">
      <h3 class="crop-title">{{ t('apps.cropTitle') }}</h3>
      <canvas
        ref="canvasEl"
        :width="W"
        :height="H"
        class="crop-canvas"
        data-testid="crop-canvas"
        @pointerdown="onDown"
        @pointermove="onMove"
        @pointerup="onUp"
        @pointercancel="onUp"
        @wheel.prevent="onWheel"
      />
      <div class="crop-zoom">
        <span class="crop-zoom-mark">−</span>
        <input
          v-model.number="scaleModel"
          type="range"
          :min="minScale"
          :max="minScale * 4"
          :step="0.001"
          data-testid="crop-zoom-range"
        />
        <span class="crop-zoom-mark">+</span>
      </div>
      <p class="crop-hint">{{ t('apps.cropHint') }}</p>
      <div class="crop-actions">
        <button class="btn" data-testid="crop-cancel" @click="emit('cancel')">
          {{ t('apps.cropCancel') }}
        </button>
        <button class="btn btn-primary" data-testid="crop-apply" :disabled="!loaded || busy" @click="apply">
          {{ t('apps.cropApply') }} ✓
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.crop-back {
  position: fixed;
  inset: 0;
  z-index: 300; /* выше .modal-back (200) — кроп открывается ПОВЕРХ EDIT-модалки */
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(3, 8, 12, 0.72);
  backdrop-filter: blur(6px);
}
.crop-box {
  background: var(--panel, #10161a);
  border: 1px solid var(--border, #1e2a2f);
  border-radius: 12px;
  padding: 18px 20px 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.crop-title {
  margin: 0;
  font-size: 14px;
  letter-spacing: 0.08em;
  color: var(--text, #d6e2e6);
}
.crop-canvas {
  width: 440px;
  height: 180px;
  border: 1px solid var(--border, #1e2a2f);
  border-radius: 8px;
  cursor: grab;
  touch-action: none;
  background:
    repeating-conic-gradient(rgba(255, 255, 255, 0.05) 0% 25%, transparent 0% 50%) 0 0 / 16px 16px;
}
.crop-canvas:active {
  cursor: grabbing;
}
.crop-zoom {
  display: flex;
  align-items: center;
  gap: 10px;
}
.crop-zoom input {
  flex: 1;
  accent-color: var(--accent, #29d398);
}
.crop-zoom-mark {
  color: var(--dim, #6b7d84);
  font-size: 15px;
}
.crop-hint {
  margin: 0;
  font-size: 11px;
  color: var(--dim, #6b7d84);
  max-width: 440px;
}
.crop-actions {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
}
</style>
