/*
 * SmartNet // crypto.ts
 * SmartHoldem (ARK/Crypti-style) key + address derivation — matches the
 * official Core-Wallet (github.com/smartholdem/Core-Wallet).
 *
 * The secret (any passphrase / private key string — NOT restricted to BIP-39)
 * is hashed to the secp256k1 private key, then:
 *
 *   privateKey  = SHA-256(utf8(secret))
 *   publicKey   = secp256k1(privateKey)            // 33-byte compressed
 *   address     = base58check( 0x3F | RIPEMD160(publicKey) )   // -> "S..."
 *
 * NETWORK (pubKeyHash) = 63 (0x3F)  |  WIF / SLIP44 = 255  |  CURVE secp256k1
 * SUB-UNIT: smartoshi (1e8)
 *
 * Verified vector:
 *   secret  "film actual type miracle where law hidden kid bicycle plastic object save"
 *   address  SXdXoyUAjUyyCC5LES6KH1XS1ewbBB4Dfu
 *
 * Mnemonic/secret-at-rest is AES-sealed; the key is SHA-256(PIN). The raw PIN
 * is never persisted (same approach as Core-Wallet).
 */
import { generateMnemonic, validateMnemonic } from '@scure/bip39'
import { wordlist } from '@scure/bip39/wordlists/english'
import { ripemd160 } from '@noble/hashes/ripemd160'
import { sha256 } from '@noble/hashes/sha256'
import { bytesToHex, hexToBytes } from '@noble/hashes/utils'
import { secp256k1 } from '@noble/curves/secp256k1'
import { base58check } from '@scure/base'
import CryptoJS from 'crypto-js'

export const NETWORK = 63
export const VERSION_BYTE = 0x3f
export const WIF = 255
export const COIN_TYPE = 255 // slip44
export const CURVE = 'secp256k1'
export const SUB_UNIT = 'smartoshi 1e8'

const b58check = base58check(sha256)

export interface DerivedAccount {
  index: number
  path: string
  address: string
  publicKey: string
}

/** Recommended fresh secret: a valid 12-word BIP-39 mnemonic. */
export function newMnemonic(): string {
  return generateMnemonic(wordlist, 128)
}

/** Optional helper — only used to flag "recommended" secrets in the UI. */
export function isValidMnemonic(secret: string): boolean {
  return validateMnemonic(secret.trim().replace(/\s+/g, ' '), wordlist)
}

function privateKeyFromSecret(secret: string): Uint8Array {
  return sha256(new TextEncoder().encode(secret.trim()))
}

function addressFromPublicKey(pub: Uint8Array): string {
  const hash = ripemd160(pub)
  const payload = new Uint8Array(21)
  payload[0] = VERSION_BYTE
  payload.set(hash, 1)
  return b58check.encode(payload)
}

/** Primary SmartHoldem address derived directly from the secret (account 0). */
export function addressFromSecret(secret: string): DerivedAccount {
  return deriveAccount(secret, 0)
}

export function primaryAddress(secret: string): string {
  return addressFromSecret(secret).address
}

/**
 * Derive a SmartHoldem secp256k1 account.
 *  - index 0  -> the secret itself (canonical Core-Wallet address)
 *  - index N  -> deterministic child secret SHA-256(secret + "/" + N),
 *               recoverable from the same master secret.
 */
export function deriveAccount(secret: string, index = 0): DerivedAccount {
  const material = index === 0 ? secret.trim() : `${secret.trim()}/${index}`
  const priv = privateKeyFromSecret(material)
  const pub = secp256k1.getPublicKey(priv, true) // compressed
  return {
    index,
    path: index === 0 ? 'sha256(secret)' : `sha256(secret·${index})`,
    address: addressFromPublicKey(pub),
    publicKey: bytesToHex(pub),
  }
}

function pinKey(pin: string): string {
  return CryptoJS.SHA256(pin).toString()
}

/** AES-seal a secret using the PIN hash as key. */
export function encryptSecret(secret: string, pin: string): string {
  return CryptoJS.AES.encrypt(secret.trim(), pinKey(pin)).toString()
}

/**
 * AES-open a sealed secret. Verifies the PIN by re-deriving the address and
 * comparing it to the expected one (works for any passphrase, not just BIP-39).
 * Returns null on wrong PIN / corrupt data.
 */
export function decryptSecret(
  cipher: string,
  pin: string,
  expectedAddress: string,
): string | null {
  try {
    const bytes = CryptoJS.AES.decrypt(cipher, pinKey(pin))
    const text = bytes.toString(CryptoJS.enc.Utf8)
    if (!text) return null
    if (expectedAddress && primaryAddress(text) !== expectedAddress) return null
    return text
  } catch {
    return null
  }
}

export function shortAddr(addr: string, head = 6, tail = 4): string {
  if (!addr || addr.length <= head + tail) return addr
  return `${addr.slice(0, head)}...${addr.slice(-tail)}`
}

/** SHA-256 of bytes -> hex (used for dApp file hashing / merkle root). */
export function sha256Hex(bytes: Uint8Array): string {
  return bytesToHex(sha256(bytes))
}

/** ECDSA-sign a 32-byte digest with the identity key (account 0). Compact hex. */
export function signDigest(secret: string, digest: Uint8Array): string {
  const priv = privateKeyFromSecret(secret)
  const sig = secp256k1.sign(digest, priv)
  return sig.toCompactHex()
}

/**
 * dApp immutable identity (application space). Mirrors the BIP-44 change
 * branch m/44'/{coin}'/0'/1/index — deterministic per app index, recoverable
 * from the master secret. The resulting SmartHoldem address IS the app URL:
 *   sth://[address]
 */
export function deriveAppIdentity(secret: string, index: number): DerivedAccount {
  const material = `${secret.trim()}/1/${index}`
  const priv = privateKeyFromSecret(material)
  const pub = secp256k1.getPublicKey(priv, true)
  return {
    index,
    path: `m/44'/${COIN_TYPE}'/0'/1/${index}`,
    address: addressFromPublicKey(pub),
    publicKey: bytesToHex(pub),
  }
}

/** Sign a digest with an application identity key (content.json signing). */
export function signDigestForApp(secret: string, index: number, digest: Uint8Array): string {
  const priv = privateKeyFromSecret(`${secret.trim()}/1/${index}`)
  return secp256k1.sign(digest, priv).toCompactHex()
}

export interface VerifyResult {
  verified: boolean
  reason: string
}

/**
 * Content-addressed content.json trust check. A dApp is "verified" only when:
 *   1. its sth:// id == the SmartHoldem address derived from its signing
 *      publicKey  (the URL IS the key — no impersonation possible), AND
 *   2. the ECDSA signature over sha256(`{id}|{name}|{merkle}`) is valid for
 *      that publicKey (content has not been tampered with).
 * This mirrors the native Rust `verify_app` command exactly.
 */
export function verifyAppManifest(m: {
  id: string
  name: string
  description?: string
  merkle: string
  signature: string
  publicKey: string
}): VerifyResult {
  try {
    if (!m.publicKey || !m.signature) return { verified: false, reason: 'no-signature' }
    const pub = hexToBytes(m.publicKey)
    if (addressFromPublicKey(pub) !== m.id) return { verified: false, reason: 'address-mismatch' }
    const digest = sha256(
      new TextEncoder().encode(`${m.id}|${m.name}|${m.description || ''}|${m.merkle}`),
    )
    const ok = secp256k1.verify(hexToBytes(m.signature), digest, pub)
    return ok ? { verified: true, reason: 'ok' } : { verified: false, reason: 'bad-signature' }
  } catch {
    return { verified: false, reason: 'error' }
  }
}
