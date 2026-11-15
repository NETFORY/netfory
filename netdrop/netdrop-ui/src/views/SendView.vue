<script setup>
import { ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { store, relayList, addTransfer, cancelTransfer, failTransfer, normalizePicked } from "../store";
import { loadCloudAccounts, saveCloudAccounts, freshCloudToken } from "../cloudAccounts";
import TransferList from "../components/TransferList.vue";

const paths = ref([]);
const ticket = ref("");
const qrSvg = ref("");
const showQr = ref(false);
const copied = ref(false);
const multiTicket = ref(false);
const preparing = ref(false);
const err = ref("");
const { t } = useI18n();

// drag&drop недоступен в Android WebView → зону перетаскивания там прячем
const isAndroid = /android/i.test(navigator.userAgent);

// новый выбор → сбрасываем предыдущий тикет (он остаётся в списке передач);
// активный мульти-тикет при этом закрывается
function resetTicket() {
  ticket.value = "";
  qrSvg.value = "";
  showQr.value = false;
  multiTicket.value = false;
  for (const tr of Object.values(store.transfers)) {
    if (tr.dir === "send" && tr.multi && tr.active) cancelTransfer(tr.id);
  }
}

// пути из drag&drop и «отправить снова» (глобальный слушатель в store.js)
watch(
  () => store.droppedPaths,
  (p) => {
    if (p && p.length) {
      paths.value = [...new Set([...paths.value, ...p])];
      store.droppedPaths = [];
      resetTicket();
      if (store.autoSend) {
        store.autoSend = false;
        startSend();
      }
    }
  },
  { immediate: true }
);

async function pickFiles() {
  const sel = await open({ multiple: true });
  if (sel) {
    try {
      const list = await normalizePicked(Array.isArray(sel) ? sel : [sel]);
      paths.value = [...new Set([...paths.value, ...list])];
      resetTicket();
    } catch (e) {
      err.value = String(e);
    }
  }
}
async function pickFolder() {
  const sel = await open({ directory: true });
  if (sel) {
    if (typeof sel === "string" && sel.startsWith("content://")) {
      err.value = t("androidFolderUnsupported");
      return;
    }
    paths.value = [...new Set([...paths.value, sel])];
    resetTicket();
  }
}

// буфер обмена: отправка текста/ссылки на доверенное устройство
const clipText = ref("");
const clipBusy = ref(false);
const clipMsg = ref("");

async function sendText(d) {
  if (!clipText.value.trim() || clipBusy.value) return;
  clipBusy.value = true;
  clipMsg.value = "…";
  try {
    await tryP2P(() => invoke("send_text_to_device", { deviceId: d.id, text: clipText.value }));
    clipMsg.value = t("clipSent") + " " + d.name;
    clipText.value = "";
  } catch (e) {
    // офлайн после 3 попыток → сообщение уезжает файлом через облачный ящик
    try {
      const p = await invoke("mailbox_text_file", { text: clipText.value });
      if (await cloudFallback(d, [p], "message.txt")) {
        clipMsg.value = t("cloudFallbackSent");
        clipText.value = "";
      } else {
        clipMsg.value = String(e) + " — " + t("cloudFallbackNone");
      }
    } catch (e2) {
      clipMsg.value = String(e2);
    }
  } finally {
    clipBusy.value = false;
    setTimeout(() => (clipMsg.value = ""), 6000);
  }
}

// 3 попытки прямого P2P с паузой; кидает последнюю ошибку
async function tryP2P(fn) {
  let last;
  for (let i = 0; i < 3; i++) {
    try {
      return await fn();
    } catch (e) {
      last = e;
      if (i < 2) await new Promise((r) => setTimeout(r, 800));
    }
  }
  throw last;
}

// Получатель офлайн, но он доверенный и для него настроен облачный ящик →
// автоматически доставляем через cloud drive (почта доверенных).
async function cloudFallback(d, sendPaths, label) {
  let st;
  try {
    st = await invoke("mailbox_status");
  } catch {
    return false;
  }
  const ob = (st.outbox || []).find((o) => o.device === d.id);
  if (!ob) return false;
  const list = loadCloudAccounts();
  const acc = list.find((a) => a.id === ob.account) || list.find((a) => a.provider === ob.provider);
  if (!acc) return false;
  const token = await freshCloudToken(acc, () => saveCloudAccounts(list));
  const id = Date.now();
  addTransfer(id, "send", "☁ " + label + " → " + d.name, 0, "");
  store.transfers[id].status = "stAccepted";
  store.transfers[id].peer = d.name;
  try {
    await invoke("mailbox_send", { id, deviceId: d.id, token, paths: sendPaths });
  } catch (e) {
    // ранняя ошибка команды — карточка не должна висеть «Передача…» вечно
    failTransfer(id, e);
    throw e;
  }
  return true;
}

async function copyIncoming() {
  try { await navigator.clipboard.writeText(store.incomingText.text); } catch {}
}

async function startSend() {
  if (!paths.value.length || preparing.value) return;
  err.value = "";
  preparing.value = true;
  store.fromShare = false;
  const sel = [...paths.value];
  try {
    const dto = await invoke("send_files", {
      paths: paths.value,
      encrypt: store.settings.encrypt,
      compress: store.settings.compress,
      relays: relayList(),
      useN1: store.settings.useN1,
      limitKib: Number(store.settings.speedLimit) || 0,
      multi: store.settings.multiRecv,
    });
    ticket.value = dto.ticket;
    qrSvg.value = dto.qrSvg || "";
    showQr.value = false;
    multiTicket.value = !!dto.multi;
    lastSendId = dto.id;
    addTransfer(dto.id, "send", dto.name, dto.size, dto.ticket);
    if (dto.multi) store.transfers[dto.id].multi = true;
    store.transfers[dto.id].retry = { kind: "ticket", paths: sel };
    paths.value = [];
  } catch (e) {
    err.value = String(e);
  } finally {
    preparing.value = false;
  }
}

// отправка на доверенное устройство (без тикета, push-режим)
async function sendToDevice(d) {
  if (!paths.value.length || preparing.value) return;
  err.value = "";
  preparing.value = true;
  store.fromShare = false;
  const sel = [...paths.value];
  try {
    const dto = await tryP2P(() =>
      invoke("send_to_device", {
        deviceId: d.id,
        deviceName: d.name,
        paths: sel,
        encrypt: store.settings.encrypt,
        compress: store.settings.compress,
        limitKib: Number(store.settings.speedLimit) || 0,
      })
    );
    addTransfer(dto.id, "send", dto.name, dto.size, "");
    store.transfers[dto.id].peer = d.name;
    store.transfers[dto.id].retry = { kind: "device", deviceId: d.id, deviceName: d.name, paths: sel };
    paths.value = [];
  } catch (e) {
    // P2P недоступен после 3 попыток → пробуем облачный ящик
    try {
      const label = (sel[0] || "").split(/[\\/]/).pop() + (sel.length > 1 ? ` (+${sel.length - 1})` : "");
      if (await cloudFallback(d, sel, label)) {
        clipMsg.value = t("cloudFallbackSent");
        setTimeout(() => (clipMsg.value = ""), 6000);
        paths.value = [];
      } else {
        err.value = String(e) + " — " + t("cloudFallbackNone");
      }
    } catch (e2) {
      err.value = String(e2);
    }
  } finally {
    preparing.value = false;
  }
}

async function copyTicket() {
  try { await navigator.clipboard.writeText(ticket.value); } catch {}
  copied.value = true;
  setTimeout(() => (copied.value = false), 1200);
}

// апгрейд тикета после выхода в сеть (relay + адреса) — обновляем текст и QR
let lastSendId = 0;
watch(
  () => store.ticketUpdate,
  (u) => {
    if (u && u.id === lastSendId && ticket.value) {
      ticket.value = u.ticket;
      if (u.qrSvg) qrSvg.value = u.qrSvg;
    }
  }
);
</script>

<template>
  <section class="panel">
    <div class="dropzone" v-if="!isAndroid" :class="{ hover: store.dragHover }">
      <div class="dz-icon">⇪</div>
      <div class="dz-text">{{ $t("dropHint") }}</div>
      <div class="dz-sub">{{ $t("dropSub") }}</div>
    </div>
    <!-- текст/ссылка на устройство — сразу под зоной перетаскивания -->
    <div v-if="store.devices.length" class="clip-block">
      <textarea
        v-model="clipText"
        :placeholder="$t('clipPh')"
        spellcheck="false"
        rows="2"
        class="clip-ta"
        data-testid="clip-text-input"
      ></textarea>
      <div class="row" style="flex-wrap: wrap">
        <button
          v-for="d in store.devices"
          :key="'t' + d.id"
          class="btn ghost btn-sm"
          :disabled="!clipText.trim() || clipBusy"
          :data-testid="'send-text-btn-' + d.id.slice(0, 6)"
          @click="sendText(d)"
        >✉ {{ d.name }}</button>
        <span class="status" v-if="clipMsg">{{ clipMsg }}</span>
      </div>
    </div>
    <div class="row">
      <button class="btn ghost" @click="pickFiles">{{ $t("pickFiles") }}</button>
      <button class="btn ghost" @click="pickFolder">{{ $t("pickFolder") }}</button>
      <span style="flex: 1"></span>
      <button v-if="paths.length" class="btn danger" @click="paths = []; resetTicket()">{{ $t("clear") }}</button>
      <button
        class="btn primary"
        data-testid="send-btn"
        :disabled="!paths.length || preparing"
        :class="{ preparing }"
        @click="startSend"
      >{{ preparing ? $t("btnPreparing") : $t("btnSend") }}</button>
    </div>
    <div v-if="preparing" class="status">{{ $t("stPreparing") }}</div>
    <div
      v-if="store.fromShare && store.devices.length && paths.length"
      class="status"
      data-testid="share-choose-hint"
    >{{ $t("shareChooseHint") }}</div>
    <div v-if="store.devices.length && paths.length" class="row" style="flex-wrap: wrap">
      <button
        v-for="d in store.devices"
        :key="d.id"
        class="btn ghost"
        :disabled="preparing"
        :data-testid="'send-to-device-btn-' + d.id.slice(0, 6)"
        @click="sendToDevice(d)"
      >⇢ {{ d.name }}</button>
    </div>
    <div v-if="paths.length" class="file-list">
      <div>{{ $t("selected") }}: {{ paths.length }}</div>
      <div v-for="p in paths" :key="p">{{ p }}</div>
    </div>

    <div v-if="ticket" class="ticket-box">
      <div class="tk-label">{{ multiTicket ? $t("ticketLabelMulti") : $t("ticketLabel") }}</div>
      <textarea class="tk-text" readonly :value="ticket" spellcheck="false"></textarea>
      <div class="row">
        <button class="btn ghost" style="flex: 1" @click="copyTicket">
          {{ copied ? $t("copied") : $t("btnCopy") }}
        </button>
        <button class="btn ghost" :class="{ 'qr-on': showQr }" @click="showQr = !showQr" title="QR">
          ▦ QR
        </button>
      </div>
      <div v-if="showQr && qrSvg" class="qr" v-html="qrSvg"></div>
    </div>
    <div v-if="err" class="status err">{{ err }}</div>

    <div
      v-if="store.incomingText"
      class="tk-box"
      data-testid="incoming-text-box"
    >
      <div class="tk-label">{{ $t("clipFrom") }} {{ store.incomingText.peer }} ({{ $t("clipCopied") }})</div>
      <div class="row">
        <input type="text" :value="store.incomingText.text" readonly spellcheck="false" />
        <button class="btn ghost btn-sm" @click="copyIncoming">{{ $t("btnCopy") }}</button>
        <button class="btn danger btn-sm" @click="store.incomingText = null">✕</button>
      </div>
    </div>

    <TransferList dir="send" />
  </section>
</template>
