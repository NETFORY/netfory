<script setup lang="ts">
/* eslint-disable no-undef */
/* Long-press confirmation button: hold to fill the bar, fires @complete at 100%. */
import { ref, onBeforeUnmount } from 'vue'

const props = withDefaults(
  defineProps<{
    label: string
    holdingLabel?: string
    duration?: number
    disabled?: boolean
    danger?: boolean
    testid?: string
  }>(),
  { duration: 1100, disabled: false, danger: false, holdingLabel: '' },
)
const emit = defineEmits<{ (e: 'complete'): void }>()

const progress = ref(0)
const holding = ref(false)
let raf = 0
let startTs = 0

function step(ts: number) {
  if (!holding.value) return
  if (!startTs) startTs = ts
  progress.value = Math.min(100, ((ts - startTs) / props.duration) * 100)
  if (progress.value >= 100) {
    reset()
    emit('complete')
    return
  }
  raf = requestAnimationFrame(step)
}
function begin() {
  if (props.disabled) return
  holding.value = true
  startTs = 0
  raf = requestAnimationFrame(step)
}
function reset() {
  holding.value = false
  startTs = 0
  progress.value = 0
  cancelAnimationFrame(raf)
}
onBeforeUnmount(() => cancelAnimationFrame(raf))
</script>

<template>
  <button
    class="hold"
    :class="{ danger, disabled }"
    :disabled="disabled"
    @pointerdown.prevent="begin"
    @pointerup="reset"
    @pointerleave="reset"
    @pointercancel="reset"
    :data-testid="testid"
  >
    <span class="hold-fill" :style="{ width: progress + '%' }" />
    <span class="hold-label">{{ holding && holdingLabel ? holdingLabel : label }}</span>
  </button>
</template>

<style scoped>
.hold {
  position: relative;
  width: 100%;
  height: 48px;
  border: 1px solid var(--accent);
  border-radius: var(--radius);
  background: var(--accent-soft);
  color: var(--accent);
  font-size: 12px;
  font-weight: var(--fw-800);
  letter-spacing: 0.14em;
  text-transform: uppercase;
  overflow: hidden;
  cursor: pointer;
  user-select: none;
  touch-action: none;
}
.hold.danger {
  border-color: var(--danger);
  color: var(--danger);
}
.hold.disabled {
  opacity: 0.45;
  cursor: not-allowed;
}
.hold-fill {
  position: absolute;
  left: 0;
  top: 0;
  bottom: 0;
  background: var(--accent);
  opacity: 0.28;
}
.hold.danger .hold-fill {
  background: var(--danger);
}
.hold-label {
  position: relative;
  z-index: 1;
}
</style>
