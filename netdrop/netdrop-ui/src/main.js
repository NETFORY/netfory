import { createApp } from "vue";
import App from "./App.vue";
import { i18n } from "./i18n";
import { initEvents } from "./store";
import "./style.css";

// Android edge-to-edge: помечаем платформу для CSS-отступов под системные бары
if (/android/i.test(navigator.userAgent)) {
  document.documentElement.classList.add("is-android");
}

initEvents();
createApp(App).use(i18n).mount("#app");
