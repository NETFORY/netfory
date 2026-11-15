<script setup lang="ts">
// Глобальная нижняя плашка загрузки языкового пакета переводчика.
// Слушает событие `translator-progress` (фоновая торрент-загрузка модели,
// стартует автоматически через ~2 мин после запуска) и показывает прогресс
// в строке состояния внизу — независимо от того, на какой странице юзер.
import { ref, computed, onMounted, onBeforeUnmount } from 'vue'
import { useI18n } from 'vue-i18n'
import { isTauri, onTranslatorProgress, translatorStatus, translatorProgress, type TranslatorProgress } from '@/lib/bridge'

const { t } = useI18n()
const progress = ref<TranslatorProgress | null>(null)
const installed = ref(true) // по умолчанию скрыто, пока не узнаем обратное
let unlisten: (() => void) | null = null
let hideTimer: ReturnType<typeof setTimeout> | null = null

const active = computed(() => {
  const p = progress.value
  if (!p || installed.value) return false
  return p.phase === 'torrent' || p.phase === 'model' || p.phase === 'tokenizer'
})
const pct = computed(() => Math.max(0, Math.min(100, progress.value?.pct || 0)))
const hasPct = computed(() => pct.value > 0 && (progress.value?.total_mb || 0) > 0)

onMounted(async () => {
  if (!isTauri) return
  try {
    const st = await translatorStatus()
    installed.value = st.installed
    // восстанавливаем индикатор уже идущей загрузки (после навигации)
    const p = await translatorProgress()
    if (p) {
      progress.value = p
      if (p.phase === 'done') installed.value = true
    }
  } catch {
    /* ignore */
  }
  unlisten = await onTranslatorProgress((p) => {
    progress.value = p
    if (p.phase === 'done') {
      installed.value = true
      if (hideTimer) clearTimeout(hideTimer)
    } else if (p.phase !== 'error') {
      installed.value = false
    }
  })
})
onBeforeUnmount(() => {
  unlisten?.()
  if (hideTimer) clearTimeout(hideTimer)
})
</script>

<template>
  <transition name="trb-slide">
    <div v-if="active" class="trb-bar" data-testid="translator-bar">
      <span class="trb-left">
        <span class="trb-spin">🧠</span>
        <span class="trb-sys">SmartNet // AI-TRANSLATOR</span>
      </span>
      <span class="trb-mid" data-testid="translator-bar-status">
        {{ t('wall.trDownloading') }}<template v-if="hasPct"> · {{ progress?.downloaded_mb }} / {{ progress?.total_mb }} MB</template>
      </span>
      <span class="trb-progress">
        <span class="trb-track" :class="{ indet: !hasPct }">
          <span v-if="hasPct" class="trb-fill" :style="{ width: `${pct}%` }"></span>
          <span v-else class="trb-indet"></span>
        </span>
        <span class="trb-pct" data-testid="translator-bar-progress">
          {{ hasPct ? `${pct.toFixed(0)}%` : '···' }}
        </span>
      </span>
    </div>
  </transition>
</template>

<style scoped>
.trb-bar {
  position: fixed;
  left: 0;
  right: 0;
  bottom: 0;
  height: 34px;
  z-index: 9998;
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 0 16px;
  background: linear-gradient(180deg, #170b2a, #0a0518);
  border-top: 1px solid #7c3aed;
  box-shadow: 0 -6px 18px rgba(0, 0, 0, 0.5);
  font-family: 'JetBrains Mono', monospace;
  user-select: none;
}
.trb-left { display: flex; align-items: center; gap: 9px; flex-shrink: 0; }
.trb-sys { font-size: 10px; letter-spacing: 0.14em; color: #c4b5fd; text-transform: uppercase; }
.trb-spin { font-size: 14px; display: inline-block; animation: trb-pulse 1.4s ease-in-out infinite; }
@keyframes trb-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.45; } }
.trb-mid {
  flex: 1; font-size: 11px; color: #ddd6fe; letter-spacing: 0.03em;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.trb-progress { display: flex; align-items: center; gap: 10px; flex-shrink: 0; }
.trb-track { width: 180px; height: 6px; border-radius: 999px; background: rgba(255, 255, 255, 0.08); overflow: hidden; }
.trb-fill { display: block; height: 100%; background: linear-gradient(90deg, #7c3aed, #00e5ff); transition: width 0.3s ease; }
.trb-track.indet { position: relative; }
.trb-indet {
  position: absolute; top: 0; left: 0; height: 100%; width: 38%; border-radius: 999px;
  background: linear-gradient(90deg, transparent, #a855f7, transparent);
  animation: trb-indet 1.15s ease-in-out infinite;
}
@keyframes trb-indet { 0% { left: -40%; } 100% { left: 100%; } }
.trb-pct {
  font-size: 11px; font-weight: 700; color: #c4b5fd;
  font-variant-numeric: tabular-nums; min-width: 38px; text-align: right;
}
.trb-slide-enter-active, .trb-slide-leave-active { transition: transform 0.28s ease, opacity 0.28s ease; }
.trb-slide-enter-from, .trb-slide-leave-to { transform: translateY(100%); opacity: 0; }
</style>
