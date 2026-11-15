// Naming Service (u://): on-chain имена SmartNet → адрес владельца.
// Регистрация в SmartNet: Type-0 transfer на NAME_SINK с lowercase-именем в
// vendorField; владелец = sender, хронологический first-win
// (1 имя = 1 адрес, 1 адрес = 1 имя). Здесь — только чтение/резолвинг.
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::service::CoreError;
use mail_store::MailStore;

/// Кошелёк-приёмник регистраций имён (идентичен naming.rs SmartNet).
pub const NAME_SINK: &str = "SeeQyvfpghaFLjVfLUTxLvpQq78LMQnAmE";
/// Системные имена — не могут принадлежать пользователям.
const RESERVED_NAMES: &[&str] = &["nameservice", "smartnet", "admin", "root", "system", "support", "group", "groups", "new", "dev"];
const TREE: &str = "naming";
const SCAN_PAGES: u32 = 25;
/// Троттлинг повторной синхронизации индекса при промахе кэша.
const RESYNC_SECS: u64 = 60;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NameIndex {
    /// имя → адрес владельца
    pub names: HashMap<String, String>,
    /// адрес владельца → имя
    pub owners: HashMap<String, String>,
    pub synced_at: u64,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Валидация имени: 2–24 символа, a-z / 0-9 и максимум одна точка
/// (правила идентичны naming.rs SmartNet).
pub fn validate_name(raw: &str) -> Option<String> {
    let name = raw.trim().to_lowercase();
    let len = name.chars().count();
    if !(2..=24).contains(&len) {
        return None;
    }
    if name.chars().filter(|c| *c == '.').count() > 1 {
        return None;
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.') {
        return None;
    }
    Some(name)
}

/// First-win: пара (sender, name) фиксируется, только если оба свободны.
fn commit(idx: &mut NameIndex, sender: &str, name: &str) {
    if RESERVED_NAMES.contains(&name) {
        return;
    }
    if idx.names.contains_key(name) || idx.owners.contains_key(sender) {
        return;
    }
    idx.names.insert(name.to_string(), sender.to_string());
    idx.owners.insert(sender.to_string(), name.to_string());
}

pub fn load(store: &MailStore) -> NameIndex {
    store.get_json::<NameIndex>(TREE, "index").ok().flatten().unwrap_or_default()
}

/// Полный ребилд индекса имён сканом входящих tx sink-кошелька (oldest-first, first-win).
pub fn sync(store: &MailStore) -> Result<NameIndex, CoreError> {
    let mut idx = NameIndex { synced_at: now_secs(), ..Default::default() };
    for page in 1..=SCAN_PAGES {
        let path = format!(
            "/transactions?recipientId={NAME_SINK}&page={page}&limit=100&orderBy=timestamp:asc"
        );
        let txt = match crate::chain::node_get(&path) {
            Ok(t) => t,
            Err(e) if e.contains("404") => break, // sink ещё без транзакций
            Err(e) => return Err(CoreError::Msg(format!("Naming Service недоступен: {e}"))),
        };
        let v: Value = serde_json::from_str(&txt).map_err(|e| CoreError::Msg(e.to_string()))?;
        let empty = vec![];
        let arr = v["data"].as_array().unwrap_or(&empty);
        for t in arr {
            let sender = t["sender"].as_str().unwrap_or("");
            let vendor = t["vendorField"].as_str().unwrap_or("");
            if sender.is_empty() {
                continue;
            }
            if let Some(name) = validate_name(vendor) {
                commit(&mut idx, sender, &name);
            }
        }
        if arr.len() < 100 {
            break;
        }
    }
    store.put_json(TREE, "index", &idx)?;
    store.flush();
    Ok(idx)
}

/// Имя → адрес владельца. При промахе кэша — одна попытка ресинка (с троттлингом).
pub fn resolve(store: &MailStore, raw: &str) -> Result<Option<String>, CoreError> {
    let Some(name) = validate_name(raw) else { return Ok(None) };
    let idx = load(store);
    if let Some(a) = idx.names.get(&name) {
        return Ok(Some(a.clone()));
    }
    if now_secs().saturating_sub(idx.synced_at) < RESYNC_SECS {
        return Ok(None);
    }
    Ok(sync(store)?.names.get(&name).cloned())
}

/// Обратный резолвинг: адрес → имя (для отображения своего alias «имя@sth»).
pub fn name_of(store: &MailStore, addr: &str) -> Result<Option<String>, CoreError> {
    let idx = load(store);
    if let Some(n) = idx.owners.get(addr) {
        return Ok(Some(n.clone()));
    }
    if now_secs().saturating_sub(idx.synced_at) < RESYNC_SECS {
        return Ok(None);
    }
    Ok(sync(store)?.owners.get(addr).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation_rules() {
        assert_eq!(validate_name(" CatSatoshi "), Some("catsatoshi".into()));
        assert_eq!(validate_name("a.b"), Some("a.b".into()));
        assert!(validate_name("a").is_none()); // короткое
        assert!(validate_name("a..b").is_none()); // две точки
        assert!(validate_name("имя").is_none()); // не latin
        assert!(validate_name("Sg2uDbwqm9YPPAa1DMaBmE1c94nuwBhHP9").is_none()); // длиннее 24
    }

    #[test]
    fn first_win_and_reserved() {
        let mut idx = NameIndex::default();
        commit(&mut idx, "Salice", "cat");
        commit(&mut idx, "Seve", "cat"); // имя занято
        commit(&mut idx, "Salice", "dog"); // владелец уже имеет имя
        commit(&mut idx, "Sx", "admin"); // reserved
        assert_eq!(idx.names.get("cat").map(String::as_str), Some("Salice"));
        assert!(idx.names.get("dog").is_none());
        assert!(idx.names.get("admin").is_none());
        assert_eq!(idx.owners.get("Salice").map(String::as_str), Some("cat"));
    }
}
