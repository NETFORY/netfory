<script setup lang="ts">
/* eslint-disable no-undef */
import { computed, nextTick, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useTerminalStore } from '@/store/terminal'

const props = defineProps<{ appId: string }>()
const terminal = useTerminalStore()
const { t } = useI18n()

const logs = computed(() => terminal.logsFor(props.appId))
const shortId = computed(() => {
  const id = props.appId || ''
  return id.length > 14 ? `${id.slice(0, 8)}…${id.slice(-4)}` : id
})

const bodyRef = ref<HTMLElement | null>(null)
function scrollToBottom() {
  nextTick(() => {
    const el = bodyRef.value
    if (el) el.scrollTop = el.scrollHeight
  })
}
watch(() => logs.value.length, scrollToBottom)
watch(() => props.appId, scrollToBottom)
</script>

<template>
  <div class="terminal" data-testid="terminal-drawer">
    <div class="term-head">
      <span class="term-title">
        TERMINAL LOGS <span class="term-sep">//</span> APP_ID:
        <span class="term-app" data-testid="terminal-app-id">{{ shortId }}</span>
      </span>
      <div class="term-actions">
        <button class="term-btn" @click="terminal.clear(appId)" data-testid="terminal-clear-btn">
          {{ t('terminal.clear') }}
        </button>
        <button class="term-btn close" @click="terminal.close()" data-testid="terminal-close-btn">✕</button>
      </div>
    </div>
    <div ref="bodyRef" class="term-body" data-testid="terminal-body">
      <p v-if="!logs.length" class="term-empty" data-testid="terminal-empty">{{ t('terminal.empty') }}</p>
      <div
        v-for="(line, i) in logs"
        :key="i"
        class="term-line"
        :class="`lt-${line.type}`"
        data-testid="terminal-line"
      >{{ line.text }}</div>
    </div>
  </div>
</template>

<style scoped>
.terminal {
  position: absolute;
  left: 0;
  right: 0;
  bottom: 0;
  height: 30%;
  min-height: 150px;
  display: flex;
  flex-direction: column;
  background: rgba(10, 15, 29, 0.96);
  backdrop-filter: blur(10px);
  border-top: 1px solid rgba(0, 229, 255, 0.32);
  box-shadow: 0 -12px 32px rgba(0, 0, 0, 0.45);
  z-index: 8;
  font-family: 'JetBrains Mono', ui-monospace, monospace;
}
.term-head {
  height: 30px;
  min-height: 30px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 12px;
  background: #0d1527;
  border-bottom: 1px solid rgba(0, 229, 255, 0.14);
}
.term-title {
  font-size: 11px;
  letter-spacing: 0.06em;
  color: #34d3ee;
}
.term-sep {
  opacity: 0.5;
}
.term-app {
  color: #7fe7ff;
}
.term-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}
.term-btn {
  font-size: 10px;
  letter-spacing: 0.08em;
  color: #5fd0e6;
  padding: 2px 8px;
  border: 1px solid rgba(0, 229, 255, 0.28);
  border-radius: 4px;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease;
}
.term-btn:hover {
  color: #d6fbff;
  border-color: rgba(0, 229, 255, 0.6);
  background: rgba(0, 229, 255, 0.08);
}
.term-btn.close:hover {
  color: #fff;
  border-color: #fb7185;
  background: rgba(251, 113, 133, 0.12);
}
.term-body {
  flex: 1;
  overflow-y: auto;
  padding: 8px 12px;
  font-size: 11.5px;
  line-height: 1.55;
  user-select: text;
  -webkit-user-select: text;
}
.term-empty {
  color: #3f5a6e;
  font-style: italic;
}
.term-line {
  white-space: pre-wrap;
  word-break: break-word;
}
.lt-system { color: #22d3ee; }
.lt-crypto { color: #34d399; }
.lt-success { color: #34d399; }
.lt-network { color: #fbbf24; }
.lt-error { color: #fb7185; }
.lt-info { color: #8aa6bd; }
</style>
