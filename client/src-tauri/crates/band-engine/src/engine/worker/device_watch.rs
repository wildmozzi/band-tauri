//! 장치 감시 단계(엔진 ②b): 입·출력 콜백이 멈췄는지 보고, 스트림 에러 누적을 로그로 남긴다.
//!
//! 바뀌는 이유: 장치 분실 판정 규칙. 모르는 것: net, 코덱, 소스 링.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::WorkerStats;
use band_dsp::stats::AudioCounters;

/// 콜백이 이 시간 동안 멈추면 장치를 잃은 것으로 본다.
const DEVICE_STALL: Duration = Duration::from_secs(1);
/// 첫 콜백을 이 시간 안에 한 번도 받지 못한 방향도 장치를 잃은 것으로 본다. 기준 시각은 **워커 감시 시작**(첫 `check`)이다 —
/// 장치 Start(cpal `play`, 자체 WASAPI `Start`)와 STUN이 모두 끝난 뒤라 실제 여유는 "스트림 Start 뒤 이 시간 + STUN"이다.
/// 장치 분리가 열기 직후·첫 콜백 전에 일어나면 누적 프레임이 0에 머물러 `DEVICE_STALL` 규칙(0이면 멈춤 아님)에 걸리지 않았다.
/// 거짓 양성은 되돌릴 수 없다(프론트가 `running=false` 2회 연속이면 연주를 끝낸다) — 그래서 보통의 첫 콜백(수 ms)보다
/// 훨씬 넉넉하게 5초로 잡고, 실제 첫 콜백 지연은 방향마다 한 번 로그로 남긴다(진단 내보내기에 들어간다).
const FIRST_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5);

/// 방향(진단·로그용).
const DIRS: [&str; 2] = ["출력", "입력"];

#[derive(Debug, Default)]
pub(super) struct DeviceWatch {
    /// 첫 `check_at` 시각(아직 안 불렸으면 None). 콜백이 한 번도 안 돈 방향의 기준 시각이다.
    started_at: Option<Duration>,
    /// 방향별(출력, 입력) 첫 콜백이 보일 때까지 걸린 시간(감시 시작 기준, 감시 전에 이미 돌았으면 0). 아직이면 None.
    first_cb: [Option<Duration>; 2],
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

    /// 입력·출력 콜백이 1초 넘게 멈추거나, 한 방향이 감시 시작 뒤 `FIRST_CALLBACK_TIMEOUT` 동안 첫 콜백을 받지 못하면
    /// 장치를 잃은 것으로 본다(리뷰 M9 / PR #5 M-2). cpal 에러 콜백은 비치명 에러도 보내므로 그것만으로는 판단하지 않는다.
    fn check_at(&mut self, counters: &AudioCounters, stats: &WorkerStats, now: Duration) {
        let played = counters.out_played_frames.load(Ordering::Relaxed);
        let captured = counters.in_captured_frames.load(Ordering::Relaxed);
        let started = *self.started_at.get_or_insert(now);
        // 첫 콜백 지연(진단): 방향마다 처음 0이 아니게 보인 반복에서 한 번 남긴다.
        for (i, count) in [played, captured].into_iter().enumerate() {
            if count > 0 && self.first_cb[i].is_none() {
                let d = now.saturating_sub(started);
                self.first_cb[i] = Some(d);
                log::info!("첫 {} 콜백: 감시 시작 뒤 {:.1}ms", DIRS[i], d.as_secs_f64() * 1e3);
            }
        }
        if played != self.last_played {
            self.last_played = played;
            self.last_played_at = now;
        }
        if captured != self.last_captured {
            self.last_captured = captured;
            self.last_captured_at = now;
        }
        // 한 번이라도 돈 방향: 마지막 진행 뒤 `DEVICE_STALL`. 한 번도 안 돈 방향: 감시 시작 뒤 `FIRST_CALLBACK_TIMEOUT`.
        let dir_stalled = |count: u64, last_at: Duration| {
            if count > 0 {
                now.saturating_sub(last_at) > DEVICE_STALL
            } else {
                now.saturating_sub(started) > FIRST_CALLBACK_TIMEOUT
            }
        };
        let stalled = dir_stalled(played, self.last_played_at) || dir_stalled(captured, self.last_captured_at);
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

    /// 첫 콜백 전 분실: 한 방향이 한 번도 돌지 않으면 감시 시작 뒤 5초가 넘을 때 멈춤으로 본다(전에는 영원히 아님).
    #[test]
    fn direction_that_never_starts_is_stalled_after_timeout() {
        let (c, s) = (AudioCounters::default(), WorkerStats::default());
        let mut w = DeviceWatch::new();
        let ms = Duration::from_millis;
        let stalled = |s: &WorkerStats| s.device_stalled.load(Ordering::Relaxed);
        w.check_at(&c, &s, ms(1_000));
        // 출력만 돌고 입력은 첫 콜백이 오지 않는다
        for t in (1_010..=6_000).step_by(10) {
            c.out_played_frames.fetch_add(480, Ordering::Relaxed);
            w.check_at(&c, &s, ms(t));
            assert!(!stalled(&s), "{t}ms: 첫 콜백 대기 5초 안은 멈춤이 아님");
        }
        c.out_played_frames.fetch_add(480, Ordering::Relaxed);
        w.check_at(&c, &s, ms(6_001));
        assert!(stalled(&s), "입력 첫 콜백이 5초 넘게 없으면 장치 분실");
        // 늦게라도 돌기 시작하면 재개
        c.in_captured_frames.store(480, Ordering::Relaxed);
        c.out_played_frames.fetch_add(480, Ordering::Relaxed);
        w.check_at(&c, &s, ms(6_010));
        assert!(!stalled(&s));
        // 첫 콜백 지연(진단): 출력은 첫 반복 직후(10ms), 입력은 늦게 돈 시각(5,010ms)
        assert_eq!(w.first_cb, [Some(ms(10)), Some(ms(5_010))]);
        // 둘 다 한 번도 안 돌아도 같다
        let (c2, s2) = (AudioCounters::default(), WorkerStats::default());
        let mut w2 = DeviceWatch::new();
        w2.check_at(&c2, &s2, ms(0));
        w2.check_at(&c2, &s2, ms(5_000));
        assert!(!stalled(&s2));
        w2.check_at(&c2, &s2, ms(5_001));
        assert!(stalled(&s2));
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
