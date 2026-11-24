// ============================================================
//  SmartNet // wallet.rs
//
//  Native wallet thin-client IPC for the drawer widget. The frontend never
//  touches key material: it asks Rust for the active account, balance,
//  history, exchange rates and swap quotes, and delegates transaction
//  signing + broadcast to the SmartHoldem node client (`sth_node`). The
//  active address is read from the unlocked vault; the session mnemonic is
//  held only in `SESSION_SEED` for the lifetime of the unlock.
//
//  Author: TechnoL0g
//  Project: SmartNet Core
//  Created: 2026-06-18
// ============================================================

use crate::sth_node;
use crate::{ensure_data_dir, DB, SESSION_SEED, VAULT_KEY};
use serde::Serialize;
use std::time::Duration;
use tauri::Emitter;

// ============================================================
//  Wallet thin-client IPC (native contract for the drawer widget)
//
//  The widget delegates account/balance/tx + signing/broadcast to Rust. These
//  are functional stubs: the active address is read from the unlocked vault so
//  the UI is correct natively; balance/history/broadcast return placeholders
//  until a SmartHoldem node/explorer API is wired in. The `incoming-tx-request`
//  event (consumed by the widget) is emitted by the dApp provider bridge.
// ============================================================

#[derive(Serialize)]
pub(crate) struct WalletAccountOut {
    address: String,
    balance: String,
    fiat: String,
    network: String,
}

#[derive(Serialize)]
pub(crate) struct WalletTxOut {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    amount: String,
    counterparty: String,
    timestamp: u64,
    confirmed: bool,
    memo: String,
}

#[derive(Serialize)]
pub(crate) struct ExchangeRateOut {
    #[serde(rename = "sthUsdt")]
    sth_usdt: f64,
}

#[derive(Serialize)]
pub(crate) struct BroadcastOut {
    id: String,
    accepted: bool,
}

/// Deterministic-ish stub tx id (FNV-1a of seed ⊕ wall-clock nanos) until the
/// real node returns a transaction hash.
pub(crate) fn stub_tx_id(seed: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut h: u64 = 0xcbf29ce484222325;
    for b in seed.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}{:016x}", h, nanos)
}

/// Аккаунт-идентичность (main, index 0): баланс для профиля, доменов, публикации
/// dapps — эти платежи всегда идут с main, независимо от выбранного в кошельке.
#[tauri::command]
pub(crate) async fn get_main_smartnet_account() -> WalletAccountOut {
    let address = crate::load_vault_internal().map(|v| v.address).unwrap_or_default();
    account_out(address).await
}

#[tauri::command]
pub(crate) async fn get_active_smartnet_account() -> WalletAccountOut {
    account_out(active_address()).await
}

async fn account_out(address: String) -> WalletAccountOut {
    tauri::async_runtime::spawn_blocking(move || {
        if address.is_empty() {
            return WalletAccountOut {
                address,
                balance: "0.0000".into(),
                fiat: "0.00".into(),
                network: "MAINNET".into(),
            };
        }
        // Кэш последнего успешного баланса: при сбое/таймауте ноды показываем
        // прошлое значение вместо «0.0000» (баланс «пропадал» при флаках сети).
        let bal_key = format!("wallet:balcache:{}", address);
        let balance_raw: u64 = match sth_node::fetch_balance_nonce(&address) {
            Ok((b, _)) => {
                let _ = crate::DB.insert(bal_key.as_bytes(), b.to_string().into_bytes());
                b
            }
            Err(_) => crate::DB
                .get(bal_key.as_bytes())
                .ok()
                .flatten()
                .and_then(|v| String::from_utf8_lossy(&v).parse().ok())
                .unwrap_or(0),
        };
        let balance_sth = balance_raw as f64 / 100_000_000.0;
        let rate = sth_node::fetch_spot_rate();
        WalletAccountOut {
            address,
            balance: sth_node::fmt_balance_sth(balance_raw),
            fiat: format!("{:.2}", balance_sth * rate),
            network: "MAINNET".into(),
        }
    })
    .await
    .unwrap_or(WalletAccountOut {
        address: String::new(),
        balance: "0.0000".into(),
        fiat: "0.00".into(),
        network: "MAINNET".into(),
    })
}

#[derive(Serialize)]
pub(crate) struct DonorTier {
    /// 0 = none, 1 = DON I (bronze), 2 = DON II (silver), 3 = DON III (gold)
    pub tier: u8,
    pub count: u32,
    #[serde(rename = "totalSth")]
    pub total_sth: String,
}

/// Donor medal tier from a user's outgoing `donate:` history:
///   DON I   — any 1+ donation
///   DON II  — total ≥ 10000 STH AND more than 1 donation
///   DON III — more than 10 donations AND total ≥ 100000 STH
#[tauri::command]
pub(crate) async fn donor_tier(address: String) -> DonorTier {
    tauri::async_runtime::spawn_blocking(move || {
        let none = DonorTier { tier: 0, count: 0, total_sth: "0".into() };
        if address.is_empty() {
            return none;
        }
        let items = sth_node::fetch_history_cached(&address);
        let mut count: u32 = 0;
        let mut total: f64 = 0.0;
        for h in items {
            if h.kind == "out" && h.memo.starts_with("donate:") {
                count += 1;
                total += h.amount.parse::<f64>().unwrap_or(0.0);
            }
        }
        let tier = if count > 10 && total >= 100000.0 {
            3
        } else if count > 1 && total >= 10000.0 {
            2
        } else if count >= 1 {
            1
        } else {
            0
        };
        DonorTier { tier, count, total_sth: format!("{:.0}", total) }
    })
    .await
    .unwrap_or(DonorTier { tier: 0, count: 0, total_sth: "0".into() })
}

#[derive(Serialize)]
pub(crate) struct DonationItem {
    pub sender: String,
    pub amount: String,
    pub memo: String,
    pub ts: u64,
}

#[derive(Serialize)]
pub(crate) struct TopDonor {
    pub addr: String,
    pub name: Option<String>,
    #[serde(rename = "totalSth")]
    pub total_sth: String,
}

#[derive(Serialize)]
pub(crate) struct DonationsOut {
    pub items: Vec<DonationItem>,
    #[serde(rename = "totalSth")]
    pub total_sth: String,
    pub donors: u32,
    pub count: u32,
    /// Highest-summing donor over the trailing 30 days (social-proof leaderboard).
    #[serde(rename = "topDonor")]
    pub top_donor: Option<TopDonor>,
}

/// Incoming `donate:<appId>` transfers to the owner address for ONE app.
/// Returns the recent feed plus an all-(scanned)-time total + distinct donor
/// count + the top donor of the last 30 days so the panel can show social proof.
/// The memo is fixed (`donate:<appId>`) so anyone can audit a dApp's tips from chain.
#[tauri::command]
pub(crate) async fn recent_donations(address: String, app_id: String) -> DonationsOut {
    tauri::async_runtime::spawn_blocking(move || {
        let empty = || DonationsOut {
            items: Vec::new(),
            total_sth: "0".into(),
            donors: 0,
            count: 0,
            top_donor: None,
        };
        if address.is_empty() || app_id.is_empty() {
            return empty();
        }
        let memo_tag = format!("donate:{}", app_id);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let month_cutoff_ms = now_ms.saturating_sub(30 * 86400 * 1000);
        let items = sth_node::fetch_history_cached(&address);
        let mut donors = std::collections::HashSet::new();
        let mut total: f64 = 0.0;
        let mut count: u32 = 0;
        let mut feed = Vec::new();
        let mut month_totals: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        for h in items.into_iter() {
            if h.kind != "in" || h.memo != memo_tag {
                continue;
            }
            count += 1;
            donors.insert(h.counterparty.clone());
            let amt = h.amount.parse::<f64>().unwrap_or(0.0);
            total += amt;
            if h.timestamp >= month_cutoff_ms {
                *month_totals.entry(h.counterparty.clone()).or_insert(0.0) += amt;
            }
            if feed.len() < 20 {
                feed.push(DonationItem {
                    sender: h.counterparty,
                    amount: h.amount,
                    memo: h.memo,
                    ts: h.timestamp,
                });
            }
        }
        let top_donor = month_totals
            .into_iter()
            .filter(|(_, v)| *v > 0.0)
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(addr, sum)| TopDonor {
                name: crate::naming::username_of_address(addr.clone()),
                addr,
                total_sth: format!("{:.4}", sum),
            });
        DonationsOut {
            items: feed,
            total_sth: format!("{:.4}", total),
            donors: donors.len() as u32,
            count,
            top_donor,
        }
    })
    .await
    .unwrap_or_else(|_| DonationsOut {
        items: Vec::new(),
        total_sth: "0".into(),
        donors: 0,
        count: 0,
        top_donor: None,
    })
}


/// SHIP-14: балансы токенов активного кошелька (дашборд). Пустой список без vault.
#[tauri::command]
pub(crate) async fn get_wallet_tokens() -> Vec<sth_node::TokenBalance> {
    tauri::async_runtime::spawn_blocking(move || {
        let address = active_address();
        if address.is_empty() {
            return Vec::new();
        }
        sth_node::fetch_wallet_tokens(&address)
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
pub(crate) async fn get_wallet_transactions(force: Option<bool>, address: Option<String>) -> Vec<WalletTxOut> {
    tauri::async_runtime::spawn_blocking(move || {
        // адрес — явно от UI (выбранный аккаунт), чтобы при переключении не
        // подмешать историю предыдущего; иначе активный
        let address = address.filter(|a| !a.is_empty()).unwrap_or_else(active_address);
        if address.is_empty() {
            return Vec::new();
        }
        // force — UI знает о новой tx (событие sth-balance-changed): не ждём 30с троттла.
        if force.unwrap_or(false) {
            sth_node::bust_history_throttle(&address);
        }
        // Локальные pending поверх подтверждённых; подтверждённые — из sled
        // (полный индекс владельца) или кэш узла при первом проходе.
        crate::txindex::wallet_history(&address)
            .into_iter()
            .take(crate::txindex::HISTORY_CAP)
            .map(|h| WalletTxOut {
                id: h.id,
                kind: h.kind,
                amount: h.amount,
                counterparty: h.counterparty,
                timestamp: h.timestamp,
                confirmed: h.confirmed,
                memo: h.memo,
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
pub(crate) async fn get_exchange_rate() -> ExchangeRateOut {
    let rate = tauri::async_runtime::spawn_blocking(sth_node::fetch_spot_rate)
        .await
        .unwrap_or(sth_node::STH_USDT_RATE);
    record_rate_sample(rate);
    ExchangeRateOut { sth_usdt: rate }
}

/// Точка истории курса STH/USDT (пул XBTS через exchange.smartholdem.io).
#[derive(Serialize, serde::Deserialize, Clone, Copy)]
pub(crate) struct RateSample {
    /// unix ms
    pub t: u64,
    pub r: f64,
}

const RATE_HIST_KEY: &[u8] = b"wallet:rate_hist";
const RATE_SAMPLE_EVERY_MS: u64 = 30 * 60 * 1000;
const RATE_KEEP_MS: u64 = 7 * 24 * 3600 * 1000;

fn load_rate_hist() -> Vec<RateSample> {
    crate::DB
        .get(RATE_HIST_KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}

/// Копим спот-курс в sled: не чаще раза в 30 мин, окно 7 дней (≈336 точек).
/// Ни биржа, ни узел истории не отдают — спарклайн строится из этих сэмплов.
fn record_rate_sample(rate: f64) {
    if !(rate > 0.0) {
        return;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let mut hist = load_rate_hist();
    if hist.last().map(|l| now.saturating_sub(l.t) < RATE_SAMPLE_EVERY_MS).unwrap_or(false) {
        return;
    }
    hist.push(RateSample { t: now, r: rate });
    hist.retain(|s| now.saturating_sub(s.t) <= RATE_KEEP_MS);
    if let Ok(b) = serde_json::to_vec(&hist) {
        let _ = crate::DB.insert(RATE_HIST_KEY, b);
    }
}

/// История курса за 7 дней для спарклайна на карточке баланса.
#[tauri::command]
pub(crate) fn get_rate_history() -> Vec<RateSample> {
    load_rate_hist()
}

/// Описание сети SmartHoldem из `GET {api}/api/node/configuration` (+ `/api/node/status`).
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkInfo {
    pub api: String,
    pub name: String,
    pub nethash: String,
    pub token: String,
    pub symbol: String,
    pub explorer: String,
    pub version: u32,
    pub wif: u32,
    pub core_version: String,
    pub height: u64,
}

/// Пробинг API другой сети SmartHoldem (вкладка Networks → «Добавить сеть»).
/// Только чтение конфигурации — платежи в сторонних сетях пока недоступны.
#[tauri::command]
pub(crate) async fn probe_network_api(url: String) -> Result<NetworkInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let base = url.trim().trim_end_matches('/').trim_end_matches("/api").to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err("Ссылка должна начинаться с http:// или https://".to_string());
        }
        let get = |path: &str| -> Result<serde_json::Value, String> {
            let txt = ureq::get(&format!("{}/api{}", base, path))
                .timeout(std::time::Duration::from_secs(8))
                .call()
                .map_err(|e| e.to_string())?
                .into_string()
                .map_err(|e| e.to_string())?;
            serde_json::from_str::<serde_json::Value>(&txt).map_err(|e| e.to_string())
        };
        let cfg = get("/node/configuration")?;
        let d = &cfg["data"];
        let nethash = d["nethash"].as_str().unwrap_or("").to_string();
        if nethash.is_empty() {
            return Err("В ответе нет nethash — это не API SmartHoldem".to_string());
        }
        let height = get("/node/status").ok().and_then(|v| v["data"]["now"].as_u64()).unwrap_or(0);
        Ok(NetworkInfo {
            api: base,
            name: d["name"].as_str().unwrap_or("").to_string(),
            nethash,
            token: d["token"].as_str().unwrap_or("").to_string(),
            symbol: d["symbol"].as_str().unwrap_or("").to_string(),
            explorer: d["explorer"].as_str().unwrap_or("").to_string(),
            version: d["version"].as_u64().unwrap_or(0) as u32,
            wif: d["wif"].as_u64().unwrap_or(0) as u32,
            core_version: cfg["data"]["core"]["version"].as_str().unwrap_or("").to_string(),
            height,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

// ============================================================
//  BIP-44 суб-аккаунты: одна сид-фраза, Account 1 = main (sha256(passphrase)),
//  Account N>1 = m/44'/255'/N-1'/0/0. Идентичность (имена, dapps, мессенджер) —
//  всегда main (`load_vault_internal`); активный аккаунт влияет ТОЛЬКО на
//  кошелёк: баланс, история, получение и отправка.
// ============================================================

const ACTIVE_IDX_KEY: &[u8] = b"wallet:active_index";

fn acct_addr_key(i: u32) -> String {
    format!("wallet:acct_addr:{}", i)
}
fn acct_label_key(i: u32) -> String {
    format!("wallet:acct_label:{}", i)
}

pub(crate) fn active_index() -> u32 {
    DB.get(ACTIVE_IDX_KEY)
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8_lossy(&v).parse().ok())
        .unwrap_or(0)
}

fn account_label(i: u32) -> String {
    DB.get(acct_label_key(i).as_bytes())
        .ok()
        .flatten()
        .map(|v| String::from_utf8_lossy(&v).to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("Account {}", i + 1))
}

/// Адрес аккаунта: index 0 — из vault; N>0 — деривация из сессии, иначе кэш sled
/// (чтобы список аккаунтов был виден и до разблокировки).
fn account_address(i: u32) -> Option<String> {
    if i == 0 {
        return crate::load_vault_internal().map(|v| v.address);
    }
    if let Some(seed) = SESSION_SEED.lock().unwrap().clone() {
        if let Ok(acc) = crate::crypto::derive_account_internal(&seed, i) {
            let _ = DB.insert(acct_addr_key(i).as_bytes(), acc.address.as_bytes());
            return Some(acc.address);
        }
    }
    DB.get(acct_addr_key(i).as_bytes()).ok().flatten().map(|v| String::from_utf8_lossy(&v).to_string())
}

/// Адреса всех аккаунтов сида (для подписки push-стрима).
pub(crate) fn all_account_addresses() -> Vec<String> {
    account_indices().into_iter().filter_map(account_address).collect()
}

/// Адрес активного аккаунта кошелька (пусто — нет vault).
pub(crate) fn active_address() -> String {
    account_address(active_index()).unwrap_or_default()
}

/// Секрет для подписи с активного аккаунта: main → сид-фраза; N>0 → `hex:<privkey>`.
fn active_secret() -> Result<String, String> {
    let seed = SESSION_SEED.lock().unwrap().clone().ok_or_else(|| "Wallet is locked".to_string())?;
    let i = active_index();
    if i == 0 {
        return Ok(seed);
    }
    Ok(format!("hex:{}", crate::crypto::bip44_private_key_hex(&seed, i)?))
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountItem {
    pub index: u32,
    pub address: String,
    pub label: String,
    pub main: bool,
    pub active: bool,
    /// Баланс в STH (строка `fmt_balance_sth`), пусто — не удалось получить.
    pub balance: String,
    pub path: String,
}

fn account_indices() -> Vec<u32> {
    let mut v = crate::load_vault_internal().map(|v| v.accounts).unwrap_or_else(|| vec![0]);
    if !v.contains(&0) {
        v.insert(0, 0);
    }
    v.sort_unstable();
    v.dedup();
    v
}

fn account_item(i: u32, with_balance: bool) -> AccountItem {
    let address = account_address(i).unwrap_or_default();
    let balance = if with_balance && !address.is_empty() {
        let key = format!("wallet:balcache:{}", address);
        match sth_node::fetch_balance_nonce(&address) {
            Ok((b, _)) => {
                let _ = DB.insert(key.as_bytes(), b.to_string().into_bytes());
                sth_node::fmt_balance_sth(b)
            }
            Err(_) => DB
                .get(key.as_bytes())
                .ok()
                .flatten()
                .and_then(|v| String::from_utf8_lossy(&v).parse::<u64>().ok())
                .map(sth_node::fmt_balance_sth)
                .unwrap_or_default(),
        }
    } else {
        String::new()
    };
    AccountItem {
        index: i,
        address,
        label: account_label(i),
        main: i == 0,
        active: i == active_index(),
        balance,
        path: if i == 0 { "sha256(secret)".into() } else { format!("m/44'/255'/{}'/0/0", i) },
    }
}

/// Все аккаунты сида с балансами (параллельно по узлу).
#[tauri::command]
pub(crate) async fn list_accounts() -> Vec<AccountItem> {
    tauri::async_runtime::spawn_blocking(|| {
        let handles: Vec<_> = account_indices().into_iter().map(|i| std::thread::spawn(move || account_item(i, true))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
    .await
    .unwrap_or_default()
}

/// Новый BIP-44 суб-аккаунт (следующий индекс). Требует разблокированной сессии.
#[tauri::command]
pub(crate) async fn add_account(label: Option<String>) -> Result<AccountItem, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let seed = SESSION_SEED.lock().unwrap().clone().ok_or_else(|| "Wallet is locked".to_string())?;
        let mut vault = crate::load_vault_internal().ok_or("no vault")?;
        let next = account_indices().last().copied().unwrap_or(0) + 1;
        let acc = crate::crypto::derive_account_internal(&seed, next)?;
        vault.accounts.push(next);
        crate::save_vault_internal(vault)?;
        let _ = DB.insert(acct_addr_key(next).as_bytes(), acc.address.as_bytes());
        if let Some(l) = label.map(|l| l.trim().to_string()).filter(|l| !l.is_empty()) {
            let _ = DB.insert(acct_label_key(next).as_bytes(), l.into_bytes());
        }
        let _ = DB.flush();
        Ok(account_item(next, false))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn rename_account(index: u32, label: String) -> Result<(), String> {
    let l = label.trim().chars().take(24).collect::<String>();
    if l.is_empty() {
        DB.remove(acct_label_key(index).as_bytes()).map_err(|e| e.to_string())?;
    } else {
        DB.insert(acct_label_key(index).as_bytes(), l.into_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Переключить активный аккаунт кошелька (баланс/история/отправка).
#[tauri::command]
pub(crate) fn set_active_account(index: u32) -> Result<AccountItem, String> {
    if !account_indices().contains(&index) {
        return Err("unknown account".into());
    }
    DB.insert(ACTIVE_IDX_KEY, index.to_string().into_bytes()).map_err(|e| e.to_string())?;
    if let Some(addr) = account_address(index) {
        sth_node::bust_history_throttle(&addr);
    }
    Ok(account_item(index, false))
}

#[derive(Serialize)]
pub(crate) struct SwapQuoteOut {
    #[serde(rename = "usdtAmount")]
    usdt_amount: f64,
    #[serde(rename = "estimatedSthToReceive")]
    estimated_sth_to_receive: f64,
    #[serde(rename = "minSthToReceive")]
    min_sth_to_receive: f64,
    #[serde(rename = "effectivePrice")]
    effective_price: f64,
}

/// Liquidity-pool quote for `usdtAmount` USDT (slippage-aware, AMM-priced).
#[tauri::command]
pub(crate) async fn get_swap_quote(usdt_amount: f64) -> Result<SwapQuoteOut, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (usdt, est, min, eff) = sth_node::fetch_swap_quote(usdt_amount)?;
        Ok(SwapQuoteOut {
            usdt_amount: usdt,
            estimated_sth_to_receive: est,
            min_sth_to_receive: min,
            effective_price: eff,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn send_funds_request(recipient: String, amount: String, memo: String) -> Result<BroadcastOut, String> {
    // подпись ключом АКТИВНОГО аккаунта (main или BIP-44 суб-адрес)
    let secret = active_secret()?;
    tauri::async_runtime::spawn_blocking(move || {
        let out = sth_node::send_transfer(&secret, &recipient, &amount, "", &memo)?;
        Ok(BroadcastOut {
            id: out.id,
            accepted: out.accepted,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn notify_exchange_deposit(amount: String) -> Result<BroadcastOut, String> {
    // Off-chain bridge notification (USDT→STH). Handled by the exchange backend.
    Ok(BroadcastOut {
        id: stub_tx_id(&format!("deposit{}", amount)),
        accepted: true,
    })
}

#[tauri::command]
pub(crate) async fn execute_exchange_sell(amount: String, usdt_address: String) -> Result<BroadcastOut, String> {
    // STH→USDT bridge sell: send `amount` STH to the exchange sell-gate with the
    // destination BEP20 address embedded in the vendorField. Signed on-chain.
    let secret = active_secret()?;
    tauri::async_runtime::spawn_blocking(move || {
        let gate = sth_node::fetch_sell_gate_address()?;
        let out = sth_node::send_transfer(&secret, &gate, &amount, "", &usdt_address)?;
        Ok(BroadcastOut {
            id: out.id,
            accepted: out.accepted,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Per-user BEP20 deposit address for BUYING STH (USDT is sent here).
#[tauri::command]
pub(crate) async fn get_buy_deposit_address() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let address = crate::load_vault_internal().map(|v| v.address).unwrap_or_default();
        if address.is_empty() {
            return Err("Wallet is locked".into());
        }
        sth_node::fetch_buy_deposit_address(&address)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// STH gateway address for SELLING STH.
#[tauri::command]
pub(crate) async fn get_sell_gate_address() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(sth_node::fetch_sell_gate_address)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn sign_and_broadcast_tx(
    app: tauri::AppHandle,
    request_id: String,
    tx_data: serde_json::Value,
) -> Result<BroadcastOut, String> {
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "Wallet is locked".to_string())?;
    let recipient = tx_data["recipientId"]
        .as_str()
        .or_else(|| tx_data["recipient"].as_str())
        .ok_or("missing recipientId")?
        .to_string();
    let amount = tx_data["amount"].as_str().unwrap_or("0").to_string();
    let fee = tx_data["fee"].as_str().unwrap_or("").to_string();
    let vendor = tx_data["vendorField"].as_str().unwrap_or("").to_string();

    let result = tauri::async_runtime::spawn_blocking(move || {
        sth_node::send_transfer(&secret, &recipient, &amount, &fee, &vendor)
    })
    .await
    .map_err(|e| e.to_string())?;

    // Resolve any pending dApp provider request waiting on this id.
    match &result {
        Ok(out) => crate::dapp::emit_dapp_response(
            &app,
            &request_id,
            Some(serde_json::json!({ "id": out.id, "broadcast": { "accept": [out.id] } })),
            None,
        ),
        Err(e) => crate::dapp::emit_dapp_response(&app, &request_id, None, Some(e.clone())),
    }

    let out = result?;
    Ok(BroadcastOut {
        id: out.id,
        accepted: out.accepted,
    })
}

#[tauri::command]
pub(crate) fn reject_tx_request(app: tauri::AppHandle, request_id: String) -> Result<BroadcastOut, String> {
    crate::dapp::emit_dapp_response(
        &app,
        &request_id,
        None,
        Some("User rejected the transaction.".into()),
    );
    Ok(BroadcastOut {
        id: String::new(),
        accepted: false,
    })
}

// ---------------------------------------------------------------------------
//  Соглашение о непроверенных dApps (NETFORY_UNVERIFIED_ACCEPT)
// ---------------------------------------------------------------------------

/// Адрес-реестр ончейн-согласий на показ непроверенных dApps.
const UNVERIFIED_ESCROW: &str = "SappsPqDQXCRvoi1V2Hs9xycQeT1ptJWV1";
/// vendorField цифрового согласия.
const UNVERIFIED_VENDOR: &str = "NETFORY_UNVERIFIED_ACCEPT";

/// Комиссия согласия (STH): runtime .env → env сборки → дефолт 9999.
fn unverified_fee_sth() -> u64 {
    std::env::var("FEE_ALLOW_ALL_DAPPS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .or_else(|| option_env!("FEE_ALLOW_ALL_DAPPS").and_then(|s| s.parse::<u64>().ok()))
        .unwrap_or(9999)
}

fn unverified_key(address: &str) -> String {
    format!("unverified_accept:{}", address)
}

#[derive(Serialize)]
pub(crate) struct UnverifiedAcceptOut {
    accepted: bool,
    #[serde(rename = "feeSth")]
    fee_sth: u64,
}

/// Проверка согласия: сперва локальная Sled-запись, иначе скан исходящих tx
/// на UNVERIFIED_ESCROW (fee = FEE_ALLOW_ALL_DAPPS, amount = 1, vendorField = ACCEPT).
/// Найденное он-чейн согласие кэшируется в Sled навсегда.
#[tauri::command]
pub(crate) async fn check_unverified_accept() -> UnverifiedAcceptOut {
    tauri::async_runtime::spawn_blocking(|| {
        let fee = unverified_fee_sth();
        let address = crate::load_vault_internal().map(|v| v.address).unwrap_or_default();
        if address.is_empty() {
            return UnverifiedAcceptOut { accepted: false, fee_sth: fee };
        }
        // Разработчики (DEVELOPERS: compile-time список + runtime/вшитый .env)
        // — доступ без оплаты соглашения
        if crate::dns_manager::is_developer(&address) {
            return UnverifiedAcceptOut { accepted: true, fee_sth: fee };
        }
        if DB.get(unverified_key(&address).as_bytes()).ok().flatten().is_some() {
            return UnverifiedAcceptOut { accepted: true, fee_sth: fee };
        }
        let accepted =
            sth_node::has_unverified_accept_tx(&address, UNVERIFIED_ESCROW, UNVERIFIED_VENDOR, fee)
                .unwrap_or(false);
        if accepted {
            let _ = DB.insert(unverified_key(&address).as_bytes(), &b"1"[..]);
            let _ = DB.flush();
        }
        UnverifiedAcceptOut { accepted, fee_sth: fee }
    })
    .await
    .unwrap_or(UnverifiedAcceptOut { accepted: false, fee_sth: 9999 })
}

/// Оплата ончейн-соглашения: перевод 1 STH с fee = FEE_ALLOW_ALL_DAPPS на
/// UNVERIFIED_ESCROW с vendorField = NETFORY_UNVERIFIED_ACCEPT. При успехе
/// согласие фиксируется в локальной базе (Sled) — доступ открыт сразу.
#[tauri::command]
pub(crate) async fn pay_unverified_accept() -> Result<BroadcastOut, String> {
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "Wallet is locked".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let address = crate::load_vault_internal().map(|v| v.address).unwrap_or_default();
        if address.is_empty() {
            return Err("Wallet is locked".into());
        }
        let fee = unverified_fee_sth();
        // Достаточно ли баланса на комиссию + перевод 1 STH
        let (balance, _nonce) = sth_node::fetch_balance_nonce(&address)?;
        let need = (fee + 1).saturating_mul(100_000_000);
        if balance < need {
            return Err(format!("INSUFFICIENT_FUNDS:{}", fee + 1));
        }
        let out = sth_node::send_transfer(
            &secret,
            UNVERIFIED_ESCROW,
            "1",
            &fee.to_string(),
            UNVERIFIED_VENDOR,
        )?;
        if !out.accepted {
            return Err("Broadcast rejected by node".into());
        }
        let _ = DB.insert(unverified_key(&address).as_bytes(), &b"1"[..]);
        let _ = DB.flush();
        Ok(BroadcastOut {
            id: out.id,
            accepted: out.accepted,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn delete_vault() -> Result<(), String> {
    DB.remove(VAULT_KEY).map_err(|e| e.to_string())?;
    // A fresh identity must NOT inherit the previous account's conversations.
    crate::chat::wipe_all_chat();
    DB.flush().map_err(|e| e.to_string())?;
    *SESSION_SEED.lock().unwrap() = None;
    Ok(())
}

/// Nuke everything: Sled DB + data/ subdirs + iroh blob store + in-memory session.
#[tauri::command(async)]
pub(crate) fn wipe_all(app: tauri::AppHandle) -> Result<(), String> {
    let _ = DB.clear();
    let _ = DB.flush();
    *SESSION_SEED.lock().unwrap() = None;
    // Reset in-memory chat caches + gossip subscriptions too.
    crate::chat::wipe_all_chat();
    let data = ensure_data_dir(&app);
    for sub in ["dapps", "manifests", "keys"] {
        let p = data.join(sub);
        let _ = std::fs::remove_dir_all(&p);
        let _ = std::fs::create_dir_all(&p);
    }
    // Physically delete the iroh blob store + node identity so no orphaned
    // blobs (old app versions, abandoned downloads) survive the reset.
    crate::p2p::wipe_blob_store_dir(&app);
    // Drop the public profile manifest too.
    let _ = std::fs::remove_file(data.join("profile_manifest.json"));
    Ok(())
}


// ============================================================
//  Phase C — Real-time balance tracker + native notifications
// ============================================================

/// Trigger an OS-level desktop notification (used when the shell is unfocused).
#[tauri::command]
pub(crate) fn show_native_notification(
    app: tauri::AppHandle,
    title: String,
    body: String,
) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
}

/// Background worker: every 12s fetch the latest transactions of the active
/// account and emit `sth-balance-changed` ONCE per new INCOMING txid (amount and
/// real sender from the tx itself). The frontend decides whether to show an
/// in-app toast (focused) or an OS notification (background).
///
/// Раньше событие строилось на дельте баланса между опросами: при чередовании
/// узлов (локальный отстаёт от пула на несколько блоков) баланс «прыгал» и одна
/// и та же старая tx уведомлялась бесконечно. Теперь ключ — txid, увиденные
/// хранятся в sled (`wallet:seen_in:<addr>`), при первом запуске текущая
/// история берётся за базу без уведомлений.
pub(crate) fn spawn_balance_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let mut seen: Option<(String, std::collections::HashSet<String>)> = None;
        loop {
            std::thread::sleep(Duration::from_secs(12));
            // offline: узлы недоступны — пропускаем тик, не копим висящие запросы
            if !crate::netwatch::is_online() {
                continue;
            }
            // SHIP-44: живой push-стрим сам уведомляет о входящих (и пишет id в
            // `wallet:seen_in`) — поллинг не нужен; при обрыве стрима базу
            // перечитаем из sled, чтобы не уведомить повторно.
            if crate::wallet_stream::is_live() {
                seen = None;
                continue;
            }
            let address = crate::load_vault_internal().map(|v| v.address).unwrap_or_default();
            if address.is_empty() {
                seen = None; // locked / no vault — reset baseline
                continue;
            }
            let hist = match sth_node::fetch_history(&address, 10) {
                Ok(h) => h,
                Err(_) => continue,
            };
            let incoming: Vec<&sth_node::HistItem> = hist.iter().filter(|t| t.kind == "in" && !t.id.is_empty()).collect();
            let seen_key = format!("wallet:seen_in:{}", address);
            if !matches!(&seen, Some((a, _)) if *a == address) {
                // Новый аккаунт/старт: база = сохранённое в sled, а если его нет —
                // всё, что видно сейчас (старые входящие не уведомляем).
                let mut s: std::collections::HashSet<String> = crate::DB
                    .get(seen_key.as_bytes())
                    .ok()
                    .flatten()
                    .and_then(|v| serde_json::from_slice(&v).ok())
                    .unwrap_or_default();
                if s.is_empty() {
                    s.extend(incoming.iter().map(|t| t.id.clone()));
                }
                seen = Some((address.clone(), s));
            }
            let set = &mut seen.as_mut().unwrap().1;
            let fresh: Vec<&sth_node::HistItem> = incoming.iter().copied().filter(|t| !set.contains(&t.id)).collect();
            if fresh.is_empty() {
                continue;
            }
            for t in &fresh {
                set.insert(t.id.clone());
            }
            // Ограничиваем память: держим последние ~200 id.
            if set.len() > 400 {
                let keep: std::collections::HashSet<String> = hist.iter().map(|t| t.id.clone()).collect();
                set.retain(|id| keep.contains(id));
            }
            if let Ok(bytes) = serde_json::to_vec(&*set) {
                let _ = crate::DB.insert(seen_key.as_bytes(), bytes);
            }
            // Новая tx в цепочке — сбрасываем троттл истории, чтобы UI после
            // уведомления сразу показал актуальный список операций.
            sth_node::bust_history_throttle(&address);
            for t in fresh {
                let amount_sth: f64 = t.amount.parse().unwrap_or(0.0);
                let _ = app.emit(
                    "sth-balance-changed",
                    serde_json::json!({
                        "amount": (amount_sth * 100_000_000.0).round() as u64,
                        "amountSth": sth_node::fmt_balance_sth((amount_sth * 100_000_000.0).round() as u64),
                        "sender": t.counterparty.clone(),
                        "txid": t.id.clone(),
                    }),
                );
            }
        }
    });
}

// ---- Токены (SHIP-14): API узла + подписанные операции typeGroup 2/3 ----

/// Чтение API токенов через узлы кошелька (iroh/clearnet). Разрешены только пути токенов.
#[tauri::command]
pub(crate) async fn token_api(path: String) -> Result<serde_json::Value, String> {
    const OK: [&str; 5] = ["/tokens", "/wallets/", "/transactions", "/node/configuration", "/sobj"];
    if !OK.iter().any(|p| path.starts_with(p)) {
        return Err("path not allowed".into());
    }
    tauri::async_runtime::spawn_blocking(move || sth_node::api_json(&path)).await.map_err(|e| e.to_string())?
}

/// Логотип токена как текст (SVG) с узла: GET /tokens/{id}/logo. PNG берётся из TokenMeta-транзакции.
#[tauri::command]
pub(crate) async fn token_logo_text(id: String) -> Result<String, String> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("bad id".into());
    }
    tauri::async_runtime::spawn_blocking(move || sth_node::node_get(&format!("/tokens/{id}/logo"), std::time::Duration::from_secs(12)))
        .await
        .map_err(|e| e.to_string())?
}

/// Проверка занятости тикера: POST /sobj/search { type: 5, name }.
#[tauri::command]
pub(crate) async fn token_ticker_taken(name: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let v = sth_node::api_post_json("/sobj/search", &serde_json::json!({ "type": 5, "name": name }))?;
        Ok(v["data"].as_array().is_some_and(|a| !a.is_empty()))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Подписать и отправить операцию токена: kind = ticker | init | transfer | mint | burn | meta.
#[tauri::command]
pub(crate) async fn token_op(kind: String, asset: serde_json::Value, fee: String) -> Result<BroadcastOut, String> {
    let secret = SESSION_SEED.lock().unwrap().clone().ok_or_else(|| "Wallet is locked".to_string())?;
    let fee: u64 = fee.parse().map_err(|_| "bad fee".to_string())?;
    let out = tauri::async_runtime::spawn_blocking(move || sth_node::send_token_op(&secret, &kind, &asset, fee))
        .await
        .map_err(|e| e.to_string())??;
    if !out.accepted {
        return Err(if out.reason.is_empty() { "rejected by node".into() } else { out.reason });
    }
    Ok(BroadcastOut { id: out.id, accepted: true })
}
