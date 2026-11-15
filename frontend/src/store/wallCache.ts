// Кэш стены: мгновенное открытие профиля из ранее загруженных постов.
// - Посты (JSON, лёгкие) — persist в localStorage, LRU до 20 авторов.
// - Медиа (data-URL картинок) и файлы (objectURL mp3/mp4) — в памяти на всю
//   сессию, ключ = CID (контент неизменяемый → кэш не протухает). Байты на
//   диске уже кэширует Rust (social_cache/ и filesdata/), поэтому после
//   рестарта первая загрузка идёт с диска, а не из сети.
import { defineStore } from 'pinia'
import { fetchWallMedia, socialFileBytes, type WallPost, type WallComment, type PostFile } from '@/lib/bridge'

const LS_KEY = 'sn.wallCache.v1'
const MAX_AUTHORS = 20
const MAX_POSTS = 50
const MAX_COMMENTS = 100

const inflight = new Set<string>()

type CommentsByAuthor = Record<string, Record<string, WallComment[]>>

function loadLS(): { posts: Record<string, WallPost[]>; order: string[]; comments: CommentsByAuthor } {
  try {
    const raw = localStorage.getItem(LS_KEY)
    if (raw) {
      const d = JSON.parse(raw)
      if (d && typeof d.posts === 'object' && Array.isArray(d.order)) {
        return {
          posts: d.posts,
          order: d.order,
          comments: d.comments && typeof d.comments === 'object' ? d.comments : {},
        }
      }
    }
  } catch { /* повреждённый кэш игнорируем */ }
  return { posts: {}, order: [], comments: {} }
}

export const useWallCacheStore = defineStore('wallCache', {
  state: () => ({
    ...loadLS(),
    media: {} as Record<string, string>, // cid -> data-URL (картинки/аватары)
    fileUrls: {} as Record<string, string>, // cid -> objectURL (mp3/mp4/img-файлы)
  }),
  actions: {
    get(author: string): WallPost[] | null {
      const list = this.posts[author]
      return list && list.length ? list : null
    },
    save(author: string, list: WallPost[]) {
      if (!author) return
      this.posts[author] = list.slice(0, MAX_POSTS)
      this.order = [author, ...this.order.filter((a) => a !== author)].slice(0, MAX_AUTHORS)
      for (const a of Object.keys(this.posts)) {
        if (!this.order.includes(a)) delete this.posts[a]
      }
      for (const a of Object.keys(this.comments)) {
        if (!this.order.includes(a)) delete this.comments[a]
      }
      // комментарии удалённых из ленты постов тоже чистим
      const alive = new Set((this.posts[author] || []).map((p) => p.id))
      const byPost = this.comments[author]
      if (byPost) {
        for (const pid of Object.keys(byPost)) {
          if (!alive.has(pid)) delete byPost[pid]
        }
      }
      this.persist()
    },
    getComments(author: string, postId: string): WallComment[] | null {
      const list = this.comments[author]?.[postId]
      return list && list.length ? list : null
    },
    saveComments(author: string, postId: string, list: WallComment[]) {
      if (!author || !postId) return
      if (!this.comments[author]) this.comments[author] = {}
      this.comments[author][postId] = list.slice(-MAX_COMMENTS)
      this.persist()
    },
    persist() {
      const payload = () =>
        JSON.stringify({ posts: this.posts, order: this.order, comments: this.comments })
      try {
        localStorage.setItem(LS_KEY, payload())
      } catch {
        // квота localStorage: оставляем 5 самых свежих авторов
        this.order = this.order.slice(0, 5)
        for (const a of Object.keys(this.posts)) {
          if (!this.order.includes(a)) delete this.posts[a]
        }
        for (const a of Object.keys(this.comments)) {
          if (!this.order.includes(a)) delete this.comments[a]
        }
        try {
          localStorage.setItem(LS_KEY, payload())
        } catch { /* не судьба */ }
      }
    },
    async loadMedia(cid: string, node: string) {
      if (!cid || this.media[cid] || inflight.has('m' + cid)) return
      inflight.add('m' + cid)
      try {
        const url = await fetchWallMedia(cid, node)
        if (url) this.media[cid] = url
      } catch { /* сеть/блоб недоступны — покажется плейсхолдер */ } finally {
        inflight.delete('m' + cid)
      }
    },
    async ensureFileUrl(f: PostFile) {
      if (!f.cid || this.fileUrls[f.cid] || inflight.has('f' + f.cid)) return
      inflight.add('f' + f.cid)
      try {
        const b64 = await socialFileBytes(f.cid, f.node)
        if (!b64) return
        const bin = atob(b64)
        const bytes = new Uint8Array(bin.length)
        for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
        this.fileUrls[f.cid] = URL.createObjectURL(
          new Blob([bytes], { type: f.mime || 'application/octet-stream' }),
        )
      } catch { /* добор не удался — кнопка загрузки останется */ } finally {
        inflight.delete('f' + f.cid)
      }
    },
  },
})
