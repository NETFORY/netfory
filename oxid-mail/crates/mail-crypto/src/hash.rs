// Email -> H32/H16/hint. Анти-брутфорс индекс реестра (Argon2id -> BLAKE3).
use argon2::{Algorithm, Argon2, Params, Version};

pub const ARGON_M_KIB: u32 = 131_072; // 128 MiB — протокольная константа v1
pub const ARGON_T: u32 = 3;
pub const ARGON_P: u32 = 1;
const SALT_INPUT: &[u8] = b"SmartNet-OxidMail-Registry-v1";
const INDEX_CONTEXT: &str = "oxid-mail-index-v1";
const HINT_CONTEXT: &str = "oxid-mail-hint-v1";

pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

fn registry_salt() -> [u8; 16] {
    let h = blake3::hash(SALT_INPUT);
    let mut s = [0u8; 16];
    s.copy_from_slice(&h.as_bytes()[..16]);
    s
}

/// Полный 32-байтный хэш email (тяжёлый: ~0.5с). Кэшировать вызывающей стороне.
pub fn email_h32(email: &str) -> [u8; 32] {
    let params = Params::new(ARGON_M_KIB, ARGON_T, ARGON_P, Some(32)).expect("valid argon2 params");
    let a = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut k = [0u8; 32];
    a.hash_password_into(normalize_email(email).as_bytes(), &registry_salt(), &mut k)
        .expect("argon2 hashing");
    blake3::derive_key(INDEX_CONTEXT, &k)
}

/// On-chain индекс (vendorField): первые 16 байт H32.
pub fn h16(h32: &[u8; 32]) -> [u8; 16] {
    let mut r = [0u8; 16];
    r.copy_from_slice(&h32[..16]);
    r
}

/// Слот-hint внутри NTFRY-конверта. Отдельный домен BLAKE3 —
/// НЕ связывается с on-chain H16 третьими лицами.
pub fn rcpt_hint(h32: &[u8; 32]) -> [u8; 8] {
    let d = blake3::derive_key(HINT_CONTEXT, h32);
    let mut r = [0u8; 8];
    r.copy_from_slice(&d[..8]);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_normalized() {
        let a = email_h32("  User@Gmail.COM ");
        let b = email_h32("user@gmail.com");
        assert_eq!(a, b);
        let c = email_h32("other@gmail.com");
        assert_ne!(a, c);
    }

    #[test]
    fn hint_not_prefix_of_h16() {
        let h32 = email_h32("user@gmail.com");
        let idx = h16(&h32);
        let hint = rcpt_hint(&h32);
        assert_ne!(&idx[..8], &hint[..]);
    }
}
