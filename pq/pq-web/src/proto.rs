//! Протокол `netfory/pqweb/1` — текстовый заголовок в начале QUIC-стрима.
//!
//! Клиент открывает bi-stream и шлёт ОДНУ строку (≤ 512 байт, `\n`):
//!   `NF1 CONNECT host:port`  → `OK\n`, дальше сырые байты в обе стороны (TLS клиент↔сайт).
//!   `NF1 PING`               → `PONG <unix_ms>\n`
//!   `NF1 BENCH <bytes>`      → `OK <bytes>\n` + столько нулевых байт (≤ 4 MiB) — оценка полосы.
//!   `NF1 INFO`               → `OK <json>\n` (версия, bundles, регион, лимиты).
//! Ошибки: `ERR <code> <slug>\n` и стрим закрывается.

pub const ALPN: &[u8] = b"netfory/pqweb/1";
pub const MAX_HEADER: usize = 512;
pub const MAX_BENCH: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Connect { host: String, port: u16 },
    Ping,
    Bench(u64),
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrCode {
    BadRequest = 400,
    NotAllowed = 403,
    TooManyStreams = 429,
    ConnectFailed = 502,
    Timeout = 504,
    Busy = 503,
}

impl ErrCode {
    pub fn slug(self) -> &'static str {
        match self {
            ErrCode::BadRequest => "bad-request",
            ErrCode::NotAllowed => "not-allowed",
            ErrCode::TooManyStreams => "too-many-streams",
            ErrCode::ConnectFailed => "connect-failed",
            ErrCode::Timeout => "timeout",
            ErrCode::Busy => "busy",
        }
    }
    pub fn line(self) -> String {
        format!("ERR {} {}\n", self as u16, self.slug())
    }
}

/// Разбор строки заголовка (без завершающего `\n`).
pub fn parse(line: &str) -> Result<Request, ErrCode> {
    let mut it = line.trim().split_whitespace();
    if it.next() != Some("NF1") {
        return Err(ErrCode::BadRequest);
    }
    match it.next() {
        Some("PING") => Ok(Request::Ping),
        Some("INFO") => Ok(Request::Info),
        Some("BENCH") => {
            let n: u64 = it.next().and_then(|v| v.parse().ok()).ok_or(ErrCode::BadRequest)?;
            if n == 0 || n > MAX_BENCH {
                return Err(ErrCode::BadRequest);
            }
            Ok(Request::Bench(n))
        }
        Some("CONNECT") => {
            let target = it.next().ok_or(ErrCode::BadRequest)?;
            // host:port; IPv6 в квадратных скобках — [::1]:443
            let (host, port) = if let Some(rest) = target.strip_prefix('[') {
                let (h, p) = rest.split_once("]:").ok_or(ErrCode::BadRequest)?;
                (h.to_string(), p)
            } else {
                let (h, p) = target.rsplit_once(':').ok_or(ErrCode::BadRequest)?;
                (h.to_string(), p)
            };
            let port: u16 = port.parse().map_err(|_| ErrCode::BadRequest)?;
            if host.is_empty() || host.len() > 253 || port == 0 || !host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':')) {
                return Err(ErrCode::BadRequest);
            }
            Ok(Request::Connect { host, port })
        }
        _ => Err(ErrCode::BadRequest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ok() {
        assert_eq!(parse("NF1 CONNECT youtube.com:443"), Ok(Request::Connect { host: "youtube.com".into(), port: 443 }));
        assert_eq!(parse("NF1 PING"), Ok(Request::Ping));
        assert_eq!(parse("NF1 BENCH 1024"), Ok(Request::Bench(1024)));
        assert_eq!(parse("NF1 INFO"), Ok(Request::Info));
    }

    #[test]
    fn parse_err() {
        assert_eq!(parse("GET / HTTP/1.1"), Err(ErrCode::BadRequest));
        assert_eq!(parse("NF1 CONNECT youtube.com"), Err(ErrCode::BadRequest));
        assert_eq!(parse("NF1 BENCH 0"), Err(ErrCode::BadRequest));
        assert_eq!(parse("NF1 CONNECT a b:443"), Err(ErrCode::BadRequest));
    }
}
