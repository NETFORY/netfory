<template>
  <div class="fixed inset-0 z-50 grid place-items-center bg-black/70 backdrop-blur-sm p-4" data-testid="add-contact-modal" @click.self="$emit('close')">
    <div class="panel rounded-lg w-full max-w-lg border border-rust/30 flex flex-col max-h-[90vh]">
      <div class="hairline border-b px-5 py-3 flex items-center gap-2">
        <UserPlus class="w-4 h-4 text-rust" />
        <h2 class="font-mono text-xs uppercase tracking-[0.25em] text-rust">// add contact</h2>
        <button class="ml-auto btn-ghost !px-2" @click="$emit('close')" data-testid="add-contact-close">
          <X class="w-4 h-4" />
        </button>
      </div>

      <div class="p-5 space-y-4 overflow-y-auto">
        <div>
          <label class="font-mono text-[10px] uppercase tracking-[0.18em] text-iron-200 block mb-1">contact string</label>
          <textarea
            v-model="input"
            rows="2"
            @input="onInput"
            placeholder="user@gmail.com · sn://add-email?… · SADDR:user@gmail.com · SADDR@sth · name@sth"
            class="w-full panel rounded-md px-3 py-2 text-sm font-mono outline-none focus:border-rust/50 border border-transparent resize-none"
            data-testid="contact-input"
          />
          <p class="text-[11px] text-iron-200 mt-1">
            Email — classic contact · URI/SADDR:email — encrypted (Local Trust) · SADDR@sth / name@sth — internal P2P
          </p>
        </div>

        <!-- Предпросмотр распознанного контакта -->
        <div v-if="preview" class="panel rounded-md p-3 border border-copper/20 space-y-1.5" data-testid="contact-preview">
          <div class="flex items-center gap-2">
            <span class="chip" :class="kindChip(preview.kind).cls" data-testid="contact-kind">{{ kindChip(preview.kind).label }}</span>
            <span class="text-sm font-mono truncate">{{ preview.email }}</span>
          </div>
          <div v-if="preview.sthAddress" class="font-mono text-[11px] text-iron-200 break-all">addr {{ preview.sthAddress }}</div>
          <div v-if="preview.x25519Hex" class="font-mono text-[11px] text-emerald-400">✔ encryption key included — chain lookup will be skipped</div>
          <div v-else-if="preview.kind === 'encrypted'" class="font-mono text-[11px] text-copper">keys will be fetched from chain, address is pinned locally</div>
        </div>
        <div v-if="parseError" class="text-xs font-mono text-rust" data-testid="contact-parse-error">! {{ parseError }}</div>

        <div class="grid grid-cols-2 gap-3">
          <div>
            <label class="font-mono text-[10px] uppercase tracking-[0.18em] text-iron-200 block mb-1">name</label>
            <input v-model="name" class="w-full panel rounded-md px-3 py-2 text-sm outline-none focus:border-rust/50 border border-transparent" data-testid="contact-name" />
          </div>
          <div>
            <label class="font-mono text-[10px] uppercase tracking-[0.18em] text-iron-200 block mb-1">note</label>
            <input v-model="note" class="w-full panel rounded-md px-3 py-2 text-sm outline-none focus:border-rust/50 border border-transparent" data-testid="contact-note" />
          </div>
        </div>

        <div class="flex items-center gap-3">
          <button class="btn-rust" :disabled="!input.trim() || saving" @click="save" data-testid="contact-save-btn">
            <UserPlus class="w-4 h-4" /> {{ saving ? 'Saving…' : 'Add contact' }}
          </button>
          <span v-if="saveError" class="text-xs font-mono text-rust" data-testid="contact-save-error">! {{ saveError }}</span>
        </div>

        <!-- Адресная книга -->
        <div class="hairline border-t pt-3">
          <div class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 mb-2">
            // address book · {{ contacts.list.length }}
          </div>
          <div v-if="!contacts.list.length" class="text-sm text-iron-200 font-mono">// empty — add your first trusted contact</div>
          <div v-for="c in contacts.list" :key="c.email" class="flex items-center gap-2 py-1.5 hairline border-b last:border-0" :data-testid="`contact-row-${c.email}`">
            <span class="chip shrink-0" :class="kindChip(c.kind).cls">{{ kindChip(c.kind).label }}</span>
            <div class="min-w-0 flex-1">
              <div class="text-sm font-mono truncate">{{ c.name ? c.name + ' · ' : '' }}{{ c.email }}</div>
              <div v-if="c.note" class="text-[11px] text-iron-200 truncate">{{ c.note }}</div>
            </div>
            <button class="btn-ghost !px-2 text-rust" @click="contacts.remove(c.email)" :data-testid="`contact-delete-${c.email}`" title="Delete contact">
              <Trash2 class="w-3.5 h-3.5" />
            </button>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup>
import { ref, onMounted } from 'vue'
import { UserPlus, X, Trash2 } from 'lucide-vue-next'
import { useContactsStore } from '../stores/contacts'

defineEmits(['close'])
const contacts = useContactsStore()

const input = ref('')
const name = ref('')
const note = ref('')
const preview = ref(null)
const parseError = ref('')
const saveError = ref('')
const saving = ref(false)
let parseTimer = null

function kindChip(kind) {
  switch (kind) {
    case 'encrypted': return { label: '🔒 ENCRYPTED', cls: 'border-copper/40 text-copper' }
    case 'p2p': return { label: '⚡ P2P', cls: 'border-emerald-500/50 text-emerald-400' }
    default: return { label: 'EMAIL', cls: 'border-white/15 text-iron-200' }
  }
}

function onInput() {
  clearTimeout(parseTimer)
  preview.value = null
  parseError.value = ''
  const v = input.value.trim()
  if (!v) return
  parseTimer = setTimeout(async () => {
    try {
      preview.value = await contacts.parse(v)
      parseError.value = ''
    } catch (e) {
      parseError.value = e.message
    }
  }, 350)
}

async function save() {
  saving.value = true
  saveError.value = ''
  try {
    await contacts.add(input.value, name.value, note.value)
    input.value = ''
    name.value = ''
    note.value = ''
    preview.value = null
  } catch (e) {
    saveError.value = e.message
  } finally {
    saving.value = false
  }
}

onMounted(() => contacts.load())
</script>
