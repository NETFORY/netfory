// ============================================================
//  SmartNet // naming.rs  —  on-chain username registry (u:// names)
//
//  A username is claimed by sending a Type-0 transfer to the registry sink
//  address with the raw lowercase name in the transaction `vendorField`. The
//  blockchain `sender` is the immutable owner (chronological FIRST-WIN: one
//  sender = one name, one name = one sender). A background indexer scans the
//  sink's incoming transactions, validates the names and cross-links them in
//  the local Sled DB:
//      naming_svc:user:<name>  -> <sender_address>
//      naming_svc:uid:<sender> -> <name>
//
//  Author: TechnoL0g
//  Created: 2026-06-20
// ============================================================

use crate::{load_vault_internal, sth_node, DB, SESSION_SEED};
use serde_json::Value;

/// Where name-registration transfers must be sent. The sender becomes the owner.
pub(crate) const NAME_SINK: &str = "SeeQyvfpghaFLjVfLUTxLvpQq78LMQnAmE";
/// Network config feed: the admin posts `PARAM:VALUE` instructions here.
const CONFIG_SINK: &str = "SeTQeEAsHnHU1Y9EBjkRVNPB3fmUvfFUrk";
const CONFIG_ADMIN: &str = "SeZLuyhhYf2qxs4ArPJ71oEu3x8EsVw51C";
/// Default name fee (whole STH) until the on-chain config is indexed.
const DEFAULT_PRICE_NAME: u64 = 100;
/// Sled key caching the admin-controlled registration fee (whole STH).
const PRICE_NAME_KEY: &[u8] = b"cfg:param:price_name";
/// Sled key caching the admin-controlled subscription (follow) fee (whole STH).
pub(crate) const PRICE_FOLLOW_KEY: &[u8] = b"cfg:param:price_follow";
/// Default subscription fee (whole STH) until the on-chain config is indexed.
pub(crate) const DEFAULT_PRICE_FOLLOW: u64 = 1;
/// How many ascending pages (×100 tx) of the sink history to scan per cycle.
const SCAN_PAGES: u32 = 25;

/// Validate + normalize a username: lowercase Latin letters, digits and at most
/// ONE dot, length 2–24. Returns the canonical lowercase form or None.
pub(crate) fn validate_name(raw: &str) -> Option<String> {
    let name = raw.trim().to_lowercase();
    let len = name.chars().count();
    if len < 2 || len > 24 {
        return None;
    }
    if name.chars().filter(|c| *c == '.').count() > 1 {
        return None;
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.') {
        return None;
    }
    Some(name)
}

fn user_key(name: &str) -> String {
    format!("naming_svc:user:{}", name)
}
fn uid_key(sender: &str) -> String {
    format!("naming_svc:uid:{}", sender)
}

/// System-reserved usernames that can never be registered (they map to native
/// SmartNet pages / roles).
const RESERVED_NAMES: &[&str] = &["nameservice", "smartnet", "admin", "root", "system", "support", "group", "groups", "new", "dev"];

fn is_reserved(name: &str) -> bool {
    // Локальный список ролей + общий большой список public/reserved.txt (HashSet).
    RESERVED_NAMES.contains(&name) || crate::dns_manager::is_reserved_label(name)
}

/// Зарезервировано ли имя для данного отправителя: адреса из `.env DEVELOPERS`
/// регистрируют reserved-имена без проверки.
fn is_reserved_for(name: &str, sender: &str) -> bool {
    is_reserved(name) && !crate::dns_manager::is_developer(sender)
}

/// С какого момента (unix) reserved-список применяется к on-chain записям.
/// Оплаченные регистрации, подтверждённые в цепи РАНЬШЕ (например `europa`,
/// 2026-06-21, список расширен позже) — легитимны и не могут быть отозваны
/// задним числом: цепочка — источник истины, список блокирует только новые.
const RESERVED_SINCE_UNIX: u64 = 1_786_233_600; // 2026-08-09T00:00:00Z

/// Commit a (sender, name) pair honoring first-win: only if NEITHER the name nor
/// the sender already has a record. Reserved names are rejected here so that even
/// a raw on-chain registration tx (bypassing the client UI) can never claim them —
/// кроме записей, подтверждённых до `RESERVED_SINCE_UNIX` (`tx_ts`, unix секунды).
/// Returns true if newly committed.
fn commit_name(sender: &str, name: &str, tx_ts: u64) -> bool {
    if is_reserved_for(name, sender) && tx_ts >= RESERVED_SINCE_UNIX {
        crate::syslog::log("naming", format!("u://{} от {} отклонено: reserved (tx {})", name, sender, tx_ts));
        return false;
    }
    let uk = user_key(name);
    let ik = uid_key(sender);
    let name_taken = DB.get(uk.as_bytes()).ok().flatten().is_some();
    let uid_taken = DB.get(ik.as_bytes()).ok().flatten().is_some();
    if name_taken || uid_taken {
        return false;
    }
    let _ = DB.insert(uk.as_bytes(), sender.as_bytes());
    let _ = DB.insert(ik.as_bytes(), name.as_bytes());
    // Разовый бонус за регистрацию имени: +1000 репутации новому владельцу.
    crate::reputation::grant_registration(sender);
    // Наша собственная регистрация подтвердилась в цепочке → событие всем окнам
    // (тост в главном, шаг онбординга в мессенджере). Флаг pending ставит
    // register_username — старые имена при первом индексе не уведомляем.
    let pk = pending_key(sender);
    if DB.get(pk.as_bytes()).ok().flatten().is_some() {
        let _ = DB.remove(pk.as_bytes());
        if let Some(app) = sth_node::app_handle() {
            use tauri::Emitter;
            let _ = app.emit("name-confirmed", serde_json::json!({ "name": name, "address": sender }));
        }
        crate::syslog::log("naming", format!("u://{} confirmed on-chain", name));
    }
    true
}

fn pending_key(addr: &str) -> String {
    format!("naming:pending:{}", addr)
}

/// Есть ли наша регистрация, ждущая подтверждения (индексер опрашивает чаще).
fn has_pending() -> bool {
    DB.scan_prefix(b"naming:pending:").next().is_some()
}

/// The current registration fee (whole STH): the admin-indexed value, else the
/// compiled default. Cached in Sled so the UI reads it instantly and offline.
pub(crate) fn name_price() -> u64 {
    if let Ok(Some(v)) = DB.get(PRICE_NAME_KEY) {
        if let Ok(s) = std::str::from_utf8(&v) {
            if let Ok(n) = s.trim().parse::<u64>() {
                if n > 0 {
                    return n;
                }
            }
        }
    }
    DEFAULT_PRICE_NAME
}

/// Parse a `PRICE_NAME:<u64>` directive out of a vendorField (whitespace-split).
fn parse_price_name(field: &str) -> Option<u64> {
    field
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("PRICE_NAME:").and_then(|v| v.trim().parse::<u64>().ok()))
}

/// Parse a `PRICE_FOLLOW:<u64>` directive out of a vendorField.
fn parse_price_follow(field: &str) -> Option<u64> {
    field
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("PRICE_FOLLOW:").and_then(|v| v.trim().parse::<u64>().ok()))
}

/// The current subscription (follow) fee in whole STH (admin-controlled, cached).
pub(crate) fn follow_price() -> u64 {
    if let Ok(Some(v)) = DB.get(PRICE_FOLLOW_KEY) {
        if let Ok(s) = std::str::from_utf8(&v) {
            if let Ok(n) = s.trim().parse::<u64>() {
                if n > 0 {
                    return n;
                }
            }
        }
    }
    DEFAULT_PRICE_FOLLOW
}

/// One config pass: read the newest ADMIN→CONFIG transfers and cache the most
/// recent `PRICE_NAME:<v>` / `PRICE_FOLLOW:<v>` directives. Newest-first.
pub(crate) fn sync_price_once() {
    let fields = match sth_node::fetch_vendor_fields(CONFIG_ADMIN, CONFIG_SINK, 100) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut got_name = false;
    let mut got_follow = false;
    for field in &fields {
        if !got_name {
            if let Some(v) = parse_price_name(field) {
                let _ = DB.insert(PRICE_NAME_KEY, v.to_string().as_bytes());
                got_name = true;
            }
        }
        if !got_follow {
            if let Some(v) = parse_price_follow(field) {
                let _ = DB.insert(PRICE_FOLLOW_KEY, v.to_string().as_bytes());
                got_follow = true;
            }
        }
        if got_name && got_follow {
            break;
        }
    }
    let _ = DB.flush();
}

/// One indexer pass: scan the sink's incoming transfers (oldest-first) and
/// register the first valid name per sender / per name. Idempotent.
pub(crate) fn index_once() {
    for page in 1..=SCAN_PAGES {
        let path = format!(
            "/transactions?recipientId={}&page={}&limit=100&orderBy=timestamp:asc",
            NAME_SINK,
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
            // страховка: узел обязан был отфильтровать по recipientId — чужие tx не индексируем
            if sender.is_empty() || t["recipient"].as_str().map(|r| r != NAME_SINK).unwrap_or(false) {
                continue;
            }
            if let Some(name) = validate_name(vendor) {
                commit_name(sender, &name, t["timestamp"]["unix"].as_u64().unwrap_or(u64::MAX));
            }
        }
        if arr.len() < 100 {
            break;
        }
    }
    let _ = DB.flush();
}

/// Прямое восстановление своего имени: если в sled его нет — читаем ИСХОДЯЩИЕ tx
/// main-кошелька на NAME_SINK (через активный узел / iroh) и коммитим первую валидную
/// регистрацию. Не зависит от полного скана sink'а: одна страница по своему адресу.
pub(crate) fn recover_my_name() -> Option<String> {
    let addr = load_vault_internal()?.address;
    if let Some(n) = DB.get(uid_key(&addr).as_bytes()).ok().flatten().and_then(|v| String::from_utf8(v.to_vec()).ok()) {
        return Some(n);
    }
    // полный локальный индекс — имя ищем в sled, без сети
    if crate::txindex::is_complete(&addr) {
        let mut regs: Vec<Value> = Vec::new();
        crate::txindex::for_each(&addr, |t| {
            if t["sender"].as_str() == Some(addr.as_str()) && t["recipient"].as_str() == Some(NAME_SINK) {
                regs.push(t.clone());
            }
            true
        });
        regs.reverse(); // старые первыми — first-win
        for t in &regs {
            if let Some(name) = validate_name(t["vendorField"].as_str().unwrap_or("")) {
                let ts = t["timestamp"]["unix"].as_u64().unwrap_or(u64::MAX);
                if commit_name(&addr, &name, ts) || DB.get(uid_key(&addr).as_bytes()).ok().flatten().is_some() {
                    crate::syslog::log("naming", format!("имя восстановлено из локального индекса: u://{}", name));
                    return DB.get(uid_key(&addr).as_bytes()).ok().flatten().and_then(|v| String::from_utf8(v.to_vec()).ok());
                }
            }
        }
        return None;
    }
    let path = format!(
        "/wallets/{}/transactions/sent?recipientId={}&page=1&limit=100&orderBy=timestamp:asc",
        addr, NAME_SINK
    );
    let txt = match sth_node::node_get(&path, std::time::Duration::from_secs(20)) {
        Ok(t) => t,
        Err(e) => {
            crate::syslog::log("naming", format!("recover_my_name: узел не ответил ({})", e));
            return None;
        }
    };
    let v: Value = serde_json::from_str(&txt).ok()?;
    let arr = v["data"].as_array()?;
    if arr.is_empty() {
        crate::syslog::log("naming", format!("recover_my_name: у {} нет регистраций на NAME_SINK", addr));
    }
    for t in arr {
        if t["recipient"].as_str() != Some(NAME_SINK) || t["sender"].as_str() != Some(addr.as_str()) {
            continue;
        }
        if let Some(name) = validate_name(t["vendorField"].as_str().unwrap_or("")) {
            let ts = t["timestamp"]["unix"].as_u64().unwrap_or(u64::MAX);
            if commit_name(&addr, &name, ts) || DB.get(uid_key(&addr).as_bytes()).ok().flatten().is_some() {
                let _ = DB.flush();
                crate::syslog::log("naming", format!("u://{} восстановлено из исходящих tx {}", name, addr));
                if let Some(app) = sth_node::app_handle() {
                    use tauri::Emitter;
                    let _ = app.emit("name-confirmed", serde_json::json!({ "name": name, "address": addr }));
                }
                return Some(name);
            }
        }
    }
    None
}

/// Background loop: refresh the name index on a fixed cadence.
pub(crate) fn spawn_indexer() {
    std::thread::spawn(|| {
        let mut first = true;
        loop {
            // offline: пропускаем сетевой тик — иначе запросы виснут в таймаутах
            if crate::netwatch::is_online() {
                // своё имя — ПЕРВЫМ, напрямую по исходящим tx (одна страница, быстро),
                // не дожидаясь полного скана sink'а (25 страниц)
                recover_my_name();
                sync_price_once();
                index_once();
            }
            if first {
                crate::sync_status::mark_done("naming");
                first = false;
            }
            // Ждём подтверждения своего имени — опрашиваем каждые 20с вместо 90.
            let secs = if has_pending() { 20 } else { 90 };
            std::thread::sleep(std::time::Duration::from_secs(secs));
        }
    });
}

// ---- IPC commands ----

/// The username owned by the currently-unlocked wallet, if any.
#[tauri::command]
pub(crate) fn get_my_username() -> Option<String> {
    let addr = load_vault_internal()?.address;
    DB.get(uid_key(&addr).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
}

/// Имя из sled, а при отсутствии — восстановление по исходящим tx main-кошелька (сеть).
#[tauri::command]
pub(crate) async fn recover_my_username() -> Option<String> {
    tauri::async_runtime::spawn_blocking(recover_my_name).await.ok().flatten()
}

/// Ручная ресинхронизация реестра с цепи (панель безопасности профиля):
/// цены → индекс имён sink'а → своё имя по исходящим tx → статистика подписчиков.
#[tauri::command]
pub(crate) async fn resync_registry(app: tauri::AppHandle) -> Result<Value, String> {
    // полный индекс истории владельца — сброс и перечитка фоном
    crate::txindex::resync_my_index(&app);
    tauri::async_runtime::spawn_blocking(|| {
        sync_price_once();
        index_once();
        let name = recover_my_name();
        let subscribers = crate::subs::resync_my_graph();
        crate::syslog::log(
            "naming",
            format!("ручная ресинхронизация реестра: имя={:?}, подписчиков={}", name, subscribers),
        );
        serde_json::json!({ "name": name, "subscribers": subscribers })
    })
    .await
    .map_err(|e| e.to_string())
}

/// Resolve a username to its owner's address via the local Sled namespace.
#[tauri::command]
pub(crate) fn resolve_username(name: String) -> Option<String> {
    let clean = validate_name(&name)?;
    DB.get(user_key(&clean).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
}

/// Is a username free to register (not yet claimed on-chain & indexed)?
#[tauri::command]
pub(crate) fn username_available(name: String) -> Result<bool, String> {
    let clean = validate_name(&name).ok_or("Недопустимое имя")?;
    let caller = load_vault_internal().map(|v| v.address).unwrap_or_default();
    if is_reserved_for(&clean, &caller) {
        return Ok(false);
    }
    Ok(DB.get(user_key(&clean).as_bytes()).ok().flatten().is_none())
}

/// RESERVED-гейт для UI: имя из списка reserved и текущий адрес не из DEVELOPERS.
#[tauri::command]
pub(crate) fn username_reserved(name: String) -> bool {
    let clean = match validate_name(&name) {
        Some(c) => c,
        None => return false,
    };
    let caller = load_vault_internal().map(|v| v.address).unwrap_or_default();
    is_reserved_for(&clean, &caller)
}

/// Register a username: broadcast a Type-0 transfer to the sink carrying the
/// lowercase name in the vendorField. The name becomes active once the indexer
/// observes the confirmed transaction (first-win).
#[tauri::command]
pub(crate) async fn register_username(name: String) -> Result<String, String> {
    let clean = validate_name(&name).ok_or("Недопустимое имя: 2–24 символа, a-z 0-9 и одна точка")?;
    let caller = load_vault_internal().map(|v| v.address).unwrap_or_default();
    // reserved-имена (public/reserved.txt) для пользователей — просто «занято»
    if is_reserved_for(&clean, &caller) || DB.get(user_key(&clean).as_bytes()).ok().flatten().is_some() {
        return Err("Имя уже занято".into());
    }
    // The wallet must not already own a name (one sender = one name).
    if let Some(addr) = load_vault_internal().map(|v| v.address) {
        if DB.get(uid_key(&addr).as_bytes()).ok().flatten().is_some() {
            return Err("За этим кошельком уже закреплено имя".into());
        }
    }
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("locked")?;
    let amount = name_price().to_string();
    let pending_addr = caller.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let outcome = sth_node::send_transfer(&secret, NAME_SINK, &amount, "", &clean)?;
        if !outcome.accepted {
            return Err(if outcome.reason.is_empty() {
                "Транзакция отклонена узлом".to_string()
            } else {
                format!("Транзакция отклонена: {}", outcome.reason)
            });
        }
        // tx принята в пул → ждём индексер (событие name-confirmed при коммите)
        if !pending_addr.is_empty() {
            let _ = DB.insert(pending_key(&pending_addr).as_bytes(), clean.as_bytes());
            let _ = DB.flush();
        }
        Ok(outcome.id)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The current on-chain registration fee in whole STH (admin-controlled).
#[tauri::command]
pub(crate) fn get_registration_fee() -> u64 {
    name_price()
}

/// Reverse lookup: the username registered to an address, if any (for the
/// clickable `@username` author badge on dApp cards).
#[tauri::command]
pub(crate) fn username_of_address(address: String) -> Option<String> {
    DB.get(uid_key(&address).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
}

#[derive(serde::Serialize)]
pub(crate) struct NameEntry {
    name: String,
    address: String,
    subscribers: u64,
    /// Auto badge: subscriber count over the verified threshold.
    verified: bool,
    /// Top tier: the SmartHoldem team wallet subscribed to this address.
    #[serde(rename = "verifiedTeam")]
    verified_team: bool,
}

/// The full on-chain username registry enriched with the cached social-graph
/// stats, sorted by subscriber count desc (the network's social leaderboard).
/// Powers the `sn://nameservice` directory page (client-side search/paginate).
#[tauri::command(async)]
pub(crate) fn list_usernames() -> Vec<NameEntry> {
    let prefix = b"naming_svc:user:";
    let mut out: Vec<NameEntry> = Vec::new();
    for kv in DB.scan_prefix(prefix) {
        if let Ok((k, v)) = kv {
            if let (Ok(ks), Ok(vs)) = (std::str::from_utf8(&k), std::str::from_utf8(&v)) {
                let name = ks.trim_start_matches("naming_svc:user:").to_string();
                if name.is_empty() {
                    continue;
                }
                let subscribers = crate::subs::cached_subscribers(vs);
                out.push(NameEntry {
                    name,
                    address: vs.to_string(),
                    subscribers,
                    verified: subscribers >= crate::subs::VERIFIED_THRESHOLD,
                    verified_team: crate::subs::cached_team_verified(vs),
                });
            }
        }
    }
    // Social leaderboard: most-followed first, then alphabetical.
    out.sort_by(|a, b| b.subscribers.cmp(&a.subscribers).then(a.name.cmp(&b.name)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_price_name_directive() {
        assert_eq!(parse_price_name("PRICE_NAME:100"), Some(100));
        assert_eq!(parse_price_name("PRICE_NAME:250 note"), Some(250));
        assert_eq!(parse_price_name("hello PRICE_NAME:7"), Some(7));
    }

    #[test]
    fn rejects_foreign_or_malformed_directive() {
        assert_eq!(parse_price_name("appPublishPrice:100"), None);
        assert_eq!(parse_price_name("PRICE_NAME:abc"), None);
        assert_eq!(parse_price_name("PRICE_NAME"), None);
        assert_eq!(parse_price_name("just text"), None);
    }

    #[test]
    fn validates_usernames() {
        assert_eq!(validate_name("TechnoL0g"), Some("technol0g".into()));
        assert_eq!(validate_name("a.b"), Some("a.b".into()));
        assert_eq!(validate_name("a"), None); // too short
        assert_eq!(validate_name("a.b.c"), None); // two dots
        assert_eq!(validate_name("bad name"), None); // space
    }
}
