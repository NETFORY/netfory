//! Саморегистрация узла на трекерах NETFORY (v0.2.0): подписанный heartbeat
//! `POST /pqweb/announce` каждые `interval` — клиенты получают узел через `GET /pqweb`
//! без ручного ввода EndpointId. Подпись — ed25519 ключом самого узла (= EndpointId).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, SecretKey};

/// Публичные трекеры NETFORY (те же, что в клиенте).
pub const DEFAULT_TRACKERS: &[&str] = &[
    "http://159.69.188.60:7374",
    "http://91.99.119.119:7374",
    "http://212.193.6.217:7374",
    "http://146.103.108.25:7374",
];

pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(8);

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn normalize(t: &str) -> String {
    let t = t.trim().trim_end_matches('/');
    if t.starts_with("http://") || t.starts_with("https://") {
        t.to_string()
    } else {
        format!("http://{t}")
    }
}

/// Канон подписи v5 (pq-web ≥ 0.6.0) — 1:1 с трекером (`pqweb_announce_canon`): адреса, relay,
/// `open`, `ipv6` и `mycelium` подписаны, чтобы клиент доверял флагам узла только с его подписью.
/// Узел без Mycelium подписывает v4-канон (без хвоста) — старые трекеры принимают его как прежде.
#[allow(clippy::too_many_arguments)]
pub fn canon(id: &str, region: &str, ver: &str, ts: u64, addrs: &[String], relay: &str, open: bool, ipv6: bool, mycelium: bool) -> String {
    let v4 = format!("pqweb-announce|{id}|{region}|{ver}|{ts}|{}|{relay}|{}|{}", addrs.join(","), if open { 1 } else { 0 }, if ipv6 { 1 } else { 0 });
    if mycelium {
        format!("{v4}|1")
    } else {
        v4
    }
}

fn is_public(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => !(v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))),
        std::net::IpAddr::V6(v6) => !(v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80),
    }
}

/// Публичные ip:port узла (из EndpointAddr iroh + явный `--public-addr`) и home-relay.
pub fn dial_info(endpoint: &Endpoint, public_addr: Option<SocketAddr>) -> (Vec<String>, String) {
    let ea = endpoint.addr();
    let mut addrs: Vec<String> = public_addr.into_iter().map(|a| a.to_string()).collect();
    for a in ea.ip_addrs() {
        if is_public(a.ip()) && !addrs.contains(&a.to_string()) {
            addrs.push(a.to_string());
        }
    }
    addrs.truncate(6);
    let relay = ea.relay_urls().next().map(|u| u.to_string()).unwrap_or_default();
    (addrs, relay)
}

/// Подписанное тело heartbeat'а.
#[allow(clippy::too_many_arguments)]
pub fn body(secret: &SecretKey, region: &str, ports: &[u16], addrs: &[String], relay: &str, open: bool, ipv6: bool, mycelium: bool) -> serde_json::Value {
    let id = secret.public().to_string();
    let ver = env!("CARGO_PKG_VERSION");
    let ts = now_unix();
    let sig = secret.sign(canon(&id, region, ver, ts, addrs, relay, open, ipv6, mycelium).as_bytes());
    serde_json::json!({
        "id": id,
        "region": region,
        "ver": ver,
        "ports": ports,
        "addrs": addrs,
        "relay": relay,
        "open": open,
        "ipv6": ipv6,
        "mycelium": mycelium,
        "ts": ts,
        "sig": hex::encode(sig.to_bytes()),
    })
}

/// Один heartbeat на один трекер (блокирующий ureq — звать из spawn_blocking).
fn post_once(tracker: &str, body: &serde_json::Value) -> Result<String, String> {
    let url = format!("{}/pqweb/announce", normalize(tracker));
    match ureq::post(&url).timeout(HTTP_TIMEOUT).set("Content-Type", "application/json").send_string(&body.to_string()) {
        Ok(resp) => {
            let txt = resp.into_string().unwrap_or_default();
            let v: serde_json::Value = serde_json::from_str(&txt).unwrap_or_default();
            let verified = v.get("verified").and_then(|x| x.as_bool()).unwrap_or(false);
            Ok(if verified { "accepted (verified)".into() } else { "accepted".into() })
        }
        Err(ureq::Error::Status(code, resp)) => {
            let txt = resp.into_string().unwrap_or_default();
            let err = serde_json::from_str::<serde_json::Value>(&txt).ok().and_then(|v| v.get("error").and_then(|e| e.as_str()).map(|s| s.to_string())).unwrap_or(txt);
            Err(format!("HTTP {code}: {err}"))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Фоновый цикл: heartbeat на все трекеры каждые `interval`; в лог — только смена
/// состояния по трекеру (accepted ↔ rejected/недоступен), чтобы не спамить.
/// `ipv6` — живой флаг egress (в `auto` может меняться между heartbeat'ами).
/// `mycelium` — узел принимает mushroom-цепочки (`--mycelium`), клиенты берут его в bootstrap.
#[allow(clippy::too_many_arguments)]
pub fn spawn(endpoint: Endpoint, secret: SecretKey, trackers: Vec<String>, region: String, ports: Vec<u16>, public_addr: Option<SocketAddr>, open: bool, ipv6: Arc<AtomicBool>, mycelium: bool, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last: Vec<Option<bool>> = vec![None; trackers.len()];
        let mut last_dial = String::new();
        loop {
            let (addrs, relay) = dial_info(&endpoint, public_addr);
            let v6 = ipv6.load(Ordering::Relaxed);
            let dial = format!("{} {} v6={}", addrs.join(","), relay, v6);
            if dial != last_dial {
                tracing::info!(addrs = %addrs.join(","), %relay, ipv6 = v6, "announce: dial info");
                last_dial = dial;
            }
            let b = body(&secret, &region, &ports, &addrs, &relay, open, v6, mycelium);
            for (i, t) in trackers.iter().enumerate() {
                let (t2, b2) = (t.clone(), b.clone());
                let res = tokio::task::spawn_blocking(move || post_once(&t2, &b2)).await.unwrap_or_else(|e| Err(e.to_string()));
                let ok = res.is_ok();
                if last[i] != Some(ok) {
                    match &res {
                        Ok(s) => tracing::info!(tracker = %t, "announce {s}"),
                        Err(e) => tracing::warn!(tracker = %t, "announce failed: {e}"),
                    }
                    last[i] = Some(ok);
                } else if let Err(e) = &res {
                    tracing::debug!(tracker = %t, "announce still failing: {e}");
                }
            }
            tokio::time::sleep(interval).await;
        }
    })
}

/// Соседние релеи Mycelium из `GET /pqweb` трекеров: EndpointId узлов с флагом `mycelium`
/// (кроме своего). Блокирующий ureq — звать из spawn_blocking.
pub fn mycelium_peers(trackers: &[String], me: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in trackers {
        let url = format!("{}/pqweb", normalize(t));
        let Ok(resp) = ureq::get(&url).timeout(HTTP_TIMEOUT).call() else { continue };
        let Ok(v) = resp.into_json::<serde_json::Value>() else { continue };
        for n in v.get("nodes").and_then(|n| n.as_array()).into_iter().flatten() {
            if !n.get("mycelium").and_then(|m| m.as_bool()).unwrap_or(false) {
                continue;
            }
            let Some(id) = n.get("id").and_then(|i| i.as_str()) else { continue };
            let id = id.trim().to_ascii_lowercase();
            if id != me && !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}
