/**
 * SmartNet // store/net.ts
 * Глобальный флаг фактического подключения к сети. Источники:
 *  - события браузера online/offline (мгновенно при обрыве Wi-Fi/кабеля);
 *  - Rust TCP-probe `net-status` (единственный надёжный сигнал на Windows/
 *    WebKitGTK после сна — navigator.onLine там часто остаётся true).
 * Все периодические сетевые поллеры обязаны проверять этот флаг и
 * пропускать тик в offline: иначе десятки IPC-вызовов виснут в таймаутах
 * и давят на WebView2 IPC — это морозит весь UI клиента.
 */
import { defineStore } from 'pinia'
import { appListen, isTauri, netOnline } from '@/lib/bridge'

export const useNetStore = defineStore('net', {
  state: () => ({
    online: typeof navigator !== 'undefined' ? navigator.onLine : true,
    inited: false,
  }),
  actions: {
    async init() {
      if (this.inited) return
      this.inited = true
      window.addEventListener('online', () => (this.online = true))
      window.addEventListener('offline', () => (this.online = false))
      if (isTauri) {
        try {
          this.online = await netOnline()
        } catch {
          /* probe ещё не запустился — оставляем navigator.onLine */
        }
        await appListen<{ online: boolean }>('net-status', (p) => {
          this.online = !!p.online
        })
      }
    },
  },
})
