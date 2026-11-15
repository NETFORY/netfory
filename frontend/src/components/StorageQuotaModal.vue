<script setup lang="ts">
import { computed, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useDiscoveryStore } from '@/store/discovery'
import { useUiStore } from '@/store/ui'

const { t } = useI18n()
const discovery = useDiscoveryStore()
const ui = useUiStore()

const open = computed(() => !!discovery.storagePrompt)
const neededMb = computed(() =>
  discovery.storagePrompt ? Math.ceil(discovery.storagePrompt.neededBytes / (1024 * 1024)) : 0,
)

// keep native child webviews hidden behind the modal (z-order fix)
watch(open, (v) => (v ? ui.openOverlay() : ui.closeOverlay()))

function confirm() {
  discovery.confirmStorageIncrease()
}
function cancel() {
  discovery.cancelStorageIncrease()
}
</script>

<template>
  <div v-if="open" class="quota-overlay" data-testid="storage-quota-modal">
    <div class="quota-card panel">
      <span class="quota-title">{{ t('storage.quotaTitle') }}</span>
      <p class="quota-msg">{{ t('storage.quotaNeed', { mb: neededMb }) }}</p>
      <div class="quota-actions">
        <button class="btn btn-primary" @click="confirm" data-testid="storage-quota-confirm">
          {{ t('storage.confirm') }}
        </button>
        <button class="btn" @click="cancel" data-testid="storage-quota-cancel">
          {{ t('storage.cancel') }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.quota-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.75);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 120;
  animation: fade 0.15s ease both;
}
.quota-card {
  width: 100%;
  max-width: 420px;
  padding: 24px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  border: 1px solid var(--accent);
}
.quota-title {
  font-size: 12px;
  letter-spacing: 0.12em;
  color: var(--accent);
  text-transform: uppercase;
  font-weight: var(--fw-700);
}
.quota-msg {
  font-size: 13px;
  line-height: 1.6;
  color: var(--text);
}
.quota-actions {
  display: flex;
  gap: 12px;
}
@keyframes fade {
  from {
    opacity: 0;
  }
  to {
    opacity: 1;
  }
}
</style>
