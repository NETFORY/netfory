/*
 * SmartNet // bridge.ts
 * Hybrid persistence + crypto layer.
 *   - Tauri runtime -> Rust IPC: ALL crypto (derivation + AES) runs natively;
 *                      the decrypted seed never leaves the Rust process.
 *   - Web runtime   -> JS crypto (lib/crypto.ts) + localStorage (durable vault)
 *                      + sessionStorage (PIN held across F5).
 *
 * Environment detection per spec:
 *   const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
 */
import type { DerivedAccount } from '@/lib/crypto'
import { verifyAppManifest } from '@/lib/crypto'

export const isTauri =
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

const VAULT_KEY = 'sth_vault'
const SESSION_KEY = 'sth_session_pin'
const APPS_KEY = 'sth_apps'

export interface VaultRecord {
  cipher: string
  address: string
  publicKey: string
  accounts: number[]
  createdAt: string
}

export interface FileEntry {
  path: string
  sha256: string
  size: number
}

export interface Manifest {
  id: string
  name: string
  description?: string
  owner: string
  files: FileEntry[]
  merkle: string
  created: string
  signature: string
  publicKey: string
  path?: string
  index?: number
  version?: number
  updated?: string
  blob?: string // iroh-blobs collection hash (P2P transit address)
  tags?: string[]
  readme?: string // web-fallback only; native reads README.md from disk
  status?: 'draft' | 'published'
  paid?: boolean
  free?: boolean
  escrowTxid?: string
  srcDir?: string
  /** Приватный dApp (E2EE-контейнер + список доступа) */
  private?: boolean
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const core = (window as any).__TAURI__?.core
  if (!core?.invoke) throw new Error('Tauri core unavailable')
  return core.invoke(cmd, args) as Promise<T>
}

/* ---------------- Vault (durable) ---------------- */

export async function vaultExists(): Promise<boolean> {
  if (isTauri) return invoke<boolean>('vault_exists')
  try {
    return !!localStorage.getItem(VAULT_KEY)
  } catch {
    return false
  }
}

export async function loadVault(): Promise<VaultRecord | null> {
  if (isTauri) return invoke<VaultRecord | null>('load_vault')
  try {
    const raw = localStorage.getItem(VAULT_KEY)
    return raw ? (JSON.parse(raw) as VaultRecord) : null
  } catch {
    return null
  }
}

export async function saveVault(record: VaultRecord): Promise<void> {
  if (isTauri) {
    await invoke('save_vault', { record })
    return
  }
  localStorage.setItem(VAULT_KEY, JSON.stringify(record))
}

export async function deleteVault(): Promise<void> {
  if (isTauri) {
    await invoke('delete_vault')
    return
  }
  localStorage.removeItem(VAULT_KEY)
  clearSession()
}

/** Nuke EVERYTHING: Sled DB + data/ dirs (native) and all browser storage. */
export async function wipeAll(): Promise<void> {
  if (isTauri) {
    try {
      await invoke('wipe_all')
    } catch {
      /* still clear browser side */
    }
  }
  try {
    localStorage.clear()
    sessionStorage.clear()
  } catch {
    /* noop */
  }
}

/* --------- Session PIN (web only, survives F5) --------- */

export function setSessionPin(pin: string): void {
  try {
    sessionStorage.setItem(SESSION_KEY, pin)
  } catch {
    /* noop */
  }
}

export function getSessionPin(): string | null {
  try {
    return sessionStorage.getItem(SESSION_KEY)
  } catch {
    return null
  }
}

export function clearSession(): void {
  try {
    sessionStorage.removeItem(SESSION_KEY)
  } catch {
    /* noop */
  }
}

/* --------- Native crypto (Tauri only) --------- */

export async function nativeCreateVault(secret: string, pin: string): Promise<DerivedAccount> {
  return invoke<DerivedAccount>('create_vault', { secret, pin })
}

export async function nativeUnlockVault(pin: string): Promise<DerivedAccount | null> {
  try {
    return await invoke<DerivedAccount>('unlock_vault', { pin })
  } catch {
    return null
  }
}

export async function nativeDeriveAccount(index: number): Promise<DerivedAccount> {
  return invoke<DerivedAccount>('derive_account', { index })
}

export async function nativeRevealSeed(): Promise<string | null> {
  return invoke<string | null>('reveal_seed')
}

export async function nativeSessionClear(): Promise<void> {
  if (isTauri) await invoke('session_clear')
}

/* --------- dApp publish pipeline --------- */

export async function listApps(): Promise<Manifest[]> {
  if (isTauri) return invoke<Manifest[]>('list_apps')
  try {
    const raw = localStorage.getItem(APPS_KEY)
    return raw ? (JSON.parse(raw) as Manifest[]) : []
  } catch {
    return []
  }
}

/** Tauri: open folder picker then create a local DRAFT (copy + hash + sign). */
export async function createDraftApp(
  name: string,
  description: string,
  tags: string[],
  logo?: number[] | null,
  preview?: number[] | null,
  privateRecipients?: string[] | null,
): Promise<Manifest | null> {
  const dir = await invoke<string | null>('pick_folder')
  if (!dir) return null
  return invoke<Manifest>('create_draft_app', {
    name,
    description,
    tags,
    dir,
    logo: logo ?? null,
    preview: preview ?? null,
    privateRecipients: privateRecipients ?? null,
  })
}

/**
 * Tauri v2: create a local DRAFT directly from a KNOWN folder path — used by the
 * native OS drag-and-drop pipeline (the WebView intercepts the drop and hands us
 * the absolute path, bypassing the folder picker).
 */
export async function createDraftAtPath(
  name: string,
  description: string,
  tags: string[],
  dir: string,
  logo?: number[] | null,
  preview?: number[] | null,
  privateRecipients?: string[] | null,
): Promise<Manifest | null> {
  if (!isTauri) return null
  return invoke<Manifest>('create_draft_app', {
    name,
    description,
    tags,
    dir,
    logo: logo ?? null,
    preview: preview ?? null,
    privateRecipients: privateRecipients ?? null,
  })
}

/* ---------------- Private dApps (E2EE) ---------------- */

export const PRIVATE_DAPP_PLACEHOLDER = '🔒 Private dApp'
export const PRIVATE_REVEAL_MARKER = '§P1§'

export interface PrivateRevealView {
  allowed: boolean
  name: string
  description: string
  tags: string[]
}

/** Прогрев: расшифровать контейнер приватного dApp в RAM (no-op для публичных). */
export async function privateDappMount(id: string): Promise<void> {
  if (!isTauri) return
  await invoke('private_dapp_mount', { id })
}

/** Выгрузить plaintext приватного dApp из памяти (закрытие вкладки). */
export async function privateDappUnmount(id: string): Promise<void> {
  if (!isTauri) return
  await invoke('private_dapp_unmount', { id })
}

/** Список адресов доступа приватного dApp (только владельцу). */
export async function privateDappRecipients(id: string): Promise<string[]> {
  if (!isTauri) return []
  return invoke<string[]>('private_dapp_recipients', { id })
}

/** Изменить список доступа: ротация ключа контейнера + version++. */
export async function privateDappSetRecipients(id: string, recipients: string[]): Promise<Manifest | null> {
  if (!isTauri) return null
  return invoke<Manifest>('private_dapp_set_recipients', { id, recipients })
}

/** Проверить reveal-payload анонса: наш ли адрес в списке + расшифрованные метаданные. */
export async function privateReveal(id: string, payload: string): Promise<PrivateRevealView> {
  return invoke<PrivateRevealView>('private_reveal', { id, payload })
}

export interface DragDropPayload {
  type: 'enter' | 'over' | 'drop' | 'leave'
  paths?: string[]
}

/**
 * Subscribe to Tauri v2 NATIVE OS file-drop events on the current webview.
 * Tauri intercepts OS drops before the DOM, so the browser `@drop` event never
 * fires inside the WebView — this is the only reliable way to capture folder
 * imports. Returns an unlisten function.
 */
export async function onNativeFileDrop(
  handler: (p: DragDropPayload) => void,
): Promise<() => void> {
  if (!isTauri) return () => {}
  // Preferred: the typed webview helper exposed via withGlobalTauri.
  const getCur = (window as any).__TAURI__?.webview?.getCurrentWebview
  if (typeof getCur === 'function') {
    try {
      const cur = getCur()
      const un = await cur.onDragDropEvent((e: any) => handler(e.payload as DragDropPayload))
      return un as () => void
    } catch {
      /* fall through to raw event names */
    }
  }
  // Fallback: raw webview-scoped event channels.
  const ev = (window as any).__TAURI__?.event
  if (!ev?.listen) return () => {}
  const map: Record<string, DragDropPayload['type']> = {
    'tauri://drag-enter': 'enter',
    'tauri://drag-over': 'over',
    'tauri://drag-drop': 'drop',
    'tauri://drag-leave': 'leave',
  }
  const unsubs: Array<() => void> = []
  for (const [name, type] of Object.entries(map)) {
    const un = await ev.listen(name, (m: any) => handler({ type, paths: m?.payload?.paths }))
    unsubs.push(un as () => void)
  }
  return () => unsubs.forEach((u) => u())
}

/**
 * Toggle fullscreen on F11. Uses the standard Web Fullscreen API as the primary
 * mechanism (works in the browser AND the Tauri WebView2 runtime, and reliably
 * EXITS via `document.fullscreenElement`). Falls back to the native Tauri window
 * API only if the web request throws. Returns the resulting fullscreen state.
 */
export async function toggleFullscreen(): Promise<boolean> {
  // Primary: Web Fullscreen API (reliable enter AND exit).
  try {
    if (typeof document !== 'undefined') {
      const doc = document as any
      const fsEl = document.fullscreenElement || doc.webkitFullscreenElement
      if (fsEl) {
        await (document.exitFullscreen?.() ?? doc.webkitExitFullscreen?.())
        return false
      }
      const el = document.documentElement as any
      await (el.requestFullscreen?.() ?? el.webkitRequestFullscreen?.())
      return true
    }
  } catch {
    /* fall through to native window toggle */
  }
  // Fallback: native Tauri OS-window fullscreen.
  if (!isTauri) return false
  try {
    const getCur = (window as any).__TAURI__?.window?.getCurrentWindow
    if (typeof getCur !== 'function') return false
    const win = getCur()
    const cur = await win.isFullscreen()
    await win.setFullscreen(!cur)
    return !cur
  } catch {
    return false
  }
}

/** Finalize a draft → published. free=false needs a confirmed escrow payment txid. */
export async function finalizePublish(
  id: string,
  free: boolean,
  txid?: string,
): Promise<Manifest | null> {
  if (!isTauri) return null
  return invoke<Manifest>('finalize_publish', { id, free, txid: txid ?? null })
}

/** Free metadata edit (title/description/tags + optional PNG/SVG logo). */
export async function editAppMetadata(
  id: string,
  name: string,
  description: string,
  tags: string[],
  logo?: number[] | null,
  preview?: number[] | null,
): Promise<Manifest | null> {
  if (!isTauri) return null
  return invoke<Manifest>('edit_app_metadata', {
    id,
    name,
    description,
    tags,
    logo: logo && logo.length ? logo : null,
    preview: preview && preview.length ? preview : null,
  })
}

/** Live-preview a draft (re-ingest from the dev folder, zero cost, no announce). */
export async function previewDraft(id: string): Promise<Manifest | null> {
  if (!isTauri) return null
  try {
    return await invoke<Manifest>('preview_draft', { id })
  } catch {
    return null
  }
}

/** A dApp's logo as a base64 PNG data URL (manual upload or app-root logo.png). */
export async function appLogo(id: string): Promise<string | null> {
  if (!isTauri) return null
  try {
    return await invoke<string | null>('app_logo', { id })
  } catch {
    return null
  }
}

/** Запись из фронтенда в системный журнал (SYSLOG) — диагностика без DevTools. */
export function uiLog(scope: string, msg: string) {
  if (!isTauri) return
  invoke('ui_log', { scope, msg }).catch(() => {})
}

/** Локальный DAPP Preview 440×180 установленного dApp как data URL (или null). */
export async function appPreview(id: string): Promise<string | null> {
  if (!isTauri) return null
  try {
    return await invoke<string | null>('app_preview', { id })
  } catch {
    return null
  }
}

/** PNG-логотип НЕустановленного dApp: кэш discovery_cache/<id>/ + ленивый P2P-fetch по iroh-hash. */
export async function discoveryLogo(
  id: string,
  logoHash: string,
  nodeIds: string[],
): Promise<string | null> {
  if (!isTauri) return null
  try {
    return await invoke<string | null>('discovery_logo', { id, logoHash, nodeIds })
  } catch {
    return null
  }
}

/** DAPP Preview 440×180 НЕустановленного dApp: кэш discovery_cache/<id>/ + ленивый P2P-fetch. */
export async function discoveryPreview(
  id: string,
  previewHash: string,
  nodeIds: string[],
): Promise<string | null> {
  if (!isTauri) return null
  try {
    return await invoke<string | null>('discovery_preview', { id, previewHash, nodeIds })
  } catch {
    return null
  }
}

export interface AppPrices {
  publish: number // whole STH (alias of base)
  update: number // whole STH (legacy flat)
  basePrice: number // flat base fee component (whole STH)
  pricePerMb: number // per-MB fee component (whole STH)
  escrow: string // global escrow node address
}

/** Global publish/update prices + escrow address (blockchain config oracle). */
export async function getAppPrices(): Promise<AppPrices> {
  if (isTauri) {
    try {
      return await invoke<AppPrices>('get_app_prices')
    } catch {
      /* fall through to defaults */
    }
  }
  return { publish: 10, update: 1, basePrice: 10, pricePerMb: 2, escrow: 'SappsPqDQXCRvoi1V2Hs9xycQeT1ptJWV1' }
}

export interface PayloadInfo {
  path: string
  name: string
  sizeBytes: number
  sizeMb: number
  fileCount: number
  cost: number // dynamic operation cost in whole STH
}

/** Dynamic operation cost (whole STH) for a folder payload: base + MB·perMb. */
export async function getPayloadPricing(dir: string): Promise<number | null> {
  if (!isTauri) return null
  try {
    return await invoke<number>('get_payload_pricing', { dir })
  } catch {
    return null
  }
}

/** Inspect a folder payload (path/name/size/file-count/cost) for the UI. */
export async function inspectPayload(dir: string): Promise<PayloadInfo | null> {
  if (!isTauri) return null
  try {
    return await invoke<PayloadInfo>('inspect_payload', { dir })
  } catch {
    return null
  }
}

/** Inspect the stored source folder of an existing draft/app by id. */
export async function inspectAppPayload(id: string): Promise<PayloadInfo | null> {
  if (!isTauri) return null
  try {
    return await invoke<PayloadInfo>('inspect_app_payload', { id })
  } catch {
    return null
  }
}

/** README.md of a dApp for the ABOUT modal (native reads the verified file). */
export async function getReadme(id: string): Promise<string | null> {
  if (isTauri) {
    try {
      return await invoke<string | null>('read_readme', { id })
    } catch {
      return null
    }
  }
  const list = await listApps()
  return list.find((m) => m.id === id)?.readme || null
}

/** Web: persist a client-side manifest (hashes computed in the browser). */
export async function saveAppWeb(manifest: Manifest): Promise<void> {
  const list = await listApps()
  list.push(manifest)
  localStorage.setItem(APPS_KEY, JSON.stringify(list))
}

/** Web: replace an existing manifest (used on update). */
export async function replaceAppWeb(manifest: Manifest): Promise<void> {
  const list = (await listApps()).map((m) => (m.id === manifest.id ? manifest : m))
  localStorage.setItem(APPS_KEY, JSON.stringify(list))
}

/** Remove a published dApp (native: Sled + data/dapps; web: localStorage). */
export async function deleteApp(id: string): Promise<void> {
  if (isTauri) {
    await invoke('delete_app', { id })
    return
  }
  const list = (await listApps()).filter((m) => m.id !== id)
  localStorage.setItem(APPS_KEY, JSON.stringify(list))
}

/** Tauri: re-select a folder and re-publish to the SAME app id (new signature/version). */
export async function updateAppNative(id: string): Promise<Manifest | null> {
  return invoke<Manifest>('update_app', { id })
}

/** Tauri: re-publish an app update from a KNOWN folder path (pre-priced flow).
 *  txid оплаты обновления пишется в манифест (escrow_txid); free → paid. */
export async function updateAppAt(id: string, dir: string, txid?: string): Promise<Manifest | null> {
  if (!isTauri) return null
  return invoke<Manifest>('update_app_at', { id, dir, txid: txid || null })
}

/* --------- data folder (Tauri only) --------- */

export async function getDataDir(): Promise<string | null> {
  if (isTauri) return invoke<string>('get_data_dir')
  return null
}

export async function setDataDir(path: string): Promise<string | null> {
  if (isTauri) return invoke<string>('set_data_dir', { path })
  return null
}

export async function pickFolder(): Promise<string | null> {
  if (isTauri) return invoke<string | null>('pick_folder')
  return null
}

/** Автопоиск стандартной локальной папки OneDrive (десктоп-клиент MS её
 *  уже синхронизирует). null — папка не найдена или не десктоп-окружение. */
export async function detectOneDriveFolder(): Promise<string | null> {
  if (isTauri) return invoke<string | null>('detect_onedrive_folder')
  return null
}

/**
 * Save bytes/text to disk. In Tauri a native "Save As" dialog appears and the
 * file is written by Rust at the chosen path; in the browser it falls back to a
 * normal download. Returns the saved path/name, or null if cancelled.
 */
export async function saveFile(
  defaultName: string,
  data: Uint8Array | string,
  mime = 'application/octet-stream',
): Promise<string | null> {
  const bytes = typeof data === 'string' ? new TextEncoder().encode(data) : data
  if (isTauri) {
    let bin = ''
    const chunk = 0x8000
    for (let i = 0; i < bytes.length; i += chunk) {
      bin += String.fromCharCode(...bytes.subarray(i, i + chunk))
    }
    const b64 = btoa(bin)
    return invoke<string | null>('save_file_dialog', { defaultName, contentsB64: b64 })
  }
  const blob = new Blob([bytes], { type: mime })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = defaultName
  a.click()
  URL.revokeObjectURL(url)
  return defaultName
}

/* --------- on-chain username registry (u:// names) --------- */

export async function getMyUsername(): Promise<string | null> {
  if (!isTauri) return null
  return invoke<string | null>('get_my_username')
}

/** Имя из sled, а при отсутствии — быстрый чек исходящих tx кошелька через iroh (сеть). */
export async function recoverMyUsername(): Promise<string | null> {
  if (!isTauri) return null
  return invoke<string | null>('recover_my_username')
}

/** Ручная ресинхронизация реестра имён/подписчиков с цепи (панель безопасности). */
export async function resyncRegistry(): Promise<{ name: string | null; subscribers: number }> {
  if (!isTauri) return { name: null, subscribers: 0 }
  return invoke<{ name: string | null; subscribers: number }>('resync_registry')
}

export async function resolveUsername(name: string): Promise<string | null> {
  if (!isTauri) return null
  return invoke<string | null>('resolve_username', { name })
}

export async function usernameAvailable(name: string): Promise<boolean> {
  if (!isTauri) return !(await loadReservedDomainNames()).has(name.trim().toLowerCase())
  return invoke<boolean>('username_available', { name })
}

/** RESERVED-гейт для u://name: имя в списке reserved и адрес не из DEVELOPERS. */
export async function usernameReserved(name: string, addr = ''): Promise<boolean> {
  if (isTauri) return invoke<boolean>('username_reserved', { name })
  const set = await loadReservedDomainNames()
  return set.has(name.trim().toLowerCase()) && !isDeveloper(addr)
}

/** Ошибка tx от Rust → текст для пользователя. `insufficient-balance:<need>:<have>` — предпроверка баланса до подписи. */
export function friendlyTxError(e: unknown, fallback: string): string {
  const code = typeof e === 'string' ? e : (e as any)?.message || ''
  const m = code.match(/^insufficient-balance:([\d.]+):([\d.]+)/)
  if (m) {
    const f = (v: string) => Number(v).toLocaleString(undefined, { maximumFractionDigits: 4 })
    return `Недостаточно STH: нужно ${f(m[1])}, на балансе ${f(m[2])}. Пополните кошелёк и повторите.`
  }
  return code || fallback
}

export async function registerUsername(name: string): Promise<string> {
  if (!isTauri) throw new Error('desktop-only')
  return invoke<string>('register_username', { name })
}

/** Admin-controlled username registration fee (whole STH). Default 100. */
export async function getRegistrationFee(): Promise<number> {
  if (!isTauri) return 100
  try {
    return await invoke<number>('get_registration_fee')
  } catch {
    return 100
  }
}

/** Reverse lookup: the u:// username registered to an address, if any. */
export async function usernameOfAddress(address: string): Promise<string | null> {
  if (!isTauri || !address) return null
  try {
    return await invoke<string | null>('username_of_address', { address })
  } catch {
    return null
  }
}

export interface NameEntry {
  name: string
  address: string
  subscribers: number
  verified: boolean
  verifiedTeam: boolean
}

/** The full on-chain username registry (for the sn://nameservice directory). */
export async function listUsernames(): Promise<NameEntry[]> {
  if (!isTauri) return []
  try {
    return await invoke<NameEntry[]>('list_usernames')
  } catch {
    return []
  }
}

/* --------- paid subscriptions (follow, sub:<username>) --------- */

export interface FollowEntry {
  name: string | null
  address: string
}

export interface FollowStats {
  subscribers: number
  following: number
  recentSubscribers: FollowEntry[]
  followingList: FollowEntry[]
}

/** Admin-controlled subscription (follow) fee in whole STH. Default 1. */
export async function getSubscribePrice(): Promise<number> {
  if (!isTauri) return 1
  try {
    return await invoke<number>('get_subscribe_price')
  } catch {
    return 1
  }
}

/** Reconstruct the on-chain follow graph (subscribers + following) for an address. */
export async function followStats(address: string): Promise<FollowStats> {
  const empty: FollowStats = { subscribers: 0, following: 0, recentSubscribers: [], followingList: [] }
  if (!isTauri || !address) return empty
  try {
    return await invoke<FollowStats>('follow_stats', { address })
  } catch {
    return empty
  }
}

/** Subscribe to a user by username (pays the follow fee with vendor sub:<name>). */
export async function subscribe(username: string): Promise<string> {
  if (!isTauri) throw new Error('desktop-only')
  return invoke<string>('subscribe', { username })
}

/** Мгновенный кэш follow-статистики (последний успешный снимок, без сети). */
export async function followStatsCached(address: string): Promise<FollowStats | null> {
  if (!isTauri || !address) return null
  try {
    return await invoke<FollowStats | null>('follow_stats_cached', { address })
  } catch {
    return null
  }
}

export interface TopEntry {
  name: string
  address: string
  subscribers: number
  growth: number
  verifiedTeam: boolean
}

/** Trending-this-week leaderboard (most new subscribers in the last 7 days). */
export async function weeklyTop(limit = 3): Promise<TopEntry[]> {
  if (!isTauri) return []
  try {
    return await invoke<TopEntry[]>('weekly_top', { limit })
  } catch {
    return []
  }
}

/** Immediately refresh an address's cached subscriber count (on new sub: tx). */
export async function refreshSubscribers(address: string): Promise<number> {
  if (!isTauri || !address) return 0
  try {
    return await invoke<number>('refresh_subscribers', { address })
  } catch {
    return 0
  }
}

/* --------- E2EE P2P chat (Phase 1) --------- */

export interface ChatRoom {
  id: string
  peerAddress: string
  peerName: string | null
  peerKey: string
  lastText: string
  lastTs: number
  kind: string
  groupSecret: string | null
  unread: number
  owner: string | null
}

export interface GroupPolicy {
  entryFee: number
  paidEntry: boolean
  inviteOnly: boolean
  channelMode: boolean
  pinned?: string | null
  banned: string[]
  logoHash?: string
  logoNode?: string
  ts: number
}
export async function getGroupPolicy(roomId: string): Promise<GroupPolicy | null> {
  if (!isTauri) return null
  try {
    return await invoke<GroupPolicy | null>('get_group_policy', { roomId })
  } catch {
    return null
  }
}
export async function setGroupPolicy(
  roomId: string,
  entryFeeSth: number,
  paidEntry: boolean,
  inviteOnly: boolean,
  channelMode: boolean,
  pinned: string | null,
  banned: string[],
  logoHash: string | null = null,
  logoNode: string | null = null,
): Promise<GroupPolicy> {
  return invoke<GroupPolicy>('set_group_policy', { roomId, entryFeeSth, paidEntry, inviteOnly, channelMode, pinned, banned, logoHash, logoNode })
}

/** Владелец: сидировать логотип группы (b64 PNG/JPG ≤128КБ) → {hash, node}. */
export async function chatGroupLogoSeed(roomId: string, dataB64: string): Promise<{ hash: string; node: string }> {
  return invoke<{ hash: string; node: string }>('chat_group_logo_seed', { roomId, dataB64 })
}

/* --------- Auto-Ban + key rotation (Phases B–D) --------- */

export interface BanEntry {
  group: string
  addr: string
  until: number // epoch secs
  at: number
}

/** Owner: ban a member on-chain (BAN-tx) + rotate the group key. */
export async function banMember(roomId: string, address: string, durationSecs: number): Promise<BanEntry> {
  return invoke<BanEntry>('ban_member', { roomId, address, durationSecs })
}
/** Owner: lift a ban. */
export async function unbanMember(roomId: string, address: string): Promise<void> {
  await invoke('unban_member', { roomId, address })
}
/** Owner moderation view: all currently-active bans. */
export async function listBans(): Promise<BanEntry[]> {
  if (!isTauri) return []
  return invoke<BanEntry[]>('list_bans')
}
/** Groups where *I* am currently banned (grey/hide them). */
export async function myGroupBans(): Promise<BanEntry[]> {
  if (!isTauri) return []
  return invoke<BanEntry[]>('my_group_bans')
}
/** Self-detect bans by scanning my incoming on-chain `ban:` txs. */
export async function syncIncomingBans(): Promise<BanEntry[]> {
  if (!isTauri) return []
  return invoke<BanEntry[]>('sync_incoming_bans')
}
/** Janitor: drop expired bans + enforce the 20k sliding window. */
export async function pruneBanTable(): Promise<number> {
  if (!isTauri) return 0
  return invoke<number>('prune_ban_table')
}
export interface ChatMessage {
  id: string
  room: string
  sender: string
  text: string
  ts: number
  mine: boolean
  sys?: string | null
  /** unix-ms последнего редактирования автором */
  edited?: number | null
}

/** v1.135.217: внешний NETFORY Messenger (ntfry-messenger) — сайдкар, как Oxid Mail. */
export async function messengerAvailable(): Promise<boolean> {
  if (!isTauri) return false
  try { return await invoke<boolean>('messenger_available') } catch { return false }
}
export async function launchMessenger(): Promise<string> {
  return invoke<string>('launch_messenger')
}
export async function messengerOpen(url: string): Promise<void> {
  return invoke('messenger_open', { url })
}

export async function toggleChatWindow(target?: string): Promise<void> {
  if (!isTauri) return
  try {
    await invoke('toggle_chat_window', { target: target ?? null })
  } catch (e) {
    console.error('toggle_chat_window', e)
  }
}

/** Open (or focus) the native SystemConsole window (`?w=syscon`). */
export async function toggleConsoleWindow(): Promise<void> {
  if (!isTauri) return
  try {
    await invoke('toggle_console_window')
  } catch (e) {
    console.error('toggle_console_window', e)
  }
}
export async function getMyChatPubkey(): Promise<string | null> {
  if (!isTauri) return null
  try {
    return await invoke<string>('get_my_chat_pubkey')
  } catch {
    return null
  }
}
export async function chatKeyPublished(): Promise<boolean> {
  if (!isTauri) return true
  try {
    return await invoke<boolean>('chat_key_published')
  } catch {
    return false
  }
}
/** Статус on-chain записи ключа: опубликована ли и не устарела ли (NodeId/ключ). */
export interface ChatKeyStatus {
  published: boolean
  stale: boolean
  node: string
  publishedNode: string
}
export async function chatKeyStatus(): Promise<ChatKeyStatus> {
  if (!isTauri) return { published: true, stale: false, node: '', publishedNode: '' }
  try {
    return await invoke<ChatKeyStatus>('chat_key_status')
  } catch {
    return { published: false, stale: false, node: '', publishedNode: '' }
  }
}
/** Manual network-fee override (STH) for chat key/cost publish txs from
 *  frontend/.env (CHAT_KEY_FEE_STH). Empty = auto (backend resolves the node's
 *  recommended dynamic transfer fee from /api/node/fees). */
function keyFeeSth(): string {
  const v = (import.meta as any).env?.CHAT_KEY_FEE_STH
  return v && String(v).trim() ? String(v).trim() : ''
}

export async function publishChatKey(): Promise<string> {
  if (!isTauri) throw new Error('desktop-only')
  return invoke<string>('publish_chat_key', { feeSth: keyFeeSth() })
}

// --- Адресная книга мессенджера (обмен контактами без блокчейна) ---
export interface ContactCard {
  address: string
  username: string
  key: string
  node: string
  addedAt: number
}
export async function chatMyContactLink(): Promise<string> {
  if (!isTauri) throw new Error('desktop-only')
  return invoke<string>('chat_my_contact_link')
}
export async function chatAddContact(link: string): Promise<ContactCard> {
  if (!isTauri) throw new Error('desktop-only')
  return invoke<ContactCard>('chat_add_contact', { link })
}
export async function chatListContacts(): Promise<ContactCard[]> {
  if (!isTauri) return []
  return invoke<ContactCard[]>('chat_list_contacts')
}
export async function chatRemoveContact(address: string): Promise<void> {
  if (!isTauri) return
  return invoke('chat_remove_contact', { address })
}
/** Открыть ссылку в адресной строке ГЛАВНОГО окна (из окна мессенджера и т.п.). */
/** http(s) из другого окна (мессенджер) → новая web-вкладка NETFORY в главном окне. */
export async function openLinkInMain(url: string): Promise<void> {
  if (!isTauri) return
  return invoke('open_link_in_main', { url })
}
export async function openInMain(url: string): Promise<void> {
  if (!isTauri) return
  return invoke('open_in_main', { url })
}

/** Node's recommended dynamic transfer fee (STH string) for display. */
export async function recommendedFee(): Promise<string> {
  if (!isTauri) return '0.25'
  try {
    return await invoke<string>('recommended_fee')
  } catch {
    return '0.25'
  }
}

/** The fee (STH) a key/cost publish will use: manual .env override or auto. */
export async function estimatePublishFee(): Promise<string> {
  const override = keyFeeSth()
  if (override) return override
  return recommendedFee()
}
export async function openRoom(target: string): Promise<ChatRoom> {
  return invoke<ChatRoom>('open_room', { target })
}
/** Create a new E2EE group chat. Returns [room, inviteLink (sn://group/<secret>~<node>~<name>)]. */
export async function createGroup(name: string): Promise<[ChatRoom, string]> {
  return invoke<[ChatRoom, string]>('create_group', { name })
}
/** Join a group from an invite link `sn://group/<secret>~<node>~<name>` (idempotent). */
export async function joinGroup(link: string): Promise<ChatRoom> {
  return invoke<ChatRoom>('join_group', { link })
}
/** Shareable invite link for an existing group (embeds our current EndpointId). */
export async function groupInvite(roomId: string): Promise<string> {
  return invoke<string>('group_invite', { roomId })
}
/** Leave / delete a room (messages + metadata + gossip subscription). */
export async function deleteRoom(roomId: string): Promise<void> {
  if (!isTauri) return
  await invoke('delete_room', { roomId })
}
export async function getRooms(): Promise<ChatRoom[]> {
  if (!isTauri) return []
  try {
    return await invoke<ChatRoom[]>('get_rooms')
  } catch {
    return []
  }
}
export async function getMessages(roomId: string): Promise<ChatMessage[]> {
  if (!isTauri) return []
  try {
    return await invoke<ChatMessage[]>('get_messages', { roomId })
  } catch {
    return []
  }
}

/** Страница истории: `limit` сообщений старше `beforeTs` (undefined = хвост). */
export async function getMessagesPage(
  roomId: string,
  beforeTs: number | undefined,
  limit: number,
): Promise<{ messages: ChatMessage[]; hasMore: boolean }> {
  if (!isTauri) return { messages: [], hasMore: false }
  try {
    return await invoke<{ messages: ChatMessage[]; hasMore: boolean }>('chat_get_messages_page', {
      roomId,
      beforeTs: beforeTs ?? null,
      limit,
    })
  } catch {
    return { messages: [], hasMore: false }
  }
}
export async function sendMessage(roomId: string, text: string): Promise<ChatMessage> {
  return invoke<ChatMessage>('send_message', { roomId, text })
}

// ===== P2P-изображения мессенджера (iroh-blobs) + кэш медиафайлов =====

export async function sendChatImage(
  roomId: string,
  dataB64: string,
  thumbB64: string,
  width: number,
  height: number,
  caption: string,
): Promise<ChatMessage> {
  return invoke<ChatMessage>('chat_send_image', { roomId, dataB64, thumbB64, width, height, caption })
}

export async function sendChatAudio(
  roomId: string,
  dataB64: string,
  name: string,
  durationMs: number,
  mime: string,
  voice: boolean,
  wave: string,
): Promise<ChatMessage> {
  return invoke<ChatMessage>('chat_send_audio', { roomId, dataB64, name, durationMs, mime, voice, wave })
}

export async function loadChatImage(roomId: string, hash: string, node: string): Promise<string> {
  return invoke<string>('chat_load_image', { roomId, hash, node })
}

export async function loadChatAudio(roomId: string, hash: string, node: string, mime: string): Promise<string> {
  return invoke<string>('chat_load_audio', { roomId, hash, node, mime })
}

export async function sendChatFile(roomId: string, dataB64: string, name: string, mime: string): Promise<ChatMessage> {
  return invoke<ChatMessage>('chat_send_file', { roomId, dataB64, name, mime })
}

export async function loadChatFile(roomId: string, hash: string, node: string, mime: string): Promise<string> {
  return invoke<string>('chat_load_file', { roomId, hash, node, mime })
}

export async function chatVideoBegin(): Promise<string> {
  return invoke<string>('chat_video_begin')
}

export async function chatVideoChunk(uploadId: string, dataB64: string): Promise<number> {
  return invoke<number>('chat_video_chunk', { uploadId, dataB64 })
}

export async function chatVideoAbort(uploadId: string): Promise<void> {
  return invoke('chat_video_abort', { uploadId })
}

export async function chatVideoSend(roomId: string, uploadId: string, name: string, durMs: number, thumbB64: string, w: number, ht: number, posterB64: string): Promise<ChatMessage> {
  return invoke<ChatMessage>('chat_video_send', { roomId, uploadId, name, durMs, thumbB64, w, ht, posterB64 })
}

export async function chatReadDropped(path: string): Promise<string> {
  return invoke<string>('chat_read_dropped', { path })
}

export async function loadChatVideo(roomId: string, hash: string, node: string): Promise<string> {
  return invoke<string>('chat_load_video', { roomId, hash, node })
}
/** Отменить висящую P2P-докачку медиа (повторный клик по кружку загрузки). */
export async function chatCancelMedia(hash: string): Promise<boolean> {
  if (!isTauri) return false
  try {
    return await invoke<boolean>('chat_cancel_media', { hash })
  } catch {
    return false
  }
}
/** Живой прогресс докачки медиа: сколько байт уже передано. */
export interface MediaProgress {
  bytes: number
  done: boolean
  /** сколько источников известно (автор + реестр провайдеров + медиа-сидеры) */
  providers?: number
  /** секунд с начала докачки (0 = ещё в очереди шлюза) */
  elapsedSecs?: number
}
export async function chatMediaProgress(hash: string): Promise<MediaProgress> {
  if (!isTauri) return { bytes: 0, done: false }
  try {
    return await invoke<MediaProgress>('chat_media_progress', { hash })
  } catch {
    return { bytes: 0, done: false }
  }
}

export async function saveChatMedia(roomId: string, hash: string, node: string, name: string, mime: string): Promise<boolean> {
  return invoke<boolean>('chat_save_media', { roomId, hash, node, name, mime })
}

/** Удалить сообщение локально (у собеседников остаётся). */
/** Редактировать своё текстовое сообщение (рассылается собеседникам, sys="edit"). */
export async function chatEditMessage(roomId: string, msgId: string, text: string): Promise<ChatMessage> {
  return invoke<ChatMessage>('chat_edit_message', { roomId, msgId, text })
}
export async function chatDeleteMessage(roomId: string, msgId: string): Promise<void> {
  return invoke('chat_delete_message', { roomId, msgId })
}

export interface ChatCacheInfo {
  bytes: number
  files: number
  maxGb: number
  autoclean: boolean
}

export async function chatCacheInfo(): Promise<ChatCacheInfo> {
  return invoke<ChatCacheInfo>('chat_cache_info')
}

export async function chatCacheSet(maxGb: number, autoclean: boolean): Promise<void> {
  return invoke('chat_cache_set', { maxGb, autoclean })
}

export async function chatCacheClear(): Promise<number> {
  return invoke<number>('chat_cache_clear')
}

/// Отдельное полноэкранное окно просмотра изображения (Telegram-style).
export async function openImageWindow(roomId: string, hash: string, node: string, size = 0): Promise<void> {
  return invoke('open_image_window', { roomId, hash, node, size })
}

export async function closeImageWindow(): Promise<void> {
  return invoke('close_image_window')
}
/** Эфемерный сигнал «печатает…» в топик комнаты (не сохраняется в историю). */
export async function chatTyping(roomId: string): Promise<void> {
  if (!isTauri) return
  try {
    await invoke('chat_typing', { roomId })
  } catch {
    /* нет комнаты/оффлайн — не критично */
  }
}

/** Счётчики доставки моих сообщений комнаты: id -> кол-во получателей. */
export async function chatAcks(roomId: string): Promise<Record<string, number>> {
  if (!isTauri) return {}
  try {
    return await invoke<Record<string, number>>('chat_acks', { roomId })
  } catch {
    return {}
  }
}
export interface MsgReaction {
  e: string
  n: number
  mine: boolean
}
/** Поставить/снять реакцию на сообщение (одна на пользователя, как в Telegram). */
export async function chatReact(roomId: string, msgId: string, emoji: string): Promise<MsgReaction[]> {
  return invoke<MsgReaction[]>('chat_react', { roomId, msgId, emoji })
}
/** Реакции всех сообщений комнаты: id -> [{e, n, mine}]. */
export async function chatReactions(roomId: string): Promise<Record<string, MsgReaction[]>> {
  if (!isTauri) return {}
  try {
    return await invoke<Record<string, MsgReaction[]>>('chat_reactions', { roomId })
  } catch {
    return {}
  }
}
export async function clearChatHistory(roomId: string): Promise<void> {
  await invoke('clear_chat_history', { roomId })
}

export interface ChatSettings {
  onlyAllowTrusted: boolean
  notifications: boolean
  msgCostSth: number
  costPublished: boolean
  autoSubscribe: boolean
}
export async function getChatSettings(): Promise<ChatSettings> {
  if (!isTauri) return { onlyAllowTrusted: false, notifications: true, msgCostSth: 0, costPublished: false, autoSubscribe: true }
  try {
    return await invoke<ChatSettings>('get_chat_settings')
  } catch {
    return { onlyAllowTrusted: false, notifications: true, msgCostSth: 0, costPublished: false, autoSubscribe: true }
  }
}
export async function setChatSettings(
  onlyAllowTrusted: boolean,
  notifications: boolean,
  msgCostSth: number,
  autoSubscribe: boolean,
): Promise<void> {
  if (!isTauri) return
  await invoke('set_chat_settings', { onlyAllowTrusted, notifications, msgCostSth, autoSubscribe })
}
export async function publishMsgCost(): Promise<string> {
  if (!isTauri) throw new Error('desktop-only')
  return invoke<string>('publish_msg_cost', { feeSth: keyFeeSth() })
}
/** A peer's per-message price in STH (0 = free). */
export async function resolveMsgCost(address: string): Promise<number> {
  if (!isTauri) return 0
  try {
    return await invoke<number>('resolve_msg_cost', { address })
  } catch {
    return 0
  }
}

export interface GossipStatus {
  online: boolean
  topics: number
  p2p: boolean
  resubscribing: number
  outbox: number
}
/** Открыть аварийный журнал системным приложением. Err "crash-log-empty" — аварий нет. */
export async function openCrashLog(): Promise<void> {
  return invoke('open_crash_log')
}
/** Live status of the Iroh Gossipsub transit engine. */
export async function gossipStatus(): Promise<GossipStatus> {
  if (!isTauri) return { online: false, topics: 0, p2p: false, resubscribing: 0, outbox: 0 }
  try {
    return await invoke<GossipStatus>('gossip_status')
  } catch {
    return { online: false, topics: 0, p2p: false, resubscribing: 0, outbox: 0 }
  }
}
/** Live gossip neighbor count on a room topic (peer presence / delivery). */
export async function gossipRoomPeers(roomId: string): Promise<number> {
  if (!isTauri) return 0
  try {
    return await invoke<number>('gossip_room_peers', { roomId })
  } catch {
    return 0
  }
}

/* --------- Системный браузер (явный выбор пользователя) --------- */

/**
 * Открыть URL в системном браузере. v1.135.225: белого списка хостов больше
 * нет — любой http(s). Вызывается только по явному действию (контекстное меню
 * ссылки, кнопки «открыть в браузере»); обычные клики идут во web-вкладку.
 */
export async function openExternal(url: string): Promise<void> {
  if (!/^https?:\/\//i.test(url)) throw new Error('bad-url')
  if (!isTauri) {
    const { openNative } = await import('@/lib/clearnet')
    openNative(url)
    return
  }
  await invoke('open_external', { url, allowed: [] })
}

/* --------- profile manifest (public developer identity) --------- */

export interface ProfileManifest {
  public: {
    identity: string
    rewardAddress: string
    isPublic: boolean
    showDomains: boolean
    updatedAt: string
  }
  private: {
    encryptedSecret: string
  }
  statsCache: {
    appsPublished: number
    appsSeeded: number
  }
}

const PROFILE_KEY = 'sth_profile_manifest'

export async function loadProfileManifest(): Promise<ProfileManifest | null> {
  if (isTauri) return invoke<ProfileManifest>('load_profile_manifest')
  // Web fallback: localStorage-backed config hydrated from the browser vault.
  let stored: Partial<ProfileManifest> = {}
  try {
    const raw = localStorage.getItem(PROFILE_KEY)
    if (raw) stored = JSON.parse(raw)
  } catch {
    stored = {}
  }
  const vault = await loadVault()
  const appsRaw = localStorage.getItem('sth_apps')
  let appsPublished = 0
  try {
    appsPublished = appsRaw ? (JSON.parse(appsRaw) as unknown[]).length : 0
  } catch {
    appsPublished = 0
  }
  return {
    public: {
      identity: stored.public?.identity || vault?.address || '',
      rewardAddress: stored.public?.rewardAddress || vault?.address || '',
      isPublic: stored.public?.isPublic ?? false,
      showDomains: stored.public?.showDomains ?? false,
      updatedAt: stored.public?.updatedAt || '',
    },
    private: { encryptedSecret: vault?.cipher || '' },
    statsCache: { appsPublished, appsSeeded: appsPublished },
  }
}

export async function saveProfileConfig(
  rewardAddress: string,
  isPublic: boolean,
  showDomains: boolean,
): Promise<ProfileManifest | null> {
  if (isTauri)
    return invoke<ProfileManifest>('save_profile_config', { rewardAddress, isPublic, showDomains })
  const current = await loadProfileManifest()
  if (!current) return null
  const next: ProfileManifest = {
    ...current,
    public: {
      ...current.public,
      rewardAddress: rewardAddress.trim() || current.public.rewardAddress,
      isPublic,
      showDomains,
      updatedAt: String(Math.floor(Date.now() / 1000)),
    },
  }
  try {
    localStorage.setItem(PROFILE_KEY, JSON.stringify(next))
  } catch {
    /* quota — ignore */
  }
  return next
}

/* --------- storage quota (Tauri only) --------- */

export interface StorageUsage {
  dataTotal: number // total bytes under data/
  dappsUsed: number // bytes under data/dapps
  limit: number // configured dapps capacity (bytes)
}

export async function storageUsage(): Promise<StorageUsage | null> {
  if (isTauri) return invoke<StorageUsage>('storage_usage')
  return null
}

export async function getStorageLimit(): Promise<number> {
  if (isTauri) return invoke<number>('get_storage_limit')
  return 0
}

export interface DiskSpace {
  totalGb: number
  freeGb: number
}

/** Physical capacity (GB) of the disk hosting data/. {0,0} on web/error. */
export async function diskSpace(): Promise<DiskSpace> {
  if (isTauri) {
    try {
      return await invoke<DiskSpace>('disk_space')
    } catch {
      return { totalGb: 0, freeGb: 0 }
    }
  }
  return { totalGb: 0, freeGb: 0 }
}

export async function setStorageLimit(bytes: number): Promise<number> {
  if (isTauri) return invoke<number>('set_storage_limit', { bytes })
  return 0
}

/* --------- dApp viewer (Multi-Webview vs iframe) --------- */

export async function openAppWindow(address: string, title: string, path?: string): Promise<boolean> {
  if (isTauri) {
    await invoke('open_app_webview', { address, title, path: path || null })
    return true
  }
  return false
}

/// Всегда открыть dApp в НОВОМ отдельном окне (для клика по
/// <a target="_blank"> на внутреннюю страницу самого dApp).
export async function openAppWindowNew(address: string, title: string, path?: string): Promise<boolean> {
  if (isTauri) {
    await invoke('open_app_webview_new', { address, title, path: path || null })
    return true
  }
  return false
}

/* --------- content.json verification (content-addressed trust) --------- */

export interface VerifyResult {
  verified: boolean
  reason: string
  version?: number
}

/**
 * Verify a dApp before/while opening it.
 *  - Tauri : native `verify_app` re-hashes the files on disk (Blake3), checks
 *            the merkle root, the signature, and that id == address(pubkey).
 *  - Web   : verifies the stored manifest signature + id↔pubkey binding.
 */
export async function verifyApp(id: string): Promise<VerifyResult> {
  if (isTauri) {
    try {
      return await invoke<VerifyResult>('verify_app', { id })
    } catch {
      return { verified: false, reason: 'error' }
    }
  }
  const list = await listApps()
  const m = list.find((x) => x.id === id)
  if (!m) return { verified: false, reason: 'unknown' }
  return { ...verifyAppManifest(m), version: m.version }
}

/* --------- Embedded native child webviews (Tauri unstable) --------- */

/** Источник установки dApp (бейдж во вкладке: ☁ cloud seeder / ⇄ P2P). */
export interface InstallSource {
  kind: 'p2p' | 'seeder'
  provider?: string | null
  uri?: string | null
  node?: string | null
  at?: string | null
  version?: number | null
}

export async function appInstallSource(id: string): Promise<InstallSource | null> {
  if (!isTauri) return null
  try {
    return await invoke<InstallSource | null>('app_install_source', { id })
  } catch {
    return null
  }
}


/** Create/position a native child webview rendering sth://<address>/[path] inside the main window. */
export async function openEmbeddedWebview(
  address: string,
  x: number,
  y: number,
  width: number,
  height: number,
  visible = true,
  grayscale = false,
  path?: string,
  fresh = false,
): Promise<boolean> {
  if (!isTauri) return false
  await invoke('open_embedded_webview', { address, x, y, width, height, visible, grayscale, path: path || null, fresh })
  return true
}

/** Rust TCP-probe: фактическое состояние сети (надёжно после сна ноутбука). */
export async function netOnline(): Promise<boolean> {
  if (!isTauri) return typeof navigator !== 'undefined' ? navigator.onLine : true
  return invoke<boolean>('net_online')
}

/** Subscribe to a global Tauri event (e.g. dApp lifecycle / terminal logs). */
export async function appListen<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<() => void> {
  if (!isTauri) return () => {}
  const e = (window as any).__TAURI__?.event
  if (!e?.listen) return () => {}
  const un = await e.listen(event, (m: any) => handler(m.payload as T))
  return un as () => void
}

/** Trigger an OS-level desktop notification (used when the shell is unfocused). */
export async function showNativeNotification(title: string, body: string): Promise<void> {
  if (!isTauri) return
  try {
    await invoke('show_native_notification', { title, body })
  } catch {
    /* notification permission denied / unavailable — ignore */
  }
}

export interface SeedingConfig {
  enabled: boolean
  limitGb: number
}

/** Read the persisted "Earn STH" background-seeding config. */
export async function getSeedingConfig(): Promise<SeedingConfig> {
  if (!isTauri) return { enabled: false, limitGb: 10 }
  try {
    return await invoke<SeedingConfig>('get_seeding_config')
  } catch {
    return { enabled: false, limitGb: 10 }
  }
}

/** Диагностика heartbeat карты сети: ответ каждого трекера на последнюю отправку. */
export interface HeartbeatStatus {
  lastAttempt: number
  anyOk: boolean
  trackers: Array<[string, string]>
  enabled: boolean
  unlocked: boolean
}
export async function heartbeatStatus(): Promise<HeartbeatStatus> {
  const none: HeartbeatStatus = { lastAttempt: 0, anyOk: false, trackers: [], enabled: false, unlocked: false }
  if (!isTauri) return none
  try {
    return await invoke<HeartbeatStatus>('heartbeat_status')
  } catch {
    return none
  }
}

/** Toggle background seeding on/off + set the disk budget (GB). */
export async function toggleBackgroundSeeding(enabled: boolean, limitGb: number): Promise<void> {
  if (!isTauri) return
  await invoke('toggle_background_seeding', { enabled, limitGb })
}

export interface DailySnapshot {
  timestamp: number
  formattedDate: string // DD.MM
  avgUptimePercent: number
  estimatedEarningsSth: number
}

/** 7-day earning + uptime history for the "Earn STH" chart (zero-filled gaps). */
export async function getSeedingHistory(): Promise<DailySnapshot[]> {
  if (!isTauri) return []
  try {
    return await invoke<DailySnapshot[]>('get_seeding_history_stats')
  } catch {
    return []
  }
}

export async function setEmbeddedBounds(
  address: string,
  x: number,
  y: number,
  width: number,
  height: number,
): Promise<void> {
  if (isTauri) await invoke('set_embedded_bounds', { address, x, y, width, height })
}

export async function hideEmbeddedWebview(address: string): Promise<void> {
  if (isTauri) await invoke('hide_embedded_webview', { address })
}

export async function closeEmbeddedWebview(address: string): Promise<void> {
  if (isTauri) await invoke('close_embedded_webview', { address })
}

/** Включить/выключить перевод страницы активного DApp (DOM-перевод в webview). */
export async function dappTranslateToggle(address: string, on: boolean, target: string): Promise<void> {
  if (isTauri) await invoke('dapp_translate_toggle', { address, on, target })
}

/** Ускоритель сборки клиента: "cuda" | "metal" | "cpu". Перевод DApp — только GPU. */
export async function translatorBuildAccel(): Promise<string> {
  if (!isTauri) return 'cpu'
  return invoke<string>('translator_build_accel')
}

/** Может ли сборка реально переводить DApp локально (GPU ИЛИ bonsai-сборка на CPU). */
export async function translatorDappCapable(): Promise<boolean> {
  if (!isTauri) return false
  return invoke<boolean>('translator_dapp_capable')
}

/** Метка движка переводов для UI: "GPU CUDA" | "GPU METAL" | "CPU + BONSAI" | "CPU". */
export async function translatorEngineLabel(): Promise<string> {
  if (!isTauri) return 'CPU'
  return invoke<string>('translator_engine_label')
}

/** Настроить перевод DApp: язык + перевод по выделению (контекстное меню). */
export async function dappTranslateConfig(address: string, target: string, selection: boolean, label: string): Promise<void> {
  if (isTauri) await invoke('dapp_translate_config', { address, target, selection, label })
}

/** Browser-like nav внутри dApp child webview: back / forward / reload. */
export async function dappHistoryBack(address: string): Promise<void> {
  if (isTauri) await invoke('dapp_history_back', { address })
}
export async function dappHistoryForward(address: string): Promise<void> {
  if (isTauri) await invoke('dapp_history_forward', { address })
}
export async function dappReload(address: string): Promise<void> {
  if (isTauri) await invoke('dapp_reload', { address })
}

/** Close ALL embedded dApp child webviews — used on boot/reload to clear
 *  orphans left over after a main-window F5 (which wipes the Vue tab state). */
export async function closeAllEmbeddedWebviews(): Promise<void> {
  if (isTauri) {
    try {
      await invoke('close_all_embedded_webviews')
    } catch {
      /* noop */
    }
    try {
      await invoke('close_all_site_webviews')
    } catch {
      /* noop */
    }
  }
}

/* --------- Web-вкладки: обычные https://-сайты (websites.rs) ---------
 * Нативный child-webview без моста window.smartholdem и без IPC; label `web-<tab>`.
 */
export async function openSiteWebview(
  tab: string,
  url: string,
  x: number,
  y: number,
  width: number,
  height: number,
): Promise<void> {
  if (isTauri) await invoke('open_site_webview', { tab, url, x, y, width, height })
}
export async function siteNavigate(tab: string, url: string): Promise<void> {
  if (isTauri) await invoke('site_navigate', { tab, url })
}
export async function setSiteBounds(tab: string, x: number, y: number, width: number, height: number): Promise<void> {
  if (isTauri) await invoke('set_site_bounds', { tab, x, y, width, height })
}
export async function hideSiteWebview(tab: string): Promise<void> {
  if (isTauri) await invoke('hide_site_webview', { tab })
}
export async function closeSiteWebview(tab: string): Promise<void> {
  if (isTauri) await invoke('close_site_webview', { tab })
}
export async function siteHistoryBack(tab: string): Promise<void> {
  if (isTauri) await invoke('site_history_back', { tab })
}
export async function siteHistoryForward(tab: string): Promise<void> {
  if (isTauri) await invoke('site_history_forward', { tab })
}
export async function siteReload(tab: string): Promise<void> {
  if (isTauri) await invoke('site_reload', { tab })
}
/* --------- PQ Web: локальный SOCKS5-роутер и туннель к узлам pq-web (webproxy.rs) --------- */
export interface WebproxyBundle { name: string; hosts: string[]; mode: 'auto' | 'relay' | 'direct'; route: 'direct' | 'relay' | null; reason: string }
export interface WebproxyNode { id: string; region: string; source: 'manual' | 'tracker' | string; verified: boolean; addr: string; open: boolean; ipv6: boolean; ok: boolean; rttMs: number; checkedAt: number; error: string }
export interface WebproxyPqNode { id: string; region: string; verified?: boolean; ver?: string; addrs?: string[]; relay?: string; open?: boolean; ipv6?: boolean; mycelium?: boolean }
export interface WebproxyAuto { nodes: WebproxyPqNode[]; signed: boolean; tracker: string; at: number; stale: boolean; error: string }
export interface WebproxyStatus {
  enabled: boolean
  port: number
  bundles: WebproxyBundle[]
  nodes: WebproxyNode[]
  active: Record<string, string>
  /** host → 'v6' | 'v4': семейство, которым узел pq-web вышел к сайту (pq-web ≥0.5.0) */
  activeFam: Record<string, string>
  manualNodes: WebproxyPqNode[]
  auto: WebproxyAuto
  total: boolean
  /** фильтр egress-семейства узлов в dVPN: off | prefer (по умолчанию) | only */
  v6Mode: 'off' | 'prefer' | 'only'
}
export const EMPTY_WEBPROXY_STATUS: WebproxyStatus = {
  enabled: false, port: 0, bundles: [], nodes: [], active: {}, activeFam: {}, manualNodes: [], auto: { nodes: [], signed: false, tracker: '', at: 0, stale: false, error: '' }, total: false, v6Mode: 'prefer',
}
export async function webproxyStatus(): Promise<WebproxyStatus> {
  if (!isTauri) return webproxyMock
  try { return await invoke<WebproxyStatus>('webproxy_status') } catch { return EMPTY_WEBPROXY_STATUS }
}
// mock для web-preview (без Tauri): демонстрация карточки PQ Web
const webproxyMock: WebproxyStatus = {
  enabled: true,
  port: 41337,
  bundles: [
    { name: 'iptest', hosts: ['2ip.ru', '*.2ip.ru', '2ip.io'], mode: 'relay', route: 'relay', reason: 'mode' },
    { name: 'telegram', hosts: ['telegram.org', '*.telegram.org', 't.me'], mode: 'auto', route: null, reason: '' },
    { name: 'x', hosts: ['x.com', '*.x.com', 'twitter.com', '*.twimg.com'], mode: 'auto', route: 'direct', reason: 'ok' },
    { name: 'youtube', hosts: ['youtube.com', '*.youtube.com', '*.googlevideo.com'], mode: 'auto', route: 'relay', reason: 'tls-stall' },
  ],
  nodes: [
    { id: 'k6bqhx3m4yt7n2e4vj7zr5xq3a8wd2f9hc4n6p8s1t3u5v7w9y2z4', region: 'NL', source: 'tracker', verified: true, addr: '', open: true, ipv6: true, ok: true, rttMs: 61, checkedAt: 0, error: '' },
    { id: 'p2w4y6a8c1e3g5i7k9m2o4q6s8u1w3y5a7c9e2g4i6k8m1o3q5s7u9', region: 'FI', source: 'tracker', verified: false, addr: '', open: false, ipv6: false, ok: true, rttMs: 84, checkedAt: 0, error: '' },
    { id: 'z9x7v5t3r1p8n6l4j2h9f7d5b3a1c8e6g4i2k9m7o5q3s1u8w6y4a2', region: 'DE', source: 'manual', verified: false, addr: '', open: false, ipv6: false, ok: false, rttMs: 0, checkedAt: 0, error: 'connect timeout' },
  ],
  active: {},
  activeFam: {},
  manualNodes: [{ id: 'z9x7v5t3r1p8n6l4j2h9f7d5b3a1c8e6g4i2k9m7o5q3s1u8w6y4a2', region: 'DE' }],
  auto: {
    nodes: [
      { id: 'k6bqhx3m4yt7n2e4vj7zr5xq3a8wd2f9hc4n6p8s1t3u5v7w9y2z4', region: 'NL', verified: true, ver: '0.2.0' },
      { id: 'p2w4y6a8c1e3g5i7k9m2o4q6s8u1w3y5a7c9e2g4i6k8m1o3q5s7u9', region: 'FI', verified: false, ver: '0.2.0' },
    ],
    signed: true,
    tracker: 'http://159.69.188.60:7374',
    at: Math.floor(Date.now() / 1000) - 240,
    stale: true,
    error: '',
  },
  total: true,
  v6Mode: 'prefer',
}
export async function webproxySetMode(bundle: string, mode: string): Promise<void> {
  if (isTauri) await invoke('webproxy_set_mode', { bundle, mode })
}
export async function webproxySetNodes(nodes: WebproxyPqNode[]): Promise<void> {
  if (isTauri) await invoke('webproxy_set_nodes', { nodes })
}
/* ---------------- Mycelium (mushroom-маршрутизация, NIP-1, experimental) ---------------- */
export interface MyceliumSettings {
  enabled: boolean
  allowDirect: boolean
  voice: boolean
  clientOnly: boolean
  bandwidthKb: number
  hops: number
  bootstrap: string[]
  /** маскирующий трафик (PADDING на простаивающих цепочках) */
  coverTraffic: boolean
}
export interface MyceliumRuntime {
  enabled: boolean
  circuitsReady: number
  circuitsBuilding: boolean
  relaysKnown: number
  guards: string[]
  hops: number
  relayEnabled: boolean
  relayCircuitsActive: number
  relayCircuitsTotal: number
  relayCells: number
  relayBytes: number
  exitStreams: number
  lastError: string
  lastErrorAgeSecs?: number
  /** Фаза 2: скрытый приём и маскирующий трафик */
  hidden?: boolean
  introReady?: number
  servicesKnown?: number
  coverTraffic?: boolean
  relayIntroActive?: number
  relayRendezvousTotal?: number
  /** разных релеев, пригодных для пути (без себя) */
  relaysUsable?: number
  /** наименьшая длина живой цепочки (0 — цепочек нет) */
  hopsEffective?: number
  /** релеев меньше, чем хопов: цепочки короче настроенной длины */
  hopsReduced?: boolean
}
export interface MyceliumCircuit {
  id: number
  path: string[]
  /** RTT от клиента до хопа (накопительно), мс; null — ещё не измерено */
  hopRttMs: (number | null)[]
  /** хоп отвечает на PING; false — участок до него мёртв (красный) */
  hopAlive?: boolean[]
  target: string | null
  e2eMs: number | null
  e2eAgeSecs: number | null
  ageSecs: number
  cellsIn: number
  cellsOut: number
  /** автоматических пересборок подряд (лимит MYCELIUM_AUTO_REBUILD_MAX) */
  retries?: number
  /** прямо сейчас строится замена */
  rebuilding?: boolean
  /** мёртвый хоп или слишком медленный участок */
  degraded?: boolean
  /** exit | intro | rendezvous | service (Фаза 2) */
  kind?: 'exit' | 'intro' | 'rendezvous' | 'service' | string
}
/** Сколько раз Mycelium пересобирает деградировавшую цепочку сам, прежде чем ждать ручной кнопки. */
export const MYCELIUM_AUTO_REBUILD_MAX = 3
export interface MyceliumInfo {
  settings: MyceliumSettings | null
  status: MyceliumRuntime | null
  started: boolean
  /** отдельный endpoint Mycelium поднят (поднимается при включении) */
  running?: boolean
  mobile: boolean
  nodeId: string
  /** EndpointId транспорта Mycelium (свой endpoint, изолирован от кошелька и чата) */
  transportId?: string
  systemDaemon: boolean
  socksPort: number
  dvpnPaused: boolean
  circuits?: MyceliumCircuit[]
  /** Фаза 3: системный VPN (TUN на всём устройстве) */
  vpn?: MyceliumVpn
}
export interface MyceliumVpn {
  enabled: boolean
  running: boolean
  error: string
  supported: boolean
  helperFound: boolean
  socksPort: number
  bypass: number
  tx: number
  rx: number
  upSecs: number
  sessions: number
}
const VPN_OFF: MyceliumVpn = { enabled: false, running: false, error: '', supported: true, helperFound: true, socksPort: 0, bypass: 0, tx: 0, rx: 0, upSecs: 0, sessions: 0 }
export const DEFAULT_MYCELIUM_SETTINGS: MyceliumSettings = { enabled: false, allowDirect: false, voice: false, clientOnly: false, bandwidthKb: 2048, hops: 3, bootstrap: [], coverTraffic: false }
// мок для web-preview: имитация построения цепочек после включения
const MYC_MOCK_KEY = 'mock_mycelium_settings'
function myceliumMockLoad(): MyceliumSettings {
  try { return { ...DEFAULT_MYCELIUM_SETTINGS, ...JSON.parse(localStorage.getItem(MYC_MOCK_KEY) || '{}') } } catch { return { ...DEFAULT_MYCELIUM_SETTINGS } }
}
let myceliumMock: MyceliumInfo = { settings: myceliumMockLoad(), status: null, started: true, mobile: false, nodeId: '5f3a'.padEnd(64, 'e'), systemDaemon: false, socksPort: 9150, dvpnPaused: false, vpn: { ...VPN_OFF } }
let myceliumVpnAt = 0
let myceliumMockOnAt = 0
function myceliumMockStatus(): MyceliumInfo {
  const s = myceliumMock.settings!
  const age = Date.now() - myceliumMockOnAt
  const ready = s.enabled ? (age > 2500 ? 3 : age > 1200 ? 1 : 0) : 0
  // 1–2 bootstrap-релея в настройках = «релеев мало»: цепочки короче (адаптивная длина)
  const few = s.bootstrap.length > 0 && s.bootstrap.length < s.hops
  const eff = few ? s.bootstrap.length : s.hops
  myceliumMock.running = s.enabled
  myceliumMock.transportId = s.enabled ? '7c1e'.padEnd(64, 'b') : ''
  myceliumMock.status = {
    relaysUsable: s.enabled ? (few ? s.bootstrap.length : 7) : 0, hopsEffective: ready ? eff : 0, hopsReduced: !!ready && few,
    enabled: s.enabled, circuitsReady: ready, circuitsBuilding: s.enabled && ready < 3, relaysKnown: s.enabled ? 7 : 0,
    guards: s.enabled ? ['a1b2c3d4'.padEnd(64, '0'), 'b2c3d4e5'.padEnd(64, '1'), 'c3d4e5f6'.padEnd(64, '2')] : [], hops: s.hops,
    relayEnabled: s.enabled && !s.clientOnly, relayCircuitsActive: s.enabled ? 2 : 0, relayCircuitsTotal: s.enabled ? 14 : 0,
    relayCells: s.enabled ? 48213 : 0, relayBytes: s.enabled ? 48213 * 2048 : 0, exitStreams: s.enabled ? 31 : 0, lastError: '',
    hidden: true, introReady: s.enabled && age > 2000 ? 2 : 0, servicesKnown: s.enabled ? 4 : 0, coverTraffic: s.coverTraffic, relayIntroActive: s.enabled ? 1 : 0, relayRendezvousTotal: s.enabled ? 6 : 0,
  }
  const v = myceliumMock.vpn!
  if (v.enabled) {
    const up = Math.floor((Date.now() - myceliumVpnAt) / 1000)
    v.running = up >= 3
    v.upSecs = v.running ? up - 3 : 0
    v.tx = v.running ? v.upSecs * 18_400 : 0
    v.rx = v.running ? v.upSecs * 71_200 : 0
    v.sessions = v.running ? 4 + Math.floor(v.upSecs / 2) : 0
  }
  return { ...myceliumMock, circuits: ready ? mockCircuits(ready, eff) : [] }
}
/** Ошибка включения VPN из меню трея (событие `mycelium-vpn-error`). */
export async function onMyceliumVpnError(cb: (error: string) => void): Promise<() => void> {
  return appListen<{ error: string }>('mycelium-vpn-error', (p) => cb(p?.error ?? 'VPN error'))
}
/** Фаза 3: системный VPN (TUN) — включение запрашивает права администратора. */
export async function myceliumVpnSet(on: boolean): Promise<MyceliumInfo | null> {
  if (!isTauri) {
    const s = myceliumMock.settings!
    if (on && !s.enabled) throw new Error('turn Mycelium on first')
    if (on && (myceliumMock.status?.circuitsReady ?? 0) === 0) throw new Error('no circuits yet — wait until Mycelium is active')
    myceliumVpnAt = on ? Date.now() : 0
    myceliumMock.vpn = { ...VPN_OFF, enabled: on, bypass: on ? 5 : 0, socksPort: on ? 41873 : 0 }
    return myceliumMockStatus()
  }
  return await invoke<MyceliumInfo>('mycelium_vpn_set', { on })
}
// мок карты: накопительные RTT с дрожанием, первая цепочка — изолированная под собеседника;
// вторая (пока не «пересобрана») деградировала: Middle молчит, авто-попытки исчерпаны → видна кнопка
let mockDegradedGen = 0
let mockRebuiltAt = 0
export function mockCircuits(n: number, hops: number): MyceliumCircuit[] {
  const base = [38, 52, 61]
  return Array.from({ length: n }, (_, i) => {
    let acc = 0
    const rtt: (number | null)[] = base.slice(0, hops).map((b) => (acc += Math.round(b * (0.8 + Math.random() * 0.5) + i * 7)))
    const alive = Array.from({ length: hops }, () => true)
    const degraded = i === 1 && hops > 1 && mockRebuiltAt === 0
    if (degraded) {
      alive[1] = false
      for (let k = 1; k < rtt.length; k++) rtt[k] = (rtt[k] ?? 0) + 940
    }
    const rebuilding = i === 1 && mockRebuiltAt > 0 && Date.now() - mockRebuiltAt < 1800
    return {
      id: 0x80000000 + i * 977 + (i === 1 ? mockDegradedGen * 13 : 0), path: Array.from({ length: hops }, (_, h) => `${'abcdef'[(i + h + (i === 1 ? mockDegradedGen : 0)) % 6]}${i}${h}`.padEnd(64, String((i + h) % 10))),
      hopRttMs: rtt, hopAlive: alive, target: i === 0 ? '9d41'.padEnd(64, '7') : null,
      e2eMs: i === 0 ? (rtt[rtt.length - 1] ?? 0) + 70 + Math.round(Math.random() * 40) : null, e2eAgeSecs: i === 0 ? 12 : null,
      ageSecs: 140 + i * 60, cellsIn: 120 + i, cellsOut: 98 + i,
      retries: degraded ? MYCELIUM_AUTO_REBUILD_MAX : 0, rebuilding, degraded,
      kind: i === 0 ? 'rendezvous' : i === 2 ? 'intro' : 'exit',
    }
  })
}
/** Пересобрать цепочку (кнопка «Построить новую цепочку»); возвращает снимок новой цепочки. */
export async function myceliumRebuild(circuitId: number): Promise<MyceliumCircuit> {
  if (!isTauri) {
    mockRebuiltAt = Date.now()
    await new Promise((r) => setTimeout(r, 1500))
    mockDegradedGen += 1
    return mockCircuits(2, myceliumMock.settings?.hops ?? 3)[1]
  }
  return await invoke<MyceliumCircuit>('mycelium_rebuild', { circuitId })
}
/** Цепочка собеседника личного чата; null — Mycelium выкл / цепочки ещё нет. */
export async function myceliumCircuit(peerAddress: string): Promise<MyceliumCircuit | null> {
  if (!isTauri) return myceliumMock.settings?.enabled ? mockCircuits(1, myceliumMock.settings.hops)[0] : null
  try { return await invoke<MyceliumCircuit | null>('mycelium_circuit', { peerAddress }) } catch { return null }
}
/** Диагностика соединений iroh одного endpoint'а (main / media / mycelium). */
export interface NetEpStats {
  name: 'main' | 'media' | 'mycelium' | string
  nodeId: string
  up: boolean
  open: number
  openedMin: number
  closedMin: number
  relayChangesMin: number
  holepunchMin: number
  direct: number
  homeRelay: string
}
export interface NetConnStats {
  endpoints: NetEpStats[]
  connectErrorsMin: number
  relayErrorsMin: number
  lastError: string
  /** шторм соединений: много открытий/ошибок реле за минуту */
  storm: boolean
  windowSecs: number
}
let netStatsMockAt = 0
/** Открытые iroh-соединения и ошибки реле за минуту — ловим «шторм соединений». */
export async function netConnStats(): Promise<NetConnStats | null> {
  if (!isTauri) {
    if (!netStatsMockAt) netStatsMockAt = Date.now()
    const myOn = !!myceliumMock.settings?.enabled
    const j = (n: number) => n + Math.floor(Math.random() * 3)
    const ep = (name: string, id: string, open: number, opened: number, up = true): NetEpStats => ({ name, nodeId: up ? id.padEnd(64, '3') : '', up, open: up ? open : 0, openedMin: up ? j(opened) : 0, closedMin: up ? j(Math.max(0, opened - 1)) : 0, relayChangesMin: 0, holepunchMin: up ? j(1) : 0, direct: up ? open + 4 : 0, homeRelay: up ? 'https://relay-ru1.smartholdem.io/' : '' })
    return { endpoints: [ep('main', '5f3a', 9, 4), ep('media', '91be', 2, 1), ep('mycelium', '7c1e', 3, 2, myOn)], connectErrorsMin: j(0), relayErrorsMin: 0, lastError: '', storm: false, windowSecs: Math.min(60, Math.max(5, Math.round((Date.now() - netStatsMockAt) / 1000))) }
  }
  try { return await invoke<NetConnStats>('net_conn_stats') } catch { return null }
}
/** null — команда недоступна (standalone-мессенджер / старый бэкенд). */
export async function myceliumStatus(): Promise<MyceliumInfo | null> {
  if (!isTauri) return myceliumMockStatus()
  try { return await invoke<MyceliumInfo>('mycelium_status') } catch { return null }
}
export async function myceliumSet(settings: MyceliumSettings): Promise<MyceliumInfo | null> {
  if (!isTauri) {
    const was = myceliumMock.settings!.enabled
    if (settings.enabled && !was) {
      myceliumMockOnAt = Date.now()
      if (webproxyMock.total) { myceliumMock.dvpnPaused = true; webproxyMock.total = false }
    }
    if (!settings.enabled && was && myceliumMock.dvpnPaused) { myceliumMock.dvpnPaused = false; webproxyMock.total = true }
    if (!settings.enabled && was) { myceliumMock.vpn = { ...VPN_OFF }; myceliumVpnAt = 0 }
    myceliumMock = { ...myceliumMock, settings: { ...settings, voice: false } }
    try { localStorage.setItem(MYC_MOCK_KEY, JSON.stringify(myceliumMock.settings)) } catch { /* noop */ }
    return myceliumMockStatus()
  }
  return await invoke<MyceliumInfo>('mycelium_set', { settings })
}

export async function webproxySetTotal(on: boolean): Promise<void> {
  if (isTauri) await invoke('webproxy_set_total', { on })
  else webproxyMock.total = on
}
export async function webproxySetV6(mode: 'off' | 'prefer' | 'only'): Promise<void> {
  if (isTauri) await invoke('webproxy_set_v6', { mode })
  else webproxyMock.v6Mode = mode
}
export async function webproxySetEnabled(enabled: boolean): Promise<void> {
  if (isTauri) await invoke('webproxy_set_enabled', { enabled })
}
export async function webproxyRecheck(): Promise<void> {
  if (isTauri) await invoke('webproxy_recheck')
}

/** Открыть папку «Загрузки» с файлом, скачанным со https-сайта. */
export async function siteRevealDownload(path: string): Promise<void> {
  if (isTauri) await invoke('site_reveal_download', { path })
}

/* --------- P2P LAN discovery (mDNS) --------- */

export interface PeerApp {
  id: string
  hash: string
  title: string
  description: string
  tags: string[]
  merkle?: string
  version?: number
  updated?: string
  paid?: boolean
  boostPool?: number
  priorityWeight?: number
  sizeBytes?: number
  cost?: number
  /** Адрес разработчика (owner подписанного манифеста) — Official Repository. */
  owner?: string
  /** iroh-hash PNG-логотипа dApp (отдельный блоб) — каталог тянет его до установки. */
  logo?: string
  /** iroh-hash DAPP Preview 440×180 (PNG/JPG) — карточки каталога/дашборда. */
  preview?: string
}

export interface PeerInfo {
  node: string
  nodeId: string
  addr: string
  apps: PeerApp[]
  lastSeen?: string
  viaSeeder?: boolean
}

export async function startDiscovery(): Promise<string | null> {
  if (isTauri) return invoke<string>('start_discovery')
  return null
}

/** Системное уведомление (Tauri notification plugin); в web — no-op. */
export async function notifySystem(title: string, body: string): Promise<void> {
  if (!isTauri) return
  try {
    const { isPermissionGranted, requestPermission, sendNotification } = await import(
      '@tauri-apps/plugin-notification'
    )
    let ok = await isPermissionGranted()
    if (!ok) ok = (await requestPermission()) === 'granted'
    if (ok) sendNotification({ title, body })
  } catch { /* плагин недоступен */ }
}

/* --------- Official Repository (проверенные разработчики и dApps) --------- */

export interface OfficialLists {
  developers: string[]
  dapps: string[]
  lastScan: number
}
/** Текущие списки official-репозитория (Sled + .env, мгновенно). */
export async function officialLists(): Promise<OfficialLists> {
  if (!isTauri) return { developers: [], dapps: [], lastScan: 0 }
  return invoke<OfficialLists>('official_lists')
}
/** Принудительный рескан он-чейн реестров. */
export async function officialRefresh(): Promise<OfficialLists> {
  if (!isTauri) return { developers: [], dapps: [], lastScan: 0 }
  return invoke<OfficialLists>('official_refresh')
}

/* --------- Соглашение о непроверенных dApps (NETFORY_UNVERIFIED_ACCEPT) --------- */

export interface UnverifiedAccept {
  accepted: boolean
  feeSth: number
}
/** Есть ли у пользователя ончейн/локальное согласие на показ непроверенных dApps. */
export async function checkUnverifiedAccept(): Promise<UnverifiedAccept> {
  const envFee = Number((import.meta as any).env?.FEE_ALLOW_ALL_DAPPS ?? 9999) || 9999
  if (!isTauri) return { accepted: false, feeSth: envFee }
  return invoke<UnverifiedAccept>('check_unverified_accept')
}
/** Оплата ончейн-соглашения (fee = FEE_ALLOW_ALL_DAPPS, amount = 1 STH). */
export async function payUnverifiedAccept(): Promise<{ id: string; accepted: boolean }> {
  return invoke<{ id: string; accepted: boolean }>('pay_unverified_accept')
}

/** Статус vDapps-анонса (страница Seeders). */
export interface VdappsStatus {
  eligible: boolean
  published: boolean
  ts: number
  devs: number
  dapps: number
  seeders: number
}
export async function vdappsStatus(): Promise<VdappsStatus> {
  if (!isTauri) return { eligible: false, published: false, ts: 0, devs: 0, dapps: 0, seeders: 0 }
  return invoke<VdappsStatus>('vdapps_status')
}

export async function discoveredPeers(): Promise<PeerInfo[]> {
  if (isTauri) return invoke<PeerInfo[]>('discovered_peers')
  return []
}

export async function stopDiscovery(): Promise<void> {
  if (isTauri) await invoke('stop_discovery')
}

/* --------- GeoLite2 peer geolocation (Tauri) --------- */

export interface GeoPoint {
  ip: string
  lat: number
  lng: number
}

/** Resolve peer addresses (ip / ip:port) to real coordinates via GeoLite2. */
export async function geoLookup(addrs: string[]): Promise<GeoPoint[]> {
  if (isTauri) return invoke<GeoPoint[]>('geo_lookup', { addrs })
  return []
}

/** Trigger the one-time GeoLite2 City DB download (native). */
export async function ensureGeoDb(): Promise<boolean> {
  if (isTauri) return invoke<boolean>('ensure_geo_db')
  return false
}

/* --------- dApp donations feed (Tauri) --------- */

export interface Donation {
  sender: string
  amount: string
  memo: string
  ts: number
}

export interface TopDonor {
  addr: string
  name: string | null
  totalSth: string
}

export interface DonationsResult {
  items: Donation[]
  totalSth: string
  donors: number
  count: number
  topDonor: TopDonor | null
}

export interface DonorTier {
  tier: number // 0 none, 1 bronze, 2 silver, 3 gold
  count: number
  totalSth: string
}

/** Donor medal tier (DON I/II/III) from a user's donation history. */
export async function donorTier(address: string): Promise<DonorTier> {
  if (isTauri) return invoke<DonorTier>('donor_tier', { address })
  const { mockDonorTier } = await import('@/wallet/ipc')
  return mockDonorTier(address)
}

/** Recent incoming `donate:<appId>` transfers + all-time total/donor count. */
export async function recentDonations(address: string, appId: string): Promise<DonationsResult> {
  if (isTauri) return invoke<DonationsResult>('recent_donations', { address, appId })
  return { items: [], totalSth: '0', donors: 0, count: 0, topDonor: null }
}

/* --------- WAN trackers + relay-only privacy (Tauri) --------- */

export async function getTrackers(): Promise<string[]> {
  if (isTauri) return invoke<string[]>('get_trackers')
  return []
}

// Phase G: оценка начислений сидеру по сети (per-app payout model).
export interface DappEarning {
  id: string
  name: string
  bankSth: number
  dailyPoolSth: number
  secondsRemaining: number
  myOnlineSecs: number
  seeders: number
  online: boolean
  estSth: number
  forecastSth: number
  nextPayoutSecs: number
}
export interface EarningsSummary {
  rewardAddress: string
  totalEstSth: number
  activeApps: DappEarning[]
}
export async function networkEarnings(rewardAddress: string): Promise<EarningsSummary> {
  if (isTauri) return invoke<EarningsSummary>('network_earnings', { rewardAddress })
  return { rewardAddress, totalEstSth: 0, activeApps: [] }
}

// ---- P5: история фактических выплат сидера (memo `payout:<appId>`) ----
export interface PayoutItem {
  id: string
  appId: string
  appName: string
  amountSth: string
  timestamp: number
  payer: string
  memo: string
  confirmed: boolean
}
export interface PayoutHistory {
  items: PayoutItem[]
  totalSth: string
  apps: number
  count: number
}

/**
 * Фактические выплаты сидеру (на кошелёк rewardAddress) от payout-сервера сети.
 * Извлекается из входящих on-chain transfers с memo `payout:<appId>[:...]`.
 * В web-preview возвращает пару демо-записей, чтобы UI был проверяем без Tauri.
 */
export async function payoutHistory(rewardAddress: string): Promise<PayoutHistory> {
  if (isTauri) {
    try {
      return await invoke<PayoutHistory>('payout_history', { rewardAddress })
    } catch {
      return { items: [], totalSth: '0.0000', apps: 0, count: 0 }
    }
  }
  const now = Date.now()
  const items: PayoutItem[] = [
    { id: 'mock-tx-1', appId: 'SmockAppSocial00000000000000000', appName: 'Social Wall', amountSth: '12.4801', timestamp: now - 3 * 86400_000, payer: 'SpayoutServer1example00000000000000', memo: 'payout:SmockAppSocial00000000000000000:19870', confirmed: true },
    { id: 'mock-tx-2', appId: 'SmockAppMusic0000000000000000000', appName: 'Music Studio', amountSth: '8.1150', timestamp: now - 5 * 86400_000, payer: 'SpayoutServer1example00000000000000', memo: 'payout:SmockAppMusic0000000000000000000:19868', confirmed: true },
    { id: 'mock-tx-3', appId: 'SmockAppSocial00000000000000000', appName: 'Social Wall', amountSth: '5.9433', timestamp: now - 8 * 86400_000, payer: 'SpayoutServer1example00000000000000', memo: 'payout:SmockAppSocial00000000000000000:19865', confirmed: true },
  ]
  const totalSth = items.reduce((s, i) => s + Number(i.amountSth || 0), 0).toFixed(4)
  const apps = new Set(items.map((i) => i.appId)).size
  return { items, totalSth, apps, count: items.length }
}

export async function setTrackers(list: string[]): Promise<void> {
  if (isTauri) await invoke('set_trackers', { list })
}

/** SmartNet DHT bootstrap seeders (ip:port). Multiple supported. */
export async function getSeeders(): Promise<string[]> {
  if (isTauri) return invoke<string[]>('get_seeders')
  return []
}

export async function setSeeders(list: string[]): Promise<void> {
  if (isTauri) await invoke('set_seeders', { list })
}

/** Auto-discovered seeders from trackers (read-only). */
export async function getSeedersAuto(): Promise<string[]> {
  if (isTauri) return invoke<string[]>('get_seeders_auto')
  return []
}

/** Full effective seeder set (manual + auto + env + default). */
export async function getSeedersEffective(): Promise<string[]> {
  if (isTauri) return invoke<string[]>('get_seeders_effective')
  return []
}

export async function getRelayOnly(): Promise<boolean> {
  if (isTauri) {
    try {
      return await invoke<boolean>('get_relay_only')
    } catch {
      return false
    }
  }
  return false
}

export async function setRelayOnly(enabled: boolean): Promise<void> {
  if (isTauri) await invoke('set_relay_only', { enabled })
}

/* --------- Mobile fallbacks: DoH + pkarr HTTPS-discovery (Phase 2) --------- */

export type MobileFallbacksMode = 'auto' | 'on' | 'off'

export async function getMobileFallbacks(): Promise<MobileFallbacksMode> {
  if (isTauri) {
    try {
      return (await invoke<string>('get_mobile_fallbacks')) as MobileFallbacksMode
    } catch {
      return 'auto'
    }
  }
  return 'auto'
}

export async function setMobileFallbacks(mode: MobileFallbacksMode): Promise<void> {
  if (isTauri) await invoke('set_mobile_fallbacks', { mode })
}

/* --------- Swarm download (bandit-failover) --------- */

export async function getSwarmEnabled(): Promise<boolean> {
  if (isTauri) {
    try {
      return await invoke<boolean>('get_swarm_enabled')
    } catch {
      return true
    }
  }
  return true
}

export async function setSwarmEnabled(enabled: boolean): Promise<void> {
  if (isTauri) await invoke('set_swarm_enabled', { enabled })
}

export interface SwarmSeeder {
  peer: string
  speed: number
  errors: number
  attempts: number
  successes: number
}

export async function swarmTopSeeders(): Promise<SwarmSeeder[]> {
  if (isTauri) {
    try {
      return await invoke<SwarmSeeder[]>('swarm_top_seeders')
    } catch {
      return []
    }
  }
  return []
}

/* --------- Trackerless DHT dApp discovery --------- */

export async function getDhtProvidersEnabled(): Promise<boolean> {
  if (isTauri) {
    try {
      return await invoke<boolean>('get_dht_providers_enabled')
    } catch {
      return true
    }
  }
  return true
}

export async function setDhtProvidersEnabled(enabled: boolean): Promise<void> {
  if (isTauri) await invoke('set_dht_providers_enabled', { enabled })
}

export interface DhtProvidersSummary {
  enabled: boolean
  seeders: number
  apps: number
  announcedApps: number
  beaconPort: number | null
  dhtNodes: number
}

export async function dhtProvidersSummary(): Promise<DhtProvidersSummary> {
  if (isTauri) {
    try {
      return await invoke<DhtProvidersSummary>('dht_providers_summary')
    } catch {
      return { enabled: true, seeders: 0, apps: 0, announcedApps: 0, beaconPort: null, dhtNodes: 0 }
    }
  }
  return { enabled: false, seeders: 0, apps: 0, announcedApps: 0, beaconPort: null, dhtNodes: 0 }
}

/* --------- Mainline DHT (pkarr) discovery --------- */

export interface SysLogLine {
  ts: number
  scope: string
  msg: string
  /** Coalesced repeat count. `1` (or missing) means first occurrence. */
  count?: number
  /** When true, this event replaces the last streamed line with the same
   *  scope+msg instead of appending. Emitted by the Rust dedup path. */
  replace?: boolean
}

/** Backlog of internal P2P/DHT/network log lines for the System Console. */
export async function sysLogHistory(): Promise<SysLogLine[]> {
  if (isTauri) {
    try {
      return await invoke<SysLogLine[]>('sys_log_history')
    } catch {
      return []
    }
  }
  return []
}

export interface DhtStats {
  enabled: boolean
  nodes: number
  via_smartnet: boolean
}

export async function getDhtEnabled(): Promise<boolean> {
  if (isTauri) {
    try {
      return await invoke<boolean>('get_dht_enabled')
    } catch {
      return true
    }
  }
  return true
}

export async function setDhtEnabled(enabled: boolean): Promise<void> {
  if (isTauri) await invoke('set_dht_enabled', { enabled })
}

export async function dhtStats(): Promise<DhtStats> {
  if (isTauri) {
    try {
      return await invoke<DhtStats>('dht_stats')
    } catch {
      return { enabled: false, nodes: 0, via_smartnet: false }
    }
  }
  return { enabled: false, nodes: 0, via_smartnet: false }
}

/* --------- «Сетевой доктор» (v1.63.13): диагностика мобильных сетей --------- */

export interface NetDoctorCheck {
  id: string
  /** ok | warn | fail */
  status: string
  detail: string
}

/** v1.135.210: диагностика discovery EndpointId — источники адресов и время, тестовый connect. */
export interface DiscoveryHit { source: string; ms: number; relays: number; addrs: number; relay: string }
export interface DiscoveryReport { id: string; hits: DiscoveryHit[]; errors: string[]; lookupMs: number; connectMs: number | null; via: string }
export async function discoveryProbe(node: string): Promise<DiscoveryReport> {
  if (!isTauri) return { id: node, hits: [], errors: ['browser mode'], lookupMs: 0, connectMs: null, via: '' }
  return await invoke<DiscoveryReport>('discovery_probe', { node })
}

/** Прогнать все пробы (~30 с). Прогресс приходит событиями `net-doctor-step`. */
export async function netDoctorRun(): Promise<NetDoctorCheck[]> {
  if (isTauri) {
    try {
      return await invoke<NetDoctorCheck[]>('net_doctor_run')
    } catch {
      return []
    }
  }
  return []
}

/** v1.63.17: точечная проверка пользовательских n1-реле (TCP/TLS 443, dual-stack).
 *  Каждый URL → одна запись `myrelay:<host>`. Прогресс — событиями
 *  `net-doctor-step`. */
export async function netDoctorCheckRelays(urls: string[]): Promise<NetDoctorCheck[]> {
  if (isTauri) {
    try {
      return await invoke<NetDoctorCheck[]>('net_doctor_check_relays', { urls })
    } catch {
      return []
    }
  }
  return []
}

/** Snapshot of live DHT node addresses (IP:port) for plotting on the globe. */
export async function dhtNodesList(): Promise<string[]> {
  if (isTauri) {
    try {
      return await invoke<string[]>('dht_nodes_list')
    } catch {
      return []
    }
  }
  return []
}

/* --------- SmartNet custom relays (n1-relay) --------- */

export interface RelayConfig {
  enabled: boolean
  urls: string[]
}

export async function getRelayConfig(): Promise<RelayConfig> {
  if (isTauri) return invoke<RelayConfig>('get_relay_config')
  return { enabled: false, urls: [] }
}

export async function setRelayConfig(enabled: boolean, urls: string[]): Promise<RelayConfig> {
  if (isTauri) return invoke<RelayConfig>('set_relay_config', { enabled, urls })
  return { enabled, urls }
}

/* --------- Стелс-режим (анти-DPI: padding + фантомный трафик + relay-изоляция) --------- */

/** Текущее состояние тумблера Стелс-режима (для синхронизации UI). */
export async function getStealthStatus(): Promise<boolean> {
  if (!isTauri) return false
  try {
    return await invoke<boolean>('get_stealth_status')
  } catch {
    return false
  }
}

/**
 * Включить/выключить Стелс-режим. `customRelays` — список наших n1-реле (https),
 * через которые изолируется сеть при следующем запуске. Возвращает сообщение о
 * применённых изменениях. Бросает Error при некорректном/пустом списке реле.
 */
export async function toggleStealthMode(enable: boolean, customRelays: string[]): Promise<string> {
  if (!isTauri) throw new Error('Стелс-режим доступен только в десктоп-приложении')
  return invoke<string>('toggle_stealth_mode', { enable, customRelays })
}

/* --------- decentralized .sth Name Service (dDNS) --------- */

export interface DomainStatus {
  input: string
  name: string
  zone: string
  valid: boolean
  available: boolean
  adminOnly: boolean
  reason: string // ok | invalid | reserved | taken
  priceSth: number
  currentAppId: string
}

export interface DomainRecord {
  name: string
  zone: string
  appId: string
  owner: string
  expires: number
}

/** Lifetime (days) a domain is registered for — `DOMAIN_DURATION_DAYS` (default 365). */
export function domainDurationDays(): number {
  const v = (import.meta as any).env?.DOMAIN_DURATION_DAYS
  const n = Number(v)
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : 365
}

/** v1.63.18+: список зарезервированных имён подгружается из публичных ассетов
 *  `/reserved.json` + `/reserved.txt` (по одному имени в строке) и хранится
 *  как Set для мгновенной O(1) проверки полных совпадений (lowercase).
 *  Первый вызов прогревает кэш; пока не прогрет — считаем список пустым
 *  (реальную защиту всё равно обеспечивает Rust-бэкенд при регистрации). */
let DOMAIN_RESERVED: Set<string> = new Set()
let reservedLoad: Promise<Set<string>> | null = null

export function loadReservedDomainNames(): Promise<Set<string>> {
  if (reservedLoad) return reservedLoad
  reservedLoad = (async () => {
    try {
      const [jr, tr] = await Promise.all([
        fetch('/reserved.json', { cache: 'force-cache' }).catch(() => null),
        fetch('/reserved.txt', { cache: 'force-cache' }).catch(() => null),
      ])
      const set = new Set<string>()
      if (jr?.ok) {
        const j = await jr.json()
        if (Array.isArray(j)) {
          for (const s of j) {
            if (typeof s === 'string' && s.trim()) set.add(s.trim().toLowerCase())
          }
        }
      }
      if (tr?.ok) {
        const txt = await tr.text()
        for (const line of txt.split(/\r?\n/)) {
          const s = line.trim().toLowerCase()
          if (s) set.add(s)
        }
      }
      DOMAIN_RESERVED = set
    } catch {
      DOMAIN_RESERVED = new Set()
    }
    return DOMAIN_RESERVED
  })()
  return reservedLoad
}
// Прогреваем кэш сразу, чтобы к моменту первого lookup список был готов.
if (typeof fetch !== 'undefined') {
  void loadReservedDomainNames()
}

/** SmartNet developers allowed to register reserved names (`.env DEVELOPERS`,
 *  comma-separated addresses). Everyone else gets `reason=reserved`. */
export function developersList(): string[] {
  const raw = ((import.meta as any).env?.DEVELOPERS as string) || ''
  return raw.split(',').map((s) => s.trim()).filter(Boolean)
}

export function isDeveloper(address: string): boolean {
  if (!address) return false
  return developersList().includes(address)
}

/** Mirror of the Rust validator/pricing so the .sth tab works in the web preview. */
function webDomainLookup(input: string, basePrice = 500, callerAddr = ''): DomainStatus {
  const s = (input || '').trim().toLowerCase()
  const [rawName, zone = 'sth'] = s.includes('.') ? [s.slice(0, s.lastIndexOf('.')), s.slice(s.lastIndexOf('.') + 1)] : [s, 'sth']
  const st: DomainStatus = {
    input, name: rawName, zone, valid: false, available: false,
    adminOnly: false, reason: 'invalid', priceSth: 0, currentAppId: '',
  }
  const okChars = /^[a-z0-9-]{1,63}$/.test(rawName) && !rawName.startsWith('-') && !rawName.endsWith('-')
  if (!okChars || zone !== 'sth') return st
  st.valid = true
  const len = rawName.length
  st.adminOnly = len <= 2
  st.priceSth = len === 3 ? basePrice * 100 : len === 4 ? basePrice * 20 : basePrice
  // Reserved-имена и 1–2-символьные метки доступны только DEVELOPERS.
  if ((DOMAIN_RESERVED.has(rawName) || st.adminOnly) && !isDeveloper(callerAddr)) {
    st.reason = 'reserved'
    return st
  }
  st.available = true
  st.reason = 'ok'
  return st
}

export async function domainLookup(name: string): Promise<DomainStatus> {
  if (isTauri) return invoke<DomainStatus>('domain_lookup', { name })
  return webDomainLookup(name)
}

export async function resolveDomain(name: string): Promise<string | null> {
  if (isTauri) {
    try {
      return await invoke<string | null>('resolve_domain', { name })
    } catch {
      return null
    }
  }
  return null
}

export async function domainOfApp(appId: string): Promise<string | null> {
  if (isTauri) {
    try {
      return await invoke<string | null>('domain_of_app', { appId })
    } catch {
      return null
    }
  }
  return null
}

export async function registerDomain(name: string, appId: string): Promise<string> {
  if (!isTauri) throw new Error('Регистрация доступна только в десктоп-клиенте')
  return invoke<string>('register_domain', { name, appId, durationDays: domainDurationDays() })
}

export async function listMyDomains(): Promise<DomainRecord[]> {
  if (isTauri) {
    try {
      return await invoke<DomainRecord[]>('list_my_domains')
    } catch {
      return []
    }
  }
  return []
}

/** Live `.sth` domains owned by a given address (for public profiles). */
export async function domainsByOwner(address: string): Promise<DomainRecord[]> {
  if (!isTauri || !address) return []
  try {
    return await invoke<DomainRecord[]>('domains_by_owner', { address })
  } catch {
    return []
  }
}

/** Active node + last-known latency (cached, no network call). */
export interface NodeStatus {
  host: string
  latencyMs: number | null
  online: boolean
  transport?: 'p2p' | 'fallback' | 'clearnet'
}
export async function nodeStatus(): Promise<NodeStatus | null> {
  if (!isTauri) return null
  try {
    return await invoke<NodeStatus>('node_status')
  } catch {
    return null
  }
}

// --- Протокол api:// (P2P-прокси к узлам через netfory-provider) ---
export interface ApiFetchResult {
  body: string
  verified: boolean
  status: number
  providerNodeId: string
  relayed: boolean
  target: string
}
export async function apiFetch(url: string): Promise<ApiFetchResult> {
  if (!isTauri) {
    throw new Error('Протокол api:// доступен только в десктоп-приложении SmartNet.')
  }
  return invoke<ApiFetchResult>('api_fetch', { url })
}

export async function renewalPrice(): Promise<number> {
  if (isTauri) {
    try {
      return await invoke<number>('renewal_price')
    } catch {
      return 250
    }
  }
  return 250
}

export async function renewDomain(name: string, appId: string): Promise<string> {
  if (!isTauri) throw new Error('Продление доступно только в десктоп-клиенте')
  return invoke<string>('renew_domain', { name, appId, durationDays: domainDurationDays() })
}

export interface SaleOrder {
  name: string
  zone: string
  seller: string
  priceSth: number
  expires: number
  originalAppId: string
}

export async function listSell(name: string, priceSth: number, expires: number): Promise<string> {
  if (!isTauri) throw new Error('Доступно только в десктоп-клиенте')
  return invoke<string>('list_sell', { name, priceSth, expires })
}

export async function domainOrder(name: string): Promise<SaleOrder | null> {
  if (isTauri) {
    try {
      return await invoke<SaleOrder | null>('domain_order', { name })
    } catch {
      return null
    }
  }
  return null
}

export async function listMarketOrders(): Promise<SaleOrder[]> {
  if (isTauri) {
    try {
      return await invoke<SaleOrder[]>('list_market_orders')
    } catch {
      return []
    }
  }
  // Web preview mock (no chain): a couple of sample listings so the marketplace
  // and the buy / Phase-3c flow are demonstrable in the browser.
  return [
    { name: 'cyberdeck', zone: 'sth', seller: 'ShMockSeLLer1example00000000000000', priceSth: 250, expires: Math.floor(Date.now() / 1000) + 30 * 86400, originalAppId: 'SmockAppId1example000000000000000' },
    { name: 'btc', zone: 'sth', seller: 'ShMockSeLLer3example00000000000000', priceSth: 99000, expires: Math.floor(Date.now() / 1000) + 90 * 86400, originalAppId: 'SmockAppId3example000000000000000' },
    { name: 'neon', zone: 'sth', seller: 'ShMockSeLLer2example00000000000000', priceSth: 4200, expires: Math.floor(Date.now() / 1000) + 60 * 86400, originalAppId: 'SmockAppId2example000000000000000' },
  ]
}

export async function atomicBuy(name: string, expectedPrice: number, newAppId = ''): Promise<string[]> {
  if (!isTauri) {
    // Web preview: simulate the 3-tx purchase so the post-buy UX is demonstrable.
    await new Promise((r) => setTimeout(r, 1200))
    return ['mock-tx-fee', 'mock-tx-seller', 'mock-tx-claim']
  }
  return invoke<string[]>('atomic_buy', { name, expectedPrice, newAppId })
}

/** Phase 3c — auto-mint a draft app (cyberpunk stub) for a just-bought domain. */
export async function createDomainDraft(domain: string): Promise<Manifest | null> {
  if (isTauri) {
    try {
      return await invoke<Manifest>('create_domain_draft', { domain })
    } catch {
      return null
    }
  }
  // Web mock: a fake draft manifest so the "go to my app" CTA has a target.
  const id = 'Sdraft' + Math.random().toString(36).slice(2, 12).padEnd(10, '0')
  const display = domain.replace(/\.sth$/, '')
  return {
    id,
    name: `${display}.sth`,
    description: 'Purchased domain draft',
    owner: 'web',
    files: [],
    merkle: '',
    created: new Date().toISOString(),
    signature: '',
    publicKey: '',
    status: 'draft',
  }
}

/* --------- auto-update preference (Tauri) --------- */

export async function getAutoUpdate(): Promise<boolean> {
  if (isTauri) {
    try {
      return await invoke<boolean>('get_auto_update')
    } catch {
      return true
    }
  }
  return true
}

export async function setAutoUpdate(enabled: boolean): Promise<void> {
  if (isTauri) await invoke('set_auto_update', { enabled })
}

/** Pre-stage an app update blob over iroh-blobs without applying it (hybrid mode). */
export async function prefetchApp(nodeId: string, hash: string): Promise<void> {
  if (isTauri) {
    try {
      await invoke('prefetch_app', { nodeId, hash })
    } catch {
      /* best-effort pre-stage; apply will fetch on demand if this fails */
    }
  }
}

export async function optimizeBlobStore(): Promise<number> {
  if (isTauri) {
    try {
      return await invoke<number>('optimize_blob_store')
    } catch {
      return 0
    }
  }
  return 0
}

export async function blobStoreUsage(): Promise<number> {
  if (isTauri) {
    try {
      return await invoke<number>('blob_store_usage')
    } catch {
      return 0
    }
  }
  return 0
}

export interface DedupGroup {
  hash: string
  appIds: string[]
}
export interface DedupStats {
  uniqueBlobs: number
  trackedApps: number
  sharedBlobs: number
  dedupedApps: number
  savedBytes: number
  groups: DedupGroup[]
}

/** Cross-app content-dedup registry stats (iroh-blobs is content-addressed). */
export async function dedupStats(): Promise<DedupStats> {
  const empty: DedupStats = { uniqueBlobs: 0, trackedApps: 0, sharedBlobs: 0, dedupedApps: 0, savedBytes: 0, groups: [] }
  if (isTauri) {
    try {
      const r = await invoke<any>('dedup_stats')
      return {
        uniqueBlobs: r.unique_blobs ?? 0,
        trackedApps: r.tracked_apps ?? 0,
        sharedBlobs: r.shared_blobs ?? 0,
        dedupedApps: r.deduped_apps ?? 0,
        savedBytes: r.saved_bytes ?? 0,
        groups: (r.groups ?? []).map((g: any) => ({ hash: g.hash, appIds: g.app_ids ?? [] })),
      }
    } catch {
      return empty
    }
  }
  // Web preview mock: two apps sharing one content blob (1 copy deduped).
  return { uniqueBlobs: 3, trackedApps: 4, sharedBlobs: 1, dedupedApps: 1, savedBytes: 2_400_000, groups: [{ hash: 'bafkrmocksharedblob000', appIds: ['SappAlpha', 'SappBeta'] }] }
}

export interface SyncStatus {
  done: number
  total: number
  complete: boolean
  pending: string[]
}

let _webSyncTicks = 0
/** First-scan progress for the boot splash. */
export async function syncStatus(): Promise<SyncStatus> {
  if (isTauri) {
    try {
      return await invoke<SyncStatus>('sync_status')
    } catch {
      return { done: 5, total: 5, complete: true, pending: [] }
    }
  }
  // Web preview: simulate a short cold-start scan (≈3 polls → done).
  _webSyncTicks = Math.min(_webSyncTicks + 2, 5)
  const done = _webSyncTicks
  return { done, total: 5, complete: done >= 5, pending: [] }
}


/* --------- Network Map (seeder leaderboard, Tauri) --------- */

export interface NetworkSeeder {
  endpointId: string
  node: string
  metricsUrl: string
  appsSeeded: number
  gbSeeded: number
  uptimeSecs: number
  score: number
  online: boolean
  lastSeen: number
}

export async function networkMap(): Promise<NetworkSeeder[]> {
  if (isTauri) {
    try {
      return await invoke<NetworkSeeder[]>('network_map')
    } catch {
      return []
    }
  }
  return []
}

/** Discovery/transport source per peer: endpointId -> 'direct'|'relay'|'mdns'|'wan'. */
export async function peerSources(ids: string[]): Promise<Record<string, string>> {
  if (isTauri && ids.length) {
    try {
      return await invoke<Record<string, string>>('peer_sources', { ids })
    } catch {
      return {}
    }
  }
  return {}
}

/** Storage proof for connected peers: peerId -> 'verified'|'partial'|'unverified'. */
export async function verifyPeersStorage(peerIds: string[]): Promise<Record<string, string>> {
  const map: Record<string, string> = {}
  if (!peerIds.length) return map
  if (isTauri) {
    try {
      const res = await invoke<{ id: string; status: string }[]>('verify_peers_storage', { peerIds })
      for (const r of res) map[r.id] = r.status
    } catch {
      /* leave empty → markers fall back to unverified (blue) */
    }
    return map
  }
  // Web preview mock: first peer verified (green), second partial (orange), rest blue.
  peerIds.forEach((id, i) => {
    map[id] = i === 0 ? 'verified' : i === 1 ? 'partial' : 'unverified'
  })
  return map
}

export interface NetworkStorage {
  totalCapacityGb: number
  totalUsedGb: number
  totalFreeGb: number
  onlineNodes: number
  totalNodes: number
  history: { t: number; usedGb: number }[]
  paymentPoolBalanceSth: number
}

/** Global mesh storage capacity/usage (tracker-aggregated). null when pending/offline. */
export async function networkStorage(): Promise<NetworkStorage | null> {
  if (!isTauri) return null
  try {
    return await invoke<NetworkStorage>('network_storage')
  } catch {
    return null
  }
}

/* --------- iroh-blobs transit (Tauri, `p2p` feature) --------- */

export interface P2pStatus {
  enabled: boolean
  nodeId: string
}

export async function p2pStatus(): Promise<P2pStatus> {
  if (isTauri) {
    try {
      return await invoke<P2pStatus>('p2p_status')
    } catch {
      return { enabled: false, nodeId: '' }
    }
  }
  return { enabled: false, nodeId: '' }
}

/**
 * Fetch a peer's dApp over iroh-blobs (by collection hash + serving node id),
 * export it into data/dapps/<id>, register it and verify content.json.
 * Returns the verified manifest on success.
 */
export async function installApp(
  id: string,
  nodeId: string,
  hash: string,
): Promise<Manifest | null> {
  if (!isTauri) return null
  return invoke<Manifest>('install_app', { id, nodeId, hash })
}

export interface DhtResolveResult {
  providers: string[]
  hash: string
  seeders: number
}

/** Трекерлесс-резолв dApp: Mainline DHT + identity-биконы → провайдеры + хэш блоба. */
export async function dhtResolveApp(id: string): Promise<DhtResolveResult | null> {
  if (!isTauri) return null
  try {
    return await invoke<DhtResolveResult>('dht_resolve_app', { id })
  } catch {
    return null
  }
}

export interface AppShareInfo {
  nodeId: string
  hash: string
}

/** Данные прямой ссылки на своё приложение: sth://<id>?n=<nodeId>&h=<hash>. */
export async function appShareInfo(id: string): Promise<AppShareInfo | null> {
  if (!isTauri) return null
  try {
    return await invoke<AppShareInfo>('app_share_info', { id })
  } catch {
    return null
  }
}


/* ---------------- File Exchange (v2.0 — decentralized files) ---------------- */

export interface FileManifest {
  cid: string
  name: string
  size: number
  mimeType: string
  owner: string
  price: number
  isPaid: boolean
  created: number
  seeding: boolean
  blob?: string
  seederNode?: string
  published?: boolean
  txid?: string
  remote?: boolean
  /** IPFS CIDv0 — proof of ownership / future IPFS interop (not the identifier). */
  ipfsCid?: string
  /** live seeder count (populated by list/meta endpoints). */
  seeds?: number
  /** unix seconds the file finished downloading locally (downloaded files). */
  downloadedAt?: number
  /** unix seconds the file was published on-chain (owned published files). */
  publishedAt?: number
  /** true while a background P2P download for this file is in progress. */
  downloading?: boolean
  /** true when the stored bytes are AES-256-GCM encrypted (E2EE private file). */
  encrypted?: boolean
  /** ECIES-wrapped AES file keys per authorized address (owner + whitelist). */
  wrappedKeys?: Record<string, string>
  /** SmartHoldem addresses allowed to decrypt (owner + recipients). */
  recipients?: string[]
  /** Real (decrypted) filename for a private file — local-only. */
  displayName?: string
  /**
   * Машиночитаемая категория файла — JSON-строка, например
   * `{"cat":"dev","repo":"git-remote-dev","ver":"1.55.47","os":"win"}`,
   * `{"cat":"music"}`, `{"cat":"image"}`. Пустая строка = «other».
   * Парсится на фронте через `parseFileTag()` из `@/lib/fileTags`.
   */
  tag?: string
}

/**
 * Shareable, self-contained `sn://file/...` link that carries ALL metadata so
 * the recipient sees the file correctly (name → extension/mime, size, paid
 * status, price, owner-to-pay) WITHOUT waiting for on-chain indexing:
 *   `sn://file/<cid>~<node>~<size>~<price>~<owner>~<name>`
 * The iroh transit hash equals the Blake3 cid, so it is NOT repeated.
 */
export function fileShareLink(m: FileManifest): string {
  const node = m.seederNode || ''
  const price = m.isPaid ? m.price || 0 : 0
  const owner = m.owner || ''
  const name = encodeURIComponent(m.name || '')
  return `sn://file/${m.cid}~${node}~${m.size || 0}~${price}~${owner}~${name}`
}

/** Register a file received via a self-contained share link (Private/P2P mode). */
export async function registerSharedFile(
  cid: string,
  blob: string,
  node: string,
  name: string,
  size: number,
  price: number,
  owner: string,
): Promise<FileManifest> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<FileManifest>('register_shared_file', { cid, blob, node, name, size, price, owner })
}

/** Notify the whitelisted recipients of a private file over the chain
 *  (`sf:<cid>~<node>` memo). Owner only. Returns the number of notifications sent. */
export async function notifyPrivateRecipients(cid: string): Promise<number> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<number>('notify_private_recipients', { cid })
}

/** Received private files (from the on-chain `sf:` scanner) — the inbox. */
export async function listPrivateInbox(): Promise<FileManifest[]> {
  if (!isTauri) return []
  return invoke<FileManifest[]>('list_private_inbox')
}

// ---- Social layer (avatars / wall) ----

/** Open the native picker for an image (png/jpg/webp). Returns the path or null. */
export async function pickImageFile(): Promise<string | null> {
  if (!isTauri) return null
  return invoke<string | null>('pick_image_file')
}

/** Set the profile avatar (downscaled to 128×128, seeded). Returns a data-URL. */
export async function setProfileAvatar(srcPath: string): Promise<string> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<string>('set_profile_avatar', { srcPath })
}

/** data-URL of the current user's avatar, or null if none set. */
export async function getProfileAvatar(): Promise<string | null> {
  if (!isTauri) return null
  return invoke<string | null>('get_profile_avatar')
}

/** Avatar CID + node of another author (to render their avatar over P2P). */
export async function authorAvatar(author: string): Promise<{ cid: string; node: string } | null> {
  if (!isTauri || !author) return null
  try {
    return await invoke<{ cid: string; node: string } | null>('author_avatar', { author })
  } catch {
    return null
  }
}

/** МГНОВЕННЫЙ аватар автора из локального дискового кэша (без сети). */
export async function authorAvatarCached(author: string): Promise<string | null> {
  if (!isTauri || !author) return null
  try {
    return await invoke<string | null>('author_avatar_cached', { author })
  } catch {
    return null
  }
}

/** Downscale (≤1024×768) and attach an image to a wall post. Returns its BLAKE3 hash. */
export async function attachImageToPost(filePath: string): Promise<string> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<string>('attach_image_to_post', { filePath })
}

// ---- Social layer (wall / microblog) ----

export interface PostFile {
  cid: string
  name: string
  size: number
  mime: string
  node: string
  title?: string
  artist?: string
  cover?: string
}
export interface AlbumRef {
  id: string
  manifest?: string
  node?: string
  title?: string
  cover?: string
  price?: number
  free?: boolean
  tracks?: number
}
export interface WallPost {
  id: string
  author: string
  text: string
  images: string[]
  files: PostFile[]
  avatar: string
  node: string
  ts: number
  comments: number
  album?: AlbumRef | null
}
export interface WallComment {
  id: string
  post_id: string
  post_author: string
  author: string
  text: string
  ts: number
  node: string
  avatar: string
}

/** Fetch a wall media blob (avatar/image) by CID → data-URL (local or over P2P). */
export async function fetchWallMedia(cid: string, node = ''): Promise<string | null> {
  if (!isTauri || !cid) return null
  try {
    return await invoke<string>('fetch_wall_media', { cid, node })
  } catch {
    return null
  }
}

/** Publish a post on my own wall. */
export async function createPost(
  text: string,
  images: string[] = [],
  files: PostFile[] = [],
): Promise<WallPost> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<WallPost>('create_post', { text, images, files, album: null })
}

/** Attach a file (mp3/mp4/png/jpg…) to a post via native picker → returns its descriptor. */
export async function socialAttachFile(srcPath: string): Promise<PostFile> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<PostFile>('social_attach_file', { srcPath })
}
/** Fetch a post file's bytes (base64), downloading over P2P if needed. */
export async function socialFileBytes(cid: string, node = ''): Promise<string | null> {
  if (!isTauri || !cid) return null
  try {
    return await invoke<string>('social_file_bytes', { cid, node })
  } catch {
    return null
  }
}
/** Download a post file and save it via the native "Save As" dialog. */
export async function socialSaveFile(cid: string, node: string, name: string): Promise<boolean> {
  if (!isTauri) return false
  return invoke<boolean>('social_save_file', { cid, node, name })
}

// -- personal playlist --
export interface PlaylistTrack {
  cid: string
  name: string
  title?: string
  artist?: string
  size?: number
  mime?: string
  node?: string
  cover?: string
  ts?: number
}
export async function playlistAdd(track: PlaylistTrack): Promise<void> {
  if (!isTauri) return
  await invoke('playlist_add', { track })
}
export async function playlistRemove(cid: string): Promise<void> {
  if (!isTauri) return
  await invoke('playlist_remove', { cid })
}
export async function playlistList(owner: string): Promise<PlaylistTrack[]> {
  if (!isTauri) return []
  return invoke<PlaylistTrack[]>('playlist_list', { owner })
}
/** Сохранить порядок треков плейлиста (drag-and-drop). `order` — массив CID. */
export async function playlistReorder(order: string[]): Promise<void> {
  if (!isTauri) return
  await invoke('playlist_reorder', { order })
}

// ---- Музыкальные альбомы (бесплатные, iroh-коллекция) ----
export interface AlbumTrack {
  title: string
  cid: string
  node?: string
  size?: number
  mime?: string
  artist?: string
  cover?: string
  /** CID бесплатного ~30-сек превью (для платных E2EE-треков). */
  preview?: string
}
export interface Album {
  id: string
  owner: string
  artist: string
  title: string
  cover?: string
  node?: string
  tracks: AlbumTrack[]
  created: number
  free?: boolean
  price?: number
  year?: number
  note?: string
  /** Строгое E2EE: треки зашифрованы, ключ выдаётся покупателям per-buyer. */
  encrypted?: boolean
}
/** Мультивыбор аудиофайлов (нативный диалог). */
export async function pickAudioFiles(): Promise<string[]> {
  if (!isTauri) return []
  return invoke<string[]>('pick_audio_files')
}
/** Прочитать ID3-теги (Title/Artist) списка файлов без импорта — для Студии. */
export async function readAudioMeta(paths: string[]): Promise<{ title: string; artist: string }[]> {
  if (!isTauri) return []
  return invoke<{ title: string; artist: string }[]>('read_audio_meta', { paths })
}
/** Опубликовать альбом (обложка + треки → iroh + пост на стене). price>0 = платный. */
export async function albumCreate(
  title: string,
  artist: string,
  coverPath: string,
  trackPaths: string[],
  trackTitles: string[],
  price: number,
  year: number,
  note: string,
  preview = false,
): Promise<Album> {
  return invoke<Album>('album_create', { title, artist, coverPath, trackPaths, trackTitles, price, year, note, preview })
}
/** Альбомы пользователя. */
export async function albumList(owner: string): Promise<Album[]> {
  if (!isTauri) return []
  return invoke<Album[]>('album_list', { owner })
}
/** Редактировать метаданные своего альбома (треки не меняются). */
export async function albumUpdate(
  id: string,
  title: string,
  artist: string,
  coverPath: string,
  price: number,
  year: number,
  note: string,
): Promise<Album> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<Album>('album_update', { id, title, artist, coverPath, price, year, note })
}
/** Удалить свой альбом (только у автора; у купивших/скачавших доступ сохраняется). */
export async function albumDelete(id: string): Promise<void> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<void>('album_delete', { id })
}
/** Все известные локально альбомы (глобальная лента «Музыка»). */
export async function albumFeed(): Promise<Album[]> {
  if (!isTauri) return []
  return invoke<Album[]>('album_feed')
}
/** Один альбом по id (страница альбома). */
export async function albumGet(id: string): Promise<Album | null> {
  if (!isTauri) return null
  return invoke<Album | null>('album_get', { id })
}
/** Купить платный альбом (перевод STH владельцу). Возвращает txid. */
export async function albumBuy(id: string): Promise<string> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<string>('album_buy', { id })
}
/** Доступ к альбому (бесплатный / владелец / оплачен). */
export async function albumAccess(id: string): Promise<boolean> {
  if (!isTauri) return false
  return invoke<boolean>('album_access', { id })
}
/** Владелец: выпустить E2EE-ключи всем подтверждённым покупателям. */
export async function albumSyncKeys(id: string): Promise<number> {
  if (!isTauri) return 0
  return invoke<number>('album_sync_keys', { id })
}
/** Покупатель: получить и распечатать свой E2EE-ключ альбома. */
export async function albumFetchKey(id: string): Promise<boolean> {
  if (!isTauri) return false
  return invoke<boolean>('album_fetch_key', { id })
}
/** Число продаж платного альбома (уникальные покупатели on-chain). */
export async function albumSales(id: string): Promise<number> {
  if (!isTauri) return 0
  return invoke<number>('album_sales', { id })
}
/** Суммарный доход автора по альбому (сумма STH по всем продажам). */
export async function albumRevenue(id: string): Promise<number> {
  if (!isTauri) return 0
  return invoke<number>('album_revenue', { id })
}
/** Временной ряд продаж альбома: точки {ts (ms), amount (STH)} для мини-графика. */
export async function albumSalesSeries(id: string): Promise<{ ts: number; amount: number }[]> {
  if (!isTauri) return []
  return invoke<{ ts: number; amount: number }[]>('album_sales_series', { id })
}
/** Ссылка sn://album/... для шаринга альбома в сети. */
export async function albumShareLink(id: string): Promise<string> {
  return invoke<string>('album_share_link', { id })
}
/** Открыть альбом по ссылке sn://album/... (добор манифеста + проверка подписи). Возвращает id. */
export async function albumOpenLink(link: string): Promise<string> {
  return invoke<string>('album_open_link', { link })
}
/** Magnet-экспорт бесплатного альбома «во внешний мир». Возвращает magnet-ссылку. */
export async function albumExportTorrent(id: string): Promise<string> {
  return invoke<string>('album_export_torrent', { id })
}
/** Mark a followed wall as read (clears its unread badge). */
export async function markWallRead(author: string): Promise<void> {
  if (!isTauri) return
  await invoke('mark_wall_read', { author })
}
/** Unread post counts per followed wall (author → count). */
export async function wallUnreadCounts(): Promise<Record<string, number>> {
  if (!isTauri) return {}
  return invoke<Record<string, number>>('wall_unread_counts')
}

/** Posts on an author's wall (newest first). beforeTs — cursor: only posts older than it. */
export async function listWall(author: string, limit = 50, beforeTs?: number): Promise<WallPost[]> {
  if (!isTauri) return []
  return invoke<WallPost[]>('list_wall', { author, limit, beforeTs: beforeTs ?? null })
}

export async function deletePost(id: string): Promise<void> {
  if (!isTauri) return
  await invoke('delete_post', { id })
}

export async function addComment(postAuthor: string, postId: string, text: string): Promise<WallComment> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<WallComment>('add_comment', { postAuthor, postId, text })
}

export async function listComments(postAuthor: string, postId: string): Promise<WallComment[]> {
  if (!isTauri) return []
  return invoke<WallComment[]>('list_comments', { postAuthor, postId })
}

// -- reactions (likes) --
export interface ReactionSummary {
  counts: Record<string, number>
  mine: string
}
/** Aggregated reactions for a post + my current reaction emoji. */
export async function postReactions(postAuthor: string, postId: string): Promise<ReactionSummary> {
  if (!isTauri) return { counts: {}, mine: '' }
  return invoke<ReactionSummary>('post_reactions', { postAuthor, postId })
}
/** Set/switch/remove my reaction (toggles off when the same emoji is sent again). */
export async function reactToPost(postAuthor: string, postId: string, emoji: string): Promise<ReactionSummary> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<ReactionSummary>('react_to_post', { postAuthor, postId, emoji })
}

export async function followWall(author: string): Promise<void> {
  if (!isTauri) return
  await invoke('follow_wall', { author })
}
export async function unfollowWall(author: string): Promise<void> {
  if (!isTauri) return
  await invoke('unfollow_wall', { author })
}
export async function listWallFollows(): Promise<string[]> {
  if (!isTauri) return []
  return invoke<string[]>('list_wall_follows')
}
export async function isFollowingWall(author: string): Promise<boolean> {
  if (!isTauri) return false
  return invoke<boolean>('is_following_wall', { author })
}
/** Subscribe to an author's live topic for the current view (no persistent follow). */
export async function openWall(author: string): Promise<void> {
  if (!isTauri) return
  await invoke('open_wall', { author })
}

// -- moderation (wall owner) --
export async function hideComment(commentId: string, target: string): Promise<void> {
  if (!isTauri) return
  await invoke('hide_comment', { commentId, target })
}
export async function banCommenter(target: string): Promise<void> {
  if (!isTauri) return
  await invoke('ban_commenter', { target })
}
export async function unbanCommenter(target: string): Promise<void> {
  if (!isTauri) return
  await invoke('unban_commenter', { target })
}
export async function listBlocked(): Promise<string[]> {
  if (!isTauri) return []
  return invoke<string[]>('list_blocked')
}

/** Subscribe to live social events (post/comment/delete/moderate/snapshot). */
/** Граф подписок в sled изменился (дельта с цепи / tx из стрима) — перечитать статистику. */
export async function onSocialGraphUpdated(cb: (e: { address: string }) => void): Promise<() => void> {
  return appListen<{ address: string }>('social-graph-updated', cb)
}

export async function onSocialEvent(
  cb: (e: { kind: string; author: string }) => void,
): Promise<() => void> {
  return appListen<{ kind: string; author: string }>('social-event', cb)
}


/** On-chain sales (distinct buyers) per CID for the owner's paid files. */
export async function fileSalesCounts(cids: string[]): Promise<Record<string, number>> {
  if (!isTauri || !cids.length) return {}
  try {
    return await invoke<Record<string, number>>('file_sales_counts', { cids })
  } catch {
    return {}
  }
}

export interface SeedInfo {
  seeds: number
  available: boolean
}

export interface FileDiskStats {
  totalBytes: number
  freeBytes: number
  usedBytes: number
  filesBytes: number
  fileCount: number
}

/** Native single-file picker → absolute path (null if cancelled / web). */
export async function pickFile(): Promise<string | null> {
  if (!isTauri) return null
  return invoke<string | null>('pick_file')
}

/** Hash → CIDv0, copy into filesdata/<XX>/, register + seed over iroh.
 *  `tag` — JSON-строка категоризации (см. FileManifest.tag / parseFileTag). */
export async function addFile(
  srcPath: string,
  price: number,
  isPaid: boolean,
  recipients: string[] = [],
  tag?: string,
): Promise<FileManifest> {
  if (!isTauri) throw new Error('File Exchange доступен только в десктоп-приложении')
  return invoke<FileManifest>('add_file', { srcPath, price, isPaid, recipients, tag: tag ?? '' })
}

export async function listMyFiles(): Promise<FileManifest[]> {
  if (!isTauri) return []
  return invoke<FileManifest[]>('list_my_files')
}

export async function listExchangeFiles(): Promise<FileManifest[]> {
  if (!isTauri) return []
  return invoke<FileManifest[]>('list_exchange_files')
}

export async function fileMeta(cid: string): Promise<FileManifest | null> {
  if (!isTauri) return null
  return invoke<FileManifest | null>('file_meta', { cid })
}

export async function setFileSeeding(cid: string, on: boolean): Promise<FileManifest> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<FileManifest>('set_file_seeding', { cid, on })
}

export async function deleteFile(cid: string): Promise<void> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  await invoke('delete_file', { cid })
}

/** Delete a downloaded (non-owned) file's local copy. */
export async function deleteDownloadedFile(cid: string): Promise<void> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  await invoke('delete_downloaded_file', { cid })
}

// ── SmartHoldem node pool ────────────────────────────────────────────────
export interface NodePing {
  host: string
  latencyMs: number | null
  online: boolean
  height: number | null
  active: boolean
  /** true — iroh-узел sth-core (`api://…`), false — legacy HTTPS. */
  p2p?: boolean
  /** Короткая причина недоступности (discovery / реле / ALPN / таймаут). */
  error?: string
}

/** SHIP-44: состояние push-стрима кошелька (`sth/events/1`). */
export interface WalletStreamStatus {
  live: boolean
  node: string
  height: number
  frames: number
  /** epoch-мс последнего кадра block/tx (v1.135.213) */
  lastBlockMs?: number
  /** подключён, но блоки не идут > 60 с */
  stale?: boolean
}

/** Принудительный реконнект стрима кошелька (клик по бейджу STALE); срабатывает ≤10 с. */
export async function walletStreamReconnect(): Promise<void> {
  if (!isTauri) return
  try { await invoke('wallet_stream_reconnect') } catch { /* ignore */ }
}

export async function walletStreamStatus(): Promise<WalletStreamStatus | null> {
  if (!isTauri) return null
  try {
    return await invoke<WalletStreamStatus>('wallet_stream_status')
  } catch {
    return null
  }
}

/** Ping the node pool (rescan); keeps the current active node. */
export async function refreshNodePool(): Promise<NodePing[]> {
  if (!isTauri) return []
  return invoke<NodePing[]>('refresh_node_pool')
}

/** Ping the pool and auto-connect to the fastest online node. */
export async function autoSelectNode(): Promise<NodePing[]> {
  if (!isTauri) return []
  return invoke<NodePing[]>('auto_select_node')
}

/** Manually pin the active node. */
export async function selectNode(host: string): Promise<string> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<string>('select_node', { host })
}

/** Currently active node hostname. */
export async function currentNode(): Promise<string> {
  if (!isTauri) return ''
  return invoke<string>('current_node')
}

/** User-added custom nodes (clearnet hostnames or `api://` P2P addresses). */
export async function listCustomNodes(): Promise<string[]> {
  if (!isTauri) return []
  return invoke<string[]>('list_custom_nodes')
}

/** Add a custom node. Returns the updated custom list. */
export async function addCustomNode(host: string): Promise<string[]> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<string[]>('add_custom_node', { host })
}

/** Remove a custom node. Returns the updated custom list. */
export async function removeCustomNode(host: string): Promise<string[]> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<string[]>('remove_custom_node', { host })
}

/** Connectivity check for a custom node (api:// via P2P /blockchain, else HTTPS). */
export interface CustomPing {
  host: string
  online: boolean
  latencyMs: number | null
  height: number | null
  verified: boolean
  p2p: boolean
  error?: string
}
export async function pingCustomNode(host: string): Promise<CustomPing> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<CustomPing>('ping_custom_node', { host })
}

export async function fileDiskStats(): Promise<FileDiskStats> {
  if (!isTauri) {
    return { totalBytes: 0, freeBytes: 0, usedBytes: 0, filesBytes: 0, fileCount: 0 }
  }
  return invoke<FileDiskStats>('file_disk_stats')
}

/** Open an `f://<cid>` file in its own native window (web: new browser tab). */
export async function openFile(cid: string, name = ''): Promise<void> {
  if (!isTauri) {
    window.open(`f://${cid}`, '_blank')
    return
  }
  await invoke('open_file_webview', { cid, name })
}

/** Open a locally-held file with the OS default application (player/viewer…). */
export async function openFileNative(cid: string): Promise<void> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  await invoke('open_file_native', { cid })
}

/** Stamp a file's CID on-chain via a Type 5 (IPFS) transaction (default 5 STH). */
export async function publishFile(cid: string): Promise<FileManifest> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<FileManifest>('publish_file', { cid })
}

/** Seed count + local availability for a CID (download page). */
export async function fileSeedCount(cid: string): Promise<SeedInfo> {
  if (!isTauri) return { seeds: 0, available: false }
  return invoke<SeedInfo>('file_seed_count', { cid })
}

export interface DownloadProgress {
  downloaded: number
  total: number
  seeds: number
  complete: boolean
}

/** Live download progress (bytes transferred, total, seeders) for a CID. */
export async function fileDownloadProgress(cid: string): Promise<DownloadProgress> {
  if (!isTauri) return { downloaded: 0, total: 0, seeds: 0, complete: false }
  return invoke<DownloadProgress>('file_download_progress', { cid })
}

/** Download a remote file's blob over iroh into the local store. */
export async function downloadFile(cid: string): Promise<FileManifest> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<FileManifest>('download_file', { cid })
}

/**
 * Start a BACKGROUND P2P download: records the file as `downloading` in the DB
 * and keeps fetching it regardless of the current page. Returns immediately.
 */
export async function startFileDownload(cid: string): Promise<FileManifest> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<FileManifest>('start_file_download', { cid })
}

/** Save a locally-held file to a user-chosen path (native save dialog). */
export async function saveFileAs(cid: string): Promise<boolean> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<boolean>('save_file_as', { cid })
}

/** Pay for a paid file (transfer + `filebuy:` memo). Returns the payment tx id. */
export async function buyFile(cid: string): Promise<string> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<string>('buy_file', { cid })
}

/** Whether the current user may access (free / owner / purchased). */
export async function checkFileAccess(cid: string): Promise<boolean> {
  if (!isTauri) return false
  return invoke<boolean>('check_file_access', { cid })
}

/* ---------------- Встроенный BitTorrent-клиент (Этап B) ---------------- */

export interface TorrentFile {
  path: string
  size: number
}
export interface TorrentMetadata {
  infoHash: string
  name: string
  totalBytes: number
  files: TorrentFile[]
}
export interface TorrentStatus {
  infoHash: string
  name: string
  totalBytes: number
  downloadedBytes: number
  uploadedBytes: number
  progress: number
  state: string
  seedersConnected: number
  peersConnected: number
  downloadSpeedBytes: number
  uploadSpeedBytes: number
  liveTcp: number
  liveUtp: number
  isUpdate: boolean
}

/** Распарсить magnet-ссылку и подгрузить метаданные (имя, размер, файлы). */
export async function parseMagnetLink(magnetUrl: string): Promise<TorrentMetadata> {
  if (!isTauri) throw new Error('Торрент-клиент доступен только в десктоп-приложении')
  return invoke<TorrentMetadata>('parse_magnet_link', { magnetUrl })
}

/** Распарсить .torrent-файл (байты) — для input[type=file]. */
export async function parseTorrentFile(bytes: Uint8Array | number[]): Promise<TorrentMetadata> {
  if (!isTauri) throw new Error('Торрент-клиент доступен только в десктоп-приложении')
  return invoke<TorrentMetadata>('parse_torrent_file', { bytes: Array.from(bytes) })
}

/** Распарсить .torrent по пути (из события drag-drop Tauri). */
export async function parseTorrentPath(path: string): Promise<TorrentMetadata> {
  if (!isTauri) throw new Error('Торрент-клиент доступен только в десктоп-приложении')
  return invoke<TorrentMetadata>('parse_torrent_path', { path })
}

/** Запустить скачивание по ранее распарсенному info-hash. */
export async function startTorrentDownload(
  infoHash: string,
  downloadPath?: string | null,
  sequential = false,
  onlyFiles?: number[] | null,
): Promise<void> {
  if (!isTauri) throw new Error('Торрент-клиент доступен только в десктоп-приложении')
  await invoke('start_torrent_download', {
    infoHash,
    downloadPath: downloadPath ?? null,
    sequential,
    onlyFiles: onlyFiles && onlyFiles.length ? onlyFiles : null,
  })
}

/**
 * Fast path: add a magnet AND start downloading in one non-blocking call.
 * Returns { infoHash, name } instantly (parsed from the magnet URI itself,
 * no DHT wait for metadata). Metadata is fetched in the background by
 * librqbit as part of the download itself. Use this for user-initiated
 * downloads (release assets, feed clicks) where a spinner staring at
 * the user for 10 minutes while list-only metadata resolves is not
 * acceptable UX.
 */
export async function quickAddMagnet(magnet: string): Promise<{ infoHash: string; name: string }> {
  if (!isTauri) throw new Error('Торрент-клиент доступен только в десктоп-приложении')
  return await invoke<{ infoHash: string; name: string }>('quick_start_magnet', { magnet })
}

export async function getActiveTorrents(): Promise<TorrentStatus[]> {
  if (!isTauri) return []
  try {
    return await invoke<TorrentStatus[]>('get_active_torrents')
  } catch {
    return []
  }
}

export async function pauseTorrent(infoHash: string): Promise<void> {
  if (isTauri) await invoke('pause_torrent', { infoHash })
}
export async function resumeTorrent(infoHash: string): Promise<void> {
  if (isTauri) await invoke('resume_torrent', { infoHash })
}
export async function removeTorrent(infoHash: string, deleteFiles = false): Promise<void> {
  if (isTauri) await invoke('remove_torrent', { infoHash, deleteFiles })
}
export async function openTorrentFolder(infoHash: string): Promise<void> {
  if (isTauri) await invoke('open_torrent_folder', { infoHash })
}

/**
 * Open a torrent's payload with the system default app. For a single-file torrent
 * it opens the file itself; for a multi-file torrent it falls back to the folder.
 */
export async function openTorrentFile(infoHash: string): Promise<void> {
  if (isTauri) await invoke('open_torrent_file', { infoHash })
}

/** Magnet-ссылка активного торрента (для «Скопировать magnet»). */
export async function getTorrentMagnet(infoHash: string): Promise<string> {
  if (!isTauri) return ''
  return invoke<string>('get_torrent_magnet', { infoHash })
}

/** Список файлов активного торрента (для выбора медиафайла в плеере). */
export async function torrentListFiles(infoHash: string): Promise<TorrentFile[]> {
  if (!isTauri) return []
  return invoke<TorrentFile[]>('torrent_list_files', { infoHash })
}

/** URL локального HTTP-стрима файла торрента (играет во время скачивания). */
export async function torrentStreamUrl(infoHash: string, fileIdx: number): Promise<string> {
  if (!isTauri) throw new Error('Стриминг доступен только в десктоп-приложении')
  return invoke<string>('torrent_stream_url', { infoHash, fileIdx })
}

/** .torrent-файл активного торрента в Base64 (для экспорта «Сохранить как»). */
export async function getTorrentFileBytes(infoHash: string): Promise<string> {
  if (!isTauri) return ''
  return invoke<string>('get_torrent_file_bytes', { infoHash })
}

/** Публичные BT-трекеры (кэш из GitHub) для формы создания торрента. */
export async function getBtTrackers(): Promise<string[]> {
  if (!isTauri) return []
  return invoke<string[]>('get_bt_trackers')
}

/** Принудительно скачать свежий список трекеров с GitHub (кнопка Update trackers). */
export async function refreshBtTrackers(): Promise<string[]> {
  return invoke<string[]>('refresh_bt_trackers')
}

/** Создать .torrent из файла/папки, сохранить в кэш и сразу начать раздачу. */
export async function createTorrentFromPath(opts: {
  sourcePath: string
  pieceLength?: number | null
  trackers: string[]
  comment?: string
  private?: boolean
  webSeeds?: string[]
}): Promise<TorrentMetadata> {
  return invoke<TorrentMetadata>('create_torrent_from_path', {
    sourcePath: opts.sourcePath,
    pieceLength: opts.pieceLength ?? null,
    trackers: opts.trackers,
    comment: opts.comment ?? null,
    private: opts.private ?? false,
    webSeeds: opts.webSeeds ?? [],
  })
}

/** Подписка на событие `torrent-progress` (раз в секунду, массив статусов). */
export async function onTorrentProgress(
  handler: (list: TorrentStatus[]) => void,
): Promise<() => void> {
  return appListen<TorrentStatus[]>('torrent-progress', handler)
}

export interface TorrentLimits {
  downloadKib: number // KiB/s, 0 = без ограничения
  uploadKib: number
}

/** Общие ограничения скорости торрент-сессии (KiB/s; 0 = безлимит). */
export async function getTorrentLimits(): Promise<TorrentLimits> {
  if (!isTauri) return { downloadKib: 0, uploadKib: 0 }
  try {
    return await invoke<TorrentLimits>('get_torrent_limits')
  } catch {
    return { downloadKib: 0, uploadKib: 0 }
  }
}

export async function setTorrentLimits(downloadKib: number, uploadKib: number): Promise<void> {
  if (isTauri) await invoke('set_torrent_limits', { downloadKib, uploadKib })
}

/** Сетевые настройки BitTorrent (порт входящих / UPnP / µTP / лимит пиров). */
export interface TorrentNetSettings {
  listenPort: number // 0 = случайный порт
  upnp: boolean
  utp: boolean // TCP+µTP (нужно для пиров за CGNAT/NAT)
  peerLimit: number // 0 = дефолт
  dht: boolean // DHT — децентрализованный поиск пиров
  lsd: boolean // LSD — обнаружение локальных пиров
}

const NET_DEFAULTS: TorrentNetSettings = {
  listenPort: 0,
  upnp: true,
  utp: true,
  peerLimit: 0,
  dht: true,
  lsd: true,
}

export async function getTorrentNetSettings(): Promise<TorrentNetSettings> {
  if (!isTauri) return { ...NET_DEFAULTS }
  try {
    return await invoke<TorrentNetSettings>('get_torrent_net_settings')
  } catch {
    return { ...NET_DEFAULTS }
  }
}

export async function setTorrentNetSettings(settings: TorrentNetSettings): Promise<void> {
  if (isTauri) await invoke('set_torrent_net_settings', { settings })
}

/** Число узлов в собственной BitTorrent-DHT торрент-клиента (как в qBittorrent). */
export async function getTorrentDhtNodes(): Promise<number> {
  if (!isTauri) return 0
  try {
    return await invoke<number>('get_torrent_dht_nodes')
  } catch {
    return 0
  }
}

/** Приватный хранительский ключ RuTracker (bt) — для приватных раздач. */
export async function getRutrackerKey(): Promise<string> {
  if (!isTauri) return ''
  try {
    return await invoke<string>('get_rutracker_key')
  } catch {
    return ''
  }
}
export async function setRutrackerKey(key: string): Promise<void> {
  if (isTauri) await invoke('set_rutracker_key', { key })
}

/** Экономика Byte-Hours: вклад ноды в раздачу и её ранг. */
export interface ProfileStats {
  byteHours: number // ГиБ·ч (объём в раздаче × время)
  rank: string // Leech | Peer | Seeder | Anchor
  uploadedBytes: number
  downloadedBytes: number
  nextRankAt: number | null // порог следующего ранга (ГиБ·ч), null для Anchor
  progressToNext: number // [0..1]
}

const PROFILE_STATS_EMPTY: ProfileStats = {
  byteHours: 0,
  rank: 'Leech',
  uploadedBytes: 0,
  downloadedBytes: 0,
  nextRankAt: 1,
  progressToNext: 0,
}

export async function getProfileStats(): Promise<ProfileStats> {
  if (!isTauri) return { ...PROFILE_STATS_EMPTY }
  try {
    return await invoke<ProfileStats>('get_profile_stats')
  } catch {
    return { ...PROFILE_STATS_EMPTY }
  }
}

/* --------- Репутация --------- */

/** Репутация адреса (для отображения под именем в профиле). */
export async function getReputation(address: string): Promise<number> {
  if (!isTauri) return 0
  try {
    return await invoke<number>('get_reputation', { address })
  } catch {
    return 0
  }
}

/** Дизлайк пользователя: платно (1 STH → 90% сжигание + 10% автору), −1 к репутации. */
export async function dislikeUser(target: string): Promise<string> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<string>('dislike_user', { target })
}


/* --------- P2P-обновления (blockchain-anchored + torrent) --------- */

export interface UpdateInfo {
  app: string
  version: string
  hash: string
  magnet: string
  infoHash: string
}

export interface UpdateManifest {
  version: string
  releases: Record<string, string>
  notes: string
}

/** Период автопроверки обновлений (часы) из CHECK_UPDATE_PERIOD. */
export async function getUpdatePeriodHours(): Promise<number> {
  if (!isTauri) return 48
  try {
    return await invoke<number>('get_update_period_hours')
  } catch {
    return 48
  }
}

/** Текущая версия клиента (CARGO_PKG_VERSION). */
export async function getClientVersion(): Promise<string> {
  if (!isTauri) return '0.0.0'
  try {
    return await invoke<string>('get_client_version')
  } catch {
    return '0.0.0'
  }
}

/** Deep link холодного старта (dev:// / sth:// / api:// из argv первого запуска). */
export async function takePendingDeepLink(): Promise<string | null> {
  if (!isTauri) return null
  try {
    return await invoke<string | null>('take_pending_deep_link')
  } catch {
    return null
  }
}

/** ОС клиента: windows | macos | linux | android. */
export async function getCurrentOs(): Promise<string> {  if (!isTauri) return 'unknown'
  try {
    return await invoke<string>('current_os')
  } catch {
    return 'unknown'
  }
}

/** Просканировать блокчейн STH; вернуть обновление, если версия новее. */
export async function checkForUpdate(): Promise<UpdateInfo | null> {
  if (!isTauri) return null
  return invoke<UpdateInfo | null>('check_for_update')
}

/** Начать фоновую загрузку обновления; возвращает info_hash раздачи. */
export async function startUpdateDownload(version: string, hash: string): Promise<string> {
  return invoke<string>('start_update_download', { version, hash })
}

/** Сброс повреждённого пакета обновления (раздача + папка версии) — для повторной загрузки. */
export async function resetUpdatePackage(version: string, hash: string): Promise<void> {
  return invoke<void>('reset_update_package', { version, hash })
}

/** Memory Guard: снимок ресурсов процесса; `alerts` — превышенные пороги («RSS 1620 MB»). */
export interface ResourceSnapshot {
  rssMb: number
  commitMb: number
  peakCommitMb: number
  handles: number
  gdi: number
  user: number
  heapLiveMb: number
  heapPeakMb: number
  bigAllocs: number
  lastBigAllocMb: number
  /** перепись подсистем: [["sled on-disk MB", 42], ...] */
  census: Array<[string, number]>
  alerts: string[]
  /** диск повреждён (NTFS 1392/1393/CRC) — нужен chkdsk; где поймали */
  diskCorrupt: boolean
  diskCorruptWhere: string
}
export async function getProcessResources(): Promise<ResourceSnapshot> {
  return invoke<ResourceSnapshot>('get_process_resources')
}

/** Прочитать manifest.json из скачанного пакета версии. */
export async function readUpdateManifest(version: string): Promise<UpdateManifest> {
  return invoke<UpdateManifest>('read_update_manifest', { version })
}

/** Абсолютный путь к файлу инсталлятора по имени (рекурсивный поиск). */
export async function resolveInstallerPath(version: string, filename: string): Promise<string> {
  return invoke<string>('resolve_installer_path', { version, filename })
}

/** Путь к инсталлятору для ОС: по имени из manifest, затем fallback по расширению. */
export async function resolveInstaller(version: string, osType: string): Promise<string> {
  return invoke<string>('resolve_installer', { version, osType })
}

/** Удалить старые папки обновлений, оставив указанные версии. */
export async function cleanupUpdateDirs(keepVersions: string[]): Promise<void> {
  if (isTauri) await invoke('cleanup_update_dirs', { keepVersions })
}

/** Запустить инсталлятор и закрыть приложение. */
export async function runP2pInstaller(installerPath: string, osType: string): Promise<void> {
  await invoke('run_p2p_installer', { installerPath, osType })
}


/* ============================================================
 *  ЛОКАЛЬНЫЙ нейро-переводчик Стены (candle + Qwen2.5 GGUF)
 * ============================================================ */

export interface TranslatorStatus {
  installed: boolean
  downloading: boolean
  disk_free_mb: number
  disk_ok: boolean
  ram_low: boolean
  threads: number
  accelerator: string
  /** Настроенное имя пресета/модели (например `qwen25-1.5b`). */
  model_name: string
  /** Файл-имя модели на диске (`data/models/<file>`). */
  model_file: string
  /** Человекочитаемое описание модели (для Settings). */
  model_description: string
  /** Поддерживается ли модель текущим движком (candle). */
  model_supported: boolean
  /** Причина, если `model_supported=false` (например Bonsai Q1_0). */
  model_unsupported_reason: string
  /** Размер пакета модели, МБ (из конфига пресета). */
  model_size_mb: number
}

export interface TranslatorPreset {
  name: string
  description: string
  supported: boolean
  unsupported_reason: string
}

/** Полный список встроенных пресетов моделей (для UI выпадающего списка). */
export async function translatorPresets(): Promise<TranslatorPreset[]> {
  if (!isTauri) return []
  return invoke<TranslatorPreset[]>('translator_presets')
}

export interface TranslatorProgress {
  phase: 'tokenizer' | 'model' | 'torrent' | 'done' | 'error'
  pct: number
  downloaded_mb: number
  total_mb: number
  error?: string | null
}

/** Статус языкового пакета (установлен / идёт загрузка / место / RAM). */
export async function translatorStatus(): Promise<TranslatorStatus> {
  if (!isTauri) return {
    installed: false, downloading: false, disk_free_mb: 0, disk_ok: true,
    ram_low: false, threads: 1, accelerator: 'cpu',
    model_name: '', model_file: '', model_description: '',
    model_supported: true, model_unsupported_reason: '', model_size_mb: 0,
  }
  return invoke<TranslatorStatus>('translator_status')
}

/** Запустить фоновую загрузку языкового пакета (~1.8 ГБ). */
export async function translatorDownload(): Promise<void> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  await invoke('translator_download')
}

/** Запустить фоновую загрузку языкового пакета ТОРРЕНТОМ (основной путь). Идемпотентно. */
export async function translatorBootstrap(): Promise<void> {
  if (!isTauri) return
  try {
    await invoke('translator_bootstrap')
  } catch {
    /* уже идёт / установлен — игнор */
  }
}

/** Подписка на прогресс загрузки пакета. */
export async function onTranslatorProgress(handler: (p: TranslatorProgress) => void): Promise<() => void> {
  return appListen<TranslatorProgress>('translator-progress', handler)
}

/** Текущий статус загрузки модели (для восстановления индикатора после навигации). */
export async function translatorProgress(): Promise<TranslatorProgress | null> {
  if (!isTauri) return null
  return invoke<TranslatorProgress | null>('translator_progress')
}

/** Определить язык текста (ISO-639-1, best-effort). '' — не определено. */
export async function detectLang(text: string): Promise<string> {
  if (!isTauri) return ''
  try {
    return await invoke<string>('detect_lang', { text })
  } catch {
    return ''
  }
}

/** Перевести текст поста/комментария на target_lang (кэш Sled → локальный инференс). */
export async function translateText(author: string, postId: string, text: string, targetLang: string): Promise<string> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<string>('translate_text', { author, postId, text, targetLang })
}

/** Фоновый пред-перевод постов подписок в кэш (лента откроется уже переведённой). */
export async function translatePrewarm(targetLang: string): Promise<void> {
  if (!isTauri) return
  try {
    await invoke('translate_prewarm', { targetLang })
  } catch {
    /* пакет не установлен / уже идёт — молча игнорируем */
  }
}

export interface TranslationCacheStats {
  count: number
  bytes: number
  avg_bytes: number
}

/** Статистика кэша переводов (число записей, объём, средний размер). */
export async function translationCacheStats(): Promise<TranslationCacheStats> {
  if (!isTauri) return { count: 0, bytes: 0, avg_bytes: 0 }
  return invoke<TranslationCacheStats>('translation_cache_stats')
}

/** Очистить кэш переводов целиком. Возвращает число удалённых записей. */
export async function clearTranslationCache(): Promise<number> {
  if (!isTauri) return 0
  return invoke<number>('clear_translation_cache')
}

// ============================ dev:// protocol (Фаза 1) ============================

export type DevTarget =
  | { target: 'user_profile'; username: string }
  | { target: 'repository'; username: string; repo: string; subsystem: string }

export interface DevRepository {
  name: string
  /** Адрес автора — для аватара в шапке репозитория. */
  address: string
  description: string
  hash: string
  updatedAt: string
  branches: string[]
  defaultBranch: string
  stars: number
  commits: number
  language: string
  seeders: number
  license: string
  releases: DevReleaseInfo[]
  /** Подписанные git-рефы (SHA последнего коммита каждой ветки). */
  refs?: DevRefInfo[]
  /** История коммитов (новые сверху, кап 50). */
  log?: DevCommitInfo[]
  /** «username/repo» источника форка (пусто, если это не fork). */
  forkedFrom?: string
}

export interface DevUser {
  username: string
  address: string
  bio: string
  avatarCid: string
  repositories: DevRepository[]
}

export interface DevSearchResult {
  username: string
  /** Адрес автора — для аватара в списках DevHub. */
  address: string
  repo: string
  description: string
  hash: string
  stars: number
  seeders: number
}

/** Git-реф репозитория (SHA коммита + имя ветки/тега). */
export interface DevRefInfo {
  sha: string
  name: string
}

/** Ассет релиза — файл из f:// (качается только по явному клику). */
export interface DevAssetInfo {
  name: string
  size: number
  cid: string
  /** magnet:?xt=urn:btih:… — торрент-транспорт релиза (v1.62+). Пусто
   *  для старых релизов, тогда UI прячет кнопку «Download» или скрывает. */
  magnet?: string
  /** v1.63.2 Publisher WebSeed (BEP-19): опциональный HTTP(S)-mirror
   *  URL (GitHub Release, S3, own CDN). Гарантирует 100% доступность
   *  релиза даже при 0 живых пирах в swarm — librqbit fallback'ится
   *  на mirror. Устанавливается автором в форме релиза. */
  mirror?: string
}

/** Релиз репозитория (аналог GitHub Releases). */
export interface DevReleaseInfo {
  tag: string
  title: string
  notes: string
  created: number
  assets: DevAssetInfo[]
}

/** Метаданные репозитория внутри подписанного индекса автора. */
export interface DevRepoMeta {
  name: string
  description: string
  license: string
  language: string
  defaultBranch: string
  branches: string[]
  created: number
  updated: number
  commits: number
  releases: DevReleaseInfo[]
  bundleHash: string
  refs: DevRefInfo[]
}

/** Элемент списка файлов (вкладка «Код»). */
export interface DevFileEntry {
  name: string
  path: string
  size: number
  isDir: boolean
}

/** Содержимое файла для просмотрщика кода. */
export interface DevFileContent {
  content: string
  size: number
  binary: boolean
  truncated: boolean
}

// --- web-fallback: детерминированный mock (зеркалит dev_router.rs) ---
function devHash(seed: string): string {
  let h = 2166136261
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  const hex = (h >>> 0).toString(16).padStart(8, '0')
  return (hex + hex + hex + hex + hex).slice(0, 40)
}
function devMockRepo(username: string, repo: string): DevRepository {
  const h = devHash(`${username}/${repo}`)
  const b = (i: number) => parseInt(h.slice(i * 2, i * 2 + 2), 16) || 1
  const langs = ['Rust', 'TypeScript', 'Vue', 'Go', 'Python', 'C++']
  return {
    name: repo,
    address: 'S' + devHash('addr:' + username).slice(0, 33),
    description: `Decentralized project «${repo}» — seeded by author @${username} over dev://.`,
    hash: h,
    updatedAt: String(Math.floor(Date.now() / 1000) - 86400),
    branches: ['main', 'dev', 'feat/p2p'],
    defaultBranch: 'main',
    stars: (b(0) * 3) % 900 + 3,
    commits: (b(2) * 7 + b(3)) % 1800 + 12,
    language: langs[b(5) % langs.length],
    seeders: (b(4) % 12) + 1,
    license: 'MIT',
    releases: [],
    refs: [{ sha: devHash(`head:${username}/${repo}`), name: 'refs/heads/main' }],
    log: [
      {
        sha: devHash(`head:${username}/${repo}`),
        message: 'Improve swarm piece scheduling',
        author: username,
        date: Math.floor(Date.now() / 1000) - 86400,
      },
      {
        sha: devHash(`c2:${username}/${repo}`),
        message: 'Initial commit',
        author: username,
        date: Math.floor(Date.now() / 1000) - 5 * 86400,
      },
    ],
  }
}
function devValidate(field: string, s: string): void {
  if (!s) throw { kind: field === 'username' ? 'missing_username' : 'invalid_characters', field, value: s }
  if (!/^[A-Za-z0-9._-]+$/.test(s)) throw { kind: 'invalid_characters', field, value: s }
}

/** Разобрать dev://-URL в типизированную цель (или пробросить DevRouterError). */
export async function parseDevUrl(url: string): Promise<DevTarget> {
  if (isTauri) return invoke<DevTarget>('parse_dev_url', { url })
  const u = (url || '').trim()
  if (!u) throw { kind: 'empty_url' }
  const clean = u.split(/[?#]/)[0]
  if (!clean.startsWith('dev://')) throw { kind: 'wrong_scheme', got: clean.split('://')[0] }
  const segs = clean.slice(6).split('/').filter(Boolean)
  if (segs.length === 0) throw { kind: 'missing_username' }
  if (segs.length > 3) throw { kind: 'too_many_segments', path: segs.join('/') }
  devValidate('username', segs[0])
  if (segs.length === 1) return { target: 'user_profile', username: segs[0] }
  devValidate('repo', segs[1])
  if (segs[2]) devValidate('subsystem', segs[2])
  return { target: 'repository', username: segs[0], repo: segs[1], subsystem: segs[2] || 'code' }
}

/** Профиль автора + его репозитории. */
export async function getUserRepositories(username: string): Promise<DevUser> {
  if (isTauri) return invoke<DevUser>('get_user_repositories', { username })
  devValidate('username', username)
  const names = ['smart-swarm', 'iroh-bridge', 'dev-protocol', 'sled-cache']
  return {
    username,
    address: 'S' + devHash('addr:' + username).slice(0, 33),
    bio: `Author @${username} — contributor to the decentralized SmartNet ecosystem.`,
    avatarCid: '',
    repositories: names.map((r) => devMockRepo(username, r)),
  }
}

/** Данные одного репозитория (dashboard проекта). */
export async function getRepository(username: string, repo: string): Promise<DevRepository> {
  if (isTauri) return invoke<DevRepository>('get_repository', { username, repo })
  devValidate('username', username)
  devValidate('repo', repo)
  return devMockRepo(username, repo)
}

/** Глобальный P2P-поиск проектов. */
export async function searchDevNetwork(query: string): Promise<DevSearchResult[]> {
  if (isTauri) return invoke<DevSearchResult[]>('search_dev_network', { query })
  const q = (query || '').trim().toLowerCase()
  const index: [string, string, string][] = [
    ['technolog', 'smart-swarm', 'Multi-armed-bandit swarm download engine'],
    ['technolog', 'iroh-bridge', 'IP:Port → Iroh EndpointId bridge for DHT'],
    ['alice', 'candle-translate', 'Local on-device AI translation via quantized Qwen2.5'],
    ['bob', 'sled-vault', 'Encrypted KV store on top of Sled'],
    ['carol', 'vue-p2p-ui', 'Vue 3 components for P2P applications'],
    ['dave', 'dht-tracker', 'Trackerless discovery via Mainline DHT'],
  ]
  return index
    .filter(([u, r, d]) => !q || u.includes(q) || r.includes(q) || d.toLowerCase().includes(q))
    .map(([u, r, d]) => {
      const repo = devMockRepo(u, r)
      return { username: u, address: repo.address, repo: r, description: d, hash: repo.hash, stars: repo.stars, seeders: repo.seeders }
    })
}

// ============================ DevHub (Фаза 2) ============================

/** Создать новый пустой репозиторий (README.md + LICENSE.md + подписанный индекс). */
export async function devhubCreateRepo(
  name: string,
  description: string,
  license: string,
  addReadme: boolean,
): Promise<DevRepoMeta> {
  if (isTauri) return invoke<DevRepoMeta>('devhub_create_repo', { name, description, license, addReadme })
  throw 'Создание репозитория доступно только в приложении SmartNet'
}

/** Удалить свой репозиторий (диск + подписанный индекс). */
export async function devhubDeleteRepo(name: string): Promise<void> {
  if (isTauri) return invoke<void>('devhub_delete_repo', { name })
}

/** Список файлов репозитория (вкладка «Код»). */
export async function devhubListFiles(username: string, repo: string, path: string): Promise<DevFileEntry[]> {
  if (isTauri) return invoke<DevFileEntry[]>('devhub_list_files', { username, repo, path })
  return [
    { name: 'README.md', path: 'README.md', size: 128, isDir: false },
    { name: 'LICENSE.md', path: 'LICENSE.md', size: 1071, isDir: false },
  ]
}

/** Содержимое файла для просмотрщика кода. */
export async function devhubReadFile(username: string, repo: string, path: string): Promise<DevFileContent> {
  if (isTauri) return invoke<DevFileContent>('devhub_read_file', { username, repo, path })
  return { content: `# ${repo}\n\nDemo content (web preview).`, size: 128, binary: false, truncated: false }
}

/** Локальны ли файлы репозитория (false у чужого до P2P-синка). */
export async function devhubFilesLocal(username: string, repo: string): Promise<boolean> {
  if (isTauri) return invoke<boolean>('devhub_files_local', { username, repo })
  return true
}

/** Скачать файлы чужого репозитория от сидеров (bundle → checkout на диск). */
export async function devhubSyncFiles(username: string, repo: string): Promise<void> {
  if (isTauri) return invoke<void>('devhub_sync_files', { username, repo })
}

/** ⑂ Fork чужого репозитория к себе (как на GitHub). Возвращает моё имя. */
export async function devhubForkRepo(username: string, repo: string): Promise<string> {
  if (isTauri) return invoke<string>('devhub_fork_repo', { username, repo })
  throw 'Fork доступен только в приложении SmartNet'
}

/** Результат «Sync fork»: подтянуть свежие коммиты оригинала в мой форк. */
export interface DevSyncForkResult {
  changed: boolean
  aheadBy: number
  newHead: string
  source: string
}

/** ⇅ Sync fork: подтянуть свежие коммиты оригинала в мой форк (как на GitHub). */
export async function devhubSyncFork(repo: string): Promise<DevSyncForkResult> {
  if (isTauri) return invoke<DevSyncForkResult>('devhub_sync_fork', { repo })
  throw 'Sync fork доступен только в приложении SmartNet'
}


/** Дневная активность автора («YYYY-MM-DD» → контрибуции) для зелёного графа. */
export async function devhubActivity(username: string): Promise<Record<string, number>> {
  if (isTauri) return invoke<Record<string, number>>('devhub_activity', { username })
  // web-мок: детерминированная псевдослучайная активность
  const out: Record<string, number> = {}
  for (let i = 0; i < 365; i++) {
    const k = new Date(Date.now() - i * 86400000).toISOString().slice(0, 10)
    const h = (username.charCodeAt(0) * 31 + i * 7) % 13
    if (h < 4) out[k] = h + 1
  }
  return out
}

export interface DevEventInfo {
  repo: string
  action: string
  /** Сообщение коммита (для push). */
  msg?: string
}

/** События конкретного дня («какие репо обновлялись») по клику на клетку графа. */
export async function devhubDayEvents(username: string, date: string): Promise<DevEventInfo[]> {
  if (isTauri) return invoke<DevEventInfo[]>('devhub_day_events', { username, date })
  return [
    { repo: 'smart-swarm', action: 'push' },
    { repo: 'iroh-bridge', action: 'created' },
  ]
}

/** Список "username/repo", которые отметил я (для кнопки ⭐). */
export async function devhubStarredByMe(): Promise<string[]> {
  if (isTauri) return invoke<string[]>('devhub_starred_by_me')
  return []
}

/** Поставить/снять подписанную ⭐. Возвращает новое состояние. */
export async function devhubToggleStar(author: string, repo: string): Promise<boolean> {
  if (isTauri) return invoke<boolean>('devhub_toggle_star', { author, repo })
  return true
}

/** Немедленный опрос сети: синк индексов всех известных авторов DevHub. Возвращает число опрашиваемых. */
export async function devhubDiscoverNetwork(): Promise<number> {
  if (isTauri) return invoke<number>('devhub_discover_network')
  return 0
}

/** Элемент ленты DevHub: свежий пуш известного автора. */
export interface DevFeedItem {
  username: string
  address: string
  repo: string
  message: string
  updated: number
  followed: boolean
  /** Последний тегированный релиз (если есть) — бейдж версии + assets. */
  latestRelease?: DevReleaseInfo
}

/** Лента свежих пушей авторов сети (отсортирована по времени обновления). */
export async function devhubFeed(): Promise<DevFeedItem[]> {
  if (isTauri) {
    try {
      return await invoke<DevFeedItem[]>('devhub_feed')
    } catch {
      return []
    }
  }
  // web-фолбэк: собираем ленту из mock-репозиториев
  const now = Math.floor(Date.now() / 1000)
  return [
    ['technolog', 'smart-swarm', 'fix: stable DHT re-announce', 3600, true, 'v0.4.2'],
    ['alice', 'candle-translate', 'feat: INT8 quantization', 86400, false, ''],
    ['technolog', 'netfory-core', 'chore: bump iroh to 1.0', 172800, true, 'v1.0.0'],
  ].map(([u, r, m, ago, f, tag]) => {
    const repo = devMockRepo(u as string, r as string)
    const t = tag as string
    const latestRelease: DevReleaseInfo | undefined = t
      ? {
          tag: t,
          title: `Release ${t}`,
          notes: '',
          created: now - (ago as number),
          assets: [
            { name: `${r}-${t}.tar.gz`, size: 1024 * 128, cid: `mock-cid-${u}-${r}-${t}` },
          ],
        }
      : undefined
    return {
      username: u as string,
      address: repo.address,
      repo: r as string,
      message: m as string,
      updated: now - (ago as number),
      followed: f as boolean,
      latestRelease,
    }
  })
}

// ---------------- Pull Requests (децентрализованные, подписанные) ----------------

/** Файл внутри PR: полное новое содержимое либо пометка удаления. */
export interface DevPrFile {
  path: string
  content: string
  deleted: boolean
}

/** Подписанный Pull Request. */
export interface DevPullRequest {
  id: string
  author: string
  address: string
  targetAuthor: string
  repo: string
  title: string
  body: string
  baseSha: string
  files: DevPrFile[]
  created: number
  /** open | merged | rejected */
  status: string
}

/** Создать и опубликовать подписанный PR в чужой репозиторий. */
export async function devhubCreatePr(
  targetAuthor: string,
  repo: string,
  title: string,
  body: string,
  baseSha: string,
  files: DevPrFile[],
): Promise<DevPullRequest> {
  if (isTauri)
    return invoke<DevPullRequest>('devhub_create_pr', { targetAuthor, repo, title, body, baseSha, files })
  throw 'Pull Request доступны только в приложении SmartNet'
}

/** PR репозитория: входящие (моё репо) либо мои исходящие (чужое). */
export async function devhubListPrs(author: string, repo: string): Promise<DevPullRequest[]> {
  if (isTauri) return invoke<DevPullRequest[]>('devhub_list_prs', { author, repo })
  return []
}

/** Принять PR: файлы применяются, публикуется новый commit/bundle. */
export async function devhubAcceptPr(id: string): Promise<void> {
  if (isTauri) return invoke<void>('devhub_accept_pr', { id })
  throw 'Доступно только в приложении SmartNet'
}

/** Отклонить PR (локальный статус). */
export async function devhubRejectPr(id: string): Promise<void> {
  if (isTauri) return invoke<void>('devhub_reject_pr', { id })
  throw 'Доступно только в приложении SmartNet'
}

/** Число открытых входящих PR (бейдж DEV HUB в сайдбаре). */
export async function devhubPrOpenCount(): Promise<number> {
  if (isTauri) return invoke<number>('devhub_pr_open_count')
  return 0
}

/** Дифф коммита (git show из бандла). Требует установленный git CLI. */
export async function devhubCommitDiff(username: string, repo: string, sha: string): Promise<string> {
  if (isTauri) return invoke<string>('devhub_commit_diff', { username, repo, sha })
  return [
    `commit ${sha}`,
    'Author: ' + username,
    '',
    '    Improve swarm piece scheduling',
    '',
    ' src/scheduler.rs | 12 ++++++------',
    ' 1 file changed, 6 insertions(+), 6 deletions(-)',
    '',
    'diff --git a/src/scheduler.rs b/src/scheduler.rs',
    '--- a/src/scheduler.rs',
    '+++ b/src/scheduler.rs',
    '@@ -10,7 +10,7 @@ impl Scheduler {',
    '     fn next_piece(&self) -> Option<u32> {',
    '-        self.queue.pop_front()',
    '+        self.queue.pop_by_priority()',
    '     }',
  ].join('\n')
}

/** Создать релиз с ассетами из f:// (качаются только по явному клику). */
export async function devhubCreateRelease(
  repo: string,
  tag: string,
  title: string,
  notes: string,
  assets: DevAssetInfo[],
): Promise<DevReleaseInfo> {
  if (isTauri) return invoke<DevReleaseInfo>('devhub_create_release', { repo, tag, title, notes, assets })
  throw 'Creating releases is only available in the SmartNet desktop app'
}

/** Edit release metadata (title + notes). Tag and assets stay unchanged. */
export async function devhubEditRelease(
  repo: string,
  tag: string,
  title: string,
  notes: string,
): Promise<DevReleaseInfo> {
  if (isTauri) return invoke<DevReleaseInfo>('devhub_edit_release', { repo, tag, title, notes })
  throw 'Editing releases is only available in the SmartNet desktop app'
}

/** Delete a release from the signed index (torrents/assets stay untouched). */
export async function devhubDeleteRelease(repo: string, tag: string): Promise<void> {
  if (isTauri) return invoke<void>('devhub_delete_release', { repo, tag })
  throw 'Deleting releases is only available in the SmartNet desktop app'
}


// --- Публичный WS-мост для встроенного тестера (api:// панель) --------------
// Использует те же Tauri-команды, что и JS-полифилл, инжектируемый в DApp'ы,
// но вызывается напрямую из главного UI SmartNet. Нужен для проверки WS-туннеля
// (netfory/api/1) без необходимости запускать сторонний DApp.

export interface DappWsOpenResult { streamId: string; status: number }
export interface DappWsEventOpen { streamId: string; status: number }
export interface DappWsEventMessage { streamId: string; kind: 'text' | 'binary'; data: string }
export interface DappWsEventClose { streamId: string; code: number; reason: string }

/**
 * Открыть WS-туннель через P2P-провайдера.
 *
 * v1.55.32 — возвращает `{ streamId, status }` из самого Promise (Rust
 * возвращает эту структуру синхронно после handshake ACK). Раньше status
 * приходил через отдельное событие `dapp-ws-open`, но эта модель имела race
 * condition: событие могло долететь до JS РАНЬШЕ, чем resolve'ился invoke,
 * поэтому listener'ы с guard'ом `if (!localStreamId) return` теряли его →
 * ws не переходил в `open` → таймаут 8с. Теперь ждём invoke, и статус уже
 * там: race невозможен.
 */
export async function dappWsOpen(
  url: string,
  headers: Record<string, string> = {},
): Promise<DappWsOpenResult> {
  if (!isTauri) throw new Error('WS доступен только в десктоп-сборке (P2P)')
  const raw = await invoke<any>('dapp_ws_open', { url, headers })
  // backward-compat: старые сборки могли вернуть просто строку
  if (typeof raw === 'string') return { streamId: raw, status: 0 }
  const streamId = raw?.stream_id ?? raw?.streamId
  const status = raw?.status ?? 0
  if (!streamId) throw new Error('dapp_ws_open: пустой stream_id в ответе')
  return { streamId, status }
}

/** Отправить кадр (text|binary — base64) в открытый WS-поток. */
export async function dappWsSend(streamId: string, kind: 'text' | 'binary', data: string): Promise<void> {
  if (!isTauri) return
  await invoke('dapp_ws_send', { streamId, kind, data })
}

/** Закрыть WS-поток. */
export async function dappWsClose(streamId: string, code = 1000, reason = ''): Promise<void> {
  if (!isTauri) return
  try { await invoke('dapp_ws_close', { streamId, code, reason }) } catch { /* ignore */ }
}

/** Подписки на события WS-туннеля. */
export function onDappWsOpen(cb: (p: DappWsEventOpen) => void) { return appListen<DappWsEventOpen>('dapp-ws-open', cb) }
export function onDappWsMessage(cb: (p: DappWsEventMessage) => void) { return appListen<DappWsEventMessage>('dapp-ws-message', cb) }
export function onDappWsClose(cb: (p: DappWsEventClose) => void) { return appListen<DappWsEventClose>('dapp-ws-close', cb) }

/* ────────────────────────── Crypto Vault (ntfry) ──────────────────────────
 * Zero-Knowledge зашифрованное облачное хранилище (Rust-ядро ntfry):
 * чанки 128КБ → ZSTD → ChaCha20-Poly1305 → Yandex Disk / local mock.
 * Прогресс push/pull приходит событием `vault-progress`. */

export interface CloudVaultStats {
  rawBytes: number
  storedBytes: number
  dedupSavedBytes: number
}
export interface CloudVaultStatus {
  provider: string
  account: string
  address: string
  files: number
  stats: CloudVaultStats
}
export interface CloudVaultRow {
  name: string
  isDir: boolean
  size: number
  modified: number
}
export interface CloudVaultPushResult {
  size: number
  chunks: number
  uploaded: number
  deduped: number
  compressedBytes: number
  stats: CloudVaultStats
}
export interface CloudVaultProgress {
  op: 'push' | 'pull'
  file: string
  done: number
  total: number
}

/** Открыть Vault. Пустая мнемоника → секрет текущего кошелька SmartNet.
 *  `account` — id аккаунта из настроек (мульти-диск): раздельный Sled-кэш. */
export async function cloudVaultOpen(
  provider: string,
  mnemonic: string,
  token: string,
  account = '',
): Promise<CloudVaultStatus> {
  if (!isTauri) throw new Error('Crypto Vault доступен только в десктоп-приложении')
  return invoke<CloudVaultStatus>('cloud_vault_open', {
    provider,
    mnemonic: mnemonic || null,
    token: token || null,
    account: account || null,
  })
}

export async function cloudVaultClose(): Promise<void> {
  if (isTauri) await invoke('cloud_vault_close')
}

export async function cloudVaultStatus(): Promise<CloudVaultStatus | null> {
  if (!isTauri) return null
  return invoke<CloudVaultStatus | null>('cloud_vault_status')
}

/** Листинг VFS-каталога из In-Memory индекса (мгновенно, без сети). */
export async function cloudVaultLs(path: string): Promise<CloudVaultRow[]> {
  if (!isTauri) return []
  return invoke<CloudVaultRow[]>('cloud_vault_ls', { path })
}

/** PUSH локального файла в VFS-каталог (chunk → zstd → chacha20 → cloud). */
export async function cloudVaultPush(srcPath: string, dstDir: string): Promise<CloudVaultPushResult> {
  if (!isTauri) throw new Error('Crypto Vault доступен только в десктоп-приложении')
  return invoke<CloudVaultPushResult>('cloud_vault_push', { srcPath, dstDir })
}

/** PULL файла из VFS через нативный «Сохранить как». false — отмена. */
export async function cloudVaultPull(srcVfs: string): Promise<boolean> {
  if (!isTauri) throw new Error('Crypto Vault доступен только в десктоп-приложении')
  return invoke<boolean>('cloud_vault_pull', { srcVfs })
}

export interface YandexOauthResult {
  accessToken: string
  expiresIn: number
}

/** «Sign in with Yandex»: браузер + loopback-перехват кода → access_token.
 *  Требует redirect URI http://127.0.0.1:8971/callback в настройках
 *  Yandex-приложения. */
export async function cloudVaultYandexOauth(): Promise<YandexOauthResult> {
  if (!isTauri) throw new Error('OAuth доступен только в десктоп-приложении')
  return invoke<YandexOauthResult>('cloud_vault_yandex_oauth')
}

/** Сброс облачного vault: удаляет /ntfry на провайдере и пересоздаёт пустой
 *  контейнер под ключ текущего кошелька (когда индекс зашифрован чужим ключом). */
export async function cloudVaultReset(provider: string, token: string, account = ''): Promise<CloudVaultStatus> {
  if (!isTauri) throw new Error('Crypto Vault доступен только в десктоп-приложении')
  return invoke<CloudVaultStatus>('cloud_vault_reset', { provider, token: token || null, account: account || null })
}

/** Обновлённые OAuth-типы vault: access + refresh + срок жизни. */
export interface VaultOauthTokens {
  accessToken: string
  refreshToken: string
  expiresIn: number
}

export async function cloudVaultYandexRefresh(refreshToken: string): Promise<VaultOauthTokens> {
  if (!isTauri) throw new Error('OAuth доступен только в десктоп-приложении')
  return invoke<VaultOauthTokens>('cloud_vault_yandex_refresh', { refreshToken })
}

/** «Sign in with Google»: системный браузер + loopback (Google запрещает webview). */
export async function cloudVaultGoogleOauth(): Promise<VaultOauthTokens> {
  if (!isTauri) throw new Error('OAuth доступен только в десктоп-приложении')
  return invoke<VaultOauthTokens>('cloud_vault_google_oauth')
}

export async function cloudVaultGoogleRefresh(refreshToken: string): Promise<VaultOauthTokens> {
  if (!isTauri) throw new Error('OAuth доступен только в десктоп-приложении')
  return invoke<VaultOauthTokens>('cloud_vault_google_refresh', { refreshToken })
}

/** Создать (пустой) каталог в VFS. */
export async function cloudVaultMkdir(path: string): Promise<void> {
  if (!isTauri) return
  await invoke('cloud_vault_mkdir', { path })
}

/** Удалить файл/каталог (рекурсивно) + GC осиротевших чанков в облаке. */
export async function cloudVaultDelete(path: string): Promise<number> {
  if (!isTauri) return 0
  return invoke<number>('cloud_vault_delete', { path })
}

export interface CloudVaultQuota {
  totalBytes: number
  usedBytes: number
}
export interface CloudVaultDictStatus {
  present: boolean
  filesUsingDict: number
}
export interface CloudVaultDictTrain {
  size: number
  samples: number
}

/** Квота облачного диска (total/used). null — провайдер не отдаёт. */
export async function cloudVaultQuota(): Promise<CloudVaultQuota | null> {
  if (!isTauri) return null
  return invoke<CloudVaultQuota | null>('cloud_vault_quota')
}

export async function cloudVaultDictStatus(): Promise<CloudVaultDictStatus | null> {
  if (!isTauri) return null
  return invoke<CloudVaultDictStatus>('cloud_vault_dict_status')
}

/** Обучение ZSTD-словаря на файлах выбранной папки. null — отмена диалога. */
export async function cloudVaultDictTrain(): Promise<CloudVaultDictTrain | null> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<CloudVaultDictTrain | null>('cloud_vault_dict_train')
}

/** Шифрование/дешифрование строк мастер-ключом кошелька (для токенов). */
export async function cloudVaultSeal(plain: string): Promise<string> {
  return invoke<string>('cloud_vault_seal', { plain })
}
export async function cloudVaultUnseal(sealed: string): Promise<string> {
  return invoke<string>('cloud_vault_unseal', { sealed })
}

/** Логин и квота аккаунта по OAuth-токену (без открытия vault) — для
 *  списка дисков в мульти-аккаунт UI. */
export interface CloudVaultAccountInfo {
  login: string
  totalBytes: number
  usedBytes: number
}
export async function cloudVaultAccountInfo(provider: string, token: string): Promise<CloudVaultAccountInfo> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<CloudVaultAccountInfo>('cloud_vault_account_info', { provider, token })
}

/** «Занято сейфом» на конкретном диске — быстрое чтение из локального Sled
 *  кэша индекса, без открытия vault. Для активной сессии возвращает свежее
 *  RAM-значение; для незнакомых аккаунтов — 0. */
export async function cloudVaultCachedStored(provider: string, account?: string): Promise<number> {
  if (!isTauri) return 0
  return invoke<number>('cloud_vault_cached_stored', { provider, account: account ?? null })
}

/* ── RAID-режим: объединение всех дисков в одно большое хранилище ── */

export interface RaidMember {
  provider: string
  token: string
  id: string
}

function raidMembersArg(members: RaidMember[]) {
  return members.map((m) => ({ provider: m.provider, token: m.token || null, id: m.id }))
}

export async function cloudVaultOpenRaid(members: RaidMember[], mode: 'stripe' | 'mirror' = 'stripe'): Promise<CloudVaultStatus> {
  if (!isTauri) throw new Error('Crypto Vault доступен только в десктоп-приложении')
  return invoke<CloudVaultStatus>('cloud_vault_open_raid', { members: raidMembersArg(members), mode })
}

export async function cloudVaultResetRaid(members: RaidMember[], mode: 'stripe' | 'mirror' = 'stripe'): Promise<CloudVaultStatus> {
  if (!isTauri) throw new Error('Crypto Vault доступен только в десктоп-приложении')
  return invoke<CloudVaultStatus>('cloud_vault_reset_raid', { members: raidMembersArg(members), mode })
}

export interface RaidResyncResult {
  checked: number
  copied: number
  failed: number
  lost: number
}

/** RAID-1 re-sync: докатка недостающих чанков на члены зеркала. */
export async function cloudVaultRaidResync(): Promise<RaidResyncResult> {
  if (!isTauri) throw new Error('Crypto Vault доступен только в десктоп-приложении')
  return invoke<RaidResyncResult>('cloud_vault_raid_resync')
}

/* ── Drag&Drop: скан брошенных путей для очереди загрузки ── */

export interface VaultScanFile {
  src: string
  rel: string
  size: number
}
export interface VaultScanResult {
  files: VaultScanFile[]
  emptyDirs: string[]
  totalBytes: number
}

/** Рекурсивный обход файлов/папок (скрытые пропускаются) для очереди. */
export async function cloudVaultScanPaths(paths: string[]): Promise<VaultScanResult> {
  if (!isTauri) throw new Error('Доступно только в десктоп-приложении')
  return invoke<VaultScanResult>('cloud_vault_scan_paths', { paths })
}

/* ── Cloud Seeder (ntfryseed): публичный анти-цензурный слой ── */

export interface SeederPublishResult {
  uri: string
  publicKey: string
  dapps: number
  nodes: number
  seeders: number
  archives: number
  updated: string
}

export interface SeederStatus {
  ok: boolean
  checkedAt: string
  lastOk?: string | null
  error?: string | null
}

export interface SeederFetchResult {
  provider: string
  publicKey: string
  dapps: { v: number; updated: string; items: any[] } | null
  nodes: { v: number; updated: string; items: any[] } | null
  seeders: { v: number; updated: string; items: string[] } | null
}

/** Провайдеры Cloud Seeder: Yandex/Google — OAuth-токен, Mail.ru — email:пароль_приложения. */
export type SeederProviderId = 'yandex' | 'mailru' | 'gdrive'

/** Публикация стейта сети (dApps/ноды/сидеры) в /ntfryseed на облаке автора.
 *  provider: 'yandex'/'gdrive' (token = OAuth) | 'mailru' (token = email:пароль_приложения).
 *  mailruWeblink: weblink папки ntfryseed, опубликованной вручную (фолбэк для
 *  VK ID-логинов, где неофициальный API публикации недоступен).
 *  secondary: публикация на ВТОРОЙ (зеркальный) диск — URI пишется в отдельный слот. */
export async function cloudSeederPublish(
  token: string,
  provider: SeederProviderId = 'yandex',
  mailruWeblink?: string,
  secondary = false,
): Promise<SeederPublishResult> {
  if (!isTauri) throw new Error('Cloud Seeder доступен только в десктоп-приложении')
  return invoke<SeederPublishResult>('cloud_seeder_publish', {
    token,
    provider,
    mailruWeblink: mailruWeblink || null,
    secondary,
  })
}

/** Чтение чужого сидера по sn://seed/… URI (анонимно, без OAuth). */
export async function cloudSeederFetch(uri: string): Promise<SeederFetchResult> {
  if (!isTauri) throw new Error('Cloud Seeder доступен только в десктоп-приложении')
  return invoke<SeederFetchResult>('cloud_seeder_fetch', { uri })
}

const LS_SEEDERS = 'sn.cloudseeders'

/** Реестр известных Cloud Seeder URI (Sled в Tauri, localStorage в вебе). */
export async function cloudSeederRegistryGet(): Promise<string[]> {
  if (!isTauri) {
    try {
      const raw = localStorage.getItem(LS_SEEDERS)
      if (raw !== null) return JSON.parse(raw)
      // первый запуск: сеем стартовые якоря из .env CLOUD_SEEDERS
      const defaults = (((import.meta as any).env?.CLOUD_SEEDERS as string) || '')
        .split(',')
        .map((s) => s.trim())
        .filter((s) => s.startsWith('sn://seed/'))
      localStorage.setItem(LS_SEEDERS, JSON.stringify(defaults))
      return defaults
    } catch {
      return []
    }
  }
  return invoke<string[]>('cloud_seeder_registry_get')
}

export async function cloudSeederRegistrySet(list: string[]): Promise<void> {
  if (!isTauri) {
    localStorage.setItem(LS_SEEDERS, JSON.stringify(list))
    return
  }
  await invoke('cloud_seeder_registry_set', { list })
}

/** Источник добавления каждого известного сидера в реестр.
 *  Значения: 'own' — свой опубликованный URI, 'manual' — пользователь
 *  добавил вручную, 'gossip' — пришёл по P2P-анонсу, 'seed_file' — из
 *  чужого seeders.enc, 'bootstrap' — стартовый якорь из .env. */
export type SeederOrigin = 'own' | 'manual' | 'gossip' | 'seed_file' | 'bootstrap'

export async function cloudSeederRegistryOriginsGet(): Promise<Record<string, SeederOrigin>> {
  if (!isTauri) return {}
  return invoke<Record<string, SeederOrigin>>('cloud_seeder_registry_origins_get')
}

/** Установка dApp напрямую из облачного сидера (без P2P). */
export async function cloudSeederInstall(uri: string, appId: string): Promise<Manifest> {
  if (!isTauri) throw new Error('Cloud Seeder доступен только в десктоп-приложении')
  return invoke<Manifest>('cloud_seeder_install', { uri, appId })
}

/** Установка dApp из ЛЮБОГО известного сидера с архивом (перебор реестра). */
export async function cloudSeederInstallAny(appId: string): Promise<Manifest> {
  if (!isTauri) throw new Error('Cloud Seeder доступен только в десктоп-приложении')
  return invoke<Manifest>('cloud_seeder_install_any', { appId })
}

/** Удаление архива dApp со СВОЕГО cloud seed диска (после локального удаления). */
export async function cloudSeederDeleteDapp(
  token: string,
  provider: SeederProviderId,
  appId: string,
): Promise<void> {
  if (!isTauri) throw new Error('Cloud Seeder доступен только в десктоп-приложении')
  await invoke('cloud_seeder_delete_dapp', { token, provider, appId })
}

export interface SeederScanOut {
  seedersOk: number
  seedersTotal: number
  nodes: number
  dapps: number
}

/** Скан облачных дисков всех известных Cloud Seeder-ов: найденные dApps
 *  вливаются в каталог Discover как via-seeder пиры (P2P не нужен). */
export async function cloudSeederScanApps(): Promise<SeederScanOut> {
  if (!isTauri) return { seedersOk: 0, seedersTotal: 0, nodes: 0, dapps: 0 }
  return invoke<SeederScanOut>('cloud_seeder_scan_apps')
}

/** Статусы последних проверок сидеров: uri → {ok, checkedAt, lastOk, error}. */
export async function cloudSeederStatusGet(): Promise<Record<string, SeederStatus>> {
  if (!isTauri) return {}
  return invoke<Record<string, SeederStatus>>('cloud_seeder_status_get')
}

/** Счётчик «спасённых» установок за сессию: uri → сколько раз сидер выручил. */
export async function cloudSeederRescuesGet(): Promise<Record<string, number>> {
  if (!isTauri) return {}
  return invoke<Record<string, number>>('cloud_seeder_rescues_get')
}

export interface SeederPublishProgress {
  phase: 'meta' | 'archive' | 'done'
  index: number
  total: number
  appId: string
  sizeBytes: number
  /** размер tar до zstd-сжатия — для показа степени сжатия */
  rawBytes: number
  status: 'packing' | 'uploading' | 'done' | 'error'
  kbps: number
  /** байт уже передано облаку в текущей заливке — для live-скорости */
  uploadedBytes: number
  /** номер текущей попытки заливки (1..=3) — бейдж «попытка N/3» при ретраях */
  attempt: number
  error?: string | null
}

/** Live-прогресс публикации стейта сети и архивов dApp в облако. */
export function onSeederPublishProgress(cb: (p: SeederPublishProgress) => void) {
  return appListen<SeederPublishProgress>('seeder-publish-progress', cb)
}

/** Реестр сидеров пополнился (gossip-анонс или merge чужого seeders.enc). */
export function onSeederRegistryUpdated(cb: () => void) {
  return appListen<unknown>('seeder-registry-updated', () => cb())
}

// ── Public Profile Seeders (bio + microblog в облако) ────────────────────

/** Результат публикации профилей: собственный + топ-10 подписок. */
export interface ProfilesPublishOut {
  uri: string
  usersPublished: number
  usersSkipped: number
  filesUploaded: number
  filesSkipped: number
  orphansDeleted: number
  updated: number
}

/** Опубликовать свой профиль + подписки в /ntfryseed/users/ на облаке. */
export async function cloudSeederPublishProfiles(
  token: string,
  provider: SeederProviderId = 'yandex',
  mailruWeblink?: string,
  secondary = false,
): Promise<ProfilesPublishOut> {
  return invoke<ProfilesPublishOut>('cloud_seeder_publish_profiles', {
    token,
    provider,
    mailruWeblink: mailruWeblink || null,
    secondary,
  })
}

export interface ProfileManifestEntry {
  userId: string
  name?: string | null
  hash: string
  updated: number
  posts: number
  files: string[]
}

export interface ProfilesManifest {
  v: number
  updated: number
  users: ProfileManifestEntry[]
}

/** Читатель: получить индекс всех опубликованных профилей у чужого сидера. */
export async function cloudSeederFetchProfilesManifest(uri: string): Promise<ProfilesManifest> {
  return invoke<ProfilesManifest>('cloud_seeder_fetch_profiles_manifest', { uri })
}

export interface CloudPostFile {
  cid: string
  name: string
  size: number
  mime: string
  node?: string
  title?: string
  artist?: string
  cover?: string
}

export interface CloudPost {
  id: string
  author: string
  text: string
  images: string[]
  files: CloudPostFile[]
  avatar: string
  node: string
  ts: number
  pubkey: string
  sig: string
}

export interface CloudComment {
  id: string
  postId: string
  postAuthor: string
  author: string
  text: string
  ts: number
  node: string
  avatar: string
  pubkey: string
  sig: string
}

export interface CloudProfilePublic {
  identity: string
  rewardAddress: string
  isPublic: boolean
  showDomains: boolean
  updatedAt: string
  name?: string | null
  avatarCid: string
  avatarNode: string
}

export interface CloudUserBundle {
  v: number
  userId: string
  profile: CloudProfilePublic
  posts: Array<{ post: CloudPost; comments: CloudComment[] }>
  files: Array<{ cid: string; name: string; size: number; mime: string }>
  updated: number
}

/** Читатель: получить полный bundle одного пользователя. */
export async function cloudSeederFetchProfile(
  uri: string,
  userId: string,
): Promise<CloudUserBundle> {
  return invoke<CloudUserBundle>('cloud_seeder_fetch_profile', { uri, userId })
}

/** Читатель: получить один прикреплённый файл (base64 plain-контента). */
export async function cloudSeederFetchProfileFile(
  uri: string,
  userId: string,
  cid: string,
): Promise<string> {
  return invoke<string>('cloud_seeder_fetch_profile_file', { uri, userId, cid })
}

export async function profileSeederAutoRepGet(): Promise<boolean> {
  if (!isTauri) return false
  return invoke<boolean>('profile_seeder_autorep_get')
}

export async function profileSeederAutoRepSet(on: boolean): Promise<void> {
  if (!isTauri) return
  await invoke('profile_seeder_autorep_set', { on })
}

export async function profileSeederLastReplicated(): Promise<number> {
  if (!isTauri) return 0
  return invoke<number>('profile_seeder_last_replicated')
}

/** Дешёвая проверка: изменился ли хоть один профиль/пост с прошлой репликации. */
export async function profileSeederHasChanges(): Promise<boolean> {
  if (!isTauri) return false
  return invoke<boolean>('profile_seeder_has_changes')
}

export interface ProfilePublishProgress {
  phase: 'collect' | 'user' | 'file' | 'manifest' | 'done'
  index: number
  total: number
  userId: string
  fileCid: string
  sizeBytes: number
  uploadedBytes: number
  kbps: number
  attempt: number
  status: 'packing' | 'uploading' | 'skipped' | 'done' | 'error'
  error?: string | null
}

export function onProfilePublishProgress(cb: (p: ProfilePublishProgress) => void) {
  return appListen<ProfilePublishProgress>('profile-publish-progress', cb)
}

// ============================================================
//  AI Computer (sn://ai-computer) — Netfory AI Pool, Phase 1
// ============================================================

export interface AiGpuInfo {
  model: string
  vramTotalMb: number
  vramFreeMb: number
  tflopsFp16: number
}

export interface AiCpuInfo {
  threads: number
  ramTotalMb: number
  ramFreeMb: number
}

export interface AiHwSnapshot {
  gpu: AiGpuInfo | null
  cpu: AiCpuInfo
}

export interface AiPoolStatus {
  enabled: boolean
  status: 'disabled' | 'connected' | 'syncing'
  lastHeartbeat: number
}

export interface AiPipeHop {
  /** iroh node id шарда */
  id: string
  /** диапазон слоёв [from..to) */
  from: number
  to: number
  /** рекуррентных (DeltaNet) слоёв; null у dense-моделей */
  rec?: number | null
  /** full-attention слоёв в диапазоне */
  attn?: number
  /** RTT шлюз→шард при сборке цепочки, мс */
  rttMs?: number | null
  vramMb?: number
}

export interface AiPipeMetrics {
  ttftMs: number
  tokPerSec: number
  requests: number
  batch: number
  /** попаданий префикс-кэша (нет у нод < 0.8.0) */
  prefixHits?: number
  /** последний TTFT без кэша (полный prefill), мс */
  ttftFullMs?: number
  /** последний TTFT с префикс-кэшем, мс */
  ttftCacheMs?: number
  /** спекулятивная генерация: раундов верификации / принято токенов */
  specRounds?: number
  specAccepted?: number
  /** живая схема цепочки шардов (граф роя), нет у нод < 0.9.0 */
  chain?: AiPipeHop[]
  /** EMA времени шага декода ШАРДА, мс (у шардовых нод, не у шлюза) */
  stepMs?: number | null
  /** continuous batching: активные слоты / лимит (нет у нод < 0.11.0) */
  slots?: number
  maxSlots?: number
}

export interface AiNodeInfo {
  /** STH-адрес ноды */
  node: string
  /** версия бинарника (нет у старых нод) */
  version?: string | null
  /** true = AI-воркер (ai://), false = телеметрия клиента */
  worker: boolean
  /** iroh NodeId воркера (ai://-адрес; нет у трекеров < v1.6) */
  irohNodeId?: string | null
  gpu: boolean
  vramFreeMb: number
  vramTotalMb: number
  tflopsFp16: number
  models: string[]
  shardReady: boolean
  throttled: boolean
  lastSeen: number
  /** метрики конвейера (только у шлюзов) */
  pipe?: AiPipeMetrics | null
}

export interface AiPipelineInfo {
  /** базовый alias модели (без @a..b) */
  model: string
  /** сколько шардовых диапазонов загружено */
  shards: number
  /** диапазоны слоёв: ["0..16", "16..32", …] */
  ranges: string[]
  /** нод с полной (нешардированной) моделью */
  fullNodes: number
  /** живой tok/s: сумма по шлюзам модели (нет у трекеров < v1.6) */
  tokPerSec?: number
  /** шлюзов, отдающих pipe-метрики */
  gateways?: number
  /** мин. цена, STH за 1 млн токенов (null/0 = бесплатно) */
  pricePerMtok?: number | null
  /** continuous batching: занятые слоты / суммарный лимит */
  slots?: number
  maxSlots?: number
}

export interface AiNetStats {
  activeNodes: number
  gpuNodes: number
  totalVramGb: number
  totalVramFreeGb: number
  totalTflopsFp16: number
  totalThreads: number
  totalRamGb: number
  totalRamFreeGb: number
  /** alias модели → число нод, где она загружена */
  models?: Record<string, number>
  /** нод в ожидании назначения слоёв (--shard-auto) */
  shardReadyNodes?: number
  /** загруженных шардовых диапазонов — активные конвейеры */
  activeShards?: number
  /** активные конвейеры по моделям (шарды + диапазоны слоёв) */
  pipelines?: AiPipelineInfo[]
  /** активные ноды с версиями (до 200 последних) */
  nodes?: AiNodeInfo[]
  /** каталог опубликованных моделей (publish-model --alias) */
  modelTickets?: AiModelTicket[]
  updatedAt: number
}

export interface AiModelTicket {
  alias: string
  ticket: string
  registeredAt: number
}

/** Скачать модель каталога в выбранный файл (Save As). null = отменено. */
export async function aiModelDownload(alias: string, ticket: string): Promise<string | null> {
  if (!isTauri) return null
  return invoke<string | null>('ai_model_download', { alias, ticket })
}

/** Занятое кешем скачанных AI-моделей место (app-data/ai-models-store), байты. */
export async function aiModelsCacheStats(): Promise<number> {
  if (!isTauri) return 0
  try {
    return await invoke<number>('ai_models_cache_stats')
  } catch {
    return 0
  }
}

/** Очистить кеш скачанных AI-моделей. Возвращает освобождённые байты. */
export async function aiModelsCacheClear(): Promise<number> {
  if (!isTauri) return 0
  return invoke<number>('ai_models_cache_clear')
}

const AI_STATUS_OFF: AiPoolStatus = { enabled: false, status: 'disabled', lastHeartbeat: 0 }

/** Текущее состояние тумблера AI Pool + статус подключения к AI-трекеру. */
export async function aiPoolStatus(): Promise<AiPoolStatus> {
  if (!isTauri) return AI_STATUS_OFF
  return invoke<AiPoolStatus>('ai_pool_status')
}

/** Включить/выключить участие в AI Pool (персистентно, sled). */
export async function aiPoolToggle(enabled: boolean): Promise<AiPoolStatus> {
  if (!isTauri) return AI_STATUS_OFF
  return invoke<AiPoolStatus>('ai_pool_toggle', { enabled })
}

/** Локальный снимок железа (GPU VRAM/TFLOPS, потоки CPU, ОЗУ). */
export async function aiHwSnapshot(): Promise<AiHwSnapshot | null> {
  if (!isTauri) return null
  try {
    return await invoke<AiHwSnapshot>('ai_hw_snapshot')
  } catch {
    return null
  }
}

/** Агрегированные AI Stats всей сети (GET /v1/ai/stats с трекеров). */
export async function aiNetStats(): Promise<AiNetStats | null> {
  if (!isTauri) return null
  try {
    return await invoke<AiNetStats>('ai_net_stats')
  } catch {
    return null
  }
}

export interface AiConsoleResult {
  text: string
  node: string
  ms: number
}

/** AI Чат Консоль (DEV): стриминговая генерация из децентрализованной сети.
 *  Токены прилетают событием `dapp-ai-token` {streamId, t} (см. appListen),
 *  промис резолвится финальным результатом. */
export async function aiConsoleGenerate(
  streamId: string,
  prompt: string,
  model?: string,
  maxTokens = 2048,
  think = true,
): Promise<AiConsoleResult> {
  if (!isTauri) throw new Error('AI-сеть доступна только в десктоп-версии')
  return invoke<AiConsoleResult>('ai_console_generate', {
    streamId,
    prompt,
    model: model || null,
    maxTokens,
    think,
  })
}

/** Остановить активную консольную генерацию. */
export async function aiConsoleCancel(streamId: string): Promise<void> {
  if (!isTauri) return
  try {
    await invoke('ai_console_cancel', { streamId })
  } catch {
    /* уже завершена */
  }
}

/* ---------------- Автозагрузка (Настройки → Интерфейс) ---------------- */
export interface AutostartSettings {
  /** крестик сворачивает в системный трей (иначе — выход) */
  closeToTray: boolean
  /** запуск при входе в систему (сразу свёрнуто в трей) */
  launchAtLogin: boolean
}
const AUTOSTART_MOCK_KEY = 'mock_autostart'
export async function autostartGet(): Promise<AutostartSettings> {
  if (!isTauri) {
    try { return { closeToTray: true, launchAtLogin: false, ...JSON.parse(localStorage.getItem(AUTOSTART_MOCK_KEY) || '{}') } } catch { return { closeToTray: true, launchAtLogin: false } }
  }
  try { return await invoke<AutostartSettings>('autostart_get') } catch { return { closeToTray: true, launchAtLogin: false } }
}
export async function autostartSet(settings: AutostartSettings): Promise<AutostartSettings> {
  if (!isTauri) {
    try { localStorage.setItem(AUTOSTART_MOCK_KEY, JSON.stringify(settings)) } catch { /* noop */ }
    return { ...settings }
  }
  return await invoke<AutostartSettings>('autostart_set', { settings })
}

/* ---------------- Панель сайта (Mycelium): трафик и IP хопов ---------------- */
export interface SiteBytes { key: string; rx: number; tx: number; hist?: [number, number][] }
const siteBytesMock: Record<string, SiteBytes> = {}
/** Получено/отправлено через Mycelium для сайта (группа CDN или базовый домен). */
export async function webproxySiteBytes(host: string): Promise<SiteBytes> {
  if (!isTauri) {
    const k = host.split('.').slice(-2).join('.')
    const m = (siteBytesMock[k] ||= { key: k, rx: 0, tx: 0 })
    if (myceliumMock.settings?.enabled) { m.rx += 40000 + Math.floor(Math.random() * 90000); m.tx += 2000 + Math.floor(Math.random() * 6000) }
    return { ...m, hist: Array.from({ length: 24 }, (_, i) => [((i * 7919) % 13) * 80000, ((i * 104729) % 11) * 4000] as [number, number]) }
  }
  try { return await invoke<SiteBytes>('webproxy_site_bytes', { host }) } catch { return { key: host, rx: 0, tx: 0 } }
}
/* ---------------- Страна выхода pq-web для сайта (GeoIP по адресам узла) ---------------- */
export interface ExitNode { id: string; addr: string; iso: string | null; region: string; mismatch: boolean; verified?: boolean }
export interface ExitCountries { key: string; direct?: boolean; pinned: string; default: string; effective: string; fallback: boolean; current: ExitNode | null; countries: { iso: string; nodes: number }[] }
export interface ExitIpResult { ip: string; iso: string | null; city: string; org: string; service: string; node: string; expected: string | null; announced: string[]; ipMatch: boolean; countryMatch: boolean }
const exitMock = { pins: {} as Record<string, string>, gen: {} as Record<string, number>, def: '' }
const exitMockNodes = [
  { id: 'pqde1'.padEnd(52, 'a'), iso: 'DE', addr: '88.198.12.7:7843', region: 'de' },
  { id: 'pqde2'.padEnd(52, 'b'), iso: 'DE', addr: '136.243.10.5:7843', region: 'de' },
  { id: 'pqnl1'.padEnd(52, 'c'), iso: 'NL', addr: '51.15.84.20:7843', region: 'nl' },
]
/** Страны, где есть узлы pq-web `--open`, текущий выход сайта и закреп. */
export async function webproxyExitCountries(host: string): Promise<ExitCountries | null> {
  if (!isTauri) {
    const k = host.split('.').slice(-2).join('.'), pin = exitMock.pins[k] || ''
    const eff = pin === '*' ? '' : pin || exitMock.def
    const f = exitMockNodes.filter((n) => !eff || n.iso === eff), pool = f.length ? f : exitMockNodes
    const c = pool[(exitMock.gen[k] || 0) % pool.length]
    return { key: k, pinned: pin, default: exitMock.def, effective: eff, fallback: !!eff && !f.length, current: host ? { ...c, mismatch: false } : null, countries: [{ iso: 'DE', nodes: 2 }, { iso: 'NL', nodes: 1 }] }
  }
  try { return await invoke<ExitCountries>('webproxy_exit_countries', { host }) } catch { return null }
}
/** Закрепить страну выхода за сайтом ('' = Авто). */
export async function webproxySetExitCountry(host: string, iso: string): Promise<void> {
  if (!isTauri) { const k = host.split('.').slice(-2).join('.'); if (iso) exitMock.pins[k] = iso; else delete exitMock.pins[k]; return }
  await invoke('webproxy_set_exit_country', { host, iso })
}
/** Страна выхода по умолчанию для всех сайтов ('' = Авто). */
export async function webproxySetExitDefault(iso: string): Promise<void> {
  if (!isTauri) { exitMock.def = iso; return }
  await invoke('webproxy_set_exit_default', { iso })
}
/** «Проверить мой IP»: HTTPS к сервису IP через тот же узел pq-web, что у сайта. */
export async function webproxyCheckIp(host: string): Promise<ExitIpResult> {
  if (!isTauri) {
    const e = await webproxyExitCountries(host)
    await new Promise((r) => setTimeout(r, 800))
    const ip = (e?.current?.addr || '').replace(/:\d+$/, '')
    return { ip, iso: e?.current?.iso ?? null, city: '', org: 'mock', service: 'ifconfig.co', node: e?.current?.id || '', expected: e?.current?.iso ?? null, announced: [ip], ipMatch: true, countryMatch: true }
  }
  return await invoke<ExitIpResult>('webproxy_check_ip', { host })
}
/** Прервать идущую проверку IP. */
export async function webproxyCheckIpCancel(host: string): Promise<void> {
  if (!isTauri) return
  try { await invoke('webproxy_check_ip_cancel', { host }) } catch { /* старый бэкенд */ }
}
/** «Новая личность»: следующий выход сайта — другой узел pq-web; возвращает прежний id. */
export async function webproxyNewIdentity(host: string): Promise<string> {
  if (!isTauri) { const k = host.split('.').slice(-2).join('.'); exitMock.gen[k] = (exitMock.gen[k] || 0) + 1; return '' }
  try { return await invoke<string>('webproxy_new_identity', { host }) } catch { return '' }
}
/** id хопа → публичный IP (подсказки адреса) — для геопривязки маршрута на глобусе. */
export async function myceliumHopAddrs(ids: string[]): Promise<Record<string, string>> {
  if (!isTauri) {
    const ips = ['88.198.12.7', '51.15.84.20', '185.70.42.9', '95.216.33.1', '146.103.108.25']
    return Object.fromEntries(ids.map((id, i) => [id, ips[i % ips.length]]))
  }
  try { return await invoke<Record<string, string>>('mycelium_hop_addrs', { ids }) } catch { return {} }
}
