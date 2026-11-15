<template>
  <section class="h-full flex flex-col overflow-hidden" data-testid="reader-view">
    <!-- Empty state -->
    <div v-if="!email" class="flex-1 grid place-items-center p-8" data-testid="reader-empty">
      <div class="text-center max-w-sm">
        <div class="w-16 h-16 mx-auto mb-4 rounded-lg panel grid place-items-center">
          <Mail class="w-7 h-7 text-iron-200" />
        </div>
        <div class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 mb-2">// idle</div>
        <h2 class="text-2xl font-semibold mb-1">Select a thread.</h2>
        <p class="text-iron-200 text-sm">
          Encrypted payloads will request your local key before unsealing.
        </p>
      </div>
    </div>

    <template v-else>
      <!-- Header -->
      <div class="hairline border-b px-4 sm:px-6 py-4 panel rounded-none">
        <div class="flex items-center gap-2 mb-3">
          <button class="btn-ghost lg:hidden !px-2" @click="emit('back')" data-testid="reader-back-btn">
            <ArrowLeft class="w-4 h-4" />
          </button>
          <button class="btn-ghost !px-2" @click="mail.toggleStar(email.id)" :title="email.starred ? 'Unstar' : 'Star'" data-testid="star-btn">
            <Star class="w-4 h-4" :class="email.starred ? 'text-rust fill-rust' : ''" />
          </button>
          <button class="btn-ghost !px-2" @click="mail.trash(email.id)" title="Move to trash" data-testid="trash-btn">
            <Trash2 class="w-4 h-4" />
          </button>
          <div class="flex-1" />
          <button class="btn-ghost text-xs" @click="emit('reply')" data-testid="reply-btn">
            <Reply class="w-4 h-4" /> <span class="hidden sm:inline">Reply</span>
          </button>
          <button class="btn-ghost text-xs" @click="emit('forward')" data-testid="forward-btn">
            <Forward class="w-4 h-4" /> <span class="hidden sm:inline">Forward</span>
          </button>
        </div>

        <div class="flex items-start gap-3 mb-2">
          <h1 class="text-xl sm:text-2xl font-semibold tracking-tight flex-1">{{ displaySubject }}</h1>
          <span v-if="verifyBadge" class="chip shrink-0" :class="verifyBadge.cls" :title="verifyBadge.hint" data-testid="verify-badge">
            {{ verifyBadge.label }}
          </span>
          <span v-if="email.encrypted" class="chip border-copper/40 text-copper shrink-0">
            <Shield class="w-3 h-3" /> OXID·ENC
          </span>
          <span v-else class="chip border-white/15 text-iron-200 shrink-0">
            <Mail class="w-3 h-3" /> CLASSIC
          </span>
        </div>

        <div class="flex items-center justify-between flex-wrap gap-2 text-xs">
          <div class="font-mono text-iron-200 flex items-center gap-1.5">
            <span class="text-iron-50">{{ email.from.name }}</span>
            <span class="text-iron-200"> · {{ email.from.email }}</span>
            <span v-if="senderInContacts" class="text-emerald-400" title="Sender is in your contacts" data-testid="sender-in-contacts">
              <UserCheck class="w-3.5 h-3.5 inline" />
            </span>
            <button
              v-else
              class="btn-ghost !px-1.5 !py-0.5"
              @click="addSenderToContacts"
              :disabled="addingSender"
              title="Add sender to contacts (pins address if verified)"
              data-testid="add-sender-contact-btn"
            >
              <UserPlus class="w-3.5 h-3.5" />
            </button>
          </div>
          <div class="font-mono text-iron-200">{{ formatFull(email.date) }}</div>
        </div>
        <div class="font-mono text-[11px] text-iron-200 mt-1">
          to: <span class="text-iron-100">{{ email.to }}</span>
        </div>
      </div>

      <!-- Body -->
      <div class="flex-1 overflow-y-auto px-4 sm:px-6 py-6">
        <div v-if="email.encrypted && !mail.isDecrypted(email.id)" class="space-y-4" data-testid="encrypted-body">
          <div class="panel rounded-lg p-4 border border-copper/20">
            <div class="flex items-center gap-2 mb-2">
              <Shield class="w-4 h-4 text-copper" />
              <span class="font-mono text-[10px] uppercase tracking-[0.22em] text-copper">SmartHoldem · Sealed</span>
            </div>
            <p class="text-sm text-iron-100">
              This payload is sealed with the sender's SmartHoldem public key.
              Unsealing requires your local seed.
            </p>
          </div>

          <pre class="encrypted-text font-mono text-[11px] leading-relaxed text-iron-300/60 break-all whitespace-pre-wrap select-none">{{ email.cipher }}</pre>

          <button class="btn-rust" @click="mail.decrypt(email.id)" data-testid="decrypt-btn">
            <KeyRound class="w-4 h-4" /> Decrypt with local key
          </button>
        </div>

        <div v-else data-testid="decrypted-body" class="prose-oxid h-full flex flex-col">
          <div v-if="email.encrypted" class="mb-4 flex items-center gap-2">
            <span class="chip border-copper/40 text-copper">
              <LockOpen class="w-3 h-3" /> UNSEALED
            </span>
            <span v-if="dec && dec.senderAddress" class="chip border-white/15 text-iron-200 font-mono text-[10px]" data-testid="sender-sth">
              from {{ dec.senderAddress }}
            </span>
            <button class="btn-ghost text-xs !px-2 !py-1" @click="mail.relock(email.id)" data-testid="relock-btn">
              <Lock class="w-3.5 h-3.5" /> Re-seal
            </button>
          </div>

          <!-- Вложения: для NTFRY-конверта берём распакованный список из dec (inline base64),
               для обычных писем — метаданные из StoredMessage (данные тянутся с сервера/store) -->
          <div
            v-if="attachmentList.length"
            class="mb-4 flex flex-wrap gap-2"
            data-testid="attachments-list"
          >
            <button
              v-for="(att, i) in attachmentList"
              :key="i"
              class="chip text-[11px] cursor-pointer hover:border-rust/60 hover:text-rust transition-colors"
              :title="isPreviewable(att) ? `Preview ${att.name}` : `Download ${att.name}`"
              @click="openAttachmentAt(i)"
              :data-testid="`attachment-${i}`"
            >
              <span v-if="isPreviewable(att)">👁</span><span v-else>⇩</span>
              {{ att.name }} · {{ fmtSize(att.size) }}
            </button>
          </div>

          <!-- HTML/TEXT переключатель для писем с HTML-версткой -->
          <div v-if="!email.encrypted && email.bodyHtml" class="mb-3 flex items-center gap-1">
            <button
              class="chip text-[10px] cursor-pointer"
              :class="viewHtml ? 'border-rust/60 text-rust' : 'text-iron-200'"
              @click="viewHtml = true"
              data-testid="view-html-btn"
            >HTML</button>
            <button
              class="chip text-[10px] cursor-pointer"
              :class="!viewHtml ? 'border-rust/60 text-rust' : 'text-iron-200'"
              @click="viewHtml = false"
              data-testid="view-text-btn"
            >TEXT</button>
          </div>

          <!-- HTML: sandboxed iframe (скрипты запрещены; клики по ссылкам перехватываются и уходят в системный браузер) -->
          <iframe
            v-if="!email.encrypted && email.bodyHtml && viewHtml"
            :srcdoc="htmlDoc"
            sandbox="allow-same-origin"
            class="flex-1 w-full min-h-[420px] rounded-md border-0 bg-white"
            @load="wireIframeLinks"
            data-testid="html-body"
          ></iframe>
          <pre v-else class="font-sans text-[15px] leading-relaxed whitespace-pre-wrap text-iron-50"><template v-for="(seg, i) in bodySegments" :key="i"><a v-if="seg.url" href="#" class="text-rust underline underline-offset-2 hover:text-copper break-all" @click.prevent="openExternal(seg.text)" :data-testid="`body-link-${i}`">{{ seg.text }}</a><template v-else>{{ seg.text }}</template></template></pre>
        </div>
      </div>
    </template>

    <!-- Оверлей предпросмотра: изображения через <img>, PDF через <iframe> с blob-URL.
         Данные тянутся из inline NTFRY-конверта или из SMTP-attachment endpoint без
         сохранения на диск. Esc / клик по фону / кнопка «Закрыть» — закрывают.
         ВАЖНО: блок живёт ВНУТРИ корневого <section> (single-root!) — компонент
         обёрнут в <transition mode="out-in"> в Mailbox.vue, а фрагмент-рут ломал
         монтирование и письма не открывались. Teleport выносит оверлей в body. -->
    <Teleport to="body">
      <div
        v-if="preview"
        class="fixed inset-0 z-[80] bg-black/85 backdrop-blur-sm flex flex-col p-4 sm:p-6"
        data-testid="attachment-preview"
        @click.self="closePreview"
        @keydown.esc="closePreview"
        tabindex="-1"
        ref="previewRoot"
      >
        <div class="flex items-center gap-2 mb-3 flex-shrink-0">
          <span class="font-mono text-[11px] uppercase tracking-[0.22em] text-iron-100 flex-1 truncate">
            {{ preview.name }} · {{ fmtSize(preview.size) }}
          </span>
          <button
            class="btn-ghost text-xs"
            @click="downloadPreview"
            data-testid="attachment-preview-download"
          >
            <Download class="w-4 h-4" /> <span class="hidden sm:inline">Скачать</span>
          </button>
          <button
            class="btn-ghost text-xs"
            @click="closePreview"
            data-testid="attachment-preview-close"
          >
            <X class="w-4 h-4" /> <span class="hidden sm:inline">Закрыть</span>
          </button>
        </div>
        <div class="flex-1 min-h-0 grid place-items-center overflow-hidden">
          <img
            v-if="preview.kind === 'image'"
            :src="preview.url"
            :alt="preview.name"
            class="max-h-full max-w-full object-contain rounded-md shadow-2xl"
            data-testid="attachment-preview-image"
          />
          <iframe
            v-else-if="preview.kind === 'pdf'"
            :src="preview.url"
            class="w-full h-full bg-white rounded-md border-0"
            data-testid="attachment-preview-pdf"
            title="PDF preview"
          ></iframe>
        </div>
      </div>
    </Teleport>
  </section>
</template>

<script setup>
import { computed, ref, watch, onMounted, onBeforeUnmount, nextTick } from 'vue'
import {
  Mail, Star, Trash2, Reply, Forward, Shield, KeyRound,
  Lock, LockOpen, ArrowLeft, UserPlus, UserCheck, Download, X,
} from 'lucide-vue-next'
import { useMailStore } from '../stores/mail'
import { useContactsStore } from '../stores/contacts'
import { openExternal, api } from '../lib/backend'
import { useThemeStore } from '../stores/theme'

const emit = defineEmits(['back', 'reply', 'forward'])
const mail = useMailStore()
const theme = useThemeStore()
const email = computed(() => mail.selected)

const decrypting = ref(false)
const decryptError = ref('')

// Расшифрованный вид NTFRY-письма
const dec = computed(() => (email.value ? mail.decryptedView(email.value.id) : null))

const displaySubject = computed(() => {
  if (email.value?.encrypted && dec.value) return dec.value.subject || '(no subject)'
  return email.value?.subject || '(no subject)'
})

const displayBody = computed(() => {
  if (email.value?.encrypted && dec.value) return dec.value.text
  return email.value?.body
})

// Кликабельные http(s)-ссылки в plain-тексте (открываются в системном браузере)
const URL_RE = /(https?:\/\/[^\s<>"')\]]+)/g
const bodySegments = computed(() => {
  const text = displayBody.value || ''
  const out = []
  let last = 0
  for (const m of text.matchAll(URL_RE)) {
    if (m.index > last) out.push({ text: text.slice(last, m.index), url: false })
    out.push({ text: m[0], url: true })
    last = m.index + m[0].length
  }
  if (last < text.length) out.push({ text: text.slice(last), url: false })
  return out
})

// Перехват кликов внутри HTML-iframe (same-origin без allow-scripts — безопасно)
function wireIframeLinks(e) {
  const doc = e.target?.contentDocument
  if (!doc) return
  doc.addEventListener('click', (ev) => {
    const a = ev.target && ev.target.closest ? ev.target.closest('a[href]') : null
    if (!a) return
    ev.preventDefault()
    openExternal(a.getAttribute('href'))
  })
}

// «В контакты» из открытого письма: если отправитель верифицирован —
// закрепляем и его STH-адрес (короткая форма ADDR:email)
const contactsStore = useContactsStore()
const addingSender = ref(false)
const senderInContacts = computed(() => {
  const from = email.value?.from?.email
  if (!from) return false
  const norm = from.trim().toLowerCase()
  return contactsStore.list.some((c) => c.email === norm)
})
async function addSenderToContacts() {
  const from = email.value?.from
  if (!from?.email || addingSender.value) return
  addingSender.value = true
  try {
    const res = email.value.encrypted ? dec.value : mail.verifyResult(email.value.id)
    const addr = res?.senderAddress
    const input = addr ? `${addr}:${from.email}` : from.email
    await contactsStore.add(input, from.name || '')
  } catch (e) {
    decryptError.value = e.message
  } finally {
    addingSender.value = false
  }
}
onMounted(() => contactsStore.load())

async function doDecrypt() {
  decrypting.value = true
  decryptError.value = ''
  try {
    await mail.decrypt(email.value.id)
  } catch (e) {
    decryptError.value = e.message
  } finally {
    decrypting.value = false
  }
}

// Бейдж подлинности: NTFRY (из dec.verified) или X-Ntfry-Sig обычного письма.
// source==='local' — доверие из адресной книги (Local Trust), приоритетнее чейна.
const BADGES = {
  verified: { label: '✔ VERIFIED SENDER', cls: 'border-emerald-500/50 text-emerald-400', hint: 'Подпись сходится с привязкой email↔адрес в блокчейне SmartHoldem' },
  verified_local: { label: '✔ TRUSTED CONTACT', cls: 'border-emerald-500/50 text-emerald-400', hint: 'Отправитель совпадает с вашим локальным доверенным контактом' },
  mismatch: { label: '⚠ SENDER MISMATCH', cls: 'border-rust/60 text-rust', hint: 'Ключ отправителя НЕ совпадает с привязкой в реестре — возможен spoofing' },
  mismatch_local: { label: '⚠ CONTACT MISMATCH', cls: 'border-rust/60 text-rust', hint: 'Ключ отправителя НЕ совпадает с вашим локальным контактом — возможен spoofing!' },
  invalid: { label: '⚠ BAD SIGNATURE', cls: 'border-rust/60 text-rust', hint: 'Криптографическая подпись письма не сходится' },
  unknown: { label: '· UNREGISTERED', cls: 'border-white/15 text-iron-200', hint: 'Отправитель не найден ни в контактах, ни в реестре SmartHoldem' },
}
function pickBadge(res) {
  if (!res) return null
  const key = res.source === 'local' && BADGES[`${res.verified}_local`] ? `${res.verified}_local` : res.verified
  return BADGES[key] || null
}
const verifyBadge = computed(() => {
  if (!email.value) return null
  if (email.value.encrypted) {
    return dec.value ? pickBadge(dec.value) : null
  }
  const res = mail.verifyResult(email.value.id)
  if (!res || res.verified === 'none') return null
  return pickBadge(res)
})

// Автопроверка X-Ntfry-Sig при открытии обычного письма
watch(
  () => email.value?.id,
  (id) => {
    if (!id) return
    const e = email.value
    if (!e.encrypted && e.ntfrySig && !mail.verifyResult(id)) mail.verifyPlain(id)
  },
  { immediate: true }
)

// Скачивание вложения из расшифрованного NTFRY-конверта (inline base64)
function downloadInline(att) {
  const bytes = Uint8Array.from(atob(att.dataB64), (c) => c.charCodeAt(0))
  const blob = new Blob([bytes], { type: att.mime || 'application/octet-stream' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = att.filename || 'attachment'
  document.body.appendChild(a)
  a.click()
  a.remove()
  URL.revokeObjectURL(url)
}

// Единый список вложений для отображения:
//  - encrypted (NTFRY / P2P): берём из расшифрованного конверта (inline base64),
//  - обычное письмо: метаданные из StoredMessage (данные подгружаются с сервера/store).
const attachmentList = computed(() => {
  if (email.value?.encrypted && dec.value?.attachments?.length) {
    return dec.value.attachments.map((a) => ({
      name: a.filename,
      mime: a.mime,
      size: a.dataB64 ? Math.floor(a.dataB64.length * 0.75) : 0,
      inline: true,
      raw: a,
    }))
  }
  return (email.value?.attachments || []).map((a) => ({
    name: a.name,
    mime: a.mime,
    size: a.size,
    inline: false,
    raw: a,
  }))
})

function downloadAttachmentAt(i) {
  const item = attachmentList.value[i]
  if (!item) return
  if (item.inline) {
    downloadInline(item.raw)
  } else {
    mail.downloadAttachment(email.value, i)
  }
}

// ---------- Превью вложений (изображения / PDF) ----------
// Оверлей поверх окна письма: изображения через <img>, PDF через <iframe blob>.
// Данные для inline (NTFRY) — base64 из dec.attachments; для SMTP —
// подтягиваем через /accounts/:id/messages/:mid/attachments/:idx без записи на диск.
const preview = ref(null) // { kind: 'image'|'pdf', url, name, size, mime, index }
const previewRoot = ref(null)

function isPreviewable(item) {
  const m = String(item?.mime || '').toLowerCase()
  return m.startsWith('image/') || m === 'application/pdf'
}

function b64ToBlob(b64, mime) {
  const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0))
  return new Blob([bytes], { type: mime || 'application/octet-stream' })
}

async function openAttachmentAt(i) {
  const item = attachmentList.value[i]
  if (!item) return
  if (!isPreviewable(item)) {
    downloadAttachmentAt(i)
    return
  }
  try {
    let blob
    if (item.inline) {
      blob = b64ToBlob(item.raw.dataB64, item.mime)
    } else {
      const att = await api(
        'GET',
        `/accounts/${email.value.accountId}/messages/${email.value.id}/attachments/${i}`,
      )
      blob = b64ToBlob(att.dataBase64, att.mime || item.mime)
    }
    if (preview.value?.url) URL.revokeObjectURL(preview.value.url)
    preview.value = {
      kind: item.mime.toLowerCase().startsWith('image/') ? 'image' : 'pdf',
      url: URL.createObjectURL(blob),
      name: item.name,
      size: item.size || blob.size,
      mime: item.mime,
      index: i,
    }
    await nextTick()
    previewRoot.value?.focus?.()
  } catch (e) {
    decryptError.value = e.message || 'Не удалось открыть предпросмотр'
  }
}

function closePreview() {
  if (preview.value?.url) URL.revokeObjectURL(preview.value.url)
  preview.value = null
}

function downloadPreview() {
  if (!preview.value) return
  downloadAttachmentAt(preview.value.index)
}

// Esc из любого места окна закрывает превью
function onKeydown(ev) {
  if (ev.key === 'Escape' && preview.value) {
    closePreview()
  }
}
onMounted(() => window.addEventListener('keydown', onKeydown))
onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeydown)
  if (preview.value?.url) URL.revokeObjectURL(preview.value.url)
})

// Смена письма — закрываем открытый превью
watch(() => email.value?.id, () => closePreview())

// HTML-режим по умолчанию, если у письма есть HTML-версия; сбрасываем при смене письма
const viewHtml = ref(true)
watch(() => email.value?.id, () => { viewHtml.value = true })

const htmlDoc = computed(() => {
  if (!email.value?.bodyHtml) return ''
  // HTML уже санитизирован в ядре (ammonia: без <script>, on*, javascript:)
  const dark = theme.mode === 'dark'
  const style = dark
    ? 'body{margin:12px;font-family:sans-serif;word-break:break-word;background:#17191d;color:#d9dde3;color-scheme:dark}a{color:#e25822}img{max-width:100%;height:auto}'
    : 'body{margin:12px;font-family:sans-serif;word-break:break-word;background:#ffffff;color:#1a1a1a}a{color:#b3441a}img{max-width:100%;height:auto}'
  return `<!doctype html><html><head><meta charset="utf-8"><style>${style}</style></head><body>${email.value.bodyHtml}</body></html>`
})

function fmtSize(n) {
  if (n > 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + ' MB'
  if (n > 1024) return (n / 1024).toFixed(0) + ' KB'
  return n + ' B'
}

function formatFull(iso) {
  const d = new Date(iso)
  return d.toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' })
}
</script>
