// ============================================================
//  netdrop-ui // src-tauri/main.rs — Tauri-команды поверх netdrop-core.
//  send_files (мультифайл/папки), receive_probe/accept/reject
//  (с докачкой *.ndpart как в CLI), история передач, QR-SVG.
//  События: "nd-event" {kind, ...}. Сообщения — на английском.
// ============================================================

use netdrop_core::crypto::{ChunkCipher, EphemeralKeyPair, Role, Ticket};
use netdrop_core::network::protocol as proto;
use netdrop_core::network::protocol::FrameType;
use netdrop_core::network::transport::{Connection, RecvStream, SendStream, Transport};
use netdrop_core::pipeline::{self, open_frame, seal_frame, Preseed};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Notify;
use tokio_util::io::SyncIoBridge;

mod cloud;
mod mailbox;

/// Ожидающая подтверждения входящая передача (probe → accept/reject).
struct Pending {
    /// None — соединение принято слушателем устройств (транспорт общий, не закрываем).
    transport: Option<Transport>,
    conn: Connection,
    tx: SendStream,
    rx: RecvStream,
    sealer: Option<ChunkCipher>,
    opener: Option<ChunkCipher>,
    meta: proto::MetaInfo,
}

struct UiState {
    pending: tokio::sync::Mutex<Option<Pending>>,
    /// Токены отмены активных передач (id → Notify).
    cancels: tokio::sync::Mutex<HashMap<u64, Arc<Notify>>>,
    /// Активные передачи для меню трея (id → подпись).
    active: std::sync::Mutex<HashMap<u64, String>>,
    /// Прогресс активных передач для подсказки трея (id → (done, total)).
    active_progress: std::sync::Mutex<HashMap<u64, (u64, u64)>>,
    /// Одноразовый секрет активного сопряжения.
    pair_secret: std::sync::Mutex<Option<[u8; 16]>>,
    /// Стоп-токен слушателя доверенных устройств.
    listener_stop: tokio::sync::Mutex<Option<Arc<Notify>>>,
    next_id: AtomicU64,
}

fn emit(app: &AppHandle, payload: serde_json::Value) {
    let _ = app.emit("nd-event", payload);
}

fn watch_progress(app: AppHandle, id: u64, counter: Arc<AtomicU64>, total: u64) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let mut tick: u32 = 0;
        loop {
            let done = counter.load(Ordering::Relaxed);
            emit(&app, json!({ "id": id, "kind": "progress", "done": done, "total": total }));
            // прогресс для подсказки трея (обновляем тултип раз в ~1 с)
            {
                let st = app.state::<UiState>();
                st.active_progress
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id, (done, total));
            }
            if tick % 5 == 0 {
                tray_tooltip_refresh(&app);
            }
            tick = tick.wrapping_add(1);
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
}

// ---------- Утилиты (тестируемые) ----------

fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() { "netdrop.bin".to_string() } else { trimmed }
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let safe = sanitize_name(name);
    let candidate = dir.join(&safe);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match safe.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (safe.clone(), String::new()),
    };
    for i in 1..10_000u32 {
        let p = dir.join(format!("{stem} ({i}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem} ({}){ext}", std::process::id()))
}

/// Отображаемое имя набора: "a.txt" | "photos" | "a.txt (+2 more)".
fn bundle_name(paths: &[PathBuf]) -> String {
    let first = paths
        .first()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "netdrop".to_string());
    if paths.len() > 1 {
        format!("{first} (+{} more)", paths.len() - 1)
    } else {
        first
    }
}

/// QR-код тикета в SVG-строку (для <img>/v-html в UI).
fn ticket_qr_svg(data: &str) -> Option<String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::with_error_correction_level(data.as_bytes(), qrcode::EcLevel::L).ok()?;
    Some(
        code.render::<svg::Color>()
            .min_dimensions(220, 220)
            .dark_color(svg::Color("#d7e2ec"))
            .light_color(svg::Color("#0a0e14"))
            .build(),
    )
}

fn walk_stats(dir: &Path, total: &mut u64, count: &mut u32) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            walk_stats(&entry.path(), total, count)?;
        } else if meta.is_file() {
            *total += meta.len();
            *count += 1;
        }
    }
    Ok(())
}

/// Статистика набора путей: (размер, файлов). Блокирующая — звать из spawn_blocking.
fn paths_stats(paths: &[PathBuf]) -> std::io::Result<(u64, u32)> {
    let mut total = 0u64;
    let mut count = 0u32;
    for p in paths {
        let meta = std::fs::metadata(p)?;
        if meta.is_dir() {
            walk_stats(p, &mut total, &mut count)?;
        } else {
            total += meta.len();
            count += 1;
        }
    }
    Ok((total, count))
}

// ---------- История передач ----------

fn history_path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("history.json"))
}

fn history_push(app: &AppHandle, entry: serde_json::Value) {
    let Some(path) = history_path(app) else { return };
    // файловый I/O — вне async-runtime, чтобы не подвешивать основной процесс
    tauri::async_runtime::spawn_blocking(move || {
        let mut list: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        list.insert(0, entry);
        list.truncate(50);
        if let Ok(s) = serde_json::to_string(&list) {
            let _ = std::fs::write(&path, s);
        }
    });
}

fn now_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Системное уведомление — только если окно не в фокусе (свёрнуто/в фоне).
fn notify_if_unfocused(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    let focused = app
        .get_webview_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false);
    if !focused {
        let _ = app.notification().builder().title(title).body(body).show();
    }
}

/// Единая точка завершения передачи: событие, история, уведомление.
fn finish_transfer(
    app: &AppHandle,
    id: u64,
    dir: &str,
    name: &str,
    size: u64,
    paths: Option<Vec<String>>,
    share_uri: Option<String>,
    path: Option<String>,
    res: Result<(), String>,
) {
    let status = match &res {
        Ok(_) => "done",
        Err(e) if e == "cancelled" => "cancelled",
        Err(e) if e.contains("kept for resume") => "paused",
        Err(_) => "error",
    };
    let mut entry = json!({
        "dir": dir, "name": name, "size": size, "status": status, "ts": now_ts(),
    });
    if let Some(p) = paths {
        entry["paths"] = json!(p);
    }
    if let Some(u) = share_uri {
        entry["shareUri"] = json!(u);
    }
    if let Some(p) = path {
        entry["path"] = json!(p);
    }
    history_push(app, entry);
    untrack_active(app, id);
    match res {
        Ok(_) => {
            let verb = if dir == "send" { "Sent" } else { "Received" };
            notify_if_unfocused(app, "netdrop", &format!("{verb}: {name}"));
        }
        Err(e) => {
            if e == "cancelled" {
                emit(app, json!({ "id": id, "kind": "cancelled" }));
            } else {
                emit(app, json!({ "id": id, "kind": "error", "message": e.clone() }));
            }
            let what = if e == "cancelled" { "Cancelled" } else { "Failed" };
            notify_if_unfocused(app, "netdrop", &format!("{what}: {name}"));
        }
    }
}

/// Открыть системный файловый менеджер с выделенным файлом.
#[tauri::command(async)]
fn show_in_folder(path: String) {
    // Android/iOS: файлового менеджера с выделением нет
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let _ = &path;
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{path}"))
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").args(["-R", &path]).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let parent = std::path::Path::new(&path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
    }
}

/// Зарегистрировать активную передачу (для меню трея).
fn track_active(app: &AppHandle, id: u64, label: String) {
    let st = app.state::<UiState>();
    st.active
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(id, label);
    tray_refresh(app);
}

/// Убрать передачу из активных (для меню трея).
fn untrack_active(app: &AppHandle, id: u64) {
    let st = app.state::<UiState>();
    st.active
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&id);
    st.active_progress
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&id);
    tray_refresh(app);
    tray_tooltip_refresh(app);
}

fn tray_refresh(app: &AppHandle) {
    #[cfg(desktop)]
    {
        let _ = update_tray_menu(app);
    }
    #[cfg(not(desktop))]
    let _ = app;
}

/// Подсказка иконки трея: активная передача + процент (Windows: ≤127 симв.).
fn tray_tooltip_refresh(app: &AppHandle) {
    #[cfg(desktop)]
    {
        let Some(tray) = app.tray_by_id("nd-tray") else {
            return;
        };
        let st = app.state::<UiState>();
        let labels = st.active.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let progress = st.active_progress.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let tip = if labels.is_empty() {
            "netdrop".to_string()
        } else {
            // первая активная передача (стабильно — минимальный id) + счётчик остальных
            let (&id, label) = labels
                .iter()
                .min_by_key(|(id, _)| **id)
                .expect("labels not empty");
            let short: String = label.chars().take(34).collect();
            let pct = progress
                .get(&id)
                .filter(|(_, total)| *total > 0)
                .map(|(done, total)| format!(" {}%", (done.saturating_mul(100) / total).min(100)))
                .unwrap_or_default();
            let more = if labels.len() > 1 {
                format!(" (+{})", labels.len() - 1)
            } else {
                String::new()
            };
            format!("netdrop — {short}{pct}{more}")
        };
        let _ = tray.set_tooltip(Some(tip));
    }
    #[cfg(not(desktop))]
    let _ = app;
}

#[cfg(desktop)]
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Перестроить меню трея: Show / активные передачи / Quit.
#[cfg(desktop)]
fn update_tray_menu(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    let Some(tray) = app.tray_by_id("nd-tray") else {
        return Ok(());
    };
    let labels: Vec<String> = {
        let st = app.state::<UiState>();
        let g = st.active.lock().unwrap_or_else(|p| p.into_inner());
        g.values().cloned().collect()
    };
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(app, "show", "Show netdrop", true, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    if labels.is_empty() {
        menu.append(&MenuItem::with_id(app, "none", "No active transfers", false, None::<&str>)?)?;
    } else {
        for (i, label) in labels.iter().enumerate() {
            menu.append(&MenuItem::with_id(app, format!("tr-{i}"), label, false, None::<&str>)?)?;
        }
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?)?;
    tray.set_menu(Some(menu))?;
    Ok(())
}

/// Создать иконку трея (клик — показать окно, меню — по правой кнопке).
#[cfg(desktop)]
fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    TrayIconBuilder::with_id("nd-tray")
        .icon(app.default_window_icon().cloned().ok_or("no window icon")?)
        .tooltip("netdrop")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    let _ = update_tray_menu(app.handle());
    Ok(())
}

/// Включить/выключить автозапуск при старте системы (свёрнутым в трей).
#[tauri::command(async)]
fn set_autostart(app: AppHandle, enable: bool) -> Result<(), String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_autostart::ManagerExt;
        let al = app.autolaunch();
        let res = if enable { al.enable() } else { al.disable() };
        return res.map_err(|e| e.to_string());
    }
    #[cfg(not(desktop))]
    {
        let _ = (app, enable);
        Ok(())
    }
}

/// Текущее состояние автозапуска.
#[tauri::command(async)]
fn get_autostart(app: AppHandle) -> bool {
    #[cfg(desktop)]
    {
        use tauri_plugin_autostart::ManagerExt;
        return app.autolaunch().is_enabled().unwrap_or(false);
    }
    #[cfg(not(desktop))]
    {
        let _ = app;
        false
    }
}

/// Сохранить содержимое share-интента во временный файл (Android share).
/// Возвращает путь — дальше файл отправляется как обычный.
#[tauri::command]
async fn stash_shared_file(
    app: AppHandle,
    name: String,
    data: Vec<u8>,
) -> Result<String, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("shared");
    tokio::fs::create_dir_all(&dir).await.ok();
    let dest = unique_path(&dir, &name);
    tokio::fs::write(&dest, data).await.map_err(|e| e.to_string())?;
    Ok(dest.to_string_lossy().to_string())
}

/// Удалить из share-кэша файлы, попавшие туда через «Поделиться»
/// (вызывается после успешной отправки).
fn cleanup_shared_paths(app: &AppHandle, paths: &[PathBuf]) {
    let Ok(cache) = app.path().app_cache_dir() else { return };
    let shared = cache.join("shared");
    for p in paths {
        if p.starts_with(&shared) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Android: экспорт принятого файла в публичный Download/netdrop через
/// MediaStore (виден в проводнике/галерее). Возвращает JSON-строку FsUri.
#[cfg(target_os = "android")]
async fn export_to_downloads(app: &AppHandle, src: &Path, name: &str) -> Result<String, String> {
    use tauri_plugin_android_fs::{AndroidFsExt, PublicGeneralPurposeDir};
    let api = app.android_fs_async();
    let ps = api.public_storage();
    if !ps.request_permission().await.map_err(|e| e.to_string())? {
        return Err("storage permission denied".to_string());
    }
    let uri = ps
        .create_new_file_with_pending(None, PublicGeneralPurposeDir::Download, format!("netdrop/{name}"), None)
        .await
        .map_err(|e| e.to_string())?;
    let mut out = api.open_file_writable(&uri).await.map_err(|e| e.to_string())?;
    let src = src.to_path_buf();
    let copy = tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let mut inp = std::fs::File::open(&src).map_err(|e| e.to_string())?;
        std::io::copy(&mut inp, &mut out).map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    if let Err(e) = copy {
        let _ = api.remove_file(&uri).await;
        return Err(e);
    }
    ps.set_pending(&uri, false).await.map_err(|e| e.to_string())?;
    serde_json::to_string(&uri).map_err(|e| e.to_string())
}

/// Android: системный диалог «Поделиться» для принятого файла.
#[cfg(target_os = "android")]
#[tauri::command]
async fn share_received_file(app: AppHandle, uri: String) -> Result<(), String> {
    use tauri_plugin_android_fs::{AndroidFsExt, FsUri};
    let uri: FsUri = serde_json::from_str(&uri).map_err(|e| e.to_string())?;
    app.android_fs_async()
        .opener()
        .share_file(&uri)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn share_received_file(_uri: String) -> Result<(), String> {
    Err("share is only supported on Android".to_string())
}

/// Отменить активную передачу по id.
#[tauri::command]
async fn cancel_transfer(state: State<'_, UiState>, id: u64) -> Result<(), String> {
    if let Some(n) = state.cancels.lock().await.get(&id) {
        n.notify_waiters();
    }
    Ok(())
}

#[tauri::command(async)]
fn history_list(app: AppHandle) -> Vec<serde_json::Value> {
    history_path(&app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

// удаление одной записи истории (свайп в UI): матчим по ts+name+dir+size
#[tauri::command(async)]
fn history_remove(app: AppHandle, ts: u64, name: String, dir: String, size: u64) {
    if let Some(p) = history_path(&app) {
        let mut list: Vec<serde_json::Value> = std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if let Some(i) = list.iter().position(|e| {
            e["ts"].as_u64() == Some(ts)
                && e["name"].as_str() == Some(name.as_str())
                && e["dir"].as_str() == Some(dir.as_str())
                && e["size"].as_u64() == Some(size)
        }) {
            list.remove(i);
            if let Ok(s) = serde_json::to_string(&list) {
                let _ = std::fs::write(&p, s);
            }
        }
    }
}

#[tauri::command(async)]
fn history_clear(app: AppHandle) {
    if let Some(p) = history_path(&app) {
        let _ = std::fs::remove_file(p);
    }
}

// ---------- Простые команды ----------

#[tauri::command]
fn default_relays() -> Vec<String> {
    netdrop_core::RELAYS_N1.iter().map(|s| s.to_string()).collect()
}

#[tauri::command(async)]
fn default_out_dir(app: AppHandle) -> String {
    let _ = &app;
    // Android: HOME/USERPROFILE нет, cwd = "/" (read-only, os error 30) —
    // пишем в приватное хранилище приложения (доступно без разрешений)
    #[cfg(target_os = "android")]
    {
        if let Ok(d) = app.path().app_data_dir() {
            let d = d.join("received");
            let _ = std::fs::create_dir_all(&d);
            return d.to_string_lossy().to_string();
        }
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    let dl = Path::new(&home).join("Downloads");
    if dl.is_dir() { dl.to_string_lossy().to_string() } else { home }
}

// ---------- Отправка ----------

/// Начать отправку набора путей (файлы и/или папки) ОДНИМ тикетом.
/// Возвращает {ticket, qrSvg, name, size, fileCount}; передача идёт в фоне.
#[tauri::command]
async fn send_files(
    app: AppHandle,
    state: State<'_, UiState>,
    paths: Vec<String>,
    encrypt: bool,
    compress: bool,
    relays: Vec<String>,
    use_n1: bool,
    limit_kib: u64,
    multi: bool,
) -> Result<serde_json::Value, String> {
    if paths.is_empty() {
        return Err("no paths selected".to_string());
    }
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    for p in &paths {
        if !tokio::fs::try_exists(p).await.unwrap_or(false) {
            return Err(format!("path not found: {}", p.display()));
        }
    }
    // один файл → прямой режим (с докачкой); иначе — tar-набор
    let single_file = paths.len() == 1
        && tokio::fs::metadata(&paths[0]).await.map(|m| m.is_file()).unwrap_or(false);
    let name = bundle_name(&paths);
    let stats_paths = paths.clone();
    let (size, file_count) = tokio::task::spawn_blocking(move || paths_stats(&stats_paths))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

    let kp = encrypt.then(EphemeralKeyPair::generate);
    let transport = Arc::new(Transport::bind(&relays, use_n1).await.map_err(|e| e.to_string())?);
    // мгновенный тикет: node_id + уже известные адреса; полные части — фоном
    let (node_id, relay_url, direct_addrs) = transport.ticket_parts_now();
    let ticket = Ticket {
        encrypted: encrypt,
        session_pubkey: kp.as_ref().map(|k| k.public_bytes()).unwrap_or([0u8; 32]),
        node_id,
        relay_url,
        direct_addrs,
    };
    let ticket_str = ticket.encode();
    let qr = ticket_qr_svg(&ticket_str);

    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(Notify::new());
    state.cancels.lock().await.insert(id, cancel.clone());
    track_active(&app, id, format!("\u{2191} {name}"));
    let rate_limit = (limit_kib > 0).then(|| limit_kib * 1024);

    // фоновый выход в сеть: когда endpoint online — шлём в UI статус и
    // обновлённый тикет (с relay и прямыми адресами) + новый QR
    {
        let tr = transport.clone();
        let app2 = app.clone();
        let session_pubkey = ticket.session_pubkey;
        tauri::async_runtime::spawn(async move {
            let (node_id, relay_url, direct_addrs) = tr.ticket_parts().await;
            let t2 = Ticket { encrypted: encrypt, session_pubkey, node_id, relay_url, direct_addrs };
            let ts = t2.encode();
            let qr2 = ticket_qr_svg(&ts);
            emit(&app2, json!({ "id": id, "kind": "net-online", "ticket": ts, "qrSvg": qr2 }));
        });
    }

    let dto = json!({
        "id": id, "ticket": ticket_str, "qrSvg": qr, "name": name,
        "size": size, "fileCount": file_count, "multi": multi,
    });
    let job = Arc::new(SendJob {
        paths, name, size, file_count, single_file, compress, encrypted: encrypt, rate_limit,
    });
    if multi {
        // мульти-режим: тикет живёт до отмены, каждый получатель — своя сессия
        tauri::async_runtime::spawn(send_multi_loop(app, id, transport, kp, job, cancel));
    } else {
        tauri::async_runtime::spawn(async move {
            let res = tokio::select! {
                r = send_session(&app, id, transport, kp, &job) => r,
                _ = cancel.notified() => Err("cancelled".to_string()),
            };
            let ok = res.is_ok();
            let st = app.state::<UiState>();
            st.cancels.lock().await.remove(&id);
            let paths_str: Vec<String> =
                job.paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
            finish_transfer(&app, id, "send", &job.name, job.size, Some(paths_str), None, None, res);
            // файл из Android share-кэша больше не нужен — чистим
            if ok {
                cleanup_shared_paths(&app, &job.paths);
            }
        });
    }
    Ok(dto)
}

/// Параметры отправки, общие для всех получателей одного тикета.
struct SendJob {
    paths: Vec<PathBuf>,
    name: String,
    size: u64,
    file_count: u32,
    single_file: bool,
    compress: bool,
    encrypted: bool,
    rate_limit: Option<u64>,
}

/// Классический режим: один получатель, транспорт закрывается после сессии.
async fn send_session(
    app: &AppHandle,
    id: u64,
    transport: Arc<Transport>,
    kp: Option<EphemeralKeyPair>,
    job: &SendJob,
) -> Result<(), String> {
    emit(app, json!({ "id": id, "kind": "waiting" }));
    let conn = transport.accept_one().await.map_err(|e| e.to_string())?;
    let res = send_to_conn(app, id, conn, kp.as_ref(), job).await;
    transport.close().await;
    res
}

/// Мульти-режим: тикет живёт до отмены отправителем; каждый подключившийся
/// получатель обслуживается независимой сессией (свой sub_id и строка в UI).
async fn send_multi_loop(
    app: AppHandle,
    parent_id: u64,
    transport: Arc<Transport>,
    kp: Option<EphemeralKeyPair>,
    job: Arc<SendJob>,
    cancel: Arc<Notify>,
) {
    let kp = kp.map(Arc::new);
    let ok_count = Arc::new(AtomicU64::new(0));
    let mut subs: Vec<Arc<Notify>> = Vec::new();
    emit(&app, json!({ "id": parent_id, "kind": "waiting", "multi": true }));
    loop {
        let conn = tokio::select! {
            c = transport.accept_one() => match c {
                Ok(c) => c,
                Err(_) => break,
            },
            _ = cancel.notified() => break,
        };
        let peer = conn.remote_id().fmt_short().to_string();
        let sub_id;
        let sub_cancel = Arc::new(Notify::new());
        subs.push(sub_cancel.clone());
        {
            let st = app.state::<UiState>();
            sub_id = st.next_id.fetch_add(1, Ordering::Relaxed);
            st.cancels.lock().await.insert(sub_id, sub_cancel.clone());
        }
        emit(&app, json!({ "id": sub_id, "kind": "sub-start", "parent": parent_id,
            "peer": peer, "name": job.name, "size": job.size }));
        track_active(&app, sub_id, format!("\u{2191} {} \u{2192} {peer}", job.name));
        let app2 = app.clone();
        let job2 = job.clone();
        let kp2 = kp.clone();
        let okc = ok_count.clone();
        tauri::async_runtime::spawn(async move {
            let res = tokio::select! {
                r = send_to_conn(&app2, sub_id, conn, kp2.as_deref(), &job2) => r,
                _ = sub_cancel.notified() => Err("cancelled".to_string()),
            };
            if res.is_ok() {
                okc.fetch_add(1, Ordering::Relaxed);
            }
            let st = app2.state::<UiState>();
            st.cancels.lock().await.remove(&sub_id);
            let paths_str: Vec<String> =
                job2.paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
            finish_transfer(&app2, sub_id, "send", &job2.name, job2.size, Some(paths_str), None, None, res);
        });
    }
    // тикет закрыт: аккуратно останавливаем незавершённые под-передачи
    for c in &subs {
        c.notify_waiters();
    }
    transport.close().await;
    {
        let st = app.state::<UiState>();
        st.cancels.lock().await.remove(&parent_id);
    }
    untrack_active(&app, parent_id);
    emit(&app, json!({ "id": parent_id, "kind": "cancelled" }));
    // share-кэш чистим после закрытия тикета, если хоть одна отправка удалась
    if ok_count.load(Ordering::Relaxed) > 0 {
        cleanup_shared_paths(&app, &job.paths);
    }
}

/// Одна сессия отправки поверх готового QUIC-соединения.
async fn send_to_conn(
    app: &AppHandle,
    id: u64,
    conn: Connection,
    kp: Option<&EphemeralKeyPair>,
    job: &SendJob,
) -> Result<(), String> {
    emit(app, json!({ "id": id, "kind": "connected", "peer": conn.remote_id().fmt_short().to_string() }));
    let (tx, mut rx) = conn.accept_bi().await.map_err(|e| e.to_string())?;

    let hs_frame = proto::read_frame(&mut rx).await.map_err(|e| e.to_string())?;
    if hs_frame.frame_type != FrameType::Handshake {
        return Err("expected Handshake frame".to_string());
    }
    let hs: proto::HandshakePayload =
        proto::decode_payload(&hs_frame.payload).map_err(|e| e.to_string())?;
    if hs.protocol_version != proto::PROTOCOL_VERSION {
        return Err(format!("incompatible receiver protocol version: {}", hs.protocol_version));
    }
    let session = match kp {
        Some(k) => Some(
            k.derive_session_shared(&hs.receiver_pubkey, Role::Sender)
                .map_err(|e| e.to_string())?,
        ),
        None => None,
    };
    send_body(app, id, conn, tx, rx, session, job).await
}

/// Тело отправки после установления сессии (общее для тикетов и push-режима).
async fn send_body(
    app: &AppHandle,
    id: u64,
    conn: Connection,
    mut tx: SendStream,
    mut rx: RecvStream,
    session: Option<netdrop_core::crypto::SessionKeys>,
    job: &SendJob,
) -> Result<(), String> {
    let chunk = proto::optimal_chunk_size(job.size);
    let (mut sealer, mut opener) = match &session {
        Some(s) => (Some(s.sealer()), Some(s.opener())),
        None => (None, None),
    };
    if let Some(s) = &session {
        emit(app, json!({ "id": id, "kind": "fingerprint", "fp": s.fingerprint() }));
    }

    let meta = proto::MetaInfo {
        name: job.name.clone(),
        size: job.size,
        is_dir: !job.single_file,
        file_count: job.file_count,
        compressed: job.compress,
        encrypted: job.encrypted,
        chunk_size: chunk as u32,
    };
    let payload = proto::encode_payload(&meta).map_err(|e| e.to_string())?;
    let mf = seal_frame(&mut sealer, FrameType::MetaInfo, &payload).map_err(|e| e.to_string())?;
    proto::write_frame(&mut tx, &mf).await.map_err(|e| e.to_string())?;

    let resp = proto::read_frame(&mut rx).await.map_err(|e| e.to_string())?;
    let offset = match resp.frame_type {
        FrameType::Ack => {
            let ack: proto::AckPayload =
                proto::decode_payload(&open_frame(&mut opener, &resp).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if job.single_file && ack.received_bytes > 0 && ack.received_bytes < job.size {
                ack.received_bytes
            } else {
                0
            }
        }
        FrameType::Error => {
            let e: proto::ErrorPayload =
                proto::decode_payload(&resp.payload).map_err(|e| e.to_string())?;
            return Err(format!("receiver rejected: {}", e.message));
        }
        _ => return Err("unexpected frame instead of Ack".to_string()),
    };
    if offset > 0 {
        emit(app, json!({ "id": id, "kind": "resuming", "offset": offset }));
    }

    emit(app, json!({ "id": id, "kind": "accepted" }));
    let progress = Arc::new(AtomicU64::new(0));
    let watcher = watch_progress(app.clone(), id, progress.clone(), job.size);

    let result = if job.single_file {
        let mut file = tokio::fs::File::open(&job.paths[0]).await.map_err(|e| e.to_string())?;
        let preseed = if offset > 0 {
            Some(Preseed::from_reader(&mut file, offset).await.map_err(|e| e.to_string())?)
        } else {
            None
        };
        pipeline::send_stream(file, &mut tx, chunk, job.compress, &mut sealer, progress.clone(), preseed, job.rate_limit)
            .await
            .map_err(|e| e.to_string())
    } else {
        // мультифайл/папки → единый tar-поток (без временного архива)
        let (tar_read, tar_write) = tokio::io::duplex(1 << 20);
        let src_paths = job.paths.clone();
        let tar_task = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let bridge = SyncIoBridge::new(tar_write);
            let mut builder = tar::Builder::new(bridge);
            for p in &src_paths {
                let entry_name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "item".to_string());
                if p.is_dir() {
                    builder.append_dir_all(&entry_name, p)?;
                } else {
                    builder.append_path_with_name(p, &entry_name)?;
                }
            }
            let mut bridge = builder.into_inner()?;
            std::io::Write::flush(&mut bridge)?;
            Ok(())
        });
        let res = pipeline::send_stream(
            tar_read, &mut tx, chunk, job.compress, &mut sealer, progress.clone(), None, job.rate_limit,
        )
        .await
        .map_err(|e| e.to_string());
        let tar_res = tar_task.await;
        match (res, tar_res) {
            (Ok(o), Ok(Ok(()))) => Ok(o),
            (Ok(_), Ok(Err(e))) => Err(format!("packing failed: {e}")),
            (Ok(_), Err(e)) => Err(format!("tar task panicked: {e}")),
            (Err(e), _) => Err(e),
        }
    };
    watcher.abort();
    let outcome = result?;
    tx.finish().map_err(|e| e.to_string())?;

    let fin = proto::read_frame(&mut rx).await.map_err(|e| e.to_string())?;
    match fin.frame_type {
        FrameType::Ack => {
            let _ = open_frame(&mut opener, &fin).map_err(|e| e.to_string())?;
            emit(app, json!({ "id": id, "kind": "done", "bytes": outcome.bytes, "name": job.name, "total": job.size }));
        }
        FrameType::Error => {
            let e: proto::ErrorPayload =
                proto::decode_payload(&fin.payload).map_err(|e| e.to_string())?;
            return Err(format!("receiver reported: {}", e.message));
        }
        _ => return Err("unexpected final frame".to_string()),
    }
    conn.close(0u32.into(), b"done");
    Ok(())
}

// ---------- Доверенные устройства (постоянный ключ, сопряжение, слушатель) ----------

fn data_file(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(name))
}

/// Постоянный секрет устройства (создаётся при первом обращении).
fn device_secret(app: &AppHandle) -> Result<[u8; 32], String> {
    let path = data_file(app, "device_key.bin").ok_or("no data dir")?;
    if let Ok(b) = std::fs::read(&path) {
        if b.len() == 32 {
            let mut k = [0u8; 32];
            k.copy_from_slice(&b);
            return Ok(k);
        }
    }
    let mut k = [0u8; 32];
    getrandom::getrandom(&mut k).map_err(|e| e.to_string())?;
    std::fs::write(&path, k).map_err(|e| e.to_string())?;
    Ok(k)
}

fn hex32(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex32(s: &str) -> Result<[u8; 32], String> {
    let s = s.trim();
    if s.len() != 64 {
        return Err("bad id length".to_string());
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn default_device_name() -> String {
    #[cfg(target_os = "android")]
    return "Android".to_string();
    #[cfg(not(target_os = "android"))]
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "NetDrop".to_string())
}

/// device.json: {"name": ..., "listen": bool, "outDir": ...}
fn load_device_cfg(app: &AppHandle) -> serde_json::Value {
    data_file(app, "device.json")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({ "name": default_device_name(), "listen": false, "outDir": "" }))
}

fn save_device_cfg(app: &AppHandle, cfg: &serde_json::Value) {
    if let Some(p) = data_file(app, "device.json") {
        let _ = std::fs::write(p, cfg.to_string());
    }
}

/// paired.json: [{"id": hex64, "name": ...}]
fn load_paired(app: &AppHandle) -> Vec<serde_json::Value> {
    data_file(app, "paired.json")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_paired(app: &AppHandle, list: &[serde_json::Value]) {
    if let Some(p) = data_file(app, "paired.json") {
        if let Ok(s) = serde_json::to_string(list) {
            let _ = std::fs::write(p, s);
        }
    }
}

fn add_paired(app: &AppHandle, id_hex: &str, name: &str) {
    let mut list = load_paired(app);
    list.retain(|d| d["id"].as_str() != Some(id_hex));
    list.push(json!({ "id": id_hex, "name": name }));
    save_paired(app, &list);
    emit(app, json!({ "kind": "devices-changed" }));
}

#[tauri::command(async)]
fn device_info(app: AppHandle) -> Result<serde_json::Value, String> {
    let secret = device_secret(&app)?;
    let id = netdrop_core::network::transport::node_id_from_secret(&secret);
    let cfg = load_device_cfg(&app);
    Ok(json!({
        "id": hex32(&id),
        "name": cfg["name"].as_str().unwrap_or("NetDrop"),
        "listen": cfg["listen"].as_bool().unwrap_or(false),
        "devices": load_paired(&app),
    }))
}

#[tauri::command(async)]
fn list_devices(app: AppHandle) -> Vec<serde_json::Value> {
    load_paired(&app)
}

#[tauri::command(async)]
fn remove_device(app: AppHandle, id: String) {
    let mut list = load_paired(&app);
    list.retain(|d| d["id"].as_str() != Some(id.as_str()));
    save_paired(&app, &list);
}

/// Сохранить имя/режим слушателя; управляет запуском/остановкой слушателя.
#[tauri::command]
async fn set_device_cfg(
    app: AppHandle,
    state: State<'_, UiState>,
    name: String,
    listen: bool,
    out_dir: String,
) -> Result<(), String> {
    let cfg = json!({ "name": name, "listen": listen, "outDir": out_dir });
    save_device_cfg(&app, &cfg);
    if listen {
        ensure_listener(&app, &state).await?;
    } else if let Some(stop) = state.listener_stop.lock().await.take() {
        stop.notify_waiters();
    }
    Ok(())
}

/// QR/код сопряжения: ndp1:<node_id_hex>:<secret_hex>:<имя>
#[tauri::command]
async fn pair_qr(app: AppHandle, state: State<'_, UiState>) -> Result<serde_json::Value, String> {
    let secret_key = device_secret(&app)?;
    let id = netdrop_core::network::transport::node_id_from_secret(&secret_key);
    let mut secret = [0u8; 16];
    getrandom::getrandom(&mut secret).map_err(|e| e.to_string())?;
    *state.pair_secret.lock().unwrap() = Some(secret);
    ensure_listener(&app, &state).await?;
    let cfg = load_device_cfg(&app);
    let name = cfg["name"].as_str().unwrap_or("NetDrop").to_string();
    let sec_hex: String = secret.iter().map(|x| format!("{x:02x}")).collect();
    let mut code = format!("ndp1:{}:{}:{}", hex32(&id), sec_hex, name);
    // облако подключено → кладём в QR ссылку на ящик + одноразовый ключ пары:
    // отсканировавший сразу знает, где проверять почту, даже без сети
    if let Some(si) = mailbox::self_info(&app) {
        let url = si["url"].as_str().unwrap_or("");
        if !url.is_empty() {
            if let Some(key) = mailbox::qr_key_new(&app) {
                use base64::Engine as _;
                code = format!(
                    "{code}|m1|{}|{}|{key}",
                    si["provider"].as_str().unwrap_or(""),
                    cloud::B64.encode(url)
                );
            }
        }
    }
    Ok(json!({ "code": code, "qrSvg": ticket_qr_svg(&code) }))
}

/// Сопряжение по коду с другого устройства (взаимное доверие).
#[tauri::command]
async fn add_device_by_code(app: AppHandle, code: String) -> Result<serde_json::Value, String> {
    let rest = code.trim().strip_prefix("ndp1:").ok_or("not a pairing code")?;
    let (rest, mextra) = match rest.split_once("|m1|") {
        Some((a, b)) => (a, Some(b.to_string())),
        None => (rest, None),
    };
    let mut parts = rest.splitn(3, ':');
    let id_hex = parts.next().ok_or("bad code")?.to_string();
    let sec_hex = parts.next().ok_or("bad code")?;
    let their_name = parts.next().unwrap_or("Device").to_string();
    let target = from_hex32(&id_hex)?;
    if sec_hex.len() != 32 {
        return Err("bad secret".to_string());
    }
    let mut secret = [0u8; 16];
    for i in 0..16 {
        secret[i] = u8::from_str_radix(&sec_hex[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    let my = device_secret(&app)?;
    let my_recip: String = hex32(&netdrop_core::network::transport::node_id_from_secret(&my))
        .chars()
        .take(16)
        .collect();
    let my_name = load_device_cfg(&app)["name"].as_str().unwrap_or("NetDrop").to_string();
    let transport = Transport::bind_with_secret(&[], true, my).await.map_err(|e| e.to_string())?;
    let conn = transport.connect_id(target).await.map_err(|e| e.to_string())?;
    let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
    let req = proto::PairRequestPayload { secret, name: my_name };
    let payload = proto::encode_payload(&req).map_err(|e| e.to_string())?;
    proto::write_frame(&mut tx, &proto::Frame::plain(FrameType::PairRequest, payload))
        .await
        .map_err(|e| e.to_string())?;
    let resp = proto::read_frame(&mut rx).await.map_err(|e| e.to_string())?;
    let name = match resp.frame_type {
        FrameType::PairAck => {
            let ack: proto::PairAckPayload =
                proto::decode_payload(&resp.payload).map_err(|e| e.to_string())?;
            ack.name
        }
        FrameType::Error => {
            let e: proto::ErrorPayload =
                proto::decode_payload(&resp.payload).map_err(|e| e.to_string())?;
            return Err(format!("pairing rejected: {}", e.message));
        }
        _ => their_name,
    };
    conn.close(0u32.into(), b"paired");
    transport.close().await;
    add_paired(&app, &id_hex, &name);
    emit(&app, json!({ "kind": "device-paired", "id": id_hex, "name": name }));
    // из QR: где и как проверять почту устройства 1 (работает до announce)
    if let Some(mx) = mextra {
        let mut it = mx.splitn(3, '|');
        let prov = it.next().unwrap_or("").to_string();
        let url_b64 = it.next().unwrap_or("");
        let key = it.next().unwrap_or("").to_string();
        use base64::Engine as _;
        if let Ok(raw) = cloud::B64.decode(url_b64) {
            if let Ok(url) = String::from_utf8(raw) {
                if !url.is_empty() && !key.is_empty() {
                    let mut list = load_paired(&app);
                    for d in list.iter_mut() {
                        if d["id"].as_str() == Some(id_hex.as_str()) {
                            d["mbox"] = json!({
                                "provider": prov, "url": url, "idx": "",
                                "key": key, "acct": "", "sub": my_recip,
                            });
                        }
                    }
                    save_paired(&app, &list);
                }
            }
        }
    }
    Ok(json!({ "id": id_hex, "name": name }))
}

/// Запустить слушатель доверенных устройств (если ещё не запущен).
async fn ensure_listener(app: &AppHandle, state: &State<'_, UiState>) -> Result<(), String> {
    let mut guard = state.listener_stop.lock().await;
    if guard.is_some() {
        return Ok(());
    }
    let stop = Arc::new(Notify::new());
    *guard = Some(stop.clone());
    drop(guard);
    let secret = device_secret(app)?;
    let transport = Arc::new(
        Transport::bind_with_secret(&[], true, secret).await.map_err(|e| e.to_string())?,
    );
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let conn = tokio::select! {
                c = transport.accept_one() => match c {
                    Ok(c) => c,
                    Err(_) => break,
                },
                _ = stop.notified() => break,
            };
            tauri::async_runtime::spawn(handle_incoming(app2.clone(), conn));
        }
        transport.close().await;
    });
    Ok(())
}

/// Обработка входящего соединения слушателя: сопряжение или push-приём.
async fn handle_incoming(app: AppHandle, conn: Connection) {
    let Ok((mut tx, mut rx)) = conn.accept_bi().await else { return };
    let Ok(first) = proto::read_frame(&mut rx).await else { return };
    let peer_hex = hex32(conn.remote_id().as_bytes());
    match first.frame_type {
        FrameType::Ping => {
            // эхо-ответ: устройство онлайн и слушает
            let _ = proto::write_frame(&mut tx, &proto::Frame::plain(FrameType::Ping, Vec::new())).await;
            let _ = tx.finish();
        }
        FrameType::PairRequest => {
            let Ok(req) = proto::decode_payload::<proto::PairRequestPayload>(&first.payload) else { return };
            let expected = app.state::<UiState>().pair_secret.lock().unwrap().take();
            if expected != Some(req.secret) {
                let err = proto::ErrorPayload { code: proto::ERR_REJECTED, message: "bad pairing secret".into() };
                if let Ok(p) = proto::encode_payload(&err) {
                    let _ = proto::write_frame(&mut tx, &proto::Frame::plain(FrameType::Error, p)).await;
                }
                return;
            }
            add_paired(&app, &peer_hex, &req.name);
            let my_name = load_device_cfg(&app)["name"].as_str().unwrap_or("NetDrop").to_string();
            let ack = proto::PairAckPayload { name: my_name };
            if let Ok(p) = proto::encode_payload(&ack) {
                let _ = proto::write_frame(&mut tx, &proto::Frame::plain(FrameType::PairAck, p)).await;
            }
            emit(&app, json!({ "kind": "device-paired", "id": peer_hex, "name": req.name }));
        }
        FrameType::TextPush => {
            // текст от доверенного устройства → в буфер обмена получателя
            let Ok(tp) = proto::decode_payload::<proto::TextPushPayload>(&first.payload) else { return };
            let paired = load_paired(&app);
            let peer_name = paired
                .iter()
                .find(|d| d["id"].as_str() == Some(peer_hex.as_str()))
                .and_then(|d| d["name"].as_str().map(String::from));
            let Some(peer_name) = peer_name else { return };
            let _ = proto::write_frame(&mut tx, &proto::Frame::plain(FrameType::Ping, Vec::new())).await;
            // служебные сообщения облачной почты (ndmx1:…) — не в буфер обмена
            if mailbox::handle_control(&app, &peer_hex, &tp.text) {
                return;
            }
            emit(&app, json!({ "kind": "text-received", "text": tp.text, "peer": peer_name }));
        }
        FrameType::PushHello => {
            let Ok(hello) = proto::decode_payload::<proto::PushHelloPayload>(&first.payload) else { return };
            let paired = load_paired(&app);
            let peer_name = paired
                .iter()
                .find(|d| d["id"].as_str() == Some(peer_hex.as_str()))
                .and_then(|d| d["name"].as_str().map(String::from));
            let Some(peer_name) = peer_name else {
                let err = proto::ErrorPayload { code: proto::ERR_REJECTED, message: "device not paired".into() };
                if let Ok(p) = proto::encode_payload(&err) {
                    let _ = proto::write_frame(&mut tx, &proto::Frame::plain(FrameType::Error, p)).await;
                }
                return;
            };
            let kp = hello.encrypted.then(EphemeralKeyPair::generate);
            let hs = proto::HandshakePayload {
                protocol_version: proto::PROTOCOL_VERSION,
                receiver_pubkey: kp.as_ref().map(|k| k.public_bytes()).unwrap_or([0u8; 32]),
            };
            let Ok(payload) = proto::encode_payload(&hs) else { return };
            if proto::write_frame(&mut tx, &proto::Frame::plain(FrameType::Handshake, payload)).await.is_err() {
                return;
            }
            let session = match kp {
                Some(k) => match k.derive_session(&hello.sender_pubkey, Role::Receiver) {
                    Ok(s) => Some(s),
                    Err(_) => return,
                },
                None => None,
            };
            let (sealer, mut opener) = match &session {
                Some(s) => (Some(s.sealer()), Some(s.opener())),
                None => (None, None),
            };
            let Ok(mf) = proto::read_frame(&mut rx).await else { return };
            if mf.frame_type != FrameType::MetaInfo {
                return;
            }
            let Ok(raw) = open_frame(&mut opener, &mf) else { return };
            let Ok(meta) = proto::decode_payload::<proto::MetaInfo>(&raw) else { return };

            let cfg = load_device_cfg(&app);
            let mut od = PathBuf::from(cfg["outDir"].as_str().unwrap_or("").trim());
            if od.as_os_str().is_empty()
                || od == Path::new(".")
                || od == Path::new("/")
                || tokio::fs::create_dir_all(&od).await.is_err()
            {
                od = PathBuf::from(default_out_dir(app.clone()));
            }
            let (id, cancel) = {
                let st = app.state::<UiState>();
                let id = st.next_id.fetch_add(1, Ordering::Relaxed);
                let cancel = Arc::new(Notify::new());
                st.cancels.lock().await.insert(id, cancel.clone());
                (id, cancel)
            };
            emit(&app, json!({ "id": id, "kind": "recv-start",
                "name": meta.name, "size": meta.size, "peer": peer_name }));
            track_active(&app, id, format!("\u{2193} {} \u{2190} {peer_name}", meta.name));
            let name = meta.name.clone();
            let size = meta.size;
            let pending = Pending { transport: None, conn, tx, rx, sealer, opener, meta };
            let res = tokio::select! {
                r = receive_session(&app, id, pending, od) => r,
                _ = cancel.notified() => Err("cancelled".to_string()),
            };
            app.state::<UiState>().cancels.lock().await.remove(&id);
            let (share, rpath) = match &res {
                Ok((u, p)) => (u.clone(), Some(p.clone())),
                Err(_) => (None, None),
            };
            finish_transfer(&app, id, "recv", &name, size, None, share, rpath, res.map(|_| ()));
        }
        _ => {}
    }
}

/// Android: скопировать content://-URI (из системного пикера) в кэш-файл.
/// Пикер Android возвращает URI, а не путь — конвертируем как share intent.
#[cfg(target_os = "android")]
#[tauri::command]
async fn stash_content_uri(app: AppHandle, uri: String) -> Result<String, String> {
    use tauri_plugin_android_fs::{AndroidFsExt, FsUri};
    let fs_uri: FsUri = serde_json::from_value(json!({ "uri": uri, "documentTopTreeUri": null }))
        .map_err(|e| e.to_string())?;
    let api = app.android_fs_async();
    let name = api.get_name(&fs_uri).await.unwrap_or_else(|_| "file".to_string());
    let name = sanitize_name(&name);
    let mut src = api.open_file_readable(&fs_uri).await.map_err(|e| e.to_string())?;
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("shared");
    tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
    let dest = unique_path(&dir, &name);
    let dest2 = dest.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let mut out = std::fs::File::create(&dest2).map_err(|e| e.to_string())?;
        std::io::copy(&mut src, &mut out).map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(dest.to_string_lossy().to_string())
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn stash_content_uri(_uri: String) -> Result<String, String> {
    Err("content URIs are only supported on Android".to_string())
}

/// Android: открыть принятый файл в подходящем приложении (ACTION_VIEW).
#[cfg(target_os = "android")]
#[tauri::command]
async fn open_received_file(app: AppHandle, uri: String) -> Result<(), String> {
    use tauri_plugin_android_fs::{AndroidFsExt, FsUri};
    let uri: FsUri = serde_json::from_str(&uri).map_err(|e| e.to_string())?;
    app.android_fs_async()
        .opener()
        .open_file(&uri)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn open_received_file(_uri: String) -> Result<(), String> {
    Err("open is only supported on Android".to_string())
}

/// Отправить текст/ссылку на доверенное устройство (буфер обмена).
#[tauri::command]
async fn send_text_to_device(app: AppHandle, device_id: String, text: String) -> Result<(), String> {
    let target = from_hex32(&device_id)?;
    let secret = device_secret(&app)?;
    let transport = Transport::bind_with_secret(&[], true, secret).await.map_err(|e| e.to_string())?;
    let conn = transport.connect_id(target).await.map_err(|e| e.to_string())?;
    let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
    let payload = proto::encode_payload(&proto::TextPushPayload { text }).map_err(|e| e.to_string())?;
    proto::write_frame(&mut tx, &proto::Frame::plain(FrameType::TextPush, payload))
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), proto::read_frame(&mut rx))
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|e| e.to_string())?;
    conn.close(0u32.into(), b"text");
    transport.close().await;
    Ok(())
}

/// Ошибка сетевой фазы — имеет смысл повторить попытку.
fn is_retryable(e: &str) -> bool {
    let e = e.to_lowercase();
    e.contains("timed out")
        || e.contains("no addressing")
        || e.contains("connect")
        || e.contains("connection lost")
        || e.contains("connection reset")
}

/// Отправка на доверенное устройство (без тикета): push-режим.
#[tauri::command]
async fn send_to_device(
    app: AppHandle,
    state: State<'_, UiState>,
    device_id: String,
    device_name: String,
    paths: Vec<String>,
    encrypt: bool,
    compress: bool,
    limit_kib: u64,
) -> Result<serde_json::Value, String> {
    if paths.is_empty() {
        return Err("no paths selected".to_string());
    }
    let target = from_hex32(&device_id)?;
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    for p in &paths {
        if !tokio::fs::try_exists(p).await.unwrap_or(false) {
            return Err(format!("path not found: {}", p.display()));
        }
    }
    let single_file = paths.len() == 1
        && tokio::fs::metadata(&paths[0]).await.map(|m| m.is_file()).unwrap_or(false);
    let name = bundle_name(&paths);
    let stats_paths = paths.clone();
    let (size, file_count) = tokio::task::spawn_blocking(move || paths_stats(&stats_paths))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(Notify::new());
    state.cancels.lock().await.insert(id, cancel.clone());
    track_active(&app, id, format!("\u{2191} {name} \u{2192} {device_name}"));
    let rate_limit = (limit_kib > 0).then(|| limit_kib * 1024);
    let job = Arc::new(SendJob {
        paths, name: name.clone(), size, file_count, single_file, compress,
        encrypted: encrypt, rate_limit,
    });
    let secret = device_secret(&app)?;
    let dto = json!({ "id": id, "name": name, "size": size, "peer": device_name });
    tauri::async_runtime::spawn(async move {
        // автоповтор сетевых сбоев: до 3 попыток с паузой 3с
        let mut res: Result<(), String> = Err(String::new());
        for attempt in 0..3u32 {
            if attempt > 0 {
                emit(&app, json!({ "id": id, "kind": "retrying", "attempt": attempt + 1 }));
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {},
                    _ = cancel.notified() => { res = Err("cancelled".to_string()); break; }
                }
            }
            res = tokio::select! {
                r = push_send(&app, id, target, secret, encrypt, &job) => r,
                _ = cancel.notified() => Err("cancelled".to_string()),
            };
            match &res {
                Ok(_) => break,
                Err(e) if e == "cancelled" => break,
                Err(e) if attempt < 2 && is_retryable(e) => continue,
                Err(_) => break,
            }
        }
        let ok = res.is_ok();
        let st = app.state::<UiState>();
        st.cancels.lock().await.remove(&id);
        let paths_str: Vec<String> =
            job.paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
        finish_transfer(&app, id, "send", &job.name, job.size, Some(paths_str), None, None, res);
        if ok {
            cleanup_shared_paths(&app, &job.paths);
        }
    });
    Ok(dto)
}

async fn push_send(
    app: &AppHandle,
    id: u64,
    target: [u8; 32],
    secret: [u8; 32],
    encrypt: bool,
    job: &SendJob,
) -> Result<(), String> {
    emit(app, json!({ "id": id, "kind": "waiting" }));
    let kp = encrypt.then(EphemeralKeyPair::generate);
    let transport = Transport::bind_with_secret(&[], true, secret).await.map_err(|e| e.to_string())?;
    let conn = transport.connect_id(target).await.map_err(|e| e.to_string())?;
    emit(app, json!({ "id": id, "kind": "connected", "peer": conn.remote_id().fmt_short().to_string() }));
    let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
    let hello = proto::PushHelloPayload {
        protocol_version: proto::PROTOCOL_VERSION,
        sender_pubkey: kp.as_ref().map(|k| k.public_bytes()).unwrap_or([0u8; 32]),
        encrypted: encrypt,
    };
    let payload = proto::encode_payload(&hello).map_err(|e| e.to_string())?;
    proto::write_frame(&mut tx, &proto::Frame::plain(FrameType::PushHello, payload))
        .await
        .map_err(|e| e.to_string())?;
    let resp = proto::read_frame(&mut rx).await.map_err(|e| e.to_string())?;
    let hs: proto::HandshakePayload = match resp.frame_type {
        FrameType::Handshake => proto::decode_payload(&resp.payload).map_err(|e| e.to_string())?,
        FrameType::Error => {
            let e: proto::ErrorPayload =
                proto::decode_payload(&resp.payload).map_err(|e| e.to_string())?;
            return Err(format!("device rejected: {}", e.message));
        }
        _ => return Err("unexpected frame instead of Handshake".to_string()),
    };
    let session = match kp {
        Some(k) => Some(k.derive_session(&hs.receiver_pubkey, Role::Sender).map_err(|e| e.to_string())?),
        None => None,
    };
    let res = send_body(app, id, conn, tx, rx, session, job).await;
    transport.close().await;
    res
}

/// Проверка доступности доверенных устройств: {id_hex: bool}.
#[tauri::command]
async fn ping_devices(app: AppHandle) -> Result<serde_json::Value, String> {
    let list = load_paired(&app);
    if list.is_empty() {
        return Ok(json!({}));
    }
    let transport = Arc::new(Transport::bind(&[], true).await.map_err(|e| e.to_string())?);
    let mut tasks = Vec::new();
    for d in &list {
        let Some(id_hex) = d["id"].as_str().map(String::from) else { continue };
        let tr = transport.clone();
        tasks.push(tauri::async_runtime::spawn(async move {
            let attempt = async {
                let target = from_hex32(&id_hex).ok()?;
                let conn = tr.connect_id(target).await.ok()?;
                let (mut tx, mut rx) = conn.open_bi().await.ok()?;
                proto::write_frame(&mut tx, &proto::Frame::plain(FrameType::Ping, Vec::new()))
                    .await
                    .ok()?;
                let f = proto::read_frame(&mut rx).await.ok()?;
                conn.close(0u32.into(), b"ping");
                (f.frame_type == FrameType::Ping).then_some(())
            };
            let ok = tokio::time::timeout(std::time::Duration::from_secs(6), attempt)
                .await
                .ok()
                .flatten()
                .is_some();
            (id_hex, ok)
        }));
    }
    let mut out = serde_json::Map::new();
    for t in tasks {
        if let Ok((id, ok)) = t.await {
            out.insert(id, json!(ok));
        }
    }
    transport.close().await;
    Ok(serde_json::Value::Object(out))
}

// ---------- Приём ----------

#[tauri::command]
async fn receive_probe(
    state: State<'_, UiState>,
    ticket: String,
) -> Result<serde_json::Value, String> {
    let t = Ticket::decode(&ticket).map_err(|e| format!("ticket: {e}"))?;
    let kp = t.encrypted.then(EphemeralKeyPair::generate);
    let transport = Transport::bind(&[], true).await.map_err(|e| e.to_string())?;
    let conn = transport.connect(&t).await.map_err(|e| e.to_string())?;
    let (mut tx, mut rx) = conn.open_bi().await.map_err(|e| e.to_string())?;

    let hs = proto::HandshakePayload {
        protocol_version: proto::PROTOCOL_VERSION,
        receiver_pubkey: kp.as_ref().map(|k| k.public_bytes()).unwrap_or([0u8; 32]),
    };
    let payload = proto::encode_payload(&hs).map_err(|e| e.to_string())?;
    proto::write_frame(&mut tx, &proto::Frame::plain(FrameType::Handshake, payload))
        .await
        .map_err(|e| e.to_string())?;

    let session = match kp {
        Some(k) => Some(k.derive_session(&t.session_pubkey, Role::Receiver).map_err(|e| e.to_string())?),
        None => None,
    };
    let (sealer, mut opener) = match &session {
        Some(s) => (Some(s.sealer()), Some(s.opener())),
        None => (None, None),
    };
    let fingerprint = session.as_ref().map(|s| s.fingerprint().to_string());

    let mf = proto::read_frame(&mut rx).await.map_err(|e| e.to_string())?;
    let meta: proto::MetaInfo = match mf.frame_type {
        FrameType::MetaInfo => {
            proto::decode_payload(&open_frame(&mut opener, &mf).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
        }
        FrameType::Error => {
            let e: proto::ErrorPayload =
                proto::decode_payload(&mf.payload).map_err(|e| e.to_string())?;
            return Err(format!("sender reported: {}", e.message));
        }
        _ => return Err("expected MetaInfo frame".to_string()),
    };

    let sender_id: String = t.node_id.iter().map(|b| format!("{b:02x}")).collect();
    let dto = json!({
        "name": meta.name, "size": meta.size, "isDir": meta.is_dir,
        "fileCount": meta.file_count, "encrypted": meta.encrypted,
        "compressed": meta.compressed, "fingerprint": fingerprint,
        "senderId": sender_id,
    });
    *state.pending.lock().await = Some(Pending { transport: Some(transport), conn, tx, rx, sealer, opener, meta });
    Ok(dto)
}

#[tauri::command]
async fn receive_accept(
    app: AppHandle,
    state: State<'_, UiState>,
    out_dir: String,
) -> Result<u64, String> {
    let pending = state.pending.lock().await.take().ok_or("no pending transfer")?;
    // защита от пустого/невалидного пути ("."/"/" → read-only fs на Android)
    let mut od = PathBuf::from(out_dir.trim());
    let bad = od.as_os_str().is_empty()
        || od == Path::new(".")
        || od == Path::new("/")
        || tokio::fs::create_dir_all(&od).await.is_err();
    if bad {
        od = PathBuf::from(default_out_dir(app.clone()));
    }
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(Notify::new());
    state.cancels.lock().await.insert(id, cancel.clone());
    track_active(&app, id, format!("\u{2193} {}", pending.meta.name));
    tauri::async_runtime::spawn(async move {
        let name = pending.meta.name.clone();
        let size = pending.meta.size;
        let res = tokio::select! {
            r = receive_session(&app, id, pending, od) => r,
            _ = cancel.notified() => Err("cancelled".to_string()),
        };
        let st = app.state::<UiState>();
        st.cancels.lock().await.remove(&id);
        let (share, rpath) = match &res {
            Ok((u, p)) => (u.clone(), Some(p.clone())),
            Err(_) => (None, None),
        };
        finish_transfer(&app, id, "recv", &name, size, None, share, rpath, res.map(|_| ()));
    });
    Ok(id)
}

async fn receive_session(app: &AppHandle, id: u64, p: Pending, out_dir: PathBuf) -> Result<(Option<String>, String), String> {
    let Pending { transport, conn, mut tx, mut rx, mut sealer, mut opener, meta } = p;
    let chunk = (meta.chunk_size as usize).min(proto::MAX_CHUNK_SIZE);
    tokio::fs::create_dir_all(&out_dir).await.ok();

    // Докачка (только одиночные файлы): *.ndpart + сайдкар — как в CLI.
    let safe_name = sanitize_name(&meta.name);
    let part_path = out_dir.join(format!("{safe_name}.ndpart"));
    let sidecar_path = out_dir.join(format!("{safe_name}.ndpart.json"));
    let mut resume_offset = 0u64;
    let mut preseed: Option<Preseed> = None;
    if !meta.is_dir {
        if let (Ok(part_meta), Ok(raw)) = (
            tokio::fs::metadata(&part_path).await,
            tokio::fs::read_to_string(&sidecar_path).await,
        ) {
            let same = serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .map(|v| {
                    v["name"].as_str() == Some(meta.name.as_str())
                        && v["size"].as_u64() == Some(meta.size)
                })
                .unwrap_or(false);
            let plen = part_meta.len();
            if same && plen > 0 && plen < meta.size {
                let mut pf = tokio::fs::File::open(&part_path).await.map_err(|e| e.to_string())?;
                preseed = Some(Preseed::from_reader(&mut pf, plen).await.map_err(|e| e.to_string())?);
                resume_offset = plen;
                emit(app, json!({ "id": id, "kind": "resuming", "offset": plen }));
            }
        }
    }

    let ack = proto::AckPayload { received_bytes: resume_offset };
    let payload = proto::encode_payload(&ack).map_err(|e| e.to_string())?;
    let f = seal_frame(&mut sealer, FrameType::Ack, &payload).map_err(|e| e.to_string())?;
    proto::write_frame(&mut tx, &f).await.map_err(|e| e.to_string())?;

    let progress = Arc::new(AtomicU64::new(0));
    let watcher = watch_progress(app.clone(), id, progress.clone(), meta.size);

    let (result, shown_path) = if meta.is_dir {
        let (untar_read, mut untar_write) = tokio::io::duplex(1 << 20);
        let od = out_dir.clone();
        let untar_task = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let bridge = SyncIoBridge::new(untar_read);
            let mut archive = tar::Archive::new(bridge);
            archive.unpack(&od)
        });
        let res = pipeline::receive_stream(
            &mut rx, &mut untar_write, chunk, meta.compressed, &mut opener,
            progress.clone(), None,
        )
        .await
        .map_err(|e| e.to_string());
        use tokio::io::AsyncWriteExt;
        let _ = untar_write.shutdown().await;
        let tar_res = untar_task.await;
        let res = match (res, tar_res) {
            (Ok(o), Ok(Ok(()))) => Ok(o),
            (Ok(_), Ok(Err(e))) => Err(format!("unpack failed: {e}")),
            (Ok(_), Err(e)) => Err(format!("untar task panicked: {e}")),
            (Err(e), _) => Err(e),
        };
        (res, out_dir.clone())
    } else {
        // сайдкар пишем ДО приёма — обрыв оставит валидную точку докачки
        let sidecar = json!({ "name": meta.name, "size": meta.size });
        tokio::fs::write(&sidecar_path, sidecar.to_string()).await.ok();
        let mut opts = tokio::fs::OpenOptions::new();
        opts.create(true).write(true);
        if resume_offset > 0 { opts.append(true); } else { opts.truncate(true); }
        let mut file = opts.open(&part_path).await.map_err(|e| e.to_string())?;
        let res = pipeline::receive_stream(
            &mut rx, &mut file, chunk, meta.compressed, &mut opener, progress.clone(), preseed,
        )
        .await
        .map_err(|e| e.to_string());
        (res, part_path.clone())
    };
    watcher.abort();

    match result {
        Ok(outcome) => {
            let ack = proto::AckPayload { received_bytes: outcome.bytes };
            let payload = proto::encode_payload(&ack).map_err(|e| e.to_string())?;
            let f = seal_frame(&mut sealer, FrameType::Ack, &payload).map_err(|e| e.to_string())?;
            proto::write_frame(&mut tx, &f).await.map_err(|e| e.to_string())?;
            let _ = tx.finish();
            // даём отправителю дочитать финальный Ack: ждём, пока он сам
            // закроет соединение (иначе у него «connection lost»)
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), conn.closed()).await;
            let final_path = if meta.is_dir {
                shown_path
            } else {
                let dest = unique_path(&out_dir, &meta.name);
                tokio::fs::rename(&part_path, &dest).await.map_err(|e| e.to_string())?;
                let _ = tokio::fs::remove_file(&sidecar_path).await;
                dest
            };
            #[allow(unused_mut)]
            let mut share_uri: Option<String> = None;
            #[allow(unused_mut)]
            let mut shown = final_path.to_string_lossy().to_string();
            // Android: одиночный файл переносим в публичный Download (MediaStore)
            #[cfg(target_os = "android")]
            if !meta.is_dir {
                match export_to_downloads(app, &final_path, &safe_name).await {
                    Ok(u) => {
                        let _ = tokio::fs::remove_file(&final_path).await;
                        share_uri = Some(u);
                        shown = format!("Download/netdrop/{safe_name}");
                    }
                    Err(e) => eprintln!("netdrop: export to public Download failed: {e}"),
                }
            }
            emit(app, json!({ "id": id, "kind": "done", "bytes": outcome.bytes,
                "path": shown, "shareUri": share_uri, "total": meta.size }));
            conn.close(0u32.into(), b"done");
            if let Some(t) = &transport {
                t.close().await;
            }
            Ok((share_uri, shown))
        }
        Err(e) => {
            let err = proto::ErrorPayload { code: proto::ERR_INTERNAL, message: e.clone() };
            if let Ok(payload) = proto::encode_payload(&err) {
                let _ = proto::write_frame(&mut tx, &proto::Frame::plain(FrameType::Error, payload)).await;
            }
            conn.close(1u32.into(), b"error");
            if let Some(t) = &transport {
                t.close().await;
            }
            if meta.is_dir {
                Err(e)
            } else {
                // частичная копия сохранена — докачка при следующем приёме
                Err(format!("{e} — partial file kept for resume"))
            }
        }
    }
}

#[tauri::command]
async fn receive_reject(state: State<'_, UiState>) -> Result<(), String> {
    if let Some(mut p) = state.pending.lock().await.take() {
        let err = proto::ErrorPayload {
            code: proto::ERR_REJECTED,
            message: "rejected by receiver".to_string(),
        };
        if let Ok(payload) = proto::encode_payload(&err) {
            let _ = proto::write_frame(&mut p.tx, &proto::Frame::plain(FrameType::Error, payload)).await;
        }
        p.conn.close(0u32.into(), b"rejected");
        if let Some(t) = &p.transport {
            t.close().await;
        }
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // единственный экземпляр (десктоп): повторный запуск не плодит копии
    // в трее, а показывает окно уже работающего. Регистрируется ПЕРВЫМ.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        show_main(app);
    }));
    let builder = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_deep_link::init());
    // приём из системного «Поделиться» + QR-сканер + публичный Download (Android)
    #[cfg(target_os = "android")]
    let builder = builder
        .plugin(tauri_plugin_sharetarget::init())
        .plugin(tauri_plugin_barcode_scanner::init())
        .plugin(tauri_plugin_android_fs::init());
    // автозапуск при старте системы (только десктоп), аргумент --minimized
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        Some(vec!["--minimized"]),
    ));
    builder
        .manage(UiState {
            pending: tokio::sync::Mutex::new(None),
            cancels: tokio::sync::Mutex::new(HashMap::new()),
            active: std::sync::Mutex::new(HashMap::new()),
            active_progress: std::sync::Mutex::new(HashMap::new()),
            pair_secret: std::sync::Mutex::new(None),
            listener_stop: tokio::sync::Mutex::new(None),
            next_id: AtomicU64::new(1),
        })
        .setup(|app| {
            // подчистка устаревшего share-кэша — в отдельном потоке,
            // чтобы не задерживать старт основного процесса
            if let Ok(cache) = app.path().app_cache_dir() {
                std::thread::spawn(move || {
                    let _ = std::fs::remove_dir_all(cache.join("shared"));
                });
            }
            // Android 13+: запрос разрешения на уведомления при первом запуске
            // (системный диалог показывается один раз; при отказе не навязываемся)
            #[cfg(target_os = "android")]
            {
                use tauri_plugin_notification::{NotificationExt, PermissionState};
                let ahn = app.handle().clone();
                tauri::async_runtime::spawn_blocking(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    if let Ok(state) = ahn.notification().permission_state() {
                        if !matches!(state, PermissionState::Granted | PermissionState::Denied) {
                            let _ = ahn.notification().request_permission();
                        }
                    }
                });
            }
            // deep-link: netdrop://ndc1… открывает облачный тикет в приложении
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                #[cfg(any(target_os = "linux", target_os = "windows"))]
                let _ = app.deep_link().register_all();
                let ah = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        emit(&ah, json!({ "kind": "deep-link", "url": url.to_string() }));
                    }
                });
                // холодный старт по ссылке: ждём загрузку фронтенда, потом отдаём URL
                let ah2 = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1800)).await;
                    use tauri_plugin_deep_link::DeepLinkExt;
                    if let Ok(Some(urls)) = ah2.deep_link().get_current() {
                        for url in urls {
                            emit(&ah2, json!({ "kind": "deep-link", "url": url.to_string() }));
                        }
                    }
                });
            }
            // почта доверенных: фоновая проверка ящиков (Yandex/Google — анонимно)
            let ahm = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mins = mailbox::poll_minutes();
                tokio::time::sleep(std::time::Duration::from_secs(90)).await;
                loop {
                    let n = mailbox::poll_inboxes(&ahm, None).await;
                    if n > 0 {
                        emit(&ahm, json!({ "kind": "mbox-found", "found": n }));
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(60 * mins)).await;
                }
            });
            // повтор подтверждений «получено» — раз в 10 минут до успеха
            let ahr = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                    mailbox::flush_receipts(&ahr).await;
                }
            });
            // автозапуск слушателя доверенных устройств (если включён)
            let ah = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if load_device_cfg(&ah)["listen"].as_bool().unwrap_or(false) {
                    let st = ah.state::<UiState>();
                    if let Err(e) = ensure_listener(&ah, &st).await {
                        eprintln!("netdrop: device listener failed: {e}");
                    }
                }
            });
            #[cfg(desktop)]
            {
                setup_tray(app)?;
                // запущены автостартом → сразу прячемся в трей
                if std::env::args().any(|a| a == "--minimized") {
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.hide();
                    }
                }
            }
            Ok(())
        })
        .on_window_event(|_window, _event| {
            // закрытие окна → сворачиваем в трей (десктоп)
            #[cfg(desktop)]
            if let tauri::WindowEvent::CloseRequested { api, .. } = _event {
                api.prevent_close();
                let _ = _window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            send_files,
            receive_probe,
            receive_accept,
            receive_reject,
            default_relays,
            default_out_dir,
            share_received_file,
            history_list,
            history_remove,
            history_clear,
            cancel_transfer,
            show_in_folder,
            set_autostart,
            get_autostart,
            stash_shared_file,
            stash_content_uri,
            open_received_file,
            send_text_to_device,
            device_info,
            list_devices,
            remove_device,
            set_device_cfg,
            pair_qr,
            add_device_by_code,
            send_to_device,
            ping_devices,
            cloud::cloud_yandex_oauth,
            cloud::cloud_yandex_auth_start,
            cloud::cloud_yandex_exchange,
            cloud::cloud_yandex_refresh,
            cloud::cloud_google_oauth,
            cloud::cloud_google_refresh,
            cloud::cloud_account_info,
            cloud::cloud_send,
            cloud::cloud_recv,
            cloud::cloud_ticket_info,
            cloud::cloud_sent_list,
            cloud::cloud_sent_delete,
            cloud::cloud_sent_forget,
            mailbox::mailbox_setup,
            mailbox::mailbox_send,
            mailbox::mailbox_poll,
            mailbox::mailbox_status,
            mailbox::mailbox_finalize,
            mailbox::mailbox_text_file,
            mailbox::mailbox_prepare
        ])
        .run(tauri::generate_context!())
        .expect("netdrop-ui failed to start");
}


// ---------- Юнит-тесты команд-хелперов ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_bad_chars() {
        assert_eq!(sanitize_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize_name("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitize_name(""), "netdrop.bin");
    }

    #[test]
    fn unique_path_increments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = unique_path(dir.path(), "report.txt");
        assert_eq!(first, dir.path().join("report.txt"));
        std::fs::write(&first, b"x").expect("write");
        let second = unique_path(dir.path(), "report.txt");
        assert_eq!(second, dir.path().join("report (1).txt"));
    }

    #[test]
    fn bundle_name_formats() {
        assert_eq!(bundle_name(&[PathBuf::from("/a/b/file.iso")]), "file.iso");
        assert_eq!(
            bundle_name(&[PathBuf::from("/a/one.txt"), PathBuf::from("/a/two.txt"), PathBuf::from("/a/three.txt")]),
            "one.txt (+2 more)"
        );
    }

    #[test]
    fn ticket_qr_svg_renders() {
        let svg = ticket_qr_svg("nd1TestTicketData123").expect("svg");
        assert!(svg.contains("<svg"));
    }

    #[test]
    fn paths_stats_counts_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.bin"), vec![0u8; 100]).expect("write");
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).expect("mkdir");
        std::fs::write(sub.join("b.bin"), vec![0u8; 50]).expect("write");
        let (total, count) = paths_stats(&[dir.path().to_path_buf()]).expect("stats");
        assert_eq!(total, 150);
        assert_eq!(count, 2);
    }

    #[test]
    fn default_relays_returns_n1() {
        let relays = default_relays();
        assert_eq!(relays.len(), 2);
        assert!(relays[0].contains("sth.cx"));
    }
}
