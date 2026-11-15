<template>
  <aside class="h-full flex flex-col panel rounded-none lg:rounded-none border-r hairline" data-testid="sidebar">
    <!-- Account switcher -->
    <div class="px-4 py-4 hairline border-b relative">
      <span class="absolute left-4 right-4 bottom-0 h-px bg-rust/60" aria-hidden="true" />
      <AccountSwitcher testid="sidebar-switcher" />
    </div>

    <!-- Folders -->
    <nav class="px-2 py-3 flex-1 overflow-y-auto">
      <div class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 px-2 pb-2">Folders</div>
      <ul class="space-y-0.5">
        <li v-for="f in folders" :key="f.id">
          <button
            class="w-full flex items-center gap-3 px-2.5 py-2 rounded-md text-sm transition group"
            :class="(mail.activeFolder === f.id && !mail.activeLabel)
              ? 'bg-rust/15 text-rust'
              : 'text-iron-100 hover:bg-white/[0.05] hover:text-iron-50'"
            @click="onSelect(f.id)"
            :data-testid="`folder-${f.id}`"
          >
            <component
              :is="f.icon"
              class="w-4 h-4"
              :class="f.id === 'p2p' && p2pPulse ? 'text-emerald-400 animate-pulse' : ''"
            />
            <span class="capitalize flex-1 text-left">{{ f.id }}</span>
            <span
              v-if="f.count > 0"
              class="font-mono text-[10px] px-1.5 py-0.5 rounded transition"
              :class="[
                (mail.activeFolder === f.id && !mail.activeLabel)
                  ? 'bg-rust text-white'
                  : (f.id === 'p2p' && p2pPulse)
                    ? 'bg-emerald-500 text-black animate-pulse ring-2 ring-emerald-400/50'
                    : 'bg-white/[0.08] text-iron-100',
              ]"
              :data-testid="`folder-${f.id}-badge`"
            >
              {{ f.count }}
            </span>
          </button>
        </li>
      </ul>

      <div class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 px-2 pb-2 pt-6">Labels</div>
      <ul class="space-y-0.5">
        <li v-for="l in labels" :key="l.id">
          <button
            class="w-full flex items-center gap-3 px-2.5 py-1.5 rounded-md text-sm transition group"
            :class="mail.activeLabel === l.id
              ? 'bg-rust/15 text-rust'
              : 'text-iron-100 hover:bg-white/[0.05] hover:text-iron-50'"
            @click="onLabelClick(l.id)"
            :data-testid="`label-${l.id}`"
          >
            <span class="w-2 h-2 rounded-full shrink-0" :style="{ background: l.color }" />
            <span class="flex-1 text-left capitalize">{{ l.id }}</span>
            <span
              v-if="labelCounts[l.id]"
              class="font-mono text-[10px] px-1.5 py-0.5 rounded shrink-0"
              :class="mail.activeLabel === l.id ? 'bg-rust text-white' : 'bg-white/[0.08] text-iron-100'"
            >
              {{ labelCounts[l.id] }}
            </span>
          </button>
        </li>
      </ul>
    </nav>

    <!-- Мой @sth-адрес: copy + QR прямо в футере (SmartNet P2P Direct) -->
    <div class="px-3 py-3 hairline border-t panel rounded-none" data-testid="my-sth-card">
      <div class="flex items-center justify-between mb-2">
        <span class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 flex items-center gap-1.5">
          <Zap class="w-3 h-3 text-emerald-400" /> My P2P Address
        </span>
        <span class="chip border-copper/40 text-copper">
          <span class="w-1.5 h-1.5 rounded-full bg-copper animate-pulse" /> ONLINE
        </span>
      </div>

      <div v-if="sthEmail" class="space-y-2">
        <button
          v-if="sthNameEmail"
          class="w-full text-left font-mono text-[11px] text-copper hover:text-orange-300 break-all bg-black/30 rounded px-2 py-1.5 border border-copper/25 transition"
          :title="copiedName ? 'Copied!' : 'u:// name · click to copy'"
          @click="copyName"
          data-testid="sidebar-sth-name"
        >
          {{ sthNameEmail }}
        </button>
        <button
          class="w-full text-left font-mono text-[11px] text-emerald-300 hover:text-emerald-200 break-all bg-black/30 rounded px-2 py-1.5 border border-emerald-500/20 transition"
          :title="copied ? 'Copied!' : 'Click to copy'"
          @click="copyAddr"
          data-testid="sidebar-sth-address"
        >
          {{ sthEmail }}
        </button>
        <div class="flex items-center gap-2">
          <button
            class="btn-ghost !px-2 !py-1 text-xs flex-1 justify-center"
            @click="copyAddr"
            data-testid="sidebar-copy-sth"
          >
            <Check v-if="copied" class="w-3.5 h-3.5 text-emerald-400" />
            <Copy v-else class="w-3.5 h-3.5" />
            <span>{{ copied ? 'copied' : 'copy' }}</span>
          </button>
          <button
            class="btn-ghost !px-2 !py-1 text-xs flex-1 justify-center"
            :class="showQr ? 'bg-white/[0.08] text-rust' : ''"
            @click="toggleQr"
            data-testid="sidebar-toggle-qr"
          >
            <QrCode class="w-3.5 h-3.5" />
            <span>{{ showQr ? 'hide' : 'QR' }}</span>
          </button>
        </div>
        <div v-if="showQr && qrData" class="grid place-items-center pt-2" data-testid="sidebar-qr">
          <img
            :src="qrData"
            alt="my P2P address"
            class="rounded-md border border-white/10 w-[168px] h-[168px] bg-white p-2"
          />
          <div class="text-[10px] text-iron-200 mt-1 font-mono">scan to add me · SmartNet</div>
        </div>
        <div v-if="qrError" class="text-[10px] font-mono text-rust" data-testid="sidebar-qr-error">! {{ qrError }}</div>
      </div>
      <div v-else class="font-mono text-[10px] text-iron-200 truncate">—</div>
    </div>
  </aside>
</template>

<script setup>
import { computed, ref, watch, onUnmounted } from 'vue'
import { Inbox, Send, FileText, Trash2, Zap, Copy, Check, QrCode } from 'lucide-vue-next'
import QRCode from 'qrcode'
import { useMailStore } from '../stores/mail'
import { useAccountStore } from '../stores/account'
import { api } from '../lib/backend'
import AccountSwitcher from './AccountSwitcher.vue'

const emit = defineEmits(['navigate'])
const mail = useMailStore()
const account = useAccountStore()

const folders = computed(() => [
  { id: 'inbox', icon: Inbox },
  { id: 'sent', icon: Send },
  { id: 'p2p', icon: Zap },
  { id: 'drafts', icon: FileText },
  { id: 'trash', icon: Trash2 },
].map((f) => {
  const counts = mail.foldersWithCounts.find((x) => x.id === f.id)
  return { ...f, count: counts?.count || 0 }
}))

const labels = [
  { id: 'work', color: '#E25822' },
  { id: 'personal', color: '#4DB6AC' },
  { id: 'vault', color: '#9498A0' },
]

const labelCounts = computed(() => mail.labelsWithCounts)

// @sth-адрес: превращаем "raw sth-address" в человекочитаемое <addr>@sth
const sthEmail = computed(() => (account.sthAddress ? `${account.sthAddress}@sth` : ''))

// Alias Naming Service: u://имя → имя@sth (если имя зарегистрировано на адрес)
const sthName = ref('')
const sthNameEmail = computed(() => (sthName.value ? `${sthName.value}@sth` : ''))
watch(
  () => account.sthAddress,
  async (a) => {
    sthName.value = ''
    if (!a) return
    try {
      const r = await api('GET', '/naming/my')
      sthName.value = r?.name || ''
    } catch { /* naming недоступен — показываем только адрес */ }
  },
  { immediate: true }
)

// Copy + QR
const copied = ref(false)
const copiedName = ref(false)
const showQr = ref(false)
const qrData = ref('')
const qrError = ref('')
let copiedTimer = null
let copiedNameTimer = null

function writeClipboard(v) {
  return navigator.clipboard.writeText(v).catch(() => {
    const ta = document.createElement('textarea')
    ta.value = v
    document.body.appendChild(ta)
    ta.select()
    document.execCommand('copy')
    ta.remove()
  })
}

async function copyAddr() {
  if (!sthEmail.value) return
  await writeClipboard(sthEmail.value)
  copied.value = true
  if (copiedTimer) clearTimeout(copiedTimer)
  copiedTimer = setTimeout(() => { copied.value = false }, 1500)
}

async function copyName() {
  if (!sthNameEmail.value) return
  await writeClipboard(sthNameEmail.value)
  copiedName.value = true
  if (copiedNameTimer) clearTimeout(copiedNameTimer)
  copiedNameTimer = setTimeout(() => { copiedName.value = false }, 1500)
}

async function ensureQr() {
  if (!sthEmail.value) return
  qrError.value = ''
  try {
    // Кодируем ту же схему что и ShareContactModal (`p2p` row) — просто адрес @sth,
    // партнёр отсканирует и добавит в контакты одним касанием.
    qrData.value = await QRCode.toDataURL(sthEmail.value, { width: 336, margin: 1 })
  } catch (e) {
    qrError.value = e.message
  }
}

async function toggleQr() {
  if (showQr.value) {
    showQr.value = false
    return
  }
  if (!qrData.value) await ensureQr()
  showQr.value = !!qrData.value
}

// Регенерация QR при смене адреса (например, adopt-handoff из SmartNet)
watch(sthEmail, async (v) => {
  qrData.value = ''
  if (v && showQr.value) await ensureQr()
})

// Pulse-бейдж папки P2P: 6 секунд подсветки после появления свежего письма.
const now = ref(Date.now())
let tick = null
const p2pPulse = computed(() => {
  const flash = mail.p2pFlash
  return flash > 0 && now.value - flash < 6000
})
watch(
  () => mail.p2pFlash,
  (v) => {
    if (v && !tick) {
      now.value = Date.now()
      tick = window.setInterval(() => {
        now.value = Date.now()
        if (Date.now() - mail.p2pFlash > 6000) {
          window.clearInterval(tick)
          tick = null
        }
      }, 500)
    }
  }
)

onUnmounted(() => {
  if (copiedTimer) clearTimeout(copiedTimer)
  if (copiedNameTimer) clearTimeout(copiedNameTimer)
  if (tick) window.clearInterval(tick)
})

function onSelect(id) {
  mail.setFolder(id)
  emit('navigate')
}

function onLabelClick(id) {
  mail.setLabel(id)
  emit('navigate')
}
</script>

