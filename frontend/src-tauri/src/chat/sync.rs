//! v1.135.229: ПРЯМАЯ анти-энтропия (ALPN `netfory/chat-sync/1`), не зависящая
//! от топологии gossip. Gossip (HyParView/Plumtree) при NAT/relay и churn'е
//! распадается на «острова»: клиент видит ONLINE N, получает часть сообщений,
//! а чужие острова до него не доходят — ни конверты, ни sys="sync" (он тоже
//! едет по gossip). Здесь клиент сам ДОЗВАНИВАЕТСЯ до известных участников
//! комнаты прямым QUIC-соединением и ВЫТЯГИВАЕТ всё, чего у него нет:
//!   req : {"room","auth","since","have":[префиксы id]}
//!   resp: {"env":[cipher,…],"peers":[EndpointId,…]} — недостающие конверты
//!         + известные ответчику участники (реестры участников сливаются)
//! `auth` = sha256(room_key ‖ "chat-sync" ‖ room_id): отвечаем только тому,
//! кто владеет ключом комнаты. Принятые конверты идут в тот же конвейер, что и
//! gossip/chat-dm (`gossip::handle_direct` → дедуп, квитанция, ре-бродкаст в
//! свой остров). Старые клиенты без этого ALPN — ошибка connect, просто skip.

use super::{envelope_cipher, get_messages, group_boots, load_room, now_ms, peer_node_id, room_key, seen_ring, Room};
use sha2::{Digest, Sha256};

pub(crate) const CHAT_SYNC_ALPN: &[u8] = b"netfory/chat-sync/1";

const WINDOW_MS: u64 = 24 * 3600 * 1000;
const MAX_ENVELOPES: usize = 40;
const MAX_RESP_BYTES: usize = 2 * 1024 * 1024;
const MAX_REQ_BYTES: usize = 64 * 1024;
/// Период фонового pull-цикла (с джиттером) и число пиров на комнату за цикл.
const PULL_EVERY_SECS: u64 = 120;
const PEERS_PER_ROOM: usize = 3;

fn auth_for(room: &Room) -> Option<String> {
    let key = room_key(room).ok()?;
    let mut h = Sha256::new();
    h.update(key);
    h.update(b"chat-sync");
    h.update(room.id.as_bytes());
    Some(hex::encode(h.finalize()))
}

/// 16-символьные префиксы id всего, что у нас есть по комнате (принятые + свои).
fn have_prefixes(room_id: &str, now: u64) -> Vec<String> {
    let mut ids = seen_ring(room_id);
    for m in get_messages(room_id.to_string()) {
        if m.sys.is_none() && !m.id.is_empty() && now.saturating_sub(m.ts) < WINDOW_MS {
            ids.push(m.id);
        }
    }
    let mut out: Vec<String> = ids.iter().map(|s| s.chars().take(16).collect()).collect();
    out.sort();
    out.dedup();
    if out.len() > 400 {
        let cut = out.len() - 400;
        out.drain(..cut);
    }
    out
}

/// Известные EndpointId участников комнаты (без порядка): bootstrap-узлы группы,
/// текущие gossip-соседи, для лички — ончейн-узел собеседника.
pub(crate) fn known_member_nodes(room: &Room) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if room.kind == "group" {
        set.extend(group_boots(&room.id));
    } else {
        set.extend(peer_node_id(&room.peer_address));
    }
    set.extend(super::gossip::room_peers(&room.id));
    if let Some(me) = crate::p2p::chat_endpoint_id() {
        set.remove(&me);
    }
    set.into_iter().collect()
}

/// Перемешанный список (детерминизм не нужен — rand есть в обоих крейтах).
pub(crate) fn shuffled(mut v: Vec<String>) -> Vec<String> {
    use rand::seq::SliceRandom;
    v.shuffle(&mut rand::thread_rng());
    v
}

// ---- ответчик ----------------------------------------------------------------

/// Ответ на запрос пира: конверты комнаты, которых у него нет.
pub(crate) fn respond(req: &[u8]) -> Option<Vec<u8>> {
    let j: serde_json::Value = serde_json::from_slice(req).ok()?;
    let room_id = j["room"].as_str()?.to_string();
    if room_id.len() > 128 {
        return None;
    }
    let room = load_room(&room_id)?;
    let auth = j["auth"].as_str().unwrap_or("");
    if auth.is_empty() || auth_for(&room).as_deref() != Some(auth) {
        crate::syslog::log("chat", "chat-sync: запрос с неверным auth отклонён");
        return None;
    }
    let now = now_ms();
    let since = j["since"].as_u64().unwrap_or(0).max(now.saturating_sub(WINDOW_MS));
    let have: Vec<String> = j["have"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let mut env: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    for m in get_messages(room_id.clone()) {
        if m.sys.is_some() || m.id.is_empty() || m.ts < since || m.ts > now.saturating_add(60_000) {
            continue;
        }
        if have.iter().any(|p| m.id.starts_with(p.as_str())) {
            continue;
        }
        let Some(c) = envelope_cipher(&room, &m) else { continue };
        bytes += c.len();
        if bytes > MAX_RESP_BYTES {
            break;
        }
        env.push(c);
        if env.len() >= MAX_ENVELOPES {
            break;
        }
    }
    if !env.is_empty() {
        crate::syslog::log("chat", format!("chat-sync: отдаём пиру {} конверт(ов)", env.len()));
    }
    // обмен реестрами участников: запросивший узнаёт тех, кого сам ещё не видел
    let peers: Vec<String> = if room.kind == "group" {
        shuffled(known_member_nodes(&room)).into_iter().take(32).collect()
    } else {
        Vec::new()
    };
    Some(serde_json::json!({ "env": env, "peers": peers }).to_string().into_bytes())
}

/// Приёмник ALPN chat-sync: одно соединение — много bi-stream'ов (по комнате).
#[derive(Debug, Clone)]
pub(crate) struct ChatSync;

impl iroh::protocol::ProtocolHandler for ChatSync {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), iroh::protocol::AcceptError> {
        loop {
            let (mut send, mut recv) = match connection.accept_bi().await {
                Ok(s) => s,
                Err(_) => break,
            };
            let Ok(buf) = recv.read_to_end(MAX_REQ_BYTES).await else { break };
            let resp = tauri::async_runtime::spawn_blocking(move || respond(&buf)).await.ok().flatten();
            if let Some(r) = resp {
                let _ = send.write_all(&r).await;
            }
            let _ = send.finish();
        }
        Ok(())
    }
}

// ---- запросчик ---------------------------------------------------------------

/// Вытянуть у узла всё недостающее по комнатам. None = не подключились
/// (офлайн / старый клиент без ALPN), Some(n) = принято n конвертов.
async fn pull_from(endpoint: &iroh::Endpoint, node: &str, rooms: &[Room]) -> Option<usize> {
    let id = node.parse::<iroh::EndpointId>().ok()?;
    let me = crate::p2p::chat_endpoint_id();
    let mut got = 0usize;
    // Mycelium вкл: каждый запрос по комнате — отдельный mushroom-поток; при отказе цепочки и
    // запрете прямого пути (fail-closed) — пропускаем узел до следующего прохода.
    let mut direct: Vec<&Room> = Vec::new();
    if crate::mycel::enabled() {
        for room in rooms {
            let Some(req) = sync_request(room).await else { continue };
            match crate::mycel::try_request(id, CHAT_SYNC_ALPN, req.as_bytes(), MAX_RESP_BYTES + 4096).await {
                Some(Ok(buf)) => got += apply_response(room, &buf, me.as_deref()),
                Some(Err(_)) => return if got > 0 { Some(got) } else { None },
                None => direct.push(room),
            }
        }
        if direct.is_empty() {
            return Some(got);
        }
    } else {
        direct = rooms.iter().collect();
    }
    let conn = match tokio::time::timeout(std::time::Duration::from_secs(10), endpoint.connect(id, CHAT_SYNC_ALPN)).await {
        Ok(Ok(c)) => c,
        _ => return None,
    };
    for room in direct {
        let Some(req) = sync_request(room).await else { continue };
        let Ok((mut send, mut recv)) = conn.open_bi().await else { break };
        if send.write_all(req.as_bytes()).await.is_err() || send.finish().is_err() {
            break;
        }
        let Ok(Ok(buf)) = tokio::time::timeout(std::time::Duration::from_secs(20), recv.read_to_end(MAX_RESP_BYTES + 4096)).await else {
            break;
        };
        got += apply_response(room, &buf, me.as_deref());
    }
    conn.close(0u32.into(), b"done");
    Some(got)
}

/// Запрос анти-энтропии по комнате: auth + окно + префиксы уже имеющихся конвертов.
async fn sync_request(room: &Room) -> Option<String> {
    let auth = auth_for(room)?;
    let now = now_ms();
    let rid = room.id.clone();
    let have = tauri::async_runtime::spawn_blocking(move || have_prefixes(&rid, now)).await.unwrap_or_default();
    Some(
        serde_json::json!({
            "room": room.id,
            "auth": auth,
            "since": now.saturating_sub(WINDOW_MS),
            "have": have,
        })
        .to_string(),
    )
}

/// Принять ответ: конверты → обработчик, участники группы → bootstrap. Возвращает число конвертов.
fn apply_response(room: &Room, buf: &[u8], me: Option<&str>) -> usize {
    let Ok(j) = serde_json::from_slice::<serde_json::Value>(buf) else { return 0 };
    let mut got = 0usize;
    if let Some(env) = j["env"].as_array() {
        for c in env.iter().filter_map(|x| x.as_str()) {
            super::gossip::handle_direct(room.id.clone(), c.to_string());
            got += 1;
        }
    }
    if room.kind == "group" {
        if let Some(peers) = j["peers"].as_array() {
            for p in peers.iter().filter_map(|x| x.as_str()).take(32) {
                if p.parse::<iroh::EndpointId>().is_ok() && Some(p) != me {
                    super::add_group_boot(&room.id, p);
                }
            }
        }
    }
    got
}

/// Один проход: по каждой живой комнате — до PEERS_PER_ROOM случайных известных узлов.
pub(crate) async fn pull_once() {
    let Some(endpoint) = crate::p2p::chat_endpoint().await else { return };
    let now = now_ms();
    let rooms: Vec<Room> = super::gossip::joined_rooms()
        .into_iter()
        .filter_map(|id| load_room(&id))
        .filter(|r| now.saturating_sub(r.last_ts) <= 48 * 3600 * 1000)
        .collect();
    // узел → комнаты, по которым он нам известен (одно соединение на узел)
    let mut by_node: std::collections::HashMap<String, Vec<Room>> = std::collections::HashMap::new();
    for room in &rooms {
        for node in shuffled(known_member_nodes(room)).into_iter().take(PEERS_PER_ROOM) {
            by_node.entry(node).or_default().push(room.clone());
        }
    }
    let mut total = 0usize;
    let mut peers_ok = 0usize;
    for (node, rs) in by_node {
        if let Some(n) = pull_from(&endpoint, &node, &rs).await {
            total += n;
            peers_ok += 1;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    if total > 0 {
        crate::syslog::log("chat", format!("chat-sync: вытянуто {total} недостающих конверт(ов) с {peers_ok} узл(ов)"));
    }
}

static PULL_NOW: once_cell::sync::Lazy<tokio::sync::Notify> = once_cell::sync::Lazy::new(tokio::sync::Notify::new);

/// Внеплановый pull (после пересаба меша / возврата сети / отправки).
pub(crate) fn pull_now() {
    PULL_NOW.notify_one();
}

/// Фоновый цикл: первый проход через 20с после старта, далее каждые ~2 мин.
pub(crate) fn spawn_pull_loop() {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        loop {
            pull_once().await;
            let jitter = now_ms() % 30;
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(PULL_EVERY_SECS + jitter)) => {}
                _ = PULL_NOW.notified() => {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
            }
        }
    });
}
