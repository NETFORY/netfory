<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount, computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { useAuthStore } from '@/store/auth'
import { useAppsStore } from '@/store/apps'
import { useTabsStore } from '@/store/tabs'
import { useDiscoveryStore } from '@/store/discovery'
import { dhtResolveApp } from '@/lib/bridge'
import { shortAddr } from '@/lib/crypto'

const route = useRoute()
const router = useRouter()
const { t } = useI18n()
const auth = useAuthStore()
const apps = useAppsStore()
const tabs = useTabsStore()
const discovery = useDiscoveryStore()

const id = computed(() => String(route.params.id || ''))
// v1.63.6: deep-link sub-path (напр. `trading/PAIR`), пришёл как ?p= query
// от `openAddress`/`onSthOpen`. Передаётся в tabs.open, чтобы после
// установки dApp открылся сразу на конкретной странице, а не на корне.
const deepPath = computed(() => {
  const raw = route.query.p
  if (typeof raw !== 'string') return ''
  return raw
})
const state = ref<'searching' | 'dht' | 'found' | 'notfound'>('searching')
let timer: ReturnType<typeof setInterval> | null = null
let deadline = 0

const found = computed(() => discovery.apps.find((a) => a.id === id.value))
// v1.129: прямая установка — провайдеры (nodeId) + хэш tar-блоба. Источник:
// n/h-параметры прямой ссылки ИЛИ трекерлесс-резолв через Mainline DHT.
const direct = ref<{ providers: string[]; hash: string } | null>(null)
let dhtTried = false

onMounted(async () => {
  // already installed? open it straight away
  if (apps.apps.some((m) => m.id === id.value)) {
    tabs.open(id.value, id.value, deepPath.value || undefined)
    router.replace({ name: 'dashboard' })
    return
  }
  // sth://<id>?n=<nodeId>&h=<hash> — самодостаточная ссылка: ставим сразу,
  // трекеры/сидеры/каталог не нужны (нода издателя должна быть онлайн).
  const qn = typeof route.query.n === 'string' ? route.query.n : ''
  const qh = typeof route.query.h === 'string' ? route.query.h : ''
  if (qn && qh) {
    direct.value = { providers: [qn], hash: qh }
    state.value = 'found'
  }
  if (auth.isTauri) await discovery.scan()
  if (direct.value) return
  deadline = Date.now() + 9000
  check()
  timer = setInterval(check, 1000)
})
onBeforeUnmount(() => {
  if (timer) clearInterval(timer)
})

function check() {
  if (found.value) {
    state.value = 'found'
    if (timer) clearInterval(timer)
  } else if (Date.now() > deadline || !auth.isTauri) {
    if (timer) clearInterval(timer)
    if (auth.isTauri && !dhtTried) {
      void tryDht()
    } else {
      state.value = 'notfound'
    }
  }
}

// Каталог (mDNS/трекеры/облака) молчит → трекерлесс-фолбэк: Mainline DHT +
// LAN-биконы. v2-ответ бикона несёт хэш tar-блоба — этого хватает для установки.
async function tryDht() {
  dhtTried = true
  state.value = 'dht'
  const r = await dhtResolveApp(id.value)
  if (r && r.providers.length && r.hash) {
    direct.value = { providers: r.providers, hash: r.hash }
    state.value = 'found'
  } else {
    state.value = 'notfound'
  }
}

async function install() {
  let ok = false
  if (found.value) ok = await discovery.install(id.value)
  if (!ok && direct.value) {
    ok = await discovery.installDirect(id.value, direct.value.providers, direct.value.hash)
  }
  if (ok) {
    tabs.open(id.value, found.value?.title || id.value, deepPath.value || undefined)
    router.replace({ name: 'dashboard' })
  }
}
</script>

<template>
  <div class="resolve" data-testid="resolve-view">
    <!-- searching -->
    <div v-if="state === 'searching'" class="center" data-testid="resolve-searching">
      <span class="spinner">⟳</span>
      <h2 class="r-title">{{ t('resolve.searching') }}</h2>
      <code class="r-url">sth://{{ shortAddr(id, 12, 10) }}</code>
      <p class="dim r-note">{{ t('resolve.searchingNote') }}</p>
    </div>

    <!-- trackerless DHT fallback search -->
    <div v-else-if="state === 'dht'" class="center" data-testid="resolve-dht">
      <span class="spinner">⟳</span>
      <h2 class="r-title">{{ t('resolve.dhtSearching') }}</h2>
      <code class="r-url">sth://{{ shortAddr(id, 12, 10) }}</code>
      <p class="dim r-note">{{ t('resolve.dhtNote') }}</p>
    </div>

    <!-- found -> install -->
    <div v-else-if="state === 'found' && (found || direct)" class="center" data-testid="resolve-found">
      <span class="r-mark hl">⊕</span>
      <h2 class="r-title">{{ found ? (found.title || t('discover.noTitle')) : shortAddr(id, 10, 8) }}</h2>
      <p class="r-desc">{{ found ? (found.description || t('discover.noDescription')) : t('resolve.directNote') }}</p>
      <div v-if="found && found.tags.length" class="r-tags">
        <span v-for="tg in found.tags" :key="tg" class="card-tag">#{{ tg }}</span>
      </div>
      <code class="r-url">sth://{{ shortAddr(id, 12, 10) }}</code>
      <p v-if="found" class="dim r-seeds">● {{ found.seeds }} {{ t('discover.seeds') }}</p>
      <p v-else-if="direct" class="dim r-seeds" data-testid="resolve-direct-badge">
        ⚡ {{ t('resolve.directBadge') }} · {{ direct.providers.length }} node
      </p>

      <div v-if="discovery.installingId === id" class="downloading">
        <span class="spinner">⟳</span> {{ t('discover.downloading') }}
      </div>
      <button
        v-else
        class="btn btn-primary r-install"
        @click="install"
        data-testid="resolve-install-btn"
      >
        {{ t('resolve.install') }} ↓
      </button>
      <p v-if="discovery.lastError" class="r-err" data-testid="resolve-error">⚠ {{ discovery.lastError }}</p>
    </div>

    <!-- not found -->
    <div v-else class="center" data-testid="resolve-notfound">
      <span class="r-mark dim">⌀</span>
      <h2 class="r-title">{{ t('resolve.notFound') }}</h2>
      <code class="r-url">sth://{{ shortAddr(id, 12, 10) }}</code>
      <p class="dim r-note">{{ t('resolve.notFoundNote') }}</p>
      <button class="btn" @click="router.replace({ name: 'dashboard' })" data-testid="resolve-home-btn">
        ← {{ t('nav.dashboard') }}
      </button>
    </div>
  </div>
</template>

<style scoped>
.resolve {
  min-height: 60vh;
  display: flex;
  align-items: center;
  justify-content: center;
}
.center {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 14px;
  text-align: center;
  max-width: 460px;
}
.spinner {
  font-size: 34px;
  color: var(--accent);
  display: inline-block;
  animation: r-spin 1s linear infinite;
}
@keyframes r-spin {
  to {
    transform: rotate(360deg);
  }
}
.r-mark {
  font-size: 42px;
}
.r-title {
  font-size: 18px;
  letter-spacing: 0.04em;
  color: var(--accent);
}
.r-desc {
  font-size: 13px;
  color: var(--text);
  line-height: 1.6;
}
.r-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  justify-content: center;
}
.card-tag {
  font-size: 10px;
  padding: 2px 7px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--text-dim);
}
.r-url {
  font-size: 12px;
  padding: 6px 10px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  word-break: break-all;
}
.r-note {
  font-size: 12px;
  line-height: 1.6;
}
.r-seeds {
  font-size: 11px;
}
.r-install {
  margin-top: 6px;
  padding: 12px 24px;
}
.downloading {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 12px;
  color: var(--accent);
}
.downloading .spinner {
  font-size: 16px;
}
.r-err {
  font-size: 12px;
  color: var(--danger);
  word-break: break-all;
}
</style>
