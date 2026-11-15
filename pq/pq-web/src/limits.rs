//! Лимиты на клиента (EndpointId): число одновременных стримов и полоса (token bucket).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::EndpointId;
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Copy)]
pub struct LimitConfig {
    /// Одновременных CONNECT-стримов на клиента.
    pub max_streams_per_client: usize,
    /// Полоса на клиента, КБит/с (сумма обоих направлений). 0 = без лимита.
    pub kbps_per_client: u64,
    /// Глобальный потолок одновременных стримов.
    pub max_streams_total: usize,
}

/// Простой token bucket: `capacity` байт, пополнение `rate` байт/с.
#[derive(Debug)]
pub struct TokenBucket {
    rate: f64,
    capacity: f64,
    state: Mutex<(f64, Instant)>,
}

impl TokenBucket {
    pub fn new(bytes_per_sec: u64) -> Self {
        let rate = bytes_per_sec as f64;
        // burst = полсекунды трафика, но не меньше 64 КБ, чтобы не дробить чтения
        let capacity = (rate * 0.5).max(64.0 * 1024.0);
        TokenBucket { rate, capacity, state: Mutex::new((capacity, Instant::now())) }
    }

    /// Списать `n` байт; вернуть, сколько нужно подождать до разрешения.
    pub fn take(&self, n: usize) -> Duration {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let elapsed = now.duration_since(st.1).as_secs_f64();
        st.0 = (st.0 + elapsed * self.rate).min(self.capacity);
        st.1 = now;
        st.0 -= n as f64;
        if st.0 >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64((-st.0) / self.rate)
        }
    }
}

#[derive(Debug)]
pub struct ClientSlot {
    pub streams: Arc<Semaphore>,
    pub bucket: Option<Arc<TokenBucket>>,
    pub bytes: AtomicU64,
    pub last_seen: Mutex<Instant>,
}

#[derive(Debug)]
pub struct Limiter {
    cfg: LimitConfig,
    pub total: Arc<Semaphore>,
    clients: Mutex<HashMap<EndpointId, Arc<ClientSlot>>>,
}

impl Limiter {
    pub fn new(cfg: LimitConfig) -> Self {
        Limiter {
            cfg,
            total: Arc::new(Semaphore::new(cfg.max_streams_total.max(1))),
            clients: Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> LimitConfig {
        self.cfg
    }

    pub fn slot(&self, id: EndpointId) -> Arc<ClientSlot> {
        let mut map = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        let slot = map
            .entry(id)
            .or_insert_with(|| {
                Arc::new(ClientSlot {
                    streams: Arc::new(Semaphore::new(self.cfg.max_streams_per_client.max(1))),
                    bucket: (self.cfg.kbps_per_client > 0)
                        .then(|| Arc::new(TokenBucket::new(self.cfg.kbps_per_client * 1000 / 8))),
                    bytes: AtomicU64::new(0),
                    last_seen: Mutex::new(Instant::now()),
                })
            })
            .clone();
        *slot.last_seen.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        slot
    }

    /// Убрать давно неактивных клиентов (раз в несколько минут из фонового таска).
    pub fn gc(&self, idle: Duration) {
        let mut map = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        map.retain(|_, s| {
            let seen = *s.last_seen.lock().unwrap_or_else(|e| e.into_inner());
            now.duration_since(seen) < idle || s.streams.available_permits() < self.cfg.max_streams_per_client.max(1)
        });
    }

    pub fn clients_online(&self) -> usize {
        self.clients.lock().map(|m| m.len()).unwrap_or(0)
    }
}

impl ClientSlot {
    pub fn add_bytes(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
    }
}
