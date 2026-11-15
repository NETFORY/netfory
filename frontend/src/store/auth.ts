import { defineStore } from 'pinia'
import {
  deriveAccount,
  encryptSecret,
  decryptSecret,
  type DerivedAccount,
} from '@/lib/crypto'
import {
  vaultExists,
  loadVault,
  saveVault,
  deleteVault,
  setSessionPin,
  getSessionPin,
  clearSession,
  nativeCreateVault,
  nativeUnlockVault,
  nativeDeriveAccount,
  nativeRevealSeed,
  nativeSessionClear,
  wipeAll,
  isTauri,
  type VaultRecord,
} from '@/lib/bridge'

export type VaultStatus = 'empty' | 'locked' | 'unlocked'

interface AuthState {
  status: VaultStatus
  ready: boolean
  isTauri: boolean
  address: string
  publicKey: string
  // web only: decrypted secret kept in memory. In Tauri it stays in Rust.
  mnemonic: string | null
  accounts: DerivedAccount[]
  error: string
  busy: boolean
}

export const useAuthStore = defineStore('auth', {
  state: (): AuthState => ({
    status: 'empty',
    ready: false,
    isTauri,
    address: '',
    publicKey: '',
    mnemonic: null,
    accounts: [],
    error: '',
    busy: false,
  }),

  getters: {
    isUnlocked: (s) => s.status === 'unlocked',
  },

  actions: {
    async init() {
      // Never let a failed IPC call hang the boot screen.
      try {
        const exists = await vaultExists()
        if (!exists) {
          this.status = 'empty'
          return
        }
        const vault = await loadVault()
        if (vault) {
          this.address = vault.address
          this.publicKey = vault.publicKey
        }
        // Web: restore unlocked session after F5 via the volatile PIN.
        if (!isTauri) {
          const sessionPin = getSessionPin()
          if (sessionPin && vault) {
            const ok = await this.unlock(sessionPin, true)
            if (ok) return
          }
        }
        this.status = 'locked'
      } catch (e) {
        // Boot must always complete — fall back to whatever we can determine.
        console.error('[auth.init] failed:', e)
        this.status = this.address ? 'locked' : 'empty'
      } finally {
        this.ready = true
      }
    },

    async createIdentity(mnemonic: string, pin: string) {
      this.busy = true
      this.error = ''
      try {
        if (isTauri) {
          const primary = await nativeCreateVault(mnemonic, pin)
          this.accounts = [primary]
          this.address = primary.address
          this.publicKey = primary.publicKey
          this.mnemonic = null
        } else {
          const primary = deriveAccount(mnemonic, 0)
          const cipher = encryptSecret(mnemonic, pin)
          const record: VaultRecord = {
            cipher,
            address: primary.address,
            publicKey: primary.publicKey,
            accounts: [0],
            createdAt: new Date().toISOString(),
          }
          await saveVault(record)
          this.mnemonic = mnemonic
          this.address = primary.address
          this.publicKey = primary.publicKey
          this.accounts = [primary]
          setSessionPin(pin)
        }
        this.status = 'unlocked'
      } finally {
        this.busy = false
      }
    },

    async unlock(pin: string, silent = false): Promise<boolean> {
      if (!silent) this.busy = true
      this.error = ''
      try {
        if (isTauri) {
          const primary = await nativeUnlockVault(pin)
          if (!primary) {
            if (!silent) this.error = 'wrongPin'
            return false
          }
          this.accounts = [primary]
          this.address = primary.address
          this.publicKey = primary.publicKey
          this.status = 'unlocked'
          return true
        }

        const vault = await loadVault()
        if (!vault) {
          this.status = 'empty'
          return false
        }
        const mnemonic = decryptSecret(vault.cipher, pin, vault.address)
        if (!mnemonic) {
          if (!silent) this.error = 'wrongPin'
          return false
        }
        const indices = vault.accounts?.length ? vault.accounts : [0]
        this.accounts = indices.map((i) => deriveAccount(mnemonic, i))
        this.mnemonic = mnemonic
        this.address = vault.address || this.accounts[0].address
        this.publicKey = vault.publicKey || this.accounts[0].publicKey
        setSessionPin(pin)
        this.status = 'unlocked'
        return true
      } finally {
        if (!silent) this.busy = false
      }
    },

    async lock() {
      this.mnemonic = null
      this.accounts = []
      clearSession()
      await nativeSessionClear()
      this.status = 'locked'
    },

    async deriveNext(): Promise<DerivedAccount | null> {
      const nextIndex = this.accounts.length
      if (isTauri) {
        const acc = await nativeDeriveAccount(nextIndex)
        this.accounts.push(acc)
        return acc
      }
      if (!this.mnemonic) return null
      const acc = deriveAccount(this.mnemonic, nextIndex)
      this.accounts.push(acc)
      const vault = await loadVault()
      if (vault) {
        vault.accounts = this.accounts.map((a) => a.index)
        await saveVault(vault)
      }
      return acc
    },

    /** Reveal the secret phrase (KEYS view). Native fetches from Rust memory. */
    async revealSeed(): Promise<string | null> {
      if (isTauri) return nativeRevealSeed()
      return this.mnemonic
    },

    async reset() {
      await deleteVault()
      this.mnemonic = null
      this.accounts = []
      this.address = ''
      this.publicKey = ''
      this.status = 'empty'
      clearSession()
      await nativeSessionClear()
      window.location.reload()
    },

    /** Full wipe: Sled DB + data/ (native) + all browser storage, then reload. */
    async wipeAll() {
      await wipeAll()
      this.mnemonic = null
      this.accounts = []
      this.address = ''
      this.publicKey = ''
      this.status = 'empty'
      window.location.reload()
    },

    async exportVaultJson(): Promise<string | null> {
      const vault = await loadVault()
      if (!vault) return null
      const envelope = {
        type: 'smartnet-vault',
        version: 1,
        network: 63,
        exportedAt: new Date().toISOString(),
        vault,
      }
      return JSON.stringify(envelope, null, 2)
    },

    async importVaultJson(raw: string): Promise<boolean> {
      try {
        const parsed = JSON.parse(raw)
        const vault: VaultRecord = parsed?.vault ?? parsed
        if (!vault || !vault.cipher || !vault.address) return false
        await saveVault(vault)
        clearSession()
        await nativeSessionClear()
        window.location.reload()
        return true
      } catch {
        return false
      }
    },
  },
})
