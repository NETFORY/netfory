<script setup lang="ts">
import { computed, defineProps, withDefaults } from 'vue'

// Мини-график поминутного трафика (SVG без библиотек): линия + заливка, последняя точка подсвечена.
const props = withDefaults(defineProps<{ data: number[]; color?: string; w?: number; h?: number; testid?: string }>(), { color: '#2ee6a6', w: 120, h: 26 })
const pts = computed(() => {
  const d = props.data.length > 1 ? props.data : [0, ...props.data, 0].slice(-2)
  const max = Math.max(1, ...d)
  const step = props.w / (d.length - 1)
  return d.map((v, i) => [i * step, props.h - 2 - (v / max) * (props.h - 4)] as const)
})
const line = computed(() => pts.value.map(([x, y], i) => `${i ? 'L' : 'M'}${x.toFixed(1)},${y.toFixed(1)}`).join(''))
const area = computed(() => `${line.value}L${props.w},${props.h}L0,${props.h}Z`)
const last = computed(() => pts.value[pts.value.length - 1])
</script>

<template>
  <svg class="spark" :viewBox="`0 0 ${w} ${h}`" :width="w" :height="h" preserveAspectRatio="none" :data-testid="testid">
    <path :d="area" :fill="color" fill-opacity="0.12" />
    <path :d="line" :stroke="color" stroke-width="1.3" fill="none" vector-effect="non-scaling-stroke" />
    <circle :cx="last[0]" :cy="last[1]" r="2" :fill="color" />
  </svg>
</template>

<style scoped>
.spark { display: block; width: 100%; height: 26px; margin-top: 4px; }
.spark path { transition: d 0.4s ease; }
</style>
