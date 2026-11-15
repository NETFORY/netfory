//! Счётчики и мини-HTTP (`/metrics` в формате Prometheus, `/healthz`) на loopback.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Default)]
pub struct Metrics {
    pub streams_total: AtomicU64,
    pub streams_active: AtomicU64,
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
    pub rejected_not_allowed: AtomicU64,
    pub rejected_limit: AtomicU64,
    pub connect_failed: AtomicU64,
    pub pings: AtomicU64,
    pub benches: AtomicU64,
    pub connections: AtomicU64,
}

impl Metrics {
    pub fn inc(&self, c: &AtomicU64) {
        c.fetch_add(1, Ordering::Relaxed);
    }
    pub fn add(&self, c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }
    pub fn dec(&self, c: &AtomicU64) {
        c.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn render(&self, clients_online: usize, endpoint_id: &str) -> String {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        format!(
            "# pq-web {ver}\n\
             pqweb_info{{endpoint_id=\"{eid}\",version=\"{ver}\"}} 1\n\
             pqweb_connections_total {conn}\n\
             pqweb_streams_total {st}\n\
             pqweb_streams_active {sa}\n\
             pqweb_clients_online {co}\n\
             pqweb_bytes_up_total {bu}\n\
             pqweb_bytes_down_total {bd}\n\
             pqweb_rejected_not_allowed_total {rna}\n\
             pqweb_rejected_limit_total {rl}\n\
             pqweb_connect_failed_total {cf}\n\
             pqweb_pings_total {pi}\n\
             pqweb_benches_total {be}\n",
            ver = env!("CARGO_PKG_VERSION"),
            eid = endpoint_id,
            conn = g(&self.connections),
            st = g(&self.streams_total),
            sa = g(&self.streams_active),
            co = clients_online,
            bu = g(&self.bytes_up),
            bd = g(&self.bytes_down),
            rna = g(&self.rejected_not_allowed),
            rl = g(&self.rejected_limit),
            cf = g(&self.connect_failed),
            pi = g(&self.pings),
            be = g(&self.benches),
        )
    }
}

/// Минимальный HTTP/1.1-сервер без зависимостей: GET /metrics, GET /healthz.
pub async fn serve_http(
    bind: std::net::SocketAddr,
    metrics: Arc<Metrics>,
    render_extra: Arc<dyn Fn() -> (usize, String) + Send + Sync>,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind).await?;
    tracing::info!("metrics: http://{bind}/metrics");
    loop {
        let (mut sock, _) = listener.accept().await?;
        let metrics = metrics.clone();
        let extra = render_extra.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let n = match tokio::time::timeout(std::time::Duration::from_secs(3), sock.read(&mut buf)).await {
                Ok(Ok(n)) => n,
                _ => return,
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split_whitespace().nth(1).unwrap_or("/");
            let (status, ctype, body) = match path {
                "/metrics" => {
                    let (clients, eid) = extra();
                    ("200 OK", "text/plain; version=0.0.4", metrics.render(clients, &eid))
                }
                "/healthz" => ("200 OK", "text/plain", "ok\n".to_string()),
                _ => ("404 Not Found", "text/plain", "not found\n".to_string()),
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
    }
}
