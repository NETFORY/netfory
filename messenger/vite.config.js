// NETFORY Messenger UI. Исходники чата, мост (bridge), i18n, сторы и стили
// переиспользуются из ../frontend/src через алиас '@' — без копирования.
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import path from 'path'
import { fileURLToPath } from 'url'
import { readFileSync } from 'fs'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const shared = path.resolve(__dirname, '../frontend/src')
const pkgVersion = JSON.parse(readFileSync(path.resolve(__dirname, 'package.json'), 'utf8')).version

// `tauri android dev` выставляет TAURI_DEV_HOST (IP машины) — dev-сервер должен слушать сеть,
// HMR идёт отдельным портом; на десктопе — как раньше, только localhost.
const host = process.env.TAURI_DEV_HOST

export default defineConfig({
  root: path.resolve(__dirname, 'ui'),
  plugins: [vue()],
  define: { __UI_VERSION__: JSON.stringify(pkgVersion) },
  envPrefix: ['VITE_', 'CHAT_KEY_FEE_STH', 'SMARTHOLDEM_NODES_P2P'],
  resolve: {
    alias: { '@': shared, '~msg': path.resolve(__dirname, 'ui') },
    // один экземпляр vue/pinia для общих исходников
    dedupe: ['vue', 'pinia', 'vue-i18n', 'vue-router'],
  },
  clearScreen: false,
  server: {
    host: host || false,
    port: 3010,
    strictPort: true,
    hmr: host ? { protocol: 'ws', host, port: 3011 } : undefined,
    fs: { allow: [path.resolve(__dirname, '..')] },
    watch: { ignored: ['**/src-tauri/target/**', '**/target/**'] },
  },
  build: { outDir: path.resolve(__dirname, 'dist'), emptyOutDir: true, target: 'esnext' },
})
