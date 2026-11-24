<script setup lang="ts">
import { computed, defineEmits, defineProps, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { errKey, fromUnits, tickerTaken, toUnits, tokenOp, txConfirmations, U64_MAX, type TokenConfig } from './api'
import { useTokT } from './t'
import { useWalletStore } from '@/store/wallet'
import TokenMetaForm from './TokenMetaForm.vue'
import HoldButton from './HoldButton.vue'
import TokenSummary from './TokenSummary.vue'

// Мастер создания: 1) тикер (SObject) → 2) выпуск (TokenInit) → 3) метаданные. Прогресс сохраняется по адресу.
const props = defineProps<{ cfg: TokenConfig; address: string }>()
const emit = defineEmits<{ (e: 'close'): void; (e: 'created', id: string, symbol: string): void }>()
const tt = useTokT()
interface Wz { step: 1 | 2 | 3 | 4; symbol: string; regTx: string; regOk: boolean; initTx: string; decimals: number }
const KEY = `sth_tok_wizard:${props.address}`
const saved: Wz | null = (() => { try { return JSON.parse(localStorage.getItem(KEY) || 'null') } catch { return null } })()
const wz = ref<Wz>(saved ?? { step: 1, symbol: '', regTx: '', regOk: false, initTx: '', decimals: 8 })
watch(wz, (v) => (v.step === 4 ? localStorage.removeItem(KEY) : localStorage.setItem(KEY, JSON.stringify(v))), { deep: true })

const wallet = useWalletStore()
const agree = ref(false)
const busy = ref(false)
const err = ref('')
const taken = ref<boolean | null>(null)
const ruleOk = computed(() => props.cfg.tickerRule.test(wz.value.symbol))
let chk: ReturnType<typeof setTimeout> | undefined
watch(() => wz.value.symbol, (s) => {
  taken.value = null
  clearTimeout(chk)
  if (!props.cfg.tickerRule.test(s) || wz.value.regTx) return
  chk = setTimeout(async () => { try { taken.value = await tickerTaken(s) } catch { taken.value = null } }, 450)
})
function onSym(e: Event) { wz.value.symbol = (e.target as HTMLInputElement).value.toUpperCase().replace(/[^A-Z0-9]/g, '').slice(0, 10) }

async function run(fn: () => Promise<void>) {
  busy.value = true
  err.value = ''
  try { await fn() } catch (e) { const k = errKey(e); err.value = k.key ? tt(k.key) : k.raw } finally { busy.value = false }
}
const register = () => run(async () => {
  wz.value.regTx = await tokenOp('ticker', { type: 5, subType: 0, action: 0, data: { name: wz.value.symbol } }, props.cfg.tickerFee)
  poll()
})
let timer: ReturnType<typeof setInterval> | undefined
function poll() {
  clearInterval(timer)
  if (!wz.value.regTx || wz.value.regOk) return
  timer = setInterval(async () => {
    if ((await txConfirmations(wz.value.regTx)) > 0) { wz.value.regOk = true; wz.value.step = 2; clearInterval(timer) }
  }, 4000)
}
onMounted(poll)
onBeforeUnmount(() => { clearInterval(timer); clearTimeout(chk) })

// Шаг 2 — параметры выпуска
const initial = ref('')
const max = ref('')
const mintable = ref(true)
const burnable = ref(true)
const iU = computed(() => toUnits(initial.value, wz.value.decimals))
const mU = computed(() => (mintable.value ? toUnits(max.value || initial.value, wz.value.decimals) : iU.value))
const supplyErr = computed(() => {
  if (!initial.value) return ''
  if (iU.value === null || mU.value === null || mU.value <= 0n || mU.value > U64_MAX || iU.value > mU.value) return tt('errSupply')
  return ''
})
const issue = () => run(async () => {
  if (iU.value === null || mU.value === null || supplyErr.value) return
  wz.value.initTx = await tokenOp('init', {
    id: wz.value.regTx, symbol: wz.value.symbol, decimals: wz.value.decimals,
    initialSupply: iU.value.toString(), supplyCap: mU.value.toString(), flags: (mintable.value ? 1 : 0) | (burnable.value ? 2 : 0),
  }, props.cfg.fees.init)
  wz.value.step = 3
})
const done = ref<{ id: string; symbol: string } | null>(null)
function finish() { done.value = { id: wz.value.regTx, symbol: wz.value.symbol }; wz.value.step = 4 }
function restart() {
  if (wz.value.regTx && wz.value.step < 4 && !confirm(tt('restartWarn'))) return
  clearInterval(timer)
  done.value = null
  err.value = ''
  wz.value = { step: 1, symbol: '', regTx: '', regOk: false, initTx: '', decimals: 8 }
}
const steps = computed(() => [tt('step1'), tt('step2'), tt('step3'), tt('step4')])
</script>

<template>
  <div class="tk-wizard" data-testid="token-wizard">
    <div class="tk-bar-head">
      <button @click="emit('close')" data-testid="token-wizard-close" aria-label="back">‹</button>
      <b>{{ tt('newToken') }}</b>
      <a class="tk-restart" @click="restart" data-testid="token-wizard-restart">{{ tt('restart') }}</a>
    </div>
    <ol class="tk-steps">
      <li v-for="(s, i) in steps" :key="s" :class="{ on: wz.step === i + 1, done: wz.step > i + 1 }"><i />{{ s }}</li>
    </ol>
    <p v-if="!cfg.enabled" class="tk-warn">{{ tt('inactive') }}</p>

    <section v-if="wz.step === 1" class="tk-card" data-testid="token-wizard-step1">
      <p class="tk-hint">{{ tt('s1Hint') }}</p>
      <label class="tk-field"><span class="tk-lbl">{{ tt('ticker') }}</span>
        <input :value="wz.symbol" @input="onSym" :disabled="!!wz.regTx" placeholder="COFFEE" class="tk-ticker" data-testid="token-wizard-ticker" />
      </label>
      <small class="tk-dim">{{ tt('tickerRule') }}</small>
      <p v-if="wz.symbol && !ruleOk" class="taf-err">{{ tt('errSymbol') }}</p>
      <p v-else-if="taken === true" class="taf-err" data-testid="token-wizard-taken">{{ tt('tickerTaken') }}</p>
      <p v-else-if="taken === false" class="tk-ok" data-testid="token-wizard-free">{{ tt('tickerFree') }}</p>
      <div v-if="wz.regTx" class="tk-wait" data-testid="token-wizard-waiting"><span class="tk-spin" />{{ tt('waiting') }} <code>{{ wz.regTx.slice(0, 10) }}…</code></div>
      <template v-else>
        <label class="tk-agree"><input type="checkbox" v-model="agree" data-testid="token-wizard-agree" /> {{ tt('agreeFee', { n: fromUnits(cfg.tickerFee, 8) }) }}</label>
        <TokenSummary :rows="[[tt('ticker'), wz.symbol || '—'], [tt('network'), String(wallet.network || '').toUpperCase()]]" :fee="cfg.tickerFee" />
        <p v-if="err" class="taf-err" data-testid="token-wizard-error">{{ err }}</p>
        <HoldButton :label="tt('holdRegister')" :disabled="!ruleOk || taken !== false || !agree || !cfg.enabled" :busy="busy" @confirm="register" data-testid="token-wizard-register" />
      </template>
    </section>

    <section v-else-if="wz.step === 2" class="tk-card" data-testid="token-wizard-step2">
      <p class="tk-ok">✓ {{ wz.symbol }} — {{ tt('confirmed') }}</p>
      <p class="tk-hint">{{ tt('s2Hint') }}</p>
      <label class="tk-field"><span class="tk-lbl">{{ tt('decimals') }}: {{ wz.decimals }}</span>
        <input type="range" min="0" max="8" v-model.number="wz.decimals" data-testid="token-wizard-decimals" />
      </label>
      <label class="tk-field"><span class="tk-lbl">{{ tt('initial') }}</span><input v-model="initial" inputmode="decimal" placeholder="1000000" data-testid="token-wizard-initial" /></label>
      <div class="tk-flags">
        <label><input type="checkbox" v-model="mintable" data-testid="token-wizard-mintable" /> {{ tt('flagMint') }}</label>
        <label><input type="checkbox" v-model="burnable" data-testid="token-wizard-burnable" /> {{ tt('flagBurn') }}</label>
      </div>
      <label v-if="mintable" class="tk-field"><span>{{ tt('max') }}</span><input v-model="max" inputmode="decimal" :placeholder="initial || '0'" data-testid="token-wizard-max" /></label>
      <small v-else class="tk-dim">{{ tt('maxAuto') }}</small>
      <small v-if="iU && mU && !supplyErr" class="tk-dim">{{ tt('initial') }}: {{ fromUnits(iU, wz.decimals) }} · {{ tt('max') }}: {{ fromUnits(mU, wz.decimals) }} {{ wz.symbol }}</small>
      <TokenSummary :rows="[[tt('tokenWord'), wz.symbol], [tt('initial'), iU ? fromUnits(iU, wz.decimals) : '—'], [tt('max'), mU ? fromUnits(mU, wz.decimals) : '—']]" :fee="cfg.fees.init" />
      <p v-if="supplyErr || err" class="taf-err" data-testid="token-wizard-error">{{ err || supplyErr }}</p>
      <HoldButton :label="tt('holdIssue')" :disabled="!iU || !!supplyErr || !cfg.enabled" :busy="busy" @confirm="issue" data-testid="token-wizard-issue" />
    </section>

    <section v-else-if="wz.step === 3" class="tk-card" data-testid="token-wizard-step3">
      <p class="tk-hint">{{ tt('s3Hint') }}</p>
      <TokenMetaForm :id="wz.regTx" :symbol="wz.symbol" :cfg="cfg" skippable @done="finish" @skip="finish" />
    </section>

    <section v-else-if="done" class="tk-card tk-done" data-testid="token-wizard-done">
      <span class="tk-done-ic">✓</span>
      <b>{{ tt('done', { s: done.symbol }) }}</b>
      <small class="tk-dim">{{ tt('doneHint') }}</small>
      <button class="tk-primary" @click="emit('created', done.id, done.symbol)" data-testid="token-wizard-open">{{ tt('open') }}</button>
    </section>
  </div>
</template>
