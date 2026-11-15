import { defineStore } from 'pinia'

const STORAGE_KEY = 'oxid.theme'

export const useThemeStore = defineStore('theme', {
  state: () => ({
    mode: 'dark',
  }),
  actions: {
    init() {
      const stored = localStorage.getItem(STORAGE_KEY)
      this.mode = stored === 'light' ? 'light' : 'dark'
      this.apply()
    },
    apply() {
      const html = document.documentElement
      html.classList.toggle('dark', this.mode === 'dark')
      html.classList.toggle('light', this.mode === 'light')
    },
    toggle() {
      this.mode = this.mode === 'dark' ? 'light' : 'dark'
      localStorage.setItem(STORAGE_KEY, this.mode)
      this.apply()
    },
    set(mode) {
      this.mode = mode === 'light' ? 'light' : 'dark'
      localStorage.setItem(STORAGE_KEY, this.mode)
      this.apply()
    },
  },
})
