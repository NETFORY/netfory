<script setup lang="ts">
/*
 * SmartNet Wallet // right-side sliding drawer (thin-client widget)
 *
 *   Globally trusted: assumes the shell is already unlocked (no PIN re-prompt).
 *   Hosts the in-drawer view router and the MetaMask-style `incoming-tx-request`
 *   interception (registered in the store on mount). Hiding native dApp webviews
 *   while open is delegated to the UI overlay flag.
 */
import { computed, ref, watch, onMounted, onBeforeUnmount } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { useWalletStore, WALLET_ACCENTS, type WalletView } from '@/store/wallet'
import { useTabsStore } from '@/store/tabs'
import { nodeStatus, isTauri } from '@/lib/bridge'
import DashboardView from '@/wallet/views/DashboardView.vue'
import WalletSkeleton from '@/wallet/components/WalletSkeleton.vue'
import SendView from '@/wallet/views/SendView.vue'
import ReceiveView from '@/wallet/views/ReceiveView.vue'
import SwapView from '@/wallet/views/SwapView.vue'
import HistoryView from '@/wallet/views/HistoryView.vue'
import VerifyView from '@/wallet/views/VerifyView.vue'
import ConnectView from '@/wallet/views/ConnectView.vue'
import AuthorizeMessageView from '@/wallet/views/AuthorizeMessageView.vue'
import ConnectedAppsView from '@/wallet/views/ConnectedAppsView.vue'
import NodesView from '@/wallet/views/NodesView.vue'
import WalletSettingsView from '@/wallet/views/WalletSettingsView.vue'

const { t } = useI18n()
const wallet = useWalletStore()
const router = useRouter()
const tabs = useTabsStore()

const views = {
  dashboard: DashboardView,
  send: SendView,
  receive: ReceiveView,
  swap: SwapView,
  history: HistoryView,
  verify: VerifyView,
  connect: ConnectView,
  authorize: AuthorizeMessageView,
  apps: ConnectedAppsView,
  nodes: NodesView,
  settings: WalletSettingsView,
} as const

const title = computed(() => t(`wallet.view.${wallet.currentView}`))
const canGoBack = computed(
  () => !['dashboard', 'verify', 'connect', 'authorize'].includes(wallet.currentView),
)

// Прилипший к низу drawer'а toolbar с быстрыми кнопками (как в браузерном
// расширении Core-Wallet). Скрыт для approval-gated view'ов (verify / connect
// / authorize) — там пользователь ДОЛЖЕН принять решение по конкретному запросу.
const showBottomBar = computed(
  () => !['verify', 'connect', 'authorize'].includes(wallet.currentView),
)
const bottomItems = computed(() => [
  { id: 'apps' as const, icon: '⌨', label: t('wallet.bottom.apps'), testid: 'wallet-bottom-apps' },
  { id: 'nodes' as const, icon: '⬢', label: t('wallet.bottom.nodes'), testid: 'wallet-bottom-nodes' },
  { id: 'settings' as const, icon: '⚙', label: t('wallet.bottom.settings'), testid: 'wallet-bottom-settings' },
])

// The drawer no longer fully hides dApp webviews — AppLayout shrinks the active
// webview by the drawer width so the app stays visible alongside the wallet.

// ---- Transport badge: shows whether wallet calls go over the private P2P
// mesh (api://) or the public clearnet pool (incl. auto-fallback). ----
const transport = ref<'p2p' | 'fallback' | 'clearnet' | ''>('')
const transportLabel = computed(() => {
  if (transport.value === 'p2p') return t('wallet.tpP2P')
  if (transport.value === 'fallback') return t('wallet.tpFallback')
  if (transport.value === 'clearnet') return t('wallet.tpClearnet')
  return ''
})
const transportHint = computed(() => {
  if (transport.value === 'p2p') return t('wallet.tpP2PHint')
  if (transport.value === 'fallback') return t('wallet.tpFallbackHint')
  if (transport.value === 'clearnet') return t('wallet.tpClearnetHint')
  return ''
})
let transportTimer: ReturnType<typeof setInterval> | null = null
async function refreshTransport() {
  const s = await nodeStatus()
  transport.value = s?.transport ?? ''
}
// Click the badge → jump to Settings → Network and focus the "Custom nodes"
// block so the user can switch transport (P2P / clearnet) right from the wallet.
function openTransportSettings() {
  wallet.closeWallet()
  tabs.goHome()
  router.push({ name: 'settings', query: { tab: 'network', focus: 'custom-nodes' } })
}
watch(
  () => wallet.isWalletOpen,
  (open) => {
    if (open) refreshTransport()
  },
)

onMounted(() => {
  wallet.init()
  refreshTransport()
  transportTimer = setInterval(refreshTransport, 5000)
})
onBeforeUnmount(() => {
  wallet.dispose()
  if (transportTimer) clearInterval(transportTimer)
})
</script>

<template>
  <teleport to="body">
    <!-- backdrop -->
    <transition name="fade">
      <div
        v-if="wallet.isWalletOpen"
        class="wallet-backdrop"
        @click="wallet.closeWallet()"
        data-testid="wallet-backdrop"
      />
    </transition>

    <!-- right drawer -->
    <transition name="slide-right">
      <aside
        v-if="wallet.isWalletOpen"
        class="wallet-drawer"
        :class="`accent-${wallet.accent}`"
        :style="WALLET_ACCENTS[wallet.accent] ?? undefined"
        :data-accent="wallet.accent"
        data-testid="wallet-drawer"
      >
        <header class="wd-head">
          <button
            v-if="canGoBack"
            class="wd-back"
            @click="wallet.setView('dashboard')"
            data-testid="wallet-back-btn"
          >
            ‹
          </button>
          <span class="wd-title">{{ title }}</span>
          <button
            v-if="transportLabel"
            class="wd-transport"
            :class="`tr-${transport}`"
            :title="transportHint + ' — ' + t('wallet.tpClick')"
            @click="openTransportSettings"
            data-testid="wallet-transport-badge"
          >
            <span class="tr-dot"></span>{{ transportLabel }}
          </button>
          <button class="wd-close" @click="wallet.closeWallet()" data-testid="wallet-close-btn">✕</button>
        </header>

        <div class="wd-body" data-testid="wallet-body">
          <!-- первое открытие без снимка: скелетон вместо пустой панели с «загрузка» -->
          <WalletSkeleton v-if="wallet.loading && !wallet.account" />
          <transition v-else name="view" mode="out-in">
            <component :is="views[wallet.currentView as WalletView]" :key="wallet.currentView" />
          </transition>
        </div>

        <!-- Sticky bottom toolbar (Core-Wallet style): quick access to Connected
             Apps / Node picker / Wallet settings. Hidden during approval-gated
             view'ов, чтобы не отвлекать пользователя от prompt'а. -->
        <nav
          v-if="showBottomBar"
          class="wd-bottom"
          data-testid="wallet-bottom-bar"
        >
          <button
            v-for="it in bottomItems"
            :key="it.id"
            class="wd-tab"
            :class="{ on: wallet.currentView === it.id }"
            @click="wallet.setView(it.id)"
            :data-testid="it.testid"
          >
            <span class="wd-tab-ic">
              {{ it.icon }}
              <!-- стрим sth/events/1 упал → работаем по legacy REST: точка-предупреждение -->
              <span
                v-if="it.id === 'nodes' && !wallet.streamLive && isTauri"
                class="wd-tab-dot"
                :title="t('wallet.nodes.streamLegacy')"
                data-testid="wallet-bottom-nodes-dot"
              />
            </span>
            <span class="wd-tab-lbl">{{ it.label }}</span>
          </button>
        </nav>

        <transition name="fade">
          <div v-if="wallet.toast" class="wd-toast" data-testid="wallet-toast">✓ {{ wallet.toast }}</div>
        </transition>
      </aside>
    </transition>
  </teleport>
</template>

<style scoped>
.wallet-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.55);
  z-index: 90;
}
.wallet-drawer {
  position: fixed;
  top: 0;
  right: 0;
  height: 100%;
  width: 400px;
  max-width: 100vw;
  z-index: 100;
  display: flex;
  flex-direction: column;
  background: var(--bg);
  border-left: 1px solid var(--border-accent);
  box-shadow: -18px 0 50px rgba(0, 0, 0, 0.45);
}
.wd-head {
  display: flex;
  align-items: center;
  gap: 10px;
  height: 52px;
  min-height: 52px;
  padding: 0 14px;
  background: var(--panel);
  border-bottom: 1px solid var(--border);
}
.wd-back {
  font-size: 20px;
  line-height: 1;
  color: var(--text-dim);
  padding: 2px 8px;
}
.wd-back:hover {
  color: var(--accent);
}
.wd-title {
  flex: 1;
  font-size: 12px;
  font-weight: var(--fw-800);
  letter-spacing: 0.18em;
  text-transform: uppercase;
  color: var(--accent);
}
.wd-transport {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 9px;
  font-weight: var(--fw-800);
  letter-spacing: 0.12em;
  text-transform: uppercase;
  padding: 3px 7px;
  border-radius: 999px;
  border: 1px solid currentColor;
  white-space: nowrap;
  cursor: pointer;
  transition: background-color 0.15s ease, transform 0.15s ease;
}
.wd-transport:hover {
  background: rgba(255, 255, 255, 0.08);
  transform: translateY(-1px);
}
.wd-transport .tr-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
  box-shadow: 0 0 6px currentColor;
}
.wd-transport.tr-p2p {
  color: #818cf8;
}
.wd-transport.tr-fallback {
  color: #fbbf24;
}
.wd-transport.tr-clearnet {
  color: var(--text-dim);
}
.wd-close {
  font-size: 13px;
  color: var(--text-dim);
  padding: 4px 8px;
}
.wd-close:hover {
  color: var(--danger);
}
.wd-body {
  flex: 1;
  overflow-y: auto;
  padding: 20px 18px 28px;
}
.wd-toast {
  position: absolute;
  left: 18px;
  right: 18px;
  bottom: 82px;
  text-align: center;
  font-size: 12px;
  font-weight: var(--fw-700);
  color: var(--success);
  background: var(--panel);
  border: 1px solid var(--success);
  border-radius: var(--radius);
  padding: 10px;
}

/* Sticky bottom toolbar — три быстрых входа (Connected Apps / Nodes / Settings). */
.wd-bottom {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 0;
  min-height: 64px;
  background: var(--panel);
  border-top: 1px solid var(--border);
}
.wd-tab {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 3px;
  padding: 10px 6px;
  border: none;
  border-top: 2px solid transparent;
  background: transparent;
  color: var(--text-dim);
  cursor: pointer;
  transition: color 0.15s ease, background-color 0.15s ease, border-color 0.15s ease;
}
.wd-tab:hover {
  color: var(--text);
  background: color-mix(in srgb, var(--accent) 6%, transparent);
}
.wd-tab.on {
  color: var(--accent);
  border-top-color: var(--accent);
  background: color-mix(in srgb, var(--accent) 10%, transparent);
}
.wd-tab-ic {
  font-size: 18px;
  line-height: 1;
  font-family: var(--font-mono, monospace);
}
.wd-tab-ic {
  position: relative;
}
.wd-tab-dot {
  position: absolute;
  top: -3px;
  right: -8px;
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #f0b429;
  box-shadow: 0 0 0 2px var(--panel-solid), 0 0 8px rgba(240, 180, 41, 0.7);
  animation: wd-dot-pulse 1.8s ease-in-out infinite;
}
@keyframes wd-dot-pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.45; }
}
.wd-tab-lbl {
  font-size: 9px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  font-weight: var(--fw-800);
}

/* slide-right transition (drawer panel) */
.slide-right-enter-active,
.slide-right-leave-active {
  transition: transform 0.3s ease-in-out;
}
.slide-right-enter-from,
.slide-right-leave-to {
  transform: translateX(100%);
}

/* backdrop + toast fade */
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.25s ease;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}

/* in-drawer view swap */
.view-enter-active,
.view-leave-active {
  transition: opacity 0.16s ease, transform 0.16s ease;
}
.view-enter-from {
  opacity: 0;
  transform: translateX(10px);
}
.view-leave-to {
  opacity: 0;
}
</style>
