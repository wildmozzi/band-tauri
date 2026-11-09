//! 소켓 루프와 메트릭 HTTP 엔드포인트.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

use crate::relay::{Outcome, Relay};
use crate::stats::Stats;

/// 수신 버퍼 (릴레이 패킷 최대 크기보다 충분히 크게)
const RECV_BUF_BYTES: usize = 2048;

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// UDP 릴레이 루프. 단일 태스크에서 수신→처리→송신을 순서대로 수행한다(패킷당 할당 없음).
pub async fn run_udp(sock: UdpSocket, mut relay: Relay) -> std::io::Result<()> {
    let stats = relay.stats().clone();
    let mut buf = [0u8; RECV_BUF_BYTES];
    let mut sweep = tokio::time::interval(Duration::from_secs(1));
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // unix 초는 토큰 만료 판정에만 쓰므로 sweep 주기로 갱신해도 충분하다
    let mut now_unix = unix_now();
    loop {
        tokio::select! {
            biased;
            r = sock.recv_from(&mut buf) => {
                let (n, from) = match r {
                    Ok(v) => v,
                    // Windows: 이전 송신 대상이 ICMP port unreachable을 돌려주면 recv가 ConnectionReset을 낸다
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                    Err(e) => return Err(e),
                };
                let t0 = Instant::now();
                let (to, out): (_, &[u8]) = match relay.handle(t0, now_unix, from, &buf[..n]) {
                    Outcome::Forward { to, offset } => (to, &buf[offset..n]),
                    Outcome::Ack { to } => (to, relay.ack()),
                    Outcome::Drop(_) => continue,
                };
                if sock.send_to(out, to).await.is_err() {
                    stats.send_errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                stats.record_latency(t0.elapsed().as_nanos() as u64);
            }
            _ = sweep.tick() => {
                now_unix = unix_now();
                relay.sweep(Instant::now());
            }
        }
    }
}

/// `GET /metrics`(Prometheus 텍스트), `GET /healthz`. 최소 HTTP/1.1 구현.
pub async fn serve_metrics(listener: TcpListener, stats: Arc<Stats>) -> std::io::Result<()> {
    loop {
        let (mut conn, _) = listener.accept().await?;
        let stats = stats.clone();
        tokio::spawn(async move {
            let mut req = [0u8; 1024];
            let n = match tokio::time::timeout(Duration::from_secs(5), conn.read(&mut req)).await {
                Ok(Ok(n)) => n,
                _ => return,
            };
            let line = req[..n].split(|&b| b == b'\r' || b == b'\n').next().unwrap_or(&[]);
            let (status, body) = if line.starts_with(b"GET /metrics") {
                ("200 OK", stats.render_prometheus())
            } else if line.starts_with(b"GET /healthz") {
                ("200 OK", "ok\n".to_string())
            } else {
                ("404 Not Found", "not found\n".to_string())
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = conn.write_all(resp.as_bytes()).await;
            let _ = conn.shutdown().await;
        });
    }
}
