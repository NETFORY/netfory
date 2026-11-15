<script setup lang="ts">
import { onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useAuthStore } from '@/store/auth'
import { getMyUsername } from '@/lib/bridge'
import LanguageSwitcher from '@/components/LanguageSwitcher.vue'
import UserAvatar from '@/components/UserAvatar.vue'

const { t } = useI18n()
const auth = useAuthStore()
const pin = ref('')
const error = ref('')
const checking = ref(false)
// Зарегистрированное имя (u://) — читается из локального кэша без разблокировки.
const myName = ref('')

onMounted(async () => {
  try {
    myName.value = (await getMyUsername()) || ''
  } catch {
    myName.value = ''
  }
})

const MIN_PIN = 4

async function decrypt() {
  error.value = ''
  checking.value = true
  try {
    const ok = await auth.unlock(pin.value)
    if (!ok) {
      error.value = t('unlock.wrongPin')
      pin.value = ''
    }
  } finally {
    checking.value = false
  }
}

// Auto-submit: as soon as a correct PIN is typed, unlock immediately without
// pressing the button. Attempts are SILENT for errors (a wrong/partial PIN shows
// none), but a loading indicator IS shown so the app never looks frozen.
watch(pin, async (val) => {
  if (val.length < MIN_PIN) return
  error.value = ''
  checking.value = true
  try {
    await auth.unlock(val, true)
  } finally {
    checking.value = false
  }
})
</script>

<template>
  <div class="auth-wrap" data-testid="unlock-layer">
    <div class="auth-top">
      <LanguageSwitcher />
    </div>

    <div class="card rise" data-testid="unlock-card">
      <div class="card-head">
        <div class="head-line">
          <span class="lock-ico">🔒</span>
          <span class="head-title">{{ t('unlock.header') }}</span>
          <span class="locked-tag">// {{ t('unlock.locked') }}</span>
        </div>
      </div>

      <div class="card-body">
        <!-- профиль владельца: аватар + имя (если имя зарегистрировано) -->
        <div v-if="myName" class="who" data-testid="unlock-profile">
          <span class="who-ring">
            <UserAvatar :address="auth.address" :size="72" />
          </span>
          <span class="who-name mono">@{{ myName }}</span>
        </div>

        <div class="addr-box" data-testid="unlock-address">
          <span class="mono-label">{{ t('unlock.walletAddress') }}</span>
          <code class="hl">{{ auth.address }}</code>
        </div>

        <label class="mono-label">{{ t('unlock.enterPin') }}</label>
        <input
          v-model="pin"
          type="password"
          inputmode="numeric"
          class="field pin-field"
          placeholder="••••••"
          @keyup.enter="decrypt"
          autofocus
          data-testid="unlock-pin-input"
        />

        <p v-if="error" class="err" data-testid="unlock-error">{{ error }}</p>
        <p v-else-if="auth.busy || checking" class="dim decrypting" data-testid="unlock-busy">
          <span class="spinner" /> {{ t('unlock.decrypting') }}
        </p>

        <button
          class="btn btn-primary btn-block big"
          :disabled="auth.busy || checking || !pin"
          @click="decrypt"
          data-testid="decrypt-vault-btn"
        >
          {{ t('unlock.decrypt') }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.auth-wrap {
  min-height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 40px 20px;
  position: relative;
}
.auth-top {
  position: absolute;
  top: 22px;
  right: 26px;
}
.card {
  width: 100%;
  max-width: 460px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow);
}
.card-head {
  padding: 18px 22px;
  border-bottom: 1px solid var(--border);
  background: var(--panel-2);
}
.head-line {
  display: flex;
  align-items: center;
  gap: 10px;
}
.lock-ico {
  font-size: 14px;
}
.head-title {
  font-weight: var(--fw-800);
  letter-spacing: 0.08em;
  font-size: 14px;
}
.locked-tag {
  margin-left: auto;
  color: var(--danger);
  font-size: 11px;
  letter-spacing: 0.1em;
}
.card-body {
  padding: 24px 22px;
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.who {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
  padding: 6px 0 2px;
  animation: who-in 0.45s ease both;
}
.who-ring {
  display: inline-flex;
  padding: 3px;
  border-radius: 50%;
  border: 1px solid var(--border-accent);
  box-shadow: 0 0 0 3px rgba(52, 211, 153, 0.08), 0 6px 24px rgba(0, 0, 0, 0.35);
}
.who-name {
  font-size: 14px;
  font-weight: var(--fw-800);
  letter-spacing: 0.06em;
  color: var(--accent);
}
@keyframes who-in {
  from {
    opacity: 0;
    transform: translateY(-6px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}
.addr-box {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 14px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.addr-box code {
  font-size: 12px;
  word-break: break-all;
}
.short {
  font-size: 11px;
}
.pin-field {
  letter-spacing: 0.3em;
  text-align: center;
  font-size: 18px;
}
.big {
  padding: 14px 16px;
}
.err {
  color: var(--danger);
  font-size: 12px;
  letter-spacing: 0.05em;
}
.decrypting {
  font-size: 11px;
  letter-spacing: 0.08em;
  display: flex;
  align-items: center;
  gap: 8px;
}
.spinner {
  width: 12px;
  height: 12px;
  border: 2px solid var(--border);
  border-top-color: var(--accent);
  border-radius: 50%;
  display: inline-block;
  animation: spin 0.7s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}
button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
