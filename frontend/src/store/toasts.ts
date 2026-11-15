import { defineStore } from 'pinia'

export type ToastKind = 'in' | 'info' | 'error'

export interface Toast {
  id: number
  title: string
  body: string
  kind: ToastKind
  /** Опциональный обработчик клика — вызывается перед удалением тоста. */
  onOpen?: () => void
}

let seq = 0

export const useToastsStore = defineStore('toasts', {
  state: () => ({
    items: [] as Toast[],
  }),
  actions: {
    push(
      title: string,
      body: string,
      kind: ToastKind = 'info',
      ttl = 3000,
      onOpen?: () => void,
    ) {
      const id = ++seq
      this.items.push({ id, title, body, kind, onOpen })
      setTimeout(() => this.remove(id), ttl)
      return id
    },
    /** Клик по тосту: если задан onOpen — вызываем, затем убираем. */
    open(id: number) {
      const t = this.items.find((x) => x.id === id)
      if (t?.onOpen) {
        try {
          t.onOpen()
        } catch {
          /* игнорируем ошибки навигации, чтобы тост всё равно закрылся */
        }
      }
      this.remove(id)
    },
    remove(id: number) {
      this.items = this.items.filter((t) => t.id !== id)
    },
  },
})
