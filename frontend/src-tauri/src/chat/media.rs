//! Мессенджер: P2P-передача изображений через iroh-blobs + локальный кэш
//! медиафайлов `data/messenger` (лимит по умолчанию 2 ГБ, суточная автоуборка:
//! при превышении лимита удаляются 25% самых старых файлов).
//!
//! Схема как в Telegram: миниатюра летит прямо в E2EE-конверте gossip (конверт
//! обязан влезть в 4КБ-кадр gossip — см. MAX_PACKET), полноразмерный JPEG
//! раздаётся по iroh-blobs от доступных участников чата.
//! Каждый, кто скачал картинку, сам становится её провайдером (ре-сид ≤7 дней).

use serde::Serialize;
use std::path::PathBuf;

use crate::DB;

const REG_PREFIX: &str = "chat:imgseed:";
const SEED_TTL_MS: u64 = 7 * 24 * 3600 * 1000;
const SEED_CAP: usize = 100;
const MAX_IMG_BYTES: usize = 8 * 1024 * 1024;
const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 25 * 1024 * 1024;
const MAX_VIDEO_BYTES: u64 = 500 * 1024 * 1024;
// Бюджет 4КБ-кадра gossip: миниатюра ≤1800 b64 + подпись ≤200 символов.
const MAX_THUMB_B64: usize = 1_800;
const MAX_CAPTION: usize = 200;

fn b64d(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).map_err(|e| e.to_string())
}

fn b64e(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// Папка кэша медиафайлов мессенджера: `<AppData>/data/messenger`.
pub(crate) fn cache_dir(app: &tauri::AppHandle) -> PathBuf {
    let d = crate::ensure_data_dir(app).join("messenger");
    let _ = std::fs::create_dir_all(&d);
    d
}

// ---- настройки кэша --------------------------------------------------------

fn cache_max_gb() -> f64 {
    super::cfg_get("chat:cache:maxgb")
        .and_then(|v| String::from_utf8(v).ok())
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|g| *g > 0.0)
        .unwrap_or(2.0)
}

fn cache_max_bytes() -> u64 {
    (cache_max_gb() * 1024.0 * 1024.0 * 1024.0) as u64
}

fn autoclean_on() -> bool {
    !matches!(super::cfg_get("chat:cache:autoclean"), Some(v) if v == b"0")
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CacheInfo {
    bytes: u64,
    files: u64,
    max_gb: f64,
    autoclean: bool,
}

fn dir_stats(dir: &PathBuf) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(md) = e.metadata() {
                if md.is_file() {
                    bytes += md.len();
                    files += 1;
                }
            }
        }
    }
    (bytes, files)
}

#[tauri::command]
pub(crate) async fn chat_cache_info(app: tauri::AppHandle) -> CacheInfo {
    let dir = cache_dir(&app);
    let (bytes, files) = tokio::task::spawn_blocking(move || dir_stats(&dir))
        .await
        .unwrap_or((0, 0));
    CacheInfo { bytes, files, max_gb: cache_max_gb(), autoclean: autoclean_on() }
}

#[tauri::command]
pub(crate) async fn chat_cache_set(max_gb: f64, autoclean: bool) -> Result<(), String> {
    let g = max_gb.clamp(0.1, 100.0);
    let _ = DB.insert(b"chat:cache:maxgb", format!("{}", g).as_bytes());
    let _ = DB.insert(b"chat:cache:autoclean", if autoclean { b"1" as &[u8] } else { b"0" });
    let _ = DB.flush();
    Ok(())
}

/// Мгновенная полная очистка кэша медиафайлов (+ реестра ре-сида).
#[tauri::command]
pub(crate) async fn chat_cache_clear(app: tauri::AppHandle) -> Result<u64, String> {
    let dir = cache_dir(&app);
    tokio::task::spawn_blocking(move || {
        let mut freed = 0u64;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if let Ok(md) = e.metadata() {
                    if md.is_file() && std::fs::remove_file(e.path()).is_ok() {
                        freed += md.len();
                    }
                }
            }
        }
        let keys: Vec<sled::IVec> = DB
            .scan_prefix(REG_PREFIX.as_bytes())
            .filter_map(|kv| kv.ok().map(|(k, _)| k))
            .collect();
        for k in keys {
            let _ = DB.remove(k);
        }
        let _ = DB.flush();
        freed
    })
    .await
    .map_err(|e| e.to_string())
}

// ---- реестр ре-сида (защита блобов от GC + раздача участникам) --------------

fn register_seed(hash: &str, path: &PathBuf) {
    let v = serde_json::json!({ "p": path.to_string_lossy(), "ts": super::now_ms() });
    let _ = DB.insert(format!("{}{}", REG_PREFIX, hash).as_bytes(), v.to_string().as_bytes());
    let _ = DB.flush();
}

/// Пути свежих (≤7 дней, максимум 100) кэш-файлов для ре-сида в iroh —
/// вызывается p2p seed-циклом, чтобы GC не выпилил блобы живых картинок.
pub(crate) fn seed_paths() -> Vec<PathBuf> {
    let now = super::now_ms();
    let mut items: Vec<(u64, PathBuf)> = Vec::new();
    for kv in DB.scan_prefix(REG_PREFIX.as_bytes()) {
        if let Ok((k, v)) = kv {
            if let Ok(j) = serde_json::from_slice::<serde_json::Value>(&v) {
                let ts = j["ts"].as_u64().unwrap_or(0);
                let p = PathBuf::from(j["p"].as_str().unwrap_or(""));
                if now.saturating_sub(ts) > SEED_TTL_MS || !p.is_file() {
                    let _ = DB.remove(k);
                    continue;
                }
                items.push((ts, p));
            }
        }
    }
    items.sort_by(|a, b| b.0.cmp(&a.0));
    items.truncate(SEED_CAP);
    items.into_iter().map(|(_, p)| p).collect()
}

// ---- отправка / получение изображений ---------------------------------------

/// Отправка изображения: JPEG (уже сжатый фронтендом до ≤1920×1080) кладётся в
/// кэш, сидируется как iroh-blob, а в комнату уходит конверт `sn-img:{json}` со
/// встроенной миниатюрой (влезает в 4 КБ gossip-кадр).
#[tauri::command]
pub(crate) async fn chat_send_image(
    app: tauri::AppHandle,
    room_id: String,
    data_b64: String,
    thumb_b64: String,
    width: u32,
    height: u32,
    caption: String,
) -> Result<super::ChatMsg, String> {
    let room = super::load_room(&room_id).ok_or("no room")?;
    if room.kind != "group" && super::peer_msg_cost(&room.peer_address) > 0 {
        return Err("img-paid-unsupported".into());
    }
    if thumb_b64.is_empty() {
        return Err("img-thumb-empty".into());
    }
    if thumb_b64.len() > MAX_THUMB_B64 {
        return Err("img-thumb-too-big".into());
    }
    let bytes = b64d(&data_b64)?;
    if bytes.is_empty() || bytes.len() > MAX_IMG_BYTES {
        return Err("img-too-big".into());
    }
    let size = bytes.len() as u64;
    let dir = cache_dir(&app);
    let tmp = dir.join(format!("tmp-{}.jpg", super::now_ms()));
    tokio::fs::write(&tmp, &bytes).await.map_err(|e| {
        crate::crashdiag::note_io_error(&e, "chat media write");
        e.to_string()
    })?;
    let hash = match crate::p2p::seed_file(&app, &tmp.to_string_lossy()).await {
        Some(h) => h,
        None => {
            let _ = std::fs::remove_file(&tmp);
            return Err("img-seed-failed".into());
        }
    };
    let dest = dir.join(format!("{}.jpg", hash));
    if std::fs::rename(&tmp, &dest).is_err() {
        let _ = std::fs::copy(&tmp, &dest);
        let _ = std::fs::remove_file(&tmp);
    }
    register_seed(&hash, &dest);
    // `n` = медиа-endpoint: докачка у нас идёт по изолированному каналу
    let node = match crate::p2p::media_node_id(&app).await {
        Some(n) => n,
        None => super::my_node_id(&app).await.unwrap_or_default(),
    };
    super::media_seeders::announce(&hash, &node, size, &[]);
    let mut meta = serde_json::json!({
        "h": hash, "n": node, "w": width, "ht": height, "s": size, "t": thumb_b64,
    });
    let cap: String = caption.trim().chars().take(MAX_CAPTION).collect();
    if !cap.is_empty() {
        meta["c"] = serde_json::Value::String(cap);
    }
    let text = format!("sn-img:{}", meta);
    let me = super::my_address().unwrap_or_default();
    let msg = super::store_msg(&room, &me, &text, true)?;
    #[cfg(feature = "p2p")]
    {
        super::broadcast_envelope(&room, &msg);
        super::spawn_direct_fallback(room.clone(), msg.clone());
    }
    Ok(msg)
}

/// Отправка аудио (mp3 или голосовое WebM/Opus): файл кладётся в кэш, сидируется
/// как iroh-blob, в комнату уходит лёгкий конверт `sn-aud:{json}`.
#[tauri::command]
pub(crate) async fn chat_send_audio(
    app: tauri::AppHandle,
    room_id: String,
    data_b64: String,
    name: String,
    duration_ms: u64,
    mime: String,
    voice: bool,
    wave: String,
) -> Result<super::ChatMsg, String> {
    let room = super::load_room(&room_id).ok_or("no room")?;
    if room.kind != "group" && super::peer_msg_cost(&room.peer_address) > 0 {
        return Err("aud-paid-unsupported".into());
    }
    let (ext, mime_norm) = if mime.contains("webm") {
        ("webm", "audio/webm")
    } else if mime.contains("mp4") || mime.contains("m4a") || mime.contains("aac") {
        ("m4a", "audio/mp4")
    } else {
        ("mp3", "audio/mpeg")
    };
    let bytes = b64d(&data_b64)?;
    if bytes.is_empty() || bytes.len() > MAX_AUDIO_BYTES {
        return Err("aud-too-big".into());
    }
    let size = bytes.len() as u64;
    let dir = cache_dir(&app);
    let tmp = dir.join(format!("tmp-{}.{}", super::now_ms(), ext));
    tokio::fs::write(&tmp, &bytes).await.map_err(|e| {
        crate::crashdiag::note_io_error(&e, "chat media write");
        e.to_string()
    })?;
    let hash = match crate::p2p::seed_file(&app, &tmp.to_string_lossy()).await {
        Some(h) => h,
        None => {
            let _ = std::fs::remove_file(&tmp);
            return Err("aud-seed-failed".into());
        }
    };
    let dest = dir.join(format!("{}.{}", hash, ext));
    if std::fs::rename(&tmp, &dest).is_err() {
        let _ = std::fs::copy(&tmp, &dest);
        let _ = std::fs::remove_file(&tmp);
    }
    register_seed(&hash, &dest);
    // имя файла без путей и управляющих символов, максимум 100 символов
    let clean: String = name
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | '\u{7f}') && !c.is_control())
        .take(100)
        .collect();
    // `n` = медиа-endpoint: докачка у нас идёт по изолированному каналу
    let node = match crate::p2p::media_node_id(&app).await {
        Some(n) => n,
        None => super::my_node_id(&app).await.unwrap_or_default(),
    };
    super::media_seeders::announce(&hash, &node, size, &[]);
    let mut meta = serde_json::json!({
        "h": hash, "n": node, "s": size, "d": duration_ms, "f": clean, "m": mime_norm,
    });
    if voice {
        meta["v"] = serde_json::Value::from(1);
    }
    // звуковая волна: до 128 hex-символов (0-f = высота бара), считается фронтендом
    let wv: String = wave.chars().filter(|c| c.is_ascii_hexdigit()).take(128).collect();
    if !wv.is_empty() {
        meta["w"] = serde_json::Value::String(wv);
    }
    let text = format!("sn-aud:{}", meta);
    let me = super::my_address().unwrap_or_default();
    let msg = super::store_msg(&room, &me, &text, true)?;
    #[cfg(feature = "p2p")]
    {
        super::broadcast_envelope(&room, &msg);
        super::spawn_direct_fallback(room.clone(), msg.clone());
    }
    Ok(msg)
}

// ---- реестр медиа-провайдеров (Provider Registry) --------------------------
// Заполняется из P:/N:-строк sync-анонсов (mod.rs handle_sync): участники,
// скачавшие видео, анонсируют его — загрузка идёт от НЕСКОЛЬКИХ источников,
// а не только от автора. Записи живут 15 минут (анонсы идут каждые ~70с).

static PROVIDERS: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<String, std::collections::HashMap<String, u64>>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

pub(crate) fn record_provider(hash: &str, node: &str) {
    if hash.is_empty()
        || hash.len() > 128
        || node.is_empty()
        || node.len() > 128
        || !hash.chars().all(|c| c.is_ascii_alphanumeric())
        || !node.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return;
    }
    let mut m = PROVIDERS.lock().unwrap();
    let e = m.entry(hash.to_string()).or_default();
    e.insert(node.to_string(), super::now_ms());
    if e.len() > 16 {
        // выкидываем самого старого
        if let Some(k) = e.iter().min_by_key(|(_, ts)| **ts).map(|(k, _)| k.clone()) {
            e.remove(&k);
        }
    }
}

pub(crate) fn providers_for(hash: &str) -> Vec<String> {
    let now = super::now_ms();
    let m = PROVIDERS.lock().unwrap();
    let mut out: Vec<(String, u64)> = m
        .get(hash)
        .map(|e| {
            e.iter()
                .filter(|(_, ts)| now.saturating_sub(**ts) < 15 * 60 * 1000)
                .map(|(k, ts)| (k.clone(), *ts))
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| b.1.cmp(&a.1)); // свежие первыми
    out.truncate(4);
    out.into_iter().map(|(k, _)| k).collect()
}

/// hash видео из конверта sn-vid (для анонса провайдера в sync-цикле)
pub(crate) fn video_hash_of(text: &str) -> Option<String> {
    let js = text.strip_prefix("sn-vid:")?;
    let v: serde_json::Value = serde_json::from_str(js).ok()?;
    v["h"].as_str().map(|s| s.to_string())
}

pub(crate) fn has_cached_video(app: &tauri::AppHandle, hash: &str) -> bool {
    if hash.is_empty() || !hash.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    cache_dir(app).join(format!("{}.mp4", hash)).is_file()
}

/// Полноразмерный медиафайл по хэшу: из локального кэша, либо P2P-докачка у
/// доступных участников (отправитель + bootstrap-ноды группы / узел пира).
/// Скачанный файл сразу регистрируется на ре-сид — мы тоже провайдер.
/// Возвращает ПУТЬ к файлу в кэше (для больших файлов — видео).
async fn load_media_path(
    app: &tauri::AppHandle,
    room_id: &str,
    hash: &str,
    node: &str,
    ext: &str,
) -> Result<PathBuf, String> {
    if hash.is_empty() || hash.len() > 128 || !hash.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("bad hash".into());
    }
    let dest = cache_dir(app).join(format!("{}.{}", hash, ext));
    if dest.is_file() {
        return Ok(dest);
    }
    let mut extra: Vec<String> = Vec::new();
    if let Some(room) = super::load_room(room_id) {
        if room.kind == "group" {
            extra.extend(super::group_boots(&room.id));
        } else if let Some(n) = super::peer_node_id(&room.peer_address) {
            extra.push(n);
        }
    }
    // Provider Registry: участники, анонсировавшие этот hash, — доп. источники
    for p in providers_for(hash) {
        if p != node && !extra.contains(&p) {
            extra.push(p);
        }
    }
    // Headless медиа-сидеры (24/7 зеркала медиа чатов) — источник, когда автор офлайн
    for p in super::media_seeders::endpoint_ids() {
        if p != node && !extra.contains(&p) {
            extra.push(p);
        }
    }
    // Шлюзы: видео — не более ОДНОЙ параллельной P2P-докачки на клиента
    // (одновременные загрузки всей группой душат QUIC-линки gossip у отправителя);
    // мелкое медиа (постеры/фото/аудио) — свой шлюз, чтобы не стоять в очереди
    // за зависшим 120-МБ видео: иначе у получателей часами размытые карточки.
    use once_cell::sync::Lazy;
    static VIDEO_GATE: Lazy<tokio::sync::Semaphore> = Lazy::new(|| tokio::sync::Semaphore::new(1));
    static SMALL_GATE: Lazy<tokio::sync::Semaphore> = Lazy::new(|| tokio::sync::Semaphore::new(2));
    let is_video = ext == "mp4";
    // Реестр отмен: hash → Notify. Повторный клик по крутящейся загрузке в UI
    // вызывает chat_cancel_media(hash) — висящая докачка прерывается мгновенно.
    let cancel = {
        let mut m = CANCELS.lock().unwrap();
        m.entry(hash.to_string()).or_insert_with(|| std::sync::Arc::new(tokio::sync::Notify::new())).clone()
    };
    let fetch = async {
        let gate: &tokio::sync::Semaphore = if is_video { &VIDEO_GATE } else { &SMALL_GATE };
        let _permit = gate.acquire().await.map_err(|e| e.to_string())?;
        if dest.is_file() {
            return Ok(dest.clone());
        }
        // для подсказки в UI: сколько источников знаем (автор + реестр + сидеры)
        let n_src = 1 + extra.len();
        FETCH_INFO.lock().unwrap().insert(hash.to_string(), (n_src, super::now_ms()));
        // Выделенный медиа-endpoint: докачка не трогает канал текстовых сообщений.
        crate::p2p::fetch_media_file(app, node, hash, &dest.to_string_lossy(), &extra).await?;
        register_seed(hash, &dest);
        // мы теперь тоже провайдер — даём сидерам ещё один источник для зеркала
        if let Some(me) = crate::p2p::media_node_id(app).await {
            let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            let mut srcs: Vec<String> = vec![node.to_string()];
            srcs.extend(extra.iter().take(4).cloned());
            super::media_seeders::announce(hash, &me, size, &srcs);
        }
        Ok(dest.clone())
    };
    // Жёсткий потолок: iroh download() может висеть вечно на мёртвом провайдере —
    // раньше это НАВСЕГДА блокировало шлюз (вечный кружок у ВСЕХ последующих
    // медиа). Видео большие — 10 минут; постеры/фото — 90 с, аудио — 180 с.
    let secs = if is_video { 600 } else if ext == "jpg" { 90 } else { 180 };
    let res = tokio::select! {
        r = tokio::time::timeout(std::time::Duration::from_secs(secs), fetch) => {
            r.unwrap_or_else(|_| Err("media-timeout".into()))
        }
        _ = cancel.notified() => Err("media-cancelled".into()),
    };
    CANCELS.lock().unwrap().remove(hash);
    FETCH_INFO.lock().unwrap().remove(hash);
    res
}

async fn load_media(
    app: &tauri::AppHandle,
    room_id: &str,
    hash: &str,
    node: &str,
    ext: &str,
) -> Result<String, String> {
    let p = load_media_path(app, room_id, hash, node, ext).await?;
    let b = tokio::fs::read(&p).await.map_err(|e| e.to_string())?;
    Ok(b64e(&b))
}

static CANCELS: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Notify>>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Отменить висящую P2P-докачку медиа (повторный клик по кружку в UI).
/// true = загрузка была активна и прервана.
#[tauri::command]
pub(crate) fn chat_cancel_media(hash: String) -> bool {
    if let Some(n) = CANCELS.lock().unwrap().get(&hash) {
        n.notify_waiters();
        true
    } else {
        false
    }
}

/// Живой прогресс докачки медиа: байты уже переданы + завершено ли + сколько
/// источников знаем и сколько секунд идёт докачка (подсказка «почему ждём»).
/// UI поллит раз в ~600мс, пока крутится кружок (проценты и скорость).
#[tauri::command]
pub(crate) async fn chat_media_progress(app: tauri::AppHandle, hash: String) -> serde_json::Value {
    let (bytes, done) = crate::p2p::blob_progress(&app, &hash).await;
    let (providers, started) = FETCH_INFO
        .lock()
        .ok()
        .and_then(|m| m.get(&hash).copied())
        .unwrap_or((0, 0));
    let elapsed = if started > 0 { super::now_ms().saturating_sub(started) / 1000 } else { 0 };
    serde_json::json!({ "bytes": bytes, "done": done, "providers": providers, "elapsedSecs": elapsed })
}

/// hash → (число известных источников, старт докачки, мс)
static FETCH_INFO: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashMap<String, (usize, u64)>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[tauri::command]
pub(crate) async fn chat_load_image(
    app: tauri::AppHandle,
    room_id: String,
    hash: String,
    node: String,
) -> Result<String, String> {
    load_media(&app, &room_id, &hash, &node, "jpg").await
}

#[tauri::command]
pub(crate) async fn chat_load_audio(
    app: tauri::AppHandle,
    room_id: String,
    hash: String,
    node: String,
    mime: String,
) -> Result<String, String> {
    let ext = if mime.contains("webm") {
        "webm"
    } else if mime.contains("mp4") || mime.contains("m4a") || mime.contains("aac") {
        "m4a"
    } else {
        "mp3"
    };
    load_media(&app, &room_id, &hash, &node, ext).await
}

/// Содержимое документа (.md/.pdf) по хэшу: P2P-докачка при необходимости,
/// возврат base64 БЕЗ сохранения на диск пользователя (просмотр в чате).
#[tauri::command]
pub(crate) async fn chat_load_file(
    app: tauri::AppHandle,
    room_id: String,
    hash: String,
    node: String,
    mime: String,
) -> Result<String, String> {
    let ext = if mime.contains("pdf") { "pdf" } else { "md" };
    load_media(&app, &room_id, &hash, &node, ext).await
}

/// Отправка документа (.pdf/.md): файл кладётся в кэш, сидируется как
/// iroh-blob, в комнату уходит лёгкий конверт `sn-file:{json}`.
#[tauri::command]
pub(crate) async fn chat_send_file(
    app: tauri::AppHandle,
    room_id: String,
    data_b64: String,
    name: String,
    mime: String,
) -> Result<super::ChatMsg, String> {
    let room = super::load_room(&room_id).ok_or("no room")?;
    if room.kind != "group" && super::peer_msg_cost(&room.peer_address) > 0 {
        return Err("file-paid-unsupported".into());
    }
    let lower = name.to_lowercase();
    let (ext, mime_norm) = if mime.contains("pdf") || lower.ends_with(".pdf") {
        ("pdf", "application/pdf")
    } else if mime.contains("markdown") || lower.ends_with(".md") || lower.ends_with(".markdown") {
        ("md", "text/markdown")
    } else {
        return Err("file-unsupported".into());
    };
    let bytes = b64d(&data_b64)?;
    if bytes.is_empty() || bytes.len() > MAX_FILE_BYTES {
        return Err("file-too-big".into());
    }
    let size = bytes.len() as u64;
    let dir = cache_dir(&app);
    let tmp = dir.join(format!("tmp-{}.{}", super::now_ms(), ext));
    tokio::fs::write(&tmp, &bytes).await.map_err(|e| {
        crate::crashdiag::note_io_error(&e, "chat media write");
        e.to_string()
    })?;
    let hash = match crate::p2p::seed_file(&app, &tmp.to_string_lossy()).await {
        Some(h) => h,
        None => {
            let _ = std::fs::remove_file(&tmp);
            return Err("file-seed-failed".into());
        }
    };
    let dest = dir.join(format!("{}.{}", hash, ext));
    if std::fs::rename(&tmp, &dest).is_err() {
        let _ = std::fs::copy(&tmp, &dest);
        let _ = std::fs::remove_file(&tmp);
    }
    register_seed(&hash, &dest);
    let clean: String = name
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | '\u{7f}') && !c.is_control())
        .take(100)
        .collect();
    let node = match crate::p2p::media_node_id(&app).await {
        Some(n) => n,
        None => super::my_node_id(&app).await.unwrap_or_default(),
    };
    super::media_seeders::announce(&hash, &node, size, &[]);
    let meta = serde_json::json!({
        "h": hash, "n": node, "s": size, "f": clean, "m": mime_norm,
    });
    let text = format!("sn-file:{}", meta);
    let me = super::my_address().unwrap_or_default();
    let msg = super::store_msg(&room, &me, &text, true)?;
    #[cfg(feature = "p2p")]
    {
        super::broadcast_envelope(&room, &msg);
        super::spawn_direct_fallback(room.clone(), msg.clone());
    }
    Ok(msg)
}

/// Логотип группы (только владелец): PNG/JPG ≤128КБ, ≤128×128 (даунскейл на
/// фронте). Сидируется как обычный blob мессенджера; владелец кладёт hash+node
/// в подписанную policy («Сохранить и разослать»), участники качают через
/// существующий chat_load_image и кэшируют в data/messenger.
#[tauri::command]
pub(crate) async fn chat_group_logo_seed(
    app: tauri::AppHandle,
    room_id: String,
    data_b64: String,
) -> Result<serde_json::Value, String> {
    let room = super::load_room(&room_id).ok_or("no room")?;
    if room.kind != "group" {
        return Err("not a group".into());
    }
    let me = super::my_address().ok_or("locked")?;
    if room.owner.as_deref() != Some(me.as_str()) {
        return Err("owner only".into());
    }
    let bytes = b64d(&data_b64)?;
    if bytes.is_empty() || bytes.len() > 128 * 1024 {
        return Err("logo-too-big".into());
    }
    let dir = cache_dir(&app);
    let tmp = dir.join(format!("tmp-logo-{}.jpg", super::now_ms()));
    tokio::fs::write(&tmp, &bytes).await.map_err(|e| {
        crate::crashdiag::note_io_error(&e, "chat media write");
        e.to_string()
    })?;
    let hash = match crate::p2p::seed_file(&app, &tmp.to_string_lossy()).await {
        Some(h) => h,
        None => {
            let _ = std::fs::remove_file(&tmp);
            return Err("logo-seed-failed".into());
        }
    };
    let dest = dir.join(format!("{}.jpg", hash));
    if std::fs::rename(&tmp, &dest).is_err() {
        let _ = std::fs::copy(&tmp, &dest);
        let _ = std::fs::remove_file(&tmp);
    }
    register_seed(&hash, &dest);
    let node = match crate::p2p::media_node_id(&app).await {
        Some(n) => n,
        None => super::my_node_id(&app).await.unwrap_or_default(),
    };
    super::media_seeders::announce(&hash, &node, bytes.len() as u64, &[]);
    Ok(serde_json::json!({ "hash": hash, "node": node }))
}

// ---- видео (.mp4, ≤500 МБ): чанк-аплоад через IPC + конверт sn-vid ---------
// 500 МБ одним invoke убивает память WebView, поэтому файл течёт в кэш
// кусками по ~8 МБ: begin → chunk×N → send (сид + конверт) / abort.

static UPLOADS: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashMap<String, PathBuf>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[tauri::command]
pub(crate) fn chat_video_begin(app: tauri::AppHandle) -> Result<String, String> {
    let id = format!("vup{}", super::now_ms());
    let path = cache_dir(&app).join(format!("{}.part", id));
    std::fs::write(&path, b"").map_err(|e| e.to_string())?;
    UPLOADS.lock().unwrap().insert(id.clone(), path);
    Ok(id)
}

#[tauri::command(async)]
pub(crate) fn chat_video_chunk(upload_id: String, data_b64: String) -> Result<u64, String> {
    let path = UPLOADS.lock().unwrap().get(&upload_id).cloned().ok_or("no upload")?;
    let bytes = b64d(&data_b64)?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).map_err(|e| e.to_string())?;
    f.write_all(&bytes).map_err(|e| e.to_string())?;
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    drop(f);
    if size > MAX_VIDEO_BYTES {
        let _ = std::fs::remove_file(&path);
        UPLOADS.lock().unwrap().remove(&upload_id);
        return Err("video-too-big".into());
    }
    Ok(size)
}

#[tauri::command]
pub(crate) fn chat_video_abort(upload_id: String) {
    if let Some(p) = UPLOADS.lock().unwrap().remove(&upload_id) {
        let _ = std::fs::remove_file(&p);
    }
}

#[tauri::command]
pub(crate) async fn chat_video_send(
    app: tauri::AppHandle,
    room_id: String,
    upload_id: String,
    name: String,
    dur_ms: u64,
    thumb_b64: Option<String>,
    w: Option<u32>,
    ht: Option<u32>,
    poster_b64: Option<String>,
) -> Result<super::ChatMsg, String> {
    let room = super::load_room(&room_id).ok_or("no room")?;
    let tmp = UPLOADS.lock().unwrap().remove(&upload_id).ok_or("no upload")?;
    if room.kind != "group" && super::peer_msg_cost(&room.peer_address) > 0 {
        let _ = std::fs::remove_file(&tmp);
        return Err("video-paid-unsupported".into());
    }
    let lower = name.to_lowercase();
    if !lower.ends_with(".mp4") && !lower.ends_with(".m4v") {
        let _ = std::fs::remove_file(&tmp);
        return Err("video-unsupported".into());
    }
    let size = std::fs::metadata(&tmp).map_err(|e| e.to_string())?.len();
    if size == 0 || size > MAX_VIDEO_BYTES {
        let _ = std::fs::remove_file(&tmp);
        return Err("video-too-big".into());
    }
    let hash = match crate::p2p::seed_file(&app, &tmp.to_string_lossy()).await {
        Some(h) => h,
        None => {
            let _ = std::fs::remove_file(&tmp);
            return Err("video-seed-failed".into());
        }
    };
    let dest = cache_dir(&app).join(format!("{}.mp4", hash));
    if std::fs::rename(&tmp, &dest).is_err() {
        let _ = std::fs::copy(&tmp, &dest);
        let _ = std::fs::remove_file(&tmp);
    }
    register_seed(&hash, &dest);
    let clean: String = name
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | '\u{7f}') && !c.is_control())
        .take(100)
        .collect();
    let node = match crate::p2p::media_node_id(&app).await {
        Some(n) => n,
        None => super::my_node_id(&app).await.unwrap_or_default(),
    };
    super::media_seeders::announce(&hash, &node, size, &[]);
    let mut meta = serde_json::json!({
        "h": hash, "n": node, "s": size, "f": clean, "m": "video/mp4", "d": dur_ms,
    });
    // кадр-постер: тот же бюджет, что у фото-миниатюр (конверт ≤ 4КБ gossip)
    if let Some(t) = thumb_b64.filter(|t| !t.is_empty() && t.len() <= MAX_THUMB_B64) {
        meta["t"] = serde_json::json!(t);
        if let (Some(w), Some(h)) = (w, ht) {
            meta["w"] = serde_json::json!(w);
            meta["ht"] = serde_json::json!(h);
        }
    }
    // качественный постер 360px: отдельный мини-блоб, в конверте только hash —
    // получатели докачивают его фоном за секунды (карточка как в Telegram)
    if let Some(pb) = poster_b64.filter(|p| !p.is_empty() && p.len() <= 600_000) {
        if let Ok(pbytes) = b64d(&pb) {
            let ptmp = cache_dir(&app).join(format!("ptmp-{}.jpg", super::now_ms()));
            if tokio::fs::write(&ptmp, &pbytes).await.is_ok() {
                if let Some(ph) = crate::p2p::seed_file(&app, &ptmp.to_string_lossy()).await {
                    let pdest = cache_dir(&app).join(format!("{}.jpg", ph));
                    if std::fs::rename(&ptmp, &pdest).is_err() {
                        let _ = std::fs::copy(&ptmp, &pdest);
                        let _ = std::fs::remove_file(&ptmp);
                    }
                    register_seed(&ph, &pdest);
                    super::media_seeders::announce(&ph, &node, pbytes.len() as u64, &[]);
                    meta["ph"] = serde_json::json!(ph);
                } else {
                    let _ = std::fs::remove_file(&ptmp);
                }
            }
        }
    }
    let text = format!("sn-vid:{}", meta);
    let me = super::my_address().unwrap_or_default();
    let msg = super::store_msg(&room, &me, &text, true)?;
    #[cfg(feature = "p2p")]
    {
        super::broadcast_envelope(&room, &msg);
        super::spawn_direct_fallback(room.clone(), msg.clone());
    }
    Ok(msg)
}

/// Чтение файла, брошенного в окно (нативный drag&drop Tauri отдаёт ПУТИ).
/// Лимит 50 МБ: крупные файлы (длинные видео) — через скрепку с чанк-аплоадом.
#[tauri::command(async)]
pub(crate) fn chat_read_dropped(path: String) -> Result<String, String> {
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a file".into());
    }
    if meta.len() > 50 * 1024 * 1024 {
        return Err("drop-too-big".into());
    }
    let b = std::fs::read(&path).map_err(|e| e.to_string())?;
    Ok(b64e(&b))
}

/// Видео по хэшу: P2P-докачка при необходимости, возврат ПУТИ к файлу в кэше
/// (проигрывается стримингом через asset-протокол, без чтения в память).
#[tauri::command]
pub(crate) async fn chat_load_video(
    app: tauri::AppHandle,
    room_id: String,
    hash: String,
    node: String,
) -> Result<String, String> {
    let p = load_media_path(&app, &room_id, &hash, &node, "mp4").await?;
    Ok(p.to_string_lossy().to_string())
}


/// «Save File»: докачивает медиа в кэш (если ещё нет) и копирует его на диск
/// через системный диалог «Сохранить как». false = пользователь отменил.
#[tauri::command]
pub(crate) async fn chat_save_media(
    app: tauri::AppHandle,
    room_id: String,
    hash: String,
    node: String,
    name: String,
    mime: String,
) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;
    let ext = if mime.starts_with("image") {
        "jpg"
    } else if mime.starts_with("video") {
        "mp4"
    } else if mime.contains("webm") {
        "webm"
    } else if mime.contains("mp4") || mime.contains("m4a") || mime.contains("aac") {
        "m4a"
    } else if mime.contains("pdf") {
        "pdf"
    } else if mime.contains("markdown") {
        "md"
    } else {
        "mp3"
    };
    // Файл берём из кэша НАПРЯМУЮ: не ждём FETCH_GATE (глобальный шлюз докачки
    // мог быть занят чужой загрузкой — тогда диалог «не появлялся» вовсе).
    let src = cache_dir(&app).join(format!("{}.{}", hash, ext));
    if !src.is_file() {
        // ВАЖНО: только путь, БЕЗ чтения файла в память — load_media (b64)
        // на 500-МБ видео раздувал процесс на ~2.5 ГБ → native OOM-crash.
        let _ = load_media_path(&app, &room_id, &hash, &node, ext).await?;
    }
    let mut suggested = name.trim().to_string();
    if suggested.is_empty() {
        suggested = format!("file.{}", ext);
    } else if !suggested.to_lowercase().ends_with(&format!(".{}", ext)) {
        suggested = format!("{}.{}", suggested, ext);
    }
    let picked = app
        .dialog()
        .file()
        .set_file_name(&suggested)
        .add_filter(ext.to_uppercase(), &[ext])
        .blocking_save_file();
    let Some(target) = picked.and_then(|f| f.into_path().ok()) else {
        return Ok(false);
    };
    // Windows-диалог может вернуть имя без расширения — дописываем сами.
    let mut target = target;
    if target.extension().is_none() {
        target.set_extension(ext);
    }
    std::fs::copy(&src, &target).map_err(|e| e.to_string())?;
    Ok(true)
}

// ---- суточная уборка кэша ----------------------------------------------------

/// Раз в сутки: если autoclean включён и объём папки превысил лимит —
/// удаляем 25% самых старых файлов (по mtime).
pub(crate) fn spawn_cache_sweeper(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(180)).await;
        loop {
            let a = app.clone();
            let _ = tokio::task::spawn_blocking(move || sweep_once(&a)).await;
            tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)).await;
        }
    });
}

fn sweep_once(app: &tauri::AppHandle) {
    if !autoclean_on() {
        return;
    }
    let dir = cache_dir(app);
    let max = cache_max_bytes();
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            if let Ok(md) = e.metadata() {
                if md.is_file() {
                    total += md.len();
                    files.push((
                        md.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                        e.path(),
                    ));
                }
            }
        }
    }
    if total <= max || files.is_empty() {
        return;
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let kill = ((files.len() + 3) / 4).max(1);
    for (_, p) in files.iter().take(kill) {
        let _ = std::fs::remove_file(p);
    }
    crate::syslog::log("chat", format!("media cache sweep: removed {} old file(s)", kill));
}
