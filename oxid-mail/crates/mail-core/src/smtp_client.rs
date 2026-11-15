// SMTP-отправка через lettre + rustls (вызывать из spawn_blocking).
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Body, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};

use crate::account::MailAccount;
use crate::message::AttachmentData;

pub fn send_mail(
    acc: &MailAccount,
    to: &str,
    subject: &str,
    body: &str,
    attachments: &[AttachmentData],
    extra_headers: &[(String, String)],
) -> Result<(), String> {
    crate::init_tls();
    let from: Mailbox = acc.email.parse().map_err(|e| format!("bad sender: {e}"))?;
    // Несколько получателей через запятую (UI: чипы, max 20) — по .to() на каждого
    let mut builder_msg = Message::builder().from(from).subject(subject);
    let mut n_rcpt = 0usize;
    for part in to.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let mb: Mailbox = part.parse().map_err(|e| format!("bad recipient {part}: {e}"))?;
        builder_msg = builder_msg.to(mb);
        n_rcpt += 1;
    }
    if n_rcpt == 0 {
        return Err("bad recipient: empty".into());
    }
    let mut email = if attachments.is_empty() {
        builder_msg
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .map_err(|e| e.to_string())?
    } else {
        let mut mp = MultiPart::mixed().singlepart(SinglePart::plain(body.to_string()));
        for a in attachments {
            let ct = ContentType::parse(&a.meta.mime)
                .unwrap_or(ContentType::parse("application/octet-stream").unwrap());
            mp = mp.singlepart(Attachment::new(a.meta.name.clone()).body(Body::new(a.data.clone()), ct));
        }
        builder_msg.multipart(mp).map_err(|e| e.to_string())?
    };
    for (name, value) in extra_headers {
        let hn = lettre::message::header::HeaderName::new_from_ascii(name.clone())
            .map_err(|e| format!("bad header name: {e}"))?;
        email
            .headers_mut()
            .insert_raw(lettre::message::header::HeaderValue::new(hn, value.clone()));
    }
    let creds = if acc.is_oauth() {
        Credentials::new(acc.email.clone(), acc.access_token.clone())
    } else {
        Credentials::new(acc.email.clone(), acc.password.clone())
    };
    let mechanisms = if acc.is_oauth() {
        vec![lettre::transport::smtp::authentication::Mechanism::Xoauth2]
    } else {
        vec![
            lettre::transport::smtp::authentication::Mechanism::Plain,
            lettre::transport::smtp::authentication::Mechanism::Login,
        ]
    };
    let builder = if acc.smtp_port == 465 {
        SmtpTransport::relay(&acc.smtp_host).map_err(|e| e.to_string())?
    } else {
        SmtpTransport::starttls_relay(&acc.smtp_host).map_err(|e| e.to_string())?
    };
    let mailer = builder
        .port(acc.smtp_port)
        .credentials(creds)
        .authentication(mechanisms)
        .build();
    mailer.send(&email).map_err(|e| format!("SMTP send failed: {e}"))?;
    Ok(())
}
