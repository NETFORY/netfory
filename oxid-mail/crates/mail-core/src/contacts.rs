// Локальная адресная книга (Local Trust Layer) — приоритетнее блокчейн-реестра.
// Типы контактов: plain (обычный email), encrypted (email + ключи/адрес),
// p2p (внутренний адрес <sthAddress>@sth). Обмен: sn://add-email?... URI,
// короткая форма "SADDR:email", p2p-адрес "SADDR@sth" или просто email.
use serde::{Deserialize, Serialize};

use crate::service::CoreError;
use mail_crypto::{hash, keys};
use mail_store::MailStore;

const TREE: &str = "contacts";
pub const P2P_SUFFIX: &str = "@sth";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    /// Ключ книги: обычный email или "<sthAddress>@sth" для p2p
    pub email: String,
    /// plain | encrypted | p2p
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub sth_address: String,
    /// Пусто — ключи берутся из чейна, но адрес сверяется с контактом
    #[serde(default)]
    pub x25519_hex: String,
    #[serde(default)]
    pub iroh_node_hex: String,
    #[serde(default)]
    pub added: u64,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn valid_addr(a: &str) -> bool {
    a.len() == 34 && a.starts_with('S') && bs58::decode(a).with_check(None).into_vec().is_ok()
}

fn validate(c: &Contact) -> Result<(), CoreError> {
    match c.kind.as_str() {
        "plain" => {
            if !c.email.contains('@') {
                return Err(CoreError::Msg("некорректный email контакта".into()));
            }
        }
        "encrypted" => {
            if !c.email.contains('@') {
                return Err(CoreError::Msg("некорректный email контакта".into()));
            }
            if !valid_addr(&c.sth_address) {
                return Err(CoreError::Msg("некорректный STH-адрес контакта".into()));
            }
        }
        "p2p" => {
            if !valid_addr(&c.sth_address) {
                return Err(CoreError::Msg("некорректный STH-адрес p2p-контакта".into()));
            }
        }
        _ => return Err(CoreError::Msg("неизвестный тип контакта".into())),
    }
    for h in [&c.x25519_hex, &c.iroh_node_hex] {
        if !h.is_empty() && hex::decode(h).map(|v| v.len() != 32).unwrap_or(true) {
            return Err(CoreError::Msg("некорректный публичный ключ контакта".into()));
        }
    }
    Ok(())
}

pub fn list(store: &MailStore) -> Result<Vec<Contact>, CoreError> {
    let mut out: Vec<Contact> = store.list_json::<Contact>(TREE)?;
    out.sort_by(|a, b| a.email.cmp(&b.email));
    Ok(out)
}

pub fn get(store: &MailStore, email: &str) -> Option<Contact> {
    store
        .get_json::<Contact>(TREE, &hash::normalize_email(email))
        .ok()
        .flatten()
}

pub fn add(store: &MailStore, mut c: Contact) -> Result<Contact, CoreError> {
    if c.kind == "p2p" && c.email.is_empty() {
        c.email = format!("{}{P2P_SUFFIX}", c.sth_address);
    }
    c.email = hash::normalize_email(&c.email);
    if c.kind.is_empty() {
        c.kind = if c.email.ends_with(P2P_SUFFIX) {
            "p2p".into()
        } else if !c.x25519_hex.is_empty() || !c.sth_address.is_empty() {
            "encrypted".into()
        } else {
            "plain".into()
        };
    }
    validate(&c)?;
    if c.added == 0 {
        c.added = now_secs();
    }
    store.put_json(TREE, &c.email, &c)?;
    store.flush();
    Ok(c)
}

pub fn remove(store: &MailStore, email: &str) -> Result<(), CoreError> {
    store.delete(TREE, &hash::normalize_email(email))?;
    store.flush();
    Ok(())
}

// ---------------------------------------------------------------------------
//  Контактная карточка / URI
// ---------------------------------------------------------------------------

const SCHEME: &str = "sn://add-email?";

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn pct_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hexpair = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hexpair, 16).ok()?);
            i += 3;
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// URI для обмена зашифрованными контактами (email + ключи).
pub fn to_uri(c: &Contact) -> String {
    let mut uri = format!(
        "{SCHEME}v=1&email={}&addr={}",
        pct_encode(&c.email),
        pct_encode(&c.sth_address)
    );
    if !c.x25519_hex.is_empty() {
        uri.push_str(&format!("&x={}", c.x25519_hex));
    }
    if !c.iroh_node_hex.is_empty() {
        uri.push_str(&format!("&node={}", c.iroh_node_hex));
    }
    if !c.name.is_empty() {
        uri.push_str(&format!("&name={}", pct_encode(&c.name)));
    }
    uri
}

/// Универсальный разбор ввода: URI / "SADDR:email" / "SADDR@sth" / обычный email.
pub fn parse_any(input: &str) -> Result<Contact, CoreError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(CoreError::Msg("пустой контакт".into()));
    }
    // Полный URI с ключами
    if let Some(query) = input.strip_prefix(SCHEME) {
        let mut c = Contact::default();
        for pair in query.split('&') {
            let Some((k, v)) = pair.split_once('=') else { continue };
            match k {
                "email" => c.email = pct_decode(v).unwrap_or_default(),
                "addr" => c.sth_address = pct_decode(v).unwrap_or_default(),
                "x" => c.x25519_hex = v.to_lowercase(),
                "node" => c.iroh_node_hex = v.to_lowercase(),
                "name" => c.name = pct_decode(v).unwrap_or_default(),
                _ => {}
            }
        }
        c.email = hash::normalize_email(&c.email);
        c.kind = if c.email.ends_with(P2P_SUFFIX) { "p2p".into() } else { "encrypted".into() };
        validate(&c)?;
        return Ok(c);
    }
    let norm = hash::normalize_email(input);
    // p2p: SADDR@sth (регистр адреса важен — base58, не нормализуем)
    if norm.ends_with(P2P_SUFFIX) {
        let addr = input[..input.len() - P2P_SUFFIX.len()].trim().to_string();
        let c = Contact {
            email: norm.clone(),
            kind: "p2p".into(),
            sth_address: addr,
            ..Default::default()
        };
        validate(&c)?;
        return Ok(c);
    }
    // Короткая форма: SADDR:email (ключи подтянутся из чейна, адрес закреплён)
    if let Some((addr, email)) = input.split_once(':') {
        if valid_addr(addr.trim()) {
            let c = Contact {
                email: hash::normalize_email(email),
                kind: "encrypted".into(),
                sth_address: addr.trim().to_string(),
                ..Default::default()
            };
            validate(&c)?;
            return Ok(c);
        }
    }
    // Обычный email
    if norm.matches('@').count() == 1 && !norm.contains(':') && !norm.contains(' ') && norm.len() > 3 {
        return Ok(Contact { email: norm, kind: "plain".into(), ..Default::default() });
    }
    Err(CoreError::Msg("нераспознанный формат: ожидается email, sn://add-email?..., SADDR:email или SADDR@sth".into()))
}

/// parse_any + резолвинг имени Naming Service: «имя@sth» (u://имя) → адрес владельца.
pub fn parse_any_resolved(store: &MailStore, input: &str) -> Result<Contact, CoreError> {
    let trimmed = input.trim();
    let norm = hash::normalize_email(trimmed);
    if norm.ends_with(P2P_SUFFIX) && !trimmed.starts_with(SCHEME) {
        let raw = trimmed[..trimmed.len() - P2P_SUFFIX.len()].trim();
        if !valid_addr(raw) {
            if let Some(name) = crate::naming::validate_name(raw) {
                let owner = crate::naming::resolve(store, &name)?.ok_or_else(|| {
                    CoreError::Msg(format!(
                        "Имя u://{name} не найдено в Naming Service — проверьте написание или дождитесь подтверждения регистрации"
                    ))
                })?;
                let c = Contact {
                    email: norm,
                    kind: "p2p".into(),
                    name: name.clone(),
                    sth_address: owner,
                    ..Default::default()
                };
                validate(&c)?;
                return Ok(c);
            }
        }
    }
    parse_any(input)
}

/// Моя карточка + три строки для «Поделиться контактом».
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareCard {
    pub contact: Contact,
    /// 1. обычный email
    pub plain: String,
    /// 2. URI с публичными ключами для шифрованной почты
    pub uri: String,
    /// 3. p2p email <sthAddress>@sth
    pub p2p: String,
}

pub fn my_card(mnemonic: &str, email: &str, name: &str) -> Result<ShareCard, CoreError> {
    let ident = crate::identity::from_secret(mnemonic)?;
    let master = keys::master_from_secret(mnemonic);
    let contact = Contact {
        email: hash::normalize_email(email),
        kind: "encrypted".into(),
        name: name.to_string(),
        note: String::new(),
        sth_address: ident.address.clone(),
        x25519_hex: hex::encode(keys::x25519_public(&keys::derive_x25519(&master))),
        iroh_node_hex: hex::encode(keys::iroh_node_id(&master)),
        added: now_secs(),
    };
    let uri = to_uri(&contact);
    Ok(ShareCard {
        plain: contact.email.clone(),
        uri,
        p2p: format!("{}{P2P_SUFFIX}", ident.address),
        contact,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: &str = "Sg2uDbwqm9YPPAa1DMaBmE1c94nuwBhHP9";

    #[test]
    fn parse_all_forms() {
        // обычный email
        let p = parse_any("  User@Gmail.COM ").unwrap();
        assert_eq!((p.kind.as_str(), p.email.as_str()), ("plain", "user@gmail.com"));
        // короткая форма ADDR:email
        let s = parse_any(&format!("{ADDR}:Techno@gmail.com")).unwrap();
        assert_eq!(s.kind, "encrypted");
        assert_eq!(s.sth_address, ADDR);
        assert!(s.x25519_hex.is_empty());
        // p2p
        let p2 = parse_any(&format!("{ADDR}@sth")).unwrap();
        assert_eq!(p2.kind, "p2p");
        assert_eq!(p2.sth_address, ADDR);
        // мусор
        assert!(parse_any("hello world").is_err());
        assert!(parse_any("BADADDR12345678901234567890123456:x@y.z").is_err());
    }

    #[test]
    fn uri_roundtrip_via_my_card() {
        let card = my_card("test phrase", "Techno@Gmail.com", "Кабан").unwrap();
        assert!(card.uri.starts_with("sn://add-email?v=1&email=techno%40gmail.com"));
        assert!(card.p2p.ends_with("@sth"));
        let parsed = parse_any(&card.uri).unwrap();
        assert_eq!(parsed.email, "techno@gmail.com");
        assert_eq!(parsed.kind, "encrypted");
        assert_eq!(parsed.x25519_hex, card.contact.x25519_hex);
        assert_eq!(parsed.name, "Кабан");
    }
}
