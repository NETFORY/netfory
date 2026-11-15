<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useAppsStore } from '@/store/apps'
import { useDiscoveryStore } from '@/store/discovery'
import NodeTelemetryPanel from '@/components/NodeTelemetryPanel.vue'

const route = useRoute()
const router = useRouter()
const { t } = useI18n()
const loading = ref(true)
const appsStore = useAppsStore()
const discovery = useDiscoveryStore()

const address = computed(() => String(route.params.address || ''))
const manifest = computed(() => appsStore.apps.find((m) => m.id === address.value) || null)
const seeds = computed(() => discovery.apps.find((a) => a.id === address.value)?.seeds ?? 0)
const panelOpen = ref(false)

// Placeholder dApp content served via iframe srcdoc (Web Fallback).
// In Tauri this route is bypassed: a child Multi-Webview window is opened instead.
const srcdoc = computed(
  () => `<!doctype html><html><head><meta charset="utf-8"><style>
    *{margin:0;font-family:'JetBrains Mono',monospace}
    body{background:#0d0f12;color:#5c7894;height:100vh;display:flex;
      flex-direction:column;align-items:center;justify-content:center;gap:18px;text-align:center}
    .a{color:#d9822b;font-weight: var(--fw-800);font-size:20px;letter-spacing:.1em}
    .b{font-size:12px;letter-spacing:.16em;text-transform:uppercase}
    .g{position:fixed;inset:0;background-image:linear-gradient(rgba(92,120,148,.06) 1px,transparent 1px),
      linear-gradient(90deg,rgba(92,120,148,.06) 1px,transparent 1px);background-size:34px 34px}
    .c{border:1px solid #232831;padding:30px 40px;background:#14171c;z-index:1}
  </style></head><body><div class="g"></div><div class="c">
    <div class="a">sth://${address.value}</div>
    <p class="b" style="margin-top:14px">${t('viewer.serving')}</p>
  </div></body></html>`,
)

onMounted(() => {
  setTimeout(() => (loading.value = false), 700)
})
</script>

<template>
  <div class="viewer" data-testid="app-viewer">
    <!-- ZeroNet-style folded corner: opens the node telemetry panel -->
    <button
      v-if="!panelOpen"
      class="corner-badge"
      @click="panelOpen = true"
      data-testid="viewer-corner-badge"
      :title="t('nodePanel.title')"
    >
      <span class="corner-fold" />
      <span class="corner-label">{{ seeds > 0 ? seeds : 'S' }}</span>
    </button>

    <NodeTelemetryPanel
      :open="panelOpen"
      :manifest="manifest"
      :address="address"
      :seeds="seeds"
      @close="panelOpen = false"
    />

    <div class="viewer-head">
      <button class="btn" @click="router.back()" data-testid="viewer-back-btn">‹ {{ t('common.back') }}</button>
      <code class="viewer-addr hl">sth://{{ address }}</code>
      <span class="viewer-status dim">{{ loading ? t('viewer.connecting') : t('viewer.serving') }}</span>
    </div>

    <p class="viewer-note dim">{{ t('viewer.placeholder') }}</p>

    <div class="frame-wrap panel">
      <div v-if="loading" class="frame-loading" data-testid="viewer-loading">
        <span class="hl">{{ t('viewer.connecting') }}<span class="blink">_</span></span>
      </div>
      <iframe
        v-show="!loading"
        :srcdoc="srcdoc"
        class="frame"
        sandbox="allow-scripts"
        data-testid="viewer-iframe"
        title="dApp"
      />
    </div>
  </div>
</template>

<style scoped>
.viewer {
  display: flex;
  flex-direction: column;
  gap: 14px;
  height: 100%;
  position: relative;
}
/* ZeroNet-style folded corner badge (top-right) */
.corner-badge {
  position: absolute;
  top: 0;
  right: 0;
  z-index: 20;
  width: 52px;
  height: 52px;
  border: none;
  background: transparent;
  cursor: pointer;
  padding: 0;
}
.corner-fold {
  position: absolute;
  inset: 0;
  background: linear-gradient(225deg, #2a3038 0%, #161a1f 60%);
  border-left: 1px solid #3a4450;
  border-bottom: 1px solid #3a4450;
  clip-path: polygon(100% 0, 0 0, 100% 100%);
  transition: background 0.18s;
}
.corner-badge:hover .corner-fold {
  background: linear-gradient(225deg, var(--accent) 0%, #161a1f 70%);
}
.corner-label {
  position: absolute;
  top: 7px;
  right: 7px;
  font-size: 11px;
  font-weight: var(--fw-700);
  color: #d7dce2;
  font-variant-numeric: tabular-nums;
  pointer-events: none;
}
.viewer-head {
  display: flex;
  align-items: center;
  gap: 14px;
}
.viewer-addr {
  font-size: 13px;
  font-weight: var(--fw-700);
}
.viewer-status {
  margin-left: auto;
  font-size: 11px;
  letter-spacing: 0.1em;
}
.viewer-note {
  font-size: 11px;
  line-height: 1.6;
  max-width: 760px;
}
.frame-wrap {
  flex: 1;
  position: relative;
  overflow: hidden;
  min-height: 360px;
}
.frame-loading {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  letter-spacing: 0.1em;
}
.frame {
  width: 100%;
  height: 100%;
  border: none;
  display: block;
}
</style>
