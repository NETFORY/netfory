<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, computed, onMounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useAppsStore } from '@/store/apps'
import { appLogo, type Manifest } from '@/lib/bridge'
import { shortAddr } from '@/lib/crypto'
import Identicon from '@/components/Identicon.vue'

const props = defineProps<{ appId: string }>()
const emit = defineEmits<{
  (e: 'saved'): void
  (e: 'request-replace'): void
}>()

const { t } = useI18n()
const apps = useAppsStore()

const app = computed<Manifest | undefined>(() => apps.apps.find((m) => m.id === props.appId))

const name = ref('')
const description = ref('')
const tags = ref('')
const logoBytes = ref<number[] | null>(null)
const logoName = ref('')
const logoInput = ref<HTMLInputElement | null>(null)
const currentLogo = ref<string>('')
const flash = ref('')
const flashOk = ref(true)

const tagList = computed(() =>
  tags.value.split(',').map((s) => s.trim()).filter(Boolean),
)
const logoPreview = computed(() => {
  if (logoBytes.value && logoBytes.value.length) {
    const isSvg = /\.svg$/i.test(logoName.value)
    const bytes = new Uint8Array(logoBytes.value)
    let bin = ''
    for (const b of bytes) bin += String.fromCharCode(b)
    const b64 = btoa(bin)
    return `data:${isSvg ? 'image/svg+xml' : 'image/png'};base64,${b64}`
  }
  return currentLogo.value
})

function prefill() {
  const m = app.value
  name.value = m?.name || ''
  description.value = m?.description || ''
  tags.value = (m?.tags || []).join(', ')
  logoBytes.value = null
  logoName.value = ''
}

async function loadLogo() {
  currentLogo.value = (await appLogo(props.appId)) || ''
}

onMounted(async () => {
  prefill()
  await loadLogo()
})
watch(() => props.appId, async () => {
  prefill()
  await loadLogo()
})

function notify(msg: string, ok = true) {
  flash.value = msg
  flashOk.value = ok
  setTimeout(() => (flash.value = ''), 2200)
}

async function onLogoPick(e: Event) {
  const input = e.target as HTMLInputElement
  const f = input.files?.[0]
  if (!f) return
  const okType = f.type === 'image/png' || f.type === 'image/svg+xml'
  const okExt = /\.(png|svg)$/i.test(f.name)
  if (!okType && !okExt) {
    notify(t('apps.logoFormat'), false)
    input.value = ''
    return
  }
  const buf = new Uint8Array(await f.arrayBuffer())
  logoBytes.value = Array.from(buf)
  logoName.value = f.name
}

async function save() {
  const m = await apps.editMetadata(
    props.appId,
    name.value,
    description.value,
    tagList.value,
    logoBytes.value,
  )
  if (m) {
    await loadLogo()
    logoBytes.value = null
    logoName.value = ''
    notify(t('apps.editedOk'), true)
    emit('saved')
  } else {
    notify(apps.lastError || 'edit-failed', false)
  }
}
</script>

<template>
  <div class="aef" data-testid="app-edit-form">
    <h3 class="aef-h">✎ {{ t('apps.editTitle') }}</h3>
    <p class="aef-app">// {{ app?.name || appId }}</p>

    <label class="aef-l">{{ t('apps.fldTitle') }}</label>
    <input v-model="name" class="aef-in" :placeholder="t('apps.fldTitle')" data-testid="aef-name" />

    <label class="aef-l">{{ t('apps.fldDesc') }}</label>
    <textarea v-model="description" class="aef-in aef-desc" rows="3" :placeholder="t('apps.fldDesc')" data-testid="aef-desc" />

    <label class="aef-l">{{ t('apps.fldTags') }}</label>
    <input v-model="tags" class="aef-in" placeholder="dex, defi, sth" data-testid="aef-tags" />

    <label class="aef-l">{{ t('apps.fldLogo') }}</label>
    <div class="aef-logo-row">
      <button class="aef-btn" @click="logoInput?.click()" data-testid="aef-logo-pick">{{ t('apps.logoPick') }}</button>
      <span class="aef-dim">{{ logoName || t('apps.logoKeep') }}</span>
      <input ref="logoInput" type="file" accept=".png,.svg,image/png,image/svg+xml" class="aef-hidden" @change="onLogoPick" />
    </div>

    <span class="aef-l aef-prev-l">{{ t('apps.livePreview') }}</span>
    <div class="aef-prev" data-testid="aef-preview">
      <div class="aef-prev-top">
        <span class="aef-prev-icon">
          <img v-if="logoPreview" :src="logoPreview" alt="" />
          <Identicon v-else :seed="appId" :size="22" />
        </span>
        <span class="aef-prev-name">// {{ name || t('discover.noTitle') }}</span>
      </div>
      <p class="aef-prev-desc">{{ description || t('profile.noDescription') }}</p>
      <div class="aef-prev-link">sth://{{ shortAddr(appId, 10, 8) }}</div>
      <div v-if="tagList.length" class="aef-prev-tags">
        <span v-for="tg in tagList.slice(0, 5)" :key="tg" class="aef-prev-tag">#{{ tg }}</span>
      </div>
    </div>

    <div class="aef-actions">
      <button class="aef-btn secondary" @click="emit('request-replace')" data-testid="aef-replace-files">
        {{ t('apps.replaceFiles') }} ↻
      </button>
      <button class="aef-btn primary" :disabled="apps.busy" @click="save" data-testid="aef-save">
        {{ t('apps.saveFree') }} ✓
      </button>
    </div>
    <p class="aef-hint">{{ t('apps.editHint') }}</p>

    <transition name="fade">
      <div v-if="flash" class="aef-flash" :class="{ err: !flashOk }" data-testid="aef-flash">
        {{ flashOk ? '✓' : '⚠' }} {{ flash }}
      </div>
    </transition>
  </div>
</template>

<style scoped>
.aef {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 4px 2px 16px;
}
.aef-h {
  font-size: 13px;
  letter-spacing: 0.1em;
  color: var(--accent);
  font-weight: var(--fw-700);
}
.aef-app {
  font-size: 11px;
  color: var(--text-dim);
  margin-bottom: 6px;
}
.aef-l {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text-dim);
  margin-top: 8px;
}
.aef-prev-l {
  margin-top: 12px;
}
.aef-in {
  background: var(--bg-elevated, rgba(255, 255, 255, 0.03));
  border: 1px solid var(--border);
  border-radius: 8px;
  color: var(--text);
  font-size: 13px;
  padding: 9px 11px;
  outline: none;
  transition: border-color 0.15s ease;
  font-family: inherit;
}
.aef-in:focus {
  border-color: var(--accent);
}
.aef-desc {
  resize: vertical;
  min-height: 64px;
}
.aef-logo-row {
  display: flex;
  align-items: center;
  gap: 10px;
}
.aef-dim {
  font-size: 11px;
  color: var(--text-dim);
}
.aef-hidden {
  display: none;
}
.aef-btn {
  background: var(--bg-elevated, rgba(255, 255, 255, 0.04));
  border: 1px solid var(--border);
  border-radius: 8px;
  color: var(--text);
  font-size: 12px;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
  padding: 9px 14px;
  cursor: pointer;
  transition: border-color 0.15s ease, background 0.15s ease, opacity 0.15s ease;
}
.aef-btn:hover {
  border-color: var(--accent);
}
.aef-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.aef-btn.primary {
  background: var(--accent);
  color: #04130d;
  border-color: var(--accent);
}
.aef-btn.secondary {
  color: #6db8ff;
  border-color: rgba(109, 184, 255, 0.4);
}
.aef-prev {
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 12px;
  background: rgba(0, 0, 0, 0.25);
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.aef-prev-top {
  display: flex;
  align-items: center;
  gap: 8px;
}
.aef-prev-icon img {
  width: 22px;
  height: 22px;
  border-radius: 5px;
  object-fit: cover;
}
.aef-prev-name {
  font-size: 13px;
  font-weight: var(--fw-700);
  color: var(--accent);
}
.aef-prev-desc {
  font-size: 12px;
  color: var(--text-dim);
}
.aef-prev-link {
  font-size: 11px;
  color: var(--text-dim);
  font-family: var(--font-mono, monospace);
}
.aef-prev-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 5px;
}
.aef-prev-tag {
  font-size: 10px;
  color: var(--text-dim);
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 1px 8px;
}
.aef-actions {
  display: flex;
  gap: 8px;
  margin-top: 14px;
}
.aef-actions .aef-btn {
  flex: 1;
}
.aef-hint {
  font-size: 10px;
  color: var(--text-dim);
  margin-top: 6px;
}
.aef-flash {
  margin-top: 10px;
  padding: 8px 12px;
  border-radius: 8px;
  font-size: 12px;
  background: rgba(0, 200, 120, 0.12);
  border: 1px solid rgba(0, 200, 120, 0.4);
  color: var(--accent);
}
.aef-flash.err {
  background: rgba(255, 90, 90, 0.12);
  border-color: rgba(255, 90, 90, 0.4);
  color: #ff8a8a;
}
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.2s ease;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
</style>
