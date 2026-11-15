// Аккаунты облаков (localStorage nd-cloud-accounts) + авто-рефреш OAuth.
// Общий модуль: используется страницей Cloud и глобальным store
// (автоудаление подтверждённых посылок почты доверенных).
import { invoke } from "@tauri-apps/api/core";

export function loadCloudAccounts() {
  try {
    return JSON.parse(localStorage.getItem("nd-cloud-accounts") || "[]");
  } catch {
    return [];
  }
}

export function saveCloudAccounts(list) {
  localStorage.setItem("nd-cloud-accounts", JSON.stringify(list));
}

// авто-рефреш OAuth-токена (за минуту до истечения); persist — колбэк сохранения
export async function freshCloudToken(acc, persist = null) {
  if (acc.provider === "mailru" || !acc.refresh) return acc.token;
  if (acc.expiresAt && Date.now() < acc.expiresAt - 60000) return acc.token;
  const cmd = acc.provider === "yandex" ? "cloud_yandex_refresh" : "cloud_google_refresh";
  const r = await invoke(cmd, { refreshToken: acc.refresh });
  acc.token = r.accessToken;
  if (r.refreshToken) acc.refresh = r.refreshToken;
  acc.expiresAt = Date.now() + (r.expiresIn || 3600) * 1000;
  if (persist) persist();
  return acc.token;
}
