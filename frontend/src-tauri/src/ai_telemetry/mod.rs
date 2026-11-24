// ============================================================
//  ai_telemetry — AI Computer (sn://ai-computer), Phase 1
//
//  Opt-in телеметрия мощностей устройства для Netfory AI Pool:
//  при включённом тумблере нода шлёт подписанный heartbeat с метриками
//  GPU/CPU/RAM на AI-трекеры каждые 15 секунд. Generation counter —
//  как в seeding.rs: toggle инвалидирует старый цикл.
// ============================================================

mod heartbeat;
pub mod hw;

use crate::DB;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static AI_GEN: AtomicU64 = AtomicU64::new(0);
/// unix ts последнего успешно принятого трекером heartbeat'а (0 = не было)
static LAST_HB_OK: AtomicU64 = AtomicU64::new(0);

// ---------- кеш скачанных моделей (app-data/ai-models-store) ----------

fn ai_models_store_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("ai-models-store"))
}

fn dir_size(p: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                total += dir_size(&path);
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    total
}

/// Занятое кешем скачанных моделей место, байты.
#[tauri::command]
pub(crate) async fn ai_models_cache_stats(app: tauri::AppHandle) -> Result<u64, String> {
    let dir = ai_models_store_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || dir_size(&dir))
        .await
        .map_err(|e| e.to_string())
}

/// Удалить кеш скачанных моделей целиком. Возвращает освобождённые байты.
#[tauri::command]
pub(crate) async fn ai_models_cache_clear(app: tauri::AppHandle) -> Result<u64, String> {
    let dir = ai_models_store_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let freed = dir_size(&dir);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        }
        Ok(freed)
    })
    .await
    .map_err(|e| e.to_string())?
}

const KEY_ENABLED: &[u8] = b"cfg:ai_pool_enabled";
const HB_INTERVAL_SECS: u64 = 15;
/// «connected», если последний успешный heartbeat моложе этого окна
const CONNECTED_WINDOW_SECS: u64 = 45;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn enabled() -> bool {
    DB.get(KEY_ENABLED)
        .ok()
        .flatten()
        .map(|v| v.as_ref() == b"1")
        .unwrap_or(false)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiPoolStatus {
    enabled: bool,
    /// disabled | connected | syncing
    status: String,
    last_heartbeat: u64,
}

fn status_now() -> AiPoolStatus {
    let en = enabled();
    let last = LAST_HB_OK.load(Ordering::Relaxed);
    let status = if !en {
        "disabled"
    } else if last > 0 && now_secs().saturating_sub(last) <= CONNECTED_WINDOW_SECS {
        "connected"
    } else {
        "syncing"
    };
    AiPoolStatus { enabled: en, status: status.into(), last_heartbeat: last }
}

#[tauri::command]
pub(crate) fn ai_pool_status() -> AiPoolStatus {
    status_now()
}

#[tauri::command]
pub(crate) fn ai_pool_toggle(enabled: bool) -> AiPoolStatus {
    let _ = DB.insert(KEY_ENABLED, if enabled { &b"1"[..] } else { &b"0"[..] });
    let _ = DB.flush();
    AI_GEN.fetch_add(1, Ordering::SeqCst);
    if enabled {
        start_loop();
    } else {
        LAST_HB_OK.store(0, Ordering::Relaxed);
        // вежливый opt-out: трекер сразу выкинет ноду из пула, не дожидаясь TTL
        std::thread::spawn(|| {
            let snap = hw::collect();
            if let Some(body) = heartbeat::build_body(&snap, false) {
                let _ = heartbeat::send(&body);
            }
        });
    }
    status_now()
}

/// Снимок железа для карточки «Моё устройство».
#[tauri::command]
pub(crate) async fn ai_hw_snapshot() -> Result<hw::HwSnapshot, String> {
    tauri::async_runtime::spawn_blocking(hw::collect)
        .await
        .map_err(|e| e.to_string())
}

/// Агрегированные AI Stats всей сети (с AI-трекеров).
#[tauri::command]
pub(crate) async fn ai_net_stats() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(heartbeat::fetch_net_stats)
        .await
        .map_err(|e| e.to_string())?
}

/// Возобновить heartbeat-цикл при старте приложения, если тумблер включён.
pub(crate) fn maybe_resume() {
    if enabled() {
        start_loop();
    }
}

fn start_loop() {
    let my_gen = AI_GEN.load(Ordering::SeqCst);
    let _ = std::thread::Builder::new().name("ai-telemetry".into()).spawn(move || loop {
        if AI_GEN.load(Ordering::SeqCst) != my_gen || !enabled() {
            break;
        }
        if crate::netwatch::is_online() {
            let snap = hw::collect();
            // build_body = None, пока кошелёк заблокирован — статус останется "syncing"
            if let Some(body) = heartbeat::build_body(&snap, true) {
                if heartbeat::send(&body) {
                    LAST_HB_OK.store(now_secs(), Ordering::Relaxed);
                }
            }
        }
        std::thread::sleep(Duration::from_secs(HB_INTERVAL_SECS));
    });
}
