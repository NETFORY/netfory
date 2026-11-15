//! Доменные ошибки ntfry (thiserror): крипто / хранилище / vault.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum CryptoError {
    #[error("невалидная BIP-39 мнемоника: {0}")]
    BadMnemonic(String),
    #[error("ошибка деривации ключа: {0}")]
    Derivation(String),
    #[error("ошибка AEAD (неверный ключ или повреждённые данные)")]
    Aead,
    #[error("повреждённый шифроблок: {0}")]
    Malformed(String),
}

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("HTTP: {0}")]
    Http(#[from] reqwest::Error),
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("облако ответило {status}: {body}")]
    Api { status: u16, body: String },
    #[error("объект не найден: {0}")]
    NotFound(String),
    #[error("провайдер не реализован: {0}")]
    NotImplemented(String),
    #[error("нет токена доступа: {0}")]
    MissingToken(String),
    #[error("сеть встала: {0}")]
    Stalled(String),
}

#[derive(Error, Debug)]
pub enum VaultError {
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("сериализация индекса: {0}")]
    Serde(String),
    #[error("сжатие: {0}")]
    Compression(String),
    #[error("кэш sled: {0}")]
    Cache(#[from] sled::Error),
    #[error("файл не найден в VFS: {0}")]
    VfsNotFound(String),
    #[error("файл сжат словарём, но user.dict недоступен")]
    DictMissing,
    #[error("индекс в облаке зашифрован другим ключом — облачный vault принадлежит другому кошельку")]
    KeyMismatch,
    #[error("файлы удалены из индекса, но GC чанков в облаке не завершён: {0}")]
    GcFailed(String),
}

pub type Result<T, E = VaultError> = std::result::Result<T, E>;
