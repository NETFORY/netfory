<script setup lang="ts">
// Глобальная страница «Музыка»: лента всех известных локально альбомов
// (свои + полученные) + кнопка «Студия» для публикации бесплатного альбома.
import { ref, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { albumFeed, albumOpenLink, type Album } from '@/lib/bridge'
import { useAuthStore } from '@/store/auth'
import { useRouter } from 'vue-router'
import { useToastsStore } from '@/store/toasts'
import AlbumCard from '@/components/AlbumCard.vue'
import MusicStudioModal from '@/components/MusicStudioModal.vue'

const { t } = useI18n()
const auth = useAuthStore()
const router = useRouter()
const toasts = useToastsStore()

const albums = ref<Album[]>([])
const loading = ref(true)
const showStudio = ref(false)
const linkInput = ref('')
const opening = ref(false)

async function openLink() {
  if (!linkInput.value.trim()) return
  opening.value = true
  try {
    const id = await albumOpenLink(linkInput.value.trim())
    linkInput.value = ''
    router.push({ name: 'album', params: { id } })
  } catch (e: any) {
    toasts.push(t('music.openFailed'), `${e?.message || e}`, 'error')
  } finally {
    opening.value = false
  }
}

async function load() {
  loading.value = true
  albums.value = await albumFeed()
  loading.value = false
}

function onCreated(a: Album) {
  albums.value = [a, ...albums.value.filter((x) => x.id !== a.id)]
}

onMounted(load)
</script>

<template>
  <div class="music-page" data-testid="music-page">
    <header class="mp-head">
      <div>
        <h1 class="mp-title">♫ {{ t('nav.music') }}</h1>
        <p class="mp-sub">{{ t('music.pageSub') }}</p>
      </div>
      <button v-if="auth.address" class="mp-studio" @click="showStudio = true" data-testid="open-studio-global">
        🎛 {{ t('music.studio') }}
      </button>
    </header>

    <div class="mp-openlink">
      <input
        v-model="linkInput"
        class="mp-link-in"
        :placeholder="t('music.openPlaceholder')"
        @keyup.enter="openLink"
        data-testid="music-open-link-input"
      />
      <button class="mp-link-btn" @click="openLink" :disabled="opening" data-testid="music-open-link-btn">
        {{ opening ? t('music.opening') : t('music.open') }}
      </button>
    </div>

    <p v-if="loading" class="mp-empty">{{ t('music.loading') }}</p>
    <p v-else-if="!albums.length" class="mp-empty" data-testid="music-empty">{{ t('music.emptyFeed') }}</p>

    <div v-else class="mp-grid" data-testid="music-grid">
      <AlbumCard v-for="a in albums" :key="a.id" :album="a" />
    </div>

    <MusicStudioModal v-if="showStudio" @close="showStudio = false" @created="onCreated" />
  </div>
</template>

<style scoped>
.music-page {
  padding: 28px 32px;
  max-width: 1440px;
  margin: 0 auto;
  height: 100%;
  overflow-y: auto;
}
.mp-head {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 26px;
}
.mp-title {
  font-size: 26px;
  font-weight: 800;
  color: var(--text);
  margin: 0;
  letter-spacing: 0.02em;
}
.mp-sub {
  margin: 6px 0 0;
  font-size: 13px;
  color: var(--text-dim);
}
.mp-studio {
  padding: 11px 20px;
  border-radius: 999px;
  border: none;
  background: linear-gradient(135deg, light-dark(#0891b2, #00e5ff), light-dark(#7e22ce, #a855f7));
  color: light-dark(#ffffff, #06121e);
  font-weight: 700;
  font-size: 12px;
  cursor: pointer;
  white-space: nowrap;
  box-shadow: 0 0 18px rgba(0, 229, 255, 0.35);
  transition: transform 0.15s ease;
}
.mp-studio:hover {
  transform: translateY(-1px);
}
.mp-empty {
  color: var(--text-dim);
  font-size: 13px;
  text-align: center;
  padding: 60px 0;
}
.mp-openlink {
  display: flex;
  gap: 10px;
  margin-bottom: 24px;
}
.mp-link-in {
  flex: 1;
  padding: 10px 14px;
  border-radius: 10px;
  border: 1px solid light-dark(rgba(0, 0, 0, 0.15), rgba(255, 255, 255, 0.12));
  background: light-dark(rgba(0, 0, 0, 0.04), rgba(255, 255, 255, 0.04));
  color: var(--text);
  font-size: 13px;
  outline: none;
}
.mp-link-in:focus {
  border-color: light-dark(#0891b2, #00e5ff);
}
.mp-link-btn {
  padding: 10px 20px;
  border-radius: 10px;
  border: 1px solid rgba(0, 229, 255, 0.3);
  background: transparent;
  color: light-dark(#0891b2, #00e5ff);
  font-size: 12px;
  cursor: pointer;
  white-space: nowrap;
}
.mp-link-btn:hover {
  background: rgba(0, 229, 255, 0.1);
}
.mp-link-btn:disabled {
  opacity: 0.6;
  cursor: default;
}
.mp-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(420px, 1fr));
  gap: 16px;
}
</style>
