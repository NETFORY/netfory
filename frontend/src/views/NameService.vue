<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { listUsernames, getMyUsername, weeklyTop, type NameEntry, type TopEntry } from '@/lib/bridge'
import Identicon from '@/components/Identicon.vue'
import IdentityRegistration from '@/components/IdentityRegistration.vue'

const { t } = useI18n()
const router = useRouter()

const all = ref<NameEntry[]>([])
const top = ref<TopEntry[]>([])
const loading = ref(true)
const search = ref('')
const pageSize = ref(15)
const page = ref(1)
const myUsername = ref<string | null>(null)
const showRegister = ref(false)

async function load() {
  loading.value = true
  try {
    all.value = await listUsernames()
    top.value = await weeklyTop(3)
    myUsername.value = await getMyUsername()
  } finally {
    loading.value = false
  }
}
onMounted(load)

const filtered = computed(() => {
  const q = search.value.trim().toLowerCase()
  if (!q) return all.value
  return all.value.filter((e) => e.name.includes(q))
})

const totalPages = computed(() => Math.max(1, Math.ceil(filtered.value.length / pageSize.value)))
const paged = computed(() => {
  if (page.value > totalPages.value) page.value = totalPages.value
  const start = (page.value - 1) * pageSize.value
  return filtered.value.slice(start, start + pageSize.value)
})

function setPageSize(n: number) {
  pageSize.value = n
  page.value = 1
}
function goPage(p: number) {
  page.value = Math.min(Math.max(1, p), totalPages.value)
}
function openProfile(e: NameEntry) {
  router.push({ name: 'profile-view', params: { identity: e.address } })
}
function openAddr(address: string) {
  router.push({ name: 'profile-view', params: { identity: address } })
}
function onRegistered() {
  showRegister.value = false
  load()
}
</script>

<template>
  <div class="ns" data-testid="nameservice-view">
    <div class="head">
      <h1 class="page-title">// {{ t('nameservice.title') }}_</h1>
      <span class="ns-total dim" data-testid="ns-total">{{ t('nameservice.total', { n: all.length }) }}</span>
    </div>
    <p class="hint dim">{{ t('nameservice.hint') }}</p>

    <!-- Trending this week: top-3 fastest-growing names -->
    <div v-if="top.length" class="topweek" data-testid="ns-topweek">
      <span class="tw-title">{{ t('nameservice.topWeek') }}</span>
      <div class="tw-list">
        <button
          v-for="(e, i) in top"
          :key="e.address"
          class="tw-card"
          :class="{ team: e.verifiedTeam }"
          @click="openAddr(e.address)"
          :data-testid="`ns-top-${e.name}`"
        >
          <span class="tw-rank">#{{ i + 1 }}</span>
          <Identicon :seed="e.address" :size="34" />
          <div class="tw-meta">
            <span class="tw-name">@{{ e.name }}</span>
            <span class="tw-growth">▲ +{{ e.growth || e.subscribers }} {{ t('nameservice.thisWeek') }}</span>
          </div>
        </button>
      </div>
    </div>

    <!-- register CTA when the wallet has no name yet -->
    <button
      v-if="!loading && !myUsername"
      class="btn btn-primary ns-reg-cta"
      @click="showRegister = true"
      data-testid="ns-register-cta"
    >
      ⬡ {{ t('nameservice.registerCta') }}
    </button>

    <!-- controls -->
    <div class="ns-controls">
      <input
        v-model="search"
        class="ns-search"
        :placeholder="t('nameservice.search')"
        data-testid="ns-search-input"
        @input="page = 1"
      />
      <div class="ns-pagesize" data-testid="ns-pagesize">
        <span class="dim">{{ t('nameservice.perPage') }}:</span>
        <button
          v-for="n in [15, 30, 60]"
          :key="n"
          class="ps-btn"
          :class="{ active: pageSize === n }"
          @click="setPageSize(n)"
          :data-testid="`ns-pagesize-${n}`"
        >{{ n }}</button>
      </div>
    </div>

    <div v-if="loading" class="ns-state dim" data-testid="ns-loading">{{ t('common.loading') }}…</div>
    <div v-else-if="!all.length" class="ns-state dim" data-testid="ns-empty">{{ t('nameservice.empty') }}</div>
    <div v-else-if="!filtered.length" class="ns-state dim" data-testid="ns-nomatch">{{ t('nameservice.noMatch') }}</div>

    <!-- 5-column profile grid -->
    <div v-else class="ns-grid" data-testid="ns-grid">
      <button
        v-for="e in paged"
        :key="e.address"
        class="ns-card"
        :class="{ team: e.verifiedTeam }"
        @click="openProfile(e)"
        :data-testid="`ns-card-${e.name}`"
      >
        <span
          v-if="e.verifiedTeam"
          class="badge-team"
          :title="t('nameservice.verifiedTeam')"
          :data-testid="`ns-team-${e.name}`"
        >✦ SmartHoldem</span>
        <Identicon :seed="e.address" :size="44" />
        <span class="ns-name">
          @{{ e.name }}
          <span
            v-if="e.verified && !e.verifiedTeam"
            class="badge-verified"
            :title="t('nameservice.verified')"
            :data-testid="`ns-verified-${e.name}`"
          >✓</span>
        </span>
        <span class="ns-subs" :data-testid="`ns-subs-${e.name}`">❤ {{ e.subscribers }}</span>
      </button>
    </div>

    <!-- pagination -->
    <div v-if="filtered.length > pageSize" class="pager" data-testid="ns-pager">
      <button class="pager-btn" :disabled="page <= 1" @click="goPage(page - 1)" data-testid="ns-prev">
        ← {{ t('nameservice.prev') }}
      </button>
      <span class="pager-info dim" data-testid="ns-page-info">
        {{ t('nameservice.page', { page, total: totalPages }) }}
      </span>
      <button class="pager-btn" :disabled="page >= totalPages" @click="goPage(page + 1)" data-testid="ns-next">
        {{ t('nameservice.next') }} →
      </button>
    </div>

    <IdentityRegistration v-if="showRegister" @close="showRegister = false" @registered="onRegistered" />
  </div>
</template>

<style scoped>
.ns {
  max-width: 1500px;
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 16px;
  flex-wrap: wrap;
}
.page-title {
  font-size: 15px;
  letter-spacing: 0.14em;
  color: var(--accent);
  font-weight: var(--fw-700);
}
.ns-total {
  font-size: 12px;
}
.hint {
  font-size: 12px;
  line-height: 1.6;
}
.topweek {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 14px 16px;
  background: linear-gradient(150deg, rgba(212, 175, 55, 0.06), var(--panel) 70%);
  border: 1px solid var(--border);
  border-left: 3px solid #d4af37;
  border-radius: var(--radius);
}
.tw-title {
  font-size: 12px;
  font-weight: var(--fw-800);
  letter-spacing: 0.12em;
  color: #e9c757;
}
.tw-list {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 10px;
}
@media (max-width: 720px) {
  .tw-list { grid-template-columns: 1fr; }
}
.tw-card {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 12px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  cursor: pointer;
  text-align: left;
  transition: border-color 0.15s ease, transform 0.15s ease;
}
.tw-card:hover {
  border-color: #d4af37;
  transform: translateY(-2px);
}
.tw-card.team {
  border-color: #b9923b;
}
.tw-rank {
  font-size: 15px;
  font-weight: var(--fw-800);
  color: #e9c757;
  min-width: 24px;
}
.tw-meta {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.tw-name {
  font-size: 13px;
  font-weight: var(--fw-700);
  color: var(--accent);
  word-break: break-all;
}
.tw-growth {
  font-size: 10px;
  color: #4ade80;
  font-weight: var(--fw-700);
}
.ns-reg-cta {
  align-self: flex-start;
  padding: 10px 16px;
}
.ns-controls {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  flex-wrap: wrap;
}
.ns-search {
  flex: 1;
  min-width: 220px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  padding: 10px 14px;
  color: var(--text);
  font-size: 14px;
  font-family: var(--font-mono, monospace);
  outline: none;
  transition: border-color 0.15s ease;
}
.ns-search:focus {
  border-color: var(--accent);
}
.ns-pagesize {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
}
.ps-btn {
  padding: 5px 11px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--text-dim);
  cursor: pointer;
  transition: border-color 0.15s ease, color 0.15s ease;
}
.ps-btn:hover {
  color: var(--accent);
}
.ps-btn.active {
  color: var(--accent);
  border-color: var(--accent);
}
.ns-state {
  padding: 24px;
  text-align: center;
  font-size: 13px;
  border: 1px dashed var(--border);
  border-radius: var(--radius);
}
.ns-grid {
  display: grid;
  grid-template-columns: repeat(5, 1fr);
  gap: 12px;
}
@media (max-width: 1100px) {
  .ns-grid { grid-template-columns: repeat(3, 1fr); }
}
@media (max-width: 640px) {
  .ns-grid { grid-template-columns: repeat(2, 1fr); }
}
.ns-card {
  position: relative;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 16px 10px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  cursor: pointer;
  transition: border-color 0.16s ease, transform 0.16s ease, box-shadow 0.16s ease;
}
.ns-card:hover {
  border-color: var(--accent);
  transform: translateY(-2px);
  box-shadow: 0 0 20px -10px var(--accent);
}
.ns-card.team {
  border-color: #b9923b;
  background: linear-gradient(160deg, #14110a 0%, var(--panel) 60%);
}
.ns-card.team:hover {
  box-shadow: 0 0 24px -8px #d4af37;
}
.ns-name {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 13px;
  font-weight: var(--fw-700);
  color: var(--accent);
  word-break: break-all;
  text-align: center;
}
.ns-subs {
  font-size: 11px;
  color: #f472b6;
  font-weight: var(--fw-700);
}
.badge-verified {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 16px;
  height: 16px;
  font-size: 10px;
  border-radius: 50%;
  background: #16a34a;
  color: #02140a;
  box-shadow: 0 0 8px -1px #22c55e;
}
/* Metallic shimmering plaque for team-verified citizens */
.badge-team {
  position: absolute;
  top: 6px;
  right: 6px;
  font-size: 9px;
  font-weight: var(--fw-800);
  letter-spacing: 0.04em;
  padding: 3px 7px;
  border-radius: 6px;
  color: #2b2206;
  border: 1px solid #f5e2a0;
  background: linear-gradient(
    100deg,
    #8a6d2b 0%,
    #d4af37 18%,
    #fff6cf 34%,
    #d4af37 50%,
    #8a6d2b 66%,
    #fff6cf 82%,
    #d4af37 100%
  );
  background-size: 250% 100%;
  animation: shimmer 3s linear infinite;
  box-shadow: 0 0 10px -2px #d4af37, inset 0 1px 0 rgba(255, 255, 255, 0.5);
}
@keyframes shimmer {
  0% { background-position: 0% 50%; }
  100% { background-position: 250% 50%; }
}
.pager {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 16px;
  padding: 8px 0;
}
.pager-btn {
  font-size: 12px;
  padding: 8px 14px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  color: var(--text);
  cursor: pointer;
  transition: border-color 0.15s ease, color 0.15s ease;
}
.pager-btn:hover:not(:disabled) {
  border-color: var(--accent);
  color: var(--accent);
}
.pager-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.pager-info {
  font-size: 12px;
  min-width: 90px;
  text-align: center;
}
</style>
