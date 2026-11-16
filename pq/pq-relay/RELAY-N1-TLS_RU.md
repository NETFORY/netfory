# n1-relay (pq-relay) без nginx: сам получает сертификат Let's Encrypt, отдаёт HTTPS/WebSocket и QUIC (QAD)

Вариант **A** из обсуждения 24.09.2026: релей слушает 443 сам, сертификат получает и продлевает встроенным ACME-клиентом
`iroh-relay` (challenge **TLS-ALPN-01** - прямо на 443, порт 80 не нужен), и тот же сертификат обслуживает QUIC-порт
**7842/udp** для QUIC Address Discovery. nginx на этом хосте больше не нужен.

## Что с доменом `relay-fsn7.sth.cx`

Домен остаётся ровно там же, где был - в DNS: A-запись (и AAAA, если есть IPv6) `relay-fsn7.sth.cx -> IP сервера`, как сейчас.
В конфиге релея домен указывается **вместо** `server_name` nginx - как имя, на которое запрашивается сертификат
(`PQ_RELAY_DOMAIN=relay-fsn7.sth.cx`). Let's Encrypt подключится к `relay-fsn7.sth.cx:443`, релей ответит на
TLS-ALPN-01 сам, положит сертификат в `PQ_RELAY_ACME_CACHE` и будет продлевать его автоматически (за ~30 дней до
истечения). Клиенты продолжают использовать `https://relay-fsn7.sth.cx` - URL не меняется, `N1_RELAYS` в sth-core и
настройки NETFORY трогать не нужно.

Условия: 443/tcp и 7842/udp открыты во внешнем фаерволе; nginx (или что-то ещё) **не** слушает 443 на этом IP; привязка к
443 - либо запуск от root, либо `AmbientCapabilities=CAP_NET_BIND_SERVICE` в systemd (ниже).

## Cargo.toml

```toml
[package]
name = "pq-relay"
version = "0.2.0"
edition = "2021"

[[bin]]
name = "pq-relay"
path = "src/main.rs"

[dependencies]
iroh-relay = { version = "1.1", features = ["server"] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std", "tls12"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "signal"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

## src/main.rs

```rust
//! SmartNet Relay (n1-relay) - self-hosted iroh relay for the SmartHoldem / NETFORY network.
//!
//! Two modes, chosen by PQ_RELAY_DOMAIN:
//!   * unset  - plain HTTP on PQ_RELAY_PORT (default 3340), TLS terminated by your reverse proxy (old mode);
//!   * set    - the relay itself serves HTTPS + WebSocket on 443, obtains/renews a Let's Encrypt certificate via
//!              ACME TLS-ALPN-01, and runs the QUIC address-discovery endpoint on PQ_RELAY_QUIC_PORT (7842/udp)
//!              with the same certificate. No nginx, no certbot.
//!
//! Env:
//!   PQ_RELAY_DOMAIN        relay-fsn7.sth.cx          public host name (DNS A/AAAA must point here)
//!   PQ_RELAY_ACME_CONTACT  mailto:ops@smartholdem.io  Let's Encrypt contact (expiry mails)
//!   PQ_RELAY_ACME_CACHE    /var/lib/pq-relay/acme   where account + certificate are stored
//!   PQ_RELAY_ACME_STAGING  1                          use the LE staging CA first (no rate limits, untrusted cert)
//!   PQ_RELAY_HTTPS_PORT    443
//!   PQ_RELAY_QUIC_PORT     7842                       0 = disable QUIC/QAD
//!   PQ_RELAY_HOST          0.0.0.0                    bind address
//!   PQ_RELAY_PORT          3340 (proxy mode) / 80 (TLS mode: plain-HTTP side, health checks)
//!   RUST_LOG                     pq_relay=info,iroh_relay=info
//!
//! Author: TechnoL0g / 2026-09-24

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use iroh_relay::server::{AcmeConfig, CertConfig, QuicConfig, RelayConfig, Server, ServerConfig, TlsConfig};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn env_port(name: &str, default: u16) -> u16 {
    env(name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pq_relay=info,iroh_relay=info".into()),
        )
        .init();

    let host = env("PQ_RELAY_HOST").unwrap_or_else(|| "0.0.0.0".into());
    let bind = |port: u16| -> Result<SocketAddr, Box<dyn std::error::Error>> { Ok(format!("{host}:{port}").parse()?) };

    let mut config = ServerConfig::default();

    match env("PQ_RELAY_DOMAIN") {
        // ---------- mode A: TLS + ACME + QUIC handled by the relay itself ----------
        Some(domain) => {
            let https_port = env_port("PQ_RELAY_HTTPS_PORT", 443);
            let http_port = env_port("PQ_RELAY_PORT", 80);
            let quic_port = env_port("PQ_RELAY_QUIC_PORT", 7842);
            let staging = env("PQ_RELAY_ACME_STAGING").is_some();
            let contact = env("PQ_RELAY_ACME_CONTACT").unwrap_or_else(|| "mailto:ops@smartholdem.io".into());
            let cache = PathBuf::from(env("PQ_RELAY_ACME_CACHE").unwrap_or_else(|| "/var/lib/pq-relay/acme".into()));
            std::fs::create_dir_all(&cache)?;

            // Let's Encrypt account + certificate for `domain`, stored in `cache`, renewed automatically.
            let acme = AcmeConfig::letsencrypt(!staging)
                .domains(vec![domain.clone()])
                .contact(vec![contact])
                .cache_path(cache);

            // rustls builder; the ACME certificate resolver is injected by iroh-relay at start. TLS 1.3 is required by QUIC.
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let server_config_builder = rustls::ServerConfig::builder_with_provider(provider)
                .with_protocol_versions(&[&rustls::version::TLS13])?
                .with_no_client_auth();

            let mut relay = RelayConfig::new(bind(http_port)?);
            relay.tls = Some(TlsConfig::new(bind(https_port)?, CertConfig::LetsEncrypt { acme_config: acme, server_config_builder }));
            config.relay = Some(relay);

            // QUIC address discovery: same certificate (server_config: None -> inherited from RelayConfig::tls).
            if quic_port > 0 {
                config.quic = Some(QuicConfig::new(bind(quic_port)?));
            }

            tracing::info!(domain, https_port, quic_port, staging, "SmartNet relay in TLS mode (ACME TLS-ALPN-01)");
            if staging {
                tracing::warn!("Let's Encrypt STAGING: the certificate is not trusted by clients - remove PQ_RELAY_ACME_STAGING for production");
            }
        }
        // ---------- legacy mode: plain HTTP behind a reverse proxy ----------
        None => {
            let port = env_port("PQ_RELAY_PORT", 3340);
            config.relay = Some(RelayConfig::new(bind(port)?));
            tracing::info!(port, "SmartNet relay in plain-HTTP mode - terminate TLS in your reverse proxy (no QUIC/QAD in this mode)");
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
```

## systemd

> **Переход со `smartnet-relay` (старое имя сервиса).** Если на сервере уже крутится `smartnet-relay.service`
> (бывшее имя `pq-relay`), он держит 443/80/7842 - новый юнит упадёт с `Failed to bind TcpListener: Address already
> in use (os error 98)`, а `https://relay-fsn7.sth.cx/` при этом продолжит открываться - это отвечает **старый** процесс.
> Порядок:
>
> ```bash
> sudo ss -ltnp 'sport = :443'                     # кто держит 443 - увидите smartnet-relay
> sudo systemctl disable --now smartnet-relay      # остановить и убрать из автозапуска старый юнит
> sudo mv /etc/systemd/system/smartnet-relay.service /root/smartnet-relay.service.bak   # (необязательно) убрать файл
> sudo systemctl daemon-reload
> # ACME-кэш и ключ релея старого сервиса можно переиспользовать - сертификат не будет выпускаться заново:
> sudo cp -a /var/lib/smartnet-relay/. /var/lib/pq-relay/ 2>/dev/null; sudo chown -R pq-relay: /var/lib/pq-relay
> sudo systemctl restart pq-relay && sudo journalctl -u pq-relay -f      # ждём "HTTPS on 0.0.0.0:443"
> ```
>
> Пользователя/каталоги старого сервиса (`smartnet-relay`, `/var/lib/smartnet-relay`, `/opt/smartnet-relay`) можно
> удалить после проверки. Если 443 держит не он, а `nginx` - см. раздел «Переход с nginx» ниже.


```ini
# /etc/systemd/system/pq-relay.service
[Unit]
Description=SmartNet Relay (n1-relay, TLS + QUIC)
After=network-online.target
Wants=network-online.target

[Service]
User=pq-relay
Group=pq-relay
Environment=PQ_RELAY_DOMAIN=relay-fsn7.sth.cx
Environment=PQ_RELAY_ACME_CONTACT=mailto:ops@smartholdem.io
Environment=PQ_RELAY_ACME_CACHE=/var/lib/pq-relay/acme
Environment=PQ_RELAY_QUIC_PORT=7842
Environment=RUST_LOG=pq_relay=info,iroh_relay=info
ExecStart=/opt/pq-relay/pq-relay
# bind 80/443 without root
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
NoNewPrivileges=true
StateDirectory=pq-relay
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd -r -s /usr/sbin/nologin pq-relay
sudo mkdir -p /var/lib/pq-relay/acme && sudo chown -R pq-relay: /var/lib/pq-relay
sudo ufw allow 443/tcp && sudo ufw allow 80/tcp && sudo ufw allow 7842/udp
```

## Переход с nginx (на каждом релее, ~2 минуты простоя)

1. Собрать новый бинарник, положить в `/opt/pq-relay/`.
2. **Сначала staging**: `PQ_RELAY_ACME_STAGING=1`, остановить nginx (`systemctl stop nginx`), запустить релей, дождаться в
   логе `acme event: ...` без ошибок и `HTTPS on 0.0.0.0:443`. Проверить: `curl -kI https://relay-fsn7.sth.cx/` -> ответ релея.
3. Убрать `PQ_RELAY_ACME_STAGING`, очистить кэш (`rm -rf /var/lib/pq-relay/acme/*`), перезапустить - боевой сертификат.
   Проверить: `curl -I https://relay-fsn7.sth.cx/` (без `-k`) -> 200, `openssl s_client -connect relay-fsn7.sth.cx:443 -alpn h2 </dev/null | grep issuer`.
4. `systemctl disable --now nginx` (или убрать только этот `server {}`), удалить cron/timer certbot для этого домена.
5. Со стороны ноды: `sth-core doctor` -> домашний релей; в логе ноды пропадают таймауты QAD-проб к 7842; NAT-узлы начинают
   получать внешний адрес от релея (`/api/ntfry/metrics -> ntfry.directAddrs` появляется публичный `ip:port` даже без
   `external_addr`).

## Почему так, а не иначе

- **Без сертификата на релее QUIC невозможен** - TLS 1.3 обязателен по спецификации QUIC; самоподписанный клиенты iroh не примут.
- **Один сертификат на всё**: ACME-резолвер `iroh-relay` отдаёт его и HTTPS/WebSocket-акцептору, и QUIC-серверу
  (`QuicConfig.server_config: None` -> наследуется из `RelayConfig::tls`).
- **TLS-ALPN-01** не требует 80-го порта и не конфликтует с обслуживанием клиентов: challenge приходит на 443 с ALPN `acme-tls/1`,
  обычные подключения - с `h2`/`http/1.1`. Порт 80 оставлен только для health-check и совместимости.
- Лимиты Let's Encrypt: 5 одинаковых сертификатов в неделю - отсюда шаг со staging.
- Вариант B (nginx + файлы certbot, только QUIC в релее) остаётся рабочим запасным: `CertConfig::Manual { server_config }` с
  сертификатом из `/etc/letsencrypt/live/…` и `--deploy-hook systemctl restart pq-relay`.

## Проверка QAD с ноды

```bash
# порт открыт и отвечает по QUIC (после апдейта релея)
sth-core doctor                       # строка iroh relay - без предупреждений
grep -c "QAD\|quic" sth-core.log      # нет повторяющихся timeouts к :7842
```
