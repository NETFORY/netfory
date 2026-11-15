/* Внешние http(s)-ссылки из Vue-страниц / dApp / чата.
   v1.135.225: белый список (ALLOW_HOST) и модалка-предупреждение ClearNet
   убраны — любой внешний сайт открывается молча во web-вкладке NETFORY.
   Системный браузер — только по явному выбору в контекстном меню ссылки. */

export const OPEN_LINK_EVENT = 'netfory:open-link'

/** Открыть внешний URL во web-вкладке NETFORY (слушатель — AppLayout). */
export function openInNetfory(url: string) {
  window.dispatchEvent(new CustomEvent<string>(OPEN_LINK_EVENT, { detail: url }))
}

/** Где открывать сторонние сайты (Настройки → Интерфейс → Browser). Общий localStorage
 *  у всех окон приложения — окно мессенджера читает тот же выбор. */
export type LinkTarget = 'netfory' | 'system'
/** Метка для open_in_main: http(s) из другого окна открыть НОВОЙ web-вкладкой (по настройке Browser). */
export const NEWTAB_PREFIX = 'newtab:'
export const LINK_TARGET_KEY = 'sth_link_target'
export function linkTarget(): LinkTarget {
  try {
    return localStorage.getItem(LINK_TARGET_KEY) === 'system' ? 'system' : 'netfory'
  } catch {
    return 'netfory'
  }
}
export function setLinkTarget(v: LinkTarget) {
  try { localStorage.setItem(LINK_TARGET_KEY, v) } catch { /* noop */ }
}

export function isExternalHttp(href: string): boolean {
  if (!/^https?:\/\//i.test(href)) return false
  try {
    const host = new URL(href).hostname.toLowerCase()
    return host !== window.location.hostname && host !== 'localhost' && !host.endsWith('.localhost')
  } catch {
    return false
  }
}

// Оригинальный window.open захватываем ОДИН раз — до патча ниже.
const nativeOpen: typeof window.open | null =
  typeof window !== 'undefined' ? window.open.bind(window) : null

/** Открыть URL через оригинальный (непропатченный) window.open. */
export function openNative(url: string) {
  nativeOpen?.(url, '_blank', 'noopener')
}

/** Программный window.open(внешний http(s)) → web-вкладка NETFORY. */
export function installWindowOpenGuard() {
  if (typeof window === 'undefined' || (window as any).__clearnetGuard) return
  ;(window as any).__clearnetGuard = true
  window.open = ((url?: string | URL, target?: string, features?: string) => {
    const href = typeof url === 'string' ? url : url?.toString() || ''
    if (isExternalHttp(href)) {
      openInNetfory(href)
      return null
    }
    return nativeOpen ? nativeOpen(url as any, target, features) : null
  }) as typeof window.open
}
