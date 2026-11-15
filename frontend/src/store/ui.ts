import { defineStore } from 'pinia'
import { i18n, detectBrowserLocale } from '@/i18n'

export type ThemeId =
  | 'default-dark'
  | 'default-light'
  | 'focus'
  | 'matte-dark'
  | 'readable-light'
  | 'eink'
  | 'neon'
export type LocaleId = 'ru' | 'en' | 'id'
export type FontId = 'jetbrains' | 'plex' | 'fira' | 'inter' | 'tenor' | 'saira'
export type DensityId = 'default' | 'compact'
export type FontSizeId = 's' | 'm' | 'l'

const THEME_KEY = 'sth_theme'
const LOCALE_KEY = 'sth_locale'
const FONT_KEY = 'sth_font'
const DENSITY_KEY = 'sth_density'
const FONTSIZE_KEY = 'sth_fontsize'
const FONTWEIGHT_KEY = 'sth_fontweight'
const NOBLUR_KEY = 'sth_noblur'
const SIDEBAR_KEY = 'sth_sidebar_collapsed'
const MODE_KEY = 'sth_ui_mode'
const MESH_OPEN_KEY = 'sth_dashboard_mesh_open'

// USER — «нормис»-режим: только основные страницы, без DevHub, MyApps и MeshMeter.
// DEV — как USER + My Applications + DevHub (для авторов dApp'ов).
// GEEK — полный набор: MeshMeter, все технические разделы, все хеши/CID видны.
export type UiMode = 'user' | 'dev' | 'geek'

// Theme cycle order for the Ctrl+Shift+T hotkey.
const THEME_CYCLE: ThemeId[] = ['default-dark', 'default-light', 'focus', 'matte-dark', 'readable-light', 'eink', 'neon']

interface UiState {
  theme: ThemeId
  locale: LocaleId
  font: FontId
  density: DensityId
  fontSize: FontSizeId
  fontWeight: number
  noBlur: boolean
  sidebarCollapsed: boolean
  /** узкий экран (≤ NARROW_PX): меню всегда свёрнуто, раскрывается поверх контента */
  narrow: boolean
  overlayCount: number
  mode: UiMode
  meshMeterOpen: boolean
}

function read<T extends string>(key: string, fallback: T): T {
  try {
    return (localStorage.getItem(key) as T) || fallback
  } catch {
    return fallback
  }
}

export const NARROW_PX = 768
const isNarrowNow = () => typeof window !== 'undefined' && window.innerWidth <= NARROW_PX

export const useUiStore = defineStore('ui', {
  state: (): UiState => ({
    theme: 'default-dark',
    // Дефолт языка — по локали системы/браузера (en для неподдерживаемых),
    // а не жёсткий 'ru': англоязычный Linux при первом запуске получал русский UI.
    locale: detectBrowserLocale(),
    font: 'inter',
    density: 'default',
    fontSize: 'm',
    fontWeight: 400,
    noBlur: false,
    sidebarCollapsed: isNarrowNow() || read(SIDEBAR_KEY, '') === '1',
    narrow: isNarrowNow(),
    overlayCount: 0,
    // Дефолт — USER, чтобы новый пользователь видел лаконичный интерфейс.
    // GEEK-режим восстанавливается из localStorage только осознанно.
    mode: (() => {
      const v = read<UiMode>(MODE_KEY, 'user')
      return v === 'dev' || v === 'geek' ? v : 'user'
    })(),
    // GEEK — по умолчанию MeshMeter развёрнут (техно-режим); иначе схлопнут в badge.
    meshMeterOpen: (() => {
      const raw = read(MESH_OPEN_KEY, '')
      if (raw === '1') return true
      if (raw === '0') return false
      return read<UiMode>(MODE_KEY, 'user') === 'geek'
    })(),
  }),

  getters: {
    // True while any Vue modal/overlay is open. Native child webviews float
    // ABOVE the DOM, so AppLayout hides them while an overlay is up (z-order).
    overlayOpen: (s) => s.overlayCount > 0,
  },

  actions: {
    init() {
      const rawTheme = read<string>(THEME_KEY, 'default-dark')
      // Миграция: тема "neumo" (Neumorphic light) удалена — уводим на default-light.
      this.theme =
        rawTheme === 'neumo'
          ? ((): ThemeId => {
              try {
                localStorage.setItem(THEME_KEY, 'default-light')
              } catch {
                /* noop */
              }
              return 'default-light'
            })()
          : (rawTheme as ThemeId)
      this.locale = read<LocaleId>(LOCALE_KEY, detectBrowserLocale())
      this.font = read<FontId>(FONT_KEY, 'inter')
      this.density = read<DensityId>(DENSITY_KEY, 'default')
      this.fontSize = read<FontSizeId>(FONTSIZE_KEY, 'm')
      this.fontWeight = parseInt(read(FONTWEIGHT_KEY, '400'), 10) || 400
      this.noBlur = read(NOBLUR_KEY, '0') === '1'
      this.applyTheme()
      this.applyLocale()
      this.applyFont()
      this.applyDensity()
      this.applyFontSize()
      this.applyFontWeight()
      this.applyNoBlur()
    },

    applyTheme() {
      document.documentElement.setAttribute('data-theme', this.theme)
    },

    applyLocale() {
      document.documentElement.setAttribute('lang', this.locale)
      ;(i18n.global.locale as any).value = this.locale
    },

    applyFont() {
      document.documentElement.setAttribute('data-font', this.font)
    },

    applyDensity() {
      document.documentElement.setAttribute('data-density', this.density)
    },

    applyFontSize() {
      document.documentElement.setAttribute('data-size', this.fontSize)
    },

    applyFontWeight() {
      document.documentElement.style.setProperty('--ui-fw', String(this.fontWeight))
    },

    applyNoBlur() {
      if (this.noBlur) document.documentElement.setAttribute('data-noblur', 'on')
      else document.documentElement.removeAttribute('data-noblur')
    },

    setTheme(theme: ThemeId) {
      const apply = () => {
        this.theme = theme
        try {
          localStorage.setItem(THEME_KEY, theme)
        } catch {
          /* noop */
        }
        this.applyTheme()
      }
      // Плавный переход между темами: View Transitions API (кросс-фейд в
      // Chromium/WebView2) с фоллбэком на CSS-транзишен цветов (класс theme-anim).
      const doc = document.documentElement
      const anyDoc = document as unknown as { startViewTransition?: (cb: () => void) => void }
      if (typeof anyDoc.startViewTransition === 'function') {
        anyDoc.startViewTransition(apply)
        return
      }
      doc.classList.add('theme-anim')
      apply()
      window.setTimeout(() => doc.classList.remove('theme-anim'), 350)
    },

    // Advance to the next theme in the cycle (Ctrl+Shift+T). Returns the new id.
    cycleTheme(): ThemeId {
      const idx = THEME_CYCLE.indexOf(this.theme)
      const next = THEME_CYCLE[(idx + 1) % THEME_CYCLE.length]
      this.setTheme(next)
      return next
    },

    /** Тумблер ☀/☾ для DEFAULT-темы: dark ↔ light. */
    toggleDayNight() {
      this.setTheme(this.theme === 'default-light' ? 'default-dark' : 'default-light')
    },

    setLocale(locale: LocaleId) {
      this.locale = locale
      try {
        localStorage.setItem(LOCALE_KEY, locale)
      } catch {
        /* noop */
      }
      this.applyLocale()
    },

    setFont(font: FontId) {
      this.font = font
      try {
        localStorage.setItem(FONT_KEY, font)
      } catch {
        /* noop */
      }
      this.applyFont()
    },

    setDensity(density: DensityId) {
      this.density = density
      try {
        localStorage.setItem(DENSITY_KEY, density)
      } catch {
        /* noop */
      }
      this.applyDensity()
    },

    setFontSize(size: FontSizeId) {
      this.fontSize = size
      try {
        localStorage.setItem(FONTSIZE_KEY, size)
      } catch {
        /* noop */
      }
      this.applyFontSize()
    },

    setFontWeight(weight: number) {
      this.fontWeight = weight
      try {
        localStorage.setItem(FONTWEIGHT_KEY, String(weight))
      } catch {
        /* noop */
      }
      this.applyFontWeight()
    },

    setNoBlur(v: boolean) {
      this.noBlur = v
      try {
        localStorage.setItem(NOBLUR_KEY, v ? '1' : '0')
      } catch {
        /* noop */
      }
      this.applyNoBlur()
    },

    // Restore all appearance settings (theme/font/weight/size/density) to defaults.
    resetAppearance() {
      this.setTheme('default-dark')
      this.setFont('inter')
      this.setDensity('default')
      this.setFontSize('m')
      this.setFontWeight(400)
      this.setNoBlur(false)
    },

    /** Смена ширины окна: на узком меню сворачивается (выбор пользователя не трогаем), на широком — восстанавливается. */
    setNarrow(n: boolean) {
      if (n === this.narrow) return
      this.narrow = n
      this.sidebarCollapsed = n || read(SIDEBAR_KEY, '') === '1'
    },
    /** На узком экране раскрытое меню закрывается после перехода/клика мимо. */
    collapseIfNarrow() {
      if (this.narrow) this.sidebarCollapsed = true
    },
    toggleSidebar() {
      this.sidebarCollapsed = !this.sidebarCollapsed
      // на узком экране раскрытие временное — сохранённый выбор для широкого не меняем
      if (this.narrow) return
      try {
        localStorage.setItem(SIDEBAR_KEY, this.sidebarCollapsed ? '1' : '0')
      } catch {
        /* noop */
      }
    },

    /** Переключить USER/DEV/GEEK-режим интерфейса (persist в localStorage). */
    setMode(mode: UiMode) {
      this.mode = mode
      try {
        localStorage.setItem(MODE_KEY, mode)
      } catch {
        /* noop */
      }
    },

    /** Свернуть/развернуть MeshMeter на Dashboard (persist в localStorage). */
    setMeshMeterOpen(open: boolean) {
      this.meshMeterOpen = open
      try {
        localStorage.setItem(MESH_OPEN_KEY, open ? '1' : '0')
      } catch {
        /* noop */
      }
    },

    openOverlay() {
      this.overlayCount += 1
    },

    closeOverlay() {
      this.overlayCount = Math.max(0, this.overlayCount - 1)
    },
  },
})
