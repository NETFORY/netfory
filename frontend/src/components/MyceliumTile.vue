<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { myceliumSet, myceliumStatus, type MyceliumInfo } from '@/lib/bridge'

// Плитка «Mycelium Protocol» на дашборде (замена «Stealth Mode»): тумблер + статус цепочек.
const router = useRouter()
const { t } = useI18n()
const info = ref<MyceliumInfo | null>(null)
const busy = ref(false)
const err = ref('')
let timer: ReturnType<typeof setInterval> | null = null

const enabled = () => !!info.value?.settings?.enabled
const ready = () => info.value?.status?.circuitsReady ?? 0
const relays = () => info.value?.status?.relaysKnown ?? 0

async function load() {
  info.value = await myceliumStatus()
}
async function onToggle() {
  if (!info.value?.settings) return
  busy.value = true
  err.value = ''
  try {
    const r = await myceliumSet({ ...info.value.settings, enabled: !info.value.settings.enabled })
    if (r) info.value = r
  } catch (e) {
    err.value = String(e)
  } finally {
    busy.value = false
  }
}
onMounted(() => {
  void load()
  timer = setInterval(() => void load(), 5000)
})
onUnmounted(() => timer && clearInterval(timer))
</script>

<template>
  <div class="myc-tile rise" :class="{ active: enabled() }" :style="{ animationDelay: '320ms' }" data-testid="dashboard-mycelium">
    <div class="mt-head">
      <span class="mt-glyph" aria-hidden="true">
        <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
          <circle cx="4" cy="12" r="2" /><circle cx="12" cy="6" r="2" /><circle cx="12" cy="18" r="2" /><circle cx="20" cy="12" r="2" />
          <path d="M6 11l4-4M6 13l4 4M14 7l4 4M14 17l4-4" />
        </svg>
      </span>
      <div class="mt-titlewrap">
        <h2 class="mt-title">{{ t('mycelium.title') }}</h2>
        <span class="mt-badge" data-testid="mycelium-tile-badge">mushroom · 3 hops · {{ t('mycelium.experimental') }}</span>
      </div>
    </div>
    <div class="mt-status">
      <span class="mt-dot" :class="{ on: enabled() && ready() > 0 }" />
      <span class="mt-state" data-testid="mycelium-tile-state">
        <template v-if="!enabled()">{{ t('mycelium.stOff') }}</template>
        <template v-else-if="ready() > 0">{{ t('mycelium.stActive', { c: ready(), r: relays() }) }}</template>
        <template v-else>{{ t('mycelium.stBuilding') }}</template>
      </span>
    </div>
    <p class="mt-hint">— {{ t('mycelium.tileHint') }}</p>
    <p v-if="info?.vpn?.enabled" class="mt-vpn" :class="{ on: info.vpn.running }" data-testid="mycelium-tile-vpn">
      <span class="mt-dot" :class="{ on: info.vpn.running }" />
      {{ t('mycelium.vpnTile') }}: {{ info.vpn.running ? t('mycelium.vpnOn') : t('mycelium.vpnStarting') }}
    </p>
    <p v-if="info && !info.started" class="mt-warn" data-testid="mycelium-tile-unavailable">⚠ {{ t('mycelium.unavailable') }}</p>
    <p v-if="err" class="mt-msg err" data-testid="mycelium-tile-error">{{ err }}</p>
    <p class="mt-warn" data-testid="mycelium-tile-settings" @click="router.push({ name: 'settings', query: { tab: 'network' } })">{{ t('mycelium.tileSettings') }} →</p>
    <button class="mt-toggle" :class="{ on: enabled(), busy }" :disabled="busy || (info !== null && !info.started)" role="switch" :aria-checked="enabled()" data-testid="mycelium-tile-toggle" @click="onToggle">
      <span class="mt-knob" />
    </button>
  </div>
</template>

<style scoped>
.myc-tile {
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
.myc-tile.active {
  border-color: color-mix(in srgb, var(--accent, #34d399) 55%, transparent);
  box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent, #34d399) 25%, transparent),
    0 8px 30px -12px color-mix(in srgb, var(--accent, #34d399) 45%, transparent);
}
.mt-head {
  display: flex;
  align-items: center;
  gap: 10px;
}
.mt-glyph {
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
.mt-titlewrap {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.mt-title {
  font-size: 15px;
  font-weight: var(--fw-700);
  color: var(--text);
  line-height: 1.1;
}
.mt-badge {
  font-family: var(--font-mono, monospace);
  font-size: 10px;
  letter-spacing: 0.02em;
  color: var(--accent, #34d399);
  opacity: 0.85;
}
.mt-status {
  display: flex;
  align-items: center;
  gap: 7px;
  margin-top: 2px;
}
.mt-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--text-dim, #6b7280);
  transition: background-color 0.25s ease, box-shadow 0.25s ease;
}
.mt-dot.on {
  background: var(--accent, #34d399);
  box-shadow: 0 0 8px color-mix(in srgb, var(--accent, #34d399) 80%, transparent);
}
.mt-state {
  font-size: 12px;
  font-weight: var(--fw-600);
  color: var(--text-muted, #9ca3af);
}
.mt-hint {
  font-size: 11.5px;
  line-height: 1.4;
  color: var(--text-dim, #8b93a1);
}
.mt-vpn {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-size: 11px;
  color: #f59e0b;
}
.mt-vpn.on { color: var(--accent, #34d399); }
.mt-warn {
  font-size: 11px;
  color: #f59e0b;
  cursor: pointer;
  text-decoration: underline dotted;
}
.mt-msg {
  font-size: 11px;
  line-height: 1.4;
  color: var(--accent, #34d399);
  border-left: 2px solid color-mix(in srgb, var(--accent, #34d399) 60%, transparent);
  padding-left: 8px;
}
.mt-msg.err {
  color: #f87171;
  border-left-color: #f87171;
}
.mt-toggle {
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
.mt-toggle.on {
  background: var(--accent, #34d399);
  border-color: var(--accent, #34d399);
}
.mt-toggle.busy {
  opacity: 0.6;
  cursor: wait;
}
.mt-knob {
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
.mt-toggle.on .mt-knob {
  left: 23px;
}
.myc-tile .mt-warn, .myc-tile .mt-msg, .myc-tile .mt-hint { max-width: calc(100% - 64px); }
</style>
