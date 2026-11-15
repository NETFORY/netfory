<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount, watch, computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  apiFetch,
  type ApiFetchResult,
  dappWsOpen,
  dappWsSend,
  dappWsClose,
  onDappWsMessage,
  onDappWsClose,
} from '@/lib/bridge'

const route = useRoute()
const router = useRouter()

// --- Режим: HTTP-запрос или WS-соединение (v1.55.31) -----------------------
type Mode = 'http' | 'ws'
const mode = ref<Mode>('http')

const url = ref('')

// --- HTTP-ветка ------------------------------------------------------------
const loading = ref(false)
const error = ref('')
const result = ref<ApiFetchResult | null>(null)

async function runHttp() {
  const u = url.value.trim()
  if (!u) return
  loading.value = true
  error.value = ''
  result.value = null
  try {
    result.value = await apiFetch(u)
  } catch (e: any) {
    error.value = typeof e === 'string' ? e : e?.message || String(e)
  } finally {
    loading.value = false
  }
}

// --- WS-ветка --------------------------------------------------------------
type WsPhase = 'idle' | 'connecting' | 'open' | 'closed' | 'error'
interface WsLog {
  ts: number
  dir: 'in' | 'out' | 'sys'
  kind: string
  data: string
}

const wsPhase = ref<WsPhase>('idle')
const wsStreamId = ref<string | null>(null)
const wsStatus = ref<number | null>(null)
const wsError = ref('')
const wsLog = ref<WsLog[]>([])
const wsSendText = ref('')

const WS_HANDSHAKE_TIMEOUT_MS = 8000
let handshakeTimer: number | null = null
let unlistenMsg: (() => void) | null = null
let unlistenClose: (() => void) | null = null

function pushLog(l: Omit<WsLog, 'ts'>) {
  wsLog.value.push({ ts: Date.now(), ...l })
  if (wsLog.value.length > 500) wsLog.value.splice(0, wsLog.value.length - 500)
}

function normalizeToWs(input: string): string {
  const s = input.trim()
  if (s.startsWith('api://')) return 'ws://' + s.slice('api://'.length)
  if (s.startsWith('ws://') || s.startsWith('wss://')) return s
  // допускаем ввод без схемы: считаем это api://
  return 'ws://' + s.replace(/^\/*/, '')
}

async function cleanupWsListeners() {
  try { unlistenMsg?.() } catch { /* noop */ }
  try { unlistenClose?.() } catch { /* noop */ }
  unlistenMsg = unlistenClose = null
}

function clearHandshakeTimer() {
  if (handshakeTimer !== null) {
    window.clearTimeout(handshakeTimer)
    handshakeTimer = null
  }
}

async function runWs() {
  const raw = url.value.trim()
  if (!raw) return
  // сброс предыдущей сессии
  if (wsStreamId.value) {
    await dappWsClose(wsStreamId.value, 1000, 'reconnect').catch(() => {})
  }
  await cleanupWsListeners()
  clearHandshakeTimer()

  wsPhase.value = 'connecting'
  wsError.value = ''
  wsStreamId.value = null
  wsStatus.value = null
  wsLog.value = []

  const wsUrl = normalizeToWs(raw)
  pushLog({ dir: 'sys', kind: 'info', data: `→ dapp_ws_open ${wsUrl}` })

  // 8-сек таймаут хендшейка (Rust не resolve'нет invoke)
  handshakeTimer = window.setTimeout(async () => {
    if (wsPhase.value === 'connecting') {
      wsPhase.value = 'error'
      wsError.value = 'WS handshake timeout (8s)'
      pushLog({ dir: 'sys', kind: 'error', data: 'handshake timeout (8s)' })
      if (wsStreamId.value) {
        await dappWsClose(wsStreamId.value, 1001, 'handshake timeout').catch(() => {})
      }
      await cleanupWsListeners()
    }
  }, WS_HANDSHAKE_TIMEOUT_MS) as unknown as number

  // v1.55.32: буферизуем ВСЕ входящие события до resolve invoke.
  // Это защищает от race: reader-task Rust'а может эмитнуть первый фрейм
  // (напр. Socket.IO OPEN) ДО того как invoke Promise доедет до JS.
  interface BufferedEvent { t: 'm' | 'c'; p: { streamId: string; [k: string]: any } }
  const pendingEvents: BufferedEvent[] = []
  let mySid: string | null = null

  const deliverMessage = (p: { kind: 'text' | 'binary'; data: string }) => {
    let text = p.data
    if (p.kind === 'binary') {
      try { text = `<binary ${atob(p.data).length} bytes>` } catch { text = '<binary>' }
    }
    pushLog({ dir: 'in', kind: p.kind, data: text })
  }

  unlistenMsg = await onDappWsMessage((p) => {
    if (mySid === null) { pendingEvents.push({ t: 'm', p: p as any }); return }
    if (p.streamId !== mySid) return
    deliverMessage(p)
  })
  unlistenClose = await onDappWsClose((p) => {
    if (mySid === null) { pendingEvents.push({ t: 'c', p: p as any }); return }
    if (p.streamId !== mySid) return
    clearHandshakeTimer()
    wsPhase.value = 'closed'
    pushLog({ dir: 'sys', kind: 'close', data: `close(${p.code}) ${p.reason}` })
    void cleanupWsListeners()
  })

  try {
    const { streamId, status } = await dappWsOpen(wsUrl, {})
    wsStreamId.value = streamId
    mySid = streamId
    wsStatus.value = status
    clearHandshakeTimer()
    wsPhase.value = 'open'
    pushLog({ dir: 'sys', kind: 'info', data: `streamId=${streamId.slice(0, 8)}…` })
    pushLog({ dir: 'sys', kind: 'open', data: `handshake ok (HTTP ${status})` })
    // Дренируем буфер: доставляем все накопившиеся события с нашим streamId.
    for (const ev of pendingEvents) {
      if (ev.p.streamId !== streamId) continue
      if (ev.t === 'm') deliverMessage(ev.p as any)
      else {
        wsPhase.value = 'closed'
        pushLog({ dir: 'sys', kind: 'close', data: `close(${ev.p.code}) ${ev.p.reason}` })
        void cleanupWsListeners()
        break
      }
    }
  } catch (e: any) {
    clearHandshakeTimer()
    wsPhase.value = 'error'
    wsError.value = typeof e === 'string' ? e : e?.message || String(e)
    pushLog({ dir: 'sys', kind: 'error', data: wsError.value })
    await cleanupWsListeners()
  }
}

async function sendWs() {
  if (wsPhase.value !== 'open' || !wsStreamId.value) return
  const text = wsSendText.value
  if (!text) return
  try {
    await dappWsSend(wsStreamId.value, 'text', text)
    pushLog({ dir: 'out', kind: 'text', data: text })
    wsSendText.value = ''
  } catch (e: any) {
    pushLog({ dir: 'sys', kind: 'error', data: `send failed: ${e?.message || e}` })
  }
}

async function closeWs() {
  if (!wsStreamId.value) return
  await dappWsClose(wsStreamId.value, 1000, 'user close').catch(() => {})
}

// --- Общее: запуск при клике / URL query ------------------------------------
async function run() {
  const u = (route.query.url as string) || url.value
  const m = (route.query.mode as string) === 'ws' ? 'ws' : 'http'
  if (!u) return
  url.value = u
  mode.value = m as Mode
  if (m === 'ws') await runWs()
  else await runHttp()
}

function submit() {
  router.push({ name: 'api-view', query: { url: url.value, mode: mode.value } })
}

function shortId(id: string) {
  return id.length > 22 ? `${id.slice(0, 12)}…${id.slice(-6)}` : id
}

const copied = ref(false)
async function copyBody() {
  if (!result.value) return
  try {
    await navigator.clipboard.writeText(result.value.body)
    copied.value = true
    setTimeout(() => (copied.value = false), 1500)
  } catch { /* clipboard unavailable */ }
}

const wsPhaseLabel = computed(() => {
  switch (wsPhase.value) {
    case 'idle': return 'ожидание'
    case 'connecting': return 'подключение…'
    case 'open': return `онлайн (HTTP ${wsStatus.value ?? '?'})`
    case 'closed': return 'закрыто'
    case 'error': return 'ошибка'
  }
  return ''
})

function fmtTs(ts: number) {
  const d = new Date(ts)
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}:${String(d.getSeconds()).padStart(2, '0')}.${String(d.getMilliseconds()).padStart(3, '0')}`
}

onMounted(run)
watch(() => route.query.url, run)
watch(() => route.query.mode, run)

onBeforeUnmount(async () => {
  clearHandshakeTimer()
  if (wsStreamId.value) {
    await dappWsClose(wsStreamId.value, 1000, 'view unmount').catch(() => {})
  }
  await cleanupWsListeners()
})
</script>

<template>
  <div class="api-view" data-testid="api-view">
    <header class="av-head">
      <div class="av-mode" data-testid="api-view-mode">
        <button
          class="av-mode-btn"
          :class="{ active: mode === 'http' }"
          @click="mode = 'http'"
          data-testid="api-view-mode-http"
        >api://</button>
        <button
          class="av-mode-btn"
          :class="{ active: mode === 'ws' }"
          @click="mode = 'ws'"
          data-testid="api-view-mode-ws"
        >ws://</button>
      </div>
      <input
        v-model="url"
        class="av-url"
        spellcheck="false"
        :placeholder="mode === 'ws' ? 'api://<nodeId>/<provider>/<path>  или  ws://…' : 'api://<nodeId>/<provider>/<path>'"
        @keydown.enter="submit"
        data-testid="api-view-url"
      />
      <button
        class="av-go"
        :disabled="loading || wsPhase === 'connecting'"
        @click="submit"
        data-testid="api-view-go"
      >
        {{ (loading || wsPhase === 'connecting') ? '…' : mode === 'ws' ? '⇋ Открыть' : '↵ Запрос' }}
      </button>
    </header>

    <p class="av-hint">
      <template v-if="mode === 'http'">
        Формат: <code>api://&lt;nodeId&gt;/&lt;provider&gt;/&lt;path&gt;</code> — запрос идёт к узлу
        блокчейна через P2P-прокси (netfory). Ответ подписан провайдером и проверяется по NodeID.
      </template>
      <template v-else>
        Тестер WS-туннеля <code>netfory/api/1</code> поверх QUIC. Хендшейк-таймаут — <b>8 сек</b>.
        Принимаются схемы <code>api://</code> (нормализуется в <code>ws://</code>), <code>ws://</code>, <code>wss://</code>.
      </template>
    </p>

    <!-- ================== HTTP-ветка =================================== -->
    <template v-if="mode === 'http'">
      <div v-if="loading" class="av-state" data-testid="api-view-loading">
        <span class="spin" /> Подключение к провайдеру по P2P…
      </div>

      <div v-else-if="error" class="av-error" data-testid="api-view-error">
        <b>Ошибка:</b> {{ error }}
      </div>

      <template v-else-if="result">
        <div class="av-meta" data-testid="api-view-meta">
          <span class="badge" :class="result.verified ? 'ok' : 'bad'" data-testid="api-view-verified">
            {{ result.verified ? '✓ Подпись верна' : '⚠ Подпись НЕ проверена' }}
          </span>
          <span class="badge dim">HTTP {{ result.status }}</span>
          <span class="badge dim">провайдер: {{ result.target }}</span>
          <span class="badge dim" :title="result.providerNodeId">node: {{ shortId(result.providerNodeId) }}</span>
          <span v-if="result.relayed" class="badge relay">↻ через реле</span>
          <button class="av-copy" @click="copyBody" data-testid="api-view-copy">
            {{ copied ? '✓ Скопировано' : '⧉ Копировать JSON' }}
          </button>
        </div>

        <pre class="av-json" data-testid="api-view-json">{{ result.body }}</pre>
      </template>
    </template>

    <!-- ================== WS-ветка ====================================== -->
    <template v-else>
      <div class="av-meta" data-testid="api-view-ws-meta">
        <span
          class="badge"
          :class="{
            ok: wsPhase === 'open',
            bad: wsPhase === 'error' || wsPhase === 'closed',
            dim: wsPhase === 'idle',
            relay: wsPhase === 'connecting',
          }"
          data-testid="api-view-ws-phase"
        >{{ wsPhaseLabel }}</span>
        <span v-if="wsStreamId" class="badge dim" data-testid="api-view-ws-stream">
          stream: {{ wsStreamId.slice(0, 8) }}…
        </span>
        <button
          v-if="wsPhase === 'open'"
          class="av-copy"
          @click="closeWs"
          data-testid="api-view-ws-close"
        >⏻ Закрыть</button>
      </div>

      <div v-if="wsError" class="av-error" data-testid="api-view-ws-error">
        <b>Ошибка:</b> {{ wsError }}
      </div>

      <div class="av-ws-log" data-testid="api-view-ws-log">
        <div v-if="wsLog.length === 0" class="av-ws-empty">
          нет событий — нажмите «Открыть», чтобы установить WS
        </div>
        <div
          v-for="(l, i) in wsLog"
          :key="i"
          class="av-ws-row"
          :class="'dir-' + l.dir"
        >
          <span class="av-ws-ts">{{ fmtTs(l.ts) }}</span>
          <span class="av-ws-arrow">{{ l.dir === 'in' ? '←' : l.dir === 'out' ? '→' : '·' }}</span>
          <span class="av-ws-kind">{{ l.kind }}</span>
          <span class="av-ws-data">{{ l.data }}</span>
        </div>
      </div>

      <div class="av-ws-send">
        <input
          v-model="wsSendText"
          class="av-url"
          spellcheck="false"
          placeholder="текстовый фрейм для отправки"
          :disabled="wsPhase !== 'open'"
          @keydown.enter="sendWs"
          data-testid="api-view-ws-send-input"
        />
        <button
          class="av-go"
          :disabled="wsPhase !== 'open' || !wsSendText"
          @click="sendWs"
          data-testid="api-view-ws-send-btn"
        >↵ Send</button>
      </div>
    </template>
  </div>
</template>

<style scoped>
.api-view {
  padding: 24px 28px;
  max-width: 1000px;
  margin: 0 auto;
  font-family: var(--font-mono, ui-monospace, monospace);
}
.av-head {
  display: flex;
  align-items: center;
  gap: 8px;
}
.av-mode {
  display: inline-flex;
  border: 1px solid var(--border, #1e2733);
  border-radius: 8px;
  overflow: hidden;
}
.av-mode-btn {
  background: transparent;
  border: 0;
  color: var(--text-dim, #64748b);
  font-family: inherit;
  font-size: 12px;
  padding: 9px 12px;
  cursor: pointer;
}
.av-mode-btn.active {
  color: var(--accent, #34d399);
  background: rgba(52, 211, 153, 0.08);
}
.av-url {
  flex: 1;
  background: var(--bg-elevated, #121821);
  border: 1px solid var(--border, #1e2733);
  border-radius: 8px;
  padding: 10px 12px;
  color: var(--text, #cbd5e1);
  font-size: 13px;
  outline: none;
}
.av-url:focus {
  border-color: var(--border-accent, #34d39955);
}
.av-go {
  padding: 10px 14px;
  border-radius: 8px;
  border: 1px solid var(--border-accent, #34d39955);
  background: transparent;
  color: var(--accent, #34d399);
  cursor: pointer;
  white-space: nowrap;
}
.av-go:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.av-hint {
  color: var(--text-dim, #64748b);
  font-size: 12px;
  margin: 14px 0 18px;
}
.av-hint code {
  color: var(--text, #cbd5e1);
}
.av-state {
  color: var(--text-dim, #64748b);
  padding: 28px 0;
}
.spin {
  display: inline-block;
  width: 12px;
  height: 12px;
  border: 2px solid var(--border, #1e2733);
  border-top-color: var(--accent, #34d399);
  border-radius: 50%;
  animation: sp 0.7s linear infinite;
  vertical-align: middle;
  margin-right: 8px;
}
@keyframes sp {
  to { transform: rotate(360deg); }
}
.av-error {
  background: rgba(248, 113, 113, 0.08);
  border: 1px solid #f8717155;
  color: #fca5a5;
  border-radius: 8px;
  padding: 14px 16px;
  font-size: 13px;
}
.av-meta {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
}
.badge {
  font-size: 11px;
  padding: 4px 10px;
  border-radius: 999px;
  border: 1px solid var(--border, #1e2733);
}
.badge.ok {
  color: #34d399;
  border-color: #34d39955;
  background: rgba(52, 211, 153, 0.08);
}
.badge.bad {
  color: #fb923c;
  border-color: #fb923c55;
  background: rgba(251, 146, 60, 0.08);
}
.badge.relay {
  color: #60a5fa;
  border-color: #60a5fa55;
}
.badge.dim {
  color: var(--text-dim, #64748b);
}
.av-copy {
  margin-left: auto;
  font-size: 11px;
  padding: 5px 10px;
  border-radius: 999px;
  border: 1px solid var(--border, #1e2733);
  background: transparent;
  color: var(--text-dim, #64748b);
  cursor: pointer;
}
.av-json {
  background: var(--bg-elevated, #0d1117);
  border: 1px solid var(--border, #1e2733);
  border-left: 2px solid var(--accent, #34d399);
  border-radius: 8px;
  padding: 16px 18px;
  color: #a5d6ff;
  font-size: 13px;
  line-height: 1.6;
  overflow: auto;
  white-space: pre-wrap;
  word-break: break-word;
}

/* ------- WS-лог ---------- */
.av-ws-log {
  background: var(--bg-elevated, #0d1117);
  border: 1px solid var(--border, #1e2733);
  border-left: 2px solid var(--accent, #34d399);
  border-radius: 8px;
  padding: 12px 14px;
  font-size: 12px;
  line-height: 1.55;
  max-height: 380px;
  overflow: auto;
}
.av-ws-empty {
  color: var(--text-dim, #64748b);
  font-style: italic;
}
.av-ws-row {
  display: grid;
  grid-template-columns: 100px 20px 70px 1fr;
  gap: 8px;
  padding: 2px 0;
  border-bottom: 1px dashed rgba(148, 163, 184, 0.08);
  word-break: break-all;
}
.av-ws-ts { color: var(--text-dim, #64748b); }
.av-ws-arrow { text-align: center; }
.av-ws-kind { color: #94a3b8; }
.dir-in .av-ws-arrow, .dir-in .av-ws-data { color: #a5d6ff; }
.dir-out .av-ws-arrow, .dir-out .av-ws-data { color: #34d399; }
.dir-sys .av-ws-arrow, .dir-sys .av-ws-data { color: #fbbf24; }

.av-ws-send {
  display: flex;
  gap: 8px;
  margin-top: 12px;
}
</style>
