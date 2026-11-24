//! Автозагрузка (Настройки → Интерфейс → Автозагрузка): крестик сворачивает в трей и
//! запуск при входе в систему (tauri-plugin-autostart, стартует свёрнутым в трей).
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri_plugin_autostart::ManagerExt;

const KEY: &str = "cfg:autostart";
/// Аргумент автозапуска: окно не показываем, приложение живёт в трее.
pub(crate) const MINIMIZED_ARG: &str = "--minimized";
static CLOSE_TO_TRAY: AtomicBool = AtomicBool::new(true);

#[derive(Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Autostart {
    pub close_to_tray: bool,
    pub launch_at_login: bool,
}

impl Default for Autostart {
    fn default() -> Self {
        Self { close_to_tray: true, launch_at_login: false }
    }
}

fn load() -> Autostart {
    crate::DB.get(KEY.as_bytes()).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default()
}

/// Читает настройку трея из БД (вызывать в setup до первого CloseRequested).
pub(crate) fn init() {
    CLOSE_TO_TRAY.store(load().close_to_tray, Ordering::SeqCst);
}

pub(crate) fn close_to_tray() -> bool {
    CLOSE_TO_TRAY.load(Ordering::SeqCst)
}

pub(crate) fn started_minimized() -> bool {
    std::env::args().any(|a| a == MINIMIZED_ARG)
}

#[tauri::command]
pub(crate) async fn autostart_get(app: tauri::AppHandle) -> Autostart {
    let mut s = load();
    // фактическое состояние ОС (пользователь мог убрать запись из автозагрузки вручную)
    if let Ok(on) = app.autolaunch().is_enabled() {
        s.launch_at_login = on;
    }
    s
}

#[tauri::command]
pub(crate) async fn autostart_set(app: tauri::AppHandle, settings: Autostart) -> Result<Autostart, String> {
    let al = app.autolaunch();
    let now = al.is_enabled().unwrap_or(false);
    if settings.launch_at_login && !now {
        al.enable().map_err(|e| e.to_string())?;
    } else if !settings.launch_at_login && now {
        al.disable().map_err(|e| e.to_string())?;
    }
    let b = serde_json::to_vec(&settings).map_err(|e| e.to_string())?;
    crate::DB.insert(KEY.as_bytes(), b).map_err(|e| e.to_string())?;
    let _ = crate::DB.flush();
    CLOSE_TO_TRAY.store(settings.close_to_tray, Ordering::SeqCst);
    crate::syslog::log("sys", format!("autostart: closeToTray={} launchAtLogin={}", settings.close_to_tray, settings.launch_at_login));
    Ok(autostart_get(app).await)
}
