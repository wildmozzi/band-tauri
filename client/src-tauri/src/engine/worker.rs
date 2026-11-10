//! 엔진 워커(비RT 스레드 1개, 설계 문서 1.1절).
//!
//! - 입력 링 → (리샘플) → 프레임 단위 인코드 → 피어별 암호화/송신
//! - 출력: 출력 콜백이 소비한 만큼 `OUT_CHUNK` 단위로 지터 버퍼/로컬 FIFO에서 꺼내 믹스 →
//!   (리샘플) → 출력 링. 출력 생산 단위는 네트워크 프레임과 무관하다(리뷰 M1).
//! - 한 번의 리필(버스트)이 끝나면 지터 버퍼와 로컬 FIFO에 `end_burst`를 알린다. 이때 쓰는
//!   시각은 워커 기상 시각이 아니라 `DeviceClock`이 추정한 출력 콜백 시각이다.
//!
//! 출력 링 수위는 "워커가 넣은 누적 샘플 − 출력 콜백이 소비한 누적 샘플(원자 카운터)"이다.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ringbuf::traits::{Consumer, Producer};
use ringbuf::{HeapCons, HeapProd};

use super::codec::Encoder;
use super::config::{frame_code_from_samples, EngineConfig, INTERNAL_RATE, MAX_PACKET_BYTES, MAX_PAYLOAD_BYTES};
use super::device::StreamInfo;
use super::jitter::shift_one;
use super::mixer::{self, MixControls};
use super::resample::StreamResampler;
use super::stats::AudioCounters;
use crate::net::packet::{FLAG_OPUS, FLAG_STEREO};
use crate::net::NetShared;

/// 워커 폴링 주기. Windows에서도 std::thread::sleep은 고해상도 타이머를 쓴다.
const POLL: Duration = Duration::from_micros(500);
/// 출력 생산 단위(48kHz 프레임). 1.33ms.
pub const OUT_CHUNK: usize = 64;

/// 믹스 탭(관람자 브릿지용) 공유 지점. 워커가 48k 스테레오 믹스를 밀어 넣는다.
#[derive(Default)]
pub struct MixTap {
    pub prod: Mutex<Option<HeapProd<f32>>>,
}

/// 워커 → 엔진 통계 공유값.
#[derive(Debug, Default)]
pub struct WorkerStats {
    /// 로컬 경로 점유량 시간평균(µs): 입력 리샘플러 + 프레임 누적 + 로컬 FIFO + 출력 링.
    pub occupancy_us: AtomicU64,
    pub local_corrections: AtomicU64,
    pub local_underruns: AtomicU64,
}

// ============================================================================
// 장치 클럭 추정
// ============================================================================

/// 출력 콜백 시각 추정기.
///
/// 워커가 본 `wall − consumed/rate`의 최솟값이 "콜백 직후에 관측한" 오프셋이다(늦게 깨어날수록
/// 값이 커지므로 최솟값이 실제 콜백 시각에 가장 가깝다). 장치 클럭과 벽시계의 드리프트를 따라가도록
/// 최솟값에 시간당 `LEAK_PPM`만큼 상향 누설을 준다.
pub struct DeviceClock {
    rate: f64,
    min_offset_us: Option<f64>,
    last_wall_us: f64,
}

/// 상향 누설률. 장치-벽시계 드리프트(보통 ±100ppm)보다 커야 하고, 작을수록 바이어스가 작다.
const LEAK_PPM: f64 = 500.0;

impl DeviceClock {
    pub fn new(rate: u32) -> Self {
        Self { rate: rate.max(1) as f64, min_offset_us: None, last_wall_us: 0.0 }
    }

    /// `consumed_frames`: 출력 콜백이 지금까지 소비한 프레임 수. 반환: 마지막 콜백 시각 추정(µs).
    pub fn update(&mut self, wall_us: u64, consumed_frames: u64) -> u64 {
        let wall = wall_us as f64;
        if consumed_frames == 0 {
            self.last_wall_us = wall;
            return wall_us;
        }
        let dev_t = consumed_frames as f64 * 1e6 / self.rate;
        let off = wall - dev_t;
        let dt = (wall - self.last_wall_us).max(0.0);
        self.last_wall_us = wall;
        let m = match self.min_offset_us {
            None => off,
            Some(m) => (m + dt * LEAK_PPM * 1e-6).min(off),
        };
        self.min_offset_us = Some(m);
        (dev_t + m).max(0.0) as u64
    }
}

// ============================================================================
// 로컬 입력 FIFO (모니터 + 믹스 탭)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalMode {
    Idle,
    Drop,
    Insert,
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn lcm(a: usize, b: usize) -> usize {
    if a == 0 || b == 0 { a.max(b) } else { a / gcd(a, b) * b }
}

/// 입력 클럭으로 들어오고 출력 클럭으로 나가는 로컬 FIFO(48kHz 스테레오).
///
/// 크기 산정(리뷰 H1): 입력 콜백 하나(48k 환산 `in_burst`)는 프레임 누적을 거쳐 한 번에
/// 최대 `release = ceil(in_burst / F) × F`를 내놓는다(패턴은 lcm(in_burst, F)마다 반복).
/// 출력은 콜백마다 `out_burst`를 꺼낸다. 하드 상한은 이 둘과 여유를 더한 값이며, 정상
/// 상태에서는 닿지 않는다.
///
/// 수위 제어(리뷰 M5): 출력 버스트 직후 점유량은 입·출력 위상에 따라 [C − release, C]
/// 범위를 오간다(입력이 release 단위 계단이므로). 드리프트가 있으면 위상이 미끄러져 모든 값이
/// 나오므로, 최솟값이 아니라 lcm(in_burst, out_burst) 주기를 덮는 창의 **평균**을 제어한다.
/// 목표 평균 = release/2 + 2청크 → 최악 위상의 최솟값도 약 2청크 여유. 목표보다 크면 1샘플씩
/// 버리고 작으면 넣는다(히스테리시스, 10ms당 최대 1샘플, 청크 전체 램프).
pub struct LocalFifo {
    buf: VecDeque<f32>,
    playing: bool,
    priming: bool,
    target_mean: f64,
    start_level: usize,
    cap: usize,
    window_bursts: usize,
    bursts: usize,
    occ_sum: f64,
    trough: usize,
    margin: usize,
    release: usize,
    budget: usize,
    window_budget: usize,
    mode: LocalMode,
    since_corr: usize,
    take_buf: Vec<f32>,
    pub corrections: u64,
    pub underruns: u64,
    pub overflows: u64,
    configured: (usize, usize, usize),
}

const LOCAL_MARGIN: usize = 2 * OUT_CHUNK;
const LOCAL_CORR_INTERVAL: usize = 480;
/// 로컬 경로 최대 보정률(ppm). 입출력 장치 간 클럭 차(보통 ±100ppm)보다 넉넉하다.
const LOCAL_MAX_PPM: f64 = 500.0;

impl LocalFifo {
    pub fn new() -> Self {
        let mut f = Self {
            buf: VecDeque::with_capacity(48_000 * 2),
            playing: false,
            priming: false,
            target_mean: 0.0,
            start_level: 0,
            cap: 0,
            window_bursts: 1,
            bursts: 0,
            occ_sum: 0.0,
            trough: usize::MAX,
            margin: LOCAL_MARGIN,
            release: 0,
            budget: 1,
            window_budget: 1,
            mode: LocalMode::Idle,
            since_corr: 0,
            take_buf: vec![0.0; (OUT_CHUNK + 1) * 2],
            corrections: 0,
            underruns: 0,
            overflows: 0,
            configured: (0, 0, 0),
        };
        f.configure(OUT_CHUNK, OUT_CHUNK, 240);
        f
    }

    /// 버스트 크기(48k 프레임 환산)와 네트워크 프레임으로 상한/창을 다시 계산한다.
    pub fn configure(&mut self, in_burst: usize, out_burst: usize, frame: usize) {
        let key = (in_burst.max(1), out_burst.max(1), frame.max(1));
        if key == self.configured {
            return;
        }
        self.configured = key;
        let (in_burst, out_burst, frame) = key;
        let release = in_burst.div_ceil(frame) * frame;
        self.release = release;
        self.margin = LOCAL_MARGIN;
        self.apply_margin();
        // 평균 창: 입력·프레임·출력 패턴 주기 lcm(lcm(in_burst, F), out_burst)의 정수배, 최소 100ms
        // 주기가 0.5초를 넘는(서로소에 가까운) 조합은 0.5초 창으로 자른다. 첫 창은 무음 priming이므로
        // 시작 무음도 최대 0.5초다.
        let pattern_samples = lcm(lcm(in_burst, frame), out_burst);
        let pattern = (pattern_samples / out_burst).max(1);
        let reps = (4800usize).div_ceil(pattern * out_burst).max(1);
        let cap = 24_000usize.div_ceil(out_burst).max(1);
        self.window_bursts = (pattern * reps).min(cap).max(1);
        // 창 하나에서 허용하는 보정 수 = 창 길이 × LOCAL_MAX_PPM (판단 주기가 길어도 과보정하지 않게)
        let window_samples = (self.window_bursts * out_burst) as f64;
        self.window_budget = ((window_samples * LOCAL_MAX_PPM * 1e-6).ceil() as usize).max(1);
        self.budget = self.window_budget;
    }

    /// 여유(margin)에서 목표·시작 수위·상한을 계산한다.
    fn apply_margin(&mut self) {
        let (in_burst, out_burst, frame) = self.configured;
        // 누적 입력 계단은 직선보다 최대 in_burst + F − 1만큼 뒤처진다(위상이 미끄러지면 평균은 그 절반).
        // 최악 위상에서도 margin이 남도록 평균 목표를 잡는다.
        self.target_mean = (in_burst + frame) as f64 / 2.0 + self.margin as f64;
        // 첫 버스트 직후 점유량이 결손 0일 때의 수위 C = target_mean + (in_burst+F)/2가 되게 시작한다.
        self.start_level = out_burst + in_burst + frame + self.margin;
        self.cap = self.release + out_burst + frame + self.start_level;
    }

    /// 언더런: 입·출력 위상이 고정된 채 콜백 순서가 뒤집히면 입력 방출 하나만큼 패인다.
    /// 여유를 방출 하나만큼 늘린다(최대 기본 + 방출 2개).
    fn grow_margin(&mut self) {
        let max = LOCAL_MARGIN + 2 * self.release;
        self.margin = (self.margin + self.release).min(max);
        self.apply_margin();
    }

    pub fn cap_frames(&self) -> usize {
        self.cap
    }

    pub fn margin(&self) -> usize {
        self.margin
    }

    pub fn frames(&self) -> usize {
        self.buf.len() / 2
    }

    pub fn push(&mut self, stereo: &[f32]) {
        self.buf.extend(stereo.iter().copied());
        let len = self.buf.len() / 2;
        if len > self.cap {
            let excess = len - self.cap;
            self.buf.drain(..excess * 2);
            self.overflows += 1;
        }
    }

    /// `out.len()/2` 프레임을 채운다.
    pub fn pull(&mut self, out: &mut [f32]) {
        let n = out.len() / 2;
        if !self.playing {
            if self.frames() >= self.start_level {
                // 시작 전에는 무음이므로 초과분(가장 오래된 샘플)을 버려 정확히 start_level에서 시작한다.
                let excess = self.frames() - self.start_level;
                self.buf.drain(..excess * 2);
                self.playing = true;
                // 첫 창은 무음으로 흘려 보내며 실제 평균 수위를 잰다(priming). 창이 끝나면
                // 오차만큼 한 번에 맞추고 들리게 시작한다 → 시작 위상과 무관하게 목표에서 출발.
                self.priming = true;
                self.occ_sum = 0.0;
                self.trough = usize::MAX;
                self.bursts = 0;
            } else {
                out.fill(0.0);
                return;
            }
        }
        if self.priming {
            let k = self.frames().min(n);
            self.buf.drain(..k * 2);
            out.fill(0.0);
            if k < n {
                self.underruns += 1;
                self.playing = false;
                self.priming = false;
                self.grow_margin();
            }
            return;
        }
        let avail = self.frames();
        self.since_corr += n;
        let corr_due = self.since_corr >= LOCAL_CORR_INTERVAL && self.budget > 0 && n > 2 && n <= OUT_CHUNK;
        if corr_due && self.mode != LocalMode::Idle {
            self.budget -= 1;
        }
        if corr_due && self.mode == LocalMode::Drop && avail > n {
            let take = n + 1;
            for (d, s) in self.take_buf[..take * 2].iter_mut().zip(self.buf.drain(..take * 2)) {
                *d = s;
            }
            shift_one(&self.take_buf[..take * 2], 2, take, out, true);
            self.since_corr = 0;
            self.corrections += 1;
            return;
        }
        if corr_due && self.mode == LocalMode::Insert && avail >= n - 1 {
            let take = n - 1;
            for (d, s) in self.take_buf[..take * 2].iter_mut().zip(self.buf.drain(..take * 2)) {
                *d = s;
            }
            shift_one(&self.take_buf[..take * 2], 2, take, out, false);
            self.since_corr = 0;
            self.corrections += 1;
            return;
        }
        let k = avail.min(n);
        for (o, s) in out[..k * 2].iter_mut().zip(self.buf.drain(..k * 2)) {
            *o = s;
        }
        if k < n {
            // 언더런: 남은 꼬리를 짧게 페이드아웃해 클릭을 줄이고 재버퍼링
            let fade = k.min(32);
            for i in 0..fade {
                let g = (fade - i) as f32 / (fade + 1) as f32;
                let idx = (k - fade + i) * 2;
                out[idx] *= g;
                out[idx + 1] *= g;
            }
            out[k * 2..].fill(0.0);
            self.underruns += 1;
            self.playing = false;
            self.mode = LocalMode::Idle;
            self.grow_margin();
        }
    }

    /// 출력 버스트 종료 시 호출.
    pub fn end_burst(&mut self) {
        if !self.playing {
            return;
        }
        let occ = self.frames();
        self.occ_sum += occ as f64;
        self.trough = self.trough.min(occ);
        self.bursts += 1;
        if self.bursts < self.window_bursts {
            return;
        }
        // 위상이 미끄러질 때는 평균이 매끄러운 제어량이고, 위상이 고정되면 골(최솟값)이 안전 기준이다.
        // 둘 중 더 보수적인(작은) 오차를 쓴다: 골이 여유보다 낮으면 무조건 채우는 쪽으로.
        let err_mean = self.occ_sum / self.bursts as f64 - self.target_mean;
        let err_trough = self.trough as f64 - self.margin as f64;
        let err = err_mean.min(err_trough);
        self.bursts = 0;
        self.occ_sum = 0.0;
        self.trough = usize::MAX;
        if self.priming {
            // 아직 들리기 전이므로 오차를 한 번에 맞춘다(앞쪽을 버리거나 무음을 덧댐).
            let adj = err.round() as i64;
            if adj > 0 {
                let k = (adj as usize).min(self.frames());
                self.buf.drain(..k * 2);
            } else {
                for _ in 0..(-adj) as usize {
                    self.buf.push_front(0.0);
                    self.buf.push_front(0.0);
                }
            }
            self.priming = false;
            self.mode = LocalMode::Idle;
            self.budget = self.window_budget;
            return;
        }
        self.mode = match self.mode {
            LocalMode::Idle if err > 48.0 => LocalMode::Drop,
            LocalMode::Idle if err < -48.0 => LocalMode::Insert,
            LocalMode::Drop if err <= 8.0 => LocalMode::Idle,
            LocalMode::Insert if err >= -8.0 => LocalMode::Idle,
            m => m,
        };
        // 비례 제어: 다음 창에서 오차만큼만(상한 LOCAL_MAX_PPM) 보정한다. 판단 주기가 길어도
        // 한 창 동안 과보정해 반대로 넘어가지 않는다.
        self.budget = (err.abs().round() as usize).clamp(1, self.window_budget);
    }
}

impl Default for LocalFifo {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 워커
// ============================================================================

pub struct WorkerParams {
    pub cfg: EngineConfig,
    pub info: StreamInfo,
    pub in_cons: HeapCons<f32>,
    pub out_prod: HeapProd<f32>,
    pub counters: Arc<AudioCounters>,
    pub controls: Arc<MixControls>,
    pub net: Option<Arc<NetShared>>,
    pub tap: Arc<MixTap>,
    pub stop: Arc<AtomicBool>,
    pub stats: Arc<WorkerStats>,
    /// 지터 버퍼 수신 시각과 같은 기준(µs)의 시계.
    pub epoch: Instant,
}

pub struct Worker {
    p: WorkerParams,
    frame: usize,
    ich: usize,
    in_rs: StreamResampler,
    out_rs: StreamResampler,
    encoder: Option<Encoder>,
    flags: u8,
    frame_code: u8,
    ts: u32,
    produced_out: u64,
    clock: DeviceClock,
    local: LocalFifo,
    in_tmp: Vec<f32>,
    frame_buf: Vec<f32>,
    stereo_buf: Vec<f32>,
    payload: Vec<u8>,
    pkt: Vec<u8>,
    local_buf: Vec<f32>,
    bus: Vec<f32>,
    tap_bus: Vec<f32>,
    peer_buf: Vec<f32>,
    out_tmp: Vec<f32>,
    reported_errors: u32,
    encode_errors: u64,
    occupancy_ewma_us: f64,
    safety: u64,
    seen_xruns: u64,
    last_xrun: Duration,
}

impl Worker {
    /// 리샘플러/인코더 생성. 실패하면 엔진 시작을 실패시킨다(리뷰 M10).
    pub fn new(p: WorkerParams) -> Result<Self, String> {
        let frame = p.cfg.frame_samples();
        let ich = p.cfg.channels as usize;
        let in_rs = StreamResampler::new(p.info.in_rate, INTERNAL_RATE, ich)?;
        let out_rs = StreamResampler::new(INTERNAL_RATE, p.info.out_rate, 2)?;
        let encoder = if p.net.is_some() {
            Some(Encoder::new(p.cfg.codec, ich, p.cfg.opus_bitrate())?)
        } else {
            None
        };
        let mut flags = 0;
        if encoder.as_ref().is_some_and(|e| e.is_opus()) {
            flags |= FLAG_OPUS;
        }
        if ich == 2 {
            flags |= FLAG_STEREO;
        }
        let frame_code = frame_code_from_samples(frame).ok_or("잘못된 프레임 크기")?;
        let clock = DeviceClock::new(p.info.out_rate);
        Ok(Self {
            frame,
            ich,
            in_rs,
            out_rs,
            encoder,
            flags,
            frame_code,
            ts: rand::random(),
            produced_out: 0,
            clock,
            local: LocalFifo::new(),
            in_tmp: vec![0.0; 4096],
            frame_buf: vec![0.0; frame * ich],
            stereo_buf: vec![0.0; frame * 2],
            payload: vec![0u8; MAX_PAYLOAD_BYTES],
            pkt: vec![0u8; MAX_PACKET_BYTES],
            local_buf: vec![0.0; OUT_CHUNK * 2],
            bus: vec![0.0; OUT_CHUNK * 2],
            tap_bus: vec![0.0; OUT_CHUNK * 2],
            peer_buf: vec![0.0; OUT_CHUNK * 2],
            out_tmp: vec![0.0; 8192],
            reported_errors: 0,
            encode_errors: 0,
            occupancy_ewma_us: 0.0,
            safety: 0,
            seen_xruns: 0,
            last_xrun: Duration::ZERO,
            p,
        })
    }

    pub fn run(mut self) {
        let _prio = super::prio::promote_current_thread();
        log::info!("워커 시작: frame {} ch {}", self.frame, self.ich);
        while !self.p.stop.load(Ordering::Relaxed) {
            self.reconfigure_local();
            self.pump_input();
            self.pump_output();
            self.track_occupancy();
            let errs = self.p.counters.stream_errors.load(Ordering::Relaxed);
            if errs != self.reported_errors {
                log::warn!("오디오 스트림 에러 누적 {}회(장치 분리 등)", errs);
                self.reported_errors = errs;
            }
            std::thread::sleep(POLL);
        }
        log::info!("워커 종료");
    }

    fn to48(frames: u32, rate: u32) -> usize {
        (frames as u64 * INTERNAL_RATE as u64).div_ceil(rate.max(1) as u64) as usize
    }

    fn in_burst48(&self) -> usize {
        let n = match self.p.counters.in_cb_frames.load(Ordering::Relaxed) {
            0 => self.p.info.buffer_frames,
            n => n,
        };
        Self::to48(n, self.p.info.in_rate)
    }

    fn out_burst_dev(&self) -> u64 {
        match self.p.counters.out_cb_frames.load(Ordering::Relaxed) {
            0 => self.p.info.buffer_frames as u64,
            n => n as u64,
        }
    }

    fn reconfigure_local(&mut self) {
        let out48 = Self::to48(self.out_burst_dev() as u32, self.p.info.out_rate);
        self.local.configure(self.in_burst48(), out48, self.frame);
    }

    fn pump_input(&mut self) {
        loop {
            let n = self.p.in_cons.pop_slice(&mut self.in_tmp);
            if n == 0 {
                break;
            }
            let n = n - n % self.ich;
            self.in_rs.push(&self.in_tmp[..n]);
        }
        while self.in_rs.available() >= self.frame {
            let mut fb = std::mem::take(&mut self.frame_buf);
            self.in_rs.pop(&mut fb);
            self.on_capture_frame(&mut fb);
            self.frame_buf = fb;
        }
    }

    fn on_capture_frame(&mut self, frame: &mut [f32]) {
        // 입력 뮤트: 송신/모니터/탭 모두 무음. 패킷은 계속 보내 수신측 타이밍을 유지한다.
        if self.p.controls.input_muted.load(Ordering::Relaxed) {
            frame.fill(0.0);
        }
        if let (Some(enc), Some(net)) = (self.encoder.as_mut(), self.p.net.as_ref()) {
            match enc.encode(frame, &mut self.payload) {
                Ok(n) => net.send_audio(&self.payload[..n], self.flags, self.frame_code, self.ts, &mut self.pkt),
                Err(e) => {
                    // 레이트 제한 로그(리뷰 L10)
                    if self.encode_errors.is_multiple_of(1000) {
                        log::warn!("인코드 실패({}회째): {e}", self.encode_errors + 1);
                    }
                    self.encode_errors += 1;
                }
            }
        }
        self.ts = self.ts.wrapping_add(self.frame as u32);
        if self.ich == 2 {
            self.local.push(frame);
        } else {
            for (d, &s) in self.stereo_buf.as_chunks_mut::<2>().0.iter_mut().zip(frame.iter()) {
                d[0] = s;
                d[1] = s;
            }
            let n = frame.len() * 2;
            let sb = std::mem::take(&mut self.stereo_buf);
            self.local.push(&sb[..n]);
            self.stereo_buf = sb;
        }
    }

    fn ring_fill(&self) -> u64 {
        let consumed = self.p.counters.out_consumed.load(Ordering::Relaxed);
        self.produced_out.saturating_sub(consumed)
    }

    /// xrun이 나면 출력 링 여유를 청크 하나씩 늘리고(최대 장치 버퍼 1개), 10초 동안 없으면 줄인다.
    /// 콜백 간격이 들쭉날쭉하거나 워커 스케줄링이 늦는 환경(느린 CI 러너 포함)에 적응한다.
    fn adapt_safety(&mut self, chunk_dev: u64) {
        let xruns = self.p.counters.xruns.load(Ordering::Relaxed);
        let now = self.p.epoch.elapsed();
        if xruns > self.seen_xruns {
            self.seen_xruns = xruns;
            self.safety = (self.safety + chunk_dev).min(self.out_burst_dev());
            self.last_xrun = now;
        } else if self.safety > 0 && now.saturating_sub(self.last_xrun) > Duration::from_secs(10) {
            self.safety = self.safety.saturating_sub(chunk_dev);
            self.last_xrun = now;
        }
    }

    fn pump_output(&mut self) {
        // 목표 수위 = 장치 출력 버퍼 1개 + 출력 청크 1개 + 적응 여유 (장치 레이트, 스테레오 샘플)
        let chunk_dev = (OUT_CHUNK as u64 * self.p.info.out_rate as u64).div_ceil(INTERNAL_RATE as u64);
        self.adapt_safety(chunk_dev);
        let target = (self.out_burst_dev() + chunk_dev + self.safety) * 2;
        if self.ring_fill() >= target {
            return;
        }
        let consumed_frames = self.p.counters.out_consumed.load(Ordering::Relaxed) / 2;
        let wall_us = self.p.epoch.elapsed().as_micros() as u64;
        let dev_now = self.clock.update(wall_us, consumed_frames);
        // 한 번에 너무 많이 만들지 않도록 상한(장치 버퍼 4개분)
        let max_chunks = ((self.out_burst_dev() * 4) / chunk_dev.max(1) + 2) as usize;
        let mut produced = 0;
        while self.ring_fill() < target && produced < max_chunks {
            self.produce_chunk();
            produced += 1;
        }
        if produced > 0 {
            if let Some(net) = self.p.net.as_ref() {
                net.for_each_peer(|peer| peer.jitter.lock().end_burst(dev_now));
            }
            self.local.end_burst();
            self.p.stats.local_corrections.store(self.local.corrections, Ordering::Relaxed);
            self.p.stats.local_underruns.store(self.local.underruns, Ordering::Relaxed);
        }
    }

    fn produce_chunk(&mut self) {
        self.bus.fill(0.0);
        self.tap_bus.fill(0.0);

        // 로컬 입력(모니터 + 탭)
        self.local.pull(&mut self.local_buf);
        let ctl = &self.p.controls;
        if ctl.monitor_enabled.load(Ordering::Relaxed) {
            mixer::add_stereo(&mut self.bus, &self.local_buf, ctl.monitor_gain.load());
        }
        mixer::add_stereo(&mut self.tap_bus, &self.local_buf, 1.0);

        // 피어
        if let Some(net) = self.p.net.as_ref() {
            let bus = &mut self.bus;
            let tap = &mut self.tap_bus;
            let pb = &mut self.peer_buf;
            net.for_each_peer(|peer| {
                peer.jitter.lock().pop(pb);
                let (gl, gr) = peer.mix.lr_gains();
                peer.mix.level.store(mixer::rms(pb));
                mixer::add_stereo_lr(bus, pb, gl, gr);
                mixer::add_stereo_lr(tap, pb, gl, gr);
            });
        }

        mixer::scale(&mut self.bus, ctl.master_gain.load());
        mixer::soft_clip_buf(&mut self.bus);

        // 믹스 탭
        if let Some(mut guard) = self.p.tap.prod.try_lock() {
            if let Some(prod) = guard.as_mut() {
                mixer::soft_clip_buf(&mut self.tap_bus);
                let _ = prod.push_slice(&self.tap_bus);
            }
        }

        self.out_rs.push(&self.bus);
        loop {
            let frames = self.out_rs.pop(&mut self.out_tmp);
            if frames == 0 {
                break;
            }
            let pushed = self.p.out_prod.push_slice(&self.out_tmp[..frames * 2]);
            self.produced_out += pushed as u64;
        }
    }

    /// 로컬 경로 점유량(시간) 이동 평균(리뷰 M12).
    fn track_occupancy(&mut self) {
        let in48 = (self.in_rs.available() + self.local.frames()) as f64;
        let out_ring = self.ring_fill() as f64 / 2.0 + self.out_rs.available() as f64;
        let us = in48 * 1e6 / INTERNAL_RATE as f64 + out_ring * 1e6 / self.p.info.out_rate.max(1) as f64;
        self.occupancy_ewma_us += (us - self.occupancy_ewma_us) * 0.01;
        self.p.stats.occupancy_us.store(self.occupancy_ewma_us as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};

    #[test]
    fn device_clock_ignores_wake_latency() {
        // 480프레임@48k(10ms)마다 콜백, 워커는 0~4ms(가끔 20ms) 늦게 깨어남
        let mut c = DeviceClock::new(48_000);
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let start = 1_000_000u64;
        let mut worst = 0i64;
        for k in 1..3000u64 {
            let cb_time = start + k * 10_000;
            let late = if k % 97 == 0 { 20_000 } else { rng.gen_range(0..4000) };
            let est = c.update(cb_time + late, k * 480);
            if k > 50 {
                worst = worst.max((est as i64 - cb_time as i64).abs());
            }
        }
        // 기상 지연(최대 20ms)이 그대로 섞이면 수 ms 오차. 추정치는 1ms 미만이어야 한다.
        assert!(worst < 1000, "콜백 시각 추정 오차 {worst}µs");
    }

    #[test]
    fn device_clock_tracks_drift() {
        // 장치 클럭이 벽시계보다 300ppm 느림
        let mut c = DeviceClock::new(48_000);
        let mut worst = 0i64;
        for k in 1..6000u64 {
            let cb_time = (k as f64 * 10_000.0 * (1.0 + 300e-6)) as u64;
            let est = c.update(cb_time + 300, k * 480);
            if k > 100 {
                worst = worst.max((est as i64 - cb_time as i64).abs());
            }
        }
        assert!(worst < 400, "드리프트 추종 오차 {worst}µs");
    }

    #[test]
    fn lcm_gcd() {
        assert_eq!(lcm(480, 120), 480);
        assert_eq!(lcm(441, 240), 35280);
        assert_eq!(gcd(1024, 240), 16);
    }

    fn run_local(in_burst: usize, out_burst: usize, frame: usize, in_ppm: f64, seconds: f64) -> (LocalFifo, usize, usize) {
        run_local_ex(in_burst, out_burst, frame, in_ppm, seconds, 0.3e-3, 0.0)
    }

    /// 입력/출력 버스트를 서로 다른 주기로 흘려 로컬 FIFO를 검사한다.
    /// `out_offset_s`: 첫 출력 버스트 시각, `in_jitter_s`: 입력 콜백마다 0..이 값만큼 늦게 도착.
    fn run_local_ex(
        in_burst: usize,
        out_burst: usize,
        frame: usize,
        in_ppm: f64,
        seconds: f64,
        out_offset_s: f64,
        in_jitter_s: f64,
    ) -> (LocalFifo, usize, usize) {
        let mut rng = rand::rngs::StdRng::seed_from_u64(11);
        let mut f = LocalFifo::new();
        f.configure(in_burst, out_burst, frame);
        let mut acc = 0usize; // 프레임 누적
        let mut phase = 0.0f64;
        let mut t_in_nominal = 0.0f64;
        let mut t_in = 0.0f64;
        let mut t_out = out_offset_s;
        let in_period = in_burst as f64 / 48_000.0 / (1.0 + in_ppm * 1e-6);
        let out_period = out_burst as f64 / 48_000.0;
        let mut out = vec![0f32; OUT_CHUNK * 2];
        let mut prev = None::<f32>;
        let (mut disc, mut silent_runs, mut zero_run) = (0, 0, 0);
        let mut frame_buf = vec![0f32; frame * 2];
        let mut last_underruns = 0;
        let mut underrun_times = Vec::new();
        while t_out < seconds {
            if t_in <= t_out {
                acc += in_burst;
                while acc >= frame {
                    for d in frame_buf.as_chunks_mut::<2>().0 {
                        let v = (phase * 2.0 * std::f64::consts::PI).sin() as f32 * 0.5;
                        phase = (phase + 440.0 / 48_000.0).fract();
                        d[0] = v;
                        d[1] = v;
                    }
                    f.push(&frame_buf);
                    acc -= frame;
                }
                t_in_nominal += in_period;
                t_in = t_in_nominal + if in_jitter_s > 0.0 { rng.gen::<f64>() * in_jitter_s } else { 0.0 };
            } else {
                let mut left = out_burst;
                while left > 0 {
                    let n = left.min(OUT_CHUNK);
                    f.pull(&mut out[..n * 2]);
                    if t_out > 1.0 {
                        for fr in out[..n * 2].as_chunks::<2>().0 {
                            if let Some(p) = prev {
                                if (fr[0] - p).abs() > 0.08 {
                                    disc += 1;
                                }
                            }
                            prev = Some(fr[0]);
                            if fr[0] == 0.0 {
                                zero_run += 1;
                                if zero_run == 120 {
                                    silent_runs += 1;
                                }
                            } else {
                                zero_run = 0;
                            }
                        }
                    }
                    left -= n;
                }
                f.end_burst();
                if f.underruns > last_underruns {
                    last_underruns = f.underruns;
                    underrun_times.push((t_out * 1000.0) as u64);
                }
                t_out += out_period;
            }
        }
        if !underrun_times.is_empty() {
            println!("in {in_burst} out {out_burst} ppm {in_ppm}: 언더런 시각(ms) {underrun_times:?}, 보정 {}", f.corrections);
        }
        (f, disc, silent_runs)
    }

    #[test]
    fn local_fifo_handles_bursts_matrix() {
        // 리뷰 매트릭스(MONITOR): buf 480/2.5ms, 1024/5ms 등에서 무음·불연속 0
        for (buf, frame) in [(128, 240), (128, 120), (480, 120), (480, 240), (1024, 240), (1024, 480), (1024, 120), (441, 240)] {
            let (f, disc, silent) = run_local(buf, buf, frame, 0.0, 5.0);
            assert_eq!(silent, 0, "buf {buf} frame {frame}: 무음 구간");
            assert_eq!(disc, 0, "buf {buf} frame {frame}: 불연속");
            assert_eq!(f.overflows, 0, "buf {buf} frame {frame}: 상한 초과");
            assert_eq!(f.underruns, 0, "buf {buf} frame {frame}: 언더런");
            // 클럭이 같으면(드리프트 0) 정착 후 보정이 멈춰야 한다: 5초 동안 시작 수렴분 이내
            println!("local buf {buf} frame {frame}: 보정 {}", f.corrections);
            assert!(f.corrections <= 10, "buf {buf} frame {frame}: 드리프트 0에서 보정 {}", f.corrections);
        }
    }

    #[test]
    fn local_fifo_learns_locked_phase_order_flips() {
        // 듀플렉스 장치처럼 입·출력 콜백이 같은 시각(위상 고정)이고 입력이 0~1ms 늦게 오기도 한다.
        // 순서가 뒤집히면 입력 방출 하나만큼 패인다 → 최대 1회 언더런 후 여유를 늘려 안정.
        for (buf, frame) in [(480, 120), (1024, 480), (256, 240), (128, 240)] {
            let period = buf as f64 / 48_000.0;
            let (f, _disc, _silent) = run_local_ex(buf, buf, frame, 0.0, 20.0, period, 1e-3);
            println!("locked buf {buf} frame {frame}: 언더런 {}, margin {}", f.underruns, f.margin());
            assert!(f.underruns <= 1, "buf {buf} frame {frame}: 학습 후에도 언더런 {}", f.underruns);
        }
    }

    #[test]
    fn local_fifo_tracks_device_clock_difference() {
        // 입출력 장치가 다르고 ±300ppm 차이: 보정은 있되 무음/불연속/언더런 없음
        for ppm in [300.0, -300.0, 100.0, -100.0] {
            for (bi, bo) in [(480, 128), (128, 480), (256, 256), (441, 128), (1024, 480), (128, 1024)] {
                let (f, disc, silent) = run_local(bi, bo, 240, ppm, 30.0);
                assert_eq!(silent, 0, "ppm {ppm} in {bi} out {bo}");
                assert_eq!(disc, 0, "ppm {ppm} in {bi} out {bo}");
                assert_eq!(f.underruns, 0, "ppm {ppm} in {bi} out {bo}");
                assert_eq!(f.overflows, 0, "ppm {ppm} in {bi} out {bo}");
                if ppm.abs() >= 300.0 {
                    // 30초 × 300ppm = 432샘플 > 데드밴드 → 보정이 있어야 한다
                    assert!(f.corrections > 0, "ppm {ppm} in {bi} out {bo}: 드리프트 보정이 있어야 함");
                }
                // 진동 없음: 필요한 양(ppm × 30초)의 1.5배 + 시작 수렴분(입력 버스트+프레임) 이내
                let needed = ppm.abs() * 1e-6 * 48_000.0 * 30.0;
                let startup = (bi + 240) as f64;
                assert!(
                    (f.corrections as f64) < needed * 1.5 + startup,
                    "ppm {ppm} in {bi} out {bo}: 보정 과다 {} (필요 {needed:.0})",
                    f.corrections
                );
                assert!(f.frames() < f.cap_frames());
            }
        }
    }
}
