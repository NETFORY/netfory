// ============================================================
//  Author: TechnoL0g / Project: SmartNet Core / Date: 2026-07
//
//  swarm/engine.rs — Планировщик параллельной загрузки блоков.
//
//  Крутит очередь блоков (pending) и пул задач `tokio::task::JoinSet` с лимитом
//  одновременности. Как только освобождается любой слот — бандит выбирает пира и
//  движок тут же забивает канал новой задачей (нет простоев сети). Каждая задача
//  обёрнута в `tokio::time::timeout`: при истечении блок мгновенно возвращается в
//  очередь на пересдачу другому пиру, а «тупящий» пир штрафуется бандитом.
// ============================================================

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::syslog;
use super::bandit::SmartSwarmBandit;
use super::{BlockHash, BlockSource, PeerId, SwarmConfig, SwarmError};

/// Результат одной асинхронной задачи загрузки блока.
enum TaskOutcome {
    Success {
        block: BlockHash,
        peer: PeerId,
        duration: Duration,
        data: Vec<u8>,
    },
    Failure {
        block: BlockHash,
        peer: PeerId,
        reason: String,
    },
}

/// Скачать dApp-коллекцию «по-файлово» параллельно с нескольких пиров.
///
/// * `source`    — сетевой источник блоков (реальный iroh или мок в тестах);
/// * `bandit`    — общий бандит выбора пира (переживает вызовы, копит веса);
/// * `blocks`    — список хэшей блоков (файлов) коллекции;
/// * `peers`     — доступные пиры от трекера/DHT;
/// * `cfg`       — лимиты одновременности/таймаутов/ε.
///
/// Возвращает карту `блок → байты` либо `SwarmError`.
pub async fn download_dapp_swarm(
    source: Arc<dyn BlockSource>,
    bandit: Arc<SmartSwarmBandit>,
    collection_id: &str,
    blocks: Vec<BlockHash>,
    peers: Vec<PeerId>,
    cfg: SwarmConfig,
) -> Result<HashMap<BlockHash, Vec<u8>>, SwarmError> {
    if peers.is_empty() {
        return Err(SwarmError::NoPeers);
    }
    for p in &peers {
        bandit.register_peer(p.clone());
    }

    let total = blocks.len();
    syslog::log(
        "swarm",
        format!(
            "🚀 swarm download [{}]: {} blocks, {} peers, thread limit {}",
            collection_id, total, peers.len(), cfg.max_concurrent
        ),
    );

    let mut pending: VecDeque<BlockHash> = blocks.into_iter().collect();
    let mut attempts: HashMap<BlockHash, u32> = HashMap::new();
    let mut done: HashMap<BlockHash, Vec<u8>> = HashMap::new();
    let mut failed: Vec<BlockHash> = Vec::new();
    let mut join_set: JoinSet<TaskOutcome> = JoinSet::new();

    loop {
        // 1. Набиваем пул задач до лимита.
        while join_set.len() < cfg.max_concurrent {
            let block = match pending.pop_front() {
                Some(b) => b,
                None => break,
            };
            let peer = match bandit.select_peer(&peers, cfg.epsilon) {
                Some(p) => p,
                None => {
                    pending.push_front(block);
                    break;
                }
            };
            let src = source.clone();
            let to = cfg.block_timeout;
            let b = block.clone();
            let pr = peer.clone();
            join_set.spawn(async move {
                let started = Instant::now();
                match timeout(to, src.fetch_block(pr.clone(), b.clone())).await {
                    Ok(Ok(data)) => TaskOutcome::Success {
                        block: b,
                        peer: pr,
                        duration: started.elapsed(),
                        data,
                    },
                    Ok(Err(reason)) => TaskOutcome::Failure { block: b, peer: pr, reason },
                    Err(_) => TaskOutcome::Failure {
                        block: b,
                        peer: pr,
                        reason: "таймаут блока".to_string(),
                    },
                }
            });
        }

        // Всё скачано и пул пуст — выходим.
        if join_set.is_empty() && pending.is_empty() {
            break;
        }

        // 2. Ждём завершения ЛЮБОЙ задачи (lock-free балансировка).
        let joined = match join_set.join_next().await {
            Some(j) => j,
            None => continue,
        };
        match joined {
            Ok(TaskOutcome::Success { block, peer, duration, data }) => {
                bandit.update_peer_reward(&peer, data.len(), duration, true);
                done.insert(block, data);
                if done.len() % 10 == 0 || done.len() == total {
                    let best = bandit
                        .best_peer()
                        .map(|(_, s)| format!("{:.0} Б/с", s))
                        .unwrap_or_else(|| "—".to_string());
                    syslog::log(
                        "swarm",
                        format!("📈 progress {}/{}, speed leader: {}", done.len(), total, best),
                    );
                }
            }
            Ok(TaskOutcome::Failure { block, peer, reason }) => {
                bandit.update_peer_reward(&peer, 0, Duration::ZERO, false);
                let a = attempts.entry(block.clone()).or_insert(0);
                *a += 1;
                if *a >= cfg.max_block_retries {
                    syslog::log(
                        "swarm",
                        format!("💀 block gave up after {} attempts: {}", *a, reason),
                    );
                    failed.push(block);
                } else {
                    pending.push_back(block);
                }
            }
            Err(join_err) => {
                syslog::log("swarm", format!("⚠️ tokio task panic: {}", join_err));
            }
        }
    }

    if !failed.is_empty() {
        return Err(SwarmError::BlocksFailed(failed));
    }
    syslog::log("swarm", format!("🎉 dApp [{}] fully assembled ({} blocks)", collection_id, total));
    Ok(done)
}
