<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, computed, watch, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { usernameAvailable, registerUsername, resolveUsername, getRegistrationFee, friendlyTxError } from '@/lib/bridge'
import { shortAddr } from '@/lib/crypto'
import TxConfirmModal from '@/components/TxConfirmModal.vue'

const { t } = useI18n()
const emit = defineEmits<{ (e: 'close'): void; (e: 'registered', name: string): void }>()

// Public on-chain registry sink (payment recipient for name claims).
const NAME_SINK = 'SeeQyvfpghaFLjVfLUTxLvpQq78LMQnAmE'

const raw = ref('')
const checking = ref(false)
const available = ref<boolean | null>(null)
const ownerAddr = ref('')
const fee = ref(100)
const submitting = ref(false)
const confirming = ref(false)
const error = ref('')
const txid = ref('')

onMounted(async () => {
  fee.value = await getRegistrationFee()
})

// Sanitize on every keystroke: lowercase, only [a-z0-9.] and at most one dot.
function sanitize(v: string): string {
  let s = v.toLowerCase().replace(/[^a-z0-9.]/g, '')
  const firstDot = s.indexOf('.')
  if (firstDot !== -1) {
    s = s.slice(0, firstDot + 1) + s.slice(firstDot + 1).replace(/\./g, '')
  }
  return s.slice(0, 24)
}
const valid = computed(() => /^[a-z0-9.]{2,24}$/.test(raw.value) && (raw.value.match(/\./g) || []).length <= 1)

let timer: any = null
watch(raw, (v) => {
  raw.value = sanitize(v)
  available.value = null
  ownerAddr.value = ''
  error.value = ''
  if (timer) clearTimeout(timer)
  if (!valid.value) return
  checking.value = true
  timer = setTimeout(async () => {
    try {
      const free = await usernameAvailable(raw.value)
      available.value = free
      // When taken, resolve the owner address to show who holds the name.
      ownerAddr.value = free ? '' : (await resolveUsername(raw.value)) || ''
    } finally {
      checking.value = false
    }
  }, 300)
})

async function submit() {
  if (!valid.value || available.value === false || submitting.value) return
  // Show the explicit transaction-confirmation step before broadcasting.
  confirming.value = true
}

async function doRegister() {
  if (submitting.value) return
  submitting.value = true
  error.value = ''
  try {
    txid.value = await registerUsername(raw.value)
    confirming.value = false
    emit('registered', raw.value)
  } catch (e: any) {
    error.value = friendlyTxError(e, 'Ошибка регистрации')
    confirming.value = false
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <div class="reg-back" @click.self="emit('close')" data-testid="name-register-modal">
    <div class="reg-term">
      <div class="reg-head">
        <span class="reg-dot" /><span class="reg-dot amber" /><span class="reg-dot green" />
        <span class="reg-title">{{ t('naming.terminal') }}</span>
      </div>

      <template v-if="!txid">
        <p class="reg-intro">{{ t('naming.intro') }}</p>
        <div class="reg-input-row">
          <span class="reg-proto">u://</span>
          <input
            v-model="raw"
            class="reg-input"
            :placeholder="t('naming.placeholder')"
            autofocus
            data-testid="name-register-input"
            @keyup.enter="submit"
          />
        </div>
        <div class="reg-status" data-testid="name-register-status">
          <span v-if="!raw" class="dim">{{ t('naming.rule') }}</span>
          <span v-else-if="!valid" class="bad">✕ {{ t('naming.invalid') }}</span>
          <span v-else-if="checking" class="dim">… {{ t('naming.checking') }}</span>
          <span v-else-if="available === true" class="ok">{{ t('naming.freeToRegister') }}</span>
          <span v-else-if="available === false" class="bad" data-testid="name-register-takenby">
            {{ ownerAddr ? t('naming.takenBy', { addr: shortAddr(ownerAddr, 8, 6) }) : `🔴 ${t('naming.taken')}` }}
          </span>
        </div>
        <p v-if="error" class="reg-error" data-testid="name-register-error">{{ error }}</p>
        <div class="reg-actions">
          <button class="btn" @click="emit('close')" data-testid="name-register-cancel">{{ t('common.cancel') }}</button>
          <button
            class="btn btn-primary"
            :disabled="!valid || available === false || submitting"
            @click="submit"
            data-testid="name-register-submit"
          >
            {{ submitting ? t('naming.broadcasting') : `${t('naming.register')} (${fee} STH)` }}
          </button>
        </div>
        <p class="reg-fee dim" data-testid="name-register-fee">{{ t('naming.feeLabel', { amount: fee }) }}</p>
        <p class="reg-fee dim">{{ t('naming.fee') }}</p>
      </template>

      <template v-else>
        <div class="reg-done" data-testid="name-register-done">
          <span class="reg-check">✓</span>
          <p class="reg-done-name">u://{{ raw }}</p>
          <p class="dim">{{ t('naming.pending') }}</p>
          <code class="reg-txid">{{ txid }}</code>
          <button class="btn btn-primary" @click="emit('close')" data-testid="name-register-close">{{ t('common.close') }}</button>
        </div>
      </template>
    </div>

    <TxConfirmModal
      v-if="confirming"
      :title="t('naming.confirmPurpose', { name: raw })"
      :recipient="NAME_SINK"
      :amount="fee"
      :vendor="raw"
      :busy="submitting"
      @confirm="doRegister"
      @cancel="confirming = false"
    />
  </div>
</template>

<style scoped>
.reg-back {
  position: fixed;
  inset: 0;
  background: rgba(2, 6, 12, 0.78);
  backdrop-filter: blur(6px);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 300;
}
.reg-term {
  width: 440px;
  max-width: 92vw;
  background: #070b11;
  border: 1px solid var(--accent);
  border-radius: 14px;
  box-shadow: 0 0 40px -8px var(--accent);
  padding: 18px;
  font-family: monospace;
}
.reg-head {
  display: flex;
  align-items: center;
  gap: 7px;
  margin-bottom: 16px;
}
.reg-dot {
  width: 11px;
  height: 11px;
  border-radius: 50%;
  background: #ef4444;
}
.reg-dot.amber { background: #f59e0b; }
.reg-dot.green { background: #22c55e; }
.reg-title {
  margin-left: 10px;
  font-size: 12px;
  letter-spacing: 0.1em;
  color: var(--accent);
}
.reg-intro {
  font-size: 13px;
  color: var(--text-dim);
  margin-bottom: 14px;
}
.reg-input-row {
  display: flex;
  align-items: center;
  border: 1px solid var(--border);
  border-radius: 9px;
  background: #04070c;
  padding: 0 12px;
}
.reg-proto {
  color: var(--accent);
  font-size: 16px;
}
.reg-input {
  flex: 1;
  background: transparent;
  border: none;
  outline: none;
  color: #e8fbff;
  font-family: monospace;
  font-size: 18px;
  padding: 12px 8px;
  letter-spacing: 0.04em;
}
.reg-status {
  min-height: 22px;
  margin: 10px 2px;
  font-size: 12px;
}
.ok { color: #22c55e; }
.bad { color: #ef4444; }
.reg-error {
  color: #ef4444;
  font-size: 12px;
  margin: 6px 2px;
}
.reg-actions {
  display: flex;
  gap: 10px;
  justify-content: flex-end;
  margin-top: 8px;
}
.reg-fee {
  font-size: 11px;
  margin-top: 10px;
}
.reg-done {
  text-align: center;
  padding: 12px 0;
}
.reg-check {
  display: inline-block;
  font-size: 40px;
  color: #22c55e;
}
.reg-done-name {
  font-size: 20px;
  color: var(--accent);
  margin: 8px 0;
}
.reg-txid {
  display: block;
  font-size: 10px;
  color: var(--text-dim);
  word-break: break-all;
  margin: 12px 0;
}
</style>
