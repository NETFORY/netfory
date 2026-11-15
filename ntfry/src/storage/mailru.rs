//! MailruCloudProvider — Облако Mail.ru через официальный WebDAV
//! (webdav.cloud.mail.ru). У облака Mail.ru НЕТ публичного REST API и OAuth:
//! официальный путь для сторонних приложений — WebDAV с Basic-аутентификацией
//! (email + «пароль для внешнего приложения» из настроек безопасности).
//! ВАЖНО: WebDAV Mail.ru доступен только на платных тарифах (Mail Space).
//! Квота: RFC 4331 PROPFIND; фолбэк — неофициальный o2.mail.ru + api/m1/user.

use async_trait::async_trait;

use super::StorageProvider;
use crate::error::StorageError;

const WEBDAV: &str = "https://webdav.cloud.mail.ru";

pub struct MailruCloudProvider {
    client: reqwest::Client,
    login: String,
    password: String,
}

/// Логин и квота аккаунта по «email:пароль_приложения» (валидирует доступ:
/// невалидные креды или бесплатный тариф → ошибка 401/403 от WebDAV).
pub async fn account_info(creds: &str) -> Result<super::AccountInfo, StorageError> {
    let (login, password) = split_creds(creds)?;
    let p = MailruCloudProvider::new(login.to_string(), password.to_string());
    p.check_auth().await?;
    let (total, used) = p.quota().await?.unwrap_or((0, 0));
    Ok(super::AccountInfo { login: login.to_string(), total, used })
}

/// «email:пароль» → (email, пароль). Пароль может содержать ':' — режем слева.
pub fn split_creds(creds: &str) -> Result<(&str, &str), StorageError> {
    creds
        .trim()
        .split_once(':')
        .filter(|(l, p)| !l.is_empty() && !p.is_empty())
        .ok_or_else(|| {
            StorageError::MissingToken("Mail.ru: нужны креды в формате email:пароль_приложения".into())
        })
}

impl MailruCloudProvider {
    pub fn new(login: String, password: String) -> Self {
        // Урок seeder-фикса: без read_timeout оборванный TCP висит навсегда.
        // pool_idle_timeout < серверного: Mail.ru быстро закрывает idle
        // keep-alive, и переиспользование мёртвого сокета даёт
        // «error sending request».
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .read_timeout(std::time::Duration::from_secs(60))
            .tcp_keepalive(std::time::Duration::from_secs(20))
            .pool_idle_timeout(std::time::Duration::from_secs(25))
            .build()
            .expect("reqwest client");
        Self { client, login, password }
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.basic_auth(&self.login, Some(&self.password))
    }

    async fn api_err(resp: reqwest::Response) -> StorageError {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        let hint = match status {
            401 => " (проверьте email и пароль для внешнего приложения)",
            402 | 403 => " (WebDAV Облака Mail.ru доступен только на платных тарифах Mail Space)",
            _ => "",
        };
        StorageError::Api { status, body: format!("{}{hint}", body.chars().take(300).collect::<String>()) }
    }

    /// PROPFIND Depth:0 корня — дешёвая проверка кредов и доступности WebDAV.
    async fn check_auth(&self) -> Result<(), StorageError> {
        let propfind = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND валиден");
        let resp = self
            .auth(self.client.request(propfind, format!("{WEBDAV}/")))
            .header("Depth", "0")
            .send()
            .await?;
        match resp.status().as_u16() {
            200..=299 => Ok(()),
            _ => Err(Self::api_err(resp).await),
        }
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

    /// Отправка с ретраями: под параллельной загрузкой чанков Mail.ru рвёт
    /// соединения (reset / stale keep-alive) и отдаёт 429/5xx. Транзиентные
    /// сбои повторяем до 4 раз с экспоненциальным бэкоффом (0.5с → 4с).
    async fn send_retry(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&[u8]>,
    ) -> Result<reqwest::Response, StorageError> {
        const RETRIES: u32 = 4;
        let url = format!("{WEBDAV}{path}");
        let mut delay = std::time::Duration::from_millis(500);
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut rb = self.auth(self.client.request(method.clone(), &url));
            if let Some(b) = body {
                rb = rb.body(b.to_vec());
            }
            match rb.send().await {
                Ok(resp) => {
                    let retryable = matches!(resp.status().as_u16(), 429 | 500 | 502 | 503 | 504);
                    if !retryable || attempt > RETRIES {
                        return Ok(resp);
                    }
                }
                Err(e) => {
                    let transient = e.is_connect() || e.is_timeout() || e.is_request();
                    if !transient || attempt > RETRIES {
                        return Err(e.into());
                    }
                }
            }
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
    }

    async fn upload(&self, path: &str, data: &[u8]) -> Result<(), StorageError> {
        let resp = self.send_retry(reqwest::Method::PUT, path, Some(data)).await?;
        match resp.status().as_u16() {
            200 | 201 | 204 => Ok(()),
            _ => Err(Self::api_err(resp).await),
        }
    }

    async fn download(&self, path: &str) -> Result<Vec<u8>, StorageError> {
        let resp = self.send_retry(reqwest::Method::GET, path, None).await?;
        if resp.status().as_u16() == 404 {
            return Err(StorageError::NotFound(path.to_string()));
        }
        if !resp.status().is_success() {
            return Err(Self::api_err(resp).await);
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// WebDAV DELETE (у Mail.ru нет REST-удаления «мимо корзины»).
    async fn dav_delete(&self, path: &str) -> Result<(), StorageError> {
        let resp = self.send_retry(reqwest::Method::DELETE, path, None).await?;
        match resp.status().as_u16() {
            200 | 202 | 204 | 404 => Ok(()),
            _ => Err(Self::api_err(resp).await),
        }
    }

    /// Существование ресурса: PROPFIND Depth:0 (207 — есть, 404 — нет).
    async fn exists(&self, path: &str) -> Result<bool, StorageError> {
        let propfind = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND валиден");
        let resp = self
            .auth(self.client.request(propfind, format!("{WEBDAV}{path}")))
            .header("Depth", "0")
            .send()
            .await?;
        match resp.status().as_u16() {
            207 | 200 => Ok(true),
            404 => Ok(false),
            _ => Err(Self::api_err(resp).await),
        }
    }

    /// Квота RFC 4331: PROPFIND quota-used-bytes / quota-available-bytes.
    async fn quota_rfc4331(&self) -> Result<Option<(u64, u64)>, StorageError> {
        const BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:prop><D:quota-available-bytes/><D:quota-used-bytes/></D:prop></D:propfind>"#;
        let propfind = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND валиден");
        let resp = self
            .auth(self.client.request(propfind, format!("{WEBDAV}/")))
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(BODY)
            .send()
            .await?;
        if !matches!(resp.status().as_u16(), 200 | 207) {
            return Ok(None);
        }
        let xml = resp.text().await?;
        let used = xml_num(&xml, "quota-used-bytes");
        let avail = xml_num(&xml, "quota-available-bytes");
        Ok(match (used, avail) {
            (Some(u), Some(a)) => Some((u + a, u)),
            _ => None,
        })
    }

    /// Фолбэк: неофициальный API (как у сторонних клиентов) —
    /// o2.mail.ru password-grant → cloud.mail.ru/api/m1/user. Любая ошибка → None.
    async fn quota_unofficial(&self) -> Option<(u64, u64)> {
        #[derive(serde::Deserialize)]
        struct Tok {
            access_token: String,
        }
        let tok: Tok = self
            .client
            .post("https://o2.mail.ru/token")
            .form(&[
                ("client_id", "cloud-win"),
                ("grant_type", "password"),
                ("username", self.login.as_str()),
                ("password", self.password.as_str()),
            ])
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        let v: serde_json::Value = self
            .client
            .get("https://cloud.mail.ru/api/m1/user")
            .query(&[("access_token", tok.access_token.as_str())])
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        let space = v.get("body")?.get("cloud")?.get("space")?;
        let num = |k: &str| space.get(k).and_then(|x| x.as_u64());
        if let (Some(t), Some(u)) = (num("bytes_total"), num("bytes_used")) {
            return Some((t, u));
        }
        // старый формат: total/used в мегабайтах
        if let (Some(t), Some(u)) = (num("total"), num("used")) {
            return Some((t * 1_048_576, u * 1_048_576));
        }
        None
    }
}

/// Достаёт число из первого XML-тега `<…tag>N</…>` (без XML-парсера).
pub(crate) fn xml_num(xml: &str, tag: &str) -> Option<u64> {
    let open = format!("{tag}>");
    let pos = xml.find(&open)?;
    let rest = &xml[pos + open.len()..];
    let end = rest.find('<')?;
    rest[..end].trim().parse().ok()
}

#[async_trait]
impl StorageProvider for MailruCloudProvider {
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
        self.exists(&format!("/ntfry/chunks/{id}")).await
    }

    async fn upload_index(&self, encrypted_index: &[u8]) -> Result<(), StorageError> {
        self.upload("/ntfry/index.enc", encrypted_index).await
    }

    async fn download_index(&self) -> Result<Vec<u8>, StorageError> {
        self.download("/ntfry/index.enc").await
    }

    /// WebDAV Mail.ru не отдаёт sha256 — фоновая валидация кэша недоступна.
    async fn remote_index_sha256(&self) -> Result<Option<String>, StorageError> {
        Ok(None)
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
        self.dav_delete("/ntfry").await
    }

    async fn delete_chunk(&self, id: &str) -> Result<(), StorageError> {
        self.dav_delete(&format!("/ntfry/chunks/{id}")).await
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
        if let Some(q) = self.quota_rfc4331().await? {
            return Ok(Some(q));
        }
        Ok(self.quota_unofficial().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creds_split_left_password_may_contain_colon() {
        let (l, p) = split_creds("user@mail.ru:ab:cd").unwrap();
        assert_eq!(l, "user@mail.ru");
        assert_eq!(p, "ab:cd");
        assert!(split_creds("nopass").is_err());
        assert!(split_creds(":pass").is_err());
        assert!(split_creds("login:").is_err());
    }

    #[test]
    fn xml_quota_extraction() {
        let xml = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:propstat><d:prop>
<d:quota-used-bytes>1024</d:quota-used-bytes>
<d:quota-available-bytes> 2048 </d:quota-available-bytes>
</d:prop></d:propstat></d:response></d:multistatus>"#;
        assert_eq!(xml_num(xml, "quota-used-bytes"), Some(1024));
        assert_eq!(xml_num(xml, "quota-available-bytes"), Some(2048));
        assert_eq!(xml_num(xml, "quota-missing"), None);
    }
}
