<script setup lang="ts">
// Оболочка NETFORY Messenger: titlebar (сеть · режим · непрочитанные), экран входа
// (standalone) и сам ChatWidget из общих исходников Netfory.
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import ChatWidget from '@/views/ChatWidget.vue'
import QrScanner from '~msg/QrScanner.vue'
import { decodeSeedQr, isSeedQr, seedQrName } from '@/lib/seedqr'
import { notifySystem } from '@/lib/bridge'

interface MsgStatus { mode: 'sidecar' | 'standalone'; unlocked: boolean; address: string; endpointId: string; version: string; dataDir: string; error?: string | null }

const st = ref<MsgStatus | null>(null)
const seed = ref('')
const pin = ref('')
const busy = ref(false)
const err = ref('')
const unread = ref(0)
const showMigrate = ref(false)
const migrateMsg = ref('')
const migrateBusy = ref(false)
const showScanner = ref(false)
const pinInput = ref<HTMLInputElement | null>(null)
// сайдкар: ключ от Netfory не пришёл/не принят → показать ошибку и дать ввести сид вручную
const hostErr = ref('')
const waitedLong = ref(false)
const manual = ref(false)
let pollTimer: ReturnType<typeof setInterval> | null = null
let waitTimer: ReturnType<typeof setTimeout> | null = null
// кнопка «Сканировать» только там, где есть камера-API (Tauri WebView на десктопе/Android)
const canScan = typeof navigator !== 'undefined' && !!navigator.mediaDevices?.getUserMedia
let uns: UnlistenFn[] = []

// пока сессия не открыта — опрашиваем статус (закрывает гонку «событие пришло до подписки»)
watch(() => st.value?.unlocked, (u) => {
  if (u) {
    if (pollTimer) clearInterval(pollTimer)
    if (waitTimer) clearTimeout(waitTimer)
    pollTimer = null
    waitTimer = null
    hostErr.value = ''
  } else if (!pollTimer) {
    pollTimer = setInterval(refresh, 2000)
  }
}, { immediate: true })

function onQrDecoded(text: string) {
  seed.value = text
  err.value = ''
  showScanner.value = false
  void nextTick(() => pinInput.value?.focus())
}

async function refresh() {
  try {
    st.value = await invoke<MsgStatus>('msg_status')
    // ошибка handshake могла случиться до загрузки UI — забираем её из статуса
    if (st.value?.error && !st.value.unlocked) hostErr.value = st.value.error
  } catch (e) { console.error(e) }
}

async function unlock() {
  err.value = ''
  hostErr.value = ''
  busy.value = true
  try {
    // QR из Netfory (ntfry://seed/v1?…) расшифровывается PIN'ом локально, сид на диск не попадает
    let value = seed.value.trim()
    const qrName = seedQrName(value)
    if (isSeedQr(value)) {
      if (pin.value.trim().length < 4) throw 'Введите PIN, показанный рядом с QR в Netfory'
      value = await decodeSeedQr(value, pin.value).catch(() => { throw 'Неверный PIN или повреждённый QR' })
    }
    await invoke('msg_unlock', { seed: value })
    // имя из QR — чтобы standalone сразу знал u://имя (в сайдкаре имя даёт Netfory)
    if (qrName) await invoke('msg_set_my_name', { name: qrName }).catch(() => {})
    seed.value = ''
    pin.value = ''
    await refresh()
  } catch (e: any) {
    err.value = typeof e === 'string' ? e : e?.message || String(e)
  } finally {
    busy.value = false
  }
}

/** Миграция из Netfory: архив приходит событием `msg-archive` (сайдкар), импортируем в свою базу. */
async function importArchive(json: string) {
  migrateBusy.value = true
  try {
    const r = await invoke<{ imported: number }>('msg_import_archive', { json })
    migrateMsg.value = `Перенесено записей: ${r.imported}. Перезапустите мессенджер, чтобы увидеть историю.`
  } catch (e: any) {
    migrateMsg.value = 'Ошибка переноса: ' + (typeof e === 'string' ? e : e?.message || e)
  } finally {
    migrateBusy.value = false
  }
}

const win = getCurrentWindow()
const minimize = () => win.minimize()
const toggleMax = () => win.toggleMaximize()
const close = () => win.close()

onMounted(async () => {
  await refresh()
  uns.push(await listen('msg-unlocked', refresh))
  uns.push(await listen('msg-locked', refresh))
  uns.push(await listen('msg-mode', refresh))
  uns.push(await listen<{ total: number }>('chat-unread', (e) => { unread.value = e.payload?.total ?? 0 }))
  uns.push(await listen<string>('msg-archive', (e) => { void importArchive(e.payload) }))
  uns.push(await listen<{ message: string }>('msg-error', (e) => { hostErr.value = e.payload?.message || 'неизвестная ошибка' }))
  // v1.135.235: standalone (в т.ч. Android) — системные уведомления о сообщениях и звонках,
  // когда окно не в фокусе / приложение свёрнуто. В сайдкар-режиме их показывает Netfory.
  uns.push(await listen<{ sender?: string; peerName?: string | null; text?: string; notify?: boolean }>('chat-incoming', (e) => {
    const p = e.payload || {}
    if (st.value?.mode !== 'standalone' || !p.notify || document.hasFocus()) return
    const who = p.peerName ? '@' + p.peerName : (p.sender || '').slice(0, 10)
    void notifySystem('NETFORY Messenger', `${who}: ${p.text || ''}`.slice(0, 180))
  }))
  uns.push(await listen<{ peerName?: string | null; peerAddress?: string }>('voice-incoming', (e) => {
    const p = e.payload || {}
    if (st.value?.mode !== 'standalone' || document.hasFocus()) return
    const who = p.peerName ? '@' + p.peerName : (p.peerAddress || '').slice(0, 10)
    void notifySystem('☎ Входящий звонок', who)
  }))
  // 20 с без ключа от Netfory — показываем подсказку и ручной ввод
  waitTimer = setTimeout(() => { waitedLong.value = true }, 20_000)
  // предложение переноса — один раз, только в сайдкар-режиме
  try {
    const done = await invoke<boolean>('msg_migration_done')
    if (!done) showMigrate.value = true
  } catch { /* ignore */ }
})
onBeforeUnmount(() => {
  uns.forEach((u) => u())
  uns = []
  if (pollTimer) clearInterval(pollTimer)
  if (waitTimer) clearTimeout(waitTimer)
})

function requestMigration() {
  // Netfory пришлёт архив по IPC → событие msg-archive
  invoke('msg_open_in_host', { url: 'netfory://messenger/export' }).catch(() => {
    migrateMsg.value = 'Перенос доступен только при запуске из Netfory.'
  })
  showMigrate.value = false
}
</script>

<template>
  <div class="msg-shell" data-testid="messenger-shell">
    <!-- после входа заголовком окна служит шапка ChatWidget (режим киоска), свой хедер не дублируем -->
    <header v-if="!st?.unlocked" class="msg-titlebar" data-tauri-drag-region>
      <span class="ttl">NETFORY // P2P MESSENGER</span>
      <span class="st" :class="{ on: !!st?.endpointId }" :title="st?.endpointId || ''" data-testid="messenger-net-status">
        <i class="dot" />{{ st?.endpointId ? 'ONLINE' : 'OFFLINE' }}
      </span>
      <span class="st" data-testid="messenger-mode">{{ st?.mode === 'sidecar' ? 'SIDECAR · NETFORY' : 'STANDALONE' }}</span>
      <span v-if="unread" class="st" data-testid="messenger-unread">✉ {{ unread }}</span>
      <span class="sp" />
      <span class="st">v{{ st?.version || '…' }}</span>
      <button @click="minimize" title="Свернуть">—</button>
      <button @click="toggleMax" title="Развернуть">▢</button>
      <button class="x" @click="close" title="Закрыть">✕</button>
    </header>

    <main class="msg-body">
      <ChatWidget v-if="st?.unlocked" />
      <section v-else class="msg-login" data-testid="messenger-login">
        <h1>ВХОД В МЕССЕНДЖЕР</h1>
        <template v-if="st?.mode === 'sidecar' && !manual">
          <p v-if="!hostErr && !waitedLong" data-testid="messenger-waiting-host"><i class="spin" />Ожидаем ключ от Netfory…</p>
          <template v-else>
            <p v-if="hostErr" class="err" data-testid="messenger-host-error">Не удалось открыть сессию: {{ hostErr }}</p>
            <p v-else class="err" data-testid="messenger-host-timeout">Netfory не передал ключ за 20 с. Подробности — в Netfory → системный журнал (строки <code>msg:app</code>) и в файле <code>{{ st?.dataDir }}/messenger.log</code>.</p>
            <div class="row">
              <button @click="manual = true" data-testid="messenger-manual-btn">ВВЕСТИ СИД ВРУЧНУЮ</button>
              <button class="ghost" @click="hostErr = ''; waitedLong = false; refresh(); waitTimer = setTimeout(() => { waitedLong = true }, 20_000)" data-testid="messenger-retry-wait-btn">ЖДАТЬ ЕЩЁ</button>
            </div>
          </template>
        </template>
        <template v-else>
          <p v-if="hostErr && !err" class="err" data-testid="messenger-host-error">{{ hostErr }}</p>
          <p>Введите сид-фразу вашего аккаунта SmartHoldem. Она остаётся только в памяти этого процесса и выводит те же чат-ключи, что и в Netfory — ваши контакты и комнаты сохранятся.</p>
          <QrScanner v-if="showScanner" @decoded="onQrDecoded" @close="showScanner = false" />
          <template v-else>
            <textarea v-model="seed" placeholder="Секрет Netfory (сид-фраза или короткий секрет)… или содержимое QR из Netfory (ntfry://seed/v1?…)" data-testid="messenger-seed-input" />
            <div v-if="isSeedQr(seed)" class="row">
              <input ref="pinInput" v-model="pin" class="pin" inputmode="numeric" maxlength="6" placeholder="PIN из Netfory" data-testid="messenger-pin-input" @keyup.enter="unlock" />
              <span class="mode">QR зашифрован PIN'ом — он показан рядом с QR в Netfory → Ключи</span>
            </div>
            <div class="row">
              <button :disabled="busy || seed.trim().length < 4" @click="unlock" data-testid="messenger-unlock-btn">{{ busy ? 'ПОДКЛЮЧАЕМСЯ…' : 'ВОЙТИ' }}</button>
              <button v-if="canScan" class="ghost" :disabled="busy" @click="showScanner = true" data-testid="messenger-scan-qr-btn">СКАНИРОВАТЬ QR</button>
              <span class="mode">STANDALONE · QR: Netfory → Ключи → «QR для мессенджера»</span>
            </div>
            <p v-if="err" class="err" data-testid="messenger-login-error">{{ err }}</p>
          </template>
        </template>
      </section>
    </main>

    <div v-if="st?.unlocked && st?.mode === 'sidecar' && showMigrate" class="msg-migrate" data-testid="messenger-migrate">
      Перенести историю, контакты и настройки чата из Netfory в мессенджер? Делается один раз; Netfory оставит свою копию как архив.
      <div>
        <button :disabled="migrateBusy" @click="requestMigration" data-testid="messenger-migrate-btn">Перенести</button>
        <button class="ghost" @click="showMigrate = false">Позже</button>
      </div>
    </div>
    <div v-if="migrateMsg" class="msg-migrate" data-testid="messenger-migrate-result">{{ migrateMsg }} <button class="ghost" @click="migrateMsg = ''">OK</button></div>
  </div>
</template>
