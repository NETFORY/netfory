<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { useWalletStore } from '@/store/wallet'
import HoldButton from '@/wallet/components/HoldButton.vue'
import { chatListContacts, usernameOfAddress, resolveUsername, isTauri } from '@/lib/bridge'
import { shortAddr } from '@/lib/crypto'

const { t } = useI18n()
const wallet = useWalletStore()

const FEE = 0.25
const recipient = ref('')
const amount = ref('')
const memo = ref('')

// Locked "pay & publish" mode: escrow recipient + amount + memo are fixed.
const publish = computed(() => wallet.publishCtx)

/* ---- адресная книга: недавние получатели из истории + контакты мессенджера ---- */
interface BookEntry {
  address: string
  name: string | null
  src: 'recent' | 'contact'
  last: number
}
const book = ref<BookEntry[]>([])
const bookOpen = ref(false)
const bookIdx = ref(0)
const nameCache = ref<Record<string, string | null>>({})
async function loadBook() {
  const seen = new Map<string, BookEntry>()
  for (const tx of wallet.transactions) {
    if (tx.type !== 'out' || !tx.counterparty) continue
    const e = seen.get(tx.counterparty)
    if (!e) seen.set(tx.counterparty, { address: tx.counterparty, name: null, src: 'recent', last: tx.timestamp })
    else if (tx.timestamp > e.last) e.last = tx.timestamp
  }
  if (isTauri) {
    try {
      for (const c of await chatListContacts()) {
        const e = seen.get(c.address)
        if (e) e.name = c.username || e.name
        else seen.set(c.address, { address: c.address, name: c.username || null, src: 'contact', last: c.addedAt * 1000 })
      }
    } catch {
      /* мессенджер недоступен */
    }
    // имена недавних получателей — из реестра имён (кэш на сессию)
    for (const e of seen.values()) {
      if (e.name || e.address in nameCache.value) { e.name = e.name ?? nameCache.value[e.address]; continue }
      nameCache.value[e.address] = null
      usernameOfAddress(e.address).then((n) => {
        nameCache.value[e.address] = n
        const hit = book.value.find((b) => b.address === e.address)
        if (hit && n) hit.name = n
      }).catch(() => {})
    }
  }
  book.value = [...seen.values()].sort((a, b) => b.last - a.last)
}
const bookFiltered = computed(() => {
  const q = recipient.value.trim().toLowerCase().replace(/^@/, '')
  const list = book.value.filter((e) => !q || e.address.toLowerCase().startsWith(q) || (e.name ?? '').toLowerCase().startsWith(q))
  return list.slice(0, 6)
})
function pickEntry(e: BookEntry) {
  recipient.value = e.address
  bookOpen.value = false
}
function onBookKey(ev: KeyboardEvent) {
  if (!bookOpen.value || !bookFiltered.value.length) return
  const n = bookFiltered.value.length
  if (ev.key === 'ArrowDown') { ev.preventDefault(); bookIdx.value = (bookIdx.value + 1) % n }
  else if (ev.key === 'ArrowUp') { ev.preventDefault(); bookIdx.value = (bookIdx.value - 1 + n) % n }
  else if (ev.key === 'Enter' || ev.key === 'Tab') { ev.preventDefault(); pickEntry(bookFiltered.value[bookIdx.value]) }
  else if (ev.key === 'Escape') bookOpen.value = false
}
/** `@имя` → адрес через реестр имён (при потере фокуса / перед отправкой). */
async function resolveIfName() {
  const v = recipient.value.trim()
  if (!v.startsWith('@') || v.length < 2) return
  const hit = book.value.find((e) => e.name && '@' + e.name.toLowerCase() === v.toLowerCase())
  if (hit) { recipient.value = hit.address; return }
  try {
    const addr = await resolveUsername(v.slice(1))
    if (addr) recipient.value = addr
  } catch {
    /* оставляем как есть — валидация не пропустит */
  }
}
onMounted(() => {
  if (publish.value) {
    recipient.value = publish.value.escrow
    amount.value = publish.value.amount
    memo.value = publish.value.memo
  }
})

const amountNum = computed(() => Number(amount.value) || 0)
const SMARTOSHI = 100_000_000
/** Точные STH: < 10 000 — 8 знаков, иначе 4 (без округления вверх). */
function fmtSth(raw: number): string {
  const r = Math.max(0, Math.round(raw))
  const whole = Math.floor(r / SMARTOSHI)
  const frac = r % SMARTOSHI
  return whole < 10_000
    ? `${whole}.${String(frac).padStart(8, '0')}`
    : `${whole}.${String(Math.floor(frac / 10_000)).padStart(4, '0')}`
}
const total = computed(() => fmtSth(Math.round(amountNum.value * SMARTOSHI) + FEE * SMARTOSHI))
const valid = computed(
  () => {
    const r = recipient.value.trim()
    return (r.length >= 26 || (r.startsWith('@') && r.length >= 3)) && amountNum.value > 0 && memo.value.length <= 64
  },
)

function setMax() {
  // Считаем в смартоши: баланс приходит точным (8 знаков), комиссия вычитается без потерь.
  const balRaw = Math.round((Number(wallet.balance) || 0) * SMARTOSHI)
  amount.value = fmtSth(balRaw - FEE * SMARTOSHI)
}

async function submit() {
  if (!valid.value) return
  if (publish.value) {
    await wallet.confirmPublishPayment()
    return
  }
  await resolveIfName()
  await wallet.sendFunds(recipient.value.trim(), amount.value, memo.value)
}
</script>

<template>
  <div class="send" data-testid="wallet-send">
    <div v-if="publish" class="publish-banner" data-testid="send-publish-banner">
      {{ publish.kind === 'boost'
        ? '⚡ ' + t('wallet.send.boostFor', { amount: publish.amount })
        : '⬡ ' + t('wallet.send.publishFor', { amount: publish.amount }) }}
    </div>
    <label class="fld">
      <span class="lbl">{{ t('wallet.send.recipient') }}</span>
      <div class="rcpt">
        <input
          v-model="recipient"
          class="inp mono"
          placeholder="S… / @name"
          spellcheck="false"
          autocomplete="off"
          :readonly="!!publish"
          @focus="!publish && (bookOpen = true, bookIdx = 0, loadBook())"
          @input="bookOpen = true; bookIdx = 0"
          @keydown="onBookKey"
          @blur="setTimeout(() => { bookOpen = false; resolveIfName() }, 120)"
          data-testid="send-recipient"
        />
        <transition name="fade">
          <div v-if="bookOpen && !publish && (bookFiltered.length || !book.length)" class="book" data-testid="send-address-book">
            <div v-if="!book.length" class="book-empty dim">{{ t('wallet.addressBook.empty') }}</div>
            <button
              v-for="(e, i) in bookFiltered"
              :key="e.address"
              type="button"
              class="book-row"
              :class="{ on: i === bookIdx }"
              @mousedown.prevent="pickEntry(e)"
              @mouseenter="bookIdx = i"
              :data-testid="`send-book-${e.address}`"
            >
              <span class="book-av">{{ (e.name ?? e.address.slice(1, 3)).slice(0, 2).toUpperCase() }}</span>
              <span class="book-main">
                <span class="book-name">{{ e.name ? '@' + e.name : shortAddr(e.address, 8, 6) }}</span>
                <span v-if="e.name" class="book-addr mono dim">{{ shortAddr(e.address, 8, 6) }}</span>
              </span>
              <span class="book-src dim">{{ t(e.src === 'contact' ? 'wallet.addressBook.contacts' : 'wallet.addressBook.recent') }}</span>
            </button>
          </div>
        </transition>
      </div>
    </label>

    <label class="fld">
      <span class="lbl-row">
        <span class="lbl">{{ t('wallet.send.amount') }}</span>
        <button v-if="!publish" class="max" @click="setMax" type="button" data-testid="send-max">{{ t('wallet.send.max') }}</button>
      </span>
      <input
        v-model="amount"
        class="inp mono"
        type="number"
        min="0"
        step="0.00000001"
        placeholder="0.0000"
        :readonly="!!publish"
        data-testid="send-amount"
      />
    </label>

    <label class="fld">
      <span class="lbl-row">
        <span class="lbl">{{ t('wallet.send.memo') }}</span>
        <span class="count dim">{{ memo.length }}/64</span>
      </span>
      <input
        v-model="memo"
        class="inp"
        maxlength="64"
        :placeholder="t('wallet.send.memoPlaceholder')"
        :readonly="!!publish"
        data-testid="send-memo"
      />
    </label>

    <div class="summary">
      <div class="row">
        <span class="dim">{{ t('wallet.send.fee') }}</span>
        <span class="mono">{{ FEE.toFixed(2) }} STH</span>
      </div>
      <div class="row total">
        <span>{{ t('wallet.send.total') }}</span>
        <span class="mono hl" data-testid="send-total">{{ total }} STH</span>
      </div>
    </div>

    <p v-if="wallet.error" class="err" data-testid="send-error">⚠ {{ wallet.error }}</p>

    <HoldButton
      :label="wallet.busy ? t('wallet.send.sending') : publish ? (publish.kind === 'boost' ? t('wallet.send.holdBoost') : t('wallet.send.holdPublish')) : t('wallet.send.hold')"
      :holding-label="t('wallet.send.holding')"
      :disabled="!valid || wallet.busy"
      :duration="1200"
      testid="send-hold-btn"
      @complete="submit"
    />
    <button
      v-if="publish"
      class="cancel-pub"
      type="button"
      @click="wallet.cancelPublishPayment()"
      data-testid="send-publish-cancel"
    >
      {{ t('common.cancel') }}
    </button>
  </div>
</template>

<style scoped>
.send {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.fld {
  display: flex;
  flex-direction: column;
  gap: 7px;
}
.lbl,
.lbl-row {
  font-size: 10px;
  letter-spacing: 0.12em;
  color: var(--text-dim);
  text-transform: uppercase;
  font-weight: var(--fw-700);
}
.lbl-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.max {
  color: var(--accent);
  font-size: 10px;
  font-weight: var(--fw-800);
  letter-spacing: 0.1em;
}
.count {
  font-weight: var(--fw-600);
}
.inp {
  width: 100%;
  padding: 12px;
  font-size: 13px;
  color: var(--text);
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
}
.inp:focus {
  outline: none;
  border-color: var(--accent);
}
.rcpt {
  position: relative;
}
.book {
  position: absolute;
  left: 0;
  right: 0;
  top: calc(100% + 4px);
  z-index: 6;
  display: flex;
  flex-direction: column;
  padding: 4px;
  background: var(--panel-solid, var(--panel));
  border: 1px solid var(--border-accent, var(--border));
  border-radius: var(--radius);
  box-shadow: 0 14px 34px -14px rgba(0, 0, 0, 0.6);
}
.book-empty {
  font-size: 11px;
  padding: 8px 10px;
}
.book-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 7px 8px;
  border-radius: 6px;
  text-align: left;
  cursor: pointer;
  color: var(--text);
  transition: background-color 0.12s ease;
}
.book-row.on,
.book-row:hover {
  background: color-mix(in srgb, var(--accent) 12%, transparent);
}
.book-av {
  width: 26px;
  height: 26px;
  flex: none;
  display: grid;
  place-items: center;
  border-radius: 50%;
  font-size: 10px;
  font-weight: var(--fw-800);
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  border: 1px solid var(--border-accent, var(--border));
}
.book-main {
  display: flex;
  flex-direction: column;
  gap: 1px;
  min-width: 0;
}
.book-name {
  font-size: 12px;
  font-weight: var(--fw-700);
}
.book-addr {
  font-size: 10px;
}
.book-src {
  margin-left: auto;
  font-size: 9px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
}
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.12s ease;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
.mono {
  font-family: var(--font-mono, monospace);
}
.summary {
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 12px;
}
.row.total {
  border-top: 1px solid var(--border);
  padding-top: 8px;
  font-weight: var(--fw-700);
}
.err {
  color: var(--danger);
  font-size: 12px;
}
.publish-banner {
  font-size: 12px;
  font-weight: var(--fw-700);
  letter-spacing: 0.04em;
  color: var(--accent);
  background: var(--accent-soft, rgba(34, 211, 238, 0.08));
  border: 1px solid var(--border-accent, rgba(34, 211, 238, 0.3));
  border-radius: var(--radius);
  padding: 10px 12px;
}
.inp[readonly] {
  opacity: 0.75;
  cursor: not-allowed;
}
.cancel-pub {
  background: none;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  color: var(--text-dim);
  padding: 8px;
  font-size: 11px;
  letter-spacing: 0.1em;
  cursor: pointer;
}
.cancel-pub:hover {
  color: var(--text);
}
</style>
