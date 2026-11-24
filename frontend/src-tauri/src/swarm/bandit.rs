// ============================================================
//  Author: TechnoL0g / Project: SmartNet Core / Date: 2026-07
//
//  swarm/bandit.rs — Многорукий бандит выбора пира (ε-greedy).
//
//  Lock-free дизайн: карта пиров лежит под `std::sync::RwLock` (write-лок
//  только при регистрации нового пира — редко), а «вес» каждого пира —
//  атомарные примитивы (`AtomicU64`/`AtomicU32`), так что горячий путь
//  (select/update во время загрузки) обходится без взаимных блокировок.
//
//  Метрики пира:
//    S_i — скорость (байт/сек), скользящее среднее EWMA с α = 0.2, чтобы
//          единичный лаг не обнулял репутацию хорошего сидера, но стабильное
//          проседание быстро выводило его из приоритета.
//    E_i — счётчик подряд идущих ошибок (таймаут/сбой). E_i > 0 → пир
//          депротизируется до первого успеха.
// ============================================================

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::syslog;
use super::PeerId;

/// Коэффициент сглаживания скользящего среднего скорости.
const EWMA_ALPHA: f64 = 0.2;
/// Во сколько раз режем оценку скорости пира при сбое (мягкий штраф).
const FAIL_SPEED_DECAY: f64 = 0.5;

/// Атомарный «вес» одного пира. Все поля — lock-free.
struct PeerStat {
    /// EWMA скорости (байт/сек), хранится как биты f64.
    speed_bits: AtomicU64,
    /// Подряд идущие ошибки (E_i).
    consecutive_errors: AtomicU32,
    attempts: AtomicU32,
    successes: AtomicU32,
}

impl PeerStat {
    fn new() -> Self {
        Self {
            speed_bits: AtomicU64::new(0.0f64.to_bits()),
            consecutive_errors: AtomicU32::new(0),
            attempts: AtomicU32::new(0),
            successes: AtomicU32::new(0),
        }
    }
    fn speed(&self) -> f64 {
        f64::from_bits(self.speed_bits.load(Ordering::Relaxed))
    }
    fn set_speed(&self, v: f64) {
        self.speed_bits.store(v.to_bits(), Ordering::Relaxed);
    }
    fn errors(&self) -> u32 {
        self.consecutive_errors.load(Ordering::Relaxed)
    }
}

/// Сериализуемый снимок веса пира (для персистентности в Sled).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerState {
    pub peer: PeerId,
    pub speed: f64,
    pub errors: u32,
    pub attempts: u32,
    pub successes: u32,
}

/// Многорукий бандит: держит веса всех известных пиров и выбирает лучшего
/// под каждый блок по стратегии ε-greedy.
pub struct SmartSwarmBandit {
    peers: RwLock<HashMap<PeerId, Arc<PeerStat>>>,
}

impl Default for SmartSwarmBandit {
    fn default() -> Self {
        Self::new()
    }
}

impl SmartSwarmBandit {
    pub fn new() -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
        }
    }

    /// Зарегистрировать пира (идемпотентно). Единственный write-lock путь.
    pub fn register_peer(&self, peer: PeerId) {
        if let Ok(map) = self.peers.read() {
            if map.contains_key(&peer) {
                return;
            }
        }
        if let Ok(mut map) = self.peers.write() {
            map.entry(peer).or_insert_with(|| Arc::new(PeerStat::new()));
        }
    }

    /// Достать Arc-хэндл веса пира (клонирование Arc под коротким read-lock),
    /// далее операции над ним lock-free.
    fn stat(&self, peer: &str) -> Option<Arc<PeerStat>> {
        self.peers.read().ok().and_then(|m| m.get(peer).cloned())
    }

    /// Выбрать пира под блок из списка кандидатов.
    ///   • с вероятностью ε — случайный пир (разведка ёмкости канала);
    ///   • иначе — эксплуатация: среди пиров с E_i == 0 берём max S_i;
    ///     если все с ошибками — берём с минимумом ошибок, при равенстве — max S_i.
    pub fn select_peer(&self, candidates: &[PeerId], epsilon: f64) -> Option<PeerId> {
        if candidates.is_empty() {
            return None;
        }
        // разведка
        if rand::thread_rng().gen::<f64>() < epsilon {
            let idx = rand::thread_rng().gen_range(0..candidates.len());
            let chosen = candidates[idx].clone();
            syslog::log(
                "swarm",
                format!("🎲 bandit EXPLORE → peer {}", short(&chosen)),
            );
            return Some(chosen);
        }
        // эксплуатация
        let mut best: Option<(PeerId, u32, f64)> = None; // (peer, errors, speed)
        for c in candidates {
            let (errs, speed) = match self.stat(c) {
                Some(s) => (s.errors(), s.speed()),
                None => (0, 0.0),
            };
            best = match best {
                None => Some((c.clone(), errs, speed)),
                Some((_, be, bs)) => {
                    // меньше ошибок предпочтительнее; при равных ошибках — выше скорость
                    let better = errs < be || (errs == be && speed > bs);
                    if better {
                        Some((c.clone(), errs, speed))
                    } else {
                        best
                    }
                }
            };
        }
        if let Some((peer, errs, speed)) = &best {
            syslog::log(
                "swarm",
                format!(
                    "🎯 bandit EXPLOIT → peer {} (S={:.0} B/s, E={})",
                    short(peer),
                    speed,
                    errs
                ),
            );
        }
        best.map(|(p, _, _)| p)
    }

    /// Обновить вес пира по итогу задачи.
    ///   success=true  → пересчёт скорости EWMA, сброс ошибок (награда);
    ///   success=false → инкремент ошибок + мягкий спад скорости (штраф).
    pub fn update_peer_reward(
        &self,
        peer: &str,
        bytes: usize,
        duration: Duration,
        success: bool,
    ) {
        let stat = match self.stat(peer) {
            Some(s) => s,
            None => {
                // новый пир — зарегистрируем и повторим
                self.register_peer(peer.to_string());
                match self.stat(peer) {
                    Some(s) => s,
                    None => return,
                }
            }
        };
        stat.attempts.fetch_add(1, Ordering::Relaxed);

        if success {
            let secs = duration.as_secs_f64().max(0.001);
            let inst = bytes as f64 / secs;
            let old = stat.speed();
            let ewma = if old <= 0.0 {
                inst
            } else {
                EWMA_ALPHA * inst + (1.0 - EWMA_ALPHA) * old
            };
            stat.set_speed(ewma);
            stat.consecutive_errors.store(0, Ordering::Relaxed);
            stat.successes.fetch_add(1, Ordering::Relaxed);
            syslog::log(
                "swarm",
                format!(
                    "✅ peer {} +reward: mgnt {:.0} B/s → EWMA {:.0} B/s",
                    short(peer),
                    inst,
                    ewma
                ),
            );
        } else {
            let errs = stat.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
            let decayed = stat.speed() * FAIL_SPEED_DECAY;
            stat.set_speed(decayed);
            syslog::log(
                "swarm",
                format!(
                    "⛔ peer {} penalty: consecutive errors {}, speed → {:.0} B/s",
                    short(peer),
                    errs,
                    decayed
                ),
            );
        }
    }

    /// Снимок весов всех пиров (для сохранения в Sled).
    pub fn export_state(&self) -> Vec<PeerState> {
        let map = match self.peers.read() {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        map.iter()
            .map(|(peer, s)| PeerState {
                peer: peer.clone(),
                speed: s.speed(),
                errors: s.errors(),
                attempts: s.attempts.load(Ordering::Relaxed),
                successes: s.successes.load(Ordering::Relaxed),
            })
            .collect()
    }

    /// Загрузить веса из снимка (после рестарта клиента).
    pub fn import_state(&self, states: Vec<PeerState>) {
        if let Ok(mut map) = self.peers.write() {
            for st in states {
                let stat = PeerStat::new();
                stat.set_speed(st.speed);
                stat.consecutive_errors.store(st.errors, Ordering::Relaxed);
                stat.attempts.store(st.attempts, Ordering::Relaxed);
                stat.successes.store(st.successes, Ordering::Relaxed);
                map.insert(st.peer, Arc::new(stat));
            }
        }
    }

    /// JSON-снимок для персистентности.
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.export_state()).unwrap_or_else(|_| "[]".to_string())
    }

    /// Восстановление из JSON-снимка (тихо игнорирует битые данные).
    pub fn from_json(&self, json: &str) {
        if let Ok(states) = serde_json::from_str::<Vec<PeerState>>(json) {
            self.import_state(states);
        }
    }

    /// «Лучшая особь»-эквивалент для лога: пир с максимальной скоростью.
    pub fn best_peer(&self) -> Option<(PeerId, f64)> {
        let map = self.peers.read().ok()?;
        map.iter()
            .map(|(p, s)| (p.clone(), s.speed()))
            .fold(None, |acc: Option<(PeerId, f64)>, (p, sp)| match acc {
                Some((_, best)) if best >= sp => acc,
                _ => Some((p, sp)),
            })
    }
}

/// Короткий вид идентификатора пира для логов.
fn short(peer: &str) -> String {
    if peer.len() > 12 {
        format!("{}…{}", &peer[..6], &peer[peer.len() - 4..])
    } else {
        peer.to_string()
    }
}
