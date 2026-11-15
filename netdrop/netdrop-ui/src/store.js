// Реактивное состояние + подписка на события Tauri ("nd-event", drag&drop).
// Параллельные передачи: transfers — реестр по id, события несут id.
import { reactive } from "vue";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { loadCloudAccounts, saveCloudAccounts, freshCloudToken } from "./cloudAccounts";

export const store = reactive({
  droppedPaths: [],
  dragHover: false,
  ticketUpdate: null, // апгрейд тикета после выхода в сеть {id, ticket, qrSvg}
  devices: [], // доверенные устройства [{id, name}]
  devicesVersion: 0,
  fromShare: false, // файл пришёл из «Поделиться» — показать выбор назначения
  recvDoneTick: 0, // метка успешного приёма (для сброса устаревших ошибок)
  incomingText: null, // входящий текст от доверенного устройства {text, peer, ts}
  autoSend: false,   // «отправить снова» из истории
  requestTab: "",    // запрос переключения вкладки
  requestScan: 0,    // запрос запуска QR-сканера (кнопка в шапке)
  transfers: {},     // id → состояние передачи
  cloudUnlocked: localStorage.getItem("nd-cloud") === "1", // скрытая страница Crypto Cloud
  pendingCloudTicket: null, // тикет из deep-link netdrop://ndc1…
  mailFoundTick: 0, // счётчик найденных облачной почтой посылок
  settings: {
    lang: localStorage.getItem("nd-lang") || "en",
    encrypt: localStorage.getItem("nd-encrypt") !== "0",
    compress: localStorage.getItem("nd-compress") !== "0",
    useN1: localStorage.getItem("nd-use-n1") !== "0",
    relays: localStorage.getItem("nd-relays") || "",
    outDir: localStorage.getItem("nd-out") || "",
    theme: localStorage.getItem("nd-theme") || "dark",
    speedLimit: Number(localStorage.getItem("nd-limit") || 0),
    trusted: localStorage.getItem("nd-trusted") || "",
    multiRecv: localStorage.getItem("nd-multi") === "1",
  },
});

export function applyTheme() {
  document.documentElement.dataset.theme = store.settings.theme;
}

// разблокировка скрытой страницы Crypto Cloud — навсегда
export function unlockCloud() {
  store.cloudUnlocked = true;
  localStorage.setItem("nd-cloud", "1");
}

export function trustedList() {
  return store.settings.trusted.split("\n").map((s) => s.trim().toLowerCase()).filter(Boolean);
}

export function saveSettings() {
  const s = store.settings;
  localStorage.setItem("nd-lang", s.lang);
  localStorage.setItem("nd-encrypt", s.encrypt ? "1" : "0");
  localStorage.setItem("nd-compress", s.compress ? "1" : "0");
  localStorage.setItem("nd-use-n1", s.useN1 ? "1" : "0");
  localStorage.setItem("nd-relays", s.relays);
  localStorage.setItem("nd-out", s.outDir);
  localStorage.setItem("nd-theme", s.theme);
  localStorage.setItem("nd-limit", String(s.speedLimit || 0));
  localStorage.setItem("nd-trusted", s.trusted);
  localStorage.setItem("nd-multi", s.multiRecv ? "1" : "0");
  applyTheme();
}

export function relayList() {
  return store.settings.relays.split("\n").map((s) => s.trim()).filter(Boolean);
}

export function human(n) {
  const u = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = Number(n) || 0, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${v} B` : `${v.toFixed(2)} ${u[i]}`;
}

export function addTransfer(id, dir, name, size, ticket = "") {
  store.transfers[id] = {
    id, dir, name, size, ticket,
    status: "stWaiting", statusKind: "", fp: "",
    done: 0, total: size, speed: 0, resumedFrom: 0, donePath: "",
    active: true, multi: false, peer: "", shareUri: "", netOnline: false, retry: null,
    _t: 0, _d: 0, _copied: false,
  };
}

// Команда упала ДО событий бэкенда (например «path not found») → карточка
// не должна вечно висеть «Передача…» с нерабочей отменой: гасим её ошибкой.
export function failTransfer(id, e) {
  const tr = store.transfers[id];
  if (tr && tr.active) {
    tr.status = String(e);
    tr.statusKind = "err";
    tr.active = false;
  }
}

// Android-пикер возвращает content://-URI — копируем их в кэш как файлы.
// На десктопе пути проходят как есть.
export async function normalizePicked(list) {
  const out = [];
  for (const p of list) {
    if (typeof p === "string" && p.startsWith("content://")) {
      out.push(await invoke("stash_content_uri", { uri: p }));
    } else if (p) {
      out.push(p);
    }
  }
  return out;
}

export async function cancelTransfer(id) {
  try { await invoke("cancel_transfer", { id }); } catch {}
}

// Android: приём файлов из системного «Поделиться» (share target).
async function initShareTarget() {
  try {
    const { listenForShareEvents } = await import("tauri-plugin-sharetarget-api");
    const { readFile } = await import("@tauri-apps/plugin-fs");
    await listenForShareEvents(async (intent) => {
      try {
        if (!intent || !intent.stream) return;
        const bytes = await readFile(intent.stream);
        const path = await invoke("stash_shared_file", {
          name: intent.name || "shared.bin",
          data: Array.from(bytes),
        });
        store.droppedPaths = [path];
        store.requestTab = "send";
        store.fromShare = true; // предложить выбор: тикет или устройство
      } catch (e) {
        console.warn("share intent failed", e);
      }
    });
  } catch {
    // десктоп: плагин share target не зарегистрирован — тихо пропускаем
  }
}

// лёгкая вибрация на телефоне (Android WebView; на десктопе — no-op)
export function haptic(ms = 30) {
  try {
    if (navigator.vibrate) navigator.vibrate(ms);
  } catch {}
}

// автонастройка облачного ящика для нового доверенного устройства
async function autoMailboxSetup(p) {
  const list = loadCloudAccounts();
  const acc = list[0];
  if (!acc) return;
  const token = await freshCloudToken(acc, () => saveCloudAccounts(list));
  await invoke("mailbox_setup", {
    deviceId: p.id, provider: acc.provider, token,
    account: acc.id, login: acc.login || "",
  });
}

// подтверждение «получено» от доверенного: удаляем посылку из его облака
async function finalizeMailbox(p) {
  const list = loadCloudAccounts();
  const acc = list.find((a) => a.id === p.account) || list.find((a) => a.provider === p.provider);
  if (!acc) return;
  const token = await freshCloudToken(acc, () => saveCloudAccounts(list));
  await invoke("mailbox_finalize", { deviceId: p.device, tid: p.tid, token });
}

export async function initEvents() {
  applyTheme();
  initShareTarget();
  try {
    const info = await invoke("device_info");
    store.devices = info.devices || [];
  } catch {}
  // самолечение: пустой или невалидный путь ("."/"/") → дефолт платформы
  const od = store.settings.outDir;
  if (!od || od === "." || od === "/") {
    try {
      store.settings.outDir = await invoke("default_out_dir");
      localStorage.setItem("nd-out", store.settings.outDir);
    } catch {}
  }
  await listen("tauri://drag-enter", () => (store.dragHover = true));
  await listen("tauri://drag-leave", () => (store.dragHover = false));
  await listen("tauri://drag-drop", (e) => {
    store.dragHover = false;
    store.droppedPaths = (e.payload && e.payload.paths) || [];
  });
  await listen("nd-event", (e) => {
    const p = e.payload || {};
    // deep-link netdrop://ndc1… → открываем Cloud с подставленным тикетом
    if (p.kind === "deep-link") {
      const m = String(p.url || "").match(/ndc1[A-Za-z0-9_-]+/);
      if (m) {
        unlockCloud();
        store.pendingCloudTicket = m[0];
        store.requestTab = "cloud";
      }
      return;
    }
    // почта доверенных: подтверждение получения → авто-удаление из облака
    if (p.kind === "mbox-confirmed") {
      finalizeMailbox(p).catch(() => {});
      return;
    }
    if (p.kind === "mbox-announce" || p.kind === "mbox-finalized") {
      store.devicesVersion++;
      return;
    }
    if (p.kind === "mbox-found") {
      store.mailFoundTick = Date.now();
      return;
    }
    if (p.kind === "text-received") {
      // текст от доверенного устройства: в буфер + плашка в UI
      store.incomingText = { text: p.text, peer: p.peer || "", ts: Date.now() };
      try { navigator.clipboard.writeText(p.text); } catch {}
      return;
    }
    // доверенные устройства: обновление списка
    if (p.kind === "device-paired" || p.kind === "devices-changed") {
      invoke("list_devices").then((d) => { store.devices = d || []; }).catch(() => {});
      store.devicesVersion++;
      // автообмен почтой: если облако подключено — в фоне настраиваем ящик
      if (p.kind === "device-paired" && p.id) autoMailboxSetup(p).catch(() => {});
      return;
    }
    // push-приём от доверенного устройства — своя строка в списке
    if (p.kind === "recv-start") {
      addTransfer(p.id, "recv", p.name, p.size, "");
      const t = store.transfers[p.id];
      t.peer = p.peer || "";
      t.status = "stConnected";
      return;
    }
    // мульти-режим: новый получатель подключился — своя строка в списке
    if (p.kind === "sub-start") {
      addTransfer(p.id, "send", p.name, p.size, "");
      const t = store.transfers[p.id];
      t.peer = p.peer || "";
      t.status = "stConnected";
      return;
    }
    const tr = store.transfers[p.id];
    if (!tr) return;
    switch (p.kind) {
      case "waiting":
        tr.status = "stWaiting";
        if (p.multi) tr.multi = true;
        break;
      case "connected": tr.status = "stConnected"; break;
      case "fingerprint": tr.fp = p.fp; break;
      case "resuming": tr.resumedFrom = p.offset; break;
      case "accepted": tr.status = "stAccepted"; break;
      case "progress": {
        tr.done = p.done; tr.total = p.total || tr.total;
        const now = performance.now();
        if (tr._t && now > tr._t && p.done >= tr._d) {
          tr.speed = ((p.done - tr._d) * 1000) / (now - tr._t);
        }
        tr._t = now; tr._d = p.done;
        break;
      }
      case "net-online": {
        // отправитель вышел в сеть: апгрейд тикета (relay + прямые адреса)
        tr.netOnline = true;
        if (p.ticket) {
          tr.ticket = p.ticket;
          store.ticketUpdate = { id: p.id, ticket: p.ticket, qrSvg: p.qrSvg || "" };
        }
        break;
      }
      case "done":
        tr.done = p.total || p.bytes; tr.total = p.total || p.bytes;
        tr.status = "stDone"; tr.statusKind = "ok"; tr.active = false;
        tr.donePath = p.path || p.name || "";
        tr.shareUri = p.shareUri || "";
        if (tr.dir === "recv") store.recvDoneTick = Date.now();
        haptic(tr.dir === "recv" ? 60 : 35); // вибро-отклик о завершении
        break;
      case "cancelled":
        tr.status = "stCancelled"; tr.statusKind = "err"; tr.active = false;
        break;
      case "retrying":
        tr.status = "stRetrying"; tr.statusKind = ""; tr.active = true;
        break;
      case "error":
        tr.status = p.message; tr.statusKind = "err"; tr.active = false;
        break;
    }
  });
}
