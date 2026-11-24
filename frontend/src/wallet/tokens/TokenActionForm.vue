<script setup lang="ts">
import { computed, defineEmits, defineProps, ref } from 'vue'
import { errKey, fromUnits, toUnits, tokenOp, utf8len, type TokenConfig, type TokenInfo } from './api'
import { useTokT } from './t'
import HoldButton from './HoldButton.vue'
import TokenSummary from './TokenSummary.vue'

// Операция токена в нижнем листе: send | multi (до N получателей) | mint | burn. Подтверждение удержанием.
const props = defineProps<{ mode: 'send' | 'multi' | 'mint' | 'burn'; info: TokenInfo; cfg: TokenConfig; address: string }>()
const emit = defineEmits<{ (e: 'done', id: string): void }>()
const tt = useTokT()
const list = ref([{ to: props.mode === 'mint' ? props.address : '', amount: '' }])
const memo = ref('')
const busy = ref(false)
const err = ref('')
const sum = ref<InstanceType<typeof TokenSummary> | null>(null)
// Multi pay: одна строка — «АДРЕС, СУММА» (запятая, пробел, ; или таб), можно вставить из таблицы / CSV.
const csv = ref('')
const csvParsed = computed(() => {
  let bad = 0
  const out: { to: string; amount: string }[] = []
  csv.value.split(/\r?\n/).forEach((l, i) => {
    const t = l.trim()
    if (!t) return
    const m = t.match(/^(\S+?)[\s,;]+([\d.,]+)$/)
    if (!m && !bad) bad = i + 1
    if (m) out.push({ to: m[1], amount: m[2].replace(',', '.') })
  })
  return { out, bad }
})
const parsed = computed(() => csvParsed.value.out)
const badLine = computed(() => csvParsed.value.bad)
const src = computed(() => (props.mode === 'multi' ? parsed.value : list.value))
function loadCsv(e: Event) {
  const f = (e.target as HTMLInputElement).files?.[0]
  if (f) f.text().then((t) => (csv.value = t.trim()))
}

const ADDR = /^S[1-9A-HJ-NP-Za-km-z]{33}$/
const needAddr = props.mode !== 'burn'
const units = computed(() => src.value.map((r) => toUnits(r.amount, props.info.decimals)))
const total = computed(() => units.value.reduce<bigint>((a, u) => a + (u ?? 0n), 0n))
const limit = computed(() => (props.mode === 'mint' ? BigInt(props.info.supplyCap) - BigInt(props.info.supply) : BigInt(props.info.balance)))
const fee = computed(() => {
  const f = props.cfg.fees
  if (props.mode === 'mint') return f.mint
  if (props.mode === 'burn') return f.burn
  return (BigInt(f.transfer) + BigInt(f.transferPerRecipient || '0') * BigInt(Math.max(0, src.value.length - 1))).toString()
})
const problem = computed(() => {
  if (!props.cfg.enabled) return tt('errInactive')
  if (badLine.value) return tt('csvBad', { n: badLine.value })
  if (src.value.length > props.cfg.maxRecipients) return tt('errRecipients')
  if (needAddr && src.value.some((r) => r.to.trim() && !ADDR.test(r.to.trim()))) return tt('errAddress')
  if (src.value.some((r, i) => r.amount && (units.value[i] === null || units.value[i]! <= 0n))) return tt('errAmount')
  if (total.value > limit.value) return tt(props.mode === 'mint' ? 'errSupply' : 'errBalance')
  if (utf8len(memo.value) > 64) return tt('errMemo')
  return ''
})
const ready = computed(() => !problem.value && total.value > 0n && src.value.length > 0 && src.value.every((r, i) => (!needAddr || ADDR.test(r.to.trim())) && (units.value[i] ?? 0n) > 0n) && !sum.value?.low)
const short = (a: string) => (a.length > 14 ? `${a.slice(0, 6)}…${a.slice(-5)}` : a || '—')
const rows = computed<[string, string][]>(() => {
  const amt = `${fromUnits(total.value, props.info.decimals)} ${props.info.symbol}`
  if (props.mode === 'burn') return [[tt('burn'), amt]]
  if (props.mode === 'mint') return [[tt('mint'), amt], [tt('sumTo'), short(list.value[0].to.trim())]]
  if (props.mode === 'multi') return [[tt('recipientsLbl'), String(src.value.length)], [tt('total'), amt]]
  return [[tt('sumSend'), amt], [tt('sumTo'), short(list.value[0].to.trim())]]
})
function max() { if (list.value.length === 1) list.value[0].amount = fromUnits(limit.value, props.info.decimals).replace(/[\s,]/g, '') }

async function submit() {
  if (!ready.value) return
  busy.value = true
  err.value = ''
  const id = props.info.id
  const tr = src.value.map((r, i) => ({ recipientId: r.to.trim(), amount: units.value[i]!.toString() }))
  const asset = props.mode === 'burn' ? { id, amount: tr[0].amount } : props.mode === 'mint' ? { id, amount: tr[0].amount, recipientId: tr[0].recipientId } : { id, memo: memo.value, transfers: tr }
  try {
    emit('done', await tokenOp(props.mode === 'burn' || props.mode === 'mint' ? props.mode : 'transfer', asset, fee.value))
  } catch (e) {
    const k = errKey(e)
    err.value = k.key ? tt(k.key) : k.raw
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="taf" :data-testid="`token-form-${mode}`">
    <template v-if="mode === 'multi'">
      <p class="tk-hint">{{ tt('csvHint') }}</p>
      <label class="tk-field">
        <span class="tk-lbl">{{ tt('recipientsLbl') }} <label class="taf-max">CSV<input type="file" accept=".csv,.txt,text/csv,text/plain" hidden @change="loadCsv" data-testid="token-form-csv-file" /></label></span>
        <textarea v-model="csv" rows="6" spellcheck="false" :placeholder="'Sxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx, 10\nSyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy, 2.5'" data-testid="token-form-csv" />
        <span class="taf-meta"><small>{{ tt('recipientsLbl') }}: {{ src.length }}/{{ cfg.maxRecipients }}</small><small>{{ tt('available', { n: fromUnits(limit, info.decimals) }) }}</small></span>
      </label>
    </template>
    <div v-else v-for="(r, i) in list" :key="i" class="taf-row">
      <label v-if="needAddr" class="tk-field">
        <span class="tk-lbl">{{ tt('recipient') }}</span>
        <input v-model="r.to" spellcheck="false" :placeholder="tt('recipient')" :data-testid="`token-form-recipient-${i}`" />
      </label>
      <label class="tk-field">
        <span class="tk-lbl">{{ tt('amount') }} ({{ info.symbol }}) <a class="taf-max" @click.prevent="max" data-testid="token-form-max">MAX</a></span>
        <input v-model="r.amount" inputmode="decimal" placeholder="0" :data-testid="`token-form-amount-${i}`" />
      </label>
    </div>
    <small v-if="mode !== 'multi'" class="tk-dim">{{ tt(mode === 'mint' ? 'mintLeft' : 'available', { n: fromUnits(limit, info.decimals) }) }} · {{ tt('decimals') }}: {{ info.decimals }}</small>
    <label v-if="mode === 'send' || mode === 'multi'" class="tk-field">
      <span class="tk-lbl">{{ tt('memoLbl') }}</span>
      <input v-model="memo" :placeholder="tt('memoPh')" data-testid="token-form-memo" />
      <small class="tk-dim">{{ tt('bytes', { n: utf8len(memo), m: 64 }) }}</small>
    </label>
    <p v-if="mode === 'burn' && total" class="tk-warn">{{ tt('burnWarn', { n: `${fromUnits(total, info.decimals)} ${info.symbol}` }) }}</p>
    <small v-if="mode === 'multi'" class="tk-dim" data-testid="token-form-fee-hint">{{ tt('feeFormula') }}: <b class="mono">{{ fromUnits(cfg.fees.transfer, 8) }} + {{ fromUnits(cfg.fees.transferPerRecipient, 8) }} × {{ Math.max(0, src.length - 1) }} STH</b></small>
    <TokenSummary ref="sum" :rows="rows" :fee="fee" />
    <p v-if="problem || err" class="taf-err" data-testid="token-form-error">{{ err || problem }}</p>
    <HoldButton :label="tt(mode === 'burn' ? 'holdBurn' : mode === 'mint' ? 'holdMint' : mode === 'multi' ? 'holdPayAll' : 'holdSend')" :disabled="!ready" :busy="busy" @confirm="submit" data-testid="token-form-submit" />
  </div>
</template>

<style scoped>
.taf { display: flex; flex-direction: column; gap: 12px; }
.taf-row { display: flex; flex-direction: column; gap: 10px; position: relative; }
.taf-meta { display: flex; justify-content: space-between; font-size: 11px; color: var(--text-dim); }
textarea { padding: 11px 12px; border-radius: 10px; border: 1px solid var(--border); background: var(--bg); color: var(--text); font-family: var(--font-mono); font-size: 12px; resize: vertical; }
textarea:focus { outline: none; border-color: var(--accent); }
.taf-max { cursor: pointer; color: var(--accent); font-weight: 800; }
.taf-del { position: absolute; top: 6px; right: 8px; background: none; border: none; color: var(--text-dim); cursor: pointer; }
.taf-add { padding: 9px; border-radius: 10px; border: 1px dashed var(--border); background: none; color: var(--accent); font: inherit; font-size: 12px; font-weight: 700; cursor: pointer; transition: border-color 0.15s; }
.taf-add:hover { border-color: var(--accent); }
</style>
