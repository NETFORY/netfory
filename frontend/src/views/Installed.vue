<script setup lang="ts">
import { ref, onMounted, computed, reactive, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { useAppsStore } from '@/store/apps'
import { useTabsStore } from '@/store/tabs'
import { useDiscoveryStore } from '@/store/discovery'
import { useEarningsStore } from '@/store/earnings'
import { useAuthStore } from '@/store/auth'
import { isTauri, appLogo, usernameOfAddress } from '@/lib/bridge'
import { shortAddr } from '@/lib/crypto'
import Identicon from '@/components/Identicon.vue'
import AboutModal from '@/components/AboutModal.vue'
import PayoutHistoryModal from '@/components/PayoutHistoryModal.vue'
import SeederEarnings from '@/components/SeederEarnings.vue'

const { t } = useI18n()
const router = useRouter()
const apps = useAppsStore()
const tabs = useTabsStore()
const discovery = useDiscoveryStore()
const earnings = useEarningsStore()
const auth = useAuthStore()

const confirmDelete = ref('')
const flash = ref('')
// история выплат сидера (перенесена сюда из MY_APPLICATIONS —
// начисления идут за установленные/сидируемые приложения)
const payoutHistoryOpen = ref(false)
const uninstallError = ref('')
const deletingId = ref('')
const aboutId = ref('')
const aboutTitle = ref('')
// Начисления по DAPP — id приложения, для которого открыт popover (или '').
const earningsFor = ref('')
const earningsTitle = ref('')
const copied = ref('')
const logos = reactive<Record<string, string>>({})
// owner-address → registered u:// username ('' = resolved, none on-chain)
const authors = reactive<Record<string, string>>({})

async function loadLogos() {
  for (const m of apps.installedApps) {
    if (logos[m.id] === undefined) {
      const url = await appLogo(m.id)
      if (url) logos[m.id] = url
    }
  }
}

// Resolve each foreign developer's address to a @username for a verified-badge
// effect; clicking it routes to their public u:// profile.
async function loadAuthors() {
  for (const m of apps.installedApps) {
    if (m.owner && authors[m.owner] === undefined) {
      authors[m.owner] = (await usernameOfAddress(m.owner)) || ''
    }
  }
}

function authorLabel(owner: string): string {
  return authors[owner] ? '@' + authors[owner] : shortAddr(owner, 8, 6)
}

function openAuthor(owner: string) {
  if (!owner) return
  // v1.63.1 BUGFIX: раньше передавали `authors[owner] || owner` (username
  // если известен, иначе address). Проблема: UserProfile.vue использует
  // `route.params.identity` как ключ для запросов followStats, wall posts,
  // rewards и т.п. — все эти функции ожидают АДРЕС, а не username.
  // Передача username ломает загрузку профиля (пустые wall/stats). Всегда
  // отдаём address; отображение @username поверх — забота UserProfile.
  router.push({ name: 'profile-view', params: { identity: owner } })
}

// network-published update available for an installed app id (P2P sync)
function updateInfo(id: string) {
  return discovery.apps.find((a) => a.id === id && a.updateAvailable) || null
}
// Есть ли по dApp ненулевая статистика выплат — если ВСЁ по нулям, золотую
// $-монету на карточке не показываем (визуальный шум для пустых приложений).
// Монета видна, когда у приложения есть активный банк выплат (bankSth) ИЛИ
// личные начисления/прогноз — раньше проверялись только личные est/forecast,
// из-за чего монета пропадала у всех приложений с живой статистикой пула.
function hasEarnings(id: string): boolean {
  const e = earnings.byApp(id)
  if (!e) return false
  return (e.bankSth || 0) > 0 || (e.estSth || 0) > 0 || (e.forecastSth || 0) > 0
}
const hasUpdates = computed(() => discovery.pendingUpdates.length > 0)

async function applyUpdate(id: string) {
  const ok = await discovery.applyUpdate(id)
  if (ok) {
    flash.value = t('installed.updated')
    setTimeout(() => (flash.value = ''), 2500)
  }
  return ok
}

// «Обновить всё»: последовательно применяем все доступные обновления
const updatingAll = ref(false)
async function updateAll() {
  if (updatingAll.value) return
  updatingAll.value = true
  try {
    for (const a of [...discovery.pendingUpdates]) {
      await applyUpdate(a.id)
    }
  } finally {
    updatingAll.value = false
  }
}

async function copyAddress(id: string) {
  try {
    await navigator.clipboard.writeText(`sth://${id}`)
  } catch {
    /* noop */
  }
  copied.value = id
  setTimeout(() => {
    if (copied.value === id) copied.value = ''
  }, 1500)
}

onMounted(() => {
  if (!apps.loaded) apps.init()
  loadLogos()
  loadAuthors()
  // run discovery so published updates for installed apps are detected
  if (isTauri) discovery.scan()
  // Обновляем снапшот начислений — нужен, чтобы решить, показывать ли
  // золотую $-монету на карточке приложения (v1.63.2: скрываем при нуле).
  if (isTauri) earnings.refresh()
})

// Адрес кошелька грузится асинхронно: если на момент mount он ещё пуст,
// earnings.refresh() молча выходит и монеты не появляются вовсе. Догоняем
// обновление, как только адрес стал известен.
watch(
  () => auth.address,
  (addr) => {
    if (addr && isTauri) earnings.refresh()
  }
)

function launch(m: { id: string; name: string }) {
  tabs.open(m.id, m.name || m.id)
}

function openAbout(m: { id: string; name: string }) {
  aboutId.value = m.id
  aboutTitle.value = m.name
}

/** Открыть popover «Начисления по DAPP» для конкретного приложения. */
function openEarnings(m: { id: string; name: string }) {
  earningsFor.value = m.id
  earningsTitle.value = m.name || m.id
}

async function uninstall(id: string) {
  confirmDelete.value = ''
  uninstallError.value = ''
  deletingId.value = id
  try {
    await apps.remove(id)
    tabs.close(id)
    flash.value = t('installed.removed')
    setTimeout(() => (flash.value = ''), 2000)
  } catch (e: any) {
    // Surface the failure — otherwise the UI just snaps back to the initial
    // "Uninstall" button as if nothing happened (that was the exact symptom
    // the user reported). Logged to console for developer diagnostics too.
    const msg = String(e?.message ?? e ?? 'unknown error')
    console.error('[installed] uninstall failed for', id, e)
    uninstallError.value = `${t('installed.removeFailed')}: ${msg}`
    setTimeout(() => (uninstallError.value = ''), 6000)
  } finally {
    deletingId.value = ''
  }
}
</script>

<template>
  <div class="installed" data-testid="installed-view">
    <div class="inst-hero">
      <h1 class="page-title">// {{ t('installed.title') }}_</h1>
      <button
        class="btn btn-ghost payouts-btn"
        @click="payoutHistoryOpen = true"
        data-testid="open-payout-history-btn"
        :title="t('payoutHistory.buttonHint')"
      >
        <span class="payouts-ic">▤</span>
        <span>{{ t('payoutHistory.button') }}</span>
      </button>
    </div>
    <p class="hint dim">{{ t('installed.hint') }}</p>

    <PayoutHistoryModal :open="payoutHistoryOpen" @close="payoutHistoryOpen = false" />

    <p v-if="flash" class="flash hl" data-testid="installed-flash">{{ flash }}</p>
    <p v-if="uninstallError" class="flash err" data-testid="installed-uninstall-error">⚠ {{ uninstallError }}</p>

    <div
      v-if="hasUpdates"
      class="upd-banner panel"
      data-testid="installed-updates-banner"
    >
      <span class="upd-dot">⟳</span>
      <span>{{ t('installed.updatesAvailable', { n: discovery.pendingUpdates.length }) }}</span>
      <span class="dim">·</span>
      <span class="dim">{{ discovery.autoUpdate ? t('installed.autoOn') : t('installed.autoOff') }}</span>
      <button
        class="btn btn-primary upd-all-btn"
        :disabled="updatingAll"
        @click="updateAll"
        data-testid="update-all-btn"
      >
        {{ updatingAll ? '…' : t('installed.updateAll') }}
      </button>
    </div>

    <div v-if="apps.installedApps.length" class="grid" data-testid="installed-list">
      <div
        v-for="m in apps.installedApps"
        :key="m.id"
        class="card panel"
        :data-testid="`installed-${m.id}`"
      >
        <div v-if="deletingId === m.id" class="del-overlay" :data-testid="`deleting-${m.id}`">
          <span class="del-spin"></span>
          <span class="del-text">Идёт удаление…</span>
        </div>
        <div class="top">
          <span class="logo-badge">
            <img v-if="logos[m.id]" :src="logos[m.id]" alt="" />
            <Identicon v-else :seed="m.id" :size="36" />
          </span>
          <span class="name">
            // {{ m.name || t('discover.noTitle') }}
            <span class="ver-badge" :data-testid="`installed-ver-${m.id}`">[v{{ m.version || 1 }}]</span>
          </span>
          <!-- ⓘ Начисления по DAPP: золотая монета → popover со всей
               «твоё начисление / банк / сидинг»-статистикой + sth://-ссылкой.
               v1.63.2: скрываем монету, если по dApp нет начислений (ни
               фактических, ни прогнозных) — она вводила в заблуждение. -->
          <button
            v-if="hasEarnings(m.id)"
            class="coin"
            @click="openEarnings(m)"
            :title="t('installed.earningsTitle')"
            :data-testid="`earnings-${m.id}`"
            aria-label="Earnings"
          >
            <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <circle cx="12" cy="12" r="9" />
              <path d="M12 6v12" />
              <path d="M15 9.5c0-1.4-1.34-2.5-3-2.5s-3 1.1-3 2.5S10.34 12 12 12s3 1.1 3 2.5-1.34 2.5-3 2.5-3-1.1-3-2.5" />
            </svg>
          </button>
          <!-- ★ Избранное — как раньше. -->
          <button
            class="star"
            :class="{ on: apps.isFavorite(m.id) }"
            @click="apps.toggleFavorite(m.id)"
            :title="t('installed.favorite')"
            :data-testid="`fav-${m.id}`"
          >
            {{ apps.isFavorite(m.id) ? '★' : '☆' }}
          </button>
          <!-- 📋 Копировать sth://-ссылку на приложение (как в Discover). -->
          <button
            class="copy-sq"
            @click="copyAddress(m.id)"
            :title="`sth://${m.id}`"
            :data-testid="`installed-copy-${m.id}`"
            aria-label="Copy link"
          >
            <svg v-if="copied !== m.id" viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect>
              <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
            </svg>
            <span v-else class="copy-ok">✓</span>
          </button>
        </div>

        <div
          v-if="updateInfo(m.id)"
          class="upd-tag"
          :data-testid="`installed-update-tag-${m.id}`"
        >
          ⟳ {{ t('installed.newVersion', { v: updateInfo(m.id)?.netVersion }) }}
        </div>
        <p class="desc dim">{{ m.description || t('discover.noDescription') }}</p>

        <div v-if="m.tags && m.tags.length" class="card-tags">
          <span v-for="tg in m.tags" :key="tg" class="card-tag">#{{ tg }}</span>
        </div>

        <div class="author dim" :data-testid="`installed-author-${m.id}`">
          {{ t('installed.author') }}:
          <code
            class="author-link"
            :class="{ verified: !!authors[m.owner] }"
            @click="openAuthor(m.owner)"
            :title="m.owner"
            :data-testid="`installed-author-link-${m.id}`"
          >{{ authorLabel(m.owner) }}</code>
        </div>

        <div class="actions">
          <button
            v-if="updateInfo(m.id)"
            class="btn btn-update"
            :disabled="discovery.updatingId === m.id"
            @click="applyUpdate(m.id)"
            :data-testid="`installed-update-${m.id}`"
          >
            {{ discovery.updatingId === m.id ? t('installed.updating') : t('installed.update') }} ⟳
          </button>
          <button class="btn btn-primary" @click="launch(m)" :data-testid="`installed-open-${m.id}`">
            {{ t('discover.open') }} →
          </button>
          <!-- «[?]» icon-only заменяет прежнюю текстовую кнопку «О ПРИЛОЖЕНИИ». -->
          <button
            class="btn btn-icon"
            @click="openAbout(m)"
            :title="t('about.button')"
            :aria-label="t('about.button')"
            :data-testid="`installed-about-${m.id}`"
          >
            ?
          </button>
          <button
            v-if="confirmDelete !== m.id"
            class="btn btn-danger"
            @click="confirmDelete = m.id"
            :data-testid="`uninstall-${m.id}`"
          >
            {{ t('installed.uninstall') }} ✕
          </button>
          <template v-else>
            <button class="btn" @click="confirmDelete = ''" :data-testid="`uninstall-cancel-${m.id}`">
              {{ t('common.cancel') }}
            </button>
            <button class="btn btn-danger" @click="uninstall(m.id)" :data-testid="`uninstall-confirm-${m.id}`">
              {{ t('common.confirm') }}
            </button>
          </template>
        </div>
      </div>
    </div>

    <div v-else class="empty panel" data-testid="installed-empty">
      <span class="empty-mark">[ 0x00 ]</span>
      <span class="dim">{{ t('installed.empty') }}</span>
    </div>

    <AboutModal v-if="aboutId" :app-id="aboutId" :title="aboutTitle" @close="aboutId = ''" />

    <!-- 🪙 Popover «Начисления по DAPP»: заменяет прежний inline-виджет
         внутри карточки. Полный SeederEarnings + sth://-ссылка с копией. -->
    <transition name="fade">
      <div
        v-if="earningsFor"
        class="ep-back"
        @click.self="earningsFor = ''"
        data-testid="installed-earnings-modal"
      >
        <div class="ep-modal panel" role="dialog" aria-modal="true">
          <button class="ep-x" @click="earningsFor = ''" aria-label="Close">✕</button>
          <h2 class="ep-title">{{ t('installed.earningsTitle') }}</h2>
          <p class="ep-sub dim">{{ earningsTitle }}</p>
          <SeederEarnings :app-id="earningsFor" />
          <div class="ep-linkrow">
            <code class="ep-link" :title="`sth://${earningsFor}`">sth://{{ shortAddr(earningsFor, 14, 10) }}</code>
            <button
              class="ep-copy"
              @click="copyAddress(earningsFor)"
              :title="t('discover.copyLink')"
              data-testid="installed-earnings-copy"
              aria-label="Copy link"
            >
              <svg v-if="copied !== earningsFor" viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect>
                <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
              </svg>
              <span v-else>✓</span>
            </button>
          </div>
        </div>
      </div>
    </transition>
  </div>
</template>

<style scoped>
.inst-hero {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 16px;
  flex-wrap: wrap;
}
.payouts-btn {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-size: 11px;
  letter-spacing: 0.1em;
  font-weight: var(--fw-700);
  padding: 8px 14px;
  border: 1px solid var(--border-accent);
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 8%, transparent);
  transition: background-color 0.15s ease, transform 0.15s ease;
}
.payouts-btn:hover {
  background: color-mix(in srgb, var(--accent) 16%, transparent);
  transform: translateY(-1px);
}
.payouts-ic {
  font-family: var(--font-mono, monospace);
  font-size: 13px;
}
.installed {
  max-width: none;
  width: 100%;
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.page-title {
  font-size: 15px;
  letter-spacing: 0.14em;
  color: var(--accent);
  font-weight: var(--fw-700);
}
.hint {
  font-size: 12px;
}
.flash {
  font-size: 12px;
}
.flash.err {
  color: var(--danger);
  padding: 8px 12px;
  border-left: 2px solid var(--danger);
  background: color-mix(in srgb, var(--danger) 10%, transparent);
}
.upd-banner {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 14px;
  font-size: 12px;
  border-left: 2px solid var(--accent);
}
.upd-all-btn {
  margin-left: auto;
  font-size: 11px;
  padding: 6px 14px;
}
.upd-dot {
  color: var(--accent);
  display: inline-block;
  animation: spin 2.4s linear infinite;
}
.upd-tag {
  align-self: flex-start;
  font-size: 10px;
  letter-spacing: 0.04em;
  padding: 2px 8px;
  border-radius: 999px;
  color: var(--accent);
  border: 1px solid var(--accent);
  background: color-mix(in srgb, var(--accent) 12%, transparent);
}
.ver-badge {
  font-size: 10px;
  font-weight: var(--fw-700);
  color: var(--secondary);
  letter-spacing: 0.04em;
  margin-left: 4px;
}
.btn-update {
  border-color: var(--accent);
  color: var(--accent);
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}
.grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(330px, 1fr));
  gap: 14px;
}
.card {
  position: relative;
  padding: 16px 18px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  border-left: 2px solid var(--secondary);
}
.del-overlay {
  position: absolute;
  inset: 0;
  z-index: 5;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 14px;
  background: rgba(6, 10, 16, 0.82);
  backdrop-filter: blur(3px);
  border-radius: inherit;
}
.del-text {
  color: #fca5a5;
  font-size: 14px;
  letter-spacing: 1px;
}
.del-spin {
  width: 20px;
  height: 20px;
  border: 2px solid rgba(252, 165, 165, 0.25);
  border-top-color: #fca5a5;
  border-radius: 50%;
  animation: del-rot 0.7s linear infinite;
}
@keyframes del-rot {
  to {
    transform: rotate(360deg);
  }
}
.top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}
.logo-badge {
  flex: none;
  width: 36px;
  height: 36px;
  border-radius: 9px;
  overflow: hidden;
  line-height: 0;
}
.logo-badge img {
  width: 36px;
  height: 36px;
  object-fit: cover;
  border-radius: 9px;
}
.name {
  flex: 1;
  font-size: 14px;
  font-weight: var(--fw-700);
  color: var(--accent);
  word-break: break-word;
}
.ver {
  font-size: 11px;
}
/* ★ Избранное — тот же квадратный chip-стиль, что у 🪙 «монеты» и 📋 «копии»
   (28×28, radius 8px, edge, hover glow), чтобы верхний ряд карточки читался
   как единая control-полоса из трёх одинаковых action-кнопок. */
.star {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px; height: 28px;
  padding: 0;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 8px;
  color: var(--text-dim);
  font-size: 15px;
  line-height: 1;
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease, transform 0.15s ease, box-shadow 0.2s ease;
}
.star:hover {
  color: var(--warning, #f5b301);
  border-color: var(--warning, #f5b301);
  background: rgba(245,158,11,0.08);
  transform: translateY(-1px);
}
.star.on {
  color: var(--warning, #f5b301);
  border-color: rgba(245,158,11,0.55);
  background: rgba(245,158,11,0.12);
  box-shadow: 0 0 12px rgba(245,158,11,0.35);
}
.card-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.card-tag {
  font-size: 10px;
  padding: 2px 7px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--text-dim);
}
.desc {
  font-size: 12px;
  line-height: 1.5;
}
.author {
  font-size: 11px;
}
.author-link {
  cursor: pointer;
  color: var(--text);
  border-bottom: 1px dashed transparent;
  transition: color 0.15s ease, border-color 0.15s ease;
}
.author-link:hover {
  color: var(--accent);
  border-bottom-color: var(--accent);
}
.author-link.verified {
  color: var(--accent);
  font-weight: var(--fw-700);
}
.url {
  font-size: 11px;
  padding: 6px 8px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  word-break: break-all;
}
.url-copy {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  background: transparent;
  border: 0;
  padding: 0;
  cursor: pointer;
  text-align: left;
}
.url-copy .url {
  flex: 1;
  transition: border-color 0.2s ease;
}
.url-copy:hover .url {
  border-color: var(--accent);
}
.copy-ic {
  font-size: 13px;
  color: var(--text-dim);
  flex-shrink: 0;
  width: 18px;
  text-align: center;
}
.url-copy:hover .copy-ic {
  color: var(--accent);
}
.actions {
  display: flex;
  gap: 10px;
  padding-top: 6px;
  border-top: 1px solid var(--border);
  flex-wrap: wrap;
}
.actions .btn {
  padding: 8px 12px;
}
.empty {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 18px;
  font-size: 12px;
}
.empty-mark {
  color: var(--secondary);
  letter-spacing: 0.1em;
}

/* ── v1.60.1 clean installed cards: coin + copy-square + [?] icon ── */
.coin {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px; height: 28px;
  padding: 0;
  border: 1px solid rgba(245,158,11,0.32);
  border-radius: 8px;
  background: rgba(245,158,11,0.06);
  color: #f5b301;
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease, transform 0.15s ease;
}
.coin:hover {
  color: #fbbf24;
  border-color: #f5b301;
  background: rgba(245,158,11,0.14);
  transform: translateY(-1px);
  box-shadow: 0 0 12px rgba(245,158,11,0.35);
}

.copy-sq {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px; height: 28px;
  padding: 0;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 8px;
  color: var(--text-dim);
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease, transform 0.15s ease;
}
.copy-sq:hover { color: var(--accent); border-color: var(--accent); background: rgba(176,133,255,0.08); transform: translateY(-1px); }
.copy-ok { color: #22c55e; font-size: 14px; font-weight: 700; }

/* Icon-only «[?]» кнопка «О приложении». */
.btn-icon {
  min-width: 34px;
  padding: 0 10px;
  font-weight: var(--fw-700);
  font-size: 14px;
}

/* Earnings popover — Discover-style, clean */
.ep-back {
  position: fixed; inset: 0; z-index: 2000;
  background: rgba(0,0,0,0.55);
  backdrop-filter: blur(6px);
  -webkit-backdrop-filter: blur(6px);
  display: flex; align-items: center; justify-content: center;
  padding: 24px;
}
.ep-modal {
  position: relative;
  width: 100%;
  max-width: 460px;
  border-radius: 16px;
  padding: 26px 28px 22px;
  background: var(--surface);
  border: 1px solid var(--border);
  box-shadow: 0 24px 64px rgba(0,0,0,0.55);
}
.ep-x {
  position: absolute; top: 12px; right: 14px;
  width: 30px; height: 30px;
  border: 0; background: transparent; color: var(--text-dim);
  font-size: 18px; cursor: pointer; border-radius: 8px;
  transition: color 0.15s ease, background-color 0.15s ease;
}
.ep-x:hover { color: var(--text); background: rgba(255,255,255,0.06); }
.ep-title { font-size: 15px; font-weight: var(--fw-700); margin: 0 0 4px; color: #f5b301; letter-spacing: 0.03em; }
.ep-sub { font-size: 12px; margin: 0 0 14px; }
.ep-linkrow {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 14px;
  padding-top: 12px;
  border-top: 1px dashed var(--border);
}
.ep-link {
  flex: 1;
  min-width: 0;
  font-size: 11px;
  color: var(--text-dim);
  font-family: var(--font-mono);
  padding: 6px 10px;
  background: rgba(0,0,0,0.18);
  border: 1px dashed var(--border);
  border-radius: 8px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.ep-copy {
  width: 30px; height: 30px;
  display: inline-flex; align-items: center; justify-content: center;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 8px;
  color: var(--text-dim);
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease, background-color 0.15s ease;
}
.ep-copy:hover { color: var(--accent); border-color: var(--accent); background: rgba(176,133,250,0.08); }

.fade-enter-active, .fade-leave-active { transition: opacity 0.15s ease; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
</style>
