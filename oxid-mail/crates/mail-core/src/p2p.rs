// Прямая P2P-доставка NTFRY-писем по Iroh (QUIC + hole-punching, реле n0).
// Нода стартует при открытии сессии; ключ ноды детерминированно из seed.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use iroh::{Endpoint, EndpointId, SecretKey};
use mail_crypto::keys;
use mail_store::MailStore;
use serde::Serialize;

pub const OXID_ALPN: &[u8] = b"oxid-mail/p2p/1";
const MAX_MSG: usize = 64 * 1024 * 1024;
/// Размер чанка при отдаче конверта — для честного «file X of Y» прогресса.
const CHUNK_SIZE: usize = 256 * 1024;

static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static EP: OnceLock<Endpoint> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

/// Прогресс P2P-раздачи одного конверта. total/sent — байты payload (NTFRY),
/// file_sizes — исходные размеры вложений: UI сам считает «файл X из Y»
/// пропорцией переданных байт.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressState {
    pub total: u64,
    pub sent: u64,
    pub file_sizes: Vec<u64>,
    pub done: bool,
    pub ok: bool,
    pub error: Option<String>,
}

static PROGRESS: OnceLock<Mutex<HashMap<String, ProgressState>>> = OnceLock::new();

fn progress_map() -> &'static Mutex<HashMap<String, ProgressState>> {
    PROGRESS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn init_progress(id: &str, total: u64, file_sizes: Vec<u64>) {
    let mut m = progress_map().lock().unwrap();
    m.insert(
        id.to_string(),
        ProgressState { total, sent: 0, file_sizes, done: false, ok: false, error: None },
    );
    // Не даём мапе разрастаться: чистим завершённые старше некоторого числа записей.
    if m.len() > 64 {
        let stale: Vec<String> = m.iter().filter(|(_, s)| s.done).map(|(k, _)| k.clone()).collect();
        for k in stale.into_iter().take(m.len().saturating_sub(32)) {
            m.remove(&k);
        }
    }
}

fn set_progress_sent(id: &str, sent: u64) {
    if let Some(s) = progress_map().lock().unwrap().get_mut(id) {
        s.sent = sent.min(s.total);
    }
}

pub(crate) fn finish_progress(id: &str, ok: bool, error: Option<String>) {
    if let Some(s) = progress_map().lock().unwrap().get_mut(id) {
        s.done = true;
        s.ok = ok;
        if ok {
            s.sent = s.total;
        }
        s.error = error;
    }
}

pub fn get_progress(id: &str) -> Option<ProgressState> {
    progress_map().lock().unwrap().get(id).cloned()
}

fn rt() -> &'static tokio::runtime::Runtime {
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("p2p tokio runtime")
    })
}

pub fn node_id_hex() -> Option<String> {
    EP.get().map(|e| hex::encode(*e.id().as_bytes()))
}

/// Запуск ноды (идемпотентно на процесс). Вызывается при открытии сессии.
pub fn start(store: Arc<MailStore>, mnemonic: &str) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let master = keys::master_from_secret(mnemonic);
    let sk_bytes = keys::derive_iroh_signing_key(&master).to_bytes();
    let mnemonic = mnemonic.to_string();
    rt().spawn(async move {
        let secret = SecretKey::from_bytes(&sk_bytes);
        let ep = match Endpoint::builder(iroh::endpoint::presets::N0)
            .secret_key(secret)
            .alpns(vec![OXID_ALPN.to_vec()])
            .bind()
            .await
        {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[p2p] iroh bind failed: {e}");
                STARTED.store(false, Ordering::SeqCst);
                return;
            }
        };
        eprintln!("[p2p] oxid-mail node online: {}", ep.id());
        let _ = EP.set(ep.clone());
        while let Some(incoming) = ep.accept().await {
            let store = store.clone();
            let mnemonic = mnemonic.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_incoming(incoming, store, mnemonic).await {
                    eprintln!("[p2p] incoming error: {e}");
                }
            });
        }
    });
}

async fn handle_incoming(
    incoming: iroh::endpoint::Incoming,
    store: Arc<MailStore>,
    mnemonic: String,
) -> Result<(), String> {
    let conn = incoming.await.map_err(|e| e.to_string())?;
    let (mut send, mut recv) = conn.accept_bi().await.map_err(|e| e.to_string())?;
    let payload = recv.read_to_end(MAX_MSG).await.map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || store_incoming(&store, &mnemonic, &payload))
        .await
        .map_err(|e| e.to_string())??;
    send.write_all(b"OK").await.map_err(|e| e.to_string())?;
    let _ = send.finish();
    // даём QUIC время доставить ack до закрытия
    tokio::time::sleep(Duration::from_millis(300)).await;
    conn.close(0u32.into(), b"done");
    Ok(())
}

/// Входящее P2P-письмо: проверяем конверт, кладём во «Входящие» системного
/// P2P-аккаунта (fallback — папка "p2p" первого аккаунта).
fn store_incoming(store: &MailStore, mnemonic: &str, payload: &[u8]) -> Result<(), String> {
    if !mail_crypto::envelope::is_ntfry(payload) {
        return Err("не NTFRY-payload".into());
    }
    let master = keys::master_from_secret(mnemonic);
    let my_sk = keys::derive_x25519(&master);
    let opened = mail_crypto::envelope::open(payload, &my_sk, None)?;
    if !opened.signature_valid {
        return Err("подпись конверта не сходится — отброшено".into());
    }
    let sender_addr = crate::identity::address_from_pubkey(&opened.sender_pubkey);
    // Автоконтакт для ответа: reply-ключи из конверта (подпись уже проверена выше,
    // адрес отправителя выведен из того же подписного ключа — ключи аутентичны).
    if let (Some(xh), Some(ih)) = (&opened.content.reply_x25519_hex, &opened.content.reply_iroh_hex) {
        let email = format!("{sender_addr}{}", crate::contacts::P2P_SUFFIX);
        let existing = crate::contacts::get(store, &email);
        let has_keys = existing
            .as_ref()
            .map(|c| !c.x25519_hex.is_empty() && !c.iroh_node_hex.is_empty())
            .unwrap_or(false);
        if !has_keys {
            let mut c = existing.unwrap_or_else(|| crate::contacts::Contact {
                email: email.clone(),
                kind: "p2p".into(),
                sth_address: sender_addr.clone(),
                ..Default::default()
            });
            c.sth_address = sender_addr.clone();
            c.x25519_hex = xh.clone();
            c.iroh_node_hex = ih.clone();
            match crate::contacts::add(store, c) {
                Ok(_) => eprintln!("[p2p] auto-contact saved for {sender_addr}"),
                Err(e) => eprintln!("[p2p] auto-contact {sender_addr} failed: {e}"),
            }
        }
    }
    let accounts = crate::service::list_accounts(store).map_err(|e| e.to_string())?;
    let (acc_id, acc_email, folder) = accounts
        .iter()
        .find(|a| a.id == crate::service::P2P_ACCOUNT_ID)
        .map(|a| (a.id.clone(), a.email.clone(), "inbox"))
        .or_else(|| accounts.first().map(|a| (a.id.clone(), a.email.clone(), "p2p")))
        .ok_or("нет аккаунтов для сохранения p2p-письма")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let msg = crate::message::StoredMessage {
        id: format!("p2p_{now}"),
        account_id: acc_id.clone(),
        folder: folder.into(),
        from: crate::message::Address {
            name: "SmartNet P2P".into(),
            email: format!("{sender_addr}@sth"),
        },
        to: acc_email,
        subject: "🔒 [SmartNet Encrypted]".into(),
        snippet: "Direct P2P message — decrypt to read".into(),
        date: crate::message::now_rfc3339(),
        read: false,
        starred: false,
        encrypted: true,
        labels: vec![],
        attachments: vec![],
        body: Some(mail_crypto::armor::armor_body(payload)),
        body_html: None,
        ntfry_sig: None,
        via: Some("p2p".into()),
    };
    store
        .put_json(&format!("msgs:{acc_id}"), &msg.id, &msg)
        .map_err(|e| e.to_string())?;
    store.flush();
    // Сигнал для UI: у P2P-аккаунта появилось новое письмо — обновиться в реальном времени.
    crate::idle::mark_dirty(&acc_id);
    eprintln!("[p2p] stored incoming message {} from {sender_addr}", msg.id);
    Ok(())
}

/// Отправка payload на ноду (блокирующая, ~до 20 сек). Err — считаем оффлайн.
pub fn send(node_hex: &str, payload: Vec<u8>) -> Result<(), String> {
    send_with_progress(node_hex, payload, None)
}

/// Отправка с честным прогрессом: конверт нарезается на чанки по 256 КБ,
/// каждый чанк — `send.write_all`; после каждого куска обновляется PROGRESS.
/// progress_id=None — прогресс не пишется (используется для дефолтного `send`).
pub fn send_with_progress(
    node_hex: &str,
    payload: Vec<u8>,
    progress_id: Option<String>,
) -> Result<(), String> {
    let ep = EP
        .get()
        .cloned()
        .ok_or("P2P-нода ещё не запущена (откройте сессию заново)")?;
    let bytes: [u8; 32] = hex::decode(node_hex)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or("битый Iroh NodeId")?;
    let node = EndpointId::from_bytes(&bytes).map_err(|e| e.to_string())?;
    let total = payload.len() as u64;
    let pid = progress_id.clone();
    rt().block_on(async move {
        let conn = tokio::time::timeout(Duration::from_secs(8), ep.connect(node, OXID_ALPN))
            .await
            .map_err(|_| "таймаут соединения — нода получателя оффлайн".to_string())?
            .map_err(|e| format!("не удалось подключиться: {e}"))?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;

        // Стримим чанками, обновляя прогресс. Верхняя граница sent — total-1,
        // финальный total выставляется в finish_progress после ACK получателя.
        let mut sent: u64 = 0;
        for chunk in payload.chunks(CHUNK_SIZE) {
            send.write_all(chunk).await.map_err(|e| e.to_string())?;
            sent += chunk.len() as u64;
            if let Some(id) = pid.as_deref() {
                // Держим прогресс чуть ниже total, чтобы 100% выставлял только ACK.
                set_progress_sent(id, sent.saturating_sub(1).min(total.saturating_sub(1)));
            }
        }
        let _ = send.finish();

        let ack = tokio::time::timeout(Duration::from_secs(15), recv.read_to_end(16))
            .await
            .map_err(|_| "нет подтверждения доставки".to_string())?
            .map_err(|e| e.to_string())?;
        conn.close(0u32.into(), b"done");
        if ack != b"OK" {
            return Err("получатель отклонил сообщение".into());
        }
        Ok(())
    })
}
