<template>
  <div class="min-h-screen relative" data-testid="app-root">
    <div class="pointer-events-none fixed inset-0 opacity-[0.04] mix-blend-overlay bg-grain bg-repeat z-0" />
    <router-view v-slot="{ Component }">
      <transition name="fade" mode="out-in">
        <component :is="Component" />
      </transition>
    </router-view>
    <ToastHost />
  </div>
</template>

<script setup>
import { onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { useAccountStore } from './stores/account'
import { useMailStore } from './stores/mail'
import ToastHost from './components/ToastHost.vue'

const account = useAccountStore()
const mail = useMailStore()
const router = useRouter()

// Handoff из SmartNet: seed приходит по stdin-IPC и живёт только в RAM.
// Не блокируем старт UI — тихо запрашиваем и, если seed пришёл, входим сами.
async function tryHandoff() {
  const core = typeof window !== 'undefined' ? window.__TAURI__?.core : null
  if (!core?.invoke) return
  try {
    await core.invoke('request_smartnet_seed')
    for (let i = 0; i < 8; i++) {
      const mnemonic = await core.invoke('handoff_take')
      if (mnemonic) {
        await account.adoptHandoff(mnemonic)
        mail.emails = []
        router.replace('/mail')
        await mail.loadAll()
        mail.refresh()
        return
      }
      await new Promise((r) => setTimeout(r, 250))
    }
  } catch {
    // standalone-запуск или SmartNet не ответил — обычный онбординг
  }
}

onMounted(() => {
  account.hydrate()
  mail.hydrate()
  tryHandoff()
})
</script>
