//! Медиа-сидеры мессенджера: headless-сидеры SmartNet (seeder v1.5.0+) с
//! включённым `media_storage_gb` зеркалируют медиа чатов 24/7.
//!
//! Обнаружение: трекеры `/seeders` → у онлайн-сидеров с `metricsUrl` читаем
//! `/metrics`; непустой `mediaEndpointId` = сидер принимает медиа. Список
//! обновляется раз в 10 минут фоновым потоком (без трекерных изменений).
//!
//! Использование: после отправки медиа (и после успешной докачки) шлём
//! `POST <seeder>/media/announce {h, n, s, p}` — сидер сразу качает блоб и
//! становится провайдером; при докачке его `mediaEndpointId` добавляется в
//! список провайдеров, поэтому медиа доступно даже когда автор офлайн.

use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Debug)]
pub(crate) struct MediaSeeder {
    /// базовый URL HTTP-API сидера (без `/metrics`), напр. http://1.2.3.4:9466
    pub base: String,
    pub endpoint_id: String,
}

static SEEDERS: Lazy<Mutex<Vec<MediaSeeder>>> = Lazy::new(|| Mutex::new(Vec::new()));
static STARTED: std::sync::Once = std::sync::Once::new();

const REFRESH_SECS: u64 = 600;
const MAX_SEEDERS: usize = 8;

/// endpointId всех известных медиа-сидеров (доп. провайдеры для докачки).
pub(crate) fn endpoint_ids() -> Vec<String> {
    SEEDERS.lock().unwrap().iter().map(|s| s.endpoint_id.clone()).collect()
}

pub(crate) fn snapshot() -> Vec<MediaSeeder> {
    SEEDERS.lock().unwrap().clone()
}

/// Запуск фонового обновления списка (идемпотентно).
pub(crate) fn start() {
    STARTED.call_once(|| {
        std::thread::Builder::new()
            .name("media-seeders".into())
            .spawn(|| {
                std::thread::sleep(Duration::from_secs(15));
                loop {
                    refresh_blocking();
                    std::thread::sleep(Duration::from_secs(REFRESH_SECS));
                }
            })
            .ok();
    });
}

#[derive(serde::Deserialize)]
struct SeederRow {
    #[serde(rename = "metricsUrl", default)]
    metrics_url: String,
    #[serde(default)]
    online: bool,
}

#[derive(serde::Deserialize)]
struct SeederMetrics {
    #[serde(rename = "mediaEndpointId", default)]
    media_endpoint_id: String,
    #[serde(rename = "mediaEnabled", default)]
    media_enabled: bool,
}

fn base_of(metrics_url: &str) -> String {
    let u = metrics_url.trim().trim_end_matches('/');
    u.strip_suffix("/metrics").unwrap_or(u).to_string()
}

fn refresh_blocking() {
    let to = Duration::from_secs(5);
    let mut urls: Vec<String> = Vec::new();
    for t in crate::p2p::get_trackers_internal() {
        let url = format!("{}/seeders", crate::p2p::normalize_tracker(&t));
        let Ok(resp) = ureq::get(&url).timeout(to).call() else { continue };
        let Ok(txt) = resp.into_string() else { continue };
        let Ok(rows) = serde_json::from_str::<Vec<SeederRow>>(&txt) else { continue };
        for r in rows {
            let m = r.metrics_url.trim();
            if r.online && m.starts_with("http") && !urls.contains(&m.to_string()) {
                urls.push(m.to_string());
            }
        }
    }
    let mut found: Vec<MediaSeeder> = Vec::new();
    for m in urls.into_iter().take(32) {
        let Ok(resp) = ureq::get(&m).timeout(to).call() else { continue };
        let Ok(txt) = resp.into_string() else { continue };
        let Ok(sm) = serde_json::from_str::<SeederMetrics>(&txt) else { continue };
        if sm.media_enabled && !sm.media_endpoint_id.is_empty() {
            found.push(MediaSeeder { base: base_of(&m), endpoint_id: sm.media_endpoint_id });
            if found.len() >= MAX_SEEDERS {
                break;
            }
        }
    }
    let n = found.len();
    *SEEDERS.lock().unwrap() = found;
    crate::syslog::log("chat", format!("media seeders: {} available", n));
}

/// Анонс медиа всем известным медиа-сидерам (fire-and-forget, вне UI-потока).
pub(crate) fn announce(hash: &str, node: &str, size: u64, extra: &[String]) {
    let seeders = snapshot();
    if seeders.is_empty() || hash.is_empty() || node.is_empty() {
        return;
    }
    let body = serde_json::json!({ "h": hash, "n": node, "s": size, "p": extra }).to_string();
    let short: String = hash.chars().take(12).collect();
    tauri::async_runtime::spawn_blocking(move || {
        let mut ok = 0usize;
        for s in &seeders {
            let url = format!("{}/media/announce", s.base);
            if ureq::post(&url)
                .timeout(Duration::from_secs(8))
                .set("Content-Type", "application/json")
                .send_string(&body)
                .is_ok()
            {
                ok += 1;
            }
        }
        crate::syslog::log(
            "chat",
            format!("media {}… announced to {}/{} seeder(s)", short, ok, seeders.len()),
        );
    });
}
