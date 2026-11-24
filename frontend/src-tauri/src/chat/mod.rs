// ============================================================
//  SmartNet // chat.rs  —  E2EE P2P messenger engine (Phase 1)
//
//  • Identity keys: an X25519 keypair is DETERMINISTICALLY derived from the
//    wallet seed (SHA-256 of the secp256k1 secret). The public key is published
//    on-chain (vendorField `xkey:<b64>`) so any peer resolving `u://name` also
//    resolves the encryption key — full Web3 trust, no handshake/MITM window.
//  • Rooms: ECDH (my secret × peer pub) → SHA-256 = Room ID (future Gossipsub
//    topic). The shared secret NEVER touches disk — recomputed in RAM on demand.
//  • Messages: AES-256-GCM (key = shared secret), stored encrypted in Sled under
//    `chat:msg:<room>:<ts>:<id>`. Rolling buffer caps history at 1000/room.
//  • Anti-spam: inbound packets from senders with NO registered name are dropped.
//
//  Phase 1 delivers the full local engine + crypto + storage; live cross-machine
//  Gossipsub transport lands in Phase 2.
//
//  Author: TechnoL0g
//  Created: 2026-06-20
// ============================================================

// Подмодули мессенджера (вынесены для будущего отделения в отдельное
// Android-приложение: криптография ключей, баны, gossip-транспорт).
// ===================== Address book (обмен контактами БЕЗ блокчейна) ========
// Контакт = адрес + username + X25519-ключ + iroh endpoint id, упакованные в
// ссылку `sth://contact?a=..&k=..&u=..&n=..` (аналог share-ссылок p2p mail).
// chat_add_contact пишет ключи напрямую в кэши key_cache/node_cache — резолв
// чата работает сразу, без сканирования блокчейна и без публикации xkey.

fn abook_key(addr: &str) -> String {
    format!("chat:abook:{}", addr)
}

fn urlenc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn urldec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ContactCard {
    pub address: String,
    #[serde(default)]
    pub username: String,
    pub key: String, // b64 X25519 (32 байта)
    #[serde(default)]
    pub node: String, // iroh endpoint id (64 hex)
    #[serde(default, rename = "addedAt")]
    pub added_at: u64,
}

/// Моя контакт-ссылка для обмена: всё, что нужно собеседнику, без блокчейна.
/// Схема sn:// (системное пространство, как sn://seed/…) — НЕ sth://, чтобы
/// не пересекаться с доменными именами dApps (contact можно зарегистрировать).
#[tauri::command]
pub(crate) async fn chat_my_contact_link(app: tauri::AppHandle) -> Result<String, String> {
    let addr = my_address().ok_or("no account")?;
    let key = get_my_chat_pubkey()?;
    let node = my_node_id(&app).await.unwrap_or_default();
    let user = crate::naming::username_of_address(addr.clone()).unwrap_or_default();
    let mut link = format!("sn://contact?a={}&k={}", addr, urlenc(&key));
    if !user.is_empty() {
        link.push_str(&format!("&u={}", urlenc(&user)));
    }
    if !node.is_empty() {
        link.push_str(&format!("&n={}", node));
    }
    Ok(link)
}

/// Добавить контакт из ссылки: валидация + запись в адресную книгу и кэши.
#[tauri::command]
pub(crate) fn chat_add_contact(link: String) -> Result<ContactCard, String> {
    let trimmed = link.trim();
    // Канонично sn://contact?…; sth://contact?… принимаем как legacy.
    let q = trimmed
        .strip_prefix("sn://contact?")
        .or_else(|| trimmed.strip_prefix("sth://contact?"))
        .ok_or("bad-link-format")?;
    let (mut address, mut key, mut node, mut username) =
        (String::new(), String::new(), String::new(), String::new());
    for pair in q.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = urldec(v);
        match k {
            "a" => address = v,
            "k" => key = v,
            "n" => node = v,
            "u" => username = v,
            _ => {}
        }
    }
    let addr_ok = address.len() >= 26
        && address.len() <= 40
        && address.starts_with('S')
        && address.chars().all(|c| c.is_ascii_alphanumeric());
    if !addr_ok {
        return Err("bad-address".into());
    }
    if !crypto::b64d(&key).map(|b| b.len() == 32).unwrap_or(false) {
        return Err("bad-key".into());
    }
    if !(node.len() == 64 && node.chars().all(|c| c.is_ascii_hexdigit())) {
        node.clear(); // endpoint id опционален — без него boot из xkey-кэша
    }
    if my_address().as_deref() == Some(address.as_str()) {
        return Err("own-contact".into());
    }
    // Кэши — резолв ключа/ноды в чате теперь работает без блокчейна.
    let _ = DB.insert(key_cache(&address).as_bytes(), key.as_bytes());
    if !node.is_empty() {
        let _ = DB.insert(node_cache(&address).as_bytes(), node.as_bytes());
    }
    let card = ContactCard {
        address: address.clone(),
        username,
        key,
        node,
        added_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let _ = DB.insert(
        abook_key(&address).as_bytes(),
        serde_json::to_vec(&card).map_err(|e| e.to_string())?,
    );
    let _ = DB.flush();
    Ok(card)
}

#[tauri::command]
pub(crate) fn chat_list_contacts() -> Vec<ContactCard> {
    let mut out: Vec<ContactCard> = DB
        .scan_prefix(b"chat:abook:")
        .filter_map(|kv| kv.ok())
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect();
    out.sort_by(|a: &ContactCard, b: &ContactCard| b.added_at.cmp(&a.added_at));
    out
}

#[tauri::command]
pub(crate) fn chat_remove_contact(address: String) {
    let _ = DB.remove(abook_key(&address).as_bytes());
    let _ = DB.flush();
}

pub(crate) mod bans;
pub(crate) mod crypto;
pub(crate) mod media;
pub(crate) mod media_seeders;

use crate::chat::bans::{add_ban, load_bans, prune_bans, remove_ban, save_bans, BanEntry};
use crate::chat::crypto::{
    b64d, b64e, decrypt, encrypt, group_key_from_secret, my_secret, shared_key,
};
use crate::{naming, subs, sth_node, DB, SESSION_SEED};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::PublicKey;

/// On-chain registry sink for identity-key publication (shared with naming).
const KEY_SINK: &str = "SeeQyvfpghaFLjVfLUTxLvpQq78LMQnAmE";
/// Max retained messages per room (rolling buffer).
const MAX_MSGS: usize = 1000;
/// Cap auto-subscriptions so we never flood the node with gossip topics. Friends
/// beyond this are reached on-demand (when their chat is opened / on activity).
const AUTO_SUB_LIMIT: usize = 50;
/// 1 STH = 100_000_000 smarthoshi.
const STH: u64 = 100_000_000;

// ---- chat settings (Sled config) ---------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct ChatSettings {
    #[serde(rename = "onlyAllowTrusted")]
    only_allow_trusted: bool,
    notifications: bool,
    /// My message price in STH (whole units) others must pay to message me.
    #[serde(rename = "msgCostSth")]
    msg_cost_sth: f64,
    /// True once my price is published on-chain.
    #[serde(rename = "costPublished")]
    cost_published: bool,
    /// Auto-subscribe to followed peers' gossip topics (resource toggle).
    #[serde(rename = "autoSubscribe")]
    auto_subscribe: bool,
}

fn cfg_get(key: &str) -> Option<Vec<u8>> {
    DB.get(key.as_bytes()).ok().flatten().map(|v| v.to_vec())
}

fn only_allow_trusted() -> bool {
    matches!(cfg_get("chat:cfg:trusted"), Some(v) if v == b"1")
}
pub(crate) fn notifications_enabled() -> bool {
    // default ON unless explicitly disabled
    !matches!(cfg_get("chat:cfg:notify"), Some(v) if v == b"0")
}
/// Auto-subscribe to followed peers' gossip topics (default ON).
fn auto_subscribe_enabled() -> bool {
    !matches!(cfg_get("chat:cfg:autosub"), Some(v) if v == b"0")
}
/// My own message price, in smarthoshi.
fn my_msg_cost() -> u64 {
    cfg_get("chat:cfg:mcost")
        .and_then(|v| String::from_utf8(v).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[tauri::command]
pub(crate) fn get_chat_settings() -> ChatSettings {
    let cost = my_msg_cost();
    ChatSettings {
        only_allow_trusted: only_allow_trusted(),
        notifications: notifications_enabled(),
        msg_cost_sth: cost as f64 / STH as f64,
        cost_published: my_address()
            .map(|a| cfg_get(&format!("chat:mcost:{}", a)).is_some())
            .unwrap_or(false),
        auto_subscribe: auto_subscribe_enabled(),
    }
}

#[tauri::command]
pub(crate) fn set_chat_settings(
    only_allow_trusted: bool,
    notifications: bool,
    msg_cost_sth: f64,
    auto_subscribe: bool,
) -> Result<(), String> {
    let _ = DB.insert(b"chat:cfg:trusted", if only_allow_trusted { b"1" as &[u8] } else { b"0" });
    let _ = DB.insert(b"chat:cfg:notify", if notifications { b"1" as &[u8] } else { b"0" });
    let _ = DB.insert(b"chat:cfg:autosub", if auto_subscribe { b"1" as &[u8] } else { b"0" });
    let cost = (msg_cost_sth.max(0.0) * STH as f64).round() as u64;
    let _ = DB.insert(b"chat:cfg:mcost", cost.to_string().as_bytes());
    let _ = DB.flush();
    Ok(())
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct Room {
    id: String,
    #[serde(rename = "peerAddress")]
    peer_address: String,
    #[serde(rename = "peerName")]
    peer_name: Option<String>,
    #[serde(rename = "peerKey")]
    peer_key: String, // base64 X25519 pub
    #[serde(rename = "lastText")]
    last_text: String,
    #[serde(rename = "lastTs")]
    last_ts: u64,
    /// "direct" (1:1 ECDH) or "group" (shared-secret broadcast topic).
    #[serde(default = "kind_direct")]
    kind: String,
    /// For groups: the hex 32-byte shared secret (= invite token). None for direct.
    #[serde(rename = "groupSecret", default)]
    group_secret: Option<String>,
    /// Unread inbound messages since the room was last opened.
    #[serde(default)]
    unread: u64,
    /// Group owner's ledger address (the creator). None for direct chats.
    #[serde(default)]
    owner: Option<String>,
    /// Latest owner-signed group policy (monetization / access / moderation).
    #[serde(default)]
    policy: Option<GroupPolicy>,
    /// Current rotated AES key (hex 32B). None => derive from `group_secret`
    /// (initial key). The gossip TOPIC stays `SHA256(group_secret)` so rotation
    /// only swaps the encryption key, never the room/topic identity.
    #[serde(rename = "groupKey", default)]
    group_key: Option<String>,
    /// Monotonic key-rotation interval. Members ignore rotations ≤ this.
    #[serde(rename = "keyVersion", default)]
    key_version: u64,
}

/// Owner-controlled group policy, synced as a signed gossip control message.
#[derive(Serialize, Deserialize, Clone, Default)]
pub(crate) struct GroupPolicy {
    /// Entry fee in smarthoshi (display ÷ STH). Advisory unless `paid_entry`.
    #[serde(rename = "entryFee", default)]
    entry_fee: u64,
    #[serde(rename = "paidEntry", default)]
    paid_entry: bool,
    #[serde(rename = "inviteOnly", default)]
    invite_only: bool,
    /// Channel mode: only the owner may publish to the main topic; members'
    /// posts are hidden from the main feed (client-filtered).
    #[serde(rename = "channelMode", default)]
    channel_mode: bool,
    /// Pinned message id (owner-set, shown as a banner to all members). Empty/None = none.
    #[serde(rename = "pinned", default)]
    pinned: Option<String>,
    /// Addresses kicked by the owner (their messages are dropped by members).
    #[serde(default)]
    banned: Vec<String>,
    /// Логотип группы: хэш blob'а (≤128КБ PNG/JPG) + media-node владельца.
    /// Пустая строка = логотипа нет. Клиенты качают через chat_load_image.
    #[serde(rename = "logoHash", default)]
    logo_hash: String,
    #[serde(rename = "logoNode", default)]
    logo_node: String,
    /// Monotonic version (ms). Stale updates are ignored.
    #[serde(default)]
    ts: u64,
}

/// Signed envelope broadcast on the group topic when the owner changes policy.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct GroupPolicyUpdate {
    policy: GroupPolicy,
    topic: String,
    #[serde(rename = "ownerPubkey")]
    owner_pubkey: String,
    sig: String,
}

fn kind_direct() -> String {
    "direct".to_string()
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct ChatMsg {
    id: String,
    room: String,
    sender: String,
    text: String,
    ts: u64,
    mine: bool,
    /// System/service event ("join", …). None = a normal user message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sys: Option<String>,
    /// unix-ms последнего редактирования автором (None = не редактировалось)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    edited: Option<u64>,
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---- identity keys -------------------------------------------------------

fn my_address() -> Option<String> {
    crate::load_vault_internal().map(|v| v.address)
}

/// My X25519 public key (base64) — the value published on-chain.
#[tauri::command]
pub(crate) fn get_my_chat_pubkey() -> Result<String, String> {
    let pk = PublicKey::from(&my_secret()?);
    Ok(b64e(pk.as_bytes()))
}

fn key_cache(addr: &str) -> String {
    format!("chat:key:{}", addr)
}

/// Resolve an address's published X25519 key (cache → self → none).
#[tauri::command]
pub(crate) fn resolve_chat_key(address: String) -> Option<String> {
    if let Some(me) = my_address() {
        if me == address {
            return get_my_chat_pubkey().ok();
        }
    }
    DB.get(key_cache(&address).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
}

/// True once BOTH my X25519 key AND my iroh EndpointId are on-chain. Returning
/// false for a key-only (legacy) publish re-prompts the user so a fresh publish
/// adds the EndpointId — without it gossip has no bootstrap and messages can't
/// be delivered.
#[tauri::command]
pub(crate) fn chat_key_published() -> bool {
    my_address()
        .map(|a| {
            DB.get(key_cache(&a).as_bytes()).ok().flatten().is_some()
                && DB.get(node_cache(&a).as_bytes()).ok().flatten().is_some()
        })
        .unwrap_or(false)
}

/// Актуальность моей on-chain записи xkey (для панели настроек мессенджера).
#[derive(Serialize, Clone)]
pub(crate) struct ChatKeyStatus {
    /// Ключ + NodeId опубликованы (локальный кэш публикации заполнен).
    published: bool,
    /// Опубликованная запись устарела: текущий NodeId или X25519-ключ
    /// отличаются от опубликованных — нужна обновляющая транзакция.
    stale: bool,
    /// Текущий iroh EndpointId (пусто без p2p / до старта endpoint).
    node: String,
    /// NodeId из опубликованной записи (пусто, если не публиковали).
    #[serde(rename = "publishedNode")]
    published_node: String,
}

/// Статус публикации ключа: опубликован ли и совпадает ли запись с текущими
/// ключом/NodeId. UI показывает кнопку публикации ТОЛЬКО когда tx реально нужна.
#[tauri::command]
pub(crate) async fn chat_key_status(app: tauri::AppHandle) -> ChatKeyStatus {
    let addr = match my_address() {
        Some(a) => a,
        None => {
            return ChatKeyStatus {
                published: false,
                stale: false,
                node: String::new(),
                published_node: String::new(),
            }
        }
    };
    let cached_key = cfg_get(&key_cache(&addr))
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_default();
    let published_node = cfg_get(&node_cache(&addr))
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_default();
    let published = !cached_key.is_empty() && !published_node.is_empty();
    let node = my_node_id(&app).await.unwrap_or_default();
    let cur_key = get_my_chat_pubkey().unwrap_or_default();
    let node_changed = !node.is_empty() && node != published_node;
    let key_changed = !cur_key.is_empty() && cur_key != cached_key;
    ChatKeyStatus {
        published,
        stale: published && (node_changed || key_changed),
        node,
        published_node,
    }
}

/// A peer's published per-message price in STH (whole units). 0 = free.
#[tauri::command]
pub(crate) fn resolve_msg_cost(address: String) -> f64 {
    peer_msg_cost(&address) as f64 / STH as f64
}

#[derive(Serialize, Clone)]
pub(crate) struct GossipStatus {
    /// Iroh gossip protocol is live on the endpoint.
    online: bool,
    /// Active topic subscriptions (one per open room).
    topics: usize,
    /// Whether this build includes the P2P transit feature at all.
    p2p: bool,
    /// Комнаты в процессе переподписки (цикл жив, live-sender отсутствует).
    resubscribing: usize,
    /// Пакетов в outbox-очередях (не ушли в mesh, ждут досылки).
    outbox: usize,
}

/// Live status of the Iroh Gossipsub transit engine (for the messenger HUD).
#[tauri::command]
pub(crate) async fn gossip_status() -> GossipStatus {
    #[cfg(feature = "p2p")]
    {
        gossip::status()
    }
    #[cfg(not(feature = "p2p"))]
    {
        GossipStatus { online: false, topics: 0, p2p: false, resubscribing: 0, outbox: 0 }
    }
}

/// Number of live gossip neighbors on a room's topic (peer presence / delivery).
#[tauri::command]
pub(crate) async fn gossip_room_peers(room_id: String) -> usize {
    #[cfg(feature = "p2p")]
    {
        gossip::room_peer_count(&room_id)
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = room_id;
        0
    }
}

/// The network fee (STH string) that a key/cost publish would use right now —
/// so the UI can show the cost before the user confirms the transaction.
#[tauri::command]
pub(crate) async fn recommended_fee() -> String {
    key_publish_fee(114, None)
}

/// Минимальная комиссия публикации ключа/цены сообщений: 0.10 STH.
const KEY_PUBLISH_MIN_FEE: u64 = sth_node::SMARTHOSHI / 10;

/// Комиссия key/cost-публикации (STH-строка): максимум из минимума 0.10 STH,
/// динамической size-aware комиссии узла (xkey: ~114 байт → ~0.011 STH) и
/// ручного override (CHAT_KEY_FEE_STH) — override не может опустить ниже минимума.
fn key_publish_fee(vendor_len: usize, fee_override: Option<String>) -> String {
    let dynamic = sth_node::transfer_dynamic_fee(vendor_len);
    let manual = fee_override.and_then(|f| sth_node::parse_sth(&f).ok()).unwrap_or(0);
    sth_node::smarthoshi_to_sth_exact(KEY_PUBLISH_MIN_FEE.max(dynamic).max(manual))
}

/// Self-транзакции адреса (recipientId == senderId) — личный on-chain реестр
/// пользователя (записи xkey:/mcost:, как donate:). Инъекция второго параметра
/// в value легальна: fetch_txs подставляет строку прямо в query.
fn fetch_txs_self(addr: &str, limit: u32) -> Result<Vec<sth_node::TxLite>, String> {
    if crate::txindex::is_complete(addr) {
        // свой адрес: личный реестр из локального индекса (новые первыми), без сети
        let mut v = Vec::new();
        crate::txindex::for_each(addr, |t| {
            if t["sender"].as_str() == Some(addr) && t["recipient"].as_str() == Some(addr) {
                v.push(sth_node::tx_lite(t, None));
            }
            v.len() < limit as usize
        });
        return Ok(v);
    }
    sth_node::fetch_txs("recipientId", &format!("{addr}&senderId={addr}"), limit)
}

/// Фаза 1 (v1.135.45): свежайшая запись с данным префиксом из ЛИЧНОГО реестра
/// (self-tx) моего адреса. Идемпотентность публикаций: такая же запись уже на
/// цепи — транзакцию не шлём. Blocking — только из spawn_blocking.
fn my_latest_self_record(me: &str, prefix: &str) -> Option<String> {
    let txs = fetch_txs_self(me, 50).ok()?;
    txs.into_iter().find_map(|t| {
        if t.recipient == me && t.vendor.starts_with(prefix) {
            Some(t.vendor)
        } else {
            None
        }
    })
}

/// Точечный резолв ключа контакта из его личного self-tx реестра + запись в
/// кэши. Заменяет глобальный скан sink (не масштабировался: окно 100 tx).
/// Blocking (сеть) — вызывать из worker/блокирующих потоков.
pub(crate) fn refresh_key_from_chain(addr: &str) -> Option<String> {
    let txs = fetch_txs_self(addr, 50).ok()?;
    for t in txs {
        if t.recipient != addr {
            continue;
        }
        if let Some(k) = t.vendor.strip_prefix("xkey:") {
            let mut parts = k.trim().splitn(2, '|');
            let key = parts.next().unwrap_or("").trim().to_string();
            let node = parts.next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            if crypto::b64d(&key).map(|b| b.len() == 32).unwrap_or(false) {
                let _ = DB.insert(key_cache(addr).as_bytes(), key.as_bytes());
                if let Some(n) = &node {
                    let _ = DB.insert(node_cache(addr).as_bytes(), n.as_bytes());
                }
                let _ = DB.flush();
                return Some(key);
            }
        }
    }
    None
}

/// Обновление ключей контактов (кого фолловлю + мои подписчики) из их личных
/// self-tx реестров, TTL 10 мин на адрес. Вызывается из потока msg-индексера.
pub(crate) fn refresh_contact_keys() {
    let me = match my_address() {
        Some(a) => a,
        None => return,
    };
    let mut addrs = cached_following(&me);
    for f in cached_followers(&me) {
        if !addrs.contains(&f) {
            addrs.push(f);
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for a in addrs.into_iter().take(60) {
        let at_key = format!("chat:keyat:{}", a);
        let last: u64 = DB
            .get(at_key.as_bytes())
            .ok()
            .flatten()
            .and_then(|v| String::from_utf8(v.to_vec()).ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if now.saturating_sub(last) < 600 {
            continue;
        }
        let _ = refresh_key_from_chain(&a);
        let _ = DB.insert(at_key.as_bytes(), now.to_string().as_bytes());
    }
}

// Debounce публикаций ключа: не чаще одного tx в 30с — гасит штормы по
// 3-4 одинаковых xkey-tx в секунду (двойные вызовы UI/рестарты).
static LAST_KEY_PUBLISH: once_cell::sync::Lazy<std::sync::Mutex<Option<std::time::Instant>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(None));

/// Publish my X25519 key + iroh EndpointId on-chain (vendorField
/// `xkey:<b64pub>|<endpointid>`) + cache locally. The EndpointId lets peers
/// bootstrap the gossip topic so messages can actually be delivered.
#[tauri::command]
pub(crate) async fn publish_chat_key(app: tauri::AppHandle, fee_sth: Option<String>) -> Result<String, String> {
    let pubk = get_my_chat_pubkey()?;
    let addr = my_address().ok_or("no account")?;
    let node = my_node_id(&app).await;
    let secret = SESSION_SEED.lock().unwrap().clone().ok_or("locked")?;
    let vendor = match &node {
        Some(n) => format!("xkey:{}|{}", pubk, n),
        None => format!("xkey:{}", pubk),
    };
    let fee_override = fee_sth.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    // Debounce: параллельные/повторные вызовы в течение 30с не создают дублей.
    {
        let mut g = LAST_KEY_PUBLISH.lock().unwrap();
        if let Some(t) = *g {
            if t.elapsed() < std::time::Duration::from_secs(30) {
                crate::syslog::log("chat", "publish_chat_key: debounce (<30с с прошлой публикации)");
                return Ok("debounced".into());
            }
        }
        *g = Some(std::time::Instant::now());
    }
    let addr_chk = addr.clone();
    let txid_res = tauri::async_runtime::spawn_blocking(move || {
        // Идемпотентность: точно такая же запись уже на цепи — не дублируем.
        let latest = my_latest_self_record(&addr_chk, "xkey:");
        if latest.as_deref() == Some(vendor.as_str()) {
            crate::syslog::log("chat", "publish_chat_key: запись xkey уже в цепи без изменений — tx не нужна");
            return Ok::<String, String>("already-on-chain".into());
        }
        // Size-aware dynamic fee: the vendorField (xkey:/mcost:) enlarges the tx,
        // so the generic /node/fees min undershoots and the node rejects it.
        let fee = key_publish_fee(vendor.len(), fee_override);
        crate::syslog::log(
            "chat",
            format!(
                "publish_chat_key: отправка xkey ({} байт, fee {} STH, прежняя запись: {})",
                vendor.len(),
                fee,
                if latest.is_some() { "есть" } else { "нет" }
            ),
        );
        // Фаза 1: публикация СЕБЕ (recipient == sender) — личный реестр,
        // amount возвращается, тратится только fee. Общий sink больше не спамим.
        let out = sth_node::send_transfer(&secret, &addr_chk, "0.00000001", &fee, &vendor).map_err(|e| {
            crate::syslog::log("chat", format!("publish_chat_key: send_transfer: {}", e));
            e
        })?;
        if !out.accepted {
            crate::syslog::log("chat", format!("publish_chat_key: узел отклонил tx: {}", out.reason));
            return Err(if out.reason.is_empty() {
                "Транзакция отклонена узлом".to_string()
            } else {
                format!("Транзакция отклонена: {}", out.reason)
            });
        }
        crate::syslog::log("chat", format!("publish_chat_key: принята tx {}", out.id));
        Ok::<String, String>(out.id)
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    let txid = match txid_res {
        Ok(t) => t,
        Err(e) => {
            // Ошибка — снимаем debounce, чтобы пользователь мог повторить сразу.
            *LAST_KEY_PUBLISH.lock().unwrap() = None;
            return Err(e);
        }
    };
    let _ = DB.insert(key_cache(&addr).as_bytes(), pubk.as_bytes());
    if let Some(n) = node {
        let _ = DB.insert(node_cache(&addr).as_bytes(), n.as_bytes());
    }
    let _ = DB.flush();
    Ok(txid)
}

/// Self-heal helper: if our current iroh EndpointId no longer matches the one
/// we last published on-chain (e.g. node.key was regenerated, or an old build
/// published an ephemeral id), re-publish the `xkey:` record ONCE so peers stop
/// bootstrapping the 1:1 topic against a dead identity. Cost-safe: only fires on
/// an actual change, and only if a key was already published (never auto-spends
/// for a fresh account) and the vault is unlocked.
#[cfg(feature = "p2p")]
pub(crate) async fn republish_endpoint_if_changed() {
    let addr = match my_address() {
        Some(a) => a,
        None => return,
    };
    let prev_node = cfg_get(&node_cache(&addr))
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_default();
    if prev_node.is_empty() {
        return; // never published before — don't auto-spend
    }
    let app = match gossip::app_handle() {
        Some(a) => a,
        None => return,
    };
    let cur = match my_node_id(&app).await {
        Some(n) => n,
        None => return,
    };
    if cur == prev_node {
        return; // unchanged — on-chain record is still valid
    }
    let pubk = match get_my_chat_pubkey() {
        Ok(p) => p,
        Err(_) => return,
    };
    let secret = match SESSION_SEED.lock().unwrap().clone() {
        Some(s) => s,
        None => return, // locked — skip; will retry next session
    };
    let vendor = format!("xkey:{}|{}", pubk, cur);
    let addr2 = addr.clone();
    let node2 = cur.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        // Идемпотентность: локальный кэш мог быть стёрт (TEMP-очистка), а запись
        // на цепи уже актуальна — тогда только чиним кэш, без транзакции.
        if my_latest_self_record(&addr2, "xkey:").as_deref() == Some(vendor.as_str()) {
            let _ = DB.insert(node_cache(&addr2).as_bytes(), node2.as_bytes());
            let _ = DB.insert(key_cache(&addr2).as_bytes(), pubk.as_bytes());
            let _ = DB.flush();
            return;
        }
        let fee = key_publish_fee(vendor.len(), None);
        if let Ok(out) = sth_node::send_transfer(&secret, &addr2, "0.00000001", &fee, &vendor) {
            if out.accepted {
                let _ = DB.insert(node_cache(&addr2).as_bytes(), node2.as_bytes());
                let _ = DB.insert(key_cache(&addr2).as_bytes(), pubk.as_bytes());
                let _ = DB.flush();
            }
        }
    })
    .await;
}

fn cost_cache(addr: &str) -> String {
    format!("chat:mcost:{}", addr)
}

fn node_cache(addr: &str) -> String {
    format!("chat:node:{}", addr)
}

/// A peer's published iroh EndpointId (for gossip topic bootstrap), if any.
pub(crate) fn peer_node_id(addr: &str) -> Option<String> {
    cfg_get(&node_cache(addr))
        .and_then(|v| String::from_utf8(v).ok())
        .filter(|s| !s.is_empty())
}

/// Опубликованный (в блокчейне/кэше) iroh EndpointId пира по его адресу.
/// Переиспользуется соц-слоем для bootstrap-подписки на стену автора.
pub(crate) fn published_endpoint(addr: &str) -> Option<String> {
    peer_node_id(addr)
}

/// Our own stable iroh EndpointId (None without the p2p feature / before the
/// endpoint is up).
async fn my_node_id(app: &tauri::AppHandle) -> Option<String> {
    #[cfg(feature = "p2p")]
    {
        crate::p2p::node_id(app).await.ok().filter(|s| !s.is_empty())
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = app;
        None
    }
}

/// Network fee (STH, as a string) for key/cost publish txs. The frontend may
/// supply a manual override from `frontend/.env` (CHAT_KEY_FEE_STH); when empty,
/// the publish auto-resolves the node's recommended dynamic transfer fee.

// Cached "following" set (anti-spam trust list). The lookup hits the chain
// (blocking HTTP), so we memoize it for 60s — critical to avoid hammering the
// node on every inbound packet AND to keep this off hot paths (5 мин: полный
// постраничный обход исходящих tx — до 30 запросов).
static FOLLOW_CACHE: once_cell::sync::Lazy<std::sync::Mutex<Option<(std::time::Instant, Vec<String>)>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(None));

/// Blocking: my trust list (subscriptions), cached 60s. NEVER call from an async
/// runtime task without spawn_blocking — it performs a blocking network request.
pub(crate) fn cached_following(me: &str) -> Vec<String> {
    if let Some((t, v)) = FOLLOW_CACHE.lock().unwrap().as_ref() {
        if t.elapsed() < std::time::Duration::from_secs(300) {
            return v.clone();
        }
    }
    let fresh = subs::following_addresses(me.to_string());
    *FOLLOW_CACHE.lock().unwrap() = Some((std::time::Instant::now(), fresh.clone()));
    fresh
}

// Кэш МОИХ подписчиков (кто зафолловил меня) — для обратной auto-subscribe.
static FOLLOWER_CACHE: once_cell::sync::Lazy<std::sync::Mutex<Option<(std::time::Instant, Vec<String>)>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(None));

/// Blocking: адреса моих подписчиков, кэш 60с (сетевой запрос — только из
/// spawn_blocking/std-потока).
pub(crate) fn cached_followers(me: &str) -> Vec<String> {
    if let Some((t, v)) = FOLLOWER_CACHE.lock().unwrap().as_ref() {
        if t.elapsed() < std::time::Duration::from_secs(300) {
            return v.clone();
        }
    }
    let fresh = subs::subscriber_addresses(me.to_string());
    *FOLLOWER_CACHE.lock().unwrap() = Some((std::time::Instant::now(), fresh.clone()));
    fresh
}

// Topics we auto-subscribed for "first contact" (people we follow) but for
// which no Room exists yet: room_id -> peer address. Lets `handle_inbound`
// lazily materialize the Room only when an actual message arrives (keeps the
// chat list clean instead of pre-creating empty rooms for everyone we follow).
static FOLLOW_TOPICS: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashMap<String, String>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Deterministic room id (= gossip TopicId) for a peer whose X25519 key we know.
fn room_id_for_addr(addr: &str) -> Option<String> {
    let peer_key = resolve_chat_key(addr.to_string())?;
    let sk = shared_key(&peer_key).ok()?;
    Some(hex::encode(Sha256::digest(sk)))
}

/// Auto-subscribe to the gossip topic of everyone we follow whose key is known,
/// so first-contact messages from acquaintances arrive WITHOUT opening the chat.
/// Idempotent; safe to call repeatedly (retries as keys get indexed).
pub(crate) fn subscribe_following() {
    if !auto_subscribe_enabled() {
        return;
    }
    let me = match my_address() {
        Some(a) => a,
        None => return,
    };
    // Prioritize by last-contact recency so the capped subscription budget goes
    // to people we actually talk to. Direct rooms carry the last activity ts;
    // never-contacted follows fall to the back.
    let mut last_seen: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for kv in DB.scan_prefix(b"chat:room:") {
        if let Ok((_, v)) = kv {
            if let Ok(r) = serde_json::from_slice::<Room>(&v) {
                if r.kind != "group" {
                    last_seen.insert(r.peer_address, r.last_ts);
                }
            }
        }
    }
    let mut follows = cached_following(&me);
    // КРИТИЧНО (двусторонняя связность): подписываемся и на топики МОИХ
    // ПОДПИСЧИКОВ. Иначе новичок, который зафолловил нас и написал первым,
    // вещает в топик, который мы не слушаем (мы его не фолловим и чат не
    // открывали) — его сообщения терялись, ответов он не получал («никого не
    // видит»). Фоллов платный — экономический анти-спам сохраняется.
    for f in cached_followers(&me) {
        if !follows.contains(&f) {
            follows.push(f);
        }
    }
    follows.sort_by(|a, b| {
        let ta = last_seen.get(a).copied().unwrap_or(0);
        let tb = last_seen.get(b).copied().unwrap_or(0);
        tb.cmp(&ta)
    });
    for addr in follows.into_iter().take(AUTO_SUB_LIMIT) {
        if let Some(room_id) = room_id_for_addr(&addr) {
            FOLLOW_TOPICS.lock().unwrap().insert(room_id.clone(), addr.clone());
            #[cfg(feature = "p2p")]
            gossip::join_for_peer(room_id, addr);
        }
    }
}


/// A peer's published per-message price (smarthoshi). 0 = free / unset.
fn peer_msg_cost(addr: &str) -> u64 {
    cfg_get(&cost_cache(addr))
        .and_then(|v| String::from_utf8(v).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Publish my per-message price on-chain (vendorField `mcost:<smarthoshi>`).
/// Senders who resolve my address will then be forced to pay this to message me.
#[tauri::command]
pub(crate) async fn publish_msg_cost(fee_sth: Option<String>) -> Result<String, String> {
    let addr = my_address().ok_or("no account")?;
    let cost = my_msg_cost();
    let secret = SESSION_SEED.lock().unwrap().clone().ok_or("locked")?;
    let vendor = format!("mcost:{}", cost);
    let fee_override = fee_sth.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let addr_chk = addr.clone();
    let txid = tauri::async_runtime::spawn_blocking(move || {
        // Идемпотентность: такая же mcost-запись уже на цепи — не дублируем.
        if my_latest_self_record(&addr_chk, "mcost:").as_deref() == Some(vendor.as_str()) {
            return Ok::<String, String>("already-on-chain".into());
        }
        // Size-aware dynamic fee: the vendorField (xkey:/mcost:) enlarges the tx,
        // so the generic /node/fees min undershoots and the node rejects it.
        let fee = key_publish_fee(vendor.len(), fee_override);
        let out = sth_node::send_transfer(&secret, &addr_chk, "0.00000001", &fee, &vendor)?;
        if !out.accepted {
            return Err(if out.reason.is_empty() {
                "Транзакция отклонена узлом".to_string()
            } else {
                format!("Транзакция отклонена: {}", out.reason)
            });
        }
        Ok::<String, String>(out.id)
    })
    .await
    .map_err(|e| e.to_string())??;
    let _ = DB.insert(cost_cache(&addr).as_bytes(), cost.to_string().as_bytes());
    let _ = DB.flush();
    Ok(txid)
}

/// Background indexer: scan the sink for `xkey:`/`mcost:` directives → cache.
pub(crate) fn index_keys_once() {
    let txs = match sth_node::fetch_txs("recipientId", KEY_SINK, 100) {
        Ok(t) => t,
        Err(_) => return,
    };
    // newest-first: keep the first key / cost seen per sender
    let mut seen_key = std::collections::HashSet::new();
    let mut seen_cost = std::collections::HashSet::new();
    for t in txs {
        if let Some(k) = t.vendor.strip_prefix("xkey:") {
            // format: xkey:<b64pub>  OR  xkey:<b64pub>|<endpointid>
            let mut parts = k.trim().splitn(2, '|');
            let key = parts.next().unwrap_or("").trim();
            let node = parts.next().map(|s| s.trim().to_string());
            // ВАЖНО: валидация ключа ДО пометки sender'а как seen — иначе
            // битый свежий tx маскировал бы более старый валидный ключ.
            if !t.sender.is_empty()
                && b64d(key).map(|b| b.len() == 32).unwrap_or(false)
                && seen_key.insert(t.sender.clone())
            {
                let _ = DB.insert(key_cache(&t.sender).as_bytes(), key.as_bytes());
                if let Some(n) = node.filter(|s| !s.is_empty()) {
                    let _ = DB.insert(node_cache(&t.sender).as_bytes(), n.as_bytes());
                }
            }
        } else if let Some(c) = t.vendor.strip_prefix("mcost:") {
            if !t.sender.is_empty() && seen_cost.insert(t.sender.clone()) {
                if let Ok(n) = c.trim().parse::<u64>() {
                    let _ = DB.insert(cost_cache(&t.sender).as_bytes(), n.to_string().as_bytes());
                }
            }
        }
    }
    let _ = DB.flush();
}

pub(crate) fn spawn_key_indexer() {
    let _ = std::thread::Builder::new().name("chat-key-indexer".into()).spawn(|| loop {
        if crate::netwatch::is_online() {
            index_keys_once(); // legacy sink-скан (переходный период)
            refresh_contact_keys(); // Фаза 1: self-tx реестры контактов
        }
        std::thread::sleep(std::time::Duration::from_secs(120));
    });
}

// ---- crypto --------------------------------------------------------------

// ---- rooms & messages ----------------------------------------------------

/// The symmetric key for a room: ECDH-derived for direct chats, or derived from
/// the shared group secret for group chats.
fn room_key(room: &Room) -> Result<[u8; 32], String> {
    if room.kind == "group" {
        // Prefer the current rotated key; fall back to the genesis secret.
        if let Some(gk) = room.group_key.as_deref() {
            if let Some(k) = group_key_from_secret(gk) {
                return Ok(k);
            }
        }
        let s = room.group_secret.as_deref().ok_or("no group secret")?;
        group_key_from_secret(s).ok_or_else(|| "bad group secret".to_string())
    } else {
        shared_key(&room.peer_key)
    }
}

/// Encrypt + broadcast a message envelope onto a room's gossip topic.
#[cfg(feature = "p2p")]
fn broadcast_envelope(room: &Room, msg: &ChatMsg) {
    if let Some(cipher) = envelope_cipher(room, msg) {
        gossip::broadcast(room.id.clone(), cipher);
    }
}

/// Шифротекст конверта (общий для gossip-рассылки и прямой доставки).
#[cfg(feature = "p2p")]
fn envelope_cipher(room: &Room, msg: &ChatMsg) -> Option<String> {
    let k = room_key(room).ok()?;
    encrypt(&k, &serde_json::to_string(msg).ok()?).ok()
}

/// ПРЯМАЯ ДОСТАВКА (ALPN chat-dm) — параллельно с gossip, дедуп по id конверта
/// на приёме. 1-на-1: конверт уходит прямым QUIC-соединением СРАЗУ, затем
/// контрольные дожимы. Группы (v1.135.229): СРАЗУ пушим до 8 случайных известных
/// участников (а не «после 3с без квитанций на первые 5 bootstrap'ов» — квитанции
/// от своего gossip-острова приходили, а другие острова сообщение не видели);
/// получатели ре-бродкастят свежий конверт в СВОЙ остров gossip
/// (`gossip::handle_direct`). Далее дожимы на 10/30с, если квитанций нет.
#[cfg(feature = "p2p")]
pub(crate) fn spawn_direct_fallback(room: Room, msg: ChatMsg) {
    if msg.sys.is_some() || msg.id.is_empty() {
        return;
    }
    let ladder: [u64; 3] = [0, 10, 30];
    tauri::async_runtime::spawn(async move {
        for delay in ladder {
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                if ack_count(&msg.id) > 0 {
                    return; // уже доставлено — дожим не нужен
                }
            }
            let app = match gossip::app_handle() {
                Some(a) => a,
                None => return,
            };
            let targets: Vec<String> = if room.kind == "group" {
                sync::shuffled(sync::known_member_nodes(&room)).into_iter().take(8).collect()
            } else {
                peer_node_id(&room.peer_address).into_iter().collect()
            };
            if targets.is_empty() {
                continue;
            }
            let cipher = match envelope_cipher(&room, &msg) {
                Some(c) => c,
                None => return,
            };
            // параллельно: один офлайн-узел (таймаут 10с) не задерживает остальных
            let handles: Vec<_> = targets
                .iter()
                .map(|node| {
                    let (app, rid, cipher, node) = (app.clone(), room.id.clone(), cipher.clone(), node.clone());
                    tauri::async_runtime::spawn(async move { crate::p2p::chat_direct_send(&app, &node, &rid, &cipher).await })
                })
                .collect();
            let total = handles.len();
            let mut delivered = 0usize;
            for h in handles {
                if matches!(h.await, Ok(true)) {
                    delivered += 1;
                }
            }
            if delivered > 0 {
                if room.kind != "group" {
                    record_ack(&app, &room.id, &msg.id, &room.peer_address);
                }
                crate::syslog::log(
                    "chat",
                    format!("direct: envelope pushed to {}/{} peer(s)", delivered, total),
                );
                if room.kind != "group" || delay > 0 {
                    return;
                }
            }
        }
    });
}

fn room_meta_key(id: &str) -> String {
    format!("chat:room:{}", id)
}
fn msg_prefix(room: &str) -> String {
    format!("chat:msg:{}:", room)
}

fn save_room(r: &Room) {
    if let Ok(j) = serde_json::to_vec(r) {
        let _ = DB.insert(room_meta_key(&r.id).as_bytes(), j);
        let _ = DB.flush();
    }
    refresh_tray_unread();
}

/// Сумма непрочитанных по всем комнатам → бейдж на иконке трея.
pub(crate) fn refresh_tray_unread() {
    let total: u64 = DB
        .scan_prefix(b"chat:room:")
        .filter_map(|kv| kv.ok())
        .filter_map(|(_, v)| serde_json::from_slice::<Room>(&v).ok())
        .map(|r| r.unread)
        .sum();
    crate::tray_set_unread(total);
}

fn load_room(id: &str) -> Option<Room> {
    DB.get(room_meta_key(id).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
}

/// Open (or create) a room with a peer by `u://name` or raw address.
#[tauri::command]
pub(crate) fn open_room(target: String) -> Result<Room, String> {
    let raw = target.trim().trim_start_matches("u://").to_string();
    // resolve username → address (or accept a raw address)
    let address = naming::resolve_username(raw.clone()).unwrap_or(raw.clone());
    let room = ensure_room_for(&address)?;
    #[cfg(feature = "p2p")]
    gossip::join(room.id.clone());
    Ok(room)
}

/// Internal: open/create a room keyed by a raw ledger address.
fn ensure_room_for(address: &str) -> Result<Room, String> {
    let peer_name = naming::username_of_address(address.to_string());
    // Кэш → личный self-tx реестр собеседника (Фаза 1: точечный резолв,
    // мгновенно после публикации — без ожидания 120с sink-индексера).
    let peer_key = resolve_chat_key(address.to_string())
        .or_else(|| refresh_key_from_chain(address))
        .ok_or("У собеседника не опубликован ключ шифрования (X25519).")?;
    // Room ID = SHA-256 of the ECDH shared secret.
    let sk = shared_key(&peer_key)?;
    let id = hex::encode(Sha256::digest(sk));
    let room = load_room(&id).unwrap_or(Room {
        id: id.clone(),
        peer_address: address.to_string(),
        peer_name,
        peer_key,
        last_text: String::new(),
        last_ts: now_ms(),
        kind: "direct".into(),
        group_secret: None,
        unread: 0,
        owner: None,
        policy: None,
        group_key: None,
        key_version: 0,
    });
    save_room(&room);
    Ok(room)
}

// ---- groups -------------------------------------------------------------

fn gboot_key(topic: &str) -> String {
    format!("chat:gboot:{}", topic)
}

/// Known iroh EndpointIds for a group's topic (gossip bootstrap peers). Learned
/// from invite links + `NeighborUp` events so the mesh self-heals across restarts.
pub(crate) fn group_boots(topic: &str) -> Vec<String> {
    cfg_get(&gboot_key(topic))
        .and_then(|v| String::from_utf8(v).ok())
        .map(|s| s.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect())
        .unwrap_or_default()
}

/// Remember a bootstrap EndpointId for a group topic (deduped, capped at 48).
/// v1.135.229: список упорядочен по давности — уже известный узел переносится
/// в КОНЕЦ (самые свежие — последние), лимит 12 → 48: это же реестр участников
/// для прямой доставки/анти-энтропии (chat-sync), а не только bootstrap gossip.
pub(crate) fn add_group_boot(topic: &str, node: &str) {
    let node = node.trim();
    if node.is_empty() {
        return;
    }
    let mut set = group_boots(topic);
    set.retain(|n| n != node);
    set.push(node.to_string());
    if set.len() > 48 {
        let drop = set.len() - 48;
        set.drain(0..drop);
    }
    let _ = DB.insert(gboot_key(topic).as_bytes(), set.join("\n").as_bytes());
    let _ = DB.flush();
}

/// Build a shareable group invite:
/// `sn://group/<secretHex>~<bootNodeId>~<b64name>~<ownerAddr>`.
fn build_invite(secret_hex: &str, node: &str, name: &str, owner: &str) -> String {
    use base64::Engine;
    let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(name.as_bytes());
    format!("sn://group/{}~{}~{}~{}", secret_hex, node, n, owner)
}

/// Create a new E2EE group chat. Random 32-byte secret → gossip topic =
/// SHA-256(secret), group AES key = SHA-256("v1"||secret). The invite link embeds
/// OUR EndpointId so joiners can bootstrap the gossip mesh (groups have no
/// on-chain identity to resolve, unlike 1:1 chats).
#[tauri::command]
pub(crate) async fn create_group(app: tauri::AppHandle, name: String) -> Result<(Room, String), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Введите название группы".into());
    }
    let me = my_address().unwrap_or_default();
    use rand::RngCore;
    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    let secret_hex = hex::encode(secret);
    let topic = hex::encode(Sha256::digest(secret));
    let room = Room {
        id: topic.clone(),
        peer_address: format!("group:{}", &topic[..12]),
        peer_name: Some(name.clone()),
        peer_key: String::new(),
        last_text: String::new(),
        last_ts: now_ms(),
        kind: "group".into(),
        group_secret: Some(secret_hex.clone()),
        unread: 0,
        owner: Some(me.clone()),
        policy: None,
        group_key: None,
        key_version: 0,
    };
    save_room(&room);
    #[cfg(feature = "p2p")]
    gossip::join(room.id.clone());
    let node = my_node_id(&app).await.unwrap_or_default();
    Ok((room, build_invite(&secret_hex, &node, &name, &me)))
}

/// Shareable invite link for an existing group (embeds OUR current EndpointId so
/// the invitee bootstraps directly off us).
#[tauri::command]
pub(crate) async fn group_invite(app: tauri::AppHandle, room_id: String) -> Result<String, String> {
    let room = load_room(&room_id).ok_or("no room")?;
    if room.kind != "group" {
        return Err("not a group".into());
    }
    let secret = room.group_secret.ok_or("no secret")?;
    let name = room.peer_name.unwrap_or_default();
    let owner = room.owner.unwrap_or_default();
    let node = my_node_id(&app).await.unwrap_or_default();
    Ok(build_invite(&secret, &node, &name, &owner))
}

/// Join a group from an invite link `sn://group/<secretHex>~<node>~<b64name>`
/// (legacy `u://group/…` accepted). Idempotent. Records the embedded bootstrap
/// EndpointId so gossip can actually connect the members.
#[tauri::command]
pub(crate) fn join_group(link: String) -> Result<Room, String> {
    let raw = link
        .trim()
        .trim_start_matches("sn://group/")
        .trim_start_matches("u://group/")
        .trim_start_matches("sn://")
        .trim_start_matches("u://")
        .trim();
    let mut it = raw.split('~');
    let secret_hex = it.next().unwrap_or("").trim();
    let boot = it.next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let name_b64 = it.next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let owner = it.next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    let secret = hex::decode(secret_hex).map_err(|_| "Неверная ссылка на группу")?;
    if secret.len() != 32 {
        return Err("Неверная ссылка на группу".into());
    }
    let topic = hex::encode(Sha256::digest(&secret));
    if let Some(b) = &boot {
        add_group_boot(&topic, b);
    }
    if let Some(mut r) = load_room(&topic) {
        // Adopt the owner from the link if we joined before it was known.
        if r.owner.is_none() && owner.is_some() {
            r.owner = owner.clone();
            save_room(&r);
        }
        #[cfg(feature = "p2p")]
        gossip::join(r.id.clone());
        return Ok(r);
    }
    let display_name = name_b64
        .and_then(|b| {
            use base64::Engine;
            base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(b).ok()
        })
        .and_then(|v| String::from_utf8(v).ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("Группа · {}", &topic[..6]));
    let room = Room {
        id: topic.clone(),
        peer_address: format!("group:{}", &topic[..12]),
        peer_name: Some(display_name),
        peer_key: String::new(),
        last_text: String::new(),
        last_ts: now_ms(),
        kind: "group".into(),
        group_secret: Some(hex::encode(&secret)),
        unread: 0,
        owner,
        policy: None,
        group_key: None,
        key_version: 0,
    };
    save_room(&room);
    #[cfg(feature = "p2p")]
    {
        gossip::join(room.id.clone());
        // Announce our arrival to the group (delayed so the mesh has time to form).
        if let Some(me) = my_address() {
            let room2 = room.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(3));
                announce_group_join(&room2, &me);
            });
        }
    }
    Ok(room)
}

/// Broadcast (and locally store) a "user joined" system message on a group topic.
#[cfg(feature = "p2p")]
fn announce_group_join(room: &Room, me: &str) {
    let app = gossip::app_handle();
    if let Ok(m) = store_sys(app.as_ref(), room, me, "join", "", true) {
        if let Ok(k) = room_key(room) {
            if let Ok(cipher) = encrypt(&k, &serde_json::to_string(&m).unwrap_or_default()) {
                gossip::broadcast(room.id.clone(), cipher);
            }
        }
    }
}

/// Leave / delete a room: remove its messages, metadata, group bootstraps, and
/// drop the live gossip subscription.
#[tauri::command]
pub(crate) fn delete_room(room_id: String) -> Result<(), String> {
    let keys: Vec<sled::IVec> = DB
        .scan_prefix(msg_prefix(&room_id).as_bytes())
        .filter_map(|kv| kv.ok().map(|(k, _)| k))
        .collect();
    for k in keys {
        let _ = DB.remove(k);
    }
    let _ = DB.remove(room_meta_key(&room_id).as_bytes());
    let _ = DB.remove(gboot_key(&room_id).as_bytes());
    #[cfg(feature = "p2p")]
    gossip::leave(room_id);
    let _ = DB.flush();
    refresh_tray_unread();
    Ok(())
}

// ---- group admin policy (owner-signed, gossip-synced) --------------------

/// Canonical byte string a policy is signed over (stable field order).
fn policy_canonical(topic: &str, p: &GroupPolicy) -> String {
    let mut banned = p.banned.clone();
    banned.sort();
    let mut s = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        topic, p.entry_fee, p.paid_entry as u8, p.invite_only as u8, p.channel_mode as u8,
        p.pinned.as_deref().unwrap_or(""), banned.join(","), p.ts
    );
    // Логотип добавляется в канонику ТОЛЬКО когда задан — политики без него
    // остаются подписе-совместимыми со старыми клиентами.
    if !p.logo_hash.is_empty() {
        s.push_str(&format!("|{}|{}", p.logo_hash, p.logo_node));
    }
    s
}

/// Sign a blob with our wallet key (secp256k1 ECDSA). Returns (pubkeyHex, sigHex).
fn sign_blob(canonical: &str) -> Result<(String, String), String> {
    let seed = SESSION_SEED.lock().unwrap().clone().ok_or("locked")?;
    let (sk, pk_hex, _addr) = crate::crypto::keys_from_material(seed.trim())?;
    let secp = secp256k1::Secp256k1::signing_only();
    let msg = secp256k1::Message::from_digest(crate::crypto::sha256(canonical.as_bytes()));
    let sig = secp.sign_ecdsa(&msg, &sk);
    Ok((pk_hex, hex::encode(sig.serialize_compact())))
}

/// Verify a blob signature AND that the signer's pubkey maps to `owner`.
fn verify_blob(canonical: &str, pubkey_hex: &str, sig_hex: &str, owner: &str) -> bool {
    let pk_bytes = match hex::decode(pubkey_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };
    if crate::crypto::address_from_pubkey(&pk_bytes) != owner {
        return false;
    }
    let pk = match secp256k1::PublicKey::from_slice(&pk_bytes) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let sb = match hex::decode(sig_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let sig = match secp256k1::ecdsa::Signature::from_compact(&sb) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let secp = secp256k1::Secp256k1::verification_only();
    let msg = secp256k1::Message::from_digest(crate::crypto::sha256(canonical.as_bytes()));
    secp.verify_ecdsa(&msg, &sig, &pk).is_ok()
}

fn policy_summary(p: &GroupPolicy) -> String {
    let mut parts = Vec::new();
    if p.paid_entry {
        parts.push(format!("платный вход {} STH", p.entry_fee as f64 / STH as f64));
    } else {
        parts.push("вход свободный".to_string());
    }
    if p.invite_only {
        parts.push("только по приглашению".to_string());
    }
    if p.channel_mode {
        parts.push("режим канала".to_string());
    }
    if !p.banned.is_empty() {
        parts.push(format!("исключено: {}", p.banned.len()));
    }
    format!("изменил настройки группы ({})", parts.join(", "))
}

/// Current group policy (if any).
#[tauri::command]
pub(crate) fn get_group_policy(room_id: String) -> Option<GroupPolicy> {
    load_room(&room_id).and_then(|r| r.policy)
}

/// Owner-only: update + sign + broadcast the group policy. Non-owners are
/// rejected ("Ошибка прав доступа").
#[tauri::command]
pub(crate) fn set_group_policy(
    room_id: String,
    entry_fee_sth: f64,
    paid_entry: bool,
    invite_only: bool,
    channel_mode: bool,
    pinned: Option<String>,
    banned: Vec<String>,
    logo_hash: Option<String>,
    logo_node: Option<String>,
) -> Result<GroupPolicy, String> {
    let mut room = load_room(&room_id).ok_or("no room")?;
    if room.kind != "group" {
        return Err("not a group".into());
    }
    let me = my_address().ok_or("locked")?;
    if room.owner.as_deref() != Some(me.as_str()) {
        return Err("Ошибка прав доступа: только владелец может менять настройки".into());
    }
    let policy = GroupPolicy {
        entry_fee: (entry_fee_sth.max(0.0) * STH as f64).round() as u64,
        paid_entry,
        invite_only,
        channel_mode,
        pinned: pinned.filter(|p| !p.trim().is_empty()),
        banned: banned.into_iter().filter(|b| !b.trim().is_empty()).collect(),
        logo_hash: logo_hash.unwrap_or_default().trim().to_string(),
        logo_node: logo_node.unwrap_or_default().trim().to_string(),
        ts: now_ms(),
    };
    let canonical = policy_canonical(&room.id, &policy);
    let (pubkey, sig) = sign_blob(&canonical)?;
    room.policy = Some(policy.clone());
    save_room(&room);

    let human = policy_summary(&policy);
    #[cfg(feature = "p2p")]
    {
        let app = gossip::app_handle();
        let _ = store_sys(app.as_ref(), &room, &me, "policy", &human, true);
        let upd = GroupPolicyUpdate {
            policy: policy.clone(),
            topic: room.id.clone(),
            owner_pubkey: pubkey,
            sig,
        };
        if let Ok(json) = serde_json::to_string(&upd) {
            let env = ChatMsg {
                id: format!("{:x}", Sha256::digest(format!("{}policy{}", policy.ts, me).as_bytes())),
                room: room.id.clone(),
                sender: me.clone(),
                text: json,
                ts: policy.ts,
                mine: false,
                sys: Some("policy".into()),
                edited: None,
            };
            if let Ok(k) = room_key(&room) {
                if let Ok(cipher) = encrypt(&k, &serde_json::to_string(&env).unwrap_or_default()) {
                    gossip::broadcast(room.id.clone(), cipher);
                }
            }
        }
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = store_sys(None, &room, &me, "policy", &human, true);
    }
    Ok(policy)
}

/// Apply an inbound owner-signed policy update (signature + ownership + freshness
/// checked). Invalid/forged updates are silently ignored.
pub(crate) fn apply_policy_update(app: &tauri::AppHandle, room: &Room, upd: &GroupPolicyUpdate) {
    let owner = match &room.owner {
        Some(o) => o.clone(),
        None => return,
    };
    if upd.topic != room.id {
        return;
    }
    if let Some(cur) = &room.policy {
        if upd.policy.ts <= cur.ts {
            return; // stale / replayed
        }
    }
    let canonical = policy_canonical(&room.id, &upd.policy);
    if !verify_blob(&canonical, &upd.owner_pubkey, &upd.sig, &owner) {
        return; // bad signature or not the owner
    }
    let mut r = room.clone();
    r.policy = Some(upd.policy.clone());
    save_room(&r);
    let human = policy_summary(&upd.policy);
    let _ = store_sys(Some(app), &r, &owner, "policy", &human, false);
}

// ============================================================
//  Auto-Ban + Group Key Rotation (Phases B–D)
//  Author: TechnoL0g / 2026-06-22
// ============================================================

fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---- member roster (owner-side whitelist source) -------------------------
fn members_key(room: &str) -> String {
    format!("chat:members:{}", room)
}
fn get_members(room: &str) -> Vec<String> {
    DB.get(members_key(room).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Vec<String>>(&v).ok())
        .unwrap_or_default()
}
/// Record a seen group participant (used as the key-rotation whitelist).
fn add_member(room: &str, addr: &str) {
    if addr.is_empty() {
        return;
    }
    let mut m = get_members(room);
    if m.iter().any(|x| x == addr) {
        return;
    }
    m.push(addr.to_string());
    if m.len() > 5000 {
        let drop = m.len() - 5000;
        m.drain(0..drop);
    }
    if let Ok(j) = serde_json::to_vec(&m) {
        let _ = DB.insert(members_key(room).as_bytes(), j);
    }
}

// ---- ban table (global, sliding-window cap) ------------------------------
// Storage helpers + BanEntry live in `chat::bans` (imported above); the command
// surface and ban-detection logic remain here.

/// Periodic janitor: drop expired bans + enforce the 20k sliding window.
/// Returns how many rows were removed.
#[tauri::command]
pub(crate) fn prune_ban_table() -> usize {
    let mut v = load_bans();
    let before = v.len();
    prune_bans(&mut v);
    save_bans(&v);
    before.saturating_sub(v.len())
}

/// All currently-active bans (owner moderation view).
#[tauri::command]
pub(crate) fn list_bans() -> Vec<BanEntry> {
    let now = now_secs();
    load_bans().into_iter().filter(|b| b.until > now).collect()
}

/// Groups where *I* am currently banned (banned user's UI: grey / hide).
#[tauri::command]
pub(crate) fn my_group_bans() -> Vec<BanEntry> {
    let me = match my_address() {
        Some(a) => a,
        None => return Vec::new(),
    };
    let now = now_secs();
    load_bans()
        .into_iter()
        .filter(|b| b.addr == me && b.until > now)
        .collect()
}

// ---- group key rotation --------------------------------------------------

/// Owner-only: roll a fresh AES key, seal it (ECDH) to every whitelisted member
/// and broadcast `sys:"key_rotation"` (encrypted with the CURRENT key so members
/// can read the envelope). The kicked member, absent from the recipients, can no
/// longer derive the key. The gossip topic is unchanged (== SHA256(group_secret)).
#[cfg(feature = "p2p")]
fn rotate_group_key(room: &mut Room) {
    use rand::RngCore;
    let me = match my_address() {
        Some(a) => a,
        None => return,
    };
    let mut nk = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nk);
    let new_hex = hex::encode(nk);
    let new_ver = room.key_version + 1;

    let banned: std::collections::HashSet<String> = room
        .policy
        .as_ref()
        .map(|p| p.banned.iter().cloned().collect())
        .unwrap_or_default();
    let mut sealed = serde_json::Map::new();
    for addr in get_members(&room.id) {
        if addr == me || banned.contains(&addr) {
            continue;
        }
        if let Some(pk) = resolve_chat_key(addr.clone()) {
            if let Ok(sk) = shared_key(&pk) {
                if let Ok(ct) = encrypt(&sk, &new_hex) {
                    sealed.insert(addr, serde_json::Value::String(ct));
                }
            }
        }
    }
    let owner_pub = get_my_chat_pubkey().unwrap_or_default();
    let payload = serde_json::json!({
        "version": new_ver,
        "groupId": room.id,
        "ownerPubkey": owner_pub,
        "keys": sealed,
    });
    let msg = ChatMsg {
        id: format!("{:x}", Sha256::digest(format!("rot{}{}", new_ver, me).as_bytes())),
        room: room.id.clone(),
        sender: me.clone(),
        text: payload.to_string(),
        ts: now_ms(),
        mine: false,
        sys: Some("key_rotation".into()),
        edited: None,
    };
    // broadcast with the OLD key (room not yet switched), then switch ourselves.
    broadcast_envelope(room, &msg);
    room.group_key = Some(new_hex);
    room.key_version = new_ver;
    save_room(room);
}

#[cfg(not(feature = "p2p"))]
fn rotate_group_key(room: &mut Room) {
    use rand::RngCore;
    let mut nk = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nk);
    room.group_key = Some(hex::encode(nk));
    room.key_version += 1;
    save_room(room);
}

/// Member side: apply an inbound `key_rotation` (strictly newer version only —
/// a version ≤ ours is a stale/replayed/missed-older packet and is ignored, which
/// also signals to a returning offline node that it skipped an interval).
fn apply_key_rotation(room: &Room, text: &str) {
    let v: serde_json::Value = match serde_json::from_str(text) {
        Ok(x) => x,
        Err(_) => return,
    };
    let version = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0);
    if version <= room.key_version {
        return;
    }
    let me = match my_address() {
        Some(a) => a,
        None => return,
    };
    let owner_pub = match v.get("ownerPubkey").and_then(|x| x.as_str()) {
        Some(s) => s,
        None => return,
    };
    let my_ct = match v
        .get("keys")
        .and_then(|k| k.get(&me))
        .and_then(|x| x.as_str())
    {
        Some(s) => s,
        None => return, // no slice for me → stay on old key (detectable version gap)
    };
    if let Ok(sk) = shared_key(owner_pub) {
        if let Ok(new_hex) = decrypt(&sk, my_ct) {
            if group_key_from_secret(&new_hex).is_some() {
                if let Some(mut r) = load_room(&room.id) {
                    r.group_key = Some(new_hex);
                    r.key_version = version;
                    save_room(&r);
                }
            }
        }
    }
}

// ---- ban / unban commands ------------------------------------------------

/// Owner-only: ban a member — write an on-chain BAN-tx, record it locally, add
/// to the moderation list, then rotate the group key so the kicked member loses
/// the ability to decrypt the stream.
#[tauri::command]
pub(crate) async fn ban_member(
    room_id: String,
    address: String,
    duration_secs: u64,
) -> Result<BanEntry, String> {
    let room = load_room(&room_id).ok_or("no room")?;
    if room.kind != "group" {
        return Err("not a group".into());
    }
    let me = my_address().ok_or("locked")?;
    if room.owner.as_deref() != Some(me.as_str()) {
        return Err("Ошибка прав доступа: только владелец может банить".into());
    }
    if address.trim().is_empty() || address == me {
        return Err("Неверный адрес".into());
    }
    let until = now_secs() + duration_secs.max(60);

    // on-chain BAN-tx: 0.00000001 STH, fee 0.01 STH, vendor ban:<until>:<groupId>
    let secret = SESSION_SEED.lock().unwrap().clone().ok_or("locked")?;
    let vendor = format!("ban:{}:{}", until, room_id);
    let addr2 = address.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        sth_node::send_transfer(&secret, &addr2, "0.00000001", "0.01", &vendor)
    })
    .await
    .map_err(|e| e.to_string())??;
    if !outcome.accepted {
        return Err("BAN-транзакция отклонена нодой".into());
    }

    // local ban table
    add_ban(&room_id, &address, until);

    // moderation list (policy.banned) — re-sign + re-broadcast via set_group_policy
    let mut banned = room
        .policy
        .as_ref()
        .map(|p| p.banned.clone())
        .unwrap_or_default();
    if !banned.iter().any(|b| b == &address) {
        banned.push(address.clone());
    }
    if let Some(p) = room.policy.clone() {
        let _ = set_group_policy(
            room_id.clone(),
            p.entry_fee as f64 / STH as f64,
            p.paid_entry,
            p.invite_only,
            p.channel_mode,
            p.pinned.clone(),
            banned,
            Some(p.logo_hash.clone()),
            Some(p.logo_node.clone()),
        );
    } else {
        let _ = set_group_policy(room_id.clone(), 0.0, false, false, false, None, banned, None, None);
    }

    // rotate the key (reload to pick up the freshly-saved policy/banned set)
    if let Some(mut r) = load_room(&room_id) {
        rotate_group_key(&mut r);
    }
    Ok(BanEntry {
        group: room_id,
        addr: address,
        until,
        at: now_secs(),
    })
}

/// Owner-only: lift a ban (re-admittance requires re-sharing the current key,
/// e.g. a fresh invite — we do not rotate on unban).
#[tauri::command]
pub(crate) fn unban_member(room_id: String, address: String) -> Result<(), String> {
    let room = load_room(&room_id).ok_or("no room")?;
    let me = my_address().ok_or("locked")?;
    if room.owner.as_deref() != Some(me.as_str()) {
        return Err("Ошибка прав доступа".into());
    }
    remove_ban(&room_id, &address);
    let banned: Vec<String> = room
        .policy
        .as_ref()
        .map(|p| p.banned.iter().filter(|b| *b != &address).cloned().collect())
        .unwrap_or_default();
    if let Some(p) = room.policy.clone() {
        let _ = set_group_policy(
            room_id,
            p.entry_fee as f64 / STH as f64,
            p.paid_entry,
            p.invite_only,
            p.channel_mode,
            p.pinned,
            banned,
            Some(p.logo_hash),
            Some(p.logo_node),
        );
    }
    Ok(())
}

/// Banned-user self-detection: scan my incoming txs for `ban:<until>:<group>`
/// and record them locally so my UI greys/hides those groups even if the owner's
/// policy broadcast never reached me. Returns my active bans.
#[tauri::command]
pub(crate) async fn sync_incoming_bans() -> Vec<BanEntry> {
    let me = match my_address() {
        Some(a) => a,
        None => return Vec::new(),
    };
    let me2 = me.clone();
    let hist = tauri::async_runtime::spawn_blocking(move || {
        sth_node::fetch_history(&me2, 100).unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    for h in hist {
        if h.kind != "in" {
            continue;
        }
        let parts: Vec<&str> = h.memo.splitn(3, ':').collect();
        if parts.len() == 3 && parts[0] == "ban" {
            if let Ok(until) = parts[1].parse::<u64>() {
                let group = parts[2];
                // SECURITY: only honor a BAN-tx sent BY the group's owner. A ban
                // tx from any non-owner address is invalid and ignored. We must
                // have the room locally to know the owner.
                let owner_ok = load_room(group)
                    .and_then(|r| r.owner)
                    .map(|o| o == h.counterparty)
                    .unwrap_or(false);
                if owner_ok && until > now_secs() {
                    add_ban(group, &me, until);
                }
            }
        }
    }
    my_group_bans()
}


/// All rooms, newest-activity first.
#[tauri::command]
pub(crate) fn get_rooms() -> Vec<Room> {    let mut out: Vec<Room> = Vec::new();
    for kv in DB.scan_prefix(b"chat:room:") {
        if let Ok((_, v)) = kv {
            if let Ok(r) = serde_json::from_slice::<Room>(&v) {
                out.push(r);
            }
        }
    }
    out.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    out
}

fn enforce_rolling(room: &str) {
    let prefix = msg_prefix(room);
    let keys: Vec<sled::IVec> = DB.scan_prefix(prefix.as_bytes()).filter_map(|kv| kv.ok().map(|(k, _)| k)).collect();
    if keys.len() > MAX_MSGS {
        // keys are timestamp-ordered lexicographically; drop the oldest.
        for k in keys.iter().take(keys.len() - MAX_MSGS) {
            let _ = DB.remove(k);
        }
    }
}

fn store_msg(room: &Room, sender: &str, text: &str, mine: bool) -> Result<ChatMsg, String> {
    store_msg_with_id(room, sender, text, mine, None, None)
}

/// Допустимый разбег часов отправителя: иначе берём локальное время.
const SENDER_TS_SKEW_MS: u64 = 24 * 3600 * 1000;

/// preset = id из конверта отправителя: ЕДИНЫЙ id сообщения на всех клиентах.
/// Раньше приёмник генерировал id заново из локального времени — id одного и
/// того же сообщения расходились между пирами, и реакции/пины по id не
/// находили сообщение у других участников.
/// preset_ts = время ОТПРАВКИ из конверта: сообщение ложится в историю по времени
/// автора, а не по моменту получения (раньше догнавшие через ресинк/после обрыва
/// сети сообщения вставали в конец ленты с чужим временем).
fn store_msg_with_id(
    room: &Room,
    sender: &str,
    text: &str,
    mine: bool,
    preset: Option<&str>,
    preset_ts: Option<u64>,
) -> Result<ChatMsg, String> {
    let key = room_key(room)?;
    let now = now_ms();
    let ts = match preset_ts {
        Some(t) if t > 0 && t.abs_diff(now) <= SENDER_TS_SKEW_MS => t,
        _ => now,
    };
    let id = match preset {
        Some(p) if !p.is_empty() && p.len() <= 64 => p.to_string(),
        _ => format!("{:x}", Sha256::digest(format!("{}{}{}", ts, sender, text).as_bytes())),
    };
    let msg = ChatMsg { id: id.clone(), room: room.id.clone(), sender: sender.into(), text: text.into(), ts, mine, sys: None, edited: None };
    let blob = encrypt(&key, &serde_json::to_string(&msg).map_err(|e| e.to_string())?)?;
    let mkey = format!("{}{:020}:{}", msg_prefix(&room.id), ts, id);
    let _ = DB.insert(mkey.as_bytes(), blob.as_bytes());
    // bump room preview
    let mut r = room.clone();
    // превью медиа/реплаев в списке комнат — метка/чистый текст вместо JSON-конверта
    r.last_text = preview_of(text);
    r.last_ts = ts.max(r.last_ts);
    if !mine {
        r.unread = r.unread.saturating_add(1);
    }
    save_room(&r);
    enforce_rolling(&room.id);
    let _ = DB.flush();
    Ok(msg)
}

/// Encrypt + persist an outbound message. If the peer has published a per-message
/// price (`msg_cost_sth > 0`), the standard P2P route is LOCKED and the ciphertext
/// is instead packed into a blockchain transfer of the required amount straight to
/// the peer's ledger address (vendorField `msg:<ciphertext>`). Free peers store
/// locally now (Phase 2 Gossipsub broadcasts the ciphertext in transit).
#[tauri::command]
pub(crate) async fn send_message(room_id: String, text: String) -> Result<ChatMsg, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("empty".into());
    }
    // Бюджет 4КБ-кадра gossip: конверт с текстом >2000 символов (или >6000
    // байт — многобайтные emoji) не влезает.
    if text.chars().count() > 2000 || text.len() > 6000 {
        return Err("msg-too-long".into());
    }
    let room = load_room(&room_id).ok_or("no room")?;
    let me = my_address().unwrap_or_default();

    // Group chat: broadcast the E2EE envelope on the shared topic (no cost path).
    if room.kind == "group" {
        let msg = store_msg(&room, &me, &text, true)?;
        #[cfg(feature = "p2p")]
        {
            broadcast_envelope(&room, &msg);
            spawn_direct_fallback(room.clone(), msg.clone());
        }
        return Ok(msg);
    }

    let cost = peer_msg_cost(&room.peer_address);

    if cost > 0 {
        // Economic gateway: pack the E2EE payload into a paid blockchain tx.
        let key = shared_key(&room.peer_key)?;
        let cipher = encrypt(&key, &text)?;
        let vendor = format!("msg:{}", cipher);
        if vendor.len() > 255 {
            return Err("Платное сообщение слишком длинное (лимит ~160 символов).".into());
        }
        let secret = SESSION_SEED.lock().unwrap().clone().ok_or("locked")?;
        let recipient = room.peer_address.clone();
        let amount = format!("{}", cost as f64 / STH as f64);
        let out = tauri::async_runtime::spawn_blocking(move || {
            sth_node::send_transfer(&secret, &recipient, &amount, "", &vendor)
        })
        .await
        .map_err(|e| e.to_string())??;
        if !out.accepted {
            return Err("Платёж за сообщение отклонён узлом.".into());
        }
        return store_msg(&room, &me, &text, true);
    }

    // Free peer: store locally; Phase 2 Gossipsub pushes the ciphertext in transit.
    let msg = store_msg(&room, &me, &text, true)?;
    #[cfg(feature = "p2p")]
    {
        broadcast_envelope(&room, &msg);
        spawn_direct_fallback(room.clone(), msg.clone());
    }
    Ok(msg)
}

// ---- delivery receipts (квитанции доставки) --------------------------------

const ACK_PREFIX: &str = "chat:ack:";

/// Квитанция доставки: получатель шлёт sys="ack" с id принятого сообщения
/// (E2EE-конверт, эфемерный — в историю не пишется).
#[allow(unused_variables)]
pub(crate) fn send_ack(room: &Room, msg_id: &str) {
    #[cfg(feature = "p2p")]
    {
        if msg_id.is_empty() {
            return;
        }
        let me = my_address().unwrap_or_default();
        let msg = ChatMsg {
            id: String::new(),
            room: room.id.clone(),
            sender: me,
            text: msg_id.into(),
            ts: now_ms(),
            mine: false,
            sys: Some("ack".into()),
            edited: None,
        };
        broadcast_envelope(room, &msg);
        // 1-на-1: дублируем квитанцию прямым каналом — индикатор доставки ✓✓
        // у отправителя работает даже при мёртвом gossip-меше.
        if room.kind != "group" {
            if let (Some(cipher), Some(node)) =
                (envelope_cipher(room, &msg), peer_node_id(&room.peer_address))
            {
                let room_id = room.id.clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(app) = gossip::app_handle() {
                        let _ = crate::p2p::chat_direct_send(&app, &node, &room_id, &cipher).await;
                    }
                });
            }
        }
    }
}

/// Учёт квитанции: множество адресов-получателей на id сообщения (cap 500),
/// плюс live-событие `chat-ack` в UI со свежим счётчиком.
pub(crate) fn record_ack(app: &tauri::AppHandle, room_id: &str, msg_id: &str, from: &str) {
    if msg_id.is_empty()
        || from.is_empty()
        || msg_id.len() > 64
        || !msg_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return;
    }
    let key = format!("{}{}", ACK_PREFIX, msg_id);
    let mut set: Vec<String> = DB
        .get(key.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
        .map(|s| s.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default();
    if set.len() >= 500 || set.iter().any(|a| a == from) {
        return;
    }
    set.push(from.to_string());
    let count = set.len() as u64;
    let _ = DB.insert(key.as_bytes(), set.join("\n").as_bytes());
    use tauri::Emitter;
    let _ = app.emit(
        "chat-ack",
        serde_json::json!({ "room": room_id, "id": msg_id, "count": count }),
    );
}

fn ack_count(msg_id: &str) -> u64 {
    DB.get(format!("{}{}", ACK_PREFIX, msg_id).as_bytes())
        .ok()
        .flatten()
        .map(|v| v.iter().filter(|b| **b == b'\n').count() as u64 + 1)
        .unwrap_or(0)
}

/// Счётчики доставки для всех моих сообщений комнаты: id -> кол-во получателей.
#[tauri::command]
pub(crate) fn chat_acks(room_id: String) -> std::collections::HashMap<String, u64> {
    let mut out = std::collections::HashMap::new();
    for m in get_messages(room_id) {
        if m.mine && !m.id.is_empty() {
            let n = ack_count(&m.id);
            if n > 0 {
                out.insert(m.id, n);
            }
        }
    }
    out
}

// ---- реакции на сообщения (Telegram-style, эфемерный sys="react") ----------

const REACT_PREFIX: &str = "chat:react:";

#[derive(Serialize, Clone)]
pub(crate) struct MsgReaction {
    e: String,
    n: u64,
    mine: bool,
}

fn load_react_map(msg_id: &str) -> std::collections::HashMap<String, Vec<String>> {
    DB.get(format!("{}{}", REACT_PREFIX, msg_id).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}

fn react_summary(msg_id: &str, me: &str) -> Vec<MsgReaction> {
    let mut out: Vec<MsgReaction> = load_react_map(msg_id)
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(e, v)| MsgReaction { n: v.len() as u64, mine: v.iter().any(|a| a == me), e })
        .collect();
    out.sort_by(|a, b| b.n.cmp(&a.n).then(a.e.cmp(&b.e)));
    out
}

/// Применить реакцию: одна реакция на пользователя — новая заменяет прежнюю.
fn apply_reaction(msg_id: &str, emoji: &str, from: &str, add: bool) {
    let mut map = load_react_map(msg_id);
    for v in map.values_mut() {
        v.retain(|a| a != from);
    }
    if add {
        let entry = map.entry(emoji.to_string()).or_default();
        if entry.len() < 500 {
            entry.push(from.to_string());
        }
    }
    map.retain(|_, v| !v.is_empty());
    let key = format!("{}{}", REACT_PREFIX, msg_id);
    if map.is_empty() {
        let _ = DB.remove(key.as_bytes());
    } else if let Ok(blob) = serde_json::to_vec(&map) {
        let _ = DB.insert(key.as_bytes(), blob);
    }
}

fn react_valid(msg_id: &str, emoji: &str) -> bool {
    !msg_id.is_empty()
        && msg_id.len() <= 64
        && msg_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !emoji.is_empty()
        && emoji.len() <= 32
        && emoji.chars().count() <= 8
}

/// Входящая реакция из gossip (payload = {"m":id,"e":emoji,"a":0|1}).
pub(crate) fn record_reaction(app: &tauri::AppHandle, room_id: &str, payload: &str, from: &str) {
    let v: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return,
    };
    let msg_id = v.get("m").and_then(|x| x.as_str()).unwrap_or("");
    let emoji = v.get("e").and_then(|x| x.as_str()).unwrap_or("");
    let add = v.get("a").and_then(|x| x.as_u64()).unwrap_or(1) == 1;
    if !react_valid(msg_id, emoji) || from.is_empty() {
        return;
    }
    apply_reaction(msg_id, emoji, from, add);
    let me = my_address().unwrap_or_default();
    use tauri::Emitter;
    let _ = app.emit(
        "chat-react",
        serde_json::json!({ "room": room_id, "id": msg_id, "reactions": react_summary(msg_id, &me) }),
    );
}

/// Поставить/снять свою реакцию: повторный клик той же эмодзи — снимает.
#[tauri::command]
pub(crate) fn chat_react(room_id: String, msg_id: String, emoji: String) -> Result<Vec<MsgReaction>, String> {
    if !react_valid(&msg_id, &emoji) {
        return Err("bad reaction".into());
    }
    let room = load_room(&room_id).ok_or("no room")?;
    let me = my_address().unwrap_or_default();
    let had_same = load_react_map(&msg_id)
        .get(&emoji)
        .map(|v| v.iter().any(|a| a == &me))
        .unwrap_or(false);
    let add = !had_same;
    apply_reaction(&msg_id, &emoji, &me, add);
    let _ = DB.flush();
    #[cfg(feature = "p2p")]
    {
        let act: u64 = if add { 1 } else { 0 };
        let payload = serde_json::json!({ "m": msg_id, "e": emoji, "a": act }).to_string();
        let msg = ChatMsg {
            id: String::new(),
            room: room.id.clone(),
            sender: me.clone(),
            text: payload,
            ts: now_ms(),
            mine: false,
            sys: Some("react".into()),
            edited: None,
        };
        broadcast_envelope(&room, &msg);
        // запись для досылки через анти-энтропию (последнее состояние на сообщение)
        let rec = serde_json::json!({ "e": emoji, "a": act, "ts": now_ms() }).to_string();
        let _ = DB.insert(
            format!("chat:myreact:{}:{}", room.id, msg_id).as_bytes(),
            rec.as_bytes(),
        );
        // 1-на-1: дублируем прямым каналом (как ack) — реакция доедет и при мёртвом gossip
        if room.kind != "group" {
            if let (Some(cipher), Some(node)) =
                (envelope_cipher(&room, &msg), peer_node_id(&room.peer_address))
            {
                let rid = room.id.clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(app) = gossip::app_handle() {
                        let _ = crate::p2p::chat_direct_send(&app, &node, &rid, &cipher).await;
                    }
                });
            }
        }
    }
    #[cfg(not(feature = "p2p"))]
    let _ = &room;
    Ok(react_summary(&msg_id, &me))
}

// ---- редактирование сообщений (sys="edit", payload {"m":id,"t":text}) --------
//
// Единый id сообщения на всех клиентах (store_msg_with_id) позволяет адресно
// заменить текст у собеседников. Право редактировать — только автор: на приёме
// требуем sender сохранённого сообщения == отправитель конверта (та же модель
// доверия, что у реакций). Доставка best-effort (онлайн-пиры через gossip,
// 1-на-1 дублируем прямым каналом); офлайн-пир увидит старый текст.

fn edit_valid(msg_id: &str, text: &str) -> bool {
    !msg_id.is_empty()
        && msg_id.len() <= 64
        && msg_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !text.trim().is_empty()
        && text.chars().count() <= 2000
        && text.len() <= 6000
}

/// Найти сохранённое сообщение комнаты по id: (ключ sled, расшифрованное).
fn find_stored_msg(room: &Room, msg_id: &str) -> Option<(sled::IVec, ChatMsg)> {
    let key = room_key(room).ok()?;
    let suffix = format!(":{}", msg_id);
    for kv in DB.scan_prefix(msg_prefix(&room.id).as_bytes()) {
        let (k, v) = kv.ok()?;
        if !std::str::from_utf8(&k).map(|s| s.ends_with(&suffix)).unwrap_or(false) {
            continue;
        }
        let plain = decrypt(&key, std::str::from_utf8(&v).ok()?).ok()?;
        let m: ChatMsg = serde_json::from_str(&plain).ok()?;
        return Some((k, m));
    }
    None
}

/// Перезаписать текст сообщения на месте (тот же ключ → тот же порядок/ts).
fn rewrite_stored_msg(room: &Room, key: sled::IVec, mut m: ChatMsg, text: &str, edited: u64) -> Result<ChatMsg, String> {
    let rk = room_key(room)?;
    // реплай-конверт: меняем только текст ответа, цитата сохраняется
    if let Some(j) = m.text.strip_prefix("sn-rep:") {
        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(j) {
            if v.get("t").is_some() {
                v["t"] = serde_json::Value::String(text.to_string());
                m.text = format!("sn-rep:{}", v);
            }
        }
    } else {
        m.text = text.to_string();
    }
    m.edited = Some(edited);
    let blob = encrypt(&rk, &serde_json::to_string(&m).map_err(|e| e.to_string())?)?;
    let _ = DB.insert(key, blob.as_bytes());
    // если это было последнее сообщение — обновляем превью комнаты
    if let Some(mut r) = load_room(&room.id) {
        if r.last_ts == m.ts {
            r.last_text = preview_of(&m.text);
            save_room(&r);
        }
    }
    let _ = DB.flush();
    Ok(m)
}

/// Редактировать СВОЁ сообщение: локально + рассылка sys="edit" собеседникам.
#[tauri::command]
pub(crate) fn chat_edit_message(room_id: String, msg_id: String, text: String) -> Result<ChatMsg, String> {
    let text = text.trim().to_string();
    if !edit_valid(&msg_id, &text) {
        return Err(if text.chars().count() > 2000 || text.len() > 6000 { "msg-too-long" } else { "bad edit" }.into());
    }
    let room = load_room(&room_id).ok_or("no room")?;
    let (key, stored) = find_stored_msg(&room, &msg_id).ok_or("message not found")?;
    if !stored.mine || stored.sys.is_some() {
        return Err("only own text messages can be edited".into());
    }
    if stored.text.starts_with("sn-img:") || stored.text.starts_with("sn-aud:")
        || stored.text.starts_with("sn-vid:") || stored.text.starts_with("sn-file:")
    {
        return Err("media messages cannot be edited".into());
    }
    let me = my_address().unwrap_or_default();
    let edited = now_ms();
    let updated = rewrite_stored_msg(&room, key, stored, &text, edited)?;
    #[cfg(feature = "p2p")]
    {
        let payload = serde_json::json!({ "m": msg_id, "t": text, "e": edited }).to_string();
        let msg = ChatMsg {
            id: String::new(),
            room: room.id.clone(),
            sender: me.clone(),
            text: payload,
            ts: edited,
            mine: false,
            sys: Some("edit".into()),
            edited: None,
        };
        broadcast_envelope(&room, &msg);
        if room.kind != "group" {
            if let (Some(cipher), Some(node)) =
                (envelope_cipher(&room, &msg), peer_node_id(&room.peer_address))
            {
                let rid = room.id.clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(app) = gossip::app_handle() {
                        let _ = crate::p2p::chat_direct_send(&app, &node, &rid, &cipher).await;
                    }
                });
            }
        }
    }
    #[cfg(not(feature = "p2p"))]
    let _ = &me;
    Ok(updated)
}

/// Входящее редактирование (payload = {"m":id,"t":text,"e":ts}) от `from`.
pub(crate) fn record_edit(app: &tauri::AppHandle, room_id: &str, payload: &str, from: &str) {
    let v: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return,
    };
    let msg_id = v.get("m").and_then(|x| x.as_str()).unwrap_or("");
    let text = v.get("t").and_then(|x| x.as_str()).unwrap_or("");
    let edited = v.get("e").and_then(|x| x.as_u64()).unwrap_or_else(now_ms);
    if !edit_valid(msg_id, text) || from.is_empty() {
        return;
    }
    let room = match load_room(room_id) {
        Some(r) => r,
        None => return,
    };
    let (key, stored) = match find_stored_msg(&room, msg_id) {
        Some(x) => x,
        None => return,
    };
    // только автор оригинала; своё не перезаписываем чужим конвертом
    if stored.mine || stored.sender != from || stored.sys.is_some() {
        return;
    }
    if let Ok(m) = rewrite_stored_msg(&room, key, stored, text, edited) {
        use tauri::Emitter;
        let _ = app.emit(
            "chat-edit",
            serde_json::json!({ "room": room_id, "id": msg_id, "text": m.text, "edited": edited }),
        );
    }
}

/// Реакции всех сообщений комнаты: id -> [{e, n, mine}].
#[tauri::command]
pub(crate) fn chat_reactions(room_id: String) -> std::collections::HashMap<String, Vec<MsgReaction>> {
    let me = my_address().unwrap_or_default();
    let mut out = std::collections::HashMap::new();
    for m in get_messages(room_id) {
        if m.id.is_empty() || m.sys.is_some() {
            continue;
        }
        let s = react_summary(&m.id, &me);
        if !s.is_empty() {
            out.insert(m.id, s);
        }
    }
    out
}

// ---- анти-энтропийный ресинк: гарантированная доставка сообщений ------------
// Gossip — best-effort: обрывы линков/лаг/рестарты теряют пакеты. Клиенты
// периодически шлют sys="sync" со списком id принятых сообщений; автор досылает
// то, чего у пира нет. Дедуп — по id конверта (chat:seen:*), так что дубликаты
// в историю не попадают. Итог: сообщение доедет, пока есть хоть окно связи.

const SYNC_WINDOW_MS: u64 = 24 * 3600 * 1000;

/// Эпоха ресинка: сообщения старше первого запуска этой версии не досылаются
/// (у пиров со старыми билдами нет дедупа по id конверта).
fn sync_epoch() -> u64 {
    if let Ok(Some(v)) = DB.get(b"chat:sync:epoch") {
        if let Ok(s) = String::from_utf8(v.to_vec()) {
            if let Ok(n) = s.parse::<u64>() {
                return n;
            }
        }
    }
    let now = now_ms();
    let _ = DB.insert(b"chat:sync:epoch", now.to_string().as_bytes());
    now
}

/// Дедуп входящих по id конверта. true = видим впервые (можно сохранять).
/// Также ведёт кольцо последних принятых id комнаты для sync-анонсов.
pub(crate) fn mark_seen(room_id: &str, msg_id: &str) -> bool {
    if msg_id.is_empty() || msg_id.len() > 64 {
        return true;
    }
    let key = format!("chat:seen:{}:{}", room_id, msg_id);
    if DB.contains_key(key.as_bytes()).unwrap_or(false) {
        return false;
    }
    let _ = DB.insert(key.as_bytes(), now_ms().to_string().as_bytes());
    let rkey = format!("chat:seenring:{}", room_id);
    let mut ring: Vec<String> = DB
        .get(rkey.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
        .map(|s| s.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default();
    ring.push(msg_id.to_string());
    if ring.len() > 60 {
        let cut = ring.len() - 60;
        ring.drain(..cut);
    }
    let _ = DB.insert(rkey.as_bytes(), ring.join("\n").as_bytes());
    true
}

fn seen_ring(room_id: &str) -> Vec<String> {
    DB.get(format!("chat:seenring:{}", room_id).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
        .map(|s| s.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default()
}

/// Мои недавние реакции комнаты для досылки: (msg_id, emoji, act, ts).
/// Хранится ПОСЛЕДНЕЕ состояние на сообщение (включая снятие, act=0) —
/// повторное применение у пиров идемпотентно.
#[cfg(feature = "p2p")]
fn my_recent_reactions(room_id: &str, now: u64) -> Vec<(String, String, u64, u64)> {
    let prefix = format!("chat:myreact:{}:", room_id);
    let mut out: Vec<(String, String, u64, u64)> = Vec::new();
    for kv in DB.scan_prefix(prefix.as_bytes()) {
        if let Ok((k, v)) = kv {
            let msg_id = String::from_utf8_lossy(&k)[prefix.len()..].to_string();
            if let Ok(j) = serde_json::from_slice::<serde_json::Value>(&v) {
                let ts = j.get("ts").and_then(|x| x.as_u64()).unwrap_or(0);
                if now.saturating_sub(ts) >= SYNC_WINDOW_MS {
                    let _ = DB.remove(&k);
                    continue;
                }
                let e = j.get("e").and_then(|x| x.as_str()).unwrap_or("").to_string();
                if e.is_empty() || !react_valid(&msg_id, &e) {
                    continue;
                }
                let a = j.get("a").and_then(|x| x.as_u64()).unwrap_or(1);
                out.push((msg_id, e, a, ts));
            }
        }
    }
    out.sort_by_key(|x| x.3);
    if out.len() > 10 {
        let cut = out.len() - 10;
        out.drain(..cut);
    }
    out
}

/// Ответ на sys="sync" пира: дослать НАШИ сообщения, которых нет в его списке.
#[allow(unused_variables)]
pub(crate) fn handle_sync(room: &Room, their_list: &str) {
    #[cfg(feature = "p2p")]
    {
        use once_cell::sync::Lazy;
        use std::sync::Mutex;
        // v1.135.125+: строки N:/P: — анонс медиа-провайдера (пишем ДО
        // rate-limit, чтобы реестр пополнялся с каждого анонса)
        let mut peer_node = "";
        for l in their_list.lines() {
            if let Some(n) = l.strip_prefix("N:") {
                peer_node = n.trim();
            }
        }
        if !peer_node.is_empty() {
            for l in their_list.lines() {
                if let Some(h) = l.strip_prefix("P:") {
                    media::record_provider(h.trim(), peer_node);
                }
            }
        }
        // не чаще раза в 20с на комнату — защита от штормов взаимного ресинка
        static LAST: Lazy<Mutex<std::collections::HashMap<String, u64>>> =
            Lazy::new(|| Mutex::new(std::collections::HashMap::new()));
        let now = now_ms();
        {
            let mut l = LAST.lock().unwrap();
            let e = l.entry(room.id.clone()).or_insert(0);
            if now.saturating_sub(*e) < 20_000 {
                return;
            }
            *e = now;
        }
        let theirs: Vec<&str> = their_list
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with("N:") && !l.starts_with("P:"))
            .collect();
        let epoch = sync_epoch();
        let all_msgs = get_messages(room.id.clone());
        let mut missing: Vec<ChatMsg> = all_msgs
            .iter()
            .filter(|m| {
                m.mine
                    && m.sys.is_none()
                    && !m.id.is_empty()
                    && m.ts >= epoch
                    && now.saturating_sub(m.ts) > 60_000
                    && now.saturating_sub(m.ts) < SYNC_WINDOW_MS
                    // крупные конверты (старые медиа с тяжёлой миниатюрой) не
                    // ресиндим через gossip — они не влезают в 4КБ-кадр
                    && m.text.len() <= 2200
                    // список пира — 16-символьные префиксы id (или полные id
                    // от старых клиентов): сверяем по префиксу
                    && !theirs.iter().any(|t| m.id.starts_with(t))
            })
            .cloned()
            .collect();
        missing.truncate(10);
        // history sync: делимся и ЧУЖИМИ сообщениями, которых нет у пира —
        // участники доносят историю друг другу при перезаходе. Конверт несёт
        // оригинальные sender/id/ts, у получателей дедуп по id (mark_seen),
        // автор отбрасывает своё эхо (sender == me) — совместимо со старыми.
        let mut relay: Vec<ChatMsg> = all_msgs
            .iter()
            .filter(|m| {
                !m.mine
                    && m.sys.is_none()
                    && !m.id.is_empty()
                    && m.ts >= epoch
                    && now.saturating_sub(m.ts) > 60_000
                    && now.saturating_sub(m.ts) < SYNC_WINDOW_MS
                    && m.text.len() <= 2200
                    && !theirs.iter().any(|t| m.id.starts_with(t))
            })
            .cloned()
            .collect();
        relay.truncate(6);
        missing.extend(relay);
        // досылка моих недавних реакций: эфемерный sys="react" мог не дойти до
        // тех, кто был оффлайн; повтор безопасен (применение идемпотентно) и
        // срабатывает только по sync-анонсу живого пира (не постоянный спам)
        let reacts = my_recent_reactions(&room.id, now);
        if missing.is_empty() && reacts.is_empty() {
            return;
        }
        if !missing.is_empty() {
            crate::syslog::log(
                "chat",
                format!("anti-entropy: re-sending {} missed message(s)", missing.len()),
            );
        }
        let room = room.clone();
        let sender = my_address().unwrap_or_default();
        tauri::async_runtime::spawn(async move {
            for m in missing {
                // джиттер: несколько участников не ресиндят одновременно
                tokio::time::sleep(std::time::Duration::from_millis(400 + now_ms() % 900)).await;
                broadcast_envelope(&room, &m);
            }
            for (mid, e, a, _) in reacts {
                tokio::time::sleep(std::time::Duration::from_millis(250 + now_ms() % 400)).await;
                let payload = serde_json::json!({ "m": mid, "e": e, "a": a }).to_string();
                let rmsg = ChatMsg {
                    id: String::new(),
                    room: room.id.clone(),
                    sender: sender.clone(),
                    text: payload,
                    ts: now_ms(),
                    mine: false,
                    sys: Some("react".into()),
                    edited: None,
                };
                broadcast_envelope(&room, &rmsg);
            }
        });
    }
}

/// Внеплановый запуск анти-энтропии (после пересаба меша): цикл проснётся,
/// подождёт ~4с, пока соседи подключатся, и разошлёт sync по всем комнатам.
static SYNC_NOW: once_cell::sync::Lazy<tokio::sync::Notify> = once_cell::sync::Lazy::new(tokio::sync::Notify::new);

pub(crate) fn sync_now() {
    SYNC_NOW.notify_one();
}

/// Периодический sync-анонс по всем подключённым комнатам + чистка seen-ключей.
/// С v1.135.125 в text добавляются строки `N:<media_node>` и `P:<video_hash>` —
/// анонс «у меня есть это видео». ОБРАТНАЯ СОВМЕСТИМОСТЬ: старые клиенты
/// трактуют их как id-префиксы и безопасно игнорируют (id сообщений никогда
/// не начинаются с "N:"/"P:").
pub(crate) fn spawn_sync_loop(app: tauri::AppHandle) {
    #[cfg(not(feature = "p2p"))]
    let _ = app;
    #[cfg(feature = "p2p")]
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(25)).await;
        let mut last_prune: u64 = 0;
        loop {
            let media_node = crate::p2p::media_node_id(&app).await.unwrap_or_default();
            for room_id in gossip::joined_rooms() {
                if let Some(room) = load_room(&room_id) {
                    let now = now_ms();
                    // список того, что у нас есть: принятые (кольцо) + свои свежие
                    let mut ids = seen_ring(&room_id);
                    let mut have_vids: Vec<String> = Vec::new();
                    for m in get_messages(room_id.clone()) {
                        if m.mine
                            && m.sys.is_none()
                            && !m.id.is_empty()
                            && now.saturating_sub(m.ts) < SYNC_WINDOW_MS
                        {
                            ids.push(m.id.clone());
                        }
                        // анонс провайдера: видео из недавних сообщений, файл в кэше
                        if !media_node.is_empty() && have_vids.len() < 8 && m.sys.is_none() {
                            if let Some(h) = media::video_hash_of(&m.text) {
                                if !have_vids.contains(&h) && media::has_cached_video(&app, &h) {
                                    have_vids.push(h);
                                }
                            }
                        }
                    }
                    if ids.len() > 80 {
                        let cut = ids.len() - 80;
                        ids.drain(..cut);
                    }
                    // 16-символьные префиксы id: 80 записей влезают в 4КБ-кадр
                    // gossip (полные sha256-id раздували sync-конверт до ~8КБ)
                    let mut short: Vec<String> =
                        ids.iter().map(|s| s.chars().take(16).collect()).collect();
                    if !have_vids.is_empty() && !media_node.is_empty() {
                        short.push(format!("N:{}", media_node));
                        for h in &have_vids {
                            short.push(format!("P:{}", h));
                        }
                    }
                    let msg = ChatMsg {
                        id: String::new(),
                        room: room.id.clone(),
                        sender: my_address().unwrap_or_default(),
                        text: short.join("\n"),
                        ts: now,
                        mine: false,
                        sys: Some("sync".into()),
                        edited: None,
                    };
                    broadcast_envelope(&room, &msg);
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            let now = now_ms();
            if now.saturating_sub(last_prune) > 3600 * 1000 {
                last_prune = now;
                prune_seen(now);
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(45 + now % 30)) => {}
                _ = SYNC_NOW.notified() => {
                    crate::syslog::log("chat", "mesh ready — instant anti-entropy sync");
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                }
            }
        }
    });
}

fn prune_seen(now: u64) {
    let mut stale: Vec<sled::IVec> = Vec::new();
    for kv in DB.scan_prefix(b"chat:seen:") {
        if let Ok((k, v)) = kv {
            let ts = String::from_utf8(v.to_vec()).ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
            if now.saturating_sub(ts) > 7 * 24 * 3600 * 1000 {
                stale.push(k);
            }
        }
    }
    for k in stale {
        let _ = DB.remove(k);
    }
}

/// Эфемерный сигнал «печатает…»: E2EE-конверт с sys="typing" в топик комнаты.
/// НЕ сохраняется в историю — приёмник только эмитит событие в UI.
#[tauri::command]
pub(crate) fn chat_typing(room_id: String) -> Result<(), String> {
    #[cfg(feature = "p2p")]
    {
        let room = load_room(&room_id).ok_or("no room")?;
        let me = my_address().unwrap_or_default();
        let msg = ChatMsg {
            id: String::new(),
            room: room_id.clone(),
            sender: me,
            text: String::new(),
            ts: now_ms(),
            mine: false,
            sys: Some("typing".into()),
            edited: None,
        };
        broadcast_envelope(&room, &msg);
    }
    #[cfg(not(feature = "p2p"))]
    let _ = room_id;
    Ok(())
}

/// Decrypt + return a room's message history (oldest-first).
#[tauri::command]
pub(crate) fn get_messages(room_id: String) -> Vec<ChatMsg> {
    get_messages_page(room_id, None, usize::MAX).messages
}

/// Страница истории для окна чата: `limit` сообщений СТРОГО СТАРШЕ `before_ts`
/// (None = хвост истории), по возрастанию времени. Ключи sled упорядочены
/// `chat:msg:<room>:<ts:020>:<id>`, поэтому читаем с конца и останавливаемся —
/// без прохода и расшифровки всей истории (окно 50 вместо тысяч сообщений).
#[derive(Serialize)]
pub(crate) struct MessagesPage {
    pub(crate) messages: Vec<ChatMsg>,
    #[serde(rename = "hasMore")]
    pub(crate) has_more: bool,
}

#[tauri::command]
pub(crate) fn chat_get_messages_page(room_id: String, before_ts: Option<u64>, limit: usize) -> MessagesPage {
    get_messages_page(room_id, before_ts, limit.max(1))
}

fn get_messages_page(room_id: String, before_ts: Option<u64>, limit: usize) -> MessagesPage {
    let room = match load_room(&room_id) {
        Some(r) => r,
        None => return MessagesPage { messages: vec![], has_more: false },
    };
    let key = match room_key(&room) {
        Ok(k) => k,
        Err(_) => return MessagesPage { messages: vec![], has_more: false },
    };
    let prefix = msg_prefix(&room_id);
    // верхняя граница диапазона: все ключи с ts < before_ts
    let upper: Vec<u8> = match before_ts {
        Some(ts) => format!("{}{:020}:", prefix, ts).into_bytes(),
        None => {
            let mut p = prefix.clone().into_bytes();
            p.push(0xff);
            p
        }
    };
    let mut out = Vec::with_capacity(limit.min(256));
    let mut has_more = false;
    for kv in DB.range(prefix.as_bytes().to_vec()..upper).rev() {
        let Ok((_, v)) = kv else { continue };
        if out.len() >= limit {
            has_more = true;
            break;
        }
        if let Ok(s) = std::str::from_utf8(&v) {
            if let Ok(plain) = decrypt(&key, s) {
                if let Ok(m) = serde_json::from_str::<ChatMsg>(&plain) {
                    out.push(m);
                }
            }
        }
    }
    out.reverse();
    // Opening a room marks it read (только при загрузке хвоста).
    if before_ts.is_none() && room.unread != 0 {
        let mut r = room;
        r.unread = 0;
        save_room(&r);
    }
    MessagesPage { messages: out, has_more }
}

/// Удалить сообщение ЛОКАЛЬНО (у собеседников копии остаются): убираем из
/// своей БД — с нашей стороны оно больше не ресинкается анти-энтропией.
#[tauri::command]
pub(crate) fn chat_delete_message(room_id: String, msg_id: String) -> Result<(), String> {
    if msg_id.trim().is_empty() {
        return Err("empty id".into());
    }
    let suffix = format!(":{}", msg_id.trim());
    let keys: Vec<sled::IVec> = DB
        .scan_prefix(msg_prefix(&room_id).as_bytes())
        .filter_map(|kv| kv.ok().map(|(k, _)| k))
        .filter(|k| std::str::from_utf8(k).map(|s| s.ends_with(&suffix)).unwrap_or(false))
        .collect();
    for k in keys {
        let _ = DB.remove(k);
    }
    let _ = DB.flush();
    Ok(())
}

/// ANNIHILATE: wipe a room's entire local message history (peer keeps theirs).
#[tauri::command]
pub(crate) fn clear_chat_history(room_id: String) -> Result<(), String> {
    let keys: Vec<sled::IVec> = DB
        .scan_prefix(msg_prefix(&room_id).as_bytes())
        .filter_map(|kv| kv.ok().map(|(k, _)| k))
        .collect();
    for k in keys {
        let _ = DB.remove(k);
    }
    if let Some(mut r) = load_room(&room_id) {
        r.last_text = String::new();
        r.unread = 0;
        save_room(&r);
    }
    let _ = DB.flush();
    Ok(())
}

/// Wipe EVERYTHING chat-related (rooms, messages, cached peer keys/nodeids/costs,
/// settings, processed-tx markers) + in-memory caches. Called on identity reset
/// so a fresh account never inherits the previous identity's conversations.
/// netwatch: интернет вернулся после обрыва — пересобрать gossip-меш всех комнат.
pub(crate) fn on_network_recovered() {
    #[cfg(feature = "p2p")]
    gossip::resubscribe_all();
}

pub(crate) fn wipe_all_chat() {
    let keys: Vec<sled::IVec> = DB
        .scan_prefix(b"chat:")
        .filter_map(|kv| kv.ok().map(|(k, _)| k))
        .collect();
    for k in keys {
        let _ = DB.remove(k);
    }
    let _ = DB.flush();
    refresh_tray_unread();
    *FOLLOW_CACHE.lock().unwrap() = None;
    FOLLOW_TOPICS.lock().unwrap().clear();
    #[cfg(feature = "p2p")]
    gossip::reset();
}


/// Anti-spam gate for inbound packets. HARD-DROP rules:
///   1. sender must own a registered on-chain name (`naming_svc`);
///   2. if `only_allow_trusted` is active, sender must additionally be trusted —
///      i.e. we already share a room OR we are subscribed to them on-chain.
fn inbound_allowed(sender: &str, following: &[String]) -> bool {
    if naming::username_of_address(sender.to_string()).is_none() {
        return false;
    }
    if !only_allow_trusted() {
        return true;
    }
    room_exists_with(sender) || following.iter().any(|a| a == sender)
}

fn room_exists_with(peer: &str) -> bool {
    for kv in DB.scan_prefix(b"chat:room:") {
        if let Ok((_, v)) = kv {
            if let Ok(r) = serde_json::from_slice::<Room>(&v) {
                if r.peer_address == peer {
                    return true;
                }
            }
        }
    }
    false
}

/// Человекочитаемое превью конверта: медиа — метками, реплаи (`sn-rep:{json}`) —
/// внутренним текстом ответа (сырой JSON в списке комнат/уведомлениях не светим).
fn preview_of(text: &str) -> String {
    if text.starts_with("sn-img:") {
        return "[photo]".into();
    }
    if text.starts_with("sn-aud:") {
        return "[audio]".into();
    }
    if text.starts_with("sn-file:") {
        return "[file]".into();
    }
    if text.starts_with("sn-vid:") {
        return "[video]".into();
    }
    if let Some(j) = text.strip_prefix("sn-rep:") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(j) {
            if let Some(t) = v.get("t").and_then(|t| t.as_str()) {
                return t.chars().take(120).collect();
            }
        }
        return "[reply]".into();
    }
    text.chars().take(120).collect()
}

/// Persist an INBOUND (not-mine) message and notify the shell so a native OS
/// notification fires even when the messenger window is minimized.
fn store_inbound(app: &tauri::AppHandle, room: &Room, sender: &str, text: &str, id: Option<&str>, ts: Option<u64>) {
    if let Ok(msg) = store_msg_with_id(room, sender, text, false, id, ts) {
        use tauri::Emitter;
        let _ = app.emit(
            "chat-incoming",
            serde_json::json!({
                "roomId": room.id,
                "sender": sender,
                "peerName": room.peer_name,
                "text": preview_of(&msg.text),
                "ts": msg.ts,
                "notify": notifications_enabled(),
            }),
        );
    }
}

/// Persist + emit a system/service message (e.g. group "join"). Stored encrypted
/// like any message (no unread bump, no OS notification).
fn store_sys(app: Option<&tauri::AppHandle>, room: &Room, sender: &str, kind: &str, text: &str, mine: bool) -> Result<ChatMsg, String> {
    let key = room_key(room)?;
    let ts = now_ms();
    let id = format!("{:x}", Sha256::digest(format!("{}{}{}sys", ts, sender, kind).as_bytes()));
    let msg = ChatMsg {
        id: id.clone(),
        room: room.id.clone(),
        sender: sender.into(),
        text: text.into(),
        ts,
        mine,
        sys: Some(kind.into()),
        edited: None,
    };
    let blob = encrypt(&key, &serde_json::to_string(&msg).map_err(|e| e.to_string())?)?;
    let mkey = format!("{}{:020}:{}", msg_prefix(&room.id), ts, id);
    let _ = DB.insert(mkey.as_bytes(), blob.as_bytes());
    enforce_rolling(&room.id);
    let _ = DB.flush();
    if let Some(app) = app {
        use tauri::Emitter;
        let _ = app.emit(
            "chat-incoming",
            serde_json::json!({ "roomId": room.id, "sender": sender, "peerName": room.peer_name, "text": "", "ts": ts, "notify": false }),
        );
    }
    Ok(msg)
}

/// One pass of the PAID-message poller: scan incoming transfers to me carrying a
/// `msg:<ciphertext>` vendorField, enforce the anti-spam + economic gates, then
/// decrypt + store + notify. Deduped by txid (`chat:seen:<id>`).
pub(crate) fn index_paid_msgs_once(app: &tauri::AppHandle) {
    let me = match my_address() {
        Some(a) => a,
        None => return,
    };
    let txs = match sth_node::fetch_txs("recipientId", &me, 50) {
        Ok(t) => t,
        Err(_) => return,
    };
    let my_cost = my_msg_cost();
    let following = cached_following(&me);
    for t in txs {
        let cipher = match t.vendor.strip_prefix("msg:") {
            Some(c) => c.to_string(),
            None => continue,
        };
        if t.id.is_empty() {
            continue;
        }
        let seen_key = format!("chat:seen:{}", t.id);
        if DB.get(seen_key.as_bytes()).ok().flatten().is_some() {
            continue;
        }
        let _ = DB.insert(seen_key.as_bytes(), b"1");
        // Economic filter: reject under-paid messages.
        if my_cost > 0 && t.amount < my_cost {
            continue;
        }
        // Anti-spam gate.
        if !inbound_allowed(&t.sender, &following) {
            continue;
        }
        let room = match ensure_room_for(&t.sender) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let key = match shared_key(&room.peer_key) {
            Ok(k) => k,
            Err(_) => continue,
        };
        if let Ok(plain) = decrypt(&key, &cipher) {
            store_inbound(app, &room, &room.peer_address, &plain, None, None);
        }
    }
    let _ = DB.flush();
}

/// Background poller (60s) for paid (blockchain-bound) messages + auto-subscribe
/// to followed peers' gossip topics (first-contact delivery without opening chat).
pub(crate) fn spawn_msg_indexer(app: tauri::AppHandle) {
    let _ = std::thread::Builder::new().name("chat-msg-indexer".into()).spawn(move || {
        let mut first = true;
        loop {
            // offline: пропускаем сетевой тик — иначе запросы виснут в таймаутах
            if crate::netwatch::is_online() {
                index_paid_msgs_once(&app);
                subscribe_following();
            }
            if first {
                crate::sync_status::mark_done("chat");
                first = false;
            }
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    });
}

// ---- Phase 2: Iroh Gossipsub transit (free messages) --------------------
#[cfg(feature = "p2p")]
pub(crate) mod gossip;
// ---- v1.135.229: прямая анти-энтропия (ALPN chat-sync), независимая от gossip ----
#[cfg(feature = "p2p")]
pub(crate) mod sync;
// ---- v1.135.230: голосовые звонки 1-на-1 (ALPN voice, Opus в QUIC-датаграммах) ----
#[cfg(feature = "p2p")]
pub(crate) mod voice;
// ---- v1.135.235: эхоподавитель PBFDAF + AGC для звонков без наушников ----
#[cfg(feature = "p2p")]
pub(crate) mod dsp;
#[cfg(not(feature = "p2p"))]
pub(crate) mod voice {
    #[tauri::command]
    pub(crate) async fn voice_call(_room_id: String) -> Result<(), String> {
        Err("P2P выключен".into())
    }
    #[tauri::command]
    pub(crate) async fn voice_answer(_accept: bool) -> Result<(), String> {
        Err("P2P выключен".into())
    }
    #[tauri::command]
    pub(crate) async fn voice_hangup() -> Result<(), String> {
        Ok(())
    }
    #[tauri::command]
    pub(crate) fn voice_mute(_on: bool) -> bool {
        false
    }
    #[tauri::command]
    pub(crate) async fn voice_audio_settings() -> Result<serde_json::Value, String> {
        Err("P2P выключен".into())
    }
    #[tauri::command]
    pub(crate) fn voice_set_audio(_input: Option<String>, _output: Option<String>, _denoise: Option<bool>, _aec: Option<bool>, _agc: Option<bool>) {}
    #[tauri::command]
    pub(crate) fn voice_audio_route(_route: String) -> Result<(), String> {
        Err("P2P выключен".into())
    }
    #[tauri::command]
    pub(crate) fn voice_meter(_on: bool) {}
    #[tauri::command]
    pub(crate) async fn voice_group_join(_room_id: String) -> Result<(), String> {
        Err("P2P выключен".into())
    }
}
