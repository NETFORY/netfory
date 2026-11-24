// ============================================================
//  SmartNet // peerbook.rs — персистентная адресная книга iroh-пиров (sled)
//
//  Зачем: `Endpoint::connect(EndpointId, ALPN)` без адресов заставляет iroh
//  каждый раз после старта резолвить NodeID через discovery (DHT/DNS/pkarr/
//  mDNS) и поднимать путь через реле. Мы запоминаем реальные адреса пира
//  (relay-URL + прямые IP:port) после КАЖДОГО успешного соединения и на
//  следующем connect отдаём их сразу → прямой путь/реле без ожидания discovery.
//
//  Безопасность: адреса — только подсказка транспорта. Личность пира всегда
//  проверяется QUIC/TLS по его ed25519-ключу (NodeID), подделанный адрес даст
//  лишь неудачный handshake, после чего вызывающий падает на discovery.
//
//    p2p:peerbook:<nodeId> → {"addrs":["relay:<url>","ip:<sock>"...],"ts":<unix>,
//                             "roles":["wallet","chat","seed","ai","api"]}
//  roles — в каких ролях пир нам встречался (один NodeID может быть и сидером,
//  и собеседником); используется для диагностики, лимитов и UI, на резолв не влияет.
// ============================================================

use crate::DB;
use iroh::address_lookup::{AddressLookup, EndpointData, EndpointInfo, Error as LookupError, Item};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, TransportAddr};
use n0_future::{boxed::BoxStream, stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// Endpoint главного приложения — чтобы запоминать адреса и ВХОДЯЩИХ пиров
/// (обработчики протоколов получают только Connection).
static ENDPOINT: OnceLock<Endpoint> = OnceLock::new();

pub(crate) fn set_endpoint(ep: &Endpoint) {
    let _ = ENDPOINT.set(ep.clone());
}

/// Запомнить адреса пира по его NodeID через глобальный endpoint (входящие
/// соединения, загрузки blobs через Downloader и т.п.).
pub(crate) fn remember_id(id: EndpointId, role: Role) {
    if let Some(ep) = ENDPOINT.get().cloned() {
        tauri::async_runtime::spawn(async move { remember(&ep, id, role).await });
    }
}

/// iroh `AddressLookup` поверх sled-книги: резолвер опрашивается конкурентно с
/// DHT/DNS/mDNS при КАЖДОМ connect по NodeID внутри iroh (включая Downloader,
/// gossip, чат) — известные пиры получают адреса мгновенно, остальные пути не
/// замедляются. Публикацию не делает (за неё отвечают pkarr/DHT).
///
/// СТЕЛС: `relay_allow` — белый список реле. В стелс-режиме книга НЕ должна
/// подсказывать n0/чужие реле, накопленные в обычном режиме, иначе relay-изоляция
/// протекает. Прямые IP пропускаются всегда (стелс-endpoint сам режет UDP).
#[derive(Debug, Clone, Default)]
pub(crate) struct PeerbookLookup {
    relay_allow: Option<Vec<RelayUrl>>,
}

impl PeerbookLookup {
    /// Обычный режим: любые сохранённые реле и IP.
    pub(crate) fn open() -> Self {
        Self { relay_allow: None }
    }

    /// Стелс: реле только из списка n1-релеев оператора.
    pub(crate) fn stealth(relay_urls: &[String]) -> Self {
        Self { relay_allow: Some(relay_urls.iter().filter_map(|u| u.parse().ok()).collect()) }
    }

    fn allowed(&self, a: &TransportAddr) -> bool {
        match (a, &self.relay_allow) {
            (TransportAddr::Relay(u), Some(allow)) => allow.contains(u),
            _ => true,
        }
    }
}

impl AddressLookup for PeerbookLookup {
    fn publish(&self, _data: &EndpointData) {}

    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, LookupError>>> {
        let raw = DB.get(key(&endpoint_id).as_bytes()).ok().flatten()?;
        let e: Entry = serde_json::from_slice(&raw).ok()?;
        if now().saturating_sub(e.ts) > TTL_SECS {
            return None;
        }
        let addrs: Vec<TransportAddr> = e
            .addrs
            .iter()
            .filter_map(|s| decode(s))
            .filter(|a| self.allowed(a))
            .collect();
        if addrs.is_empty() {
            return None;
        }
        let item = Item::new(
            EndpointInfo::from_parts(endpoint_id, EndpointData::new(addrs)),
            "sled-peerbook",
            Some(e.ts * 1_000_000),
        );
        Some(stream::iter(Some(Ok(item))).boxed())
    }
}

/// Запись старше — считаем устаревшей и не используем (пир мог сменить реле/IP).
const TTL_SECS: u64 = 30 * 86_400;
/// Максимум адресов на пира (активные первыми).
const MAX_ADDRS: usize = 8;
/// Потолок записей в книге — защита от раздувания sled чужими входящими
/// соединениями (DoS через мусорных пиров). При переполнении вытесняются самые старые.
const MAX_ENTRIES: usize = 4000;

/// Роль пира в момент контакта — тип соединения NETFORY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// sth-core узел: REST over iroh + стрим `sth/events/1` (кошелёк)
    Wallet,
    /// провайдер `api://` (dApp-бэкенды, не sth-core)
    Api,
    /// собеседник мессенджера (DM v1/v2)
    Chat,
    /// сидер / провайдер blobs (iroh-blobs)
    Seed,
    /// AI-нода (`netfory/ai/1`)
    Ai,
}

impl Role {
    fn tag(self) -> &'static str {
        match self {
            Role::Wallet => "wallet",
            Role::Api => "api",
            Role::Chat => "chat",
            Role::Seed => "seed",
            Role::Ai => "ai",
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Entry {
    addrs: Vec<String>,
    ts: u64,
    #[serde(default)]
    roles: Vec<String>,
}

/// Публичный снимок записи для диагностики/UI.
#[derive(Serialize)]
pub(crate) struct PeerEntry {
    pub node_id: String,
    pub addrs: Vec<String>,
    pub ts: u64,
    pub roles: Vec<String>,
}

fn key(id: &EndpointId) -> String {
    format!("p2p:peerbook:{}", id)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn encode(a: &TransportAddr) -> Option<String> {
    match a {
        TransportAddr::Relay(u) => Some(format!("relay:{}", u)),
        TransportAddr::Ip(s) => Some(format!("ip:{}", s)),
        _ => None,
    }
}

pub(crate) fn decode(s: &str) -> Option<TransportAddr> {
    if let Some(u) = s.strip_prefix("relay:") {
        return u.parse::<RelayUrl>().ok().map(TransportAddr::Relay);
    }
    if let Some(sock) = s.strip_prefix("ip:") {
        return sock.parse().ok().map(TransportAddr::Ip);
    }
    None
}

/// Известные адреса пира из sled (свежие) или `None`.
pub(crate) fn cached(id: &EndpointId) -> Option<EndpointAddr> {
    let raw = DB.get(key(id).as_bytes()).ok().flatten()?;
    let e: Entry = serde_json::from_slice(&raw).ok()?;
    if now().saturating_sub(e.ts) > TTL_SECS {
        return None;
    }
    let addrs: BTreeSet<TransportAddr> = e.addrs.iter().filter_map(|s| decode(s)).collect();
    if addrs.is_empty() {
        return None;
    }
    Some(EndpointAddr::from_parts(*id, addrs))
}

/// После успешного соединения — запомнить реальные адреса пира (активные первыми)
/// и роль, в которой он нам встретился (роли накапливаются).
pub(crate) async fn remember(endpoint: &Endpoint, id: EndpointId, role: Role) {
    let Some(info) = endpoint.remote_info(id).await else { return };
    let mut active: Vec<String> = Vec::new();
    let mut other: Vec<String> = Vec::new();
    for a in info.addrs() {
        let Some(s) = encode(a.addr()) else { continue };
        if matches!(a.usage(), iroh::endpoint::TransportAddrUsage::Active) {
            active.push(s);
        } else {
            other.push(s);
        }
    }
    active.extend(other);
    active.dedup();
    active.truncate(MAX_ADDRS);
    if active.is_empty() {
        return;
    }
    let prev = DB
        .get(key(&id).as_bytes())
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_slice::<Entry>(&raw).ok());
    if prev.is_none() {
        evict_if_full();
    }
    let mut roles = prev.map(|e| e.roles).unwrap_or_default();
    if !roles.iter().any(|r| r == role.tag()) {
        roles.push(role.tag().to_string());
    }
    let entry = Entry { addrs: active, ts: now(), roles };
    if let Ok(js) = serde_json::to_vec(&entry) {
        let _ = DB.insert(key(&id).as_bytes(), js);
    }
    // sth-core узел пула → поделиться его адресами с другими клиентами через трекер
    if role == Role::Wallet {
        crate::sth_hints::maybe_report(&id, &entry.addrs);
    }
}

/// Все записи книги (для диагностики/UI), новые первыми; `role` — фильтр по роли.
pub(crate) fn list(role: Option<Role>) -> Vec<PeerEntry> {
    let pfx = "p2p:peerbook:";
    let mut v: Vec<PeerEntry> = DB
        .scan_prefix(pfx.as_bytes())
        .filter_map(|kv| kv.ok())
        .filter_map(|(k, raw)| {
            let node_id = std::str::from_utf8(&k).ok()?.strip_prefix(pfx)?.to_string();
            let e: Entry = serde_json::from_slice(&raw).ok()?;
            if let Some(r) = role {
                if !e.roles.iter().any(|x| x == r.tag()) {
                    return None;
                }
            }
            Some(PeerEntry { node_id, addrs: e.addrs, ts: e.ts, roles: e.roles })
        })
        .collect();
    v.sort_by(|a, b| b.ts.cmp(&a.ts));
    v
}

/// Tauri: книга пиров по типу соединения (`wallet|api|chat|seed|ai`, пусто — все).
#[tauri::command]
pub(crate) fn peerbook_list(role: Option<String>) -> Vec<PeerEntry> {
    let r = match role.as_deref() {
        Some("wallet") => Some(Role::Wallet),
        Some("api") => Some(Role::Api),
        Some("chat") => Some(Role::Chat),
        Some("seed") => Some(Role::Seed),
        Some("ai") => Some(Role::Ai),
        _ => None,
    };
    list(r)
}

/// Книга переполнена — удалить самые старые 5 % записей (по `ts`).
fn evict_if_full() {
    let pfx = b"p2p:peerbook:";
    let mut items: Vec<(sled::IVec, u64)> = DB
        .scan_prefix(pfx)
        .filter_map(|kv| kv.ok())
        .map(|(k, raw)| (k, serde_json::from_slice::<Entry>(&raw).map(|e| e.ts).unwrap_or(0)))
        .collect();
    if items.len() < MAX_ENTRIES {
        return;
    }
    items.sort_by_key(|(_, ts)| *ts);
    for (k, _) in items.iter().take(MAX_ENTRIES / 20 + 1) {
        let _ = DB.remove(k);
    }
    crate::syslog::log("net", format!("peerbook: переполнение, вытеснено {} старых пиров", MAX_ENTRIES / 20 + 1));
}

/// Кэш пира оказался мёртвым — забыть, чтобы следующий connect шёл через discovery.
pub(crate) fn forget(id: &EndpointId) {
    let _ = DB.remove(key(id).as_bytes());
}

/// Сколько пиров в книге (для диагностики/UI).
pub(crate) fn count() -> usize {
    DB.scan_prefix(b"p2p:peerbook:").count()
}
