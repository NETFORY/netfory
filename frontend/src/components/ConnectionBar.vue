<script setup lang="ts">
/* eslint-disable no-undef */
import { ref, computed, watch, onMounted, onBeforeUnmount } from 'vue'
import { useRouter } from 'vue-router'
import {
  appListen, isTauri, cloudSeederStatusGet, getProcessResources,
  currentNode, pingCustomNode, listCustomNodes, addCustomNode, selectNode,
} from '@/lib/bridge'
import { ownSeeder } from '@/lib/seederRep'

// ── Local Node Nudge (ОТКЛЮЧЁН, SHIP-44) ─────────────────────────────────
// Ранее: пинг localhost:4003 через минуту после старта и раз в 30 минут, плашка
// «найден локальный узел». Теперь кошелёк идёт через iroh-пул sth-core — авто-
// определение не запускается (код оставлен для ручного включения в onMounted).
const LOCAL_NODE_CANDIDATES = ['http://127.0.0.1:4003', 'http://localhost:4003']
const LOCAL_POLL_MS = 30 * 60_000
const LOCAL_SNOOZE_MS = 6 * 60 * 60_000
const LS_LOCAL_DISMISSED = 'sth_local_node_nudge_dismissed_at'
const localNodeHost = ref('')
const localNodeHeight = ref<number | null>(null)
const localNodeActive = ref(false)
const localNodeBusy = ref(false)
const localNodeSnoozedUntil = ref(Number(localStorage.getItem(LS_LOCAL_DISMISSED) || 0) + LOCAL_SNOOZE_MS)
let localPoll: ReturnType<typeof setInterval> | null = null

const localNodeNudge = computed(
  () => !!localNodeHost.value && !localNodeActive.value && Date.now() >= localNodeSnoozedUntil.value,
)

async function refreshLocalNode() {
  if (!isTauri) return
  // если локальный узел снузнут — не пингуем вовсе
  if (Date.now() < localNodeSnoozedUntil.value) return
  try {
    const active = await currentNode()
    localNodeActive.value = LOCAL_NODE_CANDIDATES.includes(active)
    if (localNodeActive.value) return
    for (const host of LOCAL_NODE_CANDIDATES) {
      const p = await pingCustomNode(host).catch(() => null)
      if (p?.online) {
        localNodeHost.value = host
        localNodeHeight.value = p.height ?? null
        return
      }
    }
    localNodeHost.value = ''
  } catch {
    localNodeHost.value = ''
  }
}
async function connectLocalNode() {
  const host = localNodeHost.value
  if (!host || localNodeBusy.value) return
  localNodeBusy.value = true
  try {
    const list = await listCustomNodes()
    if (!list.includes(host)) await addCustomNode(host)
    await selectNode(host)
    localNodeActive.value = true
  } catch {
    /* узел отвалился между пингом и выбором — плашка останется до следующего опроса */
  } finally {
    localNodeBusy.value = false
  }
}
function dismissLocalNode() {
  localStorage.setItem(LS_LOCAL_DISMISSED, String(Date.now()))
  localNodeSnoozedUntil.value = Date.now() + LOCAL_SNOOZE_MS
}

// ── Memory Guard ─────────────────────────────────────────────────────────
// Раз в 5 минут читаем RSS / commit / хендлы / GDI / USER процесса (Rust,
// crashdiag::snapshot — три syscall'а). Пороги считает бэкенд; здесь только показ
// в нижней строке состояния. Клик по плашке — «понял», скрыть на 30 минут.
const RES_POLL_MS = 5 * 60_000
const RES_SNOOZE_MS = 30 * 60_000
const resAlerts = ref<string[]>([])
const resCensus = ref('')
const diskCorrupt = ref(false)
const diskCorruptWhere = ref('')
const LS_DISK_DISMISSED = 'sth_disk_corrupt_dismissed_at'
const diskSnoozedUntil = ref(Number(localStorage.getItem(LS_DISK_DISMISSED) || 0) + 24 * 60 * 60_000)
const diskAlert = computed(() => diskCorrupt.value && Date.now() >= diskSnoozedUntil.value)
function dismissDiskAlert() {
  localStorage.setItem(LS_DISK_DISMISSED, String(Date.now()))
  diskSnoozedUntil.value = Date.now() + 24 * 60 * 60_000
}
const resSnoozedUntil = ref(0)
let resPoll: ReturnType<typeof setInterval> | null = null

async function refreshResources() {
  if (!isTauri) return
  try {
    const s = await getProcessResources()
    resAlerts.value = s.alerts || []
    resCensus.value = (s.census || []).map(([k, v]) => `${k}: ${v}`).join(' · ')
    diskCorrupt.value = !!s.diskCorrupt
    diskCorruptWhere.value = s.diskCorruptWhere || ''
  } catch {
    /* команда недоступна (старый бэкенд) — молча */
  }
}
const resAlert = computed(
  () => resAlerts.value.length > 0 && Date.now() >= resSnoozedUntil.value,
)
const resAlertText = computed(() => resAlerts.value.join(' · '))
function snoozeResAlert() {
  resSnoozedUntil.value = Date.now() + RES_SNOOZE_MS
}

// Renderer-level connectivity signal (instant on Wi-Fi/cable drop) — never blocks
// the UI: it's driven by the browser online/offline events, not a network call.
// PLUS: Rust-side `net-status` probe events (WebKitGTK after laptop sleep does
// NOT fire browser online/offline — the Rust TCP-probe is the reliable source).
const online = ref(typeof navigator !== 'undefined' ? navigator.onLine : true)
const justReconnected = ref(false)
// Cloud mode = SmartNet P2P offline, но у клиента есть рабочие Cloud Seeder-ы
// (Yandex/Mail.ru доступны). В этом режиме сеть жива через облачный fallback,
// поэтому статус-бар меняет цвет с красного «нет соединения» на матовый синий.
const cloudSeederOk = ref(false)
const router = useRouter()
let seederPoll: ReturnType<typeof setInterval> | null = null
let hintTicker: ReturnType<typeof setInterval> | null = null
let reconnectTimer: ReturnType<typeof setTimeout> | null = null
let unlistenNet: (() => void) | null = null

// Тайминги подсказки «стать сидером»: показываем, если клиент в Cloud mode
// непрерывно > 1 часа И у него ещё нет своего опубликованного сидера.
// После закрытия подсказки не показываем 7 дней (защита от навязчивости).
const HINT_MIN_MS = 60 * 60_000
const HINT_SNOOZE_MS = 7 * 24 * 60 * 60_000
const LS_HINT_DISMISSED = 'sth_cloud_hint_dismissed_at'

// Отметка времени, когда клиент зашёл в текущую непрерывную сессию Cloud mode.
// null = клиент сейчас не в Cloud mode.
const cloudModeSince = ref<number | null>(null)
// Тикер для реактивного вычисления «прошёл ли час» без polling-компонента.
const nowTs = ref(Date.now())

const emit = defineEmits<{ (e: 'reconnect'): void }>()

// Cloud mode is only interesting while we're "offline" — иначе показываем
// обычную зелёную «Соединение восстановлено» плашку.
const cloudMode = computed(() => !online.value && cloudSeederOk.value)

/** Переход из плашки Cloud mode на страницу Seeders с якорем #active-seeders,
 *  чтобы Seeders.vue проскроллил и подсветил живые сидеры, держащие сеть. */
function openSeederRegistry() {
  if (!cloudMode.value) return
  router.push({ name: 'seeders', hash: '#active-seeders' }).catch(() => {})
}

/** Показывать ли подсказку «Опубликуйте свой Cloud Seeder».
 *  Условия (все одновременно):
 *  - клиент прямо сейчас в Cloud mode > 1 часа непрерывно;
 *  - у пользователя ещё нет своего опубликованного сидера (ownSeeder() null);
 *  - подсказка не была свёрнута за последние 7 дней. */
const showBecomeSeederHint = computed(() => {
  if (!cloudMode.value || !cloudModeSince.value) return false
  if (nowTs.value - cloudModeSince.value < HINT_MIN_MS) return false
  if (ownSeeder()) return false
  const dismissedAt = Number(localStorage.getItem(LS_HINT_DISMISSED) || 0)
  if (dismissedAt && Date.now() - dismissedAt < HINT_SNOOZE_MS) return false
  return true
})

function dismissBecomeSeederHint(nav = false) {
  localStorage.setItem(LS_HINT_DISMISSED, String(Date.now()))
  // Реактивно скрываем подсказку немедленно, не дожидаясь следующего тика.
  nowTs.value = Date.now()
  if (nav) router.push({ name: 'seeders' }).catch(() => {})
}

// Отслеживаем непрерывную сессию Cloud mode: вход → фиксируем ts,
// выход (P2P вернулся ИЛИ облако отвалилось) → сбрасываем.
watch(cloudMode, (on) => {
  cloudModeSince.value = on ? Date.now() : null
}, { immediate: true })

async function refreshSeederStatus() {
  if (!isTauri) {
    cloudSeederOk.value = false
    return
  }
  try {
    const map = await cloudSeederStatusGet()
    // «есть доступ к cloud seeds» = хоть один сидер отвечал успешно
    // (ok=true сейчас ИЛИ был свежий lastOk за последние 15 минут).
    const now = Date.now()
    let live = false
    for (const s of Object.values(map)) {
      if (s.ok) { live = true; break }
      if (s.lastOk) {
        const t = Date.parse(s.lastOk)
        if (Number.isFinite(t) && now - t < 15 * 60_000) { live = true; break }
      }
    }
    cloudSeederOk.value = live
  } catch {
    cloudSeederOk.value = false
  }
}

function goOffline() {
  if (!online.value) return
  online.value = false
  justReconnected.value = false
  // Как только уходим в offline — сразу проверяем состояние облачных сидеров,
  // чтобы плашка мгновенно переключилась на «Cloud mode», если он активен.
  void refreshSeederStatus()
}
function goOnline() {
  if (online.value) return
  online.value = true
  justReconnected.value = true
  // Network came back: nudge the shell to re-run discovery (DHT/seeder paths may
  // have gone stale during the outage). App.vue debounces the actual scan().
  emit('reconnect')
  if (reconnectTimer) clearTimeout(reconnectTimer)
  reconnectTimer = setTimeout(() => (justReconnected.value = false), 2600)
}

onMounted(async () => {
  window.addEventListener('offline', goOffline)
  window.addEventListener('online', goOnline)
  if (isTauri) {
    // Rust-монитор сети: единственный сигнал, который срабатывает после сна.
    unlistenNet = await appListen<{ online: boolean }>('net-status', (p) => {
      if (p.online) goOnline()
      else goOffline()
    })
  }
  // Первичная и периодическая проверка облачных сидеров (раз в 20с —
  // достаточно быстро, чтобы поймать переход cloud→dead, и не создаёт нагрузку).
  void refreshSeederStatus()
  seederPoll = setInterval(refreshSeederStatus, 20_000)
  // Тикер для подсказки «стать сидером»: раз в минуту продвигаем nowTs, чтобы
  // computed showBecomeSeederHint переоценивался (порог = 1 час непрерывного
  // Cloud mode). Одна минута — достаточно, чтобы не спамить рендер.
  hintTicker = setInterval(() => (nowTs.value = Date.now()), 60_000)
  // Memory Guard: первый снимок через 2 мин (после прогрева), далее раз в 5 минут.
  setTimeout(refreshResources, 120_000)
  resPoll = setInterval(refreshResources, RES_POLL_MS)
  // Local Node Nudge ОТКЛЮЧЁН (SHIP-44): кошелёк работает через iroh-пул sth-core,
  // локальный legacy-узел на :4003 больше не предлагаем автоматически.
})
onBeforeUnmount(() => {
  window.removeEventListener('offline', goOffline)
  window.removeEventListener('online', goOnline)
  if (unlistenNet) unlistenNet()
  if (reconnectTimer) clearTimeout(reconnectTimer)
  if (seederPoll) clearInterval(seederPoll)
  if (hintTicker) clearInterval(hintTicker)
  if (resPoll) clearInterval(resPoll)
  if (localPoll) clearInterval(localPoll)
})
</script>

<template>
  <!-- Ненавязчивая подсказка «стать сидером»: всплывает над плашкой Cloud mode
       после 1 часа непрерывной работы через облачные архивы. Свернуть можно
       крестиком (не показываем 7 дней) или кликом по CTA (перейти к публикации). -->
  <transition name="hint-slide">
    <div
      v-if="showBecomeSeederHint"
      class="conn-hint"
      role="status"
      data-testid="become-seeder-hint"
    >
      <span class="hint-icon" aria-hidden="true">☁</span>
      <div class="hint-text">
        <b class="hint-title">Опубликуйте свой Cloud Seeder</b>
        <span class="hint-sub">Станьте частью fallback-инфраструктуры — сеть держится на таких пирах, как вы.</span>
      </div>
      <button
        class="hint-cta"
        @click="dismissBecomeSeederHint(true)"
        data-testid="become-seeder-hint-cta"
      >Опубликовать →</button>
      <button
        class="hint-close"
        aria-label="Скрыть подсказку на 7 дней"
        title="Скрыть на 7 дней"
        @click="dismissBecomeSeederHint(false)"
        data-testid="become-seeder-hint-dismiss"
      >×</button>
    </div>
  </transition>

  <transition name="conn-slide">
    <div
      v-if="!online || justReconnected"
      key="conn"
      class="conn-bar"
      :class="{ ok: online, cloud: cloudMode, clickable: cloudMode }"
      :role="cloudMode ? 'button' : undefined"
      :tabindex="cloudMode ? 0 : undefined"
      :title="cloudMode ? 'Открыть Cloud Seeders — посмотреть, кто держит сеть' : undefined"
      data-testid="connection-bar"
      @click="cloudMode && openSeederRegistry()"
      @keydown.enter="cloudMode && openSeederRegistry()"
      @keydown.space.prevent="cloudMode && openSeederRegistry()"
    >
      <span class="conn-left">
        <span class="conn-dot" />
        <span class="conn-sys">SmartNet // {{ online ? 'NET' : 'OFFLINE' }}</span>
      </span>
      <span class="conn-status" data-testid="connection-status">
        <template v-if="online">Соединение восстановлено</template>
        <template v-else-if="cloudMode">Cloud mode <span class="conn-arrow">→</span></template>
        <template v-else>Нет соединения</template>
      </span>
    </div>
    <!-- Disk Health: NTFS-ошибка 1392/1393/CRC — диск требует chkdsk. Выше Memory Guard. -->
    <div
      v-else-if="diskAlert"
      key="disk"
      class="conn-bar disk"
      data-testid="disk-health-bar"
    >
      <span class="conn-left">
        <span class="conn-dot" />
        <span class="conn-sys">SmartNet // DISK</span>
      </span>
      <span class="conn-status conn-actions" data-testid="disk-health-status">
        Диск требует проверки: файл или папка повреждены (NTFS 1392{{ diskCorruptWhere ? ', ' + diskCorruptWhere : '' }}) — выполните <b class="conn-cmd">chkdsk /f</b>
        <button class="conn-close" aria-label="Скрыть на 24 часа" title="Скрыть на 24 часа" @click.stop="dismissDiskAlert" data-testid="disk-health-dismiss">×</button>
      </span>
    </div>
    <!-- Memory Guard: память/хендлы процесса выше безопасного порога.
         Показываем только когда плашка соединения не занята. Клик — скрыть на 30 мин. -->
    <div
      v-else-if="resAlert"
      key="res"
      class="conn-bar res clickable"
      role="button"
      tabindex="0"
      :title="'Ресурсы процесса выше нормы — перезапустите клиент и откройте crash-лог (Настройки). Клик: скрыть на 30 минут\n' + resCensus"
      data-testid="resource-guard-bar"
      @click="snoozeResAlert"
      @keydown.enter="snoozeResAlert"
      @keydown.space.prevent="snoozeResAlert"
    >
      <span class="conn-left">
        <span class="conn-dot" />
        <span class="conn-sys">SmartNet // RESOURCES</span>
      </span>
      <span class="conn-status" data-testid="resource-guard-status">
        Память/хендлы выше нормы: {{ resAlertText }} — перезапустите клиент
        <span class="conn-x" aria-hidden="true">×</span>
      </span>
    </div>
    <!-- Local Node Nudge: SmartHoldem отвечает на localhost:4003, но не выбран. -->
    <div
      v-else-if="localNodeNudge"
      key="local"
      class="conn-bar local"
      data-testid="local-node-bar"
    >
      <span class="conn-left">
        <span class="conn-dot" />
        <span class="conn-sys">SmartNet // LOCAL NODE</span>
      </span>
      <span class="conn-status conn-actions" data-testid="local-node-bar-status">
        Найден локальный узел
        <span class="conn-host">{{ localNodeHost.replace('http://', '') }}</span>
        <span v-if="localNodeHeight" class="conn-host">#{{ localNodeHeight }}</span>
        <button
          class="conn-btn"
          :disabled="localNodeBusy"
          @click.stop="connectLocalNode"
          data-testid="local-node-bar-connect"
        >{{ localNodeBusy ? '…' : 'Подключить' }}</button>
        <button
          class="conn-close"
          aria-label="Скрыть на 6 часов"
          title="Скрыть на 6 часов"
          @click.stop="dismissLocalNode"
          data-testid="local-node-bar-dismiss"
        >×</button>
      </span>
    </div>
  </transition>
</template>

<style scoped>
.conn-bar {
  position: fixed;
  left: 0;
  right: 0;
  bottom: 0;
  height: 32px;
  z-index: 9999;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 16px;
  background: linear-gradient(180deg, #2a0d0d, #170707);
  border-top: 1px solid #7f1d1d;
  box-shadow: 0 -6px 18px rgba(0, 0, 0, 0.45);
  font-family: 'JetBrains Mono', monospace;
  user-select: none;
  pointer-events: none;
}
.conn-bar.ok {
  background: linear-gradient(180deg, #082019, #04140f);
  border-top-color: #1f5a52;
}
/* Cloud mode: матовый глубокий синий — P2P нет, но облачные сидеры живы. */
.conn-bar.cloud {
  background: linear-gradient(180deg, #142236, #0b1524);
  border-top-color: #2a4a75;
  box-shadow: 0 -6px 18px rgba(10, 20, 40, 0.55);
}
/* Кликабельный вариант: включаем pointer-events (по-умолчанию плашка их
 * блокирует, чтобы не мешать контенту), добавляем cursor и hover-подсветку. */
.conn-bar.clickable {
  pointer-events: auto;
  cursor: pointer;
  transition: background 0.2s ease, border-top-color 0.2s ease;
}
.conn-bar.cloud.clickable:hover {
  background: linear-gradient(180deg, #1a2c46, #0e1b2e);
  border-top-color: #3b6099;
}
.conn-bar.cloud.clickable:focus-visible {
  outline: 2px solid #60a5fa;
  outline-offset: -2px;
}
/* Disk Health: красная плашка — NTFS повреждение, нужен chkdsk. */
.conn-bar.disk {
  pointer-events: auto;
  background: linear-gradient(180deg, #2a0f12, #170608);
  border-top-color: #7f1d1d;
  box-shadow: 0 -6px 18px rgba(40, 5, 8, 0.55);
}
.conn-bar.disk .conn-dot {
  background: #f87171;
  box-shadow: 0 0 8px #f87171;
  animation: conn-pulse 1.4s ease-in-out infinite;
}
.conn-bar.disk .conn-status {
  color: #fca5a5;
}
.conn-cmd {
  font-family: 'JetBrains Mono', monospace;
  color: #fecaca;
  background: rgba(255, 255, 255, 0.06);
  padding: 1px 6px;
  border-radius: 4px;
}
/* Memory Guard: янтарная плашка — ресурсы процесса выше безопасного порога. */
.conn-bar.res {
  background: linear-gradient(180deg, #2b1f08, #170f04);
  border-top-color: #92400e;
  box-shadow: 0 -6px 18px rgba(40, 25, 5, 0.55);
}
.conn-bar.res.clickable:hover {
  background: linear-gradient(180deg, #382a0c, #1e1405);
  border-top-color: #b45309;
}
.conn-bar.res .conn-dot {
  background: #fbbf24;
  box-shadow: 0 0 8px #fbbf24;
  animation: conn-pulse 1.6s ease-in-out infinite;
}
.conn-bar.res .conn-status {
  color: #fcd34d;
}
.conn-x {
  margin-left: 10px;
  opacity: 0.55;
  font-size: 13px;
}
.conn-bar.res.clickable:hover .conn-x {
  opacity: 1;
}
/* Local Node Nudge: бирюзовая плашка с кнопкой «Подключить». */
.conn-bar.local {
  pointer-events: auto;
  background: linear-gradient(180deg, #082019, #04140f);
  border-top-color: #1f5a52;
  box-shadow: 0 -6px 18px rgba(4, 20, 15, 0.55);
}
.conn-bar.local .conn-dot {
  background: #34d399;
  box-shadow: 0 0 8px #34d399;
  animation: conn-pulse 1.8s ease-in-out infinite;
}
.conn-bar.local .conn-status {
  color: #6ee7b7;
}
.conn-actions {
  display: flex;
  align-items: center;
  gap: 10px;
}
.conn-host {
  font-weight: var(--fw-500, 500);
  color: #a7f3d0;
  opacity: 0.85;
}
.conn-btn {
  padding: 3px 11px;
  font: inherit;
  font-size: 10.5px;
  font-weight: var(--fw-700);
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: #041016;
  background: #34d399;
  border: none;
  border-radius: 5px;
  cursor: pointer;
  transition: filter 0.16s ease, transform 0.12s ease;
}
.conn-btn:hover:not(:disabled) {
  filter: brightness(1.1);
}
.conn-btn:active:not(:disabled) {
  transform: translateY(1px);
}
.conn-btn:disabled {
  opacity: 0.5;
  cursor: default;
}
.conn-close {
  width: 20px;
  height: 20px;
  padding: 0;
  font: inherit;
  font-size: 15px;
  line-height: 1;
  color: #64748b;
  background: transparent;
  border: none;
  border-radius: 4px;
  cursor: pointer;
  transition: color 0.16s ease, background 0.16s ease;
}
.conn-close:hover {
  color: #e2e8f0;
  background: rgba(255, 255, 255, 0.06);
}
.conn-arrow {
  margin-left: 4px;
  opacity: 0.65;
  transition: transform 0.2s ease, opacity 0.2s ease;
  display: inline-block;
}
.conn-bar.cloud.clickable:hover .conn-arrow {
  opacity: 1;
  transform: translateX(3px);
}
.conn-left {
  display: flex;
  align-items: center;
  gap: 9px;
}
.conn-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #ef4444;
  box-shadow: 0 0 8px #ef4444;
  animation: conn-pulse 1.3s ease-in-out infinite;
}
.conn-bar.ok .conn-dot {
  background: #34d399;
  box-shadow: 0 0 8px #34d399;
  animation: none;
}
.conn-bar.cloud .conn-dot {
  background: #60a5fa;
  box-shadow: 0 0 8px #60a5fa;
  animation: conn-pulse 1.8s ease-in-out infinite;
}
@keyframes conn-pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.35; }
}
.conn-sys {
  font-size: 10px;
  letter-spacing: 0.14em;
  color: #a1a1aa;
  text-transform: uppercase;
}
.conn-status {
  font-size: 11px;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
  color: #fca5a5;
}
.conn-bar.ok .conn-status {
  color: #6ee7b7;
}
.conn-bar.cloud .conn-status {
  color: #93c5fd;
  text-transform: uppercase;
}
.conn-slide-enter-active,
.conn-slide-leave-active {
  transition: transform 0.28s ease, opacity 0.28s ease;
}
.conn-slide-enter-from,
.conn-slide-leave-to {
  transform: translateY(100%);
  opacity: 0;
}

/* ── Подсказка «стать сидером» ─────────────────────────────────────────
 * Плавает над Cloud mode-плашкой, чуть смещена от правого края, чтобы не
 * закрывать основной контент. Матовое стекло + мягкое голубое свечение —
 * в том же ключе, что и Cloud mode-плашка. */
.conn-hint {
  position: fixed;
  right: 20px;
  bottom: 44px;
  z-index: 10000;
  display: flex;
  align-items: center;
  gap: 12px;
  max-width: 460px;
  padding: 10px 12px 10px 14px;
  background: linear-gradient(180deg, rgba(20, 34, 54, 0.94), rgba(11, 21, 36, 0.94));
  backdrop-filter: blur(14px);
  -webkit-backdrop-filter: blur(14px);
  border: 1px solid rgba(96, 165, 250, 0.4);
  border-radius: 10px;
  box-shadow: 0 8px 24px rgba(6, 12, 24, 0.5), 0 0 0 1px rgba(96, 165, 250, 0.15);
  font-family: 'JetBrains Mono', monospace;
  color: #cbd5e1;
  pointer-events: auto;
}
.hint-icon {
  font-size: 22px;
  line-height: 1;
  color: #60a5fa;
  filter: drop-shadow(0 0 6px rgba(96, 165, 250, 0.5));
  flex-shrink: 0;
}
.hint-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.hint-title {
  font-size: 12px;
  font-weight: var(--fw-700);
  color: #e0e7ff;
  letter-spacing: 0.04em;
}
.hint-sub {
  font-size: 10.5px;
  color: #94a3b8;
  line-height: 1.35;
}
.hint-cta {
  flex-shrink: 0;
  padding: 6px 12px;
  font: inherit;
  font-size: 11px;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
  color: #93c5fd;
  background: rgba(96, 165, 250, 0.14);
  border: 1px solid rgba(96, 165, 250, 0.55);
  border-radius: 6px;
  cursor: pointer;
  transition: background 0.18s ease, border-color 0.18s ease, transform 0.18s ease;
}
.hint-cta:hover {
  background: rgba(96, 165, 250, 0.25);
  border-color: rgba(96, 165, 250, 0.8);
  transform: translateX(2px);
}
.hint-close {
  flex-shrink: 0;
  width: 22px;
  height: 22px;
  padding: 0;
  font: inherit;
  font-size: 16px;
  line-height: 1;
  color: #64748b;
  background: transparent;
  border: none;
  border-radius: 4px;
  cursor: pointer;
  transition: color 0.18s ease, background 0.18s ease;
}
.hint-close:hover {
  color: #e2e8f0;
  background: rgba(255, 255, 255, 0.06);
}
.hint-slide-enter-active,
.hint-slide-leave-active {
  transition: transform 0.32s ease, opacity 0.32s ease;
}
.hint-slide-enter-from,
.hint-slide-leave-to {
  transform: translateY(12px);
  opacity: 0;
}
</style>
