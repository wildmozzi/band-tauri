//! 지연 측정 도구(P1-1, GOAL 1절 "측정 방법"). 설계 문서 4절 `engine_measure_loopback`·
//! `engine_set_echo_mode`·`engine_measure_echo_peer`.
//!
//! ## 하드웨어 루프백(로컬 왕복)
//! 사용자가 인터페이스 출력 → 입력을 케이블로 잇는다(또는 스피커 → 마이크). 측정 동안 출력 콜백은 믹스 대신
//! 처프 자극만 쓰고(되먹임 방지), 입력 콜백은 라우팅된 첫 입력 채널을 사전 할당 링에 녹음한다(`rt.rs`
//! `MeasureIn/MeasureOut` — RT 쪽은 링 push/pop과 원자값뿐). 이 모듈(비RT)은 녹음을 자극과 FFT 상호상관해
//! 도착 위치를 찾는다.
//!
//! 입력·출력 프레임 카운터를 같은 시간축에 놓는 방법: 측정 스레드가 카운터와 벽시계를 계속 읽어
//! `벽시계 − 카운터/레이트`의 최솟값(= 콜백 시각에 맞춘 카운터 원점)을 방향마다 잡는다. 그러면
//! - 출력 프레임 J의 쓰기 시각 = `e_out + (J + B_out)/r` (출력 카운터는 콜백 끝에 B_out만큼 오른다)
//! - 입력 프레임 I의 캡처 시각 = `e_in + I/r` (입력 카운터는 콜백 시작에 오르고, 블록 끝 = 콜백 시각)
//! - 왕복 지연 = 캡처(I) − 쓰기(J) + B_in/r.
//!
//! 마지막 항은 "입력 블록이 다 찰 때까지 기다리는 시간"이다. 그래서 결과는 소프트웨어 패스스루
//! (입력 → 바로 출력)의 아날로그 입력 → 아날로그 출력 지연과 같다. 업계 RTL 측정값과 같고, 드라이버 보고
//! (`ASIOGetLatencies` 입력 + 출력)와 직접 비교할 수 있다. 이상적인 듀플렉스 장치에서는 B_in + B_out + 변환기/
//! 드라이버 안전 여유다.
//!
//! ## 에코 피어(네트워크 포함 왕복)
//! 측정하는 쪽(A)이 피어(B)에게 Ping으로 에코를 요청하고(`net/peer.rs` `EchoState`), B의 사용자가 동의하면
//! B의 수신 스레드가 A의 Audio 패킷을 디코드 없이 그대로 A에게 되돌린다(B의 입력은 섞지 않음).
//! A는 그동안 B에게만 내 입력 대신 무음 + 클릭 스트림을 보낸다(`EchoProbe`, 다른 피어는 평소 소리를 받는다).
//! 돌아온 클릭이 A의 지터 버퍼를 지나 출력 소스 링에 들어갈 때 재생(쓰기) 예정 시각을 기록해 클릭 송신
//! 시각(프레임 시작 기준)과 뺀다:
//! `왕복 = 프레임 + 망 왕복 + B 되돌리기 처리 + A 지터 버퍼(소스 링 포함)`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ringbuf::traits::{Consumer, Observer, Producer};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use serde::Serialize;

use super::config::INTERNAL_RATE;
use super::device::StreamInfo;
use super::rt::{MeasureEnds, MeasureShared};
use super::stats::AudioCounters;
use crate::net::NetShared;

// ============================================================================
// 신호 처리
// ============================================================================

/// 선형 처프(양끝 Hann 페이드). `amp`는 최대 진폭.
pub fn chirp(rate: u32, dur_s: f64, f0: f64, f1: f64, amp: f32, fade_s: f64) -> Vec<f32> {
    let r = rate.max(1) as f64;
    let n = (dur_s * r).round().max(2.0) as usize;
    let fade = ((fade_s * r).round() as usize).min(n / 2);
    let k = (f1 - f0) / dur_s;
    (0..n)
        .map(|i| {
            let t = i as f64 / r;
            let ph = 2.0 * std::f64::consts::PI * (f0 * t + 0.5 * k * t * t);
            let w = if i < fade {
                0.5 - 0.5 * (std::f64::consts::PI * i as f64 / fade as f64).cos()
            } else if i >= n - fade {
                0.5 - 0.5 * (std::f64::consts::PI * (n - 1 - i) as f64 / fade as f64).cos()
            } else {
                1.0
            };
            (ph.sin() * w) as f32 * amp
        })
        .collect()
}

/// 상호상관 `c[l] = Σ x[n + l]·t[n]`, l = 0..=x.len() − t.len() (FFT). `x`가 `t`보다 짧으면 빈 벡터.
pub fn xcorr(x: &[f32], t: &[f32]) -> Vec<f32> {
    if t.is_empty() || x.len() < t.len() {
        return Vec::new();
    }
    let n = (x.len() + t.len()).next_power_of_two();
    let mut planner = FftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let mut a: Vec<Complex<f32>> = x.iter().map(|&v| Complex::new(v, 0.0)).collect();
    a.resize(n, Complex::new(0.0, 0.0));
    let mut b: Vec<Complex<f32>> = t.iter().map(|&v| Complex::new(v, 0.0)).collect();
    b.resize(n, Complex::new(0.0, 0.0));
    fwd.process(&mut a);
    fwd.process(&mut b);
    for (p, q) in a.iter_mut().zip(b.iter()) {
        *p *= q.conj();
    }
    inv.process(&mut a);
    let scale = 1.0 / n as f32;
    a[..=x.len() - t.len()].iter().map(|c| c.re * scale).collect()
}

/// 상관 피크 하나.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    /// `x`에서 템플릿 시작이 맞는 위치(샘플, 포물선 보간).
    pub lag: f64,
    /// 피크 |c| / 피크 주변을 뺀 상관 RMS. 무신호·잡음이면 작다.
    pub ratio: f32,
    /// 정규화 상관계수 |c| / (‖t‖·‖x 구간‖). 0..1.
    pub coeff: f32,
    /// 극성이 뒤집혀 도착했다(케이블/장치 위상 반전).
    pub inverted: bool,
}

/// 루프백 판정 문턱: 피크/잡음 비, 상관계수.
pub const MIN_RATIO: f32 = 10.0;
pub const MIN_COEFF: f32 = 0.2;

/// `c`(`xcorr` 결과)의 `[lo, hi)`에서 가장 큰 |c|를 찾는다. 잡음 RMS는 같은 범위에서 피크 앞 `guard_before`,
/// 뒤 `guard_after` 샘플을 뺀 나머지로 잰다(잔향·부엽 제외).
pub fn detect_in(c: &[f32], x: &[f32], t: &[f32], lo: usize, hi: usize, guard_before: usize, guard_after: usize) -> Option<Detection> {
    let hi = hi.min(c.len());
    if lo >= hi {
        return None;
    }
    let (k, peak) = c[lo..hi]
        .iter()
        .enumerate()
        .map(|(i, v)| (lo + i, *v))
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))?;
    let mag = peak.abs();
    // 포물선 보간(|c| 기준)
    let mut lag = k as f64;
    if k > 0 && k + 1 < c.len() {
        let (y0, y1, y2) = (c[k - 1].abs() as f64, mag as f64, c[k + 1].abs() as f64);
        let den = y0 - 2.0 * y1 + y2;
        if den.abs() > 1e-12 {
            lag += (0.5 * (y0 - y2) / den).clamp(-0.5, 0.5);
        }
    }
    let (ex_lo, ex_hi) = (k.saturating_sub(guard_before), k + guard_after);
    let (mut sum, mut cnt) = (0.0f64, 0usize);
    for (i, v) in c[lo..hi].iter().enumerate() {
        let i = lo + i;
        if i < ex_lo || i > ex_hi {
            sum += (*v as f64) * (*v as f64);
            cnt += 1;
        }
    }
    let noise = if cnt > 0 { (sum / cnt as f64).sqrt() as f32 } else { 0.0 };
    let ratio = if noise > 0.0 {
        mag / noise
    } else if mag > 1e-9 {
        f32::INFINITY
    } else {
        0.0
    };
    let tn: f32 = t.iter().map(|v| v * v).sum::<f32>().sqrt();
    let xn: f32 = x.get(k..(k + t.len()).min(x.len())).map_or(0.0, |s| s.iter().map(|v| v * v).sum::<f32>().sqrt());
    let coeff = if tn > 0.0 && xn > 0.0 { (mag / (tn * xn)).min(1.0) } else { 0.0 };
    Some(Detection { lag, ratio, coeff, inverted: peak < 0.0 })
}

/// (중앙값, 최소, 최대, 표준편차). 빈 입력이면 None.
pub fn summarize(v: &[f64]) -> Option<(f64, f64, f64, f64)> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    let median = if n % 2 == 1 { s[n / 2] } else { (s[n / 2 - 1] + s[n / 2]) / 2.0 };
    let mean = s.iter().sum::<f64>() / n as f64;
    let var = s.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
    Some((median, s[0], s[n - 1], var.sqrt()))
}

/// 측정 판정.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MeasureVerdict {
    /// 모든(과반) 반복에서 검출, 흩어짐 ≤ 1ms.
    Ok,
    /// 신호 미검출(케이블 미연결, 입력 채널 오선택, 음소거, 게인 부족, 상대 미응답).
    NoSignal,
    /// 입력이 클리핑됐다(게인/출력 볼륨 과다). 값은 참고용.
    Clipped,
    /// 검출이 과반이 안 되거나 반복 간 차이가 크다.
    Unstable,
}

/// 루프백: 반복 간 차이(최대 − 최소)가 이보다 크면 불안정.
pub const UNSTABLE_SPREAD_MS: f64 = 1.0;
/// 에코: 클릭마다 입력 버스트 위상(≤ 입력 버퍼)과 지터 버퍼 보정(데드밴드 ≤ 프레임/2)이 달라 깨끗한 회선에서도
/// 2~3ms 흩어진다. 이보다 크면 불안정.
pub const ECHO_UNSTABLE_SPREAD_MS: f64 = 5.0;

fn verdict_of(runs: usize, detected: usize, clipped: bool, spread: Option<f64>) -> MeasureVerdict {
    verdict_with(runs, detected, clipped, spread, UNSTABLE_SPREAD_MS)
}

fn verdict_with(runs: usize, detected: usize, clipped: bool, spread: Option<f64>, max_spread: f64) -> MeasureVerdict {
    if detected == 0 {
        if clipped {
            MeasureVerdict::Clipped
        } else {
            MeasureVerdict::NoSignal
        }
    } else if clipped {
        MeasureVerdict::Clipped
    } else if detected * 2 <= runs || spread.is_some_and(|s| s > max_spread) {
        MeasureVerdict::Unstable
    } else {
        MeasureVerdict::Ok
    }
}

/// 측정 스레드 대기 중 카운터·벽시계를 읽어 카운터 원점(`벽시계 − 프레임/레이트`의 최솟값)을 잡는다.
/// 카운터를 먼저 읽고 시각을 나중에 읽는다(관찰된 카운터 값보다 이른 시각을 쓰지 않게).
struct CounterClock<'a> {
    c: &'a AudioCounters,
    base: Instant,
    rate: f64,
    e_in: f64,
    e_out: f64,
}

impl<'a> CounterClock<'a> {
    fn new(c: &'a AudioCounters, rate: u32) -> Self {
        Self { c, base: Instant::now(), rate: rate.max(1) as f64, e_in: f64::INFINITY, e_out: f64::INFINITY }
    }

    fn reset(&mut self) {
        self.e_in = f64::INFINITY;
        self.e_out = f64::INFINITY;
    }

    fn poll(&mut self) {
        let i = self.c.in_captured_frames.load(Ordering::Acquire);
        let o = self.c.out_played_frames.load(Ordering::Acquire);
        let t = self.base.elapsed().as_secs_f64();
        if i > 0 {
            self.e_in = self.e_in.min(t - i as f64 / self.rate);
        }
        if o > 0 {
            self.e_out = self.e_out.min(t - o as f64 / self.rate);
        }
    }
}

const CANCELLED: &str = "측정이 취소되었습니다";

// ============================================================================
// 하드웨어 루프백
// ============================================================================

/// 자극: 100ms 선형 처프 200Hz → min(12kHz, 0.45·레이트), −10dBFS 부근(진폭 0.3), 5ms 페이드.
pub const STIM_SECONDS: f64 = 0.1;
pub const STIM_AMP: f32 = 0.3;
/// 자극 전에 녹음하는 잡음 구간.
const PREROLL: Duration = Duration::from_millis(60);
/// 이보다 긴 왕복은 찾지 않는다(장치 안전 여유 포함 넉넉히).
pub const MAX_LOOPBACK_MS: f64 = 400.0;
/// 반복 사이 쉬는 시간(잔향 소멸).
const RUN_GAP: Duration = Duration::from_millis(120);
/// 이 진폭 이상이면 클리핑으로 본다.
pub const CLIP_LEVEL: f32 = 0.99;
const ACK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopbackRun {
    /// 검출되면 왕복 지연(ms).
    pub round_trip_ms: Option<f64>,
    pub peak_ratio: f32,
    pub correlation: f32,
    pub clipped: bool,
    pub inverted: bool,
    /// 녹음 최대 진폭(0..1).
    pub input_peak: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopbackReport {
    pub verdict: MeasureVerdict,
    /// 사람이 읽을 판정 설명(한국어).
    pub message: String,
    pub runs: Vec<LoopbackRun>,
    pub detected_runs: u32,
    /// 검출된 반복의 중앙값·최소·최대·표준편차(ms).
    pub round_trip_ms: Option<f64>,
    pub min_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub std_dev_ms: Option<f64>,
    pub sample_rate: u32,
    pub input_buffer_frames: u32,
    pub output_buffer_frames: u32,
    /// 장치 버퍼만으로 본 왕복(입력 버퍼 + 출력 버퍼, ms).
    pub buffer_round_trip_ms: f64,
    /// 드라이버 보고 지연(ms). 알 수 없으면 null(cpal WASAPI, 가짜 장치).
    pub driver_input_latency_ms: Option<f64>,
    pub driver_output_latency_ms: Option<f64>,
    pub driver_round_trip_ms: Option<f64>,
    /// 측정 시점 `estLatencyMs`(입력 버퍼 + 프레임 + 출력 버퍼).
    pub est_latency_ms: f32,
    /// 보정값 = 실측 왕복 − (입력 버퍼 + 출력 버퍼). 변환기·드라이버 안전 여유 등 `estLatencyMs`에 없는 숨은 지연.
    pub offset_ms: Option<f64>,
    /// 실측 − 드라이버 보고 왕복(드라이버 보고의 오차).
    pub driver_error_ms: Option<f64>,
    /// 보정한 로컬 지연 = `estLatencyMs` + 보정값.
    pub corrected_est_latency_ms: Option<f64>,
    pub backend: String,
}

/// 루프백 측정 작업. 엔진 락 밖에서 돈다(측정 중에도 `engine_stats` 등이 막히지 않게).
pub struct LoopbackJob {
    pub sh: Arc<MeasureShared>,
    pub ends: Arc<Mutex<Option<MeasureEnds>>>,
    pub counters: Arc<AudioCounters>,
    pub info: StreamInfo,
    pub est_latency_ms: f32,
    pub backend: String,
}

fn ms(frames: u32, rate: u32) -> f64 {
    frames as f64 * 1000.0 / rate.max(1) as f64
}

impl LoopbackJob {
    /// `runs`회(1..=20) 측정. 블로킹(반복당 약 0.7초).
    pub fn run(&self, runs: u32, cancel: &AtomicBool) -> Result<LoopbackReport, String> {
        if self.info.in_rate != self.info.out_rate {
            return Err(format!(
                "입력({}Hz)과 출력({}Hz) 레이트가 달라 루프백을 잴 수 없습니다. 같은 장치(또는 같은 레이트)로 설정하세요",
                self.info.in_rate, self.info.out_rate
            ));
        }
        let mut ends = self.ends.lock().take().ok_or("이미 측정 중입니다")?;
        self.sh.out_override.store(true, Ordering::SeqCst);
        let res = self.run_inner(&mut ends, runs.clamp(1, 20), cancel);
        // 녹음을 끄고(오류로 빠져나온 경우 포함) 출력을 믹스로 되돌린다.
        self.sh.cap_on.store(false, Ordering::Release);
        self.sh.cap_req.fetch_add(1, Ordering::AcqRel);
        self.sh.out_override.store(false, Ordering::SeqCst);
        ends.capture.clear();
        *self.ends.lock() = Some(ends);
        res
    }

    fn run_inner(&self, ends: &mut MeasureEnds, runs: u32, cancel: &AtomicBool) -> Result<LoopbackReport, String> {
        let rate = self.info.out_rate;
        let r = rate as f64;
        let stim = chirp(rate, STIM_SECONDS, 200.0, (0.45 * r).min(12_000.0), STIM_AMP, 0.005);
        let mut clock = CounterClock::new(&self.counters, rate);
        let mut tmp = vec![0f32; 8192];
        let mut cap: Vec<f32> = Vec::new();
        // 이전 측정이 중간에 끝나 자극이 남아 있으면 지금(무음 대기 중) 흘려보낸다.
        if ends.stim.occupied_len() > 0 {
            self.sh.stim_req.fetch_add(1, Ordering::AcqRel);
        }
        // 출력이 자극 모드로 바뀐 뒤 모니터 소리·잔향이 사라질 때까지
        spin(&mut clock, PREROLL * 3, cancel, &mut || {
            ends.capture.clear();
        })?;
        let mut out_runs = Vec::new();
        for _ in 0..runs {
            cap.clear();
            clock.reset();
            ends.capture.clear();
            // 녹음 시작
            self.sh.cap_on.store(true, Ordering::Release);
            let req = self.sh.cap_req.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
            wait_until(&mut clock, ACK_TIMEOUT, cancel, &mut || self.sh.cap_ack.load(Ordering::Acquire) == req, &mut || {
                drain(&mut ends.capture, &mut tmp, &mut cap)
            })
            .map_err(|e| if e == CANCELLED { e } else { "입력 콜백이 돌지 않습니다(입력 장치 확인)".to_string() })?;
            let cap_start = self.sh.cap_start.load(Ordering::Relaxed);
            // 콜백 크기는 기준 위치를 잡은 시점의 값을 쓴다(cpal WASAPI 공유처럼 가변이면 끝 값과 다를 수 있다, L7b).
            let b_in = self.counters.in_cb_frames.load(Ordering::Relaxed) as f64;
            spin(&mut clock, PREROLL, cancel, &mut || drain(&mut ends.capture, &mut tmp, &mut cap))?;
            // 자극
            if ends.stim.vacant_len() < stim.len() {
                return Err("자극 링이 비어 있지 않습니다".into());
            }
            ends.stim.push_slice(&stim);
            let sreq = self.sh.stim_req.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
            wait_until(&mut clock, ACK_TIMEOUT, cancel, &mut || self.sh.stim_ack.load(Ordering::Acquire) == sreq, &mut || {
                drain(&mut ends.capture, &mut tmp, &mut cap)
            })
            .map_err(|e| if e == CANCELLED { e } else { "출력 콜백이 돌지 않습니다(출력 장치 확인)".to_string() })?;
            let stim_start = self.sh.stim_start.load(Ordering::Relaxed);
            let b_out = self.counters.out_cb_frames.load(Ordering::Relaxed) as f64;
            let listen = Duration::from_secs_f64(STIM_SECONDS + MAX_LOOPBACK_MS / 1000.0);
            spin(&mut clock, listen, cancel, &mut || drain(&mut ends.capture, &mut tmp, &mut cap))?;
            // 녹음 끝
            self.sh.cap_on.store(false, Ordering::Release);
            let req = self.sh.cap_req.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
            let _ = wait_until(&mut clock, ACK_TIMEOUT, cancel, &mut || self.sh.cap_ack.load(Ordering::Acquire) == req, &mut || {
                drain(&mut ends.capture, &mut tmp, &mut cap)
            });
            drain(&mut ends.capture, &mut tmp, &mut cap);
            out_runs.push(analyze_loopback(&cap, &stim, r, cap_start, stim_start, clock.e_in, clock.e_out, b_in, b_out));
            spin(&mut clock, RUN_GAP, cancel, &mut || drain(&mut ends.capture, &mut tmp, &mut cap))?;
        }
        Ok(self.report(out_runs))
    }

    fn report(&self, runs: Vec<LoopbackRun>) -> LoopbackReport {
        let rate = self.info.out_rate;
        let vals: Vec<f64> = runs.iter().filter_map(|r| r.round_trip_ms).collect();
        let sum = summarize(&vals);
        let clipped = runs.iter().any(|r| r.clipped);
        let spread = sum.map(|(_, lo, hi, _)| hi - lo);
        let verdict = verdict_of(runs.len(), vals.len(), clipped, spread);
        let in_buf = match self.counters.in_cb_frames.load(Ordering::Relaxed) {
            0 => self.info.buffer_frames,
            n => n,
        };
        let out_buf = match self.counters.out_cb_frames.load(Ordering::Relaxed) {
            0 => self.info.buffer_frames,
            n => n,
        };
        let buffer_rt = ms(in_buf, rate) + ms(out_buf, rate);
        let known = |f: u32| (f > 0).then(|| ms(f, rate));
        let (din, dout) = (known(self.info.in_reported_latency_frames), known(self.info.out_reported_latency_frames));
        let drt = din.zip(dout).map(|(a, b)| a + b);
        let median = sum.map(|s| s.0);
        let offset = median.map(|m| m - buffer_rt);
        let message = match verdict {
            MeasureVerdict::Ok => format!(
                "왕복 {:.2}ms(반복 {}회, 흩어짐 {:.2}ms)",
                median.unwrap_or(0.0),
                vals.len(),
                spread.unwrap_or(0.0)
            ),
            MeasureVerdict::NoSignal => "신호를 찾지 못했습니다. 출력 → 입력 케이블 연결, 입력 채널, 입력 게인을 확인하세요".into(),
            MeasureVerdict::Clipped => "입력이 클리핑됐습니다. 입력 게인이나 출력 볼륨을 낮추고 다시 재세요".into(),
            MeasureVerdict::Unstable => format!(
                "결과가 불안정합니다(검출 {}/{}회, 흩어짐 {:.2}ms). 직접 모니터를 끄고 주변 소음을 줄인 뒤 다시 재세요",
                vals.len(),
                runs.len(),
                spread.unwrap_or(0.0)
            ),
        };
        LoopbackReport {
            verdict,
            message,
            detected_runs: vals.len() as u32,
            round_trip_ms: median,
            min_ms: sum.map(|s| s.1),
            max_ms: sum.map(|s| s.2),
            std_dev_ms: sum.map(|s| s.3),
            sample_rate: rate,
            input_buffer_frames: in_buf,
            output_buffer_frames: out_buf,
            buffer_round_trip_ms: buffer_rt,
            driver_input_latency_ms: din,
            driver_output_latency_ms: dout,
            driver_round_trip_ms: drt,
            est_latency_ms: self.est_latency_ms,
            offset_ms: offset,
            driver_error_ms: median.zip(drt).map(|(m, d)| m - d),
            corrected_est_latency_ms: offset.map(|o| self.est_latency_ms as f64 + o),
            backend: self.backend.clone(),
            runs,
        }
    }
}

/// 녹음 한 번 분석. `cap[k]`는 입력 프레임 `cap_start + k`다.
#[allow(clippy::too_many_arguments)]
pub fn analyze_loopback(
    cap: &[f32],
    stim: &[f32],
    rate: f64,
    cap_start: u64,
    stim_start: u64,
    e_in: f64,
    e_out: f64,
    b_in: f64,
    b_out: f64,
) -> LoopbackRun {
    let input_peak = cap.iter().fold(0f32, |m, v| m.max(v.abs()));
    let clipped = input_peak >= CLIP_LEVEL;
    let c = xcorr(cap, stim);
    let det = detect_in(&c, cap, stim, 0, c.len(), (0.002 * rate) as usize, stim.len());
    let mut run = LoopbackRun {
        round_trip_ms: None,
        peak_ratio: det.map_or(0.0, |d| d.ratio.min(1e6)),
        correlation: det.map_or(0.0, |d| d.coeff),
        clipped,
        inverted: det.is_some_and(|d| d.inverted),
        input_peak,
    };
    let Some(d) = det.filter(|d| d.ratio >= MIN_RATIO && d.coeff >= MIN_COEFF) else {
        return run;
    };
    if !e_in.is_finite() || !e_out.is_finite() {
        return run;
    }
    let i = cap_start as f64 + d.lag;
    let capture_t = e_in + i / rate;
    let write_t = e_out + (stim_start as f64 + b_out) / rate;
    let rtl = (capture_t - write_t + b_in / rate) * 1000.0;
    if rtl > 0.0 && rtl < MAX_LOOPBACK_MS + 100.0 {
        run.round_trip_ms = Some(rtl);
    }
    run
}

fn drain(cons: &mut ringbuf::HeapCons<f32>, tmp: &mut [f32], out: &mut Vec<f32>) {
    loop {
        let n = cons.pop_slice(tmp);
        if n == 0 {
            break;
        }
        out.extend_from_slice(&tmp[..n]);
    }
}

/// `dur` 동안 카운터를 읽으며 기다린다(양보 루프 — 카운터 원점을 콜백 직후에 잡으려면 촘촘히 읽어야 한다).
fn spin(clock: &mut CounterClock<'_>, dur: Duration, cancel: &AtomicBool, each: &mut dyn FnMut()) -> Result<(), String> {
    let t0 = Instant::now();
    let mut n = 0u32;
    while t0.elapsed() < dur {
        clock.poll();
        n = n.wrapping_add(1);
        if n.is_multiple_of(64) {
            each();
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
        }
        std::thread::yield_now();
    }
    each();
    Ok(())
}

fn wait_until(
    clock: &mut CounterClock<'_>,
    timeout: Duration,
    cancel: &AtomicBool,
    done: &mut dyn FnMut() -> bool,
    each: &mut dyn FnMut(),
) -> Result<(), String> {
    let t0 = Instant::now();
    loop {
        clock.poll();
        if done() {
            return Ok(());
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        if t0.elapsed() > timeout {
            return Err("시간 초과".into());
        }
        each();
        std::thread::yield_now();
    }
}

// ============================================================================
// 에코 피어
// ============================================================================

/// 클릭 템플릿(48kHz): 5ms 처프 500Hz → 9kHz, Hann 페이드 1ms, 진폭 0.5. 코덱(Opus)을 지나도 상관 피크가 날카롭다.
pub fn click_template() -> Vec<f32> {
    chirp(INTERNAL_RATE, 0.005, 500.0, 9_000.0, 0.5, 0.001)
}

/// 클릭 간격(48k 샘플) = 300ms. 프레임 크기(120/240/480)의 배수라 클릭이 늘 프레임 시작에 놓인다.
pub const CLICK_INTERVAL: u64 = 14_400;
/// 이보다 긴 에코 왕복은 찾지 않는다(클릭 간격보다 짧아야 클릭이 섞이지 않는다).
pub const MAX_ECHO_MS: f64 = 280.0;
/// 상대가 보고하는 되돌리기 처리 지연 상한(µs). 넘으면 잘라 쓰고 불안정으로 표시한다.
pub const MAX_PEER_ECHO_DELAY_US: u32 = 50_000;
/// 에코 판정 문턱(코덱을 지나므로 루프백보다 낮게).
pub const ECHO_MIN_RATIO: f32 = 8.0;
pub const ECHO_MIN_COEFF: f32 = 0.3;
/// 에코가 시작된 뒤 지터 버퍼를 다시 채우고 통계가 안정될 때까지 기다리는 시간.
const ECHO_SETTLE: Duration = Duration::from_millis(1500);
/// 측정 끝에 상대가 에코를 멈출 때까지 기다리는 최대 시간(그동안 상대에게는 무음을 보낸다).
const ECHO_STOP_WAIT: Duration = Duration::from_secs(4);

/// 측정하는 쪽 워커가 쓰는 에코 측정 상태(엔진 ↔ 워커 공유, 비RT `Mutex`).
pub struct EchoProbe {
    pub peer_id: String,
    pub peer_ssrc: u32,
    click: Vec<f32>,
    /// 남은 클릭 수. 0이면 무음만 보낸다(측정 전후: 상대가 내 실제 소리를 되돌리지 않게).
    clicks_left: u32,
    until_next: u64,
    cursor: Option<usize>,
    /// 클릭 송신 시각(워커 epoch µs, 프레임 시작 = 송신 시각 − 프레임 길이).
    pub emitted_us: Vec<f64>,
    /// 켜져 있으면 워커가 그 피어의 소스 링에 넣는 샘플(모노)과 재생(쓰기) 예정 시각을 모은다.
    pub tap: bool,
    pub arrivals: Vec<f32>,
    pub arrival_us: Vec<f64>,
}

impl EchoProbe {
    pub fn new(peer_id: String, peer_ssrc: u32) -> Self {
        Self {
            peer_id,
            peer_ssrc,
            click: click_template(),
            clicks_left: 0,
            until_next: 0,
            cursor: None,
            emitted_us: Vec::new(),
            tap: false,
            arrivals: Vec::new(),
            arrival_us: Vec::new(),
        }
    }

    /// 다음 프레임 경계부터 `n`개 클릭을 보낸다.
    pub fn start_clicks(&mut self, n: u32) {
        self.clicks_left = n;
        self.until_next = 0;
        self.emitted_us.clear();
        self.arrivals.clear();
        self.arrival_us.clear();
        self.tap = true;
    }

    pub fn clicks_done(&self) -> bool {
        self.clicks_left == 0 && self.cursor.is_none()
    }

    /// 워커: 이 피어에게 보낼 프레임(`ich` 채널 인터리브, 48k)을 만든다. `now_us` = 송신 시각(워커 epoch).
    pub fn next_frame(&mut self, out: &mut [f32], ich: usize, now_us: f64) {
        let ich = ich.max(1);
        let f = out.len() / ich;
        out.fill(0.0);
        for pos in 0..f {
            if self.clicks_left > 0 && self.until_next == 0 && self.cursor.is_none() {
                self.cursor = Some(0);
                self.clicks_left -= 1;
                self.until_next = CLICK_INTERVAL;
                self.emitted_us.push(now_us - (f - pos) as f64 * 1e6 / INTERNAL_RATE as f64);
            }
            self.until_next = self.until_next.saturating_sub(1);
            if let Some(c) = self.cursor {
                let v = self.click.get(c).copied().unwrap_or(0.0);
                if let Some(fr) = out.get_mut(pos * ich..pos * ich + ich) {
                    fr.fill(v);
                }
                self.cursor = (c + 1 < self.click.len()).then_some(c + 1);
            }
        }
    }

    /// 워커: 소스 링에 넣는 48k 스테레오 청크와 첫 샘플의 재생(쓰기) 예정 시각.
    pub fn record(&mut self, stereo: &[f32], first_us: f64) {
        if !self.tap {
            return;
        }
        let step = 1e6 / INTERNAL_RATE as f64;
        for (m, fr) in stereo.as_chunks::<2>().0.iter().enumerate() {
            self.arrivals.push((fr[0] + fr[1]) * 0.5);
            self.arrival_us.push(first_us + m as f64 * step);
        }
    }
}

/// 돌아온 클릭 검출: 송신 시각마다 `[송신 + 1ms, 송신 + MAX_ECHO_MS]` 창에서 상관 피크를 찾는다.
/// 반환: 클릭마다 왕복(ms), 못 찾으면 None.
pub fn analyze_echo(emitted_us: &[f64], arrivals: &[f32], arrival_us: &[f64]) -> Vec<Option<f64>> {
    let tmpl = click_template();
    let c = xcorr(arrivals, &tmpl);
    let step = 1e6 / INTERNAL_RATE as f64;
    let guard = (0.002 * INTERNAL_RATE as f64) as usize;
    emitted_us
        .iter()
        .map(|&e| {
            let lo = arrival_us.partition_point(|&t| t < e + 1000.0);
            let hi = arrival_us.partition_point(|&t| t < e + MAX_ECHO_MS * 1000.0);
            let d = detect_in(&c, arrivals, &tmpl, lo, hi, guard, guard)?;
            if d.ratio < ECHO_MIN_RATIO || d.coeff < ECHO_MIN_COEFF {
                return None;
            }
            let k = d.lag.floor().max(0.0) as usize;
            let t = arrival_us.get(k)? + (d.lag - k as f64) * step;
            Some((t - e) / 1000.0)
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EchoReport {
    pub verdict: MeasureVerdict,
    pub message: String,
    pub peer_id: String,
    /// 클릭마다 왕복(ms), 못 찾으면 null.
    pub runs: Vec<Option<f64>>,
    pub detected_runs: u32,
    /// 네트워크 포함 왕복 중앙값(ms): 프레임 + 망 왕복 + 상대 되돌리기 + 내 지터 버퍼(소스 링 포함).
    pub round_trip_ms: Option<f64>,
    pub min_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub std_dev_ms: Option<f64>,
    /// 상대가 보고한 로컬 처리 지연(되돌리기, ms).
    pub peer_echo_delay_ms: f64,
    /// 편도 추정 = (왕복 − 상대 처리 지연) / 2.
    pub one_way_ms: Option<f64>,
    /// 측정 시점 핑 RTT(ms).
    pub network_rtt_ms: Option<f32>,
    /// 측정 중 내 지터 버퍼(`bufferMs`, 소스 링 포함).
    pub jitter_buffer_ms: f32,
    pub frame_ms: f32,
    /// 내 로컬 처리 지연(`estLatencyMs`)과 그 장치 버퍼 몫.
    pub local_latency_ms: f32,
    pub input_buffer_ms: f32,
    pub output_buffer_ms: f32,
    /// 입→귀 추정(GOAL 1절) = 왕복 − 상대 처리 − 핑 RTT/2 + 입력 버퍼 + 출력 버퍼
    /// = 입력 버퍼 + 프레임 + 망 편도 + 지터 버퍼 + 출력 버퍼(상대도 나와 같은 설정이라고 가정).
    pub mouth_to_ear_ms: Option<f64>,
    /// 설명되지 않는 몫 = 왕복 − (프레임 + 핑 RTT + 상대 처리 + 지터 버퍼). 0에 가까우면 통계가 맞다.
    pub residual_ms: Option<f64>,
    pub path: Option<crate::engine::stats::PathKind>,
}

/// 에코 측정 작업(측정하는 쪽). 엔진 락 밖에서 돈다.
pub struct EchoJob {
    pub probe: Arc<Mutex<Option<EchoProbe>>>,
    pub net: Arc<NetShared>,
    pub peer_id: String,
    pub peer_ssrc: u32,
    pub frame_ms: f32,
    pub local_latency_ms: f32,
    pub input_buffer_ms: f32,
    pub output_buffer_ms: f32,
}

impl EchoJob {
    /// 상대 동의를 `wait`까지 기다린 뒤 `runs`회(1..=20) 클릭을 보내 왕복을 잰다. 블로킹.
    pub fn run(&self, runs: u32, wait: Duration, cancel: &AtomicBool) -> Result<EchoReport, String> {
        if self.probe.lock().is_some() {
            return Err("이미 에코 측정 중입니다".into());
        }
        let res = self.run_inner(runs.clamp(1, 20), wait, cancel);
        // 정리: 요청을 거두고, 상대가 에코를 멈출 때까지 무음을 보낸 뒤 지터 버퍼를 상대 소리로 다시 맞춘다.
        let _ = self.net.set_echo_request(&self.peer_id, false);
        let t0 = Instant::now();
        while t0.elapsed() < ECHO_STOP_WAIT && self.net.remote_echo(&self.peer_id).is_ok_and(|(on, _)| on) {
            std::thread::sleep(Duration::from_millis(20));
        }
        *self.probe.lock() = None;
        let _ = self.net.restart_jitter(&self.peer_id);
        res
    }

    fn sleep(&self, d: Duration, cancel: &AtomicBool) -> Result<(), String> {
        let t0 = Instant::now();
        while t0.elapsed() < d {
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    fn run_inner(&self, runs: u32, wait: Duration, cancel: &AtomicBool) -> Result<EchoReport, String> {
        self.net.set_echo_request(&self.peer_id, true)?;
        let t0 = Instant::now();
        loop {
            if self.net.remote_echo(&self.peer_id)?.0 {
                break;
            }
            if t0.elapsed() > wait {
                return Err("상대가 에코 측정을 수락하지 않았습니다(시간 초과)".into());
            }
            self.sleep(Duration::from_millis(20), cancel)?;
        }
        // 동의를 받은 뒤에야 상대에게 내 소리 대신 무음(+ 클릭)을 보낸다. 상대가 허용 여부를 고르는 동안에는
        // 평소 소리를 듣는다(PR #13 L6). 동의 직후 잠깐은 내 실제 소리가 내게 되돌아오지만 나만 듣는다.
        *self.probe.lock() = Some(EchoProbe::new(self.peer_id.clone(), self.peer_ssrc));
        // 이제 상대가 보내는 것은 되돌아온 내 무음 스트림이다. 이전(상대 소리) 도착 분포를 버리고 다시 채운다.
        self.net.restart_jitter(&self.peer_id)?;
        self.sleep(ECHO_SETTLE, cancel)?;
        if let Some(p) = self.probe.lock().as_mut() {
            p.start_clicks(runs);
        }
        let listen = Duration::from_secs_f64(runs as f64 * CLICK_INTERVAL as f64 / INTERNAL_RATE as f64 + MAX_ECHO_MS / 1000.0 + 0.1);
        let t1 = Instant::now();
        let mut jitter_ms = Vec::new();
        while t1.elapsed() < listen {
            self.sleep(Duration::from_millis(100), cancel)?;
            if let Some(b) = self.net.peer_buffer_ms(&self.peer_id) {
                jitter_ms.push(b as f64);
            }
            if !self.net.remote_echo(&self.peer_id)?.0 {
                return Err("상대가 에코를 멈췄습니다".into());
            }
        }
        let (emitted, arrivals, times) = {
            let mut g = self.probe.lock();
            let p = g.as_mut().ok_or("측정 상태가 사라졌습니다")?;
            p.tap = false;
            (std::mem::take(&mut p.emitted_us), std::mem::take(&mut p.arrivals), std::mem::take(&mut p.arrival_us))
        };
        let (_, delay_us) = self.net.remote_echo(&self.peer_id)?;
        let rtt = self.net.peer_rtt_ms(&self.peer_id);
        let path = self.net.peer_path(&self.peer_id);
        let jitter = summarize(&jitter_ms).map_or(0.0, |s| s.0) as f32;
        let per = analyze_echo(&emitted, &arrivals, &times);
        Ok(self.report(per, delay_us, rtt, jitter, path))
    }

    fn report(
        &self,
        per: Vec<Option<f64>>,
        delay_us: u32,
        rtt: Option<f32>,
        jitter: f32,
        path: Option<crate::engine::stats::PathKind>,
    ) -> EchoReport {
        let vals: Vec<f64> = per.iter().flatten().copied().collect();
        let sum = summarize(&vals);
        let spread = sum.map(|(_, lo, hi, _)| hi - lo);
        let mut verdict = verdict_with(per.len(), vals.len(), false, spread, ECHO_UNSTABLE_SPREAD_MS);
        // 상대 보고값은 인증된 상대가 정한 값이다. 되돌리기는 디코드 없이 바로 하므로 수 ms를 넘을 수 없다.
        // 범위를 벗어나면 상한으로 자르고 불안정으로 표시한다(PR #13 L2).
        let peer_bogus = delay_us > MAX_PEER_ECHO_DELAY_US;
        if peer_bogus && verdict == MeasureVerdict::Ok {
            verdict = MeasureVerdict::Unstable;
        }
        let peer_ms = delay_us.min(MAX_PEER_ECHO_DELAY_US) as f64 / 1000.0;
        let median = sum.map(|s| s.0);
        let one_way = median.map(|m| (m - peer_ms) / 2.0);
        let mouth = median.zip(rtt).map(|(m, r)| m - peer_ms - r as f64 / 2.0 + (self.input_buffer_ms + self.output_buffer_ms) as f64);
        let residual = median.zip(rtt).map(|(m, r)| m - peer_ms - r as f64 - self.frame_ms as f64 - jitter as f64);
        let message = match verdict {
            _ if peer_bogus => format!(
                "상대가 보고한 처리 지연({:.1}ms)이 비정상이라 {:.0}ms로 잘랐습니다. 상대 앱 버전을 확인하세요",
                delay_us as f64 / 1000.0,
                MAX_PEER_ECHO_DELAY_US as f64 / 1000.0
            ),
            // 에코는 클리핑 판정을 하지 않는다(verdict_with에 clipped=false).
            MeasureVerdict::Ok | MeasureVerdict::Clipped => format!(
                "네트워크 포함 왕복 {:.1}ms, 편도 추정 {:.1}ms(클릭 {}/{}개)",
                median.unwrap_or(0.0),
                one_way.unwrap_or(0.0),
                vals.len(),
                per.len()
            ),
            MeasureVerdict::NoSignal => "돌아온 클릭을 찾지 못했습니다. 상대 연결 상태(손실)를 확인하고 다시 재세요".into(),
            MeasureVerdict::Unstable => format!(
                "결과가 불안정합니다(검출 {}/{}개, 흩어짐 {:.1}ms). 회선이 불안정할 수 있습니다",
                vals.len(),
                per.len(),
                spread.unwrap_or(0.0)
            ),
        };
        EchoReport {
            verdict,
            message,
            peer_id: self.peer_id.clone(),
            detected_runs: vals.len() as u32,
            round_trip_ms: median,
            min_ms: sum.map(|s| s.1),
            max_ms: sum.map(|s| s.2),
            std_dev_ms: sum.map(|s| s.3),
            peer_echo_delay_ms: peer_ms,
            one_way_ms: one_way,
            network_rtt_ms: rtt,
            jitter_buffer_ms: jitter,
            frame_ms: self.frame_ms,
            local_latency_ms: self.local_latency_ms,
            input_buffer_ms: self.input_buffer_ms,
            output_buffer_ms: self.output_buffer_ms,
            mouth_to_ear_ms: mouth,
            residual_ms: residual,
            path,
            runs: per,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xcorr_finds_known_offset_with_subsample_precision() {
        let stim = chirp(48_000, 0.1, 200.0, 12_000.0, 0.3, 0.005);
        let mut x = vec![0f32; 20_000];
        for (i, v) in stim.iter().enumerate() {
            x[1234 + i] = *v * 0.5;
        }
        let c = xcorr(&x, &stim);
        let d = detect_in(&c, &x, &stim, 0, c.len(), 96, stim.len()).unwrap();
        assert!((d.lag - 1234.0).abs() < 0.05, "{d:?}");
        assert!(d.ratio > 100.0 && d.coeff > 0.99 && !d.inverted, "{d:?}");
        // 극성 반전
        let neg: Vec<f32> = x.iter().map(|v| -v).collect();
        let c = xcorr(&neg, &stim);
        let d = detect_in(&c, &neg, &stim, 0, c.len(), 96, stim.len()).unwrap();
        assert!(d.inverted && (d.lag - 1234.0).abs() < 0.05);
    }

    #[test]
    fn silence_and_noise_are_not_detected() {
        let stim = chirp(48_000, 0.1, 200.0, 12_000.0, 0.3, 0.005);
        let zeros = vec![0f32; 30_000];
        let run = analyze_loopback(&zeros, &stim, 48_000.0, 0, 0, 0.0, 0.0, 128.0, 128.0);
        assert!(run.round_trip_ms.is_none() && !run.clipped, "{run:?}");
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let noise: Vec<f32> = (0..30_000).map(|_| rng.gen_range(-0.3f32..0.3)).collect();
        let run = analyze_loopback(&noise, &stim, 48_000.0, 0, 0, 0.0, 0.0, 128.0, 128.0);
        assert!(run.round_trip_ms.is_none(), "잡음만으로 검출하면 안 됨: {run:?}");
        assert!(run.peak_ratio < MIN_RATIO || run.correlation < MIN_COEFF);
    }

    #[test]
    fn loopback_math_matches_time_model() {
        // 입력·출력 원점이 같고(e_in = e_out + B_out/r 관계를 직접 만든다) 자극을 7.5ms 뒤 캡처했다.
        let r = 48_000.0;
        let stim = chirp(48_000, 0.1, 200.0, 12_000.0, 0.3, 0.005);
        let mut cap = vec![0f32; 30_000];
        // 쓰기 시각(J=1000) = e_out + (1000 + 128)/r. 캡처 위치 I = cap_start(500) + k.
        // e_in = 0, e_out = 0 → write = 1128/r. 캡처 시각 = (500 + k)/r = write + 7.5ms → k = 1128 + 360 − 500
        let k = 1128 + 360 - 500;
        for (i, v) in stim.iter().enumerate() {
            cap[k + i] = *v;
        }
        let run = analyze_loopback(&cap, &stim, r, 500, 1000, 0.0, 0.0, 128.0, 128.0);
        let rt = run.round_trip_ms.unwrap();
        assert!((rt - (7.5 + 128.0 / 48.0)).abs() < 0.01, "{rt}");
        // 클리핑 판정
        let clipped: Vec<f32> = cap.iter().map(|v| (v * 4.0).clamp(-1.0, 1.0)).collect();
        let run = analyze_loopback(&clipped, &stim, r, 500, 1000, 0.0, 0.0, 128.0, 128.0);
        assert!(run.clipped);
    }

    #[test]
    fn verdicts() {
        assert_eq!(verdict_of(5, 0, false, None), MeasureVerdict::NoSignal);
        assert_eq!(verdict_of(5, 0, true, None), MeasureVerdict::Clipped);
        assert_eq!(verdict_of(5, 5, false, Some(0.2)), MeasureVerdict::Ok);
        assert_eq!(verdict_of(5, 5, false, Some(1.5)), MeasureVerdict::Unstable);
        assert_eq!(verdict_of(6, 3, false, Some(0.1)), MeasureVerdict::Unstable);
        assert_eq!(summarize(&[3.0, 1.0, 2.0]).unwrap().0, 2.0);
        assert_eq!(summarize(&[1.0, 2.0, 3.0, 4.0]).unwrap().0, 2.5);
    }

    #[test]
    fn echo_probe_emits_frame_aligned_clicks_and_detects_them() {
        let mut p = EchoProbe::new("b".into(), 1);
        p.start_clicks(3);
        let f = 120;
        let mut frame = vec![0f32; f];
        let mut stream = Vec::new();
        let mut now = 0.0;
        for _ in 0..(3 * CLICK_INTERVAL as usize / f + 10) {
            now += f as f64 * 1e6 / 48_000.0;
            p.next_frame(&mut frame, 1, now);
            stream.extend_from_slice(&frame);
        }
        assert_eq!(p.emitted_us.len(), 3);
        assert!(p.clicks_done());
        // 첫 클릭은 첫 프레임 시작(송신 시각 − 프레임 길이 = 0)
        assert!(p.emitted_us[0].abs() < 1e-6, "{:?}", p.emitted_us);
        assert!((p.emitted_us[1] - 300_000.0).abs() < 1e-6);
        // 23.4ms 늦게 재생되는 스트림으로 되돌아왔다고 하자.
        p.tap = true;
        let delay_us = 23_400.0;
        let stereo: Vec<f32> = stream.iter().flat_map(|&v| [v, v]).collect();
        for (n, ch) in stereo.chunks(128).enumerate() {
            p.record(ch, delay_us + (n * 64) as f64 * 1e6 / 48_000.0);
        }
        let per = analyze_echo(&p.emitted_us, &p.arrivals, &p.arrival_us);
        assert_eq!(per.len(), 3);
        for v in per {
            let v = v.expect("검출");
            assert!((v - 23.4).abs() < 0.05, "{v}");
        }
    }
}
