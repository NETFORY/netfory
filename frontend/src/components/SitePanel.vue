<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import NodePlanet from '@/components/NodePlanet.vue'
import CircuitMap from '@/components/CircuitMap.vue'
import Sparkline from '@/components/Sparkline.vue'
import ExitIpCheck from '@/components/ExitIpCheck.vue'
import { fmtBytes } from '@/lib/format'
import {
  myceliumStatus,
  myceliumSet,
  myceliumVpnSet,
  myceliumRebuild,
  myceliumHopAddrs,
  webproxySiteBytes,
  webproxyExitCountries,
  webproxySetExitCountry,
  webproxyNewIdentity,
  webproxySetExitDefault,
  type ExitCountries,
  type MyceliumInfo,
  type MyceliumSettings,
  type SiteBytes,
} from '@/lib/bridge'

// Выезжающая панель https-вкладки: маршрут Mycelium (глобус + карта цепочки + трафик) и мини-настройки.
const props = defineProps<{ open: boolean; host: string; node: string }>()
const emit = defineEmits<{ (e: 'close'): void; (e: 'reload'): void }>()
const { t, locale } = useI18n()

const tab = ref<'route' | 'settings'>('route')
const info = ref<MyceliumInfo | null>(null)
const bytes = ref<SiteBytes | null>(null)
const hopIps = ref<Record<string, string>>({})
const busy = ref(false)
const exit = ref<ExitCountries | null>(null)
const identPrev = ref('')
const identSingle = ref(false)
// EndpointId узла → реальный IP выхода из «Проверить мой IP»
const realIp = ref<Record<string, string>>({})
const err = ref('')
let timer: ReturnType<typeof setInterval> | undefined
let loading = false
let errAt = 0

const on = computed(() => !!info.value?.settings?.enabled)
const egressId = computed(() => exit.value?.current?.id || props.node)
const histRx = computed(() => (bytes.value?.hist ?? []).map((b) => b[0]))
const histTx = computed(() => (bytes.value?.hist ?? []).map((b) => b[1]))
const regionNames = computed(() => { try { return new Intl.DisplayNames([locale.value], { type: 'region' }) } catch { return null } })
const cName = (iso: string | null | undefined) => (iso ? `${iso} · ${regionNames.value?.of(iso) ?? iso}` : '—')
const cur = computed(() => exit.value?.current ?? null)
// узлов в пуле сайта: закреплённая страна (если не откат) или все
const poolSize = computed(() => { const e = exit.value; if (!e) return 0; const p = !e.fallback && e.effective ? e.countries.find((c) => c.iso === e.effective) : null; return p ? p.nodes : e.countries.reduce((a, c) => a + c.nodes, 0) })
const autoOn = computed(() => { const e = exit.value; return !e || !!e.fallback || (e.default ? e.pinned === '*' : !e.pinned) })
const defOn = computed(() => !!exit.value?.default && !exit.value.pinned && !exit.value.fallback)
// пин сайта, который можно сделать страной по умолчанию ('' = Авто)
const defTarget = computed(() => { const e = exit.value; if (!e?.pinned) return null; if (e.pinned !== '*' && (e.fallback || !e.countries.some((c) => c.iso === e.pinned))) return null; const v = e.pinned === '*' ? '' : e.pinned; return v === e.default ? null : v })
async function makeDefault() {
  if (defTarget.value === null || busy.value) return
  busy.value = true
  try {
    await webproxySetExitDefault(defTarget.value)
    await webproxySetExitCountry(props.host, '')
    await load()
  } catch (e) {
    err.value = String(e)
  } finally {
    busy.value = false
  }
}
const identNote = computed(() => {
  if (identSingle.value && poolSize.value <= 1) return t('site.identitySingle')
  return identPrev.value && cur.value && cur.value.id !== identPrev.value ? t('site.newIdentityDone', { n: `${cName(cur.value.iso)} · ${cur.value.addr.replace(/:\d+$/, '')}` }) : ''
})
const circuit = computed(() => {
  const exits = (info.value?.circuits ?? []).filter((c) => (c.kind ?? 'exit') === 'exit')
  // цепочка к выходу pq-web этого сайта → любая к выходу pq-web → любая живая exit-цепочка пула
  return exits.find((c) => egressId.value && c.target === egressId.value) || exits.find((c) => c.target && !c.degraded) || exits.find((c) => !c.degraded) || exits[0] || null
})
// точки маршрута: Вы → Guard → Middle → Exit → выход pq-web
const route = computed(() => {
  const c = circuit.value
  if (!c) return [] as { id: string; role: string }[]
  const roles = ['Guard', 'Middle', 'Exit']
  const pts = [{ id: 'me', role: t('site.you') }, ...c.path.map((id, i) => ({ id, role: c.path.length === 1 ? 'Guard/Exit' : roles[i === c.path.length - 1 ? 2 : i] }))]
  if (c.target) pts.push({ id: c.target, role: t('site.egress') })
  return pts
})
const routeIds = computed(() => route.value.map((p) => p.id))
const routeAddrs = computed(() => route.value.map((p) => hopIps.value[p.id] || ''))
const colorMap = computed(() => Object.fromEntries(route.value.map((p, i) => [p.id, i === 0 ? '#38bdf8' : i === route.value.length - 1 && circuit.value?.target ? '#f59e0b' : '#2ee6a6'])))
const tooltips = computed(() => Object.fromEntries(route.value.map((p) => [p.id, `${p.role}${p.id === 'me' ? ' (≈)' : ` · ${p.id.slice(0, 8)}…`}${hopIps.value[p.id] ? ` · ${hopIps.value[p.id]}` : ' (≈)'}`])))

async function load() {
  // опрос раз в 3 с не должен копиться, если бэкенд отвечает медленно
  if (loading) return
  loading = true
  try {
    info.value = await myceliumStatus()
    if (props.host) [bytes.value, exit.value] = await Promise.all([webproxySiteBytes(props.host), webproxyExitCountries(props.host)])
    // IP узла pq-web для глобуса: проверенный → из анонса (иначе точка выхода ставилась приблизительно)
    const c = exit.value?.current
    if (c) {
      const ip = realIp.value[c.id] || c.addr.replace(/:\d+$/, '').replace(/^\[|\]$/g, '')
      if (ip && hopIps.value[c.id] !== ip) hopIps.value = { ...hopIps.value, [c.id]: ip }
    }
    const need = routeIds.value.filter((id) => id !== 'me' && !(id in hopIps.value))
    if (need.length) hopIps.value = { ...hopIps.value, ...(await myceliumHopAddrs(need)) }
    // ошибка пересборки гаснет, когда цепочка снова жива
    if (err.value && Date.now() - errAt > 10000 && circuit.value && !circuit.value.degraded) err.value = ''
  } finally {
    loading = false
  }
}
watch(
  () => props.open,
  (o) => {
    clearInterval(timer)
    if (!o) return
    void load()
    timer = setInterval(() => void load(), 3000)
  },
  { immediate: true },
)
watch(() => props.host, () => { bytes.value = null; exit.value = null; identPrev.value = ''; if (props.open) void load() })
onUnmounted(() => clearInterval(timer))

async function rebuild(reload: boolean) {
  if (!circuit.value || busy.value) return
  busy.value = true
  err.value = ''
  try {
    // «Новая личность» = новая цепочка + другой узел pq-web (новый IP выхода)
    identPrev.value = ''
    identSingle.value = false
    let needRebuild = true
    if (reload) {
      identPrev.value = (await webproxyNewIdentity(props.host)) || exit.value?.current?.id || ''
      identSingle.value = poolSize.value <= 1
      // есть другой узел — у него будет своя свежая цепочка; старую не пересобираем (лишняя нагрузка на релеи)
      needRebuild = identSingle.value
    }
    if (needRebuild) await myceliumRebuild(circuit.value.id)
    if (reload) emit('reload')
    await load()
  } catch (e) {
    err.value = String(e)
    errAt = Date.now()
  } finally {
    busy.value = false
  }
}
async function pickCountry(iso: string) {
  if (busy.value || (exit.value?.pinned ?? '') === iso) return
  busy.value = true
  err.value = ''
  identPrev.value = ''
  identSingle.value = false
  try {
    await webproxySetExitCountry(props.host, iso)
    await load()
    emit('reload')
  } catch (e) {
    err.value = String(e)
  } finally {
    busy.value = false
  }
}
async function setOpt(patch: Partial<MyceliumSettings>) {
  if (!info.value?.settings || busy.value) return
  busy.value = true
  try {
    const r = await myceliumSet({ ...info.value.settings, ...patch })
    if (r) info.value = r
  } finally {
    busy.value = false
  }
}
async function setVpn(v: boolean) {
  busy.value = true
  try {
    const r = await myceliumVpnSet(v)
    if (r) info.value = r
  } finally {
    busy.value = false
  }
}
const opts = computed(() => {
  const s = info.value?.settings
  return [
    { id: 'enabled', label: t('site.optProtocol'), on: !!s?.enabled, set: (v: boolean) => setOpt({ enabled: v }) },
    { id: 'direct', label: t('site.optDirect'), on: !!s?.allowDirect, set: (v: boolean) => setOpt({ allowDirect: v }) },
    { id: 'cover', label: t('site.optCover'), on: !!s?.coverTraffic, set: (v: boolean) => setOpt({ coverTraffic: v }) },
    { id: 'relay', label: t('site.optRelay'), on: !!s && !s.clientOnly, set: (v: boolean) => setOpt({ clientOnly: !v }) },
    { id: 'vpn', label: t('site.optVpn'), on: !!info.value?.vpn?.enabled, set: (v: boolean) => setVpn(v), disabled: !on.value || info.value?.vpn?.supported === false },
  ]
})
</script>

<template>
  <transition name="sp-slide">
    <aside v-if="open" class="sp" data-testid="site-panel">
      <div class="sp-head">
        <div>
          <h2 class="sp-name" data-testid="site-panel-host">{{ host }}</h2>
          <code class="sp-sub">{{ on ? t('site.viaMycelium') : t('site.direct') }}</code>
        </div>
        <button class="sp-close" @click="emit('close')" data-testid="site-panel-close">×</button>
      </div>
      <div class="sp-tabs">
        <button :class="{ on: tab === 'route' }" @click="tab = 'route'" data-testid="site-panel-tab-route">{{ t('site.tabRoute') }}</button>
        <button :class="{ on: tab === 'settings' }" @click="tab = 'settings'" data-testid="site-panel-tab-settings">Mycelium Settings</button>
      </div>

      <template v-if="tab === 'route'">
        <template v-if="on">
          <div class="sp-planet">
            <NodePlanet :peer-ids="routeIds" :peer-addrs="routeAddrs" :seed="host" :transfer-active="true" :active="open" :route="true" :color-map="colorMap" :tooltips="tooltips" :max-points="8" />
          </div>
          <div class="sp-legend"><span><i style="background:#38bdf8" />{{ t('site.you') }} ≈</span><span><i style="background:#2ee6a6" />Mushroom</span><span><i style="background:#f59e0b" />{{ t('site.egress') }}</span></div>
          <CircuitMap v-if="circuit" :circuit="circuit" :my-id="info?.nodeId" :peer-label="host" testid="site-panel-map" @rebuilt="load" />
          <p v-else class="sp-wait" data-testid="site-panel-wait">{{ t('site.building') }}</p>
          <div v-if="exit?.direct" class="sp-direct" data-testid="site-panel-direct">{{ t('site.directLeak') }}</div>
          <div class="sp-stats">
            <div><span class="k">{{ t('site.rx') }} <em>{{ t('site.hist30') }}</em></span><b data-testid="site-panel-rx">{{ fmtBytes(bytes?.rx ?? 0) }}</b><Sparkline :data="histRx" testid="site-panel-rx-spark" /></div>
            <div><span class="k">{{ t('site.tx') }} <em>{{ t('site.hist30') }}</em></span><b data-testid="site-panel-tx">{{ fmtBytes(bytes?.tx ?? 0) }}</b><Sparkline :data="histTx" color="#38bdf8" testid="site-panel-tx-spark" /></div>
            <div class="wide" v-if="!exit"><span class="k">{{ t('site.egress') }}</span><code data-testid="site-panel-egress">{{ circuit?.target ? `${circuit.target.slice(0, 10)}…${circuit.target.slice(-4)}${hopIps[circuit.target] ? ' · ' + hopIps[circuit.target] : ''}` : '—' }}</code></div>
            <div class="wide" v-else-if="!cur"><span class="k">{{ t('site.exitIp') }}</span><code data-testid="site-panel-exit-connecting">{{ t('site.exitConnecting') }}</code></div>
            <div class="wide" v-else><span class="k">{{ t('site.egress') }} · {{ t('site.exitIp') }}</span><code data-testid="site-panel-exit-ip">{{ cName(cur.iso) }} · {{ realIp[cur.id] || cur.addr.replace(/:\d+$/, '') || '—' }}</code><small v-if="cur.verified" class="sp-okc" data-testid="site-panel-exit-verified">✓ {{ t('site.verified') }}</small><code class="sp-nid" data-testid="site-panel-egress">{{ cur.id.slice(0, 10) }}…{{ cur.id.slice(-4) }}</code><small v-if="cur.mismatch" class="sp-warn" data-testid="site-panel-exit-mismatch">⚠ {{ t('site.exitMismatch', { r: cur.region }) }}</small></div>
          </div>
          <div class="sp-cc" data-testid="site-panel-exit-country">
            <span class="k">{{ t('site.exitCountry') }}</span>
            <div class="sp-chips">
              <button v-if="exit?.default" :class="{ on: defOn }" :disabled="busy" :title="cName(exit.default)" @click="pickCountry('')" data-testid="site-exit-default">{{ t('site.exitFollowDefault') }} · {{ exit.default }}</button>
              <button :class="{ on: autoOn }" :disabled="busy" @click="pickCountry(exit?.default ? '*' : '')" data-testid="site-exit-auto">{{ t('site.exitAuto') }}</button>
              <button v-for="c in exit?.countries ?? []" :key="c.iso" :class="{ on: exit?.pinned === c.iso }" :disabled="busy" :title="cName(c.iso)" @click="pickCountry(c.iso)" :data-testid="`site-exit-${c.iso}`">{{ c.iso }}<sup>{{ c.nodes }}</sup></button>
            </div>
            <button v-if="defTarget !== null" class="sp-mkdef" :disabled="busy" @click="makeDefault" data-testid="site-panel-make-default">★ {{ t('site.makeDefault', { c: defTarget || t('site.exitAuto') }) }}</button>
            <small v-if="exit?.fallback" class="sp-warn" data-testid="site-panel-exit-fallback">⚠ {{ t('site.exitFallback') }}</small>
            <small v-else-if="exit && !exit.countries.length" class="sp-hint" data-testid="site-panel-exit-none">{{ t('site.exitNone') }}</small>
          </div>
          <div class="sp-acts">
            <button :disabled="!circuit || busy" @click="rebuild(false)" data-testid="site-panel-rebuild">⟳ {{ t('site.rebuild') }}</button>
            <button class="alt" :disabled="!circuit || busy" @click="rebuild(true)" data-testid="site-panel-new-identity">✦ {{ t('site.newIdentity') }}</button>
          </div>
          <ExitIpCheck :host="host" :node="cur?.id ?? ''" :cname="cName" @checked="(r) => { if (r.node && r.ip) realIp = { ...realIp, [r.node]: r.ip }; void load() }" />
          <p v-if="identNote" class="sp-ok" data-testid="site-panel-identity-note">{{ identNote }}</p>
          <p v-if="err" class="sp-err">{{ err }}</p>
        </template>
        <div v-else class="sp-off" data-testid="site-panel-off">
          <p>{{ t('site.offText') }}</p>
          <button :disabled="busy" @click="setOpt({ enabled: true })" data-testid="site-panel-enable">{{ t('site.enable') }}</button>
        </div>
      </template>

      <div v-else class="sp-opts" data-testid="site-panel-settings">
        <label v-for="o in opts" :key="o.id" class="sp-opt" :class="{ dis: o.disabled }">
          <span>{{ o.label }}</span>
          <button class="toggle" :class="{ on: o.on }" role="switch" :aria-checked="o.on" :disabled="busy || o.disabled" @click="o.set(!o.on)" :data-testid="`site-opt-${o.id}`"><span class="toggle-knob" /></button>
        </label>
        <p class="sp-hint">{{ t('site.optHint') }}</p>
      </div>
    </aside>
  </transition>
</template>

<style scoped>
.sp { position: absolute; top: 0; right: 0; bottom: 0; width: 500px; max-width: 92vw; z-index: 30; overflow-y: auto; background: #0d1014; border-left: 1px solid #2c3138; box-shadow: -8px 0 22px -18px rgba(0, 0, 0, 0.85); padding: 16px 16px 28px; display: flex; flex-direction: column; gap: 14px; color: #c9d1d9; }
.sp-head { display: flex; justify-content: space-between; gap: 10px; }
.sp-name { font-size: 16px; font-weight: var(--fw-700); color: #fff; word-break: break-all; }
.sp-sub { display: block; margin-top: 4px; font-size: 11px; color: #2ee6a6; }
.sp-close { background: linear-gradient(180deg, #1c2026, #14171b); border: 1px solid #2c3138; color: #8a93a0; width: 28px; height: 28px; cursor: pointer; flex-shrink: 0; transition: color 0.15s, border-color 0.15s; }
.sp-close:hover { color: #fff; border-color: #4a525c; }
.sp-tabs { display: flex; gap: 6px; }
.sp-tabs button { flex: 1; padding: 7px 10px; font: inherit; font-size: 10px; letter-spacing: 0.14em; text-transform: uppercase; background: #14171b; border: 1px solid #2c3138; color: #8a93a0; cursor: pointer; transition: color 0.15s, border-color 0.15s, background-color 0.15s; }
.sp-tabs button.on { color: #2ee6a6; border-color: #2ee6a6; background: rgba(46, 230, 166, 0.07); }
.sp-planet { height: 320px; border: 1px solid #232830; background: radial-gradient(circle at 50% 45%, #141a22 0%, #0a0c0f 70%); }
.sp-legend { display: flex; justify-content: center; gap: 16px; font-size: 10px; color: var(--text-dim); }
.sp-legend span { display: inline-flex; align-items: center; gap: 6px; }
.sp-legend i { width: 9px; height: 9px; border-radius: 50%; display: inline-block; }
.sp-wait { color: #f5c451; font-size: 12px; }
.sp-stats { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; }
.sp-stats > div { padding: 8px 10px; border: 1px solid #232830; background: #11151a; display: flex; flex-direction: column; gap: 3px; }
.sp-stats .wide { grid-column: 1 / -1; }
.sp-stats .k { font-size: 10px; letter-spacing: 0.12em; text-transform: uppercase; color: #6b7480; }
.sp-stats b { font-size: 16px; color: #2ee6a6; font-variant-numeric: tabular-nums; }
.sp-stats code { font-size: 11px; color: #f59e0b; word-break: break-all; }
.sp-acts { display: flex; gap: 8px; }
.sp-acts button, .sp-off button { flex: 1; padding: 9px 12px; font: inherit; font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase; cursor: pointer; background: transparent; border: 1px solid #2ee6a6; color: #2ee6a6; transition: background-color 0.15s; }
.sp-acts button.alt { border-color: #f59e0b; color: #f59e0b; }
.sp-acts button:hover:not(:disabled), .sp-off button:hover:not(:disabled) { background: rgba(46, 230, 166, 0.1); }
.sp-acts button:disabled { opacity: 0.4; cursor: default; }
.sp-err { color: #f87171; font-size: 11px; }
.sp-ok { color: #2ee6a6; font-size: 11px; }
.sp-direct { padding: 8px 10px; background: rgba(248, 113, 113, 0.12); border: 1px solid #f87171; color: #fca5a5; font-size: 11px; font-weight: 700; letter-spacing: 0.04em; animation: sp-pulse 1.6s ease-in-out infinite; }
@keyframes sp-pulse { 50% { border-color: rgba(248, 113, 113, 0.4); } }
.sp-okc { color: #2ee6a6; font-size: 10px; }
.sp-mkdef { align-self: flex-start; padding: 4px 10px; font: inherit; font-size: 10px; letter-spacing: 0.08em; background: transparent; border: 1px dashed #f59e0b; color: #f59e0b; cursor: pointer; transition: background-color 0.15s; }
.sp-mkdef:hover:not(:disabled) { background: rgba(245, 158, 11, 0.08); }
.sp-stats code.sp-nid { color: #6b7480; font-size: 10px; }
.sp-stats .k em { font-style: normal; color: #4a525c; margin-left: 4px; letter-spacing: 0.06em; }
.sp-warn { color: #f5c451; font-size: 10px; }
.sp-cc { display: flex; flex-direction: column; gap: 6px; padding: 8px 10px; border: 1px solid #232830; background: #11151a; }
.sp-cc .k { font-size: 10px; letter-spacing: 0.12em; text-transform: uppercase; color: #6b7480; }
.sp-chips { display: flex; flex-wrap: wrap; gap: 6px; }
.sp-chips button { padding: 5px 10px; font: inherit; font-size: 11px; letter-spacing: 0.08em; background: #14171b; border: 1px solid #2c3138; color: #8a93a0; cursor: pointer; transition: color 0.15s, border-color 0.15s, background-color 0.15s; }
.sp-chips button:hover:not(:disabled) { color: #fff; border-color: #4a525c; }
.sp-chips button.on { color: #f59e0b; border-color: #f59e0b; background: rgba(245, 158, 11, 0.08); }
.sp-chips button:disabled { cursor: default; }
.sp-chips sup { margin-left: 3px; font-size: 8px; color: #6b7480; }
.sp-off { display: flex; flex-direction: column; gap: 12px; font-size: 12px; color: var(--text-dim); }
.sp-opts { display: flex; flex-direction: column; gap: 4px; }
.sp-opt { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 2px; border-bottom: 1px solid #1d2228; font-size: 12px; }
.sp-opt.dis { opacity: 0.5; }
.sp-hint { margin-top: 8px; font-size: 11px; color: #6b7480; }
.toggle { position: relative; width: 38px; height: 20px; border-radius: 999px; border: 1px solid #2c3138; background: #14171b; cursor: pointer; flex-shrink: 0; transition: background-color 0.18s, border-color 0.18s; }
.toggle.on { background: rgba(46, 230, 166, 0.25); border-color: #2ee6a6; }
.toggle-knob { position: absolute; top: 2px; left: 2px; width: 14px; height: 14px; border-radius: 50%; background: #6b7480; transition: transform 0.18s ease, background-color 0.18s; }
.toggle.on .toggle-knob { transform: translateX(18px); background: #2ee6a6; }
.toggle:disabled { cursor: default; }
.sp-slide-enter-active, .sp-slide-leave-active { transition: transform 0.4s cubic-bezier(0.22, 1, 0.36, 1), opacity 0.3s ease-out; }
.sp-slide-enter-from, .sp-slide-leave-to { transform: translateX(100%); opacity: 0; }
</style>
