// ============================================================
//  SmartNet // stealth.rs
//
//  Стелс-режим — ультимативный анти-цензурный слой (обход ТСПУ/DPI).
//
//  Состоит из трёх частей:
//    1. Relay-изоляция сети (применяется в p2p::ensure при старте узла):
//       endpoint строится строго на кастомной relay-map (только наши n1-реле,
//       RelayMode::Custom), без дефолтных n0-реле и без mDNS → нет анонса прямых
//       UDP-адресов; весь трафик к пирам инкапсулируется в QUIC-over-relay
//       (TLS, 443) через приватные реле оператора.
//    2. Application-layer padding: каждый исходящий фрейм чата оборачивается в
//       структуру [MAGIC | payload_len(u32 BE) | payload | случайный хвост], где
//       хвост — 16..=512 случайных байт. Получатель отсекает хвост по payload_len.
//       Ломает статистический анализ по размеру пакета.
//    3. Фантомный трафик (chaff): фоновый таск шлёт по gossip-топикам пустые
//       (payload_len == 0) зашифрованные фреймы с паддингом со случайным
//       интервалом 1.5–4.5 с в периоды простоя — сетевой паттерн становится
//       монотонным (как непрерывный веб-сёрфинг). Живёт в chat::gossip (там есть
//       доступ к отправителям топиков), таймингом/фреймами управляет этот модуль.
//
//  Прикладные слои (2, 3) включаются мгновенно по тумблеру. Полная relay-изоляция
//  (1) применяется при следующем запуске приложения (без риска deadlock и обрыва
//  активных чатов при пересоздании iroh-endpoint на лету).
//
//  Никаких unwrap(): все внешние границы через Result<T, String>.
// ============================================================

use rand::{Rng, RngCore};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// ------------------------------------------------------------
//  Формат стелс-фрейма
// ------------------------------------------------------------
// MAGIC содержит байты 0x00/0x01, которые НЕ встречаются в base64/hex-строке
// шифротекста → нулевой риск ложного распознавания legacy-фрейма как стелс.
const MAGIC: [u8; 4] = [0x00, 0x53, 0x4E, 0x01]; // \0 S N \1
const HEADER_LEN: usize = 8; // MAGIC(4) + payload_len(4)
const PAD_MIN: usize = 16;
const PAD_MAX: usize = 512;

/// Результат разбора входящего пакета топика.
pub enum Incoming {
    /// Реальный полезный payload (уже без паддинга; для legacy-фреймов — как есть).
    Message(Vec<u8>),
    /// Фантомный (dummy) пакет — отбросить, не обрабатывать как сообщение.
    Chaff,
}

/// Обернуть payload в стелс-фрейм со случайным паддингом. Если стелс выключен —
/// вернуть payload как есть (legacy-совместимость: старые пиры читают напрямую).
pub fn frame_outbound(payload: Vec<u8>) -> Vec<u8> {
    if !is_stealth() {
        return payload;
    }
    wrap(&payload)
}

/// Собрать фантомный (chaff) фрейм: пустой payload + случайный паддинг.
pub fn frame_chaff() -> Vec<u8> {
    wrap(&[])
}

fn wrap(payload: &[u8]) -> Vec<u8> {
    let mut rng = rand::thread_rng();
    let pad_len = rng.gen_range(PAD_MIN..=PAD_MAX);
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len() + pad_len);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    let mut pad = vec![0u8; pad_len];
    rng.fill_bytes(&mut pad);
    out.extend_from_slice(&pad);
    out
}

/// Разобрать входящий пакет: снять паддинг стелс-фрейма, распознать chaff, либо
/// принять legacy-пакет (без MAGIC) как обычное сообщение.
pub fn parse_incoming(content: &[u8]) -> Incoming {
    if content.len() >= HEADER_LEN && content[0..4] == MAGIC {
        let len = u32::from_be_bytes([content[4], content[5], content[6], content[7]]) as usize;
        if len == 0 {
            return Incoming::Chaff; // фантомный пакет
        }
        if HEADER_LEN + len <= content.len() {
            return Incoming::Message(content[HEADER_LEN..HEADER_LEN + len].to_vec());
        }
        // Битый фрейм (длина больше пакета) — безопасно отбрасываем.
        return Incoming::Chaff;
    }
    // Legacy-фрейм без обёртки — весь контент это реальный payload.
    Incoming::Message(content.to_vec())
}

// ------------------------------------------------------------
//  Состояние + тайминги
// ------------------------------------------------------------
static STEALTH_ON: AtomicBool = AtomicBool::new(false);
static LAST_ACTIVITY_MS: AtomicU64 = AtomicU64::new(0);

/// Порог простоя (мс): chaff шлётся только если реальной активности не было дольше.
const IDLE_THRESHOLD_MS: u64 = 2000;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Активен ли стелс-режим (прикладная обфускация).
pub fn is_stealth() -> bool {
    STEALTH_ON.load(Ordering::SeqCst)
}

/// Отметить реальную сетевую активность (сбрасывает окно простоя для chaff).
pub fn note_activity() {
    LAST_ACTIVITY_MS.store(now_ms(), Ordering::SeqCst);
}

/// Идёт ли простой (можно слать фантомный трафик).
pub fn idle() -> bool {
    now_ms().saturating_sub(LAST_ACTIVITY_MS.load(Ordering::SeqCst)) > IDLE_THRESHOLD_MS
}

/// Случайный интервал до следующего chaff-пакета (1.5–4.5 с).
pub fn next_chaff_delay_ms() -> u64 {
    rand::thread_rng().gen_range(1500..=4500)
}

// ------------------------------------------------------------
//  Персистентность конфигурации
// ------------------------------------------------------------
/// Прочитать сохранённый флаг стелс-режима (источник истины при старте узла).
pub fn stealth_enabled_persisted() -> bool {
    matches!(crate::DB.get(b"cfg:stealth_enabled"), Ok(Some(v)) if v.as_ref() == b"1")
}

fn set_persisted(on: bool) -> Result<(), String> {
    let val: &[u8] = if on { b"1" } else { b"0" };
    crate::DB
        .insert(b"cfg:stealth_enabled", val)
        .map_err(|e| e.to_string())?;
    let _ = crate::DB.flush();
    Ok(())
}

/// Синхронизировать in-memory флаг с БД при запуске приложения.
pub fn init_from_db() {
    STEALTH_ON.store(stealth_enabled_persisted(), Ordering::SeqCst);
    note_activity();
}

// ------------------------------------------------------------
//  Сетевой менеджер (Tauri State)
// ------------------------------------------------------------
/// Потокобезопасное состояние сети. Критические секции — только быстрые
/// обновления полей (без .await под замком) → нет риска deadlock.
pub struct NetworkManager {
    pub stealth_enabled: bool,
    pub custom_relays: Vec<String>,
}

impl NetworkManager {
    fn from_db() -> Self {
        Self {
            stealth_enabled: stealth_enabled_persisted(),
            custom_relays: crate::p2p::get_relay_config().urls,
        }
    }
}

pub type NetState = Arc<Mutex<NetworkManager>>;

/// Экземпляр менеджера для регистрации в Tauri State (`app.manage(new_state())`).
pub fn new_state() -> NetState {
    Arc::new(Mutex::new(NetworkManager::from_db()))
}

// ------------------------------------------------------------
//  Tauri-команды
// ------------------------------------------------------------

/// Включить/выключить Стелс-режим.
///  - `enable`         : целевое состояние тумблера.
///  - `custom_relays`  : список наших n1-реле (https URL), через которые
///                       изолируется сеть при следующем старте узла.
///
/// Мгновенно: обфускация (padding + фантомный трафик). При перезапуске: полная
/// relay-изоляция iroh-endpoint (только n1, без прямого UDP).
#[tauri::command]
pub async fn toggle_stealth_mode(
    state: tauri::State<'_, NetState>,
    enable: bool,
    custom_relays: Vec<String>,
) -> Result<String, String> {
    let cleaned: Vec<String> = custom_relays
        .into_iter()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .collect();

    if enable {
        if cleaned.is_empty() {
            return Err(
                "Укажите хотя бы один n1-relay сервер в Настройках → Сеть перед включением Стелс-режима.".to_string(),
            );
        }
        for u in &cleaned {
            if !(u.starts_with("https://") || u.starts_with("http://")) {
                return Err(format!(
                    "Некорректный URL реле: {u} (ожидается https://relay-...)"
                ));
            }
        }
    }

    // Персист: флаг стелса + переиспользуем список n1-реле (он же питает сборку
    // endpoint при следующем старте). enable=false отключает n1-реле → чистый n0.
    set_persisted(enable)?;
    crate::p2p::set_relay_config(enable, cleaned.clone())?;
    STEALTH_ON.store(enable, Ordering::SeqCst);
    note_activity();

    // Обновляем Tauri State (короткая критическая секция, без .await под замком).
    {
        let mut mgr = state.lock().map_err(|_| "lock network manager".to_string())?;
        mgr.stealth_enabled = enable;
        mgr.custom_relays = cleaned;
    }

    if enable {
        #[cfg(feature = "p2p")]
        crate::chat::gossip::start_chaff();
        crate::syslog::log(
            "net",
            "STEALTH enabled: padding + phantom traffic active".to_string(),
        );
        Ok("Стелс-режим включён: обфускация трафика (padding + фантомный трафик) активна. Полная relay-изоляция сети (только n1-реле, без прямого UDP) применится после перезапуска приложения.".to_string())
    } else {
        crate::syslog::log("net", "STEALTH disabled".to_string());
        Ok("Стелс-режим выключен. Обфускация остановлена. Стандартная сеть (n0-реле + прямые соединения) восстановится после перезапуска приложения.".to_string())
    }
}

/// Текущее состояние тумблера (для синхронизации UI при загрузке).
#[tauri::command]
pub async fn get_stealth_status(state: tauri::State<'_, NetState>) -> Result<bool, String> {
    let mgr = state.lock().map_err(|_| "lock network manager".to_string())?;
    Ok(mgr.stealth_enabled)
}
