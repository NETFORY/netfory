# NETFORY Relay (n1-relay)

A lightweight, self-hosted **iroh-relay** server for the NETFORY P2P network.
Operators run it on their own servers, and NETFORY clients may choose to route traffic through these relays in addition to the standard public n0 relays (n0-computer).

> Crate: `pq-relay` 0.2.0 · binary: `pq-relay` · based on: `iroh-relay = "1"` (feature `server`) + `rustls 0.23`

> ⚠️ Since 0.2.0, the relay must serve QUIC/QAD on 7842/udp, otherwise new QUIC nodes such as sth-core and NETFORY clients behind NAT will not learn their external address (timeouts during each `net_report`; after disabling n0, only transit through relays remains).

---

## 1. What is a relay and why is it needed

In iroh, two nodes try to establish a **direct** connection (hole-punching over QUIC). When both peers are behind "strict" NAT/firewalls and a direct path cannot be established, traffic goes through a **relay** - an intermediate server that simply forwards encrypted packets between peers. The relay **does not decrypt** data:
all NETFORY E2EE cryptography (X25519 + AES-256-GCM) remains between the endpoints. The relay only sees encrypted transit and helps with address discovery (QAD - QUIC Address Discovery).

By default, a NETFORY client uses public relays **n0**. `n1-relay` is our **own** relay infrastructure ("n1" = "network tier 1", community relays of NETFORY).

### Why host your own relay
- **Independence** - the network continues to work even if public n0 relays are unavailable or restricted.
- **Speed / geography** - a relay near users (in the same region) reduces latency for transit delivery of messages and blobs.
- **Privacy** - in `relay_only` mode (Settings -> Network), the client does not reveal its direct IP to peers, and all traffic goes through the relay. Hosting your own relay means you only need to trust your own server.
- **Bandwidth control** - you control limits and resources yourself.
- **Resilience** - you can run several relays in different data centers and distribute their list to clients; NETFORY will choose the best one.

---

## 2. Features

| Feature | Description |
|---|---|
| Relay HTTP endpoint | Full iroh-relay (`/relay`) for forwarding encrypted P2P traffic. |
| QAD (QUIC Address Discovery) | 7842/udp: peers behind NAT learn their external `ip:port` for hole-punching (STUN replacement). Modes A/B. |
| Built-in ACME | Mode A: the relay obtains and renews a Let's Encrypt certificate itself (TLS-ALPN-01, port 80 not required). |
| Plain HTTP (without TLS) | Modes B/C: HTTP behind your reverse proxy. Warning: in mode C there is no QUIC/QAD. |
| Configuration via ENV | Host and port are configured via environment variables without editing code. |
| Graceful shutdown | Clean stop on `Ctrl-C` (SIGINT) - the relay finishes tasks and closes connections. |
| Structured logs | `tracing` + `tracing-subscriber` with filtering via `RUST_LOG`. |
| Minimal dependencies | Only `iroh-relay` (server) + `tokio` + `tracing`. Small binary, low resource usage. |

> ❗ The relay does **not** store messages and **does not** have access to keys/content -
> it only forwards encrypted packets. This is a "dumb" but reliable transit layer.

---

## 3. Project structure

`Cargo.toml` (key part):
```toml
[dependencies]
iroh-relay = { version = "1.0", features = ["server"] }   # feature "server" is required
tokio = { version = "1", features = ["rt-multi-thread", "macros", "signal"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

> ⚠️ Without `features = ["server"]`, the module `iroh_relay::server` is unavailable
> (error `error[E0432]: unresolved import iroh_relay::server`).

---

## 4. Modes and environment variables

The mode is selected automatically based on the provided variables:

| Mode | When | HTTPS/WS | QUIC/QAD 7842/udp | Certificate |
|---|---|---|---|---|
| **A** (recommended) | `PQ_RELAY_DOMAIN` is set | relay itself on 443 | yes | Let's Encrypt via ACME TLS-ALPN-01, auto-renew |
| **B** | `PQ_RELAY_TLS_CERT` + `_KEY` are set | nginx/Caddy on 443 -> HTTP 3340 | yes | PEM certbot is read by relay; restart after renewal |
| **C** (legacy) | nothing | proxy -> HTTP 3340 | **no** | handled by proxy |

| Variable | Purpose | Default |
|---|---|---|
| `PQ_RELAY_DOMAIN` | mode A: public relay hostname (DNS A/AAAA already points to the host) | - |
| `PQ_RELAY_ACME_CONTACT` | Let's Encrypt contact | `mailto:ops@smartholdem.io` |
| `PQ_RELAY_ACME_CACHE` | ACME account and certificate directory | `/var/lib/pq-relay/acme` |
| `PQ_RELAY_ACME_STAGING` | `1` - staging CA (first run; LE limit is 5 certs/week) | - |
| `PQ_RELAY_HTTPS_PORT` | mode A: HTTPS port | `443` |
| `PQ_RELAY_QUIC_PORT` | QUIC Address Discovery port, `0` disables it | `7842` |
| `PQ_RELAY_TLS_CERT` / `PQ_RELAY_TLS_KEY` | mode B: `fullchain.pem` / `privkey.pem` from certbot | - |
| `PQ_RELAY_HOST` | bind address | `0.0.0.0` |
| `PQ_RELAY_PORT` | plain HTTP port (A: health-check; B/C: behind proxy) | `80` (A) / `3340` (B/C) |
| `RUST_LOG` | log filter | `pq_relay=info,iroh_relay=info` |

Firewall: `ufw allow 443/tcp && ufw allow 7842/udp` (A), `ufw allow 7842/udp` (B). Ready-made systemd units are in `deploy/`.

## 5. Installation and startup

### Requirements
- Rust (stable) + Cargo - https://rustup.rs
- Open port (default `3340`) on the server / firewall.

### Build
```bash
cd /app/pq-relay

# Compile check
cargo check

# Release build (optimized binary)
cargo build --release
# binary: ./target/release/pq-relay
```

### Run
```bash
# With default settings (0.0.0.0:3340)
cargo run --release

# Or directly with the binary, overriding host/port and logs
PQ_RELAY_HOST=0.0.0.0 \
PQ_RELAY_PORT=3340 \
RUST_LOG=pq_relay=info,iroh_relay=debug \
./target/release/pq-relay
```

On successful startup, the console prints:
```
NETFORY relay (n1-relay) up on http://0.0.0.0:3340  - front with HTTPS and point clients at https://<your-host>
```

Stop with `Ctrl-C` (graceful shutdown).

---

## 6. Production deployment (HTTPS via reverse proxy)

The relay itself listens on **plain HTTP**. Put a reverse proxy with TLS in front of it (Caddy / Nginx / Traefik). Clients are given the **HTTPS** URL of your host.

### Option A - Caddy (automatic Let's Encrypt)
```caddy
# /etc/caddy/Caddyfile
relay.example.com {
    reverse_proxy 127.0.0.1:3340
}
```

### Option B - Nginx
```nginx
server {
    listen 443 ssl;
    server_name relay.example.com;

    ssl_certificate     /etc/letsencrypt/live/relay.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/relay.example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:3340;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;      # WebSocket upgrade for relay
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_read_timeout 86400s;                   # long-lived connections
    }
}
```
> The relay uses WebSocket upgrade, so the `Upgrade`/`Connection` headers and the large `proxy_read_timeout` are mandatory.

### systemd service (autostart)
```ini
# /etc/systemd/system/pq-relay.service
[Unit]
Description=NETFORY Relay (n1-relay)
After=network.target

[Service]
Environment=PQ_RELAY_HOST=127.0.0.1
Environment=PQ_RELAY_PORT=3340
Environment=RUST_LOG=pq_relay=info,iroh_relay=info
ExecStart=/opt/pq-relay/pq-relay
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```
```bash
sudo systemctl daemon-reload
sudo systemctl enable --now pq-relay
sudo journalctl -u pq-relay -f   # logs
```

> Tip: for multiple relays, run one instance in each region and distribute **all** their URLs to clients - iroh will select the best one by latency.

---

## 7. Connecting to the NETFORY frontend

Users connect custom relays directly from the app - **without rebuilding the client**.

### Where (UI)
`Settings -> Network -> NETFORY Relays (n1-relay)`
(`Settings.vue`, block `data-testid="n1-relay-row"`):
- **Toggle** `n1-relay-toggle` - enable/disable custom relay usage.
- **Input field** `n1-relay-input` - list of relay URLs, **one per line**
  (for example `https://relay.example.com`).
- **Button** `save-n1-relay-btn` - save.

### What happens under the hood
1. **Frontend** (`Settings.vue`) calls the bridge `bridge.ts`:
   - `getRelayConfig()` - loads the current settings on mount.
   - `setRelayConfig(enabled, urls)` - saves the toggle and URL list.
2. **Tauri commands** (Rust, `p2p.rs`, registered in `lib.rs`):
   - `get_relay_config` / `set_relay_config` - read/write settings in a local **sled** database under keys `cfg:n1_relay_enabled` and `cfg:n1_relay_urls` (JSON array of strings). Empty strings are filtered out.
3. **Applying to the iroh endpoint** (`p2p.rs`, `mod p2p_impl::apply_pq_relays`):
   when the P2P node initializes, if the toggle is enabled, each valid URL is parsed into `iroh::RelayUrl` and added to the relay map of the endpoint **alongside** the default n0 relays:
   ```rust
   let rc = RelayConfig::new(url.clone(), Some(RelayQuicConfig::default()));
   endpoint.insert_relay(url, Arc::new(rc)).await;
   ```
   So custom relays **augment**, rather than replace, public relays - connectivity is preserved even if one of your relays is unavailable.

### Which URL to enter
- Behind a TLS reverse proxy: `https://relay.example.com` (recommended for production).
- Local test without TLS: `http://<ip>:3340`.

### Important note about node identity
The relay **does not require** client-side registration/keys. You can change the list of relays at any time; new settings apply on the next P2P node initialization (restarting the app guarantees the changes are picked up).

---

## 8. Health check

```bash
# 1. Is the relay listening on the port?
ss -ltnp | grep 3340

# 2. Does HTTP respond (relay is up)?
curl -i http://127.0.0.1:3340/

# 3. Through a proxy (TLS) from outside
curl -i https://relay.example.com/
