<script setup>
/* eslint-disable no-undef */
// Список передач (активные и завершённые в этой сессии): отмена,
// копия тикета у отправлений, открытие папки у полученных файлов.
import { computed, ref, onBeforeUnmount } from "vue";
import { useI18n } from "vue-i18n";
import { invoke } from "@tauri-apps/api/core";
import { store, human, cancelTransfer, addTransfer } from "../store";
import { vSwipeDismiss } from "../swipe";

// dir: "send" | "recv" — фильтр списка по вкладке (пусто = все)
const props = defineProps({ dir: { type: String, default: "" } });

const { t } = useI18n();
const list = computed(() =>
  Object.values(store.transfers)
    .filter((tr) => !props.dir || tr.dir === props.dir)
    .sort((a, b) => b.id - a.id)
);
function statusText(tr) {
  if (tr.active && tr.dir === "send" && !tr.netOnline && tr.status === "stWaiting")
    return t("stNetPrep");
  if (tr.multi && tr.active) return t("stTicketOpen");
  if (tr.status === "stDone" && tr.donePath) return t("stDoneAt", { p: tr.donePath });
  return tr.status && tr.status.startsWith("st") ? t(tr.status) : tr.status;
}
function pct(tr) {
  return tr.total > 0 ? Math.min(100, (tr.done / tr.total) * 100) : 0;
}
async function copyTicket(tr) {
  try { await navigator.clipboard.writeText(tr.ticket); } catch {}
  tr._copied = true;
  setTimeout(() => (tr._copied = false), 1200);
}
function openFolder(tr) {
  // Android: тап по принятому файлу открывает его в подходящем приложении
  if (tr.dir === "recv" && tr.shareUri) {
    invoke("open_received_file", { uri: tr.shareUri }).catch(() => {});
    return;
  }
  if (tr.dir === "recv" && tr.donePath) {
    invoke("show_in_folder", { path: tr.donePath }).catch(() => {});
  }
}
// Android: системный диалог «Поделиться» принятым файлом
function shareFile(tr) {
  invoke("share_received_file", { uri: tr.shareUri }).catch(() => {});
}

// свайп влево/вправо: убрать неактивную строку из списка (3 сек на «Вернуть»)
const undo = ref(null);
function dismissRow(tr) {
  commitUndo();
  delete store.transfers[tr.id];
  undo.value = { tr, timer: setTimeout(() => (undo.value = null), 3000) };
}
function undoDismiss() {
  const u = undo.value;
  if (!u) return;
  clearTimeout(u.timer);
  store.transfers[u.tr.id] = u.tr;
  undo.value = null;
}
function commitUndo() {
  if (undo.value) {
    clearTimeout(undo.value.timer);
    undo.value = null;
  }
}
onBeforeUnmount(commitUndo);

// повтор неудачной отправки (тикет или доверенное устройство)
function retrySend(tr) {
  const r = tr.retry;
  if (!r) return;
  // повтор заменяет неудачную строку — без дублей в списке
  delete store.transfers[tr.id];
  if (r.kind === "device") {
    invoke("send_to_device", {
      deviceId: r.deviceId,
      deviceName: r.deviceName,
      paths: [...r.paths],
      encrypt: store.settings.encrypt,
      compress: store.settings.compress,
      limitKib: Number(store.settings.speedLimit) || 0,
    })
      .then((dto) => {
        addTransfer(dto.id, "send", dto.name, dto.size, "");
        const nt = store.transfers[dto.id];
        nt.peer = r.deviceName;
        nt.retry = r;
      })
      .catch(() => {});
  } else {
    // тикетная отправка: через штатный поток «отправить снова»
    store.droppedPaths = [...r.paths];
    store.autoSend = true;
    store.requestTab = "send";
  }
}
</script>

<template>
  <div v-if="list.length || undo" class="col">
    <div class="set-label">{{ $t("activeTransfers") }}</div>
    <div
      v-for="tr in list"
      :key="tr.id"
      class="transfer"
      v-swipe-dismiss="{ enabled: !tr.active, onDismiss: () => dismissRow(tr) }"
    >
      <div class="row">
        <span :class="tr.dir === 'send' ? 'dir-up' : 'dir-down'">{{ tr.dir === "send" ? "↑" : "↓" }}</span>
        <span
          class="hist-name"
          :class="{ link: tr.dir === 'recv' && tr.donePath }"
          :title="tr.dir === 'recv' && tr.donePath ? $t('openFolder') : tr.name"
          @click="openFolder(tr)"
        ><b>{{ tr.name }}</b></span>
        <span v-if="tr.peer" class="hist-dim">→ {{ tr.peer }}</span>
        <span class="hist-dim">{{ human(tr.size) }}</span>
        <span style="flex: 1"></span>
        <button
          v-if="tr.dir === 'send' && tr.ticket"
          class="btn ghost btn-sm"
          :title="$t('btnCopy')"
          @click="copyTicket(tr)"
        >{{ tr._copied ? "✓" : "⧉" }}</button>
        <button
          v-if="tr.retry && !tr.active && tr.status !== 'stDone'"
          class="btn ghost btn-sm"
          data-testid="retry-send-btn"
          :title="$t('btnRetry')"
          @click="retrySend(tr)"
        >↻</button>
        <button
          v-if="tr.shareUri"
          class="btn ghost btn-sm"
          data-testid="share-file-btn"
          :title="$t('btnShare')"
          @click="shareFile(tr)"
        >⇪</button>
        <button v-if="tr.active" class="btn danger btn-sm" @click="cancelTransfer(tr.id)">
          {{ $t("btnCancel") }}
        </button>
      </div>
      <div v-if="tr.fp && tr.active" class="fp">{{ $t("fpLabel") }} {{ tr.fp }}</div>
      <div v-if="tr.resumedFrom" class="status ok">{{ $t("resumeNote", { n: human(tr.resumedFrom) }) }}</div>
      <div class="prog" v-if="tr.active && !tr.multi">
        <div class="prog-bar"><div class="prog-fill" :style="{ width: pct(tr).toFixed(1) + '%' }"></div></div>
        <div class="prog-text">
          {{ human(tr.done) }} / {{ human(tr.total) }}
          <span v-if="tr.speed"> · {{ human(Math.round(tr.speed)) }}/s</span>
        </div>
      </div>
      <div class="status" :class="tr.statusKind">{{ statusText(tr) }}</div>
    </div>
    <div v-if="undo" class="undo-bar" data-testid="undo-bar">
      <span>{{ $t("deletedRow") }}</span>
      <span style="flex: 1"></span>
      <button class="btn ghost btn-sm" data-testid="undo-btn" @click="undoDismiss">{{ $t("btnUndo") }}</button>
    </div>
  </div>
</template>
