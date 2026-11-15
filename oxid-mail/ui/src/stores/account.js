import { defineStore } from 'pinia'
import { api } from '../lib/backend'

const STORAGE_KEY = 'oxid.account'

function mapAccount(a) {
  return {
    id: a.id,
    email: a.email,
    provider: a.provider,
    host: a.imapHost,
    port: String(a.imapPort),
    smtpHost: a.smtpHost,
    smtpPort: String(a.smtpPort),
    authKind: a.authKind || 'password',
  }
}

function toServerAccount(p) {
  // Google выдаёт app password с пробелами («abcd efgh …») — IMAP ждёт слитно
  const rawPass = p.password || ''
  return {
    email: p.email,
    password: p.provider === 'gmail' ? rawPass.replace(/\s+/g, '') : rawPass,
    provider: p.provider,
    imapHost: p.host,
    imapPort: parseInt(p.port, 10) || 993,
    smtpHost: p.smtpHost,
    smtpPort: parseInt(p.smtpPort, 10) || 465,
  }
}

export const useAccountStore = defineStore('account', {
  state: () => ({
    onboarded: false,
    seedWords: [],
    sthAddress: '',
    publicKey: '',
    accounts: [], // { id, email, provider, host, port, smtpHost, smtpPort }
    activeAccountId: null,
    isEncryptedEnabled: true,
    sealedSeed: '', // seed, зашифрованный PIN-кодом (хранится на диске вместо plaintext)
    sessionOpen: false,
    ephemeral: false, // запуск из SmartNet: seed только в RAM, на диск не пишем
    _hydrated: false,
  }),
  getters: {
    currentAccount(state) {
      return state.accounts.find((a) => a.id === state.activeAccountId) || state.accounts[0] || null
    },
    // Требуется ввод PIN: seed запечатан и ещё не расшифрован в память
    locked(state) {
      return !!state.sealedSeed && state.seedWords.length === 0
    },
  },
  actions: {
    hydrate() {
      if (this._hydrated) return
      try {
        const raw = localStorage.getItem(STORAGE_KEY)
        if (raw) {
          const data = JSON.parse(raw)
          delete data.sessionOpen
          Object.assign(this, data)
        }
      } catch (e) {
        // ignore
      }
      this._hydrated = true
    },
    persist() {
      if (this.ephemeral) return
      const { _hydrated, sessionOpen, ephemeral, ...rest } = this.$state
      // при установленном PIN seed на диск не пишем — только запечатанный блоб
      if (rest.sealedSeed) rest.seedWords = []
      localStorage.setItem(STORAGE_KEY, JSON.stringify(rest))
    },
    // Установить PIN: запечатать seed в ядре и перестать хранить plaintext
    async setPin(pin) {
      if (!this.seedWords.length) throw new Error('No seed to protect')
      const res = await api('POST', '/pin/seal', { mnemonic: this.seedWords.join(' '), pin })
      this.sealedSeed = res.blob
      this.persist()
    },
    // Разблокировка PIN-кодом: расшифровать seed в память и открыть сессию
    async unlock(pin) {
      const res = await api('POST', '/pin/open', { blob: this.sealedSeed, pin })
      this.seedWords = res.mnemonic.split(' ')
      await this.openSession()
    },
    // Настоящий BIP-39 (2048 слов + checksum) генерируется в Rust-ядре
    async generateSeed() {
      const id = await api('POST', '/identity/generate')
      this.seedWords = id.mnemonic.split(' ')
      this.sthAddress = id.address
      this.publicKey = id.publicKey
      this.persist()
      return this.seedWords
    },
    // Валидация введённой фразы + деривация STH-адреса (secp256k1, как в SmartHoldem)
    async importSeed(words) {
      const id = await api('POST', '/identity/import', { mnemonic: words.join(' ') })
      this.seedWords = id.mnemonic.split(' ')
      this.sthAddress = id.address
      this.publicKey = id.publicKey
      this.persist()
      return id
    },
    // Запуск из SmartNet: seed пришёл по IPC, живёт только в RAM (PIN не нужен).
    // Фраза может быть произвольной (не BIP-39) — валидацию пропускаем.
    async adoptHandoff(mnemonic) {
      if (this.sessionOpen) {
        await api('POST', '/session/close').catch(() => {})
        this.sessionOpen = false
      }
      const id = await api('POST', '/identity/import-secret', { secret: mnemonic })
      this.ephemeral = true
      this.seedWords = id.mnemonic.split(' ')
      this.sthAddress = id.address
      this.publicKey = id.publicKey
      this.sealedSeed = ''
      this.onboarded = true
      await this.openSession()
    },
    // Открывает зашифрованное хранилище по seed и подтягивает аккаунты
    async openSession() {
      if (this.sessionOpen) return
      if (!this.seedWords.length) throw new Error('No seed on this device — complete onboarding first')
      const res = await api('POST', '/session/open', { mnemonic: this.seedWords.join(' ') })
      this.sthAddress = res.address
      this.publicKey = res.publicKey
      this.accounts = (res.accounts || []).map(mapAccount)
      if (!this.activeAccountId || !this.accounts.find((a) => a.id === this.activeAccountId)) {
        this.activeAccountId = this.accounts[0]?.id || null
      }
      this.sessionOpen = true
      this.persist()
    },
    // Проверяет IMAP-логин на бридже, сохраняет аккаунт в шифрованное хранилище
    async addAccount(payload) {
      const created = await api('POST', '/accounts', toServerAccount(payload))
      const acc = mapAccount(created)
      this.accounts.push(acc)
      if (!this.activeAccountId) this.activeAccountId = acc.id
      this.persist()
      return acc
    },
    // Gmail OAuth «1 клик»: ядро откроет системный браузер и дождётся подтверждения
    async addOAuthGmail() {
      const created = await api('POST', '/oauth/gmail')
      const acc = mapAccount(created)
      const idx = this.accounts.findIndex((a) => a.id === acc.id)
      if (idx >= 0) this.accounts[idx] = acc
      else this.accounts.push(acc)
      if (!this.activeAccountId) this.activeAccountId = acc.id
      this.persist()
      return acc
    },
    async updateAccount(id, payload) {
      const updated = await api('PUT', `/accounts/${id}`, toServerAccount(payload))
      const acc = this.accounts.find((a) => a.id === id)
      if (acc) Object.assign(acc, mapAccount(updated))
      this.persist()
      return acc
    },
    setActiveAccount(id) {
      this.activeAccountId = id
      this.persist()
    },
    async removeAccount(id) {
      await api('DELETE', `/accounts/${id}`)
      this.accounts = this.accounts.filter((a) => a.id !== id)
      if (this.activeAccountId === id) {
        this.activeAccountId = this.accounts[0]?.id || null
      }
      this.persist()
    },
    completeOnboarding() {
      this.onboarded = true
      this.persist()
    },
    toggleEncryption() {
      this.isEncryptedEnabled = !this.isEncryptedEnabled
      this.persist()
    },
    reset() {
      api('POST', '/session/close').catch(() => {})
      this.$reset()
      localStorage.removeItem(STORAGE_KEY)
    },
  },
})
