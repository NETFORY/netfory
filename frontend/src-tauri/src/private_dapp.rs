// ============================================================
//  SmartNet // private_dapp.rs — Приватные dApps (E2EE-контейнер).
//
//  Модель (согласовано с владельцем продукта):
//    - dist/ приложения пакуется в tar и шифруется AES-256-GCM случайным
//      ключом контейнера (CEK). На диске у ВСЕХ (владелец, участники,
//      сидеры) лежит только шифртекст `app.enc` + `access.json`.
//    - CEK ECIES-оборачивается (secp256k1 ECDH, как приватные файлы) для
//      каждого адреса из списка доступа + владельца. Ключи access.json
//      входят в files[]/merkle → защищены подписью манифеста (canon v2).
//    - Расшифровка ТОЛЬКО в оперативную память (PRIVATE_RAM) на время
//      открытой вкладки; protocol.rs отдаёт байты из RAM-карты. Закрытие
//      вкладки / блокировка кошелька выгружает plaintext из памяти.
//    - Видимость: имя/описание зашифрованы (privMeta/privAnnounce);
//      публичный манифест несёт плейсхолдер. Анонс трекерам кладёт в
//      description reveal-payload (маркер §P1§ + b64 JSON {m, k}) — клиент
//      из списка расшифровывает метаданные и видит dApp в поиске, чужие
//      клиенты скрывают его полностью (fail-closed).
//    - Сидируют все (контейнер зашифрован) — живучесть выше.
//    - Изменение списка адресов владельцем: ЛЮБОЕ изменение ротирует CEK
//      (перешифрование контейнера) + version++ — удалённый участник теряет
//      доступ криптографически, а не галочкой.
//
//  Author: TechnoL0g
//  Created: 2026-06
// ============================================================

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

use base64::prelude::{Engine as _, BASE64_STANDARD};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::crypto::{
    aes_gcm_decrypt, aes_gcm_encrypt, ecies_unwrap_key, ecies_wrap_key, gen_file_key,
    keys_from_material, sha256,
};
use crate::{ensure_data_dir, load_vault_internal, Manifest, APP_PREFIX, DB, SESSION_SEED};

/// Публичное имя-плейсхолдер приватного dApp (уходит в манифест и анонсы).
pub(crate) const PRIV_PLACEHOLDER: &str = "🔒 Private dApp";
/// Маркер reveal-payload в description трекерного анонса.
pub(crate) const REVEAL_MARKER: &str = "§P1§";
pub(crate) const ACCESS_FILE: &str = "access.json";
pub(crate) const CONTAINER_FILE: &str = "app.enc";
const ACCESS_PREFIX: &str = "privaccess:";
const REVEAL_CACHE_PREFIX: &str = "privreveal:";
/// Анти zip-bomb: распакованный dist не больше 512 МБ.
const MAX_PLAIN: u64 = 512 * 1024 * 1024;

/// Расшифрованные dist-файлы открытых приватных dApps: id → (путь → байты).
static PRIVATE_RAM: Lazy<Mutex<HashMap<String, Arc<HashMap<String, Vec<u8>>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Суммарный объём dist-файлов приватных dApps в RAM (перепись памяти crashdiag).
pub(crate) fn ram_bytes() -> u64 {
    PRIVATE_RAM
        .lock()
        .map(|m| m.values().map(|f| f.values().map(|b| b.len() as u64).sum::<u64>()).sum())
        .unwrap_or(0)
}

/// Статус фоновой расшифровки контейнера: id → InProgress | Failed(причина).
/// Страница-прогресс опрашивает результат перезагрузкой.
#[derive(Clone)]
enum MountState {
    InProgress,
    Failed(String),
}
static MOUNT_STATE: Lazy<Mutex<HashMap<String, MountState>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

// ------------------------------------------------------------
//  Модели
// ------------------------------------------------------------

/// access.json — лежит в папке dApp, входит в files[]/merkle (целостность
/// гарантируется подписью манифеста). Дублируется в Sled для анонсов.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccessFile {
    /// адрес → ECIES-обёрнутый CEK (hex)
    #[serde(default)]
    pub wrapped_keys: HashMap<String, String>,
    /// адрес → публичный ключ secp256k1 (hex) — кэш для ре-обёртки без ноды
    #[serde(default)]
    pub pubkeys: HashMap<String, String>,
    /// b64(AES-GCM(cek, JSON PrivMeta)) — полные метаданные (с логотипом)
    #[serde(default)]
    pub priv_meta: String,
    /// b64(AES-GCM(cek, JSON {n,d,t})) — компакт для анонса (без логотипа)
    #[serde(default)]
    pub priv_announce: String,
}

/// Реальные метаданные приватного dApp (внутри шифртекста priv_meta).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrivMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub logo_b64: String,
    /// "png" | "svg"
    #[serde(default)]
    pub logo_ext: String,
}

/// Спецификация приватности для ingest: адрес → on-chain pubkey (hex),
/// владелец ВКЛЮЧЁН.
pub(crate) struct PrivacySpec {
    pub pubkeys: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RevealView {
    pub allowed: bool,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
}

// ------------------------------------------------------------
//  Helpers
// ------------------------------------------------------------

fn my_address() -> String {
    load_vault_internal().map(|v| v.address).unwrap_or_default()
}

fn session_secret() -> Result<String, String> {
    SESSION_SEED
        .lock()
        .map_err(|_| "locked".to_string())?
        .clone()
        .ok_or_else(|| "locked".to_string())
}

fn access_key(id: &str) -> Vec<u8> {
    format!("{}{}", ACCESS_PREFIX, id).into_bytes()
}

fn load_manifest(id: &str) -> Option<Manifest> {
    DB.get(format!("{}{}", APP_PREFIX, id).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Manifest>(&v).ok())
}

pub(crate) fn is_private(id: &str) -> bool {
    load_manifest(id).map(|m| m.private == Some(true)).unwrap_or(false)
}

/// access: Sled-кэш → файл в папке dApp (fallback).
pub(crate) fn load_access(app: &tauri::AppHandle, id: &str) -> Option<AccessFile> {
    if let Ok(Some(v)) = DB.get(access_key(id)) {
        if let Ok(a) = serde_json::from_slice::<AccessFile>(&v) {
            return Some(a);
        }
    }
    let p = ensure_data_dir(app).join("dapps").join(id).join(ACCESS_FILE);
    let bytes = std::fs::read(p).ok()?;
    serde_json::from_slice::<AccessFile>(&bytes).ok()
}

/// access ТОЛЬКО из Sled (для мест без AppHandle: list_apps, анонсы).
pub(crate) fn load_access_db(id: &str) -> Option<AccessFile> {
    DB.get(access_key(id))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<AccessFile>(&v).ok())
}

/// Записать access.json в папку dApp + Sled-кэш.
pub(crate) fn save_access(dest_root: &Path, id: &str, access: &AccessFile) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec_pretty(access).map_err(|e| e.to_string())?;
    std::fs::write(dest_root.join(ACCESS_FILE), &json).map_err(|e| e.to_string())?;
    let _ = DB.insert(access_key(id), json.clone());
    let _ = DB.flush();
    Ok(json)
}

/// После P2P-установки: скопировать access.json из папки в Sled (для анонсов).
pub(crate) fn cache_access_from_folder(app: &tauri::AppHandle, id: &str) {
    let p = ensure_data_dir(app).join("dapps").join(id).join(ACCESS_FILE);
    if let Ok(bytes) = std::fs::read(p) {
        if serde_json::from_slice::<AccessFile>(&bytes).is_ok() {
            let _ = DB.insert(access_key(id), bytes);
            let _ = DB.flush();
        }
    }
}

pub(crate) fn purge(id: &str) {
    unmount(id);
    let _ = DB.remove(access_key(id));
    let _ = DB.remove(format!("{}{}", REVEAL_CACHE_PREFIX, id).as_bytes());
    let _ = DB.flush();
}

/// Развернуть свой CEK из access (требует разблокированной сессии).
pub(crate) fn unwrap_cek(access: &AccessFile) -> Result<[u8; 32], String> {
    let secret = session_secret()?;
    let addr = my_address();
    let wrapped = access
        .wrapped_keys
        .get(&addr)
        .ok_or("нет доступа: ваш адрес не в списке приватного dApp")?;
    ecies_unwrap_key(&secret, wrapped)
}

/// Расшифровать полные метаданные (имя/описание/теги/логотип).
pub(crate) fn decrypt_meta(access: &AccessFile) -> Option<PrivMeta> {
    let cek = unwrap_cek(access).ok()?;
    let enc = BASE64_STANDARD.decode(&access.priv_meta).ok()?;
    let plain = aes_gcm_decrypt(&cek, &enc).ok()?;
    serde_json::from_slice::<PrivMeta>(&plain).ok()
}

/// Подставить реальные метаданные в манифест для UI (участник + unlocked).
pub(crate) fn decorate_manifest(m: &mut Manifest) {
    if m.private != Some(true) {
        return;
    }
    if let Some(access) = load_access_db(&m.id) {
        if let Some(meta) = decrypt_meta(&access) {
            m.name = meta.name;
            m.description = Some(meta.description);
            m.tags = meta.tags;
        }
    }
}

/// Логотип приватного dApp (data-URL) из зашифрованных метаданных.
pub(crate) fn private_logo(id: &str) -> Option<String> {
    let access = load_access_db(id)?;
    let meta = decrypt_meta(&access)?;
    if meta.logo_b64.is_empty() {
        return None;
    }
    let mime = if meta.logo_ext == "svg" { "image/svg+xml" } else { "image/png" };
    Some(format!("data:{};base64,{}", mime, meta.logo_b64))
}

/// tar dist-папки в память (детерминированный относительный layout).
pub(crate) fn tar_dir(src: &Path) -> Result<Vec<u8>, String> {
    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append_dir_all(".", src)
        .map_err(|e| format!("tar: {e}"))?;
    builder.into_inner().map_err(|e| format!("tar: {e}"))
}

fn encrypt_json(cek: &[u8; 32], v: &serde_json::Value) -> Result<String, String> {
    let plain = serde_json::to_vec(v).map_err(|e| e.to_string())?;
    Ok(BASE64_STANDARD.encode(aes_gcm_encrypt(cek, &plain)?))
}

/// Собрать AccessFile: обернуть CEK для всех адресов, запечатать метаданные.
pub(crate) fn build_access(
    cek: &[u8; 32],
    pubkeys: &HashMap<String, String>,
    meta: &PrivMeta,
) -> Result<AccessFile, String> {
    let mut wrapped_keys = HashMap::new();
    for (addr, pk) in pubkeys {
        wrapped_keys.insert(addr.clone(), ecies_wrap_key(pk, cek)?);
    }
    let meta_json = serde_json::to_value(meta).map_err(|e| e.to_string())?;
    let d_short: String = meta.description.chars().take(160).collect();
    let announce_json = serde_json::json!({ "n": meta.name, "d": d_short, "t": meta.tags });
    Ok(AccessFile {
        wrapped_keys,
        pubkeys: pubkeys.clone(),
        priv_meta: encrypt_json(cek, &meta_json)?,
        priv_announce: encrypt_json(cek, &announce_json)?,
    })
}

/// On-chain pubkeys адресов списка + владельца. Блокирующая (вызывать из
/// spawn_blocking). Адрес без исходящих транзакций блокирует публикацию.
pub(crate) fn resolve_pubkeys(recipients: &[String]) -> Result<HashMap<String, String>, String> {
    let secret = session_secret()?;
    let owner_addr = my_address();
    if owner_addr.is_empty() {
        return Err("locked".to_string());
    }
    let owner_pk = keys_from_material(&secret)?.1;
    let mut out: HashMap<String, String> = HashMap::new();
    out.insert(owner_addr.clone(), owner_pk);
    for r in recipients {
        let a = r.trim();
        if a.is_empty() || a == owner_addr || out.contains_key(a) {
            continue;
        }
        match crate::sth_node::fetch_wallet_pubkey(a) {
            Ok(Some(pk)) => {
                out.insert(a.to_string(), pk);
            }
            Ok(None) => {
                return Err(format!(
                    "Адрес {} ещё не активен в блокчейне (нет публичного ключа). Получатель должен совершить хотя бы одну исходящую транзакцию.",
                    a
                ));
            }
            Err(e) => return Err(format!("Не удалось получить ключ адреса {}: {}", a, e)),
        }
    }
    Ok(out)
}

/// Reveal-payload для трекерного анонса: маркер + b64({m, k}).
pub(crate) fn reveal_payload_for(id: &str) -> Option<String> {
    let access = load_access_db(id)?;
    if access.priv_announce.is_empty() || access.wrapped_keys.is_empty() {
        return None;
    }
    let payload = serde_json::json!({ "m": access.priv_announce, "k": access.wrapped_keys });
    Some(format!("{}{}", REVEAL_MARKER, BASE64_STANDARD.encode(payload.to_string())))
}

// ------------------------------------------------------------
//  RAM-монтирование (расшифровка только в память)
// ------------------------------------------------------------

pub(crate) fn is_mounted(id: &str) -> bool {
    PRIVATE_RAM.lock().map(|g| g.contains_key(id)).unwrap_or(false)
}

pub(crate) fn ram_map(id: &str) -> Option<Arc<HashMap<String, Vec<u8>>>> {
    PRIVATE_RAM.lock().ok()?.get(id).cloned()
}

/// Расшифровать app.enc в PRIVATE_RAM. Идемпотентно.
pub(crate) fn mount(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    if is_mounted(id) {
        return Ok(());
    }
    let access = load_access(app, id).ok_or("access.json не найден")?;
    let cek = unwrap_cek(&access)?;
    let enc = std::fs::read(
        ensure_data_dir(app).join("dapps").join(id).join(CONTAINER_FILE),
    )
    .map_err(|_| "app.enc не найден".to_string())?;
    let plain = aes_gcm_decrypt(&cek, &enc).map_err(|_| "не удалось расшифровать контейнер".to_string())?;

    let mut map: HashMap<String, Vec<u8>> = HashMap::new();
    let mut total: u64 = 0;
    let mut ar = tar::Archive::new(&plain[..]);
    for entry in ar.entries().map_err(|e| e.to_string())? {
        let mut e = entry.map_err(|e| e.to_string())?;
        if !e.header().entry_type().is_file() {
            continue;
        }
        let path = e
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let path = path.trim_start_matches("./").to_string();
        if path.is_empty() {
            continue;
        }
        let mut buf = Vec::with_capacity(e.size() as usize);
        e.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        total += buf.len() as u64;
        if total > MAX_PLAIN {
            return Err("распакованный dApp превышает лимит 512 МБ".to_string());
        }
        map.insert(path, buf);
    }
    if map.is_empty() {
        return Err("пустой контейнер".to_string());
    }
    PRIVATE_RAM
        .lock()
        .map_err(|_| "lock".to_string())?
        .insert(id.to_string(), Arc::new(map));
    crate::syslog::log("dapp", format!("private dApp {} mounted to RAM", &id[..id.len().min(12)]));
    Ok(())
}

pub(crate) fn unmount(id: &str) {
    if let Ok(mut g) = PRIVATE_RAM.lock() {
        if g.remove(id).is_some() {
            crate::syslog::log("dapp", format!("private dApp {} unmounted", &id[..id.len().min(12)]));
        }
    }
}

/// mount() c дедупликацией через MOUNT_STATE: параллельные вызовы (команда
/// прогрева + протокол-хэндлер) не расшифровывают контейнер дважды — второй
/// поток ждёт результат первого. Статус Failed остаётся для страницы ошибки.
pub(crate) fn mount_tracked(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    if is_mounted(id) {
        return Ok(());
    }
    loop {
        {
            let mut g = MOUNT_STATE.lock().map_err(|_| "lock".to_string())?;
            match g.get(id) {
                Some(MountState::InProgress) => {} // расшифровывает другой поток — ждём
                Some(MountState::Failed(e)) => {
                    let e = e.clone();
                    g.remove(id);
                    return Err(e);
                }
                None => {
                    if is_mounted(id) {
                        return Ok(());
                    }
                    g.insert(id.to_string(), MountState::InProgress);
                    break;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        if is_mounted(id) {
            return Ok(());
        }
    }
    let res = mount(app, id);
    if let Ok(mut g) = MOUNT_STATE.lock() {
        match &res {
            Ok(()) => {
                g.remove(id);
            }
            Err(e) => {
                crate::syslog::log(
                    "dapp",
                    format!("private mount {} failed: {}", &id[..id.len().min(12)], e),
                );
                g.insert(id.to_string(), MountState::Failed(e.clone()));
            }
        }
    }
    res
}

/// Выгрузить ВСЕ приватные dApps из памяти (блокировка кошелька / выход).
pub(crate) fn unmount_all() {
    if let Ok(mut g) = PRIVATE_RAM.lock() {
        g.clear();
    }
}

/// Отдать файл приватного dApp из RAM (SPA-fallback как у protocol::serve).
/// Не смонтирован → расшифровка запускается в ФОНЕ, а клиенту сразу отдаётся
/// страница-прогресс (сама перезапрашивается и открывает приложение по
/// готовности) — большие контейнеры не блокируют протокол-хэндлер.
pub(crate) fn serve_ram(app: &tauri::AppHandle, id: &str, path: &str) -> Option<(Vec<u8>, String)> {
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if let Some(map) = ram_map(id) {
        if let Some(bytes) = map.get(rel) {
            return Some((bytes.clone(), crate::protocol::mime_for(rel).to_string()));
        }
        if !crate::protocol::is_static_asset(rel) {
            if let Some(bytes) = map.get("index.html") {
                return Some((bytes.clone(), "text/html".to_string()));
            }
        }
        return None;
    }
    // Не смонтирован: ассеты до готовности index.html не отдаём (404),
    // страницы (документ) получают прогресс/ошибку.
    if crate::protocol::is_static_asset(rel) && rel != "index.html" {
        return None;
    }
    let snapshot = MOUNT_STATE.lock().ok()?.get(id).cloned();
    match snapshot {
        Some(MountState::Failed(e)) => {
            // Показали причину; F5 очистит статус и попробует заново.
            if let Ok(mut g) = MOUNT_STATE.lock() {
                g.remove(id);
            }
            Some((locked_page(&e).into_bytes(), "text/html".to_string()))
        }
        Some(MountState::InProgress) => {
            Some((progress_page().into_bytes(), "text/html".to_string()))
        }
        None => {
            let app2 = app.clone();
            let id2 = id.to_string();
            std::thread::spawn(move || {
                let _ = mount_tracked(&app2, &id2);
            });
            Some((progress_page().into_bytes(), "text/html".to_string()))
        }
    }
}

/// Страница «идёт расшифровка приложения» с анимированной шкалой.
/// Перезапрашивает себя, пока контейнер расшифровывается в RAM.
fn progress_page() -> String {
    r#"<!doctype html><html lang="ru"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>SmartNet // Private dApp</title>
<style>
  *{margin:0;padding:0;box-sizing:border-box}html,body{height:100%}
  body{display:flex;align-items:center;justify-content:center;background:#070a0f;color:#cbd5e1;
    font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
  .box{text-align:center;padding:44px 52px;max-width:540px;border:1px solid rgba(217,70,239,.3);
    border-radius:14px;background:rgba(13,18,26,.75)}
  .lock{font-size:56px;animation:pulse 1.6s ease-in-out infinite}
  @keyframes pulse{0%,100%{opacity:1}50%{opacity:.55}}
  h1{margin-top:18px;font-size:20px;color:#e2e8f0}
  p{margin-top:12px;font-size:13px;line-height:1.7;color:#94a3b8}
  .bar{margin-top:26px;height:8px;border-radius:999px;overflow:hidden;
    background:rgba(148,163,184,.15);position:relative}
  .bar::after{content:"";position:absolute;top:0;left:-40%;width:40%;height:100%;
    border-radius:999px;background:linear-gradient(90deg,transparent,#d946ef,#22d3ee,transparent);
    animation:slide 1.4s ease-in-out infinite}
  @keyframes slide{0%{left:-40%}100%{left:100%}}
  .tag{display:inline-block;margin-top:24px;padding:6px 14px;font-size:11px;letter-spacing:2px;
    text-transform:uppercase;color:#d946ef;border:1px solid rgba(217,70,239,.35);border-radius:999px}
</style></head><body><div class="box">
  <div class="lock">🔓</div><h1>Decrypting the application…</h1>
  <p>The container is decompressed into RAM.<br>
     Large applications may take longer to open — please wait.</p>
  <div class="bar"></div>
  <span class="tag">E2EE · AES-256-GCM · SmartHoldem</span>
</div><script>setTimeout(function(){location.reload()},1200)</script></body></html>"#
        .to_string()
}

/// Страница «приватный dApp заблокирован» (нет ключа / сессия закрыта).
fn locked_page(reason: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>NETFORY // Private dApp</title>
<style>
  *{{margin:0;padding:0;box-sizing:border-box}}html,body{{height:100%}}
  body{{display:flex;align-items:center;justify-content:center;background:#070a0f;color:#cbd5e1;
    font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}}
  .box{{text-align:center;padding:44px 52px;max-width:540px;border:1px solid rgba(217,70,239,.3);
    border-radius:14px;background:rgba(13,18,26,.75)}}
  .lock{{font-size:56px}}h1{{margin-top:18px;font-size:20px;color:#e2e8f0}}
  p{{margin-top:12px;font-size:13px;line-height:1.7;color:#94a3b8}}
  .tag{{display:inline-block;margin-top:22px;padding:6px 14px;font-size:11px;letter-spacing:2px;
    text-transform:uppercase;color:#d946ef;border:1px solid rgba(217,70,239,.35);border-radius:999px}}
</style></head><body><div class="box">
  <div class="lock">🔒</div><h1>Private dApp</h1>
  <p>The container is encrypted. {reason}</p>
  <p>Unlock the wallet with an address from the access list and refresh the tab.</p>
  <span class="tag">E2EE · SmartHoldem</span>
</div></body></html>"#
    )
}

// ------------------------------------------------------------
//  Tauri-команды
// ------------------------------------------------------------

/// Прогрев: расшифровать контейнер в RAM перед открытием вкладки.
#[tauri::command]
pub(crate) async fn private_dapp_mount(app: tauri::AppHandle, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || mount_tracked(&app, &id))
        .await
        .map_err(|e| e.to_string())?
}

/// Выгрузить plaintext из памяти (закрытие вкладки). No-op для публичных.
#[tauri::command(async)]
pub(crate) fn private_dapp_unmount(id: String) {
    unmount(&id);
}

/// Расшифрованные метаданные приватного dApp (участник + unlocked).
#[tauri::command(async)]
pub(crate) fn private_dapp_meta(id: String) -> Option<PrivMeta> {
    let access = load_access_db(&id)?;
    let mut meta = decrypt_meta(&access)?;
    meta.logo_b64 = String::new(); // логотип отдаёт app_logo — не гоняем base64 дважды
    if meta.name.is_empty() {
        return None;
    }
    Some(meta)
}

/// Список адресов доступа (только владельцу; без адреса владельца).
#[tauri::command(async)]
pub(crate) fn private_dapp_recipients(id: String) -> Result<Vec<String>, String> {
    let access = load_access_db(&id).ok_or("access.json not found")?;
    let me = my_address();
    let mut out: Vec<String> = access
        .wrapped_keys
        .keys()
        .filter(|a| **a != me)
        .cloned()
        .collect();
    out.sort();
    Ok(out)
}

/// Проверка reveal-payload из анонса: наш ли адрес в списке; если да —
/// расшифрованные имя/описание/теги для каталога. Кэш в Sled по хэшу payload.
#[tauri::command(async)]
pub(crate) fn private_reveal(id: String, payload: String) -> RevealView {
    let denied = RevealView {
        allowed: false,
        name: String::new(),
        description: String::new(),
        tags: Vec::new(),
    };
    let raw = match payload.strip_prefix(REVEAL_MARKER) {
        Some(r) => r,
        None => return denied,
    };
    let ph = hex::encode(sha256(raw.as_bytes()));
    let cache_key = format!("{}{}", REVEAL_CACHE_PREFIX, id);
    if let Ok(Some(v)) = DB.get(cache_key.as_bytes()) {
        if let Ok(c) = serde_json::from_slice::<serde_json::Value>(&v) {
            if c.get("ph").and_then(|x| x.as_str()) == Some(ph.as_str()) {
                return RevealView {
                    allowed: c.get("allowed").and_then(|x| x.as_bool()).unwrap_or(false),
                    name: c.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    description: c.get("description").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    tags: c
                        .get("tags")
                        .and_then(|x| x.as_array())
                        .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                };
            }
        }
    }
    let secret = match session_secret() {
        Ok(s) => s,
        Err(_) => return denied, // сессия закрыта — без кэширования отказа
    };
    let compute = || -> Option<RevealView> {
        let bytes = BASE64_STANDARD.decode(raw).ok()?;
        let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let me = my_address();
        let wrapped = v.get("k")?.get(&me)?.as_str()?.to_string();
        let cek = ecies_unwrap_key(&secret, &wrapped).ok()?;
        let m_enc = BASE64_STANDARD.decode(v.get("m")?.as_str()?).ok()?;
        let plain = aes_gcm_decrypt(&cek, &m_enc).ok()?;
        let meta: serde_json::Value = serde_json::from_slice(&plain).ok()?;
        Some(RevealView {
            allowed: true,
            name: meta.get("n").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            description: meta.get("d").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            tags: meta
                .get("t")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                .unwrap_or_default(),
        })
    };
    let view = compute().unwrap_or(denied);
    let cache = serde_json::json!({
        "ph": ph,
        "allowed": view.allowed,
        "name": view.name,
        "description": view.description,
        "tags": view.tags,
    });
    let _ = DB.insert(cache_key.as_bytes(), cache.to_string().into_bytes());
    view
}

/// Изменить список доступа (только владелец). ВСЕГДА ротирует CEK:
/// перешифровывает контейнер новым ключом, version++, re-sign — удалённый
/// адрес теряет доступ криптографически. src_dir не нужен: tar
/// восстанавливается расшифровкой текущего app.enc ключом владельца.
#[tauri::command]
pub(crate) async fn private_dapp_set_recipients(
    app: tauri::AppHandle,
    id: String,
    recipients: Vec<String>,
) -> Result<Manifest, String> {
    tauri::async_runtime::spawn_blocking(move || set_recipients_blocking(app, id, recipients))
        .await
        .map_err(|e| e.to_string())?
}

fn set_recipients_blocking(
    app: tauri::AppHandle,
    id: String,
    recipients: Vec<String>,
) -> Result<Manifest, String> {
    use secp256k1::{Message, Secp256k1};
    let secret = session_secret()?;
    let mut manifest = load_manifest(&id).ok_or("app not found")?;
    if manifest.private != Some(true) {
        return Err("this is not a private dApp".to_string());
    }
    let index = manifest.index.unwrap_or(0);
    if crate::crypto::derive_app_identity_internal(&secret, index)?.address != id {
        return Err("identity mismatch".to_string());
    }
    let access = load_access(&app, &id).ok_or("access.json not found")?;
    let old_cek = unwrap_cek(&access)?;
    let meta = decrypt_meta(&access).ok_or("failed to decrypt metadata")?;

    let data_dir = ensure_data_dir(&app);
    let dest_root = data_dir.join("dapps").join(&id);
    let enc = std::fs::read(dest_root.join(CONTAINER_FILE)).map_err(|e| e.to_string())?;
    let tar_bytes = aes_gcm_decrypt(&old_cek, &enc)?;

    // pubkeys: кэш из access для прежних адресов, нода — для новых
    let mut pubkeys: HashMap<String, String> = HashMap::new();
    let owner_addr = my_address();
    pubkeys.insert(owner_addr.clone(), keys_from_material(&secret)?.1);
    for r in &recipients {
        let a = r.trim().to_string();
        if a.is_empty() || a == owner_addr || pubkeys.contains_key(&a) {
            continue;
        }
        if let Some(pk) = access.pubkeys.get(&a) {
            pubkeys.insert(a, pk.clone());
        } else {
            match crate::sth_node::fetch_wallet_pubkey(&a) {
                Ok(Some(pk)) => {
                    pubkeys.insert(a, pk);
                }
                Ok(None) => {
                    return Err(format!(
                        "Address {} is not yet active on the blockchain (no public key). The recipient must make at least one outgoing transaction.",
                        a
                    ));
                }
                Err(e) => return Err(format!("Failed to get the public key for address {}: {}", a, e)),
            }
        }
    }

    // Rotate CEK + re-encrypt the container and metadata
    let cek = gen_file_key();
    let container = aes_gcm_encrypt(&cek, &tar_bytes)?;
    std::fs::write(dest_root.join(CONTAINER_FILE), &container).map_err(|e| e.to_string())?;
    let new_access = build_access(&cek, &pubkeys, &meta)?;
    save_access(&dest_root, &id, &new_access)?;

    // files + merkle пересчитываются с диска (access.json + app.enc)
    let mut files: Vec<crate::FileEntry> = Vec::new();
    let mut blake_parts: Vec<(String, String)> = Vec::new();
    for name in [ACCESS_FILE, CONTAINER_FILE] {
        let data = std::fs::read(dest_root.join(name)).map_err(|e| e.to_string())?;
        files.push(crate::FileEntry {
            path: name.to_string(),
            sha256: hex::encode(sha256(&data)),
            size: data.len() as u64,
        });
        blake_parts.push((name.to_string(), blake3::hash(&data).to_hex().to_string()));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    blake_parts.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = blake3::Hasher::new();
    for (p, h) in &blake_parts {
        hasher.update(p.as_bytes());
        hasher.update(h.as_bytes());
    }
    let merkle = hasher.finalize().to_hex().to_string();

    // re-sign canon v2 (имя-плейсхолдер + пустое описание из манифеста)
    let description = manifest.description.clone().unwrap_or_default();
    let digest = sha256(
        format!("{}|{}|{}|{}|{}", id, manifest.name, description, merkle, manifest.owner).as_bytes(),
    );
    let (sk, app_pubkey, _) = keys_from_material(&format!("{}/1/{}", secret.trim(), index))?;
    let secp = Secp256k1::signing_only();
    let msg = Message::from_digest_slice(&digest).map_err(|e| e.to_string())?;
    let signature = hex::encode(secp.sign_ecdsa(&msg, &sk).serialize_compact());

    manifest.files = files;
    manifest.merkle = merkle;
    manifest.signature = signature;
    manifest.public_key = app_pubkey;
    manifest.version = Some(manifest.version.unwrap_or(1) + 1);
    manifest.updated = Some(crate::crypto::now_iso());

    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let _ = std::fs::write(dest_root.join("content.json"), &json);
    let _ = std::fs::write(data_dir.join("manifests").join(format!("{}.json", id)), &json);
    DB.insert(format!("{}{}", APP_PREFIX, id).as_bytes(), json)
        .map_err(|e| e.to_string())?;
    // сбрасываем reveal-кэш и устаревший seed-tar — следующий анонс раздаст новую версию
    let _ = DB.remove(format!("{}{}", REVEAL_CACHE_PREFIX, id).as_bytes());
    let _ = std::fs::remove_file(data_dir.join("blobs").join("seed").join(format!("{}.tar", id)));
    let _ = DB.flush();
    unmount(&id); // RAM-копия устарела (новый контейнер)
    let mut out = manifest;
    decorate_manifest(&mut out);
    Ok(out)
}
