//! 장치 감시 단계(엔진 ②b): 입·출력 콜백이 멈췄는지 보고, 스트림 에러 누적을 로그로 남긴다.
//! 드라이버 리셋 요청(ASIO `kAsioResetRequest`, P1-11)을 디바운스해 "장치를 다시 열어야 함"으로 바꾼다(`ResetWatch`).
//!
//! 바뀌는 이유: 장치 분실 판정 규칙. 모르는 것: net, 코덱, 소스 링, 어느 백엔드가 리셋 요청을 세는지.

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

/// 리셋 요청이 이 시간 동안 더 오지 않으면 다시 열기를 요청한다. 드라이버는 제어판 변경 한 번에 리셋 요청을 몇 번
/// 연달아 보내기도 한다(버퍼 크기 → 레이턴시 → 리셋) — 하나로 합쳐 한 번만 다시 연다.
pub(super) const RESET_QUIET: Duration = Duration::from_millis(300);
/// 요청이 끊이지 않고 와도 첫 요청 뒤 이 시간이 지나면 다시 연다(조용해지기를 무한정 기다리지 않음).
pub(super) const RESET_MAX_WAIT: Duration = Duration::from_millis(1_500);

/// 리셋 요청 디바운스 상태.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResetState {
    /// 요청 없음.
    #[default]
    Idle,
    /// 요청을 받고 조용해지기를 기다린다: (첫 요청을 본 시각, 마지막 요청을 본 시각, 합친 요청 수).
    Pending { first_at: Duration, last_at: Duration, merged: u32 },
    /// 다시 열기를 요청했다(끝 상태). 이 엔진에서는 더 요청하지 않는다 — 다시 열면 새 엔진이 새 감시로 시작한다.
    /// 그래서 엔진 하나는 리셋 요청이 아무리 와도 다시 열기를 한 번만 부르고, 엔진을 거듭 다시 여는 반복의 상한은
    /// 프론트의 장치 분실 한도(60초에 3번, `performSession.recoverFromDeviceLoss`)가 정한다.
    Requested,
}

/// 드라이버 리셋 요청 디바운스(순수 로직, 장치·피처 무관 — 설계 문서 1.1절 "드라이버 리셋 요청").
/// 입력은 백엔드가 세는 단조 증가 카운터(`AudioCounters::device_reset_requests`)와 지금 시각뿐이다.
#[derive(Debug, Default)]
pub(super) struct ResetWatch {
    /// 마지막으로 본 카운터 값.
    seen: u32,
    state: ResetState,
}

impl ResetWatch {
    /// 반복마다. 이번 호출에서 `Requested`로 넘어가면 true(한 번만).
    pub(super) fn observe(&mut self, requests: u32, now: Duration) -> bool {
        let new = requests.wrapping_sub(self.seen);
        self.seen = requests;
        self.state = match self.state {
            ResetState::Requested => return false,
            ResetState::Idle if new == 0 => ResetState::Idle,
            ResetState::Idle => ResetState::Pending { first_at: now, last_at: now, merged: new },
            ResetState::Pending { first_at, last_at, merged } => {
                let (last_at, merged) = if new > 0 { (now, merged.saturating_add(new)) } else { (last_at, merged) };
                let quiet = now.saturating_sub(last_at) >= RESET_QUIET;
                let waited_out = now.saturating_sub(first_at) >= RESET_MAX_WAIT;
                if quiet || waited_out {
                    log::warn!(
                        "드라이버 리셋 요청 {merged}회(첫 요청 뒤 {:.0}ms) → 장치를 다시 엽니다(deviceReset)",
                        now.saturating_sub(first_at).as_secs_f64() * 1e3
                    );
                    self.state = ResetState::Requested;
                    return true;
                }
                ResetState::Pending { first_at, last_at, merged }
            }
        };
        false
    }

    #[cfg(test)]
    pub(super) fn state(&self) -> ResetState {
        self.state
    }
}

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
    reset: ResetWatch,
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
        if self.reset.observe(counters.device_reset_requests.load(Ordering::Relaxed), now) {
            stats.device_reset.store(true, Ordering::Relaxed);
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

    /// 리셋 요청 하나: 조용한 시간(`RESET_QUIET`)이 지나야 한 번만 다시 열기를 요청하고, 그 뒤로는 끝 상태(P1-11).
    #[test]
    fn single_reset_request_is_debounced_then_latched() {
        let ms = Duration::from_millis;
        let mut r = ResetWatch::default();
        assert!(!r.observe(0, ms(0)));
        assert_eq!(r.state(), ResetState::Idle);
        assert!(!r.observe(1, ms(100)), "요청을 본 반복에서는 바로 다시 열지 않는다");
        assert_eq!(r.state(), ResetState::Pending { first_at: ms(100), last_at: ms(100), merged: 1 });
        assert!(!r.observe(1, ms(399)), "조용한 시간 300ms 전");
        assert!(r.observe(1, ms(400)), "조용한 시간이 지나면 한 번");
        assert_eq!(r.state(), ResetState::Requested);
        // 끝 상태: 이 엔진에서는 더 요청하지 않는다(새 요청이 와도)
        assert!(!r.observe(1, ms(500)));
        assert!(!r.observe(5, ms(600)));
        assert!(!r.observe(5, ms(10_000)));
        assert_eq!(r.state(), ResetState::Requested);
    }

    /// 드라이버가 연달아 보내는 요청(버퍼 크기 → 레이턴시 → 리셋)은 합쳐 한 번. 요청이 끊이지 않으면 `RESET_MAX_WAIT`에서 끊는다.
    #[test]
    fn burst_of_reset_requests_merges_and_max_wait_bounds_it() {
        let ms = Duration::from_millis;
        // 50ms 간격 4번 → 마지막 요청 뒤 300ms에 한 번
        let mut r = ResetWatch::default();
        let mut fired = Vec::new();
        for (t, n) in [(0, 1), (50, 2), (100, 3), (150, 4), (200, 4), (449, 4), (450, 4), (460, 4)] {
            if r.observe(n, ms(t)) {
                fired.push(t);
            }
        }
        assert_eq!(fired, vec![450], "연속 요청 4번 → 마지막 요청(150ms) 뒤 300ms에 한 번");
        // 200ms마다 끝없이 요청 → 첫 요청 뒤 1.5초에 끊는다(조용해지기를 무한정 기다리지 않음)
        let mut r = ResetWatch::default();
        let mut fired = Vec::new();
        for i in 0..20u32 {
            if r.observe(i + 1, ms(1_000 + 200 * i as u64)) {
                fired.push(200 * i as u64);
            }
        }
        assert_eq!(fired, vec![1_600], "첫 요청 뒤 1.5초가 처음 넘는 반복(1.6초)에서 한 번");
        // 한 반복 사이에 여러 번 온 요청(카운터가 한 번에 3 증가)도 하나의 대기로 합친다
        let mut r = ResetWatch::default();
        assert!(!r.observe(3, ms(0)));
        assert_eq!(r.state(), ResetState::Pending { first_at: ms(0), last_at: ms(0), merged: 3 });
        // 카운터 넘침(u32 감김)도 새 요청으로 센다
        let mut r = ResetWatch { seen: u32::MAX, state: ResetState::Idle };
        assert!(!r.observe(0, ms(0)));
        assert_eq!(r.state(), ResetState::Pending { first_at: ms(0), last_at: ms(0), merged: 1 });
    }

    /// `DeviceWatch`가 카운터를 읽어 `device_reset`을 켠다. 콜백이 계속 돌아도(드라이버가 옛 설정으로 계속 부름) 켠다.
    #[test]
    fn device_watch_sets_device_reset_flag() {
        let (c, s) = (AudioCounters::default(), WorkerStats::default());
        let mut w = DeviceWatch::new();
        let ms = Duration::from_millis;
        let reset = |s: &WorkerStats| s.device_reset.load(Ordering::Relaxed);
        let mut t = 0;
        let mut step = |w: &mut DeviceWatch, c: &AudioCounters, until: u64| {
            while t < until {
                t += 10;
                c.out_played_frames.fetch_add(480, Ordering::Relaxed);
                c.in_captured_frames.fetch_add(480, Ordering::Relaxed);
                w.check_at(c, &s, ms(t));
            }
        };
        step(&mut w, &c, 1_000);
        assert!(!reset(&s));
        c.device_reset_requests.fetch_add(1, Ordering::Relaxed);
        step(&mut w, &c, 1_200);
        assert!(!reset(&s), "디바운스 중");
        step(&mut w, &c, 1_400);
        assert!(reset(&s), "조용한 시간 뒤 deviceReset");
        assert!(!s.device_stalled.load(Ordering::Relaxed), "콜백은 계속 돈다 — 멈춤과는 따로다");
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
