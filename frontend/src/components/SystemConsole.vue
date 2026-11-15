<script setup lang="ts">
/* eslint-disable no-undef */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, triggerRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { appListen, sysLogHistory, toggleConsoleWindow, isTauri, type SysLogLine } from '@/lib/bridge'

const { t } = useI18n()

const props = defineProps<{
  /** Detached-режим: окно `syscon`. Растягиваем на весь viewport,
   *  скрываем кнопку «✕», иначе пользователь может случайно закрыть
   *  плавающую панель из detached-окна. */
  detached?: boolean
}>()
const emit = defineEmits<{ close: [] }>()

const MAX = 500
const ALL_SCOPES = ['dht', 'p2p', 'net', 'upd', 'sys', 'dapp'] as const
type Scope = (typeof ALL_SCOPES)[number]
const lines = ref<SysLogLine[]>([])
// Active scope filter. Default = network + update + dapp scopes; 'sys' off.
const active = ref<Set<Scope>>(new Set<Scope>(['dht', 'p2p', 'net', 'upd', 'dapp']))
const query = ref('')
let unlisten: (() => void) | null = null

const SCOPE_COLOR: Record<string, string> = {
  dht: '#f59e0b',
  p2p: '#34d3ee',
  net: '#a5b4fc',
  upd: '#f0abfc',
  sys: '#34d399',
  dapp: '#22d3ee',
}
function scopeColor(s: string): string {
  return SCOPE_COLOR[s] || '#8aa6bd'
}
function toggleScope(s: Scope) {
  const next = new Set(active.value)
  if (next.has(s)) next.delete(s)
  else next.add(s)
  active.value = next
}
function onlyScope(s: Scope) {
  active.value = new Set<Scope>([s])
}
function allScopes() {
  active.value = new Set<Scope>(ALL_SCOPES)
}

const visible = computed(() => {
  const scoped = lines.value.filter((l) => active.value.has(l.scope as Scope))
  const q = query.value.trim().toLowerCase()
  if (!q) return scoped
  return scoped.filter(
    (l) => l.msg.toLowerCase().includes(q) || l.scope.toLowerCase().includes(q),
  )
})

function fmtTime(ts: number): string {
  try {
    const d = new Date(ts)
    return d.toLocaleTimeString(undefined, { hour12: false })
  } catch {
    return ''
  }
}

const copied = ref(false)
async function copyAll() {
  const text = visible.value
    .map((l) => {
      const rep = (l.count || 1) > 1 ? ` ×${l.count}` : ''
      return `${fmtTime(l.ts)} [${l.scope}] ${l.msg}${rep}`
    })
    .join('\n')
  try {
    await navigator.clipboard.writeText(text)
    copied.value = true
    setTimeout(() => (copied.value = false), 1400)
  } catch {
    /* clipboard unavailable */
  }
}

/**
 * Save visible console lines to a text file (via browser download / Tauri
 * save-dialog). Useful for pasting logs into GitHub issues when reporting
 * bugs — especially the `dapp` scope which captures every network request
 * a dApp made during the session.
 */
async function exportTxt() {
  const text = visible.value
    .map((l) => {
      const rep = (l.count || 1) > 1 ? ` ×${l.count}` : ''
      return `${fmtTime(l.ts)} [${l.scope}] ${l.msg}${rep}`
    })
    .join('\n')
  const stamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19)
  const name = `smartnet-console-${stamp}.txt`
  try {
    const blob = new Blob([text], { type: 'text/plain;charset=utf-8' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = name
    document.body.appendChild(a)
    a.click()
    document.body.removeChild(a)
    setTimeout(() => URL.revokeObjectURL(url), 1000)
  } catch {
    /* download unavailable */
  }
}

/**
 * Colour token for a `dapp`-scoped line. The Rust emitter prefixes the
 * message with a tag — `OK` (ours: sth://, api://, sn://, f://, dev://),
 * `EXT` (external HTTP/HTTPS), `ERR` (any 4xx/5xx or transport failure),
 * `INT` (data:/blob:/relative). Everything else uses the default text tone.
 */
function dappLineColor(msg: string): string {
  const tag = msg.split(' ', 1)[0]
  switch (tag) {
    case 'OK':
      return '#22c55e' // green — наши схемы
    case 'EXT':
      return '#eab308' // yellow — публичный HTTP(S)
    case 'ERR':
      return '#ef4444' // red — ошибка / 4xx / 5xx
    case 'INT':
      return '#8aa6bd' // grey — data/blob/relative
    default:
      return ''
  }
}

// ---- dApp request-detail popup ---------------------------------------
// Rust проксирует полный payload `dapp-log-event` как `dapp-log-detail`.
// Мы храним последние 500 записей в Map<id, details> и по клику на строку
// с суффиксом `#<id>` открываем modal с headers/body/curl.
interface DappDetail {
  id: string
  method: string
  url: string
  status: number
  ms: number
  ok: boolean
  size?: number
  contentType?: string
  origin?: string
  reqHeaders?: Record<string, string>
  respHeaders?: Record<string, string>
  reqBody?: string
  err?: string
}
const details = shallowRef<Map<string, DappDetail>>(new Map())
const activeDetail = ref<DappDetail | null>(null)
let unlistenDetail: (() => void) | null = null

/** Extract `#<id>` suffix from a syslog msg (see Rust `dapp-log-event`). */
function dappRowId(msg: string): string {
  const m = /#([a-z0-9]{4,})\s*$/i.exec(msg)
  return m ? m[1] : ''
}

function openDetail(line: SysLogLine) {
  if (line.scope !== 'dapp') return
  const id = dappRowId(line.msg)
  if (!id) return
  const d = details.value.get(id)
  if (d) activeDetail.value = d
}

function closeDetail() {
  activeDetail.value = null
}

/**
 * Build a runnable `curl` command from a captured request. Escapes single
 * quotes so the string can be pasted straight into a POSIX shell.
 */
function toCurl(d: DappDetail): string {
  const q = (s: string) => `'${String(s).replace(/'/g, `'\\''`)}'`
  const parts: string[] = ['curl', '-X', d.method, q(d.url)]
  const h = d.reqHeaders || {}
  for (const k of Object.keys(h)) {
    if (h[k]) parts.push('-H', q(`${k}: ${h[k]}`))
  }
  if (d.reqBody && d.method !== 'GET' && d.method !== 'HEAD') {
    parts.push('--data-raw', q(d.reqBody))
  }
  parts.push('-i', '--compressed')
  return parts.join(' \\\n  ')
}

async function copyDetailField(text: string, label: string) {
  try {
    await navigator.clipboard.writeText(text)
    console.info('[sysconsole] copied', label)
  } catch {
    /* clipboard unavailable */
  }
}

function fmtBytes(n?: number): string {
  if (!n || n <= 0) return '—'
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  return `${(n / 1024 / 1024).toFixed(2)} MB`
}

function push(line: SysLogLine) {
  // Rust уже свернул повторы: если `replace: true`, находим совпадающую
  // запись в последних N строках того же scope+msg, поднимаем её в конец
  // с обновлённым count/ts (Rust сделал то же самое в ring buffer).
  // Раньше проверяли только `lines[last]` — при чередовании двух DHT
  // сообщений (A B A B…) совпадение никогда не находилось и UI дублировал
  // строки. Окно 6 совпадает с DEDUP_WINDOW в Rust.
  if (line.replace) {
    const window = 6
    const start = Math.max(0, lines.value.length - window)
    for (let i = lines.value.length - 1; i >= start; i--) {
      const cand = lines.value[i]
      if (cand.scope === line.scope && cand.msg === line.msg) {
        // Убираем из старой позиции…
        lines.value.splice(i, 1)
        // …и добавляем свежую версию в конец с обновлённым count.
        lines.value.push({ ...cand, ts: line.ts, count: line.count })
        return
      }
    }
    // Fallback: если не нашли (например, история не догрузилась) —
    // добавляем как обычную новую строку.
  }
  lines.value.push(line)
  if (lines.value.length > MAX) lines.value.splice(0, lines.value.length - MAX)
}

const bodyRef = ref<HTMLElement | null>(null)
function scrollToBottom() {
  nextTick(() => {
    const el = bodyRef.value
    if (el) el.scrollTop = el.scrollHeight
  })
}
watch(() => visible.value.length, scrollToBottom)

onMounted(async () => {
  try {
    const hist = await sysLogHistory()
    lines.value = hist.slice(-MAX)
  } catch {
    /* ignore */
  }
  unlisten = await appListen<SysLogLine>('sys-log', (p) => push(p))
  // Ловим полный payload каждого dApp fetch/XHR — кладём в Map<id, detail>
  // для popup'а. Trim до 500 записей чтобы не течь памятью.
  unlistenDetail = await appListen<DappDetail>('dapp-log-detail', (d) => {
    if (!d || !d.id) return
    // Мутируем Map in-place — БЕЗ клонирования (иначе на спаме `plugin:event|emit`
    // GC разваливается: тысячи Map(500) в секунду). shallowRef + triggerRef
    // даёт нам reactivity без deep-watch по значениям.
    const m = details.value
    m.set(d.id, d)
    if (m.size > 500) {
      const first = m.keys().next().value
      if (first !== undefined) m.delete(first)
    }
    triggerRef(details)
  })
  scrollToBottom()
})
onBeforeUnmount(() => {
  if (unlisten) unlisten()
  if (unlistenDetail) unlistenDetail()
})

/**
 * Открыть SystemConsole в отдельном Tauri-окне (`?w=syscon`). После открытия
 * закрываем локальную плавающую панель — иначе получим ДВА активных
 * подписчика на `sys-log`, что удвоит расход памяти при спаме DHT/net.
 */
async function openInWindow() {
  try {
    await toggleConsoleWindow()
    emit('close')
  } catch {
    /* ok в web-режиме */
  }
}
</script>

<template>
  <div class="sysconsole" :class="{ 'sc-detached': props.detached }" data-testid="system-console">
    <div class="sc-head">
      <span class="sc-title">{{ t('sysconsole.title') }}</span>
      <div class="sc-scopes">
        <button
          v-for="s in ['dht','p2p','net','upd','sys','dapp']"
          :key="s"
          class="sc-chip"
          :class="{ on: active.has(s as any) }"
          :style="active.has(s as any) ? { color: scopeColor(s), borderColor: scopeColor(s) } : {}"
          @click="toggleScope(s as any)"
          @dblclick="onlyScope(s as any)"
          :title="t('sysconsole.scopeTooltip', { scope: s })"
          :data-testid="`sysconsole-scope-${s}`"
        >{{ s.toUpperCase() }}</button>
        <button class="sc-chip all" @click="allScopes" data-testid="sysconsole-scope-all">{{ t('sysconsole.scopeAll') }}</button>
      </div>
      <div class="sc-actions">
        <button class="sc-btn" @click="copyAll" data-testid="sysconsole-copy-btn">
          {{ copied ? t('sysconsole.copied') : t('sysconsole.copyAll') }}
        </button>
        <button class="sc-btn" @click="exportTxt" data-testid="sysconsole-export-btn">
          {{ t('sysconsole.exportTxt') }}
        </button>
        <button class="sc-btn" @click="lines = []" data-testid="sysconsole-clear-btn">{{ t('sysconsole.clear') }}</button>
        <button
          v-if="isTauri && !props.detached"
          class="sc-btn"
          @click="openInWindow"
          :title="t('sysconsole.detachTitle')"
          data-testid="sysconsole-detach-btn"
        >🗗 {{ t('sysconsole.detach') }}</button>
        <button
          v-if="!props.detached"
          class="sc-btn close"
          @click="emit('close')"
          data-testid="sysconsole-close-btn"
        >✕</button>
      </div>
    </div>
    <div class="sc-filter">
      <span class="sc-filter-ic">⌕</span>
      <input
        v-model="query"
        class="sc-filter-input"
        :placeholder="t('sysconsole.filterPlaceholder')"
        data-testid="sysconsole-filter-input"
      />
      <button
        v-if="query"
        class="sc-filter-clear"
        @click="query = ''"
        data-testid="sysconsole-filter-clear"
      >✕</button>
      <span class="sc-count" data-testid="sysconsole-count">{{ visible.length }}</span>
    </div>
    <div ref="bodyRef" class="sc-body" data-testid="sysconsole-body">
      <p v-if="!visible.length" class="sc-empty" data-testid="sysconsole-empty">
        {{ t('sysconsole.empty') }}
      </p>
      <div
        v-for="(l, i) in visible"
        :key="i"
        class="sc-line"
        :class="{ 'sc-line-click': l.scope === 'dapp' && dappRowId(l.msg) }"
        data-testid="sysconsole-line"
        @click="openDetail(l)"
      >
        <span class="sc-ts">{{ fmtTime(l.ts) }}</span>
        <span class="sc-scope" :style="{ color: scopeColor(l.scope) }">[{{ l.scope }}]</span>
        <span
          class="sc-msg"
          :style="l.scope === 'dapp' ? { color: dappLineColor(l.msg) } : {}"
        >{{ l.msg }}</span>
        <span
          v-if="(l.count || 1) > 1"
          class="sc-count-badge"
          :title="t('sysconsole.repeatTitle', { n: l.count })"
          data-testid="sysconsole-line-repeat"
        >×{{ l.count }}</span>
      </div>
    </div>

    <!-- dApp request-detail popup: клик по строке scope='dapp' → показывает
         полные headers / body / curl. Пустой (v-if) когда нет активного. -->
    <div
      v-if="activeDetail"
      class="sc-detail-back"
      @click.self="closeDetail"
      data-testid="sysconsole-detail-modal"
    >
      <div class="sc-detail">
        <header class="sc-detail-head">
          <div class="sc-detail-titles">
            <div class="sc-detail-line1">
              <span
                class="sc-detail-method"
                :style="{ color: dappLineColor(activeDetail.ok ? 'OK ' : 'ERR ') }"
              >{{ activeDetail.method }}</span>
              <span class="sc-detail-status">{{ activeDetail.status || 'n/a' }}</span>
              <span class="sc-detail-ms">{{ activeDetail.ms }}ms</span>
              <span v-if="activeDetail.size" class="sc-detail-size dim">{{ fmtBytes(activeDetail.size) }}</span>
            </div>
            <code class="sc-detail-url" data-testid="sysconsole-detail-url">{{ activeDetail.url }}</code>
          </div>
          <button class="sc-btn close" @click="closeDetail" data-testid="sysconsole-detail-close">✕</button>
        </header>

        <div class="sc-detail-actions">
          <button class="sc-btn" @click="copyDetailField(activeDetail.url, 'URL')" data-testid="sysconsole-detail-copy-url">
            📋 {{ t('sysconsole.detailCopyUrl') }}
          </button>
          <button class="sc-btn" @click="copyDetailField(toCurl(activeDetail), 'curl')" data-testid="sysconsole-detail-copy-curl">
            📋 {{ t('sysconsole.detailCopyCurl') }}
          </button>
        </div>

        <div v-if="activeDetail.err" class="sc-detail-err">⚠ {{ activeDetail.err }}</div>

        <section class="sc-detail-section">
          <h4>{{ t('sysconsole.detailReqHeaders') }}</h4>
          <div v-if="activeDetail.reqHeaders && Object.keys(activeDetail.reqHeaders).length" class="sc-kv">
            <div v-for="(v, k) in activeDetail.reqHeaders" :key="k" class="sc-kv-row">
              <span class="sc-kv-key mono">{{ k }}</span>
              <span class="sc-kv-val mono">{{ v }}</span>
            </div>
          </div>
          <p v-else class="dim">—</p>
        </section>

        <section v-if="activeDetail.reqBody" class="sc-detail-section">
          <h4>{{ t('sysconsole.detailReqBody') }} <span class="dim">· {{ t('sysconsole.detailReqBodyPreview') }}</span></h4>
          <pre class="sc-detail-body mono">{{ activeDetail.reqBody }}</pre>
        </section>

        <section class="sc-detail-section">
          <h4>{{ t('sysconsole.detailRespHeaders') }}</h4>
          <div v-if="activeDetail.respHeaders && Object.keys(activeDetail.respHeaders).length" class="sc-kv">
            <div v-for="(v, k) in activeDetail.respHeaders" :key="k" class="sc-kv-row">
              <span class="sc-kv-key mono">{{ k }}</span>
              <span class="sc-kv-val mono">{{ v }}</span>
            </div>
          </div>
          <p v-else class="dim">—</p>
        </section>
      </div>
    </div>
  </div>
</template>

<style scoped>
.sysconsole {
  position: fixed;
  right: 16px;
  bottom: 16px;
  width: 1080px;
  max-width: calc(100vw - 32px);
  height: 360px;
  display: flex;
  flex-direction: column;
  background: rgba(8, 12, 22, 0.97);
  backdrop-filter: blur(14px);
  border: 1px solid rgba(0, 229, 255, 0.3);
  border-radius: 10px;
  box-shadow: 0 18px 48px rgba(0, 0, 0, 0.55);
  z-index: 60;
  font-family: 'JetBrains Mono', ui-monospace, monospace;
  overflow: hidden;
}
/* Detached-режим (окно `syscon`): растягиваем на весь viewport,
   снимаем плавающий вид, без скругления, без тени. */
.sysconsole.sc-detached {
  right: 0;
  bottom: 0;
  top: 0;
  left: 0;
  width: 100vw;
  max-width: 100vw;
  height: 100vh;
  border: none;
  border-radius: 0;
  box-shadow: none;
  backdrop-filter: none;
  background: #05070d;
}
.sc-head {
  height: 34px;
  min-height: 34px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 12px;
  background: #0d1527;
  border-bottom: 1px solid rgba(0, 229, 255, 0.16);
}
.sc-title {
  font-size: 11px;
  letter-spacing: 0.08em;
  color: #34d3ee;
}
.sc-scopes {
  display: flex;
  align-items: center;
  gap: 4px;
  flex: 1;
  justify-content: center;
}
.sc-chip {
  font-size: 9.5px;
  font-weight: 700;
  letter-spacing: 0.06em;
  color: #4b6377;
  padding: 2px 7px;
  border: 1px solid rgba(120, 150, 170, 0.25);
  border-radius: 4px;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease, opacity 0.15s ease;
  opacity: 0.6;
}
.sc-chip.on {
  opacity: 1;
  background: rgba(255, 255, 255, 0.04);
}
.sc-chip.all {
  color: #8aa6bd;
}
.sc-chip:hover {
  opacity: 1;
}
.sc-sep {
  opacity: 0.5;
}
.sc-actions {
  display: flex;
  align-items: center;
  gap: 6px;
}
.sc-btn {
  font-size: 10px;
  letter-spacing: 0.06em;
  color: #5fd0e6;
  padding: 3px 8px;
  border: 1px solid rgba(0, 229, 255, 0.28);
  border-radius: 4px;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease;
}
.sc-btn:hover {
  color: #d6fbff;
  border-color: rgba(0, 229, 255, 0.6);
  background: rgba(0, 229, 255, 0.08);
}
.sc-btn.on {
  color: #fff;
  border-color: #f59e0b;
  background: rgba(245, 158, 11, 0.14);
}
.sc-btn.close:hover {
  color: #fff;
  border-color: #fb7185;
  background: rgba(251, 113, 133, 0.12);
}
.sc-filter {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  background: #0a1120;
  border-bottom: 1px solid rgba(0, 229, 255, 0.1);
}
.sc-filter-ic {
  color: #4b6377;
  font-size: 13px;
}
.sc-filter-input {
  flex: 1;
  background: none;
  border: none;
  outline: none;
  color: #d6fbff;
  font-size: 11.5px;
  font-family: inherit;
}
.sc-filter-input::placeholder {
  color: #3f5a6e;
}
.sc-filter-clear {
  color: #5fd0e6;
  font-size: 11px;
  padding: 0 6px;
}
.sc-filter-clear:hover {
  color: #fb7185;
}
.sc-count {
  font-size: 10px;
  color: #4b6377;
  min-width: 28px;
  text-align: right;
}
.sc-body {
  flex: 1;
  overflow-y: auto;
  padding: 8px 12px;
  font-size: 11.5px;
  line-height: 1.6;
  user-select: text;
  -webkit-user-select: text;
}
.sc-empty {
  color: #3f5a6e;
  font-style: italic;
}
.sc-line {
  display: flex;
  gap: 8px;
  white-space: pre-wrap;
  word-break: break-word;
}
.sc-ts {
  color: #4b6377;
  flex-shrink: 0;
}
.sc-scope {
  flex-shrink: 0;
  font-weight: 700;
}
.sc-msg {
  color: #c5d6e4;
}
.sc-count-badge {
  flex-shrink: 0;
  align-self: center;
  margin-left: 6px;
  padding: 1px 7px;
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.04em;
  color: #0d1527;
  background: #f59e0b;
  border-radius: 999px;
  line-height: 1.3;
  animation: sc-pulse 0.4s ease-out;
}
@keyframes sc-pulse {
  0% { transform: scale(1.18); background: #fbbf24; }
  100% { transform: scale(1); background: #f59e0b; }
}
</style>
