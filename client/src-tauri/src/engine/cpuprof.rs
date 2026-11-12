//! CPU 프로파일링(P1-10, GOAL 2절 "6인 풀메시 CPU ≤ 15%"): RT 콜백 실행시간 히스토그램과 워커 스레드 CPU.
//!
//! ## RT 쪽(`RtTiming::record_*`)
//! 입·출력 콜백(`rt.rs`의 `process`)이 시작과 끝에서 `Instant::now()`를 읽고, 걸린 시간을 사전할당 원자 칸에
//! 더한다(원자 덧셈 3번, 할당·락·로그 없음). Windows의 `Instant::now()`는 `QueryPerformanceCounter`다.
//! 불변 TSC가 있는 기기(Vista 이후 x64 대부분)에서 QPC는 커널 진입 없이 사용자 모드에서 RDTSC와
//! `KUSER_SHARED_DATA`의 배율만 읽는다(리눅스 vDSO `clock_gettime`과 같은 종류). 블로킹·할당이 없고
//! 수십 ns다(PR 본문 실측). TSC를 못 믿는 기기(일부 VM)에서는 HPET 등으로 폴백해 커널을 거치지만 그래도
//! 막히지 않는 읽기 전용 호출이다. 금지 대상인 "시스템콜"은 막히거나 스케줄러를 부르는 호출(대기, 할당, I/O)을 뜻한다.
//!
//! 출력 콜백(믹스, 마감이 걸린 쪽)은 히스토그램(p50/p99/최대)에, 입력 콜백은 최대값만 남긴다. 둘 다 실행시간 합에
//! 더해 "콜백 사용률"(창 벽시계 대비 %, 코어 1개 = 100)을 낸다.
//!
//! ## 비RT 쪽(`CpuSampler`)
//! `Engine::stats`가 부를 때 `WINDOW`(2초)가 지났으면 칸을 비워 창 통계를 계산하고, 워커 CPU%를 낸다.
//! 2초가 안 됐으면 직전 창 값을 돌려준다. 통계를 오래 읽지 않으면 창이 그만큼 길어진다(`windowMs`).
//!
//! 워커 CPU% = 워커 스레드 누적 사이클(`QueryThreadCycleTime`)의 창 증분 / 같은 창의 TSC 증분(`RDTSC`).
//! Windows는 스레드 사이클을 문맥 전환 때 TSC로 재서 더하므로 두 값은 같은 단위(TSC 틱)이고, 주파수 환산 없이
//! 나누면 곧 "창 벽시계 중 이 스레드가 코어를 쓴 비율"이다. `GetThreadTimes`는 클럭 틱(15.625ms) 단위 샘플링이라
//! 타이머로 깨어나 짧게 일하는 워커에서 ±50% 틀렸다(PR #54 리뷰 H1, `cycles_track_sleepy_busy_thread`가 고정).
//! 불변 TSC가 없는 기기(아주 오래된 CPU, 일부 VM)에서는 TSC 속도가 변해 오차가 생길 수 있다.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::net::txlat::{bucket_lower, bucket_of, quantile_us, BUCKETS};
use crate::sys::cpu::cycle_pct;

/// 통계 창. 송신 큐 지연(`sendQueue`)과 같다.
pub const WINDOW: Duration = crate::net::txlat::WINDOW;

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

/// 프론트로 나가는 CPU 통계(`EngineStats.cpu`, 직전 창).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuStats {
    /// 워커 스레드 CPU(%, 코어 1개 = 100, 스레드 사이클 / TSC). Windows x64가 아니거나 워커가 없거나 첫 창 전이면 null.
    pub worker_pct: Option<f32>,
    /// RT 콜백(입력+출력) 실행시간 합 / 창 벽시계(%, 코어 1개 = 100).
    pub callback_load_pct: f32,
    /// 출력 콜백 1회 실행시간 중앙값·99백분위·최대(µs).
    pub callback_p50_us: f32,
    pub callback_p99_us: f32,
    pub callback_max_us: f32,
    /// 출력 콜백 p99 / 장치 출력 주기(%). 100에 가까우면 마감을 놓친다.
    pub callback_p99_period_pct: f32,
    /// 입력 콜백 1회 실행시간 최대(µs).
    pub input_callback_max_us: f32,
    /// 창 안의 출력 콜백 수(0이면 콜백 값은 의미 없음).
    pub callbacks: u32,
    /// 창 길이(ms). 0이면 아직 첫 창이 끝나지 않았다.
    pub window_ms: f32,
}

/// 창 회전 상태(엔진이 Mutex로 쥔다, 비RT).
#[derive(Debug)]
pub struct CpuSampler {
    at: Instant,
    /// 직전 회전 시점의 TSC(창 분모).
    tsc: Option<u64>,
    /// 직전 회전 시점의 워커 누적 사이클.
    worker_cycles: Option<u64>,
    last: CpuStats,
}

impl CpuSampler {
    /// `now`·`tsc` = 엔진 시작(워커 스레드 생성 전). 워커 사이클 기준은 0이다(새 스레드).
    pub fn new(now: Instant, tsc: Option<u64>) -> Self {
        Self { at: now, tsc, worker_cycles: Some(0), last: CpuStats::default() }
    }

    /// 창이 지났으면 회전해 새 통계를, 아니면 직전 통계를 돌려준다. `tsc` = 지금 TSC(`tsc_now`),
    /// `worker_cycles` = 워커 누적 사이클(`thread_cycles`), `period_us` = 장치 출력 주기.
    pub fn sample(&mut self, now: Instant, timing: &RtTiming, tsc: Option<u64>, worker_cycles: Option<u64>, period_us: f32) -> CpuStats {
        let win = now.saturating_duration_since(self.at);
        if win < WINDOW {
            return self.last;
        }
        let rt = timing.take(win);
        let secs = win.as_secs_f64();
        let worker_pct = cycle_pct(worker_cycles, self.worker_cycles, tsc, self.tsc);
        self.at = now;
        self.tsc = tsc;
        self.worker_cycles = worker_cycles;
        self.last = CpuStats {
            worker_pct,
            callback_load_pct: rt.load_pct,
            callback_p50_us: rt.out_p50_us,
            callback_p99_us: rt.out_p99_us,
            callback_max_us: rt.out_max_us,
            callback_p99_period_pct: if period_us > 0.0 { rt.out_p99_us / period_us * 100.0 } else { 0.0 },
            input_callback_max_us: rt.in_max_us,
            callbacks: rt.callbacks,
            window_ms: (secs * 1000.0) as f32,
        };
        self.last
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

    #[test]
    fn sampler_rotates_only_after_window() {
        let t = RtTiming::default();
        let t0 = Instant::now();
        // TSC 1000틱 = 1초로 둔다
        let mut s = CpuSampler::new(t0, Some(5_000));
        t.record_out(Duration::from_micros(100));
        // 창 전: 아직 빈 통계, 칸은 그대로
        assert_eq!(s.sample(t0 + Duration::from_secs(1), &t, Some(6_000), Some(100), 2000.0), CpuStats::default());
        let c = s.sample(t0 + Duration::from_secs(2), &t, Some(7_000), Some(200), 2000.0);
        assert_eq!(c.callbacks, 1);
        assert_eq!(c.window_ms, 2000.0);
        assert!((c.worker_pct.unwrap() - 10.0).abs() < 1e-3, "200 / 2000 TSC = 10%: {c:?}");
        assert!((c.callback_p99_period_pct - 5.0).abs() < 0.7, "100µs / 2ms: {c:?}");
        // 다음 창 전에는 같은 값, 다음 창은 증분
        assert_eq!(s.sample(t0 + Duration::from_secs(3), &t, Some(8_000), Some(900), 2000.0), c);
        let d = s.sample(t0 + Duration::from_secs(4), &t, Some(9_000), Some(300), 2000.0);
        assert!((d.worker_pct.unwrap() - 5.0).abs() < 1e-3, "{d:?}");
        assert_eq!(d.callbacks, 0);
        // 워커 사이클이나 TSC를 못 읽으면 null(다음 창도 기준이 없어 null)
        let e = s.sample(t0 + Duration::from_secs(6), &t, Some(11_000), None, 0.0);
        assert_eq!(e.worker_pct, None);
        assert_eq!(e.callback_p99_period_pct, 0.0);
        assert_eq!(s.sample(t0 + Duration::from_secs(8), &t, None, Some(400), 0.0).worker_pct, None);
    }

    #[test]
    fn serializes_camel_case() {
        let j = serde_json::to_string(&CpuStats::default()).unwrap();
        for k in ["workerPct\":null", "callbackLoadPct", "callbackP50Us", "callbackP99Us", "callbackMaxUs", "callbackP99PeriodPct", "inputCallbackMaxUs", "callbacks", "windowMs"] {
            assert!(j.contains(k), "{k}: {j}");
        }
    }
}
