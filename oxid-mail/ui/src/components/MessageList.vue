<template>
  <section class="h-full flex flex-col panel rounded-none border-r hairline overflow-hidden" data-testid="message-list">
    <div class="px-4 py-3 hairline border-b flex items-center justify-between">
      <div>
        <div class="font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200">
          // {{ mail.activeLabel ? `label · ${mail.activeLabel}` : mail.activeFolder }}
        </div>
        <div class="text-lg font-semibold capitalize">
          {{ mail.activeLabel || mail.activeFolder }}
        </div>
      </div>
      <div class="font-mono text-[10px] text-iron-200" data-testid="message-count">
        {{ mail.visibleEmails.length }} items
      </div>
    </div>

    <div class="md:hidden px-4 py-2 hairline border-b">
      <div class="flex items-center gap-2 panel rounded-md px-3 py-1.5">
        <Search class="w-4 h-4 text-iron-200" />
        <input
          v-model="localSearch"
          @input="mail.setSearch(localSearch)"
          placeholder="Search…"
          class="bg-transparent flex-1 outline-none text-sm font-mono placeholder:text-iron-300/60"
          data-testid="mobile-search"
        />
      </div>
    </div>

    <ul class="flex-1 overflow-y-auto divide-y divide-white/[0.04]">
      <li
        v-if="mail.visibleEmails.length === 0"
        class="p-8 text-center text-iron-200 text-sm font-mono"
        data-testid="empty-folder"
      >
        {{ mail.loading || mail.syncing ? '// syncing…' : `// No messages ${mail.activeLabel ? `tagged "${mail.activeLabel}"` : `in ${mail.activeFolder}`}` }}
      </li>
      <li
        v-for="email in mail.visibleEmails"
        :key="email.id"
        @click="onSelect(email.id)"
        class="px-4 py-3 cursor-pointer transition group relative"
        :class="[
          email.id === mail.selectedId
            ? 'bg-rust/[0.08] border-l-2 border-rust pl-[14px]'
            : 'hover:bg-white/[0.03] border-l-2 border-transparent pl-[14px]',
          !email.read ? 'bg-white/[0.015]' : '',
        ]"
        :data-testid="`message-item-${email.id}`"
      >
        <div class="flex items-start gap-3">
          <div class="pt-0.5">
            <component
              :is="email.encrypted ? (mail.isDecrypted(email.id) ? LockOpen : Lock) : Mail"
              class="w-4 h-4"
              :class="email.encrypted ? 'text-copper' : 'text-iron-200'"
            />
          </div>
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-2 mb-0.5">
              <span class="text-sm truncate" :class="!email.read ? 'font-semibold text-iron-50' : 'text-iron-100'">
                {{ email.from.name }}
              </span>
              <span v-if="email.encrypted" class="chip border-copper/40 text-copper">ENC</span>
              <span
                v-if="email.via"
                class="shrink-0"
                :title="email.via === 'p2p' ? 'Доставлено напрямую по P2P (Iroh)' : 'Отправлено через SMTP'"
                :data-testid="`via-${email.via}-${email.id}`"
              >
                <Zap v-if="email.via === 'p2p'" class="w-3.5 h-3.5 text-emerald-400" />
                <Mail v-else class="w-3.5 h-3.5 text-sky-400" />
              </span>
              <span v-if="email.starred" class="ml-auto">
                <Star class="w-3.5 h-3.5 text-rust fill-rust" />
              </span>
              <span class="font-mono text-[10px] text-iron-200 ml-auto shrink-0" v-if="!email.starred">
                {{ formatDate(email.date) }}
              </span>
              <span class="font-mono text-[10px] text-iron-200 shrink-0" v-else>
                {{ formatDate(email.date) }}
              </span>
            </div>
            <div class="text-sm mb-0.5 truncate" :class="!email.read ? 'font-semibold text-iron-50' : 'text-iron-100'">
              {{ email.subject }}
            </div>
            <div class="text-xs text-iron-200 truncate">
              {{ email.encrypted && !mail.isDecrypted(email.id) ? '◆ Encrypted payload · click to decrypt' : email.snippet }}
            </div>
          </div>
          <span
            v-if="!email.read"
            class="w-1.5 h-1.5 rounded-full bg-rust mt-1.5 shrink-0"
          />
        </div>
      </li>
      <li v-if="mail.visibleEmails.length && (mail.activeFolder === 'inbox' || mail.activeFolder === 'sent') && !mail.activeLabel" class="px-4 py-3">
        <button
          class="w-full text-center font-mono text-[11px] uppercase tracking-[0.18em] text-iron-200 hover:text-rust transition-colors"
          :disabled="mail.loadingOlder"
          @click="mail.loadOlder()"
          data-testid="load-older-btn"
        >
          {{ mail.loadingOlder ? '// loading older…' : '// load older messages' }}
        </button>
      </li>
    </ul>
  </section>
</template>

<script setup>
import { ref } from 'vue'
import { Lock, LockOpen, Mail, Star, Search, Zap } from 'lucide-vue-next'
import { useMailStore } from '../stores/mail'

const emit = defineEmits(['select'])
const mail = useMailStore()
const localSearch = ref(mail.search)

function onSelect(id) {
  mail.select(id)
  emit('select')
}

function formatDate(iso) {
  const d = new Date(iso)
  const now = new Date()
  const sameDay = d.toDateString() === now.toDateString()
  if (sameDay) {
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  }
  const sameYear = d.getFullYear() === now.getFullYear()
  return d.toLocaleDateString([], { month: 'short', day: 'numeric', year: sameYear ? undefined : '2-digit' })
}
</script>
