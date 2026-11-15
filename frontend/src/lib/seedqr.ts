/*
 * Перенос сида в standalone NETFORY Messenger через QR (v1.135.218).
 * Полезная нагрузка QR: `ntfry://seed/v1?s=<salt b64url>&n=<nonce b64url>&d=<AES-GCM(seed) b64url>[&u=<имя>]`.
 * `u` — имя u:// владельца (публичное, на цепи), открыто: standalone-мессенджер знает имя без Netfory.
 * Ключ = SHA-256("ntfry-seed-qr|v1|" + PIN + "|" + salt). PIN (6 цифр) показывается рядом
 * с QR и вводится на втором устройстве — сам QR без PIN бесполезен.
 */
const PREFIX = 'ntfry://seed/v1?'

const b64u = {
  enc: (b: Uint8Array) => btoa(String.fromCharCode(...b)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, ''),
  dec: (s: string) => Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/').padEnd(Math.ceil(s.length / 4) * 4, '=')), (c) => c.charCodeAt(0)),
}

async function keyFor(pin: string, salt: Uint8Array): Promise<CryptoKey> {
  const material = new TextEncoder().encode(`ntfry-seed-qr|v1|${pin.trim()}|${b64u.enc(salt)}`)
  const digest = await crypto.subtle.digest('SHA-256', material)
  return crypto.subtle.importKey('raw', digest, 'AES-GCM', false, ['encrypt', 'decrypt'])
}

export function randomPin(): string {
  const a = new Uint32Array(1)
  crypto.getRandomValues(a)
  return String(a[0] % 1_000_000).padStart(6, '0')
}

export async function encodeSeedQr(seed: string, pin: string, username = ''): Promise<string> {
  const salt = crypto.getRandomValues(new Uint8Array(16))
  const nonce = crypto.getRandomValues(new Uint8Array(12))
  const key = await keyFor(pin, salt)
  const ct = new Uint8Array(await crypto.subtle.encrypt({ name: 'AES-GCM', iv: nonce }, key, new TextEncoder().encode(seed.trim())))
  const u = username.trim().replace(/^@/, '').toLowerCase()
  return `${PREFIX}s=${b64u.enc(salt)}&n=${b64u.enc(nonce)}&d=${b64u.enc(ct)}${u ? `&u=${encodeURIComponent(u)}` : ''}`
}

/** Имя u:// из payload QR (открытое поле `u`), '' если нет. */
export function seedQrName(payload: string): string {
  if (!isSeedQr(payload)) return ''
  return new URLSearchParams(payload.trim().slice(PREFIX.length)).get('u') || ''
}

export function isSeedQr(s: string): boolean {
  return s.trim().startsWith(PREFIX)
}

/** Расшифровать payload QR по PIN. Ошибка → неверный PIN или повреждённый код. */
export async function decodeSeedQr(payload: string, pin: string): Promise<string> {
  const q = new URLSearchParams(payload.trim().slice(PREFIX.length))
  const s = q.get('s'), n = q.get('n'), d = q.get('d')
  if (!s || !n || !d) throw new Error('bad qr payload')
  const key = await keyFor(pin, b64u.dec(s))
  try {
    const pt = await crypto.subtle.decrypt({ name: 'AES-GCM', iv: b64u.dec(n) }, key, b64u.dec(d))
    return new TextDecoder().decode(pt)
  } catch {
    throw new Error('wrong pin')
  }
}
