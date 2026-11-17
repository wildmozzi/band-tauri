//! CPU 프로파일링(P1-10, GOAL 2절 "6인 풀메시 CPU ≤ 15%"): RT 콜백 실행시간 히스토그램과 워커 스레드 CPU.
//!
//! RT 콜백 실행시간 히스토그램(`RtTiming::record_*`)은 `engine::rt_timing`. 이 파일은 창 회전과 보고다.
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

use std::time::{Duration, Instant};

use serde::Serialize;

use crate::net::txlat::SendLatency;
use crate::sys::cpu::cycle_pct;
use band_dsp::rt_timing::RtTiming;

/// 통계 창. 송신 큐 지연(`sendQueue`)과 같다.
pub(crate) const WINDOW: Duration = crate::net::txlat::WINDOW;

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
    pub(crate) input_callback_max_us: f32,
    /// 창 안의 출력 콜백 수(0이면 콜백 값은 의미 없음).
    pub callbacks: u32,
    /// 창 길이(ms). 0이면 아직 첫 창이 끝나지 않았다.
    pub window_ms: f32,
    /// 워커 수면 overshoot(요청보다 늦게 깬 양) 중앙값·99백분위·최대(µs, 직전 창, P2-17c). 하이브리드 코어·타이머에 따라
    /// 수백 µs까지 간다. 은닉 마감 여유가 덮어야 하는 양은 대략 `FINE_POLL`(150µs) + p99 + 워커 반복 본문이다.
    pub worker_wake_p50_us: f32,
    pub worker_wake_p99_us: f32,
    pub worker_wake_max_us: f32,
    /// 지금 쓰는 은닉 마감 여유(µs, `DEADLINE_GUARD_US` — 고정값).
    pub deadline_guard_us: f32,
}

/// 창 회전 상태(엔진이 Mutex로 쥔다, 비RT).
#[derive(Debug)]
pub(crate) struct CpuSampler {
    at: Instant,
    /// 직전 회전 시점의 TSC(창 분모).
    tsc: Option<u64>,
    /// 직전 회전 시점의 워커 누적 사이클.
    worker_cycles: Option<u64>,
    last: CpuStats,
}

impl CpuSampler {
    /// `now`·`tsc` = 엔진 시작(워커 스레드 생성 전). 워커 사이클 기준은 0이다(새 스레드).
    pub(crate) fn new(now: Instant, tsc: Option<u64>) -> Self {
        Self { at: now, tsc, worker_cycles: Some(0), last: CpuStats::default() }
    }

    /// 창이 지났으면 회전해 새 통계를, 아니면 직전 통계를 돌려준다. `tsc` = 지금 TSC(`tsc_now`),
    /// `worker_cycles` = 워커 누적 사이클(`thread_cycles`), `period_us` = 장치 출력 주기.
    /// `wake` = 워커 수면 overshoot 분포(창마다 비운다), `guard_us` = 은닉 마감 여유.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sample(&mut self, now: Instant, timing: &RtTiming, tsc: Option<u64>, worker_cycles: Option<u64>, period_us: f32, wake: &SendLatency, guard_us: u64) -> CpuStats {
        let win = now.saturating_duration_since(self.at);
        if win < WINDOW {
            return self.last;
        }
        let rt = timing.take(win);
        let w = wake.rotate();
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
            worker_wake_p50_us: w.p50_ms * 1000.0,
            worker_wake_p99_us: w.p99_ms * 1000.0,
            worker_wake_max_us: w.max_ms * 1000.0,
            deadline_guard_us: guard_us as f32,
        };
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampler_rotates_only_after_window() {
        let t = RtTiming::default();
        let t0 = Instant::now();
        // TSC 1000틱 = 1초로 둔다
        let mut s = CpuSampler::new(t0, Some(5_000));
        t.record_out(Duration::from_micros(100));
        let w = SendLatency::new();
        w.record(Duration::from_micros(300));
        // 창 전: 아직 빈 통계, 칸은 그대로
        assert_eq!(s.sample(t0 + Duration::from_secs(1), &t, Some(6_000), Some(100), 2000.0, &w, 1500), CpuStats::default());
        let c = s.sample(t0 + Duration::from_secs(2), &t, Some(7_000), Some(200), 2000.0, &w, 1500);
        assert_eq!(c.callbacks, 1);
        assert_eq!(c.window_ms, 2000.0);
        // 수면 overshoot는 창마다 비운다(P2-17c): 첫 창에 300µs 하나, 다음 창은 0
        assert!((c.worker_wake_max_us - 300.0).abs() < 1.0 && c.worker_wake_p99_us > 250.0, "{c:?}");
        assert_eq!(c.deadline_guard_us, 1500.0);
        assert!((c.worker_pct.unwrap() - 10.0).abs() < 1e-3, "200 / 2000 TSC = 10%: {c:?}");
        assert!((c.callback_p99_period_pct - 5.0).abs() < 0.7, "100µs / 2ms: {c:?}");
        // 다음 창 전에는 같은 값, 다음 창은 증분
        assert_eq!(s.sample(t0 + Duration::from_secs(3), &t, Some(8_000), Some(900), 2000.0, &w, 1500), c);
        let d = s.sample(t0 + Duration::from_secs(4), &t, Some(9_000), Some(300), 2000.0, &w, 1500);
        assert!((d.worker_pct.unwrap() - 5.0).abs() < 1e-3, "{d:?}");
        assert_eq!(d.callbacks, 0);
        assert_eq!(d.worker_wake_max_us, 0.0);
        // 워커 사이클이나 TSC를 못 읽으면 null(다음 창도 기준이 없어 null)
        let e = s.sample(t0 + Duration::from_secs(6), &t, Some(11_000), None, 0.0, &w, 1500);
        assert_eq!(e.worker_pct, None);
        assert_eq!(e.callback_p99_period_pct, 0.0);
        assert_eq!(s.sample(t0 + Duration::from_secs(8), &t, None, Some(400), 0.0, &w, 1500).worker_pct, None);
    }

    #[test]
    fn serializes_camel_case() {
        let j = serde_json::to_string(&CpuStats::default()).unwrap();
        for k in ["workerPct\":null", "callbackLoadPct", "callbackP50Us", "callbackP99Us", "callbackMaxUs", "callbackP99PeriodPct", "inputCallbackMaxUs", "callbacks", "windowMs", "workerWakeP50Us", "workerWakeP99Us", "workerWakeMaxUs", "deadlineGuardUs"] {
            assert!(j.contains(k), "{k}: {j}");
        }
    }
}
