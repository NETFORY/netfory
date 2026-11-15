//! Локальный кэш (Sled): зашифрованный снапшот VaultIndex + sha последней
//! синхронизации. Даёт мгновенный старт без сетевого запроса.
//!
//! Все операции — async: sled I/O (включая fsync) выполняется исключительно
//! в `spawn_blocking`, чтобы никогда не блокировать основной tokio-тред IPC
//! (правило проекта: каждая функция должна быть async и не блокировать
//! main thread — см. memory/RULES.md).

use std::path::{Path, PathBuf};

use crate::error::VaultError;

const K_INDEX: &[u8] = b"index_enc";
const K_SYNCED_SHA: &[u8] = b"synced_sha";
/// Открытый (незашифрованный) хинт о суммарном stored_bytes vault-а.
/// Числовая метаинформация, не раскрывающая содержимого, но полезная
/// в UI списка дисков («занято сейфом») без открытия vault.
const K_STORED_BYTES: &[u8] = b"stored_bytes";

/// Sled-хэндл держится в Arc внутри самого sled::Db, `Clone` дёшев.
/// Все методы принимают `&self` и переносят клон handle в blocking-таску.
#[derive(Clone)]
pub struct Cache {
    db: sled::Db,
}

impl Cache {
    /// Открытие Sled-кэша. Первый запуск (или холодная директория с
    /// незакрытым журналом) может занять сотни мс — выполняем в
    /// `spawn_blocking`, чтобы не морозить IPC-тред при переключении дисков.
    pub async fn open(dir: &Path) -> Result<Self, VaultError> {
        let dir: PathBuf = dir.to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<Self, VaultError> {
            std::fs::create_dir_all(&dir)?;
            Ok(Self { db: sled::open(dir.join("vault_index.sled"))? })
        })
        .await
        .map_err(|e| VaultError::Serde(format!("cache-open join: {e}")))?
    }

    /// Зашифрованный снапшот индекса (как лежал бы index.enc в облаке).
    pub async fn load_index_enc(&self) -> Option<Vec<u8>> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || db.get(K_INDEX).ok().flatten().map(|v| v.to_vec()))
            .await
            .ok()
            .flatten()
    }

    pub async fn save_index_enc(&self, data: &[u8]) -> Result<(), VaultError> {
        let db = self.db.clone();
        let data = data.to_vec();
        tokio::task::spawn_blocking(move || -> Result<(), VaultError> {
            db.insert(K_INDEX, data)?;
            db.flush()?;
            Ok(())
        })
        .await
        .map_err(|e| VaultError::Serde(format!("cache-save join: {e}")))?
    }

    /// SHA-256 index.enc, который мы в последний раз синхронизировали с облаком.
    pub async fn synced_sha(&self) -> Option<String> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            db.get(K_SYNCED_SHA)
                .ok()
                .flatten()
                .map(|v| String::from_utf8_lossy(&v).to_string())
        })
        .await
        .ok()
        .flatten()
    }

    pub async fn set_synced_sha(&self, sha: &str) -> Result<(), VaultError> {
        let db = self.db.clone();
        let sha = sha.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), VaultError> {
            db.insert(K_SYNCED_SHA, sha.as_bytes())?;
            db.flush()?;
            Ok(())
        })
        .await
        .map_err(|e| VaultError::Serde(format!("cache-sha join: {e}")))?
    }

    /// Читаем последний сохранённый `stored_bytes` для этого (provider, account).
    /// Используется UI списка дисков без открытия vault (быстрый батч).
    pub async fn stored_bytes(&self) -> Option<u64> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let raw = db.get(K_STORED_BYTES).ok().flatten()?;
            if raw.len() != 8 {
                return None;
            }
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&raw);
            Some(u64::from_le_bytes(buf))
        })
        .await
        .ok()
        .flatten()
    }

    pub async fn set_stored_bytes(&self, n: u64) -> Result<(), VaultError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<(), VaultError> {
            db.insert(K_STORED_BYTES, &n.to_le_bytes())?;
            db.flush()?;
            Ok(())
        })
        .await
        .map_err(|e| VaultError::Serde(format!("cache-bytes join: {e}")))?
    }

    /// Полная очистка локального Sled-кэша: `index_enc`, `synced_sha`,
    /// `stored_bytes`. Используется когда обнаружено, что облако было
    /// очищено извне (например, юзер удалил папку /ntfry вручную из
    /// веб-интерфейса Yandex Disk) — иначе застрявший в кэше старый индекс
    /// «воскрешает» удалённую папку при следующем push/save_index.
    pub async fn reset(&self) -> Result<(), VaultError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<(), VaultError> {
            let _ = db.remove(K_INDEX)?;
            let _ = db.remove(K_SYNCED_SHA)?;
            let _ = db.remove(K_STORED_BYTES)?;
            db.flush()?;
            Ok(())
        })
        .await
        .map_err(|e| VaultError::Serde(format!("cache-reset join: {e}")))?
    }
}
