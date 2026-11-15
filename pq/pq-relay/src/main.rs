//! SmartNet Relay (n1-relay) — self-hosted iroh relay for the SmartHoldem / NETFORY network.
//!
//! Три режима, выбираются переменными окружения:
//!
//!   A. `PQ_RELAY_DOMAIN` задан — релей сам обслуживает HTTPS + WebSocket на 443,
//!      получает/продлевает сертификат Let's Encrypt (ACME TLS-ALPN-01, порт 80 не нужен)
//!      и поднимает QUIC Address Discovery на `PQ_RELAY_QUIC_PORT` (7842/udp) с тем же
//!      сертификатом. Без nginx и certbot.
//!
//!   B. `PQ_RELAY_TLS_CERT` + `PQ_RELAY_TLS_KEY` заданы (домен — нет) — HTTP-релей
//!      остаётся за вашим reverse-proxy (nginx терминирует TLS на 443), а QUIC/QAD на 7842/udp
//!      релей поднимает сам, читая PEM-файлы certbot. После продления сертификата нужен
//!      рестарт (`--deploy-hook "systemctl restart pq-relay"`).
//!
//!   C. ничего не задано — старый режим: только plain HTTP на `PQ_RELAY_PORT` (3340),
//!      QUIC/QAD нет. Поведение прежних релеев сохранено 1:1 — обновлять можно по одному.
//!
//! Почему QUIC обязателен: RelayMap sth-core и NETFORY-клиентов ждут на релее 7842/udp для
//! QAD — так узел за NAT узнаёт свой внешний ip:port (замена STUN). Без него каждый net_report
//! получает таймаут проб, а после отключения n0 узлы за NAT теряют hole punching.
//!
//! Env:
//!   PQ_RELAY_DOMAIN        relay-fsn7.sth.cx          публичное имя (A/AAAA уже указывают сюда)
//!   PQ_RELAY_ACME_CONTACT  mailto:ops@smartholdem.io  контакт Let's Encrypt
//!   PQ_RELAY_ACME_CACHE    /var/lib/pq-relay/acme   аккаунт + сертификат
//!   PQ_RELAY_ACME_STAGING  1                          staging-CA LE (без лимитов, недоверенный серт)
//!   PQ_RELAY_HTTPS_PORT    443
//!   PQ_RELAY_QUIC_PORT     7842                       0 = выключить QUIC/QAD
//!   PQ_RELAY_TLS_CERT      /etc/letsencrypt/live/<host>/fullchain.pem   (режим B)
//!   PQ_RELAY_TLS_KEY       /etc/letsencrypt/live/<host>/privkey.pem     (режим B)
//!   PQ_RELAY_HOST          0.0.0.0
//!   PQ_RELAY_PORT          3340 (режимы B/C) / 80 (режим A: plain-HTTP, health-check)
//!   RUST_LOG                     pq_relay=info,iroh_relay=info
//!
//! Author: TechnoL0g / 2026-09-24

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use iroh_relay::server::{
    AcmeConfig, CertConfig, QuicConfig, RelayConfig, Server, ServerConfig, TlsConfig,
};

type AnyErr = Box<dyn std::error::Error>;

/// Переменные `PQ_RELAY_*`; для уже развёрнутых узлов принимается и старый
/// префикс `SMARTNET_RELAY_*` (утилита переименована smartnet-relay → pq-relay).
fn env(name: &str) -> Option<String> {
    let read = |n: &str| {
        std::env::var(n)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    read(name).or_else(|| name.strip_prefix("PQ_RELAY_").and_then(|rest| read(&format!("SMARTNET_RELAY_{rest}"))))
}

fn env_port(name: &str, default: u16) -> u16 {
    env(name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// rustls-провайдер: ring, только TLS 1.3 (обязателен для QUIC).
fn tls13_builder(
) -> Result<rustls::ConfigBuilder<rustls::ServerConfig, rustls::server::WantsServerCert>, AnyErr> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth())
}

/// Режим B: ServerConfig из PEM-файлов certbot (fullchain + privkey).
fn tls_from_pem(cert: &str, key: &str) -> Result<rustls::ServerConfig, AnyErr> {
    let certs = rustls_pemfile::certs(&mut std::io::BufReader::new(std::fs::File::open(cert)?))
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(std::fs::File::open(key)?))?
        .ok_or_else(|| format!("no private key in {key}"))?;
    let mut cfg = tls13_builder()?.with_single_cert(certs, key)?;
    cfg.alpn_protocols = vec![b"h3".to_vec()];
    Ok(cfg)
}

/// Поднять soft-лимит открытых файлов до hard (каждый клиент релея = ≥1 fd; при
/// дефолтных 1024 релей упирается в EMFILE и «зависает»). Возвращает (soft, hard) после.
fn raise_nofile() -> Option<(u64, u64)> {
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return None;
    }
    if lim.rlim_cur < lim.rlim_max {
        let want = libc::rlimit { rlim_cur: lim.rlim_max, rlim_max: lim.rlim_max };
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &want) } == 0 {
            lim = want;
        }
    }
    Some((lim.rlim_cur as u64, lim.rlim_max as u64))
}

/// Сколько fd открыто сейчас (Linux: /proc/self/fd).
fn open_fds() -> Option<usize> {
    std::fs::read_dir("/proc/self/fd").ok().map(|d| d.count())
}

#[tokio::main]
async fn main() -> Result<(), AnyErr> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pq_relay=info,iroh_relay=info".into()),
        )
        .init();

    // Лимит fd: поднимаем до hard и следим. Ниже 8192 — предупреждение: задайте
    // `LimitNOFILE=65535` в unit-файле systemd (см. deploy/*.service).
    let nofile = raise_nofile();
    match nofile {
        Some((soft, hard)) if soft < 8192 => tracing::warn!(soft, hard, "RLIMIT_NOFILE is low — set LimitNOFILE=65535 in the systemd unit"),
        Some((soft, hard)) => tracing::info!(soft, hard, "RLIMIT_NOFILE"),
        None => tracing::warn!("RLIMIT_NOFILE: cannot read"),
    }
    if let Some((soft, _)) = nofile {
        tokio::spawn(async move {
            let mut high = false;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let Some(n) = open_fds() else { continue };
                let pct = n as u64 * 100 / soft.max(1);
                if pct >= 80 && !high {
                    tracing::warn!(open = n, limit = soft, pct, "open file descriptors above 80% of the limit");
                    high = true;
                } else if pct < 60 && high {
                    tracing::info!(open = n, limit = soft, pct, "open file descriptors back to normal");
                    high = false;
                } else {
                    tracing::debug!(open = n, limit = soft, pct, "fd usage");
                }
            }
        });
    }

    let host = env("PQ_RELAY_HOST").unwrap_or_else(|| "0.0.0.0".into());
    let bind = |port: u16| -> Result<SocketAddr, AnyErr> { Ok(format!("{host}:{port}").parse()?) };
    let quic_port = env_port("PQ_RELAY_QUIC_PORT", 7842);

    let mut config = ServerConfig::default();

    match (env("PQ_RELAY_DOMAIN"), env("PQ_RELAY_TLS_CERT"), env("PQ_RELAY_TLS_KEY")) {
        // ---------- A: TLS + ACME + QUIC — всё в релее ----------
        (Some(domain), _, _) => {
            let https_port = env_port("PQ_RELAY_HTTPS_PORT", 443);
            let http_port = env_port("PQ_RELAY_PORT", 80);
            let staging = env("PQ_RELAY_ACME_STAGING").is_some();
            let contact =
                env("PQ_RELAY_ACME_CONTACT").unwrap_or_else(|| "mailto:ops@smartholdem.io".into());
            let cache = PathBuf::from(
                env("PQ_RELAY_ACME_CACHE").unwrap_or_else(|| "/var/lib/pq-relay/acme".into()),
            );
            std::fs::create_dir_all(&cache)?;

            let acme = AcmeConfig::letsencrypt(!staging)
                .domains(vec![domain.clone()])
                .contact(vec![contact])
                .cache_path(cache);

            let mut relay = RelayConfig::new(bind(http_port)?);
            relay.tls = Some(TlsConfig::new(
                bind(https_port)?,
                CertConfig::LetsEncrypt { acme_config: acme, server_config_builder: tls13_builder()? },
            ));
            config.relay = Some(relay);

            // QUIC/QAD: server_config = None → тот же ACME-сертификат, что у HTTPS.
            if quic_port > 0 {
                config.quic = Some(QuicConfig::new(bind(quic_port)?));
            }

            tracing::info!(domain, https_port, quic_port, staging, "mode A: TLS (ACME TLS-ALPN-01) + QUIC in relay");
            if staging {
                tracing::warn!("Let's Encrypt STAGING: сертификат недоверенный — уберите PQ_RELAY_ACME_STAGING для прода");
            }
        }
        // ---------- B: HTTP за nginx + QUIC с PEM certbot ----------
        (None, Some(cert), Some(key)) => {
            let port = env_port("PQ_RELAY_PORT", 3340);
            config.relay = Some(RelayConfig::new(bind(port)?));
            if quic_port > 0 {
                let mut quic = QuicConfig::new(bind(quic_port)?);
                quic.server_config = Some(tls_from_pem(&cert, &key)?);
                config.quic = Some(quic);
            }
            tracing::info!(port, quic_port, cert, "mode B: plain HTTP behind proxy + QUIC with certbot PEM (restart after renewal)");
        }
        // ---------- C: legacy — plain HTTP, без QUIC ----------
        _ => {
            let port = env_port("PQ_RELAY_PORT", 3340);
            config.relay = Some(RelayConfig::new(bind(port)?));
            tracing::warn!(port, "mode C: plain HTTP only — NO QUIC/QAD; NAT peers cannot learn their external address via this relay");
        }
    }

    let server = Server::spawn(config).await?;
    if let Some(addr) = server.https_addr() {
        println!("SmartNet relay (n1-relay) HTTPS on {addr}");
    }
    if let Some(addr) = server.http_addr() {
        println!("SmartNet relay (n1-relay) HTTP  on {addr}");
    }
    if let Some(addr) = server.quic_addr() {
        println!("SmartNet relay (n1-relay) QUIC  on {addr} (address discovery)");
    }

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down SmartNet relay…");
    server.shutdown().await?;
    Ok(())
}
