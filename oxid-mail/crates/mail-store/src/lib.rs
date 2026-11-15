// Шифрованное локальное хранилище Oxid Mail (та же схема, что ntfry):
// sha256(mnemonic) -> HKDF-SHA256 -> XChaCha20-Poly1305; значения сжаты zstd.
use std::path::Path;

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand::RngCore;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};

const NONCE_LEN: usize = 24;
const ZSTD_LEVEL: i32 = 6;

pub struct Cipher {
    aead: XChaCha20Poly1305,
}

impl Cipher {
    pub fn from_secret(secret: &str, salt: &[u8]) -> Self {
        let master = Sha256::digest(secret.trim().as_bytes());
        let hk = Hkdf::<Sha256>::new(Some(salt), &master);
        let mut key = [0u8; 32];
        hk.expand(b"mail-store-key", &mut key)
            .expect("hkdf expand 32 bytes");
        Self {
            aead: XChaCha20Poly1305::new((&key).into()),
        }
    }

    pub fn from_mnemonic(mnemonic: &str) -> Self {
        Self::from_secret(mnemonic, b"oxid-mail/store/v1")
    }

    /// zstd-сжатие + AEAD-шифрование; формат: nonce(24) ‖ ciphertext.
    pub fn seal(&self, plain: &[u8]) -> Vec<u8> {
        let compressed = zstd::encode_all(plain, ZSTD_LEVEL).expect("zstd encode");
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ct = self
            .aead
            .encrypt(XNonce::from_slice(&nonce), compressed.as_ref())
            .expect("aead encrypt");
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    pub fn open(&self, blob: &[u8]) -> Result<Vec<u8>, String> {
        if blob.len() < NONCE_LEN + 16 {
            return Err("blob too short".into());
        }
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        let compressed = self
            .aead
            .decrypt(XNonce::from_slice(nonce), ct)
            .map_err(|_| "decrypt failed (wrong key?)".to_string())?;
        zstd::decode_all(compressed.as_slice()).map_err(|e| format!("zstd decode: {e}"))
    }
}

pub struct MailStore {
    db: sled::Db,
    cipher: Cipher,
}

impl MailStore {
    pub fn open(path: impl AsRef<Path>, mnemonic: &str) -> Result<Self, String> {
        let db = sled::open(path.as_ref()).map_err(|e| format!("open store: {e}"))?;
        let cipher = Cipher::from_mnemonic(mnemonic);
        match db.get(b"__check__").map_err(|e| e.to_string())? {
            Some(v) => {
                cipher
                    .open(&v)
                    .map_err(|_| "seed does not match existing store".to_string())?;
            }
            None => {
                db.insert(b"__check__", cipher.seal(b"oxid-mail"))
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(Self { db, cipher })
    }

    fn tree(&self, name: &str) -> Result<sled::Tree, String> {
        self.db.open_tree(name).map_err(|e| e.to_string())
    }

    pub fn put_json<T: Serialize>(&self, tree: &str, key: &str, value: &T) -> Result<(), String> {
        let plain = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        self.tree(tree)?
            .insert(key.as_bytes(), self.cipher.seal(&plain))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn put_blob(&self, tree: &str, key: &str, data: &[u8]) -> Result<(), String> {
        self.tree(tree)?
            .insert(key.as_bytes(), self.cipher.seal(data))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_blob(&self, tree: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
        match self.tree(tree)?.get(key.as_bytes()).map_err(|e| e.to_string())? {
            Some(v) => Ok(Some(self.cipher.open(&v)?)),
            None => Ok(None),
        }
    }

    pub fn get_json<T: DeserializeOwned>(&self, tree: &str, key: &str) -> Result<Option<T>, String> {
        match self.tree(tree)?.get(key.as_bytes()).map_err(|e| e.to_string())? {
            Some(v) => {
                let plain = self.cipher.open(&v)?;
                Ok(Some(serde_json::from_slice(&plain).map_err(|e| e.to_string())?))
            }
            None => Ok(None),
        }
    }

    pub fn contains(&self, tree: &str, key: &str) -> Result<bool, String> {
        self.tree(tree)?
            .contains_key(key.as_bytes())
            .map_err(|e| e.to_string())
    }

    pub fn delete(&self, tree: &str, key: &str) -> Result<(), String> {
        self.tree(tree)?
            .remove(key.as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_json<T: DeserializeOwned>(&self, tree: &str) -> Result<Vec<T>, String> {
        let mut out = Vec::new();
        for item in self.tree(tree)?.iter() {
            let (_, v) = item.map_err(|e| e.to_string())?;
            let plain = self.cipher.open(&v)?;
            out.push(serde_json::from_slice(&plain).map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn drop_tree(&self, name: &str) -> Result<(), String> {
        self.db.drop_tree(name).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn flush(&self) {
        let _ = self.db.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Doc {
        id: String,
        body: String,
    }

    #[test]
    fn roundtrip_and_wrong_seed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1");
        {
            let store = MailStore::open(&path, "alpha beta gamma").unwrap();
            let doc = Doc {
                id: "m1".into(),
                body: "hello ".repeat(200),
            };
            store.put_json("msgs", "m1", &doc).unwrap();
            let got: Doc = store.get_json("msgs", "m1").unwrap().unwrap();
            assert_eq!(got, doc);
            assert!(store.contains("msgs", "m1").unwrap());
            let all: Vec<Doc> = store.list_json("msgs").unwrap();
            assert_eq!(all.len(), 1);
            store.flush();
        }
        // неверная seed-фраза не должна открывать существующее хранилище
        assert!(MailStore::open(&path, "other seed words").is_err());
        // верная — открывает и читает
        let store = MailStore::open(&path, "alpha beta gamma").unwrap();
        let got: Doc = store.get_json("msgs", "m1").unwrap().unwrap();
        assert_eq!(got.id, "m1");
        store.delete("msgs", "m1").unwrap();
        assert!(!store.contains("msgs", "m1").unwrap());
    }

    #[test]
    fn values_are_encrypted_and_compressed_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s2");
        let store = MailStore::open(&path, "alpha beta gamma").unwrap();
        let plain = "TOP-SECRET-BODY ".repeat(500);
        store
            .put_json("msgs", "m1", &Doc { id: "m1".into(), body: plain.clone() })
            .unwrap();
        let raw = store.tree("msgs").unwrap().get(b"m1").unwrap().unwrap();
        let raw_str = String::from_utf8_lossy(&raw);
        assert!(!raw_str.contains("TOP-SECRET-BODY"));
        // сжатие: 8КБ повторяющегося текста должны ужаться сильно
        assert!(raw.len() < plain.len() / 4);
    }
}
