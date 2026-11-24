// ============================================================
//  SmartNet // webview.rs
//
//  Native dApp webview hosting (Tauri `unstable` multi-webview). dApps render
//  as REAL native child webviews over the Vue tab-host area (not iframes),
//  loaded straight from the `sth://` scheme. Each is created with the
//  `window.smartholdem` bridge pre-injected, and the frontend keeps their
//  bounds in sync and shows/hides/closes them on tab switch / lock.
//
//  Author: TechnoL0g
//  Created: 2026-06-18
// ============================================================

use crate::dapp;
use crate::sanitize;
use tauri::webview::WebviewBuilder;
use tauri::Manager;
use tauri::{LogicalPosition, LogicalSize, WebviewUrl, WebviewWindowBuilder};

/// Windows/WebView2: авто-разрешение микрофона для окна мессенджера (голосовые).
/// Wry 0.55 обрабатывает PermissionRequested только для clipboard, а дефолтный
/// промпт WebView2 на кастомном tauri-origin может не показаться вовсе —
/// вешаем свой обработчик, отвечающий ALLOW ТОЛЬКО на запрос микрофона.
#[cfg(target_os = "windows")]
pub(crate) fn allow_mic(win: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
        COREWEBVIEW2_PERMISSION_STATE_ALLOW,
    };
    use webview2_com::PermissionRequestedEventHandler;
    let _ = win.with_webview(|wv| unsafe {
        let Ok(core) = wv.controller().CoreWebView2() else {
            return;
        };
        let mut token = 0i64;
        let _ = core.add_PermissionRequested(
            &PermissionRequestedEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                args.PermissionKind(&mut kind)?;
                if kind == COREWEBVIEW2_PERMISSION_KIND_MICROPHONE {
                    args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
                }
                Ok(())
            })),
            &mut token,
        );
    });
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn allow_mic(_win: &tauri::WebviewWindow) {}

/// Навигационный страж dApp-webview (v1.63.11): переход по кастомной схеме
/// SmartNet (sth:// u:// sn:// api:// f:// dev://) — например через
/// `window.location.href = 'sth://…'`, который нельзя перехватить из JS —
/// блокируется, а URL пересылается host-shell'у событием `dapp-open-link`,
/// где открывается новой native-вкладкой. sth:// на собственный appId
/// (внутренняя SPA-навигация) пропускается.
fn dapp_nav_guard(
    app: tauri::AppHandle,
    address: String,
) -> impl Fn(&tauri::Url) -> bool + Send + 'static {
    move |url| {
        let scheme = url.scheme();
        if !matches!(scheme, "sth" | "u" | "sn" | "api" | "f" | "dev") {
            return true;
        }
        if scheme == "sth"
            && url
                .host_str()
                .map(|h| h.eq_ignore_ascii_case(&address))
                .unwrap_or(false)
        {
            return true;
        }
        crate::syslog::emit_queued(&app, "dapp-open-link", serde_json::json!({ "app_id": address, "url": url.to_string() }));
        false
    }
}

#[tauri::command]
pub(crate) async fn open_app_webview(
    app: tauri::AppHandle,
    address: String,
    title: String,
    path: Option<String>,
) -> Result<(), String> {
    let label = format!("app-{}", sanitize(&address));
    if let Some(win) = app.get_webview_window(&label) {
        // v1.63.6: если приходит новый path (deep-link на внутреннюю страницу
        // dApp), навигируем существующее окно на новый URL, а не просто
        // фокусируем его на старом маршруте. Иначе поделиться ссылкой
        // вида sth://<addr>/trading/PAIR было невозможно — окно всегда
        // возвращалось на корневой /.
        if let Some(sub) = normalize_dapp_path(path.as_deref()) {
            let target = sth_url(&address, &sub);
            let js = format!(
                "try {{ window.location.assign({}); }} catch (e) {{ /* ignore */ }}",
                serde_json::to_string(&target).unwrap_or_else(|_| "\"about:blank\"".to_string())
            );
            let _ = win.eval(&js);
        }
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        // v1.135.84: сворачиваем главное окно, чтобы dApp вышел на передний
        // план и не перекрывался — пользователь просил "открыть в окне =
        // основное свернуть программно".
        if let Some(main) = app.get_window("main") {
            let _ = main.minimize();
        }
        return Ok(());
    }
    // Native dApps load via the custom sth:// scheme (no local HTTP server).
    // App id lives in the HOST (sth://<id>/) so absolute SPA asset paths
    // (/_nuxt/x.js) resolve relative to the authority and keep the app context
    // on every platform (Windows/wry маппит в `http://sth.<id>/`).
    let sub = normalize_dapp_path(path.as_deref()).unwrap_or_default();
    let raw_url = sth_url(&address, &sub);
    let url = WebviewUrl::External(
        raw_url
            .parse()
            .unwrap_or_else(|_| "about:blank".parse().unwrap()),
    );
    let win = WebviewWindowBuilder::new(&app, &label, url)
        .title(format!("SmartNet // {}", title))
        .on_navigation(dapp_nav_guard(app.clone(), address.clone()))
        .initialization_script(dapp::crypto_polyfill_script())
        .initialization_script(dapp::dapp_bridge_script(&address))
        .initialization_script(dapp::dapp_i18n_script())
        .inner_size(1100.0, 720.0)
        .position(60.0, 50.0)
        // на весь экран и НА ПЕРЕДНИЙ ПЛАН (иначе окно пряталось за основным)
        .maximized(true)
        .focused(true)
        .build()
        .map_err(|e| e.to_string())?;
    let _ = win.show();
    let _ = win.unminimize();
    let _ = win.set_focus();
    // v1.135.84: сразу сворачиваем основное окно, чтобы dApp вышел на
    // передний план (пользователь: "основное приложение свернуть программно").
    if let Some(main) = app.get_window("main") {
        let _ = main.minimize();
    }
    Ok(())
}

/// Открыть dApp в НОВОМ отдельном окне даже если такой dApp уже открыт
/// (уникальный label с наносекундной меткой). Используется когда внутри
/// dApp кликают <a target="_blank"> на свою же страницу / относительный
/// путь — пользователь ожидает именно новое окно, а не навигацию в текущем.
#[tauri::command]
pub(crate) async fn open_app_webview_new(
    app: tauri::AppHandle,
    address: String,
    title: String,
    path: Option<String>,
) -> Result<(), String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let label = format!("app-{}-{}", sanitize(&address), stamp);
    let sub = normalize_dapp_path(path.as_deref()).unwrap_or_default();
    let raw_url = sth_url(&address, &sub);
    let url = WebviewUrl::External(
        raw_url
            .parse()
            .unwrap_or_else(|_| "about:blank".parse().unwrap()),
    );
    let win = WebviewWindowBuilder::new(&app, &label, url)
        .title(format!("SmartNet // {}", title))
        .on_navigation(dapp_nav_guard(app.clone(), address.clone()))
        .initialization_script(dapp::crypto_polyfill_script())
        .initialization_script(dapp::dapp_bridge_script(&address))
        .initialization_script(dapp::dapp_i18n_script())
        .inner_size(1100.0, 720.0)
        .position(80.0, 70.0)
        // на весь экран и на передний план (см. open_app_webview)
        .maximized(true)
        .focused(true)
        .build()
        .map_err(|e| e.to_string())?;
    let _ = win.show();
    let _ = win.unminimize();
    let _ = win.set_focus();
    // v1.135.84: сворачиваем основное окно, чтобы новое окно dApp
    // не оказалось спрятанным за основным.
    if let Some(main) = app.get_window("main") {
        let _ = main.minimize();
    }
    Ok(())
}

/// Open a File Exchange file (`f://<cid>`) in its own native child window. Using
/// the registered custom scheme (NOT `window.open`, which the webview blocks as a
/// local-resource / mangles to `file:///`). Viewable types (html/img/pdf/video/
/// text) render inline; the MIME is set by the `f` scheme handler.
#[tauri::command]
pub(crate) async fn open_file_webview(
    app: tauri::AppHandle,
    cid: String,
    name: String,
) -> Result<(), String> {
    let label = format!("file-{}", sanitize(&cid));
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.set_focus();
        return Ok(());
    }
    let url = WebviewUrl::External(
        format!("f://{}/", cid)
            .parse()
            .map_err(|_| "bad f:// url".to_string())?,
    );
    WebviewWindowBuilder::new(&app, &label, url)
        .title(format!("SmartNet // {}", if name.is_empty() { &cid } else { &name }))
        .inner_size(980.0, 680.0)
        .position(80.0, 60.0)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ============================================================
//  dApp Provider Bridge (Web3 — `window.smartholdem`)
//
//  An initialization script (below) injects a `window.smartholdem` provider
//  into every embedded dApp webview. The dApp calls e.g. sendTransaction(),
//  which invokes `dapp_provider_request` here. Approval-gated methods emit
//  `incoming-tx-request` to the host shell (MetaMask-style); the shell's
//  Approve/Reject path (sign_and_broadcast_tx / reject_tx_request) then emits
//  `dapp-response` back, which the injected bridge resolves to the dApp Promise.
// ============================================================

/// Injected into each dApp webview BEFORE its page loads.
///
/// Remote dApp origins (sth:// or external URLs) are NOT permitted to invoke
/// backend commands in Tauri v2 — only the event channel is granted to them
/// (see capabilities/dapp.json). So the bridge talks to the host purely over

/// v1.135.17 (Linux/WebKitGTK): tauri-runtime-wry кладёт child-webview из
/// `Window::add_child` в вертикальный GtkBox окна через pack_start(expand=true,
/// fill=true) — main-webview и dApp делят окно 50/50 (dApp «под приложением»),
/// а wry применяет set_position/set_size ТОЛЬКО к GtkFixed/x11-родителю, для
/// GtkBox это no-op. Лечим на своей стороне: при первом вызове переносим ОБА
/// webview в единый GtkFixed (ровно тот контейнер, который wry поддерживает):
///     vbox → GtkFixed { main-webview (0,0, весь размер), dApp-webview'ы }
/// main растягивается на всё окно через signal size-allocate самого GtkFixed,
/// dApp'ы позиционируются жёстко (fixed.move_ + set_size_request) и держатся
/// ПОВЕРХ main подъёмом их GdkWindow (raise). Схема с GtkOverlay (v1.135.16)
/// не годится: overlay создаёт обёрточное GdkWindow под overlay-ребёнком на
/// всю область и оно проглатывало ввод — клики не доходили ни до dApp, ни до
/// main. В плоском GtkFixed события маршрутизируются напрямую по геометрии
/// GdkWindow каждого webview.
#[cfg(target_os = "linux")]
pub(crate) fn linux_embed_bounds(wv: &tauri::webview::Webview, x: f64, y: f64, w: f64, h: f64) {
    let _ = wv.with_webview(move |pw| {
        use gtk::prelude::*;
        let xi = x.round() as i32;
        let yi = y.round() as i32;
        let wi = w.round().max(1.0) as i32;
        let hi = h.round().max(1.0) as i32;
        let widget: gtk::Widget = pw.inner().upcast();
        let Some(parent) = widget.parent() else { return };
        // Уже в нашем GtkFixed — двигаем/растягиваем и держим поверх main.
        if let Some(fixed) = parent.downcast_ref::<gtk::Fixed>() {
            fixed.move_(&widget, xi, yi);
            widget.set_size_request(wi, hi);
            if widget.is_visible() {
                // немедленная аллокация — как делает wry для fixed-родителя
                widget.size_allocate(&gtk::Allocation::new(xi, yi, wi, hi));
            }
            if let Some(win) = widget.window() {
                win.raise();
            }
            return;
        }
        // Первый вызов для этого webview: он ещё в GtkBox окна.
        let Ok(vbox) = parent.downcast::<gtk::Box>() else { return };
        // Общий GtkFixed уже построен раньше?
        let fixed = vbox
            .children()
            .into_iter()
            .find_map(|c| c.downcast::<gtk::Fixed>().ok());
        let fixed = match fixed {
            Some(f) => f,
            None => {
                // main-webview (WindowContent) — другой ребёнок vbox.
                let main = vbox.children().into_iter().find(|c| c != &widget);
                let fx = gtk::Fixed::new();
                vbox.pack_start(&fx, true, true, 0);
                fx.show();
                if let Some(main) = main {
                    // Rust-обёртка держит strong ref — remove() его не уничтожит
                    vbox.remove(&main);
                    fx.put(&main, 0, 0);
                    // main всегда на весь клиентский размер окна
                    let mw = main.clone();
                    fx.connect_size_allocate(move |_, a| {
                        mw.set_size_request(a.width().max(1), a.height().max(1));
                    });
                    let a = vbox.allocation();
                    main.set_size_request(a.width().max(1), a.height().max(1));
                    main.show();
                }
                fx
            }
        };
        let was_visible = widget.is_visible();
        vbox.remove(&widget);
        fixed.put(&widget, xi, yi);
        widget.set_size_request(wi, hi);
        if was_visible {
            widget.show();
        }
        if let Some(win) = widget.window() {
            win.raise();
        }
        crate::syslog::log(
            "sys",
            format!("embed reparent GtkBox → GtkFixed [{},{} {}x{}]", xi, yi, wi, hi),
        );
    });
}

/// JS injected into a dApp webview to force full grayscale (E-Ink theme). A
/// !important style on <html> survives most app re-renders; a 1s re-apply guards
/// against frameworks that wipe the head.
fn grayscale_script() -> String {
    r#"(function(){var CSS='filter:grayscale(1) !important;-webkit-filter:grayscale(1) !important;';function apply(doc){try{if(!doc||doc.getElementById('__sth_gray'))return;var s=doc.createElement('style');s.id='__sth_gray';s.textContent='html{'+CSS+'}';(doc.head||doc.documentElement).appendChild(s);}catch(e){}}function sweep(){apply(document);var f=document.querySelectorAll('iframe,frame');for(var i=0;i<f.length;i++){try{apply(f[i].contentDocument);}catch(e){}}}sweep();document.addEventListener('DOMContentLoaded',sweep);setInterval(sweep,1000);try{new MutationObserver(sweep).observe(document.documentElement,{childList:true,subtree:true});}catch(e){}})();"#.to_string()
}

#[tauri::command]
pub(crate) async fn open_embedded_webview(
    app: tauri::AppHandle,
    address: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    visible: Option<bool>,
    grayscale: Option<bool>,
    path: Option<String>,
    fresh: Option<bool>,
) -> Result<(), String> {
    let label = format!("embed-{}", sanitize(&address));
    if let Some(wv) = app.get_webview(&label) {
        // v1.135.16 (Linux): bounds применяем через GTK напрямую (GtkFixed) —
        // wry игнорирует set_position/set_size для child в GtkBox (см.
        // linux_embed_bounds). hide/show-workaround v1.135.10 больше не нужен.
        #[cfg(target_os = "linux")]
        linux_embed_bounds(&wv, x, y, width, height);
        #[cfg(not(target_os = "linux"))]
        {
            let _ = wv.set_position(LogicalPosition::new(x, y));
            let _ = wv.set_size(LogicalSize::new(width, height));
        }
        // v1.63.6: если пришёл path — навигируем существующий webview на
        // внутренний маршрут dApp. Без этого делиться прямыми ссылками
        // (sth://<addr>/trading/…) было невозможно — вкладка всегда
        // возвращалась к предыдущему location.
        // v1.90.11: fresh=true — фронтенд считает, что webview НЕ существует
        // (свежая установка / переустановка dApp), а он остался с прошлой
        // сессии вкладки. Перезагружаем на корень, иначе пользователь видит
        // устаревшую страницу (в т.ч. закешированный 404 до установки).
        let sub = normalize_dapp_path(path.as_deref());
        if fresh == Some(true) || sub.is_some() {
            let target = sth_url(&address, sub.as_deref().unwrap_or(""));
            let js = format!(
                "try {{ window.location.assign({}); }} catch (e) {{ /* ignore */ }}",
                serde_json::to_string(&target).unwrap_or_else(|_| "\"about:blank\"".to_string())
            );
            let _ = wv.eval(&js);
        }
        let _ = wv.show();
        let _ = wv.set_focus();
        return Ok(());
    }
    let window = app.get_window("main").ok_or("main window missing")?;
    let sub = normalize_dapp_path(path.as_deref()).unwrap_or_default();
    let raw_url = sth_url(&address, &sub);
    let url = raw_url
        .parse()
        .map_err(|_| "bad sth url".to_string())?;
    let mut builder = WebviewBuilder::new(&label, WebviewUrl::External(url))
        .on_navigation(dapp_nav_guard(app.clone(), address.clone()))
        .initialization_script(dapp::crypto_polyfill_script())
        .initialization_script(dapp::dapp_bridge_script(&address))
        .initialization_script(dapp::dapp_i18n_script());
    if grayscale == Some(true) {
        builder = builder.initialization_script(&grayscale_script());
    }
    let _created = window
        .add_child(
            builder,
            LogicalPosition::new(x, y),
            LogicalSize::new(width, height),
        )
        .map_err(|e| e.to_string())?;
    // Linux: немедленно уносим child из GtkBox в GtkOverlay+GtkFixed с
    // реальными bounds — иначе он делит окно 50/50 с main-webview.
    #[cfg(target_os = "linux")]
    linux_embed_bounds(&_created, x, y, width, height);
    // v1.135.189: внутренний путь dApp (SPA-роутинг) → `dapp-url{tab:address,url}`
    // → адресная строка показывает sth://name/<path>.
    crate::websites::install_url_watch(&_created, app.clone(), address.clone(), "dapp-url");
    // Create hidden until the dApp page signals it has loaded (`dapp-loaded`),
    // so the host shows its own "loading from the decentralized network" overlay
    // instead of a blank/black native webview.
    if visible == Some(false) {
        if let Some(wv) = app.get_webview(&label) {
            let _ = wv.hide();
        }
    }
    Ok(())
}

/// Построить sth:// URL dApp c маркером `?sthapp=<id>` — резервный носитель
/// app id на случай, если платформенный WebView отбросит/исказит authority
/// custom-схемы (protocol.rs читает его из query и из Referer).
fn sth_url(address: &str, sub: &str) -> String {
    let sub = sub.trim_start_matches('/');
    let (main, frag) = match sub.split_once('#') {
        Some((m, f)) => (m, Some(f)),
        None => (sub, None),
    };
    let sep = if main.contains('?') { '&' } else { '?' };
    match frag {
        Some(f) => format!("sth://{address}/{main}{sep}sthapp={address}#{f}"),
        None => format!("sth://{address}/{main}{sep}sthapp={address}"),
    }
}

/// Нормализовать deep-link path для sth:// URL. Обрезаем ведущий `/`,
/// отсекаем "путь-в-корень" ("" и "/") — они не требуют навигации,
/// сохраняем query/hash как есть. Защита от protocol-injection: если
/// в path есть `://` или ведущий "//host" — сбрасываем в None.
fn normalize_dapp_path(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim();
    if s.is_empty() || s == "/" {
        return None;
    }
    // Protocol-injection guard: "//attacker.com/foo" или "http://…" внутри
    // path переопределили бы authority.
    let trimmed = s.trim_start_matches('/');
    if trimmed.contains("://") || trimmed.starts_with("//") {
        return None;
    }
    Some(trimmed.to_string())
}

#[tauri::command]
// Все команды ниже async: sync-команды Tauri исполняются на главном потоке,
// и оконные/webview-вызовы из IPC-обработчика могут дедлочить event loop
// (см. фикс open_in_main v1.135.48). async уводит их в пул потоков.
pub(crate) async fn set_embedded_bounds(
    app: tauri::AppHandle,
    address: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let label = format!("embed-{}", sanitize(&address));
    if let Some(wv) = app.get_webview(&label) {
        // v1.135.16 (Linux/WebKitGTK): wry применяет set_position/set_size
        // только к GtkFixed/x11-родителю; child из add_child живёт в GtkBox,
        // где это no-op. Репарентим в GtkFixed и двигаем через GTK напрямую
        // (см. linux_embed_bounds) — hide/show-workaround v1.135.10 снят.
        #[cfg(target_os = "linux")]
        linux_embed_bounds(&wv, x, y, width, height);
        #[cfg(not(target_os = "linux"))]
        {
            let _ = wv.set_position(LogicalPosition::new(x, y));
            let _ = wv.set_size(LogicalSize::new(width, height));
        }
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn hide_embedded_webview(app: tauri::AppHandle, address: String) -> Result<(), String> {
    let label = format!("embed-{}", sanitize(&address));
    if let Some(wv) = app.get_webview(&label) {
        let _ = wv.hide();
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn close_embedded_webview(app: tauri::AppHandle, address: String) -> Result<(), String> {
    let label = format!("embed-{}", sanitize(&address));
    if let Some(wv) = app.get_webview(&label) {
        let _ = wv.close();
    }
    Ok(())
}

/// Close every embedded dApp child webview (label prefix `embed-`). Called on
/// app boot/reload: a main-window F5 wipes the Vue tab state but leaves the
/// native child webviews floating over the reloaded shell — this reconciles
/// them away so the dashboard + controls are never covered by an orphan.
#[tauri::command]
pub(crate) async fn close_all_embedded_webviews(app: tauri::AppHandle) -> Result<(), String> {
    for (label, wv) in app.webviews() {
        if label.starts_with("embed-") {
            let _ = wv.close();
        }
    }
    Ok(())
}


/// Включить/выключить перевод страницы DApp: eval-им управляющий вызов в
/// content-скрипт webview (встроенный `embed-<addr>` или отдельное окно `app-<addr>`).
#[tauri::command]
pub(crate) async fn dapp_translate_toggle(
    app: tauri::AppHandle,
    address: String,
    on: bool,
    target: String,
) -> Result<(), String> {
    let s = sanitize(&address);
    let tgt = serde_json::to_string(&target).unwrap_or_else(|_| "\"en\"".to_string());
    let js = format!("window.__sthI18n && window.__sthI18n.set({}, {});", on, tgt);
    if let Some(wv) = app.get_webview(&format!("embed-{}", s)) {
        wv.eval(&js).map_err(|e| e.to_string())?;
        return Ok(());
    }
    if let Some(win) = app.get_webview_window(&format!("app-{}", s)) {
        win.eval(&js).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Настроить перевод DApp (язык + перевод по выделению). Вызывается из главного
/// окна при загрузке DApp (только для GPU-сборок).
#[tauri::command]
pub(crate) async fn dapp_translate_config(
    app: tauri::AppHandle,
    address: String,
    target: String,
    selection: bool,
    label: String,
) -> Result<(), String> {
    let s = sanitize(&address);
    let tgt = serde_json::to_string(&target).unwrap_or_else(|_| "\"en\"".to_string());
    let lbl = serde_json::to_string(&label).unwrap_or_else(|_| "\"Translate selection\"".to_string());
    let js = format!(
        "window.__sthI18n && window.__sthI18n.config({}, {}, {});",
        tgt, selection, lbl
    );
    if let Some(wv) = app.get_webview(&format!("embed-{}", s)) {
        wv.eval(&js).map_err(|e| e.to_string())?;
        return Ok(());
    }
    if let Some(win) = app.get_webview_window(&format!("app-{}", s)) {
        win.eval(&js).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Browser-like navigation controls for the address bar: back / forward /
/// reload. Действуют внутри native child webview'а (embed- или app-*),
/// т.е. листают ИМЕННО историю самого dApp (SPA route stack), а не
/// историю host-shell'а. Тихо no-op, если dApp с таким адресом не открыт.
#[tauri::command]
pub(crate) async fn dapp_history_back(app: tauri::AppHandle, address: String) -> Result<(), String> {
    let s = sanitize(&address);
    if let Some(wv) = app.get_webview(&format!("embed-{}", s)) {
        wv.eval("try{history.back()}catch(e){}").map_err(|e| e.to_string())?;
        return Ok(());
    }
    if let Some(win) = app.get_webview_window(&format!("app-{}", s)) {
        win.eval("try{history.back()}catch(e){}").map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn dapp_history_forward(app: tauri::AppHandle, address: String) -> Result<(), String> {
    let s = sanitize(&address);
    if let Some(wv) = app.get_webview(&format!("embed-{}", s)) {
        wv.eval("try{history.forward()}catch(e){}").map_err(|e| e.to_string())?;
        return Ok(());
    }
    if let Some(win) = app.get_webview_window(&format!("app-{}", s)) {
        win.eval("try{history.forward()}catch(e){}").map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn dapp_reload(app: tauri::AppHandle, address: String) -> Result<(), String> {
    let s = sanitize(&address);
    if let Some(wv) = app.get_webview(&format!("embed-{}", s)) {
        wv.eval("try{location.reload()}catch(e){}").map_err(|e| e.to_string())?;
        return Ok(());
    }
    if let Some(win) = app.get_webview_window(&format!("app-{}", s)) {
        win.eval("try{location.reload()}catch(e){}").map_err(|e| e.to_string())?;
    }
    Ok(())
}

