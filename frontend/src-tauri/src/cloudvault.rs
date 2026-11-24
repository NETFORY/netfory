//! Crypto Vault (ntfry) — Tauri IPC-мост к Zero-Knowledge облачному
//! хранилищу. Сессия (открытый Vault) живёт в памяти процесса; мнемоника
//! по умолчанию берётся из SESSION_SEED кошелька SmartNet (если не задана
//! своя в настройках Vault). Прогресс push/pull стримится событием
//! `vault-progress` ({op, file, done, total}).

use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use serde::Serialize;
use tauri::Emitter;
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;

use ntfry::crypto as vcrypto;
use ntfry::storage::gdrive::GoogleDriveProvider;
use ntfry::storage::local::LocalStorageProvider;
use ntfry::storage::yandex::YandexDiskProvider;
use ntfry::storage::StorageProvider;
use ntfry::vault::Vault;

struct Session {
    vault: Vault,
    provider: String,
    account: String,
    address: String,
}

static VAULT: Lazy<Mutex<Option<Session>>> = Lazy::new(|| Mutex::new(None));

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatsOut {
    raw_bytes: u64,
    stored_bytes: u64,
    dedup_saved_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatusOut {
    provider: String,
    account: String,
    address: String,
    files: usize,
    stats: VaultStatsOut,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultRowOut {
    name: String,
    is_dir: bool,
    size: u64,
    modified: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultPushOut {
    size: u64,
    chunks: usize,
    uploaded: usize,
    deduped: usize,
    compressed_bytes: u64,
    stats: VaultStatsOut,
}

fn stats_out(v: &Vault) -> VaultStatsOut {
    let s = &v.index.stats;
    VaultStatsOut {
        raw_bytes: s.raw_bytes,
        stored_bytes: s.stored_bytes,
        dedup_saved_bytes: s.dedup_saved_bytes,
    }
}

fn status_out(s: &Session) -> VaultStatusOut {
    VaultStatusOut {
        provider: s.provider.clone(),
        account: s.account.clone(),
        address: s.address.clone(),
        files: s.vault.index.files.len(),
        stats: stats_out(&s.vault),
    }
}

/// Имя каталога локального Sled-кэша: у каждого аккаунта — свой кэш.
fn cache_name(provider: &str, account: Option<&str>) -> String {
    match account.filter(|a| !a.trim().is_empty()) {
        Some(a) => format!("{provider}-{}", a.trim()),
        None => provider.to_string(),
    }
}

fn make_provider(
    base: &Path,
    provider: &str,
    token: Option<&str>,
) -> Result<Box<dyn StorageProvider>, String> {
    match provider {
        // Local Safe Disk: token = путь выбранной папки (в ней создаётся /ntfry
        // с зашифрованными чанками). Пустой token — legacy mock в data dir.
        "local" => match token.map(str::trim).filter(|t| !t.is_empty()) {
            Some(dir) => Ok(Box::new(LocalStorageProvider::new(PathBuf::from(dir)))),
            None => Ok(Box::new(LocalStorageProvider::new(base.join("ntfry").join("mock_cloud")))),
        },
        "yandex" => {
            let t = token.filter(|t| !t.trim().is_empty()).ok_or("нужен OAuth-токен Yandex Disk")?;
            Ok(Box::new(YandexDiskProvider::new(t.trim().to_string())))
        }
        "gdrive" => {
            let t = token.filter(|t| !t.trim().is_empty()).ok_or("нужен OAuth-токен Google Drive")?;
            Ok(Box::new(GoogleDriveProvider::new(t.trim().to_string())))
        }
        // Облако Mail.ru: официальный WebDAV, token = "email:пароль_приложения"
        // (OAuth/REST API у облака Mail.ru для сторонних приложений нет).
        "mailru" => {
            let t = token
                .filter(|t| !t.trim().is_empty())
                .ok_or("нужны креды Mail.ru: email:пароль_приложения")?;
            let (login, pass) =
                ntfry::storage::mailru::split_creds(t).map_err(|e| e.to_string())?;
            Ok(Box::new(ntfry::storage::mailru::MailruCloudProvider::new(
                login.to_string(),
                pass.to_string(),
            )))
        }
        // OneDrive: десктоп-клиент Microsoft уже синхронизирует локальную папку —
        // token = путь к ней, работаем как с локальным диском (без API/OAuth).
        "onedrive" => match token.map(str::trim).filter(|t| !t.is_empty()) {
            Some(dir) => Ok(Box::new(LocalStorageProvider::new(PathBuf::from(dir)))),
            None => Err("нужен путь к папке OneDrive".into()),
        },
        other => Err(format!("неизвестный провайдер '{other}'")),
    }
}

/// Открывает Vault. Пустая мнемоника → секрет текущего кошелька SmartNet.
/// `account` — id аккаунта из настроек UI (мульти-диск): свой Sled-кэш.
#[tauri::command]
pub(crate) async fn cloud_vault_open(
    app: tauri::AppHandle,
    provider: String,
    mnemonic: Option<String>,
    token: Option<String>,
    account: Option<String>,
) -> Result<VaultStatusOut, String> {
    let secret = match mnemonic.filter(|m| !m.trim().is_empty()) {
        Some(m) => m,
        None => crate::SESSION_SEED
            .lock()
            .unwrap()
            .clone()
            .ok_or("кошелёк заблокирован — разблокируйте SmartNet или задайте свою мнемонику в настройках Vault")?,
    };
    // BIP32/HKDF + secp256k1 — CPU-bound → spawn_blocking, чтобы не морозить
    // IPC-тред при переключении дисков.
    let km = tauri::async_runtime::spawn_blocking(move || vcrypto::derive_flexible(&secret))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let base = crate::ensure_data_dir(&app);
    let storage = make_provider(&base, &provider, token.as_deref())?;
    let cache_dir = base
        .join("ntfry")
        .join("cache")
        .join(cache_name(&provider, account.as_deref()));
    // КРИТИЧНО: берём мьютекс сессии ДО открытия Sled и дропаем старую
    // сессию — иначе второй хэндл кэша ловит file-lock (баг «could not
    // acquire lock on vault_index.sled» при повторном Connect).
    let mut guard = VAULT.lock().await;
    *guard = None;
    let vault = Vault::open(km.master_key, storage, &cache_dir, None)
        .await
        .map_err(|e| e.to_string())?;
    crate::syslog::log(
        "vault",
        format!("open provider={provider} files={} addr={}", vault.index.files.len(), km.address),
    );
    let session = Session { vault, provider, account: account.unwrap_or_default(), address: km.address };
    let out = status_out(&session);
    *guard = Some(session);
    Ok(out)
}

#[tauri::command]
pub(crate) async fn cloud_vault_close() {
    *VAULT.lock().await = None;
}

/// Автопоиск стандартной локальной папки OneDrive (десктоп-клиент Microsoft
/// уже её синхронизирует). Проверяем переменные окружения %OneDrive*% на
/// Windows и типовые пути `~/OneDrive`, `~/OneDrive - <tenant>` на всех ОС.
/// Возвращает первую существующую папку или None.
#[tauri::command]
pub(crate) async fn detect_onedrive_folder() -> Option<String> {
    // Файловые операции (env + read_dir) — в blocking-таске, чтобы не
    // блокировать IPC-тред (правило: все команды async и не блокируют main).
    tauri::async_runtime::spawn_blocking(|| detect_onedrive_folder_blocking())
        .await
        .ok()
        .flatten()
}

fn detect_onedrive_folder_blocking() -> Option<String> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // Windows: официальные переменные, которые ставит OneDrive-клиент.
    for var in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Ok(v) = std::env::var(var) {
            let p = PathBuf::from(v);
            if !p.as_os_str().is_empty() {
                candidates.push(p);
            }
        }
    }

    // Домашняя директория: ~/OneDrive и ~/OneDrive - <tenant>.
    let home: Option<PathBuf> = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    if let Some(h) = home {
        candidates.push(h.join("OneDrive"));
        if let Ok(rd) = std::fs::read_dir(&h) {
            for e in rd.flatten() {
                let name = e.file_name();
                let s = name.to_string_lossy();
                if s.starts_with("OneDrive - ") {
                    candidates.push(e.path());
                }
            }
        }
    }

    candidates
        .into_iter()
        .find(|p| p.is_dir())
        .map(|p| p.to_string_lossy().to_string())
}

/// Статус открытой сессии (None — Vault закрыт).
#[tauri::command]
pub(crate) async fn cloud_vault_status() -> Option<VaultStatusOut> {
    VAULT.lock().await.as_ref().map(status_out)
}

/// Листинг VFS-каталога из In-Memory индекса (мгновенно, без сети).
#[tauri::command]
pub(crate) async fn cloud_vault_ls(path: String) -> Result<Vec<VaultRowOut>, String> {
    let guard = VAULT.lock().await;
    let s = guard.as_ref().ok_or("vault не открыт")?;
    Ok(s.vault
        .index
        .ls(&path)
        .into_iter()
        .map(|r| VaultRowOut { name: r.name, is_dir: r.is_dir, size: r.size, modified: r.modified })
        .collect())
}

/// PUSH локального файла в VFS-каталог. Прогресс — событие `vault-progress`.
#[tauri::command]
pub(crate) async fn cloud_vault_push(
    app: tauri::AppHandle,
    src_path: String,
    dst_dir: String,
) -> Result<VaultPushOut, String> {
    let src = PathBuf::from(&src_path);
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("некорректный путь файла")?
        .to_string();
    let dst = format!("{}/{}", dst_dir.trim_end_matches('/'), name);
    let mut guard = VAULT.lock().await;
    let s = guard.as_mut().ok_or("vault не открыт")?;
    let ah = app.clone();
    let file = name.clone();
    let cb = move |done: u64, total: u64| {
        let _ = ah.emit(
            "vault-progress",
            serde_json::json!({ "op": "push", "file": file, "done": done, "total": total }),
        );
    };
    let st = s.vault.push(&src, &dst, Some(&cb)).await.map_err(|e| e.to_string())?;
    crate::syslog::log(
        "vault",
        format!("push {dst} · {} chunks ({} up, {} dedup)", st.chunks, st.uploaded, st.deduped),
    );
    Ok(VaultPushOut {
        size: st.size,
        chunks: st.chunks,
        uploaded: st.uploaded,
        deduped: st.deduped,
        compressed_bytes: st.compressed_bytes,
        stats: stats_out(&s.vault),
    })
}

/// PULL файла из VFS через нативный диалог «Сохранить как». false — отмена.
#[tauri::command]
pub(crate) async fn cloud_vault_pull(
    app: tauri::AppHandle,
    src_vfs: String,
) -> Result<bool, String> {
    let default_name = src_vfs.rsplit('/').next().unwrap_or("file").to_string();
    let app2 = app.clone();
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        app2.dialog().file().set_file_name(&default_name).blocking_save_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(dst) = chosen.and_then(|fp| fp.into_path().ok()) else {
        return Ok(false);
    };
    let guard = VAULT.lock().await;
    let s = guard.as_ref().ok_or("vault не открыт")?;
    let ah = app.clone();
    let file = src_vfs.clone();
    let cb = move |done: u64, total: u64| {
        let _ = ah.emit(
            "vault-progress",
            serde_json::json!({ "op": "pull", "file": file, "done": done, "total": total }),
        );
    };
    s.vault.pull(&src_vfs, &dst, Some(&cb)).await.map_err(|e| e.to_string())?;
    crate::syslog::log("vault", format!("pull {src_vfs} → {}", dst.display()));
    Ok(true)
}


// ── «Sign in with Yandex»: OAuth без сервера и без копирования ────────────
// Redirect URI приложения прибит к oauth.yandex.ru/verification_code
// («Нельзя изменить»), поэтому loopback/deep-link невозможны. Вместо этого
// открываем страницу согласия во встроенном Tauri-webview и перехватываем
// навигацию на verification_code?code=… — код забирается автоматически,
// затем меняется на access_token. Пользователь только жмёт «Разрешить».

const YA_CLIENT_ID: &str = "a21ba4730fb142a0bed238108b19db8e";
const YA_CLIENT_SECRET: &str = "d034bd943f914872972ead76ecd1a5a1";
const YA_OAUTH_WINDOW: &str = "yandex-oauth";

/// PKCE (RFC 7636): (code_verifier, S256 code_challenge).
/// Защищает authorization code от перехвата на loopback/webview.
fn pkce_pair() -> (String, String) {
    use base64::Engine;
    use rand::RngCore;
    use sha2::{Digest, Sha256};
    let mut buf = [0u8; 48];
    rand::thread_rng().fill_bytes(&mut buf);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
    let challenge =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YandexOauthOut {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

#[tauri::command]
pub(crate) async fn cloud_vault_yandex_oauth(app: tauri::AppHandle) -> Result<YandexOauthOut, String> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

    let client_id = std::env::var("NTFRY_YANDEX_CLIENT_ID").unwrap_or_else(|_| YA_CLIENT_ID.into());
    let client_secret =
        std::env::var("NTFRY_YANDEX_CLIENT_SECRET").unwrap_or_else(|_| YA_CLIENT_SECRET.into());

    // Прибираем возможное зависшее окно от прошлой попытки
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

    // Одноразовый канал: код прилетит из колбэка навигации webview
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let tx = std::sync::Mutex::new(Some(tx));

    let win = WebviewWindowBuilder::new(&app, YA_OAUTH_WINDOW, WebviewUrl::External(auth_url))
        .title("Sign in with Yandex — SmartNet")
        .inner_size(480.0, 720.0)
        .center()
        .on_navigation(move |url| {
            // Яндекс после согласия ведёт на verification_code?code=XXXXX —
            // код есть прямо в URL, страницу юзеру показывать уже не нужно.
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
                    return false; // навигацию дальше не пускаем — окно закроется
                }
            }
            true
        })
        .build()
        .map_err(|e| format!("не удалось открыть окно авторизации: {e}"))?;
    crate::syslog::log("vault", "yandex oauth: authorization window opened, waiting for consent…");

    // Ждём код (5 мин). Если юзер закрыл окно — tx дропнется и rx вернёт Err.
    let code = match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
        Ok(Ok(c)) => c,
        Ok(Err(_)) => {
            return Err("авторизация отменена (окно закрыто)".into());
        }
        Err(_) => {
            let _ = win.close();
            return Err("таймаут авторизации (5 мин) — попробуйте ещё раз".into());
        }
    };
    let _ = win.close();

    // Обмен code → access_token (ureq, блокирующий — в отдельном потоке).
    // PKCE: передаём code_verifier — Яндекс сверит его с code_challenge.
    let (cid, csec) = (client_id, client_secret);
    let json: serde_json::Value = tauri::async_runtime::spawn_blocking(move || {
        ureq::post("https://oauth.yandex.ru/token")
            .send_form(&[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("client_id", &cid),
                ("client_secret", &csec),
                ("code_verifier", &verifier),
            ])
            .map_err(|e| format!("обмен кода не удался: {e}"))
            .and_then(|r| r.into_string().map_err(|e| e.to_string()))
            .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
    })
    .await
    .map_err(|e| e.to_string())??;

    let token = json["access_token"].as_str().ok_or("нет access_token в ответе Яндекса")?.to_string();
    let refresh = json["refresh_token"].as_str().unwrap_or("").to_string();
    let expires_in = json["expires_in"].as_u64().unwrap_or(0);
    crate::syslog::log("vault", format!("yandex oauth: token received (expires_in={expires_in}s)"));
    Ok(YandexOauthOut { access_token: token, refresh_token: refresh, expires_in })
}

/// Полный сброс облачного vault: удаляет /ntfry на провайдере, чистит
/// локальный кэш и пересоздаёт пустой vault под ключ ТЕКУЩЕГО кошелька.
/// Используется, когда облачный индекс зашифрован другим ключом.
#[tauri::command]
pub(crate) async fn cloud_vault_reset(
    app: tauri::AppHandle,
    provider: String,
    token: Option<String>,
    account: Option<String>,
) -> Result<VaultStatusOut, String> {
    let secret = crate::SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("кошелёк заблокирован — разблокируйте SmartNet")?;
    let km = tauri::async_runtime::spawn_blocking(move || vcrypto::derive_flexible(&secret))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let base = crate::ensure_data_dir(&app);
    let storage = make_provider(&base, &provider, token.as_deref())?;
    storage.delete_vault().await.map_err(|e| e.to_string())?;
    let cache_dir = base
        .join("ntfry")
        .join("cache")
        .join(cache_name(&provider, account.as_deref()));
    let _ = std::fs::remove_dir_all(&cache_dir);
    let vault = Vault::init(km.master_key, storage, &cache_dir, None)
        .await
        .map_err(|e| e.to_string())?;
    crate::syslog::log("vault", format!("RESET provider={provider} — cloud /ntfry re-created for current wallet"));
    let session = Session { vault, provider, account: account.unwrap_or_default(), address: km.address };
    let out = status_out(&session);
    *VAULT.lock().await = Some(session);
    Ok(out)
}

/// Создать (пустой) каталог в VFS.
#[tauri::command]
pub(crate) async fn cloud_vault_mkdir(path: String) -> Result<(), String> {
    let mut guard = VAULT.lock().await;
    let s = guard.as_mut().ok_or("vault не открыт")?;
    s.vault.mkdir(&path).await.map_err(|e| e.to_string())
}

/// Удалить файл/каталог (рекурсивно) + GC осиротевших чанков в облаке.
#[tauri::command]
pub(crate) async fn cloud_vault_delete(path: String) -> Result<usize, String> {
    let mut guard = VAULT.lock().await;
    let s = guard.as_mut().ok_or("vault не открыт")?;
    let n = s.vault.delete(&path).await.map_err(|e| e.to_string())?;
    crate::syslog::log("vault", format!("delete {path} ({n} files) + chunk GC"));
    Ok(n)
}

/// Обновление Yandex-токена по refresh_token (авто-рефреш без браузера).
#[tauri::command]
pub(crate) async fn cloud_vault_yandex_refresh(refresh_token: String) -> Result<YandexOauthOut, String> {
    let client_id = std::env::var("NTFRY_YANDEX_CLIENT_ID").unwrap_or_else(|_| YA_CLIENT_ID.into());
    let client_secret =
        std::env::var("NTFRY_YANDEX_CLIENT_SECRET").unwrap_or_else(|_| YA_CLIENT_SECRET.into());
    let json: serde_json::Value = tauri::async_runtime::spawn_blocking(move || {
        ureq::post("https://oauth.yandex.ru/token")
            .send_form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh_token),
                ("client_id", &client_id),
                ("client_secret", &client_secret),
            ])
            .map_err(|e| format!("refresh не удался: {e}"))
            .and_then(|r| r.into_string().map_err(|e| e.to_string()))
            .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
    })
    .await
    .map_err(|e| e.to_string())??;
    let token = json["access_token"].as_str().ok_or("нет access_token")?.to_string();
    let refresh = json["refresh_token"].as_str().unwrap_or("").to_string();
    let expires_in = json["expires_in"].as_u64().unwrap_or(0);
    crate::syslog::log("vault", "yandex oauth: token refreshed via refresh_token");
    Ok(YandexOauthOut { access_token: token, refresh_token: refresh, expires_in })
}

// ── «Sign in with Google»: loopback-flow (Google запрещает webview) ───────
// Для desktop-приложений Google разрешает redirect на 127.0.0.1 с ЛЮБЫМ
// портом без регистрации — биндим эфемерный порт, открываем системный
// браузер, ловим ?code=, меняем на токены (access + refresh).

const G_CLIENT_ID: &str = "784003886336-m43ovr95oidf1sg0cbu1cpbulobbirb6.apps.googleusercontent.com";
const G_CLIENT_SECRET: &str = "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";
const G_SCOPE: &str = "https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.file";

fn google_token_request(form: Vec<(String, String)>) -> Result<YandexOauthOut, String> {
    let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let json: serde_json::Value = ureq::post("https://oauth2.googleapis.com/token")
        .send_form(&pairs)
        .map_err(|e| format!("google token endpoint: {e}"))
        .and_then(|r| r.into_string().map_err(|e| e.to_string()))
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))?;
    let token = json["access_token"].as_str().ok_or("нет access_token в ответе Google")?.to_string();
    let refresh = json["refresh_token"].as_str().unwrap_or("").to_string();
    let expires_in = json["expires_in"].as_u64().unwrap_or(0);
    Ok(YandexOauthOut { access_token: token, refresh_token: refresh, expires_in })
}

#[tauri::command]
pub(crate) async fn cloud_vault_google_oauth(app: tauri::AppHandle) -> Result<YandexOauthOut, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let client_id = std::env::var("NTFRY_GDRIVE_CLIENT_ID").unwrap_or_else(|_| G_CLIENT_ID.into());
    let client_secret =
        std::env::var("NTFRY_GDRIVE_CLIENT_SECRET").unwrap_or_else(|_| G_CLIENT_SECRET.into());

    // эфемерный порт: Google разрешает loopback без регистрации порта
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}");

    let (verifier, challenge) = pkce_pair();
    let auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={client_id}\
         &redirect_uri=http%3A%2F%2F127.0.0.1%3A{port}&response_type=code\
         &scope={G_SCOPE}&access_type=offline&prompt=consent\
         &code_challenge={challenge}&code_challenge_method=S256"
    );
    {
        use tauri_plugin_shell::ShellExt;
        app.shell().open(&auth_url, None).map_err(|e| e.to_string())?;
    }
    crate::syslog::log("vault", format!("google oauth: browser opened, loopback 127.0.0.1:{port}"));

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
                // Google URL-кодирует код (например %2F) — декодируем базово
                .map(|c| c.replace("%2F", "/").replace("%2f", "/"));
            let body = if code.is_some() {
                "<html><body style='font-family:monospace;background:#0a0a0f;color:#22c55e;text-align:center;padding-top:120px'>\
                 <h2>SmartNet · Google Drive connected &#10003;</h2><p style='color:#888'>Вкладку можно закрыть.</p></body></html>"
            } else {
                "<html><body style='font-family:monospace;background:#0a0a0f;color:#ef4444;text-align:center;padding-top:120px'>\
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
    .map_err(|_| "таймаут авторизации Google (5 мин)".to_string())?;

    let out = tauri::async_runtime::spawn_blocking(move || {
        google_token_request(vec![
            ("grant_type".into(), "authorization_code".into()),
            ("code".into(), code),
            ("client_id".into(), client_id),
            ("client_secret".into(), client_secret),
            ("redirect_uri".into(), redirect),
            ("code_verifier".into(), verifier),
        ])
    })
    .await
    .map_err(|e| e.to_string())??;
    crate::syslog::log("vault", format!("google oauth: token received (expires_in={}s)", out.expires_in));
    Ok(out)
}

/// Обновление Google-токена по refresh_token.
#[tauri::command]
pub(crate) async fn cloud_vault_google_refresh(refresh_token: String) -> Result<YandexOauthOut, String> {
    let client_id = std::env::var("NTFRY_GDRIVE_CLIENT_ID").unwrap_or_else(|_| G_CLIENT_ID.into());
    let client_secret =
        std::env::var("NTFRY_GDRIVE_CLIENT_SECRET").unwrap_or_else(|_| G_CLIENT_SECRET.into());
    let out = tauri::async_runtime::spawn_blocking(move || {
        google_token_request(vec![
            ("grant_type".into(), "refresh_token".into()),
            ("refresh_token".into(), refresh_token),
            ("client_id".into(), client_id),
            ("client_secret".into(), client_secret),
        ])
    })
    .await
    .map_err(|e| e.to_string())??;
    crate::syslog::log("vault", "google oauth: token refreshed via refresh_token");
    Ok(out)
}

/// Квота диска провайдера (total/used, байты). None — провайдер не отдаёт.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaOut {
    total_bytes: u64,
    used_bytes: u64,
}

#[tauri::command]
pub(crate) async fn cloud_vault_quota() -> Result<Option<QuotaOut>, String> {
    let guard = VAULT.lock().await;
    let s = guard.as_ref().ok_or("vault не открыт")?;
    Ok(s.vault.quota().await.map(|(t, u)| QuotaOut { total_bytes: t, used_bytes: u }))
}

/// Статус ZSTD-словаря vault.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictStatusOut {
    present: bool,
    files_using_dict: usize,
}

#[tauri::command]
pub(crate) async fn cloud_vault_dict_status() -> Result<DictStatusOut, String> {
    let guard = VAULT.lock().await;
    let s = guard.as_ref().ok_or("vault не открыт")?;
    Ok(DictStatusOut {
        present: s.vault.has_dict(),
        files_using_dict: s.vault.index.files.values().filter(|e| e.dict).count(),
    })
}

/// Обучение ZSTD-словаря (COVER) на файлах выбранной папки → /ntfry/user.dict.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictTrainOut {
    size: u64,
    samples: usize,
}

#[tauri::command]
pub(crate) async fn cloud_vault_dict_train(app: tauri::AppHandle) -> Result<Option<DictTrainOut>, String> {
    // нативный выбор папки с сэмплами
    let app2 = app.clone();
    let folder = tauri::async_runtime::spawn_blocking(move || {
        app2.dialog().file().blocking_pick_folder()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(dir) = folder.and_then(|f| f.into_path().ok()) else {
        return Ok(None);
    };
    // сэмплы: до 256 файлов ≤1МБ (только в RAM, на диск не пишем)
    let samples: Vec<Vec<u8>> = tauri::async_runtime::spawn_blocking(move || {
        let mut out: Vec<Vec<u8>> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten().take(256) {
                let p = entry.path();
                if p.is_file() {
                    if let Ok(m) = p.metadata() {
                        if m.len() <= 1024 * 1024 {
                            if let Ok(d) = std::fs::read(&p) {
                                if !d.is_empty() {
                                    out.push(d);
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    })
    .await
    .map_err(|e| e.to_string())?;
    if samples.len() < 8 {
        return Err(format!("мало сэмплов ({}) — нужно ≥8 небольших файлов в папке", samples.len()));
    }
    let n = samples.len();
    let dict = tauri::async_runtime::spawn_blocking(move || Vault::train_dict(&samples, 65536))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let size = dict.len() as u64;
    let mut guard = VAULT.lock().await;
    let s = guard.as_mut().ok_or("vault не открыт")?;
    s.vault.set_dict(dict).await.map_err(|e| e.to_string())?;
    crate::syslog::log("vault", format!("zstd dict trained: {n} samples, {size} bytes → /ntfry/user.dict"));
    Ok(Some(DictTrainOut { size, samples: n }))
}

/// Шифрование произвольной строки мастер-ключом кошелька (для хранения
/// OAuth-токенов в localStorage только в зашифрованном виде).
#[tauri::command]
pub(crate) async fn cloud_vault_seal(plain: String) -> Result<String, String> {
    let secret = crate::SESSION_SEED.lock().unwrap().clone().ok_or("кошелёк заблокирован")?;
    // BIP32/HKDF + ChaCha20-Poly1305 — CPU-bound → spawn_blocking.
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let km = vcrypto::derive_flexible(&secret).map_err(|e| e.to_string())?;
        let enc = vcrypto::encrypt(&km.master_key, plain.as_bytes()).map_err(|e| e.to_string())?;
        Ok(hex::encode(enc))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn cloud_vault_unseal(sealed: String) -> Result<String, String> {
    let secret = crate::SESSION_SEED.lock().unwrap().clone().ok_or("кошелёк заблокирован")?;
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let km = vcrypto::derive_flexible(&secret).map_err(|e| e.to_string())?;
        let data = hex::decode(sealed.trim()).map_err(|e| e.to_string())?;
        let plain = vcrypto::decrypt(&km.master_key, &data).map_err(|e| e.to_string())?;
        String::from_utf8(plain).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Логин и квота аккаунта провайдера по OAuth-токену (для списка дисков
/// в UI мульти-аккаунта, без открытия vault).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfoOut {
    login: String,
    total_bytes: u64,
    used_bytes: u64,
}

/// Local Safe Disk: login = путь папки, квота = раздел диска, где она лежит
/// (mount point с самым длинным совпадением префикса, как в apps::data_disk_space).
fn local_disk_info(dir: &Path) -> Result<ntfry::storage::AccountInfo, String> {
    if !dir.is_dir() {
        return Err(format!("папка не найдена: {}", dir.display()));
    }
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut best: Option<(usize, u64, u64)> = None;
    for disk in &disks {
        let mp = disk.mount_point();
        if dir.starts_with(mp) {
            let len = mp.as_os_str().len();
            if best.map_or(true, |(b, _, _)| len > b) {
                best = Some((len, disk.total_space(), disk.available_space()));
            }
        }
    }
    let (total, avail) = best.map(|(_, t, a)| (t, a)).unwrap_or((0, 0));
    Ok(ntfry::storage::AccountInfo {
        login: dir.display().to_string(),
        total,
        used: total.saturating_sub(avail),
    })
}

#[tauri::command]
pub(crate) async fn cloud_vault_account_info(
    provider: String,
    token: String,
) -> Result<AccountInfoOut, String> {
    let t = token.trim();
    if t.is_empty() {
        return Err(match provider.as_str() {
            "local" => "нужен путь к папке Local Safe Disk".into(),
            "onedrive" => "нужен путь к папке OneDrive".into(),
            "mailru" => "нужны креды Mail.ru: email:пароль_приложения".into(),
            _ => "нужен OAuth-токен".to_string(),
        });
    }
    let info = match provider.as_str() {
        // OneDrive-папку меряем так же, как локальную: квота раздела диска
        "local" | "onedrive" => {
            let dir = PathBuf::from(t);
            tauri::async_runtime::spawn_blocking(move || local_disk_info(&dir))
                .await
                .map_err(|e| e.to_string())??
        }
        "yandex" => ntfry::storage::yandex::account_info(t).await.map_err(|e| e.to_string())?,
        "gdrive" => ntfry::storage::gdrive::account_info(t).await.map_err(|e| e.to_string())?,
        "mailru" => ntfry::storage::mailru::account_info(t).await.map_err(|e| e.to_string())?,
        other => return Err(format!("провайдер '{other}' не поддерживает аккаунты")),
    };
    Ok(AccountInfoOut { login: info.login, total_bytes: info.total, used_bytes: info.used })
}

/// Быстрое чтение «занято сейфом» для пары (provider, account) — из локального
/// Sled-кэша, без открытия vault. Если этот аккаунт уже активен в сессии —
/// возвращаем свежее значение прямо из RAM. Если кэша нет вовсе (например,
/// диск ещё ни разу не подключали) — 0.
#[tauri::command]
pub(crate) async fn cloud_vault_cached_stored(
    app: tauri::AppHandle,
    provider: String,
    account: Option<String>,
) -> Result<u64, String> {
    {
        let guard = VAULT.lock().await;
        if let Some(s) = guard.as_ref() {
            let acc = account.clone().unwrap_or_default();
            if s.provider == provider && s.account == acc {
                return Ok(s.vault.index.stats.stored_bytes);
            }
        }
    }
    let base = crate::ensure_data_dir(&app);
    let cache_dir = base
        .join("ntfry")
        .join("cache")
        .join(cache_name(&provider, account.as_deref()));
    if !cache_dir.exists() {
        return Ok(0);
    }
    // Cache::open/stored_bytes теперь сами внутри spawn_blocking — sled I/O
    // не блокирует IPC-тред при батчевом опросе занятости всех дисков.
    let cache = ntfry::cache::Cache::open(&cache_dir).await.map_err(|e| e.to_string())?;
    Ok(cache.stored_bytes().await.unwrap_or(0))
}

// ── RAID-режим: объединение всех дисков в одно большое хранилище ──────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RaidMemberIn {
    pub provider: String,
    pub token: Option<String>,
    pub id: String,
}

fn make_raid(base: &Path, members: &[RaidMemberIn], mode: &str) -> Result<Box<dyn StorageProvider>, String> {
    use ntfry::storage::raid::{RaidMode, RaidProvider};
    if members.len() < 2 {
        return Err("RAID-режим требует минимум 2 подключённых диска".into());
    }
    let raid_mode = if mode == "mirror" { RaidMode::Mirror } else { RaidMode::Stripe };
    let mut mem: Vec<(String, Box<dyn StorageProvider>)> = Vec::new();
    for m in members {
        mem.push((m.id.clone(), make_provider(base, &m.provider, m.token.as_deref())?));
    }
    Ok(Box::new(RaidProvider::new(mem, raid_mode)))
}

/// Стабильный account-ключ RAID-сессии: тип + отсортированные id членов.
fn raid_account(members: &[RaidMemberIn], mode: &str) -> String {
    let mut ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
    ids.sort();
    format!("{mode}:{}", ids.join("+"))
}

#[tauri::command]
pub(crate) async fn cloud_vault_open_raid(
    app: tauri::AppHandle,
    members: Vec<RaidMemberIn>,
    mode: Option<String>,
) -> Result<VaultStatusOut, String> {
    let mode = mode.unwrap_or_else(|| "stripe".into());
    let secret = crate::SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("кошелёк заблокирован — разблокируйте SmartNet")?;
    let km = tauri::async_runtime::spawn_blocking(move || vcrypto::derive_flexible(&secret))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let base = crate::ensure_data_dir(&app);
    let storage = make_raid(&base, &members, &mode)?;
    let account = raid_account(&members, &mode);
    let cache_dir = base
        .join("ntfry")
        .join("cache")
        .join(format!("raid-{}", &vcrypto::sha256_hex(account.as_bytes())[..12]));
    let mut guard = VAULT.lock().await;
    *guard = None;
    let vault = Vault::open(km.master_key, storage, &cache_dir, None)
        .await
        .map_err(|e| e.to_string())?;
    crate::syslog::log(
        "vault",
        format!("open RAID({mode})×{} files={} addr={}", members.len(), vault.index.files.len(), km.address),
    );
    let session = Session { vault, provider: "raid".into(), account, address: km.address };
    let out = status_out(&session);
    *guard = Some(session);
    Ok(out)
}

#[tauri::command]
pub(crate) async fn cloud_vault_reset_raid(
    app: tauri::AppHandle,
    members: Vec<RaidMemberIn>,
    mode: Option<String>,
) -> Result<VaultStatusOut, String> {
    let mode = mode.unwrap_or_else(|| "stripe".into());
    let secret = crate::SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("кошелёк заблокирован — разблокируйте SmartNet")?;
    let km = tauri::async_runtime::spawn_blocking(move || vcrypto::derive_flexible(&secret))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let base = crate::ensure_data_dir(&app);
    let storage = make_raid(&base, &members, &mode)?;
    storage.delete_vault().await.map_err(|e| e.to_string())?;
    let account = raid_account(&members, &mode);
    let cache_dir = base
        .join("ntfry")
        .join("cache")
        .join(format!("raid-{}", &vcrypto::sha256_hex(account.as_bytes())[..12]));
    let _ = std::fs::remove_dir_all(&cache_dir);
    let vault = Vault::init(km.master_key, storage, &cache_dir, None)
        .await
        .map_err(|e| e.to_string())?;
    crate::syslog::log("vault", format!("RESET RAID×{} — /ntfry re-created on all disks", members.len()));
    let session = Session { vault, provider: "raid".into(), account, address: km.address };
    let out = status_out(&session);
    *VAULT.lock().await = Some(session);
    Ok(out)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RaidResyncOut {
    checked: u64,
    copied: u64,
    failed: u64,
    lost: u64,
}

/// RAID-1 re-sync: докатка недостающих чанков на члены зеркала.
/// Прогресс — событие `vault-progress` (op = "resync").
#[tauri::command]
pub(crate) async fn cloud_vault_raid_resync(app: tauri::AppHandle) -> Result<RaidResyncOut, String> {
    let mut guard = VAULT.lock().await;
    let s = guard.as_mut().ok_or("vault не открыт")?;
    if s.provider != "raid" {
        return Err("re-sync доступен только в RAID-режиме".into());
    }
    let ah = app.clone();
    let cb = move |done: u64, total: u64| {
        let _ = ah.emit(
            "vault-progress",
            serde_json::json!({ "op": "resync", "file": "RAID mirror", "done": done, "total": total }),
        );
    };
    let st = s.vault.resync(Some(&cb)).await.map_err(|e| e.to_string())?;
    crate::syslog::log(
        "vault",
        format!("RAID re-sync: checked={} copied={} failed={} lost={}", st.checked, st.copied, st.failed, st.lost),
    );
    Ok(RaidResyncOut { checked: st.checked, copied: st.copied, failed: st.failed, lost: st.lost })
}

// ── Drag&Drop: рекурсивный скан папок для очереди загрузки ────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanFileOut {
    src: String,
    rel: String,
    size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanOut {
    files: Vec<ScanFileOut>,
    empty_dirs: Vec<String>,
    total_bytes: u64,
}

/// Обход брошенных в dropzone путей: файлы → плоский список с относительными
/// VFS-путями (структура папок сохраняется), пустые каталоги — отдельно.
/// Скрытые файлы/папки (.*) пропускаются.
#[tauri::command]
pub(crate) async fn cloud_vault_scan_paths(paths: Vec<String>) -> Result<ScanOut, String> {
    tauri::async_runtime::spawn_blocking(move || {
        fn walk(dir: &Path, rel: &str, out: &mut ScanOut) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            let mut empty = true;
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let p = e.path();
                let child_rel = format!("{rel}/{name}");
                if p.is_dir() {
                    empty = false;
                    walk(&p, &child_rel, out);
                } else if p.is_file() {
                    empty = false;
                    let size = p.metadata().map(|m| m.len()).unwrap_or(0);
                    out.total_bytes += size;
                    out.files.push(ScanFileOut { src: p.to_string_lossy().to_string(), rel: child_rel, size });
                }
            }
            if empty {
                out.empty_dirs.push(rel.to_string());
            }
        }
        let mut out = ScanOut { files: vec![], empty_dirs: vec![], total_bytes: 0 };
        for raw in paths {
            let p = PathBuf::from(&raw);
            let Some(name) = p.file_name().and_then(|n| n.to_str()).map(String::from) else { continue };
            if name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                walk(&p, &name, &mut out);
            } else if p.is_file() {
                let size = p.metadata().map(|m| m.len()).unwrap_or(0);
                out.total_bytes += size;
                out.files.push(ScanFileOut { src: raw, rel: name, size });
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}
