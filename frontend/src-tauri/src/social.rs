//! Социальный слой SmartNet — децентрализованная стена (микроблог).
//!
//! Хранилище изолировано по локальному профилю: `data/users/<user>/wall/`.
//!   • `avatar.png`             — аватар пользователя (строго 128×128).
//!   • `wall/images/<hash>.png` — картинки постов (даунскейл ≤ 1024×768).
//!   • `wall/snapshot.json`     — снапшот истории стены (раздаётся новым подписчикам).
//! Картинки/снапшоты регистрируются в iroh-blobs (CID = BLAKE3) и раздаются.
//!
//! Сеть (Фаза 2): у каждого автора — свой gossip-топик `blake3("smartnet-wall" ‖ addr)`.
//! Автор вещает подписанные события (пост/коммент/удаление/модерация/снапшот) в свой
//! топик; подписчики получают их вживую, а офлайн-историю добирают из снапшота-блоба.
//! Локальный кэш постов/комментов/банов — в `Sled` (общая БД `DB`).
//!
//! Автор: TechnoL0g · Создано: 2026-06

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tauri_plugin_dialog::DialogExt;

use crate::{ensure_data_dir, DB};

/// Стабильный локальный идентификатор текущего пользователя (адрес кошелька).
fn my_address() -> String {
    crate::load_vault_internal()
        .map(|v| v.address)
        .unwrap_or_default()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Каталог соц-данных текущего пользователя: `data/users/<address>/`.
fn user_root(app: &AppHandle) -> Result<PathBuf, String> {
    let addr = my_address();
    if addr.is_empty() {
        return Err("locked".to_string());
    }
    let root = ensure_data_dir(app).join("users").join(&addr);
    std::fs::create_dir_all(root.join("wall").join("images")).map_err(|e| e.to_string())?;
    Ok(root)
}

/// Общий кэш скачанных из сети картинок/снапшотов чужих стен.
fn cache_dir(app: &AppHandle) -> PathBuf {
    let dir = ensure_data_dir(app).join("social_cache");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn avatar_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(user_root(app)?.join("avatar.png"))
}

/// data-URL из PNG-файла (небольшой — аватар/превью безопасно инлайнить в UI).
fn png_data_url(path: &PathBuf) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

// ======================= Подпись/проверка событий =======================

/// Подписать каноничную строку ключом кошелька (secp256k1 ECDSA).
/// Возвращает (pubkeyHex, sigHex).
fn sign(canonical: &str) -> Result<(String, String), String> {
    let seed = crate::SESSION_SEED.lock().unwrap().clone().ok_or("locked")?;
    let (sk, pk_hex, _addr) = crate::crypto::keys_from_material(seed.trim())?;
    let secp = secp256k1::Secp256k1::signing_only();
    let msg = secp256k1::Message::from_digest(crate::crypto::sha256(canonical.as_bytes()));
    let sig = secp.sign_ecdsa(&msg, &sk);
    Ok((pk_hex, hex::encode(sig.serialize_compact())))
}

/// Проверить подпись И что её автор (pubkey → адрес) совпадает с `owner`.
fn verify(canonical: &str, pubkey_hex: &str, sig_hex: &str, owner: &str) -> bool {
    let pk_bytes = match hex::decode(pubkey_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };
    if crate::crypto::address_from_pubkey(&pk_bytes) != owner {
        return false;
    }
    let pk = match secp256k1::PublicKey::from_slice(&pk_bytes) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let sb = match hex::decode(sig_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let sig = match secp256k1::ecdsa::Signature::from_compact(&sb) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let secp = secp256k1::Secp256k1::verification_only();
    let msg = secp256k1::Message::from_digest(crate::crypto::sha256(canonical.as_bytes()));
    secp.verify_ecdsa(&msg, &sig, &pk).is_ok()
}

// ============================ Модели данных ============================

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct PostFile {
    pub cid: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    #[serde(default)]
    pub node: String, // EndpointId автора (bootstrap для скачивания файла)
    #[serde(default)]
    pub title: String, // ID3 Title (для аудио)
    #[serde(default)]
    pub artist: String, // ID3 Artist (для аудио)
    #[serde(default)]
    pub cover: String, // BLAKE3 CID обложки из ID3 APIC (wall/images), пусто если нет
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct AlbumRef {
    pub id: String,
    #[serde(default)]
    pub manifest: String,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub price: f64,
    #[serde(default)]
    pub free: bool,
    #[serde(default)]
    pub tracks: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Post {
    pub id: String,
    pub author: String,
    pub text: String,
    #[serde(default)]
    pub images: Vec<String>, // BLAKE3 CID картинок (iroh-blobs)
    #[serde(default)]
    pub files: Vec<PostFile>, // прикреплённые файлы (mp3/mp4/png/jpg…)
    #[serde(default)]
    pub avatar: String, // CID аватара автора на момент поста
    #[serde(default)]
    pub node: String, // EndpointId автора (bootstrap для скачивания медиа)
    #[serde(default)]
    pub album: Option<AlbumRef>, // кликабельный баннер альбома (анонс)
    pub ts: i64,
    pub pubkey: String,
    pub sig: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Comment {
    pub id: String,
    pub post_id: String,
    pub post_author: String,
    pub author: String,
    pub text: String,
    pub ts: i64,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub avatar: String, // CID аватара автора комментария (косметика)
    pub pubkey: String,
    pub sig: String,
}

fn post_canonical(p: &Post) -> String {
    let files: Vec<&str> = p.files.iter().map(|f| f.cid.as_str()).collect();
    format!(
        "post|{}|{}|{}|{}|{}|{}",
        p.id,
        p.author,
        p.ts,
        p.text,
        p.images.join(","),
        files.join(",")
    )
}

fn comment_canonical(c: &Comment) -> String {
    format!(
        "comment|{}|{}|{}|{}|{}|{}",
        c.id, c.post_author, c.post_id, c.author, c.ts, c.text
    )
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Reaction {
    pub post_author: String,
    pub post_id: String,
    pub reactor: String,
    pub emoji: String, // "" = снята
    pub ts: i64,
    pub pubkey: String,
    pub sig: String,
}

fn react_canonical(r: &Reaction) -> String {
    format!(
        "react|{}|{}|{}|{}|{}",
        r.post_author, r.post_id, r.reactor, r.emoji, r.ts
    )
}

fn gen_id(seed: &str) -> String {
    blake3::hash(format!("{}|{}", seed, now_ms()).as_bytes()).to_hex()[..24].to_string()
}

// ============================ Хранилище (Sled) ============================

fn post_key(author: &str, ts: i64, id: &str) -> String {
    format!("social:post:{}:{:020}:{}", author, ts, id)
}
fn comment_key(post_author: &str, post_id: &str, ts: i64, id: &str) -> String {
    format!("social:cmt:{}:{}:{:020}:{}", post_author, post_id, ts, id)
}
fn react_key(post_author: &str, post_id: &str, reactor: &str) -> String {
    format!("social:react:{}:{}:{}", post_author, post_id, reactor)
}

fn store_post(p: &Post) {
    if let Ok(bytes) = serde_json::to_vec(p) {
        let _ = DB.insert(post_key(&p.author, p.ts, &p.id).as_bytes(), bytes);
        let _ = DB.flush();
    }
}
fn store_comment(c: &Comment) {
    if let Ok(bytes) = serde_json::to_vec(c) {
        let _ = DB.insert(
            comment_key(&c.post_author, &c.post_id, c.ts, &c.id).as_bytes(),
            bytes,
        );
        let _ = DB.flush();
    }
}

fn get_reaction(pa: &str, pid: &str, reactor: &str) -> Option<Reaction> {
    DB.get(react_key(pa, pid, reactor).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Reaction>(&v).ok())
}
fn store_reaction(r: &Reaction) {
    if let Ok(bytes) = serde_json::to_vec(r) {
        let _ = DB.insert(
            react_key(&r.post_author, &r.post_id, &r.reactor).as_bytes(),
            bytes,
        );
        let _ = DB.flush();
    }
}
fn reactions_of(pa: &str, pid: &str) -> Vec<Reaction> {
    let prefix = format!("social:react:{}:{}:", pa, pid);
    DB.scan_prefix(prefix.as_bytes())
        .filter_map(|kv| kv.ok())
        .filter_map(|(_, v)| serde_json::from_slice::<Reaction>(&v).ok())
        .collect()
}

/// Уже видели это событие? (защита от повторной обработки в gossip.)
fn seen(id: &str) -> bool {
    let k = format!("social:seen:{}", id);
    if DB.get(k.as_bytes()).ok().flatten().is_some() {
        return true;
    }
    let _ = DB.insert(k.as_bytes(), b"1");
    false
}

fn is_blocked_by_owner(owner: &str, target: &str) -> bool {
    DB.get(format!("social:block:{}:{}", owner, target).as_bytes())
        .ok()
        .flatten()
        .is_some()
}

/// Тумбстоун удалённого поста (чтобы устаревший снапшот не воскресил его).
fn mark_deleted(author: &str, id: &str) {
    let _ = DB.insert(format!("social:del:{}:{}", author, id).as_bytes(), b"1");
}
pub(crate) fn is_deleted(author: &str, id: &str) -> bool {
    DB.get(format!("social:del:{}:{}", author, id).as_bytes())
        .ok()
        .flatten()
        .is_some()
}

/// Запомнить актуальный CID аватара автора (+ его node) для показа у подписчиков.
fn set_author_avatar(author: &str, cid: &str, node: &str) {
    if cid.trim().is_empty() {
        return;
    }
    let _ = DB.insert(
        format!("social:avcid:{}", author).as_bytes(),
        format!("{}\t{}", cid, node).as_bytes(),
    );
}
fn author_avatar_of(author: &str) -> Option<(String, String)> {
    let v = DB.get(format!("social:avcid:{}", author).as_bytes()).ok().flatten()?;
    let s = String::from_utf8(v.to_vec()).ok()?;
    let mut parts = s.splitn(2, '\t');
    let cid = parts.next()?.to_string();
    let node = parts.next().unwrap_or("").to_string();
    if cid.is_empty() { None } else { Some((cid, node)) }
}
fn is_comment_hidden(post_author: &str, comment_id: &str) -> bool {
    DB.get(format!("social:hidden:{}:{}", post_author, comment_id).as_bytes())
        .ok()
        .flatten()
        .is_some()
}

pub(crate) fn wall_posts(author: &str, limit: usize) -> Vec<Post> {
    let prefix = format!("social:post:{}:", author);
    let mut out: Vec<Post> = DB
        .scan_prefix(prefix.as_bytes())
        .filter_map(|kv| kv.ok())
        .filter_map(|(_, v)| serde_json::from_slice::<Post>(&v).ok())
        .collect();
    out.sort_by(|a, b| b.ts.cmp(&a.ts)); // новые сверху
    out.truncate(limit);
    out
}

pub(crate) fn post_comments(post_author: &str, post_id: &str) -> Vec<Comment> {
    let prefix = format!("social:cmt:{}:{}:", post_author, post_id);
    let mut out: Vec<Comment> = DB
        .scan_prefix(prefix.as_bytes())
        .filter_map(|kv| kv.ok())
        .filter_map(|(_, v)| serde_json::from_slice::<Comment>(&v).ok())
        .filter(|c| !is_comment_hidden(post_author, &c.id))
        .collect();
    out.sort_by(|a, b| a.ts.cmp(&b.ts)); // старые сверху (хронология)
    out
}

// ============================ DTO для фронтенда ============================

#[derive(Serialize, Clone)]
pub(crate) struct PostView {
    pub id: String,
    pub author: String,
    pub text: String,
    pub images: Vec<String>,
    pub files: Vec<PostFile>,
    pub avatar: String,
    pub node: String,
    pub ts: i64,
    pub comments: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<AlbumRef>,
}

fn post_view(p: &Post) -> PostView {
    PostView {
        id: p.id.clone(),
        author: p.author.clone(),
        text: p.text.clone(),
        images: p.images.clone(),
        files: p.files.clone(),
        avatar: p.avatar.clone(),
        node: p.node.clone(),
        ts: p.ts,
        comments: post_comments(&p.author, &p.id).len(),
        album: p.album.clone(),
    }
}

// ============================ Обработка изображений ============================

/// Обработать выбранный файл в аватар 128×128 PNG и сохранить на диск.
fn process_avatar(app: &AppHandle, src_path: &str) -> Result<(PathBuf, String), String> {
    let img = image::open(src_path.trim())
        .map_err(|e| format!("Не удалось открыть изображение: {}", e))?;
    let square = img.resize_to_fill(128, 128, image::imageops::FilterType::Lanczos3);
    let mut buf = Vec::new();
    square
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| format!("Не удалось перекодировать: {}", e))?;
    let hash = blake3::hash(&buf).to_hex().to_string();
    let dest = avatar_path(app)?;
    std::fs::write(&dest, &buf).map_err(|e| e.to_string())?;
    // копию сохраняем под CID в кэше, чтобы стабильно раздавать хэш аватара
    let cached = cache_dir(app).join(format!("{}.png", hash));
    if !cached.exists() {
        let _ = std::fs::write(&cached, &buf);
    }
    Ok((cached, hash))
}

/// Даунскейл картинки поста ≤ 1024×768 (сохраняя пропорции) → PNG на диск.
fn process_post_image(app: &AppHandle, src_path: &str) -> Result<(PathBuf, String), String> {
    let img = image::open(src_path.trim())
        .map_err(|e| format!("Не удалось открыть изображение: {}", e))?;
    let scaled = if img.width() > 1024 || img.height() > 768 {
        img.resize(1024, 768, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let mut buf = Vec::new();
    scaled
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| format!("Не удалось перекодировать: {}", e))?;
    let hash = blake3::hash(&buf).to_hex().to_string();
    let dest = user_root(app)?
        .join("wall")
        .join("images")
        .join(format!("{}.png", hash));
    if !dest.exists() {
        std::fs::write(&dest, &buf).map_err(|e| e.to_string())?;
    }
    Ok((dest, hash))
}

// ============================ Tauri-команды: медиа ============================

/// Открыть системный диалог выбора картинки (png/jpg) для аватара/поста.
#[tauri::command(async)]
pub(crate) fn pick_image_file(app: AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .add_filter("Изображения", &["png", "jpg", "jpeg", "webp"])
        .blocking_pick_file()
        .and_then(|f| f.into_path().ok())
        .map(|p| p.to_string_lossy().to_string())
}

/// Установить аватар: сжать до 128×128, сохранить, зарегистрировать iroh-blob.
/// Возвращает data-URL нового аватара для мгновенного показа в UI.
#[tauri::command]
pub(crate) async fn set_profile_avatar(app: AppHandle, src_path: String) -> Result<String, String> {
    let app2 = app.clone();
    let (dest, hash) =
        tauri::async_runtime::spawn_blocking(move || process_avatar(&app2, &src_path))
            .await
            .map_err(|e| e.to_string())??;
    let _ = crate::p2p::seed_file(&app, &dest.to_string_lossy()).await;
    let addr = my_address();
    let _ = DB.insert(format!("social:avatar:{}", addr).as_bytes(), hash.as_bytes());
    let node = crate::p2p::node_id(&app).await.unwrap_or_default();
    set_author_avatar(&addr, &hash, &node);
    let _ = DB.flush();
    // v1.63.2: сразу зеркалим PNG в общий кэш по CID — этот путь читают
    // все виджеты (UnlockLayer, Wall, Profile) через `author_avatar_cached`
    // и `fetch_wall_media`. Без зеркалирования собственный аватар был
    // доступен только через `avatar.png`, а после ре-логина или из чужих
    // компонентов иногда падал на Identicon, пока iroh loopback не
    // засеет копию. Копия ~10 KB, race conditions устраняются полностью.
    {
        let cache = cache_dir(&app).join(format!("{}.png", &hash));
        if !cache.exists() {
            let _ = std::fs::create_dir_all(cache.parent().unwrap_or(&cache));
            if let Err(e) = std::fs::copy(&dest, &cache) {
                crate::syslog::log("social", format!("avatar cache mirror fail: {e}"));
            }
        }
    }
    png_data_url(&avatar_path(&app)?).ok_or_else(|| "avatar read failed".to_string())
}

/// data-URL собственного аватара (если установлен) — для шапки профиля.
#[tauri::command]
pub(crate) async fn get_profile_avatar(app: AppHandle) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || png_data_url(&avatar_path(&app).ok()?))
        .await
        .ok()
        .flatten()
}

/// BLAKE3-хэш собственного аватара (CID для раздачи/встраивания в снапшот).
#[tauri::command]
pub(crate) async fn my_avatar_hash() -> Option<String> {
    let addr = my_address();
    DB.get(format!("social:avatar:{}", addr).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
}

#[derive(Serialize, Clone)]
pub(crate) struct AuthorAvatar {
    pub cid: String,
    pub node: String,
}

/// CID+node аватара автора (для показа аватара чужого профиля у подписчика).
#[tauri::command]
pub(crate) async fn author_avatar(author: String) -> Option<AuthorAvatar> {
    author_avatar_of(&author).map(|(cid, node)| AuthorAvatar { cid, node })
}

/// МГНОВЕННЫЙ аватар автора из локального кэша (без сети): проверяем ВСЕ
/// три возможных места хранения PNG-байтов аватара:
///   1. `data/users/<addr>/avatar.png` — собственный загруженный аватар
///      (только если author == my_address; путь к user_root требует
///      разблокированного кошелька, но при вводе PIN адрес уже известен
///      и `my_address()` возвращает его до полной разблокировки).
///   2. `data/social_cache/<cid>.png` — сюда `fetch_wall_media` кладёт
///      скачанные через iroh медиа стены (аватары/картинки чужих постов).
///   3. `data/users/<addr>/wall/images/<cid>.png` — свои прикреплённые
///      картинки к постам.
///   4. `data/filesdata/<xx>/<cid>` — файлообменник (legacy fallback).
/// Возврат None → UI покажет Identicon и параллельно тянет из сети через
/// fetch_wall_media (которая сохранит блоб в social_cache для мгновенного
/// показа при следующем запуске).
///
/// v1.63.1 BUGFIX: раньше проверялся только пункт (4); собственный аватар
/// (пункт 1) при вводе PIN отображался как Identicon у пользователей,
/// у которых P2P-loopback ещё не успел засеять аватар обратно в кэш.
#[tauri::command]
pub(crate) async fn author_avatar_cached(app: AppHandle, author: String) -> Option<String> {
    let (cid, _node) = author_avatar_of(&author)?;
    let is_me = author == my_address();
    tauri::async_runtime::spawn_blocking(move || {
        // (1) собственный аватар — сразу читаем с диска, без CID (файл лежит
        // под фиксированным именем avatar.png в user_root).
        if is_me {
            if let Ok(root) = user_root(&app) {
                let own = root.join("avatar.png");
                if own.exists() {
                    if let Some(url) = png_data_url(&own) {
                        return Some(url);
                    }
                }
            }
        }
        // (2) общий кэш скачанных через P2P медиа.
        let cache = cache_dir(&app).join(format!("{}.png", cid));
        if cache.exists() {
            if let Some(url) = png_data_url(&cache) {
                return Some(url);
            }
        }
        // (3) свои картинки стены (для авторов, у которых стены-снапшоты
        // экспортировали аватар в этот путь исторически).
        if let Ok(root) = user_root(&app) {
            let wall_img = root.join("wall").join("images").join(format!("{}.png", cid));
            if wall_img.exists() {
                if let Some(url) = png_data_url(&wall_img) {
                    return Some(url);
                }
            }
        }
        // (4) legacy fallback: файлообменник.
        let p = crate::files::file_store_path(&app, &cid);
        if p.exists() { png_data_url(&p) } else { None }
    })
    .await
    .ok()
    .flatten()
}

/// Прикрепить картинку к посту: даунскейл, сохранить в `wall/images/`,
/// импортировать в iroh-blobs. Возвращает BLAKE3-хэш (CID) для JSON поста.
#[tauri::command]
pub(crate) async fn attach_image_to_post(app: AppHandle, file_path: String) -> Result<String, String> {
    let app2 = app.clone();
    let (dest, hash) =
        tauri::async_runtime::spawn_blocking(move || process_post_image(&app2, &file_path))
            .await
            .map_err(|e| e.to_string())??;
    let _ = crate::p2p::seed_file(&app, &dest.to_string_lossy()).await;
    Ok(hash)
}

/// Скачать медиа стены (аватар/картинку) по CID и вернуть data-URL.
/// Сначала ищем локально (свои файлы/кэш), иначе тянем из сети через iroh-blobs.
#[tauri::command]
pub(crate) async fn fetch_wall_media(
    app: AppHandle,
    cid: String,
    node: String,
) -> Result<String, String> {
    if cid.trim().is_empty() {
        return Err("empty cid".to_string());
    }
    let app2 = app.clone();
    let cid2 = cid.clone();
    // локальный поиск (свои картинки/аватар/кэш) — без сети
    let local = tauri::async_runtime::spawn_blocking(move || -> Option<PathBuf> {
        let candidates = [
            cache_dir(&app2).join(format!("{}.png", cid2)),
            user_root(&app2)
                .ok()?
                .join("wall")
                .join("images")
                .join(format!("{}.png", cid2)),
        ];
        candidates.into_iter().find(|p| p.exists())
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Some(p) = local {
        return png_data_url(&p).ok_or_else(|| "read failed".to_string());
    }
    // добор из сети → кэш
    let dest = cache_dir(&app).join(format!("{}.png", cid));
    crate::p2p::fetch_file(&app, node.trim(), cid.trim(), &dest.to_string_lossy(), &[]).await?;
    png_data_url(&dest).ok_or_else(|| "read failed".to_string())
}

// ============================ Tauri-команды: файлы поста ============================

/// Тонкая обёртка для удаления локального файла из соц-слоя (диск + Sled).
fn purge_local_file(app: &AppHandle, cid: &str) {
    crate::files::purge_file(app, cid);
}

/// Удалить закэшированную картинку стены по CID (свои wall/images + общий кэш).
fn purge_wall_image(app: &AppHandle, cid: &str) {
    if cid.trim().is_empty() {
        return;
    }
    let _ = std::fs::remove_file(cache_dir(app).join(format!("{}.png", cid)));
    if let Ok(root) = user_root(app) {
        let _ = std::fs::remove_file(root.join("wall").join("images").join(format!("{}.png", cid)));
    }
}

/// Прикрепить файл к посту: выбор файла (нативный диалог) → сохранение в
/// filesdata/<XX>/<CID> + манифест в БД + сид iroh-blobs (публичный режим —
/// без приватного/платного). Возвращает описание прикрепления для поста.
#[tauri::command]
pub(crate) async fn social_attach_file(app: AppHandle, src_path: String) -> Result<PostFile, String> {
    // ID3 Artist/Title + встроенная обложка (APIC) — в blocking-потоке (CPU/IO).
    let sp = src_path.clone();
    let (title, artist, cover_bytes) = tauri::async_runtime::spawn_blocking(move || {
        let (t, a) = read_audio_tags(&sp);
        (t, a, read_audio_cover(&sp))
    })
    .await
    .map_err(|e| e.to_string())?;
    let m = crate::files::add_file(app.clone(), src_path, 0.0, false, vec![], Some(r#"{"cat":"music"}"#.into())).await?;
    let node = crate::p2p::node_id(&app).await.unwrap_or_default();
    // Обложку сохраняем как обычную картинку стены (content-addressed → дедуп по
    // альбому: одна обложка на много треков = один blob) и раздаём по iroh-blobs.
    let mut cover = String::new();
    if let Some(bytes) = cover_bytes {
        let app2 = app.clone();
        let res = tauri::async_runtime::spawn_blocking(move || process_cover_bytes(&app2, &bytes)).await;
        if let Ok(Ok((dest, hash))) = res {
            let _ = crate::p2p::seed_file(&app, &dest.to_string_lossy()).await;
            cover = hash;
        }
    }
    Ok(PostFile {
        cid: m.cid,
        name: m.name,
        size: m.size,
        mime: m.mime_type,
        node,
        title,
        artist,
        cover,
    })
}

/// Извлечь встроенную обложку (APIC) из аудиофайла. Предпочитает CoverFront,
/// иначе — первую картинку. Пусто, если картинок нет.
fn read_audio_cover(path: &str) -> Option<Vec<u8>> {
    use lofty::file::TaggedFileExt;
    use lofty::picture::PictureType;
    let probe = lofty::probe::Probe::open(path.trim()).and_then(|p| p.read()).ok()?;
    let tag = probe.primary_tag().or_else(|| probe.first_tag())?;
    let pics = tag.pictures();
    if pics.is_empty() {
        return None;
    }
    let pic = pics
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .unwrap_or(&pics[0]);
    Some(pic.data().to_vec())
}

/// Декодировать байты обложки → PNG ≤512×512 → BLAKE3 → wall/images/<hash>.png.
fn process_cover_bytes(app: &AppHandle, bytes: &[u8]) -> Result<(PathBuf, String), String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("cover decode: {}", e))?;
    let scaled = if img.width() > 512 || img.height() > 512 {
        img.resize(512, 512, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let mut buf = Vec::new();
    scaled
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let hash = blake3::hash(&buf).to_hex().to_string();
    let dest = user_root(app)?
        .join("wall")
        .join("images")
        .join(format!("{}.png", hash));
    if !dest.exists() {
        std::fs::write(&dest, &buf).map_err(|e| e.to_string())?;
    }
    Ok((dest, hash))
}

/// Прочитать ID3/метаданные (Title, Artist) из аудиофайла. Пусто — если нет тегов.
fn read_audio_tags(path: &str) -> (String, String) {
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;
    let probe = match lofty::probe::Probe::open(path.trim()).and_then(|p| p.read()) {
        Ok(f) => f,
        Err(_) => return (String::new(), String::new()),
    };
    let tag = probe.primary_tag().or_else(|| probe.first_tag());
    match tag {
        Some(t) => (
            t.title().map(|s| s.to_string()).unwrap_or_default(),
            t.artist().map(|s| s.to_string()).unwrap_or_default(),
        ),
        None => (String::new(), String::new()),
    }
}

/// Убедиться, что файл поста есть локально (иначе — добор из сети по CID),
/// вернуть его байты в base64 (для проигрывания mp3/показа/скачивания).
#[tauri::command]
pub(crate) async fn social_file_bytes(
    app: AppHandle,
    cid: String,
    node: String,
) -> Result<String, String> {
    if cid.trim().is_empty() {
        return Err("empty cid".to_string());
    }
    let dest = crate::files::file_store_path(&app, cid.trim());
    if !dest.exists() {
        crate::p2p::fetch_file(&app, node.trim(), cid.trim(), &dest.to_string_lossy(), &[]).await?;
    }
    let app2 = app.clone();
    let cid2 = cid.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = std::fs::read(crate::files::file_store_path(&app2, &cid2))
            .map_err(|e| e.to_string())?;
        // E2EE-альбомы: зашифрованный трек прозрачно расшифровывается локальным
        // ключом (владелец или покупатель после album_fetch_key).
        if let Some(plain) = try_decrypt_album_track(&cid2, &bytes) {
            return Ok(base64::engine::general_purpose::STANDARD.encode(plain));
        }
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Если cid — трек зашифрованного альбома и ключ есть локально → плейнтекст.
fn try_decrypt_album_track(cid: &str, data: &[u8]) -> Option<Vec<u8>> {
    for kv in DB.scan_prefix(b"social:album:").flatten() {
        let a = match serde_json::from_slice::<Album>(&kv.1) {
            Ok(a) => a,
            Err(_) => continue,
        };
        if !a.encrypted || !a.tracks.iter().any(|t| t.cid == cid) {
            continue;
        }
        let akh = DB
            .get(format!("social:albumkey:{}", a.id).as_bytes())
            .ok()
            .flatten()?;
        let mut ak = [0u8; 32];
        hex::decode_to_slice(String::from_utf8_lossy(&akh).as_ref(), &mut ak).ok()?;
        return crate::crypto::aes_gcm_decrypt(&ak, data).ok();
    }
    None
}

/// Скачать файл поста и сохранить копию через системный диалог «Сохранить как».
#[tauri::command]
pub(crate) async fn social_save_file(
    app: AppHandle,
    cid: String,
    node: String,
    name: String,
) -> Result<bool, String> {
    let src = crate::files::file_store_path(&app, cid.trim());
    if !src.exists() {
        crate::p2p::fetch_file(&app, node.trim(), cid.trim(), &src.to_string_lossy(), &[]).await?;
    }
    let picked = app
        .dialog()
        .file()
        .set_file_name(&name)
        .blocking_save_file();
    let Some(target) = picked.and_then(|f| f.into_path().ok()) else {
        return Ok(false);
    };
    std::fs::copy(&src, &target).map_err(|e| e.to_string())?;
    Ok(true)
}

// ============================ Персональный плейлист ============================

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct PlaylistTrack {
    pub cid: String,
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub ts: i64,
}

/// Добавить трек в мой персональный плейлист (виден другим на моём профиле).
#[tauri::command]
pub(crate) async fn playlist_add(mut track: PlaylistTrack) -> Result<(), String> {
    let me = my_address();
    if me.is_empty() {
        return Err("locked".to_string());
    }
    if track.cid.trim().is_empty() {
        return Err("empty".to_string());
    }
    // защита от дублей: один трек (cid) в плейлисте один раз
    let dup_prefix = format!("social:pl:{}:", me);
    for kv in DB.scan_prefix(dup_prefix.as_bytes()).filter_map(|k| k.ok()) {
        if let Ok(t) = serde_json::from_slice::<PlaylistTrack>(&kv.1) {
            if t.cid == track.cid {
                return Ok(());
            }
        }
    }
    if track.ts == 0 {
        track.ts = now_ms();
    }
    let key = format!("social:pl:{}:{:020}:{}", me, track.ts, track.cid);
    let bytes = serde_json::to_vec(&track).map_err(|e| e.to_string())?;
    let _ = DB.insert(key.as_bytes(), bytes);
    let _ = DB.flush();
    Ok(())
}

/// Удалить трек из моего плейлиста по CID.
#[tauri::command]
pub(crate) async fn playlist_remove(cid: String) -> Result<(), String> {
    let me = my_address();
    let prefix = format!("social:pl:{}:", me);
    for kv in DB.scan_prefix(prefix.as_bytes()).filter_map(|k| k.ok()) {
        if let Ok(t) = serde_json::from_slice::<PlaylistTrack>(&kv.1) {
            if t.cid == cid {
                let _ = DB.remove(&kv.0);
            }
        }
    }
    let _ = DB.flush();
    Ok(())
}

/// Плейлист пользователя `owner` (виден другим). Порядок берётся из
/// `social:pl_order:<owner>` (drag-and-drop); треки вне списка порядка идут
/// после упорядоченных, новые сверху.
#[tauri::command]
pub(crate) async fn playlist_list(owner: String) -> Vec<PlaylistTrack> {
    tauri::async_runtime::spawn_blocking(move || {
        let prefix = format!("social:pl:{}:", owner);
        let mut out: Vec<PlaylistTrack> = DB
            .scan_prefix(prefix.as_bytes())
            .filter_map(|kv| kv.ok())
            .filter_map(|(_, v)| serde_json::from_slice::<PlaylistTrack>(&v).ok())
            .collect();
        // явный порядок (если сохранён пользователем)
        let order: Vec<String> = DB
            .get(format!("social:pl_order:{}", owner).as_bytes())
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_slice::<Vec<String>>(&v).ok())
            .unwrap_or_default();
        let pos: std::collections::HashMap<&String, usize> =
            order.iter().enumerate().map(|(i, c)| (c, i)).collect();
        out.sort_by(|a, b| match (pos.get(&a.cid), pos.get(&b.cid)) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.ts.cmp(&a.ts),
        });
        out
    })
    .await
    .unwrap_or_default()
}

/// Сохранить порядок треков моего плейлиста (drag-and-drop). `order` — массив CID.
#[tauri::command]
pub(crate) async fn playlist_reorder(order: Vec<String>) -> Result<(), String> {
    let me = my_address();
    if me.is_empty() {
        return Err("locked".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let key = format!("social:pl_order:{}", me);
        if let Ok(bytes) = serde_json::to_vec(&order) {
            let _ = DB.insert(key.as_bytes(), bytes);
            let _ = DB.flush();
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}



// ============================ Музыкальные альбомы (бесплатные) ============================

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct AlbumTrack {
    pub title: String,
    pub cid: String,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub cover: String,
    /// CID бесплатного ~30-сек превью (незашифрованный фрагмент платного трека).
    #[serde(default)]
    pub preview: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Album {
    pub id: String,
    pub owner: String,
    pub artist: String,
    pub title: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub node: String,
    pub tracks: Vec<AlbumTrack>,
    pub created: i64,
    #[serde(default)]
    pub free: bool,
    #[serde(default)]
    pub price: f64,
    #[serde(default)]
    pub manifest: String,
    #[serde(default)]
    pub year: i32,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub pubkey: String,
    #[serde(default)]
    pub sig: String,
    /// Строгое E2EE: треки зашифрованы AES-256-GCM ключом альбома; покупатели
    /// получают ECIES-обёрнутый ключ из keyfile'а владельца (per-buyer DRM).
    #[serde(default)]
    pub encrypted: bool,
}

fn album_canonical(a: &Album) -> String {
    let tracks: Vec<String> = a.tracks.iter().map(|t| t.cid.clone()).collect();
    format!(
        "album|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        a.id, a.owner, a.created, a.title, a.artist, a.cover, a.price, a.year, a.note, tracks.join(",")
    )
}

fn album_key(owner: &str, created: i64, id: &str) -> String {
    format!("social:album:{}:{:020}:{}", owner, created, id)
}

fn store_album(a: &Album) {
    if let Ok(bytes) = serde_json::to_vec(a) {
        let _ = DB.insert(album_key(&a.owner, a.created, &a.id).as_bytes(), bytes);
        let _ = DB.flush();
    }
}

/// Мультивыбор аудиофайлов для загрузки альбома.
#[tauri::command(async)]
pub(crate) fn pick_audio_files(app: AppHandle) -> Vec<String> {
    app.dialog()
        .file()
        .add_filter("Аудио", &["mp3", "flac", "wav", "m4a", "ogg", "aac"])
        .blocking_pick_files()
        .map(|v| {
            v.into_iter()
                .filter_map(|f| f.into_path().ok())
                .map(|p| p.to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Опубликовать бесплатный альбом: обложка + треки → iroh-блобы, подписанный
/// манифест в Sled + пост-анонс на своей стене (подписчики видят и слушают).
#[tauri::command]
pub(crate) async fn album_create(
    app: AppHandle,
    title: String,
    artist: String,
    cover_path: String,
    track_paths: Vec<String>,
    track_titles: Vec<String>,
    price: f64,
    year: i32,
    note: String,
    preview: bool,
) -> Result<Album, String> {
    let owner = my_address();
    if owner.is_empty() {
        return Err("locked".into());
    }
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("Укажите название альбома".into());
    }
    if track_paths.is_empty() {
        return Err("Добавьте хотя бы один трек".into());
    }
    if track_paths.len() > 50 {
        return Err("Не более 50 треков в альбоме".into());
    }

    let node = crate::p2p::node_id(&app).await.unwrap_or_default();
    let is_paid = price > 0.0;
    // v1.60 SOFT-PAID: платные альбомы больше НЕ используют строгое E2EE.
    // Опыт эксплуатации показал, что доставка per-buyer ECIES-ключа через
    // P2P слишком часто ломается (владелец офлайн → покупатель месяцами
    // ждёт ключ). Возвращаемся к простой блокчейн-модели: оплата → memo
    // `albumbuy:<id>:<buyer>` → `album_access` подтверждает по on-chain
    // истории → покупатель играет. Инфраструктура E2EE
    // (`album_ak`, `album_fetch_key`, `album_sync_keys`) остаётся в
    // кодовой базе для СТАРЫХ зашифрованных альбомов (`encrypted: true`),
    // но НОВЫЕ платные альбомы публикуются без AES-обёртки.

    // Обложка альбома (если задана) → wall/images + сид.
    let mut cover = String::new();
    if !cover_path.trim().is_empty() {
        let app2 = app.clone();
        let cp = cover_path.clone();
        if let Ok(Ok((dest, hash))) =
            tauri::async_runtime::spawn_blocking(move || process_post_image(&app2, &cp)).await
        {
            let _ = crate::p2p::seed_file(&app, &dest.to_string_lossy()).await;
            cover = hash;
        }
    }

    // Треки: теги/обложка + добавление в файловый слой (сид iroh-blobs).
    let mut tracks: Vec<AlbumTrack> = Vec::new();
    for (i, path) in track_paths.iter().enumerate() {
        let sp = path.clone();
        let (t_title, t_artist, cover_bytes) = tauri::async_runtime::spawn_blocking(move || {
            let (t, a) = read_audio_tags(&sp);
            (t, a, read_audio_cover(&sp))
        })
        .await
        .map_err(|e| e.to_string())?;
        let (t_cid, t_size, t_mime, t_preview) = if is_paid {
            // v1.60 SOFT-PAID: полный трек сидируется в plaintext через file
            // layer (как бесплатный трек). Опционально режем ~30-сек превью
            // (первые 768 KB — играбельно для mp3/flac с начала файла)
            // отдельным CID'ом, чтобы не-купившие могли попробовать альбом.
            let m = crate::files::add_file(app.clone(), path.clone(), 0.0, false, vec![], Some(r#"{"cat":"music"}"#.into())).await?;
            let pcid = if preview {
                let sp2 = path.clone();
                let app2 = app.clone();
                let pdest = tauri::async_runtime::spawn_blocking(
                    move || -> Result<std::path::PathBuf, String> {
                        const PREVIEW_BYTES: usize = 768 * 1024;
                        let bytes = std::fs::read(&sp2).map_err(|e| e.to_string())?;
                        let cut = bytes.len().min(PREVIEW_BYTES);
                        let frag = &bytes[..cut];
                        let p = cache_dir(&app2)
                            .join(format!("albumprev-{}.bin", blake3::hash(frag).to_hex()));
                        std::fs::write(&p, frag).map_err(|e| e.to_string())?;
                        Ok(p)
                    },
                )
                .await
                .map_err(|e| e.to_string())??;
                crate::p2p::seed_file(&app, &pdest.to_string_lossy())
                    .await
                    .unwrap_or_default()
            } else {
                String::new()
            };
            (m.cid, m.size, m.mime_type, pcid)
        } else {
            let m = crate::files::add_file(app.clone(), path.clone(), 0.0, false, vec![], Some(r#"{"cat":"music"}"#.into())).await?;
            (m.cid, m.size, m.mime_type, String::new())
        };
        // обложка трека: общая обложка альбома, иначе — из ID3 этого трека
        let mut tcover = cover.clone();
        if tcover.is_empty() {
            if let Some(bytes) = cover_bytes {
                let app2 = app.clone();
                if let Ok(Ok((dest, hash))) =
                    tauri::async_runtime::spawn_blocking(move || process_cover_bytes(&app2, &bytes)).await
                {
                    let _ = crate::p2p::seed_file(&app, &dest.to_string_lossy()).await;
                    tcover = hash;
                }
            }
        }
        // если обложка альбома не задавалась — берём первую найденную из треков
        if cover.is_empty() && !tcover.is_empty() {
            cover = tcover.clone();
        }
        let manual = track_titles.get(i).map(|s| s.trim().to_string()).unwrap_or_default();
        let disp = if !manual.is_empty() {
            manual
        } else if !t_title.is_empty() {
            t_title.clone()
        } else {
            std::path::Path::new(path)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| format!("Track {}", i + 1))
        };
        tracks.push(AlbumTrack {
            title: disp,
            cid: t_cid,
            node: node.clone(),
            size: t_size,
            mime: t_mime,
            artist: if t_artist.is_empty() { artist.clone() } else { t_artist },
            cover: tcover,
            preview: t_preview,
        });
    }

    let created = now_ms();
    let id = gen_id(&format!("{}|album|{}", owner, title));
    // v1.60 SOFT-PAID: local albumkey хранилище больше НЕ используется для
    // новых альбомов (см. комментарий выше). Инфраструктура сохраняется
    // только для чтения ключей СТАРЫХ (encrypted=true) альбомов.
    let mut album = Album {
        id: id.clone(),
        owner: owner.clone(),
        artist: if artist.trim().is_empty() { owner.clone() } else { artist.trim().to_string() },
        title: title.clone(),
        cover: cover.clone(),
        node: node.clone(),
        tracks,
        created,
        free: !is_paid,
        price: if is_paid { price } else { 0.0 },
        manifest: String::new(),
        year: if year > 0 { year } else { 0 },
        note: note.trim().chars().take(2000).collect(),
        pubkey: String::new(),
        sig: String::new(),
        encrypted: false,
    };
    let (pk, sig) = sign(&album_canonical(&album))?;
    album.pubkey = pk;
    album.sig = sig;
    // Сидируем сам манифест как iroh-blob → его CID уедет в ссылку для шаринга
    // (получатель добирает манифест по CID и проверяет подпись).
    {
        let app2 = app.clone();
        let alb = album.clone();
        let seeded = tauri::async_runtime::spawn_blocking(move || -> Option<(std::path::PathBuf, String)> {
            let bytes = serde_json::to_vec(&alb).ok()?;
            let hash = blake3::hash(&bytes).to_hex().to_string();
            let dest = cache_dir(&app2).join(format!("album-{}.json", hash));
            std::fs::write(&dest, &bytes).ok()?;
            Some((dest, hash))
        })
        .await
        .ok()
        .flatten();
        if let Some((dest, hash)) = seeded {
            let _ = crate::p2p::seed_file(&app, &dest.to_string_lossy()).await;
            album.manifest = hash;
        }
    }
    store_album(&album);

    // Анонс на своей стене с кликабельным баннером альбома (AlbumRef).
    // Платный: только обложка + баннер (без файлов, чтобы контент не раздавался
    // бесплатно). Бесплатный: обложка + баннер + треки как файлы (играются в ленте).
    let images = if cover.is_empty() { vec![] } else { vec![cover.clone()] };
    let aref = AlbumRef {
        id: album.id.clone(),
        manifest: album.manifest.clone(),
        node: node.clone(),
        title: album.title.clone(),
        cover: cover.clone(),
        price: album.price,
        free: album.free,
        tracks: album.tracks.len(),
    };
    if is_paid {
        let text = format!("🎵 **{}** — платный альбом · {} трек(ов) · {} STH", title, album.tracks.len(), price);
        let _ = create_post(app.clone(), text, images, vec![], Some(aref)).await;
    } else {
        let post_files: Vec<PostFile> = album
            .tracks
            .iter()
            .map(|t| PostFile {
                cid: t.cid.clone(),
                name: format!("{}.mp3", t.title),
                size: t.size,
                mime: if t.mime.is_empty() { "audio/mpeg".into() } else { t.mime.clone() },
                node: t.node.clone(),
                title: t.title.clone(),
                artist: t.artist.clone(),
                cover: t.cover.clone(),
            })
            .collect();
        let text = format!("🎵 **{}** — альбом · {} трек(ов)", title, album.tracks.len());
        let _ = create_post(app.clone(), text, images, post_files, Some(aref)).await;
    }

    // NAT-proof gossip: подписчики получат тост «новый альбом» через шину.
    // Работает и для FREE (без keyfile) и для PAID (до первой покупки).
    album_new_broadcast(&album.id, &album.owner, &album.title, is_paid);

    Ok(album)
}

/// Прочитать ID3-теги (Title/Artist) без добавления файлов — для предзаполнения
/// названий треков в «Студии музыканта».
#[derive(Serialize, Clone)]
pub(crate) struct AudioMeta {
    pub title: String,
    pub artist: String,
}

#[tauri::command]
pub(crate) async fn read_audio_meta(paths: Vec<String>) -> Vec<AudioMeta> {
    tauri::async_runtime::spawn_blocking(move || {
        paths
            .iter()
            .map(|p| {
                let (title, artist) = read_audio_tags(p);
                AudioMeta { title, artist }
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default()
}

/// Найти альбом по id среди всех известных локально манифестов.
fn find_album(id: &str) -> Option<Album> {
    DB.scan_prefix(b"social:album:")
        .filter_map(|kv| kv.ok())
        .filter_map(|(_, v)| serde_json::from_slice::<Album>(&v).ok())
        .find(|a| a.id == id)
}

/// Все альбомы владельца (для встраивания в снапшот стены).
fn albums_of(owner: &str) -> Vec<Album> {
    let prefix = format!("social:album:{}:", owner);
    DB.scan_prefix(prefix.as_bytes())
        .filter_map(|kv| kv.ok())
        .filter_map(|(_, v)| serde_json::from_slice::<Album>(&v).ok())
        .collect()
}

/// Получить один альбом по id (для отдельной страницы альбома).
#[tauri::command]
pub(crate) async fn album_get(id: String) -> Option<Album> {
    tauri::async_runtime::spawn_blocking(move || find_album(&id))
        .await
        .ok()
        .flatten()
}

/// Редактировать метаданные собственного альбома (название, артист, год,
/// заметка, цена, при необходимости — новая обложка). Треки не меняются.
/// Манифест перевыпускается: пере-подпись + пересидирование блоба (новый CID),
/// пересборка снапшота — чтобы у подписчиков обновились карточки.
#[tauri::command]
pub(crate) async fn album_update(
    app: AppHandle,
    id: String,
    title: String,
    artist: String,
    cover_path: String,
    price: f64,
    year: i32,
    note: String,
) -> Result<Album, String> {
    let me = my_address();
    if me.is_empty() {
        return Err("locked".into());
    }
    let mut album = find_album(&id).ok_or("альбом не найден")?;
    if album.owner != me {
        return Err("это не ваш альбом".into());
    }
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("Укажите название альбома".into());
    }

    // Новая обложка (если выбрана) → wall/images + сид.
    if !cover_path.trim().is_empty() {
        let app2 = app.clone();
        let cp = cover_path.clone();
        if let Ok(Ok((dest, hash))) =
            tauri::async_runtime::spawn_blocking(move || process_post_image(&app2, &cp)).await
        {
            let _ = crate::p2p::seed_file(&app, &dest.to_string_lossy()).await;
            album.cover = hash;
        }
    }

    let is_paid = price > 0.0;
    album.title = title;
    album.artist = if artist.trim().is_empty() { album.owner.clone() } else { artist.trim().to_string() };
    album.year = if year > 0 { year } else { 0 };
    album.note = note.trim().chars().take(2000).collect();
    album.free = !is_paid;
    album.price = if is_paid { price } else { 0.0 };

    // Пере-подпись + пересидирование манифеста (новый CID).
    let (pk, sig) = sign(&album_canonical(&album))?;
    album.pubkey = pk;
    album.sig = sig;
    {
        let app2 = app.clone();
        let alb = album.clone();
        let seeded = tauri::async_runtime::spawn_blocking(move || -> Option<(std::path::PathBuf, String)> {
            let bytes = serde_json::to_vec(&alb).ok()?;
            let hash = blake3::hash(&bytes).to_hex().to_string();
            let dest = cache_dir(&app2).join(format!("album-{}.json", hash));
            std::fs::write(&dest, &bytes).ok()?;
            Some((dest, hash))
        })
        .await
        .ok()
        .flatten();
        if let Some((dest, hash)) = seeded {
            let _ = crate::p2p::seed_file(&app, &dest.to_string_lossy()).await;
            album.manifest = hash;
        }
    }
    store_album(&album);
    net::rebuild_snapshot(app);
    Ok(album)
}

/// Удалить собственный альбом ТОЛЬКО у автора: манифест убирается из локальной
/// БД (исчезает из профиля/ленты автора и из его снапшота стены). Блобы треков
/// не вычищаются, событие удаления не рассылается — поэтому у тех, кто уже
/// купил/скачал альбом, доступ и локальная копия сохраняются.
#[tauri::command]
pub(crate) async fn album_delete(app: AppHandle, id: String) -> Result<(), String> {
    let me = my_address();
    if me.is_empty() {
        return Err("locked".into());
    }
    let album = find_album(&id).ok_or("альбом не найден")?;
    if album.owner != me {
        return Err("это не ваш альбом".into());
    }
    let me2 = me.clone();
    let id2 = id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let key = album_key(&me2, album.created, &id2);
        let _ = DB.remove(key.as_bytes());
        let _ = DB.flush();
    })
    .await
    .map_err(|e| e.to_string())?;
    net::rebuild_snapshot(app);
    Ok(())
}

/// Купить платный альбом: перевод STH напрямую владельцу с memo
/// `albumbuy:<id>:<buyer>` (как у платных файлов). Возвращает txid.
#[tauri::command]
pub(crate) async fn album_buy(id: String) -> Result<String, String> {
    let a = find_album(&id).ok_or("альбом не найден")?;
    if a.free || a.price <= 0.0 {
        return Err("альбом бесплатный".to_string());
    }
    let buyer = my_address();
    if buyer.is_empty() {
        return Err("locked".to_string());
    }
    if a.owner == buyer {
        return Err("это ваш альбом".to_string());
    }
    let secret = {
        let guard = crate::SESSION_SEED.lock().unwrap();
        guard.as_ref().ok_or("locked")?.clone()
    };
    let price = format!("{}", a.price);
    let memo = format!("albumbuy:{}:{}", id, buyer);
    let owner = a.owner.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        crate::sth_node::send_transfer(&secret, &owner, &price, "", &memo)
    })
    .await
    .map_err(|e| e.to_string())??;
    if !outcome.accepted {
        return Err("Сеть отклонила оплату (проверьте баланс)".to_string());
    }
    // +100 к репутации покупателя за покупку альбома
    crate::reputation::add(&buyer, 100);
    Ok(outcome.id)
}

/// Адрес сжигания STH: 90% стоимости дизлайка уходит сюда безвозвратно.
pub(crate) const BURN_ADDR: &str = "smartHoLdemBurnAddrHereXXXmUW7f";

/// Дизлайк пользователя: платная операция (1 STH), понижает репутацию цели на 1.
/// Оплата двумя переводами с memo `reputacion-off`, комиссия 0.1 STH:
///   0.9 STH → адрес сжигания (BURN_ADDR), 0.1 STH → дизлайкнутому адресу.
#[tauri::command]
pub(crate) async fn dislike_user(target: String) -> Result<String, String> {
    let me = my_address();
    if me.is_empty() {
        return Err("locked".to_string());
    }
    if me == target {
        return Err("нельзя дизлайкнуть самого себя".to_string());
    }
    let secret = {
        let guard = crate::SESSION_SEED.lock().unwrap();
        guard.as_ref().ok_or("locked")?.clone()
    };
    let t2 = target.clone();
    let txid = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let memo = "reputacion-off";
        // 90% → сжигание
        let burn = crate::sth_node::send_transfer(&secret, BURN_ADDR, "0.9", "0.1", memo)?;
        if !burn.accepted {
            return Err("Сеть отклонила транзакцию сжигания (проверьте баланс)".to_string());
        }
        // 10% → дизлайкнутому автору
        let to_author = crate::sth_node::send_transfer(&secret, &t2, "0.1", "0.1", memo)?;
        if !to_author.accepted {
            return Err("Сеть отклонила перевод автору".to_string());
        }
        Ok(burn.id)
    })
    .await
    .map_err(|e| e.to_string())??;
    // локально понижаем репутацию цели
    crate::reputation::add(&target, -1);
    Ok(txid)
}

/// Доступ к альбому: бесплатный/владелец → да; иначе — подтверждённая оплата
/// `albumbuy:<id>:<buyer>` в истории владельца.
#[tauri::command]
pub(crate) async fn album_access(id: String) -> bool {
    let a = match find_album(&id) {
        Some(a) => a,
        None => return false,
    };
    let buyer = my_address();
    if a.free || a.price <= 0.0 || a.owner == buyer || buyer.is_empty() {
        return a.free || a.price <= 0.0 || a.owner == buyer;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let expect = format!("albumbuy:{}:{}", a.id, buyer);
        crate::sth_node::fetch_history(&a.owner, 50)
            .map(|items| {
                items.iter().any(|it| {
                    it.kind == "in" && it.confirmed && it.counterparty == buyer && it.memo == expect
                })
            })
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// MIME аудиофайла по расширению (для манифеста E2EE-трека).
fn audio_mime_of(path: &str) -> String {
    match std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
        .as_str()
    {
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "m4a" | "aac" => "audio/mp4",
        "ogg" => "audio/ogg",
        _ => "audio/mpeg",
    }
    .to_string()
}

/// Владелец: выпустить ECIES-обёрнутые ключи всем подтверждённым покупателям.
/// Keyfile {buyer → wrapped} сидируется iroh-blob'ом, хэш анонсируется в DHT
/// (album_infohash) — покупатель получает ключ, пока владелец онлайн.
/// Возвращает число покупателей с выпущенным ключом.
#[tauri::command]
pub(crate) async fn album_sync_keys(app: AppHandle, id: String) -> Result<u32, String> {
    let a = find_album(&id).ok_or("альбом не найден")?;
    let me = my_address();
    if me.is_empty() {
        return Err("locked".to_string());
    }
    if a.owner != me {
        return Err("ключи выпускает только владелец альбома".to_string());
    }
    if !a.encrypted {
        return Ok(0);
    }
    let ak_hex = DB
        .get(format!("social:albumkey:{}", id).as_bytes())
        .ok()
        .flatten()
        .map(|v| String::from_utf8_lossy(&v).to_string())
        .ok_or("ключ альбома не найден в локальном хранилище")?;
    let mut ak = [0u8; 32];
    hex::decode_to_slice(&ak_hex, &mut ak).map_err(|e| e.to_string())?;

    // Подтверждённые покупатели из on-chain истории владельца.
    let owner = a.owner.clone();
    let aid = a.id.clone();
    let buyers: Vec<String> = tauri::async_runtime::spawn_blocking(move || {
        use std::collections::HashSet;
        let prefix = format!("albumbuy:{}:", aid);
        let mut out: HashSet<String> = HashSet::new();
        if let Ok(items) = crate::sth_node::fetch_history(&owner, 500) {
            for it in items {
                if it.kind == "in" && it.confirmed {
                    if let Some(rest) = it.memo.strip_prefix(&prefix) {
                        if !rest.is_empty() {
                            out.insert(rest.to_string());
                        }
                    }
                }
            }
        }
        out.into_iter().collect()
    })
    .await
    .map_err(|e| e.to_string())?;

    let keys_key = format!("social:albumkeys:{}", id);
    let mut map: std::collections::HashMap<String, String> = DB
        .get(keys_key.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();
    let mut added = 0u32;
    for b in buyers {
        if map.contains_key(&b) {
            continue;
        }
        let b2 = b.clone();
        // pubkey покупателя гарантированно on-chain: он платил за альбом.
        let pk = tauri::async_runtime::spawn_blocking(move || {
            crate::sth_node::fetch_wallet_pubkey(&b2)
        })
        .await
        .map_err(|e| e.to_string())?;
        if let Ok(Some(pk)) = pk {
            if let Ok(w) = crate::crypto::ecies_wrap_key(&pk, &ak) {
                map.insert(b, w);
                added += 1;
            }
        }
    }

    let published = DB
        .get(format!("social:albumkeysblob:{}", id).as_bytes())
        .ok()
        .flatten()
        .is_some();
    if added > 0 || !published {
        let bytes = serde_json::to_vec(&map).map_err(|e| e.to_string())?;
        let dest = cache_dir(&app).join(format!("albumkeys-{}.json", id));
        std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
        let hash = crate::p2p::seed_file(&app, &dest.to_string_lossy())
            .await
            .ok_or("P2P-узел не запущен — не удалось засидировать keyfile")?;
        let _ = DB.insert(keys_key.as_bytes(), bytes);
        let _ = DB.insert(
            format!("social:albumkeysblob:{}", id).as_bytes(),
            hash.clone().into_bytes(),
        );
        let _ = DB.flush();
        crate::p2p::announce_album_keys(&id, &hash);
        // NAT-proof: мгновенный gossip-анонс keyfile'а (DHT/бикон за NAT
        // недостижимы — покупатель получит провайдера через gossip-шину).
        #[cfg(feature = "p2p")]
        if let Ok(eid) = crate::p2p::node_id(&app).await {
            if let Some(p) = album_gossip_payload(&id, &eid, &hash) {
                crate::chat::gossip::broadcast_on_devhub_topic(p);
            }
        }
        crate::syslog::log(
            "social",
            format!("E2EE album {}: keyfile published ({} buyers)", id, map.len()),
        );
    }
    Ok(map.len() as u32)
}

/// Покупатель: получить и распечатать свой ключ альбома (после оплаты).
/// Ищем владельца в DHT → качаем keyfile → ECIES-unwrap своим секретом.
#[tauri::command]
pub(crate) async fn album_fetch_key(app: AppHandle, id: String) -> Result<bool, String> {
    let key_key = format!("social:albumkey:{}", id);
    if DB.get(key_key.as_bytes()).ok().flatten().is_some() {
        return Ok(true);
    }
    let me = my_address();
    if me.is_empty() {
        return Err("locked".to_string());
    }
    let (providers, xh) = crate::p2p::album_keys_discover(&id).await;
    let hash = xh.ok_or("владелец альбома сейчас офлайн — ключ пока недоступен")?;
    if providers.is_empty() {
        return Err("владелец альбома сейчас офлайн — ключ пока недоступен".to_string());
    }
    let tmp = std::env::temp_dir().join(format!("albkeys-{}.json", id));
    crate::p2p::fetch_file(&app, &providers[0], &hash, &tmp.to_string_lossy(), &providers[1..])
        .await?;
    let raw = std::fs::read(&tmp).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp);
    let map: std::collections::HashMap<String, String> =
        serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let wrapped = map.get(&me).ok_or(
        "ключ для вашего адреса ещё не выпущен — владелец синхронизирует продажи при следующем выходе в сеть",
    )?;
    let secret = {
        let guard = crate::SESSION_SEED.lock().unwrap();
        guard.as_ref().ok_or("locked")?.clone()
    };
    let ak = crate::crypto::ecies_unwrap_key(&secret, wrapped)?;
    let _ = DB.insert(key_key.as_bytes(), hex::encode(ak).into_bytes());
    let _ = DB.flush();
    crate::syslog::log("social", format!("E2EE album {}: key received and unpacked", id));
    Ok(true)
}

// ------------------------------------------------------------
//  Gossip-анонсы keyfile'ов платных альбомов (NAT-proof канал)
// ------------------------------------------------------------

/// Компактный анонс владельца альбома в общей gossip-шине (см. chat/gossip.rs).
#[derive(serde::Serialize, serde::Deserialize)]
struct AlbumKeyAnnounce {
    v: u32,
    /// маркер типа сообщения ("alb")
    k: String,
    /// id альбома
    id: String,
    /// iroh EndpointId владельца (провайдер keyfile-blob'а)
    e: String,
    /// blake3-хэш keyfile-blob'а
    x: String,
    t: u64,
    /// адрес автора (для тоста «новый альбом»); опционально для обратной
    /// совместимости со старыми клиентами.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    o: String,
    /// название альбома (truncated 96), опционально.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    n: String,
}

/// Компактный анонс «новый альбом» — без keyfile (работает и для FREE-альбомов).
/// Используется, чтобы подписчики получили тост о выпуске сразу после публикации,
/// даже если keyfile ещё не выпущен (никто не купил).
#[derive(serde::Serialize, serde::Deserialize)]
struct AlbumNewAnnounce {
    v: u32,
    /// маркер типа сообщения ("albnew")
    k: String,
    /// id альбома
    id: String,
    /// адрес автора
    o: String,
    /// название альбома (truncated 96)
    n: String,
    /// платный ли альбом
    #[serde(default)]
    p: bool,
    t: u64,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn album_gossip_payload(id: &str, eid: &str, hash: &str) -> Option<Vec<u8>> {
    let (o, n) = album_owner_title(id);
    serde_json::to_vec(&AlbumKeyAnnounce {
        v: 1,
        k: "alb".into(),
        id: id.to_string(),
        e: eid.to_string(),
        x: hash.to_string(),
        t: now_secs(),
        o,
        n,
    })
    .ok()
}

/// Найти (owner_address, title) альбома по id, сканируя `social:album:` префикс
/// в Sled. Возвращает пустые строки, если альбом ещё не в кэше.
fn album_owner_title(id: &str) -> (String, String) {
    for kv in DB.scan_prefix(b"social:album:").flatten() {
        let Ok(a) = serde_json::from_slice::<Album>(&kv.1) else { continue };
        if a.id == id {
            let mut t = a.title.clone();
            if t.chars().count() > 96 {
                t = t.chars().take(96).collect();
            }
            return (a.owner.clone(), t);
        }
    }
    (String::new(), String::new())
}

/// Анонсы keyfile'ов ВСЕХ моих платных альбомов (периодический бродкаст).
pub(crate) async fn album_gossip_payloads(app: &tauri::AppHandle) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let eid = match crate::p2p::node_id(app).await {
        Ok(e) => e,
        Err(_) => return out,
    };
    for kv in DB.scan_prefix(b"social:albumkeysblob:").flatten() {
        let (Ok(ks), Ok(hash)) = (
            std::str::from_utf8(&kv.0).map(|s| s.to_string()),
            String::from_utf8(kv.1.to_vec()),
        ) else {
            continue;
        };
        let id = ks.trim_start_matches("social:albumkeysblob:");
        if id.is_empty() || hash.is_empty() {
            continue;
        }
        if let Some(p) = album_gossip_payload(id, &eid, &hash) {
            out.push(p);
        }
        if out.len() >= 16 {
            break;
        }
    }
    // Периодический «новый альбом»-анонс для СВЕЖИХ (24ч) публикаций —
    // подписчик, только что вышедший в сеть, всё равно получит тост.
    let cutoff = now_secs().saturating_sub(24 * 3600);
    let me = my_address();
    if !me.is_empty() {
        for kv in DB.scan_prefix(b"social:album:").flatten() {
            let Ok(a) = serde_json::from_slice::<Album>(&kv.1) else { continue };
            if a.owner != me {
                continue;
            }
            if (a.created as u64) < cutoff {
                continue;
            }
            let mut n = a.title.clone();
            if n.chars().count() > 96 {
                n = n.chars().take(96).collect();
            }
            if let Ok(p) = serde_json::to_vec(&AlbumNewAnnounce {
                v: 1,
                k: "albnew".into(),
                id: a.id.clone(),
                o: a.owner.clone(),
                n,
                p: !a.free,
                t: now_secs(),
            }) {
                out.push(p);
            }
            if out.len() >= 32 {
                break;
            }
        }
    }
    out
}

/// Rate-limit входящих альбомных анонсов (per album id).
static ALB_GOSSIP_SEEN: std::sync::Mutex<Option<std::collections::HashMap<String, u64>>> =
    std::sync::Mutex::new(None);

/// Обработать входящий анонс keyfile'а. true — сообщение было альбомным
/// (даже если отброшено), чтобы devhub-диспетчер не пробовал другие форматы.
/// Безопасность: анонс — подсказка; сам ключ покупатель распечатывает ECIES
/// своим секретом, чужой/поддельный keyfile бесполезен.
pub(crate) fn handle_album_gossip(app: &tauri::AppHandle, bytes: &[u8]) -> bool {
    // Разбираем как компактный «новый альбом» — работает и для FREE-альбомов
    // (без keyfile). Единственная задача — тост подписчикам.
    if let Ok(nn) = serde_json::from_slice::<AlbumNewAnnounce>(bytes) {
        if nn.v == 1 && nn.k == "albnew" && !nn.id.is_empty() && !nn.o.is_empty() {
            handle_album_new_announce(app, &nn);
            return true;
        }
    }
    let Ok(an) = serde_json::from_slice::<AlbumKeyAnnounce>(bytes) else {
        return false;
    };
    if an.v != 1 || an.k != "alb" {
        return false;
    }
    if an.id.is_empty() || an.e.is_empty() || an.x.is_empty() {
        return true;
    }
    if an.id.len() > 64 || an.e.len() > 80 || an.x.len() > 80 {
        return true;
    }
    {
        let mut guard = ALB_GOSSIP_SEEN.lock().unwrap();
        let map = guard.get_or_insert_with(Default::default);
        let now = now_secs();
        if let Some(last) = map.get(&an.id) {
            if now.saturating_sub(*last) < 30 {
                return true;
            }
        }
        map.insert(an.id.clone(), now);
        if map.len() > 2048 {
            map.retain(|_, t| now.saturating_sub(*t) < 3600);
        }
    }
    // EndpointId владельца → кэш discovery (album_keys_discover фолбэкается).
    let pkey = format!("dht:providers:albkeys:{}", an.id);
    let mut ids: Vec<String> = DB
        .get(pkey.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if !ids.contains(&an.e) {
        ids.insert(0, an.e.clone());
        ids.truncate(8);
        let _ = DB.insert(pkey.as_bytes(), ids.join(",").into_bytes());
    }
    let _ = DB.insert(
        format!("social:albgossip:xh:{}", an.id).as_bytes(),
        an.x.clone().into_bytes(),
    );
    // Расширенный «alb»-анонс (с полями o/n) тоже может нести первое упоминание
    // альбома от подписки → пробуем эмитнуть тост «новый альбом» (идемпотентно).
    if !an.o.is_empty() && !an.n.is_empty() {
        let nn = AlbumNewAnnounce {
            v: 1,
            k: "albnew".into(),
            id: an.id.clone(),
            o: an.o.clone(),
            n: an.n.clone(),
            p: !an.x.is_empty(),
            t: an.t,
        };
        handle_album_new_announce(app, &nn);
    }
    true
}

/// «Новый альбом»: если автор в моих подписках и id не виден ранее — тост.
fn handle_album_new_announce(app: &tauri::AppHandle, nn: &AlbumNewAnnounce) {
    if nn.id.len() > 64 || nn.o.len() > 64 || nn.n.len() > 128 {
        return;
    }
    let me = my_address();
    if me == nn.o {
        return;
    }
    // Идемпотентность: первое упоминание альбома → тост.
    let seen_key = format!("social:albseen:{}", nn.id);
    if DB.get(seen_key.as_bytes()).ok().flatten().is_some() {
        return;
    }
    let _ = DB.insert(seen_key.as_bytes(), now_secs().to_string().into_bytes());
    // Тостом бьём только по подпискам (иначе UI-спам всей сети).
    if me.is_empty() || !is_following(&me, &nn.o) {
        return;
    }
    let username = crate::naming::username_of_address(nn.o.clone()).unwrap_or_default();
    let _ = app.emit(
        "music-new-album",
        serde_json::json!({
            "id": nn.id,
            "owner": nn.o,
            "username": username,
            "title": nn.n,
            "paid": nn.p,
        }),
    );
    crate::syslog::log(
        "social",
        format!("gossip: new album «{}» from {} — toast to subscriber", nn.n, nn.o),
    );
}

/// Проверить: адрес `me` подписан на `author` (по кэшу follow-статистики).
fn is_following(me: &str, author: &str) -> bool {
    let Some(bytes) = DB.get(format!("subs:statscache:{}", me).as_bytes()).ok().flatten() else {
        return false;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    v.get("followingList")
        .and_then(|l| l.as_array())
        .map(|arr| {
            arr.iter().any(|e| {
                e.get("address").and_then(|a| a.as_str()) == Some(author)
            })
        })
        .unwrap_or(false)
}

/// Мгновенный gossip-анонс «новый альбом» (вызывается из album_create для paid и free).
#[cfg(feature = "p2p")]
pub(crate) fn album_new_broadcast(id: &str, owner: &str, title: &str, paid: bool) {
    let mut n = title.to_string();
    if n.chars().count() > 96 {
        n = n.chars().take(96).collect();
    }
    let payload = match serde_json::to_vec(&AlbumNewAnnounce {
        v: 1,
        k: "albnew".into(),
        id: id.to_string(),
        o: owner.to_string(),
        n,
        p: paid,
        t: now_secs(),
    }) {
        Ok(p) => p,
        Err(_) => return,
    };
    crate::chat::gossip::broadcast_on_devhub_topic(payload);
}

#[cfg(not(feature = "p2p"))]
pub(crate) fn album_new_broadcast(_id: &str, _owner: &str, _title: &str, _paid: bool) {}

/// Число продаж альбома: уникальные покупатели по подтверждённым входящим
#[tauri::command]
pub(crate) async fn album_sales(id: String) -> usize {
    let a = match find_album(&id) {
        Some(a) => a,
        None => return 0,
    };
    if a.free {
        return 0;
    }
    tauri::async_runtime::spawn_blocking(move || {
        use std::collections::HashSet;
        let prefix = format!("albumbuy:{}:", a.id);
        let mut buyers: HashSet<String> = HashSet::new();
        if let Ok(items) = crate::sth_node::fetch_history(&a.owner, 500) {
            for it in items {
                if it.kind == "in" && it.confirmed {
                    if let Some(rest) = it.memo.strip_prefix(&prefix) {
                        if !rest.is_empty() {
                            buyers.insert(rest.to_string());
                        }
                    }
                }
            }
        }
        buyers.len()
    })
    .await
    .unwrap_or(0)
}

/// Суммарный доход автора по альбому: сумма STH по всем подтверждённым входящим
/// платежам `albumbuy:<id>:<buyer>` в истории владельца.
#[tauri::command]
pub(crate) async fn album_revenue(id: String) -> f64 {
    let a = match find_album(&id) {
        Some(a) => a,
        None => return 0.0,
    };
    if a.free {
        return 0.0;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let prefix = format!("albumbuy:{}:", a.id);
        let mut total = 0.0_f64;
        if let Ok(items) = crate::sth_node::fetch_history(&a.owner, 500) {
            for it in items {
                if it.kind == "in" && it.confirmed && it.memo.starts_with(&prefix) {
                    total += it.amount.parse::<f64>().unwrap_or(0.0);
                }
            }
        }
        total
    })
    .await
    .unwrap_or(0.0)
}

/// Временной ряд продаж альбома: точки {ts, amount} по подтверждённым входящим
/// платежам `albumbuy:<id>:<buyer>` (для мини-графика динамики спроса у автора).
#[derive(Serialize, Clone)]
pub(crate) struct SalePoint {
    pub ts: u64,
    pub amount: f64,
}

#[tauri::command]
pub(crate) async fn album_sales_series(id: String) -> Vec<SalePoint> {
    let a = match find_album(&id) {
        Some(a) => a,
        None => return Vec::new(),
    };
    if a.free {
        return Vec::new();
    }
    tauri::async_runtime::spawn_blocking(move || {
        let prefix = format!("albumbuy:{}:", a.id);
        let mut pts: Vec<SalePoint> = Vec::new();
        if let Ok(items) = crate::sth_node::fetch_history(&a.owner, 500) {
            for it in items {
                if it.kind == "in" && it.confirmed && it.memo.starts_with(&prefix) {
                    pts.push(SalePoint {
                        ts: it.timestamp,
                        amount: it.amount.parse::<f64>().unwrap_or(0.0),
                    });
                }
            }
        }
        pts.sort_by(|x, y| x.ts.cmp(&y.ts));
        pts
    })
    .await
    .unwrap_or_default()
}

/// Ссылка для шаринга альбома в сети: `sn://album/<id>~<manifestCid>~<node>`.
#[tauri::command]
pub(crate) async fn album_share_link(id: String) -> Result<String, String> {
    let a = find_album(&id).ok_or("альбом не найден")?;
    if a.manifest.is_empty() {
        return Err("манифест альбома ещё не засидирован".to_string());
    }
    Ok(format!("sn://album/{}~{}~{}", a.id, a.manifest, a.node))
}

/// Открыть альбом по ссылке `sn://album/<id>~<cid>~<node>`: добрать манифест из
/// сети по CID, проверить подпись владельца и сохранить локально. Возвращает id.
#[tauri::command]
pub(crate) async fn album_open_link(app: AppHandle, link: String) -> Result<String, String> {
    let body = link
        .trim()
        .trim_start_matches("sn://album/")
        .trim_start_matches("album/");
    let parts: Vec<&str> = body.split('~').collect();
    if parts.len() < 2 {
        return Err("некорректная ссылка".to_string());
    }
    let id = parts[0].to_string();
    let cid = parts[1].to_string();
    let node = parts.get(2).cloned().unwrap_or("").to_string();
    if let Some(existing) = find_album(&id) {
        return Ok(existing.id);
    }
    let dest = cache_dir(&app).join(format!("album-{}.json", cid));
    if !dest.exists() {
        crate::p2p::fetch_file(&app, node.trim(), cid.trim(), &dest.to_string_lossy(), &[]).await?;
    }
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let bytes = std::fs::read(&dest).map_err(|e| e.to_string())?;
        let album: Album = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if !verify(&album_canonical(&album), &album.pubkey, &album.sig, &album.owner) {
            return Err("подпись альбома недействительна".to_string());
        }
        store_album(&album);
        let _ = &app2;
        Ok(album.id)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Magnet-экспорт бесплатного альбома «во внешний мир»: собираем треки во
/// временную папку и создаём торрент (librqbit). Возвращает magnet-ссылку.
#[tauri::command]
pub(crate) async fn album_export_torrent(app: AppHandle, id: String) -> Result<String, String> {
    let a = find_album(&id).ok_or("альбом не найден")?;
    if !a.free {
        return Err("Magnet-экспорт доступен только для бесплатных альбомов".to_string());
    }
    fn sanitize(s: &str) -> String {
        s.chars()
            .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
            .collect::<String>()
            .trim()
            .to_string()
    }
    let dir = std::env::temp_dir().join(format!("smartnet-album-{}", sanitize(&a.title)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (i, tk) in a.tracks.iter().enumerate() {
        let src = crate::files::file_store_path(&app, &tk.cid);
        if !src.exists() {
            crate::p2p::fetch_file(&app, tk.node.trim(), tk.cid.trim(), &src.to_string_lossy(), &[]).await?;
        }
        let ext = match tk.mime.as_str() {
            "audio/flac" | "audio/x-flac" => "flac",
            "audio/wav" | "audio/x-wav" => "wav",
            "audio/ogg" => "ogg",
            "audio/mp4" | "audio/aac" => "m4a",
            _ => "mp3",
        };
        let fname = format!("{:02} - {}.{}", i + 1, sanitize(&tk.title), ext);
        let _ = std::fs::copy(&src, dir.join(fname));
    }
    let trackers = crate::torrent::get_bt_trackers().await;
    let comment = Some(format!("SmartNet album: {} — {}", a.artist, a.title));
    let meta = crate::torrent::create_torrent_from_path(
        app.clone(),
        dir.to_string_lossy().to_string(),
        None,
        trackers,
        comment,
        Some(false),
        None,
    )
    .await?;
    crate::torrent::get_torrent_magnet(meta.info_hash).await
}

/// Альбомы пользователя (новые сверху).
#[tauri::command]
pub(crate) async fn album_list(owner: String) -> Vec<Album> {
    tauri::async_runtime::spawn_blocking(move || {
        let prefix = format!("social:album:{}:", owner);
        let mut out: Vec<Album> = DB
            .scan_prefix(prefix.as_bytes())
            .filter_map(|kv| kv.ok())
            .filter_map(|(_, v)| serde_json::from_slice::<Album>(&v).ok())
            .collect();
        out.sort_by(|a, b| b.created.cmp(&a.created));
        out
    })
    .await
    .unwrap_or_default()
}

/// Все известные локально альбомы (глобальная лента «Музыка»): свои + артистов,
/// чьи манифесты уже получены (новые сверху).
#[tauri::command]
pub(crate) async fn album_feed() -> Vec<Album> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut out: Vec<Album> = DB
            .scan_prefix(b"social:album:")
            .filter_map(|kv| kv.ok())
            .filter_map(|(_, v)| serde_json::from_slice::<Album>(&v).ok())
            .collect();
        out.sort_by(|a, b| b.created.cmp(&a.created));
        out
    })
    .await
    .unwrap_or_default()
}

// ============================ Tauri-команды: стена ============================

/// Опубликовать пост на собственной стене (подписать + сохранить + разослать).
#[tauri::command]
pub(crate) async fn create_post(
    app: AppHandle,
    text: String,
    images: Vec<String>,
    files: Vec<PostFile>,
    album: Option<AlbumRef>,
) -> Result<PostView, String> {
    let author = my_address();
    if author.is_empty() {
        return Err("locked".to_string());
    }
    let text = text.trim().to_string();
    if text.is_empty() && images.is_empty() && files.is_empty() && album.is_none() {
        return Err("empty".to_string());
    }
    if text.chars().count() > 5000 {
        return Err("too long".to_string());
    }
    let ts = now_ms();
    let id = gen_id(&format!("{}|{}", author, text));
    let avatar = my_avatar_hash().await.unwrap_or_default();
    let node = crate::p2p::node_id(&app).await.unwrap_or_default();
    let mut p = Post {
        id: id.clone(),
        author: author.clone(),
        text,
        images,
        files,
        avatar,
        node,
        album,
        ts,
        pubkey: String::new(),
        sig: String::new(),
    };
    let (pk, sig) = sign(&post_canonical(&p))?;
    p.pubkey = pk;
    p.sig = sig;
    store_post(&p);
    let _ = seen(&p.id);
    // разослать в свой топик + пересобрать снапшот
    net::ensure_own(app.clone());
    net::broadcast(&author, &Envelope::Post(p.clone()));
    net::rebuild_snapshot(app.clone());
    Ok(post_view(&p))
}

/// Список постов стены автора (новые сверху). before_ts — курсор пагинации:
/// вернуть посты строго старше указанной метки времени.
#[tauri::command]
pub(crate) async fn list_wall(
    author: String,
    limit: Option<usize>,
    before_ts: Option<i64>,
) -> Vec<PostView> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut posts = wall_posts(&author, usize::MAX);
        if let Some(b) = before_ts {
            posts.retain(|p| p.ts < b);
        }
        posts.truncate(limit.unwrap_or(50));
        posts.iter().map(post_view).collect()
    })
    .await
    .unwrap_or_default()
}

/// Удалить собственный пост (подписанное событие удаления рассылается подписчикам).
#[tauri::command]
pub(crate) async fn delete_post(app: AppHandle, id: String) -> Result<(), String> {
    let author = my_address();
    if author.is_empty() {
        return Err("locked".to_string());
    }
    let author2 = author.clone();
    let id2 = id.clone();
    let app2 = app.clone();
    let ev = tauri::async_runtime::spawn_blocking(move || -> Result<Envelope, String> {
        let prefix = format!("social:post:{}:", author2);
        let mut removed = false;
        let mut file_cids: Vec<String> = Vec::new();
        for kv in DB.scan_prefix(prefix.as_bytes()).filter_map(|k| k.ok()) {
            if let Ok(p) = serde_json::from_slice::<Post>(&kv.1) {
                if p.id == id2 {
                    file_cids = p.files.iter().map(|f| f.cid.clone()).collect();
                    for cid in &p.images {
                        purge_wall_image(&app2, cid);
                    }
                    let _ = DB.remove(&kv.0);
                    removed = true;
                }
            }
        }
        if !removed {
            return Err("not found".to_string());
        }
        // удалить прикреплённые файлы с диска (и из индекса владельца)
        for cid in &file_cids {
            purge_local_file(&app2, cid);
        }
        mark_deleted(&author2, &id2);
        let _ = DB.flush();
        let ts = now_ms();
        let (pk, sig) = sign(&format!("delpost|{}|{}|{}", id2, author2, ts))?;
        Ok(Envelope::DeletePost { id: id2, author: author2, ts, pubkey: pk, sig })
    })
    .await
    .map_err(|e| e.to_string())??;
    net::broadcast(&author, &ev);
    net::rebuild_snapshot(app);
    Ok(())
}

/// Оставить комментарий к посту (подписать + сохранить + отправить в топик автора стены).
#[tauri::command]
pub(crate) async fn add_comment(
    app: AppHandle,
    post_author: String,
    post_id: String,
    text: String,
) -> Result<Comment, String> {
    let author = my_address();
    if author.is_empty() {
        return Err("locked".to_string());
    }
    // Репутация < 1 → нельзя комментировать чужие стены.
    if post_author != author && crate::reputation::effective(&author) < 1 {
        return Err("Недостаточно репутации для комментирования (нужно ≥ 1)".to_string());
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("empty".to_string());
    }
    if text.chars().count() > 2000 {
        return Err("too long".to_string());
    }
    let ts = now_ms();
    let id = gen_id(&format!("{}|{}", author, text));
    let node = crate::p2p::node_id(&app).await.unwrap_or_default();
    let avatar = my_avatar_hash().await.unwrap_or_default();
    let mut c = Comment {
        id,
        post_id,
        post_author: post_author.clone(),
        author: author.clone(),
        text,
        ts,
        node,
        avatar,
        pubkey: String::new(),
        sig: String::new(),
    };
    let (pk, sig) = sign(&comment_canonical(&c))?;
    c.pubkey = pk;
    c.sig = sig;
    store_comment(&c);
    let _ = seen(&c.id);
    net::broadcast(&post_author, &Envelope::Comment(c.clone()));
    if post_author == author {
        net::rebuild_snapshot(app);
    }
    Ok(c)
}

/// Комментарии к посту (хронологически, без скрытых).
#[tauri::command]
pub(crate) async fn list_comments(post_author: String, post_id: String) -> Vec<Comment> {
    tauri::async_runtime::spawn_blocking(move || post_comments(&post_author, &post_id))
        .await
        .unwrap_or_default()
}

// -------- Реакции (лайки) --------

#[derive(Serialize, Clone)]
pub(crate) struct ReactionSummary {
    pub counts: std::collections::HashMap<String, usize>,
    pub reactors: std::collections::HashMap<String, Vec<String>>,
    pub mine: String,
}

fn reaction_summary(pa: &str, pid: &str) -> ReactionSummary {
    let me = my_address();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut reactors: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut mine = String::new();
    for r in reactions_of(pa, pid) {
        if r.emoji.is_empty() {
            continue;
        }
        *counts.entry(r.emoji.clone()).or_insert(0) += 1;
        reactors.entry(r.emoji.clone()).or_default().push(r.reactor.clone());
        if r.reactor == me {
            mine = r.emoji.clone();
        }
    }
    ReactionSummary { counts, reactors, mine }
}

/// Агрегированные реакции поста + моя текущая реакция.
#[tauri::command]
pub(crate) async fn post_reactions(post_author: String, post_id: String) -> ReactionSummary {
    tauri::async_runtime::spawn_blocking(move || reaction_summary(&post_author, &post_id))
        .await
        .unwrap_or(ReactionSummary {
            counts: std::collections::HashMap::new(),
            reactors: std::collections::HashMap::new(),
            mine: String::new(),
        })
}

/// Поставить/сменить/снять реакцию на пост (переключение при повторном эмодзи).
/// Одна реакция на пользователя на пост; событие подписывается и рассылается.
#[tauri::command]
pub(crate) async fn react_to_post(
    app: AppHandle,
    post_author: String,
    post_id: String,
    emoji: String,
) -> Result<ReactionSummary, String> {
    let me = my_address();
    if me.is_empty() {
        return Err("locked".to_string());
    }
    let pa = post_author.clone();
    let pid = post_id.clone();
    let me2 = me.clone();
    let (reaction, summary) =
        tauri::async_runtime::spawn_blocking(move || -> Result<(Reaction, ReactionSummary), String> {
            let emoji = emoji.chars().take(8).collect::<String>();
            let current = get_reaction(&pa, &pid, &me2).map(|r| r.emoji).unwrap_or_default();
            let new_emoji = if current == emoji { String::new() } else { emoji };
            let mut r = Reaction {
                post_author: pa.clone(),
                post_id: pid.clone(),
                reactor: me2.clone(),
                emoji: new_emoji,
                ts: now_ms(),
                pubkey: String::new(),
                sig: String::new(),
            };
            let (pk, sig) = sign(&react_canonical(&r))?;
            r.pubkey = pk;
            r.sig = sig;
            store_reaction(&r);
            let summary = reaction_summary(&pa, &pid);
            Ok((r, summary))
        })
        .await
        .map_err(|e| e.to_string())??;
    net::broadcast(&post_author, &Envelope::React(reaction));
    if post_author == me {
        net::rebuild_snapshot(app);
    }
    Ok(summary)
}

// -------- Подписки на стены --------

/// Подписаться на стену автора (получать живые события + добрать историю).
#[tauri::command]
pub(crate) async fn follow_wall(author: String) -> Result<(), String> {
    if author.trim().is_empty() {
        return Err("empty".to_string());
    }
    let _ = DB.insert(format!("social:follow:{}", author).as_bytes(), b"1");
    let _ = DB.flush();
    net::subscribe(&author);
    Ok(())
}

#[tauri::command]
pub(crate) async fn unfollow_wall(author: String) -> Result<(), String> {
    let _ = DB.remove(format!("social:follow:{}", author).as_bytes());
    let _ = DB.flush();
    net::unsubscribe(&author);
    Ok(())
}

#[tauri::command]
pub(crate) async fn list_wall_follows() -> Vec<String> {
    tauri::async_runtime::spawn_blocking(|| {
        DB.scan_prefix(b"social:follow:")
            .filter_map(|kv| kv.ok())
            .filter_map(|(k, _)| {
                String::from_utf8(k.to_vec())
                    .ok()
                    .map(|s| s.trim_start_matches("social:follow:").to_string())
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// (post_id, text) недавних постов автора — для фонового пред-перевода ленты.
pub(crate) fn wall_post_texts(author: &str, limit: usize) -> Vec<(String, String)> {
    wall_posts(author, limit)
        .into_iter()
        .filter(|p| !p.text.trim().is_empty())
        .map(|p| (p.id, p.text))
        .collect()
}

/// Список стен, на которые подписан пользователь (sync-версия для пред-перевода).
pub(crate) fn wall_follow_list() -> Vec<String> {
    DB.scan_prefix(b"social:follow:")
        .filter_map(|kv| kv.ok())
        .filter_map(|(k, _)| {
            String::from_utf8(k.to_vec())
                .ok()
                .map(|s| s.trim_start_matches("social:follow:").to_string())
        })
        .collect()
}

#[tauri::command]
pub(crate) async fn is_following_wall(author: String) -> bool {
    DB.get(format!("social:follow:{}", author).as_bytes())
        .ok()
        .flatten()
        .is_some()
}

/// Открыть (просмотреть) стену: гарантирует подписку на топик для живых событий,
/// не добавляя её в постоянный список подписок.
#[tauri::command]
pub(crate) async fn open_wall(author: String) {
    if !author.trim().is_empty() {
        net::subscribe(&author);
    }
}

// -------- Модерация (владелец стены) --------

/// Скрыть комментарий на своей стене (+ рассылка подписанного события).
#[tauri::command]
pub(crate) async fn hide_comment(comment_id: String, target: String) -> Result<(), String> {
    let owner = my_address();
    if owner.is_empty() {
        return Err("locked".to_string());
    }
    let owner2 = owner.clone();
    let ev = tauri::async_runtime::spawn_blocking(move || -> Option<Envelope> {
        let _ = DB.insert(
            format!("social:hidden:{}:{}", owner2, comment_id).as_bytes(),
            b"1",
        );
        let _ = DB.flush();
        let ts = now_ms();
        let canonical = format!("mod|hide|{}|{}|{}|{}", owner2, target, comment_id, ts);
        let (pk, sig) = sign(&canonical).ok()?;
        Some(Envelope::Moderate {
            post_author: owner2,
            action: "hide".to_string(),
            target,
            comment_id,
            ts,
            pubkey: pk,
            sig,
        })
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Some(ev) = ev {
        net::broadcast(&owner, &ev);
    }
    Ok(())
}

/// Забанить пользователя на своей стене (его комменты не принимаются/скрываются).
#[tauri::command]
pub(crate) async fn ban_commenter(target: String) -> Result<(), String> {
    let owner = my_address();
    if owner.is_empty() {
        return Err("locked".to_string());
    }
    let owner2 = owner.clone();
    let ev = tauri::async_runtime::spawn_blocking(move || -> Option<Envelope> {
        let _ = DB.insert(
            format!("social:block:{}:{}", owner2, target).as_bytes(),
            b"1",
        );
        let _ = DB.flush();
        let ts = now_ms();
        let canonical = format!("mod|ban|{}|{}||{}", owner2, target, ts);
        let (pk, sig) = sign(&canonical).ok()?;
        Some(Envelope::Moderate {
            post_author: owner2,
            action: "ban".to_string(),
            target,
            comment_id: String::new(),
            ts,
            pubkey: pk,
            sig,
        })
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Some(ev) = ev {
        net::broadcast(&owner, &ev);
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn unban_commenter(target: String) -> Result<(), String> {
    let owner = my_address();
    if owner.is_empty() {
        return Err("locked".to_string());
    }
    let _ = DB.remove(format!("social:block:{}:{}", owner, target).as_bytes());
    let _ = DB.flush();
    Ok(())
}

#[tauri::command]
pub(crate) async fn list_blocked() -> Vec<String> {
    let owner = my_address();
    tauri::async_runtime::spawn_blocking(move || {
        let prefix = format!("social:block:{}:", owner);
        DB.scan_prefix(prefix.as_bytes())
            .filter_map(|kv| kv.ok())
            .filter_map(|(k, _)| {
                String::from_utf8(k.to_vec())
                    .ok()
                    .map(|s| s.trim_start_matches(&prefix).to_string())
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

// -------- Непрочитанные записи на отслеживаемых стенах --------

/// Отметить стену прочитанной (запоминаем ts последней просмотренной записи).
#[tauri::command]
pub(crate) async fn mark_wall_read(author: String) {
    let latest = wall_posts(&author, 1).first().map(|p| p.ts).unwrap_or_else(now_ms);
    let _ = DB.insert(format!("social:read:{}", author).as_bytes(), latest.to_string().as_bytes());
    let _ = DB.flush();
}

/// Число непрочитанных записей по каждой отслеживаемой стене (author → count).
#[tauri::command]
pub(crate) async fn wall_unread_counts() -> std::collections::HashMap<String, usize> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut out = std::collections::HashMap::new();
        for kv in DB.scan_prefix(b"social:follow:").filter_map(|k| k.ok()) {
            let author = match String::from_utf8(kv.0.to_vec()) {
                Ok(s) => s.trim_start_matches("social:follow:").to_string(),
                Err(_) => continue,
            };
            if author.is_empty() {
                continue;
            }
            let last_read: i64 = DB
                .get(format!("social:read:{}", author).as_bytes())
                .ok()
                .flatten()
                .and_then(|v| String::from_utf8(v.to_vec()).ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let count = wall_posts(&author, 100)
                .iter()
                .filter(|p| p.ts > last_read)
                .count();
            if count > 0 {
                out.insert(author, count);
            }
        }
        out
    })
    .await
    .unwrap_or_default()
}


// ============================ Приём событий из сети ============================

/// Общий приёмник событий (вызывается gossip-движком после снятия padding).
/// `topic_author` — владелец топика (стены), с которого пришёл пакет.
pub(crate) fn ingest(app: &AppHandle, topic_author: &str, json: &str) {
    let ev: Envelope = match serde_json::from_str(json) {
        Ok(e) => e,
        Err(_) => return,
    };
    let me = my_address();
    match ev {
        Envelope::Post(p) => {
            // автор поста обязан быть владельцем топика
            if p.author != topic_author {
                return;
            }
            if !verify(&post_canonical(&p), &p.pubkey, &p.sig, &p.author) {
                return;
            }
            if is_deleted(&p.author, &p.id) {
                return;
            }
            set_author_avatar(&p.author, &p.avatar, &p.node);
            if seen(&p.id) {
                return;
            }
            store_post(&p);
            emit(app, "post", &p.author);
        }
        Envelope::Comment(c) => {
            if c.post_author != topic_author {
                return;
            }
            if !verify(&comment_canonical(&c), &c.pubkey, &c.sig, &c.author) {
                return;
            }
            // владелец стены отклоняет комменты забаненных
            if is_blocked_by_owner(&c.post_author, &c.author) {
                return;
            }
            if seen(&c.id) {
                return;
            }
            store_comment(&c);
            emit(app, "comment", &c.post_author);
        }
        Envelope::DeletePost {
            id,
            author,
            ts,
            pubkey,
            sig,
        } => {
            if author != topic_author {
                return;
            }
            let canonical = format!("delpost|{}|{}|{}", id, author, ts);
            if !verify(&canonical, &pubkey, &sig, &author) {
                return;
            }
            let prefix = format!("social:post:{}:", author);
            for kv in DB.scan_prefix(prefix.as_bytes()).filter_map(|k| k.ok()) {
                if let Ok(p) = serde_json::from_slice::<Post>(&kv.1) {
                    if p.id == id {
                        for f in &p.files {
                            purge_local_file(app, &f.cid);
                        }
                        for cid in &p.images {
                            purge_wall_image(app, cid);
                        }
                        let _ = DB.remove(&kv.0);
                    }
                }
            }
            mark_deleted(&author, &id);
            let _ = DB.flush();
            emit(app, "delete", &author);
        }
        Envelope::Moderate {
            post_author,
            action,
            target,
            comment_id,
            ts,
            pubkey,
            sig,
        } => {
            if post_author != topic_author {
                return;
            }
            let canonical = match action.as_str() {
                "hide" => format!("mod|hide|{}|{}|{}|{}", post_author, target, comment_id, ts),
                "ban" => format!("mod|ban|{}|{}||{}", post_author, target, ts),
                _ => return,
            };
            if !verify(&canonical, &pubkey, &sig, &post_author) {
                return;
            }
            if action == "hide" {
                let _ = DB.insert(
                    format!("social:hidden:{}:{}", post_author, comment_id).as_bytes(),
                    b"1",
                );
            } else if action == "ban" {
                let _ = DB.insert(
                    format!("social:block:{}:{}", post_author, target).as_bytes(),
                    b"1",
                );
            }
            let _ = DB.flush();
            emit(app, "moderate", &post_author);
        }
        Envelope::Snapshot { author, cid, node } => {
            if author != topic_author || cid.trim().is_empty() {
                return;
            }
            if seen(&format!("snap:{}", cid)) {
                return;
            }
            let _ = DB.insert(format!("social:snap:{}", author).as_bytes(), cid.as_bytes());
            let _ = DB.flush();
            net::fetch_snapshot(app.clone(), author, cid, node);
        }
        Envelope::ReqSnapshot { author } => {
            // отвечаем только за СВОЮ стену
            if author == me && author == topic_author {
                net::rebuild_snapshot(app.clone());
            }
        }
        Envelope::React(r) => {
            if r.post_author != topic_author {
                return;
            }
            if !verify(&react_canonical(&r), &r.pubkey, &r.sig, &r.reactor) {
                return;
            }
            // порядок: игнорируем устаревшие обновления реакции
            if let Some(ex) = get_reaction(&r.post_author, &r.post_id, &r.reactor) {
                if ex.ts >= r.ts {
                    return;
                }
            }
            store_reaction(&r);
            emit(app, "react", &r.post_author);
        }
    }
}

fn emit(app: &AppHandle, kind: &str, author: &str) {
    let _ = app.emit(
        "social-event",
        serde_json::json!({ "kind": kind, "author": author }),
    );
}

// ============================ Снапшот-модель ============================

#[derive(Serialize, Deserialize, Clone)]
struct Snapshot {
    author: String,
    avatar: String,
    node: String,
    posts: Vec<Post>,
    comments: Vec<Comment>,
    #[serde(default)]
    reactions: Vec<Reaction>,
    #[serde(default)]
    albums: Vec<Album>,
    ts: i64,
}

// ============================ Envelope (gossip) ============================

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "t")]
pub(crate) enum Envelope {
    Post(Post),
    Comment(Comment),
    DeletePost {
        id: String,
        author: String,
        ts: i64,
        pubkey: String,
        sig: String,
    },
    Moderate {
        post_author: String,
        action: String,
        target: String,
        comment_id: String,
        ts: i64,
        pubkey: String,
        sig: String,
    },
    Snapshot {
        author: String,
        cid: String,
        node: String,
    },
    ReqSnapshot {
        author: String,
    },
    React(Reaction),
}

// ============================ Сетевой движок (gossip) ============================

#[cfg(feature = "p2p")]
pub(crate) mod net {
    use super::{ingest, my_address, user_root, wall_posts, post_comments, albums_of, store_album, album_canonical, verify, Comment, Snapshot, Envelope, cache_dir};
    use crate::DB;
    use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
    use iroh_gossip::{Gossip, TopicId};
    use once_cell::sync::Lazy;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tauri::AppHandle;

    const MAX_PACKET: usize = 60 * 1024;

    static GOSSIP: Lazy<Mutex<Option<Gossip>>> = Lazy::new(|| Mutex::new(None));
    static APP: Lazy<Mutex<Option<AppHandle>>> = Lazy::new(|| Mutex::new(None));
    // author -> sender (живая подписка на топик стены автора)
    static SENDERS: Lazy<Mutex<HashMap<String, GossipSender>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    fn topic_of(author: &str) -> TopicId {
        let mut h = blake3::Hasher::new();
        h.update(b"smartnet-wall:v1:");
        h.update(author.as_bytes());
        TopicId::from_bytes(*h.finalize().as_bytes())
    }

    /// Установка live-handle gossip (вызывается при поднятии endpoint).
    pub(crate) fn install(app: AppHandle, gossip: Gossip) {
        *APP.lock().unwrap() = Some(app.clone());
        *GOSSIP.lock().unwrap() = Some(gossip);
        // подписка на собственную стену + все отслеживаемые стены
        ensure_own(app.clone());
        for kv in DB.scan_prefix(b"social:follow:").filter_map(|k| k.ok()) {
            if let Ok(s) = String::from_utf8(kv.0.to_vec()) {
                let author = s.trim_start_matches("social:follow:").to_string();
                if !author.is_empty() {
                    subscribe(&author);
                }
            }
        }
        // пересобрать снапшот своей стены (сид блоба истории для новых подписчиков)
        rebuild_snapshot(app);
    }

    pub(crate) fn app_handle() -> Option<AppHandle> {
        APP.lock().unwrap().clone()
    }

    /// Подписаться на собственную стену (чтобы принимать комментарии/запросы).
    pub(crate) fn ensure_own(_app: AppHandle) {
        let me = my_address();
        if !me.is_empty() {
            subscribe(&me);
        }
    }

    /// Подписаться на топик стены автора; bootstrap — по опубликованному EndpointId.
    pub(crate) fn subscribe(author: &str) {
        let gossip = match GOSSIP.lock().unwrap().clone() {
            Some(g) => g,
            None => return,
        };
        if SENDERS.lock().unwrap().contains_key(author) {
            return;
        }
        let me = my_address();
        let bootstrap: Vec<iroh::EndpointId> = crate::chat::published_endpoint(author)
            .and_then(|s| s.parse::<iroh::EndpointId>().ok())
            .into_iter()
            .collect();
        let topic = topic_of(author);
        let author_s = author.to_string();
        tauri::async_runtime::spawn(async move {
            let sub = match gossip.subscribe(topic, bootstrap).await {
                Ok(s) => s,
                Err(_) => return,
            };
            let (tx, rx) = sub.split();
            SENDERS.lock().unwrap().insert(author_s.clone(), tx);
            // просим у владельца снапшот истории (если это не наша стена)
            if author_s != me {
                broadcast(&author_s, &Envelope::ReqSnapshot { author: author_s.clone() });
            }
            pump(author_s, rx).await;
        });
    }

    pub(crate) fn unsubscribe(author: &str) {
        SENDERS.lock().unwrap().remove(author);
    }

    async fn pump(author: String, mut rx: GossipReceiver) {
        use n0_future::StreamExt;
        let me = my_address();
        while let Some(ev) = rx.next().await {
            match ev {
                Ok(Event::Received(m)) => {
                    if let crate::stealth::Incoming::Message(bytes) =
                        crate::stealth::parse_incoming(&m.content)
                    {
                        let json = String::from_utf8_lossy(&bytes).to_string();
                        let a = author.clone();
                        tauri::async_runtime::spawn_blocking(move || {
                            if let Some(app) = app_handle() {
                                ingest(&app, &a, &json);
                            }
                        });
                    }
                }
                Ok(Event::NeighborUp(_)) => {
                    // сосед на топике поднялся → теперь запрос истории точно дойдёт
                    if author != me {
                        broadcast(&author, &Envelope::ReqSnapshot { author: author.clone() });
                    }
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        SENDERS.lock().unwrap().remove(&author);
    }

    /// Разослать событие в топик стены `author` (≤60КБ, с padding анти-DPI).
    pub(crate) fn broadcast(author: &str, ev: &Envelope) {
        let json = match serde_json::to_string(ev) {
            Ok(j) => j,
            Err(_) => return,
        };
        if json.len() > MAX_PACKET {
            return;
        }
        subscribe(author);
        crate::stealth::note_activity();
        let payload = crate::stealth::frame_outbound(json.into_bytes());
        let author_s = author.to_string();
        tauri::async_runtime::spawn(async move {
            // небольшая задержка, чтобы подписка успела установиться
            let sender = SENDERS.lock().unwrap().get(&author_s).cloned();
            let tx = match sender {
                Some(t) => t,
                None => {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    match SENDERS.lock().unwrap().get(&author_s).cloned() {
                        Some(t) => t,
                        None => return,
                    }
                }
            };
            let _ = tx.broadcast(payload.into()).await;
        });
    }

    /// Пересобрать снапшот собственной стены, засидить блоб и анонсировать CID.
    pub(crate) fn rebuild_snapshot(app: AppHandle) {
        tauri::async_runtime::spawn(async move {
            let me = my_address();
            if me.is_empty() {
                return;
            }
            let node = crate::p2p::node_id(&app).await.unwrap_or_default();
            let avatar = super::my_avatar_hash().await.unwrap_or_default();
            let app2 = app.clone();
            let me2 = me.clone();
            let build = tauri::async_runtime::spawn_blocking(move || -> Result<(std::path::PathBuf, String), String> {
                let posts = wall_posts(&me2, 100);
                let mut comments: Vec<Comment> = Vec::new();
                let mut reactions: Vec<super::Reaction> = Vec::new();
                for p in &posts {
                    comments.extend(post_comments(&me2, &p.id));
                    for r in super::reactions_of(&me2, &p.id) {
                        if !r.emoji.is_empty() {
                            reactions.push(r);
                        }
                    }
                }
                let snap = Snapshot {
                    author: me2.clone(),
                    avatar,
                    node,
                    posts,
                    comments,
                    reactions,
                    albums: albums_of(&me2),
                    ts: super::now_ms(),
                };
                let bytes = serde_json::to_vec(&snap).map_err(|e| e.to_string())?;
                let dest = user_root(&app2)?.join("wall").join("snapshot.json");
                std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
                Ok((dest, String::new()))
            })
            .await;
            let dest = match build {
                Ok(Ok((d, _))) => d,
                _ => return,
            };
            if let Some(cid) = crate::p2p::seed_file(&app, &dest.to_string_lossy()).await {
                let _ = DB.insert(format!("social:snap:{}", me).as_bytes(), cid.as_bytes());
                let _ = DB.flush();
                let node2 = crate::p2p::node_id(&app).await.unwrap_or_default();
                broadcast(&me, &Envelope::Snapshot { author: me.clone(), cid, node: node2 });
            }
        });
    }

    /// Скачать снапшот стены автора по CID, проверить подписи, влить историю в Sled.
    pub(crate) fn fetch_snapshot(app: AppHandle, author: String, cid: String, node: String) {
        tauri::async_runtime::spawn(async move {
            let dest = cache_dir(&app).join(format!("{}.json", cid));
            if crate::p2p::fetch_file(&app, node.trim(), cid.trim(), &dest.to_string_lossy(), &[])
                .await
                .is_err()
            {
                return;
            }
            let app2 = app.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || {
                let bytes = match std::fs::read(&dest) {
                    Ok(b) => b,
                    Err(_) => return,
                };
                let snap: Snapshot = match serde_json::from_slice(&bytes) {
                    Ok(s) => s,
                    Err(_) => return,
                };
                if snap.author != author {
                    return;
                }
                super::set_author_avatar(&author, &snap.avatar, &snap.node);
                let mut changed = false;
                for p in snap.posts {
                    if p.author != author || super::is_deleted(&p.author, &p.id) {
                        continue;
                    }
                    if super::verify(&super::post_canonical(&p), &p.pubkey, &p.sig, &p.author) {
                        let k = super::post_key(&p.author, p.ts, &p.id);
                        if DB.get(k.as_bytes()).ok().flatten().is_none() {
                            super::store_post(&p);
                            changed = true;
                        }
                    }
                }
                for c in snap.comments {
                    if c.post_author != author {
                        continue;
                    }
                    if super::verify(&super::comment_canonical(&c), &c.pubkey, &c.sig, &c.author) {
                        let k = super::comment_key(&c.post_author, &c.post_id, c.ts, &c.id);
                        if DB.get(k.as_bytes()).ok().flatten().is_none() {
                            super::store_comment(&c);
                            changed = true;
                        }
                    }
                }
                for r in snap.reactions {
                    if r.post_author != author || r.emoji.is_empty() {
                        continue;
                    }
                    if super::verify(&super::react_canonical(&r), &r.pubkey, &r.sig, &r.reactor) {
                        let newer = super::get_reaction(&r.post_author, &r.post_id, &r.reactor)
                            .map(|ex| r.ts > ex.ts)
                            .unwrap_or(true);
                        if newer {
                            super::store_reaction(&r);
                            changed = true;
                        }
                    }
                }
                for a in snap.albums {
                    if a.owner != author {
                        continue;
                    }
                    if super::find_album(&a.id).is_none()
                        && verify(&album_canonical(&a), &a.pubkey, &a.sig, &a.owner)
                    {
                        store_album(&a);
                        changed = true;
                    }
                }
                if changed {
                    super::emit(&app2, "snapshot", &author);
                }
            })
            .await;
        });
    }
}

// ---- Заглушки соц-сети для сборки без P2P (десктоп собирается с --features p2p) ----
#[cfg(not(feature = "p2p"))]
pub(crate) mod net {
    use super::Envelope;
    use tauri::AppHandle;
    pub(crate) fn install(_app: AppHandle, _g: ()) {}
    pub(crate) fn ensure_own(_app: AppHandle) {}
    pub(crate) fn subscribe(_author: &str) {}
    pub(crate) fn unsubscribe(_author: &str) {}
    pub(crate) fn broadcast(_author: &str, _ev: &Envelope) {}
    pub(crate) fn rebuild_snapshot(_app: AppHandle) {}
    pub(crate) fn fetch_snapshot(_app: AppHandle, _a: String, _c: String, _n: String) {}
}
