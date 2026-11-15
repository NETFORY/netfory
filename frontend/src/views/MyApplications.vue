<script setup lang="ts">
import { ref, reactive, computed, onMounted, onBeforeUnmount, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { isTauri, appShareInfo, openAppWindow, getAppPrices, appLogo, appPreview, onNativeFileDrop, pickFolder, inspectPayload, inspectAppPayload, getMyUsername, privateDappRecipients, privateDappSetRecipients, type PayloadInfo, type Manifest } from '@/lib/bridge'
import { useAppsStore } from '@/store/apps'
import { useTabsStore } from '@/store/tabs'
import { useWalletStore } from '@/store/wallet'
import { useRouter } from 'vue-router'
import { shortAddr } from '@/lib/crypto'
import Identicon from '@/components/Identicon.vue'
import AboutModal from '@/components/AboutModal.vue'
import PreviewCropModal from '@/components/PreviewCropModal.vue'
import FileQrModal from '@/components/FileQrModal.vue'

const { t } = useI18n()
const apps = useAppsStore()
const tabs = useTabsStore()
const wallet = useWalletStore()
const router = useRouter()

// my registered u:// username (clickable author badge on my published apps)
const myUsername = ref<string | null>(null)
function openMyProfile() {
  router.push({ name: 'profile-view', params: { identity: myUsername.value || wallet.mainAddress } })
}

type Tab = 'published' | 'drafts' | 'publish'
const tab = ref<Tab>('published')

// search filter across published + drafts (by name)
const searchQuery = ref('')
// Sort key for apps: use `updated` if set, otherwise `created`. Higher = newer.
function updatedTs(m: { updated?: string; created?: string }): number {
  const raw = m.updated || m.created || '0'
  const n = Number(raw)
  return Number.isFinite(n) ? n : 0
}
function sortByUpdatedDesc<T extends { updated?: string; created?: string }>(list: T[]): T[] {
  return [...list].sort((a, b) => updatedTs(b) - updatedTs(a))
}
const filteredPublished = computed(() => {
  const q = searchQuery.value.trim().toLowerCase()
  const base = q ? apps.myPublished.filter((m) => m.name.toLowerCase().includes(q)) : apps.myPublished
  return sortByUpdatedDesc(base)
})
const filteredDrafts = computed(() => {
  const q = searchQuery.value.trim().toLowerCase()
  const base = q ? apps.myDrafts.filter((m) => m.name.toLowerCase().includes(q)) : apps.myDrafts
  return sortByUpdatedDesc(base)
})

/** Sum of all file sizes in the app manifest (bytes). */
function appSizeBytes(m: { files?: { size?: number }[] }): number {
  if (!m.files || !m.files.length) return 0
  let total = 0
  for (const f of m.files) total += Number(f.size || 0)
  return total
}

const dragging = ref(false)
const appName = ref('')
const appDesc = ref('')
// P5: модалка "История выплат" (payout:<appId> зачисления от сети).
const appTags = ref<string[]>([])
const tagInput = ref('')
const logoBytes = ref<number[] | null>(null)
const logoName = ref('')
// обязательный DAPP Preview 440×180 (JPG/PNG ≤ 256 KB) — карточка каталога
const previewBytes = ref<number[] | null>(null)
const previewName = ref('')

/* ---- Private dApp (E2EE-контейнер, доступ по списку адресов) ---- */
const privEnabled = ref(false)
const privList = ref('')
const privRecipients = computed(() =>
  privList.value
    .split(/[\n,;\s]+/)
    .map((s) => s.trim())
    .filter(Boolean),
)
// модалка «Список доступа» на опубликованных приватных dApps
const accessModalId = ref<string | null>(null)
const accessText = ref('')
const accessBusy = ref(false)
async function openAccess(m: Manifest) {
  accessModalId.value = m.id
  accessBusy.value = true
  try {
    accessText.value = (await privateDappRecipients(m.id)).join('\n')
  } catch (e: any) {
    accessText.value = ''
    notify(String(e?.message || e))
  } finally {
    accessBusy.value = false
  }
}
async function saveAccess() {
  if (!accessModalId.value) return
  const list = accessText.value
    .split(/[\n,;\s]+/)
    .map((s) => s.trim())
    .filter(Boolean)
  accessBusy.value = true
  try {
    const m = await privateDappSetRecipients(accessModalId.value, list)
    if (m) {
      notify(t('apps.privSaved'))
      accessModalId.value = null
      await apps.init()
      loadLogos()
    }
  } catch (e: any) {
    notify(String(e?.message || e))
  } finally {
    accessBusy.value = false
  }
}
const aboutId = ref('')
const aboutTitle = ref('')
const flash = ref('')
const copied = ref('')
const confirmDelete = ref('')
const deletingId = ref('')
const fileInput = ref<HTMLInputElement | null>(null)
const logoInput = ref<HTMLInputElement | null>(null)
const previewInput = ref<HTMLInputElement | null>(null)

// publish-price oracle + escrow (dynamic pricing)
const publishPrice = ref(10)
const basePrice = ref(10)
const pricePerMb = ref(2)
const escrow = ref('SappsPqDQXCRvoi1V2Hs9xycQeT1ptJWV1')

// staged folder (dropped/selected) awaiting draft creation
const staged = ref<PayloadInfo | null>(null)

// publish modal + its priced payload
const publishModalId = ref('')
const publishFree = ref(false)
const publishInfo = ref<PayloadInfo | null>(null)

// update modal + its priced payload
const updateModalId = ref('')
const updateInfo = ref<PayloadInfo | null>(null)

// boost modal
const boostModalId = ref('')
const boostAmount = ref('')

// per-app logo data-URL cache
const logos = reactive<Record<string, string>>({})
// DAPP Preview 440×180 — обложка, которую владелец загрузил при публикации/edit.
// Показывается справа от карточки в MY_APPLICATIONS_ (см. loadPreviews()).
const previews = reactive<Record<string, string>>({})

onMounted(async () => {
  if (!apps.loaded) await apps.init()
  if (!apps.myPublished.length && apps.myDrafts.length) tab.value = 'drafts'
  if (!apps.myApps.length) tab.value = 'publish'
  myUsername.value = await getMyUsername()
  try {
    const p = await getAppPrices()
    publishPrice.value = p.basePrice
    basePrice.value = p.basePrice
    pricePerMb.value = p.pricePerMb
    escrow.value = p.escrow
  } catch {
    /* defaults */
  }
  loadLogos()
  prefetchDirectLinks()
  // honor a deep-link edit request (e.g. "Manage app" from the telemetry panel)
  if (apps.editRequestId) {
    const m = apps.apps.find((x) => x.id === apps.editRequestId)
    apps.editRequestId = ''
    if (m) {
      tab.value = 'published'
      openEdit(m)
    }
  }
})

/* ---- native (Tauri v2) OS drag-and-drop ----
 * Tauri intercepts OS file drops before the DOM, so the browser `@drop` event
 * never reaches the WebView. We capture Tauri's native drag-drop channel and
 * route the dropped folder PATH straight into the draft pipeline. */
let unlistenDrop: (() => void) | null = null
onMounted(async () => {
  if (!isTauri) return
  unlistenDrop = await onNativeFileDrop((e) => {
    if (tab.value !== 'publish') return
    if (e.type === 'enter' || e.type === 'over') {
      dragging.value = true
      return
    }
    if (e.type === 'leave') {
      dragging.value = false
      return
    }
    if (e.type === 'drop') {
      dragging.value = false
      void onNativeDrop(e.paths || [])
    }
  })
})
onBeforeUnmount(() => {
  if (unlistenDrop) unlistenDrop()
})

// After a successful publish payment (finalize flips status→published), jump the
// user straight to the Published tab + confirm, so the app visibly "moves" out
// of Drafts the moment the wallet accepts the transaction.
watch(
  () => apps.publishTick,
  (tick) => {
    if (!tick) return
    tab.value = 'published'
    loadLogos()
    notify(t('apps.publishedOk'))
  },
)

async function onNativeDrop(paths: string[]) {
  if (paths.length !== 1) {
    notify(t('apps.dropMultiError'))
    return
  }
  await stageFolder(paths[0])
}

/** Inspect + stage a dropped/selected folder, surfacing name/files/path/cost. */
async function stageFolder(path: string) {
  const info = await inspectPayload(path)
  if (!info) {
    notify(t('apps.dropNotFolder'))
    return
  }
  staged.value = info
}

function clearStaged() {
  staged.value = null
}

/* human-readable size + MB number */
function fmtSize(bytes: number): string {
  if (bytes >= 1048576) return (bytes / 1048576).toFixed(1) + ' МБ'
  if (bytes >= 1024) return (bytes / 1024).toFixed(1) + ' КБ'
  return bytes + ' Б'
}

async function refreshLogo(id: string) {
  if (!isTauri) return
  const url = await appLogo(id)
  if (url) logos[id] = url
  else delete logos[id]
  // Preview обновляем той же командой — владельцу важно видеть свежий 440×180
  // сразу после Edit/Publish, без ре-скана каталога.
  const p = await appPreview(id)
  if (p) previews[id] = p
  else delete previews[id]
}
async function loadLogos(force = false) {
  if (!isTauri) return
  for (const m of apps.myApps) {
    if (force || logos[m.id] === undefined) {
      const url = await appLogo(m.id)
      if (url) logos[m.id] = url
      else if (force) delete logos[m.id]
    }
    if (force || previews[m.id] === undefined) {
      const p = await appPreview(m.id)
      if (p) previews[m.id] = p
      else if (force) delete previews[m.id]
    }
  }
}

function notify(msg: string) {
  flash.value = msg
  setTimeout(() => (flash.value = ''), 2400)
}

/* generative fallback badge color from the app id */
function hueFor(id: string): number {
  let h = 0
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) % 360
  return h
}

/* ---- tags ---- */
function addTag() {
  const v = tagInput.value.trim().toLowerCase().replace(/[,\s]+/g, '')
  if (v && !appTags.value.includes(v) && appTags.value.length < 8) appTags.value.push(v)
  tagInput.value = ''
}
function removeTag(tg: string) {
  appTags.value = appTags.value.filter((x) => x !== tg)
}

function openAbout(m: { id: string; name: string }) {
  aboutId.value = m.id
  aboutTitle.value = m.name
}

function resetForm() {
  appName.value = ''
  appDesc.value = ''
  appTags.value = []
  tagInput.value = ''
  logoBytes.value = null
  logoName.value = ''
  previewBytes.value = null
  previewName.value = ''
  privEnabled.value = false
  privList.value = ''
}

function missingMeta(): boolean {
  if (!appName.value.trim()) {
    notify(t('apps.titleRequired'))
    return true
  }
  if (!appDesc.value.trim()) {
    notify(t('apps.descRequired'))
    return true
  }
  // DAPP Preview 440×180 обязателен для публичных dApp (у приватных скрыт)
  if (!privEnabled.value && (!previewBytes.value || !previewBytes.value.length)) {
    notify(t('apps.previewRequired'))
    return true
  }
  return false
}

/* ---- logo pick (PNG or SVG only) ---- */
async function onLogoPick(e: Event) {
  const input = e.target as HTMLInputElement
  const f = input.files?.[0]
  if (!f) return
  const okType = f.type === 'image/png' || f.type === 'image/svg+xml'
  const okExt = /\.(png|svg)$/i.test(f.name)
  if (!okType && !okExt) {
    notify(t('apps.logoFormat'))
    input.value = ''
    return
  }
  const buf = new Uint8Array(await f.arrayBuffer())
  logoBytes.value = Array.from(buf)
  logoName.value = f.name
}

/* ---- DAPP Preview: любая картинка → кроп/масштаб в 440×180 (модалка) ---- */
const cropFile = ref<File | null>(null)
const cropTarget = ref<'new' | 'edit'>('new')

function onPreviewPick(e: Event) {
  const input = e.target as HTMLInputElement
  const f = input.files?.[0]
  input.value = ''
  if (!f) return
  cropTarget.value = 'new'
  cropFile.value = f
}

function onCropApply(bytes: number[], name: string) {
  if (cropTarget.value === 'edit') {
    editPreviewBytes.value = bytes
    editPreviewName.value = name
  } else {
    previewBytes.value = bytes
    previewName.value = name
  }
  cropFile.value = null
}

function onCropDecodeFail() {
  cropFile.value = null
  notify(t('apps.previewFormat'))
}

/* ---- create draft ---- */
async function pickFolderToStage() {
  const dir = await pickFolder()
  if (dir) await stageFolder(dir)
}

async function createDraftTrigger() {
  if (missingMeta()) return
  // приватный dApp: нужен непустой список адресов и нативный клиент
  if (privEnabled.value) {
    if (!isTauri) {
      notify(t('apps.privNativeOnly'))
      return
    }
    if (!privRecipients.value.length) {
      notify(t('apps.privNeedAddr'))
      return
    }
  }
  if (isTauri) {
    // Native: must have a staged folder (dropped or picked) before creating.
    if (!staged.value) {
      await pickFolderToStage()
      return
    }
    const m = await apps.createDraftFromPath(
      appName.value,
      appDesc.value,
      appTags.value,
      staged.value.path,
      logoBytes.value,
      previewBytes.value,
      privEnabled.value ? privRecipients.value : null,
    )
    if (m) {
      notify(t('apps.draftCreated'))
      resetForm()
      clearStaged()
      tab.value = 'drafts'
      loadLogos()
    } else if (apps.lastError) {
      notify(apps.lastError.includes('folder') ? t('apps.dropNotFolder') : apps.lastError)
    }
  } else {
    fileInput.value?.click()
  }
}

async function onSelect(e: Event) {
  const input = e.target as HTMLInputElement
  const m = await apps.createDraft(appName.value, appDesc.value, appTags.value, input.files, logoBytes.value, previewBytes.value)
  if (m) {
    notify(t('apps.draftCreated'))
    resetForm()
    tab.value = 'drafts'
  }
  input.value = ''
}

async function onDrop(e: DragEvent) {
  dragging.value = false
  if (missingMeta()) return
  if (privEnabled.value) {
    // приватные dApps доступны только в нативном клиенте
    notify(t('apps.privNativeOnly'))
    return
  }
  const m = await apps.createDraft(appName.value, appDesc.value, appTags.value, e.dataTransfer?.files, logoBytes.value, previewBytes.value)
  if (m) {
    notify(t('apps.draftCreated'))
    resetForm()
    tab.value = 'drafts'
  }
}

/* ---- preview (drafts) ---- */
async function previewApp(id: string) {
  if (isTauri) {
    await apps.preview(id)
    await refreshLogo(id)
  }
  launchTab({ id, name: id })
}

/* ---- publish flow ---- */
async function openPublishModal(id: string) {
  publishModalId.value = id
  publishFree.value = false
  publishInfo.value = null
  // price the payload by its stored source folder weight (dynamic cost)
  publishInfo.value = await inspectAppPayload(id)
}
/** Dynamic publish cost (STH) for the open modal — from the priced payload. */
const publishCost = computed(() => publishInfo.value?.cost ?? basePrice.value)
async function confirmPublish() {
  const id = publishModalId.value
  if (!id) return
  if (publishFree.value) {
    const m = await apps.finalize(id, true)
    if (m) {
      notify(t('apps.publishedOk'))
      tab.value = 'published'
    } else {
      notify(apps.lastError || 'publish-failed')
    }
    publishModalId.value = ''
  } else {
    // hand off to the wallet drawer: locked Pay & Publish → finalize on success
    wallet.openPublishPayment(id, publishCost.value, escrow.value)
    publishModalId.value = ''
  }
}

/* ---- update / delete ---- */
async function updateApp(id: string) {
  if (isTauri) {
    // pick the new folder, price it, then confirm + pay the dynamic update fee
    const dir = await pickFolder()
    if (!dir) return
    const info = await inspectPayload(dir)
    if (!info) {
      notify(t('apps.dropNotFolder'))
      return
    }
    updateModalId.value = id
    updateInfo.value = info
  } else {
    fileInput.value?.click()
  }
}
function confirmUpdate() {
  const id = updateModalId.value
  const info = updateInfo.value
  if (!id || !info) return
  wallet.openUpdatePayment(id, info.cost, escrow.value, info.path)
  updateModalId.value = ''
  updateInfo.value = null
}
const updateApp_ = computed(() => apps.apps.find((m: Manifest) => m.id === updateModalId.value))

/* ---- edit (free metadata) modal ---- */
const editModalId = ref('')
const editName = ref('')
const editDescription = ref('')
const editTags = ref('')
const editLogoBytes = ref<number[] | null>(null)
const editLogoName = ref('')
const editLogoInput = ref<HTMLInputElement | null>(null)
// EDIT: замена DAPP Preview 440×180 без переиздания (free-редакт)
const editPreviewBytes = ref<number[] | null>(null)
const editPreviewName = ref('')
const editPreviewInput = ref<HTMLInputElement | null>(null)
const editApp_ = computed(() => apps.apps.find((m: Manifest) => m.id === editModalId.value))

// live preview of the dApp card while editing (mirrors the Discovered card).
const editLogoPreview = computed(() => {
  if (editLogoBytes.value && editLogoBytes.value.length) {
    const isSvg = /\.svg$/i.test(editLogoName.value)
    const bytes = new Uint8Array(editLogoBytes.value)
    let bin = ''
    const chunk = 0x8000
    for (let i = 0; i < bytes.length; i += chunk) {
      bin += String.fromCharCode(...bytes.subarray(i, i + chunk))
    }
    return `data:${isSvg ? 'image/svg+xml' : 'image/png'};base64,${btoa(bin)}`
  }
  return logos[editModalId.value] || ''
})
const editTagList = computed(() =>
  editTags.value.split(',').map((s) => s.trim()).filter(Boolean),
)

function openEdit(m: Manifest) {
  editModalId.value = m.id
  editName.value = m.name
  editDescription.value = m.description || ''
  editTags.value = (m.tags || []).join(', ')
  editLogoBytes.value = null
  editLogoName.value = ''
  editPreviewBytes.value = null
  editPreviewName.value = ''
}
async function onEditLogoPick(e: Event) {
  const input = e.target as HTMLInputElement
  const f = input.files?.[0]
  if (!f) return
  const okType = f.type === 'image/png' || f.type === 'image/svg+xml'
  const okExt = /\.(png|svg)$/i.test(f.name)
  if (!okType && !okExt) {
    notify(t('apps.logoFormat'))
    input.value = ''
    return
  }
  const buf = new Uint8Array(await f.arrayBuffer())
  editLogoBytes.value = Array.from(buf)
  editLogoName.value = f.name
}
async function onEditPreviewPick(e: Event) {
  const input = e.target as HTMLInputElement
  const f = input.files?.[0]
  input.value = ''
  if (!f) return
  cropTarget.value = 'edit'
  cropFile.value = f
}
async function saveEdit() {
  const id = editModalId.value
  if (!id) return
  const tags = editTags.value.split(',').map((s) => s.trim()).filter(Boolean)
  const m = await apps.editMetadata(id, editName.value, editDescription.value, tags, editLogoBytes.value, editPreviewBytes.value)
  if (m) {
    notify(t('apps.editedOk'))
    await refreshLogo(id)
    editModalId.value = ''
  } else {
    notify(apps.lastError || 'edit-failed')
  }
}
async function replaceFiles() {
  const id = editModalId.value
  editModalId.value = ''
  if (id) await updateApp(id)
}
async function removeApp(id: string) {
  confirmDelete.value = ''
  deletingId.value = id
  try {
    await apps.remove(id)
    tabs.close(id)
    notify(t('apps.deletedOk'))
  } finally {
    deletingId.value = ''
  }
}

/* ---- launch ---- */
function launchTab(m: { id: string; name: string }) {
  tabs.open(m.id, m.name)
}
async function launchWindow(id: string) {
  const native = await openAppWindow(id, id)
  if (!native) tabs.open(id, id)
}
async function copyId(id: string) {
  try {
    await navigator.clipboard.writeText(`sth://${id}`)
    copied.value = id
    setTimeout(() => (copied.value = ''), 1300)
  } catch {
    /* noop */
  }
}

// Прямая ссылка sth://<id>?n=<nodeId>&h=<hash> — установка напрямую с нашей
// ноды, без трекеров и headless-сидеров (нода должна быть онлайн).
// Префетч при загрузке списка: кнопки ⚡ DIRECT LINK / QR серые (disabled),
// пока ссылка не готова — клик всегда копирует уже готовую ссылку из кэша.
const copiedDirect = ref('')
const directLinks = ref<Record<string, string>>({})
let prefetchingLinks = false
async function prefetchDirectLinks() {
  if (!isTauri || prefetchingLinks) return
  prefetchingLinks = true
  try {
    // последовательно: бэкенд при промахе сам досидирует (seed_all тяжёлый)
    for (const m of apps.apps) {
      if (directLinks.value[m.id]) continue
      try {
        const info = await appShareInfo(m.id)
        if (info && info.nodeId && info.hash)
          directLinks.value[m.id] = `sth://${m.id}?n=${info.nodeId}&h=${info.hash}`
      } catch {
        /* кнопка останется серой */
      }
    }
  } finally {
    prefetchingLinks = false
  }
}
watch(
  () => [apps.apps.length, apps.publishTick],
  () => prefetchDirectLinks(),
)
async function copyDirectLink(id: string) {
  const link = directLinks.value[id]
  if (!link) return
  try {
    await navigator.clipboard.writeText(link)
    copiedDirect.value = id
    setTimeout(() => (copiedDirect.value = ''), 1300)
  } catch {
    /* noop */
  }
}

// QR-код прямой ссылки — поделиться приложением с телефона одним сканом.
const qrLink = ref('')
const qrName = ref('')
function openQr(m: { id: string; name: string }) {
  const link = directLinks.value[m.id]
  if (!link) return
  qrName.value = m.name
  qrLink.value = link
}

const modalApp = computed(() => apps.apps.find((m: Manifest) => m.id === publishModalId.value))

/* ---- boost flow ---- */
function openBoostModal(id: string) {
  boostModalId.value = id
  boostAmount.value = ''
}
function confirmBoost() {
  const id = boostModalId.value
  const amt = Number(boostAmount.value)
  if (!id || !(amt > 0)) return
  wallet.openBoostPayment(id, amt, escrow.value)
  boostModalId.value = ''
}
const boostApp = computed(() => apps.apps.find((m: Manifest) => m.id === boostModalId.value))
</script>

<template>
  <div class="apps" data-testid="apps-view">
    <div class="apps-hero">
      <h1 class="page-title">// {{ t('apps.title') }}_</h1>
    </div>

    <!-- QR прямой ссылки: скан с телефона → установка напрямую с нашей ноды -->
    <FileQrModal
      v-if="qrLink"
      :link="qrLink"
      :name="qrName"
      :title="t('apps.qrTitle')"
      :hint="t('apps.qrHint')"
      @close="qrLink = ''"
    />

    <!-- TABS -->
    <div class="tabs" data-testid="apps-tabs">
      <button
        class="tab"
        :class="{ on: tab === 'published' }"
        @click="tab = 'published'"
        data-testid="tab-published"
      >
        {{ t('apps.tabPublished') }} <span class="cnt">{{ apps.myPublished.length }}</span>
      </button>
      <button
        class="tab"
        :class="{ on: tab === 'drafts' }"
        @click="tab = 'drafts'"
        data-testid="tab-drafts"
      >
        {{ t('apps.tabDrafts') }} <span class="cnt">{{ apps.myDrafts.length }}</span>
      </button>
      <button
        class="tab tab-cta"
        :class="{ on: tab === 'publish' }"
        @click="tab = 'publish'"
        data-testid="tab-publish"
      >
        + {{ t('apps.tabPublish') }}
      </button>
    </div>

    <p v-if="flash" class="flash hl" data-testid="apps-flash">{{ flash }}</p>

    <!-- SEARCH BAR (filters published + drafts by name) -->
    <div v-if="tab !== 'publish'" class="search-row" data-testid="apps-search-row">
      <span class="search-ic">⌕</span>
      <input
        v-model="searchQuery"
        class="search-input"
        type="text"
        :placeholder="t('apps.searchPlaceholder')"
        data-testid="apps-search-input"
      />
      <button v-if="searchQuery" class="search-clear" @click="searchQuery = ''" data-testid="apps-search-clear">✕</button>
    </div>

    <!-- ===================== PUBLISH (create draft) ===================== -->
    <section v-if="tab === 'publish'" class="pane" data-testid="pane-publish">
      <!-- dropzone: empty state OR staged folder summary -->
      <div
        class="drop"
        :class="{ active: dragging, busy: apps.busy, staged: !!staged }"
        @dragover.prevent="dragging = true"
        @dragleave.prevent="dragging = false"
        @drop.prevent="onDrop"
        data-testid="apps-dropzone"
      >
        <template v-if="!staged">
          <div class="drop-icon">{{ apps.busy ? '⟳' : '↥' }}</div>
          <p class="drop-main">{{ apps.busy ? t('apps.creating') : t('apps.dropZone') }}</p>
          <p class="drop-sub dim">{{ t('apps.dropHint') }}</p>
        </template>
        <div v-else class="staged-box" data-testid="apps-staged">
          <div class="staged-head">
            <span class="staged-ok">✓ {{ t('apps.folderAccepted') }}</span>
            <button class="staged-clear" @click="clearStaged" data-testid="apps-staged-clear">✕ {{ t('apps.changeFolder') }}</button>
          </div>
          <div class="staged-name" data-testid="apps-staged-name">📁 {{ staged.name }}</div>
          <code class="staged-path" data-testid="apps-staged-path">{{ staged.path }}</code>
          <div class="staged-meta">
            <span data-testid="apps-staged-files">{{ t('apps.filesLabel') }}: <b class="hl">{{ staged.fileCount }}</b></span>
            <span data-testid="apps-staged-size">{{ t('apps.sizeLabel') }}: <b class="hl">{{ fmtSize(staged.sizeBytes) }}</b></span>
            <span class="staged-cost" data-testid="apps-staged-cost">{{ t('apps.costLabel') }}: <b>{{ staged.cost }} STH</b></span>
          </div>
        </div>
      </div>

      <div class="controls">
        <input v-model="appName" class="field" :placeholder="t('apps.appName')" data-testid="apps-name-input" />
        <textarea
          v-model="appDesc"
          class="field desc-field"
          rows="2"
          :placeholder="t('apps.appDesc')"
          data-testid="apps-desc-input"
        />
        <button class="btn btn-primary" :disabled="apps.busy" @click="createDraftTrigger" data-testid="select-folder-btn">
          {{ isTauri && !staged ? t('apps.selectFolder') : t('apps.createDraft') }}
        </button>
        <input ref="fileInput" type="file" webkitdirectory multiple class="hidden-input" @change="onSelect" />
      </div>
      <p class="req-note dim" data-testid="apps-req-note">{{ t('apps.metaRequired') }}</p>

      <!-- logo + обязательный DAPP Preview 440×180 -->
      <div class="logo-row">
        <button class="btn" @click="logoInput?.click()" data-testid="apps-logo-pick">⬡ {{ t('apps.logoPick') }}</button>
        <span v-if="logoName" class="dim logo-name" data-testid="apps-logo-name">{{ logoName }} · {{ t('apps.logoSelected') }}</span>
        <template v-if="!privEnabled">
          <button class="btn" @click="previewInput?.click()" data-testid="apps-preview-pick">▭ {{ t('apps.previewPick') }}</button>
          <span v-if="previewName" class="dim logo-name" data-testid="apps-preview-name">{{ previewName }} · {{ t('apps.previewSelected') }}</span>
        </template>
        <span class="dim logo-hint">{{ t('apps.logoLabel') }}<template v-if="!privEnabled"> · {{ t('apps.previewLabel') }}</template></span>
        <input ref="logoInput" type="file" accept=".png,.svg,image/png,image/svg+xml" class="hidden-input" @change="onLogoPick" />
        <input ref="previewInput" type="file" accept="image/*" class="hidden-input" @change="onPreviewPick" />
      </div>

      <!-- tags -->
      <div class="tags-row">
        <div class="tag-chips">
          <span v-for="tg in appTags" :key="tg" class="tag-chip" :data-testid="`tag-chip-${tg}`">
            #{{ tg }}
            <button class="tag-x" @click="removeTag(tg)" :data-testid="`tag-remove-${tg}`">✕</button>
          </span>
        </div>
        <input
          v-model="tagInput"
          class="field tag-input"
          :placeholder="t('apps.tagsPlaceholder')"
          :disabled="appTags.length >= 8"
          @keydown.enter.prevent="addTag"
          @keydown="(e) => (e.key === ',' ? (e.preventDefault(), addTag()) : null)"
          data-testid="apps-tags-input"
        />
      </div>

      <!-- Private dApp (E2EE) -->
      <div class="priv-row">
        <label class="priv-check" data-testid="apps-private-row">
          <input type="checkbox" v-model="privEnabled" data-testid="apps-private-toggle" />
          <span class="priv-lock">🔒</span> {{ t('apps.privToggle') }}
        </label>
        <template v-if="privEnabled">
          <textarea
            v-model="privList"
            class="field priv-field"
            rows="3"
            :placeholder="t('apps.privListPlaceholder')"
            data-testid="apps-private-list"
          ></textarea>
          <p class="dim priv-hint">{{ t('apps.privHint') }}</p>
        </template>
      </div>
    </section>

    <!-- ===================== DRAFTS ===================== -->
    <section v-else-if="tab === 'drafts'" class="pane" data-testid="pane-drafts">
      <div v-if="filteredDrafts.length" class="app-list" data-testid="drafts-list">
        <div v-for="m in filteredDrafts" :key="m.id" class="app-card panel draft" :data-testid="`draft-${m.id}`">
          <div v-if="deletingId === m.id" class="del-overlay" :data-testid="`deleting-${m.id}`">
            <span class="del-spin"></span>
            <span class="del-text">Идёт удаление…</span>
          </div>
          <div class="card-body">
          <div class="card-head">
            <span class="logo-badge" v-if="logos[m.id]"><img :src="logos[m.id]" alt="" /></span>
            <span v-else class="logo-badge gen" :style="{ background: `hsl(${hueFor(m.id)} 70% 22%)`, color: `hsl(${hueFor(m.id)} 90% 70%)` }">
              {{ m.name.slice(0, 2).toUpperCase() }}
            </span>
            <div class="head-text">
              <div class="app-top">
                <span class="app-name">// {{ m.name }}</span>
                <span class="badge badge-draft" :data-testid="`badge-draft-${m.id}`">{{ t('apps.badgeDraft') }}</span>
                <span v-if="m.private" class="badge badge-priv" :title="t('apps.privBadge')" :data-testid="`badge-priv-${m.id}`">🔒 PRIVATE</span>
              </div>
              <p class="app-desc dim">{{ m.description || '—' }}</p>
            </div>
          </div>
          <div class="app-meta">
            <span class="dim">{{ t('apps.files') }}: <b class="hl">{{ m.files.length }}</b></span>
            <span class="dim" :data-testid="`draft-size-${m.id}`">{{ t('apps.sizeLabel') }}: <b class="hl">{{ fmtSize(appSizeBytes(m)) }}</b></span>
          </div>
          <div class="app-actions">
            <button class="btn" @click="previewApp(m.id)" :data-testid="`preview-app-${m.id}`">{{ t('apps.preview') }} ▶</button>
            <button class="btn btn-primary" @click="openPublishModal(m.id)" :data-testid="`publish-app-${m.id}`">{{ t('apps.publish') }} ⬡</button>
            <button class="btn" @click="openEdit(m)" :data-testid="`edit-app-${m.id}`">{{ t('apps.edit') }} ✎</button>
            <template v-if="confirmDelete !== m.id">
              <button class="btn btn-danger" @click="confirmDelete = m.id" :data-testid="`delete-app-${m.id}`">{{ t('apps.delete') }} ✕</button>
            </template>
            <template v-else>
              <button class="btn" @click="confirmDelete = ''" :data-testid="`delete-cancel-${m.id}`">{{ t('common.cancel') }}</button>
              <button class="btn btn-danger" @click="removeApp(m.id)" :data-testid="`delete-confirm-${m.id}`">{{ t('common.confirm') }}</button>
            </template>
          </div>
          </div>
          <!-- Preview 440×180 справа — только если владелец загрузил обложку. -->
          <div v-if="previews[m.id]" class="card-preview" :data-testid="`draft-preview-${m.id}`">
            <img :src="previews[m.id]" alt="" />
          </div>
        </div>
      </div>
      <div v-else class="empty panel" data-testid="drafts-empty">
        <span class="empty-mark">[ 0x00 ]</span><span class="dim">{{ searchQuery ? t('apps.searchEmpty') : t('apps.draftsEmpty') }}</span>
      </div>
    </section>

    <!-- ===================== PUBLISHED ===================== -->
    <section v-else class="pane" data-testid="pane-published">
      <div v-if="filteredPublished.length" class="app-list" data-testid="apps-list">
        <div v-for="m in filteredPublished" :key="m.id" class="app-card panel" :data-testid="`app-${m.id}`">
          <div v-if="deletingId === m.id" class="del-overlay" :data-testid="`deleting-${m.id}`">
            <span class="del-spin"></span>
            <span class="del-text">Идёт удаление…</span>
          </div>
          <div class="card-body">
          <div class="card-head">
            <span class="logo-badge" v-if="logos[m.id]"><img :src="logos[m.id]" alt="" /></span>
            <span v-else class="logo-badge gen" :style="{ background: `hsl(${hueFor(m.id)} 70% 22%)`, color: `hsl(${hueFor(m.id)} 90% 70%)` }">
              {{ m.name.slice(0, 2).toUpperCase() }}
            </span>
            <div class="head-text">
              <div class="app-top">
                <span class="app-name">// {{ m.name }} <span class="dim ver">v{{ m.version || 1 }}</span></span>
                <span
                  class="badge"
                  :class="m.free ? 'badge-free' : 'badge-paid'"
                  :data-testid="`badge-${m.id}`"
                >{{ m.free ? t('apps.badgeFree') : t('apps.badgePaid') }}</span>
                <span v-if="m.private" class="badge badge-priv" :title="t('apps.privBadge')" :data-testid="`badge-priv-${m.id}`">🔒 PRIVATE</span>
              </div>
              <p class="app-desc dim" :data-testid="`app-desc-${m.id}`">{{ m.description || t('discover.noDescription') }}</p>
            </div>
          </div>

          <div v-if="m.tags && m.tags.length" class="card-tags" :data-testid="`app-tags-${m.id}`">
            <span v-for="tg in m.tags" :key="tg" class="card-tag">#{{ tg }}</span>
          </div>

          <div class="app-url-row">
            <code class="app-url" data-testid="app-url">sth://{{ m.id }}</code>
            <button class="btn copy-btn" @click="copyId(m.id)" :data-testid="`copy-app-${m.id}`">
              {{ copied === m.id ? t('common.copied') : t('common.copy') }}
            </button>
            <button
              class="btn copy-btn direct-btn"
              @click="copyDirectLink(m.id)"
              :disabled="!directLinks[m.id]"
              :title="directLinks[m.id] ? t('apps.directLinkHint') : t('apps.directLinkErr')"
              :data-testid="`copy-direct-${m.id}`"
            >
              {{ copiedDirect === m.id ? t('common.copied') : '⚡ ' + t('apps.directLink') }}
            </button>
            <button
              class="btn copy-btn direct-btn"
              @click="openQr(m)"
              :disabled="!directLinks[m.id]"
              :title="directLinks[m.id] ? t('apps.qrBtnHint') : t('apps.directLinkErr')"
              :data-testid="`qr-direct-${m.id}`"
            >
              ▦ QR
            </button>
          </div>

          <div class="app-meta">
            <span class="dim">{{ t('apps.files') }}: <b class="hl">{{ m.files.length }}</b></span>
            <span class="dim" :data-testid="`app-size-${m.id}`">{{ t('apps.sizeLabel') }}: <b class="hl">{{ fmtSize(appSizeBytes(m)) }}</b></span>
            <span class="dim">{{ t('apps.merkle') }}: <code>{{ shortAddr(m.merkle, 8, 6) }}</code></span>
            <span
              v-if="myUsername"
              class="dim author-link"
              @click="openMyProfile"
              :title="`u://${myUsername}`"
              :data-testid="`app-author-${m.id}`"
            >@{{ myUsername }}</span>
          </div>

          <div class="app-actions">
            <button class="btn btn-primary" @click="launchTab(m)" :data-testid="`launch-app-${m.id}`">{{ t('apps.openTab') }} →</button>
            <button class="btn" @click="launchWindow(m.id)" :data-testid="`window-app-${m.id}`">{{ t('apps.openWindow') }} ⧉</button>
            <button v-if="!m.free" class="btn btn-boost" @click="openBoostModal(m.id)" :data-testid="`boost-app-${m.id}`">⚡ {{ t('apps.boost') }}</button>
            <button class="btn" @click="openAbout(m)" :data-testid="`about-app-${m.id}`">{{ t('about.button') }} ⓘ</button>
            <button v-if="m.private" class="btn" @click="openAccess(m)" :data-testid="`access-app-${m.id}`">🔒 {{ t('apps.privAccess') }}</button>
            <button class="btn" @click="openEdit(m)" :data-testid="`edit-app-${m.id}`">{{ t('apps.edit') }} ✎</button>
            <template v-if="confirmDelete !== m.id">
              <button class="btn btn-danger" @click="confirmDelete = m.id" :data-testid="`delete-app-${m.id}`">{{ t('apps.delete') }} ✕</button>
            </template>
            <template v-else>
              <button class="btn" @click="confirmDelete = ''" :data-testid="`delete-cancel-${m.id}`">{{ t('common.cancel') }}</button>
              <button class="btn btn-danger" @click="removeApp(m.id)" :data-testid="`delete-confirm-${m.id}`">{{ t('common.confirm') }}</button>
            </template>
          </div>
          </div>
          <!-- Preview 440×180 справа от карточки (~30% ширины). Показываем
               только если владелец загрузил обложку, иначе оставляем полную
               ширину контенту, без пустого прямоугольника. -->
          <div v-if="previews[m.id]" class="card-preview" :data-testid="`app-preview-${m.id}`">
            <img :src="previews[m.id]" alt="" />
          </div>
        </div>
      </div>
      <div v-else class="empty panel" data-testid="apps-empty">
        <span class="empty-mark">[ 0x00 ]</span><span class="dim">{{ searchQuery ? t('apps.searchEmpty') : t('apps.publishedEmpty') }}</span>
      </div>
    </section>

    <!-- ===================== ACCESS LIST MODAL (Private dApp) ===================== -->
    <div v-if="accessModalId" class="modal-back" @click.self="accessModalId = null" data-testid="access-modal">
      <div class="modal panel">
        <h2 class="modal-title">🔒 {{ t('apps.privAccessTitle') }}</h2>
        <p class="modal-app dim">// sth://{{ shortAddr(accessModalId, 10, 6) }}</p>
        <p class="paid-note dim">{{ t('apps.privAccessHint') }}</p>
        <textarea
          v-model="accessText"
          class="field priv-field"
          rows="6"
          :disabled="accessBusy"
          :placeholder="t('apps.privListPlaceholder')"
          data-testid="access-modal-list"
        ></textarea>
        <div class="modal-actions">
          <button class="btn" @click="accessModalId = null" data-testid="access-cancel">{{ t('common.cancel') }}</button>
          <button class="btn btn-primary" :disabled="accessBusy" @click="saveAccess" data-testid="access-save">
            {{ accessBusy ? '…' : t('apps.privSave') }}
          </button>
        </div>
      </div>
    </div>

    <!-- ===================== PUBLISH MODAL ===================== -->
    <div v-if="publishModalId" class="modal-back" @click.self="publishModalId = ''" data-testid="publish-modal">
      <div class="modal panel">
        <h2 class="modal-title">⬡ {{ t('apps.publishTitle') }}</h2>
        <p class="modal-app dim">// {{ modalApp?.name }}</p>

        <div class="price-box" v-if="!publishFree" data-testid="publish-price-box">
          <span class="dim">{{ t('apps.publishPriceLabel') }}</span>
          <span class="price" data-testid="publish-price">{{ publishCost }} STH</span>
        </div>
        <p v-if="!publishFree && publishInfo" class="size-line dim" data-testid="publish-size-line">
          {{ t('apps.sizeLabel') }}: {{ publishInfo.sizeMb }} {{ t('apps.mb') }} · {{ t('apps.costLabel') }}: {{ publishCost }} STH ({{ t('apps.baseAndWeight') }})
        </p>
        <p v-if="!publishFree" class="paid-note dim">{{ t('apps.publishPaidNote') }}</p>

        <label class="free-row">
          <input type="checkbox" v-model="publishFree" data-testid="publish-free-checkbox" />
          <span>{{ t('apps.freeCheckbox') }}</span>
        </label>

        <div v-if="publishFree" class="warn" data-testid="publish-free-warning">
          <div class="warn-title">⚠ {{ t('apps.freeWarningTitle') }}</div>
          <p>{{ t('apps.freeWarning') }}</p>
        </div>

        <div class="modal-actions">
          <button class="btn" @click="publishModalId = ''" data-testid="publish-cancel">{{ t('common.cancel') }}</button>
          <button class="btn btn-primary" @click="confirmPublish" data-testid="publish-confirm">
            {{ publishFree ? t('apps.publishFree') : t('apps.payAndPublish') }}
          </button>
        </div>
      </div>
    </div>

    <!-- ===================== BOOST MODAL ===================== -->
    <div v-if="boostModalId" class="modal-back" @click.self="boostModalId = ''" data-testid="boost-modal">
      <div class="modal panel">
        <h2 class="modal-title boost">⚡ {{ t('apps.boostTitle') }}</h2>
        <p class="modal-app dim">// {{ boostApp?.name }}</p>
        <p class="paid-note dim">{{ t('apps.boostNote') }}</p>
        <label class="fld">
          <span class="dim boost-lbl">{{ t('apps.boostAmount') }}</span>
          <input
            v-model="boostAmount"
            class="field"
            type="number"
            min="1"
            step="1"
            placeholder="10"
            data-testid="boost-amount-input"
          />
        </label>
        <div class="modal-actions">
          <button class="btn" @click="boostModalId = ''" data-testid="boost-cancel">{{ t('common.cancel') }}</button>
          <button class="btn btn-boost" :disabled="!(Number(boostAmount) > 0)" @click="confirmBoost" data-testid="boost-confirm">
            ⚡ {{ t('apps.boostConfirm') }}
          </button>
        </div>
      </div>
    </div>

    <!-- ===================== UPDATE MODAL (dynamic fee) ===================== -->
    <Teleport to="body">
    <div v-if="updateModalId" class="modal-back" @click.self="updateModalId = ''" data-testid="update-modal">
      <div class="modal panel">
        <h2 class="modal-title">↻ {{ t('apps.updateTitle') }}</h2>
        <p class="modal-app dim">// {{ updateApp_?.name }}</p>
        <div class="price-box" v-if="updateInfo" data-testid="update-price-box">
          <span class="dim">{{ t('apps.updatePriceLabel') }}</span>
          <span class="price" data-testid="update-price">{{ updateInfo.cost }} STH</span>
        </div>
        <p v-if="updateInfo" class="size-line dim" data-testid="update-size-line">
          {{ t('apps.sizeLabel') }}: {{ updateInfo.sizeMb }} {{ t('apps.mb') }} · {{ t('apps.costLabel') }}: {{ updateInfo.cost }} STH ({{ t('apps.baseAndWeight') }})
        </p>
        <code v-if="updateInfo" class="staged-path" data-testid="update-path">{{ updateInfo.path }}</code>
        <p class="paid-note dim">{{ t('apps.updatePaidNote') }}</p>
        <div class="modal-actions">
          <button class="btn" @click="updateModalId = ''" data-testid="update-cancel">{{ t('common.cancel') }}</button>
          <button class="btn btn-primary" @click="confirmUpdate" data-testid="update-confirm">
            {{ t('apps.payAndUpdate') }}
          </button>
        </div>
      </div>
    </div>
    </Teleport>

    <!-- ===================== EDIT MODAL ===================== -->
    <div v-if="editModalId" class="modal-back" @click.self="editModalId = ''" data-testid="edit-modal">
      <div class="modal panel edit-modal">
        <h2 class="modal-title">✎ {{ t('apps.editTitle') }}</h2>
        <p class="modal-app dim">// {{ editApp_?.name }}</p>

        <label class="field-label">{{ t('apps.fldTitle') }}</label>
        <input v-model="editName" class="field" :placeholder="t('apps.fldTitle')" data-testid="edit-name" />

        <label class="field-label">{{ t('apps.fldDesc') }}</label>
        <textarea v-model="editDescription" class="field desc-field" rows="3" :placeholder="t('apps.fldDesc')" data-testid="edit-desc" />

        <label class="field-label">{{ t('apps.fldTags') }}</label>
        <input v-model="editTags" class="field" placeholder="dex, defi, sth" data-testid="edit-tags" />

        <label class="field-label">{{ t('apps.fldLogo') }}</label>
        <div class="logo-row">
          <button class="btn" @click="editLogoInput?.click()" data-testid="edit-logo-pick">{{ t('apps.logoPick') }}</button>
          <span class="dim logo-name">{{ editLogoName || t('apps.logoKeep') }}</span>
          <input ref="editLogoInput" type="file" accept=".png,.svg,image/png,image/svg+xml" class="hidden-input" @change="onEditLogoPick" />
        </div>

        <label class="field-label">{{ t('apps.fldPreview') }}</label>
        <div class="logo-row">
          <button class="btn" @click="editPreviewInput?.click()" data-testid="edit-preview-pick">{{ t('apps.previewPick') }}</button>
          <span class="dim logo-name">{{ editPreviewName || t('apps.previewKeep') }}</span>
          <input ref="editPreviewInput" type="file" accept="image/*" class="hidden-input" @change="onEditPreviewPick" />
        </div>

        <!-- live preview: how the card looks in "find apps" -->
        <span class="field-label preview-label">{{ t('apps.livePreview') }}</span>
        <div class="prev-card" data-testid="edit-preview-card">
          <div class="prev-top">
            <span class="prev-icon">
              <img v-if="editLogoPreview" :src="editLogoPreview" alt="" />
              <Identicon v-else :seed="editModalId" :size="24" />
            </span>
            <span class="prev-name">// {{ editName || t('discover.noTitle') }}</span>
          </div>
          <p class="prev-desc">{{ editDescription || t('profile.noDescription') }}</p>
          <div class="prev-link">sth://{{ shortAddr(editModalId, 10, 8) }}</div>
          <div class="prev-actions"><span class="prev-btn">{{ t('discover.open') }} →</span></div>
          <div v-if="editTagList.length" class="prev-tags">
            <span v-for="tg in editTagList.slice(0, 5)" :key="tg" class="prev-tag">#{{ tg }}</span>
          </div>
        </div>

        <div class="modal-actions edit-actions">
          <button class="btn" @click="editModalId = ''" data-testid="edit-cancel">{{ t('common.cancel') }}</button>
          <button class="btn btn-secondary" @click="replaceFiles" data-testid="edit-replace-files">{{ t('apps.replaceFiles') }} ↻</button>
          <button class="btn btn-primary" :disabled="apps.busy" @click="saveEdit" data-testid="edit-save">{{ t('apps.saveFree') }} ✓</button>
        </div>
        <p class="paid-note dim">{{ t('apps.editHint') }}</p>
      </div>
    </div>

    <AboutModal v-if="aboutId" :app-id="aboutId" :title="aboutTitle" @close="aboutId = ''" />

    <!-- кроп любой картинки в кадр DAPP Preview 440×180 (публикация + EDIT) -->
    <PreviewCropModal
      v-if="cropFile"
      :file="cropFile"
      @apply="onCropApply"
      @cancel="cropFile = null"
      @decodefail="onCropDecodeFail"
    />
  </div>
</template>

<style scoped>
.apps {
  max-width: 1400px;
  display: flex;
  flex-direction: column;
  gap: 18px;
}
.page-title {
  font-size: 15px;
  letter-spacing: 0.14em;
  color: var(--accent);
  font-weight: var(--fw-700);
}
/* P5: header row aligns title + "История выплат" call-to-action on one line. */
.apps-hero {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 16px;
  flex-wrap: wrap;
}
.tabs {
  display: flex;
  gap: 6px;
  border-bottom: 1px solid var(--border);
}
.search-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.search-row:focus-within {
  border-color: var(--border-accent);
}
.search-ic {
  font-size: 14px;
  color: var(--text-dim);
}
.search-input {
  flex: 1;
  background: none;
  border: none;
  outline: none;
  font-size: 12px;
  color: var(--text);
}
.search-input::placeholder {
  color: var(--text-dim);
}
.search-clear {
  background: none;
  border: 1px solid var(--border);
  color: var(--text-dim);
  font-size: 10px;
  padding: 2px 7px;
  border-radius: 999px;
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease;
}
.search-clear:hover {
  color: var(--danger);
  border-color: var(--danger);
}
.tab {
  background: none;
  border: none;
  border-bottom: 2px solid transparent;
  color: var(--text-dim);
  font-size: 12px;
  font-weight: var(--fw-700);
  letter-spacing: 0.08em;
  padding: 10px 14px;
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease;
}
.tab:hover {
  color: var(--text);
}
.tab.on {
  color: var(--accent);
  border-bottom-color: var(--accent);
}
.tab .cnt {
  font-size: 10px;
  padding: 1px 6px;
  margin-left: 4px;
  background: var(--bg-elevated);
  border-radius: 999px;
  color: var(--text-dim);
}
.tab-cta {
  margin-left: auto;
  color: var(--accent);
}
.pane {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.drop {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
  padding: 44px 24px;
  background: var(--panel);
  border: 1px dashed var(--border-accent);
  border-radius: var(--radius);
  transition: background-color 0.2s ease, transform 0.2s ease;
}
.drop.active {
  background: var(--accent-soft);
  transform: scale(1.005);
}
.drop.staged {
  border-style: solid;
  border-color: var(--accent);
  background: var(--accent-soft);
  align-items: stretch;
  padding: 18px 20px;
}
.staged-box {
  display: flex;
  flex-direction: column;
  gap: 8px;
  text-align: left;
  width: 100%;
}
.staged-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.staged-ok {
  color: #34d399;
  font-size: 12px;
  font-weight: var(--fw-800);
  letter-spacing: 0.08em;
}
.staged-clear {
  background: none;
  border: 1px solid var(--border);
  color: var(--text-dim);
  font-size: 10px;
  letter-spacing: 0.06em;
  padding: 3px 9px;
  border-radius: 999px;
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease;
}
.staged-clear:hover {
  color: var(--danger);
  border-color: var(--danger);
}
.staged-name {
  font-size: 14px;
  font-weight: var(--fw-700);
  color: var(--accent);
  letter-spacing: 0.04em;
}
.staged-path {
  font-size: 11px;
  color: var(--text-dim);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  padding: 7px 9px;
  word-break: break-all;
}
.staged-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 18px;
  font-size: 12px;
  color: var(--text-dim);
}
.staged-cost {
  color: #34d399;
  font-weight: var(--fw-700);
}
.size-line {
  font-size: 11px;
  letter-spacing: 0.04em;
  margin-top: -6px;
}
.drop-icon {
  font-size: 32px;
  color: var(--accent);
}
.drop.busy .drop-icon {
  animation: spin 1s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}
.drop-main {
  font-size: 14px;
  font-weight: var(--fw-700);
  letter-spacing: 0.1em;
}
.drop-sub {
  font-size: 12px;
}
.controls {
  display: flex;
  gap: 12px;
  align-items: flex-start;
  flex-wrap: wrap;
}
.controls .field {
  flex: 1;
  min-width: 200px;
}
.desc-field {
  resize: vertical;
  font-family: inherit;
  line-height: 1.5;
}
.req-note {
  font-size: 11px;
  margin-top: -8px;
}
.logo-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}
.logo-name {
  font-size: 11px;
  color: var(--accent);
}
.logo-hint {
  font-size: 11px;
  flex-basis: 100%;
}
.tags-row {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.tag-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.tag-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 11px;
  padding: 4px 8px;
  background: var(--bg-elevated);
  border: 1px solid var(--border-accent);
  border-radius: 999px;
  color: var(--accent);
}
.tag-x {
  background: none;
  border: none;
  color: var(--text-dim);
  cursor: pointer;
  font-size: 10px;
}
.tag-x:hover {
  color: var(--danger);
}
.tag-input {
  max-width: 340px;
}
.hidden-input {
  display: none;
}
.flash {
  font-size: 12px;
  letter-spacing: 0.06em;
}
.app-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.app-card {
  padding: 16px 18px;
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: 16px;
  border-left: 2px solid var(--accent);
  position: relative;
}
/* Если у карточки есть preview-обложка справа — раскладываем 70/30.
   :has() поддерживается всеми целевыми браузерами (WebView2, Safari 15.4+).
   Fallback (`.app-card > .card-preview`) для WebView-ов без :has() не нужен —
   Tauri v2 использует свежий Chromium/WebView2, где :has уже давно доступен. */
.app-card:has(> .card-preview) {
  grid-template-columns: minmax(0, 7fr) minmax(0, 3fr);
  align-items: start;
}
.card-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
  min-width: 0;
}
.card-preview {
  /* Стандартизированный контейнер: строго 440×180 (aspect-ratio),
     align-self: start — превью не растягивается по высоте карточки,
     даже если контент слева выше (теги, доп. метаданные). Все обложки
     в списке имеют одинаковый визуальный размер и не «вылазят» за
     границы карточки. */
  align-self: start;
  width: 100%;
  aspect-ratio: 440 / 180;
  border-radius: 10px;
  overflow: hidden;
  background: rgba(6, 10, 16, 0.4);
  border: 1px solid var(--border);
  position: relative;
}
.card-preview img {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}
/* Узкие экраны: preview уходит под контент и растягивается на всю ширину. */
@media (max-width: 900px) {
  .app-card:has(> .card-preview) {
    grid-template-columns: minmax(0, 1fr);
  }
}
.del-overlay {
  position: absolute;
  inset: 0;
  z-index: 5;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 14px;
  background: rgba(6, 10, 16, 0.82);
  backdrop-filter: blur(3px);
  border-radius: inherit;
}
.del-text {
  color: #fca5a5;
  font-size: 14px;
  letter-spacing: 1px;
}
.del-spin {
  width: 20px;
  height: 20px;
  border: 2px solid rgba(252, 165, 165, 0.25);
  border-top-color: #fca5a5;
  border-radius: 50%;
  animation: del-rot 0.7s linear infinite;
}
@keyframes del-rot {
  to {
    transform: rotate(360deg);
  }
}
.app-card.draft {
  border-left-color: var(--secondary, #fbbf24);
}
.card-head {
  display: flex;
  gap: 12px;
  align-items: flex-start;
}
.logo-badge {
  width: 46px;
  height: 46px;
  flex: 0 0 46px;
  border-radius: 10px;
  overflow: hidden;
  display: flex;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--border);
}
.logo-badge img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}
.logo-badge.gen {
  font-size: 16px;
  font-weight: var(--fw-800);
  letter-spacing: 0.05em;
}
.head-text {
  flex: 1;
  min-width: 0;
}
.app-top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.app-name {
  font-size: 14px;
  font-weight: var(--fw-700);
  color: var(--accent);
}
.ver {
  font-size: 10px;
}
.badge {
  font-size: 9px;
  font-weight: var(--fw-800);
  letter-spacing: 0.08em;
  padding: 3px 8px;
  border-radius: 999px;
  white-space: nowrap;
}
.badge-paid {
  color: #34d399;
  background: rgba(52, 211, 153, 0.12);
  border: 1px solid rgba(52, 211, 153, 0.3);
}
.badge-free {
  color: #fbbf24;
  background: rgba(251, 191, 36, 0.12);
  border: 1px solid rgba(251, 191, 36, 0.3);
}
.badge-draft {
  color: var(--text-dim);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
}
.badge-priv {
  color: #d946ef;
  background: rgba(217, 70, 239, 0.1);
  border: 1px solid rgba(217, 70, 239, 0.35);
}
/* --- Private dApp: чекбокс + список адресов --- */
.priv-row {
  margin-top: 14px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.priv-check {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--text-dim);
  cursor: pointer;
  user-select: none;
  transition: color 0.15s ease;
}
.priv-check:hover {
  color: #d946ef;
}
.priv-check input {
  accent-color: #d946ef;
}
.priv-lock {
  font-size: 13px;
}
.priv-field {
  width: 100%;
  resize: vertical;
  font-family: inherit;
  font-size: 12px;
  line-height: 1.6;
}
.priv-hint {
  font-size: 11px;
  line-height: 1.5;
}
.app-desc {
  font-size: 12px;
  line-height: 1.5;
  margin-top: 4px;
}
.card-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.card-tag {
  font-size: 10px;
  padding: 2px 7px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--text-dim);
}
.app-url-row {
  display: flex;
  align-items: center;
  gap: 10px;
}
.app-url {
  flex: 1;
  font-size: 12px;
  padding: 8px 10px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  word-break: break-all;
}
.copy-btn {
  padding: 8px 12px;
  white-space: nowrap;
}
.direct-btn {
  color: var(--accent);
  border-color: var(--accent);
}
/* ссылка ещё не готова (идёт сидинг) — серая некликабельная кнопка */
.direct-btn:disabled {
  color: var(--dim, #6b7684);
  border-color: color-mix(in srgb, var(--dim, #6b7684) 45%, transparent);
  opacity: 0.55;
  cursor: not-allowed;
}
.app-meta {
  display: flex;
  gap: 22px;
  flex-wrap: wrap;
  font-size: 11px;
}
.app-meta code {
  font-size: 11px;
}
.app-meta .author-link {
  cursor: pointer;
  color: var(--accent);
  font-weight: var(--fw-700);
  border-bottom: 1px dashed transparent;
  transition: border-color 0.15s ease;
}
.app-meta .author-link:hover {
  border-bottom-color: var(--accent);
}
.app-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  padding-top: 4px;
  border-top: 1px solid var(--border);
}
.app-actions .btn {
  padding: 8px 12px;
}
.empty {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 18px;
  font-size: 12px;
}
.empty-mark {
  color: var(--secondary);
  letter-spacing: 0.1em;
}
button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
/* publish modal */
.modal-back {
  position: fixed;
  inset: 0;
  background: rgba(3, 6, 10, 0.72);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 200;
}
.modal {
  width: 460px;
  max-width: 92vw;
  padding: 24px 26px;
  display: flex;
  flex-direction: column;
  gap: 14px;
  border-top: 2px solid var(--accent);
}
.modal-title {
  font-size: 14px;
  font-weight: var(--fw-800);
  letter-spacing: 0.1em;
  color: var(--accent);
}
.edit-modal {
  gap: 8px;
}
.field-label {
  font-size: 10px;
  font-weight: var(--fw-700);
  letter-spacing: 0.08em;
  color: var(--text-dim);
  margin-top: 6px;
}
.edit-actions {
  margin-top: 12px;
  flex-wrap: wrap;
}
.preview-label {
  margin-top: 12px;
}
.prev-card {
  margin-top: 6px;
  padding: 12px 14px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--bg);
}
.prev-top {
  display: flex;
  align-items: center;
  gap: 8px;
}
.prev-icon {
  width: 24px;
  height: 24px;
  border-radius: 6px;
  overflow: hidden;
  line-height: 0;
  flex: none;
}
.prev-icon img {
  width: 24px;
  height: 24px;
  object-fit: cover;
  border-radius: 6px;
}
.prev-name {
  font-size: 13px;
  font-weight: var(--fw-700);
  color: var(--accent);
}
.prev-desc {
  font-size: 12px;
  color: var(--text-dim);
  margin: 8px 0;
}
.prev-link {
  font-family: monospace;
  font-size: 11px;
  color: var(--text-dim);
}
.prev-actions {
  margin-top: 8px;
}
.prev-btn {
  display: inline-block;
  padding: 4px 12px;
  font-size: 11px;
  font-weight: var(--fw-700);
  color: #04121a;
  background: var(--accent);
  border-radius: 6px;
}
.prev-tags {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
  margin-top: 10px;
  padding-top: 8px;
  border-top: 1px solid var(--border);
}
.prev-tag {
  font-size: 10px;
  color: var(--text-dim);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 5px;
  padding: 1px 6px;
}
.btn-secondary {
  border-color: rgba(96, 165, 250, 0.45) !important;
  color: #60a5fa !important;
}
.btn-secondary:hover {
  background: rgba(96, 165, 250, 0.1) !important;
}
.modal-title.boost {
  color: #fbbf24;
}
.btn-boost {
  color: #fbbf24 !important;
  border-color: rgba(251, 191, 36, 0.4) !important;
}
.btn-boost:hover {
  background: rgba(251, 191, 36, 0.1) !important;
}
.boost-lbl {
  font-size: 12px;
  display: block;
  margin-bottom: 6px;
}
.modal-app {
  font-size: 12px;
  margin-top: -6px;
}
.price-box {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 16px;
  background: var(--bg-elevated);
  border: 1px solid var(--border-accent);
  border-radius: var(--radius);
}
.price {
  font-size: 20px;
  font-weight: var(--fw-800);
  color: var(--accent);
}
.paid-note {
  font-size: 11px;
  line-height: 1.6;
}
.free-row {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 12px;
  cursor: pointer;
}
.warn {
  border: 1px solid rgba(251, 191, 36, 0.4);
  background: rgba(251, 191, 36, 0.08);
  border-radius: var(--radius);
  padding: 12px 14px;
}
.warn-title {
  color: #fbbf24;
  font-weight: var(--fw-800);
  font-size: 12px;
  letter-spacing: 0.06em;
  margin-bottom: 6px;
}
.warn p {
  font-size: 11.5px;
  line-height: 1.6;
  color: var(--text-dim);
}
.modal-actions {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  padding-top: 4px;
}
</style>
