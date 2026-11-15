// Google OAuth 2.0 — Desktop App (installed) flow с loopback-редиректом и PKCE.
// Открывает СИСТЕМНЫЙ браузер (Google блокирует OAuth во встроенных webview),
// ловит code на 127.0.0.1:{случайный порт}, меняет на access+refresh токены.
// XOAUTH2 — для IMAP AUTHENTICATE и SMTP AUTH.
// REMINDER: DO NOT HARDCODE THE URL, OR ADD ANY FALLBACKS OR REDIRECT URLS, THIS BREAKS THE AUTH
// (redirect_uri здесь — loopback 127.0.0.1 с динамическим портом, как требует Google для desktop-клиентов)
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};

// Desktop-клиент пользователя (для installed apps client_secret не считается тайной по докам Google).
// Переопределяются переменными окружения GOOGLE_CLIENT_ID / GOOGLE_CLIENT_SECRET.
const DEFAULT_CLIENT_ID: &str = "781493281342-821a8p6tdujstbo8fjrebgbgl69ah4hv.apps.googleusercontent.com";
const DEFAULT_CLIENT_SECRET: &str = "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";

const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://mail.google.com/ openid email";

pub fn client_id() -> String {
    std::env::var("GOOGLE_CLIENT_ID").unwrap_or_else(|_| DEFAULT_CLIENT_ID.into())
}
fn client_secret() -> String {
    std::env::var("GOOGLE_CLIENT_SECRET").unwrap_or_else(|_| DEFAULT_CLIENT_SECRET.into())
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

#[derive(Debug, Clone)]
pub struct OAuthTokens {
    pub email: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
}

fn rand_string(len: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::thread_rng();
    (0..len).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect()
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Достаёт email из id_token (JWT payload, base64url) — токен получен нами по TLS напрямую от Google.
fn email_from_id_token(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("email")?.as_str().map(|s| s.to_string())
}

/// Ждёт редирект Google на loopback и вытаскивает ?code=
fn wait_for_code(listener: &TcpListener, expected_state: &str, timeout: Duration) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false).ok();
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let first = req.lines().next().unwrap_or("");
                // GET /?code=...&state=... HTTP/1.1
                let query = first.split_whitespace().nth(1).unwrap_or("").split('?').nth(1).unwrap_or("");
                let mut code = None;
                let mut state = None;
                let mut error = None;
                for kv in query.split('&') {
                    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                    match k {
                        "code" => code = Some(v.to_string()),
                        "state" => state = Some(v.to_string()),
                        "error" => error = Some(v.to_string()),
                        _ => {}
                    }
                }
                let ok = code.is_some() && state.as_deref() == Some(expected_state);
                let body = if ok {
                    "<html><body style='font-family:sans-serif;background:#15171A;color:#eee;display:grid;place-items:center;height:100vh'><div><h2 style='color:#E25822'>Oxid Mail</h2><p>Вход выполнен — вернитесь в приложение.<br/>Signed in — return to the app.</p></div></body></html>"
                } else {
                    "<html><body style='font-family:sans-serif'>OAuth error — return to the app.</body></html>"
                };
                let _ = stream.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes(),
                );
                if let Some(e) = error {
                    return Err(format!("Google OAuth error: {e}"));
                }
                if ok {
                    return Ok(code.unwrap());
                }
                // чужой запрос (favicon и т.п.) — ждём дальше
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("OAuth timeout: подтверждение в браузере не получено за 180 секунд".into());
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Полный интерактивный вход: браузер → code → токены → email. Блокирует до 180 сек.
pub fn gmail_sign_in() -> Result<OAuthTokens, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("loopback bind: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}");

    let verifier = rand_string(64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = rand_string(32);

    let auth_url = format!(
        "{AUTH_ENDPOINT}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&code_challenge={}&code_challenge_method=S256&state={}",
        urlencode(&client_id()),
        urlencode(&redirect_uri),
        urlencode(SCOPE),
        challenge,
        state
    );
    webbrowser::open(&auth_url).map_err(|e| format!("не удалось открыть браузер: {e}"))?;

    let code = wait_for_code(&listener, &state, Duration::from_secs(180))?;

    let resp: serde_json::Value = ureq::post(TOKEN_ENDPOINT)
        .send_form(&[
            ("code", code.as_str()),
            ("client_id", &client_id()),
            ("client_secret", &client_secret()),
            ("redirect_uri", &redirect_uri),
            ("grant_type", "authorization_code"),
            ("code_verifier", &verifier),
        ])
        .map_err(|e| format!("token exchange: {e}"))?
        .into_json()
        .map_err(|e| e.to_string())?;

    let access_token = resp["access_token"].as_str().ok_or("no access_token in response")?.to_string();
    let refresh_token = resp["refresh_token"].as_str().ok_or("no refresh_token (нужны access_type=offline&prompt=consent)")?.to_string();
    let expires_in = resp["expires_in"].as_u64().unwrap_or(3600);
    let email = resp["id_token"]
        .as_str()
        .and_then(email_from_id_token)
        .ok_or("no email in id_token")?;

    Ok(OAuthTokens {
        email,
        access_token,
        refresh_token,
        expires_at: now_unix() + expires_in.saturating_sub(60),
    })
}

/// Обновление access_token по refresh_token. Возвращает (access_token, expires_at).
pub fn refresh_access_token(refresh_token: &str) -> Result<(String, u64), String> {
    let resp: serde_json::Value = ureq::post(TOKEN_ENDPOINT)
        .send_form(&[
            ("refresh_token", refresh_token),
            ("client_id", &client_id()),
            ("client_secret", &client_secret()),
            ("grant_type", "refresh_token"),
        ])
        .map_err(|e| format!("token refresh: {e}"))?
        .into_json()
        .map_err(|e| e.to_string())?;
    let access = resp["access_token"].as_str().ok_or("no access_token on refresh")?.to_string();
    let expires_in = resp["expires_in"].as_u64().unwrap_or(3600);
    Ok((access, now_unix() + expires_in.saturating_sub(60)))
}

/// Сырая XOAUTH2-строка (base64 делает сам imap/lettre слой).
pub fn xoauth2_raw(email: &str, access_token: &str) -> String {
    format!("user={email}\x01auth=Bearer {access_token}\x01\x01")
}
