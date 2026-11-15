<script setup lang="ts">
/* eslint-disable no-undef */
// Просмотр .md прямо в чате (Telegram-style): тёмная модалка, код-блоки с
// кнопкой «копировать», цитаты с боковой полосой, таблицы. Без записи на диск.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Marked } from 'marked'
import DOMPurify from 'dompurify'

const props = defineProps<{ name: string; text: string }>()
const emit = defineEmits<{ (e: 'close'): void }>()
const { t } = useI18n()

const copied = ref('')
let copyTimer: number | null = null

function esc(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

// свой экземпляр Marked: не трогаем глобальный renderer других экранов
const md = new Marked({
  breaks: true,
  renderer: {
    code({ text, lang }: { text: string; lang?: string }) {
      const label = (lang || '').trim()
      return (
        `<div class="mdv-codewrap"><div class="mdv-codehead" role="button" tabindex="0">` +
        `<span class="mdv-codelang">${esc(label) || 'code'}</span>` +
        `<span class="mdv-copy">⧉ ${esc(t('messenger.mdCopy'))}</span>` +
        `</div><pre class="mdv-pre"><code>${esc(text)}</code></pre></div>`
      )
    },
  },
})

const html = computed(() => {
  const raw = md.parse(props.text || '', { async: false }) as string
  return DOMPurify.sanitize(raw, { ADD_ATTR: ['role', 'tabindex'] })
})

// делегированный клик: шапка код-блока копирует содержимое соседнего <pre>
async function onBodyClick(e: MouseEvent) {
  const head = (e.target as HTMLElement).closest('.mdv-codehead')
  if (!head) return
  const pre = head.parentElement?.querySelector('pre')
  if (!pre) return
  try {
    await navigator.clipboard.writeText(pre.textContent || '')
    copied.value = t('messenger.mdCopied')
    if (copyTimer) clearTimeout(copyTimer)
    copyTimer = window.setTimeout(() => (copied.value = ''), 1600)
  } catch {}
}
</script>

<template>
  <div class="mdv-overlay" @click.self="emit('close')" data-testid="md-viewer-modal">
    <div class="mdv-modal">
      <div class="mdv-head">
        <span class="mdv-title" :title="name">{{ name }}</span>
        <button class="mdv-close" @click="emit('close')" data-testid="md-viewer-close" :title="t('messenger.mdClose')">✕</button>
      </div>
      <div class="mdv-body md" v-html="html" @click="onBodyClick" data-testid="md-viewer-body" />
      <transition name="mdv-fade">
        <div v-if="copied" class="mdv-toast" data-testid="md-viewer-copied">{{ copied }}</div>
      </transition>
    </div>
  </div>
</template>

<style scoped>
.mdv-overlay {
  position: fixed;
  inset: 0;
  z-index: 260;
  background: rgba(8, 12, 18, 0.62);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px 14px;
}
.mdv-modal {
  position: relative;
  width: min(780px, 96vw);
  max-height: 90vh;
  display: flex;
  flex-direction: column;
  background: #1c2530;
  border: 1px solid rgba(122, 160, 200, 0.14);
  border-radius: 12px;
  box-shadow: 0 22px 60px rgba(0, 0, 0, 0.55);
  overflow: hidden;
}
.mdv-head {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 16px;
  border-bottom: 1px solid rgba(122, 160, 200, 0.12);
  background: #202b38;
}
.mdv-title {
  flex: 1;
  font-weight: 600;
  font-size: 0.95rem;
  color: #e8eef5;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.mdv-close {
  border: 0;
  background: transparent;
  color: #8fa5bb;
  font-size: 1rem;
  cursor: pointer;
  padding: 4px 8px;
  border-radius: 6px;
  transition: background-color 0.15s ease, color 0.15s ease;
}
.mdv-close:hover {
  background: rgba(255, 255, 255, 0.07);
  color: #fff;
}
.mdv-body {
  overflow-y: auto;
  padding: 18px 22px 26px;
  color: #dbe4ee;
  font-size: 0.92rem;
  line-height: 1.6;
  word-break: break-word;
}
.mdv-toast {
  position: absolute;
  bottom: 16px;
  left: 50%;
  transform: translateX(-50%);
  background: rgba(15, 21, 28, 0.92);
  color: #cfe3f7;
  border: 1px solid rgba(122, 160, 200, 0.2);
  padding: 7px 16px;
  border-radius: 20px;
  font-size: 0.82rem;
}
.mdv-fade-enter-active,
.mdv-fade-leave-active {
  transition: opacity 0.2s ease;
}
.mdv-fade-enter-from,
.mdv-fade-leave-to {
  opacity: 0;
}

/* ---- разметка markdown (Telegram-подобная) ---- */
.md :deep(h1) {
  font-size: 1.65rem;
  line-height: 1.25;
  margin: 0.4em 0 0.55em;
  color: #f2f6fa;
  font-weight: 700;
}
.md :deep(h2) {
  font-size: 1.3rem;
  margin: 1.1em 0 0.5em;
  color: #eef3f8;
  font-weight: 700;
}
.md :deep(h3) {
  font-size: 1.08rem;
  margin: 1em 0 0.4em;
  color: #e8eef5;
  font-weight: 600;
}
.md :deep(p) {
  margin: 0.5em 0;
}
.md :deep(a) {
  color: #5eb1f0;
  text-decoration: none;
}
.md :deep(a:hover) {
  text-decoration: underline;
}
.md :deep(ul),
.md :deep(ol) {
  margin: 0.5em 0;
  padding-left: 1.4em;
}
.md :deep(li) {
  margin: 0.28em 0;
}
.md :deep(blockquote) {
  margin: 0.7em 0;
  padding: 8px 14px;
  border-left: 3px solid #4ea1f3;
  background: rgba(78, 161, 243, 0.09);
  border-radius: 6px;
  color: #c9d8e6;
}
.md :deep(blockquote p) {
  margin: 0.25em 0;
}
.md :deep(code) {
  font-family: 'JetBrains Mono', 'Fira Code', Consolas, monospace;
  font-size: 0.85em;
  color: #7cc0ff;
  background: rgba(110, 168, 254, 0.11);
  padding: 1px 5px;
  border-radius: 4px;
}
.md :deep(.mdv-codewrap) {
  margin: 0.8em 0;
  border-radius: 8px;
  overflow: hidden;
  background: #141b24;
  border: 1px solid rgba(122, 160, 200, 0.1);
}
.md :deep(.mdv-codehead) {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 5px 12px;
  background: #10161e;
  color: #6f89a3;
  font-size: 0.74rem;
  cursor: pointer;
  user-select: none;
  transition: color 0.15s ease;
}
.md :deep(.mdv-codehead:hover) {
  color: #9fc4e8;
}
.md :deep(.mdv-copy) {
  color: #4ea1f3;
}
.md :deep(.mdv-pre) {
  margin: 0;
  padding: 12px 14px;
  overflow-x: auto;
}
.md :deep(.mdv-pre code) {
  background: none;
  padding: 0;
  color: #cfe3f7;
  font-size: 0.82rem;
  line-height: 1.55;
}
.md :deep(table) {
  border-collapse: collapse;
  margin: 0.8em 0;
  width: 100%;
  font-size: 0.86rem;
}
.md :deep(th),
.md :deep(td) {
  border: 1px solid rgba(122, 160, 200, 0.16);
  padding: 7px 10px;
  text-align: left;
}
.md :deep(th) {
  background: rgba(78, 161, 243, 0.1);
  color: #e8eef5;
}
.md :deep(hr) {
  border: 0;
  border-top: 1px solid rgba(122, 160, 200, 0.16);
  margin: 1.2em 0;
}
.md :deep(img) {
  max-width: 100%;
  border-radius: 8px;
}
</style>
