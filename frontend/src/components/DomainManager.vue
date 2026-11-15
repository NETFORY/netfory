<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, computed, watch, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { useAppsStore } from '@/store/apps'
import { useTabsStore } from '@/store/tabs'
import { useWalletStore } from '@/store/wallet'
import {
  domainLookup,
  registerDomain,
  renewDomain,
  renewalPrice,
  listSell,
  domainOrder,
  listMyDomains,
  domainDurationDays,
  type DomainStatus,
  type DomainRecord,
  type SaleOrder,
  type Manifest,
} from '@/lib/bridge'

const props = defineProps<{ appId: string }>()
const { t } = useI18n()
const apps = useAppsStore()
const tabs = useTabsStore()
const wallet = useWalletStore()
const router = useRouter()

// An already-listed domain's badge jumps to the marketplace instead of the
// inline sell form (the listing is live — manage/buy it there).
function goMarket() {
  tabs.goHome()
  router.push({ name: 'domarket' })
}

// From a draft's locked .sth panel, jump straight to "My Applications" to publish.
function goPublish() {
  tabs.goHome()
  router.push({ name: 'apps' })
}

const query = ref('')
const status = ref<DomainStatus | null>(null)
const checking = ref(false)
const confirming = ref(false)
const busy = ref(false)
const flash = ref('')
const flashOk = ref(true)
const mine = ref<DomainRecord[]>([])
const suggestions = ref<{ name: string; priceSth: number }[]>([])
const basePrice = ref(500)
let debounce: number | undefined

const manifest = computed<Manifest | undefined>(() => apps.apps.find((m) => m.id === props.appId))

// A DRAFT app cannot bind/buy a name until it is published & paid. If the draft
// was auto-minted from a domain purchase, its name is `<domain>.sth`.
const isDraft = computed(() => manifest.value?.status === 'draft')
const boughtDomain = computed(() => {
  const n = (manifest.value?.name || '').toLowerCase()
  return n.endsWith('.sth') ? n.replace(/\.sth$/, '') : ''
})

const fullName = computed(() => query.value.trim().toLowerCase().replace(/\.sth$/, ''))
const durationDays = domainDurationDays()

const premium3 = computed(() => basePrice.value * 100)
const premium4 = computed(() => basePrice.value * 20)

const balanceSth = computed(() => Number(wallet.mainBalance) || 0)
const renewPrice = ref(250)
const renewName = ref('')
const renewBusy = ref(false)

const canPayRegister = computed(
  () => !!status.value && status.value.available && balanceSth.value >= status.value.priceSth,
)

function daysLeft(rec: DomainRecord): number {
  return Math.ceil((rec.expires * 1000 - Date.now()) / 86400000)
}
function isRenewable(rec: DomainRecord): boolean {
  return daysLeft(rec) <= 30
}

async function doRenew(rec: DomainRecord) {
  if (renewBusy.value || balanceSth.value < renewPrice.value) return
  renewBusy.value = true
  try {
    await renewDomain(`${rec.name}.${rec.zone}`, props.appId)
    notify(t('nodePanel.domRenewOk', { name: `${rec.name}.${rec.zone}` }), true)
    renewName.value = ''
    await loadMine()
  } catch (e: any) {
    notify(typeof e === 'string' ? e : e?.message || 'error', false)
  } finally {
    renewBusy.value = false
  }
}

const canRegister = computed(
  () => !!status.value && status.value.valid && status.value.available,
)

function notify(msg: string, ok = true) {
  flash.value = msg
  flashOk.value = ok
  setTimeout(() => (flash.value = ''), 2600)
}

async function loadMine() {
  const all = await listMyDomains()
  mine.value = all.filter((d) => d.appId === props.appId)
  const map: Record<string, SaleOrder | null> = {}
  for (const d of mine.value) {
    map[d.name] = await domainOrder(`${d.name}.${d.zone}`)
  }
  orders.value = map
}

// --- marketplace: list a domain for sale (Phase 3a) ---
const orders = ref<Record<string, SaleOrder | null>>({})
const sellName = ref('')
const sellPrice = ref('')
const sellDuration = ref(0)
const sellBusy = ref(false)
const SELL_DURATIONS = [
  { key: '1w', secs: 7 * 86400, label: '1 нед' },
  { key: '1m', secs: 30 * 86400, label: '1 мес' },
  { key: '3m', secs: 90 * 86400, label: '3 мес' },
  { key: '6m', secs: 180 * 86400, label: '6 мес' },
]
function durationAllowed(rec: DomainRecord, secs: number): boolean {
  return Math.floor(Date.now() / 1000) + secs <= rec.expires
}
function openSell(rec: DomainRecord) {
  sellName.value = sellName.value === rec.name ? '' : rec.name
  sellPrice.value = ''
  // default to the first duration that fits inside the registration window
  const first = SELL_DURATIONS.find((d) => durationAllowed(rec, d.secs))
  sellDuration.value = first ? first.secs : 0
}
async function doSell(rec: DomainRecord) {
  const price = Math.floor(Number(sellPrice.value))
  if (!price || price <= 0 || !sellDuration.value || sellBusy.value) return
  sellBusy.value = true
  try {
    const expires = Math.floor(Date.now() / 1000) + sellDuration.value
    await listSell(`${rec.name}.${rec.zone}`, price, expires)
    notify(t('nodePanel.domSellOk', { name: `${rec.name}.${rec.zone}` }), true)
    sellName.value = ''
    await loadMine()
  } catch (e: any) {
    notify(typeof e === 'string' ? e : e?.message || 'error', false)
  } finally {
    sellBusy.value = false
  }
}

// --- name-candidate generator (no AI: transliteration + slug heuristics) ---
const TRANSLIT: Record<string, string> = {
  а: 'a', б: 'b', в: 'v', г: 'g', д: 'd', е: 'e', ё: 'e', ж: 'zh', з: 'z', и: 'i',
  й: 'i', к: 'k', л: 'l', м: 'm', н: 'n', о: 'o', п: 'p', р: 'r', с: 's', т: 't',
  у: 'u', ф: 'f', х: 'h', ц: 'c', ч: 'ch', ш: 'sh', щ: 'sch', ъ: '', ы: 'y', ь: '',
  э: 'e', ю: 'yu', я: 'ya',
}
function translit(s: string): string {
  return s.toLowerCase().split('').map((c) => TRANSLIT[c] ?? c).join('')
}
function slugify(s: string): string {
  return translit(s)
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 63)
}
function labelOk(s: string): boolean {
  return /^[a-z0-9-]{3,63}$/.test(s) && !s.startsWith('-') && !s.endsWith('-')
}
function buildCandidates(): string[] {
  const m = manifest.value
  const title = (m?.name || '').trim()
  const tags = (m?.tags || []).map((x) => slugify(x)).filter(Boolean)
  const slug = slugify(title)
  const words = slug.split('-').filter(Boolean)
  const first = words[0] || ''
  const initials = words.map((w) => w[0]).join('') // acronym
  const noVowels = first.replace(/[aeiouy]/g, '')
  const out = [
    first,
    slug,
    words.slice(0, 2).join(''),
    tags[0] ? `${first}-${tags[0]}` : '',
    initials,
    noVowels,
    tags[0] || '',
  ]
  // unique, valid (≥3 chars so non-admin can register), cap to a handful
  return Array.from(new Set(out)).filter(labelOk).slice(0, 8)
}
async function buildSuggestions() {
  const cands = buildCandidates()
  const found: { name: string; priceSth: number }[] = []
  for (const c of cands) {
    const st = await domainLookup(c)
    if (st.valid && st.name.length >= 5 && st.priceSth > 0) basePrice.value = st.priceSth
    if (st.valid && st.available && !st.adminOnly) {
      found.push({ name: st.name, priceSth: st.priceSth })
    }
    if (found.length >= 5) break
  }
  suggestions.value = found
}
function pick(name: string) {
  query.value = name
}

onMounted(async () => {
  await loadMine()
  await buildSuggestions()
  renewPrice.value = await renewalPrice()
})

watch(query, () => {
  confirming.value = false
  status.value = null
  if (debounce) clearTimeout(debounce)
  const raw = query.value.trim()
  if (!raw) return
  checking.value = true
  debounce = window.setTimeout(async () => {
    try {
      status.value = await domainLookup(fullName.value)
    } finally {
      checking.value = false
    }
  }, 350)
})

const statusLabel = computed(() => {
  const s = status.value
  if (!s) return ''
  if (!s.valid) return t('nodePanel.domInvalid')
  if (s.reason === 'reserved') return t('nodePanel.domReserved')
  if (s.reason === 'taken') return t('nodePanel.domTaken')
  if (s.available) return t('nodePanel.domFree')
  return ''
})
const statusKind = computed(() => {
  const s = status.value
  if (!s || !s.valid) return 'bad'
  if (s.available) return 'ok'
  return 'bad'
})

async function doRegister() {
  if (!canRegister.value || busy.value) return
  busy.value = true
  try {
    await registerDomain(fullName.value, props.appId)
    notify(t('nodePanel.domSuccess', { name: fullName.value }), true)
    query.value = ''
    status.value = null
    confirming.value = false
    await loadMine()
    await buildSuggestions()
  } catch (e: any) {
    notify(typeof e === 'string' ? e : e?.message || 'error', false)
  } finally {
    busy.value = false
  }
}

function fmtExpiry(unix: number): string {
  const d = new Date(unix * 1000)
  const days = Math.ceil((unix * 1000 - Date.now()) / 86400000)
  return `${d.toLocaleDateString()} · ${days} ${t('nodePanel.domDays')}`
}
</script>

<template>
  <div class="dm" data-testid="domain-manager">
    <div class="dm-head">
      <span class="dm-glyph">.sth</span>
      <p class="dm-title">{{ t('nodePanel.domReg') }}</p>
      <p class="dm-sub">{{ t('nodePanel.domSubtitle') }}</p>
    </div>

    <div class="dm-search" v-if="!isDraft">
      <div class="dm-input-wrap">
        <span class="dm-prefix">sth://</span>
        <input
          v-model="query"
          class="dm-input"
          :placeholder="t('nodePanel.domPlaceholder')"
          spellcheck="false"
          autocomplete="off"
          data-testid="domain-input"
        />
      </div>

      <!-- name-candidate suggestions (derived from the app name + tags) -->
      <div v-if="suggestions.length" class="dm-suggest" data-testid="domain-suggestions">
        <span class="dm-suggest-h">{{ t('nodePanel.domSuggest') }}</span>
        <div class="dm-chips">
          <button
            v-for="s in suggestions"
            :key="s.name"
            class="dm-chip"
            @click="pick(s.name)"
            :data-testid="`domain-suggestion-${s.name}`"
          >
            {{ s.name }} <i>{{ s.priceSth }} STH</i>
          </button>
        </div>
      </div>
      <p class="dm-premium" data-testid="domain-premium">
        ★ {{ t('nodePanel.domPremium', { p3: premium3, p4: premium4 }) }}
      </p>

      <div v-if="checking" class="dm-status dim" data-testid="domain-status">…</div>
      <div
        v-else-if="status"
        class="dm-status"
        :class="statusKind"
        data-testid="domain-status"
      >
        <span class="dm-dot" />
        <span class="dm-status-text">{{ statusLabel }}</span>
        <span v-if="status.valid" class="dm-price" data-testid="domain-price">
          {{ status.priceSth }} STH / {{ durationDays }} {{ t('nodePanel.domDays') }}
        </span>
      </div>

      <p v-if="status?.adminOnly && status?.valid" class="dm-note" data-testid="domain-admin-note">
        ⚠ {{ t('nodePanel.domAdminOnly') }}
      </p>

      <template v-if="canRegister">
        <button
          v-if="!confirming"
          class="dm-btn primary"
          :disabled="busy"
          @click="confirming = true"
          data-testid="domain-register-btn"
        >
          {{ t('nodePanel.domRegisterBtn', { price: status!.priceSth }) }}
        </button>
        <div v-else class="dm-confirm" data-testid="domain-confirm">
          <p class="dm-confirm-q">
            {{ t('nodePanel.domConfirmQ', { name: fullName, price: status!.priceSth }) }}
          </p>
          <div class="dm-balance" :class="{ low: !canPayRegister }" data-testid="domain-balance">
            {{ t('nodePanel.domBalance') }}: {{ balanceSth }} STH
            <span v-if="!canPayRegister" class="dm-balance-warn">· {{ t('nodePanel.domInsufficient') }}</span>
          </div>
          <div class="dm-confirm-row">
            <button class="dm-btn" :disabled="busy" @click="confirming = false" data-testid="domain-cancel-btn">
              {{ t('common.cancel') }}
            </button>
            <button
              class="dm-btn primary"
              :disabled="busy || !canPayRegister"
              @click="doRegister"
              data-testid="domain-confirm-btn"
            >
              {{ busy ? '…' : t('nodePanel.domConfirmBtn') }}
            </button>
          </div>
        </div>
      </template>
    </div>

    <div v-else class="dm-draft-notice" data-testid="domain-draft-notice">
      <span class="dm-draft-icon">🔒</span>
      <p class="dm-draft-text">{{ t('nodePanel.domDraftLocked') }}</p>
      <div v-if="boughtDomain" class="dm-draft-bought" data-testid="domain-draft-bought">
        <span class="dm-draft-bought-label">{{ t('nodePanel.domDraftBought') }}</span>
        <span class="dm-draft-bought-name">sth://{{ boughtDomain }}</span>
      </div>
      <button class="dm-btn primary dm-draft-publish" @click="goPublish" data-testid="domain-draft-publish">
        {{ t('nodePanel.domDraftPublish') }}
      </button>
    </div>

    <div v-if="mine.length" class="dm-mine" data-testid="domain-mine">
      <span class="dm-mine-h">{{ t('nodePanel.domMine') }}</span>
      <div v-for="d in mine" :key="d.name" class="dm-mine-block">
        <div class="dm-mine-row">
          <span class="dm-mine-name" :class="{ exp: daysLeft(d) <= 7 }">{{ d.name }}.{{ d.zone }}</span>
          <span
            v-if="orders[d.name]"
            class="dm-tag"
            :title="`${orders[d.name]!.priceSth} STH`"
            data-testid="domain-onsale-tag"
          >
            <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20.59 13.41 13.42 20.6a2 2 0 0 1-2.83 0L2 12V2h10l8.59 8.59a2 2 0 0 1 0 2.82z"/><circle cx="7" cy="7" r="1.2" fill="currentColor"/></svg>
            {{ orders[d.name]!.priceSth }}
          </span>
          <span class="dm-mine-exp" :class="{ warn: daysLeft(d) <= 30 && daysLeft(d) > 7, danger: daysLeft(d) <= 7 }">
            {{ fmtExpiry(d.expires) }}
          </span>
          <button
            v-if="isRenewable(d)"
            class="dm-renew-btn"
            @click="renewName = renewName === d.name ? '' : d.name"
            :data-testid="`domain-renew-${d.name}`"
          >
            {{ t('nodePanel.domRenew') }}
          </button>
          <button
            class="dm-sell-btn"
            :class="{ on: !!orders[d.name] }"
            @click="orders[d.name] ? goMarket() : openSell(d)"
            :data-testid="`domain-sell-${d.name}`"
          >
            {{ orders[d.name] ? t('nodePanel.domOnSale') : t('nodePanel.domSell') }}
          </button>
        </div>

        <div v-if="renewName === d.name" class="dm-renew-form" data-testid="domain-renew-form">
          <p class="dm-renew-q">
            {{ t('nodePanel.domRenewQ', { name: `${d.name}.${d.zone}`, price: renewPrice }) }}
          </p>
          <div class="dm-balance" :class="{ low: balanceSth < renewPrice }">
            {{ t('nodePanel.domBalance') }}: {{ balanceSth }} STH
            <span v-if="balanceSth < renewPrice" class="dm-balance-warn">· {{ t('nodePanel.domInsufficient') }}</span>
          </div>
          <button
            class="dm-btn primary"
            :disabled="renewBusy || balanceSth < renewPrice"
            @click="doRenew(d)"
            :data-testid="`domain-renew-confirm-${d.name}`"
          >
            {{ renewBusy ? '…' : t('nodePanel.domRenewBtn', { price: renewPrice }) }}
          </button>
        </div>

        <div v-if="sellName === d.name" class="dm-sell-form" data-testid="domain-sell-form">
          <p class="dm-renew-q">{{ t('nodePanel.domSellTitle') }}</p>
          <input
            v-model="sellPrice"
            type="number"
            min="1"
            class="dm-input dm-sell-price"
            :placeholder="t('nodePanel.domSellPrice')"
            :data-testid="`domain-sell-price-${d.name}`"
          />
          <p v-if="Number(sellPrice) > 0" class="dm-net" data-testid="domain-sell-net">
            {{ t('nodePanel.domNet', { net: (Number(sellPrice) * 0.995).toFixed(2) }) }}
          </p>
          <div class="dm-sell-durs">
            <button
              v-for="opt in SELL_DURATIONS"
              :key="opt.key"
              class="dm-dur"
              :class="{ on: sellDuration === opt.secs }"
              :disabled="!durationAllowed(d, opt.secs)"
              @click="sellDuration = opt.secs"
              :data-testid="`domain-sell-dur-${opt.key}-${d.name}`"
            >
              {{ opt.label }}
            </button>
          </div>
          <button
            class="dm-btn primary"
            :disabled="sellBusy || !Number(sellPrice) || !sellDuration"
            @click="doSell(d)"
            :data-testid="`domain-sell-confirm-${d.name}`"
          >
            {{ sellBusy ? '…' : t('nodePanel.domSellConfirm') }}
          </button>
        </div>
      </div>
    </div>

    <transition name="fade">
      <div v-if="flash" class="dm-flash" :class="{ err: !flashOk }" data-testid="domain-flash">
        {{ flashOk ? '✓' : '⚠' }} {{ flash }}
      </div>
    </transition>
  </div>
</template>

<style scoped>
.dm {
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 6px 2px 16px;
}
.dm-head {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  text-align: center;
}
.dm-glyph {
  font-size: 30px;
  font-weight: 900;
  letter-spacing: 0.04em;
  color: var(--accent);
}
.dm-title {
  font-size: 13px;
  font-weight: 700;
  color: var(--text);
}
.dm-sub {
  font-size: 11px;
  color: var(--text-dim);
  max-width: 240px;
}
.dm-search {
  display: flex;
  flex-direction: column;
  gap: 9px;
}
.dm-input-wrap {
  display: flex;
  align-items: center;
  border: 1px solid var(--border);
  background: rgba(0, 0, 0, 0.3);
  border-radius: 8px;
  overflow: hidden;
}
.dm-input-wrap:focus-within {
  border-color: var(--accent);
}
.dm-input {
  flex: 1;
  background: transparent;
  border: none;
  outline: none;
  color: var(--text);
  font-size: 14px;
  padding: 10px 12px;
  font-family: inherit;
}
.dm-prefix {
  padding: 0 6px 0 12px;
  font-size: 13px;
  font-weight: 700;
  color: var(--accent);
  border-right: 1px solid var(--border);
  white-space: nowrap;
}
.dm-suggest {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.dm-suggest-h {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.dm-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.dm-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 5px 9px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: rgba(0, 0, 0, 0.25);
  color: var(--text);
  font-size: 12px;
  cursor: pointer;
  transition: border-color 0.15s, color 0.15s;
}
.dm-chip:hover {
  border-color: var(--accent);
  color: var(--accent);
}
.dm-chip i {
  font-style: normal;
  font-size: 10px;
  color: var(--text-dim);
}
.dm-premium {
  font-size: 10px;
  color: #e7c14b;
}
.dm-status {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
}
.dm-status.ok {
  color: var(--accent);
}
.dm-status.bad {
  color: #ff8a8a;
}
.dm-dot {
  width: 8px;
  height: 8px;
  border-radius: 999px;
  background: currentColor;
}
.dm-price {
  margin-left: auto;
  color: var(--text-dim);
  font-size: 11px;
}
.dm-note {
  font-size: 11px;
  color: #e7c14b;
}
.dm-btn {
  padding: 10px 14px;
  border-radius: 8px;
  border: 1px solid var(--border);
  background: var(--bg-elevated, rgba(255, 255, 255, 0.04));
  color: var(--text);
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.04em;
  cursor: pointer;
  transition: border-color 0.15s, background 0.15s, opacity 0.15s;
}
.dm-btn:hover {
  border-color: var(--accent);
}
.dm-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.dm-btn.primary {
  background: var(--accent);
  color: #04130d;
  border-color: var(--accent);
}
.dm-confirm {
  display: flex;
  flex-direction: column;
  gap: 8px;
  border: 1px solid var(--accent);
  border-radius: 8px;
  padding: 11px;
  background: rgba(127, 217, 207, 0.07);
}
.dm-confirm-q {
  font-size: 12px;
  color: var(--text);
}
.dm-confirm-row {
  display: flex;
  gap: 8px;
}
.dm-confirm-row .dm-btn {
  flex: 1;
}
.dm-draft-notice {
  margin-top: 8px;
  padding: 20px 18px;
  border: 1px solid rgba(231, 193, 75, 0.35);
  border-radius: 12px;
  background: rgba(231, 193, 75, 0.06);
  text-align: center;
}
.dm-draft-icon {
  font-size: 22px;
}
.dm-draft-text {
  margin-top: 8px;
  font-size: 13px;
  line-height: 1.5;
  color: #e7c14b;
  font-weight: 700;
}
.dm-draft-bought {
  margin-top: 14px;
  padding-top: 12px;
  border-top: 1px solid var(--border);
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.dm-draft-bought-label {
  font-size: 10px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.dm-draft-bought-name {
  font-size: 17px;
  font-weight: 800;
  font-family: var(--font-mono, monospace);
  color: var(--accent);
  word-break: break-all;
}
.dm-draft-publish {
  margin-top: 16px;
}

.dm-mine {
  display: flex;
  flex-direction: column;
  gap: 6px;
  border-top: 1px solid var(--border);
  padding-top: 12px;
}
.dm-mine-h {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--text-dim);
}
.dm-mine-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 12px;
}
.dm-mine-name {
  color: var(--accent);
  font-weight: 700;
}
.dm-mine-exp {
  color: var(--text-dim);
  font-size: 10px;
}
.dm-mine-exp.warn {
  color: #e7c14b;
}
.dm-mine-exp.danger {
  color: #ff6a6a;
}
.dm-mine-name.exp {
  color: #ff8a8a;
}
.dm-mine-block {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.dm-renew-btn {
  margin-left: auto;
  padding: 3px 10px;
  border-radius: 999px;
  border: 1px solid var(--accent);
  background: transparent;
  color: var(--accent);
  font-size: 11px;
  font-weight: 700;
  cursor: pointer;
  transition: background 0.15s, color 0.15s;
}
.dm-renew-btn:hover {
  background: var(--accent);
  color: #04130d;
}
.dm-renew-form {
  display: flex;
  flex-direction: column;
  gap: 8px;
  border: 1px solid var(--accent);
  border-radius: 8px;
  padding: 10px;
  background: rgba(127, 217, 207, 0.06);
}
.dm-renew-q {
  font-size: 12px;
  color: var(--text);
}
.dm-tag {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  font-size: 10px;
  font-weight: 700;
  color: #e7c14b;
  border: 1px solid rgba(231, 193, 75, 0.5);
  border-radius: 999px;
  padding: 1px 7px 1px 5px;
}
.dm-sell-btn {
  padding: 3px 10px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: transparent;
  color: var(--text-dim);
  font-size: 11px;
  font-weight: 700;
  cursor: pointer;
  transition: border-color 0.15s, color 0.15s;
}
.dm-sell-btn:hover {
  border-color: #e7c14b;
  color: #e7c14b;
}
.dm-sell-btn.on {
  border-color: #e7c14b;
  color: #e7c14b;
}
.dm-sell-form {
  display: flex;
  flex-direction: column;
  gap: 8px;
  border: 1px solid rgba(231, 193, 75, 0.4);
  border-radius: 8px;
  padding: 10px;
  background: rgba(231, 193, 75, 0.05);
}
.dm-sell-price {
  font-size: 13px;
}
.dm-sell-durs {
  display: flex;
  gap: 6px;
}
.dm-dur {
  flex: 1;
  padding: 6px 4px;
  border-radius: 6px;
  border: 1px solid var(--border);
  background: rgba(0, 0, 0, 0.25);
  color: var(--text);
  font-size: 11px;
  cursor: pointer;
  transition: border-color 0.15s, background 0.15s, opacity 0.15s;
}
.dm-dur:hover:not(:disabled) {
  border-color: #e7c14b;
}
.dm-dur.on {
  background: #e7c14b;
  color: #04130d;
  border-color: #e7c14b;
}
.dm-dur:disabled {
  opacity: 0.3;
  cursor: not-allowed;
}
.dm-net {
  font-size: 11px;
  color: var(--accent);
}
.dm-balance {
  font-size: 11px;
  color: var(--text-dim);
}
.dm-balance.low {
  color: #ff8a8a;
}
.dm-balance-warn {
  color: #ff6a6a;
  font-weight: 700;
}
.dm-flash {
  padding: 8px 12px;
  border-radius: 8px;
  font-size: 12px;
  background: rgba(0, 200, 120, 0.12);
  border: 1px solid rgba(0, 200, 120, 0.4);
  color: var(--accent);
}
.dm-flash.err {
  background: rgba(255, 90, 90, 0.12);
  border-color: rgba(255, 90, 90, 0.4);
  color: #ff8a8a;
}
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.2s ease;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
.dim {
  color: var(--text-dim);
}
</style>
