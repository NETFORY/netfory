<script setup lang="ts">
import { defineEmits, defineProps, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { webproxyCheckIp, webproxyCheckIpCancel, type ExitIpResult } from '@/lib/bridge'

// «Проверить мой IP»: HTTPS к сервису IP через ту же цепочку и тот же узел pq-web, что у сайта.
const props = defineProps<{ host: string; node: string; cname: (iso: string | null | undefined) => string }>()
const emit = defineEmits<{ (e: 'checked', r: ExitIpResult): void }>()
const { t } = useI18n()
const res = ref<ExitIpResult | null>(null)
const err = ref('')
const busy = ref(false)

async function run() {
  if (busy.value) return
  busy.value = true
  err.value = ''
  try {
    res.value = await webproxyCheckIp(props.host)
    emit('checked', res.value)
  } catch (e) {
    res.value = null
    err.value = String(e)
  } finally {
    busy.value = false
  }
}
// сменился сайт или узел выхода (пин, «по умолчанию», новая личность) — старый результат неактуален
watch(() => [props.host, props.node], ([h, n], [ph]) => { if (h !== ph || (res.value && n && n !== res.value.node)) { res.value = null; err.value = '' } })
</script>

<template>
  <div class="xc" data-testid="site-panel-ip-check">
    <div class="xc-row">
      <button :disabled="busy" @click="run" data-testid="site-panel-check-ip">{{ busy ? t('site.checkingIp') : t('site.checkIp') }}</button>
      <button v-if="busy" class="xc-cancel" @click="webproxyCheckIpCancel(host)" data-testid="site-panel-check-ip-cancel">{{ t('site.cancel') }}</button>
    </div>
    <div v-if="res" class="xc-res" data-testid="site-panel-ip-result">
      <b data-testid="site-panel-ip-value">{{ res.ip || '—' }}</b>
      <span data-testid="site-panel-ip-country">{{ cname(res.iso) }}<template v-if="res.city"> · {{ res.city }}</template></span>
      <small v-if="res.org">{{ res.org }} · {{ res.service }}</small>
      <small v-if="res.countryMatch && res.ipMatch" class="ok" data-testid="site-panel-ip-ok">✓ {{ t('site.checkOk') }}</small>
      <small v-if="!res.countryMatch" class="warn" data-testid="site-panel-ip-country-mismatch">⚠ {{ t('site.checkCountryMismatch', { e: res.expected || '—', r: res.iso || '—' }) }}</small>
      <small v-if="!res.ipMatch" class="warn" data-testid="site-panel-ip-mismatch">⚠ {{ t('site.checkIpMismatch', { a: res.announced.join(', ') }) }}</small>
    </div>
    <small v-if="err" class="err" data-testid="site-panel-ip-error">{{ err }}</small>
  </div>
</template>

<style scoped>
.xc { display: flex; flex-direction: column; gap: 6px; }
.xc button { padding: 8px 12px; font: inherit; font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase; cursor: pointer; background: transparent; border: 1px solid #38bdf8; color: #38bdf8; transition: background-color 0.15s; }
.xc button:hover:not(:disabled) { background: rgba(56, 189, 248, 0.1); }
.xc button:disabled { opacity: 0.6; cursor: progress; }
.xc-row { display: flex; gap: 6px; }
.xc-row > button:first-child { flex: 1; }
.xc .xc-cancel { border-color: #f87171; color: #f87171; }
.xc .xc-cancel:hover { background: rgba(248, 113, 113, 0.1); }
.xc-res { display: flex; flex-direction: column; gap: 3px; padding: 8px 10px; border: 1px solid #232830; background: #11151a; animation: xc-in 0.3s ease-out; }
.xc-res b { font-size: 15px; color: #fff; font-variant-numeric: tabular-nums; }
.xc-res span { font-size: 12px; color: #c9d1d9; }
.xc-res small { font-size: 10px; color: #6b7480; }
.xc .ok { color: #2ee6a6; }
.xc .warn { color: #f5c451; }
.xc .err { color: #f87171; font-size: 11px; }
@keyframes xc-in { from { opacity: 0; transform: translateY(-4px); } to { opacity: 1; transform: none; } }
</style>
