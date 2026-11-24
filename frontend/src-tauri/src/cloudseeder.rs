//! Cloud Seeder (ntfryseed) — Tauri IPC-мост публичного анти-цензурного
//! слоя. Автор подписывает стейт сети (dApps / ноды / известные сидеры)
//! ключом кошелька, шифрует ключом из СВОЕГО pubkey (HKDF) и заливает в
//! общедоступную папку /ntfryseed на облаке. Любой пир по URI
//! sn://seed/y@<folder_token>@<pubkey> читает и проверяет стейт БЕЗ OAuth.
//! Вместе с метаданными в облако уходят зашифрованные tar-архивы самих
//! dApp (`dapp-<id>.enc`) — пир может установить dApp при мёртвом P2P.

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

use ntfry::seeder::{self, CloudSeederUri, SeederProvider};

/// Однократная зачистка мусора от неудачной chunked-логики v1.90.0..1:
/// в глобальном sled DB могли остаться ключи `seeder_resume:y:*` — они
/// больше нигде не читаются, но занимают место. Вызывается при первой
/// публикации.
fn drop_legacy_resume_state() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut victims: Vec<Vec<u8>> = Vec::new();
        for row in crate::DB.scan_prefix(b"seeder_resume:") {
            if let Ok((k, _)) = row {
                victims.push(k.to_vec());
            }
        }
        for k in victims {
            let _ = crate::DB.remove(k);
        }
    });
}

/// In-memory карта appId → (сидер-URI, version) — у кого лежит архив dApp
/// (заполняется при каждом fetch/fallback из dapps.enc; version нужна, чтобы
/// при облачной установке предпочесть самую свежую копию и исключить старые).
static APP_SOURCES: Lazy<Mutex<HashMap<String, Vec<(String, u64)>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn record_app_sources(uri: &str, dapps: &serde_json::Value) {
    let Some(items) = dapps.get("items").and_then(|v| v.as_array()) else {
        return;
    };
    let mut map = APP_SOURCES.lock().unwrap();
    for d in items {
        if d.get("archive").and_then(|a| a.as_bool()) != Some(true) {
            continue;
        }
        let Some(id) = d.get("id").and_then(|i| i.as_str()) else { continue };
        let ver = d.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
        let list = map.entry(id.to_string()).or_default();
        match list.iter_mut().find(|(u, _)| u == uri) {
            Some(e) => e.1 = ver,
            None => list.push((uri.to_string(), ver)),
        }
    }
}

/// In-memory счётчик «спасённых» установок за сессию: uri → сколько раз
/// dApp был успешно установлен именно из этого сидера (когда P2P не помог).
static RESCUE_COUNTS: Lazy<Mutex<HashMap<String, u32>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[tauri::command]
pub(crate) fn cloud_seeder_rescues_get() -> HashMap<String, u32> {
    RESCUE_COUNTS.lock().unwrap().clone()
}

/// Sled-реестр известных Cloud Seeder URI (используется и Cloud Fallback
/// при старте клиента).
const SEEDERS_KEY: &[u8] = b"cfg:cloud_seeders";
/// Sled-карта uri → статус последней проверки (fetch).
const STATUS_KEY: &[u8] = b"cfg:cloud_seeder_status";
/// Sled-карта appId → merkle уже залитого в облако архива (skip-гейт).
const UPLOADED_KEY: &[u8] = b"cfg:seeder_uploaded";
/// Sled: свой опубликованный Seeder-URI (для gossip-анонса при старте).
const OWN_URI_KEY: &[u8] = b"cfg:own_seeder_uri";
/// Sled: свой ВТОРОЙ (зеркальный) Seeder-URI — публикация на secondary-диск.
const OWN_URI2_KEY: &[u8] = b"cfg:own_seeder_uri2";
/// Sled-карта uri → источник добавления в реестр ("own"|"manual"|"gossip"|"seed_file"|"bootstrap").
/// Позволяет UI показать пользователю, откуда пришёл каждый известный сидер.
const ORIGINS_KEY: &[u8] = b"cfg:cloud_seeder_origins";

pub(crate) fn own_seeder_uri() -> Option<String> {
    crate::DB
        .get(OWN_URI_KEY)
        .ok()
        .flatten()
        .map(|v| String::from_utf8_lossy(&v).to_string())
        .filter(|s| !s.is_empty())
}

pub(crate) fn own_seeder_uri2() -> Option<String> {
    crate::DB
        .get(OWN_URI2_KEY)
        .ok()
        .flatten()
        .map(|v| String::from_utf8_lossy(&v).to_string())
        .filter(|s| !s.is_empty())
}

/// "yandex" | "mailru" | "gdrive"/"google" → SeederProvider.
pub(crate) fn parse_provider(s: Option<&str>) -> Result<SeederProvider, String> {
    match s.unwrap_or("yandex") {
        "yandex" => Ok(SeederProvider::Yandex),
        "mailru" => Ok(SeederProvider::Mailru),
        "gdrive" | "google" => Ok(SeederProvider::Google),
        other => Err(format!("провайдер '{other}' не поддерживается")),
    }
}

pub(crate) fn known_seeders_internal() -> Vec<String> {
    if let Some(v) = crate::DB.get(SEEDERS_KEY).ok().flatten() {
        let list = serde_json::from_slice::<Vec<String>>(&v).unwrap_or_default();
        // Самолечение: пустой реестр = клиент слеп (нет якорей сети). Такое
        // случалось у сборок без CLOUD_SEEDERS в .env — пересеваем bootstrap,
        // иначе без трекеров каталог навсегда остаётся пустым.
        if !list.is_empty() {
            return list;
        }
    }
    seed_bootstrap()
}

/// Первый запуск / пустой реестр: сеем стартовые якоря сети.
fn seed_bootstrap() -> Vec<String> {
    let defaults = default_seeders();
    if let Ok(val) = serde_json::to_vec(&defaults) {
        let _ = crate::DB.insert(SEEDERS_KEY, val);
    }
    // Помечаем стартовые URI как "bootstrap" — иначе UI показывал бы их
    // как "manual" после первого cloud_seeder_registry_set из фронта.
    let mut origins = origins_map();
    for u in &defaults {
        origins.entry(u.clone()).or_insert_with(|| "bootstrap".to_string());
    }
    save_origins(&origins);
    defaults
}

/// Стартовые Cloud Seeder URI (бутстрап-якоря сети): runtime-переменная
/// CLOUD_SEEDERS, иначе строка CLOUD_SEEDERS= из frontend/.env (вшита при сборке).
static ENV_FILE: &str = include_str!("../../.env");

/// Вшитый последний рубеж: якорные сидеры сети SmartNet. Используются, когда
/// ни runtime-переменная, ни .env сборки не дали ни одного валидного URI —
/// сборка из чистого репозитория (у которого .env в .gitignore) обязана
/// видеть «белые» dApps сразу, даже при лежащих трекерах.
const BUILTIN_SEEDERS: &[&str] = &[
    "sn://seed/y@UA5tC3XEu6wPK+7uadVS2NavDA9ThHEnK+jPWly6gRgj9dbQkN6b2bUSrX+1jdr6q/J6bpmRyOJonT3VoXnDag==@039d6f18cee2981098b860560f47da3e0ceb7ac946ace065b6e7288f78986ef35b",
];

fn default_seeders() -> Vec<String> {
    let raw = std::env::var("CLOUD_SEEDERS").unwrap_or_else(|_| {
        ENV_FILE
            .lines()
            .find_map(|l| l.trim().strip_prefix("CLOUD_SEEDERS=").map(str::to_string))
            .unwrap_or_default()
    });
    let list: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| s.starts_with("sn://seed/"))
        .map(str::to_string)
        .collect();
    if !list.is_empty() {
        return list;
    }
    BUILTIN_SEEDERS.iter().map(|s| s.to_string()).collect()
}

#[tauri::command]
pub(crate) fn cloud_seeder_registry_get() -> Vec<String> {
    known_seeders_internal()
}

// ── Указатель обновления клиента (update.enc) ───────────────────────────────
//  Доверенный сидер (его pubkey входит в .env CLOUD_SEEDERS) публикует рядом
//  с dapps list зашифрованный файл update.enc — строку vendorField последнего
//  анонса обновления из блокчейна: "smartnet:<version>:<btih>". Пишется ТОЛЬКО
//  когда blockchain api доступен и версия изменилась. Клиенты без доступа к
//  API блокчейна читают файл как fallback, расшифровывают локально и качают
//  обновление торрентом по magnet, как обычно.

/// Sled (legacy, ключи теперь per-диск cfg:seeder_update_pub_ver:<code>):
/// версия клиента, чей указатель уже опубликован.
#[allow(dead_code)]
const UPDATE_PUB_VER_KEY: &[u8] = b"cfg:seeder_update_pub_ver";
/// Имя файла-указателя в корне /ntfryseed.
const UPDATE_FILE: &str = "update.enc";

/// Pubkeys доверенных сидеров из .env CLOUD_SEEDERS.
fn trusted_env_pubkeys() -> Vec<String> {
    default_seeders()
        .iter()
        .filter_map(|u| CloudSeederUri::parse(u).ok().map(|p| p.public_key))
        .collect()
}

/// Разбор строки-указателя "<app>:<version>:<btih>" (формат vendorField).
/// Правило формата: smartnet — имя приложения, дальше версия и hash magnet.
fn parse_update_memo(s: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = s.trim().splitn(3, ':').collect();
    if parts.len() != 3 || parts[0].trim() != crate::updater::APP_NAME {
        return None;
    }
    let (ver, hash) = (parts[1].trim(), parts[2].trim());
    if ver.is_empty() || hash.is_empty() {
        return None;
    }
    Some((ver.to_string(), hash.to_string()))
}

/// Валидный анонс обновления клиента? sender = UPDATE_CREATOR,
/// vendorField = "smartnet:<version>:<btih>". Возвращает (version, btih).
fn tx_is_update(tx: &serde_json::Value) -> Option<(String, String)> {
    if tx["sender"].as_str() != Some(crate::updater::UPDATE_CREATOR) {
        return None;
    }
    parse_update_memo(tx["vendorField"].as_str().unwrap_or(""))
}

/// Последний валидный анонс обновления из блокчейна (блокирующий —
/// вызывать из spawn_blocking). Возвращает (version, memo-строка
/// "smartnet:<ver>:<btih>" — именно она пишется в update.enc).
/// None = blockchain api недоступен либо валидного анонса нет — тогда
/// указатель НЕ публикуется.
fn latest_update_tx() -> Option<(String, String)> {
    let path = format!(
        "/transactions?recipientId={}&page=1&limit=50&orderBy=timestamp:desc",
        crate::updater::UPDATE_RECIPIENT
    );
    let txt = crate::sth_node::node_get(&path, std::time::Duration::from_secs(12)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    for tx in v.get("data")?.as_array()? {
        if let Some((ver, _)) = tx_is_update(tx) {
            let memo = tx["vendorField"].as_str().unwrap_or("").trim().to_string();
            return Some((ver, memo));
        }
    }
    None
}

/// Fallback-чтение анонса обновления: update.enc у ДОВЕРЕННЫХ (.env
/// CLOUD_SEEDERS) сидеров. Внутри файла — зашифрованная строка
/// "smartnet:<ver>:<btih>"; расшифровка локально ключом из pubkey автора
/// (проверка подписи внутри seeder::open). Возвращает самую свежую
/// (version, btih) среди ответивших сидеров.
pub(crate) async fn fetch_update_from_cloud() -> Option<(String, String)> {
    let mut best: Option<(String, String)> = None;
    for uri in default_seeders() {
        let Ok(parsed) = CloudSeederUri::parse(&uri) else { continue };
        let blob = match fetch_public(&parsed, UPDATE_FILE).await {
            Ok(b) => b,
            Err(_) => continue,
        };
        let pk = parsed.public_key.clone();
        let Ok(Ok(payload)) =
            tauri::async_runtime::spawn_blocking(move || seeder::open(&blob, &pk)).await
        else {
            continue;
        };
        let Some((ver, hash)) = parse_update_memo(&payload) else { continue };
        let newer = best
            .as_ref()
            .map(|(bv, _)| crate::updater::version_gt(&ver, bv))
            .unwrap_or(true);
        if newer {
            best = Some((ver, hash));
        }
    }
    if let Some((v, _)) = &best {
        crate::syslog::log("seeder", format!("update.enc: cloud update pointer v{v}"));
    }
    best
}

#[tauri::command]
pub(crate) fn cloud_seeder_registry_set(list: Vec<String>) -> Result<(), String> {
    let val = serde_json::to_vec(&list).map_err(|e| e.to_string())?;
    crate::DB.insert(SEEDERS_KEY, val).map_err(|e| e.to_string())?;
    // Синхронизация карты источников: новые URI, у которых ещё нет origin,
    // считаются ручным добавлением (пользователь либо вставил URI в поле
    // "+ ADD TO REGISTRY", либо экспорт/импорт реестра). Для URI, ушедших
    // из списка (удалены пользователем), origin тоже удаляем — иначе
    // карта разрослась бы бесконечно.
    let own = own_seeder_uri();
    let mut origins = origins_map();
    let in_list: std::collections::HashSet<&String> = list.iter().collect();
    origins.retain(|k, _| in_list.contains(k));
    for u in &list {
        if u.trim().is_empty() {
            continue;
        }
        let default_src = if own.as_deref() == Some(u.as_str()) { "own" } else { "manual" };
        origins.entry(u.clone()).or_insert_with(|| default_src.to_string());
    }
    save_origins(&origins);
    Ok(())
}

/// Карта uri → источник добавления в реестр. Пусто → все URI ещё "unknown".
pub(crate) fn origins_map() -> HashMap<String, String> {
    crate::DB
        .get(ORIGINS_KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}

fn save_origins(map: &HashMap<String, String>) {
    if let Ok(val) = serde_json::to_vec(map) {
        let _ = crate::DB.insert(ORIGINS_KEY, val);
    }
}

/// UI-помощник: возвращает {uri: origin} для всех известных сидеров.
#[tauri::command]
pub(crate) fn cloud_seeder_registry_origins_get() -> HashMap<String, String> {
    origins_map()
}

fn signing_key() -> Result<([u8; 32], [u8; 33]), String> {
    let secret = crate::SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("кошелёк заблокирован — разблокируйте SmartNet")?;
    ntfry::crypto::derive_address_keypair(&secret).map_err(|e| e.to_string())
}

// ── Статус сидеров: uri → результат последней проверки ─────────────────────

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SeederStatus {
    pub ok: bool,
    pub checked_at: String,
    #[serde(default)]
    pub last_ok: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

fn status_map() -> HashMap<String, SeederStatus> {
    crate::DB
        .get(STATUS_KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}

fn record_status(uri: &str, err: Option<String>) {
    let mut map = status_map();
    let now = crate::crypto::now_iso();
    let e = map.entry(uri.to_string()).or_default();
    e.checked_at = now.clone();
    e.ok = err.is_none();
    if err.is_none() {
        e.last_ok = Some(now);
    }
    e.error = err;
    if let Ok(val) = serde_json::to_vec(&map) {
        let _ = crate::DB.insert(STATUS_KEY, val);
    }
}

#[tauri::command]
pub(crate) fn cloud_seeder_status_get() -> HashMap<String, SeederStatus> {
    status_map()
}

// ── Карта уже залитых архивов dApp: appId → merkle ─────────────────────────

/// Ключ карты «appId → merkle»: раздельно на КАЖДЫЙ диск (primary/secondary
/// могут быть разными провайдерами и содержать разный набор архивов).
fn uploaded_key(prov: SeederProvider, secondary: bool) -> Vec<u8> {
    format!("cfg:seeder_uploaded:{}{}", prov.code(), if secondary { ":2" } else { "" })
        .into_bytes()
}

/// Имена файлов логотипа dApp в папке приложения (порядок как в seed_all).
fn logo_names(id: &str) -> Vec<String> {
    vec![
        format!("{id}-logo.png"),
        "logo.png".into(),
        "logo.PNG".into(),
        "icon.png".into(),
    ]
}

/// Имена файлов DAPP Preview 440×180 (порядок как в seed_all).
fn preview_names(id: &str) -> Vec<String> {
    vec![format!("{id}-preview.png"), format!("{id}-preview.jpg")]
}

/// blake3-хэш каталожного медиа dApp (== iroh-хэшу блоба из seed_all).
/// Возвращает (hash, имя файла) первого валидного кандидата, иначе пустые строки.
fn own_media_hash(dir: &std::path::Path, names: &[String], png_only: bool) -> (String, String) {
    for n in names {
        if let Ok(b) = std::fs::read(dir.join(n)) {
            let ok = !b.is_empty()
                && b.len() <= crate::p2p::LOGO_MAX_BYTES
                && (b.starts_with(&crate::p2p::PNG_MAGIC)
                    || (!png_only && b.starts_with(&crate::p2p::JPEG_MAGIC)));
            if ok {
                return (blake3::hash(&b).to_hex().to_string(), n.clone());
            }
        }
    }
    (String::new(), String::new())
}

/// Ключ AES-256-GCM для шифрования каталожных медиа в облаке. Детерминирован
/// от компонентов имени файла (safeId, prefix, blake3-хэш оригинала) —
/// отдельные индексы не нужны: любой клиент выведет ключ из имени
/// `<safeId>-<prefix>-<hash>.<ext>.enc`. Это обфускация от сканеров облачных
/// дисков (картинки не лежат в открытом виде), а не криптографическая тайна;
/// подлинность гарантирует blake3-проверка plaintext после расшифровки.
fn media_key(safe_id: &str, prefix: &str, hash: &str) -> [u8; 32] {
    let ikm = format!("{safe_id}|{prefix}|{}", hash.to_ascii_lowercase());
    blake3::derive_key("ntfry discovery media enc v1", ikm.as_bytes())
}

/// Репликация каталожных медиа в `/ntfryseed/discovery_cache/` — облачный
/// fallback картинок, когда недоступны seeds/peers. Заливаются:
///   1) медиа СОБСТВЕННЫХ dApps (в т.ч. бесплатных link-only) из data/dapps/<id>/ —
///      публикатор офлайн ≠ пустые карточки у других клиентов;
///   2) кэш чужих dApps `data/discovery_cache/*` (не более 300 самых свежих).
/// Только новые файлы (имена content-addressed: новый хэш = новое имя).
/// В облако файлы кладутся ТОЛЬКО зашифрованными (AES-256-GCM, `.enc`),
/// ключ детерминированно выводится из имени — см. [`media_key`].
async fn replicate_discovery_media(
    app: &tauri::AppHandle,
    prov: SeederProvider,
    secondary: bool,
    token: &str,
) -> usize {
    let mut jobs: Vec<(String, Vec<u8>)> = Vec::new();
    // 1) собственные dApps: их медиа лежит в data/dapps/<id>/, а не в discovery_cache
    let dapps_dir = crate::ensure_data_dir(app).join("dapps");
    for m in crate::apps::list_apps() {
        if m.status.as_deref() == Some("draft") || m.private == Some(true) {
            continue;
        }
        let dir = dapps_dir.join(&m.id);
        let safe = crate::sanitize(&m.id);
        for (names, png_only, prefix) in [
            (logo_names(&m.id), true, "logo"),
            (preview_names(&m.id), false, "preview"),
        ] {
            let (hash, fname) = own_media_hash(&dir, &names, png_only);
            if hash.is_empty() {
                continue;
            }
            let ext = if fname.to_ascii_lowercase().ends_with(".jpg") { "jpg" } else { "png" };
            if let Ok(bytes) = std::fs::read(dir.join(&fname)) {
                // в облако — только зашифрованные .enc (ключ из имени файла)
                if let Ok(ct) =
                    crate::crypto::aes_gcm_encrypt(&media_key(&safe, prefix, &hash), &bytes)
                {
                    jobs.push((format!("{safe}-{prefix}-{hash}.{ext}.enc"), ct));
                }
            }
        }
    }
    // 2) кэш чужих dApps (300 самых свежих папок discovery_cache)
    let root = crate::ensure_data_dir(app).join("discovery_cache");
    if let Ok(rd) = std::fs::read_dir(&root) {
        let mut dirs: Vec<(std::time::SystemTime, std::path::PathBuf)> = rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| {
                let t = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                (t, e.path())
            })
            .collect();
        dirs.sort_by(|a, b| b.0.cmp(&a.0)); // свежие первыми
        dirs.truncate(300);
        for (_, dir) in dirs {
            let app_id = dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let Ok(files) = std::fs::read_dir(&dir) else { continue };
            for f in files.flatten() {
                let fname = f.file_name().to_string_lossy().to_string();
                if !(fname.starts_with("logo-") || fname.starts_with("preview-")) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(f.path()) else { continue };
                // fname = <prefix>-<hash>.<ext> → ключ из компонентов имени
                let Some((pfx, rest)) = fname.split_once('-') else { continue };
                let Some((h, _ext)) = rest.rsplit_once('.') else { continue };
                let Ok(ct) = crate::crypto::aes_gcm_encrypt(&media_key(&app_id, pfx, h), &bytes)
                else {
                    continue;
                };
                jobs.push((format!("{app_id}-{fname}.enc"), ct)); // плоско: <appId>-logo-<hash>.png.enc
            }
        }
    }
    // 3) заливка только новых имён (dedup-карта в Sled per провайдер/диск)
    let key = format!(
        "cfg:seeder_media_uploaded:{}{}",
        prov.code(),
        if secondary { ":2" } else { "" }
    );
    let mut map: std::collections::HashSet<String> = crate::DB
        .get(key.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();
    let mut uploaded = 0usize;
    for (remote, bytes) in jobs {
        // +28 — AES-GCM overhead (nonce 12 + tag 16) поверх лимита оригинала
        if map.contains(&remote) || bytes.is_empty() || bytes.len() > 262_144 + 28 {
            continue;
        }
        let ok = match prov {
            SeederProvider::Google => {
                seeder::gdrive_upload_media_file(token, &remote, bytes).await.is_ok()
            }
            SeederProvider::Mailru => {
                seeder::mailru_upload_media_file(token, &remote, bytes).await.is_ok()
            }
            _ => seeder::yandex_upload_media_file(token, &remote, bytes).await.is_ok(),
        };
        if ok {
            map.insert(remote);
            uploaded += 1;
        }
    }
    if uploaded > 0 {
        if let Ok(v) = serde_json::to_vec(&map) {
            let _ = crate::DB.insert(key.as_bytes(), v);
        }
    }
    uploaded
}

fn uploaded_map(prov: SeederProvider, secondary: bool) -> HashMap<String, String> {
    if let Some(v) = crate::DB.get(uploaded_key(prov, secondary)).ok().flatten() {
        return serde_json::from_slice(&v).unwrap_or_default();
    }
    // Миграция: старая единая карта принадлежит primary-диску текущего own-URI.
    if !secondary {
        let legacy_prov = own_seeder_uri()
            .and_then(|u| CloudSeederUri::parse(&u).ok())
            .map(|p| p.provider);
        if legacy_prov == Some(prov) {
            if let Some(v) = crate::DB.get(UPLOADED_KEY).ok().flatten() {
                return serde_json::from_slice(&v).unwrap_or_default();
            }
        }
    }
    HashMap::new()
}

fn set_uploaded(prov: SeederProvider, secondary: bool, id: &str, merkle: &str) {
    let mut map = uploaded_map(prov, secondary);
    map.insert(id.to_string(), merkle.to_string());
    if let Ok(val) = serde_json::to_vec(&map) {
        let _ = crate::DB.insert(uploaded_key(prov, secondary), val);
    }
}

/// Сброс merkle-маркера заливки: после удаления архива с облака повторная
/// публикация этого dApp (если он снова появится) не будет пропущена гейтом.
/// Чистим маркер во ВСЕХ картах (провайдеры × primary/secondary + legacy).
fn remove_uploaded(id: &str) {
    let mut keys: Vec<Vec<u8>> = vec![UPLOADED_KEY.to_vec()];
    for prov in [SeederProvider::Yandex, SeederProvider::Mailru, SeederProvider::Google] {
        for secondary in [false, true] {
            keys.push(uploaded_key(prov, secondary));
        }
    }
    for key in keys {
        if let Some(v) = crate::DB.get(&key).ok().flatten() {
            let mut map: HashMap<String, String> =
                serde_json::from_slice(&v).unwrap_or_default();
            if map.remove(id).is_some() {
                if let Ok(val) = serde_json::to_vec(&map) {
                    let _ = crate::DB.insert(key, val);
                }
            }
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PublishProgress {
    phase: String, // "meta" | "archive" | "done"
    index: usize,
    total: usize,
    app_id: String,
    size_bytes: u64,
    /// размер tar ДО zstd-сжатия — для показа степени сжатия в UI
    raw_bytes: u64,
    status: String, // "packing" | "uploading" | "done" | "error"
    kbps: u64,
    /// байт уже передано облаку в текущей заливке — для live-скорости в UI
    uploaded_bytes: u64,
    /// номер текущей попытки заливки (1..=3) — бейдж «попытка N/3» при ретраях
    attempt: u32,
    error: Option<String>,
}

fn emit_progress(app: &tauri::AppHandle, p: &PublishProgress) {
    use tauri::Emitter;
    let _ = app.emit("seeder-publish-progress", p.clone());
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeederPublishOut {
    uri: String,
    public_key: String,
    dapps: usize,
    nodes: usize,
    seeders: usize,
    /// сколько архивов dApp сейчас есть в облаке (после этой публикации)
    archives: usize,
    updated: String,
}

/// Стейт сети для публикации: (dapps.json, nodes.json, seeders.json)
/// + счётчики + кандидаты на заливку архива (id, merkle) — non-draft dApps,
/// чья папка существует в data/dapps.
#[allow(clippy::type_complexity)]
fn build_network_state(
    dapps_dir: &std::path::Path,
    gated_vdapps: bool,
) -> Result<(Vec<(String, String)>, usize, usize, usize, Vec<(String, String)>), String> {
    let updated = crate::crypto::now_iso();

    // dApps: созданные и установленные (реестр app:*), кроме черновиков
    let announced: std::collections::HashSet<String> =
        crate::announced_apps_internal().into_iter().collect();
    let mut candidates: Vec<(String, String)> = Vec::new();
    let dapps: Vec<serde_json::Value> = crate::apps::list_apps()
        .into_iter()
        .filter(|m| m.status.as_deref() != Some("draft"))
        .map(|m| {
            let is_priv = m.private == Some(true);
            // Архив публикуем только для полноценного dApp: папка без
            // index.html дала бы получателям гарантированный 404 по sth://.
            // Приватный dApp: вместо index.html — E2EE-контейнер app.enc.
            let has_folder = dapps_dir.join(&m.id).is_dir();
            let entry = if is_priv { crate::private_dapp::CONTAINER_FILE } else { "index.html" };
            let valid = has_folder && dapps_dir.join(&m.id).join(entry).is_file();
            if has_folder && !valid {
                crate::syslog::log(
                    "seeder",
                    format!("publish: archive {} skipped — no {} in folder", m.id, entry),
                );
            }
            if valid {
                candidates.push((m.id.clone(), m.merkle.clone()));
            }
            // Приватный dApp: description несёт reveal-payload (§P1§…) — как в
            // трекерных анонсах; клиенты из списка доступа расшифруют
            // имя/описание, остальные скроют dApp полностью (fail-closed).
            let description = if is_priv {
                serde_json::Value::String(
                    crate::private_dapp::reveal_payload_for(&m.id).unwrap_or_default(),
                )
            } else {
                serde_json::json!(m.description)
            };
            // Фаза 2 медиа: хэши logo/preview в dapps.enc — клиент без живых
            // пиров узнаёт, ЧТО качать, и берёт файл из /ntfryseed/discovery_cache
            // (blake3 == iroh-хэшу блоба, см. own_media_hash).
            let (logo_hash, preview_hash) = if is_priv || !valid {
                (String::new(), String::new())
            } else {
                let dir = dapps_dir.join(&m.id);
                (
                    own_media_hash(&dir, &logo_names(&m.id), true).0,
                    own_media_hash(&dir, &preview_names(&m.id), false).0,
                )
            };
            serde_json::json!({
                "id": m.id,
                "name": m.name,
                "description": description,
                "version": m.version,
                "merkle": m.merkle,
                "blob": m.blob,
                "tags": m.tags,
                "owner": m.owner,
                "updated": m.updated,
                "announced": announced.contains(&m.id),
                "logo": logo_hash,
                "preview": preview_hash,
                // рядом с метаданными в облаке лежит зашифрованный tar dApp
                "archive": valid,
            })
        })
        .collect();

    // Ноды: таблица найденных пиров (mDNS + трекеры) с nodeId
    let peers = crate::p2p::discovered_peers();
    let nodes = serde_json::to_value(&peers).map_err(|e| e.to_string())?;

    // Известные Cloud Seeder-ы (цепная реакция: сеть размазывает сама себя)
    let seeders = known_seeders_internal();

    let n = (dapps.len(), peers.len(), seeders.len());
    let wrap = |items: serde_json::Value| -> Result<String, String> {
        serde_json::to_string(&serde_json::json!({ "v": 1, "updated": updated, "items": items }))
            .map_err(|e| e.to_string())
    };
    let files = vec![
        ("dapps.enc".to_string(), wrap(serde_json::Value::Array(dapps))?),
        ("nodes.enc".to_string(), wrap(nodes)?),
        ("seeders.enc".to_string(), wrap(serde_json::json!(seeders))?),
    ];
    let mut files = files;
    // vDapps: анонс official-списков для пиров без доступа к блокчейну.
    // Публикуют ТОЛЬКО доверенные разработчики и только если данные изменились.
    if let Some(f) = build_vdapps_file(&updated, gated_vdapps) {
        files.push(f);
    }
    Ok((files, n.0, n.1, n.2, candidates))
}

/// Хеш+счётчики vDapps, ожидающие подтверждения успешной публикации.
static PENDING_VDAPPS: Lazy<Mutex<Option<(String, u64, u64, u64)>>> =
    Lazy::new(|| Mutex::new(None));
const VDAPPS_PUB_HASH_KEY: &[u8] = b"official:vdapps_pub_hash";
const VDAPPS_PUB_INFO_KEY: &[u8] = b"official:vdapps_pub_info";

/// Статус vDapps-анонса для страницы Seeders.
#[derive(Debug, serde::Serialize)]
pub(crate) struct VdappsStatus {
    /// текущий пользователь — доверенный разработчик (может анонсировать)
    eligible: bool,
    published: bool,
    ts: u64,
    devs: u64,
    dapps: u64,
    seeders: u64,
}

#[tauri::command]
pub(crate) async fn vdapps_status() -> VdappsStatus {
    // spawn_blocking: чтение vault-файла и Sled — не на main-потоке
    tauri::async_runtime::spawn_blocking(vdapps_status_inner)
        .await
        .unwrap_or_else(|_| vdapps_status_inner())
}

fn vdapps_status_inner() -> VdappsStatus {
    let me = crate::load_vault_internal()
        .map(|v| v.address)
        .unwrap_or_default();
    let eligible = !me.is_empty() && crate::official::developers().contains(&me);
    let info: serde_json::Value = crate::DB
        .get(VDAPPS_PUB_INFO_KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or(serde_json::Value::Null);
    let g = |k: &str| info.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    VdappsStatus {
        eligible,
        published: !info.is_null(),
        ts: g("ts"),
        devs: g("devs"),
        dapps: g("dapps"),
        seeders: g("seeders"),
    }
}

/// vdapps.enc: {developers[], dapps[], seeders{appId:[nodeId]}}.
/// Гейт «только новое»: хеш считается по стабильным спискам (devs+dapps),
/// волатильная карта сидеров не триггерит лишние заливки на облако.
/// `gated=false` (secondary-зеркало): гейт и PENDING-коммит пропускаются —
/// файл мелкий, зеркало всегда получает свежий анонс.
fn build_vdapps_file(updated: &str, gated: bool) -> Option<(String, String)> {
    let me = crate::load_vault_internal()
        .map(|v| v.address)
        .unwrap_or_default();
    if me.is_empty() {
        return None;
    }
    let devs = crate::official::developers();
    if !devs.contains(&me) {
        return None; // vDapps публикуют только доверенные разработчики
    }
    let dapps = crate::official::official_dapps();
    if devs.is_empty() && dapps.is_empty() {
        return None; // нет данных (блокчейн ещё не сканирован) — нечего анонсировать
    }
    let digest = hex::encode(crate::crypto::sha256(
        format!("{}|{}", devs.join(","), dapps.join(",")).as_bytes(),
    ));
    if gated {
        let prev = crate::DB
            .get(VDAPPS_PUB_HASH_KEY)
            .ok()
            .flatten()
            .map(|v| String::from_utf8_lossy(&v).to_string())
            .unwrap_or_default();
        if prev == digest {
            return None; // информация не менялась — не гоняем облако впустую
        }
    }
    let seeders_map = crate::p2p::official_app_providers();
    let items = serde_json::json!({
        "developers": devs,
        "dapps": dapps,
        // известные nodeId сидеров official-приложений (если есть)
        "seeders": seeders_map,
    });
    let body = serde_json::to_string(
        &serde_json::json!({ "v": 1, "updated": updated, "items": items }),
    )
    .ok()?;
    if gated {
        *PENDING_VDAPPS.lock().unwrap() = Some((
            digest,
            devs.len() as u64,
            dapps.len() as u64,
            seeders_map.len() as u64,
        ));
    }
    Some(("vdapps.enc".to_string(), body))
}

/// Публикация стейта сети в /ntfryseed на облаке автора (Яндекс Диск,
/// Облако Mail.ru или Google Drive) + заливка зашифрованных tar-архивов dApp
/// (только изменившихся — гейт по merkle). Возвращает готовый Seeder-URI.
/// `provider`: "yandex"/"gdrive" (token = OAuth) | "mailru" (token = email:пароль_приложения).
/// `secondary`: публикация на ВТОРОЙ (зеркальный) диск — свой URI пишется в
/// отдельный слот и не перекрывает основной.
#[tauri::command]
pub(crate) async fn cloud_seeder_publish(
    app: tauri::AppHandle,
    token: String,
    provider: Option<String>,
    mailru_weblink: Option<String>,
    secondary: Option<bool>,
) -> Result<SeederPublishOut, String> {
    let prov = parse_provider(provider.as_deref())?;
    let secondary = secondary.unwrap_or(false);
    if token.trim().is_empty() {
        return Err(match prov {
            SeederProvider::Mailru => "нужны креды Mail.ru: email:пароль_приложения".into(),
            SeederProvider::Google => "нужен OAuth-токен Google Drive".into(),
            _ => "нужен OAuth-токен Yandex Disk".into(),
        });
    }
    let (sk, pk) = signing_key()?;
    let pk_hex = hex::encode(pk);
    let dapps_dir = crate::ensure_data_dir(&app).join("dapps");
    // Гейт версии update.enc — раздельно на каждый диск (провайдер × роль).
    let update_ver_key =
        format!("cfg:seeder_update_pub_ver:{}{}", prov.code(), if secondary { ":2" } else { "" });

    // Sled + подпись + шифрование — блокирующие, уводим с main-потока
    let dd = dapps_dir.clone();
    let pk_hex2 = pk_hex.clone();
    let update_key2 = update_ver_key.clone();
    let gated_vdapps = !secondary;
    let (files, n_dapps, n_nodes, n_seeders, updated, candidates, update_ver) =
        tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
            let (payloads, d, n, s, cands) = build_network_state(&dd, gated_vdapps)?;
            let updated = crate::crypto::now_iso();
            let mut files: Vec<(String, Vec<u8>)> = Vec::with_capacity(payloads.len() + 1);
            for (name, payload) in payloads {
                files.push((name, seeder::seal(&payload, &sk).map_err(|e| e.to_string())?));
            }
            // Доверенный сидер (.env CLOUD_SEEDERS) → рядом с dapps list публикуем
            // указатель обновления клиента (update.enc). Гейт по версии: одинаковую
            // версию повторно на облако не заливаем. Ошибки не валят публикацию.
            let mut update_ver: Option<String> = None;
            if trusted_env_pubkeys().contains(&pk_hex2) {
                match latest_update_tx() {
                    Some((ver, payload)) => {
                        let last = crate::DB
                            .get(update_key2.as_bytes())
                            .ok()
                            .flatten()
                            .map(|v| String::from_utf8_lossy(&v).to_string());
                        if last.as_deref() == Some(ver.as_str()) {
                            crate::syslog::log(
                                "seeder",
                                format!("update.enc: v{ver} already on cloud — skipped"),
                            );
                        } else {
                            match seeder::seal(&payload, &sk) {
                                Ok(sealed) => {
                                    files.push((UPDATE_FILE.to_string(), sealed));
                                    update_ver = Some(ver);
                                }
                                Err(e) => crate::syslog::log(
                                    "seeder",
                                    format!("update.enc: encryption failed — {e}"),
                                ),
                            }
                        }
                    }
                    None => crate::syslog::log(
                        "seeder",
                        "update.enc: no blockchain update announcement — skipped"
                            .to_string(),
                    ),
                }
            }
            Ok((files, d, n, s, updated, cands, update_ver))
        })
        .await
        .map_err(|e| e.to_string())??;

    emit_progress(&app, &PublishProgress {
        phase: "meta".into(), index: 0, total: 0, app_id: String::new(),
        size_bytes: 0, raw_bytes: 0, status: "uploading".into(), kbps: 0, uploaded_bytes: 0, attempt: 1, error: None,
    });
    let folder_token = match prov {
        SeederProvider::Google => seeder::gdrive_upload_seed(token.trim(), &files).await,
        SeederProvider::Mailru => {
            // Weblink, если уже известен: явный (пользователь один раз опубликовал
            // папку вручную и вставил ссылку) или из своего прошлого m@-URI —
            // тогда o2/api-m1 не вызываются вовсе (обязательно для VK ID-логинов).
            let known = mailru_weblink
                .map(|w| w.trim().to_string())
                .filter(|w| !w.is_empty())
                .or_else(|| {
                    let own = if secondary { own_seeder_uri2() } else { own_seeder_uri() };
                    own.and_then(|u| CloudSeederUri::parse(&u).ok())
                        .filter(|p| p.provider == SeederProvider::Mailru)
                        .map(|p| p.folder_token)
                });
            seeder::mailru_upload_seed(token.trim(), &files, known.as_deref())
                .await
                .map_err(|e| {
                    // Файлы уже залиты по WebDAV — не удалась только публикация
                    // папки (o2 недоступен для VK ID). Маркер для UI: показать
                    // аварийный ввод публичной ссылки.
                    if known.is_none() {
                        ntfry::error::StorageError::MissingToken(format!("MR_NEED_WEBLINK: {e}"))
                    } else {
                        e
                    }
                })
        }
        _ => seeder::yandex_upload_seed(token.trim(), &files).await,
    }
    .map_err(|e| e.to_string())?;

    // Корневые файлы (включая update.enc) легли на облако → фиксируем
    // опубликованную версию указателя обновления (skip-гейт следующих публикаций).
    if let Some(ver) = &update_ver {
        let _ = crate::DB.insert(update_ver_key.as_bytes(), ver.as_bytes());
        crate::syslog::log(
            "seeder",
            format!("update.enc: update pointer v{ver} published to /ntfryseed"),
        );
    }
    // vdapps.enc успешно опубликован → фиксируем хеш + сводку анонса
    if let Some((h, nd, na, ns)) = PENDING_VDAPPS.lock().unwrap().take() {
        let _ = crate::DB.insert(VDAPPS_PUB_HASH_KEY, h.as_bytes());
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let info = serde_json::json!({ "ts": ts, "devs": nd, "dapps": na, "seeders": ns });
        let _ = crate::DB.insert(VDAPPS_PUB_INFO_KEY, info.to_string().as_bytes());
        crate::syslog::log(
            "seeder",
            format!("vdapps.enc: announced {nd} developers, {na} dApps, {ns} seeders"),
        );
    }

    // Архивы dApp: tar → seal_bytes → PUT dapps/<appId>.enc. Только изменившиеся
    // по merkle относительно прошлой заливки; ошибка одного архива не валит
    // публикацию. Каждый шаг транслируется в UI событием seeder-publish-progress.
    let uploaded = uploaded_map(prov, secondary);
    let todo: Vec<(String, String)> = candidates
        .iter()
        .filter(|(id, m)| uploaded.get(id).map(|u| u != m).unwrap_or(true))
        .cloned()
        .collect();
    let total = todo.len();
    let mut new_archives = 0usize;
    for (i, (id, merkle)) in todo.iter().enumerate() {
        let mut prog = PublishProgress {
            phase: "archive".into(),
            index: i + 1,
            total,
            app_id: id.clone(),
            size_bytes: 0,
            raw_bytes: 0,
            status: "packing".into(),
            kbps: 0,
            uploaded_bytes: 0,
            attempt: 1,
            error: None,
        };
        emit_progress(&app, &prog);
        let dir = dapps_dir.join(id);
        let sealed = tauri::async_runtime::spawn_blocking(move || -> Result<(u64, Vec<u8>), String> {
            let mut tarball: Vec<u8> = Vec::new();
            {
                let mut b = tar::Builder::new(&mut tarball);
                b.append_dir_all(".", &dir).map_err(|e| e.to_string())?;
                b.finish().map_err(|e| e.to_string())?;
            }
            let raw = tarball.len() as u64;
            let sealed = seeder::seal_bytes(&tarball, &sk).map_err(|e| e.to_string())?;
            Ok((raw, sealed))
        })
        .await
        .map_err(|e| e.to_string())?;
        let (raw, sealed) = match sealed {
            Ok(s) => s,
            Err(e) => {
                crate::syslog::log("seeder", format!("archive {id}: build failed — {e}"));
                prog.status = "error".into();
                prog.error = Some(e);
                emit_progress(&app, &prog);
                continue;
            }
        };
        let size = sealed.len();
        prog.size_bytes = size as u64;
        prog.raw_bytes = raw;
        prog.status = "uploading".into();
        emit_progress(&app, &prog);
        let name = format!("{}.enc", crate::sanitize(id));
        let started = std::time::Instant::now();
        // Live-скорость: колбэк из стрима тела → событие в UI (не чаще ~2 раз/сек).
        // Смена attempt (ретрай после обрыва) пробрасывается сразу, без троттлинга.
        let on_progress: Option<Box<dyn Fn(u64, u32) + Send + Sync>> = {
            use std::sync::atomic::{AtomicU64, Ordering};
            let app2 = app.clone();
            let tpl = prog.clone();
            let last_ms = std::sync::Arc::new(AtomicU64::new(0));
            let last_attempt = std::sync::Arc::new(AtomicU64::new(1));
            Some(Box::new(move |sent: u64, attempt: u32| {
                let ms = started.elapsed().as_millis() as u64;
                let prev = last_ms.load(Ordering::Relaxed);
                let attempt_changed = last_attempt.swap(attempt as u64, Ordering::Relaxed) != attempt as u64;
                if !attempt_changed && ms.saturating_sub(prev) < 450 && sent < tpl.size_bytes {
                    return;
                }
                last_ms.store(ms, Ordering::Relaxed);
                let mut p = tpl.clone();
                p.uploaded_bytes = sent;
                p.attempt = attempt;
                p.kbps = ((sent as f64 / 1024.0) / (ms as f64 / 1000.0).max(0.001)) as u64;
                emit_progress(&app2, &p);
            }))
        };
        match match prov {
            SeederProvider::Mailru => {
                seeder::mailru_upload_seed_file(token.trim(), &name, sealed, on_progress).await
            }
            SeederProvider::Google => {
                seeder::gdrive_upload_seed_file(token.trim(), &name, sealed, on_progress).await
            }
            _ => {
                drop_legacy_resume_state();
                seeder::yandex_upload_seed_file(token.trim(), &name, sealed, on_progress).await
            }
        } {
            Ok(uploaded) => {
                set_uploaded(prov, secondary, id, merkle);
                new_archives += 1;
                let secs = started.elapsed().as_secs_f64().max(0.001);
                prog.status = "done".into();
                prog.uploaded_bytes = size as u64;
                prog.kbps = ((size as f64 / 1024.0) / secs) as u64;
                emit_progress(&app, &prog);
                if uploaded {
                    crate::syslog::log(
                        "seeder",
                        format!(
                            "archive {id} uploaded to /ntfryseed ({} KB → {} KB, {} KB/s)",
                            raw / 1024,
                            size / 1024,
                            prog.kbps
                        ),
                    );
                } else {
                    crate::syslog::log(
                        "seeder",
                        format!(
                            "archive {id}: identical already in cloud (sha256 matched) — upload skipped, saved {} KB",
                            size / 1024
                        ),
                    );
                }
            }
            Err(e) => {
                prog.status = "error".into();
                prog.error = Some(e.to_string());
                emit_progress(&app, &prog);
                crate::syslog::log("seeder", format!("archive {id}: upload failed — {e}"));
            }
        }
    }
    emit_progress(&app, &PublishProgress {
        phase: "done".into(), index: total, total, app_id: String::new(),
        size_bytes: 0, raw_bytes: 0, status: "done".into(), kbps: 0, uploaded_bytes: 0, attempt: 1, error: None,
    });

    // Одноразовая очистка legacy-архивов из корня /ntfryseed. До миграции
    // архивы dApp лежали как dapp-<id>.enc в корне; теперь — только в
    // /ntfryseed/dapps/. Удаляем каждый устаревший файл; 404 — идемпотентно.
    // Публикацию НЕ валим при любых ошибках облака.
    let mut legacy_deleted = 0usize;
    for (id, _) in &candidates {
        let legacy_name = format!("dapp-{}.enc", crate::sanitize(id));
        let del = match prov {
            SeederProvider::Mailru => {
                seeder::mailru_delete_seed_root_file(token.trim(), &legacy_name).await
            }
            SeederProvider::Google => {
                seeder::gdrive_delete_seed_root_file(token.trim(), &legacy_name).await
            }
            _ => seeder::yandex_delete_seed_root_file(token.trim(), &legacy_name).await,
        };
        match del {
            Ok(()) => {
                legacy_deleted += 1;
                crate::syslog::log("seeder", format!("legacy: removed {legacy_name} from /ntfryseed"));
            }
            Err(ntfry::error::StorageError::NotFound(_)) => {}
            Err(e) => crate::syslog::log("seeder", format!("legacy {legacy_name}: {e}")),
        }
    }
    if legacy_deleted > 0 {
        crate::syslog::log("seeder", format!("legacy: cleaned {legacy_deleted} obsolete archives from root"));
    }

    let uri = CloudSeederUri {
        provider: prov,
        folder_token,
        public_key: pk_hex.clone(),
    }
    .to_uri();
    // Свой URI: primary — в основной слот, secondary — в зеркальный.
    // Оба вливаются в реестр как "own" и анонсируются gossip-бродкастом.
    let _ = crate::DB.insert(if secondary { OWN_URI2_KEY } else { OWN_URI_KEY }, uri.as_bytes());
    registry_add_internal(&[uri.clone()], "own");
    #[cfg(feature = "p2p")]
    crate::chat::gossip::seeder_announce_now();
    // Каталожные медиа (logo/preview) — облачный fallback, когда нет живых пиров.
    let n_media = replicate_discovery_media(&app, prov, secondary, token.trim()).await;
    crate::syslog::log(
        "seeder",
        format!(
            "publish ntfryseed: {n_dapps} dApps, {n_nodes} nodes, {n_seeders} seeders, +{new_archives} archives, +{n_media} media → {uri}"
        ),
    );
    Ok(SeederPublishOut {
        uri,
        public_key: pk_hex,
        dapps: n_dapps,
        nodes: n_nodes,
        seeders: n_seeders,
        archives: candidates.len(),
        updated,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeederFetchOut {
    provider: String,
    public_key: String,
    dapps: serde_json::Value,
    nodes: serde_json::Value,
    seeders: serde_json::Value,
    /// vDapps-анонс official-списков (null у старых сидеров).
    vdapps: serde_json::Value,
}

/// Чтение чужого сидера по URI: анонимная загрузка (без OAuth) →
/// расшифровка ключом из pubkey автора → проверка подписи.
/// Отсутствующий файл — не ошибка (null в ответе).
#[tauri::command]
pub(crate) async fn cloud_seeder_fetch(uri: String) -> Result<SeederFetchOut, String> {
    fetch_seeder_internal(&uri).await
}

/// Обёртка с записью статуса (жив/недоступен + время) и источников
/// архивов dApp в Sled/память.
pub(crate) async fn fetch_seeder_internal(uri: &str) -> Result<SeederFetchOut, String> {
    let r = fetch_seeder_impl(uri).await;
    record_status(uri, r.as_ref().err().cloned());
    if let Ok(out) = &r {
        record_app_sources(uri, &out.dapps);
        // Авто-обмен: seeders.enc чужого сидера вливается в наш реестр
        merge_seeders_from(&out.seeders);
    }
    r
}

/// Анонимное скачивание файла из опубликованной папки сидера (без OAuth).
async fn fetch_public(
    parsed: &CloudSeederUri,
    path: &str,
) -> Result<Vec<u8>, ntfry::error::StorageError> {
    match parsed.provider {
        SeederProvider::Yandex => seeder::yandex_fetch_public(&parsed.folder_token, path).await,
        SeederProvider::Mailru => seeder::mailru_fetch_public(&parsed.folder_token, path).await,
        SeederProvider::Google => seeder::gdrive_fetch_public(&parsed.folder_token, path).await,
    }
}

/// Облачный fallback каталожных медиа (logo/preview): анонимно тянет
/// `/ntfryseed/discovery_cache/<safeId>-<prefix>-<hash>.<ext>.enc` (шифрованный,
/// ключ из имени — [`media_key`]), затем legacy-имя без `.enc`, с известных
/// сидеров (живые по статусу — первыми, не более 5). Имя content-addressed:
/// blake3(plaintext) обязан совпасть с hash из анонса — облако не доверено,
/// подменённый файл отбраковывается и пробуется следующий сидер.
pub(crate) async fn fetch_discovery_media(
    safe_id: &str,
    prefix: &str,
    hash: &str,
    allow_jpeg: bool,
) -> Option<Vec<u8>> {
    let mut uris = known_seeders_internal();
    let status = status_map();
    uris.sort_by_key(|u| match status.get(u) {
        Some(s) if s.ok => 0u8,
        None => 1,
        Some(_) => 2,
    });
    uris.truncate(5);
    let want = hash.to_ascii_lowercase();
    let mut exts: Vec<&str> = vec!["png"];
    if allow_jpeg {
        exts.push("jpg");
    }
    'seeders: for uri in &uris {
        let Ok(parsed) = CloudSeederUri::parse(uri) else { continue };
        for ext in &exts {
            // сначала зашифрованный .enc, затем legacy-имя в открытом виде
            for enc in [true, false] {
                let name =
                    format!("{}-{}-{}.{}{}", safe_id, prefix, hash, ext, if enc { ".enc" } else { "" });
                let path = format!("{}/{}", seeder::SEED_MEDIA_DIR, name);
                match fetch_public(&parsed, &path).await {
                    Ok(raw) => {
                        let b = if enc {
                            // восстановление оригинала: ключ выводится из имени
                            match crate::crypto::aes_gcm_decrypt(
                                &media_key(safe_id, prefix, hash),
                                &raw,
                            ) {
                                Ok(p) => p,
                                Err(_) => {
                                    crate::syslog::log(
                                        "seeder",
                                        format!("media fallback: {path} — не расшифровался, пропуск"),
                                    );
                                    continue;
                                }
                            }
                        } else {
                            raw
                        };
                        if b.is_empty() || b.len() > crate::p2p::LOGO_MAX_BYTES {
                            continue;
                        }
                        if blake3::hash(&b).to_hex().as_str() != want {
                            crate::syslog::log(
                                "seeder",
                                format!("media fallback: {path} — blake3 не совпал, сидер пропущен"),
                            );
                            continue;
                        }
                        return Some(b);
                    }
                    Err(ntfry::error::StorageError::NotFound(_)) => {}
                    // сидер недоступен — не мучаем его другими вариантами
                    Err(_) => continue 'seeders,
                }
            }
        }
    }
    None
}

fn provider_name(p: SeederProvider) -> &'static str {
    match p {
        SeederProvider::Yandex => "yandex",
        SeederProvider::Mailru => "mailru",
        SeederProvider::Google => "gdrive",
    }
}

async fn fetch_seeder_impl(uri: &str) -> Result<SeederFetchOut, String> {
    let parsed = CloudSeederUri::parse(uri).map_err(|e| e.to_string())?;

    let mut out: Vec<serde_json::Value> = Vec::with_capacity(3);
    for file in seeder::SEED_FILES {
        let blob = match fetch_public(&parsed, file).await {
            Ok(b) => b,
            Err(ntfry::error::StorageError::NotFound(_)) => {
                out.push(serde_json::Value::Null);
                continue;
            }
            Err(e) => return Err(format!("{file}: {e}")),
        };
        let pk = parsed.public_key.clone();
        let payload = tauri::async_runtime::spawn_blocking(move || seeder::open(&blob, &pk))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{file}: {e}"))?;
        out.push(serde_json::from_str(&payload).map_err(|e| e.to_string())?);
    }
    let mut it = out.into_iter();
    let (dapps, nodes, seeders) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
    let vdapps = it.next().unwrap_or(serde_json::Value::Null);
    // fallback без блокчейна: official-списки от ДОВЕРЕННОГО издателя
    if !vdapps.is_null() {
        apply_vdapps(&parsed.public_key, &vdapps);
    }
    crate::syslog::log("seeder", format!("fetch OK: {uri}"));
    Ok(SeederFetchOut {
        provider: provider_name(parsed.provider).into(),
        public_key: parsed.public_key,
        dapps,
        nodes,
        seeders,
        vdapps,
    })
}

/// Принять vDapps: издатель обязан сам быть в известном списке DEVELOPERS
/// (цепочка доверия от админа из .env) ЛИБО быть якорным сидером сборки
/// (его pubkey вшит в CLOUD_SEEDERS — та же модель доверия, что у update.enc).
/// Иначе анонс игнорируется.
fn apply_vdapps(pubkey_hex: &str, v: &serde_json::Value) {
    let Ok(pk) = hex::decode(pubkey_hex.trim()) else {
        return;
    };
    let addr = crate::crypto::address_from_pubkey(&pk);
    let trusted_anchor = trusted_env_pubkeys()
        .iter()
        .any(|p| p.eq_ignore_ascii_case(pubkey_hex.trim()));
    if !trusted_anchor && !crate::official::developers().contains(&addr) {
        return;
    }
    let arr = |k: &str| -> Vec<String> {
        v.get("items")
            .and_then(|i| i.get(k))
            .and_then(|d| d.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (nd, na) = crate::official::merge_from_cloud(&arr("developers"), &arr("dapps"));
    if nd + na > 0 {
        crate::syslog::log(
            "official",
            format!("vDapps from {addr}: +{nd} developers, +{na} dApps (cloud fallback)"),
        );
    }
}

/// Установка dApp напрямую из облачного сидера (P2P не нужен):
/// скачивание dapps/<appId>.enc → расшифровка + проверка подписи автора →
/// квота хранилища → распаковка в data/dapps/<id> → регистрация в Sled.
/// Целостность добивается фронтендом через verify_app (merkle + подпись).
#[tauri::command]
pub(crate) async fn cloud_seeder_install(
    app: tauri::AppHandle,
    uri: String,
    app_id: String,
) -> Result<crate::Manifest, String> {
    install_from_seeder(&app, &uri, &app_id).await
}

/// Установка dApp из ЛЮБОГО известного сидера: сначала URI, у которых архив
/// этого dApp точно видели (APP_SOURCES), затем весь реестр. Первый успех —
/// результат; STORAGE_LIMIT — терминальная ошибка (везде одна и та же квота).
#[tauri::command]
pub(crate) async fn cloud_seeder_install_any(
    app: tauri::AppHandle,
    app_id: String,
) -> Result<crate::Manifest, String> {
    let mut candidates: Vec<(String, u64)> = APP_SOURCES
        .lock()
        .unwrap()
        .get(&app_id)
        .cloned()
        .unwrap_or_default();
    for uri in known_seeders_internal() {
        if !candidates.iter().any(|(u, _)| u == &uri) {
            candidates.push((uri, 0));
        }
    }
    // Самая свежая version — первой: исключаем установку устаревших архивов
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    if candidates.is_empty() {
        return Err("нет известных Cloud Seeder-ов — добавьте URI на странице Seeders".into());
    }
    let mut last_err = String::new();
    for (uri, _) in &candidates {
        match install_from_seeder(&app, uri, &app_id).await {
            Ok(m) => return Ok(m),
            Err(e) => {
                if e.starts_with("STORAGE_LIMIT:") {
                    return Err(e);
                }
                crate::syslog::log(
                    "seeder",
                    format!("install {app_id} from {uri} failed: {e}"),
                );
                last_err = e;
            }
        }
    }
    Err(format!(
        "архив {app_id} не найден ни у одного из {} сидеров ({last_err})",
        candidates.len()
    ))
}

/// Удалить архив dApp со СВОЕГО cloud seed диска — вызывается после
/// локального удаления приложения пользователем. Пробует актуальное имя
/// (dapps/<id>.enc) и legacy-раскладки; 404 (уже нет) — не ошибка.
#[tauri::command]
pub(crate) async fn cloud_seeder_delete_dapp(
    token: String,
    provider: Option<String>,
    app_id: String,
) -> Result<(), String> {
    let prov = parse_provider(provider.as_deref())?;
    if token.trim().is_empty() {
        return Err("нужен токен провайдера облака".into());
    }
    let id_safe = crate::sanitize(&app_id);
    let mut deleted = 0usize;
    for name in [format!("{id_safe}.enc"), format!("dapp-{id_safe}.enc")] {
        let r = match prov {
            SeederProvider::Mailru => {
                seeder::mailru_delete_seed_dapp_file(token.trim(), &name).await
            }
            SeederProvider::Google => {
                seeder::gdrive_delete_seed_dapp_file(token.trim(), &name).await
            }
            _ => seeder::yandex_delete_seed_dapp_file(token.trim(), &name).await,
        };
        match r {
            Ok(()) => deleted += 1,
            Err(ntfry::error::StorageError::NotFound(_)) => {}
            Err(e) => return Err(format!("{name}: {e}")),
        }
    }
    // legacy: плоский dapp-<id>.enc в корне /ntfryseed
    let legacy_root = format!("dapp-{id_safe}.enc");
    let r = match prov {
        SeederProvider::Mailru => {
            seeder::mailru_delete_seed_root_file(token.trim(), &legacy_root).await
        }
        SeederProvider::Google => {
            seeder::gdrive_delete_seed_root_file(token.trim(), &legacy_root).await
        }
        _ => seeder::yandex_delete_seed_root_file(token.trim(), &legacy_root).await,
    };
    match r {
        Ok(()) => deleted += 1,
        Err(ntfry::error::StorageError::NotFound(_)) => {}
        Err(e) => return Err(format!("{legacy_root}: {e}")),
    }
    remove_uploaded(&app_id);
    crate::syslog::log(
        "seeder",
        format!("cloud archive {app_id} removed from seed disk ({deleted} files)"),
    );
    Ok(())
}

async fn install_from_seeder(
    app: &tauri::AppHandle,
    uri: &str,
    app_id: &str,
) -> Result<crate::Manifest, String> {
    let parsed = CloudSeederUri::parse(uri).map_err(|e| e.to_string())?;
    let id_safe = crate::sanitize(app_id);
    // Актуальная структура: /ntfryseed/dapps/<appId>.enc; фолбэки для старых
    // публикаций: dapps/dapp-<id>.enc, затем плоско dapp-<id>.enc в корне.
    let names = [
        format!("{}/{}.enc", seeder::SEED_DAPPS_DIR, id_safe),
        format!("{}/dapp-{}.enc", seeder::SEED_DAPPS_DIR, id_safe),
        format!("dapp-{}.enc", id_safe),
    ];
    let mut blob: Option<Vec<u8>> = None;
    let mut last_err = String::new();
    for name in &names {
        match fetch_public(&parsed, name).await {
            Ok(b) => {
                blob = Some(b);
                break;
            }
            Err(ntfry::error::StorageError::NotFound(_)) => {
                last_err = format!("{name}: не найден");
            }
            Err(e) => return Err(format!("{name}: {e}")),
        }
    }
    let Some(blob) = blob else { return Err(last_err) };

    let pk = parsed.public_key.clone();
    let data = crate::ensure_data_dir(app);
    let id = app_id.to_string();
    let manifest_raw = tauri::async_runtime::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let tarball = seeder::open_bytes(&blob, &pk).map_err(|e| e.to_string())?;
        // Квота хранилища — как у P2P-инсталла (STORAGE_LIMIT:<bytes>)
        let dapps = data.join("dapps");
        let projected = crate::dir_size(&dapps)
            .saturating_sub(crate::dir_size(&dapps.join(&id)))
            .saturating_add(tarball.len() as u64);
        let limit = crate::get_storage_limit_internal();
        if projected > limit {
            return Err(format!("STORAGE_LIMIT:{}", projected - limit));
        }
        let dest = dapps.join(&id);
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        let mut ar = tar::Archive::new(std::io::Cursor::new(tarball));
        if let Err(e) = ar.unpack(&dest) {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(e.to_string());
        }
        // Защита от битых архивов: контент читаем из content.json, а входная
        // точка зависит от типа dApp — публичному нужен index.html в корне,
        // приватному (флаг "private" в content.json) — E2EE-контейнер app.enc.
        let manifest_raw = std::fs::read(dest.join("content.json")).map_err(|e| {
            let _ = std::fs::remove_dir_all(&dest);
            format!("битый архив dApp: нет content.json ({e})")
        })?;
        let is_priv = serde_json::from_slice::<serde_json::Value>(&manifest_raw)
            .ok()
            .and_then(|v| v.get("private").and_then(|p| p.as_bool()))
            .unwrap_or(false);
        let entry = if is_priv { crate::private_dapp::CONTAINER_FILE } else { "index.html" };
        if !dest.join(entry).is_file() {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(format!("битый архив dApp: в корне нет {entry}"));
        }
        Ok(manifest_raw)
    })
    .await
    .map_err(|e| e.to_string())??;

    let cleanup = || {
        let _ = std::fs::remove_dir_all(crate::ensure_data_dir(app).join("dapps").join(app_id));
    };
    let mut manifest: crate::Manifest = match serde_json::from_slice(&manifest_raw) {
        Ok(m) => m,
        Err(e) => {
            cleanup();
            return Err(format!("битый архив dApp: content.json не читается ({e})"));
        }
    };
    if manifest.id != app_id {
        cleanup();
        return Err(format!("id-mismatch:{}", manifest.id));
    }
    crate::DB
        .insert(format!("{}{}", crate::APP_PREFIX, app_id).as_bytes(), manifest_raw)
        .map_err(|e| e.to_string())?;
    crate::DB
        .insert(format!("announce:{}", app_id).as_bytes(), crate::crypto::now_iso().into_bytes())
        .map_err(|e| e.to_string())?;
    // Источник установки для бейджа вкладки (☁ cloud seeder)
    let src = serde_json::json!({
        "kind": "seeder",
        "provider": provider_name(parsed.provider),
        "uri": uri,
        "at": crate::crypto::now_iso(),
        "version": manifest.version,
    });
    let _ = crate::DB.insert(
        format!("install_src:{}", app_id).as_bytes(),
        src.to_string().into_bytes(),
    );
    let _ = crate::DB.flush();
    // Приватный dApp: кэш access.json в Sled (анонсы/декор UI) + подстановка
    // расшифрованных метаданных — как при P2P-установке.
    if manifest.private == Some(true) {
        crate::private_dapp::cache_access_from_folder(app, app_id);
        crate::private_dapp::decorate_manifest(&mut manifest);
    }
    *RESCUE_COUNTS.lock().unwrap().entry(uri.to_string()).or_insert(0) += 1;
    crate::syslog::log("seeder", format!("install from cloud: {app_id}"));
    Ok(manifest)
}

// ============================================================
//  Phase 3b — Cloud Fallback: если через 7с после старта discovery живых
//  P2P-пиров нет, тянем стейт сети из всех известных Cloud Seeder-ов и
//  перепроверяем каждые 5 минут, пока P2P не оживёт.
// ============================================================

static FALLBACK_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn spawn_cloud_fallback() {
    use std::sync::atomic::Ordering;
    if FALLBACK_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(7)).await;
        // Не завершаем цикл навсегда при появлении живого P2P: встаём в режим
        // ожидания и возобновляем опрос облаков, если P2P снова пропадает.
        let mut was_alive = false;
        // Фоновый скан облаков ПРИ ЖИВОМ P2P (v1.135.20): новые dApps могут
        // существовать только в облаках (паблишер офлайн). Первый скан — через
        // ~2 минуты после старта (быстро дополнить каталог), далее раз в
        // 30 минут, не более 3 сидеров за проход (живые по статусу — первыми).
        let mut next_bg = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            let alive = crate::p2p::has_direct_peers();
            if alive != was_alive {
                crate::syslog::log(
                    "seeder",
                    if alive {
                        "P2P alive — cloud fallback on standby".to_string()
                    } else {
                        "P2P gone — cloud fallback resumed".to_string()
                    },
                );
                was_alive = alive;
            }
            if !alive {
                run_fallback_once().await;
                next_bg = std::time::Instant::now() + std::time::Duration::from_secs(1800);
            } else if std::time::Instant::now() >= next_bg {
                run_fallback_scan(3).await;
                next_bg = std::time::Instant::now() + std::time::Duration::from_secs(1800);
            }
            tokio::time::sleep(std::time::Duration::from_secs(if alive { 30 } else { 300 })).await;
        }
    });
}

/// Слить новые URI в Sled-реестр (dedupe), вернуть число добавленных.
/// `source` записывается в карту origins для UI ("own" всегда перезаписывает).
fn registry_add_internal(new_uris: &[String], source: &str) -> usize {
    let mut list = known_seeders_internal();
    let mut origins = origins_map();
    let mut added = 0;
    for u in new_uris {
        if u.trim().is_empty() {
            continue;
        }
        if !list.contains(u) {
            list.push(u.clone());
            added += 1;
        }
        // "own" — приоритетный источник (публикация своего URI перекрывает
        // ранее записанный gossip/manual для того же URI). Остальные —
        // first-writer-wins, чтобы не терять исходный источник знакомства.
        if source == "own" {
            origins.insert(u.clone(), source.to_string());
        } else {
            origins.entry(u.clone()).or_insert_with(|| source.to_string());
        }
    }
    if added > 0 {
        if let Ok(val) = serde_json::to_vec(&list) {
            let _ = crate::DB.insert(SEEDERS_KEY, val);
        }
    }
    save_origins(&origins);
    added
}

/// Слить чужой seeders.enc в свой реестр — вызывается при КАЖДОМ успешном
/// fetch (ручной, «проверить все», установка, fallback). Цепная реакция:
/// сеть размазывает список сидеров сама.
fn merge_seeders_from(seeders: &serde_json::Value) {
    let Some(items) = seeders.get("items") else { return };
    let Ok(list) = serde_json::from_value::<Vec<String>>(items.clone()) else { return };
    let valid: Vec<String> = list
        .into_iter()
        .filter(|u| CloudSeederUri::parse(u).is_ok())
        .collect();
    let added = registry_add_internal(&valid, "seed_file");
    if added > 0 {
        crate::syslog::log("seeder", format!("+{added} new seeder URIs from external seeders.enc"));
        emit_registry_updated();
    }
}

fn emit_registry_updated() {
    #[cfg(feature = "p2p")]
    if let Some(app) = crate::chat::gossip::app_handle() {
        use tauri::Emitter;
        let _ = app.emit("seeder-registry-updated", ());
    }
}

// ── Gossip-анонс своего Seeder-URI (авто-обмен между живыми пирами) ────────

/// Подписанный анонс {v,kind,uri,ts,sig}: ECDSA ключом кошелька над
/// sha256("seeder-announce|uri|ts"); pubkey уже содержится в самом URI.
fn sign_announce(uri: &str, sk: &[u8; 32]) -> Option<Vec<u8>> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    use sha2::Digest;
    let digest = sha2::Sha256::digest(format!("seeder-announce|{uri}|{ts}").as_bytes());
    let secp = secp256k1::Secp256k1::new();
    let secret = secp256k1::SecretKey::from_slice(sk).ok()?;
    let msg = secp256k1::Message::from_digest_slice(&digest).ok()?;
    let sig = secp.sign_ecdsa(&msg, &secret);
    serde_json::to_vec(&serde_json::json!({
        "v": 1,
        "kind": "seeder-uri",
        "uri": uri,
        "ts": ts,
        "sig": hex::encode(sig.serialize_compact()),
    }))
    .ok()
}

/// Анонсы всех своих сидеров (primary + secondary-зеркало) — сеть знает оба.
pub(crate) fn gossip_announce_payloads() -> Vec<Vec<u8>> {
    let Ok((sk, _)) = signing_key() else { return Vec::new() };
    [own_seeder_uri(), own_seeder_uri2()]
        .into_iter()
        .flatten()
        .filter_map(|uri| sign_announce(&uri, &sk))
        .collect()
}

/// Входящий gossip-анонс чужого сидера: подпись проверяется pubkey ИЗ URI
/// (спам чужими URI отвергается), свежесть ±1 час. Валидный — в реестр.
pub(crate) fn handle_gossip_announce(bytes: &[u8]) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else { return };
    if v.get("kind").and_then(|k| k.as_str()) != Some("seeder-uri") {
        return;
    }
    let (Some(uri), Some(ts), Some(sig_hex)) = (
        v.get("uri").and_then(|u| u.as_str()),
        v.get("ts").and_then(|t| t.as_u64()),
        v.get("sig").and_then(|s| s.as_str()),
    ) else {
        return;
    };
    let Ok(parsed) = CloudSeederUri::parse(uri) else { return };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if ts.abs_diff(now) > 3600 {
        return;
    }
    use sha2::Digest;
    let digest = sha2::Sha256::digest(format!("seeder-announce|{uri}|{ts}").as_bytes());
    let (Ok(pk_bytes), Ok(sig_bytes)) = (hex::decode(&parsed.public_key), hex::decode(sig_hex))
    else {
        return;
    };
    let (Ok(pk), Ok(sig), Ok(msg)) = (
        secp256k1::PublicKey::from_slice(&pk_bytes),
        secp256k1::ecdsa::Signature::from_compact(&sig_bytes),
        secp256k1::Message::from_digest_slice(&digest),
    ) else {
        return;
    };
    if secp256k1::Secp256k1::verification_only().verify_ecdsa(&msg, &sig, &pk).is_err() {
        return;
    }
    if own_seeder_uri().as_deref() == Some(uri) || own_seeder_uri2().as_deref() == Some(uri) {
        return;
    }
    let added = registry_add_internal(&[uri.to_string()], "gossip");
    if added > 0 {
        crate::syslog::log("seeder", format!("gossip: new seeder in network → {uri}"));
        emit_registry_updated();
    }
}

/// Один проход по сидерам: скачать подписанный стейт с их облачных дисков и
/// влить ноды/dApps в таблицу пиров. Возвращает счётчики (сидеров OK, всего,
/// нод, dApps). `limit == 0` — опросить всех (аварийный fallback при мёртвом
/// P2P или скан по кнопке); `limit > 0` — фоновый скан при живом P2P: не более
/// `limit` сидеров за проход, живые по статусу — первыми.
async fn run_fallback_scan(limit: usize) -> (usize, usize, usize, usize) {
    let mut uris = known_seeders_internal();
    if uris.is_empty() {
        crate::syslog::log(
            "seeder",
            "cloud scan: seeder registry is empty".to_string(),
        );
        return (0, 0, 0, 0);
    }
    if limit > 0 && uris.len() > limit {
        let status = status_map();
        uris.sort_by_key(|u| match status.get(u) {
            Some(s) if s.ok => 0u8,
            None => 1,
            Some(_) => 2,
        });
        uris.truncate(limit);
    }
    let total = uris.len();
    crate::syslog::log(
        "seeder",
        format!(
            "cloud scan: polling {total} seeders{}",
            if limit > 0 { " (background, P2P alive)" } else { " (P2P unreachable)" }
        ),
    );
    let (mut nodes, mut dapps, mut ok) = (0usize, 0usize, 0usize);
    for uri in uris {
        match fetch_seeder_internal(&uri).await {
            Ok(out) => {
                ok += 1;
                if let Some(items) = out.nodes.get("items") {
                    nodes += crate::p2p::merge_seeder_nodes(items);
                }
                if let Some(items) = out.dapps.get("items") {
                    dapps += crate::p2p::merge_seeder_dapps(&out.public_key, items);
                }
                // seeders.enc вливается в реестр внутри fetch_seeder_internal
            }
            Err(e) => crate::syslog::log("seeder", format!("cloud scan: {uri} — {e}")),
        }
    }
    crate::syslog::log(
        "seeder",
        format!("cloud scan: {ok} seeders OK → merged {nodes} nodes, {dapps} dApps"),
    );
    (ok, total, nodes, dapps)
}

/// Полный проход по всем известным сидерам (без лимита).
async fn run_fallback_once() -> (usize, usize, usize, usize) {
    run_fallback_scan(0).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeederScanOut {
    seeders_ok: usize,
    seeders_total: usize,
    nodes: usize,
    dapps: usize,
}

/// Скан облачных дисков всех известных Cloud Seeder-ов по требованию UI
/// (страница Discover при недоступном P2P). Найденные dApps появляются в
/// каталоге как синтетические via_seeder-пиры и ставятся через
/// cloud_seeder_install_any без P2P.
#[tauri::command]
pub(crate) async fn cloud_seeder_scan_apps() -> Result<SeederScanOut, String> {
    let (ok, total, nodes, dapps) = run_fallback_once().await;
    Ok(SeederScanOut { seeders_ok: ok, seeders_total: total, nodes, dapps })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(sender: &str, vendor: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "1040a5fe6c8b8ea8c9afc4cbd2da11ec3ded608c8cff68f41e1d442b7edf1299",
            "sender": sender,
            "recipient": "SaQUdQbxZGJdjzZpLPyDYAqtE47EupDaTe",
            "vendorField": vendor,
            "timestamp": { "unix": 1785455312u64 }
        })
    }

    #[test]
    fn parses_update_memo_string() {
        let (ver, hash) =
            parse_update_memo("smartnet:1.99.1:d6d063cf7cdd9a2594174122e73f805fee4cafa1")
                .expect("валидная строка");
        assert_eq!(ver, "1.99.1");
        assert_eq!(hash, "d6d063cf7cdd9a2594174122e73f805fee4cafa1");
        assert!(parse_update_memo("hello world").is_none());
        assert!(parse_update_memo("otherapp:1.0.0:deadbeef").is_none());
        assert!(parse_update_memo("smartnet:1.0.0:").is_none());
    }

    #[test]
    fn picks_valid_update_announcement() {
        let tx = sample(
            crate::updater::UPDATE_CREATOR,
            "smartnet:1.99.1:d6d063cf7cdd9a2594174122e73f805fee4cafa1",
        );
        let (ver, hash) = tx_is_update(&tx).expect("валидный анонс");
        assert_eq!(ver, "1.99.1");
        assert_eq!(hash, "d6d063cf7cdd9a2594174122e73f805fee4cafa1");
    }

    #[test]
    fn rejects_foreign_sender_and_bad_memo() {
        // чужой отправитель
        let tx = sample("SfakeSenderAddress", "smartnet:9.9.9:deadbeef");
        assert!(tx_is_update(&tx).is_none());
        // не тот формат memo
        let tx = sample(crate::updater::UPDATE_CREATOR, "hello world");
        assert!(tx_is_update(&tx).is_none());
    }
}
