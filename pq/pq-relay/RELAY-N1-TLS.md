# n1-relay (pq-relay) without nginx: it obtains the Let's Encrypt certificate itself, serves HTTPS/WebSocket and QUIC (QAD)

Variant **A** from the discussion on 24.09.2026: the relay listens on 443 itself, obtains and renews a certificate via the built-in ACME client of `iroh-relay`
(`TLS-ALPN-01` challenge - directly on 443, port 80 is not required), and uses the same certificate to serve the QUIC port
**7842/udp** for QUIC Address Discovery. nginx on this host is no longer needed.

## What about the domain `relay-fsn7.sth.cx`

The domain remains exactly where it was before - in DNS: an A record (and AAAA, if present) `relay-fsn7.sth.cx -> server IP`, as it is now.
In the relay config, the domain is specified **instead of** `server_name` in nginx - as the name for which the certificate is requested
(`PQ_RELAY_DOMAIN=relay-fsn7.sth.cx`). Let's Encrypt connects to `relay-fsn7.sth.cx:443`; the relay replies to the
TLS-ALPN-01 challenge itself, stores the certificate in `PQ_RELAY_ACME_CACHE`, and renews it automatically (about 30 days before expiry).
Clients continue to use `https://relay-fsn7.sth.cx` - the URL does not change, and `N1_RELAYS` in sth-core and NETFORY settings do not need to be touched.

Requirements: 443/tcp and 7842/udp are open in the external firewall; nginx (or anything else) **does not** listen on  443 on this IP; binding to 443 requires either running as root or `AmbientCapabilities=CAP_NET_BIND_SERVICE` in systemd (below).

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

> **Transition from `smartnet-relay` (old service name).** If a `smartnet-relay.service`
> (former name of `pq-relay`) is already running on the server, it keeps 443/80/7842 - the new unit will fail with `Failed to bind TcpListener: Address already in use (os error 98)`, and `https://relay-fsn7.sth.cx/` will still be served by the **old** process.
> Steps:
>
> ```bash
> sudo ss -ltnp 'sport = :443'                     # who owns 443 - you will see smartnet-relay
> sudo systemctl disable --now smartnet-relay      # stop and disable the old unit
> sudo mv /etc/systemd/system/smartnet-relay.service /root/smartnet-relay.service.bak   # (optional) remove the file
> sudo systemctl daemon-reload
> # The old relay's ACME cache and key can be reused - the certificate will not be re-issued:
> sudo cp -a /var/lib/smartnet-relay/. /var/lib/pq-relay/ 2>/dev/null; sudo chown -R pq-relay: /var/lib/pq-relay
> sudo systemctl restart pq-relay && sudo journalctl -u pq-relay -f      # wait for "HTTPS on 0.0.0.0:443"
> ```
>
> The old user/directories (`smartnet-relay`, `/var/lib/smartnet-relay`, `/opt/smartnet-relay`) can be removed after verification. If 443 is held by nginx instead of this service, see the section "Migrating from nginx" below.


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

## Migration from nginx (for each relay, ~2 minutes downtime)

1. Build the new binary and place it in `/opt/pq-relay/`.
2. **First do staging**: `PQ_RELAY_ACME_STAGING=1`, stop nginx (`systemctl stop nginx`), start the relay, wait for the log message `acme event: ...` without errors and `HTTPS on 0.0.0.0:443`. Verify: `curl -kI https://relay-fsn7.sth.cx/` -> the relay replies.
3. Remove `PQ_RELAY_ACME_STAGING`, clear the cache (`rm -rf /var/lib/pq-relay/acme/*`), restart - production certificate.
   Verify: `curl -I https://relay-fsn7.sth.cx/` (without `-k`) -> 200, `openssl s_client -connect relay-fsn7.sth.cx:443 -alpn h2 </dev/null | grep issuer`.
4. `systemctl disable --now nginx` (or remove only this `server {}` block), delete cron/timer certbot for this domain.
5. From the node side: `sth-core doctor` -> home relay; in the node logs the QAD probe timeouts to 7842 disappear; NAT nodes begin to receive a public address from the relay (`/api/ntfry/metrics -> ntfry.directAddrs` shows a public `ip:port` even without `external_addr`).

## Why this design and not another

- **Without a certificate on the relay, QUIC is impossible** - TLS 1.3 is mandatory by the QUIC specification; self-signed certificates are not accepted by iroh clients.
- **One certificate for everything**: the ACME resolver in `iroh-relay` serves it to both the HTTPS/WebSocket acceptor and the QUIC server (`QuicConfig.server_config: None` -> inherited from `RelayConfig::tls`).
- **TLS-ALPN-01** does not require port 80 and does not conflict with serving clients: the challenge arrives on 443 with ALPN `acme-tls/1`, normal connections use `h2`/`http/1.1`. Port 80 is kept only for health checks and compatibility.
- Let's Encrypt limits: 5 identical certificates per week - this is why the staging step exists.
- Variant B (nginx + certbot files, QUIC only on the relay) remains a working fallback: `CertConfig::Manual { server_config }` with the certificate from `/etc/letsencrypt/live/...` and `--deploy-hook systemctl restart pq-relay`.

## Verifying QAD from a node

```bash
# port is open and responds over QUIC (after relay update)
sth-core doctor                       # relay line - without warnings
grep -c "QAD\|quic" sth-core.log      # no repeated timeouts to :7842
```
