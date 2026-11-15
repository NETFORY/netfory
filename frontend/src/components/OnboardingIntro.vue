<script setup lang="ts">
/* eslint-disable no-undef */
// Дружелюбный экран знакомства (первый запуск): 3 шага —
// приватность, приложения, облако. Показывается один раз
// (localStorage `sth_onboarding_seen`), закрывается «Пропустить»/«Начать».
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useUiStore } from '@/store/ui'

const emit = defineEmits<{ (e: 'close'): void }>()
const { t } = useI18n()
const ui = useUiStore()
const step = ref(0)

const steps = ['privacy', 'apps', 'cloud'] as const

function next() {
  if (step.value < steps.length - 1) step.value += 1
  else finish()
}
function finish() {
  try {
    localStorage.setItem('sth_onboarding_seen', '1')
  } catch {
    /* noop */
  }
  emit('close')
}

// Регистрируем оверлей, чтобы нативные child-webview спрятались под ним.
onMounted(() => ui.openOverlay())
onBeforeUnmount(() => ui.closeOverlay())
</script>

<template>
  <div class="ob-backdrop" data-testid="onboarding-modal">
    <div class="ob-card" role="dialog" aria-modal="true">
      <span class="ob-welcome">{{ t('onboarding.welcome') }}</span>

      <div class="ob-art" :class="`ob-art-${steps[step]}`" aria-hidden="true">
        <!-- privacy: щит -->
        <svg v-if="steps[step] === 'privacy'" viewBox="0 0 24 24" width="44" height="44" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
          <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
          <path d="M9 12l2 2 4-4" />
        </svg>
        <!-- apps: сетка приложений -->
        <svg v-else-if="steps[step] === 'apps'" viewBox="0 0 24 24" width="44" height="44" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
          <rect x="3" y="3" width="7" height="7" rx="1.5" />
          <rect x="14" y="3" width="7" height="7" rx="1.5" />
          <rect x="3" y="14" width="7" height="7" rx="1.5" />
          <rect x="14" y="14" width="7" height="7" rx="1.5" />
        </svg>
        <!-- cloud: облако -->
        <svg v-else viewBox="0 0 24 24" width="44" height="44" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
          <path d="M17.5 19a4.5 4.5 0 0 0 .42-8.98 6 6 0 0 0-11.7 1.62A3.75 3.75 0 0 0 6.75 19h10.75z" />
        </svg>
      </div>

      <h2 class="ob-title" data-testid="onboarding-title">{{ t(`onboarding.${steps[step]}Title`) }}</h2>
      <p class="ob-text">{{ t(`onboarding.${steps[step]}Text`) }}</p>

      <div class="ob-dots" aria-hidden="true">
        <span
          v-for="(s, i) in steps"
          :key="s"
          class="ob-dot"
          :class="{ on: i === step }"
        />
      </div>

      <div class="ob-actions">
        <button class="ob-skip" @click="finish" data-testid="onboarding-skip-btn">
          {{ t('onboarding.skip') }}
        </button>
        <button class="ob-next" @click="next" data-testid="onboarding-next-btn">
          {{ step < steps.length - 1 ? t('onboarding.next') : t('onboarding.start') }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.ob-backdrop {
  position: fixed;
  inset: 0;
  z-index: 4000;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.55);
  backdrop-filter: blur(6px);
}
.ob-card {
  width: min(420px, calc(100vw - 48px));
  background: var(--panel-solid, var(--panel));
  border-radius: 16px;
  box-shadow: var(--shadow, 0 8px 30px rgba(0, 0, 0, 0.4));
  padding: 36px 32px 28px;
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  animation: ob-rise 0.35s cubic-bezier(0.16, 1, 0.3, 1);
}
@keyframes ob-rise {
  from {
    opacity: 0;
    transform: translateY(14px) scale(0.98);
  }
  to {
    opacity: 1;
    transform: none;
  }
}
.ob-welcome {
  font-size: 11px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--text-dim);
  margin-bottom: 20px;
}
.ob-art {
  width: 88px;
  height: 88px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  margin-bottom: 20px;
  transition: background-color 0.25s ease, color 0.25s ease;
}
.ob-art-privacy {
  color: #a78bfa;
  background: rgba(167, 139, 250, 0.12);
}
.ob-art-apps {
  color: #f5b301;
  background: rgba(245, 179, 1, 0.12);
}
.ob-art-cloud {
  color: var(--accent);
  background: var(--accent-soft, rgba(46, 227, 198, 0.12));
}
.ob-title {
  font-size: 20px;
  font-weight: var(--fw-700, 700);
  color: var(--text);
  margin: 0 0 10px;
}
.ob-text {
  font-size: 13.5px;
  line-height: 1.65;
  color: var(--text-dim);
  margin: 0 0 22px;
  min-height: 66px;
}
.ob-dots {
  display: flex;
  gap: 7px;
  margin-bottom: 24px;
}
.ob-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--border-subtle, rgba(128, 128, 128, 0.3));
  transition: background-color 0.2s ease, transform 0.2s ease;
}
.ob-dot.on {
  background: var(--accent);
  transform: scale(1.25);
}
.ob-actions {
  display: flex;
  gap: 10px;
  width: 100%;
  justify-content: space-between;
  align-items: center;
}
.ob-skip {
  background: transparent;
  border: none;
  color: var(--text-dim);
  font-size: 13px;
  cursor: pointer;
  padding: 10px 14px;
  border-radius: var(--radius-btn, 10px);
  transition: color 0.15s ease, background-color 0.15s ease;
}
.ob-skip:hover {
  color: var(--text);
  background: var(--panel-hover);
}
.ob-next {
  background: var(--accent);
  color: #0c1f1b;
  border: none;
  font-size: 13px;
  font-weight: var(--fw-700, 700);
  letter-spacing: 0.04em;
  cursor: pointer;
  padding: 11px 26px;
  border-radius: var(--radius-btn, 10px);
  transition: background-color 0.15s ease, box-shadow 0.15s ease, transform 0.15s ease;
}
.ob-next:hover {
  background: var(--accent-hover, var(--accent));
  box-shadow: var(--shadow-sm);
}
html[data-theme='default-light'] .ob-next {
  color: #ffffff;
}
</style>
