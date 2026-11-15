import { defineStore } from 'pinia'

export type LogType = 'system' | 'crypto' | 'network' | 'success' | 'error' | 'info'

export interface LogEntry {
  text: string
  type: LogType
}

export interface CryptoReport {
  polyfilled: boolean
  integrity: boolean
  mode: string
}

interface TerminalState {
  isOpen: boolean
  logs: Record<string, LogEntry[]>
  reports: Record<string, CryptoReport>
  loaded: Record<string, boolean>
}

const MAX_LOGS = 500

/** Normalize an incoming log type (also infer from the message prefix). */
function normType(text: string, type?: string): LogType {
  const t = (type || '').toLowerCase()
  if (t === 'error' || t === 'success' || t === 'network' || t === 'system') return t as LogType
  if (/\[sth:\/\/crypto\]/.test(text)) return 'crypto'
  if (/\[sth:\/\/network\]/.test(text)) return 'network'
  if (/\[sth:\/\/system\]/.test(text)) return 'system'
  if (/error|fail|✗/i.test(text)) return 'error'
  return 'info'
}

/**
 * Per-dApp terminal log buffers + crypto self-test reports. Logs are keyed by
 * app_id (the sth:// address) so sessions never leak into each other.
 */
export const useTerminalStore = defineStore('terminal', {
  state: (): TerminalState => ({
    isOpen: false,
    logs: {},
    reports: {},
    loaded: {},
  }),

  getters: {
    logsFor: (s) => (id: string | null): LogEntry[] => (id && s.logs[id]) || [],
    reportFor: (s) => (id: string | null): CryptoReport | null => (id && s.reports[id]) || null,
    isLoaded: (s) => (id: string | null): boolean => !!(id && s.loaded[id]),
  },

  actions: {
    toggle() {
      this.isOpen = !this.isOpen
    },
    open() {
      this.isOpen = true
    },
    close() {
      this.isOpen = false
    },
    addLog(p: { app_id: string; text: string; type?: string }) {
      if (!p?.app_id || !p.text) return
      const arr = (this.logs[p.app_id] ||= [])
      arr.push({ text: p.text, type: normType(p.text, p.type) })
      if (arr.length > MAX_LOGS) arr.splice(0, arr.length - MAX_LOGS)
    },
    clear(id: string | null) {
      if (id) this.logs[id] = []
    },
    setReport(p: { app_id: string; polyfilled?: boolean; integrity?: boolean; mode?: string }) {
      if (!p?.app_id) return
      this.reports[p.app_id] = {
        polyfilled: !!p.polyfilled,
        integrity: !!p.integrity,
        mode: p.mode || '',
      }
    },
    setLoaded(id: string) {
      if (id) this.loaded[id] = true
    },
    reset(id: string) {
      delete this.logs[id]
      delete this.reports[id]
      delete this.loaded[id]
    },
  },
})
