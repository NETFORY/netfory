<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useUpdater } from '@/lib/updater'

const { t } = useI18n()
const { state, openModal } = useUpdater()

// Плашка видна: во время скачивания ИЛИ когда обновление готово, но модалка
// закрыта (пользователь нажал «Позже») — как персистентная кнопка «Обновить».
const visible = computed(
  () =>
    state.phase === 'downloading' ||
    (state.phase === 'ready' && !state.showModal) ||
    state.phase === 'installing',
)
const version = computed(() => state.info?.version || '')
</script>

<template>
  <transition name="upd-slide">
    <div v-if="visible" class="upd-bar" data-testid="update-bar">
      <!-- Скачивание -->
      <template v-if="state.phase === 'downloading'">
        <span class="upd-left">
          <span class="upd-spin">⟳</span>
          <span class="upd-sys">SmartNet // UPDATE{{ version ? ` v${version}` : '' }}</span>
        </span>
        <span class="upd-mid" data-testid="update-bar-status">
          {{ state.statusText || t('updater.fallbackStatus') }}
        </span>
        <span class="upd-progress">
          <span class="upd-progress-track" :class="{ indet: state.indeterminate }">
            <span
              v-if="!state.indeterminate"
              class="upd-progress-fill"
              :style="{ width: `${state.progress}%` }"
            ></span>
            <span v-else class="upd-progress-indet"></span>
          </span>
          <span class="upd-pct" data-testid="update-bar-progress">
            {{ state.indeterminate ? '···' : `${state.progress}%` }}
          </span>
        </span>
      </template>

      <!-- Готово к установке (модалка закрыта) -->
      <template v-else-if="state.phase === 'ready'">
        <span class="upd-left">
          <span class="upd-dot ready"></span>
          <span class="upd-sys">SmartNet // UPDATE</span>
        </span>
        <span class="upd-mid">{{ t('updater.readyBar', { v: version }) }}</span>
        <button class="upd-btn" @click="openModal" data-testid="update-bar-install-btn">
          {{ t('updater.updateBtn') }}
        </button>
      </template>

      <!-- Установка -->
      <template v-else>
        <span class="upd-left">
          <span class="upd-spin">⟳</span>
          <span class="upd-sys">SmartNet // UPDATE</span>
        </span>
        <span class="upd-mid">{{ t('updater.installing') }}</span>
      </template>
    </div>
  </transition>
</template>

<style scoped>
.upd-bar {
  position: fixed;
  left: 0;
  right: 0;
  bottom: 0;
  height: 34px;
  z-index: 10000;
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 0 16px;
  background: linear-gradient(180deg, #071a2a, #04101a);
  border-top: 1px solid #0e7490;
  box-shadow: 0 -6px 18px rgba(0, 0, 0, 0.5);
  font-family: 'JetBrains Mono', monospace;
  user-select: none;
}
.upd-left {
  display: flex;
  align-items: center;
  gap: 9px;
  flex-shrink: 0;
}
.upd-sys {
  font-size: 10px;
  letter-spacing: 0.14em;
  color: #67e8f9;
  text-transform: uppercase;
}
.upd-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #22d3ee;
  box-shadow: 0 0 8px #22d3ee;
}
.upd-dot.ready {
  background: #34d399;
  box-shadow: 0 0 8px #34d399;
  animation: upd-pulse 1.6s ease-in-out infinite;
}
@keyframes upd-pulse {
  0%,
  100% {
    opacity: 1;
  }
  50% {
    opacity: 0.4;
  }
}
.upd-spin {
  font-size: 14px;
  color: #22d3ee;
  display: inline-block;
  animation: upd-rot 1s linear infinite;
}
@keyframes upd-rot {
  to {
    transform: rotate(360deg);
  }
}
.upd-mid {
  flex: 1;
  font-size: 11px;
  color: #cbd5e1;
  letter-spacing: 0.03em;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.upd-progress {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-shrink: 0;
}
.upd-progress-track {
  width: 180px;
  height: 6px;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.08);
  overflow: hidden;
}
.upd-progress-fill {
  display: block;
  height: 100%;
  background: linear-gradient(90deg, #0891b2, #22d3ee);
  transition: width 0.3s ease;
}
.upd-progress-track.indet {
  position: relative;
}
.upd-progress-indet {
  position: absolute;
  top: 0;
  left: 0;
  height: 100%;
  width: 38%;
  border-radius: 999px;
  background: linear-gradient(90deg, transparent, #22d3ee, transparent);
  animation: upd-indet 1.15s ease-in-out infinite;
}
@keyframes upd-indet {
  0% {
    left: -40%;
  }
  100% {
    left: 100%;
  }
}
.upd-pct {
  font-size: 11px;
  font-weight: 700;
  color: #67e8f9;
  font-variant-numeric: tabular-nums;
  min-width: 38px;
  text-align: right;
}
.upd-btn {
  flex-shrink: 0;
  padding: 6px 16px;
  border-radius: 999px;
  border: 1px solid #22d3ee;
  background: rgba(34, 211, 238, 0.12);
  color: #67e8f9;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.04em;
  cursor: pointer;
  transition:
    background-color 0.14s ease,
    transform 0.12s ease;
}
.upd-btn:hover {
  background: rgba(34, 211, 238, 0.22);
}
.upd-btn:active {
  transform: translateY(1px);
}
.upd-slide-enter-active,
.upd-slide-leave-active {
  transition:
    transform 0.28s ease,
    opacity 0.28s ease;
}
.upd-slide-enter-from,
.upd-slide-leave-to {
  transform: translateY(100%);
  opacity: 0;
}
</style>
