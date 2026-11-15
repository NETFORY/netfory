<script setup lang="ts">
// eslint-disable-next-line vue/no-import-compiler-macros -- линтер платформы не знает макросов <script setup>
import { defineProps, defineEmits, ref, computed, onMounted, onBeforeUnmount, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { walletInvoke } from '@/wallet/ipc'
import { getMyUsername, getRegistrationFee, usernameAvailable, registerUsername, openInMain, appListen } from '@/lib/bridge'

const props = defineProps<{ keyPublished: boolean; publishing: boolean; publishFee: string; address: string }>()
const emit = defineEmits<{ (e: 'publish-key'): void; (e: 'close'): void; (e: 'done'): void }>()
const { t } = useI18n()

const LS_PENDING = 'sth_chat_pending_name'
const balance = ref<number | null>(null)
const username = ref<string | null>(null)
const nameFee = ref(100)
const name = ref('')
const nameState = ref<'' | 'checking' | 'free' | 'taken' | 'reserved' | 'bad'>('')
const registering = ref(false)
const err = ref('')
const copied = ref(false)
const pendingName = ref<string>(loadPending())
let poll: ReturnType<typeof setInterval> | null = null
let unName: (() => void) | null = null
let checkTimer: ReturnType<typeof setTimeout> | null = null

function loadPending(): string {
  try {
    const p = JSON.parse(localStorage.getItem(LS_PENDING) || 'null')
    // регистрация «висит» не дольше 30 минут — дальше считаем неудачной
    return p && Date.now() - p.ts < 30 * 60_000 ? String(p.name) : ''
  } catch {
    return ''
  }
}

const TX_FEE = 0.25
const keyFee = computed(() => Number(props.publishFee) || TX_FEE)
const needName = computed(() => !username.value)
const needKey = computed(() => !props.keyPublished)
/** Сколько STH нужно на оставшиеся шаги. */
const need = computed(() => (needName.value && !pendingName.value ? nameFee.value + TX_FEE : 0) + (needKey.value ? keyFee.value : 0))
const hasFunds = computed(() => balance.value !== null && (need.value === 0 || balance.value >= need.value))
const canRegister = computed(() => balance.value !== null && balance.value >= nameFee.value + TX_FEE)
const canPublish = computed(() => balance.value !== null && balance.value >= keyFee.value)
const allDone = computed(() => !needName.value && !needKey.value)
const step = computed(() => (!hasFunds.value ? 1 : needName.value ? 2 : needKey.value ? 3 : 4))

function fmt(n: number) {
  return n.toLocaleString(undefined, { maximumFractionDigits: 4 })
}

async function refresh() {
  try {
    const acc = await walletInvoke<{ balance: string }>('get_active_smartnet_account')
    const b = Number(acc.balance)
    if (Number.isFinite(b)) balance.value = b
  } catch {
    /* узел недоступен — оставляем прошлое значение */
  }
  try {
    username.value = await getMyUsername()
    if (username.value && pendingName.value) {
      pendingName.value = ''
      localStorage.removeItem(LS_PENDING)
    }
  } catch {
    /* noop */
  }
}

function friendly(e: unknown): string {
  const code = typeof e === 'string' ? e : (e as any)?.message || ''
  const m = code.match(/^insufficient-balance:([\d.]+):([\d.]+)/)
  if (m) return t('messenger.onb.insufficient', { need: fmt(Number(m[1])), have: fmt(Number(m[2])) })
  return code || t('messenger.onb.failed')
}

watch(name, (v) => {
  err.value = ''
  if (checkTimer) clearTimeout(checkTimer)
  const clean = v.trim().toLowerCase()
  if (!clean) {
    nameState.value = ''
    return
  }
  if (!/^[a-z0-9]{2,24}(\.[a-z0-9]{1,24})?$/.test(clean) || clean.length > 24) {
    nameState.value = 'bad'
    return
  }
  nameState.value = 'checking'
  checkTimer = setTimeout(async () => {
    try {
      // reserved-имена (public/reserved.txt) показываем как занятые
      nameState.value = (await usernameAvailable(clean)) ? 'free' : 'taken'
    } catch {
      nameState.value = ''
    }
  }, 350)
})

async function doRegister() {
  const clean = name.value.trim().toLowerCase()
  if (!clean || nameState.value !== 'free' || !canRegister.value) return
  registering.value = true
  err.value = ''
  try {
    await registerUsername(clean)
    pendingName.value = clean
    localStorage.setItem(LS_PENDING, JSON.stringify({ name: clean, ts: Date.now() }))
    void refresh()
  } catch (e) {
    err.value = friendly(e)
  } finally {
    registering.value = false
  }
}

function openWallet() {
  openInMain('sn://wallet').catch(() => {})
}

async function copyAddr() {
  try {
    await navigator.clipboard.writeText(props.address)
    copied.value = true
    setTimeout(() => (copied.value = false), 1500)
  } catch {
    /* noop */
  }
}

// standalone-мессенджер (без Netfory): кошелька рядом нет — показываем только адрес для пополнения
const hasWalletApp = ref(true)
onMounted(async () => {
  walletInvoke<boolean>('msg_is_sidecar').then((v) => { hasWalletApp.value = v !== false }).catch(() => {})
  getRegistrationFee().then((f) => (nameFee.value = f)).catch(() => {})
  await refresh()
  poll = setInterval(refresh, 10_000)
  // подтверждение имени в цепочке — шаг закрывается сразу, без ожидания опроса
  appListen<{ name?: string }>('name-confirmed', () => void refresh()).then((un) => { unName = un }).catch(() => {})
})
onBeforeUnmount(() => {
  if (poll) clearInterval(poll)
  if (unName) unName()
  if (checkTimer) clearTimeout(checkTimer)
})
</script>

<template>
  <div class="onb" data-testid="chat-onboarding">
    <div class="onb-card">
      <div class="onb-head">
        <span class="onb-kicker">// NEW INTERNET</span>
        <button class="onb-x" @click="emit('close')" :title="t('messenger.onb.later')" data-testid="chat-onb-close">✕</button>
      </div>
      <h2 class="onb-title">{{ allDone ? t('messenger.onb.doneTitle') : t('messenger.onb.title') }}</h2>
      <p class="onb-sub">{{ allDone ? t('messenger.onb.doneSub') : t('messenger.onb.sub') }}</p>

      <ol class="steps">
        <!-- 1. Баланс -->
        <li class="step" :class="{ done: hasFunds, active: step === 1 }" data-testid="chat-onb-step-balance">
          <span class="dot">{{ hasFunds ? '✓' : '1' }}</span>
          <div class="body">
            <div class="row">
              <span class="name">{{ t('messenger.onb.s1') }}</span>
              <span class="mono val" data-testid="chat-onb-balance">{{ balance === null ? '…' : fmt(balance) }} STH</span>
            </div>
            <p v-if="!hasFunds" class="hint">{{ t('messenger.onb.s1hint', { need: fmt(need) }) }}</p>
            <div v-if="!hasFunds" class="acts">
              <button v-if="address" class="btn ghost mono" @click="copyAddr" data-testid="chat-onb-copy-addr">{{ copied ? '✓' : address.slice(0, 10) + '…' + address.slice(-6) }}</button>
              <button v-if="hasWalletApp" class="btn" @click="openWallet" data-testid="chat-onb-open-wallet">{{ t('messenger.onb.topUp') }}</button>
              <span v-else class="hint" data-testid="chat-onb-topup-hint">{{ t('messenger.onb.topUpStandalone') }}</span>
            </div>
          </div>
        </li>

        <!-- 2. Имя -->
        <li class="step" :class="{ done: !needName, active: step === 2 }" data-testid="chat-onb-step-name">
          <span class="dot">{{ !needName ? '✓' : '2' }}</span>
          <div class="body">
            <div class="row">
              <span class="name">{{ t('messenger.onb.s2') }}</span>
              <span v-if="username" class="mono val ok">u://{{ username }}</span>
              <span v-else class="mono val dim">{{ fmt(nameFee) }} STH</span>
            </div>
            <template v-if="needName">
              <p v-if="pendingName" class="hint pending" data-testid="chat-onb-name-pending">⏳ {{ t('messenger.onb.pending', { name: pendingName }) }}</p>
              <template v-else>
                <p class="hint">{{ t('messenger.onb.s2hint') }}</p>
                <div class="acts">
                  <div class="inp-wrap">
                    <span class="pfx mono">u://</span>
                    <input v-model="name" class="mono" maxlength="24" :placeholder="t('messenger.onb.namePh')" :disabled="registering" @keydown.enter="doRegister" data-testid="chat-onb-name-input" />
                    <span class="st mono" :class="nameState" data-testid="chat-onb-name-state">{{ nameState ? t('messenger.onb.st_' + nameState) : '' }}</span>
                  </div>
                  <button class="btn" :disabled="registering || nameState !== 'free' || !canRegister" @click="doRegister" data-testid="chat-onb-register">
                    {{ registering ? t('messenger.onb.registering') : t('messenger.onb.register') }}
                  </button>
                </div>
              </template>
            </template>
          </div>
        </li>

        <!-- 3. Ключ -->
        <li class="step" :class="{ done: !needKey, active: step === 3 }" data-testid="chat-onb-step-key">
          <span class="dot">{{ !needKey ? '✓' : '3' }}</span>
          <div class="body">
            <div class="row">
              <span class="name">{{ t('messenger.onb.s3') }}</span>
              <span class="mono val" :class="needKey ? 'dim' : 'ok'">{{ needKey ? '~' + fmt(keyFee) + ' STH' : t('messenger.keyOnline') }}</span>
            </div>
            <template v-if="needKey">
              <p class="hint">{{ t('messenger.onb.s3hint') }}</p>
              <div class="acts">
                <button class="btn" :disabled="publishing || !canPublish" @click="emit('publish-key')" data-testid="chat-onb-publish-key">
                  {{ publishing ? t('messenger.publishing') : t('messenger.publishKey') }}
                </button>
              </div>
            </template>
          </div>
        </li>

        <!-- 4. Общайтесь -->
        <li class="step" :class="{ done: allDone, active: step === 4 }" data-testid="chat-onb-step-chat">
          <span class="dot">{{ allDone ? '✓' : '4' }}</span>
          <div class="body">
            <div class="row"><span class="name">{{ t('messenger.onb.s4') }}</span></div>
            <p class="hint">{{ t('messenger.onb.s4hint') }}</p>
          </div>
        </li>
      </ol>

      <p v-if="err" class="onb-err" data-testid="chat-onb-error">{{ err }}</p>

      <div class="onb-foot">
        <button v-if="allDone" class="btn big" @click="emit('done')" data-testid="chat-onb-done">{{ t('messenger.onb.start') }} →</button>
        <button v-else class="later" @click="emit('close')" data-testid="chat-onb-later">{{ t('messenger.onb.later') }}</button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.onb {
  position: absolute;
  inset: 0;
  z-index: 60;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  background: rgba(6, 8, 12, 0.72);
  backdrop-filter: blur(10px);
  -webkit-backdrop-filter: blur(10px);
}
.onb-card {
  width: 560px;
  max-width: 100%;
  max-height: 100%;
  overflow: auto;
  background: var(--panel, #0f1318);
  border: 1px solid var(--border, #1f2937);
  border-radius: 14px;
  padding: 22px 24px 18px;
  box-shadow: 0 24px 64px rgba(0, 0, 0, 0.55);
  animation: onb-in 0.32s cubic-bezier(0.2, 0.8, 0.2, 1);
}
@keyframes onb-in {
  from { opacity: 0; transform: translateY(14px) scale(0.98); }
  to { opacity: 1; transform: none; }
}
.onb-head { display: flex; justify-content: space-between; align-items: center; }
.onb-kicker { font-size: 10px; letter-spacing: 0.22em; color: var(--accent); font-weight: var(--fw-800); }
.onb-x { background: none; border: none; color: var(--secondary); cursor: pointer; font-size: 14px; padding: 4px 6px; }
.onb-x:hover { color: var(--text); }
.onb-title { margin: 10px 0 4px; font-size: 22px; font-weight: var(--fw-800); letter-spacing: -0.01em; color: var(--text); }
.onb-sub { margin: 0 0 18px; font-size: 13px; line-height: 1.5; color: var(--secondary); }
.steps { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 8px; }
.step {
  display: flex;
  gap: 12px;
  padding: 12px 14px;
  border-radius: 10px;
  border: 1px solid var(--border, #1f2937);
  background: rgba(255, 255, 255, 0.02);
  opacity: 0.55;
  transition: opacity 0.2s, border-color 0.2s, background-color 0.2s;
}
.step.active { opacity: 1; border-color: color-mix(in srgb, var(--accent) 55%, transparent); background: color-mix(in srgb, var(--accent) 7%, transparent); }
.step.done { opacity: 0.85; }
.dot {
  flex: none;
  width: 24px;
  height: 24px;
  border-radius: 50%;
  display: grid;
  place-items: center;
  font-size: 12px;
  font-weight: var(--fw-800);
  color: var(--text);
  background: rgba(255, 255, 255, 0.08);
}
.step.done .dot { background: var(--accent); color: #04110f; }
.step.active .dot { background: var(--accent); color: #04110f; box-shadow: 0 0 0 4px color-mix(in srgb, var(--accent) 22%, transparent); }
.body { flex: 1; min-width: 0; }
.row { display: flex; justify-content: space-between; align-items: baseline; gap: 10px; }
.name { font-size: 13.5px; font-weight: var(--fw-700); color: var(--text); }
.val { font-size: 12px; color: var(--text); }
.val.ok { color: var(--accent); }
.dim { color: var(--secondary); }
.hint { margin: 6px 0 0; font-size: 12px; line-height: 1.45; color: var(--secondary); }
.hint.pending { color: #f0b429; }
.acts { display: flex; gap: 8px; margin-top: 10px; flex-wrap: wrap; align-items: center; }
.btn {
  border: none;
  border-radius: 8px;
  padding: 8px 14px;
  font-size: 11.5px;
  font-weight: var(--fw-800);
  letter-spacing: 0.06em;
  text-transform: uppercase;
  cursor: pointer;
  color: #04110f;
  background: var(--accent);
  transition: transform 0.12s, opacity 0.15s;
}
.btn:hover:not(:disabled) { transform: translateY(-1px); }
.btn:disabled { opacity: 0.4; cursor: not-allowed; }
.btn.ghost { background: transparent; color: var(--text); border: 1px solid var(--border, #1f2937); text-transform: none; letter-spacing: 0; }
.btn.big { padding: 12px 22px; font-size: 13px; width: 100%; }
.inp-wrap { flex: 1; min-width: 200px; display: flex; align-items: center; gap: 6px; background: rgba(0, 0, 0, 0.35); border: 1px solid var(--border, #1f2937); border-radius: 8px; padding: 0 10px; }
.inp-wrap:focus-within { border-color: var(--accent); }
.pfx { color: var(--secondary); font-size: 12px; }
.inp-wrap input { flex: 1; min-width: 0; background: none; border: none; outline: none; color: var(--text); font-size: 13px; padding: 8px 0; }
.st { font-size: 10px; letter-spacing: 0.08em; text-transform: uppercase; }
.st.free { color: var(--accent); }
.st.taken, .st.reserved, .st.bad { color: #f87171; }
.st.checking { color: var(--secondary); }
.onb-err { margin: 12px 0 0; padding: 8px 12px; border-radius: 8px; background: #450a0a; color: #fca5a5; font-size: 12px; }
.onb-foot { margin-top: 16px; display: flex; justify-content: center; }
.later { background: none; border: none; color: var(--secondary); font-size: 12px; cursor: pointer; text-decoration: underline dotted; }
.later:hover { color: var(--text); }
.mono { font-family: 'JetBrains Mono', ui-monospace, monospace; }
</style>
