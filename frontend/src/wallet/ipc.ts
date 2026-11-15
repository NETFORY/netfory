/*
 * SmartNet Wallet // Tauri IPC bridge + dev-mock layer
 *
 *   The thin-client wallet delegates ALL crypto / key-storage / broadcasting to
 *   the Rust core via Tauri IPC. In the browser preview (no Tauri runtime) a
 *   high-fidelity MOCK layer answers the same commands so the UI is fully
 *   testable. Native Rust commands are wired on the local Tauri build.
 *
 *   Commands (Rust contract):
 *     get_active_smartnet_account  -> WalletAccount
 *     get_wallet_transactions      -> WalletTx[]
 *     get_exchange_rate            -> { sthUsdt: number }
 *     send_funds_request   { recipient, amount, memo }      -> BroadcastResult
 *     notify_exchange_deposit { amount }                     -> BroadcastResult
 *     execute_exchange_sell  { amount, usdtAddress }         -> BroadcastResult
 *     sign_and_broadcast_tx  { requestId, txData }           -> BroadcastResult
 *     reject_tx_request      { requestId }                   -> BroadcastResult
 *
 *   Event (Rust -> UI):  incoming-tx-request  (MetaMask-style interception)
 */
import { isTauri } from '@/lib/bridge'

export interface WalletAccount {
  address: string
  balance: string // STH, formatted
  fiat: string // USDT equivalent, formatted
  network: string // e.g. "MAINNET"
}

export interface WalletTx {
  id: string
  type: 'in' | 'out'
  amount: string
  counterparty: string
  timestamp: number
  confirmed: boolean
  memo?: string
}

/** BIP-44 аккаунт сида (Rust `AccountItem`): index 0 — main, N>0 — m/44'/255'/N'/0/0. */
export interface WalletAccountItem {
  index: number
  address: string
  label: string
  main: boolean
  active: boolean
  balance: string
  path: string
}

/** SHIP-14: баланс токена кошелька (Rust `TokenBalance`). */
export interface WalletToken {
  id: string
  symbol: string
  decimals: number
  balance: string
  amount: string
  name: string
  logoUrl: string
}

export interface IncomingTxRequest {
  requestId: string
  origin: string
  recipient: string
  amount: string
  fee: string
  memo?: string
  method: 'signTransaction' | 'sendTransaction'
}

/**
 * `signMessage` intercept payload (Core-Wallet AuthorizeMessage flow).
 * The user must explicitly consent before the signature is produced.
 */
export interface IncomingSignMessageRequest {
  requestId: string
  origin: string
  message: string
}

export interface BroadcastResult {
  id: string
  accepted: boolean
}

export interface SwapQuote {
  usdtAmount: number
  estimatedSthToReceive: number
  minSthToReceive: number
  effectivePrice: number
}

export type UnlistenFn = () => void

const core = () => (window as any).__TAURI__?.core
const evt = () => (window as any).__TAURI__?.event

/** invoke a Rust command, or fall back to the dev mock in the browser. */
export async function walletInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) {
    const c = core()
    if (c?.invoke) return c.invoke(cmd, args) as Promise<T>
  }
  return mockInvoke<T>(cmd, args)
}

/** listen for a Rust event, or register a mock handler (dev helpers fire it). */
export async function walletListen<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  if (isTauri) {
    const e = evt()
    if (e?.listen) {
      const un = await e.listen(event, (m: any) => handler(m.payload as T))
      return un as UnlistenFn
    }
  }
  return mockListen<T>(event, handler)
}

/* ============================================================
 *  DEV MOCK LAYER (browser preview only)
 * ============================================================ */

const RATE = 0.00135 // 1 STH = $0.001350
const delay = (ms: number) => new Promise((r) => setTimeout(r, ms))
const randHex = (n = 32) =>
  Array.from({ length: n }, () => Math.floor(Math.random() * 16).toString(16)).join('')

/** Reuse the shell's real identity address when present so the mock feels integrated. */
function shellAddress(): string {
  try {
    const raw = localStorage.getItem('sth_vault')
    if (raw) {
      const v = JSON.parse(raw)
      if (v?.address) return v.address as string
    }
  } catch {
    /* noop */
  }
  return 'SSU6TvP9mKQ2rLxY8wN3cZ5aB4dEfHj7Xq'
}

let mockBalance = 1337.71109
let mockTxs: WalletTx[] = []
function seedTxs() {
  if (mockTxs.length) return
  const now = Date.now()
  mockTxs = [
    { id: randHex(), type: 'in', amount: '250.00000000', counterparty: 'SXdXoyUAjUyyCC5LES6KH1XS1ewbBB4Dfu', timestamp: now - 3600_000, confirmed: true, memo: 'faucet' },
    { id: randHex(), type: 'out', amount: '12.50000000', counterparty: 'SeZLuyhhYf2qxs4ArPJ71oEu3x8EsVw51C', timestamp: now - 7200_000, confirmed: true, memo: 'poker:room42:buyin' },
    { id: randHex(), type: 'in', amount: '1000.00000000', counterparty: 'SfYpsr9k2mPqxs4ArPJ71oEu3x8EsVw51C', timestamp: now - 86400_000, confirmed: true },
    { id: randHex(), type: 'out', amount: '5.25000000', counterparty: 'SgT3vXj9k2mPq7rLxY8wN3cZ5aB4dEfHjk', timestamp: now - 172800_000, confirmed: true, memo: 'swap:STH→USDT' },
  ]
}

let mockActiveIdx = 0
const mockAccounts: WalletAccountItem[] = [
  { index: 0, address: 'SbV9T9dQkR4o7Xw2mLp1Zc8HvN5aE3qH7XDaE', label: 'Account 1', main: true, active: true, balance: '1337.71109000', path: 'sha256(secret)' },
  { index: 1, address: 'SZZHEKp3Q9mN7vLw2rTc8Hx1Ya5Ub4Kd6x2th8b', label: 'Account 2', main: false, active: false, balance: '42.5', path: "m/44'/255'/1'/0/0" },
]

async function mockInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  await delay(160)
  const a = args ?? {}
  switch (cmd) {
    case 'get_active_smartnet_account':
    case 'get_main_smartnet_account':
      return { address: shellAddress(), balance: mockBalance.toFixed(8), fiat: (mockBalance * RATE).toFixed(2), network: 'MAINNET' } as T
    case 'get_wallet_transactions':
      seedTxs()
      return [...mockTxs] as T
    case 'list_accounts':
      return mockAccounts.map((a) => ({ ...a, active: a.index === mockActiveIdx })) as T
    case 'add_account': {
      const index = mockAccounts.length
      const a = { index, address: 'S' + Array.from({ length: 33 }, () => 'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz123456789'[(index * 7 + Math.floor(Math.random() * 57)) % 57]).join(''), label: String((args as any)?.label || '') || `Account ${index + 1}`, main: false, active: false, balance: '0', path: `m/44'/255'/${index}'/0/0` }
      mockAccounts.push(a)
      return { ...a } as T
    }
    case 'rename_account': {
      const a = mockAccounts.find((x) => x.index === (args as any)?.index)
      if (a) a.label = String((args as any)?.label || '') || `Account ${a.index + 1}`
      return undefined as T
    }
    case 'set_active_account':
      mockActiveIdx = Number((args as any)?.index ?? 0)
      return { ...mockAccounts[mockActiveIdx], active: true } as T
    case 'get_rate_history': {
      // 7 дней синтетики вокруг 0.00148 (в Tauri — реальные сэмплы из sled)
      const now = Date.now()
      return Array.from({ length: 84 }, (_, i) => {
        const t = now - (83 - i) * 2 * 3600 * 1000
        const r = 0.00148 * (1 + 0.06 * Math.sin(i / 9) + 0.02 * Math.sin(i / 2.3) + (i / 83) * 0.05)
        return { t, r }
      }) as T
    }
    case 'probe_network_api':
      await new Promise((r) => setTimeout(r, 600))
      return {
        api: String((args as any)?.url ?? '').replace(/\/$/, ''),
        name: 'SmartHoldem Testnet',
        nethash: 'f3a7d2c9e1b04a6f8c2d5e7b9a1c3f5d7e9b1a3c5d7f9e1b3a5c7d9f1e3b5a7c',
        token: 'TSTH',
        symbol: 'TSTH',
        explorer: 'https://testnet-explorer.smartholdem.io',
        version: 30,
        wif: 170,
        coreVersion: '3.9.0',
        height: 1204551,
      } as T
    case 'get_wallet_tokens':
      return [
        { id: 'a'.repeat(64), symbol: 'COFFEE', decimals: 2, balance: '125050', amount: '1250.5', name: 'Coffee Token', logoUrl: '' },
        { id: 'b'.repeat(64), symbol: 'NTFRY', decimals: 8, balance: '4200000000', amount: '42', name: 'Netfory Credits', logoUrl: '' },
      ] as T
    case 'get_exchange_rate':
      return { sthUsdt: RATE } as T
    case 'get_swap_quote': {
      // AMM slippage model so the browser preview behaves like the live pool.
      const usdt = Number(a.usdtAmount ?? 0)
      const effectivePrice = RATE * (1 + usdt / 4800)
      const estimatedSthToReceive = effectivePrice > 0 ? usdt / effectivePrice : 0
      const minSthToReceive = estimatedSthToReceive * 0.95
      return { usdtAmount: usdt, estimatedSthToReceive, minSthToReceive, effectivePrice } as T
    }
    case 'get_buy_deposit_address':
      return '0x6e7bf823a6d0db405dd197dc39998dc3a33c7b63' as T
    case 'get_sell_gate_address':
      return 'STHEXZ4waHd6Wwd5F6f1Xcwjne72hrVTkg' as T
    case 'send_funds_request': {
      const id = randHex()
      mockBalance -= Number(a.amount ?? 0) + 0.25
      mockTxs.unshift({ id, type: 'out', amount: String(a.amount), counterparty: String(a.recipient), timestamp: Date.now(), confirmed: true, memo: String(a.memo ?? '') })
      return { id, accepted: true } as T
    }
    case 'notify_exchange_deposit': {
      // simulate the off-chain bridge crediting STH after ~12s (BUY settlement)
      const credited = Number(a.amount ?? 0)
      setTimeout(() => {
        mockBalance += credited
        mockTxs.unshift({ id: randHex(), type: 'in', amount: String(credited), counterparty: 'BRIDGE · USDT→STH', timestamp: Date.now(), confirmed: true, memo: 'swap:USDT→STH' })
      }, 12000)
      return { id: randHex(), accepted: true } as T
    }
    case 'execute_exchange_sell': {
      const id = randHex()
      mockBalance -= Number(a.amount ?? 0) + 0.25
      mockTxs.unshift({ id, type: 'out', amount: String(a.amount), counterparty: 'GATEWAY · BEP20', timestamp: Date.now(), confirmed: true, memo: 'swap:STH→USDT' })
      return { id, accepted: true } as T
    }
    case 'sign_and_broadcast_tx': {
      const id = randHex()
      const tx = (a.txData ?? {}) as any
      mockBalance -= Number(tx.amount ?? 0) + Number(tx.fee ?? 0)
      mockTxs.unshift({ id, type: 'out', amount: String(tx.amount), counterparty: String(tx.recipientId), timestamp: Date.now(), confirmed: true, memo: String(tx.vendorField ?? '') })
      return { id, accepted: true } as T
    }
    case 'reject_tx_request':
      return { id: '', accepted: false } as T
    case 'resolve_dapp_connect':
      return undefined as T
    case 'resolve_sign_message': {
      // Web-preview: deterministic fake signature envelope so DApp flows are
      // testable end-to-end without a real Rust wallet. In native mode the
      // real bip-schnorr signature is produced by `sth_node::sign_message`.
      const msg = String((a.message ?? '') as string)
      const sig = randHex(64)
      const hash = randHex(32)
      const pubkey = randHex(33)
      // The real event flow uses `dapp-response`; the mock returns void because
      // there is no dApp bridge listener in the browser preview.
      return { address: shellAddress(), publicKey: pubkey, hash, message: msg, signature: sig } as T
    }
    case 'reject_sign_message':
      return undefined as T
    default:
      throw new Error(`mock: unknown command "${cmd}"`)
  }
}

const mockHandlers: Record<string, Array<(p: any) => void>> = {}
function mockListen<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  ;(mockHandlers[event] ||= []).push(handler as any)
  installDevHelpers()
  return Promise.resolve(() => {
    mockHandlers[event] = (mockHandlers[event] ?? []).filter((h) => h !== handler)
  })
}
function fireMock(event: string, payload: unknown) {
  ;(mockHandlers[event] ?? []).forEach((h) => h(payload))
}

/**
 * Web-preview only: donor medal tier computed from the MOCK wallet history,
 * mirroring the Rust `donor_tier` logic so the DON medal shows in the browser
 * preview after donating (the mock layer only tracks the shell's own wallet, so
 * a tier is only returned for the current user's own address).
 */
export function mockDonorTier(address: string): { tier: number; count: number; totalSth: string } {
  seedTxs()
  if (!address || address !== shellAddress()) return { tier: 0, count: 0, totalSth: '0' }
  let count = 0
  let total = 0
  for (const t of mockTxs) {
    if (t.type === 'out' && (t.memo ?? '').startsWith('donate:')) {
      count += 1
      total += Number(t.amount) || 0
    }
  }
  const tier = count > 10 && total >= 100000 ? 3 : count > 1 && total >= 10000 ? 2 : count >= 1 ? 1 : 0
  return { tier, count, totalSth: total.toFixed(0) }
}

/**
 * Browser dev helpers — call from DevTools to exercise the dApp interception
 * flow without a compiled Tauri runtime:
 *   window.__sthDevIncomingTx({ origin, recipient, amount, memo, method })
 */
function installDevHelpers() {
  const w = window as any
  if (w.__sthDevReady) return
  w.__sthDevReady = true
  w.__sthDevIncomingTx = (p: Partial<IncomingTxRequest> = {}) =>
    fireMock('incoming-tx-request', {
      requestId: randHex(8),
      origin: p.origin ?? 'https://playpoker.pro',
      recipient: p.recipient ?? 'SeZLuyhhYf2qxs4ArPJ71oEu3x8EsVw51C',
      amount: p.amount ?? '5',
      fee: p.fee ?? '0.25',
      memo: p.memo ?? 'poker:room42:buyin',
      method: p.method ?? 'sendTransaction',
    } satisfies IncomingTxRequest)
  w.__sthDevIncomingConnect = (origin = 'sth://SPj9F92YJ6ksdFcRDk7tPWD3Fvsedy6gvM') =>
    fireMock('incoming-connect-request', { id: randHex(8), origin })
  // Dev helper: fire an approval-gated `signMessage` request so the browser
  // preview can drive the AuthorizeMessage flow without a real dApp bridge.
  //   window.__sthDevIncomingSignMessage({ origin, message })
  w.__sthDevIncomingSignMessage = (p: Partial<IncomingSignMessageRequest> = {}) =>
    fireMock('incoming-sign-message-request', {
      requestId: randHex(8),
      origin: p.origin ?? 'https://playpoker.pro',
      message:
        p.message ??
        'Login to PlayPoker\nnonce: ' + randHex(8) + '\nissued: ' + new Date().toISOString(),
    } satisfies IncomingSignMessageRequest)
}
