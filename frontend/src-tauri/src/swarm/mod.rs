// ============================================================
//  Author: TechnoL0g / Project: SmartNet Core / Date: 2026-07
//
//  swarm/mod.rs — Асинхронный отказоустойчивый swarm-движок загрузки dApp.
//
//  Контур 2 (клиент): скачивание dApp «по-файлово» ПАРАЛЛЕЛЬНО с нескольких
//  пиров одновременно. Выбор пира под каждый блок делает многорукий бандит
//  (`bandit::SmartSwarmBandit`, ε-greedy): с вероятностью (1-ε) берётся пир с
//  максимальной скоростью S_i и нулём ошибок E_i (эксплуатация), с вероятностью
//  ε — случайный пир (разведка ёмкости канала). При таймауте блок мгновенно
//  переназначается другому пиру, а «тупящий» пир получает штраф.
//
//  Сеть спрятана за trait `BlockSource`, поэтому движок тестируется на моке
//  (см. `tests.rs`) без реальной сети, а `source_iroh::IrohBlockSource` даёт
//  боевую реализацию поверх iroh-blobs.
//
//  Правило проекта: только async, никаких блокирующих вызовов в главном потоке,
//  ошибки — через `Result`/`SwarmError`, без `unwrap()` на боевом пути.
// ============================================================
#![allow(dead_code, unused_imports)]

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

pub mod bandit;
pub mod engine;
#[cfg(feature = "p2p")]
pub mod source_iroh;

#[cfg(test)]
mod tests;

pub use bandit::SmartSwarmBandit;
pub use engine::download_dapp_swarm;

/// Хэш блока (в нашей модели «блок» == отдельный файл dApp = отдельный
/// content-addressed blob в iroh-коллекции).
pub type BlockHash = String;
/// Идентификатор пира (iroh EndpointId в текстовом виде).
pub type PeerId = String;

/// Готовый к спавну future одной блокирующей сетевой задачи. Владеет всеми
/// нужными данными (`'static`) и `Send`, чтобы жить внутри `JoinSet`.
pub type BlockFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send>>;

/// Абстракция сетевого источника блоков. Реальная реализация (iroh) и мок
/// (тесты) взаимозаменяемы через `Arc<dyn BlockSource>`.
pub trait BlockSource: Send + Sync + 'static {
    /// Скачать байты одного блока `block` у конкретного пира `peer`.
    /// Возвращает `Err(причина)` при сетевой ошибке/недоступности.
    fn fetch_block(&self, peer: PeerId, block: BlockHash) -> BlockFuture;
}

/// Явные типы ошибок движка (без строковой «каши»).
#[derive(Debug, Clone)]
pub enum SwarmError {
    /// Трекер не дал ни одного пира.
    NoPeers,
    /// Часть блоков не удалось скачать даже после ретраев со всех пиров.
    BlocksFailed(Vec<BlockHash>),
    /// Внутренняя ошибка рантайма (паника таски и т.п.).
    Runtime(String),
}

impl std::fmt::Display for SwarmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwarmError::NoPeers => write!(f, "нет доступных сидеров для загрузки"),
            SwarmError::BlocksFailed(v) => {
                write!(f, "не удалось скачать {} блок(ов) после всех ретраев", v.len())
            }
            SwarmError::Runtime(e) => write!(f, "ошибка рантайма swarm: {}", e),
        }
    }
}

impl std::error::Error for SwarmError {}

/// Параметры движка загрузки.
#[derive(Debug, Clone)]
pub struct SwarmConfig {
    /// Максимум одновременных потоков (concurrency limit).
    pub max_concurrent: usize,
    /// Таймаут на скачивание одного блока — по истечении блок переназначается.
    pub block_timeout: Duration,
    /// ε для стратегии разведки (доля случайных выборов пира).
    pub epsilon: f64,
    /// Сколько раз повторять один и тот же блок (на разных пирах) до отказа.
    pub max_block_retries: u32,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 8,
            block_timeout: Duration::from_secs(20),
            epsilon: 0.15,
            max_block_retries: 8,
        }
    }
}
