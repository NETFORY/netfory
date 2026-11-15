<script setup>
import { ref, onMounted, onBeforeUnmount } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { store, human } from "../store";
import { vSwipeDismiss } from "../swipe";

const items = ref([]);

async function load() {
  try { items.value = await invoke("history_list"); } catch { items.value = []; }
}
async function clearAll() {
  // очистка всего — отложенное удаление больше не нужно
  if (undo.value) {
    clearTimeout(undo.value.timer);
    undo.value = null;
  }
  try { await invoke("history_clear"); } catch {}
  items.value = [];
}
function fmtTs(ts) {
  return new Date(ts * 1000).toLocaleString();
}
// «отправить снова»: подставить тот же набор путей и запустить отправку
function sendAgain(it) {
  store.droppedPaths = [...it.paths];
  store.autoSend = true;
  store.requestTab = "send";
}
// Android: «Поделиться» принятым файлом из истории
function shareFile(it) {
  invoke("share_received_file", { uri: it.shareUri }).catch(() => {});
}
// тап по имени: открыть файл (Android) или показать в папке (десктоп)
function openItem(it) {
  if (it.shareUri) {
    invoke("open_received_file", { uri: it.shareUri }).catch(() => {});
    return;
  }
  if (it.path) invoke("show_in_folder", { path: it.path }).catch(() => {});
}
// свайп влево/вправо: удалить запись с 3-секундным окном «Вернуть».
// Бэкенд трогаем только после истечения окна (commitUndo).
const undo = ref(null);
function removeItem(it, i) {
  commitUndo();
  items.value.splice(i, 1);
  undo.value = { it, index: i, timer: setTimeout(commitUndo, 3000) };
}
function commitUndo() {
  const u = undo.value;
  if (!u) return;
  clearTimeout(u.timer);
  undo.value = null;
  invoke("history_remove", {
    ts: u.it.ts, name: u.it.name, dir: u.it.dir, size: u.it.size,
  }).catch(() => {});
}
function undoRemove() {
  const u = undo.value;
  if (!u) return;
  clearTimeout(u.timer);
  items.value.splice(Math.min(u.index, items.value.length), 0, u.it);
  undo.value = null;
}
onBeforeUnmount(commitUndo);
onMounted(load);
</script>

<template>
  <section class="panel">
    <div v-if="!items.length" class="status">{{ $t("histEmpty") }}</div>
    <div
      v-for="(it, i) in items"
      :key="it.ts + ':' + it.name + ':' + i"
      class="hist-item"
      data-testid="history-item"
      v-swipe-dismiss="{ onDismiss: () => removeItem(it, i) }"
    >
      <span>{{ it.dir === "send" ? "↑" : "↓" }}</span>
      <span
        class="hist-name"
        :class="{ link: it.dir === 'recv' && (it.shareUri || it.path) }"
        :title="it.name"
        @click="openItem(it)"
      >{{ it.name }}</span>
      <span class="hist-dim">{{ human(it.size) }}</span>
      <span class="hist-dim">{{ fmtTs(it.ts) }}</span>
      <span class="badge" :class="it.status">{{ it.status }}</span>
      <button
        v-if="it.dir === 'send' && it.paths && it.paths.length"
        class="btn ghost btn-sm"
        :title="$t('sendAgain')"
        @click="sendAgain(it)"
      >↻</button>
      <button
        v-if="it.shareUri"
        class="btn ghost btn-sm"
        :title="$t('btnShare')"
        @click="shareFile(it)"
      >⇪</button>
    </div>
    <div v-if="undo" class="undo-bar" data-testid="history-undo-bar">
      <span>{{ $t("deletedRow") }}</span>
      <span style="flex: 1"></span>
      <button class="btn ghost btn-sm" data-testid="history-undo-btn" @click="undoRemove">{{ $t("btnUndo") }}</button>
    </div>
    <div v-if="items.length" class="row">
      <span style="flex: 1"></span>
      <button class="btn danger" @click="clearAll">{{ $t("histClear") }}</button>
    </div>
  </section>
</template>
