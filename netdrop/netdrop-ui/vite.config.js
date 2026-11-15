import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// Tauri: фиксированный порт dev-сервера, сборка в dist/
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    target: "es2021",
  },
});
