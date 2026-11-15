<template>
  <div class="fixed inset-0 z-50 grid place-items-center bg-black/70 backdrop-blur-sm p-4" data-testid="share-contact-modal" @click.self="$emit('close')">
    <div class="panel rounded-lg w-full max-w-lg border border-rust/30">
      <div class="hairline border-b px-5 py-3 flex items-center gap-2">
        <Share2 class="w-4 h-4 text-rust" />
        <h2 class="font-mono text-xs uppercase tracking-[0.25em] text-rust">// share my contact</h2>
        <button class="ml-auto btn-ghost !px-2" @click="$emit('close')" data-testid="share-contact-close">
          <X class="w-4 h-4" />
        </button>
      </div>

      <div class="p-5 space-y-4">
        <p class="text-xs text-iron-200">
          Send one of these to a trusted peer over any channel. Adding it locally beats
          the blockchain registry — DoS/squatting-proof (Local Trust Layer).
        </p>

        <div v-if="loading" class="text-sm font-mono text-iron-200">// deriving keys…</div>
        <div v-else-if="error" class="text-xs font-mono text-rust" data-testid="share-error">! {{ error }}</div>

        <template v-else-if="card">
          <div v-for="row in rows" :key="row.id" class="panel rounded-md p-3 border border-white/10" :data-testid="`share-row-${row.id}`">
            <div class="flex items-center gap-2 mb-1.5">
              <component :is="row.icon" class="w-3.5 h-3.5" :class="row.iconCls" />
              <span class="font-mono text-[10px] uppercase tracking-[0.2em] text-iron-200">{{ row.label }}</span>
              <button class="ml-auto btn-ghost !px-2 !py-1 text-xs" @click="toggleQr(row)" :data-testid="`share-qr-${row.id}`">
                <QrCode class="w-3.5 h-3.5" /> {{ qrFor === row.id ? 'hide' : 'QR' }}
              </button>
              <button class="btn-ghost !px-2 !py-1 text-xs" @click="copy(row)" :data-testid="`share-copy-${row.id}`">
                <Check v-if="copied === row.id" class="w-3.5 h-3.5 text-emerald-400" />
                <Copy v-else class="w-3.5 h-3.5" />
                {{ copied === row.id ? 'copied' : 'copy' }}
              </button>
            </div>
            <div class="font-mono text-[11px] break-all text-iron-100 select-all">{{ row.value }}</div>
            <div class="text-[10px] text-iron-200 mt-1">{{ row.hint }}</div>
            <div v-if="qrFor === row.id && qrData" class="mt-3 grid place-items-center" :data-testid="`share-qr-img-${row.id}`">
              <img :src="qrData" alt="contact QR" class="rounded-md border border-white/10 w-[220px] h-[220px] bg-white p-2" />
              <div class="text-[10px] text-iron-200 mt-1 font-mono">scan with SmartNet</div>
            </div>
          </div>
        </template>
      </div>
    </div>
  </div>
</template>

<script setup>
import { ref, computed, onMounted } from 'vue'
import { Share2, X, Copy, Check, Mail, Shield, Zap, AtSign, QrCode } from 'lucide-vue-next'
import QRCode from 'qrcode'
import { useContactsStore } from '../stores/contacts'
import { useAccountStore } from '../stores/account'
import { api } from '../lib/backend'

defineEmits(['close'])
const contacts = useContactsStore()
const account = useAccountStore()

const card = ref(null)
const loading = ref(true)
const error = ref('')
const copied = ref('')
const qrFor = ref('')
const qrData = ref('')

// QR контактной карточки — для обмена при личной встрече
async function toggleQr(row) {
  if (qrFor.value === row.id) {
    qrFor.value = ''
    return
  }
  try {
    qrData.value = await QRCode.toDataURL(row.value, { width: 440, margin: 1 })
    qrFor.value = row.id
  } catch (e) {
    error.value = e.message
  }
}

// Alias Naming Service: u://имя → имя@sth (если имя зарегистрировано на адрес)
const sthName = ref('')

const rows = computed(() => {
  if (!card.value) return []
  const out = []
  // classic email скрываем, когда классическая почта не добавлена:
  // у системного P2P-аккаунта plain — это тот же <адрес>@sth (дубль блока 3)
  const hasClassic = !card.value.plain.toLowerCase().endsWith('@sth')
  if (hasClassic) {
    out.push({
      id: 'plain',
      label: 'classic email',
      icon: Mail,
      iconCls: 'text-iron-200',
      value: card.value.plain,
      hint: 'Plain SMTP mail, no encryption',
    })
  }
  out.push({
    id: 'uri',
    label: 'encrypted contact uri',
    icon: Shield,
    iconCls: 'text-copper',
    value: card.value.uri,
    hint: 'Public key + email — peer adds it locally and encrypts to you without touching the chain',
  })
  out.push({
    id: 'p2p',
    label: 'p2p address',
    icon: Zap,
    iconCls: 'text-emerald-400',
    value: card.value.p2p,
    hint: 'Internal SmartNet address (direct Iroh delivery — next phase)',
  })
  if (sthName.value) {
    out.push({
      id: 'name',
      label: 'u://name alias',
      icon: AtSign,
      iconCls: 'text-rust',
      value: `${sthName.value}@sth`,
      hint: 'Registered Naming Service alias — resolves to your P2P address on-chain',
    })
  }
  return out.map((r, i) => ({ ...r, label: `${i + 1} · ${r.label}` }))
})

async function copy(row) {
  try {
    await navigator.clipboard.writeText(row.value)
  } catch {
    const ta = document.createElement('textarea')
    ta.value = row.value
    document.body.appendChild(ta)
    ta.select()
    document.execCommand('copy')
    ta.remove()
  }
  copied.value = row.id
  setTimeout(() => { if (copied.value === row.id) copied.value = '' }, 1500)
}

onMounted(async () => {
  try {
    const acc = account.currentAccount
    if (!acc) throw new Error('no active mailbox')
    card.value = await contacts.myCard(acc.id)
  } catch (e) {
    error.value = e.message
  } finally {
    loading.value = false
  }
  try {
    const r = await api('GET', '/naming/my')
    sthName.value = r?.name || ''
  } catch { /* naming недоступен — показываем только адрес */ }
})
</script>
