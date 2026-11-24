// ============================================================
//  Author: TechnoL0g / Project: SmartNet Core / Date: 2026-07
//
//  dht_dapp.rs — Identity-Beacon для трекерлесс-обнаружения сидеров dApp.
//
//  Проблема: BEP5 DHT (`get_peers`) отдаёт IP:port, а iroh соединяется по
//  EndpointId (публичный ключ). Этот модуль — мост IP:port → проверенный
//  EndpointId. Работает на ОТДЕЛЬНОМ UDP-сокете (Вариант Б): чистый tokio,
//  без вмешательства в QUIC-сокет iroh (quinn забирает свой сокет целиком и
//  отбросил бы наш сырой HELLO как повреждённый QUIC-пакет).
//
//  Протокол (1 датаграмма в каждую сторону, без handshake):
//    HELLO  (24 Б):  MAGIC(4)=STH\x01 | app_infohash(20)
//    REPLY (108 Б):  MAGIC(4)=STH\x02 | endpoint_id(32) | ts(8, BE) | sig(64)
//  Подпись: ed25519 iroh-ключом сидера над [app_infohash | ts | endpoint_id].
//  Клиент проверяет подпись самим endpoint_id из пакета → мусор из DHT
//  отсекается на 100%, поддельного провайдера подсунуть нельзя.
// ============================================================
#![cfg(feature = "p2p")]
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh::{EndpointId, PublicKey, SecretKey, Signature};
use tokio::net::UdpSocket;

pub const MAGIC_HELLO: [u8; 4] = *b"STH\x01";
pub const MAGIC_REPLY: [u8; 4] = *b"STH\x02";
/// v2-ответ DevHub-сидера: дополнительно несёт blake3-хэш подписанного индекса.
pub const MAGIC_REPLY2: [u8; 4] = *b"STH\x03";
pub const HELLO_LEN: usize = 24; // 4 + 20
pub const REPLY_LEN: usize = 108; // 4 + 32 + 8 + 64
pub const REPLY2_LEN: usize = 140; // 4 + 32 + 8 + 32(extra hash) + 64
/// Допустимый перекос времени для защиты от replay (сек).
pub const MAX_SKEW_SECS: u64 = 300;
/// Минимальный интервал между ответами одному IP (анти-DDoS/усиление).
const PER_IP_MIN_INTERVAL_MS: u64 = 1500;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Канонические байты под подпись: app_infohash(20) | ts(8, BE) | endpoint_id(32).
fn signing_message(app_infohash: &[u8; 20], ts: u64, endpoint_id: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(60);
    m.extend_from_slice(app_infohash);
    m.extend_from_slice(&ts.to_be_bytes());
    m.extend_from_slice(endpoint_id);
    m
}

/// Канонические байты под подпись v2: v1 + extra_hash(32).
fn signing_message2(app_infohash: &[u8; 20], ts: u64, endpoint_id: &[u8; 32], xh: &[u8; 32]) -> Vec<u8> {
    let mut m = signing_message(app_infohash, ts, endpoint_id);
    m.extend_from_slice(xh);
    m
}

/// Собрать HELLO-пакет клиента.
pub fn encode_hello(app_infohash: &[u8; 20]) -> [u8; HELLO_LEN] {
    let mut p = [0u8; HELLO_LEN];
    p[0..4].copy_from_slice(&MAGIC_HELLO);
    p[4..24].copy_from_slice(app_infohash);
    p
}

/// Разобрать HELLO → app_infohash (или None при неверном формате/магии).
pub fn decode_hello(buf: &[u8]) -> Option<[u8; 20]> {
    if buf.len() != HELLO_LEN || buf[0..4] != MAGIC_HELLO {
        return None;
    }
    let mut ih = [0u8; 20];
    ih.copy_from_slice(&buf[4..24]);
    Some(ih)
}

/// Собрать подписанный REPLY-пакет сидера.
pub fn encode_reply(app_infohash: &[u8; 20], secret: &SecretKey) -> [u8; REPLY_LEN] {
    let eid_bytes: [u8; 32] = *secret.public().as_bytes();
    let ts = now_secs();
    let sig = secret.sign(&signing_message(app_infohash, ts, &eid_bytes));
    let mut p = [0u8; REPLY_LEN];
    p[0..4].copy_from_slice(&MAGIC_REPLY);
    p[4..36].copy_from_slice(&eid_bytes);
    p[36..44].copy_from_slice(&ts.to_be_bytes());
    p[44..108].copy_from_slice(&sig.to_bytes());
    p
}

/// Разобрать и ПРОВЕРИТЬ REPLY: формат, магия, свежесть (replay), подпись.
/// Возвращает проверенный `EndpointId` только при валидной подписи.
pub fn decode_verify_reply(buf: &[u8], app_infohash: &[u8; 20], max_skew: u64) -> Option<EndpointId> {
    if buf.len() != REPLY_LEN || buf[0..4] != MAGIC_REPLY {
        return None;
    }
    let mut eid = [0u8; 32];
    eid.copy_from_slice(&buf[4..36]);
    let mut tsb = [0u8; 8];
    tsb.copy_from_slice(&buf[36..44]);
    let ts = u64::from_be_bytes(tsb);
    let now = now_secs();
    // свежесть: ts в окне [now - skew, now + skew]
    if ts + max_skew < now || ts > now + max_skew {
        return None;
    }
    let mut sigb = [0u8; 64];
    sigb.copy_from_slice(&buf[44..108]);
    let endpoint = PublicKey::from_bytes(&eid).ok()?;
    let sig = Signature::from_bytes(&sigb);
    endpoint.verify(&signing_message(app_infohash, ts, &eid), &sig).ok()?;
    Some(endpoint)
}

/// Собрать подписанный v2-REPLY: как v1, но с extra-хэшем (blake3 индекса
/// DevHub-автора) — клиент сразу знает, какой blob качать, без доп. раундтрипа.
pub fn encode_reply2(app_infohash: &[u8; 20], secret: &SecretKey, xh: &[u8; 32]) -> [u8; REPLY2_LEN] {
    let eid_bytes: [u8; 32] = *secret.public().as_bytes();
    let ts = now_secs();
    let sig = secret.sign(&signing_message2(app_infohash, ts, &eid_bytes, xh));
    let mut p = [0u8; REPLY2_LEN];
    p[0..4].copy_from_slice(&MAGIC_REPLY2);
    p[4..36].copy_from_slice(&eid_bytes);
    p[36..44].copy_from_slice(&ts.to_be_bytes());
    p[44..76].copy_from_slice(xh);
    p[76..140].copy_from_slice(&sig.to_bytes());
    p
}

/// Разобрать и ПРОВЕРИТЬ REPLY любой версии → (EndpointId, extra-хэш v2 или None).
pub fn decode_verify_reply_ex(
    buf: &[u8],
    app_infohash: &[u8; 20],
    max_skew: u64,
) -> Option<(EndpointId, Option<[u8; 32]>)> {
    if let Some(eid) = decode_verify_reply(buf, app_infohash, max_skew) {
        return Some((eid, None));
    }
    if buf.len() != REPLY2_LEN || buf[0..4] != MAGIC_REPLY2 {
        return None;
    }
    let mut eid = [0u8; 32];
    eid.copy_from_slice(&buf[4..36]);
    let mut tsb = [0u8; 8];
    tsb.copy_from_slice(&buf[36..44]);
    let ts = u64::from_be_bytes(tsb);
    let now = now_secs();
    if ts + max_skew < now || ts > now + max_skew {
        return None;
    }
    let mut xh = [0u8; 32];
    xh.copy_from_slice(&buf[44..76]);
    let mut sigb = [0u8; 64];
    sigb.copy_from_slice(&buf[76..140]);
    let endpoint = PublicKey::from_bytes(&eid).ok()?;
    let sig = Signature::from_bytes(&sigb);
    endpoint
        .verify(&signing_message2(app_infohash, ts, &eid, &xh), &sig)
        .ok()?;
    Some((endpoint, Some(xh)))
}

/// Набор app_infohash’ей, которые этот сидер сейчас раздаёт (обновляется циклом
/// сидирования). Бикон отвечает ТОЛЬКО по ним — не палит идентичность зря.
pub type ServedSet = Arc<Mutex<std::collections::HashSet<[u8; 20]>>>;
/// infohash → extra-хэш (blake3 blob'а подписанного DevHub-индекса) для v2-ответа.
pub type ExtraMap = Arc<Mutex<HashMap<[u8; 20], [u8; 32]>>>;

/// Фиксированный LAN-порт бикона (fallback +10): обнаружение соседей по
/// широковещательному probe без Mainline DHT (локальные сети, офлайн-LAN).
pub const LAN_BEACON_PORT: u16 = 42_777;
const LAN_PORT_RANGE: u16 = 10;

/// v1.63.13: Dual-stack UDP-бинд. Один сокет `[::]` с v6only=off принимает и
/// IPv6, и IPv4 (v4-mapped) — критично для мобильных сетей, где CGNAT
/// обходится ТОЛЬКО прямым v6-соединением. v6only выставляем явно через
/// socket2 (на Windows дефолт v6only=on, tokio его не трогает). На системах
/// без IPv6-стека — фолбэк на чистый 0.0.0.0 (v4-only, как раньше).
async fn bind_dual_udp(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let try_v6 = || -> std::io::Result<std::net::UdpSocket> {
        let s = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
        let _ = s.set_only_v6(false);
        s.set_nonblocking(true)?;
        let addr: SocketAddr = (std::net::Ipv6Addr::UNSPECIFIED, port).into();
        s.bind(&addr.into())?;
        Ok(s.into())
    };
    match try_v6() {
        Ok(std_sock) => UdpSocket::from_std(std_sock),
        Err(_) => UdpSocket::bind(("0.0.0.0", port)).await,
    }
}

/// Поднять бикон на ЭФЕМЕРНОМ UDP-порту. Возвращает выбранный порт (его сидер
/// анонсирует в BEP5 DHT) и спавнит фоновый цикл ответов.
pub async fn spawn_beacon(secret: SecretKey, served: ServedSet, extra: ExtraMap) -> Result<u16, String> {
    let socket = bind_dual_udp(0).await.map_err(|e| e.to_string())?;
    let port = socket.local_addr().map_err(|e| e.to_string())?.port();
    tokio::spawn(run_beacon(socket, secret.clone(), served.clone(), extra.clone()));
    // Второй бикон на фиксированном LAN-порту: соседи находят нас broadcast'ом
    // даже когда Mainline DHT недоступен (несколько клиентов — fallback-порты).
    for p in LAN_BEACON_PORT..LAN_BEACON_PORT + LAN_PORT_RANGE {
        if let Ok(ls) = bind_dual_udp(p).await {
            tokio::spawn(run_beacon(ls, secret.clone(), served.clone(), extra.clone()));
            break;
        }
    }
    Ok(port)
}

async fn run_beacon(socket: UdpSocket, secret: SecretKey, served: ServedSet, extra: ExtraMap) {
    let mut buf = [0u8; 64];
    let mut last_seen: HashMap<IpAddr, u64> = HashMap::new();
    loop {
        let (n, from) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ih = match decode_hello(&buf[..n]) {
            Some(v) => v,
            None => continue,
        };
        // раздаём ли мы этот app? если нет — молчим (анти-leak)
        if !served.lock().unwrap().contains(&ih) {
            continue;
        }
        // анти-DDoS/усиление: не чаще 1 ответа / PER_IP_MIN_INTERVAL_MS на IP
        let now = now_ms();
        let ip = from.ip();
        if let Some(prev) = last_seen.get(&ip) {
            if now.saturating_sub(*prev) < PER_IP_MIN_INTERVAL_MS {
                continue;
            }
        }
        last_seen.insert(ip, now);
        if last_seen.len() > 4096 {
            last_seen.retain(|_, t| now.saturating_sub(*t) < 60_000);
        }
        // v2-ответ (с хэшем DevHub-индекса), если для infohash он зарегистрирован.
        let xh = extra.lock().unwrap().get(&ih).copied();
        match xh {
            Some(x) => {
                let reply = encode_reply2(&ih, &secret, &x);
                let _ = socket.send_to(&reply, from).await;
            }
            None => {
                let reply = encode_reply(&ih, &secret);
                let _ = socket.send_to(&reply, from).await;
            }
        }
    }
}

/// Клиент: опросить бикон по адресу → проверенный `EndpointId` сидера (или None).
pub async fn probe(addr: SocketAddr, app_infohash: [u8; 20], timeout: Duration) -> Option<EndpointId> {
    probe_ex(addr, app_infohash, timeout).await.map(|(e, _)| e)
}

/// Клиент: опрос бикона с поддержкой v2 → (EndpointId, extra-хэш индекса или None).
pub async fn probe_ex(
    addr: SocketAddr,
    app_infohash: [u8; 20],
    timeout: Duration,
) -> Option<(EndpointId, Option<[u8; 32]>)> {
    // Сокет по семейству адреса цели: v6-бикон соседа-мобилы опрашиваем с
    // v6-сокета (send_to с v4-сокета на v6-адрес падает family mismatch).
    let socket = if addr.is_ipv6() {
        UdpSocket::bind("[::]:0").await.ok()?
    } else {
        UdpSocket::bind("0.0.0.0:0").await.ok()?
    };
    socket.send_to(&encode_hello(&app_infohash), addr).await.ok()?;
    let mut buf = [0u8; 192];
    let (n, _) = tokio::time::timeout(timeout, socket.recv_from(&mut buf)).await.ok()?.ok()?;
    decode_verify_reply_ex(&buf[..n], &app_infohash, MAX_SKEW_SECS)
}

/// LAN-обнаружение: широковещательный hello на фиксированные порты биконов,
/// собираем ВСЕ подписанные ответы за timeout. Работает без Mainline DHT —
/// пиры локальной сети (и второй клиент на этой же машине) находятся сразу.
pub async fn lan_probe(
    app_infohash: [u8; 20],
    timeout: Duration,
) -> Vec<(EndpointId, Option<[u8; 32]>)> {
    let mut out: Vec<(EndpointId, Option<[u8; 32]>)> = Vec::new();
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0").await else {
        return out;
    };
    let _ = socket.set_broadcast(true);
    let hello = encode_hello(&app_infohash);
    for p in LAN_BEACON_PORT..LAN_BEACON_PORT + LAN_PORT_RANGE {
        let _ = socket.send_to(&hello, ("255.255.255.255", p)).await;
        let _ = socket.send_to(&hello, ("127.0.0.1", p)).await;
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buf = [0u8; 192];
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, _))) => {
                if let Some(v) = decode_verify_reply_ex(&buf[..n], &app_infohash, MAX_SKEW_SECS) {
                    if !out.iter().any(|(e, _)| *e == v.0) {
                        out.push(v);
                    }
                }
            }
            _ => break,
        }
    }
    out
}

/// `app_infohash = sha1("smartnet:dapp:" + app_id)` (20 байт, BEP5-совместимо).
pub fn app_infohash(app_id: &str) -> [u8; 20] {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(b"smartnet:dapp:");
    h.update(app_id.as_bytes());
    let d = h.finalize();
    let mut out = [0u8; 20];
    out.copy_from_slice(&d);
    out
}

/// `dev_infohash = sha1("smartnet:dev:" + address)` — обнаружение сидеров
/// DevHub-автора (подписанный индекс dev://-репозиториев) через Mainline DHT.
pub fn dev_infohash(address: &str) -> [u8; 20] {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(b"smartnet:dev:");
    h.update(address.as_bytes());
    let d = h.finalize();
    let mut out = [0u8; 20];
    out.copy_from_slice(&d);
    out
}

/// Infohash keyfile'а платного альбома: покупатели находят владельца в DHT
/// и получают хэш blob'а с ECIES-обёрнутыми ключами (v2-ответ бикона).
pub fn album_infohash(album_id: &str) -> [u8; 20] {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(b"smartnet:albkeys:");
    h.update(album_id.as_bytes());
    let d = h.finalize();
    let mut out = [0u8; 20];
    out.copy_from_slice(&d);
    out
}
