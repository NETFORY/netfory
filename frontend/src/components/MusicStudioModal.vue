<script setup lang="ts">
/* eslint-disable no-undef */
// «Студия музыканта» — модалка публикации бесплатного альбома (iroh-коллекция):
// обложка + список mp3 + метаданные → album_create.
import { ref, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { pickImageFile, pickAudioFiles, readAudioMeta, albumCreate, albumUpdate, type Album } from '@/lib/bridge'
import { useToastsStore } from '@/store/toasts'

const props = defineProps<{ edit?: Album | null }>()
const emit = defineEmits<{ (e: 'close'): void; (e: 'created', album: Album): void }>()
const { t } = useI18n()
const toasts = useToastsStore()

const isEdit = computed(() => !!props.edit)
const title = ref(props.edit?.title || '')
const artist = ref(props.edit?.artist || '')
const coverPath = ref('')
const coverName = ref(props.edit?.cover ? t('music.currentCover') : '')
const tracks = ref<{ path: string; title: string }[]>([])
const publishing = ref(false)
const isPaid = ref(!!props.edit && !props.edit.free)
const price = ref<number | null>(props.edit?.price ? props.edit.price : null)
// Бесплатные ~30-сек превью платных треков (галочка автора).
const withPreview = ref(true)
const year = ref<number | null>(props.edit?.year ? props.edit.year : null)
const note = ref(props.edit?.note || '')

function fileBase(p: string): string {
  const name = p.split(/[\\/]/).pop() || p
  return name.replace(/\.[^.]+$/, '')
}

async function chooseCover() {
  const p = await pickImageFile()
  if (p) {
    coverPath.value = p
    coverName.value = p.split(/[\\/]/).pop() || p
  }
}

async function addTracks() {
  const paths = await pickAudioFiles()
  const fresh = paths.filter((p) => !tracks.value.some((x) => x.path === p))
  if (!fresh.length) return
  const meta = await readAudioMeta(fresh)
  fresh.forEach((p, i) => {
    const m = meta[i]
    tracks.value.push({ path: p, title: (m?.title || '').trim() || fileBase(p) })
    // если поле исполнителя пустое — подставим из ID3 первого трека
    if (!artist.value.trim() && m?.artist) artist.value = m.artist.trim()
  })
}

function removeTrack(i: number) {
  tracks.value.splice(i, 1)
}

async function publish() {
  if (!title.value.trim()) {
    toasts.push(t('music.errNoTitle'), '', 'error')
    return
  }
  if (!tracks.value.length && !isEdit.value) {
    toasts.push(t('music.errNoTracks'), '', 'error')
    return
  }
  const finalPrice = isPaid.value ? Number(price.value) || 0 : 0
  if (isPaid.value && finalPrice <= 0) {
    toasts.push(t('music.errNoPrice'), '', 'error')
    return
  }
  publishing.value = true
  try {
    if (isEdit.value && props.edit) {
      const album = await albumUpdate(
        props.edit.id,
        title.value,
        artist.value,
        coverPath.value,
        finalPrice,
        Number(year.value) || 0,
        note.value,
      )
      toasts.push(t('music.updated'), '', 'info')
      emit('created', album)
      emit('close')
      return
    }
    const album = await albumCreate(
      title.value,
      artist.value,
      coverPath.value,
      tracks.value.map((x) => x.path),
      tracks.value.map((x) => x.title),
      finalPrice,
      Number(year.value) || 0,
      note.value,
      isPaid.value && withPreview.value,
    )
    toasts.push(t('music.published'), '', 'info')
    emit('created', album)
    emit('close')
  } catch (e: any) {
    toasts.push(t('music.errPublish'), `${e?.message || e}`, 'error')
  } finally {
    publishing.value = false
  }
}
</script>

<template>
  <div class="studio-backdrop" @click.self="emit('close')" data-testid="music-studio-modal">
    <div class="studio">
      <div class="studio-head">
        <span class="studio-title">🎵 {{ isEdit ? t('music.editAlbum') : t('music.studio') }}</span>
        <button class="studio-x" @click="emit('close')" data-testid="studio-close">✕</button>
      </div>

      <div class="studio-body">
        <div class="row">
          <button class="cover-pick" @click="chooseCover" data-testid="studio-pick-cover">
            <span v-if="!coverName" class="cp-plus">＋</span>
            <span v-else class="cp-ok">✓</span>
            <span class="cp-lbl">{{ coverName || t('music.pickCover') }}</span>
          </button>
        </div>

        <label class="fld">
          <span class="fld-lbl">{{ t('music.albumTitle') }} *</span>
          <input v-model="title" class="fld-in" :placeholder="t('music.albumTitle')" data-testid="studio-title" />
        </label>
        <label class="fld">
          <span class="fld-lbl">{{ t('music.artist') }}</span>
          <input v-model="artist" class="fld-in" :placeholder="t('music.artist')" data-testid="studio-artist" />
        </label>

        <label class="fld">
          <span class="fld-lbl">{{ t('music.year') }}</span>
          <input
            v-model.number="year"
            type="number"
            min="1900"
            max="2100"
            step="1"
            class="fld-in"
            :placeholder="t('music.yearPlaceholder')"
            data-testid="studio-year"
          />
        </label>

        <label class="fld">
          <span class="fld-lbl">{{ t('music.note') }}</span>
          <textarea
            v-model="note"
            class="fld-in note-in"
            rows="3"
            maxlength="2000"
            :placeholder="t('music.notePlaceholder')"
            data-testid="studio-note"
          />
        </label>

        <div class="pay-row">
          <label class="pay-toggle">
            <input type="checkbox" v-model="isPaid" data-testid="studio-paid-toggle" />
            <span>{{ t('music.paidAlbum') }}</span>
          </label>
          <input
            v-if="isPaid"
            v-model.number="price"
            type="number"
            min="1"
            step="1"
            class="fld-in price-in"
            :placeholder="t('music.priceSth')"
            data-testid="studio-price"
          />
          <label v-if="isPaid && !isEdit" class="pay-toggle" title="Бесплатный ~30-сек фрагмент каждого трека для всех слушателей">
            <input type="checkbox" v-model="withPreview" data-testid="studio-preview-toggle" />
            <span>30-сек превью</span>
          </label>
        </div>

        <div class="tracks-head">
          <span class="fld-lbl">{{ t('music.tracks') }} ({{ isEdit ? props.edit?.tracks.length || 0 : tracks.length }})</span>
          <button v-if="!isEdit" class="add-tracks" @click="addTracks" data-testid="studio-add-tracks">＋ {{ t('music.addTracks') }}</button>
        </div>
        <p v-if="isEdit" class="tracks-empty">{{ t('music.tracksLocked') }}</p>
        <ul v-else-if="tracks.length" class="track-rows">
          <li v-for="(tk, i) in tracks" :key="tk.path" class="track-row" :data-testid="`studio-track-${i}`">
            <span class="tr-n">{{ i + 1 }}</span>
            <input v-model="tk.title" class="tr-in" :data-testid="`studio-track-title-${i}`" />
            <button class="tr-del" @click="removeTrack(i)" :data-testid="`studio-track-remove-${i}`">✕</button>
          </li>
        </ul>
        <p v-else class="tracks-empty">{{ t('music.noTracksYet') }}</p>
      </div>

      <div class="studio-foot">
        <button class="btn ghost" @click="emit('close')" :disabled="publishing">{{ t('music.cancel') }}</button>
        <button class="btn primary" @click="publish" :disabled="publishing" data-testid="studio-publish">
          {{ publishing ? (isEdit ? t('music.saving') : t('music.publishing')) : (isEdit ? t('music.save') : t('music.publish')) }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.studio-backdrop {
  position: fixed;
  inset: 0;
  z-index: 100;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(4, 8, 16, 0.72);
  backdrop-filter: blur(6px);
  -webkit-backdrop-filter: blur(6px);
}
.studio {
  width: 520px;
  max-width: 94vw;
  max-height: 88vh;
  display: flex;
  flex-direction: column;
  background: rgba(12, 17, 28, 0.98);
  border: 1px solid rgba(0, 229, 255, 0.3);
  border-radius: 14px;
  box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6);
}
.studio-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px 18px;
  border-bottom: 1px solid rgba(0, 229, 255, 0.16);
}
.studio-title {
  font-size: 14px;
  font-weight: 700;
  letter-spacing: 0.06em;
  color: #eaf6ff;
}
.studio-x {
  background: none;
  border: none;
  color: #7fb4cf;
  cursor: pointer;
  font-size: 15px;
}
.studio-x:hover {
  color: #ff6b6b;
}
.studio-body {
  padding: 18px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.cover-pick {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 12px 14px;
  border: 1px dashed rgba(0, 229, 255, 0.35);
  border-radius: 10px;
  background: rgba(0, 229, 255, 0.04);
  color: #b9d6e6;
  cursor: pointer;
  font-size: 12px;
  transition: border-color 0.15s ease;
}
.cover-pick:hover {
  border-color: #00e5ff;
}
.cp-plus,
.cp-ok {
  font-size: 16px;
  color: #00e5ff;
}
.fld {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.fld-lbl {
  font-size: 11px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: #7fb4cf;
}
.fld-in,
.tr-in {
  padding: 9px 11px;
  border-radius: 8px;
  border: 1px solid rgba(255, 255, 255, 0.12);
  background: rgba(255, 255, 255, 0.04);
  color: #eaf6ff;
  font-size: 13px;
  outline: none;
}
.fld-in:focus,
.tr-in:focus {
  border-color: #00e5ff;
}
.tracks-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.pay-row {
  display: flex;
  align-items: center;
  gap: 12px;
}
.pay-toggle {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: #b9d6e6;
  cursor: pointer;
}
.pay-toggle input {
  accent-color: #00e5ff;
}
.price-in {
  width: 140px;
}
.note-in {
  resize: vertical;
  min-height: 60px;
  font-family: inherit;
  line-height: 1.5;
}
.add-tracks {
  padding: 6px 12px;
  font-size: 11px;
  border-radius: 999px;
  border: 1px solid rgba(0, 229, 255, 0.3);
  background: transparent;
  color: #00e5ff;
  cursor: pointer;
}
.add-tracks:hover {
  background: rgba(0, 229, 255, 0.1);
}
.track-rows {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.track-row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.tr-n {
  width: 20px;
  font-size: 11px;
  color: #5f7d90;
  text-align: right;
}
.tr-in {
  flex: 1;
}
.tr-del {
  background: none;
  border: none;
  color: #5f7d90;
  cursor: pointer;
  font-size: 12px;
}
.tr-del:hover {
  color: #ff6b6b;
}
.tracks-empty {
  margin: 0;
  font-size: 12px;
  color: #6f96ad;
  text-align: center;
  padding: 10px;
}
.studio-foot {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  padding: 14px 18px;
  border-top: 1px solid rgba(0, 229, 255, 0.16);
}
.btn {
  padding: 9px 18px;
  font-size: 12px;
  border-radius: 999px;
  cursor: pointer;
  border: 1px solid transparent;
  transition: all 0.15s ease;
}
.btn.ghost {
  background: transparent;
  border-color: rgba(255, 255, 255, 0.16);
  color: #b9d6e6;
}
.btn.ghost:hover {
  border-color: #7fb4cf;
}
.btn.primary {
  background: linear-gradient(135deg, #00e5ff, #a855f7);
  color: #06121e;
  font-weight: 700;
}
.btn.primary:disabled {
  opacity: 0.6;
  cursor: default;
}
</style>
