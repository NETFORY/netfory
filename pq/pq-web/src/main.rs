//! pq-web — консольная утилита: egress-туннель NETFORY для https://-сайтов поверх Iroh QUIC.
//!
//!   pq-web serve --key /var/lib/pq-web/key.hex --allow bundles.json --relay https://relay.example
//!   pq-web serve --region NL --tracker http://159.69.188.60:7374   # саморегистрация на трекере
//!   pq-web id --key /var/lib/pq-web/key.hex        # показать EndpointId узла
//!   pq-web check youtube.com --allow bundles.json  # проверить allowlist
//!   pq-web bundles-example > bundles.json          # стартовый набор youtube/x/telegram
//!   pq-web serve --open --ip-mode prefer6 --egress-v6-prefix 2a01:db8:1:2::/64   # IPv6-egress, свой адрес на клиента
//!   pq-web serve --open --egress-v6-prefix 2a01:db8:1:2::/64 --egress-v6-per conn  # случайный адрес на соединение
//!   pq-web serve --open --mycelium                  # + релей Mycelium (mushroom-цепочки NETFORY) на том же узле
//!
//! Все опции доступны и как переменные окружения `PQ_WEB_*` (для systemd EnvironmentFile).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pq_web::allow::Bundles;
use pq_web::egress::{IpMode, V6Per, V6Prefix};
use pq_web::limits::LimitConfig;
use pq_web::Config;

#[derive(Parser, Debug)]
#[command(
    name = "pq-web",
    version,
    about = "PQ Web — NETFORY egress tunnel for https:// sites over Iroh QUIC (CONNECT, no MITM)",
    long_about = None,
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Запустить узел (по умолчанию, если подкоманда не указана)
    Serve(ServeArgs),
    /// Показать EndpointId (публичный ключ) узла для файла ключа
    Id {
        #[arg(long, env = "PQ_WEB_KEY", default_value = "pq-web.key")]
        key: PathBuf,
    },
    /// Проверить, разрешён ли хост allowlist'ом
    Check {
        host: String,
        #[arg(long, env = "PQ_WEB_ALLOW")]
        allow: Option<PathBuf>,
    },
    /// Напечатать стартовый bundles.json (youtube / x / telegram)
    BundlesExample,
}

#[derive(clap::Args, Debug, Clone)]
struct ServeArgs {
    /// UDP-адрес QUIC-сокета
    #[arg(long, env = "PQ_WEB_BIND", default_value = "0.0.0.0:7843")]
    bind: SocketAddr,

    /// Файл секретного ключа узла (hex, 32 байта); создаётся, если отсутствует
    #[arg(long, env = "PQ_WEB_KEY", default_value = "pq-web.key")]
    key: PathBuf,

    /// bundles.json (allowlist доменов). Без файла — встроенный набор youtube/x/telegram
    #[arg(long, env = "PQ_WEB_ALLOW")]
    allow: Option<PathBuf>,

    /// dVPN-режим: пропускать ЛЮБЫЕ https-хосты (порты по-прежнему ограничены --ports).
    /// Узел анонсируется трекерам как `open` — клиенты в режиме «все сайты через pq-web»
    /// выбирают только такие узлы. Алиас: --allow-all
    #[arg(long, alias = "allow-all", env = "PQ_WEB_OPEN", default_value_t = false)]
    open: bool,

    /// Relay-сервер (home relay); можно повторять. Пусто — реле NETFORY
    /// (relay-fsn7/relay-ru1.sth.cx, как у клиентов); `--relay n0` — публичные n0
    #[arg(long = "relay", env = "PQ_WEB_RELAYS", value_delimiter = ',')]
    relays: Vec<String>,

    /// Отключить DNS/pkarr-discovery (клиенты должны знать relay/адрес узла)
    #[arg(long, env = "PQ_WEB_NO_DISCOVERY", default_value_t = false)]
    no_discovery: bool,

    /// Разрешённые порты назначения (через запятую)
    #[arg(long, env = "PQ_WEB_PORTS", value_delimiter = ',', default_value = "443")]
    ports: Vec<u16>,

    /// Одновременных стримов на клиента
    #[arg(long, env = "PQ_WEB_MAX_STREAMS", default_value_t = 64)]
    max_streams: usize,

    /// Общий потолок одновременных стримов
    #[arg(long, env = "PQ_WEB_MAX_STREAMS_TOTAL", default_value_t = 4096)]
    max_streams_total: usize,

    /// Полоса на клиента, КБит/с (0 = без лимита)
    #[arg(long, env = "PQ_WEB_KBPS", default_value_t = 20_000)]
    kbps: u64,

    /// Таймаут простоя стрима, секунд
    #[arg(long, env = "PQ_WEB_IDLE_SECS", default_value_t = 90)]
    idle_secs: u64,

    /// Таймаут TCP-коннекта к сайту, секунд
    #[arg(long, env = "PQ_WEB_CONNECT_SECS", default_value_t = 8)]
    connect_secs: u64,

    /// Метка региона узла (например NL) — отдаётся в INFO
    #[arg(long, env = "PQ_WEB_REGION", default_value = "")]
    region: String,

    /// Адрес мини-HTTP метрик (/metrics, /healthz). Пусто — выключено
    #[arg(long, env = "PQ_WEB_METRICS")]
    metrics: Option<SocketAddr>,

    /// Трекер NETFORY для саморегистрации (можно повторять / через запятую).
    /// По умолчанию — публичные трекеры NETFORY
    #[arg(long = "tracker", env = "PQ_WEB_TRACKERS", value_delimiter = ',')]
    trackers: Vec<String>,

    /// Не анонсироваться на трекерах (клиенты вводят EndpointId вручную)
    #[arg(long, env = "PQ_WEB_NO_ANNOUNCE", default_value_t = false)]
    no_announce: bool,

    /// Период heartbeat'а на трекеры, секунд
    #[arg(long, env = "PQ_WEB_ANNOUNCE_SECS", default_value_t = 60)]
    announce_secs: u64,

    /// Публичный ip:port узла (если за NAT с проброшенным портом); иначе определяется iroh
    #[arg(long, env = "PQ_WEB_PUBLIC_ADDR")]
    public_addr: Option<SocketAddr>,

    /// Семейство адресов для TCP к сайтам: auto (проверить исходящий IPv6 и, если работает,
    /// предпочитать AAAA; Happy Eyeballs с откатом на A), prefer6, prefer4, only6, only4
    #[arg(long, env = "PQ_WEB_IP_MODE", default_value = "auto")]
    ip_mode: IpMode,

    /// IPv6-префикс исходящих адресов, например 2a01:4f8:c013:ab91::/64 (на хосте: sysctl
    /// net.ipv6.ip_nonlocal_bind=1 и `ip -6 route add local <prefix> dev lo`)
    #[arg(long, env = "PQ_WEB_EGRESS_V6_PREFIX")]
    egress_v6_prefix: Option<V6Prefix>,

    /// Как выдавать IPv6 из префикса: client — стабильный адрес на клиента (EndpointId),
    /// conn — случайный на каждое соединение
    #[arg(long, env = "PQ_WEB_EGRESS_V6_PER", default_value = "client")]
    egress_v6_per: V6Per,

    /// Релей Mycelium (NIP-1, v0.6.0): принимать mushroom-цепочки NETFORY (Guard/Middle/Exit) на том же
    /// EndpointId — ALPN `netfory/mycelium/1` + gossip-директория. Узел анонсируется трекерам как
    /// `mycelium`, клиенты NETFORY берут его в bootstrap автоматически
    #[arg(long, env = "PQ_WEB_MYCELIUM", default_value_t = false)]
    mycelium: bool,

    /// Лимит канала релея Mycelium, КБ/с (0 = без лимита)
    #[arg(long, env = "PQ_WEB_MYCELIUM_KBPS", default_value_t = 4096)]
    mycelium_kbps: u64,

    /// Одновременных mushroom-цепочек через этот релей
    #[arg(long, env = "PQ_WEB_MYCELIUM_MAX_CIRCUITS", default_value_t = 256)]
    mycelium_max_circuits: u32,

    /// Соседние релеи Mycelium (EndpointId, через запятую) для входа в директорию;
    /// остальные релеи NETFORY подхватываются с трекеров сами
    #[arg(long = "mycelium-bootstrap", env = "PQ_WEB_MYCELIUM_BOOTSTRAP", value_delimiter = ',')]
    mycelium_bootstrap: Vec<String>,

    /// Подробный лог (эквивалент RUST_LOG=pq_web=debug)
    #[arg(short, long, default_value_t = false)]
    verbose: bool,
}

fn load_bundles(path: &Option<PathBuf>) -> Result<Bundles> {
    match path {
        Some(p) => {
            let txt = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
            Bundles::from_json(&txt).with_context(|| format!("parse {}", p.display()))
        }
        None => Ok(Bundles::example()),
    }
}

fn init_logging(verbose: bool) {
    use tracing_subscriber::EnvFilter;
    let default = if verbose { "pq_web=debug,iroh=info" } else { "pq_web=info,iroh=warn" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
}

async fn run_serve(a: ServeArgs) -> Result<()> {
    init_logging(a.verbose);
    let key = pq_web::load_or_create_key(&a.key)?;
    let bundles = load_bundles(&a.allow)?;
    if a.open {
        tracing::info!("--open: dVPN-режим — любой https-хост на портах {:?} (bundles не ограничивают)", a.ports);
    }
    let cfg = Config {
        bind: a.bind,
        secret_key: Some(key),
        relays: a.relays.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        no_discovery: a.no_discovery,
        bundles,
        allow_all: a.open,
        ports: a.ports.clone(),
        limits: LimitConfig {
            max_streams_per_client: a.max_streams,
            kbps_per_client: a.kbps,
            max_streams_total: a.max_streams_total,
        },
        idle: Duration::from_secs(a.idle_secs),
        connect_timeout: Duration::from_secs(a.connect_secs),
        region: a.region.clone(),
        metrics_bind: a.metrics,
        trackers: if a.no_announce {
            Vec::new()
        } else if a.trackers.iter().any(|t| !t.trim().is_empty()) {
            a.trackers.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
        } else {
            pq_web::announce::DEFAULT_TRACKERS.iter().map(|s| s.to_string()).collect()
        },
        announce_interval: Duration::from_secs(a.announce_secs.max(15)),
        public_addr: a.public_addr,
        ip_mode: a.ip_mode,
        egress_v6_prefix: a.egress_v6_prefix,
        egress_v6_per: a.egress_v6_per,
        mycelium: a.mycelium,
        mycelium_bandwidth: a.mycelium_kbps * 1024,
        mycelium_max_circuits: a.mycelium_max_circuits.max(1),
        mycelium_bootstrap: a.mycelium_bootstrap.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
    };
    let names = cfg.bundles.names();
    let server = pq_web::serve(cfg).await?;
    println!("pq-web {} · EndpointId: {}", env!("CARGO_PKG_VERSION"), server.id());
    println!("bind: {} · bundles: {} · ports: {:?} · region: {}", a.bind, names.join(","), a.ports, if a.region.is_empty() { "-" } else { &a.region });
    println!("relays: {}", if a.relays.is_empty() { pq_web::DEFAULT_RELAYS.join(",") } else { a.relays.join(",") });
    println!("mode: {}", if a.open { "OPEN — any https host (dVPN)" } else { "bundles only (youtube/x/telegram/iptest) — add --open for dVPN" });
    println!(
        "egress: ip-mode={} · ipv6 {} · v6-prefix {} · v6-per {}",
        a.ip_mode,
        if server.egress.ipv6() { "ON (AAAA first, Happy Eyeballs)" } else { "off (A first)" },
        a.egress_v6_prefix.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
        if a.egress_v6_prefix.is_some() { a.egress_v6_per.as_str() } else { "-" }
    );
    match &server.mycelium {
        Some(m) => println!(
            "mycelium: relay ON — {} KB/s · max {} circuits · bootstrap {} · relays known {} (NETFORY clients add this node to Mycelium bootstrap via tracker)",
            a.mycelium_kbps,
            a.mycelium_max_circuits.max(1),
            a.mycelium_bootstrap.len(),
            m.status().relays_known
        ),
        None => println!("mycelium: off — add --mycelium to relay NETFORY mushroom circuits"),
    }
    if a.no_announce {
        println!("announce: off (--no-announce)");
    } else {
        println!("announce: {} tracker(s), every {}s — clients discover this node via GET /pqweb", if a.trackers.iter().any(|t| !t.trim().is_empty()) { a.trackers.len() } else { pq_web::announce::DEFAULT_TRACKERS.len() }, a.announce_secs.max(15));
    }
    tracing::info!("waiting for relay/online…");
    match tokio::time::timeout(Duration::from_secs(15), server.endpoint.online()).await {
        Ok(()) => {
            tracing::info!("online: {:?}", server.endpoint.addr());
            let (addrs, relay) = pq_web::announce::dial_info(&server.endpoint, a.public_addr);
            // строка для ручного ввода в NETFORY → Settings → PQ Web
            println!("dial: {} {} {} {}", server.endpoint.id(), if a.region.is_empty() { "-" } else { &a.region }, addrs.join(" "), relay);
        }
        Err(_) => tracing::warn!("relay ещё не подключён (клиенты смогут дозвониться напрямую по UDP)"),
    }
    println!("ready — Ctrl+C для остановки");
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutdown…");
    server.shutdown().await;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        None => run_serve(cli.serve).await,
        Some(Cmd::Serve(a)) => run_serve(a).await,
        Some(Cmd::Id { key }) => {
            let k = pq_web::load_or_create_key(&key)?;
            println!("{}", k.public());
            Ok(())
        }
        Some(Cmd::Check { host, allow }) => {
            let b = load_bundles(&allow)?;
            match b.match_host(&host) {
                Some(bundle) => {
                    println!("ALLOW {host} → bundle {bundle}");
                    Ok(())
                }
                None => {
                    println!("DENY  {host}");
                    std::process::exit(2);
                }
            }
        }
        Some(Cmd::BundlesExample) => {
            println!("{}", serde_json::to_string_pretty(&Bundles::example())?);
            Ok(())
        }
    }
}
