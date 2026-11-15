// BIP-39 идентичность SmartHoldem: адрес = base58check(0x3F ‖ ripemd160(pubkey)),
// приватный ключ = sha256(mnemonic) — идентично crypto.rs SmartNet-клиента.
use bip39::{Language, Mnemonic, MnemonicType};
use ripemd::Ripemd160;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const VERSION_BYTE: u8 = 0x3f; // SmartHoldem pubKeyHash 63

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub mnemonic: String,
    pub address: String,
    pub public_key: String,
}

pub fn normalize(phrase: &str) -> String {
    phrase
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn generate() -> Identity {
    let m = Mnemonic::new(MnemonicType::Words12, Language::English);
    from_mnemonic(m.phrase()).expect("fresh mnemonic is always valid")
}

/// Identity из ПРОИЗВОЛЬНОЙ секретной фразы (без BIP-39 валидации, только trim).
/// Кошелёк SmartNet допускает произвольные фразы; деривация идентична его
/// index-0 аккаунту: privkey = sha256(secret.trim()).
pub fn from_secret(phrase: &str) -> Result<Identity, String> {
    let phrase = phrase.trim().to_string();
    if phrase.is_empty() {
        return Err("empty secret phrase".into());
    }
    let sk_bytes = Sha256::digest(phrase.as_bytes());
    let sk = SecretKey::from_slice(&sk_bytes).map_err(|e| e.to_string())?;
    let secp = Secp256k1::signing_only();
    let pk = PublicKey::from_secret_key(&secp, &sk).serialize();
    Ok(Identity {
        address: address_from_pubkey(&pk),
        public_key: hex::encode(pk),
        mnemonic: phrase,
    })
}

pub fn from_mnemonic(phrase: &str) -> Result<Identity, String> {
    let phrase = normalize(phrase);
    Mnemonic::from_phrase(&phrase, Language::English)
        .map_err(|e| format!("invalid BIP-39 mnemonic: {e}"))?;
    from_secret(&phrase)
}

pub fn address_from_pubkey(pk_comp: &[u8]) -> String {
    let mut rip = Ripemd160::new();
    rip.update(pk_comp);
    let hash = rip.finalize();
    let mut payload = Vec::with_capacity(21);
    payload.push(VERSION_BYTE);
    payload.extend_from_slice(&hash);
    bs58::encode(payload).with_check().into_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identity_is_stable_and_sth_prefixed() {
        let id = generate();
        assert!(id.address.starts_with('S'));
        assert_eq!(id.mnemonic.split(' ').count(), 12);
        let again = from_mnemonic(&id.mnemonic).unwrap();
        assert_eq!(again.address, id.address);
        assert_eq!(again.public_key, id.public_key);
    }

    #[test]
    fn normalization_is_case_and_space_insensitive() {
        let id = generate();
        let messy = format!("  {}  ", id.mnemonic.to_uppercase().replace(' ', "   "));
        assert_eq!(from_mnemonic(&messy).unwrap().address, id.address);
    }

    #[test]
    fn arbitrary_secret_phrase_accepted() {
        // SmartNet допускает произвольные фразы — from_secret без BIP-39 валидации
        let id = from_secret("  My Custom Wallet Phrase 42!  ").unwrap();
        assert!(id.address.starts_with('S'));
        assert_eq!(id.mnemonic, "My Custom Wallet Phrase 42!");
        // регистр значим (деривация идентична SmartNet: sha256(secret.trim()))
        let other = from_secret("my custom wallet phrase 42!").unwrap();
        assert_ne!(other.address, id.address);
        assert!(from_secret("   ").is_err());
    }

    #[test]
    fn invalid_mnemonic_rejected() {
        assert!(from_mnemonic("foo bar baz").is_err());
        assert!(from_mnemonic("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon zebra").is_err());
    }
}
