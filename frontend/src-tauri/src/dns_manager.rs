// ============================================================
//  SmartNet // dns_manager.rs  —  decentralized .sth Name Service (dDNS)
//
//  A domain is claimed by sending a Type-0 transfer to the DOMAIN sink address
//  with a strict, machine-readable vendorField:
//
//      reg:<name>:<zone>:<expires_unix>:<appId>
//      renew:<name>:<zone>:<expires_unix>:<appId>     (phase 2)
//
//  The blockchain `sender` becomes the immutable owner. A background indexer
//  scans the sink's incoming transfers (oldest-first → CHRONOLOGICAL FIRST-WIN),
//  validates the name + paid amount against the tiered price, and cross-links
//  everything in the local Sled DB so `sth://<name>.sth` resolves instantly &
//  offline to its target appId:
//
//      dns:name:<name>:<zone>  -> DomainRecord { name, zone, appId, owner, expires }
//      dns:rev:<appId>         -> "<name>.<zone>"   (primary domain for URL-bar)
//
//  Prices are admin-controlled on-chain (no hardcode lock-in): the admin posts
//  `PRICE_DOMAIN:sth:<v>` / `PRICE_SUBDOMAIN:sth:<v>` to the config sink; only
//  transfers from the admin address are honored.
//
//  Author: TechnoL0g
//  Created: 2026-06-23
// ============================================================

use crate::{load_vault_internal, sth_node, DB, SESSION_SEED};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// Registration / renewal sink. Every transfer here is a domain operation.
pub(crate) const DOMAIN_SINK: &str = "SPzdYYu3AiXGhViTkvV9Esog2QKFdomAin";
/// Marketplace order sink. Owners post sell-orders here; the indexer treats it
/// as the decentralized order book.
pub(crate) const MARKET_SINK: &str = "SeLLRk5qHwjG5hVpFhv1eTz1sMkPojmmmq";
/// Marketplace fee sink (0.5% of each sale).
pub(crate) const FEE_SINK: &str = "SdbuY92dW6xBwco2Dhh62ThpRAgnhKoLqW";
/// Marketplace commission, in basis points (0.5% = 50 bp).
const FEE_BP: u64 = 50;
/// Network config feed: the admin posts `PARAM:zone:VALUE` instructions here.
const CONFIG_SINK: &str = "SeTQeEAsHnHU1Y9EBjkRVNPB3fmUvfFUrk";
/// The ONLY address whose config transfers are trusted for price updates.
const CONFIG_ADMIN: &str = "SeZLuyhhYf2qxs4ArPJ71oEu3x8EsVw51C";

/// Default base domain fee (whole STH) until the on-chain config is indexed.
const DEFAULT_PRICE_DOMAIN: u64 = 500;
/// Default sub-domain fee (whole STH), flat for all lengths (phase 2).
const DEFAULT_PRICE_SUBDOMAIN: u64 = 10;
const PRICE_DOMAIN_KEY: &[u8] = b"cfg:param:price_domain";
const PRICE_SUBDOMAIN_KEY: &[u8] = b"cfg:param:price_subdomain";

/// Smarthoshi per 1 STH.
const SMARTHOSHI: u64 = 100_000_000;
/// How many ascending pages (×100 tx) of the sink history to scan per cycle.
const SCAN_PAGES: u32 = 25;
/// Only `.sth` is live today; the protocol is zone-generic for future zones.
const ZONE: &str = "sth";
/// Hard cap on a registration's lifetime regardless of the (client-supplied)
/// expiry embedded in the vendorField — blocks abusive far-future timestamps.
const MAX_DURATION_SECS: u64 = 366 * 86_400 + 2 * 86_400;

// ---------------------------------------------------------------------------
//  Model
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct DomainRecord {
    pub name: String,
    pub zone: String,
    #[serde(rename = "appId")]
    pub app_id: String,
    pub owner: String,
    /// Unix seconds when the registration expires.
    pub expires: u64,
}

#[derive(Serialize)]
pub(crate) struct DomainStatus {
    pub input: String,
    pub name: String,
    pub zone: String,
    /// Passes the character/length filter.
    pub valid: bool,
    /// Free to register right now (valid, not reserved, not actively held).
    pub available: bool,
    /// 1–2 char names: only DEVELOPERS may register them.
    #[serde(rename = "adminOnly")]
    pub admin_only: bool,
    /// `ok` | `invalid` | `reserved` | `taken`.
    pub reason: String,
    #[serde(rename = "priceSth")]
    pub price_sth: u64,
    /// The appId a taken name currently points at (else empty).
    #[serde(rename = "currentAppId")]
    pub current_app_id: String,
}

// ---------------------------------------------------------------------------
//  Keys / helpers
// ---------------------------------------------------------------------------

fn name_key(name: &str, zone: &str) -> String {
    format!("dns:name:{}:{}", name, zone)
}
fn rev_key(app_id: &str) -> String {
    format!("dns:rev:{}", app_id)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// System-reserved labels — loaded at compile-time from `frontend/public/reserved.json`
/// so the desktop client and the web preview share ONE source of truth. To edit
/// the list, update `reserved.json` and rebuild (the frontend picks up the same
/// file at runtime via `/reserved.json`). Wrapped in `Lazy` so parse cost hits
/// once on first lookup.
static RESERVED_JSON: &str = include_str!("../../public/reserved.json");
/// Большой список запрещённых имён (по одному в строке, общий для u://name и
/// sth://name) — загружается в сборку из `frontend/public/reserved.txt` и
/// хранится как HashSet для мгновенной O(1) проверки полных совпадений.
static RESERVED_TXT: &str = include_str!("../../public/reserved.txt");
static RESERVED_NAMES: once_cell::sync::Lazy<std::collections::HashSet<String>> =
    once_cell::sync::Lazy::new(|| {
        let mut set: std::collections::HashSet<String> =
            serde_json::from_str::<Vec<String>>(RESERVED_JSON)
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
        set.extend(
            RESERVED_TXT
                .lines()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty()),
        );
        set
    });

/// SmartNet core developers — the only addresses allowed to register a
/// reserved name (`admin.sth`, `smartnet.sth`, …). Mirrors the `.env`
/// `DEVELOPERS=…` list; kept in-sync via a compile-time constant so
/// off-chain UI (bridge.ts) and on-chain enforcement (this file) can't drift.
const DEVELOPERS: &[&str] = &[
    "SSU6TvycebBMTvc8WHiKTfG9xmX1QSniZu",
    "SeZLuyhhYf2qxs4ArPJ71oEu3x8EsVw51C",
];

fn is_reserved(name: &str) -> bool {
    RESERVED_NAMES.contains(name)
}

/// Общая проверка для реестра u://name (naming.rs): имя в списке reserved.
pub(crate) fn is_reserved_label(name: &str) -> bool {
    is_reserved(name)
}

/// True when the caller is one of the SmartNet developers — bypasses the
/// reserved-name gate so system labels can be minted by the core team.
/// Учитывает и compile-time список, и runtime `.env DEVELOPERS`.
pub(crate) fn is_developer(addr: &str) -> bool {
    if addr.is_empty() {
        return false;
    }
    DEVELOPERS.iter().any(|d| *d == addr)
        || crate::official::seed_developers().iter().any(|d| d == addr)
}

/// May `addr` register the reserved-or-free name `name`?
/// - Not reserved              → yes (anyone).
/// - Reserved AND developer    → yes.
/// - Reserved AND non-developer→ no.
fn can_register_reserved(name: &str, addr: &str) -> bool {
    !is_reserved(name) || is_developer(addr)
}

/// Validate + normalize a domain label: lowercase `a-z 0-9 -`, hyphen never at
/// the edges, length 1–63. Returns the canonical lowercase label or None.
pub(crate) fn validate_name(raw: &str) -> Option<String> {
    let name = raw.trim().to_lowercase();
    let len = name.chars().count();
    if len < 1 || len > 63 {
        return None;
    }
    if name.starts_with('-') || name.ends_with('-') {
        return None;
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return None;
    }
    Some(name)
}

/// Split a user-typed token (`explorer`, `explorer.sth`) into (label, zone).
/// Defaults the zone to `sth` when omitted.
fn split_name_zone(input: &str) -> (String, String) {
    let s = input.trim().to_lowercase();
    if let Some((n, z)) = s.rsplit_once('.') {
        (n.to_string(), z.to_string())
    } else {
        (s, ZONE.to_string())
    }
}

fn parse_u64_param(field: &str, prefix: &str) -> Option<u64> {
    field
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix(prefix).and_then(|v| v.trim().parse::<u64>().ok()))
}

/// Base domain price (whole STH): admin-indexed value, else compiled default.
pub(crate) fn base_domain_price() -> u64 {
    if let Ok(Some(v)) = DB.get(PRICE_DOMAIN_KEY) {
        if let Ok(s) = std::str::from_utf8(&v) {
            if let Ok(n) = s.trim().parse::<u64>() {
                if n > 0 {
                    return n;
                }
            }
        }
    }
    DEFAULT_PRICE_DOMAIN
}

/// Flat sub-domain price (whole STH) — phase 2.
pub(crate) fn subdomain_price() -> u64 {
    if let Ok(Some(v)) = DB.get(PRICE_SUBDOMAIN_KEY) {
        if let Ok(s) = std::str::from_utf8(&v) {
            if let Ok(n) = s.trim().parse::<u64>() {
                if n > 0 {
                    return n;
                }
            }
        }
    }
    DEFAULT_PRICE_SUBDOMAIN
}

/// Tiered price for a label of `len` chars given the `base` price.
///   1–2 chars → base (DEVELOPERS-only)   3 → ×100   4 → ×20   5+ → ×1
fn tier_price(len: usize, base: u64) -> u64 {
    match len {
        0 => base,
        1 | 2 => base,
        3 => base.saturating_mul(100),
        4 => base.saturating_mul(20),
        _ => base,
    }
}

fn is_admin_only(len: usize) -> bool {
    len <= 2
}

/// Grace window (seconds) around expiry during which only the owner may renew.
const GRACE_SECS: u64 = 30 * 86_400;

/// Renewal fee (whole STH): flat half of the base domain price, all lengths.
pub(crate) fn renew_price() -> u64 {
    (base_domain_price() / 2).max(1)
}

/// Read a record regardless of expiry (used for renewal / grace checks).
fn get_record(name: &str, zone: &str) -> Option<DomainRecord> {
    DB.get(name_key(name, zone).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
}

fn write_record(rec: &DomainRecord) {
    if let Ok(bytes) = serde_json::to_vec(rec) {
        let _ = DB.insert(name_key(&rec.name, &rec.zone).as_bytes(), bytes);
        let rk = rev_key(&rec.app_id);
        if DB.get(rk.as_bytes()).ok().flatten().is_none() {
            let _ = DB.insert(rk.as_bytes(), format!("{}.{}", rec.name, rec.zone).as_bytes());
        }
    }
}

/// True if the name has an active (non-expired) registration.
fn active_record(name: &str, zone: &str) -> Option<DomainRecord> {
    let rec: DomainRecord = DB
        .get(name_key(name, zone).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())?;
    if rec.expires > now_unix() {
        Some(rec)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
//  Indexing (blockchain → local registry)
// ---------------------------------------------------------------------------

/// Commit a record honoring chronological first-win: only writes if the name is
/// currently FREE (no record, or the previous one expired). Reserved labels are
/// rejected here too, so a raw on-chain tx bypassing the UI can never claim them
/// UNLESS the sender is a listed SmartNet developer (`DEVELOPERS`).
fn commit_record(rec: DomainRecord) -> bool {
    if !can_register_reserved(&rec.name, &rec.owner) {
        return false;
    }
    if active_record(&rec.name, &rec.zone).is_some() {
        return false; // first-win: an active holder already owns it
    }
    let bytes = match serde_json::to_vec(&rec) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let _ = DB.insert(name_key(&rec.name, &rec.zone).as_bytes(), bytes);
    // primary reverse mapping (for URL-bar display): keep the earliest claim
    let rk = rev_key(&rec.app_id);
    if DB.get(rk.as_bytes()).ok().flatten().is_none() {
        let _ = DB.insert(rk.as_bytes(), format!("{}.{}", rec.name, rec.zone).as_bytes());
    }
    true
}

/// Parse + validate a registration tx and (if it pays enough) commit it.
fn ingest_tx(sender: &str, amount_raw: u64, tx_time: u64, vendor: &str) {
    // Memo formats on the domain sink:
    //   reg:<name>:<zone>:<expires>:<appId>     (5 parts)
    //   renew:<name>:<zone>:<expires>:<appId>   (5 parts)
    //   buy:<name>:<zone>:<appId>               (4 parts — NO expires)
    let parts: Vec<&str> = vendor.split(':').collect();
    if parts.len() < 4 || parts.len() > 5 {
        return;
    }
    let kind = parts[0];
    let name = match validate_name(parts[1]) {
        Some(n) => n,
        None => return,
    };
    let zone = parts[2].trim().to_lowercase();
    if zone != ZONE {
        return;
    }
    if !can_register_reserved(&name, sender) {
        return;
    }
    // `buy:` carries the appId at index 3 and has no expires field; register/renew
    // carry expires at index 3 and the appId at index 4.
    let (field_exp, app_id) = if kind == "buy" {
        if parts.len() != 4 {
            return;
        }
        (0u64, parts[3].trim())
    } else {
        if parts.len() != 5 {
            return;
        }
        (parts[3].trim().parse::<u64>().unwrap_or(0), parts[4].trim())
    };
    if app_id.is_empty() {
        return;
    }

    if kind == "renew" {
        // Renewal: only the existing owner, within the grace window, extends.
        let mut rec = match get_record(&name, &zone) {
            Some(r) => r,
            None => return,
        };
        if rec.owner != sender {
            return;
        }
        if tx_time > rec.expires.saturating_add(GRACE_SECS) {
            return; // grace lapsed → name considered released
        }
        let required = renew_price().saturating_mul(SMARTHOSHI);
        if amount_raw < required {
            return;
        }
        let base_from = rec.expires.max(tx_time);
        let cap = base_from.saturating_add(MAX_DURATION_SECS);
        let new_exp = field_exp.min(cap).max(base_from);
        if new_exp <= rec.expires {
            return;
        }
        rec.expires = new_exp;
        rec.app_id = app_id.to_string();
        write_record(&rec);
        return;
    }

    if kind != "reg" {
        if kind == "buy" {
            transfer_on_buy(sender, tx_time, &name, &zone, app_id);
        }
        return;
    }
    let len = name.chars().count();
    if is_admin_only(len) && !is_developer(sender) {
        return; // Имена из 1–2 символов: только DEVELOPERS
    }
    // Paid-amount enforcement against the tiered price.
    let required = tier_price(len, base_domain_price()).saturating_mul(SMARTHOSHI);
    if amount_raw < required {
        return;
    }
    // Expiry: trust the client-supplied value but never exceed the hard cap.
    let cap = tx_time.saturating_add(MAX_DURATION_SECS);
    let expires = field_exp.min(cap);
    if expires <= tx_time {
        return; // already-expired / garbage timestamp
    }
    commit_record(DomainRecord {
        name,
        zone,
        app_id: app_id.to_string(),
        owner: sender.to_string(),
        expires,
    });
}

/// One indexer pass: scan the sink's incoming transfers oldest-first.
pub(crate) fn index_once() {
    for page in 1..=SCAN_PAGES {
        let path = format!(
            "/transactions?recipientId={}&page={}&limit=100&orderBy=timestamp:asc",
            DOMAIN_SINK,
            page
        );
        let txt = match sth_node::node_get(&path, std::time::Duration::from_secs(20)) {
            Ok(t) => t,
            Err(_) => break,
        };
        let v: Value = match serde_json::from_str(&txt) {
            Ok(v) => v,
            Err(_) => break,
        };
        let empty = vec![];
        let arr = v["data"].as_array().unwrap_or(&empty);
        if arr.is_empty() {
            break;
        }
        for t in arr {
            let sender = t["sender"].as_str().unwrap_or("");
            let vendor = t["vendorField"].as_str().unwrap_or("");
            if sender.is_empty() || vendor.is_empty() {
                continue;
            }
            let amount_raw: u64 = t["amount"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| t["amount"].as_u64())
                .unwrap_or(0);
            let tx_time = t["timestamp"]["unix"].as_u64().unwrap_or(0);
            ingest_tx(sender, amount_raw, tx_time, vendor);
        }
        if arr.len() < 100 {
            break;
        }
    }
    let _ = DB.flush();
}

/// One config pass: cache the newest admin-posted `PRICE_DOMAIN:sth:<v>` and
/// `PRICE_SUBDOMAIN:sth:<v>` directives (admin transfers only). Newest-first.
pub(crate) fn sync_price_once() {
    let fields = match sth_node::fetch_vendor_fields(CONFIG_ADMIN, CONFIG_SINK, 100) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut got_domain = false;
    let mut got_sub = false;
    for field in &fields {
        if !got_domain {
            if let Some(v) = parse_u64_param(field, "PRICE_DOMAIN:sth:") {
                let _ = DB.insert(PRICE_DOMAIN_KEY, v.to_string().as_bytes());
                got_domain = true;
            }
        }
        if !got_sub {
            if let Some(v) = parse_u64_param(field, "PRICE_SUBDOMAIN:sth:") {
                let _ = DB.insert(PRICE_SUBDOMAIN_KEY, v.to_string().as_bytes());
                got_sub = true;
            }
        }
        if got_domain && got_sub {
            break;
        }
    }
    let _ = DB.flush();
}

/// Background loop: index domains frequently; refresh prices every ~4 hours.
pub(crate) fn spawn_indexer() {
    std::thread::spawn(|| {
        let mut last_price = 0u64;
        let mut first = true;
        loop {
            // offline: пропускаем сетевой тик — иначе запросы виснут в таймаутах
            if crate::netwatch::is_online() {
                let now = now_unix();
                if now.saturating_sub(last_price) >= 4 * 3600 {
                    sync_price_once();
                    last_price = now;
                }
                index_once();
                index_orders_once();
            }
            if first {
                crate::sync_status::mark_done("dns");
                first = false;
            }
            std::thread::sleep(std::time::Duration::from_secs(90));
        }
    });
}

// ---------------------------------------------------------------------------
//  Marketplace — sell orders (Phase 3a)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct SaleOrder {
    pub name: String,
    pub zone: String,
    pub seller: String,
    #[serde(rename = "priceSth")]
    pub price_sth: u64,
    /// Offer expiry (unix). Never later than the domain's registration expiry.
    pub expires: u64,
    #[serde(rename = "originalAppId")]
    pub original_app_id: String,
}

fn order_key(name: &str, zone: &str) -> String {
    format!("dns:order:{}:{}", name, zone)
}

/// True when an order is still valid: not expired AND the domain is still owned
/// by the seller (a transfer/expiry invalidates the listing).
fn order_is_live(o: &SaleOrder) -> bool {
    if o.expires <= now_unix() {
        return false;
    }
    match get_record(&o.name, &o.zone) {
        Some(rec) => rec.owner == o.seller && rec.app_id == o.original_app_id,
        None => false,
    }
}

fn get_order(name: &str, zone: &str) -> Option<SaleOrder> {
    DB.get(order_key(name, zone).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
}

/// Ingest one sell-order tx: `<sellerId>:<name>:<price>:<expires>`. Only the
/// current domain owner may list it, and the offer can't outlive registration.
fn ingest_order(sender: &str, tx_time: u64, vendor: &str) {
    let parts: Vec<&str> = vendor.split(':').collect();
    if parts.len() != 4 {
        return;
    }
    if parts[0] != sender {
        return; // seller field must match the actual sender
    }
    let name = match validate_name(parts[1]) {
        Some(n) => n,
        None => return,
    };
    let price: u64 = parts[2].trim().parse().unwrap_or(0);
    if price == 0 {
        return;
    }
    let expires: u64 = parts[3].trim().parse().unwrap_or(0);
    if expires <= tx_time {
        return;
    }
    let rec = match get_record(&name, ZONE) {
        Some(r) => r,
        None => return,
    };
    if rec.owner != sender {
        return; // only the owner can list
    }
    if expires > rec.expires {
        return; // offer can't outlive the registration
    }
    let order = SaleOrder {
        name,
        zone: ZONE.to_string(),
        seller: sender.to_string(),
        price_sth: price,
        expires,
        original_app_id: rec.app_id,
    };
    if let Ok(bytes) = serde_json::to_vec(&order) {
        let _ = DB.insert(order_key(&order.name, &order.zone).as_bytes(), bytes);
    }
}

/// Scan the market sink (oldest-first) and refresh the local order book.
pub(crate) fn index_orders_once() {
    for page in 1..=SCAN_PAGES {
        let path = format!(
            "/transactions?recipientId={}&page={}&limit=100&orderBy=timestamp:asc",
            MARKET_SINK,
            page
        );
        let txt = match sth_node::node_get(&path, std::time::Duration::from_secs(20)) {
            Ok(t) => t,
            Err(_) => break,
        };
        let v: Value = match serde_json::from_str(&txt) {
            Ok(v) => v,
            Err(_) => break,
        };
        let empty = vec![];
        let arr = v["data"].as_array().unwrap_or(&empty);
        if arr.is_empty() {
            break;
        }
        for t in arr {
            let sender = t["sender"].as_str().unwrap_or("");
            let vendor = t["vendorField"].as_str().unwrap_or("");
            if sender.is_empty() || vendor.is_empty() {
                continue;
            }
            let tx_time = t["timestamp"]["unix"].as_u64().unwrap_or(0);
            ingest_order(sender, tx_time, vendor);
        }
        if arr.len() < 100 {
            break;
        }
    }
    let _ = DB.flush();
}

/// List a domain you own for sale. Posts `<seller>:<name>:<price>:<expires>` to
/// the market sink and optimistically writes the local order.
#[tauri::command]
pub(crate) async fn list_sell(
    name: String,
    price_sth: u64,
    expires: u64,
) -> Result<String, String> {
    let (raw_name, _zone) = split_name_zone(&name);
    let clean = validate_name(&raw_name).ok_or("Недопустимое имя")?;
    if price_sth == 0 {
        return Err("Укажите цену".into());
    }
    let rec = get_record(&clean, ZONE).ok_or("Домен не найден")?;
    let addr = load_vault_internal().map(|v| v.address).ok_or("locked")?;
    if rec.owner != addr {
        return Err("Продавать может только владелец домена".into());
    }
    let now = now_unix();
    if rec.expires <= now {
        return Err("Срок регистрации домена истёк".into());
    }
    if expires <= now || expires > rec.expires {
        return Err("Срок оффера не может превышать срок регистрации домена".into());
    }
    let vendor = format!("{}:{}:{}:{}", addr, clean, price_sth, expires);
    let secret = SESSION_SEED.lock().unwrap().clone().ok_or("locked")?;
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        sth_node::send_transfer(&secret, MARKET_SINK, "0.00000001", "", &vendor)
    })
    .await
    .map_err(|e| e.to_string())??;
    if !outcome.accepted {
        return Err("Транзакция отклонена узлом".into());
    }
    let order = SaleOrder {
        name: clean,
        zone: ZONE.to_string(),
        seller: addr,
        price_sth,
        expires,
        original_app_id: rec.app_id,
    };
    if let Ok(bytes) = serde_json::to_vec(&order) {
        let _ = DB.insert(order_key(&order.name, &order.zone).as_bytes(), bytes);
        let _ = DB.flush();
    }
    Ok(outcome.id)
}

/// The live sell-order for a domain (if any), for the owner's card badge.
#[tauri::command]
pub(crate) fn domain_order(name: String) -> Option<SaleOrder> {
    let (raw_name, _zone) = split_name_zone(&name);
    let clean = validate_name(&raw_name)?;
    let o = get_order(&clean, ZONE)?;
    if order_is_live(&o) {
        Some(o)
    } else {
        None
    }
}

/// All live sell-orders (the marketplace listing source).
#[tauri::command(async)]
pub(crate) fn list_market_orders() -> Vec<SaleOrder> {
    let mut out = Vec::new();
    for kv in DB.scan_prefix(b"dns:order:") {
        if let Ok((_, v)) = kv {
            if let Ok(o) = serde_json::from_slice::<SaleOrder>(&v) {
                if order_is_live(&o) {
                    out.push(o);
                }
            }
        }
    }
    out.sort_by(|a, b| a.price_sth.cmp(&b.price_sth));
    out
}

/// 0.5% commission (whole-STH order price) split, returned in smarthoshi.
fn fee_split(price_sth: u64) -> (u64, u64) {
    let total = price_sth.saturating_mul(SMARTHOSHI);
    let fee = total.saturating_mul(FEE_BP) / 10_000;
    (fee, total.saturating_sub(fee))
}

/// On-chain `vendorField` is length-capped (SmartHoldem truncates to ~64 bytes),
/// so a longer memo is stored truncated. Match the truncated on-chain value
/// against the full expected memo: exact, or a sufficiently-long prefix of it.
/// This makes the buy verification robust to truncation (the `buydom:` leg —
/// `buydom:<seller>:<buyer>:<name>` — easily exceeds 64 bytes and was silently
/// truncated, which previously broke ownership transfer after a paid purchase).
fn memo_matches(onchain: &str, expected: &str) -> bool {
    onchain == expected
        || (onchain.len() >= 32 && expected.len() > onchain.len() && expected.starts_with(onchain))
}

/// Verify on-chain that `buyer` actually paid for `order`: the 0.5% fee to the
/// fee sink (`buydom:`) AND ≥99.5% to the seller (`selldom:`). Returns true only
/// when BOTH legs are present — otherwise the ownership transfer (tx #3) is
/// ignored, blocking a buyer who tries to claim a name without paying.
fn verify_buy_payment(buyer: &str, name: &str, order: &SaleOrder) -> bool {
    let (fee_raw, seller_raw) = fee_split(order.price_sth);
    let fee_min = fee_raw as f64 / SMARTHOSHI as f64;
    let seller_min = seller_raw as f64 / SMARTHOSHI as f64;
    // New compact memos (always fit the vendorField cap). Buyer is implied by the
    // tx sender, the counterparties are fixed (fee sink / seller).
    let buydom_new = format!("buydom:{}:{}", name, order.zone);
    let selldom_new = format!("selldom:{}:{}", name, order.zone);
    // Legacy long memos (pre-fix) — kept for backward compatibility with
    // already-broadcast purchases; matched truncation-tolerantly.
    let buydom_old = format!("buydom:{}:{}:{}", order.seller, buyer, name);
    let selldom_old = format!("selldom:{}:{}", buyer, name);
    let hist = sth_node::fetch_history_cached(buyer);
    let mut paid_fee = false;
    let mut paid_seller = false;
    for h in &hist {
        if h.kind != "out" {
            continue;
        }
        let amt = h.amount.parse::<f64>().unwrap_or(0.0);
        if !paid_fee
            && h.counterparty == FEE_SINK
            && (memo_matches(&h.memo, &buydom_new) || memo_matches(&h.memo, &buydom_old))
            && amt + 1e-9 >= fee_min
        {
            paid_fee = true;
        }
        if !paid_seller
            && h.counterparty == order.seller
            && (memo_matches(&h.memo, &selldom_new) || memo_matches(&h.memo, &selldom_old))
            && amt + 1e-9 >= seller_min
        {
            paid_seller = true;
        }
        if paid_fee && paid_seller {
            return true;
        }
    }
    paid_fee && paid_seller
}

/// Honor a `buy:<name>:<zone>:<newAppId>` claim ONLY when the buyer has paid the
/// seller + fee for the active order. Transfers ownership, clears the order and
/// records the deal.
fn transfer_on_buy(buyer: &str, tx_time: u64, name: &str, zone: &str, new_app_id: &str) {
    let mut rec = match get_record(name, zone) {
        Some(r) => r,
        None => return,
    };
    let order = match get_order(name, zone) {
        Some(o) => o,
        None => return, // no active listing → nothing to buy
    };
    if order.seller != rec.owner || order.original_app_id != rec.app_id {
        return; // listing no longer matches the record
    }
    if tx_time > order.expires {
        return; // offer already expired
    }
    if buyer == order.seller {
        return;
    }
    if !verify_buy_payment(buyer, name, &order) {
        return; // payment legs (fee + seller) not both present → ignore claim
    }
    let old_app = rec.app_id.clone();
    rec.owner = buyer.to_string();
    if !new_app_id.is_empty() {
        rec.app_id = new_app_id.to_string();
    }
    write_record(&rec);
    let _ = DB.remove(order_key(name, zone).as_bytes());
    // Append to the immutable deal history (for future price charts).
    let deal = serde_json::json!({
        "name": name, "zone": zone, "seller": order.seller, "buyer": buyer,
        "oldAppId": old_app, "newAppId": rec.app_id, "priceSth": order.price_sth, "ts": tx_time,
    });
    if let Ok(bytes) = serde_json::to_vec(&deal) {
        let _ = DB.insert(format!("dns:deal:{}:{}", tx_time, name).as_bytes(), bytes);
    }
    let _ = DB.flush();
}

/// Atomic 3-transaction purchase of a listed domain. Pre-checks the live order,
/// then broadcasts: (1) 0.5% fee → fee sink, (2) ≥99.5% → seller, (3) claim →
/// domain sink. Nonces auto-chain via `send_transfer`. Ownership only changes
/// once the indexer re-validates all three legs on-chain.
#[tauri::command]
pub(crate) async fn atomic_buy(
    name: String,
    expected_price: u64,
    new_app_id: String,
) -> Result<Vec<String>, String> {
    let (raw_name, _zone) = split_name_zone(&name);
    let clean = validate_name(&raw_name).ok_or("Недопустимое имя")?;
    let order = get_order(&clean, ZONE).filter(order_is_live).ok_or("Ордер не найден или снят")?;
    if order.price_sth != expected_price {
        return Err("Условия продажи изменились, обновите страницу".into());
    }
    let now = now_unix();
    if order.expires <= now + 3600 {
        return Err("До истечения оффера менее 1 часа — покупка недоступна".into());
    }
    let buyer = load_vault_internal().map(|v| v.address).ok_or("locked")?;
    if buyer == order.seller {
        return Err("Нельзя купить собственный домен".into());
    }
    let app_for = if new_app_id.trim().is_empty() { buyer.clone() } else { new_app_id.trim().to_string() };
    let (fee_raw, seller_raw) = fee_split(order.price_sth);
    let fee_amt = sth_node::smarthoshi_to_sth(fee_raw);
    let seller_amt = sth_node::smarthoshi_to_sth(seller_raw);
    let seller = order.seller.clone();
    let nm = clean.clone();

    let secret = SESSION_SEED.lock().unwrap().clone().ok_or("locked")?;
    let outcome = tauri::async_runtime::spawn_blocking(move || -> Result<Vec<String>, String> {
        let mut ids = Vec::new();
        // tx1 — commission. Short memo (`buydom:<name>:<zone>`) so it always fits
        // the on-chain vendorField cap (~64 bytes); the buyer is implied by the
        // tx sender and the fee sink is the fixed recipient.
        let v1 = format!("buydom:{}:{}", nm, ZONE);
        let t1 = sth_node::send_transfer(&secret, FEE_SINK, &fee_amt, "", &v1)?;
        if !t1.accepted {
            return Err("Комиссионная транзакция отклонена".into());
        }
        ids.push(t1.id);
        // tx2 — seller payout. Short memo (`selldom:<name>:<zone>`).
        let v2 = format!("selldom:{}:{}", nm, ZONE);
        let t2 = sth_node::send_transfer(&secret, &seller, &seller_amt, "", &v2)?;
        if !t2.accepted {
            return Err("Транзакция продавцу отклонена".into());
        }
        ids.push(t2.id);
        // tx3 — ownership claim
        let v3 = format!("buy:{}:{}:{}", nm, ZONE, app_for);
        let t3 = sth_node::send_transfer(&secret, DOMAIN_SINK, "0.00000001", "", &v3)?;
        if !t3.accepted {
            return Err("Транзакция передачи имени отклонена".into());
        }
        ids.push(t3.id);
        Ok(ids)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(outcome)
}

// ---------------------------------------------------------------------------
//  IPC commands
// ---------------------------------------------------------------------------

/// Resolve `explorer` / `explorer.sth` → the target appId (active records only).
#[tauri::command]
pub(crate) fn resolve_domain(name: String) -> Option<String> {
    let (n, z) = split_name_zone(&name);
    let clean = validate_name(&n)?;
    active_record(&clean, &z).map(|r| r.app_id)
}

/// The primary registered domain for an appId, for URL-bar display (`name.sth`).
#[tauri::command]
pub(crate) fn domain_of_app(app_id: String) -> Option<String> {
    let label = DB
        .get(rev_key(&app_id).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())?;
    let (n, z) = split_name_zone(&label);
    // Only surface it if the mapping is still active AND still points here.
    let rec = active_record(&n, &z)?;
    if rec.app_id == app_id {
        Some(label)
    } else {
        None
    }
}

/// Live lookup powering the .sth registration field (validity / availability /
/// tiered price / current holder).
#[tauri::command]
pub(crate) fn domain_lookup(name: String) -> DomainStatus {
    let (raw_name, zone) = split_name_zone(&name);
    let mut st = DomainStatus {
        input: name.clone(),
        name: raw_name.clone(),
        zone: zone.clone(),
        valid: false,
        available: false,
        admin_only: false,
        reason: "invalid".into(),
        price_sth: 0,
        current_app_id: String::new(),
    };
    let clean = match validate_name(&raw_name) {
        Some(c) => c,
        None => return st,
    };
    if zone != ZONE {
        return st;
    }
    st.valid = true;
    st.name = clean.clone();
    let len = clean.chars().count();
    st.admin_only = is_admin_only(len);
    st.price_sth = tier_price(len, base_domain_price());
    // Reserved labels и 1–2-символьные имена: скрываем от обычных пользователей;
    // разработчики (DEVELOPERS) видят их как `ok`, чтобы UI отрисовал кнопку
    // регистрации (register_domain всё равно перепроверяет как belt-and-braces).
    if is_reserved(&clean) || st.admin_only {
        let caller = load_vault_internal().map(|v| v.address).unwrap_or_default();
        if !is_developer(&caller) {
            st.reason = "reserved".into();
            return st;
        }
    }
    if let Some(rec) = active_record(&clean, &zone) {
        st.reason = "taken".into();
        st.current_app_id = rec.app_id;
        return st;
    }
    st.available = true;
    st.reason = "ok".into();
    st
}

/// Register a `.sth` domain: validates locally, then broadcasts a Type-0
/// transfer to the DOMAIN sink carrying `reg:<name>:<zone>:<expires>:<appId>`
/// and the tiered fee. Optimistically commits on acceptance so it resolves
/// immediately; the indexer reaffirms from the chain.
#[tauri::command]
pub(crate) async fn register_domain(
    name: String,
    app_id: String,
    duration_days: u64,
) -> Result<String, String> {
    let (raw_name, zone) = split_name_zone(&name);
    if zone != ZONE {
        return Err("Поддерживается только зона .sth".into());
    }
    let clean = validate_name(&raw_name)
        .ok_or("Недопустимое имя: a-z 0-9 и дефис (не с краю), 1–63 символа")?;
    if app_id.trim().is_empty() {
        return Err("Не указан appId приложения".into());
    }
    if active_record(&clean, &zone).is_some() {
        return Err("Имя уже занято".into());
    }
    let addr = load_vault_internal().map(|v| v.address).ok_or("locked")?;
    if !can_register_reserved(&clean, &addr) {
        return Err("RESERVED — Пожалуйста выберите другое имя для регистрации!".into());
    }
    let len = clean.chars().count();
    if is_admin_only(len) && !is_developer(&addr) {
        return Err("RESERVED — имена из 1–2 символов доступны только разработчикам сети".into());
    }
    let price = tier_price(len, base_domain_price());
    let days = duration_days.clamp(1, 366);
    let expires = now_unix().saturating_add(days.saturating_mul(86_400));
    let vendor = format!("reg:{}:{}:{}:{}", clean, zone, expires, app_id.trim());

    let secret = SESSION_SEED.lock().unwrap().clone().ok_or("locked")?;
    let amount = price.to_string();
    let app_for_commit = app_id.trim().to_string();
    let name_for_commit = clean.clone();
    let zone_for_commit = zone.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        sth_node::send_transfer(&secret, DOMAIN_SINK, &amount, "", &vendor)
    })
    .await
    .map_err(|e| e.to_string())??;
    if !outcome.accepted {
        return Err("Транзакция отклонена узлом".into());
    }
    // Optimistic local commit → resolves right away (first-win safe).
    commit_record(DomainRecord {
        name: name_for_commit,
        zone: zone_for_commit,
        app_id: app_for_commit,
        owner: addr,
        expires,
    });
    let _ = DB.flush();
    Ok(outcome.id)
}

/// All active domains owned by the currently-unlocked wallet.
#[tauri::command(async)]
pub(crate) fn list_my_domains() -> Vec<DomainRecord> {
    let addr = match load_vault_internal() {
        Some(v) => v.address,
        None => return vec![],
    };
    let now = now_unix();
    let mut out = Vec::new();
    for kv in DB.scan_prefix(b"dns:name:") {
        if let Ok((_, v)) = kv {
            if let Ok(rec) = serde_json::from_slice::<DomainRecord>(&v) {
                // include grace-period domains so the owner can still renew them
                if rec.owner == addr && rec.expires.saturating_add(GRACE_SECS) > now {
                    out.push(rec);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// All live `.sth` domains owned by `address` (from the local dDNS index).
/// Used to render another user's domains on their public profile.
#[tauri::command(async)]
pub(crate) fn domains_by_owner(address: String) -> Vec<DomainRecord> {
    let now = now_unix();
    let mut out = Vec::new();
    for kv in DB.scan_prefix(b"dns:name:") {
        if let Ok((_, v)) = kv {
            if let Ok(rec) = serde_json::from_slice::<DomainRecord>(&v) {
                if rec.owner == address && rec.expires.saturating_add(GRACE_SECS) > now {
                    out.push(rec);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Flat renewal fee (whole STH) for the UI.
#[tauri::command]
pub(crate) fn renewal_price() -> u64 {
    renew_price()
}

/// Renew a domain you own. Allowed in the window [expires − 30d, expires + 30d].
/// Broadcasts `renew:<name>:<zone>:<new_expires>:<appId>` with the half fee and
/// optimistically extends the local record.
#[tauri::command]
pub(crate) async fn renew_domain(
    name: String,
    app_id: String,
    duration_days: u64,
) -> Result<String, String> {
    let (raw_name, zone) = split_name_zone(&name);
    let clean = validate_name(&raw_name).ok_or("Недопустимое имя")?;
    let mut rec = get_record(&clean, &zone).ok_or("Домен не найден")?;
    let addr = load_vault_internal().map(|v| v.address).ok_or("locked")?;
    if rec.owner != addr {
        return Err("Продлить может только владелец домена".into());
    }
    let now = now_unix();
    if now < rec.expires.saturating_sub(GRACE_SECS) {
        return Err("Продление доступно за 30 дней до истечения срока".into());
    }
    if now > rec.expires.saturating_add(GRACE_SECS) {
        return Err("Льготный период истёк — имя освободилось".into());
    }
    let price = renew_price();
    let days = duration_days.clamp(1, 366);
    let base_from = rec.expires.max(now);
    let new_exp = base_from.saturating_add(days.saturating_mul(86_400));
    let app_for = if app_id.trim().is_empty() { rec.app_id.clone() } else { app_id.trim().to_string() };
    let vendor = format!("renew:{}:{}:{}:{}", clean, zone, new_exp, app_for);

    let secret = SESSION_SEED.lock().unwrap().clone().ok_or("locked")?;
    let amount = price.to_string();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        sth_node::send_transfer(&secret, DOMAIN_SINK, &amount, "", &vendor)
    })
    .await
    .map_err(|e| e.to_string())??;
    if !outcome.accepted {
        return Err("Транзакция отклонена узлом".into());
    }
    rec.expires = new_exp;
    rec.app_id = app_for;
    write_record(&rec);
    let _ = DB.flush();
    Ok(outcome.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_labels() {
        assert_eq!(validate_name("Explorer"), Some("explorer".into()));
        assert_eq!(validate_name("a-b-c"), Some("a-b-c".into()));
        assert_eq!(validate_name("-bad"), None);
        assert_eq!(validate_name("bad-"), None);
        assert_eq!(validate_name("white space"), None);
        assert_eq!(validate_name("под"), None); // non-ascii
    }

    #[test]
    fn tiered_pricing() {
        assert_eq!(tier_price(5, 500), 500);
        assert_eq!(tier_price(4, 500), 10_000);
        assert_eq!(tier_price(3, 500), 50_000);
        assert_eq!(tier_price(2, 500), 500);
        assert!(is_admin_only(1));
        assert!(is_admin_only(2));
        assert!(!is_admin_only(3));
    }

    #[test]
    fn splits_name_zone() {
        assert_eq!(split_name_zone("explorer.sth"), ("explorer".into(), "sth".into()));
        assert_eq!(split_name_zone("explorer"), ("explorer".into(), "sth".into()));
    }

    #[test]
    fn parses_price_directive() {
        assert_eq!(parse_u64_param("PRICE_DOMAIN:sth:500", "PRICE_DOMAIN:sth:"), Some(500));
        assert_eq!(parse_u64_param("PRICE_SUBDOMAIN:sth:10 x", "PRICE_SUBDOMAIN:sth:"), Some(10));
        assert_eq!(parse_u64_param("PRICE_DOMAIN:sth:abc", "PRICE_DOMAIN:sth:"), None);
    }

    #[test]
    fn memo_matches_handles_vendorfield_truncation() {
        let seller = "SSU6TvyceXXXXXXXXXXXXXXXXXXXXXSniZu"; // 34-char STH address
        let buyer = "SboZHv8y3ohGbA5ophSCifZA5zCiD6rQyq"; // 34-char STH address
        // The fee leg memo `buydom:<seller>:<buyer>:<name>` exceeds the ~64-byte
        // vendorField cap, so the chain stores a truncated prefix.
        let full = format!("buydom:{}:{}:{}", seller, buyer, "blockexplorer");
        assert!(full.len() > 64, "memo must exceed the cap for this case");
        let onchain = full.chars().take(64).collect::<String>();
        assert_ne!(onchain, full); // truncated → exact compare would fail
        assert!(memo_matches(&onchain, &full)); // prefix-tolerant compare passes

        // Seller leg fits in 64 bytes → exact match.
        let selldom = format!("selldom:{}:{}", buyer, "blockexplorer");
        assert!(selldom.len() <= 64);
        assert!(memo_matches(&selldom, &selldom));

        // Guards: empty / too-short / unrelated memos must NOT match.
        assert!(!memo_matches("", &full));
        assert!(!memo_matches("buydom:", &full));
        assert!(!memo_matches("selldom:other:name", &full));
        // A different seller's truncated prefix must not match this order.
        let other = format!("buydom:{}:{}:{}", "SdiffSellerXXXXXXXXXXXXXXXXXXXXabcd", buyer, "blockexplorer");
        let other_trunc = other.chars().take(64).collect::<String>();
        assert!(!memo_matches(&other_trunc, &full));
    }
}
