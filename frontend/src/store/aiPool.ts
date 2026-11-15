/*
 * store/aiPool.ts — состояние страницы sn://ai-computer.
 * Локальный статус — каждые 5 с, железо + сетевые AI Stats — каждые 10 с.
 */
import { defineStore } from 'pinia'
import {
  isTauri,
  aiPoolStatus,
  aiPoolToggle,
  aiHwSnapshot,
  aiNetStats,
  type AiHwSnapshot,
  type AiNetStats,
  type AiPoolStatus,
} from '@/lib/bridge'

let localTimer: number | null = null
let netTimer: number | null = null

export const useAiPoolStore = defineStore('aiPool', {
  state: () => ({
    enabled: false,
    status: 'disabled' as AiPoolStatus['status'],
    hw: null as AiHwSnapshot | null,
    net: null as AiNetStats | null,
    netFailed: false,
    busy: false,
  }),
  actions: {
    async refreshLocal() {
      if (!isTauri) return
      const st = await aiPoolStatus()
      this.enabled = st.enabled
      this.status = st.status
    },
    async refreshHw() {
      const hw = await aiHwSnapshot()
      if (hw) this.hw = hw
    },
    async refreshNet() {
      const n = await aiNetStats()
      if (n) {
        this.net = n
        this.netFailed = false
      } else if (!this.net) {
        this.netFailed = true
      }
    },
    async toggle() {
      if (!isTauri || this.busy) return
      this.busy = true
      try {
        const st = await aiPoolToggle(!this.enabled)
        this.enabled = st.enabled
        this.status = st.status
      } finally {
        this.busy = false
      }
    },
    startPolling() {
      this.stopPolling()
      this.refreshLocal()
      this.refreshHw()
      this.refreshNet()
      localTimer = window.setInterval(() => this.refreshLocal(), 5000)
      netTimer = window.setInterval(() => {
        this.refreshNet()
        this.refreshHw()
      }, 10000)
    },
    stopPolling() {
      if (localTimer) {
        clearInterval(localTimer)
        localTimer = null
      }
      if (netTimer) {
        clearInterval(netTimer)
        netTimer = null
      }
    },
  },
})
