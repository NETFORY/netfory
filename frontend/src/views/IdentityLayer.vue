<script setup lang="ts">
import { ref, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useAuthStore } from '@/store/auth'
import { newMnemonic, isValidMnemonic, primaryAddress } from '@/lib/crypto'
import LanguageSwitcher from '@/components/LanguageSwitcher.vue'

const { t } = useI18n()
const auth = useAuthStore()

type Step = 'choose' | 'mnemonic' | 'pin'
const step = ref<Step>('choose')
const mode = ref<'generate' | 'import'>('generate')
const mnemonic = ref('')
const importText = ref('')
const pin = ref('')
const pinConfirm = ref('')
const error = ref('')
const copied = ref(false)

const words = computed(() => mnemonic.value.split(' ').filter(Boolean))
const previewAddress = computed(() => {
  try {
    return mnemonic.value.trim() ? primaryAddress(mnemonic.value) : ''
  } catch {
    return ''
  }
})

function startGenerate() {
  mode.value = 'generate'
  mnemonic.value = newMnemonic()
  error.value = ''
  step.value = 'mnemonic'
}

function startImport() {
  mode.value = 'import'
  mnemonic.value = ''
  importText.value = ''
  error.value = ''
  step.value = 'mnemonic'
}

function confirmMnemonic() {
  error.value = ''
  if (mode.value === 'import') {
    const clean = importText.value.trim().replace(/\s+/g, ' ')
    if (!clean) {
      error.value = t('identity.invalidMnemonic')
      return
    }
    // Core-Wallet accepts ANY passphrase / private key — no BIP-39 restriction.
    mnemonic.value = clean
  }
  step.value = 'pin'
}

async function copyMnemonic() {
  try {
    await navigator.clipboard.writeText(mnemonic.value)
    copied.value = true
    setTimeout(() => (copied.value = false), 1500)
  } catch {
    /* noop */
  }
}

async function createVault() {
  error.value = ''
  if (pin.value.length < 4) {
    error.value = t('identity.pinTooShort')
    return
  }
  if (pin.value !== pinConfirm.value) {
    error.value = t('identity.pinMismatch')
    return
  }
  await auth.createIdentity(mnemonic.value, pin.value)
}
</script>

<template>
  <div class="auth-wrap" data-testid="identity-layer">
    <div class="auth-top">
      <LanguageSwitcher />
    </div>

    <div class="card rise" data-testid="identity-card">
      <div class="card-head">
        <div class="head-line">
          <span class="dot" />
          <span class="head-title">{{ t('identity.header') }}</span>
        </div>
        <div class="head-sub mono-label">{{ t('identity.sub') }}</div>
      </div>

      <!-- STEP: choose -->
      <div v-if="step === 'choose'" class="card-body" data-testid="step-choose">
        <h2 class="section-h">// {{ t('identity.title') }}_</h2>
        <p class="intro dim">{{ t('identity.intro') }}</p>
        <button class="btn btn-primary btn-block big" @click="startGenerate" data-testid="generate-mnemonic-btn">
          {{ t('identity.generate') }}
        </button>
        <button class="btn btn-block" @click="startImport" data-testid="import-mnemonic-btn">
          {{ t('identity.import') }}
        </button>
      </div>

      <!-- STEP: mnemonic -->
      <div v-else-if="step === 'mnemonic'" class="card-body" data-testid="step-mnemonic">
        <h2 class="section-h">// {{ t('identity.yourMnemonic') }}_</h2>

        <template v-if="mode === 'generate'">
          <div class="seed-grid" data-testid="mnemonic-grid">
            <div v-for="(w, i) in words" :key="i" class="seed-cell">
              <span class="seed-idx">{{ String(i + 1).padStart(2, '0') }}</span>
              <span class="seed-word">{{ w }}</span>
            </div>
          </div>
          <p class="warn" data-testid="mnemonic-warning">⚠ {{ t('identity.warning') }}</p>
          <button class="btn btn-block" @click="copyMnemonic" data-testid="copy-mnemonic-btn">
            {{ copied ? t('common.copied') : t('common.copy') }}
          </button>
        </template>

        <template v-else>
          <textarea
            v-model="importText"
            class="field seed-input"
            rows="3"
            :placeholder="t('identity.importPlaceholder')"
            data-testid="import-mnemonic-input"
          />
          <p class="import-note dim">{{ t('identity.importNote') }}</p>
        </template>

        <p v-if="error" class="err" data-testid="mnemonic-error">{{ error }}</p>

        <div class="row-btns">
          <button class="btn" @click="step = 'choose'" data-testid="mnemonic-back-btn">
            {{ t('common.back') }}
          </button>
          <button class="btn btn-primary flex1" @click="confirmMnemonic" data-testid="mnemonic-continue-btn">
            {{ t('identity.continue') }}
          </button>
        </div>
      </div>

      <!-- STEP: pin -->
      <div v-else class="card-body" data-testid="step-pin">
        <h2 class="section-h">// {{ t('identity.setPin') }}_</h2>
        <p class="intro dim">{{ t('identity.pinHint') }}</p>

        <label class="mono-label">{{ t('identity.pin') }}</label>
        <input
          v-model="pin"
          type="password"
          inputmode="numeric"
          class="field pin-field"
          placeholder="••••"
          data-testid="pin-input"
        />
        <label class="mono-label">{{ t('identity.pinConfirm') }}</label>
        <input
          v-model="pinConfirm"
          type="password"
          inputmode="numeric"
          class="field pin-field"
          placeholder="••••"
          @keyup.enter="createVault"
          data-testid="pin-confirm-input"
        />

        <div v-if="previewAddress" class="addr-preview" data-testid="address-preview">
          <span class="mono-label">{{ t('identity.derivedAddress') }}</span>
          <code class="hl">{{ previewAddress }}</code>
        </div>

        <p v-if="error" class="err" data-testid="pin-error">{{ error }}</p>

        <div class="row-btns">
          <button class="btn" @click="step = 'mnemonic'" data-testid="pin-back-btn">
            {{ t('common.back') }}
          </button>
          <button
            class="btn btn-primary flex1"
            :disabled="auth.busy"
            @click="createVault"
            data-testid="create-vault-btn"
          >
            {{ t('identity.createVault') }}
          </button>
        </div>
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
  max-width: 520px;
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
.dot {
  width: 8px;
  height: 8px;
  background: var(--accent);
  border-radius: 50%;
  box-shadow: 0 0 10px var(--accent-glow);
}
.head-title {
  font-weight: var(--fw-800);
  letter-spacing: 0.08em;
  font-size: 14px;
}
.head-sub {
  margin-top: 6px;
}
.card-body {
  padding: 24px 22px;
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.section-h {
  font-size: 13px;
  letter-spacing: 0.14em;
  color: var(--accent);
  font-weight: var(--fw-700);
}
.intro {
  font-size: 12.5px;
  line-height: 1.6;
}
.big {
  padding: 14px 16px;
}
.seed-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 8px;
}
.seed-cell {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 9px 10px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.seed-idx {
  font-size: 10px;
  color: var(--secondary);
}
.seed-word {
  font-size: 13px;
  font-weight: var(--fw-600);
}
.seed-input {
  resize: none;
}
.import-note {
  font-size: 11px;
  line-height: 1.5;
  margin-top: -6px;
}
.warn {
  font-size: 11px;
  color: var(--accent);
  line-height: 1.5;
}
.pin-field {
  letter-spacing: 0.3em;
}
.addr-preview {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 12px;
  background: var(--bg-elevated);
  border: 1px dashed var(--border-accent);
  border-radius: var(--radius);
}
.addr-preview code {
  font-size: 12px;
  word-break: break-all;
}
.row-btns {
  display: flex;
  gap: 10px;
  margin-top: 4px;
}
.flex1 {
  flex: 1;
}
.err {
  color: var(--danger);
  font-size: 12px;
  letter-spacing: 0.05em;
}
button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
