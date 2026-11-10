//! 실행: `RELAY_SECRET=... band-relay`
//!
//! | env | 기본값 | 설명 |
//! |---|---|---|
//! | `RELAY_SECRET` | (필수, ≥32바이트) | 시그널링 서버와 공유하는 HMAC 키 |
//! | `RELAY_BIND` | `0.0.0.0:5000` | UDP 바인드. fly.io에서는 `fly-global-services:5000` |
//! | `RELAY_METRICS_BIND` | `0.0.0.0:9090` | `/metrics`, `/healthz` (빈 문자열이면 끔) |
//! | `RELAY_MAX_BPS` | 2000000 | 클라이언트당 bit/s 상한 |
//! | `RELAY_MAX_PPS` | 2500 | 클라이언트당 packet/s 상한 |
//! | `RELAY_MAX_BINDS_PER_SEC` | 20000 | 신규/갱신 RelayBind 서버 전체 처리율 (keepalive 제외) |
//! | `RELAY_MAX_BINDS_PER_SEC_PER_SUBNET` | 50 | 신규/갱신 RelayBind 출발 서브넷(/24, /64)당 처리율 |
//! | `RELAY_MAX_REGISTRATIONS` | 20000 | 전체 등록 수 상한 |
//! | `RELAY_STATS_LOG_SECS` | 60 | 통계 로그 주기 (0이면 끔) |

use std::sync::Arc;
use std::time::Duration;

use band_relay::{server, Limits, Relay, Stats};
use tokio::net::{lookup_host, TcpListener, UdpSocket};

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v.trim().parse().unwrap_or_else(|_| panic!("{key} 값이 올바르지 않습니다: {v}")),
        _ => default,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let secret = std::env::var("RELAY_SECRET").unwrap_or_default();
    if secret.len() < 32 {
        eprintln!("[relay] RELAY_SECRET(32바이트 이상)이 필요합니다");
        std::process::exit(2);
    }
    let defaults = Limits::default();
    let limits = Limits {
        max_bits_per_sec: env_or("RELAY_MAX_BPS", defaults.max_bits_per_sec),
        max_packets_per_sec: env_or("RELAY_MAX_PPS", defaults.max_packets_per_sec),
        max_binds_per_sec: env_or("RELAY_MAX_BINDS_PER_SEC", defaults.max_binds_per_sec),
        max_binds_per_sec_per_subnet: env_or("RELAY_MAX_BINDS_PER_SEC_PER_SUBNET", defaults.max_binds_per_sec_per_subnet),
        max_registrations: env_or("RELAY_MAX_REGISTRATIONS", defaults.max_registrations),
        ..defaults
    };
    let bind = std::env::var("RELAY_BIND").unwrap_or_else(|_| "0.0.0.0:5000".into());
    // fly.io UDP는 `fly-global-services` 주소에 바인드해야 응답의 출발 주소가 올바르다 → 이름 해석 후 바인드
    let addr = lookup_host(&bind)
        .await?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| std::io::Error::other(format!("RELAY_BIND 주소를 해석할 수 없습니다: {bind}")))?;
    let sock = UdpSocket::bind(addr).await?;
    eprintln!("[relay] UDP {} (요청: {bind}) limits={limits:?}", sock.local_addr()?);

    let stats = Arc::new(Stats::default());
    let metrics_bind = std::env::var("RELAY_METRICS_BIND").unwrap_or_else(|_| "0.0.0.0:9090".into());
    if !metrics_bind.is_empty() {
        let listener = TcpListener::bind(&metrics_bind).await?;
        eprintln!("[relay] metrics http://{}/metrics", listener.local_addr()?);
        let s = stats.clone();
        tokio::spawn(async move {
            if let Err(e) = server::serve_metrics(listener, s).await {
                eprintln!("[relay] metrics 서버 종료: {e}");
            }
        });
    }
    let log_secs: u64 = env_or("RELAY_STATS_LOG_SECS", 60);
    if log_secs > 0 {
        let s = stats.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(log_secs));
            t.tick().await;
            loop {
                t.tick().await;
                eprintln!("[relay] {}", s.summary());
            }
        });
    }

    let relay = Relay::new(secret.into_bytes(), limits, stats);
    tokio::select! {
        _ = server::run_udp(sock, relay) => Ok(()),
        _ = tokio::signal::ctrl_c() => {
            eprintln!("[relay] 종료");
            Ok(())
        }
    }
}
