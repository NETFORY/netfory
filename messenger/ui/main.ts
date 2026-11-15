import { createApp } from 'vue'
import { createPinia } from 'pinia'
import { i18n } from '@/i18n'
import { useUiStore } from '@/store/ui'
import App from '~msg/App.vue'
import '@/styles/fonts.css'
import '@/styles/themes.css'
import '@/styles/base.css'
import '~msg/messenger.css'

const app = createApp(App)
app.config.errorHandler = (err, _i, info) => console.error('[vue-error]', info, err)
app.use(createPinia())
app.use(i18n)
useUiStore().init()
app.mount('#app')
