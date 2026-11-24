// ============================================================
//  SmartNet // reputation.rs — локальный реестр репутации.
//
//  Репутация хранится в Sled по ключу `reputation:<address>` как i64.
//  Правила (только «вперёд», с момента этого обновления):
//    +1000 — разовый бонус за регистрацию имени (grant_registration)
//    +2000 — значение по умолчанию для УЖЕ зарегистрированных без записи
//            (ленивый бэкфилл в effective)
//    +10   — за каждого нового подписчика
//    +1    — за подписку на другого автора
//    +100  — за покупку альбома
//    +250  — за размещение платного dApp
//     -1   — за дизлайк (платный, 1 STH: 90% сжигание + 10% автору)
//  При репутации < 1 пользователь не может комментировать чужие стены.
//
//  Author: TechnoL0g
//  Created: 2026-06-26
// ============================================================

use crate::DB;

pub(crate) const REGISTRATION_BONUS: i64 = 1000;
pub(crate) const EXISTING_DEFAULT: i64 = 2000;

fn key(addr: &str) -> String {
    format!("reputation:{}", addr)
}

fn read_raw(addr: &str) -> Option<i64> {
    DB.get(key(addr).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| std::str::from_utf8(&v).ok().and_then(|s| s.trim().parse::<i64>().ok()))
}

fn write(addr: &str, val: i64) {
    let _ = DB.insert(key(addr).as_bytes(), val.to_string().as_bytes());
}

/// Эффективная репутация: сохранённое значение; иначе 2000 для уже
/// зарегистрированного адреса (ленивый бэкфилл и запись); иначе 0.
pub(crate) fn effective(addr: &str) -> i64 {
    if let Some(v) = read_raw(addr) {
        return v;
    }
    if crate::naming::username_of_address(addr.to_string()).is_some() {
        write(addr, EXISTING_DEFAULT);
        return EXISTING_DEFAULT;
    }
    0
}

/// Начислить дельту (может быть отрицательной). База — текущее эффективное
/// значение (с учётом бэкфилла). Возвращает новое значение.
pub(crate) fn add(addr: &str, delta: i64) -> i64 {
    if addr.is_empty() {
        return 0;
    }
    let next = effective(addr) + delta;
    write(addr, next);
    next
}

/// Разовый бонус за регистрацию имени: ставим 1000 только если записи ещё нет
/// (idempotent — commit_name срабатывает один раз на нового владельца).
pub(crate) fn grant_registration(addr: &str) {
    if !addr.is_empty() && read_raw(addr).is_none() {
        write(addr, REGISTRATION_BONUS);
    }
}

/// Разовое начисление бонуса за достижение (`tag` — уникальный идентификатор
/// награды, например "seed_anchor"). Прибавляется к основной репутации один
/// раз и помечается флагом в Sled (`reputation:once:<addr>:<tag>`), чтобы
/// фоновый тик не начислял его повторно. Возвращает true, если начислено сейчас.
pub(crate) fn grant_once(addr: &str, tag: &str, bonus: i64) -> bool {
    if addr.is_empty() {
        return false;
    }
    let once_key = format!("reputation:once:{}:{}", addr, tag);
    if matches!(DB.get(once_key.as_bytes()), Ok(Some(_))) {
        return false;
    }
    add(addr, bonus);
    let _ = DB.insert(once_key.as_bytes(), b"1");
    let _ = DB.flush();
    true
}

/// Репутация адреса (для отображения под именем в профиле).
#[tauri::command]
pub(crate) async fn get_reputation(address: String) -> i64 {
    tauri::async_runtime::spawn_blocking(move || effective(&address))
        .await
        .unwrap_or(0)
}
