//! 송신 큐 지연 히스토그램(P1-3): 캡처 프레임 생성(워커가 프레임을 모은 시각) → 피어별 `send_to` 완료까지.
//!
//! 기록(`record`)은 워커 스레드의 송신 경로에서 원자 덧셈 두 번뿐이다(락·할당 없음). 제어 스레드가
//! `WINDOW`마다 `rotate`로 현재 창을 비워 p50/p99/최대를 계산해 둔다. 통계 조회는 그 스냅샷을 읽는다.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;

/// 분위수를 계산하는 창(통계는 직전 창 기준).
pub const WINDOW: Duration = Duration::from_secs(2);

/// 0..16µs는 1µs 칸, 그 위는 2의 거듭제곱 구간마다 8칸(상대 오차 ≤ 12.5%). 2^(4+17)µs ≈ 2.1s까지.
const LINEAR: usize = 16;
const SUB: usize = 8;
const OCTAVES: usize = 17;
pub const BUCKETS: usize = LINEAR + OCTAVES * SUB;

fn bucket_of(us: u64) -> usize {
    if us < LINEAR as u64 {
        return us as usize;
    }
    let e = 63 - us.leading_zeros() as usize; // us >= 16 → e >= 4
    let sub = ((us >> (e - 3)) & (SUB as u64 - 1)) as usize;
    (LINEAR + (e - 4) * SUB + sub).min(BUCKETS - 1)
}

/// 칸의 대표값(µs): 칸 하한 + 폭/2.
fn bucket_value(i: usize) -> f64 {
    if i < LINEAR {
        return i as f64 + 0.5;
    }
    let e = (i - LINEAR) / SUB + 4;
    let sub = (i - LINEAR) % SUB;
    let width = 1u64 << (e - 3);
    ((1u64 << e) + sub as u64 * width) as f64 + width as f64 / 2.0
}

/// 프론트로 나가는 송신 큐 지연 통계(`EngineStats.sendQueue`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendQueueStats {
    /// 직전 창(2초) 패킷별 지연 중앙값(ms).
    pub p50_ms: f32,
    /// 직전 창 99백분위(ms).
    pub p99_ms: f32,
    /// 직전 창 최대(ms, 칸 근사 아님 — 실측).
    pub max_ms: f32,
    /// 직전 창에 잰 오디오 패킷 수(0이면 위 값은 의미 없음).
    pub packets: u32,
}

pub struct SendLatency {
    buckets: Box<[AtomicU32]>,
    max_us: AtomicU64,
    last: Mutex<SendQueueStats>,
}

impl Default for SendLatency {
    fn default() -> Self {
        Self::new()
    }
}

impl SendLatency {
    pub fn new() -> Self {
        Self { buckets: (0..BUCKETS).map(|_| AtomicU32::new(0)).collect(), max_us: AtomicU64::new(0), last: Mutex::new(SendQueueStats::default()) }
    }

    /// 패킷 하나의 지연 기록(송신 경로, 원자값만).
    #[inline]
    pub fn record(&self, d: Duration) {
        let us = d.as_micros().min(u64::MAX as u128) as u64;
        self.buckets[bucket_of(us)].fetch_add(1, Ordering::Relaxed);
        self.max_us.fetch_max(us, Ordering::Relaxed);
    }

    /// 현재 창을 비우고 분위수를 계산해 스냅샷으로 둔다(제어 스레드).
    pub fn rotate(&self) -> SendQueueStats {
        let mut counts = [0u32; BUCKETS];
        let mut total: u64 = 0;
        for (c, b) in counts.iter_mut().zip(self.buckets.iter()) {
            *c = b.swap(0, Ordering::Relaxed);
            total += *c as u64;
        }
        let max_us = self.max_us.swap(0, Ordering::Relaxed);
        let q = |p: f64| -> f32 {
            // 순위 ceil(p·N)번째 표본이 든 칸
            let rank = ((p * total as f64).ceil() as u64).max(1);
            let mut acc = 0u64;
            for (i, &c) in counts.iter().enumerate() {
                acc += c as u64;
                if acc >= rank {
                    // 칸 대표값이 실측 최대를 넘지 않게
                    return (bucket_value(i).min(max_us as f64) / 1000.0) as f32;
                }
            }
            0.0
        };
        let s = if total == 0 {
            SendQueueStats::default()
        } else {
            SendQueueStats { p50_ms: q(0.50), p99_ms: q(0.99), max_ms: max_us as f32 / 1000.0, packets: total.min(u32::MAX as u64) as u32 }
        };
        *self.last.lock() = s;
        s
    }

    pub fn last(&self) -> SendQueueStats {
        *self.last.lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_monotonic_and_cover_value() {
        let mut prev = 0;
        for us in 0..200_000u64 {
            let b = bucket_of(us);
            assert!(b >= prev && b < BUCKETS, "us {us}");
            prev = b;
            // 대표값은 실제 값과 칸 폭 이내(상대 오차 ≤ 1/8 + 0.5µs)
            let v = bucket_value(b);
            assert!((v - us as f64).abs() <= us as f64 / 8.0 + 0.5, "us {us} → {v}");
        }
        assert_eq!(bucket_of(u64::MAX), BUCKETS - 1);
    }

    #[test]
    fn quantiles_and_window_reset() {
        let h = SendLatency::new();
        // 98개 20µs, 1개 1000µs, 1개 5000µs → p50 ≈ 20µs, p99 ≈ 1ms, max = 5ms
        for _ in 0..98 {
            h.record(Duration::from_micros(20));
        }
        h.record(Duration::from_micros(1000));
        h.record(Duration::from_micros(5000));
        let s = h.rotate();
        assert_eq!(s.packets, 100);
        assert!((s.p50_ms - 0.020).abs() < 0.003, "{s:?}");
        assert!((s.p99_ms - 1.0).abs() < 0.13, "{s:?}");
        assert_eq!(s.max_ms, 5.0);
        assert_eq!(h.last(), s);
        // 다음 창은 비어 있다
        let e = h.rotate();
        assert_eq!(e, SendQueueStats::default());
    }

    #[test]
    fn quantile_never_exceeds_max() {
        let h = SendLatency::new();
        h.record(Duration::from_micros(17));
        let s = h.rotate();
        assert!(s.p50_ms <= s.max_ms && s.p99_ms <= s.max_ms, "{s:?}");
    }

    #[test]
    fn serializes_camel_case() {
        let j = serde_json::to_string(&SendQueueStats::default()).unwrap();
        assert!(j.contains("\"p50Ms\"") && j.contains("\"p99Ms\"") && j.contains("\"maxMs\"") && j.contains("\"packets\""), "{j}");
    }
}
