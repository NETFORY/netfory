![NETFORY-Relay.jpg](NETFORY-Relay.jpg)
[RU](README_RU.md) | [EN](README.md)
# NETFORY Relay (n1-relay)
A lightweight, self‑hosted **relay** server for the NETFORY peer‑to‑peer network.

Operators run it on their own servers, and NETFORY clients can *optionally*
route traffic through these relays – in addition to the standard public
relays provided by n0 (n0‑computer).

> Crate: `NETFORY-relay` · Binary: `NETFORY-relay` · Based on: `iroh-relay = "1.0"` (feature `server`)

---

## 1. What is a relay and why you need it

Two nodes attempt to establish a **direct** connection (hole‑punching over
QUIC). When both peers are behind strict NAT/firewalls and a direct path
cannot be punched, traffic goes through a **relay** – an intermediary server
that simply forwards encrypted packets between peers. The relay **does not
decrypt** data: all NETFORY end‑to‑end encryption (X25519 + AES‑256‑GCM)
remains between the endpoints. The relay sees only encrypted transit and
assists with address discovery (QAD – QUIC Address Discovery).

By default, a NETFORY client uses the public relays provided by **n0**.
`n1-relay` is our **own** relay infrastructure ("n1" = "network tier 1",
relays operated by the SmartHoldem community).

### Why host your own relay
- **Independence** – the network keeps working even if the public n0 relays
  become unavailable or rate‑limited.
- **Speed / Geography** – a relay close to users (in their region) provides
  lower latency for delivering messages and blobs.
- **Privacy** – in `relay_only` mode (Settings > Network) the client does
  not reveal its direct IP to peers; all traffic goes through the relay.
  Hosting your own relay means you only need to trust your own server.
- **Bandwidth control** – you manage limits and resources yourself.
- **Resilience** – you can deploy several relays in different data‑centres
  and distribute the list to clients; the network picks the best one
  automatically.

---

## 2. Features

| Feature | Description |
|---|---|
| Relay HTTP endpoint | A full‑featured NETFORY‑relay (`/relay`) for forwarding encrypted P2P traffic. |
| QAD (QUIC Address Discovery) | Helps peers discover their external addresses for hole‑punching. |
| Plain HTTP (no TLS) | The relay listens on plain HTTP – TLS is added externally (reverse‑proxy), simplifying deployment and certificate rotation. |
| Configuration via environment variables | Host and port are set through environment variables, no code changes needed. |
| Graceful shutdown | A clean stop on `Ctrl‑C` (SIGINT) – the relay completes tasks and closes connections. |
| Structured logs | `tracing` + `tracing‑subscriber` with a filter via `RUST_LOG`. |
| Minimal dependencies | Only `NETFORY-relay` (server) + `tokio` + `tracing`. Small binary, low resource usage. |

> ❗ The relay **does not** store messages and **has no** access to keys or
> content – it only forwards encrypted packets. It is "dumb" but reliable
> transit.

---

## 3. Project structure

```
/app/NETFORY-relay/
├── Cargo.toml          # Dependencies (NETFORY-relay feature "server", tokio, tracing)
├── src/
│   └── main.rs         # Entry point: config from ENV > Server::spawn > wait for Ctrl-C

```

`Cargo.toml` (key parts):
```toml
[dependencies]
NETFORY-relay = { version = "1.0", features = ["server"] }   # Feature "server" is required
tokio = { version = "1", features = ["rt-multi-thread", "macros", "signal"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

> ⚠️ Without `features = ["server"]` the `iroh_relay::server` module is
> unavailable (error `error[E0432]: unresolved import iroh_relay::server`).

---

## 4. Environment variables

| Variable | Purpose | Default |
|---|---|---|
| `NETFORY_RELAY_HOST` | Bind address for the HTTP server | `0.0.0.0` |
| `NETFORY_RELAY_PORT` | Bind port for the HTTP server | `3340` |
| `RUST_LOG` | Log level / filter (`tracing`) | `NETFORY_relay=info,iroh_relay=info` |

---

## 5. Installation and running

### Requirements
- Rust (stable) + Cargo – https://rustup.rs
- An open port (default `3340`) on the server / in the firewall.

### Build
```bash
cd /NETFORY-relay

# Check compilation
cargo check

# Release build (optimised binary)
cargo build --release
# Binary: ./target/release/NETFORY-relay
```

### Run
```bash
# With default settings (0.0.0.0:3340)
cargo run --release

# Or directly with the binary, overriding port/host and logs
NETFORY_RELAY_HOST=0.0.0.0 \
NETFORY_RELAY_PORT=3340 \
RUST_LOG=NETFORY_relay=info,iroh_relay=debug \
./target/release/NETFORY-relay
```

On a successful start the console will print:
```
NETFORY relay (n1-relay) up on http://0.0.0.0:3340  - front with HTTPS and point clients at https://<your-host>
```

Stop the relay with `Ctrl‑C` (graceful shutdown).

---

## 6. Production deployment (HTTPS via reverse proxy)

The relay itself listens on **plain HTTP**. For the internet, put a TLS‑enabled
reverse proxy in front of it (Caddy / Nginx / Traefik). Clients receive the
**HTTPS** URL of your host.

### Option A – Caddy (automatic Let’s Encrypt)
```caddy
# /etc/caddy/Caddyfile
relay.smartholdem.io {
    reverse_proxy 127.0.0.1:3340
}
```

### Option B – Nginx
```nginx
server {
    listen 443 ssl;
    server_name relay.smartholdem.io;

    ssl_certificate     /etc/letsencrypt/live/relay.smartholdem.io/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/relay.smartholdem.io/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:3340;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;      # WebSocket upgrade for the relay
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_read_timeout 86400s;                   # Long‑lived connections
    }
}
```
> The relay uses a WebSocket upgrade, so the `Upgrade`/`Connection` headers
> and a large `proxy_read_timeout` are required.

### systemd service (auto‑start)
```ini
# /etc/systemd/system/NETFORY-relay.service
[Unit]
Description=NETFORY Relay (n1-relay)
After=network.target

[Service]
Environment=NETFORY_RELAY_HOST=127.0.0.1
Environment=NETFORY_RELAY_PORT=3340
Environment=RUST_LOG=NETFORY_relay=info,iroh_relay=info
ExecStart=/opt/NETFORY-relay/NETFORY-relay
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```
```bash
sudo systemctl daemon-reload
sudo systemctl enable --now NETFORY-relay
sudo journalctl -u NETFORY-relay -f   # Logs
```

> Tip: for multiple relays, deploy one instance per region and distribute
> **all** their URLs to clients – the network will pick the one with the
> lowest latency.

---

## 7. Connecting to the NETFORY frontend

Users add custom relays directly from the application – **no client rebuild
required**.

### Where (UI)
`Settings > Network > NETFORY Relays (n1-relay)`
(`Settings.vue`, block `data-testid="n1-relay-row"`):
- **Toggle** `n1-relay-toggle` – enable / disable custom relays.
- **Input** `n1-relay-input` – list of relay URLs, **one per line**
  (e.g. `https://relay.smartholdem.io`).
- **Button** `save-n1-relay-btn` – save.

### What happens under the hood
1. **Frontend** (`Settings.vue`) calls the `bridge.ts` bridge:
    - `getRelayConfig()` – loads current settings on `onMount`.
    - `setRelayConfig(enabled, urls)` – saves the toggle and the URL list.
2. **Tauri commands** (Rust, `p2p.rs`, registered in `lib.rs`):
    - `get_relay_config` / `set_relay_config` – read / write settings to the
      local **sled** database under keys `cfg:n1_relay_enabled` and
      `cfg:n1_relay_urls` (JSON array of strings). Empty strings are filtered
      out.
3. **Applied to the iroh endpoint** (`p2p.rs`, `mod p2p_impl::apply_NETFORY_relays`):
   when the P2P node initialises, if the toggle is on, each valid URL is
   parsed into an `iroh::RelayUrl` and added to the endpoint’s relay map
   **alongside** the standard n0 relays:
   ```rust
   let rc = RelayConfig::new(url.clone(), Some(RelayQuicConfig::default()));
   endpoint.insert_relay(url, Arc::new(rc)).await;
   ```
   Custom relays **supplement** rather than replace the public ones –
   connectivity is preserved even if one of your relays goes down.

### Which URL to enter
- Behind a TLS reverse proxy: `https://relay.smartholdem.io` (recommended for
  production).
- Local test without TLS: `http://<ip>:3340`.

### Important note on node identity
The relay **does not require** any registration or keys from the client. You
can change the relay list at any time; the new settings are applied on the
next P2P node initialisation (restarting the app guarantees the changes take
effect).

---

## 8. Health check

```bash
# 1. Is the relay listening on the port?
ss -ltnp | grep 3340

# 2. Does HTTP respond? (relay is up)
curl -i http://127.0.0.1:3340/

# 3. Via the proxy (TLS) from outside
curl -i https://relay.smartholdem.io/
```
In the application: enable the toggle, enter the URL, save – relay logs will
show connection entries when clients connect (`RUST_LOG=...=debug` for details).

---

## 9. Common issues

| Symptom | Cause / fix |
|---|---|
| `error[E0432]: unresolved import iroh_relay::server` | The `server` feature is not enabled for `NETFORY-relay` in `Cargo.toml`. |
| Client does not connect via the relay | The URL must be `https://...` (through a TLS proxy) or `http://ip:port` on the local network; check that the port is open and that the `Upgrade`/`Connection` headers are set in the proxy. |
| Connections drop after ~1 minute | Increase `proxy_read_timeout` (the relay holds long‑lived WebSocket connections). |
| No logs | Set `RUST_LOG=NETFORY_relay=info,iroh_relay=debug`. |
| Port already in use | Change `NETFORY_RELAY_PORT` or free the port. |

---

## 10. Security and privacy

- The relay forwards **only encrypted** traffic; message content and keys are
  inaccessible to it (E2EE remains on the endpoints).
- The relay sees the connection itself and transit network addresses – therefore
  host the relay on trusted infrastructure when enabling `relay_only`.
- It is recommended to always expose the relay over **HTTPS** (via a TLS proxy).
