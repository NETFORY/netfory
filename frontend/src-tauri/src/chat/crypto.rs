// ============================================================
//  SmartNet // chat/crypto.rs
//
//  Leaf cryptographic + codec helpers for the E2EE chat: base64 codec,
//  the deterministic per-user X25519 secret, ECDH shared-key derivation,
//  AES-256-GCM seal/open, and the group symmetric-key derivation. Extracted
//  from chat.rs to keep the chat command surface focused; these are pure
//  helpers with no Tauri command attributes (safe to share `pub(crate)`).
//
//  Author: TechnoL0g
//  Created: 2026-06-24
// ============================================================

use crate::SESSION_SEED;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

pub(crate) fn b64e(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}
pub(crate) fn b64d(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// Derive the per-user X25519 secret deterministically from the wallet seed.
pub(crate) fn my_secret() -> Result<StaticSecret, String> {
    let seed = SESSION_SEED.lock().unwrap().clone().ok_or("locked")?;
    let mut h = Sha256::new();
    h.update(b"smartnet-x25519-v1");
    h.update(seed.trim().as_bytes());
    let bytes: [u8; 32] = h.finalize().into();
    Ok(StaticSecret::from(bytes))
}

/// ECDH(my_secret, peer_pubkey) → SHA-256 → AES-256 symmetric key.
pub(crate) fn shared_key(peer_key_b64: &str) -> Result<[u8; 32], String> {
    let bytes = b64d(peer_key_b64).ok_or("bad key")?;
    if bytes.len() != 32 {
        return Err("bad key len".into());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    let peer = PublicKey::from(arr);
    let shared = my_secret()?.diffie_hellman(&peer);
    Ok(Sha256::digest(shared.as_bytes()).into())
}

pub(crate) fn encrypt(key: &[u8; 32], plain: &str) -> Result<String, String> {
    let cipher = Aes256Gcm::new(key.into());
    let mut nb = [0u8; 12];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut nb);
    let nonce = Nonce::from_slice(&nb);
    let ct = cipher.encrypt(nonce, plain.as_bytes()).map_err(|_| "enc")?;
    let mut out = nb.to_vec();
    out.extend_from_slice(&ct);
    Ok(b64e(&out))
}

pub(crate) fn decrypt(key: &[u8; 32], blob: &str) -> Result<String, String> {
    let raw = b64d(blob).ok_or("bad blob")?;
    if raw.len() < 13 {
        return Err("short".into());
    }
    let cipher = Aes256Gcm::new(key.into());
    let nonce = Nonce::from_slice(&raw[..12]);
    let pt = cipher.decrypt(nonce, &raw[12..]).map_err(|_| "dec")?;
    String::from_utf8(pt).map_err(|_| "utf8".into())
}

/// Derive a group's AES-256 key from its hex 32-byte shared secret.
pub(crate) fn group_key_from_secret(secret_hex: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(secret_hex).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut h = Sha256::new();
    h.update(b"smartnet-group-v1");
    h.update(&bytes);
    Some(h.finalize().into())
}
