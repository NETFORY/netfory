// ============================================================
//  SmartNet // devhub_ipc.rs — loopback-IPC для git-remote-dev.
//
//  Ультра-лёгкий HTTP-сервер на 127.0.0.1:<эфемерный порт> — мост между
//  утилитой `git-remote-dev` (Git Remote Helper) и работающим клиентом.
//  Helper запускается самим git на время clone/push/pull, находит порт и
//  токен через discovery-файл (~/.config/smartnet/devhub-ipc.json) и
//  дергает три эндпоинта:
//    GET  /refs?user=&repo=            → JSON { exists, refs: [{sha,name}] }
//    GET  /bundle?user=&repo=          → байты полного git-bundle
//    POST /push?user=&repo=&commits=&refs=<urlenc JSON>  (тело = bundle)
//  Авторизация: заголовок X-Auth со случайным токеном (файл читается только
//  локальным пользователем ОС). Слушаем ТОЛЬКО loopback.
//
//  Author: TechnoL0g
//  Created: 2026-06
// ============================================================

use tauri::AppHandle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Максимальный размер принимаемого bundle (512 МБ).
const MAX_BODY: usize = 512 * 1024 * 1024;

/// Discovery-файл для helper'а: {"port":N,"token":"hex"}.
fn discovery_path() -> Option<std::path::PathBuf> {
    let base = if let Ok(appdata) = std::env::var("APPDATA") {
        std::path::PathBuf::from(appdata)
    } else if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home).join(".config")
    } else {
        return None;
    };
    Some(base.join("smartnet").join("devhub-ipc.json"))
}

fn gen_token() -> String {
    let pid = std::process::id();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let addr = crate::load_vault_internal().map(|v| v.address).unwrap_or_default();
    blake3::hash(format!("devhub-ipc:{}:{}:{}", pid, now, addr).as_bytes())
        .to_hex()
        .to_string()
}

/// Минимальный percent-decode для query-значений.
fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        if it.next() == Some(key) {
            return Some(url_decode(it.next().unwrap_or("")));
        }
    }
    None
}

async fn respond(stream: &mut TcpStream, code: u16, ctype: &str, body: &[u8]) {
    let status = match code {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        code, status, ctype, body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.flush().await;
}

async fn handle(mut stream: TcpStream, app: AppHandle, token: String) {
    // Читаем заголовки (до \r\n\r\n, максимум 64 КБ).
    let mut head = Vec::with_capacity(2048);
    let mut buf = [0u8; 4096];
    let split_at;
    loop {
        let n = match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        head.extend_from_slice(&buf[..n]);
        if let Some(pos) = head.windows(4).position(|w| w == b"\r\n\r\n") {
            split_at = pos + 4;
            break;
        }
        if head.len() > 65536 {
            return;
        }
    }
    let header_text = String::from_utf8_lossy(&head[..split_at]).to_string();
    let mut lines = header_text.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    let mut auth = String::new();
    let mut content_length: usize = 0;
    for l in lines {
        let low = l.to_ascii_lowercase();
        if let Some(v) = low.strip_prefix("x-auth:") {
            auth = v.trim().to_string();
        }
        if let Some(v) = low.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    if auth != token {
        respond(&mut stream, 401, "text/plain; charset=utf-8", "bad token".as_bytes()).await;
        return;
    }
    if content_length > MAX_BODY {
        respond(&mut stream, 404, "text/plain; charset=utf-8", "body too large".as_bytes()).await;
        return;
    }
    // Дочитываем тело (bundle может быть большим).
    let mut body = head[split_at..].to_vec();
    while body.len() < content_length {
        let n = match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        body.extend_from_slice(&buf[..n]);
    }

    let user = query_param(&query, "user").unwrap_or_default();
    let repo = query_param(&query, "repo").unwrap_or_default();
    let address = crate::devhub::resolve_dev_username(&user);

    match (method.as_str(), path.as_str()) {
        ("GET", "/refs") => {
            let Some(addr) = address else {
                respond(&mut stream, 404, "application/json", br#"{"exists":false,"refs":[]}"#).await;
                return;
            };
            // Чужой автор без кэша: пробуем P2P-синк индекса (для git clone).
            if crate::devhub::load_index(&app, &addr).is_none() {
                let _ = crate::devhub::sync_remote(&app, &user).await;
            }
            let idx = crate::devhub::load_index(&app, &addr);
            let meta = idx.as_ref().and_then(|i| i.repos.iter().find(|m| m.name == repo));
            let exists = crate::devhub::repo_dir(&app, &addr, &repo).exists() || meta.is_some();
            let mut refs = crate::devhub::repo_refs(&app, &addr, &repo);
            if refs.is_empty() {
                // Рефы чужого репо приходят в составе ПОДПИСАННОГО индекса.
                if let Some(m) = meta {
                    refs = m.refs.clone();
                }
            }
            if refs.is_empty() && exists && crate::devhub::is_my_address(&addr) {
                // Легаси-репо без git-истории: генерируем initial commit,
                // иначе `git clone` отдаст пустой репозиторий.
                if crate::devhub::ensure_bundle(&app, &user, &repo).await.is_ok() {
                    refs = crate::devhub::repo_refs(&app, &addr, &repo);
                }
            }
            let json = serde_json::json!({ "exists": exists, "refs": refs }).to_string();
            respond(&mut stream, 200, "application/json", json.as_bytes()).await;
        }
        ("GET", "/bundle") => {
            // Свой/кэш — мгновенно; чужой — swarm-загрузка по bundle_hash из индекса.
            match crate::devhub::ensure_bundle(&app, &user, &repo).await {
                Ok(path) => match std::fs::read(&path) {
                    Ok(bytes) => respond(&mut stream, 200, "application/octet-stream", &bytes).await,
                    Err(e) => respond(&mut stream, 404, "text/plain; charset=utf-8", e.to_string().as_bytes()).await,
                },
                Err(e) => respond(&mut stream, 404, "text/plain; charset=utf-8", e.as_bytes()).await,
            }
        }
        ("POST", "/push") => {
            let commits: u32 = query_param(&query, "commits")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let refs: Vec<crate::devhub::DevRef> = query_param(&query, "refs")
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_default();
            match crate::devhub::apply_push(&app, &user, &repo, &body, refs, commits).await {
                Ok(()) => respond(&mut stream, 200, "text/plain; charset=utf-8", b"ok").await,
                Err(e) => respond(&mut stream, 404, "text/plain; charset=utf-8", e.as_bytes()).await,
            }
        }
        _ => respond(&mut stream, 404, "text/plain; charset=utf-8", b"not found").await,
    }
}

/// Поднять loopback-IPC и записать discovery-файл. Вызывается в setup().
pub(crate) fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(l) => l,
            Err(e) => {
                crate::syslog::log("devhub", format!("IPC for git-remote-dev failed to start: {}", e));
                return;
            }
        };
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        let token = gen_token();
        if let Some(path) = discovery_path() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let json = serde_json::json!({ "port": port, "token": token }).to_string();
            let _ = std::fs::write(&path, json);
        }
        crate::syslog::log(
            "devhub",
            format!("git-remote-dev IPC on 127.0.0.1:{} (discovery: ~/.config/smartnet/devhub-ipc.json)", port),
        );
        loop {
            if let Ok((stream, _)) = listener.accept().await {
                let app2 = app.clone();
                let tok = token.clone();
                tauri::async_runtime::spawn(async move { handle(stream, app2, tok).await });
            }
        }
    });
}
