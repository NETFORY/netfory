import { defineStore } from 'pinia'
import {
  isTauri,
  verifyApp,
  appInstallSource,
  privateDappMount,
  privateDappUnmount,
  type InstallSource,
} from '@/lib/bridge'

export interface Tab {
  id: string
  title: string
  verified: boolean | null // null = checking, true = ✓ verified, false = ⚠ unverified
  /** v1.63.20: если `verified === false` — конкретная причина от `verify_app`
   *  (`missing-file:assets/logo.png`, `file-hash-mismatch:index.html`, …).
   *  Показывается в тултипе значка ⚠ и логируется в консоль браузера. */
  verifyReason?: string
  kind: 'app' | 'profile' | 'web' | 'start' // 'profile' — страница пользователя (u://); 'web' — обычный https://-сайт; 'start' — стартовая страница новой вкладки (поиск SmartNet)
  identity?: string // for profile tabs: the resolved STH address
  /** v1.135.183: web-вкладка — текущий https://-URL сайта (обновляется событием site-nav). */
  url?: string
  /** web-вкладка: страница в процессе загрузки (индикатор в табе). */
  loading?: boolean
  /** v1.92.0: откуда установлен dApp — ☁ cloud seeder или ⇄ прямой P2P.
   *  null = локально созданный / источник неизвестен (бейдж не рисуем). */
  source?: InstallSource | null
  /** v1.63.6: deep-link path (sub-route внутри dApp, напр. "trading/PAIR").
   *  Устанавливается при open(id, title, path); AppLayout передаёт его в
   *  openEmbeddedWebview на активации вкладки, после чего сбрасывается,
   *  чтобы дальнейшие переключения не «прыгали» обратно на deep-link. */
  pendingPath?: string
}

let webSeq = 0

interface TabsState {
  tabs: Tab[]
  activeId: string | null
}

/**
 * Browser-style tab manager. Each tab hosts an isolated dApp surface:
 *  - Tauri : an <iframe> loading sth://<id>/ served by the custom protocol
 *            (real app, isolated browsing context, no local HTTP port).
 *  - Web   : a placeholder surface (Web Fallback).
 * activeId === null => the internal client views (router) are shown.
 */
export const useTabsStore = defineStore('tabs', {
  state: (): TabsState => ({
    tabs: [],
    activeId: null,
  }),

  getters: {
    activeTab: (s) => s.tabs.find((t) => t.id === s.activeId) || null,
  },

  actions: {
    open(id: string, title: string, path?: string) {
      const existing = this.tabs.find((t) => t.id === id)
      if (!existing) {
        this.tabs.push({ id, title: title || id, verified: null, kind: 'app', pendingPath: path || undefined })
      } else if (path) {
        // Вкладка уже есть — deep-link просит открыть конкретную страницу.
        // Ставим pendingPath, AppLayout навигирует существующий webview.
        existing.pendingPath = path
      }
      this.activeId = id
      // прогрев приватного dApp: расшифровка контейнера в RAM (no-op для публичных)
      void privateDappMount(id).catch(() => {})
      this.verify(id)
      this.loadSource(id)
    },

    /** Подтянуть источник установки (☁ seeder / ⇄ p2p) для бейджа вкладки. */
    async loadSource(id: string) {
      try {
        const src = await appInstallSource(id)
        const tab = this.tabs.find((t) => t.id === id)
        if (tab) tab.source = src
      } catch {
        /* нет источника — бейдж просто не рисуем */
      }
    },

    /** Забрать и очистить pendingPath у активной вкладки (одноразовый деплой). */
    consumePendingPath(id: string): string | undefined {
      const tab = this.tabs.find((t) => t.id === id)
      if (!tab) return undefined
      const p = tab.pendingPath
      tab.pendingPath = undefined
      return p
    },

    /** Open a user/developer PUBLIC profile (u://) as its own tab (visitor mode). */
    openProfile(identity: string, title: string) {
      const id = `u://${identity}`
      if (!this.tabs.some((t) => t.id === id)) {
        this.tabs.push({ id, title: title || id, verified: true, kind: 'profile', identity })
      } else {
        const tab = this.tabs.find((t) => t.id === id)
        if (tab) tab.identity = identity
      }
      this.activeId = id
    },

    /** v1.135.224: новая вкладка — стартовая страница с поиском по SmartNet (dApps,
     *  u://-имена, раздаваемые сайты, закладки) и запасным Google. */
    openStart(title: string): string {
      webSeq += 1
      const id = `start:${Date.now().toString(36)}${webSeq}`
      this.tabs.push({ id, title, verified: true, kind: 'start' })
      this.activeId = id
      return id
    },

    /** v1.135.183: открыть обычный https://-сайт отдельной web-вкладкой.
     *  Каждый вызов — новая вкладка (как Ctrl+T в браузере); навигация внутри
     *  существующей делается через siteNavigate из адресной строки. */
    openWeb(url: string): string {
      webSeq += 1
      const id = `web:${Date.now().toString(36)}${webSeq}`
      let title = url
      try { title = new URL(url).hostname.replace(/^www\./, '') } catch {}
      // в браузере события site-nav не придут — индикатор загрузки не зажигаем
      this.tabs.push({ id, title, verified: true, kind: 'web', url, loading: isTauri })
      this.activeId = id
      return id
    },

    /** Обновить URL/статус загрузки web-вкладки (событие site-nav из Rust). */
    setWebNav(id: string, url: string, loading: boolean) {
      const tab = this.tabs.find((t) => t.id === id && t.kind === 'web')
      if (!tab) return
      tab.url = url
      tab.loading = loading
    },

    /** Текущий URL web-вкладки без смены статуса загрузки (SPA pushState / фрагменты — событие site-url). */
    setWebUrl(id: string, url: string) {
      const tab = this.tabs.find((t) => t.id === id && t.kind === 'web')
      if (tab && url) tab.url = url
    },

    /** Заголовок документа web-вкладки (событие site-title из Rust). */
    setWebTitle(id: string, title: string) {
      const tab = this.tabs.find((t) => t.id === id && t.kind === 'web')
      if (tab && title.trim()) tab.title = title.trim()
    },

    /** Content-addressed trust check; flips the tab badge to ✓ / ⚠. */
    async verify(id: string) {
      try {
        const r = await verifyApp(id)
        const tab = this.tabs.find((t) => t.id === id)
        if (tab) {
          tab.verified = r.verified
          tab.verifyReason = r.verified ? undefined : (r.reason || 'unknown')
        }
        // v1.63.20: bad manifest — свистим в консоль браузера (виден в DevTools
        // и в SystemConsole scope="dapp" пишет Rust-сторона). Даёт разработчику
        // dApp точный путь ломающего файла (напр. `missing-file:assets/logo.png`).
        if (!r.verified) {
          // eslint-disable-next-line no-console
          console.warn(`[SmartNet verify] ${id} — ${r.reason}`)
        }
      } catch (e) {
        const tab = this.tabs.find((t) => t.id === id)
        if (tab) {
          tab.verified = false
          tab.verifyReason = 'error'
        }
        // eslint-disable-next-line no-console
        console.warn(`[SmartNet verify] ${id} — error`, e)
      }
    },

    setActive(id: string) {
      this.activeId = id
    },

    goHome() {
      this.activeId = null
    },

    close(id: string) {
      const idx = this.tabs.findIndex((t) => t.id === id)
      if (idx === -1) return
      // приватный dApp: выгрузить расшифрованный контейнер из RAM
      if (this.tabs[idx].kind === 'app') void privateDappUnmount(id).catch(() => {})
      this.tabs.splice(idx, 1)
      if (this.activeId === id) {
        const next = this.tabs[idx] || this.tabs[idx - 1] || null
        this.activeId = next ? next.id : null
      }
    },

    closeAll() {
      for (const t of this.tabs) {
        if (t.kind === 'app') void privateDappUnmount(t.id).catch(() => {})
      }
      this.tabs = []
      this.activeId = null
    },
  },
})
