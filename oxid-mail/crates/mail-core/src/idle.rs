// IMAP IDLE watcher: фоновый поток на аккаунт, при новом письме ставит dirty-флаг.
// UI опрашивает лёгкий endpoint updates и делает sync только когда есть события.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::account::MailAccount;
use mail_store::MailStore;

static GENERATION: AtomicU64 = AtomicU64::new(0);

struct WatchState {
    running: bool,
    dirty: Arc<AtomicBool>,
}

fn registry() -> &'static Mutex<HashMap<String, WatchState>> {
    static REG: OnceLock<Mutex<HashMap<String, WatchState>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Инвалидация всех watcher'ов (вызывать при открытии/закрытии сессии).
pub fn bump_generation() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// Снять и вернуть dirty-флаг аккаунта.
pub fn take_dirty(account_id: &str) -> bool {
    let reg = registry().lock().unwrap();
    reg.get(account_id)
        .map(|w| w.dirty.swap(false, Ordering::SeqCst))
        .unwrap_or(false)
}

/// Пометить аккаунт как dirty извне (например, из P2P-приёмника).
/// Создаёт запись в реестре, если её ещё нет — чтобы `take_dirty` увидел флаг
/// даже для аккаунтов без активного IDLE-watcher'а (как системный P2P).
pub fn mark_dirty(account_id: &str) {
    let mut reg = registry().lock().unwrap();
    let entry = reg.entry(account_id.to_string()).or_insert_with(|| WatchState {
        running: false,
        dirty: Arc::new(AtomicBool::new(false)),
    });
    entry.dirty.store(true, Ordering::SeqCst);
}

/// Запустить IDLE-поток для аккаунта (идемпотентно).
pub fn start(store: Arc<MailStore>, account_id: String) {
    let dirty = {
        let mut reg = registry().lock().unwrap();
        let entry = reg.entry(account_id.clone()).or_insert_with(|| WatchState {
            running: false,
            dirty: Arc::new(AtomicBool::new(false)),
        });
        if entry.running {
            return;
        }
        entry.running = true;
        entry.dirty.clone()
    };
    let my_gen = GENERATION.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        loop {
            if GENERATION.load(Ordering::SeqCst) != my_gen {
                break;
            }
            let mut acc: MailAccount = match store.get_json("accounts", &account_id) {
                Ok(Some(a)) => a,
                _ => break, // аккаунт удалён
            };
            if crate::service::ensure_token(&store, &mut acc).is_err() {
                std::thread::sleep(Duration::from_secs(120));
                continue;
            }
            match crate::imap_client::idle_wait(&acc) {
                Ok(true) => {
                    dirty.store(true, Ordering::SeqCst);
                }
                Ok(false) => {} // таймаут цикла — перезаходим
                Err(_) => std::thread::sleep(Duration::from_secs(60)),
            }
        }
        let mut reg = registry().lock().unwrap();
        if let Some(w) = reg.get_mut(&account_id) {
            w.running = false;
        }
    });
}
