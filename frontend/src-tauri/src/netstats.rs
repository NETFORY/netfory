//! Диагностика соединений iroh (Настройки → Сеть → Диагностика): открытые соединения и
//! открыто/закрыто/смены реле за минуту по endpoint'ам (основной, медиа, Mycelium) + ошибки
//! connect за минуту — чтобы ловить «шторм соединений» (реле «no available capacity»).
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);
const NAMES: [&str; 3] = ["main", "media", "mycelium"];

#[derive(Clone, Copy, Default)]
struct Snap {
    opened: u64,
    closed: u64,
    relay_change: u64,
    holepunch: u64,
    direct: u64,
}

static HIST: Mutex<VecDeque<(Instant, [Option<Snap>; 3])>> = Mutex::new(VecDeque::new());
static ERRORS: Mutex<VecDeque<(Instant, bool)>> = Mutex::new(VecDeque::new());
static LAST_ERR: Mutex<String> = Mutex::new(String::new());
static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn is_relay_err(e: &str) -> bool {
    let l = e.to_ascii_lowercase();
    l.contains("relay") || l.contains("capacity")
}

/// Неудачный connect (кошелёк/узлы sth/чат) — в счётчик ошибок за минуту.
pub(crate) fn note_connect_error(e: &str) {
    let now = Instant::now();
    let mut q = ERRORS.lock().unwrap();
    q.push_back((now, is_relay_err(e)));
    while q.front().is_some_and(|(t, _)| now.duration_since(*t) > WINDOW) || q.len() > 5000 {
        q.pop_front();
    }
    *LAST_ERR.lock().unwrap() = e.chars().take(200).collect();
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EpStats {
    pub name: &'static str,
    pub node_id: String,
    pub up: bool,
    /// Открыто сейчас (opened − closed за всё время endpoint'а).
    pub open: u64,
    pub opened_min: u64,
    pub closed_min: u64,
    pub relay_changes_min: u64,
    pub holepunch_min: u64,
    /// Соединений, ставших прямыми (без реле), за всё время.
    pub direct: u64,
    pub home_relay: String,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnStats {
    pub endpoints: Vec<EpStats>,
    pub connect_errors_min: u64,
    pub relay_errors_min: u64,
    pub last_error: String,
    pub storm: bool,
    pub window_secs: u64,
}

#[cfg(feature = "p2p")]
fn endpoints() -> [Option<iroh::Endpoint>; 3] {
    [crate::p2p::endpoint_now(), crate::p2p::media_endpoint_now(), crate::mycel::endpoint_now()]
}

#[cfg(feature = "p2p")]
fn snap(ep: &iroh::Endpoint) -> Snap {
    let m = &ep.metrics().socket;
    Snap { opened: m.num_conns_opened.get(), closed: m.num_conns_closed.get(), relay_change: m.relay_home_change.get(), holepunch: m.holepunch_attempts.get(), direct: m.num_conns_direct.get() }
}

#[cfg(feature = "p2p")]
fn sample() -> [Option<Snap>; 3] {
    let eps = endpoints();
    [eps[0].as_ref().map(snap), eps[1].as_ref().map(snap), eps[2].as_ref().map(snap)]
}

/// Снимок счётчиков раз в 5 с (история ~минута) — дельты «за минуту» без опроса из UI.
pub(crate) fn spawn_sampler() {
    #[cfg(feature = "p2p")]
    {
        if STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        tokio::spawn(async move {
            loop {
                push_sample();
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }
}

#[cfg(feature = "p2p")]
fn push_sample() {
    let now = Instant::now();
    let s = sample();
    let mut h = HIST.lock().unwrap();
    h.push_back((now, s));
    while h.len() > 2 && h.front().is_some_and(|(t, _)| now.duration_since(*t) > WINDOW + Duration::from_secs(5)) {
        h.pop_front();
    }
}

pub(crate) fn stats() -> ConnStats {
    let now = Instant::now();
    let (errs, relay_errs) = {
        let q = ERRORS.lock().unwrap();
        let recent: Vec<bool> = q.iter().filter(|(t, _)| now.duration_since(*t) <= WINDOW).map(|(_, r)| *r).collect();
        (recent.len() as u64, recent.iter().filter(|r| **r).count() as u64)
    };
    let mut out = ConnStats { connect_errors_min: errs, relay_errors_min: relay_errs, last_error: LAST_ERR.lock().unwrap().clone(), window_secs: WINDOW.as_secs(), ..Default::default() };
    #[cfg(feature = "p2p")]
    {
        let eps = endpoints();
        let cur = sample();
        let h = HIST.lock().unwrap();
        if let Some((t0, _)) = h.front() {
            out.window_secs = now.duration_since(*t0).as_secs().clamp(5, WINDOW.as_secs());
        }
        for i in 0..3 {
            let Some(ep) = eps[i].as_ref() else {
                out.endpoints.push(EpStats { name: NAMES[i], ..Default::default() });
                continue;
            };
            let c = cur[i].unwrap_or_default();
            let base = h.iter().find_map(|(_, s)| s[i]).unwrap_or(c);
            out.endpoints.push(EpStats {
                name: NAMES[i],
                node_id: ep.id().to_string(),
                up: true,
                open: c.opened.saturating_sub(c.closed),
                opened_min: c.opened.saturating_sub(base.opened),
                closed_min: c.closed.saturating_sub(base.closed),
                relay_changes_min: c.relay_change.saturating_sub(base.relay_change),
                holepunch_min: c.holepunch.saturating_sub(base.holepunch),
                direct: c.direct,
                home_relay: ep.addr().relay_urls().next().map(|u| u.to_string()).unwrap_or_default(),
            });
        }
    }
    let opened: u64 = out.endpoints.iter().map(|e| e.opened_min).sum();
    let relay_changes: u64 = out.endpoints.iter().map(|e| e.relay_changes_min).sum();
    out.storm = opened >= 40 || relay_errs >= 10 || errs >= 20 || relay_changes >= 6;
    out
}

#[tauri::command]
pub(crate) async fn net_conn_stats() -> ConnStats {
    stats()
}
