// Простой in-memory стор для всплывающих уведомлений (P2P-раздача,
// системные подсказки). Автоскрытие по ttl. Без внешних зависимостей.
// Поддерживает прогресс-бар (0..100) через поле `progress` — используется
// для «файл X из Y раздано» при P2P-отправке вложений.
import { defineStore } from 'pinia'

let uid = 0

export const useToastStore = defineStore('toast', {
  state: () => ({
    items: [], // { id, kind, title, text, ttl, progress?: 0..100 }
  }),
  actions: {
    push({ kind = 'info', title = '', text = '', ttl = 6000, progress = null } = {}) {
      const id = ++uid
      const item = { id, kind, title, text, ttl, progress, _timer: null }
      if (ttl > 0) item._timer = setTimeout(() => this.dismiss(id), ttl)
      this.items.push(item)
      return id
    },
    /// Обновить существующий toast: текст/kind/progress. Опционально перезаписать ttl
    /// (например, для перехода из «идёт раздача» → «✓ раздано» на 6с).
    update(id, patch = {}) {
      const t = this.items.find((x) => x.id === id)
      if (!t) return
      if ('kind' in patch) t.kind = patch.kind
      if ('title' in patch) t.title = patch.title
      if ('text' in patch) t.text = patch.text
      if ('progress' in patch) t.progress = patch.progress
      if ('ttl' in patch) {
        if (t._timer) clearTimeout(t._timer)
        t.ttl = patch.ttl
        if (patch.ttl > 0) t._timer = setTimeout(() => this.dismiss(id), patch.ttl)
      }
    },
    dismiss(id) {
      const t = this.items.find((x) => x.id === id)
      if (t?._timer) clearTimeout(t._timer)
      this.items = this.items.filter((x) => x.id !== id)
    },
    clear() {
      for (const t of this.items) if (t._timer) clearTimeout(t._timer)
      this.items = []
    },
  },
})
