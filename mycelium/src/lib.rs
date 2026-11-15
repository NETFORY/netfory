//! Mycelium — mushroom-маршрутизация NETFORY поверх Iroh QUIC (NIP-1).
//!
//! Guard → Middle → Exit, X25519/HKDF-SHA256/ChaCha20-Poly1305, ячейки 2048 байт, несколько
//! логических потоков в цепочке, директория релеев по gossip + bootstrap, пул заранее построенных
//! цепочек с изоляцией по адресату, закреплённые Guard-узлы, политика выхода и лимиты релея.
//!
//! ```ignore
//! let my = Mycelium::start(endpoint.clone(), Some(gossip), Config::default()).await?;
//! router.accept(mycelium::ALPN, my.handler());
//! let stream = my.open_stream(target, b"netfory/chat-dm/2").await?;
//! ```
pub mod cell;
pub mod circuit;
pub mod crypto;
pub mod directory;
pub mod relay;
pub mod socks;

pub use circuit::{Circuit, CircuitInfo, MushroomStream, ServiceReq, ALPN, HOP_DEAD_AFTER, KIND_EXIT, KIND_INTRO, KIND_RENDEZVOUS, KIND_SERVICE, SEGMENT_SLOW_MS};
pub use directory::{Directory, ExitPolicy, GuardSet, RelayDescriptor, RelayLimits, Role, ServiceDescriptor};
pub use relay::RelayHandler;

use anyhow::{anyhow, bail, Result};
use iroh::{Endpoint, EndpointId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Автоматических пересборок деградировавшей цепочки подряд; дальше — только вручную.
pub const AUTO_REBUILD_MAX: u32 = 3;
/// Цепочка моложе этого не пересобирается автоматически (RTT ещё не измерены).
const AUTO_REBUILD_MIN_AGE: Duration = Duration::from_secs(20);
/// Здоровая цепочка старше этого сбрасывает счётчик автопересборок.
const RETRIES_RESET_AFTER: Duration = Duration::from_secs(180);
/// Период обслуживания: мёртвые цепочки, добор пула, апгрейд коротких цепочек.
const MAINTENANCE_EVERY: Duration = Duration::from_secs(10);
/// Короткая цепочка (релеев не хватало) пересобирается до полной длины не раньше этого.
const UPGRADE_MIN_AGE: Duration = Duration::from_secs(45);
/// Потолок паузы между пересборками цепочки с мёртвым хопом (пересобирается без лимита попыток).
const DEAD_BACKOFF_MAX: Duration = Duration::from_secs(160);
/// Сколько ждём B у точки встречи после INTRODUCE1.
const RENDEZVOUS_WAIT: Duration = Duration::from_secs(25);
/// Попыток собрать цепочку (каждая — без хопа, на котором упала предыдущая).
const BUILD_ATTEMPTS: usize = 3;
/// Ошибка младше — показывается даже при живых цепочках.
const LAST_ERROR_FRESH: Duration = Duration::from_secs(30);
/// Не начинать новую попытку сборки, если с начала прошло больше (итог ≤ ~30 с + последняя попытка).
const BUILD_BUDGET: Duration = Duration::from_secs(20);
/// Сколько помнить провал сборки к адресату для ожидающих single-flight.
const TARGET_FAIL_TTL: Duration = Duration::from_secs(4);

/// Обработчик входящих запросов скрытого приёма (B): (ALPN, запрос) → ответ (None — отказ).
pub type ServiceHandler = Arc<dyn Fn(Vec<u8>, Vec<u8>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<u8>>> + Send>> + Send + Sync>;

/// Настройки клиента/релея.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Число хопов 1..=3 (меньше 3 — слабее анонимность).
    pub hops: u8,
    /// Поднимать релей (десктоп с публично достижимым узлом).
    pub relay: bool,
    /// Сколько цепочек держать готовыми в пуле.
    pub pool_size: usize,
    pub bootstrap: Vec<EndpointId>,
    pub exit_policy: ExitPolicy,
    pub limits: RelayLimits,
    /// Каталог для guard-набора и кэша.
    pub data_dir: PathBuf,
    /// Фаза 2: скрытый приём — держать intro-цепочки и публиковать описатель (получатель анонимен).
    pub hidden: bool,
    /// Число intro-точек получателя.
    pub intro_points: usize,
    /// Маскирующий трафик: PADDING-ячейки на простаивающих цепочках.
    pub cover_traffic: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hops: 3,
            relay: false,
            pool_size: 2,
            bootstrap: Vec::new(),
            exit_policy: ExitPolicy::default(),
            limits: RelayLimits::default(),
            data_dir: std::env::temp_dir().join("mycelium"),
            hidden: true,
            intro_points: 2,
            cover_traffic: false,
        }
    }
}

/// Снимок состояния для UI.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub enabled: bool,
    pub circuits_ready: usize,
    pub circuits_building: bool,
    pub relays_known: usize,
    pub guards: Vec<String>,
    pub hops: u8,
    pub relay_enabled: bool,
    pub relay_circuits_active: u32,
    pub relay_circuits_total: u64,
    pub relay_cells: u64,
    pub relay_bytes: u64,
    pub exit_streams: u64,
    pub last_error: String,
    /// Сколько секунд назад была последняя ошибка.
    pub last_error_age_secs: u64,
    /// Фаза 2.
    pub hidden: bool,
    pub intro_ready: usize,
    pub services_known: usize,
    pub cover_traffic: bool,
    pub relay_intro_active: u32,
    pub relay_rendezvous_total: u64,
    /// Разных релеев, пригодных для пути (без себя).
    pub relays_usable: usize,
    /// Наименьшая длина живой цепочки (0 — цепочек нет).
    pub hops_effective: u8,
    /// Релеев меньше, чем хопов: цепочки короче настроенной длины (анонимность слабее).
    pub hops_reduced: bool,
}

struct Inner {
    endpoint: Endpoint,
    cfg: Mutex<Config>,
    dir: Directory,
    guards: Mutex<GuardSet>,
    handler: RelayHandler,
    /// Общий пул + изолированные цепочки по адресату (NIP-1 §6.1: Exit не связывает контакты).
    pool: Mutex<Vec<Arc<Circuit>>>,
    per_target: Mutex<HashMap<EndpointId, Arc<Circuit>>>,
    building: AtomicBool,
    enabled: AtomicBool,
    /// Последняя ошибка построения + когда случилась (в статусе гаснет, если цепочки живы).
    last_error: Mutex<(String, Option<std::time::Instant>)>,
    /// single-flight: одна сборка цепочки на адресата (браузер открывает 6–10 соединений разом).
    target_locks: Mutex<HashMap<EndpointId, Arc<tokio::sync::Mutex<()>>>>,
    /// Недавний провал сборки к адресату: ожидавшие single-flight сразу получают ту же ошибку.
    target_fail: Mutex<HashMap<EndpointId, (std::time::Instant, String)>>,
    dir_tx: Mutex<Option<iroh_gossip::api::GossipSender>>,
    /// Gossip-топик директории поднимается лениво — только когда включён клиент или релей
    /// (выключенный Mycelium не открывает ни одного соединения). Пиры до старта копятся в `pending_peers`.
    gossip: Option<iroh_gossip::net::Gossip>,
    dir_started: AtomicBool,
    pending_peers: Mutex<Vec<EndpointId>>,
    /// id цепочек, для которых прямо сейчас строится замена.
    rebuilding: Mutex<HashSet<u32>>,
    /// Фаза 2: intro-цепочки получателя, сервисные (rendezvous со стороны B), обработчик запросов.
    intro: Mutex<Vec<Arc<Circuit>>>,
    service: Mutex<Vec<Arc<Circuit>>>,
    service_handler: Mutex<Option<ServiceHandler>>,
    intro_busy: AtomicBool,
    /// Ключ личности для скрытого приёма (владелец описателя, rendezvous). По умолчанию — ключ
    /// транспорта; приложение с отдельным endpoint'ом Mycelium подставляет ключ основного узла.
    identity: Mutex<iroh::SecretKey>,
}

#[derive(Clone)]
pub struct Mycelium(Arc<Inner>);

impl Mycelium {
    /// Создать экземпляр: релей-обработчик готов сразу (регистрируйте в Router), цепочки
    /// строятся после `set_enabled(true)`. `gossip` — для директории релеев.
    /// `extra_peers` — знакомые узлы для входа в gossip-топик директории (кроме bootstrap).
    pub async fn start(endpoint: Endpoint, gossip: Option<iroh_gossip::net::Gossip>, cfg: Config, extra_peers: Vec<EndpointId>) -> Result<Mycelium> {
        let dir = Directory::with_bootstrap(cfg.bootstrap.clone());
        let guards = GuardSet::load(&cfg.data_dir.join("guards.json"));
        let handler = RelayHandler::new(endpoint.clone(), cfg.exit_policy.clone(), cfg.limits.clone(), cfg.relay);
        let relay_on = cfg.relay;
        *handler.0.dir.lock().unwrap() = Some(dir.clone());
        let identity = endpoint.secret_key().clone();
        let me = Mycelium(Arc::new(Inner {
            endpoint,
            cfg: Mutex::new(cfg),
            dir,
            guards: Mutex::new(guards),
            handler,
            pool: Mutex::new(Vec::new()),
            per_target: Mutex::new(HashMap::new()),
            building: AtomicBool::new(false),
            enabled: AtomicBool::new(false),
            last_error: Mutex::new((String::new(), None)),
            target_locks: Mutex::new(HashMap::new()),
            target_fail: Mutex::new(HashMap::new()),
            dir_tx: Mutex::new(None),
            gossip,
            dir_started: AtomicBool::new(false),
            pending_peers: Mutex::new(extra_peers),
            rebuilding: Mutex::new(HashSet::new()),
            intro: Mutex::new(Vec::new()),
            service: Mutex::new(Vec::new()),
            service_handler: Mutex::new(None),
            intro_busy: AtomicBool::new(false),
            identity: Mutex::new(identity),
        }));
        if relay_on {
            me.ensure_directory();
        }
        me.spawn_maintenance();
        me.spawn_cover_traffic();
        Ok(me)
    }

    /// Ключ личности скрытого приёма (см. `Inner::identity`); вызывать до `set_enabled(true)`.
    pub fn set_identity(&self, sk: iroh::SecretKey) {
        *self.0.identity.lock().unwrap() = sk;
    }
    fn identity(&self) -> iroh::SecretKey {
        self.0.identity.lock().unwrap().clone()
    }
    /// EndpointId транспорта Mycelium (может отличаться от личности).
    pub fn transport_id(&self) -> EndpointId {
        self.0.endpoint.id()
    }
    /// Подсказки адреса релеев (ip:port, url реле) — например, из трекера: прямое QUIC без discovery.
    pub fn add_hints(&self, hints: Vec<(EndpointId, Vec<String>)>) {
        for (n, a) in hints {
            self.0.dir.add_hint(n, a);
        }
    }

    /// Обработчик входящих запросов скрытого приёма (без него INTRODUCE игнорируются).
    pub fn set_service_handler(&self, h: ServiceHandler) {
        *self.0.service_handler.lock().unwrap() = Some(h);
    }
    pub fn set_cover_traffic(&self, on: bool) {
        self.0.cfg.lock().unwrap().cover_traffic = on;
    }
    pub fn set_hidden(&self, on: bool) {
        self.0.cfg.lock().unwrap().hidden = on;
        if !on {
            for c in self.0.intro.lock().unwrap().drain(..) {
                c.close();
            }
        }
    }

    pub fn handler(&self) -> RelayHandler {
        self.0.handler.clone()
    }
    pub fn directory(&self) -> &Directory {
        &self.0.dir
    }
    pub fn is_enabled(&self) -> bool {
        self.0.enabled.load(Ordering::Relaxed)
    }

    /// Подписаться на gossip-топик директории (один раз; до этого Mycelium молчит в сети).
    fn ensure_directory(&self) {
        let Some(g) = self.0.gossip.clone() else { return };
        if self.0.dir_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let peers: Vec<EndpointId> = std::mem::take(&mut *self.0.pending_peers.lock().unwrap());
        self.spawn_directory(g, peers);
    }

    /// Вкл/выкл клиентский режим: при включении — фоновое построение пула.
    pub fn set_enabled(&self, on: bool) {
        self.0.enabled.store(on, Ordering::SeqCst);
        if on {
            self.ensure_directory();
            let me = self.clone();
            tokio::spawn(async move {
                me.fill_pool().await;
                me.ensure_hidden_service().await;
            });
        } else {
            for c in self.0.pool.lock().unwrap().drain(..) {
                c.close();
            }
            for (_, c) in self.0.per_target.lock().unwrap().drain() {
                c.close();
            }
            for c in self.0.intro.lock().unwrap().drain(..).chain(self.0.service.lock().unwrap().drain(..)) {
                c.close();
            }
        }
    }
    pub fn set_relay_enabled(&self, on: bool) {
        let was = self.0.handler.0.enabled.swap(on, Ordering::Relaxed);
        self.0.cfg.lock().unwrap().relay = on;
        if on && !was {
            self.ensure_directory();
            let tx = self.0.dir_tx.lock().unwrap().clone();
            if let Some(tx) = tx {
                let me = self.clone();
                tokio::spawn(async move { me.announce(&tx).await });
            }
        }
    }
    pub fn set_bandwidth(&self, bps: u64) {
        self.0.handler.0.limiter.set(bps);
        self.0.handler.0.limits.lock().unwrap().bandwidth = bps;
        self.0.cfg.lock().unwrap().limits.bandwidth = bps;
    }
    pub fn set_hops(&self, hops: u8) {
        self.0.cfg.lock().unwrap().hops = hops.clamp(1, 3);
    }
    /// Подключить к топику директории дополнительных пиров (знакомые узлы из книги пиров).
    pub fn join_peers(&self, peers: Vec<EndpointId>) {
        let me = self.0.endpoint.id();
        let peers: Vec<EndpointId> = peers.into_iter().filter(|p| *p != me).collect();
        if peers.is_empty() {
            return;
        }
        let tx = self.0.dir_tx.lock().unwrap().clone();
        match tx {
            Some(tx) => {
                tokio::spawn(async move {
                    let _ = tx.join_peers(peers).await;
                });
            }
            None => {
                let mut pend = self.0.pending_peers.lock().unwrap();
                for p in peers {
                    if !pend.contains(&p) {
                        pend.push(p);
                    }
                }
            }
        }
    }
    pub fn add_bootstrap(&self, nodes: Vec<EndpointId>) {
        self.join_peers(nodes.clone());
        self.0.dir.add_bootstrap(nodes.clone());
        let mut cfg = self.0.cfg.lock().unwrap();
        for n in nodes {
            if !cfg.bootstrap.contains(&n) {
                cfg.bootstrap.push(n);
            }
        }
    }

    pub fn status(&self) -> Status {
        // ВАЖНО: каждый замок берётся отдельным оператором. Временный MutexGuard внутри литерала
        // `Status { .. }` живёт до конца всего выражения — второй `cfg.lock()` в том же литерале
        // навсегда блокировал поток tokio (все опросы статуса → весь рантайм приложения вставал).
        let m = &self.0.handler.0.metrics;
        let (hops, hidden, cover_traffic) = {
            let c = self.0.cfg.lock().unwrap();
            (c.hops, c.hidden, c.cover_traffic)
        };
        let want = hops.clamp(1, 3);
        let live: Vec<usize> = {
            let pool = self.0.pool.lock().unwrap();
            pool.iter().filter(|c| !c.is_closed()).map(|c| c.path.len()).collect()
        };
        let live_pt: Vec<usize> = {
            let pt = self.0.per_target.lock().unwrap();
            pt.values().filter(|c| !c.is_closed()).map(|c| c.path.len()).collect()
        };
        let min_len = live.iter().chain(live_pt.iter()).min().copied().unwrap_or(0) as u8;
        let guards: Vec<String> = {
            let g = self.0.guards.lock().unwrap();
            g.guards.iter().map(|g| g.to_string()).collect()
        };
        let (last_error, err_at) = { self.0.last_error.lock().unwrap().clone() };
        let intro_ready = {
            let i = self.0.intro.lock().unwrap();
            i.iter().filter(|c| !c.is_closed()).count()
        };
        let self_ids = self.self_ids();
        let relays_usable = self.0.dir.usable(&self_ids);
        let relays_known = self.0.dir.count() + self.0.dir.bootstrap().len();
        let services_known = self.0.dir.services_count();
        Status {
            relays_usable,
            hops_effective: min_len,
            hops_reduced: min_len > 0 && min_len < want,
            enabled: self.is_enabled(),
            circuits_ready: live.len() + live_pt.len(),
            last_error_age_secs: err_at.map(|t| t.elapsed().as_secs()).unwrap_or(0),
            circuits_building: self.0.building.load(Ordering::Relaxed),
            relays_known,
            guards,
            hops,
            relay_enabled: self.0.handler.0.enabled.load(Ordering::Relaxed),
            relay_circuits_active: m.circuits_active.load(Ordering::Relaxed),
            relay_circuits_total: m.circuits_total.load(Ordering::Relaxed),
            relay_cells: m.cells_relayed.load(Ordering::Relaxed),
            relay_bytes: m.bytes_relayed.load(Ordering::Relaxed),
            exit_streams: m.exit_streams.load(Ordering::Relaxed),
            // старая ошибка при живых цепочках — уже не актуальна
            last_error: if !live.is_empty() || !live_pt.is_empty() { if err_at.is_some_and(|t| t.elapsed() < LAST_ERROR_FRESH) { last_error } else { String::new() } } else { last_error },
            hidden,
            intro_ready,
            services_known,
            cover_traffic,
            relay_intro_active: m.intro_active.load(Ordering::Relaxed),
            relay_rendezvous_total: m.rendezvous_total.load(Ordering::Relaxed),
        }
    }

    /// Путь, последний хоп которого — заданный узел (RP или intro-точка).
    fn path_ending_at(&self, last: EndpointId, exclude: &[EndpointId]) -> Result<Vec<EndpointId>> {
        let mut ex = exclude.to_vec();
        ex.push(last);
        let mut path = self.choose_path(&ex)?;
        *path.last_mut().unwrap() = last;
        Ok(path)
    }

    async fn build_to(&self, last: EndpointId, exclude: &[EndpointId]) -> Result<Arc<Circuit>> {
        let path = self.path_ending_at(last, exclude)?;
        let first = self.0.dir.addr_of(path[0]);
        Circuit::build_via(&self.0.endpoint, path, Some(first)).await
    }

    /// Свои id (транспорт и личность) — никогда не хоп собственной цепочки.
    fn self_ids(&self) -> Vec<EndpointId> {
        let mut v = vec![self.0.endpoint.id(), self.0.identity.lock().unwrap().public()];
        v.dedup();
        v
    }

    /// A: цепочка до скрытого получателя через точку встречи (NIP-1 §7): ESTABLISH_RENDEZVOUS на RP →
    /// INTRODUCE1 через intro-точку B → B приходит на RP → сквозной слой A↔B. RP/intro не видят ни A, ни B.
    async fn connect_hidden(&self, target: EndpointId, d: ServiceDescriptor) -> Result<Arc<Circuit>> {
        let mut excl = vec![target];
        excl.extend(d.intros.iter().copied());
        // RP-цепочка: из пула (если не проходит через B/intro), иначе свежая
        let from_pool = {
            let mut pool = self.0.pool.lock().unwrap();
            pool.retain(|c| !c.is_closed());
            pool.iter().position(|c| !c.path.iter().any(|n| excl.contains(n))).map(|i| pool.remove(i))
        };
        let rv = match from_pool {
            Some(c) => c,
            None => self.build_one(&excl).await?,
        };
        let rp = *rv.path.last().unwrap();
        let res: Result<()> = async {
            let cookie = rv.establish_rendezvous().await?;
            let (sk, c_e) = crypto::client_begin();
            let mut last_err = anyhow!("no intro points");
            for intro in &d.intros {
                let ic = match self.build_to(*intro, &[target, rp]).await {
                    Ok(c) => c,
                    Err(e) => {
                        last_err = e;
                        continue;
                    }
                };
                let r = ic.introduce(&target, &rp, &cookie, &c_e).await;
                ic.close();
                match r {
                    Ok(()) => return rv.await_rendezvous(sk, &c_e, &target, RENDEZVOUS_WAIT).await,
                    Err(e) => last_err = e,
                }
            }
            Err(last_err)
        }
        .await;
        match res {
            Ok(()) => Ok(rv),
            Err(e) => {
                rv.close();
                self.set_error(format!("rendezvous: {e}"));
                Err(e)
            }
        }
    }

    /// B: intro-цепочки (регистрация на intro-релеях) + публикация описателя.
    async fn maintain_intro(&self, tx: Option<iroh_gossip::api::GossipSender>) {
        let (hidden, want) = {
            let c = self.0.cfg.lock().unwrap();
            (c.hidden, c.intro_points.clamp(1, 4))
        };
        if !hidden || self.0.service_handler.lock().unwrap().is_none() || self.0.intro_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let before: Vec<EndpointId> = self.intro_points();
        self.0.intro.lock().unwrap().retain(|c| !c.is_closed() && !c.needs_rotation());
        let mut tries = 0;
        while self.is_enabled() && self.0.intro.lock().unwrap().len() < want && tries < 4 {
            tries += 1;
            let excl: Vec<EndpointId> = self.intro_points();
            match self.build_one(&excl).await {
                Ok(c) => match c.establish_intro(&self.identity()).await {
                    Ok(rx) => {
                        self.spawn_intro_listener(c.clone(), rx);
                        self.0.intro.lock().unwrap().push(c);
                    }
                    Err(e) => {
                        c.close();
                        self.set_error(format!("intro: {e}"));
                    }
                },
                Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
            }
        }
        self.0.intro_busy.store(false, Ordering::SeqCst);
        let now = self.intro_points();
        if let (Some(tx), true) = (tx, !now.is_empty() && now != before) {
            self.announce_service(&tx).await;
        }
    }

    fn intro_points(&self) -> Vec<EndpointId> {
        self.0.intro.lock().unwrap().iter().filter(|c| !c.is_closed()).map(|c| *c.path.last().unwrap()).collect()
    }

    pub fn my_service_descriptor(&self) -> Option<ServiceDescriptor> {
        let intros = self.intro_points();
        if intros.is_empty() {
            return None;
        }
        let sk = self.identity();
        Some(ServiceDescriptor { owner: sk.public(), intros, ts: directory::now_secs(), sig: [0u8; 64] }.sign(&sk))
    }

    async fn announce_service(&self, tx: &iroh_gossip::api::GossipSender) {
        if let Some(d) = self.my_service_descriptor() {
            if let Ok(b) = serde_json::to_vec(&d) {
                let _ = tx.broadcast(b.into()).await;
            }
        }
    }

    /// B: INTRODUCE2 с intro-релея → цепочка к RP, RENDEZVOUS1, обслуживание запросов A.
    fn spawn_intro_listener(&self, ic: Arc<Circuit>, mut rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) {
        let me = self.clone();
        tokio::spawn(async move {
            while let Some(data) = rx.recv().await {
                if ic.is_closed() {
                    break;
                }
                if data.len() < 32 + cell::COOKIE_LEN + 32 {
                    continue;
                }
                let Ok(rp) = EndpointId::from_bytes(data[..32].try_into().unwrap()) else { continue };
                let cookie: [u8; cell::COOKIE_LEN] = data[32..32 + cell::COOKIE_LEN].try_into().unwrap();
                let c_e: [u8; 32] = data[32 + cell::COOKIE_LEN..64 + cell::COOKIE_LEN].try_into().unwrap();
                let me2 = me.clone();
                tokio::spawn(async move {
                    if let Err(e) = me2.serve_rendezvous(rp, cookie, c_e).await {
                        tracing::debug!("mycelium: rendezvous (B): {e}");
                        me2.set_error(format!("rendezvous (B): {e}"));
                    }
                });
            }
        });
    }

    async fn serve_rendezvous(&self, rp: EndpointId, cookie: [u8; cell::COOKIE_LEN], c_e: [u8; 32]) -> Result<()> {
        let handler = self.0.service_handler.lock().unwrap().clone().ok_or_else(|| anyhow!("no service handler"))?;
        let c = self.build_to(rp, &[]).await?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServiceReq>();
        c.rendezvous_answer(&self.identity(), &cookie, &c_e, tx)?;
        self.0.service.lock().unwrap().retain(|s| !s.is_closed());
        self.0.service.lock().unwrap().push(c.clone());
        tokio::spawn(async move {
            while let Some((sid, alpn, req)) = rx.recv().await {
                let resp = (*handler)(alpn, req).await.unwrap_or_default();
                if c.service_respond(sid, &resp).is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    /// Маскирующий трафик: PADDING на всех живых цепочках со случайным интервалом 0.7–2.5 с.
    fn spawn_cover_traffic(&self) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                let ms = 700 + (rand::random::<u64>() % 1800);
                tokio::time::sleep(Duration::from_millis(ms)).await;
                if !me.is_enabled() || !me.0.cfg.lock().unwrap().cover_traffic {
                    continue;
                }
                let mut all: Vec<Arc<Circuit>> = me.0.per_target.lock().unwrap().values().cloned().collect();
                all.extend(me.0.pool.lock().unwrap().iter().cloned());
                all.extend(me.0.intro.lock().unwrap().iter().cloned());
                all.extend(me.0.service.lock().unwrap().iter().cloned());
                for c in all.into_iter().filter(|c| !c.is_closed()) {
                    let _ = c.send_padding();
                }
            }
        });
    }

    /// Выбрать путь: Guard из закреплённого набора, Middle/Exit — взвешенно, без повторов и «семей».
    /// Релеев меньше, чем хопов → путь короче (последний хоп обязан уметь Exit); как только релеев
    /// станет достаточно, обслуживание пересоберёт цепочку до полной длины.
    fn choose_path(&self, exclude: &[EndpointId]) -> Result<Vec<EndpointId>> {
        let hops = self.0.cfg.lock().unwrap().hops.clamp(1, 3) as usize;
        let mut excl: Vec<EndpointId> = exclude.to_vec();
        excl.extend(self.self_ids());
        let mut fams: Vec<String> = Vec::new();
        let mut path = Vec::with_capacity(hops);
        {
            let mut gs = self.0.guards.lock().unwrap();
            // ушедшие из сети guard'ы (нет ни описателя, ни bootstrap) и свои id — сразу вон
            let before = gs.guards.len();
            let me = self.self_ids();
            gs.guards.retain(|g| !me.contains(g) && self.0.dir.is_known(g));
            gs.refill(&self.0.dir);
            if gs.guards.len() != before || gs.chosen_at == 0 {
                let cfg = self.0.cfg.lock().unwrap();
                let _ = gs.save(&cfg.data_dir.join("guards.json"));
            }
            let guard = match gs.guards.iter().find(|g| !excl.contains(g)).copied() {
                Some(g) => g,
                None => self.0.dir.pick(Role::Guard, &excl, &[]).map(|x| x.0).ok_or_else(|| anyhow!("no known relays (guard)"))?,
            };
            path.push(guard);
            excl.push(guard);
        }
        for role in [Role::Middle, Role::Exit].into_iter().take(hops - 1) {
            let Some((n, fam)) = self.0.dir.pick(role, &excl, &fams) else { break };
            if !fam.is_empty() {
                fams.push(fam);
            }
            excl.push(n);
            path.push(n);
        }
        if !self.0.dir.can_exit(path.last().unwrap()) {
            bail!("no relays for role Exit");
        }
        if path.len() < hops {
            tracing::debug!("mycelium: only {} of {hops} hops available — reduced circuit", path.len());
        }
        Ok(path)
    }

    fn set_error(&self, e: String) {
        *self.0.last_error.lock().unwrap() = (e, Some(std::time::Instant::now()));
    }

    /// Собрать цепочку: до `BUILD_ATTEMPTS` попыток; сбой засчитывается только виновному хопу,
    /// он же исключается из следующей попытки (раньше штрафовался весь путь, включая guard).
    async fn build_one(&self, exclude: &[EndpointId]) -> Result<Arc<Circuit>> {
        let mut excl = exclude.to_vec();
        let mut last: Option<anyhow::Error> = None;
        let t_start = std::time::Instant::now();
        for attempt in 0..BUILD_ATTEMPTS {
            if !self.is_enabled() {
                bail!("Mycelium is disabled");
            }
            if attempt > 0 && t_start.elapsed() > BUILD_BUDGET {
                break;
            }
            let path = match self.choose_path(&excl) {
                Ok(p) => p,
                Err(e) => {
                    last.get_or_insert(e);
                    break;
                }
            };
            let t0 = std::time::Instant::now();
            let first = self.0.dir.addr_of(path[0]);
            match Circuit::build_via(&self.0.endpoint, path.clone(), Some(first)).await {
                Ok(c) => {
                    self.0.dir.note_latency(path[0], t0.elapsed().as_millis() as u32);
                    *self.0.last_error.lock().unwrap() = (String::new(), None);
                    return Ok(c);
                }
                Err(e) => {
                    match failed_hop(&e.to_string()).filter(|i| *i < path.len()) {
                        Some(i) => {
                            self.0.dir.note_failure(path[i]);
                            excl.push(path[i]);
                            // мёртвый guard не держим 30 дней: после 3 сбоев подряд — замена
                            if i == 0 && self.0.dir.failures(&path[0]) >= 3 {
                                self.0.guards.lock().unwrap().guards.retain(|g| *g != path[0]);
                            }
                        }
                        // виновный неизвестен — guard оставляем, остальной путь меняем
                        None => excl.extend(path.iter().skip(1).copied()),
                    }
                    tracing::debug!("mycelium: build failed ({e}), retrying without the failed hop");
                    last = Some(e);
                }
            }
        }
        let e = last.unwrap_or_else(|| anyhow!("no relays"));
        self.set_error(e.to_string());
        Err(e)
    }

    /// Пул: держим `pool_size` готовых цепочек, чтобы первое сообщение не ждало рукопожатий.
    async fn fill_pool(&self) {
        if self.0.building.swap(true, Ordering::SeqCst) {
            return;
        }
        let want = self.0.cfg.lock().unwrap().pool_size;
        let mut tries = 0;
        while self.is_enabled() && tries < 6 {
            self.0.pool.lock().unwrap().retain(|c| !c.is_closed() && !c.needs_rotation());
            if self.0.pool.lock().unwrap().len() >= want {
                break;
            }
            tries += 1;
            match self.build_one(&[]).await {
                Ok(c) => self.0.pool.lock().unwrap().push(c),
                Err(_) => tokio::time::sleep(Duration::from_secs(3)).await,
            }
        }
        self.0.building.store(false, Ordering::SeqCst);
    }

    /// Цепочка для адресата: своя на каждого (изоляция), берётся из пула или строится.
    async fn circuit_for(&self, target: EndpointId) -> Result<Arc<Circuit>> {
        if !self.is_enabled() {
            bail!("Mycelium is disabled");
        }
        if let Some(c) = self.0.per_target.lock().unwrap().get(&target).cloned() {
            if !c.is_closed() && !c.needs_rotation() {
                return Ok(c);
            }
        }
        // single-flight: параллельные запросы к одному адресату ждут одну сборку, а не строят N цепочек
        let lock = self.0.target_locks.lock().unwrap().entry(target).or_default().clone();
        let _flight = lock.lock().await;
        if let Some(c) = self.0.per_target.lock().unwrap().get(&target).cloned() {
            if !c.is_closed() && !c.needs_rotation() {
                return Ok(c);
            }
        }
        // лидер только что провалился — не повторяем N сборок подряд за каждым соединением
        let recent = self.0.target_fail.lock().unwrap().get(&target).filter(|(t, _)| t.elapsed() < TARGET_FAIL_TTL).map(|(_, e)| e.clone());
        if let Some(e) = recent {
            bail!("{e}");
        }
        let r = self.circuit_for_locked(target).await;
        match &r {
            Ok(_) => {
                self.0.target_fail.lock().unwrap().remove(&target);
            }
            Err(e) => {
                self.0.target_fail.lock().unwrap().insert(target, (std::time::Instant::now(), e.to_string()));
            }
        }
        r
    }

    /// Сборка цепочки к адресату (вызывается под single-flight замком).
    async fn circuit_for_locked(&self, target: EndpointId) -> Result<Arc<Circuit>> {
        // у адресата опубликован описатель скрытого приёма → через точку встречи (Exit не нужен)
        if let Some(d) = self.0.dir.service(&target) {
            let c = self.connect_hidden(target, d).await?;
            self.0.per_target.lock().unwrap().insert(target, c.clone());
            let me = self.clone();
            tokio::spawn(async move { me.fill_pool().await });
            return Ok(c);
        }
        let from_pool = {
            let mut pool = self.0.pool.lock().unwrap();
            pool.retain(|c| !c.is_closed());
            // цепочка не должна проходить через самого адресата
            pool.iter().position(|c| !c.path.contains(&target)).map(|i| pool.remove(i))
        };
        let c = match from_pool {
            Some(c) => c,
            None => self.build_one(&[target]).await?,
        };
        self.0.per_target.lock().unwrap().insert(target, c.clone());
        let me = self.clone();
        tokio::spawn(async move { me.fill_pool().await });
        Ok(c)
    }

    /// Открыть поток к `target` по `alpn` через цепочку (как `endpoint.connect(...).open_bi()`).
    pub async fn open_stream(&self, target: EndpointId, alpn: &[u8]) -> Result<MushroomStream> {
        let c = self.circuit_for(target).await?;
        match c.open_stream(target, alpn).await {
            Ok(s) => Ok(s),
            Err(e) => {
                // цепочка могла умереть — одна повторная попытка на свежей
                {
                    // убираем только свою цепочку — другой поток мог уже поставить свежую
                    let mut pt = self.0.per_target.lock().unwrap();
                    if pt.get(&target).is_some_and(|x| Arc::ptr_eq(x, &c)) {
                        pt.remove(&target);
                    }
                }
                c.close();
                let c2 = self.circuit_for(target).await.map_err(|e2| anyhow!("{e}; retry: {e2}"))?;
                c2.open_stream(target, alpn).await
            }
        }
    }

    /// Запрос-ответ через цепочку адресата с замером полного времени до собеседника.
    pub async fn request(&self, target: EndpointId, alpn: &[u8], payload: &[u8], max: usize) -> Result<Vec<u8>> {
        let t0 = std::time::Instant::now();
        let s = self.open_stream(target, alpn).await?;
        let c = s.circuit().clone();
        let r = s.request(payload, max).await?;
        c.note_e2e(t0.elapsed());
        Ok(r)
    }

    fn info_of(&self, c: &Circuit, target: Option<EndpointId>) -> CircuitInfo {
        let rebuilding = self.0.rebuilding.lock().unwrap().contains(&c.id);
        c.info(target, rebuilding)
    }

    /// Живые цепочки для карты: сначала изолированные (с адресатом), затем пул.
    /// Попутно шлёт PING хопам (не чаще раза в 3 с) — свежие RTT появятся к следующему опросу.
    pub fn circuits(&self) -> Vec<CircuitInfo> {
        let gap = Duration::from_secs(3);
        let mut out = Vec::new();
        for (t, c) in self.0.per_target.lock().unwrap().iter().filter(|(_, c)| !c.is_closed()) {
            c.probe_hops(gap);
            out.push(self.info_of(c, Some(*t)));
        }
        for c in self.0.pool.lock().unwrap().iter().filter(|c| !c.is_closed()) {
            c.probe_hops(gap);
            out.push(self.info_of(c, None));
        }
        let extra: Vec<Arc<Circuit>> = self.0.intro.lock().unwrap().iter().chain(self.0.service.lock().unwrap().iter()).filter(|c| !c.is_closed()).cloned().collect();
        for c in extra {
            c.probe_hops(gap);
            out.push(self.info_of(&c, None));
        }
        out
    }

    /// Фаза 3 (системный VPN): адреса, которые TUN-туннель обязан пускать мимо себя, иначе
    /// транспорт самого Mycelium уйдёт в туннель и зациклится — guard-релеи (прямые ip:port
    /// и их реле) и домашние реле iroh этого узла.
    pub async fn bypass_addrs(&self) -> (Vec<std::net::SocketAddr>, Vec<iroh::RelayUrl>) {
        let mut ids: Vec<EndpointId> = self.0.guards.lock().unwrap().guards.clone();
        let live: Vec<EndpointId> = {
            let pt = self.0.per_target.lock().unwrap();
            let pool = self.0.pool.lock().unwrap();
            let intro = self.0.intro.lock().unwrap();
            let service = self.0.service.lock().unwrap();
            pt.values().chain(pool.iter()).chain(intro.iter()).chain(service.iter()).filter(|c| !c.is_closed()).filter_map(|c| c.path.first().copied()).collect()
        };
        ids.extend(live);
        ids.sort();
        ids.dedup();
        let mut ips = Vec::new();
        let mut relays: Vec<iroh::RelayUrl> = self.0.endpoint.addr().relay_urls().cloned().collect();
        for id in ids {
            if let Some(info) = self.0.endpoint.remote_info(id).await {
                for a in info.addrs() {
                    match a.addr() {
                        iroh::TransportAddr::Ip(sa) => ips.push(*sa),
                        iroh::TransportAddr::Relay(u) => relays.push(u.clone()),
                        _ => {}
                    }
                }
            }
        }
        ips.sort();
        ips.dedup();
        relays.dedup();
        (ips, relays)
    }

    /// Цепочка конкретного собеседника (если уже построена).
    pub fn circuit_for_target(&self, target: &EndpointId) -> Option<CircuitInfo> {
        let c = self.0.per_target.lock().unwrap().get(target).filter(|c| !c.is_closed()).cloned()?;
        c.probe_hops(Duration::from_secs(3));
        Some(self.info_of(&c, Some(*target)))
    }

    /// Слот цепочки по id: адресат изолированной или `None` для пула.
    fn locate(&self, id: u32) -> Option<(Option<EndpointId>, Arc<Circuit>)> {
        if let Some((t, c)) = self.0.per_target.lock().unwrap().iter().find(|(_, c)| c.id == id) {
            return Some((Some(*t), c.clone()));
        }
        self.0.pool.lock().unwrap().iter().find(|c| c.id == id).map(|c| (None, c.clone()))
    }

    /// Построить замену цепочке `id` (мёртвые хопы исключаются), поставить в тот же слот и
    /// закрыть старую. Пока строится — старая остаётся в карте с `rebuilding: true`.
    /// `auto` — вызов из обслуживания (увеличивает счётчик автопересборок).
    pub async fn rebuild(&self, id: u32, auto: bool) -> Result<CircuitInfo> {
        if !self.is_enabled() {
            bail!("Mycelium is disabled");
        }
        let (target, old) = self.locate(id).ok_or_else(|| anyhow!("circuit {id:#x} not found"))?;
        if !self.0.rebuilding.lock().unwrap().insert(id) {
            bail!("circuit is already being rebuilt");
        }
        let mut exclude = old.dead_hops();
        for n in &exclude {
            self.0.dir.note_failure(*n);
        }
        if let Some(t) = target {
            exclude.push(t);
        }
        // медленная, но живая цепочка: Middle/Exit меняем (guard закреплён — остаётся)
        // (короткая цепочка — это апгрейд до полной длины: старые хопы не исключаем)
        let want = self.0.cfg.lock().unwrap().hops.clamp(1, 3) as usize;
        if exclude.len() == target.is_some() as usize && old.path.len() >= want {
            exclude.extend(old.path.iter().skip(1).copied());
        }
        let res = self.build_one(&exclude).await;
        self.0.rebuilding.lock().unwrap().remove(&id);
        let new = res?;
        new.set_retries(if auto { old.retries() + 1 } else { 0 });
        match target {
            Some(t) => {
                self.0.per_target.lock().unwrap().insert(t, new.clone());
            }
            None => {
                let mut pool = self.0.pool.lock().unwrap();
                pool.retain(|c| c.id != id);
                pool.push(new.clone());
            }
        }
        old.close();
        tracing::info!("mycelium: circuit {id:#x} rebuilt → {:#x} ({})", new.id, if auto { "auto" } else { "manual" });
        Ok(self.info_of(&new, target))
    }

    /// Автопересборка деградировавших цепочек: до `AUTO_REBUILD_MAX` попыток подряд,
    /// затем — кнопка «Построить новую цепочку» в UI.
    /// Цепочка с мёртвым хопом пересобирается без лимита (пауза растёт 20→160 с), медленная —
    /// до `AUTO_REBUILD_MAX` раз, короткая (релеев не хватало) — апгрейдится при появлении релеев.
    async fn auto_rebuild(&self) {
        let want = self.0.cfg.lock().unwrap().hops.clamp(1, 3) as usize;
        let mut all: Vec<(Option<EndpointId>, Arc<Circuit>)> = self.0.per_target.lock().unwrap().iter().map(|(t, c)| (Some(*t), c.clone())).collect();
        all.extend(self.0.pool.lock().unwrap().iter().map(|c| (None, c.clone())));
        for (target, c) in all.into_iter().filter(|(_, c)| !c.is_closed()) {
            c.probe_hops(Duration::from_secs(3));
            if c.created.elapsed() < AUTO_REBUILD_MIN_AGE || self.0.rebuilding.lock().unwrap().contains(&c.id) {
                continue;
            }
            let dead = !c.dead_hops().is_empty();
            let short = c.path.len() < want && c.created.elapsed() >= UPGRADE_MIN_AGE && {
                let mut ex = self.self_ids();
                ex.extend(target);
                self.0.dir.usable(&ex) > c.path.len()
            };
            if dead {
                let backoff = (AUTO_REBUILD_MIN_AGE * (1u32 << c.retries().min(3))).min(DEAD_BACKOFF_MAX);
                if c.created.elapsed() < backoff {
                    continue;
                }
            } else if !short {
                if !c.is_degraded() {
                    if c.retries() > 0 && c.created.elapsed() > RETRIES_RESET_AFTER {
                        c.set_retries(0);
                    }
                    continue;
                }
                if c.retries() >= AUTO_REBUILD_MAX {
                    continue;
                }
            }
            let me = self.clone();
            let id = c.id;
            tokio::spawn(async move {
                if let Err(e) = me.rebuild(id, true).await {
                    tracing::debug!("mycelium: auto-rebuild {id:#x}: {e}");
                }
            });
        }
    }

    /// Описатель собственного релея (подписанный).
    pub fn my_descriptor(&self) -> RelayDescriptor {
        let cfg = self.0.cfg.lock().unwrap();
        RelayDescriptor {
            node: self.0.endpoint.id(),
            version: directory::PROTO_VERSION,
            guard: true,
            middle: true,
            exit: true,
            bandwidth: cfg.limits.bandwidth,
            max_circuits: cfg.limits.max_circuits,
            exit_alpns: cfg.exit_policy.allowed_alpn_prefixes.clone(),
            family: String::new(),
            ts: directory::now_secs(),
            sig: [0u8; 64],
            addrs: self.my_addr_hints(),
        }
        .sign(self.0.endpoint.secret_key())
    }

    /// Свои прямые ip:port и реле iroh — подсказки в описателе релея.
    fn my_addr_hints(&self) -> Vec<String> {
        let a = self.0.endpoint.addr();
        let mut v: Vec<String> = a.ip_addrs().map(|s| s.to_string()).take(6).collect();
        v.extend(a.relay_urls().map(|u| u.to_string()));
        v
    }

    /// Директория по gossip: принимаем описатели; свой (если релей вкл) анонсируем каждые 5 минут
    /// и сразу при появлении нового соседа — новичку не нужно ждать полный период.
    fn spawn_directory(&self, gossip: iroh_gossip::net::Gossip, extra_peers: Vec<EndpointId>) {
        let me = self.clone();
        tokio::spawn(async move {
            use n0_future::StreamExt;
            let mut boot = me.0.dir.bootstrap();
            boot.extend(extra_peers);
            let myid = me.0.endpoint.id();
            boot.retain(|p| *p != myid);
            let Ok(sub) = gossip.subscribe(dir_topic(), boot).await else {
                // повторная попытка подписки при следующем включении
                me.0.dir_started.store(false, Ordering::SeqCst);
                return;
            };
            let (tx, mut rx) = sub.split();
            *me.0.dir_tx.lock().unwrap() = Some(tx.clone());
            // пиры, пришедшие пока шла подписка
            let late: Vec<EndpointId> = std::mem::take(&mut *me.0.pending_peers.lock().unwrap());
            if !late.is_empty() {
                let _ = tx.join_peers(late).await;
            }
            let me2 = me.clone();
            let tx2 = tx.clone();
            tokio::spawn(async move {
                loop {
                    me2.announce(&tx2).await;
                    if me2.is_enabled() {
                        me2.announce_service(&tx2).await;
                    }
                    tokio::time::sleep(directory::ANNOUNCE_EVERY).await;
                }
            });
            while let Some(Ok(ev)) = rx.next().await {
                match ev {
                    iroh_gossip::api::Event::Received(m) => {
                        if let Ok(d) = serde_json::from_slice::<RelayDescriptor>(&m.content) {
                            if d.node != myid && me.0.dir.insert(d) {
                                tracing::debug!("mycelium: relay added to directory, total {}", me.0.dir.count());
                            }
                        } else if let Ok(d) = serde_json::from_slice::<ServiceDescriptor>(&m.content) {
                            if !me.self_ids().contains(&d.owner) && me.0.dir.insert_service(d) {
                                tracing::debug!("mycelium: hidden-service descriptor, total {}", me.0.dir.services_count());
                            }
                        }
                    }
                    iroh_gossip::api::Event::NeighborUp(_) => {
                        me.announce(&tx).await;
                        if me.is_enabled() {
                            me.announce_service(&tx).await;
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    async fn announce(&self, tx: &iroh_gossip::api::GossipSender) {
        if self.0.handler.0.enabled.load(Ordering::Relaxed) {
            if let Ok(b) = serde_json::to_vec(&self.my_descriptor()) {
                let _ = tx.broadcast(b.into()).await;
            }
        }
    }

    /// Ротация (10 мин / 1 млн ячеек), PING хопов живых цепочек, автопересборка, добор пула.
    fn spawn_maintenance(&self) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(MAINTENANCE_EVERY).await;
                if !me.is_enabled() {
                    continue;
                }
                let rotten: Vec<EndpointId> = me.0.per_target.lock().unwrap().iter().filter(|(_, c)| c.is_closed() || c.needs_rotation()).map(|(k, _)| *k).collect();
                for k in rotten {
                    if let Some(c) = me.0.per_target.lock().unwrap().remove(&k) {
                        c.close();
                    }
                }
                me.auto_rebuild().await;
                me.fill_pool().await;
                me.0.service.lock().unwrap().retain(|c| !c.is_closed());
                let tx = me.0.dir_tx.lock().unwrap().clone();
                me.maintain_intro(tx).await;
            }
        });
    }

    /// Публичный вызов: поднять intro-цепочки сейчас (после set_enabled, не ждать 30 с обслуживания).
    pub async fn ensure_hidden_service(&self) {
        let tx = self.0.dir_tx.lock().unwrap().clone();
        self.maintain_intro(tx).await;
    }
}

/// Топик директории: sha256(DIR_TOPIC) → 32 байта TopicId.
fn dir_topic() -> iroh_gossip::TopicId {
    use sha2::{Digest, Sha256};
    let h: [u8; 32] = Sha256::digest(directory::DIR_TOPIC).into();
    iroh_gossip::TopicId::from_bytes(h)
}

/// Индекс хопа пути, на котором упала сборка: «guard: …» → 0, «hop N: …» → N-1.
fn failed_hop(e: &str) -> Option<usize> {
    if e.starts_with("guard:") {
        return Some(0);
    }
    let n: usize = e.strip_prefix("hop ")?.split(':').next()?.trim().parse().ok()?;
    n.checked_sub(1)
}
