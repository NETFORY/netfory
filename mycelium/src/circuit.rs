//! Клиентская цепочка (NIP-1 §5): телескопическое построение Guard → Middle → Exit, луковое
//! шифрование ячеек, логические потоки внутри цепочки и `MushroomStream` (AsyncRead + AsyncWrite).
use crate::cell::{self, frame_cmd, relay_cmd, RelayCell, BODY, CELL, COOKIE_LEN, RV_HOP, TAG};
use crate::crypto::{self, HopKeys};
use anyhow::{anyhow, bail, Result};
use iroh::{Endpoint, EndpointId};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::{mpsc, oneshot};

pub const ALPN: &[u8] = b"netfory/mycelium/1";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// EXTENDED: релей сам ждёт следующий хоп до 8+8 с — клиент ждёт чуть дольше релея.
const EXTEND_TIMEOUT: Duration = Duration::from_secs(20);
/// PING без PONG дольше этого — хоп считается мёртвым (участок карты горит красным).
pub const HOP_DEAD_AFTER: Duration = Duration::from_secs(10);
/// Участок (RTT(k) − RTT(k−1)) медленнее этого — повод для автоматической пересборки.
pub const SEGMENT_SLOW_MS: u32 = 1500;

/// Назначение цепочки для карты: обычный Exit, intro-цепочка получателя, rendezvous (A) или
/// обслуживание скрытого приёма (B).
pub const KIND_EXIT: u8 = 0;
pub const KIND_INTRO: u8 = 1;
pub const KIND_RENDEZVOUS: u8 = 2;
pub const KIND_SERVICE: u8 = 3;
pub fn kind_name(k: u8) -> &'static str {
    match k {
        KIND_INTRO => "intro",
        KIND_RENDEZVOUS => "rendezvous",
        KIND_SERVICE => "service",
        _ => "exit",
    }
}
/// Входящий запрос скрытого приёма (B): (stream, ALPN, тело).
pub type ServiceReq = (u16, Vec<u8>, Vec<u8>);

enum StreamEv {
    Connected,
    Data(Vec<u8>),
    End,
    Error(String),
}

pub struct Circuit {
    pub id: u32,
    pub path: Vec<EndpointId>,
    pub created: Instant,
    hops: Mutex<Vec<HopKeys>>,
    out_tx: mpsc::UnboundedSender<Vec<u8>>,
    streams: Mutex<HashMap<u16, mpsc::UnboundedSender<StreamEv>>>,
    extend_waiters: Mutex<HashMap<u8, oneshot::Sender<Vec<u8>>>>,
    next_stream: AtomicU16,
    closed: AtomicBool,
    pub cells_out: AtomicU64,
    pub cells_in: AtomicU64,
    /// RTT до каждого хопа (мс) по PING/PONG; индекс = номер хопа − 1.
    hop_rtt: Mutex<Vec<Option<u32>>>,
    /// Хоп отвечает на PING (false — нет PONG дольше `HOP_DEAD_AFTER`).
    hop_alive: Mutex<Vec<bool>>,
    ping_sent: Mutex<HashMap<u8, Instant>>,
    last_probe: Mutex<Option<Instant>>,
    /// Время последнего запроса-ответа до собеседника через цепочку (мс, когда).
    e2e: Mutex<Option<(u32, Instant)>>,
    /// Сколько раз подряд эта цепочка (в своём слоте) пересобиралась автоматически.
    retries: AtomicU32,
    /// Фаза 2: сквозной слой A↔B поверх rendezvous (виртуальный хоп 4); bool — мы клиент (A).
    rv: Mutex<Option<(HopKeys, bool)>>,
    /// Ответы на управляющие команды (INTRO_ESTABLISHED, RENDEZVOUS_ESTABLISHED, RENDEZVOUS2, INTRODUCE_ACK).
    ctl: Mutex<HashMap<u8, oneshot::Sender<Result<Vec<u8>>>>>,
    /// INTRODUCE2 с intro-релея (B).
    intro_tx: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    /// Входящие потоки скрытого приёма (B): stream → (ALPN, накопленный запрос).
    inbound: Mutex<HashMap<u16, (Vec<u8>, Vec<u8>)>>,
    svc_tx: Mutex<Option<mpsc::UnboundedSender<ServiceReq>>>,
    kind: AtomicU8,
}

/// Снимок цепочки для UI-карты.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CircuitInfo {
    pub id: u32,
    pub path: Vec<String>,
    /// RTT от клиента до хопа (накопительно), мс.
    pub hop_rtt_ms: Vec<Option<u32>>,
    /// Хоп отвечает на PING; false — участок до него мёртв.
    pub hop_alive: Vec<bool>,
    pub target: Option<String>,
    pub e2e_ms: Option<u32>,
    pub e2e_age_secs: Option<u64>,
    pub age_secs: u64,
    pub cells_in: u64,
    pub cells_out: u64,
    /// Автоматических пересборок подряд в этом слоте (лимит — `AUTO_REBUILD_MAX`).
    pub retries: u32,
    /// Сейчас строится замена этой цепочке.
    pub rebuilding: bool,
    /// Мёртвый хоп или слишком медленный участок — кандидат на пересборку.
    pub degraded: bool,
    /// exit | intro | rendezvous | service.
    pub kind: &'static str,
}

impl Circuit {
    /// Построить цепочку по `path` (1..=3 узла): CREATE с первым, затем EXTEND через предыдущие.
    pub async fn build(endpoint: &Endpoint, path: Vec<EndpointId>) -> Result<Arc<Circuit>> {
        let first = path.first().map(|n| iroh::EndpointAddr::new(*n));
        Self::build_via(endpoint, path, first).await
    }

    /// Как `build`, но к guard — по адресу с подсказками (прямые ip:port / реле).
    pub async fn build_via(endpoint: &Endpoint, path: Vec<EndpointId>, first: Option<iroh::EndpointAddr>) -> Result<Arc<Circuit>> {
        if path.is_empty() || path.len() > cell::MAX_HOPS {
            bail!("path must contain 1..=3 relays");
        }
        let first = first.unwrap_or_else(|| iroh::EndpointAddr::new(path[0]));
        let conn = tokio::time::timeout(HANDSHAKE_TIMEOUT, endpoint.connect(first, ALPN)).await.map_err(|_| anyhow!("guard: connect timeout"))?.map_err(|e| anyhow!("guard: {e}"))?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| anyhow!("guard: {e}"))?;
        let id: u32 = rand::random::<u32>() | 0x8000_0000;
        // хоп 1: CREATE/CREATED напрямую по QUIC-потоку
        let (sk, c_e) = crypto::client_begin();
        let mut body = vec![1u8];
        body.extend_from_slice(&c_e);
        send.write_all(&cell::frame(id, frame_cmd::CREATE, &body)).await.map_err(|e| anyhow!("guard: {e}"))?;
        let mut f = vec![0u8; CELL];
        tokio::time::timeout(HANDSHAKE_TIMEOUT, recv.read_exact(&mut f)).await.map_err(|_| anyhow!("guard: CREATED timeout"))?.map_err(|e| anyhow!("guard: {e}"))?;
        let (cid, cmd, created) = cell::parse_frame(&f).ok_or_else(|| anyhow!("guard: malformed frame"))?;
        if cid != id || cmd != frame_cmd::CREATED {
            bail!("guard: expected CREATED");
        }
        let keys = crypto::client_finish(sk, &c_e, 1, &path[0], created)?;
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let circ = Arc::new(Circuit {
            id,
            path: path.clone(),
            created: Instant::now(),
            hops: Mutex::new(vec![keys]),
            out_tx,
            streams: Mutex::new(HashMap::new()),
            extend_waiters: Mutex::new(HashMap::new()),
            next_stream: AtomicU16::new(1),
            closed: AtomicBool::new(false),
            cells_out: AtomicU64::new(0),
            cells_in: AtomicU64::new(0),
            hop_rtt: Mutex::new(vec![None; path.len()]),
            hop_alive: Mutex::new(vec![true; path.len()]),
            ping_sent: Mutex::new(HashMap::new()),
            last_probe: Mutex::new(None),
            e2e: Mutex::new(None),
            retries: AtomicU32::new(0),
            rv: Mutex::new(None),
            ctl: Mutex::new(HashMap::new()),
            intro_tx: Mutex::new(None),
            inbound: Mutex::new(HashMap::new()),
            svc_tx: Mutex::new(None),
            kind: AtomicU8::new(KIND_EXIT),
        });
        // писатель к guard
        let c_w = circ.clone();
        tokio::spawn(async move {
            while let Some(fr) = out_rx.recv().await {
                // DESTROY — последний кадр цепочки: дальше писатель не нужен (иначе висит навсегда)
                let last = cell::parse_frame(&fr).is_some_and(|(_, c, _)| c == frame_cmd::DESTROY);
                if send.write_all(&fr).await.is_err() || last {
                    break;
                }
            }
            let _ = send.finish();
            c_w.mark_closed();
        });
        // читатель от guard
        let c_r = circ.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; CELL];
            loop {
                if recv.read_exact(&mut buf).await.is_err() {
                    break;
                }
                if c_r.handle_frame(&buf).is_err() {
                    break;
                }
            }
            c_r.close(); // DESTROY в очередь → писатель завершится
            conn.close(0u32.into(), b"circuit closed");
        });
        // хопы 2..: EXTEND через уже построенную часть
        // недостроенная цепочка (ошибка ИЛИ отмена future по таймауту вызывающего) закрывается в Drop,
        // иначе reader/writer и соединение к guard висят до idle-timeout QUIC
        let mut guard = CloseOnDrop(Some(circ.clone()));
        for (i, node) in path.iter().enumerate().skip(1) {
            circ.extend(*node, (i + 1) as u8).await?;
        }
        guard.0 = None;
        Ok(circ)
    }

    async fn extend(&self, node: EndpointId, hop: u8) -> Result<()> {
        let (sk, c_e) = crypto::client_begin();
        let mut data = vec![hop];
        data.extend_from_slice(node.as_bytes());
        data.extend_from_slice(&c_e);
        let (tx, rx) = oneshot::channel();
        self.extend_waiters.lock().unwrap().insert(hop, tx);
        // EXTEND адресован последнему построенному хопу (0-based индекс hop-2)
        self.send_relay((hop - 2) as usize, RelayCell::new(relay_cmd::EXTEND, 0, data)).map_err(|e| anyhow!("hop {hop}: {e}"))?;
        let created = tokio::time::timeout(EXTEND_TIMEOUT, rx).await.map_err(|_| anyhow!("hop {hop}: EXTENDED timeout"))?.map_err(|_| anyhow!("hop {hop}: circuit closed"))?;
        if created.len() == 1 {
            bail!("hop {hop}: relay refused (code {}: previous hop could not reach this relay)", created[0]);
        }
        let keys = crypto::client_finish(sk, &c_e, hop, &node, &created)?;
        self.hops.lock().unwrap().push(keys);
        Ok(())
    }

    pub fn hop_count(&self) -> usize {
        self.hops.lock().unwrap().len()
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
    pub fn needs_rotation(&self) -> bool {
        self.hops.lock().unwrap().iter().any(|h| h.needs_rotation())
    }
    fn mark_closed(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let streams: Vec<_> = self.streams.lock().unwrap().drain().map(|(_, tx)| tx).collect();
            for tx in streams {
                let _ = tx.send(StreamEv::End);
            }
            self.extend_waiters.lock().unwrap().clear();
            self.ctl.lock().unwrap().clear();
            *self.intro_tx.lock().unwrap() = None;
            *self.svc_tx.lock().unwrap() = None;
        }
    }
    pub fn kind(&self) -> u8 {
        self.kind.load(Ordering::Relaxed)
    }
    pub fn set_kind(&self, k: u8) {
        self.kind.store(k, Ordering::Relaxed);
    }
    pub fn has_rendezvous(&self) -> bool {
        self.rv.lock().unwrap().is_some()
    }

    /// Ячейка в сквозной слой A↔B: виртуальный хоп 4 поверх `n` реальных.
    fn send_relay_rv(&self, c: RelayCell) -> Result<()> {
        if self.is_closed() {
            bail!("circuit closed");
        }
        let mut hops = self.hops.lock().unwrap();
        let n = hops.len();
        let mut rv = self.rv.lock().unwrap();
        let Some((k, client)) = rv.as_mut() else { bail!("no rendezvous layer") };
        let inner = c.encode(cell::plain_capacity(n + 1));
        let mut body = if *client { k.seal_fwd(&inner) } else { k.seal_bwd(&inner) };
        for j in (0..n).rev() {
            body = hops[j].seal_fwd(&body);
        }
        self.cells_out.fetch_add(1, Ordering::Relaxed);
        self.out_tx.send(cell::frame(self.id, frame_cmd::RELAY, &body)).map_err(|_| anyhow!("writer closed"))
    }

    /// Управляющая команда последнему хопу с ожиданием ответа `reply`.
    async fn control(&self, cmd: u8, data: Vec<u8>, reply: u8) -> Result<Vec<u8>> {
        let (tx, rx) = oneshot::channel();
        self.ctl.lock().unwrap().insert(reply, tx);
        self.send_relay(self.hop_count() - 1, RelayCell::new(cmd, 0, data))?;
        tokio::time::timeout(HANDSHAKE_TIMEOUT, rx).await.map_err(|_| anyhow!("timeout waiting for reply {reply}"))?.map_err(|_| anyhow!("circuit closed"))?
    }

    /// B: зарегистрироваться intro-точкой на последнем хопе (подпись владельца привязана к релею).
    pub async fn establish_intro(&self, secret: &iroh::SecretKey) -> Result<mpsc::UnboundedReceiver<Vec<u8>>> {
        let relay = *self.path.last().unwrap();
        let owner = secret.public();
        let mut data = owner.as_bytes().to_vec();
        data.extend_from_slice(&secret.sign(&crate::relay::intro_msg(&owner, &relay)).to_bytes());
        let (tx, rx) = mpsc::unbounded_channel();
        *self.intro_tx.lock().unwrap() = Some(tx);
        self.control(relay_cmd::ESTABLISH_INTRO, data, relay_cmd::INTRO_ESTABLISHED).await?;
        self.set_kind(KIND_INTRO);
        Ok(rx)
    }

    /// A: ESTABLISH_RENDEZVOUS на последнем хопе (RP); возвращает cookie.
    pub async fn establish_rendezvous(&self) -> Result<[u8; COOKIE_LEN]> {
        let mut cookie = [0u8; COOKIE_LEN];
        cell::random_fill(&mut cookie);
        self.control(relay_cmd::ESTABLISH_RENDEZVOUS, cookie.to_vec(), relay_cmd::RENDEZVOUS_ESTABLISHED).await?;
        Ok(cookie)
    }

    /// A: INTRODUCE1 через intro-цепочку: `[owner][RP][cookie][C_e]`; ждём INTRODUCE_ACK.
    pub async fn introduce(&self, owner: &EndpointId, rp: &EndpointId, cookie: &[u8; COOKIE_LEN], c_e: &[u8; 32]) -> Result<()> {
        let mut data = owner.as_bytes().to_vec();
        data.extend_from_slice(rp.as_bytes());
        data.extend_from_slice(cookie);
        data.extend_from_slice(c_e);
        self.control(relay_cmd::INTRODUCE1, data, relay_cmd::INTRODUCE_ACK).await.map(|_| ())
    }

    /// A: ждать RENDEZVOUS2 на RP-цепочке и завершить сквозное рукопожатие с B.
    pub async fn await_rendezvous(&self, sk: crypto::StaticSecret, c_e: &[u8; 32], owner: &EndpointId, wait: Duration) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.ctl.lock().unwrap().insert(relay_cmd::RENDEZVOUS2, tx);
        let created = tokio::time::timeout(wait, rx).await.map_err(|_| anyhow!("RENDEZVOUS2 timeout (peer did not arrive)"))?.map_err(|_| anyhow!("circuit closed"))??;
        let keys = crypto::client_finish(sk, c_e, RV_HOP, owner, &created)?;
        *self.rv.lock().unwrap() = Some((keys, true));
        self.set_kind(KIND_RENDEZVOUS);
        Ok(())
    }

    /// B: ответить на INTRODUCE2 через цепочку к RP: RENDEZVOUS1 `[cookie][CREATED]`; слой A↔B готов.
    pub fn rendezvous_answer(&self, secret: &iroh::SecretKey, cookie: &[u8; COOKIE_LEN], c_e: &[u8; 32], svc_tx: mpsc::UnboundedSender<ServiceReq>) -> Result<()> {
        let mut create = vec![RV_HOP];
        create.extend_from_slice(c_e);
        let (keys, created) = crypto::relay_respond(secret, &create)?;
        *self.rv.lock().unwrap() = Some((keys, false));
        *self.svc_tx.lock().unwrap() = Some(svc_tx);
        self.set_kind(KIND_SERVICE);
        let mut data = cookie.to_vec();
        data.extend_from_slice(&created);
        self.send_relay(self.hop_count() - 1, RelayCell::new(relay_cmd::RENDEZVOUS1, 0, data))
    }

    /// B: ответ на входящий запрос скрытого приёма — DATA-ячейки + END.
    pub fn service_respond(&self, sid: u16, resp: &[u8]) -> Result<()> {
        let cap = cell::data_capacity(self.hop_count() + 1);
        for chunk in resp.chunks(cap.max(1)) {
            self.send_relay_rv(RelayCell::new(relay_cmd::DATA, sid, chunk.to_vec()))?;
        }
        self.send_relay_rv(RelayCell::new(relay_cmd::END, sid, Vec::new()))
    }

    /// Маскирующая ячейка последнему хопу (или в сквозной слой, если он есть).
    pub fn send_padding(&self) -> Result<()> {
        if self.has_rendezvous() {
            self.send_relay_rv(RelayCell::new(relay_cmd::PADDING, 0, Vec::new()))
        } else {
            self.send_relay(self.hop_count() - 1, RelayCell::new(relay_cmd::PADDING, 0, Vec::new()))
        }
    }
    pub fn close(&self) {
        let _ = self.out_tx.send(cell::frame(self.id, frame_cmd::DESTROY, &[]));
        self.mark_closed();
    }

    /// Луковица для хопа `hop_idx` (0-based): шифруем его слой, затем слои предыдущих хопов.
    fn send_relay(&self, hop_idx: usize, c: RelayCell) -> Result<()> {
        if self.is_closed() {
            bail!("circuit closed");
        }
        let mut hops = self.hops.lock().unwrap();
        if hop_idx >= hops.len() {
            bail!("hop {hop_idx} not built yet");
        }
        let mut body = c.encode(cell::plain_capacity(hop_idx + 1));
        for j in (0..=hop_idx).rev() {
            body = hops[j].seal_fwd(&body);
        }
        debug_assert_eq!(body.len(), BODY);
        self.cells_out.fetch_add(1, Ordering::Relaxed);
        // в очередь под замком hops — порядок ячеек = порядок счётчиков nonce
        let r = self.out_tx.send(cell::frame(self.id, frame_cmd::RELAY, &body)).map_err(|_| anyhow!("writer closed"));
        drop(hops);
        r
    }

    /// Снимаем слои назад: первый хоп, чья ячейка распознана, — отправитель.
    fn handle_frame(&self, f: &[u8]) -> Result<()> {
        let (cid, cmd, body) = cell::parse_frame(f).ok_or_else(|| anyhow!("malformed frame"))?;
        if cid != self.id {
            return Ok(());
        }
        match cmd {
            frame_cmd::DESTROY => bail!("DESTROY from guard"),
            frame_cmd::RELAY => {}
            _ => return Ok(()),
        }
        self.cells_in.fetch_add(1, Ordering::Relaxed);
        let mut hops = self.hops.lock().unwrap();
        let mut plain = body[..BODY].to_vec();
        let mut found: Option<(u8, RelayCell)> = None;
        for (j, h) in hops.iter_mut().enumerate() {
            let take = BODY - TAG * j;
            plain = h.open_bwd(&plain[..take])?;
            if let Some(c) = RelayCell::decode(&plain) {
                found = Some(((j + 1) as u8, c));
                break;
            }
        }
        let n = hops.len();
        drop(hops);
        if found.is_none() {
            // сквозной слой A↔B поверх rendezvous
            let mut rv = self.rv.lock().unwrap();
            if let Some((k, client)) = rv.as_mut() {
                let cap = cell::plain_capacity(n);
                let inner = if *client { k.open_bwd(&plain[..cap])? } else { k.open_fwd(&plain[..cap])? };
                found = RelayCell::decode(&inner).map(|c| (RV_HOP, c));
            }
        }
        match found {
            Some((hop, c)) => {
                self.dispatch(hop, c);
                Ok(())
            }
            None => bail!("cell not recognised by any hop"),
        }
    }

    fn dispatch(&self, from_hop: u8, c: RelayCell) {
        match c.cmd {
            relay_cmd::PADDING => {}
            relay_cmd::INTRO_ESTABLISHED | relay_cmd::RENDEZVOUS_ESTABLISHED | relay_cmd::RENDEZVOUS2 | relay_cmd::INTRODUCE_ACK => {
                if let Some(tx) = self.ctl.lock().unwrap().remove(&c.cmd) {
                    let _ = tx.send(Ok(c.data));
                }
            }
            relay_cmd::INTRODUCE2 => {
                if let Some(tx) = self.intro_tx.lock().unwrap().clone() {
                    let _ = tx.send(c.data);
                }
            }
            // B: входящий поток скрытого приёма из сквозного слоя
            relay_cmd::BEGIN if from_hop == RV_HOP => {
                let alen = c.data.first().copied().unwrap_or(0) as usize;
                let alpn = c.data.get(1..1 + alen).unwrap_or_default().to_vec();
                self.inbound.lock().unwrap().insert(c.stream, (alpn, Vec::new()));
                let _ = self.send_relay_rv(RelayCell::new(relay_cmd::CONNECTED, c.stream, Vec::new()));
            }
            relay_cmd::DATA | relay_cmd::END if from_hop == RV_HOP && self.inbound.lock().unwrap().contains_key(&c.stream) => {
                if c.cmd == relay_cmd::DATA {
                    if let Some(e) = self.inbound.lock().unwrap().get_mut(&c.stream) {
                        e.1.extend_from_slice(&c.data);
                    }
                } else if let Some((alpn, req)) = self.inbound.lock().unwrap().remove(&c.stream) {
                    if let Some(tx) = self.svc_tx.lock().unwrap().as_ref() {
                        let _ = tx.send((c.stream, alpn, req));
                    }
                }
            }
            relay_cmd::EXTENDED | relay_cmd::ERROR if c.stream == 0 => {
                let hop = from_hop + 1;
                if let Some(tx) = self.extend_waiters.lock().unwrap().remove(&hop) {
                    let _ = tx.send(if c.cmd == relay_cmd::ERROR { vec![c.data.first().copied().unwrap_or(0)] } else { c.data });
                } else if c.cmd == relay_cmd::ERROR {
                    let msg = String::from_utf8_lossy(&c.data).into_owned();
                    for (_, tx) in self.ctl.lock().unwrap().drain() {
                        let _ = tx.send(Err(anyhow!("relay refused: {msg}")));
                    }
                }
            }
            relay_cmd::PONG => {
                if let Some(t0) = self.ping_sent.lock().unwrap().remove(&from_hop) {
                    let ms = t0.elapsed().as_millis().min(u32::MAX as u128) as u32;
                    if let Some(slot) = self.hop_rtt.lock().unwrap().get_mut(from_hop as usize - 1) {
                        *slot = Some(ms);
                    }
                    if let Some(a) = self.hop_alive.lock().unwrap().get_mut(from_hop as usize - 1) {
                        *a = true;
                    }
                }
            }
            relay_cmd::PING => {
                let _ = self.send_relay((from_hop - 1) as usize, RelayCell::new(relay_cmd::PONG, 0, Vec::new()));
            }
            relay_cmd::CLOSE => self.mark_closed(),
            relay_cmd::CONNECTED | relay_cmd::DATA | relay_cmd::END | relay_cmd::ERROR => {
                let tx = self.streams.lock().unwrap().get(&c.stream).cloned();
                if let Some(tx) = tx {
                    let ev = match c.cmd {
                        relay_cmd::CONNECTED => StreamEv::Connected,
                        relay_cmd::DATA => StreamEv::Data(c.data),
                        relay_cmd::END => StreamEv::End,
                        _ => StreamEv::Error(String::from_utf8_lossy(&c.data).into_owned()),
                    };
                    let _ = tx.send(ev);
                }
            }
            _ => {}
        }
    }

    /// PING каждому построенному хопу — PONG обновит `hop_rtt` (не чаще раза в `min_gap`).
    /// Хоп, не ответивший на предыдущий PING за `HOP_DEAD_AFTER`, помечается мёртвым.
    pub fn probe_hops(&self, min_gap: Duration) {
        {
            let mut lp = self.last_probe.lock().unwrap();
            if lp.is_some_and(|t| t.elapsed() < min_gap) {
                return;
            }
            *lp = Some(Instant::now());
        }
        for idx in 0..self.hop_count() {
            let hop = idx as u8 + 1;
            {
                let mut sent = self.ping_sent.lock().unwrap();
                // Some(true) — прежний PING завис; Some(false) — ещё в полёте (отметку не сбиваем)
                let stale = sent.get(&hop).map(|t| t.elapsed() >= HOP_DEAD_AFTER);
                if stale == Some(true) {
                    if let Some(a) = self.hop_alive.lock().unwrap().get_mut(idx) {
                        *a = false;
                    }
                }
                if stale != Some(false) {
                    sent.insert(hop, Instant::now());
                }
            }
            let _ = self.send_relay(idx, RelayCell::new(relay_cmd::PING, 0, Vec::new()));
        }
    }
    /// Хопы без PONG — исключаются при пересборке.
    pub fn dead_hops(&self) -> Vec<EndpointId> {
        let alive = self.hop_alive.lock().unwrap();
        self.path.iter().zip(alive.iter()).filter(|(_, a)| !**a).map(|(n, _)| *n).collect()
    }
    /// Мёртвый хоп или участок медленнее `SEGMENT_SLOW_MS`.
    pub fn is_degraded(&self) -> bool {
        if self.hop_alive.lock().unwrap().iter().any(|a| !*a) {
            return true;
        }
        let rtt = self.hop_rtt.lock().unwrap();
        let mut prev = 0u32;
        for r in rtt.iter().flatten() {
            if r.saturating_sub(prev) > SEGMENT_SLOW_MS {
                return true;
            }
            prev = *r;
        }
        false
    }
    pub fn retries(&self) -> u32 {
        self.retries.load(Ordering::Relaxed)
    }
    pub fn set_retries(&self, n: u32) {
        self.retries.store(n, Ordering::Relaxed);
    }
    pub fn note_e2e(&self, d: Duration) {
        *self.e2e.lock().unwrap() = Some((d.as_millis().min(u32::MAX as u128) as u32, Instant::now()));
    }
    pub fn info(&self, target: Option<EndpointId>, rebuilding: bool) -> CircuitInfo {
        // замки — отдельными операторами: временный guard в литерале живёт до конца выражения,
        // а `is_degraded()` ниже снова берёт hop_alive/hop_rtt → вечная блокировка потока
        let e2e = *self.e2e.lock().unwrap();
        let hop_rtt_ms = self.hop_rtt.lock().unwrap().clone();
        let hop_alive = self.hop_alive.lock().unwrap().clone();
        let degraded = self.is_degraded();
        CircuitInfo {
            id: self.id,
            path: self.path.iter().map(|n| n.to_string()).collect(),
            hop_rtt_ms,
            hop_alive,
            target: target.map(|t| t.to_string()),
            e2e_ms: e2e.map(|x| x.0),
            e2e_age_secs: e2e.map(|x| x.1.elapsed().as_secs()),
            age_secs: self.created.elapsed().as_secs(),
            cells_in: self.cells_in.load(Ordering::Relaxed),
            cells_out: self.cells_out.load(Ordering::Relaxed),
            retries: self.retries(),
            rebuilding,
            degraded,
            kind: kind_name(self.kind()),
        }
    }

    pub fn ping(&self) -> Result<()> {
        self.send_relay(self.hop_count() - 1, RelayCell::new(relay_cmd::PING, 0, Vec::new()))
    }

    /// Открыть логический поток к `target` с ALPN `alpn` через Exit.
    pub async fn open_stream(self: &Arc<Self>, target: EndpointId, alpn: &[u8]) -> Result<MushroomStream> {
        let exit = self.hop_count() - 1;
        let rv = self.has_rendezvous();
        let sid = self.next_stream.fetch_add(1, Ordering::Relaxed);
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.streams.lock().unwrap().insert(sid, tx);
        let mut data = vec![alpn.len() as u8];
        data.extend_from_slice(alpn);
        data.extend_from_slice(target.as_bytes());
        let begin = RelayCell::new(relay_cmd::BEGIN, sid, data);
        if rv {
            self.send_relay_rv(begin)?;
        } else {
            self.send_relay(exit, begin)?;
        }
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, rx.recv()).await {
            Ok(Some(StreamEv::Connected)) => {}
            Ok(Some(StreamEv::Error(e))) => {
                self.streams.lock().unwrap().remove(&sid);
                bail!("exit refused: {e}");
            }
            Ok(_) => {
                self.streams.lock().unwrap().remove(&sid);
                bail!("stream closed before CONNECTED");
            }
            Err(_) => {
                self.streams.lock().unwrap().remove(&sid);
                bail!("CONNECTED timeout");
            }
        }
        Ok(MushroomStream { circ: self.clone(), sid, rx, pending: Vec::new(), eof: false, exit, rv })
    }
}

/// Двунаправленный поток через цепочку: как QUIC bi-stream, но за тремя релеями.
pub struct MushroomStream {
    circ: Arc<Circuit>,
    sid: u16,
    rx: mpsc::UnboundedReceiver<StreamEv>,
    pending: Vec<u8>,
    eof: bool,
    exit: usize,
    /// Поток идёт через rendezvous (сквозной слой), а не через Exit.
    rv: bool,
}

impl MushroomStream {
    fn send(&self, c: RelayCell) -> Result<()> {
        if self.rv {
            self.circ.send_relay_rv(c)
        } else {
            self.circ.send_relay(self.exit, c)
        }
    }
    pub fn circuit(&self) -> &Arc<Circuit> {
        &self.circ
    }
    /// Удобный «запрос-ответ»: записать всё, закрыть запись, прочитать до конца (с лимитом).
    pub async fn request(mut self, payload: &[u8], max: usize) -> Result<Vec<u8>> {
        self.write_all(payload).await?;
        self.shutdown().await?;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 8192];
        loop {
            let n = self.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            if out.len() > max {
                bail!("response larger than {max} bytes");
            }
        }
        Ok(out)
    }
}

impl AsyncRead for MushroomStream {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        loop {
            if !self.pending.is_empty() {
                let n = self.pending.len().min(buf.remaining());
                buf.put_slice(&self.pending[..n]);
                self.pending.drain(..n);
                return Poll::Ready(Ok(()));
            }
            if self.eof {
                return Poll::Ready(Ok(()));
            }
            match self.rx.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) | Poll::Ready(Some(StreamEv::End)) => self.eof = true,
                Poll::Ready(Some(StreamEv::Data(d))) => self.pending = d,
                Poll::Ready(Some(StreamEv::Connected)) => {}
                Poll::Ready(Some(StreamEv::Error(e))) => return Poll::Ready(Err(std::io::Error::other(e))),
            }
        }
    }
}

impl AsyncWrite for MushroomStream {
    fn poll_write(self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        let n = buf.len().min(cell::data_capacity(self.exit + 1 + self.rv as usize));
        self.send(RelayCell::new(relay_cmd::DATA, self.sid, buf[..n].to_vec())).map_err(|e| std::io::Error::other(e.to_string()))?;
        Poll::Ready(Ok(n))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let _ = self.send(RelayCell::new(relay_cmd::END, self.sid, Vec::new()));
        Poll::Ready(Ok(()))
    }
}

impl Drop for MushroomStream {
    fn drop(&mut self) {
        self.circ.streams.lock().unwrap().remove(&self.sid);
    }
}

/// Закрыть цепочку при выходе из области видимости, если сборка не дошла до конца.
struct CloseOnDrop(Option<Arc<Circuit>>);
impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if let Some(c) = self.0.take() {
            c.close();
        }
    }
}
