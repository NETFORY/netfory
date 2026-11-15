<script setup lang="ts">
import { computed, defineEmits, defineProps, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { MYCELIUM_AUTO_REBUILD_MAX, myceliumRebuild, type MyceliumCircuit } from '@/lib/bridge'

// Живая схема цепочки: Вы → Guard → Middle → Exit → собеседник, задержка каждого участка.
// RTT приходят накопительными (до хопа k), участок = RTT(k) − RTT(k−1).
// Красный участок (мёртвый хоп или > 200 мс) → кнопка «Построить новую цепочку»; до неё Mycelium
// сам делает до MYCELIUM_AUTO_REBUILD_MAX попыток (счётчик `retries`).
const props = defineProps<{ circuit: MyceliumCircuit; peerLabel?: string; testid?: string; myId?: string }>()
const emit = defineEmits<{ (e: 'rebuilt', c: MyceliumCircuit, oldId: number): void; (e: 'error', msg: string): void }>()
const { t } = useI18n()

const ROLES = ['guard', 'middle', 'exit'] as const
const short = (id: string) => id.slice(0, 6) + '…' + id.slice(-4)

// Фаза 2: у rendezvous/service последний хоп — точка встречи (RP), у intro — intro-точка
const kind = computed(() => props.circuit.kind || 'exit')
const lastRole = computed(() => (kind.value === 'rendezvous' || kind.value === 'service' ? 'rp' : kind.value === 'intro' ? 'intro' : 'exit'))
const hops = computed(() =>
  props.circuit.path.map((id, i) => {
    const n = props.circuit.path.length
    const role = n === 1 ? lastRole.value : i === 0 ? 'guard' : i === n - 1 ? lastRole.value : 'middle'
    return { id, role: role as (typeof ROLES)[number] | 'rp' | 'intro', alive: props.circuit.hopAlive?.[i] ?? true }
  }),
)
// хвост карты: Exit → собеседник (неизмеряемый), RP → собеседник через его 3 хопа, intro → ожидание
const tailLabel = computed(() => {
  if (kind.value === 'rendezvous' || kind.value === 'service') return t('mycelium.peerHidden')
  if (kind.value === 'intro') return t('mycelium.introWait')
  return props.circuit.target ? t('mycelium.peer') : t('mycelium.pool')
})
// Пороги участка: до 300 мс — зелёный, до 1.5 с — оранжевый (нормально для сайтов, быстрее Tor),
// дольше — красный (как SEGMENT_SLOW_MS в крейте: повод пересобрать цепочку)
const SEG_OK_MS = 300
const SEG_SLOW_MS = 1500
const segs = computed(() =>
  props.circuit.hopRttMs.map((r, i) => {
    if (r == null) return null
    const prev = i === 0 ? 0 : props.circuit.hopRttMs[i - 1]
    return prev == null ? null : Math.max(0, r - prev)
  }),
)
const toExit = computed(() => props.circuit.hopRttMs[props.circuit.hopRttMs.length - 1] ?? null)
const segClass = (ms: number | null, i: number) => (!hops.value[i]?.alive ? 'dead' : ms == null ? 'na' : ms < SEG_OK_MS ? 'fast' : ms < SEG_SLOW_MS ? 'mid' : 'slow')
const peer = computed(() => {
  if (kind.value === 'intro') return t('mycelium.introHint')
  if (kind.value === 'service') return t('mycelium.serviceHint')
  return props.peerLabel || (props.circuit.target ? short(props.circuit.target) : t('mycelium.pool'))
})

// красный участок на карте — повод предложить пересборку
const red = computed(() => hops.value.some((h) => !h.alive) || segs.value.some((s) => s != null && s >= SEG_SLOW_MS))
const busy = ref(false)
const rebuilding = computed(() => busy.value || !!props.circuit.rebuilding)
const retries = computed(() => props.circuit.retries ?? 0)
const autoLeft = computed(() => Math.max(0, MYCELIUM_AUTO_REBUILD_MAX - retries.value))
// новая цепочка пришла (другой id) — локальная занятость снимается
watch(() => props.circuit.id, () => (busy.value = false))

async function rebuild() {
  if (rebuilding.value) return
  busy.value = true
  const oldId = props.circuit.id
  try {
    emit('rebuilt', await myceliumRebuild(oldId), oldId)
  } catch (e) {
    emit('error', String(e))
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="cmap" :class="{ red, rebuilding }" :data-testid="testid || `mycelium-map-${circuit.id}`">
    <div class="track">
      <div class="node me" :title="myId" data-testid="cmap-node-me"><i class="ndot" /><span class="nrole">{{ t('mycelium.you') }}</span><code v-if="myId" class="nid mono" data-testid="cmap-node-me-id">{{ short(myId) }}</code></div>
      <template v-for="(h, i) in hops" :key="h.id">
        <div class="seg" :class="segClass(segs[i], i)" :data-testid="`cmap-seg-${i}`">
          <span class="line"><i class="pulse" :style="{ animationDelay: `${i * 0.35}s` }" /></span>
          <span class="ms mono">{{ !h.alive ? t('mycelium.hopDead') : segs[i] == null ? '…' : `${segs[i]} ${t('mycelium.ms')}` }}</span>
        </div>
        <div class="node" :class="[h.role, { dead: !h.alive, rp: h.role === 'rp' || h.role === 'intro' }]" :title="h.alive ? h.id : `${h.id} — ${t('mycelium.hopDeadHint')}`" :data-testid="`cmap-node-${h.role}`">
          <i class="ndot" />
          <span class="nrole">{{ t(`mycelium.${h.role}`) }}</span>
          <code class="nid mono">{{ short(h.id) }}</code>
        </div>
      </template>
      <div class="seg na last" :title="kind === 'exit' ? t('mycelium.exitPeerHint') : kind === 'intro' ? t('mycelium.introHint') : t('mycelium.rpHint')" data-testid="cmap-seg-peer">
        <span class="line dashed" />
        <span class="ms mono">{{ kind === 'rendezvous' || kind === 'service' ? '···3···' : '?' }}</span>
      </div>
      <div class="node peer" :class="{ pool: !circuit.target && kind === 'exit', hidden: kind !== 'exit' }" data-testid="cmap-node-peer">
        <i class="ndot" />
        <span class="nrole">{{ tailLabel }}</span>
        <code class="nid mono">{{ peer }}</code>
      </div>
    </div>
    <div v-if="kind !== 'exit'" class="kind mono" :class="kind" :data-testid="`cmap-kind-${kind}`">{{ t(`mycelium.kind_${kind}`) }}</div>
    <div class="foot mono">
      <span data-testid="cmap-to-exit">{{ t(lastRole === 'rp' ? 'mycelium.toRp' : lastRole === 'intro' ? 'mycelium.toIntro' : 'mycelium.toExit') }}: <b>{{ toExit == null ? '…' : `${toExit} ${t('mycelium.ms')}` }}</b></span>
      <span v-if="circuit.target" data-testid="cmap-e2e">
        {{ t('mycelium.toPeer') }}:
        <b v-if="circuit.e2eMs != null">{{ circuit.e2eMs }} {{ t('mycelium.ms') }}</b>
        <b v-else>—</b>
        <em v-if="circuit.e2eAgeSecs != null">· {{ t('mycelium.agoSec', { n: circuit.e2eAgeSecs }) }}</em>
        <em v-else>· {{ t('mycelium.e2eNone') }}</em>
      </span>
      <span class="dim">{{ t('mycelium.circAge', { n: Math.round(circuit.ageSecs / 60) }) }}</span>
    </div>
    <!-- пересборка: показывается, когда участок горит красным (или уже строится замена) -->
    <div v-if="red || rebuilding" class="rebuild" data-testid="cmap-rebuild">
      <span class="rb-txt" data-testid="cmap-rebuild-hint">
        <template v-if="rebuilding">{{ t('mycelium.rebuilding') }}</template>
        <template v-else-if="autoLeft > 0">{{ t('mycelium.autoRetry', { n: retries, max: MYCELIUM_AUTO_REBUILD_MAX }) }}</template>
        <template v-else>{{ t('mycelium.autoExhausted', { max: MYCELIUM_AUTO_REBUILD_MAX }) }}</template>
      </span>
      <button class="rb-btn" :class="{ wait: rebuilding }" :disabled="rebuilding" @click="rebuild" data-testid="cmap-rebuild-btn">
        <i v-if="rebuilding" class="rb-spin" />{{ rebuilding ? t('mycelium.rebuildingBtn') : t('mycelium.rebuild') }}
      </button>
    </div>
  </div>
</template>

<style scoped>
.cmap { --ok: #2ee6a6; --mid: #f5c451; --slow: #ff5d6c; padding: 12px 14px; border: 1px solid var(--border); background: color-mix(in srgb, var(--bg) 70%, transparent); transition: border-color 0.2s ease; }
.cmap.red { border-color: color-mix(in srgb, var(--slow) 55%, var(--border)); }
.track { display: flex; align-items: flex-start; overflow-x: auto; padding-bottom: 4px; }
.node { flex: none; display: grid; justify-items: center; gap: 3px; min-width: 64px; }
.ndot { width: 12px; height: 12px; border-radius: 50%; border: 2px solid var(--ok); background: var(--bg); box-shadow: 0 0 8px rgba(46, 230, 166, 0.35); transition: border-color 0.2s ease, box-shadow 0.2s ease; }
.node.me .ndot { background: var(--ok); }
.node.peer .ndot { border-color: var(--text-dim); box-shadow: none; }
.node.peer.pool .ndot { border-style: dashed; }
.node.dead .ndot { border-color: var(--slow); box-shadow: 0 0 8px rgba(255, 93, 108, 0.45); background: repeating-linear-gradient(45deg, var(--slow) 0 2px, var(--bg) 2px 4px); }
.nrole { font-size: 9px; letter-spacing: 0.16em; text-transform: uppercase; color: var(--text-dim); }
.node.guard .nrole, .node.middle .nrole, .node.exit .nrole { color: var(--ok); }
.node.rp .nrole { color: #7fb8ff; }
.node.rp .ndot { border-color: #7fb8ff; box-shadow: 0 0 8px rgba(127, 184, 255, 0.45); }
.node.peer.hidden .ndot { border-color: #7fb8ff; border-style: dashed; box-shadow: none; }
.kind { margin-top: 8px; font-size: 10px; letter-spacing: 0.14em; text-transform: uppercase; color: #7fb8ff; }
.kind.intro { color: var(--text-dim); }
.node.dead .nrole { color: var(--slow); }
.nid { font-size: 10px; color: var(--text); }
.seg { flex: 1 1 60px; min-width: 56px; display: grid; justify-items: center; gap: 4px; padding-top: 5px; }
.line { position: relative; width: 100%; height: 2px; background: var(--ok); overflow: hidden; }
.seg.mid .line { background: var(--mid); }
.seg.slow .line { background: var(--slow); }
.seg.dead .line { background: repeating-linear-gradient(90deg, var(--slow) 0 4px, transparent 4px 8px); animation: cmap-blink 1.1s ease-in-out infinite; }
.seg.na .line { background: var(--border); }
.line.dashed { background: repeating-linear-gradient(90deg, var(--text-dim) 0 5px, transparent 5px 10px); }
.pulse { position: absolute; top: -2px; left: -8px; width: 8px; height: 6px; border-radius: 3px; background: #fff; opacity: 0.85; animation: cmap-flow 1.6s linear infinite; }
.seg.na .pulse, .seg.dead .pulse { display: none; }
@keyframes cmap-flow { from { left: -8px; } to { left: 100%; } }
@keyframes cmap-blink { 0%, 100% { opacity: 1; } 50% { opacity: 0.35; } }
.ms { font-size: 10px; color: var(--ok); }
.seg.mid .ms { color: var(--mid); }
.seg.slow .ms, .seg.dead .ms { color: var(--slow); }
.seg.dead .ms { font-weight: var(--fw-800, 800); letter-spacing: 0.08em; }
.seg.na .ms { color: var(--text-dim); }
.seg.last { cursor: help; }
.foot { display: flex; flex-wrap: wrap; gap: 6px 18px; margin-top: 10px; font-size: 11px; color: var(--text-dim); }
.foot b { color: var(--text); }
.foot em { font-style: normal; }
.rebuild { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 8px 16px; margin-top: 10px; padding-top: 10px; border-top: 1px dashed color-mix(in srgb, var(--slow) 45%, var(--border)); }
.rb-txt { font-size: 11px; color: var(--text-dim); line-height: 1.45; max-width: 60ch; }
.cmap.rebuilding .rb-txt { color: var(--mid); }
.rb-btn { display: inline-flex; align-items: center; gap: 8px; padding: 7px 14px; background: transparent; border: 1px solid var(--slow); color: var(--slow); font-size: 10px; letter-spacing: 0.16em; text-transform: uppercase; font-weight: var(--fw-800, 800); cursor: pointer; transition: background-color 0.16s ease, color 0.16s ease, border-color 0.16s ease; }
.rb-btn:hover:not(:disabled) { background: var(--slow); color: var(--bg); }
.rb-btn.wait, .rb-btn:disabled { border-color: var(--mid); color: var(--mid); cursor: default; }
.rb-spin { width: 10px; height: 10px; border-radius: 50%; border: 2px solid currentColor; border-right-color: transparent; animation: cmap-spin 0.8s linear infinite; }
@keyframes cmap-spin { to { transform: rotate(360deg); } }
.mono { font-family: var(--mono, ui-monospace, monospace); }
@media (prefers-reduced-motion: reduce) { .pulse { animation: none; display: none; } .seg.dead .line, .rb-spin { animation: none; } }
</style>
