// ============================================================
//  SmartNet // oracle.rs
//
//  Blockchain Configuration Oracle + decentralized-store constants.
//  Global app prices (publish / update) are synced from on-chain admin
//  directives: a transaction from the ADMIN address to the CONFIG address
//  whose `vendorField` carries `appPublishPrice:<STH>` / `appUpdatePrice:<STH>`.
//  Newest values win; results are cached in the Sled DB so the UI/tracker can
//  read them instantly and offline. Defaults: publish = 100 STH, update = 1 STH.
//
//  Author: TechnoL0g
//  Project: SmartNet Core
//  Created: 2026-06-20
// ============================================================

use crate::DB;
use serde::Serialize;

/// Global Sapps escrow node — receives publish payments + seeding boosts.
/// Publish memo MUST be `sth:<app_id>`; boost memo `sth_boost:<app_id>`.
pub(crate) const ESCROW_ADDRESS: &str = "SappsPqDQXCRvoi1V2Hs9xycQeT1ptJWV1";
/// Admin wallet allowed to broadcast price directives (sender).
pub(crate) const ORACLE_SENDER: &str = "SeZLuyhhYf2qxs4ArPJ71oEu3x8EsVw51C";
/// Config wallet the price directives are sent to (recipient).
pub(crate) const ORACLE_RECIPIENT: &str = "SeTQeEAsHnHU1Y9EBjkRVNPB3fmUvfFUrk";

const DEFAULT_UPDATE_PRICE: u64 = 1;
/// Dynamic-pricing defaults — aligned with the live network governance tx
/// `sth_oracle:price_mb=2:base=10`. Total = base + round(MB) * price_per_mb.
const DEFAULT_BASE_PRICE: u64 = 10; // whole STH, flat base for any publish/update
const DEFAULT_PRICE_PER_MB: u64 = 2; // whole STH per (rounded) megabyte of payload
const KEY_UPDATE: &[u8] = b"cfg:app_update_price";
const KEY_BASE: &[u8] = b"cfg:app_base_price";
const KEY_PER_MB: &[u8] = b"cfg:app_price_per_mb";

/// 1 MiB in bytes — the unit the per-MB price is charged against.
pub(crate) const MIB: u64 = 1_048_576;

fn read_price(key: &[u8], default: u64) -> u64 {
    if let Ok(Some(v)) = DB.get(key) {
        if let Ok(s) = std::str::from_utf8(&v) {
            if let Ok(n) = s.trim().parse::<u64>() {
                return n;
            }
        }
    }
    default
}

fn write_price(key: &[u8], value: u64) {
    let _ = DB.insert(key, value.to_string().as_bytes());
    let _ = DB.flush();
}

pub(crate) fn update_price() -> u64 {
    read_price(KEY_UPDATE, DEFAULT_UPDATE_PRICE)
}

/// Flat base fee (whole STH) for publishing OR updating a dApp.
pub(crate) fn base_price() -> u64 {
    read_price(KEY_BASE, DEFAULT_BASE_PRICE)
}

/// Per-megabyte fee (whole STH) charged on the payload weight.
pub(crate) fn price_per_mb() -> u64 {
    read_price(KEY_PER_MB, DEFAULT_PRICE_PER_MB)
}

/// Convert a payload byte size to whole megabytes, rounded half-up so a 1.5 MB
/// app weighs 2 MB and anything under 0.5 MB weighs 0 (charged base only).
pub(crate) fn bytes_to_mb(size_bytes: u64) -> u64 {
    (size_bytes + MIB / 2) / MIB
}

/// UNIFIED dynamic pricing formula shared by publish AND update:
///   Total_Cost = base_price + (Size_In_MB * price_per_mb)   [whole STH]
pub(crate) fn compute_cost(size_bytes: u64) -> u64 {
    base_price().saturating_add(bytes_to_mb(size_bytes).saturating_mul(price_per_mb()))
}

#[derive(Debug, Serialize)]
pub(crate) struct AppPrices {
    /// Legacy alias kept for the UI — equals the dynamic base fee.
    publish: u64,
    /// Update price in whole STH (legacy flat — superseded by dynamic cost).
    update: u64,
    /// Flat base fee (whole STH) component of the dynamic formula.
    #[serde(rename = "basePrice")]
    base_price: u64,
    /// Per-MB fee (whole STH) component of the dynamic formula.
    #[serde(rename = "pricePerMb")]
    price_per_mb: u64,
    /// Global escrow node address (recipient for publish payments).
    escrow: String,
}

#[tauri::command]
pub(crate) fn get_app_prices() -> AppPrices {
    AppPrices {
        publish: base_price(),
        update: update_price(),
        base_price: base_price(),
        price_per_mb: price_per_mb(),
        escrow: ESCROW_ADDRESS.to_string(),
    }
}

/// Parse a single `key:<u64>` directive out of a vendorField. Returns the value
/// when the field starts with `prefix` and the remainder is a valid integer.
fn parse_directive(field: &str, prefix: &str) -> Option<u64> {
    field
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix(prefix).and_then(|v| v.trim().parse::<u64>().ok()))
}

/// Parse a dynamic-pricing governance directive of the form
/// `sth_oracle:price_mb=<v>:base=<v>` (also tolerates `price_mb==<v>`).
/// Returns `(base, price_per_mb)` — both required for the update to apply.
pub(crate) fn parse_oracle_directive(field: &str) -> Option<(u64, u64)> {
    let tok = field.split_whitespace().find(|t| t.starts_with("sth_oracle:"))?;
    let body = tok.strip_prefix("sth_oracle:")?;
    let mut base: Option<u64> = None;
    let mut per_mb: Option<u64> = None;
    for seg in body.split(':') {
        let mut kv = seg.splitn(2, '=');
        let key = kv.next().unwrap_or("").trim();
        // tolerate the `key==value` spelling by trimming a leading '='
        let val = kv.next().unwrap_or("").trim_start_matches('=').trim();
        match key {
            "base" => base = val.parse::<u64>().ok(),
            "price_mb" => per_mb = val.parse::<u64>().ok(),
            _ => {}
        }
    }
    match (base, per_mb) {
        (Some(b), Some(p)) => Some((b, p)),
        _ => None,
    }
}

/// One sync pass: read the newest admin→config transactions and apply the most
/// recent dynamic-pricing directive (`sth_oracle:price_mb=..:base=..`) plus the
/// legacy `appUpdatePrice:` if present. Newest-first; node errors leave cache.
pub(crate) fn sync_prices_once() {
    let fields = match crate::sth_node::fetch_vendor_fields(ORACLE_SENDER, ORACLE_RECIPIENT, 100) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut got_pricing = false;
    let mut got_update = false;
    for field in &fields {
        if !got_pricing {
            if let Some((base, per_mb)) = parse_oracle_directive(field) {
                write_price(KEY_BASE, base);
                write_price(KEY_PER_MB, per_mb);
                got_pricing = true;
            }
        }
        if !got_update {
            if let Some(v) = parse_directive(field, "appUpdatePrice:") {
                write_price(KEY_UPDATE, v);
                got_update = true;
            }
        }
        if got_pricing && got_update {
            break;
        }
    }
}

/// Background oracle: sync once on boot, then every 24h. Runs on a plain thread
/// (no async runtime) so it works in the default build profile too.
pub(crate) fn spawn_oracle_sync() {
    std::thread::spawn(|| {
        let mut first = true;
        loop {
            let online = crate::netwatch::is_online();
            if online {
                sync_prices_once();
            }
            if first {
                crate::sync_status::mark_done("oracle");
                first = false;
            }
            // offline: повторная попытка через 5 минут, иначе раз в сутки
            let secs = if online { 24 * 60 * 60 } else { 300 };
            std::thread::sleep(std::time::Duration::from_secs(secs));
        }
    });
}

const KEY_INCENTIVIZED: &[u8] = b"cfg:enable_incentivized_seeding";

/// Global monetization kill-switch (default ON). Persisted in the core Sled DB.
/// OFF ⇒ the network keeps serving (tracker exposes all apps, seeder skips
/// priority eviction, storage challenges pause) even if the chain is unavailable.
pub(crate) fn incentivized_enabled() -> bool {
    match DB.get(KEY_INCENTIVIZED) {
        Ok(Some(v)) => v.as_ref() != b"0",
        _ => true,
    }
}

#[tauri::command]
pub(crate) fn get_incentivized_seeding() -> bool {
    incentivized_enabled()
}

#[tauri::command]
pub(crate) fn set_incentivized_seeding(enabled: bool) -> Result<bool, String> {
    DB.insert(KEY_INCENTIVIZED, if enabled { b"1" as &[u8] } else { b"0" })
        .map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_oracle_directive() {
        assert_eq!(
            parse_oracle_directive("sth_oracle:price_mb=2:base=10"),
            Some((10, 2))
        );
        // order-independent + tolerant of the `==` spelling
        assert_eq!(
            parse_oracle_directive("sth_oracle:base=25:price_mb==4"),
            Some((25, 4))
        );
    }

    #[test]
    fn rejects_partial_or_foreign_directive() {
        assert_eq!(parse_oracle_directive("sth_oracle:base=10"), None);
        assert_eq!(parse_oracle_directive("appPublishPrice:100"), None);
        assert_eq!(parse_oracle_directive("hello world"), None);
    }

    #[test]
    fn bytes_to_mb_rounds_half_up() {
        assert_eq!(bytes_to_mb(0), 0);
        assert_eq!(bytes_to_mb(MIB / 2 - 1), 0); // just under 0.5 MB
        assert_eq!(bytes_to_mb(MIB / 2), 1); // exactly 0.5 MB rounds up
        assert_eq!(bytes_to_mb(MIB), 1);
        assert_eq!(bytes_to_mb(MIB + MIB / 2), 2); // 1.5 MB -> 2
    }
}
