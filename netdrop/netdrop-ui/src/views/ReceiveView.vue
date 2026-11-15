<script setup>
import { ref, watch, onMounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { store, human, addTransfer, trustedList, saveSettings } from "../store";
import TransferList from "../components/TransferList.vue";

const ticket = ref("");
const offer = ref(null);
const status = ref("");
const statusKind = ref("");
// QR-сканер камерой — только Android (плагин barcode-scanner)
const isAndroid = /android/i.test(navigator.userAgent);
const scanning = ref(false);
let scanner = null;

// авто-вставка тикета из буфера обмена при открытии вкладки
onMounted(async () => {
  // запуск сканера по кнопке «▦ QR» из шапки (запрос свежее 5 с)
  if (store.requestScan && Date.now() - store.requestScan < 5000) {
    store.requestScan = 0;
    scanQr();
    return;
  }
  if (ticket.value) return;
  try {
    const clip = await navigator.clipboard.readText();
    if (clip && clip.trim().startsWith("nd1")) ticket.value = clip.trim();
  } catch {}
});

// повторный клик по «▦ QR», когда вкладка «Получить» уже открыта
watch(
  () => store.requestScan,
  (v) => {
    if (v) {
      store.requestScan = 0;
      scanQr();
    }
  }
);

async function pasteClip() {
  try {
    const clip = await navigator.clipboard.readText();
    if (clip) ticket.value = clip.trim();
  } catch {}
}

function stopScanUi() {
  scanning.value = false;
  document.documentElement.classList.remove("qr-scanning");
}

async function scanQr() {
  status.value = "";
  statusKind.value = "";
  try {
    scanner = scanner || (await import("@tauri-apps/plugin-barcode-scanner"));
    let perm = await scanner.checkPermissions();
    if (perm !== "granted") perm = await scanner.requestPermissions();
    if (perm !== "granted") {
      status.value = "stCamDenied";
      statusKind.value = "err";
      return;
    }
    scanning.value = true;
    document.documentElement.classList.add("qr-scanning");
    const res = await scanner.scan({
      windowed: true,
      formats: [scanner.Format.QRCode],
      cameraDirection: "back",
    });
    stopScanUi();
    const text = ((res && res.content) || "").trim();
    if (text.startsWith("ndp1:")) {
      // QR сопряжения устройства
      await invoke("add_device_by_code", { code: text });
      store.devicesVersion++;
      status.value = "stDevicePaired";
      statusKind.value = "ok";
      return;
    }
    if (text) {
      ticket.value = text;
      if (text.startsWith("nd1")) await probe();
    }
  } catch (e) {
    stopScanUi();
    const msg = String(e);
    // отмена пользователем — не ошибка
    if (!/cancel/i.test(msg)) {
      status.value = msg;
      statusKind.value = "err";
    }
  }
}

async function cancelScan() {
  try {
    if (scanner) await scanner.cancel();
  } catch {}
  stopScanUi();
}

// успешный приём → убираем устаревшую красную ошибку probe
watch(
  () => store.recvDoneTick,
  () => {
    if (statusKind.value === "err") {
      status.value = "";
      statusKind.value = "";
    }
  }
);

async function probe() {
  if (!ticket.value.trim()) return;
  status.value = "stConnecting";
  statusKind.value = "";
  offer.value = null;
  try {
    const meta = await invoke("receive_probe", { ticket: ticket.value.trim() });
    status.value = "";
    // доверенный отправитель → принимаем без подтверждения
    if (meta.senderId && trustedList().includes(meta.senderId.toLowerCase())) {
      offer.value = meta;
      status.value = "stAutoAccept";
      await accept();
      return;
    }
    offer.value = meta;
  } catch (e) {
    const msg = String(e);
    // мгновенный тикет: отправитель мог ещё не выйти в сеть
    status.value = /timed out/i.test(msg) ? "stSenderNotReady" : msg;
    statusKind.value = "err";
  }
}

async function accept() {
  const o = offer.value;
  offer.value = null;
  try {
    const id = await invoke("receive_accept", { outDir: store.settings.outDir || "" });
    addTransfer(id, "recv", o.name, o.size);
    status.value = "";
    ticket.value = "";
  } catch (e) {
    status.value = String(e);
    statusKind.value = "err";
  }
}

function trustSender() {
  const o = offer.value;
  if (!o || !o.senderId) return;
  if (!trustedList().includes(o.senderId.toLowerCase())) {
    store.settings.trusted = (store.settings.trusted + "\n" + o.senderId).trim();
    saveSettings();
  }
}

async function reject() {
  offer.value = null;
  try { await invoke("receive_reject"); } catch {}
  status.value = "stRejected";
  statusKind.value = "";
}
</script>

<template>
  <section class="panel">
    <textarea v-model="ticket" :placeholder="$t('ticketPh')" spellcheck="false"></textarea>
    <div class="row">
      <button class="btn ghost" @click="pasteClip">{{ $t("btnPaste") }}</button>
      <button v-if="isAndroid" class="btn ghost" data-testid="scan-qr-btn" @click="scanQr">
        ▦ {{ $t("btnScanQr") }}
      </button>
      <span style="flex: 1"></span>
      <button class="btn primary" :disabled="!ticket.trim()" @click="probe">{{ $t("btnConnect") }}</button>
    </div>

    <div v-if="scanning" class="qr-scan-overlay" data-testid="qr-scan-overlay">
      <div class="qr-scan-hint">{{ $t("qrScanHint") }}</div>
      <div class="qr-scan-frame"></div>
      <button class="btn danger" @click="cancelScan">{{ $t("btnCancel") }}</button>
    </div>

    <div v-if="offer" class="offer">
      <div class="offer-title">{{ $t("offerTitle") }}</div>
      <div class="offer-meta">
        {{ offer.isDir ? $t("folderSet") : $t("file") }}: <b>{{ offer.name }}</b>
        ({{ human(offer.size) }}<span v-if="offer.isDir">, {{ offer.fileCount }} {{ $t("files") }}</span>)
        · {{ offer.encrypted ? $t("encOn") : $t("encOff") }}
      </div>
      <div v-if="offer.fingerprint" class="fp">{{ $t("fpLabel") }} {{ offer.fingerprint }}</div>
      <div class="row">
        <button class="btn primary" @click="accept">{{ $t("btnAccept") }}</button>
        <button class="btn danger" @click="reject">{{ $t("btnReject") }}</button>
        <span style="flex: 1"></span>
        <button class="btn ghost btn-sm" :title="$t('btnTrustHint')" @click="trustSender(); accept()">
          {{ $t("btnTrust") }}
        </button>
      </div>
    </div>

    <div class="status" :class="statusKind">
      {{ status && status.startsWith("st") ? $t(status) : status }}
    </div>

    <TransferList dir="recv" />
  </section>
</template>
