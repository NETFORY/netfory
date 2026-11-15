//! PQ Web — egress-туннель NETFORY для https://-сайтов поверх Iroh QUIC.
//!
//! Роль: узел с собственным EndpointId принимает стримы с ALPN `netfory/pqweb/1`,
//! по `NF1 CONNECT host:port` открывает TCP к сайту и прозрачно гоняет байты
//! (TLS остаётся сквозным клиент↔сайт, MITM нет). Allowlist — Domain Bundles,
//! лимиты стримов/полосы на клиента, SSRF-защита, метрики.
//!
//! Библиотека (`serve(Config)`) — чтобы позже встроить сервис в узлы SmartHoldem;
//! бинарь `pq-web` — консольная утилита поверх неё.

pub mod allow;
pub mod announce;
pub mod egress;
pub mod limits;
pub mod metrics;
pub mod proto;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::endpoint::{presets, Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, RelayMode, SecretKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use allow::Bundles;
use egress::{Egress, IpMode, V6Per, V6Prefix};
use limits::{ClientSlot, LimitConfig, Limiter};
use metrics::Metrics;
use proto::{ErrCode, Request};

#[derive(Debug, Clone)]
pub struct Config {
    /// UDP-адрес QUIC-сокета (по умолчанию 0.0.0.0:7843).
    pub bind: SocketAddr,
    /// Секретный ключ узла (32 байта). None → сгенерировать (см. `load_or_create_key`).
    pub secret_key: Option<SecretKey>,
    /// Relay-серверы (home relay). Пусто → публичные n0.
    pub relays: Vec<String>,
    /// Без DNS/pkarr-discovery (только прямой адрес/relay из EndpointAddr).
    pub no_discovery: bool,
    pub bundles: Bundles,
    /// `--open`: любые https-хосты (dVPN-режим), анонсируется трекерам как `open`.
    pub allow_all: bool,
    /// Разрешённые порты назначения.
    pub ports: Vec<u16>,
    pub limits: LimitConfig,
    /// Таймаут простоя стрима (нет байт ни в одну сторону).
    pub idle: Duration,
    /// Таймаут TCP-коннекта к сайту.
    pub connect_timeout: Duration,
    /// Метка региона для INFO (например "NL").
    pub region: String,
    /// Адрес мини-HTTP метрик (None — выключено).
    pub metrics_bind: Option<SocketAddr>,
    /// Трекеры NETFORY для саморегистрации (`POST /pqweb/announce`); пусто — не анонсироваться.
    pub trackers: Vec<String>,
    /// Период heartbeat'а на трекеры.
    pub announce_interval: Duration,
    /// Явный публичный ip:port (за NAT с проброшенным портом); иначе берётся из iroh.
    pub public_addr: Option<std::net::SocketAddr>,
    /// v0.4.0: семейство адресов для TCP к сайту (`auto` — автодетект исходящего IPv6).
    pub ip_mode: IpMode,
    /// v0.4.0: IPv6-префикс исходящих адресов.
    pub egress_v6_prefix: Option<V6Prefix>,
    /// v0.5.0: как выдавать IPv6 из префикса — `client` (стабильно на EndpointId) | `conn`.
    pub egress_v6_per: V6Per,
    /// v0.6.0: релей Mycelium (NIP-1) на том же EndpointId — ALPN `netfory/mycelium/1` + gossip-директория.
    pub mycelium: bool,
    /// Лимит канала релея Mycelium, байт/с (0 — без лимита).
    pub mycelium_bandwidth: u64,
    pub mycelium_max_circuits: u32,
    /// Соседние релеи Mycelium (EndpointId) для входа в директорию; остальные подхватываются с трекера.
    pub mycelium_bootstrap: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bind: "0.0.0.0:7843".parse().unwrap(),
            secret_key: None,
            relays: Vec::new(),
            no_discovery: false,
            bundles: Bundles::example(),
            allow_all: false,
            ports: vec![443],
            limits: LimitConfig { max_streams_per_client: 64, kbps_per_client: 20_000, max_streams_total: 4096 },
            idle: Duration::from_secs(90),
            connect_timeout: Duration::from_secs(8),
            region: String::new(),
            metrics_bind: None,
            trackers: announce::DEFAULT_TRACKERS.iter().map(|s| s.to_string()).collect(),
            announce_interval: announce::DEFAULT_INTERVAL,
            public_addr: None,
            ip_mode: IpMode::Auto,
            egress_v6_prefix: None,
            egress_v6_per: V6Per::Client,
            mycelium: false,
            mycelium_bandwidth: 4 * 1024 * 1024,
            mycelium_max_circuits: 256,
            mycelium_bootstrap: Vec::new(),
        }
    }
}

/// Ключ узла из файла (hex, 32 байта); если файла нет — сгенерировать и сохранить (0600).
pub fn load_or_create_key(path: &PathBuf) -> Result<SecretKey> {
    if path.exists() {
        let hex_str = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let bytes = hex::decode(hex_str.trim()).context("key file: bad hex")?;
        let arr: [u8; 32] = bytes.as_slice().try_into().context("key file: need 32 bytes")?;
        return Ok(SecretKey::from_bytes(&arr));
    }
    let key = SecretKey::generate();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(path, hex::encode(key.to_bytes())).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(key)
}

/// Запущенный сервис: держит Endpoint и Router; `shutdown()` для graceful-останова.
pub struct Server {
    pub endpoint: Endpoint,
    router: Router,
    pub metrics: Arc<Metrics>,
    pub limiter: Arc<Limiter>,
    /// Состояние egress (режим/IPv6) — для вывода в CLI.
    pub egress: Arc<Egress>,
    /// Релей Mycelium (v0.6.0), если включён `--mycelium`.
    pub mycelium: Option<mycelium::Mycelium>,
    announcer: Option<tokio::task::JoinHandle<()>>,
    reprobe: Option<tokio::task::JoinHandle<()>>,
    peers_loop: Option<tokio::task::JoinHandle<()>>,
}

impl Server {
    pub fn id(&self) -> iroh::EndpointId {
        self.endpoint.id()
    }
    pub async fn shutdown(self) {
        if let Some(a) = self.announcer {
            a.abort();
        }
        if let Some(r) = self.reprobe {
            r.abort();
        }
        if let Some(p) = self.peers_loop {
            p.abort();
        }
        let _ = self.router.shutdown().await;
        self.endpoint.close().await;
    }
}

/// Поднять узел и начать принимать стримы `netfory/pqweb/1`.
/// Реле NETFORY (те же, что встроены в клиент): общий home-relay с клиентами — путь
/// сервер→клиент через relay работает даже там, где n0-реле недоступны/замедлены.
pub const DEFAULT_RELAYS: &[&str] = &["https://relay-fsn7.sth.cx", "https://relay-ru1.sth.cx"];

pub async fn serve(cfg: Config) -> Result<Server> {
    let secret = cfg.secret_key.clone().unwrap_or_else(SecretKey::generate);
    let relays: Vec<String> = if cfg.relays.is_empty() { DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect() } else { cfg.relays.clone() };
    let relay_mode = if relays.iter().any(|r| r == "n0") {
        RelayMode::Default
    } else {
        use iroh_relay::{RelayConfig, RelayMap, RelayQuicConfig};
        let mut configs = Vec::new();
        for u in &relays {
            let url: iroh::RelayUrl = u.parse().with_context(|| format!("bad relay url {u}"))?;
            configs.push(Arc::new(RelayConfig::new(url, Some(RelayQuicConfig::default()))));
        }
        RelayMode::Custom(RelayMap::from_iter(configs))
    };
    let builder = if cfg.no_discovery {
        Endpoint::builder(presets::Minimal)
    } else {
        Endpoint::builder(presets::N0)
    };
    let mut alpns = vec![proto::ALPN.to_vec()];
    if cfg.mycelium {
        alpns.push(mycelium::ALPN.to_vec());
        alpns.push(iroh_gossip::ALPN.to_vec());
    }
    let endpoint = builder
        .secret_key(secret.clone())
        .alpns(alpns)
        .relay_mode(relay_mode)
        .bind_addr(cfg.bind)
        .context("bind addr")?
        .bind()
        .await
        .context("bind endpoint")?;

    let metrics = Arc::new(Metrics::default());
    let limiter = Arc::new(Limiter::new(cfg.limits));
    // egress (v0.4.0): проба исходящего IPv6 до приёма первых стримов, далее раз в 10 мин (auto)
    let egress = Arc::new(Egress::new(cfg.ip_mode, cfg.egress_v6_prefix).with_v6_per(cfg.egress_v6_per, secret.clone()));
    let v6_now = egress.probe().await;
    tracing::info!(mode = %cfg.ip_mode, ipv6 = v6_now, prefix = %cfg.egress_v6_prefix.map(|p| p.to_string()).unwrap_or_else(|| "-".into()), per = %cfg.egress_v6_per, "egress");
    let reprobe = egress.spawn_reprobe();
    let handler = PqWeb {
        cfg: Arc::new(cfg.clone()),
        metrics: metrics.clone(),
        limiter: limiter.clone(),
        egress: egress.clone(),
    };
    let mut router_b = Router::builder(endpoint.clone()).accept(proto::ALPN, handler);
    // Mycelium (v0.6.0): релей mushroom-цепочек NETFORY на том же узле — Guard/Middle/Exit,
    // политика выхода по умолчанию (только Netfory-ALPN), директория по gossip.
    let mut my: Option<mycelium::Mycelium> = None;
    let mut peers_loop = None;
    if cfg.mycelium {
        let gossip = iroh_gossip::net::Gossip::builder().spawn(endpoint.clone());
        let boot: Vec<iroh::EndpointId> = cfg.mycelium_bootstrap.iter().filter_map(|s| s.trim().parse().ok()).collect();
        let mcfg = mycelium::Config {
            hops: 3,
            relay: true,
            pool_size: 0,
            bootstrap: boot,
            exit_policy: mycelium::ExitPolicy::default(),
            limits: mycelium::RelayLimits { bandwidth: cfg.mycelium_bandwidth, max_circuits: cfg.mycelium_max_circuits },
            data_dir: std::env::temp_dir().join("pq-web-mycelium"),
            hidden: false,
            intro_points: 0,
            cover_traffic: false,
        };
        let m = mycelium::Mycelium::start(endpoint.clone(), Some(gossip.clone()), mcfg, Vec::new()).await.context("mycelium start")?;
        router_b = router_b.accept(mycelium::ALPN, m.handler()).accept(iroh_gossip::ALPN, gossip);
        tracing::info!(bandwidth = cfg.mycelium_bandwidth, max_circuits = cfg.mycelium_max_circuits, bootstrap = cfg.mycelium_bootstrap.len(), "mycelium: relay ON");
        if !cfg.trackers.is_empty() {
            peers_loop = Some(spawn_mycelium_peers(m.clone(), cfg.trackers.clone(), endpoint.id().to_string()));
        }
        my = Some(m);
    }
    let router = router_b.spawn();

    // GC неактивных клиентов
    {
        let lim = limiter.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(300)).await;
                lim.gc(Duration::from_secs(1800));
            }
        });
    }
    if let Some(mb) = cfg.metrics_bind {
        let m = metrics.clone();
        let lim = limiter.clone();
        let eid = endpoint.id().to_string();
        let extra: Arc<dyn Fn() -> (usize, String) + Send + Sync> = Arc::new(move || (lim.clients_online(), eid.clone()));
        tokio::spawn(async move {
            if let Err(e) = metrics::serve_http(mb, m, extra).await {
                tracing::error!("metrics http: {e}");
            }
        });
    }
    // саморегистрация на трекерах (v0.2.0): подписанный heartbeat, клиенты найдут узел сами
    let announcer = if cfg.trackers.is_empty() {
        tracing::info!("announce: off (no trackers)");
        None
    } else {
        tracing::info!("announce: {} tracker(s), every {:?}", cfg.trackers.len(), cfg.announce_interval);
        Some(announce::spawn(endpoint.clone(), secret, cfg.trackers.clone(), cfg.region.clone(), cfg.ports.clone(), cfg.public_addr, cfg.allow_all, egress.ipv6_flag(), cfg.mycelium, cfg.announce_interval))
    };
    Ok(Server { endpoint, router, metrics, limiter, egress, mycelium: my, announcer, reprobe, peers_loop })
}

/// Соседние релеи Mycelium с трекеров (узлы pq-web с `mycelium:true`): раз в 5 минут
/// добавляем в bootstrap/директорию — релеи NETFORY находят друг друга без ручного ввода.
/// Раз в период — сводка релея в лог.
fn spawn_mycelium_peers(my: mycelium::Mycelium, trackers: Vec<String>, me: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut known: Vec<String> = Vec::new();
        loop {
            let (t, m) = (trackers.clone(), me.clone());
            let peers = tokio::task::spawn_blocking(move || announce::mycelium_peers(&t, &m)).await.unwrap_or_default();
            let fresh: Vec<String> = peers.into_iter().filter(|p| !known.contains(p)).collect();
            if !fresh.is_empty() {
                let ids: Vec<iroh::EndpointId> = fresh.iter().filter_map(|s| s.parse().ok()).collect();
                tracing::info!(n = ids.len(), "mycelium: NETFORY relays from tracker → bootstrap");
                my.add_bootstrap(ids);
                known.extend(fresh);
            }
            let st = my.status();
            tracing::info!(relays = st.relays_known, active = st.relay_circuits_active, total = st.relay_circuits_total, cells = st.relay_cells, bytes = st.relay_bytes, exits = st.exit_streams, intro = st.relay_intro_active, rendezvous = st.relay_rendezvous_total, "mycelium: relay stats");
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    })
}

#[derive(Debug, Clone)]
struct PqWeb {
    cfg: Arc<Config>,
    metrics: Arc<Metrics>,
    limiter: Arc<Limiter>,
    egress: Arc<Egress>,
}

/// Компактная сводка путей соединения: `[*]relay:https://…/ 48ms | ip:1.2.3.4:7843 12ms` (`*` = выбранный).
fn paths_summary(conn: &Connection) -> String {
    let paths = conn.paths();
    let mut parts = Vec::new();
    for p in paths.iter() {
        parts.push(format!(
            "{}{}:{:?} {}ms",
            if p.is_selected() { "*" } else { "" },
            if p.is_relay() { "relay" } else { "ip" },
            p.remote_addr(),
            p.rtt().as_millis()
        ));
    }
    if parts.is_empty() { "no paths".into() } else { parts.join(" | ") }
}

impl ProtocolHandler for PqWeb {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let peer = connection.remote_id();
        self.metrics.inc(&self.metrics.connections);
        tracing::info!(%peer, paths = %paths_summary(&connection), "connection");
        let slot = self.limiter.slot(peer);
        loop {
            let (send, recv) = match connection.accept_bi().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::info!(%peer, paths = %paths_summary(&connection), "connection closed: {e}");
                    break;
                }
            };
            // диагностика пути: relay/direct, selected, rtt — на каждом новом стриме
            tracing::info!(%peer, paths = %paths_summary(&connection), "stream");
            let this = self.clone();
            let slot = slot.clone();
            tokio::spawn(async move {
                if let Err(e) = this.handle_stream(peer, slot, send, recv).await {
                    tracing::info!(%peer, "stream error: {e}");
                }
            });
        }
        Ok(())
    }
}

impl PqWeb {
    async fn handle_stream(&self, peer: iroh::EndpointId, slot: Arc<ClientSlot>, mut send: SendStream, mut recv: RecvStream) -> Result<()> {
        let line = match tokio::time::timeout(Duration::from_secs(10), read_header(&mut recv)).await {
            Ok(Ok(l)) => l,
            Ok(Err(e)) => {
                tracing::info!(%peer, "bad header: {e}");
                return reject(&mut send, ErrCode::BadRequest).await;
            }
            Err(_) => {
                tracing::info!(%peer, "header timeout (10 s, no bytes from client)");
                return reject(&mut send, ErrCode::Timeout).await;
            }
        };
        let req = match proto::parse(&line) {
            Ok(r) => r,
            Err(code) => {
                tracing::info!(%peer, %line, "bad request");
                return reject(&mut send, code).await;
            }
        };
        match req {
            Request::Ping => {
                self.metrics.inc(&self.metrics.pings);
                let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                send.write_all(format!("PONG {ms}\n").as_bytes()).await?;
                let _ = send.finish();
                // ждём подтверждения доставки: если за 5 с нет ACK — путь сервер→клиент не работает
                match tokio::time::timeout(Duration::from_secs(5), send.stopped()).await {
                    Ok(_) => tracing::info!(%peer, "ping → pong (delivered)"),
                    Err(_) => tracing::warn!(%peer, "ping → pong NOT acked in 5 s — server→client path broken (see paths)"),
                }
                Ok(())
            }
            Request::Info => {
                let info = serde_json::json!({
                    "service": "pq-web",
                    "version": env!("CARGO_PKG_VERSION"),
                    "proto": String::from_utf8_lossy(proto::ALPN),
                    "region": self.cfg.region,
                    "bundles": self.cfg.bundles.names(),
                    "ports": self.cfg.ports,
                    "maxStreams": self.cfg.limits.max_streams_per_client,
                    "kbps": self.cfg.limits.kbps_per_client,
                    "open": self.cfg.allow_all,
                    "ipMode": self.cfg.ip_mode.as_str(),
                    "ipv6": self.egress.ipv6(),
                    "v6Prefix": self.cfg.egress_v6_prefix.is_some(),
                    "v6Per": self.cfg.egress_v6_per.as_str(),
                });
                send.write_all(format!("OK {info}\n").as_bytes()).await?;
                let _ = send.finish();
                Ok(())
            }
            Request::Bench(n) => {
                self.metrics.inc(&self.metrics.benches);
                send.write_all(format!("OK {n}\n").as_bytes()).await?;
                let zeros = vec![0u8; 64 * 1024];
                let mut left = n;
                while left > 0 {
                    let k = (zeros.len() as u64).min(left) as usize;
                    send.write_all(&zeros[..k]).await?;
                    left -= k as u64;
                }
                let _ = send.finish();
                Ok(())
            }
            Request::Connect { host, port } => self.connect(peer, slot, host, port, send, recv).await,
        }
    }

    async fn connect(&self, peer: iroh::EndpointId, slot: Arc<ClientSlot>, host: String, port: u16, mut send: SendStream, recv: RecvStream) -> Result<()> {
        if !self.cfg.ports.contains(&port) {
            self.metrics.inc(&self.metrics.rejected_not_allowed);
            return reject(&mut send, ErrCode::NotAllowed).await;
        }
        let bundle = if self.cfg.allow_all {
            "*".to_string()
        } else {
            match self.cfg.bundles.match_host(&host) {
                Some(b) => b.to_string(),
                None => {
                    self.metrics.inc(&self.metrics.rejected_not_allowed);
                    tracing::debug!(%peer, %host, "not allowed");
                    return reject(&mut send, ErrCode::NotAllowed).await;
                }
            }
        };
        // лимиты: на клиента и общий
        let Ok(_client_permit) = slot.streams.clone().try_acquire_owned() else {
            self.metrics.inc(&self.metrics.rejected_limit);
            return reject(&mut send, ErrCode::TooManyStreams).await;
        };
        let Ok(_total_permit) = self.limiter.total.clone().try_acquire_owned() else {
            self.metrics.inc(&self.metrics.rejected_limit);
            return reject(&mut send, ErrCode::Busy).await;
        };

        let tcp = match tokio::time::timeout(self.cfg.connect_timeout, dial(&host, port, &self.egress, peer.as_bytes())).await {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                self.metrics.inc(&self.metrics.connect_failed);
                tracing::debug!(%peer, %host, "connect failed: {e}");
                return reject(&mut send, ErrCode::ConnectFailed).await;
            }
            Err(_) => {
                self.metrics.inc(&self.metrics.connect_failed);
                return reject(&mut send, ErrCode::Timeout).await;
            }
        };
        let _ = tcp.set_nodelay(true);
        let fam = if tcp.peer_addr().map(|a| a.is_ipv6()).unwrap_or(false) { "v6" } else { "v4" };
        // v0.5.0: семейство в ответе — клиент показывает бейдж v4/v6; старые клиенты читают префикс «OK»
        send.write_all(format!("OK {fam}\n").as_bytes()).await?;
        self.metrics.inc(&self.metrics.streams_total);
        self.metrics.inc(&self.metrics.streams_active);
        let src = tcp.local_addr().map(|a| a.ip().to_string()).unwrap_or_default();
        tracing::info!(%peer, %host, port, %bundle, fam, %src, "tunnel open");

        let (up, down) = pipe(recv, send, tcp, slot.clone(), self.cfg.idle).await;
        self.metrics.add(&self.metrics.bytes_up, up);
        self.metrics.add(&self.metrics.bytes_down, down);
        self.metrics.dec(&self.metrics.streams_active);
        slot.add_bytes(up + down);
        tracing::info!(%peer, %host, up, down, "tunnel closed");
        Ok(())
    }
}

async fn reject(send: &mut SendStream, code: ErrCode) -> Result<()> {
    let _ = send.write_all(code.line().as_bytes()).await;
    let _ = send.finish();
    Ok(())
}

/// Первая строка стрима (≤ MAX_HEADER байт, до `\n`).
async fn read_header(recv: &mut RecvStream) -> Result<String> {
    let mut buf = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    loop {
        // именно AsyncReadExt::read — у noq::RecvStream есть свой read() с Option<usize>
        let n = AsyncReadExt::read(recv, &mut byte).await?;
        if n == 0 {
            anyhow::bail!("eof before header");
        }
        if byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
        if buf.len() > proto::MAX_HEADER {
            anyhow::bail!("header too long");
        }
    }
    Ok(String::from_utf8(buf)?)
}

fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_documentation()
                // CGNAT 100.64.0.0/10 и benchmarking 198.18.0.0/15 (частые DNS-заглушки)
                || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]))
                || (v4.octets()[0] == 198 && (18..=19).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped (::ffff:a.b.c.d) и IPv4-compatible — проверяем как v4 (SSRF через AAAA)
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_forbidden_ip(IpAddr::V4(v4));
            }
            let s = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || (s[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (s[0] & 0xff00) == 0xff00 // multicast ff00::/8
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation 2001:db8::/32
                || (s[0] == 0x0100 && s[1] == 0) // discard-only 100::/64
                || (s[0] == 0x2002 && is_forbidden_ip(IpAddr::V4(std::net::Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8)))) // 6to4 с приватным v4
                || (s[0] == 0x0064 && s[1] == 0xff9b && is_forbidden_ip(IpAddr::V4(std::net::Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8)))) // NAT64 64:ff9b::/96 с приватным v4
        }
    }
}

/// DNS + TCP к сайту; приватные/loopback адреса запрещены (SSRF). Семейство адресов и
/// исходящий IPv6 — по `Egress` (Happy Eyeballs между AAAA и A); `client` — EndpointId.
async fn dial(host: &str, port: u16, egress: &Egress, client: &[u8]) -> std::io::Result<TcpStream> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
    if addrs.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "dns: no addresses"));
    }
    let total = addrs.len();
    let allowed: Vec<SocketAddr> = addrs.into_iter().filter(|a| !is_forbidden_ip(a.ip())).collect();
    if allowed.is_empty() {
        tracing::debug!(%host, total, "all resolved addresses forbidden (SSRF)");
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "forbidden address"));
    }
    egress.dial(allowed, Some(client)).await
}

/// Двунаправленная прокачка QUIC-стрим ↔ TCP с лимитом полосы и таймаутом простоя.
/// Возвращает (байт от клиента к сайту, байт от сайта к клиенту).
async fn pipe(mut recv: RecvStream, mut send: SendStream, tcp: TcpStream, slot: Arc<ClientSlot>, idle: Duration) -> (u64, u64) {
    let (mut tcp_r, mut tcp_w) = tcp.into_split();
    let bucket_a = slot.bucket.clone();
    let bucket_b = slot.bucket.clone();

    let up = tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = match tokio::time::timeout(idle, AsyncReadExt::read(&mut recv, &mut buf)).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                Ok(Ok(n)) => n,
            };
            if let Some(b) = &bucket_a {
                let wait = b.take(n);
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
            }
            if tcp_w.write_all(&buf[..n]).await.is_err() {
                break;
            }
            total += n as u64;
        }
        let _ = tcp_w.shutdown().await;
        total
    });
    let down = tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = match tokio::time::timeout(idle, tcp_r.read(&mut buf)).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                Ok(Ok(n)) => n,
            };
            if let Some(b) = &bucket_b {
                let wait = b.take(n);
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
            }
            if send.write_all(&buf[..n]).await.is_err() {
                break;
            }
            total += n as u64;
        }
        let _ = send.finish();
        total
    });
    let up = up.await.unwrap_or(0);
    let down = down.await.unwrap_or(0);
    (up, down)
}

/// Активных стримов сейчас (для CLI-вывода/healthcheck).
pub fn active_streams(m: &Metrics) -> u64 {
    m.streams_active.load(Ordering::Relaxed)
}
