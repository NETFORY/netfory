import { createI18n } from 'vue-i18n'
import ru from './ru'
import en from './en'
import id from './id'

const SUPPORTED = ['ru', 'en', 'id'] as const
type Locale = typeof SUPPORTED[number]

const stored = (() => {
  try {
    return localStorage.getItem('sth_locale')
  } catch {
    return null
  }
})()

export const detectBrowserLocale = (): Locale => {
  try {
    const candidates: string[] = []
    if (typeof navigator !== 'undefined') {
      if (Array.isArray(navigator.languages)) candidates.push(...navigator.languages)
      if (navigator.language) candidates.push(navigator.language)
    }
    for (const raw of candidates) {
      const code = raw.toLowerCase().split(/[-_]/)[0]
      if ((SUPPORTED as readonly string[]).includes(code)) return code as Locale
    }
  } catch {
    /* ignore */
  }
  return 'en'
}

const initialLocale: Locale =
  stored && (SUPPORTED as readonly string[]).includes(stored)
    ? (stored as Locale)
    : detectBrowserLocale()

export const i18n = createI18n({
  legacy: false,
  globalInjection: true,
  locale: initialLocale,
  fallbackLocale: 'en',
  messages: { ru, en, id },
})
