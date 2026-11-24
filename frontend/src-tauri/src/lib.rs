/*
 * SmartNet // Tauri v2 backend library (smartnet_lib)
 *
 *   Crate root + orchestration. Domain logic is split into modules:
 *     - crypto.rs   : key/address derivation + AES vault sealing
 *     - protocol.rs : custom `sth://` URI scheme handler
 *     - p2p.rs      : mDNS + HTTP trackers + iroh-blobs + update preferences
 *     - sth_node.rs : SmartHoldem node REST client + tx signing
 *   This file keeps the vault/session IPC, dApp publish pipeline, content
 *   verification, the dApp provider bridge and the embedded webview host.
 *
 *   - SmartHoldem (STH-style) key + address derivation (secp256k1, network 63).
 *   - crypto-js compatible AES vault (OpenSSL Salted__/EVP_BytesToKey-MD5).
 *   - Sled DB: durable vault + dApp registry (key "app:<APP_ID>").
 *   - dApp provisioning pipeline: native folder dialog -> recursive deep copy
 *       into data/dapps/<APP_ID>/ -> Blake3 root hash -> Sled index ->
 *       content.json signed with the app identity key -> gossip announce.
 *   - Custom `sth://` URI scheme: zero-local-server rendering. Resolves
 *       sth://<APP_ID>/<path> straight from the isolated app folder into the
 *       child Multi-Webview as raw bytes with the correct MIME type.
 *
 *   APP IDENTITY (immutable URL): m/44'/255'/0'/1/index (STH-namespaced),
 *   formatted as a SmartHoldem `S...` address == sth://[address].
 *
 *   Author: TechnoL0g
 *   Created: 2026-06-18
 */

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::Manager;
use tauri::Listener;

mod crashdiag;
mod sth_node;
mod watchdog;
mod crypto;
mod protocol;
mod p2p;
#[cfg(feature = "p2p")]
mod peerbook;
#[cfg(feature = "p2p")]
mod sth_hints;
#[cfg(not(feature = "p2p"))]
mod peerbook {
    /// Без P2P книги пиров нет — пустой список для единого API фронта.
    #[tauri::command]
    pub(crate) fn peerbook_list(_role: Option<String>) -> Vec<serde_json::Value> {
        Vec::new()
    }
}
mod ai_client;
#[cfg(feature = "p2p")]
mod dht_dapp;
#[cfg(feature = "p2p")]
mod swarm;
mod syslog;
mod netstat;
mod wallet;
mod dapp;
mod webview;
mod websites;
mod webproxy;
mod mycel;
mod mycel_vpn;
mod netstats;
mod autostart;
mod apps;
mod private_dapp;
mod profile;
mod naming;
mod dns_manager;
mod subs;
mod txindex;
mod clearnet;
mod chat;
pub(crate) mod android;
mod sync_status;
mod oracle;
mod core;
mod seeding;
mod geo;
mod files;
mod torrent;
mod updater;
mod stealth;
mod social;
mod reputation;
mod translator;
mod mail_launcher;
mod messenger_launcher;
mod translator_config;
mod dev_router;
mod devhub;
mod devhub_git;
mod devhub_ipc;
mod devhub_pr;
mod netwatch;
mod official;
mod cloudvault;
mod cloudseeder;
mod profile_seeder;
mod ai_telemetry;
mod wallet_stream;

use crate::crypto::*;
use crate::protocol::resolve_request;

pub(crate) const VAULT_KEY: &str = "sth_vault";
pub(crate) const APP_PREFIX: &str = "app:";

/// Каталог данных приложения БЕЗ AppHandle — нужен до старта Tauri, потому
/// что Lazy-DB инициализируется первым же обращением. Повторяет логику
/// tauri `app_data_dir()`: Windows %APPDATA%, macOS ~/Library/Application
/// Support, Linux $XDG_DATA_HOME | ~/.local/share, + идентификатор из
/// tauri.conf.json.
pub(crate) fn os_app_data_dir() -> Option<PathBuf> {
    const IDENT: &str = "io.smartholdem.smartnet";
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library").join("Application Support"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    base.map(|b| b.join(IDENT))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir_recursive(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

pub(crate) static DB: Lazy<sled::Db> = Lazy::new(|| {
    // v1.135.35: БД переехала из %TEMP%/smartnet_db в постоянный каталог
    // данных <app_data>/data/smartnet_db (рядом с dapps/manifests/keys) —
    // очистка TEMP (Storage Sense/CCleaner) убивала клиента и стирала
    // конфиг. Существующая база из TEMP переносится ОДИН раз: rename (тот
    // же диск — мгновенно), при неудаче — рекурсивная копия. Если ни
    // APPDATA, ни HOME недоступны — остаёмся в TEMP, как раньше.
    let legacy = std::env::temp_dir().join("smartnet_db");
    let dir = match os_app_data_dir() {
        Some(base) => {
            let target = base.join("data").join("smartnet_db");
            if !target.exists() && legacy.exists() {
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::rename(&legacy, &target).is_err() {
                    if let Err(e) = copy_dir_recursive(&legacy, &target) {
                        crash_log(&format!("sled DB migration TEMP→data failed: {e}"));
                        let _ = std::fs::remove_dir_all(&target);
                        return sled::open(&legacy).expect("failed to open Sled DB");
                    }
                }
            }
            target
        }
        None => legacy,
    };
    // Лимит page-cache sled: по умолчанию 1 ГБ, и sled его заметно превышает при
    // частых записях (известная проблема 0.34) — один из кандидатов на RSS 11 ГБ.
    sled::Config::new()
        .path(dir)
        .cache_capacity(128 * 1024 * 1024)
        .open()
        .expect("failed to open Sled DB")
});

/// Аварийный журнал на диске: <app_data>/smartnet-crash.log. GUI-процесс
/// Windows не имеет видимого stderr, поэтому после «тихого» закрытия клиента
/// иначе не остаётся никаких улик. v1.135.93: переехал из %TEMP% — очистка
/// TEMP стирала лог вместе с уликами (и валила WebView2, см. реанимацию окна).
/// Аллокатор-обёртка: фиксирует ALLOCATION FAILURE (OOM) в crash-лог до abort'а.
#[global_allocator]
static GLOBAL_ALLOC: crashdiag::DiagAlloc = crashdiag::DiagAlloc;

pub(crate) fn crash_log(msg: &str) {
    use std::io::Write;
    let path = os_app_data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("smartnet-crash.log");
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{} v{}] {}", ts, env!("CARGO_PKG_VERSION"), msg);
    }
}

/// true после CloseRequested главного окна — отличает штатное закрытие
/// пользователем от аварийного Destroyed (крэш WebView-рендерера).
static MAIN_CLOSE_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// true — пользователь выбрал «Выход» в трее: крестик/CloseRequested теперь
/// действительно закрывает приложение, а не сворачивает в трей.
static TRAY_QUIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Иконка в трее: крестик главного окна сворачивает в трей (P2P-сидинг, чат и
/// раздачи продолжают работать), ЛКМ по иконке / «Показать» — возвращает окно,
/// «Выход» — штатно завершает процесс.
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    // серая (неактивная) строка состояния: цепочки Mycelium и P2P онлайн, обновляется раз в 5 с
    let status = MenuItem::with_id(app, "tray-status", tray_status_text(), false, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let show = MenuItem::with_id(app, "tray-show", "Show NETFORY", true, None::<&str>)?;
    // Фаза 3: быстрый тумблер системного VPN Mycelium (текст меняется в tray_set_vpn)
    let vpn = MenuItem::with_id(app, "tray-vpn", TRAY_VPN_OFF_TEXT, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray-quit", "Exit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&status, &sep, &show, &vpn, &quit])?;
    *TRAY_VPN_ITEM.lock().unwrap() = Some(vpn);
    spawn_tray_status(status);

    // v1.135.184: get_window, НЕ get_webview_window — после add_child (dApp/web-
    // вкладка) главное окно содержит >1 webview и get_webview_window("main")
    // возвращает None → «Показать NETFORY» из трея молча не срабатывал.
    fn show_main(app: &tauri::AppHandle) {
        if let Some(w) = app.get_window("main") {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip(format!("NETFORY v{}", env!("CARGO_PKG_VERSION")))
        .on_menu_event(|app, event| match event.id.as_ref() {
            "tray-show" => show_main(app),
            "tray-vpn" => {
                tauri::async_runtime::spawn(async {
                    let on = mycel_vpn::info().enabled;
                    let r = mycel_vpn::mycelium_vpn_set(!on).await;
                    if let Err(e) = r {
                        syslog::log("mycelium", format!("vpn (tray): {e}"));
                        if let Some(app) = sth_node::app_handle() {
                            syslog::emit_queued(&app, "mycelium-vpn-error", serde_json::json!({ "error": e }));
                        }
                    }
                });
            }
            "tray-quit" => {
                TRAY_QUIT.store(true, std::sync::atomic::Ordering::SeqCst);
                MAIN_CLOSE_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
                app.exit(0);
            }
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
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Текст строки состояния трея: «Mycelium: 3 circuits · P2P: online (9)».
fn tray_status_text() -> String {
    #[cfg(feature = "p2p")]
    {
        let myc = match mycel::instance() {
            Some(m) if m.is_enabled() => {
                let st = m.status();
                match st.circuits_ready {
                    0 if st.circuits_building => "building…".to_string(),
                    0 => "no circuits".to_string(),
                    1 => "1 circuit".to_string(),
                    n => format!("{n} circuits"),
                }
            }
            _ if mycel::enabled() => "starting…".to_string(),
            _ => "off".to_string(),
        };
        let p2p = match p2p::endpoint_now() {
            None => "off".to_string(),
            Some(ep) => {
                let m = &ep.metrics().socket;
                let open = m.num_conns_opened.get().saturating_sub(m.num_conns_closed.get());
                if open > 0 {
                    format!("online ({open})")
                } else if ep.addr().relay_urls().next().is_some() {
                    "online".to_string()
                } else {
                    "connecting…".to_string()
                }
            }
        };
        format!("Mycelium: {myc} · P2P: {p2p}")
    }
    #[cfg(not(feature = "p2p"))]
    {
        "Mycelium: off · P2P: off".to_string()
    }
}

fn spawn_tray_status(item: tauri::menu::MenuItem<tauri::Wry>) {
    tauri::async_runtime::spawn(async move {
        let mut last = String::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let text = tray_status_text();
            if text != last {
                let _ = item.set_text(&text);
                last = text;
            }
        }
    });
}

/// Последнее показанное в трее число непрочитанных (u32::MAX — ещё не ставили).
static TRAY_UNREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);

/// Бейдж непрочитанных на иконке трея: красный круг с числом в правом верхнем
/// углу (0 — исходная иконка). Tooltip дублирует число. Вызывается из чата при
/// каждом изменении счётчиков комнат; одинаковое значение не перерисовывается.
static TRAY_RING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// v1.135.231: входящий звонок — мигание иконки в трее (show/hide 0.6с) + мигание окна в панели задач.
pub(crate) fn tray_ring(on: bool) {
    use std::sync::atomic::Ordering;
    if !on {
        TRAY_RING.store(false, Ordering::SeqCst);
        return;
    }
    if TRAY_RING.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(app) = sth_node::app_handle() else { return };
    for w in app.webview_windows().values() {
        let _ = w.request_user_attention(Some(tauri::UserAttentionType::Critical));
    }
    std::thread::spawn(move || {
        let mut visible = true;
        while TRAY_RING.load(Ordering::SeqCst) {
            if let Some(tray) = app.tray_by_id("main-tray") {
                visible = !visible;
                let _ = tray.set_visible(visible);
            }
            std::thread::sleep(std::time::Duration::from_millis(600));
        }
        if let Some(tray) = app.tray_by_id("main-tray") {
            let _ = tray.set_visible(true);
        }
    });
}

pub(crate) fn tray_set_unread(count: u64) {
    use std::sync::atomic::Ordering;
    let n = count.min(u32::MAX as u64 - 1) as u32;
    if TRAY_UNREAD.swap(n, Ordering::SeqCst) == n {
        return;
    }
    tray_render();
}

/// Системный VPN Mycelium поднят: зелёная метка на иконке трея + пункт меню «Выключить VPN».
static TRAY_VPN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static TRAY_VPN_ITEM: Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>> = Mutex::new(None);
const TRAY_VPN_OFF_TEXT: &str = "Mycelium On";
const TRAY_VPN_ON_TEXT: &str = "Mycelium Off";

pub(crate) fn tray_set_vpn(on: bool) {
    use std::sync::atomic::Ordering;
    if TRAY_VPN.swap(on, Ordering::SeqCst) == on {
        return;
    }
    if let Some(item) = TRAY_VPN_ITEM.lock().unwrap().as_ref() {
        let _ = item.set_text(if on { TRAY_VPN_ON_TEXT } else { TRAY_VPN_OFF_TEXT });
    }
    tray_render();
}

/// Перерисовать иконку трея: базовая + бейдж непрочитанных + зелёная метка VPN; tooltip дублирует.
fn tray_render() {
    use std::sync::atomic::Ordering;
    let n = match TRAY_UNREAD.load(Ordering::SeqCst) {
        u32::MAX => 0,
        v => v,
    };
    let vpn = TRAY_VPN.load(Ordering::SeqCst);
    let Some(app) = sth_node::app_handle() else { return };
    let Some(tray) = app.tray_by_id("main-tray") else { return };
    let Some(base) = app.default_window_icon() else { return };
    let mut tooltip = format!("NETFORY v{}", env!("CARGO_PKG_VERSION"));
    if n > 0 {
        tooltip.push_str(&format!(" — unread: {n}"));
    }
    if vpn {
        tooltip.push_str(" — Mycelium dVPN on");
    }
    let _ = tray.set_tooltip(Some(tooltip));
    let res = if n == 0 && !vpn {
        tray.set_icon(Some(base.clone()))
    } else {
        tray.set_icon(Some(badge_icon(base, n, vpn)))
    };
    if let Err(e) = res {
        syslog::log("sys", format!("tray badge: {e}"));
    }
}

/// Иконка + бейдж: круг в правом верхнем углу, цифры 3×5 пикс. шрифтом (масштаб по размеру иконки);
/// `vpn` — зелёная метка в правом нижнем углу (системный VPN Mycelium поднят).
fn badge_icon(base: &tauri::image::Image<'_>, count: u32, vpn: bool) -> tauri::image::Image<'static> {
    const FONT: [[u8; 5]; 10] = [
        [0b111, 0b101, 0b101, 0b101, 0b111],
        [0b010, 0b110, 0b010, 0b010, 0b111],
        [0b111, 0b001, 0b111, 0b100, 0b111],
        [0b111, 0b001, 0b111, 0b001, 0b111],
        [0b101, 0b101, 0b111, 0b001, 0b001],
        [0b111, 0b100, 0b111, 0b001, 0b111],
        [0b111, 0b100, 0b111, 0b101, 0b111],
        [0b111, 0b001, 0b001, 0b001, 0b001],
        [0b111, 0b101, 0b111, 0b101, 0b111],
        [0b111, 0b101, 0b111, 0b001, 0b111],
    ];
    let (w, h) = (base.width(), base.height());
    let mut px = base.rgba().to_vec();
    px.resize((w * h * 4) as usize, 0);
    if vpn {
        let size = w.min(h) as f32;
        let r = (size * 0.26).max(4.0);
        let (cx, cy) = (w as f32 - r, h as f32 - r);
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let d = (dx * dx + dy * dy).sqrt();
                if d > r + 0.5 {
                    continue;
                }
                let i = ((y * w + x) * 4) as usize;
                let a = (r + 0.5 - d).clamp(0.0, 1.0);
                let (cr, cg, cb) = if d > r - 1.2 { (255u8, 255u8, 255u8) } else { (46u8, 230u8, 166u8) };
                px[i] = (cr as f32 * a + px[i] as f32 * (1.0 - a)) as u8;
                px[i + 1] = (cg as f32 * a + px[i + 1] as f32 * (1.0 - a)) as u8;
                px[i + 2] = (cb as f32 * a + px[i + 2] as f32 * (1.0 - a)) as u8;
                px[i + 3] = (255.0 * a + px[i + 3] as f32 * (1.0 - a)) as u8;
            }
        }
        if count == 0 {
            return tauri::image::Image::new_owned(px, w, h);
        }
    }
    let text = if count > 99 { "99".to_string() } else { count.to_string() };
    let digits: Vec<usize> = text.bytes().map(|b| (b - b'0') as usize).collect();
    let size = w.min(h) as f32;
    let r = (size * 0.36).max(5.0);
    let (cx, cy) = (w as f32 - r, r);
    let scale = ((r * 2.0 * 0.5) / 5.0).floor().max(1.0) as u32; // высота цифр ≈ 50% диаметра
    let tw = digits.len() as u32 * 3 * scale + (digits.len() as u32 - 1) * scale;
    let th = 5 * scale;
    let tx0 = (cx - tw as f32 / 2.0).round() as i64;
    let ty0 = (cy - th as f32 / 2.0).round() as i64;
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            if d > r + 0.5 {
                continue;
            }
            let i = ((y * w + x) * 4) as usize;
            let a = (r + 0.5 - d).clamp(0.0, 1.0); // сглаженный край
            // белая обводка 1px, затем красная заливка
            let (cr, cg, cb) = if d > r - 1.2 { (255u8, 255u8, 255u8) } else { (229u8, 57u8, 53u8) };
            px[i] = (cr as f32 * a + px[i] as f32 * (1.0 - a)) as u8;
            px[i + 1] = (cg as f32 * a + px[i + 1] as f32 * (1.0 - a)) as u8;
            px[i + 2] = (cb as f32 * a + px[i + 2] as f32 * (1.0 - a)) as u8;
            px[i + 3] = (255.0 * a + px[i + 3] as f32 * (1.0 - a)) as u8;
        }
    }
    for (k, &d) in digits.iter().enumerate() {
        let ox = tx0 + (k as u32 * 4 * scale) as i64;
        for (row, bits) in FONT[d].iter().enumerate() {
            for col in 0..3u32 {
                if bits & (0b100 >> col) == 0 {
                    continue;
                }
                for sy in 0..scale {
                    for sx in 0..scale {
                        let x = ox + (col * scale + sx) as i64;
                        let y = ty0 + (row as u32 * scale + sy) as i64;
                        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
                            continue;
                        }
                        let i = ((y as u32 * w + x as u32) * 4) as usize;
                        px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                    }
                }
            }
        }
    }
    tauri::image::Image::new_owned(px, w, h)
}

/// Открыть аварийный журнал (<app_data>/smartnet-crash.log) системным
/// приложением. Err "crash-log-empty" — если аварий ещё не зафиксировано.
#[tauri::command]
async fn open_crash_log(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_shell::ShellExt;
    let path = os_app_data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("smartnet-crash.log");
    if !path.is_file() {
        return Err("crash-log-empty".into());
    }
    #[allow(deprecated)]
    app.shell()
        .open(path.to_string_lossy().to_string(), None)
        .map_err(|e| e.to_string())
}

/// Маркер живой сессии: создаётся на старте, снимается при чистом выходе.
/// Если при старте маркер уже есть — прошлая сессия умерла БЕЗ чистого выхода
/// и без записи паники (нативный крэш/OOM/kill/питание) — фиксируем улику.
/// Лежит в каталоге данных (не в TEMP): очистка TEMP не даёт ложную «грязь».
pub(crate) fn run_marker_path() -> std::path::PathBuf {
    os_app_data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("smartnet-run.marker")
}

/// true когда завершение приложения инициировано штатно (app.exit / выход):
/// Destroyed-обработчик не должен реанимировать окно во время выхода.
static APP_EXITING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Одна попытка реанимации главного окна за сессию (защита от цикла).
static MAIN_RECOVERY_TRIED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Windows SEH: нативные исключения (access violation, stack overflow,
/// fast-fail/abort) убивают процесс МИМО rust-паник и panic-hook — «тихое»
/// закрытие без единой строки в crash-логе. Фиксируем код исключения на диск
/// до смерти процесса.
#[cfg(windows)]
fn install_native_crash_filter() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SetUnhandledExceptionFilter, EXCEPTION_POINTERS,
    };
    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        let (code, addr) = unsafe {
            if info.is_null() || (*info).ExceptionRecord.is_null() {
                (0u32, 0usize)
            } else {
                let rec = (*info).ExceptionRecord;
                ((*rec).ExceptionCode as u32, (*rec).ExceptionAddress as usize)
            }
        };
        crash_log(&format!(
            "NATIVE EXCEPTION 0x{code:08X} at 0x{addr:X} — process dies outside Rust panic \
             (0xC0000005=access violation, 0xC00000FD=stack overflow, 0xC0000409=fast-fail/abort)"
        ));
        0 // EXCEPTION_CONTINUE_SEARCH — стандартная обработка (смерть) продолжается
    }
    unsafe {
        SetUnhandledExceptionFilter(Some(filter));
    }
}

// decrypted secret lives ONLY here, in the Rust process memory.
pub(crate) static SESSION_SEED: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

#[derive(Debug, Serialize, Deserialize, Clone)]
struct VaultRecord {
    cipher: String,
    pub(crate) address: String,
    #[serde(rename = "publicKey")]
    public_key: String,
    pub(crate) accounts: Vec<u32>,
    #[serde(rename = "createdAt")]
    created_at: String,
}

#[derive(Debug, Serialize)]
struct SessionStatus {
    unlocked: bool,
    address: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct FileEntry {
    pub(crate) path: String,
    pub(crate) sha256: String,
    pub(crate) size: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct Manifest {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    pub(crate) owner: String,
    pub(crate) files: Vec<FileEntry>,
    pub(crate) merkle: String,
    pub(crate) created: String,
    pub(crate) signature: String,
    #[serde(rename = "publicKey")]
    pub(crate) public_key: String,
    pub(crate) path: Option<String>,
    pub(crate) index: Option<u32>,
    pub(crate) version: Option<u32>,
    pub(crate) updated: Option<String>,
    #[serde(default)]
    pub(crate) blob: Option<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) readme: Option<String>,
    /// Store state: "draft" (local only) or "published". Absent ⇒ legacy published.
    #[serde(default)]
    pub(crate) status: Option<String>,
    /// True once an on-chain publish payment to the escrow node was confirmed.
    #[serde(default)]
    pub(crate) paid: Option<bool>,
    /// True when the dev opted to publish for free (NOT announced by trackers).
    #[serde(default)]
    pub(crate) free: Option<bool>,
    /// txid of the confirmed publish payment (when paid).
    #[serde(default, rename = "escrowTxid")]
    pub(crate) escrow_txid: Option<String>,
    /// Developer source folder (drafts) — used by live Preview re-ingest.
    #[serde(default, rename = "srcDir")]
    pub(crate) src_dir: Option<String>,
    /// True для приватного (E2EE) dApp — контент зашифрован для списка адресов.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) private: Option<bool>,
}

// ============================================================
//  Crypto helpers — see `crypto.rs` (key derivation, address, AES vault).
// ============================================================

// ============================================================
//  Vault IPC
// ============================================================

#[tauri::command]
fn vault_exists() -> bool {
    DB.contains_key(VAULT_KEY).unwrap_or(false)
}

/// Toggle the native borderless P2P chat window. Spawns it (1160×680, min 1160×680)
/// on first use; otherwise show/hide. An optional `target` (u://name or address)
/// is emitted to the window so it auto-opens that conversation.
#[tauri::command]
async fn toggle_chat_window(app: tauri::AppHandle, target: Option<String>) -> Result<(), String> {
    use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
    if let Some(win) = app.get_webview_window("p2p_chat_main") {
        if win.is_visible().unwrap_or(false) && target.is_none() {
            let _ = win.hide();
        } else {
            let _ = win.show();
            let _ = win.set_focus();
            if let Some(t) = target {
                let _ = win.emit("chat-open-target", t);
            }
        }
        return Ok(());
    }
    let win = WebviewWindowBuilder::new(&app, "p2p_chat_main", WebviewUrl::App("index.html?w=chat".into()))
        .title("NETFORY // P2P Messenger")
        .inner_size(1160.0, 680.0)
        .min_inner_size(1160.0, 680.0)
        .resizable(true)
        .decorations(false)
        .build()
        .map_err(|e| e.to_string())?;
    // Windows: авто-ALLOW микрофона для голосовых сообщений (см. webview::allow_mic).
    crate::webview::allow_mic(&win);
    if let Some(t) = target {
        let w = win.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(1300));
            let _ = w.emit("chat-open-target", t);
        });
    }
    Ok(())
}

/// Полноэкранное окно просмотра изображения из мессенджера (Telegram-style):
/// клик по картинке в чате открывает её на весь экран, клик/Escape — закрывает.
/// Параметры уходят и в query-строку, и событием `imgview-open` (страховка от
/// потери query string на некоторых платформах). ВСЕГДА async — оконные операции
/// в sync-командах дедлочат main thread (см. v1.135.48).
#[tauri::command]
async fn open_image_window(
    app: tauri::AppHandle,
    room_id: String,
    hash: String,
    node: String,
    size: Option<u64>,
) -> Result<(), String> {
    use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
    let sz = size.unwrap_or(0);
    let payload = serde_json::json!({ "room": room_id, "hash": hash, "node": node, "size": sz });
    if let Some(win) = app.get_webview_window("p2p_img_view") {
        let _ = win.show();
        let _ = win.set_focus();
        let _ = win.emit("imgview-open", payload);
        return Ok(());
    }
    let url = format!("index.html?w=imgview&room={}&hash={}&node={}&s={}", room_id, hash, node, sz);
    let win = WebviewWindowBuilder::new(&app, "p2p_img_view", WebviewUrl::App(url.into()))
        .title("NETFORY // Image")
        .fullscreen(true)
        .decorations(false)
        // прозрачное окно: за картинкой — полупрозрачное затенение рабочего
        // стола (виньетка в CSS), а не глухой чёрный фон
        .transparent(true)
        .build()
        .map_err(|e| e.to_string())?;
    let _ = win.set_focus();
    let w = win.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(900));
        let _ = w.emit("imgview-open", payload);
    });
    Ok(())
}

/// Закрыть окно просмотра изображения (клик по картинке / Escape).
#[tauri::command]
async fn close_image_window(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    if let Some(win) = app.get_webview_window("p2p_img_view") {
        let _ = win.close();
    }
    Ok(())
}


/// Toggle the native System Console window. Spawns it (1180×460, min 780×320)
/// on first use with `?w=syscon` — `App.vue` detects that flag and renders
/// only the console (no sidebar / no wallet drawer). Subsequent calls
/// show/hide/focus the existing window instead of stacking new ones.
#[tauri::command]
async fn toggle_console_window(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
    if let Some(win) = app.get_webview_window("syscon") {
        if win.is_visible().unwrap_or(false) {
            let _ = win.set_focus();
        } else {
            let _ = win.show();
            let _ = win.set_focus();
        }
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, "syscon", WebviewUrl::App("index.html?w=syscon".into()))
        .title("NETFORY // System Console")
        .inner_size(1180.0, 460.0)
        .min_inner_size(780.0, 320.0)
        .resizable(true)
        .decorations(true)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}


#[tauri::command]
fn load_vault() -> Option<VaultRecord> {
    match DB.get(VAULT_KEY) {
        Ok(Some(bytes)) => serde_json::from_slice::<VaultRecord>(&bytes).ok(),
        _ => None,
    }
}

/// Non-command alias so sibling modules (p2p) can read the vault without
/// tripping the `#[tauri::command]` macro's re-export rules on pub functions.
pub(crate) fn load_vault_internal() -> Option<VaultRecord> {
    load_vault()
}

pub(crate) fn save_vault_internal(record: VaultRecord) -> Result<(), String> {
    save_vault(record)
}

#[tauri::command]
fn save_vault(record: VaultRecord) -> Result<(), String> {
    let bytes = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
    DB.insert(VAULT_KEY, bytes).map_err(|e| e.to_string())?;
    DB.flush().map_err(|e| e.to_string())?;
    Ok(())
}

// ============================================================
//  Wallet thin-client IPC (account/balance/history/swap/sign) — see `wallet.rs`
// ============================================================

// ============================================================
//  Native crypto IPC
// ============================================================

#[tauri::command]
fn create_vault(secret: String, pin: String) -> Result<AccountInfo, String> {
    let primary = derive_account_internal(&secret, 0)?;
    let cipher = aes_encrypt(secret.trim(), &pin_passphrase(&pin));
    let record = VaultRecord {
        cipher,
        address: primary.address.clone(),
        public_key: primary.public_key.clone(),
        accounts: vec![0],
        created_at: now_iso(),
    };
    save_vault(record)?;
    *SESSION_SEED.lock().unwrap() = Some(secret.trim().to_string());
    Ok(primary)
}

#[tauri::command]
async fn unlock_vault(pin: String) -> Result<AccountInfo, String> {
    // ЖЁСТКОЕ ПРАВИЛО: никакой крипто-работы и файлового I/O на main-потоке —
    // sync-команды Tauri исполняются на главном потоке и подвешивают UI.
    tauri::async_runtime::spawn_blocking(move || {
        let vault = load_vault().ok_or_else(|| "no vault".to_string())?;
        let secret = aes_decrypt(&vault.cipher, &pin_passphrase(&pin)).ok_or("wrong pin")?;
        let acc = derive_account_internal(&secret, 0)?;
        if acc.address != vault.address {
            return Err("wrong pin".to_string());
        }
        *SESSION_SEED.lock().unwrap() = Some(secret);
        Ok(acc)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn derive_account(index: u32) -> Result<AccountInfo, String> {
    let guard = SESSION_SEED.lock().unwrap();
    let secret = guard.as_ref().ok_or("locked")?;
    let acc = derive_account_internal(secret, index)?;
    if let Some(mut v) = load_vault() {
        if !v.accounts.contains(&index) {
            v.accounts.push(index);
            let _ = save_vault(v);
        }
    }
    Ok(acc)
}

#[tauri::command]
fn reveal_seed() -> Option<String> {
    SESSION_SEED.lock().unwrap().clone()
}

#[tauri::command]
fn session_clear() {
    messenger_launcher::shutdown();
    *SESSION_SEED.lock().unwrap() = None;
    // приватные dApps: plaintext живёт только при разблокированной сессии
    private_dapp::unmount_all();
}

#[tauri::command]
fn session_status() -> SessionStatus {
    let unlocked = SESSION_SEED.lock().unwrap().is_some();
    let address = load_vault().map(|v| v.address).unwrap_or_default();
    SessionStatus { unlocked, address }
}

// ============================================================
//  data/ directory + dApp publish pipeline
// ============================================================

fn data_root(app: &tauri::AppHandle) -> PathBuf {
    // User-configurable override (lets the user move data/ to another disk).
    if let Ok(Some(v)) = DB.get(b"cfg:data_dir") {
        if let Ok(s) = String::from_utf8(v.to_vec()) {
            if !s.trim().is_empty() {
                return PathBuf::from(s);
            }
        }
    }
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("data")
}

pub(crate) fn ensure_data_dir(app: &tauri::AppHandle) -> PathBuf {
    let base = data_root(app);
    for sub in ["", "dapps", "manifests", "keys"] {
        let _ = std::fs::create_dir_all(base.join(sub));
    }
    base
}

// `get_data_dir`, `set_data_dir` + the data/dapps storage-quota commands + the
// dApp publish / verify pipeline now live in `apps.rs`. Shared low-level helpers
// (ensure_data_dir, dir_size, storage-limit) stay below.

// ============================================================
//  Storage quota (data/dapps capacity limit)
// ============================================================

/// Default & MINIMUM app-storage limit for data/dapps (8 GiB). The user may
/// raise it in Settings, but never set it below this floor.
pub(crate) const DEFAULT_STORAGE_LIMIT: u64 = 8 * 1024 * 1024 * 1024;

/// Recursively sum the byte size of every file under `path`.
/// Sanitize an arbitrary string into a filesystem-safe slug (shared helper).
pub(crate) fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}


pub(crate) fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(path) {
        for e in rd.flatten() {
            match e.metadata() {
                Ok(md) if md.is_dir() => total += dir_size(&e.path()),
                Ok(md) => total += md.len(),
                Err(_) => {}
            }
        }
    }
    total
}

pub(crate) fn get_storage_limit_internal() -> u64 {
    if let Ok(Some(v)) = DB.get(b"cfg:storage_limit") {
        if v.len() == 8 {
            let mut b = [0u8; 8];
            b.copy_from_slice(&v);
            return u64::from_le_bytes(b).max(DEFAULT_STORAGE_LIMIT);
        }
    }
    DEFAULT_STORAGE_LIMIT
}

// Storage-quota commands + folder picker moved to `apps.rs`.

// dApp publish pipeline (ingest/publish/update/delete/list), the README reader
// and content.json verification moved to `apps.rs`. The raw announce-index scan
// stays here as the shared reader used by `apps::announced_apps` and the p2p
// seeding loop.
pub(crate) fn announced_apps_internal() -> Vec<String> {
    let mut out = Vec::new();
    for item in DB.scan_prefix(b"announce:") {
        if let Ok((k, _)) = item {
            if let Ok(s) = std::str::from_utf8(&k) {
                out.push(s.trim_start_matches("announce:").to_string());
            }
        }
    }
    out
}

// ============================================================
//  content.json verification (content-addressed trust) — see `apps.rs`
//  (re-hash on disk + Blake3 merkle + id↔pubkey + ECDSA signature).
// ============================================================

// ============================================================
//  P2P discovery (mDNS + HTTP trackers + iroh-blobs) and the
//  auto-update / relay / tracker preferences — see `p2p.rs`.
// ============================================================

// ============================================================
//  Custom sth:// URI scheme — see `protocol.rs`
//  (mime_for, resolve_request, path-traversal guard).
// ============================================================

// ============================================================
//  Native dApp webviews — see `webview.rs` (multi-webview host)
//  Web3 provider bridge — see `dapp.rs` (window.smartholdem)
// ============================================================

// ============================================================
//  entrypoint
// ============================================================

/// Deep link холодного старта: argv первого запуска буферизуется здесь,
/// фронт забирает его один раз после монтирования (событие `deep-link`
/// эмитить бессмысленно — listener ещё не зарегистрирован).
static PENDING_DEEP_LINK: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Забрать (и очистить) deep link холодного старта.
#[tauri::command]
fn take_pending_deep_link() -> Option<String> {
    PENDING_DEEP_LINK.lock().unwrap().take()
}

/// Мини-загрузчик .env для Rust-процесса (KEY=VALUE построчно) из текущего
/// каталога и каталога исполняемого файла. Реальные переменные окружения
/// имеют приоритет; используется для NTFRY_* OAuth-оверрайдов без пересборки.
fn load_env_files() {
    let mut paths = vec![std::path::PathBuf::from(".env")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            paths.push(dir.join(".env"));
        }
    }
    for p in paths {
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let (k, v) = (k.trim(), v.trim());
                if !k.is_empty() && std::env::var(k).is_err() {
                    std::env::set_var(k, v);
                }
            }
        }
    }
    // Вшитый при сборке frontend/.env — самый низкий приоритет (дефолт
    // CHUNK_SIZE для ntfry без пересборки пользователем).
    if std::env::var("CHUNK_SIZE").is_err() {
        if let Some(v) = include_str!("../../.env")
            .lines()
            .find_map(|l| l.trim().strip_prefix("CHUNK_SIZE="))
        {
            let v = v.trim();
            if !v.is_empty() {
                std::env::set_var("CHUNK_SIZE", v);
            }
        }
    }
    // Вшитый .env также раздаёт настройки AI-модели переводов (см. модуль
    // `translator_config`). Пользователь может переопределить любой префикс
    // `TRANSLATOR_*` через системное окружение или локальный .env.
    for line in include_str!("../../.env").lines() {
        let line = line.trim();
        if !line.starts_with("TRANSLATOR_") {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() && std::env::var(k).is_err() {
                std::env::set_var(k, v);
            }
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Открыть ссылку в ГЛАВНОМ окне (адресная строка оболочки) из любого
/// другого окна (мессенджер/консоль): фокусируем main и шлём targeted-событие.
/// ВАЖНО: команда ОБЯЗАНА быть async — sync-команды Tauri исполняются на
/// главном потоке, а set_focus/show/unminimize изнутри IPC-обработчика на нём
/// же дедлочат event loop (окно «Не отвечает»). async уводит в пул потоков,
/// оконные вызовы корректно маршалятся в main loop без блокировки.
#[tauri::command]
async fn open_link_in_main(app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri::{Emitter, Manager};
    let u = url.trim();
    if !(u.starts_with("https://") || u.starts_with("http://")) {
        return Err("only http(s)".into());
    }
    if let Some(w) = app.get_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    app.emit_to("main", "netfory:open-link", u.to_string()).map_err(|e| e.to_string())
}

/// Ссылка (любая схема) из другого окна → адресная строка главного окна.
#[tauri::command]
async fn open_in_main(app: tauri::AppHandle, url: String) {
    use tauri::{Emitter, Manager};
    if let Some(w) = app.get_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    let _ = app.emit_to("main", "netfory:open-address", url);
}

pub fn run() {
    // v1.135.190: SOCKS5-роутер web-вкладок — порт и PAC для WebView2 нужны ДО первого webview.
    webproxy::start_early();
    load_env_files();
    // reqwest 0.13 (librqbit tracker-comms) собран с rustls-no-provider: без
    // установленного крипто-провайдера ЛЮБОЙ его Client паникует в tokio-worker
    // («No rustls crypto provider is configured» в crash-логе). Ставим ring
    // глобально один раз; повторная установка безопасна (Err игнорируем).
    let _ = rustls::crypto::ring::default_provider().install_default();
    // Глобальный panic-hook: паника ЛЮБОГО потока фиксируется на диске
    // (%TEMP%/smartnet-crash.log) с местом и сообщением — единственный способ
    // разобраться в «тихом» закрытии GUI-клиента без консоли.
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("<unnamed>").to_string();
        crash_log(&format!("PANIC in thread '{thread}': {info}"));
        default_panic(info);
    }));
    // Детектор «грязного» завершения прошлой сессии (см. run_marker_path).
    let marker = run_marker_path();
    if marker.exists() {
        let hb = crashdiag::last_heartbeat(&marker);
        crash_log(&format!(
            "previous session ended WITHOUT clean exit and WITHOUT recorded panic — \
             native crash / OOM / killed by OS / power loss / forced shutdown · last heartbeat (≤5 min before death): {}",
            if hb.is_empty() { "n/a (old marker format)" } else { hb.as_str() }
        ));
    }
    let _ = std::fs::write(&marker, env!("CARGO_PKG_VERSION"));
    // SEH-фильтр: коды нативных исключений Windows попадают в crash-лог.
    #[cfg(windows)]
    install_native_crash_filter();
    // VEH (stack overflow / heap corruption — мимо SEH-фильтра) + heartbeat
    // памяти/хендлов в run-marker (улика при следующем грязном старте).
    crashdiag::install_veh();
    crashdiag::spawn_heartbeat();
    tauri::Builder::default()
        // Защита от повторного запуска: второй экземпляр не стартует, а фокусирует
        // уже открытое окно. ДОЛЖЕН быть первым плагином (требование Tauri).
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            use tauri::{Emitter, Manager};
            if let Some(win) = app.get_window("main") {
                let _ = win.unminimize();
                let _ = win.show();
                let _ = win.set_focus();
            }
            // Deep link на Windows/Linux приходит аргументом ко второму запуску:
            // выуживаем dev:// / sth:// / api:// и пробрасываем на фронтенд
            // событием `deep-link`.
            if let Some(url) = args
                .iter()
                .find(|a| {
                    a.starts_with("dev://") || a.starts_with("sth://") || a.starts_with("api://")
                })
            {
                let _ = app.emit("deep-link", url.clone());
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![autostart::MINIMIZED_ARG]),
        ))
        .on_window_event(|window, event| {
            // Closing the main shell must quit the whole process — otherwise the
            // hidden P2P messenger helper window keeps the app alive (it lingered
            // in dev after the main window was closed).
            if window.label() == "main" {
                use std::sync::atomic::Ordering;
                match event {
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        // Крестик → в трей (если трей поднялся). «Выход» из трея
                        // или системное завершение — закрываем по-настоящему.
                        if !TRAY_QUIT.load(Ordering::SeqCst)
                            && !APP_EXITING.load(Ordering::SeqCst)
                            && autostart::close_to_tray()
                            && window.app_handle().tray_by_id("main-tray").is_some()
                        {
                            api.prevent_close();
                            let _ = window.hide();
                            return;
                        }
                        MAIN_CLOSE_REQUESTED.store(true, Ordering::SeqCst);
                    }
                    tauri::WindowEvent::Destroyed => {
                        if MAIN_CLOSE_REQUESTED.load(Ordering::SeqCst)
                            || APP_EXITING.load(Ordering::SeqCst)
                        {
                            window.app_handle().exit(0);
                            return;
                        }
                        // Destroyed БЕЗ CloseRequested = окно убито не пользователем
                        // (крэш WebView2-рендерера, очистка %TEMP% с живыми
                        // структурами WebView2, OOM, GPU-драйвер) — фиксируем
                        // улику и пробуем РЕАНИМИРОВАТЬ окно вместо выхода.
                        crash_log(
                            "main window Destroyed WITHOUT CloseRequested — abnormal \
                             (WebView renderer crash / TEMP wipe / OOM / GPU?); trying recovery",
                        );
                        if !MAIN_RECOVERY_TRIED.swap(true, Ordering::SeqCst) {
                            let app = window.app_handle().clone();
                            let cfg = app
                                .config()
                                .app
                                .windows
                                .iter()
                                .find(|w| w.label == "main")
                                .cloned();
                            if let Some(cfg) = cfg {
                                tauri::async_runtime::spawn(async move {
                                    // секунда — WebView2 пересоздаёт служебные файлы
                                    tokio::time::sleep(std::time::Duration::from_millis(
                                        1000,
                                    ))
                                    .await;
                                    let built =
                                        tauri::WebviewWindowBuilder::from_config(&app, &cfg)
                                            .and_then(|b| b.build());
                                    match built {
                                        Ok(_) => crash_log(
                                            "main window RECOVERED after abnormal Destroyed",
                                        ),
                                        Err(e) => {
                                            crash_log(&format!(
                                                "main window recovery FAILED: {e} — exiting"
                                            ));
                                            app.exit(0);
                                        }
                                    }
                                });
                                return;
                            }
                        }
                        window.app_handle().exit(0);
                    }
                    _ => {}
                }
            }
        })
        .register_asynchronous_uri_scheme_protocol("api", |ctx, request, responder| {
            // api://<nodeId>/<provider>/<path> — прозрачный P2P-транспорт для
            // dApp fetch. WebView видит обычный HTTP-ответ, но реальный
            // запрос идёт через iroh к netfory-провайдеру, ответ подписан
            // Ed25519 (проверяем в `api_request_impl`), исходное тело
            // апстрима лежит в `pdata.body_b64` — его и отдаём dApp как
            // тело ответа. Метаданные подписи прокидываем заголовками
            // `X-Sth-Sig-Verified` / `X-Sth-Provider` / `X-Sth-Signed-At`
            // / `X-Sth-Relayed`, чтобы dApp мог проверить целостность.
            //
            // WebView2 (Windows/Linux) маппит схему в `<nodeId>.api.localhost`
            // — снимаем синтетические суффиксы, восстанавливая NodeID.
            let app = ctx.app_handle().clone();
            let uri = request.uri().clone();
            let method = request.method().as_str().to_string();
            let body_bytes = request.body().to_vec();
            // Собираем заголовки из fetch-запроса dApp'а (Authorization: Bearer …
            // и др.) — форвардим их через mesh-packet провайдеру. Whitelist
            // hop-by-hop заголовков делается в `api_request_impl`.
            let req_headers: std::collections::BTreeMap<String, String> = request
                .headers()
                .iter()
                .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.as_str().to_string(), s.to_string())))
                .collect();

            let raw_host = uri.host().unwrap_or("").to_string();
            let host = raw_host
                .strip_suffix(".localhost")
                .unwrap_or(&raw_host)
                .to_string();
            let node_id = host
                .strip_suffix(".api")
                .map(|s| s.to_string())
                .unwrap_or(host);

            // path в URI начинается с `/<provider>/...`; отделяем провайдера.
            let raw_path = uri.path().to_string();
            let (provider, sub_path) = {
                let trimmed = raw_path.trim_start_matches('/');
                match trimmed.find('/') {
                    Some(i) => (trimmed[..i].to_string(), format!("/{}", &trimmed[i + 1..])),
                    None => (trimmed.to_string(), "/".to_string()),
                }
            };
            let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
            let full_path = format!("{sub_path}{query}");
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();

            // OPTIONS preflight — короткое замыкание: возвращаем CORS-headers
            // сразу, без похода в P2P (провайдер не имплементирует OPTIONS).
            if method.eq_ignore_ascii_case("OPTIONS") {
                let pre = tauri::http::Response::builder()
                    .status(204)
                    .header("Access-Control-Allow-Origin", "*")
                    .header("Access-Control-Allow-Methods", "GET, POST, PUT, PATCH, DELETE, OPTIONS")
                    .header(
                        "Access-Control-Allow-Headers",
                        request
                            .headers()
                            .get("access-control-request-headers")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("*"),
                    )
                    .header("Access-Control-Max-Age", "600")
                    .header("Cache-Control", "no-store")
                    .body(Vec::new())
                    .unwrap();
                responder.respond(pre);
                return;
            }

            // Логгируем в SystemConsole — виден в scope=net, тег [api] —
            // так пользователь сразу знает, что handler дошёл и активен.
            crate::syslog::log(
                "net",
                format!("[api] {method} {node_id}/{provider}{full_path}"),
            );

            tauri::async_runtime::spawn(async move {
                fn err_response(msg: String) -> tauri::http::Response<Vec<u8>> {
                    tauri::http::Response::builder()
                        .status(502)
                        .header("Content-Type", "application/json; charset=utf-8")
                        .header("Access-Control-Allow-Origin", "*")
                        .header("Cache-Control", "no-store")
                        .body(
                            serde_json::json!({ "error": msg, "source": "api://" })
                                .to_string()
                                .into_bytes(),
                        )
                        .unwrap()
                }
                if node_id.is_empty() || provider.is_empty() {
                    responder.respond(err_response(
                        "api://<nodeId>/<provider>/<path> — не хватает segments".into(),
                    ));
                    return;
                }
                #[cfg(feature = "p2p")]
                let res = crate::p2p::api_request(
                    app,
                    format!("api://{node_id}/{provider}{full_path}"),
                    method,
                    body_str,
                    req_headers,
                )
                .await;
                #[cfg(not(feature = "p2p"))]
                let res: Result<crate::p2p::ApiFetchResult, String> = Err(
                    "Протокол api:// доступен только в десктоп-сборке (P2P).".into(),
                );

                match res {
                    Err(e) => responder.respond(err_response(e)),
                    Ok(r) => {
                        // `r.body` — красиво отформатированный JSON вида
                        // `{ ...upstream fields..., "pdata": {...} }`. Извлекаем
                        // из `pdata.body_b64` СЫРОЕ тело апстрима — именно его
                        // и ждёт dApp (иначе он ловит наш envelope с `pdata`
                        // и падает: неверный JSON-контракт).
                        let (body_bytes, content_type) = match serde_json::from_str::<
                            serde_json::Value,
                        >(&r.body)
                        {
                            Ok(v) => {
                                let b64 = v
                                    .get("pdata")
                                    .and_then(|p| p.get("body_b64"))
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("");
                                let decoded = if b64.is_empty() {
                                    r.body.clone().into_bytes()
                                } else {
                                    use base64::Engine;
                                    base64::engine::general_purpose::STANDARD
                                        .decode(b64.as_bytes())
                                        .unwrap_or_else(|_| r.body.clone().into_bytes())
                                };
                                // sniff Content-Type: JSON если начинается с '{' / '['
                                let ct = decoded
                                    .iter()
                                    .find(|b| !b.is_ascii_whitespace())
                                    .map(|b| match b {
                                        b'{' | b'[' => "application/json; charset=utf-8",
                                        b'<' => "text/html; charset=utf-8",
                                        _ => "text/plain; charset=utf-8",
                                    })
                                    .unwrap_or("application/octet-stream");
                                (decoded, ct)
                            }
                            Err(_) => (r.body.clone().into_bytes(), "application/json; charset=utf-8"),
                        };
                        let status =
                            if r.status == 0 { 200 } else { r.status };
                        let mut resp = tauri::http::Response::builder()
                            .status(status)
                            .header("Content-Type", content_type)
                            .header("Access-Control-Allow-Origin", "*")
                            .header("Access-Control-Expose-Headers", "*")
                            .header("Cache-Control", "no-store")
                            .header("X-Sth-Sig-Verified", if r.verified { "1" } else { "0" })
                            .header("X-Sth-Provider", r.provider_node_id.clone())
                            .header("X-Sth-Signed-At", r.signed_at.to_string())
                            .header("X-Sth-Relayed", if r.relayed { "1" } else { "0" })
                            .header("X-Sth-Target", r.target.clone());
                        // preflight? Отдельно не обрабатываем — ту же CORS-политику
                        // держим для всех методов, WebView сам не шлёт OPTIONS.
                        let _ = &mut resp;
                        responder.respond(resp.body(body_bytes).unwrap());
                    }
                }
            });
        })
        .register_uri_scheme_protocol("sth", |ctx, request| {
            let app = ctx.app_handle();
            let uri = request.uri().clone();
            let host = uri.host().unwrap_or("").to_string();
            // app id now travels in the HOST (sth://<id>/...) so absolute SPA
            // asset paths resolve relative to the authority and keep the app
            // context on every platform; root-absolute assets arriving on a
            // host-stripped request are routed back via the Referer header.
            let referer = request
                .headers()
                .get("referer")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            match resolve_request(app, &host, uri.path(), &referer, uri.query().unwrap_or("")) {
                Some((bytes, mime)) => tauri::http::Response::builder()
                    .status(200)
                    .header("Content-Type", mime)
                    .header("Access-Control-Allow-Origin", "*")
                    // never cache: each sth://<id> app is distinct content; a shared
                    // webview cache must not serve one app's index.html/assets for another.
                    .header("Cache-Control", "no-store, no-cache, must-revalidate")
                    .header("Pragma", "no-cache")
                    .body(bytes)
                    .unwrap(),
                None => tauri::http::Response::builder()
                    .status(404)
                    .header("Content-Type", "text/html; charset=utf-8")
                    .header("Cache-Control", "no-store")
                    .body(crate::protocol::not_found_page().into_bytes())
                    .unwrap(),
            }
        })
        .register_uri_scheme_protocol("f", |ctx, request| {
            // f://<cid> — decentralized File Exchange access. The CID rides in the
            // authority (host); the path is a fallback. resolve_file_request does a
            // case-insensitive registry recovery for down-cased authorities and
            // enforces the free/owner access gate (paid verification = Stage 3).
            let app = ctx.app_handle();
            let uri = request.uri().clone();
            let raw_host = uri.host().unwrap_or("").to_string();
            // Windows/WebView2 maps the scheme to `<cid>.f.localhost`; strip the
            // synthetic suffixes so the CID survives on every platform.
            let host = raw_host.strip_suffix(".localhost").unwrap_or(&raw_host);
            let host = host.strip_suffix(".f").unwrap_or(host).to_string();
            let cid = if host.is_empty() || host.eq_ignore_ascii_case("f") {
                uri.path().trim_start_matches('/').to_string()
            } else {
                host
            };
            match crate::files::resolve_file_request(app, &cid) {
                Some((bytes, mime)) => tauri::http::Response::builder()
                    .status(200)
                    .header("Content-Type", mime)
                    .header("Access-Control-Allow-Origin", "*")
                    .header("Cache-Control", "no-store, no-cache, must-revalidate")
                    .header("Pragma", "no-cache")
                    .body(bytes)
                    .unwrap(),
                None => tauri::http::Response::builder()
                    .status(404)
                    .header("Content-Type", "text/html; charset=utf-8")
                    .header("Cache-Control", "no-store")
                    .body(crate::protocol::not_found_page().into_bytes())
                    .unwrap(),
            }
        })
        .setup(|app| {
            autostart::init();
            if let Err(e) = setup_tray(app.handle()) {
                syslog::log("sys", format!("tray icon unavailable: {e}"));
            } else if autostart::started_minimized() {
                // автозапуск при входе в систему: сразу в трей, окно — по клику на иконку
                use tauri::Manager;
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
                syslog::log("sys", "started minimized to tray (autostart)");
            }
            webproxy::start(app.handle().clone());
            // ---- Startup Profiler: длительность каждого шага запуска ----
            let boot_t0 = std::time::Instant::now();
            let mut boot_marks: Vec<(&'static str, u128)> = Vec::new();
            let mut boot_step_t = std::time::Instant::now();
            macro_rules! boot_mark {
                ($n:expr) => {{
                    boot_marks.push(($n, boot_step_t.elapsed().as_millis()));
                    boot_step_t = std::time::Instant::now();
                }};
            }
            let dir = ensure_data_dir(&app.handle());
            println!("[SmartNet] data directory ready: {}", dir.display());
            boot_mark!("data-dir");
            // dev:// deep linking: на dev/Linux регистрируем схему в рантайме и
            // слушаем открытие ссылки — пробрасываем URL на фронтенд событием
            // `deep-link` (Vue Router перехватит и смаршрутизирует).
            {
                use tauri::Manager;
                use tauri::Emitter;
                use tauri_plugin_deep_link::DeepLinkExt;
                #[cfg(any(target_os = "linux", debug_assertions))]
                {
                    let _ = app.deep_link().register("dev");
                    let _ = app.deep_link().register("sth");
                    let _ = app.deep_link().register("api");
                }
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    if let Some(url) = event.urls().into_iter().next() {
                        let _ = handle.emit("deep-link", url.to_string());
                    }
                });
                // Холодный старт: ссылка первого запуска лежит в argv, но фронт
                // подпишется на `deep-link` позже — буферизуем, фронт заберёт
                // через команду take_pending_deep_link после монтирования.
                if let Some(url) = std::env::args().find(|a| {
                    a.starts_with("dev://") || a.starts_with("sth://") || a.starts_with("api://")
                }) {
                    *PENDING_DEEP_LINK.lock().unwrap() = Some(url);
                }
            }
            // Стелс-режим: синхронизируем флаг из БД + регистрируем сетевой
            // менеджер в Tauri State (потокобезопасно, Arc<Mutex<NetworkManager>>).
            boot_mark!("deep-link");
            crate::stealth::init_from_db();
            boot_mark!("stealth-db (sled open/recovery)");
            app.manage(crate::stealth::new_state());
            // Floating System Console: register the app handle so internal
            // P2P/DHT/network events can stream to the UI console.
            crate::syslog::init(app.handle().clone());
            boot_mark!("syslog-init");
            crate::syslog::log("sys", format!("data directory ready: {}", dir.display()));
            // Show the current client version in the window title bar.
            if let Some(win) = app.get_window("main") {
                let _ = win.set_title(&format!(
                    "NETFORY // SmartHoldem WEB4 P2P network v{}",
                    env!("CARGO_PKG_VERSION")
                ));
            }
            // Blockchain config oracle: sync global app prices on boot + every 24h.
            crate::oracle::spawn_oracle_sync();
            // Official Repository: скан реестров доверенных разработчиков и
            // одобренных dApps (при старте + раз в 24ч).
            crate::official::spawn_scanner();
            // On-chain username registry: scan the naming sink + index u:// names.
            crate::naming::spawn_indexer();
            // Decentralized .sth Name Service: index domain registrations + prices.
            crate::dns_manager::spawn_indexer();
            crate::files::spawn_indexer();
            crate::files::spawn_private_scanner(app.handle().clone());
            crate::subs::spawn_indexer();
            // Node pool: ping the SmartHoldem mainnet pool and connect to the
            // fastest node; re-evaluate periodically (auto-failover overloaded nodes).
            // Give the (blocking) node REST client access to the P2P transport
            // so an `api://` node can be used transparently for wallet calls.
            // ДО селектора: пинг iroh-узлов пула идёт через этот handle.
            crate::sth_node::set_app_handle(app.handle().clone());
            crate::sth_node::spawn_node_selector();
            // SHIP-44: push-события кошелька по iroh (`sth/events/1`) вместо поллинга.
            crate::wallet_stream::spawn(app.handle().clone());
            // Бейдж непрочитанных в трее — стартовое значение из БД комнат.
            crate::chat::refresh_tray_unread();
            // Watchdog UI-потока (диагностика «не отвечает»); OFF если WATCHDOG≠true
            crate::watchdog::spawn(app.handle().clone());
            crate::chat::spawn_key_indexer();
            // Phase 2 chat: poll incoming PAID (blockchain-bound) messages.
            crate::chat::spawn_msg_indexer(app.handle().clone());
            crate::chat::media::spawn_cache_sweeper(app.handle().clone());
            crate::chat::spawn_sync_loop(app.handle().clone());
            // Phase C: watch the wallet balance and emit `sth-balance-changed`
            // on incoming funds (frontend → toast when focused, OS notification when not).
            crate::wallet::spawn_balance_watcher(app.handle().clone());
            // Phase G: resume background "Earn STH" seeding if the user left it on.
            boot_mark!("bg-spawns");
            crate::seeding::maybe_resume(app.handle());
            boot_mark!("seeding-resume");
            // AI Computer: возобновить телеметрию AI Pool, если тумблер включён.
            crate::ai_telemetry::maybe_resume();
            boot_mark!("ai-telemetry");
            // BitTorrent: init the librqbit session + resume persisted torrents at
            // startup, so downloads/seeds continue immediately (previously the
            // session was lazy — torrents only reappeared after user interaction).
            crate::torrent::resume_on_startup(app.handle().clone());
            // Глобальный индикатор скорости сети (левый сайдбар): rx/tx всех
            // интерфейсов раз в секунду событием `net-speed`.
            crate::netstat::start(app.handle().clone());
            boot_mark!("torrent-resume+netstat");
            // Языковой пакет переводчика: если модель ещё не установлена, через
            // 2 минуты после старта тихо запускаем скачивание ТОРРЕНТОМ в фоне
            // (по аналогии с AUTOUPDATE). Прогресс идёт на Стену через события.
            {
                let th = app.handle().clone();
                let _ = std::thread::Builder::new().name("translator-bootstrap".into()).spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(120));
                    let th2 = th.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = crate::translator::translator_bootstrap(th2).await;
                    });
                });
            }
            // P2P-обновления: стартовая зачистка папок обновлений (оставляем
            // только текущую рабочую версию). В ФОНЕ: удаление гигабайтных
            // пакетов на медленном диске блокировало главный поток на секунды
            // («приложение висит первые секунды после запуска»).
            {
                let cleanup_handle = app.handle().clone();
                std::thread::spawn(move || {
                    crate::updater::cleanup_on_start(&cleanup_handle);
                });
            }
            // asset:// scope: кэш мессенджера может жить вне $APPDATA, если
            // пользователь перенёс папку данных (cfg:data_dir) — без этого
            // видео из чата не воспроизводится (asset-протокол блокирует путь).
            {
                use tauri::Manager as _;
                let _ = app
                    .asset_protocol_scope()
                    .allow_directory(crate::chat::media::cache_dir(&app.handle()), true);
            }
            // Мониторинг фактического интернет-подключения (navigator.onLine в
            // WebView после сна не срабатывает) → событие `net-status` в UI.
            crate::netwatch::spawn(app.handle().clone());
            // Loopback-IPC для git-remote-dev (git clone/push/pull dev://…).
            crate::devhub_ipc::spawn(app.handle().clone());
            boot_mark!("asset-scope+netwatch+devhub");
            // Живой поиск DevHub: фоновый ре-синк известных авторов (раз в 30 мин).
            crate::devhub::spawn_resync(app.handle().clone());
            // Web3 dApp bridge: embedded dApps EMIT `dapp-request` over the event
            // channel (remote origins can't invoke commands). We dispatch + reply
            // via `dapp-response` / surface approval on the host Verify view.
            let bridge_handle = app.handle().clone();
            app.listen_any("dapp-request", move |event| {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    let method = v["method"].as_str().unwrap_or("").to_string();
                    let origin = v["origin"].as_str().unwrap_or("").to_string();
                    let params = v["params"].clone();
                    if !id.is_empty() && !method.is_empty() {
                        crate::dapp::process_dapp_request(&bridge_handle, id, method, params, origin);
                    }
                }
            });
            // Перевод страниц DApp: content-скрипт эмитит `dapp-translate-request`
            // (пакет строк), бэкенд переводит и отвечает `dapp-translate-response`.
            let tr_handle = app.handle().clone();
            app.listen_any("dapp-translate-request", move |event| {
                crate::translator::handle_dapp_translate_request(&tr_handle, event.payload());
            });
            // dApp network tap → SystemConsole. Bridge JS в webview'ах dApp'ов
            // эмитит `dapp-log-event` на каждый fetch/XHR. Мы форматируем
            // короткую строку для scope='dapp' и проксируем полный payload
            // как `dapp-log-detail` (frontend показывает detail-popup при
            // клике). Никакой network-активности мы НЕ выполняем — WebView
            // сам делает запрос, мы только наблюдаем.
            let log_handle = app.handle().clone();
            app.listen_any("dapp-log-event", move |event| {
                use tauri::Emitter;
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    let method = v["method"].as_str().unwrap_or("GET").to_string();
                    let url = v["url"].as_str().unwrap_or("").to_string();
                    // Жёсткий фильтр внутренних IPC-запросов Tauri (WebView2/WKWebView
                    // маршрутизируют `invoke` / `event.emit` через `http://ipc.localhost/*`).
                    // Если JS-хук пропустил (race при `api.event.emit`) — Rust страхует.
                    // Иначе один вызов `emit('dapp-log-event')` рекурсивно порождает
                    // новый fetch к ipc.localhost → мгновенно вешает CPU и память.
                    {
                        let low = url.to_ascii_lowercase();
                        if low.starts_with("http://ipc.localhost")
                            || low.starts_with("https://ipc.localhost")
                            || low.starts_with("http://tauri.localhost")
                            || low.starts_with("https://tauri.localhost")
                            || low.starts_with("ipc://")
                            || low.starts_with("tauri://")
                            || low.contains("ipc.localhost/")
                            || low.contains("tauri.localhost/")
                        {
                            return;
                        }
                    }
                    let status = v["status"].as_u64().unwrap_or(0);
                    let ms = v["ms"].as_u64().unwrap_or(0);
                    let ok = v["ok"].as_bool().unwrap_or(false);
                    let err = v["err"].as_str().unwrap_or("").to_string();
                    let lower = url.to_lowercase();
                    let is_ours = ["sth://", "api://", "sn://", "f://", "dev://"]
                        .iter()
                        .any(|p| lower.starts_with(p));
                    let is_http = lower.starts_with("http://") || lower.starts_with("https://");
                    let tag = if !ok || (status > 0 && status >= 400) {
                        "ERR"
                    } else if is_ours {
                        "OK"
                    } else if is_http {
                        "EXT"
                    } else {
                        "INT"
                    };
                    let status_str = if status == 0 {
                        if err.is_empty() { "n/a".to_string() } else { format!("err:{}", err) }
                    } else {
                        status.to_string()
                    };
                    // #id — линкует строку syslog с detail-payload'ом на фронте.
                    let id_short = if id.is_empty() { String::new() } else { format!(" #{}", id) };
                    crate::syslog::log(
                        "dapp",
                        format!("{tag} {method} {status_str} {ms}ms {url}{id_short}"),
                    );
                    // Проксируем полный payload на фронт — там его положат в
                    // Map<id, details> для popup при клике по строке.
                    let _ = log_handle.emit("dapp-log-detail", v);
                }
            });
            boot_mark!("dapp-listeners");
            // итог профайлера: шаги ≥3мс + общее время setup — сразу видно,
            // что тормозит старт (лог «boot» в системной консоли)
            {
                let total = boot_t0.elapsed().as_millis();
                for (n, ms) in &boot_marks {
                    if *ms >= 3 {
                        crate::syslog::log("boot", format!("step '{}': {} ms", n, ms));
                    }
                }
                crate::syslog::log("boot", format!("setup total: {} ms", total));
                println!("[SmartNet] setup total: {} ms", total);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            take_pending_deep_link,
            mail_launcher::launch_oxid_mail,
            messenger_launcher::launch_messenger,
            messenger_launcher::messenger_available,
            messenger_launcher::messenger_running,
            messenger_launcher::messenger_open,
            messenger_launcher::chat_export_archive,
            official::official_lists,
            official::official_refresh,
            cloudseeder::vdapps_status,
            vault_exists,
            load_vault,
            save_vault,
            wallet::delete_vault,
            wallet::wipe_all,
            create_vault,
            unlock_vault,
            derive_account,
            reveal_seed,
            session_clear,
            session_status,
            apps::get_data_dir,
            apps::set_data_dir,
            apps::app_install_source,
            apps::storage_usage,
            apps::get_storage_limit,
            apps::set_storage_limit,
            apps::disk_space,
            wallet::get_active_smartnet_account,
            wallet::get_wallet_transactions,
            wallet::get_wallet_tokens,
            wallet::get_rate_history,
            wallet::probe_network_api,
            wallet::list_accounts,
            wallet::get_main_smartnet_account,
            wallet::add_account,
            wallet::rename_account,
            wallet::set_active_account,
            wallet::recent_donations,
            wallet::donor_tier,
            wallet::get_exchange_rate,
            wallet::get_swap_quote,
            wallet::get_buy_deposit_address,
            wallet::get_sell_gate_address,
            wallet::send_funds_request,
            wallet::notify_exchange_deposit,
            wallet::execute_exchange_sell,
            wallet::sign_and_broadcast_tx,
            wallet::token_api,
            wallet::token_logo_text,
            wallet::token_ticker_taken,
            wallet::token_op,
            wallet::reject_tx_request,
            wallet::check_unverified_accept,
            wallet::pay_unverified_accept,
            dapp::dapp_provider_request,
            dapp::resolve_dapp_connect,
            dapp::resolve_sign_message,
            dapp::reject_sign_message,
            apps::pick_folder,
            apps::save_file_dialog,
            apps::create_draft_app,
            apps::create_domain_draft,
            apps::finalize_publish,
            apps::edit_app_metadata,
            apps::preview_draft,
            apps::app_logo,
            apps::app_preview,
            apps::update_app,
            apps::update_app_at,
            apps::get_payload_pricing,
            apps::inspect_payload,
            apps::inspect_app_payload,
            apps::delete_app,
            apps::list_apps,
            private_dapp::private_dapp_mount,
            private_dapp::private_dapp_unmount,
            private_dapp::private_dapp_meta,
            private_dapp::private_dapp_recipients,
            private_dapp::private_dapp_set_recipients,
            private_dapp::private_reveal,
            apps::announced_apps,
            apps::verify_app,
            apps::read_readme,
            profile::load_profile_manifest,
            profile::save_profile_config,
            naming::get_my_username,
            naming::recover_my_username,
            naming::resync_registry,
            naming::resolve_username,
            naming::username_available,
            naming::username_reserved,
            naming::register_username,
            naming::get_registration_fee,
            naming::username_of_address,
            naming::list_usernames,
            dns_manager::resolve_domain,
            dns_manager::domain_of_app,
            dns_manager::domain_lookup,
            dns_manager::register_domain,
            dns_manager::renew_domain,
            dns_manager::renewal_price,
            dns_manager::list_sell,
            dns_manager::domain_order,
            dns_manager::list_market_orders,
            dns_manager::atomic_buy,
            dns_manager::list_my_domains,
            subs::get_subscribe_price,
            subs::follow_stats,
            subs::follow_stats_cached,
            subs::subscribe,
            subs::weekly_top,
            subs::refresh_subscribers,
            clearnet::open_external,
            chat::get_my_chat_pubkey,
            chat::resolve_chat_key,
            chat::chat_key_published,
            chat::chat_key_status,
            chat::publish_chat_key,
            open_in_main,
            open_link_in_main,
            chat::chat_my_contact_link,
            chat::chat_add_contact,
            chat::chat_list_contacts,
            chat::chat_remove_contact,
            chat::resolve_msg_cost,
            chat::gossip_status,
            open_crash_log,
            crashdiag::get_process_resources,
            chat::gossip_room_peers,
            chat::recommended_fee,
            chat::get_chat_settings,
            chat::set_chat_settings,
            chat::publish_msg_cost,
            chat::open_room,
            chat::create_group,
            chat::join_group,
            chat::group_invite,
            chat::delete_room,
            chat::get_group_policy,
            chat::set_group_policy,
            chat::ban_member,
            chat::unban_member,
            chat::list_bans,
            chat::my_group_bans,
            chat::prune_ban_table,
            chat::sync_incoming_bans,
            chat::get_rooms,
            chat::get_messages,
            chat::chat_get_messages_page,
            chat::voice::voice_call,
            chat::voice::voice_answer,
            chat::voice::voice_hangup,
            chat::voice::voice_mute,
            chat::voice::voice_audio_settings,
            chat::voice::voice_set_audio,
            chat::voice::voice_audio_route,
            chat::voice::voice_meter,
            chat::voice::voice_group_join,
            chat::send_message,
            chat::chat_typing,
            chat::chat_acks,
            chat::chat_react,
            chat::chat_edit_message,
            chat::chat_reactions,
            chat::clear_chat_history,
            chat::chat_delete_message,
            chat::media::chat_send_image,
            chat::media::chat_load_image,
            chat::media::chat_cache_info,
            chat::media::chat_cache_set,
            chat::media::chat_cache_clear,
            chat::media::chat_send_audio,
            chat::media::chat_send_file,
            chat::media::chat_load_file,
            chat::media::chat_video_begin,
            chat::media::chat_video_chunk,
            chat::media::chat_video_abort,
            chat::media::chat_video_send,
            chat::media::chat_load_video,
            chat::media::chat_read_dropped,
            chat::media::chat_load_audio,
            chat::media::chat_cancel_media,
            chat::media::chat_media_progress,
            chat::media::chat_save_media,
            chat::media::chat_group_logo_seed,
            toggle_chat_window,
            open_image_window,
            close_image_window,
            toggle_console_window,
            oracle::get_app_prices,
            oracle::get_incentivized_seeding,
            oracle::set_incentivized_seeding,
            seeding::get_seeding_config,
            seeding::toggle_background_seeding,
            seeding::get_seeding_history_stats,
            seeding::heartbeat_status,
            ai_telemetry::ai_pool_status,
            ai_telemetry::ai_pool_toggle,
            ai_telemetry::ai_hw_snapshot,
            ai_telemetry::ai_net_stats,
            ai_client::ai_console_generate,
            ai_client::ai_console_cancel,
            ai_client::ai_model_download,
            ai_telemetry::ai_models_cache_stats,
            ai_telemetry::ai_models_cache_clear,
            wallet::show_native_notification,
            p2p::start_discovery,
            p2p::discovered_peers,
            p2p::app_share_info,
            p2p::dht_resolve_app,
            p2p::stop_discovery,
            p2p::get_trackers,
            p2p::network_earnings,
            p2p::payout_history,
            p2p::set_trackers,
            p2p::get_seeders,
            p2p::set_seeders,
            p2p::get_seeders_auto,
            p2p::get_seeders_effective,
            p2p::get_relay_only,
            p2p::set_relay_only,
            p2p::get_mobile_fallbacks,
            p2p::set_mobile_fallbacks,
            p2p::get_swarm_enabled,
            p2p::set_swarm_enabled,
            p2p::swarm_top_seeders,
            p2p::get_dht_providers_enabled,
            p2p::set_dht_providers_enabled,
            p2p::dht_providers_summary,
            p2p::get_dht_enabled,
            p2p::set_dht_enabled,
            p2p::dht_stats,
            p2p::dht_nodes_list,
            p2p::net_doctor_run,
            p2p::net_doctor_check_relays,
            syslog::sys_log_history,
            syslog::ui_log,
            p2p::get_relay_config,
            p2p::set_relay_config,
            p2p::get_auto_update,
            p2p::set_auto_update,
            p2p::p2p_status,
            p2p::api_fetch,
            p2p::dapp_ws_open,
            p2p::dapp_ws_send,
            p2p::dapp_ws_close,
            p2p::network_map,
            p2p::peer_sources,
            p2p::discovery_probe,
            p2p::network_storage,
            p2p::blob_store_usage,
            p2p::optimize_blob_store,
            p2p::dedup_stats,
            p2p::verify_peers_storage,
            sync_status::sync_status,
            p2p::install_app,
            p2p::prefetch_app,
            p2p::discovery_logo,
            p2p::discovery_preview,
            webview::open_app_webview,
            webview::open_app_webview_new,
            webview::open_file_webview,
            webview::open_embedded_webview,
            webview::set_embedded_bounds,
            webview::hide_embedded_webview,
            webview::close_embedded_webview,
            webview::close_all_embedded_webviews,
            webview::dapp_translate_toggle,
            webview::dapp_translate_config,
            webview::dapp_history_back,
            webview::dapp_history_forward,
            webview::dapp_reload,
            websites::open_site_webview,
            websites::site_navigate,
            websites::ui_set_locale,
            websites::set_site_bounds,
            websites::hide_site_webview,
            websites::close_site_webview,
            websites::close_all_site_webviews,
            websites::site_history_back,
            websites::site_history_forward,
            websites::site_reload,
            websites::site_reveal_download,
            webproxy::webproxy_status,
            webproxy::webproxy_set_mode,
            webproxy::webproxy_set_nodes,
            webproxy::webproxy_set_enabled,
            webproxy::webproxy_set_total,
            mycel::mycelium_status,
            mycel::mycelium_set,
            mycel::mycelium_circuit,
            mycel::mycelium_rebuild,
            mycel::mycelium_hop_addrs,
            webproxy::webproxy_site_bytes,
            webproxy::webproxy_exit_countries,
            webproxy::webproxy_set_exit_country,
            webproxy::webproxy_new_identity,
            webproxy::webproxy_set_exit_default,
            webproxy::webproxy_check_ip,
            webproxy::webproxy_check_ip_cancel,
            mycel_vpn::mycelium_vpn_set,
            netstats::net_conn_stats,
            autostart::autostart_get,
            autostart::autostart_set,
            webproxy::webproxy_set_v6,
            webproxy::webproxy_recheck,
            geo::geo_db_ready,
            geo::ensure_geo_db,
            geo::geo_lookup,
            files::pick_file,
            files::add_file,
            files::list_my_files,
            files::list_exchange_files,
            files::file_meta,
            files::set_file_seeding,
            files::delete_file,
            files::file_disk_stats,
            files::publish_file,
            files::register_shared_file,
            files::file_seed_count,
            files::file_download_progress,
            files::download_file,
            files::start_file_download,
            files::delete_downloaded_file,
            files::save_file_as,
            files::open_file_native,
            files::buy_file,
            files::file_sales_counts,
            files::check_file_access,
            files::notify_private_recipients,
            files::list_private_inbox,
            social::pick_image_file,
            social::set_profile_avatar,
            social::get_profile_avatar,
            social::my_avatar_hash,
            social::author_avatar,
            social::author_avatar_cached,
            social::attach_image_to_post,
            social::fetch_wall_media,
            social::create_post,
            social::list_wall,
            social::delete_post,
            social::add_comment,
            social::list_comments,
            social::post_reactions,
            social::react_to_post,
            social::follow_wall,
            social::unfollow_wall,
            social::list_wall_follows,
            social::is_following_wall,
            social::open_wall,
            social::hide_comment,
            social::ban_commenter,
            social::unban_commenter,
            social::list_blocked,
            social::social_attach_file,
            social::social_file_bytes,
            social::social_save_file,
            social::playlist_add,
            social::playlist_remove,
            social::playlist_list,
            social::playlist_reorder,
            social::pick_audio_files,
            social::read_audio_meta,
            social::album_create,
            social::album_update,
            social::album_delete,
            social::album_list,
            social::album_feed,
            social::album_get,
            social::album_buy,
            social::album_access,
            social::album_sync_keys,
            social::album_fetch_key,
            social::album_sales,
            social::album_revenue,
            social::album_sales_series,
            social::dislike_user,
            reputation::get_reputation,
            translator::translator_status,
            translator::translator_download,
            translator::translator_bootstrap,
            translator::detect_lang,
            translator::translate_text,
            translator::translate_prewarm,
            translator::translation_cache_stats,
            translator::clear_translation_cache,
            translator::translator_progress,
            translator::translator_build_accel,
            translator::translator_dapp_capable,
            translator::translator_engine_label,
            translator::translator_presets,
            social::album_share_link,
            social::album_open_link,
            social::album_export_torrent,
            social::mark_wall_read,
            social::wall_unread_counts,
            sth_node::refresh_node_pool,
            sth_node::auto_select_node,
            sth_node::select_node,
            sth_node::current_node,
            sth_node::node_status,
            wallet_stream::wallet_stream_status,
            wallet_stream::wallet_stream_reconnect,
            txindex::txindex_status,
            peerbook::peerbook_list,
            sth_node::list_custom_nodes,
            sth_node::add_custom_node,
            sth_node::remove_custom_node,
            sth_node::ping_custom_node,
            dns_manager::domains_by_owner,
            torrent::parse_magnet_link,
            torrent::parse_torrent_file,
            torrent::parse_torrent_path,
            torrent::start_torrent_download,
            torrent::quick_start_magnet,
            torrent::get_active_torrents,
            torrent::pause_torrent,
            torrent::resume_torrent,
            torrent::remove_torrent,
            torrent::open_torrent_folder,
            torrent::open_torrent_file,
            torrent::get_torrent_magnet,
            torrent::get_torrent_file_bytes,
            torrent::create_torrent_from_path,
            torrent::get_bt_trackers,
            torrent::refresh_bt_trackers,
            torrent::get_torrent_limits,
            torrent::set_torrent_limits,
            torrent::get_torrent_dht_nodes,
            torrent::get_profile_stats,
            torrent::get_torrent_net_settings,
            torrent::set_torrent_net_settings,
            torrent::get_rutracker_key,
            torrent::set_rutracker_key,
            updater::get_update_period_hours,
            updater::get_client_version,
            updater::current_os,
            updater::check_for_update,
            updater::start_update_download,
            updater::read_update_manifest,
            updater::resolve_installer,
            updater::reset_update_package,
            updater::resolve_installer_path,
            updater::cleanup_update_dirs,
            updater::run_p2p_installer,
            stealth::toggle_stealth_mode,
            stealth::get_stealth_status,
            netwatch::net_online,
            dev_router::parse_dev_url,
            dev_router::get_user_repositories,
            dev_router::get_repository,
            dev_router::search_dev_network,
            devhub::devhub_create_repo,
            devhub::devhub_feed,
            devhub::devhub_delete_repo,
            devhub::devhub_list_files,
            devhub::devhub_read_file,
            devhub::devhub_files_local,
            devhub::devhub_sync_files,
            devhub::devhub_fork_repo,
            devhub::devhub_sync_fork,
            devhub::devhub_activity,
            devhub::devhub_day_events,
            devhub::devhub_create_release,
            devhub::devhub_edit_release,
            devhub::devhub_delete_release,
            devhub::devhub_toggle_star,
            devhub::devhub_starred_by_me,
            devhub::devhub_discover_network,
            devhub::devhub_commit_diff,
            devhub_pr::devhub_create_pr,
            devhub_pr::devhub_list_prs,
            devhub_pr::devhub_accept_pr,
            devhub_pr::devhub_reject_pr,
            devhub_pr::devhub_pr_open_count,
            torrent::torrent_list_files,
            torrent::torrent_stream_url,
            cloudvault::cloud_vault_open,
            cloudvault::cloud_vault_close,
            cloudvault::detect_onedrive_folder,
            cloudvault::cloud_vault_status,
            cloudvault::cloud_vault_ls,
            cloudvault::cloud_vault_push,
            cloudvault::cloud_vault_pull,
            cloudvault::cloud_vault_yandex_oauth,
            cloudvault::cloud_vault_yandex_refresh,
            cloudvault::cloud_vault_google_oauth,
            cloudvault::cloud_vault_google_refresh,
            cloudvault::cloud_vault_mkdir,
            cloudvault::cloud_vault_delete,
            cloudvault::cloud_vault_reset,
            cloudvault::cloud_vault_quota,
            cloudvault::cloud_vault_dict_status,
            cloudvault::cloud_vault_dict_train,
            cloudvault::cloud_vault_seal,
            cloudvault::cloud_vault_unseal,
            cloudvault::cloud_vault_account_info,
            cloudvault::cloud_vault_cached_stored,
            cloudvault::cloud_vault_open_raid,
            cloudvault::cloud_vault_reset_raid,
            cloudvault::cloud_vault_raid_resync,
            cloudvault::cloud_vault_scan_paths,
            cloudseeder::cloud_seeder_publish,
            cloudseeder::cloud_seeder_fetch,
            cloudseeder::cloud_seeder_install,
            cloudseeder::cloud_seeder_install_any,
            cloudseeder::cloud_seeder_scan_apps,
            cloudseeder::cloud_seeder_delete_dapp,
            cloudseeder::cloud_seeder_status_get,
            cloudseeder::cloud_seeder_rescues_get,
            cloudseeder::cloud_seeder_registry_get,
            cloudseeder::cloud_seeder_registry_set,
            cloudseeder::cloud_seeder_registry_origins_get,
            profile_seeder::cloud_seeder_publish_profiles,
            profile_seeder::cloud_seeder_fetch_profiles_manifest,
            profile_seeder::cloud_seeder_fetch_profile,
            profile_seeder::cloud_seeder_fetch_profile_file,
            profile_seeder::profile_seeder_autorep_get,
            profile_seeder::profile_seeder_autorep_set,
            profile_seeder::profile_seeder_last_replicated,
            profile_seeder::profile_seeder_has_changes,
        ])
        .build(tauri::generate_context!())
        .expect("error while running SmartNet")
        .run(|_app, event| {
            match event {
                // штатный выход инициирован — реанимацию окон не запускаем
                tauri::RunEvent::ExitRequested { .. } => {
                    APP_EXITING.store(true, std::sync::atomic::Ordering::SeqCst);
                    let _ = DB.flush();
                }
                // штатное завершение — снимаем маркер грязного выхода
                tauri::RunEvent::Exit => {
                    let _ = DB.flush();
                    let _ = std::fs::remove_file(run_marker_path());
                }
                _ => {}
            }
        });
}
