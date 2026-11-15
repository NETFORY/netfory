//! Локальный SOCKS5-вход в Mycelium: хост `<EndpointId>.iroh`, порт → ALPN целевого сервиса.
//! Фаза 3: любой другой хост (домен / IPv4 / IPv6) отдаётся в `Egress` — обработчик выхода в
//! интернет через цепочку (в Netfory — узел pq-web `--open` за Exit-релеем).
use crate::circuit::MushroomStream;
use crate::Mycelium;
use anyhow::{bail, Result};
use iroh::EndpointId;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const DEFAULT_PORT: u16 = 9150;

/// Выход в интернет для не-`.iroh` хостов: (host, port) → поток через цепочку.
pub type Egress = Arc<dyn Fn(String, u16) -> Pin<Box<dyn Future<Output = Result<MushroomStream>> + Send>> + Send + Sync>;

/// Принимать SOCKS5-клиентов бесконечно.
pub async fn serve(listener: TcpListener, my: Mycelium, ports: HashMap<u16, String>, egress: Option<Egress>) {
    let ports = Arc::new(ports);
    while let Ok((sock, _)) = listener.accept().await {
        let my = my.clone();
        let ports = ports.clone();
        let egress = egress.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(sock, my, &ports, egress.as_ref()).await {
                tracing::debug!("socks: {e}");
            }
        });
    }
}

/// Есть ли на порту живой SOCKS5 (системный myceliumd): приветствие без аутентификации.
pub async fn probe(port: u16) -> bool {
    let fut = async {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        s.write_all(&[5, 1, 0]).await.ok()?;
        let mut r = [0u8; 2];
        s.read_exact(&mut r).await.ok()?;
        Some(r == [5, 0])
    };
    matches!(tokio::time::timeout(std::time::Duration::from_secs(2), fut).await, Ok(Some(true)))
}

/// Минимальный SOCKS5 (RFC 1928): без аутентификации, только CONNECT.
pub async fn handle(mut s: TcpStream, my: Mycelium, ports: &HashMap<u16, String>, egress: Option<&Egress>) -> Result<()> {
    let mut hdr = [0u8; 2];
    s.read_exact(&mut hdr).await?;
    if hdr[0] != 5 {
        bail!("not SOCKS5");
    }
    let mut methods = vec![0u8; hdr[1] as usize];
    s.read_exact(&mut methods).await?;
    s.write_all(&[5, 0]).await?;
    let mut req = [0u8; 4];
    s.read_exact(&mut req).await?;
    if req[1] != 1 {
        s.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        bail!("CONNECT only");
    }
    let host = match req[3] {
        3 => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l).await?;
            let mut h = vec![0u8; l[0] as usize];
            s.read_exact(&mut h).await?;
            String::from_utf8(h)?
        }
        1 => {
            let mut a = [0u8; 4];
            s.read_exact(&mut a).await?;
            std::net::Ipv4Addr::from(a).to_string()
        }
        4 => {
            let mut a = [0u8; 16];
            s.read_exact(&mut a).await?;
            std::net::Ipv6Addr::from(a).to_string()
        }
        _ => {
            s.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            bail!("ATYP");
        }
    };
    let mut pb = [0u8; 2];
    s.read_exact(&mut pb).await?;
    let port = u16::from_be_bytes(pb);
    let opened = match host.strip_suffix(".iroh").and_then(|n| n.parse::<EndpointId>().ok()) {
        Some(node) => {
            let Some(alpn) = ports.get(&port) else {
                s.write_all(&[5, 2, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
                bail!("port {port} is not mapped to an ALPN");
            };
            my.open_stream(node, alpn.as_bytes()).await
        }
        None => match egress {
            Some(eg) => eg(host.clone(), port).await,
            None => {
                s.write_all(&[5, 4, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
                bail!("host must be <EndpointId>.iroh: {host}");
            }
        },
    };
    match opened {
        Ok(mut mushroom) => {
            s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            let _ = tokio::io::copy_bidirectional(&mut s, &mut mushroom).await;
            Ok(())
        }
        Err(e) => {
            s.write_all(&[5, 1, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            Err(e)
        }
    }
}
