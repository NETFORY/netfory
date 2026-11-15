<script setup>
import { ref, computed, watch, onMounted, onBeforeUnmount } from "vue";
import { useI18n } from "vue-i18n";
import { store, saveSettings, unlockCloud, haptic } from "./store";
import SendView from "./views/SendView.vue";
import ReceiveView from "./views/ReceiveView.vue";
import HistoryView from "./views/HistoryView.vue";
import SettingsView from "./views/SettingsView.vue";
import CloudView from "./views/CloudView.vue";

const { locale } = useI18n();
const tab = ref("send");
const cloudTab = { id: "cloud", label: "tabCloud", icon: "\u2601", cmp: CloudView };
const tabs = computed(() => [
  { id: "send", label: "tabSend", icon: "\u25B4", cmp: SendView },
  { id: "receive", label: "tabReceive", icon: "\u25BE", cmp: ReceiveView },
  { id: "history", label: "tabHistory", icon: "\u2630", cmp: HistoryView },
  ...(store.cloudUnlocked ? [cloudTab] : []),
  { id: "settings", label: "tabSettings", icon: "\u2699", cmp: SettingsView },
]);
// QR-сканер камерой доступен только на Android
const isAndroid = /android/i.test(navigator.userAgent);
// глобальная кнопка «Скан QR» в шапке: всегда под рукой для сопряжения
function topScan() {
  switchTab("receive");
  store.requestScan = Date.now();
}
function setLang(l) {
  locale.value = l;
  store.settings.lang = l;
  saveSettings();
}
function switchTab(id) {
  tab.value = id;
}
// секретный жест: 8 быстрых кликов по логотипу (≤3 с между кликами)
// открывает скрытую страницу Crypto Cloud навсегда
let logoClicks = 0;
let lastLogoClick = 0;
function logoTap() {
  const now = Date.now();
  logoClicks = now - lastLogoClick <= 3000 ? logoClicks + 1 : 1;
  lastLogoClick = now;
  if (logoClicks >= 8) {
    logoClicks = 0;
    if (!store.cloudUnlocked) unlockCloud();
    switchTab("cloud");
  }
}
// запрос переключения вкладки (например «отправить снова» из истории)
watch(
  () => store.requestTab,
  (id) => {
    if (id) {
      switchTab(id);
      store.requestTab = "";
    }
  }
);
// свайп влево/вправо листает вкладки (привычный мобильный жест)
let swX = 0;
let swY = 0;
let swOk = false;
function onTouchStart(e) {
  if (e.touches.length !== 1) {
    swOk = false;
    return;
  }
  const t = e.touches[0];
  swX = t.clientX;
  swY = t.clientY;
  const tag = (e.target?.tagName || "").toLowerCase();
  // не мешаем вводу текста и QR-сканеру
  swOk =
    !["input", "textarea", "select"].includes(tag) &&
    !document.documentElement.classList.contains("qr-scanning");
}
function onTouchEnd(e) {
  if (!swOk) return;
  const t = e.changedTouches[0];
  const dx = t.clientX - swX;
  const dy = t.clientY - swY;
  // горизонтальный жест ≥70px и заметно сильнее вертикального
  if (Math.abs(dx) < 70 || Math.abs(dx) < Math.abs(dy) * 1.5) return;
  const ids = tabs.value.map((x) => x.id);
  const next = ids.indexOf(tab.value) + (dx < 0 ? 1 : -1);
  if (next >= 0 && next < ids.length) {
    switchTab(ids[next]);
    haptic(15); // вибро-отклик на свайп
  }
}
onMounted(() => {
  window.addEventListener("touchstart", onTouchStart, { passive: true });
  window.addEventListener("touchend", onTouchEnd, { passive: true });
});
onBeforeUnmount(() => {
  window.removeEventListener("touchstart", onTouchStart);
  window.removeEventListener("touchend", onTouchEnd);
});
</script>

<template>
  <header class="top">
    <div class="logo" data-testid="netdrop-logo" @click="logoTap">net<span>drop</span></div>
    <div class="top-right">
      <button
        v-if="isAndroid"
        class="qr-top"
        data-testid="top-scan-qr-btn"
        :title="$t('btnScanQr')"
        @click="topScan"
      >▦ QR</button>
      <button class="lang-btn" :class="{ active: store.settings.lang === 'en' }" @click="setLang('en')">EN</button>
      <button class="lang-btn" :class="{ active: store.settings.lang === 'ru' }" @click="setLang('ru')">RU</button>
    </div>
  </header>
  <nav class="tabs">
    <button v-for="t in tabs" :key="t.id" class="tab" :class="{ active: tab === t.id }" @click="switchTab(t.id)">
      <span class="tab-ico">{{ t.icon }}</span><span class="tab-lbl">{{ $t(t.label) }}</span>
    </button>
  </nav>
  <component :is="tabs.find((t) => t.id === tab).cmp" />
</template>

<style scoped>
.top-right {
  display: flex;
  align-items: center;
  gap: 6px;
}
.qr-top {
  padding: 3px 10px;
  border: 1px solid var(--accent, #2dd4a7);
  border-radius: 999px;
  background: transparent;
  color: var(--accent, #2dd4a7);
  font: inherit;
  font-size: 12px;
  cursor: pointer;
  transition: background-color 0.15s ease, color 0.15s ease;
}
.qr-top:active {
  background: var(--accent, #2dd4a7);
  color: #0a0e14;
}
</style>
