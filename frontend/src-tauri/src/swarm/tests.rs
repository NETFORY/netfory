// ============================================================
//  Author: TechnoL0g / Project: SmartNet Core / Date: 2026-07
//
//  swarm/tests.rs — Симуляционные unit-тесты Контура 2.
//
//  Главный тест: 50 блоков, 3 пира. Один пир (slow) с какого-то момента жёстко
//  проседает (уходит в ступор > таймаута). Проверяем, что движок + бандит
//  перестраивают граф загрузки в пользу живых нод: slow обслуживает мизер,
//  а все 50 блоков всё равно скачиваются.
// ============================================================

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::bandit::SmartSwarmBandit;
use super::engine::download_dapp_swarm;
use super::{BlockFuture, BlockHash, BlockSource, PeerId, SwarmConfig};

/// Мок-источник с управляемой деградацией одного пира.
struct MockSource {
    calls: Arc<AtomicU64>,
    /// после скольких СУММАРНЫХ вызовов slow-пир начинает жёстко тупить
    degrade_after: u64,
    slow_peer: PeerId,
    /// счётчик УСПЕШНЫХ отдач по каждому пиру
    served: Arc<Mutex<HashMap<PeerId, u32>>>,
}

impl BlockSource for MockSource {
    fn fetch_block(&self, peer: PeerId, _block: BlockHash) -> BlockFuture {
        let calls = self.calls.clone();
        let served = self.served.clone();
        let slow = self.slow_peer.clone();
        let degrade_after = self.degrade_after;
        Box::pin(async move {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if peer == slow && n >= degrade_after {
                // жёсткое проседание: зависаем дольше таймаута → движок словит timeout
                tokio::time::sleep(Duration::from_secs(5)).await;
                return Err("slow peer stalled".to_string());
            }
            // нормальная сеть: здоровые пиры быстрее slow даже до деградации
            let latency = if peer == slow { 45 } else { 20 };
            tokio::time::sleep(Duration::from_millis(latency)).await;
            *served.lock().unwrap().entry(peer).or_insert(0) += 1;
            Ok(vec![0u8; 256 * 1024])
        })
    }
}

#[tokio::test]
async fn swarm_reroutes_around_degrading_peer() {
    let peers: Vec<PeerId> = vec![
        "peer_healthy_a".to_string(),
        "peer_healthy_b".to_string(),
        "peer_slow".to_string(),
    ];
    let blocks: Vec<BlockHash> = (0..50).map(|i| format!("block_{i:02}")).collect();

    let served = Arc::new(Mutex::new(HashMap::new()));
    let source: Arc<dyn BlockSource> = Arc::new(MockSource {
        calls: Arc::new(AtomicU64::new(0)),
        degrade_after: 6, // очень рано уходит в ступор
        slow_peer: "peer_slow".to_string(),
        served: served.clone(),
    });

    let bandit = Arc::new(SmartSwarmBandit::new());
    let cfg = SwarmConfig {
        max_concurrent: 6,
        block_timeout: Duration::from_millis(300),
        epsilon: 0.1,
        max_block_retries: 20,
    };

    let out = download_dapp_swarm(source, bandit.clone(), "test-dapp", blocks, peers, cfg)
        .await
        .expect("swarm должен собрать все блоки, обойдя мёртвый пир");

    // 1) все 50 блоков скачаны
    assert_eq!(out.len(), 50, "должны быть скачаны все 50 блоков");

    // 2) граф перестроен в пользу живых нод: slow отдал мизер
    let map = served.lock().unwrap();
    let slow = *map.get("peer_slow").unwrap_or(&0);
    let a = *map.get("peer_healthy_a").unwrap_or(&0);
    let b = *map.get("peer_healthy_b").unwrap_or(&0);
    println!("SERVED → healthy_a={a}, healthy_b={b}, slow={slow}");

    assert!(
        slow <= 8,
        "slow-пир не должен обслуживать много блоков после деградации (было {slow})"
    );
    assert!(
        (a + b) >= 42,
        "основную массу блоков должны отдать живые ноды (было {})",
        a + b
    );
    // сумма успешных отдач == числу блоков
    assert_eq!(a + b + slow, 50);
}

#[tokio::test]
async fn no_peers_errors_out() {
    let source: Arc<dyn BlockSource> = Arc::new(MockSource {
        calls: Arc::new(AtomicU64::new(0)),
        degrade_after: u64::MAX,
        slow_peer: String::new(),
        served: Arc::new(Mutex::new(HashMap::new())),
    });
    let bandit = Arc::new(SmartSwarmBandit::new());
    let res = download_dapp_swarm(
        source,
        bandit,
        "x",
        vec!["b1".to_string()],
        vec![],
        SwarmConfig::default(),
    )
    .await;
    assert!(matches!(res, Err(super::SwarmError::NoPeers)));
}

#[test]
fn bandit_prefers_faster_zero_error_peer() {
    let bandit = SmartSwarmBandit::new();
    let peers = vec!["fast".to_string(), "slow".to_string()];
    bandit.register_peer("fast".to_string());
    bandit.register_peer("slow".to_string());
    // fast быстрее
    bandit.update_peer_reward("fast", 1_000_000, Duration::from_millis(100), true);
    bandit.update_peer_reward("slow", 100_000, Duration::from_millis(100), true);
    // при ε=0 (чистая эксплуатация) должен стабильно выбирать fast
    for _ in 0..20 {
        assert_eq!(bandit.select_peer(&peers, 0.0).as_deref(), Some("fast"));
    }
    // штрафуем fast → у него появляются ошибки, выбор уходит на slow (E=0)
    bandit.update_peer_reward("fast", 0, Duration::ZERO, false);
    assert_eq!(bandit.select_peer(&peers, 0.0).as_deref(), Some("slow"));
}

#[test]
fn bandit_state_roundtrips_json() {
    let bandit = SmartSwarmBandit::new();
    bandit.update_peer_reward("p1", 500_000, Duration::from_millis(100), true);
    let json = bandit.to_json();
    let restored = SmartSwarmBandit::new();
    restored.from_json(&json);
    let a = bandit.best_peer();
    let b = restored.best_peer();
    assert_eq!(a.map(|x| x.0), b.map(|x| x.0));
}
