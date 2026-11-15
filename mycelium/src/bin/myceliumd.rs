//! myceliumd — консольный узел Mycelium: релей + локальный SOCKS5 (`127.0.0.1:9150`).
//! Адресация через SOCKS5: хост `<EndpointId>.iroh`, порт → ALPN по таблице из конфига.
//!
//!   myceliumd relay            # только релей
//!   myceliumd socks            # релей (если включён в конфиге) + SOCKS5
//!   myceliumd --config my.toml socks
use anyhow::{anyhow, Context, Result};
use iroh::{Endpoint, EndpointId, SecretKey};
use mycelium::{Config, Mycelium};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::net::TcpListener;

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct DaemonConfig {
    /// relay | client
    role: String,
    hops: u8,
    bandwidth: u64,
    max_circuits: u32,
    socks_port: u16,
    bootstrap: Vec<String>,
    exit_alpn_prefixes: Vec<String>,
    /// SOCKS5-порт → ALPN целевого сервиса.
    ports: HashMap<String, String>,
    data_dir: PathBuf,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let mut ports = HashMap::new();
        ports.insert("443".into(), "netfory/api/1".into());
        ports.insert("4003".into(), "netfory/api/1".into());
        ports.insert("7000".into(), "netfory/chat-dm/2".into());
        ports.insert("7001".into(), "netfory/chat-sync/1".into());
        Self {
            role: "relay".into(),
            hops: 3,
            bandwidth: 2 * 1024 * 1024,
            max_circuits: 64,
            socks_port: 9150,
            bootstrap: Vec::new(),
            exit_alpn_prefixes: mycelium::ExitPolicy::default().allowed_alpn_prefixes,
            ports,
            data_dir: dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("myceliumd"),
        }
    }
}

fn load_secret(dir: &PathBuf) -> Result<SecretKey> {
    std::fs::create_dir_all(dir)?;
    let p = dir.join("node.key");
    if let Ok(hex_s) = std::fs::read_to_string(&p) {
        let b: [u8; 32] = hex::decode(hex_s.trim())?.try_into().map_err(|_| anyhow!("node.key: bad length"))?;
        return Ok(SecretKey::from_bytes(&b));
    }
    let sk = SecretKey::generate();
    std::fs::write(&p, hex::encode(sk.to_bytes()))?;
    Ok(sk)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("mycelium=info".parse()?)).init();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg_path = PathBuf::from("myceliumd.toml");
    if let Some(i) = args.iter().position(|a| a == "--config") {
        cfg_path = PathBuf::from(args.get(i + 1).ok_or_else(|| anyhow!("--config <file>"))?);
        args.drain(i..=i + 1);
    }
    let cmd = args.first().cloned().unwrap_or_else(|| "socks".into());
    let dc: DaemonConfig = match std::fs::read_to_string(&cfg_path) {
        Ok(t) => toml::from_str(&t).context("config")?,
        Err(_) => {
            let d = DaemonConfig::default();
            let _ = std::fs::write(&cfg_path, toml::to_string_pretty(&d)?);
            eprintln!("default config created: {}", cfg_path.display());
            d
        }
    };
    if cmd == "init" {
        return Ok(());
    }
    let secret = load_secret(&dc.data_dir)?;
    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).secret_key(secret).alpns(vec![mycelium::ALPN.to_vec(), iroh_gossip::ALPN.to_vec()]).bind().await?;
    let gossip = iroh_gossip::net::Gossip::builder().spawn(endpoint.clone());
    let bootstrap: Vec<EndpointId> = dc.bootstrap.iter().filter_map(|s| s.parse().ok()).collect();
    let cfg = Config {
        hops: dc.hops,
        relay: dc.role == "relay",
        pool_size: 2,
        bootstrap,
        exit_policy: mycelium::ExitPolicy { allowed_alpn_prefixes: dc.exit_alpn_prefixes.clone() },
        limits: mycelium::RelayLimits { bandwidth: dc.bandwidth, max_circuits: dc.max_circuits },
        data_dir: dc.data_dir.clone(),
        hidden: false,
        intro_points: 2,
        cover_traffic: false,
    };
    let my = Mycelium::start(endpoint.clone(), Some(gossip.clone()), cfg, vec![]).await?;
    let router = iroh::protocol::Router::builder(endpoint.clone()).accept(mycelium::ALPN, my.handler()).accept(iroh_gossip::ALPN, gossip).spawn();
    println!("myceliumd · node {} · role {} · relays {}", endpoint.id(), dc.role, my.status().relays_known);

    if cmd == "socks" {
        my.set_enabled(true);
        let listener = TcpListener::bind(("127.0.0.1", dc.socks_port)).await?;
        println!("SOCKS5 on 127.0.0.1:{} — hosts like <EndpointId>.iroh, port → ALPN: {:?}", dc.socks_port, dc.ports);
        let ports: HashMap<u16, String> = dc.ports.iter().filter_map(|(k, v)| Some((k.parse().ok()?, v.clone()))).collect();
        tokio::spawn(mycelium::socks::serve(listener, my.clone(), ports, None));
    }
    tokio::signal::ctrl_c().await?;
    router.shutdown().await?;
    Ok(())
}
