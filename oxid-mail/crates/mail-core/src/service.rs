// Универсальный сервис-слой Oxid Mail: одна реализация команд
// для всех транспортов (HTTP-бридж в web-dev и Tauri invoke).
// Все функции БЛОКИРУЮЩИЕ — вызывающая сторона оборачивает в spawn_blocking.
use std::path::Path;

use serde::Deserialize;

use crate::account::{AccountPublic, MailAccount};
use crate::identity::{self, Identity};
use crate::imap_client;
use crate::message::{self, StoredMessage};
use crate::oauth;
use crate::smtp_client;
use mail_store::MailStore;

#[derive(Debug)]
pub enum CoreError {
    NotFound(&'static str),
    Msg(String),
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::NotFound(what) => write!(f, "{what} not found"),
            CoreError::Msg(m) => write!(f, "{m}"),
        }
    }
}

impl From<String> for CoreError {
    fn from(s: String) -> Self {
        CoreError::Msg(s)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRequest {
    pub email: String,
    #[serde(default)]
    pub password: String,
    pub provider: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessagePatch {
    pub read: Option<bool>,
    pub starred: Option<bool>,
    pub folder: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentIn {
    pub name: String,
    #[serde(default)]
    pub mime: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendRequest {
    pub to: String,
    pub subject: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub attachments: Vec<AttachmentIn>,
    /// Шифрование SmartNet (NTFRY): получатели должны быть привязаны в реестре
    #[serde(default)]
    pub encrypt: bool,
    /// Идентификатор задачи P2P-раздачи для отслеживания прогресса из UI
    /// (UI генерирует UUID перед отправкой). Актуально только для @sth-получателей.
    #[serde(default)]
    pub progress_id: Option<String>,
}

fn msgs_tree(account_id: &str) -> String {
    format!("msgs:{account_id}")
}

fn atts_tree(account_id: &str) -> String {
    format!("atts:{account_id}")
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Открывает шифрованное хранилище по секретной фразе; данные в {data_dir}/{address}/.
/// Произвольные фразы SmartNet допустимы (BIP-39 валидация — только в UI импорта).
pub fn open_store(data_dir: &Path, mnemonic: &str) -> Result<(Identity, MailStore, Vec<AccountPublic>), CoreError> {
    let ident = identity::from_secret(mnemonic)?;
    let store = MailStore::open(data_dir.join(&ident.address), &ident.mnemonic)?;
    ensure_p2p_account(&store, &ident)?;
    let publics = list_accounts(&store)?;
    Ok((ident, store, publics))
}

/// id системного P2P-аккаунта "<STH-адрес>@sth".
pub const P2P_ACCOUNT_ID: &str = "p2p";

/// Системный P2P-аккаунт создаётся из seed при каждом открытии сессии (идемпотентно).
fn ensure_p2p_account(store: &MailStore, ident: &Identity) -> Result<(), CoreError> {
    let email = format!("{}@sth", ident.address);
    if let Some(existing) = store.get_json::<MailAccount>("accounts", P2P_ACCOUNT_ID)? {
        if existing.email == email {
            return Ok(());
        }
    }
    let acc = MailAccount {
        id: P2P_ACCOUNT_ID.into(),
        email,
        password: String::new(),
        provider: "smartnet".into(),
        imap_host: String::new(),
        imap_port: 0,
        smtp_host: String::new(),
        smtp_port: 0,
        auth_kind: "p2p".into(),
        refresh_token: String::new(),
        access_token: String::new(),
        token_expires_at: 0,
    };
    store.put_json("accounts", P2P_ACCOUNT_ID, &acc)?;
    store.flush();
    Ok(())
}

pub fn list_accounts(store: &MailStore) -> Result<Vec<AccountPublic>, CoreError> {
    let accounts: Vec<MailAccount> = store.list_json("accounts")?;
    Ok(accounts.iter().map(|a| a.public()).collect())
}

pub fn get_account(store: &MailStore, id: &str) -> Result<MailAccount, CoreError> {
    store
        .get_json("accounts", id)?
        .ok_or(CoreError::NotFound("account"))
}

/// Для OAuth-аккаунтов: обновить access_token при истечении и сохранить в store.
pub fn ensure_token(store: &MailStore, acc: &mut MailAccount) -> Result<(), CoreError> {
    if !acc.is_oauth() {
        return Ok(());
    }
    if !acc.access_token.is_empty() && acc.token_expires_at > oauth::now_unix() + 30 {
        return Ok(());
    }
    let (access, expires_at) = oauth::refresh_access_token(&acc.refresh_token)?;
    acc.access_token = access;
    acc.token_expires_at = expires_at;
    store.put_json("accounts", &acc.id, acc)?;
    Ok(())
}

/// «1 клик» Gmail: браузер → OAuth → верификация IMAP XOAUTH2 → аккаунт в store.
pub fn oauth_gmail_sign_in(store: &MailStore) -> Result<AccountPublic, CoreError> {
    let tokens = oauth::gmail_sign_in()?;
    // если этот gmail уже добавлен — обновляем токены вместо дубля
    let existing: Vec<MailAccount> = store.list_json("accounts")?;
    let id = existing
        .iter()
        .find(|a| a.email.eq_ignore_ascii_case(&tokens.email))
        .map(|a| a.id.clone())
        .unwrap_or_else(|| format!("acc_{}", now_ms()));
    let acc = MailAccount {
        id,
        email: tokens.email,
        password: String::new(),
        provider: "gmail".into(),
        imap_host: "imap.gmail.com".into(),
        imap_port: 993,
        smtp_host: "smtp.gmail.com".into(),
        smtp_port: 465,
        auth_kind: "oauth".into(),
        refresh_token: tokens.refresh_token,
        access_token: tokens.access_token,
        token_expires_at: tokens.expires_at,
    };
    imap_client::verify_login(&acc)?;
    store.put_json("accounts", &acc.id, &acc)?;
    store.flush();
    Ok(acc.public())
}

/// id вида "{folder}_{uid}" → (server-folder, uid); local_* → None.
fn imap_ref(msg_id: &str) -> Option<(&str, u32)> {
    let (folder, uid) = msg_id.rsplit_once('_')?;
    if folder != "inbox" && folder != "sent" {
        return None;
    }
    uid.parse::<u32>().ok().filter(|u| *u > 0).map(|u| (folder, u))
}

/// Проверяет IMAP-логин и сохраняет аккаунт (пароль — только в шифрованном хранилище).
pub fn add_account(store: &MailStore, req: AccountRequest) -> Result<AccountPublic, CoreError> {
    if req.password.is_empty() {
        return Err(CoreError::Msg("password / app key is required".into()));
    }
    let acc = MailAccount {
        id: format!("acc_{}", now_ms()),
        email: req.email,
        password: req.password,
        provider: req.provider,
        imap_host: req.imap_host,
        imap_port: req.imap_port,
        smtp_host: req.smtp_host,
        smtp_port: req.smtp_port,
        auth_kind: "password".into(),
        refresh_token: String::new(),
        access_token: String::new(),
        token_expires_at: 0,
    };
    imap_client::verify_login(&acc)?;
    store.put_json("accounts", &acc.id, &acc)?;
    store.flush();
    Ok(acc.public())
}

pub fn update_account(store: &MailStore, id: &str, req: AccountRequest) -> Result<AccountPublic, CoreError> {
    if id == P2P_ACCOUNT_ID {
        return Err(CoreError::Msg("Системный аккаунт SmartNet P2P нельзя редактировать".into()));
    }
    let old = get_account(store, id)?;
    let mut acc = MailAccount {
        id: id.to_string(),
        email: req.email,
        password: if req.password.is_empty() { old.password } else { req.password },
        provider: req.provider,
        imap_host: req.imap_host,
        imap_port: req.imap_port,
        smtp_host: req.smtp_host,
        smtp_port: req.smtp_port,
        auth_kind: old.auth_kind,
        refresh_token: old.refresh_token,
        access_token: old.access_token,
        token_expires_at: old.token_expires_at,
    };
    ensure_token(store, &mut acc)?;
    imap_client::verify_login(&acc)?;
    store.put_json("accounts", id, &acc)?;
    store.flush();
    Ok(acc.public())
}

pub fn delete_account(store: &MailStore, id: &str) -> Result<(), CoreError> {
    if id == P2P_ACCOUNT_ID {
        return Err(CoreError::Msg("Системный аккаунт SmartNet P2P нельзя удалить".into()));
    }
    store.delete("accounts", id)?;
    store.drop_tree(&msgs_tree(id))?;
    store.drop_tree(&atts_tree(id))?;
    store.flush();
    Ok(())
}

/// IMAP-синхронизация: INBOX(50) + Sent(30); локальные флаги не перетираются.
pub fn sync_account(store: &MailStore, id: &str) -> Result<u32, CoreError> {
    let mut acc = get_account(store, id)?;
    if acc.is_p2p() {
        return Ok(0); // P2P-аккаунт: письма приходят напрямую через Iroh
    }
    ensure_token(store, &mut acc)?;
    let fetched = imap_client::sync_fetch(&acc, 50, 30)?;
    let tree = msgs_tree(id);
    let att_tree = atts_tree(id);
    let mut new_count = 0u32;
    for ff in fetched {
        for m in ff.messages {
            let (parsed, atts) = message::parse_raw(&m.raw, id, &ff.folder, m.uid, m.seen, m.flagged);
            if store.contains(&tree, &parsed.id)? {
                continue;
            }
            for (i, a) in atts.iter().enumerate() {
                store.put_blob(&att_tree, &format!("{}:{}", parsed.id, i), &a.data)?;
            }
            store.put_json(&tree, &parsed.id, &parsed)?;
            new_count += 1;
        }
    }
    store.flush();
    Ok(new_count)
}

/// Догрузка старых писем папки (пагинация >50).
pub fn load_older(store: &MailStore, id: &str, folder: &str, before_uid: u32, count: u32) -> Result<u32, CoreError> {
    let mut acc = get_account(store, id)?;
    if acc.is_p2p() {
        return Ok(0);
    }
    ensure_token(store, &mut acc)?;
    let older = imap_client::fetch_older(&acc, folder, before_uid, count)?;
    let tree = msgs_tree(id);
    let att_tree = atts_tree(id);
    let mut new_count = 0u32;
    for m in older {
        let (parsed, atts) = message::parse_raw(&m.raw, id, folder, m.uid, m.seen, m.flagged);
        if store.contains(&tree, &parsed.id)? {
            continue;
        }
        for (i, a) in atts.iter().enumerate() {
            store.put_blob(&att_tree, &format!("{}:{}", parsed.id, i), &a.data)?;
        }
        store.put_json(&tree, &parsed.id, &parsed)?;
        new_count += 1;
    }
    store.flush();
    Ok(new_count)
}

/// Вложение по индексу: (meta, bytes).
pub fn get_attachment(store: &MailStore, id: &str, mid: &str, idx: usize) -> Result<(message::AttachmentMeta, Vec<u8>), CoreError> {
    let msg: StoredMessage = store
        .get_json(&msgs_tree(id), mid)?
        .ok_or(CoreError::NotFound("message"))?;
    let meta = msg.attachments.get(idx).cloned().ok_or(CoreError::NotFound("attachment"))?;
    let data = store
        .get_blob(&atts_tree(id), &format!("{mid}:{idx}"))?
        .ok_or(CoreError::NotFound("attachment data"))?;
    Ok((meta, data))
}

pub fn list_messages(store: &MailStore, id: &str) -> Result<Vec<StoredMessage>, CoreError> {
    let msgs: Vec<StoredMessage> = store.list_json(&msgs_tree(id))?;
    Ok(msgs.iter().map(|m| m.summary()).collect())
}

pub fn get_message(store: &MailStore, id: &str, mid: &str) -> Result<StoredMessage, CoreError> {
    store
        .get_json(&msgs_tree(id), mid)?
        .ok_or(CoreError::NotFound("message"))
}

pub fn patch_message(store: &MailStore, id: &str, mid: &str, patch: MessagePatch) -> Result<StoredMessage, CoreError> {
    let tree = msgs_tree(id);
    let mut msg: StoredMessage = store
        .get_json(&tree, mid)?
        .ok_or(CoreError::NotFound("message"))?;
    let mut moved_to_trash = false;
    if let Some(r) = patch.read {
        msg.read = r;
    }
    if let Some(s) = patch.starred {
        msg.starred = s;
    }
    if let Some(f) = patch.folder {
        moved_to_trash = f == "trash" && msg.folder != "trash";
        msg.folder = f;
    }
    store.put_json(&tree, mid, &msg)?;
    // Пуш на IMAP-сервер (best-effort: офлайн не должен ломать локальные действия)
    if let Some((srv_folder, uid)) = imap_ref(mid) {
        if let Ok(mut acc) = get_account(store, id) {
            if ensure_token(store, &mut acc).is_ok() {
                if let Some(r) = patch.read {
                    if let Err(e) = imap_client::set_flag(&acc, srv_folder, uid, "\\Seen", r) {
                        eprintln!("[flags] push \\Seen failed for {mid}: {e}");
                    }
                }
                if let Some(s) = patch.starred {
                    if let Err(e) = imap_client::set_flag(&acc, srv_folder, uid, "\\Flagged", s) {
                        eprintln!("[flags] push \\Flagged failed for {mid}: {e}");
                    }
                }
                if moved_to_trash {
                    // серверное перемещение в папку Trash (COPY + \Deleted + EXPUNGE)
                    if let Err(e) = imap_client::move_to_trash(&acc, srv_folder, uid) {
                        eprintln!("[trash] server move failed for {mid}: {e}");
                    }
                }
            }
        }
    }
    Ok(msg.summary())
}

pub fn delete_message(store: &MailStore, id: &str, mid: &str) -> Result<(), CoreError> {
    // Локальное удаление (на сервере письмо уже в Trash после перемещения в корзину).
    let msg: Option<StoredMessage> = store.get_json(&msgs_tree(id), mid)?;
    if let Some(m) = msg {
        for i in 0..m.attachments.len() {
            store.delete(&atts_tree(id), &format!("{mid}:{i}"))?;
        }
    }
    store.delete(&msgs_tree(id), mid)?;
    Ok(())
}

/// SMTP-отправка + локальная копия в Sent. При req.encrypt — NTFRY-конверт;
/// иначе, если аккаунт привязан — заголовок X-Ntfry-Sig (Anti-Spoofing).
pub fn send_message(store: &MailStore, mnemonic: &str, id: &str, req: SendRequest) -> Result<StoredMessage, CoreError> {
    let mut acc = get_account(store, id)?;
    ensure_token(store, &mut acc)?;
    use base64::Engine;
    let atts: Vec<message::AttachmentData> = req
        .attachments
        .iter()
        .map(|a| {
            let data = base64::engine::general_purpose::STANDARD
                .decode(&a.data_base64)
                .map_err(|e| CoreError::Msg(format!("bad attachment base64: {e}")))?;
            Ok(message::AttachmentData {
                meta: message::AttachmentMeta {
                    name: a.name.clone(),
                    mime: if a.mime.is_empty() { "application/octet-stream".into() } else { a.mime.clone() },
                    size: data.len(),
                },
                data,
            })
        })
        .collect::<Result<_, CoreError>>()?;

    // Жёсткий лимит суммарного размера вложений: 25 МБ (совпадает с UI-ограничением
    // Dropzone). Для inline-упаковки в NTFRY-конверт этот лимит важен, чтобы не
    // раздувать одно письмо и оставаться в пределах SMTP-квот большинства провайдеров.
    const MAX_ATTACHMENTS_BYTES: usize = 25 * 1024 * 1024;
    let total_bytes: usize = atts.iter().map(|a| a.data.len()).sum();
    if total_bytes > MAX_ATTACHMENTS_BYTES {
        return Err(CoreError::Msg(format!(
            "Суммарный размер вложений {:.1} МБ превышает лимит 25 МБ",
            total_bytes as f64 / 1024.0 / 1024.0
        )));
    }

    let master = mail_crypto::keys::master_from_secret(mnemonic);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if req.to.trim().to_lowercase().ends_with("@sth") {
        return send_p2p_message(store, &master, mnemonic, id, &acc, req, atts, now);
    }
    if acc.is_p2p() {
        return Err(CoreError::Msg(
            "С аккаунта SmartNet P2P можно отправлять только на адреса …@sth (Pure P2P)".into(),
        ));
    }

    if req.encrypt {
        use mail_crypto::envelope::{self, RecipientKey};
        let rcpt_emails: Vec<String> = req
            .to
            .split(',')
            .map(|s| {
                let s = s.trim();
                s.rsplit('<').next().unwrap_or(s).trim_end_matches('>').trim().to_string()
            })
            .filter(|s| !s.is_empty())
            .collect();
        if rcpt_emails.is_empty() {
            return Err(CoreError::Msg("не указан получатель".into()));
        }
        let mut rcpt_keys = Vec::with_capacity(rcpt_emails.len() + 1);
        for e in &rcpt_emails {
            // Local Trust Layer: ключ из адресной книги — чейн не опрашивается
            let x_hex = match crate::contacts::get(store, e) {
                Some(c) if !c.x25519_hex.is_empty() => c.x25519_hex,
                Some(c) if !c.sth_address.is_empty() => {
                    // Контакт закрепил адрес (короткая форма) — ключи из чейна со сверкой
                    let entry = crate::crypto_mail::lookup_email(store, e, false)?.ok_or_else(|| {
                        CoreError::Msg(format!("Контакт {e} закреплён локально, но не найден в реестре блокчейна"))
                    })?;
                    if entry.address != c.sth_address {
                        return Err(CoreError::Msg(format!(
                            "⚠ Адрес {e} в блокчейне ({}) НЕ совпадает с локальным контактом ({}) — возможен сквоттинг, отправка отменена",
                            entry.address, c.sth_address
                        )));
                    }
                    entry.x25519_hex
                }
                _ => {
                    // Холодный контакт: Discovery через блокчейн-реестр
                    let entry = crate::crypto_mail::lookup_email(store, e, false)?.ok_or_else(|| {
                        CoreError::Msg(format!("Получатель {e} не привязан к SmartNet — шифрование невозможно. Отправьте обычным письмом."))
                    })?;
                    if entry.conflict {
                        return Err(CoreError::Msg(format!("⚠ Привязка {e} в конфликте — шифрование небезопасно")));
                    }
                    entry.x25519_hex
                }
            };
            let x: [u8; 32] = hex::decode(&x_hex)
                .ok()
                .and_then(|v| v.try_into().ok())
                .ok_or_else(|| CoreError::Msg("битый ключ контакта/реестра".into()))?;
            let h32 = crate::crypto_mail::email_h32_cached(store, e)?;
            rcpt_keys.push(RecipientKey { hint: mail_crypto::hash::rcpt_hint(&h32), x25519: x });
        }
        // Собственный слот — чтобы читать свою копию из Sent на любом устройстве
        let self_h32 = crate::crypto_mail::email_h32_cached(store, &acc.email)?;
        rcpt_keys.push(RecipientKey {
            hint: mail_crypto::hash::rcpt_hint(&self_h32),
            x25519: mail_crypto::keys::x25519_public(&mail_crypto::keys::derive_x25519(&master)),
        });
        let content = envelope::EnvelopeContent {
            subject: req.subject.clone(),
            text: req.body.clone(),
            html: None,
            attachments: atts
                .iter()
                .map(|a| envelope::Attachment {
                    filename: a.meta.name.clone(),
                    mime: a.meta.mime.clone(),
                    data_b64: base64::engine::general_purpose::STANDARD.encode(&a.data),
                })
                .collect(),
            ts: now,
            in_reply_to: None,
            reply_x25519_hex: None,
            reply_iroh_hex: None,
        };
        let payload = envelope::seal(&content, &master, &rcpt_keys)?;
        let armored = mail_crypto::armor::armor_body(&payload);
        for e in &rcpt_emails {
            smtp_client::send_mail(&acc, e, "🔒 [SmartNet Encrypted]", &armored, &[], &[])?;
        }
        return store_sent_copy(store, id, &acc, req, atts, Some(armored), "smtp");
    }

    // Обычное письмо: подписываем, если аккаунт привязан (confirmed)
    let mut headers: Vec<(String, String)> = Vec::new();
    let bound = store
        .get_json::<crate::crypto_mail::BindingState>("bindings", id)?
        .map(|b| b.status == "confirmed")
        .unwrap_or(false);
    if bound {
        let hdr = mail_crypto::sig::sign_message(
            &master,
            &acc.email,
            &req.to,
            &req.subject,
            now,
            &crate::crypto_mail::norm_body(&req.body),
        )?;
        headers.push((mail_crypto::sig::HEADER_NAME.to_string(), hdr));
    }
    smtp_client::send_mail(&acc, &req.to, &req.subject, &req.body, &atts, &headers)?;
    store_sent_copy(store, id, &acc, req, atts, None, "smtp")
}

/// Прямая P2P-доставка на <SADDR>@sth по Iroh с fallback на SMTP
/// (если у получателя есть локальный контакт с обычным email).
#[allow(clippy::too_many_arguments)]
fn send_p2p_message(
    store: &MailStore,
    master: &[u8; 32],
    _mnemonic: &str,
    id: &str,
    acc: &MailAccount,
    req: SendRequest,
    atts: Vec<message::AttachmentData>,
    now: u64,
) -> Result<StoredMessage, CoreError> {
    use base64::Engine;
    use mail_crypto::envelope::{self, RecipientKey};
    let to_raw = req.to.trim().to_string();
    let local = to_raw[..to_raw.len() - "@sth".len()].trim().to_string();

    // Ключи получателя: локальный контакт (Local Trust) → реестр по адресу
    let norm_to = mail_crypto::hash::normalize_email(&to_raw);
    let contact = crate::contacts::get(store, &norm_to);
    // Получатель: STH-адрес, закреплённый адрес контакта или имя Naming Service (u://имя@sth)
    let addr = if crate::contacts::valid_addr(&local) {
        local
    } else if let Some(a) = contact.as_ref().map(|c| c.sth_address.clone()).filter(|a| !a.is_empty()) {
        a
    } else if let Some(name) = crate::naming::validate_name(&local) {
        crate::naming::resolve(store, &name)?.ok_or_else(|| {
            CoreError::Msg(format!(
                "Имя u://{name} не найдено в Naming Service — отправка на {name}@sth невозможна"
            ))
        })?
    } else {
        return Err(CoreError::Msg(format!(
            "Некорректный P2P-получатель «{local}@sth»: ожидается STH-адрес или имя u://"
        )));
    };
    let (x_hex, node_hex) = match &contact {
        Some(c) if !c.x25519_hex.is_empty() && !c.iroh_node_hex.is_empty() => {
            (c.x25519_hex.clone(), c.iroh_node_hex.clone())
        }
        _ => {
            let by_addr = |idx: &crate::chain::RegistryIndex| {
                idx.entries
                    .values()
                    .find(|e| e.address == addr)
                    .map(|e| (e.x25519_hex.clone(), e.iroh_node_hex.clone()))
            };
            let mut found = by_addr(&crate::crypto_mail::registry_load(store));
            if found.is_none() {
                // кэш пуст/устарел — одна попытка досинхронизации реестра
                found = by_addr(&crate::crypto_mail::registry_sync(store)?);
            }
            found.ok_or_else(|| {
                CoreError::Msg(format!(
                    "Ключи для {addr} не найдены. Попросите получателя прислать вам карточку контакта (Oxid Mail → Контакты → «Моя карточка», строка sn://add-email?…) и добавьте её в контакты. Либо пусть он напишет вам первым — его ключи для ответа сохранятся автоматически. Email-аккаунты появляются в реестре после привязки email"
                ))
            })?
        }
    };
    let x: [u8; 32] = hex::decode(&x_hex)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| CoreError::Msg("битый ключ контакта/реестра".into()))?;

    let mut rcpt_keys = vec![RecipientKey { hint: [0u8; 8], x25519: x }];
    // Свой слот — читаем свою копию (в т.ч. при SMTP-fallback через IMAP)
    let self_h32 = crate::crypto_mail::email_h32_cached(store, &acc.email)?;
    rcpt_keys.push(RecipientKey {
        hint: mail_crypto::hash::rcpt_hint(&self_h32),
        x25519: mail_crypto::keys::x25519_public(&mail_crypto::keys::derive_x25519(master)),
    });
    let content = envelope::EnvelopeContent {
        subject: req.subject.clone(),
        text: req.body.clone(),
        html: None,
        attachments: atts
            .iter()
            .map(|a| envelope::Attachment {
                filename: a.meta.name.clone(),
                mime: a.meta.mime.clone(),
                data_b64: base64::engine::general_purpose::STANDARD.encode(&a.data),
            })
            .collect(),
        ts: now,
        in_reply_to: None,
        // Reply-ключи: получатель автоматически сохранит контакт и сможет ответить
        reply_x25519_hex: Some(hex::encode(mail_crypto::keys::x25519_public(
            &mail_crypto::keys::derive_x25519(master),
        ))),
        reply_iroh_hex: Some(hex::encode(mail_crypto::keys::iroh_node_id(master))),
    };
    let payload = envelope::seal(&content, master, &rcpt_keys)?;
    let armored = mail_crypto::armor::armor_body(&payload);

    // Регистрируем прогресс P2P-раздачи (если UI прислал progressId).
    // Прогресс отдаётся UI отдельным HTTP/IPC-каналом, пока идёт block_on Iroh.
    let progress_id = req.progress_id.clone();
    if let Some(pid) = progress_id.as_deref() {
        let file_sizes: Vec<u64> = atts.iter().map(|a| a.data.len() as u64).collect();
        crate::p2p::init_progress(pid, payload.len() as u64, file_sizes);
    }

    match crate::p2p::send_with_progress(&node_hex, payload.clone(), progress_id.clone()) {
        Ok(()) => {
            if let Some(pid) = progress_id.as_deref() {
                crate::p2p::finish_progress(pid, true, None);
            }
            store_sent_copy(store, id, acc, req, atts, Some(armored), "p2p")
        }
        Err(p2p_err) => {
            // SMTP-fallback: контакт с тем же адресом и обычным email (не для P2P-аккаунта — у него нет SMTP)
            let fb = if acc.is_p2p() {
                None
            } else {
                crate::contacts::list(store)?
                    .into_iter()
                    .find(|c| c.sth_address == addr && !c.email.ends_with("@sth") && c.email.contains('@'))
            };
            if let Some(fb) = fb {
                smtp_client::send_mail(acc, &fb.email, "🔒 [SmartNet Encrypted]", &armored, &[], &[])?;
                if let Some(pid) = progress_id.as_deref() {
                    // SMTP-fallback: доставка не через Iroh, но конверт ушёл — отмечаем как done+ok
                    // с пометкой в error, чтобы UI мог показать «через SMTP».
                    crate::p2p::finish_progress(pid, true, Some(format!("smtp-fallback: {p2p_err}")));
                }
                store_sent_copy(store, id, acc, req, atts, Some(armored), "smtp")
            } else {
                if let Some(pid) = progress_id.as_deref() {
                    crate::p2p::finish_progress(pid, false, Some(p2p_err.clone()));
                }
                Err(CoreError::Msg(format!(
                    "P2P-доставка не удалась ({p2p_err}). SMTP-fallback недоступен: нет контакта с обычным email для {addr}"
                )))
            }
        }
    }
}

fn store_sent_copy(
    store: &MailStore,
    id: &str,
    acc: &MailAccount,
    req: SendRequest,
    atts: Vec<message::AttachmentData>,
    sealed: Option<String>, // NTFRY-armor: тело хранится конвертом, читается через self-слот
    via: &str,              // "p2p" | "smtp"
) -> Result<StoredMessage, CoreError> {
    let encrypted = sealed.is_some();
    let msg = StoredMessage {
        id: format!("local_{}", now_ms()),
        account_id: id.to_string(),
        folder: "sent".into(),
        from: message::Address {
            name: "You".into(),
            email: acc.email.clone(),
        },
        to: req.to,
        subject: req.subject,
        snippet: req
            .body
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(140)
            .collect(),
        date: message::now_rfc3339(),
        read: true,
        starred: false,
        encrypted,
        labels: vec![],
        attachments: atts.iter().map(|a| a.meta.clone()).collect(),
        body: Some(sealed.unwrap_or(req.body)),
        body_html: None,
        ntfry_sig: None,
        via: Some(via.to_string()),
    };
    for (i, a) in atts.iter().enumerate() {
        store.put_blob(&atts_tree(id), &format!("{}:{}", msg.id, i), &a.data)?;
    }
    store.put_json(&msgs_tree(id), &msg.id, &msg)?;
    store.flush();
    Ok(msg)
}

// ---------- PIN-защита seed (как vault SmartNet) ----------

/// Шифрует мнемонику PIN-кодом → base64-блоб для хранения на диске.
pub fn pin_seal(mnemonic: &str, pin: &str) -> Result<String, CoreError> {
    if pin.trim().len() < 4 {
        return Err(CoreError::Msg("PIN must be at least 4 characters".into()));
    }
    identity::from_secret(mnemonic)?; // непустая фраза (произвольные фразы SmartNet допустимы)
    let cipher = mail_store::Cipher::from_secret(pin.trim(), b"oxid-mail/pin/v1");
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(cipher.seal(mnemonic.trim().as_bytes())))
}

/// Расшифровывает seed-блоб PIN-кодом.
pub fn pin_open(blob_b64: &str, pin: &str) -> Result<String, CoreError> {
    use base64::Engine;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(blob_b64.trim())
        .map_err(|e| CoreError::Msg(format!("bad sealed blob: {e}")))?;
    let cipher = mail_store::Cipher::from_secret(pin.trim(), b"oxid-mail/pin/v1");
    let plain = cipher.open(&blob).map_err(|_| CoreError::Msg("wrong PIN".to_string()))?;
    String::from_utf8(plain).map_err(|e| CoreError::Msg(e.to_string()))
}

// ---------- IMAP IDLE ----------

/// Запустить IDLE-наблюдатели для всех аккаунтов сессии.
pub fn start_watchers(store: &std::sync::Arc<MailStore>) {
    if let Ok(accounts) = store.list_json::<MailAccount>("accounts") {
        for a in accounts {
            if a.is_p2p() {
                continue; // у P2P-аккаунта нет IMAP
            }
            crate::idle::start(store.clone(), a.id);
        }
    }
}
