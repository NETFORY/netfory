<template>
  <transition name="fade" appear>
    <div
      class="fixed inset-0 z-50 grid place-items-end sm:place-items-center bg-black/70 backdrop-blur-sm p-0 sm:p-6"
      data-testid="compose-modal"
    >
      <div class="modal-surface w-full sm:max-w-2xl rounded-t-2xl sm:rounded-2xl overflow-hidden shadow-2xl flex flex-col max-h-[90vh] border">
        <!-- Header -->
        <div class="hairline border-b px-5 py-3 flex items-center justify-between modal-header">
          <div class="flex items-center gap-2">
            <component :is="modeIcon" class="w-4 h-4 text-rust" />
            <div class="font-mono text-[11px] uppercase tracking-[0.22em] text-iron-100">
              {{ modeLabel }}
            </div>
            <span
              v-if="initial && initial.mode"
              class="font-mono text-[10px] text-iron-200 ml-2 truncate max-w-[260px]"
            >
              · {{ initial.mode === 'reply' ? `to ${initial.to}` : `subject: ${initial.subject}` }}
            </span>
          </div>
          <button class="btn-ghost !px-2" @click="emit('close')" data-testid="compose-close-btn">
            <X class="w-4 h-4" />
          </button>
        </div>

        <!-- From (when multi-account) -->
        <div
          v-if="account.accounts.length > 1"
          class="hairline border-b px-5 py-2.5 flex items-center gap-3"
        >
          <span class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 w-12">From</span>
          <select
            v-model="fromAccountId"
            class="flex-1 bg-transparent outline-none text-sm font-mono cursor-pointer"
            data-testid="compose-from"
          >
            <option
              v-for="a in account.accounts"
              :key="a.id"
              :value="a.id"
              class="bg-iron-600 text-iron-50"
            >
              {{ a.email }} · {{ a.provider }}
            </option>
          </select>
        </div>

        <!-- Fields -->
        <div
          class="px-5 py-4 space-y-3 flex-1 overflow-y-auto relative"
          @dragenter.prevent="dragCounter++"
          @dragleave.prevent="dragCounter = Math.max(0, dragCounter - 1)"
          @dragover.prevent
          @drop.prevent="onDrop"
        >
          <div class="relative flex items-start gap-3 hairline border-b pb-2">
            <span class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 w-12 pt-1">To</span>
            <div class="flex-1 flex flex-wrap items-center gap-1.5 min-w-0">
              <!-- Чипы получателей (удаляемые), несколько через запятую, max 20 -->
              <span
                v-for="(r, i) in recipients"
                :key="r"
                class="chip text-[11px] flex items-center gap-1.5 max-w-[240px]"
                :class="r.toLowerCase().endsWith('@sth') ? 'border-copper/40 text-copper' : ''"
                :data-testid="`compose-to-chip-${i}`"
              >
                <span class="truncate font-mono">{{ r }}</span>
                <button
                  type="button"
                  class="text-iron-200 hover:text-rust transition-colors shrink-0"
                  @click="removeRecipient(i)"
                  :title="`Убрать ${r}`"
                  :data-testid="`compose-to-chip-remove-${i}`"
                >
                  <X class="w-3 h-3" />
                </button>
              </span>
              <input
                v-model="toInput"
                :placeholder="recipients.length ? '' : (isP2P ? 'name@sth · STHaddress@sth' : 'recipient@example.com')"
                class="bg-transparent flex-1 min-w-[140px] outline-none text-sm font-mono py-0.5"
                autocomplete="off"
                spellcheck="false"
                @focus="toFocus = true"
                @blur="onToBlur"
                @keydown="onToKeydown"
                data-testid="compose-to"
              />
            </div>
            <!-- Автокомплит получателя: совпадения из адресной книги (max 25) -->
            <div
              v-if="toFocus && toSuggestions.length"
              class="absolute left-[60px] right-0 top-full mt-1 z-20 modal-surface border hairline rounded-lg shadow-2xl max-h-56 overflow-y-auto"
              data-testid="compose-to-suggest"
            >
              <button
                v-for="(c, i) in toSuggestions"
                :key="c.email"
                type="button"
                class="w-full text-left px-3 py-2 text-sm font-mono flex items-center gap-2 transition-colors"
                :class="i === toSelIdx ? 'bg-rust/[0.12] text-rust' : 'hover:bg-white/[0.05]'"
                @mousedown.prevent="pickContact(c)"
                @mousemove="toSelIdx = i"
                :data-testid="`compose-to-suggest-${i}`"
              >
                <span class="truncate">{{ c.email }}</span>
                <span v-if="c.name" class="text-iron-200 text-xs truncate">· {{ c.name }}</span>
                <span v-if="c.kind === 'p2p'" class="chip text-[9px] border-copper/40 text-copper ml-auto shrink-0">P2P</span>
              </button>
            </div>
          </div>
          <div v-if="toError" class="text-xs font-mono text-rust -mt-1" data-testid="compose-to-error">
            {{ toError }}
          </div>
          <div class="flex items-center gap-3 hairline border-b pb-2">
            <span class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 w-12">Subject</span>
            <input v-model="form.subject" placeholder="(no subject)" class="bg-transparent flex-1 outline-none text-sm" data-testid="compose-subject" />
          </div>
          <textarea
            ref="bodyRef"
            v-model="form.body"
            rows="9"
            placeholder="Write your message…"
            class="w-full bg-transparent outline-none text-sm leading-relaxed font-sans resize-none"
            data-testid="compose-body"
          />

          <!-- Список выбранных вложений -->
          <div
            v-if="attachments.length"
            class="flex flex-wrap gap-2 pt-1"
            data-testid="compose-attachments-list"
          >
            <div
              v-for="(a, i) in attachments"
              :key="a.uid"
              class="chip text-[11px] flex items-center gap-2 border-copper/40"
              :data-testid="`compose-attachment-${i}`"
            >
              <Paperclip class="w-3 h-3 text-copper" />
              <span class="truncate max-w-[180px]" :title="a.name">{{ a.name }}</span>
              <span class="text-iron-200 font-mono">{{ fmtSize(a.size) }}</span>
              <button
                class="text-iron-200 hover:text-rust transition-colors"
                @click="removeAttachment(i)"
                :title="`Убрать ${a.name}`"
                :data-testid="`compose-attachment-remove-${i}`"
              >
                <X class="w-3 h-3" />
              </button>
            </div>
          </div>

          <!-- Компактная подсказка + скрытый input -->
          <div v-if="attachError" class="text-xs font-mono text-rust" data-testid="attach-error">
            {{ attachError }}
          </div>

          <!-- Overlay Dropzone при перетаскивании -->
          <div
            v-if="dragCounter > 0"
            class="absolute inset-0 z-10 flex items-center justify-center pointer-events-none rounded-lg border-2 border-dashed border-copper bg-black/60 backdrop-blur-sm"
            data-testid="compose-dropzone-overlay"
          >
            <div class="text-center px-4">
              <Paperclip class="w-8 h-8 mx-auto text-copper mb-2" />
              <div class="font-mono text-xs uppercase tracking-[0.22em] text-copper">
                Отпустите файлы, чтобы прикрепить
              </div>
              <div class="font-mono text-[10px] text-iron-200 mt-1">
                Всего не более 25 МБ · {{ fmtSize(totalSize) }} из 25 МБ
              </div>
            </div>
          </div>
        </div>

        <!-- Footer -->
        <div class="hairline border-t px-5 py-3 flex items-center justify-between flex-wrap gap-3 modal-footer">
          <!-- Attach button -->
          <div class="flex items-center gap-3">
            <button
              class="btn-ghost text-xs"
              @click="fileInput?.click()"
              type="button"
              data-testid="compose-attach-btn"
              :title="`Прикрепить файл (${fmtSize(totalSize)} из 25 МБ)`"
            >
              <Paperclip class="w-4 h-4" /> Прикрепить
            </button>
            <input
              ref="fileInput"
              type="file"
              multiple
              class="hidden"
              @change="onFilePick"
              data-testid="compose-file-input"
            />
            <span
              v-if="attachments.length"
              class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200"
              data-testid="attach-total"
            >
              {{ attachments.length }} · {{ fmtSize(totalSize) }} / 25 МБ
            </span>
          </div>

          <!-- SmartHoldem toggle -->
          <label class="flex items-center gap-3 select-none group cursor-pointer" :class="isP2P ? 'opacity-70 pointer-events-none' : ''" data-testid="encrypt-toggle">
            <span
              class="relative w-10 h-5 rounded-full transition-colors duration-200"
              :class="form.encrypted ? 'bg-copper shadow-copper-glow' : 'bg-iron-300/40'"
            >
              <span
                class="absolute top-0.5 left-0.5 w-4 h-4 rounded-full bg-white transition-transform duration-200 shadow"
                :class="form.encrypted ? 'translate-x-5' : 'translate-x-0'"
              />
            </span>
            <span class="flex flex-col">
              <span class="text-xs font-semibold flex items-center gap-1.5">
                <Shield class="w-3.5 h-3.5" :class="form.encrypted ? 'text-copper' : 'text-iron-200'" />
                Secure with SmartHoldem
              </span>
              <span class="font-mono text-[10px] uppercase tracking-[0.18em] text-iron-200">
                {{ isP2P ? 'NTFRY · Pure P2P (Iroh)' : form.encrypted ? 'NTFRY · end-to-end encrypted' : 'plain SMTP' }}
              </span>
            </span>
            <input type="checkbox" v-model="form.encrypted" :disabled="isP2P" class="sr-only" />
          </label>

          <div class="flex items-center gap-2 ml-auto">
            <span v-if="sendError" class="text-xs font-mono text-rust max-w-[280px] truncate" :title="sendError" data-testid="send-error">{{ sendError }}</span>
            <button class="btn-ghost text-xs" @click="emit('close')" data-testid="compose-cancel-btn">Cancel</button>
            <button class="btn-rust" @click="send" :disabled="!canSend || sending" data-testid="compose-send-btn">
              <Send class="w-4 h-4" /> {{ sending ? 'Sending…' : 'Send' }}
            </button>
          </div>
        </div>
      </div>
    </div>
  </transition>
</template>

<script setup>
import { reactive, ref, computed, watch, onMounted, nextTick } from 'vue'
import { Pencil, X, Shield, Send, Reply, Forward, Paperclip } from 'lucide-vue-next'
import { useMailStore } from '../stores/mail'
import { useAccountStore } from '../stores/account'
import { useToastStore } from '../stores/toast'
import { useContactsStore } from '../stores/contacts'
import { api } from '../lib/backend'

const props = defineProps({
  initial: { type: Object, default: null },
})

const emit = defineEmits(['close'])
const mail = useMailStore()
const account = useAccountStore()
const toast = useToastStore()
const contacts = useContactsStore()

const form = reactive({
  subject: props.initial?.subject || '',
  body: props.initial?.body || '',
  encrypted: false, // NTFRY-шифрование SmartNet (получатель должен быть привязан в реестре)
})

const sending = ref(false)
const sendError = ref('')

const fromAccountId = ref(account.activeAccountId || account.accounts[0]?.id || null)
const bodyRef = ref(null)

// ---------- Вложения ----------
// Лимит суммарного размера — 25 МБ (совпадает с бэкенд-проверкой в service.rs).
// Для encrypted (NTFRY) и P2P — файлы кладутся inline в конверт, без sn://file/.
// Для обычного SMTP — стандартные MIME-attachment части.
const MAX_TOTAL_BYTES = 25 * 1024 * 1024
const attachments = ref([]) // { uid, name, mime, size, dataB64 }
const attachError = ref('')
const fileInput = ref(null)
const dragCounter = ref(0)

const totalSize = computed(() => attachments.value.reduce((s, a) => s + a.size, 0))

let uidSeq = 0

function readFileAsBase64(file) {
  return new Promise((resolve, reject) => {
    const r = new FileReader()
    r.onerror = () => reject(new Error(`Не удалось прочитать файл ${file.name}`))
    r.onload = () => {
      // reader.result: "data:mime;base64,PAYLOAD" — берём часть после запятой
      const s = String(r.result || '')
      const comma = s.indexOf(',')
      resolve(comma >= 0 ? s.slice(comma + 1) : s)
    }
    r.readAsDataURL(file)
  })
}

async function addFiles(files) {
  attachError.value = ''
  const incoming = Array.from(files || []).filter(Boolean)
  if (!incoming.length) return
  let running = totalSize.value
  const accepted = []
  for (const f of incoming) {
    if (running + f.size > MAX_TOTAL_BYTES) {
      attachError.value = `«${f.name}» пропущен: превышен суммарный лимит 25 МБ`
      continue
    }
    running += f.size
    accepted.push(f)
  }
  for (const f of accepted) {
    try {
      const dataB64 = await readFileAsBase64(f)
      attachments.value.push({
        uid: ++uidSeq,
        name: f.name,
        mime: f.type || 'application/octet-stream',
        size: f.size,
        dataB64,
      })
    } catch (e) {
      attachError.value = e.message
    }
  }
}

function onFilePick(e) {
  addFiles(e.target?.files)
  // Позволяем повторно выбрать тот же файл после удаления
  if (fileInput.value) fileInput.value.value = ''
}

function onDrop(e) {
  dragCounter.value = 0
  const dt = e.dataTransfer
  if (!dt) return
  addFiles(dt.files)
}

function removeAttachment(i) {
  attachments.value.splice(i, 1)
  attachError.value = ''
}

function fmtSize(n) {
  if (n >= 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + ' MB'
  if (n >= 1024) return (n / 1024).toFixed(0) + ' KB'
  return n + ' B'
}

// Системный P2P-аккаунт SmartNet: только @sth-получатели, шифрование всегда включено
const isP2P = computed(() => {
  const a = account.accounts.find((x) => x.id === fromAccountId.value)
  return a?.provider === 'smartnet'
})
watch(isP2P, (v) => {
  if (v) form.encrypted = true
}, { immediate: true })

const modeIcon = computed(() => {
  if (props.initial?.mode === 'reply') return Reply
  if (props.initial?.mode === 'forward') return Forward
  return Pencil
})

const modeLabel = computed(() => {
  if (props.initial?.mode === 'reply') return 'Reply'
  if (props.initial?.mode === 'forward') return 'Forward'
  return 'New message'
})

// ---------- Получатели: удаляемые чипы, несколько через запятую ----------
const recipients = ref(
  String(props.initial?.to || '')
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean)
)
const toInput = ref('')
const toError = ref('')
// P2P-конверт NTFRY адресуется одному получателю; SMTP — до 20
const maxRcpt = computed(() => (isP2P.value ? 1 : 20))

function commitToken(raw) {
  const v = String(raw || '').trim().replace(/^,+|,+$/g, '').trim()
  if (!v) return
  if (recipients.value.length >= maxRcpt.value) {
    toError.value = `Максимум ${maxRcpt.value} получател${maxRcpt.value === 1 ? 'ь' : 'ей'}`
    return
  }
  const norm = v.toLowerCase()
  // P2P-конверт NTFRY адресуется одному узлу — @sth нельзя смешивать с другими
  const isSth = norm.endsWith('@sth')
  const hasSth = recipients.value.some((r) => r.toLowerCase().endsWith('@sth'))
  if ((isSth && recipients.value.length) || hasSth) {
    toError.value = 'P2P-адрес @sth может быть только единственным получателем'
    return
  }
  if (!recipients.value.some((r) => r.toLowerCase() === norm)) recipients.value.push(v)
  toInput.value = ''
  toError.value = ''
}

function removeRecipient(i) {
  recipients.value.splice(i, 1)
  toError.value = ''
}

// Итоговый список: чипы + недописанный адрес в поле ввода
function allRecipients() {
  const out = [...recipients.value]
  const pending = toInput.value.trim().replace(/^,+|,+$/g, '').trim()
  if (pending && !out.some((r) => r.toLowerCase() === pending.toLowerCase())) out.push(pending)
  return out.slice(0, maxRcpt.value)
}
const canSend = computed(() => recipients.value.length > 0 || !!toInput.value.trim())

function onToKeydown(e) {
  toError.value = ''
  if (e.key === ',') {
    e.preventDefault()
    commitToken(toInput.value)
    return
  }
  if (e.key === 'Enter') {
    e.preventDefault()
    if (toFocus.value && toSuggestions.value.length) {
      pickContact(toSuggestions.value[toSelIdx.value] || toSuggestions.value[0])
    } else {
      commitToken(toInput.value)
    }
    return
  }
  if (e.key === 'Backspace' && !toInput.value && recipients.value.length) {
    recipients.value.pop()
    return
  }
  if (e.key === 'ArrowDown') { e.preventDefault(); moveToSel(1); return }
  if (e.key === 'ArrowUp') { e.preventDefault(); moveToSel(-1); return }
  if (e.key === 'Escape') { toFocus.value = false }
}

function onToBlur() {
  toFocus.value = false
  commitToken(toInput.value)
}

// ---------- Автокомплит получателя из адресной книги (max 25 совпадений) ----------
const toFocus = ref(false)
const toSelIdx = ref(0)
const toSuggestions = computed(() => {
  const q = String(toInput.value || '').trim().toLowerCase()
  if (!q) return []
  const picked = new Set(recipients.value.map((r) => r.toLowerCase()))
  return contacts.list
    .filter((c) => {
      const em = String(c.email || '').toLowerCase()
      if (em === q || picked.has(em)) return false // уже введён/выбран
      return (
        em.includes(q) ||
        String(c.name || '').toLowerCase().includes(q) ||
        String(c.sthAddress || '').toLowerCase().startsWith(q)
      )
    })
    .slice(0, 25)
})
watch(toSuggestions, () => { toSelIdx.value = 0 })

function pickContact(c) {
  toInput.value = ''
  commitToken(c.email)
}
function moveToSel(d) {
  const n = toSuggestions.value.length
  if (!n) return
  toSelIdx.value = (toSelIdx.value + d + n) % n
}

// Position cursor at the top of the body (above the quoted block) for reply/forward
onMounted(async () => {
  if (!contacts.list.length) contacts.load().catch(() => {})
  await nextTick()
  if (props.initial && bodyRef.value) {
    bodyRef.value.focus()
    bodyRef.value.setSelectionRange(0, 0)
    bodyRef.value.scrollTop = 0
  }
})

// Реальная SMTP-отправка через oxid-bridge / Tauri-ядро.
// Для P2P-получателей (@sth) с вложениями — запускаем прогресс-poller «файл X из Y»:
// UI генерирует progressId, ядро пишет прогресс отдачи чанков конверта в общую мапу,
// а UI поллит /api/p2p/progress/:pid и обновляет тот же toast.
async function send() {
  const fromAcc = account.accounts.find((a) => a.id === fromAccountId.value) || account.currentAccount
  if (!fromAcc) {
    sendError.value = 'No mailbox configured'
    return
  }
  if (totalSize.value > MAX_TOTAL_BYTES) {
    sendError.value = `Суммарный размер вложений ${fmtSize(totalSize.value)} превышает лимит 25 МБ`
    return
  }
  const toList = allRecipients()
  if (!toList.length) {
    toError.value = 'Укажите получателя'
    return
  }
  sending.value = true
  sendError.value = ''

  const payload = attachments.value.map((a) => ({
    name: a.name,
    mime: a.mime,
    dataBase64: a.dataB64,
  }))
  const isSthTo = toList.some((r) => r.toLowerCase().endsWith('@sth'))
  const isP2PSend = (isP2P.value || isSthTo) && payload.length > 0
  const progressId = isP2PSend ? genProgressId() : null

  // Стартовый toast + polling прогресса
  let toastId = null
  let pollTimer = null
  if (isP2PSend) {
    toastId = toast.push({
      kind: 'info',
      title: 'P2P-раздача…',
      text: `файл 1 из ${payload.length} — соединяемся с получателем`,
      ttl: 0,
      progress: 0,
    })
    const fileSizes = attachments.value.map((a) => a.size)
    const sumFileBytes = fileSizes.reduce((s, x) => s + x, 0) || 1
    pollTimer = setInterval(async () => {
      try {
        const p = await api('GET', `/p2p/progress/${encodeURIComponent(progressId)}`)
        if (!p) return
        const total = Number(p.total) || 0
        const sent = Math.max(0, Math.min(Number(p.sent) || 0, total))
        if (total === 0) return
        const ratio = sent / total
        // «файл X из Y» — по кумулятивной сумме исходных размеров файлов
        const virtual = ratio * sumFileBytes
        let acc = 0
        let idx = payload.length
        for (let i = 0; i < fileSizes.length; i++) {
          acc += fileSizes[i]
          if (virtual < acc) { idx = i + 1; break }
        }
        const pct = Math.round(ratio * 100)
        toast.update(toastId, {
          text: `файл ${idx} из ${payload.length} · ${pct}%`,
          progress: pct,
        })
      } catch {
        // тихо: канал прогресса необязательный
      }
    }, 500)
  }

  try {
    const msg = await mail.sendEmail({
      accountId: fromAcc.id,
      to: toList.join(', '),
      subject: form.subject || '(no subject)',
      body: form.body,
      attachments: payload,
      encrypt: form.encrypted,
      progressId,
    })
    if (pollTimer) { clearInterval(pollTimer); pollTimer = null }
    if (toastId != null) {
      if (msg?.via === 'p2p') {
        toast.update(toastId, {
          kind: 'success',
          title: 'P2P-раздача завершена',
          text: `✓ все ${payload.length} файл(ов) раздано получателю`,
          progress: 100,
          ttl: 6000,
        })
      } else if (msg?.via === 'smtp') {
        // Сработал SMTP-fallback: конверт ушёл по SMTP (получатель прочтёт через IMAP).
        toast.update(toastId, {
          kind: 'success',
          title: 'Доставлено по SMTP',
          text: `✓ ${payload.length} файл(ов) — P2P был недоступен, отправлено через почтовый сервер`,
          progress: 100,
          ttl: 6000,
        })
      } else {
        toast.dismiss(toastId)
      }
    } else if (msg?.via === 'p2p' && payload.length > 0) {
      // Резервный вариант: если по какой-то причине progressId не был установлен —
      // старое уведомление о раздаче.
      toast.push({
        kind: 'info',
        title: 'P2P-раздача активна',
        text:
          `«${form.subject || '(no subject)'}» с ${payload.length} вложением(-ями) раздаётся по Iroh, ` +
          `пока oxid-mail открыт. Если получатель офлайн — он заберёт письмо, когда снова появится в сети.`,
        ttl: 8000,
      })
    }
    emit('close')
  } catch (e) {
    sendError.value = e.message
    if (pollTimer) clearInterval(pollTimer)
    if (toastId != null) toast.dismiss(toastId)
  } finally {
    sending.value = false
  }
}

// UUID v4 из crypto, с фолбэком на Math.random для очень старых WebView
function genProgressId() {
  try {
    if (typeof crypto !== 'undefined' && crypto.randomUUID) return crypto.randomUUID()
  } catch { /* fallthrough */ }
  return 'p2p-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 10)
}
</script>
