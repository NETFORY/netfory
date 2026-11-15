import { defineStore } from 'pinia'
import { authorAvatar, authorAvatarCached, fetchWallMedia } from '@/lib/bridge'

// Кэш профильной информации других пользователей: аватары показываются
// МГНОВЕННО (localStorage → без единого IPC, ещё до ответа Rust), затем
// дисковый кэш Rust, затем фоновая догрузка свежего блоба по P2P.
const inflight = new Set<string>()
const refreshed = new Set<string>() // фоновое обновление — раз за сессию

const LS_KEY = 'sn.avatars.v1'
const MAX_ENTRIES = 30
const MAX_ITEM_BYTES = 96 * 1024 // слишком большие data-URL в LS не кладём

function loadLS(): { map: Record<string, string>; order: string[] } {
  try {
    const raw = localStorage.getItem(LS_KEY)
    if (raw) {
      const d = JSON.parse(raw)
      if (d && typeof d.map === 'object' && Array.isArray(d.order)) {
        return { map: d.map, order: d.order }
      }
    }
  } catch { /* повреждённый кэш игнорируем */ }
  return { map: {}, order: [] }
}
const ls = loadLS()

export const useUsersStore = defineStore('users', {
  state: () => ({
    /** address → data-URL аватара (undefined = ещё не спрашивали, null = нет). */
    avatars: { ...ls.map } as Record<string, string | null>,
    order: [...ls.order] as string[],
  }),
  actions: {
    persist(address: string, url: string) {
      if (!url || url.length > MAX_ITEM_BYTES) return
      this.order = [address, ...this.order.filter((a) => a !== address)].slice(0, MAX_ENTRIES)
      const map: Record<string, string> = {}
      for (const a of this.order) {
        const v = a === address ? url : this.avatars[a]
        if (v) map[a] = v
      }
      try {
        localStorage.setItem(LS_KEY, JSON.stringify({ map, order: this.order }))
      } catch {
        // квота: оставляем 10 самых свежих
        this.order = this.order.slice(0, 10)
        const small: Record<string, string> = {}
        for (const a of this.order) if (map[a]) small[a] = map[a]
        try {
          localStorage.setItem(LS_KEY, JSON.stringify({ map: small, order: this.order }))
        } catch { /* не судьба */ }
      }
    },
    /** Обеспечить аватар адреса: мгновенно из кэша + фоновое обновление. */
    ensure(address: string) {
      if (!address || inflight.has(address)) return
      // уже есть (из LS или ранее) → только одно фоновое обновление за сессию
      if (address in this.avatars && refreshed.has(address)) return
      inflight.add(address)
      refreshed.add(address)
      ;(async () => {
        try {
          // 1) мгновенно: локальный дисковый кэш Rust (без сети),
          //    если LS ещё не дал картинку
          if (!this.avatars[address]) {
            const cached = await authorAvatarCached(address)
            if (cached || !(address in this.avatars)) this.avatars[address] = cached
            if (cached) this.persist(address, cached)
          }
          // 2) фоном: актуальный cid + догрузка блоба по P2P (кэшируется на диск)
          const info = await authorAvatar(address)
          if (info?.cid) {
            const url = await fetchWallMedia(info.cid, info.node)
            if (url && url !== this.avatars[address]) {
              this.avatars[address] = url
              this.persist(address, url)
            }
          }
        } catch {
          if (!(address in this.avatars)) this.avatars[address] = null
        } finally {
          inflight.delete(address)
        }
      })()
    },
    /** Принудительно обновить (например после события смены аватара). */
    refresh(address: string) {
      delete this.avatars[address]
      refreshed.delete(address)
      this.ensure(address)
    },
  },
})
