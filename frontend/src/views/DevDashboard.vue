<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import {
  appListen,
  devhubDiscoverNetwork,
  devhubFeed,
  getMyUsername,
  getUserRepositories,
  isTauri,
  quickAddMagnet,
  searchDevNetwork,
  startFileDownload,
  type DevAssetInfo,
  type DevFeedItem,
  type DevRepository,
  type DevSearchResult,
} from '@/lib/bridge'
import { useToastsStore } from '@/store/toasts'
import UserAvatar from '@/components/UserAvatar.vue'

const router = useRouter()
const toasts = useToastsStore()
const query = ref('')
const results = ref<DevSearchResult[]>([])
const popular = ref<DevSearchResult[]>([])
const myUsername = ref<string>('')
const myRepos = ref<DevRepository[]>([])
const loading = ref(false)
const discovering = ref(false)
// лента свежих пушей + фильтр «только подписки»
const feed = ref<DevFeedItem[]>([])
const feedFollowedOnly = ref(true)
const feedView = computed(() =>
  feedFollowedOnly.value ? feed.value.filter((f) => f.followed) : feed.value,
)
let debounce: ReturnType<typeof setTimeout> | undefined
let syncDebounce: ReturnType<typeof setTimeout> | undefined
let unlistenSync: (() => void) | null = null

async function runSearch() {
  loading.value = true
  try {
    results.value = await searchDevNetwork(query.value)
  } finally {
    loading.value = false
  }
}
watch(query, () => {
  if (debounce) clearTimeout(debounce)
  debounce = setTimeout(runSearch, 250)
})

function openRepo(username: string, repo: string) {
  router.push({ name: 'dev-repo', params: { username, repo } })
}
function openUser(username: string) {
  router.push({ name: 'dev-user', params: { username } })
}

async function reloadLists() {
  popular.value = await searchDevNetwork('')
  feed.value = await devhubFeed()
  // если подписок в ленте нет — показываем всю сеть, чтобы лента не пустовала
  if (feedFollowedOnly.value && !feed.value.some((f) => f.followed)) {
    feedFollowedOnly.value = false
  }
  if (query.value) await runSearch()
}

function feedAge(ts: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000) - ts)
  if (s < 3600) return `${Math.max(1, Math.floor(s / 60))} min ago`
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`
  return `${Math.floor(s / 86400)} d ago`
}

// Компактный формат размера ассета для кнопки скачивания.
function fmtSize(b: number): string {
  if (!b || b < 1024) return `${b || 0} B`
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`
  if (b < 1024 * 1024 * 1024) return `${(b / 1024 / 1024).toFixed(1)} MB`
  return `${(b / 1024 / 1024 / 1024).toFixed(2)} GB`
}

// Реестр «сейчас качается» — блокирует повторный клик и подсвечивает кнопку.
const downloadingCid = ref<Record<string, boolean>>({})
async function downloadReleaseAsset(asset: DevAssetInfo) {
  const cid = asset.cid
  if (!cid || downloadingCid.value[cid]) return
  downloadingCid.value[cid] = true
  try {
    if (!isTauri) {
      // Web-превью: не дёргаем нативный клиент, просто эмулируем тост.
      toasts.push('dev://', `Demo: desktop client would fetch «${asset.name}» via BitTorrent`, 'info', 4000)
      return
    }
    // v1.63.2: для релизов v1.62+ (у которых есть magnet) используем
    // torrent fast-path — старт мгновенный, метаданные подтянутся в фоне.
    // Legacy-релизы без magnet идут по прежнему f://-пути.
    if (asset.magnet) {
      await quickAddMagnet(asset.magnet)
      toasts.push('dev://', `⇩ «${asset.name}» added — download in progress (see Torrents)`, 'in', 5000)
    } else {
      await startFileDownload(cid)
      toasts.push('dev://', `Download of «${asset.name}» started (f://) — see File Exchange`, 'in')
    }
  } catch (e: any) {
    toasts.push('dev://', typeof e === 'string' ? e : 'Failed to start download', 'error')
  } finally {
    downloadingCid.value[cid] = false
  }
}

// Клик по бейджу версии — открыть репозиторий на вкладке «Релизы».
function openRepoReleases(username: string, repo: string) {
  router.push({ name: 'dev-repo', params: { username, repo }, query: { tab: 'releases' } })
}

// Опрос сети вручную: синкаем всех зарегистрированных on-chain авторов.
async function discoverNetwork() {
  if (discovering.value) return
  discovering.value = true
  try {
    const n = await devhubDiscoverNetwork()
    toasts.push('dev://', `Polling ${n} network authors — discovered projects will appear here`, 'in')
  } finally {
    setTimeout(() => (discovering.value = false), 4000)
  }
}

onMounted(async () => {
  await reloadLists()
  myUsername.value = (await getMyUsername()) || ''
  if (myUsername.value) {
    try {
      const u = await getUserRepositories(myUsername.value)
      myRepos.value = u.repositories
    } catch {
      myRepos.value = []
    }
  }
  if (isTauri) {
    // Пришёл свежий индекс автора из P2P → обновляем списки (с дебаунсом).
    unlistenSync = await appListen('devhub-synced', () => {
      if (syncDebounce) clearTimeout(syncDebounce)
      syncDebounce = setTimeout(reloadLists, 800)
    })
  }
})
onBeforeUnmount(() => {
  if (unlistenSync) unlistenSync()
  if (syncDebounce) clearTimeout(syncDebounce)
})
</script>

<template>
  <div class="dev-hub" data-testid="dev-dashboard">
    <header class="dh-head">
      <div class="dh-head-row">
        <div>
          <h1 class="dh-title"><span class="mono">dev://</span> Development Hub</h1>
          <p class="dh-sub dim">Decentralized standard for SmartNet projects — search, your repositories and community projects.</p>
        </div>
        <div class="dh-head-actions">
          <button
            class="dh-refresh"
            :disabled="discovering"
            @click="discoverNetwork"
            title="Poll network: sync signed indices of every registered author"
            data-testid="dev-dashboard-discover-btn"
          >
            {{ discovering ? '⟳ Polling…' : '⟳ Refresh from network' }}
          </button>
          <button v-if="myUsername" class="dh-new" @click="router.push({ name: 'dev-new' })" data-testid="dev-dashboard-new-btn">
            + New repository
          </button>
        </div>
      </div>
    </header>

    <!-- глобальный P2P-поиск -->
    <div class="dh-search" data-testid="dev-search-row">
      <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
        <circle cx="11" cy="11" r="8"></circle><line x1="21" y1="21" x2="16.65" y2="16.65"></line>
      </svg>
      <input
        v-model="query"
        class="dh-input"
        type="text"
        placeholder="Search repositories across the network: name, project, description…"
        data-testid="dev-search-input"
      />
      <span v-if="loading" class="dim dh-load">…</span>
    </div>

    <!-- результаты поиска -->
    <section v-if="query" class="dh-block" data-testid="dev-search-results">
      <h2 class="dh-h2">Search results ({{ results.length }})</h2>
      <div v-if="results.length" class="dh-list">
        <button
          v-for="r in results"
          :key="r.username + '/' + r.repo"
          class="dh-row"
          @click="openRepo(r.username, r.repo)"
          :data-testid="`dev-result-${r.username}-${r.repo}`"
        >
          <span class="dh-repo mono">
            <UserAvatar :address="r.address" :size="18" class="dh-uav" />
            dev://{{ r.username }}/{{ r.repo }}
          </span>
          <span class="dh-desc dim">{{ r.description }}</span>
          <span class="dh-meta">★ {{ r.stars }} · <span class="seed">◉ {{ r.seeders }}</span></span>
        </button>
      </div>
      <p v-else class="dim">No matches found in the network for «{{ query }}».</p>
    </section>

    <!-- лента свежих пушей -->
    <section v-if="feed.length" class="dh-block" data-testid="dev-feed">
      <div class="dh-feed-head">
        <h2 class="dh-h2">Update feed</h2>
        <div class="dh-feed-toggle" role="tablist">
          <button
            class="dh-ftab"
            :class="{ on: feedFollowedOnly }"
            @click="feedFollowedOnly = true"
            data-testid="dev-feed-followed-tab"
          >Follows</button>
          <button
            class="dh-ftab"
            :class="{ on: !feedFollowedOnly }"
            @click="feedFollowedOnly = false"
            data-testid="dev-feed-all-tab"
          >Whole network</button>
        </div>
      </div>
      <div v-if="feedView.length" class="dh-list">
        <div
          v-for="f in feedView"
          :key="f.username + '/' + f.repo + '/' + f.updated"
          class="dh-feed-item"
        >
          <button
            class="dh-row dh-feed-row"
            @click="openRepo(f.username, f.repo)"
            :data-testid="`dev-feed-${f.username}-${f.repo}`"
          >
            <span class="dh-repo mono">
              <UserAvatar :address="f.address" :size="18" class="dh-uav" />
              <span class="at" @click.stop="openUser(f.username)">@{{ f.username }}</span>/{{ f.repo }}
              <span v-if="f.followed" class="dh-fol" title="You follow this author">★</span>
              <span
                v-if="f.latestRelease"
                class="dh-tag mono"
                :title="`Open release ${f.latestRelease.tag}`"
                :data-testid="`dev-feed-tag-${f.username}-${f.repo}`"
                @click.stop="openRepoReleases(f.username, f.repo)"
              >{{ f.latestRelease.tag }}</span>
            </span>
            <span class="dh-desc dim">{{ f.message || 'repository update' }}</span>
            <span class="dh-meta dim">{{ feedAge(f.updated) }}</span>
          </button>
          <!-- список файлов последнего релиза с прямыми ссылками на скачивание -->
          <div
            v-if="f.latestRelease && f.latestRelease.assets && f.latestRelease.assets.length"
            class="dh-assets"
            :data-testid="`dev-feed-assets-${f.username}-${f.repo}`"
          >
            <button
              v-for="a in f.latestRelease.assets"
              :key="a.cid"
              class="dh-asset mono"
              :class="{ downloading: downloadingCid[a.cid] }"
              :disabled="downloadingCid[a.cid]"
              @click.stop="downloadReleaseAsset(a)"
              :data-testid="`dev-feed-asset-${a.cid}`"
              :title="`Download «${a.name}» (${fmtSize(a.size)})`"
            >
              <span class="ic">⬇</span>
              <span class="nm">{{ a.name }}</span>
              <span class="sz dim">{{ fmtSize(a.size) }}</span>
            </button>
          </div>
        </div>
      </div>
      <p v-else class="dim" data-testid="dev-feed-empty">
        No pushes yet from authors you follow — subscribe to developers on their profile pages.
      </p>
    </section>

    <!-- мои репозитории -->
    <section v-if="myUsername" class="dh-block" data-testid="dev-my-repos">
      <h2 class="dh-h2">My repositories <span class="mono dim">@{{ myUsername }}</span></h2>
      <div v-if="myRepos.length" class="dh-grid">
        <button
          v-for="rp in myRepos"
          :key="rp.name"
          class="dh-card"
          @click="openRepo(myUsername, rp.name)"
          :data-testid="`dev-myrepo-${rp.name}`"
        >
          <span class="dh-card-name mono">{{ rp.name }}</span>
          <span class="dh-card-desc dim">{{ rp.description }}</span>
          <span class="dh-card-meta">
            <span v-if="rp.language" class="lang">{{ rp.language }} · </span>★ {{ rp.stars }} · <span class="seed">◉ {{ rp.seeders }}</span>
          </span>
        </button>
      </div>
      <div v-else class="dh-empty" data-testid="dev-my-repos-empty">
        <p class="dim">You don't have any repositories yet — create your first one and share it with the network.</p>
        <button class="dh-new" @click="router.push({ name: 'dev-new' })" data-testid="dev-dashboard-empty-create">
          Create repository
        </button>
      </div>
    </section>

    <!-- проекты сети -->
    <section class="dh-block" data-testid="dev-popular">
      <h2 class="dh-h2">Network projects</h2>
      <div v-if="popular.length" class="dh-list">
        <button
          v-for="r in popular"
          :key="r.username + '/' + r.repo"
          class="dh-row"
          @click="openRepo(r.username, r.repo)"
          :data-testid="`dev-popular-${r.username}-${r.repo}`"
        >
          <span class="dh-repo mono">
            <UserAvatar :address="r.address" :size="18" class="dh-uav" />
            <span class="at" @click.stop="openUser(r.username)">@{{ r.username }}</span>/{{ r.repo }}
          </span>
          <span class="dh-desc dim">{{ r.description }}</span>
          <span class="dh-meta">★ {{ r.stars }} · <span class="seed">◉ {{ r.seeders }}</span></span>
        </button>
      </div>
      <p v-else class="dim" data-testid="dev-popular-empty">
        No published projects found in the network yet. P2P index exchange will come online in the next update.
      </p>
    </section>
  </div>
</template>

<style scoped>
.dev-hub { padding: 28px 32px; max-width: 1100px; margin: 0 auto; height: 100%; overflow-y: auto; }
.dh-head { margin-bottom: 20px; }
.dh-head-row { display: flex; align-items: flex-start; justify-content: space-between; gap: 18px; flex-wrap: wrap; }
.dh-head-actions { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
.dh-refresh {
  padding: 10px 16px; border-radius: 10px; font-size: 13px; font-weight: var(--fw-700);
  color: var(--accent); background: transparent; border: 1px solid var(--border-accent);
  cursor: pointer; white-space: nowrap; transition: background-color 0.15s ease;
}
.dh-refresh:hover:not(:disabled) { background: rgba(255,255,255,0.05); }
.dh-refresh:disabled { opacity: 0.5; cursor: wait; }
.dh-new {
  padding: 10px 18px; border-radius: 10px; font-size: 13px; font-weight: var(--fw-800);
  color: #04140f; background: #34d399; border: none; cursor: pointer; white-space: nowrap;
  transition: transform 0.15s ease;
}
.dh-new:hover { transform: translateY(-1px); }
.dh-empty { display: flex; flex-direction: column; align-items: flex-start; gap: 12px; padding: 24px; border-radius: 12px; border: 1px dashed var(--border); }
.dh-title { font-size: 26px; font-weight: var(--fw-800); letter-spacing: 0.02em; }
.dh-title .mono { color: var(--accent); }
.dh-sub { font-size: 13px; margin-top: 6px; }
.mono { font-family: var(--font-mono, monospace); }
.dh-search {
  display: flex; align-items: center; gap: 9px;
  padding: 11px 14px; border-radius: 999px;
  background: var(--bg-elevated); border: 1px solid var(--border); color: var(--text-dim);
  margin-bottom: 26px;
}
.dh-input { flex: 1; background: transparent; border: none; outline: none; color: var(--text); font-size: 14px; }
.dh-load { font-size: 18px; }
.dh-block { margin-bottom: 28px; }
.dh-h2 { font-size: 12px; letter-spacing: 0.1em; text-transform: uppercase; color: var(--secondary); font-weight: var(--fw-700); margin-bottom: 12px; }
.dh-list { display: flex; flex-direction: column; gap: 8px; }
.dh-feed-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 12px; }
.dh-feed-head .dh-h2 { margin-bottom: 0; }
.dh-feed-toggle { display: inline-flex; gap: 4px; padding: 3px; border-radius: 999px; border: 1px solid var(--border); background: rgba(255,255,255,0.02); }
.dh-ftab {
  padding: 5px 14px; border-radius: 999px; font-size: 11px; font-weight: var(--fw-700);
  letter-spacing: 0.04em; color: var(--text-dim); background: transparent; border: none;
  cursor: pointer; transition: background-color 0.15s ease, color 0.15s ease;
}
.dh-ftab.on { color: var(--accent); background: rgba(255,255,255,0.06); }
.dh-fol { color: #fbbf24; font-size: 11px; }
.dh-row {
  display: grid; grid-template-columns: 1fr auto; grid-template-areas: "repo meta" "desc meta";
  gap: 2px 16px; text-align: left; padding: 12px 16px; border-radius: 12px;
  background: rgba(255,255,255,0.02); border: 1px solid var(--border); cursor: pointer;
  transition: border-color 0.15s ease, transform 0.15s ease, background-color 0.15s ease;
}
.dh-row:hover { border-color: var(--border-accent); transform: translateX(2px); background: rgba(255,255,255,0.04); }
.dh-repo { grid-area: repo; font-size: 13px; font-weight: var(--fw-700); color: var(--accent); display: inline-flex; align-items: center; gap: 7px; }
.dh-uav { vertical-align: middle; }
.dh-repo .at { color: var(--secondary); cursor: pointer; }
.dh-repo .at:hover { text-decoration: underline; }
.dh-desc { grid-area: desc; font-size: 12px; }
.dh-meta { grid-area: meta; align-self: center; font-size: 12px; white-space: nowrap; }
.seed { color: #34d399; }
.lang { color: var(--secondary); }
.dh-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 12px; }
.dh-card {
  display: flex; flex-direction: column; gap: 6px; text-align: left;
  padding: 14px 16px; border-radius: 12px; background: rgba(255,255,255,0.02);
  border: 1px solid var(--border); border-left: 2px solid var(--accent); cursor: pointer;
  transition: transform 0.15s ease, box-shadow 0.15s ease;
}
.dh-card:hover { transform: translateY(-2px); box-shadow: 0 8px 24px -12px rgba(0,0,0,0.7); }
.dh-card-name { font-size: 14px; font-weight: var(--fw-800); }
.dh-card-desc { font-size: 11px; line-height: 1.5; }
.dh-card-meta { font-size: 11px; margin-top: 2px; }

/* ---- лента: бейдж тегированного релиза + прямые ассеты ---- */
.dh-feed-item { display: flex; flex-direction: column; gap: 6px; }
.dh-tag {
  display: inline-flex; align-items: center; gap: 4px;
  padding: 2px 8px; margin-left: 4px;
  font-size: 10px; font-weight: var(--fw-800); letter-spacing: 0.04em;
  color: #fbbf24; background: rgba(251, 191, 36, 0.08);
  border: 1px solid rgba(251, 191, 36, 0.35); border-radius: 999px;
  cursor: pointer; transition: background-color 0.15s ease, transform 0.15s ease;
}
.dh-tag::before { content: 'v'; opacity: 0.7; margin-right: -2px; }
.dh-tag:hover { background: rgba(251, 191, 36, 0.16); transform: translateY(-1px); }
.dh-assets {
  display: flex; flex-wrap: wrap; gap: 8px;
  padding: 0 8px 4px 44px; /* отступ под avatar-репо в родительской строке */
}
.dh-asset {
  display: inline-flex; align-items: center; gap: 8px;
  padding: 6px 12px; font-size: 11px; font-weight: var(--fw-700);
  color: var(--text); background: rgba(52, 211, 153, 0.06);
  border: 1px solid rgba(52, 211, 153, 0.28); border-radius: 8px;
  cursor: pointer; transition: background-color 0.15s ease, border-color 0.15s ease;
}
.dh-asset:hover:not(:disabled) { background: rgba(52, 211, 153, 0.14); border-color: #34d399; }
.dh-asset:disabled, .dh-asset.downloading { opacity: 0.55; cursor: wait; }
.dh-asset .ic { color: #34d399; font-size: 13px; }
.dh-asset .nm { max-width: 220px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dh-asset .sz { font-size: 10px; }
</style>
