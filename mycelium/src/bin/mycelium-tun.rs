//! mycelium-tun — привилегированный TUN-помощник системного VPN Mycelium (NIP-1, Фаза 3).
//!
//! Поднимает TUN-интерфейс (wintun / utun / /dev/net/tun), делает его маршрутом по умолчанию и
//! заворачивает весь TCP-трафик устройства в локальный SOCKS5 Netfory (`--socks`), откуда он
//! идёт по mushroom-цепочке к Exit и дальше через узел pq-web в интернет. DNS — по TCP через ту же
//! цепочку (ничего не утекает к локальному резолверу). Адреса из `--bypass` (guard-релеи, реле
//! iroh) идут мимо туннеля: без этого транспорт самого Mycelium зациклился бы.
//!
//! Управление — строка по TCP на `--ctl`: `<token> STATUS` → `OK up=<с> tx=<байт> rx=<байт>`,
//! `<token> STOP` → `OK` и выход (маршруты снимаются). Если SOCKS Netfory не отвечает дольше
//! 30 с (приложение закрыто), помощник выходит сам.
//!
//! Запуск: Netfory (`mycel_vpn.rs`) поднимает процесс с правами администратора — UAC (Windows),
//! pkexec (Linux), osascript «with administrator privileges» (macOS).
use clap::Parser;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

static TX: AtomicU64 = AtomicU64::new(0);
static RX: AtomicU64 = AtomicU64::new(0);

struct Opts {
    socks: SocketAddr,
    ctl: SocketAddr,
    token: String,
    bypass: Vec<String>,
    dns_addr: String,
    log: Option<String>,
    verbose: bool,
}

fn usage() -> ! {
    eprintln!("usage: mycelium-tun --socks IP:PORT --ctl IP:PORT --token TOKEN [--bypass IP[,IP…]] [--dns-addr IP] [--log FILE] [-v]");
    std::process::exit(2);
}

fn parse() -> Opts {
    let mut o = Opts { socks: "127.0.0.1:9150".parse().unwrap(), ctl: "127.0.0.1:9151".parse().unwrap(), token: String::new(), bypass: Vec::new(), dns_addr: "1.1.1.1".into(), log: None, verbose: false };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--socks" => o.socks = it.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()),
            "--ctl" => o.ctl = it.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()),
            "--token" => o.token = it.next().unwrap_or_else(|| usage()),
            "--bypass" => o.bypass.extend(it.next().unwrap_or_default().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty())),
            "--dns-addr" => o.dns_addr = it.next().unwrap_or_else(|| usage()),
            "--log" => o.log = it.next(),
            "-v" => o.verbose = true,
            _ => usage(),
        }
    }
    if o.token.is_empty() {
        usage();
    }
    o
}

/// Простейший логгер в файл (stderr у процесса с правами администратора никто не видит).
struct FileLog(std::sync::Mutex<std::fs::File>);
impl log::Log for FileLog {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        use std::io::Write;
        if let Ok(mut f) = self.0.lock() {
            let _ = writeln!(f, "{} {:<5} {}: {}", chrono_lite(), r.level(), r.target(), r.args());
        }
    }
    fn flush(&self) {}
}
fn chrono_lite() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{:02}:{:02}:{:02}", (s / 3600) % 24, (s / 60) % 60, s % 60)
}

unsafe extern "C" fn traffic_cb(status: *const tun2proxy::TrafficStatus, _: *mut std::ffi::c_void) {
    let st = unsafe { &*status };
    TX.store(st.tx, Ordering::Relaxed);
    RX.store(st.rx, Ordering::Relaxed);
}

async fn ctl_server(listener: TcpListener, token: String, started: Instant, shutdown: tun2proxy::CancellationToken) {
    loop {
        let Ok((sock, _)) = listener.accept().await else { break };
        let token = token.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            let mut rd = BufReader::new(sock);
            let mut line = String::new();
            if tokio::time::timeout(std::time::Duration::from_secs(5), rd.read_line(&mut line)).await.is_err() {
                return;
            }
            let mut parts = line.split_whitespace();
            let (tok, cmd) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let mut sock = rd.into_inner();
            if tok != token {
                let _ = sock.write_all(b"ERR token\n").await;
                return;
            }
            match cmd {
                "STATUS" => {
                    let s = format!("OK up={} tx={} rx={}\n", started.elapsed().as_secs(), TX.load(Ordering::Relaxed), RX.load(Ordering::Relaxed));
                    let _ = sock.write_all(s.as_bytes()).await;
                }
                "STOP" => {
                    let _ = sock.write_all(b"OK\n").await;
                    log::info!("STOP received");
                    shutdown.cancel();
                }
                _ => {
                    let _ = sock.write_all(b"ERR cmd\n").await;
                }
            }
        });
    }
}

/// Netfory закрылся (SOCKS не отвечает 3 раза подряд) — снимаем туннель, иначе устройство без сети.
async fn watchdog(socks: SocketAddr, shutdown: tun2proxy::CancellationToken) {
    let mut fails = 0;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let ok = tokio::time::timeout(std::time::Duration::from_secs(3), TcpStream::connect(socks)).await.map(|r| r.is_ok()).unwrap_or(false);
        fails = if ok { 0 } else { fails + 1 };
        if fails >= 3 {
            log::warn!("SOCKS {socks} unreachable 30 s — shutting down tunnel");
            shutdown.cancel();
            return;
        }
    }
}

fn main() {
    let o = parse();
    if let Some(p) = &o.log {
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = log::set_boxed_logger(Box::new(FileLog(std::sync::Mutex::new(f))));
            log::set_max_level(if o.verbose { log::LevelFilter::Debug } else { log::LevelFilter::Info });
        }
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio");
    let code = rt.block_on(async move {
        let ctl = match TcpListener::bind(o.ctl).await {
            Ok(l) => l,
            Err(e) => {
                log::error!("ctl bind {}: {e}", o.ctl);
                return 3;
            }
        };
        let mut argv: Vec<String> = vec!["mycelium-tun".into(), "--proxy".into(), format!("socks5://{}", o.socks), "--dns".into(), "over-tcp".into(), "--dns-addr".into(), o.dns_addr.clone(), "--setup".into(), "--ipv6-enabled".into(), "--exit-on-fatal-error".into(), "--max-sessions".into(), "600".into()];
        for b in &o.bypass {
            argv.push("--bypass".into());
            argv.push(b.clone());
        }
        // сам SOCKS и DNS-сервер тоже не в туннель: первый — петля, второй — Netfory ходит к нему напрямую через цепочку
        argv.push("--bypass".into());
        argv.push(o.socks.ip().to_string());
        let args = match tun2proxy::Args::try_parse_from(&argv) {
            Ok(a) => a,
            Err(e) => {
                log::error!("args: {e}");
                return 2;
            }
        };
        unsafe { tun2proxy::tun2proxy_set_traffic_status_callback(1, Some(traffic_cb), std::ptr::null_mut()) };
        let shutdown = tun2proxy::CancellationToken::new();
        let started = Instant::now();
        tokio::spawn(ctl_server(ctl, o.token.clone(), started, shutdown.clone()));
        tokio::spawn(watchdog(o.socks, shutdown.clone()));
        let sd = shutdown.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                sd.cancel();
            }
        });
        log::info!("tunnel up: socks {} · ctl {} · bypass {}", o.socks, o.ctl, o.bypass.len());
        let mtu = tun2proxy::DEFAULT_MTU;
        match tun2proxy::general_run_async(args, mtu, cfg!(target_os = "macos"), shutdown).await {
            Ok(_) => {
                log::info!("tunnel down");
                0
            }
            Err(e) => {
                log::error!("tun2proxy: {e}");
                1
            }
        }
    });
    drop(rt);
    std::process::exit(code);
}
