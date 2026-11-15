import { defineStore } from 'pinia'
import { api } from '../lib/backend'
import { useAccountStore } from './account'

export const useMailStore = defineStore('mail', {
  state: () => ({
    emails: [],
    selectedId: null,
    decrypted: {}, // id -> DecryptedView (NTFRY)
    verifyMap: {}, // id -> VerifyResult (X-Ntfry-Sig)
    activeFolder: 'inbox',
    activeLabel: null,
    search: '',
    loading: false,
    syncing: false,
    loadingOlder: false,
    error: '',
    _hydrated: false,
    _seenIds: null,
    p2pFlash: 0, // timestamp последнего входящего P2P-письма (для pulse-анимации бейджа)
  }),
  getters: {
    accountEmails(state) {
      const acc = useAccountStore()
      const activeId = acc.activeAccountId
      if (!activeId) return state.emails
      return state.emails.filter((e) => !e.accountId || e.accountId === activeId)
    },
    foldersWithCounts() {
      const folders = ['inbox', 'sent', 'p2p', 'drafts', 'trash']
      const scope = this.accountEmails
      return folders.map((f) => ({
        id: f,
        count: scope.filter((e) => e.folder === f && !e.read).length,
        total: scope.filter((e) => e.folder === f).length,
      }))
    },
    labelsWithCounts() {
      const scope = this.accountEmails.filter((e) => e.folder !== 'trash')
      const counts = {}
      scope.forEach((e) => (e.labels || []).forEach((l) => {
        counts[l] = (counts[l] || 0) + (e.read ? 0 : 1)
      }))
      return counts
    },
    visibleEmails(state) {
      const q = state.search.trim().toLowerCase()
      let list = this.accountEmails
      if (state.activeLabel) {
        list = list
          .filter((e) => e.folder !== 'trash')
          .filter((e) => (e.labels || []).includes(state.activeLabel))
      } else {
        list = list.filter((e) => e.folder === state.activeFolder)
      }
      return list
        .filter((e) => {
          if (!q) return true
          return (
            (e.subject || '').toLowerCase().includes(q) ||
            (e.snippet || '').toLowerCase().includes(q) ||
            (e.from?.name || '').toLowerCase().includes(q) ||
            (e.from?.email || '').toLowerCase().includes(q)
          )
        })
        .sort((a, b) => new Date(b.date) - new Date(a.date))
    },
    selected(state) {
      // ВАЖНО: ищем только среди писем активного аккаунта — id вида
      // "inbox_<uid>" коллизируют между аккаунтами, глобальный поиск
      // открывал чужое письмо
      return this.accountEmails.find((e) => e.id === state.selectedId) || null
    },
    // Составной ключ кэшей расшифровки/подписи: id вида "inbox_<uid>"
    // коллизирует между аккаунтами — кэши храним с префиксом аккаунта
    cacheKey(state) {
      return (id) => {
        const e =
          this.accountEmails.find((x) => x.id === id) ||
          state.emails.find((x) => x.id === id)
        return e ? `${e.accountId || 'local'}|${id}` : id
      }
    },
    isDecrypted() {
      return (id) => !!this.decrypted[this.cacheKey(id)]
    },
    decryptedView() {
      return (id) => this.decrypted[this.cacheKey(id)] || null
    },
    verifyResult() {
      return (id) => this.verifyMap[this.cacheKey(id)] || null
    },
  },
  actions: {
    hydrate() {
      this._hydrated = true
    },
    // Загрузка кэша писем всех аккаунтов из зашифрованного хранилища
    async loadAll() {
      const acc = useAccountStore()
      if (!acc.accounts.length) {
        this.emails = []
        return
      }
      this.loading = true
      try {
        const lists = await Promise.all(
          acc.accounts.map((a) => api('GET', `/accounts/${a.id}/messages`).catch(() => []))
        )
        this.emails = lists.flat()
        this._seenIds = new Set(
          this.emails.filter((e) => e.folder === 'inbox').map((e) => `${e.accountId}|${e.id}`)
        )
      } finally {
        this.loading = false
      }
    },
    // Реальная IMAP-синхронизация активного аккаунта
    async refresh() {
      const acc = useAccountStore()
      const active = acc.currentAccount
      if (!active || this.syncing) return
      this.syncing = true
      try {
        await api('POST', `/accounts/${active.id}/sync`)
        const list = await api('GET', `/accounts/${active.id}/messages`)
        this._mergeAccountList(active.id, list)
        this._notifyFresh()
        this.error = ''
      } catch (e) {
        this.error = e.message
      } finally {
        this.syncing = false
      }
    },
    // Список с сервера приходит без тел (summary) — сохраняем уже загруженные body/bodyHtml
    _mergeAccountList(accountId, list) {
      const loaded = new Map(
        this.emails
          .filter((e) => e.accountId === accountId && (e.body != null || e.bodyHtml != null))
          .map((e) => [e.id, e])
      )
      const merged = list.map((m) => {
        const old = loaded.get(m.id)
        return old ? { ...m, body: old.body, bodyHtml: old.bodyHtml } : m
      })
      this.emails = [...this.emails.filter((e) => e.accountId !== accountId), ...merged]
    },
    // Системное уведомление (Tauri) о новых непрочитанных письмах во Входящих
    _notifyFresh() {
      const inbox = this.emails.filter((e) => e.folder === 'inbox')
      if (this._seenIds) {
        const fresh = inbox.filter((e) => !this._seenIds.has(`${e.accountId}|${e.id}`) && !e.read)
        if (fresh.length) {
          const core = typeof window !== 'undefined' ? window.__TAURI__?.core : null
          const f = fresh[0]
          core?.invoke?.('notify_new_mail', {
            count: fresh.length,
            from: f.from?.name || f.from?.email || '',
            subject: f.subject || '',
          }).catch(() => {})
        }
      }
      this._seenIds = new Set(inbox.map((e) => `${e.accountId}|${e.id}`))
    },
    async select(id) {
      this.selectedId = id
      const email = this.accountEmails.find((e) => e.id === id)
      if (!email) return
      if (email.body === undefined || email.body === null) {
        try {
          const full = await api('GET', `/accounts/${email.accountId}/messages/${id}`)
          Object.assign(email, full)
        } catch (e) {
          // тело недоступно — покажем сниппет
        }
      }
      if (!email.read) {
        email.read = true
        api('PATCH', `/accounts/${email.accountId}/messages/${id}`, { read: true }).catch(() => {})
      }
    },
    clearSelection() {
      this.selectedId = null
    },
    setFolder(folder) {
      this.activeFolder = folder
      this.activeLabel = null
      this.selectedId = null
    },
    setLabel(name) {
      this.activeLabel = this.activeLabel === name ? null : name
      this.selectedId = null
    },
    setSearch(q) {
      this.search = q
    },
    // Реальная расшифровка NTFRY-конверта в ядре (кэшируется в шифрованном store)
    async decrypt(id) {
      const e = this.accountEmails.find((x) => x.id === id)
      if (!e) throw new Error('message not found')
      const view = await api('POST', `/accounts/${e.accountId}/messages/${id}/decrypt`)
      this.decrypted = { ...this.decrypted, [`${e.accountId || 'local'}|${id}`]: view }
      return view
    },
    relock(id) {
      const key = this.cacheKey(id)
      const { [key]: _, ...rest } = this.decrypted
      this.decrypted = rest
    },
    // Проверка Anti-Spoofing подписи обычного письма (X-Ntfry-Sig)
    async verifyPlain(id) {
      const e = this.accountEmails.find((x) => x.id === id)
      if (!e) return null
      try {
        const res = await api('GET', `/accounts/${e.accountId}/messages/${id}/verify`)
        this.verifyMap = { ...this.verifyMap, [`${e.accountId || 'local'}|${id}`]: res }
        return res
      } catch {
        return null
      }
    },
    toggleStar(id) {
      const e = this.accountEmails.find((x) => x.id === id)
      if (!e) return
      e.starred = !e.starred
      api('PATCH', `/accounts/${e.accountId}/messages/${id}`, { starred: e.starred }).catch(() => {})
    },
    trash(id) {
      const e = this.accountEmails.find((x) => x.id === id)
      if (!e) return
      if (e.folder === 'trash') {
        this.emails = this.emails.filter((x) => x !== e)
        api('DELETE', `/accounts/${e.accountId}/messages/${id}`).catch(() => {})
      } else {
        e.folder = 'trash'
        api('PATCH', `/accounts/${e.accountId}/messages/${id}`, { folder: 'trash' }).catch(() => {})
      }
      this.selectedId = null
    },
    // Реальная SMTP-отправка через бридж; encrypt=true — NTFRY-шифрование SmartNet.
    // progressId (опц.) — идентификатор задачи P2P-раздачи, по нему UI поллит
    // прогресс отдачи вложений (см. /api/p2p/progress/:pid).
    async sendEmail({ accountId, to, subject, body, attachments = [], encrypt = false, progressId = null }) {
      const req = { to, subject, body, attachments, encrypt }
      if (progressId) req.progressId = progressId
      const msg = await api('POST', `/accounts/${accountId}/send`, req)
      this.emails.unshift(msg)
      return msg
    },
    // Догрузка старых писем текущей папки (пагинация >50)
    async loadOlder() {
      const acc = useAccountStore()
      const active = acc.currentAccount
      if (!active || this.loadingOlder) return 0
      const folder = this.activeFolder
      if (folder !== 'inbox' && folder !== 'sent') return 0
      const uids = this.emails
        .filter((e) => e.accountId === active.id && e.id.startsWith(`${folder}_`))
        .map((e) => parseInt(e.id.split('_')[1], 10))
        .filter((n) => n > 0)
      if (!uids.length) return 0
      this.loadingOlder = true
      try {
        const res = await api('POST', `/accounts/${active.id}/older`, {
          folder,
          beforeUid: Math.min(...uids),
          count: 50,
        })
        if (res.fetched > 0) {
          const list = await api('GET', `/accounts/${active.id}/messages`)
          this._mergeAccountList(active.id, list)
        }
        return res.fetched
      } catch (e) {
        this.error = e.message
        return 0
      } finally {
        this.loadingOlder = false
      }
    },
    // Опрос P2P-приёмника: если ядро приняло входящее письмо — подтянуть его
    // и триггернуть pulse-бейдж + системное уведомление (работает в реальном времени
    // независимо от активного аккаунта).
    async checkP2P() {
      const acc = useAccountStore()
      if (!acc.sessionOpen) return
      try {
        const res = await api('GET', '/accounts/p2p/updates').catch(() => null)
        if (!res || !res.dirty) return
        const list = await api('GET', '/accounts/p2p/messages')
        const prevIds = new Set(
          this.emails.filter((e) => e.accountId === 'p2p').map((e) => e.id)
        )
        this._mergeAccountList('p2p', list)
        const fresh = list.filter((m) => !prevIds.has(m.id) && !m.read)
        if (fresh.length) {
          this.p2pFlash = Date.now()
          const core = typeof window !== 'undefined' ? window.__TAURI__?.core : null
          const f = fresh[0]
          core?.invoke?.('notify_new_mail', {
            count: fresh.length,
            from: f.from?.name || f.from?.email || 'SmartNet P2P',
            subject: f.subject || 'Encrypted P2P message',
          }).catch(() => {})
        }
      } catch {
        // тихо: p2p-канал необязательный
      }
    },
    // Быстрый опрос IDLE-флага: sync только когда сервер сообщил о новом письме
    async checkUpdates() {
      const acc = useAccountStore()
      const active = acc.currentAccount
      if (!active || this.syncing) return
      try {
        const res = await api('GET', `/accounts/${active.id}/updates`)
        if (res.dirty) this.refresh()
      } catch (e) {
        // тихо: updates — необязательный канал
      }
    },
    // Скачивание вложения (base64 → Blob → download)
    async downloadAttachment(email, idx) {
      const att = await api('GET', `/accounts/${email.accountId}/messages/${email.id}/attachments/${idx}`)
      const bytes = Uint8Array.from(atob(att.dataBase64), (c) => c.charCodeAt(0))
      const blob = new Blob([bytes], { type: att.mime || 'application/octet-stream' })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = att.name || 'attachment'
      document.body.appendChild(a)
      a.click()
      a.remove()
      URL.revokeObjectURL(url)
    },
  },
})
