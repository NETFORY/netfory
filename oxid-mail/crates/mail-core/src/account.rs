use serde::{Deserialize, Serialize};

/// Полная запись аккаунта (хранится ТОЛЬКО в зашифрованном mail-store).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailAccount {
    pub id: String,
    pub email: String,
    pub password: String,
    pub provider: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    /// "password" (по умолчанию) | "oauth" (Gmail XOAUTH2)
    #[serde(default)]
    pub auth_kind: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub token_expires_at: u64,
}

impl MailAccount {
    pub fn is_oauth(&self) -> bool {
        self.auth_kind == "oauth"
    }

    /// Системный P2P-аккаунт SmartNet (без IMAP/SMTP, доставка через Iroh).
    pub fn is_p2p(&self) -> bool {
        self.provider == "smartnet"
    }
}

/// Публичная проекция без пароля — уходит на фронтенд.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPublic {
    pub id: String,
    pub email: String,
    pub provider: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub auth_kind: String,
}

impl MailAccount {
    pub fn public(&self) -> AccountPublic {
        AccountPublic {
            id: self.id.clone(),
            email: self.email.clone(),
            provider: self.provider.clone(),
            imap_host: self.imap_host.clone(),
            imap_port: self.imap_port,
            smtp_host: self.smtp_host.clone(),
            smtp_port: self.smtp_port,
            auth_kind: if self.auth_kind.is_empty() { "password".into() } else { self.auth_kind.clone() },
        }
    }
}
