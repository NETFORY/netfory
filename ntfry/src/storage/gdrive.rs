//! GoogleDriveProvider — Google Drive API v3, все данные в папке /ntfry
//! (подпапка chunks/). Аутентификация: Bearer OAuth-токен со скоупом
//! https://www.googleapis.com/auth/drive.file. Файлы адресуются по имени
//! внутри своей папки; ID папок кэшируются на время жизни провайдера.

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::Mutex;

use super::StorageProvider;
use crate::error::StorageError;

const API: &str = "https://www.googleapis.com/drive/v3/files";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// Email и квота аккаунта Google по OAuth-токену (без открытия vault).
pub async fn account_info(token: &str) -> Result<super::AccountInfo, StorageError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct User {
        email_address: Option<String>,
        display_name: Option<String>,
    }
    #[derive(Deserialize)]
    struct Quota {
        limit: Option<String>,
        usage: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct About {
        user: Option<User>,
        storage_quota: Option<Quota>,
    }
    let resp = reqwest::Client::new()
        .get("https://www.googleapis.com/drive/v3/about")
        .bearer_auth(token)
        .query(&[("fields", "user(emailAddress,displayName),storageQuota(limit,usage)")])
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(StorageError::Api { status, body: body.chars().take(300).collect() });
    }
    let a: About = resp.json().await?;
    let login = a
        .user
        .and_then(|u| u.email_address.or(u.display_name))
        .unwrap_or_default();
    let (total, used) = a
        .storage_quota
        .map(|q| {
            (
                q.limit.and_then(|s| s.parse().ok()).unwrap_or(0),
                q.usage.and_then(|s| s.parse().ok()).unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));
    Ok(super::AccountInfo { login, total, used })
}

pub struct GoogleDriveProvider {
    client: reqwest::Client,
    token: String,
    /// (id папки ntfry, id папки chunks) — ленивый кэш.
    ids: Mutex<Option<(String, String)>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileMeta {
    id: String,
    #[serde(default)]
    sha256_checksum: Option<String>,
}

#[derive(Deserialize)]
struct FileList {
    #[serde(default)]
    files: Vec<FileMeta>,
}

impl GoogleDriveProvider {
    pub fn new(token: String) -> Self {
        // connect_timeout: провайдер не должен висеть бесконечно при обрыве сети
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self { client, token, ids: Mutex::new(None) }
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.bearer_auth(&self.token)
    }

    async fn api_err(resp: reqwest::Response) -> StorageError {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        StorageError::Api { status, body: body.chars().take(300).collect() }
    }

    /// Поиск файла/папки по имени внутри родителя (trashed=false).
    async fn find(&self, name: &str, parent: &str, fields: &str) -> Result<Option<FileMeta>, StorageError> {
        let q = format!("name='{name}' and '{parent}' in parents and trashed=false");
        let resp = self
            .auth(self.client.get(API).query(&[
                ("q", q.as_str()),
                ("fields", &format!("files({fields})") as &str),
                ("pageSize", "1"),
            ]))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Self::api_err(resp).await);
        }
        let list: FileList = resp.json().await?;
        Ok(list.files.into_iter().next())
    }

    async fn create_folder(&self, name: &str, parent: &str) -> Result<String, StorageError> {
        let resp = self
            .auth(self.client.post(API).query(&[("fields", "id")]))
            .json(&serde_json::json!({ "name": name, "mimeType": FOLDER_MIME, "parents": [parent] }))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Self::api_err(resp).await);
        }
        let meta: FileMeta = resp.json().await?;
        Ok(meta.id)
    }

    /// Гарантирует /ntfry и /ntfry/chunks, кэширует их ID.
    async fn folders(&self) -> Result<(String, String), StorageError> {
        let mut guard = self.ids.lock().await;
        if let Some(ids) = guard.clone() {
            return Ok(ids);
        }
        let ntfry = match self.find("ntfry", "root", "id").await? {
            Some(m) => m.id,
            None => self.create_folder("ntfry", "root").await?,
        };
        let chunks = match self.find("chunks", &ntfry, "id").await? {
            Some(m) => m.id,
            None => self.create_folder("chunks", &ntfry).await?,
        };
        *guard = Some((ntfry.clone(), chunks.clone()));
        Ok((ntfry, chunks))
    }

    /// Создание файла: multipart/related (метаданные JSON + бинарный контент).
    async fn create_file(&self, name: &str, parent: &str, data: &[u8]) -> Result<(), StorageError> {
        const BOUNDARY: &str = "ntfry_multipart_boundary";
        let meta = serde_json::json!({ "name": name, "parents": [parent] }).to_string();
        let mut body: Vec<u8> = Vec::with_capacity(data.len() + meta.len() + 256);
        body.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n").as_bytes());
        body.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{BOUNDARY}--").as_bytes());
        let resp = self
            .auth(self.client.post(UPLOAD).query(&[("uploadType", "multipart"), ("fields", "id")]))
            .header("Content-Type", format!("multipart/related; boundary={BOUNDARY}"))
            .body(body)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Self::api_err(resp).await);
        }
        Ok(())
    }

    /// Перезапись контента существующего файла по ID.
    async fn update_file(&self, id: &str, data: &[u8]) -> Result<(), StorageError> {
        let resp = self
            .auth(self.client.patch(format!("{UPLOAD}/{id}")).query(&[("uploadType", "media")]))
            .header("Content-Type", "application/octet-stream")
            .body(data.to_vec())
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Self::api_err(resp).await);
        }
        Ok(())
    }

    /// Создать или перезаписать файл по имени внутри родителя.
    async fn upload_named(&self, name: &str, parent: &str, data: &[u8]) -> Result<(), StorageError> {
        match self.find(name, parent, "id").await? {
            Some(m) => self.update_file(&m.id, data).await,
            None => self.create_file(name, parent, data).await,
        }
    }

    async fn download_named(&self, name: &str, parent: &str) -> Result<Vec<u8>, StorageError> {
        let meta = self
            .find(name, parent, "id")
            .await?
            .ok_or_else(|| StorageError::NotFound(name.to_string()))?;
        let resp = self
            .auth(self.client.get(format!("{API}/{}", meta.id)).query(&[("alt", "media")]))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Self::api_err(resp).await);
        }
        Ok(resp.bytes().await?.to_vec())
    }
}

#[async_trait]
impl StorageProvider for GoogleDriveProvider {
    async fn init_vault_dir(&self) -> Result<(), StorageError> {
        self.folders().await.map(|_| ())
    }

    async fn upload_chunk(&self, id: &str, data: &[u8]) -> Result<(), StorageError> {
        let (_, chunks) = self.folders().await?;
        // чанки content-addressed и иммутабельны — создаём без поиска дубля
        self.create_file(id, &chunks, data).await
    }

    async fn download_chunk(&self, id: &str) -> Result<Vec<u8>, StorageError> {
        let (_, chunks) = self.folders().await?;
        self.download_named(id, &chunks).await
    }

    async fn chunk_exists(&self, id: &str) -> Result<bool, StorageError> {
        let (_, chunks) = self.folders().await?;
        Ok(self.find(id, &chunks, "id").await?.is_some())
    }

    async fn upload_index(&self, encrypted_index: &[u8]) -> Result<(), StorageError> {
        let (ntfry, _) = self.folders().await?;
        self.upload_named("index.enc", &ntfry, encrypted_index).await
    }

    async fn download_index(&self) -> Result<Vec<u8>, StorageError> {
        let (ntfry, _) = self.folders().await?;
        self.download_named("index.enc", &ntfry).await
    }

    async fn remote_index_sha256(&self) -> Result<Option<String>, StorageError> {
        let (ntfry, _) = self.folders().await?;
        Ok(self
            .find("index.enc", &ntfry, "id,sha256Checksum")
            .await?
            .and_then(|m| m.sha256_checksum))
    }

    async fn download_dict(&self) -> Result<Option<Vec<u8>>, StorageError> {
        let (ntfry, _) = self.folders().await?;
        match self.download_named("user.dict", &ntfry).await {
            Ok(d) => Ok(Some(d)),
            Err(StorageError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn upload_dict(&self, data: &[u8]) -> Result<(), StorageError> {
        let (ntfry, _) = self.folders().await?;
        self.upload_named("user.dict", &ntfry, data).await
    }

    async fn delete_vault(&self) -> Result<(), StorageError> {
        let Some(meta) = self.find("ntfry", "root", "id").await? else {
            return Ok(());
        };
        let resp = self.auth(self.client.delete(format!("{API}/{}", meta.id))).send().await?;
        if !resp.status().is_success() && resp.status().as_u16() != 404 {
            return Err(Self::api_err(resp).await);
        }
        *self.ids.lock().await = None;
        Ok(())
    }

    async fn delete_chunk(&self, id: &str) -> Result<(), StorageError> {
        let (_, chunks) = self.folders().await?;
        let Some(meta) = self.find(id, &chunks, "id").await? else {
            return Ok(());
        };
        let resp = self.auth(self.client.delete(format!("{API}/{}", meta.id))).send().await?;
        if !resp.status().is_success() && resp.status().as_u16() != 404 {
            return Err(Self::api_err(resp).await);
        }
        Ok(())
    }

    async fn download_meta(&self) -> Result<Option<Vec<u8>>, StorageError> {
        let (ntfry, _) = self.folders().await?;
        match self.download_named("meta", &ntfry).await {
            Ok(d) => Ok(Some(d)),
            Err(StorageError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn upload_meta(&self, data: &[u8]) -> Result<(), StorageError> {
        let (ntfry, _) = self.folders().await?;
        self.upload_named("meta", &ntfry, data).await
    }

    async fn quota(&self) -> Result<Option<(u64, u64)>, StorageError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Quota {
            limit: Option<String>,
            usage: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct About {
            storage_quota: Option<Quota>,
        }
        let resp = self
            .auth(self.client.get("https://www.googleapis.com/drive/v3/about").query(&[(
                "fields",
                "storageQuota",
            )]))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let a: About = resp.json().await?;
        Ok(a.storage_quota.and_then(|q| {
            let t = q.limit.and_then(|s| s.parse::<u64>().ok())?;
            let u = q.usage.and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
            Some((t, u))
        }))
    }
}
