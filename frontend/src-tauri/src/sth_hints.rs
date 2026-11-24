//! SmartHoldem sth-core hints (v1.135.215): запасной путь к P2P-узлам `api://…/sth-core`,
//! когда discovery (DNS n0 / pkarr / mDNS / DHT) и sled-книга не дали адреса.
//!
//! * После удачного соединения с узлом ПУЛА клиент (не чаще раза в 30 мин на узел)
//!   отправляет на трекер `POST /sth/hint` его реальные транспортные адреса
//!   (`relay:…` / `ip:…` — тот же формат, что в peerbook). IP самого клиента не уходит.
//! * При неудаче connect к узлу пула — `GET /sth/hints` (кэш 10 мин, по трекерам по
//!   очереди) → `EndpointAddr` с подсказками → ещё одна попытка connect.
//!
//! Мы не зависим от трекера: без него работает обычный discovery; подсказки — только
//! ускорение/спасение. Безопасность не страдает — iroh аутентифицирует узел по EndpointId.

use iroh::{EndpointAddr, EndpointId};
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const FETCH_TTL: Duration = Duration::from_secs(600);
const REPORT_GAP: Duration = Duration::from_secs(1800);
const HTTP_TIMEOUT: Duration = Duration::from_secs(6);

struct Cache {
    at: Option<Instant>,
    hints: HashMap<String, Vec<String>>,
    reported: HashMap<String, Instant>,
}

static CACHE: std::sync::LazyLock<Mutex<Cache>> =
    std::sync::LazyLock::new(|| Mutex::new(Cache { at: None, hints: HashMap::new(), reported: HashMap::new() }));

/// EndpointId узла из `api://<id>/sth-core`.
fn pool_ids() -> Vec<String> {
    crate::sth_node::iroh_nodes()
        .iter()
        .filter_map(|u| u.strip_prefix("api://").and_then(|r| r.split('/').next()).map(|s| s.to_ascii_lowercase()))
        .collect()
}

/// Узел входит во встроенный пул sth-core (подсказки шлём/берём только для них).
pub(crate) fn is_pool_node(id: &EndpointId) -> bool {
    let hex = id.to_string().to_ascii_lowercase();
    pool_ids().iter().any(|p| *p == hex)
}

/// Репортёр — sha256 от нашего EndpointId (не раскрывает сам id), для подсчёта
/// независимых подтверждений на трекере.
fn reporter_tag() -> String {
    let me = crate::p2p::my_endpoint_id_hex().unwrap_or_default();
    let h = hex::encode(crate::crypto::sha256(format!("sth-hint-reporter|{me}").as_bytes()));
    h[..16].to_string()
}

/// После удачного соединения с узлом пула: отправить его адреса на трекер (фон, троттл).
pub(crate) fn maybe_report(id: &EndpointId, addrs: &[String]) {
    if addrs.is_empty() || !is_pool_node(id) {
        return;
    }
    let hex = id.to_string().to_ascii_lowercase();
    {
        let mut c = CACHE.lock().unwrap();
        if c.reported.get(&hex).map_or(false, |t| t.elapsed() < REPORT_GAP) {
            return;
        }
        c.reported.insert(hex.clone(), Instant::now());
    }
    let trackers = crate::p2p::get_trackers_internal();
    if trackers.is_empty() {
        return;
    }
    let body = serde_json::json!({ "id": hex, "addrs": addrs, "reporter": reporter_tag() });
    let short = hex[..10].to_string();
    std::thread::Builder::new()
        .name("sth-hint-report".into())
        .spawn(move || {
            for (i, t) in trackers.iter().enumerate() {
                let url = format!("{}/sth/hint", crate::p2p::normalize_tracker(t));
                match ureq::post(&url).timeout(HTTP_TIMEOUT).set("Content-Type", "application/json").send_string(&body.to_string()) {
                    Ok(_) => {
                        crate::syslog::log("net", format!("sth-hint: адреса узла {short}… отправлены на трекер #{}", i + 1));
                        return;
                    }
                    Err(ureq::Error::Status(429, _)) => return,
                    Err(_) => continue,
                }
            }
        })
        .ok();
}

/// Подсказки с трекеров (кэш 10 мин). Блокирующий HTTP — вызывать из spawn_blocking.
fn fetch_blocking() -> HashMap<String, Vec<String>> {
    {
        let c = CACHE.lock().unwrap();
        if c.at.map_or(false, |t| t.elapsed() < FETCH_TTL) {
            return c.hints.clone();
        }
    }
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let pool = pool_ids();
    for (i, t) in crate::p2p::get_trackers_internal().iter().enumerate() {
        let url = format!("{}/sth/hints", crate::p2p::normalize_tracker(t));
        let Ok(resp) = ureq::get(&url).timeout(HTTP_TIMEOUT).call() else { continue };
        let Ok(txt) = resp.into_string() else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { continue };
        let Some(nodes) = v["nodes"].as_array() else { continue };
        for n in nodes {
            let Some(id) = n["id"].as_str() else { continue };
            let id = id.to_ascii_lowercase();
            if !pool.iter().any(|p| *p == id) {
                continue; // чужие id игнорируем — только встроенный пул
            }
            let addrs: Vec<String> = n["addrs"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(|s| s.to_string())).take(8).collect();
            if !addrs.is_empty() {
                out.entry(id).or_insert(addrs);
            }
        }
        if !out.is_empty() {
            crate::syslog::log("net", format!("sth-hints: трекер #{} дал подсказки по {} узлам", i + 1, out.len()));
            break;
        }
    }
    let mut c = CACHE.lock().unwrap();
    c.at = Some(Instant::now());
    if !out.is_empty() {
        c.hints = out.clone();
    }
    c.hints.clone()
}

/// `EndpointAddr` узла из подсказок трекера (None — нет подсказок / не узел пула).
pub(crate) async fn addr_for(id: &EndpointId) -> Option<EndpointAddr> {
    if !is_pool_node(id) {
        return None;
    }
    let hex = id.to_string().to_ascii_lowercase();
    let hints = tokio::task::spawn_blocking(fetch_blocking).await.ok()?;
    let raw = hints.get(&hex)?;
    let addrs: BTreeSet<iroh::TransportAddr> = raw.iter().filter_map(|s| crate::peerbook::decode(s)).collect();
    if addrs.is_empty() {
        return None;
    }
    Some(EndpointAddr::from_parts(*id, addrs))
}

/// Сбросить кэш подсказок (например, после смены списка трекеров).
#[allow(dead_code)]
pub(crate) fn invalidate() {
    CACHE.lock().unwrap().at = None;
}
