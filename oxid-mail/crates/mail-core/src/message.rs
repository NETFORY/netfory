use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    pub name: String,
    pub mime: String,
    pub size: usize,
}

pub struct AttachmentData {
    pub meta: AttachmentMeta,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub id: String,
    pub account_id: String,
    pub folder: String,
    pub from: Address,
    pub to: String,
    pub subject: String,
    pub snippet: String,
    pub date: String,
    pub read: bool,
    pub starred: bool,
    pub encrypted: bool,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub attachments: Vec<AttachmentMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_html: Option<String>,
    /// Значение заголовка X-Ntfry-Sig (Anti-Spoofing подпись отправителя)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ntfry_sig: Option<String>,
    /// Путь доставки отправленного письма: "p2p" | "smtp"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

impl StoredMessage {
    pub fn summary(&self) -> StoredMessage {
        let mut s = self.clone();
        s.body = None;
        s.body_html = None;
        s
    }
}

pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    mail_parser::DateTime::from_timestamp(secs).to_rfc3339()
}

fn addr_pair(a: &mail_parser::Addr) -> (String, String) {
    (
        a.name.as_deref().unwrap_or("").to_string(),
        a.address.as_deref().unwrap_or("").to_string(),
    )
}

fn first_addr(h: Option<&mail_parser::Address>) -> (String, String) {
    h.and_then(|a| a.first()).map(addr_pair).unwrap_or_default()
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Разбор сырого RFC-822 письма: сообщение + вложения (HTML санитизируется через ammonia).
pub fn parse_raw(raw: &[u8], account_id: &str, folder: &str, uid: u32, seen: bool, flagged: bool) -> (StoredMessage, Vec<AttachmentData>) {
    use mail_parser::MimeHeaders;
    let parsed = mail_parser::MessageParser::default().parse(raw);
    let mut atts: Vec<AttachmentData> = Vec::new();
    let (from_name, from_email, to, subject, date, text, html) = match &parsed {
        Some(m) => {
            for part in m.attachments() {
                let data = part.contents().to_vec();
                if data.is_empty() {
                    continue;
                }
                let name = part
                    .attachment_name()
                    .unwrap_or("attachment")
                    .to_string();
                let mime = part
                    .content_type()
                    .map(|ct| match ct.subtype() {
                        Some(s) => format!("{}/{}", ct.ctype(), s),
                        None => ct.ctype().to_string(),
                    })
                    .unwrap_or_else(|| "application/octet-stream".into());
                atts.push(AttachmentData {
                    meta: AttachmentMeta { name, mime, size: data.len() },
                    data,
                });
            }
            let (fname, femail) = first_addr(m.from());
            let (tname, temail) = first_addr(m.to());
            let to_str = if tname.is_empty() {
                temail
            } else {
                format!("{tname} <{temail}>")
            };
            (
                fname,
                femail,
                to_str,
                m.subject().unwrap_or("(no subject)").to_string(),
                m.date().map(|d| d.to_rfc3339()).unwrap_or_default(),
                m.body_text(0).map(|c| c.to_string()),
                m.body_html(0).map(|c| c.to_string()),
            )
        }
        None => (
            String::new(),
            String::new(),
            String::new(),
            "(unparseable message)".into(),
            String::new(),
            None,
            None,
        ),
    };
    let text = text.or_else(|| html.as_deref().map(strip_tags));
    let ntfry_sig = parsed
        .as_ref()
        .and_then(|m| m.header_raw("X-Ntfry-Sig"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let encrypted = text
        .as_deref()
        .map(mail_crypto::armor::contains_ntfry)
        .unwrap_or(false);
    // Санитизация HTML: вырезаем <script>, on*-обработчики, javascript:-ссылки и т.п.
    let html = html.map(|h| ammonia::clean(&h));
    let snippet: String = text
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(140)
        .collect();
    let display_name = if from_name.is_empty() {
        from_email
            .split('@')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown")
            .to_string()
    } else {
        from_name
    };
    let msg = StoredMessage {
        id: format!("{folder}_{uid}"),
        account_id: account_id.to_string(),
        folder: folder.to_string(),
        from: Address {
            name: display_name,
            email: from_email,
        },
        to,
        subject,
        snippet,
        date: if date.is_empty() { now_rfc3339() } else { date },
        read: seen || folder == "sent",
        starred: flagged,
        encrypted,
        labels: vec![],
        attachments: atts.iter().map(|a| a.meta.clone()).collect(),
        body: text,
        body_html: html,
        ntfry_sig,
        via: None,
    };
    (msg, atts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_rfc822() {
        let raw = b"From: Alice <alice@example.com>\r\nTo: bob@example.com\r\nSubject: Hello\r\nDate: Tue, 10 Jun 2025 10:00:00 +0000\r\n\r\nHi Bob, this is a test.";
        let (m, atts) = parse_raw(raw, "acc1", "inbox", 42, false, false);
        assert!(atts.is_empty());
        assert_eq!(m.id, "inbox_42");
        assert_eq!(m.from.name, "Alice");
        assert_eq!(m.from.email, "alice@example.com");
        assert_eq!(m.subject, "Hello");
        assert!(m.body.as_deref().unwrap().contains("test"));
        assert!(!m.read);
        assert!(m.snippet.contains("Hi Bob"));
    }
}
