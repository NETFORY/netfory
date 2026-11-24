/*
 * SmartNet // SmartHoldem (STH) node client + transaction engine
 *
 *   Live integration with the SmartHoldem network REST API
 *   (https://node0.smartholdem.io/api). Reads balances and transfer history,
 *   and builds + signs + broadcasts v2 transfer transactions entirely in Rust.
 *
 *   The secret (passphrase / private key material) is supplied by the caller
 *   from the in-memory session and NEVER leaves the Rust process.
 *
 *   Signing scheme: SmartHoldem transactions use the legacy bip-schnorr
 *   construction over secp256k1 (Pieter Wuille reference, "is-square" R
 *   convention) — NOT BIP-340. The exact algorithm is reproduced below so the
 *   signatures validate on SmartHoldem Core nodes.
 *
 *   Amounts are expressed in `smarthoshi` (1 STH = 100_000_000 smarthoshi).
 *
 *   Author: TechnoL0g
 *   Created: 2026-06-18
 */

use num_bigint::BigUint;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Per-address last-used (broadcast) nonce with the moment it was sent. The node
/// returns the CONFIRMED nonce, so for rapid follow-up sends within a block
/// window we advance past our own in-flight nonce — but only for `INFLIGHT_TTL`
/// (a few blocks). After that the chain nonce is authoritative again.
///
/// Раньше кэш жил вечно и рос при каждом «accept» любого узла (в т.ч. локального
/// пула, чью tx сеть потом отвергла) — nonce уезжал вперёд (API 1250, кошелёк
/// слал 1255 → ERR_BAD_NONCE у делегата). Теперь: TTL 60с, сброс при отказе.
struct InflightNonce {
    nonce: u64,
    at: std::time::Instant,
}
const INFLIGHT_TTL: Duration = Duration::from_secs(60);

fn nonce_cache() -> &'static Mutex<HashMap<String, InflightNonce>> {
    static CACHE: OnceLock<Mutex<HashMap<String, InflightNonce>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Следующий nonce для подписи: chain+1, либо (in-flight+1), если мы сами
/// отправили tx менее 60с назад и цепочка её ещё не учла.
fn next_nonce(sender: &str, chain_nonce: u64) -> u64 {
    let mut cache = nonce_cache().lock().unwrap();
    cache.retain(|_, v| v.at.elapsed() < INFLIGHT_TTL);
    let candidate = chain_nonce + 1;
    let next = match cache.get(sender) {
        Some(v) if v.nonce >= candidate => v.nonce + 1,
        _ => candidate,
    };
    if next != candidate {
        crate::syslog::log("net", format!("nonce: chain {} → in-flight {} → using {}", chain_nonce, next - 1, next));
    }
    next
}

/// Итог broadcast'а для кэша: принята сетью → запомнить; отказ → забыть
/// (в т.ч. ERR_BAD_NONCE — следующая попытка пойдёт строго от API).
fn note_nonce_outcome(sender: &str, nonce: u64, outcome: &BroadcastOutcome) {
    let mut cache = nonce_cache().lock().unwrap();
    if outcome.accepted {
        cache.insert(sender.to_string(), InflightNonce { nonce, at: std::time::Instant::now() });
    } else {
        cache.remove(sender);
    }
}

/// Default SmartHoldem mainnet node pool (overridable via `SMARTHOLDEM_NODES`,
/// comma-separated hostnames). On startup we ping all and connect to the lowest
/// -latency online node; on a request failure we fail over to ONE backup (never
/// hammer the whole pool — the public nodes IP-ban clients that poll too hard).
const DEFAULT_NODES: &[&str] = &[
    "node0.smartholdem.io",
    "node1.smartholdem.io",
    "node2.smartholdem.io",
    "node3.smartholdem.io",
    "node4.smartholdem.io",
    "node5.smartholdem.io",
    "node0.sth.cx",
];

/// Основной пул: узлы sth-core по iroh (SHIP-44, ALPN `netfory/api/1` для
/// REST + `sth/events/1` для push-событий). Адрес `api://<EndpointId>/sth-core`
/// — путь REST добавляется как `/api/...`. HTTPS-пул ниже остаётся legacy
/// fallback: используется, когда ни один iroh-узел недоступен. Переопределение:
/// `SMARTHOLDEM_IROH_NODES` (через запятую).
const DEFAULT_IROH_NODES: &[&str] = &[
    "api://1692c5f342542465819ff31c31432e933455ef3daa788d7297065869e723b790/sth-core",
    "api://d8c32e15974a4197f16d61b7cc03e4be4338cde2deca6310589584adbc465bf6/sth-core",
    "api://b429afd5ce4f59989ab0f87d2e4778cfaac94ed27e2ffd5c32998661ed99f761/sth-core",
    "api://d3e9d5fd1904cc5c1189c1fa3de219b94fe8dc1bcca43b5c81590384a3e352b2/sth-core",
    "api://58513d44a5ccbd8605633a1e7568bda0caca930ae02f73dd21b6eb0dd4bca76c/sth-core",
    "api://74e98fd504bec4337d958f55aa72cd07a8962128508dc63b6e797a82295c3520/sth-core",
    "api://67ea4f941b41e06d51140586d9d222ec3dac88e7a91c5fdc49d2788bec83c8b4/sth-core",
    "api://3d446a8bc525d1060200ba99939515385218744211f412c95703974700850b73/sth-core",
    "api://3852440f15a0ad9527426b4ae0f25889f4744f0302c06b2eb8487f2e561fdcf6/sth-core",
    "api://3e141032f9996c63b7bab2c47c6bd2778a6656de08ca33ff8d9a93ebb091bb7a/sth-core",
];

pub(crate) fn iroh_nodes() -> &'static Vec<String> {
    static NODES: OnceLock<Vec<String>> = OnceLock::new();
    NODES.get_or_init(|| {
        let from_env = std::env::var("SMARTHOLDEM_IROH_NODES").ok().map(|s| {
            s.split(',')
                .map(|x| x.trim().trim_end_matches('/').to_string())
                .filter(|x| x.starts_with("api://"))
                .collect::<Vec<_>>()
        });
        match from_env {
            Some(v) if !v.is_empty() => v,
            _ => DEFAULT_IROH_NODES.iter().map(|s| s.to_string()).collect(),
        }
    })
}

/// Доступен ли iroh-транспорт в этой сборке (desktop `p2p`). Без него
/// iroh-пул не пингуется и активным по умолчанию остаётся HTTPS-узел.
fn iroh_available() -> bool {
    cfg!(feature = "p2p")
}

/// Стандартный пул: iroh-узлы sth-core первыми, затем встроенные clearnet-узлы
/// (v1.135.210: всегда в пуле как резерв — `auto_select_best` берёт их, только когда
/// ни один P2P-узел не онлайн; удалить их из UI нельзя). Без P2P — только clearnet.
fn pool_all() -> Vec<String> {
    let mut v = if iroh_available() { iroh_nodes().clone() } else { Vec::new() };
    v.extend(nodes().iter().cloned());
    v
}

/// Legacy HTTPS-пул (только без P2P / для is_custom_clearnet).
fn nodes() -> &'static Vec<String> {
    static NODES: OnceLock<Vec<String>> = OnceLock::new();
    NODES.get_or_init(|| {
        let from_env = std::env::var("SMARTHOLDEM_NODES").ok().map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
        });
        match from_env {
            Some(v) if !v.is_empty() => v,
            _ => DEFAULT_NODES.iter().map(|s| s.to_string()).collect(),
        }
    })
}

static ACTIVE_NODE: Mutex<Option<String>> = Mutex::new(None);
/// Last measured latency (ms) of the active node, cached so the UI can show a
/// connection-quality dot without issuing a fresh request.
static ACTIVE_LATENCY: Mutex<Option<u64>> = Mutex::new(None);

/// Whether the last wallet/blockchain call over a selected `api://` node had to
/// fall back to the clearnet HTTPS pool. Drives the transport badge: when the
/// active node is P2P, `true` = "clearnet-fallback", `false` = "p2p".
static LAST_FALLBACK: Mutex<bool> = Mutex::new(false);

fn set_fallback(v: bool) {
    *LAST_FALLBACK.lock().unwrap() = v;
}

/// App handle, captured at startup so the synchronous node client can dispatch
/// `api://` P2P requests onto the Tauri/iroh async runtime (see `node_get`).
static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Called once from `lib.rs` setup so the P2P transport is reachable from the
/// (blocking) node REST client.
pub fn set_app_handle(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);
}

/// App handle для не-командных путей (трей, события) — None до setup.
pub(crate) fn app_handle() -> Option<tauri::AppHandle> {
    APP_HANDLE.get().cloned()
}

/// True when `host` is a P2P node address (`api://<nodeId>/<provider>`) rather
/// than a clearnet hostname.
fn is_p2p(host: &str) -> bool {
    host.starts_with("api://")
}

/// Currently selected node (defaults to the first iroh node when the P2P
/// transport is available, else the first HTTPS pool node).
pub fn active_host() -> String {
    ACTIVE_NODE.lock().unwrap().clone().unwrap_or_else(|| {
        if iroh_available() {
            iroh_nodes()[0].clone()
        } else {
            nodes()[0].clone()
        }
    })
}

/// Pin the active node (manual selection / auto-failover). Pool hostnames and
/// `api://` P2P addresses are both accepted; unknown clearnet hosts are ignored
/// (the UI validates custom additions before calling).
pub fn set_active_node(host: String) {
    let known = is_p2p(&host)
        || pool_all().iter().any(|n| n == &host)
        || custom_nodes().lock().unwrap().iter().any(|n| n == &host);
    if known {
        *ACTIVE_NODE.lock().unwrap() = Some(host);
        set_fallback(false); // fresh node — clear stale fallback state
    }
}

/// Переключить активный узел с уведомлением (syslog + событие `sth-node-switched`
/// для статус-строки). Ничего не делает, если узел уже активен.
pub(crate) fn switch_active(to: &str, reason: &str) {
    let from = active_host();
    if from == to {
        return;
    }
    set_active_node(to.to_string());
    if active_host() != to {
        return; // неизвестный узел — set_active_node отклонил
    }
    crate::syslog::log("net", format!("узел: {} → {} ({})", short_host(&from), short_host(to), reason));
    if let Some(app) = APP_HANDLE.get() {
        use tauri::Emitter;
        let _ = app.emit(
            "sth-node-switched",
            serde_json::json!({ "from": from, "to": to, "reason": reason, "p2p": is_p2p(to) }),
        );
    }
}

/// REST base URL of the active node (e.g. `https://node0.smartholdem.io/api`).
/// For a P2P node this is the `api://<nodeId>/<provider>/api` form so that the
/// netfory provider proxies the SmartHoldem REST path to the upstream node.
pub fn api_base() -> String {
    let host = active_host();
    if is_p2p(&host) {
        format!("{}/api", host.trim_end_matches('/'))
    } else {
        format!("{}/api", origin(&host))
    }
}

/// SmartHoldem exchange / liquidity-pool gateway (XBTS). The active endpoint is
/// resolved at call time: clearnet `EXCHANGE_API` first, then `EXCHANGE_API_P2P`
/// (comma-separated `api://` nodes) as fallback — see `resolve_exchange`.
const EXCHANGE_API_DEFAULT: &str = "https://exchange.smartholdem.io";
const EXCHANGE_P2P_DEFAULT: &str =
    "api://a21963bef9e2d6a1a8109ad310e973c956f7d7402713cf55f414850f5894464a/sth-exchange";
/// Address version byte (network pubKeyHash 63).
const VERSION_BYTE: u8 = 0x3f;
/// 1 STH in smarthoshi.
pub(crate) const SMARTHOSHI: u64 = 100_000_000;
/// STH ⇄ USDT reference rate (used for the fiat estimate).
pub const STH_USDT_RATE: f64 = 0.00135;

const NET_TIMEOUT: Duration = Duration::from_secs(12);
/// Short timeout for HOT background polls (balance / subscribers) — fail fast so
/// the UI stays responsive on a weak link. SmartHoldem block time is ~8 s, so
/// these polls run on a slow cadence and must not stall.
const HOT_TIMEOUT: Duration = Duration::from_secs(4);

/// Drive a P2P (iroh) future to completion from a synchronous/blocking thread.
/// The node REST client runs inside `spawn_blocking`, so we schedule the future
/// on the Tauri async runtime (which owns the iroh reactor) and block on a
/// channel for the result — avoiding a nested-runtime panic.
#[cfg(feature = "p2p")]
fn run_async<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    tauri::async_runtime::spawn(async move {
        let _ = tx.send(fut.await);
    });
    rx.recv().expect("p2p task channel closed")
}

/// Perform a node REST request over the P2P (`api://`) transport to `base`
/// (`api://<nodeId>/<provider>`). Returns the node's JSON body text (which
/// still carries the upstream `data`/`meta` keys).
#[cfg(feature = "p2p")]
fn p2p_node_request_to(base: &str, method: &str, path: &str, body: &str, timeout: Duration) -> Result<String, String> {
    let app = APP_HANDLE.get().ok_or("P2P-транспорт ещё не инициализирован")?.clone();
    let url = format!("{}/api{}", base.trim_end_matches('/'), path);
    let method = method.to_string();
    let body = body.to_string();
    // Hard overall deadline so an unreachable provider can't stall the wallet:
    // at least P2P_MIN_DEADLINE (hole-punch/relay setup), or the caller's budget.
    let deadline = timeout.max(P2P_MIN_DEADLINE);
    let res = run_async(async move {
        match tokio::time::timeout(deadline, crate::p2p::api_request(app, url, method, body, Default::default())).await {
            Ok(r) => r,
            Err(_) => Err("таймаут P2P-запроса".to_string()),
        }
    })?;
    if res.status == 404 {
        return Err("node error: status 404".to_string());
    }
    if res.status >= 400 {
        return Err(format!("node error: status {}", res.status));
    }
    Ok(res.body)
}

#[cfg(not(feature = "p2p"))]
fn p2p_node_request_to(_base: &str, _method: &str, _path: &str, _body: &str, _timeout: Duration) -> Result<String, String> {
    Err("Протокол api:// доступен только в десктоп-сборке (P2P).".to_string())
}

/// Минимальный общий дедлайн P2P-запроса до авто-fallback'а.
/// P2P round-trips through a netfory provider were measured at ~4–9 s, so 16 s
/// gives healthy headroom and avoids premature clearnet fallback on slow hops.
#[cfg(feature = "p2p")]
const P2P_MIN_DEADLINE: Duration = Duration::from_secs(16);

/// A P2P error worth falling back from = a transport/provider failure (timeout,
/// connection lost, bad response) or 429 (rate limit — switch node, SHIP-44) or
/// 503 (wallet_api disabled on that node). A clean upstream HTTP status (e.g.
/// 404) is a definitive answer and must NOT trigger a replay.
fn should_fallback(err: &str) -> bool {
    !err.contains("status ") || err.contains("status 429") || err.contains("status 503")
}

/// Порядок iroh-узлов для запроса: активный (если iroh) первым, затем остальной пул по кругу.
fn iroh_order() -> Vec<String> {
    let active = active_host();
    let pool = iroh_nodes();
    let mut order: Vec<String> = Vec::with_capacity(pool.len() + 1);
    if is_p2p(&active) {
        order.push(active.clone());
    }
    let start = pool.iter().position(|n| *n == active).map(|i| i + 1).unwrap_or(0);
    for n in pool.iter().cycle().skip(start).take(pool.len()) {
        if !order.contains(n) {
            order.push(n.clone());
        }
    }
    order
}

/// Хеджирование: если узел не ответил за это время — параллельно стартуем следующий,
/// не дожидаясь его 16-секундного дедлайна. Первый успешный ответ побеждает.
const P2P_HEDGE_DELAY: Duration = Duration::from_secs(4);

/// Запрос по группе iroh-узлов с параллельным fallback: первый узел стартует сразу,
/// каждые `P2P_HEDGE_DELAY` без ответа подключается следующий; транспортные сбои /
/// 429 / 503 тоже сразу запускают следующий. Побеждает первый успешный ответ (или
/// окончательный 4xx); успешный не-активный узел становится активным. Err — когда все.
fn p2p_group(method: &str, path: &str, body: &str, timeout: Duration) -> Result<String, String> {
    let order = iroh_order();
    if order.is_empty() {
        return Err("нет iroh-узлов".to_string());
    }
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<String, String>)>();
    let spawn = |i: usize| {
        let tx = tx.clone();
        let (host, m, p, b) = (order[i].clone(), method.to_string(), path.to_string(), body.to_string());
        std::thread::spawn(move || {
            let r = p2p_node_request_to(&host, &m, &p, &b, timeout);
            let _ = tx.send((i, r));
        });
    };
    spawn(0);
    let (mut started, mut finished) = (1usize, 0usize);
    let mut last = String::new();
    loop {
        match rx.recv_timeout(P2P_HEDGE_DELAY) {
            Ok((i, Ok(t))) => {
                if i > 0 && is_p2p(&active_host()) {
                    switch_active(&order[i], "активный P2P-узел не ответил вовремя");
                }
                return Ok(t);
            }
            Ok((_, Err(e))) if !should_fallback(&e) => return Err(e),
            Ok((i, Err(e))) => {
                crate::syslog::log("net", format!("iroh-узел {} недоступен: {}", short_host(&order[i]), e));
                last = e;
                finished += 1;
                if started < order.len() {
                    spawn(started);
                    started += 1;
                } else if finished >= started {
                    return Err(last);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if started < order.len() {
                    crate::syslog::log(
                        "net",
                        format!("iroh-узел {} молчит {} с — параллельно пробуем {}", short_host(&order[started - 1]), P2P_HEDGE_DELAY.as_secs(), short_host(&order[started])),
                    );
                    spawn(started);
                    started += 1;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err(last),
        }
    }
}

/// Короткое имя узла для логов: `api://1692c5f3…/sth-core` или hostname.
pub(crate) fn short_host(host: &str) -> String {
    match host.strip_prefix("api://") {
        Some(rest) => {
            let (id, name) = rest.split_once('/').unwrap_or((rest, ""));
            format!("api://{}…/{}", &id[..id.len().min(8)], name)
        }
        None => host.to_string(),
    }
}

/// Emit a one-shot UI marker that a wallet/blockchain call fell back from a
/// (stalled/unreachable) `api://` node to the clearnet HTTPS pool.
fn emit_fallback(failed_host: &str, method: &str, err: &str) {
    if let Some(app) = APP_HANDLE.get() {
        use tauri::Emitter;
        let _ = app.emit(
            "node-p2p-fallback",
            serde_json::json!({ "host": failed_host, "method": method, "error": err }),
        );
    }
}

/// GET `path` over the clearnet HTTPS pool, trying each host in `order` with
/// failover. On a successful failover (i > 0) the host becomes active.
fn https_get(order: &[String], path: &str, timeout: Duration) -> Result<String, String> {
    let mut last = "no nodes".to_string();
    for (i, host) in order.iter().enumerate() {
        let url = format!("{}/api{}", origin(host), path);
        match ureq::get(&url).timeout(timeout).call() {
            Ok(r) => {
                if i > 0 && !is_p2p(&active_host()) {
                    switch_active(host, "активный clearnet-узел не ответил");
                }
                return r.into_string().map_err(|e| e.to_string());
            }
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                last = format!("status {} {}", code, body);
                // 4xx (кроме 429) — окончательный ответ узла (например 404 «кошелёк
                // ещё не в цепочке»), а не сбой транспорта: НЕ идём на бэкап. Раньше
                // 404 с активного узла затирался ошибкой бэкапа (DNS-сбой node0 у
                // пользователя) — новый кошелёк не мог подписать первую tx.
                if (400..500).contains(&code) && code != 429 {
                    return Err(last);
                }
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

pub(crate) fn node_get(path: &str, timeout: Duration) -> Result<String, String> {
    let active = active_host();
    // Пользовательский clearnet-узел: сначала он, при сбое — группа iroh.
    if !is_p2p(&active) {
        match https_get(&[active.clone()], path, timeout) {
            Ok(t) => return Ok(t),
            Err(e) if should_fallback(&e) && iroh_available() => {
                set_fallback(true);
                emit_fallback(&active, "GET", &e);
                return p2p_group("GET", path, "", timeout);
            }
            Err(e) => return Err(e),
        }
    }
    match p2p_group("GET", path, "", timeout) {
        Ok(t) => {
            set_fallback(false);
            Ok(t)
        }
        Err(e) => {
            set_fallback(true);
            // все P2P-узлы недоступны → встроенный clearnet-пул как резерв; ответивший
            // узел становится активным (фоновый селектор вернёт P2P, когда он оживёт)
            clearnet_backup_get(path, timeout, e)
        }
    }
}

/// Резерв: GET по встроенному clearnet-пулу, когда недоступна вся группа iroh.
fn clearnet_backup_get(path: &str, timeout: Duration, p2p_err: String) -> Result<String, String> {
    let pool = nodes();
    if pool.is_empty() {
        return Err(p2p_err);
    }
    for host in pool.iter() {
        match https_get(std::slice::from_ref(host), path, timeout) {
            Ok(t) => {
                switch_active(host, "P2P-узлы недоступны → clearnet-резерв");
                return Ok(t);
            }
            Err(e) if should_fallback(&e) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(format!("{p2p_err}; clearnet-резерв тоже недоступен"))
}

/// POST JSON на один clearnet-узел (пользовательский localhost:4003 и т.п.).
fn https_post(host: &str, path: &str, body: &str, timeout: Duration) -> Result<String, String> {
    let url = format!("{}/api{}", origin(host), path);
    match ureq::post(&url)
        .timeout(timeout)
        .set("Content-Type", "application/json")
        .send_string(body)
    {
        Ok(r) => r.into_string().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(code, r)) => {
            let txt = r.into_string().unwrap_or_default();
            Err(format!("status {} {}", code, txt))
        }
        Err(e) => Err(e.to_string()),
    }
}

fn node_post(path: &str, body: &str, timeout: Duration) -> Result<String, String> {
    let active = active_host();
    if !is_p2p(&active) {
        match https_post(&active, path, body, timeout) {
            Ok(t) => return Ok(t),
            Err(e) if should_fallback(&e) && iroh_available() => {
                set_fallback(true);
                emit_fallback(&active, "POST", &e);
                return p2p_group("POST", path, body, timeout);
            }
            Err(e) => return Err(e),
        }
    }
    match p2p_group("POST", path, body, timeout) {
        Ok(t) => {
            set_fallback(false);
            Ok(t)
        }
        Err(e) => {
            set_fallback(true);
            // резерв: первый отвечающий clearnet-узел пула (POST не ретраим по всем —
            // tx не должна уйти дважды)
            for host in nodes().iter() {
                match https_post(host, path, body, timeout) {
                    Ok(t) => {
                        switch_active(host, "P2P-узлы недоступны → clearnet-резерв");
                        return Ok(t);
                    }
                    Err(e2) if should_fallback(&e2) => continue,
                    Err(e2) => return Err(e2),
                }
            }
            Err(e)
        }
    }
}

/// Live status of one pooled node (for the UI node-pool list).
#[derive(Debug, serde::Serialize, Clone)]
pub(crate) struct NodePing {
    host: String,
    #[serde(rename = "latencyMs")]
    latency_ms: Option<u64>,
    online: bool,
    height: Option<u64>,
    active: bool,
    /// true — iroh-узел (`api://`), false — legacy HTTPS.
    p2p: bool,
    /// Причина недоступности (коротко) — чтобы в UI было видно, где рвётся: discovery / реле / ALPN / таймаут.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Короткая причина для UI (без хвостов iroh-ошибок).
fn short_err(e: &str) -> String {
    let one = e.lines().next().unwrap_or(e).trim();
    let mut t: String = one.chars().take(90).collect();
    if one.chars().count() > 90 {
        t.push('…');
    }
    t
}

fn ping_one(host: &str) -> NodePing {
    if is_p2p(host) {
        return ping_iroh(host);
    }
    let url = format!("{}/api/blockchain", origin(host));
    let start = std::time::Instant::now();
    match ureq::get(&url).timeout(HOT_TIMEOUT).call() {
        Ok(resp) => {
            let ms = start.elapsed().as_millis() as u64;
            let height = resp
                .into_string()
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|v| v["data"]["block"]["height"].as_u64());
            NodePing { host: host.to_string(), latency_ms: Some(ms), online: true, height, active: false, p2p: false, error: None }
        }
        Err(e) => NodePing { host: host.to_string(), latency_ms: None, online: false, height: None, active: false, p2p: false, error: Some(short_err(&e.to_string())) },
    }
}

/// Пинг iroh-узла: `GET /api/blockchain` по `netfory/api/1` (первый connect
/// включает hole-punch/relay, поэтому латентность первого пинга завышена;
/// соединение остаётся в пуле и следующие запросы идут быстро).
fn ping_iroh(host: &str) -> NodePing {
    let start = std::time::Instant::now();
    match p2p_node_request_to(host, "GET", "/blockchain", "", HOT_TIMEOUT) {
        Ok(txt) => {
            let ms = start.elapsed().as_millis() as u64;
            let height = serde_json::from_str::<Value>(&txt).ok().and_then(|v| v["data"]["block"]["height"].as_u64());
            NodePing { host: host.to_string(), latency_ms: Some(ms), online: true, height, active: false, p2p: true, error: None }
        }
        Err(e) => {
            crate::syslog::log("net", format!("узел {} недоступен: {e}", short_host(host)));
            NodePing { host: host.to_string(), latency_ms: None, online: false, height: None, active: false, p2p: true, error: Some(short_err(&e)) }
        }
    }
}

impl NodePing {
    /// Высота цепи по ответу узла (None — офлайн/нет данных).
    pub(crate) fn height(&self) -> Option<u64> {
        self.height
    }
}

/// Ping the whole pool (iroh + HTTPS) in parallel; returns the list sorted:
/// online iroh by latency, then online HTTPS by latency, then offline. The
/// active node is flagged. Does NOT change the active node.
pub(crate) fn ping_all() -> Vec<NodePing> {
    let handles: Vec<_> = pool_all()
        .into_iter()
        .map(|h| std::thread::spawn(move || ping_one(&h)))
        .collect();
    let active = active_host();
    let mut out: Vec<NodePing> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
    for p in out.iter_mut() {
        p.active = p.host == active;
    }
    out.sort_by_key(|p| (!p.online, !p.p2p, p.latency_ms.unwrap_or(u64::MAX)));
    if let Some(p) = out.iter().find(|p| p.host == active) {
        *ACTIVE_LATENCY.lock().unwrap() = p.latency_ms;
    }
    out
}

/// Максимальное отставание узла (блоков) от лучшего в пуле, при котором он
/// ещё считается синхронным для автовыбора.
const MAX_HEIGHT_LAG: u64 = 5;

/// Ping the pool and connect to the best node: iroh first (lowest latency among
/// synced iroh nodes), HTTPS only when no iroh node is online. Returns the
/// sorted list with the (new) active node flagged.
pub(crate) fn auto_select_best() -> Vec<NodePing> {
    let mut pings = ping_all();
    let best_h = pings.iter().filter_map(|p| p.height).max().unwrap_or(0);
    let synced = |p: &NodePing| p.online && p.height.map(|h| h + MAX_HEIGHT_LAG >= best_h).unwrap_or(true);
    let pick = pings
        .iter()
        .find(|p| p.p2p && synced(p))
        .or_else(|| pings.iter().find(|p| synced(p)))
        .or_else(|| pings.iter().find(|p| p.online));
    if let Some(best) = pick {
        let cur = active_host();
        let cur_down = pings.iter().find(|p| p.host == cur).map_or(true, |p| !p.online);
        let reason = if cur_down {
            "активный узел не отвечает"
        } else if best.p2p && !is_p2p(&cur) {
            "P2P-узел снова доступен"
        } else {
            "найден более быстрый узел"
        };
        switch_active(&best.host, reason);
    }
    let active = active_host();
    for p in pings.iter_mut() {
        p.active = p.host == active;
    }
    pings
}

/// Background node selector: pick the best node at startup, then re-evaluate on
/// a slow cadence so an overloaded node is replaced automatically.
pub(crate) fn spawn_node_selector() {
    std::thread::spawn(|| {
        // iroh-эндпоинт поднимается в setup() чуть позже — даём ему стартовать,
        // иначе первый скан увидит только HTTPS.
        std::thread::sleep(Duration::from_secs(3));
        // P2P-узлы «проявляются» со временем (DHT/discovery прогревается): проверки
        // через 1 → 5 → 15 → 30 → 50 мин после старта, далее каждые 50 мин.
        const SCHEDULE_SECS: &[u64] = &[60, 240, 600, 900, 1200];
        let mut step = 0usize;
        loop {
            // Respect a manual selection: only auto-rotate while a default-pool
            // node is active (a user-pinned custom node must persist).
            if crate::netwatch::is_online() && pool_all().iter().any(|n| *n == active_host()) {
                auto_select_best();
            }
            let wait = SCHEDULE_SECS.get(step).copied().unwrap_or(3000);
            step += 1;
            std::thread::sleep(Duration::from_secs(wait));
        }
    });
}

/// Ping the pool (UI "rescan"). Активный узел пула не ответил (стал «серым») →
/// сразу переключаемся на лучший онлайн (P2P приоритетнее clearnet).
#[tauri::command]
pub(crate) async fn refresh_node_pool() -> Vec<NodePing> {
    tauri::async_runtime::spawn_blocking(|| {
        let pings = ping_all();
        let cur = active_host();
        let cur_down = pool_all().iter().any(|n| *n == cur) && pings.iter().any(|p| p.host == cur && !p.online);
        if cur_down && pings.iter().any(|p| p.online) {
            return auto_select_best();
        }
        pings
    })
    .await
    .unwrap_or_default()
}

/// Ping the pool and auto-connect to the fastest node (UI "auto").
#[tauri::command]
pub(crate) async fn auto_select_node() -> Vec<NodePing> {
    tauri::async_runtime::spawn_blocking(auto_select_best).await.unwrap_or_default()
}

/// Manually pin the active node.
#[tauri::command]
pub(crate) fn select_node(host: String) -> Result<String, String> {
    let known = is_p2p(&host)
        || pool_all().iter().any(|n| n == &host)
        || custom_nodes().lock().unwrap().iter().any(|n| n == &host);
    if known {
        set_active_node(host.clone());
        Ok(host)
    } else {
        Err("Узел не входит в пул".to_string())
    }
}

// ---------------------------------------------------------------------------
//  Custom (user-added) nodes — clearnet hostnames or `api://` P2P addresses.
//  Persisted in sled so they survive restarts.
// ---------------------------------------------------------------------------

fn custom_nodes() -> &'static Mutex<Vec<String>> {
    static C: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    C.get_or_init(|| {
        let list = crate::DB
            .get(b"cfg:custom_nodes")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_slice::<Vec<String>>(&v).ok())
            .unwrap_or_default();
        Mutex::new(list)
    })
}

fn persist_custom_nodes(list: &[String]) {
    if let Ok(bytes) = serde_json::to_vec(list) {
        let _ = crate::DB.insert(b"cfg:custom_nodes", bytes);
        let _ = crate::DB.flush();
    }
}

/// Validate + canonicalize a user-entered node address. Clearnet hosts are
/// stored as a bare hostname (`node6.smartholdem.io`); P2P addresses keep the
/// `api://<nodeId>/<provider>` form.
fn normalize_node(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("пустой адрес узла".to_string());
    }
    if let Some(rest) = s.strip_prefix("api://") {
        if !rest.trim_matches('/').contains('/') {
            return Err("формат: api://<nodeId>/<provider>".to_string());
        }
        return Ok(s.trim_end_matches('/').to_string());
    }
    let (scheme, rest) = if let Some(r) = s.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = s.strip_prefix("http://") {
        ("http", r)
    } else {
        ("https", s)
    };
    // host[:port] без пути; нижний регистр (hostname нечувствителен к регистру)
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or("").trim().to_ascii_lowercase();
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (h.to_string(), Some(p.to_string())),
        _ => (hostport.clone(), None),
    };
    if host.is_empty() || host.contains('@') || host.contains(' ') {
        return Err("некорректный адрес узла".to_string());
    }
    if let Some(p) = &port {
        if p.parse::<u16>().map(|v| v == 0).unwrap_or(true) {
            return Err("некорректный порт узла".to_string());
        }
    }
    // Локальный узел SmartHoldem (http://localhost:4003, http://127.0.0.1:4003,
    // http://192.168.1.10:4003) — допускаем http и хосты без точки (localhost).
    let is_local = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok();
    if !is_local && !host.contains('.') {
        return Err("некорректный адрес узла".to_string());
    }
    // Каноническая форма: голый hostname для https без порта (совместимо со
    // старыми записями), иначе полный origin со схемой и портом.
    if scheme == "https" && port.is_none() {
        Ok(host)
    } else {
        Ok(match port {
            Some(p) => format!("{}://{}:{}", scheme, host, p),
            None => format!("{}://{}", scheme, host),
        })
    }
}

/// Origin узла для REST-вызовов: `https://<host>` для голого hostname,
/// либо сохранённый origin со схемой (`http://localhost:4003`).
pub(crate) fn origin(host: &str) -> String {
    let h = host.trim().trim_end_matches('/');
    let o = if h.starts_with("http://") || h.starts_with("https://") {
        h.to_string()
    } else {
        format!("https://{}", h)
    };
    // `localhost` резолвится в ::1 и 127.0.0.1; узел слушает только IPv4, и на
    // Windows отказ по IPv6 стоит ~2×1с ретраев SYN — отсюда «пинг 2000мс» у
    // локального узла. Ходим сразу по 127.0.0.1 (в UI имя остаётся как ввёл пользователь).
    o.replacen("://localhost", "://127.0.0.1", 1)
}

/// User-added node addresses (persisted).
#[tauri::command]
pub(crate) fn list_custom_nodes() -> Vec<String> {
    custom_nodes().lock().unwrap().clone()
}

/// Add a custom node (clearnet or `api://`). Returns the updated custom list.
#[tauri::command]
pub(crate) fn add_custom_node(host: String) -> Result<Vec<String>, String> {
    let norm = normalize_node(&host)?;
    if pool_all().iter().any(|n| n == &norm) {
        return Err("узел уже в стандартном пуле".to_string());
    }
    let mut list = custom_nodes().lock().unwrap();
    if !list.contains(&norm) {
        list.push(norm);
        persist_custom_nodes(&list);
    }
    Ok(list.clone())
}

/// Remove a custom node. If it was active, fall back to the default pool.
#[tauri::command]
pub(crate) fn remove_custom_node(host: String) -> Vec<String> {
    let mut list = custom_nodes().lock().unwrap();
    list.retain(|n| n != &host);
    persist_custom_nodes(&list);
    drop(list);
    if active_host() == host {
        *ACTIVE_NODE.lock().unwrap() = None; // назад к пулу по умолчанию (iroh → HTTPS)
    }
    custom_nodes().lock().unwrap().clone()
}

/// Result of a custom-node connectivity check.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CustomPing {
    host: String,
    online: bool,
    latency_ms: Option<u64>,
    height: Option<u64>,
    /// For `api://` nodes: provider's Ed25519 signature verified. Clearnet → true.
    verified: bool,
    p2p: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn ping_https(host: String) -> CustomPing {
    let h = host.clone();
    let (online, latency_ms, height) = tauri::async_runtime::spawn_blocking(move || {
        let url = format!("{}/api/blockchain", origin(&h));
        let start = std::time::Instant::now();
        match ureq::get(&url).timeout(HOT_TIMEOUT).call() {
            Ok(resp) => {
                let ms = start.elapsed().as_millis() as u64;
                let height = resp
                    .into_string()
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .and_then(|v| v["data"]["block"]["height"].as_u64());
                (true, Some(ms), height)
            }
            Err(_) => (false, None, None),
        }
    })
    .await
    .unwrap_or((false, None, None));
    CustomPing { host, online, latency_ms, height, verified: online, p2p: false, error: None }
}

#[cfg(feature = "p2p")]
async fn ping_p2p(app: tauri::AppHandle, host: String) -> CustomPing {
    let url = format!("{}/api/blockchain", host.trim_end_matches('/'));
    let start = std::time::Instant::now();
    match crate::p2p::api_request(app, url, "GET".to_string(), String::new(), Default::default()).await {
        Ok(res) => {
            let ms = start.elapsed().as_millis() as u64;
            let height = serde_json::from_str::<Value>(&res.body)
                .ok()
                .and_then(|v| v["data"]["block"]["height"].as_u64());
            let online = res.status > 0 && res.status < 400;
            CustomPing { host, online, latency_ms: Some(ms), height, verified: res.verified, p2p: true, error: None }
        }
        Err(e) => {
            crate::syslog::log("net", format!("проверка {} не удалась: {e}", short_host(&host)));
            CustomPing { host, online: false, latency_ms: None, height: None, verified: false, p2p: true, error: Some(short_err(&e)) }
        }
    }
}

#[cfg(not(feature = "p2p"))]
async fn ping_p2p(_app: tauri::AppHandle, host: String) -> CustomPing {
    CustomPing { host, online: false, latency_ms: None, height: None, verified: false, p2p: true, error: Some("P2P доступен только в десктоп-сборке".into()) }
}

/// Check connectivity to a custom node before pinning it. For `api://` nodes
/// this routes a real `/blockchain` query through the P2P mesh and reports
/// whether the provider's signature verified; clearnet nodes get an HTTPS ping.
#[tauri::command]
pub(crate) async fn ping_custom_node(app: tauri::AppHandle, host: String) -> CustomPing {
    if is_p2p(&host) {
        ping_p2p(app, host).await
    } else {
        ping_https(host).await
    }
}

/// Current active node hostname.
#[tauri::command]
pub(crate) fn current_node() -> String {
    active_host()
}

/// Active node + its last-known latency (cached — no network call). Drives the
/// connection-quality dot next to the globe icon.
#[derive(Debug, serde::Serialize)]
pub(crate) struct NodeStatus {
    host: String,
    #[serde(rename = "latencyMs")]
    latency_ms: Option<u64>,
    online: bool,
    /// "p2p" | "fallback" | "clearnet" — current effective transport.
    transport: String,
}

#[tauri::command]
pub(crate) fn node_status() -> NodeStatus {
    let latency_ms = *ACTIVE_LATENCY.lock().unwrap();
    let host = active_host();
    let transport = if !is_p2p(&host) {
        "clearnet"
    } else if *LAST_FALLBACK.lock().unwrap() {
        "fallback"
    } else {
        "p2p"
    }
    .to_string();
    NodeStatus {
        online: latency_ms.is_some() || is_p2p(&host),
        host,
        latency_ms,
        transport,
    }
}

fn p() -> BigUint {
    BigUint::parse_bytes(
        b"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F",
        16,
    )
    .unwrap()
}

fn n() -> BigUint {
    BigUint::parse_bytes(
        b"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141",
        16,
    )
    .unwrap()
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut r = [0u8; 32];
    r.copy_from_slice(&out);
    r
}

/// Left-pad a big-endian byte slice to exactly 32 bytes.
fn pad32(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let len = bytes.len().min(32);
    out[32 - len..].copy_from_slice(&bytes[bytes.len() - len..]);
    out
}

// ---------------------------------------------------------------------------
//  Key / address derivation (matches the wallet's SmartHoldem model)
// ---------------------------------------------------------------------------

/// (private key 32B, compressed public key hex, STH address) from arbitrary
/// key material (passphrase or hex private key). index 0 == the secret itself.
pub fn keypair_from_secret(secret: &str) -> Result<([u8; 32], String, String), String> {
    // `hex:<64 hex>` — готовый приватный ключ (BIP-44 суб-аккаунт), иначе passphrase.
    let priv_bytes: [u8; 32] = match secret.trim().strip_prefix("hex:") {
        Some(h) => hex::decode(h).map_err(|e| e.to_string())?.try_into().map_err(|_| "bad hex key length".to_string())?,
        None => sha256(secret.trim().as_bytes()),
    };
    let sk = SecretKey::from_slice(&priv_bytes).map_err(|e| e.to_string())?;
    let secp = Secp256k1::signing_only();
    let pk = PublicKey::from_secret_key(&secp, &sk);
    let comp = pk.serialize(); // 33-byte compressed

    let mut rip = ripemd::Ripemd160::new();
    rip.update(comp);
    let hash = rip.finalize();
    let mut payload = Vec::with_capacity(21);
    payload.push(VERSION_BYTE);
    payload.extend_from_slice(&hash);
    let address = bs58::encode(payload).with_check().into_string();

    Ok((priv_bytes, hex::encode(comp), address))
}

// ---------------------------------------------------------------------------
//  Legacy bip-schnorr signing (secp256k1, "is-square" R convention)
//
//    a = secret scalar
//    A = a*G                          (compressed, 33B)
//    k = sha256(a || m) mod n
//    R = k*G ; if y(R) is not a QR mod p -> k = n - k
//    r = x(R)                         (32B)
//    e = sha256(r || A || m) mod n
//    s = (k + e*a) mod n
//    sig = r(32) || s(32)             (64B)
// ---------------------------------------------------------------------------

fn sign_schnorr_legacy(priv32: &[u8; 32], pubkey_comp_hex: &str, msg: &[u8; 32]) -> Result<String, String> {
    let n = n();
    let p = p();
    let secp = Secp256k1::signing_only();

    // k = sha256(priv || msg) mod n
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(priv32);
    buf.extend_from_slice(msg);
    let k0 = BigUint::from_bytes_be(&sha256(&buf)) % &n;
    if k0 == BigUint::from(0u8) {
        return Err("schnorr: k=0".into());
    }
    let k0_bytes = pad32(&k0.to_bytes_be());

    // R = k0 * G  (use a secret key to perform the scalar multiplication)
    let k_sk = SecretKey::from_slice(&k0_bytes).map_err(|e| e.to_string())?;
    let r_point = PublicKey::from_secret_key(&secp, &k_sk).serialize_uncompressed();
    let r_x = &r_point[1..33];
    let r_y = &r_point[33..65];

    // is y(R) a quadratic residue mod p ?  ( y^((p-1)/2) == 1 )
    let y = BigUint::from_bytes_be(r_y);
    let exp = (&p - BigUint::from(1u8)) / BigUint::from(2u8);
    let is_square = y.modpow(&exp, &p) == BigUint::from(1u8);
    let k = if is_square { k0 } else { (&n - &k0) % &n };

    // e = sha256(r_x || A_compressed || msg) mod n
    let a_comp = hex::decode(pubkey_comp_hex).map_err(|e| e.to_string())?;
    let mut ebuf = Vec::with_capacity(32 + 33 + 32);
    ebuf.extend_from_slice(r_x);
    ebuf.extend_from_slice(&a_comp);
    ebuf.extend_from_slice(msg);
    let e = BigUint::from_bytes_be(&sha256(&ebuf)) % &n;

    // s = (k + e*a) mod n
    let a = BigUint::from_bytes_be(priv32);
    let s = (&k + (&e * &a)) % &n;
    let s_bytes = pad32(&s.to_bytes_be());

    let mut sig = Vec::with_capacity(64);
    sig.extend_from_slice(r_x);
    sig.extend_from_slice(&s_bytes);
    Ok(hex::encode(sig))
}

/// Проверка legacy bip-schnorr подписи (обратная операция к
/// `sign_schnorr_legacy`): R' = s*G − e*A; подпись валидна, если y(R') —
/// квадратичный вычет mod p и x(R') == r. Используется DevHub'ом для
/// верификации подписанных индексов репозиториев чужих авторов.
pub(crate) fn verify_schnorr_legacy(pubkey_comp_hex: &str, msg: &[u8; 32], sig_hex: &str) -> bool {
    let inner = || -> Option<bool> {
        let sig = hex::decode(sig_hex).ok()?;
        if sig.len() != 64 {
            return Some(false);
        }
        let n = n();
        let p = p();
        let r_x = &sig[..32];
        let s_bytes: [u8; 32] = sig[32..].try_into().ok()?;
        let s_int = BigUint::from_bytes_be(&s_bytes);
        if s_int >= n || s_int == BigUint::from(0u8) {
            return Some(false);
        }
        if BigUint::from_bytes_be(r_x) >= p {
            return Some(false);
        }
        // e = sha256(r || A || m) mod n
        let a_comp = hex::decode(pubkey_comp_hex).ok()?;
        let mut ebuf = Vec::with_capacity(32 + 33 + 32);
        ebuf.extend_from_slice(r_x);
        ebuf.extend_from_slice(&a_comp);
        ebuf.extend_from_slice(msg);
        let e = BigUint::from_bytes_be(&sha256(&ebuf)) % &n;
        let secp = Secp256k1::new();
        let a_pk = PublicKey::from_slice(&a_comp).ok()?;
        // R' = s*G + (n − e)*A
        let s_sk = SecretKey::from_slice(&s_bytes).ok()?;
        let s_g = PublicKey::from_secret_key(&secp, &s_sk);
        let r_point = if e == BigUint::from(0u8) {
            s_g
        } else {
            let neg_e = (&n - &e) % &n;
            let neg_bytes: [u8; 32] = pad32(&neg_e.to_bytes_be()).try_into().ok()?;
            let scalar = secp256k1::Scalar::from_be_bytes(neg_bytes).ok()?;
            let e_a = a_pk.mul_tweak(&secp, &scalar).ok()?;
            s_g.combine(&e_a).ok()?
        };
        let ser = r_point.serialize_uncompressed();
        let ry = BigUint::from_bytes_be(&ser[33..65]);
        // y(R') должен быть квадратичным вычетом mod p ("is-square" конвенция).
        let exp = (&p - BigUint::from(1u8)) / BigUint::from(2u8);
        if ry.modpow(&exp, &p) != BigUint::from(1u8) {
            return Some(false);
        }
        Some(&ser[1..33] == r_x)
    };
    inner().unwrap_or(false)
}

// ---------------------------------------------------------------------------
//  v2 transfer serialization (AIP-11 wire format)
// ---------------------------------------------------------------------------

pub(crate) fn parse_sth(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(0);
    }
    let mut parts = s.splitn(2, '.');
    let whole_raw = parts.next().unwrap_or("0");
    let frac_raw = parts.next().unwrap_or("");
    // Keep ONLY ASCII digits. This makes the parser char-boundary-safe (a stray
    // non-ASCII char in the amount/fee string used to panic `frac.truncate(8)`
    // with "is_char_boundary") and rejects any malformed input cleanly.
    let whole: String = whole_raw.chars().filter(|c| c.is_ascii_digit()).collect();
    let whole_v: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| format!("bad amount: {}", s))?
    };
    // Up to 8 fractional digits (smartoshi precision), right-padded with zeros.
    let mut frac: String = frac_raw
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(8)
        .collect();
    while frac.len() < 8 {
        frac.push('0');
    }
    let frac_v: u64 = frac.parse().map_err(|_| format!("bad amount: {}", s))?;
    whole_v
        .checked_mul(SMARTHOSHI)
        .and_then(|w| w.checked_add(frac_v))
        .ok_or_else(|| "amount overflow".to_string())
}

fn recipient_buffer(addr: &str) -> Result<Vec<u8>, String> {
    let decoded = bs58::decode(addr)
        .with_check(None)
        .into_vec()
        .map_err(|_| format!("invalid STH address: {}", addr))?;
    if decoded.len() != 21 {
        return Err(format!("invalid STH address length: {}", addr));
    }
    Ok(decoded)
}

/// Build the canonical (unsigned) wire bytes for a v2 transfer.
fn serialize_transfer(
    pubkey_comp_hex: &str,
    nonce: u64,
    fee: u64,
    amount: u64,
    vendor: &str,
    recipient: &str,
) -> Result<Vec<u8>, String> {
    let mut b = Vec::with_capacity(128);
    b.push(0xff); // magic
    b.push(0x02); // version 2
    b.push(VERSION_BYTE); // network 63
    b.extend_from_slice(&1u32.to_le_bytes()); // typeGroup = Core
    b.extend_from_slice(&0u16.to_le_bytes()); // type = transfer
    b.extend_from_slice(&nonce.to_le_bytes()); // nonce
    b.extend_from_slice(&hex::decode(pubkey_comp_hex).map_err(|e| e.to_string())?); // senderPublicKey (33B)
    b.extend_from_slice(&fee.to_le_bytes()); // fee

    // vendorField
    let vf = vendor.as_bytes();
    if vf.is_empty() {
        b.push(0x00);
    } else {
        if vf.len() > 255 {
            return Err("vendorField exceeds 255 bytes".into());
        }
        b.push(vf.len() as u8);
        b.extend_from_slice(vf);
    }

    // transfer asset
    b.extend_from_slice(&amount.to_le_bytes()); // amount
    b.extend_from_slice(&0u32.to_le_bytes()); // expiration = 0
    b.extend_from_slice(&recipient_buffer(recipient)?); // recipientId (21B)
    Ok(b)
}

#[derive(Clone)]
pub struct SignedTx {
    pub id: String,
    pub serialized: String,
    pub data: Value,
}

/// Canonical (unsigned) wire bytes for a v2 IPFS (Type 5) transaction. Mirrors
/// `@smartholdem/crypto` (ARK Core v2/v3): common header + EMPTY vendorField +
/// asset. IMPORTANT: IPFS transactions have `hasVendorField() == false`, so the
/// node forces the vendorField byte to 0x00 when it re-serializes for signature
/// verification — we MUST match that (a non-empty vendorField breaks the sig).
/// The asset is the RAW base58-decoded CIDv0 multihash (34 bytes: 0x12,0x20 +
/// 32-byte sha256), appended WITHOUT a length prefix.
fn serialize_ipfs(
    pubkey_comp_hex: &str,
    nonce: u64,
    fee: u64,
    cid: &str,
) -> Result<Vec<u8>, String> {
    let mut b = Vec::with_capacity(96);
    b.push(0xff); // magic
    b.push(0x02); // version 2
    b.push(VERSION_BYTE); // network 63
    b.extend_from_slice(&1u32.to_le_bytes()); // typeGroup = Core
    b.extend_from_slice(&5u16.to_le_bytes()); // type = IPFS
    b.extend_from_slice(&nonce.to_le_bytes());
    b.extend_from_slice(&hex::decode(pubkey_comp_hex).map_err(|e| e.to_string())?); // senderPublicKey
    b.extend_from_slice(&fee.to_le_bytes());

    // vendorField is NOT supported on IPFS → always 0x00.
    b.push(0x00);

    // IPFS asset: raw multihash bytes (base58btc-decoded CID), no length prefix.
    let mh = bs58::decode(cid).into_vec().map_err(|_| "invalid CID (base58)".to_string())?;
    if mh.len() < 2 || mh[0] != 0x12 || mh[1] as usize != mh.len() - 2 {
        return Err("CID is not a valid CIDv0 multihash".into());
    }
    b.extend_from_slice(&mh);
    Ok(b)
}

/// Build + sign a v2 IPFS (Type 5) transaction stamping `cid` on-chain (CID-only;
/// IPFS carries no vendorField). `fee` is in satoshi.
pub fn build_signed_ipfs(secret: &str, cid: &str, fee: u64, nonce: u64) -> Result<SignedTx, String> {
    let (priv32, pubkey_hex, _addr) = keypair_from_secret(secret)?;

    let unsigned = serialize_ipfs(&pubkey_hex, nonce, fee, cid)?;
    let msg = sha256(&unsigned);
    let signature = sign_schnorr_legacy(&priv32, &pubkey_hex, &msg)?;

    let mut full = unsigned.clone();
    full.extend_from_slice(&hex::decode(&signature).unwrap());
    let id = hex::encode(sha256(&full));

    let data = json!({
        "version": 2,
        "network": VERSION_BYTE,
        "typeGroup": 1,
        "type": 5,
        "nonce": nonce.to_string(),
        "senderPublicKey": pubkey_hex,
        "fee": fee.to_string(),
        "amount": "0",
        "asset": { "ipfs": cid },
        "signature": signature,
        "id": id,
    });

    Ok(SignedTx {
        id,
        serialized: hex::encode(full),
        data,
    })
}

/// SmartHoldem node dynamic-fee policy for IPFS (from /node/configuration →
/// transactionPool.dynamicFees): minFeeBroadcast (satoshi/byte) and the IPFS
/// addonBytes. Dynamic fees are enabled, so an IPFS stamp costs ~0.015 STH
/// instead of the 5 STH static fee.
const MIN_FEE_BROADCAST: u64 = 3000;
const IPFS_ADDON_BYTES: u64 = 250;

/// Minimum accepted dynamic fee (satoshi) for a CID-only IPFS tx. Serialized
/// size is deterministic: header(58) + empty vendorField(1) + asset(34) +
/// signature(64) = 157 bytes.
fn ipfs_dynamic_fee() -> u64 {
    let serialized = 157u64;
    (IPFS_ADDON_BYTES + serialized) * MIN_FEE_BROADCAST
}

/// Dynamic-fee addonBytes for a Type-0 transfer (SmartHoldem node policy).
const TRANSFER_ADDON_BYTES: u64 = 100;

/// Minimum accepted dynamic fee (satoshi) for a Type-0 transfer carrying a
/// `vendor_len`-byte vendorField. The serialized size is deterministic:
/// header(58) + vendorField(1+len) + asset(amount 8 + expiration 4 + recipient 21)
/// + signature(64) = 156 + len. A short generic transfer needs ~0.001 STH, but a
/// `xkey:` key-publish carries a ~100-byte vendorField, so its real minimum fee is
/// ~0.011 STH — undershooting it made the node reject the publish ("ERR_LOW_FEE").
pub(crate) fn transfer_dynamic_fee(vendor_len: usize) -> u64 {
    let serialized = 156u64 + vendor_len as u64;
    (TRANSFER_ADDON_BYTES + serialized) * MIN_FEE_BROADCAST
}

/// Exact STH string (8 dp) from smarthoshi — no rounding that could undershoot
/// a computed minimum fee (unlike the 4-dp display formatter).
pub(crate) fn smarthoshi_to_sth_exact(v: u64) -> String {
    format!("{}.{:08}", v / SMARTHOSHI, v % SMARTHOSHI)
}

/// One-shot: fetch nonce, build + sign + broadcast an IPFS (Type 5) CID stamp.
/// `fee_sth` empty → minimum dynamic fee (~0.0122 STH); otherwise the given STH.
pub fn publish_ipfs(secret: &str, cid: &str, fee_sth: &str) -> Result<BroadcastOutcome, String> {
    let (_priv, _pk, sender_addr) = keypair_from_secret(secret)?;
    let (balance, chain_nonce) = fetch_tx_nonce(&sender_addr)?;
    let next = next_nonce(&sender_addr, chain_nonce);
    let fee = if fee_sth.trim().is_empty() {
        ipfs_dynamic_fee()
    } else {
        parse_sth(fee_sth)?
    };
    ensure_balance(balance, 0, fee)?;
    let signed = build_signed_ipfs(secret, cid, fee, next)?;
    let outcome = broadcast(&signed.data)?;
    note_nonce_outcome(&sender_addr, next, &outcome);
    retry_with_expected_nonce(&sender_addr, next, outcome, |n| build_signed_ipfs(secret, cid, fee, n))
}

/// Scan the chain for IPFS (Type 5) transactions, newest-first. Returns
/// (cid, sender, vendorField, unix_ts) tuples for the file indexer.
pub fn fetch_ipfs_txs(limit: u32) -> Result<Vec<(String, String, String, u64)>, String> {
    let limit = limit.clamp(1, 100);
    let path = format!(
        "/transactions?type=5&typeGroup=1&limit={}&orderBy=timestamp:desc",
        limit
    );
    match node_get(&path, NET_TIMEOUT) {
        Ok(txt) => {
            let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
            let empty = vec![];
            let arr = v["data"].as_array().unwrap_or(&empty);
            let mut out = Vec::with_capacity(arr.len());
            for t in arr {
                let cid = t["asset"]["ipfs"].as_str().unwrap_or("").to_string();
                if cid.is_empty() {
                    continue;
                }
                let sender = t["sender"].as_str().unwrap_or("").to_string();
                let vendor = t["vendorField"].as_str().unwrap_or("").to_string();
                let ts = t["timestamp"]["unix"].as_u64().unwrap_or(0);
                out.push((cid, sender, vendor, ts));
            }
            Ok(out)
        }
        Err(e) if e.contains("404") => Ok(vec![]),
        Err(e) => Err(format!("node error: {}", e)),
    }
}

/// Build + sign a v2 transfer. Returns the txid, raw wire hex and the JSON data
/// object suitable for `POST /api/transactions`.
pub fn build_signed_transfer(
    secret: &str,
    recipient: &str,
    amount_sth: &str,
    fee_sth: &str,
    vendor: &str,
    nonce: u64,
) -> Result<SignedTx, String> {
    let (priv32, pubkey_hex, _addr) = keypair_from_secret(secret)?;
    let amount = parse_sth(amount_sth)?;
    let fee = if fee_sth.trim().is_empty() {
        SMARTHOSHI / 4 // default 0.25 STH
    } else {
        parse_sth(fee_sth)?
    };
    if amount == 0 {
        return Err("amount must be greater than zero".into());
    }

    let unsigned = serialize_transfer(&pubkey_hex, nonce, fee, amount, vendor, recipient)?;
    let msg = sha256(&unsigned);
    let signature = sign_schnorr_legacy(&priv32, &pubkey_hex, &msg)?;

    // full serialized = unsigned bytes + signature
    let mut full = unsigned.clone();
    full.extend_from_slice(&hex::decode(&signature).unwrap());
    let id = hex::encode(sha256(&full));

    let mut data = json!({
        "version": 2,
        "network": VERSION_BYTE,
        "typeGroup": 1,
        "type": 0,
        "nonce": nonce.to_string(),
        "senderPublicKey": pubkey_hex,
        "fee": fee.to_string(),
        "amount": amount.to_string(),
        "expiration": 0,
        "recipientId": recipient,
        "signature": signature,
        "id": id,
    });
    if !vendor.is_empty() {
        data["vendorField"] = Value::String(vendor.to_string());
    }

    Ok(SignedTx {
        id,
        serialized: hex::encode(full),
        data,
    })
}

// ---------------------------------------------------------------------------
//  REST API
// ---------------------------------------------------------------------------

/// (balance in smarthoshi, current nonce). Returns (0, 0) for an unseen wallet.
pub fn fetch_balance_nonce(address: &str) -> Result<(u64, u64), String> {
    match node_get(&format!("/wallets/{}", address), HOT_TIMEOUT) {
        Ok(txt) => {
            let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
            let d = &v["data"];
            let balance = d["balance"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
            let nonce = d["nonce"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
            Ok((balance, nonce))
        }
        // 404 == wallet not yet on chain (zero balance / nonce)
        Err(e) if e.contains("404") => Ok((0, 0)),
        Err(e) => Err(format!("node error: {}", e)),
    }
}

/// Fetch a wallet's on-chain secp256k1 public key (hex). Returns `Ok(None)` when
/// the wallet has never broadcast a transaction (no publicKey exists on chain
/// yet) — the caller uses this to block E2EE sharing to an inactive address.
pub fn fetch_wallet_pubkey(address: &str) -> Result<Option<String>, String> {
    match node_get(&format!("/wallets/{}", address), HOT_TIMEOUT) {
        Ok(txt) => {
            let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
            Ok(v["data"]["publicKey"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()))
        }
        // 404 == wallet not yet on chain → no public key available
        Err(e) if e.contains("404") => Ok(None),
        Err(e) => Err(format!("node error: {}", e)),
    }
}


#[derive(Clone, Serialize, Deserialize)]
pub struct HistItem {
    pub id: String,
    pub kind: String, // "in" | "out"
    pub amount: String,
    pub counterparty: String,
    pub timestamp: u64, // ms
    pub confirmed: bool,
    pub memo: String,
}

pub(crate) fn smarthoshi_to_sth(v: u64) -> String {
    format!("{:.4}", v as f64 / SMARTHOSHI as f64)
}

/// Баланс для UI без округления вверх: до 10 000 STH — точные 8 знаков,
/// выше — 4 знака с ОТСЕЧЕНИЕМ (не round). Раньше `{:.4}` округлял
/// 3858.71109 → 3858.7111, и «MAX» давал сумму больше реального баланса.
pub(crate) fn fmt_balance_sth(raw: u64) -> String {
    let whole = raw / SMARTHOSHI;
    let frac = raw % SMARTHOSHI;
    if whole < 10_000 {
        format!("{}.{:08}", whole, frac)
    } else {
        format!("{}.{:04}", whole, frac / 10_000)
    }
}

/// Официальный payout-адрес сети (multipayment vendorField "seed:reward").
pub const SEED_PAYER: &str = "SappsPqDQXCRvoi1V2Hs9xycQeT1ptJWV1";

/// Инкрементальный сбор входящих seed:reward мультиплатежей для `address`.
/// Нода НЕ отдаёт multipayment в /wallets/{addr}/transactions получателя
/// (это перевод SappsP→SappsP «самому себе»), поэтому опрашиваем /transactions
/// по senderId payout-адреса и складываем найденное в sled
/// (`seedpay:<addr>:<txid>`) — без пересканирования всего при каждом открытии.
pub fn sync_seed_rewards(address: &str) {
    if address.is_empty() {
        return;
    }
    // не чаще раза в 2 минуты — бережём ноду (и стабильность кошелька)
    let ts_key = format!("seedpay:ts:{}", address);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let last_sync: u64 = crate::DB
        .get(ts_key.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8_lossy(&v).parse().ok())
        .unwrap_or(0);
    if now.saturating_sub(last_sync) < 120 {
        return;
    }
    let _ = crate::DB.insert(ts_key.as_bytes(), now.to_string().into_bytes());
    let last_key = format!("seedpay:last:{}", address);
    let last_seen: String = crate::DB
        .get(last_key.as_bytes())
        .ok()
        .flatten()
        .map(|v| String::from_utf8_lossy(&v).to_string())
        .unwrap_or_default();
    let mut newest: Option<String> = None;
    'pages: for page in 1..=10u32 {
        let path = format!(
            "/transactions?type=6&typeGroup=1&senderId={}&page={}&limit=100&orderBy=timestamp:desc",
            SEED_PAYER, page
        );
        let body = match node_get(&path, NET_TIMEOUT) {
            Ok(b) => b,
            Err(_) => break,
        };
        let v: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => break,
        };
        let arr = match v["data"].as_array() {
            Some(a) if !a.is_empty() => a,
            _ => break,
        };
        for t in arr {
            let id = t["id"].as_str().unwrap_or("");
            if id.is_empty() {
                continue;
            }
            if newest.is_none() {
                newest = Some(id.to_string());
            }
            if id == last_seen {
                break 'pages;
            }
            if t["vendorField"].as_str().unwrap_or("") != "seed:reward" {
                continue;
            }
            let mine: u64 = t["asset"]["payments"]
                .as_array()
                .map(|ps| {
                    ps.iter()
                        .filter(|p| p["recipientId"].as_str() == Some(address))
                        .map(|p| {
                            p["amount"]
                                .as_str()
                                .and_then(|s| s.parse().ok())
                                .or_else(|| p["amount"].as_u64())
                                .unwrap_or(0)
                        })
                        .sum()
                })
                .unwrap_or(0);
            if mine == 0 {
                continue;
            }
            let ts = t["timestamp"]["unix"].as_u64().unwrap_or(0) * 1000;
            let rec = serde_json::json!({ "a": mine, "t": ts }).to_string();
            let _ = crate::DB
                .insert(format!("seedpay:{}:{}", address, id).as_bytes(), rec.into_bytes());
        }
        if arr.len() < 100 {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if let Some(n) = newest {
        let _ = crate::DB.insert(last_key.as_bytes(), n.into_bytes());
    }
}

/// Накопленные seed:reward выплаты адреса из локальной базы:
/// (txid, amount_smarthoshi, ts_ms).
pub fn seed_rewards_local(address: &str) -> Vec<(String, u64, u64)> {
    let prefix = format!("seedpay:{}:", address);
    let mut out = Vec::new();
    for kv in crate::DB.scan_prefix(prefix.as_bytes()).flatten() {
        let id = String::from_utf8_lossy(&kv.0)[prefix.len()..].to_string();
        if let Ok(v) = serde_json::from_slice::<Value>(&kv.1) {
            out.push((id, v["a"].as_u64().unwrap_or(0), v["t"].as_u64().unwrap_or(0)));
        }
    }
    out
}

pub fn fetch_history(address: &str, limit: u32) -> Result<Vec<HistItem>, String> {
    // SmartHoldem node rejects limit > 100 (HTTP 422 pagination cap), which
    // previously made callers passing 200 silently fall back to an empty/error
    // result (e.g. donor medals never showed). Clamp to the node's max page size.
    let limit = limit.clamp(1, 100);
    match node_get(&format!("/wallets/{}/transactions?limit={}", address, limit), HOT_TIMEOUT) {
        Ok(txt) => {
            let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
            let empty = vec![];
            let arr = v["data"].as_array().unwrap_or(&empty);
            let mut out = Vec::with_capacity(arr.len());
            for t in arr {
                out.push(parse_hist_item(t, address));
            }
            Ok(out)
        }
        Err(e) if e.contains("404") => Ok(vec![]),
        Err(e) => Err(format!("node error: {}", e)),
    }
}

/// Parse a single node transaction JSON object into a [`HistItem`] from the
/// perspective of `address` (decides in/out direction & counterparty).
pub(crate) fn parse_hist_item(t: &Value, address: &str) -> HistItem {
    let recipient = t["recipient"].as_str().unwrap_or("");
    let sender = t["sender"].as_str().unwrap_or("");
    let ts = t["timestamp"]["unix"].as_u64().unwrap_or(0) * 1000;
    // tx подтверждена после первого блока: у неё есть blockId (в локальном индексе
    // поле confirmations вырезано — оно устаревает, а blockId вечен)
    let confirmations = t["confirmations"]
        .as_u64()
        .unwrap_or_else(|| t["blockId"].as_str().map_or(0, |b| if b.is_empty() { 0 } else { 1 }));
    let id = t["id"].as_str().unwrap_or("").to_string();
    let memo = t["vendorField"].as_str().unwrap_or("").to_string();
    // Мультиплатёж (type 6): у транзакции amount = "0", реальные суммы лежат
    // в asset.payments[] — раньше кошелёк показывал такие операции как 0.0000.
    if let Some(payments) = t["asset"]["payments"].as_array() {
        let pay_amt = |p: &Value| -> u64 {
            p["amount"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| p["amount"].as_u64())
                .unwrap_or(0)
        };
        let (kind, amount_raw, counterparty) = if sender == address {
            // я отправитель: сумма всех выплат, контрагент — первый получатель
            let total: u64 = payments.iter().map(pay_amt).sum();
            let first = payments
                .first()
                .and_then(|p| p["recipientId"].as_str())
                .unwrap_or(recipient)
                .to_string();
            ("out", total, first)
        } else {
            // я получатель: только мои выплаты из списка
            let mine: u64 = payments
                .iter()
                .filter(|p| p["recipientId"].as_str() == Some(address))
                .map(|p| pay_amt(p))
                .sum();
            ("in", mine, sender.to_string())
        };
        return HistItem {
            id,
            kind: kind.into(),
            amount: fmt_balance_sth(amount_raw),
            counterparty,
            timestamp: ts,
            confirmed: confirmations > 0,
            memo,
        };
    }
    let is_in = recipient == address;
    let amount_raw: u64 = t["amount"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| t["amount"].as_u64())
        .unwrap_or(0);
    HistItem {
        id,
        kind: if is_in { "in".into() } else { "out".into() },
        amount: fmt_balance_sth(amount_raw),
        counterparty: if is_in { sender.to_string() } else { recipient.to_string() },
        timestamp: ts,
        confirmed: confirmations > 0,
        memo,
    }
}

/// Есть ли у `address` исходящее ончейн-согласие о непроверенных dApps:
/// перевод на `recipient` с vendorField = `vendor`, fee = `fee_sth` STH, amount = 1 STH.
pub fn has_unverified_accept_tx(
    address: &str,
    recipient: &str,
    vendor: &str,
    fee_sth: u64,
) -> Result<bool, String> {
    let path = format!(
        "/transactions?senderId={}&recipientId={}&page=1&limit=100&orderBy=timestamp:desc",
        address, recipient
    );
    let body = node_get(&path, NET_TIMEOUT)?;
    let v: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let want_fee = fee_sth.saturating_mul(SMARTHOSHI);
    if let Some(arr) = v["data"].as_array() {
        for t in arr {
            if t["vendorField"].as_str().unwrap_or("") != vendor {
                continue;
            }
            let fee: u64 = t["fee"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| t["fee"].as_u64())
                .unwrap_or(0);
            let amount: u64 = t["amount"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| t["amount"].as_u64())
                .unwrap_or(0);
            if fee == want_fee && amount == SMARTHOSHI {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Fetch ONE page (≤100 items) of a wallet's transactions, newest-first, plus
/// the node's reported total transaction count (drives incremental caching).
fn fetch_history_page(address: &str, page: u32) -> Result<(Vec<HistItem>, u64), String> {
    let path = format!(
        "/wallets/{}/transactions?page={}&limit=100&orderBy=timestamp:desc",
        address, page
    );
    match node_get(&path, NET_TIMEOUT) {
        Ok(txt) => {
            let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
            let total = v["meta"]["totalCount"].as_u64().unwrap_or(0);
            let empty = vec![];
            let arr = v["data"].as_array().unwrap_or(&empty);
            let out = arr.iter().map(|t| parse_hist_item(t, address)).collect();
            Ok((out, total))
        }
        Err(e) if e.contains("404") => Ok((vec![], 0)),
        Err(e) => Err(format!("node error: {}", e)),
    }
}

#[derive(Serialize, Deserialize, Default)]
struct TxCache {
    total_count: u64,
    items: Vec<HistItem>, // newest-first
}

/// Min seconds between live node hits for the SAME address (anti-spam guard so
/// the public node never gets hammered by rapid UI refreshes).
const HIST_THROTTLE_SECS: u64 = 30;
/// Hard cap on pages walked per refresh (100 tx/page) so a brand-new, very busy
/// address can't trigger a huge scan in one go.
const HIST_MAX_PAGES: u32 = 5;

/// Сброс троттла истории: следующий `fetch_history_cached` пойдёт в узел сразу
/// (вызывается при входящей tx и при принудительном refresh из UI).
pub fn bust_history_throttle(address: &str) {
    hist_throttle().lock().unwrap().remove(address);
}

fn hist_throttle() -> &'static Mutex<HashMap<String, std::time::Instant>> {
    static T: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Persistent, incremental transaction history for `address`.
///
/// Strategy (gentle on the public node):
///   1. Serve the sled-cached list directly if refreshed < HIST_THROTTLE_SECS ago.
///   2. Otherwise fetch page 1 + read `meta.totalCount`. If the total hasn't
///      grown since last cache, return the cache untouched (no extra requests).
///   3. If new transactions exist, walk only the pages we don't already have
///      (stopping at the first overlap with a cached tx id, or HIST_MAX_PAGES),
///      merge newest-first (dedupe by id) and persist back to sled.
///
/// The cache lives in the app's sled DB, so it survives restarts and is wiped
/// together with the rest of local storage by the annihilation feature.
pub fn fetch_history_cached(address: &str) -> Vec<HistItem> {
    if address.is_empty() {
        return vec![];
    }
    // Адрес локального кошелька с полным индексом — история из sled (стрим
    // держит её актуальной), в узел не ходим.
    if crate::txindex::is_complete(address) {
        return crate::txindex::history(address);
    }
    // v2: bump после фикса мультиплатежей (.102) — в v1 лежат записи,
    // распарсенные старым парсером (amount=0, kind=out), их нельзя доверять
    let key = format!("txcache:v2:{}", address);
    let cache: TxCache = crate::DB
        .get(key.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();

    {
        let mut last = hist_throttle().lock().unwrap();
        if let Some(t) = last.get(address) {
            if t.elapsed().as_secs() < HIST_THROTTLE_SECS && !cache.items.is_empty() {
                return cache.items;
            }
        }
        last.insert(address.to_string(), std::time::Instant::now());
    }

    let (page1, total) = match fetch_history_page(address, 1) {
        Ok(x) => x,
        Err(_) => return cache.items,
    };
    // meta.totalCount — ОЦЕНКА (totalCountIsEstimate: true), доверять нельзя.
    // Свежесть определяем по самому новому tx id первой страницы.
    let newest_cached = cache.items.first().map(|h| h.id.as_str()).unwrap_or("");
    if !newest_cached.is_empty()
        && page1.first().map(|h| h.id == newest_cached).unwrap_or(false)
    {
        return cache.items;
    }

    let known: std::collections::HashSet<String> =
        cache.items.iter().map(|h| h.id.clone()).collect();
    let mut fetched: Vec<HistItem> = Vec::new();
    let mut page_items = page1;
    let mut page = 1u32;
    loop {
        let overlap = page_items.iter().any(|h| known.contains(&h.id));
        let full = page_items.len() >= 100;
        fetched.append(&mut page_items);
        if overlap || !full || page >= HIST_MAX_PAGES {
            break;
        }
        page += 1;
        std::thread::sleep(Duration::from_millis(300)); // be gentle on the node
        match fetch_history_page(address, page) {
            Ok((items, _)) if !items.is_empty() => page_items = items,
            _ => break,
        }
    }

    // Merge fetched (newest) ahead of the existing cache, dedupe by tx id.
    let mut merged: Vec<HistItem> = Vec::with_capacity(fetched.len() + cache.items.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for h in fetched.into_iter().chain(cache.items.into_iter()) {
        if !h.id.is_empty() && seen.insert(h.id.clone()) {
            merged.push(h);
        }
    }
    let new_cache = TxCache { total_count: total, items: merged };
    if let Ok(bytes) = serde_json::to_vec(&new_cache) {
        let _ = crate::DB.insert(key.as_bytes(), bytes);
        let _ = crate::DB.flush();
    }
    new_cache.items
}

/// Fetch the `vendorField` strings of recent transactions matching an exact
/// (sender, recipient) pair, newest-first. Used by the price/config oracle to
/// read admin-broadcast `appPublishPrice:<v>` / `appUpdatePrice:<v>` directives.
pub fn fetch_vendor_fields(sender: &str, recipient: &str, limit: u32) -> Result<Vec<String>, String> {
    let path = format!(
        "/transactions?recipientId={}&senderId={}&page=1&limit={}&orderBy=timestamp:desc",
        recipient, sender, limit
    );
    match node_get(&path, NET_TIMEOUT) {
        Ok(txt) => {
            let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
            let empty = vec![];
            let arr = v["data"].as_array().unwrap_or(&empty);
            Ok(arr
                .iter()
                .filter_map(|t| t["vendorField"].as_str().map(|s| s.to_string()))
                .collect())
        }
        Err(e) if e.contains("404") => Ok(vec![]),
        Err(e) => Err(format!("node error: {}", e)),
    }
}

/// Recommended dynamic transfer fee (as a STH string) from `/api/node/fees`.
/// Uses the network `min` accepted transfer fee (data["1"]["transfer"]["min"]),
/// falling back to 0.25 STH (the node's static transfer floor) on any error.
pub fn recommended_transfer_fee() -> String {
    let fallback = "0.25".to_string();
    let smarthoshi: u64 = match node_get("/node/fees", NET_TIMEOUT) {
        Ok(txt) => serde_json::from_str::<Value>(&txt)
            .ok()
            .and_then(|v| {
                let f = &v["data"]["1"]["transfer"]["min"];
                f.as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .or_else(|| f.as_u64())
            })
            .unwrap_or(0),
        Err(_) => 0,
    };
    // Guard against a 0/absent min — never go below the static transfer floor.
    if smarthoshi >= 1_000_000 {
        format!("{}", smarthoshi as f64 / 100_000_000.0)
    } else {
        fallback
    }
}


/// Total STH (in smarthoshi) ever sent to `escrow` whose `vendorField` exactly
/// equals `memo`. Used by the (future) tracker payment-verification + the client
/// to confirm a publish/boost payment landed on-chain.
pub fn fetch_paid_amount(escrow: &str, memo: &str, limit: u32) -> Result<u64, String> {
    let path = format!(
        "/transactions?recipientId={}&page=1&limit={}&orderBy=timestamp:desc",
        escrow, limit
    );
    match node_get(&path, NET_TIMEOUT) {
        Ok(txt) => {
            let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
            let empty = vec![];
            let arr = v["data"].as_array().unwrap_or(&empty);
            let mut total = 0u64;
            for t in arr {
                if t["vendorField"].as_str() == Some(memo) {
                    let amt: u64 = t["amount"]
                        .as_str()
                        .and_then(|s| s.parse().ok())
                        .or_else(|| t["amount"].as_u64())
                        .unwrap_or(0);
                    total = total.saturating_add(amt);
                }
            }
            Ok(total)
        }
        Err(e) if e.contains("404") => Ok(0),
        Err(e) => Err(format!("node error: {}", e)),
    }
}


#[derive(Clone, Serialize)]
pub struct BroadcastOutcome {
    pub id: String,
    pub accepted: bool,
    /// Node-reported rejection reason (e.g. "ERR_LOW_FEE: ...") when !accepted.
    pub reason: String,
}

/// A lightweight transaction view for the follow/subscription indexer.
#[derive(Clone, Serialize)]
pub struct TxLite {
    pub id: String,
    pub sender: String,
    pub recipient: String,
    pub vendor: String,
    pub amount: u64, // smarthoshi
    pub timestamp: u64, // unix seconds
    /// Сетевой epoch (секунды от genesis) — курсор для `timestamp.from=` дельта-запросов.
    pub epoch: u64,
}

fn parse_amount(v: &Value) -> u64 {
    v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_u64()).unwrap_or(0)
}

/// Fetch recent transactions matching a query param (e.g. `recipientId` or
/// `senderId`), newest-first. Returns (sender, recipient, vendorField, ts).
/// Фильтр применяет узел (проверено на live-сети) — клиентской дофильтрации нет.
pub fn fetch_txs(param: &str, value: &str, limit: u32) -> Result<Vec<TxLite>, String> {
    fetch_txs_page(param, value, 1, limit)
}

/// Все страницы (по 100) до первой неполной или `max_pages` — для адресов с
/// тысячами tx, где нужная запись давно не в первой сотне (например `sub:`).
pub fn fetch_txs_all(param: &str, value: &str, max_pages: u32) -> Result<Vec<TxLite>, String> {
    let mut out = Vec::new();
    for page in 1..=max_pages.max(1) {
        let batch = fetch_txs_page(param, value, page, 100)?;
        let short = batch.len() < 100;
        out.extend(batch);
        if short {
            break;
        }
    }
    Ok(out)
}

/// TxLite из JSON узла; для multipayment получатель/сумма — по `wanted`.
pub(crate) fn tx_lite(t: &Value, wanted: Option<&str>) -> TxLite {
    let (mut recipient, mut amount) = (
        t["recipient"].as_str().unwrap_or("").to_string(),
        parse_amount(&t["amount"]),
    );
    if recipient.is_empty() {
        if let Some(w) = wanted {
            if let Some(p) = t["asset"]["payments"]
                .as_array()
                .and_then(|ps| ps.iter().find(|p| p["recipientId"].as_str() == Some(w)))
            {
                recipient = w.to_string();
                amount = parse_amount(&p["amount"]);
            }
        }
    }
    TxLite {
        id: t["id"].as_str().unwrap_or("").to_string(),
        sender: t["sender"].as_str().unwrap_or("").to_string(),
        recipient,
        vendor: t["vendorField"].as_str().unwrap_or("").to_string(),
        amount,
        timestamp: t["timestamp"]["unix"].as_u64().unwrap_or(0),
        epoch: t["timestamp"]["epoch"].as_u64().unwrap_or(0),
    }
}

fn fetch_txs_page(param: &str, value: &str, page: u32, limit: u32) -> Result<Vec<TxLite>, String> {
    let path = format!(
        "/transactions?{}={}&page={}&limit={}&orderBy=timestamp:desc",
        param, value, page, limit
    );
    match node_get(&path, HOT_TIMEOUT) {
        Ok(txt) => {
            let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
            let empty = vec![];
            let arr = v["data"].as_array().unwrap_or(&empty);
            // value может содержать инъекцию второго параметра ("addr&senderId=addr")
            let wanted = value.split('&').next().unwrap_or(value);
            let out: Vec<TxLite> = arr
                .iter()
                .map(|t| tx_lite(t, if param == "recipientId" { Some(wanted) } else { None }))
                .collect();
            Ok(out)
        }
        Err(e) if e.contains("404") => Ok(vec![]),
        Err(e) => Err(format!("node error: {}", e)),
    }
}


// ---------------------------------------------------------------------------
//  Local Node Tx Guard. Пользовательский clearnet-узел (localhost:4003 и т.п.)
//  может отставать от сети (устаревший nonce) или не иметь пиров (tx принята в
//  пул, но никуда не разослана) — для отправителя это «тихо не отправилось».
//  Поэтому для таких узлов nonce берём как max(свой узел, узел пула), а каждый
//  broadcast зеркалируем на узел пула (txid детерминирован — повтор безопасен).
// ---------------------------------------------------------------------------

/// Активный узел — пользовательский clearnet (не api:// и не из встроенного
/// clearnet-пула; встроенные clearnet-узлы — штатный резерв, зеркалить их не нужно).
fn is_custom_clearnet(host: &str) -> bool {
    !is_p2p(host) && !nodes().iter().any(|n| n == host)
}

/// Узел пула, через который дублируются tx-операции пользовательского узла:
/// первый iroh-узел (в сборке без P2P — первый legacy HTTPS).
fn mirror_pool_host() -> String {
    pool_all().first().cloned().unwrap_or_default()
}

/// GET к зеркалу пула (iroh или HTTPS — по типу адреса).
fn mirror_get(host: &str, path: &str, timeout: Duration) -> Result<String, String> {
    if is_p2p(host) {
        p2p_node_request_to(host, "GET", path, "", timeout)
    } else {
        https_get(&[host.to_string()], path, timeout)
    }
}

fn mirror_post(host: &str, path: &str, body: &str, timeout: Duration) -> Result<String, String> {
    if is_p2p(host) {
        p2p_node_request_to(host, "POST", path, body, timeout)
    } else {
        https_post(host, path, body, timeout)
    }
}

fn parse_wallet_nonce(txt: &str) -> u64 {
    serde_json::from_str::<Value>(txt)
        .ok()
        .and_then(|v| v["data"]["nonce"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

/// Максимальный nonce НЕПОДТВЕРЖДЁННЫХ tx отправителя в пуле активного узла
/// (`GET /transactions/unconfirmed?senderId=`, SHIP-44 §5): узел ждёт
/// `chain nonce + pending + 1`, и без этого кошелёк получал ERR_BAD_NONCE
/// «nonce 1254, expected 1256». Legacy-узел без фильтра отдаёт весь пул —
/// фильтруем по sender сами. 0 — pending нет / эндпоинт недоступен.
/// (min, max) nonce неподтверждённых tx отправителя в пуле активного узла; (0, 0) — пусто.
fn fetch_pending_nonces(address: &str) -> (u64, u64) {
    let path = format!("/transactions/unconfirmed?senderId={}&limit=100", address);
    let Ok(txt) = node_get(&path, HOT_TIMEOUT) else { return (0, 0) };
    let Ok(v) = serde_json::from_str::<Value>(&txt) else { return (0, 0) };
    let nonces: Vec<u64> = v["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|t| t["sender"].as_str() == Some(address) || t["senderId"].as_str() == Some(address))
                .filter_map(|t| t["nonce"].as_str().and_then(|s| s.parse::<u64>().ok()).or_else(|| t["nonce"].as_u64()))
                .collect()
        })
        .unwrap_or_default();
    (nonces.iter().copied().min().unwrap_or(0), nonces.iter().copied().max().unwrap_or(0))
}

/// Nonce пула, если его неподтверждённые tx вообще могут лечь в блок: цепочка должна
/// начинаться с chain+1. Разрыв (min > chain+1) — tx с nonce chain+1 потеряна (другой узел
/// отверг её как ERR_BAD_NONCE при гонке gossip), хвост в пуле никогда не подтвердится —
/// строить поверх него нельзя, берём chain+1.
fn fetch_pending_nonce(address: &str, chain_nonce: u64) -> u64 {
    let (min, max) = fetch_pending_nonces(address);
    if max == 0 {
        return 0;
    }
    if min > chain_nonce + 1 {
        crate::syslog::log(
            "net",
            format!("nonce: в пуле узла висят {}..{} без {} (chain {}) — хвост не подтвердится, игнорируем", min, max, chain_nonce + 1, chain_nonce),
        );
        return 0;
    }
    max
}

/// Nonce для подписи новой tx: с активного узла + pending в его пуле, а для
/// пользовательского clearnet-узла — ещё и с узла пула (берём максимум:
/// локальный узел может отставать).
fn fetch_tx_nonce(address: &str) -> Result<(u64, u64), String> {
    let (balance, mut nonce) = fetch_balance_nonce(address)?;
    let pending = fetch_pending_nonce(address, nonce);
    if pending > nonce {
        crate::syslog::log("net", format!("nonce: chain {} + pending до {} в пуле узла — берём {}", nonce, pending, pending));
        nonce = pending;
    }
    let active = active_host();
    if is_custom_clearnet(&active) {
        let mirror = mirror_pool_host();
        if let Ok(txt) = mirror_get(&mirror, &format!("/wallets/{}", address), HOT_TIMEOUT) {
            let pool_nonce = parse_wallet_nonce(&txt);
            if pool_nonce > nonce {
                crate::syslog::log(
                    "net",
                    format!("узел {} отстаёт: nonce {} < {} на {} — берём nonce пула", active, nonce, pool_nonce, mirror),
                );
                nonce = pool_nonce;
            }
        }
    }
    Ok((balance, nonce))
}

/// Баланс токена SHIP-14 кошелька (из `GET /wallets/:address/tokens`).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TokenBalance {
    pub id: String,
    pub symbol: String,
    pub decimals: u32,
    /// Сырой баланс в минимальных единицах (строка, как отдаёт узел).
    pub balance: String,
    /// Отформатированный баланс с учётом decimals.
    pub amount: String,
    pub name: String,
    pub logo_url: String,
}

/// Форматирование сырого баланса токена по decimals: без хвостовых нулей.
pub(crate) fn fmt_token_amount(raw: &str, decimals: u32) -> String {
    let raw = raw.trim_start_matches('0');
    let raw = if raw.is_empty() { "0" } else { raw };
    if decimals == 0 {
        return raw.to_string();
    }
    let d = decimals as usize;
    let padded = if raw.len() <= d { format!("{}{}", "0".repeat(d + 1 - raw.len()), raw) } else { raw.to_string() };
    let (int, frac) = padded.split_at(padded.len() - d);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() { int.to_string() } else { format!("{}.{}", int, frac) }
}

/// Балансы токенов SHIP-14 с манифестами. 404 / узел без поддержки → пустой список.
pub(crate) fn fetch_wallet_tokens(address: &str) -> Vec<TokenBalance> {
    let Ok(txt) = node_get(&format!("/wallets/{}/tokens", address), HOT_TIMEOUT) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<Value>(&txt) else { return Vec::new() };
    let Some(arr) = v["data"].as_array() else { return Vec::new() };
    arr.iter()
        .filter_map(|t| {
            let id = t["id"].as_str().or(t["tokenId"].as_str())?.to_string();
            let balance = t["balance"].as_str().map(|s| s.to_string()).or_else(|| t["balance"].as_u64().map(|n| n.to_string()))?;
            if balance == "0" {
                return None;
            }
            let decimals = t["decimals"].as_u64().unwrap_or(0) as u32;
            Some(TokenBalance {
                amount: fmt_token_amount(&balance, decimals),
                symbol: t["symbol"].as_str().unwrap_or("").to_string(),
                decimals,
                balance,
                name: t["meta"]["name"].as_str().unwrap_or("").to_string(),
                logo_url: t["meta"]["logoUrl"].as_str().unwrap_or("").to_string(),
                id,
            })
        })
        .collect()
}

/// Nonce, который узел ждёт, по тексту отказа ERR_BAD_NONCE. Форматы:
/// sth_core — «nonce 1254, expected 1256»; ark-core — «the sender S… has nonce 1251»
/// (текущий подтверждённый → ждёт +1). None, если это не ошибка nonce.
pub(crate) fn bad_nonce_expected(reason: &str) -> Option<u64> {
    let low = reason.to_lowercase();
    if !low.contains("nonce") {
        return None;
    }
    let num_after = |key: &str| -> Option<u64> {
        let i = low.find(key)? + key.len();
        let digits: String = low[i..].trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    };
    num_after("expected").or_else(|| num_after("has nonce").map(|n| n + 1))
}

/// Повторный broadcast с nonce, который назвал узел в ERR_BAD_NONCE (один раз).
/// Покрывает оба случая: кэш кошелька убежал вперёд ИЛИ пул узла держит
/// неподтверждённые tx и ждёт nonce больше, чем API `/wallets` (1254 vs expected 1256).
fn retry_with_expected_nonce(
    sender: &str,
    sent_nonce: u64,
    first: BroadcastOutcome,
    rebuild: impl Fn(u64) -> Result<SignedTx, String>,
) -> Result<BroadcastOutcome, String> {
    if first.accepted {
        return Ok(first);
    }
    let Some(expected) = bad_nonce_expected(&first.reason).filter(|n| *n != sent_nonce && *n > 0) else {
        return Ok(first);
    };
    crate::syslog::log("net", format!("nonce: узел ждёт {} (отправили {}) — переподписываем и повторяем", expected, sent_nonce));
    let signed = rebuild(expected)?;
    let outcome = broadcast(&signed.data)?;
    note_nonce_outcome(sender, expected, &outcome);
    Ok(outcome)
}

/// Предпроверка до подписи: баланс ≥ amount + fee. Иначе tx гарантированно
/// отклонит узел (ERR_APPLY insufficient balance) — не тратим broadcast и не
/// показываем пользователю сырой JSON. Ошибка машиночитаема для UI.
fn ensure_balance(balance: u64, amount: u64, fee: u64) -> Result<(), String> {
    let need = amount.saturating_add(fee);
    if balance < need {
        return Err(format!(
            "insufficient-balance:{}:{}",
            smarthoshi_to_sth_exact(need),
            smarthoshi_to_sth_exact(balance)
        ));
    }
    Ok(())
}

/// Разбор ответа узла на POST /transactions: accept-список либо `errors: { <txid>: [...] }`.
fn parse_broadcast(txt: &str, id: &str) -> BroadcastOutcome {
    let v: Value = serde_json::from_str(txt).unwrap_or(Value::Null);
    let accepted = v["data"]["accept"]
        .as_array()
        .map(|a| a.iter().any(|x| x.as_str() == Some(id)))
        .unwrap_or(false);
    let mut reason = String::new();
    if !accepted {
        // ARK returns 200 with `errors: { <txid>: [{type, message}] }`.
        if let Some(errs) = v["errors"].get(id).and_then(|e| e.as_array()) {
            reason = errs
                .iter()
                .map(|e| {
                    let t = e["type"].as_str().unwrap_or("");
                    let m = e["message"].as_str().unwrap_or("");
                    if t.is_empty() { m.to_string() } else { format!("{t}: {m}") }
                })
                .collect::<Vec<_>>()
                .join("; ");
        }
        if reason.is_empty() {
            reason = v["errors"].to_string();
        }
        // Узел уже получил эту tx по gossip от другого узла (зеркало приходит
        // через ~0.3с после локального broadcast) — это не отказ.
        let low = reason.to_lowercase();
        if low.contains("duplicate") || low.contains("already") {
            return BroadcastOutcome { id: id.to_string(), accepted: true, reason: String::new() };
        }
    }
    BroadcastOutcome { id: id.to_string(), accepted, reason }
}

/// Повторная отправка уже подписанной tx (watchdog pending в txindex.rs).
pub(crate) fn rebroadcast(data: &Value) -> Result<BroadcastOutcome, String> {
    broadcast(data)
}

/// Сброс in-flight nonce отправителя (pending снята как потерянная — строить поверх неё нельзя).
pub(crate) fn forget_inflight_nonce(sender: &str) {
    nonce_cache().lock().unwrap().remove(sender);
}

fn broadcast(data: &Value) -> Result<BroadcastOutcome, String> {
    let id = data["id"].as_str().unwrap_or("").to_string();
    let short = id[..id.len().min(12)].to_string();
    let body = json!({ "transactions": [data] }).to_string();
    let active = active_host();
    let primary = node_post("/transactions", &body, NET_TIMEOUT).map(|txt| parse_broadcast(&txt, &id));

    if !is_custom_clearnet(&active) {
        return match primary {
            Ok(o) => {
                if !o.accepted {
                    crate::syslog::log("net", format!("tx {} rejected — {}", short, o.reason));
                }
                Ok(o)
            }
            Err(e) => Err(format!("broadcast rejected: {}", e)),
        };
    }

    // Пользовательский узел: зеркалим tx на узел пула, чтобы она точно дошла до сети.
    let mirror = mirror_pool_host();
    let mirrored = mirror_post(&mirror, "/transactions", &body, NET_TIMEOUT).map(|txt| parse_broadcast(&txt, &id));
    match (&primary, &mirrored) {
        (Ok(p), Ok(m)) => crate::syslog::log(
            "net",
            format!("tx {}: {} accept={} · зеркало {} accept={}{}", short, active, p.accepted, mirror, m.accepted,
                if m.accepted || m.reason.is_empty() { String::new() } else { format!(" ({})", m.reason) }),
        ),
        (Err(e), Ok(m)) => crate::syslog::log(
            "net",
            format!("tx {}: узел {} недоступен ({}) — отправлено через {} accept={}", short, active, e, mirror, m.accepted),
        ),
        (Ok(p), Err(e)) => crate::syslog::log(
            "net",
            format!("tx {}: {} accept={} · зеркало {} недоступно ({})", short, active, p.accepted, mirror, e),
        ),
        (Err(a), Err(b)) => crate::syslog::log("net", format!("tx {}: оба узла недоступны: {} / {}", short, a, b)),
    }
    // Итог: вердикт узла ПУЛА главнее — он видит актуальное состояние сети.
    // Локальный пул может принять tx с уехавшим nonce, которую делегаты отвергнут
    // (ERR_BAD_NONCE) — показывать пользователю «отправлено» в этом случае нельзя.
    // Локальный accept засчитываем, только если пул недоступен.
    match (primary, mirrored) {
        (_, Ok(m)) if m.accepted => Ok(m),
        (_, Ok(m)) if !m.reason.is_empty() => {
            crate::syslog::log("net", format!("tx {} rejected by pool — {}", short, m.reason));
            Ok(m)
        }
        (Ok(p), _) if p.accepted => Ok(p),
        (Ok(p), _) => {
            crate::syslog::log("net", format!("tx {} rejected — {}", short, p.reason));
            Ok(p)
        }
        (Err(e), _) => Err(format!("broadcast rejected: {}", e)),
    }
}

/// One-shot: fetch nonce, build + sign + broadcast a transfer.
pub fn send_transfer(
    secret: &str,
    recipient: &str,
    amount_sth: &str,
    fee_sth: &str,
    vendor: &str,
) -> Result<BroadcastOutcome, String> {
    let (_priv, _pk, sender_addr) = keypair_from_secret(secret)?;
    let (balance, chain_nonce) = fetch_tx_nonce(&sender_addr)?;
    // Nonce строго от API (+1); in-flight кэш учитывается только 60с после accept.
    let next = next_nonce(&sender_addr, chain_nonce);

    let fee = if fee_sth.trim().is_empty() { SMARTHOSHI / 4 } else { parse_sth(fee_sth)? };
    ensure_balance(balance, parse_sth(amount_sth)?, fee)?;
    let signed = build_signed_transfer(secret, recipient, amount_sth, fee_sth, vendor, next)?;
    let outcome = broadcast(&signed.data)?;
    note_nonce_outcome(&sender_addr, next, &outcome);
    // подписанные байты принятой tx — для переотправки, если пул узла её потеряет
    let last_signed = std::cell::RefCell::new(signed.clone());
    let out = retry_with_expected_nonce(&sender_addr, next, outcome, |n| {
        let s = build_signed_transfer(secret, recipient, amount_sth, fee_sth, vendor, n)?;
        *last_signed.borrow_mut() = s.clone();
        Ok(s)
    })?;
    if out.accepted {
        crate::txindex::add_pending_raw(&sender_addr, &out.id, &last_signed.into_inner().data);
        // исходящая сразу видна в истории как pending — до кадра `tx` стрима
        crate::txindex::add_pending(
            &sender_addr,
            HistItem {
                id: out.id.clone(),
                kind: "out".into(),
                amount: fmt_balance_sth(parse_sth(amount_sth).unwrap_or(0)),
                counterparty: recipient.to_string(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
                confirmed: false,
                memo: vendor.to_string(),
            },
        );
    }
    Ok(out)
}

/// Sign an arbitrary message (login-with-wallet). Returns (publicKey, hashHex, signature).
pub fn sign_message(secret: &str, message: &str) -> Result<(String, String, String), String> {
    let (priv32, pubkey_hex, _addr) = keypair_from_secret(secret)?;
    let hash = sha256(message.as_bytes());
    let signature = sign_schnorr_legacy(&priv32, &pubkey_hex, &hash)?;
    Ok((pubkey_hex, hex::encode(hash), signature))
}

// ---------------------------------------------------------------------------
//  Exchange / liquidity-pool pricing (XBTS) — mirrors wallet-pro exchange.ts
//
//  Endpoint resolution (per `resolve_exchange`, cached ~30s):
//    1) clearnet `EXCHANGE_API` (default https://exchange.smartholdem.io) —
//       reachable when `GET {base}/status` returns `true`.
//    2) fallback: each `api://` node in `EXCHANGE_API_P2P` — reachable when the
//       provider's signed `GET {base}/status` decodes `pdata.body_b64` == "true".
//  If neither status probe passes, clearnet is still attempted best-effort.
// ---------------------------------------------------------------------------

fn exchange_clear_base() -> String {
    std::env::var("EXCHANGE_API")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| EXCHANGE_API_DEFAULT.to_string())
}

fn exchange_p2p_bases() -> Vec<String> {
    std::env::var("EXCHANGE_API_P2P")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().trim_end_matches('/').to_string())
                .filter(|x| x.starts_with("api://"))
                .collect::<Vec<String>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![EXCHANGE_P2P_DEFAULT.to_string()])
}

#[derive(Clone)]
enum ExBase {
    Clear(String),
    P2p(String),
}

fn exchange_cache() -> &'static Mutex<Option<(ExBase, std::time::Instant)>> {
    static CACHE: OnceLock<Mutex<Option<(ExBase, std::time::Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}
const EXCHANGE_CACHE_TTL: Duration = Duration::from_secs(30);

/// Clearnet health probe: `GET {base}/status` → body is `true`/`false`.
fn exchange_status_clear(base: &str) -> bool {
    match ureq::get(&format!("{}/status", base)).timeout(HOT_TIMEOUT).call() {
        Ok(r) => r
            .into_string()
            .map(|t| t.trim().trim_matches('"').eq_ignore_ascii_case("true"))
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// P2P health probe: the provider's signed `GET {base}/status` returns a `pdata`
/// envelope; the actual boolean is base64 in `pdata.body_b64`.
#[cfg(feature = "p2p")]
fn exchange_status_p2p(base: &str) -> bool {
    use base64::Engine;
    let app = match APP_HANDLE.get() {
        Some(a) => a.clone(),
        None => return false,
    };
    let url = format!("{}/status", base);
    let res = run_async(async move {
        match tokio::time::timeout(
            P2P_MIN_DEADLINE,
            crate::p2p::api_request(app, url, "GET".to_string(), String::new(), Default::default()),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err("timeout".to_string()),
        }
    });
    match res {
        Ok(r) if r.status < 400 => serde_json::from_str::<Value>(&r.body)
            .ok()
            .and_then(|v| v["pdata"]["body_b64"].as_str().map(|s| s.to_string()))
            .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|s| s.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false),
        _ => false,
    }
}
#[cfg(not(feature = "p2p"))]
fn exchange_status_p2p(_base: &str) -> bool {
    false
}

/// Resolve the exchange endpoint to use now (clearnet preferred, P2P fallback),
/// cached for `EXCHANGE_CACHE_TTL` so `/status` isn't probed on every call.
fn resolve_exchange() -> ExBase {
    if let Some((b, at)) = exchange_cache().lock().unwrap().clone() {
        if at.elapsed() < EXCHANGE_CACHE_TTL {
            return b;
        }
    }
    let clear = exchange_clear_base();
    let chosen = if exchange_status_clear(&clear) {
        ExBase::Clear(clear)
    } else {
        exchange_p2p_bases()
            .into_iter()
            .find(|b| exchange_status_p2p(b))
            .map(ExBase::P2p)
            .unwrap_or(ExBase::Clear(clear)) // last resort: clearnet best-effort
    };
    *exchange_cache().lock().unwrap() = Some((chosen.clone(), std::time::Instant::now()));
    chosen
}

/// GET an exchange REST `path` (leading `/`) over the resolved transport. On a
/// request failure the cache is invalidated and the other transport is tried.
fn exchange_get(path: &str, timeout: Duration) -> Result<String, String> {
    match resolve_exchange() {
        ExBase::Clear(base) => {
            match ureq::get(&format!("{}{}", base, path)).timeout(timeout).call() {
                Ok(r) => return r.into_string().map_err(|e| e.to_string()),
                Err(clear_err) => {
                    *exchange_cache().lock().unwrap() = None;
                    for pb in exchange_p2p_bases() {
                        if let Ok(t) = exchange_get_p2p(&pb, path, timeout) {
                            *exchange_cache().lock().unwrap() =
                                Some((ExBase::P2p(pb), std::time::Instant::now()));
                            return Ok(t);
                        }
                    }
                    Err(format!("exchange error: {}", clear_err))
                }
            }
        }
        ExBase::P2p(base) => match exchange_get_p2p(&base, path, timeout) {
            Ok(t) => Ok(t),
            Err(p2p_err) => {
                *exchange_cache().lock().unwrap() = None;
                let clear = exchange_clear_base();
                ureq::get(&format!("{}{}", clear, path))
                    .timeout(timeout)
                    .call()
                    .map_err(|_| format!("exchange error: {}", p2p_err))
                    .and_then(|r| r.into_string().map_err(|e| e.to_string()))
            }
        },
    }
}

#[cfg(feature = "p2p")]
fn exchange_get_p2p(base: &str, path: &str, timeout: Duration) -> Result<String, String> {
    let app = APP_HANDLE.get().ok_or("P2P-транспорт ещё не инициализирован")?.clone();
    let url = format!("{}{}", base, path);
    let deadline = timeout.max(P2P_MIN_DEADLINE);
    let res = run_async(async move {
        match tokio::time::timeout(
            deadline,
            crate::p2p::api_request(app, url, "GET".to_string(), String::new(), Default::default()),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err("таймаут P2P-запроса".to_string()),
        }
    })?;
    if res.status >= 400 {
        return Err(format!("exchange p2p status {}", res.status));
    }
    Ok(res.body)
}
#[cfg(not(feature = "p2p"))]
fn exchange_get_p2p(_base: &str, _path: &str, _timeout: Duration) -> Result<String, String> {
    Err("Протокол api:// доступен только в десктоп-сборке (P2P).".to_string())
}

/// Live spot STH/USDT price from the liquidity pool. Falls back to the static
/// reference rate when the gateway is unreachable.
pub fn fetch_spot_rate() -> f64 {
    if let Ok(txt) = exchange_get("/xbts/pool/sth-usdt", NET_TIMEOUT) {
        if let Ok(v) = serde_json::from_str::<Value>(&txt) {
            if let Some(p) = v["price"].as_f64() {
                if p > 0.0 {
                    return p;
                }
            }
        }
    }
    STH_USDT_RATE
}

/// Pool quote for a given USDT volume (AMM, slippage-aware). Returns
/// (usdtAmount, estimatedSthToReceive, minSthToReceive, effectivePrice).
pub fn fetch_swap_quote(usdt_amount: f64) -> Result<(f64, f64, f64, f64), String> {
    let txt = exchange_get(
        &format!("/xbts/pool/sth-usdt-real?usdt_amount={}", usdt_amount),
        NET_TIMEOUT,
    )?;
    let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
    let eff = v["effectivePrice"].as_f64().ok_or("invalid quote response")?;
    let usdt = v["usdtAmount"].as_f64().unwrap_or(usdt_amount);
    let est = v["estimatedSthToReceive"]
        .as_f64()
        .or_else(|| v["sthToReceive"].as_f64())
        .ok_or("invalid quote response")?;
    let min = v["minSthToReceive"].as_f64().unwrap_or(est);
    Ok((usdt, est, min, eff))
}

/// Per-user BEP20 (BSC) deposit address for BUYING STH (send USDT here).
pub fn fetch_buy_deposit_address(sth_address: &str) -> Result<String, String> {
    let txt = exchange_get(&format!("/exchange-address/bsc/{}", sth_address), NET_TIMEOUT)?;
    let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
    v["address"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "no deposit address".to_string())
}

/// STH gateway address for SELLING STH (send STH here, USDT goes to your BEP20).
pub fn fetch_sell_gate_address() -> Result<String, String> {
    let txt = exchange_get("/sell-sth-address-gate", NET_TIMEOUT)?;
    let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
    v["address"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "no gate address".to_string())
}


#[cfg(test)]
mod live_ipfs_test {
    /// One-off LIVE mainnet broadcast to verify Type 5 (IPFS) serialization is
    /// accepted by the SmartHoldem node. Ignored by default; run explicitly:
    ///   STH_TEST_PASS="..." STH_TEST_CID="Qm..." \
    ///     cargo test --features p2p publish_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn publish_live() {
        let pass = std::env::var("STH_TEST_PASS").expect("set STH_TEST_PASS");
        let cid = std::env::var("STH_TEST_CID").expect("set STH_TEST_CID");
        let out = super::publish_ipfs(&pass, &cid, "").expect("publish_ipfs failed");
        println!("ACCEPTED={} id={}", out.accepted, out.id);
        assert!(out.accepted, "node rejected the IPFS tx");
    }
}

// ============================================================================
// SHIP-14 токены (typeGroup 3) и регистрация тикера SObject (typeGroup 2, type 6)
// Раскладка asset — docs/TOKENS_RU.md §serialization; SObject — как Magistrate Entity
// (type u8, subType u8, action u8, regId len+bytes, name len+utf8, ipfs len+utf8).
// ============================================================================

fn hex32(id: &str) -> Result<Vec<u8>, String> {
    let b = hex::decode(id.trim()).map_err(|_| "token id: not hex".to_string())?;
    if b.len() != 32 {
        return Err("token id must be 32 bytes".into());
    }
    Ok(b)
}

fn u64s(v: &Value, k: &str) -> Result<u64, String> {
    v[k].as_str().map(|s| s.to_string()).or_else(|| v[k].as_u64().map(|n| n.to_string())).ok_or(format!("missing {k}"))?.parse::<u64>().map_err(|_| format!("{k}: not u64"))
}

/// (typeGroup, type, asset-bytes, asset-json) для операции токена.
fn token_asset(kind: &str, a: &Value) -> Result<(u32, u16, Vec<u8>, Value), String> {
    let mut b = Vec::new();
    match kind {
        "ticker" => {
            let name = a["name"].as_str().unwrap_or("").trim().to_string();
            if !(3..=10).contains(&name.len()) || !name.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
                return Err("ticker must match ^[A-Z0-9]{3,10}$".into());
            }
            b.extend_from_slice(&[5, 0, 0, 0]); // type=5 subType=0 action=register, regId len 0
            b.push(name.len() as u8);
            b.extend_from_slice(name.as_bytes());
            b.push(0); // ipfsData
            Ok((2, 6, b, json!({ "type": 5, "subType": 0, "action": 0, "data": { "name": name } })))
        }
        "init" => {
            let id = a["id"].as_str().unwrap_or("");
            let dec = a["decimals"].as_u64().unwrap_or(0).min(8) as u8;
            let flags = a["flags"].as_u64().unwrap_or(0) as u8;
            let (init, cap) = (u64s(a, "initialSupply")?, u64s(a, "supplyCap")?);
            b.extend_from_slice(&hex32(id)?);
            b.push(dec);
            b.push(flags);
            b.extend_from_slice(&init.to_le_bytes());
            b.extend_from_slice(&cap.to_le_bytes());
            Ok((3, 0, b, json!({ "token": { "id": id, "decimals": dec, "flags": flags, "initialSupply": init.to_string(), "supplyCap": cap.to_string() } })))
        }
        "transfer" => {
            let id = a["id"].as_str().unwrap_or("");
            let list = a["transfers"].as_array().ok_or("missing transfers")?;
            if list.is_empty() || list.len() > 64 {
                return Err("1–64 recipients".into());
            }
            let memo = a["memo"].as_str().unwrap_or("");
            if memo.len() > 64 {
                return Err("memo > 64 bytes".into());
            }
            b.extend_from_slice(&hex32(id)?);
            b.extend_from_slice(&(list.len() as u16).to_le_bytes());
            let mut out = Vec::new();
            for t in list {
                let r = t["recipientId"].as_str().ok_or("missing recipientId")?;
                let amt = u64s(t, "amount")?;
                b.extend_from_slice(&amt.to_le_bytes()); // u64 amount, затем 21 байт адреса (sth-core tx_serializer)
                b.extend_from_slice(&recipient_buffer(r)?);
                out.push(json!({ "recipientId": r, "amount": amt.to_string() }));
            }
            b.push(memo.len() as u8);
            b.extend_from_slice(memo.as_bytes());
            let mut tok = json!({ "id": id, "transfers": out });
            if !memo.is_empty() {
                tok["memo"] = json!(memo);
            }
            Ok((3, 1, b, json!({ "token": tok })))
        }
        "mint" => {
            let id = a["id"].as_str().unwrap_or("");
            let (amt, r) = (u64s(a, "amount")?, a["recipientId"].as_str().ok_or("missing recipientId")?);
            b.extend_from_slice(&hex32(id)?);
            b.extend_from_slice(&amt.to_le_bytes());
            b.extend_from_slice(&recipient_buffer(r)?);
            Ok((3, 2, b, json!({ "token": { "id": id, "amount": amt.to_string(), "recipientId": r } })))
        }
        "burn" => {
            let id = a["id"].as_str().unwrap_or("");
            let amt = u64s(a, "amount")?;
            b.extend_from_slice(&hex32(id)?);
            b.extend_from_slice(&amt.to_le_bytes());
            Ok((3, 3, b, json!({ "token": { "id": id, "amount": amt.to_string() } })))
        }
        "meta" => {
            let id = a["id"].as_str().unwrap_or("");
            let m = &a["meta"];
            let (name, desc, site) = (m["name"].as_str().unwrap_or(""), m["description"].as_str().unwrap_or(""), m["website"].as_str().unwrap_or(""));
            if name.is_empty() || name.len() > 64 || desc.len() > 512 || site.len() > 128 {
                return Err("meta: name 1–64, description ≤512, website ≤128 bytes".into());
            }
            let lt = m["logoType"].as_str().unwrap_or("");
            let logo = match m["logo"].as_str() {
                Some(s) if !s.is_empty() => base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s).map_err(|_| "logo: bad base64".to_string())?,
                _ => Vec::new(),
            };
            if logo.len() > 8192 {
                return Err("logo > 8 KB".into());
            }
            b.extend_from_slice(&hex32(id)?);
            b.push(name.len() as u8);
            b.extend_from_slice(name.as_bytes());
            b.extend_from_slice(&(desc.len() as u16).to_le_bytes());
            b.extend_from_slice(desc.as_bytes());
            b.push(site.len() as u8);
            b.extend_from_slice(site.as_bytes());
            b.push(if logo.is_empty() { 0 } else if lt == "png" { 2 } else { 1 });
            b.extend_from_slice(&(logo.len() as u16).to_le_bytes());
            b.extend_from_slice(&logo);
            let mut meta = json!({ "name": name, "description": desc, "website": site });
            if !logo.is_empty() {
                meta["logoType"] = json!(lt);
                meta["logo"] = m["logo"].clone();
            }
            Ok((3, 4, b, json!({ "token": { "id": id, "meta": meta } })))
        }
        // Рынок: SELL(5) / SELL_V2(8) = u64 amount + u64 price; BUY(6) = orderId + u64 amount; CANCEL(7) = orderId.
        "sell" | "sell_v2" => {
            let id = a["id"].as_str().unwrap_or("");
            let (amt, price) = (u64s(a, "amount")?, u64s(a, "price")?);
            if amt == 0 || price == 0 {
                return Err("amount and price must be > 0".into());
            }
            b.extend_from_slice(&hex32(id)?);
            b.extend_from_slice(&amt.to_le_bytes());
            b.extend_from_slice(&price.to_le_bytes());
            let ty = if kind == "sell" { 5 } else { 8 };
            Ok((3, ty, b, json!({ "token": { "id": id, "amount": amt.to_string(), "price": price.to_string() } })))
        }
        "buy" | "cancel" => {
            let id = a["id"].as_str().unwrap_or("");
            let oid = a["orderId"].as_str().unwrap_or("");
            b.extend_from_slice(&hex32(id)?);
            b.extend_from_slice(&hex32(oid)?);
            if kind == "buy" {
                let amt = u64s(a, "amount")?;
                b.extend_from_slice(&amt.to_le_bytes());
                return Ok((3, 6, b, json!({ "token": { "id": id, "orderId": oid, "amount": amt.to_string() } })));
            }
            Ok((3, 7, b, json!({ "token": { "id": id, "orderId": oid } })))
        }
        _ => Err(format!("unknown token op {kind}")),
    }
}

fn build_signed_token(secret: &str, kind: &str, asset: &Value, fee: u64, nonce: u64) -> Result<SignedTx, String> {
    let (priv32, pubkey_hex, _addr) = keypair_from_secret(secret)?;
    let (tg, ty, body, asset_json) = token_asset(kind, asset)?;
    let mut b = Vec::with_capacity(96 + body.len());
    b.push(0xff);
    b.push(0x02);
    b.push(VERSION_BYTE);
    b.extend_from_slice(&tg.to_le_bytes());
    b.extend_from_slice(&ty.to_le_bytes());
    b.extend_from_slice(&nonce.to_le_bytes());
    b.extend_from_slice(&hex::decode(&pubkey_hex).map_err(|e| e.to_string())?);
    b.extend_from_slice(&fee.to_le_bytes());
    b.push(0x00); // vendorField не используется
    b.extend_from_slice(&body);
    let signature = sign_schnorr_legacy(&priv32, &pubkey_hex, &sha256(&b))?;
    let mut full = b;
    full.extend_from_slice(&hex::decode(&signature).map_err(|e| e.to_string())?);
    let id = hex::encode(sha256(&full));
    let data = json!({
        "version": 2, "network": VERSION_BYTE, "typeGroup": tg, "type": ty,
        "nonce": nonce.to_string(), "senderPublicKey": pubkey_hex, "fee": fee.to_string(), "amount": "0",
        "asset": asset_json, "signature": signature, "id": id,
    });
    Ok(SignedTx { id, serialized: hex::encode(full), data })
}

/// Подписать и отправить операцию токена; `fee` — в смартоши (из `constants.tokenFees`).
pub fn send_token_op(secret: &str, kind: &str, asset: &Value, fee: u64) -> Result<BroadcastOutcome, String> {
    let (_p, _pk, sender) = keypair_from_secret(secret)?;
    let (balance, chain_nonce) = fetch_tx_nonce(&sender)?;
    ensure_balance(balance, 0, fee)?;
    let next = next_nonce(&sender, chain_nonce);
    let signed = build_signed_token(secret, kind, asset, fee, next)?;
    let first = broadcast(&signed.data)?;
    note_nonce_outcome(&sender, next, &first);
    retry_with_expected_nonce(&sender, next, first, |n| build_signed_token(secret, kind, asset, fee, n))
}

/// GET к API узла (через активный узел / iroh) → JSON.
pub(crate) fn api_json(path: &str) -> Result<Value, String> {
    serde_json::from_str(&node_get(path, NET_TIMEOUT)?).map_err(|e| e.to_string())
}

/// POST к API узла → JSON.
pub(crate) fn api_post_json(path: &str, body: &Value) -> Result<Value, String> {
    serde_json::from_str(&node_post(path, &body.to_string(), NET_TIMEOUT)?).map_err(|e| e.to_string())
}
