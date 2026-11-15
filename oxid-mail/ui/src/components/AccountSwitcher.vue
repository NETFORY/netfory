<template>
  <div class="relative" :data-testid="testid || 'account-switcher'">
    <!-- Trigger -->
    <button
      ref="triggerRef"
      type="button"
      class="w-full text-left flex items-center gap-3 group focus:outline-none"
      :class="compact ? 'panel rounded-md px-2.5 py-1.5 hover:border-white/20 transition-colors' : ''"
      @click="toggle"
      :data-testid="`${testid || 'switcher'}-trigger`"
    >
      <div
        class="rounded-md bg-rust grid place-items-center shrink-0"
        :class="compact ? 'w-7 h-7' : 'w-9 h-9'"
      >
        <span
          class="font-mono font-bold text-white"
          :class="compact ? 'text-[11px]' : 'text-sm'"
        >{{ initials }}</span>
      </div>
      <div v-if="!compact" class="min-w-0 flex-1">
        <div class="text-sm font-semibold truncate">{{ currentEmail }}</div>
        <div class="font-mono text-[10px] uppercase tracking-[0.18em] text-iron-200 flex items-center gap-1">
          <span class="w-1.5 h-1.5 rounded-full bg-copper" /> {{ providerName }}
        </div>
      </div>
      <div v-else class="min-w-0 hidden md:block">
        <div class="text-xs font-semibold truncate max-w-[160px]">{{ currentEmail }}</div>
      </div>
      <ChevronsUpDown class="w-4 h-4 text-iron-200 group-hover:text-iron-50 shrink-0" />
    </button>

    <!-- Dropdown -->
    <Teleport to="body">
      <transition name="fade">
        <div
          v-if="open"
          class="fixed z-[100] panel rounded-lg shadow-2xl overflow-hidden"
          :style="dropdownStyle"
          :data-testid="`${testid || 'switcher'}-dropdown`"
        >
          <div class="px-3 py-2 hairline border-b font-mono text-[10px] uppercase tracking-[0.22em] text-iron-200">
            mailboxes · {{ account.accounts.length }}
          </div>

          <ul class="max-h-64 overflow-y-auto py-1">
            <li v-for="a in account.accounts" :key="a.id">
              <button
                type="button"
                class="w-full flex items-center gap-3 px-3 py-2 text-left transition hover:bg-white/[0.06]"
                :class="a.id === account.activeAccountId ? 'bg-rust/10' : ''"
                @click="selectAccount(a.id)"
                :data-testid="`${testid || 'switcher'}-option-${a.id}`"
              >
                <div
                  class="w-7 h-7 rounded grid place-items-center shrink-0"
                  :class="a.id === account.activeAccountId ? 'bg-rust text-white' : 'bg-white/[0.06] text-iron-100'"
                >
                  <span class="font-mono text-[11px] font-bold">{{ initialsFor(a.email) }}</span>
                </div>
                <div class="min-w-0 flex-1">
                  <div class="text-sm font-medium truncate" :class="a.id === account.activeAccountId ? 'text-rust' : ''">
                    {{ a.email }}
                  </div>
                  <div class="font-mono text-[10px] uppercase tracking-[0.18em] text-iron-200">{{ a.provider }}</div>
                </div>
                <Check v-if="a.id === account.activeAccountId" class="w-4 h-4 text-rust shrink-0" />
              </button>
            </li>
          </ul>

          <div class="hairline border-t">
            <button
              type="button"
              class="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium text-rust hover:bg-rust/10 transition"
              @click="addAccount"
              :data-testid="`${testid || 'switcher'}-add-btn`"
            >
              <Plus class="w-4 h-4" />
              Add mailbox
            </button>
          </div>
        </div>
      </transition>
    </Teleport>

    <!-- click-outside backdrop -->
    <Teleport to="body">
      <div
        v-if="open"
        class="fixed inset-0 z-[99]"
        @click="open = false"
        aria-hidden="true"
      />
    </Teleport>
  </div>
</template>

<script setup>
import { ref, computed, nextTick } from 'vue'
import { useRouter } from 'vue-router'
import { ChevronsUpDown, Check, Plus } from 'lucide-vue-next'
import { useAccountStore } from '../stores/account'

const props = defineProps({
  compact: { type: Boolean, default: false },
  testid: { type: String, default: '' },
})

const account = useAccountStore()
const router = useRouter()
const open = ref(false)
const triggerRef = ref(null)
const dropdownStyle = ref({})

async function toggle() {
  open.value = !open.value
  if (open.value) {
    await nextTick()
    positionDropdown()
  }
}

function positionDropdown() {
  const trigger = triggerRef.value
  if (!trigger) return
  const rect = trigger.getBoundingClientRect()
  const dropdownWidth = props.compact ? 288 : Math.max(rect.width, 240)
  let left
  if (props.compact) {
    // align right edge with trigger right edge
    left = Math.min(rect.right - dropdownWidth, window.innerWidth - dropdownWidth - 8)
    left = Math.max(left, 8)
  } else {
    left = rect.left
  }
  dropdownStyle.value = {
    top: `${rect.bottom + 8}px`,
    left: `${left}px`,
    width: `${dropdownWidth}px`,
  }
}

const currentEmail = computed(() => account.currentAccount?.email || 'No account')
const providerName = computed(() => (account.currentAccount?.provider || 'no provider').toUpperCase())

function initialsFor(email) {
  return (email || 'OX').slice(0, 2).toUpperCase()
}
const initials = computed(() => initialsFor(currentEmail.value))

function selectAccount(id) {
  account.setActiveAccount(id)
  open.value = false
}

function addAccount() {
  open.value = false
  router.push({ name: 'settings', query: { add: '1' } })
}
</script>
