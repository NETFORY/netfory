<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, onMounted, onBeforeUnmount, watch } from 'vue'
import * as THREE from 'three'
import ThreeGlobe from 'three-globe'
import { geoLookup } from '@/lib/bridge'

// Stage 2/3 (visual A): neon continents + real connected peers bound to the
// globe. Each peer = a content bar (height ∝ volume) + a pulsing green
// "presence" marker (green = honest SmartHoldem seeder; pulse intensifies while
// the node is actively transferring chunks). Geo-coords come from GeoLite2 when
// the peer's public IP resolves (Stage 4); private/unknown peers fall back to a
// deterministic hash placement.
const props = defineProps<{
  peerIds: string[]
  peerAddrs?: string[] // index-aligned with peerIds (LAN/loopback => fallback)
  seed: string // app address — stable fallback layout key
  transferActive: boolean // node is actively sending/receiving chunks
  proofs?: Record<string, string> // peerId -> 'verified' | 'partial' | 'unverified'
  colorMap?: Record<string, string> // peerId -> hex colour (overrides proof colour)
  tooltips?: Record<string, string> // peerId -> hover tooltip text (optional)
  maxPoints?: number // cap of plotted points (default 24)
  active: boolean // panel open → run the render loop
  route?: boolean // маршрут Mycelium: светящиеся дуги между точками в порядке peerIds
}>()

// Hover-тултип по точке узла (текст даёт родитель через `tooltips`).
const tip = ref<{ show: boolean; x: number; y: number; text: string }>({
  show: false,
  x: 0,
  y: 0,
  text: '',
})
let raycaster: THREE.Raycaster | null = null
const ndc = new THREE.Vector2()

// addr (ip[:port]) -> resolved {lat,lng}; filled async by GeoLite2.
const geoCoords = ref<Record<string, { lat: number; lng: number }>>({})

const host = ref<HTMLDivElement | null>(null)
let renderer: THREE.WebGLRenderer | null = null
let scene: THREE.Scene | null = null
let camera: THREE.PerspectiveCamera | null = null
let globe: ThreeGlobe | null = null
let markers: THREE.Mesh[] = []
let raf = 0
let ro: ResizeObserver | null = null
let countries: { features: unknown[] } | null = null

// drag-to-rotate state ("heavy" feel: low sensitivity + inertial friction)
let dragging = false
let vx = 0
let vy = 0
let lastX = 0
let lastY = 0
const DRAG_SENS = 0.0016
const IDLE_SPIN = 0.0016
const FRICTION = 0.93
const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v))
let onDown: ((e: PointerEvent) => void) | null = null
let onMove: ((e: PointerEvent) => void) | null = null
let onUp: ((e: PointerEvent) => void) | null = null
let onWheel: ((e: WheelEvent) => void) | null = null
let onLeave: (() => void) | null = null
// Пользовательский зум (колесо мыши): 1 = авто-вписывание, >1 крупнее, <1 мельче.
let userZoom = 1
const ZOOM_MIN = 0.6
const ZOOM_MAX = 3

// Дистанция камеры, при которой планета целиком вписывается в кадр с небольшим
// отступом — независимо от размера/пропорций окна (масштабируется под окно).
function fitCamera() {
  if (!camera || !renderer || !host.value) return
  const w = host.value.clientWidth || 320
  const h = host.value.clientHeight || 200
  const aspect = w / h
  camera.aspect = aspect
  const fovV = (45 * Math.PI) / 180
  const vHalf = Math.tan(fovV / 2)
  // Ограничивающая сторона (для 3:2 — вертикаль); учитываем горизонталь для portrait.
  const bindingHalf = Math.min(vHalf, vHalf * aspect)
  const R = 118 // видимый радиус планеты с атмосферой/маркерами
  const dist = R / bindingHalf / Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, userZoom))
  camera.position.set(0, dist * 0.12, dist)
  camera.lookAt(0, 0, 0)
  camera.updateProjectionMatrix()
  renderer.setSize(w, h)
}

const TEAL = '#14c9b8'
const GREEN = new THREE.Color('#34d399')
const ORANGE = new THREE.Color('#f59e0b')
const BLUE = new THREE.Color('#38bdf8')
const TIP = '#7dffea'

// Storage-proof colour: green = verified seeder, orange = partial, blue = unproven.
function proofColor(id: string): THREE.Color {
  const c = props.colorMap?.[id]
  if (c) return new THREE.Color(c)
  const st = props.proofs?.[id]
  if (st === 'verified') return GREEN
  if (st === 'partial') return ORANGE
  return BLUE
}

function h01(s: string): number {
  let h = 2166136261
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  return ((h >>> 0) % 100000) / 100000
}

// one entry per real connected peer (GeoLite2 coords when the peer's public IP
// resolves, deterministic hash otherwise)
function peerLayout() {
  const ids = props.peerIds.length ? props.peerIds : [props.seed]
  const addrs = props.peerAddrs || []
  return ids.slice(0, props.maxPoints ?? 24).map((id, i) => {
    const geo = addrs[i] ? geoCoords.value[addrs[i]] : undefined
    return {
      id,
      lat: geo ? geo.lat : -55 + h01(id + ':lat') * 125,
      lng: geo ? geo.lng : -180 + h01(id + ':lng') * 360,
      // Высота «шипов» уменьшена на 15% (множитель 0.85), чтобы самые длинные линии были короче.
      alt: (0.08 + h01(id + ':val') * 0.42) * 0.85,
      real: !!geo,
    }
  })
}

// Resolve real coordinates for the current peer addresses via GeoLite2.
async function resolveGeo() {
  const addrs = (props.peerAddrs || []).filter((a) => a && !(a in geoCoords.value))
  if (!addrs.length) return
  try {
    const pts = await geoLookup(Array.from(new Set(addrs)))
    if (pts.length) {
      const next = { ...geoCoords.value }
      for (const p of pts) next[p.ip] = { lat: p.lat, lng: p.lng }
      geoCoords.value = next
      if (renderer) applyPeers()
    }
  } catch {
    /* GeoLite2 unavailable — keep the hash fallback */
  }
}

// Raycast курсора по маркерам узлов → показать/скрыть тултип с текстом от родителя.
function updateHover(e: PointerEvent) {
  if (!raycaster || !camera || !renderer || !markers.length) return
  const rect = renderer.domElement.getBoundingClientRect()
  ndc.x = ((e.clientX - rect.left) / rect.width) * 2 - 1
  ndc.y = -((e.clientY - rect.top) / rect.height) * 2 + 1
  raycaster.setFromCamera(ndc, camera)
  const hits = raycaster.intersectObjects(markers, false)
  if (hits.length) {
    const id = hits[0].object.userData.id as string
    const text = props.tooltips?.[id]
    if (text) {
      tip.value = { show: true, x: e.clientX - rect.left, y: e.clientY - rect.top, text }
      return
    }
  }
  if (tip.value.show) tip.value = { ...tip.value, show: false }
}

function applyPeers() {
  if (!globe) return
  const layout = peerLayout()
  // content bars
  globe
    .pointsData(layout)
    .pointLat('lat')
    .pointLng('lng')
    .pointAltitude('alt')
    .pointRadius(0.32)
    .pointColor((d: any) => `#${proofColor(d.id).getHexString()}`)
    .pointsMerge(false)
  // маршрут: Вы → Guard → Middle → Exit → выход — дуги между соседними точками
  const arcs = props.route
    ? layout.slice(1).map((p, i) => ({ startLat: layout[i].lat, startLng: layout[i].lng, endLat: p.lat, endLng: p.lng }))
    : []
  globe
    .arcsData(arcs)
    .arcColor(() => ['rgba(46,230,166,0.35)', 'rgba(46,230,166,1)'])
    .arcStroke(0.9)
    .arcAltitudeAutoScale(0.45)
    .arcDashLength(0.45)
    .arcDashGap(0.18)
    .arcDashAnimateTime(1800)
  // pulsing presence markers (own children of the globe group)
  for (const m of markers) {
    globe.remove(m)
    m.geometry.dispose()
  }
  markers = []
  for (const p of layout) {
    const pos = globe.getCoords(p.lat, p.lng, 0.02)
    const mesh = new THREE.Mesh(
      new THREE.SphereGeometry(1.5, 12, 12),
      new THREE.MeshBasicMaterial({
        color: proofColor(p.id).clone(),
        transparent: true,
        opacity: 0.9,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
      }),
    )
    mesh.position.set(pos.x, pos.y, pos.z)
    mesh.userData.phase = h01(p.id) * Math.PI * 2
    mesh.userData.id = p.id
    globe.add(mesh)
    markers.push(mesh)
  }
}

function init() {
  if (!host.value || !countries) return
  const w = host.value.clientWidth || 320
  const hgt = host.value.clientHeight || 200

  scene = new THREE.Scene()
  camera = new THREE.PerspectiveCamera(45, w / hgt, 0.1, 2000)
  camera.position.set(0, 34, 232)
  camera.lookAt(0, 0, 0)

  renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true })
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2))
  renderer.setSize(w, hgt)
  host.value.appendChild(renderer.domElement)
  // Вписываем планету в кадр (учёт пропорций окна).
  fitCamera()

  // drag-to-rotate (grab the globe and spin it; releases into inertia)
  const el = renderer.domElement
  raycaster = new THREE.Raycaster()
  el.style.cursor = 'grab'
  el.style.touchAction = 'none'
  onDown = (e: PointerEvent) => {
    dragging = true
    lastX = e.clientX
    lastY = e.clientY
    vx = 0
    vy = 0
    tip.value.show = false
    el.style.cursor = 'grabbing'
    el.setPointerCapture(e.pointerId)
  }
  onMove = (e: PointerEvent) => {
    if (dragging && globe) {
      const dx = e.clientX - lastX
      const dy = e.clientY - lastY
      lastX = e.clientX
      lastY = e.clientY
      vy = dx * DRAG_SENS
      vx = dy * DRAG_SENS
      globe.rotation.y += vy
      globe.rotation.x = clamp(globe.rotation.x + vx, -0.6, 0.6)
      return
    }
    updateHover(e)
  }
  onUp = (e: PointerEvent) => {
    dragging = false
    el.style.cursor = 'grab'
    try {
      el.releasePointerCapture(e.pointerId)
    } catch {
      /* already released */
    }
  }
  el.addEventListener('pointerdown', onDown)
  el.addEventListener('pointermove', onMove)
  el.addEventListener('pointerup', onUp)
  el.addEventListener('pointercancel', onUp)
  onLeave = () => {
    tip.value.show = false
  }
  el.addEventListener('pointerleave', onLeave)
  // Зум колесом мыши по изображению планеты.
  onWheel = (e: WheelEvent) => {
    e.preventDefault()
    const factor = e.deltaY < 0 ? 1.12 : 1 / 1.12
    userZoom = clamp(userZoom * factor, ZOOM_MIN, ZOOM_MAX)
    fitCamera()
  }
  el.addEventListener('wheel', onWheel, { passive: false })

  scene.add(new THREE.AmbientLight(0xffffff, 1.4))
  const dir = new THREE.DirectionalLight(0x9fffef, 0.5)
  dir.position.set(1, 1, 1)
  scene.add(dir)

  globe = new ThreeGlobe()
    .hexPolygonsData(countries.features as object[])
    .hexPolygonResolution(2)
    .hexPolygonMargin(0.62)
    .hexPolygonAltitude(0.006)
    .hexPolygonColor(() => TEAL)
    .showAtmosphere(true)
    .atmosphereColor(TEAL)
    .atmosphereAltitude(0.16)
  const gm = globe.globeMaterial() as THREE.MeshPhongMaterial
  gm.color = new THREE.Color(0x07120f)
  gm.emissive = new THREE.Color(0x04181a)
  gm.emissiveIntensity = 0.25
  globe.rotation.z = 0.28
  // Планета уменьшена на 10% внутри квадрата рендера (точки/атмосфера масштабируются вместе).
  globe.scale.setScalar(0.9)
  applyPeers()
  scene.add(globe)

  const clock = new THREE.Clock()
  const loop = () => {
    raf = requestAnimationFrame(loop)
    const t = clock.getElapsedTime()
    // rotation: user drag (with inertia) when grabbed, gentle auto-spin otherwise
    if (globe && !dragging) {
      if (Math.abs(vy) > IDLE_SPIN) {
        globe.rotation.y += vy
        vy *= FRICTION
      } else {
        globe.rotation.y += IDLE_SPIN
      }
      if (Math.abs(vx) > 0.0004) {
        globe.rotation.x = clamp(globe.rotation.x + vx, -0.6, 0.6)
        vx *= FRICTION
      }
    }
    // presence pulse — faster + brighter while chunks are flowing
    const speed = props.transferActive ? 4.2 : 1.8
    const amp = props.transferActive ? 1.3 : 0.6
    for (const m of markers) {
      const ph = (m.userData.phase as number) || 0
      const k = 0.5 + 0.5 * Math.sin(t * speed + ph)
      const s = 1 + amp * k
      m.scale.setScalar(s)
      ;(m.material as THREE.MeshBasicMaterial).opacity = 0.35 + 0.6 * k
    }
    renderer!.render(scene!, camera!)
  }
  loop()

  ro = new ResizeObserver(() => {
    if (!host.value || !renderer || !camera) return
    fitCamera()
  })
  ro.observe(host.value)
}

async function ensureDataAndInit() {
  if (!countries) {
    try {
      const r = await fetch('/countries.geojson')
      countries = await r.json()
    } catch {
      countries = { features: [] }
    }
  }
  if (props.active && !renderer) {
    // Defer the (synchronous, ~heavy) ThreeGlobe hex-polygon tessellation to
    // the next frames so the route/panel paints FIRST — otherwise opening the
    // view froze the whole UI thread while the globe geometry was built.
    requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        if (props.active && !renderer) init()
      }),
    )
  }
  resolveGeo()
}

function dispose() {
  cancelAnimationFrame(raf)
  ro?.disconnect()
  ro = null
  markers = []
  if (renderer) {
    const el = renderer.domElement
    if (onDown) el.removeEventListener('pointerdown', onDown)
    if (onMove) el.removeEventListener('pointermove', onMove)
    if (onUp) {
      el.removeEventListener('pointerup', onUp)
      el.removeEventListener('pointercancel', onUp)
    }
    if (onWheel) el.removeEventListener('wheel', onWheel)
    if (onLeave) el.removeEventListener('pointerleave', onLeave)
    renderer.dispose()
    renderer.domElement.remove()
    renderer = null
  }
  onDown = onMove = onUp = null
  onWheel = null
  onLeave = null
  raycaster = null
  tip.value.show = false
  dragging = false
  vx = vy = 0
  scene = null
  camera = null
  globe = null
}

onMounted(() => {
  if (props.active) ensureDataAndInit()
})
watch(
  () => props.active,
  (on) => {
    if (on) ensureDataAndInit()
    else if (renderer) dispose()
  },
)
watch([() => props.peerIds, () => props.seed], () => {
  if (renderer) applyPeers()
})
watch(
  () => props.proofs,
  () => {
    if (renderer) applyPeers()
  },
  { deep: true },
)
watch(
  () => props.colorMap,
  () => {
    if (renderer) applyPeers()
  },
  { deep: true },
)
watch(
  () => props.peerAddrs,
  () => {
    if (props.active) resolveGeo()
  },
  { deep: true },
)
onBeforeUnmount(dispose)
</script>

<template>
  <div ref="host" class="node-planet" data-testid="node-planet">
    <div
      v-if="tip.show"
      class="planet-tip"
      :style="{ left: tip.x + 12 + 'px', top: tip.y + 12 + 'px' }"
      data-testid="node-planet-tip"
    >
      {{ tip.text }}
    </div>
  </div>
</template>

<style scoped>
.node-planet {
  width: 100%;
  height: 100%;
  position: relative;
}
.node-planet :deep(canvas) {
  display: block;
}
.planet-tip {
  position: absolute;
  z-index: 5;
  max-width: 240px;
  padding: 6px 10px;
  border-radius: 8px;
  background: rgba(6, 18, 26, 0.92);
  border: 1px solid rgba(125, 255, 234, 0.35);
  color: #d7ecf7;
  font-size: 11px;
  line-height: 1.35;
  pointer-events: none;
  white-space: nowrap;
  backdrop-filter: blur(6px);
  box-shadow: 0 6px 20px rgba(0, 0, 0, 0.4);
}
</style>
