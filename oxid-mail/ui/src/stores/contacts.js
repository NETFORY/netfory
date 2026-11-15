import { defineStore } from 'pinia'
import { api } from '../lib/backend'

export const useContactsStore = defineStore('contacts', {
  state: () => ({
    list: [],
    loading: false,
    error: '',
  }),
  actions: {
    async load() {
      this.loading = true
      try {
        this.list = await api('GET', '/contacts')
        this.error = ''
      } catch (e) {
        this.error = e.message
      } finally {
        this.loading = false
      }
    },
    // Предпросмотр без сохранения: email / sn://add-email?... / SADDR:email / SADDR@sth
    async parse(input) {
      return await api('POST', '/contacts/parse', { input })
    },
    async add(input, name = '', note = '') {
      const c = await api('POST', '/contacts', { input, name, note })
      await this.load()
      return c
    },
    async remove(email) {
      await api('DELETE', `/contacts/${encodeURIComponent(email)}`)
      await this.load()
    },
    // Моя карточка (plain / uri / p2p) для «Поделиться контактом»
    async myCard(accountId) {
      return await api('GET', `/accounts/${accountId}/contact-card`)
    },
  },
})
