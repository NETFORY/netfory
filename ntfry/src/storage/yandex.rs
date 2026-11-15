//! YandexDiskProvider — гибрид WebDAV + REST, всё внутри /ntfry/.
//! Горячий путь (PUT/GET чанков, индекса, словаря) — WebDAV
//! (webdav.yandex.ru): одна HTTP-транзакция вместо двухфазного REST
//! (получить href → передать байты) — заметно быстрее на мелких чанках.
//! REST остаётся для того, чего WebDAV не умеет: sha256 индекса, квота
//! диска, permanent-удаление (мимо корзины).
//! Аутентификация везде одинаковая: `Authorization: OAuth <token>`.

use async_trait::async_trait;
use serde::Deserialize;

use super::StorageProvider;
use crate::error::StorageError;

const API: &str = "https://cloud-api.yandex.net/v1/disk/resources";
const WEBDAV: &str = "https://webdav.yandex.ru";

pub struct YandexDiskProvider {
    client: reqwest::Client,
    token: String,
}

#[derive(Deserialize)]
struct Meta {
    sha256: Option<String>,
}

/// Логин и квота аккаунта Яндекса по OAuth-токену (без открытия vault).
/// Один запрос GET /v1/disk/ отдаёт и пользователя, и total/used.
pub async fn account_info(token: &str) -> Result<super::AccountInfo, StorageError> {
    #[derive(Deserialize)]
    struct User {
        login: Option<String>,
        display_name: Option<String>,
    }
    #[derive(Deserialize)]
    struct Disk {
        user: Option<User>,
        total_space: Option<u64>,
        used_space: Option<u64>,
    }
    let resp = reqwest::Client::new()
        .get("https://cloud-api.yandex.net/v1/disk/")
        .header("Authorization", format!("OAuth {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(StorageError::Api { status, body: body.chars().take(300).collect() });
    }
    let d: Disk = resp.json().await?;
    let login = d
        .user
        .and_then(|u| u.login.or(u.display_name))
        .unwrap_or_default();
    Ok(super::AccountInfo {
        login,
        total: d.total_space.unwrap_or(0),
        used: d.used_space.unwrap_or(0),
    })
}

impl YandexDiskProvider {
    pub fn new(token: String) -> Self {
        // connect_timeout: провайдер не должен висеть бесконечно при обрыве сети
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self { client, token }
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.header("Authorization", format!("OAuth {}", self.token))
    }

    async fn api_err(resp: reqwest::Response) -> StorageError {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        StorageError::Api { status, body: body.chars().take(300).collect() }
    }

    /// WebDAV MKCOL — 201 создано, 405 уже существует (оба — успех).
    async fn mkdir(&self, path: &str) -> Result<(), StorageError> {
        let mkcol = reqwest::Method::from_bytes(b"MKCOL").expect("MKCOL валиден");
        let resp = self
            .auth(self.client.request(mkcol, format!("{WEBDAV}{path}")))
            .send()
            .await?;
        match resp.status().as_u16() {
            200 | 201 | 405 => Ok(()),
            _ => Err(Self::api_err(resp).await),
        }
    }

    /// WebDAV PUT — одна транзакция (вместо REST href-цепочки).
    async fn upload(&self, path: &str, data: &[u8]) -> Result<(), StorageError> {
        let resp = self
            .auth(self.client.put(format!("{WEBDAV}{path}")).body(data.to_vec()))
            .send()
            .await?;
        match resp.status().as_u16() {
            200 | 201 | 204 => Ok(()),
            _ => Err(Self::api_err(resp).await),
        }
    }

    /// WebDAV GET — одна транзакция.
    async fn download(&self, path: &str) -> Result<Vec<u8>, StorageError> {
        let resp = self
            .auth(self.client.get(format!("{WEBDAV}{path}")))
            .send()
            .await?;
        if resp.status().as_u16() == 404 {
            return Err(StorageError::NotFound(path.to_string()));
        }
        if !resp.status().is_success() {
            return Err(Self::api_err(resp).await);
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// Метаданные ресурса (sha256 считает сам Yandex).
    async fn meta_sha256(&self, path: &str) -> Result<Option<String>, StorageError> {
        let resp = self
            .auth(self.client.get(API).query(&[("path", path), ("fields", "sha256")]))
            .send()
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(Self::api_err(resp).await);
        }
        let meta: Meta = resp.json().await?;
        Ok(meta.sha256)
    }

    /// Существует ли ресурс по пути (дешёвый запрос метаданных).
    async fn exists(&self, path: &str) -> Result<bool, StorageError> {
        let resp = self
            .auth(self.client.get(API).query(&[("path", path), ("fields", "path")]))
            .send()
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(false);
        }
        if !resp.status().is_success() {
            return Err(Self::api_err(resp).await);
        }
        Ok(true)
    }
}

#[async_trait]
impl StorageProvider for YandexDiskProvider {
    async fn init_vault_dir(&self) -> Result<(), StorageError> {
        self.mkdir("/ntfry").await?;
        self.mkdir("/ntfry/chunks").await
    }

    async fn upload_chunk(&self, id: &str, data: &[u8]) -> Result<(), StorageError> {
        self.upload(&format!("/ntfry/chunks/{id}"), data).await
    }

    async fn download_chunk(&self, id: &str) -> Result<Vec<u8>, StorageError> {
        self.download(&format!("/ntfry/chunks/{id}")).await
    }

    async fn chunk_exists(&self, id: &str) -> Result<bool, StorageError> {
        self.exists(&format!("disk:/ntfry/chunks/{id}")).await
    }

    async fn upload_index(&self, encrypted_index: &[u8]) -> Result<(), StorageError> {
        self.upload("/ntfry/index.enc", encrypted_index).await
    }

    async fn download_index(&self) -> Result<Vec<u8>, StorageError> {
        self.download("/ntfry/index.enc").await
    }

    async fn remote_index_sha256(&self) -> Result<Option<String>, StorageError> {
        self.meta_sha256("disk:/ntfry/index.enc").await
    }

    async fn download_dict(&self) -> Result<Option<Vec<u8>>, StorageError> {
        match self.download("/ntfry/user.dict").await {
            Ok(d) => Ok(Some(d)),
            Err(StorageError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn upload_dict(&self, data: &[u8]) -> Result<(), StorageError> {
        self.upload("/ntfry/user.dict", data).await
    }

    async fn delete_vault(&self) -> Result<(), StorageError> {
        let resp = self
            .auth(self.client.delete(API).query(&[
                ("path", "disk:/ntfry"),
                ("permanently", "true"),
            ]))
            .send()
            .await?;
        match resp.status().as_u16() {
            200 | 202 | 204 | 404 => Ok(()),
            _ => Err(Self::api_err(resp).await),
        }
    }

    async fn delete_chunk(&self, id: &str) -> Result<(), StorageError> {
        let path = format!("disk:/ntfry/chunks/{id}");
        let resp = self
            .auth(self.client.delete(API).query(&[("path", path.as_str()), ("permanently", "true")]))
            .send()
            .await?;
        match resp.status().as_u16() {
            200 | 202 | 204 | 404 => Ok(()),
            _ => Err(Self::api_err(resp).await),
        }
    }

    async fn download_meta(&self) -> Result<Option<Vec<u8>>, StorageError> {
        match self.download("/ntfry/meta").await {
            Ok(d) => Ok(Some(d)),
            Err(StorageError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn upload_meta(&self, data: &[u8]) -> Result<(), StorageError> {
        self.upload("/ntfry/meta", data).await
    }

    async fn quota(&self) -> Result<Option<(u64, u64)>, StorageError> {
        #[derive(Deserialize)]
        struct Disk {
            total_space: Option<u64>,
            used_space: Option<u64>,
        }
        let resp = self
            .auth(self.client.get("https://cloud-api.yandex.net/v1/disk/").query(&[(
                "fields",
                "total_space,used_space",
            )]))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let d: Disk = resp.json().await?;
        Ok(match (d.total_space, d.used_space) {
            (Some(t), Some(u)) => Some((t, u)),
            _ => None,
        })
    }
}
