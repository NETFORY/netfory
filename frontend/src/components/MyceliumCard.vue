<script setup lang="ts">
import { computed, defineEmits, defineProps, onMounted, onUnmounted, ref, watch, withDefaults } from 'vue'
import { useI18n } from 'vue-i18n'
import CircuitMap from '@/components/CircuitMap.vue'
import ExitDefaultRow from '@/components/ExitDefaultRow.vue'
import { DEFAULT_MYCELIUM_SETTINGS, myceliumSet, myceliumStatus, myceliumVpnSet, onMyceliumVpnError, type MyceliumCircuit, type MyceliumInfo, type MyceliumSettings } from '@/lib/bridge'

// Mycelium (NIP-1): тумблер, статус цепочек, под-тумблеры, режим релея, системный VPN (Фаза 3), тех. детали.
const props = withDefaults(defineProps<{ active?: boolean }>(), { active: true })
const emit = defineEmits<{ (e: 'status', s: MyceliumInfo | null): void }>()
const { t } = useI18n()

const info = ref<MyceliumInfo | null>(null)
const cfg = ref<MyceliumSettings>({ ...DEFAULT_MYCELIUM_SETTINGS })
const bootText = ref('')
const busy = ref(false)
const vpnBusy = ref(false)
const vpnErr = ref('')
const saved = ref(false)
const techOpen = ref(false)
const err = ref('')
let timer: ReturnType<typeof setInterval> | null = null

const st = computed(() => info.value?.status || null)
const vpn = computed(() => info.value?.vpn || null)
const vpnLevel = computed<'off' | 'start' | 'on' | 'fail'>(() => {
  const v = vpn.value
  if (!v) return 'off'
  if (v.enabled && v.running) return 'on'
  if (v.enabled) return 'start'
  return v.error ? 'fail' : 'off'
})
// VPN можно включить только при активных цепочках и найденном помощнике
const vpnCanOn = computed(() => !!vpn.value && vpn.value.supported && vpn.value.helperFound && cfg.value.enabled && (st.value?.circuitsReady ?? 0) > 0)
function humanUp(s: number): string {
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60)
  return h ? `${h}h ${m}m` : m ? `${m}m ${s % 60}s` : `${s}s`
}
async function toggleVpn() {
  const v = vpn.value
  if (!v) return
  vpnBusy.value = true
  vpnErr.value = ''
  try {
    const r = await withTimeout(myceliumVpnSet(!v.enabled), 30000)
    if (r) {
      info.value = r
      emit('status', r)
    }
  } catch (e) {
    vpnErr.value = String((e as Error)?.message ?? e)
  } finally {
    vpnBusy.value = false
  }
}
const level = computed<'off' | 'build' | 'ok' | 'fail'>(() => {
  if (!cfg.value.enabled) return 'off'
  const s = st.value
  if (s && s.circuitsReady > 0) return 'ok'
  if (s && (s.circuitsBuilding || !s.lastError)) return 'build'
  return 'fail'
})
const relayLine = computed(() => {
  if (info.value?.mobile) return t('mycelium.relayMobile')
  if (info.value?.systemDaemon) return t('mycelium.relaySystem')
  if (!cfg.value.enabled) return t('mycelium.relayIdle')
  return st.value?.relayEnabled ? t('mycelium.relayOn') : t('mycelium.relayOff')
})

function humanBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  if (n < 1024 ** 3) return `${(n / 1024 / 1024).toFixed(1)} MB`
  return `${(n / 1024 ** 3).toFixed(2)} GB`
}
const short = (id: string) => (id ? id.slice(0, 10) + '…' + id.slice(-4) : '—')

async function load() {
  const r = await myceliumStatus()
  info.value = r
  if (r?.settings && !busy.value) {
    cfg.value = { ...r.settings }
    if (!bootText.value) bootText.value = r.settings.bootstrap.join('\n')
  }
  emit('status', r)
}
// ответ бэкенда не должен блокировать карточку бесконечно: 20 с → ошибка, настройки перечитываются
function withTimeout<T>(p: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(t('mycelium.timeout'))), ms)
    p.then((v) => { clearTimeout(timer); resolve(v) }, (e) => { clearTimeout(timer); reject(e) })
  })
}
async function apply(patch: Partial<MyceliumSettings>) {
  busy.value = true
  const prev = cfg.value
  const next = { ...cfg.value, ...patch }
  cfg.value = next
  err.value = ''
  try {
    const r = await withTimeout(myceliumSet(next), 20000)
    if (r) {
      info.value = r
      emit('status', r)
    }
  } catch (e) {
    cfg.value = prev
    err.value = String((e as Error)?.message ?? e)
    void load()
  } finally {
    busy.value = false
  }
}
// новая цепочка встала в слот старой — подменяем в списке до следующего опроса
function onRebuilt(c: MyceliumCircuit, oldId: number) {
  err.value = ''
  if (!info.value?.circuits) return
  const i = info.value.circuits.findIndex((x) => x.id === oldId || x.id === c.id)
  if (i >= 0) info.value.circuits.splice(i, 1, c)
  else info.value.circuits.push(c)
  void load()
}
function parseBoot(): string[] {
  return bootText.value.split('\n').map((s) => s.trim()).filter(Boolean)
}
// есть ли в поле несохранённые bootstrap-релеи
function bootDirty(): boolean {
  const cur = parseBoot()
  const saved = cfg.value.bootstrap || []
  return cur.length !== saved.length || cur.some((v, i) => v !== saved[i])
}
async function saveBootstrap() {
  await apply({ bootstrap: parseBoot() })
  saved.value = true
  setTimeout(() => (saved.value = false), 1600)
}
// автосохранение: уход из поля (blur/change) или закрытие карточки — чтобы список не терялся без кнопки
function autoSaveBootstrap() {
  if (bootTimer) { clearTimeout(bootTimer); bootTimer = null }
  if (bootDirty()) void saveBootstrap()
}
// и через 1.5 с после последнего ввода — на случай закрытия окна с фокусом в поле
let bootTimer: ReturnType<typeof setTimeout> | null = null
function onBootInput() {
  if (bootTimer) clearTimeout(bootTimer)
  bootTimer = setTimeout(autoSaveBootstrap, 1500)
}

function startPoll() {
  if (!timer) timer = setInterval(() => void load(), 3000)
}
function stopPoll() {
  if (timer) clearInterval(timer)
  timer = null
}
watch(() => props.active, (a) => (a ? (void load(), startPoll()) : stopPoll()))
let unlistenVpn: (() => void) | null = null
onMounted(async () => {
  void load()
  if (props.active) startPoll()
  unlistenVpn = await onMyceliumVpnError((e) => { vpnErr.value = e; void load() })
})
onUnmounted(() => {
  stopPoll()
  unlistenVpn?.()
  autoSaveBootstrap()
})
</script>

<template>
  <section class="panel block myc" data-testid="settings-mycelium">
    <div class="myc-head">
      <div class="myc-title">
        <span class="myc-name">{{ t('mycelium.title') }}</span>
        <span class="myc-exp" data-testid="mycelium-experimental">{{ t('mycelium.experimental') }}</span>
      </div>
      <button
        class="myc-switch"
        :class="{ on: cfg.enabled }"
        role="switch"
        :aria-checked="cfg.enabled"
        :disabled="busy || (info !== null && !info.started)"
        @click="apply({ enabled: !cfg.enabled })"
        data-testid="mycelium-toggle"
      ><span class="knob" /></button>
    </div>
    <p class="myc-hint">
      {{ t('mycelium.hint') }}
      <span class="myc-link" title="docs/NIP-1-Mycelium.md" data-testid="mycelium-nip-link">{{ t('mycelium.nip') }}</span>
    </p>
    <p v-if="err" class="myc-warn" data-testid="mycelium-error">{{ err }}</p>
    <p v-if="info && !info.started" class="myc-warn" data-testid="mycelium-unavailable">{{ t('mycelium.unavailable') }}</p>

    <!-- статус: цвет за секунду -->
    <div class="myc-status" :class="level" data-testid="mycelium-status">
      <i class="sdot" />
      <span class="mono" data-testid="mycelium-status-text">
        <template v-if="level === 'off'">{{ t('mycelium.stOff') }}</template>
        <template v-else-if="level === 'build'">{{ t('mycelium.stBuilding') }}</template>
        <template v-else-if="level === 'ok'">{{ t('mycelium.stActive', { c: st?.circuitsReady ?? 0, r: st?.relaysKnown ?? 0 }) }}</template>
        <template v-else>{{ t('mycelium.stFail') }}</template>
      </span>
    </div>
    <p v-if="level === 'fail'" class="myc-warn" data-testid="mycelium-fail-hint">{{ t('mycelium.stFailHint') }}</p>
    <p v-if="cfg.enabled && st?.hopsReduced" class="myc-warn" data-testid="mycelium-hops-reduced">
      {{ t('mycelium.hopsReduced', { n: st?.hopsEffective ?? 0, h: st?.hops ?? cfg.hops, r: st?.relaysUsable ?? 0 }) }}
    </p>
    <p v-if="info?.dvpnPaused" class="myc-dvpn" data-testid="mycelium-dvpn-paused">⏸ {{ t('mycelium.dvpnPaused') }}</p>

    <div class="myc-grid">
      <div class="myc-col">
        <label class="myc-opt" data-testid="mycelium-allow-direct-row">
          <input type="checkbox" :checked="cfg.allowDirect" @change="apply({ allowDirect: !cfg.allowDirect })" data-testid="mycelium-allow-direct" />
          <span class="opt-t">{{ t('mycelium.allowDirect') }}</span>
          <span class="opt-h">{{ t('mycelium.allowDirectHint') }}</span>
        </label>
        <label class="myc-opt" data-testid="mycelium-cover-row">
          <input type="checkbox" :checked="cfg.coverTraffic" :disabled="!cfg.enabled" @change="apply({ coverTraffic: !cfg.coverTraffic })" data-testid="mycelium-cover-traffic" />
          <span class="opt-t">{{ t('mycelium.cover') }}</span>
          <span class="opt-h">{{ t('mycelium.coverHint') }}</span>
        </label>
        <p v-if="cfg.enabled" class="myc-line" data-testid="mycelium-hidden-line">
          <i class="sdot sm" :class="(st?.introReady ?? 0) > 0 ? 'ok' : 'off'" />
          {{ t('mycelium.hidden') }}:
          <b class="mono">{{ (st?.introReady ?? 0) > 0 ? t('mycelium.hiddenOn', { n: st?.introReady ?? 0 }) : t('mycelium.hiddenWait') }}</b>
          <em v-if="st?.servicesKnown"> · {{ t('mycelium.servicesKnown', { n: st?.servicesKnown }) }}</em>
        </p>
        <ExitDefaultRow />
        <label class="myc-opt dimmed" data-testid="mycelium-voice-row">
          <input type="checkbox" :checked="false" disabled data-testid="mycelium-voice" />
          <span class="opt-t">{{ t('mycelium.voice') }}</span>
          <span class="opt-h">{{ t('mycelium.voiceHint') }}</span>
        </label>
        <ul class="myc-scope" data-testid="mycelium-scope">
          <li class="via">{{ t('mycelium.routed') }}</li>
          <li class="dir">{{ t('mycelium.direct') }}</li>
          <li class="lat">{{ t('mycelium.latency') }}</li>
        </ul>
      </div>

      <div class="myc-col">
        <h4 class="myc-sub">{{ t('mycelium.relayTitle') }}</h4>
        <p class="myc-line mono" data-testid="mycelium-relay-line">{{ relayLine }}</p>
        <label v-if="!info?.mobile" class="myc-opt inline" data-testid="mycelium-client-only-row">
          <input type="checkbox" :checked="cfg.clientOnly" @change="apply({ clientOnly: !cfg.clientOnly })" data-testid="mycelium-client-only" />
          <span class="opt-t">{{ t('mycelium.clientOnly') }}</span>
        </label>
        <div v-if="!info?.mobile" class="myc-bw" data-testid="mycelium-bandwidth">
          <span class="opt-t">{{ t('mycelium.bandwidth') }}</span>
          <input
            type="range" min="256" max="16384" step="256"
            :value="cfg.bandwidthKb"
            :disabled="cfg.clientOnly"
            @change="apply({ bandwidthKb: Number(($event.target as HTMLInputElement).value) })"
            data-testid="mycelium-bandwidth-range"
          />
          <b class="mono" data-testid="mycelium-bandwidth-value">{{ (cfg.bandwidthKb / 1024).toFixed(2) }} MB/s</b>
        </div>
        <div class="myc-metrics" data-testid="mycelium-metrics">
          <div><span>{{ t('mycelium.served') }}</span><b class="mono" data-testid="mycelium-m-served">{{ st?.relayCircuitsTotal ?? 0 }} <em>· {{ st?.relayCircuitsActive ?? 0 }} {{ t('mycelium.active') }}</em></b></div>
          <div><span>{{ t('mycelium.cells') }}</span><b class="mono" data-testid="mycelium-m-cells">{{ st?.relayCells ?? 0 }}</b></div>
          <div><span>{{ t('mycelium.bytes') }}</span><b class="mono" data-testid="mycelium-m-bytes">{{ humanBytes(st?.relayBytes ?? 0) }}</b></div>
          <div><span>{{ t('mycelium.exits') }}</span><b class="mono" data-testid="mycelium-m-exits">{{ st?.exitStreams ?? 0 }}</b></div>
          <div><span>{{ t('mycelium.rendezvousServed') }}</span><b class="mono" data-testid="mycelium-m-rv">{{ st?.relayRendezvousTotal ?? 0 }} <em>· {{ st?.relayIntroActive ?? 0 }} intro</em></b></div>
        </div>
        <p class="myc-line" data-testid="mycelium-system">
          <i class="sdot sm" :class="info?.systemDaemon ? 'ok' : 'off'" />
          {{ t('mycelium.system') }}:
          <b class="mono">{{ info?.systemDaemon ? t('mycelium.systemOn', { port: info?.socksPort ?? 9150 }) : t('mycelium.systemOff') }}</b>
        </p>
      </div>
    </div>

    <!-- Фаза 3: системный VPN — TUN на всём устройстве через цепочку -->
    <div v-if="vpn" class="myc-vpn" :class="vpnLevel" data-testid="mycelium-vpn">
      <div class="vpn-head">
        <div class="vpn-title">
          <span class="vpn-name">{{ t('mycelium.vpnTitle') }}</span>
        </div>
        <button
          class="myc-switch vpn"
          :class="{ on: vpn.enabled }"
          role="switch"
          :aria-checked="vpn.enabled"
          :disabled="vpnBusy || (!vpn.enabled && !vpnCanOn)"
          :title="!vpn.enabled && !vpnCanOn ? t('mycelium.vpnNeedMycelium') : ''"
          @click="toggleVpn"
          data-testid="mycelium-vpn-toggle"
        ><span class="knob" /></button>
      </div>
      <p class="myc-hint">{{ t('mycelium.vpnHint') }}</p>
      <div class="vpn-status" data-testid="mycelium-vpn-status">
        <i class="sdot" :class="vpnLevel" />
        <span class="mono" data-testid="mycelium-vpn-status-text">
          <template v-if="vpnLevel === 'on'">{{ t('mycelium.vpnOn') }}</template>
          <template v-else-if="vpnLevel === 'start'">{{ t('mycelium.vpnStarting') }}</template>
          <template v-else>{{ t('mycelium.vpnOff') }}</template>
        </span>
      </div>
      <p v-if="vpnLevel === 'on'" class="myc-line mono" data-testid="mycelium-vpn-stats">
        {{ t('mycelium.vpnStats', { up: humanUp(vpn.upSecs), tx: humanBytes(vpn.tx), rx: humanBytes(vpn.rx), n: vpn.sessions }) }}
        <br />{{ t('mycelium.vpnBypass', { n: vpn.bypass }) }}
      </p>
      <p v-if="vpnErr || vpn.error" class="myc-warn" data-testid="mycelium-vpn-error">{{ vpnErr || vpn.error }}</p>
      <p v-if="!vpn.supported" class="myc-warn" data-testid="mycelium-vpn-unsupported">{{ t('mycelium.vpnUnsupported') }}</p>
      <p v-else-if="!vpn.helperFound" class="myc-warn" data-testid="mycelium-vpn-no-helper">{{ t('mycelium.vpnNoHelper') }}</p>
      <p v-else-if="!vpn.enabled && !vpnCanOn" class="myc-line" data-testid="mycelium-vpn-need">{{ t('mycelium.vpnNeedMycelium') }}</p>
      <ul class="myc-scope" data-testid="mycelium-vpn-notes">
        <li class="lat">{{ t('mycelium.vpnAdmin') }}</li>
        <li class="lat">{{ t('mycelium.vpnDns') }}</li>
      </ul>
    </div>

    <!-- живая карта цепочек: Вы → Guard → Middle → Exit → собеседник -->
    <div v-if="cfg.enabled" class="myc-map" data-testid="mycelium-map">
      <h4 class="myc-sub">{{ t('mycelium.mapTitle') }}</h4>
      <p v-if="!info?.circuits?.length" class="myc-line" data-testid="mycelium-map-empty">{{ t('mycelium.mapEmpty') }}</p>
      <div v-else class="myc-map-list">
        <CircuitMap v-for="c in info.circuits" :key="c.id" :circuit="c" :my-id="info.nodeId" @rebuilt="onRebuilt" @error="err = $event" />
      </div>
    </div>

    <!-- технические детали — свёрнуты -->
    <button class="myc-tech-btn" @click="techOpen = !techOpen" :aria-expanded="techOpen" data-testid="mycelium-tech-toggle">
      {{ techOpen ? '▾' : '▸' }} {{ t('mycelium.tech') }}
    </button>
    <div v-if="techOpen" class="myc-tech" data-testid="mycelium-tech">
      <div class="tech-row">
        <span>{{ t('mycelium.hops') }}</span>
        <div class="hops">
          <button v-for="h in [1, 2, 3]" :key="h" class="hop" :class="{ on: cfg.hops === h }" @click="apply({ hops: h })" :data-testid="`mycelium-hops-${h}`">{{ h }}</button>
        </div>
        <span v-if="cfg.hops < 3" class="warn-t" data-testid="mycelium-hops-warn">{{ t('mycelium.hopsWarn') }}</span>
      </div>
      <div class="tech-row"><span>{{ t('mycelium.circuits') }}</span><b class="mono">{{ st?.circuitsReady ?? 0 }}</b></div>
      <div class="tech-row"><span>{{ t('mycelium.relays') }}</span><b class="mono" data-testid="mycelium-relays-known">{{ st?.relaysKnown ?? 0 }} <em v-if="st?.relaysUsable !== undefined">· {{ t('mycelium.relaysUsable', { n: st?.relaysUsable ?? 0 }) }}</em></b></div>
      <div class="tech-row"><span>{{ t('mycelium.nodeId') }}</span><code class="mono" :title="info?.nodeId" data-testid="mycelium-node-id">{{ short(info?.nodeId || '') }}</code></div>
      <div class="tech-row"><span>{{ t('mycelium.transportId') }}</span><code class="mono" :title="info?.transportId || t('mycelium.transportIdle')" data-testid="mycelium-transport-id">{{ info?.transportId ? short(info.transportId) : t('mycelium.transportIdle') }}</code></div>
      <div class="tech-row top">
        <span>{{ t('mycelium.guards') }}</span>
        <div class="guards" data-testid="mycelium-guards">
          <code v-for="g in st?.guards || []" :key="g" class="mono">{{ short(g) }}</code>
          <code v-if="!st?.guards?.length" class="mono dimc">—</code>
        </div>
      </div>
      <div v-if="st?.lastError" class="tech-row"><span>{{ t('mycelium.lastError') }}</span><code class="mono err" data-testid="mycelium-last-error">{{ st.lastError }}<template v-if="st.lastErrorAgeSecs"> · {{ t('mycelium.agoSecs', { n: st.lastErrorAgeSecs }) }}</template></code></div>
      <label class="boot-l">{{ t('mycelium.bootstrap') }}</label>
      <textarea v-model="bootText" class="boot mono" rows="3" spellcheck="false" placeholder="5f3a…e9d1" @change="autoSaveBootstrap" @input="onBootInput" data-testid="mycelium-bootstrap-input" />
      <button class="myc-save" @click="saveBootstrap" data-testid="mycelium-bootstrap-save">{{ saved ? t('mycelium.saved') : t('mycelium.save') }}</button>
    </div>
  </section>
</template>

<style scoped>
.myc { --ok: #2ee6a6; --build: #f5c451; --fail: #ff5d6c; }
.myc-head { display: flex; align-items: center; justify-content: space-between; gap: 14px; }
.myc-title { display: flex; align-items: baseline; gap: 10px; }
.myc-name { font-size: 12px; letter-spacing: 0.16em; text-transform: uppercase; font-weight: var(--fw-800); }
.myc-exp { font-size: 9px; letter-spacing: 0.18em; text-transform: uppercase; color: var(--build); border: 1px solid currentColor; padding: 1px 6px; }
.myc-switch { flex: none; width: 46px; height: 24px; border-radius: 999px; background: var(--ui-accent, #1c232b); border: 1px solid var(--border); position: relative; cursor: pointer; transition: background-color 0.18s ease, border-color 0.18s ease; }
.myc-switch.on { background: var(--ok); border-color: var(--ok); }
.myc-switch:disabled { opacity: 0.5; cursor: default; }
.myc-switch .knob { position: absolute; top: 2px; left: 2px; width: 18px; height: 18px; border-radius: 50%; background: var(--bg); transition: transform 0.18s ease; }
.myc-switch.on .knob { transform: translateX(22px); }
.myc-hint { margin: 10px 0 0; font-size: 12px; line-height: 1.55; color: var(--text-dim); max-width: 78ch; }
.myc-link { color: var(--accent); margin-left: 6px; white-space: nowrap; }
.myc-status { display: inline-flex; align-items: center; gap: 10px; margin-top: 16px; padding: 8px 14px; border: 1px solid var(--border); font-size: 12px; transition: border-color 0.2s ease, color 0.2s ease; }
.myc-status .sdot, .sdot { width: 8px; height: 8px; border-radius: 50%; background: #55606d; display: inline-block; flex: none; }
.sdot.sm { width: 6px; height: 6px; margin-right: 4px; }
.sdot.ok { background: var(--ok); }
.myc-status.ok { border-color: var(--ok); color: var(--ok); }
.myc-status.ok .sdot { background: var(--ok); box-shadow: 0 0 8px var(--ok); }
.myc-status.build { border-color: var(--build); color: var(--build); }
.myc-status.build .sdot { background: var(--build); animation: myc-pulse 1.2s ease-in-out infinite; }
.myc-status.fail { border-color: var(--fail); color: var(--fail); }
.myc-status.fail .sdot { background: var(--fail); }
@keyframes myc-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.3; } }
.myc-warn { margin: 8px 0 0; font-size: 11px; color: var(--fail); max-width: 78ch; }
.myc-dvpn { margin: 8px 0 0; font-size: 11px; color: var(--build); }
.myc-grid { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 0 32px; margin-top: 18px; }
@media (max-width: 1100px) { .myc-grid { grid-template-columns: 1fr; } }
.myc-col { min-width: 0; }
.myc-opt { display: grid; grid-template-columns: auto 1fr; gap: 2px 10px; align-items: start; padding: 10px 12px; border: 1px solid var(--border); margin-bottom: 10px; cursor: pointer; transition: border-color 0.16s ease; }
.myc-opt:hover { border-color: var(--text-dim); }
.myc-opt.inline { grid-template-columns: auto 1fr; padding: 8px 12px; }
.myc-opt.dimmed { opacity: 0.55; cursor: default; }
.myc-opt input { margin-top: 2px; accent-color: var(--ok); }
.opt-t { font-size: 12px; font-weight: var(--fw-700, 700); }
.opt-h { grid-column: 2; font-size: 11px; color: var(--text-dim); line-height: 1.45; }
.myc-scope { list-style: none; margin: 4px 0 0; padding: 0; font-size: 11px; color: var(--text-dim); display: grid; gap: 4px; }
.myc-scope li::before { content: '›'; margin-right: 6px; }
.myc-scope .via::before { color: var(--ok); }
.myc-scope .dir::before { color: var(--build); }
.myc-sub { margin: 0 0 8px; font-size: 10px; letter-spacing: 0.16em; color: var(--accent); font-weight: var(--fw-800); text-transform: uppercase; }
.myc-line { margin: 0 0 10px; font-size: 11px; color: var(--text-dim); }
.myc-line em { font-style: normal; }
.myc-bw { display: grid; grid-template-columns: auto 1fr auto; gap: 10px; align-items: center; margin: 4px 0 12px; }
.myc-bw input { accent-color: var(--ok); }
.myc-metrics { display: grid; grid-template-columns: 1fr 1fr; gap: 1px; background: var(--border); border: 1px solid var(--border); margin-bottom: 12px; }
.myc-metrics > div { background: var(--bg); padding: 8px 10px; display: grid; gap: 2px; }
.myc-metrics span { font-size: 9px; letter-spacing: 0.14em; text-transform: uppercase; color: var(--text-dim); }
.myc-metrics b { font-size: 13px; }
.myc-metrics em { font-style: normal; font-size: 10px; color: var(--text-dim); }
.myc-map { margin-top: 18px; }
.myc-vpn { margin-top: 18px; padding: 14px 16px; border: 1px solid var(--border); transition: border-color 0.2s ease; }
.myc-vpn.on { border-color: var(--ok); }
.myc-vpn.start { border-color: var(--build); }
.myc-vpn.fail { border-color: var(--fail); }
.vpn-head { display: flex; align-items: center; justify-content: space-between; gap: 14px; }
.vpn-title { display: flex; align-items: baseline; gap: 10px; }
.vpn-name { font-size: 11px; letter-spacing: 0.08em; font-weight: var(--fw-800); }
.vpn-status { display: inline-flex; align-items: center; gap: 10px; margin-top: 12px; font-size: 12px; }
.vpn-status .sdot.on { background: var(--ok); box-shadow: 0 0 8px var(--ok); }
.vpn-status .sdot.start { background: var(--build); animation: myc-pulse 1.2s ease-in-out infinite; }
.vpn-status .sdot.fail { background: var(--fail); }
.myc-vpn .myc-line { margin-top: 8px; line-height: 1.6; }
.myc-map-list { display: grid; gap: 10px; }
.myc-tech-btn { align-self: flex-start; justify-self: start; text-align: left; margin-top: 6px; background: transparent; border: 0; padding: 4px 0; color: var(--text-dim); font-size: 11px; letter-spacing: 0.08em; cursor: pointer; transition: color 0.16s ease; }
.myc-tech-btn:hover { color: var(--text); }
.myc-tech { margin-top: 8px; padding: 12px 14px; border: 1px dashed var(--border); display: grid; gap: 8px; font-size: 11px; }
.tech-row { display: grid; grid-template-columns: 180px auto 1fr; gap: 10px; align-items: center; color: var(--text-dim); }
.tech-row.top { align-items: start; }
.tech-row b, .tech-row code { color: var(--text); }
.hops { display: inline-flex; border: 1px solid var(--border); }
.hop { width: 30px; height: 24px; background: transparent; border: 0; border-right: 1px solid var(--border); color: var(--text-dim); cursor: pointer; transition: background-color 0.16s ease, color 0.16s ease; }
.hop:last-child { border-right: 0; }
.hop.on { background: var(--ok); color: var(--bg); font-weight: var(--fw-800); }
.warn-t { color: var(--build); }
.guards { display: grid; gap: 2px; }
.dimc { color: var(--text-dim) !important; }
.err { color: var(--fail) !important; word-break: break-word; }
.boot-l { margin-top: 6px; color: var(--text-dim); }
.boot { width: 100%; background: var(--bg); color: var(--text); border: 1px solid var(--border); padding: 8px; font-size: 11px; resize: vertical; }
.myc-save { justify-self: start; padding: 6px 14px; background: transparent; border: 1px solid var(--ok); color: var(--ok); font-size: 10px; letter-spacing: 0.16em; font-weight: var(--fw-800); cursor: pointer; transition: background-color 0.16s ease, color 0.16s ease; }
.myc-save:hover:not(:disabled) { background: var(--ok); color: var(--bg); }
.mono { font-family: var(--mono, ui-monospace, monospace); }
</style>
