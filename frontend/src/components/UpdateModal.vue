<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useUpdater } from '@/lib/updater'

const { t } = useI18n()
const { state, installNow, redownload, dismissModal } = useUpdater()

const version = computed(() => state.info?.version || '')
const notes = computed(() => state.manifest?.notes || '')
const canInstall = computed(() => !!state.installerPath)
</script>

<template>
  <div v-if="state.showModal" class="um-overlay" data-testid="update-modal">
    <div class="um-card">
      <header class="um-head">
        <span class="um-tag">{{ t('updater.modalTag') }}</span>
        <button class="um-x" @click="dismissModal" data-testid="update-modal-close">✕</button>
      </header>

      <div class="um-body">
        <div class="um-icon">⇧</div>
        <h3 class="um-title">{{ t('updater.modalTitle') }}</h3>
        <p class="um-ver" data-testid="update-modal-version">
          SmartNet <b>v{{ version }}</b>
        </p>
        <p v-if="notes" class="um-notes">{{ notes }}</p>
        <p v-if="!canInstall" class="um-warn" data-testid="update-modal-noinstaller">
          {{ t('updater.modalNoInstaller') }}
        </p>
        <p v-if="!canInstall && state.error" class="um-diag" data-testid="update-modal-diag">
          {{ state.error }}
        </p>
        <button
          class="um-redl"
          :class="{ hot: !canInstall }"
          :title="t('updater.redownloadHint')"
          @click="redownload"
          data-testid="update-redownload-btn"
        >
          ↻ {{ t('updater.redownload') }}
        </button>
      </div>

      <footer class="um-foot">
        <button class="um-later" @click="dismissModal" data-testid="update-later-btn">
          {{ t('updater.later') }}
        </button>
        <button
          class="um-now"
          :disabled="!canInstall"
          @click="installNow"
          data-testid="update-now-btn"
        >
          {{ t('updater.updateNow') }}
        </button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
.um-overlay {
  position: fixed;
  inset: 0;
  z-index: 10001;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(4, 7, 12, 0.8);
  backdrop-filter: blur(6px);
}
.um-card {
  width: min(440px, 92vw);
  background: var(--bg-elevated, #0e1218);
  border: 1px solid #0e7490;
  border-radius: 14px;
  box-shadow:
    0 24px 60px rgba(0, 0, 0, 0.6),
    0 0 0 1px rgba(34, 211, 238, 0.08);
  overflow: hidden;
}
.um-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 16px;
  border-bottom: 1px solid var(--border-subtle);
}
.um-tag {
  font-size: 10px;
  font-weight: 800;
  letter-spacing: 0.16em;
  color: #22d3ee;
  border: 1px solid rgba(34, 211, 238, 0.4);
  border-radius: 6px;
  padding: 3px 8px;
}
.um-x {
  color: var(--text-dim);
  font-size: 14px;
  padding: 4px 8px;
  cursor: pointer;
}
.um-x:hover {
  color: var(--danger);
}
.um-body {
  padding: 26px 24px 18px;
  text-align: center;
}
.um-icon {
  width: 56px;
  height: 56px;
  margin: 0 auto 14px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 28px;
  color: #041016;
  background: linear-gradient(135deg, #0891b2, #22d3ee);
  border-radius: 50%;
  box-shadow: 0 0 24px rgba(34, 211, 238, 0.4);
}
.um-title {
  font-size: 18px;
  color: var(--text);
  margin: 0 0 6px;
}
.um-ver {
  font-size: 13px;
  color: var(--text-dim);
  margin: 0 0 12px;
}
.um-ver b {
  color: #67e8f9;
}
.um-notes {
  font-size: 12px;
  color: var(--text-dim);
  line-height: 1.5;
  margin: 0 0 8px;
  max-height: 160px;
  overflow-y: auto;
  white-space: pre-wrap;
  text-align: left;
  padding: 10px 12px;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid var(--border-subtle);
  border-radius: 8px;
}
.um-warn {
  font-size: 11px;
  color: #fbbf24;
  line-height: 1.4;
  margin: 8px 0 0;
}
.um-diag {
  font-size: 10px;
  color: var(--text-dim);
  line-height: 1.45;
  margin: 6px 0 0;
  padding: 8px 10px;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid var(--border-subtle);
  border-radius: 6px;
  text-align: left;
  word-break: break-word;
  max-height: 90px;
  overflow-y: auto;
}
.um-redl {
  margin-top: 12px;
  padding: 6px 12px;
  background: transparent;
  border: 1px dashed var(--border);
  border-radius: 7px;
  color: var(--text-dim);
  font-size: 11px;
  letter-spacing: 0.04em;
  cursor: pointer;
  transition: color 0.14s ease, border-color 0.14s ease;
}
.um-redl:hover {
  color: #67e8f9;
  border-color: rgba(34, 211, 238, 0.6);
}
.um-redl.hot {
  color: #fbbf24;
  border-color: rgba(251, 191, 36, 0.55);
}
.um-foot {
  display: flex;
  gap: 10px;
  padding: 14px 18px;
  border-top: 1px solid var(--border-subtle);
}
.um-later {
  flex: 1;
  padding: 11px;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 9px;
  color: var(--text-dim);
  font-size: 13px;
  cursor: pointer;
  transition: color 0.14s ease;
}
.um-later:hover {
  color: var(--text);
}
.um-now {
  flex: 2;
  padding: 11px;
  background: linear-gradient(135deg, #0891b2, #22d3ee);
  color: #041016;
  font-weight: 800;
  letter-spacing: 0.04em;
  font-size: 13px;
  border-radius: 9px;
  border: none;
  cursor: pointer;
  transition:
    filter 0.16s ease,
    transform 0.12s ease;
}
.um-now:hover:not(:disabled) {
  filter: brightness(1.1);
}
.um-now:active:not(:disabled) {
  transform: translateY(1px);
}
.um-now:disabled {
  opacity: 0.45;
  cursor: default;
}
</style>
