<script setup lang="ts">
/*
 * SmartNet Wallet // ConnectedAppsView
 *
 *   Список dApp origin'ов, которым пользователь дал доверие
 *   (`approvedOrigins`) при handshake `getAccount`. Позволяет отозвать
 *   доверие (Disconnect) — аналог экрана "Access · Connected Apps" в
 *   браузерном расширении Core-Wallet.
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useWalletStore } from '@/store/wallet'

const { t } = useI18n()
const wallet = useWalletStore()

// Свежее подключение сверху (список хранится в порядке добавления в LS).
const origins = computed(() => [...wallet.approvedOrigins].reverse())

/** Отображаемое имя dApp: пытаемся распарсить как sth://ADDRESS или как URL. */
function displayName(origin: string): string {
  const s = origin.trim()
  const m = /^sth:\/\/([^/]+)/i.exec(s)
  if (m) return m[1]
  try {
    return new URL(s).host || s
  } catch {
    return s
  }
}
function displayHost(origin: string): string {
  return displayName(origin)
}

// Двухшаговое подтверждение отзыва (защита от промаха мышью).
const confirming = ref<string | null>(null)
function askConfirm(o: string) {
  confirming.value = o
}
function cancelConfirm() {
  confirming.value = null
}
function disconnect(o: string) {
  wallet.revokeOrigin(o)
  wallet.flash(t('wallet.apps.disconnected'))
  confirming.value = null
}
</script>

<template>
  <div class="capps" data-testid="wallet-connected-apps">
    <p class="hdr-title">{{ t('wallet.apps.title') }}</p>
    <p class="desc dim">{{ t('wallet.apps.desc') }}</p>

    <div v-if="!origins.length" class="empty" data-testid="apps-empty">
      <span class="empty-mark">[ ∅ ]</span>
      <p class="dim">{{ t('wallet.apps.empty') }}</p>
    </div>

    <div v-else class="list" data-testid="apps-list">
      <div
        v-for="origin in origins"
        :key="origin"
        class="row"
        :data-testid="`app-row-${origin}`"
      >
        <div class="ava">{{ displayName(origin).charAt(0).toUpperCase() }}</div>
        <div class="meta">
          <span class="name" :data-testid="`app-name-${origin}`">{{ displayName(origin) }}</span>
          <code class="host mono">{{ displayHost(origin) }}</code>
        </div>
        <button
          v-if="confirming !== origin"
          class="btn-disc"
          @click="askConfirm(origin)"
          :data-testid="`app-disc-${origin}`"
        >
          {{ t('wallet.apps.disconnect') }}
        </button>
        <div v-else class="confirm">
          <button
            class="btn-mini"
            @click="cancelConfirm"
            :data-testid="`app-disc-cancel-${origin}`"
          >
            {{ t('wallet.apps.cancel') }}
          </button>
          <button
            class="btn-mini danger"
            @click="disconnect(origin)"
            :data-testid="`app-disc-confirm-${origin}`"
          >
            {{ t('wallet.apps.confirmDisc') }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.capps {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.hdr-title {
  margin: 0;
  font-size: 11px;
  font-weight: var(--fw-800);
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--accent);
}
.desc {
  font-size: 11.5px;
  line-height: 1.5;
  margin: 0 2px;
}
.empty {
  padding: 40px 20px;
  text-align: center;
}
.empty-mark {
  display: block;
  font-family: var(--font-mono, monospace);
  color: var(--accent);
  font-weight: var(--fw-700);
  margin-bottom: 8px;
}
.list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--panel);
  transition: border-color 0.15s ease;
}
.row:hover {
  border-color: var(--border-accent);
}
.ava {
  width: 30px;
  height: 30px;
  border-radius: 8px;
  display: grid;
  place-items: center;
  font-weight: var(--fw-800);
  font-size: 13px;
  color: var(--bg);
  background: var(--accent);
  flex-shrink: 0;
}
.meta {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.name {
  font-size: 12px;
  font-weight: var(--fw-700);
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.host {
  font-size: 10px;
  color: var(--text-dim);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.mono {
  font-family: var(--font-mono, monospace);
}
.btn-disc {
  font-size: 10px;
  letter-spacing: 0.08em;
  color: var(--danger);
  border: 1px solid color-mix(in srgb, var(--danger) 55%, transparent);
  border-radius: 4px;
  padding: 6px 10px;
  font-weight: var(--fw-700);
  transition: background-color 0.15s ease;
  flex-shrink: 0;
  background: transparent;
}
.btn-disc:hover {
  background: color-mix(in srgb, var(--danger) 18%, transparent);
}
.confirm {
  display: flex;
  gap: 5px;
  flex-shrink: 0;
}
.btn-mini {
  font-size: 10px;
  padding: 6px 8px;
  border-radius: 4px;
  border: 1px solid var(--border);
  color: var(--text-dim);
  font-weight: var(--fw-700);
  letter-spacing: 0.05em;
  background: transparent;
}
.btn-mini:hover {
  color: var(--text);
  border-color: var(--border-accent);
}
.btn-mini.danger {
  color: var(--danger);
  border-color: color-mix(in srgb, var(--danger) 55%, transparent);
  background: color-mix(in srgb, var(--danger) 18%, transparent);
}
</style>
