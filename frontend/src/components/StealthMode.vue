<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { getStealthStatus, toggleStealthMode, getRelayConfig } from '@/lib/bridge'

const router = useRouter()
const { t, locale } = useI18n()

const enabled = ref(false)
const busy = ref(false)
const relays = ref<string[]>([])
const message = ref('')
const isError = ref(false)

const ru = computed(() => String(locale.value).startsWith('ru'))

/**
 * Бэкенд (stealth.rs) возвращает жёстко зашитые русские строки. Здесь мы
 * распознаём их по ключевым фрагментам и подставляем перевод из i18n для
 * текущей локали (en / id / ru).
 */
function localizeBackendMessage(raw: string): string {
  if (!raw) return ''
  if (raw.includes('Укажите хотя бы один n1-relay')) return t('stealth.msgNoRelays')
  if (raw.includes('Стелс-режим включён')) return t('stealth.msgEnabled')
  return raw
}

const T = computed(() => ({
  title: 'Стелс-режим',
  titleEn: 'Stealth Mode',
  badge: ru.value ? 'anti-DPI · relay-only' : 'anti-DPI · relay-only',
  on: ru.value ? 'Активен' : 'Active',
  off: ru.value ? 'Отключён' : 'Disabled',
  hint: ru.value
    ? '— изоляция трафика строго через ваши n1-реле (TLS/443), обфускация против ТСПУ/DPI'
    : '— traffic isolated strictly via your n1 relays (TLS/443), anti-DPI obfuscation',
  noRelays: ru.value
    ? 'Добавьте n1-relay серверы в Настройках → Сеть'
    : 'Add n1-relay servers in Settings → Network',
}))

async function load() {
  try {
    enabled.value = await getStealthStatus()
    const cfg = await getRelayConfig()
    relays.value = cfg.urls || []
  } catch {
    /* web preview / not available */
  }
}

async function onToggle() {
  if (busy.value) return
  const next = !enabled.value
  busy.value = true
  message.value = ''
  isError.value = false
  try {
    const cfg = await getRelayConfig()
    relays.value = cfg.urls || []
    const msg = await toggleStealthMode(next, relays.value)
    enabled.value = next
    message.value = localizeBackendMessage(msg)
    isError.value = false
  } catch (e: any) {
    isError.value = true
    message.value = localizeBackendMessage(e?.message || String(e))
  } finally {
    busy.value = false
  }
}

onMounted(load)
</script>

<template>
  <div
    class="stealth-block rise"
    :class="{ active: enabled }"
    :style="{ animationDelay: '320ms' }"
    data-testid="dashboard-stealth"
  >
    <div class="st-head">
      <span class="st-glyph" aria-hidden="true">
        <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
          <path d="M12 2 4 5v6c0 5 3.5 8.5 8 11 4.5-2.5 8-6 8-11V5l-8-3z" />
          <path d="M9 12l2 2 4-4" />
        </svg>
      </span>
      <div class="st-titlewrap">
        <h2 class="st-title">{{ ru ? T.title : T.titleEn }}</h2>
        <span class="st-badge" data-testid="stealth-badge">{{ T.badge }}</span>
      </div>
    </div>

    <div class="st-status">
      <span class="st-dot" :class="{ on: enabled }" />
      <span class="st-state" data-testid="stealth-state">{{ enabled ? T.on : T.off }}</span>
    </div>

    <p class="st-hint">{{ T.hint }}</p>

    <p
      v-if="!relays.length"
      class="st-warn"
      data-testid="stealth-no-relays"
      @click="router.push({ name: 'settings' })"
    >
      ⚠ {{ T.noRelays }}
    </p>

    <p
      v-if="message"
      class="st-msg"
      :class="{ err: isError }"
      data-testid="stealth-message"
    >
      {{ message }}
    </p>

    <button
      class="st-toggle"
      :class="{ on: enabled, busy }"
      :disabled="busy"
      role="switch"
      :aria-checked="enabled"
      data-testid="stealth-toggle"
      @click="onToggle"
    >
      <span class="st-knob" />
    </button>
  </div>
</template>

<style scoped>
.stealth-block {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 16px;
  background: var(--panel);
  border: 1px solid var(--border-subtle);
  border-radius: var(--radius);
  box-shadow: var(--shadow-sm);
  overflow: hidden;
  transition: border-color 0.25s ease, box-shadow 0.25s ease, background-color 0.2s ease;
}
.stealth-block.active {
  border-color: color-mix(in srgb, var(--accent, #34d399) 55%, transparent);
  box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent, #34d399) 25%, transparent),
    0 8px 30px -12px color-mix(in srgb, var(--accent, #34d399) 45%, transparent);
}
.st-head {
  display: flex;
  align-items: center;
  gap: 10px;
}
.st-glyph {
  display: grid;
  place-items: center;
  width: 34px;
  height: 34px;
  border-radius: 9px;
  color: var(--accent, #34d399);
  background: color-mix(in srgb, var(--accent, #34d399) 12%, transparent);
  border: 1px solid color-mix(in srgb, var(--accent, #34d399) 30%, transparent);
  flex-shrink: 0;
}
.st-titlewrap {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.st-title {
  font-size: 15px;
  font-weight: var(--fw-700);
  color: var(--text);
  line-height: 1.1;
}
.st-badge {
  font-family: var(--font-mono, monospace);
  font-size: 10px;
  letter-spacing: 0.02em;
  color: var(--accent, #34d399);
  opacity: 0.85;
}
.st-status {
  display: flex;
  align-items: center;
  gap: 7px;
  margin-top: 2px;
}
.st-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--text-dim, #6b7280);
  transition: background-color 0.25s ease, box-shadow 0.25s ease;
}
.st-dot.on {
  background: var(--accent, #34d399);
  box-shadow: 0 0 8px color-mix(in srgb, var(--accent, #34d399) 80%, transparent);
}
.st-state {
  font-size: 12px;
  font-weight: var(--fw-600);
  color: var(--text-muted, #9ca3af);
}
.st-hint {
  font-size: 11.5px;
  line-height: 1.4;
  color: var(--text-dim, #8b93a1);
}
.st-warn {
  font-size: 11px;
  color: #f59e0b;
  cursor: pointer;
  text-decoration: underline dotted;
}
.st-msg {
  font-size: 11px;
  line-height: 1.4;
  color: var(--accent, #34d399);
  border-left: 2px solid color-mix(in srgb, var(--accent, #34d399) 60%, transparent);
  padding-left: 8px;
}
.st-msg.err {
  color: #f87171;
  border-left-color: #f87171;
}
.st-toggle {
  position: absolute;
  right: 16px;
  bottom: 16px;
  width: 46px;
  height: 26px;
  border-radius: 999px;
  border: 1px solid var(--border-subtle);
  background: color-mix(in srgb, var(--text-dim, #6b7280) 22%, transparent);
  cursor: pointer;
  padding: 0;
  transition: background-color 0.25s ease, border-color 0.25s ease;
}
.st-toggle.on {
  background: var(--accent, #34d399);
  border-color: var(--accent, #34d399);
}
.st-toggle.busy {
  opacity: 0.6;
  cursor: wait;
}
.st-knob {
  position: absolute;
  top: 50%;
  left: 3px;
  transform: translateY(-50%);
  width: 20px;
  height: 20px;
  border-radius: 50%;
  background: #fff;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.4);
  transition: left 0.22s cubic-bezier(0.4, 0, 0.2, 1);
}
.st-toggle.on .st-knob {
  left: 23px;
}
</style>
