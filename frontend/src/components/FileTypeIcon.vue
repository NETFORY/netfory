<script setup lang="ts">
/* eslint-disable no-undef */
import { computed } from 'vue'

const props = defineProps<{ name: string }>()

const DOC = ['txt', 'md', 'markdown', 'doc', 'docx', 'pdf', 'rtf', 'odt', 'csv', 'json', 'xml', 'log']
const IMAGE = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'bmp', 'heic', 'heif', 'tiff', 'ico', 'avif']
const VIDEO = ['mp4', 'mov', 'avi', 'mkv', 'webm', 'wmv', 'flv', 'm4v', 'mpg', 'mpeg', '3gp']
const AUDIO = ['mp3', 'wav', 'flac', 'aac', 'ogg', 'm4a', 'opus', 'wma', 'aiff']
const ARCHIVE = ['zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz']

const kind = computed(() => {
  const ext = (props.name.split('.').pop() || '').toLowerCase()
  if (IMAGE.includes(ext)) return 'image'
  if (VIDEO.includes(ext)) return 'video'
  if (AUDIO.includes(ext)) return 'audio'
  if (DOC.includes(ext)) return 'doc'
  if (ARCHIVE.includes(ext)) return 'archive'
  return 'file'
})
</script>

<template>
  <span class="ft" :class="kind" :data-testid="`file-type-${kind}`" :title="kind">
    <!-- document / text -->
    <svg v-if="kind === 'doc' || kind === 'file'" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <path d="M14 2v6h6" />
      <path v-if="kind === 'doc'" d="M9 13h6M9 17h6M9 9h2" />
    </svg>
    <!-- image -->
    <svg v-else-if="kind === 'image'" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <circle cx="9" cy="9" r="1.6" />
      <path d="m21 15-3.1-3.1a2 2 0 0 0-2.8 0L6 21" />
    </svg>
    <!-- video -->
    <svg v-else-if="kind === 'video'" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
      <rect x="2" y="4" width="20" height="16" rx="2" />
      <path d="m10 9 5 3-5 3z" fill="currentColor" stroke="none" />
    </svg>
    <!-- audio -->
    <svg v-else-if="kind === 'audio'" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
      <path d="M9 18V5l12-2v13" />
      <circle cx="6" cy="18" r="3" />
      <circle cx="18" cy="16" r="3" />
    </svg>
    <!-- archive -->
    <svg v-else viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
      <rect x="3" y="3" width="18" height="5" rx="1" />
      <path d="M5 8v11a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8" />
      <path d="M10 12h4" />
    </svg>
  </span>
</template>

<style scoped>
.ft {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 34px;
  height: 34px;
  border-radius: 9px;
  flex: 0 0 auto;
  border: 1px solid currentColor;
  background: color-mix(in srgb, currentColor 12%, transparent);
}
.ft svg {
  width: 18px;
  height: 18px;
}
.ft.doc,
.ft.file {
  color: #34d399;
}
.ft.image {
  color: #22d3ee;
}
.ft.video {
  color: #e879f9;
}
.ft.audio {
  color: #fbbf24;
}
.ft.archive {
  color: #94a3b8;
}
</style>
