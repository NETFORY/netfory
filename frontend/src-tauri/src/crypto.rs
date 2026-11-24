// ============================================================
//  SmartNet // crypto.rs
//
//  SmartHoldem key derivation, address encoding and the crypto-js
//  compatible AES vault sealing used to protect the mnemonic at rest.
//
//  Address model (identical to the SmartHoldem/ARK client):
//    address = base58check(0x3F ‖ ripemd160(secp256k1 compressed pubkey))
//  Vault sealing mirrors OpenSSL "Salted__" + EVP_BytesToKey(MD5) +
//  AES-256-CBC/Pkcs7 so web (crypto-js) and native vaults interoperate.
//
//  Author: TechnoL0g
//  Created: 2026-06-18
// ============================================================

use aes::Aes256;
use base64::prelude::{Engine as _, BASE64_STANDARD};
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use md5::Md5;
use rand::RngCore;
use ripemd::Ripemd160;
use secp256k1::ecdh::SharedSecret;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use serde::Serialize;
use sha2::{Digest, Sha256};

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};

type Aes256CbcEnc = cbc::Encryptor<Aes256>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;

pub(crate) const VERSION_BYTE: u8 = 0x3f; // SmartHoldem network pubKeyHash 63
pub(crate) const COIN_TYPE: u32 = 255; // SmartHoldem SLIP-44 coin type

/// A derived account / identity exposed to the frontend.
#[derive(Debug, Serialize, Clone)]
pub(crate) struct AccountInfo {
    pub index: u32,
    pub path: String,
    pub address: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
}

/// Current unix time (seconds) as a string — used for created/updated stamps.
pub(crate) fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{}", secs)
}

/// SHA-256 digest of `data`.
pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut r = [0u8; 32];
    r.copy_from_slice(&out);
    r
}

/// Encode a compressed secp256k1 pubkey into a SmartHoldem `S...` address.
pub(crate) fn address_from_pubkey(pk_comp: &[u8]) -> String {
    let mut rip = Ripemd160::new();
    rip.update(pk_comp);
    let hash = rip.finalize();
    let mut payload = Vec::with_capacity(21);
    payload.push(VERSION_BYTE);
    payload.extend_from_slice(&hash);
    bs58::encode(payload).with_check().into_string()
}

/// Wallet account. index 0 = secret itself; N = sha256(secret + "/" + N).
/// Аккаунт N: index 0 — sha256(passphrase) (как wallet-pro v1 / Core-Wallet
/// «Account 1»), index N>0 — BIP-44 `m/44'/255'/N'/0/0` из bip39-seed мнемоники
/// (совместимо с браузерным расширением Core-Wallet: та же фраза → те же адреса).
pub(crate) fn derive_account_internal(secret: &str, index: u32) -> Result<AccountInfo, String> {
    let s = secret.trim();
    if index == 0 {
        let (_, pk_hex, address) = keys_from_material(s)?;
        return Ok(AccountInfo { index, path: "sha256(secret)".to_string(), address, public_key: pk_hex });
    }
    let priv32 = bip44_private_key(s, index)?;
    let sk = SecretKey::from_slice(&priv32).map_err(|e| e.to_string())?;
    let secp = Secp256k1::signing_only();
    let comp = PublicKey::from_secret_key(&secp, &sk).serialize();
    Ok(AccountInfo {
        index,
        path: format!("m/44'/255'/{}'/0/0", index),
        address: address_from_pubkey(&comp),
        public_key: hex::encode(comp),
    })
}

/// Приватный ключ дочернего аккаунта в hex — для подписи tx с суб-адреса
/// (`keypair_from_secret("hex:…")` в sth_node).
pub(crate) fn bip44_private_key_hex(secret: &str, index: u32) -> Result<String, String> {
    bip44_private_key(secret, index).map(hex::encode)
}

fn hmac_sha512(key: &[u8], data: &[u8]) -> [u8; 64] {
    use hmac::Mac;
    let mut mac = <hmac::Hmac<sha2::Sha512> as hmac::Mac>::new_from_slice(key).expect("hmac key");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// BIP-39: seed = PBKDF2-HMAC-SHA512(mnemonic, "mnemonic" + passphrase(""), 2048) → 64 байта
/// (dkLen = один блок, поэтому без общего PBKDF2).
fn bip39_seed(mnemonic: &str) -> [u8; 64] {
    let salt = b"mnemonic";
    let mut salt_block = salt.to_vec();
    salt_block.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha512(mnemonic.as_bytes(), &salt_block);
    let mut t = u;
    for _ in 1..2048 {
        u = hmac_sha512(mnemonic.as_bytes(), &u);
        for (ti, ui) in t.iter_mut().zip(u.iter()) {
            *ti ^= ui;
        }
    }
    t
}

/// BIP-32 secp256k1: m/44'/255'/index'/0/0.
fn bip44_private_key(mnemonic: &str, index: u32) -> Result<[u8; 32], String> {
    const HARD: u32 = 0x8000_0000;
    let seed = bip39_seed(mnemonic);
    let master = hmac_sha512(b"Bitcoin seed", &seed);
    let mut k = SecretKey::from_slice(&master[..32]).map_err(|e| format!("bip32 master: {e}"))?;
    let mut chain: [u8; 32] = master[32..].try_into().unwrap();
    let secp = Secp256k1::signing_only();
    for child in [44 | HARD, 255 | HARD, index | HARD, 0, 0] {
        let mut data = Vec::with_capacity(37);
        if child & HARD != 0 {
            data.push(0);
            data.extend_from_slice(&k.secret_bytes());
        } else {
            data.extend_from_slice(&PublicKey::from_secret_key(&secp, &k).serialize());
        }
        data.extend_from_slice(&child.to_be_bytes());
        let i = hmac_sha512(&chain, &data);
        let il: [u8; 32] = i[..32].try_into().unwrap();
        let tweak = secp256k1::Scalar::from_be_bytes(il).map_err(|_| "bip32: IL out of range".to_string())?;
        k = k.add_tweak(&tweak).map_err(|e| format!("bip32 child: {e}"))?;
        chain = i[32..].try_into().unwrap();
    }
    Ok(k.secret_bytes())
}

/// Application identity (immutable dApp URL): m/44'/255'/0'/1/index, ARK-namespaced.
pub(crate) fn derive_app_identity_internal(secret: &str, index: u32) -> Result<AccountInfo, String> {
    let material = format!("{}/1/{}", secret.trim(), index);
    let (_, pk_hex, address) = keys_from_material(&material)?;
    Ok(AccountInfo {
        index,
        path: format!("m/44'/{}'/0'/1/{}", COIN_TYPE, index),
        address,
        public_key: pk_hex,
    })
}

/// (secret_key, pubkey_hex, address) from arbitrary key material.
pub(crate) fn keys_from_material(material: &str) -> Result<(SecretKey, String, String), String> {
    let priv_bytes = sha256(material.trim().as_bytes());
    let sk = SecretKey::from_slice(&priv_bytes).map_err(|e| e.to_string())?;
    let secp = Secp256k1::signing_only();
    let pk = PublicKey::from_secret_key(&secp, &sk);
    let comp = pk.serialize();
    Ok((sk, hex::encode(comp), address_from_pubkey(&comp)))
}

/// Stretch a PIN into the AES passphrase bytes (hex of its sha256).
pub(crate) fn pin_passphrase(pin: &str) -> Vec<u8> {
    hex::encode(sha256(pin.as_bytes())).into_bytes()
}

/// OpenSSL EVP_BytesToKey (MD5) — derives (key, iv) from passphrase + salt.
fn evp_bytes_to_key(pass: &[u8], salt: &[u8], key_len: usize, iv_len: usize) -> (Vec<u8>, Vec<u8>) {
    let mut derived: Vec<u8> = Vec::new();
    let mut block: Vec<u8> = Vec::new();
    while derived.len() < key_len + iv_len {
        let mut h = Md5::new();
        if !block.is_empty() {
            h.update(&block);
        }
        h.update(pass);
        h.update(salt);
        block = h.finalize().to_vec();
        derived.extend_from_slice(&block);
    }
    (
        derived[..key_len].to_vec(),
        derived[key_len..key_len + iv_len].to_vec(),
    )
}

/// crypto-js compatible AES-256-CBC seal: base64("Salted__" ‖ salt ‖ ciphertext).
pub(crate) fn aes_encrypt(plaintext: &str, pass: &[u8]) -> String {
    let mut salt = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut salt);
    let (key, iv) = evp_bytes_to_key(pass, &salt, 32, 16);
    let key_arr: [u8; 32] = key.try_into().unwrap();
    let iv_arr: [u8; 16] = iv.try_into().unwrap();
    let ct = Aes256CbcEnc::new(&key_arr.into(), &iv_arr.into())
        .encrypt_padded_vec_mut::<Pkcs7>(plaintext.as_bytes());
    let mut out = Vec::with_capacity(16 + ct.len());
    out.extend_from_slice(b"Salted__");
    out.extend_from_slice(&salt);
    out.extend_from_slice(&ct);
    BASE64_STANDARD.encode(out)
}

/// Reverse of [`aes_encrypt`] — returns the plaintext, or None on bad PIN/format.
pub(crate) fn aes_decrypt(b64: &str, pass: &[u8]) -> Option<String> {
    let raw = BASE64_STANDARD.decode(b64.trim()).ok()?;
    if raw.len() < 16 || &raw[..8] != b"Salted__" {
        return None;
    }
    let salt = &raw[8..16];
    let ct = &raw[16..];
    let (key, iv) = evp_bytes_to_key(pass, salt, 32, 16);
    let key_arr: [u8; 32] = key.try_into().ok()?;
    let iv_arr: [u8; 16] = iv.try_into().ok()?;
    let pt = Aes256CbcDec::new(&key_arr.into(), &iv_arr.into())
        .decrypt_padded_vec_mut::<Pkcs7>(ct)
        .ok()?;
    String::from_utf8(pt).ok()
}


// ============================================================
//  E2EE File Exchange — ECIES (secp256k1 ECDH) + AES-256-GCM
//
//  Content sealing:   AES-256-GCM over the file container (nonce ‖ ct‖tag).
//  Key wrapping:      per recipient, an ephemeral secp256k1 keypair does ECDH
//                     with the recipient's on-chain public key; the shared
//                     secret (SHA-256 of the shared point) is the AES-256 KEK
//                     used to GCM-wrap the random file key.
//  Wrapped layout:    hex( ephPub(33) ‖ nonce(12) ‖ wrappedKey‖tag ).
//
//  Author: TechnoL0g
//  Created: 2026-06-20
// ============================================================

/// Fresh random 32-byte AES-256 file key.
pub(crate) fn gen_file_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut k);
    k
}

/// AES-256-GCM seal: output = nonce(12) ‖ ciphertext‖tag.
pub(crate) fn aes_gcm_encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(key.into());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher.encrypt(&nonce, plaintext).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Reverse of [`aes_gcm_encrypt`] — expects nonce(12) ‖ ciphertext‖tag.
pub(crate) fn aes_gcm_decrypt(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 12 {
        return Err("ciphertext too short".to_string());
    }
    let cipher = Aes256Gcm::new(key.into());
    let nonce = Nonce::from_slice(&data[..12]);
    cipher
        .decrypt(nonce, &data[12..])
        .map_err(|_| "не удалось расшифровать (неверный ключ или повреждённые данные)".to_string())
}

/// ECIES-wrap the 32-byte file key for a recipient's compressed secp256k1
/// public key (hex). Returns hex( ephPub(33) ‖ nonce(12) ‖ wrappedKey‖tag ).
pub(crate) fn ecies_wrap_key(recipient_pk_hex: &str, file_key: &[u8; 32]) -> Result<String, String> {
    let pk_bytes = hex::decode(recipient_pk_hex.trim()).map_err(|e| e.to_string())?;
    let recipient = PublicKey::from_slice(&pk_bytes).map_err(|e| e.to_string())?;
    let secp = Secp256k1::new();
    let mut eph = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut eph);
    let eph_sk = SecretKey::from_slice(&eph).map_err(|e| e.to_string())?;
    let eph_pk = PublicKey::from_secret_key(&secp, &eph_sk);
    let shared = SharedSecret::new(&recipient, &eph_sk);
    let kek: [u8; 32] = shared.secret_bytes();
    let wrapped = aes_gcm_encrypt(&kek, file_key)?;
    let mut out = Vec::with_capacity(33 + wrapped.len());
    out.extend_from_slice(&eph_pk.serialize());
    out.extend_from_slice(&wrapped);
    Ok(hex::encode(out))
}

/// Unwrap a file key produced by [`ecies_wrap_key`] using the current user's
/// secret material (the wallet seed). `keys_from_material` yields the same
/// secret key whose public key the sender ECDH-wrapped against.
pub(crate) fn ecies_unwrap_key(secret_material: &str, wrapped_hex: &str) -> Result<[u8; 32], String> {
    let raw = hex::decode(wrapped_hex.trim()).map_err(|e| e.to_string())?;
    if raw.len() < 33 + 12 {
        return Err("wrapped key too short".to_string());
    }
    let (sk, _, _) = keys_from_material(secret_material)?;
    let eph_pk = PublicKey::from_slice(&raw[..33]).map_err(|e| e.to_string())?;
    let shared = SharedSecret::new(&eph_pk, &sk);
    let kek: [u8; 32] = shared.secret_bytes();
    let fk = aes_gcm_decrypt(&kek, &raw[33..])?;
    if fk.len() != 32 {
        return Err("bad file key length".to_string());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&fk);
    Ok(out)
}
