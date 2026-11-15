// Детерминированная деривация почтовых ключей из seed-фразы SmartHoldem.
// master = sha256(secret.trim()) — идентично identity.rs / crypto.rs SmartNet.
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as XPublic, StaticSecret};

const INFO_X25519: &[u8] = b"oxid-mail/x25519/v1";
const INFO_IROH: &[u8] = b"oxid-mail/iroh-node/v1";

pub fn master_from_secret(phrase: &str) -> [u8; 32] {
    let d = Sha256::digest(phrase.trim().as_bytes());
    let mut m = [0u8; 32];
    m.copy_from_slice(&d);
    m
}

fn expand(master: &[u8; 32], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, master);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm).expect("hkdf expand 32");
    okm
}

pub fn derive_x25519(master: &[u8; 32]) -> StaticSecret {
    StaticSecret::from(expand(master, INFO_X25519)) // dalek клампит сам
}

pub fn x25519_public(sk: &StaticSecret) -> [u8; 32] {
    XPublic::from(sk).to_bytes()
}

pub fn derive_iroh_signing_key(master: &[u8; 32]) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&expand(master, INFO_IROH))
}

/// Iroh NodeId = ed25519 verifying key (32 байта).
pub fn iroh_node_id(master: &[u8; 32]) -> [u8; 32] {
    derive_iroh_signing_key(master).verifying_key().to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_domain_separated() {
        let m = master_from_secret("  My Custom SmartNet Wallet Phrase 42!  ");
        let m2 = master_from_secret("My Custom SmartNet Wallet Phrase 42!");
        assert_eq!(m, m2);
        let x1 = x25519_public(&derive_x25519(&m));
        let x2 = x25519_public(&derive_x25519(&m));
        assert_eq!(x1, x2);
        let node = iroh_node_id(&m);
        assert_ne!(x1, node);
        let other = master_from_secret("another phrase");
        assert_ne!(x25519_public(&derive_x25519(&other)), x1);
    }
}
