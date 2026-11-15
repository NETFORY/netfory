//! Ячейки Mycelium (NIP-1 §3). Кадр фиксированного размера 2048 байт:
//! `[circ_id u32 BE][cmd u8][body 2043]`. Для RELAY-кадров body — луковица: хоп k
//! расшифровывает первые `BODY - 16*(k-1)` байт (каждый слой ChaCha20-Poly1305 добавляет
//! 16 байт тега), остаток — случайное дополнение.
//!
//! Внутренняя relay-ячейка (открытый текст для хопа-получателя):
//! `[magic u32][cmd u8][stream u16][len u16][data len][random pad]`.
use rand::RngCore;

pub const CELL: usize = 2048;
pub const BODY: usize = CELL - 5;
pub const TAG: usize = 16;
pub const MAX_HOPS: usize = 3;
pub const MAGIC: u32 = 0x4D59_4345; // "MYCE"
const RELAY_HDR: usize = 4 + 1 + 2 + 2;

/// Команды кадра (между соседними хопами, без луковицы).
pub mod frame_cmd {
    pub const CREATE: u8 = 0;
    pub const CREATED: u8 = 1;
    pub const RELAY: u8 = 2;
    pub const DESTROY: u8 = 3;
}

/// Команды relay-ячейки (внутри луковицы).
pub mod relay_cmd {
    pub const DATA: u8 = 1;
    pub const PING: u8 = 2;
    pub const PONG: u8 = 3;
    pub const CLOSE: u8 = 4;
    pub const EXTEND: u8 = 5;
    pub const EXTENDED: u8 = 6;
    pub const BEGIN: u8 = 7;
    pub const CONNECTED: u8 = 8;
    pub const END: u8 = 9;
    pub const SENDME: u8 = 10;
    pub const ERROR: u8 = 11;
    /// Фаза 2: маскирующий трафик — получатель молча отбрасывает.
    pub const PADDING: u8 = 12;
    /// Фаза 2: rendezvous (NIP-1 §7). Получатель B → intro-релей: `[owner 32][sig 64]`.
    pub const ESTABLISH_INTRO: u8 = 13;
    pub const INTRO_ESTABLISHED: u8 = 14;
    /// A → intro-релей: `[owner 32][RP 32][cookie 20][C_e 32]`.
    pub const INTRODUCE1: u8 = 15;
    /// A → RP: `[cookie 20]`.
    pub const ESTABLISH_RENDEZVOUS: u8 = 16;
    pub const RENDEZVOUS_ESTABLISHED: u8 = 17;
    /// B → RP: `[cookie 20][CREATED]` — RP сшивает цепочки A и B.
    pub const RENDEZVOUS1: u8 = 18;
    /// RP → A: `[CREATED]`.
    pub const RENDEZVOUS2: u8 = 19;
    /// intro-релей → B: `[RP 32][cookie 20][C_e 32]`.
    pub const INTRODUCE2: u8 = 20;
    pub const INTRODUCE_ACK: u8 = 21;
    pub const LAST: u8 = INTRODUCE_ACK;
}
/// Виртуальный «4-й хоп»: сквозной слой A↔B поверх rendezvous.
pub const RV_HOP: u8 = 4;
pub const COOKIE_LEN: usize = 20;

/// Полезная ёмкость открытого текста для хопа с индексом `hop` (1..=3).
pub fn plain_capacity(hop: usize) -> usize {
    BODY - TAG * hop
}
/// Максимум данных в одной DATA-ячейке для хопа `hop`.
pub fn data_capacity(hop: usize) -> usize {
    plain_capacity(hop) - RELAY_HDR
}

pub fn random_fill(buf: &mut [u8]) {
    rand::thread_rng().fill_bytes(buf);
}

/// Кадр CREATE/CREATED/DESTROY/RELAY: 2048 байт.
pub fn frame(circ: u32, cmd: u8, body: &[u8]) -> Vec<u8> {
    debug_assert!(body.len() <= BODY);
    let mut f = vec![0u8; CELL];
    f[..4].copy_from_slice(&circ.to_be_bytes());
    f[4] = cmd;
    f[5..5 + body.len()].copy_from_slice(body);
    random_fill(&mut f[5 + body.len()..]);
    f
}

pub fn parse_frame(f: &[u8]) -> Option<(u32, u8, &[u8])> {
    if f.len() != CELL {
        return None;
    }
    Some((u32::from_be_bytes([f[0], f[1], f[2], f[3]]), f[4], &f[5..]))
}

/// Разобранная relay-ячейка.
#[derive(Debug, Clone)]
pub struct RelayCell {
    pub cmd: u8,
    pub stream: u16,
    pub data: Vec<u8>,
}

impl RelayCell {
    pub fn new(cmd: u8, stream: u16, data: impl Into<Vec<u8>>) -> Self {
        Self { cmd, stream, data: data.into() }
    }

    /// Открытый текст ровно `capacity` байт (с случайным дополнением).
    pub fn encode(&self, capacity: usize) -> Vec<u8> {
        assert!(self.data.len() + RELAY_HDR <= capacity, "relay cell overflow");
        let mut p = vec![0u8; capacity];
        p[..4].copy_from_slice(&MAGIC.to_be_bytes());
        p[4] = self.cmd;
        p[5..7].copy_from_slice(&self.stream.to_be_bytes());
        p[7..9].copy_from_slice(&(self.data.len() as u16).to_be_bytes());
        p[9..9 + self.data.len()].copy_from_slice(&self.data);
        random_fill(&mut p[9 + self.data.len()..]);
        p
    }

    /// `None` — это не ячейка для нас (следующий слой луковицы).
    pub fn decode(p: &[u8]) -> Option<Self> {
        if p.len() < RELAY_HDR || u32::from_be_bytes([p[0], p[1], p[2], p[3]]) != MAGIC {
            return None;
        }
        let cmd = p[4];
        if !(relay_cmd::DATA..=relay_cmd::LAST).contains(&cmd) {
            return None;
        }
        let stream = u16::from_be_bytes([p[5], p[6]]);
        let len = u16::from_be_bytes([p[7], p[8]]) as usize;
        if RELAY_HDR + len > p.len() {
            return None;
        }
        Some(Self { cmd, stream, data: p[RELAY_HDR..RELAY_HDR + len].to_vec() })
    }
}
