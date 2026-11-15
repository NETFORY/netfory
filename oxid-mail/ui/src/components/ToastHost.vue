<template>
  <div
    class="fixed z-[60] bottom-4 right-4 flex flex-col gap-2 max-w-sm w-full pointer-events-none"
    data-testid="toast-host"
  >
    <transition-group name="toast" tag="div" class="flex flex-col gap-2">
      <div
        v-for="t in toast.items"
        :key="t.id"
        class="pointer-events-auto modal-surface border shadow-2xl rounded-lg px-4 py-3 flex items-start gap-3"
        :class="{
          'border-copper/50': t.kind === 'info',
          'border-emerald-500/50': t.kind === 'success',
          'border-rust/60': t.kind === 'error',
        }"
        :data-testid="`toast-${t.id}`"
      >
        <component
          :is="iconFor(t.kind)"
          class="w-4 h-4 mt-0.5 flex-shrink-0"
          :class="{
            'text-copper': t.kind === 'info',
            'text-emerald-400': t.kind === 'success',
            'text-rust': t.kind === 'error',
          }"
        />
        <div class="flex-1 min-w-0">
          <div v-if="t.title" class="font-mono text-[11px] uppercase tracking-[0.2em] text-iron-100 mb-0.5">
            {{ t.title }}
          </div>
          <div class="text-xs text-iron-100 leading-relaxed break-words">{{ t.text }}</div>
          <!-- Прогресс-бар P2P-раздачи «файл X из Y» — только если задан progress -->
          <div
            v-if="typeof t.progress === 'number'"
            class="mt-2 h-1 w-full rounded-full bg-white/10 overflow-hidden"
            :data-testid="`toast-progress-${t.id}`"
          >
            <div
              class="h-full transition-[width] duration-300 ease-out"
              :class="t.kind === 'success' ? 'bg-emerald-400' : 'bg-copper'"
              :style="{ width: Math.max(0, Math.min(100, t.progress)) + '%' }"
            />
          </div>
        </div>
        <button
          class="text-iron-200 hover:text-rust transition-colors"
          @click="toast.dismiss(t.id)"
          :data-testid="`toast-dismiss-${t.id}`"
        >
          <X class="w-3.5 h-3.5" />
        </button>
      </div>
    </transition-group>
  </div>
</template>

<script setup>
import { X, Info, CheckCircle2, AlertTriangle } from 'lucide-vue-next'
import { useToastStore } from '../stores/toast'

const toast = useToastStore()

function iconFor(kind) {
  if (kind === 'success') return CheckCircle2
  if (kind === 'error') return AlertTriangle
  return Info
}
</script>

<style scoped>
.toast-enter-from {
  opacity: 0;
  transform: translateY(8px);
}
.toast-leave-to {
  opacity: 0;
  transform: translateX(20px);
}
.toast-enter-active,
.toast-leave-active {
  transition: opacity 200ms ease, transform 200ms ease;
}
</style>
