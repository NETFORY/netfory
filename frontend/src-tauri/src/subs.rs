// ============================================================
//  SmartNet // subs.rs  —  paid account subscriptions (follow)
//
//  A subscription is a Type-0 transfer of the (admin-configured) follow fee
//  sent DIRECTLY to the followed user's address, carrying `sub:<username>` in
//  the transaction `vendorField`. The blockchain `sender` is the subscriber, so
//  the social graph is fully reconstructible from the chain alone:
//      • MY subscribers  = incoming txs to my address with vendor `sub:<myname>`
//      • MY following    = outgoing txs from my address with vendor `sub:*`
//
//  Only users that own a registered u:// name can be subscribed to.
//
//  Author: TechnoL0g
//  Created: 2026-06-20
// ============================================================

use crate::{load_vault_internal, naming, sth_node, DB, SESSION_SEED};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Потолок страниц (×100) при ПЕРВОМ полном обходе исходящих tx адреса (following).
const FOLLOW_PAGES: u32 = 30;
/// Потолок страниц (×100) входящих `sub:<name>` (подписчики).
const SUB_PAGES: u32 = 20;

// ---------------------------------------------------------------------------
//  Инкрементальный граф подписок в sled (источник для мгновенного UI):
//    subs:g:out:<A>:<name>   → {"address","ts"}   A подписан на name
//    subs:g:in:<A>:<sender>  → ts                 sender подписан на A
//    subs:g:cur:out:<A> / subs:g:cur:in:<A>       → последний обработанный epoch
//  Первый проход — полный постраничный скан (узел фильтрует по vendorField сам),
//  дальше — только дельта `timestamp.from=<cursor+1>` + tx из стрима sth/events/1.
// ---------------------------------------------------------------------------

fn out_prefix(addr: &str) -> String {
    format!("subs:g:out:{}:", addr)
}
fn in_prefix(addr: &str) -> String {
    format!("subs:g:in:{}:", addr)
}
fn cur_key(dir: &str, addr: &str) -> String {
    format!("subs:g:cur:{}:{}", dir, addr)
}

fn get_cursor(dir: &str, addr: &str) -> Option<u64> {
    DB.get(cur_key(dir, addr).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| std::str::from_utf8(&v).ok().and_then(|s| s.parse().ok()))
}

fn set_cursor(dir: &str, addr: &str, epoch: u64) {
    let _ = DB.insert(cur_key(dir, addr).as_bytes(), epoch.to_string().as_bytes());
}

/// Ребро «A подписан на name» (адрес получателя + unix ts первой подписки).
fn put_out_edge(addr: &str, name: &str, target: &str, ts: u64) -> bool {
    let k = format!("{}{}", out_prefix(addr), name);
    if DB.contains_key(k.as_bytes()).unwrap_or(false) {
        return false;
    }
    let _ = DB.insert(k.as_bytes(), serde_json::json!({ "address": target, "ts": ts }).to_string().as_bytes());
    true
}

/// Ребро «sender подписан на A» (unix ts первой подписки).
fn put_in_edge(addr: &str, sender: &str, ts: u64) -> bool {
    let k = format!("{}{}", in_prefix(addr), sender);
    if DB.contains_key(k.as_bytes()).unwrap_or(false) {
        return false;
    }
    let _ = DB.insert(k.as_bytes(), ts.to_string().as_bytes());
    true
}

fn delta_suffix(cur: Option<u64>) -> String {
    cur.map(|c| format!("&timestamp.from={}", c + 1)).unwrap_or_default()
}

/// Подтянуть исходящие `sub:*` адреса в sled (полный скан или дельта от курсора).
/// Возвращает число новых рёбер; Err — узел недоступен (курсор не двигаем).
fn sync_out(addr: &str) -> Result<usize, String> {
    let cur = get_cursor("out", addr);
    let txs = if crate::txindex::is_complete(addr) {
        // локальный кошелёк: полная история уже в sled — без сети, потоково
        let mut v = Vec::new();
        crate::txindex::for_each(addr, |t| {
            if t["sender"].as_str() == Some(addr) && t["vendorField"].as_str().map_or(false, |m| m.starts_with("sub:")) {
                v.push(sth_node::tx_lite(t, None));
            }
            true
        });
        v
    } else {
        let q = format!("{addr}&type=0&typeGroup=1{}", delta_suffix(cur));
        sth_node::fetch_txs_all("senderId", &q, FOLLOW_PAGES)?
    };
    let mut added = 0;
    let mut max_epoch = cur.unwrap_or(0);
    for t in &txs {
        max_epoch = max_epoch.max(t.epoch);
        if let Some(name) = t.vendor.strip_prefix("sub:") {
            let name = name.trim();
            if !name.is_empty() && !t.recipient.is_empty() && put_out_edge(addr, name, &t.recipient, t.timestamp) {
                added += 1;
            }
        }
    }
    set_cursor("out", addr, max_epoch);
    Ok(added)
}

/// Подтянуть входящие `sub:<name>` для адреса (узел фильтрует по vendorField).
fn sync_in(addr: &str, name: &str) -> Result<usize, String> {
    let cur = get_cursor("in", addr);
    let target = format!("sub:{}", name);
    let txs = if crate::txindex::is_complete(addr) {
        let mut v = Vec::new();
        crate::txindex::for_each(addr, |t| {
            if t["vendorField"].as_str() == Some(target.as_str()) {
                v.push(sth_node::tx_lite(t, Some(addr)));
            }
            true
        });
        v
    } else {
        let q = format!("{addr}&vendorField=sub:{name}{}", delta_suffix(cur));
        sth_node::fetch_txs_all("recipientId", &q, SUB_PAGES)?
    };
    let mut added = 0;
    let mut max_epoch = cur.unwrap_or(0);
    for t in &txs {
        max_epoch = max_epoch.max(t.epoch);
        if t.vendor == target && !t.sender.is_empty() && put_in_edge(addr, &t.sender, t.timestamp) {
            added += 1;
        }
    }
    set_cursor("in", addr, max_epoch);
    Ok(added)
}

/// Инкрементальная синхронизация графа адреса: входящие (если есть имя) и, при
/// `with_out`, исходящие. Пересчитывает кэш-статистику. Возвращает число новых рёбер.
pub(crate) fn sync_graph(addr: &str, with_out: bool) -> usize {
    // Свой адрес с полным локальным индексом: если индекс не менялся с прошлого
    // прохода — граф актуален, не перечитываем историю (память/CPU).
    if crate::txindex::is_complete(addr) {
        let cur = crate::txindex::seq(addr);
        let k = format!("subs:g:txseq:{}", addr);
        let prev = DB.get(k.as_bytes()).ok().flatten().and_then(|v| std::str::from_utf8(&v).ok()?.parse::<u64>().ok());
        let have_out = !with_out || get_cursor("out", addr).is_some();
        if prev == Some(cur) && have_out {
            return 0;
        }
        let _ = DB.insert(k.as_bytes(), cur.to_string().as_bytes());
    }
    let mut added = 0;
    if let Some(name) = naming::username_of_address(addr.to_string()) {
        if let Ok(n) = sync_in(addr, &name) {
            added += n;
        }
    }
    if with_out {
        if let Ok(n) = sync_out(addr) {
            added += n;
        }
    }
    recompute_stats(addr);
    if added > 0 {
        let _ = DB.flush();
    }
    added
}

/// Индексирован ли адрес хотя бы раз (есть курсор) — тогда UI читает sled мгновенно.
fn is_indexed(addr: &str, with_out: bool) -> bool {
    let in_ok = naming::username_of_address(addr.to_string()).is_none() || get_cursor("in", addr).is_some();
    in_ok && (!with_out || get_cursor("out", addr).is_some())
}

/// Сброс графа адреса (ручная ресинхронизация): рёбра + курсоры → следующий
/// `sync_graph` делает полный скан.
pub(crate) fn reset_graph(addr: &str) {
    for pfx in [out_prefix(addr), in_prefix(addr)] {
        for kv in DB.scan_prefix(pfx.as_bytes()) {
            if let Ok((k, _)) = kv {
                let _ = DB.remove(k);
            }
        }
    }
    let _ = DB.remove(cur_key("out", addr).as_bytes());
    let _ = DB.remove(cur_key("in", addr).as_bytes());
}

/// Полная ресинхронизация графа своего адреса с цепи (кнопка в панели безопасности).
pub(crate) fn resync_my_graph() -> u64 {
    let me = match load_vault_internal().map(|v| v.address) {
        Some(a) => a,
        None => return 0,
    };
    reset_graph(&me);
    sync_graph(&me, true);
    cached_subscribers(&me)
}

/// Входящие рёбра (sender, ts) — новые первыми.
fn in_edges(addr: &str) -> Vec<(String, u64)> {
    let pfx = in_prefix(addr);
    let mut v: Vec<(String, u64)> = DB
        .scan_prefix(pfx.as_bytes())
        .filter_map(|kv| kv.ok())
        .filter_map(|(k, val)| {
            let sender = std::str::from_utf8(&k).ok()?.strip_prefix(pfx.as_str())?.to_string();
            let ts = std::str::from_utf8(&val).ok()?.parse().unwrap_or(0);
            Some((sender, ts))
        })
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

/// Исходящие рёбра (name, address, ts) — новые первыми.
fn out_edges(addr: &str) -> Vec<(String, String, u64)> {
    let pfx = out_prefix(addr);
    let mut v: Vec<(String, String, u64)> = DB
        .scan_prefix(pfx.as_bytes())
        .filter_map(|kv| kv.ok())
        .filter_map(|(k, val)| {
            let name = std::str::from_utf8(&k).ok()?.strip_prefix(pfx.as_str())?.to_string();
            let j: serde_json::Value = serde_json::from_slice(&val).ok()?;
            Some((name, j["address"].as_str()?.to_string(), j["ts"].as_u64().unwrap_or(0)))
        })
        .collect();
    v.sort_by(|a, b| b.2.cmp(&a.2));
    v
}

/// Tx из стрима `sth/events/1` (подтверждённая): ребро в граф + событие фронту.
/// `me` — адрес подписки стрима (наш кошелёк).
pub(crate) fn on_stream_tx(app: &tauri::AppHandle, me: &str, tx: &serde_json::Value) {
    let vendor = tx["vendorField"].as_str().unwrap_or("");
    let name = match vendor.strip_prefix("sub:") {
        Some(n) if !n.trim().is_empty() => n.trim(),
        _ => return,
    };
    let sender = tx["sender"].as_str().unwrap_or("");
    let recipient = tx["recipient"].as_str().unwrap_or("");
    let ts = tx["timestamp"]["unix"].as_u64().unwrap_or_else(unix_now);
    let epoch = tx["timestamp"]["epoch"].as_u64().unwrap_or(0);
    let mut changed = false;
    if sender == me && !recipient.is_empty() {
        changed |= put_out_edge(me, name, recipient, ts);
        if epoch > 0 {
            set_cursor("out", me, epoch.max(get_cursor("out", me).unwrap_or(0)));
        }
    }
    if recipient == me && naming::username_of_address(me.to_string()).as_deref() == Some(name) && !sender.is_empty() {
        changed |= put_in_edge(me, sender, ts);
        if epoch > 0 {
            set_cursor("in", me, epoch.max(get_cursor("in", me).unwrap_or(0)));
        }
    }
    if changed {
        recompute_stats(me);
        let _ = DB.flush();
        use tauri::Emitter;
        let _ = app.emit("social-graph-updated", serde_json::json!({ "address": me }));
        crate::syslog::log("subs", format!("стрим: новое ребро {} ({} → {})", vendor, sender, recipient));
    }
}

/// Rolling window (seconds) for the "trending this week" growth metric.
const WEEK_SECS: u64 = 7 * 24 * 3600;
/// Subscriber count above which a profile earns the auto "Verified" badge.
/// (Test threshold; bump to 50 for production.)
pub(crate) const VERIFIED_THRESHOLD: u64 = 10;
/// SmartHoldem team wallet — a `sub:` from this address mints the metallic
/// "Verified by SmartHoldem" plaque (highest trust tier).
pub(crate) const TEAM_ADMIN: &str = "SeZLuyhhYf2qxs4ArPJ71oEu3x8EsVw51C";

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn week_key(addr: &str) -> String {
    format!("subs:wk:{}", addr)
}

fn count_key(addr: &str) -> String {
    format!("subs:count:{}", addr)
}
fn team_key(addr: &str) -> String {
    format!("subs:team:{}", addr)
}

/// Cached subscriber count for an address (0 until the indexer runs).
pub(crate) fn cached_subscribers(addr: &str) -> u64 {
    DB.get(count_key(addr).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| std::str::from_utf8(&v).ok().and_then(|s| s.trim().parse().ok()))
        .unwrap_or(0)
}

/// Whether the SmartHoldem team wallet has subscribed to an address.
pub(crate) fn cached_team_verified(addr: &str) -> bool {
    matches!(DB.get(team_key(addr).as_bytes()), Ok(Some(v)) if v.as_ref() == b"1")
}

/// Cached number of NEW subscribers gained in the last 7 days (trending metric).
pub(crate) fn cached_weekly(addr: &str) -> u64 {
    DB.get(week_key(addr).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| std::str::from_utf8(&v).ok().and_then(|s| s.trim().parse().ok()))
        .unwrap_or(0)
}

/// Пересчёт кэш-статистики адреса из sled-графа (число подписчиков, рост за 7
/// дней, флаг команды) — без сети.
fn recompute_stats(addr: &str) {
    let now = unix_now();
    let edges = in_edges(addr);
    let week = edges.iter().filter(|(_, ts)| *ts > 0 && now.saturating_sub(*ts) <= WEEK_SECS).count();
    let team = edges.iter().any(|(s, _)| s == TEAM_ADMIN);
    let _ = DB.insert(count_key(addr).as_bytes(), edges.len().to_string().as_bytes());
    let _ = DB.insert(week_key(addr).as_bytes(), week.to_string().as_bytes());
    let _ = DB.insert(team_key(addr).as_bytes(), if team { b"1" as &[u8] } else { b"0" });
}

/// Refresh the cached social-graph stats for a SINGLE address (subscriber count,
/// 7-day growth, team flag). Дельта с цепи + пересчёт. Returns the unique-subscriber count.
fn index_addr(_name: &str, addr: &str) -> u64 {
    sync_graph(addr, false);
    cached_subscribers(addr)
}

/// One indexer pass: refresh cached stats for every registered username (powers
/// the directory ranking, badges & weekly top widget, offline-ready).
pub(crate) fn index_once() {
    for kv in DB.scan_prefix(b"naming_svc:user:") {
        let (k, v) = match kv {
            Ok(p) => p,
            Err(_) => continue,
        };
        let (ks, vs) = match (std::str::from_utf8(&k), std::str::from_utf8(&v)) {
            (Ok(a), Ok(b)) => (a, b),
            _ => continue,
        };
        let name = ks.trim_start_matches("naming_svc:user:");
        if name.is_empty() || vs.is_empty() {
            continue;
        }
        index_addr(name, vs);
    }
}

/// Immediately refresh an address's subscriber stats — called when a fresh
/// incoming `sub:` transfer lands, so counts update without waiting for the
/// background cycle. Returns the new subscriber count (0 if the address has no
/// registered name and thus cannot be subscribed to).
#[tauri::command]
pub(crate) async fn refresh_subscribers(address: String) -> u64 {
    tauri::async_runtime::spawn_blocking(move || refresh_subscribers_inner(address))
        .await
        .unwrap_or(0)
}

pub(crate) fn refresh_subscribers_inner(address: String) -> u64 {
    match naming::username_of_address(address.clone()) {
        Some(name) => index_addr(&name, &address),
        None => 0,
    }
}

/// Background subscription indexer: refresh the social graph cache on a cadence.
pub(crate) fn spawn_indexer() {
    let _ = std::thread::Builder::new().name("subs-indexer".into()).spawn(|| {
        let mut first = true;
        loop {
            // offline: пропускаем сетевой тик — иначе запросы виснут в таймаутах
            if crate::netwatch::is_online() {
                index_once();
            }
            if first {
                crate::sync_status::mark_done("subs");
                first = false;
            }
            std::thread::sleep(std::time::Duration::from_secs(180));
        }
    });
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct Entry {
    /// Registered u:// username, if the counterparty owns one.
    name: Option<String>,
    address: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FollowStats {
    subscribers: u64,
    following: u64,
    /// Last (newest-first) unique subscribers — for the avatar mini-grid.
    #[serde(rename = "recentSubscribers")]
    recent_subscribers: Vec<Entry>,
    /// Who this address follows (newest-first, unique) — the "My subscriptions".
    #[serde(rename = "followingList")]
    following_list: Vec<Entry>,
}

/// Addresses this account is subscribed to (for the chat trust filter).
/// Из sled-графа (дельта с цепи — если адрес ещё не индексирован, полный скан).
pub(crate) fn following_addresses(address: String) -> Vec<String> {
    if !is_indexed(&address, true) {
        sync_graph(&address, true);
    }
    out_edges(&address).into_iter().map(|(_, a, _)| a).collect()
}

/// Адреса ПОДПИСЧИКОВ этого аккаунта (кто зафолловил его) — для обратной
/// auto-subscribe в мессенджере: без неё первое сообщение новичка-фолловера
/// уходит в топик, который мы ещё не слушаем, и теряется.
pub(crate) fn subscriber_addresses(address: String) -> Vec<String> {
    if naming::username_of_address(address.clone()).is_none() {
        return Vec::new(); // без имени подписаться на нас нельзя
    }
    if !is_indexed(&address, false) {
        sync_graph(&address, false);
    }
    in_edges(&address).into_iter().map(|(s, _)| s).collect()
}

fn entry_for(address: &str) -> Entry {
    Entry {
        name: naming::username_of_address(address.to_string()),
        address: address.to_string(),
    }
}

/// The current subscription fee in whole STH (admin-controlled).
#[tauri::command]
pub(crate) fn get_subscribe_price() -> u64 {
    naming::follow_price()
}

#[derive(Debug, Serialize)]
pub(crate) struct TopEntry {
    name: String,
    address: String,
    subscribers: u64,
    growth: u64,
    #[serde(rename = "verifiedTeam")]
    verified_team: bool,
}

/// The "trending this week" leaderboard: registered users ranked by NEW
/// subscribers gained in the last 7 days (falls back to total subscribers when
/// no growth yet, so the widget is never empty). Returns up to `limit` entries.
#[tauri::command(async)]
pub(crate) fn weekly_top(limit: usize) -> Vec<TopEntry> {
    let mut all: Vec<TopEntry> = Vec::new();
    for kv in DB.scan_prefix(b"naming_svc:user:") {
        if let Ok((k, v)) = kv {
            if let (Ok(ks), Ok(vs)) = (std::str::from_utf8(&k), std::str::from_utf8(&v)) {
                let name = ks.trim_start_matches("naming_svc:user:").to_string();
                if name.is_empty() {
                    continue;
                }
                all.push(TopEntry {
                    name,
                    subscribers: cached_subscribers(vs),
                    growth: cached_weekly(vs),
                    verified_team: cached_team_verified(vs),
                    address: vs.to_string(),
                });
            }
        }
    }
    all.sort_by(|a, b| {
        b.growth
            .cmp(&a.growth)
            .then(b.subscribers.cmp(&a.subscribers))
            .then(a.name.cmp(&b.name))
    });
    // Only surface entries that actually have traction (any subscriber/growth).
    all.retain(|e| e.growth > 0 || e.subscribers > 0);
    all.truncate(limit);
    all
}

/// Граф подписок адреса. Адрес уже индексирован → мгновенно из sled, дельта с
/// цепи докатывается фоном (событие `social-graph-updated` при изменениях).
/// Не индексирован → первый полный скан блокирующе (в spawn_blocking).
#[tauri::command]
pub(crate) async fn follow_stats(app: tauri::AppHandle, address: String) -> FollowStats {
    tauri::async_runtime::spawn_blocking(move || {
        if address.is_empty() {
            return build_stats(&address);
        }
        if is_indexed(&address, true) {
            let addr = address.clone();
            std::thread::spawn(move || {
                if crate::netwatch::is_online() && sync_graph(&addr, true) > 0 {
                    use tauri::Emitter;
                    let _ = app.emit("social-graph-updated", serde_json::json!({ "address": addr }));
                }
            });
        } else if crate::netwatch::is_online() {
            sync_graph(&address, true);
        }
        build_stats(&address)
    })
    .await
    .unwrap_or_else(|_| build_stats(""))
}

/// Мгновенный снимок follow-статистики из sled-графа (без сети).
#[tauri::command]
pub(crate) fn follow_stats_cached(address: String) -> Option<FollowStats> {
    if address.is_empty() || !is_indexed(&address, true) {
        return None;
    }
    Some(build_stats(&address))
}

/// Собрать FollowStats из sled-графа (микросекунды, офлайн).
fn build_stats(address: &str) -> FollowStats {
    if address.is_empty() {
        return FollowStats { subscribers: 0, following: 0, recent_subscribers: vec![], following_list: vec![] };
    }
    let ins = in_edges(address);
    let recent_subscribers: Vec<Entry> = ins.iter().take(10).map(|(s, _)| entry_for(s)).collect();
    let following_list: Vec<Entry> = out_edges(address)
        .into_iter()
        .map(|(name, addr, _)| Entry { name: Some(name), address: addr })
        .collect();
    FollowStats {
        subscribers: ins.len() as u64,
        following: following_list.len() as u64,
        recent_subscribers,
        following_list,
    }
}

/// Subscribe to a user by username: broadcast the follow fee to their address
/// with `sub:<username>` in the vendorField. Returns the txid.
#[tauri::command]
pub(crate) async fn subscribe(username: String) -> Result<String, String> {
    let clean = naming::validate_name(&username).ok_or("Недопустимое имя")?;
    let author = naming::resolve_username(clean.clone()).ok_or("Пользователь с таким именем не найден")?;
    if let Some(me) = load_vault_internal().map(|v| v.address) {
        if me == author {
            return Err("Нельзя подписаться на самого себя".into());
        }
    }
    let secret = SESSION_SEED.lock().unwrap().clone().ok_or("locked")?;
    let amount = naming::follow_price().to_string();
    let vendor = format!("sub:{}", clean);
    tauri::async_runtime::spawn_blocking(move || {
        let outcome = sth_node::send_transfer(&secret, &author, &amount, "", &vendor)?;
        if !outcome.accepted {
            return Err("Транзакция отклонена узлом".to_string());
        }
        // Репутация (только вперёд): автору +10 за нового подписчика,
        // подписчику +1 за подписку.
        crate::reputation::add(&author, 10);
        if let Some(me) = load_vault_internal().map(|v| v.address) {
            crate::reputation::add(&me, 1);
        }
        Ok(outcome.id)
    })
    .await
    .map_err(|e| e.to_string())?
}
