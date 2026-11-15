<script setup lang="ts">
/* eslint-disable no-undef */
// Карточка «Моё устройство»: GPU / VRAM / TFLOPS / CPU / RAM + статус подключения.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { AiHwSnapshot } from '@/lib/bridge'
import { gbFromMb } from '@/lib/aiFormat'
import AiStatTile from './AiStatTile.vue'

const props = defineProps<{
  hw: AiHwSnapshot | null
  status: 'disabled' | 'connected' | 'syncing'
}>()

const { t } = useI18n()

const statusText = computed(() => {
  if (props.status === 'connected') return t('ai.statusConnected')
  if (props.status === 'syncing') return t('ai.statusSyncing')
  return t('ai.statusDisabled')
})
</script>

<template>
  <section class="hw-card" data-testid="ai-hardware-card">
    <div class="card-head">
      <h2>{{ $t('ai.localHw') }}</h2>
      <span class="status-pill" :class="status" data-testid="ai-connection-status">
        <span class="dot"></span>{{ statusText }}
      </span>
    </div>

    <div class="gpu-line" data-testid="ai-gpu-model">
      <span class="gpu-tag">{{ $t('ai.gpu') }}</span>
      <b v-if="hw?.gpu" class="gpu-name">{{ hw.gpu.model }}</b>
      <span v-else class="gpu-none">{{ $t('ai.noGpu') }}</span>
    </div>

    <div class="tiles">
      <template v-if="hw?.gpu">
        <AiStatTile
          :label="$t('ai.vramTotal')"
          :value="gbFromMb(hw.gpu.vramTotalMb)"
          unit="GB"
          testid="ai-vram-total"
        />
        <AiStatTile
          :label="$t('ai.vramFree')"
          :value="gbFromMb(hw.gpu.vramFreeMb)"
          unit="GB"
          testid="ai-vram-free"
        />
        <AiStatTile
          :label="$t('ai.tflops')"
          :value="hw.gpu.tflopsFp16.toFixed(1)"
          unit="TFLOPS"
          accent
          testid="ai-tflops"
        />
      </template>
      <AiStatTile
        :label="$t('ai.threads')"
        :value="String(hw?.cpu.threads ?? '—')"
        testid="ai-cpu-threads"
      />
      <AiStatTile
        :label="$t('ai.ramTotal')"
        :value="hw ? gbFromMb(hw.cpu.ramTotalMb) : '—'"
        unit="GB"
        testid="ai-ram-total"
      />
      <AiStatTile
        :label="$t('ai.ramFree')"
        :value="hw ? gbFromMb(hw.cpu.ramFreeMb) : '—'"
        unit="GB"
        testid="ai-ram-free"
      />
    </div>
  </section>
</template>

<style scoped>
.hw-card {
  border: 1px solid rgba(148, 163, 184, 0.14);
  border-radius: 13px;
  background: rgba(10, 16, 24, 0.55);
  padding: 18px 20px;
  margin-bottom: 18px;
}
.card-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  margin-bottom: 14px;
}
h2 {
  font-size: 13px;
  letter-spacing: 2px;
  color: #94a3b8;
}
.status-pill {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  font-size: 11.5px;
  padding: 5px 11px;
  border-radius: 999px;
  border: 1px solid rgba(148, 163, 184, 0.2);
  color: #94a3b8;
}
.status-pill .dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #64748b;
}
.status-pill.connected {
  border-color: rgba(52, 211, 153, 0.4);
  color: #34d399;
}
.status-pill.connected .dot {
  background: #34d399;
  box-shadow: 0 0 7px rgba(52, 211, 153, 0.7);
}
.status-pill.syncing {
  border-color: rgba(251, 191, 36, 0.4);
  color: #fbbf24;
}
.status-pill.syncing .dot {
  background: #fbbf24;
  animation: pulse 1.2s ease-in-out infinite;
}
@keyframes pulse {
  50% {
    opacity: 0.3;
  }
}
.gpu-line {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 14px;
  min-width: 0;
}
.gpu-tag {
  font-size: 10px;
  letter-spacing: 1.5px;
  color: #22d3ee;
  border: 1px solid rgba(34, 211, 238, 0.35);
  border-radius: 6px;
  padding: 3px 8px;
  flex-shrink: 0;
}
.gpu-name {
  font-family: ui-monospace, monospace;
  font-size: 14px;
  color: #e2e8f0;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.gpu-none {
  font-size: 12.5px;
  color: #64748b;
}
.tiles {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
  gap: 10px;
}
</style>
