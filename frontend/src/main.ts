import { createApp } from 'vue'
import { createPinia } from 'pinia'
import App from '@/App.vue'
import router from '@/router'
import { i18n } from '@/i18n'
import { useUiStore } from '@/store/ui'
import { useAuthStore } from '@/store/auth'
import '@/styles/fonts.css'
import '@/styles/themes.css'
import '@/styles/base.css'

const app = createApp(App)
// Глобальный перехват ошибок рендера: логируем вместо «тихой» пустой страницы,
// чтобы падение одного view было видно в консоли и диагностировалось.
app.config.errorHandler = (err, _instance, info) => {
  console.error('[vue-error]', info, err)
}
app.use(createPinia())
app.use(router)
app.use(i18n)

// Bootstrap persisted UI prefs + auth status before first paint.
const ui = useUiStore()
ui.init()
const auth = useAuthStore()
auth.init()

// Проброс хранительского ключа RuTracker из .env (RUTRACKER_BT) в бэкенд.
// Ключ дописывается к announce RuTracker (?kk=<key>) для приватных раздач.
const rutrackerKey = import.meta.env.RUTRACKER_BT
if (rutrackerKey) {
  import('@/lib/bridge')
    .then(({ setRutrackerKey }) => setRutrackerKey(String(rutrackerKey)))
    .catch(() => {})
}

app.mount('#app')
