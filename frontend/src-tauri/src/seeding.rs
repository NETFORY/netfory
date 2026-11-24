// Author: TechnoL0g
// Project: SmartNet Core
//
// ============================================================
//  seeding.rs — "Earn STH" desktop background-seeding control plane
//
//  Embeds the headless-seeder lifecycle inside the GUI shell under an explicit
//  user toggle. When ON it (a) sends signed 5-min availability heartbeats to the
//  trackers (the client already re-announces & serves its installed dApps over
//  the P2P layer), and (b) pushes a 10s live-metrics broadcast to the UI. When
//  OFF it cancels both loops; user-installed apps are NEVER touched.
//
//  Generation counter = graceful demobilization: each toggle bumps SEEDING_GEN;
//  a loop exits as soon as its captured generation is stale.
// ============================================================

use crate::crypto::{keys_from_material, sha256};
use crate::{ensure_data_dir, DB, SESSION_SEED};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

static SEEDING_GEN: AtomicU64 = AtomicU64::new(0);

const KEY_ENABLED: &[u8] = b"cfg:desktop_seeding_enabled";
const KEY_LIMIT: &[u8] = b"cfg:desktop_seeding_limit_gb";
/// v1.135.185: стартовый бюджет диска для новых пользователей — 3 ГБ.
const DEFAULT_LIMIT_GB: u64 = 3;

// Earnings model (gamified estimate, Phase G+).
const BASE_NETWORK_REWARD: f64 = 5.0; // STH/day floor for an online seeder
const NETWORK_CAPACITY_GB: f64 = 10_000.0; // assumed active network disk capacity
const MAX_K_UP: f64 = 2.5; // 24/7 uptime ceiling (matches tracker K_up)
// Floor so seeded volume ALWAYS moves the forecast even with an empty boost
// pool (fixes the "5.1 STH stagnation": density used to be boost/capacity ≈ 0,
// collapsing the GB term to nothing so 1 app and 3 apps looked identical).
const MIN_DENSITY: f64 = 0.5; // STH per GB/day floor
// Per distinct app served — availability incentive. This is what makes seeding
// 3 apps forecast strictly more than seeding 1, independent of byte size.
const PER_APP_REWARD: f64 = 0.75; // STH/day per active app

/// v1.135.185: раздача включена ПО УМОЛЧАНИЮ (ключ отсутствует = новый
/// пользователь → ON, 3 ГБ). Явное «0» (пользователь выключил) уважается.
fn enabled() -> bool {
    match DB.get(KEY_ENABLED) {
        Ok(Some(v)) => v.as_ref() == b"1",
        _ => true,
    }
}

/// Результат последней отправки heartbeat по трекерам (для диагностики
/// «почему меня нет на карте сети»). Заполняется супервизором раздачи.
#[derive(Debug, Serialize, Clone, Default)]
pub(crate) struct HeartbeatStatus {
    /// unix-секунды последней попытки (0 — ещё не отправляли)
    #[serde(rename = "lastAttempt")]
    last_attempt: u64,
    /// хоть один трекер принял (HTTP 2xx)
    #[serde(rename = "anyOk")]
    any_ok: bool,
    /// по каждому трекеру: url → "ok" | "http <код>: <тело>" | "error: <текст>"
    trackers: Vec<(String, String)>,
    /// раздача включена (супервизор жив)
    enabled: bool,
    /// кошелёк разблокирован (без сид-фразы heartbeat не подписать)
    unlocked: bool,
}

static HB_STATUS: once_cell::sync::Lazy<std::sync::Mutex<HeartbeatStatus>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HeartbeatStatus::default()));

#[tauri::command]
pub(crate) async fn heartbeat_status() -> HeartbeatStatus {
    let mut st = HB_STATUS.lock().map(|g| g.clone()).unwrap_or_default();
    st.enabled = enabled();
    st.unlocked = SESSION_SEED.lock().map(|g| g.is_some()).unwrap_or(false);
    st
}

fn limit_gb() -> u64 {
    DB.get(KEY_LIMIT)
        .ok()
        .flatten()
        .and_then(|v| std::str::from_utf8(&v).ok().and_then(|s| s.trim().parse().ok()))
        .unwrap_or(DEFAULT_LIMIT_GB)
}

#[derive(Debug, Serialize)]
pub(crate) struct SeedingConfig {
    enabled: bool,
    #[serde(rename = "limitGb")]
    limit_gb: u64,
}

#[tauri::command]
pub(crate) fn get_seeding_config() -> SeedingConfig {
    SeedingConfig { enabled: enabled(), limit_gb: limit_gb() }
}

#[tauri::command]
pub(crate) fn toggle_background_seeding(
    app: AppHandle,
    enabled: bool,
    limit_gb: u64,
) -> Result<(), String> {
    // Bound the budget to what the drive can actually hold (free space), never
    // below the legacy 100 GB floor. The arbitrary 100 GB UI cap is gone.
    let ceiling = crate::apps::free_disk_gb(&app).max(100);
    let lg = limit_gb.clamp(1, ceiling);
    DB.insert(KEY_ENABLED, if enabled { b"1" as &[u8] } else { b"0" })
        .map_err(|e| e.to_string())?;
    DB.insert(KEY_LIMIT, lg.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    let _ = DB.flush();
    // bump generation: stops any running supervisor, then (re)start if enabled
    SEEDING_GEN.fetch_add(1, Ordering::SeqCst);
    if enabled {
        start_supervisor(app);
    }
    Ok(())
}

/// Auto-boot on app startup if the user previously enabled seeding.
pub(crate) fn maybe_resume(app: &AppHandle) {
    if enabled() {
        start_supervisor(app.clone());
    }
}

#[derive(Debug, Serialize, Clone)]
struct MetricsPayload {
    enabled: bool,
    status: String,
    #[serde(rename = "seededBytes")]
    seeded_bytes: u64,
    #[serde(rename = "seededGb")]
    seeded_gb: f64,
    #[serde(rename = "limitGb")]
    limit_gb: u64,
    #[serde(rename = "activeApps")]
    active_apps: u32,
    #[serde(rename = "uptime24h")]
    uptime_24h: f64,
    #[serde(rename = "kUp")]
    k_up: f64,
    #[serde(rename = "estDailyEarnings")]
    est_daily_earnings: f64,
    #[serde(rename = "maxDailyEarnings")]
    max_daily_earnings: f64,
    #[serde(rename = "densityPerGb")]
    density_per_gb: f64,
}

/// Gamified daily-earnings estimate. Returns (estimated, potential_max, density).
/// `density = max(MIN_DENSITY, boostPool / capacity)` STH per GB/day.
/// `Estimated = (Base + seededGb·density + activeApps·PerApp) · K_up`  — rewards
///   ACTUAL contribution, so seeding more apps / more data raises it.
/// `Max       = (Base + limitGb·density + activeApps·PerApp) · MAX_K_UP` — the
///   potential if you filled your reserved disk and ran 24/7.
/// All zeroed when incentivized seeding is OFF.
fn compute_earnings(
    seeded_gb: f64,
    limit_gb: u64,
    active_apps: u32,
    k_up: f64,
    boost_total_sth: f64,
    incentivized: bool,
) -> (f64, f64, f64) {
    if !incentivized {
        return (0.0, 0.0, 0.0);
    }
    let density = (boost_total_sth / NETWORK_CAPACITY_GB).max(MIN_DENSITY);
    let app_bonus = active_apps as f64 * PER_APP_REWARD;
    let est_yield = BASE_NETWORK_REWARD + seeded_gb * density + app_bonus;
    let max_yield = BASE_NETWORK_REWARD + (limit_gb as f64) * density + app_bonus;
    let est = est_yield * k_up;
    let max = max_yield * MAX_K_UP;
    (round4(est), round4(max), round4(density))
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// Derive an effective availability multiplier (K_up) from the measured uptime
/// ratio. 100% uptime → MAX_K_UP, linear below. This keeps the forecast tied to
/// REAL uptime even when the tracker reports K_up=0 for fresh / low-history
/// seeders (the cause of the network-wide "≈ 0.0000 STH/day" forecast bug).
fn uptime_to_kup(uptime_pct: f64) -> f64 {
    (uptime_pct / 100.0).clamp(0.0, 1.0) * MAX_K_UP
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

// ============================================================
//  7-day earning & uptime history (P1)
//
//  Ring-buffer of the last 7 calendar days of seeding performance, persisted in
//  the client Sled DB. The current day's snapshot is a running mean updated on
//  every 10s supervisor tick; previous days stay finalized. Records older than
//  7 days are evicted automatically.
// ============================================================

const KEY_HISTORY: &[u8] = b"cfg:seeding_history";
const KEY_HIST_ACC: &[u8] = b"cfg:seeding_hist_acc";
const HISTORY_DAYS: usize = 7;
const SECS_PER_DAY: u64 = 86_400;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct DailySnapshot {
    pub timestamp: u64,
    #[serde(rename = "formattedDate")]
    pub formatted_date: String,
    #[serde(rename = "avgUptimePercent")]
    pub avg_uptime_percent: f64,
    #[serde(rename = "estimatedEarningsSth")]
    pub estimated_earnings_sth: f64,
}

/// Running daily accumulator (mean source) — persisted so restarts keep today's
/// partial average intact.
#[derive(Debug, Serialize, Deserialize, Default)]
struct HistAcc {
    day: u64,
    sum_up: f64,
    sum_earn: f64,
    n: u64,
}

/// Civil date (DD.MM) from a unix timestamp — Howard Hinnant's days→y/m/d
/// algorithm, pure integer math, no external date crate, no panics.
fn date_ddmm(ts: u64) -> String {
    let z = (ts / SECS_PER_DAY) as i64 + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:02}.{:02}", d.max(1), m.max(1))
}

fn read_history() -> Vec<DailySnapshot> {
    DB.get(KEY_HISTORY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Vec<DailySnapshot>>(&v).ok())
        .unwrap_or_default()
}

/// Fold one live sample into today's running mean and persist the trimmed
/// 7-day ring. Best-effort: storage errors never abort the seeding loop.
fn record_history(uptime_percent: f64, est_earnings: f64) {
    let day = now_secs() / SECS_PER_DAY;
    let mut acc: HistAcc = DB
        .get(KEY_HIST_ACC)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();
    if acc.day != day {
        acc = HistAcc { day, sum_up: 0.0, sum_earn: 0.0, n: 0 };
    }
    acc.sum_up += uptime_percent;
    acc.sum_earn += est_earnings;
    acc.n += 1;
    let n = acc.n.max(1) as f64;
    let ts = day * SECS_PER_DAY;
    let snap = DailySnapshot {
        timestamp: ts,
        formatted_date: date_ddmm(ts),
        avg_uptime_percent: round1(acc.sum_up / n),
        estimated_earnings_sth: round4(acc.sum_earn / n),
    };
    let mut hist = read_history();
    match hist.iter_mut().find(|s| s.timestamp / SECS_PER_DAY == day) {
        Some(e) => *e = snap,
        None => hist.push(snap),
    }
    hist.sort_by_key(|s| s.timestamp);
    if hist.len() > HISTORY_DAYS {
        let cut = hist.len() - HISTORY_DAYS;
        hist.drain(0..cut);
    }
    if let Ok(b) = serde_json::to_vec(&hist) {
        let _ = DB.insert(KEY_HISTORY, b);
    }
    if let Ok(b) = serde_json::to_vec(&acc) {
        let _ = DB.insert(KEY_HIST_ACC, b);
    }
    let _ = DB.flush();
}

/// 7-day history for the "Earn STH" chart. ALWAYS returns exactly 7 entries
/// (today + the previous 6 days); days without recorded metrics are zero-filled
/// placeholders so the chart renders a stable 7-bar layout.
#[tauri::command]
pub(crate) async fn get_seeding_history_stats() -> Result<Vec<DailySnapshot>, String> {
    let today = now_secs() / SECS_PER_DAY;
    let stored = read_history();
    let mut out = Vec::with_capacity(HISTORY_DAYS);
    for i in (0..HISTORY_DAYS as u64).rev() {
        let day = today.saturating_sub(i);
        let ts = day * SECS_PER_DAY;
        match stored.iter().find(|s| s.timestamp / SECS_PER_DAY == day) {
            Some(s) => out.push(s.clone()),
            None => out.push(DailySnapshot {
                timestamp: ts,
                formatted_date: date_ddmm(ts),
                avg_uptime_percent: 0.0,
                estimated_earnings_sth: 0.0,
            }),
        }
    }
    Ok(out)
}

/// Best-effort sum of all apps' boost pools (STH) from the first reachable tracker.
fn fetch_network_boost_total() -> f64 {
    for t in crate::p2p::get_trackers_internal() {
        let url = format!("{}/apps", crate::p2p::normalize_tracker(&t));
        if let Ok(resp) = ureq::get(&url).timeout(Duration::from_secs(8)).call() {
            if let Ok(txt) = resp.into_string() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                    if let Some(arr) = v.as_array() {
                        let total: u64 = arr
                            .iter()
                            .filter_map(|a| a["boostPool"].as_u64())
                            .sum();
                        return total as f64 / 1e8;
                    }
                }
            }
        }
    }
    0.0
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = e.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

fn active_app_ids() -> Vec<String> {
    DB.scan_prefix(b"announce:")
        .keys()
        .filter_map(|k| k.ok())
        .filter_map(|k| {
            std::str::from_utf8(&k).ok().and_then(|s| s.strip_prefix("announce:").map(|x| x.to_string()))
        })
        .collect()
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Build a signed heartbeat body for the active account (None when locked).
/// `seeded_gb` is folded in (unsigned, informational) so the tracker can count
/// this home node's actual contribution toward the global "seeded · 24h" meter.
fn build_heartbeat(ids: &[String], seeded_gb: f64) -> Option<(String, String)> {
    let secret = { SESSION_SEED.lock().ok()?.as_ref()?.clone() };
    let (sk, pk_hex, address) = keys_from_material(secret.trim()).ok()?;
    let ts = now_secs();
    let mut sorted = ids.to_vec();
    sorted.sort();
    let digest = sha256(format!("sthb|{}|{}|{}", address, ts, sorted.join(",")).as_bytes());
    let msg = secp256k1::Message::from_digest_slice(&digest).ok()?;
    let secp = secp256k1::Secp256k1::signing_only();
    let signature = hex::encode(secp.sign_ecdsa(&msg, &sk).serialize_compact());
    let body = serde_json::json!({
        "timestamp": ts,
        "rewardAddress": address,
        "activeAppIds": sorted,
        "publicKey": pk_hex,
        "signature": signature,
        // Unsigned extras: fold home-node budget + actual seeded volume into the
        // global capacity / "seeded · 24h" meters. Informational only (same
        // self-report trust model as headless /metrics), so kept out of the
        // signed digest to avoid breaking older clients.
        "storageLimitGb": limit_gb(),
        "seededGb": seeded_gb,
        // Накопленные Byte-Hours торрент-раздачи (ГиБ·ч) — для ранжирования этой
        // ноды на публичном /map-лидерборде наравне с headless-сидерами.
        "byteHours": crate::torrent::current_byte_hours(),
    })
    .to_string();
    Some((address, body))
}

/// Best-effort fetch of this node's uptime ratio + K_up from the first tracker.
fn fetch_uptime(address: &str) -> (f64, f64) {
    for t in crate::p2p::get_trackers_internal() {
        let url = format!("{}/seeder/heartbeat/{}", crate::p2p::normalize_tracker(&t), address);
        if let Ok(resp) = ureq::get(&url).timeout(Duration::from_secs(8)).call() {
            if let Ok(txt) = resp.into_string() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                    let up = v["uptime24h"].as_f64().unwrap_or(0.0);
                    let k = v["kUp"].as_f64().unwrap_or(1.0);
                    return (up, k);
                }
            }
        }
    }
    (0.0, 1.0)
}

fn start_supervisor(app: AppHandle) {
    let my_gen = SEEDING_GEN.load(Ordering::SeqCst);
    let _ = std::thread::Builder::new().name("seeding-supervisor".into()).spawn(move || {
        let dapps_dir = ensure_data_dir(&app).join("dapps");
        let mut last_hb: u64 = 0;
        loop {
            if SEEDING_GEN.load(Ordering::SeqCst) != my_gen {
                // demobilized: emit a final STOPPED frame and exit (apps untouched)
                let _ = app.emit(
                    "seeding-metrics-broadcast",
                    MetricsPayload {
                        enabled: false,
                        status: "STOPPED".into(),
                        seeded_bytes: 0,
                        seeded_gb: 0.0,
                        limit_gb: limit_gb(),
                        active_apps: 0,
                        uptime_24h: 0.0,
                        k_up: 1.0,
                        est_daily_earnings: 0.0,
                        max_daily_earnings: 0.0,
                        density_per_gb: 0.0,
                    },
                );
                break;
            }
            let ids = active_app_ids();
            let now = now_secs();
            // 5-minute signed heartbeat
            let mut address = String::new();
            if crate::netwatch::is_online() && now.saturating_sub(last_hb) >= 300 {
                let hb_seeded_gb = (dir_size(&dapps_dir) as f64 / 1e9 * 100.0).round() / 100.0;
                if let Some((addr, body)) = build_heartbeat(&ids, hb_seeded_gb) {
                    address = addr;
                    // v1.135.185: трекеры опрашиваются параллельно, ответы
                    // фиксируются в HB_STATUS (+ syslog `seed` при отказе) —
                    // раньше `let _ = post` скрывал причину отсутствия ноды на карте.
                    let trackers = crate::p2p::get_trackers_internal();
                    let results: Vec<(String, String)> = std::thread::scope(|sc| {
                        let handles: Vec<_> = trackers
                            .iter()
                            .map(|t| {
                                let url = format!("{}/seeder/heartbeat", crate::p2p::normalize_tracker(t));
                                let body = body.clone();
                                sc.spawn(move || {
                                    let res = ureq::post(&url)
                                        .timeout(Duration::from_secs(8))
                                        .set("Content-Type", "application/json")
                                        .send_string(&body);
                                    let verdict = match res {
                                        Ok(_) => "ok".to_string(),
                                        Err(ureq::Error::Status(code, resp)) => {
                                            let txt = resp.into_string().unwrap_or_default();
                                            format!("http {}: {}", code, txt.chars().take(120).collect::<String>())
                                        }
                                        Err(e) => format!("error: {}", e.to_string().chars().take(120).collect::<String>()),
                                    };
                                    (url, verdict)
                                })
                            })
                            .collect();
                        handles.into_iter().filter_map(|h| h.join().ok()).collect()
                    });
                    let any_ok = results.iter().any(|(_, v)| v == "ok");
                    if !any_ok {
                        crate::syslog::log(
                            "seed",
                            format!("heartbeat: ни один трекер не принял: {:?}", results),
                        );
                    }
                    if let Ok(mut g) = HB_STATUS.lock() {
                        g.last_attempt = now;
                        g.any_ok = any_ok;
                        g.trackers = results;
                    }
                    last_hb = now;
                }
            }
            if address.is_empty() {
                if let Ok(g) = SESSION_SEED.lock() {
                    if let Some(s) = g.as_ref() {
                        if let Ok((_, _, a)) = keys_from_material(s.trim()) {
                            address = a;
                        }
                    }
                }
            }
            let bytes = dir_size(&dapps_dir);
            let seeded_gb = (bytes as f64 / 1e9 * 100.0).round() / 100.0;
            let (uptime, tracker_kup) = if address.is_empty() || !crate::netwatch::is_online() {
                (0.0, 1.0)
            } else {
                fetch_uptime(&address)
            };
            // Never let a tracker-reported K_up=0 zero-out the forecast: fall back
            // to the uptime-derived multiplier (fixes the global "0 STH/day" bug).
            let k_up = tracker_kup.max(uptime_to_kup(uptime));
            let incentivized = crate::oracle::incentivized_enabled();
            let boost_total = if incentivized && crate::netwatch::is_online() {
                fetch_network_boost_total()
            } else {
                0.0
            };
            let active_apps = ids.len() as u32;
            let (est, max, density) =
                compute_earnings(seeded_gb, limit_gb(), active_apps, k_up, boost_total, incentivized);
            // fold this sample into the 7-day rolling history (today's mean)
            record_history(uptime, est);
            let _ = app.emit(
                "seeding-metrics-broadcast",
                MetricsPayload {
                    enabled: true,
                    status: "ACTIVE".into(),
                    seeded_bytes: bytes,
                    seeded_gb,
                    limit_gb: limit_gb(),
                    active_apps,
                    uptime_24h: uptime,
                    k_up,
                    est_daily_earnings: est,
                    max_daily_earnings: max,
                    density_per_gb: density,
                },
            );
            std::thread::sleep(Duration::from_secs(10));
        }
    });
}
