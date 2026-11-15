//! Криптослой: BIP-39 сид → BIP-32 (HMAC-SHA512, secp256k1) деривация →
//! ChaCha20-Poly1305 AEAD. Формат шифроблока: nonce(12) || ciphertext+tag.

use bip39::{Language, Mnemonic, MnemonicType, Seed};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hmac::{Hmac, Mac};
use rand::RngCore;
use secp256k1::{PublicKey, Scalar, Secp256k1, SecretKey};
use sha2::{Digest, Sha256, Sha512};
use sha3::Keccak256;

use crate::error::CryptoError;

type HmacSha512 = Hmac<Sha512>;

const HARDENED: u32 = 0x8000_0000;
/// Мастер-ключ шифрования: m/999'/0'/0'/0
pub const MASTER_KEY_PATH: &[u32] = &[999 | HARDENED, HARDENED, HARDENED, 0];
/// Блокчейн-адрес (ETH-совместимый): m/44'/60'/0'/0/0
pub const ADDRESS_PATH: &[u32] = &[44 | HARDENED, 60 | HARDENED, HARDENED, 0, 0];

pub struct KeyMaterial {
    pub mnemonic: String,
    pub master_key: [u8; 32],
    pub address: String,
}

pub fn generate_mnemonic() -> String {
    Mnemonic::new(MnemonicType::Words12, Language::English)
        .phrase()
        .to_string()
}

/// Полная деривация: мнемоника → мастер-ключ шифрования + блокчейн-адрес.
pub fn derive(mnemonic_phrase: &str) -> Result<KeyMaterial, CryptoError> {
    let m = Mnemonic::from_phrase(mnemonic_phrase.trim(), Language::English)
        .map_err(|e| CryptoError::BadMnemonic(e.to_string()))?;
    let seed = Seed::new(&m, "");
    derive_from_seed(seed.as_bytes(), m.phrase().to_string())
}

/// Гибкая деривация: валидная BIP-39 фраза → стандартный путь; любой другой
/// секрет (кошелёк SmartNet допускает произвольные passphrase) → сид =
/// SHA-512("ntfry-flex-seed" || secret), далее те же BIP-32 пути.
pub fn derive_flexible(secret: &str) -> Result<KeyMaterial, CryptoError> {
    let s = secret.trim();
    if s.is_empty() {
        return Err(CryptoError::BadMnemonic("пустой секрет".into()));
    }
    if let Ok(m) = Mnemonic::from_phrase(s, Language::English) {
        let seed = Seed::new(&m, "");
        return derive_from_seed(seed.as_bytes(), m.phrase().to_string());
    }
    let mut h = Sha512::new();
    h.update(b"ntfry-flex-seed");
    h.update(s.as_bytes());
    let seed = h.finalize();
    derive_from_seed(&seed, s.to_string())
}

fn derive_from_seed(seed: &[u8], mnemonic: String) -> Result<KeyMaterial, CryptoError> {
    let master_key = derive_priv(seed, MASTER_KEY_PATH)?;
    let addr_key = derive_priv(seed, ADDRESS_PATH)?;
    let address = eth_address(&addr_key)?;
    Ok(KeyMaterial { mnemonic, master_key, address })
}

/// BIP-32 деривация приватного ключа по пути (hardened + normal).
fn derive_priv(seed: &[u8], path: &[u32]) -> Result<[u8; 32], CryptoError> {
    let mut mac = <HmacSha512 as Mac>::new_from_slice(b"Bitcoin seed").expect("hmac");
    mac.update(seed);
    let i = mac.finalize().into_bytes();
    let mut key: [u8; 32] = i[..32].try_into().expect("32");
    let mut chain: [u8; 32] = i[32..].try_into().expect("32");
    let secp = Secp256k1::new();
    let err = |e: &dyn std::fmt::Display| CryptoError::Derivation(e.to_string());
    for &idx in path {
        let mut mac = <HmacSha512 as Mac>::new_from_slice(&chain).expect("hmac");
        if idx & HARDENED != 0 {
            mac.update(&[0u8]);
            mac.update(&key);
        } else {
            let sk = SecretKey::from_slice(&key).map_err(|e| err(&e))?;
            mac.update(&PublicKey::from_secret_key(&secp, &sk).serialize());
        }
        mac.update(&idx.to_be_bytes());
        let i = mac.finalize().into_bytes();
        let il: [u8; 32] = i[..32].try_into().expect("32");
        let parent = SecretKey::from_slice(&key).map_err(|e| err(&e))?;
        let child = parent
            .add_tweak(&Scalar::from_be_bytes(il).map_err(|e| err(&e))?)
            .map_err(|e| err(&e))?;
        key = child.secret_bytes();
        chain = i[32..].try_into().expect("32");
    }
    Ok(key)
}

/// ETH-адрес: keccak256(uncompressed_pubkey[1..])[12..], lowercase hex.
fn eth_address(priv_key: &[u8; 32]) -> Result<String, CryptoError> {
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(priv_key)
        .map_err(|e| CryptoError::Derivation(e.to_string()))?;
    let pk = PublicKey::from_secret_key(&secp, &sk).serialize_uncompressed();
    let hash = Keccak256::digest(&pk[1..]);
    Ok(format!("0x{}", hex::encode(&hash[12..])))
}

/// Ключевая пара подписи Cloud Seeder: secp256k1 по адресному пути кошелька
/// (m/44'/60'/0'/0/0). Возвращает (приватный 32 байта, compressed pubkey 33).
pub fn derive_address_keypair(secret: &str) -> Result<([u8; 32], [u8; 33]), CryptoError> {
    let s = secret.trim();
    if s.is_empty() {
        return Err(CryptoError::BadMnemonic("пустой секрет".into()));
    }
    let seed: Vec<u8> = if let Ok(m) = Mnemonic::from_phrase(s, Language::English) {
        Seed::new(&m, "").as_bytes().to_vec()
    } else {
        let mut h = Sha512::new();
        h.update(b"ntfry-flex-seed");
        h.update(s.as_bytes());
        h.finalize().to_vec()
    };
    let priv_key = derive_priv(&seed, ADDRESS_PATH)?;
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&priv_key).map_err(|e| CryptoError::Derivation(e.to_string()))?;
    let pk = PublicKey::from_secret_key(&secp, &sk).serialize();
    Ok((priv_key, pk))
}

pub fn encrypt(key: &[u8; 32], plain: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plain)
        .map_err(|_| CryptoError::Aead)?;
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn decrypt(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if data.len() < 12 + 16 {
        return Err(CryptoError::Malformed("блок короче nonce+tag".into()));
    }
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(&data[..12]), &data[12..])
        .map_err(|_| CryptoError::Aead)
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Маскированный hex мастер-ключа для вывода в CLI (не палим весь ключ).
pub fn masked_hex(key: &[u8; 32]) -> String {
    let h = hex::encode(key);
    format!("{}…{}", &h[..6], &h[h.len() - 6..])
}

/// Публичный отпечаток ключа для /ntfry/meta: односторонний хэш от секрета.
/// Позволяет мгновенно детектить «чужой кошелёк» без скачивания индекса.
pub fn key_fingerprint(key: &[u8; 32]) -> String {
    let mut h = Sha256::new();
    h.update(b"ntfry-fp-v1");
    h.update(key);
    hex::encode(h.finalize())[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn bip39_derivation_is_deterministic_and_matches_eth_vector() {
        let a = derive(TEST_MNEMONIC).unwrap();
        let b = derive(TEST_MNEMONIC).unwrap();
        assert_eq!(a.master_key, b.master_key);
        // Общеизвестный вектор m/44'/60'/0'/0/0 для стандартной мнемоники
        assert_eq!(a.address, "0x9858effd232b4033e47d90003d41ec34ecaeda94");
        // Мастер-ключ шифрования (m/999'/0'/0'/0) отличается от адресного пути
        assert_ne!(a.master_key, [0u8; 32]);
    }

    #[test]
    fn aead_roundtrip_and_tamper_detection() {
        let km = derive(TEST_MNEMONIC).unwrap();
        let plain = b"ntfry zero-knowledge chunk".to_vec();
        let enc = encrypt(&km.master_key, &plain).unwrap();
        assert_eq!(decrypt(&km.master_key, &enc).unwrap(), plain);
        let mut broken = enc.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xff;
        assert!(decrypt(&km.master_key, &broken).is_err());
    }

    #[test]
    fn derive_flexible_bip39_matches_strict_and_arbitrary_secret_works() {
        let strict = derive(TEST_MNEMONIC).unwrap();
        let flex = derive_flexible(TEST_MNEMONIC).unwrap();
        assert_eq!(strict.master_key, flex.master_key);
        assert_eq!(strict.address, flex.address);
        // произвольный секрет (не BIP-39) — детерминированный результат
        let a = derive_flexible("моя произвольная фраза кошелька").unwrap();
        let b = derive_flexible("моя произвольная фраза кошелька").unwrap();
        assert_eq!(a.master_key, b.master_key);
        assert_ne!(a.master_key, strict.master_key);
    }
}
