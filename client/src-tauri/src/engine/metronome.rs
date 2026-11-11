//! 공유 메트로놈(P2-3, 설계 1.4절).
//!
//! - 시간축: 기준 피어(방장) 엔진의 시계. 박 k의 시각 = `start_us + k·60/bpm`(기준 시계 µs).
//!   방장 엔진은 기준 피어 없음(오프셋 0), 다른 연주자는 `net::clock::ClockSync`로 추정한 오프셋으로 내 시계로 바꾼다.
//! - 정렬: 모든 연주자의 **귀에 같은 절대 시각**에 클릭이 들린다(설계 1.4절의 근거). 클릭 프레임은
//!   출력 콜백 시각 추정(`DeviceClock`)과 장치 출력 버퍼로 "귀 도착 시각"을 맞춘다.
//! - RT 경로: 출력 콜백은 미리 계산한 48kHz 파형(`ClickRt`)과 원자값 하나(`ClickShared::next` = 다음 클릭
//!   프레임 + 강박 비트)만 쓴다. 할당·락·시스템콜·로그 없음. 워커(`BeatScheduler`)가 다음 클릭을 미리 올린다.
//! - 클릭은 장치 출력에만 섞는다. 믹스 탭(관람 방송·녹음)과 측정 자극에는 들어가지 않는다.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::config::INTERNAL_RATE;
use super::stats::AtomicF32;

/// 다음 클릭 없음.
pub const NO_CLICK: u64 = u64::MAX;
/// BPM 범위.
pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 300.0;
/// 박자(마디당 박) 범위.
pub const MAX_BEATS_PER_BAR: u32 = 16;
/// 방장이 시작 시각을 정하지 않으면 지금 + 이만큼(µs). 다른 연주자에게 시그널링이 닿을 여유.
pub const DEFAULT_START_LEAD_US: f64 = 300_000.0;
/// 클릭 파형 길이(48kHz 샘플) = 30ms.
pub const CLICK_LEN: usize = 1440;
/// 클릭 볼륨 기본값.
pub const DEFAULT_CLICK_GAIN: f32 = 0.8;
/// 올려 둔 클릭의 제 시각이 이만큼(초) 넘게 지났다고 다시 계산되면(콜백 공백 뒤) 울리지 않고 건너뛴다.
const STALE_SKIP_S: f64 = 0.005;
/// 루프백 보정값 `offsetMs`로 받을 수 있는 범위(ms). 측정 오차로 음수가 나올 수 있다.
pub const MIN_LOOPBACK_OFFSET_MS: f64 = -50.0;
pub const MAX_LOOPBACK_OFFSET_MS: f64 = 200.0;
/// 루프백에서 구한 숨은 출력 지연 상한(µs).
const MAX_MEASURED_HIDDEN_OUT_US: f64 = 100_000.0;

fn encode(frame: u64, accent: bool) -> u64 {
    (frame << 1) | accent as u64
}

fn decode(v: u64) -> (u64, bool) {
    (v >> 1, v & 1 == 1)
}

/// 클릭 파형(48kHz): 사인 버스트, 1ms 상승 + 6ms 지수 감쇠. 첫 샘플부터 0이 아니다(시작 위치 판정용).
pub fn click_wave(freq_hz: f32, amp: f32) -> Box<[f32]> {
    let r = INTERNAL_RATE as f32;
    (0..CLICK_LEN)
        .map(|n| {
            let t = (n + 1) as f32 / r;
            let attack = ((n + 1) as f32 / (0.001 * r)).min(1.0);
            let decay = (-(n as f32) / (0.006 * r)).exp();
            amp * attack * decay * (std::f32::consts::TAU * freq_hz * t).sin()
        })
        .collect()
}

/// 워커 ↔ 출력 콜백 공유 상태. 모두 원자값.
#[derive(Debug)]
pub struct ClickShared {
    /// 다음 클릭: `frame << 1 | accent`(`out_played_frames` 기준 장치 프레임). 없으면 `NO_CLICK`.
    /// 워커가 올리고(CAS), 콜백이 시작하면서 `NO_CLICK`으로 되돌린다(CAS).
    pub next: AtomicU64,
    /// 48kHz 파형을 장치 레이트로 읽는 보폭(= 48000/장치 레이트). 엔진이 장치를 연 뒤 한 번 쓴다.
    pub step: AtomicF32,
    /// 울린 클릭 수.
    pub clicks: AtomicU64,
    /// 늦게 시작한 클릭 수(파형 중간부터 울렸거나 통째로 놓침).
    pub late: AtomicU64,
    /// 콜백 공백 뒤 제 시각이 이미 지나 건너뛴 클릭 수(PR #34 M3).
    pub skipped: AtomicU64,
}

impl Default for ClickShared {
    fn default() -> Self {
        Self { next: AtomicU64::new(NO_CLICK), step: AtomicF32::new(1.0), clicks: AtomicU64::new(0), late: AtomicU64::new(0), skipped: AtomicU64::new(0) }
    }
}

/// 출력 콜백 쪽 클릭 생성기(RT). 파형은 생성 시 미리 할당한다.
pub struct ClickRt {
    sh: Arc<ClickShared>,
    accent: Box<[f32]>,
    normal: Box<[f32]>,
    /// 이번 콜백에서 시작할 클릭: (콜백 안 프레임, 강박, 파형 시작 위치).
    start: Option<(usize, bool, f32)>,
    /// 울리는 중인 클릭: (강박, 파형 위치).
    playing: Option<(bool, f32)>,
    step: f32,
}

impl ClickRt {
    pub fn new(sh: Arc<ClickShared>) -> Self {
        Self { sh, accent: click_wave(1760.0, 0.6), normal: click_wave(1100.0, 0.45), start: None, playing: None, step: 1.0 }
    }

    /// RT: 콜백 시작. `before` = 이 콜백 첫 프레임 위치, `frames` = 이 콜백 프레임 수.
    /// `audible = false`(측정 자극이 장치 출력을 대신하는 중)이면 이번 콜백에 걸린 클릭을 소비만 하고 세지 않는다(PR #34 L1).
    pub fn begin(&mut self, before: u64, frames: usize, audible: bool) {
        self.start = None;
        self.step = self.sh.step.load();
        if self.step.is_nan() || self.step <= 0.0 {
            self.step = 1.0;
        }
        let v = self.sh.next.load(Ordering::Acquire);
        if v == NO_CLICK {
            return;
        }
        let (f, accent) = decode(v);
        if f >= before + frames as u64 {
            return;
        }
        // 이번 콜백에 걸리면 가져간다. 워커가 동시에 바꿨으면(CAS 실패) 다음 콜백에서 새 값을 본다.
        if self.sh.next.compare_exchange(v, NO_CLICK, Ordering::AcqRel, Ordering::Relaxed).is_err() {
            return;
        }
        if !audible {
            self.playing = None;
            return;
        }
        if f >= before {
            self.start = Some(((f - before) as usize, accent, 0.0));
            self.sh.clicks.fetch_add(1, Ordering::Relaxed);
        } else {
            // 늦음: 파형이 아직 남아 있으면 그 위치부터(남은 부분의 시각은 정확), 아니면 버린다.
            self.sh.late.fetch_add(1, Ordering::Relaxed);
            let pos = (before - f) as f32 * self.step;
            if pos < CLICK_LEN as f32 {
                self.start = Some((0, accent, pos));
                self.sh.clicks.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// RT: 콜백 안 `i`번째 프레임의 클릭 샘플(게인 전).
    #[inline]
    pub fn sample(&mut self, i: usize) -> f32 {
        if let Some((at, accent, pos)) = self.start {
            if at == i {
                self.playing = Some((accent, pos));
                self.start = None;
            }
        }
        let Some((accent, pos)) = self.playing else {
            return 0.0;
        };
        let w = if accent { &self.accent } else { &self.normal };
        let idx = pos as usize;
        let frac = pos - idx as f32;
        let a = w.get(idx).copied().unwrap_or(0.0);
        let b = w.get(idx + 1).copied().unwrap_or(0.0);
        let next = pos + self.step;
        self.playing = (next < w.len() as f32).then_some((accent, next));
        a + (b - a) * frac
    }
}

// ============================================================================
// 계획(커맨드) ↔ 워커
// ============================================================================

/// `engine_set_metronome` 인자(설계 4절).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetronomeConfig {
    pub running: bool,
    pub bpm: f64,
    pub beats_per_bar: u32,
    /// 박 0 시각(기준 피어 시계, µs). 방장(기준 피어 없음)이 비우면 지금 + 300ms.
    #[serde(default)]
    pub start_us: Option<f64>,
    /// 기준 피어(방장 엔진의 peerId). 없으면 내 엔진이 기준(방장).
    #[serde(default)]
    pub ref_peer_id: Option<String>,
}

/// 적용된 계획(`engine_set_metronome` 반환값, 방장이 시그널링으로 알리는 값).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetronomePlan {
    pub running: bool,
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub start_us: f64,
    pub ref_peer_id: Option<String>,
}

impl Default for MetronomePlan {
    fn default() -> Self {
        Self { running: false, bpm: 120.0, beats_per_bar: 4, start_us: 0.0, ref_peer_id: None }
    }
}

impl MetronomePlan {
    /// 검증 후 계획으로. `now_us` = 내 엔진 시계.
    pub fn from_config(c: MetronomeConfig, now_us: f64) -> Result<Self, String> {
        if !c.bpm.is_finite() || !(MIN_BPM..=MAX_BPM).contains(&c.bpm) {
            return Err(format!("BPM은 {MIN_BPM}..{MAX_BPM}이어야 합니다"));
        }
        if !(1..=MAX_BEATS_PER_BAR).contains(&c.beats_per_bar) {
            return Err(format!("박자는 1..{MAX_BEATS_PER_BAR}이어야 합니다"));
        }
        let ref_peer_id = c.ref_peer_id.filter(|s| !s.is_empty());
        let start_us = match (c.start_us, &ref_peer_id) {
            (Some(s), _) if s.is_finite() && s >= 0.0 => s,
            (Some(_), _) => return Err("startUs가 잘못되었습니다".into()),
            (None, None) => now_us + DEFAULT_START_LEAD_US,
            (None, Some(_)) => return Err("기준 피어를 따를 때는 startUs가 필요합니다".into()),
        };
        Ok(Self { running: c.running, bpm: c.bpm, beats_per_bar: c.beats_per_bar, start_us, ref_peer_id })
    }

    pub fn period_us(&self) -> f64 {
        60e6 / self.bpm
    }
}

/// 메트로놈 상태(`EngineStats.metronome`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetronomeStats {
    pub running: bool,
    /// 시간축이 잡혔다(방장이거나, 기준 피어와 시계 표본이 있음). false면 클릭을 내지 않는다.
    pub synced: bool,
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub start_us: f64,
    pub ref_peer_id: Option<String>,
    /// 기준 피어 시계 − 내 시계(ms). 방장이면 0.
    pub clock_offset_ms: Option<f64>,
    /// 시계 추정에 쓴 최소 RTT(ms).
    pub clock_rtt_ms: Option<f64>,
    /// 추정 시계 드리프트(ppm).
    pub drift_ppm: Option<f64>,
    /// 마지막으로 예약한 박 번호(0부터).
    pub beat: Option<u64>,
    pub clicks: u64,
    pub late_clicks: u64,
    /// 콜백 공백 뒤 제 시각이 지나 건너뛴 클릭 수.
    pub skipped_clicks: u64,
    /// 클릭 시각에 반영한 숨은 출력 지연(ms). 출처는 `output_latency_source`.
    /// null = 드라이버 보고도 루프백 측정도 없어 보정 안 됨(cpal WASAPI 등). UI가 "루프백 측정으로 보정"을 안내할 수 있다.
    pub output_latency_ms: Option<f64>,
    /// `output_latency_ms`의 출처(드라이버 보고 / 루프백 측정). null = 보정 안 됨.
    pub output_latency_source: Option<OutputLatencySource>,
    /// 기준 피어가 Pong에 시계를 싣지 않는 옛 엔진이다(PR #34 L4). 이 계획으로는 `synced`가 영원히 false다.
    pub ref_clock_unsupported: bool,
    /// 시계 추정 직선의 무게중심에서 지금까지의 외삽 거리(ms, PR #34 L6). 기울기 오차 × 이 거리가 오프셋 오차가 된다.
    pub clock_extrapolation_ms: Option<f64>,
}

/// 숨은 출력 지연 보정의 출처.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputLatencySource {
    /// 드라이버 보고 출력 지연 − 콜백 크기(ASIO `ASIOGetLatencies`, 자체 WASAPI `GetStreamLatency`).
    Driver,
    /// 루프백 측정 보정값(`offsetMs`)에서 나눈 출력 쪽 몫(`measured_hidden_out_us`).
    Loopback,
}

/// 루프백 측정 보정값 `offset_ms`(= 실측 왕복 − 입력 버퍼 − 출력 버퍼 = 숨은 입력 + 숨은 출력)에서
/// 숨은 출력 지연(µs)을 나눈다(PR #34 M2(a)).
/// - 드라이버가 입력 쪽 지연만 알려 주면(`hidden_in_us`) 그것을 뺀 나머지.
/// - 아니면 반씩(입·출력 대칭 가정: 변환기·WASAPI 엔진 주기·USB 안전 여유는 보통 양방향에 비슷하게 있다).
///   비대칭이면 (숨은 출력 − 숨은 입력)/2만큼 남는다(설계 1.4절).
///
/// 0..100ms로 자른다.
pub fn measured_hidden_out_us(offset_ms: f64, hidden_in_us: Option<f64>) -> f64 {
    let total = offset_ms * 1000.0;
    let out = match hidden_in_us {
        Some(h) => total - h,
        None => total * 0.5,
    };
    if out.is_finite() {
        out.clamp(0.0, MAX_MEASURED_HIDDEN_OUT_US)
    } else {
        0.0
    }
}

/// 숨은 출력 지연 선택: 드라이버 보고가 있으면 그것(직접 출력 쪽 값), 없으면 루프백 측정에서 나눈 값.
pub fn choose_hidden_out(driver_us: Option<f64>, loopback_us: Option<f64>) -> Option<(f64, OutputLatencySource)> {
    driver_us
        .map(|h| (h, OutputLatencySource::Driver))
        .or_else(|| loopback_us.map(|h| (h, OutputLatencySource::Loopback)))
}

/// 커맨드 ↔ 워커 공유: 계획(세대 번호와 함께)과 워커가 쓰는 상태.
#[derive(Debug, Default)]
pub struct MetronomeCtl {
    pub plan: Mutex<(u64, MetronomePlan)>,
    pub stats: Mutex<MetronomeStats>,
    pub click: Arc<ClickShared>,
    /// 이 장치 구성의 루프백 보정값 `offsetMs`(PR #34 M2(a)). 드라이버가 출력 지연을 보고하지 않을 때 쓴다.
    pub loopback_offset_ms: Mutex<Option<f64>>,
}

impl MetronomeCtl {
    /// 루프백 보정값 설정·해제. 범위 밖이면 거부한다.
    pub fn set_loopback_offset(&self, offset_ms: Option<f64>) -> Result<(), String> {
        if let Some(o) = offset_ms {
            if !o.is_finite() || !(MIN_LOOPBACK_OFFSET_MS..=MAX_LOOPBACK_OFFSET_MS).contains(&o) {
                return Err(format!("offsetMs는 {MIN_LOOPBACK_OFFSET_MS}..{MAX_LOOPBACK_OFFSET_MS}ms여야 합니다"));
            }
        }
        *self.loopback_offset_ms.lock() = offset_ms;
        Ok(())
    }

    pub fn set_plan(&self, plan: MetronomePlan) {
        let mut p = self.plan.lock();
        p.0 += 1;
        p.1 = plan;
    }

    pub fn stats(&self) -> MetronomeStats {
        let mut s = self.stats.lock().clone();
        s.clicks = self.click.clicks.load(Ordering::Relaxed);
        s.late_clicks = self.click.late.load(Ordering::Relaxed);
        s.skipped_clicks = self.click.skipped.load(Ordering::Relaxed);
        s
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
pub fn hidden_out_latency_us(reported_frames: u32, burst_frames: u64, rate: f64) -> Option<f64> {
    if reported_frames == 0 || rate <= 0.0 {
        return None;
    }
    Some((reported_frames as f64 - burst_frames as f64).max(0.0) * 1e6 / rate)
}

impl OutClock {
    /// 장치 프레임 J가 귀에 닿는 시각(내 시계 µs). 콜백이 프레임 [b, b+n)을 쓰는 시각은 `m + (b+n)/r`이고
    /// (`m` = `DeviceClock`의 `벽시계 − 재생/레이트` 최솟값), 그 버퍼는 장치 출력 버퍼 1개(n)와
    /// 숨은 출력 지연(`extra_out_us`) 뒤에 울린다. 따라서 귀 시각 = `m + (J + 2n)/r + extra`.
    pub fn ear_time_us(&self, frame: f64) -> f64 {
        self.dev_now_us + (frame + 2.0 * self.burst as f64 - self.played as f64) * 1e6 / self.rate + self.extra_out_us
    }

    /// 귀 시각 `t_us`에 울릴 장치 프레임.
    pub fn frame_for(&self, t_us: f64) -> f64 {
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

    /// 반복마다 호출. `offset_us` = 기준 시계 − 내 시계(방장 0, 모르면 None). 반환: 올려 둔 박 번호.
    pub fn tick(&mut self, gen: u64, plan: &MetronomePlan, offset_us: Option<f64>, clock: &OutClock, sh: &ClickShared) -> Option<u64> {
        if gen != self.gen {
            self.gen = gen;
            self.withdraw(sh);
            self.last_beat = None;
        }
        let (true, Some(offset)) = (plan.running, offset_us) else {
            self.withdraw(sh);
            return None;
        };
        if clock.dev_now_us <= 0.0 || clock.rate <= 0.0 {
            return self.pending.map(|p| p.0);
        }
        let period = plan.period_us();
        let beat_frame = |k: u64| clock.frame_for(plan.start_us + k as f64 * period - offset).round();
        let guard = clock.guard();
        if let Some((k, raw)) = self.pending {
            if sh.next.load(Ordering::Acquire) != raw {
                // 콜백이 가져갔다(울림)
                self.last_beat = Some(k);
                self.pending = None;
            } else {
                // 시계 추정이 바뀌었으면 아직 멀리 있는 클릭만 다시 맞춘다(울리기 직전 값은 건드리지 않음).
                let old = decode(raw).0 as f64;
                let new = beat_frame(k);
                if !(old >= guard && new < guard && old - new > clock.rate * STALE_SKIP_S) {
                    if new != old && old >= guard && new >= guard {
                        let v = encode(new as u64, decode(raw).1);
                        if sh.next.compare_exchange(raw, v, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                            self.pending = Some((k, v));
                        }
                    }
                    return Some(k);
                }
                // 콜백 공백 뒤 시계가 다시 잡혀 보니 이 박의 제 시각이 이미 지났다(PR #34 M3):
                // 공백만큼 늦은 클릭을 내지 않고 건너뛴 뒤 다음 박을 올린다.
                self.pending = None;
                self.last_beat = Some(k);
                if sh.next.compare_exchange(raw, NO_CLICK, Ordering::AcqRel, Ordering::Relaxed).is_err() {
                    return Some(k); // 그 사이 콜백이 가져갔다(울림)
                }
                sh.skipped.fetch_add(1, Ordering::Relaxed);
            }
        }
        // 다음 박: 지난 박 다음, 그리고 지금 올려도 늦지 않은 첫 박(0 이상).
        let t_guard_ref = clock.ear_time_us(guard) + offset;
        let k_min = ((t_guard_ref - plan.start_us) / period).ceil().max(0.0) as u64;
        let mut k = self.last_beat.map_or(k_min, |l| (l + 1).max(k_min));
        let mut f = beat_frame(k);
        while f < guard {
            k += 1;
            f = beat_frame(k);
        }
        let v = encode(f as u64, k.is_multiple_of(plan.beats_per_bar as u64));
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
    fn config_validation() {
        let c = |bpm: f64, b: u32, s: Option<f64>, r: Option<&str>| MetronomeConfig {
            running: true,
            bpm,
            beats_per_bar: b,
            start_us: s,
            ref_peer_id: r.map(String::from),
        };
        assert!(MetronomePlan::from_config(c(10.0, 4, None, None), 0.0).is_err());
        assert!(MetronomePlan::from_config(c(f64::NAN, 4, None, None), 0.0).is_err());
        assert!(MetronomePlan::from_config(c(120.0, 0, None, None), 0.0).is_err());
        assert!(MetronomePlan::from_config(c(120.0, 4, None, Some("h")), 0.0).is_err());
        assert!(MetronomePlan::from_config(c(120.0, 4, Some(-1.0), None), 0.0).is_err());
        let p = MetronomePlan::from_config(c(120.0, 4, None, None), 1000.0).unwrap();
        assert_eq!(p.start_us, 1000.0 + DEFAULT_START_LEAD_US);
        let p = MetronomePlan::from_config(c(90.0, 3, Some(5.0), Some("")), 0.0).unwrap();
        assert_eq!(p.ref_peer_id, None);
    }

    #[test]
    fn rt_plays_click_at_exact_frame_and_late_start() {
        let sh = Arc::new(ClickShared::default());
        let mut rt = ClickRt::new(sh.clone());
        sh.next.store(encode(1000 + 37, true), Ordering::Release);
        rt.begin(1000, 64, true);
        let out: Vec<f32> = (0..64).map(|i| rt.sample(i)).collect();
        assert!(out[..37].iter().all(|&x| x == 0.0));
        assert!(out[37] != 0.0);
        assert_eq!(sh.next.load(Ordering::Relaxed), NO_CLICK);
        assert_eq!(sh.clicks.load(Ordering::Relaxed), 1);
        // 늦게 본 클릭: 10프레임 지난 위치부터
        let mut rt2 = ClickRt::new(sh.clone());
        sh.next.store(encode(990, false), Ordering::Release);
        rt2.begin(1000, 64, true);
        let x = rt2.sample(0);
        assert!((x - click_wave(1100.0, 0.45)[10]).abs() < 1e-6);
        assert_eq!(sh.late.load(Ordering::Relaxed), 1);
        // 파형이 끝날 만큼 늦으면 버린다
        sh.next.store(encode(0, false), Ordering::Release);
        let mut rt3 = ClickRt::new(sh.clone());
        rt3.begin(100_000, 64, true);
        assert!((0..64).all(|i| rt3.sample(i) == 0.0));
        assert_eq!(sh.clicks.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn rt_ignores_future_click() {
        let sh = Arc::new(ClickShared::default());
        let mut rt = ClickRt::new(sh.clone());
        sh.next.store(encode(5000, false), Ordering::Release);
        rt.begin(1000, 64, true);
        assert!((0..64).all(|i| rt.sample(i) == 0.0));
        assert_ne!(sh.next.load(Ordering::Relaxed), NO_CLICK);
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
    fn rt_swallows_click_during_measurement() {
        // 측정 자극이 출력을 대신하는 중에는 클릭을 소비만 하고 세지 않는다(PR #34 L1)
        let sh = Arc::new(ClickShared::default());
        let mut rt = ClickRt::new(sh.clone());
        sh.next.store(encode(1010, true), Ordering::Release);
        rt.begin(1000, 64, false);
        assert!((0..64).all(|i| rt.sample(i) == 0.0));
        assert_eq!(sh.next.load(Ordering::Relaxed), NO_CLICK);
        assert_eq!(sh.clicks.load(Ordering::Relaxed), 0);
        assert_eq!(sh.late.load(Ordering::Relaxed), 0);
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
    fn loopback_offset_splits_into_output_share() {
        // PR #34 M2(a): offset = 숨은 입력 + 숨은 출력
        assert_eq!(measured_hidden_out_us(20.0, None), 10_000.0, "입력 쪽 모르면 반");
        assert_eq!(measured_hidden_out_us(20.0, Some(4_000.0)), 16_000.0, "입력 쪽을 알면 뺀 나머지");
        assert_eq!(measured_hidden_out_us(-3.0, None), 0.0, "음수(측정 오차)는 0");
        assert_eq!(measured_hidden_out_us(20.0, Some(25_000.0)), 0.0);
        assert_eq!(measured_hidden_out_us(1000.0, None), MAX_MEASURED_HIDDEN_OUT_US);
        assert_eq!(measured_hidden_out_us(f64::NAN, None), 0.0);
        // 드라이버 보고가 우선, 없으면 루프백, 둘 다 없으면 보정 안 함
        assert_eq!(choose_hidden_out(Some(1.0), Some(2.0)), Some((1.0, OutputLatencySource::Driver)));
        assert_eq!(choose_hidden_out(None, Some(2.0)), Some((2.0, OutputLatencySource::Loopback)));
        assert_eq!(choose_hidden_out(None, None), None);
        // 커맨드 범위 검사
        let ctl = MetronomeCtl::default();
        assert!(ctl.set_loopback_offset(Some(f64::INFINITY)).is_err());
        assert!(ctl.set_loopback_offset(Some(500.0)).is_err());
        assert!(ctl.set_loopback_offset(Some(7.5)).is_ok());
        assert_eq!(*ctl.loopback_offset_ms.lock(), Some(7.5));
        assert!(ctl.set_loopback_offset(None).is_ok());
        assert_eq!(*ctl.loopback_offset_ms.lock(), None);
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
