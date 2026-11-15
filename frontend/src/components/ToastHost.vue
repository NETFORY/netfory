<script setup lang="ts">
import { useToastsStore } from '@/store/toasts'

const toasts = useToastsStore()
</script>

<template>
  <div class="toast-host" data-testid="toast-host">
    <TransitionGroup name="toast">
      <div
        v-for="t in toasts.items"
        :key="t.id"
        class="toast"
        :class="[`k-${t.kind}`, { clickable: !!t.onOpen }]"
        data-testid="toast"
        @click="toasts.open(t.id)"
      >
        <span class="ic">{{ t.kind === 'in' ? '↓' : t.kind === 'error' ? '⚠' : '◈' }}</span>
        <div class="body">
          <div class="t-title" data-testid="toast-title">{{ t.title }}</div>
          <div class="t-body" data-testid="toast-body">{{ t.body }}</div>
          <div v-if="t.onOpen" class="t-hint dim">нажмите, чтобы открыть →</div>
        </div>
      </div>
    </TransitionGroup>
  </div>
</template>

<style scoped>
.toast-host {
  position: fixed;
  top: 18px;
  right: 18px;
  z-index: 9999;
  display: flex;
  flex-direction: column;
  gap: 10px;
  pointer-events: none;
}
.toast {
  pointer-events: auto;
  display: flex;
  gap: 12px;
  align-items: flex-start;
  min-width: 280px;
  max-width: 380px;
  padding: 13px 15px;
  border-radius: 12px;
  background: rgba(13, 18, 26, 0.92);
  border: 1px solid var(--border);
  backdrop-filter: blur(14px);
  box-shadow: 0 12px 40px rgba(0, 0, 0, 0.45);
  cursor: pointer;
  border-left: 3px solid var(--accent);
}
.toast.k-in {
  border-left-color: #34d399;
  box-shadow: 0 0 30px rgba(52, 211, 153, 0.18), 0 12px 40px rgba(0, 0, 0, 0.45);
}
.toast.k-error {
  border-left-color: var(--danger);
}
.ic {
  font-size: 18px;
  font-weight: var(--fw-800);
  line-height: 1.2;
  color: #34d399;
}
.toast.k-info .ic {
  color: var(--accent);
}
.toast.k-error .ic {
  color: var(--danger);
}
.body {
  flex: 1;
  min-width: 0;
}
.t-title {
  font-size: 12px;
  font-weight: var(--fw-800);
  letter-spacing: 0.04em;
  color: var(--text);
}
.t-body {
  font-size: 12px;
  color: var(--text-dim);
  margin-top: 3px;
  word-break: break-word;
}
.t-hint {
  font-size: 10px;
  letter-spacing: 0.04em;
  margin-top: 5px;
  color: var(--accent);
  opacity: 0.8;
}
.toast.clickable {
  border-left-width: 4px;
}
.toast.clickable:hover {
  transform: translateX(-2px);
  border-left-color: #fbbf24;
}
.toast-enter-active,
.toast-leave-active {
  transition: opacity 0.3s ease, transform 0.3s ease;
}
.toast-enter-from {
  opacity: 0;
  transform: translateX(40px);
}
.toast-leave-to {
  opacity: 0;
  transform: translateX(40px);
}
</style>
