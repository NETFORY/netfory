// ============================================================
//  SmartNet // apps.rs
//
//  dApp lifecycle on the native client: the data/ directory + storage-quota
//  management, the publish/update pipeline (recursive deep-copy -> Blake3
//  merkle root -> ECDSA sign with the app identity key -> Sled registry +
//  content.json), the registry readers (list/announced/readme) and the
//  content-addressed `verify_app` (re-hash on disk + signature + id↔pubkey).
//
//  Shared low-level primitives (DB, Manifest, ensure_data_dir, sanitize,
//  dir_size, storage-limit helpers) live in the crate root and are reused here.
//
//  Author: TechnoL0g
//  Created: 2026-06-19
// ============================================================

use crate::crypto::*;
use crate::{
    dir_size, ensure_data_dir, get_storage_limit_internal, load_vault_internal, sanitize, FileEntry, Manifest,
    APP_PREFIX, DB, DEFAULT_STORAGE_LIMIT, SESSION_SEED,
};
use secp256k1::ecdsa::Signature;
use secp256k1::{Message, PublicKey, Secp256k1};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri_plugin_dialog::DialogExt;

// ============================================================
//  data/ directory
// ============================================================

#[tauri::command]
pub(crate) fn get_data_dir(app: tauri::AppHandle) -> String {
    ensure_data_dir(&app).to_string_lossy().to_string()
}

/// Progress payload streamed to the UI during a data-directory relocation.
#[derive(Clone, Serialize)]
struct TransferProgress {
    phase: String,
    #[serde(rename = "movedBytes")]
    moved_bytes: u64,
    #[serde(rename = "totalBytes")]
    total_bytes: u64,
    #[serde(rename = "currentItem")]
    current_item: String,
}

/// Relocate the data/ root to a user-chosen folder (e.g. another disk).
/// Physically transfers dapps/, blobs/, manifests/, keys/ and node.key from the
/// current location to the new one, streaming a `data-transfer-progress` event
/// so the UI can show a progress bar. The cfg:data_dir pointer is only switched
/// AFTER the copy succeeds, so a failed/aborted move never strands the apps.
#[tauri::command]
pub(crate) async fn set_data_dir(app: tauri::AppHandle, path: String) -> Result<String, String> {
    let dest = PathBuf::from(path.trim());
    let src = ensure_data_dir(&app);

    // Same folder (canonicalized) → nothing to move, just persist & return.
    let same = match (src.canonicalize(), dest.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => src == dest,
    };
    if same {
        return Ok(src.to_string_lossy().to_string());
    }

    // Guard: never copy a folder into its own subdirectory (would recurse).
    if let (Ok(a), Ok(b)) = (src.canonicalize(), dest.parent().map(|p| p.canonicalize()).transpose().map(|o| o.unwrap_or_else(|| dest.clone()))) {
        if b.starts_with(&a) {
            return Err("Целевая папка находится внутри текущей папки данных".into());
        }
    }

    let dest_clone = dest.clone();
    tauri::async_runtime::spawn_blocking(move || transfer_data_dir(app, src, dest_clone))
        .await
        .map_err(|e| e.to_string())??;

    Ok(dest.to_string_lossy().to_string())
}

/// Blocking worker: flush Sled, move the tree with progress, switch the pointer.
fn transfer_data_dir(
    app: tauri::AppHandle,
    src: PathBuf,
    dest: PathBuf,
) -> Result<(), String> {
    use tauri::Emitter;

    // Flush the Sled DB so any pending writes are durable before we move files.
    let _ = DB.flush();

    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;

    let total = dir_size(&src).saturating_sub(SKIP_TOP.iter().map(|s| dir_size(&src.join(s))).sum::<u64>());
    let _ = app.emit(
        "data-transfer-progress",
        TransferProgress {
            phase: "scanning".into(),
            moved_bytes: 0,
            total_bytes: total,
            current_item: String::new(),
        },
    );

    let mut moved: u64 = 0;
    copy_tree_with_progress(&app, &src, &dest, total, &mut moved)?;

    // Switch the active pointer ONLY after a fully successful copy.
    DB.insert(b"cfg:data_dir", dest.to_string_lossy().as_bytes())
        .map_err(|e| e.to_string())?;
    DB.flush().map_err(|e| e.to_string())?;

    // Best-effort cleanup of the old location (data already safely copied).
    // Sled (`smartnet_db`) остаётся на месте — он не переносится (см. SKIP_TOP).
    let _ = app.emit(
        "data-transfer-progress",
        TransferProgress {
            phase: "cleanup".into(),
            moved_bytes: total,
            total_bytes: total,
            current_item: String::new(),
        },
    );
    if let Ok(rd) = std::fs::read_dir(&src) {
        for e in rd.flatten() {
            if SKIP_TOP.iter().any(|s| e.file_name() == *s) {
                continue;
            }
            let p = e.path();
            let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        }
    }

    let _ = app.emit(
        "data-transfer-progress",
        TransferProgress {
            phase: "done".into(),
            moved_bytes: total,
            total_bytes: total,
            current_item: dest.to_string_lossy().to_string(),
        },
    );
    Ok(())
}

/// Верхнеуровневые записи data/, которые НЕ переносятся: sled-база
/// `smartnet_db` живёт по фиксированному пути `<app_data>/data/smartnet_db`
/// (см. `DB` в lib.rs — открывается ДО чтения cfg:data_dir) и залочена
/// процессом (Windows: os error 33 при копировании `db`). Раньше перенос
/// падал на ней, а cleanup мог снести базу вместе с сейфом.
const SKIP_TOP: &[&str] = &["smartnet_db"];

/// Файл заблокирован другим процессом/нами (Windows ERROR_SHARING_VIOLATION=32,
/// ERROR_LOCK_VIOLATION=33).
fn is_locked_err(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(32) | Some(33))
}

/// Служебные файлы хранилищ, которые пересоздаются при следующем старте
/// (redb-индекс iroh-blobs, lock-файлы) — их можно пропустить, если залочены.
fn is_recreatable(name: &str) -> bool {
    name.starts_with("blobs.db") || name.ends_with(".lock") || name == "LOCK"
}

/// Recursively copy `src` into `dst`, emitting per-file progress. Falls back to
/// a byte copy on every file (cross-device safe; `fs::rename` won't work across
/// disks, which is the whole point of relocating).
fn copy_tree_with_progress(
    app: &tauri::AppHandle,
    src: &Path,
    dst: &Path,
    total: u64,
    moved: &mut u64,
) -> Result<(), String> {
    copy_tree_inner(app, src, dst, total, moved, true)
}

fn copy_tree_inner(
    app: &tauri::AppHandle,
    src: &Path,
    dst: &Path,
    total: u64,
    moved: &mut u64,
    top: bool,
) -> Result<(), String> {
    use tauri::Emitter;

    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let entries = std::fs::read_dir(src).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let name = entry.file_name();
        if top && SKIP_TOP.iter().any(|s| name == *s) {
            continue;
        }
        let to = dst.join(&name);
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            copy_tree_inner(app, &from, &to, total, moved, false)?;
        } else {
            if let Err(e) = std::fs::copy(&from, &to) {
                let fname = name.to_string_lossy().to_string();
                if is_locked_err(&e) && is_recreatable(&fname) {
                    crate::syslog::log("sys", format!("перенос данных: {} заблокирован — пропущен (пересоздастся)", from.display()));
                    continue;
                }
                return Err(format!("Не удалось скопировать {}: {}", from.display(), e));
            }
            *moved += entry.metadata().map(|m| m.len()).unwrap_or(0);
            let _ = app.emit(
                "data-transfer-progress",
                TransferProgress {
                    phase: "copying".into(),
                    moved_bytes: *moved,
                    total_bytes: total,
                    current_item: name.to_string_lossy().to_string(),
                },
            );
        }
    }
    Ok(())
}

// ============================================================
//  Storage quota (data/dapps capacity limit)
// ============================================================

#[derive(Debug, Serialize, Default)]
pub(crate) struct StorageUsage {
    #[serde(rename = "dataTotal")]
    data_total: u64,
    #[serde(rename = "dappsUsed")]
    dapps_used: u64,
    limit: u64,
}

#[tauri::command]
pub(crate) async fn storage_usage(app: tauri::AppHandle) -> StorageUsage {
    tokio::task::spawn_blocking(move || {
        let data = ensure_data_dir(&app);
        StorageUsage {
            data_total: dir_size(&data),
            dapps_used: dir_size(&data.join("dapps")),
            limit: get_storage_limit_internal(),
        }
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
pub(crate) fn get_storage_limit() -> u64 {
    get_storage_limit_internal()
}

#[derive(Debug, Serialize, Default)]
pub(crate) struct DiskSpace {
    #[serde(rename = "totalGb")]
    total_gb: f64,
    #[serde(rename = "freeGb")]
    free_gb: f64,
}

/// Physical capacity of the disk that hosts the data/ directory, as
/// (total_bytes, free_bytes). Picks the mount point with the longest prefix
/// match against the data dir (handles nested mounts). (0, 0) if undetermined.
pub(crate) fn data_disk_space(app: &tauri::AppHandle) -> (u64, u64) {
    let data = ensure_data_dir(app);
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut best: Option<(usize, u64, u64)> = None;
    for disk in &disks {
        let mp = disk.mount_point();
        if data.starts_with(mp) {
            let len = mp.as_os_str().len();
            if best.map_or(true, |(b, _, _)| len > b) {
                best = Some((len, disk.total_space(), disk.available_space()));
            }
        }
    }
    best.map(|(_, t, a)| (t, a)).unwrap_or((0, 0))
}

/// Free space (whole GB) on the data-dir disk — used to bound the STH Earn
/// seeding budget to what the drive can actually hold.
pub(crate) fn free_disk_gb(app: &tauri::AppHandle) -> u64 {
    const GB: u64 = 1024 * 1024 * 1024;
    data_disk_space(app).1 / GB
}

/// Physical capacity of the disk that hosts the data/ directory. Lets the
/// "STH Earn" seeding budget scale to the real drive size (NAS/RAID up to TBs)
/// instead of the arbitrary 100 GB UI cap.
#[tauri::command]
pub(crate) async fn disk_space(app: tauri::AppHandle) -> DiskSpace {
    tokio::task::spawn_blocking(move || {
        let (total, avail) = data_disk_space(&app);
        const GB: f64 = 1024.0 * 1024.0 * 1024.0;
        DiskSpace {
            total_gb: total as f64 / GB,
            free_gb: avail as f64 / GB,
        }
    })
    .await
    .unwrap_or_default()
}

/// Set the data/dapps storage limit (bytes). Clamped to the 8 GiB minimum.
#[tauri::command]
pub(crate) fn set_storage_limit(bytes: u64) -> Result<u64, String> {
    let applied = bytes.max(DEFAULT_STORAGE_LIMIT);
    let le = applied.to_le_bytes();
    DB.insert(b"cfg:storage_limit", &le[..])
        .map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(applied)
}

#[tauri::command(async)]
pub(crate) fn pick_folder(app: tauri::AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .blocking_pick_folder()
        .and_then(|fp| fp.into_path().ok())
        .map(|p| p.to_string_lossy().to_string())
}

/// Native "Save As" dialog: prompt the user for a path (suggesting `default_name`),
/// then write the base64-decoded `contents_b64` there. Returns the saved path, or
/// None if the user cancelled. Used for the keypair export + the profile share
/// card so the desktop app shows a real file picker (the in-webview `<a download>`
/// is unreliable on WebView2/webkit).
#[tauri::command(async)]
pub(crate) fn save_file_dialog(
    app: tauri::AppHandle,
    default_name: String,
    contents_b64: String,
) -> Result<Option<String>, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(contents_b64.as_bytes())
        .map_err(|e| e.to_string())?;
    let picked = app
        .dialog()
        .file()
        .set_file_name(&default_name)
        .blocking_save_file();
    match picked.and_then(|fp| fp.into_path().ok()) {
        Some(path) => {
            std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
            Ok(Some(path.to_string_lossy().to_string()))
        }
        None => Ok(None),
    }
}

// ============================================================
//  Dynamic payload pricing (size-based publish/update fee)
// ============================================================

/// Recursively count regular files under a directory (for the dropzone UI).
fn dir_file_count(path: &Path) -> u64 {
    let mut n = 0u64;
    if let Ok(rd) = std::fs::read_dir(path) {
        for e in rd.flatten() {
            match e.metadata() {
                Ok(md) if md.is_dir() => n += dir_file_count(&e.path()),
                Ok(_) => n += 1,
                Err(_) => {}
            }
        }
    }
    n
}

#[derive(Debug, Serialize)]
pub(crate) struct PayloadInfo {
    /// absolute folder path (echoed back for the UI).
    path: String,
    /// folder leaf name.
    name: String,
    /// total payload size in bytes.
    #[serde(rename = "sizeBytes")]
    size_bytes: u64,
    /// payload size in whole (rounded) megabytes — the billed weight.
    #[serde(rename = "sizeMb")]
    size_mb: u64,
    /// number of regular files in the payload.
    #[serde(rename = "fileCount")]
    file_count: u64,
    /// dynamic operation cost in whole STH (base + sizeMb * pricePerMb).
    cost: u64,
}

fn inspect_dir(dir: &str) -> Result<PayloadInfo, String> {
    let src = PathBuf::from(dir.trim());
    if !src.is_dir() {
        return Err("source folder not found".to_string());
    }
    let size_bytes = dir_size(&src);
    Ok(PayloadInfo {
        path: src.to_string_lossy().to_string(),
        name: src
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        size_bytes,
        size_mb: crate::oracle::bytes_to_mb(size_bytes),
        file_count: dir_file_count(&src),
        cost: crate::oracle::compute_cost(size_bytes),
    })
}

/// UNIFIED dynamic price (whole STH) for a publish OR update payload at `dir`.
/// Total_Cost = base_price + (Size_In_MB * price_per_mb).
#[tauri::command]
pub(crate) fn get_payload_pricing(dir: String) -> Result<u64, String> {
    Ok(inspect_dir(&dir)?.cost)
}

/// Rich payload inspection (path/name/size/file-count/cost) for the dropzone +
/// publish/update confirmation modals.
#[tauri::command]
pub(crate) fn inspect_payload(dir: String) -> Result<PayloadInfo, String> {
    inspect_dir(&dir)
}

/// Inspect the stored source folder of an existing draft/app by id (used to
/// price the Publish modal, which has no freshly-picked folder).
#[tauri::command]
pub(crate) fn inspect_app_payload(id: String) -> Result<PayloadInfo, String> {
    let raw = DB
        .get(format!("{}{}", APP_PREFIX, id).as_bytes())
        .map_err(|e| e.to_string())?
        .ok_or("app not found")?;
    let m: Manifest = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let dir = m.src_dir.ok_or("source folder missing — re-create the draft")?;
    inspect_dir(&dir)
}

// ============================================================
//  dApp publish pipeline
// ============================================================

/// Monotonic app index — persisted in `cfg:app_index_seq` and never reused, even
/// after deletions, so a re-created draft can NEVER collide with an existing
/// app's deterministic id (the old `count`-based scheme reused indices and could
/// silently overwrite / duplicate apps).
fn next_app_index() -> u32 {
    let stored = DB
        .get(b"cfg:app_index_seq")
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    // never below the highest index already in the registry (legacy DBs).
    let used_next = DB
        .scan_prefix(APP_PREFIX.as_bytes())
        .filter_map(|r| r.ok())
        .filter_map(|(_, v)| serde_json::from_slice::<Manifest>(&v).ok())
        .filter_map(|m| m.index)
        .max()
        .map(|m| m + 1)
        .unwrap_or(0);
    let next = stored.max(used_next);
    let _ = DB.insert(b"cfg:app_index_seq", (next + 1).to_string().as_bytes());
    let _ = DB.flush();
    next
}

fn collect_and_copy(
    src_root: &Path,
    cur: &Path,
    dest_root: &Path,
    entries: &mut Vec<FileEntry>,
    blake_parts: &mut Vec<(String, String)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(cur)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_and_copy(src_root, &path, dest_root, entries, blake_parts)?;
        } else {
            let rel = path.strip_prefix(src_root).unwrap_or(&path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let dest = dest_root.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let data = std::fs::read(&path)?;
            std::fs::write(&dest, &data)?;
            entries.push(FileEntry {
                path: rel_str.clone(),
                sha256: hex::encode(sha256(&data)),
                size: data.len() as u64,
            });
            blake_parts.push((rel_str, blake3::hash(&data).to_hex().to_string()));
        }
    }
    Ok(())
}

/// Detect the logo image format from magic bytes. Only PNG and SVG are accepted;
/// anything else is treated as PNG (the frontend already restricts the picker).
fn logo_ext(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        return "png";
    }
    let head = &bytes[..bytes.len().min(512)];
    if let Ok(s) = std::str::from_utf8(head) {
        let t = s.trim_start();
        if t.starts_with("<?xml") || t.starts_with("<svg") || t.contains("<svg") {
            return "svg";
        }
    }
    "png"
}

/// v1.63.19: pre-scan `dest_root` (recursively) and rebuild BOTH
/// `manifest.files` (SHA-256 per file) AND the Blake3 merkle from the CURRENT
/// on-disk state — reproducing the same layout `collect_and_copy` produces on
/// initial ingest. Used by `edit_app_metadata` after replacing the logo: the
/// file payload on disk has changed (the new logo overwrites/adds a file, the
/// old logo names get removed), so the manifest's file list + merkle MUST be
/// re-derived to match, otherwise `verify_app` fails with `file-hash-mismatch`
/// / `missing-file` and the UI shows the red "content.json signature does NOT
/// match the app address" banner. Sorted by path — matches ingest ordering.
fn rescan_dest_files_and_merkle(dest_root: &Path) -> std::io::Result<(Vec<FileEntry>, String)> {
    fn walk(
        root: &Path,
        cur: &Path,
        files: &mut Vec<FileEntry>,
        blake_parts: &mut Vec<(String, String)>,
    ) -> std::io::Result<()> {
        for entry in std::fs::read_dir(cur)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, files, blake_parts)?;
            } else {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                // content.json is the manifest itself — never listed in files.
                if rel_str == "content.json" {
                    continue;
                }
                let data = std::fs::read(&path)?;
                files.push(FileEntry {
                    path: rel_str.clone(),
                    sha256: hex::encode(sha256(&data)),
                    size: data.len() as u64,
                });
                blake_parts.push((rel_str, blake3::hash(&data).to_hex().to_string()));
            }
        }
        Ok(())
    }
    let mut files: Vec<FileEntry> = Vec::new();
    let mut blake_parts: Vec<(String, String)> = Vec::new();
    walk(dest_root, dest_root, &mut files, &mut blake_parts)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    blake_parts.sort_by(|a, b| a.0.cmp(&b.0));
    let mut root_hasher = blake3::Hasher::new();
    for (p, h) in &blake_parts {
        root_hasher.update(p.as_bytes());
        root_hasher.update(h.as_bytes());
    }
    Ok((files, root_hasher.finalize().to_hex().to_string()))
}

/// Read an existing logo's raw bytes from a dApp folder: canonical
/// `<id>-logo.svg|png` or legacy `logo.svg|png` / `icon.png`. Used to preserve a
/// previously-uploaded logo across re-ingest (finalize/preview/update wipe the
/// folder), so the logo no longer vanishes the moment a draft is published.
fn read_logo_bytes(dir: &Path, id: &str) -> Option<Vec<u8>> {
    let names = [
        format!("{}-logo.svg", id),
        format!("{}-logo.png", id),
        "logo.svg".to_string(),
        "logo.png".to_string(),
        "icon.png".to_string(),
    ];
    for name in names {
        if let Ok(bytes) = std::fs::read(dir.join(&name)) {
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
    }
    None
}

/// Формат DAPP Preview по magic-байтам: только PNG или обычный (Huffman) JPEG.
/// JPEG с арифметическим кодированием отбраковывается — WebView его не рисует.
fn preview_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        return Some("png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        if crate::p2p::jpeg_is_arithmetic(bytes) {
            return None;
        }
        return Some("jpg");
    }
    None
}

/// Байты существующего DAPP Preview (`<id>-preview.png|jpg`) — сохраняет
/// preview при ре-инжесте (finalize/preview/update стирают папку).
fn read_preview_bytes(dir: &Path, id: &str) -> Option<Vec<u8>> {
    for name in [format!("{}-preview.png", id), format!("{}-preview.jpg", id)] {
        if let Ok(bytes) = std::fs::read(dir.join(&name)) {
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
    }
    None
}

/// Shared ingest: copy + Blake3 root + sign (with app key) + persist manifest.
/// `status` is "draft" or "published"; the gossip `announce:<id>` key is written
/// ONLY for a PAID published app (free/draft apps are never network-announced).
fn ingest_app(
    app: &tauri::AppHandle,
    secret: &str,
    app_id: &str,
    index: u32,
    name: &str,
    description: &str,
    tags: &[String],
    src: &Path,
    version: u32,
    status: &str,
    paid: bool,
    free: bool,
    escrow_txid: Option<String>,
    logo: Option<Vec<u8>>,
    preview: Option<Vec<u8>>,
    privacy: Option<crate::private_dapp::PrivacySpec>,
) -> Result<Manifest, String> {
    // Приватный dApp — отдельный конвейер (E2EE-контейнер вместо файлов);
    // preview у приватных не сохраняется (не светим visual identity).
    if let Some(spec) = privacy {
        return ingest_private(
            app, secret, app_id, index, name, description, tags, src, version, status, paid,
            free, escrow_txid, logo, spec,
        );
    }
    let owner = derive_account_internal(secret, 0)?.address;
    let app_path = derive_app_identity_internal(secret, index)?.path;

    let data_dir = ensure_data_dir(app);
    let dest_root = data_dir.join("dapps").join(app_id);
    // Preserve an already-uploaded logo across re-ingest: finalize_publish /
    // preview_draft / update_app pass logo=None and we wipe dest_root below, so
    // without this the logo was lost the instant a draft got published.
    let preserved_logo = if logo.is_none() {
        read_logo_bytes(&dest_root, app_id)
    } else {
        None
    };
    // Preview 440×180 переживает ре-инжест так же, как logo.
    let preserved_preview = if preview.is_none() {
        read_preview_bytes(&dest_root, app_id)
    } else {
        None
    };
    let _ = std::fs::remove_dir_all(&dest_root);
    std::fs::create_dir_all(&dest_root).map_err(|e| e.to_string())?;

    let mut files: Vec<FileEntry> = Vec::new();
    let mut blake_parts: Vec<(String, String)> = Vec::new();
    collect_and_copy(src, src, &dest_root, &mut files, &mut blake_parts).map_err(|e| e.to_string())?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    blake_parts.sort_by(|a, b| a.0.cmp(&b.0));

    // Защита разработчика: без index.html в корне dApp никогда не откроется
    // (sth:// отдаст 404) — блокируем публикацию сразу, а не у пользователей.
    if !files.iter().any(|f| f.path == "index.html") {
        return Err(
            "в корне dApp нет index.html — приложение не откроется по sth://. Положите index.html в корень папки сборки".to_string(),
        );
    }

    // Manual logo (PNG/SVG) — pure presentation metadata (NOT part of the signed
    // merkle), written into the app folder as `<id>-logo.<ext>`. It is archived
    // with the rest of the dApp so it travels over P2P and persists across
    // re-ingest. A fresh upload wins; otherwise the preserved one is restored.
    if let Some(bytes) = logo.filter(|b| !b.is_empty()).or(preserved_logo) {
        let ext = logo_ext(&bytes);
        let _ = std::fs::write(dest_root.join(format!("{}-logo.{}", app_id, ext)), &bytes);
    }

    // DAPP Preview 440×180 (PNG/JPG, ≤ 256 KB) — presentation-metadata как logo:
    // пишется рядом как `<id>-preview.<ext>` и распространяется отдельным блобом.
    if let Some(bytes) = preview.filter(|b| !b.is_empty()).or(preserved_preview) {
        if bytes.len() <= crate::p2p::LOGO_MAX_BYTES {
            if let Some(ext) = preview_ext(&bytes) {
                let _ =
                    std::fs::write(dest_root.join(format!("{}-preview.{}", app_id, ext)), &bytes);
            }
        }
    }

    let mut root_hasher = blake3::Hasher::new();
    for (p, h) in &blake_parts {
        root_hasher.update(p.as_bytes());
        root_hasher.update(h.as_bytes());
    }
    let merkle = root_hasher.finalize().to_hex().to_string();

    let now = now_iso();
    // canon v2: owner включён в подпись — бейдж Official нельзя подделать,
    // подменив owner в анонсе (Official Repository анти-спуфинг)
    let digest =
        sha256(format!("{}|{}|{}|{}|{}", app_id, name, description, merkle, owner).as_bytes());
    let (sk, app_pubkey, _) = keys_from_material(&format!("{}/1/{}", secret.trim(), index))?;
    let secp = Secp256k1::signing_only();
    let msg = Message::from_digest_slice(&digest).map_err(|e| e.to_string())?;
    let signature = hex::encode(secp.sign_ecdsa(&msg, &sk).serialize_compact());

    // normalize tags (lowercase, deduped, max 8) — metadata only, not signed
    let mut clean_tags: Vec<String> = Vec::new();
    for tg in tags {
        let t = tg.trim().to_lowercase();
        if !t.is_empty() && !clean_tags.contains(&t) {
            clean_tags.push(t);
        }
        if clean_tags.len() >= 8 {
            break;
        }
    }

    let manifest = Manifest {
        id: app_id.to_string(),
        name: name.to_string(),
        description: Some(description.to_string()),
        owner,
        files,
        merkle,
        created: now.clone(),
        signature,
        public_key: app_pubkey,
        path: Some(app_path),
        index: Some(index),
        version: Some(version),
        updated: Some(now),
        blob: None,
        tags: clean_tags,
        readme: None,
        status: Some(status.to_string()),
        paid: Some(paid),
        free: Some(free),
        escrow_txid,
        src_dir: Some(src.to_string_lossy().to_string()),
        private: None,
    };

    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let _ = std::fs::write(dest_root.join("content.json"), &json);
    let _ = std::fs::write(
        data_dir.join("manifests").join(format!("{}.json", app_id)),
        &json,
    );
    DB.insert(format!("{}{}", APP_PREFIX, app_id).as_bytes(), json)
        .map_err(|e| e.to_string())?;
    // Network-announce ONLY a PAID published app; free/draft apps stay local
    // (shareable solely via a direct sth://<id> link).
    if status == "published" && paid {
        DB.insert(
            format!("announce:{}", app_id).as_bytes(),
            now_iso().into_bytes(),
        )
        .map_err(|e| e.to_string())?;
    } else {
        let _ = DB.remove(format!("announce:{}", app_id).as_bytes());
    }
    // публичный ре-ingest поверх бывшего приватного — чистим access-кэш
    let _ = DB.remove(format!("privaccess:{}", app_id).as_bytes());
    let _ = DB.flush();
    Ok(manifest)
}

/// Приватный ingest: dist → tar → AES-256-GCM `app.enc` + `access.json`.
/// Публичный манифест несёт плейсхолдер-имя (реальные метаданные — в
/// зашифрованном privMeta), подпись — canon v2 над плейсхолдером; целостность
/// access.json/app.enc гарантирует merkle.
#[allow(clippy::too_many_arguments)]
fn ingest_private(
    app: &tauri::AppHandle,
    secret: &str,
    app_id: &str,
    index: u32,
    real_name: &str,
    real_description: &str,
    real_tags: &[String],
    src: &Path,
    version: u32,
    status: &str,
    paid: bool,
    free: bool,
    escrow_txid: Option<String>,
    logo: Option<Vec<u8>>,
    privacy: crate::private_dapp::PrivacySpec,
) -> Result<Manifest, String> {
    use base64::Engine as _;
    let owner = derive_account_internal(secret, 0)?.address;
    let app_path = derive_app_identity_internal(secret, index)?.path;
    if !src.join("index.html").is_file() {
        return Err(
            "в корне dApp нет index.html — приложение не откроется по sth://. Положите index.html в корень папки сборки".to_string(),
        );
    }

    // логотип: параметр → logo.png из src → прежний privMeta (Sled)
    let prev_meta = crate::private_dapp::load_access_db(app_id)
        .and_then(|a| crate::private_dapp::decrypt_meta(&a));
    let (logo_b64, logo_ext_s) =
        match logo.filter(|b| !b.is_empty()).or_else(|| read_logo_bytes(src, app_id)) {
            Some(bytes) => {
                let ext = logo_ext(&bytes).to_string();
                (base64::engine::general_purpose::STANDARD.encode(&bytes), ext)
            }
            None => prev_meta.map(|m| (m.logo_b64, m.logo_ext)).unwrap_or_default(),
        };

    let mut clean_tags: Vec<String> = Vec::new();
    for tg in real_tags {
        let t = tg.trim().to_lowercase();
        if !t.is_empty() && !clean_tags.contains(&t) {
            clean_tags.push(t);
        }
        if clean_tags.len() >= 8 {
            break;
        }
    }

    let tar_bytes = crate::private_dapp::tar_dir(src)?;
    let cek = crate::crypto::gen_file_key();
    let container = crate::crypto::aes_gcm_encrypt(&cek, &tar_bytes)?;
    let meta = crate::private_dapp::PrivMeta {
        name: real_name.to_string(),
        description: real_description.to_string(),
        tags: clean_tags,
        logo_b64,
        logo_ext: logo_ext_s,
    };
    let access = crate::private_dapp::build_access(&cek, &privacy.pubkeys, &meta)?;

    let data_dir = ensure_data_dir(app);
    let dest_root = data_dir.join("dapps").join(app_id);
    let _ = std::fs::remove_dir_all(&dest_root);
    std::fs::create_dir_all(&dest_root).map_err(|e| e.to_string())?;
    std::fs::write(dest_root.join(crate::private_dapp::CONTAINER_FILE), &container)
        .map_err(|e| e.to_string())?;
    let access_json = crate::private_dapp::save_access(&dest_root, app_id, &access)?;

    let mut files: Vec<FileEntry> = Vec::new();
    let mut blake_parts: Vec<(String, String)> = Vec::new();
    let pairs: [(&str, &[u8]); 2] = [
        (crate::private_dapp::ACCESS_FILE, &access_json),
        (crate::private_dapp::CONTAINER_FILE, &container),
    ];
    for (fname, data) in pairs {
        files.push(FileEntry {
            path: fname.to_string(),
            sha256: hex::encode(sha256(data)),
            size: data.len() as u64,
        });
        blake_parts.push((fname.to_string(), blake3::hash(data).to_hex().to_string()));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    blake_parts.sort_by(|a, b| a.0.cmp(&b.0));
    let mut root_hasher = blake3::Hasher::new();
    for (p, h) in &blake_parts {
        root_hasher.update(p.as_bytes());
        root_hasher.update(h.as_bytes());
    }
    let merkle = root_hasher.finalize().to_hex().to_string();

    let name = crate::private_dapp::PRIV_PLACEHOLDER;
    let description = String::new();
    let now = now_iso();
    let digest =
        sha256(format!("{}|{}|{}|{}|{}", app_id, name, description, merkle, owner).as_bytes());
    let (sk, app_pubkey, _) = keys_from_material(&format!("{}/1/{}", secret.trim(), index))?;
    let secp = Secp256k1::signing_only();
    let msg = Message::from_digest_slice(&digest).map_err(|e| e.to_string())?;
    let signature = hex::encode(secp.sign_ecdsa(&msg, &sk).serialize_compact());

    let manifest = Manifest {
        id: app_id.to_string(),
        name: name.to_string(),
        description: Some(description),
        owner,
        files,
        merkle,
        created: now.clone(),
        signature,
        public_key: app_pubkey,
        path: Some(app_path),
        index: Some(index),
        version: Some(version),
        updated: Some(now),
        blob: None,
        tags: Vec::new(),
        readme: None,
        status: Some(status.to_string()),
        paid: Some(paid),
        free: Some(free),
        escrow_txid,
        src_dir: Some(src.to_string_lossy().to_string()),
        private: Some(true),
    };
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let _ = std::fs::write(dest_root.join("content.json"), &json);
    let _ = std::fs::write(
        data_dir.join("manifests").join(format!("{}.json", app_id)),
        &json,
    );
    DB.insert(format!("{}{}", APP_PREFIX, app_id).as_bytes(), json)
        .map_err(|e| e.to_string())?;
    if status == "published" && paid {
        DB.insert(
            format!("announce:{}", app_id).as_bytes(),
            now_iso().into_bytes(),
        )
        .map_err(|e| e.to_string())?;
    } else {
        let _ = DB.remove(format!("announce:{}", app_id).as_bytes());
    }
    let _ = DB.remove(format!("privreveal:{}", app_id).as_bytes());
    let _ = DB.flush();
    crate::private_dapp::unmount(app_id); // старая RAM-копия недействительна
    let mut out = manifest;
    crate::private_dapp::decorate_manifest(&mut out);
    Ok(out)
}

/// Для private-манифестов: реальные метаданные + PrivacySpec (pubkeys из
/// кэша access) для повторного ingest (finalize / preview / update).
fn private_reingest_ctx(
    existing: &Manifest,
) -> Result<Option<(String, String, Vec<String>, crate::private_dapp::PrivacySpec)>, String> {
    if existing.private != Some(true) {
        return Ok(None);
    }
    let access = crate::private_dapp::load_access_db(&existing.id)
        .ok_or("access.json приватного dApp не найден")?;
    let meta = crate::private_dapp::decrypt_meta(&access)
        .ok_or("не удалось расшифровать метаданные приватного dApp (кошелёк заблокирован?)")?;
    Ok(Some((
        meta.name,
        meta.description,
        meta.tags,
        crate::private_dapp::PrivacySpec { pubkeys: access.pubkeys },
    )))
}

/// Create a NEW dApp DRAFT: derive a fresh app identity (sth://address) and
/// ingest it LOCALLY (status="draft", not announced). The developer can Preview
/// it for free, then pay to Publish. Title + description are MANDATORY.
#[tauri::command]
pub(crate) async fn create_draft_app(
    app: tauri::AppHandle,
    name: String,
    description: String,
    tags: Vec<String>,
    dir: String,
    logo: Option<Vec<u8>>,
    preview: Option<Vec<u8>>,
    private_recipients: Option<Vec<String>>,
) -> Result<Manifest, String> {
    tauri::async_runtime::spawn_blocking(move || {
        create_draft_app_blocking(app, name, description, tags, dir, logo, preview, private_recipients)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[allow(clippy::too_many_arguments)]
fn create_draft_app_blocking(
    app: tauri::AppHandle,
    name: String,
    description: String,
    tags: Vec<String>,
    dir: String,
    logo: Option<Vec<u8>>,
    preview: Option<Vec<u8>>,
    private_recipients: Option<Vec<String>>,
) -> Result<Manifest, String> {
    if name.trim().is_empty() {
        return Err("title required".to_string());
    }
    if description.trim().is_empty() {
        return Err("description required".to_string());
    }
    let secret = {
        let guard = SESSION_SEED.lock().unwrap();
        guard.as_ref().ok_or("locked")?.clone()
    };
    let src = PathBuf::from(&dir);
    if !src.is_dir() {
        return Err("source folder not found".to_string());
    }
    // Приватный dApp: резолвим on-chain pubkeys списка доступа (+ владелец)
    let recipients: Vec<String> = private_recipients
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let privacy = if recipients.is_empty() {
        None
    } else {
        Some(crate::private_dapp::PrivacySpec {
            pubkeys: crate::private_dapp::resolve_pubkeys(&recipients)?,
        })
    };
    // De-dup: if a DRAFT already points at this same source folder, update it in
    // place (same id/index) instead of minting a duplicate draft (e.g. the user
    // re-picked the same folder or double-clicked "create").
    let canon = src.canonicalize().ok();
    let mut dedup: Option<(String, u32, u32)> = None;
    for item in DB.scan_prefix(APP_PREFIX.as_bytes()) {
        let Ok((_, v)) = item else { continue };
        let Ok(m) = serde_json::from_slice::<Manifest>(&v) else { continue };
        if m.status.as_deref() != Some("draft") {
            continue;
        }
        if let Some(sd) = &m.src_dir {
            let same = sd == &dir || PathBuf::from(sd).canonicalize().ok() == canon;
            if same {
                dedup = Some((
                    m.id.clone(),
                    m.index.unwrap_or_else(next_app_index),
                    m.version.unwrap_or(1),
                ));
                break;
            }
        }
    }
    let (app_id, index, version) = match dedup {
        Some(v) => v,
        None => {
            let index = next_app_index();
            (derive_app_identity_internal(&secret, index)?.address, index, 1)
        }
    };
    ingest_app(
        &app, &secret, &app_id, index, name.trim(), description.trim(), &tags, &src, version,
        "draft", false, false, None, logo, preview, privacy,
    )
}

/// Cyberpunk index.html stub for a freshly bought domain (RU/EN). The page
/// greets the new owner and shows the domain it is bound to.
fn domain_draft_html(domain: &str) -> String {
    format!(r#"<!doctype html>
<html lang="ru">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>sth://{d}</title>
<style>
  *{{margin:0;box-sizing:border-box;font-family:'JetBrains Mono',ui-monospace,monospace}}
  body{{min-height:100vh;background:#05070a;color:#cdfff4;display:flex;align-items:center;justify-content:center;overflow:hidden}}
  .grid{{position:fixed;inset:0;background-image:linear-gradient(rgba(0,255,200,.05) 1px,transparent 1px),linear-gradient(90deg,rgba(0,255,200,.05) 1px,transparent 1px);background-size:38px 38px;animation:drift 18s linear infinite}}
  @keyframes drift{{to{{background-position:38px 38px}}}}
  .card{{position:relative;z-index:1;border:1px solid rgba(0,255,200,.35);border-radius:16px;padding:48px 40px;background:rgba(8,16,20,.85);backdrop-filter:blur(12px);box-shadow:0 0 60px -20px rgba(0,255,200,.5);max-width:560px;text-align:center}}
  .tag{{font-size:11px;letter-spacing:.4em;text-transform:uppercase;color:#00ffc8;opacity:.8}}
  h1{{margin:14px 0 6px;font-size:30px;font-weight:800;color:#7fd9cf;word-break:break-all;text-shadow:0 0 22px rgba(0,255,200,.45)}}
  .sub{{font-size:13px;line-height:1.7;color:#6f8c96;margin-top:14px}}
  .pulse{{display:inline-block;width:9px;height:9px;border-radius:50%;background:#00ffc8;margin-right:8px;animation:p 1.4s ease-in-out infinite;box-shadow:0 0 12px #00ffc8}}
  @keyframes p{{0%,100%{{opacity:.3}}50%{{opacity:1}}}}
  .en{{margin-top:18px;font-size:11px;color:#3f5560}}
</style>
</head>
<body>
  <div class="grid"></div>
  <main class="card">
    <div class="tag">SmartNet // dDNS</div>
    <h1>sth://{d}</h1>
    <p class="sub"><span class="pulse"></span>Домен ваш. Это черновик-заглушка — опубликуйте своё приложение в «Мои приложения», чтобы заменить эту страницу.</p>
    <p class="en">This domain is yours. Replace this placeholder by publishing your own dApp in "My Applications".</p>
  </main>
</body>
</html>"#, d = domain)
}

/// Phase 3c — auto-mint a DRAFT app for a just-bought domain, WITHOUT the user
/// picking a folder: writes a cyberpunk index.html stub into a temp dir, ingests
/// it as `status:draft`, and returns the new Manifest (its `id` == the new appId
/// the buyer can bind the domain to). Caller passes the returned id to atomic_buy.
#[tauri::command]
pub(crate) async fn create_domain_draft(
    app: tauri::AppHandle,
    domain: String,
) -> Result<Manifest, String> {
    tauri::async_runtime::spawn_blocking(move || create_domain_draft_blocking(app, domain))
        .await
        .map_err(|e| e.to_string())?
}

fn create_domain_draft_blocking(app: tauri::AppHandle, domain: String) -> Result<Manifest, String> {
    let secret = {
        let guard = SESSION_SEED.lock().unwrap();
        guard.as_ref().ok_or("locked")?.clone()
    };
    let clean = domain.trim().to_lowercase();
    if clean.is_empty() {
        return Err("domain required".to_string());
    }
    let display = clean.strip_suffix(".sth").unwrap_or(&clean).to_string();
    let index = next_app_index();
    let app_id = derive_app_identity_internal(&secret, index)?.address;

    // Stage the generated stub in a temp folder, then ingest + clean up.
    let tmp = ensure_data_dir(&app).join("tmp").join(&app_id);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    std::fs::write(tmp.join("index.html"), domain_draft_html(&display))
        .map_err(|e| e.to_string())?;

    let name = format!("{}.sth", display);
    let desc = format!("Купленный домен sth://{} — черновик / Purchased domain, draft", display);
    let result = ingest_app(
        &app, &secret, &app_id, index, &name, &desc, &[], &tmp, 1,
        "draft", false, false, None, None, None, None,
    );
    let _ = std::fs::remove_dir_all(&tmp);
    result
}


/// Finalize publishing a DRAFT: flip status to "published". `free=false` requires
/// a confirmed on-chain escrow payment (`paid=true` + `txid`); `free=true` keeps
/// the app local-only (NOT announced — shareable by direct link). Re-ingests from
/// the stored source folder so the published content is fresh.
#[tauri::command]
pub(crate) async fn finalize_publish(
    app: tauri::AppHandle,
    id: String,
    free: bool,
    txid: Option<String>,
) -> Result<Manifest, String> {
    tauri::async_runtime::spawn_blocking(move || finalize_publish_blocking(app, id, free, txid))
        .await
        .map_err(|e| e.to_string())?
}

fn finalize_publish_blocking(
    app: tauri::AppHandle,
    id: String,
    free: bool,
    txid: Option<String>,
) -> Result<Manifest, String> {
    let secret = {
        let guard = SESSION_SEED.lock().unwrap();
        guard.as_ref().ok_or("locked")?.clone()
    };
    let raw = DB
        .get(format!("{}{}", APP_PREFIX, id).as_bytes())
        .map_err(|e| e.to_string())?
        .ok_or("app not found")?;
    let existing: Manifest = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let index = existing.index.unwrap_or(0);
    if derive_app_identity_internal(&secret, index)?.address != id {
        return Err("identity mismatch".to_string());
    }
    let paid = !free;
    if paid && txid.as_deref().unwrap_or("").is_empty() {
        return Err("payment txid required".to_string());
    }
    let src = existing
        .src_dir
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .ok_or("Папка с исходными файлами приложения не найдена на диске — добавьте папку заново и пересоздайте черновик")?;
    let description = existing.description.clone().unwrap_or_default();
    // Приватный dApp: реальные метаданные из privMeta + ключи доступа из кэша
    let (name, description, tags, privacy) = match private_reingest_ctx(&existing)? {
        Some((n, d, t, p)) => (n, d, t, Some(p)),
        None => (existing.name.clone(), description, existing.tags.clone(), None),
    };
    let manifest = ingest_app(
        &app, &secret, &id, index, &name, &description, &tags, &src,
        existing.version.unwrap_or(1), "published", paid, free, txid, None, None, privacy,
    )?;
    // +250 к репутации автора за размещение платного dApp.
    if paid {
        if let Some(v) = load_vault_internal() {
            crate::reputation::add(&v.address, 250);
        }
    }
    Ok(manifest)
}

/// FREE metadata edit: update title / description / tags (+ optional new PNG/SVG
/// logo) of an existing app WITHOUT touching the file payload — merkle, version,
/// files and payment status are preserved. The announce signature is re-signed
/// over the new `id|name|description|merkle` so trackers accept the refreshed
/// metadata on the next heartbeat. No payment required (only file replacement is
/// a paid update). The stale seed tar is dropped so peers rebuild it with the
/// updated content.json + logo.
#[tauri::command]
pub(crate) async fn edit_app_metadata(
    app: tauri::AppHandle,
    id: String,
    name: String,
    description: String,
    tags: Vec<String>,
    logo: Option<Vec<u8>>,
    preview: Option<Vec<u8>>,
) -> Result<Manifest, String> {
    tauri::async_runtime::spawn_blocking(move || {
        edit_app_metadata_blocking(app, id, name, description, tags, logo, preview)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn edit_app_metadata_blocking(
    app: tauri::AppHandle,
    id: String,
    name: String,
    description: String,
    tags: Vec<String>,
    logo: Option<Vec<u8>>,
    preview: Option<Vec<u8>>,
) -> Result<Manifest, String> {
    let secret = {
        let guard = SESSION_SEED.lock().unwrap();
        guard.as_ref().ok_or("locked")?.clone()
    };
    let raw = DB
        .get(format!("{}{}", APP_PREFIX, id).as_bytes())
        .map_err(|e| e.to_string())?
        .ok_or("app not found")?;
    let mut manifest: Manifest = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let index = manifest.index.unwrap_or(0);
    if derive_app_identity_internal(&secret, index)?.address != id {
        return Err("identity mismatch".to_string());
    }
    let name = name.trim();
    let description = description.trim();
    if name.is_empty() {
        return Err("Заголовок обязателен".to_string());
    }

    // Приватный dApp: правим ЗАШИФРОВАННЫЕ метаданные (privMeta/privAnnounce);
    // публичный плейсхолдер не меняется, merkle пересчитывается с диска.
    if manifest.private == Some(true) {
        return edit_private_metadata(
            app,
            manifest,
            secret,
            index,
            name.to_string(),
            description.to_string(),
            tags,
            logo,
        );
    }

    let data_dir = ensure_data_dir(&app);
    let dest_root = data_dir.join("dapps").join(&id);

    // optional new logo — drop any previous one so only the fresh file remains
    if let Some(bytes) = logo.filter(|b| !b.is_empty()) {
        for n in [
            format!("{}-logo.png", id),
            format!("{}-logo.svg", id),
            "logo.png".to_string(),
            "logo.svg".to_string(),
        ] {
            let _ = std::fs::remove_file(dest_root.join(n));
        }
        let ext = logo_ext(&bytes);
        let _ = std::fs::write(dest_root.join(format!("{}-logo.{}", id, ext)), &bytes);
    }

    // optional new 440×180 preview — drop the previous one, write `<id>-preview.<ext>`
    if let Some(bytes) = preview.filter(|b| !b.is_empty()) {
        if bytes.len() <= crate::p2p::LOGO_MAX_BYTES {
            if let Some(ext) = preview_ext(&bytes) {
                for n in [format!("{}-preview.png", id), format!("{}-preview.jpg", id)] {
                    let _ = std::fs::remove_file(dest_root.join(n));
                }
                let _ = std::fs::write(dest_root.join(format!("{}-preview.{}", id, ext)), &bytes);
            }
        }
    }

    // v1.63.19: файловый payload на диске мог измениться (новый логотип
    // перезаписал/добавил файл, старые лого-файлы удалены) — без пересчёта
    // `manifest.files` + `manifest.merkle` `verify_app` даёт `file-hash-mismatch`
    // или `missing-file`, и UI показывает красный баннер «подпись content.json
    // НЕ совпадает с адресом приложения». Пересканируем dest_root и
    // пере-подписываем над НОВЫМ merkle. Делаем это ВСЕГДА (не только при
    // logo=Some) — так уже сломанный ранее dApp самолечится следующим free-
    // редактом текста, без требования заново загружать логотип. Для 300 МБ
    // это разовая цена в несколько секунд IO — приемлема при редком edit.
    let (rescanned_files, rescanned_merkle) = rescan_dest_files_and_merkle(&dest_root)
        .map_err(|e| format!("не удалось пересканировать файлы приложения: {e}"))?;
    manifest.files = rescanned_files;
    manifest.merkle = rescanned_merkle;

    // re-sign over id|name|description|merkle|owner (canon v2, merkle refreshed from disk)
    let digest = sha256(
        format!("{}|{}|{}|{}|{}", id, name, description, manifest.merkle, manifest.owner)
            .as_bytes(),
    );
    let (sk, app_pubkey, _) = keys_from_material(&format!("{}/1/{}", secret.trim(), index))?;
    let secp = Secp256k1::signing_only();
    let msg = Message::from_digest_slice(&digest).map_err(|e| e.to_string())?;
    let signature = hex::encode(secp.sign_ecdsa(&msg, &sk).serialize_compact());

    let mut clean_tags: Vec<String> = Vec::new();
    for tg in &tags {
        let t = tg.trim().to_lowercase();
        if !t.is_empty() && !clean_tags.contains(&t) {
            clean_tags.push(t);
        }
        if clean_tags.len() >= 8 {
            break;
        }
    }

    manifest.name = name.to_string();
    manifest.description = Some(description.to_string());
    manifest.tags = clean_tags;
    manifest.signature = signature;
    manifest.public_key = app_pubkey;
    manifest.updated = Some(now_iso());

    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let _ = std::fs::write(dest_root.join("content.json"), &json);
    let _ = std::fs::write(
        data_dir.join("manifests").join(format!("{}.json", id)),
        &json,
    );
    DB.insert(format!("{}{}", APP_PREFIX, id).as_bytes(), json)
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(
        data_dir
            .join("blobs")
            .join("seed")
            .join(format!("{}.tar", id)),
    );
    let _ = DB.flush();
    Ok(manifest)
}

/// FREE-редактирование метаданных ПРИВАТНОГО dApp: пере-шифрование
/// privMeta/privAnnounce тем же CEK, пересчёт merkle (access.json изменился),
/// re-sign canon v2 над плейсхолдером. Версия не меняется (как у публичного
/// free-edit); список доступа не трогается.
#[allow(clippy::too_many_arguments)]
fn edit_private_metadata(
    app: tauri::AppHandle,
    mut manifest: Manifest,
    secret: String,
    index: u32,
    name: String,
    description: String,
    tags: Vec<String>,
    logo: Option<Vec<u8>>,
) -> Result<Manifest, String> {
    use base64::Engine as _;
    let id = manifest.id.clone();
    let access = crate::private_dapp::load_access_db(&id).ok_or("access.json не найден")?;
    let cek = crate::private_dapp::unwrap_cek(&access)?;
    let mut meta = crate::private_dapp::decrypt_meta(&access)
        .ok_or("не удалось расшифровать метаданные приватного dApp")?;
    meta.name = name;
    meta.description = description;
    let mut clean_tags: Vec<String> = Vec::new();
    for tg in &tags {
        let t = tg.trim().to_lowercase();
        if !t.is_empty() && !clean_tags.contains(&t) {
            clean_tags.push(t);
        }
        if clean_tags.len() >= 8 {
            break;
        }
    }
    meta.tags = clean_tags;
    if let Some(bytes) = logo.filter(|b| !b.is_empty()) {
        meta.logo_ext = logo_ext(&bytes).to_string();
        meta.logo_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    }
    let new_access = crate::private_dapp::build_access(&cek, &access.pubkeys, &meta)?;
    let data_dir = ensure_data_dir(&app);
    let dest_root = data_dir.join("dapps").join(&id);
    crate::private_dapp::save_access(&dest_root, &id, &new_access)?;

    let (files, merkle) = rescan_dest_files_and_merkle(&dest_root)
        .map_err(|e| format!("не удалось пересканировать файлы приложения: {e}"))?;
    manifest.files = files;
    manifest.merkle = merkle;
    let pub_desc = manifest.description.clone().unwrap_or_default();
    let digest = sha256(
        format!("{}|{}|{}|{}|{}", id, manifest.name, pub_desc, manifest.merkle, manifest.owner)
            .as_bytes(),
    );
    let (sk, app_pubkey, _) = keys_from_material(&format!("{}/1/{}", secret.trim(), index))?;
    let secp = Secp256k1::signing_only();
    let msg = Message::from_digest_slice(&digest).map_err(|e| e.to_string())?;
    manifest.signature = hex::encode(secp.sign_ecdsa(&msg, &sk).serialize_compact());
    manifest.public_key = app_pubkey;
    manifest.updated = Some(now_iso());

    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let _ = std::fs::write(dest_root.join("content.json"), &json);
    let _ = std::fs::write(data_dir.join("manifests").join(format!("{}.json", id)), &json);
    DB.insert(format!("{}{}", APP_PREFIX, id).as_bytes(), json)
        .map_err(|e| e.to_string())?;
    let _ = DB.remove(format!("privreveal:{}", id).as_bytes());
    let _ = std::fs::remove_file(
        data_dir.join("blobs").join("seed").join(format!("{}.tar", id)),
    );
    let _ = DB.flush();
    let mut out = manifest;
    crate::private_dapp::decorate_manifest(&mut out);
    Ok(out)
}


/// Live Preview a draft (or any owned app) at ZERO cost: re-ingest from the dev
/// source folder (so edits show up) WITHOUT publishing/announcing, then the
/// caller opens sth://<id>. Returns the refreshed manifest.
#[tauri::command]
pub(crate) fn preview_draft(app: tauri::AppHandle, id: String) -> Result<Manifest, String> {
    let secret = {
        let guard = SESSION_SEED.lock().unwrap();
        guard.as_ref().ok_or("locked")?.clone()
    };
    let raw = DB
        .get(format!("{}{}", APP_PREFIX, id).as_bytes())
        .map_err(|e| e.to_string())?
        .ok_or("app not found")?;
    let existing: Manifest = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let index = existing.index.unwrap_or(0);
    let status = existing.status.clone().unwrap_or_else(|| "published".into());
    let paid = existing.paid.unwrap_or(false);
    let free = existing.free.unwrap_or(false);
    let description = existing.description.clone().unwrap_or_default();
    match existing.src_dir.as_deref().map(PathBuf::from).filter(|p| p.is_dir()) {
        // re-ingest from disk so live edits are reflected
        Some(src) => {
            let (name, description, tags, privacy) = match private_reingest_ctx(&existing)? {
                Some((n, d, t, p)) => (n, d, t, Some(p)),
                None => (existing.name.clone(), description, existing.tags.clone(), None),
            };
            ingest_app(
                &app, &secret, &id, index, &name, &description, &tags, &src,
                existing.version.unwrap_or(1), &status, paid, free, existing.escrow_txid.clone(),
                None, None, privacy,
            )
        }
        // no source on this machine — serve the already-ingested copy as-is
        None => Ok(existing),
    }
}

/// Return a dApp's logo as a base64 data URL. Looks for the canonical
/// `<id>-logo.svg|png` (new), then legacy `logo.svg|png` / `icon.png`. SVG is
/// served as `image/svg+xml`, raster as `image/png`. None ⇒ UI shows a badge.
#[tauri::command(async)]
pub(crate) fn app_logo(app: tauri::AppHandle, id: String) -> Option<String> {
    use base64::Engine;
    // Приватный dApp: логотип лежит в зашифрованных метаданных (не на диске)
    if crate::private_dapp::is_private(&id) {
        return crate::private_dapp::private_logo(&id);
    }
    let base = ensure_data_dir(&app).join("dapps").join(&id);
    let svg_names = [format!("{}-logo.svg", id), "logo.svg".to_string()];
    for name in svg_names {
        if let Ok(bytes) = std::fs::read(base.join(&name)) {
            if !bytes.is_empty() {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                return Some(format!("data:image/svg+xml;base64,{}", b64));
            }
        }
    }
    let png_names = [
        format!("{}-logo.png", id),
        "logo.png".to_string(),
        "logo.PNG".to_string(),
        "icon.png".to_string(),
    ];
    for name in png_names {
        if let Ok(bytes) = std::fs::read(base.join(&name)) {
            if !bytes.is_empty() {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                return Some(format!("data:image/png;base64,{}", b64));
            }
        }
    }
    None
}

/// Одноразовая запись в системный журнал (dedup по ключу) — диагностика
/// медиа карточек без спама при каждом опросе каталога.
fn log_once(key: String, msg: String) {
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    if seen.lock().unwrap().insert(key) {
        crate::syslog::log("dapp", msg);
    }
}

/// Локальный DAPP Preview 440×180 (`<id>-preview.png|jpg`) как base64 data URL.
/// None ⇒ карточка остаётся на градиентном fallback.
#[tauri::command(async)]
pub(crate) fn app_preview(app: tauri::AppHandle, id: String) -> Option<String> {
    use base64::Engine;
    let short: String = id.chars().take(12).collect();
    // приватные dApp: preview не сохраняется (visual identity скрыта)
    if crate::private_dapp::is_private(&id) {
        log_once(
            format!("pv:priv:{}", id),
            format!("app_preview {}… — private dApp, skip", short),
        );
        return None;
    }
    let base = ensure_data_dir(&app).join("dapps").join(&id);
    for (name, mime) in [
        (format!("{}-preview.png", id), "image/png"),
        (format!("{}-preview.jpg", id), "image/jpeg"),
    ] {
        if let Ok(bytes) = std::fs::read(base.join(&name)) {
            if !bytes.is_empty() {
                log_once(
                    format!("pv:ok:{}:{}:{}", id, name, bytes.len()),
                    format!("app_preview {}… — OK: {} ({} B)", short, name, bytes.len()),
                );
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                return Some(format!("data:{};base64,{}", mime, b64));
            }
        }
    }
    log_once(
        format!("pv:miss:{}", id),
        format!("app_preview {}… — файл не найден: {}", short, base.display()),
    );
    None
}

/// Update an EXISTING dApp in place (content-addressed): same address + signing key,
/// pick a new folder, re-hash, re-sign, bump version. The user selects the folder.
#[tauri::command]
pub(crate) async fn update_app(app: tauri::AppHandle, id: String) -> Result<Manifest, String> {
    let dir = app
        .dialog()
        .file()
        .blocking_pick_folder()
        .and_then(|fp| fp.into_path().ok())
        .ok_or("cancelled")?;
    update_app_at(app, id, dir.to_string_lossy().to_string(), None).await
}

/// Update an existing dApp from a KNOWN folder path (drag-and-drop / pre-priced
/// dynamic-payment update flow). Same immutable id + signing key; re-hash,
/// re-sign, version++. Re-ingests the supplied folder.
#[tauri::command]
pub(crate) async fn update_app_at(
    app: tauri::AppHandle,
    id: String,
    dir: String,
    txid: Option<String>,
) -> Result<Manifest, String> {
    tauri::async_runtime::spawn_blocking(move || update_app_at_blocking(app, id, dir, txid))
        .await
        .map_err(|e| e.to_string())?
}

fn update_app_at_blocking(
    app: tauri::AppHandle,
    id: String,
    dir: String,
    txid: Option<String>,
) -> Result<Manifest, String> {
    let secret = {
        let guard = SESSION_SEED.lock().unwrap();
        guard.as_ref().ok_or("locked")?.clone()
    };
    let raw = DB
        .get(format!("{}{}", APP_PREFIX, id).as_bytes())
        .map_err(|e| e.to_string())?
        .ok_or("app not found")?;
    let existing: Manifest = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let index = existing.index.unwrap_or(0);

    // Confirm the signing key still maps to the immutable app id.
    if derive_app_identity_internal(&secret, index)?.address != id {
        return Err("identity mismatch".to_string());
    }

    let src = PathBuf::from(dir.trim());
    if !src.is_dir() {
        return Err("source folder not found".to_string());
    }

    let next_version = existing.version.unwrap_or(1) + 1;
    let description = existing.description.clone().unwrap_or_default();
    // Замена файлов — всегда ОПЛАЧЕННОЕ обновление (динамический update fee),
    // поэтому бесплатное (link-only) приложение переходит в статус платного:
    // paid=true → ingest_app пишет announce:<id> → распространение по всем
    // правилам платных (mDNS a{i}, трекеры, headless-сидеры).
    let status = "published".to_string();
    let paid = true;
    let free = false;
    if existing.free == Some(true) {
        crate::syslog::log(
            "dapp",
            format!("update {}: free → paid (оплаченная замена файлов, v{})", id, next_version),
        );
    }
    let (name, description, tags, privacy) = match private_reingest_ctx(&existing)? {
        Some((n, d, t, p)) => (n, d, t, Some(p)),
        None => (existing.name.clone(), description, existing.tags.clone(), None),
    };
    ingest_app(
        &app,
        &secret,
        &id,
        index,
        &name,
        &description,
        &tags,
        &src,
        next_version,
        &status,
        paid,
        free,
        // txid платежа за обновление; при отсутствии — прежний escrow_txid
        txid.filter(|t| !t.trim().is_empty()).or(existing.escrow_txid.clone()),
        None,
        None,
        privacy,
    )
}

/// Read the README.md (if any) from a dApp's folder, for the ABOUT modal.
#[tauri::command(async)]
pub(crate) fn read_readme(app: tauri::AppHandle, id: String) -> Option<String> {
    let base = ensure_data_dir(&app).join("dapps").join(&id);
    for name in ["README.md", "readme.md", "Readme.md", "README.MD"] {
        if let Ok(s) = std::fs::read_to_string(base.join(name)) {
            return Some(s);
        }
    }
    None
}

/// Delete a published dApp: broadcast an OWNER-SIGNED tombstone to the trackers
/// (so they stop announcing it and headless seeders purge it), then remove it
/// locally — Sled registry + announce index, data/dapps folder, manifest file,
/// AND the seed/download blob tars under data/blobs. Normal peers keep their copy
/// until the user deletes it themselves.
#[tauri::command]
pub(crate) async fn delete_app(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || delete_app_blocking(app, id))
        .await
        .map_err(|e| e.to_string())??;
    // Debounced (500 ms) reclaim — coalesces bulk-uninstall bursts into ONE
    // seed_all() pass so the UI doesn't stutter between clicks. Fire-and-
    // forget: we return immediately, GC catches up on its own schedule.
    crate::p2p::schedule_reclaim_after_delete(&app2);
    Ok(())
}

fn delete_app_blocking(app: tauri::AppHandle, id: String) -> Result<(), String> {
    // Owner-signed tombstone broadcast (best-effort): sign sha256(id|delete|ts)
    // with this app's key and POST to every tracker.
    //
    // Robustness: use `.lock().ok()` NOT `.unwrap()` — if some prior task
    // panicked while holding SESSION_SEED, the mutex becomes poisoned and
    // every subsequent `.unwrap()` would re-panic, which propagates out of
    // `spawn_blocking` as a JoinError → the frontend receives a rejected
    // Promise and the UI does nothing (that was the exact "click Uninstall,
    // nothing happens" symptom the user hit). Poisoned lock → skip
    // broadcast, still do the local purge.
    let secret = SESSION_SEED
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .filter(|s| !s.trim().is_empty());
    if let Some(secret) = secret {
        // Owner-signed delete broadcast is fired off-thread so a slow /
        // unreachable tracker (5s timeout × N trackers) can't stall the UI —
        // the local purge below is what actually makes the app disappear
        // from the list, and it MUST return fast.
        if let Ok(Some(bytes)) = DB.get(format!("{}{}", APP_PREFIX, id).as_bytes()) {
            if let Ok(m) = serde_json::from_slice::<Manifest>(&bytes) {
                let index = m.index.unwrap_or(0);
                if let Ok((sk, pubkey, _)) =
                    keys_from_material(&format!("{}/1/{}", secret.trim(), index))
                {
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let digest = sha256(format!("{}|delete|{}", id, ts).as_bytes());
                    if let Ok(msg) = Message::from_digest_slice(&digest) {
                        let secp = Secp256k1::signing_only();
                        let signature =
                            hex::encode(secp.sign_ecdsa(&msg, &sk).serialize_compact());
                        let id_owned = id.clone();
                        let pk_owned = pubkey.clone();
                        std::thread::spawn(move || {
                            crate::p2p::broadcast_delete(&id_owned, &pk_owned, &signature, ts);
                        });
                    }
                }
            }
        }
    }

    // Local purge — this is what the user actually cares about (make the app
    // vanish from "Установленные" list). Runs unconditionally, even if we
    // could not sign a tombstone (foreign-owner installed apps have no seed
    // material derived from OUR wallet, so signing is expected to skip).
    let _ = DB.remove(format!("{}{}", APP_PREFIX, id).as_bytes());
    let _ = DB.remove(format!("announce:{}", id).as_bytes());
    let _ = DB.remove(format!("install_src:{}", id).as_bytes());
    // приватные dApps: выгрузить RAM-копию + стереть access/reveal-кэш
    crate::private_dapp::purge(&id);
    let _ = DB.flush();
    let data_dir = ensure_data_dir(&app);
    let _ = std::fs::remove_dir_all(data_dir.join("dapps").join(&id));
    let _ = std::fs::remove_file(data_dir.join("manifests").join(format!("{}.json", id)));
    let _ = std::fs::remove_file(data_dir.join("blobs").join("seed").join(format!("{}.tar", id)));
    let _ = std::fs::remove_file(
        data_dir
            .join("blobs")
            .join(format!("dl-{}.tar", sanitize(&id))),
    );
    Ok(())
}

/// Источник установки dApp: "p2p" (прямая iroh-раздача) или "seeder"
/// (облачный архив Cloud Seeder-а). Пишется при каждой установке/обновлении,
/// читается UI для бейджа во вкладке (☁ / ⇄).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallSource {
    kind: String,
    provider: Option<String>,
    uri: Option<String>,
    node: Option<String>,
    at: Option<String>,
    version: Option<u32>,
}

#[tauri::command]
pub(crate) fn app_install_source(id: String) -> Option<InstallSource> {
    let raw = DB.get(format!("install_src:{id}").as_bytes()).ok()??;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    Some(InstallSource {
        kind: v.get("kind")?.as_str()?.to_string(),
        provider: v.get("provider").and_then(|x| x.as_str()).map(String::from),
        uri: v.get("uri").and_then(|x| x.as_str()).map(String::from),
        node: v.get("node").and_then(|x| x.as_str()).map(String::from),
        at: v.get("at").and_then(|x| x.as_str()).map(String::from),
        version: v.get("version").and_then(|x| x.as_u64()).map(|n| n as u32),
    })
}

#[tauri::command]
pub(crate) fn list_apps() -> Vec<Manifest> {
    let mut out = Vec::new();
    for item in DB.scan_prefix(APP_PREFIX.as_bytes()) {
        if let Ok((_, v)) = item {
            if let Ok(m) = serde_json::from_slice::<Manifest>(&v) {
                out.push(m);
            }
        }
    }
    out
}

/// Gossip / tracker foundation — announced app ids for P2P seeding.
#[tauri::command(async)]
pub(crate) fn announced_apps() -> Vec<String> {
    crate::announced_apps_internal()
}

// ============================================================
//  content.json verification (content-addressed trust)
// ============================================================

#[derive(Debug, Serialize)]
pub(crate) struct VerifyResult {
    verified: bool,
    reason: String,
    version: Option<u32>,
}

#[allow(dead_code)]
fn verify_fail(reason: &str) -> VerifyResult {
    VerifyResult {
        verified: false,
        reason: reason.to_string(),
        version: None,
    }
}

/// Verify a dApp's `content.json` before/while opening `sth://<id>`:
///   1. re-hash every file on disk (SHA-256) and recompute the Blake3 merkle —
///      must match the manifest (content not tampered).
///   2. id must equal the SmartHoldem address derived from the signing pubkey
///      (the URL *is* the key — impersonation impossible).
///   3. the ECDSA signature over sha256(id|name|description|merkle) must be valid.
///
/// v1.63.20: on failure, `reason` carries the offending file path when
/// applicable (e.g. `missing-file:assets/logo.png`, `file-hash-mismatch:index.html`)
/// so the UI can show it in the tab tooltip. Every failure is ALSO written to
/// SystemConsole under `scope="dapp"` with the app id short-form + reason —
/// dApp devs open the console (bottom bar → SYSCON) and see exactly what's broken.
#[tauri::command(async)]
pub(crate) fn verify_app(app: tauri::AppHandle, id: String) -> VerifyResult {
    // Fold the console + return path so every early-exit logs a matching line.
    let fail = |reason: String| -> VerifyResult {
        let short = if id.len() > 12 { &id[..12] } else { &id[..] };
        crate::syslog::log("dapp", format!("verify {}… — FAIL: {}", short, reason));
        VerifyResult { verified: false, reason, version: None }
    };

    let base = ensure_data_dir(&app).join("dapps").join(&id);
    let raw = match std::fs::read(base.join("content.json")) {
        Ok(r) => r,
        Err(_) => return fail("no-content-json".to_string()),
    };
    let m: Manifest = match serde_json::from_slice(&raw) {
        Ok(m) => m,
        Err(_) => return fail("bad-manifest".to_string()),
    };
    if m.id != id {
        return fail(format!("id-mismatch:{}", m.id));
    }

    // 1. files on disk -> merkle. Missing file / hash-mismatch reasons carry
    // the offending path so devs can jump straight to it.
    let mut blake_parts: Vec<(String, String)> = Vec::new();
    for f in &m.files {
        let data = match std::fs::read(base.join(&f.path)) {
            Ok(d) => d,
            Err(_) => return fail(format!("missing-file:{}", f.path)),
        };
        if hex::encode(sha256(&data)) != f.sha256 {
            return fail(format!("file-hash-mismatch:{}", f.path));
        }
        blake_parts.push((f.path.clone(), blake3::hash(&data).to_hex().to_string()));
    }
    blake_parts.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = blake3::Hasher::new();
    for (p, h) in &blake_parts {
        hasher.update(p.as_bytes());
        hasher.update(h.as_bytes());
    }
    if hasher.finalize().to_hex().to_string() != m.merkle {
        return fail("merkle-mismatch".to_string());
    }

    // 2. id == address(pubkey)
    let pk_bytes = match hex::decode(&m.public_key) {
        Ok(b) => b,
        Err(_) => return fail("bad-pubkey".to_string()),
    };
    let pk = match PublicKey::from_slice(&pk_bytes) {
        Ok(p) => p,
        Err(_) => return fail("bad-pubkey".to_string()),
    };
    if address_from_pubkey(&pk.serialize()) != id {
        return fail("address-mismatch".to_string());
    }

    // 3. ECDSA подпись: канон v2 (…|owner) с fallback на легаси-канон без
    // owner — старые установленные dApps остаются валидными до ре-паблиша.
    let description = m.description.clone().unwrap_or_default();
    let sig = match hex::decode(&m.signature).ok().and_then(|b| Signature::from_compact(&b).ok()) {
        Some(s) => s,
        None => return fail("bad-signature".to_string()),
    };
    let secp = Secp256k1::verification_only();
    let check = |canon: String| -> bool {
        let digest = sha256(canon.as_bytes());
        match Message::from_digest_slice(&digest) {
            Ok(msg) => secp.verify_ecdsa(&msg, &sig, &pk).is_ok(),
            Err(_) => false,
        }
    };
    let v2 = format!("{}|{}|{}|{}|{}", id, m.name, description, m.merkle, m.owner);
    let v1 = format!("{}|{}|{}|{}", id, m.name, description, m.merkle);
    if !check(v2) && !check(v1) {
        return fail("bad-signature".to_string());
    }
    VerifyResult {
        verified: true,
        reason: "ok".to_string(),
        version: m.version,
    }
}
