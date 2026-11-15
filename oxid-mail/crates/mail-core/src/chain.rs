// SmartHoldem chain-клиент Oxid Mail: пул нод, реестр email↔адрес,
// сборка/подпись/бродкаст v2 transfer (legacy bip-schnorr — идентично sth_node.rs SmartNet).
use mail_crypto::registry::{parse as parse_record, RegistryRecord, BIND_AMOUNT, BIND_FEE, REGISTRY_ADDRESS};
use num_bigint::BigUint;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;

const DEFAULT_NODES: &[&str] = &[
    "node0.smartholdem.io",
    "node1.smartholdem.io",
    "node2.smartholdem.io",
    "node3.smartholdem.io",
    "node4.smartholdem.io",
    "node5.smartholdem.io",
    "node0.sth.cx",
];
const TIMEOUT: Duration = Duration::from_secs(12);
pub const SMARTHOSHI: u64 = 100_000_000;

fn nodes() -> Vec<String> {
    match std::env::var("SMARTHOLDEM_NODES") {
        Ok(s) if !s.trim().is_empty() => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        _ => DEFAULT_NODES.iter().map(|s| s.to_string()).collect(),
    }
}

pub(crate) fn node_get(path: &str) -> Result<String, String> {
    let mut last = String::new();
    for host in nodes() {
        let url = format!("https://{host}/api{path}");
        match ureq::get(&url).timeout(TIMEOUT).call() {
            Ok(r) => return r.into_string().map_err(|e| e.to_string()),
            Err(ureq::Error::Status(code, r)) => {
                // 4xx — детерминированный ответ (404 wallet not found и т.п.), failover не поможет
                let txt = r.into_string().unwrap_or_default();
                return Err(format!("status {code} {txt}"));
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("all nodes unreachable: {last}"))
}

fn node_post(path: &str, body: &str) -> Result<String, String> {
    // Бродкаст — на ОДНУ ноду (txid детерминирован, но не долбим пул)
    let host = nodes().into_iter().next().ok_or("empty node pool")?;
    let url = format!("https://{host}/api{path}");
    match ureq::post(&url).timeout(TIMEOUT).set("Content-Type", "application/json").send_string(body) {
        Ok(r) => r.into_string().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(code, r)) => Err(format!("status {code} {}", r.into_string().unwrap_or_default())),
        Err(e) => Err(e.to_string()),
    }
}

// ---------------------------------------------------------------------------
//  Балансы / нонсы
// ---------------------------------------------------------------------------

/// (баланс в smarthoshi, текущий nonce). Невиданный кошелёк -> (0, 0).
pub fn fetch_balance_nonce(address: &str) -> Result<(u64, u64), String> {
    match node_get(&format!("/wallets/{address}")) {
        Ok(txt) => {
            let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
            let d = &v["data"];
            Ok((
                d["balance"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
                d["nonce"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
            ))
        }
        Err(e) if e.contains("404") => Ok((0, 0)),
        Err(e) => Err(format!("node error: {e}")),
    }
}

// ---------------------------------------------------------------------------
//  Legacy bip-schnorr (is-square R) — байт-в-байт как sth_node.rs SmartNet
// ---------------------------------------------------------------------------

fn p() -> BigUint {
    BigUint::parse_bytes(b"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F", 16).unwrap()
}
fn n() -> BigUint {
    BigUint::parse_bytes(b"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141", 16).unwrap()
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut r = [0u8; 32];
    r.copy_from_slice(&Sha256::digest(data));
    r
}

fn pad32(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let len = bytes.len().min(32);
    out[32 - len..].copy_from_slice(&bytes[bytes.len() - len..]);
    out
}

fn sign_schnorr_legacy(priv32: &[u8; 32], pubkey_comp_hex: &str, msg: &[u8; 32]) -> Result<String, String> {
    let n = n();
    let p = p();
    let secp = Secp256k1::signing_only();
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(priv32);
    buf.extend_from_slice(msg);
    let k0 = BigUint::from_bytes_be(&sha256(&buf)) % &n;
    if k0 == BigUint::from(0u8) {
        return Err("schnorr: k=0".into());
    }
    let k_sk = SecretKey::from_slice(&pad32(&k0.to_bytes_be())).map_err(|e| e.to_string())?;
    let r_point = PublicKey::from_secret_key(&secp, &k_sk).serialize_uncompressed();
    let r_x = &r_point[1..33];
    let y = BigUint::from_bytes_be(&r_point[33..65]);
    let exp = (&p - BigUint::from(1u8)) / BigUint::from(2u8);
    let k = if y.modpow(&exp, &p) == BigUint::from(1u8) { k0 } else { (&n - &k0) % &n };
    let a_comp = hex::decode(pubkey_comp_hex).map_err(|e| e.to_string())?;
    let mut ebuf = Vec::with_capacity(97);
    ebuf.extend_from_slice(r_x);
    ebuf.extend_from_slice(&a_comp);
    ebuf.extend_from_slice(msg);
    let e = BigUint::from_bytes_be(&sha256(&ebuf)) % &n;
    let a = BigUint::from_bytes_be(priv32);
    let s = (&k + (&e * &a)) % &n;
    let mut sig = Vec::with_capacity(64);
    sig.extend_from_slice(r_x);
    sig.extend_from_slice(&pad32(&s.to_bytes_be()));
    Ok(hex::encode(sig))
}

#[cfg(test)]
fn verify_schnorr_legacy(pubkey_comp_hex: &str, msg: &[u8; 32], sig_hex: &str) -> bool {
    let inner = || -> Option<bool> {
        let sig = hex::decode(sig_hex).ok()?;
        if sig.len() != 64 {
            return Some(false);
        }
        let (n, p) = (n(), p());
        let r_x = &sig[..32];
        let s_bytes: [u8; 32] = sig[32..].try_into().ok()?;
        let s_int = BigUint::from_bytes_be(&s_bytes);
        if s_int >= n || s_int == BigUint::from(0u8) || BigUint::from_bytes_be(r_x) >= p {
            return Some(false);
        }
        let a_comp = hex::decode(pubkey_comp_hex).ok()?;
        let mut ebuf = Vec::with_capacity(97);
        ebuf.extend_from_slice(r_x);
        ebuf.extend_from_slice(&a_comp);
        ebuf.extend_from_slice(msg);
        let e = BigUint::from_bytes_be(&sha256(&ebuf)) % &n;
        let secp = Secp256k1::new();
        let a_pk = PublicKey::from_slice(&a_comp).ok()?;
        let s_sk = SecretKey::from_slice(&s_bytes).ok()?;
        let s_g = PublicKey::from_secret_key(&secp, &s_sk);
        let r_point = if e == BigUint::from(0u8) {
            s_g
        } else {
            let neg_e = (&n - &e) % &n;
            let scalar = secp256k1::Scalar::from_be_bytes(pad32(&neg_e.to_bytes_be())).ok()?;
            s_g.combine(&a_pk.mul_tweak(&secp, &scalar).ok()?).ok()?
        };
        let ser = r_point.serialize_uncompressed();
        let ry = BigUint::from_bytes_be(&ser[33..65]);
        let exp = (&p - BigUint::from(1u8)) / BigUint::from(2u8);
        if ry.modpow(&exp, &p) != BigUint::from(1u8) {
            return Some(false);
        }
        Some(&ser[1..33] == r_x)
    };
    inner().unwrap_or(false)
}

// ---------------------------------------------------------------------------
//  v2 transfer (AIP-11)
// ---------------------------------------------------------------------------

fn recipient_buffer(addr: &str) -> Result<Vec<u8>, String> {
    let decoded = bs58::decode(addr).with_check(None).into_vec().map_err(|_| format!("invalid STH address: {addr}"))?;
    if decoded.len() != 21 {
        return Err(format!("invalid STH address length: {addr}"));
    }
    Ok(decoded)
}

fn serialize_transfer(pubkey_comp_hex: &str, nonce: u64, fee: u64, amount: u64, vendor: &str, recipient: &str) -> Result<Vec<u8>, String> {
    let mut b = Vec::with_capacity(128);
    b.push(0xff);
    b.push(0x02);
    b.push(crate::identity::VERSION_BYTE);
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&nonce.to_le_bytes());
    b.extend_from_slice(&hex::decode(pubkey_comp_hex).map_err(|e| e.to_string())?);
    b.extend_from_slice(&fee.to_le_bytes());
    let vf = vendor.as_bytes();
    if vf.is_empty() {
        b.push(0x00);
    } else {
        if vf.len() > 255 {
            return Err("vendorField exceeds 255 bytes".into());
        }
        b.push(vf.len() as u8);
        b.extend_from_slice(vf);
    }
    b.extend_from_slice(&amount.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&recipient_buffer(recipient)?);
    Ok(b)
}

pub struct SignedTx {
    pub id: String,
    pub data: Value,
}

pub fn build_signed_transfer(secret: &str, recipient: &str, amount: u64, fee: u64, vendor: &str, nonce: u64) -> Result<SignedTx, String> {
    let id = crate::identity::from_secret(secret)?;
    let priv32 = sha256(id.mnemonic.as_bytes());
    let pubkey_hex = id.public_key.clone();
    let unsigned = serialize_transfer(&pubkey_hex, nonce, fee, amount, vendor, recipient)?;
    let msg = sha256(&unsigned);
    let signature = sign_schnorr_legacy(&priv32, &pubkey_hex, &msg)?;
    let mut full = unsigned;
    full.extend_from_slice(&hex::decode(&signature).unwrap());
    let txid = hex::encode(sha256(&full));
    let mut data = json!({
        "version": 2,
        "network": crate::identity::VERSION_BYTE,
        "typeGroup": 1,
        "type": 0,
        "nonce": nonce.to_string(),
        "senderPublicKey": pubkey_hex,
        "fee": fee.to_string(),
        "amount": amount.to_string(),
        "expiration": 0,
        "recipientId": recipient,
        "signature": signature,
        "id": txid,
    });
    if !vendor.is_empty() {
        data["vendorField"] = Value::String(vendor.to_string());
    }
    Ok(SignedTx { id: txid, data })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BroadcastOutcome {
    pub id: String,
    pub accepted: bool,
    pub reason: String,
}

pub fn broadcast(data: &Value) -> Result<BroadcastOutcome, String> {
    let id = data["id"].as_str().unwrap_or("").to_string();
    let body = json!({ "transactions": [data] });
    let txt = node_post("/transactions", &body.to_string()).map_err(|e| format!("broadcast rejected: {e}"))?;
    let v: Value = serde_json::from_str(&txt).unwrap_or(Value::Null);
    let accepted = v["data"]["accept"]
        .as_array()
        .map(|a| a.iter().any(|x| x.as_str() == Some(&id)))
        .unwrap_or(false);
    let mut reason = String::new();
    if !accepted {
        if let Some(errs) = v["errors"].get(&id).and_then(|e| e.as_array()) {
            reason = errs
                .iter()
                .map(|e| {
                    let t = e["type"].as_str().unwrap_or("");
                    let m = e["message"].as_str().unwrap_or("");
                    if t.is_empty() { m.to_string() } else { format!("{t}: {m}") }
                })
                .collect::<Vec<_>>()
                .join("; ");
        }
        if reason.is_empty() {
            reason = v["errors"].to_string();
        }
    }
    Ok(BroadcastOutcome { id, accepted, reason })
}

/// One-shot привязка: проверка баланса (>= amount+fee), nonce, сборка tx с
/// vendorField записи реестра, бродкаст. Возвращает txid.
pub fn send_registry_tx(secret: &str, vendor: &str) -> Result<BroadcastOutcome, String> {
    let id = crate::identity::from_secret(secret)?;
    let (balance, chain_nonce) = fetch_balance_nonce(&id.address)?;
    let need = BIND_AMOUNT + BIND_FEE;
    if balance < need {
        return Err(format!(
            "Недостаточно STH: нужно {:.1} STH (0.5 + 0.5 комиссия), на {} только {:.4}",
            need as f64 / SMARTHOSHI as f64,
            id.address,
            balance as f64 / SMARTHOSHI as f64
        ));
    }
    let tx = build_signed_transfer(secret, REGISTRY_ADDRESS, BIND_AMOUNT, BIND_FEE, vendor, chain_nonce + 1)?;
    broadcast(&tx.data)
}

/// Подтверждена ли транзакция (confirmations > 0). Ok(None) — ещё не видна.
pub fn tx_confirmed(txid: &str) -> Result<Option<bool>, String> {
    match node_get(&format!("/transactions/{txid}")) {
        Ok(txt) => {
            let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
            Ok(Some(v["data"]["confirmations"].as_u64().unwrap_or(0) > 0))
        }
        Err(e) if e.contains("404") => Ok(None),
        Err(e) => Err(format!("node error: {e}")),
    }
}

// ---------------------------------------------------------------------------
//  Реестр email ↔ STH-адрес (клиентский индексер, правила v1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RegEntry {
    pub address: String,
    pub x25519_hex: String,
    pub iroh_node_hex: String,
    pub bound_ts: u64,
    pub updated_ts: u64,
    pub txid: String,
    /// Кто-то другой пытался привязать этот же H16 (⚠ в UI).
    pub conflict: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryIndex {
    /// hex(H16) -> запись владельца (earliest-wins)
    pub entries: HashMap<String, RegEntry>,
    /// totalCount реестрового кошелька на момент последнего скана
    pub seen_count: u64,
    pub synced_at: u64,
}

/// Сырая реестровая транзакция (по возрастанию времени).
pub struct RegTx {
    pub sender: String,
    pub vendor: String,
    pub ts: u64,
    pub txid: String,
}

impl RegistryIndex {
    /// Применить одну tx (ВЫЗЫВАТЬ строго в порядке возрастания timestamp).
    pub fn apply(&mut self, tx: &RegTx) {
        let Some(rec) = parse_record(&tx.vendor) else { return };
        match rec {
            RegistryRecord::Bind(b) => {
                let key = hex::encode(b.h16);
                match self.entries.get_mut(&key) {
                    None => {
                        self.entries.insert(
                            key,
                            RegEntry {
                                address: tx.sender.clone(),
                                x25519_hex: hex::encode(b.x25519),
                                iroh_node_hex: hex::encode(b.iroh_node),
                                bound_ts: tx.ts,
                                updated_ts: tx.ts,
                                txid: tx.txid.clone(),
                                conflict: false,
                            },
                        );
                    }
                    Some(e) if e.address == tx.sender => {
                        // Ротация ключей владельцем
                        e.x25519_hex = hex::encode(b.x25519);
                        e.iroh_node_hex = hex::encode(b.iroh_node);
                        e.updated_ts = tx.ts;
                        e.txid = tx.txid.clone();
                    }
                    Some(e) => e.conflict = true, // чужая поздняя привязка — игнор + флаг
                }
            }
            RegistryRecord::Revoke(h16) => {
                let key = hex::encode(h16);
                if self.entries.get(&key).map(|e| e.address == tx.sender).unwrap_or(false) {
                    self.entries.remove(&key);
                }
            }
        }
    }

    pub fn lookup(&self, h16: &[u8; 16]) -> Option<&RegEntry> {
        self.entries.get(&hex::encode(h16))
    }
}

/// Одна страница входящих tx реестрового кошелька (по возрастанию времени).
/// Возвращает (реестровые tx, сырой размер страницы до фильтра).
fn fetch_registry_page(page: u32) -> Result<(Vec<RegTx>, usize), String> {
    let path = format!(
        "/wallets/{REGISTRY_ADDRESS}/transactions?page={page}&limit=100&orderBy=timestamp:asc"
    );
    match node_get(&path) {
        Ok(txt) => {
            let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
            let empty = vec![];
            let arr = v["data"].as_array().unwrap_or(&empty);
            let raw_len = arr.len();
            let out = arr
                .iter()
                .filter(|t| t["recipient"].as_str() == Some(REGISTRY_ADDRESS))
                .map(|t| RegTx {
                    sender: t["sender"].as_str().unwrap_or("").to_string(),
                    vendor: t["vendorField"].as_str().unwrap_or("").to_string(),
                    ts: t["timestamp"]["unix"].as_u64().unwrap_or(0),
                    txid: t["id"].as_str().unwrap_or("").to_string(),
                })
                .collect();
            Ok((out, raw_len))
        }
        Err(e) if e.contains("404") => Ok((vec![], 0)), // реестр ещё пуст
        Err(e) => Err(format!("node error: {e}")),
    }
}

/// Полная синхронизация реестра с цепочкой. meta.totalCount у нод — ОЦЕНКА
/// (totalCountIsEstimate: true), доверять ей нельзя: качаем страницы до конца
/// и полностью ребилдим индекс (реестр компактен — 1 запрос на 100 tx).
pub fn sync_registry(index: &mut RegistryIndex) -> Result<bool, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut fresh = RegistryIndex { synced_at: now, ..Default::default() };
    let mut applied: u64 = 0;
    let mut page = 1u32;
    loop {
        let (txs, raw_len) = fetch_registry_page(page)?;
        for tx in &txs {
            fresh.apply(tx);
        }
        applied += txs.len() as u64;
        if raw_len < 100 {
            break;
        }
        page += 1;
    }
    fresh.seen_count = applied;
    let changed = fresh.entries != index.entries;
    *index = fresh;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_crypto::registry::{encode_bind, encode_revoke, BindRecord};

    #[test]
    fn schnorr_sign_verify_roundtrip() {
        let id = crate::identity::from_secret("chain test phrase").unwrap();
        let priv32 = sha256(id.mnemonic.as_bytes());
        let msg = sha256(b"hello smartholdem");
        let sig = sign_schnorr_legacy(&priv32, &id.public_key, &msg).unwrap();
        assert!(verify_schnorr_legacy(&id.public_key, &msg, &sig));
        let other = sha256(b"tampered");
        assert!(!verify_schnorr_legacy(&id.public_key, &other, &sig));
    }

    #[test]
    fn transfer_serialization_shape() {
        let id = crate::identity::from_secret("chain test phrase").unwrap();
        let vendor = "oxid1:test";
        let unsigned =
            serialize_transfer(&id.public_key, 1, BIND_FEE, BIND_AMOUNT, vendor, REGISTRY_ADDRESS).unwrap();
        // header(58) + vendor(1+len) + asset(33)
        assert_eq!(unsigned.len(), 58 + 1 + vendor.len() + 33);
        let tx = build_signed_transfer("chain test phrase", REGISTRY_ADDRESS, BIND_AMOUNT, BIND_FEE, vendor, 1).unwrap();
        assert_eq!(tx.data["recipientId"].as_str().unwrap(), REGISTRY_ADDRESS);
        assert_eq!(tx.data["amount"].as_str().unwrap(), "50000000");
        assert_eq!(tx.data["vendorField"].as_str().unwrap(), vendor);
        assert_eq!(tx.id.len(), 64);
    }

    fn bind_tx(sender: &str, h16: [u8; 16], x: u8, ts: u64, txid: &str) -> RegTx {
        RegTx {
            sender: sender.into(),
            vendor: encode_bind(&BindRecord { h16, x25519: [x; 32], iroh_node: [x; 32] }),
            ts,
            txid: txid.into(),
        }
    }

    #[test]
    fn registry_rules_earliest_wins_rotation_revoke() {
        let mut idx = RegistryIndex::default();
        let h = [9u8; 16];
        // 1) первый застолбил
        idx.apply(&bind_tx("Salice", h, 1, 100, "tx1"));
        assert_eq!(idx.lookup(&h).unwrap().address, "Salice");
        // 2) чужая поздняя привязка -> conflict, владелец не меняется
        idx.apply(&bind_tx("Seve", h, 2, 200, "tx2"));
        let e = idx.lookup(&h).unwrap();
        assert_eq!(e.address, "Salice");
        assert!(e.conflict);
        assert_eq!(e.x25519_hex, hex::encode([1u8; 32]));
        // 3) ротация владельцем -> ключи обновлены
        idx.apply(&bind_tx("Salice", h, 3, 300, "tx3"));
        let e = idx.lookup(&h).unwrap();
        assert_eq!(e.x25519_hex, hex::encode([3u8; 32]));
        assert_eq!(e.bound_ts, 100);
        assert_eq!(e.updated_ts, 300);
        // 4) revoke чужим -> игнор; revoke владельцем -> запись удалена
        idx.apply(&RegTx { sender: "Seve".into(), vendor: encode_revoke(&h), ts: 400, txid: "tx4".into() });
        assert!(idx.lookup(&h).is_some());
        idx.apply(&RegTx { sender: "Salice".into(), vendor: encode_revoke(&h), ts: 500, txid: "tx5".into() });
        assert!(idx.lookup(&h).is_none());
        // 5) после revoke hash свободен — новый earliest-wins
        idx.apply(&bind_tx("Seve", h, 4, 600, "tx6"));
        assert_eq!(idx.lookup(&h).unwrap().address, "Seve");
        // мусорные vendorField игнорируются
        idx.apply(&RegTx { sender: "X".into(), vendor: "donation!".into(), ts: 700, txid: "tx7".into() });
        assert_eq!(idx.entries.len(), 1);
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    #[ignore] // живой тест: требует сети до нод SmartHoldem
    fn live_sync_registry() {
        let mut idx = RegistryIndex::default();
        let changed = sync_registry(&mut idx).expect("sync");
        println!("changed={changed} total={} entries={}", idx.seen_count, idx.entries.len());
        assert!(idx.synced_at > 0);
    }
}
