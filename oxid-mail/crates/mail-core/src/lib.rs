// mail-core: универсальное ядро Oxid Mail (общее для standalone/Tauri/bridge)
pub mod account;
pub mod chain;
pub mod contacts;
pub mod crypto_mail;
pub mod identity;
pub mod idle;
pub mod imap_client;
pub mod message;
pub mod naming;
pub mod p2p;
pub mod oauth;
pub mod service;
pub mod smtp_client;

use std::sync::Once;

static TLS_INIT: Once = Once::new();

/// Установить ring как процессный crypto-provider для rustls (идемпотентно).
pub fn init_tls() {
    TLS_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
