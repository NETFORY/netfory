import { defineStore } from 'pinia'

/** Закладка https://-сайта (v1.135.185). Хранится в localStorage. */
export interface Bookmark {
  url: string
  title: string
  addedAt: number
}

const LS_KEY = 'sth_web_bookmarks'

function load(): Bookmark[] {
  try {
    const arr = JSON.parse(localStorage.getItem(LS_KEY) || '[]')
    return Array.isArray(arr)
      ? arr.filter((b) => b && typeof b.url === 'string' && /^https:\/\//i.test(b.url))
      : []
  } catch {
    return []
  }
}

/** Нормализация: только https, без hash, host в нижнем регистре. */
export function normalizeBookmarkUrl(raw: string): string | null {
  try {
    const u = new URL(raw.trim())
    if (u.protocol !== 'https:' || !u.hostname) return null
    u.hash = ''
    return u.toString()
  } catch {
    return null
  }
}

export function bookmarkHost(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, '')
  } catch {
    return url
  }
}

export const useBookmarksStore = defineStore('bookmarks', {
  state: () => ({ items: load() as Bookmark[] }),
  getters: {
    has: (s) => (url: string) => {
      const n = normalizeBookmarkUrl(url)
      return !!n && s.items.some((b) => b.url === n)
    },
  },
  actions: {
    persist() {
      try { localStorage.setItem(LS_KEY, JSON.stringify(this.items)) } catch {}
    },
    add(url: string, title?: string): boolean {
      const n = normalizeBookmarkUrl(url)
      if (!n || this.items.some((b) => b.url === n)) return false
      this.items.unshift({ url: n, title: (title || '').trim() || bookmarkHost(n), addedAt: Date.now() })
      this.persist()
      return true
    },
    remove(url: string) {
      const n = normalizeBookmarkUrl(url) || url
      this.items = this.items.filter((b) => b.url !== n)
      this.persist()
    },
    toggle(url: string, title?: string): boolean {
      if (this.has(url)) { this.remove(url); return false }
      return this.add(url, title)
    },
  },
})
