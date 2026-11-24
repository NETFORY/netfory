// ============================================================
//  SmartNet // websites.rs
//
//  Обычные https://-сайты во вкладках NETFORY (v1.135.191). Тот же механизм,
//  что у dApp (нативный child-webview поверх tab-host), но с ДРУГИМ набором
//  правил:
//    • label `web-<tab>` — НЕ совпадает с `embed-*`/`app-*`, поэтому
//      capabilities/dapp.json (event-канал Tauri) на сайты не распространяется;
//    • НИКАКОГО моста к Tauri в initialization_script: ни window.smartholdem,
//      ни polyfill'ов — произвольный сайт не должен видеть мост к кошельку.
//      Единственный скрипт — LINK_SCRIPT (v1.135.226): target=_blank /
//      window.open / Ctrl+клик → `ntfry-tab://open?u=…` → новая вкладка;
//    • навигация только https:// (http:// блокируется → фронт переоткрывает
//      как https), кастомные схемы SmartNet уходят host-shell'у событием
//      `site-open-link`, всё остальное (file:, javascript:, …) — блок;
//    • window.open / target=_blank → событие `site-open-link{new_window}` →
//      новая вкладка NETFORY, а не отдельное окно;
//    • URL/заголовок/индикатор загрузки → события `site-nav` / `site-title`.
//
//  Author: TechnoL0g
//  Created: 2026-06-27
// ============================================================

use crate::sanitize;
use tauri::webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder};
use tauri::Manager;
use tauri::{LogicalPosition, LogicalSize, WebviewUrl};

fn label(tab: &str) -> String {
    format!("web-{}", sanitize(tab))
}

/// Последний https-URL, на который переходила web-вкладка (tab → url). Нужен
/// для собственной страницы ошибки: WebView2 в NavigationCompleted не отдаёт
/// URL без COM-строк, а nav_guard видит каждый переход (в т.ч. редиректы).
static LAST_URL: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn remember_url(tab: &str, url: &tauri::Url) {
    if url.scheme() == "https" {
        if let Ok(mut g) = LAST_URL.lock() {
            g.insert(tab.to_string(), (url.to_string(), std::time::Instant::now()));
        }
    }
}

/// Роутер (webproxy) не смог напрямую подключиться к bundle-хосту и перевёл группу на
/// relay. Если этот хост — главный документ вкладки, начавшей навигацию < 8 с назад,
/// сразу показываем HUD туннеля (не зависим от того, как движок сообщит об ошибке).
pub(crate) fn on_bundle_direct_failed(app: &tauri::AppHandle, host: &str) {
    let host = host.to_ascii_lowercase();
    let tabs: Vec<(String, String)> = LAST_URL
        .lock()
        .map(|g| {
            g.iter()
                .filter(|(_, (u, at))| at.elapsed() < std::time::Duration::from_secs(8) && host_of(u).to_ascii_lowercase() == host)
                .map(|(t, (u, _))| (t.clone(), u.clone()))
                .collect()
        })
        .unwrap_or_default();
    for (tab, url) in tabs {
        let page = failure_page_url(&tab, &url, "ERR_DIRECT_BLOCKED", t3("Прямое соединение с сайтом не удалось.", "Direct connection to the site failed.", "Koneksi langsung ke situs gagal."));
        if let Ok(target) = page.parse::<tauri::Url>() {
            if let Some(w) = app.get_webview(&label(&tab)) {
                let _ = w.navigate(target);
            }
        }
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Общий cyberpunk-стиль служебных страниц «NETFORY Browser» (ошибка / HUD туннеля).
const PAGE_CSS: &str = r##":root{--bg:#07090d;--panel:#0d1117;--line:#1b2430;--accent:#17e9c7;--accent2:#ff3d81;--warn:#f5b342;--dim:#5c7894;--text:#dbe7f3}
*{box-sizing:border-box;margin:0}
html,body{height:100%}
body{background:var(--bg);color:var(--text);font-family:"JetBrains Mono",Consolas,ui-monospace,monospace;display:flex;align-items:center;justify-content:center;overflow:hidden;position:relative}
.grid{position:fixed;inset:0;background-image:linear-gradient(rgba(23,233,199,.06) 1px,transparent 1px),linear-gradient(90deg,rgba(23,233,199,.06) 1px,transparent 1px);background-size:36px 36px;mask-image:radial-gradient(ellipse at center,#000 30%,transparent 80%)}
.scan{position:fixed;inset:0;background:repeating-linear-gradient(0deg,transparent 0 2px,rgba(0,0,0,.18) 2px 4px);pointer-events:none;opacity:.6}
.card{position:relative;z-index:1;width:min(640px,92vw);padding:34px 38px 30px;background:linear-gradient(180deg,rgba(13,17,23,.96),rgba(9,12,17,.96));border:1px solid var(--line);box-shadow:0 0 0 1px rgba(23,233,199,.12),0 30px 80px rgba(0,0,0,.65);clip-path:polygon(0 0,calc(100% - 22px) 0,100% 22px,100% 100%,22px 100%,0 calc(100% - 22px))}
.brand{display:flex;align-items:center;gap:10px;font-size:11px;letter-spacing:.24em;text-transform:uppercase;color:var(--accent)}
.brand i{width:8px;height:8px;background:var(--accent);box-shadow:0 0 12px var(--accent);display:inline-block;animation:blink 1.6s infinite}
@keyframes blink{0%,100%{opacity:1}50%{opacity:.25}}
h1{margin-top:22px;font-size:26px;line-height:1.15;letter-spacing:.02em}
h1 b{color:var(--accent2);text-shadow:0 0 18px rgba(255,61,129,.45)}
h1 b.ok{color:var(--accent);text-shadow:0 0 18px rgba(23,233,199,.45)}
.host{margin-top:12px;font-size:14px;color:var(--text);word-break:break-all}
.host span{color:var(--dim)}
.why{margin-top:18px;padding:12px 14px;border-left:2px solid var(--accent2);background:rgba(255,61,129,.06);font-size:12px;color:#c9d4e0;line-height:1.5}
.code{margin-top:14px;font-size:10px;letter-spacing:.18em;color:var(--dim);text-transform:uppercase}
.acts{margin-top:26px;display:flex;gap:12px;flex-wrap:wrap}
button{font:inherit;font-size:12px;letter-spacing:.14em;text-transform:uppercase;padding:11px 20px;border:1px solid var(--accent);background:var(--accent);color:#04110f;cursor:pointer;font-weight:800;transition:transform .12s,box-shadow .12s,background-color .12s}
button:hover{transform:translateY(-1px);box-shadow:0 0 22px rgba(23,233,199,.45)}
button.ghost{background:transparent;color:var(--accent)}
.foot{margin-top:26px;font-size:10px;letter-spacing:.16em;color:var(--dim);text-transform:uppercase;display:flex;justify-content:space-between}
.log{margin-top:20px;padding:14px 16px;background:rgba(0,0,0,.35);border:1px solid var(--line);font-size:12px;line-height:1.7}
.ln{opacity:0;transform:translateX(-6px);animation:in .35s forwards}
.ln.warn{color:var(--warn)}
.ln.ok{color:var(--accent);animation-delay:.45s}
.ln.dim{color:var(--dim);animation-delay:.9s}
.ln b{font-weight:800}
@keyframes in{to{opacity:1;transform:none}}
.bar{margin-top:18px;height:3px;background:rgba(23,233,199,.12);overflow:hidden;position:relative}
.bar i{position:absolute;inset:0;width:40%;background:linear-gradient(90deg,transparent,var(--accent),transparent);box-shadow:0 0 14px var(--accent);animation:sweep 1.1s linear infinite}
@keyframes sweep{from{transform:translateX(-100%)}to{transform:translateX(250%)}}
.glitch{position:relative;display:inline-block}
.glitch::after{content:attr(data-t);position:absolute;left:2px;top:0;color:var(--accent2);clip-path:inset(40% 0 30% 0);opacity:.6;animation:gl 2.2s steps(2) infinite}
@keyframes gl{0%,88%,100%{transform:none;opacity:0}90%{transform:translate(-3px,1px);opacity:.7}94%{transform:translate(3px,-1px);opacity:.5}}"##;

fn data_url(html: &str) -> String {
    use base64::Engine;
    format!("data:text/html;base64,{}", base64::engine::general_purpose::STANDARD.encode(html.as_bytes()))
}

fn host_of(url: &str) -> String {
    url.parse::<tauri::Url>().ok().and_then(|u| u.host_str().map(|h| h.to_string())).unwrap_or_default()
}

// ---- язык страниц WebView: 0 = ru, 1 = en, 2 = id (UI сообщает через `ui_set_locale`) ----
static UI_LANG: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

/// Язык интерфейса для страниц ошибок / HUD / меню ссылок (неизвестный → en).
#[tauri::command]
pub(crate) async fn ui_set_locale(lang: String) {
    let v = match lang.get(..2).unwrap_or("").to_ascii_lowercase().as_str() {
        "ru" => 0,
        "id" => 2,
        _ => 1,
    };
    UI_LANG.store(v, std::sync::atomic::Ordering::Relaxed);
}

fn t3(ru: &'static str, en: &'static str, id: &'static str) -> &'static str {
    match UI_LANG.load(std::sync::atomic::Ordering::Relaxed) {
        0 => ru,
        2 => id,
        _ => en,
    }
}

fn lang_code() -> &'static str {
    t3("ru", "en", "id")
}

/// Собственная страница ошибки «NETFORY Browser» (cyberpunk) вместо стандартной
/// Edge/WebKit. Загружается как data:-URL; кнопка «ОБНОВИТЬ» и toolbar-reload
/// (`site_reload`) повторяют переход на исходный https-URL (`window.__nfRetry`).
fn error_page_url(url: &str, code: &str, reason: &str) -> String {
    let host = host_of(url);
    let orig_js = serde_json::to_string(url).unwrap_or_else(|_| "\"about:blank\"".into());
    let html = format!(
        r##"<!doctype html><html lang="{lang}"><head><meta charset="utf-8"><title>{host}</title>
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>{css}</style></head><body>
<div class="grid"></div><div class="scan"></div>
<main class="card">
  <div class="brand"><i></i>NETFORY Browser</div>
  <h1>{title}</h1>
  <div class="host"><span>https://</span>{host}</div>
  <div class="why">{reason}</div>
  <div class="code">{code}</div>
  <div class="acts">
    <button onclick="window.__nfRetry()">⟳ {reload}</button>
    <button class="ghost" onclick="history.back()">← {back}</button>
  </div>
  <div class="foot"><span>WEB4 · P2P NETWORK</span><span>SmartHoldem</span></div>
</main>
<script>window.__nfRetry=function(){{location.replace({orig_js})}};</script>
</body></html>"##,
        css = PAGE_CSS,
        lang = lang_code(),
        title = t3("Сайт <b>недоступен</b>", "Site <b>unavailable</b>", "Situs <b>tidak tersedia</b>"),
        reload = t3("Обновить", "Reload", "Muat ulang"),
        back = t3("Назад", "Back", "Kembali"),
        host = html_escape(&host),
        reason = html_escape(reason),
        code = html_escape(code),
        orig_js = orig_js,
    );
    data_url(&html)
}

/// Mycelium-режим: цепочки ещё нет (или сайт не ответил через неё) — свой IP не светим,
/// показываем ожидание и сами повторяем переход.
fn mycelium_page_url(url: &str, ready: bool) -> String {
    let host = host_of(url);
    let orig_js = serde_json::to_string(url).unwrap_or_else(|_| "\"about:blank\"".into());
    let (title, why, delay) = if ready {
        (
            t3("Mycelium <b>переподключается</b>", "Mycelium <b>is reconnecting</b>", "Mycelium <b>menyambung ulang</b>"),
            t3(
                "Сайт не ответил через mushroom-цепочку. Прямое соединение не используется — ваш IP не раскрывается. Пробуем другой выход…",
                "The site did not respond through the mushroom circuit. No direct connection is used — your IP stays hidden. Trying another exit…",
                "Situs tidak merespons melalui rantai mushroom. Koneksi langsung tidak digunakan — IP Anda tetap tersembunyi. Mencoba jalur keluar lain…",
            ),
            6000,
        )
    } else {
        (
            t3("Mycelium <b>строит цепочку…</b>", "Mycelium <b>is building a circuit…</b>", "Mycelium <b>membangun rantai…</b>"),
            t3(
                "Ищем релеи и собираем mushroom-цепочку. Пока её нет, сайт не открывается напрямую — ваш IP не раскрывается.",
                "Looking for relays and building a mushroom circuit. Until it is ready the site is not opened directly — your IP stays hidden.",
                "Mencari relay dan membangun rantai mushroom. Sampai siap, situs tidak dibuka langsung — IP Anda tetap tersembunyi.",
            ),
            3500,
        )
    };
    let html = format!(
        r##"<!doctype html><html lang="{lang}"><head><meta charset="utf-8"><title>{host}</title>
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>{css}</style></head><body>
<div class="grid"></div><div class="scan"></div>
<main class="card">
  <div class="brand"><i></i>NETFORY Browser · Mycelium</div>
  <h1>{title}</h1>
  <div class="host"><span>https://</span>{host}</div>
  <div class="why">{why}</div>
  <div class="code">MYCELIUM · AUTO-RETRY</div>
  <div class="acts">
    <button onclick="window.__nfRetry()">⟳ {retry}</button>
    <button class="ghost" onclick="history.back()">← {back}</button>
  </div>
  <div class="foot"><span>MUSHROOM ROUTING</span><span>SmartHoldem</span></div>
</main>
<script>window.__nfRetry=function(){{location.replace({orig_js})}};setTimeout(window.__nfRetry,{delay});</script>
</body></html>"##,
        css = PAGE_CSS,
        lang = lang_code(),
        retry = t3("Повторить", "Retry", "Coba lagi"),
        back = t3("Назад", "Back", "Kembali"),
        host = html_escape(&host),
        orig_js = orig_js,
    );
    data_url(&html)
}

#[cfg(feature = "p2p")]
fn mycelium_ready() -> bool {
    crate::mycel::instance().is_some_and(|m| m.is_enabled() && m.status().circuits_ready > 0)
}
#[cfg(not(feature = "p2p"))]
fn mycelium_ready() -> bool {
    false
}

/// HUD туннеля (v1.135.191): прямой путь к bundle-хосту ограничен, роутер уже
/// перевёл группу на pq-web — показываем «Routing via NETFORY Relay…» и через
/// ~900 мс повторяем исходный переход (теперь он пойдёт через туннель).
fn hud_page_url(url: &str, hud: &crate::webproxy::Hud, attempt: u32) -> String {
    let host = host_of(url);
    let orig_js = serde_json::to_string(url).unwrap_or_else(|_| "\"about:blank\"".into());
    let node_short = if hud.node.len() > 14 { format!("{}…{}", &hud.node[..8], &hud.node[hud.node.len() - 4..]) } else { hud.node.clone() };
    let relay_label = if hud.region.is_empty() { format!("NETFORY Relay {node_short}") } else { format!("NETFORY Relay {} · {node_short}", hud.region) };
    let rtt = if hud.rtt_ms > 0 { format!(" [{} ms]", hud.rtt_ms) } else { String::new() };
    // attempt > 0: холодное соединение с узлом ещё устанавливается (реле, UDP заблокирован)
    let (title_b, ok_line, delay_ms) = if attempt > 0 {
        (
            t3("соединяется", "connecting", "menghubungkan"),
            format!("[..] Connecting to <b>{}</b> via relay (UDP blocked by ISP) — attempt {attempt}/3…", html_escape(&relay_label)),
            3000u32,
        )
    } else {
        (t3("переключается", "switching", "beralih"), format!("[OK] Routing via <b>{}</b>{}…", html_escape(&relay_label), rtt), 900u32)
    };
    let html = format!(
        r##"<!doctype html><html lang="{lang}"><head><meta charset="utf-8"><title>{host}</title>
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>{css}</style></head><body>
<div class="grid"></div><div class="scan"></div>
<main class="card">
  <div class="brand"><i></i>NETFORY Browser · PQ WEB</div>
  <h1><span class="glitch" data-t="{route}">{route}</span> <b class="ok">{title_b}</b></h1>
  <div class="host"><span>https://</span>{host}</div>
  <div class="log">
    <div class="ln warn">[WARN] Direct connection to <b>{host}</b> restricted by ISP</div>
    <div class="ln ok">{ok_line}</div>
    <div class="ln dim">TLS end-to-end · relay sees only host:port · bundle {bundle}</div>
  </div>
  <div class="bar"><i></i></div>
  <div class="foot"><span>P2P TUNNEL · IROH QUIC</span><span>SmartHoldem</span></div>
</main>
<script>window.__nfRetry=function(){{location.replace({orig_js})}};setTimeout(window.__nfRetry,{delay});</script>
</body></html>"##,
        css = PAGE_CSS,
        host = html_escape(&host),
        lang = lang_code(),
        route = t3("Маршрут", "Route", "Rute"),
        title_b = title_b,
        ok_line = ok_line,
        bundle = html_escape(&hud.bundle),
        orig_js = orig_js,
        delay = delay_ms,
    );
    data_url(&html)
}

/// Вкладка → (host, первый показ HUD, число показов). Повторные сбои того же хоста
/// в течение 90 с: «relay» — 1 HUD, затем ошибка; «relay-failed» (холодный connect к узлу
/// через реле при заблокированном UDP) — до 3 HUD «Connecting…» с ретраем через 3 с.
static HUD_SHOWN: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant, u32)>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Сколько раз HUD уже показан этой вкладке для host за последние 90 с (и инкремент).
fn hud_count_inc(tab: &str, host: &str) -> u32 {
    let Ok(mut g) = HUD_SHOWN.lock() else { return 99 };
    let fresh = matches!(g.get(tab), Some((h, at, _)) if h == host && at.elapsed() < std::time::Duration::from_secs(90));
    if !fresh {
        g.insert(tab.to_string(), (host.to_string(), std::time::Instant::now(), 0));
    }
    let e = g.get_mut(tab).expect("inserted");
    e.2 += 1;
    e.2 - 1
}

/// Страница при сбое загрузки https-URL: HUD туннеля (bundle-хост, маршрут уже
/// relay, узел есть, HUD ещё не показывали) либо страница ошибки с уточнённой
/// причиной (нет узлов pq-web / туннель не ответил / обычная сетевая ошибка).
fn failure_page_url(tab: &str, url: &str, code: &str, reason: &str) -> String {
    if crate::mycel::enabled() {
        return mycelium_page_url(url, mycelium_ready());
    }
    let host = host_of(url).to_ascii_lowercase();
    if code == "ERR_INTERNET_DISCONNECTED" || !crate::webproxy::mark_blocked(&host, code) {
        return error_page_url(url, code, reason);
    }
    let Some(hud) = crate::webproxy::hud_for(&host) else { return error_page_url(url, code, reason) };
    match hud.state {
        "relay" => {
            let shown = hud_count_inc(tab, &host);
            if shown >= 1 {
                crate::syslog::log("web", format!("[{tab}] {host}: сбой после HUD туннеля ({code}) → страница ошибки"));
                return error_page_url(
                    url,
                    "PQ_TUNNEL_FAILED",
                    &t3(
                        "Прямое соединение с {host} ограничено, а туннель PQ Web через узел {node} не ответил. Проверьте узлы: Настройки → PQ Web.",
                        "Direct connection to {host} is restricted, and the PQ Web tunnel via node {node} did not respond. Check your nodes: Settings → PQ Web.",
                        "Koneksi langsung ke {host} dibatasi, dan terowongan PQ Web melalui node {node} tidak merespons. Periksa node: Pengaturan → PQ Web.",
                    )
                    .replace("{host}", &host)
                    .replace("{node}", if hud.node.is_empty() { "—" } else { hud.node.as_str() }),
                );
            }
            crate::syslog::log("web", format!("[{tab}] {host}: {code} → HUD туннеля (bundle {}, узел {} [{}], {} ms)", hud.bundle, hud.node, hud.region, hud.rtt_ms));
            hud_page_url(url, &hud, 0)
        }
        "no-nodes" => error_page_url(
            url,
            "PQ_NO_NODES",
            &t3(
                "Прямое соединение с {host} ограничено провайдером. Узлы PQ Web не настроены и не получены с трекера — добавьте их: Настройки → PQ Web.",
                "Direct connection to {host} is restricted by your ISP. No PQ Web nodes are configured or received from the tracker — add them: Settings → PQ Web.",
                "Koneksi langsung ke {host} dibatasi oleh ISP. Tidak ada node PQ Web yang diatur atau diterima dari tracker — tambahkan: Pengaturan → PQ Web.",
            )
            .replace("{host}", &host),
        ),
        _ => {
            // узел не ответил на первый CONNECT — обычно холодное QUIC-соединение через реле
            // (UDP заблокирован): даём до 3 попыток с HUD «Connecting…», затем честная ошибка
            let shown = hud_count_inc(tab, &host);
            if shown < 3 {
                crate::syslog::log("web", format!("[{tab}] {host}: туннель не ответил → HUD «connecting», попытка {}/3", shown + 1));
                return hud_page_url(url, &hud, shown + 1);
            }
            error_page_url(
                url,
                "PQ_TUNNEL_FAILED",
                &t3(
                    "Прямое соединение с {host} ограничено, туннель PQ Web не ответил после 3 попыток (узлы недоступны). Повторите позже или проверьте узлы: Настройки → PQ Web.",
                    "Direct connection to {host} is restricted, and the PQ Web tunnel did not respond after 3 attempts (nodes unavailable). Try again later or check your nodes: Settings → PQ Web.",
                    "Koneksi langsung ke {host} dibatasi, terowongan PQ Web tidak merespons setelah 3 percobaan (node tidak tersedia). Coba lagi nanti atau periksa node: Pengaturan → PQ Web.",
                )
                .replace("{host}", &host),
            )
        }
    }
}

/// Windows/WebView2: подписка на NavigationCompleted — при СЕТЕВОЙ ошибке
/// (DNS/соединение/таймаут/TLS) заменяем стандартную страницу Edge на нашу.
/// HTTP-ошибки сайта (404/500 → статус UNKNOWN) и отмену (window.stop) не трогаем.
#[cfg(target_os = "windows")]
fn install_error_page(wv: &tauri::webview::Webview, app: tauri::AppHandle, tab: String) {
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::NavigationCompletedEventHandler;
    let lbl = label(&tab);
    let _ = wv.with_webview(move |pw| unsafe {
        let Ok(core) = pw.controller().CoreWebView2() else {
            return;
        };
        install_script_dialogs(&core, app.clone(), tab.clone());
        let mut token = 0i64;
        let _ = core.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |_sender, args| {
                let Some(args) = args else { return Ok(()) };
                let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                args.WebErrorStatus(&mut status)?;
                let (code, reason) = match status {
                    COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN
                    | COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED
                    | COREWEBVIEW2_WEB_ERROR_STATUS_VALID_AUTHENTICATION_CREDENTIALS_REQUIRED
                    | COREWEBVIEW2_WEB_ERROR_STATUS_VALID_PROXY_AUTHENTICATION_REQUIRED => {
                        // успешная загрузка (в т.ч. нашей data:-страницы) — вернуть webview на экран
                        let was_hidden = HIDDEN_FOR_ERROR.lock().map(|mut g| g.remove(&tab)).unwrap_or(false);
                        if was_hidden {
                            if let Some(w) = app.get_webview(&lbl) {
                                let _ = w.show();
                            }
                        }
                        return Ok(());
                    }
                    COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED => ("ERR_NAME_NOT_RESOLVED", t3("Не удалось найти IP-адрес сервера. Проверьте адрес сайта, подключение к интернету и настройки DNS.", "Could not find the server's IP address. Check the site address, your internet connection and DNS settings.", "Alamat IP server tidak ditemukan. Periksa alamat situs, koneksi internet, dan pengaturan DNS.")),
                    COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT => ("ERR_TIMED_OUT", t3("Сервер слишком долго не отвечает.", "The server is taking too long to respond.", "Server terlalu lama merespons.")),
                    COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE => ("ERR_ADDRESS_UNREACHABLE", t3("Сервер недостижим по сети.", "The server is unreachable over the network.", "Server tidak dapat dijangkau melalui jaringan.")),
                    COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT => ("ERR_CONNECTION_REFUSED", t3("Сервер отклонил соединение.", "The server refused the connection.", "Server menolak koneksi.")),
                    COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED => ("ERR_CONNECTION_ABORTED", t3("Соединение прервано.", "The connection was aborted.", "Koneksi terputus.")),
                    COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET => ("ERR_CONNECTION_RESET", t3("Соединение сброшено сервером.", "The connection was reset by the server.", "Koneksi direset oleh server.")),
                    COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED => ("ERR_INTERNET_DISCONNECTED", t3("Нет подключения к интернету.", "No internet connection.", "Tidak ada koneksi internet.")),
                    COREWEBVIEW2_WEB_ERROR_STATUS_ERROR_HTTP_INVALID_SERVER_RESPONSE => ("ERR_INVALID_RESPONSE", t3("Сервер вернул некорректный ответ.", "The server returned an invalid response.", "Server mengirim respons yang tidak valid.")),
                    COREWEBVIEW2_WEB_ERROR_STATUS_REDIRECT_FAILED => ("ERR_TOO_MANY_REDIRECTS", t3("Сбой перенаправления.", "Redirect failed.", "Pengalihan gagal.")),
                    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT
                    | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED
                    | COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS
                    | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED
                    | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID => ("ERR_CERT_INVALID", t3("Сертификат TLS сайта недействителен — соединение небезопасно.", "The site's TLS certificate is invalid — the connection is not secure.", "Sertifikat TLS situs tidak valid — koneksi tidak aman.")),
                    _ => ("ERR_FAILED", t3("Неизвестная сетевая ошибка.", "Unknown network error.", "Kesalahan jaringan tidak diketahui.")),
                };
                let url = LAST_URL.lock().ok().and_then(|g| g.get(&tab).map(|(u, _)| u.clone())).unwrap_or_default();
                if url.is_empty() {
                    return Ok(());
                }
                // защита от цикла «ошибка → наша страница → ошибка …» (браузерный процесс WebView2
                // уходил в 100% и вешал все окна): не больше 3 подмен за 10 с на вкладку
                if !error_nav_allowed(&tab) {
                    crate::syslog::log("web", format!("[{tab}] {url}: {code} — цикл страниц ошибки остановлен"));
                    if HIDDEN_FOR_ERROR.lock().map(|mut g| g.remove(&tab)).unwrap_or(false) {
                        if let Some(w) = app.get_webview(&lbl) {
                            let _ = w.show();
                        }
                    }
                    return Ok(());
                }
                crate::syslog::log("web", format!("{}: {} → страница ошибки NETFORY", url, code));
                if let Ok(target) = failure_page_url(&tab, &url, code, reason).parse::<tauri::Url>() {
                    if let Some(w) = app.get_webview(&lbl) {
                        // спрятать, пока грузится наша страница — без мигания заглушки Edge
                        let _ = w.hide();
                        if let Ok(mut g) = HIDDEN_FOR_ERROR.lock() {
                            g.insert(tab.clone());
                        }
                        let _ = w.navigate(target);
                    }
                }
                Ok(())
            })),
            &mut token,
        );
    });
}

/// Не больше 3 подмен на страницу ошибки за 10 с на вкладку.
#[cfg(target_os = "windows")]
fn error_nav_allowed(tab: &str) -> bool {
    static HIST: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashMap<String, Vec<std::time::Instant>>>> =
        once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let Ok(mut g) = HIST.lock() else { return true };
    let v = g.entry(tab.to_string()).or_default();
    v.retain(|t| t.elapsed() < std::time::Duration::from_secs(10));
    if v.len() >= 3 {
        return false;
    }
    v.push(std::time::Instant::now());
    true
}

/// Диалоги сайтов (alert/confirm/prompt/beforeunload) без нативных модальных окон WebView2:
/// модалка «Покинуть сайт?» пряталась под webview и блокировала главное окно целиком.
/// beforeunload — уходим со страницы; alert — тост в NETFORY; confirm/prompt — «Отмена» + тост.
#[cfg(target_os = "windows")]
unsafe fn install_script_dialogs(core: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2, app: tauri::AppHandle, tab: String) {
    use tauri::Emitter;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::ScriptDialogOpeningEventHandler;
    if let Ok(settings) = core.Settings() {
        let _ = settings.SetAreDefaultScriptDialogsEnabled(false);
    }
    let mut token = 0i64;
    let _ = core.add_ScriptDialogOpening(
        &ScriptDialogOpeningEventHandler::create(Box::new(move |_sender, args| {
            let Some(args) = args else { return Ok(()) };
            let mut kind = COREWEBVIEW2_SCRIPT_DIALOG_KIND::default();
            args.Kind(&mut kind)?;
            let mut msg = windows::core::PWSTR::null();
            let message = if args.Message(&mut msg).is_ok() { webview2_com::take_pwstr(msg) } else { String::new() };
            let kind_s = match kind {
                COREWEBVIEW2_SCRIPT_DIALOG_KIND_BEFOREUNLOAD => {
                    args.Accept()?;
                    return Ok(());
                }
                COREWEBVIEW2_SCRIPT_DIALOG_KIND_ALERT => {
                    args.Accept()?;
                    "alert"
                }
                COREWEBVIEW2_SCRIPT_DIALOG_KIND_CONFIRM => "confirm",
                _ => "prompt",
            };
            crate::syslog::log("web", format!("[{tab}] site {kind_s}: {}", message.chars().take(160).collect::<String>()));
            let _ = app.emit_to("main", "site-dialog", serde_json::json!({ "tab": tab, "kind": kind_s, "message": message.chars().take(500).collect::<String>() }));
            Ok(())
        })),
        &mut token,
    );
}

/// Вкладки, для которых webview спрятан до загрузки нашей страницы ошибки
/// (v1.135.189: убирает мигание стандартной blank/error-страницы движка).
#[cfg(target_os = "windows")]
static HIDDEN_FOR_ERROR: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashSet<String>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

/// Следить за текущим URL webview (в т.ч. SPA pushState/replaceState и
/// #фрагменты, которые не дают page-load событий) и слать `<event>{tab,url}`.
/// Общий для https-вкладок (`site-url`) и dApp-вкладок (`dapp-url`).
/// Windows: SourceChanged + HistoryChanged → ICoreWebView2::Source; Linux: notify::uri.
#[cfg(target_os = "windows")]
pub(crate) fn install_url_watch(wv: &tauri::webview::Webview, app: tauri::AppHandle, tab: String, event: &'static str) {
    use webview2_com::{HistoryChangedEventHandler, SourceChangedEventHandler};
    let _ = wv.with_webview(move |pw| unsafe {
        let Ok(core) = pw.controller().CoreWebView2() else {
            return;
        };
        // sender берём из аргументов события (не захватываем core → нет COM-цикла)
        let app_src = app.clone();
        let tab_src = tab.clone();
        let mut token_src = 0i64;
        let _ = core.add_SourceChanged(
            &SourceChangedEventHandler::create(Box::new(move |sender, _args| {
                if let Some(c) = sender {
                    emit_current_source(&c, &app_src, &tab_src, event);
                }
                Ok(())
            })),
            &mut token_src,
        );
        let mut token_hist = 0i64;
        let _ = core.add_HistoryChanged(
            &HistoryChangedEventHandler::create(Box::new(move |sender, _args| {
                if let Some(c) = sender {
                    emit_current_source(&c, &app, &tab, event);
                }
                Ok(())
            })),
            &mut token_hist,
        );
    });
}

#[cfg(target_os = "linux")]
pub(crate) fn install_url_watch(wv: &tauri::webview::Webview, app: tauri::AppHandle, tab: String, event: &'static str) {
    use webkit2gtk::WebViewExt;
    let _ = wv.with_webview(move |pw| {
        let view = pw.inner();
        view.connect_uri_notify(move |v| {
            if let Some(u) = v.uri() {
                if !u.starts_with("data:") && !u.is_empty() {
                    crate::syslog::emit_queued(&app, event, serde_json::json!({ "tab": tab, "url": u.as_str() }));
                }
            }
        });
    });
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub(crate) fn install_url_watch(_wv: &tauri::webview::Webview, _app: tauri::AppHandle, _tab: String, _event: &'static str) {}

/// Windows: прочитать ICoreWebView2::Source и отправить `<event>{tab,url}` (кроме data:).
#[cfg(target_os = "windows")]
fn emit_current_source(
    core: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2,
    app: &tauri::AppHandle,
    tab: &str,
    event: &'static str,
) {
    // PWSTR без прямой зависимости от крейта `windows`: берём null-указатель из
    // CoTaskMemPWSTR::take(), после вызова строку освобождает take_pwstr().
    let url = unsafe {
        let mut holder = webview2_com::CoTaskMemPWSTR::default();
        let mut p = holder.take();
        if core.Source(&mut p).is_err() {
            return;
        }
        webview2_com::take_pwstr(p)
    };
    if !url.is_empty() && !url.starts_with("data:") {
        crate::syslog::emit_queued(&app, event, serde_json::json!({ "tab": tab, "url": url }));
    }
}

/// Linux/WebKitGTK: сигналы `load-failed` (сеть) и `load-failed-with-tls-errors`
/// (сертификат) → наша страница. Возврат `true` = обработано, движок не рисует
/// свою заглушку. Отмены (Cancelled) и policy-прерывания (наш nav_guard) пропускаем.
#[cfg(target_os = "linux")]
fn install_error_page(wv: &tauri::webview::Webview, _app: tauri::AppHandle, tab: String) {
    use webkit2gtk::WebViewExt;
    let _ = wv.with_webview(move |pw| {
        let view = pw.inner();
        let tab_net = tab.clone();
        view.connect_load_failed(move |v, _ev, failing_uri, err| {
            if !failing_uri.starts_with("https://") {
                return false;
            }
            if err.matches(webkit2gtk::NetworkError::Cancelled) || err.is::<webkit2gtk::PolicyError>() {
                return false;
            }
            let msg = err.message().to_lowercase();
            let (code, reason) = if msg.contains("resolve") || msg.contains("name or service") || msg.contains("не удалось разрешить") {
                ("ERR_NAME_NOT_RESOLVED", t3("Не удалось найти IP-адрес сервера. Проверьте адрес сайта, подключение к интернету и настройки DNS.", "Could not find the server's IP address. Check the site address, your internet connection and DNS settings.", "Alamat IP server tidak ditemukan. Periksa alamat situs, koneksi internet, dan pengaturan DNS."))
            } else if msg.contains("timed out") || msg.contains("timeout") {
                ("ERR_TIMED_OUT", t3("Сервер слишком долго не отвечает.", "The server is taking too long to respond.", "Server terlalu lama merespons."))
            } else if msg.contains("refused") {
                ("ERR_CONNECTION_REFUSED", t3("Сервер отклонил соединение.", "The server refused the connection.", "Server menolak koneksi."))
            } else if msg.contains("unreachable") {
                ("ERR_ADDRESS_UNREACHABLE", t3("Сервер недостижим по сети.", "The server is unreachable over the network.", "Server tidak dapat dijangkau melalui jaringan."))
            } else if msg.contains("reset") {
                ("ERR_CONNECTION_RESET", t3("Соединение сброшено сервером.", "The connection was reset by the server.", "Koneksi direset oleh server."))
            } else {
                ("ERR_FAILED", t3("Сетевая ошибка при подключении к сайту.", "Network error while connecting to the site.", "Kesalahan jaringan saat menghubungkan ke situs."))
            };
            crate::syslog::log("web", format!("[{}] {}: {} ({}) → страница ошибки NETFORY", tab_net, failing_uri, code, err.message()));
            v.load_uri(&failure_page_url(&tab_net, failing_uri, code, reason));
            true
        });
        let tab_tls = tab.clone();
        view.connect_load_failed_with_tls_errors(move |v, failing_uri, _cert, _flags| {
            crate::syslog::log("web", format!("[{}] {}: ERR_CERT_INVALID → страница ошибки NETFORY", tab_tls, failing_uri));
            v.load_uri(&error_page_url(
                failing_uri,
                "ERR_CERT_INVALID",
                t3("Сертификат TLS сайта недействителен — соединение небезопасно.", "The site's TLS certificate is invalid — the connection is not secure.", "Sertifikat TLS situs tidak valid — koneksi tidak aman."),
            ));
            true
        });
    });
}

/// macOS/WKWebView: wry владеет navigation-delegate, `didFailProvisionalNavigation`
/// перехватить нельзя, а движок при сбое оставляет пустую страницу. Поэтому
/// на каждом `PageLoadEvent::Started` делаем пре-флайт DNS + TCP:443 (4 с) в
/// фоновом потоке; если сеть недоступна и `Finished` так и не пришёл —
/// показываем нашу страницу (см. `macos_preflight`).
#[cfg(target_os = "macos")]
fn install_error_page(_wv: &tauri::webview::Webview, _app: tauri::AppHandle, _tab: String) {}

#[cfg(target_os = "macos")]
static LOAD_STATE: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashMap<String, (u64, bool)>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Пре-флайт сети: DNS + TCP-коннект к host:443 (таймаут `to`). Err → (код, причина).
/// v1.135.189: выполняется ПЕРЕД навигацией из адресной строки (spawn_blocking),
/// чтобы при недоступном сайте сразу открыть нашу страницу без мигания
/// стандартной blank/error-страницы движка.
fn preflight(url: &tauri::Url, to: std::time::Duration) -> Result<(), (&'static str, &'static str)> {
    use std::net::{TcpStream, ToSocketAddrs};
    let Some(host) = url.host_str() else { return Ok(()) };
    // bundle-хост (YouTube/X/Telegram) при активном роутере: маршрут решает webproxy
    // (probe + relay), прямой пре-флайт дал бы ложную «ошибку» на заблокированном сайте
    if crate::webproxy::is_bundle_host(host) {
        return Ok(());
    }
    let port = url.port().unwrap_or(443);
    match (host, port).to_socket_addrs() {
        Err(_) => Err(("ERR_NAME_NOT_RESOLVED", t3("Не удалось найти IP-адрес сервера. Проверьте адрес сайта, подключение к интернету и настройки DNS.", "Could not find the server's IP address. Check the site address, your internet connection and DNS settings.", "Alamat IP server tidak ditemukan. Periksa alamat situs, koneksi internet, dan pengaturan DNS."))),
        Ok(mut addrs) => match addrs.next() {
            None => Err(("ERR_NAME_NOT_RESOLVED", t3("DNS не вернул ни одного адреса.", "DNS returned no addresses.", "DNS tidak mengembalikan alamat apa pun."))),
            Some(a) => match TcpStream::connect_timeout(&a, to) {
                Ok(_) => Ok(()),
                Err(e) => match e.kind() {
                    std::io::ErrorKind::TimedOut => Err(("ERR_TIMED_OUT", t3("Сервер слишком долго не отвечает.", "The server is taking too long to respond.", "Server terlalu lama merespons."))),
                    std::io::ErrorKind::ConnectionRefused => Err(("ERR_CONNECTION_REFUSED", t3("Сервер отклонил соединение.", "The server refused the connection.", "Server menolak koneksi."))),
                    _ => Err(("ERR_ADDRESS_UNREACHABLE", t3("Сервер недостижим по сети.", "The server is unreachable over the network.", "Server tidak dapat dijangkau melalui jaringan."))),
                },
            },
        },
    }
}

/// URL для навигации из адресной строки: сам сайт или (после неудачного
/// пре-флайта) сразу наша страница ошибки. Блокирующий — звать из spawn_blocking.
fn resolve_nav_target(tab: &str, target: tauri::Url) -> tauri::Url {
    // Mycelium: прямой пре-флайт (локальный DNS + TCP) раскрыл бы IP — только проверка цепочки
    if crate::mycel::enabled() {
        if mycelium_ready() || crate::mycel::load().allow_direct {
            return target;
        }
        crate::syslog::log("web", format!("[{tab}] {target}: Mycelium ещё без цепочки → страница ожидания"));
        return mycelium_page_url(target.as_str(), false).parse().unwrap_or(target);
    }
    match preflight(&target, std::time::Duration::from_millis(2500)) {
        Ok(()) => target,
        Err((code, reason)) => {
            crate::syslog::log("web", format!("[{}] {}: {} (preflight) → страница ошибки NETFORY", tab, target, code));
            failure_page_url(tab, target.as_str(), code, reason).parse().unwrap_or(target)
        }
    }
}

/// macOS: учёт Started/Finished + фоновый пре-флайт; при сбое — наша страница.
#[cfg(target_os = "macos")]
fn macos_preflight(app: tauri::AppHandle, tab: String, url: tauri::Url, started: bool) {
    let generation = {
        let mut g = match LOAD_STATE.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let e = g.entry(tab.clone()).or_insert((0, false));
        if started {
            e.0 += 1;
            e.1 = false;
        } else {
            e.1 = true;
        }
        e.0
    };
    if !started || url.scheme() != "https" {
        return;
    }
    std::thread::spawn(move || {
        let Err((code, reason)) = preflight(&url, std::time::Duration::from_secs(4)) else { return };
        // страница уже успела загрузиться или началась другая навигация — не трогаем
        let still_pending = LOAD_STATE
            .lock()
            .ok()
            .and_then(|g| g.get(&tab).map(|(g2, fin)| *g2 == generation && !*fin))
            .unwrap_or(false);
        if !still_pending {
            return;
        }
        crate::syslog::log("web", format!("[{}] {}: {} (preflight) → страница ошибки NETFORY", tab, url, code));
        if let Ok(target) = failure_page_url(&tab, url.as_str(), code, reason).parse::<tauri::Url>() {
            if let Some(w) = app.get_webview(&label(&tab)) {
                let _ = w.navigate(target);
            }
        }
    });
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn install_error_page(_wv: &tauri::webview::Webview, _app: tauri::AppHandle, _tab: String) {}

/// Разрешён ли URL для web-вкладки: только https (+ служебные blank/blob/data
/// для внутренних нужд сайтов — плееры, воркеры, превью).
fn allowed(url: &tauri::Url) -> bool {
    matches!(url.scheme(), "https" | "about" | "blob" | "data")
}

fn parse_https(url: &str) -> Result<tauri::Url, String> {
    let u: tauri::Url = url.trim().parse().map_err(|_| "bad url".to_string())?;
    if u.scheme() != "https" {
        return Err("https-only".into());
    }
    if u.host_str().map(|h| h.is_empty()).unwrap_or(true) {
        return Err("bad url".into());
    }
    Ok(u)
}

/// Страж навигации: https — свободно; http → блок + событие (фронт откроет
/// https-вариант); схемы SmartNet → host-shell; остальное — блок.
/// v1.135.225: каждый переход пишем в syslog — диагностика «клик не открывает».
fn nav_guard(app: tauri::AppHandle, tab: String) -> impl Fn(&tauri::Url) -> bool + Send + 'static {
    move |url| {
        remember_url(&tab, url);
        if allowed(url) {
            if url.scheme() == "https" {
                crate::syslog::log("web", format!("[{}] nav → {}", tab, url));
            }
            return true;
        }
        let scheme = url.scheme();
        if scheme == "http" {
            let mut up = url.clone();
            if up.set_scheme("https").is_ok() {
                crate::syslog::emit_queued(&app, "site-open-link", serde_json::json!({ "tab": tab, "url": up.to_string(), "new_window": false, "upgraded": true }));
            }
            return false;
        }
        if matches!(scheme, "sth" | "u" | "sn" | "api" | "f" | "dev") {
            crate::syslog::emit_queued(&app, "site-open-link", serde_json::json!({ "tab": tab, "url": url.to_string(), "new_window": false }));
        } else if scheme == "ntfry-tab" {
            // v1.135.226: LINK_SCRIPT внутри страницы перевёл target=_blank /
            // window.open / Ctrl+клик в навигацию ntfry-tab://open?u=<url> —
            // открываем новой вкладкой NETFORY (NewWindowRequested у WebView2
            // на Google-выдаче до нас не доходил).
            let raw = url.query_pairs().find(|(k, _)| k == "u").map(|(_, v)| v.to_string()).unwrap_or_default();
            crate::syslog::log("web", format!("[{}] new-tab (script) → {}", tab, raw));
            if let Some(link) = link_as_https(&raw) {
                crate::syslog::emit_queued(&app, "site-open-link", serde_json::json!({ "tab": tab, "url": link, "new_window": true }));
            }
        } else {
            crate::syslog::log("web", format!("nav blocked ({}): {}", scheme, url));
        }
        false
    }
}

/// Скрипт web-вкладки (только главный фрейм, без какого-либо моста к Tauri):
/// ссылки target=_blank, Ctrl/⌘+клик, средняя кнопка и window.open(http) →
/// навигация `ntfry-tab://open?u=…`, которую nav_guard превращает в новую
/// вкладку NETFORY. Плюс страховка: если JS сайта отменил клик по ВНЕШНЕЙ
/// (другой origin) ссылке и за 600 мс ни ушёл, ни сменил URL (SPA-роутер) —
/// переходим по href сами (Google-выдача в WebView2).
const LINK_SCRIPT: &str = r#"(function(){
  if (window.__ntfryLinks) return; window.__ntfryLinks = true;
  function abs(u){ try { return new URL(String(u), location.href).href; } catch (e) { return ''; } }
  function isHttp(u){ return /^https?:\/\//i.test(u); }
  function newTab(u){ var h = abs(u); if (!isHttp(h)) return false; try { location.href = 'ntfry-tab://open?u=' + encodeURIComponent(h); } catch (e) {} return true; }
  var nativeOpen = window.open;
  window.open = function(u){ var h = abs(u); if (h && isHttp(h) && newTab(h)) return null; return nativeOpen ? nativeOpen.apply(window, arguments) : null; };
  function anchorOf(e){ var t = e.target; if (t && t.nodeType !== 1) t = t.parentElement; return t && t.closest ? t.closest('a[href]') : null; }
  var gone = false; addEventListener('pagehide', function(){ gone = true; });
  document.addEventListener('click', function(e){
    if (e.button !== 0) return;
    var a = anchorOf(e); if (!a) return;
    var h = a.href; if (!isHttp(h)) return;
    var blank = (a.getAttribute('target') || '').toLowerCase() === '_blank';
    if (blank || e.ctrlKey || e.metaKey) { if (newTab(h)) { e.preventDefault(); e.stopImmediatePropagation(); } return; }
    if (a.origin === location.origin) return;
    var before = location.href;
    setTimeout(function(){ if (!gone && location.href === before && e.defaultPrevented && !window.__ntfryNav) { window.__ntfryNav = true; location.href = a.href; } }, 600);
  }, true);
  document.addEventListener('auxclick', function(e){
    if (e.button !== 1) return;
    var a = anchorOf(e); if (!a || !isHttp(a.href)) return;
    if (newTab(a.href)) { e.preventDefault(); e.stopImmediatePropagation(); }
  }, true);
})();"#;

#[cfg(target_os = "linux")]
fn apply_bounds(wv: &tauri::webview::Webview, x: f64, y: f64, w: f64, h: f64) {
    crate::webview::linux_embed_bounds(wv, x, y, w, h);
}

#[cfg(not(target_os = "linux"))]
fn apply_bounds(wv: &tauri::webview::Webview, x: f64, y: f64, w: f64, h: f64) {
    let _ = wv.set_position(LogicalPosition::new(x, y));
    let _ = wv.set_size(LogicalSize::new(w, h));
}

/// Создать (или показать/переместить) web-вкладку с https://-URL поверх tab-host.
#[tauri::command]
pub(crate) async fn open_site_webview(
    app: tauri::AppHandle,
    tab: String,
    url: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let lbl = label(&tab);
    if let Some(wv) = app.get_webview(&lbl) {
        apply_bounds(&wv, x, y, width, height);
        let _ = wv.show();
        let _ = wv.set_focus();
        return Ok(());
    }
    let target = parse_https(&url)?;
    let tab_pf = tab.clone();
    let target = tokio::task::spawn_blocking(move || resolve_nav_target(&tab_pf, target))
        .await
        .map_err(|e| e.to_string())?;
    let window = app.get_window("main").ok_or("main window missing")?;
    let app_dl = app.clone();
    let tab_dl = tab.clone();
    let app_nav = app.clone();
    let app_title = app.clone();
    let app_new = app.clone();
    let tab_nav = tab.clone();
    let tab_title = tab.clone();
    let tab_new = tab.clone();
    #[allow(unused_mut)]
    let mut builder = WebviewBuilder::new(&lbl, WebviewUrl::External(target))
        .initialization_script(LINK_SCRIPT)
        .on_navigation(nav_guard(app.clone(), tab.clone()))
        .on_page_load(move |_wv, payload| {
            // наша data:-страница ошибки не должна попадать в адресную строку
            if payload.url().scheme() == "data" {
                return;
            }
            let loading = matches!(payload.event(), PageLoadEvent::Started);
            #[cfg(target_os = "macos")]
            macos_preflight(app_nav.clone(), tab_nav.clone(), payload.url().clone(), loading);
            crate::syslog::emit_queued(&app_nav, "site-nav", serde_json::json!({ "tab": tab_nav, "url": payload.url().to_string(), "loading": loading }));
        })
        .on_document_title_changed(move |_wv, title| {
            crate::syslog::emit_queued(&app_title, "site-title", serde_json::json!({ "tab": tab_title, "title": title }));
        })
        .on_download(move |_wv, event| {
            // v1.135.189: системный диалог WebView2 перекрывается main-webview —
            // сохраняем в папку «Загрузки» и сообщаем фронту (тост + открыть папку).
            handle_download(&app_dl, &tab_dl, event)
        })
        .on_new_window(move |url, _features| {
            // Новое окно сайта (target=_blank / window.open) → новая вкладка NETFORY.
            // v1.135.225: http поднимаем до https; всё логируем (about:blank и т.п. —
            // сайт открыл пустое окно и собирался задать location позже: так не умеем).
            crate::syslog::log("web", format!("[{}] new-window → {}", tab_new, url));
            if let Some(link) = link_as_https(url.as_str()) {
                crate::syslog::emit_queued(&app_new, "site-open-link", serde_json::json!({ "tab": tab_new, "url": link, "new_window": true }));
            }
            NewWindowResponse::Deny
        });
    // Linux/macOS: proxy per-webview (на Windows — process-wide PAC из webproxy::start_early)
    #[cfg(not(target_os = "windows"))]
    {
        let port = crate::webproxy::port();
        if port != 0 {
            if let Ok(u) = format!("socks5://127.0.0.1:{port}").parse::<tauri::Url>() {
                builder = builder.proxy_url(u);
            }
        }
    }
    let _created = window
        .add_child(builder, LogicalPosition::new(x, y), LogicalSize::new(width, height))
        .map_err(|e| e.to_string())?;
    // Linux: сразу уносим child из GtkBox в GtkFixed с реальными bounds (см. webview.rs).
    #[cfg(target_os = "linux")]
    apply_bounds(&_created, x, y, width, height);
    remember_url(&tab, &url.parse::<tauri::Url>().unwrap_or_else(|_| "about:blank".parse().unwrap()));
    install_error_page(&_created, app.clone(), tab.clone());
    install_url_watch(&_created, app.clone(), tab.clone(), "site-url");
    install_link_menu(&_created, app.clone(), tab.clone());
    crate::syslog::log("web", format!("open {} → {}", tab, url));
    Ok(())
}

// ---- контекстное меню ссылки (v1.135.225) ---------------------------------
//  ПКМ по <a href="http(s)://…"> внутри web-вкладки: к стандартному меню движка
//  добавляем «Открыть в новой вкладке NETFORY» и «Открыть в системном браузере».

fn link_menu_tab() -> &'static str {
    t3("Открыть в новой вкладке NETFORY", "Open in new NETFORY tab", "Buka di tab NETFORY baru")
}
fn link_menu_system() -> &'static str {
    t3("Открыть в системном браузере", "Open in system browser", "Buka di browser sistem")
}

/// http(s)-ссылка → https-строка (web-вкладки — только TLS); иначе None.
fn link_as_https(link: &str) -> Option<String> {
    let mut u: tauri::Url = link.trim().parse().ok()?;
    match u.scheme() {
        "https" => {}
        "http" => u.set_scheme("https").ok()?,
        _ => return None,
    }
    u.host_str().filter(|h| !h.is_empty())?;
    Some(u.to_string())
}

/// Действие пункта меню: новая вкладка NETFORY (событие фронту) или системный браузер.
fn link_action(app: &tauri::AppHandle, tab: &str, link: &str, system: bool) {
    if system {
        use tauri_plugin_shell::ShellExt;
        crate::syslog::log("web", format!("[{}] меню ссылки → системный браузер: {}", tab, link));
        let _ = app.shell().open(link.to_string(), None);
    } else {
        crate::syslog::log("web", format!("[{}] меню ссылки → новая вкладка: {}", tab, link));
        crate::syslog::emit_queued(&app, "site-open-link", serde_json::json!({ "tab": tab, "url": link, "new_window": true }));
    }
}

/// Windows/WebView2: ContextMenuRequested (ICoreWebView2_11) — для цели со
/// ссылкой вставляем два своих пункта + разделитель в начало стандартного меню.
#[cfg(target_os = "windows")]
fn install_link_menu(wv: &tauri::webview::Webview, app: tauri::AppHandle, tab: String) {
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::{ContextMenuRequestedEventHandler, CoTaskMemPWSTR, CustomItemSelectedEventHandler};
    use windows_core::Interface;
    let _ = wv.with_webview(move |pw| unsafe {
        let Ok(core) = pw.controller().CoreWebView2() else {
            return;
        };
        let Ok(core11) = core.cast::<ICoreWebView2_11>() else {
            crate::syslog::log("web", "контекстное меню ссылок недоступно: нужен WebView2 Runtime ≥ 1.0.1185".to_string());
            return;
        };
        let Ok(env9) = core
            .cast::<ICoreWebView2_2>()
            .and_then(|c| c.Environment())
            .and_then(|e| e.cast::<ICoreWebView2Environment9>())
        else {
            return;
        };
        let mut token = 0i64;
        let _ = core11.add_ContextMenuRequested(
            &ContextMenuRequestedEventHandler::create(Box::new(move |_sender, args| {
                let Some(args) = args else { return Ok(()) };
                let target = args.ContextMenuTarget()?;
                let mut has_link = windows_core::BOOL::default();
                target.HasLinkUri(&mut has_link)?;
                if !has_link.as_bool() {
                    return Ok(());
                }
                let raw = {
                    let mut holder = CoTaskMemPWSTR::default();
                    let mut p = holder.take();
                    target.LinkUri(&mut p)?;
                    webview2_com::take_pwstr(p)
                };
                let Some(link) = link_as_https(&raw) else { return Ok(()) };
                let items = args.MenuItems()?;
                for (i, (caption, system)) in [(link_menu_tab(), false), (link_menu_system(), true)].into_iter().enumerate() {
                    let text = windows_core::HSTRING::from(caption);
                    let item = env9.CreateContextMenuItem(
                        &text,
                        None::<&windows::Win32::System::Com::IStream>,
                        COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND,
                    )?;
                    let (app, tab, link) = (app.clone(), tab.clone(), link.clone());
                    let mut t = 0i64;
                    item.add_CustomItemSelected(
                        &CustomItemSelectedEventHandler::create(Box::new(move |_, _| {
                            link_action(&app, &tab, &link, system);
                            Ok(())
                        })),
                        &mut t,
                    )?;
                    items.InsertValueAtIndex(i as u32, &item)?;
                }
                let sep = env9.CreateContextMenuItem(
                    windows_core::PCWSTR::null(),
                    None::<&windows::Win32::System::Com::IStream>,
                    COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_SEPARATOR,
                )?;
                items.InsertValueAtIndex(2, &sep)?;
                Ok(())
            })),
            &mut token,
        );
    });
}

/// Linux/WebKitGTK: сигнал `context-menu` — по HitTestResult со ссылкой
/// добавляем свои пункты (GAction) в начало стандартного меню.
#[cfg(target_os = "linux")]
fn install_link_menu(wv: &tauri::webview::Webview, app: tauri::AppHandle, tab: String) {
    use gtk::prelude::*;
    use webkit2gtk::{ContextMenuExt, ContextMenuItem, HitTestResultExt, WebViewExt};
    let _ = wv.with_webview(move |pw| {
        let view = pw.inner();
        view.connect_context_menu(move |_v, menu, _event, hit| {
            let Some(uri) = hit.link_uri() else { return false };
            let Some(link) = link_as_https(uri.as_str()) else { return false };
            for (i, (caption, system)) in [(link_menu_tab(), false), (link_menu_system(), true)].into_iter().enumerate() {
                let action = gtk::gio::SimpleAction::new(if system { "ntfry-link-system" } else { "ntfry-link-tab" }, None);
                let (app, tab, link) = (app.clone(), tab.clone(), link.clone());
                action.connect_activate(move |_, _| link_action(&app, &tab, &link, system));
                menu.insert(&ContextMenuItem::from_gaction(&action, caption, None), i as i32);
            }
            menu.insert(&ContextMenuItem::new_separator(), 2);
            false
        });
    });
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn install_link_menu(_wv: &tauri::webview::Webview, _app: tauri::AppHandle, _tab: String) {}

/// Перейти по новому https://-URL внутри существующей web-вкладки.
#[tauri::command]
pub(crate) async fn site_navigate(app: tauri::AppHandle, tab: String, url: String) -> Result<(), String> {
    let target = parse_https(&url)?;
    remember_url(&tab, &target);
    let tab_pf = tab.clone();
    let target = tokio::task::spawn_blocking(move || resolve_nav_target(&tab_pf, target))
        .await
        .map_err(|e| e.to_string())?;
    let wv = app.get_webview(&label(&tab)).ok_or("no webview")?;
    wv.navigate(target).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn set_site_bounds(
    app: tauri::AppHandle,
    tab: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    if let Some(wv) = app.get_webview(&label(&tab)) {
        apply_bounds(&wv, x, y, width, height);
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn hide_site_webview(app: tauri::AppHandle, tab: String) -> Result<(), String> {
    if let Some(wv) = app.get_webview(&label(&tab)) {
        let _ = wv.hide();
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn close_site_webview(app: tauri::AppHandle, tab: String) -> Result<(), String> {
    if let Some(wv) = app.get_webview(&label(&tab)) {
        let _ = wv.close();
    }
    if let Ok(mut g) = LAST_URL.lock() {
        g.remove(&tab);
    }
    Ok(())
}

/// Закрыть все web-вкладки (F5 главного окна / lock — как close_all_embedded_webviews).
#[tauri::command]
pub(crate) async fn close_all_site_webviews(app: tauri::AppHandle) -> Result<(), String> {
    for (lbl, wv) in app.webviews() {
        if lbl.starts_with("web-") {
            let _ = wv.close();
        }
    }
    Ok(())
}

fn eval_in(app: &tauri::AppHandle, tab: &str, js: &str) -> Result<(), String> {
    if let Some(wv) = app.get_webview(&label(tab)) {
        wv.eval(js).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn site_history_back(app: tauri::AppHandle, tab: String) -> Result<(), String> {
    eval_in(&app, &tab, "try{history.back()}catch(e){}")
}

#[tauri::command]
pub(crate) async fn site_history_forward(app: tauri::AppHandle, tab: String) -> Result<(), String> {
    eval_in(&app, &tab, "try{history.forward()}catch(e){}")
}

#[tauri::command]
pub(crate) async fn site_reload(app: tauri::AppHandle, tab: String) -> Result<(), String> {
    // на нашей data:-странице ошибки reload = повторить исходный https-переход
    eval_in(
        &app,
        &tab,
        "try{if(location.protocol==='data:'&&window.__nfRetry){window.__nfRetry()}else{location.reload()}}catch(e){}",
    )
}

/// Имя файла для загрузки: из предложенного движком destination, иначе из URL.
fn download_name(url: &tauri::Url, suggested: &std::path::Path) -> String {
    let from_dest = suggested.file_name().and_then(|n| n.to_str()).unwrap_or("").trim().to_string();
    let name = if !from_dest.is_empty() {
        from_dest
    } else {
        url.path_segments()
            .and_then(|mut s| s.next_back())
            .map(|s| percent_decode(s))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "download".to_string())
    };
    // без разделителей путей и служебных символов
    let clean: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    clean.trim_matches('.').to_string()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()).ok_or(()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Уникальный путь в папке: name.ext → name (1).ext …
fn unique_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{}", e)),
        _ => (name.to_string(), String::new()),
    };
    for i in 1..1000 {
        let c = dir.join(format!("{} ({}){}", stem, i, ext));
        if !c.exists() {
            return c;
        }
    }
    dir.join(format!("{}-{}{}", stem, std::process::id(), ext))
}

/// Загрузки со https-сайтов: Requested → путь в «Загрузки» (+ событие
/// `site-download{state:"started"}`), Finished → `state:"finished"` с путём/успехом.
fn handle_download(app: &tauri::AppHandle, tab: &str, event: tauri::webview::DownloadEvent<'_>) -> bool {
    use tauri::webview::DownloadEvent;
    match event {
        DownloadEvent::Requested { url, destination } => {
            let dir = app
                .path()
                .download_dir()
                .or_else(|_| app.path().home_dir())
                .unwrap_or_else(|_| std::env::temp_dir());
            let _ = std::fs::create_dir_all(&dir);
            let name = download_name(&url, destination);
            let path = unique_path(&dir, &name);
            *destination = path.clone();
            crate::syslog::log("web", format!("[{}] download {} → {}", tab, url, path.display()));
            crate::syslog::emit_queued(&app, "site-download", serde_json::json!({
                    "tab": tab, "state": "started", "url": url.to_string(),
                    "name": path.file_name().and_then(|n| n.to_str()).unwrap_or(&name),
                    "path": path.to_string_lossy(),
                }));
            true
        }
        DownloadEvent::Finished { url, path, success } => {
            let p = path.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            crate::syslog::log("web", format!("[{}] download finished ok={} {} {}", tab, success, url, p));
            crate::syslog::emit_queued(&app, "site-download", serde_json::json!({
                    "tab": tab, "state": "finished", "url": url.to_string(), "success": success,
                    "name": std::path::Path::new(&p).file_name().and_then(|n| n.to_str()).unwrap_or(""),
                    "path": p,
                }));
            true
        }
        _ => false,
    }
}

/// Открыть папку с загруженным файлом в системном проводнике.
#[tauri::command]
pub(crate) async fn site_reveal_download(app: tauri::AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_shell::ShellExt;
    let p = std::path::PathBuf::from(&path);
    let dir = if p.is_dir() { p.clone() } else { p.parent().map(|d| d.to_path_buf()).unwrap_or(p.clone()) };
    // только внутри «Загрузки»/домашней папки — команда не должна быть универсальным opener'ом
    let allowed = [app.path().download_dir().ok(), app.path().home_dir().ok()];
    if !allowed.iter().flatten().any(|root| dir.starts_with(root)) {
        return Err("path outside downloads".into());
    }
    app.shell().open(dir.to_string_lossy().to_string(), None).map_err(|e| e.to_string())
}
