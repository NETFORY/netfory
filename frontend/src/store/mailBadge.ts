// Бейдж непрочитанных писем Oxid Mail: значение приходит по IPC-каналу
// (событие "oxid-mail-event" type=unread), последнее известное — в localStorage.
import { defineStore } from 'pinia'

const KEY = 'oxid.mail.unread'

export const useMailBadgeStore = defineStore('mailBadge', {
  state: () => ({
    unread: Number(localStorage.getItem(KEY) || 0) || 0,
  }),
  actions: {
    set(n: number) {
      this.unread = Math.max(0, Math.floor(Number(n) || 0))
      localStorage.setItem(KEY, String(this.unread))
    },
  },
})
