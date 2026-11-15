<script setup>
// Crypto Cloud — скрытая страница (8 быстрых кликов по логотипу).
// Подключение облачных дисков (Yandex / Google Drive / Mail.ru WebDAV)
// и обмен файлами через шифрованную папку .netdrop, когда прямая
// P2P-связь недоступна. Тикет ndc1… содержит ссылку + E2E-ключ.
import { ref, computed, onMounted, watch } from "vue";
import { useI18n } from "vue-i18n";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { store, human, addTransfer, failTransfer, normalizePicked } from "../store";
import { loadCloudAccounts, saveCloudAccounts, freshCloudToken } from "../cloudAccounts";
import TransferList from "../components/TransferList.vue";

const { t } = useI18n();
const err = ref("");
const busy = ref(false);
const isAndroid = /android/i.test(navigator.userAgent);

// ── аккаунты облаков (localStorage: nd-cloud-accounts) ──
const accounts = ref([]);
const PROV = { yandex: "YA", gdrive: "GD", mailru: "MR" };
const genId = () => Math.random().toString(36).slice(2, 10);

function loadAccounts() {
  accounts.value = loadCloudAccounts();
}
function saveAccounts() {
  saveCloudAccounts(accounts.value);
}

async function refreshInfo(acc) {
  try {
    const i = await invoke("cloud_account_info", { provider: acc.provider, token: acc.token });
    acc.login = i.login || acc.login;
    acc.total = i.total || 0;
    acc.used = i.used || 0;
    saveAccounts();
  } catch {}
}

// общий финал OAuth: сохранение аккаунта + авто-подготовка ящика почты
async function finishOauth(provider, r) {
  const acc = {
    id: genId(), provider, login: "",
    token: r.accessToken, refresh: r.refreshToken || "",
    expiresAt: Date.now() + (r.expiresIn || 3600) * 1000,
    total: 0, used: 0,
  };
  await refreshInfo(acc);
  // дедуп по provider+login: повторный OAuth обновляет токены
  const dup = accounts.value.find((a) => a.provider === provider && a.login && a.login === acc.login);
  if (dup) Object.assign(dup, acc, { id: dup.id });
  else accounts.value.push(acc);
  saveAccounts();
  if (!sendAccId.value) sendAccId.value = accounts.value[0].id;
  // авто: папка почты .netdrop/mailbox создаётся и публикуется сразу
  invoke("mailbox_prepare", {
    provider: acc.provider, token: acc.token, account: acc.id, login: acc.login || "",
  }).then(loadMbox).catch(() => {});
}

// Android: OAuth Яндекса через системный браузер + ручной ввод кода
// (второе webview-окно на Android не поддерживается — зависает белым экраном)
const yaWait = ref(false);
const yaCode = ref("");
const yaVerifier = ref("");
const oauthBusy = ref(false);

async function addOauthAccount(provider) {
  err.value = "";
  if (provider === "yandex" && isAndroid) {
    try {
      const s = await invoke("cloud_yandex_auth_start");
      yaVerifier.value = s.verifier;
      yaCode.value = "";
      yaWait.value = true;
      await openUrl(s.url);
    } catch (e) {
      err.value = String(e);
    }
    return;
  }
  busy.value = true;
  oauthBusy.value = true;
  try {
    const cmd = provider === "yandex" ? "cloud_yandex_oauth" : "cloud_google_oauth";
    const r = await invoke(cmd);
    await finishOauth(provider, r);
  } catch (e) {
    err.value = String(e);
  } finally {
    busy.value = false;
    oauthBusy.value = false;
  }
}

async function confirmYaCode() {
  if (!yaCode.value.trim() || busy.value) return;
  err.value = "";
  busy.value = true;
  try {
    const r = await invoke("cloud_yandex_exchange", {
      code: yaCode.value.trim(), verifier: yaVerifier.value,
    });
    yaWait.value = false;
    yaCode.value = "";
    await finishOauth("yandex", r);
  } catch (e) {
    err.value = String(e);
  } finally {
    busy.value = false;
  }
}

const mailruCreds = ref("");
async function addMailru() {
  if (!mailruCreds.value.includes(":")) {
    err.value = t("cloudMailruPh");
    return;
  }
  err.value = "";
  busy.value = true;
  try {
    const token = mailruCreds.value.trim();
    const i = await invoke("cloud_account_info", { provider: "mailru", token });
    const acc = {
      id: genId(), provider: "mailru", login: i.login, token,
      refresh: "", expiresAt: 0, total: i.total || 0, used: i.used || 0,
    };
    const dup = accounts.value.find((a) => a.provider === "mailru" && a.login === acc.login);
    if (dup) Object.assign(dup, acc, { id: dup.id });
    else accounts.value.push(acc);
    saveAccounts();
    mailruCreds.value = "";
    if (!sendAccId.value) sendAccId.value = accounts.value[0].id;
    invoke("mailbox_prepare", {
      provider: "mailru", token: acc.token, account: acc.id, login: acc.login || "",
    }).then(loadMbox).catch(() => {});
  } catch (e) {
    err.value = String(e);
  } finally {
    busy.value = false;
  }
}

function removeAccount(acc) {
  accounts.value = accounts.value.filter((a) => a.id !== acc.id);
  saveAccounts();
  if (sendAccId.value === acc.id) sendAccId.value = accounts.value[0]?.id || "";
}

// авто-рефреш OAuth-токена перед операцией (за минуту до истечения)
async function freshToken(acc) {
  return freshCloudToken(acc, saveAccounts);
}

// ── отправка через облако ──
const paths = ref([]);
const sendAccId = ref("");
const ticket = ref("");
const qrSvg = ref("");
const showQr = ref(false);
const copied = ref(false);
const sendAcc = computed(() => accounts.value.find((a) => a.id === sendAccId.value) || accounts.value[0]);

async function pickFiles() {
  const sel = await open({ multiple: true });
  if (sel) {
    try {
      // Android: content://-URI конвертируются в кэш-файлы (иначе бэкенд
      // падает «path not found», а карточка передачи виснет навсегда)
      const list = await normalizePicked(Array.isArray(sel) ? sel : [sel]);
      paths.value = [...new Set([...paths.value, ...list])];
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
  }
}

function bundleName(list) {
  const f = (list[0] || "").split(/[\\/]/).pop() || "netdrop";
  return list.length > 1 ? `${f} (+${list.length - 1} more)` : f;
}

async function startUpload() {
  const acc = sendAcc.value;
  if (!acc || !paths.value.length || busy.value) return;
  err.value = "";
  ticket.value = "";
  busy.value = true;
  const id = Date.now();
  addTransfer(id, "send", "☁ " + bundleName(paths.value), 0, "");
  store.transfers[id].status = "stAccepted";
  try {
    const token = await freshToken(acc);
    const dto = await invoke("cloud_send", {
      id, provider: acc.provider, token,
      account: acc.id, login: acc.login || "",
      paths: [...paths.value],
    });
    ticket.value = dto.ticket;
    qrSvg.value = dto.qrSvg || "";
    showQr.value = false;
    store.transfers[id].ticket = dto.ticket;
    paths.value = [];
    await loadSent();
    refreshInfo(acc);
  } catch (e) {
    err.value = String(e);
    failTransfer(id, e);
  } finally {
    busy.value = false;
  }
}

async function copyTicket() {
  try { await navigator.clipboard.writeText(ticket.value); } catch {}
  copied.value = true;
  setTimeout(() => (copied.value = false), 1200);
}

// короткая ссылка netdrop://ndc1… — открывает тикет в приложении одним кликом
const linkCopied = ref(false);
async function copyLink() {
  try { await navigator.clipboard.writeText("netdrop://" + ticket.value); } catch {}
  linkCopied.value = true;
  setTimeout(() => (linkCopied.value = false), 1200);
}

// ── почта доверенных (через облако) ──
const mbStatus = ref({ outbox: [], inboxes: [], receipts: 0, pollMin: 20 });
const mailMsg = ref("");
const pollBusy = ref(false);

async function loadMbox() {
  try { mbStatus.value = await invoke("mailbox_status"); } catch {}
}
function outboxFor(devId) {
  return (mbStatus.value.outbox || []).find((o) => o.device === devId);
}
function inboxFrom(devId) {
  return (mbStatus.value.inboxes || []).some((i) => i.device === devId);
}
async function setupMailbox(d) {
  const acc = sendAcc.value;
  if (!acc || busy.value) return;
  err.value = "";
  busy.value = true;
  try {
    const token = await freshToken(acc);
    await invoke("mailbox_setup", {
      deviceId: d.id, provider: acc.provider, token,
      account: acc.id, login: acc.login || "",
    });
    await loadMbox();
  } catch (e) {
    err.value = String(e);
  } finally {
    busy.value = false;
  }
}
async function mailboxSend(d) {
  const ob = outboxFor(d.id);
  if (!ob || !paths.value.length || busy.value) return;
  const acc = accounts.value.find((a) => a.id === ob.account)
    || accounts.value.find((a) => a.provider === ob.provider);
  if (!acc) { err.value = t("cloudAccountGone"); return; }
  err.value = "";
  busy.value = true;
  const id = Date.now();
  addTransfer(id, "send", "☁ " + bundleName(paths.value) + " → " + d.name, 0, "");
  store.transfers[id].status = "stAccepted";
  store.transfers[id].peer = d.name;
  try {
    const token = await freshToken(acc);
    await invoke("mailbox_send", { id, deviceId: d.id, token, paths: [...paths.value] });
    paths.value = [];
    await loadMbox();
    refreshInfo(acc);
  } catch (e) {
    err.value = String(e);
    failTransfer(id, e);
  } finally {
    busy.value = false;
  }
}
async function pollNow() {
  if (pollBusy.value) return;
  pollBusy.value = true;
  mailMsg.value = "";
  const mailru = accounts.value.find((a) => a.provider === "mailru");
  try {
    const r = await invoke("mailbox_poll", { mailruToken: mailru ? mailru.token : null });
    mailMsg.value = t("cloudMailChecked", { n: r.found || 0 });
  } catch (e) {
    mailMsg.value = String(e);
  } finally {
    pollBusy.value = false;
    setTimeout(() => (mailMsg.value = ""), 6000);
  }
}
// фон нашёл новые посылки / изменились ящики → обновляем статус
watch(() => store.mailFoundTick, () => loadMbox());
watch(() => store.devicesVersion, () => loadMbox());

// ── приём из облака ──
const recvTicket = ref("");
const recvErr = ref("");
const recvBusy = ref(false);
const recvInfo = ref(null);
const needMailru = computed(
  () => recvInfo.value?.provider === "mailru" && !accounts.value.some((a) => a.provider === "mailru")
);

async function inspectTicket() {
  recvInfo.value = null;
  const v = recvTicket.value.trim().replace(/^netdrop:\/\//, "");
  if (!v.startsWith("ndc1")) return;
  try {
    recvInfo.value = await invoke("cloud_ticket_info", { ticket: v });
  } catch {
    recvInfo.value = null;
  }
}
async function pasteTicket() {
  try {
    recvTicket.value = (await navigator.clipboard.readText()).trim();
    inspectTicket();
  } catch {}
}
async function startDownload() {
  const v = recvTicket.value.trim();
  if (!v || recvBusy.value) return;
  recvErr.value = "";
  recvBusy.value = true;
  const id = Date.now();
  addTransfer(id, "recv", "☁ " + (recvInfo.value?.name || "cloud"), recvInfo.value?.size || 0, "");
  store.transfers[id].status = "stConnecting";
  const mailru = accounts.value.find((a) => a.provider === "mailru");
  try {
    await invoke("cloud_recv", {
      id, ticket: v,
      outDir: store.settings.outDir || "",
      mailruToken: mailru ? mailru.token : null,
    });
    recvTicket.value = "";
    recvInfo.value = null;
  } catch (e) {
    recvErr.value = String(e);
    failTransfer(id, e);
  } finally {
    recvBusy.value = false;
  }
}

// ── реестр «в облаке» (что лежит на дисках, можно удалить) ──
const sent = ref([]);
const sentCopied = ref("");
async function loadSent() {
  try { sent.value = await invoke("cloud_sent_list"); } catch {}
}
async function copySent(row) {
  try { await navigator.clipboard.writeText(row.ticket); } catch {}
  sentCopied.value = row.tid;
  setTimeout(() => (sentCopied.value = ""), 1200);
}
async function deleteSent(row) {
  err.value = "";
  const acc =
    accounts.value.find((a) => a.id === row.account) ||
    accounts.value.find((a) => a.provider === row.provider);
  if (!acc) {
    err.value = t("cloudAccountGone");
    return;
  }
  try {
    const token = await freshToken(acc);
    await invoke("cloud_sent_delete", {
      tid: row.tid, provider: row.provider, token, gfolder: row.gfolder || "",
    });
    await loadSent();
    refreshInfo(acc);
  } catch (e) {
    err.value = String(e);
  }
}
async function forgetSent(row) {
  await invoke("cloud_sent_forget", { tid: row.tid }).catch(() => {});
  await loadSent();
}

onMounted(() => {
  loadAccounts();
  loadSent();
  loadMbox();
  if (accounts.value.length) sendAccId.value = accounts.value[0].id;
  for (const a of accounts.value) refreshInfo(a);
});

// тикет из deep-link netdrop://ndc1… (store.js кладёт его при событии)
watch(
  () => store.pendingCloudTicket,
  (v) => {
    if (v) {
      recvTicket.value = v;
      store.pendingCloudTicket = null;
      inspectTicket();
    }
  },
  { immediate: true }
);
</script>

<template>
  <section class="panel" data-testid="cloud-view">
    <!-- облачные диски -->
    <div class="cl-title">{{ $t("cloudDrives") }}</div>
    <div v-if="!accounts.length" class="status">{{ $t("cloudNoAccounts") }}</div>
    <div v-for="a in accounts" :key="a.id" class="cl-acc" :data-testid="'cloud-acc-' + a.provider">
      <span class="cl-prov" :data-prov="a.provider">{{ PROV[a.provider] }}</span>
      <span class="cl-login">{{ a.login || a.provider }}</span>
      <span v-if="a.total" class="cl-quota">{{ human(a.used) }} / {{ human(a.total) }}</span>
      <button class="btn danger btn-sm" data-testid="cloud-acc-remove" @click="removeAccount(a)">✕</button>
    </div>
    <div class="row" style="flex-wrap: wrap">
      <button class="btn ghost" data-testid="cloud-add-yandex" :disabled="busy" @click="addOauthAccount('yandex')">
        {{ $t("cloudAddYandex") }}
      </button>
      <button class="btn ghost" data-testid="cloud-add-google" :disabled="busy" @click="addOauthAccount('gdrive')">
        {{ $t("cloudAddGoogle") }}
      </button>
    </div>
    <!-- Android: ввод кода подтверждения Яндекса из системного браузера -->
    <template v-if="yaWait">
      <div class="status cl-note">{{ $t("cloudYaCodeHint") }}</div>
      <div class="row">
        <input
          type="text"
          v-model="yaCode"
          :placeholder="$t('cloudYaCodePh')"
          inputmode="numeric"
          spellcheck="false"
          data-testid="cloud-ya-code-input"
          style="flex: 1"
          @keyup.enter="confirmYaCode"
        />
        <button
          class="btn primary btn-sm"
          data-testid="cloud-ya-code-ok"
          :disabled="busy || !yaCode.trim()"
          @click="confirmYaCode"
        >{{ $t("cloudYaCodeOk") }}</button>
        <button class="btn ghost btn-sm" data-testid="cloud-ya-code-cancel" @click="yaWait = false">✕</button>
      </div>
    </template>
    <div v-if="oauthBusy" class="status" data-testid="cloud-oauth-busy">{{ $t("cloudOauthBrowser") }}</div>
    <div class="row">
      <input
        type="text"
        v-model="mailruCreds"
        :placeholder="$t('cloudMailruPh')"
        spellcheck="false"
        data-testid="cloud-mailru-input"
        style="flex: 1"
      />
      <button class="btn ghost btn-sm" data-testid="cloud-add-mailru" :disabled="busy || !mailruCreds" @click="addMailru">
        {{ $t("cloudAddMailru") }}
      </button>
    </div>
    <div class="status cl-note">{{ $t("cloudE2eNote", { n: 100 }) }}</div>
    <div v-if="err" class="status err" data-testid="cloud-error">{{ err }}</div>

    <!-- почта доверенных -->
    <div class="cl-title">{{ $t("cloudMailTitle") }}</div>
    <div v-if="!store.devices.length" class="status">{{ $t("cloudMailNoDevices") }}</div>
    <div v-for="d in store.devices" :key="'mb' + d.id" class="cl-acc" :data-testid="'mbox-dev-' + d.id.slice(0, 6)">
      <span class="cl-login">{{ d.name }}</span>
      <span v-if="outboxFor(d.id)" class="cl-quota">
        → {{ PROV[outboxFor(d.id).provider] }}
        <template v-if="outboxFor(d.id).pending"> · {{ $t("cloudMailPending", { n: outboxFor(d.id).pending }) }}</template>
      </span>
      <span v-if="inboxFrom(d.id)" class="cl-quota">⇦ ✓</span>
      <button
        class="btn ghost btn-sm"
        :title="$t('cloudMailSetup')"
        :disabled="busy || !sendAcc"
        :data-testid="'mbox-setup-' + d.id.slice(0, 6)"
        @click="setupMailbox(d)"
      >⚙</button>
      <button
        class="btn ghost btn-sm"
        :title="$t('cloudMailSend')"
        :disabled="busy || !paths.length || !outboxFor(d.id)"
        :data-testid="'mbox-send-' + d.id.slice(0, 6)"
        @click="mailboxSend(d)"
      >☁→</button>
    </div>
    <div v-if="store.devices.length" class="row">
      <button class="btn ghost" style="flex: 1" data-testid="mbox-poll-btn" :disabled="pollBusy" @click="pollNow">
        {{ pollBusy ? "…" : $t("cloudMailCheck") }}
      </button>
    </div>
    <div v-if="mailMsg" class="status" data-testid="mbox-poll-msg">{{ mailMsg }}</div>
    <div v-if="store.devices.length" class="status cl-note">{{ $t("cloudMailHint", { n: mbStatus.pollMin }) }}</div>

    <!-- отправка -->
    <div class="cl-title">{{ $t("cloudSendTitle") }}</div>
    <div class="row">
      <button class="btn ghost" @click="pickFiles">{{ $t("pickFiles") }}</button>
      <button class="btn ghost" @click="pickFolder">{{ $t("pickFolder") }}</button>
      <span style="flex: 1"></span>
      <button v-if="paths.length" class="btn danger" @click="paths = []">{{ $t("clear") }}</button>
    </div>
    <div v-if="accounts.length" class="row">
      <select v-model="sendAccId" class="cl-select" data-testid="cloud-account-select">
        <option v-for="a in accounts" :key="a.id" :value="a.id">
          {{ PROV[a.provider] }} · {{ a.login || a.provider }}
        </option>
      </select>
      <button
        class="btn primary"
        data-testid="cloud-upload-btn"
        :disabled="!paths.length || busy || !sendAcc"
        @click="startUpload"
      >{{ busy ? $t("btnPreparing") : $t("cloudUpload") }}</button>
    </div>
    <div v-if="busy" class="status">{{ $t("cloudUploading") }}</div>
    <div v-if="sendAcc && sendAcc.provider === 'mailru'" class="status">{{ $t("cloudMailruHint") }}</div>
    <div v-if="paths.length" class="file-list">
      <div>{{ $t("selected") }}: {{ paths.length }}</div>
      <div v-for="p in paths" :key="p">{{ p }}</div>
    </div>
    <div v-if="ticket" class="ticket-box">
      <div class="tk-label">{{ $t("cloudTicketLabel") }}</div>
      <textarea class="tk-text" readonly :value="ticket" spellcheck="false" data-testid="cloud-ticket-out"></textarea>
      <div class="row">
        <button class="btn ghost" style="flex: 1" data-testid="cloud-ticket-copy" @click="copyTicket">
          {{ copied ? $t("copied") : $t("btnCopy") }}
        </button>
        <button class="btn ghost" style="flex: 1" data-testid="cloud-link-copy" @click="copyLink">
          {{ linkCopied ? $t("copied") : $t("cloudLinkCopy") }}
        </button>
        <button v-if="qrSvg" class="btn ghost" :class="{ 'qr-on': showQr }" @click="showQr = !showQr" title="QR">
          ▦ QR
        </button>
      </div>
      <div v-if="showQr && qrSvg" class="qr" v-html="qrSvg"></div>
    </div>

    <!-- приём -->
    <div class="cl-title">{{ $t("cloudRecvTitle") }}</div>
    <div class="row">
      <input
        type="text"
        v-model="recvTicket"
        :placeholder="$t('cloudTicketPh')"
        spellcheck="false"
        data-testid="cloud-recv-input"
        style="flex: 1"
        @input="inspectTicket"
      />
      <button class="btn ghost btn-sm" @click="pasteTicket">{{ $t("btnPaste") }}</button>
    </div>
    <div v-if="recvInfo" class="status" data-testid="cloud-recv-info">
      {{ recvInfo.name }} · {{ human(recvInfo.size) }} · {{ recvInfo.chunks }}× · {{ recvInfo.provider }}
    </div>
    <div v-if="needMailru" class="status err">{{ $t("cloudMailruHint") }}</div>
    <div class="row">
      <button
        class="btn primary"
        style="flex: 1"
        data-testid="cloud-download-btn"
        :disabled="!recvTicket.trim() || recvBusy"
        @click="startDownload"
      >{{ $t("cloudDownload") }}</button>
    </div>
    <div v-if="recvErr" class="status err" data-testid="cloud-recv-error">{{ recvErr }}</div>

    <!-- что лежит в облаке -->
    <template v-if="sent.length">
      <div class="cl-title">{{ $t("cloudSentTitle") }}</div>
      <div v-for="r in sent" :key="r.tid" class="cl-acc" data-testid="cloud-sent-row">
        <span class="cl-prov" :data-prov="r.provider">{{ PROV[r.provider] }}</span>
        <span class="cl-login">{{ r.name }}</span>
        <span class="cl-quota">{{ human(r.size) }}</span>
        <button class="btn ghost btn-sm" :title="$t('btnCopy')" @click="copySent(r)">
          {{ sentCopied === r.tid ? "✓" : "⧉" }}
        </button>
        <button class="btn danger btn-sm" :title="$t('cloudDeleteRemote')" data-testid="cloud-sent-delete" @click="deleteSent(r)">⌫</button>
        <button class="btn ghost btn-sm" :title="$t('cloudForget')" @click="forgetSent(r)">✕</button>
      </div>
    </template>

    <TransferList />
  </section>
</template>

<style scoped>
.cl-title {
  margin-top: 14px;
  font-size: 11px;
  letter-spacing: 0.12em;
  opacity: 0.65;
}
.cl-acc {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 8px;
  margin-top: 6px;
  border: 1px solid rgba(128, 148, 160, 0.18);
  border-radius: 8px;
  font-size: 13px;
}
.cl-prov {
  flex: 0 0 auto;
  padding: 1px 6px;
  border-radius: 5px;
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.06em;
  background: rgba(45, 212, 167, 0.14);
}
.cl-prov[data-prov="gdrive"] {
  background: rgba(96, 165, 250, 0.16);
}
.cl-prov[data-prov="mailru"] {
  background: rgba(251, 146, 60, 0.16);
}
.cl-login {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.cl-quota {
  flex: 0 0 auto;
  font-size: 11px;
  opacity: 0.6;
}
.cl-note {
  font-size: 11px;
  opacity: 0.55;
}
.cl-select {
  flex: 1;
  min-width: 120px;
}
</style>
