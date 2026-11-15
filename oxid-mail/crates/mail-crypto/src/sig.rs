// X-Ntfry-Sig: подпись обычных (нешифрованных) писем для Anti-Spoofing бейджа.
// Формат заголовка: v1; pk=<b64>; ts=<unix>; sig=<b64 compact ECDSA>
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use secp256k1::{ecdsa::Signature, Message, PublicKey, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};

pub const HEADER_NAME: &str = "X-Ntfry-Sig";
const DOMAIN: &[u8] = b"oxid-mail/sig/v1";

#[derive(Debug, Clone)]
pub struct VerifiedSig {
    pub pubkey: [u8; 33],
    pub ts: u64,
}

fn canon_digest(from: &str, to: &str, subject: &str, ts: u64, body: &[u8]) -> [u8; 32] {
    let bh = blake3::hash(body);
    let mut h = Sha256::new();
    for part in [
        DOMAIN,
        from.trim().to_lowercase().as_bytes(),
        to.trim().to_lowercase().as_bytes(),
        subject.trim().as_bytes(),
        ts.to_string().as_bytes(),
        bh.as_bytes(),
    ] {
        h.update(part);
        h.update([0u8]);
    }
    h.finalize().into()
}

pub fn sign_message(
    sender_secret: &[u8; 32],
    from: &str,
    to: &str,
    subject: &str,
    ts: u64,
    body: &[u8],
) -> Result<String, String> {
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(sender_secret).map_err(|e| e.to_string())?;
    let pk = PublicKey::from_secret_key(&secp, &sk).serialize();
    let msg = Message::from_digest_slice(&canon_digest(from, to, subject, ts, body))
        .map_err(|e| e.to_string())?;
    let sig = secp.sign_ecdsa(&msg, &sk).serialize_compact();
    Ok(format!("v1; pk={}; ts={ts}; sig={}", B64.encode(pk), B64.encode(sig)))
}

pub fn verify_message(
    header_value: &str,
    from: &str,
    to: &str,
    subject: &str,
    body: &[u8],
) -> Option<VerifiedSig> {
    let mut pk_b64 = None;
    let mut ts: Option<u64> = None;
    let mut sig_b64 = None;
    let mut version_ok = false;
    for part in header_value.split(';') {
        let part = part.trim();
        if part == "v1" {
            version_ok = true;
        } else if let Some(v) = part.strip_prefix("pk=") {
            pk_b64 = Some(v);
        } else if let Some(v) = part.strip_prefix("ts=") {
            ts = v.parse().ok();
        } else if let Some(v) = part.strip_prefix("sig=") {
            sig_b64 = Some(v);
        }
    }
    if !version_ok {
        return None;
    }
    let (pk_b64, ts, sig_b64) = (pk_b64?, ts?, sig_b64?);
    let pk_bytes: [u8; 33] = B64.decode(pk_b64).ok()?.try_into().ok()?;
    let sig_bytes: [u8; 64] = B64.decode(sig_b64).ok()?.try_into().ok()?;
    let secp = Secp256k1::verification_only();
    let msg = Message::from_digest_slice(&canon_digest(from, to, subject, ts, body)).ok()?;
    let sig = Signature::from_compact(&sig_bytes).ok()?;
    let pk = PublicKey::from_slice(&pk_bytes).ok()?;
    secp.verify_ecdsa(&msg, &sig, &pk).ok()?;
    Some(VerifiedSig { pubkey: pk_bytes, ts })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;

    #[test]
    fn sign_verify_and_tamper() {
        let master = keys::master_from_secret("sender phrase");
        let hdr = sign_message(&master, "User1@Gmail.com", "user2@gmail.com", "Hi", 1_760_000_000, b"body").unwrap();
        // регистронезависимость адресов
        let ok = verify_message(&hdr, "user1@gmail.com", "USER2@gmail.com ", "Hi", b"body").unwrap();
        assert_eq!(ok.ts, 1_760_000_000);
        // подмена темы/тела/адреса ломает подпись
        assert!(verify_message(&hdr, "user1@gmail.com", "user2@gmail.com", "Hi!", b"body").is_none());
        assert!(verify_message(&hdr, "user1@gmail.com", "user2@gmail.com", "Hi", b"evil").is_none());
        assert!(verify_message(&hdr, "evil@gmail.com", "user2@gmail.com", "Hi", b"body").is_none());
        assert!(verify_message("garbage", "a", "b", "c", b"d").is_none());
    }
}
