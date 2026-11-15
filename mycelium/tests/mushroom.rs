//! Сквозной тест: клиент → Guard → Middle → Exit → цель на локальных iroh-узлах (без релеев n0).
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::presets;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointId, RelayMode};
use mycelium::{Circuit, Config, Mycelium};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ECHO_ALPN: &[u8] = b"netfory/test-echo/1";

#[derive(Debug, Clone)]
struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            tokio::spawn(async move {
                let data = recv.read_to_end(4 * 1024 * 1024).await.unwrap_or_default();
                let mut out = b"echo:".to_vec();
                out.extend_from_slice(&data);
                let _ = send.write_all(&out).await;
                let _ = send.finish();
                let _ = send.stopped().await;
            });
        }
        Ok(())
    }
}

async fn node(lookup: &MemoryLookup) -> Endpoint {
    let ep = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup.clone())
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    // прямые адреса появляются не сразу после bind — без них lookup отдаёт пустую запись
    for _ in 0..100 {
        if ep.addr().ip_addrs().next().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    lookup.add_endpoint_info(ep.addr());
    ep
}

struct Net {
    lookup: MemoryLookup,
    relays: Vec<(Endpoint, Router, Mycelium)>,
    target: (Endpoint, Router),
}

async fn net(n_relays: usize) -> Net {
    let lookup = MemoryLookup::new();
    let mut relays = Vec::new();
    for _ in 0..n_relays {
        let ep = node(&lookup).await;
        let cfg = Config { relay: true, data_dir: tempdir(), ..Config::default() };
        let my = Mycelium::start(ep.clone(), None, cfg, vec![]).await.unwrap();
        let r = Router::builder(ep.clone()).accept(mycelium::ALPN, my.handler()).spawn();
        relays.push((ep, r, my));
    }
    let tep = node(&lookup).await;
    let tr = Router::builder(tep.clone()).accept(ECHO_ALPN, Echo).spawn();
    Net { lookup, relays, target: (tep, tr) }
}

fn tempdir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("myc-test-{}", rand_u64()));
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn rand_u64() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64 ^ (std::process::id() as u64) << 32
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_hop_request_response() {
    let n = net(3).await;
    let client = node(&n.lookup).await;
    let path: Vec<EndpointId> = n.relays.iter().map(|r| r.0.id()).collect();
    let circ = tokio::time::timeout(Duration::from_secs(20), Circuit::build(&client, path)).await.unwrap().unwrap();
    assert_eq!(circ.hop_count(), 3);
    let s = circ.open_stream(n.target.0.id(), ECHO_ALPN).await.unwrap();
    let resp = s.request(b"hello mycelium", 1 << 20).await.unwrap();
    assert_eq!(resp, b"echo:hello mycelium");
    // релеи действительно пропустили ячейки, exit открыл поток
    let exit = &n.relays[2].2.status();
    assert_eq!(exit.exit_streams, 1);
    assert!(n.relays[0].2.status().relay_cells > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_streams_large_payload() {
    let n = net(3).await;
    let client = node(&n.lookup).await;
    let path: Vec<EndpointId> = n.relays.iter().map(|r| r.0.id()).collect();
    let circ = Circuit::build(&client, path).await.unwrap();
    let target = n.target.0.id();
    let mut tasks = Vec::new();
    for i in 0..6u8 {
        let c = circ.clone();
        tasks.push(tokio::spawn(async move {
            let payload: Vec<u8> = (0..200_000u32).map(|k| (k as u8).wrapping_add(i)).collect();
            let mut s = c.open_stream(target, ECHO_ALPN).await.unwrap();
            s.write_all(&payload).await.unwrap();
            s.shutdown().await.unwrap();
            let mut out = Vec::new();
            s.read_to_end(&mut out).await.unwrap();
            assert_eq!(&out[..5], b"echo:");
            assert_eq!(&out[5..], &payload[..], "поток {i}");
        }));
    }
    for t in tasks {
        tokio::time::timeout(Duration::from_secs(60), t).await.unwrap().unwrap();
    }
    assert!(!circ.is_closed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exit_policy_blocks_foreign_alpn() {
    let n = net(3).await;
    let client = node(&n.lookup).await;
    let path: Vec<EndpointId> = n.relays.iter().map(|r| r.0.id()).collect();
    let circ = Circuit::build(&client, path).await.unwrap();
    let err = circ.open_stream(n.target.0.id(), b"http/1.1").await.err().unwrap();
    assert!(err.to_string().contains("exit policy"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_mode_disabled_relay_rejects() {
    let n = net(3).await;
    n.relays[1].2.set_relay_enabled(false);
    let client = node(&n.lookup).await;
    let path: Vec<EndpointId> = n.relays.iter().map(|r| r.0.id()).collect();
    assert!(Circuit::build(&client, path).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn high_level_pool_and_isolation() {
    let n = net(4).await;
    let client = node(&n.lookup).await;
    let cfg = Config { bootstrap: n.relays.iter().map(|r| r.0.id()).collect(), data_dir: tempdir(), ..Config::default() };
    let my = Mycelium::start(client.clone(), None, cfg, vec![]).await.unwrap();
    my.set_enabled(true);
    // пул наполняется в фоне
    for _ in 0..100 {
        if my.status().circuits_ready >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(my.status().circuits_ready >= 2, "{:?}", my.status());
    let s = my.open_stream(n.target.0.id(), ECHO_ALPN).await.unwrap();
    assert_eq!(s.request(b"x", 1024).await.unwrap(), b"echo:x");
    let st = my.status();
    assert_eq!(st.guards.len(), 3);
    my.set_enabled(false);
    assert_eq!(my.status().circuits_ready, 0);
    assert!(my.open_stream(n.target.0.id(), ECHO_ALPN).await.is_err(), "fail-closed при выключенном Mycelium");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn socks5_through_circuit() {
    let n = net(3).await;
    let client = node(&n.lookup).await;
    let cfg = Config { bootstrap: n.relays.iter().map(|r| r.0.id()).collect(), data_dir: tempdir(), ..Config::default() };
    let my = Mycelium::start(client.clone(), None, cfg, vec![]).await.unwrap();
    my.set_enabled(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut ports = std::collections::HashMap::new();
    ports.insert(7000u16, String::from_utf8(ECHO_ALPN.to_vec()).unwrap());
    tokio::spawn(mycelium::socks::serve(listener, my.clone(), ports, None));
    assert!(mycelium::socks::probe(port).await);
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(&[5, 1, 0]).await.unwrap();
    let mut r = [0u8; 2];
    s.read_exact(&mut r).await.unwrap();
    let host = format!("{}.iroh", n.target.0.id());
    let mut req = vec![5, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&7000u16.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).await.unwrap();
    assert_eq!(rep[1], 0, "SOCKS CONNECT отклонён");
    s.write_all(b"via socks").await.unwrap();
    s.shutdown().await.unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    assert_eq!(out, b"echo:via socks");
    assert!(!mycelium::socks::probe(1).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gossip_directory_discovers_relays() {
    let lookup = MemoryLookup::new();
    let mut nodes = Vec::new();
    let mut first: Option<EndpointId> = None;
    for i in 0..4 {
        let ep = node(&lookup).await;
        let g = iroh_gossip::net::Gossip::builder().spawn(ep.clone());
        let cfg = Config { relay: i < 3, data_dir: tempdir(), ..Config::default() };
        let my = Mycelium::start(ep.clone(), Some(g.clone()), cfg, first.into_iter().collect()).await.unwrap();
        let r = Router::builder(ep.clone()).accept(mycelium::ALPN, my.handler()).accept(iroh_gossip::ALPN, g).spawn();
        if first.is_none() {
            first = Some(ep.id());
        }
        nodes.push((ep, r, my));
    }
    let client = &nodes[3].2;
    // директория ленивая: клиент подписывается на gossip только при включении
    client.set_enabled(true);
    for _ in 0..100 {
        if client.directory().count() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(client.directory().count(), 3, "клиент должен узнать 3 релея через gossip");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hop_rtt_and_e2e_for_map() {
    let n = net(3).await;
    let client = node(&n.lookup).await;
    let cfg = Config { bootstrap: n.relays.iter().map(|r| r.0.id()).collect(), data_dir: tempdir(), ..Config::default() };
    let my = Mycelium::start(client.clone(), None, cfg, vec![]).await.unwrap();
    my.set_enabled(true);
    let target = n.target.0.id();
    assert_eq!(my.request(target, ECHO_ALPN, b"m", 1024).await.unwrap(), b"echo:m");
    let first = my.circuit_for_target(&target).unwrap();
    assert_eq!(first.path.len(), 3);
    assert!(first.e2e_ms.is_some());
    tokio::time::sleep(Duration::from_millis(400)).await;
    let c = my.circuit_for_target(&target).unwrap();
    assert!(c.hop_rtt_ms.iter().all(|r| r.is_some()), "{:?}", c.hop_rtt_ms);
    assert!(my.circuits().iter().any(|c| c.target.as_deref() == Some(&target.to_string())));
}

/// Фаза 2: A достигает B через точку встречи — B не публикует адрес, ни RP, ни intro не видят пары.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rendezvous_hidden_service() {
    let n = net(6).await;
    let boot: Vec<EndpointId> = n.relays.iter().map(|r| r.0.id()).collect();
    let b_ep = node(&n.lookup).await;
    let b = Mycelium::start(b_ep.clone(), None, Config { bootstrap: boot.clone(), data_dir: tempdir(), ..Config::default() }, vec![]).await.unwrap();
    b.set_service_handler(std::sync::Arc::new(|alpn: Vec<u8>, req: Vec<u8>| -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<u8>>> + Send>> {
        Box::pin(async move {
            let mut out = b"hidden:".to_vec();
            out.extend_from_slice(&alpn);
            out.push(b':');
            out.extend_from_slice(&req);
            Some(out)
        })
    }));
    b.set_enabled(true);
    for _ in 0..200 {
        if b.status().intro_ready >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let desc = b.my_service_descriptor().expect("intro-цепочки B не поднялись");
    assert!(desc.verify());

    let a_ep = node(&n.lookup).await;
    let a = Mycelium::start(a_ep.clone(), None, Config { bootstrap: boot, data_dir: tempdir(), ..Config::default() }, vec![]).await.unwrap();
    assert!(a.directory().insert_service(desc));
    a.set_enabled(true);
    let resp = tokio::time::timeout(Duration::from_secs(60), a.request(b_ep.id(), ECHO_ALPN, b"hi", 4096)).await.unwrap().unwrap();
    assert_eq!(resp, b"hidden:netfory/test-echo/1:hi");
    let info = a.circuit_for_target(&b_ep.id()).unwrap();
    assert_eq!(info.kind, "rendezvous");
    assert!(!info.path.contains(&b_ep.id().to_string()), "RP-цепочка не проходит через B");
    assert!(a.circuits().iter().all(|c| c.kind != "exit" || c.target.is_none()));
    assert!(b.circuits().iter().any(|c| c.kind == "service"), "{:?}", b.circuits());
    // второй запрос — по той же сшитой цепочке
    let resp2 = tokio::time::timeout(Duration::from_secs(30), a.request(b_ep.id(), ECHO_ALPN, b"again", 4096)).await.unwrap().unwrap();
    assert_eq!(resp2, b"hidden:netfory/test-echo/1:again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adaptive_short_path_with_two_relays() {
    // hops=3, а релеев всего 2: цепочка строится из 2 хопов (а не «no relays for role Exit»)
    let n = net(2).await;
    let client = node(&n.lookup).await;
    let cfg = Config { bootstrap: n.relays.iter().map(|r| r.0.id()).collect(), data_dir: tempdir(), ..Config::default() };
    let my = Mycelium::start(client.clone(), None, cfg, vec![]).await.unwrap();
    my.set_enabled(true);
    for _ in 0..100 {
        if my.status().circuits_ready >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let st = my.status();
    assert!(st.circuits_ready >= 1, "{st:?}");
    assert_eq!(st.hops_effective, 2);
    assert!(st.hops_reduced);
    assert_eq!(st.relays_usable, 2);
    let s = my.open_stream(n.target.0.id(), ECHO_ALPN).await.unwrap();
    assert_eq!(s.request(b"short", 1024).await.unwrap(), b"echo:short");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn address_hints_without_discovery() {
    // релеи не опубликованы в lookup: клиент знает только id + подсказку ip:port
    let lookup = MemoryLookup::new();
    let hidden = MemoryLookup::new();
    let mut relays = Vec::new();
    for _ in 0..1 {
        let ep = node(&hidden).await;
        let my = Mycelium::start(ep.clone(), None, Config { relay: true, data_dir: tempdir(), ..Config::default() }, vec![]).await.unwrap();
        let r = Router::builder(ep.clone()).accept(mycelium::ALPN, my.handler()).spawn();
        relays.push((ep, r, my));
    }
    let tep = node(&hidden).await;
    let _tr = Router::builder(tep.clone()).accept(ECHO_ALPN, Echo).spawn();
    let client = node(&lookup).await;
    // Exit (релей) находит цель через свой lookup — как в сети
    let rid = relays[0].0.id();
    let cfg = Config { hops: 1, bootstrap: vec![rid], data_dir: tempdir(), ..Config::default() };
    let my = Mycelium::start(client.clone(), None, cfg, vec![]).await.unwrap();
    let hints: Vec<String> = relays[0].0.addr().ip_addrs().map(|a| a.to_string()).collect();
    assert!(!hints.is_empty());
    my.add_hints(vec![(rid, hints)]);
    my.set_enabled(true);
    let s = tokio::time::timeout(Duration::from_secs(20), my.open_stream(tep.id(), ECHO_ALPN)).await.unwrap().unwrap();
    assert_eq!(s.request(b"hint", 1024).await.unwrap(), b"echo:hint");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identity_is_never_a_hop() {
    let n = net(3).await;
    let client = node(&n.lookup).await;
    // «основной узел» приложения сам релей из bootstrap — в собственную цепочку попасть не должен
    let main_sk = n.relays[0].0.secret_key().clone();
    let cfg = Config { bootstrap: n.relays.iter().map(|r| r.0.id()).collect(), data_dir: tempdir(), ..Config::default() };
    let my = Mycelium::start(client.clone(), None, cfg, vec![]).await.unwrap();
    my.set_identity(main_sk.clone());
    my.set_enabled(true);
    for _ in 0..100 {
        if my.status().circuits_ready >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for c in my.circuits() {
        assert!(!c.path.contains(&main_sk.public().to_string()), "{:?}", c.path);
    }
    assert_eq!(my.status().relays_usable, 2);
}

