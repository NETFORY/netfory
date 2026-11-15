<template>
  <div class="relative min-h-screen flex items-center justify-center p-4 sm:p-8" data-testid="onboarding-root">
    <!-- Background type accents -->
    <div class="pointer-events-none absolute inset-0 select-none overflow-hidden">
      <div class="absolute -top-24 -right-24 w-[420px] h-[420px] rounded-full bg-rust/10 blur-3xl" />
      <div class="absolute -bottom-24 -left-24 w-[420px] h-[420px] rounded-full bg-copper/10 blur-3xl" />
    </div>

    <div class="relative panel w-full max-w-3xl rounded-2xl p-6 sm:p-10 shadow-panel" data-testid="onboarding-card">
      <!-- Header -->
      <div class="flex items-center justify-between mb-8">
        <div class="flex items-center gap-3">
          <div class="w-9 h-9 rounded-md bg-rust grid place-items-center shadow-rust-glow">
            <ShieldCheck class="w-5 h-5 text-white" :stroke-width="2.2" />
          </div>
          <div>
            <div class="font-mono text-[11px] uppercase tracking-[0.24em] text-iron-200">OXID // v0.1</div>
            <div class="text-lg font-semibold">Zero-Server Onboarding</div>
          </div>
        </div>
        <button class="btn-ghost text-xs mono" @click="theme.toggle" data-testid="theme-toggle-onboarding">
          <Moon v-if="theme.mode === 'dark'" class="w-4 h-4" />
          <Sun v-else class="w-4 h-4" />
          {{ theme.mode === 'dark' ? 'DARK' : 'LIGHT' }}
        </button>
      </div>

      <!-- Stepper -->
      <ol class="flex items-center gap-3 mb-8 font-mono text-[10px] uppercase tracking-[0.2em]">
        <li
          v-for="(label, idx) in ['Seed', 'Provider', 'Confirm']"
          :key="label"
          class="flex items-center gap-3"
        >
          <span
            class="w-6 h-6 grid place-items-center rounded-sm border"
            :class="step > idx ? 'bg-rust border-rust text-white' : (step === idx ? 'border-rust text-rust' : 'border-white/15 text-iron-200')"
          >
            {{ idx + 1 }}
          </span>
          <span :class="step >= idx ? 'text-iron-50' : 'text-iron-200'">{{ label }}</span>
          <span v-if="idx < 2" class="w-8 h-px bg-white/10" />
        </li>
      </ol>

      <!-- Step 0: BIP-39 -->
      <section v-if="step === 0" data-testid="step-seed">
        <h1 class="text-3xl sm:text-4xl font-semibold tracking-tight mb-2">
          Forge your <span class="text-rust">12-word</span> key.
        </h1>
        <p class="text-iron-200 text-sm mb-6 max-w-prose">
          No accounts. No servers. Your identity is derived locally from a BIP-39 seed.
          Write it down. Lose it and the keys are gone.
        </p>

        <div class="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-4 gap-2.5 mb-5" data-testid="seed-grid">
          <label
            v-for="(word, i) in seedWords"
            :key="i"
            class="relative panel rounded-md px-3 py-2 flex items-center gap-2 focus-within:border-rust focus-within:shadow-rust-glow transition"
          >
            <span class="font-mono text-[10px] text-iron-200 w-5">{{ String(i + 1).padStart(2, '0') }}</span>
            <input
              v-model="seedWords[i]"
              :placeholder="`word`"
              class="bg-transparent flex-1 outline-none font-mono text-sm text-iron-50 placeholder:text-iron-300/60"
              autocomplete="off"
              spellcheck="false"
              :data-testid="`seed-word-${i}`"
            />
          </label>
        </div>

        <div class="flex flex-wrap items-center gap-3">
          <button class="btn-rust" @click="generate" data-testid="generate-seed-btn">
            <Sparkles class="w-4 h-4" /> Generate seed
          </button>
          <button class="btn-ghost" @click="clearSeed" data-testid="clear-seed-btn">
            <Eraser class="w-4 h-4" /> Clear
          </button>
          <button
            v-if="isTauri"
            class="btn-ghost border border-copper/40 text-copper hover:shadow-rust-glow"
            :disabled="connectingSmartnet"
            @click="connectSmartnet"
            data-testid="connect-smartnet-btn"
          >
            <Link2 class="w-4 h-4" /> {{ connectingSmartnet ? 'Connecting…' : 'Connect SmartNet' }}
          </button>
          <span class="ml-auto text-xs font-mono text-iron-200">
            <span class="text-copper">{{ filledCount }}</span>/12 entered
          </span>
        </div>

        <p v-if="smartnetError" class="mt-4 text-xs font-mono text-rust" data-testid="smartnet-error">{{ smartnetError }}</p>
        <p v-if="seedError" class="mt-4 text-xs font-mono text-rust" data-testid="seed-error">{{ seedError }}</p>

        <div class="mt-8 flex justify-end">
          <button class="btn-rust" :disabled="filledCount < 12 || validating" @click="goProvider" data-testid="seed-next-btn">
            {{ validating ? 'Validating…' : 'Continue' }} <ArrowRight class="w-4 h-4" />
          </button>
        </div>
      </section>

      <!-- Step 1: Provider -->
      <section v-else-if="step === 1" data-testid="step-provider">
        <h1 class="text-3xl sm:text-4xl font-semibold tracking-tight mb-2">
          Plug in your <span class="text-rust">first mailbox</span>.
        </h1>
        <p class="text-iron-200 text-sm mb-6 max-w-prose">
          Pick a provider. We store credentials locally — they never leave your device.
        </p>

        <div class="grid grid-cols-2 sm:grid-cols-4 gap-3 mb-6">
          <button
            v-for="p in providers"
            :key="p.id"
            class="panel rounded-lg p-4 text-left transition group"
            :class="providerId === p.id ? 'border-rust shadow-rust-glow' : 'hover:border-white/20'"
            @click="selectProvider(p.id)"
            :data-testid="`provider-${p.id}`"
          >
            <component :is="p.icon" class="w-5 h-5 mb-2" :class="providerId === p.id ? 'text-rust' : 'text-iron-100'" />
            <div class="font-semibold text-sm">{{ p.label }}</div>
            <div class="font-mono text-[10px] uppercase tracking-[0.18em] text-iron-200 mt-1">{{ p.tag }}</div>
          </button>
        </div>

        <!-- Gmail: только вход по app password (OAuth-кнопка скрыта по решению 2026-06) -->

        <div class="grid grid-cols-1 sm:grid-cols-2 gap-3">
          <div class="sm:col-span-2">
            <label class="block text-[10px] uppercase tracking-[0.2em] font-mono text-iron-200 mb-1.5">Email address</label>
            <input v-model="form.email" type="email" placeholder="you@example.com" class="input-base" data-testid="acc-email" />
          </div>
          <div class="sm:col-span-2">
            <label class="block text-[10px] uppercase tracking-[0.2em] font-mono text-iron-200 mb-1.5">Password / App key</label>
            <input v-model="form.password" type="password" placeholder="•••••••••••" class="input-base" data-testid="acc-password" />
          </div>

          <div v-if="help" class="sm:col-span-2 panel rounded-md p-3 border border-copper/20" data-testid="provider-help">
            <div class="flex items-center gap-2 mb-1.5">
              <KeyRound class="w-3.5 h-3.5 text-copper" />
              <span class="font-mono text-[10px] uppercase tracking-[0.2em] text-copper">{{ help.title }}</span>
            </div>
            <ol class="text-[11px] text-iron-200 list-decimal list-inside space-y-0.5 mb-2">
              <li v-for="s in help.steps" :key="s">{{ s }}</li>
            </ol>
            <button
              type="button"
              class="font-mono text-[11px] text-copper underline decoration-dotted break-all text-left"
              @click="copyHelpUrl"
              title="Click to copy"
              data-testid="provider-help-url"
            >
              {{ helpCopied ? '✓ copied to clipboard' : help.url }}
            </button>
          </div>

          <template v-if="providerId === 'custom'">
            <div>
              <label class="block text-[10px] uppercase tracking-[0.2em] font-mono text-iron-200 mb-1.5">IMAP host</label>
              <input v-model="form.host" placeholder="imap.example.com" class="input-base" data-testid="acc-imap-host" />
            </div>
            <div>
              <label class="block text-[10px] uppercase tracking-[0.2em] font-mono text-iron-200 mb-1.5">IMAP port</label>
              <input v-model="form.port" placeholder="993" class="input-base" data-testid="acc-imap-port" />
            </div>
            <div>
              <label class="block text-[10px] uppercase tracking-[0.2em] font-mono text-iron-200 mb-1.5">SMTP host</label>
              <input v-model="form.smtpHost" placeholder="smtp.example.com" class="input-base" data-testid="acc-smtp-host" />
            </div>
            <div>
              <label class="block text-[10px] uppercase tracking-[0.2em] font-mono text-iron-200 mb-1.5">SMTP port</label>
              <input v-model="form.smtpPort" placeholder="465" class="input-base" data-testid="acc-smtp-port" />
            </div>
          </template>
        </div>

        <div class="mt-8 flex items-center justify-between">
          <button class="btn-ghost" @click="step = 0" data-testid="back-to-seed-btn">
            <ArrowLeft class="w-4 h-4" /> Back
          </button>
          <button class="btn-rust" :disabled="!canSubmitProvider" @click="step = 2" data-testid="provider-next-btn">
            Continue <ArrowRight class="w-4 h-4" />
          </button>
        </div>
      </section>

      <!-- Step 2: Confirm -->
      <section v-else data-testid="step-confirm">
        <h1 class="text-3xl sm:text-4xl font-semibold tracking-tight mb-2">
          <span class="text-rust">Forged.</span> Ready to ignite.
        </h1>
        <p class="text-iron-200 text-sm mb-6">Your identity is now bound to this device.</p>

        <div class="panel rounded-lg p-5 space-y-4 font-mono text-sm">
          <div>
            <div class="text-[10px] uppercase tracking-[0.22em] text-iron-200 mb-1">STH Address</div>
            <div class="text-rust break-all" data-testid="confirm-sth">{{ account.sthAddress }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-[0.22em] text-iron-200 mb-1">Mailbox</div>
            <div data-testid="confirm-email">{{ form.email || '—' }}</div>
            <div class="text-iron-200 text-xs">{{ providerLabel }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-[0.22em] text-iron-200 mb-1">Public key</div>
            <div class="text-copper break-all text-xs">{{ account.publicKey }}</div>
          </div>
        </div>

        <p v-if="finishError" class="mt-6 text-xs font-mono text-rust break-all" data-testid="finish-error">{{ finishError }}</p>

        <div class="mt-8 flex items-center justify-between">
          <button class="btn-ghost" @click="step = 1" :disabled="finishing" data-testid="back-to-provider-btn">
            <ArrowLeft class="w-4 h-4" /> Back
          </button>
          <button class="btn-rust" @click="finish" :disabled="finishing" data-testid="finish-onboarding-btn">
            <Zap class="w-4 h-4" /> {{ finishing ? 'Verifying IMAP…' : 'Enter Oxid Mail' }}
          </button>
        </div>
      </section>
    </div>
  </div>
</template>

<script setup>
import { ref, computed, reactive } from 'vue'
import { useRouter } from 'vue-router'
import {
  ShieldCheck, Sparkles, Eraser, ArrowRight, ArrowLeft, Zap, Moon, Sun,
  Mail, AtSign, Server, Globe, KeyRound, Link2,
} from 'lucide-vue-next'
import { useAccountStore } from '../stores/account'
import { useThemeStore } from '../stores/theme'
import { providerHelp } from '../lib/providerHelp'

const account = useAccountStore()
const theme = useThemeStore()
const router = useRouter()

account.hydrate()

const step = ref(0)
const seedWords = ref(Array.from({ length: 12 }, () => ''))
const providerId = ref('gmail')

// Connect SmartNet: pull-запрос seed по IPC-каналу (запуск из SmartNet)
const isTauri = typeof window !== 'undefined' && !!window.__TAURI__?.core
const connectingSmartnet = ref(false)
const smartnetError = ref('')
async function connectSmartnet() {
  const core = window.__TAURI__?.core
  if (!core?.invoke) return
  connectingSmartnet.value = true
  smartnetError.value = ''
  try {
    await core.invoke('request_smartnet_seed')
    for (let i = 0; i < 12; i++) {
      await new Promise((r) => setTimeout(r, 250))
      const mnemonic = await core.invoke('handoff_take')
      if (mnemonic) {
        await account.adoptHandoff(mnemonic)
        router.replace('/mail')
        return
      }
    }
    smartnetError.value =
      'SmartNet не отвечает: запустите почту кнопкой «Почта» из SmartNet и убедитесь, что кошелёк разблокирован.'
  } catch (e) {
    smartnetError.value = e.message
  } finally {
    connectingSmartnet.value = false
  }
}

const providers = [
  { id: 'gmail', label: 'Gmail', tag: 'App password', icon: Mail },
  { id: 'yandex', label: 'Yandex', tag: 'App password', icon: AtSign },
  { id: 'yahoo', label: 'Yahoo', tag: 'App password', icon: Globe },
  { id: 'custom', label: 'Custom IMAP', tag: 'Manual setup', icon: Server },
]

const presets = {
  gmail: { host: 'imap.gmail.com', port: '993', smtpHost: 'smtp.gmail.com', smtpPort: '465' },
  yandex: { host: 'imap.yandex.com', port: '993', smtpHost: 'smtp.yandex.com', smtpPort: '465' },
  yahoo: { host: 'imap.mail.yahoo.com', port: '993', smtpHost: 'smtp.mail.yahoo.com', smtpPort: '465' },
  custom: { host: '', port: '', smtpHost: '', smtpPort: '' },
}

const form = reactive({
  email: '',
  password: '',
  host: presets.gmail.host,
  port: presets.gmail.port,
  smtpHost: presets.gmail.smtpHost,
  smtpPort: presets.gmail.smtpPort,
})

const filledCount = computed(() => seedWords.value.filter((w) => w.trim().length > 0).length)
const canSubmitProvider = computed(() => form.email.includes('@') && form.password.length > 0)
const providerLabel = computed(() => providers.find((p) => p.id === providerId.value)?.label || '')
const help = computed(() => providerHelp[providerId.value] || null)
const helpCopied = ref(false)

function copyHelpUrl() {
  if (!help.value) return
  navigator.clipboard?.writeText(help.value.url).catch(() => {})
  helpCopied.value = true
  window.setTimeout(() => { helpCopied.value = false }, 1500)
}

function selectProvider(id) {
  providerId.value = id
  Object.assign(form, presets[id])
}

const seedError = ref('')
const validating = ref(false)
const finishing = ref(false)
const finishError = ref('')

async function generate() {
  seedError.value = ''
  try {
    const words = await account.generateSeed()
    seedWords.value = [...words]
  } catch (e) {
    seedError.value = e.message
  }
}

function clearSeed() {
  seedWords.value = Array.from({ length: 12 }, () => '')
  seedError.value = ''
}

// Валидация BIP-39 (checksum) в Rust-ядре перед переходом к провайдеру
async function goProvider() {
  seedError.value = ''
  validating.value = true
  try {
    await account.importSeed(seedWords.value)
    step.value = 1
  } catch (e) {
    seedError.value = e.message
  } finally {
    validating.value = false
  }
}

async function finish() {
  finishing.value = true
  finishError.value = ''
  try {
    await account.openSession()
    await account.addAccount({
      email: form.email,
      password: form.password,
      provider: providerId.value,
      host: form.host,
      port: form.port,
      smtpHost: form.smtpHost,
      smtpPort: form.smtpPort,
    })
    account.completeOnboarding()
    router.push({ name: 'mail' })
  } catch (e) {
    finishError.value = e.message
  } finally {
    finishing.value = false
  }
}
</script>
