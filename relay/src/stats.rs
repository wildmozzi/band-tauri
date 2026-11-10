//! 원자적 통계 + Prometheus 텍스트 출력 (`GET /metrics`).

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// 폐기 사유. 인덱스로 카운터 배열에 대응한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// magic/version/type/길이 불일치
    Malformed,
    /// 등록되지 않은(또는 만료된) 주소에서 온 RelayData
    Unregistered,
    /// 토큰 위조/형식 오류/ssrc 불일치
    BadToken,
    ExpiredToken,
    /// 전역 RelayBind 처리율 초과 (HMAC CPU 남용 방지)
    BindRateLimited,
    /// 내부 패킷 sender_ssrc가 등록된 ssrc와 다름
    SsrcMismatch,
    /// 같은 방에 dest_ssrc가 없음 (다른 방 포함)
    UnknownDest,
    /// 클라이언트별 대역/패킷률 상한 초과
    RateLimited,
}

impl DropReason {
    pub const ALL: [DropReason; 8] = [
        DropReason::Malformed,
        DropReason::Unregistered,
        DropReason::BadToken,
        DropReason::ExpiredToken,
        DropReason::BindRateLimited,
        DropReason::SsrcMismatch,
        DropReason::UnknownDest,
        DropReason::RateLimited,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DropReason::Malformed => "malformed",
            DropReason::Unregistered => "unregistered",
            DropReason::BadToken => "bad_token",
            DropReason::ExpiredToken => "expired_token",
            DropReason::BindRateLimited => "bind_rate_limited",
            DropReason::SsrcMismatch => "ssrc_mismatch",
            DropReason::UnknownDest => "unknown_dest",
            DropReason::RateLimited => "rate_limited",
        }
    }
}

/// 처리 지연 히스토그램 상한(µs). 마지막 버킷 이후는 +Inf.
pub const LATENCY_BUCKETS_US: [u64; 10] = [5, 10, 20, 50, 100, 200, 500, 1000, 5000, 20000];

#[derive(Default)]
pub struct Stats {
    pub rx_packets: AtomicU64,
    pub rx_bytes: AtomicU64,
    pub forwarded_packets: AtomicU64,
    pub forwarded_bytes: AtomicU64,
    pub bind_ok: AtomicU64,
    pub bind_rejected: AtomicU64,
    /// 이미 등록된 주소의 같은 토큰 keepalive (HMAC·버킷 없이 처리)
    pub bind_keepalive: AtomicU64,
    /// 새 주소에 보낸 CookieRequired 응답
    pub cookie_sent: AtomicU64,
    pub send_errors: AtomicU64,
    pub recv_errors: AtomicU64,
    pub registrations: AtomicU64,
    pub rooms: AtomicU64,
    drops: [AtomicU64; 8],
    latency_buckets: [AtomicU64; LATENCY_BUCKETS_US.len() + 1],
    latency_sum_ns: AtomicU64,
    latency_count: AtomicU64,
    latency_max_ns: AtomicU64,
}

impl Stats {
    pub fn count_drop(&self, r: DropReason) {
        self.drops[r as usize].fetch_add(1, Relaxed);
    }

    pub fn drops(&self, r: DropReason) -> u64 {
        self.drops[r as usize].load(Relaxed)
    }

    /// 수신 → 송신 완료까지의 처리 지연 기록
    pub fn record_latency(&self, ns: u64) {
        let us = ns / 1000;
        let idx = LATENCY_BUCKETS_US.iter().position(|&b| us < b).unwrap_or(LATENCY_BUCKETS_US.len());
        self.latency_buckets[idx].fetch_add(1, Relaxed);
        self.latency_sum_ns.fetch_add(ns, Relaxed);
        self.latency_count.fetch_add(1, Relaxed);
        self.latency_max_ns.fetch_max(ns, Relaxed);
    }

    pub fn latency_count(&self) -> u64 {
        self.latency_count.load(Relaxed)
    }

    /// 히스토그램 기반 분위수 근사(µs, 버킷 상한). 표본이 없으면 None.
    pub fn latency_quantile_us(&self, q: f64) -> Option<u64> {
        let total = self.latency_count();
        if total == 0 {
            return None;
        }
        let target = ((total as f64) * q).ceil().max(1.0) as u64;
        let mut acc = 0;
        for (i, b) in self.latency_buckets.iter().enumerate() {
            acc += b.load(Relaxed);
            if acc >= target {
                return Some(LATENCY_BUCKETS_US.get(i).copied().unwrap_or(u64::MAX));
            }
        }
        Some(u64::MAX)
    }

    /// 한 줄 요약 (주기 로그용)
    pub fn summary(&self) -> String {
        let n = self.latency_count();
        let avg_us = if n > 0 { self.latency_sum_ns.load(Relaxed) as f64 / n as f64 / 1000.0 } else { 0.0 };
        let dropped: u64 = DropReason::ALL.iter().map(|&r| self.drops(r)).sum();
        format!(
            "regs={} rooms={} rx={} fwd={} drop={} bind_ok={} bind_rej={} keepalive={} cookie={} lat_avg={:.1}us lat_p99<={}us lat_max={}us",
            self.registrations.load(Relaxed),
            self.rooms.load(Relaxed),
            self.rx_packets.load(Relaxed),
            self.forwarded_packets.load(Relaxed),
            dropped,
            self.bind_ok.load(Relaxed),
            self.bind_rejected.load(Relaxed),
            self.bind_keepalive.load(Relaxed),
            self.cookie_sent.load(Relaxed),
            avg_us,
            self.latency_quantile_us(0.99).unwrap_or(0),
            self.latency_max_ns.load(Relaxed) / 1000,
        )
    }

    /// Prometheus 텍스트 포맷
    pub fn render_prometheus(&self) -> String {
        let mut s = String::with_capacity(2048);
        let mut counter = |name: &str, help: &str, kind: &str, v: u64| {
            let _ = writeln!(s, "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {v}");
        };
        counter("relay_rx_packets_total", "received datagrams", "counter", self.rx_packets.load(Relaxed));
        counter("relay_rx_bytes_total", "received bytes", "counter", self.rx_bytes.load(Relaxed));
        counter("relay_forwarded_packets_total", "forwarded engine packets", "counter", self.forwarded_packets.load(Relaxed));
        counter("relay_forwarded_bytes_total", "forwarded bytes", "counter", self.forwarded_bytes.load(Relaxed));
        counter("relay_bind_ok_total", "accepted RelayBind", "counter", self.bind_ok.load(Relaxed));
        counter("relay_bind_rejected_total", "valid token but room/server full or ssrc conflict", "counter", self.bind_rejected.load(Relaxed));
        counter("relay_bind_keepalive_total", "keepalive binds (fast path)", "counter", self.bind_keepalive.load(Relaxed));
        counter("relay_cookie_sent_total", "CookieRequired replies to new addresses", "counter", self.cookie_sent.load(Relaxed));
        counter("relay_send_errors_total", "send_to failures", "counter", self.send_errors.load(Relaxed));
        counter("relay_recv_errors_total", "recv_from failures (skipped)", "counter", self.recv_errors.load(Relaxed));
        counter("relay_registrations", "active registrations", "gauge", self.registrations.load(Relaxed));
        counter("relay_rooms", "active rooms", "gauge", self.rooms.load(Relaxed));

        let _ = writeln!(s, "# HELP relay_dropped_total dropped datagrams by reason\n# TYPE relay_dropped_total counter");
        for r in DropReason::ALL {
            let _ = writeln!(s, "relay_dropped_total{{reason=\"{}\"}} {}", r.label(), self.drops(r));
        }

        let _ = writeln!(
            s,
            "# HELP relay_processing_seconds recv-to-send latency inside the relay\n# TYPE relay_processing_seconds histogram"
        );
        let mut acc = 0;
        for (i, b) in self.latency_buckets.iter().enumerate() {
            acc += b.load(Relaxed);
            match LATENCY_BUCKETS_US.get(i) {
                Some(us) => {
                    let _ = writeln!(s, "relay_processing_seconds_bucket{{le=\"{}\"}} {acc}", *us as f64 / 1e6);
                }
                None => {
                    let _ = writeln!(s, "relay_processing_seconds_bucket{{le=\"+Inf\"}} {acc}");
                }
            }
        }
        let _ = writeln!(s, "relay_processing_seconds_sum {}", self.latency_sum_ns.load(Relaxed) as f64 / 1e9);
        let _ = writeln!(s, "relay_processing_seconds_count {}", self.latency_count());
        let _ = writeln!(
            s,
            "# HELP relay_processing_max_seconds max recv-to-send latency\n# TYPE relay_processing_max_seconds gauge\nrelay_processing_max_seconds {}",
            self.latency_max_ns.load(Relaxed) as f64 / 1e9
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_and_render() {
        let s = Stats::default();
        assert_eq!(s.latency_quantile_us(0.5), None);
        for _ in 0..98 {
            s.record_latency(3_000); // 3µs → 5µs 버킷
        }
        s.record_latency(150_000); // 150µs → 200µs 버킷
        s.record_latency(50_000_000); // 50ms → +Inf
        assert_eq!(s.latency_quantile_us(0.5), Some(5));
        assert_eq!(s.latency_quantile_us(0.99), Some(200));
        assert_eq!(s.latency_quantile_us(1.0), Some(u64::MAX));
        s.count_drop(DropReason::RateLimited);
        let text = s.render_prometheus();
        assert!(text.contains("relay_dropped_total{reason=\"rate_limited\"} 1"));
        assert!(text.contains("relay_processing_seconds_bucket{le=\"+Inf\"} 100"));
        assert!(text.contains("relay_processing_seconds_count 100"));
    }
}
