<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { webproxyExitCountries, webproxySetExitDefault, type ExitCountries } from '@/lib/bridge'

// Настройки → Сеть → Mycelium: страна выхода pq-web по умолчанию для всех сайтов (пин сайта важнее).
const { t, locale } = useI18n()
const info = ref<ExitCountries | null>(null)
const busy = ref(false)
const names = (() => { try { return new Intl.DisplayNames([locale.value], { type: 'region' }) } catch { return null } })()
const label = (iso: string) => `${iso} · ${names?.of(iso) ?? iso}`

async function load() { info.value = await webproxyExitCountries('') }
async function pick(iso: string) {
  busy.value = true
  try { await webproxySetExitDefault(iso); await load() } finally { busy.value = false }
}
onMounted(load)
</script>

<template>
  <div class="xd" data-testid="mycelium-exit-default-row">
    <div class="xd-txt">
      <span class="xd-t">{{ t('site.exitDefault') }}</span>
      <span class="xd-h" :class="{ warn: info?.fallback }">{{ info?.fallback ? t('site.exitFallback') : t('site.exitDefaultHint') }}</span>
    </div>
    <select class="xd-sel" :value="info?.default ?? ''" :disabled="busy" @change="pick(($event.target as HTMLSelectElement).value)" data-testid="mycelium-exit-default">
      <option value="">{{ t('site.exitAuto') }}</option>
      <option v-for="c in info?.countries ?? []" :key="c.iso" :value="c.iso">{{ label(c.iso) }} ({{ c.nodes }})</option>
      <option v-if="info?.default && !info.countries.some((c) => c.iso === info?.default)" :value="info.default">{{ label(info.default) }} — 0</option>
    </select>
  </div>
</template>

<style scoped>
.xd { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 12px; border: 1px solid var(--border); margin-bottom: 10px; transition: border-color 0.16s ease; }
.xd:hover { border-color: var(--text-dim); }
.xd-txt { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.xd-t { font-size: 12px; font-weight: var(--fw-700, 700); }
.xd-h { font-size: 11px; color: var(--text-dim); line-height: 1.45; }
.xd-h.warn { color: #f5c451; }
.xd-sel { flex-shrink: 0; background: #14171b; border: 1px solid #2c3138; color: #c9d1d9; font: inherit; font-size: 11px; padding: 5px 6px; max-width: 170px; cursor: pointer; transition: border-color 0.15s; }
.xd-sel:hover { border-color: #4a525c; }
</style>
