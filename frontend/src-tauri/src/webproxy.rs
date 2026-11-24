// ============================================================
//  SmartNet // webproxy.rs  (v1.135.199)
//
//  Локальный SOCKS5-роутер для web-вкладок: браузер (WebView2/WebKit) шлёт весь
//  http(s)-трафик на 127.0.0.1:<port> (Windows — process-wide PAC через
//  WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS до создания первого webview; Linux/macOS —
//  proxy_url web-вкладки). Роутер решает per-host:
//    • не bundle-хост → DIRECT (обычный TCP);
//    • bundle-хост (Domain Bundles: youtube / x / telegram) → режим bundle
//      auto | relay | direct; в auto — probe прямого пути (TCP + TLS ClientHello с
//      SNI ≤ 2.5 с), результат кэшируется на bundle; при блокировке → туннель к узлу
//      pq-web (Iroh QUIC, ALPN netfory/pqweb/1, `NF1 CONNECT host:port`).
//  Мониторинг pq-web-узлов: через 10 с после старта и каждые 30 мин (PING → RTT).
//  Никакого MITM: TLS остаётся сквозным браузер ↔ сайт.
//
//  Author: TechnoL0g · Created: 2026-06-28
// ============================================================

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub(crate) const PQWEB_ALPN: &[u8] = b"netfory/pqweb/1";
const PROBE_TIMEOUT: Duration = Duration::from_millis(2500);
const CACHE_OK: Duration = Duration::from_secs(600);
const CACHE_BLOCKED: Duration = Duration::from_secs(120);
const KEY_BUNDLES: &str = "cfg:webproxy:bundles";
const KEY_NODES: &str = "cfg:webproxy:nodes";
const KEY_NODES_AUTO: &str = "cfg:webproxy:nodes_auto";
const KEY_ENABLED: &str = "cfg:webproxy:enabled";
/// dVPN «total» (v1.135.197): ВСЕ https-сайты web-вкладок идут через pq-web (кроме групп mode=direct).
const KEY_TOTAL: &str = "cfg:webproxy:total";
/// Фильтр egress-семейства узлов (v1.135.207): off | prefer (по умолчанию) | only.
const KEY_V6: &str = "cfg:webproxy:v6";
/// Штраф в ранге для узлов без IPv6 в режиме prefer — v6-узлы идут первыми, остальные как откат.
const V6_PENALTY: u64 = 100_000;
const RELAY_FAIL_TTL: Duration = Duration::from_secs(60);
const TRACKER_TIMEOUT: Duration = Duration::from_secs(6);

// ---------------------------------------------------------------- config
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct Bundle {
    pub hosts: Vec<String>,
    /// auto | relay | direct
    #[serde(default = "default_mode")]
    pub mode: String,
}
fn default_mode() -> String {
    "auto".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct Bundles {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub bundles: std::collections::BTreeMap<String, Bundle>,
}

/// Узел pq-web: EndpointId (z-base32/hex как печатает `pq-web id`) + метка региона.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct PqNode {
    pub id: String,
    #[serde(default)]
    pub region: String,
    /// узел из allowlist оператора трекера (v1.135.192)
    #[serde(default)]
    pub verified: bool,
    /// версия pq-web из heartbeat узла
    #[serde(default)]
    pub ver: String,
    /// публичные ip:port узла (v1.135.195) — набираем напрямую, без DNS/pkarr-discovery
    #[serde(default)]
    pub addrs: Vec<String>,
    /// home-relay узла
    #[serde(default)]
    pub relay: String,
    /// узел пропускает любой https-хост (`pq-web serve --open`) — нужен для режима dVPN
    #[serde(default)]
    pub open: bool,
    /// узел выходит к сайтам по IPv6 (pq-web ≥ 0.4.0, подписано в announce v4) — бейдж «v6» (v1.135.204)
    #[serde(default)]
    pub ipv6: bool,
    /// узел — релей Mycelium (`pq-web serve --mycelium`, announce v5) — bootstrap для mushroom-цепочек (v1.135.239)
    #[serde(default)]
    pub mycelium: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct NodeHealth {
    pub id: String,
    pub region: String,
    /// manual | tracker
    pub source: String,
    pub verified: bool,
    /// первый известный ip:port узла (для UI)
    pub addr: String,
    /// узел с `--open` (любой хост)
    pub open: bool,
    /// исходящий IPv6 у узла
    pub ipv6: bool,
    pub ok: bool,
    #[serde(rename = "rttMs")]
    pub rtt_ms: u64,
    #[serde(rename = "checkedAt")]
    pub checked_at: u64,
    pub error: String,
}

/// Список узлов, полученный с трекера (`GET /pqweb`, v1.135.191): sled `cfg:webproxy:nodes_auto`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct AutoNodes {
    #[serde(default)]
    pub nodes: Vec<PqNode>,
    /// подпись оператора проверена пиннутым ключом
    #[serde(default)]
    pub signed: bool,
    #[serde(default)]
    pub tracker: String,
    #[serde(default)]
    pub at: u64,
    /// последняя попытка обновления провалилась — работаем с кэшем
    #[serde(default)]
    pub stale: bool,
    /// причина последнего неуспеха (для UI); пусто = ок
    #[serde(default)]
    pub error: String,
}

/// Ответ трекера `GET /pqweb`; канон подписи: ["pqweb", ts, "id@region"…].join("\n").
#[derive(Deserialize)]
struct PqWebResp {
    #[serde(default)]
    nodes: Vec<PqNode>,
    #[serde(default)]
    ts: u64,
    #[serde(default)]
    sig: String,
}

fn default_bundles() -> Bundles {
    let mk = |hosts: &[&str]| Bundle { hosts: hosts.iter().map(|h| h.to_string()).collect(), mode: "auto".into() };
    let mut b = std::collections::BTreeMap::new();
    b.insert("youtube".into(), mk(&["youtube.com", "*.youtube.com", "youtu.be", "*.googlevideo.com", "*.ytimg.com", "*.ggpht.com", "*.youtube-nocookie.com"]));
    b.insert("x".into(), mk(&["x.com", "*.x.com", "twitter.com", "*.twitter.com", "*.twimg.com", "t.co"]));
    b.insert("telegram".into(), mk(&["telegram.org", "*.telegram.org", "t.me", "*.t.me", "*.tdesktop.com", "telegram.me"]));
    // тестовая группа (v1.135.196): 2ip.ru показывает IP выхода — всегда через узел, чтобы видеть IP pq-web
    b.insert("iptest".into(), Bundle { hosts: vec!["2ip.ru".into(), "*.2ip.ru".into(), "2ip.io".into(), "*.2ip.io".into()], mode: "relay".into() });
    Bundles { version: 2, bundles: b }
}

fn load_bundles() -> Bundles {
    let mut b = crate::DB
        .get(KEY_BUNDLES.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Bundles>(&v).ok())
        .filter(|b| !b.bundles.is_empty())
        .unwrap_or_else(default_bundles);
    // новые встроенные группы добавляются к сохранённому пользователем набору
    for (name, def) in default_bundles().bundles {
        b.bundles.entry(name).or_insert(def);
    }
    b
}

fn load_manual_nodes() -> Vec<PqNode> {
    crate::DB
        .get(KEY_NODES.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Vec<PqNode>>(&v).ok())
        .unwrap_or_default()
}

fn load_auto() -> AutoNodes {
    crate::DB
        .get(KEY_NODES_AUTO.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<AutoNodes>(&v).ok())
        .unwrap_or_default()
}

/// Все узлы: ручные (приоритет) + с трекера, без дублей по id. `.1` = источник.
fn load_nodes() -> Vec<(PqNode, &'static str)> {
    let mut out: Vec<(PqNode, &'static str)> = load_manual_nodes().into_iter().map(|n| (n, "manual")).collect();
    for n in load_auto().nodes {
        if let Some((m, _)) = out.iter_mut().find(|(m, _)| m.id.eq_ignore_ascii_case(&n.id)) {
            // ручной узел без адресов — дополняем ip:port/relay из heartbeat трекера
            if m.addrs.is_empty() {
                m.addrs = n.addrs.clone();
            }
            if m.relay.is_empty() {
                m.relay = n.relay.clone();
            }
            if m.region.is_empty() {
                m.region = n.region.clone();
            }
            m.open = m.open || n.open;
            m.ipv6 = m.ipv6 || n.ipv6;
            m.mycelium = m.mycelium || n.mycelium;
        } else {
            out.push((n, "tracker"));
        }
    }
    out
}

/// EndpointId узлов pq-web с флагом `mycelium` (ручные + трекер) — bootstrap-релеи Mycelium.
pub(crate) fn mycelium_node_ids() -> Vec<String> {
    load_nodes().into_iter().filter(|(n, _)| n.mycelium).map(|(n, _)| n.id).collect()
}

/// Подсказки адреса релеев Mycelium из heartbeat трекера: (id, [ip:port…, relay-url]).
pub(crate) fn mycelium_node_hints() -> Vec<(String, Vec<String>)> {
    load_nodes()
        .into_iter()
        .filter(|(n, _)| n.mycelium)
        .map(|(n, _)| {
            let mut a = n.addrs.clone();
            if !n.relay.is_empty() {
                a.push(n.relay.clone());
            }
            (n.id, a)
        })
        .collect()
}

fn enabled() -> bool {
    !matches!(crate::DB.get(KEY_ENABLED.as_bytes()), Ok(Some(v)) if v.as_ref() == b"0")
}

/// Режим dVPN: весь web-трафик вкладок через узлы pq-web с `open` (любой хост).
pub(crate) fn total() -> bool {
    matches!(crate::DB.get(KEY_TOTAL.as_bytes()), Ok(Some(v)) if v.as_ref() == b"1")
}

/// Предпочтение IPv6-узлов в dVPN: `off` — не учитывать, `prefer` — v6 первыми (по
/// умолчанию, при недоступности берём обычные), `only` — жёсткий фильтр (без v6-узлов ошибка).
pub(crate) fn v6_mode() -> String {
    match crate::DB.get(KEY_V6.as_bytes()) {
        Ok(Some(v)) if v.as_ref() == b"off" => "off".into(),
        Ok(Some(v)) if v.as_ref() == b"only" => "only".into(),
        _ => "prefer".into(),
    }
}

fn host_matches(rule: &str, host: &str) -> bool {
    let rule = rule.trim().trim_end_matches('.').to_ascii_lowercase();
    if let Some(suffix) = rule.strip_prefix("*.") {
        host.len() > suffix.len() + 1 && host.ends_with(suffix) && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
    } else {
        rule == host
    }
}

fn bundle_of(b: &Bundles, host: &str) -> Option<String> {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    b.bundles.iter().find(|(_, v)| v.hosts.iter().any(|r| host_matches(r, &h))).map(|(k, _)| k.clone())
}

// ---------------------------------------------------------------- state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Route {
    Direct,
    Relay,
    /// Mycelium включён: ВСЁ (в т.ч. группы YouTube/X/Telegram) через mushroom-цепочку → pq-web `--open`
    Mycelium,
}

#[derive(Default)]
struct State {
    port: u16,
    /// bundle → (маршрут, до какого момента действителен, причина)
    routes: HashMap<String, (Route, Instant, String)>,
    health: Vec<NodeHealth>,
    app: Option<tauri::AppHandle>,
    /// host → узел (для бейджа в адресной строке)
    active: HashMap<String, String>,
    /// host → «v6»/«v4»: семейство, которым узел вышел к сайту (pq-web ≥0.5.0)
    active_fam: HashMap<String, String>,
    /// bundle → когда туннель к pq-web не удался (HUD не показываем, честная ошибка)
    relay_failed: HashMap<String, Instant>,
}

static STATE: Lazy<Mutex<State>> = Lazy::new(|| Mutex::new(State::default()));
static LISTENER: Mutex<Option<std::net::TcpListener>> = Mutex::new(None);

fn st() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn port() -> u16 {
    st().port
}

/// Хост входит в Domain Bundle (роутер сам решает маршрут — пре-флайт браузера не нужен).
pub(crate) fn is_bundle_host(host: &str) -> bool {
    st().port != 0 && (total() || bundle_of(&load_bundles(), host).is_some())
}

/// Данные для HUD-страницы «Routing via NETFORY Relay…» (websites.rs) при ошибке
/// загрузки bundle-хоста, чей маршрут уже переведён на relay.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Hud {
    pub bundle: String,
    /// relay | no-nodes | relay-failed
    pub state: &'static str,
    pub node: String,
    pub region: String,
    #[serde(rename = "rttMs")]
    pub rtt_ms: u64,
}

/// Браузер сам упёрся в сетевую ошибку на bundle-хосте (DPI-reset после probe,
/// подменённый сертификат и т.п.) → переводим группу на relay (TTL CACHE_BLOCKED).
/// true = маршрут переключён/уже relay; false = хост вне bundles или mode=direct.
pub(crate) fn mark_blocked(host: &str, why: &str) -> bool {
    let bundles = load_bundles();
    let Some(bundle) = bundle_of(&bundles, host) else { return false };
    let mode = bundles.bundles.get(&bundle).map(|b| b.mode.as_str()).unwrap_or("auto");
    if mode == "direct" {
        return false;
    }
    let mut s = st();
    if s.port == 0 {
        return false;
    }
    let already = matches!(s.routes.get(&bundle), Some((Route::Relay, until, _)) if *until > Instant::now());
    if !already {
        crate::syslog::log("proxy", format!("{host}: браузер получил {why} → bundle {bundle} на relay"));
        s.routes.insert(bundle.clone(), (Route::Relay, Instant::now() + CACHE_BLOCKED, format!("browser:{why}")));
    }
    true
}

pub(crate) fn hud_for(host: &str) -> Option<Hud> {
    let bundles = load_bundles();
    let bundle = bundle_of(&bundles, host)?;
    let mode = bundles.bundles.get(&bundle).map(|b| b.mode.as_str()).unwrap_or("auto");
    if mode == "direct" {
        return None;
    }
    let s = st();
    if s.port == 0 {
        return None;
    }
    let relay_now = mode == "relay" || matches!(s.routes.get(&bundle), Some((Route::Relay, until, _)) if *until > Instant::now());
    if !relay_now {
        return None;
    }
    let failed = matches!(s.relay_failed.get(&bundle), Some(t) if t.elapsed() < RELAY_FAIL_TTL);
    let health = s.health.clone();
    drop(s);
    let Some(best) = best_nodes(false).into_iter().next() else {
        return Some(Hud { bundle, state: "no-nodes", node: String::new(), region: String::new(), rtt_ms: 0 });
    };
    let rtt = health.iter().find(|h| h.id == best.id && h.ok).map(|h| h.rtt_ms).unwrap_or(0);
    Some(Hud { bundle, state: if failed { "relay-failed" } else { "relay" }, node: best.id, region: best.region, rtt_ms: rtt })
}

/// PAC: весь http(s) → локальный SOCKS5 с фолбэком DIRECT; loopback — напрямую.
#[allow(dead_code)]
fn pac(port: u16) -> String {
    format!(
        "function FindProxyForURL(url, host) {{ if (isPlainHostName(host) || host === 'localhost' || host === '127.0.0.1' || host === '::1') return 'DIRECT'; return 'SOCKS5 127.0.0.1:{port}; DIRECT'; }}"
    )
}

/// ДО tauri::Builder: биндим порт и (Windows) прописываем PAC в окружение WebView2.
pub(crate) fn start_early() {
    if !enabled() {
        return;
    }
    let Ok(listener) = std::net::TcpListener::bind("127.0.0.1:0") else {
        crate::syslog::log("proxy", "не удалось занять loopback-порт для SOCKS5");
        return;
    };
    let Ok(addr) = listener.local_addr() else { return };
    let _ = listener.set_nonblocking(true);
    st().port = addr.port();
    *LISTENER.lock().unwrap_or_else(|e| e.into_inner()) = Some(listener);
    #[cfg(target_os = "windows")]
    {
        use base64::Engine;
        let pac_url = format!(
            "data:application/x-ns-proxy-autoconfig;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(pac(addr.port()))
        );
        let mut args = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").unwrap_or_default();
        if !args.is_empty() {
            args.push(' ');
        }
        args.push_str(&format!("--proxy-pac-url={pac_url}"));
        std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", args);
    }
    crate::syslog::log("proxy", format!("SOCKS5-роутер web-вкладок на 127.0.0.1:{}", addr.port()));
}

/// В setup(): accept-loop роутера + мониторинг pq-web (10 с, затем каждые 30 мин).
pub(crate) fn start(app: tauri::AppHandle) {
    let Some(listener) = LISTENER.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    st().app = Some(app.clone());
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = tauri::async_runtime::spawn_blocking(refresh_geo).await;
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            sample_hist();
            let _ = tauri::async_runtime::spawn_blocking(refresh_geo).await;
        }
    });
    tauri::async_runtime::spawn(async move {
        let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
        loop {
            let Ok((sock, _)) = listener.accept().await else { continue };
            tauri::async_runtime::spawn(async move {
                if let Err(e) = handle_socks(sock).await {
                    crate::syslog::log("proxy", format!("socks: {e}"));
                }
            });
        }
    });
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        loop {
            let _ = fetch_tracker_nodes().await;
            check_nodes().await;
            tokio::time::sleep(Duration::from_secs(1800)).await;
        }
    });
    // keepalive (v1.135.198): тёплое QUIC-соединение к лучшему узлу — первый CONNECT из вкладки
    // не ждёт холодный handshake через реле (при заблокированном UDP он занимал 10–20 с и срывал
    // первую загрузку). PING каждые 20 с по всем кэшированным соединениям, RTT — в health.
    #[cfg(feature = "p2p")]
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(25)).await;
        loop {
            keepalive_tick().await;
            tokio::time::sleep(Duration::from_secs(20)).await;
        }
    });
}

/// Один проход keepalive: PING по кэшированным соединениям (+ прогрев лучшего узла, если кэш пуст).
#[cfg(feature = "p2p")]
async fn keepalive_tick() {
    if !enabled() {
        return;
    }
    let nodes = load_nodes();
    if nodes.is_empty() {
        return;
    }
    let cached: Vec<String> = CONNS.lock().await.keys().cloned().collect();
    let mut targets: Vec<PqNode> = nodes.iter().filter(|(n, _)| cached.contains(&n.id)).map(|(n, _)| n.clone()).collect();
    if targets.is_empty() {
        if let Some(best) = best_nodes(false).into_iter().next() {
            targets.push(best);
        }
    }
    let mut changed = false;
    for n in targets {
        let res = tokio::time::timeout(Duration::from_secs(20), ping_node(&n)).await.unwrap_or(Err("keepalive timeout".into()));
        let mut s = st();
        let now = now_secs();
        match s.health.iter_mut().find(|h| h.id == n.id) {
            Some(h) => {
                let was_ok = h.ok;
                match &res {
                    Ok(rtt) => {
                        h.ok = true;
                        h.rtt_ms = *rtt;
                        h.error.clear();
                    }
                    Err(e) => {
                        h.ok = false;
                        h.error = e.clone();
                    }
                }
                h.checked_at = now;
                changed |= was_ok != h.ok;
            }
            None => {
                s.health.push(NodeHealth {
                    id: n.id.clone(),
                    region: n.region.clone(),
                    source: "manual".into(),
                    verified: n.verified,
                    addr: n.addrs.first().cloned().unwrap_or_default(),
                    open: n.open,
                    ipv6: n.ipv6,
                    ok: res.is_ok(),
                    rtt_ms: *res.as_ref().unwrap_or(&0),
                    checked_at: now,
                    error: res.as_ref().err().cloned().unwrap_or_default(),
                });
                changed = true;
            }
        }
        if let Err(e) = &res {
            crate::syslog::log("proxy", format!("keepalive {}: {e}", &n.id[..n.id.len().min(10)]));
        }
    }
    if changed {
        let (health, app) = {
            let s = st();
            (s.health.clone(), s.app.clone())
        };
        if let Some(app) = app {
            let _ = app.emit("webproxy-relays", serde_json::json!(health));
        }
    }
}

// ---------------------------------------------------------------- SOCKS5
async fn handle_socks(mut sock: TcpStream) -> Result<(), String> {
    let mut head = [0u8; 2];
    sock.read_exact(&mut head).await.map_err(|e| e.to_string())?;
    if head[0] != 0x05 {
        return Err("not socks5".into());
    }
    let mut methods = vec![0u8; head[1] as usize];
    sock.read_exact(&mut methods).await.map_err(|e| e.to_string())?;
    sock.write_all(&[0x05, 0x00]).await.map_err(|e| e.to_string())?;
    let mut req = [0u8; 4];
    sock.read_exact(&mut req).await.map_err(|e| e.to_string())?;
    if req[1] != 0x01 {
        let _ = sock.write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await;
        return Err("only CONNECT".into());
    }
    let host = match req[3] {
        0x01 => {
            let mut b = [0u8; 4];
            sock.read_exact(&mut b).await.map_err(|e| e.to_string())?;
            std::net::Ipv4Addr::from(b).to_string()
        }
        0x03 => {
            let mut l = [0u8; 1];
            sock.read_exact(&mut l).await.map_err(|e| e.to_string())?;
            let mut b = vec![0u8; l[0] as usize];
            sock.read_exact(&mut b).await.map_err(|e| e.to_string())?;
            String::from_utf8_lossy(&b).to_string()
        }
        0x04 => {
            let mut b = [0u8; 16];
            sock.read_exact(&mut b).await.map_err(|e| e.to_string())?;
            std::net::Ipv6Addr::from(b).to_string()
        }
        _ => return Err("bad atyp".into()),
    };
    let mut pb = [0u8; 2];
    sock.read_exact(&mut pb).await.map_err(|e| e.to_string())?;
    let port = u16::from_be_bytes(pb);

    let bundles = load_bundles();
    let bundle = bundle_of(&bundles, &host);
    #[cfg(feature = "p2p")]
    if crate::mycel::enabled() {
        return via_mycelium(sock, &host, port, bundle.as_deref().unwrap_or("")).await;
    }
    let total = total();
    let route = match &bundle {
        // dVPN total: любой хост вне групп → через узел с --open
        None if total => Route::Relay,
        None => Route::Direct,
        Some(b) => decide(b, &bundles.bundles[b].mode, &host, port, total).await,
    };
    let ok_reply = [0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
    match route {
        Route::Direct => {
            let upstream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((host.as_str(), port))).await;
            let mut upstream = match upstream {
                Ok(Ok(s)) => s,
                _ => {
                    // прямой путь упал у bundle-хоста → на следующий запрос пойдём через relay
                    if let Some(b) = &bundle {
                        let app = {
                            let mut s = st();
                            s.routes.insert(b.clone(), (Route::Relay, Instant::now() + CACHE_BLOCKED, "connect-failed".into()));
                            s.app.clone()
                        };
                        if let Some(app) = app {
                            crate::websites::on_bundle_direct_failed(&app, &host);
                        }
                    }
                    let _ = sock.write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await;
                    return Ok(());
                }
            };
            let _ = upstream.set_nodelay(true);
            sock.write_all(&ok_reply).await.map_err(|e| e.to_string())?;
            let _ = tokio::io::copy_bidirectional(&mut sock, &mut upstream).await;
            Ok(())
        }
        // общий бюджет на туннель: браузер не должен висеть минуту, если узел мёртв
        Route::Mycelium => Ok(()),
        Route::Relay => match tokio::time::timeout(Duration::from_secs(25), open_tunnel(&host, port, bundle.is_none())).await.unwrap_or_else(|_| Err("туннель: таймаут 25 с".into())) {
            Ok((node, _conn, recv, send, fam)) => {
                sock.write_all(&ok_reply).await.map_err(|e| e.to_string())?;
                if let Some(b) = &bundle {
                    st().relay_failed.remove(b);
                }
                emit_route(&host, bundle.as_deref().unwrap_or(""), Route::Relay, &node, &fam);
                let mut joined = tokio::io::join(recv, send);
                let _ = tokio::io::copy_bidirectional(&mut sock, &mut joined).await;
                Ok(())
            }
            Err(e) => {
                crate::syslog::log("proxy", format!("{host}:{port} relay failed: {e} → DIRECT"));
                if let Some(b) = &bundle {
                    st().relay_failed.insert(b.clone(), Instant::now());
                }
                let _ = sock.write_all(&[0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await;
                Ok(())
            }
        },
    }
}

/// Mycelium-режим вкладок: никакого DIRECT и pq-web напрямую. Нет цепочки / сбой — отвечаем SOCKS «OK»
/// и закрываем: ошибка SOCKS дала бы браузеру фолбэк `DIRECT` из PAC (утечка IP), а пустой ответ
/// превращается в нашу страницу «Mycelium строит цепочку…».
#[cfg(feature = "p2p")]
async fn via_mycelium(mut sock: TcpStream, host: &str, port: u16, bundle: &str) -> Result<(), String> {
    let ok_reply = [0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
    let r = tokio::time::timeout(Duration::from_secs(45), crate::mycel_vpn::tcp_via_mycelium(host, port)).await.unwrap_or_else(|_| Err("timeout 45 s".into()));
    match r {
        Ok((node, up)) => {
            sock.write_all(&ok_reply).await.map_err(|e| e.to_string())?;
            emit_route(host, bundle, Route::Mycelium, &node, "");
            let key = site_key(host);
            lk(&SITE_EXIT).insert(key.clone(), node.clone());
            lk(&SITE_DIRECT).remove(&key);
            let kill = site_kill(&key);
            let ctr = site_counter(&key);
            let (mut sr, mut sw) = sock.into_split();
            let (mut ur, mut uw) = tokio::io::split(up);
            let c1 = ctr.clone();
            let tx = async move {
                let mut b = vec![0u8; 16 * 1024];
                while let Ok(n) = sr.read(&mut b).await {
                    if n == 0 || uw.write_all(&b[..n]).await.is_err() {
                        break;
                    }
                    c1.1.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
                }
                let _ = uw.shutdown().await;
            };
            let rx = async move {
                let mut b = vec![0u8; 16 * 1024];
                while let Ok(n) = ur.read(&mut b).await {
                    if n == 0 || sw.write_all(&b[..n]).await.is_err() {
                        break;
                    }
                    ctr.0.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
                }
                let _ = sw.shutdown().await;
            };
            // «Новая личность» / смена страны рвёт живые туннели сайта — браузер переподключится через новый выход
            tokio::select! {
                _ = async { tokio::join!(tx, rx) } => {}
                _ = kill.notified() => {}
            }
        }
        Err(e) if crate::mycel::load().allow_direct => {
            // пользователь разрешил прямое соединение при недоступности Mycelium
            crate::syslog::log("proxy", format!("{host}:{port} mycelium: {e} → DIRECT (allowDirect)"));
            match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((host, port))).await {
                Ok(Ok(mut up)) => {
                    sock.write_all(&ok_reply).await.map_err(|e| e.to_string())?;
                    emit_route(host, bundle, Route::Direct, "", "");
                    // SitePanel покажет красную плашку «DIRECT — ваш реальный IP»
                    lk(&SITE_DIRECT).insert(site_key(host), Instant::now());
                    let _ = tokio::io::copy_bidirectional(&mut sock, &mut up).await;
                }
                _ => {
                    let _ = sock.write_all(&ok_reply).await;
                }
            }
        }
        Err(e) => {
            crate::syslog::log("proxy", format!("{host}:{port} mycelium: {e} → blocked (no direct leak)"));
            let _ = sock.write_all(&ok_reply).await;
        }
    }
    Ok(())
}

type SiteCtr = std::sync::Arc<(std::sync::atomic::AtomicU64, std::sync::atomic::AtomicU64)>;
/// Трафик через Mycelium по «сайту»: группа (YouTube = youtube.com + ytimg + googlevideo …) или базовый домен.
static SITE_BYTES: once_cell::sync::Lazy<std::sync::Mutex<HashMap<String, SiteCtr>>> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

fn site_key(host: &str) -> String {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if let Some(b) = bundle_of(&load_bundles(), &h) {
        return format!("bundle:{b}");
    }
    let parts: Vec<&str> = h.split('.').collect();
    if parts.len() >= 2 { parts[parts.len() - 2..].join(".") } else { h }
}

fn site_counter(key: &str) -> SiteCtr {
    SITE_BYTES.lock().unwrap().entry(key.to_string()).or_default().clone()
}

/// Получено / отправлено через Mycelium для сайта вкладки (включая его CDN из той же группы).
#[tauri::command]
pub(crate) async fn webproxy_site_bytes(host: String) -> serde_json::Value {
    let key = site_key(&host);
    let c = SITE_BYTES.lock().unwrap().get(&key).cloned();
    let (rx, tx) = c.map(|c| (c.0.load(std::sync::atomic::Ordering::Relaxed), c.1.load(std::sync::atomic::Ordering::Relaxed))).unwrap_or((0, 0));
    let hist: Vec<[u64; 2]> = lk(&SITE_HIST)
        .get(&key)
        .map(|h| h.buckets.iter().copied().chain(std::iter::once([rx.saturating_sub(h.last[0]), tx.saturating_sub(h.last[1])])).collect())
        .unwrap_or_else(|| vec![[rx, tx]]);
    serde_json::json!({ "key": key, "rx": rx, "tx": tx, "hist": hist })
}

fn lk<T>(m: &'static Lazy<Mutex<T>>) -> std::sync::MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------- site history / exit country (v1.136.2)
const HIST_LEN: usize = 30;
const AVOID_TTL: Duration = Duration::from_secs(600);
const KEY_EXIT: &str = "cfg:webproxy:exit_country";
/// Страны узлов, подтверждённые «Проверить мой IP» (EndpointId → ISO).
const KEY_NODE_CC: &str = "cfg:webproxy:node_cc";
/// Страна выхода по умолчанию для всех сайтов ("" = Авто).
const KEY_EXIT_DEFAULT: &str = "cfg:webproxy:exit_default";
/// Пин сайта «Авто» поверх страны по умолчанию.
const PIN_AUTO: &str = "*";

#[derive(Default)]
struct Hist {
    last: [u64; 2],
    buckets: std::collections::VecDeque<[u64; 2]>,
}
/// Поминутная история трафика сайта (≤ 30 мин): дельты rx/tx.
static SITE_HIST: Lazy<Mutex<HashMap<String, Hist>>> = Lazy::new(|| Mutex::new(HashMap::new()));
/// сайт → EndpointId pq-web, через который он вышел последним
static SITE_EXIT: Lazy<Mutex<HashMap<String, String>>> = Lazy::new(|| Mutex::new(HashMap::new()));
/// сайт → узлы, которые уже были выходом до «Новой личности» (круговая ротация: давние — раньше)
static SITE_AVOID: Lazy<Mutex<HashMap<String, Vec<(String, Instant)>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static SITE_KILL: Lazy<Mutex<HashMap<String, std::sync::Arc<tokio::sync::Notify>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
/// EndpointId → страна по GeoIP. Заполняет ТОЛЬКО фоновая `refresh_geo` (spawn_blocking):
/// сетевой путь и UI читают кэш и никогда не трогают файл базы (70 МБ).
static NODE_CC: Lazy<Mutex<HashMap<String, (Option<String>, Instant)>>> = Lazy::new(|| Mutex::new(HashMap::new()));
const NODE_CC_NEG_TTL: Duration = Duration::from_secs(120);
/// сайт → когда последний раз ушёл DIRECT (allowDirect при недоступном Mycelium)
static SITE_DIRECT: Lazy<Mutex<HashMap<String, Instant>>> = Lazy::new(|| Mutex::new(HashMap::new()));
const DIRECT_SHOW: Duration = Duration::from_secs(90);
/// Отмена «Проверить мой IP» — по сайту (отмена в одной вкладке не трогает другие).
static CHECK_CANCEL: Lazy<Mutex<HashMap<String, std::sync::Arc<tokio::sync::Notify>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
/// Потолок проверки: одна сборка цепочки (≤ 30 с в mycelium) + HTTPS.
const CHECK_DEADLINE: Duration = Duration::from_secs(40);
/// Записи настроек выхода в sled — строго по очереди (иначе старый снимок может затереть новый).
static PERSIST: Lazy<tokio::sync::Mutex<()>> = Lazy::new(|| tokio::sync::Mutex::new(()));

/// Настройки выхода в памяти: sled читается один раз, запись — в фоне (никогда в UI-потоке).
#[derive(Clone, Default)]
struct ExitCfg {
    pins: HashMap<String, String>,
    default: String,
    verified: HashMap<String, String>,
}
static EXIT_CFG: Lazy<Mutex<ExitCfg>> = Lazy::new(|| {
    let json = |k: &str| -> HashMap<String, String> { crate::DB.get(k.as_bytes()).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default() };
    let default = crate::DB.get(KEY_EXIT_DEFAULT.as_bytes()).ok().flatten().map(|v| String::from_utf8_lossy(&v).to_string()).filter(|s| valid_iso(s)).unwrap_or_default();
    Mutex::new(ExitCfg { pins: json(KEY_EXIT), default, verified: json(KEY_NODE_CC) })
});

/// Сохранить настройки выхода в sled вне async-потоков.
async fn persist_exit_cfg() -> Result<(), String> {
    let _q = PERSIST.lock().await;
    let c = lk(&EXIT_CFG).clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        crate::DB.insert(KEY_EXIT.as_bytes(), serde_json::to_vec(&c.pins).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        crate::DB.insert(KEY_EXIT_DEFAULT.as_bytes(), c.default.as_bytes()).map_err(|e| e.to_string())?;
        crate::DB.insert(KEY_NODE_CC.as_bytes(), serde_json::to_vec(&c.verified).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        crate::DB.flush().map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Свои EndpointId — свой узел никогда не выход своих же сайтов.
fn own_ids() -> Vec<String> {
    #[cfg(feature = "p2p")]
    {
        let mut v: Vec<String> = crate::p2p::my_endpoint_id_hex().into_iter().collect();
        if let Some(m) = crate::mycel::instance() {
            v.push(m.transport_id().to_string());
        }
        v
    }
    #[cfg(not(feature = "p2p"))]
    {
        Vec::new()
    }
}

/// Узлы-выходы `--open` без своих id.
fn open_nodes() -> Vec<PqNode> {
    let me = own_ids();
    best_nodes(true).into_iter().filter(|n| !me.iter().any(|m| m.eq_ignore_ascii_case(&n.id))).collect()
}

/// Блокирующее: дозаполнить кэш стран узлов по GeoIP (только из spawn_blocking).
fn refresh_geo() {
    let Some(app) = st().app.clone() else { return };
    for n in open_nodes() {
        let fresh = lk(&NODE_CC).get(&n.id).is_some_and(|(v, at)| v.is_some() || at.elapsed() < NODE_CC_NEG_TTL);
        if fresh {
            continue;
        }
        let v = n.addrs.iter().find_map(|a| crate::geo::country_iso(&app, a));
        lk(&NODE_CC).insert(n.id.clone(), (v, Instant::now()));
    }
}

#[cfg_attr(not(feature = "p2p"), allow(dead_code))]
fn site_kill(key: &str) -> std::sync::Arc<tokio::sync::Notify> {
    lk(&SITE_KILL).entry(key.to_string()).or_default().clone()
}

fn sample_hist() {
    let totals: Vec<(String, [u64; 2])> = SITE_BYTES
        .lock()
        .unwrap()
        .iter()
        .map(|(k, c)| (k.clone(), [c.0.load(std::sync::atomic::Ordering::Relaxed), c.1.load(std::sync::atomic::Ordering::Relaxed)]))
        .collect();
    let mut h = lk(&SITE_HIST);
    for (k, t) in totals {
        let e = h.entry(k).or_default();
        e.buckets.push_back([t[0].saturating_sub(e.last[0]), t[1].saturating_sub(e.last[1])]);
        while e.buckets.len() > HIST_LEN - 1 {
            e.buckets.pop_front();
        }
        e.last = t;
    }
}

fn load_exit_pins() -> HashMap<String, String> {
    lk(&EXIT_CFG).pins.clone()
}

/// Метка региона оператора → ISO («de», «DE-fra», «nl_ams») — если начинается с 2 букв.
fn region_iso(region: &str) -> Option<String> {
    let head = region.trim().split(|c: char| c == '-' || c == '_' || c == ' ' || c == '/').next().unwrap_or("");
    let iso = (head.len() == 2 && head.chars().all(|c| c.is_ascii_alphabetic())).then(|| head.to_ascii_uppercase())?;
    Some(if iso == "UK" { "GB".into() } else { iso })
}

/// Страна узла по GeoIP — только из кэша (`refresh_geo`).
fn geo_iso(n: &PqNode) -> Option<String> {
    lk(&NODE_CC).get(&n.id).and_then(|(v, _)| v.clone())
}

fn load_verified() -> HashMap<String, String> {
    lk(&EXIT_CFG).verified.clone()
}

fn valid_iso(iso: &str) -> bool {
    iso.len() == 2 && iso.chars().all(|c| c.is_ascii_alphabetic())
}

pub(crate) fn exit_default() -> String {
    lk(&EXIT_CFG).default.clone()
}

/// Страна выхода узла: подтверждённая проверкой IP → GeoIP → метка региона.
fn node_country(n: &PqNode) -> Option<String> {
    let ver = lk(&EXIT_CFG).verified.get(&n.id).cloned();
    ver.or_else(|| geo_iso(n)).or_else(|| region_iso(&n.region))
}

/// Какая страна действует для сайта: пин сайта → страна по умолчанию → Авто (None).
fn effective_country(key: &str) -> Option<String> {
    let c = lk(&EXIT_CFG);
    match c.pins.get(key).map(String::as_str) {
        Some(PIN_AUTO) => None,
        Some(cc) => Some(cc.to_string()),
        None => Some(c.default.clone()).filter(|s| !s.is_empty()),
    }
}

/// Узлы-выходы для сайта: фильтр по действующей стране (нет узлов → Авто),
/// узлы, уже бывшие выходом до «Новой личности», — в конец (единственный узел остаётся рабочим).
#[cfg_attr(not(feature = "p2p"), allow(dead_code))]
pub(crate) fn egress_nodes(host: &str) -> Vec<PqNode> {
    let key = site_key(host);
    let mut nodes = open_nodes();
    if let Some(cc) = effective_country(&key) {
        let f: Vec<PqNode> = nodes.iter().filter(|n| node_country(n).as_deref() == Some(cc.as_str())).cloned().collect();
        if !f.is_empty() {
            nodes = f;
        }
    }
    let avoid: Vec<String> = lk(&SITE_AVOID).get(&key).map(|v| v.iter().filter(|(_, at)| at.elapsed() < AVOID_TTL).map(|(id, _)| id.clone()).collect()).unwrap_or_default();
    if !avoid.is_empty() {
        // свежие узлы — первыми; уже бывшие выходом — в конец, давние раньше недавних (round-robin)
        let pos = |n: &PqNode| avoid.iter().position(|id| *id == n.id);
        let (mut used, mut fresh): (Vec<PqNode>, Vec<PqNode>) = nodes.into_iter().partition(|n| pos(n).is_some());
        used.sort_by_key(|n| pos(n));
        fresh.extend(used);
        nodes = fresh;
    }
    nodes
}

/// Сбросить живые туннели сайта и забыть его выход.
fn reset_site(key: &str) {
    lk(&SITE_EXIT).remove(key);
    site_kill(key).notify_waiters();
}

/// Страны выхода, где есть узлы pq-web `--open`, + текущий выход сайта, пин и страна по умолчанию.
#[tauri::command]
pub(crate) async fn webproxy_exit_countries(host: String) -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(move || {
        refresh_geo();
        let key = site_key(&host);
        let nodes = open_nodes();
        let mut cc: std::collections::BTreeMap<String, usize> = Default::default();
        for n in &nodes {
            if let Some(c) = node_country(n) {
                *cc.entry(c).or_default() += 1;
            }
        }
        let verified = load_verified();
        let cur_id = lk(&SITE_EXIT).get(&key).cloned().unwrap_or_default();
        let current = nodes.iter().find(|n| n.id == cur_id).map(|n| {
            let ver = verified.get(&n.id).cloned();
            let reg = region_iso(&n.region);
            let iso = ver.clone().or_else(|| geo_iso(n)).or(reg.clone());
            serde_json::json!({
                "id": n.id,
                "addr": n.addrs.first().cloned().unwrap_or_default(),
                "iso": iso,
                "region": n.region,
                "verified": ver.is_some(),
                "mismatch": matches!((&iso, &reg), (Some(g), Some(r)) if g != r),
            })
        });
        let effective = effective_country(&key);
        let direct = lk(&SITE_DIRECT).get(&key).is_some_and(|t| t.elapsed() < DIRECT_SHOW);
        serde_json::json!({
            "key": key,
            "direct": direct,
            "pinned": load_exit_pins().get(&key).cloned().unwrap_or_default(),
            "default": exit_default(),
            "effective": effective.clone().unwrap_or_default(),
            "fallback": effective.is_some_and(|e| !cc.contains_key(&e)),
            "current": current,
            "countries": cc.into_iter().map(|(iso, n)| serde_json::json!({ "iso": iso, "nodes": n })).collect::<Vec<_>>(),
        })
    })
    .await
    .unwrap_or_default()
}

/// Закрепить страну выхода за сайтом (`""` = как по умолчанию, `"*"` = Авто); живые туннели сайта рвутся.
#[tauri::command]
pub(crate) async fn webproxy_set_exit_country(host: String, iso: String) -> Result<(), String> {
    let key = site_key(&host);
    let iso = iso.trim().to_ascii_uppercase();
    if !iso.is_empty() && iso != PIN_AUTO && !valid_iso(&iso) {
        return Err("bad country code".into());
    }
    {
        let mut c = lk(&EXIT_CFG);
        if iso.is_empty() {
            c.pins.remove(&key);
        } else {
            c.pins.insert(key.clone(), iso);
        }
    }
    reset_site(&key);
    persist_exit_cfg().await
}

/// Страна выхода по умолчанию для всех сайтов (`""` = Авто); рвём туннели сайтов без своего пина.
#[tauri::command]
pub(crate) async fn webproxy_set_exit_default(iso: String) -> Result<(), String> {
    let iso = iso.trim().to_ascii_uppercase();
    if !iso.is_empty() && !valid_iso(&iso) {
        return Err("bad country code".into());
    }
    lk(&EXIT_CFG).default = iso;
    let pins = load_exit_pins();
    let keys: Vec<String> = lk(&SITE_KILL).keys().filter(|k| !pins.contains_key(*k)).cloned().collect();
    for k in keys {
        reset_site(&k);
    }
    persist_exit_cfg().await
}

/// Прервать идущую «Проверить мой IP».
#[tauri::command]
pub(crate) async fn webproxy_check_ip_cancel(host: String) {
    let n = lk(&CHECK_CANCEL).get(&site_key(&host)).cloned();
    if let Some(n) = n {
        n.notify_waiters();
    }
}

/// «Проверить мой IP»: HTTPS-запрос к сервису IP через ту же цепочку и тот же узел pq-web, что у сайта.
#[tauri::command]
pub(crate) async fn webproxy_check_ip(host: String) -> Result<serde_json::Value, String> {
    #[cfg(feature = "p2p")]
    {
        // общий потолок 25 с и кнопка «Отмена» — проверка не висит минутами
        let n = lk(&CHECK_CANCEL).entry(site_key(&host)).or_default().clone();
        let cancel = n.notified();
        tokio::select! {
            r = tokio::time::timeout(CHECK_DEADLINE, check_ip(&host)) => r.unwrap_or_else(|_| Err(format!("timeout {} s", CHECK_DEADLINE.as_secs()))),
            _ = cancel => Err("cancelled".into()),
        }
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = host;
        Err("Mycelium is not available in this build".into())
    }
}

/// Сервисы IP: (хост, путь, поле ISO страны, поле организации).
#[cfg(feature = "p2p")]
const IP_SERVICES: &[(&str, &str, &str, &str)] = &[("ifconfig.co", "/json", "country_iso", "asn_org"), ("ipinfo.io", "/json", "country", "org")];

#[cfg(feature = "p2p")]
async fn check_ip(host: &str) -> Result<serde_json::Value, String> {
    let key = site_key(host);
    let mut nodes = egress_nodes(host);
    if let Some(id) = lk(&SITE_EXIT).get(&key).cloned() {
        if let Some(i) = nodes.iter().position(|n| n.id == id) {
            let n = nodes.remove(i);
            nodes.insert(0, n);
        }
    }
    if nodes.is_empty() {
        return Err("no pq-web nodes with --open".into());
    }
    let mut last = String::new();
    let t0 = Instant::now();
    for (svc, path, iso_f, org_f) in IP_SERVICES {
        let left = CHECK_DEADLINE.saturating_sub(t0.elapsed());
        if left < Duration::from_secs(5) {
            break;
        }
        let r = async {
            let (node_id, stream) = tokio::time::timeout(left, crate::mycel_vpn::tcp_via_nodes(nodes.clone(), svc, 443)).await.map_err(|_| "circuit timeout".to_string())??;
            let body = tokio::time::timeout(Duration::from_secs(10), https_get(stream, svc, path)).await.map_err(|_| "https timeout".to_string())??;
            let v: serde_json::Value = serde_json::from_str(body.trim()).map_err(|e| format!("json: {e}"))?;
            Ok::<_, String>((node_id, v))
        }
        .await;
        let (node_id, v) = match r {
            Ok(x) => x,
            Err(e) => {
                last = format!("{svc}: {e}");
                continue;
            }
        };
        let ip = v["ip"].as_str().unwrap_or_default().to_string();
        let real = v[*iso_f].as_str().filter(|s| valid_iso(s)).map(|s| s.to_ascii_uppercase());
        let node = nodes.iter().find(|n| n.id == node_id);
        let expected = node.and_then(node_country);
        let announced: Vec<String> = node.map(|n| n.addrs.iter().filter_map(|a| a.parse::<SocketAddr>().ok().map(|s| s.ip().to_string()).or_else(|| a.parse::<std::net::IpAddr>().ok().map(|i| i.to_string()))).collect()).unwrap_or_default();
        let ip_match = announced.is_empty() || announced.iter().any(|a| *a == ip);
        let country_match = real.is_none() || expected.is_none() || real == expected;
        if let Some(r) = &real {
            // реальная страна выхода узла — дальше используется в чипах и фильтре страны
            let changed = lk(&EXIT_CFG).verified.insert(node_id.clone(), r.clone()).as_ref() != Some(r);
            if changed {
                let _ = persist_exit_cfg().await;
            }
        }
        crate::syslog::log("proxy", format!("check-ip {key}: {ip} {real:?} via {} (expected {expected:?})", &node_id[..node_id.len().min(10)]));
        return Ok(serde_json::json!({
            "ip": ip,
            "iso": real,
            "city": v["city"].as_str().unwrap_or_default(),
            "org": v[*org_f].as_str().unwrap_or_default(),
            "service": svc,
            "node": node_id,
            "expected": expected,
            "announced": announced,
            "ipMatch": ip_match,
            "countryMatch": country_match,
        }));
    }
    Err(last)
}

/// Минимальный HTTPS GET поверх произвольного потока (rustls + webpki-roots), `Connection: close`.
#[cfg(feature = "p2p")]
async fn https_get<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(stream: S, host: &str, path: &str) -> Result<String, String> {
    use std::sync::Arc;
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;
    let mut tls = tokio_rustls::TlsConnector::from(Arc::new(cfg)).connect(name, stream).await.map_err(|e| format!("tls: {e}"))?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: curl/8.5.0\r\nAccept: application/json\r\nConnection: close\r\n\r\n");
    tls.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    // сервер может закрыть без close_notify — берём то, что успели прочитать
    let r = (&mut tls).take(256 * 1024).read_to_end(&mut buf).await;
    if buf.is_empty() {
        r.map_err(|e| e.to_string())?;
        return Err("empty response".into());
    }
    let sep = buf.windows(4).position(|w| w == b"\r\n\r\n").ok_or("bad http response")?;
    let head = String::from_utf8_lossy(&buf[..sep]).to_ascii_lowercase();
    let body = &buf[sep + 4..];
    let status = head.lines().next().unwrap_or_default().to_string();
    if !status.contains(" 200") {
        return Err(status);
    }
    let body = if head.contains("transfer-encoding: chunked") { dechunk(body) } else { body.to_vec() };
    Ok(String::from_utf8_lossy(&body).to_string())
}

#[cfg(feature = "p2p")]
fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(i) = body.windows(2).position(|w| w == b"\r\n") {
        let size = String::from_utf8_lossy(&body[..i]);
        let Ok(n) = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16) else { break };
        let rest = &body[i + 2..];
        if n == 0 {
            break;
        }
        out.extend_from_slice(&rest[..rest.len().min(n)]);
        if rest.len() < n + 2 {
            break;
        }
        body = &rest[n + 2..];
    }
    out
}

/// «Новая личность»: следующий выход сайта — другой узел pq-web (новый IP), туннели сайта рвутся.
#[tauri::command]
pub(crate) async fn webproxy_new_identity(host: String) -> String {
    let key = site_key(&host);
    let prev = lk(&SITE_EXIT).get(&key).cloned().unwrap_or_default();
    if !prev.is_empty() {
        let mut av = lk(&SITE_AVOID);
        let v = av.entry(key.clone()).or_default();
        v.retain(|(id, at)| *id != prev && at.elapsed() < AVOID_TTL);
        v.push((prev.clone(), Instant::now()));
        if v.len() > 16 {
            v.remove(0);
        }
    }
    reset_site(&key);
    prev
}

/// Решение для bundle-хоста: режим → кэш → probe прямого пути.
async fn decide(bundle: &str, mode: &str, host: &str, port: u16, total: bool) -> Route {
    match mode {
        "relay" => return Route::Relay,
        "direct" => return Route::Direct,
        // total: группы auto тоже через узел, без probe
        _ if total => return Route::Relay,
        _ => {}
    }
    if let Some((r, until, _)) = st().routes.get(bundle) {
        if *until > Instant::now() {
            return *r;
        }
    }
    let (route, reason) = match probe_direct(host, port).await {
        Ok(()) => (Route::Direct, "ok".to_string()),
        Err(why) => (Route::Relay, why),
    };
    let ttl = if route == Route::Direct { CACHE_OK } else { CACHE_BLOCKED };
    crate::syslog::log("proxy", format!("probe {host}: {reason} → {:?} (bundle {bundle})", route));
    let app = {
        let mut s = st();
        s.routes.insert(bundle.to_string(), (route, Instant::now() + ttl, reason));
        s.app.clone()
    };
    if route == Route::Relay {
        emit_route(host, bundle, Route::Relay, "", "");
        // HUD «Routing via NETFORY Relay…» во вкладке, если этот хост — её главный документ
        if let Some(app) = app {
            crate::websites::on_bundle_direct_failed(&app, host);
        }
    }
    route
}

/// Probe: DNS → TCP → TLS ClientHello с SNI → любые байты назад (ServerHello/alert) ≤ 2.5 с.
/// Классификация: dns-fail | dns-poison | rst | timeout | tls-stall.
async fn probe_direct(host: &str, port: u16) -> Result<(), String> {
    let addrs: Vec<SocketAddr> = tokio::time::timeout(PROBE_TIMEOUT, tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| "dns-timeout".to_string())?
        .map_err(|_| "dns-fail".to_string())?
        .collect();
    let Some(addr) = addrs.first().copied() else { return Err("dns-fail".into()) };
    let ip = addr.ip();
    let poisoned = match ip {
        std::net::IpAddr::V4(v) => v.is_loopback() || v.is_private() || v.is_unspecified() || v.is_link_local() || (v.octets()[0] == 198 && (18..=19).contains(&v.octets()[1])),
        std::net::IpAddr::V6(v) => v.is_loopback() || v.is_unspecified(),
    };
    if poisoned {
        return Err("dns-poison".into());
    }
    let t0 = Instant::now();
    let mut s = match tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(addr)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused || e.kind() == std::io::ErrorKind::ConnectionReset => return Err("rst".into()),
        Ok(Err(e)) => return Err(format!("tcp:{}", e.kind().to_string().to_lowercase())),
        Err(_) => return Err("timeout".into()),
    };
    if port != 443 {
        return Ok(());
    }
    let hello = client_hello(host);
    s.write_all(&hello).await.map_err(|_| "tls-write".to_string())?;
    let left = PROBE_TIMEOUT.saturating_sub(t0.elapsed()).max(Duration::from_millis(300));
    let mut buf = [0u8; 16];
    match tokio::time::timeout(left, s.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => Ok(()),
        Ok(Ok(_)) => Err("rst".into()),
        Ok(Err(_)) => Err("rst".into()),
        Err(_) => Err("tls-stall".into()),
    }
}

/// Минимальный TLS ClientHello (TLS 1.2 record, версии 1.3+1.2, SNI, x25519 key_share).
fn client_hello(host: &str) -> Vec<u8> {
    let mut rnd = [0u8; 64];
    for (i, b) in rnd.iter_mut().enumerate() {
        *b = (Instant::now().elapsed().as_nanos() as u8) ^ (i as u8).wrapping_mul(97) ^ 0x5a;
    }
    let sni = host.as_bytes();
    let mut ext = Vec::new();
    // server_name
    let mut sn = Vec::new();
    sn.extend_from_slice(&((sni.len() + 3) as u16).to_be_bytes());
    sn.push(0);
    sn.extend_from_slice(&(sni.len() as u16).to_be_bytes());
    sn.extend_from_slice(sni);
    push_ext(&mut ext, 0x0000, &sn);
    push_ext(&mut ext, 0x000a, &[0x00, 0x04, 0x00, 0x1d, 0x00, 0x17]); // groups: x25519, secp256r1
    push_ext(&mut ext, 0x000d, &[0x00, 0x08, 0x04, 0x03, 0x08, 0x04, 0x04, 0x01, 0x05, 0x01]); // sig algs
    push_ext(&mut ext, 0x002b, &[0x04, 0x03, 0x04, 0x03, 0x03]); // supported_versions 1.3, 1.2
    let mut ks = Vec::new();
    ks.extend_from_slice(&[0x00, 0x24, 0x00, 0x1d, 0x00, 0x20]);
    ks.extend_from_slice(&rnd[32..64]);
    push_ext(&mut ext, 0x0033, &ks); // key_share x25519
    push_ext(&mut ext, 0x0010, &[0x00, 0x0c, 0x02, 0x68, 0x32, 0x08, 0x68, 0x74, 0x74, 0x70, 0x2f, 0x31, 0x2e, 0x31]); // ALPN h2, http/1.1
    let mut body = Vec::new();
    body.extend_from_slice(&[0x03, 0x03]);
    body.extend_from_slice(&rnd[..32]);
    body.push(0); // session id
    body.extend_from_slice(&[0x00, 0x06, 0x13, 0x01, 0x13, 0x02, 0xc0, 0x2f]); // ciphers
    body.extend_from_slice(&[0x01, 0x00]); // compression
    body.extend_from_slice(&(ext.len() as u16).to_be_bytes());
    body.extend_from_slice(&ext);
    let mut hs = vec![0x01, 0, 0, 0];
    let l = body.len();
    hs[1] = (l >> 16) as u8;
    hs[2] = (l >> 8) as u8;
    hs[3] = l as u8;
    hs.extend_from_slice(&body);
    let mut rec = vec![0x16, 0x03, 0x01];
    rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
    rec.extend_from_slice(&hs);
    rec
}

fn push_ext(out: &mut Vec<u8>, ty: u16, data: &[u8]) {
    out.extend_from_slice(&ty.to_be_bytes());
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
}

/// `fam` — «v6»/«v4»: каким семейством узел pq-web вышел к сайту (пусто — узел старый/неизвестно).
fn emit_route(host: &str, bundle: &str, route: Route, node: &str, fam: &str) {
    let mut s = st();
    if route == Route::Relay && !node.is_empty() {
        s.active.insert(host.to_string(), node.to_string());
        if !fam.is_empty() {
            s.active_fam.insert(host.to_string(), fam.to_string());
        }
    }
    if let Some(app) = &s.app {
        let _ = app.emit("webproxy-route", serde_json::json!({ "host": host, "bundle": bundle, "route": route, "node": node, "fam": fam }));
    }
}

// ---------------------------------------------------------------- pq-web tunnel
/// (узел, живое соединение — держим на всё время туннеля, иначе drop закрывает QUIC-коннект)
#[cfg(feature = "p2p")]
type TunnelIo = (String, iroh::endpoint::Connection, iroh::endpoint::RecvStream, iroh::endpoint::SendStream, String);

/// Одно QUIC-соединение на узел pq-web (v1.135.192): браузер открывает 6–10 TCP-коннектов
/// параллельно — каждый стал бы отдельным handshake через relay/hole-punch (медленно, часть
/// «timed out»). Теперь стримы мультиплексируются в одном соединении.
#[cfg(feature = "p2p")]
static CONNS: Lazy<tokio::sync::Mutex<HashMap<String, iroh::endpoint::Connection>>> = Lazy::new(|| tokio::sync::Mutex::new(HashMap::new()));
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REPLY_TIMEOUT: Duration = Duration::from_secs(12);

/// Узлы по рангу: ok + минимальный RTT; ручные/verified при близком RTT выше непроверенных;
/// без health — в конец (но пробуем).
/// `need_open` — хост вне групп (dVPN total): годятся только узлы с `--open`
/// (ручные узлы без сведений — тоже пробуем).
pub(crate) fn best_nodes(need_open: bool) -> Vec<PqNode> {
    let nodes = load_nodes();
    let health = st().health.clone();
    let v6 = v6_mode();
    let mut ranked: Vec<(u64, PqNode)> = nodes
        .into_iter()
        .filter(|(n, source)| !need_open || n.open || *source == "manual")
        .filter(|(n, _)| v6 != "only" || n.ipv6)
        .map(|(n, source)| {
            let h = health.iter().find(|h| h.id == n.id);
            let score = match h {
                Some(h) if h.ok => h.rtt_ms,
                Some(_) => 1_000_000,
                None => 500_000,
            };
            let trust = if source == "manual" || n.verified { 0 } else { 40 };
            let v6_pen = if v6 == "off" || n.ipv6 { 0 } else { V6_PENALTY };
            (score + trust + v6_pen, n)
        })
        .collect();
    ranked.sort_by_key(|(s, _)| *s);
    ranked.into_iter().map(|(_, n)| n).collect()
}

#[cfg(feature = "p2p")]
/// Компактная сводка путей соединения: `[*]relay:https://…/ 48ms | ip:1.2.3.4:7843 12ms` (`*` = выбранный).
fn paths_summary(conn: &iroh::endpoint::Connection) -> String {
    let paths = conn.paths();
    let mut parts = Vec::new();
    for p in paths.iter() {
        parts.push(format!(
            "{}{}:{:?} {}ms",
            if p.is_selected() { "*" } else { "" },
            if p.is_relay() { "relay" } else { "ip" },
            p.remote_addr(),
            p.rtt().as_millis()
        ));
    }
    if parts.is_empty() { "no paths".into() } else { parts.join(" | ") }
}

/// Живое соединение с узлом: из кэша либо новый connect (таймаут 15 с). Если у узла известны
/// ip:port/relay (из трекера или ручного ввода), набираем по ним напрямую — без discovery.
#[cfg(feature = "p2p")]
async fn conn_to(node: &PqNode) -> Result<iroh::endpoint::Connection, String> {
    let node_id = node.id.as_str();
    let ep = crate::p2p::endpoint_now().ok_or("p2p узел ещё не поднят")?;
    let eid: iroh::EndpointId = node_id.parse().map_err(|_| "bad EndpointId".to_string())?;
    let mut target = iroh::EndpointAddr::new(eid);
    for a in &node.addrs {
        if let Ok(sa) = a.parse::<std::net::SocketAddr>() {
            target = target.with_ip_addr(sa);
        }
    }
    if let Ok(u) = node.relay.parse::<iroh::RelayUrl>() {
        target = target.with_relay_url(u);
    }
    {
        let mut cache = CONNS.lock().await;
        let alive = cache.get(node_id).map(|c| c.close_reason().is_none());
        match alive {
            Some(true) => return Ok(cache[node_id].clone()),
            Some(false) => {
                cache.remove(node_id);
            }
            None => {}
        }
    }
    let t0 = Instant::now();
    let conn = match tokio::time::timeout(CONNECT_TIMEOUT, ep.connect(target, PQWEB_ALPN)).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return Err(format!("connect: {e}")),
        Err(_) => return Err("connect timeout (15 с)".into()),
    };
    crate::syslog::log(
        "proxy",
        format!(
            "pq-web {}: QUIC-соединение за {} мс (dial: {} {}), paths: {}",
            &node_id[..node_id.len().min(10)],
            t0.elapsed().as_millis(),
            if node.addrs.is_empty() { "discovery".to_string() } else { node.addrs.join(",") },
            node.relay,
            paths_summary(&conn)
        ),
    );
    CONNS.lock().await.insert(node_id.to_string(), conn.clone());
    Ok(conn)
}

/// Сбросить кэшированное соединение (после ошибки стрима/таймаута ответа).
#[cfg(feature = "p2p")]
async fn drop_conn(node_id: &str) {
    if let Some(c) = CONNS.lock().await.remove(node_id) {
        c.close(1u32.into(), b"reset");
    }
}

/// Запрос-ответ по новому bi-стриму: `line` → первая строка ответа. При ошибке соединение
/// выбрасывается из кэша, чтобы следующий вызов переподключился.
#[cfg(feature = "p2p")]
async fn request(node: &PqNode, line: &str) -> Result<(iroh::endpoint::Connection, iroh::endpoint::RecvStream, iroh::endpoint::SendStream, String), String> {
    let node_id = node.id.as_str();
    let conn = conn_to(node).await?;
    let res: Result<_, String> = async {
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| format!("open_bi: {e}"))?;
        send.write_all(line.as_bytes()).await.map_err(|e| format!("write: {e}"))?;
        let reply = read_line(&mut recv).await?;
        Ok((recv, send, reply))
    }
    .await;
    match res {
        Ok((recv, send, reply)) => Ok((conn, recv, send, reply)),
        Err(e) => {
            crate::syslog::log("proxy", format!("pq-web {}: {e}; paths: {}", &node_id[..node_id.len().min(10)], paths_summary(&conn)));
            drop_conn(node_id).await;
            Err(e)
        }
    }
}

#[cfg(feature = "p2p")]
async fn open_tunnel(host: &str, port: u16, need_open: bool) -> Result<TunnelIo, String> {
    let mut last = if v6_mode() == "only" {
        "нет узлов pq-web с IPv6: включён фильтр «только v6» (Settings → PQ Web)".to_string()
    } else if need_open {
        "нет узлов pq-web с --open для режима dVPN (Settings → PQ Web)".to_string()
    } else {
        "нет узлов pq-web (Settings → PQ Web)".to_string()
    };
    for node in best_nodes(need_open).into_iter().take(3) {
        // одна повторная попытка на узел: первая ошибка могла быть протухшим соединением
        for attempt in 0..2 {
            match request(&node, &format!("NF1 CONNECT {host}:{port}\n")).await {
                Ok((conn, recv, send, reply)) => {
                    // pq-web ≥0.5.0 отвечает «OK v6»/«OK v4» — семейство, по которому узел вышел к сайту
                    let r = reply.trim();
                    if r == "OK" || r.starts_with("OK ") {
                        let fam = r.strip_prefix("OK").unwrap_or("").trim().to_string();
                        return Ok((node.id.clone(), conn, recv, send, fam));
                    }
                    // явный отказ узла (ERR …) — повторять бессмысленно
                    last = format!("{}: {}", node.id, reply.trim());
                    break;
                }
                Err(e) => {
                    last = format!("{}: {e}", node.id);
                    if attempt == 0 {
                        crate::syslog::log("proxy", format!("{host}: {last} → повтор"));
                    }
                }
            }
        }
    }
    Err(last)
}

#[cfg(not(feature = "p2p"))]
async fn open_tunnel(_host: &str, _port: u16, _need_open: bool) -> Result<(String, (), tokio::io::Empty, tokio::io::Sink, String), String> {
    Err("p2p feature disabled".into())
}

#[cfg(feature = "p2p")]
async fn read_line(recv: &mut iroh::endpoint::RecvStream) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    loop {
        let n = tokio::time::timeout(REPLY_TIMEOUT, AsyncReadExt::read(recv, &mut b)).await.map_err(|_| "нет ответа от узла (12 с)".to_string())?.map_err(|e| e.to_string())?;
        if n == 0 || b[0] == b'\n' || buf.len() > 512 {
            break;
        }
        buf.push(b[0]);
    }
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// PING всех узлов pq-web → health (RTT), событие `webproxy-relays`.
async fn check_nodes() {
    let nodes = load_nodes();
    // параллельно: connect до 15 с на узел, последовательно UI ждал бы минуту
    let mut tasks = Vec::new();
    for (n, source) in nodes {
        tasks.push(tauri::async_runtime::spawn(async move {
            let mut h = NodeHealth { id: n.id.clone(), region: n.region.clone(), source: source.into(), verified: n.verified, addr: n.addrs.first().cloned().unwrap_or_default(), open: n.open, ipv6: n.ipv6, checked_at: now_secs(), ..Default::default() };
            match ping_node(&n).await {
                Ok(rtt) => {
                    h.ok = true;
                    h.rtt_ms = rtt;
                }
                Err(e) => h.error = e,
            }
            crate::syslog::log("proxy", format!("pq-web {} [{}]: {}", n.id, n.region, if h.ok { format!("{} ms", h.rtt_ms) } else { h.error.clone() }));
            h
        }));
    }
    let mut out = Vec::new();
    for t in tasks {
        if let Ok(h) = t.await {
            out.push(h);
        }
    }
    let app = {
        let mut s = st();
        s.health = out.clone();
        s.app.clone()
    };
    if let Some(app) = app {
        let _ = app.emit("webproxy-relays", serde_json::json!(out));
    }
}

#[cfg(feature = "p2p")]
async fn ping_node(node: &PqNode) -> Result<u64, String> {
    // RTT = только PING→PONG по уже установленному соединению (connect — отдельно, в syslog)
    let _ = conn_to(node).await?;
    let t0 = Instant::now();
    let (_conn, _recv, mut send, reply) = request(node, "NF1 PING\n").await?;
    let _ = send.finish();
    if !reply.starts_with("PONG") {
        return Err(format!("unexpected: {reply}"));
    }
    Ok(t0.elapsed().as_millis() as u64)
}

#[cfg(not(feature = "p2p"))]
async fn ping_node(_node: &PqNode) -> Result<u64, String> {
    Err("p2p feature disabled".into())
}

// ---------------------------------------------------------------- tracker list
/// `GET /pqweb` на трекерах (по очереди, ureq в spawn_blocking): первый валидный
/// непустой список → sled. Подпись: если ключ оператора известен (env/TOFU) —
/// обязана сходиться, иначе список отбрасывается; без ключа — unsigned-режим.
/// Возвращает true, если набор узлов изменился (нужен check_nodes).
async fn fetch_tracker_nodes() -> bool {
    let trackers = crate::p2p::get_trackers_internal();
    if trackers.is_empty() {
        return false;
    }
    // Ok(list) | Err(последняя причина для UI)
    let res: Result<AutoNodes, String> = tokio::task::spawn_blocking(move || {
        let mut why = String::from("трекеры недоступны");
        for (i, t) in trackers.into_iter().enumerate() {
            // в UI адреса трекеров не показываем — только порядковый номер
            let tl = format!("трекер #{}", i + 1);
            let url = format!("{}/pqweb", crate::p2p::normalize_tracker(&t));
            let resp = match ureq::get(&url).timeout(TRACKER_TIMEOUT).call() {
                Ok(r) => r,
                Err(e) => {
                    why = format!("{tl}: {}", e.kind()); // без URL — ureq печатает адрес трекера в тексте ошибки
                    continue;
                }
            };
            let Ok(txt) = resp.into_string() else { continue };
            let r = match serde_json::from_str::<PqWebResp>(&txt) {
                Ok(r) => r,
                Err(_) => {
                    why = format!("{tl}: /pqweb — не JSON (старый трекер < 1.6.6?)");
                    continue;
                }
            };
            let nodes: Vec<PqNode> = r
                .nodes
                .into_iter()
                .map(|n| PqNode {
                    id: n.id.trim().to_ascii_lowercase(),
                    region: n.region.trim().to_string(),
                    verified: n.verified,
                    ver: n.ver.chars().take(24).collect(),
                    addrs: n.addrs.iter().filter(|a| a.parse::<std::net::SocketAddr>().is_ok()).take(6).cloned().collect(),
                    relay: n.relay.trim().to_string(),
                    open: n.open,
                    ipv6: n.ipv6,
                    mycelium: n.mycelium,
                })
                .filter(|n| n.id.len() >= 32 && n.id.chars().all(|c| c.is_ascii_alphanumeric()))
                .collect();
            if nodes.is_empty() {
                why = format!("{tl}: узлов pq-web пока нет");
                continue;
            }
            let mut canon = vec!["pqweb".to_string(), r.ts.to_string()];
            canon.extend(nodes.iter().map(|n| format!("{}@{}", n.id, n.region)));
            let key_known = crate::p2p::operator_pubkey().is_some();
            if !crate::p2p::operator_seeders_ok(&canon, &r.sig) {
                why = if r.sig.is_empty() {
                    format!("{tl}: список без подписи, а ключ оператора закреплён (на трекере не задан OPERATOR_SECRET)")
                } else {
                    format!("{tl}: подпись оператора не сходится")
                };
                crate::syslog::log("proxy", format!("/pqweb отброшен: {why}"));
                continue;
            }
            return Ok(AutoNodes { nodes, signed: key_known && !r.sig.is_empty(), tracker: t, at: now_secs(), stale: false, error: String::new() });
        }
        Err(why)
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    let prev = load_auto();
    let auto = match res {
        Ok(a) => a,
        Err(why) => {
            // трекеры недоступны/пусты — остаёмся на кэше из sled (список не трогаем), помечаем stale
            let mut p = prev.clone();
            p.stale = !p.nodes.is_empty();
            p.error = why.clone();
            if let Ok(v) = serde_json::to_vec(&p) {
                let _ = crate::DB.insert(KEY_NODES_AUTO.as_bytes(), v);
            }
            if !prev.nodes.is_empty() && !prev.stale {
                crate::syslog::log("proxy", format!("{why} — используем кэш узлов pq-web ({} шт., {} мин)", prev.nodes.len(), now_secs().saturating_sub(prev.at) / 60));
            } else if prev.error != why {
                crate::syslog::log("proxy", format!("список pq-web с трекера не получен: {why}"));
            }
            return false;
        }
    };
    if let Ok(v) = serde_json::to_vec(&auto) {
        let _ = crate::DB.insert(KEY_NODES_AUTO.as_bytes(), v);
    }
    // релеи Mycelium NETFORY (pq-web --mycelium) — bootstrap mushroom-цепочек без ручного ввода
    crate::mycel::on_tracker_nodes();
    let same = prev.nodes.len() == auto.nodes.len() && prev.nodes.iter().zip(&auto.nodes).all(|(a, b)| a.id == b.id);
    if !same || prev.stale {
        crate::syslog::log(
            "proxy",
            format!("узлы pq-web с трекера {}: {} шт. ({})", auto.tracker, auto.nodes.len(), if auto.signed { "подписано" } else { "без подписи" }),
        );
    }
    !same
}

// ---------------------------------------------------------------- commands
#[derive(Serialize)]
pub(crate) struct BundleStatus {
    name: String,
    hosts: Vec<String>,
    mode: String,
    route: Option<Route>,
    reason: String,
}

#[derive(Serialize)]
pub(crate) struct Status {
    enabled: bool,
    port: u16,
    bundles: Vec<BundleStatus>,
    nodes: Vec<NodeHealth>,
    /// host → узел для активных relay-туннелей
    active: HashMap<String, String>,
    /// host → «v6»/«v4» — семейство исходящего соединения узла
    #[serde(rename = "activeFam")]
    active_fam: HashMap<String, String>,
    /// ручные узлы (Settings)
    #[serde(rename = "manualNodes")]
    manual_nodes: Vec<PqNode>,
    /// список с трекера (подписанный)
    auto: AutoNodes,
    /// dVPN total: все сайты через pq-web
    total: bool,
    /// фильтр IPv6-узлов: off | prefer | only
    #[serde(rename = "v6Mode")]
    v6_mode: String,
}

static FETCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Открыли карточку/обновили статус — если список с трекера старше 2 мин, тихо обновить в фоне.
fn refresh_auto_if_stale(auto: &AutoNodes) {
    use std::sync::atomic::Ordering;
    if !enabled() || now_secs().saturating_sub(auto.at) < 120 || FETCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async {
        if fetch_tracker_nodes().await {
            check_nodes().await;
        } else if let Some(app) = st().app.clone() {
            let _ = app.emit("webproxy-relays", serde_json::json!([]));
        }
        FETCHING.store(false, Ordering::SeqCst);
    });
}

#[tauri::command]
pub(crate) fn webproxy_status() -> Status {
    let b = load_bundles();
    let auto = load_auto();
    refresh_auto_if_stale(&auto);
    let s = st();
    let bundles = b
        .bundles
        .iter()
        .map(|(name, v)| {
            let (route, reason) = match s.routes.get(name) {
                Some((r, until, why)) if *until > Instant::now() => (Some(*r), why.clone()),
                _ => (None, String::new()),
            };
            BundleStatus { name: name.clone(), hosts: v.hosts.clone(), mode: v.mode.clone(), route, reason }
        })
        .collect();
    Status {
        enabled: enabled(),
        port: s.port,
        bundles,
        nodes: s.health.clone(),
        active: s.active.clone(),
        active_fam: s.active_fam.clone(),
        manual_nodes: load_manual_nodes(),
        auto,
        total: total(),
        v6_mode: v6_mode(),
    }
}

/// Фильтр v6-узлов для dVPN: `off` | `prefer` | `only`. Сбрасывает кэш маршрутов —
/// применяется к следующим туннелям сразу, старое поведение (без фильтра) = `off`.
#[tauri::command]
pub(crate) fn webproxy_set_v6(mode: String) -> Result<(), String> {
    if !matches!(mode.as_str(), "off" | "prefer" | "only") {
        return Err("mode: off|prefer|only".into());
    }
    crate::DB.insert(KEY_V6.as_bytes(), mode.as_bytes()).map_err(|e| e.to_string())?;
    let app = {
        let mut s = st();
        s.routes.clear();
        s.relay_failed.clear();
        s.app.clone()
    };
    crate::syslog::log("proxy", format!("pq-web фильтр IPv6: {mode}"));
    if let Some(app) = app {
        let _ = app.emit("webproxy-relays", serde_json::json!([]));
    }
    Ok(())
}

/// Включился Mycelium — dVPN «все сайты через pq-web» выключаем (он покрыт Mycelium hops=1+).
pub(crate) fn force_total_off() {
    if !total() {
        return;
    }
    let _ = crate::DB.insert(KEY_TOTAL.as_bytes(), b"0".as_slice());
    let app = {
        let mut s = st();
        s.routes.clear();
        s.app.clone()
    };
    crate::syslog::log("proxy", "dVPN total: выкл — включён Mycelium");
    if let Some(app) = app {
        let _ = app.emit("webproxy-total", serde_json::json!({ "on": false }));
    }
}

/// Тумблер dVPN (иконка в адресной строке / Settings): применяется мгновенно — PAC уже
/// шлёт весь трафик в роутер, меняется только решение relay/direct. Кэш маршрутов сбрасывается.
#[tauri::command]
pub(crate) fn webproxy_set_total(on: bool) -> Result<(), String> {
    if on && crate::mycel::enabled() {
        return Err("dVPN на паузе: включён Mycelium (двойной транспорт удваивает задержку)".into());
    }
    crate::DB.insert(KEY_TOTAL.as_bytes(), if on { b"1".as_slice() } else { b"0".as_slice() }).map_err(|e| e.to_string())?;
    let app = {
        let mut s = st();
        s.routes.clear();
        s.relay_failed.clear();
        s.app.clone()
    };
    crate::syslog::log("proxy", format!("dVPN total: {}", if on { "ВКЛ — все сайты через pq-web" } else { "выкл" }));
    if let Some(app) = app {
        let _ = app.emit("webproxy-total", serde_json::json!({ "on": on }));
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn webproxy_set_mode(bundle: String, mode: String) -> Result<(), String> {
    if !matches!(mode.as_str(), "auto" | "relay" | "direct") {
        return Err("mode: auto|relay|direct".into());
    }
    let mut b = load_bundles();
    let entry = b.bundles.get_mut(&bundle).ok_or("no such bundle")?;
    entry.mode = mode;
    crate::DB.insert(KEY_BUNDLES.as_bytes(), serde_json::to_vec(&b).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    st().routes.remove(&bundle);
    Ok(())
}

/// Список узлов pq-web (JSON `[{"id":"…","region":"NL"}]`); сразу перепроверка.
#[tauri::command]
pub(crate) async fn webproxy_set_nodes(nodes: Vec<PqNode>) -> Result<(), String> {
    let clean: Vec<PqNode> = nodes
        .into_iter()
        .map(|n| PqNode {
            id: n.id.trim().to_ascii_lowercase(),
            region: n.region.trim().to_string(),
            verified: false,
            ver: String::new(),
            addrs: n.addrs.iter().filter(|a| a.parse::<std::net::SocketAddr>().is_ok()).take(6).cloned().collect(),
            relay: n.relay.trim().to_string(),
            open: n.open,
            ipv6: n.ipv6,
            mycelium: n.mycelium,
        })
        .filter(|n| !n.id.is_empty())
        .collect();
    // UI показывает только id [регион]: ip:port/relay, введённые раньше, сохраняем по id
    let prev = load_manual_nodes();
    let clean: Vec<PqNode> = clean
        .into_iter()
        .map(|mut n| {
            if let Some(p) = prev.iter().find(|p| p.id.eq_ignore_ascii_case(&n.id)) {
                if n.addrs.is_empty() {
                    n.addrs = p.addrs.clone();
                }
                if n.relay.is_empty() {
                    n.relay = p.relay.clone();
                }
                n.open = n.open || p.open;
                n.ipv6 = n.ipv6 || p.ipv6;
                n.mycelium = n.mycelium || p.mycelium;
            }
            n
        })
        .collect();
    crate::DB.insert(KEY_NODES.as_bytes(), serde_json::to_vec(&clean).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    check_nodes().await;
    Ok(())
}

#[tauri::command]
pub(crate) fn webproxy_set_enabled(enabled: bool) -> Result<(), String> {
    crate::DB.insert(KEY_ENABLED.as_bytes(), if enabled { b"1".as_slice() } else { b"0".as_slice() }).map_err(|e| e.to_string())?;
    Ok(())
}

/// Сбросить кэш маршрутов, обновить список с трекера и перепроверить узлы.
#[tauri::command]
pub(crate) async fn webproxy_recheck() {
    {
        let mut s = st();
        s.routes.clear();
        s.relay_failed.clear();
    }
    let _ = fetch_tracker_nodes().await;
    check_nodes().await;
}
