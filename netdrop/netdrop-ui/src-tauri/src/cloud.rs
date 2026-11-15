// ============================================================
//  netdrop-ui // cloud.rs — Crypto Cloud: обмен файлами через
//  облачные диски (Yandex Disk / Google Drive / Mail.ru WebDAV),
//  когда прямая P2P-связь недоступна.
//
//  Схема: на диске создаётся папка .netdrop/<tid>/ с чанками
//  c0000, c0001… (≤100 МиБ каждый), каждый чанк E2E-шифруется
//  ChaCha20-Poly1305 (nonce = номер чанка, AAD = tid). Ключ живёт
//  ТОЛЬКО в тикете "ndc1…" — в облаке лежит мусор. Папка
//  публикуется (Yandex public_url / Google anyone-reader), поэтому
//  получателю аккаунт облака не нужен. Mail.ru WebDAV публичных
//  ссылок не умеет: приём возможен только с тем же аккаунтом.
// ============================================================

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

pub(crate) const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Размер чанка: NETDROP_CLOUD_CHUNK_MB задаётся при сборке (.env), ≤100 МиБ.
pub(crate) fn chunk_size() -> usize {
    option_env!("NETDROP_CLOUD_CHUNK_MB")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| (1..=100).contains(v))
        .unwrap_or(100)
        * 1024
        * 1024
}

/// Общий HTTP-клиент. Уроки ntfry: без read_timeout оборванный TCP висит
/// навсегда; pool_idle_timeout < серверного, иначе мёртвый keep-alive сокет.
pub(crate) fn http() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .read_timeout(std::time::Duration::from_secs(300))
            .tcp_keepalive(std::time::Duration::from_secs(20))
            .pool_idle_timeout(std::time::Duration::from_secs(25))
            .build()
            .expect("reqwest client")
    })
}

/// Авторетрай транзиентных сбоев облака: 3 повтора с паузами 1с/3с/7с.
pub(crate) async fn retry<T, F, Fut>(op: F) -> Result<T, String>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    const DELAYS_S: [u64; 3] = [1, 3, 7];
    let mut attempt = 0usize;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < DELAYS_S.len() => {
                eprintln!("cloud: retry {}/3 in {}s: {e}", attempt + 1, DELAYS_S[attempt]);
                tokio::time::sleep(std::time::Duration::from_secs(DELAYS_S[attempt])).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

pub(crate) async fn api_err(prefix: &str, r: reqwest::Response) -> String {
    let s = r.status().as_u16();
    let b = r.text().await.unwrap_or_default();
    format!("{prefix} {s}: {}", b.chars().take(200).collect::<String>())
}

// ---------- Тикет ndc1… ----------

#[derive(Serialize, Deserialize, Clone)]
struct CloudTicket {
    v: u8,
    /// провайдер: yandex | gdrive | mailru
    p: String,
    /// id передачи (.netdrop/<tid>/)
    t: String,
    /// имя файла/набора
    n: String,
    /// исходный размер, байт
    s: u64,
    /// число чанков
    c: u32,
    /// набор упакован в tar
    #[serde(default)]
    tar: bool,
    /// ключ E2E (base64url, 32 байта)
    k: String,
    /// публичная ссылка (Yandex)
    #[serde(default)]
    u: String,
    /// file id чанков по порядку (Google Drive)
    #[serde(default)]
    ids: Vec<String>,
}

const TICKET_PREFIX: &str = "ndc1";

fn encode_ticket(t: &CloudTicket) -> String {
    format!("{TICKET_PREFIX}{}", B64.encode(serde_json::to_vec(t).unwrap_or_default()))
}

fn decode_ticket(s: &str) -> Result<CloudTicket, String> {
    let s = s.trim();
    let s = s.strip_prefix("netdrop://").unwrap_or(s);
    let body = s.strip_prefix(TICKET_PREFIX).ok_or("not a cloud ticket (ndc1… expected)")?;
    let raw = B64.decode(body.trim()).map_err(|_| "ticket corrupted (base64)")?;
    serde_json::from_slice(&raw).map_err(|_| "ticket corrupted (format)".to_string())
}

// ---------- E2E шифрование чанков ----------

fn chunk_nonce(idx: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&idx.to_be_bytes());
    n
}

pub(crate) fn seal_chunk(key: &[u8; 32], tid: &str, idx: u64, plain: &[u8]) -> Result<Vec<u8>, String> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(Nonce::from_slice(&chunk_nonce(idx)), Payload { msg: plain, aad: tid.as_bytes() })
        .map_err(|_| "chunk encryption failed".to_string())
}

pub(crate) fn open_chunk(key: &[u8; 32], tid: &str, idx: u64, ct: &[u8]) -> Result<Vec<u8>, String> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(Nonce::from_slice(&chunk_nonce(idx)), Payload { msg: ct, aad: tid.as_bytes() })
        .map_err(|_| "chunk decryption failed — wrong key or corrupted data".to_string())
}

// ---------- Yandex Disk (REST) ----------

const YA_API: &str = "https://cloud-api.yandex.net/v1/disk";

pub(crate) fn ya_auth(token: &str) -> String {
    format!("OAuth {token}")
}

pub(crate) async fn ya_mkdir(token: &str, path: &str) -> Result<(), String> {
    let r = http()
        .put(format!("{YA_API}/resources"))
        .query(&[("path", path)])
        .header("Authorization", ya_auth(token))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    // 409 — папка уже существует
    if r.status().is_success() || r.status().as_u16() == 409 {
        Ok(())
    } else {
        Err(api_err("Yandex", r).await)
    }
}

pub(crate) async fn ya_upload(token: &str, path: &str, data: bytes::Bytes) -> Result<(), String> {
    let r = http()
        .get(format!("{YA_API}/resources/upload"))
        .query(&[("path", path), ("overwrite", "true")])
        .header("Authorization", ya_auth(token))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(api_err("Yandex", r).await);
    }
    let j: serde_json::Value = r.json().await.map_err(|e| e.to_string())?;
    let href = j["href"].as_str().ok_or("Yandex: no upload href")?;
    let r = http().put(href).body(data).send().await.map_err(|e| e.to_string())?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err(api_err("Yandex upload", r).await)
    }
}

pub(crate) async fn ya_publish(token: &str, path: &str) -> Result<String, String> {
    let r = http()
        .put(format!("{YA_API}/resources/publish"))
        .query(&[("path", path)])
        .header("Authorization", ya_auth(token))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(api_err("Yandex publish", r).await);
    }
    let r = http()
        .get(format!("{YA_API}/resources"))
        .query(&[("path", path), ("fields", "public_url")])
        .header("Authorization", ya_auth(token))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(api_err("Yandex", r).await);
    }
    let j: serde_json::Value = r.json().await.map_err(|e| e.to_string())?;
    j["public_url"].as_str().map(String::from).ok_or_else(|| "Yandex: no public_url".to_string())
}

/// Скачивание из публичной папки — БЕЗ авторизации (аккаунт не нужен).
pub(crate) async fn ya_public_download(public_key: &str, path: &str) -> Result<bytes::Bytes, String> {
    let r = http()
        .get(format!("{YA_API}/public/resources/download"))
        .query(&[("public_key", public_key), ("path", path)])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(api_err("Yandex public", r).await);
    }
    let j: serde_json::Value = r.json().await.map_err(|e| e.to_string())?;
    let href = j["href"].as_str().ok_or("Yandex: no download href")?;
    let r = http().get(href).send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(api_err("Yandex download", r).await);
    }
    r.bytes().await.map_err(|e| e.to_string())
}

pub(crate) async fn ya_delete(token: &str, path: &str) -> Result<(), String> {
    let r = http()
        .delete(format!("{YA_API}/resources"))
        .query(&[("path", path), ("permanently", "true")])
        .header("Authorization", ya_auth(token))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if r.status().is_success() || r.status().as_u16() == 404 {
        Ok(())
    } else {
        Err(api_err("Yandex delete", r).await)
    }
}

// ---------- Google Drive (REST v3, scope drive.file) ----------

const G_API: &str = "https://www.googleapis.com/drive/v3";
const G_UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";

pub(crate) async fn g_find_folder(token: &str, name: &str, parent: Option<&str>) -> Result<Option<String>, String> {
    let parent = parent.unwrap_or("root");
    let q = format!(
        "name = '{name}' and '{parent}' in parents and mimeType = 'application/vnd.google-apps.folder' and trashed = false"
    );
    let r = http()
        .get(format!("{G_API}/files"))
        .query(&[("q", q.as_str()), ("fields", "files(id)"), ("pageSize", "1")])
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(api_err("Google", r).await);
    }
    let j: serde_json::Value = r.json().await.map_err(|e| e.to_string())?;
    Ok(j["files"][0]["id"].as_str().map(String::from))
}

pub(crate) async fn g_create_folder(token: &str, name: &str, parent: Option<&str>) -> Result<String, String> {
    let mut body = json!({ "name": name, "mimeType": "application/vnd.google-apps.folder" });
    if let Some(p) = parent {
        body["parents"] = json!([p]);
    }
    let r = http()
        .post(format!("{G_API}/files"))
        .query(&[("fields", "id")])
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(api_err("Google mkdir", r).await);
    }
    let j: serde_json::Value = r.json().await.map_err(|e| e.to_string())?;
    j["id"].as_str().map(String::from).ok_or_else(|| "Google: no folder id".to_string())
}

pub(crate) async fn g_upload(token: &str, parent: &str, name: &str, data: bytes::Bytes) -> Result<String, String> {
    let boundary = "ndcloud7351boundary";
    let meta = json!({ "name": name, "parents": [parent] }).to_string();
    let mut body = Vec::with_capacity(data.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&data);
    body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
    let r = http()
        .post(G_UPLOAD)
        .query(&[("uploadType", "multipart"), ("fields", "id")])
        .header("Content-Type", format!("multipart/related; boundary={boundary}"))
        .bearer_auth(token)
        .body(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(api_err("Google upload", r).await);
    }
    let j: serde_json::Value = r.json().await.map_err(|e| e.to_string())?;
    j["id"].as_str().map(String::from).ok_or_else(|| "Google: no file id".to_string())
}

/// Обновление содержимого файла по id (id стабилен — нужен для index.json почты).
pub(crate) async fn g_update_media(token: &str, file_id: &str, data: bytes::Bytes) -> Result<(), String> {
    let r = http()
        .patch(format!("https://www.googleapis.com/upload/drive/v3/files/{file_id}"))
        .query(&[("uploadType", "media")])
        .header("Content-Type", "application/octet-stream")
        .bearer_auth(token)
        .body(data)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err(api_err("Google update", r).await)
    }
}

/// Доступ «всем, у кого есть ссылка» на папку передачи (наследуется чанками).
pub(crate) async fn g_anyone_reader(token: &str, file_id: &str) -> Result<(), String> {
    let r = http()
        .post(format!("{G_API}/files/{file_id}/permissions"))
        .bearer_auth(token)
        .json(&json!({ "role": "reader", "type": "anyone" }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err(api_err("Google share", r).await)
    }
}

/// Анонимное скачивание публичного файла (без confirm-страницы вирус-скана).
pub(crate) async fn g_public_download(file_id: &str) -> Result<bytes::Bytes, String> {
    let r = http()
        .get("https://drive.usercontent.google.com/download")
        .query(&[("id", file_id), ("export", "download"), ("confirm", "t")])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(api_err("Google download", r).await);
    }
    let ct = r
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if ct.contains("text/html") {
        return Err("Google: file is not public yet or was deleted".to_string());
    }
    r.bytes().await.map_err(|e| e.to_string())
}

pub(crate) async fn g_delete(token: &str, file_id: &str) -> Result<(), String> {
    let r = http()
        .delete(format!("{G_API}/files/{file_id}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if r.status().is_success() || r.status().as_u16() == 404 {
        Ok(())
    } else {
        Err(api_err("Google delete", r).await)
    }
}

// ---------- Mail.ru Облако (официальный WebDAV, Basic auth) ----------

const MR_WEBDAV: &str = "https://webdav.cloud.mail.ru";

pub(crate) fn mr_split(creds: &str) -> Result<(&str, &str), String> {
    creds
        .trim()
        .split_once(':')
        .filter(|(l, p)| !l.is_empty() && !p.is_empty())
        .ok_or_else(|| "Mail.ru: creds must be email:app_password".to_string())
}

pub(crate) async fn mr_mkcol(login: &str, pass: &str, path: &str) -> Result<(), String> {
    let m = reqwest::Method::from_bytes(b"MKCOL").expect("MKCOL");
    let r = http()
        .request(m, format!("{MR_WEBDAV}{path}"))
        .basic_auth(login, Some(pass))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    // 405 — коллекция уже существует
    if r.status().is_success() || r.status().as_u16() == 405 {
        Ok(())
    } else {
        Err(api_err("Mail.ru mkcol", r).await)
    }
}

pub(crate) async fn mr_put(login: &str, pass: &str, path: &str, data: bytes::Bytes) -> Result<(), String> {
    let r = http()
        .put(format!("{MR_WEBDAV}{path}"))
        .basic_auth(login, Some(pass))
        .body(data)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err(api_err("Mail.ru upload", r).await)
    }
}

pub(crate) async fn mr_get(login: &str, pass: &str, path: &str) -> Result<bytes::Bytes, String> {
    let r = http()
        .get(format!("{MR_WEBDAV}{path}"))
        .basic_auth(login, Some(pass))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(api_err("Mail.ru download", r).await);
    }
    r.bytes().await.map_err(|e| e.to_string())
}

pub(crate) async fn mr_delete(login: &str, pass: &str, path: &str) -> Result<(), String> {
    let r = http()
        .delete(format!("{MR_WEBDAV}{path}"))
        .basic_auth(login, Some(pass))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if r.status().is_success() || r.status().as_u16() == 404 {
        Ok(())
    } else {
        Err(api_err("Mail.ru delete", r).await)
    }
}

// ---------- OAuth (те же приложения, что и в NETFORY) ----------

const YA_CLIENT_ID: &str = "a21ba4730fb142a0bed238108b19db8e";
const YA_CLIENT_SECRET: &str = "d034bd943f914872972ead76ecd1a5a1";
const YA_OAUTH_WINDOW: &str = "yandex-oauth";

const G_CLIENT_ID: &str = "784003886336-m43ovr95oidf1sg0cbu1cpbulobbirb6.apps.googleusercontent.com";
const G_CLIENT_SECRET: &str = "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";
const G_SCOPE: &str = "https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.file";

/// PKCE (RFC 7636): (code_verifier, S256 code_challenge).
fn pkce_pair() -> (String, String) {
    use rand::RngCore;
    use sha2::{Digest, Sha256};
    let mut buf = [0u8; 48];
    rand::thread_rng().fill_bytes(&mut buf);
    let verifier = B64.encode(buf);
    let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OauthOut {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

async fn token_post(url: &str, form: &[(&str, &str)]) -> Result<OauthOut, String> {
    let j: serde_json::Value = http()
        .post(url)
        .form(form)
        .send()
        .await
        .map_err(|e| format!("token endpoint: {e}"))?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let token = j["access_token"]
        .as_str()
        .ok_or_else(|| format!("no access_token in response: {j}"))?
        .to_string();
    Ok(OauthOut {
        access_token: token,
        refresh_token: j["refresh_token"].as_str().unwrap_or("").to_string(),
        expires_in: j["expires_in"].as_u64().unwrap_or(0),
    })
}

fn open_browser(app: &AppHandle, url: &str) {
    use tauri_plugin_opener::OpenerExt;
    let _ = app.opener().open_url(url, None::<&str>);
}

/// Android: второе webview-окно не поддерживается — OAuth идёт через
/// системный браузер. start → {url, verifier}; пользователь копирует
/// код подтверждения со страницы Яндекса → exchange(code, verifier).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OauthStart {
    url: String,
    verifier: String,
}

#[tauri::command(async)]
pub(crate) fn cloud_yandex_auth_start() -> OauthStart {
    let client_id = std::env::var("NETDROP_YANDEX_CLIENT_ID").unwrap_or_else(|_| YA_CLIENT_ID.into());
    let (verifier, challenge) = pkce_pair();
    let url = format!(
        "https://oauth.yandex.ru/authorize?response_type=code&client_id={client_id}\
         &code_challenge={challenge}&code_challenge_method=S256"
    );
    OauthStart { url, verifier }
}

#[tauri::command]
pub(crate) async fn cloud_yandex_exchange(code: String, verifier: String) -> Result<OauthOut, String> {
    let client_id = std::env::var("NETDROP_YANDEX_CLIENT_ID").unwrap_or_else(|_| YA_CLIENT_ID.into());
    let client_secret =
        std::env::var("NETDROP_YANDEX_CLIENT_SECRET").unwrap_or_else(|_| YA_CLIENT_SECRET.into());
    token_post(
        "https://oauth.yandex.ru/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", code.trim()),
            ("client_id", &client_id),
            ("client_secret", &client_secret),
            ("code_verifier", &verifier),
        ],
    )
    .await
}

/// «Sign in with Yandex» (десктоп): встроенный webview, перехват verification_code.
#[tauri::command]
pub(crate) async fn cloud_yandex_oauth(app: AppHandle) -> Result<OauthOut, String> {
    #[cfg(target_os = "android")]
    {
        let _ = app;
        Err("Android: use cloud_yandex_auth_start".to_string())
    }
    #[cfg(not(target_os = "android"))]
    {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    let client_id = std::env::var("NETDROP_YANDEX_CLIENT_ID").unwrap_or_else(|_| YA_CLIENT_ID.into());
    let client_secret =
        std::env::var("NETDROP_YANDEX_CLIENT_SECRET").unwrap_or_else(|_| YA_CLIENT_SECRET.into());

    if let Some(w) = app.get_webview_window(YA_OAUTH_WINDOW) {
        let _ = w.close();
    }

    let (verifier, challenge) = pkce_pair();
    let auth_url = format!(
        "https://oauth.yandex.ru/authorize?response_type=code&client_id={client_id}\
         &code_challenge={challenge}&code_challenge_method=S256"
    )
    .parse()
    .map_err(|e| format!("bad url: {e}"))?;

    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let tx = std::sync::Mutex::new(Some(tx));

    let win = WebviewWindowBuilder::new(&app, YA_OAUTH_WINDOW, WebviewUrl::External(auth_url))
        .title("Sign in with Yandex — netdrop")
        .inner_size(480.0, 720.0)
        .on_navigation(move |url| {
            if url.host_str() == Some("oauth.yandex.ru") && url.path() == "/verification_code" {
                if let Some(code) = url
                    .query_pairs()
                    .find(|(k, _)| k == "code")
                    .map(|(_, v)| v.to_string())
                    .filter(|c| !c.is_empty())
                {
                    if let Some(tx) = tx.lock().unwrap().take() {
                        let _ = tx.send(code);
                    }
                    return false;
                }
            }
            true
        })
        .build()
        .map_err(|e| format!("failed to open auth window: {e}"))?;

    let code = match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
        Ok(Ok(c)) => c,
        Ok(Err(_)) => return Err("authorization cancelled (window closed)".into()),
        Err(_) => {
            let _ = win.close();
            return Err("authorization timeout (5 min) — try again".into());
        }
    };
    let _ = win.close();

    token_post(
        "https://oauth.yandex.ru/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("client_secret", &client_secret),
            ("code_verifier", &verifier),
        ],
    )
    .await
    }
}

#[tauri::command]
pub(crate) async fn cloud_yandex_refresh(refresh_token: String) -> Result<OauthOut, String> {
    let client_id = std::env::var("NETDROP_YANDEX_CLIENT_ID").unwrap_or_else(|_| YA_CLIENT_ID.into());
    let client_secret =
        std::env::var("NETDROP_YANDEX_CLIENT_SECRET").unwrap_or_else(|_| YA_CLIENT_SECRET.into());
    token_post(
        "https://oauth.yandex.ru/token",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token),
            ("client_id", &client_id),
            ("client_secret", &client_secret),
        ],
    )
    .await
}

/// «Sign in with Google»: loopback 127.0.0.1 + системный браузер
/// (Google запрещает OAuth во встроенных webview). Работает и на Android:
/// браузер устройства достучится до 127.0.0.1 приложения.
#[tauri::command]
pub(crate) async fn cloud_google_oauth(app: AppHandle) -> Result<OauthOut, String> {
    use tokio::net::TcpListener;

    let client_id = std::env::var("NETDROP_GDRIVE_CLIENT_ID").unwrap_or_else(|_| G_CLIENT_ID.into());
    let client_secret =
        std::env::var("NETDROP_GDRIVE_CLIENT_SECRET").unwrap_or_else(|_| G_CLIENT_SECRET.into());

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}");

    let (verifier, challenge) = pkce_pair();
    let auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={client_id}\
         &redirect_uri=http%3A%2F%2F127.0.0.1%3A{port}&response_type=code\
         &scope={G_SCOPE}&access_type=offline&prompt=consent\
         &code_challenge={challenge}&code_challenge_method=S256"
    );
    open_browser(&app, &auth_url);

    let code = tokio::time::timeout(std::time::Duration::from_secs(300), async {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { continue };
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.lines().next().unwrap_or("").split_whitespace().nth(1).unwrap_or("").to_string();
            let code = path
                .split("code=")
                .nth(1)
                .map(|s| s.split('&').next().unwrap_or("").to_string())
                .filter(|c| !c.is_empty())
                .map(|c| c.replace("%2F", "/").replace("%2f", "/"));
            let body = if code.is_some() {
                "<html><body style='font-family:monospace;background:#0a0e14;color:#2dd4a7;text-align:center;padding-top:120px'>\
                 <h2>netdrop &middot; Google Drive connected &#10003;</h2><p style='color:#888'>You can close this tab.</p></body></html>"
            } else {
                "<html><body style='font-family:monospace;background:#0a0e14;color:#ef4444;text-align:center;padding-top:120px'>\
                 <h2>Authorization failed</h2></body></html>"
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            if let Some(c) = code {
                return c;
            }
        }
    })
    .await
    .map_err(|_| "Google authorization timeout (5 min)".to_string())?;

    token_post(
        "https://oauth2.googleapis.com/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("client_secret", &client_secret),
            ("redirect_uri", &redirect),
            ("code_verifier", &verifier),
        ],
    )
    .await
}

#[tauri::command]
pub(crate) async fn cloud_google_refresh(refresh_token: String) -> Result<OauthOut, String> {
    let client_id = std::env::var("NETDROP_GDRIVE_CLIENT_ID").unwrap_or_else(|_| G_CLIENT_ID.into());
    let client_secret =
        std::env::var("NETDROP_GDRIVE_CLIENT_SECRET").unwrap_or_else(|_| G_CLIENT_SECRET.into());
    token_post(
        "https://oauth2.googleapis.com/token",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token),
            ("client_id", &client_id),
            ("client_secret", &client_secret),
        ],
    )
    .await
}

// ---------- Инфо об аккаунте (логин + квота) ----------

#[tauri::command]
pub(crate) async fn cloud_account_info(provider: String, token: String) -> Result<serde_json::Value, String> {
    match provider.as_str() {
        "yandex" => {
            let r = http()
                .get("https://login.yandex.ru/info?format=json")
                .header("Authorization", ya_auth(&token))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let login = if r.status().is_success() {
                let j: serde_json::Value = r.json().await.map_err(|e| e.to_string())?;
                j["login"].as_str().unwrap_or("").to_string()
            } else {
                String::new()
            };
            let r = http()
                .get(YA_API)
                .query(&[("fields", "total_space,used_space")])
                .header("Authorization", ya_auth(&token))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !r.status().is_success() {
                return Err(api_err("Yandex", r).await);
            }
            let j: serde_json::Value = r.json().await.map_err(|e| e.to_string())?;
            Ok(json!({
                "login": login,
                "total": j["total_space"].as_u64().unwrap_or(0),
                "used": j["used_space"].as_u64().unwrap_or(0),
            }))
        }
        "gdrive" => {
            let r = http()
                .get(format!("{G_API}/about"))
                .query(&[("fields", "user,storageQuota")])
                .bearer_auth(&token)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !r.status().is_success() {
                return Err(api_err("Google", r).await);
            }
            let j: serde_json::Value = r.json().await.map_err(|e| e.to_string())?;
            let parse = |v: &serde_json::Value| v.as_str().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
            Ok(json!({
                "login": j["user"]["emailAddress"].as_str().unwrap_or(""),
                "total": parse(&j["storageQuota"]["limit"]),
                "used": parse(&j["storageQuota"]["usage"]),
            }))
        }
        "mailru" => {
            let (login, pass) = mr_split(&token)?;
            let m = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND");
            let body = r#"<?xml version="1.0"?><D:propfind xmlns:D="DAV:"><D:prop><D:quota-available-bytes/><D:quota-used-bytes/></D:prop></D:propfind>"#;
            let r = http()
                .request(m, format!("{MR_WEBDAV}/"))
                .basic_auth(login, Some(pass))
                .header("Depth", "0")
                .header("Content-Type", "application/xml")
                .body(body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = r.status().as_u16();
            if status == 401 {
                return Err("Mail.ru: invalid email or app password".into());
            }
            if status == 402 || status == 403 {
                return Err("Mail.ru: WebDAV is available on paid Mail Space plans only".into());
            }
            if status >= 400 {
                return Err(api_err("Mail.ru", r).await);
            }
            let text = r.text().await.unwrap_or_default();
            let num = |tag: &str| -> u64 {
                text.split(tag)
                    .nth(1)
                    .map(|s| s.chars().skip_while(|c| *c == '>').take_while(|c| c.is_ascii_digit()).collect::<String>())
                    .and_then(|d| d.parse::<u64>().ok())
                    .unwrap_or(0)
            };
            let used = num("quota-used-bytes");
            let avail = num("quota-available-bytes");
            Ok(json!({ "login": login, "total": used + avail, "used": used }))
        }
        other => Err(format!("unknown provider '{other}'")),
    }
}

// ---------- Реестр отправленных передач (cloud_sent.json) ----------

fn sent_read(app: &AppHandle) -> Vec<serde_json::Value> {
    crate::data_file(app, "cloud_sent.json")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn sent_write(app: &AppHandle, list: &[serde_json::Value]) {
    if let Some(p) = crate::data_file(app, "cloud_sent.json") {
        let _ = std::fs::write(p, serde_json::to_string(list).unwrap_or_default());
    }
}

#[tauri::command(async)]
pub(crate) fn cloud_sent_list(app: AppHandle) -> Vec<serde_json::Value> {
    sent_read(&app)
}

/// Забыть запись локально (без удаления из облака).
#[tauri::command(async)]
pub(crate) fn cloud_sent_forget(app: AppHandle, tid: String) {
    let mut list = sent_read(&app);
    list.retain(|e| e["tid"].as_str() != Some(tid.as_str()));
    sent_write(&app, &list);
}

async fn delete_remote(provider: &str, token: &str, tid: &str, gfolder: &str) -> Result<(), String> {
    match provider {
        "yandex" => ya_delete(token, &format!("disk:/.netdrop/{tid}")).await,
        "gdrive" => {
            if gfolder.is_empty() {
                Ok(())
            } else {
                g_delete(token, gfolder).await
            }
        }
        "mailru" => {
            let (l, p) = mr_split(token)?;
            mr_delete(l, p, &format!("/.netdrop/{tid}")).await
        }
        other => Err(format!("unknown provider '{other}'")),
    }
}

/// Удалить передачу из облака (освобождает место, тикет перестаёт работать).
#[tauri::command]
pub(crate) async fn cloud_sent_delete(
    app: AppHandle,
    tid: String,
    provider: String,
    token: String,
    gfolder: String,
) -> Result<(), String> {
    delete_remote(&provider, &token, &tid, &gfolder).await?;
    cloud_sent_forget(app, tid);
    Ok(())
}

// ---------- Отправка / приём ----------

/// Флаг отмены, совместимый с общей командой cancel_transfer.
pub(crate) async fn cancel_flag(app: &AppHandle, id: u64) -> Arc<AtomicBool> {
    let state = app.state::<crate::UiState>();
    let notify = Arc::new(tokio::sync::Notify::new());
    state.cancels.lock().await.insert(id, notify.clone());
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    tauri::async_runtime::spawn(async move {
        notify.notified().await;
        f2.store(true, Ordering::Relaxed);
    });
    flag
}

pub(crate) fn build_tar(paths: &[PathBuf], dest: &Path) -> Result<(), String> {
    let f = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut b = tar::Builder::new(f);
    for p in paths {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let meta = std::fs::metadata(p).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            b.append_dir_all(&name, p).map_err(|e| e.to_string())?;
        } else {
            b.append_path_with_name(p, &name).map_err(|e| e.to_string())?;
        }
    }
    b.into_inner().map(|_| ()).map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn cloud_send_inner(
    provider: &str,
    token: &str,
    tid: &str,
    key: &[u8; 32],
    src: &Path,
    size: u64,
    chunks: u32,
    counter: &AtomicU64,
    cancelled: &AtomicBool,
    ids: &mut Vec<String>,
    gfolder: &mut String,
) -> Result<String, String> {
    // подготовка каталога .netdrop/<tid>/
    match provider {
        "yandex" => {
            ya_mkdir(token, "disk:/.netdrop").await?;
            ya_mkdir(token, &format!("disk:/.netdrop/{tid}")).await?;
        }
        "gdrive" => {
            let root = match g_find_folder(token, ".netdrop", None).await? {
                Some(id) => id,
                None => g_create_folder(token, ".netdrop", None).await?,
            };
            *gfolder = g_create_folder(token, tid, Some(&root)).await?;
        }
        "mailru" => {
            let (l, p) = mr_split(token)?;
            mr_mkcol(l, p, "/.netdrop").await?;
            mr_mkcol(l, p, &format!("/.netdrop/{tid}")).await?;
        }
        other => return Err(format!("unknown provider '{other}'")),
    }
    let gf = gfolder.clone();

    let mut f = tokio::fs::File::open(src).await.map_err(|e| e.to_string())?;
    let csz = chunk_size() as u64;
    for i in 0..chunks as u64 {
        if cancelled.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let want = std::cmp::min(csz, size - i * csz) as usize;
        let mut buf = vec![0u8; want];
        f.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
        // шифрование 100 МиБ — CPU-bound → вне async-потока
        let (k, t) = (*key, tid.to_string());
        let enc = tauri::async_runtime::spawn_blocking(move || seal_chunk(&k, &t, i, &buf))
            .await
            .map_err(|e| e.to_string())??;
        let data = bytes::Bytes::from(enc);
        let cname = format!("c{i:04}");
        match provider {
            "yandex" => {
                let p = format!("disk:/.netdrop/{tid}/{cname}");
                retry(|| ya_upload(token, &p, data.clone())).await?;
            }
            "gdrive" => {
                let fid = retry(|| g_upload(token, &gf, &cname, data.clone())).await?;
                ids.push(fid);
            }
            "mailru" => {
                let (l, pw) = mr_split(token)?;
                let p = format!("/.netdrop/{tid}/{cname}");
                retry(|| mr_put(l, pw, &p, data.clone())).await?;
            }
            _ => unreachable!(),
        }
        counter.fetch_add(want as u64, Ordering::Relaxed);
    }

    // публикация: получателю не нужен аккаунт (Yandex/Google)
    match provider {
        "yandex" => ya_publish(token, &format!("disk:/.netdrop/{tid}")).await,
        "gdrive" => {
            g_anyone_reader(token, &gf).await?;
            Ok(String::new())
        }
        _ => Ok(String::new()),
    }
}

/// Загрузка файлов в облако → тикет ndc1… (ссылка + E2E-ключ внутри).
#[tauri::command]
pub(crate) async fn cloud_send(
    app: AppHandle,
    state: State<'_, crate::UiState>,
    id: u64,
    provider: String,
    token: String,
    account: String,
    login: String,
    paths: Vec<String>,
) -> Result<serde_json::Value, String> {
    if paths.is_empty() {
        return Err("no paths selected".to_string());
    }
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    for p in &paths {
        if !tokio::fs::try_exists(p).await.unwrap_or(false) {
            return Err(format!("path not found: {}", p.display()));
        }
    }
    let single_file = paths.len() == 1
        && tokio::fs::metadata(&paths[0]).await.map(|m| m.is_file()).unwrap_or(false);
    let name = crate::bundle_name(&paths);

    // несколько файлов/папка → tar во временный файл
    let (src_path, tar_mode, tmp) = if single_file {
        (paths[0].clone(), false, None)
    } else {
        let cache = app.path().app_cache_dir().map_err(|e| e.to_string())?;
        tokio::fs::create_dir_all(&cache).await.ok();
        let tmp = cache.join(format!("ndcloud-{id}.tar"));
        let (t2, ps) = (tmp.clone(), paths.clone());
        tauri::async_runtime::spawn_blocking(move || build_tar(&ps, &t2))
            .await
            .map_err(|e| e.to_string())??;
        (tmp.clone(), true, Some(tmp))
    };
    let size = tokio::fs::metadata(&src_path).await.map_err(|e| e.to_string())?.len();
    let chunk = chunk_size() as u64;
    let chunks = size.div_ceil(chunk).max(1) as u32;

    let tid: String = {
        use rand::RngCore;
        let mut b = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut b);
        b.iter().map(|x| format!("{x:02x}")).collect()
    };
    let mut key = [0u8; 32];
    {
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut key);
    }

    let cancelled = cancel_flag(&app, id).await;
    crate::track_active(&app, id, format!("☁ {name}"));
    let counter = Arc::new(AtomicU64::new(0));
    let watcher = crate::watch_progress(app.clone(), id, counter.clone(), size);
    crate::emit(&app, json!({ "id": id, "kind": "accepted" }));

    let mut ids: Vec<String> = Vec::new();
    let mut gfolder = String::new();
    let res = cloud_send_inner(
        &provider, &token, &tid, &key, &src_path, size, chunks,
        &counter, &cancelled, &mut ids, &mut gfolder,
    )
    .await;

    watcher.abort();
    crate::untrack_active(&app, id);
    state.cancels.lock().await.remove(&id);
    if let Some(t) = tmp {
        let _ = tokio::fs::remove_file(t).await;
    }

    match res {
        Ok(url) => {
            let ticket = encode_ticket(&CloudTicket {
                v: 1,
                p: provider.clone(),
                t: tid.clone(),
                n: name.clone(),
                s: size,
                c: chunks,
                tar: tar_mode,
                k: B64.encode(key),
                u: url,
                ids: ids.clone(),
            });
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let mut list = sent_read(&app);
            list.insert(0, json!({
                "tid": tid, "provider": provider, "account": account, "login": login,
                "name": name, "size": size, "chunks": chunks, "ts": ts,
                "ticket": ticket, "gfolder": gfolder,
            }));
            sent_write(&app, &list);
            crate::emit(&app, json!({ "id": id, "kind": "done", "bytes": size, "total": size, "path": "" }));
            Ok(json!({
                "ticket": ticket,
                "qrSvg": crate::ticket_qr_svg(&ticket),
                "name": name, "size": size, "chunks": chunks, "tid": tid,
            }))
        }
        Err(e) => {
            // best-effort: подчищаем недокачанные чанки в облаке
            let _ = delete_remote(&provider, &token, &tid, &gfolder).await;
            if cancelled.load(Ordering::Relaxed) {
                crate::emit(&app, json!({ "id": id, "kind": "cancelled" }));
                Err("cancelled".into())
            } else {
                crate::emit(&app, json!({ "id": id, "kind": "error", "message": e.clone() }));
                Err(e)
            }
        }
    }
}

/// Источник чанков передачи (публичные ссылки / WebDAV).
pub(crate) enum ChunkSource {
    Yandex { url: String, prefix: String },
    Gdrive { ids: Vec<String> },
    Mailru { creds: String, base: String },
}

impl ChunkSource {
    pub(crate) async fn fetch(&self, i: u64) -> Result<bytes::Bytes, String> {
        let cname = format!("c{i:04}");
        match self {
            ChunkSource::Yandex { url, prefix } => {
                let path = format!("{prefix}/{cname}");
                retry(|| ya_public_download(url, &path)).await
            }
            ChunkSource::Gdrive { ids } => {
                let fid = ids.get(i as usize).ok_or("chunk id missing in manifest")?;
                retry(|| g_public_download(fid)).await
            }
            ChunkSource::Mailru { creds, base } => {
                let (l, p) = mr_split(creds)?;
                let path = format!("{base}/{cname}");
                retry(|| mr_get(l, p, &path)).await
            }
        }
    }
}

/// Скачивание шифрованной передачи с ДОКАЧКОЙ: рядом с .part живёт сайдкар
/// {tid, done}; при обрыве/отмене частичный файл сохраняется, при повторе
/// качаем со следующего чанка (чанки дешифруются независимо: nonce = индекс).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn download_encrypted(
    app: &AppHandle,
    id: u64,
    src: &ChunkSource,
    tid: &str,
    key: &[u8; 32],
    name: &str,
    chunks: u32,
    tar: bool,
    out_dir: &Path,
    counter: &AtomicU64,
    cancelled: &AtomicBool,
) -> Result<String, String> {
    let safe = crate::sanitize_name(name);
    let short: String = tid.chars().take(8).collect();
    let part = out_dir.join(format!("{safe}.{short}.ndcloud.part"));
    let sidecar = out_dir.join(format!("{safe}.{short}.ndcloud.part.json"));
    // докачка: сайдкар с тем же tid → продолжаем со следующего чанка
    let mut start: u64 = 0;
    if part.exists() {
        if let Ok(s) = std::fs::read_to_string(&sidecar) {
            if let Ok(j) = serde_json::from_str::<serde_json::Value>(&s) {
                if j["tid"].as_str() == Some(tid) {
                    start = j["done"].as_u64().unwrap_or(0).min(chunks as u64);
                }
            }
        }
    }
    let expected = start * chunk_size() as u64;
    if start > 0 {
        let plen = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
        if plen < expected {
            start = 0; // файл короче ожидаемого — качаем заново
        }
    }
    let mut f;
    if start > 0 {
        // срезаем возможный недописанный хвост чанка и встаём в конец
        let fh = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .await
            .map_err(|e| e.to_string())?;
        fh.set_len(expected).await.map_err(|e| e.to_string())?;
        f = fh;
        f.seek(std::io::SeekFrom::End(0)).await.map_err(|e| e.to_string())?;
        counter.store(expected, Ordering::Relaxed);
        crate::emit(app, json!({ "id": id, "kind": "resuming", "offset": expected }));
    } else {
        f = tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?;
        let _ = tokio::fs::remove_file(&sidecar).await;
    }
    for i in start..chunks as u64 {
        if cancelled.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let enc = src.fetch(i).await?;
        let (k, t, ev) = (*key, tid.to_string(), enc.to_vec());
        let plain = tauri::async_runtime::spawn_blocking(move || open_chunk(&k, &t, i, &ev))
            .await
            .map_err(|e| e.to_string())??;
        f.write_all(&plain).await.map_err(|e| e.to_string())?;
        f.flush().await.map_err(|e| e.to_string())?;
        counter.fetch_add(plain.len() as u64, Ordering::Relaxed);
        let _ = tokio::fs::write(&sidecar, json!({ "tid": tid, "done": i + 1 }).to_string()).await;
    }
    let _ = f.sync_all().await;
    drop(f);
    let _ = tokio::fs::remove_file(&sidecar).await;

    if tar {
        let (od, pp) = (out_dir.to_path_buf(), part.clone());
        tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
            let fh = std::fs::File::open(&pp).map_err(|e| e.to_string())?;
            let mut ar = tar::Archive::new(fh);
            ar.unpack(&od).map_err(|e| format!("unpack failed: {e}"))
        })
        .await
        .map_err(|e| e.to_string())??;
        let _ = tokio::fs::remove_file(&part).await;
        Ok(out_dir.to_string_lossy().to_string())
    } else {
        let dest = crate::unique_path(out_dir, name);
        tokio::fs::rename(&part, &dest).await.map_err(|e| e.to_string())?;
        Ok(dest.to_string_lossy().to_string())
    }
}

/// Скачивание по облачному тикету ndc1… (Yandex/Google — без аккаунта).
#[tauri::command]
pub(crate) async fn cloud_recv(
    app: AppHandle,
    state: State<'_, crate::UiState>,
    id: u64,
    ticket: String,
    out_dir: String,
    mailru_token: Option<String>,
) -> Result<serde_json::Value, String> {
    let t = decode_ticket(&ticket)?;
    let key_raw = B64.decode(t.k.trim()).map_err(|_| "ticket key corrupted")?;
    if key_raw.len() != 32 {
        return Err("ticket key has wrong length".into());
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&key_raw);

    let mut od = PathBuf::from(out_dir.trim());
    if od.as_os_str().is_empty() {
        od = PathBuf::from(crate::default_out_dir(app.clone()));
    }
    tokio::fs::create_dir_all(&od).await.ok();

    let src = match t.p.as_str() {
        "yandex" => {
            if t.u.is_empty() {
                return Err("ticket has no public link".into());
            }
            ChunkSource::Yandex { url: t.u.clone(), prefix: String::new() }
        }
        "gdrive" => ChunkSource::Gdrive { ids: t.ids.clone() },
        "mailru" => {
            let tok = mailru_token
                .clone()
                .ok_or("Mail.ru: connect the same Mail.ru account on the Cloud tab to receive")?;
            ChunkSource::Mailru { creds: tok, base: format!("/.netdrop/{}", t.t) }
        }
        other => return Err(format!("unknown provider '{other}'")),
    };

    let cancelled = cancel_flag(&app, id).await;
    crate::track_active(&app, id, format!("☁↓ {}", t.n));
    let counter = Arc::new(AtomicU64::new(0));
    let watcher = crate::watch_progress(app.clone(), id, counter.clone(), t.s);
    crate::emit(&app, json!({ "id": id, "kind": "connected" }));

    let res = download_encrypted(&app, id, &src, &t.t, &key, &t.n, t.c, t.tar, &od, &counter, &cancelled).await;

    watcher.abort();
    crate::untrack_active(&app, id);
    state.cancels.lock().await.remove(&id);

    match res {
        Ok(path) => {
            crate::emit(&app, json!({ "id": id, "kind": "done", "bytes": t.s, "total": t.s, "path": path }));
            Ok(json!({ "path": path, "name": t.n, "size": t.s }))
        }
        Err(e) => {
            if cancelled.load(Ordering::Relaxed) {
                crate::emit(&app, json!({ "id": id, "kind": "cancelled" }));
                Err("cancelled".into())
            } else {
                crate::emit(&app, json!({ "id": id, "kind": "error", "message": e.clone() }));
                Err(e)
            }
        }
    }
}

/// Лёгкий разбор тикета для UI (имя/размер/провайдер до скачивания).
#[tauri::command(async)]
pub(crate) fn cloud_ticket_info(ticket: String) -> Result<serde_json::Value, String> {
    let t = decode_ticket(&ticket)?;
    Ok(json!({ "provider": t.p, "name": t.n, "size": t.s, "chunks": t.c, "tid": t.t }))
}

// ---------- Тесты ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_roundtrip() {
        let t = CloudTicket {
            v: 1,
            p: "yandex".into(),
            t: "a1b2c3d4e5f60708".into(),
            n: "报告 report.pdf".into(),
            s: 123_456_789,
            c: 2,
            tar: false,
            k: B64.encode([7u8; 32]),
            u: "https://disk.yandex.ru/d/xxxx".into(),
            ids: vec![],
        };
        let s = encode_ticket(&t);
        assert!(s.starts_with(TICKET_PREFIX));
        let d = decode_ticket(&s).expect("decode");
        assert_eq!(d.n, t.n);
        assert_eq!(d.s, t.s);
        assert_eq!(d.u, t.u);
    }

    #[test]
    fn ticket_bad_prefix_rejected() {
        assert!(decode_ticket("nd1abc").is_err());
        assert!(decode_ticket("garbage").is_err());
    }

    #[test]
    fn chunk_seal_open_roundtrip() {
        let key = [9u8; 32];
        let ct = seal_chunk(&key, "tid01", 3, b"hello cloud").expect("seal");
        assert_eq!(open_chunk(&key, "tid01", 3, &ct).expect("open"), b"hello cloud".to_vec());
        // не тот индекс (nonce) → отказ
        assert!(open_chunk(&key, "tid01", 4, &ct).is_err());
        // не тот tid (AAD) → отказ
        assert!(open_chunk(&key, "tid02", 3, &ct).is_err());
    }

    #[test]
    fn creds_split() {
        assert!(mr_split("user@mail.ru:pa:ss").is_ok());
        assert!(mr_split("nopassword").is_err());
    }

    #[test]
    fn chunk_size_default_100mib() {
        assert_eq!(chunk_size(), 100 * 1024 * 1024);
    }
}
