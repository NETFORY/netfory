<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount } from 'vue'
import { useI18n } from 'vue-i18n'
import { syncStatus } from '@/lib/bridge'

const { t } = useI18n()
const done = ref(0)
const total = ref(5)
const visible = ref(false)
const dismissed = ref(false)
let timer: ReturnType<typeof setInterval> | null = null
const startedAt = Date.now()
const MAX_MS = 20000

const pct = computed(() => (total.value ? Math.round((done.value / total.value) * 100) : 100))
const canSkip = ref(false)

async function poll() {
  try {
    const s = await syncStatus()
    done.value = s.done
    total.value = s.total
    // Only surface the splash if the very first scan is still running.
    if (!s.complete && !dismissed.value) visible.value = true
    if (s.complete || Date.now() - startedAt > MAX_MS) {
      visible.value = false
      if (timer) clearInterval(timer)
    }
  } catch {
    visible.value = false
    if (timer) clearInterval(timer)
  }
}

function skip() {
  dismissed.value = true
  visible.value = false
  if (timer) clearInterval(timer)
}

onMounted(() => {
  poll()
  timer = setInterval(poll, 900)
  setTimeout(() => (canSkip.value = true), 4000)
})
onBeforeUnmount(() => {
  if (timer) clearInterval(timer)
})
</script>

<template>
  <transition name="sync-fade">
    <div v-if="visible" class="sync-splash" data-testid="sync-splash">
      <div class="sync-card">
        <div class="sync-orbit">
          <span class="sync-core"></span>
          <span class="sync-ring"></span>
        </div>
        <h2 class="sync-title" data-testid="sync-splash-title">{{ t('sync.title') }}</h2>
        <p class="sync-sub">{{ t('sync.sub') }}</p>
        <div class="sync-bar">
          <div class="sync-bar-fill" :style="{ width: pct + '%' }" data-testid="sync-splash-bar"></div>
        </div>
        <p class="sync-count" data-testid="sync-splash-count">{{ done }} / {{ total }} · {{ pct }}%</p>
        <button v-if="canSkip" class="sync-skip" @click="skip" data-testid="sync-splash-skip">
          {{ t('sync.skip') }}
        </button>
      </div>
    </div>
  </transition>
</template>

<style scoped>
.sync-splash {
  position: fixed;
  inset: 0;
  z-index: 9000;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(3, 7, 11, 0.92);
  backdrop-filter: blur(10px);
}
.sync-card {
  width: min(420px, 90vw);
  text-align: center;
  padding: 40px 32px;
  border: 1px solid var(--accent, #2dd4bf);
  border-radius: 18px;
  background: rgba(8, 18, 22, 0.96);
  box-shadow: 0 0 80px -24px var(--accent, #2dd4bf);
}
.sync-orbit {
  position: relative;
  width: 64px;
  height: 64px;
  margin: 0 auto 18px;
}
.sync-core {
  position: absolute;
  inset: 26px;
  border-radius: 50%;
  background: var(--accent, #2dd4bf);
  box-shadow: 0 0 18px var(--accent, #2dd4bf);
  animation: sync-pulse 1.4s ease-in-out infinite;
}
.sync-ring {
  position: absolute;
  inset: 0;
  border: 2px solid transparent;
  border-top-color: var(--accent, #2dd4bf);
  border-right-color: var(--accent, #2dd4bf);
  border-radius: 50%;
  animation: sync-spin 1s linear infinite;
}
@keyframes sync-spin { to { transform: rotate(360deg); } }
@keyframes sync-pulse { 0%, 100% { opacity: 0.4; transform: scale(0.85); } 50% { opacity: 1; transform: scale(1); } }
.sync-title {
  font-size: 18px;
  font-weight: 800;
  color: var(--text, #e7faf6);
  letter-spacing: 0.02em;
}
.sync-sub {
  margin-top: 8px;
  font-size: 12px;
  line-height: 1.6;
  color: var(--text-dim, #6f8c96);
}
.sync-bar {
  margin-top: 20px;
  height: 6px;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.08);
  overflow: hidden;
}
.sync-bar-fill {
  height: 100%;
  border-radius: 999px;
  background: var(--accent, #2dd4bf);
  box-shadow: 0 0 12px var(--accent, #2dd4bf);
  transition: width 0.5s ease;
}
.sync-count {
  margin-top: 10px;
  font-size: 12px;
  font-family: var(--font-mono, monospace);
  color: var(--accent, #2dd4bf);
}
.sync-skip {
  margin-top: 18px;
  background: transparent;
  border: 1px solid var(--border, #1d2b30);
  color: var(--text-dim, #6f8c96);
  font-size: 11px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  padding: 8px 16px;
  border-radius: 8px;
  cursor: pointer;
  transition: border-color 0.15s, color 0.15s;
}
.sync-skip:hover {
  border-color: var(--accent, #2dd4bf);
  color: var(--accent, #2dd4bf);
}
.sync-fade-enter-active, .sync-fade-leave-active { transition: opacity 0.4s ease; }
.sync-fade-enter-from, .sync-fade-leave-to { opacity: 0; }
</style>

<style>
/* Светлая тема default-light: светлый оверлей и белая карточка,
   чтобы тёмный текст темы не сливался с тёмным фоном. */
html[data-theme='default-light'] .sync-splash {
  background: rgba(240, 242, 247, 0.94);
}
html[data-theme='default-light'] .sync-card {
  background: #ffffff;
  border-color: rgba(14, 147, 132, 0.35);
  box-shadow: 0 16px 48px -16px rgba(14, 147, 132, 0.35), 0 2px 10px rgba(30, 41, 59, 0.08);
}
html[data-theme='default-light'] .sync-title {
  color: #202124;
}
html[data-theme='default-light'] .sync-sub {
  color: #5f6368;
}
html[data-theme='default-light'] .sync-bar {
  background: rgba(30, 41, 59, 0.1);
}
html[data-theme='default-light'] .sync-bar-fill {
  box-shadow: 0 0 8px rgba(14, 147, 132, 0.4);
}
html[data-theme='default-light'] .sync-skip {
  border-color: rgba(30, 41, 59, 0.18);
  color: #5f6368;
}
</style>
