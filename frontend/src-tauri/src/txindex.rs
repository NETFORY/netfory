// ============================================================
//  SmartNet // txindex.rs — локальный полный индекс транзакций владельца
//
//  Все tx адресов ЛОКАЛЬНОГО кошелька (main + BIP-44 суб-аккаунты) фоном
//  синхронизируются в sled и дальше держатся в актуальном состоянии стримом
//  `sth/events/1`. История, имена, подписки, ключи — читаются из sled мгновенно
//  и офлайн; в сеть ходим только за дельтой.
//
//    txi:<A>:t:<epoch:010>:<id>  → tx JSON узла (без подписей)
//    txi:<A>:id:<id>             → ""      (dedupe)
//    txi:<A>:cur                 → epoch последней обработанной страницы
//    txi:<A>:done                → "1"     (первый полный проход завершён)
//    txi:<A>:p:<id>              → HistItem (pending: отправлено/в мемпуле, ещё не в блоке)
//    txi:<A>:seq                 → счётчик изменений (растёт на каждый upsert) — потребители
//                                  (граф подписок, история UI) пересчитывают ТОЛЬКО при росте
//
//  ПАМЯТЬ: история адреса — тысячи JSON; `serde_json::Value` на всю историю разом
//  (десятки МБ мелких аллокаций) на Windows-аллокаторе фрагментирует кучу и она не
//  возвращается ОС (наблюдали рост heap ~400 МБ/час при периодических пересканах).
//  Поэтому: обход потоково (`for_each`), по одной записи, и только при изменении `seq`.
// ============================================================

use crate::{sth_node, DB};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const PAGE: u32 = 100;
const PAGE_PAUSE: Duration = Duration::from_millis(150);
/// Сколько tx адреса отдаём в UI из локальной базы.
pub(crate) const HISTORY_CAP: usize = 1000;
/// Pending без подтверждения дольше этого — выпала из мемпула, убираем.
const PENDING_TTL_MS: u64 = 15 * 60 * 1000;
/// Pending без подтверждения дольше этого — сверяем с узлом напрямую (блок ~8 с).
const PENDING_VERIFY_AFTER_MS: u64 = 20_000;
/// Не чаще одной сверки на tx за этот интервал.
const PENDING_VERIFY_EVERY_MS: u64 = 15_000;
/// Pending, которой нет ни в блоке, ни в пуле дольше этого (после переотправок) — потеряна.
const PENDING_DROP_AFTER_MS: u64 = 3 * 60 * 1000;

fn k_tx(addr: &str, epoch: u64, id: &str) -> String {
    format!("txi:{}:t:{:010}:{}", addr, epoch, id)
}
fn k_id(addr: &str, id: &str) -> String {
    format!("txi:{}:id:{}", addr, id)
}
fn k_cur(addr: &str) -> String {
    format!("txi:{}:cur", addr)
}
fn k_done(addr: &str) -> String {
    format!("txi:{}:done", addr)
}
fn k_pending(addr: &str, id: &str) -> String {
    format!("txi:{}:p:{}", addr, id)
}
fn k_seq(addr: &str) -> String {
    format!("txi:{}:seq", addr)
}
fn k_raw(addr: &str, id: &str) -> String {
    format!("txi:{}:raw:{}", addr, id)
}

/// Подписанный JSON принятой tx — для переотправки, если пул узла её потеряет.
pub(crate) fn add_pending_raw(addr: &str, id: &str, data: &Value) {
    if id.is_empty() {
        return;
    }
    let _ = DB.insert(k_raw(addr, id).as_bytes(), data.to_string().as_bytes());
}

/// Снять pending как потерянную: убрать запись и подписанные байты, сбросить in-flight nonce,
/// сообщить UI (`wallet-event` type=tx-dropped) — иначе кошелёк «висит» и новые tx строятся поверх.
fn drop_pending(addr: &str, id: &str, reason: &str) {
    let _ = DB.remove(k_pending(addr, id).as_bytes());
    let _ = DB.remove(k_raw(addr, id).as_bytes());
    let _ = DB.flush();
    sth_node::forget_inflight_nonce(addr);
    crate::syslog::log("txindex", format!("{}: pending {} снята — {}", addr, &id[..id.len().min(10)], reason));
    if let Some(app) = sth_node::app_handle() {
        let _ = app.emit("wallet-event", json!({ "type": "tx-dropped", "address": addr, "id": id, "reason": reason }));
    }
}

/// Счётчик изменений индекса адреса (0 — пусто/не индексирован).
pub(crate) fn seq(addr: &str) -> u64 {
    get_u64(&k_seq(addr)).unwrap_or(0)
}

fn bump_seq(addr: &str) {
    let _ = DB.update_and_fetch(k_seq(addr).as_bytes(), |old| {
        let n = old
            .and_then(|v| std::str::from_utf8(v).ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        Some(n.to_string().into_bytes())
    });
}

/// Потоковый обход tx адреса (новые первыми) без материализации всей истории:
/// JSON парсится по одному, `f` возвращает `false`, чтобы остановиться.
pub(crate) fn for_each(addr: &str, mut f: impl FnMut(&Value) -> bool) {
    for (_, v) in DB.scan_prefix(format!("txi:{}:t:", addr).as_bytes()).rev().filter_map(|kv| kv.ok()) {
        if let Ok(t) = serde_json::from_slice::<Value>(&v) {
            if !f(&t) {
                break;
            }
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Отправленная/мемпульная tx адреса — видна в истории как pending до блока.
pub(crate) fn add_pending(addr: &str, item: sth_node::HistItem) {
    if item.id.is_empty() || DB.contains_key(k_id(addr, &item.id).as_bytes()).unwrap_or(false) {
        return;
    }
    if let Ok(js) = serde_json::to_vec(&item) {
        let _ = DB.insert(k_pending(addr, &item.id).as_bytes(), js);
        let _ = DB.flush();
    }
}

/// Pending адреса, новые первыми; протухшие (>15 мин) удаляются по пути.
pub(crate) fn pending(addr: &str) -> Vec<sth_node::HistItem> {
    let now = now_ms();
    let mut v: Vec<sth_node::HistItem> = Vec::new();
    for (k, val) in DB.scan_prefix(format!("txi:{}:p:", addr).as_bytes()).filter_map(|kv| kv.ok()) {
        match serde_json::from_slice::<sth_node::HistItem>(&val) {
            Ok(it) if now.saturating_sub(it.timestamp) <= PENDING_TTL_MS => v.push(it),
            other => {
                if let Ok(it) = other {
                    let _ = DB.remove(k_raw(addr, &it.id).as_bytes());
                }
                let _ = DB.remove(k);
            }
        }
    }
    v.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    v
}

fn running() -> &'static Mutex<HashSet<String>> {
    static R: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashSet::new()))
}

fn get_u64(key: &str) -> Option<u64> {
    DB.get(key.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| std::str::from_utf8(&v).ok().and_then(|s| s.parse().ok()))
}

/// Первый полный проход по адресу завершён — sled можно считать полной историей.
pub(crate) fn is_complete(addr: &str) -> bool {
    matches!(DB.get(k_done(addr).as_bytes()), Ok(Some(v)) if v.as_ref() == b"1")
}

/// Сколько tx адреса лежит локально.
pub(crate) fn count(addr: &str) -> u64 {
    DB.scan_prefix(format!("txi:{}:id:", addr).as_bytes()).count() as u64
}

/// Убираем подписи/публичные ключи — для истории и фич они не нужны, а места
/// на десятках тысяч tx экономят половину.
fn slim(tx: &Value) -> Value {
    let mut t = tx.clone();
    if let Some(o) = t.as_object_mut() {
        for k in ["signature", "signSignature", "signatures", "senderPublicKey", "confirmations"] {
            o.remove(k);
        }
    }
    t
}

/// Записать tx в индекс адреса (подтверждённую). true — новая запись.
pub(crate) fn upsert(addr: &str, tx: &Value) -> bool {
    let id = tx["id"].as_str().unwrap_or("");
    // в индекс попадают только tx, вошедшие в блок (первое подтверждение)
    let in_block = tx["blockId"].as_str().map_or(false, |b| !b.is_empty());
    if id.is_empty() || !in_block || DB.contains_key(k_id(addr, id).as_bytes()).unwrap_or(false) {
        return false;
    }
    let epoch = tx["timestamp"]["epoch"].as_u64().unwrap_or(0);
    let _ = DB.insert(k_tx(addr, epoch, id).as_bytes(), slim(tx).to_string().as_bytes());
    let _ = DB.insert(k_id(addr, id).as_bytes(), b"");
    // легла в блок — из pending убираем
    let _ = DB.remove(k_pending(addr, id).as_bytes());
    let _ = DB.remove(k_raw(addr, id).as_bytes());
    bump_seq(addr);
    true
}

/// Кэш готовой истории UI по адресу: (seq индекса, HistItem'ы). Пересборка только
/// при изменении seq — `get_wallet_transactions` дёргается на каждый кадр стрима.
/// Ключи — только адреса локального кошелька (main + суб-аккаунты), т.е. ограничен.
static HISTORY_CACHE: OnceLock<Mutex<std::collections::HashMap<String, (u64, std::sync::Arc<Vec<sth_node::HistItem>>)>>> =
    OnceLock::new();

/// История адреса из локального индекса (HistItem, новые первыми, до HISTORY_CAP).
pub(crate) fn history(addr: &str) -> Vec<sth_node::HistItem> {
    let cur = seq(addr);
    let cache = HISTORY_CACHE.get_or_init(|| Mutex::new(Default::default()));
    // Arc берём под замком, Vec клонируем уже без него (replace-guard короткий).
    let hit = cache.lock().unwrap().get(addr).filter(|(s, _)| *s == cur).map(|(_, v)| v.clone());
    if let Some(v) = hit {
        return (*v).clone();
    }
    let mut out: Vec<sth_node::HistItem> = Vec::with_capacity(HISTORY_CAP.min(256));
    for_each(addr, |t| {
        out.push(sth_node::parse_hist_item(t, addr));
        out.len() < HISTORY_CAP
    });
    cache
        .lock()
        .unwrap()
        .insert(addr.to_string(), (cur, std::sync::Arc::new(out.clone())));
    out
}

/// История для UI кошелька: локальные pending (отправки/мемпул) поверх
/// подтверждённых. Подтверждённые — из sled, если индекс полный, иначе с узла.
/// Если tx уже есть среди подтверждённых — pending-копия удаляется (иначе после
/// `refresh` подтверждённая tx снова показывалась как «ждём блок» до TTL).
pub(crate) fn wallet_history(addr: &str) -> Vec<sth_node::HistItem> {
    let confirmed = sth_node::fetch_history_cached(addr);
    let mut out: Vec<sth_node::HistItem> = Vec::new();
    for p in pending(addr) {
        if confirmed.iter().any(|h| h.id == p.id) {
            let _ = DB.remove(k_pending(addr, &p.id).as_bytes());
            let _ = DB.remove(k_raw(addr, &p.id).as_bytes());
        } else {
            out.push(p);
        }
    }
    verify_stale_pending(addr, &out);
    out.extend(confirmed);
    out.truncate(HISTORY_CAP);
    out
}

/// Pending старше PENDING_VERIFY_AFTER_MS без подтверждения в индексе — спросить узел
/// напрямую (`/transactions/<id>`): легла в блок → в индекс и из pending. Фоновый
/// поток, не чаще раза в PENDING_VERIFY_EVERY_MS на tx (стрим мог пропустить кадр).
fn verify_stale_pending(addr: &str, pend: &[sth_node::HistItem]) {
    let now = now_ms();
    let stale: Vec<(String, u64)> = pend
        .iter()
        .filter(|p| now.saturating_sub(p.timestamp) >= PENDING_VERIFY_AFTER_MS)
        .map(|p| (p.id.clone(), now.saturating_sub(p.timestamp)))
        .collect();
    if stale.is_empty() || !crate::netwatch::is_online() {
        return;
    }
    let ids: Vec<(String, u64)> = {
        let mut m = verifying().lock().unwrap();
        stale
            .into_iter()
            .filter(|(id, _)| match m.get(id) {
                Some(t) if t.elapsed().as_millis() < PENDING_VERIFY_EVERY_MS as u128 => false,
                _ => {
                    m.insert(id.clone(), std::time::Instant::now());
                    true
                }
            })
            .collect()
    };
    if ids.is_empty() {
        return;
    }
    let addr = addr.to_string();
    let _ = std::thread::Builder::new().name("txindex-verify".into()).spawn(move || {
        let mut changed = false;
        for (id, age_ms) in ids {
            let (in_block, tx) = match sth_node::node_get(&format!("/transactions/{}", id), Duration::from_secs(12)) {
                Ok(txt) => {
                    let Ok(v) = serde_json::from_str::<Value>(&txt) else { continue };
                    let tx = v["data"].clone();
                    (tx["blockId"].as_str().map_or(false, |b| !b.is_empty()), Some(tx))
                }
                Err(e) if e.contains("status 404") => (false, None),
                Err(_) => continue, // узел недоступен — решать нечего
            };
            if in_block {
                // в блоке: индекс + снять pending (upsert сам убирает ключ)
                if !upsert(&addr, tx.as_ref().unwrap()) {
                    let _ = DB.remove(k_pending(&addr, &id).as_bytes());
                }
                let _ = DB.remove(k_raw(&addr, &id).as_bytes());
                sth_node::bust_history_throttle(&addr);
                crate::syslog::log("txindex", format!("{}: pending {} подтверждена узлом (стрим пропустил кадр)", addr, &id[..id.len().min(10)]));
                changed = true;
                continue;
            }
            if tx.is_some() {
                // узел знает tx (пул), блока ещё нет
                if age_ms >= PENDING_DROP_AFTER_MS {
                    drop_pending(&addr, &id, "висит в пуле без блока дольше 3 мин");
                    changed = true;
                }
                continue;
            }
            // ни в блоке, ни в пуле: пул узла потерял tx (гонка nonce между узлами) — переотправляем
            // подписанные байты; окончательный отказ или истёкшее время — снимаем pending
            match DB.get(k_raw(&addr, &id).as_bytes()).ok().flatten().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
                Some(raw) if age_ms < PENDING_DROP_AFTER_MS => match sth_node::rebroadcast(&raw) {
                    Ok(o) if o.accepted => {
                        crate::syslog::log("txindex", format!("{}: pending {} переотправлена в пул", addr, &id[..id.len().min(10)]));
                    }
                    Ok(o) => {
                        let tx_nonce = raw["nonce"].as_str().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
                        let superseded = sth_node::bad_nonce_expected(&o.reason).map_or(false, |exp| exp > tx_nonce);
                        drop_pending(&addr, &id, &if superseded { "nonce уже использован другой tx".to_string() } else { format!("узел отклонил: {}", o.reason) });
                        changed = true;
                    }
                    Err(_) => {}
                },
                _ => {
                    drop_pending(&addr, &id, "не попала ни в блок, ни в пул");
                    changed = true;
                }
            }
        }
        if changed {
            verifying().lock().unwrap().retain(|_, t| t.elapsed().as_secs() < 600);
            if let Some(app) = sth_node::app_handle() {
                let _ = app.emit("wallet-event", json!({ "type": "history", "address": addr }));
            }
        }
    });
}

fn verifying() -> &'static Mutex<std::collections::HashMap<String, std::time::Instant>> {
    static V: OnceLock<Mutex<std::collections::HashMap<String, std::time::Instant>>> = OnceLock::new();
    V.get_or_init(|| Mutex::new(Default::default()))
}

/// Полный сброс индекса адреса (ручная ресинхронизация).
pub(crate) fn reset(addr: &str) {
    for kv in DB.scan_prefix(format!("txi:{}:", addr).as_bytes()) {
        if let Ok((k, _)) = kv {
            let _ = DB.remove(k);
        }
    }
    if let Some(c) = HISTORY_CACHE.get() {
        c.lock().unwrap().remove(addr);
    }
    let _ = DB.flush();
}

fn emit_progress(app: &AppHandle, addr: &str, indexed: u64, total: u64, done: bool) {
    let _ = app.emit(
        "txindex-progress",
        json!({ "address": addr, "indexed": indexed, "total": total, "done": done }),
    );
}

/// Одна страница узла: все tx адреса (sender ИЛИ recipient, включая
/// multipayment) от `from_epoch` включительно, по возрастанию времени.
fn fetch_page(addr: &str, from_epoch: u64, page: u32) -> Result<(Vec<Value>, u64), String> {
    let path = format!(
        "/transactions?address={}&timestamp.from={}&page={}&limit={}&orderBy=timestamp:asc",
        addr, from_epoch, page, PAGE
    );
    let txt = sth_node::node_get(&path, Duration::from_secs(25))?;
    let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
    let total = v["meta"]["totalCount"].as_u64().unwrap_or(0);
    Ok((v["data"].as_array().cloned().unwrap_or_default(), total))
}

/// Догнать цепь для адреса: от курсора страницами до первой неполной.
/// Курсор двигается после КАЖДОЙ страницы (asc-порядок делает это безопасным).
fn sync_addr(app: &AppHandle, addr: &str) {
    let mut cur = get_u64(&k_cur(addr)).unwrap_or(0);
    let mut page = 1u32;
    let mut indexed = count(addr);
    loop {
        if !crate::netwatch::is_online() {
            return;
        }
        let (txs, total) = match fetch_page(addr, cur, page) {
            Ok(x) => x,
            Err(e) => {
                crate::syslog::log("txindex", format!("{}: узел не ответил ({}) — продолжим позже", addr, e));
                return;
            }
        };
        let mut max_epoch = cur;
        for t in &txs {
            max_epoch = max_epoch.max(t["timestamp"]["epoch"].as_u64().unwrap_or(0));
            if upsert(addr, t) {
                indexed += 1;
            }
        }
        let full = txs.len() as u32 >= PAGE;
        // курсор — на последний epoch; если страница целиком в одной секунде
        // (курсор не сдвинулся), идём на следующую страницу того же запроса
        if max_epoch > cur {
            cur = max_epoch;
            page = 1;
        } else {
            page += 1;
        }
        let _ = DB.insert(k_cur(addr).as_bytes(), cur.to_string().as_bytes());
        if !full {
            let _ = DB.insert(k_done(addr).as_bytes(), b"1");
            let _ = DB.flush();
            emit_progress(app, addr, indexed, total.max(indexed), true);
            return;
        }
        let _ = DB.flush();
        emit_progress(app, addr, indexed, total.max(indexed), false);
        std::thread::sleep(PAGE_PAUSE);
    }
}

/// Запустить (идемпотентно) фоновую синхронизацию для адресов локального
/// кошелька. Повторный вызов для уже работающего адреса — no-op.
pub(crate) fn ensure(app: AppHandle, addrs: Vec<String>) {
    for addr in addrs {
        if addr.is_empty() || !running().lock().unwrap().insert(addr.clone()) {
            continue;
        }
        let app = app.clone();
        let _ = std::thread::Builder::new().name(format!("txindex-{}", &addr[..8.min(addr.len())])).spawn(move || {
            let first = !is_complete(&addr);
            if first {
                crate::syslog::log("txindex", format!("{}: первый полный проход истории", addr));
            }
            sync_addr(&app, &addr);
            if first && is_complete(&addr) {
                crate::syslog::log("txindex", format!("{}: история синхронизирована ({} tx)", addr, count(&addr)));
                // индекс полный — граф подписок и имя теперь считаем локально
                crate::subs::sync_graph(&addr, true);
                let _ = app.emit("social-graph-updated", json!({ "address": addr }));
                let _ = app.emit("wallet-event", json!({ "type": "history", "address": addr }));
            }
            running().lock().unwrap().remove(&addr);
        });
    }
}

#[derive(serde::Serialize)]
pub(crate) struct IndexStatus {
    indexed: u64,
    done: bool,
    syncing: bool,
}

/// Состояние локального индекса адреса (для индикатора в кошельке).
#[tauri::command]
pub(crate) fn txindex_status(address: String) -> IndexStatus {
    IndexStatus {
        indexed: count(&address),
        done: is_complete(&address),
        syncing: running().lock().unwrap().contains(&address),
    }
}

/// Ручная полная ресинхронизация индекса своего адреса (панель безопасности).
pub(crate) fn resync_my_index(app: &AppHandle) {
    for addr in crate::wallet::all_account_addresses() {
        if running().lock().unwrap().contains(&addr) {
            continue;
        }
        reset(&addr);
    }
    ensure(app.clone(), crate::wallet::all_account_addresses());
}
