<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, onMounted, onBeforeUnmount } from 'vue'
import { useI18n } from 'vue-i18n'
import { pickFile, addFile, appListen, type FileManifest } from '@/lib/bridge'
import { fmtBytes } from '@/lib/format'

const emit = defineEmits<{ (e: 'close'): void; (e: 'added', m: FileManifest): void }>()
const props = defineProps<{ initialPath?: string }>()
const { t } = useI18n()

const path = ref(props.initialPath || '')
const fileName = ref(
  props.initialPath ? props.initialPath.split(/[\\/]/).pop() || props.initialPath : '',
)
const isPaid = ref(false)
const price = ref<number>(1)
const isPrivate = ref(false)
const recipientsText = ref('')
const busy = ref(false)
const error = ref('')
const progress = ref({ done: 0, total: 0, pct: 0 })

let unlisten: (() => void) | null = null
onMounted(async () => {
  // live hashing / encryption progress emitted by the Rust backend
  unlisten = await appListen<{ done: number; total: number }>('file-add-progress', (p) => {
    progress.value = {
      done: p.done,
      total: p.total,
      pct: p.total > 0 ? Math.min(100, Math.round((p.done / p.total) * 100)) : 0,
    }
  })
})
onBeforeUnmount(() => {
  if (unlisten) unlisten()
})

function parseRecipients(): string[] {
  return Array.from(
    new Set(
      recipientsText.value
        .split(/[\s,;]+/)
        .map((s) => s.trim())
        .filter((s) => s.length > 0),
    ),
  )
}

async function choose() {
  error.value = ''
  const p = await pickFile()
  if (p) {
    path.value = p
    fileName.value = p.split(/[\\/]/).pop() || p
  }
}

async function submit() {
  if (!path.value) {
    error.value = t('addFile.errNoFile')
    return
  }
  let recipients: string[] = []
  if (isPrivate.value) {
    recipients = parseRecipients()
    if (recipients.length === 0) {
      error.value = t('addFile.errNoRecipients')
      return
    }
    const bad = recipients.find((a) => !/^S[1-9A-HJ-NP-Za-km-z]{25,40}$/.test(a))
    if (bad) {
      error.value = t('addFile.errBadAddress', { addr: bad })
      return
    }
  } else if (isPaid.value && (!price.value || price.value <= 0)) {
    error.value = t('addFile.errPriceZero')
    return
  }
  busy.value = true
  error.value = ''
  progress.value = { done: 0, total: 0, pct: 0 }
  try {
    const m = await addFile(
      path.value,
      isPrivate.value ? 0 : isPaid.value ? Number(price.value) : 0,
      isPrivate.value ? false : isPaid.value,
      recipients,
    )
    emit('added', m)
    emit('close')
  } catch (e: any) {
    error.value = String(e?.message || e)
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="fx-modal-back" @click.self="emit('close')" data-testid="add-file-modal">
    <div class="fx-modal">
      <header class="fx-modal-head">
        <h3>{{ t('addFile.title') }}</h3>
        <button class="x" @click="emit('close')" data-testid="add-file-close">✕</button>
      </header>

      <button class="dropzone" @click="choose" data-testid="add-file-pick">
        <span class="dz-ico">⬆</span>
        <span v-if="!fileName" class="dz-label">{{ t('addFile.pick') }}</span>
        <span v-else class="dz-file">{{ fileName }}</span>
      </button>

      <!-- E2EE private file -->
      <label class="paid-row">
        <input type="checkbox" v-model="isPrivate" data-testid="add-file-private-toggle" />
        <span>{{ t('addFile.privToggle') }}</span>
      </label>

      <div v-if="isPrivate" class="private-box">
        <label>{{ t('addFile.recipientsLabel') }}</label>
        <textarea
          v-model="recipientsText"
          rows="4"
          :placeholder="t('addFile.recipientsPlaceholder')"
          data-testid="add-file-recipients"
        ></textarea>
        <p class="private-hint">
          {{ t('addFile.recipientsHint') }}
        </p>
      </div>

      <!-- paid file (only for public files) -->
      <template v-if="!isPrivate">
        <label class="paid-row">
          <input type="checkbox" v-model="isPaid" data-testid="add-file-paid-toggle" />
          <span>{{ t('addFile.paidToggle') }}</span>
        </label>

        <div v-if="isPaid" class="price-row">
          <label>{{ t('addFile.priceLabel') }}</label>
          <input type="number" min="0" step="0.1" v-model="price" data-testid="add-file-price" />
        </div>
      </template>

      <p v-if="error" class="err" data-testid="add-file-error">{{ error }}</p>

      <!-- hashing / encryption progress for large files -->
      <div v-if="busy && progress.total > 0" class="hash-progress" data-testid="add-file-progress">
        <div class="hash-bar"><div class="hash-fill" :style="{ width: progress.pct + '%' }"></div></div>
        <div class="hash-meta">
          <span>{{ progress.pct }}%</span>
          <span>{{ fmtBytes(progress.done) }} / {{ fmtBytes(progress.total) }}</span>
        </div>
      </div>

      <button class="submit" :disabled="busy" @click="submit" data-testid="add-file-submit">
        <template v-if="busy">{{ isPrivate ? t('addFile.encrypting') : t('addFile.hashing') }}</template>
        <template v-else>{{ isPrivate ? t('addFile.submitPriv') : t('addFile.submitPub') }}</template>
      </button>
      <p class="hint">
        {{ t('addFile.footer') }}
      </p>
    </div>
  </div>
</template>

<style scoped>
.fx-modal-back {
  position: fixed;
  inset: 0;
  z-index: 1000;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(3, 6, 10, 0.78);
  backdrop-filter: blur(6px);
}
.fx-modal {
  width: 440px;
  max-width: 92vw;
  border: 1px solid rgba(34, 211, 238, 0.3);
  border-radius: 14px;
  background: #0a0f16;
  padding: 22px;
  box-shadow: 0 0 60px rgba(34, 211, 238, 0.12);
}
.fx-modal-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 18px;
}
.fx-modal-head h3 {
  font-size: 15px;
  letter-spacing: 2px;
  color: #22d3ee;
}
.x {
  background: none;
  border: none;
  color: #64748b;
  cursor: pointer;
  font-size: 16px;
}
.dropzone {
  width: 100%;
  border: 1.5px dashed rgba(34, 211, 238, 0.4);
  border-radius: 10px;
  background: rgba(34, 211, 238, 0.04);
  color: #cbd5e1;
  padding: 26px 16px;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  cursor: pointer;
  transition: background-color 0.2s, border-color 0.2s;
}
.dropzone:hover {
  background: rgba(34, 211, 238, 0.08);
  border-color: rgba(34, 211, 238, 0.7);
}
.dz-ico {
  font-size: 24px;
  color: #22d3ee;
}
.dz-file {
  color: #22d3ee;
  font-size: 13px;
  word-break: break-all;
}
.paid-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 16px 0 6px;
  color: #cbd5e1;
  font-size: 13px;
  cursor: pointer;
}
.private-box {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 6px 0 4px;
}
.private-box > label {
  font-size: 11px;
  letter-spacing: 1px;
  color: #64748b;
}
.private-box textarea {
  background: #060a0f;
  border: 1px solid rgba(34, 211, 238, 0.25);
  border-radius: 8px;
  padding: 9px 12px;
  color: #e2e8f0;
  font-family: ui-monospace, monospace;
  font-size: 12px;
  resize: vertical;
}
.private-hint {
  font-size: 11px;
  color: #64748b;
  line-height: 1.5;
  margin: 2px 0 0;
}
.price-row {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 8px;
}
.price-row label {
  font-size: 11px;
  letter-spacing: 1px;
  color: #64748b;
}
.price-row input {
  background: #060a0f;
  border: 1px solid rgba(34, 211, 238, 0.25);
  border-radius: 8px;
  padding: 9px 12px;
  color: #e2e8f0;
}
.err {
  color: #f87171;
  font-size: 12px;
  margin: 8px 0;
}
.hash-progress {
  margin: 12px 0 4px;
}
.hash-bar {
  height: 6px;
  border-radius: 4px;
  background: rgba(34, 211, 238, 0.12);
  overflow: hidden;
}
.hash-fill {
  height: 100%;
  background: linear-gradient(90deg, #22d3ee, #0891b2);
  transition: width 0.2s ease;
}
.hash-meta {
  display: flex;
  justify-content: space-between;
  font-size: 11px;
  color: #64748b;
  margin-top: 6px;
}
.submit {
  width: 100%;
  margin-top: 14px;
  padding: 12px;
  border: none;
  border-radius: 10px;
  background: linear-gradient(90deg, #22d3ee, #0891b2);
  color: #03121a;
  font-weight: 700;
  letter-spacing: 1px;
  cursor: pointer;
}
.submit:disabled {
  opacity: 0.6;
  cursor: wait;
}
.hint {
  margin-top: 12px;
  font-size: 11px;
  color: #475569;
  text-align: center;
  line-height: 1.5;
}
</style>
