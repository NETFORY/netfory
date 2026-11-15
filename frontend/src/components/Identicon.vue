<script setup lang="ts">
/* eslint-disable no-undef */
import { computed } from 'vue'

/**
 * Deterministic neon identicon — a symmetric 5x5 pixel grid + dual-tone neon
 * palette derived purely from the seed string (the sth:// identity address).
 * No network, no uploads: the same address always renders the same avatar.
 */
const props = withDefaults(defineProps<{ seed: string; size?: number }>(), {
  size: 96,
})

// FNV-1a 32-bit — small, fast, deterministic.
function hash(str: string): number {
  let h = 0x811c9dc5
  for (let i = 0; i < str.length; i++) {
    h ^= str.charCodeAt(i)
    h = Math.imul(h, 0x01000193)
  }
  return h >>> 0
}

const model = computed(() => {
  const seed = props.seed || 'sth'
  const h = hash(seed)
  const h2 = hash(seed + '::accent')
  const hue = h % 360
  const hue2 = (hue + 140 + (h2 % 80)) % 360
  const fg = `hsl(${hue}, 95%, 62%)`
  const fg2 = `hsl(${hue2}, 95%, 60%)`

  // 5 columns × 5 rows, mirrored across the vertical axis (3 unique cols).
  const cells: { x: number; y: number; c: string }[] = []
  for (let col = 0; col < 3; col++) {
    for (let row = 0; row < 5; row++) {
      const bit = hash(`${seed}:${col}:${row}`)
      if (bit % 2 === 0) {
        const color = bit % 3 === 0 ? fg2 : fg
        cells.push({ x: col, y: row, c: color })
        if (col < 2) cells.push({ x: 4 - col, y: row, c: color })
      }
    }
  }
  return { cells, hue, fg }
})

const unit = computed(() => props.size / 5)
</script>

<template>
  <svg
    :width="size"
    :height="size"
    viewBox="0 0 5 5"
    class="identicon"
    :style="{ '--glow': model.fg }"
    data-testid="identicon"
    role="img"
    :aria-label="`avatar ${seed}`"
  >
    <rect x="0" y="0" width="5" height="5" fill="#0b0e13" />
    <rect
      v-for="(cell, i) in model.cells"
      :key="i"
      :x="cell.x"
      :y="cell.y"
      width="1"
      height="1"
      :fill="cell.c"
      rx="0.12"
    />
  </svg>
</template>

<style scoped>
.identicon {
  border-radius: 22%;
  border: 1px solid rgba(255, 255, 255, 0.08);
  box-shadow: 0 0 16px -2px var(--glow);
  background: #0b0e13;
  display: block;
}
</style>
