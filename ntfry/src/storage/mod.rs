//! Абстракция облачного хранилища. Все данные приложения живут в /ntfry/:
//! index.enc, user.dict, chunks/<sha256>.

pub mod gdrive;
pub mod local;
pub mod mailru;
pub mod raid;
pub mod yandex;

use async_trait::async_trait;

use crate::error::StorageError;

/// Информация об аккаунте провайдера (для списка дисков в UI).
pub struct AccountInfo {
    pub login: String,
    pub total: u64,
    pub used: u64,
}

/// Итог RAID-1 re-sync: сколько чанков проверено / докатано / потеряно.
#[derive(Debug, Default, Clone, Copy)]
pub struct ResyncStats {
    pub checked: u64,
    pub copied: u64,
    pub failed: u64,
    pub lost: u64,
}

/// Транзиентная ошибка: сетевой сбой/таймаут (reqwest), 429 или 5xx —
/// имеет смысл повторить. 401/403/404 и пр. — повторять бессмысленно.
pub fn is_transient(e: &StorageError) -> bool {
    match e {
        StorageError::Http(_) => true,
        StorageError::Stalled(_) => true,
        StorageError::Api { status, .. } => *status == 429 || *status >= 500,
        _ => false,
    }
}

/// Авторетрай транзиентных ошибок облака: до 3 повторов с паузами 1с/3с/7с.
/// Урок Mail.ru WebDAV: под параллельной нагрузкой единичные PUT рвутся,
/// без ретрая падает вся загрузка большого файла.
pub async fn with_retry<T, F, Fut>(op: F) -> Result<T, StorageError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, StorageError>>,
{
    const DELAYS_S: [u64; 3] = [1, 3, 7];
    let mut attempt = 0usize;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < DELAYS_S.len() && is_transient(&e) => {
                eprintln!("облако: транзиентная ошибка, повтор {}/3 через {}с: {e}", attempt + 1, DELAYS_S[attempt]);
                tokio::time::sleep(std::time::Duration::from_secs(DELAYS_S[attempt])).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

#[async_trait]
pub trait StorageProvider: Send + Sync {
    /// Создаёт /ntfry и /ntfry/chunks на провайдере.
    async fn init_vault_dir(&self) -> Result<(), StorageError>;
    async fn upload_chunk(&self, id: &str, data: &[u8]) -> Result<(), StorageError>;
    async fn download_chunk(&self, id: &str) -> Result<Vec<u8>, StorageError>;
    async fn upload_index(&self, encrypted_index: &[u8]) -> Result<(), StorageError>;
    async fn download_index(&self) -> Result<Vec<u8>, StorageError>;
    /// SHA-256 удалённого index.enc (для фоновой валидации кэша). None — нет файла
    /// или провайдер не отдаёт хэш.
    async fn remote_index_sha256(&self) -> Result<Option<String>, StorageError>;
    /// Пользовательский ZSTD-словарь /ntfry/user.dict (None — отсутствует).
    async fn download_dict(&self) -> Result<Option<Vec<u8>>, StorageError>;
    async fn upload_dict(&self, data: &[u8]) -> Result<(), StorageError>;
    /// Полное удаление /ntfry на провайдере (reset vault). Отсутствие — не ошибка.
    async fn delete_vault(&self) -> Result<(), StorageError>;
    /// Удаление одного чанка (GC осиротевших чанков). 404 — не ошибка.
    async fn delete_chunk(&self, id: &str) -> Result<(), StorageError>;
    /// Открытый метафайл /ntfry/meta (отпечаток ключа и т.п.). None — нет.
    async fn download_meta(&self) -> Result<Option<Vec<u8>>, StorageError>;
    async fn upload_meta(&self, data: &[u8]) -> Result<(), StorageError>;
    /// Квота диска провайдера: (total, used) в байтах. None — неизвестно.
    async fn quota(&self) -> Result<Option<(u64, u64)>, StorageError>;
    /// Есть ли чанк на провайдере. Default — пробная загрузка (дорого);
    /// провайдеры переопределяют дешёвой проверкой метаданных.
    async fn chunk_exists(&self, id: &str) -> Result<bool, StorageError> {
        match self.download_chunk(id).await {
            Ok(_) => Ok(true),
            Err(StorageError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
    /// RAID-1 re-sync: докатка недостающих чанков на члены зеркала.
    async fn resync(
        &self,
        _ids: &[String],
        _progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
    ) -> Result<ResyncStats, StorageError> {
        Err(StorageError::NotImplemented("re-sync доступен только в RAID-1 (зеркало)".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_classification() {
        assert!(is_transient(&StorageError::Api { status: 500, body: String::new() }));
        assert!(is_transient(&StorageError::Api { status: 429, body: String::new() }));
        assert!(!is_transient(&StorageError::Api { status: 401, body: String::new() }));
        assert!(!is_transient(&StorageError::NotFound("x".into())));
        assert!(!is_transient(&StorageError::MissingToken("x".into())));
    }

    #[tokio::test]
    async fn retry_gives_up_on_permanent_error() {
        let calls = std::sync::atomic::AtomicU32::new(0);
        let r: Result<(), _> = with_retry(|| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Err(StorageError::Api { status: 403, body: "нет прав".into() }) }
        })
        .await;
        assert!(r.is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
