import { defineStore } from 'pinia'
import { hexToBytes } from '@noble/hashes/utils'
import {
  isTauri,
  listApps,
  createDraftApp,
  createDraftAtPath,
  finalizePublish,
  editAppMetadata,
  previewDraft,
  saveAppWeb,
  replaceAppWeb,
  deleteApp,
  updateAppNative,
  updateAppAt,
  startDiscovery,
  type Manifest,
} from '@/lib/bridge'
import { useAuthStore } from '@/store/auth'
import { replicateNow } from '@/lib/seederRep'
import { deriveAppIdentity, signDigestForApp, sha256Hex } from '@/lib/crypto'

interface AppsState {
  apps: Manifest[]
  busy: boolean
  lastError: string
  loaded: boolean
  favorites: string[]
  publishTick: number
  lastPublishedId: string
  editRequestId: string
}

const FAV_KEY = 'sth_favorites'
function loadFavorites(): string[] {
  try {
    return JSON.parse(localStorage.getItem(FAV_KEY) || '[]')
  } catch {
    return []
  }
}

/** Pull the root README.md text out of an uploaded folder (web publish). */
async function extractReadme(files: FileList): Promise<string | undefined> {
  for (const f of Array.from(files)) {
    const rel = (f as any).webkitRelativePath || f.name
    const path = (rel.split('/').slice(1).join('/') || f.name).toLowerCase()
    if (path === 'readme.md') return await f.text()
  }
  return undefined
}

function cleanTags(tags: string[]): string[] {
  const out: string[] = []
  for (const t of tags) {
    const v = t.trim().toLowerCase()
    if (v && !out.includes(v)) out.push(v)
    if (out.length >= 8) break
  }
  return out
}

async function hashFiles(
  files: FileList,
): Promise<{ entries: { path: string; sha256: string; size: number }[]; merkle: string }> {
  const entries: { path: string; sha256: string; size: number }[] = []
  for (const f of Array.from(files)) {
    const buf = new Uint8Array(await f.arrayBuffer())
    const rel = (f as any).webkitRelativePath || f.name
    const path = rel.split('/').slice(1).join('/') || f.name
    entries.push({ path, sha256: sha256Hex(buf), size: f.size })
  }
  entries.sort((a, b) => a.path.localeCompare(b.path))
  const merkle = sha256Hex(new TextEncoder().encode(entries.map((e) => e.sha256).join('')))
  return { entries, merkle }
}

export const useAppsStore = defineStore('apps', {
  state: (): AppsState => ({
    apps: [],
    busy: false,
    lastError: '',
    loaded: false,
    favorites: loadFavorites(),
    publishTick: 0,
    lastPublishedId: '',
    editRequestId: '',
  }),

  getters: {
    count: (s) => s.apps.length,
    // Apps I authored (owner == my address) — eligible for update/delete.
    myApps: (s) => {
      const auth = useAuthStore()
      return s.apps.filter((m) => m.owner === auth.address)
    },
    // My PUBLISHED apps (status published or legacy without a status).
    myPublished(): Manifest[] {
      return (this as any).myApps.filter((m: Manifest) => (m.status ?? 'published') === 'published')
    },
    // My DRAFTS (local-only, not yet published/paid).
    myDrafts(): Manifest[] {
      return (this as any).myApps.filter((m: Manifest) => m.status === 'draft')
    },
    // dApps installed from OTHER developers (foreign owner) — read-only.
    installedApps: (s) => {
      const auth = useAuthStore()
      return s.apps.filter((m) => m.owner !== auth.address)
    },
    // Starred installed apps, surfaced on the dashboard.
    favoriteApps: (s) => s.apps.filter((m) => s.favorites.includes(m.id)),
  },

  actions: {
    async init() {
      this.apps = await listApps()
      this.loaded = true
    },

    /** Request the MyApplications view to open the edit modal for an app. */
    requestEdit(id: string) {
      this.editRequestId = id
    },

    /** Create a local DRAFT (native: copy/hash/sign; web: client-side manifest). */
    async createDraft(
      name: string,
      description: string,
      tags: string[] = [],
      files?: FileList | null,
      logo?: number[] | null,
      preview?: number[] | null,
    ): Promise<Manifest | null> {
      this.busy = true
      this.lastError = ''
      try {
        if (!name.trim()) {
          this.lastError = 'no-title'
          return null
        }
        if (!description.trim()) {
          this.lastError = 'no-description'
          return null
        }
        const tg = cleanTags(tags)
        if (isTauri) {
          const m = await createDraftApp(name, description, tg, logo, preview)
          if (m) this.apps.push(m)
          return m
        }
        if (!files || !files.length) {
          this.lastError = 'no-files'
          return null
        }
        const m = await this.publishWeb(name, description, tg, files)
        m.status = 'draft'
        this.apps.push(m)
        return m
      } catch (e: any) {
        this.lastError = e?.message || 'publish-failed'
        return null
      } finally {
        this.busy = false
      }
    },

    /** Native drag-and-drop: create a DRAFT from a dropped folder PATH (Tauri only). */
    async createDraftFromPath(
      name: string,
      description: string,
      tags: string[] = [],
      dir: string,
      logo?: number[] | null,
      preview?: number[] | null,
      privateRecipients?: string[] | null,
    ): Promise<Manifest | null> {
      this.busy = true
      this.lastError = ''
      try {
        if (!name.trim()) {
          this.lastError = 'no-title'
          return null
        }
        if (!description.trim()) {
          this.lastError = 'no-description'
          return null
        }
        const m = await createDraftAtPath(name, description, cleanTags(tags), dir, logo, preview, privateRecipients)
        if (m) this.apps.push(m)
        return m
      } catch (e: any) {
        this.lastError = e?.message || 'publish-failed'
        return null
      } finally {
        this.busy = false
      }
    },

    /** Finalize a draft → published. free=false requires a confirmed escrow txid. */
    async finalize(id: string, free: boolean, txid?: string): Promise<Manifest | null> {
      this.busy = true
      this.lastError = ''
      try {
        if (isTauri) {
          const m = await finalizePublish(id, free, txid)
          if (m) {
            this.replaceLocal(m)
            this.lastPublishedId = id
            this.publishTick = Date.now()
            // Критично: пере-сидируем новый blob и пересобираем анонс-снапшот,
            // иначе на трекеры/DHT продолжает уходить СТАРАЯ версия (напр. v1),
            // и сидеры никогда не узнают о новой версии до перезапуска приложения.
            startDiscovery().catch((e) => console.warn('re-announce after publish failed', e))
            // Параллельно с обычной публикацией — мгновенная репликация в Cloud Seeder
            replicateNow()
          }
          return m
        }
        // web preview: just flip the local manifest
        const m = this.apps.find((x) => x.id === id)
        if (m) {
          m.status = 'published'
          m.free = free
          m.paid = !free
          m.escrowTxid = txid
          this.lastPublishedId = id
          this.publishTick = Date.now()
        }
        return m ?? null
      } catch (e: any) {
        this.lastError = e?.message || 'publish-failed'
        return null
      } finally {
        this.busy = false
      }
    },

    /** Live-preview a draft (re-ingest from dev folder, zero cost). */
    async preview(id: string): Promise<Manifest | null> {
      const m = await previewDraft(id)
      if (m) this.replaceLocal(m)
      return m
    },

    async publishWeb(
      name: string,
      description: string,
      tags: string[],
      files: FileList,
    ): Promise<Manifest> {
      const auth = useAuthStore()
      const secret = (await auth.revealSeed()) || ''
      const index = this.apps.length
      const idAcc = deriveAppIdentity(secret, index)
      const { entries, merkle } = await hashFiles(files)
      const readme = await extractReadme(files)

      const rootName = name.trim()
      const desc = description.trim()
      const created = String(Math.floor(Date.now() / 1000))
      const digestHex = sha256Hex(
        new TextEncoder().encode(`${idAcc.address}|${rootName}|${desc}|${merkle}`),
      )
      const signature = secret ? signDigestForApp(secret, index, hexToBytes(digestHex)) : ''

      const manifest: Manifest = {
        id: idAcc.address,
        name: rootName,
        description: desc,
        owner: auth.address,
        files: entries,
        merkle,
        created,
        signature,
        publicKey: idAcc.publicKey,
        path: idAcc.path,
        index,
        version: 1,
        updated: created,
        tags,
        readme,
      }
      await saveAppWeb(manifest)
      return manifest
    },

    /** Update an existing app in place — SAME id/key, new files, new signature, version++. */
    async update(id: string, files?: FileList | null): Promise<Manifest | null> {
      this.busy = true
      this.lastError = ''
      try {
        if (isTauri) {
          const m = await updateAppNative(id)
          if (m) {
            this.replaceLocal(m)
            startDiscovery().catch((e) => console.warn('re-announce after update failed', e))
            // Обновление dApp → мгновенная репликация в Cloud Seeder
            replicateNow()
          }
          return m
        }
        if (!files || !files.length) {
          this.lastError = 'no-files'
          return null
        }
        const m = await this.updateWeb(id, files)
        if (m) this.replaceLocal(m)
        return m
      } catch (e: any) {
        this.lastError = e?.message || 'update-failed'
        return null
      } finally {
        this.busy = false
      }
    },

    /** FREE metadata edit (title/description/tags + optional logo/preview) — no payment. */
    async editMetadata(
      id: string,
      name: string,
      description: string,
      tags: string[],
      logo?: number[] | null,
      preview?: number[] | null,
    ): Promise<Manifest | null> {
      this.busy = true
      this.lastError = ''
      try {
        const m = await editAppMetadata(id, name, description, tags, logo, preview)
        if (m) {
          this.replaceLocal(m)
          if (isTauri) {
            startDiscovery().catch((e) => console.warn('re-announce after edit failed', e))
            // Метаданные тоже уходят в dapps.enc сидера → мгновенная репликация
            replicateNow()
          }
        }
        return m
      } catch (e: any) {
        this.lastError = typeof e === 'string' ? e : e?.message || 'edit-failed'
        return null
      } finally {
        this.busy = false
      }
    },

    /** Native pre-priced update: re-publish a new version from a known folder PATH. */
    async updateFromPath(id: string, dir: string, txid?: string): Promise<Manifest | null> {
      this.busy = true
      this.lastError = ''
      try {
        const m = await updateAppAt(id, dir, txid)
        if (m) {
          this.replaceLocal(m)
          if (isTauri) {
            startDiscovery().catch((e) => console.warn('re-announce after update failed', e))
            // Обновление dApp → мгновенная репликация в Cloud Seeder (не ждём 30 мин)
            replicateNow()
          }
        }
        return m
      } catch (e: any) {
        this.lastError = e?.message || 'update-failed'
        return null
      } finally {
        this.busy = false
      }
    },

    async updateWeb(id: string, files: FileList): Promise<Manifest | null> {      const existing = this.apps.find((m) => m.id === id)
      if (!existing) return null
      const auth = useAuthStore()
      const secret = (await auth.revealSeed()) || ''
      const index = existing.index ?? 0
      const idAcc = deriveAppIdentity(secret, index)
      // sanity: the re-derived address must still match the immutable app id
      if (idAcc.address !== id) return null

      const { entries, merkle } = await hashFiles(files)
      const desc = existing.description || ''
      const updated = String(Math.floor(Date.now() / 1000))
      const digestHex = sha256Hex(
        new TextEncoder().encode(`${id}|${existing.name}|${desc}|${merkle}`),
      )
      const signature = secret ? signDigestForApp(secret, index, hexToBytes(digestHex)) : ''

      const manifest: Manifest = {
        ...existing,
        files: entries,
        merkle,
        signature,
        updated,
        version: (existing.version || 1) + 1,
      }
      await replaceAppWeb(manifest)
      return manifest
    },

    async remove(id: string) {
      await deleteApp(id)
      this.apps = this.apps.filter((m) => m.id !== id)
      this.reannounce()
      // best-effort: убрать архив удалённого dApp и со своего cloud seed диска
      void import('@/lib/seederRep').then((m) => m.deleteDappFromCloud(id))
    },

    replaceLocal(manifest: Manifest) {
      const i = this.apps.findIndex((m) => m.id === manifest.id)
      if (i >= 0) this.apps[i] = manifest
      else this.apps.push(manifest)
      // Переанонсировать набор приложений в сеть (mDNS + снапшот трекера),
      // чтобы после публикации/обновления версии пиры сразу видели новую
      // версию, а не старую из устаревшего снапшота (иначе v3 у автора, но v1
      // у других, пока автор снова не откроет «Найти приложения»).
      this.reannounce()
    },

    /** Fire-and-forget: rebuild the discovery announce (version, hash, metadata). */
    reannounce() {
      if (!isTauri) return
      startDiscovery().catch(() => {
        /* p2p может быть выключен — не критично */
      })
    },

    isFavorite(id: string): boolean {
      return this.favorites.includes(id)
    },

    toggleFavorite(id: string) {
      if (this.favorites.includes(id)) {
        this.favorites = this.favorites.filter((x) => x !== id)
      } else {
        this.favorites = [...this.favorites, id]
      }
      try {
        localStorage.setItem(FAV_KEY, JSON.stringify(this.favorites))
      } catch {
        /* ignore */
      }
    },
  },
})
