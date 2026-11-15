import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import path from 'path'
import { fileURLToPath } from 'url'
import i18nCheck from './scripts/vite-plugin-i18n-check.js'

// SmartNet Vue 3 client. Web Fallback dev-server runs on :3000 (supervisor managed).
// Tauri picks up the same dev-server / dist output for native builds.
//
// ВАЖНО: все пути ниже привязаны к абсолютному пути этого файла (`__dirname`), а
// не к `process.cwd()`. Иначе, когда `tauri build` запускает `yarn build` из
// другой рабочей директории (например, при плоской раскладке `src-tauri/` в
// корне репозитория), vite пишет `dist/` не туда, где Tauri его потом ищет, и
// выдаёт ошибку `Unable to find your web assets`.
const __dirname = path.dirname(fileURLToPath(import.meta.url))

// Версия UI-бандла зашивается при сборке (из package.json): клиент дописывает
// её в заголовок окна и подсвечивает расхождение с Rust-версией (устаревший
// dist в сборке больше не спутать со свежим — см. AppLayout syncTitleVersions).
import { readFileSync } from 'fs'
const pkgVersion = JSON.parse(readFileSync(path.resolve(__dirname, 'package.json'), 'utf8')).version

export default defineConfig({
  root: __dirname,
  // i18n-check: каждая строка локалей компилируется компилятором vue-i18n; ошибка (напр. неэкранированный @)
  // валит `vite build` — в production такая строка бросает SyntaxError и компонент исчезает (см. RULES.md R5).
  plugins: [vue(), i18nCheck({ dir: path.resolve(__dirname, 'src/i18n') })],
  define: { __UI_VERSION__: JSON.stringify(pkgVersion) },
  // Expose ALLOW_HOST (ClearNet whitelist) to the client alongside VITE_*.
  envPrefix: ['VITE_', 'ALLOW_HOST', 'CHAT_KEY_FEE_STH', 'FEE_ALLOW_ALL_DAPPS', 'DOMAIN_DURATION_DAYS', 'FILE_EXCHANGE_PAGE_ALLOW', 'DEVELOPERS', 'RUTRACKER_BT', 'SMARTHOLDEM_NODES_P2P', 'CLOUD_SEEDERS'],
  resolve: {
    alias: { '@': path.resolve(__dirname, 'src') },
  },
  clearScreen: false,
  server: {
    host: '0.0.0.0',
    port: 3000,
    strictPort: true,
    allowedHosts: true,
    hmr: { clientPort: 443, protocol: 'wss' },
    watch: {
      // Never watch Rust build artifacts / native target — they blow past the
      // inotify watcher limit (ENOSPC) and crash the dev server.
      ignored: ['**/src-tauri/target/**', '**/target/**', '**/.git/**'],
    },
  },
  build: {
    // Абсолютный outDir — гарантирует, что бандл всегда попадёт рядом с
    // vite.config.js, а не в текущий cwd. Tauri в `frontendDist: "../dist"`
    // указывает именно на этот каталог (src-tauri рядом с vite.config.js).
    outDir: path.resolve(__dirname, 'dist'),
    emptyOutDir: true,
    target: 'esnext',
  },
})
