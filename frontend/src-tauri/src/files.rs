// ============================================================
//  SmartNet // files.rs  — Decentralized File Exchange (v2.0, Этап 1)
//
//  Local file registry + hierarchical content store + `f://` access scheme.
//    - Storage: data/filesdata/<XX>/<CID>  (XX = first 2 chars of the CID)
//    - CID    : real IPFS CIDv0 (base58btc of multihash 0x12,0x20 || sha256)
//    - Index  : Sled  `files:cid:<cid>`  -> FileManifest JSON
//               Sled  `user:files:<addr>` -> Vec<cid> JSON
//    - Transit: iroh-blobs (seed/announce) — wired via crate::p2p (best-effort)
//
//  On-chain publish (Type 5 IPFS tx) + paid-access verification land in Этап 2/3.
// ============================================================

use crate::{dir_size, ensure_data_dir, DB, SESSION_SEED};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::Emitter;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_shell::ShellExt;

const FILE_PREFIX: &str = "files:cid:";
const USER_FILES_PREFIX: &str = "user:files:";
const SEEDS_PREFIX: &str = "file:seeds:";

/// Dedicated on-chain registry for PUBLIC/paid file publications (memo Type 0
/// transfers). The indexer parses ONLY this address. (Entity-based publishing is
/// kept "behind a flag" and will replace this once `aip36` activates on-chain.)
const FILE_REGISTRY: &str = "SfiLejYCAfrx9ahEfyMzpLeGTBa6Qzd3Mm";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FileManifest {
    pub cid: String,
    pub name: String,
    pub size: u64,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub owner: String,
    /// price in whole STH (0 when free)
    pub price: f64,
    #[serde(rename = "isPaid")]
    pub is_paid: bool,
    /// unix seconds
    pub created: u64,
    /// node is actively seeding this CID over iroh
    pub seeding: bool,
    /// iroh-blobs content hash (transit address), set after a successful seed
    #[serde(default)]
    pub blob: Option<String>,
    /// iroh endpoint id of a known seeder (so downloaders can fetch the blob)
    #[serde(default, rename = "seederNode")]
    pub seeder_node: Option<String>,
    /// stamped on-chain via a public registry memo transaction
    #[serde(default)]
    pub published: bool,
    /// the registry (memo) transaction id, once published
    #[serde(default)]
    pub txid: Option<String>,
    /// discovered from the chain registry (not stored locally — file unavailable)
    #[serde(default)]
    pub remote: bool,
    /// IPFS CIDv0 — kept ONLY as proof of ownership / for future IPFS-network
    /// interop. NOT used for P2P transit or as the local identifier.
    #[serde(default, rename = "ipfsCid")]
    pub ipfs_cid: Option<String>,
    /// transient live seeder count — populated at list/meta time, NOT
    /// authoritative in the DB (we never rely on a persisted value).
    #[serde(default)]
    pub seeds: usize,
    /// unix seconds we finished downloading this file locally (downloaded files).
    #[serde(default, rename = "downloadedAt")]
    pub downloaded_at: Option<u64>,
    /// unix seconds we published this file on-chain (owned published files).
    #[serde(default, rename = "publishedAt")]
    pub published_at: Option<u64>,
    /// true while a background P2P download for this CID is in progress.
    #[serde(default)]
    pub downloading: bool,
    /// true when the stored bytes are AES-256-GCM encrypted (E2EE private file).
    #[serde(default)]
    pub encrypted: bool,
    /// ECIES-wrapped AES-256 file key per authorized address (owner + whitelist).
    /// value = hex(ephPub33 ‖ nonce12 ‖ wrappedKey‖tag). Empty for public files.
    #[serde(default, rename = "wrappedKeys")]
    pub wrapped_keys: HashMap<String, String>,
    /// SmartHoldem addresses allowed to decrypt (owner + recipients).
    #[serde(default)]
    pub recipients: Vec<String>,
    /// Real (decrypted) filename for a private file — local-only, never transmitted.
    /// Set for the owner at creation and for a recipient after a successful download.
    #[serde(default, rename = "displayName", skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Машиночитаемая категоризация файла в хранилище — JSON-строка формата
    /// `{"cat":"dev","repo":"...","ver":"...","os":"win"}` для релизных
    /// бинарников из DevHub, `{"cat":"music"}` для музыки, `{"cat":"image"}`
    /// для обложек и т.д. Пустая строка = «other» (файл загружен вручную,
    /// не привязан к сервису). Поле opaque для Rust'а — парсим только на
    /// фронте, чтобы можно было добавлять новые категории без изменения
    /// схемы. Задаётся при `add_file`, наследуется полностью.
    #[serde(default)]
    pub tag: String,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn my_address() -> String {
    crate::load_vault_internal()
        .map(|v| v.address)
        .unwrap_or_default()
}

/// IPFS CIDv0: base58btc( 0x12 0x20 || sha256(bytes) ). 46-char "Qm…" string.
/// Kept ONLY as proof of ownership / for future IPFS-network interop.
fn compute_cidv0(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut mh = Vec::with_capacity(34);
    mh.push(0x12); // sha2-256 multihash code
    mh.push(0x20); // length = 32
    mh.extend_from_slice(&digest);
    bs58::encode(mh).into_string()
}

/// Native content identifier: Blake3 hex (matches the app/merkle hashing used
/// everywhere else in SmartNet and the iroh-blobs transit hash). This is the
/// `cid` used for storage, `f://`, `sn://file/` and P2P transit.
fn compute_blake3(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Absolute on-disk path for a CID: data/filesdata/<XX>/<CID>.
pub(crate) fn file_store_path(app: &tauri::AppHandle, cid: &str) -> PathBuf {
    let shard = &cid[..cid.len().min(2)];
    ensure_data_dir(app)
        .join("filesdata")
        .join(shard)
        .join(cid)
}

fn read_user_index(addr: &str) -> Vec<String> {
    DB.get(format!("{}{}", USER_FILES_PREFIX, addr).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Vec<String>>(&v).ok())
        .unwrap_or_default()
}

fn write_user_index(addr: &str, list: &[String]) {
    if let Ok(json) = serde_json::to_vec(list) {
        let _ = DB.insert(format!("{}{}", USER_FILES_PREFIX, addr).as_bytes(), json);
    }
}

pub(crate) fn persist_manifest(m: &FileManifest) -> Result<(), String> {
    let json = serde_json::to_vec(m).map_err(|e| e.to_string())?;
    DB.insert(format!("{}{}", FILE_PREFIX, m.cid).as_bytes(), json)
        .map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(())
}

/// Полностью удалить файл: с диска (filesdata/<XX>/<CID>), из манифеста Sled и из
/// пользовательского индекса. Синхронный best-effort helper для соц-слоя.
pub(crate) fn purge_file(app: &tauri::AppHandle, cid: &str) {
    let _ = std::fs::remove_file(file_store_path(app, cid));
    if let Some(m) = load_manifest(cid) {
        let mut idx = read_user_index(&m.owner);
        idx.retain(|c| c != cid);
        write_user_index(&m.owner, &idx);
    }
    let _ = DB.remove(format!("{}{}", FILE_PREFIX, cid).as_bytes());
    let _ = DB.flush();
}

fn load_manifest(cid: &str) -> Option<FileManifest> {
    DB.get(format!("{}{}", FILE_PREFIX, cid).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<FileManifest>(&v).ok())
}

// ============================================================
//  Commands
// ============================================================

/// Native single-file picker. Returns the absolute path, or None if cancelled.
#[tauri::command(async)]
pub(crate) fn pick_file(app: tauri::AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .blocking_pick_file()
        .and_then(|fp| fp.into_path().ok())
        .map(|p| p.to_string_lossy().to_string())
}

/// Add a local file to the exchange: hash → CIDv0 → copy into filesdata/<XX>/ →
/// register in Sled (manifest + per-user index) → seed over iroh (best-effort).
///
/// `tag` — необязательная JSON-строка категоризации (`{"cat":"dev","repo":"...","ver":"...","os":"..."}`);
/// пустая строка = «other» на фронте. Полностью opaque для Rust'а.
#[tauri::command]
pub(crate) async fn add_file(
    app: tauri::AppHandle,
    src_path: String,
    price: f64,
    is_paid: bool,
    recipients: Vec<String>,
    #[allow(non_snake_case)] tag: Option<String>,
) -> Result<FileManifest, String> {
    // Private (E2EE) mode: resolve every recipient's on-chain secp256k1 public
    // key (+ the owner's own key) so the symmetric file key can be ECIES-wrapped
    // for each of them. A recipient with no on-chain public key (never sent a tx)
    // blocks the upload with a clear error. Pubkeys are fetched in the backend via
    // the existing SmartHoldem node pool (ureq/sth_node) — no CSP/CORS issues.
    let mut pubkeys: HashMap<String, String> = HashMap::new();
    if !recipients.is_empty() {
        let owner_secret = {
            let g = SESSION_SEED.lock().unwrap();
            g.as_ref().ok_or("locked")?.clone()
        };
        let owner_pk = crate::crypto::keys_from_material(&owner_secret)?.1;
        let owner_addr = my_address();
        if owner_addr.is_empty() {
            return Err("locked".to_string());
        }
        pubkeys.insert(owner_addr.clone(), owner_pk);

        let recs: Vec<String> = recipients
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && *s != owner_addr)
            .collect();
        let resolved = tauri::async_runtime::spawn_blocking(move || {
            recs.into_iter()
                .map(|a| {
                    let r = crate::sth_node::fetch_wallet_pubkey(&a);
                    (a, r)
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| e.to_string())?;
        for (addr, res) in resolved {
            match res {
                Ok(Some(pk)) => {
                    pubkeys.insert(addr, pk);
                }
                Ok(None) => {
                    return Err(format!(
                        "Адрес {} ещё не активен в блокчейне (нет публичного ключа). Получатель должен совершить хотя бы одну исходящую транзакцию.",
                        addr
                    ));
                }
                Err(e) => {
                    return Err(format!("Не удалось получить ключ адреса {}: {}", addr, e));
                }
            }
        }
    }

    let app2 = app.clone();
    let recipients_list: Vec<String> = pubkeys.keys().cloned().collect();
    let tag_str = tag.unwrap_or_default();
    let mut m = tauri::async_runtime::spawn_blocking(move || {
        add_file_blocking(app, src_path, price, is_paid, pubkeys, recipients_list, tag_str)
    })
    .await
    .map_err(|e| e.to_string())??;

    // Best-effort iroh seed: store the (encrypted, for private files) blob +
    // protect it from GC; persist hash and advertise this node as a seeder.
    let path = file_store_path(&app2, &m.cid);
    if let Some(hash) = crate::p2p::seed_file(&app2, &path.to_string_lossy()).await {
        m.blob = Some(hash);
        if let Ok(node) = crate::p2p::node_id(&app2).await {
            if !node.is_empty() {
                m.seeder_node = Some(node.clone());
                add_seeder(&m.cid, &node);
            }
        }
        let _ = persist_manifest(&m);
    }
    Ok(m)
}

fn seeds_key(cid: &str) -> Vec<u8> {
    format!("{}{}", SEEDS_PREFIX, cid).into_bytes()
}

/// Record a node id as a seeder of `cid` (dedup). Drives the seed counter shown
/// on the download page.
fn add_seeder(cid: &str, node: &str) {
    if node.is_empty() {
        return;
    }
    let mut set: Vec<String> = DB
        .get(seeds_key(cid))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();
    if !set.iter().any(|n| n == node) {
        set.push(node.to_string());
        if let Ok(j) = serde_json::to_vec(&set) {
            let _ = DB.insert(seeds_key(cid), j);
        }
    }
}

fn seed_count(cid: &str) -> usize {
    DB.get(seeds_key(cid))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Vec<String>>(&v).ok())
        .map(|s| s.len())
        .unwrap_or(0)
}

/// All known seeder EndpointIds for `cid` — passed to the P2P downloader so it can
/// race every provider (DHT-resolved) instead of a single fixed `seeder_node`.
fn file_seeders(cid: &str) -> Vec<String> {
    DB.get(seeds_key(cid))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Vec<String>>(&v).ok())
        .unwrap_or_default()
}

fn add_file_blocking(
    app: tauri::AppHandle,
    src_path: String,
    price: f64,
    is_paid: bool,
    pubkeys: HashMap<String, String>,
    recipients: Vec<String>,
    tag: String,
) -> Result<FileManifest, String> {
    let src = PathBuf::from(src_path.trim());
    if !src.is_file() {
        return Err("Файл не найден".to_string());
    }
    let owner = my_address();
    if owner.is_empty() {
        return Err("locked".to_string());
    }
    let orig_name = src
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let total = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
    let private = !pubkeys.is_empty();

    let manifest = if private {
        // --- E2EE private file: read fully (progress events), wrap the real name
        // in an encrypted container, AES-256-GCM seal, ECIES-wrap the key per
        // recipient. Seeders/network only ever see the opaque ciphertext. ---
        let bytes = read_with_progress(&app, &src, total)?;
        let orig_mime = crate::protocol::mime_for(&orig_name).to_string();
        let fk = crate::crypto::gen_file_key();
        // Seal the metadata (real name/mime) in its own small AES-GCM section so a
        // recipient can cheaply recover the display name right after a download,
        // without decrypting the whole payload.
        let meta_json = serde_json::json!({ "name": orig_name, "mime": orig_mime }).to_string();
        let meta_enc = crate::crypto::aes_gcm_encrypt(&fk, meta_json.as_bytes())?;
        let content_enc = crate::crypto::aes_gcm_encrypt(&fk, &bytes)?;
        let mut wrapped_keys: HashMap<String, String> = HashMap::new();
        for (addr, pk) in &pubkeys {
            wrapped_keys.insert(addr.clone(), crate::crypto::ecies_wrap_key(pk, &fk)?);
        }
        // The transit blob carries the (individually ECIES-sealed) wrapped keys
        // so a recipient can self-serve their key after a P2P download — the
        // sf:<cid>~<node> notification only needs to reference the blob.
        let blob_bytes = build_private_blob(&wrapped_keys, &meta_enc, &content_enc)?;
        let cid = compute_blake3(&blob_bytes);
        let ipfs_cid = compute_cidv0(&blob_bytes);
        let dest = file_store_path(&app, &cid);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if !dest.exists() {
            std::fs::write(&dest, &blob_bytes).map_err(|e| e.to_string())?;
        }
        let _ = app.emit(
            "file-add-progress",
            serde_json::json!({ "done": total, "total": total }),
        );
        FileManifest {
            cid,
            name: "🔒 Приватный файл".to_string(),
            size: blob_bytes.len() as u64,
            mime_type: "application/octet-stream".to_string(),
            owner: owner.clone(),
            price: 0.0,
            is_paid: false,
            created: now_secs(),
            seeding: true,
            blob: None,
            seeder_node: None,
            published: false,
            txid: None,
            remote: false,
            ipfs_cid: Some(ipfs_cid),
            seeds: 0,
            downloaded_at: None,
            published_at: None,
            downloading: false,
            encrypted: true,
            wrapped_keys,
            recipients,
            display_name: Some(orig_name.clone()),
            tag: tag.clone(),
        }
    } else {
        // --- Public/regular file: single-pass streaming hash + copy (never loads
        // the whole file into RAM; emits hashing progress for large files). ---
        let (cid, ipfs_cid, size) = hash_and_store_stream(&app, &src, total)?;
        let dest = file_store_path(&app, &cid);
        let paid = is_paid && price > 0.0;
        FileManifest {
            cid,
            name: orig_name,
            size,
            mime_type: crate::protocol::mime_for(&dest.to_string_lossy()).to_string(),
            owner: owner.clone(),
            price: if paid { price } else { 0.0 },
            is_paid: paid,
            created: now_secs(),
            seeding: true,
            blob: None,
            seeder_node: None,
            published: false,
            txid: None,
            remote: false,
            ipfs_cid: Some(ipfs_cid),
            seeds: 0,
            downloaded_at: None,
            published_at: None,
            downloading: false,
            encrypted: false,
            wrapped_keys: HashMap::new(),
            recipients: Vec::new(),
            display_name: None,
            tag: tag.clone(),
        }
    };
    persist_manifest(&manifest)?;

    // per-user index (dedupe)
    let mut idx = read_user_index(&owner);
    if !idx.contains(&manifest.cid) {
        idx.push(manifest.cid.clone());
        write_user_index(&owner, &idx);
    }
    Ok(manifest)
}

/// Emit hashing/read progress to the UI (throttled to whole-percent changes).
fn emit_progress(app: &tauri::AppHandle, done: u64, total: u64, last_pct: &mut i64) {
    let pct = if total > 0 {
        (done as i128 * 100 / total as i128) as i64
    } else {
        0
    };
    if pct != *last_pct {
        *last_pct = pct;
        let _ = app.emit(
            "file-add-progress",
            serde_json::json!({ "done": done, "total": total }),
        );
    }
}

/// Read a whole file into memory in chunks, emitting progress (private files).
fn read_with_progress(app: &tauri::AppHandle, src: &PathBuf, total: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(src).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(total as usize);
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    let mut done = 0u64;
    let mut last = -1i64;
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        done += n as u64;
        emit_progress(app, done, total, &mut last);
    }
    Ok(out)
}

/// Streaming Blake3 cid + IPFS CIDv0 hashing while copying into the content store
/// (single pass via a temp file → atomic rename), emitting progress for large
/// files. Returns (cid, ipfs_cid, size). Never loads the whole file into RAM.
fn hash_and_store_stream(
    app: &tauri::AppHandle,
    src: &PathBuf,
    total: u64,
) -> Result<(String, String, u64), String> {
    use std::io::{Read, Write};
    let files_root = ensure_data_dir(app).join("filesdata");
    std::fs::create_dir_all(&files_root).map_err(|e| e.to_string())?;
    let tmp = files_root.join(format!(".tmp-{}-{}", std::process::id(), now_secs()));
    let mut f = std::fs::File::open(src).map_err(|e| e.to_string())?;
    let mut out = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut b3 = blake3::Hasher::new();
    let mut sh = Sha256::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    let mut done = 0u64;
    let mut last = -1i64;
    loop {
        let n = match f.read(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e.to_string());
            }
        };
        if n == 0 {
            break;
        }
        b3.update(&buf[..n]);
        sh.update(&buf[..n]);
        if let Err(e) = out.write_all(&buf[..n]) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.to_string());
        }
        done += n as u64;
        emit_progress(app, done, total, &mut last);
    }
    drop(out);
    let cid = b3.finalize().to_hex().to_string();
    let digest = sh.finalize();
    let mut mh = Vec::with_capacity(34);
    mh.push(0x12);
    mh.push(0x20);
    mh.extend_from_slice(&digest);
    let ipfs_cid = bs58::encode(mh).into_string();
    let dest = file_store_path(app, &cid);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if dest.exists() {
        let _ = std::fs::remove_file(&tmp);
    } else if std::fs::rename(&tmp, &dest).is_err() {
        // cross-device fallback (temp on a different mount than the store)
        std::fs::copy(&tmp, &dest).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&tmp);
    }
    Ok((cid, ipfs_cid, done))
}

/// Decrypt a stored file's on-disk bytes if it is a private (E2EE) file. Returns
/// (plaintext, real_name, mime). For public files the bytes are returned as-is.
/// Errors if the current user's address is not among the authorized recipients.
fn decrypt_stored(m: &FileManifest, stored: Vec<u8>) -> Result<(Vec<u8>, String, String), String> {
    if !m.encrypted {
        return Ok((stored, m.name.clone(), m.mime_type.clone()));
    }
    let (keys, meta_enc, content_enc) = parse_private_blob(&stored)?;
    let me = my_address();
    let wrapped = keys
        .get(&me)
        .or_else(|| m.wrapped_keys.get(&me))
        .ok_or("нет доступа: файл зашифрован не для вашего адреса")?;
    let secret = {
        let g = SESSION_SEED.lock().unwrap();
        g.as_ref().ok_or("locked")?.clone()
    };
    let fk = crate::crypto::ecies_unwrap_key(&secret, wrapped)?;
    let (name, mime) = parse_meta(&crate::crypto::aes_gcm_decrypt(&fk, &meta_enc)?);
    let content = crate::crypto::aes_gcm_decrypt(&fk, &content_enc)?;
    Ok((content, name, mime))
}

/// Cheaply recover a private file's real name by decrypting ONLY the small meta
/// section from disk (no need to read/decrypt the whole payload). Returns None if
/// the file isn't a readable private blob for the current user.
fn decrypt_private_name(m: &FileManifest, path: &PathBuf) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).ok()?;
    if &magic != PRIVATE_MAGIC {
        return None;
    }
    let mut lb = [0u8; 4];
    f.read_exact(&mut lb).ok()?;
    let klen = u32::from_le_bytes(lb) as usize;
    if klen > 1_000_000 {
        return None;
    }
    let mut keys_buf = vec![0u8; klen];
    f.read_exact(&mut keys_buf).ok()?;
    let keys: HashMap<String, String> = serde_json::from_slice(&keys_buf).ok()?;
    let mut mlb = [0u8; 4];
    f.read_exact(&mut mlb).ok()?;
    let mlen = u32::from_le_bytes(mlb) as usize;
    if mlen > 1_000_000 {
        return None;
    }
    let mut meta_buf = vec![0u8; mlen];
    f.read_exact(&mut meta_buf).ok()?;
    let me = my_address();
    let wrapped = keys.get(&me).or_else(|| m.wrapped_keys.get(&me))?;
    let secret = {
        let g = SESSION_SEED.lock().unwrap();
        g.as_ref()?.clone()
    };
    let fk = crate::crypto::ecies_unwrap_key(&secret, wrapped).ok()?;
    let meta = crate::crypto::aes_gcm_decrypt(&fk, &meta_buf).ok()?;
    Some(parse_meta(&meta).0)
}

/// Cheaply recover the original plaintext size of a private file from its SFP2
/// header (no decryption): total − header(magic+lens+keys+meta) − GCM overhead
/// (12-byte nonce + 16-byte tag). Returns None if it isn't a private blob.
fn private_plain_size(path: &PathBuf) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let total = std::fs::metadata(path).ok()?.len();
    let mut f = std::fs::File::open(path).ok()?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).ok()?;
    if &magic != PRIVATE_MAGIC {
        return None;
    }
    let mut lb = [0u8; 4];
    f.read_exact(&mut lb).ok()?;
    let klen = u32::from_le_bytes(lb) as u64;
    f.seek(SeekFrom::Start(8 + klen)).ok()?;
    let mut mlb = [0u8; 4];
    f.read_exact(&mut mlb).ok()?;
    let mlen = u32::from_le_bytes(mlb) as u64;
    // magic(4) + keys_len(4) + keys + meta_len(4) + meta = start of content_enc
    let header = 4 + 4 + klen + 4 + mlen;
    if total < header + 28 {
        return None;
    }
    Some((total - header).saturating_sub(28)) // − GCM nonce(12) + tag(16)
}


fn parse_meta(meta_bytes: &[u8]) -> (String, String) {
    let v: serde_json::Value = serde_json::from_slice(meta_bytes).unwrap_or_default();
    let name = v["name"].as_str().unwrap_or("file").to_string();
    let mime = v["mime"]
        .as_str()
        .unwrap_or("application/octet-stream")
        .to_string();
    (name, mime)
}

const PRIVATE_MAGIC: &[u8; 4] = b"SFP2";

/// Assemble the private-file transit blob:
///   magic ‖ u32 keys_len ‖ keys_json ‖ u32 meta_len ‖ meta_enc ‖ content_enc
/// Wrapped keys are individually ECIES-sealed, so shipping them in the (public)
/// blob is safe and lets any recipient decrypt after a P2P pull. The metadata is
/// a separate AES-GCM section so the real name can be recovered cheaply.
fn build_private_blob(
    wrapped_keys: &HashMap<String, String>,
    meta_enc: &[u8],
    content_enc: &[u8],
) -> Result<Vec<u8>, String> {
    let keys_json = serde_json::to_vec(wrapped_keys).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(12 + keys_json.len() + meta_enc.len() + content_enc.len());
    out.extend_from_slice(PRIVATE_MAGIC);
    out.extend_from_slice(&(keys_json.len() as u32).to_le_bytes());
    out.extend_from_slice(&keys_json);
    out.extend_from_slice(&(meta_enc.len() as u32).to_le_bytes());
    out.extend_from_slice(meta_enc);
    out.extend_from_slice(content_enc);
    Ok(out)
}

/// Reverse of [`build_private_blob`] → (wrapped_keys, meta_enc, content_enc).
fn parse_private_blob(
    blob: &[u8],
) -> Result<(HashMap<String, String>, Vec<u8>, Vec<u8>), String> {
    if blob.len() < 8 || &blob[..4] != PRIVATE_MAGIC {
        return Err("это не приватный blob".to_string());
    }
    let klen = u32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]]) as usize;
    if blob.len() < 8 + klen + 4 {
        return Err("повреждённый приватный blob".to_string());
    }
    let keys: HashMap<String, String> =
        serde_json::from_slice(&blob[8..8 + klen]).map_err(|e| e.to_string())?;
    let mpos = 8 + klen;
    let mlen = u32::from_le_bytes([
        blob[mpos],
        blob[mpos + 1],
        blob[mpos + 2],
        blob[mpos + 3],
    ]) as usize;
    let mstart = mpos + 4;
    if blob.len() < mstart + mlen {
        return Err("повреждённый приватный blob".to_string());
    }
    let meta_enc = blob[mstart..mstart + mlen].to_vec();
    let content_enc = blob[mstart + mlen..].to_vec();
    Ok((keys, meta_enc, content_enc))
}

/// Files the user owns OR has downloaded (present on local disk) — both appear
/// in "My Files". Owned files come from the per-user index; downloaded files are
/// detected by a present local blob.
#[tauri::command]
pub(crate) async fn list_my_files(app: tauri::AppHandle) -> Vec<FileManifest> {
    tokio::task::spawn_blocking(move || {
        let owner = my_address();
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<FileManifest> = Vec::new();
        for cid in read_user_index(&owner) {
            if let Some(mut m) = load_manifest(&cid) {
                m.seeds = seed_count(&m.cid);
                seen.insert(m.cid.clone());
                out.push(m);
            }
        }
        // downloaded (non-owned) files we hold locally
        for item in DB.scan_prefix(FILE_PREFIX.as_bytes()) {
            if let Ok((_, v)) = item {
                if let Ok(mut m) = serde_json::from_slice::<FileManifest>(&v) {
                    if !seen.contains(&m.cid)
                        && (file_store_path(&app, &m.cid).is_file() || m.downloading)
                    {
                        m.seeds = seed_count(&m.cid);
                        out.push(m);
                    }
                }
            }
        }
        out
    })
    .await
    .unwrap_or_default()
}

/// All files known to this node (Этап 1: locally registered; chain-indexed later).
#[tauri::command]
pub(crate) async fn list_exchange_files() -> Vec<FileManifest> {
    tokio::task::spawn_blocking(|| {
        let mut out = Vec::new();
        for item in DB.scan_prefix(FILE_PREFIX.as_bytes()) {
            if let Ok((_, v)) = item {
                if let Ok(mut m) = serde_json::from_slice::<FileManifest>(&v) {
                    m.seeds = seed_count(&m.cid);
                    out.push(m);
                }
            }
        }
        out
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
pub(crate) fn file_meta(cid: String) -> Option<FileManifest> {
    load_manifest(&cid).map(|mut m| {
        m.seeds = seed_count(&m.cid);
        m
    })
}

/// Toggle seeding for a CID (owner only). Stops/keeps protecting the blob from GC.
#[tauri::command]
pub(crate) async fn set_file_seeding(
    app: tauri::AppHandle,
    cid: String,
    on: bool,
) -> Result<FileManifest, String> {
    let mut m = load_manifest(&cid).ok_or("not found")?;
    if m.owner != my_address() {
        return Err("not owner".to_string());
    }
    m.seeding = on;
    persist_manifest(&m)?;
    if on {
        let path = file_store_path(&app, &cid);
        if let Some(hash) = crate::p2p::seed_file(&app, &path.to_string_lossy()).await {
            m.blob = Some(hash);
            persist_manifest(&m)?;
        }
    } else {
        // refresh the keep-set from remaining seeded files → next GC reclaims it
        crate::p2p::reclaim_after_delete(&app).await;
    }
    Ok(m)
}

/// Delete a file: remove from disk + Sled (manifest + user index), stop seeding.
#[tauri::command]
pub(crate) async fn delete_file(app: tauri::AppHandle, cid: String) -> Result<(), String> {
    let m = load_manifest(&cid).ok_or("not found")?;
    if m.owner != my_address() {
        return Err("not owner".to_string());
    }
    let path = file_store_path(&app, &cid);
    let _ = std::fs::remove_file(&path);
    let _ = DB.remove(format!("{}{}", FILE_PREFIX, cid).as_bytes());
    let mut idx = read_user_index(&m.owner);
    idx.retain(|c| c != &cid);
    write_user_index(&m.owner, &idx);
    let _ = DB.flush();
    // reclaim the now-orphaned content blob (dedup-safe, no-op if P2P off)
    crate::p2p::reclaim_after_delete(&app).await;
    Ok(())
}

#[derive(Debug, Serialize, Default)]
pub(crate) struct FileDiskStats {
    #[serde(rename = "totalBytes")]
    total_bytes: u64,
    #[serde(rename = "freeBytes")]
    free_bytes: u64,
    #[serde(rename = "usedBytes")]
    used_bytes: u64,
    /// bytes used specifically by the File Exchange store (data/filesdata)
    #[serde(rename = "filesBytes")]
    files_bytes: u64,
    #[serde(rename = "fileCount")]
    file_count: u64,
}

/// Disk usage for the "My Files" page (sysinfo + filesdata footprint).
/// Async + spawn_blocking: sysinfo disk enumeration and the recursive directory
/// walk must never run on the main thread (they can stall the UI, especially over
/// RDP). Polled by the UI, so it must always return quickly / non-blocking.
#[tauri::command]
pub(crate) async fn file_disk_stats(app: tauri::AppHandle) -> FileDiskStats {
    tokio::task::spawn_blocking(move || {
        let (total, avail) = crate::apps::data_disk_space(&app);
        let files_bytes = dir_size(&ensure_data_dir(&app).join("filesdata"));
        let file_count = DB.scan_prefix(FILE_PREFIX.as_bytes()).count() as u64;
        FileDiskStats {
            total_bytes: total,
            free_bytes: avail,
            used_bytes: total.saturating_sub(avail),
            files_bytes,
            file_count,
        }
    })
    .await
    .unwrap_or_default()
}

/// On-disk paths of every CID we currently seed — used by the iroh seed loop to
/// (re)protect file blobs from GC alongside dApp blobs on each discovery cycle.
pub(crate) fn seeding_file_paths(app: &tauri::AppHandle) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for item in DB.scan_prefix(FILE_PREFIX.as_bytes()) {
        if let Ok((_, v)) = item {
            if let Ok(m) = serde_json::from_slice::<FileManifest>(&v) {
                if m.seeding {
                    let p = file_store_path(app, &m.cid);
                    if p.is_file() {
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}

/// Resolve an `f://<cid>` request to (bytes, mime) for the custom URI scheme.
/// Этап 1 access policy: free files and the owner's own files are served; a paid
/// file owned by someone else returns None (payment verification = Этап 3).
///
/// CIDv0 is case-sensitive but URL parsers lower-case the authority, so an exact
/// miss falls back to a case-insensitive registry scan to recover the canonical
/// CID (lets `f://<cid>` keep working when the host got down-cased).
pub(crate) fn resolve_file_request(app: &tauri::AppHandle, cid: &str) -> Option<(Vec<u8>, String)> {
    let raw = cid.trim().trim_matches('/');
    if raw.is_empty() {
        return None;
    }
    let m = load_manifest(raw).or_else(|| find_manifest_ci(raw))?;
    let allowed = !m.is_paid || m.owner == my_address();
    if !allowed {
        return None;
    }
    let path = file_store_path(app, &m.cid);
    let stored = std::fs::read(&path).ok()?;
    // Private files are decrypted on access (only an authorized address succeeds).
    let (bytes, _name, mime) = decrypt_stored(&m, stored).ok()?;
    Some((bytes, mime))
}

/// Case-insensitive registry lookup (recovers a down-cased CIDv0 authority).
fn find_manifest_ci(cid: &str) -> Option<FileManifest> {
    for item in DB.scan_prefix(FILE_PREFIX.as_bytes()) {
        if let Ok((_, v)) = item {
            if let Ok(m) = serde_json::from_slice::<FileManifest>(&v) {
                if m.cid.eq_ignore_ascii_case(cid) {
                    return Some(m);
                }
            }
        }
    }
    None
}

// ============================================================
//  Public/Commercial mode — memo (Type 0) registry + chain indexer
//  (Entity-based publishing kept behind a flag for post-aip36 activation.)
// ============================================================

/// Compact registry memo (≤255B): identifies a published file for the indexer.
/// `{"t":"file","cid":..,"b":<irohHash>,"p":<priceSTH>,"n":<name>,"sz":<size>,"nd":<node>}`
fn build_registry_memo(m: &FileManifest) -> String {
    let mut name = m.name.clone();
    let make = |n: &str| {
        serde_json::json!({
            "t": "file", "cid": m.cid, "b": m.blob, "p": m.price,
            "n": n, "sz": m.size, "nd": m.seeder_node,
        })
        .to_string()
    };
    while make(&name).len() > 255 && !name.is_empty() {
        name.pop();
    }
    make(&name)
}

/// Publish a file to the PUBLIC marketplace: a memo Type 0 transfer to the file
/// registry address (owner only). Paid files can be bought by unlimited users.
#[tauri::command]
pub(crate) async fn publish_file(app: tauri::AppHandle, cid: String) -> Result<FileManifest, String> {
    let mut m = load_manifest(&cid).ok_or("not found")?;
    if m.owner != my_address() {
        return Err("not owner".to_string());
    }
    if m.published {
        return Ok(m);
    }
    // make sure we are seeding + advertised before announcing publicly
    if m.blob.is_none() {
        let path = file_store_path(&app, &cid);
        if let Some(hash) = crate::p2p::seed_file(&app, &path.to_string_lossy()).await {
            m.blob = Some(hash);
        }
    }
    if m.seeder_node.is_none() {
        if let Ok(node) = crate::p2p::node_id(&app).await {
            if !node.is_empty() {
                m.seeder_node = Some(node.clone());
                add_seeder(&cid, &node);
            }
        }
    }
    let secret = {
        let guard = SESSION_SEED.lock().unwrap();
        guard.as_ref().ok_or("locked")?.clone()
    };
    let memo = build_registry_memo(&m);
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        crate::sth_node::send_transfer(&secret, FILE_REGISTRY, "0.00000001", "", &memo)
    })
    .await
    .map_err(|e| e.to_string())??;

    if !outcome.accepted {
        return Err("Сеть отклонила публикацию (проверьте баланс)".to_string());
    }
    m.published = true;
    m.txid = Some(outcome.id);
    m.published_at = Some(now_secs());
    persist_manifest(&m)?;
    Ok(m)
}

/// Index a public file registration discovered in the registry's inbound memos,
/// WITHOUT clobbering a file we own/store locally.
fn index_registry_memo(sender: &str, memo: &str, ts_ms: u64) {
    let meta: serde_json::Value = match serde_json::from_str(memo) {
        Ok(v) => v,
        Err(_) => return,
    };
    if meta["t"].as_str() != Some("file") {
        return;
    }
    let cid = meta["cid"].as_str().unwrap_or("").to_string();
    if cid.is_empty() {
        return;
    }
    if let Some(existing) = load_manifest(&cid) {
        if !existing.remote {
            return; // keep local/owned entry authoritative
        }
    }
    let price = meta["p"].as_f64().unwrap_or(0.0);
    let blob = meta["b"].as_str().map(|s| s.to_string());
    let node = meta["nd"].as_str().map(|s| s.to_string());
    if let Some(n) = &node {
        add_seeder(&cid, n);
    }
    let m = FileManifest {
        cid: cid.clone(),
        name: meta["n"].as_str().filter(|s| !s.is_empty()).unwrap_or(&cid).to_string(),
        size: meta["sz"].as_u64().unwrap_or(0),
        mime_type: crate::protocol::mime_for(&cid).to_string(),
        owner: sender.to_string(),
        price: if price > 0.0 { price } else { 0.0 },
        is_paid: price > 0.0,
        created: ts_ms / 1000,
        seeding: false,
        blob,
        seeder_node: node,
        published: true,
        txid: None,
        remote: true,
        ipfs_cid: None,
        seeds: 0,
        downloaded_at: None,
        published_at: None,
        downloading: false,
        encrypted: false,
        wrapped_keys: HashMap::new(),
        recipients: Vec::new(),
        display_name: None,
        tag: String::new(),
    };
    let _ = persist_manifest(&m);
}

/// Register a file shared via a self-contained `sn://file/<cid>~<blob>~<node>`
/// link (Private/Anonymous P2P mode). Stores a remote manifest so the download
/// page can fetch the blob directly — no on-chain publication required.
#[tauri::command]
pub(crate) fn register_shared_file(
    cid: String,
    blob: String,
    node: String,
    name: String,
    size: u64,
    price: f64,
    owner: String,
) -> Result<FileManifest, String> {
    let cid = cid.trim().to_string();
    if cid.is_empty() {
        return Err("empty cid".to_string());
    }
    if let Some(existing) = load_manifest(&cid) {
        return Ok(existing); // already known (owned/downloaded/indexed)
    }
    let display = if name.trim().is_empty() { cid.clone() } else { name.trim().to_string() };
    let node = node.trim().to_string();
    let paid = price > 0.0;
    let m = FileManifest {
        cid: cid.clone(),
        mime_type: crate::protocol::mime_for(&display).to_string(),
        name: display,
        size,
        owner: owner.trim().to_string(),
        price: if paid { price } else { 0.0 },
        is_paid: paid,
        created: now_secs(),
        seeding: false,
        blob: if blob.trim().is_empty() { None } else { Some(blob.trim().to_string()) },
        seeder_node: if node.is_empty() { None } else { Some(node.clone()) },
        published: false,
        txid: None,
        remote: true,
        ipfs_cid: None,
        seeds: 0,
        downloaded_at: None,
        published_at: None,
        downloading: false,
        encrypted: false,
        wrapped_keys: HashMap::new(),
        recipients: Vec::new(),
        display_name: None,
        tag: String::new(),
    };
    if !node.is_empty() {
        add_seeder(&cid, &node);
    }
    persist_manifest(&m)?;
    Ok(m)
}

/// Background indexer: scan inbound memos on the FILE_REGISTRY address and
/// register public files for the marketplace.
pub(crate) fn spawn_indexer() {
    let _ = std::thread::Builder::new().name("files-indexer".into()).spawn(|| loop {
        // offline: короткая пауза вместо сетевого запроса, который повиснет
        if !crate::netwatch::is_online() {
            std::thread::sleep(std::time::Duration::from_secs(15));
            continue;
        }
        if let Ok(items) = crate::sth_node::fetch_history(FILE_REGISTRY, 100) {
            for it in items {
                if it.kind == "in" && !it.memo.is_empty() {
                    index_registry_memo(&it.counterparty, &it.memo, it.timestamp);
                }
            }
            let _ = DB.flush();
        }
        std::thread::sleep(std::time::Duration::from_secs(60));
    });
}

#[derive(Debug, Serialize)]
pub(crate) struct SeedInfo {
    seeds: usize,
    available: bool,
}

/// Seed count + local availability for the download page.
#[tauri::command]
pub(crate) fn file_seed_count(app: tauri::AppHandle, cid: String) -> SeedInfo {
    SeedInfo {
        seeds: seed_count(&cid),
        available: file_store_path(&app, &cid).is_file(),
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct DownloadProgress {
    /// bytes already written locally for this CID
    downloaded: u64,
    /// total file size in bytes (0 when unknown)
    total: u64,
    /// live seeder count
    seeds: usize,
    /// the blob is fully present on disk
    complete: bool,
}

/// Live download progress for the file download page: bytes transferred so far
/// (from the iroh blob store status), total size and current seeder count.
#[tauri::command]
pub(crate) async fn file_download_progress(app: tauri::AppHandle, cid: String) -> DownloadProgress {
    let m = load_manifest(&cid);
    let total = m.as_ref().map(|x| x.size).unwrap_or(0);
    let seeds = seed_count(&cid);
    // already exported to disk → 100%
    if file_store_path(&app, &cid).is_file() {
        return DownloadProgress { downloaded: total, total, seeds, complete: true };
    }
    let blob = m.and_then(|x| x.blob);
    let (mut downloaded, complete) = match blob {
        Some(b) => crate::p2p::blob_progress(&app, &b).await,
        None => (0, false),
    };
    if complete {
        downloaded = total;
    } else if total > 0 {
        downloaded = downloaded.min(total);
    }
    DownloadProgress { downloaded, total, seeds, complete }
}

/// Download a remote file's blob over iroh and store it locally (it then appears
/// in "My Files"). No-op if already local. Tauri-native (desktop only).
#[tauri::command]
pub(crate) async fn download_file(app: tauri::AppHandle, cid: String) -> Result<FileManifest, String> {
    let mut m = load_manifest(&cid).ok_or("not found")?;
    let dest = file_store_path(&app, &cid);
    if dest.is_file() {
        return Ok(m); // already have it
    }
    // paid files require a confirmed purchase (owner is exempt)
    if m.is_paid && m.owner != my_address() && !has_paid(&m) {
        return Err("payment required".to_string());
    }
    let blob = m.blob.clone().ok_or("no transit hash for this file")?;
    let node = m.seeder_node.clone().ok_or("no known seeder")?;
    crate::p2p::fetch_file(&app, &node, &blob, &dest.to_string_lossy(), &file_seeders(&cid)).await?;
    m.seeding = true;
    m.downloaded_at = Some(now_secs());
    if m.encrypted {
        m.display_name = decrypt_private_name(&m, &dest);
        if let Some(sz) = private_plain_size(&dest) {
            m.size = sz;
        }
    }
    if let Ok(my_node) = crate::p2p::node_id(&app).await {
        if !my_node.is_empty() {
            add_seeder(&cid, &my_node);
            // we now hold + seed this file → our re-share links should point to us
            m.seeder_node = Some(my_node);
        }
    }
    persist_manifest(&m)?;
    Ok(m)
}

/// Background downloads in progress this session (dedup guard).
static ACTIVE_DL: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Start (or resume) a P2P download in the BACKGROUND: records `downloading` in
/// the DB immediately and spawns the iroh fetch so it keeps going regardless of
/// which page the user is on. Returns at once with `downloading = true`. The file
/// then appears in "My Files" → «Скачиваются» until it completes.
#[tauri::command]
pub(crate) async fn start_file_download(
    app: tauri::AppHandle,
    cid: String,
) -> Result<FileManifest, String> {
    let mut m = load_manifest(&cid).ok_or("not found")?;
    let dest = file_store_path(&app, &cid);
    if dest.is_file() {
        if m.downloading {
            m.downloading = false;
            let _ = persist_manifest(&m);
        }
        return Ok(m); // already have it
    }
    // paid files require a confirmed purchase (owner is exempt) — checked up front
    if m.is_paid && m.owner != my_address() && !has_paid(&m) {
        return Err("payment required".to_string());
    }
    let blob = m.blob.clone().ok_or("no transit hash for this file")?;
    let node = m.seeder_node.clone().ok_or("no known seeder")?;
    // dedup: if a background fetch for this cid is already running, no-op
    {
        let mut act = ACTIVE_DL.lock().map_err(|_| "lock".to_string())?;
        if act.contains(&cid) {
            m.downloading = true;
            return Ok(m);
        }
        act.insert(cid.clone());
    }
    // record the downloading state so the «Скачиваются» tab shows it everywhere
    m.downloading = true;
    let _ = persist_manifest(&m);

    let fallback = m.clone();
    let dest_str = dest.to_string_lossy().to_string();
    let cid_bg = cid.clone();
    let providers = file_seeders(&cid);
    tauri::async_runtime::spawn(async move {
        let res = crate::p2p::fetch_file(&app, &node, &blob, &dest_str, &providers).await;
        let mut mm = load_manifest(&cid_bg).unwrap_or(fallback);
        mm.downloading = false;
        if res.is_ok() {
            mm.seeding = true;
            mm.downloaded_at = Some(now_secs());
            if mm.encrypted {
                let p = file_store_path(&app, &cid_bg);
                mm.display_name = decrypt_private_name(&mm, &p);
                if let Some(sz) = private_plain_size(&p) {
                    mm.size = sz;
                }
            }
            if let Ok(my_node) = crate::p2p::node_id(&app).await {
                if !my_node.is_empty() {
                    add_seeder(&cid_bg, &my_node);
                    mm.seeder_node = Some(my_node);
                }
            }
        }
        let _ = persist_manifest(&mm);
        if let Ok(mut act) = ACTIVE_DL.lock() {
            act.remove(&cid_bg);
        }
    });
    Ok(m)
}
/// blob from disk + drops the local manifest/seed cache, then reclaims the blob.
/// Owned files must use `delete_file` instead.
#[tauri::command]
pub(crate) async fn delete_downloaded_file(app: tauri::AppHandle, cid: String) -> Result<(), String> {
    let m = load_manifest(&cid).ok_or("not found")?;
    if m.owner == my_address() && !m.owner.is_empty() {
        return Err("это ваш файл — используйте обычное удаление".to_string());
    }
    let path = file_store_path(&app, &cid);
    let _ = std::fs::remove_file(&path);
    let _ = DB.remove(format!("{}{}", FILE_PREFIX, cid).as_bytes());
    let _ = DB.remove(seeds_key(&cid));
    let _ = DB.flush();
    crate::p2p::reclaim_after_delete(&app).await;
    Ok(())
}

/// Save a locally-held file to a user-chosen path via the native save dialog.
#[tauri::command(async)]
pub(crate) fn save_file_as(app: tauri::AppHandle, cid: String) -> Result<bool, String> {
    let m = load_manifest(&cid).ok_or("not found")?;
    let src = file_store_path(&app, &cid);
    if !src.is_file() {
        return Err("файл ещё не скачан".to_string());
    }
    // decrypt private files & recover the real name (public files pass through)
    let stored = std::fs::read(&src).map_err(|e| e.to_string())?;
    let (bytes, real_name, _mime) = decrypt_stored(&m, stored)?;
    let chosen = app
        .dialog()
        .file()
        .set_file_name(&real_name)
        .blocking_save_file();
    match chosen.and_then(|fp| fp.into_path().ok()) {
        Some(dst) => {
            std::fs::write(&dst, &bytes).map_err(|e| e.to_string())?;
            Ok(true)
        }
        None => Ok(false), // cancelled
    }
}

/// Open a locally-held file with the OS default application (player, viewer…).
/// The on-disk store path is the bare CID (no extension), so we export a cached
/// temp copy WITH the real name so the OS resolves the correct default app.
#[tauri::command(async)]
pub(crate) fn open_file_native(app: tauri::AppHandle, cid: String) -> Result<(), String> {
    let m = load_manifest(&cid).ok_or("not found")?;
    let src = file_store_path(&app, &cid);
    if !src.is_file() {
        return Err("файл ещё не скачан".to_string());
    }
    // decrypt private files & recover the real name so the OS resolves the app
    let stored = std::fs::read(&src).map_err(|e| e.to_string())?;
    let (bytes, real_name, _mime) = decrypt_stored(&m, stored)?;
    let mut tmp_dir = std::env::temp_dir();
    tmp_dir.push("smartnet-open");
    std::fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
    let safe: String = real_name
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    let safe = if safe.trim_matches('_').is_empty() { cid.clone() } else { safe };
    let tmp = tmp_dir.join(format!("{}-{}", &cid[..cid.len().min(8)], safe));
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    app.shell()
        .open(tmp.to_string_lossy().to_string(), None)
        .map_err(|e| e.to_string())
}

/// Pay for a paid file: transfer the price to the owner with the
/// `filebuy:<cid>:<buyer>` memo. Returns the payment tx id.
#[tauri::command]
pub(crate) async fn buy_file(_app: tauri::AppHandle, cid: String) -> Result<String, String> {
    let m = load_manifest(&cid).ok_or("not found")?;
    if !m.is_paid {
        return Err("файл бесплатный".to_string());
    }
    let buyer = my_address();
    if buyer.is_empty() {
        return Err("locked".to_string());
    }
    let secret = {
        let guard = SESSION_SEED.lock().unwrap();
        guard.as_ref().ok_or("locked")?.clone()
    };
    let price = format!("{}", m.price);
    let memo = format!("filebuy:{}:{}", cid, buyer);
    let owner = m.owner.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        crate::sth_node::send_transfer(&secret, &owner, &price, "", &memo)
    })
    .await
    .map_err(|e| e.to_string())??;
    if !outcome.accepted {
        return Err("Сеть отклонила оплату (проверьте баланс)".to_string());
    }
    Ok(outcome.id)
}

/// True once the current user has a confirmed `filebuy:<cid>:<buyer>` payment to
/// the file owner. Checked by the download page polling loop.
fn has_paid(m: &FileManifest) -> bool {
    let buyer = my_address();
    if buyer.is_empty() {
        return false;
    }
    let expect = format!("filebuy:{}:{}", m.cid, buyer);
    crate::sth_node::fetch_history(&m.owner, 50)
        .map(|items| {
            items
                .iter()
                .any(|it| it.kind == "in" && it.confirmed && it.counterparty == buyer && it.memo == expect)
        })
        .unwrap_or(false)
}

/// Sales counter for the owner's paid files: distinct on-chain buyers per CID,
/// counted from confirmed inbound `filebuy:<cid>:<buyer>` memos. One history
/// scan covers all requested CIDs. Owner-only (uses my_address as the owner).
#[tauri::command]
pub(crate) async fn file_sales_counts(
    cids: Vec<String>,
) -> std::collections::HashMap<String, usize> {
    use std::collections::{HashMap, HashSet};
    let mut out: HashMap<String, usize> = cids.iter().cloned().map(|c| (c, 0usize)).collect();
    let owner = my_address();
    if owner.is_empty() || out.is_empty() {
        return out;
    }
    let wanted: HashSet<String> = out.keys().cloned().collect();
    let items = tauri::async_runtime::spawn_blocking(move || {
        crate::sth_node::fetch_history(&owner, 500).unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    let mut buyers: HashMap<String, HashSet<String>> = HashMap::new();
    for it in items {
        if it.kind != "in" || !it.confirmed {
            continue;
        }
        if let Some(rest) = it.memo.strip_prefix("filebuy:") {
            if let Some(idx) = rest.rfind(':') {
                let cid = rest[..idx].to_string();
                if wanted.contains(&cid) {
                    buyers.entry(cid).or_default().insert(rest[idx + 1..].to_string());
                }
            }
        }
    }
    for (cid, set) in buyers {
        out.insert(cid, set.len());
    }
    out
}

/// Access check for the download page (free / owner / purchased).
#[tauri::command]
pub(crate) async fn check_file_access(app: tauri::AppHandle, cid: String) -> bool {
    let m = match load_manifest(&cid) {
        Some(m) => m,
        None => return false,
    };
    let _ = app;
    if !m.is_paid || m.owner == my_address() {
        return true;
    }
    tauri::async_runtime::spawn_blocking(move || has_paid(&m))
        .await
        .unwrap_or(false)
}

// ============================================================
//  E2EE Private Files — Phase B: on-chain notifications + inbox scanner
//
//  The owner notifies each whitelisted recipient with a memo transfer whose
//  vendorField carries a compact `sf:<cid>~<node>` link (≤255 B). Each client
//  scans its OWN inbound transactions for the `sf:` prefix, registers a remote
//  private manifest and raises a `private-file-received` event → toast + a
//  "Private Files" tab. Decryption self-serves the wrapped key from the blob.
// ============================================================

const PRIVATE_INBOX_KEY: &[u8] = b"file:private:inbox";

fn read_private_inbox() -> Vec<String> {
    DB.get(PRIVATE_INBOX_KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Vec<String>>(&v).ok())
        .unwrap_or_default()
}

fn add_private_inbox(cid: &str) {
    let mut list = read_private_inbox();
    if !list.iter().any(|c| c == cid) {
        list.insert(0, cid.to_string());
        if let Ok(j) = serde_json::to_vec(&list) {
            let _ = DB.insert(PRIVATE_INBOX_KEY, j);
        }
    }
}

/// Notify every whitelisted recipient of a private file over the chain: a memo
/// transfer carrying `sf:<cid>~<node>` in the vendorField (standard fee). Owner
/// only. Returns the number of accepted notifications.
#[tauri::command]
pub(crate) async fn notify_private_recipients(
    app: tauri::AppHandle,
    cid: String,
) -> Result<usize, String> {
    let mut m = load_manifest(&cid).ok_or("not found")?;
    if m.owner != my_address() {
        return Err("not owner".to_string());
    }
    if !m.encrypted {
        return Err("файл не приватный".to_string());
    }
    // ensure the blob is seeded and this node is advertised so the link resolves
    if m.blob.is_none() {
        let path = file_store_path(&app, &cid);
        if let Some(hash) = crate::p2p::seed_file(&app, &path.to_string_lossy()).await {
            m.blob = Some(hash);
        }
    }
    let node = match m.seeder_node.clone() {
        Some(n) if !n.is_empty() => n,
        _ => {
            let n = crate::p2p::node_id(&app).await.unwrap_or_default();
            if !n.is_empty() {
                m.seeder_node = Some(n.clone());
                add_seeder(&cid, &n);
            }
            n
        }
    };
    if node.is_empty() {
        return Err("нет активного P2P-узла для формирования ссылки".to_string());
    }
    persist_manifest(&m)?;

    let link = format!("sf:{}~{}", cid, node);
    if link.len() > 255 {
        return Err("ссылка превышает лимит vendorField (255 байт)".to_string());
    }
    let me = my_address();
    let targets: Vec<String> = m
        .recipients
        .iter()
        .chain(m.wrapped_keys.keys())
        .map(|s| s.trim().to_string())
        .filter(|a| !a.is_empty() && *a != me)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if targets.is_empty() {
        return Err("нет получателей для оповещения".to_string());
    }
    let secret = {
        let g = SESSION_SEED.lock().unwrap();
        g.as_ref().ok_or("locked")?.clone()
    };
    let sent = tauri::async_runtime::spawn_blocking(move || {
        let mut ok = 0usize;
        for addr in targets {
            if let Ok(o) = crate::sth_node::send_transfer(&secret, &addr, "0.00000001", "1", &link) {
                if o.accepted {
                    ok += 1;
                }
            }
        }
        ok
    })
    .await
    .map_err(|e| e.to_string())?;
    if sent == 0 {
        return Err("Сеть отклонила оповещения (проверьте баланс)".to_string());
    }
    m.published = true;
    m.published_at = Some(now_secs());
    persist_manifest(&m)?;
    Ok(sent)
}

/// Received private files (from the on-chain `sf:` scanner) — the "Private Files"
/// inbox. Ordered newest-first.
#[tauri::command]
pub(crate) async fn list_private_inbox() -> Vec<FileManifest> {
    tauri::async_runtime::spawn_blocking(|| {
        read_private_inbox()
            .into_iter()
            .filter_map(|cid| {
                load_manifest(&cid).map(|mut m| {
                    m.seeds = seed_count(&m.cid);
                    m
                })
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default()
}

/// One scan pass over the current user's inbound transactions: register any
/// `sf:<cid>~<node>` private-file notifications and raise a UI event.
fn scan_private_inbox_once(app: &tauri::AppHandle) {
    let me = my_address();
    if me.is_empty() {
        return;
    }
    let txs = match crate::sth_node::fetch_txs("recipientId", &me, 50) {
        Ok(t) => t,
        Err(_) => return,
    };
    for t in txs {
        let payload = match t.vendor.strip_prefix("sf:") {
            Some(p) => p.trim().to_string(),
            None => continue,
        };
        if t.id.is_empty() {
            continue;
        }
        let seen = format!("file:sf:seen:{}", t.id);
        if DB.get(seen.as_bytes()).ok().flatten().is_some() {
            continue;
        }
        let _ = DB.insert(seen.as_bytes(), b"1");

        let mut parts = payload.splitn(2, '~');
        let cid = parts.next().unwrap_or("").trim().to_string();
        let node = parts.next().unwrap_or("").trim().to_string();
        if cid.is_empty() {
            continue;
        }
        // never clobber a file we own / already hold locally
        if load_manifest(&cid).map(|x| !x.remote).unwrap_or(false) {
            add_private_inbox(&cid);
            continue;
        }
        let m = FileManifest {
            cid: cid.clone(),
            name: "🔒 Приватный файл".to_string(),
            size: 0,
            mime_type: "application/octet-stream".to_string(),
            owner: t.sender.clone(),
            price: 0.0,
            is_paid: false,
            created: t.timestamp,
            seeding: false,
            // the iroh transit hash equals the Blake3 cid
            blob: Some(cid.clone()),
            seeder_node: if node.is_empty() { None } else { Some(node.clone()) },
            published: false,
            txid: Some(t.id.clone()),
            remote: true,
            ipfs_cid: None,
            seeds: 0,
            downloaded_at: None,
            published_at: None,
            downloading: false,
            encrypted: true,
            wrapped_keys: HashMap::new(),
            recipients: Vec::new(),
            display_name: None,
            tag: String::new(),
        };
        if !node.is_empty() {
            add_seeder(&cid, &node);
        }
        let _ = persist_manifest(&m);
        add_private_inbox(&cid);
        let _ = app.emit(
            "private-file-received",
            serde_json::json!({ "cid": cid, "from": t.sender }),
        );
    }
    let _ = DB.flush();
}

/// Background poller (45s) for incoming private-file notifications.
pub(crate) fn spawn_private_scanner(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        if crate::netwatch::is_online() {
            scan_private_inbox_once(&app);
        }
        std::thread::sleep(std::time::Duration::from_secs(45));
    });
}
