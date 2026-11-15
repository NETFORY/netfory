// NTFRY-конверт: ECIES (ephemeral X25519 -> HKDF -> XChaCha20-Poly1305) + zstd + подпись secp256k1.
// Layout: "NTFRY1" | eph_pub(32) | n(1) | n×{hint(8) wrap(48)} | sender_pub(33) | nonce(24) | ct | sig(64)
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use secp256k1::{ecdsa::Signature, Message, PublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as XPublic, StaticSecret};

pub const MAGIC: &[u8; 6] = b"NTFRY1";
const INFO_WRAP: &[u8] = b"oxid-mail/wrap/v1";
const SLOT: usize = 8 + 48; // hint + wrapped CEK (32 + 16 tag)
/// Лимит распакованного контента (анти zip-bomb): 64 МБ.
const MAX_PLAIN: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub filename: String,
    pub mime: String,
    pub data_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvelopeContent {
    pub subject: String,
    pub text: String,
    #[serde(default)]
    pub html: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    pub ts: u64,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    /// Ключи отправителя для ответа (hex): x25519-публичный ключ.
    /// Внутри шифртекста и под подписью — получатель может сразу ответить P2P.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_x25519_hex: Option<String>,
    /// Ключи отправителя для ответа (hex): Iroh node ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_iroh_hex: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RecipientKey {
    pub hint: [u8; 8],
    pub x25519: [u8; 32],
}

#[derive(Debug)]
pub struct Opened {
    pub content: EnvelopeContent,
    /// Сжатый secp256k1 pubkey отправителя (33 байта) — для сверки с реестром.
    pub sender_pubkey: [u8; 33],
    pub signature_valid: bool,
}

fn wrap_key(shared: &[u8; 32], eph_pub: &[u8; 32]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(eph_pub), shared);
    let mut okm = [0u8; 32];
    hk.expand(INFO_WRAP, &mut okm).expect("hkdf wrap");
    okm
}

pub fn seal(
    content: &EnvelopeContent,
    sender_secret: &[u8; 32],
    recipients: &[RecipientKey],
) -> Result<Vec<u8>, String> {
    if recipients.is_empty() || recipients.len() > 255 {
        return Err("recipients must be 1..=255".into());
    }
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(sender_secret).map_err(|e| e.to_string())?;
    let sender_pub = PublicKey::from_secret_key(&secp, &sk).serialize();

    let eph = StaticSecret::random_from_rng(OsRng);
    let eph_pub = XPublic::from(&eph).to_bytes();
    let mut cek = [0u8; 32];
    OsRng.fill_bytes(&mut cek);

    let mut buf = Vec::with_capacity(256 + content.text.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&eph_pub);
    buf.push(recipients.len() as u8);
    let zero_nonce = XNonce::from([0u8; 24]); // ключ уникален на (eph, rcpt) — zero nonce безопасен
    for r in recipients {
        let shared = eph.diffie_hellman(&XPublic::from(r.x25519)).to_bytes();
        let cipher = XChaCha20Poly1305::new((&wrap_key(&shared, &eph_pub)).into());
        let wrap = cipher.encrypt(&zero_nonce, cek.as_slice()).map_err(|e| e.to_string())?;
        buf.extend_from_slice(&r.hint);
        buf.extend_from_slice(&wrap); // 48
    }
    buf.extend_from_slice(&sender_pub);

    let json = serde_json::to_vec(content).map_err(|e| e.to_string())?;
    let packed = zstd::encode_all(json.as_slice(), 10).map_err(|e| e.to_string())?;
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    buf.extend_from_slice(&nonce);
    let cipher = XChaCha20Poly1305::new((&cek).into());
    let ct = cipher
        .encrypt(&XNonce::from(nonce), packed.as_slice())
        .map_err(|e| e.to_string())?;
    buf.extend_from_slice(&ct);

    let digest = Sha256::digest(&buf);
    let msg = Message::from_digest_slice(&digest).map_err(|e| e.to_string())?;
    let sig = secp.sign_ecdsa(&msg, &sk).serialize_compact();
    buf.extend_from_slice(&sig);
    Ok(buf)
}

pub fn is_ntfry(payload: &[u8]) -> bool {
    payload.len() > 6 && &payload[..6] == MAGIC
}

pub fn open(
    payload: &[u8],
    my_x25519: &StaticSecret,
    my_hint: Option<[u8; 8]>,
) -> Result<Opened, String> {
    if payload.len() < 6 + 32 + 1 + SLOT + 33 + 24 + 16 + 64 {
        return Err("payload too short".into());
    }
    if &payload[..6] != MAGIC {
        return Err("not an NTFRY payload".into());
    }
    let mut off = 6;
    let eph_pub: [u8; 32] = payload[off..off + 32].try_into().unwrap();
    off += 32;
    let n = payload[off] as usize;
    off += 1;
    if n == 0 || payload.len() < off + n * SLOT + 33 + 24 + 16 + 64 {
        return Err("malformed slots".into());
    }
    let slots = &payload[off..off + n * SLOT];
    off += n * SLOT;
    let sender_pubkey: [u8; 33] = payload[off..off + 33].try_into().unwrap();
    off += 33;
    let nonce: [u8; 24] = payload[off..off + 24].try_into().unwrap();
    off += 24;
    let ct = &payload[off..payload.len() - 64];
    let sig_bytes = &payload[payload.len() - 64..];

    let secp = Secp256k1::verification_only();
    let digest = Sha256::digest(&payload[..payload.len() - 64]);
    let signature_valid = (|| {
        let msg = Message::from_digest_slice(&digest).ok()?;
        let sig = Signature::from_compact(sig_bytes).ok()?;
        let pk = PublicKey::from_slice(&sender_pubkey).ok()?;
        secp.verify_ecdsa(&msg, &sig, &pk).ok()
    })()
    .is_some();

    let shared = my_x25519.diffie_hellman(&XPublic::from(eph_pub)).to_bytes();
    let wk = wrap_key(&shared, &eph_pub);
    let wrap_cipher = XChaCha20Poly1305::new((&wk).into());
    let zero_nonce = XNonce::from([0u8; 24]);
    // Сначала слоты с совпавшим hint, затем все остальные (hint — только оптимизация).
    let mut order: Vec<usize> = Vec::with_capacity(n);
    if let Some(h) = my_hint {
        order.extend((0..n).filter(|i| slots[i * SLOT..i * SLOT + 8] == h));
    }
    let rest: Vec<usize> = (0..n).filter(|i| !order.contains(i)).collect();
    order.extend(rest);
    let mut cek: Option<[u8; 32]> = None;
    for i in order {
        let wrap = &slots[i * SLOT + 8..(i + 1) * SLOT];
        if let Ok(k) = wrap_cipher.decrypt(&zero_nonce, wrap) {
            if k.len() == 32 {
                cek = Some(k.try_into().unwrap());
                break;
            }
        }
    }
    let cek = cek.ok_or("no slot for this key (письмо адресовано не этому ключу)")?;

    let cipher = XChaCha20Poly1305::new((&cek).into());
    let packed = cipher
        .decrypt(&XNonce::from(nonce), ct)
        .map_err(|_| "content decrypt failed".to_string())?;
    let mut json = Vec::new();
    let mut dec = zstd::Decoder::new(packed.as_slice()).map_err(|e| e.to_string())?;
    std::io::Read::take(&mut dec, MAX_PLAIN as u64 + 1)
        .read_to_end(&mut json)
        .map_err(|e| e.to_string())?;
    if json.len() > MAX_PLAIN {
        return Err("decompressed content too large".into());
    }
    let content: EnvelopeContent = serde_json::from_slice(&json).map_err(|e| e.to_string())?;
    Ok(Opened { content, sender_pubkey, signature_valid })
}

use std::io::Read;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;

    fn content() -> EnvelopeContent {
        EnvelopeContent {
            subject: "Секретная тема".into(),
            text: "Привет, это шифрованное письмо SmartNet!".into(),
            html: Some("<b>Привет</b>".into()),
            attachments: vec![Attachment {
                filename: "a.txt".into(),
                mime: "text/plain".into(),
                data_b64: "aGVsbG8=".into(),
            }],
            ts: 1_760_000_000,
            in_reply_to: None,
            reply_x25519_hex: Some("aa".repeat(32)),
            reply_iroh_hex: Some("bb".repeat(32)),
        }
    }

    #[test]
    fn seal_open_roundtrip_multi_recipient() {
        let sender_master = keys::master_from_secret("sender phrase");
        let r1 = keys::master_from_secret("rcpt one");
        let r2 = keys::master_from_secret("rcpt two");
        let r1_sk = keys::derive_x25519(&r1);
        let r2_sk = keys::derive_x25519(&r2);
        let rcpts = vec![
            RecipientKey { hint: [1; 8], x25519: keys::x25519_public(&r1_sk) },
            RecipientKey { hint: [2; 8], x25519: keys::x25519_public(&r2_sk) },
        ];
        let payload = seal(&content(), &sender_master, &rcpts).unwrap();
        assert!(is_ntfry(&payload));

        let o1 = open(&payload, &r1_sk, Some([1; 8])).unwrap();
        assert!(o1.signature_valid);
        assert_eq!(o1.content, content());
        // второй получатель без hint (полный перебор слотов)
        let o2 = open(&payload, &r2_sk, None).unwrap();
        assert_eq!(o2.content.text, content().text);
        // посторонний ключ не расшифрует
        let stranger = keys::derive_x25519(&keys::master_from_secret("stranger"));
        assert!(open(&payload, &stranger, None).is_err());
    }

    #[test]
    fn tamper_breaks_signature_or_content() {
        let sender = keys::master_from_secret("sender");
        let r = keys::derive_x25519(&keys::master_from_secret("rcpt"));
        let rcpts = vec![RecipientKey { hint: [7; 8], x25519: keys::x25519_public(&r) }];
        let mut payload = seal(&content(), &sender, &rcpts).unwrap();
        // порча подписи -> signature_valid = false, контент читается
        let last = payload.len() - 1;
        payload[last] ^= 0xff;
        let o = open(&payload, &r, None).unwrap();
        assert!(!o.signature_valid);
        // порча шифротекста -> ошибка AEAD
        let mid = payload.len() - 100;
        payload[mid] ^= 0xff;
        assert!(open(&payload, &r, None).is_err());
    }
}
