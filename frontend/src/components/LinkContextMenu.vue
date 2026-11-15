<script setup lang="ts">
// Контекстное меню внешней http(s)-ссылки в Vue-интерфейсе (ПКМ по <a href>):
// открыть во вкладке NETFORY или в системном браузере.
import { defineEmits, defineProps } from 'vue'
import { useI18n } from 'vue-i18n'

defineProps<{ x: number; y: number; url: string }>()
const emit = defineEmits<{ (e: 'tab'): void; (e: 'system'): void; (e: 'close'): void }>()
const { t } = useI18n()
</script>

<template>
  <div class="ctx-backdrop" @click="emit('close')" @contextmenu.prevent="emit('close')" data-testid="link-ctx-backdrop">
    <div class="ctx-menu" :style="{ left: x + 'px', top: y + 'px' }" @click.stop data-testid="link-context-menu">
      <div class="ctx-url" :title="url">{{ url }}</div>
      <button class="ctx-item" @click="emit('tab')" data-testid="link-ctx-open-tab">{{ t('web.linkOpenTab') }}</button>
      <button class="ctx-item" @click="emit('system')" data-testid="link-ctx-open-system">{{ t('web.linkOpenSystem') }}</button>
    </div>
  </div>
</template>

<style scoped>
.ctx-backdrop { position: fixed; inset: 0; z-index: 1100; }
.ctx-menu {
  position: fixed;
  min-width: 220px;
  max-width: 360px;
  background: light-dark(var(--panel), #0a0f16);
  border: 1px solid rgba(34, 211, 238, 0.3);
  border-radius: 10px;
  padding: 6px;
  box-shadow: 0 12px 40px rgba(3, 6, 10, 0.7);
}
.ctx-url {
  padding: 6px 12px 8px;
  font-size: 11px;
  color: var(--muted, #6b7f93);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  border-bottom: 1px solid rgba(34, 211, 238, 0.15);
  margin-bottom: 4px;
}
.ctx-item {
  display: block;
  width: 100%;
  text-align: left;
  background: transparent;
  border: none;
  color: var(--text);
  padding: 9px 12px;
  border-radius: 7px;
  font-size: 13px;
  cursor: pointer;
  transition: background-color 0.15s, color 0.15s;
}
.ctx-item:hover { background: rgba(34, 211, 238, 0.12); color: light-dark(#0e7490, #22d3ee); }
</style>
