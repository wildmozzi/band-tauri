//! 커맨드 ↔ 워커 공유(`MetronomeCtl`·`MetronomeRt`, 상태 칸 `MetroReport`)와 워커 쪽 스케줄러(`OutClock`·`BeatScheduler`).
//! 워커 쪽 끝은 기다리지 않는다(원자값·`try_lock`·seqlock, 엔진 ②b-6b). 클릭은 `click::ClickShared`로 올린다(엔진 ⑥-3).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use super::click::{decode, encode, ClickShared, NO_CLICK};
use super::plan::{MetronomePlan, MetronomeStats, OutputLatencySource, MAX_LOOPBACK_OFFSET_MS, MIN_LOOPBACK_OFFSET_MS};
use band_dsp::stats::{SeqCell, SeqWriter};

/// 올려 둔 박의 자리가 옮겨질 때(기준 시계 추정이 뜀 — 동기 갱신·기준 피어 변경, 콜백 공백 뒤 장치 시계 재수렴, 계획 변경)
/// 그 박을 새 자리에서 이만큼(초)까지 어긋나게 울린다. 새 자리가 이미 지나 이보다 늦게밖에 울릴 수 없으면 그 박은 빼고
/// (`skippedClicks`) 다음 박부터 새 시계로 맞춘다(재정렬 1회). 근거는 설계 1.4절 "시계 추정이 뛸 때".
const MAX_SHIFT_S: f64 = 0.020;
/// 세대가 바뀐 반복에서 올려 둔 클릭을 새 계획의 가장 가까운 박으로 보는 범위(박 간격 비율). 그 박의 새 자리로 옮기고
/// (`MAX_SHIFT_S` 규칙), 넘으면 새 계획에 없는 박이라 거둔다.
const ADOPT_FRAC: f64 = 0.25;
/// 두 클릭 사이 최소 간격(박 간격 비율). 마지막 클릭에서 이보다 가까운 박은 이미 울린 박이 새 시계·새 계획으로 다시 계산된
/// 것이라 건너뛴다(이중 클릭 방지).
const MIN_GAP_FRAC: f64 = 0.5;

/// 워커가 0.1초마다(또는 계획이 바뀐 반복에) 내는 상태 한 벌. 전부 수·불리언이라 seqlock 칸(`SeqCell`)에 싣는다.
/// `MetronomeStats`에서 `ref_peer_id`(문자열 — 제어 쪽 계획에서 읽는다)와 클릭 카운터(`ClickShared` 원자값)를 뺀 것.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MetroReport {
    /// 워커가 적용한 계획 세대.
    pub applied_gen: u64,
    pub running: bool,
    pub synced: bool,
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub start_us: f64,
    pub clock_offset_ms: Option<f64>,
    pub clock_rtt_ms: Option<f64>,
    pub drift_ppm: Option<f64>,
    pub beat: Option<u64>,
    pub output_latency_ms: Option<f64>,
    pub output_latency_source: Option<OutputLatencySource>,
    pub ref_clock_unsupported: bool,
    pub clock_extrapolation_ms: Option<f64>,
}

/// `MetroReport` 칸 수.
const REPORT_WORDS: usize = 10;

// 칸 1의 비트: 아래 32비트 = 플래그, 위 32비트 = `beats_per_bar`.
const F_RUNNING: u64 = 1 << 0;
const F_SYNCED: u64 = 1 << 1;
const F_UNSUPPORTED: u64 = 1 << 2;
const F_OFFSET: u64 = 1 << 3;
const F_RTT: u64 = 1 << 4;
const F_DRIFT: u64 = 1 << 5;
const F_BEAT: u64 = 1 << 6;
const F_OUT_LAT: u64 = 1 << 7;
const F_EXTRAP: u64 = 1 << 8;
const F_SRC_DRIVER: u64 = 1 << 9;
const F_SRC_LOOPBACK: u64 = 1 << 10;

impl MetroReport {
    /// 전부 0인 칸은 `MetroReport::default()`다(워커가 한 번도 내기 전의 상태 = 전과 같은 `MetronomeStats::default()`).
    fn pack(&self) -> [u64; REPORT_WORDS] {
        let bit = |on: bool, f: u64| if on { f } else { 0 };
        let flags = bit(self.running, F_RUNNING)
            | bit(self.synced, F_SYNCED)
            | bit(self.ref_clock_unsupported, F_UNSUPPORTED)
            | bit(self.clock_offset_ms.is_some(), F_OFFSET)
            | bit(self.clock_rtt_ms.is_some(), F_RTT)
            | bit(self.drift_ppm.is_some(), F_DRIFT)
            | bit(self.beat.is_some(), F_BEAT)
            | bit(self.output_latency_ms.is_some(), F_OUT_LAT)
            | bit(self.clock_extrapolation_ms.is_some(), F_EXTRAP)
            | match self.output_latency_source {
                Some(OutputLatencySource::Driver) => F_SRC_DRIVER,
                Some(OutputLatencySource::Loopback) => F_SRC_LOOPBACK,
                None => 0,
            };
        let f = |v: Option<f64>| v.unwrap_or(0.0).to_bits();
        [
            self.applied_gen,
            flags | (self.beats_per_bar as u64) << 32,
            self.bpm.to_bits(),
            self.start_us.to_bits(),
            f(self.clock_offset_ms),
            f(self.clock_rtt_ms),
            f(self.drift_ppm),
            self.beat.unwrap_or(0),
            f(self.output_latency_ms),
            f(self.clock_extrapolation_ms),
        ]
    }

    fn unpack(w: &[u64; REPORT_WORDS]) -> Self {
        let fl = w[1];
        let on = |f: u64| fl & f != 0;
        let f = |flag: u64, i: usize| on(flag).then(|| f64::from_bits(w[i]));
        Self {
            applied_gen: w[0],
            running: on(F_RUNNING),
            synced: on(F_SYNCED),
            bpm: f64::from_bits(w[2]),
            beats_per_bar: (fl >> 32) as u32,
            start_us: f64::from_bits(w[3]),
            clock_offset_ms: f(F_OFFSET, 4),
            clock_rtt_ms: f(F_RTT, 5),
            drift_ppm: f(F_DRIFT, 6),
            beat: on(F_BEAT).then_some(w[7]),
            output_latency_ms: f(F_OUT_LAT, 8),
            output_latency_source: if on(F_SRC_DRIVER) {
                Some(OutputLatencySource::Driver)
            } else if on(F_SRC_LOOPBACK) {
                Some(OutputLatencySource::Loopback)
            } else {
                None
            },
            ref_clock_unsupported: on(F_UNSUPPORTED),
            clock_extrapolation_ms: f(F_EXTRAP, 9),
        }
    }
}

/// 루프백 보정값 없음(`loopback_offset` 원자값). 유효한 값은 −50..200ms의 유한값이라 이 비트열(NaN)과 겹치지 않는다.
const NO_LOOPBACK_OFFSET: u64 = u64::MAX;

/// 커맨드 ↔ 워커 공유(엔진 ②b-6b, 설계 4.2절). **워커는 여기서 기다리지 않는다**:
/// - 계획: 제어 평면이 락 안에서 쓰고 세대를 발행한다. 워커는 세대가 바뀐 반복에서만 `try_lock`으로 복사한다.
/// - 루프백 보정: 원자값.
/// - 상태 보고: seqlock 칸(워커 하나가 쓴다 — 작성 끝은 `MetronomeRt`).
///
/// 제어 쪽 끝이다. 워커 쪽 끝 `MetronomeRt`와 짝으로만 만든다(`pair`).
#[derive(Debug)]
pub struct MetronomeCtl {
    /// (세대, 계획). 제어 평면끼리만 기다린다(`set_plan`, `stats`의 `ref_peer_id` 참조 복제).
    plan: Mutex<(u64, MetronomePlan)>,
    /// `plan.0`의 사본. `set_plan`이 락 안에서 새 세대를 쓴 뒤 발행한다.
    plan_gen: AtomicU64,
    /// 워커가 `try_lock`에 실패해 옛 계획으로 돈 반복 수(진단, 설계 7.4절).
    plan_retries: AtomicU64,
    /// 상태 보고 칸(읽는 쪽). 작성 끝(`SeqWriter`)은 `MetronomeRt`가 갖는다.
    report: Arc<SeqCell<REPORT_WORDS>>,
    pub(crate) click: Arc<ClickShared>,
    /// 이 장치 구성의 루프백 보정값 `offsetMs`의 `f64` 비트(PR #34 M2(a)). 드라이버가 출력 지연을 보고하지 않을 때 쓴다.
    loopback_offset: AtomicU64,
    /// 계획에서 내려온 옛 `ref_peer_id`: (그 문자열이 계획에서 빠진 세대, 문자열). 워커가 그 세대 이상을 적용했다고
    /// 보고하면(워커 복사본이 더는 쥐지 않음) 제어 평면이 놓는다 — 워커가 옛 문자열의 마지막 참조가 되어 해제하지 않게
    /// (#168 리뷰 M-1). 워커는 이 칸을 보지 않는다. 용량 `RETIRED_CAP`은 `pair`에서 미리 잡는다.
    retired: Mutex<Vec<(u64, Arc<str>)>>,
}

/// `MetronomeCtl::retired` 상한. 워커가 이만큼의 계획 변경을 적용하지 못한 채(멈춤·정지) 더 쌓이면 가장 옛 것부터 놓는다 —
/// 그때만 워커가 마지막 참조일 수 있다(워커가 그 문자열을 아직 쥐고 있을 때).
const RETIRED_CAP: usize = 16;

/// `MetronomeCtl`의 워커 쪽 끝. `Clone`이 아니고 `pair`가 하나만 만든다 — 상태 보고 칸의 작성자가 하나임이 타입으로
/// 보장된다(`publish(&mut self)`, #141 리뷰 M-1). 모든 메서드는 기다리지 않는다(원자값·`try_lock`).
#[derive(Debug)]
pub struct MetronomeRt {
    ctl: Arc<MetronomeCtl>,
    /// 상태 보고 칸의 작성 끝(하나뿐 — `SeqCell::pair`).
    report: SeqWriter<REPORT_WORDS>,
}

impl MetronomeCtl {
    /// (제어 쪽 끝, 워커 쪽 끝).
    pub fn pair() -> (Arc<Self>, MetronomeRt) {
        let (report_cell, report) = SeqCell::pair();
        let ctl = Arc::new(Self {
            plan: Mutex::default(),
            plan_gen: AtomicU64::new(0),
            plan_retries: AtomicU64::new(0),
            report: report_cell,
            click: Arc::default(),
            loopback_offset: AtomicU64::new(NO_LOOPBACK_OFFSET),
            retired: Mutex::new(Vec::with_capacity(RETIRED_CAP)),
        });
        (ctl.clone(), MetronomeRt { ctl, report })
    }

    /// 루프백 보정값 설정·해제. 범위 밖이면 거부한다.
    pub(crate) fn set_loopback_offset(&self, offset_ms: Option<f64>) -> Result<(), String> {
        if let Some(o) = offset_ms {
            if !o.is_finite() || !(MIN_LOOPBACK_OFFSET_MS..=MAX_LOOPBACK_OFFSET_MS).contains(&o) {
                return Err(format!("offsetMs는 {MIN_LOOPBACK_OFFSET_MS}..{MAX_LOOPBACK_OFFSET_MS}ms여야 합니다"));
            }
        }
        self.loopback_offset.store(offset_ms.map_or(NO_LOOPBACK_OFFSET, f64::to_bits), Ordering::Release);
        Ok(())
    }

    pub(crate) fn loopback_offset_ms(&self) -> Option<f64> {
        match self.loopback_offset.load(Ordering::Acquire) {
            NO_LOOPBACK_OFFSET => None,
            b => Some(f64::from_bits(b)),
        }
    }

    /// 새 계획을 넣고 세대를 발행한다. `ref_peer_id` 문자열은 워커가 해제하지 않게 다룬다(#168 리뷰 M-1):
    /// - 지금 계획과 같은 문자열이면 지금의 `Arc`를 그대로 쓴다(새 `Arc`는 부른 쪽이 놓는다) — 같은 피어 재전송은 워커 쪽 변화 0.
    /// - 다르면 옛 `Arc`를 `retired`로 옮기고, 워커가 그 세대 이상을 적용했다고 보고한 뒤의 `set_plan`에서 놓는다.
    pub fn set_plan(&self, mut plan: MetronomePlan) {
        // 락 밖에서 읽는다(보고가 늦으면 더 작게 보일 뿐 — 놓는 시점이 늦어지는 쪽이라 안전하다).
        let applied = MetroReport::unpack(&self.report.read()).applied_gen;
        let (gen, old) = {
            let mut p = self.plan.lock();
            if plan.ref_peer_id.as_deref() == p.1.ref_peer_id.as_deref() {
                plan.ref_peer_id.clone_from(&p.1.ref_peer_id);
            }
            p.0 += 1;
            let gen = p.0;
            let old = std::mem::replace(&mut p.1, plan).ref_peer_id;
            let still = old.as_ref().is_some_and(|o| p.1.ref_peer_id.as_ref().is_some_and(|n| Arc::ptr_eq(o, n)));
            self.plan_gen.store(gen, Ordering::Release);
            (gen, if still { None } else { old })
        };
        let mut r = self.retired.lock();
        r.retain(|(g, _)| *g > applied);
        if let Some(o) = old {
            if r.len() == RETIRED_CAP {
                r.remove(0);
            }
            r.push((gen, o));
        }
    }

    /// `EngineStats.metronome`. 수 필드는 워커가 마지막으로 **적용한** 계획과 계산값(칸 한 벌)이고, `ref_peer_id`만 제어 쪽
    /// 계획에서 읽는다 — `set_plan` 직후 워커 반복 1회(약 1ms) 동안 수 필드보다 한 발 앞설 수 있다(계약 문서 4절).
    pub(crate) fn stats(&self) -> MetronomeStats {
        let r = MetroReport::unpack(&self.report.read());
        // 락 안은 참조 수 증가 하나(할당 없음, #143 리뷰 L-4). 계약의 문자열은 락 밖에서 만든다.
        let ref_peer = self.plan.lock().1.ref_peer_id.clone();
        let ref_peer_id = ref_peer.as_deref().map(String::from);
        MetronomeStats {
            running: r.running,
            synced: r.synced,
            bpm: r.bpm,
            beats_per_bar: r.beats_per_bar,
            start_us: r.start_us,
            ref_peer_id,
            clock_offset_ms: r.clock_offset_ms,
            clock_rtt_ms: r.clock_rtt_ms,
            drift_ppm: r.drift_ppm,
            beat: r.beat,
            clicks: self.click.clicks.load(Ordering::Relaxed),
            late_clicks: self.click.late.load(Ordering::Relaxed),
            skipped_clicks: self.click.skipped.load(Ordering::Relaxed),
            output_latency_ms: r.output_latency_ms,
            output_latency_source: r.output_latency_source,
            ref_clock_unsupported: r.ref_clock_unsupported,
            clock_extrapolation_ms: r.clock_extrapolation_ms,
        }
    }

    /// (지금 계획 세대, 워커가 마지막 보고에 실은 적용 세대). 진단·테스트: 워커가 새 계획을 적용했는지 본다.
    #[cfg(test)]
    pub(crate) fn plan_gens(&self) -> (u64, u64) {
        (self.plan_gen.load(Ordering::Acquire), MetroReport::unpack(&self.report.read()).applied_gen)
    }

    /// 워커가 계획 `try_lock`에 실패한 반복 수를 돌려주고 비운다(진단·테스트).
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn take_plan_retries(&self) -> u64 {
        self.plan_retries.swap(0, Ordering::Relaxed)
    }
}

impl MetronomeRt {
    /// 반복마다. 세대가 `mine.0`과 같으면 원자 읽기 하나로 끝난다. 다르면 `try_lock`으로 복사한다 — 못 잡으면(제어 스레드가
    /// `set_plan`·`stats` 중) 옛 계획으로 이번 반복을 돌고 다음 반복에 다시 본다. 할당 없음(`ref_peer_id`는 `Arc<str>` — 참조 수만 바뀐다).
    pub fn refresh_plan(&self, mine: &mut (u64, MetronomePlan)) {
        let c = &self.ctl;
        if c.plan_gen.load(Ordering::Acquire) == mine.0 {
            return;
        }
        let Some(p) = c.plan.try_lock() else {
            c.plan_retries.fetch_add(1, Ordering::Relaxed);
            return;
        };
        mine.0 = p.0;
        let (m, src) = (&mut mine.1, &p.1);
        m.running = src.running;
        m.bpm = src.bpm;
        m.beats_per_bar = src.beats_per_bar;
        m.start_us = src.start_us;
        m.ref_peer_id.clone_from(&src.ref_peer_id); // `Arc` 참조 수(옛 값의 마지막 참조였다면 여기서 해제)
    }

    pub(crate) fn loopback_offset_ms(&self) -> Option<f64> {
        self.ctl.loopback_offset_ms()
    }

    pub(crate) fn click(&self) -> &ClickShared {
        &self.ctl.click
    }

    /// 상태 한 벌을 낸다(기다리지 않는다). `&mut self` + `Clone` 아님 = 작성자 하나.
    pub fn publish(&mut self, r: &MetroReport) {
        self.report.write(&r.pack());
    }
}

// ============================================================================
// 워커 쪽 스케줄러
// ============================================================================

/// 출력 장치 시각 사상: 프레임 `played`의 출력 콜백 시각 추정이 `dev_now_us`(워커 `FeedTiming`).
#[derive(Debug, Clone, Copy)]
pub struct OutClock {
    /// 마지막 출력 콜백 시각 추정(내 시계 µs). 0이면 아직 모름.
    pub dev_now_us: f64,
    /// 그 콜백 끝의 누적 재생 프레임.
    pub played: u64,
    pub rate: f64,
    /// 출력 콜백 크기(장치 프레임).
    pub burst: u64,
    /// 장치 출력 버퍼 1개 너머의 숨은 출력 지연(µs): 드라이버 보고 출력 지연 − 콜백 크기(PR #34 M2).
    /// WASAPI 공유 엔진 주기·드라이버 안전 오프셋·USB 내부 버퍼. 모르면 0(보정 안 됨).
    pub extra_out_us: f64,
}

/// 드라이버 보고 출력 지연(장치 프레임, 0 = 모름)에서 숨은 출력 지연(µs)을 구한다.
/// 보고값은 "콜백이 넘긴 버퍼가 들리기까지"이고 이미 버퍼 1개(콜백 크기)를 포함하므로 그만큼 뺀다.
/// 반환: `None` = 드라이버가 알려 주지 않음(보정 0).
pub(crate) fn hidden_out_latency_us(reported_frames: u32, burst_frames: u64, rate: f64) -> Option<f64> {
    if reported_frames == 0 || rate <= 0.0 {
        return None;
    }
    Some((reported_frames as f64 - burst_frames as f64).max(0.0) * 1e6 / rate)
}

impl OutClock {
    /// 장치 프레임 J가 귀에 닿는 시각(내 시계 µs). 콜백이 프레임 [b, b+n)을 쓰는 시각은 `m + (b+n)/r`이고
    /// (`m` = `DeviceClock`의 `벽시계 − 재생/레이트` 최솟값), 그 버퍼는 장치 출력 버퍼 1개(n)와
    /// 숨은 출력 지연(`extra_out_us`) 뒤에 울린다. 따라서 귀 시각 = `m + (J + 2n)/r + extra`.
    pub(crate) fn ear_time_us(&self, frame: f64) -> f64 {
        self.dev_now_us + (frame + 2.0 * self.burst as f64 - self.played as f64) * 1e6 / self.rate + self.extra_out_us
    }

    /// 귀 시각 `t_us`에 울릴 장치 프레임.
    pub(crate) fn frame_for(&self, t_us: f64) -> f64 {
        (t_us - self.extra_out_us - self.dev_now_us) * self.rate / 1e6 + self.played as f64 - 2.0 * self.burst as f64
    }

    /// 이 프레임 이상이어야 다음 콜백(진행 중일 수 있음)과 겹치지 않는다: 콜백 2개 + 2ms.
    fn guard(&self) -> f64 {
        self.played as f64 + 2.0 * self.burst as f64 + self.rate * 0.002
    }
}

/// 다음 클릭 하나를 미리 올려 두는 스케줄러(워커). 기준 시계 → 내 시계 → 장치 프레임.
#[derive(Debug, Default)]
pub struct BeatScheduler {
    gen: u64,
    /// 올려 둔 클릭: (박 번호, 올린 원자값).
    pending: Option<(u64, u64)>,
    last_beat: Option<u64>,
    /// 마지막으로 콜백이 가져간 클릭의 귀 시각(내 시계 µs). 세대가 바뀌어도 남는다(박 번호와 달리 시각은 계획과 무관).
    last_click_us: Option<f64>,
}

impl BeatScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    fn withdraw(&mut self, sh: &ClickShared) {
        if let Some((_, raw)) = self.pending.take() {
            let _ = sh.next.compare_exchange(raw, NO_CLICK, Ordering::AcqRel, Ordering::Relaxed);
        }
    }

    /// 올려 둔 클릭을 콜백이 가져갔다(울림). `numbered` = 박 번호가 지금 계획 것이다(세대가 바뀐 반복이면 아니다 — 시각만 남긴다).
    fn taken(&mut self, k: u64, raw: u64, clock: &OutClock, numbered: bool) {
        self.pending = None;
        if numbered {
            self.last_beat = Some(k);
        }
        self.last_click_us = Some(clock.ear_time_us(decode(raw).0 as f64));
    }

    /// 계획 세대가 바뀐 반복: 올려 둔 클릭이 새 계획의 어느 박 근처(박 간격의 `ADOPT_FRAC` 안)면 그 박으로 이름을 바꾼다 —
    /// 자리·강박은 이어서 `follow`가 새 계획대로 맞춘다. 아니면 거둔다(새 계획에 없는 박은 울리지 않는다).
    ///
    /// 처음(#143)에는 세대가 바뀌면 무조건 거둬, 클릭이 이미 가드(콜백 2개 + 2ms) 안이면 같은 계획을 다시 넣어도 박이 빠졌다
    /// (#143 리뷰 L-5). #168은 박 자리 ±1ms 안만 남겨, 그 사이 시계 추정이 1ms 넘게 움직이면(기준 피어 변경 등) 여전히 빠졌다
    /// (#168 리뷰 L-3). 이제 박의 동일성은 "가장 가까운 새 박"으로 정하고, 자리가 얼마나 옮겨졌는지는 `follow`의 규칙 하나가 다룬다.
    fn adopt_pending(&mut self, plan: &MetronomePlan, offset: f64, clock: &OutClock, sh: &ClickShared) {
        let Some((_, raw)) = self.pending else {
            return;
        };
        let frame = decode(raw).0 as f64;
        let period = plan.period_us();
        // 이 클릭이 귀에 닿는 기준 시계 시각 → 새 계획의 가장 가까운 박
        let k = ((clock.ear_time_us(frame) + offset - plan.start_us) / period).round();
        let near = k >= 0.0 && (clock.frame_for(plan.start_us + k * period - offset) - frame).abs() <= clock.rate * period * 1e-6 * ADOPT_FRAC;
        if near {
            self.pending = Some((k as u64, raw));
        } else {
            self.withdraw(sh);
        }
    }

    /// 올려 둔 박 `k`(아직 콜백이 가져가지 않음)를 지금 시계·계획의 자리 `new`(장치 프레임)와 강박 `want`로 맞춘다.
    /// 반환: 아직 올려 둔 박이 있다(없으면 호출자가 다음 박을 고른다).
    ///
    /// 가드(`OutClock::guard`) 너머로는 언제든 옮길 수 있다: 다음 콜백 2개가 가져가지 않는 구간이라, 콜백의 `load`와 CAS 사이에
    /// 워커 CAS가 끼어도 콜백은 이번에 건너뛰고 새 자리에서 제때 울린다. 가드 안에서 자리를 다시 쓰면 그 경합이 늦은 클릭이 되므로
    /// 가드 안 클릭의 자리는 건드리지 않는다(강박만, 다음 콜백 2개 밖에서 — #168 리뷰 L-1).
    /// - 새 자리가 가드 너머: 거기로. 시계가 뒤로 뛰어 박이 밀리면 곧 울릴 클릭도 옮긴다(전에는 옛 자리에 일찍 울렸다).
    /// - 새 자리가 가드 안이고 클릭은 가드 너머: 가드에 붙여 늦게 울린다(늦음 ≤ `MAX_SHIFT_S`). 더 늦으면 뺀다.
    ///   전에는 5ms 넘게 늦으면 무조건 뺐다(콜백 공백용 규칙이 시계 추정 점프에도 걸려 박이 빠졌다).
    /// - 둘 다 가드 안: 옛 자리에 울린다(새 자리에서 `MAX_SHIFT_S` 안일 때). 그보다 늦은 자리면 거둔다(뺀 박).
    fn follow(&mut self, k: u64, raw: u64, new: f64, want: bool, clock: &OutClock, sh: &ClickShared) -> bool {
        let (frame, accent) = decode(raw);
        let old = frame as f64;
        let guard = clock.guard();
        let max_shift = clock.rate * MAX_SHIFT_S;
        let v = if new >= guard {
            encode(new as u64, want)
        } else if old >= guard {
            if guard - new <= max_shift {
                encode(guard.ceil() as u64, want)
            } else {
                NO_CLICK
            }
        } else if old - new > max_shift {
            NO_CLICK
        } else if want != accent && old >= clock.played as f64 + 2.0 * clock.burst as f64 {
            encode(frame, want)
        } else {
            raw
        };
        if v == raw {
            return true;
        }
        if sh.next.compare_exchange(raw, v, Ordering::AcqRel, Ordering::Relaxed).is_err() {
            // 그 사이 콜백이 가져갔다(옛 값으로 울림)
            self.taken(k, raw, clock, true);
            return false;
        }
        if v == NO_CLICK {
            // 새 자리가 이미 `MAX_SHIFT_S` 넘게 지났다(콜백 공백 뒤, 큰 시계 점프): 늦은 클릭을 내지 않고 빼고 다음 박부터 맞춘다.
            self.pending = None;
            self.last_beat = Some(k);
            sh.skipped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.pending = Some((k, v));
        true
    }

    /// 반복마다 호출. `offset_us` = 기준 시계 − 내 시계(방장 0, 모르면 None). 반환: 올려 둔 박 번호.
    pub fn tick(&mut self, gen: u64, plan: &MetronomePlan, offset_us: Option<f64>, clock: &OutClock, sh: &ClickShared) -> Option<u64> {
        let regen = gen != self.gen;
        if regen {
            self.gen = gen;
            self.last_beat = None;
        }
        let (true, Some(offset)) = (plan.running, offset_us) else {
            self.withdraw(sh);
            return None;
        };
        if clock.dev_now_us <= 0.0 || clock.rate <= 0.0 {
            if regen {
                self.withdraw(sh);
            }
            return self.pending.map(|p| p.0);
        }
        if let Some((k, raw)) = self.pending {
            if sh.next.load(Ordering::Acquire) != raw {
                // 콜백이 가져갔다(울림). 세대가 바뀐 반복이면 박 번호는 옛 계획 것이라 시각만 남긴다.
                self.taken(k, raw, clock, !regen);
            }
        }
        let period = plan.period_us();
        let bpb = plan.beats_per_bar as u64;
        let beat_frame = |k: u64| clock.frame_for(plan.start_us + k as f64 * period - offset).round();
        if regen {
            self.adopt_pending(plan, offset, clock, sh);
        }
        if let Some((k, raw)) = self.pending {
            if self.follow(k, raw, beat_frame(k), k.is_multiple_of(bpb), clock, sh) {
                return Some(k);
            }
        }
        // 다음 박: 지난 박 다음, 지금 올려도 늦지 않은 첫 박(0 이상), 그리고 마지막 클릭에서 박 간격의 `MIN_GAP_FRAC` 넘게 떨어진 박.
        let guard = clock.guard();
        let t_guard_ref = clock.ear_time_us(guard) + offset;
        let k_min = ((t_guard_ref - plan.start_us) / period).ceil().max(0.0) as u64;
        let mut k = self.last_beat.map_or(k_min, |l| (l + 1).max(k_min));
        let not_before = self.last_click_us.map_or(f64::NEG_INFINITY, |t| t + period * MIN_GAP_FRAC);
        let mut f = beat_frame(k);
        while f < guard || clock.ear_time_us(f) < not_before {
            k += 1;
            f = beat_frame(k);
        }
        let v = encode(f as u64, k.is_multiple_of(bpb));
        if sh.next.compare_exchange(NO_CLICK, v, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            self.pending = Some((k, v));
            Some(k)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(played: u64) -> OutClock {
        OutClock { dev_now_us: played as f64 / 48_000.0 * 1e6 + 10_000.0, played, rate: 48_000.0, burst: 128, extra_out_us: 0.0 }
    }

    fn plan(bpm: f64, start: f64) -> MetronomePlan {
        MetronomePlan { running: true, bpm, beats_per_bar: 4, start_us: start, ref_peer_id: None }
    }

    #[test]
    fn scheduler_publishes_beats_in_order_with_accents() {
        let sh = ClickShared::default();
        let mut s = BeatScheduler::new();
        let c = clock(48_000);
        let p = plan(120.0, c.ear_time_us(c.played as f64) + 100_000.0);
        assert_eq!(s.tick(1, &p, Some(0.0), &c, &sh), Some(0));
        let (f0, acc0) = decode(sh.next.load(Ordering::Relaxed));
        assert!(acc0);
        assert!((c.ear_time_us(f0 as f64) - p.start_us).abs() < 1e6 / 48_000.0);
        // 콜백이 가져가면 다음 박(0.5초 뒤, 약박)
        sh.next.store(NO_CLICK, Ordering::Release);
        assert_eq!(s.tick(1, &p, Some(0.0), &c, &sh), Some(1));
        let (f1, acc1) = decode(sh.next.load(Ordering::Relaxed));
        assert!(!acc1);
        assert_eq!(f1 - f0, 24_000);
        // 정지하면 거둔다
        let mut stopped = p.clone();
        stopped.running = false;
        assert_eq!(s.tick(1, &stopped, Some(0.0), &c, &sh), None);
        assert_eq!(sh.next.load(Ordering::Relaxed), NO_CLICK);
    }

    #[test]
    fn scheduler_skips_past_beats_and_waits_for_sync() {
        let sh = ClickShared::default();
        let mut s = BeatScheduler::new();
        let c = clock(480_000);
        // 박 0이 3.2초 전 → 지금 이후 첫 박부터
        let p = plan(60.0, c.ear_time_us(c.played as f64) - 3_200_000.0);
        assert_eq!(s.tick(1, &p, None, &c, &sh), None, "시계 모르면 내지 않음");
        assert_eq!(s.tick(1, &p, Some(0.0), &c, &sh), Some(4));
    }

    #[test]
    fn scheduler_retimes_only_far_clicks() {
        let sh = ClickShared::default();
        let mut s = BeatScheduler::new();
        let c = clock(48_000);
        let p = plan(120.0, c.ear_time_us(c.played as f64) + 200_000.0);
        s.tick(1, &p, Some(0.0), &c, &sh);
        let f0 = decode(sh.next.load(Ordering::Relaxed)).0;
        // 오프셋이 1ms 바뀌면 48프레임 이동
        s.tick(1, &p, Some(1000.0), &c, &sh);
        assert_eq!(decode(sh.next.load(Ordering::Relaxed)).0, f0 - 48);
        // 클릭이 코앞이면 그대로 둔다
        let near = OutClock { played: f0 - 100, dev_now_us: c.dev_now_us + (f0 - 100 - c.played) as f64 / 48.0 * 1000.0, ..c };
        let before = sh.next.load(Ordering::Relaxed);
        s.tick(1, &p, Some(5000.0), &near, &sh);
        assert_eq!(sh.next.load(Ordering::Relaxed), before);
    }

    #[test]
    fn hidden_output_latency_shifts_click_earlier() {
        // 드라이버 보고 출력 지연 = 콜백 128 + 숨은 352프레임(7.33ms @48k)
        assert_eq!(hidden_out_latency_us(0, 128, 48_000.0), None, "모르면 보정 안 함");
        assert_eq!(hidden_out_latency_us(100, 128, 48_000.0), Some(0.0), "보고값이 버퍼보다 작으면 0");
        let h = hidden_out_latency_us(480, 128, 48_000.0).unwrap();
        assert!((h - 352.0 / 48.0 * 1000.0).abs() < 1e-6);
        let c0 = clock(48_000);
        let c1 = OutClock { extra_out_us: h, ..c0 };
        let t = c0.ear_time_us(60_000.0) + 50_000.0;
        // 같은 귀 시각에 울리려면 숨은 지연만큼 이른 프레임에 써야 한다
        assert!((c0.frame_for(t) - c1.frame_for(t) - 352.0).abs() < 1e-6);
        assert!((c1.ear_time_us(c1.frame_for(t)) - t).abs() < 1e-6);
    }

    #[test]
    fn loopback_offset_command_range() {
        // 커맨드 범위 검사
        let (ctl, rt) = MetronomeCtl::pair();
        assert_eq!(rt.loopback_offset_ms(), None);
        assert!(ctl.set_loopback_offset(Some(f64::INFINITY)).is_err());
        assert!(ctl.set_loopback_offset(Some(500.0)).is_err());
        assert!(ctl.set_loopback_offset(Some(7.5)).is_ok());
        assert_eq!(rt.loopback_offset_ms(), Some(7.5));
        assert!(ctl.set_loopback_offset(Some(-50.0)).is_ok());
        assert_eq!(rt.loopback_offset_ms(), Some(-50.0));
        assert!(ctl.set_loopback_offset(None).is_ok());
        assert_eq!(rt.loopback_offset_ms(), None);
    }

    #[test]
    fn report_pack_roundtrip() {
        assert_eq!(MetroReport::unpack(&[0; REPORT_WORDS]), MetroReport::default(), "빈 칸 = 기본값");
        let full = MetroReport {
            applied_gen: 7,
            running: true,
            synced: true,
            bpm: 133.5,
            beats_per_bar: 7,
            start_us: 1.25e9,
            clock_offset_ms: Some(-41.5),
            clock_rtt_ms: Some(0.0),
            drift_ppm: Some(-12.25),
            beat: Some(0),
            output_latency_ms: Some(7.33),
            output_latency_source: Some(OutputLatencySource::Loopback),
            ref_clock_unsupported: true,
            clock_extrapolation_ms: Some(1500.0),
        };
        assert_eq!(MetroReport::unpack(&full.pack()), full);
        let driver = MetroReport { output_latency_source: Some(OutputLatencySource::Driver), beat: None, clock_rtt_ms: None, ..full };
        assert_eq!(MetroReport::unpack(&driver.pack()), driver);
        // Some(0.0)과 None은 구분된다
        let none = MetroReport { clock_offset_ms: None, ..full };
        assert_ne!(MetroReport::unpack(&none.pack()), MetroReport { clock_offset_ms: Some(0.0), ..full });
    }

    /// 계획: 세대가 같으면 건드리지 않고, 바뀌면 복사한다. 제어 스레드가 락을 쥐고 있으면 **기다리지 않고** 옛 계획으로 돈다.
    #[test]
    fn worker_end_never_waits_for_plan_lock() {
        let (ctl, mut rt) = MetronomeCtl::pair();
        let mut mine = (0, MetronomePlan::default());
        rt.refresh_plan(&mut mine);
        assert_eq!(mine, (0, MetronomePlan::default()));
        let p1 = MetronomePlan { ref_peer_id: Some("host".into()), ..plan(90.0, 5.0) };
        ctl.set_plan(p1.clone());
        rt.refresh_plan(&mut mine);
        assert_eq!(mine, (1, p1.clone()));
        // 제어 쪽이 락을 쥔 동안(선점된 `set_plan`·`stats`): 워커 끝은 돌아오고 옛 계획을 유지한다
        // 워커 끝은 별도 스레드에서 부르고 시한을 둔다: `try_lock`이 `lock`으로 되돌아가면 멈추지 않고 실패한다(#143 리뷰 L-1).
        ctl.set_plan(plan(200.0, 9.0));
        let held = ctl.plan.lock();
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || {
            rt.refresh_plan(&mut mine);
            rt.refresh_plan(&mut mine);
            let offset = rt.loopback_offset_ms();
            rt.publish(&MetroReport { applied_gen: mine.0, bpm: 90.0, ..Default::default() });
            tx.send((mine.clone(), offset)).unwrap();
            (rt, mine)
        });
        let got = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("워커 쪽 끝이 계획 락을 기다린다");
        drop(held);
        let (rt, mut mine) = t.join().unwrap();
        assert_eq!(got, ((1, p1), None));
        assert_eq!(ctl.take_plan_retries(), 2);
        rt.refresh_plan(&mut mine);
        assert_eq!(mine, (2, plan(200.0, 9.0)));
        assert_eq!(ctl.take_plan_retries(), 0);
        // 상태: 수 필드는 워커가 낸 값(적용 세대 1), refPeerId만 제어 쪽 지금 계획(세대 2)
        let s = ctl.stats();
        assert_eq!((s.bpm, s.ref_peer_id), (90.0, None));
        assert_eq!(ctl.plan_gens(), (2, 1));
    }

    /// #143 리뷰 L-5: 가드 안 클릭은 세대가 바뀌어도 새 계획의 박이면 남고(강박 비트는 새 계획대로), 아니면 거둔다.
    #[test]
    fn regen_keeps_near_click_on_new_grid_and_fixes_accent() {
        let sh = ClickShared::default();
        let mut s = BeatScheduler::new();
        let c = clock(48_000);
        // 박 4(강박, 박자 4)가 가드 안(가드보다 귀 시각 1ms 앞 — 콜백 2개 밖)에 오도록
        let p4 = plan(120.0, c.ear_time_us(c.guard()) - 1000.0 - 4.0 * 500_000.0);
        s.last_beat = Some(3);
        let f4 = p4.start_us + 2_000_000.0;
        let raw = encode(c.frame_for(f4).round() as u64, true);
        sh.next.store(raw, Ordering::Release);
        s.pending = Some((4, raw));
        s.gen = 1;
        assert!(decode(raw).0 < c.guard() as u64);
        assert!(decode(raw).0 >= c.played + 2 * c.burst);
        // 박자만 3으로: 같은 프레임, 박 4는 약박
        let p3 = MetronomePlan { beats_per_bar: 3, ..p4.clone() };
        assert_eq!(s.tick(2, &p3, Some(0.0), &c, &sh), Some(4));
        assert_eq!(decode(sh.next.load(Ordering::Relaxed)), (decode(raw).0, false));
        // 같은 계획 다시: 그대로
        let now = sh.next.load(Ordering::Relaxed);
        assert_eq!(s.tick(3, &p3, Some(0.0), &c, &sh), Some(4));
        assert_eq!(sh.next.load(Ordering::Relaxed), now);
        // 시작 시각이 2ms 당겨진 계획(#168 리뷰 L-3): 가장 가까운 새 박은 여전히 박 4이고 새 자리도 가드 안이다 — 자리를 다시 쓰지
        // 않고 옛 자리에 울린다(새 자리보다 2ms 늦음 ≤ `MAX_SHIFT_S`). 전에는 ±1ms 밖이라 거둬 박 4가 빠졌다.
        let moved = MetronomePlan { start_us: p3.start_us - 2000.0, ..p3.clone() };
        assert_eq!(s.tick(4, &moved, Some(0.0), &c, &sh), Some(4));
        assert_eq!(sh.next.load(Ordering::Relaxed), now);
        assert_eq!(sh.skipped.load(Ordering::Relaxed), 0);
        // 30ms 당겨진 계획: 옛 자리에 울리면 새 자리보다 30ms 늦다(> 20ms) → 빼고(건너뜀 1) 다음 박(가드 밖)을 올린다
        let far = MetronomePlan { start_us: p3.start_us - 30_000.0, ..p3.clone() };
        assert_eq!(s.tick(5, &far, Some(0.0), &c, &sh), Some(5));
        assert!(decode(sh.next.load(Ordering::Relaxed)).0 as f64 >= c.guard());
        assert_eq!(sh.skipped.load(Ordering::Relaxed), 1);
        // 콜백 2개 안(곧 가져갈 구간)의 클릭은 박자가 바뀌어도 강박 비트를 고치지 않는다(#168 리뷰 L-1 — 콜백 CAS와 겹침 방지)
        let mut s = BeatScheduler::new();
        let soon = encode(c.played + c.burst, true);
        let p_soon = plan(120.0, c.ear_time_us((c.played + c.burst) as f64) - 4.0 * 500_000.0);
        sh.next.store(soon, Ordering::Release);
        s.pending = Some((4, soon));
        s.gen = 1;
        let p_soon3 = MetronomePlan { beats_per_bar: 3, ..p_soon };
        assert_eq!(s.tick(2, &p_soon3, Some(0.0), &c, &sh), Some(4));
        assert_eq!(sh.next.load(Ordering::Relaxed), soon, "곧 울릴 클릭은 옛 강박 그대로");
    }

    /// #168 리뷰 M-1: 같은 문자열 재전송은 지금의 `Arc`를 재사용하고, 바뀐 문자열의 옛 `Arc`는 워커가 그 세대를 적용했다고
    /// 보고한 뒤에 제어 평면이 놓는다(워커가 마지막 참조가 되지 않는다). 해제 0 자체는 `tests/rt_alloc.rs`가 할당 계수로 본다.
    #[test]
    fn set_plan_reuses_same_ref_and_retires_old_until_applied() {
        let (ctl, mut rt) = MetronomeCtl::pair();
        let mut mine = (0, MetronomePlan::default());
        let with = |id: &str| MetronomePlan { ref_peer_id: Some(id.into()), ..plan(120.0, 0.0) };
        let apply = |rt: &mut MetronomeRt, mine: &mut (u64, MetronomePlan)| {
            rt.refresh_plan(mine);
            rt.publish(&MetroReport { applied_gen: mine.0, ..Default::default() });
        };
        ctl.set_plan(with("host"));
        apply(&mut rt, &mut mine);
        let first = mine.1.ref_peer_id.clone().unwrap();
        // 같은 문자열 재전송: 같은 Arc
        ctl.set_plan(with("host"));
        apply(&mut rt, &mut mine);
        assert!(Arc::ptr_eq(mine.1.ref_peer_id.as_ref().unwrap(), &first));
        assert!(ctl.retired.lock().is_empty());
        // 다른 문자열: 옛 Arc는 retired로(워커가 아직 옛 세대)
        ctl.set_plan(with("bob"));
        assert_eq!(ctl.retired.lock().len(), 1);
        drop(first);
        // 워커가 적용하면 옛 Arc의 참조는 retired 하나뿐 — 다음 set_plan이 놓는다
        apply(&mut rt, &mut mine);
        assert_eq!(Arc::strong_count(&ctl.retired.lock()[0].1), 1);
        ctl.set_plan(with("bob"));
        assert!(ctl.retired.lock().is_empty());
        // 상한: 워커가 적용하지 않은 채 계속 바뀌면 RETIRED_CAP에서 멈춘다
        for i in 0..40 {
            ctl.set_plan(with(&format!("p{i}")));
        }
        assert_eq!(ctl.retired.lock().len(), RETIRED_CAP);
    }

    /// #168 리뷰 L-3: 콜백이 박 4를 가져간 직후(워커가 아직 못 본 사이) 세대가 바뀌고 시계가 10ms 뒤로 뛰면, 새 시계로 박 4의
    /// 자리는 가드 너머라 다시 고를 수 있다. 박 번호를 잊은 세대 변경 뒤에도 마지막 클릭 시각이 남아 박 4를 다시 울리지 않는다.
    #[test]
    fn regen_right_after_taken_click_does_not_replay_it() {
        let sh = ClickShared::default();
        let mut s = BeatScheduler::new();
        let c = clock(48_000);
        let p = plan(120.0, c.ear_time_us(c.guard()) + 3000.0 - 2_000_000.0);
        assert_eq!(s.tick(1, &p, Some(0.0), &c, &sh), Some(4));
        sh.next.store(NO_CLICK, Ordering::Release); // 콜백이 가져감
        assert_eq!(s.tick(2, &p, Some(-10_000.0), &c, &sh), Some(5), "박 4를 두 번 울리지 않는다");
        let (f5, _) = decode(sh.next.load(Ordering::Relaxed));
        assert!((c.ear_time_us(f5 as f64) - (p.start_us + 2_500_000.0 + 10_000.0)).abs() < 1e6 / 48_000.0);
        // 박 번호는 새 계획 것으로: 같은 격자를 박 0이 두 박 늦은 계획으로 다시 넣고, 그 직전에 콜백이 가져갔으면 다음 박은
        // 새 번호 3(= 옛 박 5)이다. 옛 번호(4)를 지난 박으로 남기면 새 박 3·4가 빠진다.
        let mut s = BeatScheduler::new();
        sh.next.store(NO_CLICK, Ordering::Release);
        assert_eq!(s.tick(1, &p, Some(0.0), &c, &sh), Some(4));
        sh.next.store(NO_CLICK, Ordering::Release);
        let shifted = MetronomePlan { start_us: p.start_us + 1_000_000.0, ..p.clone() };
        assert_eq!(s.tick(2, &shifted, Some(0.0), &c, &sh), Some(3));
    }

    /// 시계가 뒤로 뛰어(박이 밀림) 가드 안 클릭의 새 자리가 가드 너머가 되면 옮긴다 — 전에는 옛 자리에 일찍 울렸다.
    /// 새 자리도 가드 안이면 그대로 둔다(옛 자리에 울림, 어긋남은 가드 폭 안).
    #[test]
    fn backward_step_moves_in_guard_click_later() {
        let sh = ClickShared::default();
        let mut s = BeatScheduler::new();
        let c = clock(48_000);
        let p = plan(120.0, c.ear_time_us(c.guard()) + 3000.0 - 2_000_000.0);
        assert_eq!(s.tick(1, &p, Some(0.0), &c, &sh), Some(4));
        let f4 = decode(sh.next.load(Ordering::Relaxed)).0;
        // 4ms 지나 클릭이 가드 안(콜백 2개 밖)
        let later = OutClock { played: c.played + 192, dev_now_us: c.dev_now_us + 4000.0, ..c };
        assert!((f4 as f64) < later.guard() && f4 >= later.played + 2 * later.burst);
        let raw = sh.next.load(Ordering::Relaxed);
        assert_eq!(s.tick(1, &p, Some(-500.0), &later, &sh), Some(4));
        assert_eq!(sh.next.load(Ordering::Relaxed), raw, "0.5ms 밀림: 새 자리도 가드 안 → 그대로");
        assert_eq!(s.tick(1, &p, Some(-10_000.0), &later, &sh), Some(4));
        assert_eq!(decode(sh.next.load(Ordering::Relaxed)), (f4 + 480, true), "10ms 밀림: 새 자리(가드 너머)로");
        assert_eq!(sh.skipped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn scheduler_skips_click_made_stale_by_callback_gap() {
        // 박 0을 올려 둔 뒤 장치가 300ms 멈췄다가 재개 → 시계가 다시 잡히면 박 0의 제 시각은 이미 지났다.
        // 공백만큼 늦게 울리지 않고 건너뛰고 다음 박을 올린다(PR #34 M3).
        let sh = ClickShared::default();
        let mut s = BeatScheduler::new();
        let c = clock(48_000);
        let p = plan(120.0, c.ear_time_us(c.played as f64) + 100_000.0);
        assert_eq!(s.tick(1, &p, Some(0.0), &c, &sh), Some(0));
        let (f0, _) = decode(sh.next.load(Ordering::Relaxed));
        // 공백 300ms: 재생 카운터는 거의 그대로(64프레임), 벽시계는 300ms 진행
        let after = OutClock { played: c.played + 64, dev_now_us: c.dev_now_us + 300_000.0, ..c };
        assert_eq!(s.tick(1, &p, Some(0.0), &after, &sh), Some(1), "박 0은 건너뛰고 박 1");
        let (f1, acc1) = decode(sh.next.load(Ordering::Relaxed));
        assert!(!acc1);
        assert!((after.ear_time_us(f1 as f64) - (p.start_us + 500_000.0)).abs() < 1e6 / 48_000.0);
        assert!(f1 > f0);
        assert_eq!(sh.skipped.load(Ordering::Relaxed), 1);
    }
}
