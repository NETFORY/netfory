<script setup lang="ts">
/* eslint-disable no-undef */
// Плеер стриминга торрента: играет видео/аудио ВО ВРЕМЯ скачивания через
// локальный HTTP-стрим librqbit (Range-перемотка, куски приоритизируются
// вокруг позиции просмотра).
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { torrentListFiles, torrentStreamUrl, type TorrentFile } from '@/lib/bridge'

const { t } = useI18n()

const props = defineProps<{ infoHash: string; name: string }>()
const emit = defineEmits<{ (e: 'close'): void }>()

const MEDIA_EXT = ['mp4', 'm4v', 'webm', 'mkv', 'mov', 'avi', 'ts', 'mp3', 'm4a', 'aac', 'flac', 'ogg', 'oga', 'wav']
const AUDIO_EXT = ['mp3', 'm4a', 'aac', 'flac', 'ogg', 'oga', 'wav']

const files = ref<{ idx: number; file: TorrentFile }[]>([])
const err = ref('')
const src = ref('')
const currentName = ref('')
const isAudio = computed(() => AUDIO_EXT.includes((currentName.value.split('.').pop() || '').toLowerCase()))

function extOf(p: string): string {
  return (p.split('.').pop() || '').toLowerCase()
}
function fmtSize(b: number): string {
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(0)} ${t('torrents.kb')}`
  if (b < 1024 * 1024 * 1024) return `${(b / 1024 / 1024).toFixed(1)} ${t('torrents.mb')}`
  return `${(b / 1024 / 1024 / 1024).toFixed(2)} ${t('torrents.gb')}`
}

async function play(idx: number, file: TorrentFile) {
  try {
    src.value = await torrentStreamUrl(props.infoHash, idx)
    currentName.value = file.path
  } catch (e: any) {
    err.value = typeof e === 'string' ? e : e?.message || t('torrents.streamOpenFailed')
  }
}

onMounted(async () => {
  try {
    const all = await torrentListFiles(props.infoHash)
    files.value = all
      .map((file, idx) => ({ idx, file }))
      .filter((e) => MEDIA_EXT.includes(extOf(e.file.path)))
    if (!files.value.length) {
      err.value = t('torrents.streamNoMedia')
      return
    }
    // Один медиафайл — играем сразу; иначе самый большой по умолчанию.
    const biggest = [...files.value].sort((a, b) => b.file.size - a.file.size)[0]
    await play(biggest.idx, biggest.file)
  } catch (e: any) {
    err.value = typeof e === 'string' ? e : e?.message || t('torrents.streamMetaNotReady')
  }
})
</script>

<template>
  <div class="tsm-overlay" @click.self="emit('close')" data-testid="torrent-stream-modal">
    <div class="tsm">
      <div class="tsm-head">
        <span class="tsm-title mono" :title="name">▶ {{ name }}</span>
        <button class="tsm-close" @click="emit('close')" data-testid="torrent-stream-close">✕</button>
      </div>

      <p v-if="err" class="tsm-err" data-testid="torrent-stream-error">{{ err }}</p>

      <template v-else>
        <audio
          v-if="src && isAudio"
          :src="src"
          controls
          autoplay
          class="tsm-audio"
          data-testid="torrent-stream-audio"
        ></audio>
        <video
          v-else-if="src"
          :src="src"
          controls
          autoplay
          class="tsm-video"
          data-testid="torrent-stream-video"
        ></video>

        <div v-if="files.length > 1" class="tsm-files" data-testid="torrent-stream-files">
          <button
            v-for="e in files"
            :key="e.idx"
            class="tsm-file"
            :class="{ active: e.file.path === currentName }"
            @click="play(e.idx, e.file)"
            :data-testid="`torrent-stream-file-${e.idx}`"
          >
            <span class="mono tsm-file-name">{{ e.file.path }}</span>
            <span class="dim">{{ fmtSize(e.file.size) }}</span>
          </button>
        </div>
        <p class="tsm-hint dim">
          {{ t('torrents.streamHint') }}
        </p>
      </template>
    </div>
  </div>
</template>

<style scoped>
.tsm-overlay {
  position: fixed; inset: 0; z-index: 90; display: flex; align-items: center; justify-content: center;
  background: rgba(0, 0, 0, 0.72); backdrop-filter: blur(6px);
}
.tsm {
  width: min(880px, 94vw); max-height: 92vh; overflow-y: auto;
  background: var(--panel, #101418); border: 1px solid var(--border); border-radius: 14px; padding: 16px 18px;
}
.tsm-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 12px; }
.tsm-title { font-size: 13px; font-weight: var(--fw-700); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tsm-close { background: transparent; border: none; color: var(--secondary); font-size: 15px; cursor: pointer; }
.tsm-close:hover { color: var(--accent); }
.tsm-video { width: 100%; max-height: 62vh; border-radius: 10px; background: #000; }
.tsm-audio { width: 100%; }
.tsm-err { font-size: 13px; color: var(--danger, #e5534b); padding: 18px 4px; }
.tsm-files { display: flex; flex-direction: column; gap: 6px; margin-top: 12px; }
.tsm-file {
  display: flex; justify-content: space-between; gap: 12px; padding: 7px 12px; font-size: 12px;
  background: rgba(255, 255, 255, 0.02); border: 1px solid var(--border); border-radius: 8px;
  cursor: pointer; text-align: left; color: inherit; transition: border-color 0.12s ease;
}
.tsm-file:hover { border-color: var(--border-accent); }
.tsm-file.active { border-color: var(--accent); }
.tsm-file-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tsm-hint { font-size: 11px; margin-top: 10px; }
</style>
