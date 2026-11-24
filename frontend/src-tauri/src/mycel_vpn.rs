//! Mycelium Фаза 3 — системный VPN на всём устройстве (NIP-1 §9).
//!
//! Схема: TUN (помощник `mycelium-tun`, права администратора) → локальный SOCKS5 Netfory →
//! mushroom-цепочка → Exit → узел pq-web `--open` (ALPN `netfory/pqweb/1`, `NF1 CONNECT host:port`)
//! → интернет. DNS устройства идёт по TCP через ту же цепочку. Мимо туннеля — только транспорт
//! самого Mycelium: прямые адреса guard-релеев и реле iroh (иначе петля).
//!
//! Состояние VPN не сохраняется между запусками (каждое включение — запрос прав администратора).
use serde::Serialize;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

#[cfg(target_os = "windows")]
const HELPER: &str = "mycelium-tun.exe";
#[cfg(not(target_os = "windows"))]
const HELPER: &str = "mycelium-tun";

const SUPPORTED: bool = cfg!(any(target_os = "windows", target_os = "linux", target_os = "macos"));

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VpnInfo {
    /// Пользователь включил VPN (помощник запускается / запущен).
    pub enabled: bool,
    /// Туннель поднят и отвечает на STATUS.
    pub running: bool,
    pub error: String,
    pub supported: bool,
    /// Бинарь `mycelium-tun` найден рядом с Netfory.
    pub helper_found: bool,
    pub socks_port: u16,
    pub bypass: usize,
    pub tx: u64,
    pub rx: u64,
    pub up_secs: u64,
    /// Открыто TCP-сессий через SOCKS за всё время.
    pub sessions: u64,
}

#[derive(Default)]
struct State {
    enabled: bool,
    running: bool,
    error: String,
    socks_port: u16,
    ctl: Option<SocketAddr>,
    token: String,
    bypass: usize,
    tx: u64,
    rx: u64,
    up_secs: u64,
    fails: u32,
    sessions: u64,
    started: Option<Instant>,
}

static ST: Mutex<State> = Mutex::new(State { enabled: false, running: false, error: String::new(), socks_port: 0, ctl: None, token: String::new(), bypass: 0, tx: 0, rx: 0, up_secs: 0, fails: 0, sessions: 0, started: None });
static DATA_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

pub(crate) fn set_data_dir(p: PathBuf) {
    *DATA_DIR.lock().unwrap() = Some(p);
}

/// Путь к помощнику проверяется на диске не чаще раза в 30 с (статус опрашивается каждые 3 с).
static HELPER_CACHE: Mutex<Option<(Option<PathBuf>, Instant)>> = Mutex::new(None);

fn helper_path() -> Option<PathBuf> {
    if let Some((p, at)) = HELPER_CACHE.lock().unwrap().clone() {
        if at.elapsed() < std::time::Duration::from_secs(30) {
            return p;
        }
    }
    let p = helper_path_uncached();
    *HELPER_CACHE.lock().unwrap() = Some((p.clone(), Instant::now()));
    p
}

fn helper_path_uncached() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("NTFRY_MYCELIUM_TUN") {
        let p = PathBuf::from(p.trim());
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for c in [dir.join(HELPER), dir.join("mycelium-tun").join(HELPER)] {
        if c.is_file() {
            return Some(c);
        }
    }
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(HELPER)).find(|c| c.is_file()))
}

pub(crate) fn info() -> VpnInfo {
    let s = ST.lock().unwrap();
    VpnInfo {
        enabled: s.enabled,
        running: s.running,
        error: s.error.clone(),
        supported: SUPPORTED,
        helper_found: helper_path().is_some(),
        socks_port: s.socks_port,
        bypass: s.bypass,
        tx: s.tx,
        rx: s.rx,
        up_secs: s.up_secs,
        sessions: s.sessions,
    }
}

#[cfg(feature = "p2p")]
mod imp {
    use super::*;
    use mycelium::socks::Egress;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};

    const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(40);
    const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

    /// Выход в интернет: поток к pq-web `--open` через цепочку + рукопожатие `NF1 CONNECT`.
    fn egress(my: mycelium::Mycelium) -> Egress {
        Arc::new(move |host: String, port: u16| {
            let my = my.clone();
            Box::pin(async move {
                ST.lock().unwrap().sessions += 1;
                open_egress(&my, &host, port).await.map(|(_, s)| s)
            })
        })
    }

    /// TCP к host:port через цепочку Mycelium (Exit → pq-web `--open`). Для web-вкладок
    /// (роутер webproxy) — без TUN. Возвращает (EndpointId узла pq-web, поток).
    pub(crate) async fn tcp_via_mycelium(host: &str, port: u16) -> Result<(String, mycelium::MushroomStream), String> {
        let my = crate::mycel::instance().ok_or("Mycelium is not running")?;
        if !my.is_enabled() {
            return Err("Mycelium is off".into());
        }
        open_egress(my, host, port).await.map_err(|e| e.to_string())
    }

    /// То же, но через заданный список узлов pq-web (проверка IP выхода — тот же узел, что у сайта).
    pub(crate) async fn tcp_via_nodes(nodes: Vec<crate::webproxy::PqNode>, host: &str, port: u16) -> Result<(String, mycelium::MushroomStream), String> {
        let my = crate::mycel::instance().ok_or("Mycelium is not running")?;
        if !my.is_enabled() {
            return Err("Mycelium is off".into());
        }
        open_via(my, nodes, host, port).await.map_err(|e| e.to_string())
    }

    async fn open_egress(my: &mycelium::Mycelium, host: &str, port: u16) -> anyhow::Result<(String, mycelium::MushroomStream)> {
        open_via(my, crate::webproxy::egress_nodes(host), host, port).await
    }

    async fn open_via(my: &mycelium::Mycelium, nodes: Vec<crate::webproxy::PqNode>, host: &str, port: u16) -> anyhow::Result<(String, mycelium::MushroomStream)> {
        {
            {
                if nodes.is_empty() {
                    anyhow::bail!("no pq-web nodes with --open (Settings → PQ Web)");
                }
                let mut last = String::new();
                for node in nodes.into_iter().take(3) {
                    let Ok(id) = node.id.parse::<iroh::EndpointId>() else { continue };
                    let r = async {
                        let mut s = tokio::time::timeout(CONNECT_TIMEOUT, my.open_stream(id, crate::webproxy::PQWEB_ALPN)).await.map_err(|_| anyhow::anyhow!("circuit timeout"))??;
                        s.write_all(format!("NF1 CONNECT {host}:{port}\n").as_bytes()).await?;
                        let mut line = Vec::new();
                        let mut b = [0u8; 1];
                        loop {
                            let n = tokio::time::timeout(REPLY_TIMEOUT, s.read(&mut b)).await.map_err(|_| anyhow::anyhow!("no reply from pq-web"))??;
                            if n == 0 || b[0] == b'\n' || line.len() > 256 {
                                break;
                            }
                            line.push(b[0]);
                        }
                        let reply = String::from_utf8_lossy(&line).trim().to_string();
                        if reply == "OK" || reply.starts_with("OK ") {
                            Ok((node.id.clone(), s))
                        } else {
                            Err(anyhow::anyhow!("{}: {reply}", &node.id[..node.id.len().min(10)]))
                        }
                    }
                    .await;
                    match r {
                        Ok(s) => return Ok(s),
                        Err(e) => last = e.to_string(),
                    }
                }
                Err(anyhow::anyhow!("egress failed: {last}"))
            }
        }
    }

    /// Локальный SOCKS5 для TUN-помощника (один на процесс, порт случайный, только 127.0.0.1).
    async fn ensure_socks(my: &mycelium::Mycelium) -> Result<u16, String> {
        if let p @ 1.. = ST.lock().unwrap().socks_port {
            return Ok(p);
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| format!("socks bind: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        tokio::spawn(mycelium::socks::serve(listener, my.clone(), Default::default(), Some(egress(my.clone()))));
        ST.lock().unwrap().socks_port = port;
        crate::syslog::log("mycelium", format!("vpn: SOCKS5 for TUN on 127.0.0.1:{port}"));
        Ok(port)
    }

    /// Адреса мимо туннеля: прямые ip guard-релеев + реле iroh (резолвим заранее — после подъёма
    /// TUN DNS идёт через цепочку, а реле нужно для самой цепочки).
    async fn bypass_list(my: &mycelium::Mycelium) -> Vec<String> {
        let (ips, relays) = my.bypass_addrs().await;
        let mut out: Vec<String> = ips.iter().map(|a| a.ip().to_string()).collect();
        let mut hosts: Vec<String> = relays.iter().filter_map(|u| u.host_str().map(|h| h.to_string())).collect();
        hosts.extend(crate::p2p::effective_relay_urls().iter().map(|u| host_of(u)).filter(|h| !h.is_empty()));
        hosts.sort();
        hosts.dedup();
        for h in hosts {
            if let Ok(addrs) = tokio::time::timeout(std::time::Duration::from_secs(4), tokio::net::lookup_host((h.as_str(), 443))).await {
                if let Ok(addrs) = addrs {
                    out.extend(addrs.map(|a| a.ip().to_string()));
                }
            }
        }
        out.retain(|ip| !ip.starts_with("127.") && ip != "::1");
        out.sort();
        out.dedup();
        out
    }

    /// `https://host[:port]/…` → host.
    fn host_of(u: &str) -> String {
        let authority = u.split("://").nth(1).unwrap_or(u).split('/').next().unwrap_or("");
        if let Some(v6) = authority.strip_prefix('[') {
            return v6.split(']').next().unwrap_or("").to_string();
        }
        authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority).to_string()
    }

    async fn free_port() -> Result<u16, String> {
        TcpListener::bind("127.0.0.1:0").await.and_then(|l| l.local_addr()).map(|a| a.port()).map_err(|e| e.to_string())
    }

    /// Запуск помощника с правами администратора; процесс отвязан от Netfory, общение — по ctl-порту.
    fn spawn_elevated(helper: &PathBuf, args: &[String]) -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            let list = args.iter().map(|a| format!("'{}'", a.replace('\'', "''"))).collect::<Vec<_>>().join(",");
            let script = format!("Start-Process -FilePath '{}' -ArgumentList @({list}) -Verb RunAs -WindowStyle Hidden", helper.display().to_string().replace('\'', "''"));
            std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script]).creation_flags(0x0800_0000).spawn().map_err(|e| format!("powershell: {e}"))?;
            Ok(())
        }
        #[cfg(target_os = "linux")]
        {
            let mut cmd = std::process::Command::new("pkexec");
            cmd.arg(helper).args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            cmd.spawn().map_err(|e| format!("pkexec: {e} (install polkit or run: sudo {} {})", helper.display(), args.join(" ")))?;
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            let quoted = std::iter::once(helper.display().to_string()).chain(args.iter().cloned()).map(|a| format!("'{}'", a.replace('\'', "'\\''"))).collect::<Vec<_>>().join(" ");
            let shell = format!("{quoted} >/dev/null 2>&1 &").replace('\\', "\\\\").replace('"', "\\\"");
            std::process::Command::new("osascript").args(["-e", &format!("do shell script \"{shell}\" with administrator privileges")]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map_err(|e| format!("osascript: {e}"))?;
            Ok(())
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        {
            let _ = (helper, args);
            Err("system VPN is desktop-only".into())
        }
    }

    async fn ctl(cmd: &str) -> Result<String, String> {
        let (addr, token) = {
            let s = ST.lock().unwrap();
            (s.ctl.ok_or("helper is not running")?, s.token.clone())
        };
        let io = async {
            let mut sock = TcpStream::connect(addr).await.map_err(|e| e.to_string())?;
            sock.write_all(format!("{token} {cmd}\n").as_bytes()).await.map_err(|e| e.to_string())?;
            let mut rd = BufReader::new(sock);
            let mut line = String::new();
            rd.read_line(&mut line).await.map_err(|e| e.to_string())?;
            Ok::<_, String>(line.trim().to_string())
        };
        let line = tokio::time::timeout(std::time::Duration::from_secs(4), io).await.map_err(|_| "helper timeout".to_string())??;
        if let Some(rest) = line.strip_prefix("OK") {
            Ok(rest.trim().to_string())
        } else {
            Err(line)
        }
    }

    fn parse_status(s: &str) {
        let mut st = ST.lock().unwrap();
        for kv in s.split_whitespace() {
            if let Some((k, v)) = kv.split_once('=') {
                let n: u64 = v.parse().unwrap_or(0);
                match k {
                    "up" => st.up_secs = n,
                    "tx" => st.tx = n,
                    "rx" => st.rx = n,
                    _ => {}
                }
            }
        }
    }

    pub(crate) async fn start() -> Result<(), String> {
        if !SUPPORTED {
            return Err("system VPN is available on Windows, Linux and macOS only".into());
        }
        let my = crate::mycel::instance().ok_or("Mycelium is not running")?;
        if !my.is_enabled() {
            return Err("turn Mycelium on first".into());
        }
        if my.status().circuits_ready == 0 {
            return Err("no circuits yet — wait until Mycelium is active".into());
        }
        let helper = helper_path().ok_or_else(|| format!("{HELPER} not found next to Netfory"))?;
        if ST.lock().unwrap().enabled {
            return Ok(());
        }
        let socks = ensure_socks(my).await?;
        let bypass = bypass_list(my).await;
        let ctl_port = free_port().await?;
        let token: String = (0..16).map(|_| format!("{:02x}", rand::random::<u8>())).collect();
        let log = DATA_DIR.lock().unwrap().clone().map(|d| d.join("mycelium-tun.log"));
        let mut args = vec!["--socks".to_string(), format!("127.0.0.1:{socks}"), "--ctl".into(), format!("127.0.0.1:{ctl_port}"), "--token".into(), token.clone()];
        if !bypass.is_empty() {
            args.push("--bypass".into());
            args.push(bypass.join(","));
        }
        if let Some(l) = &log {
            args.push("--log".into());
            args.push(l.display().to_string());
        }
        {
            let mut s = ST.lock().unwrap();
            s.enabled = true;
            s.running = false;
            s.error.clear();
            s.ctl = Some(SocketAddr::from(([127, 0, 0, 1], ctl_port)));
            s.token = token;
            s.bypass = bypass.len();
            s.fails = 0;
            s.tx = 0;
            s.rx = 0;
            s.up_secs = 0;
            s.started = Some(Instant::now());
        }
        crate::syslog::log("mycelium", format!("vpn: starting {} · bypass {} addrs · ctl 127.0.0.1:{ctl_port}", helper.display(), bypass.len()));
        // запуск процесса (pkexec / powershell / osascript) — блокирующий вызов, уводим в пул
        let (h2, a2) = (helper.clone(), args.clone());
        let spawned = tauri::async_runtime::spawn_blocking(move || spawn_elevated(&h2, &a2)).await.unwrap_or_else(|e| Err(e.to_string()));
        if let Err(e) = spawned {
            let mut s = ST.lock().unwrap();
            s.enabled = false;
            s.error = e.clone();
            return Err(e);
        }
        // ждём, пока помощник пройдёт UAC/pkexec и поднимет туннель (до 90 с)
        tokio::spawn(async move {
            for _ in 0..45 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                if !ST.lock().unwrap().enabled {
                    return;
                }
                if let Ok(s) = ctl("STATUS").await {
                    parse_status(&s);
                    {
                        let mut st = ST.lock().unwrap();
                        st.running = true;
                        st.error.clear();
                    }
                    crate::tray_set_vpn(true);
                    crate::syslog::log("mycelium", "vpn: tunnel is up — all device traffic goes through Mycelium");
                    return;
                }
            }
            let mut st = ST.lock().unwrap();
            if st.enabled && !st.running {
                st.enabled = false;
                st.error = "helper did not start (administrator rights declined or TUN failed — see mycelium-tun.log)".into();
                crate::syslog::log("mycelium", format!("vpn: {}", st.error));
            }
        });
        Ok(())
    }

    pub(crate) async fn stop() {
        let was = ST.lock().unwrap().enabled;
        if !was {
            return;
        }
        let r = ctl("STOP").await;
        let mut s = ST.lock().unwrap();
        s.enabled = false;
        s.running = false;
        s.ctl = None;
        s.token.clear();
        s.started = None;
        if let Err(e) = r {
            s.error = format!("stop: {e}");
        }
        drop(s);
        crate::tray_set_vpn(false);
        crate::syslog::log("mycelium", "vpn: tunnel stopped");
    }

    /// Опрос помощника раз в 5 с: статистика; 3 ошибки подряд — туннель упал.
    pub(crate) fn spawn_poll_loop() {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let (enabled, running) = {
                    let s = ST.lock().unwrap();
                    (s.enabled, s.running)
                };
                if !enabled || !running {
                    continue;
                }
                match ctl("STATUS").await {
                    Ok(s) => {
                        parse_status(&s);
                        ST.lock().unwrap().fails = 0;
                    }
                    Err(e) => {
                        let mut st = ST.lock().unwrap();
                        st.fails += 1;
                        if st.fails >= 3 {
                            st.enabled = false;
                            st.running = false;
                            st.ctl = None;
                            st.error = format!("tunnel helper stopped responding ({e})");
                            crate::syslog::log("mycelium", format!("vpn: {}", st.error));
                            drop(st);
                            crate::tray_set_vpn(false);
                        }
                    }
                }
            }
        });
    }
}

#[cfg(feature = "p2p")]
pub(crate) use imp::{spawn_poll_loop, start, stop, tcp_via_mycelium, tcp_via_nodes};

#[cfg(not(feature = "p2p"))]
pub(crate) async fn start() -> Result<(), String> {
    Err("Mycelium is not available in this build".into())
}
#[cfg(not(feature = "p2p"))]
pub(crate) async fn stop() {}
#[cfg(not(feature = "p2p"))]
pub(crate) fn spawn_poll_loop() {}

/// Тумблер «VPN на всём устройстве»: включение запрашивает права администратора.
#[tauri::command]
pub(crate) async fn mycelium_vpn_set(on: bool) -> Result<crate::mycel::MyceliumInfo, String> {
    if on {
        start().await?;
    } else {
        stop().await;
    }
    Ok(crate::mycel::mycelium_status().await)
}
