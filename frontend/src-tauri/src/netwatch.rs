// ============================================================
//  SmartNet // netwatch.rs — фактический мониторинг интернет-подключения.
//
//  Проблема: `navigator.onLine` в WebView (особенно WebKitGTK) после сна
//  ноутбука событий offline/online НЕ шлёт — UI не знает, что сети нет.
//  Решение: лёгкий периодический TCP-probe из Rust (несколько публичных
//  анкеров, таймаут 3 с). При смене состояния эмитим событие `net-status`
//  { online: bool } — строка состояния внизу показывает «Нет соединения».
//
//  Author: TechnoL0g
//  Created: 2026-06
// ============================================================

use std::sync::atomic::{AtomicU8, Ordering};
use tauri::{AppHandle, Emitter};

/// 0 = offline, 1 = online, 2 = ещё не проверяли.
static ONLINE: AtomicU8 = AtomicU8::new(2);

/// Публичные анкеры для проверки (достаточно ЛЮБОГО успешного TCP-handshake).
const PROBES: &[&str] = &["1.1.1.1:443", "8.8.8.8:53", "9.9.9.9:443"];
/// Таймаут одного probe.
const PROBE_TIMEOUT_SECS: u64 = 3;
/// Интервал проверок: онлайн — реже, офлайн — чаще (быстрее ловим возврат сети).
const INTERVAL_ONLINE_SECS: u64 = 15;
const INTERVAL_OFFLINE_SECS: u64 = 5;

async fn probe_once() -> bool {
    for addr in PROBES {
        let attempt = tokio::time::timeout(
            std::time::Duration::from_secs(PROBE_TIMEOUT_SECS),
            tokio::net::TcpStream::connect(addr),
        )
        .await;
        if matches!(attempt, Ok(Ok(_))) {
            return true;
        }
    }
    false
}

/// Текущее состояние сети (для других модулей; 2=unknown считаем онлайном).
pub(crate) fn is_online() -> bool {
    ONLINE.load(Ordering::Relaxed) != 0
}

/// Текущее состояние сети для фронтенда (инициализация строки состояния).
#[tauri::command]
pub(crate) fn net_online() -> bool {
    is_online()
}

/// Запустить фоновый цикл мониторинга. Эмитит `net-status` при смене состояния.
pub(crate) fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let up = probe_once().await;
            let prev = ONLINE.swap(if up { 1 } else { 0 }, Ordering::Relaxed);
            let changed = prev == 2 || (prev == 1) != up;
            if changed {
                let _ = app.emit("net-status", serde_json::json!({ "online": up }));
                // Возврат сети после обрыва: соединения gossip-соседей мертвы, а
                // NeighborDown по ним может так и не прийти (зомби «ONLINE N»).
                // Не ждём 5-минутный watchdog — сразу перезапускаем меш.
                if up && prev == 0 {
                    crate::p2p::notify_network_change().await;
                    crate::chat::on_network_recovered();
                }
                crate::syslog::log(
                    "net",
                    if up {
                        "✓ internet connection is up".to_string()
                    } else {
                        "✗ internet connection lost — waiting for recovery".to_string()
                    },
                );
            }
            let secs = if up { INTERVAL_ONLINE_SECS } else { INTERVAL_OFFLINE_SECS };
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        }
    });
}
