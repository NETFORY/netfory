<script setup lang="ts">
import { defineProps, defineEmits } from 'vue'
// Нижний лист поверх экрана кошелька (размытый фон), закрывается по фону, ✕ или Esc.
defineProps<{ title: string; testid?: string }>()
const emit = defineEmits<{ (e: 'close'): void }>()
</script>

<template>
  <Teleport to="[data-testid='wallet-drawer']">
    <div class="ts-back" @click.self="emit('close')" @keydown.esc="emit('close')" tabindex="-1">
      <section class="ts" :data-testid="testid">
        <i class="ts-grip" />
        <header><b>{{ title }}</b><button @click="emit('close')" aria-label="close" data-testid="token-sheet-close">✕</button></header>
        <div class="ts-body"><slot /></div>
      </section>
    </div>
  </Teleport>
</template>

<style scoped>
.ts-back { position: absolute; inset: 0; z-index: 40; display: flex; align-items: flex-end; background: rgba(5, 5, 8, 0.55); backdrop-filter: blur(6px); animation: ts-fade 0.18s ease-out; }
.ts { width: 100%; max-height: 88%; overflow-y: auto; padding: 8px 16px 18px; border-radius: 18px 18px 0 0; border-top: 1px solid var(--border); background: var(--surface, #16161a); animation: ts-up 0.22s cubic-bezier(0.2, 0.8, 0.2, 1); }
.ts-grip { display: block; width: 38px; height: 4px; margin: 0 auto 10px; border-radius: 2px; background: var(--border); }
.ts header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 12px; }
.ts header b { font-size: 12px; letter-spacing: 0.1em; text-transform: uppercase; }
.ts header button { background: none; border: none; color: var(--text-dim); cursor: pointer; font-size: 14px; }
.ts-body { display: flex; flex-direction: column; gap: 12px; }
@keyframes ts-fade { from { opacity: 0; } }
@keyframes ts-up { from { transform: translateY(40px); opacity: 0; } }
</style>
