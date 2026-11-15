<template>
  <div class="h-screen w-full flex flex-col overflow-hidden" data-testid="mailbox-root">
    <!-- Top bar -->
    <header class="hairline border-b flex items-center gap-3 px-3 sm:px-5 py-2.5 panel rounded-none">
      <button class="lg:hidden btn-ghost !px-2" @click="mobilePane = 'sidebar'" data-testid="open-sidebar-btn">
        <Menu class="w-5 h-5" />
      </button>
      <div class="flex items-center gap-2.5 min-w-0">
        <div class="w-7 h-7 rounded bg-rust grid place-items-center shrink-0">
          <Flame class="w-4 h-4 text-white" :stroke-width="2.2" />
        </div>
        <div class="hidden sm:block min-w-0">
          <div class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200 leading-none mb-0.5">OXID // INBOX</div>
          <div class="text-sm font-semibold leading-none truncate">{{ account.currentAccount?.email || '—' }}</div>
        </div>
      </div>

      <div class="flex-1 max-w-xl mx-auto hidden md:flex items-center gap-2 panel rounded-md px-3 py-1.5">
        <Search class="w-4 h-4 text-iron-200" />
        <input
          v-model="searchInput"
          @input="mail.setSearch(searchInput)"
          placeholder="Search subjects, senders, snippets…"
          class="bg-transparent flex-1 outline-none text-sm font-mono placeholder:text-iron-300/60"
          data-testid="mailbox-search"
        />
        <kbd class="text-[10px] font-mono text-iron-200 hidden lg:inline">⌘K</kbd>
      </div>

      <div class="ml-auto flex items-center gap-1">
        <button class="btn-ghost !px-2" @click="showAddContact = true" title="Add contact" data-testid="add-contact-btn">
          <UserPlus class="w-4 h-4" />
        </button>
        <button class="btn-ghost !px-2" @click="showShareContact = true" title="Share my contact" data-testid="share-contact-btn">
          <Share2 class="w-4 h-4" />
        </button>
        <button class="btn-ghost !px-2" @click="mail.refresh()" :title="mail.syncing ? 'Syncing…' : 'Sync mail'" data-testid="sync-btn">
          <RefreshCw class="w-4 h-4" :class="mail.syncing ? 'animate-spin text-rust' : ''" />
        </button>
        <button class="btn-ghost !px-2" @click="theme.toggle" :title="theme.mode" data-testid="theme-toggle-mailbox">
          <Moon v-if="theme.mode === 'dark'" class="w-4 h-4" />
          <Sun v-else class="w-4 h-4" />
        </button>
        <router-link to="/settings" class="btn-ghost !px-2" data-testid="open-settings-link">
          <Settings class="w-4 h-4" />
        </router-link>
        <button class="btn-rust !py-1.5 !px-3" @click="showCompose = true" data-testid="compose-btn">
          <Pencil class="w-4 h-4" /> <span class="hidden sm:inline">Compose</span>
        </button>
      </div>
    </header>

    <div v-if="mail.error" class="hairline border-b px-4 py-1.5 text-xs font-mono bg-rust/10 text-rust flex items-center gap-2" data-testid="mail-error">
      <span class="truncate">{{ mail.error }}</span>
      <button class="ml-auto underline shrink-0" @click="mail.error = ''" data-testid="mail-error-dismiss">dismiss</button>
    </div>

    <!-- PIN Unlock overlay: seed запечатан на диске, ждём PIN -->
    <div v-if="account.locked" class="fixed inset-0 z-50 grid place-items-center bg-black/70 backdrop-blur-sm" data-testid="unlock-overlay">
      <div class="panel rounded-lg p-8 w-[340px] border border-rust/30">
        <h2 class="font-mono text-xs uppercase tracking-[0.25em] text-rust mb-1">// vault locked</h2>
        <p class="text-sm text-iron-100 mb-4">Enter PIN to unseal your keys</p>
        <input
          v-model="unlockPin"
          type="password"
          inputmode="numeric"
          placeholder="••••••"
          class="input-base text-center tracking-[0.4em] mb-3"
          @keyup.enter="doUnlock"
          data-testid="unlock-pin-input"
        />
        <p v-if="unlockError" class="text-xs font-mono text-rust mb-3" data-testid="unlock-error">{{ unlockError }}</p>
        <button class="btn-rust w-full justify-center" :disabled="unlocking || unlockPin.length < 4" @click="doUnlock" data-testid="unlock-btn">
          {{ unlocking ? 'Unsealing…' : 'Unlock' }}
        </button>
      </div>
    </div>

    <!-- Three panes -->
    <div class="flex-1 min-h-0 grid grid-cols-1 lg:grid-cols-[240px_360px_1fr]">
      <!-- Sidebar -->
      <Sidebar
        v-show="!isMobile || mobilePane === 'sidebar'"
        class="lg:block"
        :class="isMobile ? 'absolute inset-0 z-30 panel rounded-none' : ''"
        @navigate="onSidebarNavigate"
      />

      <!-- Middle list -->
      <transition name="pane" mode="out-in">
        <MessageList
          v-if="!isMobile || mobilePane === 'list'"
          :key="mail.activeFolder"
          @select="onSelectMessage"
        />
      </transition>

      <!-- Reader -->
      <transition name="pane" mode="out-in">
        <ReaderView
          v-if="!isMobile || mobilePane === 'reader'"
          :key="mail.selectedId || 'empty'"
          @back="mobilePane = 'list'"
          @reply="onReply"
          @forward="onForward"
        />
      </transition>
    </div>

    <!-- Compose modal -->
    <ComposeModal
      v-if="showCompose"
      :initial="composeInitial"
      @close="closeCompose"
    />

    <!-- Контакты: добавление и «поделиться» -->
    <AddContactModal v-if="showAddContact" @close="showAddContact = false" />
    <ShareContactModal v-if="showShareContact" @close="showShareContact = false" />
  </div>
</template>

<script setup>
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import { useRoute } from 'vue-router'
import { Menu, Flame, Search, Moon, Sun, Settings, Pencil, RefreshCw, UserPlus, Share2 } from 'lucide-vue-next'
import Sidebar from '../components/Sidebar.vue'
import MessageList from '../components/MessageList.vue'
import ReaderView from '../components/ReaderView.vue'
import ComposeModal from '../components/ComposeModal.vue'
import AddContactModal from '../components/AddContactModal.vue'
import ShareContactModal from '../components/ShareContactModal.vue'
import { useMailStore } from '../stores/mail'
import { useAccountStore } from '../stores/account'
import { useThemeStore } from '../stores/theme'

const mail = useMailStore()
const account = useAccountStore()
const theme = useThemeStore()
const route = useRoute()

// when active account changes, clear the selected message (it may no longer be visible)
watch(() => account.activeAccountId, () => {
  mail.clearSelection()
})

mail.hydrate()
account.hydrate()

if (route.params.folder) mail.setFolder(route.params.folder)

const searchInput = ref(mail.search)
const showCompose = ref(false)
const showAddContact = ref(false)
const showShareContact = ref(false)
const composeInitial = ref(null)
const mobilePane = ref('list') // 'sidebar' | 'list' | 'reader'

const windowWidth = ref(window.innerWidth)
const isMobile = computed(() => windowWidth.value < 1024)

const onResize = () => {
  windowWidth.value = window.innerWidth
}
onMounted(async () => {
  window.addEventListener('resize', onResize)
  if (!account.locked) {
    await boot()
  }
  // Автопроверка: лёгкий IDLE-опрос каждые 20с + полный sync каждые 2 мин
  // без document.hidden: уведомления должны приходить и в свёрнутом окне
  updatesTimer = window.setInterval(() => {
    if (!account.locked) {
      mail.checkUpdates()
      mail.checkP2P()
    }
  }, 20000)
  pollTimer = window.setInterval(() => {
    if (!document.hidden && !account.locked) mail.refresh()
  }, 120000)
})

async function boot() {
  try {
    await account.openSession()
    await mail.loadAll()
    mail.refresh() // фоновая IMAP-синхронизация, без блокировки UI
  } catch (e) {
    mail.error = e.message
  }
}

// PIN unlock
const unlockPin = ref('')
const unlockError = ref('')
const unlocking = ref(false)
async function doUnlock() {
  if (unlockPin.value.length < 4) return
  unlocking.value = true
  unlockError.value = ''
  try {
    await account.unlock(unlockPin.value)
    unlockPin.value = ''
    await mail.loadAll()
    mail.refresh()
  } catch (e) {
    unlockError.value = e.message
  } finally {
    unlocking.value = false
  }
}
onUnmounted(() => {
  window.removeEventListener('resize', onResize)
  if (pollTimer) window.clearInterval(pollTimer)
  if (updatesTimer) window.clearInterval(updatesTimer)
  document.title = 'Oxid Mail'
})

// Счётчик непрочитанных: заголовок окна + отчёт в SmartNet по IPC (бейдж Mail)
let pollTimer = null
let updatesTimer = null
watch(
  () => mail.emails.filter((e) => e.folder === 'inbox' && !e.read).length,
  (n) => {
    document.title = n > 0 ? `(${n}) Oxid Mail` : 'Oxid Mail'
    const core = typeof window !== 'undefined' ? window.__TAURI__?.core : null
    core?.invoke?.('report_unread', { count: n }).catch(() => {})
  },
  { immediate: true }
)

function onSidebarNavigate() {
  mobilePane.value = 'list'
}
function onSelectMessage() {
  mobilePane.value = 'reader'
}

function quoteBody(email, isForward = false) {
  const date = new Date(email.date).toLocaleString()
  // Для NTFRY-писем цитируем РАСШИФРОВАННЫЙ текст (и настоящую тему), а не
  // armored-контейнер: ответ всё равно шифруется целиком при отправке.
  const dec = email.encrypted ? mail.decryptedView(email.id) : null
  // Ещё не расшифровано (Reply до открытия) — не цитируем armored-мусор.
  const body = (email.encrypted ? dec?.text : email.body) || ''
  const subject = dec?.subject || email.subject
  if (isForward) {
    return (
      '\n\n---------- Forwarded message ----------\n' +
      `From: ${email.from.name} <${email.from.email}>\n` +
      `Date: ${date}\n` +
      `Subject: ${subject}\n` +
      `To: ${email.to}\n\n` +
      body
    )
  }
  const quoted = body.split('\n').map((l) => `> ${l}`).join('\n')
  return `\n\nOn ${date}, ${email.from.name} <${email.from.email}> wrote:\n${quoted}\n`
}

// Настоящая тема письма: для расшифрованных NTFRY — из контейнера.
function realSubject(email) {
  const dec = email.encrypted ? mail.decryptedView(email.id) : null
  return dec?.subject || email.subject
}

function onReply() {
  const e = mail.selected
  if (!e) return
  const subj = realSubject(e)
  composeInitial.value = {
    mode: 'reply',
    to: e.from.email,
    subject: /^re:/i.test(subj) ? subj : `Re: ${subj}`,
    body: quoteBody(e, false),
    encrypted: !!e.encrypted,
    inReplyTo: e.id,
  }
  showCompose.value = true
}

function onForward() {
  const e = mail.selected
  if (!e) return
  const subj = realSubject(e)
  composeInitial.value = {
    mode: 'forward',
    to: '',
    subject: /^fwd:/i.test(subj) ? subj : `Fwd: ${subj}`,
    body: quoteBody(e, true),
    encrypted: !!e.encrypted,
    forwardOf: e.id,
  }
  showCompose.value = true
}

function closeCompose() {
  showCompose.value = false
  composeInitial.value = null
}
</script>
