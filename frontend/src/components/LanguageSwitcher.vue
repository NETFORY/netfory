<script setup lang="ts">
import { ref, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useUiStore, type LocaleId } from '@/store/ui'

const ui = useUiStore()
const { locale } = useI18n()
const open = ref(false)

const langs: { id: LocaleId; label: string }[] = [
  { id: 'ru', label: 'РУССКИЙ' },
  { id: 'en', label: 'ENGLISH' },
  { id: 'id', label: 'INDONESIA' },
]

const current = computed(() => (ui.locale === 'ru' ? 'RU' : ui.locale === 'id' ? 'ID' : 'EN'))

function pick(id: LocaleId) {
  ui.setLocale(id)
  locale.value = id
  open.value = false
}
</script>

<template>
  <div class="lang" data-testid="language-switcher">
    <button class="lang-trigger" @click="open = !open" data-testid="language-trigger">
      <span class="globe">⌖</span>
      <span class="code">{{ current }}</span>
      <span class="caret" :class="{ up: open }">▾</span>
    </button>
    <div v-if="open" class="lang-menu" data-testid="language-menu">
      <button
        v-for="l in langs"
        :key="l.id"
        class="lang-item"
        :class="{ active: ui.locale === l.id }"
        @click="pick(l.id)"
        :data-testid="`language-option-${l.id}`"
      >
        <span>{{ l.label }}</span>
        <span v-if="ui.locale === l.id" class="hl">●</span>
      </button>
    </div>
    <div v-if="open" class="lang-backdrop" @click="open = false" />
  </div>
</template>

<style scoped>
.lang {
  position: relative;
}
.lang-trigger {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 7px 11px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--panel-2);
  font-size: 11px;
  letter-spacing: 0.08em;
  transition: border-color 0.18s ease, color 0.18s ease;
}
.lang-trigger:hover {
  border-color: var(--border-accent);
  color: var(--accent);
}
.globe {
  color: var(--accent);
}
.code {
  font-weight: var(--fw-700);
}
.caret {
  font-size: 9px;
  transition: transform 0.18s ease;
}
.caret.up {
  transform: rotate(180deg);
}
.lang-menu {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  min-width: 150px;
  background: var(--panel);
  border: 1px solid var(--border-accent);
  border-radius: var(--radius);
  z-index: 60;
  box-shadow: var(--shadow);
  animation: fade 0.14s ease both;
}
.lang-item {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 12px;
  font-size: 11px;
  letter-spacing: 0.1em;
  border-bottom: 1px solid var(--border);
  transition: background-color 0.14s ease, color 0.14s ease;
}
.lang-item:last-child {
  border-bottom: none;
}
.lang-item:hover {
  background: var(--accent-soft);
  color: var(--accent);
}
.lang-item.active {
  color: var(--accent);
}
.lang-backdrop {
  position: fixed;
  inset: 0;
  z-index: 50;
}
</style>
