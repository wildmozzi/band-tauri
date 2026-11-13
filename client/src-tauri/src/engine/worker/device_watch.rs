//! 장치 감시 단계(엔진 ②b): 입·출력 콜백이 멈췄는지 보고, 스트림 에러 누적을 로그로 남긴다.
//!
//! 바뀌는 이유: 장치 분실 판정 규칙. 모르는 것: net, 코덱, 소스 링.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::WorkerStats;
use crate::engine::stats::AudioCounters;

/// 콜백이 이 시간 동안 멈추면 장치를 잃은 것으로 본다.
const DEVICE_STALL: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub(super) struct DeviceWatch {
    last_played: u64,
    last_played_at: Duration,
    last_captured: u64,
    last_captured_at: Duration,
    reported_errors: u32,
}

impl DeviceWatch {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// 반복마다. `epoch` = 워커 시계 기준.
    pub(super) fn check(&mut self, counters: &AudioCounters, stats: &WorkerStats, epoch: Instant) {
        self.check_at(counters, stats, epoch.elapsed());
    }

    /// 입력·출력 콜백이 1초 넘게 멈추면 장치를 잃은 것으로 본다(리뷰 M9 / PR #5 M-2).
    /// cpal 에러 콜백은 비치명 에러도 보내므로 그것만으로는 판단하지 않는다.
    fn check_at(&mut self, counters: &AudioCounters, stats: &WorkerStats, now: Duration) {
        let played = counters.out_played_frames.load(Ordering::Relaxed);
        let captured = counters.in_captured_frames.load(Ordering::Relaxed);
        if played != self.last_played {
            self.last_played = played;
            self.last_played_at = now;
        }
        if captured != self.last_captured {
            self.last_captured = captured;
            self.last_captured_at = now;
        }
        let stalled = (played > 0 && now.saturating_sub(self.last_played_at) > DEVICE_STALL)
            || (captured > 0 && now.saturating_sub(self.last_captured_at) > DEVICE_STALL);
        if stalled != stats.device_stalled.load(Ordering::Relaxed) {
            if stalled {
                log::error!("오디오 장치 콜백이 멈췄습니다(장치 분리 가능성)");
            } else {
                log::info!("오디오 장치 콜백 재개");
            }
            stats.device_stalled.store(stalled, Ordering::Relaxed);
        }
        let errs = counters.stream_errors.load(Ordering::Relaxed);
        if errs != self.reported_errors {
            log::warn!("오디오 스트림 에러 누적 {}회(장치 분리 등)", errs);
            self.reported_errors = errs;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 카운터를 멈췄다 움직이며 `device_stalled` 전이를 본다(가상 시각, 설계 5.4절).
    #[test]
    fn stall_and_resume_transitions() {
        let (c, s) = (AudioCounters::default(), WorkerStats::default());
        let mut w = DeviceWatch::new();
        let ms = Duration::from_millis;
        let stalled = |s: &WorkerStats| s.device_stalled.load(Ordering::Relaxed);
        // 콜백이 아직 한 번도 안 돌았으면(0) 멈춤이 아니다
        w.check_at(&c, &s, ms(5_000));
        assert!(!stalled(&s));
        // 둘 다 돌기 시작
        c.out_played_frames.store(128, Ordering::Relaxed);
        c.in_captured_frames.store(128, Ordering::Relaxed);
        w.check_at(&c, &s, ms(5_001));
        assert!(!stalled(&s));
        // 딱 1초는 아직 아니다(초과여야 한다)
        w.check_at(&c, &s, ms(6_001));
        assert!(!stalled(&s));
        w.check_at(&c, &s, ms(6_002));
        assert!(stalled(&s), "출력·입력이 1초 넘게 멈춤");
        // 출력만 다시 돌면 입력이 아직 멈춰 있어 그대로
        c.out_played_frames.store(256, Ordering::Relaxed);
        w.check_at(&c, &s, ms(6_003));
        assert!(stalled(&s));
        // 입력도 돌면 재개
        c.in_captured_frames.store(256, Ordering::Relaxed);
        w.check_at(&c, &s, ms(6_004));
        assert!(!stalled(&s));
        // 입력만 멈춰도 멈춤
        c.out_played_frames.store(512, Ordering::Relaxed);
        w.check_at(&c, &s, ms(7_100));
        assert!(stalled(&s), "입력만 1초 넘게 멈춤");
    }

    #[test]
    fn stream_errors_are_reported_once_per_change() {
        let (c, s) = (AudioCounters::default(), WorkerStats::default());
        let mut w = DeviceWatch::new();
        c.stream_errors.store(3, Ordering::Relaxed);
        w.check_at(&c, &s, Duration::ZERO);
        assert_eq!(w.reported_errors, 3);
        w.check_at(&c, &s, Duration::from_millis(1));
        assert_eq!(w.reported_errors, 3);
    }
}
