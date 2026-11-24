// ============================================================
//  SmartNet // profile.rs
//
//  Public developer / social identity layer. A `profile_manifest.json` file in
//  the data/ root holds three sections:
//    - public:      shareable identity (sth:// address), reward address,
//                   visibility flag, last-updated timestamp.
//    - private:     the AES-encrypted vault secret (never leaves the device in
//                   cleartext; exported only as an encrypted envelope).
//    - stats_cache: cheap, locally-computed counters (apps published / seeded).
//
//  No avatar uploads: the UI renders a DETERMINISTIC identicon from the
//  identity address, so this module stores no binary blobs.
//
//  All disk/crypto work is offloaded to `spawn_blocking` to keep the WebView2
//  UI thread responsive on Windows. No `panic!` / `unwrap()`.
//
//  Author: TechnoL0g
//  Created: 2026-06-20
// ============================================================

use crate::{ensure_data_dir, load_vault_internal, APP_PREFIX, DB};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ProfilePublic {
    pub identity: String,
    #[serde(rename = "rewardAddress")]
    pub reward_address: String,
    #[serde(rename = "isPublic")]
    pub is_public: bool,
    /// when true, the owner's `.sth` domains are shown on their public profile
    #[serde(rename = "showDomains", default)]
    pub show_domains: bool,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ProfilePrivate {
    #[serde(rename = "encryptedSecret")]
    pub encrypted_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ProfileStats {
    #[serde(rename = "appsPublished")]
    pub apps_published: u32,
    #[serde(rename = "appsSeeded")]
    pub apps_seeded: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ProfileManifest {
    pub public: ProfilePublic,
    pub private: ProfilePrivate,
    #[serde(rename = "statsCache")]
    pub stats_cache: ProfileStats,
}

fn manifest_path(app: &tauri::AppHandle) -> PathBuf {
    ensure_data_dir(app).join("profile_manifest.json")
}

/// Count registry-backed apps (published) and announced apps (seeded).
fn compute_stats() -> ProfileStats {
    let apps_published = DB.scan_prefix(APP_PREFIX.as_bytes()).count() as u32;
    let apps_seeded = DB.scan_prefix(b"announce:").count() as u32;
    ProfileStats {
        apps_published,
        apps_seeded,
    }
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // RFC3339-ish; the UI only displays it, so epoch-derived is fine.
    format!("{}", secs)
}

/// Read the manifest from disk, hydrating defaults from the unlocked vault when
/// the file is missing or the identity drifted. Always refreshes stats_cache.
fn load_blocking(app: tauri::AppHandle) -> ProfileManifest {
    let path = manifest_path(&app);
    let mut manifest: ProfileManifest = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();

    // Bind public identity + private secret to the current vault if present.
    if let Some(vault) = load_vault_internal() {
        if manifest.public.identity.is_empty() {
            manifest.public.identity = vault.address.clone();
        }
        if manifest.public.reward_address.is_empty() {
            manifest.public.reward_address = vault.address.clone();
        }
        manifest.private.encrypted_secret = vault.cipher.clone();
    }

    manifest.stats_cache = compute_stats();
    manifest
}

fn save_blocking(
    app: tauri::AppHandle,
    reward_address: String,
    is_public: bool,
    show_domains: bool,
) -> Result<ProfileManifest, String> {
    let mut manifest = load_blocking(app.clone());
    let reward = reward_address.trim();
    if !reward.is_empty() {
        manifest.public.reward_address = reward.to_string();
    }
    manifest.public.is_public = is_public;
    manifest.public.show_domains = show_domains;
    manifest.public.updated_at = now_iso();

    let path = manifest_path(&app);
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(manifest)
}

/// IPC: load (or lazily create) the local profile manifest.
#[tauri::command]
pub(crate) async fn load_profile_manifest(app: tauri::AppHandle) -> Result<ProfileManifest, String> {
    tauri::async_runtime::spawn_blocking(move || load_blocking(app))
        .await
        .map_err(|e| e.to_string())
}

/// IPC: persist the editable public config (reward address + visibility).
#[tauri::command]
pub(crate) async fn save_profile_config(
    app: tauri::AppHandle,
    reward_address: String,
    is_public: bool,
    show_domains: bool,
) -> Result<ProfileManifest, String> {
    tauri::async_runtime::spawn_blocking(move || {
        save_blocking(app, reward_address, is_public, show_domains)
    })
    .await
    .map_err(|e| e.to_string())?
}
