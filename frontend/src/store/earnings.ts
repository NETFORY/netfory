import { defineStore } from 'pinia'
import { networkEarnings, type EarningsSummary, type DappEarning } from '@/lib/bridge'
import { useAuthStore } from '@/store/auth'

const TTL_MS = 60_000 // не дёргаем трекеры чаще раза в минуту

export const useEarningsStore = defineStore('earnings', {
  state: () => ({
    summary: null as EarningsSummary | null,
    loading: false,
    loadedAt: 0,
    _inflight: null as Promise<void> | null,
  }),
  getters: {
    totalEstSth(state): number {
      return state.summary?.totalEstSth || 0
    },
    activeApps(state): DappEarning[] {
      return state.summary?.activeApps || []
    },
    byApp: (state) => (id: string): DappEarning | null =>
      state.summary?.activeApps.find((a) => a.id === id) || null,
  },
  actions: {
    async refresh(force = false) {
      const auth = useAuthStore()
      if (!auth.address) return
      if (!force && Date.now() - this.loadedAt < TTL_MS) return
      // дедуп: несколько виджетов монтируются одновременно — один запрос на всех
      if (this._inflight) return this._inflight
      this.loading = true
      this._inflight = (async () => {
        try {
          this.summary = await networkEarnings(auth.address)
          this.loadedAt = Date.now()
        } catch {
          // трекеры недоступны — оставляем прошлый снимок
        } finally {
          this.loading = false
          this._inflight = null
        }
      })()
      return this._inflight
    },
  },
})
