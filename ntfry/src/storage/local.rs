//! LocalStorageProvider — эмуляция облака в локальной папке (zero-network
//! тестирование): <root>/ntfry/{index.enc, user.dict, chunks/}.

use std::path::PathBuf;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use super::StorageProvider;
use crate::error::StorageError;

pub struct LocalStorageProvider {
    root: PathBuf,
}

impl LocalStorageProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("ntfry")
    }
}

#[async_trait]
impl StorageProvider for LocalStorageProvider {
    async fn init_vault_dir(&self) -> Result<(), StorageError> {
        tokio::fs::create_dir_all(self.vault().join("chunks")).await?;
        Ok(())
    }

    async fn upload_chunk(&self, id: &str, data: &[u8]) -> Result<(), StorageError> {
        tokio::fs::write(self.vault().join("chunks").join(id), data).await?;
        Ok(())
    }

    async fn download_chunk(&self, id: &str) -> Result<Vec<u8>, StorageError> {
        let p = self.vault().join("chunks").join(id);
        tokio::fs::read(&p)
            .await
            .map_err(|_| StorageError::NotFound(format!("chunk {id}")))
    }

    async fn chunk_exists(&self, id: &str) -> Result<bool, StorageError> {
        Ok(tokio::fs::try_exists(self.vault().join("chunks").join(id)).await.unwrap_or(false))
    }

    async fn upload_index(&self, encrypted_index: &[u8]) -> Result<(), StorageError> {
        tokio::fs::write(self.vault().join("index.enc"), encrypted_index).await?;
        Ok(())
    }

    async fn download_index(&self) -> Result<Vec<u8>, StorageError> {
        tokio::fs::read(self.vault().join("index.enc"))
            .await
            .map_err(|_| StorageError::NotFound("index.enc".into()))
    }

    async fn remote_index_sha256(&self) -> Result<Option<String>, StorageError> {
        match tokio::fs::read(self.vault().join("index.enc")).await {
            Ok(data) => Ok(Some(hex::encode(Sha256::digest(&data)))),
            Err(_) => Ok(None),
        }
    }

    async fn download_dict(&self) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(tokio::fs::read(self.vault().join("user.dict")).await.ok())
    }

    async fn upload_dict(&self, data: &[u8]) -> Result<(), StorageError> {
        tokio::fs::write(self.vault().join("user.dict"), data).await?;
        Ok(())
    }

    async fn delete_vault(&self) -> Result<(), StorageError> {
        match tokio::fs::remove_dir_all(self.vault()).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn delete_chunk(&self, id: &str) -> Result<(), StorageError> {
        match tokio::fs::remove_file(self.vault().join("chunks").join(id)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn download_meta(&self) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(tokio::fs::read(self.vault().join("meta")).await.ok())
    }

    async fn upload_meta(&self, data: &[u8]) -> Result<(), StorageError> {
        tokio::fs::write(self.vault().join("meta"), data).await?;
        Ok(())
    }

    async fn quota(&self) -> Result<Option<(u64, u64)>, StorageError> {
        Ok(None)
    }
}
