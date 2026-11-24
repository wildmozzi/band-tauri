//! 조건부 고정(P 코어 정책 3안, P1-21 3단계 ②)의 판단 — "묶어 두되, 끊김이 늘면 풀고 안정되면 다시 묶는다".
//!
//! 고정(`SetThreadSelectedCpuSets`)은 P 코어가 같은 우선순위 부하로 차도 스레드를 빈 E 코어로 보내지 못한다(2단계 리뷰 H1).
//! 3안은 그때 고정을 풀어 스케줄러에 맡기고(= P 코어 정책 끔과 같은 상태), 증상이 가라앉으면 다시 묶는다.
//!
//! 이 파일에는 OS 호출이 없다:
//! - [`Decider`]: 1초 창 하나의 관측([`Window`])을 받아 유지·풀기·다시 묶기를 정하는 **순수 상태 기계**(결정적 단위 시험).
//! - [`AdaptivePin`]: 판단 결과를 엔진 스레드들이 읽는 원자값(바라는 상태·횟수).
//! - [`Collector`]: 워커가 반복마다 올리는 창 누적(지역 값 — 원자값 아님)과 창 경계.
//!
//! 누가 무엇을 하는가(설계 문서 1.1절 "조건부 고정"):
//! - **판단은 워커 스레드가 반복 끝에서** 한다(RT 콜백 밖). 읽는 신호는 ① RT 콜백이 올리는 xrun 카운터(원자값 읽기) ② 워커
//!   자신의 기상 지연·반복 본문 시간(자기 스레드에서 잰 값) ③ P 코어 포화 추정(`placement::PBusy`, 창마다 한 번).
//! - **적용은 각 스레드가 자기 자신에게** 한다(`PolicySlot::sync_adaptive` — 워커는 반복마다, 수신 스레드는 패킷마다 원자값
//!   하나를 읽고, 바뀌었을 때만 OS를 부른다). RT 콜백 스레드는 이 정책의 대상이 아니고 여기 값을 읽지도 않는다.
//! - 할당·락·로그 없음(워커 반복 안에서 돈다 — `engine::alloc_tests`).
//!
//! 판단 기준과 이력(히스테리시스)은 아래 상수와 `Decider::step`이 전부다.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

/// 판단 창 길이. 창마다 한 번 판단한다.
pub(crate) const WINDOW: Duration = Duration::from_secs(1);
/// 기상 지연(요청한 수면보다 늦게 깬 양)이 이 값 이상이면 "멈칫" 하나로 센다. 평소 꼬리(225H 유휴 p99.9 ≈ 1.1ms, 최대 ≈ 1.3ms —
/// 설계 문서 1.1절 `DEADLINE_GUARD_US` 실측)보다 위, 은닉 마감 여유(1.5ms)를 넘겨 실제로 마감을 놓치는 크기다.
pub(crate) const LATE_WAKE: Duration = Duration::from_micros(2_000);
/// 반복 본문(수면 제외)이 이 값 이상이면 "멈칫" 하나로 센다. 가장 무거운 측정 구성의 본문 p99가 ≈ 1.4ms라, 이 크기는 계산이
/// 아니라 본문 도중에 CPU를 빼앗긴 것이다.
pub(crate) const SLOW_BODY: Duration = Duration::from_micros(3_000);

/// 나쁜 창: 창 하나에 xrun이 이만큼 이상.
pub(crate) const BAD_XRUNS: u32 = 2;
/// 나쁜 창: 창 하나에 멈칫이 이만큼 이상.
pub(crate) const BAD_STALLS: u32 = 5;
/// P 코어가 포화(이 % 이상 바쁨)면 문턱을 낮춘다: xrun 1 또는 멈칫 2부터 나쁜 창.
pub(crate) const SATURATED_PCT: u8 = 90;
pub(crate) const SATURATED_BAD_XRUNS: u32 = 1;
pub(crate) const SATURATED_BAD_STALLS: u32 = 2;
/// 심한 창: 이 창 하나로 바로 푼다(연속 조건 없이).
pub(crate) const SEVERE_XRUNS: u32 = 8;
pub(crate) const SEVERE_STALLS: u32 = 40;
/// 나쁜 창이 연속 이만큼이면 푼다(한 번 튄 것으로는 풀지 않는다).
pub(crate) const RELEASE_AFTER_BAD: u32 = 2;
/// 푼 뒤 다시 묶기까지 최소로 기다리는 창 수(처음). 다시 묶자마자 또 풀리면 두 배씩 늘어 `HOLD_MAX`까지 간다.
pub(crate) const HOLD_MIN: u32 = 10;
pub(crate) const HOLD_MAX: u32 = 320;
/// 다시 묶으려면 푼 상태에서 좋은 창이 연속 이만큼.
pub(crate) const REBIND_GOOD: u32 = 5;
/// 다시 묶으려면 P 코어 바쁨이 이 % 미만(추정값이 있을 때만 본다 — 없으면 기다린 시간과 좋은 창만으로 정한다).
pub(crate) const REBIND_BUSY_BELOW_PCT: u8 = 60;
/// (다시) 묶고 이 창 수 안에 또 풀리면 "묶은 것이 원인"으로 보고 기다림을 두 배로 한다.
pub(crate) const PROBE_WINDOWS: u32 = 15;
/// 묶은 채 이 창 수를 버티면 기다림을 `HOLD_MIN`으로 되돌린다.
pub(crate) const STABLE_WINDOWS: u32 = 60;

/// 1초 창 하나의 관측.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Window {
    /// 이 창에서 늘어난 xrun(RT 콜백이 센 값의 증분).
    pub(crate) xruns: u32,
    /// 이 창의 멈칫 수(`LATE_WAKE` 이상 늦게 깸 + `SLOW_BODY` 이상 걸린 본문).
    pub(crate) stalls: u32,
    /// P 코어 바쁨 추정(%, 0..=100). 못 재면 None.
    pub(crate) p_busy_pct: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Health {
    Good,
    Bad,
    Severe,
}

/// 창 하나의 상태.
pub(crate) fn health(w: &Window) -> Health {
    if w.xruns >= SEVERE_XRUNS || w.stalls >= SEVERE_STALLS {
        return Health::Severe;
    }
    let saturated = w.p_busy_pct.is_some_and(|b| b >= SATURATED_PCT);
    let (x, s) = if saturated { (SATURATED_BAD_XRUNS, SATURATED_BAD_STALLS) } else { (BAD_XRUNS, BAD_STALLS) };
    if w.xruns >= x || w.stalls >= s {
        Health::Bad
    } else {
        Health::Good
    }
}

/// 창 하나를 본 뒤의 결정.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Keep,
    /// 고정을 푼다.
    Release,
    /// 다시 묶는다.
    Rebind,
}

/// 조건부 고정 상태 기계. 묶인 상태로 시작한다.
///
/// - **묶인 상태**: 심한 창 하나 또는 나쁜 창 연속 `RELEASE_AFTER_BAD`개 → 푼다. 푼 뒤 최소 `backoff`창은 다시 묶지 않는다.
///   (다시) 묶고 `PROBE_WINDOWS`창 안에 풀렸으면 다음 기다림을 두 배로(`HOLD_MAX`까지), `STABLE_WINDOWS`창을 버텼으면
///   기다림을 `HOLD_MIN`으로 되돌린다.
/// - **풀린 상태**: 기다림이 끝났고, 좋은 창이 연속 `REBIND_GOOD`개이고, P 코어 바쁨 추정이 있으면 `REBIND_BUSY_BELOW_PCT` 미만일
///   때 → 다시 묶는다. 풀린 상태의 나쁜 창은 고정 탓이 아니므로 좋은 창 연속만 끊는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decider {
    pinned: bool,
    bad_run: u32,
    good_run: u32,
    /// 풀린 상태에서 다시 묶기까지 남은 창 수.
    hold: u32,
    /// 다음에 풀릴 때 기다릴 창 수.
    backoff: u32,
    /// (다시) 묶은 뒤 지난 창 수.
    since_pin: u32,
}

impl Default for Decider {
    fn default() -> Self {
        Self { pinned: true, bad_run: 0, good_run: 0, hold: 0, backoff: HOLD_MIN, since_pin: 0 }
    }
}

impl Decider {
    #[cfg(test)]
    pub(crate) fn pinned(&self) -> bool {
        self.pinned
    }

    pub(crate) fn step(&mut self, w: Window) -> Verdict {
        let h = health(&w);
        if self.pinned {
            self.since_pin = self.since_pin.saturating_add(1);
            match h {
                Health::Good => self.bad_run = 0,
                Health::Bad | Health::Severe => self.bad_run += 1,
            }
            if h == Health::Severe || self.bad_run >= RELEASE_AFTER_BAD {
                self.pinned = false;
                self.hold = self.backoff;
                if self.since_pin <= PROBE_WINDOWS {
                    self.backoff = (self.backoff * 2).min(HOLD_MAX);
                }
                self.bad_run = 0;
                self.good_run = 0;
                return Verdict::Release;
            }
            if self.since_pin >= STABLE_WINDOWS {
                self.backoff = HOLD_MIN;
            }
            Verdict::Keep
        } else {
            self.hold = self.hold.saturating_sub(1);
            self.good_run = if h == Health::Good { self.good_run.saturating_add(1) } else { 0 };
            let calm = w.p_busy_pct.is_none_or(|b| b < REBIND_BUSY_BELOW_PCT);
            if self.hold == 0 && self.good_run >= REBIND_GOOD && calm {
                self.pinned = true;
                self.since_pin = 0;
                self.bad_run = 0;
                return Verdict::Rebind;
            }
            Verdict::Keep
        }
    }
}

/// `EngineStats.threadPolicy.adaptive`: 조건부 고정의 지금 상태와 누적 횟수.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdaptiveStats {
    /// 지금 풀려 있다(판단 기준 — 각 스레드의 적용 결과는 `worker.pCores`·`netRecv.pCores`).
    pub(crate) released: bool,
    /// 엔진 시작 뒤 푼 횟수.
    pub(crate) releases: u32,
    /// 엔진 시작 뒤 다시 묶은 횟수.
    pub(crate) rebinds: u32,
    /// 마지막 창의 P 코어 바쁨 추정(%). 못 재면 null.
    pub(crate) p_busy_pct: Option<u8>,
}

const BUSY_UNKNOWN: u8 = u8::MAX;

/// 조건부 고정의 공유 상태(엔진 하나에 하나): 워커가 쓰고 워커·수신 스레드·통계가 읽는다. 원자값만.
#[derive(Debug)]
pub(crate) struct AdaptivePin {
    want_pinned: AtomicBool,
    releases: AtomicU32,
    rebinds: AtomicU32,
    p_busy_pct: AtomicU8,
}

impl Default for AdaptivePin {
    fn default() -> Self {
        Self { want_pinned: AtomicBool::new(true), releases: AtomicU32::new(0), rebinds: AtomicU32::new(0), p_busy_pct: AtomicU8::new(BUSY_UNKNOWN) }
    }
}

impl AdaptivePin {
    /// 지금 묶여 있어야 하는가(스레드가 반복마다 읽는 값).
    #[inline]
    pub(crate) fn want_pinned(&self) -> bool {
        self.want_pinned.load(Ordering::Relaxed)
    }

    /// 판단 결과를 적는다(워커).
    pub(crate) fn publish(&self, v: Verdict, p_busy_pct: Option<u8>) {
        self.p_busy_pct.store(p_busy_pct.map_or(BUSY_UNKNOWN, |b| b.min(100)), Ordering::Relaxed);
        match v {
            Verdict::Keep => {}
            Verdict::Release => {
                self.releases.fetch_add(1, Ordering::Relaxed);
                self.want_pinned.store(false, Ordering::Relaxed);
            }
            Verdict::Rebind => {
                self.rebinds.fetch_add(1, Ordering::Relaxed);
                self.want_pinned.store(true, Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn stats(&self) -> AdaptiveStats {
        let b = self.p_busy_pct.load(Ordering::Relaxed);
        AdaptiveStats {
            released: !self.want_pinned(),
            releases: self.releases.load(Ordering::Relaxed),
            rebinds: self.rebinds.load(Ordering::Relaxed),
            p_busy_pct: (b != BUSY_UNKNOWN).then_some(b),
        }
    }
}

/// 워커가 쥐는 창 누적: 반복마다 멈칫을 세고, 창이 차면 관측 하나를 낸다. 지역 값(워커 스레드만 만진다).
#[derive(Debug)]
pub(crate) struct Collector {
    window_start: Instant,
    xruns_at_start: u64,
    stalls: u32,
}

impl Collector {
    pub(crate) fn new(now: Instant, xruns_total: u64) -> Self {
        Self { window_start: now, xruns_at_start: xruns_total, stalls: 0 }
    }

    /// 수면에서 깬 뒤: 요청보다 늦게 깬 양.
    #[inline]
    pub(crate) fn note_wake(&mut self, overshoot: Duration) {
        if overshoot >= LATE_WAKE {
            self.stalls = self.stalls.saturating_add(1);
        }
    }

    /// 반복 본문(수면 제외)에 걸린 시간.
    #[inline]
    pub(crate) fn note_body(&mut self, body: Duration) {
        if body >= SLOW_BODY {
            self.stalls = self.stalls.saturating_add(1);
        }
    }

    /// 창이 찼으면 관측을 내고 다음 창을 연다. `xruns_total`은 RT 콜백이 올리는 누적 xrun, `p_busy`는 창이 찼을 때만 부른다.
    #[inline]
    pub(crate) fn roll(&mut self, now: Instant, xruns_total: u64, p_busy: impl FnOnce() -> Option<u8>) -> Option<Window> {
        if now.saturating_duration_since(self.window_start) < WINDOW {
            return None;
        }
        let w = Window {
            xruns: u32::try_from(xruns_total.saturating_sub(self.xruns_at_start)).unwrap_or(u32::MAX),
            stalls: self.stalls,
            p_busy_pct: p_busy(),
        };
        self.window_start = now;
        self.xruns_at_start = xruns_total;
        self.stalls = 0;
        Some(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: Window = Window { xruns: 0, stalls: 0, p_busy_pct: None };
    const BAD: Window = Window { xruns: BAD_XRUNS, stalls: 0, p_busy_pct: None };
    const SEVERE: Window = Window { xruns: SEVERE_XRUNS, stalls: 0, p_busy_pct: None };

    fn run(d: &mut Decider, w: Window, n: u32) -> Vec<Verdict> {
        (0..n).map(|_| d.step(w)).collect()
    }

    /// 다시 묶일 때까지 좋은 창을 넣고 걸린 창 수를 돌려준다.
    fn windows_until_rebind(d: &mut Decider, w: Window) -> u32 {
        for n in 1..=10_000 {
            if d.step(w) == Verdict::Rebind {
                return n;
            }
        }
        panic!("다시 묶이지 않음: {d:?}");
    }

    #[test]
    fn window_health_thresholds() {
        let w = |xruns, stalls, p_busy_pct| Window { xruns, stalls, p_busy_pct };
        assert_eq!(health(&w(0, 0, None)), Health::Good);
        assert_eq!(health(&w(BAD_XRUNS - 1, BAD_STALLS - 1, None)), Health::Good);
        assert_eq!(health(&w(BAD_XRUNS, 0, None)), Health::Bad);
        assert_eq!(health(&w(0, BAD_STALLS, Some(50))), Health::Bad);
        assert_eq!(health(&w(SEVERE_XRUNS, 0, None)), Health::Severe);
        assert_eq!(health(&w(0, SEVERE_STALLS, Some(0))), Health::Severe);
        // P 코어가 포화면 문턱이 낮아진다. 포화만으로는(증상 없이) 나쁘지 않다 — 우선순위가 낮은 부하는 엔진 스레드가 밀어낸다
        assert_eq!(health(&w(1, 0, Some(SATURATED_PCT - 1))), Health::Good);
        assert_eq!(health(&w(1, 0, Some(SATURATED_PCT))), Health::Bad);
        assert_eq!(health(&w(0, SATURATED_BAD_STALLS, Some(100))), Health::Bad);
        assert_eq!(health(&w(0, 0, Some(100))), Health::Good);
    }

    /// 좋은 창만 오면 끝까지 묶여 있다.
    #[test]
    fn stays_pinned_while_good() {
        let mut d = Decider::default();
        assert!(run(&mut d, GOOD, 1_000).iter().all(|v| *v == Verdict::Keep));
        assert!(d.pinned());
    }

    /// 한 번 튄 나쁜 창으로는 풀지 않는다. 연속 둘이면 푼다. 심한 창은 하나로 푼다.
    #[test]
    fn releases_after_consecutive_bad_or_one_severe() {
        let mut d = Decider::default();
        for _ in 0..20 {
            assert_eq!(d.step(BAD), Verdict::Keep);
            assert_eq!(d.step(GOOD), Verdict::Keep, "좋은 창이 연속을 끊는다");
        }
        assert_eq!(d.step(BAD), Verdict::Keep);
        assert_eq!(d.step(BAD), Verdict::Release);
        assert!(!d.pinned());

        let mut d = Decider::default();
        assert_eq!(d.step(SEVERE), Verdict::Release);
    }

    /// 푼 뒤: 기다림(`HOLD_MIN`)이 끝나고 좋은 창이 이어져야 다시 묶는다. 나쁜 창은 좋은 창 연속을 끊는다.
    #[test]
    fn rebinds_after_hold_and_good_run() {
        let mut d = Decider::default();
        run(&mut d, GOOD, STABLE_WINDOWS); // 처음 묶은 뒤 충분히 지남 → 기다림은 최소값
        assert_eq!(d.step(SEVERE), Verdict::Release);
        assert_eq!(windows_until_rebind(&mut d, GOOD), HOLD_MIN);
        assert!(d.pinned());

        // 기다림이 끝날 무렵 나쁜 창이 오면 거기서부터 좋은 창을 다시 센다
        run(&mut d, GOOD, STABLE_WINDOWS);
        assert_eq!(d.step(SEVERE), Verdict::Release);
        assert!(run(&mut d, GOOD, HOLD_MIN - 1).iter().all(|v| *v == Verdict::Keep));
        assert_eq!(d.step(BAD), Verdict::Keep);
        assert_eq!(windows_until_rebind(&mut d, GOOD), REBIND_GOOD);
    }

    /// P 코어가 아직 바쁘면(추정값이 있을 때) 다시 묶지 않는다. 가라앉으면 묶는다.
    #[test]
    fn rebind_waits_for_p_cores_to_calm_down() {
        let busy = Window { p_busy_pct: Some(REBIND_BUSY_BELOW_PCT), ..GOOD };
        let calm = Window { p_busy_pct: Some(REBIND_BUSY_BELOW_PCT - 1), ..GOOD };
        let mut d = Decider::default();
        assert_eq!(d.step(SEVERE), Verdict::Release);
        assert!(run(&mut d, busy, 500).iter().all(|v| *v == Verdict::Keep));
        assert!(!d.pinned());
        assert_eq!(d.step(calm), Verdict::Rebind);
    }

    /// 다시 묶자마자 또 풀리면 기다림이 두 배씩 늘고(`HOLD_MAX`까지), 묶은 채 오래 버티면 최소값으로 돌아간다 — 계속 나쁜 환경에서
    /// 풀고 묶기를 되풀이하는 횟수가 줄어든다.
    #[test]
    fn hold_backs_off_when_rebinding_keeps_failing() {
        let mut d = Decider::default();
        run(&mut d, GOOD, STABLE_WINDOWS);
        let mut holds = Vec::new();
        for _ in 0..8 {
            assert_eq!(d.step(SEVERE), Verdict::Release);
            holds.push(windows_until_rebind(&mut d, GOOD));
        }
        // 첫 풀림은 오래 버틴 뒤라 최소, 그 뒤는 묶자마자 풀려 두 배씩
        assert_eq!(holds, [10, 10, 20, 40, 80, 160, 320, 320]);
        // 오래 버티면 되돌아간다
        run(&mut d, GOOD, STABLE_WINDOWS);
        assert_eq!(d.step(SEVERE), Verdict::Release);
        assert_eq!(windows_until_rebind(&mut d, GOOD), HOLD_MIN);
    }

    /// 시작 직후(처음 묶은 지 `PROBE_WINDOWS` 안)에 풀리면 그것도 "묶은 것이 원인"으로 보고 다음 기다림을 늘린다.
    #[test]
    fn early_release_after_start_counts_as_failed_pin() {
        let mut d = Decider::default();
        assert_eq!(d.step(SEVERE), Verdict::Release);
        assert_eq!(windows_until_rebind(&mut d, GOOD), HOLD_MIN);
        assert_eq!(d.step(SEVERE), Verdict::Release);
        assert_eq!(windows_until_rebind(&mut d, GOOD), HOLD_MIN * 2);
    }

    /// 계속 나쁜 환경(묶으면 곧 나빠짐)에서 1시간 동안 풀고 묶는 횟수의 상한: 기다림이 320창까지 늘어 한 시간에 16번
    /// (처음 6번이 약 11분 안, 그 뒤는 약 5분 24초마다 한 번 — 묶여 있는 시간은 매번 2초).
    #[test]
    fn flapping_is_bounded_in_hostile_environment() {
        let mut d = Decider::default();
        let mut releases = 0;
        for _ in 0..3_600 {
            // 묶여 있으면 나쁜 창, 풀려 있으면 좋은 창(= 고정이 원인인 환경)
            let w = if d.pinned() { BAD } else { GOOD };
            if d.step(w) == Verdict::Release {
                releases += 1;
            }
        }
        assert_eq!(releases, 16);
    }

    #[test]
    fn collector_rolls_once_per_window_and_counts_stalls() {
        let t0 = Instant::now();
        let mut c = Collector::new(t0, 100);
        c.note_wake(LATE_WAKE - Duration::from_micros(1));
        c.note_wake(LATE_WAKE);
        c.note_body(SLOW_BODY - Duration::from_micros(1));
        c.note_body(SLOW_BODY);
        assert_eq!(c.roll(t0 + WINDOW - Duration::from_millis(1), 103, || panic!("창이 차기 전에는 부르지 않는다")), None);
        assert_eq!(c.roll(t0 + WINDOW, 103, || Some(42)), Some(Window { xruns: 3, stalls: 2, p_busy_pct: Some(42) }));
        // 다음 창은 0부터, 기준은 직전 창 끝
        assert_eq!(c.roll(t0 + WINDOW + WINDOW / 2, 104, || None), None);
        assert_eq!(c.roll(t0 + WINDOW * 2, 104, || None), Some(Window { xruns: 1, stalls: 0, p_busy_pct: None }));
        // 카운터가 줄어도(엔진 재시작은 새 Collector지만 방어) 0에서 멈춘다
        assert_eq!(c.roll(t0 + WINDOW * 3, 0, || None), Some(Window { xruns: 0, stalls: 0, p_busy_pct: None }));
    }

    #[test]
    fn shared_state_publishes_verdicts() {
        let a = AdaptivePin::default();
        assert!(a.want_pinned());
        assert_eq!(a.stats(), AdaptiveStats { released: false, releases: 0, rebinds: 0, p_busy_pct: None });
        a.publish(Verdict::Keep, Some(30));
        assert_eq!(a.stats().p_busy_pct, Some(30));
        a.publish(Verdict::Release, Some(250));
        assert!(!a.want_pinned());
        assert_eq!(a.stats(), AdaptiveStats { released: true, releases: 1, rebinds: 0, p_busy_pct: Some(100) });
        a.publish(Verdict::Rebind, None);
        assert_eq!(a.stats(), AdaptiveStats { released: false, releases: 1, rebinds: 1, p_busy_pct: None });
        assert_eq!(serde_json::to_string(&a.stats()).unwrap(), r#"{"released":false,"releases":1,"rebinds":1,"pBusyPct":null}"#);
    }
}
