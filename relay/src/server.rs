//! 소켓 루프와 메트릭 HTTP 엔드포인트.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

use crate::relay::{Outcome, Relay};
use crate::stats::Stats;

/// 수신 버퍼 (릴레이 패킷 최대 크기보다 충분히 크게)
const RECV_BUF_BYTES: usize = 2048;

/// 만료 등록·쉬는 서브넷 버킷 정리와 `now_unix` 갱신 주기
const SWEEP_EVERY: Duration = Duration::from_secs(1);

/// 연속 수신 오류가 이만큼이면 1ms 쉰다(지속 오류에서 바쁜 루프 방지)
const RECV_ERROR_BACKOFF_AFTER: u32 = 64;

/// 메트릭 HTTP 동시 연결 상한. 넘는 연결은 응답 없이 바로 닫는다(감사 L3 — 연결을 열어 두기만 해도 태스크·메모리가 쌓였다).
/// 스크레이프·헬스체크는 동시에 몇 개뿐이다.
pub const METRICS_MAX_CONNECTIONS: usize = 16;

/// 메트릭 HTTP 요청 읽기·응답 쓰기 각각의 시한(쓰기 시한은 감사 L3 — 읽지 않는 상대가 쓰기 태스크를 붙잡지 않게)
const METRICS_IO_TIMEOUT: Duration = Duration::from_secs(5);

/// 메트릭 연결 하나의 자리(놓으면 수를 줄인다)
struct MetricsSlot(Arc<AtomicUsize>);

impl Drop for MetricsSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// UDP 릴레이 루프. 단일 태스크에서 수신→처리→송신을 순서대로 수행한다(패킷당 할당 없음).
pub async fn run_udp(sock: UdpSocket, mut relay: Relay) {
    let stats = relay.stats().clone();
    let mut buf = [0u8; RECV_BUF_BYTES];
    let mut sweep = tokio::time::interval(SWEEP_EVERY);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // unix 초는 토큰 만료·쿠키 창 판정에만 쓰므로 정리 주기(1초)로 갱신해도 충분하다
    let mut now_unix = unix_now();
    // 정리 마감. 수신 갈래에서도 본다: 소켓이 쉬지 않고 차 있으면 `biased` select가 늘 수신을 골라 정리 틱이 굶는다
    // (감사 M1 — 폭주 중 만료 등록이 쌓여 `ServerFull`이 되고, `now_unix`가 멈춰 만료된 토큰·쿠키를 받았다).
    let mut next_sweep = Instant::now() + SWEEP_EVERY;
    // 연속 수신 오류 수. 데이터그램 단위 오류(Windows WSAEMSGSIZE — 2048B 넘는 데이터그램 등)는 쉬지 않고 넘어가고, 지속
    // 오류(연속 `RECV_ERROR_BACKOFF_AFTER`회)일 때만 잠깐 쉰다(감사 L1 — 전에는 오류마다 1ms를 쉬어 큰 데이터그램 폭주로
    // 릴레이 전체가 느려졌다).
    let mut consecutive_errors = 0u32;
    loop {
        tokio::select! {
            biased;
            r = sock.recv_from(&mut buf) => {
                let (n, from) = match r {
                    Ok(v) => v,
                    // Windows: 이전 송신 대상이 ICMP port unreachable을 돌려주면 recv가 ConnectionReset을 낸다
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                    // 그 밖의 데이터그램 단위 오류(Windows WSAEMSGSIZE 등)로 릴레이가 죽지 않게 세고 넘어간다.
                    // 지속 오류일 때만 바쁜 루프가 되지 않도록 잠깐 쉰다.
                    Err(_) => {
                        stats.recv_errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        consecutive_errors += 1;
                        if consecutive_errors >= RECV_ERROR_BACKOFF_AFTER {
                            consecutive_errors = 0;
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        continue;
                    }
                };
                consecutive_errors = 0;
                let t0 = Instant::now();
                if t0 >= next_sweep {
                    now_unix = unix_now();
                    relay.sweep(t0);
                    next_sweep = t0 + SWEEP_EVERY;
                }
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
                // 한가할 때의 정리. 수신 갈래가 막 정리했으면 그 마감부터 다시 센다.
                let now = Instant::now();
                now_unix = unix_now();
                relay.sweep(now);
                next_sweep = now + SWEEP_EVERY;
            }
        }
    }
}

/// `GET /metrics`(Prometheus 텍스트), `GET /healthz`. 최소 HTTP/1.1 구현.
/// 동시 연결은 [`METRICS_MAX_CONNECTIONS`]까지, 읽기·쓰기는 각각 [`METRICS_IO_TIMEOUT`] 안에서만 기다린다.
pub async fn serve_metrics(listener: TcpListener, stats: Arc<Stats>) -> std::io::Result<()> {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let (mut conn, _) = listener.accept().await?;
        if active.fetch_add(1, Ordering::AcqRel) >= METRICS_MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            drop(conn);
            continue;
        }
        let slot = MetricsSlot(active.clone());
        let stats = stats.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let mut req = [0u8; 1024];
            let n = match tokio::time::timeout(METRICS_IO_TIMEOUT, conn.read(&mut req)).await {
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
            let _ = tokio::time::timeout(METRICS_IO_TIMEOUT, async {
                conn.write_all(resp.as_bytes()).await?;
                conn.shutdown().await
            })
            .await;
        });
    }
}
