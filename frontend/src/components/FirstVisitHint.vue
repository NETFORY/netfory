<script setup lang="ts">
/* eslint-disable no-undef */
// Ненавязчивая подсказка при первом открытии раздела. Показывается один раз,
// закрытие запоминается в localStorage по ключу `storageKey`.
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{ storageKey: string; titleKey: string; textKey: string }>()
const { t } = useI18n()

const visible = ref(false)
try {
  visible.value = !localStorage.getItem(props.storageKey)
} catch {
  visible.value = false
}
function dismiss() {
  try {
    localStorage.setItem(props.storageKey, '1')
  } catch {
    /* noop */
  }
  visible.value = false
}
</script>

<template>
  <div v-if="visible" class="fv-hint" :data-testid="`hint-${props.storageKey}`">
    <span class="fv-ico" aria-hidden="true">
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <path d="M9 18h6" />
        <path d="M10 22h4" />
        <path d="M12 2a7 7 0 0 0-4 12.7c.6.5 1 1.4 1 2.3h6c0-.9.4-1.8 1-2.3A7 7 0 0 0 12 2z" />
      </svg>
    </span>
    <div class="fv-body">
      <b class="fv-title">{{ t(titleKey) }}</b>
      <span class="fv-text">{{ t(textKey) }}</span>
    </div>
    <button class="fv-gotit" @click="dismiss" :data-testid="`hint-gotit-${props.storageKey}`">
      {{ t('hints.gotIt') }}
    </button>
  </div>
</template>

<style scoped>
.fv-hint {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 16px;
  border-radius: var(--radius-card, 12px);
  background: var(--accent-soft, rgba(46, 227, 198, 0.08));
  border: 1px solid var(--border-accent, transparent);
  margin-bottom: 4px;
  animation: fv-in 0.3s ease;
}
@keyframes fv-in {
  from {
    opacity: 0;
    transform: translateY(-6px);
  }
  to {
    opacity: 1;
    transform: none;
  }
}
.fv-ico {
  color: var(--accent);
  flex-shrink: 0;
  display: flex;
}
.fv-body {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.fv-title {
  font-size: 13px;
  color: var(--text);
}
.fv-text {
  font-size: 12.5px;
  line-height: 1.5;
  color: var(--text-dim);
}
.fv-gotit {
  margin-left: auto;
  flex-shrink: 0;
  background: transparent;
  border: 1px solid var(--border-accent, var(--accent));
  color: var(--accent);
  font-size: 12px;
  padding: 7px 16px;
  border-radius: var(--radius-btn, 10px);
  cursor: pointer;
  transition: background-color 0.15s ease, color 0.15s ease;
}
.fv-gotit:hover {
  background: var(--accent);
  color: #0c1f1b;
}
</style>
