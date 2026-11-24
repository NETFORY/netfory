// ============================================================
//  SmartNet // syslog.rs
//
//  Lightweight in-process log bus for the floating System Console in the
//  client UI. Internal P2P / DHT / network events are pushed into a capped
//  ring buffer (max 500 lines) AND streamed live to the frontend via the
//  Tauri `sys-log` event. The console fetches the backlog on open
//  (`sys_log_history`) and appends live lines afterwards.
//
//  Scopes: "dht" | "p2p" | "net" | "upd" | "sys" | "dapp".
//
//  Repeat coalescing (v1.55.18): identical consecutive lines within the same
//  scope are folded into a single row with a `×N` counter, both in the ring
//  buffer AND on the live stream. The frontend detects a replacement by the
//  `replace: true` flag on the streamed event and rewrites the previous
//  matching row in place instead of appending a new one. Keeps DHT/discover
//  poll spam out of view without silencing anything.
// ============================================================

use once_cell::sync::Lazy;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

const MAX_LINES: usize = 500;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LogLine {
    /// Unix epoch milliseconds.
    pub ts: u64,
    pub scope: String,
    pub msg: String,
    /// Repeat count for coalesced duplicates. `1` for a fresh line.
    #[serde(default)]
    pub count: u32,
    /// True when this event **replaces** the last streamed line (identical
    /// scope+base_msg) instead of appending a new one. UI uses this to
    /// rewrite the previous row instead of pushing.
    #[serde(default)]
    pub replace: bool,
}

static SYS_LOG: Lazy<Mutex<VecDeque<LogLine>>> =
    Lazy::new(|| Mutex::new(VecDeque::with_capacity(MAX_LINES)));
static LOG_APP: Lazy<Mutex<Option<AppHandle>>> = Lazy::new(|| Mutex::new(None));

/// Число строк в кольцевом буфере (для переписи памяти crashdiag).
pub(crate) fn line_count() -> usize {
    SYS_LOG.lock().map(|l| l.len()).unwrap_or(0)
}

/// Store the app handle so `log()` can stream lines to the UI from anywhere.
pub(crate) fn init(app: AppHandle) {
    *LOG_APP.lock().unwrap() = Some(app);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Push a line to the ring buffer, mirror it to stdout, and stream it to the
/// floating System Console (if the UI is up). If the previous line in the
/// same scope has the identical base message, the existing row is mutated in
/// place with an incremented `count` and a fresh timestamp — the event is
/// then emitted with `replace: true` so the UI can update the last row
/// rather than growing the list.
pub(crate) fn log(scope: &str, msg: impl Into<String>) {
    let msg = msg.into();
    let ts = now_ms();

    // Prepare the event to stream. Filled inside the lock so we know whether
    // this was a fresh append (`replace = false`) or a coalesced repeat
    // (`replace = true`, count incremented).
    let event: LogLine;
    {
        let mut buf = SYS_LOG.lock().unwrap();
        // v1.63.4: window-based dedup. Ранее сравнивали только с `back()`,
        // из-за чего чередующиеся сообщения одного скоупа (напр., DHT
        // пинг-понг `merged peer DHT hints` ↔ `harvested live DHT nodes`)
        // никогда не схлопывались и засоряли консоль. Теперь просматриваем
        // ПОСЛЕДНИЕ 6 записей: если находим совпадающий (scope, msg) —
        // удаляем его из старой позиции, поднимаем в конец с bump'ом
        // count. Порядок остальных строк сохраняется. Окно 6 покрывает
        // типовые чередования (2–4 сообщения) без риска «поднимать»
        // очень старые записи и путать пользователя.
        const DEDUP_WINDOW: usize = 6;
        let start = buf.len().saturating_sub(DEDUP_WINDOW);
        let mut match_at: Option<usize> = None;
        for i in (start..buf.len()).rev() {
            if let Some(line) = buf.get(i) {
                if line.scope == scope && line.msg == msg {
                    match_at = Some(i);
                    break;
                }
            }
        }
        if let Some(i) = match_at {
            // Извлекаем и снова вставляем в конец с bump'ом count.
            let mut hit = buf.remove(i).expect("index in bounds");
            hit.count = hit.count.saturating_add(1);
            hit.ts = ts;
            // stdout mirror: только первое появление, иначе flood.
            event = LogLine {
                ts,
                scope: hit.scope.clone(),
                msg: hit.msg.clone(),
                count: hit.count,
                replace: true,
            };
            buf.push_back(hit);
        } else {
            println!("[{}] {}", scope, msg);
            if buf.len() >= MAX_LINES {
                buf.pop_front();
            }
            let fresh = LogLine {
                ts,
                scope: scope.to_string(),
                msg,
                count: 1,
                replace: false,
            };
            buf.push_back(fresh.clone());
            event = fresh;
        }
    }
    if let Some(app) = LOG_APP.lock().unwrap().clone() {
        emit_queued(&app, "sys-log", serde_json::to_value(&event).unwrap_or_default());
    }
}

// ---- отложенный emit (v1.135.227) ------------------------------------------
//  Emit из колбэков WebView2 (NavigationStarting, NavigationCompleted, …) на
//  главном потоке реентерабельно вызывает eval во всех webview поверх уже
//  выполняющегося handle_user_message / вложенного message-pump → зависание
//  или падение без паники (крашлог 1791664985). Все события из колбэков идут
//  через один рабочий поток: порядок сохраняется, главный поток не блокируется.
static EMIT_TX: Lazy<Mutex<Option<std::sync::mpsc::Sender<(AppHandle, String, serde_json::Value)>>>> =
    Lazy::new(|| Mutex::new(None));

pub(crate) fn emit_queued(app: &AppHandle, event: &str, payload: serde_json::Value) {
    let mut guard = EMIT_TX.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        let (tx, rx) = std::sync::mpsc::channel::<(AppHandle, String, serde_json::Value)>();
        std::thread::Builder::new()
            .name("emit-queue".into())
            .spawn(move || {
                while let Ok((app, ev, payload)) = rx.recv() {
                    let _ = app.emit(ev.as_str(), payload);
                }
            })
            .ok();
        *guard = Some(tx);
    }
    if let Some(tx) = guard.as_ref() {
        let _ = tx.send((app.clone(), event.to_string(), payload));
    }
}

/// Backlog snapshot for the console to render on open.
#[tauri::command]
pub(crate) fn sys_log_history() -> Vec<LogLine> {
    SYS_LOG.lock().unwrap().iter().cloned().collect()
}

/// Запись из фронтенда (UI) в системный журнал — диагностика без DevTools.
#[tauri::command]
pub(crate) fn ui_log(scope: String, msg: String) {
    log(&scope, msg);
}
