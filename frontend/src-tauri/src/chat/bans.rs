// ============================================================
//  SmartNet // chat/bans.rs
//
//  The global group-moderation ban table: a single sled key (`chat:bans`)
//  holding a sliding-window-capped Vec<BanEntry>. Pure storage helpers
//  (load/save/prune/add/remove) extracted from chat.rs; the Tauri command
//  surface (ban_member/unban_member/list_bans/…) stays in chat.rs and calls
//  these. No command attributes here, so the items are freely `pub(crate)`.
//
//  Author: TechnoL0g
//  Created: 2026-06-24
// ============================================================

use crate::DB;
use serde::{Deserialize, Serialize};

pub(crate) const BANS_KEY: &[u8] = b"chat:bans";
pub(crate) const MAX_BANS: usize = 20000;

fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct BanEntry {
    pub group: String,
    pub addr: String,
    /// epoch-seconds ban-end (== the on-chain `ban:<until>:<group>` timestamp).
    pub until: u64,
    /// epoch-seconds insertion time — sliding-window eviction key.
    pub at: u64,
}

pub(crate) fn load_bans() -> Vec<BanEntry> {
    DB.get(BANS_KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Vec<BanEntry>>(&v).ok())
        .unwrap_or_default()
}
pub(crate) fn save_bans(v: &[BanEntry]) {
    if let Ok(j) = serde_json::to_vec(v) {
        let _ = DB.insert(BANS_KEY, j);
        let _ = DB.flush();
    }
}
/// Drop expired rows; if still over cap, evict the oldest by insertion time.
pub(crate) fn prune_bans(v: &mut Vec<BanEntry>) {
    let now = now_secs();
    v.retain(|b| b.until > now);
    if v.len() > MAX_BANS {
        v.sort_by_key(|b| b.at);
        let drop = v.len() - MAX_BANS;
        v.drain(0..drop);
    }
}
pub(crate) fn add_ban(group: &str, addr: &str, until: u64) {
    let mut v = load_bans();
    v.retain(|b| !(b.group == group && b.addr == addr)); // replace any existing
    v.push(BanEntry {
        group: group.into(),
        addr: addr.into(),
        until,
        at: now_secs(),
    });
    prune_bans(&mut v);
    save_bans(&v);
}
pub(crate) fn remove_ban(group: &str, addr: &str) {
    let mut v = load_bans();
    v.retain(|b| !(b.group == group && b.addr == addr));
    save_bans(&v);
}
