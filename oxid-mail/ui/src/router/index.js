import { createRouter, createWebHashHistory } from 'vue-router'
import { useAccountStore } from '../stores/account'

const routes = [
  { path: '/', redirect: '/mail' },
  { path: '/onboarding', name: 'onboarding', component: () => import('../views/Onboarding.vue') },
  { path: '/mail', name: 'mail', component: () => import('../views/Mailbox.vue') },
  { path: '/mail/:folder', name: 'mail-folder', component: () => import('../views/Mailbox.vue') },
  { path: '/settings', name: 'settings', component: () => import('../views/Settings.vue') },
]

const router = createRouter({
  history: createWebHashHistory(),
  routes,
})

router.beforeEach((to) => {
  const account = useAccountStore()
  account.hydrate()
  if (!account.onboarded && to.name !== 'onboarding') {
    return { name: 'onboarding' }
  }
  if (account.onboarded && to.name === 'onboarding') {
    return { name: 'mail' }
  }
})

export default router
