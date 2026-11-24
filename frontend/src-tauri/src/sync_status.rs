// ============================================================
//  SmartNet // sync_status.rs
//
//  Tracks first-scan completion of the background blockchain indexers so the
//  frontend can show a "Синхронизация с сетью…" splash on a cold start (a fresh
//  device has empty sled, so the first full chain scan takes a while — without
//  feedback it looks frozen). All indexers run on their own threads; each marks
//  its stage done after the FIRST pass. Pure in-memory, no DB.
//
//  Author: TechnoL0g
//  Created: 2026-06-24
// ============================================================

use once_cell::sync::Lazy;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Registered up front so `total` is stable for a smooth progress bar.
static STAGES: Lazy<Mutex<BTreeMap<&'static str, bool>>> = Lazy::new(|| {
    let mut m = BTreeMap::new();
    for k in ["oracle", "naming", "dns", "subs", "chat"] {
        m.insert(k, false);
    }
    Mutex::new(m)
});

/// Mark an indexer's first scan complete (idempotent).
pub(crate) fn mark_done(stage: &'static str) {
    if let Ok(mut m) = STAGES.lock() {
        if let Some(v) = m.get_mut(stage) {
            *v = true;
        }
    }
}

#[derive(serde::Serialize)]
pub(crate) struct SyncStatus {
    pub done: usize,
    pub total: usize,
    pub complete: bool,
    pub pending: Vec<String>,
}

/// First-scan progress for the boot splash. Polled by the frontend until
/// `complete`.
#[tauri::command]
pub(crate) fn sync_status() -> SyncStatus {
    let m = STAGES.lock().unwrap();
    let total = m.len();
    let done = m.values().filter(|v| **v).count();
    let pending: Vec<String> = m.iter().filter(|(_, v)| !**v).map(|(k, _)| k.to_string()).collect();
    SyncStatus {
        done,
        total,
        complete: done == total,
        pending,
    }
}
