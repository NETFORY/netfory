<script setup lang="ts">
// sn://ai-computer — AI Computer (Netfory AI Pool, Phase 1).
// Тумблер opt-in участия + карточка железа + сетевые AI Stats.
import { onMounted, onBeforeUnmount } from 'vue'
import { useAuthStore } from '@/store/auth'
import { useAiPoolStore } from '@/store/aiPool'
import AiHardwareCard from '@/components/ai/AiHardwareCard.vue'
import AiNetworkStats from '@/components/ai/AiNetworkStats.vue'
import AiPipelines from '@/components/ai/AiPipelines.vue'
import AiModelCatalog from '@/components/ai/AiModelCatalog.vue'
import AiNodesList from '@/components/ai/AiNodesList.vue'
import AiChatConsole from '@/components/ai/AiChatConsole.vue'

const auth = useAuthStore()
const pool = useAiPoolStore()

onMounted(() => {
  if (auth.isTauri) pool.startPolling()
})
onBeforeUnmount(() => pool.stopPolling())
</script>

<template>
  <div class="ai-page" data-testid="ai-computer-page">
    <header class="head">
      <div>
        <h1>{{ $t('ai.title') }}</h1>
        <p class="sub">{{ $t('ai.subtitle') }}</p>
      </div>
      <label
        class="pool-toggle"
        :class="{ on: pool.enabled, disabled: pool.busy || !auth.isTauri }"
        data-testid="ai-pool-toggle"
      >
        <input
          type="checkbox"
          :checked="pool.enabled"
          :disabled="pool.busy || !auth.isTauri"
          @change="pool.toggle()"
          data-testid="ai-pool-toggle-input"
        />
        <span class="track"><span class="thumb"></span></span>
        <span class="toggle-label">{{ $t('ai.enableToggle') }}</span>
      </label>
    </header>

    <div v-if="!auth.isTauri" class="notice" data-testid="ai-web-only-notice">
      {{ $t('ai.webOnly') }}
    </div>

    <template v-else>
      <AiHardwareCard :hw="pool.hw" :status="pool.status" />
      <AiNetworkStats :net="pool.net" :failed="pool.netFailed" />
      <AiPipelines :net="pool.net" />
      <AiModelCatalog :net="pool.net" />
      <AiNodesList :net="pool.net" />
      <AiChatConsole />
      <p class="hint">{{ $t('ai.hbHint') }}</p>
    </template>
  </div>
</template>

<style scoped>
.ai-page {
  max-width: 860px;
  margin: 0 auto;
  padding: 28px 24px;
}
.head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
  margin-bottom: 22px;
  flex-wrap: wrap;
}
h1 {
  font-size: 22px;
  letter-spacing: 3px;
  color: #22d3ee;
}
.sub {
  margin-top: 8px;
  font-size: 13px;
  color: #64748b;
  max-width: 480px;
  line-height: 1.5;
}
.notice {
  text-align: center;
  padding: 50px 0;
  color: #64748b;
}
.hint {
  margin-top: 14px;
  font-size: 11.5px;
  color: #475569;
  line-height: 1.5;
}

/* ---- тумблер AI Pool ---- */
.pool-toggle {
  display: inline-flex;
  align-items: center;
  gap: 11px;
  cursor: pointer;
  user-select: none;
  flex-shrink: 0;
  padding: 10px 14px;
  border: 1px solid rgba(148, 163, 184, 0.18);
  border-radius: 12px;
  background: rgba(15, 23, 32, 0.5);
  transition: border-color 0.2s, background-color 0.2s;
}
.pool-toggle.on {
  border-color: rgba(52, 211, 153, 0.45);
  background: rgba(52, 211, 153, 0.07);
}
.pool-toggle.disabled {
  opacity: 0.55;
  cursor: default;
}
.pool-toggle input {
  display: none;
}
.track {
  width: 40px;
  height: 22px;
  border-radius: 999px;
  background: rgba(100, 116, 139, 0.45);
  position: relative;
  transition: background-color 0.2s;
  flex-shrink: 0;
}
.pool-toggle.on .track {
  background: #34d399;
}
.thumb {
  position: absolute;
  top: 3px;
  left: 3px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: #e2e8f0;
  transition: transform 0.2s;
}
.pool-toggle.on .thumb {
  transform: translateX(18px);
}
.toggle-label {
  font-size: 12.5px;
  font-weight: 700;
  letter-spacing: 0.8px;
  color: #e2e8f0;
}
</style>
