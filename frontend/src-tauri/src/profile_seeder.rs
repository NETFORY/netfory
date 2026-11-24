//! Public Profile Seeder — репликация профилей и микроблогов в облако.
//!
//! Тот же движок Cloud Seeder (`ntfry::seeder`) + `/ntfryseed/users/` для
//! публикации:
//!   • свой профиль + свои 25 последних постов + комментарии к ним
//!   • топ-10 профилей, на которых я подписан (по 10 постов у каждого)
//!   • прикреплённые к постам файлы (`PostFile.cid` → BLAKE3-контент-адрес,
//!     дедуп бесплатно через sha256 pre-check)
//!
//! Раскладка:
//!   /ntfryseed/users/<userId>.enc         — bundle (JSON) с профилем и постами
//!   /ntfryseed/users/<userId>/files/<cid>.enc — прикреплённые файлы (иммутабельные)
//!   /ntfryseed/users/manifest.enc          — индекс {userId → hash, updated, files}
//!
//! Изменения:
//!   • bundle публикуется, если его sha256 изменился с прошлой репликации
//!   • файл заливается только если его CID новый (sha256 pre-check у Яндекса
//!     пропустит идентичное содержимое)
//!   • при удалении поста осиротевшие файлы удаляются с облака
//!
//! Приватность (выбор пользователя из design-фазы):
//!   • публикуем ТОЛЬКО ProfilePublic — bio/reward/domains/updatedAt
//!   • ProfilePrivate (encrypted_secret) НИКОГДА не покидает клиент
//!   • пропускаем удалённые посты (`social::is_deleted`) и скрытые комментарии

use ntfry::seeder::{self, CloudSeederUri, SeederProvider};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use tauri::{AppHandle, Emitter};

use crate::social::{Post, PostFile, Comment};

// ── Sled-ключи ─────────────────────────────────────────────────────────────

/// Тумблер репликации профилей (0/1, default 0 — opt-in).
const AUTOREP_KEY: &[u8] = b"cfg:profile_seeder_autorep";
/// ms последней успешной репликации.
const LAST_KEY: &[u8] = b"cfg:profile_seeder_last";
/// Карта {userId → SnapshotEntry} — что уже опубликовано, для дельты.
const STATE_KEY: &[u8] = b"cfg:profile_seeder_state";

// ── Публичные структуры ────────────────────────────────────────────────────

/// Публичная часть профиля из profile.rs (репликуем только это).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProfilePublicSlim {
    pub identity: String,
    pub reward_address: String,
    pub is_public: bool,
    #[serde(default)]
    pub show_domains: bool,
    pub updated_at: String,
    /// зарегистрированное имя u:// (если есть) — берём из naming
    #[serde(default)]
    pub name: Option<String>,
    /// CID аватара + node (для показа читателем без блокчейн-запроса)
    #[serde(default)]
    pub avatar_cid: String,
    #[serde(default)]
    pub avatar_node: String,
}

/// Один пост + его комментарии (уже прошедшие фильтр hidden).
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct PostWithComments {
    pub post: Post,
    pub comments: Vec<Comment>,
}

/// Полный «bundle» пользователя. Один такой bundle публикуется как
/// `/ntfryseed/users/<userId>.enc` (seal_bytes → ChaCha20+ECDSA).
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct UserBundle {
    pub v: u8,
    pub user_id: String,
    pub profile: ProfilePublicSlim,
    pub posts: Vec<PostWithComments>,
    /// список CID всех прикреплённых файлов из этого bundle
    /// (используется читателем и delta-логикой для трекинга сирот)
    pub files: Vec<PostFileEntry>,
    /// ms момента сборки — читатель видит «свежесть» без разбора постов
    pub updated: i64,
}

/// Копия PostFile для манифеста (без дублирования Post-структуры).
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct PostFileEntry {
    pub cid: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
}

/// Одна запись в manifest.enc корня /ntfryseed/users/.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct ManifestEntry {
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// sha256(bundle-json без hash-поля) — читатель сравнивает с локальным кэшем
    pub hash: String,
    pub updated: i64,
    pub posts: usize,
    pub files: Vec<String>, // CID-ы
}

/// Индекс всех опубликованных профилей + версия автора-сидера.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct ProfilesManifest {
    pub v: u8,
    pub updated: i64,
    pub users: Vec<ManifestEntry>,
}

// ── Sled snapshot: что уже лежит в облаке ─────────────────────────────────

/// Что мы уже опубликовали в облако для этого userId (для дельта-репликации).
#[derive(Serialize, Deserialize, Clone, Default)]
pub(crate) struct SnapshotEntry {
    pub hash: String,
    pub files: Vec<String>, // CID-ы файлов, уже залитых
    pub updated: i64,
}

/// Ключ снапшота — раздельно на каждый диск (primary/secondary-зеркало).
fn state_key(prov: SeederProvider, secondary: bool) -> Vec<u8> {
    format!("cfg:profile_seeder_state:{}{}", prov.code(), if secondary { ":2" } else { "" })
        .into_bytes()
}

fn load_state(prov: SeederProvider, secondary: bool) -> HashMap<String, SnapshotEntry> {
    if let Some(v) = crate::DB.get(state_key(prov, secondary)).ok().flatten() {
        return serde_json::from_slice(&v).unwrap_or_default();
    }
    // Миграция: старый единый снапшот принадлежит primary-диску own-URI.
    if !secondary {
        let legacy_prov = crate::cloudseeder::own_seeder_uri()
            .and_then(|u| CloudSeederUri::parse(&u).ok())
            .map(|p| p.provider);
        if legacy_prov == Some(prov) {
            if let Some(v) = crate::DB.get(STATE_KEY).ok().flatten() {
                return serde_json::from_slice(&v).unwrap_or_default();
            }
        }
    }
    HashMap::new()
}

fn save_state(prov: SeederProvider, secondary: bool, map: &HashMap<String, SnapshotEntry>) {
    if let Ok(v) = serde_json::to_vec(map) {
        let _ = crate::DB.insert(state_key(prov, secondary), v);
    }
}

// ── Тумблер репликации ────────────────────────────────────────────────────

#[tauri::command]
pub(crate) fn profile_seeder_autorep_get() -> bool {
    matches!(crate::DB.get(AUTOREP_KEY), Ok(Some(v)) if v.as_ref() == b"1")
}

#[tauri::command]
pub(crate) fn profile_seeder_autorep_set(on: bool) {
    let _ = crate::DB.insert(AUTOREP_KEY, if on { b"1".as_ref() } else { b"0".as_ref() });
}

#[tauri::command]
pub(crate) fn profile_seeder_last_replicated() -> i64 {
    crate::DB
        .get(LAST_KEY)
        .ok()
        .flatten()
        .and_then(|v| std::str::from_utf8(&v).ok().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

fn set_last_replicated(ms: i64) {
    let _ = crate::DB.insert(LAST_KEY, ms.to_string().as_bytes());
}

// ── Live-прогресс публикации ──────────────────────────────────────────────

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProfileProgress {
    phase: String, // "collect" | "user" | "file" | "manifest" | "done"
    index: usize,
    total: usize,
    user_id: String,
    file_cid: String,
    size_bytes: u64,
    uploaded_bytes: u64,
    kbps: u64,
    attempt: u32,
    status: String, // "packing" | "uploading" | "skipped" | "done" | "error"
    error: Option<String>,
}

fn emit_progress(app: &AppHandle, p: &ProfileProgress) {
    let _ = app.emit("profile-publish-progress", p.clone());
}

// ── Сбор bundle-а для одного userId ───────────────────────────────────────

fn my_address() -> String {
    crate::load_vault_internal().map(|v| v.address).unwrap_or_default()
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// avatarCid для пользователя из sled (`social:avcid:<addr>` = "<cid>\t<node>").
fn author_avatar(addr: &str) -> (String, String) {
    if let Some(v) = crate::DB.get(format!("social:avcid:{}", addr).as_bytes()).ok().flatten() {
        if let Ok(s) = std::str::from_utf8(&v) {
            let mut it = s.splitn(2, '\t');
            let cid = it.next().unwrap_or("").to_string();
            let node = it.next().unwrap_or("").to_string();
            return (cid, node);
        }
    }
    (String::new(), String::new())
}

/// Собрать bundle с постами и комментариями для одного адреса.
/// Возвращает None, если у пользователя нет ни одного поста и он не мы.
fn build_user_bundle(app: &AppHandle, user_id: &str, post_limit: usize) -> Option<UserBundle> {
    let posts = crate::social::wall_posts(user_id, post_limit * 3); // берём с запасом
    // Отфильтровать удалённые + обрезать до лимита
    let alive: Vec<Post> = posts
        .into_iter()
        .filter(|p| !crate::social::is_deleted(&p.author, &p.id))
        .take(post_limit)
        .collect();

    let is_me = user_id == my_address();
    if alive.is_empty() && !is_me {
        return None;
    }

    // Профиль. Для «меня» — читаем profile_manifest.json; для чужих — минимум.
    let (identity, reward, is_public, show_domains, updated_at) = if is_me {
        let path = crate::ensure_data_dir(app).join("profile_manifest.json");
        let m: serde_json::Value = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(serde_json::json!({}));
        let pub_ = m.get("public").cloned().unwrap_or_default();
        (
            pub_.get("identity").and_then(|v| v.as_str()).unwrap_or(user_id).to_string(),
            pub_.get("rewardAddress").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            pub_.get("isPublic").and_then(|v| v.as_bool()).unwrap_or(true),
            pub_.get("showDomains").and_then(|v| v.as_bool()).unwrap_or(false),
            pub_.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        )
    } else {
        (user_id.to_string(), String::new(), true, false, String::new())
    };

    let name = crate::naming::username_of_address(user_id.to_string());
    let (avatar_cid, avatar_node) = author_avatar(user_id);

    let profile = ProfilePublicSlim {
        identity,
        reward_address: reward,
        is_public,
        show_domains,
        updated_at,
        name,
        avatar_cid,
        avatar_node,
    };

    // Комментарии + PostFile-массив (для дельты)
    let mut posts_with: Vec<PostWithComments> = Vec::with_capacity(alive.len());
    let mut file_index: HashMap<String, PostFileEntry> = HashMap::new();
    for p in alive {
        let comments = crate::social::post_comments(&p.author, &p.id);
        for f in &p.files {
            file_index.entry(f.cid.clone()).or_insert(PostFileEntry {
                cid: f.cid.clone(),
                name: f.name.clone(),
                size: f.size,
                mime: f.mime.clone(),
            });
        }
        posts_with.push(PostWithComments { post: p, comments });
    }

    let files: Vec<PostFileEntry> = file_index.into_values().collect();

    Some(UserBundle {
        v: 1,
        user_id: user_id.to_string(),
        profile,
        posts: posts_with,
        files,
        updated: now_ms(),
    })
}

fn hash_bundle(b: &UserBundle) -> String {
    // Стабильная сериализация: serde_json пишет поля в порядке декларации структуры.
    // Для устойчивости к обновлениям — исключаем `updated` (меняется всегда):
    #[derive(Serialize)]
    struct Hashable<'a> {
        v: u8,
        user_id: &'a str,
        profile: &'a ProfilePublicSlim,
        posts: &'a [PostWithComments],
        files: &'a [PostFileEntry],
    }
    let h = Hashable {
        v: b.v,
        user_id: &b.user_id,
        profile: &b.profile,
        posts: &b.posts,
        files: &b.files,
    };
    let bytes = serde_json::to_vec(&h).unwrap_or_default();
    hex::encode(Sha256::digest(&bytes))
}

// ── Ключ подписи (тот же, что у обычного сидера) ─────────────────────────

fn signing_key() -> Result<([u8; 32], [u8; 33]), String> {
    let secret = crate::SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("кошелёк заблокирован — разблокируйте SmartNet")?;
    ntfry::crypto::derive_address_keypair(&secret).map_err(|e| e.to_string())
}

// ── Provider-агностичный upload/delete ───────────────────────────────────

async fn upload_subpath(
    provider: SeederProvider,
    token: &str,
    subpath: &str,
    data: Vec<u8>,
    on_progress: Option<Box<dyn Fn(u64, u32) + Send + Sync + 'static>>,
) -> Result<bool, String> {
    match provider {
        SeederProvider::Yandex => seeder::yandex_upload_subpath(token, subpath, data, on_progress)
            .await
            .map_err(|e| e.to_string()),
        SeederProvider::Mailru => seeder::mailru_upload_subpath(token, subpath, data, on_progress)
            .await
            .map_err(|e| e.to_string()),
        SeederProvider::Google => seeder::gdrive_upload_subpath(token, subpath, data, on_progress)
            .await
            .map_err(|e| e.to_string()),
    }
}

async fn delete_subpath(
    provider: SeederProvider,
    token: &str,
    subpath: &str,
) -> Result<(), String> {
    let r = match provider {
        SeederProvider::Yandex => seeder::yandex_delete_subpath(token, subpath).await,
        SeederProvider::Mailru => seeder::mailru_delete_subpath(token, subpath).await,
        SeederProvider::Google => seeder::gdrive_delete_subpath(token, subpath).await,
    };
    match r {
        Ok(()) | Err(ntfry::error::StorageError::NotFound(_)) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

// ── Публикация ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesPublishOut {
    /// свой Cloud Seeder URI (тот же, что у публикации сети — реюз)
    pub uri: String,
    pub users_published: usize,
    pub users_skipped: usize,
    pub files_uploaded: usize,
    pub files_skipped: usize,
    pub orphans_deleted: usize,
    pub updated: i64,
}

/// Основной entry-point: собирает bundle-ы, публикует delta, обновляет manifest.
/// Не валится, если пользователь не публиковал сеть ранее — тогда URI будет
/// собран из своего pubkey + текущего folder_token (не реюзается, но
/// корректен только для только-что-опубликованных облаков; для реалистичного
/// сценария рекомендуется сначала опубликовать сеть кнопкой Publish).
#[tauri::command]
pub(crate) async fn cloud_seeder_publish_profiles(
    app: AppHandle,
    token: String,
    provider: Option<String>,
    mailru_weblink: Option<String>,
    secondary: Option<bool>,
) -> Result<ProfilesPublishOut, String> {
    let prov = crate::cloudseeder::parse_provider(provider.as_deref())?;
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
    let my = my_address();
    if my.is_empty() {
        return Err("кошелёк не разблокирован (нет адреса)".into());
    }

    emit_progress(&app, &ProfileProgress {
        phase: "collect".into(), index: 0, total: 0, user_id: String::new(),
        file_cid: String::new(), size_bytes: 0, uploaded_bytes: 0, kbps: 0, attempt: 1,
        status: "packing".into(), error: None,
    });

    // 1. Собрать список: свой + топ-10 подписок (в порядке following_list из subs.rs)
    let following = crate::subs::following_addresses(my.clone());
    let mut targets: Vec<(String, usize)> = vec![(my.clone(), 25)];
    for addr in following.into_iter().take(10) {
        if addr != my {
            targets.push((addr, 10));
        }
    }

    // 2. Построить bundle-ы + вычислить hash для дельты
    let mut bundles: Vec<UserBundle> = Vec::new();
    for (uid, limit) in &targets {
        if let Some(b) = build_user_bundle(&app, uid, *limit) {
            bundles.push(b);
        }
    }

    // 3. Дельта: что реально нужно заливать
    let mut state = load_state(prov, secondary);
    let mut users_published = 0usize;
    let mut users_skipped = 0usize;
    let mut files_uploaded = 0usize;
    let mut files_skipped = 0usize;
    let mut orphans_deleted = 0usize;
    let mut all_files_new: HashSet<String> = HashSet::new();

    let total = bundles.len();
    for (i, bundle) in bundles.iter().enumerate() {
        let uid = bundle.user_id.clone();
        let new_hash = hash_bundle(bundle);
        let prev = state.get(&uid).cloned().unwrap_or_default();

        // 3a. Файлы: для каждого CID из bundle — если не публиковался ранее ИЛИ
        // не помечен в prev.files — грузим. sha256 pre-check внутри resilient_put
        // при неизменившемся содержимом пропустит заливку без байтов трафика.
        let bundle_cids: HashSet<String> =
            bundle.files.iter().map(|f| f.cid.clone()).collect();
        let prev_cids: HashSet<String> = prev.files.iter().cloned().collect();
        let new_cids: Vec<&PostFileEntry> = bundle
            .files
            .iter()
            .filter(|f| !prev_cids.contains(&f.cid))
            .collect();

        for pf in new_cids {
            let src = crate::files::file_store_path(&app, &pf.cid);
            if !src.is_file() {
                // файл не найден локально (например, стёрт из iroh-blobs) — пропускаем
                continue;
            }
            let bytes = match std::fs::read(&src) {
                Ok(b) => b,
                Err(_) => continue,
            };
            // seal_bytes — детерминирован; sha256 pre-check у Яндекса пропустит,
            // если такой же контент уже лежит у кого-то другого в облаке.
            let sealed = match ntfry::seeder::seal_bytes(&bytes, &sk) {
                Ok(s) => s,
                Err(e) => {
                    crate::syslog::log(
                        "profile-seeder",
                        format!("seal_bytes({}): {}", pf.cid, e),
                    );
                    continue;
                }
            };
            let size = sealed.len() as u64;
            let subpath = format!("users/{}/files/{}.enc", uid, pf.cid);
            let mut prog = ProfileProgress {
                phase: "file".into(),
                index: i + 1,
                total,
                user_id: uid.clone(),
                file_cid: pf.cid.clone(),
                size_bytes: size,
                uploaded_bytes: 0,
                kbps: 0,
                attempt: 1,
                status: "uploading".into(),
                error: None,
            };
            emit_progress(&app, &prog);
            let started = std::time::Instant::now();
            let on_progress: Option<Box<dyn Fn(u64, u32) + Send + Sync>> = {
                use std::sync::atomic::{AtomicU64, Ordering};
                let app2 = app.clone();
                let tpl = prog.clone();
                let last_ms = std::sync::Arc::new(AtomicU64::new(0));
                Some(Box::new(move |sent, attempt| {
                    let ms = started.elapsed().as_millis() as u64;
                    let prev = last_ms.load(Ordering::Relaxed);
                    if ms.saturating_sub(prev) < 450 && sent < tpl.size_bytes {
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
            match upload_subpath(prov, token.trim(), &subpath, sealed, on_progress).await {
                Ok(true) => {
                    files_uploaded += 1;
                    prog.status = "done".into();
                    prog.uploaded_bytes = size;
                    emit_progress(&app, &prog);
                }
                Ok(false) => {
                    files_skipped += 1;
                    prog.status = "skipped".into();
                    emit_progress(&app, &prog);
                }
                Err(e) => {
                    prog.status = "error".into();
                    prog.error = Some(e.clone());
                    emit_progress(&app, &prog);
                    crate::syslog::log(
                        "profile-seeder",
                        format!("file {}: upload failed — {}", pf.cid, e),
                    );
                }
            }
        }
        // Пропущенные (уже были в prev.files) — счётчик
        files_skipped += prev_cids.intersection(&bundle_cids).count();
        all_files_new.extend(bundle_cids.iter().cloned());

        // 3b. Осиротевшие файлы: prev.files, которых больше нет в bundle → DELETE
        let orphans: Vec<String> = prev
            .files
            .iter()
            .filter(|c| !bundle_cids.contains(*c))
            .cloned()
            .collect();
        for cid in &orphans {
            let subpath = format!("users/{}/files/{}.enc", uid, cid);
            match delete_subpath(prov, token.trim(), &subpath).await {
                Ok(()) => orphans_deleted += 1,
                Err(e) => crate::syslog::log(
                    "profile-seeder",
                    format!("deleting orphan {}: {}", cid, e),
                ),
            }
        }

        // 3c. Bundle: заливаем, только если hash изменился
        if prev.hash == new_hash && !prev.hash.is_empty() {
            users_skipped += 1;
            let prog = ProfileProgress {
                phase: "user".into(),
                index: i + 1,
                total,
                user_id: uid.clone(),
                file_cid: String::new(),
                size_bytes: 0,
                uploaded_bytes: 0,
                kbps: 0,
                attempt: 1,
                status: "skipped".into(),
                error: None,
            };
            emit_progress(&app, &prog);
            // Обновим state (files могли поменяться после дельты)
            state.insert(
                uid.clone(),
                SnapshotEntry {
                    hash: new_hash,
                    files: bundle_cids.into_iter().collect(),
                    updated: now_ms(),
                },
            );
            continue;
        }

        let bundle_bytes = serde_json::to_vec(bundle).map_err(|e| e.to_string())?;
        let sealed = ntfry::seeder::seal_bytes(&bundle_bytes, &sk).map_err(|e| e.to_string())?;
        let subpath = format!("users/{}.enc", uid);
        let mut prog = ProfileProgress {
            phase: "user".into(),
            index: i + 1,
            total,
            user_id: uid.clone(),
            file_cid: String::new(),
            size_bytes: sealed.len() as u64,
            uploaded_bytes: 0,
            kbps: 0,
            attempt: 1,
            status: "uploading".into(),
            error: None,
        };
        emit_progress(&app, &prog);
        match upload_subpath(prov, token.trim(), &subpath, sealed, None).await {
            Ok(_) => {
                users_published += 1;
                prog.status = "done".into();
                emit_progress(&app, &prog);
            }
            Err(e) => {
                prog.status = "error".into();
                prog.error = Some(e.clone());
                emit_progress(&app, &prog);
                crate::syslog::log(
                    "profile-seeder",
                    format!("bundle {}: upload failed — {}", uid, e),
                );
                continue;
            }
        }
        state.insert(
            uid.clone(),
            SnapshotEntry {
                hash: new_hash,
                files: bundle_cids.into_iter().collect(),
                updated: now_ms(),
            },
        );
    }

    // 4. Manifest: индекс всех опубликованных профилей (перезаписываем всегда)
    let manifest = ProfilesManifest {
        v: 1,
        updated: now_ms(),
        users: bundles
            .iter()
            .map(|b| ManifestEntry {
                user_id: b.user_id.clone(),
                name: b.profile.name.clone(),
                hash: state.get(&b.user_id).map(|s| s.hash.clone()).unwrap_or_default(),
                updated: b.updated,
                posts: b.posts.len(),
                files: b.files.iter().map(|f| f.cid.clone()).collect(),
            })
            .collect(),
    };
    let manifest_bytes = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    let sealed_manifest =
        ntfry::seeder::seal_bytes(&manifest_bytes, &sk).map_err(|e| e.to_string())?;
    let mut mprog = ProfileProgress {
        phase: "manifest".into(),
        index: bundles.len(),
        total: bundles.len(),
        user_id: String::new(),
        file_cid: String::new(),
        size_bytes: sealed_manifest.len() as u64,
        uploaded_bytes: 0,
        kbps: 0,
        attempt: 1,
        status: "uploading".into(),
        error: None,
    };
    emit_progress(&app, &mprog);
    if let Err(e) =
        upload_subpath(prov, token.trim(), "users/manifest.enc", sealed_manifest, None).await
    {
        mprog.status = "error".into();
        mprog.error = Some(e.clone());
        emit_progress(&app, &mprog);
        crate::syslog::log("profile-seeder", format!("manifest: upload failed — {}", e));
    } else {
        mprog.status = "done".into();
        emit_progress(&app, &mprog);
    }

    save_state(prov, secondary, &state);
    set_last_replicated(now_ms());

    // 5. URI: тот же, что у уже опубликованного своего сидера (реюз folder_token);
    // для secondary-зеркала — зеркальный слот.
    let own = if secondary {
        crate::cloudseeder::own_seeder_uri2()
    } else {
        crate::cloudseeder::own_seeder_uri()
    };
    let uri = match own {
        Some(u) => u,
        None => {
            // Если пользователь не публиковал сеть — сообщим об этом,
            // но не рушим публикацию: bundle-ы уже лежат в облаке.
            // URI строим по mailru weblink (если задан) или пустой folder_token.
            let folder_token = mailru_weblink.clone().unwrap_or_default();
            CloudSeederUri {
                provider: prov,
                folder_token,
                public_key: pk_hex.clone(),
            }
            .to_uri()
        }
    };

    emit_progress(&app, &ProfileProgress {
        phase: "done".into(), index: bundles.len(), total: bundles.len(),
        user_id: String::new(), file_cid: String::new(),
        size_bytes: 0, uploaded_bytes: 0, kbps: 0, attempt: 1,
        status: "done".into(), error: None,
    });
    crate::syslog::log(
        "profile-seeder",
        format!(
            "publish profiles: {} new bundles, {} skipped, {} new files, {} orphans removed",
            users_published, users_skipped, files_uploaded, orphans_deleted
        ),
    );
    Ok(ProfilesPublishOut {
        uri,
        users_published,
        users_skipped,
        files_uploaded,
        files_skipped,
        orphans_deleted,
        updated: now_ms(),
    })
}

// ── Чтение чужого сидера ──────────────────────────────────────────────────

async fn fetch_public(
    parsed: &CloudSeederUri,
    subpath: &str,
) -> Result<Vec<u8>, ntfry::error::StorageError> {
    match parsed.provider {
        SeederProvider::Yandex => seeder::yandex_fetch_public(&parsed.folder_token, subpath).await,
        SeederProvider::Mailru => seeder::mailru_fetch_public(&parsed.folder_token, subpath).await,
        SeederProvider::Google => seeder::gdrive_fetch_public(&parsed.folder_token, subpath).await,
    }
}

/// Читатель: получить список опубликованных профилей у сидера.
#[tauri::command]
pub(crate) async fn cloud_seeder_fetch_profiles_manifest(
    uri: String,
) -> Result<ProfilesManifest, String> {
    let parsed = CloudSeederUri::parse(&uri).map_err(|e| e.to_string())?;
    let blob = fetch_public(&parsed, "users/manifest.enc").await.map_err(|e| e.to_string())?;
    let pk = parsed.public_key.clone();
    let plain = tauri::async_runtime::spawn_blocking(move || ntfry::seeder::open_bytes(&blob, &pk))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    serde_json::from_slice::<ProfilesManifest>(&plain).map_err(|e| e.to_string())
}

/// Читатель: получить bundle одного профиля.
#[tauri::command]
pub(crate) async fn cloud_seeder_fetch_profile(
    uri: String,
    user_id: String,
) -> Result<UserBundle, String> {
    let parsed = CloudSeederUri::parse(&uri).map_err(|e| e.to_string())?;
    let sub = format!("users/{}.enc", user_id.trim());
    let blob = fetch_public(&parsed, &sub).await.map_err(|e| e.to_string())?;
    let pk = parsed.public_key.clone();
    let plain = tauri::async_runtime::spawn_blocking(move || ntfry::seeder::open_bytes(&blob, &pk))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    serde_json::from_slice::<UserBundle>(&plain).map_err(|e| e.to_string())
}

/// Читатель: получить один прикреплённый файл (сырые байты plain-контента).
/// Байты возвращаются как base64 — Tauri IPC JSON не любит бинарь.
#[tauri::command]
pub(crate) async fn cloud_seeder_fetch_profile_file(
    uri: String,
    user_id: String,
    cid: String,
) -> Result<String, String> {
    let parsed = CloudSeederUri::parse(&uri).map_err(|e| e.to_string())?;
    let sub = format!("users/{}/files/{}.enc", user_id.trim(), cid.trim());
    let blob = fetch_public(&parsed, &sub).await.map_err(|e| e.to_string())?;
    let pk = parsed.public_key.clone();
    let plain = tauri::async_runtime::spawn_blocking(move || ntfry::seeder::open_bytes(&blob, &pk))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(plain))
}

// ── Фоновая репликация: раз в час, если хоть один bundle изменился ────────

/// Дешёвая проверка: изменился ли какой-нибудь bundle с прошлой репликации?
/// Если ничего не поменялось — вызывающая сторона отменяет цикл (0 запросов
/// к облаку).
#[tauri::command]
pub(crate) fn profile_seeder_has_changes(app: AppHandle) -> bool {
    let my = my_address();
    if my.is_empty() {
        return false;
    }
    let prov = crate::cloudseeder::own_seeder_uri()
        .and_then(|u| CloudSeederUri::parse(&u).ok())
        .map(|p| p.provider)
        .unwrap_or(SeederProvider::Yandex);
    let state = load_state(prov, false);
    let following = crate::subs::following_addresses(my.clone());
    let mut targets: Vec<(String, usize)> = vec![(my.clone(), 25)];
    for addr in following.into_iter().take(10) {
        if addr != my {
            targets.push((addr, 10));
        }
    }
    for (uid, limit) in &targets {
        let Some(b) = build_user_bundle(&app, uid, *limit) else { continue };
        let h = hash_bundle(&b);
        match state.get(uid) {
            Some(prev) if prev.hash == h => continue,
            _ => return true,
        }
    }
    false
}
