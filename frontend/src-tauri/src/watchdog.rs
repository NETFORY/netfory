//! Watchdog главного потока (диагностика «NETFORY не отвечает»).
//!
//! Windows показывает диалог «не отвечает», когда UI-поток (event loop tao +
//! WebView2-хост) не обрабатывает сообщения >5 с. Фоновый поток каждые 2 с
//! шлёт пинг в главный поток через `run_on_main_thread`; если ответа нет
//! >8 с — пишем улику в crash-лог (длительность, RDP-сессия), при
//! восстановлении — «recovered after N s». Сам watchdog ничего не чинит.
//!
//! ВЫКЛЮЧЕН по умолчанию, чтобы не тратить ресурсы пользователей. Включение
//! при сборке: `WATCHDOG=true` в frontend/.env (вшивается include_str!) или
//! переменная окружения сборки/рантайма `WATCHDOG=true`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;

const PING_EVERY: Duration = Duration::from_secs(2);
const STALL_AFTER: Duration = Duration::from_secs(8);

static T0: Lazy<Instant> = Lazy::new(Instant::now);
/// миллисекунды от T0 последнего ответа главного потока
static LAST_PONG_MS: AtomicU64 = AtomicU64::new(0);
static STALLED: AtomicBool = AtomicBool::new(false);
/// момент последнего ответа перед зависанием (для длительности в «recovered»)
static STALL_SINCE_MS: AtomicU64 = AtomicU64::new(0);

fn parse_flag(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Приоритет: окружение рантайма → окружение сборки → вшитый .env → false.
pub(crate) fn enabled() -> bool {
    if let Some(v) = std::env::var("WATCHDOG").ok().and_then(|v| parse_flag(&v)) {
        return v;
    }
    if let Some(v) = option_env!("WATCHDOG").and_then(parse_flag) {
        return v;
    }
    include_str!("../../.env")
        .lines()
        .find_map(|l| l.trim().strip_prefix("WATCHDOG="))
        .and_then(parse_flag)
        .unwrap_or(false)
}

/// Текущая сессия — удалённая (RDP)? Улика для crash-лога.
pub(crate) fn is_remote_session() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_REMOTESESSION};
        unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn now_ms() -> u64 {
    T0.elapsed().as_millis() as u64
}

/// Запуск (идемпотентно не требуется — вызывается один раз из setup).
pub(crate) fn spawn(app: tauri::AppHandle) {
    if !enabled() {
        crate::syslog::log("boot", "main-thread watchdog: OFF (WATCHDOG=true in .env to enable)");
        return;
    }
    LAST_PONG_MS.store(now_ms(), Ordering::Relaxed);
    crate::syslog::log(
        "boot",
        format!(
            "main-thread watchdog: ON (ping {}s, stall >{}s) · remote session: {}",
            PING_EVERY.as_secs(),
            STALL_AFTER.as_secs(),
            is_remote_session()
        ),
    );
    std::thread::Builder::new()
        .name("ui-watchdog".into())
        .spawn(move || loop {
            // пинг: замыкание выполнится, когда главный поток дойдёт до очереди
            let _ = app.run_on_main_thread(|| {
                LAST_PONG_MS.store(now_ms(), Ordering::Relaxed);
            });
            std::thread::sleep(PING_EVERY);
            let silent = Duration::from_millis(now_ms().saturating_sub(LAST_PONG_MS.load(Ordering::Relaxed)));
            if silent > STALL_AFTER {
                if !STALLED.swap(true, Ordering::Relaxed) {
                    STALL_SINCE_MS.store(LAST_PONG_MS.load(Ordering::Relaxed), Ordering::Relaxed);
                    crate::crash_log(&format!(
                        "MAIN THREAD STALL: UI event loop silent for {}s — remote (RDP) session: {} — \
                         window will show «not responding» (WebView2 sync call / GPU-process hang / \
                         blocking work on UI thread)",
                        silent.as_secs(),
                        is_remote_session()
                    ));
                }
            } else if STALLED.swap(false, Ordering::Relaxed) {
                let total = now_ms().saturating_sub(STALL_SINCE_MS.load(Ordering::Relaxed)) / 1000;
                let msg = format!(
                    "main thread RECOVERED after ~{}s stall — remote session: {}",
                    total,
                    is_remote_session()
                );
                crate::crash_log(&msg);
                crate::syslog::log("boot", msg);
            }
        })
        .ok();
}
