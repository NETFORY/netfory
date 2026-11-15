// Транспортный адаптер Oxid Mail — ЕДИНАЯ точка обращения UI к ядру.
// Tauri-режим: window.__TAURI__.core.invoke (in-process, без сервера).
// Web-dev: HTTP к oxid-bridge (:8002) с теми же командами.
const isTauri =
  typeof window !== 'undefined' &&
  ('__TAURI_INTERNALS__' in window || '__TAURI__' in window)

const BRIDGE_URL =
  import.meta.env.VITE_BRIDGE_URL ||
  `${window.location.protocol}//${window.location.hostname}:8002`

// REST-путь бриджа -> имя Tauri-команды + аргументы (поверхности идентичны)
function toTauriCall(method, path, body) {
  if (path === '/identity/generate') return ['identity_generate', {}]
  if (path === '/identity/import') return ['identity_import', { mnemonic: body.mnemonic }]
  if (path === '/identity/import-secret') return ['identity_import_secret', { secret: body.secret }]
  if (path === '/session/open') return ['session_open', { mnemonic: body.mnemonic }]
  if (path === '/session/close') return ['session_close', {}]
  if (path === '/session') return ['session_get', {}]
  if (path === '/oauth/gmail') return ['oauth_gmail_sign_in', {}]
  if (path === '/pin/seal') return ['pin_seal', { mnemonic: body.mnemonic, pin: body.pin }]
  if (path === '/pin/open') return ['pin_open', { blob: body.blob, pin: body.pin }]
  if (path === '/accounts') {
    if (method === 'GET') return ['accounts_list', {}]
    if (method === 'POST') return ['account_add', { req: body }]
  }
  let m
  if ((m = path.match(/^\/accounts\/([^/]+)$/))) {
    if (method === 'PUT') return ['account_update', { id: m[1], req: body }]
    if (method === 'DELETE') return ['account_delete', { id: m[1] }]
  }
  if ((m = path.match(/^\/accounts\/([^/]+)\/sync$/))) return ['account_sync', { id: m[1] }]
  if ((m = path.match(/^\/accounts\/([^/]+)\/older$/))) return ['account_older', { id: m[1], folder: body.folder, beforeUid: body.beforeUid, count: body.count }]
  if ((m = path.match(/^\/accounts\/([^/]+)\/updates$/))) return ['account_updates', { id: m[1] }]
  if ((m = path.match(/^\/p2p\/progress\/(.+)$/))) return ['p2p_progress', { pid: decodeURIComponent(m[1]) }]
  if ((m = path.match(/^\/accounts\/([^/]+)\/send$/))) return ['account_send', { id: m[1], req: body }]
  if ((m = path.match(/^\/accounts\/([^/]+)\/bind$/))) {
    if (method === 'POST') return ['account_bind', { id: m[1] }]
    return ['account_bind_status', { id: m[1] }]
  }
  if (path === '/registry/sync') return ['registry_sync', {}]
  if (path === '/registry/lookup') return ['registry_lookup', { email: body.email }]
  if (path === '/contacts') {
    if (method === 'POST') return ['contact_add', { input: body.input || '', name: body.name || '', note: body.note || '' }]
    return ['contacts_list', {}]
  }
  if (path === '/contacts/parse') return ['contact_parse', { input: body.input }]
  if (path === '/naming/my') return ['naming_my', {}]
  if ((m = path.match(/^\/contacts\/(.+)$/)) && method === 'DELETE') {
    return ['contact_remove', { email: decodeURIComponent(m[1]) }]
  }
  if ((m = path.match(/^\/accounts\/([^/]+)\/contact-card$/))) return ['contact_card', { id: m[1] }]
  if ((m = path.match(/^\/accounts\/([^/]+)\/messages\/([^/]+)\/decrypt$/))) {
    return ['message_decrypt', { id: m[1], mid: m[2] }]
  }
  if ((m = path.match(/^\/accounts\/([^/]+)\/messages\/([^/]+)\/verify$/))) {
    return ['message_verify', { id: m[1], mid: m[2] }]
  }
  if ((m = path.match(/^\/accounts\/([^/]+)\/messages$/))) return ['messages_list', { id: m[1] }]
  if ((m = path.match(/^\/accounts\/([^/]+)\/messages\/([^/]+)\/attachments\/(\d+)$/))) {
    return ['attachment_get', { id: m[1], mid: m[2], idx: parseInt(m[3], 10) }]
  }
  if ((m = path.match(/^\/accounts\/([^/]+)\/messages\/([^/]+)$/))) {
    if (method === 'GET') return ['message_get', { id: m[1], mid: m[2] }]
    if (method === 'PATCH') return ['message_patch', { id: m[1], mid: m[2], patch: body }]
    if (method === 'DELETE') return ['message_delete', { id: m[1], mid: m[2] }]
  }
  throw new Error(`No Tauri mapping for ${method} ${path}`)
}

// Открытие http(s)-ссылок из писем в системном браузере (Tauri) / новой вкладке (web)
export async function openExternal(url) {
  const u = String(url || '').trim()
  if (!/^https?:\/\//i.test(u)) return
  if (isTauri) {
    try {
      await window.__TAURI__.core.invoke('open_external', { url: u })
      return
    } catch (e) { /* фолбэк ниже */ }
  }
  window.open(u, '_blank', 'noopener,noreferrer')
}

async function invokeTauri(method, path, body) {
  const core = window.__TAURI__?.core
  if (!core?.invoke) throw new Error('Tauri core unavailable')
  const [cmd, args] = toTauriCall(method, path, body)
  try {
    return await core.invoke(cmd, args)
  } catch (e) {
    throw new Error(typeof e === 'string' ? e : e?.message || String(e))
  }
}

export async function api(method, path, body) {
  if (isTauri) return invokeTauri(method, path, body)
  let res
  try {
    res = await fetch(`${BRIDGE_URL}/api${path}`, {
      method,
      headers: body !== undefined ? { 'Content-Type': 'application/json' } : undefined,
      body: body !== undefined ? JSON.stringify(body) : undefined,
    })
  } catch (e) {
    throw new Error(`oxid-bridge is unreachable at ${BRIDGE_URL} — start it: cargo run -p oxid-bridge`)
  }
  const data = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`)
  return data
}
