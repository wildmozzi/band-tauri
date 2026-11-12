//! 로컬 입력 FIFO(모니터 + 믹스 탭): 입력 클럭으로 들어와 출력 클럭으로 나간다.


use std::collections::VecDeque;

use super::super::jitter::shift_one;
use super::OUT_CHUNK;

// ============================================================================
// 로컬 입력 FIFO (모니터 + 믹스 탭)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalMode {
    Idle,
    Drop,
    Insert,
}

pub(super) fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

pub(super) fn lcm(a: usize, b: usize) -> usize {
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
