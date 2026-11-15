//! NETFORY chat core — общая часть мессенджера и Netfory.
//!
//! * `identity` — детерминированный вывод ключей из сид-фразы: чат-ключ X25519
//!   (`xkey`, тот же, что в Netfory — переписка с существующими контактами не ломается)
//!   и секрет iroh-endpoint'а мессенджера (новый транспортный EndpointId).
//! * `ipc` — JSONL-протокол сайдкара (Netfory ⇄ ntfry-messenger по stdin/stdout).
//! * `host` — абстракции «источник идентичности» и «кошелёк» (IPC или локальная подпись).

pub mod identity {
    use sha2::{Digest, Sha256};

    /// 32 байта секрета чат-ключа X25519 — ровно как `chat/crypto.rs::my_secret` в Netfory:
    /// sha256("smartnet-x25519-v1" + seed.trim()). Менять нельзя — сломает все комнаты.
    pub fn chat_xkey_material(seed: &str) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"smartnet-x25519-v1");
        h.update(seed.trim().as_bytes());
        h.finalize().into()
    }

    /// Секрет iroh-endpoint'а мессенджера (ed25519 seed): отдельный домен, чтобы
    /// EndpointId мессенджера не совпадал с Netfory и не выводился из xkey.
    pub fn messenger_endpoint_secret(seed: &str) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"netfory-messenger-endpoint|v1|");
        h.update(seed.trim().as_bytes());
        h.finalize().into()
    }

    /// Короткий отпечаток сида для сверки «тот же аккаунт» без раскрытия сида.
    pub fn seed_fingerprint(seed: &str) -> String {
        let mut h = Sha256::new();
        h.update(b"netfory-seed-fp|");
        h.update(seed.trim().as_bytes());
        hex::encode(&h.finalize()[..8])
    }
}

pub mod ipc {
    use serde::{Deserialize, Serialize};

    /// Netfory → мессенджер (stdin, по строке JSON).
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum ToMessenger {
        /// Рукопожатие: сид-фраза сессии (только RAM), адрес кошелька, язык UI.
        Hello { seed: String, address: String, #[serde(default)] lang: String, #[serde(default)] data_dir: String },
        /// Показать/сфокусировать окно (повторный клик «Мессенджер»).
        Focus,
        /// Открыть ссылку: u://, sth://contact?…, sn://group/…
        Open { url: String },
        /// Ответ на запрос резолва имени/адреса.
        Resolved { req: u64, #[serde(default)] value: Option<String> },
        /// Результат платежа по запросу `PayRequest`.
        PayResult { req: u64, ok: bool, #[serde(default)] tx_id: String, #[serde(default)] error: String },
        /// Архив чата Netfory для разовой миграции (JSON `chat_export_archive`).
        Archive { json: String },
        /// Ответ на `HostCall`: `value` — JSON результата команды Netfory, `error` — текст ошибки.
        HostResult { req: u64, ok: bool, #[serde(default)] value: serde_json::Value, #[serde(default)] error: String },
        /// Netfory завершает сессию — мессенджер закрывается.
        Shutdown,
    }

    /// Мессенджер → Netfory (stdout, по строке JSON).
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum ToHost {
        Ready { version: String, endpoint_id: String },
        Unread { total: u64 },
        NewMessage { room: String, from: String, preview: String },
        /// Запрос резолва: kind = "username" (имя → адрес) | "address" (адрес → имя).
        Resolve { req: u64, kind: String, q: String },
        /// Запрос платежа кошельку Netfory (платный вход в группу, донат).
        PayRequest { req: u64, recipient: String, amount: String, fee: String, vendor: String, reason: String },
        /// Открыть в Netfory (профиль, кошелёк).
        OpenInHost { url: String },
        /// Запрос архива чата для разовой миграции.
        ExportRequest { req: u64 },
        /// Прокси Tauri-команды Netfory (белый список в messenger_launcher): имя, баланс, аватары, подписки.
        HostCall { req: u64, cmd: String, #[serde(default)] args: serde_json::Value },
        Log { scope: String, msg: String },
    }
}

pub mod host {
    /// Источник идентичности мессенджера.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Mode {
        /// Запущен Netfory: сид получен по stdin, кошелёк — по IPC.
        Sidecar,
        /// Отдельная программа: сид введён пользователем, кошелёк — локальная подпись.
        Standalone,
    }

    /// Исход платежа.
    #[derive(Debug, Clone)]
    pub struct PayOutcome {
        pub tx_id: String,
        pub accepted: bool,
        pub reason: String,
    }

    /// Кошелёк для платежей из чата. Реализации: IPC к Netfory (desktop sidecar),
    /// локальная подпись (standalone/Android — этап 2), заглушка «скоро».
    pub trait Wallet: Send + Sync {
        fn pay(&self, recipient: &str, amount_sth: &str, fee_sth: &str, vendor: &str, reason: &str) -> Result<PayOutcome, String>;
    }

    pub struct NotAvailable;
    impl Wallet for NotAvailable {
        fn pay(&self, _r: &str, _a: &str, _f: &str, _v: &str, _why: &str) -> Result<PayOutcome, String> {
            Err("Платежи из чата появятся в следующей версии мессенджера".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::identity::*;
    #[test]
    fn deterministic_and_distinct() {
        let a = chat_xkey_material("abandon abandon ability");
        assert_eq!(a, chat_xkey_material(" abandon abandon ability "));
        assert_ne!(a, messenger_endpoint_secret("abandon abandon ability"));
        assert_eq!(seed_fingerprint("x").len(), 16);
    }
}
