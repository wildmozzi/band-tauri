//! RT 콜백 실행시간 히스토그램(P1-10, 계층 1). 창 회전·보고는 `engine::cpuprof`.
//!
//! 입·출력 콜백(`rt.rs`의 `process`)이 시작과 끝에서 `Instant::now()`를 읽고, 걸린 시간을 사전할당 원자 칸에
//! 더한다(원자 덧셈 3번, 할당·락·로그 없음). Windows의 `Instant::now()`는 `QueryPerformanceCounter`다.
//! 불변 TSC가 있는 기기(Vista 이후 x64 대부분)에서 QPC는 커널 진입 없이 사용자 모드에서 RDTSC와
//! `KUSER_SHARED_DATA`의 배율만 읽는다(리눅스 vDSO `clock_gettime`과 같은 종류). 블로킹·할당이 없고
//! 수십 ns다(PR 본문 실측). TSC를 못 믿는 기기(일부 VM)에서는 HPET 등으로 폴백해 커널을 거치지만 그래도
//! 막히지 않는 읽기 전용 호출이다. 금지 대상인 "시스템콜"은 막히거나 스케줄러를 부르는 호출(대기, 할당, I/O)을 뜻한다.
//!
//! 출력 콜백(믹스, 마감이 걸린 쪽)은 히스토그램(p50/p99/최대)에, 입력 콜백은 최대값만 남긴다. 둘 다 실행시간 합에
//! 더해 "콜백 사용률"(창 벽시계 대비 %, 코어 1개 = 100)을 낸다.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use crate::net::txlat::{bucket_lower, bucket_of, quantile_us, BUCKETS};

/// RT 콜백 실행시간 누적(콜백 → 통계 조회). 모두 원자값이다.
#[derive(Debug)]
pub struct RtTiming {
    /// 출력 콜백 실행시간 칸(µs, `net::txlat`과 같은 칸: 1µs 칸 0..16µs, 그 위 상대 오차 ≤ 12.5%).
    out_buckets: Box<[AtomicU32]>,
    out_max_us: AtomicU64,
    in_max_us: AtomicU64,
    /// 입·출력 콜백 실행시간 합(ns).
    busy_ns: AtomicU64,
}

impl Default for RtTiming {
    fn default() -> Self {
        Self {
            out_buckets: (0..BUCKETS).map(|_| AtomicU32::new(0)).collect(),
            out_max_us: AtomicU64::new(0),
            in_max_us: AtomicU64::new(0),
            busy_ns: AtomicU64::new(0),
        }
    }
}

/// 한 창의 콜백 실행시간 요약.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RtWindow {
    pub load_pct: f32,
    pub out_p50_us: f32,
    pub out_p99_us: f32,
    pub out_max_us: f32,
    pub in_max_us: f32,
    pub callbacks: u32,
}

#[inline]
fn nanos(d: Duration) -> u64 {
    d.as_nanos().min(u64::MAX as u128) as u64
}

impl RtTiming {
    /// RT: 출력 콜백 1회 실행시간 기록.
    #[inline]
    pub fn record_out(&self, d: Duration) {
        let ns = nanos(d);
        let us = ns / 1000;
        if let Some(b) = self.out_buckets.get(bucket_of(us)) {
            b.fetch_add(1, Ordering::Relaxed);
        }
        self.out_max_us.fetch_max(us, Ordering::Relaxed);
        self.busy_ns.fetch_add(ns, Ordering::Relaxed);
    }

    /// RT: 입력 콜백 1회 실행시간 기록.
    #[inline]
    pub fn record_in(&self, d: Duration) {
        let ns = nanos(d);
        self.in_max_us.fetch_max(ns / 1000, Ordering::Relaxed);
        self.busy_ns.fetch_add(ns, Ordering::Relaxed);
    }

    /// 비RT: 현재 창을 비우고 요약한다. `window` = 직전 비움 이후 벽시계.
    pub fn take(&self, window: Duration) -> RtWindow {
        let mut counts = [0u32; BUCKETS];
        let mut total = 0u64;
        for (c, b) in counts.iter_mut().zip(self.out_buckets.iter()) {
            *c = b.swap(0, Ordering::Relaxed);
            total += *c as u64;
        }
        // 창 경계 경합은 송신 큐 지연과 같게 처리한다: 최대는 가장 높은 비지 않은 칸의 하한 아래로 내리지 않는다.
        let top = counts.iter().rposition(|&c| c > 0).map_or(0, bucket_lower);
        let out_max = self.out_max_us.swap(0, Ordering::Relaxed).max(top);
        let in_max = self.in_max_us.swap(0, Ordering::Relaxed);
        let busy = self.busy_ns.swap(0, Ordering::Relaxed);
        let win_ns = nanos(window).max(1);
        RtWindow {
            load_pct: (busy as f64 / win_ns as f64 * 100.0) as f32,
            out_p50_us: quantile_us(&counts, total, out_max as f64, 0.50) as f32,
            out_p99_us: quantile_us(&counts, total, out_max as f64, 0.99) as f32,
            out_max_us: out_max as f32,
            in_max_us: in_max as f32,
            callbacks: total.min(u32::MAX as u64) as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_quantiles_load_and_reset() {
        let t = RtTiming::default();
        // 출력 98회 × 20µs, 1회 300µs, 1회 900µs; 입력 100회 × 5µs
        for _ in 0..98 {
            t.record_out(Duration::from_micros(20));
        }
        t.record_out(Duration::from_micros(300));
        t.record_out(Duration::from_micros(900));
        for _ in 0..100 {
            t.record_in(Duration::from_micros(5));
        }
        let w = t.take(Duration::from_millis(100));
        assert_eq!(w.callbacks, 100);
        assert!((w.out_p50_us - 20.0).abs() < 3.0, "{w:?}");
        assert!((w.out_p99_us - 300.0).abs() < 40.0, "{w:?}");
        assert_eq!(w.out_max_us, 900.0);
        assert_eq!(w.in_max_us, 5.0);
        // 합 = 1960 + 300 + 900 + 500 = 3660µs / 100ms = 3.66%
        assert!((w.load_pct - 3.66).abs() < 0.01, "{w:?}");
        assert_eq!(t.take(Duration::from_millis(100)), RtWindow::default());
    }
}
