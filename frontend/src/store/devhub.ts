import { defineStore } from 'pinia'
import { devhubPrOpenCount } from '@/lib/bridge'

/** Счётчики DevHub для сайдбара (бейдж открытых входящих Pull Request). */
export const useDevhubStore = defineStore('devhub', {
  state: () => ({
    openPrs: 0,
  }),
  actions: {
    async refreshPrCount() {
      try {
        this.openPrs = await devhubPrOpenCount()
      } catch {
        this.openPrs = 0
      }
    },
  },
})
