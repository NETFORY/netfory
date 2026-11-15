<script setup lang="ts">
// AI Чат Консоль (DEV-only): многоходовый диалог со стримингом ответа прямо
// из децентрализованной AI-сети. Контекст (вся переписка) уходит одним
// промптом — префикс-кэш шардов делает повторный prefill мгновенным.
// Видна только адресам из .env DEVELOPERS.
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  isTauri,
  isDeveloper,
  aiConsoleGenerate,
  aiConsoleCancel,
  appListen,
  saveFile,
} from '@/lib/bridge'
import { useWalletStore } from '@/store/wallet'
import { useAiPoolStore } from '@/store/aiPool'
import { Marked } from 'marked'
import DOMPurify from 'dompurify'
import hljs from 'highlight.js/lib/common'
import 'highlight.js/styles/github-dark-dimmed.css'

// свой экземпляр Marked — не трогаем глобальный (WallFeed/AboutModal);
// подсветка кода: marked v13+ передаёт в renderer токен {text, lang}
const md = new Marked({
  breaks: true,
  renderer: {
    code({ text, lang }: { text: string; lang?: string }) {
      const l = lang && hljs.getLanguage(lang) ? lang : undefined
      const out = l
        ? hljs.highlight(text, { language: l }).value
        : hljs.highlightAuto(text).value
      return `<pre class="md-code"><code class="hljs">${out}</code></pre>`
    },
  },
})

/** markdown ответа → безопасный HTML (подсветка кода, списки, таблицы) */
function renderMd(text: string): string {
  if (!text) return ''
  const raw = md.parse(text, { async: false }) as string
  return DOMPurify.sanitize(raw)
}

const wallet = useWalletStore()
const pool = useAiPoolStore()
const { t } = useI18n()
const visible = computed(() => isTauri && isDeveloper(wallet.mainAddress))
/** загруженные модели сети — для выпадающего списка */
const netModels = computed(() => Object.keys(pool.net?.models ?? {}).sort())

interface ChatMsg {
  role: 'user' | 'assistant'
  content: string
}

const prompt = ref('')
const model = ref('')
const system = ref(localStorage.getItem('sth_ai_console_system') || '')
const showSystem = ref(false)
watch(system, (v) => {
  try {
    localStorage.setItem('sth_ai_console_system', v)
  } catch {
    /* квота — не критично */
  }
})
const messages = ref<ChatMsg[]>([])

/** «думающие» модели (Qwen3.x): вкл/выкл блок рассуждений <think>.
 * По умолчанию ВКЛ — не вмешиваемся в промпт (thinking-only модели вроде
 * Qwen3.8 ломаются от подстановки пустого <think>). */
const think = ref(localStorage.getItem('sth_ai_console_think') !== '0')
watch(think, (v) => {
  try {
    localStorage.setItem('sth_ai_console_think', v ? '1' : '0')
  } catch {
    /* квота — не критично */
  }
})

/** разобрать ответ на рассуждения <think>…</think> и сам ответ */
function splitThink(s: string): { think: string; answer: string; live: boolean } {
  const start = s.indexOf('<think>')
  if (start === -1) return { think: '', answer: s, live: false }
  const end = s.indexOf('</think>')
  if (end === -1) return { think: s.slice(start + 7).trim(), answer: '', live: true }
  return {
    think: s.slice(start + 7, end).trim(),
    answer: (s.slice(0, start) + s.slice(end + 8)).trim(),
    live: false,
  }
}

// --- пресеты системных промптов: встроенные роли + свои (localStorage) ---
interface SysPreset {
  name: string
  text: string
}
const PRESETS_KEY = 'sth_ai_console_sys_presets'
const customPresets = ref<SysPreset[]>([])
try {
  customPresets.value = JSON.parse(localStorage.getItem(PRESETS_KEY) || '[]')
} catch {
  customPresets.value = []
}
const builtinPresets = computed<SysPreset[]>(() => [
  { name: t('ai.presetTranslator'), text: t('ai.presetTranslatorText') },
  { name: t('ai.presetCoder'), text: t('ai.presetCoderText') },
  { name: t('ai.presetSummary'), text: t('ai.presetSummaryText') },
])
function persistPresets() {
  try {
    localStorage.setItem(PRESETS_KEY, JSON.stringify(customPresets.value))
  } catch {
    /* квота — не критично */
  }
}
function applyPreset(p: SysPreset) {
  system.value = p.text
}
function savePreset() {
  const text = system.value.trim()
  if (!text) return
  const name = text.length > 24 ? text.slice(0, 24) + '…' : text
  customPresets.value = customPresets.value.filter((p) => p.text !== text)
  customPresets.value.unshift({ name, text })
  customPresets.value = customPresets.value.slice(0, 8)
  persistPresets()
}
function removePreset(i: number) {
  customPresets.value.splice(i, 1)
  persistPresets()
}
const busy = ref(false)
const error = ref('')
const meta = ref<{ node: string; ms: number } | null>(null)
/** живой индикатор скорости: чанки ≈ токены, обновляется на каждом чанке */
const speed = ref<{ toks: number; rate: number } | null>(null)
let genT0 = 0
const chatEl = ref<HTMLElement | null>(null)
let sid = ''
let unlisten: (() => void) | null = null

// автопрокрутка чата при стриминге
watch(
  () => messages.value.map((m) => m.content.length).join(','),
  () => {
    void nextTick(() => {
      if (chatEl.value) chatEl.value.scrollTop = chatEl.value.scrollHeight
    })
  },
)

// --- история консоли (localStorage, последние 10 обменов) ---
interface HistoryEntry {
  prompt: string
  model: string
  text: string
  node: string
  ms: number
  at: number
}
const HISTORY_KEY = 'sth_ai_console_history'
const HISTORY_MAX = 10
const history = ref<HistoryEntry[]>([])
const showHistory = ref(false)
try {
  history.value = JSON.parse(localStorage.getItem(HISTORY_KEY) || '[]')
} catch {
  history.value = []
}
function persistHistory() {
  try {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(history.value))
  } catch {
    /* квота — не критично */
  }
}
function saveEntry(userMsg: string, text: string, node: string, ms: number) {
  history.value.unshift({
    prompt: userMsg,
    model: model.value.trim(),
    text,
    node,
    ms,
    at: Date.now(),
  })
  history.value = history.value.slice(0, HISTORY_MAX)
  persistHistory()
}
function restoreEntry(h: HistoryEntry) {
  if (busy.value) return
  messages.value = [
    { role: 'user', content: h.prompt },
    { role: 'assistant', content: h.text },
  ]
  model.value = h.model
  meta.value = { node: h.node, ms: h.ms }
  error.value = ''
  showHistory.value = false
}
function removeEntry(i: number) {
  history.value.splice(i, 1)
  persistHistory()
}
function clearHistory() {
  history.value = []
  persistHistory()
}
function fmtAt(ts: number): string {
  return new Date(ts).toLocaleString([], {
    day: '2-digit',
    month: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

function newChat() {
  if (busy.value) return
  messages.value = []
  meta.value = null
  error.value = ''
}

/** экспорт всей переписки в markdown-файл (нативный Save As в Tauri) */
async function exportChat() {
  if (!messages.value.length) return
  const header = [
    `# NETFORY AI Console — ${new Date().toLocaleString()}`,
    model.value.trim() ? `Model: \`${model.value.trim()}\`` : '',
    meta.value ? `Node: \`${meta.value.node}\` · ${meta.value.ms} ms` : '',
  ].filter(Boolean)
  const body = messages.value.map(
    (m) => `**${m.role === 'user' ? 'User' : 'Assistant'}:**\n\n${m.content}\n`,
  )
  const name = `ai-chat-${new Date().toISOString().slice(0, 19).replace(/[T:]/g, '-')}.md`
  try {
    await saveFile(name, [...header, '', ...body].join('\n'), 'text/markdown')
  } catch (e: any) {
    error.value = String(e?.message ?? e)
  }
}

/** транскрипт диалога одним промптом — стабильное начало = хит префикс-кэша.
 *  Системный промпт идёт первым: его kv прогревается на шардах один раз. */
function buildTranscript(): string {
  const sys = system.value.trim()
  return (
    (sys ? 'System: ' + sys + '\n' : '') +
    messages.value
      .map((m) =>
        m.role === 'user'
          ? 'User: ' + m.content
          // рассуждения <think> в контекст не тащим — только сам ответ
          : 'Assistant: ' + (splitThink(m.content).answer || m.content),
      )
      .join('\n') + '\nAssistant:'
  )
}

async function send() {
  const p = prompt.value.trim()
  if (!p || busy.value) return
  busy.value = true
  error.value = ''
  meta.value = null
  messages.value.push({ role: 'user', content: p })
  const transcript = buildTranscript()
  messages.value.push({ role: 'assistant', content: '' })
  const idx = messages.value.length - 1
  prompt.value = ''
  sid = 'console-' + Date.now().toString(36) + Math.random().toString(36).slice(2, 6)
  unlisten = await appListen<{ streamId: string; t: string }>('dapp-ai-token', (m) => {
    if (m?.streamId === sid) {
      messages.value[idx].content += m.t || ''
      const toks = (speed.value?.toks ?? 0) + 1
      const dt = Math.max(0.2, (Date.now() - genT0) / 1000)
      speed.value = { toks, rate: Math.round((toks / dt) * 10) / 10 }
    }
  })
  speed.value = null
  genT0 = Date.now()
  try {
    const r = await aiConsoleGenerate(sid, transcript, model.value.trim() || undefined, 2048, think.value)
    let text = r.text || messages.value[idx].content
    // модель иногда продолжает транскрипт за пользователя — обрезаем
    const cut = text.indexOf('\nUser:')
    if (cut > 0) text = text.slice(0, cut)
    messages.value[idx].content = text.trim()
    meta.value = { node: r.node, ms: r.ms }
    saveEntry(p, messages.value[idx].content, r.node, r.ms)
  } catch (e: any) {
    error.value = String(e?.message ?? e)
    if (!messages.value[idx].content) messages.value.splice(idx, 1)
  } finally {
    unlisten?.()
    unlisten = null
    busy.value = false
  }
}

function stop() {
  if (busy.value && sid) void aiConsoleCancel(sid)
}

onBeforeUnmount(() => {
  unlisten?.()
  if (busy.value && sid) void aiConsoleCancel(sid)
})
</script>

<template>
  <section v-if="visible" class="console-card" data-testid="ai-console">
    <div class="card-head">
      <h2>{{ $t('ai.consoleTitle') }}</h2>
      <span class="dev-badge">DEV</span>
      <button
        v-if="messages.length"
        class="new-chat"
        :disabled="busy"
        data-testid="ai-console-new-chat"
        @click="newChat"
      >
        {{ $t('ai.consoleNewChat') }}
      </button>
      <button
        v-if="messages.length"
        class="export-btn"
        data-testid="ai-console-export"
        @click="exportChat"
      >
        {{ $t('ai.consoleExport') }}
      </button>
      <button
        class="think-toggle"
        :class="{ active: think }"
        :disabled="busy"
        data-testid="ai-console-think-toggle"
        :title="$t('ai.consoleThinkHint')"
        @click="think = !think"
      >
        {{ $t('ai.consoleThink') }}
      </button>
      <button
        class="sys-toggle"
        :class="{ active: showSystem || system.trim() }"
        data-testid="ai-console-system-toggle"
        :title="$t('ai.consoleSystemBtn')"
        @click="showSystem = !showSystem"
      >
        SYS
      </button>
      <button
        class="hist-toggle"
        :class="{ active: showHistory }"
        data-testid="ai-console-history-toggle"
        @click="showHistory = !showHistory"
      >
        {{ $t('ai.historyTitle') }} ({{ history.length }})
      </button>
    </div>

    <div v-if="showSystem" class="sys-wrap">
      <textarea
        v-model="system"
        class="sys-input"
        rows="2"
        :placeholder="$t('ai.consoleSystem')"
        :disabled="busy"
        data-testid="ai-console-system"
      />
      <div class="preset-row" data-testid="ai-console-presets">
        <button
          v-for="(p, i) in builtinPresets"
          :key="p.name"
          class="preset-chip"
          :class="{ active: system.trim() === p.text }"
          :data-testid="`ai-console-preset-${i}`"
          :disabled="busy"
          @click="applyPreset(p)"
        >
          {{ p.name }}
        </button>
        <button
          v-for="(p, i) in customPresets"
          :key="'c' + p.text"
          class="preset-chip custom"
          :class="{ active: system.trim() === p.text }"
          :data-testid="`ai-console-preset-custom-${i}`"
          :disabled="busy"
          @click="applyPreset(p)"
        >
          {{ p.name }}
          <span
            class="chip-del"
            :data-testid="`ai-console-preset-del-${i}`"
            @click.stop="removePreset(i)"
          >×</span>
        </button>
        <button
          class="preset-save"
          :disabled="busy || !system.trim()"
          data-testid="ai-console-preset-save"
          @click="savePreset"
        >
          + {{ $t('ai.presetSave') }}
        </button>
      </div>
    </div>

    <div v-if="showHistory" class="hist-panel" data-testid="ai-console-history">
      <div v-if="!history.length" class="hist-empty">{{ $t('ai.historyEmpty') }}</div>
      <template v-else>
        <div
          v-for="(h, i) in history"
          :key="h.at"
          class="hist-row"
          :data-testid="`ai-console-history-item-${i}`"
          @click="restoreEntry(h)"
        >
          <span class="hist-at">{{ fmtAt(h.at) }}</span>
          <span class="hist-prompt">{{ h.prompt }}</span>
          <span v-if="h.model" class="hist-model">{{ h.model }}</span>
          <button
            class="hist-del"
            :data-testid="`ai-console-history-del-${i}`"
            @click.stop="removeEntry(i)"
          >
            ×
          </button>
        </div>
        <button class="hist-clear" data-testid="ai-console-history-clear" @click="clearHistory">
          {{ $t('ai.historyClear') }}
        </button>
      </template>
    </div>

    <div v-if="messages.length" ref="chatEl" class="chat" data-testid="ai-console-chat">
      <div
        v-for="(m, i) in messages"
        :key="i"
        class="bubble"
        :class="m.role"
        :data-testid="`ai-console-msg-${i}`"
      >
        <template v-if="m.role === 'assistant' && splitThink(m.content).think">
          <details class="think-block" :open="splitThink(m.content).live" :data-testid="`ai-console-think-${i}`">
            <summary>
              {{ splitThink(m.content).live ? $t('ai.thinkLive') : $t('ai.thinkDone') }}
            </summary>
            <pre class="think-text">{{ splitThink(m.content).think }}</pre>
          </details>
          <div class="bubble-md" :data-testid="`ai-console-md-${i}`" v-html="renderMd(splitThink(m.content).answer)" />
          <span v-if="busy && i === messages.length - 1" class="cursor">▌</span>
        </template>
        <template v-else-if="m.role === 'assistant'">
          <div class="bubble-md" :data-testid="`ai-console-md-${i}`" v-html="renderMd(m.content)" />
          <span v-if="busy && i === messages.length - 1" class="cursor">▌</span>
        </template>
        <pre v-else class="bubble-text">{{ m.content }}</pre>
      </div>
    </div>

    <textarea
      v-model="prompt"
      class="prompt-input"
      rows="3"
      :placeholder="$t('ai.consolePrompt')"
      :disabled="busy"
      data-testid="ai-console-prompt"
      @keydown.ctrl.enter.prevent="send"
    />

    <div class="controls">
      <select
        v-model="model"
        class="model-input"
        :disabled="busy"
        data-testid="ai-console-model"
      >
        <option value="">{{ $t('ai.consoleModelAuto') }}</option>
        <option v-for="m in netModels" :key="m" :value="m">{{ m }}</option>
      </select>
      <button
        v-if="!busy"
        class="btn send"
        :disabled="!prompt.trim()"
        data-testid="ai-console-send"
        @click="send"
      >
        {{ $t('ai.consoleSend') }}
      </button>
      <button v-else class="btn stop" data-testid="ai-console-stop" @click="stop">
        {{ $t('ai.consoleStop') }}
      </button>
    </div>

    <div v-if="error" class="err" data-testid="ai-console-error">{{ error }}</div>
    <div v-if="speed && busy" class="meta speed-badge" data-testid="ai-console-speed">
      ⚡ {{ speed.rate }} tok/s · {{ speed.toks }} ток
    </div>
    <div v-if="meta" class="meta" data-testid="ai-console-meta">
      {{ meta.node }} · {{ meta.ms }} ms<span v-if="speed"> · ⚡ {{ speed.rate }} tok/s ({{ speed.toks }} ток)</span>
    </div>

    <p class="hint">{{ $t('ai.consoleHint') }}</p>
  </section>
</template>

<style scoped>
.speed-badge {
  color: #22d3ee;
  animation: speedpulse 1.2s ease-in-out infinite;
}
@keyframes speedpulse {
  0%,
  100% {
    opacity: 1;
  }
  50% {
    opacity: 0.55;
  }
}
.console-card {
  margin-top: 14px;
  border: 1px solid rgba(34, 211, 238, 0.2);
  border-radius: 13px;
  background: rgba(10, 16, 24, 0.55);
  padding: 18px 20px;
}
.card-head {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 12px;
}
h2 {
  font-size: 13px;
  letter-spacing: 2px;
  color: #94a3b8;
}
.dev-badge {
  font-size: 9.5px;
  font-weight: 800;
  letter-spacing: 1.5px;
  color: #0a1018;
  background: #22d3ee;
  border-radius: 4px;
  padding: 2px 7px;
}
.new-chat {
  margin-left: auto;
  background: none;
  border: 1px solid rgba(52, 211, 153, 0.35);
  border-radius: 999px;
  color: #34d399;
  font-size: 11px;
  padding: 4px 12px;
  cursor: pointer;
  transition: background-color 0.15s;
}
.new-chat:hover {
  background: rgba(52, 211, 153, 0.1);
}
.new-chat:disabled {
  opacity: 0.4;
  cursor: default;
}
.export-btn {
  background: none;
  border: 1px solid rgba(34, 211, 238, 0.35);
  border-radius: 999px;
  color: #22d3ee;
  font-size: 11px;
  padding: 4px 12px;
  cursor: pointer;
  transition: background-color 0.15s;
}
.export-btn:hover {
  background: rgba(34, 211, 238, 0.1);
}
.hist-toggle {
  margin-left: auto;
  background: none;
  border: 1px solid rgba(148, 163, 184, 0.25);
  border-radius: 999px;
  color: #94a3b8;
  font-size: 11px;
  padding: 4px 12px;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s;
}
.new-chat ~ .hist-toggle {
  margin-left: 0;
}
.sys-toggle {
  margin-left: auto;
  background: none;
  border: 1px solid rgba(148, 163, 184, 0.25);
  border-radius: 999px;
  color: #94a3b8;
  font-size: 10px;
  font-weight: 800;
  letter-spacing: 1px;
  padding: 4px 10px;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s;
}
.sys-toggle.active,
.sys-toggle:hover {
  color: #fbbf24;
  border-color: rgba(251, 191, 36, 0.45);
}
.think-toggle {
  margin-left: auto;
  background: none;
  border: 1px solid rgba(148, 163, 184, 0.25);
  border-radius: 999px;
  color: #94a3b8;
  font-size: 10px;
  font-weight: 800;
  letter-spacing: 1px;
  padding: 4px 10px;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s;
}
.think-toggle.active,
.think-toggle:hover {
  color: #a78bfa;
  border-color: rgba(167, 139, 250, 0.5);
}
.new-chat ~ .think-toggle,
.think-toggle ~ .sys-toggle {
  margin-left: 0;
}
.think-block {
  margin-bottom: 6px;
  border: 1px dashed rgba(167, 139, 250, 0.3);
  border-radius: 8px;
  background: rgba(167, 139, 250, 0.05);
  padding: 4px 8px;
}
.think-block summary {
  cursor: pointer;
  font-size: 10.5px;
  letter-spacing: 0.5px;
  color: #a78bfa;
  user-select: none;
}
.think-text {
  margin: 6px 0 2px;
  white-space: pre-wrap;
  word-break: break-word;
  font-family: ui-monospace, monospace;
  font-size: 11px;
  line-height: 1.55;
  color: #8b9bb4;
}
.new-chat ~ .sys-toggle,
.sys-toggle ~ .hist-toggle {
  margin-left: 0;
}
.sys-input {
  width: 100%;
  resize: vertical;
  min-height: 40px;
  margin-bottom: 10px;
  font-size: 12px;
  line-height: 1.5;
  color: #fbbf24;
  background: rgba(2, 8, 16, 0.6);
  border: 1px solid rgba(251, 191, 36, 0.25);
  border-radius: 10px;
  padding: 8px 12px;
  outline: none;
  transition: border-color 0.15s;
}
.sys-input:focus {
  border-color: rgba(251, 191, 36, 0.5);
}
.sys-wrap {
  margin-bottom: 10px;
}
.sys-wrap .sys-input {
  margin-bottom: 6px;
}
.preset-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.preset-chip {
  background: none;
  border: 1px solid rgba(148, 163, 184, 0.25);
  border-radius: 999px;
  color: #94a3b8;
  font-size: 10.5px;
  padding: 3px 10px;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s;
}
.preset-chip:hover,
.preset-chip.active {
  color: #fbbf24;
  border-color: rgba(251, 191, 36, 0.45);
}
.preset-chip.custom {
  color: #a5b4fc;
}
.preset-chip.custom:hover,
.preset-chip.custom.active {
  color: #fbbf24;
}
.chip-del {
  margin-left: 5px;
  color: #64748b;
}
.chip-del:hover {
  color: #f87171;
}
.preset-save {
  background: none;
  border: 1px dashed rgba(148, 163, 184, 0.35);
  border-radius: 999px;
  color: #64748b;
  font-size: 10.5px;
  padding: 3px 10px;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s;
}
.preset-save:hover:not(:disabled) {
  color: #34d399;
  border-color: rgba(52, 211, 153, 0.4);
}
.preset-save:disabled {
  opacity: 0.4;
  cursor: default;
}
.hist-toggle:hover,
.hist-toggle.active {
  color: #22d3ee;
  border-color: rgba(34, 211, 238, 0.45);
}
.hist-panel {
  margin-bottom: 12px;
  border: 1px solid rgba(148, 163, 184, 0.14);
  border-radius: 10px;
  background: rgba(2, 8, 16, 0.5);
  padding: 8px;
}
.hist-empty {
  font-size: 11.5px;
  color: #64748b;
  text-align: center;
  padding: 10px 0;
}
.hist-row {
  display: flex;
  align-items: center;
  gap: 8px;
  border-radius: 7px;
  padding: 6px 8px;
  cursor: pointer;
  transition: background-color 0.12s;
}
.hist-row:hover {
  background: rgba(34, 211, 238, 0.07);
}
.hist-at {
  font-family: ui-monospace, monospace;
  font-size: 10px;
  color: #64748b;
  flex-shrink: 0;
}
.hist-prompt {
  flex: 1;
  font-size: 11.5px;
  color: #e2e8f0;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.hist-model {
  font-family: ui-monospace, monospace;
  font-size: 10px;
  color: #34d399;
  flex-shrink: 0;
}
.hist-del {
  background: none;
  border: none;
  color: #64748b;
  font-size: 14px;
  cursor: pointer;
  padding: 0 4px;
  line-height: 1;
}
.hist-del:hover {
  color: #f87171;
}
.hist-clear {
  display: block;
  margin: 6px auto 2px;
  background: none;
  border: none;
  color: #64748b;
  font-size: 10.5px;
  letter-spacing: 0.5px;
  text-transform: uppercase;
  cursor: pointer;
}
.hist-clear:hover {
  color: #f87171;
}
.chat {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-bottom: 12px;
  max-height: 420px;
  overflow-y: auto;
  padding: 4px 2px;
}
.bubble {
  max-width: 86%;
  border-radius: 12px;
  padding: 8px 12px;
}
.bubble.user {
  align-self: flex-end;
  background: rgba(34, 211, 238, 0.1);
  border: 1px solid rgba(34, 211, 238, 0.25);
}
.bubble.assistant {
  align-self: flex-start;
  background: rgba(52, 211, 153, 0.06);
  border: 1px solid rgba(52, 211, 153, 0.2);
}
.bubble-text {
  margin: 0;
  white-space: pre-wrap;
  word-break: break-word;
  font-family: ui-monospace, monospace;
  font-size: 12.5px;
  line-height: 1.6;
  color: #e2e8f0;
}
/* markdown-ответ: стили через :deep — v-html вне scoped-области */
.bubble-md {
  font-size: 12.5px;
  line-height: 1.65;
  color: #e2e8f0;
  word-break: break-word;
}
.bubble-md :deep(p) {
  margin: 0 0 8px;
}
.bubble-md :deep(p:last-child) {
  margin-bottom: 0;
}
.bubble-md :deep(h1),
.bubble-md :deep(h2),
.bubble-md :deep(h3),
.bubble-md :deep(h4) {
  margin: 10px 0 6px;
  font-size: 13.5px;
  color: #a5f3fc;
}
.bubble-md :deep(ul),
.bubble-md :deep(ol) {
  margin: 4px 0 8px;
  padding-left: 20px;
}
.bubble-md :deep(li) {
  margin: 2px 0;
}
.bubble-md :deep(code) {
  font-family: ui-monospace, monospace;
  font-size: 11.5px;
  background: rgba(34, 211, 238, 0.09);
  border-radius: 4px;
  padding: 1px 5px;
  color: #a5f3fc;
}
.bubble-md :deep(pre.md-code) {
  margin: 6px 0 10px;
  padding: 10px 12px;
  border: 1px solid rgba(148, 163, 184, 0.15);
  border-radius: 9px;
  background: rgba(2, 8, 16, 0.75);
  overflow-x: auto;
}
.bubble-md :deep(pre.md-code code) {
  background: none;
  padding: 0;
  color: inherit;
  font-size: 11.5px;
  line-height: 1.55;
}
.bubble-md :deep(blockquote) {
  margin: 6px 0;
  padding: 2px 10px;
  border-left: 3px solid rgba(167, 139, 250, 0.5);
  color: #94a3b8;
}
.bubble-md :deep(table) {
  border-collapse: collapse;
  margin: 6px 0 10px;
  font-size: 11.5px;
}
.bubble-md :deep(th),
.bubble-md :deep(td) {
  border: 1px solid rgba(148, 163, 184, 0.25);
  padding: 4px 9px;
}
.bubble-md :deep(a) {
  color: #22d3ee;
}
.prompt-input {
  width: 100%;
  resize: vertical;
  min-height: 64px;
  font-size: 13px;
  line-height: 1.5;
  color: #e2e8f0;
  background: rgba(2, 8, 16, 0.6);
  border: 1px solid rgba(148, 163, 184, 0.18);
  border-radius: 10px;
  padding: 10px 12px;
  outline: none;
  transition: border-color 0.15s;
}
.prompt-input:focus {
  border-color: rgba(34, 211, 238, 0.45);
}
.controls {
  margin-top: 10px;
  display: flex;
  gap: 10px;
  align-items: center;
}
.model-input {
  flex: 1;
  max-width: 240px;
  font-family: ui-monospace, monospace;
  font-size: 12px;
  color: #e2e8f0;
  background: rgba(2, 8, 16, 0.6);
  border: 1px solid rgba(148, 163, 184, 0.18);
  border-radius: 999px;
  padding: 7px 14px;
  outline: none;
  appearance: none;
  cursor: pointer;
}
.model-input option {
  background: #0a1018;
  color: #e2e8f0;
}
.btn {
  border: none;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.6px;
  padding: 8px 22px;
  cursor: pointer;
  transition: opacity 0.15s, transform 0.15s;
}
.btn:hover {
  transform: translateY(-1px);
}
.btn:disabled {
  opacity: 0.4;
  cursor: default;
  transform: none;
}
.send {
  background: #22d3ee;
  color: #0a1018;
}
.stop {
  background: rgba(248, 113, 113, 0.15);
  color: #f87171;
  border: 1px solid rgba(248, 113, 113, 0.4);
}
.cursor {
  color: #34d399;
  animation: blink 1s steps(1) infinite;
}
@keyframes blink {
  50% {
    opacity: 0;
  }
}
.err {
  margin-top: 8px;
  font-size: 12px;
  color: #f87171;
}
.meta {
  margin-top: 6px;
  font-family: ui-monospace, monospace;
  font-size: 10.5px;
  color: #64748b;
}
.hint {
  margin-top: 10px;
  font-size: 10.5px;
  color: #475569;
  line-height: 1.5;
}
</style>
