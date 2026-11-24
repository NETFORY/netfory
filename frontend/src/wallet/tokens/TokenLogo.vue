<script setup lang="ts">
import { defineProps, onMounted, ref, watch } from 'vue'
import { cachedLogo, tokenLogo, type TokenInfo } from './api'

// Логотип токена: локальный кэш → logoUrl узла → цветная заглушка с буквами тикера.
const props = defineProps<{ id: string; symbol: string; info?: TokenInfo | null; size?: number; square?: boolean }>()
const src = ref(cachedLogo(props.id) || '')
const hue = [...props.id].reduce((a, c) => (a * 31 + c.charCodeAt(0)) % 360, 7)
async function load() { if (!src.value) src.value = await tokenLogo(props.id, props.info) }
onMounted(load)
watch(() => props.info, load)
</script>

<template>
  <span class="tl" :class="{ sq: square }" :style="{ width: `${size ?? 36}px`, height: `${size ?? 36}px`, background: src ? 'transparent' : `hsl(${hue} 60% 22%)`, color: `hsl(${hue} 80% 70%)` }">
    <img v-if="src" :src="src" :alt="symbol" @error="src = ''" />
    <b v-else :style="{ fontSize: `${(size ?? 36) * 0.34}px` }">{{ symbol.slice(0, 3) }}</b>
  </span>
</template>

<style scoped>
.tl { display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0; border-radius: 22%; overflow: hidden; border: 1px solid var(--border); background-color: var(--bg); }
.tl.sq { border-radius: 18%; }
.tl img { width: 100%; height: 100%; object-fit: cover; }
.tl b { font-family: var(--font-mono); letter-spacing: 0.02em; }
</style>
