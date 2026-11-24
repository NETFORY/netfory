<script setup lang="ts">
import { defineEmits, defineProps, onBeforeUnmount, ref } from 'vue'

// Кнопка «удерживайте для подтверждения» (как в расширении): защита от случайной отправки.
const props = defineProps<{ label: string; disabled?: boolean; busy?: boolean; ms?: number }>()
const emit = defineEmits<{ (e: 'confirm'): void }>()
const p = ref(0)
let raf = 0, t0 = 0
function tick(now: number) {
  p.value = Math.min(1, (now - t0) / (props.ms ?? 1100))
  if (p.value >= 1) { stop(); emit('confirm'); return }
  raf = requestAnimationFrame(tick)
}
function start() {
  if (props.disabled || props.busy || raf) return
  t0 = performance.now()
  raf = requestAnimationFrame(tick)
}
function stop() { cancelAnimationFrame(raf); raf = 0; p.value = 0 }
onBeforeUnmount(stop)
</script>

<template>
  <button type="button" class="hold" :class="{ dis: disabled || busy }" :disabled="disabled" @pointerdown.prevent="start" @pointerup="stop" @pointerleave="stop" @pointercancel="stop" @keydown.enter.space.prevent="start" @keyup.enter.space="stop">
    <i class="hold-fill" :style="{ transform: `scaleX(${p})` }" />
    <span class="hold-dot" :class="{ spin: busy }" />
    <span class="hold-lbl">{{ busy ? '…' : label }}</span>
  </button>
</template>

<style scoped>
.hold { position: relative; overflow: hidden; width: 100%; display: flex; align-items: center; justify-content: center; gap: 10px; padding: 15px 12px; border-radius: 12px; border: 1.5px solid var(--accent); background: color-mix(in srgb, var(--accent) 8%, transparent); color: var(--accent); font: inherit; font-size: 13px; font-weight: 800; letter-spacing: 0.06em; text-transform: uppercase; cursor: pointer; user-select: none; touch-action: none; transition: opacity 0.15s; }
.hold.dis { opacity: 0.4; cursor: default; }
.hold-fill { position: absolute; inset: 0; background: color-mix(in srgb, var(--accent) 30%, transparent); transform-origin: left; transform: scaleX(0); }
.hold-dot { position: relative; width: 9px; height: 9px; border-radius: 50%; background: #8b7cf6; box-shadow: 0 0 8px #8b7cf6; }
.hold-dot.spin { animation: hold-pulse 0.8s ease-in-out infinite alternate; }
.hold-lbl { position: relative; }
@keyframes hold-pulse { to { opacity: 0.3; } }
</style>
