// ============================================================
//  SmartNet // clearnet.rs  —  открытие ссылок в системном браузере
//
//  v1.135.225: белый список хостов (ALLOW_HOST / WHITELIST) снят — любой
//  http(s)-URL открывается в системном браузере. Вызывается только по явному
//  действию пользователя (контекстное меню ссылки, кнопки «открыть в браузере»);
//  обычные клики по внешним ссылкам идут во web-вкладку NETFORY (websites.rs).
//
//  Author: TechnoL0g
//  Created: 2026-06-20
// ============================================================

use tauri_plugin_shell::ShellExt;

/// Открыть http(s)-URL в системном браузере. Параметр `allowed` сохранён ради
/// совместимости IPC мессенджера (host.rs) и игнорируется.
#[tauri::command]
pub(crate) fn open_external(app: tauri::AppHandle, url: String, _allowed: Vec<String>) -> Result<(), String> {
    let trimmed = url.trim();
    let lower = trimmed.to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("Недопустимый протокол".into());
    }
    crate::syslog::log("web", format!("системный браузер → {trimmed}"));
    app.shell()
        .open(trimmed.to_string(), None)
        .map_err(|e| e.to_string())
}
