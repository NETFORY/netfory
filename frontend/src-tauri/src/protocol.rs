// ============================================================
//  SmartNet // protocol.rs
//
//  Custom `sth://` URI scheme handler (zero-local-server rendering).
//  Installed dApps are served straight from `data/dapps/<app_id>` as
//  MIME-typed bytes into the child webview — no local HTTP port.
//  Includes an SPA fallback (serve index.html for extension-less routes)
//  so in-app navigation + F5/CTRL+R reloads keep working, and a
//  path-traversal guard so a dApp can never escape its own sandbox.
//
//  Author: TechnoL0g
//  Created: 2026-06-18
// ============================================================

use crate::ensure_data_dir;

/// Cyberpunk-styled HTML served on a 404 (broken/unresolvable dApp link) instead
/// of a bare `text/plain` message — keeps the in-app experience on-brand.
pub(crate) fn not_found_page() -> String {
    r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>SmartNet // App not found</title>
<style>
  *{margin:0;padding:0;box-sizing:border-box}
  html,body{height:100%}
  body{display:flex;align-items:center;justify-content:center;background:#070a0f;
    color:#cbd5e1;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
    overflow:hidden;position:relative}
  body::before{content:"";position:absolute;inset:0;
    background:repeating-linear-gradient(0deg,rgba(34,211,238,.04) 0 1px,transparent 1px 3px);
    pointer-events:none}
  .box{position:relative;text-align:center;padding:48px 56px;max-width:560px;
    border:1px solid rgba(34,211,238,.25);border-radius:14px;
    background:rgba(13,18,26,.72);backdrop-filter:blur(8px);
    box-shadow:0 0 60px rgba(34,211,238,.08),inset 0 0 30px rgba(34,211,238,.04)}
  .code{font-size:76px;font-weight:800;letter-spacing:4px;line-height:1;
    color:#22d3ee;text-shadow:0 0 18px rgba(34,211,238,.55);
    animation:flick 3.5s infinite}
  @keyframes flick{0%,92%,100%{opacity:1}93%{opacity:.35}95%{opacity:1}96%{opacity:.5}}
  .scheme{margin-top:14px;font-size:13px;letter-spacing:3px;color:#64748b;text-transform:uppercase}
  h1{margin-top:22px;font-size:22px;color:#e2e8f0;letter-spacing:1px}
  p{margin-top:14px;font-size:13.5px;line-height:1.7;color:#94a3b8}
  .tag{display:inline-block;margin-top:26px;padding:7px 16px;font-size:11px;letter-spacing:2px;
    text-transform:uppercase;color:#22d3ee;border:1px solid rgba(34,211,238,.3);border-radius:999px}
  .glow{position:absolute;width:280px;height:280px;border-radius:50%;
    background:radial-gradient(circle,rgba(217,70,239,.18),transparent 70%);
    top:-120px;right:-100px;filter:blur(10px)}
</style></head>
<body><div class="box"><div class="glow"></div>
  <div class="code">404</div>
  <div class="scheme">sth:// protocol</div>
  <h1>Приложение не найдено</h1>
  <p>Этот dApp не установлен локально или ссылка повреждена.<br>
     Установите приложение из децентрализованного стора или проверьте адрес <span style="color:#22d3ee">sth://&lt;id&gt;</span>.</p>
  <span class="tag">Autonomous P2P · SmartHoldem</span>
</div></body></html>"#
        .to_string()
}


/// Map a file extension to a Content-Type for the `sth://` response.
pub(crate) fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "application/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Find an installed dApp folder by id, matching the real directory name
/// case-insensitively (URL parsers may lower-case the segment).
fn dapp_dir(app: &tauri::AppHandle, id: &str) -> Option<std::path::PathBuf> {
    if id.is_empty() {
        return None;
    }
    let dapps = ensure_data_dir(app).join("dapps");
    let exact = dapps.join(id);
    if exact.is_dir() {
        return Some(exact);
    }
    std::fs::read_dir(&dapps).ok().and_then(|rd| {
        rd.flatten().map(|e| e.path()).find(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.eq_ignore_ascii_case(id))
                    .unwrap_or(false)
        })
    })
}

/// Recover the dApp id from a URL host/authority, normalizing the per-platform
/// custom-scheme mapping:
///   - Linux/macOS (native authority scheme): host == `<APP_ID>`
///   - Windows/Android (WebView2 mapping):     host == `<APP_ID>.sth.localhost`
///     (the scheme name `sth` becomes the parent label; the app id survives as
///     the leftmost subdomain). A bare `sth.localhost` means the id was dropped.
/// Returns "" when no app id is present (caller then falls back to the Referer).
pub(crate) fn host_app_id(host: &str) -> String {
    let h = host.strip_suffix(".localhost").unwrap_or(host);
    let h = h.strip_suffix(".sth").unwrap_or(h);
    // wry 0.55 (Windows/WebView2) применяет workaround `sth://<id>/` →
    // `http://sth.<id>/`: в Referer схема приходит ПРЕФИКСОМ хоста.
    let h = h.strip_prefix("sth.").unwrap_or(h);
    if h.is_empty() || h.eq_ignore_ascii_case("sth") || h.eq_ignore_ascii_case("localhost") {
        return String::new();
    }
    h.to_string()
}

/// Extract the host/authority from a full URL (used to recover the app id from a
/// `Referer` header when an absolute-path subresource arrives on a stripped host).
fn url_host(url: &str) -> String {
    let after = url.splitn(2, "://").nth(1).unwrap_or("");
    after
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Extract the `sthapp=<id>` marker from a query string (initial-URL fallback
/// when the platform WebView drops/mangles the custom-scheme authority).
fn query_app_id(query: &str) -> String {
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("sthapp="))
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}

/// Query-string part of a full URL (between '?' and '#').
fn url_query(url: &str) -> &str {
    url.splitn(2, '?')
        .nth(1)
        .unwrap_or("")
        .split('#')
        .next()
        .unwrap_or("")
}

/// Resolve an `sth://` request to file bytes + MIME.
///
/// dApps are loaded at `sth://<APP_ID>/` so the id lives in the HOST (authority).
/// Absolute SPA asset paths (`/_nuxt/x.js`, `/assets/app.css`) then resolve
/// relative to that authority — `sth://<APP_ID>/_nuxt/x.js` — preserving the dApp
/// context on EVERY platform (the WebView2 `sth.<APP_ID>` workaround mapping is
/// normalized in `host_app_id`). Routing, in order:
///   1. HOST is an installed app id                          (page + all assets)
///   2. app id recovered from the `Referer` host             (host-stripped case)
///   3. `?sthapp=<id>` marker in the request / Referer query (last resort)
/// Every unresolved request is logged to SYSCON (scope `dapp`) with the raw
/// host/path/referer so a 404 is diagnosable on the user's machine.
pub(crate) fn resolve_request(
    app: &tauri::AppHandle,
    host: &str,
    path: &str,
    referer: &str,
    query: &str,
) -> Option<(Vec<u8>, String)> {
    let mut id = host_app_id(host);
    if id.is_empty() {
        id = host_app_id(&url_host(referer));
    }
    if id.is_empty() {
        id = query_app_id(query);
    }
    if id.is_empty() {
        id = query_app_id(url_query(referer));
    }
    if id.is_empty() {
        crate::syslog::log(
            "dapp",
            format!("sth 404: app id not extracted (host='{host}', path='{path}', referer='{referer}')"),
        );
        return None;
    }
    // Канонический id и приватность берём из content.json папки dApp.
    // WebView лоуеркейсит host, а на Windows exact-путь находит папку в
    // ЛЮБОМ регистре (FS регистронезависимая) — восстановить регистр по
    // имени папки нельзя. Манифест из папки надёжен на всех платформах
    // и не зависит от регистра Sled-ключей.
    if let Some(dir) = dapp_dir(app, &id) {
        if let Ok(raw) = std::fs::read(dir.join("content.json")) {
            if let Ok(m) = serde_json::from_slice::<crate::Manifest>(&raw) {
                let is_priv = m.private == Some(true);
                id = m.id;
                if is_priv {
                    // Приватный dApp: контент отдаётся ТОЛЬКО из расшифрованной
                    // RAM-карты (plaintext на диск не пишется никогда).
                    return crate::private_dapp::serve_ram(app, &id, path);
                }
            }
        }
    }
    // Fallback: папка не найдена, но манифест есть в Sled.
    if crate::private_dapp::is_private(&id) {
        return crate::private_dapp::serve_ram(app, &id, path);
    }
    let Some(base) = dapp_dir(app, &id) else {
        let dapps = ensure_data_dir(app).join("dapps");
        let have: Vec<String> = std::fs::read_dir(&dapps)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().to_str().map(String::from))
                    .take(20)
                    .collect()
            })
            .unwrap_or_default();
        crate::syslog::log(
            "dapp",
            format!(
                "sth 404: dApp folder '{id}' not found (host='{host}', path='{path}', in data/dapps: [{}])",
                have.join(", ")
            ),
        );
        return None;
    };
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let out = serve(&base, rel);
    if out.is_none() {
        crate::syslog::log(
            "dapp",
            format!("sth 404: file '{rel}' not found in {}", base.display()),
        );
    }
    out
}

/// Serve `rel` from an app `base` dir, with an SPA fallback to index.html.
fn serve(base: &std::path::Path, rel: &str) -> Option<(Vec<u8>, String)> {
    let canon_base = base.canonicalize().ok()?;
    // 1) try the requested file as-is (path-traversal sandboxed)
    if let Some(found) = read_within_sandbox(&canon_base, &base.join(rel), rel) {
        return Some(found);
    }
    // 2) SPA fallback: маршрут без ИЗВЕСТНОГО статик-расширения → index.html.
    //    v1.63.9: раньше ЛЮБАЯ точка в последнем сегменте считалась признаком
    //    ассета — deep-link'и вида /trading/XBTSX.STH_XBTSX.USDT (пара с
    //    точкой) падали в 404 вместо SPA-роутинга. Теперь ассет — только
    //    реальное web-расширение из списка.
    if !is_static_asset(rel) {
        return read_within_sandbox(&canon_base, &base.join("index.html"), "index.html");
    }
    None
}

/// Путь указывает на статический ассет (известное web-расширение)?
pub(crate) fn is_static_asset(rel: &str) -> bool {
    const EXT: &[&str] = &[
        "html", "htm", "js", "mjs", "css", "json", "svg", "png", "jpg", "jpeg", "gif", "webp",
        "avif", "ico", "woff2", "woff", "ttf", "otf", "eot", "wasm", "txt", "map", "xml",
        "webmanifest", "mp3", "mp4", "webm", "ogg", "wav", "pdf", "zip", "gz", "br",
    ];
    std::path::Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| EXT.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Read a file only if it canonicalizes inside `canon_base` (path-traversal guard).
fn read_within_sandbox(
    canon_base: &std::path::Path,
    target: &std::path::Path,
    rel: &str,
) -> Option<(Vec<u8>, String)> {
    let canon_target = target.canonicalize().ok()?;
    if !canon_target.starts_with(canon_base) {
        return None;
    }
    let bytes = std::fs::read(&canon_target).ok()?;
    Some((bytes, mime_for(rel).to_string()))
}
