/*
 * Official Repository — списки доверенных разработчиков и одобренных dApps
 * из блокчейна SmartHoldem.
 *
 *   Два специальных адреса сканируются при запуске и раз в 24 часа через
 *   доступный узел из пула кошелька (sth_node::node_get, с failover):
 *     • DEVS_ADDR  — vendorField входящих tx от админа = адрес доверенного
 *       разработчика (его dApps считаются official автоматически);
 *     • DAPPS_ADDR — vendorField = appId одобренного dApp (для приложений
 *       разработчиков вне списка DEVELOPERS).
 *
 *   Результат пишется в Sled (official:dev:* / official:dapp:*). Если
 *   блокчейн недоступен — работает стартовый список DEVELOPERS из .env
 *   (runtime), из окружения сборки (option_env!) или зашитый дефолт.
 *   Фильтрация — на стороне UI (SmartNet только помечает, прямые
 *   sn:// ссылки продолжают работать без ограничений).
 */

use serde::Serialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::DB;

/// Админский адрес, с которого назначаются разработчики и одобряются dApps.
const ADMIN_SENDER: &str = "SeZLuyhhYf2qxs4ArPJ71oEu3x8EsVw51C";
/// Адрес-реестр доверенных разработчиков (vendorField = адрес разработчика).
const DEVS_ADDR: &str = "SdevsH3kMbcCFWnyYVtD2UYnBQQm66MxDn";
/// Адрес-реестр одобренных dApps (vendorField = appId).
const DAPPS_ADDR: &str = "SdAppjWaQf4v2JjuubvtwsKe3yuUCuq8i8";

/// Адреса реестров можно переопределить через .env (OFFICIAL_DEVELOPERS /
/// OFFICIAL_DAPPS); один узел никогда не зашиваем — пул берёт sth_node.
fn devs_addr() -> String {
    std::env::var("OFFICIAL_DEVELOPERS")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEVS_ADDR.to_string())
}
fn dapps_addr() -> String {
    std::env::var("OFFICIAL_DAPPS")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DAPPS_ADDR.to_string())
}

const DEV_PREFIX: &str = "official:dev:";
const DAPP_PREFIX: &str = "official:dapp:";
const TS_KEY: &[u8] = b"official:ts";

/// Стартовый список DEVELOPERS: runtime .env → env сборки → вшитый при
/// сборке frontend/.env (include_str!) → дефолт (админ). Вшитый fallback
/// критичен для собранного приложения: рядом с exe обычно нет .env, и без
/// него адреса разработчиков теряются (ломался обход оплаты ACCEPT).
pub(crate) fn seed_developers() -> Vec<String> {
    let raw = std::env::var("DEVELOPERS")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| option_env!("DEVELOPERS").map(|s| s.to_string()).filter(|s| !s.trim().is_empty()))
        .or_else(|| {
            include_str!("../../.env")
                .lines()
                .find_map(|l| l.trim().strip_prefix("DEVELOPERS="))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| ADMIN_SENDER.to_string());
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Грубая проверка формы SmartHoldem-адреса/appId (base58, префикс S).
fn looks_like_address(s: &str) -> bool {
    s.len() >= 26 && s.len() <= 40 && s.starts_with('S') && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Скан входящих транзакций реестра: vendorField от ADMIN_SENDER → список.
/// Пагинация: идём по страницам (limit=100) до meta.pageCount, чтобы реестр
/// не обрезался на первых 100 tx (cap 50 страниц = 5000 tx). Ошибка любой
/// страницы = ошибка скана: node_get уже делает failover по пулу узлов,
/// а частичный результат опасен — store_set полностью заменяет снапшот.
fn scan_registry(recipient: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut page: u64 = 1;
    loop {
        let path = format!(
            "/transactions?recipientId={}&senderId={}&page={}&limit=100&orderBy=timestamp:desc",
            recipient, ADMIN_SENDER, page
        );
        let body = crate::sth_node::node_get(&path, Duration::from_secs(12))?;
        let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let arr = v.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
        for tx in &arr {
            // senderId уже в запросе, но сверяем поле ответа (защита от кривого узла)
            let sender = tx.get("sender").and_then(|s| s.as_str()).unwrap_or("");
            if sender != ADMIN_SENDER {
                continue;
            }
            let vf = tx
                .get("vendorField")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if looks_like_address(&vf) && !out.contains(&vf) {
                out.push(vf);
            }
        }
        let page_count =
            v.pointer("/meta/pageCount").and_then(|p| p.as_u64()).unwrap_or(page);
        if arr.is_empty() || page >= page_count || page >= 50 {
            break;
        }
        page += 1;
    }
    Ok(out)
}

/// Заменить набор ключей с данным префиксом на свежий список.
fn store_set(prefix: &str, list: &[String]) {
    let stale: Vec<Vec<u8>> = DB
        .scan_prefix(prefix.as_bytes())
        .flatten()
        .map(|(k, _)| k.to_vec())
        .collect();
    for k in stale {
        let _ = DB.remove(k);
    }
    for a in list {
        let _ = DB.insert(format!("{}{}", prefix, a).as_bytes(), &[1u8][..]);
    }
    let _ = DB.flush();
}

fn read_set(prefix: &str) -> Vec<String> {
    DB.scan_prefix(prefix.as_bytes())
        .flatten()
        .map(|(k, _)| String::from_utf8_lossy(&k).trim_start_matches(prefix).to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Актуальный список доверенных разработчиков: .env-сиды + блокчейн (Sled).
pub(crate) fn developers() -> Vec<String> {
    let mut set = seed_developers();
    for a in read_set(DEV_PREFIX) {
        if !set.contains(&a) {
            set.push(a);
        }
    }
    set
}

/// Одобренные dApps (appId) из блокчейна (Sled).
pub(crate) fn official_dapps() -> Vec<String> {
    read_set(DAPP_PREFIX)
}

/// Слить списки из vDapps-анонса доверенного сидера (insert-only, cloud
/// fallback для пиров без доступа к блокчейну). Возвращает (+devs, +dapps).
pub(crate) fn merge_from_cloud(devs: &[String], dapps: &[String]) -> (usize, usize) {
    let known_devs = read_set(DEV_PREFIX);
    let known_dapps = read_set(DAPP_PREFIX);
    let (mut nd, mut na) = (0usize, 0usize);
    for a in devs {
        if looks_like_address(a) && !known_devs.contains(a) {
            let _ = DB.insert(format!("{}{}", DEV_PREFIX, a).as_bytes(), &[1u8][..]);
            nd += 1;
        }
    }
    for a in dapps {
        if looks_like_address(a) && !known_dapps.contains(a) {
            let _ = DB.insert(format!("{}{}", DAPP_PREFIX, a).as_bytes(), &[1u8][..]);
            na += 1;
        }
    }
    if nd + na > 0 {
        let _ = DB.flush();
    }
    (nd, na)
}

fn last_scan_ts() -> u64 {
    DB.get(TS_KEY)
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Полный скан обоих реестров → Sled. Ошибка сети не фатальна: остаётся
/// прошлый снапшот + .env-сиды.
pub(crate) fn refresh_now() -> Result<(usize, usize), String> {
    let devs = scan_registry(&devs_addr())?;
    let dapps = scan_registry(&dapps_addr())?;
    store_set(DEV_PREFIX, &devs);
    store_set(DAPP_PREFIX, &dapps);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = DB.insert(TS_KEY, now.to_string().as_bytes());
    let _ = DB.flush();
    crate::syslog::log(
        "official",
        format!("registry updated: {} developers, {} approved dApps", devs.len(), dapps.len()),
    );
    Ok((devs.len(), dapps.len()))
}

/// Фоновый сканер: старт с задержкой (не конкурируем с разблокировкой и
/// стартовыми запросами кошелька), далее раз в 24ч; при неудаче ретрай 10 мин.
/// Если прошлый скан свежее 23ч (Sled ts) — первый цикл пропускается.
pub(crate) fn spawn_scanner() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(20));
        loop {
            let fresh_enough = {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                now.saturating_sub(last_scan_ts()) < 23 * 60 * 60
            };
            let ok = fresh_enough || (crate::netwatch::is_online() && refresh_now().is_ok());
            let sleep_secs: u64 = if ok { 60 * 60 } else { 10 * 60 };
            std::thread::sleep(Duration::from_secs(sleep_secs));
        }
    });
}

#[derive(Debug, Serialize)]
pub(crate) struct OfficialLists {
    developers: Vec<String>,
    dapps: Vec<String>,
    #[serde(rename = "lastScan")]
    last_scan: u64,
}

fn lists() -> OfficialLists {
    OfficialLists {
        developers: developers(),
        dapps: official_dapps(),
        last_scan: last_scan_ts(),
    }
}

/// Текущие списки official-репозитория (Sled + .env).
/// async + spawn_blocking: Sled I/O никогда не исполняется на main-потоке.
#[tauri::command]
pub(crate) async fn official_lists() -> OfficialLists {
    tauri::async_runtime::spawn_blocking(lists)
        .await
        .unwrap_or_else(|_| lists())
}

/// Принудительный рескан обоих реестров (кнопка «Обновить» в UI).
#[tauri::command]
pub(crate) async fn official_refresh() -> Result<OfficialLists, String> {
    tauri::async_runtime::spawn_blocking(refresh_now)
        .await
        .map_err(|e| e.to_string())??;
    Ok(lists())
}
