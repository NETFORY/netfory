// ============================================================
//  SmartNet // updater.ts
//
//  Клиентский сервис P2P-обновлений: таймеры проверки, сканирование
//  блокчейна STH, фоновое торрент-скачивание, менеджмент строки состояния
//  (UpdateBar) и модального окна. Источник истины — блокчейн, транспорт —
//  торрент (см. src-tauri/src/updater.rs).
// ============================================================

import { reactive } from 'vue'
import {
  isTauri,
  checkForUpdate,
  startUpdateDownload,
  readUpdateManifest,
  resolveInstaller,
  resetUpdatePackage,
  cleanupUpdateDirs,
  runP2pInstaller,
  getCurrentOs,
  getUpdatePeriodHours,
  getActiveTorrents,
  getTorrentDhtNodes,
  type UpdateInfo,
  type UpdateManifest,
} from './bridge'
import { useNetStore } from '@/store/net'
import { i18n } from '@/i18n'

// Строки статуса локализованы (en/ru/id) — берём переводы вне компонента.
const t = (key: string, params?: Record<string, unknown>): string =>
  params ? i18n.global.t(key, params) : i18n.global.t(key)

const FIRST_CHECK_MS = 5 * 60 * 1000 // первая проверка — через 5 минут после старта
const PROGRESS_POLL_MS = 2000 // опрос прогресса торрента

export type UpdatePhase = 'idle' | 'checking' | 'downloading' | 'ready' | 'installing'

interface UpdaterState {
  phase: UpdatePhase
  progress: number // 0..100
  info: UpdateInfo | null
  manifest: UpdateManifest | null
  installerPath: string
  osType: string
  showModal: boolean
  error: string
  // Живой статус загрузки для строки состояния (UpdateBar):
  statusText: string // человекочитаемая фраза «что происходит сейчас»
  indeterminate: boolean // true, пока нет метаданных (шкала «бегущая», не %)
  peers: number
  seeders: number
  dhtNodes: number
  speedBytes: number
}

const state = reactive<UpdaterState>({
  phase: 'idle',
  progress: 0,
  info: null,
  manifest: null,
  installerPath: '',
  osType: '',
  showModal: false,
  error: '',
  statusText: '',
  indeterminate: true,
  peers: 0,
  seeders: 0,
  dhtNodes: 0,
  speedBytes: 0,
})

let firstTimer: ReturnType<typeof setTimeout> | null = null
let periodTimer: ReturnType<typeof setInterval> | null = null
let progressTimer: ReturnType<typeof setInterval> | null = null
let started = false
/** Версия, для которой уже делали авто-сброс битого пакета (один раз за сессию). */
let healedVersion = ''

async function runCheck(): Promise<void> {
  // Не мешаем активной загрузке/готовому обновлению.
  if (state.phase === 'downloading' || state.phase === 'ready' || state.phase === 'installing') {
    return
  }
  // offline: проверка обновления повиснет в таймаутах — ждём следующего периода
  if (!useNetStore().online) return
  state.phase = 'checking'
  state.error = ''
  try {
    const info = await checkForUpdate()
    if (info) {
      await beginDownload(info)
    } else {
      state.phase = 'idle'
    }
  } catch (e: any) {
    state.error = e?.message || String(e)
    state.phase = 'idle'
  }
}

async function beginDownload(info: UpdateInfo): Promise<void> {
  state.info = info
  state.progress = 0
  state.indeterminate = true
  state.peers = 0
  state.seeders = 0
  state.speedBytes = 0
  state.statusText = t('updater.starting')
  state.phase = 'downloading'
  try {
    const infoHash = await startUpdateDownload(info.version, info.hash)
    state.info = { ...info, infoHash }
    startProgressPolling(infoHash)
  } catch (e: any) {
    state.error = e?.message || String(e)
    state.phase = 'idle'
  }
}

function fmtSpeed(bytes: number): string {
  if (!bytes || bytes < 1024) return `${bytes || 0} ${t('updater.bps')}`
  const kb = bytes / 1024
  if (kb < 1024) return `${kb.toFixed(0)} ${t('updater.kibps')}`
  return `${(kb / 1024).toFixed(1)} ${t('updater.mibps')}`
}

// Сменяющиеся подсказки, пока ищем источник (метаданные ещё не пришли).
const SEARCH_HINT_KEYS = [
  'updater.connecting',
  'updater.searchDht',
  'updater.waitSeeds',
  'updater.reqMeta',
]

function startProgressPolling(infoHash: string): void {
  stopProgressPolling()
  let tick = 0
  state.indeterminate = true
  state.statusText = t('updater.connecting')
  progressTimer = setInterval(async () => {
    tick += 1
    try {
      const [list, dht] = await Promise.all([
        getActiveTorrents(),
        getTorrentDhtNodes().catch(() => 0),
      ])
      state.dhtNodes = typeof dht === 'number' ? dht : 0
      // ВАЖНО: не называть переменную `t` — затенит функцию перевода t()
      // (баг v1.135.33: TypeError глушился catch'ем, onDownloadComplete
      // не вызывался и модалка «готово к установке» никогда не показывалась).
      const tor = list.find((x) => x.infoHash === infoHash)
      if (!tor) {
        // Торрент ещё не зарегистрирован в сессии — только подключаемся.
        state.indeterminate = true
        state.statusText = `${t('updater.connecting')} (DHT: ${state.dhtNodes})`
        return
      }
      state.progress = Math.round((tor.progress || 0) * 100)
      state.peers = tor.peersConnected || 0
      state.seeders = tor.seedersConnected || 0
      state.speedBytes = tor.downloadSpeedBytes || 0

      const hasMeta = (tor.totalBytes || 0) > 0
      state.indeterminate = !hasMeta
      if (tor.state === 'Checking') {
        // рескан после перезапуска: progress = ПРОВЕРЕННЫЕ байты, не скачанные
        state.statusText = `${t('updater.checking')} ${state.progress}%`
      } else if (!hasMeta) {
        // Метаданные ещё не получены — крутим подсказки + живые счётчики.
        const hint = t(SEARCH_HINT_KEYS[Math.floor(tick / 2) % SEARCH_HINT_KEYS.length])
        state.statusText = `${hint} · DHT: ${state.dhtNodes} · ${t('updater.peersShort')}: ${state.peers}`
      } else {
        state.statusText =
          `${t('updater.downloading')} · ${t('updater.seedsShort')} ${state.seeders} / ${t('updater.peersShort')} ${state.peers}` +
          (state.speedBytes > 0 ? ` · ↓ ${fmtSpeed(state.speedBytes)}` : '')
      }

      // ГОТОВО только в состоянии Seeding (= Live + все куски прошли
      // hash-проверку). Раньше срабатывало и на `progress >= 1`, но в фазе
      // Checking (рескан после перезапуска) librqbit отдаёт progress_bytes =
      // ПРОВЕРЕННЫЕ байты — прогресс доезжал до 100% на недокачанном пакете,
      // и юзеру предлагался БИТЫЙ инсталлятор (v1.135.95, Win10).
      if (tor.state === 'Seeding') {
        stopProgressPolling()
        await onDownloadComplete()
      }
    } catch {
      /* временная ошибка опроса — продолжаем */
    }
  }, PROGRESS_POLL_MS)
}

function stopProgressPolling(): void {
  if (progressTimer) {
    clearInterval(progressTimer)
    progressTimer = null
  }
}

/** Битый пакет (пустой manifest / усечённый инсталлятор после рестарта во
 *  время записи): один раз за сессию сбрасываем раздачу+папку и качаем заново.
 *  Раньше такой пакет застревал навсегда — «Инсталлятор не найден» даже после
 *  перезапуска (v1.135.129, Win10). */
async function healCorruptPackage(info: UpdateInfo, reason: string): Promise<boolean> {
  if (healedVersion === info.version) return false
  healedVersion = info.version
  try {
    await resetUpdatePackage(info.version, info.hash)
  } catch {
    return false
  }
  state.error = ''
  state.statusText = t('updater.packageCorruptRetry')
  console.warn(`[updater] corrupt package v${info.version} reset — re-downloading (${reason})`)
  await beginDownload({ ...info, infoHash: '' })
  return true
}

function isCorruptError(msg: string): boolean {
  return /corrupt-package|manifest|не найден|not found|усеч/i.test(msg)
}

async function onDownloadComplete(): Promise<void> {
  const info = state.info
  if (!info) return
  state.progress = 100
  state.indeterminate = false
  state.statusText = t('updater.downloaded')
  let failure = ''
  try {
    // Раздача остаётся активной (сидируем обновление). Читаем manifest (для notes).
    state.manifest = await readUpdateManifest(info.version)
    // Зачистка: удаляем предыдущие версии, оставляем только новую.
    await cleanupUpdateDirs([info.version])
    // Резолвим путь к инсталлятору под текущую ОС (manifest + fallback по расширению).
    state.osType = await getCurrentOs()
    try {
      state.installerPath = await resolveInstaller(info.version, state.osType)
    } catch (e: any) {
      state.installerPath = ''
      failure = e?.message || String(e)
    }
  } catch (e: any) {
    failure = e?.message || String(e)
  }
  if (failure && isCorruptError(failure) && (await healCorruptPackage(info, failure))) {
    return
  }
  state.error = failure
  // Готово к установке: показываем модалку.
  state.phase = 'ready'
  state.showModal = true
}

/** «Позже»: закрыть модалку, оставить плашку с кнопкой «Обновить» в строке. */
function dismissModal(): void {
  state.showModal = false
}

/** Повторно открыть модалку (например, по клику «Обновить» в строке). */
function openModal(): void {
  if (state.phase === 'ready') state.showModal = true
}

/** «Обновить сейчас»: запустить инсталлятор и закрыть приложение. */
async function installNow(): Promise<void> {
  if (state.phase !== 'ready' || !state.installerPath) {
    state.error = t('updater.installerNotFound')
    return
  }
  state.phase = 'installing'
  state.showModal = false
  try {
    await runP2pInstaller(state.installerPath, state.osType)
    // Rust закрывает приложение (exit 0) после запуска инсталлятора.
  } catch (e: any) {
    const msg = e?.message || String(e)
    if (/corrupt-package/.test(msg) && state.info && (await healCorruptPackage(state.info, msg))) {
      return
    }
    state.error = msg
    state.phase = 'ready'
    state.showModal = true
  }
}

/** Кнопка «Скачать пакет заново»: сброс раздачи + папки версии и загрузка с нуля
 *  (без лимита «один раз за сессию», как у авто-лечения). */
async function redownload(): Promise<void> {
  const info = state.info
  if (!info || state.phase === 'installing' || state.phase === 'downloading') return
  stopProgressPolling()
  state.showModal = false
  state.manifest = null
  state.installerPath = ''
  state.error = ''
  state.statusText = t('updater.redownloading')
  try {
    await resetUpdatePackage(info.version, info.hash)
  } catch (e: any) {
    state.error = e?.message || String(e)
    state.phase = 'ready'
    state.showModal = true
    return
  }
  console.warn(`[updater] manual reset of package v${info.version} — re-downloading`)
  await beginDownload({ ...info, infoHash: '' })
}

/** Запуск сервиса: первая проверка через 5 мин, далее по периоду из .env. */
async function start(): Promise<void> {
  if (started || !isTauri) return
  started = true
  const hours = await getUpdatePeriodHours()
  const periodMs = Math.max(1, hours) * 3600 * 1000
  firstTimer = setTimeout(() => {
    runCheck()
    periodTimer = setInterval(runCheck, periodMs)
  }, FIRST_CHECK_MS)
}

function stop(): void {
  if (firstTimer) clearTimeout(firstTimer)
  if (periodTimer) clearInterval(periodTimer)
  stopProgressPolling()
  firstTimer = null
  periodTimer = null
  started = false
}

export function useUpdater() {
  return {
    state,
    start,
    stop,
    checkNow: runCheck,
    installNow,
    redownload,
    dismissModal,
    openModal,
  }
}
