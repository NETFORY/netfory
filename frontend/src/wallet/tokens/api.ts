// Токены SHIP-14 для Rust-кошелька: чтение через узлы кошелька (iroh/clearnet),
// подписанные операции typeGroup 2/3, локальный кэш метаданных и логотипов.
import { isTauri } from '@/lib/bridge'
import { walletInvoke } from '@/wallet/ipc'

export interface TokenFees { init: string; transfer: string; transferPerRecipient: string; mint: string; burn: string; meta: string }
export interface MarketCfg { enabled: boolean; fees: { sell: string; buy: string; cancel: string }; maxOpenOrders: number; pricePerToken: boolean }
export interface TokenConfig { enabled: boolean; fees: TokenFees; tickerFee: string; maxRecipients: number; tickerRule: RegExp; market: MarketCfg | null }
export interface TokenOrder {
  orderId: string; tokenId: string; seller: string; price: string; priceUnit: string; pricePerToken: string | null
  amount: string; filled: string; remaining: string; total: string; height: number; open: boolean
}
export interface TokenTrade { txId: string; buyer: string; seller: string; amount: string; pricePerToken: string; total: string; timestamp: number; time?: string }
export interface TokenMeta { name: string; description: string; website: string; logoType?: string; logoUrl?: string }
export interface TokenRow { id: string; symbol: string; decimals: number; balance: string; owner: boolean }
export interface TokenInfo extends TokenRow {
  supply: string; supplyCap: string; ownerAddress: string; mintable: boolean; burnable: boolean; initHeight: number; holders?: number; meta: TokenMeta | null
}
export interface TokenTx { id: string; type: number; label: string; amount: string; dir: 'in' | 'out' | 'self'; counterparty: string; timestamp: number; confirmed: boolean }

const DEF_FEES: TokenFees = { init: '500000000000', transfer: '10000000', transferPerRecipient: '1000000', mint: '100000000', burn: '10000000', meta: '10000000000' }
const SOBJ_FEE = '5000000000'
export const STH = 100_000_000n

const api = <T = any>(path: string) => walletInvoke<T>('token_api', { path })

/* ---------------- суммы: человеко-читаемые ↔ минимальные единицы (BigInt) ---------------- */
export const U64_MAX = 18446744073709551615n
export function toUnits(human: string, decimals: number): bigint | null {
  const s = human.trim().replace(',', '.')
  if (!/^\d+(\.\d+)?$/.test(s)) return null
  const [i, f = ''] = s.split('.')
  if (f.length > decimals) return null
  const v = BigInt(i + f.padEnd(decimals, '0'))
  return v > U64_MAX ? null : v
}
export function fromUnits(raw: string | bigint, decimals: number): string {
  const v = BigInt(raw || 0)
  if (!decimals) return v.toLocaleString('en-US')
  const p = 10n ** BigInt(decimals)
  const f = (v % p).toString().padStart(decimals, '0').replace(/0+$/, '')
  return (v / p).toLocaleString('en-US') + (f ? '.' + f : '')
}
export const sth = (raw: string | bigint) => fromUnits(raw, 8)

/* ---------------- кэш (localStorage) ---------------- */
const MKEY = (id: string) => `sth_tok_meta:${id}`
const LKEY = (id: string) => `sth_tok_logo:${id}`
const META_TTL = 6 * 3600_000
function cacheGet<T>(k: string, ttl = Infinity): T | null {
  try { const r = JSON.parse(localStorage.getItem(k) || 'null'); return r && Date.now() - r.at < ttl ? (r.v as T) : null } catch { return null }
}
function cacheSet(k: string, v: unknown) { try { localStorage.setItem(k, JSON.stringify({ at: Date.now(), v })) } catch { /* квота */ } }
export const cachedInfo = (id: string) => cacheGet<TokenInfo>(MKEY(id))
export const cachedLogo = (id: string) => cacheGet<string>(LKEY(id))
export function cacheLogoFromFile(id: string, dataUrl: string) { cacheSet(LKEY(id), dataUrl) }

/** Логотип: кэш → meta.logo узла → SVG /tokens/{id}/logo → последняя TokenMeta-транзакция владельца → logoUrl. */
const inflight = new Map<string, Promise<string>>()
const MISS = (id: string) => `sth_tok_logo_miss:${id}`
const b64url = (type: string | undefined, b64: string) => `data:${type === 'svg' ? 'image/svg+xml' : 'image/png'};base64,${b64}`
const safeSvg = (t: string) => /^\s*(<\?xml[^>]*>\s*)?<svg[\s>]/i.test(t) && !/<script|on\w+\s*=|javascript:/i.test(t)
export function tokenLogo(id: string, info?: TokenInfo | null): Promise<string> {
  const c = cachedLogo(id)
  if (c) return Promise.resolve(c)
  if (!isTauri) return Promise.resolve('')
  if (cacheGet<boolean>(MISS(id), 10 * 60_000)) return Promise.resolve('')
  let p = inflight.get(id)
  if (!p) {
    p = findLogo(id, info).then((d) => { if (d) cacheSet(LKEY(id), d); else cacheSet(MISS(id), true); return d }).catch(() => '').finally(() => inflight.delete(id))
    inflight.set(id, p)
  }
  return p
}
async function findLogo(id: string, info?: TokenInfo | null): Promise<string> {
  const raw = (await api(`/tokens/${id}`).catch(() => null))?.data ?? {}
  const meta = raw.meta ?? info?.meta ?? {}
  if (typeof meta.logo === 'string' && meta.logo.length > 20) return b64url(meta.logoType, meta.logo)
  const svg = await walletInvoke<string>('token_logo_text', { id }).catch(() => '')
  if (svg && safeSvg(svg)) return `data:image/svg+xml;base64,${btoa(unescape(encodeURIComponent(svg)))}`
  const owner = raw.owner ?? raw.ownerAddress ?? info?.ownerAddress
  if (owner) {
    const txs = (await api(`/transactions?typeGroup=3&type=4&senderId=${owner}&limit=100&orderBy=timestamp:desc`).catch(() => null))?.data ?? []
    const m = txs.find((t: any) => t.asset?.token?.id === id && t.asset.token.meta?.logo)?.asset.token.meta
    if (m) return b64url(m.logoType, m.logo)
  }
  const url = meta.logoUrl
  if (url && /^https?:/.test(url)) {
    const blob = await (await fetch(url)).blob()
    if (blob.size <= 16384) return await new Promise<string>((r) => { const fr = new FileReader(); fr.onload = () => r(String(fr.result)); fr.readAsDataURL(blob) })
  }
  return ''
}

/* ---------------- конфигурация сети ---------------- */
let cfg: TokenConfig | null = null
export async function tokenConfig(): Promise<TokenConfig> {
  if (cfg) return cfg
  try {
    const c = (await api('/node/configuration'))?.data?.constants ?? {}
    cfg = {
      enabled: !!c.tokens,
      fees: { ...DEF_FEES, ...(c.tokenFees ?? {}) },
      tickerFee: SOBJ_FEE,
      maxRecipients: Number(c.tokenTransferMaxRecipients ?? 64),
      tickerRule: new RegExp(c.tokenTickerRule ?? '^[A-Z0-9]{3,10}$'),
      market: c.tokenMarket?.fees
        ? { enabled: !!c.tokenMarket.enabled, fees: c.tokenMarket.fees, maxOpenOrders: Number(c.tokenMarket.maxOpenOrders ?? 64), pricePerToken: !!c.tokenMarket.pricePerToken }
        : isTauri ? null : { enabled: true, fees: { sell: '10000000', buy: '10000000', cancel: '10000000' }, maxOpenOrders: 64, pricePerToken: true },
    }
  } catch {
    cfg = { enabled: false, fees: DEF_FEES, tickerFee: SOBJ_FEE, maxRecipients: 64, tickerRule: /^[A-Z0-9]{3,10}$/, market: null }
  }
  return cfg
}

/* ---------------- чтение ---------------- */
function parseInfo(d: any, address: string, balance = '0'): TokenInfo {
  const flags = Number(d.flags ?? 0)
  return {
    id: d.id, symbol: d.symbol ?? d.ticker ?? '???', decimals: Number(d.decimals ?? 0), balance: String(d.balance ?? balance),
    supply: String(d.supply ?? '0'), supplyCap: String(d.supplyCap ?? d.supply ?? '0'), ownerAddress: d.owner ?? '',
    owner: !!address && d.owner === address, mintable: d.mintable ?? (flags & 1) === 1, burnable: d.burnable ?? (flags & 2) === 2,
    initHeight: Number(d.initHeight ?? 0), holders: d.holders != null ? Number(d.holders) : undefined, meta: d.meta ?? null,
  }
}

/** Токены адреса: на балансе + выпущенные мной (даже с нулевым балансом). */
export async function listTokens(address: string): Promise<TokenRow[]> {
  if (!isTauri) return mockList(address)
  const [held, wallet] = await Promise.all([
    api(`/wallets/${address}/tokens`).catch(() => null),
    api(`/wallets/${address}`).catch(() => null),
  ])
  const rows = new Map<string, TokenRow>()
  for (const t of held?.data ?? []) rows.set(t.id, { id: t.id, symbol: t.symbol, decimals: Number(t.decimals ?? 0), balance: String(t.balance ?? '0'), owner: false })
  const issued = wallet?.data?.attributes?.tokensIssued ?? {}
  for (const [id, t] of Object.entries<any>(issued)) {
    const r = rows.get(id)
    rows.set(id, r ? { ...r, owner: true } : { id, symbol: t.symbol, decimals: Number(t.decimals ?? 0), balance: '0', owner: true })
  }
  return [...rows.values()].sort((a, b) => Number(b.owner) - Number(a.owner) || a.symbol.localeCompare(b.symbol))
}

export async function tokenInfo(id: string, address: string, balance?: string): Promise<TokenInfo> {
  if (!isTauri) return mockInfo(id, address)
  const d = (await api(`/tokens/${id}`))?.data ?? {}
  const info = parseInfo({ id, ...d }, address, balance)
  cacheSet(MKEY(id), info)
  return info
}
export const infoStale = (id: string) => !cacheGet(MKEY(id), META_TTL)

const LABELS = ['init', 'transfer', 'mint', 'burn', 'meta', 'sell', 'buy', 'cancel', 'sell']
export interface HistTokenTx extends TokenTx { tokenId: string; symbol: string; memo: string }
function parseTx(t: any, address: string, decimals: number): TokenTx {
  const a = t.asset?.token ?? {}
  const mine = t.sender === address
  let amount = 0n, cp = ''
  if (t.type === 1) {
    for (const x of a.transfers ?? []) {
      if (mine || x.recipientId === address) { amount += BigInt(x.amount); cp = mine ? x.recipientId : t.sender }
    }
  } else if (a.amount) { amount = BigInt(a.amount); cp = a.recipientId ?? '' }
  else if (t.type === 0) amount = BigInt(a.initialSupply ?? 0)
  const dir = t.type === 1 ? (mine ? 'out' : 'in') : (t.type === 2 && a.recipientId === address && !mine) || (t.type === 6 && mine) ? 'in' : 'self'
  return { id: t.id, type: t.type, label: LABELS[t.type] ?? String(t.type), amount: fromUnits(amount, decimals), dir, counterparty: cp, timestamp: (t.timestamp?.unix ?? 0) * 1000, confirmed: (t.confirmations ?? 0) > 0 }
}
const addrTxs = (address: string) => api(`/transactions?typeGroup=3&address=${address}&limit=50&orderBy=timestamp:desc`).catch(() => null)

/** Последние операции адреса по токену (до `limit`). */
export async function tokenTxs(id: string, address: string, decimals: number, limit = 5): Promise<TokenTx[]> {
  if (!isTauri) return mockTxs(id, address, limit)
  const r = await addrTxs(address)
  return (r?.data ?? []).filter((t: any) => t.asset?.token?.id === id).slice(0, limit).map((t: any) => parseTx(t, address, decimals))
}

/** Все операции адреса по токенам — для общей истории кошелька (метаданные-only пропускаем). */
export async function historyTokenTxs(address: string): Promise<HistTokenTx[]> {
  const rows = await listTokens(address).catch(() => [] as TokenRow[])
  const by = new Map(rows.map((r) => [r.id, r]))
  if (!isTauri) return rows.flatMap((r) => mockTxs(r.id, address, 2).map((x, i) => ({ ...x, id: `${r.id.slice(0, 8)}${i}`.padEnd(64, 'e'), timestamp: x.timestamp - 1800_000, tokenId: r.id, symbol: r.symbol, memo: '' })))
  const r = await addrTxs(address)
  const out: HistTokenTx[] = []
  for (const t of r?.data ?? []) {
    const id = t.asset?.token?.id
    if (!id || t.type === 4) continue
    const row = by.get(id) ?? cachedInfo(id)
    const symbol = row?.symbol ?? t.asset?.token?.symbol ?? id.slice(0, 6)
    out.push({ ...parseTx(t, address, row?.decimals ?? 0), tokenId: id, symbol, memo: t.asset.token.memo ?? '' })
  }
  return out
}

/** Тикер занят: уже есть SObject-регистрация (type 5) или выпущенный токен с таким символом. */
export async function tickerTaken(name: string): Promise<boolean> {
  if (!isTauri) return name === 'TAKEN'
  const [sobj, tok] = await Promise.allSettled([api(`/sobj?type=5&name=${encodeURIComponent(name)}&limit=1`), api(`/tokens/${encodeURIComponent(name)}`)])
  if (tok.status === 'fulfilled' && tok.value?.data?.id) return true
  if (sobj.status === 'fulfilled') return (sobj.value?.data ?? []).some((x: any) => x.data?.name === name && !x.isResigned)
  return walletInvoke<boolean>('token_ticker_taken', { name })
}

/* ---------------- рынок (SHIP-14 market: sell / buy / cancel) ---------------- */
/** STH (smartoshi), которые платит покупатель за `amount` единиц: ceil(amount × price / priceUnit). */
export function orderCost(o: Pick<TokenOrder, 'price' | 'priceUnit'>, amount: bigint): bigint {
  const unit = BigInt(o.priceUnit || '1') || 1n
  return (amount * BigInt(o.price) + unit - 1n) / unit
}
export const perToken = (o: TokenOrder, decimals: number) => BigInt(o.pricePerToken ?? (BigInt(o.price) * 10n ** BigInt(decimals)) / (BigInt(o.priceUnit || '1') || 1n))
export async function fetchOrders(id: string): Promise<TokenOrder[]> {
  if (!isTauri) return MOCK_ORDERS(id)
  return (((await api(`/tokens/${id}/orders?limit=100`))?.data ?? []) as TokenOrder[]).filter((o) => o.open !== false)
}
export async function fetchTrades(id: string, limit = 100): Promise<TokenTrade[]> {
  if (!isTauri) return MOCK_TRADES()
  return ((await api(`/tokens/${id}/trades?limit=${limit}`))?.data ?? []) as TokenTrade[]
}
export interface TokenHolder { address: string; balance: string }
export async function fetchHolders(id: string): Promise<TokenHolder[]> {
  if (!isTauri) return [{ address: 'SMockAddressForBrowserPreview00001', balance: '700000000' }, { address: 'SNotMeAddress000000000000000000000', balance: '250000000' }, { address: 'SThirdHolder00000000000000000000aa', balance: '50000000' }]
  const r = ((await api(`/tokens/${id}/holders?limit=100`))?.data ?? []) as TokenHolder[]
  return r.filter((h) => h.balance !== '0').sort((a, b) => (BigInt(b.balance) > BigInt(a.balance) ? 1 : -1))
}
/** Последняя рыночная цена (smartoshi за 1 целый токен) с кэшем на 5 минут; null — сделок не было. */
export async function lastPrice(id: string): Promise<bigint | null> {
  const k = `sth_tok_px:${id}`
  const c = cacheGet<string>(k, 5 * 60_000)
  if (c !== null) return c ? BigInt(c) : null
  const t = (await fetchTrades(id, 1).catch(() => [])).sort((a, b) => tradeMs(b) - tradeMs(a))[0]
  const p = t?.pricePerToken && t.pricePerToken !== '0' ? t.pricePerToken : ''
  cacheSet(k, p)
  return p ? BigInt(p) : null
}
export const tradeMs = (t: TokenTrade) => (t.time ? Date.parse(t.time) : t.timestamp > 1e12 ? t.timestamp : t.timestamp * 1000)
const MOCK_ORDERS = (id: string): TokenOrder[] => [
  { orderId: 'c3'.padEnd(64, '1'), tokenId: id, seller: 'SNotMeAddress000000000000000000000', price: '150000', priceUnit: '100', pricePerToken: '150000', amount: '50000', filled: '10000', remaining: '40000', total: '75000', height: 1, open: true },
]
const MOCK_TRADES = (): TokenTrade[] => [0, 1, 2, 3].map((i) => ({ txId: `d${i}`.padEnd(64, '2'), buyer: 'SBuyer', seller: 'SSeller', amount: String(1000 * (i + 1)), pricePerToken: String(120000 + i * 15000), total: '0', timestamp: Math.floor(Date.now() / 1000) - i * 86400 }))

/** Подтверждения транзакции (0 — ещё в пуле, -1 — узел её не знает). */
export async function txConfirmations(txId: string): Promise<number> {
  if (!isTauri) return 1
  try { return Number((await api(`/transactions/${txId}`))?.data?.confirmations ?? 0) } catch { return -1 }
}

/* ---------------- операции ---------------- */
export type TokenOp = 'ticker' | 'init' | 'transfer' | 'mint' | 'burn' | 'meta' | 'sell' | 'sell_v2' | 'buy' | 'cancel'
export async function tokenOp(kind: TokenOp, asset: Record<string, unknown>, fee: string): Promise<string> {
  if (!isTauri) { await new Promise((r) => setTimeout(r, 700)); return Array.from({ length: 64 }, () => Math.floor(Math.random() * 16).toString(16)).join('') }
  const r = await walletInvoke<{ id: string }>('token_op', { kind, asset, fee })
  return r.id
}

const ERRS: Record<string, string> = {
  TokenNotSmartObjectOwnerError: 'errOwnerTicker', TokenSymbolInvalidError: 'errSymbol', TokenAlreadyInitializedError: 'errInited',
  TokenSupplyInvalidError: 'errSupply', StaticFeeMismatchError: 'errFee', TokenInsufficientBalanceError: 'errBalance',
  TokenTransferRecipientsError: 'errRecipients', TokenMemoTooLongError: 'errMemo', TokenMetaInvalidError: 'errMeta',
  TokenLogoInvalidError: 'errLogo', TokenLogoTooLargeError: 'errLogoSize', TokenNotOwnerError: 'errNotOwner',
  TokenOrderSelfTradeError: 'mErrSelf', TokenOrderClosedError: 'mErrClosed', TokenOrderNotFoundError: 'mErrClosed', TokenOrderAmountError: 'mErrAmount',
  TokenTooManyOrdersError: 'mErrTooMany', TokenOrderNotOwnerError: 'mErrNotSeller',
  'before tokens activation': 'errInactive', ERR_PENDING: 'errPending', 'Wallet is locked': 'errLocked', 'Insufficient': 'errSth',
}
/** Ошибка узла → ключ перевода (или исходный текст). */
export function errKey(e: unknown): { key?: string; raw: string } {
  const raw = String((e as any)?.message ?? e)
  const k = Object.keys(ERRS).find((x) => raw.includes(x))
  return k ? { key: ERRS[k], raw } : { raw }
}

/* ---------------- проверка логотипа ---------------- */
export async function readLogo(file: File): Promise<{ type: 'svg' | 'png'; b64: string; dataUrl: string } | string> {
  if (file.size > 8192) return 'errLogoSize'
  const buf = new Uint8Array(await file.arrayBuffer())
  const isPng = buf[0] === 0x89 && buf[1] === 0x50 && buf[2] === 0x4e && buf[3] === 0x47
  const isSvg = /\.svg$/i.test(file.name) || file.type === 'image/svg+xml'
  if (!isPng && !isSvg) return 'errLogo'
  if (isSvg && /<script|on\w+\s*=|javascript:/i.test(new TextDecoder().decode(buf))) return 'errLogo'
  let bin = ''
  buf.forEach((b) => { bin += String.fromCharCode(b) })
  const b64 = btoa(bin)
  return { type: isPng ? 'png' : 'svg', b64, dataUrl: `data:${isPng ? 'image/png' : 'image/svg+xml'};base64,${b64}` }
}
export const utf8len = (s: string) => new TextEncoder().encode(s).length

/* ---------------- браузерный мок ---------------- */
const MOCK = [
  { id: 'a1'.padEnd(64, '0'), symbol: 'COFFEE', decimals: 2, balance: '1250000', owner: true, supply: '10000000', supplyCap: '100000000', flags: 3, meta: { name: 'Coffee Points', description: 'Loyalty points for a coffee chain', website: 'https://coffee.example' } },
  { id: 'b2'.padEnd(64, '0'), symbol: 'GOLD', decimals: 8, balance: '420000000', owner: false, supply: '2100000000000000', supplyCap: '2100000000000000', flags: 2, meta: null },
]
function mockList(_a: string): TokenRow[] { return MOCK.map(({ id, symbol, decimals, balance, owner }) => ({ id, symbol, decimals, balance, owner })) }
function mockInfo(id: string, a: string): TokenInfo {
  const m = MOCK.find((x) => x.id === id) ?? MOCK[0]
  return parseInfo({ ...m, owner: m.owner ? a : 'SNotMeAddress000000000000000000000', holders: 12 }, a)
}
function mockTxs(id: string, a: string, n: number): TokenTx[] {
  const m = MOCK.find((x) => x.id === id) ?? MOCK[0]
  return Array.from({ length: n }, (_, i) => ({ id: String(i).padEnd(64, 'f'), type: i === n - 1 ? 0 : 1, label: i === n - 1 ? 'init' : 'transfer', amount: fromUnits(String(1000 * (i + 1)), m.decimals), dir: i % 2 ? 'in' : 'out', counterparty: 'SSU6TvycebBMTvc8WHiKTfG9xmX1QSniZu', timestamp: Date.now() - i * 3600_000, confirmed: i > 0 }) as TokenTx)
  void a
}
