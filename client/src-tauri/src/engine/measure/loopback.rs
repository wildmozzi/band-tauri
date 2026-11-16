//! 하드웨어 루프백 측정 작업(`engine_measure_loopback`): 카운터 원점 잡기, 자극 재생·녹음, 반복·판정. 시간 모델은
//! `measure` 모듈 문서.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ringbuf::traits::{Consumer, Observer, Producer};
use serde::Serialize;

use super::analysis::{analyze_loopback, chirp, ms, summarize, verdict_of, LoopbackRun, MeasureVerdict, CANCELLED, MAX_LOOPBACK_MS};
use crate::engine::device::StreamInfo;
use crate::engine::rt::{MeasureEnds, MeasureShared};
use crate::engine::stats::AudioCounters;

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

// ============================================================================
// 하드웨어 루프백
// ============================================================================

/// 자극: 100ms 선형 처프 200Hz → min(12kHz, 0.45·레이트), −10dBFS 부근(진폭 0.3), 5ms 페이드.
pub const STIM_SECONDS: f64 = 0.1;
pub const STIM_AMP: f32 = 0.3;
/// 자극 전에 녹음하는 잡음 구간.
const PREROLL: Duration = Duration::from_millis(60);
/// 반복 사이 쉬는 시간(잔향 소멸).
const RUN_GAP: Duration = Duration::from_millis(120);
const ACK_TIMEOUT: Duration = Duration::from_secs(2);
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
