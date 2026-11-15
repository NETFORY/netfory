/*
 * SmartNet Wallet // shared shell store
 *
 *   Global reactive state for the right-side wallet drawer widget. Holds the
 *   open/closed flag, the in-drawer view router, the active account snapshot
 *   (fetched from Rust), and the pending dApp signature request intercepted via
 *   the `incoming-tx-request` event. The widget is STATELESS w.r.t. secrets —
 *   no keys, no PIN: it only mirrors what the Rust core returns.
 */
import { defineStore } from 'pinia'
import {
  walletInvoke,
  walletListen,
  type WalletAccount,
  type WalletTx,
  type IncomingTxRequest,
  type IncomingSignMessageRequest,
  type BroadcastResult,
  type UnlistenFn,
  type WalletToken,
  type WalletAccountItem,
} from '@/wallet/ipc'
import { walletStreamStatus, walletStreamReconnect } from '@/lib/bridge'
import { friendlyTxError } from '@/lib/bridge'
import { useNetStore } from '@/store/net'
import { useAuthStore } from '@/store/auth'

export type WalletView = 'dashboard' | 'send' | 'receive' | 'swap' | 'history' | 'verify' | 'connect' | 'authorize' | 'apps' | 'nodes' | 'settings'

// kept at module scope (non-reactive, not serialized into state)
let unlisten: UnlistenFn | null = null
let unlistenConnect: UnlistenFn | null = null
let unlistenSign: UnlistenFn | null = null
let unlistenStream: UnlistenFn | null = null
let unlistenIndex: UnlistenFn | null = null
let inited = false
let refreshTick = 0
let bridgeTimer: ReturnType<typeof setInterval> | null = null
let refreshTimer: ReturnType<typeof setInterval> | null = null
let flashTimer: ReturnType<typeof setTimeout> | null = null
const MAX_BRIDGE_TRIES = 30
const ORIGINS_KEY = 'sth_dapp_origins'
/** Снимок дашборда (аккаунт, история, курс, токены, аккаунты) — панель открывается мгновенно. */
const SNAP_KEY = 'sth_wallet_snap'
const SNAP_TX = 50

interface WalletSnapshot {
  account: WalletAccount
  transactions: WalletTx[]
  rate: number
  rateHistory: RateSample[]
  tokens: WalletToken[]
  accounts: WalletAccountItem[]
  netHeight: number
  ts: number
}

function snapKey(): string | null {
  const id = useAuthStore().address
  return id ? `${SNAP_KEY}:${id}` : null
}

/** Surface the real backend error (Tauri rejects with a plain string). */
function errText(e: any, fallback: string): string {
  const raw = typeof e === 'string' && e.trim() ? e : e?.message ?? fallback
  return friendlyTxError(raw, fallback)
}

/** Сырой баланс токена → строка с учётом decimals (зеркало Rust `fmt_token_amount`). */
function fmtTokenAmount(raw: string, decimals: number): string {
  const r = raw.replace(/^0+/, '') || '0'
  if (decimals <= 0) return r
  const padded = r.length <= decimals ? '0'.repeat(decimals + 1 - r.length) + r : r
  const int = padded.slice(0, padded.length - decimals)
  const frac = padded.slice(padded.length - decimals).replace(/0+$/, '')
  return frac ? `${int}.${frac}` : int
}

/** HistItem из Rust (`kind`) → WalletTx фронта (`type`). */
function toWalletTx(it: WalletFrame['item']): WalletTx | null {
  if (!it?.id) return null
  const kind = (it as any).kind ?? it.type
  return {
    id: it.id,
    type: kind === 'in' ? 'in' : 'out',
    amount: it.amount,
    counterparty: it.counterparty,
    timestamp: it.timestamp,
    confirmed: !!it.confirmed,
    memo: it.memo,
  }
}

interface ConnectRequest {
  id: string
  origin: string
}

interface WalletState {
  isWalletOpen: boolean
  currentView: WalletView
  account: WalletAccount | null
  transactions: WalletTx[]
  /** История по адресам (последний снимок) — мгновенный показ при переключении. */
  txByAddr: Record<string, WalletTx[]>
  /** Вспышка баланса после изменения: up — пришло, down — ушло (сбрасывается через 1.5с). */
  balanceFlash: '' | 'up' | 'down'
  rate: number
  loading: boolean
  busy: boolean
  error: string
  toast: string
  /** Последняя pending, снятая как потерянная (баннер в Dashboard до закрытия). */
  droppedTx: { id: string; reason: string; at: number } | null
  pendingRequest: IncomingTxRequest | null
  bridge: 'idle' | 'pending' | 'confirmed'
  bridgeTries: number
  pendingConnect: ConnectRequest | null
  pendingSignMessage: IncomingSignMessageRequest | null
  approvedOrigins: string[]
  publishCtx: { appId: string; amount: string; escrow: string; memo: string; kind: 'publish' | 'boost' | 'update'; dir?: string } | null
  /** SHIP-44: push-стрим `sth/events/1` подключён (баланс/tx приходят без поллинга). */
  streamLive: boolean
  /** Узел стрима (`api://…/sth-core`). */
  streamNode: string
  /** Стрим подключён, но блоки не идут > 60 с — данные могут быть устаревшими. */
  streamStale: boolean
  /** Date.now() последнего кадра block/tx (0 — не было). */
  lastBlockAt: number
  /** Прогресс фоновой индексации истории владельца в sled. */
  txIndex: TxIndexProgress | null
  /** Высота сети по кадрам `block` стрима (0 — неизвестна). */
  netHeight: number
  /** SHIP-14: балансы токенов кошелька (только ненулевые). */
  tokens: WalletToken[]
  /** Тап по балансу: STH ⇄ USDT ⇄ скрыт (••••). Хранится в localStorage. */
  balanceMode: BalanceMode
  /** История курса STH/USDT за 7 дней (спарклайн), unix ms + rate. */
  rateHistory: RateSample[]
  /** Сохранённые сети SmartHoldem (Networks → «Добавить сеть»). Mainnet — встроенная. */
  networks: SavedNetwork[]
  /** id активной сети; всё кроме 'mainnet' — режим только просмотра (платежи недоступны). */
  activeNetworkId: string
  /** Акцент панели кошелька (как три облика расширения): 'teal' — по теме приложения. */
  accent: WalletAccent
  /** Main-аккаунт (index 0) — идентичность: профиль, домены, публикация dapps платят с него. */
  mainAccount: WalletAccount | null
  /** BIP-44 аккаунты сида (main + суб-адреса). */
  accounts: WalletAccountItem[]
  accountsLoading: boolean
}

export type WalletAccent = 'teal' | 'orange' | 'violet'
/** Палитры акцента: применяются CSS-переменными на корне .wallet-drawer. */
export const WALLET_ACCENTS: Record<WalletAccent, Record<string, string> | null> = {
  teal: null, // наследовать тему приложения
  orange: {
    '--accent': '#ff7a1a',
    '--accent-hover': '#ff944d',
    '--accent-soft': 'rgba(255, 122, 26, 0.12)',
    '--accent-glow': 'rgba(255, 122, 26, 0.45)',
    '--border-accent': 'rgba(255, 122, 26, 0.45)',
    '--secondary': '#f0b429',
    '--success': '#ff7a1a',
    '--status': '#ff7a1a',
  },
  violet: {
    '--accent': '#7c6cff',
    '--accent-hover': '#9a8dff',
    '--accent-soft': 'rgba(124, 108, 255, 0.12)',
    '--accent-glow': 'rgba(124, 108, 255, 0.45)',
    '--border-accent': 'rgba(124, 108, 255, 0.45)',
    '--secondary': '#2ee3c6',
    '--success': '#7c6cff',
    '--status': '#7c6cff',
  },
}
const LS_ACCENT = 'wallet.accent'

export type BalanceMode = 'sth' | 'usdt' | 'hidden'
export interface RateSample {
  t: number
  r: number
}
export interface SavedNetwork {
  id: string
  api: string
  name: string
  nethash: string
  token: string
  symbol: string
  explorer: string
  version: number
  wif: number
  coreVersion?: string
  height?: number
  builtin?: boolean
}

const LS_BALANCE_MODE = 'wallet.balanceMode'
const LS_NETWORKS = 'wallet.networks'
const LS_ACTIVE_NET = 'wallet.activeNetwork'
const MAINNET: SavedNetwork = {
  id: 'mainnet',
  api: '',
  name: 'SmartHoldem Mainnet',
  nethash: '',
  token: 'STH',
  symbol: 'STH',
  explorer: '',
  version: 63,
  wif: 255,
  builtin: true,
}
function lsGet<T>(k: string, def: T): T {
  try {
    const v = localStorage.getItem(k)
    return v ? (JSON.parse(v) as T) : def
  } catch {
    return def
  }
}

/** Кадр push-стрима кошелька (Rust `wallet_stream.rs` → событие `wallet-event`). */
interface WalletFrame {
  type: string
  live?: boolean
  node?: string
  height?: number
  balanceSth?: string
  block?: { height?: number }
  item?: WalletTx & { kind?: string }
  tx?: { id?: string }
  wallet?: { address?: string }
  /** SHIP-14: `attributes.tokens` кадра balance → [{id, balance}]. */
  tokens?: { id: string; balance: string }[]
  address?: string
  /** tx-dropped (Rust txindex.rs): pending снята как потерянная. */
  id?: string
  reason?: string
}

/** Прогресс локального индекса истории (Rust txindex.rs, событие `txindex-progress`). */
export interface TxIndexProgress {
  address: string
  indexed: number
  total: number
  done: boolean
}

export const useWalletStore = defineStore('wallet', {
  state: (): WalletState => ({
    isWalletOpen: false,
    currentView: 'dashboard',
    account: null,
    transactions: [],
    txByAddr: {},
    balanceFlash: '',
    rate: 0,
    loading: false,
    busy: false,
    error: '',
    toast: '',
    droppedTx: null,
    pendingRequest: null,
    bridge: 'idle',
    bridgeTries: 0,
    pendingConnect: null,
    pendingSignMessage: null,
    approvedOrigins: [],
    publishCtx: null,
    streamLive: false,
    streamNode: '',
    streamStale: false,
    lastBlockAt: 0,
    txIndex: null,
    netHeight: 0,
    tokens: [],
    balanceMode: lsGet<BalanceMode>(LS_BALANCE_MODE, 'sth'),
    rateHistory: [],
    networks: [MAINNET, ...lsGet<SavedNetwork[]>(LS_NETWORKS, []).filter((n) => n.id !== 'mainnet')],
    activeNetworkId: lsGet<string>(LS_ACTIVE_NET, 'mainnet'),
    accent: lsGet<WalletAccent>(LS_ACCENT, 'teal'),
    mainAccount: null,
    accounts: [],
    accountsLoading: false,
  }),

  getters: {
    address: (s): string => s.account?.address ?? '',
    balance: (s): string => s.account?.balance ?? '0',
    fiat: (s): string => s.account?.fiat ?? '0',
    /** Баланс/фиат main-аккаунта — для профиля и платежей приложения (не зависит от выбранного в кошельке). */
    mainBalance: (s): string => s.mainAccount?.balance ?? s.account?.balance ?? '0',
    mainFiat: (s): string => s.mainAccount?.fiat ?? s.account?.fiat ?? '0',
    mainAddress: (s): string => s.mainAccount?.address ?? s.account?.address ?? '',
    network: (s): string => s.account?.network ?? 'MAINNET',
    recent: (s): WalletTx[] => s.transactions.slice(0, 3),
    /** Неподтверждённые tx (кадры mempoolTx) — карточки с таймером до подтверждения. */
    pendingTxs: (s): WalletTx[] => s.transactions.filter((t) => !t.confirmed),
    activeNetwork: (s): SavedNetwork => s.networks.find((n) => n.id === s.activeNetworkId) ?? s.networks[0],
    /** Не mainnet → только просмотр/управление сетями, платежи отключены. */
    viewOnly: (s): boolean => s.activeNetworkId !== 'mainnet',
    activeAccount: (s): WalletAccountItem | undefined => s.accounts.find((a) => a.active),
    /** Сумма балансов всех аккаунтов сида (STH, 8 знаков). */
    totalBalance: (s): string => {
      if (s.accounts.length < 2) return ''
      const sum = s.accounts.reduce((acc, a) => acc + (Number(a.balance) || 0), 0)
      return sum.toLocaleString('ru-RU', { minimumFractionDigits: 4, maximumFractionDigits: 4 }).replace(/,/g, '.').replace(/\s/g, ' ')
    },
  },

  actions: {
    toggleWallet() {
      this.isWalletOpen ? this.closeWallet() : this.openWallet()
    },
    /* ---- BIP-44 аккаунты ---- */
    async loadAccounts() {
      this.accountsLoading = true
      try {
        this.accounts = await walletInvoke<WalletAccountItem[]>('list_accounts')
      } catch {
        /* нет vault / заблокирован */
      } finally {
        this.accountsLoading = false
      }
    },
    async switchAccount(index: number) {
      const next = this.accounts.find((a) => a.index === index)
      if (!next || this.accounts.find((a) => a.active)?.index === index) return
      // Оптимистично: адрес/баланс уже известны из списка — показываем мгновенно,
      // sled-запись и подгрузку истории/токенов делаем фоном.
      this.accounts = this.accounts.map((a) => ({ ...a, active: a.index === index }))
      this.account = {
        address: next.address,
        balance: next.balance,
        fiat: (Number(next.balance) * this.rate).toFixed(2),
        network: this.account?.network ?? 'MAINNET',
      }
      // сразу — снимок истории этого адреса из памяти, параллельно свежий из sled
      // (миллисекунды), не дожидаясь полного refresh (курс, токены, аккаунт…)
      this.transactions = this.txByAddr[next.address] ?? []
      this.tokens = []
      this.currentView = 'dashboard'
      const addr = next.address
      walletInvoke<WalletTx[]>('get_wallet_transactions', { force: false, address: addr })
        .then((txs) => {
          this.txByAddr[addr] = txs
          if (this.account?.address === addr) this.transactions = txs
        })
        .catch(() => {})
      try {
        await walletInvoke('set_active_account', { index })
      } catch {
        void this.loadAccounts()
        return
      }
      void this.refresh(true)
    },
    async addAccount(label?: string) {
      const a = await walletInvoke<WalletAccountItem>('add_account', { label: label ?? null })
      this.accounts = [...this.accounts, a]
      return a
    },
    async renameAccount(index: number, label: string) {
      await walletInvoke('rename_account', { index, label })
      this.accounts = this.accounts.map((a) => (a.index === index ? { ...a, label: label.trim() || `Account ${index + 1}` } : a))
    },
    setAccent(a: WalletAccent) {
      this.accent = a
      try {
        localStorage.setItem(LS_ACCENT, JSON.stringify(a))
      } catch {
        /* noop */
      }
    },
    /** Тап по балансу: STH → USDT → скрыт → STH. */
    cycleBalanceMode() {
      const order: BalanceMode[] = ['sth', 'usdt', 'hidden']
      this.balanceMode = order[(order.indexOf(this.balanceMode) + 1) % order.length]
      try {
        localStorage.setItem(LS_BALANCE_MODE, JSON.stringify(this.balanceMode))
      } catch {
        /* noop */
      }
    },
    persistNetworks() {
      try {
        localStorage.setItem(LS_NETWORKS, JSON.stringify(this.networks.filter((n) => !n.builtin)))
        localStorage.setItem(LS_ACTIVE_NET, JSON.stringify(this.activeNetworkId))
      } catch {
        /* noop */
      }
    },
    /** Сохранить сеть после пробинга `/api/node/configuration` (id = nethash). */
    addNetwork(info: Omit<SavedNetwork, 'id'>) {
      const id = info.nethash.slice(0, 16)
      const next = { ...info, id }
      const i = this.networks.findIndex((n) => n.id === id)
      if (i >= 0) this.networks.splice(i, 1, next)
      else this.networks.push(next)
      this.persistNetworks()
      return id
    },
    removeNetwork(id: string) {
      if (id === 'mainnet') return
      this.networks = this.networks.filter((n) => n.id !== id)
      if (this.activeNetworkId === id) this.activeNetworkId = 'mainnet'
      this.persistNetworks()
    },
    switchNetwork(id: string) {
      if (!this.networks.some((n) => n.id === id)) return
      this.activeNetworkId = id
      this.persistNetworks()
      this.currentView = 'dashboard'
    },
    openWallet() {
      this.isWalletOpen = true
      // панель из верхнего меню всегда открывается на главной (баланс);
      // служебные экраны (connect/authorize/verify) не сбрасываем — их ждёт dApp
      if (!['connect', 'authorize', 'verify'].includes(this.currentView)) this.currentView = 'dashboard'
      this.error = ''
      // refresh balance/history whenever the wallet is opened
      this.refresh()
      void this.loadAccounts()
    },
    closeWallet() {
      this.isWalletOpen = false
    },
    setView(v: WalletView) {
      this.error = ''
      this.currentView = v
    },
    flash(msg: string) {
      this.toast = msg
      setTimeout(() => {
        if (this.toast === msg) this.toast = ''
      }, 1900)
    },

    /** Исходящая сразу в историю как pending (Rust тоже держит её в txindex до блока). */
    pushPendingOut(id: string, recipient: string, amount: string, memo: string) {
      if (!id || this.transactions.some((t) => t.id === id)) return
      const n = Number(amount)
      this.transactions = [
        {
          id,
          type: 'out',
          amount: Number.isFinite(n) ? n.toFixed(8) : amount,
          counterparty: recipient,
          timestamp: Date.now(),
          confirmed: false,
          memo,
        },
        ...this.transactions,
      ]
    },

    /** После успешного broadcast: обновить сразу (pending в пуле) и через 9с — когда tx легла в блок (~8с). */
    async refreshAfterBroadcast() {
      await this.refresh(true)
      setTimeout(() => {
        if (useNetStore().online) this.refresh(true)
      }, 9000)
    },

    /** Fetch the account snapshot + register the dApp signature interceptor (once). */
    async init() {
      if (inited) return
      inited = true
      // restore the trusted dApp origin whitelist (connect-permission)
      try {
        this.approvedOrigins = JSON.parse(localStorage.getItem(ORIGINS_KEY) ?? '[]')
      } catch {
        this.approvedOrigins = []
      }
      // последний снимок — панель рендерится сразу, сеть догоняет фоном
      this.restoreSnapshot()
      await this.refresh()
      unlisten = await walletListen<IncomingTxRequest>('incoming-tx-request', (p) => {
        // MetaMask-style: force the drawer open onto the verification view.
        this.pendingRequest = p
        this.currentView = 'verify'
        this.isWalletOpen = true
      })
      // dApp connect (getAccount) — auto-approve known origins, else prompt.
      unlistenConnect = await walletListen<ConnectRequest>('incoming-connect-request', (c) => {
        if (this.approvedOrigins.includes(c.origin)) {
          walletInvoke('resolve_dapp_connect', { id: c.id }).catch(() => {})
          return
        }
        this.pendingConnect = c
        this.currentView = 'connect'
        this.isWalletOpen = true
      })
      // dApp `signMessage` — approval-gated by default. Если origin уже в
      // списке доверенных (пользователь при handshake `getAccount` явно нажал
      // "Подключить"), считаем что доверие распространяется и на подписи —
      // подписываем автоматически, без повторного prompt'а. Это UX-паритет с
      // расширением Core-Wallet: dApp получил доступ → dApp получает подписи.
      // Для отзыва доверия — Wallet → Приложения → DISCONNECT.
      unlistenSign = await walletListen<IncomingSignMessageRequest>(
        'incoming-sign-message-request',
        (m) => {
          console.info(
            '[wallet] signMessage request from',
            m.origin,
            '· len:',
            (m.message || '').length,
          )
          if (this.approvedOrigins.includes(m.origin)) {
            // Auto-approve — trusted origin. Overlay в dApp webview будет
            // мгновенно снят приходом `dapp-response`.
            console.info('[wallet] auto-signing for trusted origin', m.origin)
            walletInvoke('resolve_sign_message', {
              requestId: m.requestId,
              message: m.message,
            }).catch((e) => {
              console.warn('[wallet] auto-sign failed', e)
            })
            return
          }
          // Unknown origin — bring focus to main window so the user sees the
          // prompt, then show it. focus() лениво (Tauri optional API), в
          // web-preview просто noop.
          try {
            const w = (window as any).__TAURI__?.window?.getCurrentWindow?.()
            w?.setFocus?.().catch?.(() => {})
            w?.unminimize?.().catch?.(() => {})
          } catch {
            /* noop */
          }
          this.pendingSignMessage = m
          this.currentView = 'authorize'
          this.isWalletOpen = true
        },
      )
      // SHIP-44: push-события кошелька по iroh (Rust wallet_stream.rs).
      unlistenStream = await walletListen<WalletFrame>('wallet-event', (f) => this.onStreamFrame(f))
      // фоновая индексация полной истории владельца в sled — прогресс для дашборда
      unlistenIndex = await walletListen<TxIndexProgress>('txindex-progress', (p) => {
        if (p.address !== this.account?.address) return
        this.txIndex = p
        if (p.done) {
          setTimeout(() => { if (this.txIndex?.done) this.txIndex = null }, 4000)
          void this.refresh(true)
        }
      })
      // стрим мог подняться до монтирования стора — снимаем текущее состояние
      walletStreamStatus().then((st) => {
        if (!st) return
        this.streamLive = st.live
        this.streamNode = st.node
        this.streamStale = !!st.stale
        if (st.lastBlockMs) this.lastBlockAt = st.lastBlockMs
        if (st.height) this.netHeight = st.height
      })
      // keep the balance/history fresh in the background (block time ≈ 8s).
      // При живом push-стриме поллинг — лишь страховка раз в 2 минуты.
      if (refreshTimer) clearInterval(refreshTimer)
      refreshTimer = setInterval(() => {
        // offline → узлы недоступны, запрос повиснет в таймаутах — пропускаем
        if (!useNetStore().online) return
        refreshTick++
        // блоки не идут > 60 с при «живом» стриме → STALE: поллинг снова каждые 30 с
        this.streamStale = this.streamLive && this.lastBlockAt > 0 && Date.now() - this.lastBlockAt > 60_000
        if (this.streamLive && !this.streamStale && refreshTick % 4 !== 0) return
        this.refresh()
      }, 30000)
    },

    /**
     * SHIP-14: `attributes.tokens` из кадра balance — известные токены обновляем
     * мгновенно по decimals из манифеста; новый tokenId → перечитать манифесты с узла.
     */
    applyTokenFrame(list: { id: string; balance: string }[]) {
      const known = new Map(this.tokens.map((t) => [t.id, t]))
      let needManifest = false
      const next: WalletToken[] = []
      for (const { id, balance } of list) {
        if (balance === '0') continue
        const k = known.get(id)
        if (!k) {
          needManifest = true
          continue
        }
        next.push({ ...k, balance, amount: fmtTokenAmount(balance, k.decimals) })
      }
      this.tokens = next
      if (needManifest) walletInvoke<WalletToken[]>('get_wallet_tokens').then((t) => (this.tokens = t)).catch(() => {})
    },

    /** Клик по бейджу STALE: принудительный реконнект стрима + немедленный refresh. */
    async reconnectStream() {
      await walletStreamReconnect()
      void this.refresh(true)
    },

    /** Кадр стрима `sth/events/1`: баланс мгновенно, pending в истории, высота сети. */
    onStreamFrame(f: WalletFrame) {
      // любой кадр данных приходит только по живому стриму
      if (f.type !== 'stream') this.streamLive = true
      switch (f.type) {
        case 'stream':
          this.streamLive = !!f.live
          this.streamNode = f.node ?? ''
          this.streamStale = false
          this.lastBlockAt = f.live ? Date.now() : 0
          if (f.height) this.netHeight = f.height
          // стрим поднялся после обрыва — узел мог дослать пропущенное
          if (f.live) void this.refresh(true)
          break
        case 'block':
          if (f.block?.height) this.netHeight = f.block.height
          this.lastBlockAt = Date.now()
          this.streamStale = false
          break
        case 'balance': {
          if (f.wallet?.address && this.accounts.length) {
            this.accounts = this.accounts.map((a) => (a.address === f.wallet!.address && f.balanceSth ? { ...a, balance: f.balanceSth } : a))
          }
          if (!this.account || !f.balanceSth) break
          if (f.wallet?.address && f.wallet.address !== this.account.address) break
          const prev = Number(this.account.balance)
          const next = Number(f.balanceSth)
          if (Number.isFinite(prev) && Number.isFinite(next) && prev !== next) {
            this.balanceFlash = ''
            requestAnimationFrame(() => (this.balanceFlash = next > prev ? 'up' : 'down'))
            if (flashTimer) clearTimeout(flashTimer)
            flashTimer = setTimeout(() => (this.balanceFlash = ''), 1500)
            this.account = { ...this.account, balance: f.balanceSth, fiat: (next * this.rate).toFixed(2) }
          }
          if (f.tokens) this.applyTokenFrame(f.tokens)
          break
        }
        case 'mempoolTx': {
          // история — строго активного адреса; кадры суб-аккаунтов не подмешиваем
          if (f.address && f.address !== this.account?.address) break
          const it = toWalletTx(f.item)
          if (it && !this.transactions.some((t) => t.id === it.id)) this.transactions = [it, ...this.transactions]
          break
        }
        case 'tx': {
          this.lastBlockAt = Date.now()
          this.streamStale = false
          if (f.height) this.netHeight = f.height
          if (f.address && f.address !== this.account?.address) break
          const it = toWalletTx(f.item)
          if (it) {
            const rest = this.transactions.filter((t) => t.id !== it.id)
            this.transactions = [it, ...rest]
          }
          // история/баланс с узла — единый источник истины после подтверждения
          void this.refresh(true)
          break
        }
        case 'rollback':
        case 'resync':
        case 'history':
          void this.refresh(true)
          break
        case 'tx-dropped':
          // pending потеряна пулом узла (гонка nonce) — убираем из истории и говорим пользователю
          this.transactions = this.transactions.filter((t) => t.confirmed || t.id !== f.id)
          this.droppedTx = { id: f.id ?? '', reason: f.reason ?? '', at: Date.now() }
          void this.refresh(true)
          break
      }
    },

    /** force — известно о новой tx (входящая/наша отправка): история без 30с троттла. */
    async refresh(force = false) {
      // Only show the full-screen loader on the very first load — later refreshes
      // update in the background so the drawer never blanks out.
      this.loading = this.account === null
      this.error = ''
      try {
        const prevIds = new Set(this.transactions.map((t) => t.id))
        // история запрашивается ЯВНО для выбранного адреса — при переключении
        // аккаунта ответ для старого адреса отбрасывается
        const askedAddr = this.account?.address ?? ''
        // Независимые запросы: упавший курс не должен блокировать обновление
        // баланса и истории (раньше Promise.all отменял всё целиком).
        const [accountR, txR, rateR, tokR, histR, mainR] = await Promise.allSettled([
          walletInvoke<WalletAccount>('get_active_smartnet_account'),
          walletInvoke<WalletTx[]>('get_wallet_transactions', { force, address: askedAddr || null }),
          walletInvoke<{ sthUsdt: number }>('get_exchange_rate'),
          walletInvoke<WalletToken[]>('get_wallet_tokens'),
          walletInvoke<RateSample[]>('get_rate_history'),
          walletInvoke<WalletAccount>('get_main_smartnet_account'),
        ])
        if (mainR.status === 'fulfilled') this.mainAccount = mainR.value
        if (tokR.status === 'fulfilled') this.tokens = tokR.value
        if (accountR.status === 'fulfilled' && this.accounts.length) {
          const addr = accountR.value.address
          this.accounts = this.accounts.map((a) => (a.address === addr ? { ...a, balance: accountR.value.balance } : a))
        }
        if (histR.status === 'fulfilled' && histR.value.length) this.rateHistory = histR.value
        if (accountR.status === 'rejected') throw accountR.reason
        const account = accountR.value
        // пока ждали — пользователь переключил аккаунт: снимок устарел
        if (askedAddr && this.account && this.account.address !== askedAddr) return
        const transactions = txR.status === 'fulfilled' ? txR.value : this.transactions
        // Баланс изменился (и это не первая загрузка) → вспышка на главной кошелька.
        const prevBal = Number(this.account?.balance)
        const nextBal = Number(account.balance)
        if (this.account && Number.isFinite(prevBal) && Number.isFinite(nextBal) && prevBal !== nextBal) {
          this.balanceFlash = ''
          requestAnimationFrame(() => (this.balanceFlash = nextBal > prevBal ? 'up' : 'down'))
          if (flashTimer) clearTimeout(flashTimer)
          flashTimer = setTimeout(() => (this.balanceFlash = ''), 1500)
        }
        this.account = account
        this.transactions = transactions
        if (txR.status === 'fulfilled' && account.address) this.txByAddr[account.address] = transactions
        if (rateR.status === 'fulfilled') this.rate = rateR.value.sthUsdt
        this.saveSnapshot()
        // A fresh INCOMING `sub:` transfer means a new subscriber — refresh the
        // cached count immediately instead of waiting for the 180s indexer.
        if (this.account?.address) {
          const newSub = transactions.some(
            (t) => t.type === 'in' && t.memo?.startsWith('sub:') && !prevIds.has(t.id),
          )
          if (newSub && prevIds.size > 0) {
            const { refreshSubscribers } = await import('@/lib/bridge')
            refreshSubscribers(this.account.address).catch(() => {})
          }
        }
      } catch (e: any) {
        this.error = errText(e, 'Failed to load wallet')
      } finally {
        this.loading = false
      }
    },

    /** Снимок дашборда в localStorage (по идентичности) — для мгновенного открытия панели. */
    saveSnapshot() {
      const k = snapKey()
      if (!k || !this.account) return
      const snap: WalletSnapshot = {
        account: this.account,
        transactions: this.transactions.slice(0, SNAP_TX),
        rate: this.rate,
        rateHistory: this.rateHistory.slice(-120),
        tokens: this.tokens,
        accounts: this.accounts,
        netHeight: this.netHeight,
        ts: Date.now(),
      }
      try {
        localStorage.setItem(k, JSON.stringify(snap))
      } catch {
        /* quota */
      }
    },

    /** Восстановить снимок до первого сетевого ответа. false — снимка нет (первое открытие). */
    restoreSnapshot(): boolean {
      const k = snapKey()
      if (!k || this.account) return !!this.account
      try {
        const raw = localStorage.getItem(k)
        if (!raw) return false
        const s = JSON.parse(raw) as WalletSnapshot
        if (!s?.account?.address) return false
        this.account = s.account
        this.transactions = s.transactions ?? []
        if (s.account.address) this.txByAddr[s.account.address] = this.transactions
        if (s.rate) this.rate = s.rate
        if (s.rateHistory?.length) this.rateHistory = s.rateHistory
        if (s.tokens?.length) this.tokens = s.tokens
        if (s.accounts?.length) this.accounts = s.accounts
        if (s.netHeight) this.netHeight = s.netHeight
        return true
      } catch {
        return false
      }
    },

    /** Open the drawer on a LOCKED Send view to pay the app-publish fee. */
    openPublishPayment(appId: string, amountSTH: number, escrow: string) {
      this.publishCtx = {
        appId,
        amount: String(amountSTH),
        escrow,
        memo: `sth:${appId}`,
        kind: 'publish',
      }
      this.error = ''
      this.currentView = 'send'
      this.isWalletOpen = true
    },

    /** Open the drawer on a LOCKED Send view to tip a published app's boost pool. */
    openBoostPayment(appId: string, amountSTH: number, escrow: string) {
      this.publishCtx = {
        appId,
        amount: String(amountSTH),
        escrow,
        memo: `sth_boost:${appId}`,
        kind: 'boost',
      }
      this.error = ''
      this.currentView = 'send'
      this.isWalletOpen = true
    },

    /** Open the drawer on a LOCKED Send view to pay the dynamic UPDATE fee. */
    openUpdatePayment(appId: string, amountSTH: number, escrow: string, dir: string) {
      this.publishCtx = {
        appId,
        amount: String(amountSTH),
        escrow,
        memo: `sth:${appId}`,
        kind: 'update',
        dir,
      }
      this.error = ''
      this.currentView = 'send'
      this.isWalletOpen = true
    },

    /** Broadcast the locked publish/boost payment; finalize the draft on publish. */
    async confirmPublishPayment(): Promise<boolean> {
      const ctx = this.publishCtx
      if (!ctx) return false
      this.busy = true
      this.error = ''
      try {
        const res = await walletInvoke<BroadcastResult>('send_funds_request', {
          recipient: ctx.escrow,
          amount: String(ctx.amount),
          memo: ctx.memo,
        })
        if (ctx.kind === 'boost') {
          this.flash(`Boosted · ${res.id.slice(0, 10)}…`)
          this.publishCtx = null
          this.currentView = 'dashboard'
          await this.refreshAfterBroadcast()
          return true
        }
        const { useAppsStore } = await import('@/store/apps')
        const apps = useAppsStore()
        if (ctx.kind === 'update') {
          const m = await apps.updateFromPath(ctx.appId, ctx.dir || '', res.id)
          if (!m) {
            this.error = apps.lastError || 'Update failed'
            return false
          }
          this.flash(`Updated · ${res.id.slice(0, 10)}…`)
          this.publishCtx = null
          this.currentView = 'dashboard'
          await this.refreshAfterBroadcast()
          return true
        }
        const m = await apps.finalize(ctx.appId, false, res.id)
        if (!m) {
          this.error = apps.lastError || 'Publish failed'
          return false
        }
        this.flash(`Published · ${res.id.slice(0, 10)}…`)
        this.publishCtx = null
        this.currentView = 'dashboard'
        await this.refreshAfterBroadcast()
        return true
      } catch (e: any) {
        this.error = errText(e, 'Payment failed')
        return false
      } finally {
        this.busy = false
      }
    },

    cancelPublishPayment() {
      this.publishCtx = null
      this.currentView = 'dashboard'
    },

    async sendFunds(recipient: string, amount: string, memo: string): Promise<boolean> {
      this.busy = true
      this.error = ''
      try {
        const res = await walletInvoke<BroadcastResult>('send_funds_request', {
          recipient: String(recipient),
          amount: String(amount),
          memo: String(memo ?? ''),
        })
        this.flash(`Sent · ${res.id.slice(0, 10)}…`)
        this.currentView = 'dashboard'
        this.pushPendingOut(res.id, String(recipient), String(amount), String(memo ?? ''))
        await this.refreshAfterBroadcast()
        return true
      } catch (e: any) {
        this.error = errText(e, 'Transfer failed')
        return false
      } finally {
        this.busy = false
      }
    },

    async notifyExchangeDeposit(amount: string): Promise<boolean> {
      this.busy = true
      this.error = ''
      try {
        await walletInvoke('notify_exchange_deposit', { amount: String(amount) })
        this.flash('Deposit reported · awaiting bridge')
        this.startBridgeWatch()
        return true
      } catch (e: any) {
        this.error = errText(e, 'Failed to report deposit')
        return false
      } finally {
        this.busy = false
      }
    },

    async executeExchangeSell(amount: string, usdtAddress: string): Promise<boolean> {
      this.busy = true
      this.error = ''
      try {
        const r = await walletInvoke<BroadcastResult>('execute_exchange_sell', {
          amount: String(amount),
          usdtAddress: String(usdtAddress),
        })
        this.flash(`Sell submitted · ${r.id.slice(0, 10)}…`)
        await this.refreshAfterBroadcast()
        this.startBridgeWatch()
        return true
      } catch (e: any) {
        this.error = errText(e, 'Sell failed')
        return false
      } finally {
        this.busy = false
      }
    },

    /**
     * Watch for bridge settlement by polling the on-chain balance every 10s (up
     * to ~5 min). Lives in the store so it survives drawer/view remounts.
     */
    startBridgeWatch() {
      this.stopBridgeWatch()
      const baseline = Number(this.balance) || 0
      this.bridge = 'pending'
      this.bridgeTries = 0
      bridgeTimer = setInterval(async () => {
        if (!useNetStore().online) return
        this.bridgeTries += 1
        await this.refresh(true)
        if (Math.abs((Number(this.balance) || 0) - baseline) > 1e-8) {
          this.bridge = 'confirmed'
          this.stopBridgeWatch()
        } else if (this.bridgeTries >= MAX_BRIDGE_TRIES) {
          this.bridge = 'idle'
          this.stopBridgeWatch()
        }
      }, 10000)
    },
    stopBridgeWatch() {
      if (bridgeTimer) {
        clearInterval(bridgeTimer)
        bridgeTimer = null
      }
    },
    resetBridge() {
      this.stopBridgeWatch()
      this.bridge = 'idle'
      this.bridgeTries = 0
    },

    /** Approve the intercepted dApp request — delegates signing + broadcast to Rust. */
    async approve() {
      const req = this.pendingRequest
      if (!req) return
      this.busy = true
      this.error = ''
      try {
        const res = await walletInvoke<BroadcastResult>('sign_and_broadcast_tx', {
          requestId: req.requestId,
          txData: {
            recipientId: String(req.recipient),
            amount: String(req.amount),
            fee: String(req.fee),
            vendorField: String(req.memo ?? ''),
          },
        })
        this.flash(`Broadcast · ${res.id.slice(0, 10)}…`)
        this.pendingRequest = null
        // Show the wallet (not a blank loader); let the tx settle, then refresh.
        this.currentView = 'dashboard'
        void this.refreshAfterBroadcast()
      } catch (e: any) {
        this.error = errText(e, 'Signing failed')
      } finally {
        this.busy = false
      }
    },

    /** Reject the intercepted dApp request — signals failure back to Rust. */
    async reject() {
      const req = this.pendingRequest
      if (req) {
        try {
          await walletInvoke('reject_tx_request', { requestId: req.requestId })
        } catch {
          /* noop */
        }
      }
      this.pendingRequest = null
      this.currentView = 'dashboard'
    },

    /** Approve a dApp connect (getAccount) request; optionally remember the origin. */
    async approveConnect(remember = true) {
      const c = this.pendingConnect
      if (!c) return
      if (remember && !this.approvedOrigins.includes(c.origin)) {
        this.approvedOrigins.push(c.origin)
        localStorage.setItem(ORIGINS_KEY, JSON.stringify(this.approvedOrigins))
      }
      try {
        await walletInvoke('resolve_dapp_connect', { id: c.id })
      } catch {
        /* noop */
      }
      this.pendingConnect = null
      this.currentView = 'dashboard'
      this.flash('dApp connected')
    },
    async rejectConnect() {
      const c = this.pendingConnect
      if (c) {
        try {
          await walletInvoke('reject_tx_request', { requestId: c.id })
        } catch {
          /* noop */
        }
      }
      this.pendingConnect = null
      this.currentView = 'dashboard'
    },
    /** Forget a trusted dApp origin (revoke connect-permission). */
    revokeOrigin(origin: string) {
      this.approvedOrigins = this.approvedOrigins.filter((o) => o !== origin)
      localStorage.setItem(ORIGINS_KEY, JSON.stringify(this.approvedOrigins))
    },

    /**
     * Approve a `signMessage` request → Rust signs with the session key and
     * resolves the dApp Promise via the `dapp-response` event. Signatures are
     * NEVER auto-approved: this action is only reachable via the AuthorizeMessage
     * view after the user pressed "Sign".
     */
    async approveSignMessage() {
      const req = this.pendingSignMessage
      if (!req) return
      this.busy = true
      this.error = ''
      try {
        await walletInvoke('resolve_sign_message', {
          requestId: req.requestId,
          message: req.message,
        })
        this.flash('Сообщение подписано ✓')
        this.pendingSignMessage = null
        this.currentView = 'dashboard'
      } catch (e: any) {
        this.error = errText(e, 'Не удалось подписать сообщение')
      } finally {
        this.busy = false
      }
    },
    async rejectSignMessage() {
      const req = this.pendingSignMessage
      if (req) {
        try {
          await walletInvoke('reject_sign_message', { requestId: req.requestId })
        } catch {
          /* noop */
        }
      }
      this.pendingSignMessage = null
      this.currentView = 'dashboard'
    },

    dispose() {
      unlisten?.()
      unlisten = null
      unlistenConnect?.()
      unlistenConnect = null
      unlistenSign?.()
      unlistenSign = null
      unlistenStream?.()
      unlistenStream = null
      unlistenIndex?.()
      unlistenIndex = null
      inited = false
    },
  },
})
