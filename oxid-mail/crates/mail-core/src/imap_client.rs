// Синхронный IMAP-клиент поверх rustls (вызывать из spawn_blocking).
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use crate::account::MailAccount;

type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;
type ImapSession = imap::Session<TlsStream>;

pub struct FetchedMessage {
    pub uid: u32,
    pub raw: Vec<u8>,
    pub seen: bool,
    pub flagged: bool,
}

pub struct FolderFetch {
    pub folder: String,
    pub messages: Vec<FetchedMessage>,
}

fn tls(host: &str, port: u16) -> Result<TlsStream, String> {
    crate::init_tls();
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| format!("bad host {host}: {e}"))?;
    let conn = rustls::ClientConnection::new(Arc::new(config), name).map_err(|e| e.to_string())?;
    let tcp = TcpStream::connect((host, port)).map_err(|e| format!("connect {host}:{port}: {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(45))).ok();
    tcp.set_write_timeout(Some(Duration::from_secs(45))).ok();
    Ok(rustls::StreamOwned::new(conn, tcp))
}

/// Переводит сырые IMAP-отказы в понятные инструкции (Gmail/Yandex требуют app password).
fn friendly_login_error(raw: &str) -> String {
    let low = raw.to_lowercase();
    if low.contains("application-specific password required") {
        return format!(
            "Gmail rejects account passwords over IMAP. Create an App Password: \
             Google Account → Security → 2-Step Verification → App passwords \
             (myaccount.google.com/apppasswords), then use the 16-character code \
             instead of your password. [{raw}]"
        );
    }
    if low.contains("imap is disabled") || low.contains("invalid credentials") {
        return format!(
            "Login rejected. Yandex: create an app password (id.yandex.ru/security/app-passwords) \
             and enable IMAP in Yandex.Mail → Settings → «Почтовые программы». \
             Gmail: use an App Password (myaccount.google.com/apppasswords). [{raw}]"
        );
    }
    format!("IMAP login failed: {raw}")
}

struct XOAuth2 {
    user: String,
    token: String,
}

impl imap::Authenticator for XOAuth2 {
    type Response = String;
    fn process(&self, _data: &[u8]) -> String {
        crate::oauth::xoauth2_raw(&self.user, &self.token)
    }
}

fn login(acc: &MailAccount) -> Result<ImapSession, String> {
    let stream = tls(&acc.imap_host, acc.imap_port)?;
    let client = imap::Client::new(stream);
    if acc.is_oauth() {
        let auth = XOAuth2 {
            user: acc.email.clone(),
            token: acc.access_token.clone(),
        };
        client
            .authenticate("XOAUTH2", &auth)
            .map_err(|(e, _)| format!("Gmail OAuth login failed: {e} (попробуйте переподключить аккаунт)"))
    } else {
        client
            .login(&acc.email, &acc.password)
            .map_err(|(e, _)| friendly_login_error(&e.to_string()))
    }
}

/// UI-имя папки → реальное имя ящика на сервере.
fn resolve_mailbox(session: &mut ImapSession, folder: &str) -> Option<String> {
    match folder {
        "inbox" => Some("INBOX".to_string()),
        "sent" => find_sent_folder(session),
        _ => None,
    }
}

/// Пуш флага (\Seen / \Flagged) на сервер по UID. Best-effort.
pub fn set_flag(acc: &MailAccount, folder: &str, uid: u32, flag: &str, on: bool) -> Result<(), String> {
    let mut session = login(acc)?;
    let mailbox = resolve_mailbox(&mut session, folder).ok_or_else(|| format!("no server mailbox for {folder}"))?;
    session.select(&mailbox).map_err(|e| e.to_string())?;
    let op = if on { "+FLAGS" } else { "-FLAGS" };
    session
        .uid_store(uid.to_string(), format!("{op} ({flag})"))
        .map_err(|e| e.to_string())?;
    session.logout().ok();
    Ok(())
}

/// Окончательное удаление на сервере: +\Deleted и EXPUNGE по UID.
pub fn delete_permanent(acc: &MailAccount, folder: &str, uid: u32) -> Result<(), String> {
    let mut session = login(acc)?;
    let mailbox = resolve_mailbox(&mut session, folder).ok_or_else(|| format!("no server mailbox for {folder}"))?;
    session.select(&mailbox).map_err(|e| e.to_string())?;
    session
        .uid_store(uid.to_string(), "+FLAGS (\\Deleted)")
        .map_err(|e| e.to_string())?;
    session.expunge().map_err(|e| e.to_string())?;
    session.logout().ok();
    Ok(())
}

fn find_trash_folder(session: &mut ImapSession) -> Option<String> {
    let names = session.list(None, Some("*")).ok()?;
    for n in names.iter() {
        for a in n.attributes() {
            if let imap::types::NameAttribute::Custom(c) = a {
                if c.as_ref().eq_ignore_ascii_case("\\Trash") {
                    return Some(n.name().to_string());
                }
            }
        }
    }
    for cand in ["Trash", "Deleted Items", "Deleted Messages", "[Gmail]/Trash", "Корзина"] {
        if names.iter().any(|n| n.name() == cand) {
            return Some(cand.to_string());
        }
    }
    None
}

/// Перемещение письма в серверную папку Trash (COPY + \Deleted + EXPUNGE).
/// Если Trash не найдена — просто помечаем \Deleted (мягкое удаление).
pub fn move_to_trash(acc: &MailAccount, folder: &str, uid: u32) -> Result<(), String> {
    let mut session = login(acc)?;
    let trash = find_trash_folder(&mut session);
    let mailbox = resolve_mailbox(&mut session, folder).ok_or_else(|| format!("no server mailbox for {folder}"))?;
    session.select(&mailbox).map_err(|e| e.to_string())?;
    if let Some(trash) = trash {
        if trash != mailbox {
            session.uid_copy(uid.to_string(), &trash).map_err(|e| e.to_string())?;
        }
        session
            .uid_store(uid.to_string(), "+FLAGS (\\Deleted)")
            .map_err(|e| e.to_string())?;
        session.expunge().map_err(|e| e.to_string())?;
    } else {
        session
            .uid_store(uid.to_string(), "+FLAGS (\\Deleted)")
            .map_err(|e| e.to_string())?;
    }
    session.logout().ok();
    Ok(())
}

/// Догрузка старых писем: UID-диапазон до before_uid (окно 500, берём последние count).
pub fn fetch_older(acc: &MailAccount, folder: &str, before_uid: u32, count: u32) -> Result<Vec<FetchedMessage>, String> {
    if before_uid <= 1 {
        return Ok(vec![]);
    }
    let mut session = login(acc)?;
    let mailbox = resolve_mailbox(&mut session, folder).ok_or_else(|| format!("no server mailbox for {folder}"))?;
    session.select(&mailbox).map_err(|e| e.to_string())?;
    let lo = before_uid.saturating_sub(500).max(1);
    let hi = before_uid - 1;
    let set = format!("{lo}:{hi}");
    let fetches = session
        .uid_fetch(&set, "(UID FLAGS BODY.PEEK[])")
        .map_err(|e| format!("uid_fetch {folder}: {e}"))?;
    let mut out: Vec<FetchedMessage> = Vec::new();
    for f in fetches.iter() {
        let raw = match f.body() {
            Some(b) => b.to_vec(),
            None => continue,
        };
        out.push(FetchedMessage {
            uid: f.uid.unwrap_or(0),
            raw,
            seen: f.flags().iter().any(|fl| matches!(fl, imap::types::Flag::Seen)),
            flagged: f.flags().iter().any(|fl| matches!(fl, imap::types::Flag::Flagged)),
        });
    }
    session.logout().ok();
    out.sort_by_key(|m| m.uid);
    if out.len() > count as usize {
        out = out.split_off(out.len() - count as usize);
    }
    Ok(out)
}

/// Один цикл IMAP IDLE на INBOX. Ok(true) — пришло событие, Ok(false) — тихий выход.
pub fn idle_wait(acc: &MailAccount) -> Result<bool, String> {
    let mut session = login(acc)?;
    session.select("INBOX").map_err(|e| e.to_string())?;
    let idle = session.idle().map_err(|e| e.to_string())?;
    // wait() блокирует до события; серверы обычно сбрасывают IDLE ~каждые 30 мин —
    // тогда wait вернётся/даст ошибку и watcher-цикл перезайдёт заново
    idle.wait().map_err(|e| e.to_string())?;
    Ok(true)
}

/// Проверка логина при добавлении аккаунта.
pub fn verify_login(acc: &MailAccount) -> Result<(), String> {
    let mut s = login(acc)?;
    s.logout().ok();
    Ok(())
}

fn find_sent_folder(session: &mut ImapSession) -> Option<String> {
    let names = session.list(None, Some("*")).ok()?;
    for n in names.iter() {
        for a in n.attributes() {
            if let imap::types::NameAttribute::Custom(c) = a {
                if c.as_ref().eq_ignore_ascii_case("\\Sent") {
                    return Some(n.name().to_string());
                }
            }
        }
    }
    for cand in [
        "Sent",
        "Sent Items",
        "Sent Messages",
        "[Gmail]/Sent Mail",
        "Отправленные",
    ] {
        if names.iter().any(|n| n.name() == cand) {
            return Some(cand.to_string());
        }
    }
    None
}

fn fetch_folder(session: &mut ImapSession, mailbox: &str, limit: u32) -> Result<Vec<FetchedMessage>, String> {
    let mb = session
        .select(mailbox)
        .map_err(|e| format!("select {mailbox}: {e}"))?;
    let total = mb.exists;
    if total == 0 {
        return Ok(vec![]);
    }
    let start = if total > limit { total - limit + 1 } else { 1 };
    let set = format!("{start}:{total}");
    let fetches = session
        .fetch(&set, "(UID FLAGS BODY.PEEK[])")
        .map_err(|e| format!("fetch {mailbox}: {e}"))?;
    let mut out = Vec::new();
    for f in fetches.iter() {
        let raw = match f.body() {
            Some(b) => b.to_vec(),
            None => continue,
        };
        let seen = f
            .flags()
            .iter()
            .any(|fl| matches!(fl, imap::types::Flag::Seen));
        let flagged = f
            .flags()
            .iter()
            .any(|fl| matches!(fl, imap::types::Flag::Flagged));
        out.push(FetchedMessage {
            uid: f.uid.unwrap_or(0),
            raw,
            seen,
            flagged,
        });
    }
    Ok(out)
}

/// Скачивает INBOX + папку «Отправленные» (если найдена) за один сеанс.
pub fn sync_fetch(acc: &MailAccount, inbox_limit: u32, sent_limit: u32) -> Result<Vec<FolderFetch>, String> {
    let mut session = login(acc)?;
    let mut out = vec![FolderFetch {
        folder: "inbox".into(),
        messages: fetch_folder(&mut session, "INBOX", inbox_limit)?,
    }];
    if let Some(sent) = find_sent_folder(&mut session) {
        if let Ok(messages) = fetch_folder(&mut session, &sent, sent_limit) {
            out.push(FolderFetch {
                folder: "sent".into(),
                messages,
            });
        }
    }
    session.logout().ok();
    Ok(out)
}
