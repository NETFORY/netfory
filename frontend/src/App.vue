<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount } from 'vue'
import { useI18n } from 'vue-i18n'
import { useAuthStore } from '@/store/auth'
import { useToastsStore } from '@/store/toasts'
import { useWalletStore } from '@/store/wallet'
import { isTauri, closeAllEmbeddedWebviews, appListen, showNativeNotification, toggleFullscreen, takePendingDeepLink } from '@/lib/bridge'
import { shortAddr } from '@/lib/crypto'
import AppLayout from '@/layouts/AppLayout.vue'
import IdentityLayer from '@/views/IdentityLayer.vue'
import UnlockLayer from '@/views/UnlockLayer.vue'
import ToastHost from '@/components/ToastHost.vue'
import SyncSplash from '@/components/SyncSplash.vue'
import ConnectionBar from '@/components/ConnectionBar.vue'
import UpdateBar from '@/components/UpdateBar.vue'
import UpdateModal from '@/components/UpdateModal.vue'
import TranslatorBar from '@/components/TranslatorBar.vue'
import SystemConsole from '@/components/SystemConsole.vue'
import { useUpdater } from '@/lib/updater'
import ChatWidget from '@/views/ChatWidget.vue'
import ImageViewer from '@/views/ImageViewer.vue'
import { useDiscoveryStore } from '@/store/discovery'
import { useUiStore } from '@/store/ui'
import { useNetStore } from '@/store/net'
import { useRouter } from 'vue-router'
import { openDevUrl } from '@/lib/devLink'

const { t } = useI18n()
const auth = useAuthStore()
const toasts = useToastsStore()
const wallet = useWalletStore()
const discovery = useDiscoveryStore()
const ui = useUiStore()
const updater = useUpdater()
const router = useRouter()
const screen = computed(() => auth.status)
// The native chat window boots the same SPA with ?w=chat — render ONLY the
// messenger there. Detect it via the URL query OR (more robustly) the native
// Tauri window label `p2p_chat_main`, because a programmatically-built window
// may not preserve the query string on every platform (→ blank white window).
function detectChatWindow(): boolean {
  if (typeof window === 'undefined') return false
  try {
    if (new URLSearchParams(window.location.search).get('w') === 'chat') return true
    const w = window as any
    const label =
      w.__TAURI_INTERNALS__?.metadata?.currentWindow?.label ??
      w.__TAURI__?.window?.getCurrentWindow?.()?.label
    return label === 'p2p_chat_main'
  } catch {
    return false
  }
}
const isChatWindow = detectChatWindow()

// Detached System Console runs the same SPA with `?w=syscon` — render ONLY
// the console component (no sidebar, no wallet drawer). Same defensive
// fallback via native window label as for the chat window.
function detectConsoleWindow(): boolean {
  if (typeof window === 'undefined') return false
  try {
    if (new URLSearchParams(window.location.search).get('w') === 'syscon') return true
    const w = window as any
    const label =
      w.__TAURI_INTERNALS__?.metadata?.currentWindow?.label ??
      w.__TAURI__?.window?.getCurrentWindow?.()?.label
    return label === 'syscon'
  } catch {
    return false
  }
}
const isConsoleWindow = detectConsoleWindow()

// Полноэкранное окно просмотра изображения мессенджера (`?w=imgview`) —
// рендерим ТОЛЬКО просмотрщик (без layout, тостов и апдейтера).
function detectImgWindow(): boolean {
  if (typeof window === 'undefined') return false
  try {
    if (new URLSearchParams(window.location.search).get('w') === 'imgview') return true
    const w = window as any
    const label =
      w.__TAURI_INTERNALS__?.metadata?.currentWindow?.label ??
      w.__TAURI__?.window?.getCurrentWindow?.()?.label
    return label === 'p2p_img_view'
  } catch {
    return false
  }
}
const isImgWindow = detectImgWindow()

interface BalancePayload {
  amount?: number
  amountSth?: string
  sender?: string
}

// Focus-aware triage: in-app Tailwind toast when the shell is focused, native OS
// notification when it's backgrounded/minimized. Refreshes the wallet balance.
function onBalanceChanged(p: BalancePayload) {
  const amt = p.amountSth ?? (p.amount ? (p.amount / 1e8).toFixed(4) : '0')
  const sender = p.sender ? shortAddr(p.sender, 6, 4) : t('notifications.unknownSender')
  const body = t('notifications.received', { amount: amt, sender })
  if (typeof document !== 'undefined' && document.hasFocus()) {
    toasts.push(t('notifications.incomingTitle'), body, 'in')
  } else if (isTauri) {
    showNativeNotification('SmartNet Wallet', body)
  } else {
    toasts.push(t('notifications.incomingTitle'), body, 'in')
  }
  // Сразу и ещё раз через 6с (кэш API узла / распространение по пулу).
  wallet.refresh?.(true)
  setTimeout(() => wallet.refresh?.(true), 6000)
}

let unlisten: (() => void) | null = null
let unlistenChat: (() => void) | null = null
let unlistenPriv: (() => void) | null = null
let unlistenName: (() => void) | null = null
// Имя u:// подтвердилось в цепочке (индексер): тост в фокусе / системное уведомление в фоне.
function onNameConfirmed(p: { name?: string }) {
  if (!p?.name) return
  const body = t('notifications.nameConfirmed', { name: p.name })
  if (typeof document !== 'undefined' && document.hasFocus()) {
    toasts.push(t('notifications.nameConfirmedTitle'), body, 'in')
  } else if (isTauri) {
    showNativeNotification('NETFORY', body)
  } else {
    toasts.push(t('notifications.nameConfirmedTitle'), body, 'in')
  }
  window.dispatchEvent(new CustomEvent('name-confirmed', { detail: p.name }))
}
let unlistenDeep: (() => void) | null = null
let reconnectScan: ReturnType<typeof setTimeout> | null = null
function webHook(e: Event) {
  onBalanceChanged((e as CustomEvent).detail || {})
}

// F11 → toggle native OS fullscreen (shell only; ignore while typing so it
// never hijacks an input). No-op on web — handled inside toggleFullscreen.
// Ctrl+Shift+T → cycle UI theme (Focus → Matte Dark → Light → E-Ink).
const THEME_NAMES: Record<string, string> = {
  focus: 'FOCUS',
  'matte-dark': 'MATTE DARK',
  'readable-light': 'READABLE LIGHT',
  eink: 'E-INK',
}
function onKeydown(e: KeyboardEvent) {
  if ((e.ctrlKey || e.metaKey) && e.shiftKey && (e.key === 'T' || e.key === 't')) {
    e.preventDefault()
    const next = ui.cycleTheme()
    toasts.push(t('settings.appearance'), THEME_NAMES[next] || next, 'in')
    return
  }
  if (e.key !== 'F11') return
  e.preventDefault()
  void toggleFullscreen()
}

// Network restored (from ConnectionBar): re-run discovery after a short debounce
// so stale DHT/seeder paths refresh and the chat/app lists update instantly.
function onReconnect() {
  if (isChatWindow) return
  if (reconnectScan) clearTimeout(reconnectScan)
  reconnectScan = setTimeout(() => {
    void discovery.scan()
  }, 500)
}

interface ChatIncoming {
  sender?: string
  peerName?: string | null
  text?: string
  notify?: boolean
}
// Incoming P2P message → native OS notification (only from the main shell so the
// user sees it even when the messenger window is minimized). Respects the
// per-user notifications toggle (carried as `notify` on the event).
function onChatIncoming(p: ChatIncoming) {
  if (isChatWindow || !p.notify) return
  const who = p.peerName ? '@' + p.peerName : p.sender ? shortAddr(p.sender, 6, 4) : ''
  const body = `${who}: ${p.text || ''}`.slice(0, 180)
  if (typeof document !== 'undefined' && document.hasFocus()) {
    toasts.push(t('chat.notifyTitle'), body, 'in')
  } else if (isTauri) {
    showNativeNotification(t('chat.notifyTitle'), body)
  } else {
    toasts.push(t('chat.notifyTitle'), body, 'in')
  }
}

interface PrivateFilePayload {
  cid?: string
  from?: string
}
// Incoming E2EE private-file notification (on-chain `sf:` scanner) → toast /
// native notification; the file appears in "My Files" → Private Files.
function onPrivateFile(p: PrivateFilePayload) {
  if (isChatWindow) return
  const who = p.from ? shortAddr(p.from, 6, 4) : t('notifications.unknownSender')
  const body = t('fx.privateReceivedBody', { sender: who })
  if (typeof document !== 'undefined' && document.hasFocus()) {
    toasts.push(t('fx.privateReceivedTitle'), body, 'in')
  } else if (isTauri) {
    showNativeNotification('SmartNet', body)
  } else {
    toasts.push(t('fx.privateReceivedTitle'), body, 'in')
  }
}

function routeDeepLink(url: string) {
  if (url.startsWith('dev://')) {
    void openDevUrl(router, url)
  } else if (url.startsWith('sth://')) {
    // Кидаем событие на AppLayout — тот знает, как открывать dApp'ы
    // (нужен доступ к apps store + WebViewManager). AppLayout может быть
    // ещё не смонтирован (кошелёк заблокирован / холодный старт) —
    // буферизуем URL, AppLayout заберёт его в своём onMounted.
    ;(window as any).__sthPendingOpen = url
    window.dispatchEvent(new CustomEvent('sth-open', { detail: url }))
  } else if (url.startsWith('api://')) {
    ;(window as any).__apiPendingOpen = url
    window.dispatchEvent(new CustomEvent('api-open', { detail: url }))
  }
}

onMounted(async () => {
  // Отдельное окно консоли — не заряжаем вообще ничего кроме SystemConsole
  // (он сам подпишется на `sys-log` через свой lifecycle). Иначе тянули бы
  // за собой auth-store, кошелёк, updater, discovery — целый параллельный
  // экземпляр приложения, что тратит память и держит фоновые таймеры.
  if (isConsoleWindow) return
  // Глобальный флаг сети: все сетевые поллеры проверяют его и молчат в offline.
  void useNetStore().init()
  if (isTauri) {
    closeAllEmbeddedWebviews()
    unlisten = await appListen<BalancePayload>('sth-balance-changed', onBalanceChanged)
    unlistenChat = await appListen<ChatIncoming>('chat-incoming', onChatIncoming)
    unlistenPriv = await appListen<PrivateFilePayload>('private-file-received', onPrivateFile)
    unlistenName = await appListen<{ name?: string }>('name-confirmed', onNameConfirmed)
    // dev:// / sth:// / api:// deep link (внешний клик / второй запуск) →
    // маршрутизация во фронте:
    //   dev://…  — исходники в DEV HUB (Vue Router)
    //   sth://<APP_ID>[/path?query]  — открыть installed dApp по адресу
    //   api://…  — Netfory tool с URL bar'ом (адресная строка)
    unlistenDeep = await appListen<string>('deep-link', (url) => {
      if (typeof url !== 'string') return
      routeDeepLink(url)
    })
    // Холодный старт: ссылка первого запуска буферизуется в Rust —
    // забираем её после регистрации listener'а.
    const pending = await takePendingDeepLink()
    if (pending) routeDeepLink(pending)
  }
  // Web-testable hook: window.dispatchEvent(new CustomEvent('dev-open', { detail: 'dev://user/repo' }))
  window.addEventListener('dev-open', ((e: CustomEvent) => {
    if (typeof e.detail === 'string') void openDevUrl(router, e.detail)
  }) as EventListener)
  // Web-testable hook: window.dispatchEvent(new CustomEvent('sth-balance-changed', { detail }))
  window.addEventListener('sth-balance-changed', webHook as EventListener)
  window.addEventListener('keydown', onKeydown)
  // P2P-обновления: запускаем фоновый таймер только в главном окне (не в чате).
  if (!isChatWindow && !isImgWindow) void updater.start()
})

onBeforeUnmount(() => {
  if (unlisten) unlisten()
  if (unlistenChat) unlistenChat()
  if (unlistenPriv) unlistenPriv()
  if (unlistenName) unlistenName()
  if (unlistenDeep) unlistenDeep()
  if (reconnectScan) clearTimeout(reconnectScan)
  window.removeEventListener('sth-balance-changed', webHook as EventListener)
  window.removeEventListener('keydown', onKeydown)
  updater.stop()
})
</script>

<template>
  <div class="root" :class="{ 'chat-root': isChatWindow, 'syscon-root': isConsoleWindow }" data-testid="app-root">
    <ImageViewer v-if="isImgWindow" />
    <SystemConsole v-else-if="isConsoleWindow" :detached="true" @close="() => {}" />
    <template v-else>
      <ChatWidget v-if="isChatWindow" />
      <template v-else>
        <div v-if="!auth.ready" class="boot" data-testid="boot-screen">
          <span class="boot-logo">STH://</span>
          <span class="dim">INITIALIZING CORE<span class="blink">_</span></span>
        </div>

        <template v-else>
          <IdentityLayer v-if="screen === 'empty'" />
          <UnlockLayer v-else-if="screen === 'locked'" />
          <AppLayout v-else />
        </template>

        <ToastHost />
        <SyncSplash />
      </template>
      <ConnectionBar @reconnect="onReconnect" />
      <template v-if="!isChatWindow">
        <UpdateBar />
        <UpdateModal />
        <TranslatorBar />
      </template>
    </template>
  </div>
</template>

<style scoped>
.root {
  min-height: 100vh;
}
.chat-root {
  background: #09090b;
}
.syscon-root {
  /* Полноэкранная консоль: убираем внутренние отступы, sizing управляется
     самим SystemConsole (position:fixed → inset:0 в detached-режиме). */
  background: #05070d;
  height: 100vh;
  overflow: hidden;
}
.boot {
  height: 100vh;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 14px;
}
.boot-logo {
  color: var(--accent);
  font-size: 28px;
  font-weight: var(--fw-800);
  letter-spacing: 0.1em;
}
</style>
