//! Релей Mycelium (NIP-1 §5.3): принимает кадры по ALPN `netfory/mycelium/1`, снимает свой
//! слой, обслуживает EXTEND (строит следующий хоп), BEGIN/DATA/END (Exit к целевому узлу),
//! пересылает чужие ячейки дальше и добавляет свой слой на обратном пути.
use crate::cell::{self, frame_cmd, relay_cmd, RelayCell, BODY, CELL, TAG};
use crate::crypto::{self, HopKeys};
use crate::directory::{ExitPolicy, RelayLimits};
use anyhow::{anyhow, Result};
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

type Tx = mpsc::UnboundedSender<Vec<u8>>;

/// Счётчики релея для панели статуса.
#[derive(Default)]
pub struct RelayMetrics {
    pub circuits_total: AtomicU64,
    pub circuits_active: AtomicU32,
    pub cells_relayed: AtomicU64,
    pub bytes_relayed: AtomicU64,
    pub exit_streams: AtomicU64,
    /// Фаза 2: обслуживаемые intro-регистрации и сшитые rendezvous-пары.
    pub intro_active: AtomicU32,
    pub rendezvous_total: AtomicU64,
}

/// Token bucket на весь релей (байт/с).
pub struct Limiter {
    bytes_per_sec: AtomicU64,
    state: Mutex<(f64, std::time::Instant)>,
}

impl Limiter {
    pub fn new(bps: u64) -> Self {
        Self { bytes_per_sec: AtomicU64::new(bps), state: Mutex::new((bps as f64, std::time::Instant::now())) }
    }
    pub fn set(&self, bps: u64) {
        self.bytes_per_sec.store(bps, Ordering::Relaxed);
    }
    pub async fn take(&self, n: usize) {
        loop {
            let bps = self.bytes_per_sec.load(Ordering::Relaxed) as f64;
            if bps <= 0.0 {
                return;
            }
            let wait = {
                let mut s = self.state.lock().unwrap();
                let now = std::time::Instant::now();
                s.0 = (s.0 + now.duration_since(s.1).as_secs_f64() * bps).min(bps);
                s.1 = now;
                if s.0 >= n as f64 {
                    s.0 -= n as f64;
                    None
                } else {
                    Some(std::time::Duration::from_secs_f64((n as f64 - s.0) / bps))
                }
            };
            match wait {
                None => return,
                Some(d) => tokio::time::sleep(d).await,
            }
        }
    }
}

pub struct RelayState {
    pub endpoint: Endpoint,
    pub secret: iroh::SecretKey,
    pub policy: Mutex<ExitPolicy>,
    pub limits: Mutex<RelayLimits>,
    pub limiter: Limiter,
    pub metrics: RelayMetrics,
    /// Выкл — входящие цепочки отвергаются (режим «только клиент»).
    pub enabled: AtomicBool,
    /// Фаза 2: intro-точки (владелец → его цепочка) и ожидающие rendezvous (cookie → цепочка A).
    pub intro: Mutex<HashMap<EndpointId, Peer>>,
    pub rendezvous: Mutex<HashMap<[u8; cell::COOKIE_LEN], Peer>>,
    /// Директория клиента на этом узле — подсказки адреса для EXTEND к следующему хопу.
    pub dir: Mutex<Option<crate::directory::Directory>>,
}

/// Цепочка на этом релее с её обратным каналом к клиенту.
#[derive(Clone)]
pub struct Peer {
    circ: Arc<Mutex<Circ>>,
    cid: u32,
    tx: Tx,
}

/// Куда уходит нераспознанная (не наша) ячейка: следующему хопу или в сшитую цепочку (RP).
#[derive(Clone)]
enum Next {
    Hop(Tx, u32),
    Join(Peer),
}

#[derive(Clone)]
pub struct RelayHandler(pub Arc<RelayState>);

impl std::fmt::Debug for RelayHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayHandler").field("enabled", &self.0.enabled.load(Ordering::Relaxed)).finish()
    }
}

impl RelayHandler {
    pub fn new(endpoint: Endpoint, policy: ExitPolicy, limits: RelayLimits, enabled: bool) -> Self {
        let secret = endpoint.secret_key().clone();
        Self(Arc::new(RelayState {
            endpoint,
            secret,
            limiter: Limiter::new(limits.bandwidth),
            policy: Mutex::new(policy),
            limits: Mutex::new(limits),
            metrics: RelayMetrics::default(),
            enabled: AtomicBool::new(enabled),
            intro: Mutex::new(HashMap::new()),
            rendezvous: Mutex::new(HashMap::new()),
            dir: Mutex::new(None),
        }))
    }
}

impl iroh::protocol::ProtocolHandler for RelayHandler {
    async fn accept(&self, conn: Connection) -> Result<(), iroh::protocol::AcceptError> {
        if !self.0.enabled.load(Ordering::Relaxed) {
            conn.close(1u32.into(), b"relay disabled");
            return Ok(());
        }
        let st = self.0.clone();
        while let Ok((send, recv)) = conn.accept_bi().await {
            let st = st.clone();
            tokio::spawn(async move {
                if let Err(e) = serve_link(st, send, recv).await {
                    tracing::debug!("mycelium link: {e}");
                }
            });
        }
        Ok(())
    }
}

struct Circ {
    keys: HopKeys,
    hop: u8,
    next: Option<Next>,
    exit_streams: HashMap<u16, Tx>,
    /// Владелец intro-регистрации на этой цепочке (снимается при DESTROY).
    intro_owner: Option<EndpointId>,
}

/// Один QUIC-поток от предыдущего хопа/клиента: на нём живут цепочки с разными circ_id.
async fn serve_link(st: Arc<RelayState>, mut send: iroh::endpoint::SendStream, mut recv: iroh::endpoint::RecvStream) -> Result<()> {
    let (prev_tx, mut prev_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let st_w = st.clone();
    tokio::spawn(async move {
        while let Some(f) = prev_rx.recv().await {
            st_w.limiter.take(f.len()).await;
            if send.write_all(&f).await.is_err() {
                break;
            }
        }
    });
    let circs: Arc<Mutex<HashMap<u32, Arc<Mutex<Circ>>>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut buf = vec![0u8; CELL];
    let result = loop {
        if recv.read_exact(&mut buf).await.is_err() {
            break Ok(());
        }
        let Some((cid, cmd, body)) = cell::parse_frame(&buf) else { break Err(anyhow!("malformed frame")) };
        match cmd {
            frame_cmd::CREATE => {
                let max = st.limits.lock().unwrap().max_circuits;
                if st.metrics.circuits_active.load(Ordering::Relaxed) >= max {
                    let _ = prev_tx.send(cell::frame(cid, frame_cmd::DESTROY, &[]));
                    continue;
                }
                match crypto::relay_respond(&st.secret, body) {
                    Ok((keys, created)) => {
                        let hop = body[0];
                        circs.lock().unwrap().insert(cid, Arc::new(Mutex::new(Circ { keys, hop, next: None, exit_streams: HashMap::new(), intro_owner: None })));
                        st.metrics.circuits_total.fetch_add(1, Ordering::Relaxed);
                        st.metrics.circuits_active.fetch_add(1, Ordering::Relaxed);
                        let _ = prev_tx.send(cell::frame(cid, frame_cmd::CREATED, &created));
                    }
                    Err(_) => {
                        let _ = prev_tx.send(cell::frame(cid, frame_cmd::DESTROY, &[]));
                    }
                }
            }
            frame_cmd::DESTROY => {
                if let Some(c) = circs.lock().unwrap().remove(&cid) {
                    st.metrics.circuits_active.fetch_sub(1, Ordering::Relaxed);
                    teardown(&st, &c);
                }
            }
            frame_cmd::RELAY => {
                let Some(c) = circs.lock().unwrap().get(&cid).cloned() else { continue };
                st.metrics.cells_relayed.fetch_add(1, Ordering::Relaxed);
                st.metrics.bytes_relayed.fetch_add(CELL as u64, Ordering::Relaxed);
                if let Err(e) = handle_relay(&st, &c, cid, body, &prev_tx).await {
                    tracing::debug!("circ {cid}: {e}");
                    circs.lock().unwrap().remove(&cid);
                    st.metrics.circuits_active.fetch_sub(1, Ordering::Relaxed);
                    teardown(&st, &c);
                    let _ = prev_tx.send(cell::frame(cid, frame_cmd::DESTROY, &[]));
                }
            }
            _ => {}
        }
    };
    let left: Vec<Arc<Mutex<Circ>>> = circs.lock().unwrap().drain().map(|(_, c)| c).collect();
    if !left.is_empty() {
        st.metrics.circuits_active.fetch_sub(left.len() as u32, Ordering::Relaxed);
    }
    for c in &left {
        teardown(&st, c);
    }
    result
}

/// Цепочка закрыта: DESTROY следующему хопу, сшитой паре — CLOSE, intro-регистрация снимается.
fn teardown(st: &Arc<RelayState>, c: &Arc<Mutex<Circ>>) {
    let (next, owner) = {
        let mut g = c.lock().unwrap();
        (g.next.take(), g.intro_owner.take())
    };
    match next {
        Some(Next::Hop(tx, ncid)) => {
            let _ = tx.send(cell::frame(ncid, frame_cmd::DESTROY, &[]));
        }
        Some(Next::Join(p)) => {
            p.circ.lock().unwrap().next = None;
            backward(&mut p.circ.lock().unwrap(), p.cid, RelayCell::new(relay_cmd::CLOSE, 0, Vec::new()), &p.tx);
        }
        None => {}
    }
    if let Some(o) = owner {
        if st.intro.lock().unwrap().remove(&o).is_some() {
            st.metrics.intro_active.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Обратная ячейка от этого хопа клиенту. Шифрование и постановка в очередь — под одним
/// замком Circ: иначе параллельные потоки перемешают порядок счётчиков nonce и клиент оборвёт цепочку.
fn backward(c: &mut Circ, cid: u32, cell_: RelayCell, tx: &Tx) -> bool {
    let hop = c.hop as usize;
    let plain = cell_.encode(cell::plain_capacity(hop));
    let body = c.keys.seal_bwd(&plain);
    tx.send(cell::frame(cid, frame_cmd::RELAY, &body)).is_ok()
}

async fn handle_relay(st: &Arc<RelayState>, c: &Arc<Mutex<Circ>>, cid: u32, body: &[u8], prev_tx: &Tx) -> Result<()> {
    let (plain, hop, cell_opt) = {
        let mut g = c.lock().unwrap();
        let take = BODY - TAG * (g.hop as usize - 1);
        let plain = g.keys.open_fwd(&body[..take])?;
        let cell_opt = RelayCell::decode(&plain);
        (plain, g.hop, cell_opt)
    };
    let Some(rc) = cell_opt else {
        // не наша — дальше по цепочке (дополняем до полного тела) или в сшитую цепочку (RP:
        // сквозной слой A↔B для нас непрозрачен — отдаём назад партнёру под его ключами)
        let next = c.lock().unwrap().next.clone();
        match next {
            Some(Next::Hop(tx, ncid)) => {
                let _ = tx.send(cell::frame(ncid, frame_cmd::RELAY, &plain));
            }
            Some(Next::Join(p)) => {
                let body = p.circ.lock().unwrap().keys.seal_bwd(&plain[..cell::plain_capacity(hop as usize)]);
                let _ = p.tx.send(cell::frame(p.cid, frame_cmd::RELAY, &body));
            }
            None => return Err(anyhow!("unrecognised cell and no next hop")),
        }
        return Ok(());
    };
    match rc.cmd {
        relay_cmd::PADDING => {}
        relay_cmd::ESTABLISH_INTRO => {
            if rc.data.len() < 96 {
                return Err(anyhow!("ESTABLISH_INTRO: body too short"));
            }
            let owner = EndpointId::from_bytes(rc.data[..32].try_into().unwrap()).map_err(|_| anyhow!("ESTABLISH_INTRO: bad owner"))?;
            let sig = iroh::Signature::from_bytes(rc.data[32..96].try_into().unwrap());
            if owner.verify(&intro_msg(&owner, &st.endpoint.id()), &sig).is_err() {
                backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::ERROR, 0, b"intro sig".to_vec()), prev_tx);
                return Ok(());
            }
            c.lock().unwrap().intro_owner = Some(owner);
            if st.intro.lock().unwrap().insert(owner, Peer { circ: c.clone(), cid, tx: prev_tx.clone() }).is_none() {
                st.metrics.intro_active.fetch_add(1, Ordering::Relaxed);
            }
            backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::INTRO_ESTABLISHED, 0, Vec::new()), prev_tx);
        }
        relay_cmd::INTRODUCE1 => {
            if rc.data.len() < 32 + 32 + cell::COOKIE_LEN + 32 {
                return Err(anyhow!("INTRODUCE1: body too short"));
            }
            let owner = EndpointId::from_bytes(rc.data[..32].try_into().unwrap()).map_err(|_| anyhow!("INTRODUCE1: bad owner"))?;
            let peer = st.intro.lock().unwrap().get(&owner).cloned();
            match peer {
                Some(p) => {
                    backward(&mut p.circ.lock().unwrap(), p.cid, RelayCell::new(relay_cmd::INTRODUCE2, 0, rc.data[32..].to_vec()), &p.tx);
                    backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::INTRODUCE_ACK, 0, Vec::new()), prev_tx);
                }
                None => {
                    backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::ERROR, 0, b"no intro".to_vec()), prev_tx);
                }
            }
        }
        relay_cmd::ESTABLISH_RENDEZVOUS => {
            let cookie: [u8; cell::COOKIE_LEN] = rc.data.get(..cell::COOKIE_LEN).and_then(|c| c.try_into().ok()).ok_or_else(|| anyhow!("ESTABLISH_RENDEZVOUS: cookie"))?;
            let mut rv = st.rendezvous.lock().unwrap();
            if rv.len() >= 1024 {
                rv.retain(|_, p| !p.tx.is_closed());
            }
            rv.insert(cookie, Peer { circ: c.clone(), cid, tx: prev_tx.clone() });
            drop(rv);
            backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::RENDEZVOUS_ESTABLISHED, 0, Vec::new()), prev_tx);
        }
        relay_cmd::RENDEZVOUS1 => {
            let cookie: [u8; cell::COOKIE_LEN] = rc.data.get(..cell::COOKIE_LEN).and_then(|c| c.try_into().ok()).ok_or_else(|| anyhow!("RENDEZVOUS1: cookie"))?;
            let a = st.rendezvous.lock().unwrap().remove(&cookie);
            match a {
                Some(a) if Arc::ptr_eq(&a.circ, c) => return Err(anyhow!("RENDEZVOUS1: same circuit")),
                Some(a) if a.circ.lock().unwrap().hop != hop => {
                    backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::ERROR, 0, b"hop count mismatch".to_vec()), prev_tx);
                }
                Some(a) => {
                    let me = Peer { circ: c.clone(), cid, tx: prev_tx.clone() };
                    a.circ.lock().unwrap().next = Some(Next::Join(me));
                    c.lock().unwrap().next = Some(Next::Join(a.clone()));
                    st.metrics.rendezvous_total.fetch_add(1, Ordering::Relaxed);
                    backward(&mut a.circ.lock().unwrap(), a.cid, RelayCell::new(relay_cmd::RENDEZVOUS2, 0, rc.data[cell::COOKIE_LEN..].to_vec()), &a.tx);
                }
                None => {
                    backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::ERROR, 0, b"no rendezvous".to_vec()), prev_tx);
                }
            }
        }
        relay_cmd::PING => {
            backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::PONG, 0, Vec::new()), prev_tx);
        }
        relay_cmd::EXTEND => {
            if c.lock().unwrap().next.is_some() || rc.data.len() < 65 {
                return Err(anyhow!("EXTEND: already extended / body too short"));
            }
            let next_hop = rc.data[0];
            let node = EndpointId::from_bytes(rc.data[1..33].try_into().unwrap()).map_err(|_| anyhow!("EXTEND: bad node"))?;
            let mut create = vec![next_hop];
            create.extend_from_slice(&rc.data[33..65]);
            match extend_to(st, node, create).await {
                Ok((tx, ncid, created, recv)) => {
                    // обратный поток от следующего хопа: добавляем свой слой и отдаём назад
                    c.lock().unwrap().next = Some(Next::Hop(tx, ncid));
                    spawn_next_reader(c.clone(), cid, recv, prev_tx.clone());
                    backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::EXTENDED, 0, created), prev_tx);
                }
                Err(e) => {
                    tracing::debug!("EXTEND → {node}: {e}");
                    backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::ERROR, 0, vec![1u8]), prev_tx);
                }
            }
        }
        relay_cmd::BEGIN => {
            let data = &rc.data;
            let alen = *data.first().ok_or_else(|| anyhow!("BEGIN: empty"))? as usize;
            if data.len() < 1 + alen + 32 {
                return Err(anyhow!("BEGIN: body too short"));
            }
            let alpn = data[1..1 + alen].to_vec();
            let target = EndpointId::from_bytes(data[1 + alen..1 + alen + 32].try_into().unwrap()).map_err(|_| anyhow!("BEGIN: bad target"))?;
            if !st.policy.lock().unwrap().allows(&alpn) {
                backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::ERROR, rc.stream, b"exit policy".to_vec()), prev_tx);
                return Ok(());
            }
            match begin_exit(st, c.clone(), cid, rc.stream, target, &alpn, prev_tx.clone()).await {
                Ok(()) => {
                    st.metrics.exit_streams.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::ERROR, rc.stream, e.to_string().into_bytes()), prev_tx);
                }
            }
        }
        relay_cmd::DATA | relay_cmd::END => {
            let tx = c.lock().unwrap().exit_streams.get(&rc.stream).cloned();
            if let Some(tx) = tx {
                if rc.cmd == relay_cmd::END {
                    c.lock().unwrap().exit_streams.remove(&rc.stream);
                    drop(tx); // закрытие канала → writer делает finish()
                } else {
                    let _ = tx.send(rc.data);
                }
            }
        }
        relay_cmd::CLOSE => return Err(anyhow!("CLOSE from client")),
        _ => {}
    }
    Ok(())
}

/// Что подписывает владелец при ESTABLISH_INTRO: привязка к конкретному intro-релею.
pub fn intro_msg(owner: &EndpointId, relay: &EndpointId) -> Vec<u8> {
    let mut m = b"netfory/mycelium/1 intro|".to_vec();
    m.extend_from_slice(owner.as_bytes());
    m.extend_from_slice(relay.as_bytes());
    m
}

/// Таймаут каждого шага extend (connect, CREATED): 8+8 с < EXTEND_TIMEOUT клиента (26 с).
const EXTEND_HOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Построить следующий хоп: соединение по ALPN, CREATE → CREATED; читатель обратных кадров.
async fn extend_to(st: &Arc<RelayState>, node: EndpointId, create: Vec<u8>) -> Result<(Tx, u32, Vec<u8>, iroh::endpoint::RecvStream)> {
    let addr = st.dir.lock().unwrap().as_ref().map(|d| d.addr_of(node)).unwrap_or_else(|| iroh::EndpointAddr::new(node));
    let conn = tokio::time::timeout(EXTEND_HOP_TIMEOUT, st.endpoint.connect(addr, crate::circuit::ALPN)).await.map_err(|_| anyhow!("timeout"))??;
    let (mut send, mut recv) = conn.open_bi().await?;
    let ncid: u32 = rand::random::<u32>() & 0x7fff_ffff;
    send.write_all(&cell::frame(ncid, frame_cmd::CREATE, &create)).await?;
    let mut f = vec![0u8; CELL];
    tokio::time::timeout(EXTEND_HOP_TIMEOUT, recv.read_exact(&mut f)).await.map_err(|_| anyhow!("CREATED timeout"))??;
    let (rcid, cmd, created) = cell::parse_frame(&f).ok_or_else(|| anyhow!("malformed CREATED"))?;
    if rcid != ncid || cmd != frame_cmd::CREATED {
        return Err(anyhow!("next hop refused"));
    }
    let created = created[..crypto::CREATED_LEN].to_vec();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let st_w = st.clone();
    tokio::spawn(async move {
        while let Some(fr) = rx.recv().await {
            st_w.limiter.take(fr.len()).await;
            if send.write_all(&fr).await.is_err() {
                break;
            }
        }
        let _ = send.finish();
    });
    Ok((tx, ncid, created, recv))
}

/// Читатель обратных кадров от следующего хопа — запускается после EXTENDED (нужны prev_tx и Circ).
fn spawn_next_reader(c: Arc<Mutex<Circ>>, cid: u32, mut recv: iroh::endpoint::RecvStream, prev_tx: Tx) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; CELL];
        while recv.read_exact(&mut buf).await.is_ok() {
            let Some((_, cmd, body)) = cell::parse_frame(&buf) else { break };
            match cmd {
                frame_cmd::RELAY => {
                    let mut g = c.lock().unwrap();
                    let take = BODY - TAG * g.hop as usize;
                    let layered = g.keys.seal_bwd(&body[..take]);
                    if prev_tx.send(cell::frame(cid, frame_cmd::RELAY, &layered)).is_err() {
                        break;
                    }
                }
                frame_cmd::DESTROY => {
                    let _ = prev_tx.send(cell::frame(cid, frame_cmd::DESTROY, &[]));
                    break;
                }
                _ => {}
            }
        }
    });
}

/// Exit: соединение с целевым узлом по запрошенному ALPN; данные потока ↔ DATA-ячейки.
async fn begin_exit(st: &Arc<RelayState>, c: Arc<Mutex<Circ>>, cid: u32, sid: u16, target: EndpointId, alpn: &[u8], prev_tx: Tx) -> Result<()> {
    let conn = tokio::time::timeout(std::time::Duration::from_secs(12), st.endpoint.connect(target, alpn)).await.map_err(|_| anyhow!("target connect timeout"))??;
    let (mut send, mut recv) = conn.open_bi().await?;
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    c.lock().unwrap().exit_streams.insert(sid, tx);
    tokio::spawn(async move {
        while let Some(d) = rx.recv().await {
            if send.write_all(&d).await.is_err() {
                break;
            }
        }
        let _ = send.finish();
    });
    // CONNECTED — до запуска читателя цели, чтобы DATA не обогнала его
    backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::CONNECTED, sid, Vec::new()), &prev_tx);
    let hop = c.lock().unwrap().hop as usize;
    let st2 = st.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; cell::data_capacity(hop)];
        loop {
            match recv.read(&mut buf).await {
                Ok(Some(n)) if n > 0 => {
                    st2.metrics.bytes_relayed.fetch_add(n as u64, Ordering::Relaxed);
                    if !backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::DATA, sid, buf[..n].to_vec()), &prev_tx) {
                        break;
                    }
                }
                _ => break,
            }
        }
        backward(&mut c.lock().unwrap(), cid, RelayCell::new(relay_cmd::END, sid, Vec::new()), &prev_tx);
        c.lock().unwrap().exit_streams.remove(&sid);
    });
    Ok(())
}
