<script setup lang="ts">
// Сканер QR для standalone-входа: камера через getUserMedia, распознавание
// нативным BarcodeDetector (Android/Chrome) с fallback на jsQR (WebKit/WebView2).
import { onBeforeUnmount, onMounted, ref } from 'vue'
import jsQR from 'jsqr'
import { isSeedQr } from '@/lib/seedqr'

const emit = defineEmits<{ decoded: [text: string]; close: [] }>()

const video = ref<HTMLVideoElement | null>(null)
const err = ref('')
const hint = ref('Наведите камеру на QR из Netfory → Ключи')
const engine = ref('')
const cameras = ref<MediaDeviceInfo[]>([])
const camIdx = ref(0)
let stream: MediaStream | null = null
let raf = 0
let lastDecode = 0
let detector: any = null
let canvas: HTMLCanvasElement | null = null
let done = false

function describeError(e: any): string {
  const name = e?.name || ''
  if (name === 'NotAllowedError' || name === 'SecurityError') return 'Доступ к камере запрещён. Разрешите камеру для NETFORY Messenger в настройках системы и попробуйте снова.'
  if (name === 'NotFoundError' || name === 'OverconstrainedError') return 'Камера не найдена. Вставьте сид-фразу или содержимое QR текстом.'
  if (name === 'NotReadableError') return 'Камера занята другим приложением.'
  return 'Не удалось открыть камеру: ' + (e?.message || String(e))
}

function stop() {
  cancelAnimationFrame(raf)
  stream?.getTracks().forEach((t) => t.stop())
  stream = null
}

async function start(constraints: MediaTrackConstraints) {
  stop()
  err.value = ''
  try {
    stream = await navigator.mediaDevices.getUserMedia({ video: constraints, audio: false })
    const v = video.value!
    v.srcObject = stream
    await v.play()
    // список камер доступен только после выдачи разрешения
    cameras.value = (await navigator.mediaDevices.enumerateDevices()).filter((d) => d.kind === 'videoinput')
    raf = requestAnimationFrame(tick)
  } catch (e) {
    err.value = describeError(e)
  }
}

function switchCamera() {
  if (cameras.value.length < 2) return
  camIdx.value = (camIdx.value + 1) % cameras.value.length
  void start({ deviceId: { exact: cameras.value[camIdx.value].deviceId } })
}

function onResult(text: string) {
  if (done) return
  if (!isSeedQr(text)) {
    hint.value = 'Это не QR мессенджера NETFORY — нужен код из Netfory → Ключи'
    return
  }
  done = true
  stop()
  emit('decoded', text.trim())
}

async function tick(ts: number) {
  const v = video.value
  if (!v || !stream) return
  raf = requestAnimationFrame(tick)
  if (ts - lastDecode < 120 || v.readyState < 2 || !v.videoWidth) return
  lastDecode = ts
  try {
    if (detector) {
      const codes = await detector.detect(v)
      if (codes?.length) onResult(codes[0].rawValue)
      return
    }
    canvas ??= document.createElement('canvas')
    canvas.width = v.videoWidth
    canvas.height = v.videoHeight
    const ctx = canvas.getContext('2d', { willReadFrequently: true })!
    ctx.drawImage(v, 0, 0)
    const img = ctx.getImageData(0, 0, canvas.width, canvas.height)
    const code = jsQR(img.data, img.width, img.height, { inversionAttempts: 'dontInvert' })
    if (code?.data) onResult(code.data)
  } catch (e) {
    console.warn('qr decode', e)
  }
}

onMounted(async () => {
  if (!navigator.mediaDevices?.getUserMedia) {
    err.value = 'Камера недоступна в этом окружении. Вставьте сид-фразу текстом.'
    return
  }
  const BD = (window as any).BarcodeDetector
  if (BD) {
    try {
      const formats: string[] = await BD.getSupportedFormats?.() ?? ['qr_code']
      if (formats.includes('qr_code')) detector = new BD({ formats: ['qr_code'] })
    } catch { detector = null }
  }
  engine.value = detector ? 'BarcodeDetector' : 'jsQR'
  await start({ facingMode: { ideal: 'environment' } })
})
onBeforeUnmount(stop)
</script>

<template>
  <div class="qr-scan" data-testid="qr-scanner">
    <div class="qr-head">
      <span class="ttl">СКАНИРОВАНИЕ QR</span>
      <span class="eng" :title="'Движок распознавания'">{{ engine }}</span>
      <span class="sp" />
      <button v-if="cameras.length > 1" class="ghost" @click="switchCamera" data-testid="qr-switch-camera-btn">КАМЕРА {{ camIdx + 1 }}/{{ cameras.length }}</button>
      <button class="ghost" @click="emit('close')" data-testid="qr-scanner-close-btn">ОТМЕНА</button>
    </div>
    <div class="qr-view">
      <video ref="video" playsinline muted autoplay data-testid="qr-video" />
      <div v-if="!err" class="qr-frame" />
      <p v-if="err" class="qr-err" data-testid="qr-scanner-error">{{ err }}</p>
    </div>
    <p class="qr-hint" data-testid="qr-scanner-hint">{{ err ? 'Можно закрыть сканер и ввести данные вручную.' : hint }}</p>
  </div>
</template>
