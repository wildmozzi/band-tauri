//! 피어 시계 오프셋 추정(P2-3 공유 메트로놈, 설계 2절 "Pong 평문"·1.4절).
//!
//! 각 엔진의 시계는 `NetShared::now_us()`(Net epoch부터의 µs)다. Ping의 timestamp(내 시계 하위 32비트)와
//! Pong 평문의 응답자 시계(u64)로 NTP식 표본을 만든다.
//!
//! - 표본: `t0` = Ping 송신(내 시계), `t3` = Pong 수신, `r` = 상대가 Pong을 보낸 시각(상대 시계).
//!   `offset = r − (t0 + t3)/2`(상대 − 나), `rtt = t3 − t0`. 오차 ≤ (rtt − 최소 왕복)/2 + 경로 비대칭/2.
//! - 큐잉 지연이 큰 표본을 버리려고 2초 버킷마다 RTT 최소 표본 하나만 남긴다(최근 8버킷 = 16초).
//! - 드리프트(PC 시계 차, 수십~수백 ppm)는 버킷 최소 표본들의 가중 최소제곱 직선으로 따라간다.
//!   가중치는 그 창의 최소 RTT보다 RTT가 클수록 작다(큐잉이 섞인 표본의 영향 축소).
//! - RTT가 대칭으로 바뀌면 오프셋은 그대로다. 한 방향만 바뀌는(비대칭) 변화는 원리상 관측할 수 없다.

/// 버킷 길이(µs).
const BUCKET_US: f64 = 2_000_000.0;
/// 유지하는 지난 버킷 수(현재 버킷 별도). 8 × 2초 = 16초 창.
const BUCKETS: usize = 8;
/// 직선 기울기를 쓰려면 버킷 최소 표본이 이만큼, 시간 폭이 `SLOPE_MIN_SPAN_US` 이상이어야 한다.
const SLOPE_MIN_POINTS: usize = 3;
const SLOPE_MIN_SPAN_US: f64 = 3_000_000.0;
/// 기울기(드리프트) 상한(ppm). PC 시계 차의 현실 범위를 넉넉히 덮는다.
const MAX_DRIFT_PPM: f64 = 1000.0;
/// 가중치 척도: 근처 최소 RTT보다 이만큼 크면 가중치 1/2.
const WEIGHT_SCALE_US: f64 = 500.0;
/// 가중치 기준(근처 최소 RTT)을 찾는 시간 폭(µs, 앞뒤).
const LOCAL_US: f64 = 4_000_000.0;
/// 표본 RTT 상한(µs). 넘으면 버린다. 경로 비대칭으로 오프셋이 끌릴 수 있는 폭(RTT/2)도 이것으로 묶인다(PR #34 L5).
const MAX_RTT_US: f64 = 500_000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    /// (t0 + t3)/2 — 내 시계(µs).
    mid_us: f64,
    /// 상대 − 나(µs).
    offset_us: f64,
    rtt_us: f64,
}

/// 오프셋 추정 결과.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockEstimate {
    /// 요청 시각에서 (상대 시계 − 내 시계), µs.
    pub offset_us: f64,
    /// 창 안 최소 RTT(µs).
    pub rtt_us: f64,
    /// 추정 드리프트(상대 시계가 내 시계보다 빠른 비율, ppm). 기울기를 쓰지 못하면 0.
    pub drift_ppm: f64,
}

/// 피어 하나의 시계 오프셋 추정기.
#[derive(Debug, Default, Clone)]
pub struct ClockSync {
    bucket_start_us: Option<f64>,
    cur: Option<Sample>,
    /// 지난 버킷 최소 표본(최근이 앞).
    prev: [Option<Sample>; BUCKETS],
    samples: u64,
}

impl ClockSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// 지금까지 받은 표본 수.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Ping/Pong 표본 하나. `t0_us`/`t3_us` = 내 시계의 Ping 송신·Pong 수신, `remote_us` = 상대 시계.
    pub fn on_sample(&mut self, t0_us: f64, t3_us: f64, remote_us: f64) {
        let rtt = t3_us - t0_us;
        if !(0.0..MAX_RTT_US).contains(&rtt) || !remote_us.is_finite() {
            return;
        }
        let mid = (t0_us + t3_us) * 0.5;
        let s = Sample { mid_us: mid, offset_us: remote_us - mid, rtt_us: rtt };
        let start = *self.bucket_start_us.get_or_insert(mid);
        if mid - start >= BUCKET_US {
            let steps = (((mid - start) / BUCKET_US) as usize).min(BUCKETS + 1);
            for _ in 0..steps {
                self.prev.rotate_right(1);
                self.prev[0] = self.cur.take();
            }
            self.bucket_start_us = Some(start + steps as f64 * BUCKET_US);
            if steps > BUCKETS {
                // 오래 표본이 없었다: 새 기준에서 다시 시작
                self.bucket_start_us = Some(mid);
            }
        }
        if self.cur.is_none_or(|c| s.rtt_us <= c.rtt_us) {
            self.cur = Some(s);
        }
        self.samples += 1;
    }

    /// `at_us`(내 시계)에서의 오프셋 추정. 표본이 없으면 None.
    pub fn estimate(&self, at_us: f64) -> Option<ClockEstimate> {
        let pts = || self.cur.iter().chain(self.prev.iter().flatten());
        let rtt_min = pts().map(|s| s.rtt_us).fold(f64::INFINITY, f64::min);
        if !rtt_min.is_finite() {
            return None;
        }
        // 가중치 기준은 그 표본 근처(±LOCAL_US)의 최소 RTT다. 경로가 바뀌어 RTT 수준이 달라져도
        // 새 구간 표본이 옛 최소값에 눌려 무시되지 않는다(대칭 변화는 오프셋을 바꾸지 않으므로).
        let weight = |s: &Sample| {
            let local = pts()
                .filter(|o| (o.mid_us - s.mid_us).abs() <= LOCAL_US)
                .map(|o| o.rtt_us)
                .fold(f64::INFINITY, f64::min);
            let x = (s.rtt_us - local) / WEIGHT_SCALE_US;
            1.0 / (1.0 + x * x)
        };
        let (mut sw, mut st, mut so) = (0.0, 0.0, 0.0);
        let mut n = 0usize;
        let (mut tmin, mut tmax) = (f64::INFINITY, f64::NEG_INFINITY);
        for s in pts() {
            let w = weight(s);
            sw += w;
            st += w * s.mid_us;
            so += w * s.offset_us;
            tmin = tmin.min(s.mid_us);
            tmax = tmax.max(s.mid_us);
            n += 1;
        }
        let t_mean = st / sw;
        let o_mean = so / sw;
        let mut slope = 0.0;
        if n >= SLOPE_MIN_POINTS && tmax - tmin >= SLOPE_MIN_SPAN_US {
            let (mut num, mut den) = (0.0, 0.0);
            for s in pts() {
                let w = weight(s);
                let dt = s.mid_us - t_mean;
                num += w * dt * (s.offset_us - o_mean);
                den += w * dt * dt;
            }
            if den > 0.0 {
                slope = (num / den).clamp(-MAX_DRIFT_PPM * 1e-6, MAX_DRIFT_PPM * 1e-6);
            }
        }
        Some(ClockEstimate { offset_us: o_mean + slope * (at_us - t_mean), rtt_us: rtt_min, drift_ppm: slope * 1e6 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 결정적 의사 난수(xorshift).
    struct Rng(u64);
    impl Rng {
        fn next_f64(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
        /// 평균 `mean`의 지수 분포(큐잉 지연).
        fn exp(&mut self, mean: f64) -> f64 {
            -mean * (1.0 - self.next_f64()).max(1e-12).ln()
        }
    }

    /// 상대 시계 = 내 시계 × (1 + ppm) + offset. 편도 = base + 지수 큐잉.
    fn run(ppm: f64, offset: f64, base_one_way: impl Fn(f64) -> f64, queue_mean: f64, secs: f64) -> (ClockSync, f64) {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut c = ClockSync::new();
        let remote = |t: f64| t * (1.0 + ppm * 1e-6) + offset;
        let mut t = 1_000_000.0;
        let mut worst: f64 = 0.0;
        while t < secs * 1e6 {
            let d1 = base_one_way(t) + rng.exp(queue_mean);
            let r = remote(t + d1);
            let d2 = base_one_way(t) + rng.exp(queue_mean);
            c.on_sample(t, t + d1 + d2, r);
            t += 500_000.0;
            if t > 8e6 {
                let e = c.estimate(t).unwrap();
                worst = worst.max((e.offset_us - (remote(t) - t)).abs());
            }
        }
        (c, worst)
    }

    #[test]
    fn no_samples_none() {
        assert!(ClockSync::new().estimate(0.0).is_none());
    }

    #[test]
    fn exact_offset_without_jitter() {
        let (c, worst) = run(0.0, 123_456.0, |_| 5000.0, 0.0, 20.0);
        let e = c.estimate(20e6).unwrap();
        assert!((e.offset_us - 123_456.0).abs() < 1.0, "{e:?}");
        assert!(worst < 1.0);
        assert!((e.rtt_us - 10_000.0).abs() < 1.0);
    }

    #[test]
    fn tracks_drift_and_jitter_within_1ms() {
        for ppm in [-200.0, 0.0, 200.0] {
            let (c, worst) = run(ppm, -5e9, |_| 6000.0, 1500.0, 120.0);
            // 편도 큐잉 평균 1.5ms(p95 ≈ 4.5ms)의 거친 회선: 표본 오차 = (q1 − q2)/2
            assert!(worst < 1000.0, "ppm {ppm}: 최악 오차 {worst}µs");
            let e = c.estimate(120e6).unwrap();
            assert!((e.drift_ppm - ppm).abs() < 30.0, "ppm {ppm}: 추정 {}", e.drift_ppm);
        }
    }

    #[test]
    fn symmetric_rtt_change_keeps_offset() {
        // 20초에 편도 5ms → 20ms(대칭), 60초에 다시 3ms
        let one_way = |t: f64| if t < 20e6 { 5000.0 } else if t < 60e6 { 20_000.0 } else { 3000.0 };
        let (_, worst) = run(150.0, 777.0, one_way, 1000.0, 100.0);
        assert!(worst < 500.0, "최악 오차 {worst}µs");
    }

    #[test]
    fn rejects_bad_samples() {
        let mut c = ClockSync::new();
        c.on_sample(10.0, 5.0, 0.0);
        c.on_sample(0.0, 3e6, 0.0);
        c.on_sample(0.0, 10.0, f64::NAN);
        assert_eq!(c.samples(), 0);
    }
}
