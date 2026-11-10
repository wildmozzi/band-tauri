//! 엔진 워커(비RT 스레드 1개, 설계 문서 1.1절).
//!
//! - 입력 링 → (리샘플) → 프레임 단위 인코드 → 피어별 암호화/송신
//! - 출력(콜백 구동 믹스, P1-0): 믹스는 출력 콜백이 한다(`rt.rs`). 워커는 소스(로컬 1 + 피어별)마다
//!   디코드된 PCM을 소스 링에 공급한다.
//!   - 피어: 받은 데이터는 미리 꺼내 두고, 은닉(PLC)은 출력 콜백 마감 직전에만 한다(`PeerFeed`).
//!     출력 콜백이 돌 때마다 지터 버퍼 깊이를 **재생 위치**(소스 링 머리) 기준으로 잰다.
//!   - 로컬(모니터·탭): 콜백이 소비한 만큼 `OUT_CHUNK` 단위로 로컬 FIFO에서 꺼내 다시 채운다.
//! - 깊이 측정 시각은 워커 기상 시각이 아니라 `DeviceClock`이 추정한 출력 콜백 시각이다.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ringbuf::traits::{Consumer, Observer, Producer};
use ringbuf::{HeapCons, HeapProd};

use super::codec::Encoder;
use super::config::{frame_code_from_samples, EngineConfig, INTERNAL_RATE, MAX_PACKET_BYTES, MAX_PAYLOAD_BYTES};
use super::device::StreamInfo;
use super::jitter::{shift_one, JitterBuffer};
use super::measure::EchoProbe;
use super::mixer::MixControls;
use super::resample::{StreamResampler, CHUNK as RESAMPLE_CHUNK};
use super::rt::{SourceBank, SourceShared, LOCAL_SOURCE, MAX_SOURCES, SLOT_ACTIVE, SLOT_DRAIN, SLOT_FREE};
use super::stats::AudioCounters;
use crate::net::packet::{FLAG_OPUS, FLAG_STEREO};
use crate::net::peer::Peer;
use crate::net::NetShared;

/// 워커 폴링 주기. Windows에서도 std::thread::sleep은 고해상도 타이머를 쓴다.
const POLL: Duration = Duration::from_micros(500);
/// 피어 은닉 마감이 가까울 때의 폴링 주기.
const FINE_POLL: Duration = Duration::from_micros(150);
/// 출력 생산 단위(48kHz 프레임). 1.33ms.
pub const OUT_CHUNK: usize = 64;

/// 워커 → 엔진 통계 공유값.
#[derive(Debug, Default)]
pub struct WorkerStats {
    /// 모니터 경로 점유량 시간평균(µs): 입력 리샘플러 + 로컬 FIFO + 로컬 리샘플러 + 로컬 소스 링.
    pub monitor_occupancy_us: AtomicU64,
    pub local_corrections: AtomicU64,
    pub local_underruns: AtomicU64,
    /// 피어 소스를 출력 콜백 마감 직전에 은닉으로 채운 횟수(패킷이 마감까지 안 옴).
    pub deadline_fills: AtomicU64,
    /// 피어 소스 링이 모자라 콜백이 0을 채우고, 그만큼 스트림을 건너뛴 장치 프레임 누적(워커 마감 놓침).
    pub skipped_frames: AtomicU64,
    /// 입력 또는 출력 콜백이 1초 넘게 멈췄다(장치 분리 등). 다시 돌면 false.
    pub device_stalled: AtomicBool,
}

/// 콜백이 이 시간 동안 멈추면 장치를 잃은 것으로 본다.
const DEVICE_STALL: Duration = Duration::from_secs(1);

// ============================================================================
// 장치 클럭 추정
// ============================================================================

/// 출력 콜백 시각 추정기.
///
/// 입력은 출력 콜백이 장치에 내보낸 **누적 프레임 수**(`out_played_frames`, 언더런으로 0을 채운 부분 포함)다.
/// 워커가 본 `wall − played/rate`는 콜백 직후에 관측할수록 작으므로, 최근 `WINDOW` 동안의 최솟값이
/// 실제 콜백 시각에 가장 가깝다. 최솟값을 고정 창(0.5초 버킷 4개)에서 구하므로
/// - 워커 기상 지연(값이 커지는 쪽)은 무시되고,
/// - 장치 정지·절전 복귀처럼 오프셋이 위로 한 번 뛰는 불연속도 최대 2초 안에 따라잡고,
/// - 장치-벽시계 드리프트는 창 길이만큼의 바이어스(500ppm에서 1ms 이하)로 따라간다.
///
/// 이전 구현은 소비 샘플 수(`out_consumed`)와 전역 최솟값 + 500ppm 누설을 써서, 출력 언더런마다
/// 결손이 0.5ms/s로만 회복되어 실제 지연이 조용히 쌓였다(PR #5 리뷰 H-1).
pub struct DeviceClock {
    rate: f64,
    bucket_start_us: f64,
    cur_min: f64,
    prev_mins: [f64; CLOCK_BUCKETS - 1],
    started: bool,
}

const CLOCK_BUCKET_US: f64 = 500_000.0;
const CLOCK_BUCKETS: usize = 4;

impl DeviceClock {
    pub fn new(rate: u32) -> Self {
        Self {
            rate: rate.max(1) as f64,
            bucket_start_us: 0.0,
            cur_min: f64::INFINITY,
            prev_mins: [f64::INFINITY; CLOCK_BUCKETS - 1],
            started: false,
        }
    }

    /// `played_frames`: 출력 콜백이 지금까지 장치에 내보낸 프레임 수. 반환: 마지막 콜백 시각 추정(µs).
    pub fn update(&mut self, wall_us: u64, played_frames: u64) -> u64 {
        let wall = wall_us as f64;
        if played_frames == 0 {
            return wall_us;
        }
        if !self.started {
            self.started = true;
            self.bucket_start_us = wall;
        }
        if wall - self.bucket_start_us >= CLOCK_BUCKET_US {
            // 버킷 회전(오래 쉬었으면 여러 칸)
            let steps = (((wall - self.bucket_start_us) / CLOCK_BUCKET_US) as usize).min(CLOCK_BUCKETS);
            for _ in 0..steps {
                self.prev_mins.rotate_right(1);
                self.prev_mins[0] = self.cur_min;
                self.cur_min = f64::INFINITY;
            }
            self.bucket_start_us += steps as f64 * CLOCK_BUCKET_US;
        }
        let dev_t = played_frames as f64 * 1e6 / self.rate;
        let off = wall - dev_t;
        self.cur_min = self.cur_min.min(off);
        let m = self.prev_mins.iter().copied().fold(self.cur_min, f64::min);
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
    since_underrun: usize,
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
/// 학습한 여유를 되돌리기 전 무사고 시간(48k 샘플 = 10초).
const LOCAL_MARGIN_DECAY_SAMPLES: usize = 480_000;

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
            since_underrun: 0,
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
        self.since_underrun = 0;
        self.apply_margin();
    }

    /// 10초 동안 언더런이 없으면 학습한 여유를 방출 하나만큼 되돌린다(PR #5 M-5: 한 번의 스파이크가
    /// 세션 끝까지 모니터·송출 지연을 키우지 않도록). 수위 제어가 남는 양을 서서히 줄인다.
    fn decay_margin(&mut self, out_burst: usize) {
        if self.margin <= LOCAL_MARGIN {
            self.since_underrun = 0;
            return;
        }
        self.since_underrun += out_burst;
        if self.since_underrun >= LOCAL_MARGIN_DECAY_SAMPLES {
            self.since_underrun = 0;
            self.margin = self.margin.saturating_sub(self.release.max(1)).max(LOCAL_MARGIN);
            self.apply_margin();
        }
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
        let out_burst = self.configured.1;
        self.decay_margin(out_burst);
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
// 콜백 구동 믹스: 피어 소스 공급(PeerFeed)
// ============================================================================

/// 출력 콜백 마감 여유(µs): 다음 콜백 예상 시각보다 이만큼 앞에서 은닉을 결정한다.
/// 워커 폴링(0.5ms)과 Windows 기상 지연을 덮는다. 지터 목표(`ceil(p95/F)+1`프레임)의 +1프레임(≥ 2.5ms)이
/// 이 여유보다 커서 정상 상태에서는 마감 전에 실제 패킷이 이미 와 있다(engine-notes "콜백 구동 믹스").
pub const DEADLINE_GUARD_US: u64 = 1500;

/// 출력 콜백 타이밍(워커가 매 반복 계산).
#[derive(Debug, Clone, Copy)]
pub struct FeedTiming {
    /// 지난 반복 이후 출력 콜백이 돌았다(이번 반복에서 깊이를 잰다).
    pub new_callback: bool,
    /// 마지막 출력 콜백 시각 추정치(장치 클럭, µs).
    pub dev_now_us: u64,
    /// 워커가 실제로 깨어난 시각(µs).
    pub wall_us: u64,
    /// 다음 출력 콜백 예상 시각(µs).
    pub next_cb_us: u64,
    /// 다음 콜백이 꺼낼 장치 프레임 수.
    pub out_burst: usize,
    pub out_rate: u32,
    /// 이 타이밍을 만들 때의 출력 콜백 시작 횟수. 콜백 도중이었으면 `None`(이번 반복은 판단하지 않음).
    pub cb_seq: Option<u64>,
}

impl FeedTiming {
    fn dev_to_48(&self, frames: f64) -> f64 {
        frames * INTERNAL_RATE as f64 / self.out_rate.max(1) as f64
    }

    fn chunk_dev(&self) -> usize {
        (OUT_CHUNK * self.out_rate as usize).div_ceil(INTERNAL_RATE as usize)
    }
}

/// 출력 콜백 관찰기: 재생 프레임 카운터로 콜백 시각을 추정하고(`DeviceClock`) 반복마다 `FeedTiming`을 만든다.
pub struct CallbackTracker {
    clock: DeviceClock,
    cb_played: u64,
    dev_now_us: u64,
}

impl CallbackTracker {
    pub fn new(out_rate: u32) -> Self {
        Self { clock: DeviceClock::new(out_rate), cb_played: 0, dev_now_us: 0 }
    }

    /// 장치 클럭은 재생 프레임 수(언더런 포함)로 추정한다(PR #5 H-1).
    pub fn observe(&mut self, c: &AudioCounters, wall_us: u64, out_burst: usize, out_rate: u32) -> FeedTiming {
        let begin = c.out_cb_begin.load(Ordering::SeqCst);
        let played = c.out_played_frames.load(Ordering::SeqCst);
        // 콜백 도중이면(시작 ≠ 끝) 재생 카운터와 링 점유량이 어긋날 수 있으므로 다음 반복으로 미룬다.
        let cb_seq = (c.out_cb_end.load(Ordering::SeqCst) == begin).then_some(begin);
        let new_callback = cb_seq.is_some() && played != self.cb_played && played > 0;
        if new_callback {
            self.cb_played = played;
            self.dev_now_us = self.clock.update(wall_us, played);
        }
        let out_rate = out_rate.max(1);
        let next_cb_us = if self.dev_now_us == 0 {
            0
        } else {
            self.dev_now_us + out_burst as u64 * 1_000_000 / out_rate as u64
        };
        FeedTiming { new_callback, dev_now_us: self.dev_now_us, wall_us, next_cb_us, out_burst, out_rate, cb_seq }
    }
}

/// `PeerFeed::service` 한 번의 결과(진단용).
#[derive(Debug, Default, Clone, Copy)]
pub struct FeedEvents {
    /// 마감에 쫓겨 은닉으로 채웠다.
    pub deadline_fill: bool,
    /// 콜백이 0으로 채워 건너뛴 장치 프레임.
    pub skipped_frames: u64,
    /// 링이 다음 콜백에 모자라고 은닉 마감이 한 폴링 주기 안이다. 워커는 짧게 자서 마감을 놓치지 않는다.
    pub urgent: bool,
}

/// 피어 하나의 출력 소스: 소스 링 슬롯 + 48k → 장치 레이트 리샘플러.
///
/// 규칙(설계 근거는 engine-notes "콜백 구동 믹스"):
/// 1. **실제 데이터는 미리**: 지터 버퍼가 은닉 없이 꺼낼 수 있으면(`can_pop_real`) 소스 링을
///    `다음 콜백 소비량 + 2청크`까지 채운다. 나머지는 지터 버퍼에 남겨 패킷 단위 버림이 동작하게 한다.
/// 2. **은닉은 마감 직전에만**: 링이 다음 콜백 소비량보다 적고 데이터가 없으면, 다음 콜백 예상 시각
///    − `DEADLINE_GUARD_US`가 지난 뒤에만 `pop`(PLC)한다. 늦게 오는 패킷을 마지막까지 기다린다.
/// 3. **깊이는 재생 위치 기준**: 콜백이 돌 때마다 `end_burst(콜백 시각, 벽시계, 링 점유량)`.
///    그래서 소스 링에 머무는 시간이 지터 버퍼 깊이(`bufferMs`)에 포함되고 목표로 제어된다.
///    믹스된 출력 링이 따로 없으므로 장치 버퍼 1개만큼의 하한이 사라진다.
/// 4. **언더런 때 지연을 쌓지 않음**: 콜백이 링 부족으로 0을 채운 프레임 수만큼 스트림을 건너뛴다.
/// 5. **적응 여유**(PR #11 M4): 언더런이 나면 피어별 여유(`margin`)를 청크 하나씩 늘린다. 여유만큼 선행량 상한·
///    부족 판정·은닉 마감을 앞당겨, 워커 기상이 늦거나 콜백이 격자보다 이르게 오는 환경에서 같은 언더런이 반복되지
///    않게 한다. 여유는 소스 링(재생 위치 앞)에 있으므로 `bufferMs`에 포함되고 지연 회계는 그대로다. 언더런이 없으면 0.
///    상한은 `min(다음 콜백 소비량, (F + 2ms)/2 × 프로필 배율)`이라 목표 추가분 2·여유가 1프레임 + 2ms를 넘지 않는다
///    (P1-5, PR #16 M3: 이전 상한은 콜백 소비량이라 10ms 버퍼에서 +19.6ms). 마지막 언더런 뒤 `MARGIN_HOLD_US` × 배율
///    동안 유지하고, 그 뒤 `MARGIN_STEP_US`마다 청크씩 줄인다(이전: 10초마다 청크 → 480프레임이면 75초).
pub struct PeerFeed {
    slot: usize,
    key: (usize, u32),
    rs: StreamResampler,
    seen_underrun: u64,
    seen: bool,
    /// 적응 여유(장치 프레임).
    margin: usize,
    /// 마지막 여유 증가(언더런) 시각(µs).
    margin_at_us: u64,
    /// 마지막 여유 감소 시각(µs).
    margin_step_us: u64,
}

/// 마지막 언더런 뒤 여유를 유지하는 시간(균형 프로필, `JitterTuning::margin_scale`배).
const MARGIN_HOLD_US: u64 = 10_000_000;
/// 유지 시간이 지난 뒤 여유를 청크 하나씩 줄이는 간격.
const MARGIN_STEP_US: u64 = 2_000_000;

impl PeerFeed {
    pub fn new(slot: usize, key: (usize, u32), out_rate: u32, shared: &SourceShared) -> Result<Self, String> {
        Ok(Self {
            slot,
            key,
            rs: StreamResampler::new(INTERNAL_RATE, out_rate, 2)?,
            seen_underrun: shared.underrun_frames.load(Ordering::Relaxed),
            seen: true,
            margin: 0,
            margin_at_us: 0,
            margin_step_us: 0,
        })
    }

    /// 여유 상한(장치 프레임): 다음 콜백 소비량과 `(F + 2ms)/2 × 배율` 중 작은 값(최소 청크 하나).
    fn margin_cap(&self, jb: &JitterBuffer, t: &FeedTiming) -> usize {
        let cap48 = (jb.frame_samples() as f64 + 2.0 * INTERNAL_RATE as f64 / 1000.0) / 2.0 * jb.tuning().margin_scale;
        let cap_dev = (cap48 * t.out_rate as f64 / INTERNAL_RATE as f64) as usize;
        cap_dev.max(t.chunk_dev()).min(t.out_burst.max(t.chunk_dev()))
    }

    /// 은닉 판단에 필요한 p95 너머 깊이(샘플, 48k)를 지터 버퍼에 알린다(`JitterBuffer::set_consumer_lead`, P1-13).
    /// 여유는 두 번 쓰인다: 은닉 마감을 여유만큼 앞당기고(`guard_us`), 그 마감에 다음 콜백 소비량보다 여유만큼 더
    /// 채운다(`need`). 마감 시각에 실제 패킷이 와 있으려면 재생 위치 기준 깊이가 `p95 + DEADLINE_GUARD + 2·여유`
    /// 이상이어야 한다(engine-notes "콜백 구동 믹스"의 필요 조건에 여유를 넣은 것). 1배만 반영하면 결정적 스윕에서
    /// 언더런 수 초 뒤 은닉이 남았다.
    fn consumer_lead(&self, t: &FeedTiming) -> f64 {
        DEADLINE_GUARD_US as f64 * INTERNAL_RATE as f64 / 1e6 + t.dev_to_48(2.0 * self.margin as f64)
    }

    /// 현재 적응 여유(장치 프레임, 진단·테스트용).
    pub fn margin(&self) -> usize {
        self.margin
    }

    pub fn slot(&self) -> usize {
        self.slot
    }

    /// 링 + 리샘플러 출력에 있는 장치 프레임.
    fn dev_ahead(&self, prod: &HeapProd<f32>) -> usize {
        prod.occupied_len() / 2 + self.rs.available()
    }

    /// 지금 소스 링 꼬리에 넣을 샘플의 재생(장치 쓰기) 예정 시각(µs, 워커 epoch). 링·리샘플러에 앞서 있는
    /// 양만큼 다음 출력 콜백보다 뒤다. 에코 측정의 도착 시각으로 쓴다.
    fn play_time_us(&self, prod: &HeapProd<f32>, t: &FeedTiming) -> f64 {
        let ahead48 = t.dev_to_48(self.dev_ahead(prod) as f64) + self.rs.pending_input() as f64;
        t.next_cb_us as f64 + ahead48 * 1e6 / INTERNAL_RATE as f64
    }

    /// 48k 청크 하나를 리샘플해 소스 링에 넣는다. 링이 가득 차면 리샘플러에 남는다.
    fn push(&mut self, chunk: &[f32], prod: &mut HeapProd<f32>, tmp: &mut [f32]) {
        self.rs.push(chunk);
        self.drain_rs(prod, tmp);
    }

    fn drain_rs(&mut self, prod: &mut HeapProd<f32>, tmp: &mut [f32]) {
        loop {
            let room = (prod.vacant_len() / 2).min(tmp.len() / 2);
            if room == 0 {
                break;
            }
            let n = self.rs.pop(&mut tmp[..room * 2]);
            if n == 0 {
                break;
            }
            prod.push_slice(&tmp[..n * 2]);
        }
    }

    /// 한 반복분 처리. `consistent()`는 `t`를 만든 뒤 링 점유량을 읽을 때까지 출력 콜백이 돌지 않았는지
    /// 확인한다. 돌았다면(링은 이미 비었는데 마감 시각은 이전 콜백 기준) 깊이 측정과 마감 판단을 이번
    /// 반복에서 하지 않는다. 이 검사가 없으면 한 주기 이르게 은닉했다(위상에 따라 수 초에 수 회).
    #[allow(clippy::too_many_arguments)]
    pub fn service(
        &mut self,
        jb: &mut JitterBuffer,
        prod: &mut HeapProd<f32>,
        sh: &SourceShared,
        t: &FeedTiming,
        consistent: &dyn Fn() -> bool,
        chunk: &mut [f32],
        tmp: &mut [f32],
        mut probe: Option<&mut EchoProbe>,
    ) -> FeedEvents {
        let mut ev = FeedEvents::default();
        let chunk = &mut chunk[..OUT_CHUNK * 2];
        self.drain_rs(prod, tmp);
        if t.new_callback {
            // (4) 콜백이 모자라 0을 채운 만큼 스트림을 건너뛴다. 안 하면 그만큼 재생이 영구히 늦어진다.
            let u = sh.underrun_frames.load(Ordering::Relaxed);
            let d = u.saturating_sub(self.seen_underrun);
            self.seen_underrun = u;
            if d > 0 && jb.is_playing() {
                // (5) 여유가 늘면 지터 목표도 깊어진다(`consumer_lead`). 0 채움이 이미 재생을 d만큼 늦췄으므로,
                // 늘어난 목표만큼은 건너뛰지 않고 남겨 두면 보정(삽입 램프) 없이 바로 새 목표 깊이가 된다.
                // 건너뛰기는 상한(max_ms) 적용 뒤의 목표 차이로 계산한다(PR #16 L1).
                let before = jb.target_samples();
                self.margin = (self.margin + t.chunk_dev()).min(self.margin_cap(jb, t));
                self.margin_at_us = t.wall_us;
                self.margin_step_us = t.wall_us;
                jb.set_consumer_lead(self.consumer_lead(t));
                let d48 = t.dev_to_48(d as f64);
                let skip = (d48 - (jb.target_samples() - before)).max(0.0).round();
                // 건너뛰지 않고 남긴 만큼 깊이가 바로 커진다. 깊이 추정에도 바로 반영해 삽입 보정이 끼지 않게 한다.
                jb.shift_depth(d48 - skip);
                let mut left = skip as usize;
                while left > 0 {
                    let k = left.min(OUT_CHUNK);
                    jb.pop(&mut chunk[..k * 2]);
                    left -= k;
                }
                // 실제로 건너뛴 양(장치 프레임, PR #16 L2). 0 채움 중 늘어난 목표로 남긴 몫은 빼고 센다.
                ev.skipped_frames = (skip * t.out_rate as f64 / INTERNAL_RATE as f64).round() as u64;
            } else if self.margin > 0 {
                let hold = (MARGIN_HOLD_US as f64 * jb.tuning().margin_scale) as u64;
                let now = t.wall_us;
                if now.saturating_sub(self.margin_at_us) > hold && now.saturating_sub(self.margin_step_us) > MARGIN_STEP_US {
                    self.margin = self.margin.saturating_sub(t.chunk_dev()).min(self.margin_cap(jb, t));
                    self.margin_step_us = now;
                }
            }
            jb.set_consumer_lead(self.consumer_lead(t));
            // (3) 재생 위치 기준 깊이: 링·리샘플러에 있는 양(재생 위치가 그만큼 과거)을 넘긴다.
            let ahead = t.dev_to_48(self.dev_ahead(prod) as f64) + self.rs.pending_input() as f64;
            if consistent() {
                jb.end_burst(t.dev_now_us, t.wall_us, ahead);
            }
        }
        if !jb.is_playing() {
            sh.playing.store(false, Ordering::Relaxed);
            return ev;
        }
        // (1) 실제 데이터는 미리
        // 패킷 꼬리(< 청크)까지 정확히 꺼낸다. 청크 단위로만 꺼내면 실제 데이터가 FIFO에 남아 있는데도
        // 마감에 은닉이 끼었다(위상에 따라 수 초에 수 회).
        // 유효 여유(PR #16 M1, P1-5): 여유가 늘면 목표가 2·여유만큼 깊어지는데, 0 채움으로 남긴 양이 모자라면 나머지는
        // 삽입 램프(수백 ms)로 채운다. 그동안 늘어난 여유를 마감·채울 양에 그대로 쓰면 깊이가 아직 없어 마감 채움(은닉)이
        // 났다. 깊이 부족분 s의 절반만큼 여유를 덜 써서 `깊이 ≥ 지터 + 1.5ms + 2·유효 여유`를 유지한다.
        let short48 = (-jb.depth_error()).max(0.0);
        let short_dev = (short48 / 2.0 * t.out_rate as f64 / INTERNAL_RATE as f64).ceil() as usize;
        let m_eff = self.margin.saturating_sub(short_dev);
        let lead_cap = t.out_burst + 2 * t.chunk_dev() + m_eff;
        while self.dev_ahead(prod) < lead_cap && prod.vacant_len() >= OUT_CHUNK * 4 {
            let k = jb.real_frames().min(OUT_CHUNK);
            if k == 0 {
                break;
            }
            jb.pop(&mut chunk[..k * 2]);
            if let Some(p) = probe.as_deref_mut() {
                p.record(&chunk[..k * 2], self.play_time_us(prod, t));
            }
            self.push(&chunk[..k * 2], prod, tmp);
        }
        // (2) 은닉은 마감 직전에만, 다음 콜백에 모자란 만큼만(+ 적응 여유)
        let need = t.out_burst + m_eff;
        let guard_us = DEADLINE_GUARD_US + m_eff as u64 * 1_000_000 / t.out_rate.max(1) as u64;
        let short = self.dev_ahead(prod) < need;
        if short && t.wall_us + guard_us >= t.next_cb_us && consistent() {
            ev.deadline_fill = true;
            let mut guard = 0;
            while self.dev_ahead(prod) < need && guard < 64 && jb.is_playing() {
                let deficit = t.dev_to_48((need - self.dev_ahead(prod)) as f64).ceil() as usize;
                let k = deficit.clamp(1, OUT_CHUNK);
                jb.pop(&mut chunk[..k * 2]);
                if let Some(p) = probe.as_deref_mut() {
                    p.record(&chunk[..k * 2], self.play_time_us(prod, t));
                }
                self.push(&chunk[..k * 2], prod, tmp);
                guard += 1;
            }
        } else if short && t.wall_us + guard_us + POLL.as_micros() as u64 >= t.next_cb_us {
            ev.urgent = true;
        }
        sh.playing.store(true, Ordering::Relaxed);
        ev
    }
}

// ============================================================================
// 워커
// ============================================================================

pub struct WorkerParams {
    pub cfg: EngineConfig,
    pub info: StreamInfo,
    pub in_cons: HeapCons<f32>,
    /// 소스 링 생산자(슬롯별)와 공유 상태. 믹스 탭 소비자는 엔진이 가진다.
    pub prods: Vec<HeapProd<f32>>,
    pub bank: Arc<SourceBank>,
    pub counters: Arc<AudioCounters>,
    pub controls: Arc<MixControls>,
    pub net: Option<Arc<NetShared>>,
    pub stop: Arc<AtomicBool>,
    pub stats: Arc<WorkerStats>,
    /// 지터 버퍼 수신 시각과 같은 기준(µs)의 시계.
    pub epoch: Instant,
    /// 에코 측정(P1-1): 있으면 그 피어에게 내 입력 대신 무음 + 클릭을 보내고, 그 피어 소리의 재생 시각을 모은다.
    pub probe: Arc<parking_lot::Mutex<Option<EchoProbe>>>,
}

pub struct Worker {
    p: WorkerParams,
    frame: usize,
    ich: usize,
    in_rs: StreamResampler,
    /// 로컬(모니터) 소스: 48k → 장치 레이트.
    local_rs: StreamResampler,
    encoder: Option<Encoder>,
    /// 에코 측정 대상 피어용 인코더(측정 중에만 있음). 다른 피어와 코덱 상태를 섞지 않는다.
    alt_encoder: Option<Encoder>,
    alt_frame: Vec<f32>,
    alt_payload: Vec<u8>,
    flags: u8,
    frame_code: u8,
    ts: u32,
    tracker: CallbackTracker,
    local: LocalFifo,
    feeds: Vec<PeerFeed>,
    in_tmp: Vec<f32>,
    /// 48k 입력 블록(리샘플 직후, 내부 채널).
    in48_buf: Vec<f32>,
    /// 인코더용 프레임 누적.
    cap_acc: VecDeque<f32>,
    frame_buf: Vec<f32>,
    stereo_buf: Vec<f32>,
    payload: Vec<u8>,
    pkt: Vec<u8>,
    local_buf: Vec<f32>,
    chunk_buf: Vec<f32>,
    out_tmp: Vec<f32>,
    reported_errors: u32,
    encode_errors: u64,
    monitor_ewma_us: f64,
    safety: u64,
    seen_xruns: u64,
    last_xrun: Duration,
    last_played: u64,
    last_played_at: Duration,
    last_captured: u64,
    last_captured_at: Duration,
    slot_full_logged: bool,
}

/// 지원하는 장치 레이트 범위.
const MIN_DEVICE_RATE: u32 = 8_000;
const MAX_DEVICE_RATE: u32 = 384_000;

impl Worker {
    /// 리샘플러/인코더 생성. 실패하면 엔진 시작을 실패시킨다(리뷰 M10).
    pub fn new(p: WorkerParams) -> Result<Self, String> {
        let frame = p.cfg.frame_samples();
        let ich = p.cfg.channels as usize;
        for (what, r) in [("입력", p.info.in_rate), ("출력", p.info.out_rate)] {
            if !(MIN_DEVICE_RATE..=MAX_DEVICE_RATE).contains(&r) {
                return Err(format!("지원하지 않는 {what} 장치 레이트: {r}Hz"));
            }
        }
        if p.prods.len() != MAX_SOURCES {
            return Err("소스 링 수가 맞지 않습니다".into());
        }
        let in_rs = StreamResampler::new(p.info.in_rate, INTERNAL_RATE, ich)?;
        let local_rs = StreamResampler::new(INTERNAL_RATE, p.info.out_rate, 2)?;
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
        // 로컬 소스는 항상 켜져 있다(모니터 꺼짐 = 버스 게인 0, 탭에는 계속 나감).
        let local_slot = &p.bank.slots[LOCAL_SOURCE];
        local_slot.set_gains((0.0, 0.0), (1.0, 1.0));
        local_slot.state.store(SLOT_ACTIVE, Ordering::Release);
        Ok(Self {
            frame,
            ich,
            in_rs,
            local_rs,
            encoder,
            alt_encoder: None,
            alt_frame: vec![0.0; frame * ich],
            alt_payload: vec![0u8; MAX_PAYLOAD_BYTES],
            flags,
            frame_code,
            ts: rand::random(),
            tracker: CallbackTracker::new(p.info.out_rate),
            local: LocalFifo::new(),
            feeds: Vec::with_capacity(MAX_SOURCES),
            in_tmp: vec![0.0; 4096],
            in48_buf: vec![0.0; 2048 * ich],
            cap_acc: VecDeque::with_capacity((frame + 4096) * ich),
            frame_buf: vec![0.0; frame * ich],
            stereo_buf: vec![0.0; 2048 * 2],
            payload: vec![0u8; MAX_PAYLOAD_BYTES],
            pkt: vec![0u8; MAX_PACKET_BYTES],
            local_buf: vec![0.0; OUT_CHUNK * 2],
            chunk_buf: vec![0.0; OUT_CHUNK * 2],
            out_tmp: vec![0.0; 8192],
            reported_errors: 0,
            encode_errors: 0,
            monitor_ewma_us: 0.0,
            safety: 0,
            seen_xruns: 0,
            last_xrun: Duration::ZERO,
            last_played: 0,
            last_played_at: Duration::ZERO,
            last_captured: 0,
            last_captured_at: Duration::ZERO,
            slot_full_logged: false,
            p,
        })
    }

    pub fn run(mut self) {
        let _prio = super::prio::promote_current_thread();
        log::info!("워커 시작: frame {} ch {}", self.frame, self.ich);
        while !self.p.stop.load(Ordering::Relaxed) {
            self.reconfigure_local();
            self.pump_input();
            let timing = self.timing();
            let urgent = self.service_peers(&timing);
            self.service_local();
            self.track_occupancy();
            self.watch_device();
            let errs = self.p.counters.stream_errors.load(Ordering::Relaxed);
            if errs != self.reported_errors {
                log::warn!("오디오 스트림 에러 누적 {}회(장치 분리 등)", errs);
                self.reported_errors = errs;
            }
            // 은닉 마감이 가까우면 짧게 잔다(Windows 고해상도 타이머라도 0.5ms 폴링은 마감을 넘길 수 있다).
            std::thread::sleep(if urgent { FINE_POLL } else { POLL });
        }
        // 정지: 피어 슬롯을 닫는다(콜백이 아직 돌면 남은 소리를 버린다).
        for f in self.feeds.drain(..) {
            let sh = &self.p.bank.slots[f.slot];
            sh.playing.store(false, Ordering::Relaxed);
            sh.state.store(SLOT_DRAIN, Ordering::Release);
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
            0 => match self.p.info.out_period_frames {
                0 => self.p.info.buffer_frames as u64,
                n => n as u64,
            },
            n => n as u64,
        }
    }

    fn reconfigure_local(&mut self) {
        let out48 = Self::to48(self.out_burst_dev() as u32, self.p.info.out_rate);
        // 모니터/탭은 프레임 누적 전에 분기하므로 입력 방출 단위는 리샘플러 청크(패스스루면 1)다.
        let granularity = if self.in_rs.is_passthrough() { 1 } else { RESAMPLE_CHUNK };
        self.local.configure(self.in_burst48(), out48, granularity);
    }

    /// 입력·출력 콜백이 1초 넘게 멈추면 장치를 잃은 것으로 본다(리뷰 M9 / PR #5 M-2).
    /// cpal 에러 콜백은 비치명 에러도 보내므로 그것만으로는 판단하지 않는다.
    fn watch_device(&mut self) {
        let now = self.p.epoch.elapsed();
        let played = self.p.counters.out_played_frames.load(Ordering::Relaxed);
        let captured = self.p.counters.in_captured_frames.load(Ordering::Relaxed);
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
        if stalled != self.p.stats.device_stalled.load(Ordering::Relaxed) {
            if stalled {
                log::error!("오디오 장치 콜백이 멈췄습니다(장치 분리 가능성)");
            } else {
                log::info!("오디오 장치 콜백 재개");
            }
            self.p.stats.device_stalled.store(stalled, Ordering::Relaxed);
        }
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
        let muted = self.p.controls.input_muted.load(Ordering::Relaxed);
        // 리샘플 직후 모니터/탭으로 바로 보낸다(프레임 누적 대기 없음, PR #5 M-5).
        loop {
            let mut buf = std::mem::take(&mut self.in48_buf);
            let frames = self.in_rs.pop(&mut buf);
            if frames == 0 {
                self.in48_buf = buf;
                break;
            }
            let block = &mut buf[..frames * self.ich];
            // 입력 뮤트: 송신/모니터/탭 모두 무음. 패킷은 계속 보내 수신측 타이밍을 유지한다.
            if muted {
                block.fill(0.0);
            }
            self.push_local(block);
            self.cap_acc.extend(block.iter().copied());
            self.in48_buf = buf;
        }
        let n = self.frame * self.ich;
        while self.cap_acc.len() >= n {
            let mut fb = std::mem::take(&mut self.frame_buf);
            for (d, s) in fb.iter_mut().zip(self.cap_acc.drain(..n)) {
                *d = s;
            }
            self.on_capture_frame(&fb);
            self.frame_buf = fb;
        }
    }

    fn push_local(&mut self, block: &[f32]) {
        if self.ich == 2 {
            self.local.push(block);
        } else {
            let sb = &mut self.stereo_buf;
            for (d, &s) in sb.as_chunks_mut::<2>().0.iter_mut().zip(block.iter()) {
                d[0] = s;
                d[1] = s;
            }
            let n = block.len() * 2;
            let sb = std::mem::take(&mut self.stereo_buf);
            self.local.push(&sb[..n]);
            self.stereo_buf = sb;
        }
    }

    /// 에코 측정 대상 피어용 프레임(무음 + 클릭)을 만들어 인코드한다. 반환: (피어 id, 페이로드 길이).
    fn probe_frame(&mut self) -> Option<(String, usize)> {
        let now_us = self.p.epoch.elapsed().as_secs_f64() * 1e6;
        let mut g = self.p.probe.lock();
        let Some(pr) = g.as_mut() else {
            self.alt_encoder = None;
            return None;
        };
        if self.alt_encoder.is_none() {
            match Encoder::new(self.p.cfg.codec, self.ich, self.p.cfg.opus_bitrate()) {
                Ok(e) => self.alt_encoder = Some(e),
                Err(e) => {
                    log::warn!("에코 측정 인코더 생성 실패: {e}");
                    return None;
                }
            }
        }
        pr.next_frame(&mut self.alt_frame, self.ich, now_us);
        let enc = self.alt_encoder.as_mut()?;
        let n = enc.encode(&self.alt_frame, &mut self.alt_payload).ok()?;
        Some((pr.peer_id.clone(), n))
    }

    fn on_capture_frame(&mut self, frame: &[f32]) {
        let alt = if self.encoder.is_some() { self.probe_frame() } else { None };
        if let (Some(enc), Some(net)) = (self.encoder.as_mut(), self.p.net.as_ref()) {
            let alt = alt.as_ref().map(|(id, n)| (id.as_str(), &self.alt_payload[..*n]));
            match enc.encode(frame, &mut self.payload) {
                Ok(n) => net.send_audio(&self.payload[..n], self.flags, self.frame_code, self.ts, &mut self.pkt, alt),
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
    }

    fn timing(&mut self) -> FeedTiming {
        let wall_us = self.p.epoch.elapsed().as_micros() as u64;
        let out_burst = self.out_burst_dev() as usize;
        self.tracker.observe(&self.p.counters, wall_us, out_burst, self.p.info.out_rate)
    }

    /// 피어 목록과 소스 슬롯을 맞춘다(새 피어에 빈 슬롯 배정, 사라진 피어 슬롯 해제).
    fn sync_peer_slots(&mut self, net: &NetShared) {
        for f in &mut self.feeds {
            f.seen = false;
        }
        let out_rate = self.p.info.out_rate;
        net.for_each_peer(|peer| {
            let key = (peer as *const Peer as usize, peer.ssrc);
            if let Some(f) = self.feeds.iter_mut().find(|f| f.key == key) {
                f.seen = true;
                return;
            }
            let used = |s: usize| self.feeds.iter().any(|f| f.slot == s);
            let free = (1..MAX_SOURCES)
                .find(|&s| !used(s) && self.p.bank.slots[s].state.load(Ordering::Acquire) == SLOT_FREE);
            let Some(slot) = free else {
                if !self.slot_full_logged {
                    log::warn!("출력 소스 슬롯이 부족합니다(피어 {})", peer.id);
                    self.slot_full_logged = true;
                }
                return;
            };
            let sh = &self.p.bank.slots[slot];
            match PeerFeed::new(slot, key, out_rate, sh) {
                Ok(f) => {
                    sh.playing.store(false, Ordering::Relaxed);
                    sh.state.store(SLOT_ACTIVE, Ordering::Release);
                    self.feeds.push(f);
                }
                Err(e) => log::error!("피어 출력 소스 생성 실패: {e}"),
            }
        });
        let bank = &self.p.bank;
        self.feeds.retain(|f| {
            if !f.seen {
                let sh = &bank.slots[f.slot];
                sh.playing.store(false, Ordering::Relaxed);
                sh.state.store(SLOT_DRAIN, Ordering::Release);
            }
            f.seen
        });
    }

    /// 반환: 은닉 마감이 가까운 피어가 있다(짧게 자야 함).
    fn service_peers(&mut self, t: &FeedTiming) -> bool {
        let Some(net) = self.p.net.clone() else {
            return false;
        };
        self.sync_peer_slots(&net);
        let counters = self.p.counters.clone();
        let cb_seq = t.cb_seq;
        let consistent = move || cb_seq.is_some_and(|s| counters.out_cb_begin.load(Ordering::SeqCst) == s);
        let (mut fills, mut skipped, mut urgent) = (0u64, 0u64, false);
        let probe = self.p.probe.clone();
        let mut guard = probe.lock();
        let mut tap = guard.as_mut().filter(|p| p.tap);
        net.for_each_peer(|peer| {
            let key = (peer as *const Peer as usize, peer.ssrc);
            let Some(f) = self.feeds.iter_mut().find(|f| f.key == key) else {
                return;
            };
            let sh = &self.p.bank.slots[f.slot];
            let g = peer.mix.lr_gains();
            sh.set_gains(g, g);
            peer.mix.level.store(sh.level.load());
            let Some(prod) = self.p.prods.get_mut(f.slot) else {
                return;
            };
            let ev = f.service(&mut peer.jitter.lock(), prod, sh, t, &consistent, &mut self.chunk_buf, &mut self.out_tmp, tap.as_deref_mut().filter(|p| p.peer_id == peer.id && p.peer_ssrc == peer.ssrc));
            fills += u64::from(ev.deadline_fill);
            skipped += ev.skipped_frames;
            urgent |= ev.urgent;
        });
        if fills > 0 {
            self.p.stats.deadline_fills.fetch_add(fills, Ordering::Relaxed);
        }
        if skipped > 0 {
            self.p.stats.skipped_frames.fetch_add(skipped, Ordering::Relaxed);
        }
        urgent
    }

    /// xrun이 나면 로컬 소스 여유를 청크 하나씩 늘리고(최대 장치 버퍼 1개), 10초 동안 없으면 줄인다.
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

    /// 로컬(모니터 + 탭) 소스. 로컬 FIFO가 입·출력 클럭 차를 흡수하고 자기 수위를 제어하므로, 소스 링은
    /// 콜백이 소비한 만큼 "장치 버퍼 1개 + 청크 + 적응 여유"로 다시 채운다(모니터 경로는 GOAL 지표 밖).
    fn service_local(&mut self) {
        let ctl = &self.p.controls;
        let mon = if ctl.monitor_enabled.load(Ordering::Relaxed) { ctl.monitor_gain.load() } else { 0.0 };
        let sh = &self.p.bank.slots[LOCAL_SOURCE];
        sh.set_gains((mon, mon), (1.0, 1.0));
        let chunk_dev = (OUT_CHUNK as u64 * self.p.info.out_rate as u64).div_ceil(INTERNAL_RATE as u64);
        self.adapt_safety(chunk_dev);
        let target = self.out_burst_dev() + chunk_dev + self.safety;
        let fill = |w: &Self| (w.p.prods[LOCAL_SOURCE].occupied_len() / 2 + w.local_rs.available()) as u64;
        if fill(self) >= target {
            return;
        }
        // 한 번에 너무 많이 만들지 않도록 상한(장치 버퍼 4개분)
        let max_chunks = ((self.out_burst_dev() * 4) / chunk_dev.max(1) + 2) as usize;
        let mut produced = 0;
        while fill(self) < target && produced < max_chunks {
            self.local.pull(&mut self.local_buf);
            self.local_rs.push(&self.local_buf);
            let prod = &mut self.p.prods[LOCAL_SOURCE];
            loop {
                let room = (prod.vacant_len() / 2).min(self.out_tmp.len() / 2);
                let n = if room == 0 { 0 } else { self.local_rs.pop(&mut self.out_tmp[..room * 2]) };
                if n == 0 {
                    break;
                }
                prod.push_slice(&self.out_tmp[..n * 2]);
            }
            produced += 1;
        }
        if produced > 0 {
            self.local.end_burst();
            self.p.bank.slots[LOCAL_SOURCE].playing.store(true, Ordering::Relaxed);
            self.p.stats.local_corrections.store(self.local.corrections, Ordering::Relaxed);
            self.p.stats.local_underruns.store(self.local.underruns, Ordering::Relaxed);
        }
    }

    /// 모니터 경로 점유량(시간) 이동 평균(리뷰 M12).
    fn track_occupancy(&mut self) {
        let in48 = (self.in_rs.available() + self.local.frames()) as f64;
        let local_dev = (self.p.prods[LOCAL_SOURCE].occupied_len() / 2 + self.local_rs.available()) as f64;
        let out_us = local_dev * 1e6 / self.p.info.out_rate.max(1) as f64;
        let mon_us = in48 * 1e6 / INTERNAL_RATE as f64 + out_us;
        self.monitor_ewma_us += (mon_us - self.monitor_ewma_us) * 0.01;
        self.p.stats.monitor_occupancy_us.store(self.monitor_ewma_us as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[path = "worker_tune_sim.rs"]
mod tune_sim;

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
    fn device_clock_recovers_from_offset_step() {
        // 장치가 10ms 동안 콜백을 못 하고(정지·절전 복귀·카운터 결손) 이후 정상: 오프셋이 +10ms로 한 번 뛴다.
        // 이전 구현(전역 최솟값 + 500ppm 누설)은 회복에 20초가 걸려 그동안 깊이가 과소 측정됐다(PR #5 H-1).
        let mut c = DeviceClock::new(48_000);
        let mut errs = Vec::new();
        for k in 1..800u64 {
            let lost = if k > 200 { 480 } else { 0 }; // k=200 이후 카운터가 10ms 뒤처짐
            let cb_time = k * 10_000;
            let est = c.update(cb_time + 200, k * 480 - lost);
            // 추정은 "마지막 콜백 시각"이어야 한다: 카운터가 뒤처져도 실제 시각은 cb_time
            errs.push((k, est as i64 - cb_time as i64));
        }
        let just_after = errs.iter().find(|(k, _)| *k == 205).map(|e| e.1).unwrap_or(0);
        assert!(just_after < -9_000, "결손 직후에는 창이 지나기 전까지 이르게 추정: {just_after}");
        // 2.5초(250콜백) 뒤에는 오차 1ms 미만으로 돌아와야 한다(최솟값 창 2초)
        let worst = errs.iter().filter(|(k, _)| *k > 200 + 250).map(|e| e.1.abs()).max().unwrap_or(0);
        assert!(worst < 1000, "결손 후 2.5초 안에 재동기화: 최대 오차 {worst}µs");
    }

    #[test]
    fn device_clock_no_accumulation_with_repeated_deficits() {
        // 초당 1회 2.67ms 결손이 이어져도 추정 오차가 발산하지 않는다(리뷰 재현: 이전 구현은 110초에 +227ms).
        let mut c = DeviceClock::new(48_000);
        let mut lost_total = 0u64;
        let mut worst = 0i64;
        let mut late_mean = (0i64, 0i64);
        for k in 1..11_000u64 {
            if k % 100 == 0 {
                lost_total += 128;
            }
            let cb_time = k * 10_000;
            let est = c.update(cb_time + 150, k * 480 - lost_total);
            let err = est as i64 - cb_time as i64;
            if k > 300 {
                // 창(2초) 안의 결손만 반영된다: 오차는 최대 결손 3개분으로 묶이고 시간이 지나도 늘지 않는다
                worst = worst.max(err.abs());
            }
            if k > 10_000 {
                late_mean = (late_mean.0 + err, late_mean.1 + 1);
            }
        }
        assert!(worst < 3 * 2_667 + 500, "결손이 누적되면 안 됨: 최대 오차 {worst}µs");
        let mean = late_mean.0 / late_mean.1.max(1);
        assert!(mean.abs() < 6_000, "110초 뒤 평균 오차 {mean}µs (이전 구현은 +227ms까지 발산)");
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

    // ------------------------------------------------------------------------
    // 콜백 구동 믹스 결정적 시뮬레이션(P1-0): 송신 장치 → 패킷 → 지터 버퍼 → PeerFeed(워커) → OutputRt(콜백)
    // ------------------------------------------------------------------------

    use crate::engine::codec::Encoder as SimEncoder;
    use crate::engine::config::CodecKind;
    use crate::engine::jitter::{JitterSettings, JitterStats};
    use crate::engine::rt::output_path;
    use crate::net::packet::{Header, PacketType};

    #[derive(Clone, Copy)]
    struct FeedSim {
        in_buf: usize,
        out_buf: usize,
        frame: usize,
        out_phase_us: u64,
        seconds: f64,
        /// (주기 µs, 멈춤 µs, 끝 µs): 출력 콜백이 멈췄다가 몰아서 따라잡는다. 끝 시각 이후에는 멈추지 않는다.
        out_stall: Option<(u64, u64, u64)>,
        /// 워커 기상 지연 상한(µs).
        worker_jitter_us: u64,
        /// 출력 콜백이 격자보다 0..이 값(µs) 이르게 온다(실장비 WASAPI 독점 프로브: 최대 ≈1.7ms 이름, M3).
        out_early_us: u64,
        /// 입력·출력 장치 콜백이 0..이 값(µs) 늦게 깨어난다(가짜 장치 `wake_jitter_us`와 같은 모델:
        /// 다음 콜백은 원래 격자로 따라잡는다).
        dev_late_us: u64,
        /// 망 지연 지터 상한(µs, 기본 0.1ms).
        net_jitter_us: u64,
        /// 패킷 손실 확률(0..1).
        loss: f64,
        seed: u64,
    }

    struct FeedSimResult {
        js: JitterStats,
        /// 실행 끝의 피어 적응 여유(장치 프레임).
        margin: usize,
        /// 전반/후반 xrun 수(적응 효과 확인용).
        xruns_halves: (u64, u64),
        /// 4초 이후(지연 통계 창이 찬 뒤) 구간의 (은닉, 마감 채움, 보정) 증가량
        steady: (u64, u64, u64),
        skipped: u64,
        xruns: u64,
        /// (시각 s, 벽시계 버퍼 ms)
        wall_trace: Vec<(f64, f32)>,
        /// 4초(정착 구간 시작) 시점의 목표(ms).
        base_target_ms: f32,
        /// 이벤트 시각(µs): 은닉(지터 버퍼 concealed 증가), xrun(0 채움 콜백).
        conceal_us: Vec<u64>,
        xrun_us: Vec<u64>,
        /// 출력 L 채널(불연속 검사용, 2초 이후)
        out: Vec<f32>,
    }

    fn run_feed_sim(p: FeedSim) -> FeedSimResult {
        const RATE: f64 = 48_000.0;
        let mut rng = rand::rngs::StdRng::seed_from_u64(p.seed);
        let counters = Arc::new(AudioCounters::default());
        let controls = Arc::new(MixControls::default());
        let (mut rt, mut feed) = output_path(counters.clone(), controls);
        let slot = 1;
        let sh = &feed.bank.slots[slot];
        sh.set_gains((1.0, 1.0), (1.0, 1.0));
        sh.state.store(SLOT_ACTIVE, Ordering::Release);
        let mut pf = PeerFeed::new(slot, (0, 0), 48_000, sh).unwrap();
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
        let mut tracker = CallbackTracker::new(48_000);
        let mut enc = SimEncoder::new(CodecKind::Pcm16, 1, 0).unwrap();
        let fcode = frame_code_from_samples(p.frame).unwrap();
        let mut chunk = vec![0f32; OUT_CHUNK * 2];
        let mut tmp = vec![0f32; 8192];
        let mut dev = vec![0f32; p.out_buf * 2];
        let mut payload = vec![0u8; MAX_PAYLOAD_BYTES];

        // 송신 상태
        let (mut seq, mut ts, mut acc, mut phase) = (7u32, 1000u32, 0usize, 0.0f64);
        let mut pending: Vec<(u64, Header, Vec<u8>)> = Vec::new();
        let in_period = p.in_buf as f64 / RATE * 1e6;
        let out_period = p.out_buf as f64 / RATE * 1e6;
        let (mut k_in, mut k_out) = (1u64, 0u64);
        let mut t_w = 0u64;
        let end_us = (p.seconds * 1e6) as u64;
        let mut res = FeedSimResult {
            js: JitterStats::default(),
            margin: 0,
            xruns_halves: (0, 0),
            steady: (0, 0, 0),
            skipped: 0,
            xruns: 0,
            base_target_ms: 0.0,
            conceal_us: Vec::new(),
            xrun_us: Vec::new(),
            wall_trace: Vec::new(),
            out: Vec::new(),
        };
        let mut half_xruns: Option<u64> = None;
        let mut base: Option<(JitterStats, u64)> = None;
        let mut fills = 0u64;
        let mut seen_ev = (0u64, 0u64);
        let mut next_trace = 2_000_000u64;
        // 출력 콜백 k의 예정 시각(멈춤 구간에 걸리면 구간 끝으로 밀림 → 몰아서 따라잡음).
        // 이른 콜백: k마다 결정적 의사난수로 0..out_early_us만큼 앞당긴다(순서는 유지: 이름 < 주기).
        // 콜백 k의 결정적 의사난수 0..max(µs). salt로 출력 이름/출력 늦음/입력 늦음을 서로 독립으로 만든다.
        // (salt 0은 기존 이른 콜백 수열을 그대로 유지한다.)
        let jit = |k: u64, salt: u64, max: u64| -> u64 {
            if max == 0 {
                return 0;
            }
            let h = (k ^ p.seed ^ salt).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            (h >> 11) % max
        };
        let out_due = |k: u64| -> u64 {
            let early = jit(k, 0, p.out_early_us.min(out_period as u64 * 9 / 10));
            let late = jit(k, 0x5A5A_0000_0000, p.dev_late_us);
            let nominal = (p.out_phase_us + (k as f64 * out_period) as u64).saturating_sub(early) + late;
            match p.out_stall {
                Some((every, len, until)) if nominal % every < len && nominal > every && nominal < until => nominal - nominal % every + len,
                _ => nominal,
            }
        };
        loop {
            let t_in = (k_in as f64 * in_period) as u64 + jit(k_in, 0xA5A5_0000_0000, p.dev_late_us.min(in_period as u64 * 9 / 10));
            let t_out = out_due(k_out);
            let t_arr = pending.iter().map(|x| x.0).min().unwrap_or(u64::MAX);
            let t = t_in.min(t_out).min(t_arr).min(t_w);
            if t > end_us {
                break;
            }
            if t == t_arr {
                let i = pending.iter().position(|x| x.0 == t).unwrap();
                let (_, h, pl) = pending.swap_remove(i);
                jb.insert(&h, &pl, t);
            } else if t == t_in {
                // 입력 콜백: in_buf 샘플 캡처 → 프레임이 차면 패킷(망 지연 0.2ms + 0..0.1ms)
                acc += p.in_buf;
                while acc >= p.frame {
                    let pcm: Vec<f32> = (0..p.frame)
                        .map(|_| {
                            let v = (phase * 2.0 * std::f64::consts::PI).sin() as f32 * 0.5;
                            phase = (phase + 440.0 / RATE).fract();
                            v
                        })
                        .collect();
                    let n = enc.encode(&pcm, &mut payload).unwrap();
                    let h = Header { ptype: PacketType::Audio, ssrc: 1, seq, timestamp: ts, flags: 0, frame_code: fcode };
                    let delay = 200 + rng.gen_range(0..p.net_jitter_us.max(100));
                    if !(p.loss > 0.0 && rng.gen_bool(p.loss)) {
                        pending.push((t + delay, h, payload[..n].to_vec()));
                    }
                    seq = seq.wrapping_add(1);
                    ts = ts.wrapping_add(p.frame as u32);
                    acc -= p.frame;
                }
                k_in += 1;
            } else if t == t_out {
                rt.process(&mut dev, 2);
                if t >= 2_000_000 {
                    res.out.extend(dev.as_chunks::<2>().0.iter().map(|f| f[0]));
                }
                k_out += 1;
            } else {
                // 워커 반복(Worker::timing + service_peers와 같은 순서)
                let timing = tracker.observe(&counters, t, p.out_buf, 48_000);
                let ev = pf.service(&mut jb, &mut feed.prods[slot], &feed.bank.slots[slot], &timing, &|| true, &mut chunk, &mut tmp, None);
                fills += u64::from(ev.deadline_fill);
                res.skipped += ev.skipped_frames;
                let (c_now, x_now) = (jb.stats().concealed, counters.xruns.load(Ordering::Relaxed));
                if c_now > seen_ev.0 {
                    res.conceal_us.push(t);
                }
                if x_now > seen_ev.1 {
                    res.xrun_us.push(t);
                }
                seen_ev = (c_now, x_now);
                if t >= end_us / 2 && half_xruns.is_none() {
                    half_xruns = Some(counters.xruns.load(Ordering::Relaxed));
                }
                if t >= 4_000_000 && base.is_none() {
                    base = Some((jb.stats(), fills));
                }
                if t >= next_trace {
                    res.wall_trace.push((t as f64 / 1e6, jb.stats().wall_buffer_ms));
                    next_trace += 500_000;
                }
                // Worker::run과 같은 폴링: 은닉 마감이 가까우면(urgent) FINE_POLL, 아니면 POLL. 여기에 기상 지연.
                let poll = if ev.urgent { FINE_POLL } else { POLL }.as_micros() as u64;
                t_w = t + poll + if p.worker_jitter_us > 0 { rng.gen_range(0..p.worker_jitter_us) } else { 0 };
            }
        }
        res.js = jb.stats();
        let (b, f) = base.unwrap_or_default();
        res.steady = (res.js.concealed - b.concealed, fills - f, res.js.corrections() - b.corrections());
        res.base_target_ms = b.target_ms;
        res.xruns = counters.xruns.load(Ordering::Relaxed);
        let h = half_xruns.unwrap_or(res.xruns);
        res.xruns_halves = (h, res.xruns - h);
        res.margin = pf.margin();
        res
    }

    /// 피어 여유 `margin`(48k 장치 프레임)이 목표에 더하는 깊이(ms): 2·여유(P1-13 `PeerFeed::consumer_lead`, P1-5부터
    /// 소비자 요구 깊이 전체가 목표에 들어간다). 계약 단언용(PR #16 L3): 공식을 바꾸면 이 함수와 설계 문서를 같이 바꾼다.
    fn expected_extra_ms(margin: usize) -> f32 {
        2.0 * margin as f32 / 48.0
    }

    /// 출력 멈춤·이른 콜백 없이 같은 링크를 돌렸을 때의 목표(ms): 여유가 없을 때의 기본 목표.
    fn reference_target_ms(p: FeedSim) -> f32 {
        let r = run_feed_sim(FeedSim { out_stall: None, out_early_us: 0, worker_jitter_us: 300, ..p });
        assert_eq!(r.margin, 0, "기준 실행에는 여유가 없어야 함");
        r.js.target_ms
    }

    fn discontinuities(x: &[f32]) -> usize {
        x.windows(2).filter(|w| (w[1] - w[0]).abs() > 0.08).count()
    }

    #[test]
    fn callback_mix_sim_phase_and_buffer_matrix() {
        // 위상이 다른 입·출력(송신 입력 버퍼 ≠ 수신 출력 버퍼, 출력 위상 0..9ms), 워커 기상 지연 0..0.8ms.
        // 지터 없는 링크: 정착 후 은닉·마감 채움·보정 0, 출력 불연속 0, xrun 0.
        for (in_buf, out_buf, frame) in [(128, 128, 120), (480, 480, 120), (128, 480, 240), (480, 128, 120), (256, 1024, 240), (1024, 64, 480)] {
            for phase in [0u64, 1300, 4700, 9100] {
                let r = run_feed_sim(FeedSim {
                    in_buf,
                    out_buf,
                    frame,
                    out_phase_us: phase,
                    seconds: 10.0,
                    out_stall: None,
                    worker_jitter_us: 800,
                    out_early_us: 0,
                    dev_late_us: 0,
                    net_jitter_us: 0,
                    loss: 0.0,
                    seed: phase + in_buf as u64,
                });
                let d = discontinuities(&r.out);
                println!(
                    "sim in {in_buf} out {out_buf} F {frame} 위상 {phase}: 정착 후 은닉/마감/보정 {:?}, buffer {:.2}/{:.2}ms wall {:.2}ms, 불연속 {d}, xruns {}",
                    r.steady, r.js.buffer_ms, r.js.target_ms, r.js.wall_buffer_ms, r.xruns
                );
                assert_eq!(r.steady, (0, 0, 0), "in {in_buf} out {out_buf} F {frame} 위상 {phase}");
                assert_eq!(d, 0, "in {in_buf} out {out_buf} F {frame} 위상 {phase}: 불연속");
                assert_eq!(r.xruns, 0, "in {in_buf} out {out_buf} F {frame} 위상 {phase}: xrun");
                assert!(r.out.len() > 48_000 * 5);
            }
        }
    }

    #[test]
    fn callback_mix_sim_output_stalls_do_not_accumulate_latency() {
        // 출력 콜백이 1초마다 8ms 멈췄다 몰아서 따라잡는다 → 소스 링이 모자라 콜백이 0을 채운다(언더런).
        // 그만큼 스트림을 건너뛰지 않으면 매번 재생이 늦어져 지연이 쌓인다(초당 수 ms).
        for out_buf in [128usize, 256] {
            let p = FeedSim {
                in_buf: 128,
                out_buf,
                frame: 240,
                out_phase_us: 700,
                seconds: 20.0,
                out_stall: Some((1_000_000, 8_000, u64::MAX)),
                worker_jitter_us: 300,
                out_early_us: 0,
                dev_late_us: 0,
                net_jitter_us: 0,
                loss: 0.0,
                seed: 5,
            };
            let r = run_feed_sim(p);
            let mean = |a: f64, b: f64| {
                let v: Vec<f32> = r.wall_trace.iter().filter(|x| x.0 >= a && x.0 < b).map(|x| x.1).collect();
                v.iter().sum::<f32>() / v.len().max(1) as f32
            };
            let (early, late) = (mean(3.0, 7.0), mean(15.0, 20.0));
            println!(
                "sim stall out {out_buf}: xruns {}, 건너뜀 {} 프레임, 벽시계 버퍼 3~7s {early:.2}ms → 15~20s {late:.2}ms, 목표 {:.2}ms, 은닉 {}, 정착 후 보정 {}",
                r.xruns, r.skipped, r.js.target_ms, r.js.concealed, r.steady.2
            );
            // 멈춤이 실제로 언더런을 일으킨다. 첫 언더런 뒤에는 피어 적응 여유(M4)가 늘어 같은 멈춤의 일부를 흡수한다.
            assert!(r.xruns >= 3, "멈춤이 실제로 언더런을 일으켜야 함: {}", r.xruns);
            assert!(r.margin > 0, "언더런 뒤 적응 여유가 늘어야 함");
            // 0 채움은 건너뛰거나(여유가 상한일 때) 늘어난 목표 깊이로 남긴다(`skipped`는 실제로 건너뛴 양만, PR #16 L2).
            // 어느 쪽이든 지연이 쌓이지 않는 것은 아래 벽시계 버퍼로 확인한다.
            // 건너뛰기가 타이밍을 바로 되돌리므로 버림 보정이 필요 없다. 건너뛰지 않으면 매 멈춤마다 늦어진 만큼을
            // 패킷당 1~2샘플씩 버려 되돌린다(보정 수백 회, 버퍼는 목표 위로 치우침). 여유가 늘면 목표도 깊어지는데
            // (P1-13, `expected_extra_ms`) 그 깊이는 0 채움으로 이미 늦어진 만큼을 건너뛰지 않고 남겨 만든다. 남길 0 채움이
            // 모자랄 때만 삽입 보정으로 채우므로 보정 총량은 늘어난 목표(샘플)를 넘지 않는다.
            let extra_ms = expected_extra_ms(r.margin);
            assert!(r.steady.2 as f32 <= extra_ms * 48.0 + 1.0, "보정 {}은 늘어난 목표 {extra_ms:.2}ms를 넘지 않아야 함", r.steady.2);
            assert!((r.js.lead_ms - 1.5 - extra_ms).abs() < 0.01, "lead = 1.5ms + 2·여유(계약): {:.2}", r.js.lead_ms);
            // 지연은 쌓이지 않는다: 기본 목표(멈춤 없는 같은 링크)는 그대로이고 늘어난 것은 여유분뿐이다.
            let base = reference_target_ms(p);
            assert!((r.js.target_ms - extra_ms - base).abs() < 0.3, "기본 목표 불변: {:.2} − 여유 {extra_ms:.2} vs {base:.2}", r.js.target_ms);
            assert!(late - early < extra_ms + 1.5, "지연 누적: {early:.2} → {late:.2}ms (여유 {extra_ms:.2}ms)");
            assert!((late - r.js.target_ms).abs() < 3.0, "버퍼가 목표 근처: {late:.2} vs {:.2}", r.js.target_ms);
        }
    }

    #[test]
    fn callback_mix_sim_early_callbacks_no_premature_concealment() {
        // 실장비 WASAPI 독점처럼 출력 콜백이 격자보다 최대 1.7ms 이르게 온다(PR #11 M3). 망 지터 없는 링크:
        // 실제 데이터가 선행량(다음 콜백 + 2청크)으로 미리 있으므로 정착 후 은닉·마감 채움·보정 0, 불연속 0.
        for (in_buf, out_buf, frame) in [(128, 128, 120), (128, 144, 120), (96, 144, 240), (480, 480, 240)] {
            for phase in [0u64, 2100] {
                let r = run_feed_sim(FeedSim {
                    in_buf,
                    out_buf,
                    frame,
                    out_phase_us: phase,
                    seconds: 10.0,
                    out_stall: None,
                    worker_jitter_us: 600,
                    out_early_us: 1700,
                    dev_late_us: 0,
                    net_jitter_us: 0,
                    loss: 0.0,
                    seed: 11 + phase + out_buf as u64,
                });
                let d = discontinuities(&r.out);
                println!(
                    "sim early in {in_buf} out {out_buf} F {frame} 위상 {phase}: 정착 후 은닉/마감/보정 {:?}, 건너뜀 {}, xruns {}, 여유 {}, buffer {:.2}/{:.2}ms, 불연속 {d}, xrun 시각 {:?} 은닉 시각 {:?}",
                    r.steady, r.skipped, r.xruns, r.margin, r.js.buffer_ms, r.js.target_ms, r.xrun_us, r.conceal_us
                );
                assert_eq!(r.steady, (0, 0, 0), "in {in_buf} out {out_buf} 위상 {phase}");
                assert_eq!(d, 0, "in {in_buf} out {out_buf} 위상 {phase}: 불연속");
                assert_eq!(r.xruns, 0, "in {in_buf} out {out_buf} 위상 {phase}: xrun");
            }
        }
    }

    #[test]
    fn callback_mix_sim_early_callbacks_with_loss_conceal_without_zero_fill() {
        // 이른 콜백(최대 1.7ms) + 손실 5%(PR #11 M3): 빠진 패킷은 은닉 마감(다음 콜백 예상 − 1.5ms)에 PLC로
        // 채운다. 콜백이 마감보다 이르게 와도 선행량(다음 콜백 + 2청크)이 남아 있어야 하므로 0 채움(언더런)·
        // 건너뛰기가 없어야 하고, 적응 여유(M4)도 늘지 않는다(기본 상태 지연 불변). 은닉은 손실만큼 생긴다.
        for out_buf in [128usize, 144] {
            let r = run_feed_sim(FeedSim {
                in_buf: 128,
                out_buf,
                frame: 120,
                out_phase_us: 900,
                seconds: 30.0,
                out_stall: None,
                worker_jitter_us: 600,
                out_early_us: 1700,
                dev_late_us: 0,
                net_jitter_us: 0,
                loss: 0.05,
                seed: 3 + out_buf as u64,
            });
            println!(
                "sim early+loss out {out_buf}: xruns 전반 {} / 후반 {}, 건너뜀 {}, 여유 {}프레임, 은닉 {}, buffer {:.2}/{:.2}ms wall {:.2}ms",
                r.xruns_halves.0, r.xruns_halves.1, r.skipped, r.margin, r.js.concealed, r.js.buffer_ms, r.js.target_ms, r.js.wall_buffer_ms
            );
            assert!(r.js.concealed > 0, "손실이 은닉되어야 함");
            assert_eq!((r.xruns, r.skipped), (0, 0), "이른 콜백에 0 채움 없음");
            assert_eq!(r.margin, 0, "언더런이 없으면 여유도 0");
            assert!((r.js.wall_buffer_ms - r.js.target_ms).abs() < 4.0, "버퍼가 목표 근처");
        }
    }

    // ------------------------------------------------------------------------
    // 실시간 통합 테스트(tests/loopback.rs, `#[ignore]` + CI realtime 잡)의 결정적 짝.
    // 같은 장치 버퍼·프레임·위상·이른 콜백·기상 지연을 가상 시간으로 돌린다. 스케줄러가 끼어들 수 없으므로
    // 허용치 없이 정확히 0을 요구한다: 이 테스트가 깨지면 엔진 회귀이고, 이 테스트가 녹색인데 실시간 짝만
    // 깨지면 실행 환경(러너 스케줄링)이 원인이다(P3-0).
    // ------------------------------------------------------------------------

    /// `with_corrections`: 정착 후 깊이 보정도 0이어야 하는가. 입력 장치 기상 지연(최대 1ms)이 크면 패킷 도착 p95가
    /// 프레임 경계를 넘나들어 목표가 다시 학습되고(정당한 보정, link_matrix 주석 참고) 실시간 짝도 보정은 세지 않는다.
    fn assert_clean_link(r: &FeedSimResult, label: &str, with_corrections: bool) {
        let d = discontinuities(&r.out);
        println!(
            "{label}: 정착 후 은닉/마감/보정 {:?}, 건너뜀 {}, xruns {}, 여유 {}, buffer {:.2}/{:.2}ms(4s 목표 {:.2}) wall {:.2}ms, 불연속 {d}",
            r.steady, r.skipped, r.xruns, r.margin, r.js.buffer_ms, r.js.target_ms, r.base_target_ms, r.js.wall_buffer_ms
        );
        assert_eq!((r.steady.0, r.steady.1), (0, 0), "{label}: 정착 후 은닉/마감 채움");
        if with_corrections {
            assert_eq!(r.steady.2, 0, "{label}: 정착 후 보정");
        }
        assert_eq!(d, 0, "{label}: 불연속");
        assert_eq!(r.xruns, 0, "{label}: xrun");
        assert!(r.out.len() > 48_000 * 5, "{label}: 출력 기록 부족");
    }

    #[test]
    fn callback_mix_sim_mirrors_realtime_phase_sweep() {
        // 짝: tests/loopback.rs callback_mix_phase_sweep_no_premature_concealment
        // (buf, frame_ms) ∈ {(480, 2.5), (441, 5), (128, 2.5)} × 출력 위상 {0, 3, 7.5ms}, 장치 기상 지연 0..1ms.
        for (buf, frame) in [(480usize, 120usize), (441, 240), (128, 120)] {
            for phase in [0u64, 3000, 7500] {
                let r = run_feed_sim(FeedSim {
                    in_buf: buf,
                    out_buf: buf,
                    frame,
                    out_phase_us: phase,
                    seconds: 10.0,
                    out_stall: None,
                    worker_jitter_us: 800,
                    out_early_us: 0,
                    dev_late_us: 1000,
                    net_jitter_us: 0,
                    loss: 0.0,
                    seed: 9 + phase,
                });
                assert_clean_link(&r, &format!("sim≡RT 위상 스윕 buf {buf} F {frame} 위상 {phase}"), false);
            }
        }
    }

    #[test]
    fn callback_mix_sim_mirrors_realtime_early_callbacks() {
        // 짝: tests/loopback.rs callback_mix_early_callbacks_real_time
        // 송신 입력 96, 수신 출력 144/128, 프레임 2.5ms, 출력 이른 콜백 0..1.7ms + 늦은 기상 0..0.5ms.
        for (out_buf, phase) in [(144usize, 0u64), (144, 1800), (128, 900)] {
            let r = run_feed_sim(FeedSim {
                in_buf: 96,
                out_buf,
                frame: 120,
                out_phase_us: phase,
                seconds: 10.0,
                out_stall: None,
                worker_jitter_us: 600,
                out_early_us: 1700,
                dev_late_us: 500,
                net_jitter_us: 0,
                loss: 0.0,
                seed: 21 + phase,
            });
            assert_clean_link(&r, &format!("sim≡RT 이른 콜백 out {out_buf} 위상 {phase}"), true);
        }
    }

    #[test]
    fn callback_mix_sim_margin_after_underrun_no_steady_concealment() {
        // P1-13 회귀(P3-0에서 발견): 실시간 짝(callback_mix_early_callbacks_real_time, out 144 / 위상 1.8ms)이 CI와
        // 로컬에서 간헐 실패하던 원인의 결정적 재현. 이른 콜백(0..1.7ms) + 워커 기상 지연 0..1.3ms → 언더런 1회 →
        // 여유 +64프레임. 여유만큼 need·lead_cap·은닉 마감을 앞당기는데 지터 목표가 그대로면 마감에 실제 데이터가
        // 없어 은닉(마감 채움)하고 깊이 보정이 수백 회 따라붙었다(수정 전: 정착 후 은닉/마감/보정 (6, 6, 716),
        // buffer 5.28/5.00ms). 수정: 지터 목표에 여유를 반영(`PeerFeed::consumer_lead`).
        let r = run_feed_sim(FeedSim {
            in_buf: 96,
            out_buf: 144,
            frame: 120,
            out_phase_us: 1800,
            seconds: 10.0,
            out_stall: None,
            worker_jitter_us: 1300,
            out_early_us: 1700,
            dev_late_us: 500,
            net_jitter_us: 0,
            loss: 0.0,
            seed: 21 + 1800,
        });
        println!(
            "여유 후 정착: 은닉/마감/보정 {:?}, 건너뜀 {}, xruns {}, 여유 {}, buffer {:.2}/{:.2}ms, 은닉 시각 {:?}, xrun 시각 {:?}",
            r.steady, r.skipped, r.xruns, r.margin, r.js.buffer_ms, r.js.target_ms, r.conceal_us, r.xrun_us
        );
        assert!(r.xruns >= 1 && r.margin > 0, "전제: 언더런이 나서 여유가 늘어야 함");
        assert_eq!((r.steady.0, r.steady.1), (0, 0), "여유가 늘어난 뒤 정착 구간 은닉/마감 채움");
        let extra_ms = expected_extra_ms(r.margin);
        assert!((r.js.lead_ms - 1.5 - extra_ms).abs() < 0.01, "lead = 1.5ms + 2·여유(계약): {:.2}", r.js.lead_ms);
        assert!(r.js.target_ms >= r.js.jitter_est_ms + r.js.lead_ms, "목표 ≥ 지터 추정 + lead: {:?}", r.js);
        assert!((r.js.wall_buffer_ms - r.js.target_ms).abs() < 2.0, "버퍼가 새 목표를 따라감: {:.2}/{:.2}", r.js.wall_buffer_ms, r.js.target_ms);
    }

    /// 언더런 순간(그 콜백의 은닉)을 빼고, 언더런 뒤 `settle_us`가 지난 다음에 일어난 은닉 수.
    fn concealments_after_underruns(r: &FeedSimResult, settle_us: u64) -> usize {
        r.conceal_us
            .iter()
            .filter(|&&c| c >= 2_000_000 && !r.xrun_us.iter().any(|&x| c + 1_000 >= x && c < x + settle_us))
            .count()
    }

    #[test]
    fn callback_mix_sim_margin_sweep_no_concealment_after_underrun() {
        // P1-13: 위 재현을 위상·워커 기상 지연·시드로 넓혀 본다. 언더런이 나서 여유가 는 실행마다, 언더런 순간을
        // 빼면(여유 학습 자체는 허용) 은닉이 0이어야 한다. 목표에 여유를 1배만 더하면 64실행 중 여러 실행에서
        // 언더런 수 초 뒤에 은닉이 났다(마감이 여유만큼 앞당겨지고 채울 양도 여유만큼 커서 2배가 필요).
        // PR #16 M1(P1-5): 출력 멈춤 축(4초까지 1초마다 6ms)을 더해 언더런 2회 이상 → 여유 128 이상도 넣는다. 제외 창은
        // 언더런 뒤 200ms(이전 기준, 단언)와 3ms(그 콜백 한 주기, 수치 기록) 두 가지로 센다.
        let (mut runs, mut learned, mut big) = (0, 0, 0);
        let (mut win200, mut win3, mut xr) = (0usize, 0usize, 0u64);
        for out_buf in [144usize, 128] {
            for phase in [0u64, 900, 1800, 2500] {
                for (wj, stall) in [(1300u64, None), (1700, None), (1300, Some((1_000_000u64, 6_000u64, 4_500_000u64)))] {
                    for seed in 0..4u64 {
                        let r = run_feed_sim(FeedSim {
                            in_buf: 96,
                            out_buf,
                            frame: 120,
                            out_phase_us: phase,
                            seconds: 10.0,
                            out_stall: stall,
                            worker_jitter_us: wj,
                            out_early_us: 1700,
                            dev_late_us: 500,
                            net_jitter_us: 0,
                            loss: 0.0,
                            seed: seed * 1000 + phase + wj,
                        });
                        runs += 1;
                        let label = format!("out {out_buf} 위상 {phase} 기상 {wj} 멈춤 {} 시드 {seed}", stall.is_some());
                        if r.margin > 0 || r.xruns > 0 {
                            learned += 1;
                            big += usize::from(r.xruns >= 2);
                            let late = concealments_after_underruns(&r, 200_000);
                            win200 += concealments_after_underruns(&r, 3_000) - late;
                            win3 += r.conceal_us.iter().filter(|&&c| c >= 2_000_000).count() - concealments_after_underruns(&r, 3_000);
                            xr += r.xruns;
                            println!(
                                "{label}: 여유 {}, xruns {}, 은닉 시각 {:?}, xrun 시각 {:?}, 정착 후 {:?}, buffer {:.2}/{:.2}ms",
                                r.margin, r.xruns, r.conceal_us, r.xrun_us, r.steady, r.js.buffer_ms, r.js.target_ms
                            );
                            assert_eq!(late, 0, "{label}: 언더런 뒤 은닉");
                        } else {
                            assert_eq!(r.steady.0, 0, "{label}: 언더런 없는 실행의 정착 후 은닉 (시각 {:?})", r.conceal_us);
                        }
                    }
                }
            }
        }
        println!(
            "여유 스윕: {runs}실행 중 여유 학습 {learned}(언더런 2회 이상 {big}), 2초 이후 xrun {xr}, 은닉: 언더런 콜백(3ms 창) {win3}, 3ms~200ms 창 {win200}, 200ms 뒤 0"
        );
        assert!(learned >= 10, "스윕이 여유 학습을 충분히 일으켜야 함: {learned}");
        assert!(big >= 10, "멈춤 축이 언더런 2회 이상(여유 128 이상)을 일으켜야 함: {big}");
        // 언더런 직후 3ms~200ms 창의 은닉(이전에는 이 창에 가려졌다, M1)
        assert_eq!(win200, 0, "언더런 직후 은닉(3ms~200ms 창)");
    }

    #[test]
    fn callback_mix_sim_margin_decays_back_to_base_latency() {
        // P1-13: 여유가 목표에 들어가므로 지연이 영구히 커지면 안 된다. 처음 6초 동안만 1초마다 8ms 멈춤(언더런)을
        // 주고 그 뒤 멈춤을 없앤다. 마지막 언더런 뒤 10초 유지, 그 뒤 2초마다 청크씩 줄어(P1-5, PR #16 M3) 여유 0,
        // 목표 = 기본 목표, 벽시계 버퍼도 원래 수준으로 돌아와야 한다. 내려오는 동안 은닉·불연속·xrun 없음.
        // out 480(10ms, WASAPI 공유 수준): 이전에는 여유가 480까지 올라 +19.6ms, 복귀 75초 이상이었다.
        for out_buf in [128usize, 256, 480] {
            let p = FeedSim {
                in_buf: 128,
                out_buf,
                frame: 240,
                out_phase_us: 700,
                seconds: 40.0,
                // 멈춤은 출력 2.5주기 이상(out 480이면 25ms): 짧으면 큰 버퍼에서는 언더런이 나지 않는다.
                out_stall: Some((1_000_000, (out_buf as u64 * 1_000_000 / 48_000 * 5 / 2).max(8_000), 6_000_000)),
                worker_jitter_us: 300,
                out_early_us: 0,
                dev_late_us: 0,
                net_jitter_us: 0,
                loss: 0.0,
                seed: 5,
            };
            let r = run_feed_sim(p);
            let base = reference_target_ms(p);
            let mean = |a: f64, b: f64| {
                let v: Vec<f32> = r.wall_trace.iter().filter(|x| x.0 >= a && x.0 < b).map(|x| x.1).collect();
                v.iter().sum::<f32>() / v.len().max(1) as f32
            };
            let (peak, end) = (mean(6.5, 9.0), mean(35.0, 40.0));
            let last_xrun = r.xrun_us.last().copied().unwrap_or(0);
            let conc_after = r.conceal_us.iter().filter(|&&c| c > last_xrun + 1_000).count();
            let d = discontinuities(&r.out[(7 * 48_000).min(r.out.len())..]);
            println!(
                "여유 감소 out {out_buf}: xruns {} (마지막 {:.2}s), 여유 {}, 버퍼 6.5~9s {peak:.2}ms → 45~50s {end:.2}ms, 목표 {:.2}ms, 마지막 xrun 뒤 은닉 {conc_after}, 7s 이후 불연속 {d}",
                r.xruns,
                last_xrun as f64 / 1e6,
                r.margin,
                r.js.target_ms,
            );
            assert!(r.xruns >= 2 && last_xrun < 6_100_000, "언더런은 멈춤 구간에만: {:?}", r.xrun_us);
            let back = r.wall_trace.iter().filter(|x| x.1 > base + 1.0).map(|x| x.0).next_back().unwrap_or(0.0) - last_xrun as f64 / 1e6;
            println!("여유 감소 out {out_buf}: 기본 목표 {base:.2}ms, 최대 추가(6.5~9s) {:.2}ms, 마지막 언더런 → 버퍼 복귀(기본+1ms) {back:.1}s", peak - base);
            assert!(peak > base + 2.0, "멈춤 구간 뒤에는 여유만큼 지연이 커져 있어야 함(전제): {peak:.2}");
            // 상한: 추가분 2·여유 ≤ 1프레임 + 2ms(+ 데드밴드·측정 오차 1ms)
            assert!(peak - base <= 5.0 + 2.0 + 1.0, "추가 지연 상한: {:.2}ms", peak - base);
            assert!(back <= 20.0, "복귀 시간 {back:.1}s");
            assert_eq!(r.margin, 0, "언더런이 사라지면 여유도 0으로");
            assert!((r.js.target_ms - base).abs() < 0.3, "목표가 기본 목표로 복귀: {:.2} vs {base:.2}", r.js.target_ms);
            assert!((end - base).abs() < 1.5, "벽시계 버퍼가 원래 수준으로 복귀: {end:.2} vs {base:.2}");
            assert_eq!(conc_after, 0, "여유가 줄어드는 동안 은닉");
            assert_eq!(d, 0, "여유가 줄어드는 동안 불연속");
        }
    }
}
