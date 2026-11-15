<script setup lang="ts">
// Профиль разработчика — аналог страницы автора на GitHub: последние
// репозитории (не все сразу), фильтр, кнопка «Новый» на своём профиле.
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import Identicon from '@/components/Identicon.vue'
import UserAvatar from '@/components/UserAvatar.vue'
import {
  appListen,
  devhubActivity,
  devhubDayEvents,
  getMyUsername,
  getUserRepositories,
  isTauri,
  type DevEventInfo,
  type DevRepository,
  type DevUser,
} from '@/lib/bridge'
import { shortAddr } from '@/lib/crypto'

const route = useRoute()
const router = useRouter()
const user = ref<DevUser | null>(null)
const loading = ref(true)
const error = ref('')
const myUsername = ref('')
const filter = ref('')
const showAll = ref(false)

const RECENT_LIMIT = 6

const isOwn = computed(() => !!user.value && user.value.username === myUsername.value)

const sorted = computed<DevRepository[]>(() => {
  if (!user.value) return []
  return [...user.value.repositories].sort((a, b) => Number(b.updatedAt) - Number(a.updatedAt))
})
const filtered = computed(() => {
  const q = filter.value.trim().toLowerCase()
  if (!q) return sorted.value
  return sorted.value.filter((r) => r.name.toLowerCase().includes(q) || r.description.toLowerCase().includes(q))
})
const visible = computed(() => (showAll.value || filter.value ? filtered.value : filtered.value.slice(0, RECENT_LIMIT)))
const hasMore = computed(() => !showAll.value && !filter.value && filtered.value.length > RECENT_LIMIT)

function timeAgo(secsStr: string): string {
  const secs = Number(secsStr) || 0
  const d = Date.now() / 1000 - secs
  if (d < 60) return 'just now'
  if (d < 3600) return `${Math.floor(d / 60)} min ago`
  if (d < 86400) return `${Math.floor(d / 3600)} h ago`
  return `${Math.floor(d / 86400)} d ago`
}

// ---- зелёный граф контрибуций (по подписанным seq-обновлениям индекса) ----
const activity = ref<Record<string, number>>({})
const totalContribs = computed(() => Object.values(activity.value).reduce((a, b) => a + b, 0))
const weeks = computed(() => {
  const end = new Date()
  end.setUTCHours(0, 0, 0, 0)
  const start = new Date(end.getTime() - 370 * 86400000)
  start.setTime(start.getTime() - ((start.getUTCDay() + 6) % 7) * 86400000) // к понедельнику
  const cols: { date: string; count: number; level: number }[][] = []
  let cur = new Date(start)
  while (cur <= end) {
    const col: { date: string; count: number; level: number }[] = []
    for (let i = 0; i < 7 && cur <= end; i++) {
      const k = cur.toISOString().slice(0, 10)
      const c = activity.value[k] || 0
      col.push({ date: k, count: c, level: c === 0 ? 0 : c < 3 ? 1 : c < 6 ? 2 : 3 })
      cur = new Date(cur.getTime() + 86400000)
    }
    cols.push(col)
  }
  return cols
})

// ---- клик по дню: список событий (какие репо обновлялись) ----
const selectedDay = ref('')
const dayEvents = ref<DevEventInfo[]>([])
const dayLoading = ref(false)
const ACTION_RU: Record<string, string> = {
  created: 'created',
  push: 'push',
  deleted: 'deleted',
  release: 'release',
}
async function pickDay(date: string, count: number) {
  if (selectedDay.value === date) {
    selectedDay.value = ''
    return
  }
  selectedDay.value = date
  dayEvents.value = []
  if (!count) return
  dayLoading.value = true
  try {
    dayEvents.value = await devhubDayEvents(String(route.params.username || ''), date)
  } catch {
    dayEvents.value = []
  } finally {
    dayLoading.value = false
  }
}

let unlistenSync: (() => void) | null = null
onBeforeUnmount(() => {
  if (unlistenSync) unlistenSync()
})

async function load() {
  loading.value = true
  error.value = ''
  const username = String(route.params.username || '')
  try {
    user.value = await getUserRepositories(username)
    devhubActivity(username)
      .then((a) => (activity.value = a))
      .catch(() => (activity.value = {}))
  } catch (e: any) {
    error.value =
      e?.kind === 'invalid_characters'
        ? 'Invalid username.'
        : e?.kind === 'not_found'
          ? `The name @${username} is not registered in the on-chain registry.`
          : 'Author not found in the network.'
    user.value = null
  } finally {
    loading.value = false
  }
}
onMounted(async () => {
  myUsername.value = (await getMyUsername()) || ''
  await load()
  if (isTauri) {
    // P2P-синк чужого индекса завершился в фоне → перезагружаем профиль.
    unlistenSync = await appListen<{ username: string }>('devhub-synced', (p) => {
      if (p.username === String(route.params.username || '')) load()
    })
  }
})
watch(() => route.params.username, load)

function openRepo(repo: string) {
  router.push({ name: 'dev-repo', params: { username: user!.value!.username, repo } })
}
</script>

<template>
  <div class="dev-user" data-testid="dev-user-profile">
    <button class="back" @click="router.push({ name: 'dev' })" data-testid="dev-back">‹ dev:// hub</button>

    <p v-if="loading" class="dim" data-testid="dev-user-loading">Resolving name via on-chain registry…</p>
    <p v-else-if="error" class="err" data-testid="dev-user-error">{{ error }}</p>

    <div v-else-if="user" class="du-layout">
      <!-- Левая колонка: карточка автора -->
      <aside class="du-aside" data-testid="dev-user-card">
        <UserAvatar v-if="user.address" :address="user.address" :size="96" />
        <Identicon v-else :seed="user.username" :size="96" />
        <h1 class="du-name mono">dev://{{ user.username }}</h1>
        <p v-if="user.bio" class="du-bio dim">{{ user.bio }}</p>
        <code class="du-addr" :title="user.address">{{ shortAddr(user.address, 10, 8) }}</code>
        <div class="du-stat-line">
          <span class="v">{{ user.repositories.length }}</span>
          <span class="dim">repositories</span>
        </div>
      </aside>

      <!-- Правая колонка: репозитории -->
      <main class="du-main">
        <div class="du-toolbar">
          <input
            v-model="filter"
            class="du-filter"
            type="text"
            placeholder="Find repository…"
            data-testid="dev-user-filter"
          />
          <button
            v-if="isOwn"
            class="du-new"
            @click="router.push({ name: 'dev-new' })"
            data-testid="dev-user-new-btn"
          >+ New</button>
        </div>

        <!-- зелёный граф контрибуций: подписанные seq-обновления индекса -->
        <section class="du-contrib" data-testid="dev-user-contrib">
          <h2 class="du-h2">{{ totalContribs }} contributions in the last year</h2>
          <div class="du-graph">
            <div v-for="(col, ci) in weeks" :key="ci" class="du-week">
              <span
                v-for="c in col"
                :key="c.date"
                class="du-day clickable"
                :class="[`lvl${c.level}`, { picked: selectedDay === c.date }]"
                :title="`${c.date}: ${c.count}`"
                @click="pickDay(c.date, c.count)"
                :data-testid="`dev-contrib-day-${c.date}`"
              />
            </div>
          </div>
          <!-- события выбранного дня -->
          <div v-if="selectedDay" class="du-day-events" data-testid="dev-contrib-day-events">
            <p class="du-day-title mono">{{ selectedDay }}</p>
            <p v-if="dayLoading" class="dim">Loading events…</p>
            <template v-else-if="dayEvents.length">
              <button
                v-for="(ev, i) in dayEvents"
                :key="i"
                class="du-event"
                @click="openRepo(ev.repo)"
                :data-testid="`dev-contrib-event-${i}`"
              >
                <span class="mono du-event-repo">{{ ev.repo }}</span>
                <span class="du-event-action" :class="ev.action">{{ ACTION_RU[ev.action] || ev.action }}</span>
                <span v-if="ev.msg" class="du-event-msg dim" :title="ev.msg">{{ ev.msg }}</span>
              </button>
            </template>
            <p v-else class="dim">No contributions on this day.</p>
          </div>
          <div class="du-legend dim">
            Less
            <span class="du-day lvl0" /><span class="du-day lvl1" /><span class="du-day lvl2" /><span class="du-day lvl3" />
            More
          </div>
        </section>

        <h2 class="du-h2">{{ filter ? 'Results' : 'Latest repositories' }}</h2>

        <div v-if="visible.length" class="du-list" data-testid="dev-user-repos">
          <button
            v-for="rp in visible"
            :key="rp.name"
            class="du-row"
            @click="openRepo(rp.name)"
            :data-testid="`dev-repo-card-${rp.name}`"
          >
            <span class="du-row-top">
              <span class="du-row-name mono">{{ rp.name }}</span>
              <span class="du-pub">Public</span>
            </span>
            <span v-if="rp.description" class="du-row-desc dim">{{ rp.description }}</span>
            <span class="du-row-meta dim">
              <span v-if="rp.language" class="lang"><span class="lang-dot" />{{ rp.language }}</span>
              <span v-if="rp.license">{{ rp.license }}</span>
              <span>updated {{ timeAgo(rp.updatedAt) }}</span>
              <span class="seed">◉ {{ rp.seeders }}</span>
            </span>
          </button>
        </div>

        <div v-else class="du-empty" data-testid="dev-user-empty">
          <p class="dim">{{ filter ? 'Nothing found.' : `@${user.username} has no repositories yet.` }}</p>
          <button v-if="isOwn && !filter" class="du-new" @click="router.push({ name: 'dev-new' })" data-testid="dev-user-empty-create">
            Create first repository
          </button>
        </div>

        <button v-if="hasMore" class="du-more" @click="showAll = true" data-testid="dev-user-show-all">
          All repositories ({{ filtered.length }})
        </button>
      </main>
    </div>
  </div>
</template>

<style scoped>
.dev-user { padding: 28px 32px; max-width: 1100px; margin: 0 auto; height: 100%; overflow-y: auto; }
.mono { font-family: var(--font-mono, monospace); }
.back { font-size: 12px; color: var(--text-dim); margin-bottom: 18px; cursor: pointer; background: none; border: none; }
.back:hover { color: var(--accent); }
.err { color: #f87171; }
.du-layout { display: grid; grid-template-columns: 260px 1fr; gap: 28px; align-items: start; }
@media (max-width: 860px) { .du-layout { grid-template-columns: 1fr; } }
.du-aside {
  display: flex; flex-direction: column; gap: 10px;
  padding: 20px; border-radius: 16px;
  background: rgba(255,255,255,0.02); border: 1px solid var(--border);
}
.du-name { font-size: 18px; font-weight: var(--fw-800); color: var(--accent); word-break: break-all; }
.du-bio { font-size: 13px; line-height: 1.5; }
.du-addr { font-size: 11px; color: var(--secondary); }
.du-stat-line { display: flex; gap: 6px; align-items: baseline; font-size: 13px; margin-top: 4px; }
.du-stat-line .v { font-size: 18px; font-weight: var(--fw-800); }
.du-toolbar { display: flex; gap: 10px; margin-bottom: 18px; }
.du-filter {
  flex: 1; padding: 9px 14px; border-radius: 10px; font-size: 13px;
  background: var(--bg-elevated); border: 1px solid var(--border); color: var(--text); outline: none;
}
.du-filter:focus { border-color: var(--border-accent); }
.du-new {
  padding: 9px 16px; border-radius: 10px; font-size: 13px; font-weight: var(--fw-800);
  color: #04140f; background: #34d399; border: none; cursor: pointer; white-space: nowrap;
  transition: transform 0.15s ease;
}
.du-new:hover { transform: translateY(-1px); }
.du-h2 { font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase; color: var(--secondary); font-weight: var(--fw-700); margin-bottom: 12px; }
.du-list { display: flex; flex-direction: column; gap: 10px; }
.du-row {
  display: flex; flex-direction: column; gap: 5px; text-align: left;
  padding: 14px 18px; border-radius: 12px; background: rgba(255,255,255,0.02);
  border: 1px solid var(--border); cursor: pointer;
  transition: border-color 0.15s ease, transform 0.15s ease;
}
.du-row:hover { border-color: var(--border-accent); transform: translateX(2px); }
.du-row-top { display: flex; align-items: center; gap: 10px; }
.du-row-name { font-size: 15px; font-weight: var(--fw-800); color: var(--accent); }
.du-pub { padding: 2px 10px; border-radius: 999px; font-size: 10px; color: #34d399; border: 1px solid rgba(52,211,153,0.4); }
.du-row-desc { font-size: 12px; line-height: 1.5; }
.du-row-meta { display: flex; flex-wrap: wrap; gap: 14px; font-size: 11px; margin-top: 2px; }
.lang { display: inline-flex; align-items: center; gap: 5px; color: var(--secondary); }
.lang-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--accent); display: inline-block; }
.seed { color: #34d399; }
.du-contrib {
  padding: 16px 18px; border-radius: 12px; margin-bottom: 20px;
  background: rgba(255,255,255,0.02); border: 1px solid var(--border);
  overflow-x: auto;
}
.du-graph { display: flex; gap: 3px; margin-top: 4px; }
.du-week { display: flex; flex-direction: column; gap: 3px; }
.du-day { width: 10px; height: 10px; border-radius: 2px; display: inline-block; flex: none; }
.du-day.lvl0 { background: rgba(255,255,255,0.06); }
.du-day.lvl1 { background: #0e4429; }
.du-day.lvl2 { background: #26a641; }
.du-day.lvl3 { background: #39d353; }
.du-day.clickable { cursor: pointer; transition: transform 0.1s ease; }
.du-day.clickable:hover { transform: scale(1.35); }
.du-day.picked { outline: 1px solid var(--accent); }
.du-day-events { margin-top: 12px; padding: 12px 14px; border-radius: 10px; border: 1px solid var(--border); background: rgba(255,255,255,0.02); display: flex; flex-direction: column; gap: 6px; }
.du-day-title { font-size: 12px; color: var(--secondary); margin-bottom: 2px; }
.du-event { display: flex; align-items: center; gap: 10px; padding: 6px 10px; border-radius: 8px; background: none; border: 1px solid transparent; cursor: pointer; text-align: left; transition: border-color 0.12s ease; }
.du-event:hover { border-color: var(--border-accent); }
.du-event-repo { font-size: 12px; color: var(--accent); }
.du-event-action { font-size: 10px; padding: 2px 9px; border-radius: 999px; border: 1px solid var(--border); color: var(--text-dim); }
.du-event-msg { font-size: 11px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 380px; }
.du-event-action.created { color: #34d399; border-color: rgba(52,211,153,0.4); }
.du-event-action.push { color: var(--accent); border-color: var(--border-accent); }
.du-event-action.deleted { color: #f87171; border-color: rgba(248,113,113,0.4); }
.du-event-action.release { color: #fbbf24; border-color: rgba(251,191,36,0.4); }
.du-legend { display: flex; align-items: center; gap: 4px; font-size: 10px; margin-top: 10px; justify-content: flex-end; }
.du-empty { padding: 32px; border-radius: 12px; border: 1px dashed var(--border); text-align: center; display: flex; flex-direction: column; gap: 14px; align-items: center; }
.du-more { margin-top: 14px; padding: 8px 16px; border-radius: 999px; font-size: 12px; color: var(--accent); background: transparent; border: 1px solid var(--border-accent); cursor: pointer; }
.du-more:hover { background: rgba(255,255,255,0.04); }
.dim { color: var(--text-dim); }
</style>
