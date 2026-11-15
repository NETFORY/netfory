import { defineStore } from 'pinia'
import {
  isTauri,
  appListen,
  getSeedingConfig,
  toggleBackgroundSeeding,
  getSeedingHistory,
  diskSpace,
  type DailySnapshot,
} from '@/lib/bridge'

export interface SeedingMetrics {
  status: string // ACTIVE | STOPPED
  seededBytes: number
  seededGb: number
  limitGb: number
  activeApps: number
  uptime24h: number // percent
  kUp: number
  estDailyEarnings: number
  maxDailyEarnings: number
  densityPerGb: number
}

const EMPTY: SeedingMetrics = {
  status: 'STOPPED',
  seededBytes: 0,
  seededGb: 0,
  limitGb: 10,
  activeApps: 0,
  uptime24h: 0,
  kUp: 1.0,
  estDailyEarnings: 0,
  maxDailyEarnings: 0,
  densityPerGb: 0,
}

export const useSeedingStore = defineStore('seeding', {
  state: () => ({
    isSeedingEnabled: false,
    storageLimitGb: 10,
    metrics: { ...EMPTY } as SeedingMetrics,
    history: [] as DailySnapshot[],
    busy: false,
    listening: false,
    received: false, // first metrics broadcast arrived (estimate ready)
    available: isTauri, // native-only feature
    diskTotalGb: 0, // physical disk capacity (native, 0 on web)
    diskFreeGb: 0,
  }),
  getters: {
    // Slider ceiling = real free space on the data-dir disk (NAS/RAID up to TBs);
    // can't allocate more than what's actually available. Falls back to 100 on web.
    maxLimitGb(state): number {
      return state.diskFreeGb > 0 ? Math.max(1, Math.floor(state.diskFreeGb)) : 100
    },
  },
  actions: {
    async init() {
      const cfg = await getSeedingConfig()
      this.isSeedingEnabled = cfg.enabled
      this.storageLimitGb = cfg.limitGb
      this.metrics.limitGb = cfg.limitGb
      this.metrics.status = cfg.enabled ? 'ACTIVE' : 'STOPPED'
      if (isTauri) {
        const d = await diskSpace()
        this.diskTotalGb = d.totalGb
        this.diskFreeGb = d.freeGb
      }
      await this.fetchHistory()
      if (!this.listening && isTauri) {
        this.listening = true
        await appListen<SeedingMetrics>('seeding-metrics-broadcast', (m) => {
          this.metrics = { ...this.metrics, ...m }
          this.received = true
          // refresh today's bar live as new samples fold in
          void this.fetchHistory()
        })
      }
    },
    async fetchHistory() {
      if (!isTauri) return
      this.history = await getSeedingHistory()
    },
    async toggleSeeding(enabled: boolean, limit?: number) {
      this.busy = true
      try {
        const lg = Math.min(this.maxLimitGb, Math.max(1, limit ?? this.storageLimitGb))
        await toggleBackgroundSeeding(enabled, lg)
        this.isSeedingEnabled = enabled
        this.storageLimitGb = lg
        this.metrics.limitGb = lg
        if (enabled) this.received = false // re-enter "forecasting" until first calc
        if (!enabled) this.metrics = { ...EMPTY, limitGb: lg }
      } finally {
        this.busy = false
      }
    },
    async setLimit(limit: number) {
      const lg = Math.min(this.maxLimitGb, Math.max(1, limit))
      this.storageLimitGb = lg
      if (this.isSeedingEnabled) await this.toggleSeeding(true, lg)
    },
  },
})
