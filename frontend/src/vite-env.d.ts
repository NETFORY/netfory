/// <reference types="vite/client" />

declare module '*.vue' {
  import type { DefineComponent } from 'vue'
  const component: DefineComponent<{}, {}, any>
  export default component
}

interface Window {
  __TAURI_INTERNALS__?: unknown
  __TAURI__?: any
}

// Версия UI-бандла, зашитая при сборке (vite define, из package.json)
declare const __UI_VERSION__: string
