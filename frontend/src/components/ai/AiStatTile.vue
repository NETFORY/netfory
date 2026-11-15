<script setup lang="ts">
/* eslint-disable no-undef */
// Универсальная плитка-метрика (значение + подпись), переиспользуемая
// и в карточке устройства, и в сетевом дашборде AI Stats.
defineProps<{
  label: string
  value: string
  unit?: string
  accent?: boolean
  testid?: string
}>()
</script>

<template>
  <div class="tile" :class="{ accent }" :data-testid="testid || 'ai-stat-tile'">
    <div class="tile-value">
      {{ value }}<span v-if="unit" class="tile-unit">{{ unit }}</span>
    </div>
    <div class="tile-label">{{ label }}</div>
  </div>
</template>

<style scoped>
.tile {
  border: 1px solid rgba(148, 163, 184, 0.14);
  border-radius: 11px;
  background: rgba(15, 23, 32, 0.5);
  padding: 14px 16px;
  min-width: 0;
  transition: border-color 0.2s;
}
.tile:hover {
  border-color: rgba(34, 211, 238, 0.35);
}
.tile.accent {
  border-color: rgba(34, 211, 238, 0.35);
  background: rgba(34, 211, 238, 0.06);
}
.tile-value {
  font-family: ui-monospace, monospace;
  font-size: 20px;
  font-weight: 700;
  color: #e2e8f0;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.tile.accent .tile-value {
  color: #22d3ee;
}
.tile-unit {
  font-size: 12px;
  font-weight: 500;
  color: #64748b;
  margin-left: 5px;
}
.tile-label {
  margin-top: 5px;
  font-size: 11px;
  letter-spacing: 0.8px;
  text-transform: uppercase;
  color: #64748b;
}
</style>
