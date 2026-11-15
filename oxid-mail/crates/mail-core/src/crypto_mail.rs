// crypto_mail: привязка email↔STH (Proof-of-Ownership), реестр, NTFRY-письма.
// Все функции блокирующие (вызывать из spawn_blocking).
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::chain::{self, RegEntry, RegistryIndex};
use crate::identity;
use crate::imap_client;
use crate::message::StoredMessage;
use crate::service::{ensure_token, get_account, CoreError};
use crate::smtp_client;
use mail_crypto::envelope;
use mail_crypto::registry as reg;
use mail_crypto::{armor, hash, keys, sig};
use mail_store::MailStore;

const REG_TREE: &str = "registry";
const BIND_TREE: &str = "bindings";
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn ntfry_tree(account_id: &str) -> String {
    format!("ntfry:{account_id}")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
//  H32-кэш и реестр
// ---------------------------------------------------------------------------

/// Тяжёлый Argon2-хэш email с кэшем в store (первый вызов ~0.5с, дальше мгновенно).
pub fn email_h32_cached(store: &MailStore, email: &str) -> Result<[u8; 32], CoreError> {
    let key = format!("h32:{}", hash::normalize_email(email));
    if let Some(hexed) = store.get_json::<String>(REG_TREE, &key)? {
        if let Ok(v) = hex::decode(&hexed) {
            if let Ok(arr) = <[u8; 32]>::try_from(v.as_slice()) {
                return Ok(arr);
            }
        }
    }
    let h32 = hash::email_h32(email);
    store.put_json(REG_TREE, &key, &hex::encode(h32))?;
    Ok(h32)
}

pub fn registry_load(store: &MailStore) -> RegistryIndex {
    store
        .get_json::<RegistryIndex>(REG_TREE, "index")
        .ok()
        .flatten()
        .unwrap_or_default()
}

pub fn registry_sync(store: &MailStore) -> Result<RegistryIndex, CoreError> {
    let mut idx = registry_load(store);
    chain::sync_registry(&mut idx)?;
    store.put_json(REG_TREE, "index", &idx)?;
    store.flush();
    Ok(idx)
}

/// Поиск записи получателя по email. `fresh` — форс-синк перед поиском.
pub fn lookup_email(store: &MailStore, email: &str, fresh: bool) -> Result<Option<RegEntry>, CoreError> {
    let h32 = email_h32_cached(store, email)?;
    let h16 = hash::h16(&h32);
    let idx = if fresh { registry_sync(store)? } else { registry_load(store) };
    if let Some(e) = idx.lookup(&h16) {
        return Ok(Some(e.clone()));
    }
    if !fresh {
        // не нашли в кэше — одна попытка досинхронизации
        let idx = registry_sync(store)?;
        return Ok(idx.lookup(&h16).cloned());
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
//  Привязка (Proof-of-Ownership)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BindingState {
    /// none | proving | tx_sent | confirmed | conflict | error
    pub status: String,
    pub email: String,
    pub address: String,
    pub txid: String,
    pub error: String,
    /// Провайдер подписал proof-письмо DKIM
    pub dkim: bool,
    pub updated: u64,
}

fn save_state(store: &MailStore, account_id: &str, st: &BindingState) -> Result<(), CoreError> {
    store.put_json(BIND_TREE, account_id, st)?;
    store.flush();
    Ok(())
}

/// Текущий статус привязки; при tx_sent — поллит подтверждение транзакции.
pub fn binding_status(store: &MailStore, account_id: &str) -> Result<BindingState, CoreError> {
    let mut st: BindingState = store
        .get_json(BIND_TREE, account_id)?
        .unwrap_or_else(|| BindingState { status: "none".into(), ..Default::default() });
    if st.status == "tx_sent" && !st.txid.is_empty() {
        if let Ok(Some(true)) = chain::tx_confirmed(&st.txid) {
            st.status = "confirmed".into();
            st.updated = now_secs();
            let _ = registry_sync(store);
            save_state(store, account_id, &st)?;
        }
    }
    Ok(st)
}

const PROOF_MARK: &str = "OXID-PROOF:";
const PROOF_SIG_MARK: &str = "OXID-SIG:";

/// Полный флоу привязки email к STH-адресу (блокирует до ~2 минут):
/// баланс → proof-письмо себе (SMTP) → IMAP-верификация подписи/DKIM → tx в реестр.
pub fn bind_email(store: &MailStore, mnemonic: &str, account_id: &str) -> Result<BindingState, CoreError> {
    let mut acc = get_account(store, account_id)?;
    if acc.is_p2p() {
        return Err(CoreError::Msg(
            "P2P-аккаунт SmartNet не требует привязки: ключи выводятся из seed".into(),
        ));
    }
    ensure_token(store, &mut acc)?;
    let ident = identity::from_secret(mnemonic)?;
    let master = keys::master_from_secret(mnemonic);
    let x25519_pub = keys::x25519_public(&keys::derive_x25519(&master));
    let iroh_node = keys::iroh_node_id(&master);
    let h32 = email_h32_cached(store, &acc.email)?;
    let h16 = hash::h16(&h32);

    let mut st = BindingState {
        status: "proving".into(),
        email: acc.email.clone(),
        address: ident.address.clone(),
        ..Default::default()
    };
    st.updated = now_secs();

    // Пре-чек реестра: конфликт / уже привязан
    let idx = registry_sync(store)?;
    if let Some(e) = idx.lookup(&h16) {
        if e.address != ident.address {
            st.status = "conflict".into();
            st.error = format!("Email уже привязан к другому адресу: {}", e.address);
            save_state(store, account_id, &st)?;
            return Ok(st);
        }
        if e.x25519_hex == hex::encode(x25519_pub) {
            st.status = "confirmed".into();
            save_state(store, account_id, &st)?;
            return Ok(st);
        }
        // тот же адрес, другие ключи — ротация, продолжаем
    }

    // Баланс
    let (balance, _) = chain::fetch_balance_nonce(&ident.address)?;
    let need = reg::BIND_AMOUNT + reg::BIND_FEE;
    if balance < need {
        st.status = "error".into();
        st.error = format!(
            "Недостаточно STH на {}: нужно 1.0 STH (0.5 + 0.5 комиссия), доступно {:.4}",
            ident.address,
            balance as f64 / chain::SMARTHOSHI as f64
        );
        save_state(store, account_id, &st)?;
        return Ok(st);
    }
    save_state(store, account_id, &st)?;

    // Proof-письмо самому себе
    let mut token_bytes = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut token_bytes);
    let token = hex::encode(token_bytes);
    let ts = now_secs();
    let proof_json = json!({
        "email": hash::normalize_email(&acc.email),
        "sthAddress": ident.address,
        "x25519": hex::encode(x25519_pub),
        "irohNodeId": hex::encode(iroh_node),
        "ts": ts,
    })
    .to_string();
    let subject = format!("SmartNet Proof-of-Ownership [{token}]");
    let proof_sig = sig::sign_message(&master, &acc.email, &acc.email, &subject, ts, proof_json.as_bytes())?;
    let body = format!(
        "Служебное письмо SmartNet Oxid Mail: подтверждение владения ящиком.\n\
         Не удаляйте его до завершения привязки.\n\n\
         {PROOF_MARK} {}\n{PROOF_SIG_MARK} {}\n",
        B64.encode(proof_json.as_bytes()),
        proof_sig
    );
    smtp_client::send_mail(&acc, &acc.email, &subject, &body, &[], &[])?;

    // IMAP-верификация: ждём письмо до 90 секунд
    let mut verified = false;
    let mut dkim = false;
    for _ in 0..9 {
        std::thread::sleep(std::time::Duration::from_secs(10));
        let fetched = match imap_client::sync_fetch(&acc, 15, 5) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for ff in &fetched {
            for m in &ff.messages {
                let raw_str = String::from_utf8_lossy(&m.raw);
                if !raw_str.contains(&token) {
                    continue;
                }
                let parsed = mail_parser::MessageParser::default().parse(&m.raw[..]);
                let text = parsed
                    .as_ref()
                    .and_then(|p| p.body_text(0))
                    .map(|c| c.to_string())
                    .unwrap_or_default();
                let Some((json_b64, sig_line)) = extract_proof(&text) else { continue };
                let Ok(json_bytes) = B64.decode(json_b64.trim()) else { continue };
                if sig::verify_message(&sig_line, &acc.email, &acc.email, &subject, &json_bytes).is_some() {
                    verified = true;
                    dkim = raw_str.to_ascii_lowercase().contains("dkim-signature:");
                }
            }
        }
        if verified {
            break;
        }
    }
    if !verified {
        st.status = "error".into();
        st.error = "Proof-письмо не пришло или подпись не сошлась (проверьте IMAP и повторите)".into();
        save_state(store, account_id, &st)?;
        return Ok(st);
    }
    st.dkim = dkim;

    // Транзакция в реестр
    let vendor = reg::encode_bind(&reg::BindRecord { h16, x25519: x25519_pub, iroh_node });
    let outcome = chain::send_registry_tx(mnemonic, &vendor)?;
    if !outcome.accepted {
        st.status = "error".into();
        st.error = format!("Нода отклонила транзакцию: {}", outcome.reason);
        save_state(store, account_id, &st)?;
        return Ok(st);
    }
    st.status = "tx_sent".into();
    st.txid = outcome.id;
    st.updated = now_secs();
    save_state(store, account_id, &st)?;
    Ok(st)
}

fn extract_proof(text: &str) -> Option<(String, String)> {
    let mut json_b64 = None;
    let mut sig_line = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix(PROOF_MARK) {
            json_b64 = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix(PROOF_SIG_MARK) {
            sig_line = Some(v.trim().to_string());
        }
    }
    Some((json_b64?, sig_line?))
}

// ---------------------------------------------------------------------------
//  NTFRY: расшифровка и верификация
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecryptedView {
    pub subject: String,
    pub text: String,
    #[serde(default)]
    pub html: Option<String>,
    #[serde(default)]
    pub attachments: Vec<envelope::Attachment>,
    /// verified | mismatch | unknown | invalid
    pub verified: String,
    /// Источник вердикта: local (адресная книга) | chain | none
    #[serde(default)]
    pub source: String,
    pub sender_address: String,
}

/// Нормализация тела для подписи (SMTP-транспорт меняет переводы строк).
pub fn norm_body(body: &str) -> Vec<u8> {
    body.replace("\r\n", "\n").trim().as_bytes().to_vec()
}

/// Вердикт подлинности: локальный контакт (Local Trust) приоритетнее чейна.
/// Возвращает (verdict, source: local|chain|none).
fn verdict(store: &MailStore, from_email: &str, sender_address: &str) -> (String, String) {
    if let Some(c) = crate::contacts::get(store, from_email) {
        if !c.sth_address.is_empty() {
            return if c.sth_address == sender_address {
                ("verified".into(), "local".into())
            } else {
                ("mismatch".into(), "local".into())
            };
        }
    }
    match lookup_email(store, from_email, false) {
        Ok(Some(e)) if e.address == sender_address && !e.conflict => ("verified".into(), "chain".into()),
        Ok(Some(_)) => ("mismatch".into(), "chain".into()),
        _ => ("unknown".into(), "none".into()),
    }
}

/// Расшифровка NTFRY-письма (кэшируется в store).
pub fn decrypt_message(
    store: &MailStore,
    mnemonic: &str,
    account_id: &str,
    mid: &str,
) -> Result<DecryptedView, CoreError> {
    let tree = ntfry_tree(account_id);
    if let Some(cached) = store.get_json::<DecryptedView>(&tree, mid)? {
        return Ok(cached);
    }
    let msg: StoredMessage = store
        .get_json(&format!("msgs:{account_id}"), mid)?
        .ok_or(CoreError::NotFound("message"))?;
    let body = msg.body.as_deref().unwrap_or("");
    let Some(payload) = armor::extract(body) else {
        // Легаси-копия Sent: тело сохранено плейнтекстом (до фикса) — отдаём как есть
        if msg.folder == "sent" && !body.is_empty() {
            let own = identity::from_secret(mnemonic).map(|i| i.address).unwrap_or_default();
            let view = DecryptedView {
                subject: msg.subject.clone(),
                text: body.to_string(),
                html: None,
                attachments: vec![],
                verified: "verified".into(),
                source: "local".into(),
                sender_address: own,
            };
            store.put_json(&tree, mid, &view)?;
            store.flush();
            return Ok(view);
        }
        return Err(CoreError::Msg("NTFRY-блок не найден в письме".into()));
    };
    let master = keys::master_from_secret(mnemonic);
    let my_sk = keys::derive_x25519(&master);
    let acc = get_account(store, account_id)?;
    let my_hint = email_h32_cached(store, &acc.email).ok().map(|h| hash::rcpt_hint(&h));
    let opened = envelope::open(&payload, &my_sk, my_hint)
        .map_err(|e| CoreError::Msg(format!("Не удалось расшифровать: {e}")))?;
    let sender_address = identity::address_from_pubkey(&opened.sender_pubkey);
    let own_address = identity::from_secret(mnemonic).map(|i| i.address).unwrap_or_default();
    let (verified, source) = if !opened.signature_valid {
        ("invalid".into(), "none".into())
    } else if sender_address == own_address {
        // Собственное письмо (копия в Sent) — доверяем локальному ключу
        ("verified".into(), "local".into())
    } else {
        verdict(store, &msg.from.email, &sender_address)
    };
    let view = DecryptedView {
        subject: opened.content.subject,
        text: opened.content.text,
        html: opened.content.html,
        attachments: opened.content.attachments,
        verified,
        source,
        sender_address,
    };
    store.put_json(&tree, mid, &view)?;
    store.flush();
    Ok(view)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResult {
    /// verified | mismatch | unknown | invalid | none
    pub verified: String,
    /// local | chain | none
    #[serde(default)]
    pub source: String,
    pub sender_address: String,
}

/// Проверка X-Ntfry-Sig обычного (нешифрованного) письма.
pub fn verify_plain(store: &MailStore, account_id: &str, mid: &str) -> Result<VerifyResult, CoreError> {
    let msg: StoredMessage = store
        .get_json(&format!("msgs:{account_id}"), mid)?
        .ok_or(CoreError::NotFound("message"))?;
    let Some(hdr) = msg.ntfry_sig.as_deref() else {
        return Ok(VerifyResult { verified: "none".into(), source: "none".into(), sender_address: String::new() });
    };
    let acc = get_account(store, account_id)?;
    let body = norm_body(msg.body.as_deref().unwrap_or(""));
    // Кандидаты поля to: как в письме / чистый email аккаунта
    let to_email = msg.to.rsplit('<').next().unwrap_or(&msg.to).trim_end_matches('>').trim().to_string();
    let ok = [to_email.as_str(), acc.email.as_str(), msg.to.as_str()]
        .iter()
        .find_map(|to| sig::verify_message(hdr, &msg.from.email, to, &msg.subject, &body));
    match ok {
        Some(v) => {
            let sender_address = identity::address_from_pubkey(&v.pubkey);
            let (verified, source) = verdict(store, &msg.from.email, &sender_address);
            Ok(VerifyResult { verified, source, sender_address })
        }
        None => Ok(VerifyResult { verified: "invalid".into(), source: "none".into(), sender_address: String::new() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_proof_lines() {
        let body = "hello\nOXID-PROOF: eyJhIjoxfQ==\nOXID-SIG: v1; pk=abc; ts=1; sig=zzz\n";
        let (j, s) = extract_proof(body).unwrap();
        assert_eq!(j, "eyJhIjoxfQ==");
        assert!(s.starts_with("v1;"));
        assert!(extract_proof("nothing here").is_none());
    }

    #[test]
    fn norm_body_strips_crlf() {
        assert_eq!(norm_body("a\r\nb\r\n"), b"a\nb".to_vec());
    }
}
