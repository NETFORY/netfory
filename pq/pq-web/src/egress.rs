//! Выбор семейства адресов и исходящего IP для TCP к сайту (v0.4.0).
//!
//! * `IpMode` — `auto` (по умолчанию: при старте и раз в 10 мин проверяем, работает ли
//!   исходящий IPv6; если да — предпочитаем AAAA), `prefer6`, `prefer4`, `only6`, `only4`.
//! * Happy Eyeballs (RFC 8305, упрощённо): сначала предпочтительное семейство, через
//!   `HE_DELAY` параллельно стартует второе; побеждает первое установленное соединение.
//! * `--egress-v6-prefix 2a01:db8::/64` — исходящий IPv6 берётся из префикса (нужен
//!   `sysctl net.ipv6.ip_nonlocal_bind=1` + `ip -6 route add local <prefix> dev lo`).
//! * `--egress-v6-per client|conn` (v0.5.0) — `client`: стабильный адрес на клиента
//!   (host-биты = ed25519-подпись узла над EndpointId клиента → один IPv6 на все его
//!   соединения, сессии/логины сайтов не рвутся); `conn`: случайный адрес на каждый CONNECT.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpSocket, TcpStream};

/// Задержка перед запуском второго семейства (RFC 8305 рекомендует 250 мс).
pub const HE_DELAY: Duration = Duration::from_millis(300);
/// Таймаут одной TCP-попытки.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(4);
/// Сколько адресов одного семейства пробуем последовательно.
const MAX_PER_FAMILY: usize = 3;
/// Публичные IPv6-адреса для проверки исходящего v6 (TCP:443, без данных).
const PROBE_V6: &[&str] = &["[2606:4700:4700::1111]:443", "[2001:4860:4860::8888]:443", "[2620:fe::fe]:443"];
/// Период повторной проверки IPv6 в режиме `auto`.
pub const REPROBE_EVERY: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IpMode {
    /// Автодетект исходящего IPv6 → prefer6, иначе prefer4.
    #[default]
    Auto,
    Prefer6,
    Prefer4,
    Only6,
    Only4,
}

impl IpMode {
    pub fn parse(s: &str) -> Option<IpMode> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => IpMode::Auto,
            "prefer6" | "prefer-6" | "v6" | "ipv6" => IpMode::Prefer6,
            "prefer4" | "prefer-4" | "v4" | "ipv4" => IpMode::Prefer4,
            "only6" | "only-6" | "6" => IpMode::Only6,
            "only4" | "only-4" | "4" => IpMode::Only4,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            IpMode::Auto => "auto",
            IpMode::Prefer6 => "prefer6",
            IpMode::Prefer4 => "prefer4",
            IpMode::Only6 => "only6",
            IpMode::Only4 => "only4",
        }
    }
}

impl std::str::FromStr for IpMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        IpMode::parse(s).ok_or_else(|| format!("bad ip-mode {s:?} (auto|prefer6|prefer4|only6|only4)"))
    }
}

impl std::fmt::Display for IpMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Как выдавать IPv6 из префикса: стабильно на клиента или случайно на соединение.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum V6Per {
    #[default]
    Client,
    Conn,
}

impl std::str::FromStr for V6Per {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "client" | "peer" | "user" => Ok(V6Per::Client),
            "conn" | "connection" | "random" => Ok(V6Per::Conn),
            _ => Err(format!("bad egress-v6-per {s:?} (client|conn)")),
        }
    }
}

impl V6Per {
    pub fn as_str(self) -> &'static str {
        match self {
            V6Per::Client => "client",
            V6Per::Conn => "conn",
        }
    }
}

impl std::fmt::Display for V6Per {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Доменный разделитель для подписи EndpointId → host-биты IPv6.
const V6_DERIVE_DOMAIN: &[u8] = b"pqweb-v6-egress|";

/// IPv6-префикс исходящих адресов (`addr/len`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V6Prefix {
    pub addr: Ipv6Addr,
    pub len: u8,
}

impl std::str::FromStr for V6Prefix {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (a, l) = s.trim().split_once('/').ok_or_else(|| "need addr/len, e.g. 2a01:db8::/64".to_string())?;
        let addr: Ipv6Addr = a.trim().parse().map_err(|e| format!("bad ipv6 {a:?}: {e}"))?;
        let len: u8 = l.trim().parse().map_err(|_| format!("bad prefix len {l:?}"))?;
        if !(32..=128).contains(&len) {
            return Err("prefix len must be 32..=128".into());
        }
        Ok(V6Prefix { addr, len })
    }
}

impl std::fmt::Display for V6Prefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.len)
    }
}

impl V6Prefix {
    /// Адрес внутри префикса с заданными host-битами (0 → 1: сам адрес сети не берём).
    fn with_host(&self, host: u128) -> Ipv6Addr {
        let base = u128::from(self.addr);
        if self.len >= 128 {
            return self.addr;
        }
        let host_bits = 128 - self.len as u32;
        let mask: u128 = if host_bits >= 128 { u128::MAX } else { (1u128 << host_bits) - 1 };
        let mut host = host & mask;
        if host == 0 {
            host = 1;
        }
        Ipv6Addr::from((base & !mask) | host)
    }

    /// Случайный адрес внутри префикса (режим `conn`).
    pub fn random_addr(&self) -> Ipv6Addr {
        self.with_host(rand::random::<u128>())
    }

    /// Стабильный адрес на клиента (режим `client`): host-биты — первые 16 байт
    /// детерминированной ed25519-подписи узла над `domain|EndpointId`. Без ключа узла
    /// адрес не предсказать, при перезапуске узла — тот же.
    pub fn addr_for_client(&self, key: &iroh::SecretKey, client: &[u8]) -> Ipv6Addr {
        let mut msg = Vec::with_capacity(V6_DERIVE_DOMAIN.len() + client.len());
        msg.extend_from_slice(V6_DERIVE_DOMAIN);
        msg.extend_from_slice(client);
        let sig = key.sign(&msg).to_bytes();
        let mut h = [0u8; 16];
        h.copy_from_slice(&sig[..16]);
        self.with_host(u128::from_be_bytes(h))
    }
}

/// Живое состояние egress: результат автодетекта IPv6 (в `auto`) или заданный режим.
#[derive(Debug, Clone)]
pub struct Egress {
    pub mode: IpMode,
    pub v6_prefix: Option<V6Prefix>,
    /// Как раздавать адреса из префикса (client — стабильно на EndpointId, conn — случайно).
    pub v6_per: V6Per,
    /// Ключ узла для вывода адреса клиента (нужен только в режиме `client`).
    v6_key: Option<iroh::SecretKey>,
    /// Исходящий IPv6 доступен (для `auto` — результат пробы; для prefer6/only6 — true).
    ipv6_ok: Arc<AtomicBool>,
}

/// Порядок семейств при наборе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    V6First,
    V4First,
    V6Only,
    V4Only,
}

impl Egress {
    pub fn new(mode: IpMode, v6_prefix: Option<V6Prefix>) -> Self {
        let ok = matches!(mode, IpMode::Prefer6 | IpMode::Only6);
        Egress { mode, v6_prefix, v6_per: V6Per::Conn, v6_key: None, ipv6_ok: Arc::new(AtomicBool::new(ok)) }
    }

    /// Включить режим выдачи адресов из префикса; ключ узла нужен для `client`.
    pub fn with_v6_per(mut self, per: V6Per, key: iroh::SecretKey) -> Self {
        self.v6_per = per;
        self.v6_key = Some(key);
        self
    }

    /// Исходящий IPv6 для клиента: стабильный (client) или случайный (conn); None без префикса.
    pub fn src6_for(&self, client: Option<&[u8]>) -> Option<IpAddr> {
        let p = self.v6_prefix?;
        let addr = match (self.v6_per, &self.v6_key, client) {
            (V6Per::Client, Some(key), Some(id)) => p.addr_for_client(key, id),
            _ => p.random_addr(),
        };
        Some(IpAddr::V6(addr))
    }

    /// Узел сейчас выходит в интернет по IPv6 (для INFO/announce/бейджа «v6» в клиенте).
    pub fn ipv6(&self) -> bool {
        self.ipv6_ok.load(Ordering::Relaxed)
    }

    pub fn ipv6_flag(&self) -> Arc<AtomicBool> {
        self.ipv6_ok.clone()
    }

    pub fn plan(&self) -> Plan {
        match self.mode {
            IpMode::Only6 => Plan::V6Only,
            IpMode::Only4 => Plan::V4Only,
            IpMode::Prefer6 => Plan::V6First,
            IpMode::Prefer4 => Plan::V4First,
            IpMode::Auto => {
                if self.ipv6() {
                    Plan::V6First
                } else {
                    Plan::V4First
                }
            }
        }
    }

    /// Проба исходящего IPv6 (TCP к публичным v6-резолверам, 3 с). В `auto` обновляет флаг.
    pub async fn probe(&self) -> bool {
        let src = self.v6_prefix.map(|p| p.random_addr());
        let mut ok = false;
        for a in PROBE_V6 {
            let Ok(addr) = a.parse::<SocketAddr>() else { continue };
            if tokio::time::timeout(Duration::from_secs(3), connect_from(addr, src.map(IpAddr::V6))).await.map(|r| r.is_ok()).unwrap_or(false) {
                ok = true;
                break;
            }
        }
        if self.mode == IpMode::Auto {
            let was = self.ipv6_ok.swap(ok, Ordering::Relaxed);
            if was != ok {
                tracing::info!("egress: ipv6 {} (auto)", if ok { "available → prefer AAAA" } else { "unavailable → prefer A" });
            }
        } else if matches!(self.mode, IpMode::Prefer6 | IpMode::Only6) && !ok {
            tracing::warn!("egress: ip-mode={} but outbound IPv6 probe failed", self.mode);
        }
        ok
    }

    /// Фоновая перепроверка IPv6 в режиме `auto`.
    pub fn spawn_reprobe(&self) -> Option<tokio::task::JoinHandle<()>> {
        if self.mode != IpMode::Auto {
            return None;
        }
        let me = self.clone();
        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(REPROBE_EVERY).await;
                me.probe().await;
            }
        }))
    }

    /// Разложить разрешённые адреса по семействам и набрать по плану (Happy Eyeballs).
    /// `client` — EndpointId клиента (32 байта) для стабильного исходящего IPv6.
    pub async fn dial(&self, addrs: Vec<SocketAddr>, client: Option<&[u8]>) -> std::io::Result<TcpStream> {
        let (v6, v4): (Vec<SocketAddr>, Vec<SocketAddr>) = addrs.into_iter().partition(|a| a.is_ipv6());
        let src6 = self.src6_for(client);
        match self.plan() {
            Plan::V6Only => try_family(v6, src6).await,
            Plan::V4Only => try_family(v4, None).await,
            Plan::V6First => happy_eyeballs(v6, src6, v4, None).await,
            Plan::V4First => happy_eyeballs(v4, None, v6, src6).await,
        }
    }
}

fn no_addr() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotFound, "no address of requested family")
}

/// TCP-коннект, при `src` — с явным исходящим адресом (bind перед connect).
async fn connect_from(addr: SocketAddr, src: Option<IpAddr>) -> std::io::Result<TcpStream> {
    match src {
        Some(ip) if ip.is_ipv6() == addr.is_ipv6() => {
            let sock = if addr.is_ipv6() { TcpSocket::new_v6()? } else { TcpSocket::new_v4()? };
            sock.bind(SocketAddr::new(ip, 0))?;
            sock.connect(addr).await
        }
        _ => TcpStream::connect(addr).await,
    }
}

/// Последовательные попытки внутри одного семейства (до MAX_PER_FAMILY адресов).
async fn try_family(addrs: Vec<SocketAddr>, src: Option<IpAddr>) -> std::io::Result<TcpStream> {
    if addrs.is_empty() {
        return Err(no_addr());
    }
    let mut last = no_addr();
    for a in addrs.into_iter().take(MAX_PER_FAMILY) {
        match tokio::time::timeout(ATTEMPT_TIMEOUT, connect_from(a, src)).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) => last = e,
            Err(_) => last = std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timeout"),
        }
    }
    Err(last)
}

/// Happy Eyeballs: `primary` сразу, `secondary` через HE_DELAY (или сразу, если primary пуст);
/// возвращается первое успешное соединение, ошибка — когда оба семейства исчерпаны.
async fn happy_eyeballs(primary: Vec<SocketAddr>, src_p: Option<IpAddr>, secondary: Vec<SocketAddr>, src_s: Option<IpAddr>) -> std::io::Result<TcpStream> {
    if primary.is_empty() {
        return try_family(secondary, src_s).await;
    }
    if secondary.is_empty() {
        return try_family(primary, src_p).await;
    }
    let p = try_family(primary, src_p);
    let s = async move {
        tokio::time::sleep(HE_DELAY).await;
        try_family(secondary, src_s).await
    };
    tokio::pin!(p);
    tokio::pin!(s);
    let mut p_err: Option<std::io::Error> = None;
    let mut s_err: Option<std::io::Error> = None;
    loop {
        tokio::select! {
            r = &mut p, if p_err.is_none() => match r {
                Ok(t) => return Ok(t),
                Err(e) => p_err = Some(e),
            },
            r = &mut s, if s_err.is_none() => match r {
                Ok(t) => return Ok(t),
                Err(e) => s_err = Some(e),
            },
            else => break,
        }
    }
    Err(p_err.or(s_err).unwrap_or_else(no_addr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode() {
        assert_eq!(IpMode::parse("auto"), Some(IpMode::Auto));
        assert_eq!(IpMode::parse("PREFER6"), Some(IpMode::Prefer6));
        assert_eq!(IpMode::parse("only4"), Some(IpMode::Only4));
        assert_eq!(IpMode::parse("nope"), None);
    }

    #[test]
    fn prefix_random_inside() {
        let p: V6Prefix = "2a01:4f8:c013:ab91::/64".parse().unwrap();
        for _ in 0..64 {
            let a = p.random_addr();
            assert_eq!(a.segments()[..4], p.addr.segments()[..4]);
            assert_ne!(a, p.addr);
        }
        assert!("2a01:4f8::/16".parse::<V6Prefix>().is_err());
        assert!("2a01:4f8::".parse::<V6Prefix>().is_err());
    }

    #[test]
    fn per_client_stable_and_distinct() {
        let p: V6Prefix = "2a01:4f8:c013:ab91::/64".parse().unwrap();
        let key = iroh::SecretKey::from([7u8; 32]);
        let a1 = p.addr_for_client(&key, &[1u8; 32]);
        let a2 = p.addr_for_client(&key, &[1u8; 32]);
        let b = p.addr_for_client(&key, &[2u8; 32]);
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
        assert_eq!(a1.segments()[..4], p.addr.segments()[..4]);
        assert_ne!(a1, p.addr);
        let other = iroh::SecretKey::from([8u8; 32]);
        assert_ne!(a1, p.addr_for_client(&other, &[1u8; 32]));
    }

    #[test]
    fn src6_modes() {
        let p: V6Prefix = "2a01:db8::/64".parse().unwrap();
        let key = iroh::SecretKey::from([9u8; 32]);
        let e = Egress::new(IpMode::Prefer6, Some(p)).with_v6_per(V6Per::Client, key.clone());
        assert_eq!(e.src6_for(Some(&[3u8; 32])), e.src6_for(Some(&[3u8; 32])));
        assert!(e.src6_for(None).is_some());
        assert!(Egress::new(IpMode::Prefer6, None).src6_for(Some(&[3u8; 32])).is_none());
        assert_eq!("client".parse::<V6Per>(), Ok(V6Per::Client));
        assert_eq!("conn".parse::<V6Per>(), Ok(V6Per::Conn));
        assert!("x".parse::<V6Per>().is_err());
    }

    #[test]
    fn plan_follows_mode() {
        assert_eq!(Egress::new(IpMode::Only6, None).plan(), Plan::V6Only);
        assert_eq!(Egress::new(IpMode::Prefer4, None).plan(), Plan::V4First);
        assert_eq!(Egress::new(IpMode::Auto, None).plan(), Plan::V4First);
        assert_eq!(Egress::new(IpMode::Prefer6, None).plan(), Plan::V6First);
    }
}
