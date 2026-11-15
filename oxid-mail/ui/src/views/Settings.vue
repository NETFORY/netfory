<template>
  <div class="min-h-screen p-4 sm:p-8" data-testid="settings-root">
    <div class="max-w-4xl mx-auto">
      <!-- Header -->
      <div class="flex items-center justify-between mb-8">
        <div class="flex items-center gap-3">
          <router-link to="/mail" class="btn-ghost !px-2" data-testid="back-to-mail-btn">
            <ArrowLeft class="w-4 h-4" />
          </router-link>
          <div>
            <div class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200">// SETTINGS</div>
            <h1 class="text-2xl sm:text-3xl font-semibold">Workshop</h1>
          </div>
        </div>
        <button class="btn-ghost text-xs mono" @click="theme.toggle" data-testid="theme-toggle-settings">
          <Moon v-if="theme.mode === 'dark'" class="w-4 h-4" />
          <Sun v-else class="w-4 h-4" />
          {{ theme.mode === 'dark' ? 'DARK' : 'LIGHT' }}
        </button>
      </div>

      <div class="grid lg:grid-cols-2 gap-5">
        <!-- Accounts -->
        <section class="panel rounded-xl p-5 lg:col-span-2" data-testid="accounts-section">
          <div class="flex items-center justify-between mb-4">
            <div class="flex items-center gap-2">
              <AtSign class="w-4 h-4 text-rust" />
              <h2 class="font-semibold">Accounts</h2>
              <span class="font-mono text-[10px] uppercase tracking-[0.18em] text-iron-200 ml-1">
                · {{ account.accounts.length }}
              </span>
            </div>
            <button
              class="btn-rust !py-1.5 !px-3 text-xs"
              @click="onToggleAddForm"
              data-testid="toggle-add-account-btn"
            >
              <Plus class="w-4 h-4" />
              {{ (formMode === 'create') ? 'Cancel' : 'Add account' }}
            </button>
          </div>

          <ul class="space-y-2 mb-3">
            <template v-for="a in account.accounts" :key="a.id">
              <li
                class="panel rounded-md p-3 flex items-center gap-3 transition cursor-pointer"
                :class="a.id === account.activeAccountId ? 'border-rust/40' : 'hover:border-white/20'"
                @click="account.setActiveAccount(a.id)"
                :data-testid="`account-row-${a.id}`"
              >
                <div class="w-8 h-8 rounded bg-rust/15 grid place-items-center">
                  <Mail class="w-4 h-4 text-rust" />
                </div>
                <div class="min-w-0 flex-1">
                  <div class="text-sm font-semibold truncate">{{ a.email }}</div>
                  <div class="font-mono text-[10px] uppercase tracking-[0.18em] text-iron-200">
                    {{ a.provider }} · {{ a.provider === 'smartnet' ? 'iroh direct' : (a.host || 'oauth') + (a.port ? `:${a.port}` : '') }}
                  </div>
                </div>
                <span v-if="a.provider === 'smartnet'" class="chip border-copper/40 text-copper" :data-testid="`p2p-chip-${a.id}`">P2P</span>
                <span v-if="a.id === account.activeAccountId" class="chip border-copper/40 text-copper">ACTIVE</span>
                <button
                  v-if="a.provider !== 'smartnet'"
                  class="btn-ghost !px-2"
                  @click.stop="startEdit(a)"
                  :title="'Edit ' + a.email"
                  :data-testid="`edit-account-${a.id}`"
                >
                  <Pencil class="w-4 h-4" />
                </button>
                <button
                  v-if="a.provider !== 'smartnet'"
                  class="btn-ghost !px-2"
                  @click.stop="confirmRemove(a.id)"
                  :data-testid="`remove-account-${a.id}`"
                >
                  <Trash2 class="w-4 h-4" />
                </button>
              </li>

              <!-- Inline edit form for this row -->
              <li
                v-if="formMode === 'edit' && editingId === a.id"
                class="panel rounded-lg p-4 border-copper/30"
                :data-testid="`edit-account-form-${a.id}`"
              >
                <div class="font-mono text-[10px] uppercase tracking-[0.22em] text-copper mb-3 flex items-center gap-1.5">
                  <Pencil class="w-3 h-3" /> editing · {{ a.email }}
                </div>
                <AccountForm
                  :form="accountForm"
                  :providers="providers"
                  @select-provider="selectProviderInForm"
                />
                <div v-if="formError" class="mt-3 text-xs text-rust font-mono" data-testid="form-error">
                  ! {{ formError }}
                </div>
                <div class="mt-4 flex items-center justify-end gap-2">
                  <button class="btn-ghost text-xs" @click="closeForm" data-testid="cancel-edit-btn">
                    Cancel
                  </button>
                  <button class="btn-rust !py-1.5" @click="saveEdit" data-testid="save-edit-btn">
                    <Save class="w-4 h-4" /> Save changes
                  </button>
                </div>
              </li>
            </template>
            <li v-if="account.accounts.length === 0" class="text-sm text-iron-200 font-mono">// no accounts</li>
          </ul>

          <!-- Add-account inline form -->
          <transition name="fade">
            <div
              v-if="formMode === 'create'"
              class="panel rounded-lg p-4 border-rust/20"
              data-testid="add-account-form"
            >
              <div class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 mb-3">
                // new mailbox
              </div>
              <AccountForm
                :form="accountForm"
                :providers="providers"
                @select-provider="selectProviderInForm"
              />
              <div v-if="formError" class="mt-3 text-xs text-rust font-mono" data-testid="add-account-error">
                ! {{ formError }}
              </div>
              <div class="mt-4 flex items-center justify-end gap-2">
                <button class="btn-ghost text-xs" @click="closeForm" data-testid="cancel-add-account-btn">
                  Cancel
                </button>
                <button class="btn-rust !py-1.5" @click="saveNewAccount" data-testid="save-account-btn">
                  <Plus class="w-4 h-4" /> Add mailbox
                </button>
              </div>
            </div>
          </transition>
        </section>

        <!-- Appearance -->
        <section class="panel rounded-xl p-5 lg:col-span-2" data-testid="appearance-section">
          <div class="flex items-center gap-2 mb-4">
            <Palette class="w-4 h-4 text-rust" />
            <h2 class="font-semibold">Appearance</h2>
          </div>

          <div class="grid sm:grid-cols-2 gap-3">
            <button
              v-for="opt in [{id:'dark', label:'Dark · Iron'}, {id:'light', label:'Light · Patina'}]"
              :key="opt.id"
              class="w-full panel rounded-md p-3 flex items-center gap-3 text-left transition"
              :class="theme.mode === opt.id ? 'border-rust' : 'hover:border-white/20'"
              @click="theme.set(opt.id)"
              :data-testid="`theme-option-${opt.id}`"
            >
              <span
                class="w-8 h-8 rounded"
                :style="opt.id === 'dark' ? 'background:linear-gradient(135deg,#2B2D31,#0E1012)' : 'background:linear-gradient(135deg,#F4F1ED,#D8D2C7)'"
              />
              <span class="flex-1">
                <span class="text-sm font-semibold block">{{ opt.label }}</span>
                <span class="font-mono text-[10px] uppercase tracking-[0.18em] text-iron-200">
                  {{ opt.id === 'dark' ? 'Default · industrial' : 'Soft · daytime' }}
                </span>
              </span>
              <Check v-if="theme.mode === opt.id" class="w-4 h-4 text-rust" />
            </button>
          </div>
        </section>

        <!-- Security -->
        <section class="panel rounded-xl p-5 lg:col-span-2" data-testid="security-section">
          <div class="flex items-center gap-2 mb-4">
            <Shield class="w-4 h-4 text-copper" />
            <h2 class="font-semibold">Security · SmartHoldem</h2>
          </div>

          <div class="grid sm:grid-cols-2 gap-4 mb-4">
            <div>
              <div class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 mb-1">STH Address</div>
              <div class="font-mono text-sm text-rust break-all" data-testid="settings-sth">
                {{ account.sthAddress || '—' }}
              </div>
            </div>
            <div>
              <div class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 mb-1">Public key</div>
              <div class="font-mono text-xs text-copper break-all" data-testid="settings-public-key">
                {{ account.publicKey || '—' }}
              </div>
            </div>
          </div>

          <!-- Привязка email ↔ STH-адрес (Anti-Spoofing) -->
          <div class="hairline border-t pt-4 mb-4" data-testid="binding-section">
            <div class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 mb-2">
              // EMAIL ↔ STH BINDING · Anti-Spoofing
            </div>
            <p class="text-xs text-iron-200 mb-3 max-w-prose">
              Binding proves mailbox ownership (proof e-mail + IMAP check) and publishes your
              encryption keys to the SmartHoldem chain. Cost: 0.5 STH + 0.5 STH fee.
              Encrypted NTFRY mail works only between bound addresses.
            </p>
            <div v-if="account.currentAccount && account.currentAccount.provider !== 'smartnet'" class="flex items-center gap-3 flex-wrap">
              <span class="text-sm font-mono truncate max-w-[240px]">{{ account.currentAccount.email }}</span>
              <span class="chip" :class="bindChip.cls" data-testid="bind-status">{{ bindChip.label }}</span>
              <button
                class="btn-rust !py-1.5 text-xs"
                @click="bindNow"
                :disabled="binding || bindState.status === 'confirmed' || bindState.status === 'tx_sent'"
                data-testid="bind-btn"
              >
                <Shield class="w-3.5 h-3.5" />
                {{ binding ? 'Binding… (up to 2 min)' : bindState.status === 'confirmed' ? 'Bound' : 'Bind (1 STH)' }}
              </button>
              <span v-if="bindState.txid" class="font-mono text-[10px] text-iron-200 break-all" data-testid="bind-txid">
                tx {{ bindState.txid.slice(0, 16) }}…
              </span>
            </div>
            <div v-else class="text-sm text-iron-200 font-mono">// add a mailbox first</div>
            <div v-if="bindState.error" class="mt-2 text-xs font-mono text-rust max-w-prose" data-testid="bind-error">
              ! {{ bindState.error }}
            </div>
            <div v-if="bindState.status === 'confirmed' && bindState.dkim" class="mt-2 text-xs font-mono text-emerald-400">
              ✔ ownership proven · DKIM signed by provider
            </div>
          </div>

          <div class="hairline border-t pt-4 flex flex-wrap items-center gap-3">
            <label class="flex items-center gap-3 cursor-pointer select-none" data-testid="global-encrypt-toggle">
              <span
                class="relative w-10 h-5 rounded-full transition-colors duration-200"
                :class="account.isEncryptedEnabled ? 'bg-copper shadow-copper-glow' : 'bg-white/[0.12]'"
              >
                <span
                  class="absolute top-0.5 left-0.5 w-4 h-4 rounded-full bg-white transition-transform duration-200"
                  :class="account.isEncryptedEnabled ? 'translate-x-5' : 'translate-x-0'"
                />
              </span>
              <span class="text-sm">Encrypt new messages by default</span>
              <input type="checkbox" :checked="account.isEncryptedEnabled" @change="account.toggleEncryption()" class="sr-only" />
            </label>

            <button class="btn-ghost text-xs ml-auto" @click="showSeed = !showSeed" data-testid="toggle-seed-btn">
              <Eye v-if="!showSeed" class="w-4 h-4" />
              <EyeOff v-else class="w-4 h-4" />
              {{ showSeed ? 'Hide seed' : 'View seed' }}
            </button>
            <button
              v-if="canExportSeed"
              class="btn-rust !py-1.5"
              @click="exportSeed"
              data-testid="export-seed-btn"
            >
              <Download class="w-4 h-4" /> Export seed
            </button>
          </div>

          <transition name="fade">
            <div v-if="showSeed" class="mt-4 grid grid-cols-3 sm:grid-cols-4 gap-2" data-testid="seed-display">
              <div
                v-for="(w, i) in account.seedWords"
                :key="i"
                class="panel rounded px-2.5 py-1.5 font-mono text-xs flex items-center gap-2"
              >
                <span class="text-iron-200 text-[10px]">{{ String(i + 1).padStart(2, '0') }}</span>
                <span>{{ w }}</span>
              </div>
            </div>
          </transition>
        </section>

        <!-- Danger zone -->
        <section class="panel rounded-xl p-5 lg:col-span-2 border-rust/20" data-testid="danger-section">
          <div class="flex items-center gap-2 mb-3">
            <Flame class="w-4 h-4 text-rust" />
            <h2 class="font-semibold text-rust">Danger zone</h2>
          </div>
          <p class="text-sm text-iron-200 mb-4 max-w-prose">
            Resets all local data — seed, accounts, decrypted state. There is no recovery.
          </p>
          <button class="btn-ghost text-xs border border-rust/40 hover:bg-rust/10 text-rust" @click="resetAll" data-testid="reset-btn">
            <Trash2 class="w-4 h-4" /> Burn it down
          </button>
        </section>
      </div>
    </div>
  </div>
</template>

<script setup>
import { ref, reactive, onMounted, watch } from 'vue'
import { useRouter, useRoute } from 'vue-router'
import {
  ArrowLeft, AtSign, Mail, Trash2, Palette, Check, Shield, Eye, EyeOff,
  Download, Flame, Moon, Sun, Plus, Server, Globe, Pencil, Save,
} from 'lucide-vue-next'
import { useAccountStore } from '../stores/account'
import { useThemeStore } from '../stores/theme'
import { useMailStore } from '../stores/mail'
import { useToastStore } from '../stores/toast'
import AccountForm from '../components/AccountForm.vue'

const account = useAccountStore()
const theme = useThemeStore()
const mail = useMailStore()
const toast = useToastStore()
const router = useRouter()
const route = useRoute()

account.hydrate()

const showSeed = ref(false)

// ---------- Привязка email ↔ STH (Anti-Spoofing) ----------
import { computed, onUnmounted } from 'vue'
import { api } from '../lib/backend'

const bindState = ref({ status: 'none', error: '', txid: '', dkim: false })
const binding = ref(false)
let bindPoll = null

const bindChip = computed(() => {
  switch (bindState.value.status) {
    case 'confirmed': return { label: '✔ BOUND', cls: 'border-emerald-500/50 text-emerald-400' }
    case 'tx_sent': return { label: '⏳ WAITING CHAIN', cls: 'border-copper/40 text-copper' }
    case 'proving': return { label: '⏳ PROVING', cls: 'border-copper/40 text-copper' }
    case 'conflict': return { label: '⚠ CONFLICT', cls: 'border-rust/60 text-rust' }
    case 'error': return { label: '! ERROR', cls: 'border-rust/60 text-rust' }
    default: return { label: 'NOT BOUND', cls: 'border-white/15 text-iron-200' }
  }
})

async function loadBindStatus() {
  const acc = account.currentAccount
  if (!acc) return
  try {
    bindState.value = await api('GET', `/accounts/${acc.id}/bind`)
    if (bindState.value.status === 'tx_sent' && !bindPoll) {
      bindPoll = setInterval(loadBindStatus, 10000)
    } else if (bindState.value.status !== 'tx_sent' && bindPoll) {
      clearInterval(bindPoll)
      bindPoll = null
    }
  } catch { /* сессия ещё не открыта */ }
}

async function bindNow() {
  const acc = account.currentAccount
  if (!acc || binding.value) return
  binding.value = true
  try {
    bindState.value = await api('POST', `/accounts/${acc.id}/bind`)
    if (bindState.value.status === 'tx_sent' && !bindPoll) {
      bindPoll = setInterval(loadBindStatus, 10000)
    }
  } catch (e) {
    bindState.value = { ...bindState.value, status: 'error', error: e.message }
  } finally {
    binding.value = false
  }
}

watch(() => account.activeAccountId, loadBindStatus)
onMounted(loadBindStatus)
onUnmounted(() => { if (bindPoll) clearInterval(bindPoll) })

const providers = [
  { id: 'gmail', label: 'Gmail', icon: Mail },
  { id: 'yandex', label: 'Yandex', icon: AtSign },
  { id: 'yahoo', label: 'Yahoo', icon: Globe },
  { id: 'custom', label: 'Custom IMAP', icon: Server },
]

const presets = {
  gmail:  { host: 'imap.gmail.com',         port: '993', smtpHost: 'smtp.gmail.com',         smtpPort: '465' },
  yandex: { host: 'imap.yandex.com',        port: '993', smtpHost: 'smtp.yandex.com',        smtpPort: '465' },
  yahoo:  { host: 'imap.mail.yahoo.com',    port: '993', smtpHost: 'smtp.mail.yahoo.com',    smtpPort: '465' },
  custom: { host: '', port: '', smtpHost: '', smtpPort: '' },
}

// Form state machine: null | 'create' | 'edit'
const formMode = ref(null)
const editingId = ref(null)
const formError = ref('')

const accountForm = reactive({
  provider: 'gmail',
  email: '',
  password: '',
  ...presets.gmail,
})

function resetForm(provider = 'gmail') {
  accountForm.provider = provider
  accountForm.email = ''
  accountForm.password = ''
  Object.assign(accountForm, presets[provider])
  formError.value = ''
}

function selectProviderInForm(id) {
  // Preserve email/password when switching provider; only override hosts
  accountForm.provider = id
  Object.assign(accountForm, presets[id])
}

function onToggleAddForm() {
  if (formMode.value === 'create') {
    closeForm()
  } else {
    editingId.value = null
    resetForm('gmail')
    formMode.value = 'create'
  }
}

function startEdit(a) {
  editingId.value = a.id
  formMode.value = 'edit'
  formError.value = ''
  accountForm.provider = a.provider || 'gmail'
  accountForm.email = a.email || ''
  accountForm.password = '' // never preload password — user re-enters or leaves blank
  accountForm.host = a.host || ''
  accountForm.port = a.port || ''
  accountForm.smtpHost = a.smtpHost || ''
  accountForm.smtpPort = a.smtpPort || ''
}

function closeForm() {
  formMode.value = null
  editingId.value = null
  resetForm('gmail')
}

async function saveNewAccount() {
  formError.value = ''
  if (!accountForm.email.includes('@')) {
    formError.value = 'Email is required.'
    return
  }
  if (!accountForm.password) {
    formError.value = 'Password / app key is required.'
    return
  }
  if (account.accounts.some((a) => a.email.toLowerCase() === accountForm.email.toLowerCase())) {
    formError.value = 'This mailbox is already added.'
    return
  }
  formError.value = 'Verifying IMAP login…'
  try {
    await account.addAccount({
      email: accountForm.email,
      password: accountForm.password,
      provider: accountForm.provider,
      host: accountForm.host,
      port: accountForm.port,
      smtpHost: accountForm.smtpHost,
      smtpPort: accountForm.smtpPort,
    })
    closeForm()
  } catch (e) {
    formError.value = e.message
  }
}

async function saveEdit() {
  formError.value = ''
  if (!accountForm.email.includes('@')) {
    formError.value = 'Email is required.'
    return
  }
  // duplicate check excluding the current account
  if (account.accounts.some((a) => a.id !== editingId.value && a.email.toLowerCase() === accountForm.email.toLowerCase())) {
    formError.value = 'Another account already uses this email.'
    return
  }
  formError.value = 'Verifying IMAP login…'
  try {
    await account.updateAccount(editingId.value, {
      email: accountForm.email,
      password: accountForm.password,
      provider: accountForm.provider,
      host: accountForm.host,
      port: accountForm.port,
      smtpHost: accountForm.smtpHost,
      smtpPort: accountForm.smtpPort,
    })
    closeForm()
  } catch (e) {
    formError.value = e.message
  }
}

async function confirmRemove(id) {
  if (account.accounts.length <= 1) {
    if (!confirm('This is your only mailbox. Removing it will leave you without an active account. Continue?')) return
  }
  try {
    await account.removeAccount(id)
    if (editingId.value === id) closeForm()
  } catch (e) {
    formError.value = e.message
  }
}

// auto-open add form via ?add=1 (from header switcher etc.)
onMounted(() => {
  if (route.query.add === '1') {
    formMode.value = 'create'
    resetForm('gmail')
    // strip the query param to keep URL clean
    router.replace({ name: 'settings' })
  }
})

// Экспорт seed через системный диалог «Сохранить как…» (только Tauri);
// вне Tauri диалог невозможен — кнопка скрыта
const canExportSeed = typeof window !== 'undefined' && !!window.__TAURI__?.core?.invoke

async function exportSeed() {
  const payload = {
    sthAddress: account.sthAddress,
    publicKey: account.publicKey,
    seed: account.seedWords,
    exportedAt: new Date().toISOString(),
  }
  const fileName = `oxid-seed-${account.sthAddress?.slice(0, 8) || 'export'}.json`
  try {
    const saved = await window.__TAURI__.core.invoke('export_seed_file', {
      fileName,
      contents: JSON.stringify(payload, null, 2),
    })
    if (saved) {
      toast.push({ kind: 'success', title: 'Seed exported', text: fileName })
    }
  } catch (e) {
    toast.push({ kind: 'error', title: 'Export failed', text: String(e) })
  }
}

function resetAll() {
  if (!confirm('This will erase all local data. Continue?')) return
  account.reset()
  localStorage.removeItem('oxid.mail')
  mail.$reset()
  router.push({ name: 'onboarding' })
}
</script>
