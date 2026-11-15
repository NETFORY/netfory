<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, onMounted, onBeforeUnmount } from 'vue'
import { marked } from 'marked'
import DOMPurify from 'dompurify'
import { useI18n } from 'vue-i18n'
import { useUiStore } from '@/store/ui'
import { getReadme } from '@/lib/bridge'

const props = defineProps<{ appId: string; title: string }>()
const emit = defineEmits<{ (e: 'close'): void }>()
const { t } = useI18n()
const ui = useUiStore()

const html = ref('')
const loading = ref(true)

onMounted(async () => {
  ui.openOverlay() // hide native child webviews behind the modal (z-order)
  const md = await getReadme(props.appId)
  if (md) {
    const parsed = await marked.parse(md, { breaks: true })
    html.value = DOMPurify.sanitize(parsed)
  }
  loading.value = false
})
onBeforeUnmount(() => ui.closeOverlay())
</script>

<template>
  <div class="about-backdrop" @click.self="emit('close')" data-testid="about-modal">
    <div class="about-card panel">
      <div class="about-head">
        <span class="about-title">// README · {{ title }}</span>
        <button class="about-close" @click="emit('close')" data-testid="about-close">✕</button>
      </div>
      <div class="about-body">
        <p v-if="loading" class="dim">{{ t('common.loading') }}…</p>
        <div v-else-if="html" class="md" v-html="html" data-testid="about-content" />
        <p v-else class="dim" data-testid="about-empty">{{ t('about.none') }}</p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.about-backdrop {
  position: fixed;
  inset: 0;
  z-index: 200;
  background: rgba(0, 0, 0, 0.6);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
}
.about-card {
  width: min(760px, 100%);
  max-height: 84vh;
  display: flex;
  flex-direction: column;
  border-left: 2px solid var(--accent);
}
.about-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 18px;
  border-bottom: 1px solid var(--border);
}
.about-title {
  font-size: 13px;
  letter-spacing: 0.1em;
  color: var(--accent);
  font-weight: var(--fw-700);
  word-break: break-all;
}
.about-close {
  background: none;
  border: none;
  color: var(--text-dim);
  font-size: 16px;
  cursor: pointer;
}
.about-close:hover {
  color: var(--danger);
}
.about-body {
  padding: 18px 22px;
  overflow-y: auto;
}
.md {
  font-size: 13px;
  line-height: 1.7;
  color: var(--text);
}
.md :deep(h1),
.md :deep(h2),
.md :deep(h3) {
  color: var(--accent);
  margin: 18px 0 8px;
  line-height: 1.3;
}
.md :deep(h1) {
  font-size: 20px;
}
.md :deep(h2) {
  font-size: 16px;
}
.md :deep(p) {
  margin: 8px 0;
}
.md :deep(ul),
.md :deep(ol) {
  padding-left: 22px;
  margin: 8px 0;
}
.md :deep(code) {
  background: var(--bg-elevated);
  padding: 2px 6px;
  border-radius: 4px;
  font-size: 12px;
}
.md :deep(pre) {
  background: var(--bg-elevated);
  padding: 12px 14px;
  border-radius: var(--radius);
  overflow-x: auto;
  border: 1px solid var(--border);
}
.md :deep(pre code) {
  background: none;
  padding: 0;
}
.md :deep(a) {
  color: var(--accent);
  text-decoration: underline;
}
.md :deep(img) {
  max-width: 100%;
  border-radius: var(--radius);
}
.md :deep(blockquote) {
  border-left: 3px solid var(--secondary);
  padding-left: 12px;
  color: var(--text-dim);
  margin: 8px 0;
}
.md :deep(table) {
  border-collapse: collapse;
  width: 100%;
}
.md :deep(th),
.md :deep(td) {
  border: 1px solid var(--border);
  padding: 6px 10px;
  font-size: 12px;
}
</style>
