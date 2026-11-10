//! 피어별 적응형 지터 버퍼 + 드리프트 보정 + PLC (설계 문서 1.2절).
//!
//! ## 사용법(워커)
//! 출력 콜백이 장치 버퍼를 소비하면 워커가 같은 양을 작은 청크로 연달아 `pop`한다(버스트).
//! 버스트가 끝나면 `end_burst(now, wall)`를 한 번 부른다. 깊이 측정·재생 시작·보정 판단은 모두
//! `end_burst`에서만 한다. `now`는 워커가 깨어난 시각이 아니라 **출력 콜백 시각 추정치**
//! (장치 클럭)다. 워커 기상 지연이 깊이 측정에 섞이면 제어가 발산한다(리뷰 M2/M4, CI 실패 원인).
//!
//! ## 깊이(depth)의 정의
//! 패킷 i의 `transit_i = arrival_i - ts_i`(샘플)의 최근 200개 최솟값을 `min_transit`라 할 때
//!
//! ```text
//! depth = now - ts(다음에 꺼낼 샘플) - min_transit      (버스트 직후 측정)
//! ```
//!
//! 버스트 직후가 한 주기 중 가장 얕은 시점이므로, 깊이가 목표 이상이면 다음 버스트도 채워진다.
//!
//! ## 목표 깊이
//! 상대 지연 `transit - min_transit`의 95퍼센타일(최근 200개)을 p95라 할 때
//! `target = clamp(ceil(p95 / F) + 1, min_frames, max_frames) × F` (F = 피어 패킷 프레임).
//! 목표는 바로 올리되, 내리는 것은 더 낮은 후보가 2초 동안 유지될 때만 한다(목표 떨림 방지).
//!
//! ## 보정 (데드밴드 + 히스테리시스)
//! 오차 `e = depth_ewma - target`에 대해
//! - 대기 상태에서 `e > enter`면 버림 모드, `e < -enter`면 삽입 모드로 들어간다.
//! - 버림/삽입 모드에서는 디코드한 패킷마다 1샘플을 버리거나 넣는다(프레임 전체에 걸친 램프).
//! - `|e| < exit`가 되면 대기로 돌아간다. `enter = max(F/2, 1ms)`, `exit = max(F/8, 4샘플)`.
//! - 실제로 쌓인 패킷이 목표보다 3F 이상 많고 e ≥ 3F이면 패킷 단위로 버린다. 버리는 패킷도
//!   디코드해 디코더 상태를 유지하고, 다음 프레임과 짧게 크로스페이드한다.
//!
//! ## 손실/지연
//! - 다음 seq가 없고 그 뒤 seq가 이미 있으면 손실로 보고 은닉(Opus: 다음 패킷 FEC → 없으면
//!   PLC, PCM: 직전 프레임 페이드아웃 1회 후 무음) 후 seq 전진.
//! - 다음 seq도 그 뒤도 없으면(언더런) 은닉 프레임을 끼워 넣되 seq는 전진하지 않는다. 늦게 온
//!   패킷을 그대로 재생하므로 깊이가 늘어난다. 200ms 이상 지속되면 재버퍼링.
//! - 이미 재생 지난 seq는 폐기하고 `late` 증가.

use std::collections::VecDeque;

use super::codec::Decoder;
use super::config::{frame_samples_from_code, INTERNAL_RATE, MAX_PAYLOAD_BYTES};
use crate::net::packet::Header;

/// 슬롯 링 크기: 2의 거듭제곱(u32 seq 랩에서도 슬롯 충돌 없음, 리뷰 L1). 2.5ms 기준 2.56초.
const SLOT_COUNT: usize = 1024;
const SLOT_MASK: u32 = SLOT_COUNT as u32 - 1;
/// 지연 통계 창 크기.
const DELAY_WINDOW: usize = 200;
/// p95/최솟값 재계산 주기(패킷 수).
const STATS_EVERY: usize = 10;
/// 깊이 EWMA 계수(버스트당 1회 측정).
const DEPTH_ALPHA: f64 = 0.05;
/// 최대 패킷 프레임(10ms).
const MAX_PKT_FRAME: usize = 480;
/// 손실률 창: 최대 2000패킷(2.5ms × 5초).
const LOSS_WINDOW_MAX: usize = 2000;
/// 언더런 은닉이 이 시간(ms) 이상 지속되면 재버퍼링.
const STARVE_RESET_MS: f64 = 200.0;
/// 목표를 내리기 전 유지 시간(ms).
const TARGET_DECAY_MS: f64 = 2000.0;
/// 패킷 단위 버림 시 크로스페이드 길이(샘플).
const DROP_XFADE: usize = 48;
/// 벽시계 깊이 최솟값 버킷 길이(µs). 보고값은 직전+현재 버킷의 최솟값(0.5~1초).
const WALL_BUCKET_US: u64 = 500_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JitterSettings {
    /// 0이면 자동.
    pub min_ms: f32,
    pub max_ms: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct JitterStats {
    pub buffer_ms: f32,
    pub target_ms: f32,
    pub jitter_ms: f32,
    pub loss_pct: f32,
    pub late: u64,
    pub concealed: u64,
    /// 생산된 패킷 프레임 수(정상 디코드 + 은닉).
    pub produced: u64,
    pub dropped_frames: u64,
    pub samples_dropped: u64,
    pub samples_inserted: u64,
    pub rebuffers: u64,
    /// 벽시계로 잰 실제 버퍼 지연(최근 0.5~1초 최솟값, ms). 장치 클럭 추정과 독립적이다.
    pub wall_buffer_ms: f32,
}

impl JitterStats {
    /// 샘플/프레임 보정 총 횟수.
    pub fn corrections(&self) -> u64 {
        self.samples_dropped + self.samples_inserted + self.dropped_frames
    }
}

struct Slot {
    seq: u32,
    valid: bool,
    len: usize,
    flags: u8,
    frame: usize,
    ts: u32,
    data: Box<[u8]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// 첫 패킷 대기 또는 목표 깊이까지 채우는 중.
    Buffering,
    Playing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Correction {
    Idle,
    Drop,
    Insert,
}

pub struct JitterBuffer {
    settings: JitterSettings,
    slots: Vec<Slot>,
    state: State,
    next_seq: u32,
    has_stream: bool,
    highest: u32,

    decoder: Option<Decoder>,
    /// 디코드된 스테레오 인터리브 샘플 FIFO.
    fifo: VecDeque<f32>,
    /// FIFO 끝 샘플 다음 위치의 타임스탬프(확장).
    fifo_end_ts: i64,

    // 타임스탬프 언랩
    have_ts: bool,
    last_ts: u32,
    ts_ext_base: i64,

    // 지연 통계
    transits: [i64; DELAY_WINDOW],
    transit_count: usize,
    transit_pos: usize,
    since_stats: usize,
    scratch: Vec<i64>,
    min_transit: i64,
    p95_rel: f64,
    rfc_jitter: f64,
    last_transit: Option<i64>,

    // 목표(프레임 수) 및 하향 히스테리시스
    target_frames: f64,
    lower_for_samples: f64,

    depth_ewma: Option<f64>,
    correction: Correction,
    drop_frames_pending: usize,
    trimming: bool,
    corr_step: i32,
    wall_bucket_start: u64,
    wall_cur_min: f64,
    wall_prev_min: f64,
    pkt_frame: usize,
    pkt_channels: usize,

    // 은닉
    last_frame: Vec<f32>,
    last_frame_valid: bool,
    conceal_run: usize,
    starve_samples: usize,

    // 스크래치
    dec_buf: Vec<f32>,
    shift_buf: Vec<f32>,
    xfade_buf: Vec<f32>,
    xfade_len: usize,
    xfade_ch: usize,

    // 손실 창
    loss_ring: Vec<bool>,
    loss_pos: usize,
    loss_len: usize,
    loss_lost: usize,

    stats: JitterStats,
}

fn seq_diff(a: u32, b: u32) -> i32 {
    a.wrapping_sub(b) as i32
}

impl JitterBuffer {
    pub fn new(settings: JitterSettings) -> Self {
        let slots = (0..SLOT_COUNT)
            .map(|_| Slot {
                seq: 0,
                valid: false,
                len: 0,
                flags: 0,
                frame: 0,
                ts: 0,
                data: vec![0u8; MAX_PAYLOAD_BYTES].into_boxed_slice(),
            })
            .collect();
        Self {
            settings,
            slots,
            state: State::Buffering,
            next_seq: 0,
            has_stream: false,
            highest: 0,
            decoder: None,
            fifo: VecDeque::with_capacity(MAX_PKT_FRAME * 16 * 2),
            fifo_end_ts: 0,
            have_ts: false,
            last_ts: 0,
            ts_ext_base: 0,
            transits: [0; DELAY_WINDOW],
            transit_count: 0,
            transit_pos: 0,
            since_stats: 0,
            scratch: Vec::with_capacity(DELAY_WINDOW),
            min_transit: 0,
            p95_rel: 0.0,
            rfc_jitter: 0.0,
            last_transit: None,
            target_frames: 0.0,
            lower_for_samples: 0.0,
            depth_ewma: None,
            correction: Correction::Idle,
            drop_frames_pending: 0,
            trimming: false,
            corr_step: 1,
            wall_bucket_start: 0,
            wall_cur_min: f64::INFINITY,
            wall_prev_min: f64::INFINITY,
            pkt_frame: 240,
            pkt_channels: 1,
            last_frame: vec![0.0; MAX_PKT_FRAME * 2],
            last_frame_valid: false,
            conceal_run: 0,
            starve_samples: 0,
            dec_buf: vec![0.0; MAX_PKT_FRAME * 2],
            shift_buf: vec![0.0; (MAX_PKT_FRAME + 4) * 2],
            xfade_buf: vec![0.0; MAX_PKT_FRAME * 2],
            xfade_len: 0,
            xfade_ch: 0,
            loss_ring: vec![false; LOSS_WINDOW_MAX],
            loss_pos: 0,
            loss_len: 0,
            loss_lost: 0,
            stats: JitterStats::default(),
        }
    }

    fn samples_per_ms() -> f64 {
        INTERNAL_RATE as f64 / 1000.0
    }

    fn now_samples(now_us: u64) -> i64 {
        (now_us as i128 * INTERNAL_RATE as i128 / 1_000_000) as i64
    }

    fn unwrap_ts(&mut self, ts: u32) -> i64 {
        if !self.have_ts {
            self.have_ts = true;
            self.last_ts = ts;
            self.ts_ext_base = ts as i64;
            return self.ts_ext_base;
        }
        let d = ts.wrapping_sub(self.last_ts) as i32 as i64;
        let ext = self.ts_ext_base + d;
        if d > 0 {
            self.last_ts = ts;
            self.ts_ext_base = ext;
        }
        ext
    }

    /// 이미 본 타임스탬프 근처 값을 확장 타임스탬프로 변환(상태 변경 없음).
    fn ts_ext(&self, ts: u32) -> i64 {
        self.ts_ext_base + ts.wrapping_sub(self.last_ts) as i32 as i64
    }

    fn slot_index(seq: u32) -> usize {
        (seq & SLOT_MASK) as usize
    }

    fn reset_stream(&mut self) {
        for s in &mut self.slots {
            s.valid = false;
        }
        self.wall_cur_min = f64::INFINITY;
        self.wall_prev_min = f64::INFINITY;
        self.state = State::Buffering;
        self.has_stream = false;
        self.fifo.clear();
        self.depth_ewma = None;
        self.correction = Correction::Idle;
        self.drop_frames_pending = 0;
        self.trimming = false;
        self.conceal_run = 0;
        self.starve_samples = 0;
        self.last_frame_valid = false;
        self.xfade_len = 0;
    }

    /// 패킷 프레임 길이(샘플).
    fn frame(&self) -> usize {
        self.pkt_frame.max(1)
    }

    fn frame_limits(&self) -> (f64, f64) {
        let f = self.frame() as f64;
        let spm = Self::samples_per_ms();
        let min_f = if self.settings.min_ms > 0.0 {
            (self.settings.min_ms as f64 * spm / f).ceil().max(1.0)
        } else {
            1.0
        };
        let max_f = (self.settings.max_ms as f64 * spm / f).floor().max(min_f);
        (min_f, max_f)
    }

    pub fn target_samples(&self) -> f64 {
        let (min_f, max_f) = self.frame_limits();
        self.target_frames.clamp(min_f, max_f) * self.frame() as f64
    }

    fn update_target(&mut self, elapsed_samples: f64) {
        let f = self.frame() as f64;
        let (min_f, max_f) = self.frame_limits();
        // 프레임 경계 근처의 p95가 목표를 25↔30ms처럼 오가지 않도록 양자화에 히스테리시스(±F/4):
        // 올릴 때는 p95가 현재 용량을 F/4 넘게 초과해야, 내릴 때는 F/4 넘게 밑돌아야 한다.
        let h = f / 4.0;
        let up = (((self.p95_rel - h).max(0.0) / f).ceil() + 1.0).clamp(min_f, max_f);
        let down = (((self.p95_rel + h) / f).ceil() + 1.0).clamp(min_f, max_f);
        if self.target_frames == 0.0 {
            self.target_frames = ((self.p95_rel / f).ceil() + 1.0).clamp(min_f, max_f);
            return;
        }
        if up > self.target_frames {
            self.target_frames = up;
            self.lower_for_samples = 0.0;
        } else if down < self.target_frames {
            let cand = down;
            self.lower_for_samples += elapsed_samples;
            if self.lower_for_samples >= TARGET_DECAY_MS * Self::samples_per_ms() {
                self.target_frames = cand;
                self.lower_for_samples = 0.0;
            }
        } else {
            self.lower_for_samples = 0.0;
        }
    }

    /// 수신 패킷 삽입. `payload`는 복호화된 평문. `now_us`는 수신 시각.
    pub fn insert(&mut self, hdr: &Header, payload: &[u8], now_us: u64) {
        let Some(frame) = frame_samples_from_code(hdr.frame_code) else {
            return;
        };
        if payload.len() > MAX_PAYLOAD_BYTES {
            return;
        }
        let seq = hdr.seq;

        if self.has_stream {
            let d = seq_diff(seq, self.next_seq);
            if d < 0 {
                // 버퍼링 중에는 순서가 뒤바뀐 직전 패킷만 시작점을 당긴다(재전송으로 크게
                // 되돌리는 것을 막기 위해 목표 깊이 이내로 제한, 리뷰 L5).
                let back_limit = (self.target_frames.max(1.0) as i32 + 1).min(16);
                if self.state == State::Buffering && -d <= back_limit {
                    self.next_seq = seq;
                } else {
                    self.stats.late += 1;
                    self.record_delay(hdr.timestamp, now_us);
                    return;
                }
            } else if d as usize >= SLOT_COUNT {
                // 너무 먼 미래: 스트림 재시작으로 간주
                self.reset_stream();
            }
        }
        if !self.has_stream {
            self.has_stream = true;
            self.next_seq = seq;
            self.highest = seq;
            self.state = State::Buffering;
        }
        if seq_diff(seq, self.highest) > 0 {
            self.highest = seq;
        }

        let slot = &mut self.slots[Self::slot_index(seq)];
        if slot.valid && slot.seq == seq {
            return; // 중복
        }
        slot.seq = seq;
        slot.valid = true;
        slot.len = payload.len();
        slot.flags = hdr.flags;
        slot.frame = frame;
        slot.ts = hdr.timestamp;
        slot.data[..payload.len()].copy_from_slice(payload);
        self.pkt_frame = frame;

        self.record_delay(hdr.timestamp, now_us);
    }

    fn record_delay(&mut self, ts: u32, now_us: u64) {
        let ts_ext = self.unwrap_ts(ts);
        let transit = Self::now_samples(now_us) - ts_ext;
        if let Some(prev) = self.last_transit {
            let d = (transit - prev).abs() as f64;
            self.rfc_jitter += (d - self.rfc_jitter) / 16.0;
        }
        self.last_transit = Some(transit);
        self.transits[self.transit_pos] = transit;
        self.transit_pos = (self.transit_pos + 1) % DELAY_WINDOW;
        self.transit_count = (self.transit_count + 1).min(DELAY_WINDOW);
        self.since_stats += 1;
        if self.transit_count <= STATS_EVERY || self.since_stats >= STATS_EVERY {
            let elapsed = self.since_stats as f64 * self.frame() as f64;
            self.since_stats = 0;
            self.recompute_delay_stats();
            self.update_target(elapsed);
        } else if transit < self.min_transit {
            self.min_transit = transit;
        }
    }

    fn recompute_delay_stats(&mut self) {
        let n = self.transit_count;
        if n == 0 {
            return;
        }
        self.scratch.clear();
        self.scratch.extend_from_slice(&self.transits[..n]);
        let min = self.scratch.iter().copied().min().unwrap_or(0);
        let k = ((n as f64 * 0.95).ceil() as usize).clamp(1, n) - 1;
        let (_, p, _) = self.scratch.select_nth_unstable(k);
        self.p95_rel = (*p - min) as f64;
        self.min_transit = min;
    }

    /// 현재 재생 위치 기준 깊이(샘플).
    fn depth_now(&self, now_us: u64) -> f64 {
        let play_ts = self.fifo_end_ts - (self.fifo.len() / 2) as i64;
        (Self::now_samples(now_us) - play_ts - self.min_transit) as f64
    }

    /// 다음 seq의 시작 타임스탬프 기준 깊이(버퍼링 중 판단용).
    fn depth_if_start(&self, now_us: u64) -> Option<f64> {
        let slot = &self.slots[Self::slot_index(self.next_seq)];
        if !(slot.valid && slot.seq == self.next_seq) {
            return None;
        }
        Some((Self::now_samples(now_us) - self.ts_ext(slot.ts) - self.min_transit) as f64)
    }

    /// 실제로 받아 둔(또는 그 사이 빈) 샘플 수: 다음 seq부터 최고 seq까지 + FIFO.
    fn queued_samples(&self) -> f64 {
        let span = seq_diff(self.highest, self.next_seq) + 1;
        span.max(0) as f64 * self.frame() as f64 + (self.fifo.len() / 2) as f64
    }

    /// 스테레오 인터리브로 `out.len() / 2` 프레임을 채운다. 버퍼링 중이면 무음.
    pub fn pop(&mut self, out: &mut [f32]) {
        let need = out.len() / 2;
        if self.state == State::Buffering {
            out.fill(0.0);
            return;
        }
        while self.fifo.len() / 2 < need {
            if !self.produce_packet() {
                break;
            }
        }
        let avail = (self.fifo.len() / 2).min(need);
        for (o, s) in out[..avail * 2].iter_mut().zip(self.fifo.drain(..avail * 2)) {
            *o = s;
        }
        out[avail * 2..].fill(0.0);
    }

    /// 출력 버스트 종료. `now_us`는 출력 콜백 시각 추정치(장치 클럭 기준).
    /// `wall_us`는 워커가 실제로 깨어난 벽시계 시각이다. 이것으로 잰 깊이는 기상 지연만큼 크게 나오므로,
    /// 1초 창의 최솟값이 장치 클럭 추정과 **독립적인** 실제 재생 지연이 된다(PR #5 H-1: 클럭 추정이
    /// 틀려 지연이 몰래 늘어나는 경우를 통계로 드러낸다).
    pub fn end_burst(&mut self, now_us: u64, wall_us: u64) {
        match self.state {
            State::Buffering => self.try_start(now_us),
            State::Playing => {
                let dw = self.depth_now(wall_us);
                if wall_us.saturating_sub(self.wall_bucket_start) >= WALL_BUCKET_US {
                    self.wall_prev_min = self.wall_cur_min;
                    self.wall_cur_min = f64::INFINITY;
                    self.wall_bucket_start = wall_us;
                }
                self.wall_cur_min = self.wall_cur_min.min(dw);
                let d = self.depth_now(now_us);
                let e = match self.depth_ewma {
                    None => d,
                    Some(e) => e + (d - e) * DEPTH_ALPHA,
                };
                self.depth_ewma = Some(e);
                self.update_correction(e);
            }
        }
    }

    fn update_correction(&mut self, ewma: f64) {
        let f = self.frame() as f64;
        let target = self.target_samples();
        let err = ewma - target;
        // 데드밴드: F/2를 1ms~2.5ms로 제한(10ms 프레임에서 목표 ±5ms까지 머물지 않게, PR #5 L-1)
        let spm = Self::samples_per_ms();
        let enter = (f / 2.0).clamp(spm, 2.5 * spm);
        let exit = (enter / 4.0).max(4.0);
        // 오차가 한 프레임을 넘으면(목표 상향 직후 등) 패킷당 2샘플로 빨리 따라간다(PR #5 M-1).
        // 국소 피치 편차는 2/F(240샘플이면 약 14센트), 오차가 1프레임 안으로 들어오면 1샘플로 돌아온다.
        self.corr_step = if err.abs() > f { 2 } else { 1 };
        self.correction = match self.correction {
            Correction::Idle if err > enter => Correction::Drop,
            Correction::Idle if err < -enter => Correction::Insert,
            Correction::Drop if err <= exit => Correction::Idle,
            Correction::Insert if err >= -exit => Correction::Idle,
            c => c,
        };
        // 패킷 단위 버림(히스테리시스): 추정 깊이와 실제 쌓인 양이 모두 목표를 3F 이상 넘으면
        // 시작하고, 둘 중 하나가 목표 + F 안으로 들어오면 멈춘다. 버스트당 최대 1패킷.
        let queued_excess = self.queued_samples() - target;
        if !self.trimming && err >= 3.0 * f && queued_excess >= 3.0 * f {
            self.trimming = true;
        } else if self.trimming && (err < f || queued_excess < f) {
            self.trimming = false;
        }
        self.drop_frames_pending = usize::from(self.trimming);
    }

    /// 버퍼링 → 재생 전환. 시작 깊이를 목표에 정확히 맞춘다(초과분은 건너뜀).
    fn try_start(&mut self, now_us: u64) {
        self.skip_missing_head();
        let target = self.target_samples();
        let Some(depth) = self.depth_if_start(now_us) else {
            return;
        };
        if depth < target {
            return;
        }
        let f = self.frame();
        let mut excess = depth - target;
        // 통째로 건너뛸 수 있는 패킷(뒤 패킷이 이미 있는 경우만)
        while excess >= f as f64 && self.slot_has(self.next_seq.wrapping_add(1)) {
            let idx = Self::slot_index(self.next_seq);
            self.slots[idx].valid = false;
            self.next_seq = self.next_seq.wrapping_add(1);
            excess -= f as f64;
        }
        let slot = &self.slots[Self::slot_index(self.next_seq)];
        self.fifo_end_ts = self.ts_ext(slot.ts);
        self.fifo.clear();
        self.state = State::Playing;
        self.correction = Correction::Idle;
        self.drop_frames_pending = 0;
        self.trimming = false;
        self.conceal_run = 0;
        self.starve_samples = 0;
        // 첫 패킷의 앞부분을 잘라 깊이 = 목표
        if self.produce_packet() {
            let cut = (excess.round().max(0.0) as usize).min(self.fifo.len() / 2);
            self.fifo.drain(..cut * 2);
        }
        self.depth_ewma = Some(self.depth_now(now_us));
    }

    /// 버퍼링 중 시작 seq가 없고 뒤 패킷이 있으면 처음 존재하는 seq로 시작점을 옮긴다.
    fn skip_missing_head(&mut self) {
        if !self.has_stream || self.slot_has(self.next_seq) {
            return;
        }
        let span = seq_diff(self.highest, self.next_seq);
        if span <= 0 {
            return;
        }
        for k in 1..=(span as u32).min(SLOT_COUNT as u32) {
            let s = self.next_seq.wrapping_add(k);
            if self.slot_has(s) {
                self.next_seq = s;
                return;
            }
        }
    }

    fn has_later_packet(&self) -> bool {
        seq_diff(self.highest, self.next_seq) > 0
    }

    fn slot_has(&self, seq: u32) -> bool {
        let s = &self.slots[Self::slot_index(seq)];
        s.valid && s.seq == seq
    }

    fn push_loss(&mut self, lost: bool) {
        let win = ((5000.0 * Self::samples_per_ms()) as usize / self.frame()).clamp(1, LOSS_WINDOW_MAX);
        while self.loss_len >= win {
            let oldest = (self.loss_pos + LOSS_WINDOW_MAX - self.loss_len) % LOSS_WINDOW_MAX;
            if self.loss_ring[oldest] {
                self.loss_lost -= 1;
            }
            self.loss_len -= 1;
        }
        self.loss_ring[self.loss_pos] = lost;
        self.loss_pos = (self.loss_pos + 1) % LOSS_WINDOW_MAX;
        self.loss_len += 1;
        if lost {
            self.loss_lost += 1;
        }
    }

    fn ensure_decoder(&mut self, opus: bool, ch: usize) {
        if !self.decoder.as_ref().is_some_and(|d| d.matches(opus, ch)) {
            self.decoder = Decoder::new(opus, ch).ok();
        }
    }

    /// 패킷 단위 버림: 디코더 상태 유지를 위해 디코드하고, 다음 프레임과 크로스페이드할
    /// 앞부분을 보관한다.
    fn drop_one_packet(&mut self) {
        let seq = self.next_seq;
        let idx = Self::slot_index(seq);
        if self.slot_has(seq) {
            let (opus, stereo, len, pframe) = {
                let s = &self.slots[idx];
                (s.flags & 1 != 0, s.flags & 2 != 0, s.len, s.frame)
            };
            let ch = if stereo { 2 } else { 1 };
            self.ensure_decoder(opus, ch);
            let n = pframe * ch;
            let ok = match self.decoder.as_mut() {
                Some(d) => d.decode(&self.slots[idx].data[..len], &mut self.xfade_buf[..n]).unwrap_or(0) == pframe,
                None => false,
            };
            if ok {
                self.xfade_len = DROP_XFADE.min(pframe);
                self.xfade_ch = ch;
            }
            self.slots[idx].valid = false;
        }
        self.next_seq = seq.wrapping_add(1);
        self.stats.dropped_frames += 1;
        let f = self.frame() as f64;
        if let Some(e) = self.depth_ewma.as_mut() {
            *e -= f;
        }
    }

    /// 패킷 하나만큼 FIFO에 생산. 재버퍼링으로 전환하면 false.
    fn produce_packet(&mut self) -> bool {
        if self.drop_frames_pending > 0 && self.has_later_packet() && self.slot_has(self.next_seq) {
            self.drop_frames_pending -= 1;
            self.drop_one_packet();
        }

        let f = self.frame();
        let seq = self.next_seq;
        let idx = Self::slot_index(seq);
        let ch;
        let frames;

        if self.slot_has(seq) {
            let (opus, stereo, len, pframe, ts) = {
                let s = &self.slots[idx];
                (s.flags & 1 != 0, s.flags & 2 != 0, s.len, s.frame, s.ts)
            };
            ch = if stereo { 2 } else { 1 };
            self.ensure_decoder(opus, ch);
            let out = &mut self.dec_buf[..pframe * ch];
            let decoded = match self.decoder.as_mut() {
                Some(d) => d.decode(&self.slots[idx].data[..len], out).unwrap_or(0),
                None => 0,
            };
            if decoded != pframe {
                out.fill(0.0);
            }
            // 패킷 버림 직후: 버린 프레임 앞부분 → 새 프레임으로 크로스페이드
            if self.xfade_len > 0 && self.xfade_ch == ch {
                let l = self.xfade_len;
                for i in 0..l {
                    let w = (i + 1) as f32 / (l + 1) as f32;
                    for c in 0..ch {
                        let k = i * ch + c;
                        out[k] = self.xfade_buf[k] * (1.0 - w) + out[k] * w;
                    }
                }
            }
            self.xfade_len = 0;
            frames = pframe;
            self.slots[idx].valid = false;
            self.next_seq = seq.wrapping_add(1);
            self.fifo_end_ts = self.ts_ext(ts) + pframe as i64;
            self.last_frame[..frames * ch].copy_from_slice(&self.dec_buf[..frames * ch]);
            self.last_frame_valid = true;
            self.pkt_channels = ch;
            self.conceal_run = 0;
            self.starve_samples = 0;
            self.push_loss(false);
        } else {
            let lost = self.has_later_packet();
            if !lost {
                // 언더런: seq 전진 없이 은닉 프레임 삽입
                self.starve_samples += f;
                if self.starve_samples as f64 > STARVE_RESET_MS * Self::samples_per_ms() {
                    self.stats.rebuffers += 1;
                    let keep_next = self.next_seq;
                    self.reset_stream();
                    self.has_stream = true;
                    self.next_seq = keep_next;
                    self.highest = keep_next.wrapping_sub(1);
                    return false;
                }
            }
            ch = self.pkt_channels;
            frames = f;
            let out = &mut self.dec_buf[..frames * ch];
            let mut ok = false;
            if let Some(dec) = self.decoder.as_mut() {
                if matches!(dec, Decoder::Opus { .. }) {
                    let next = seq.wrapping_add(1);
                    let ns = &self.slots[Self::slot_index(next)];
                    let r = if lost && ns.valid && ns.seq == next {
                        dec.decode_fec(&ns.data[..ns.len], out)
                    } else {
                        dec.decode_plc(out)
                    };
                    ok = r.map(|n| n == frames).unwrap_or(false);
                }
            }
            if !ok {
                // PCM(또는 디코더 없음): 직전 프레임 페이드아웃 1회, 이후 무음
                if self.conceal_run == 0 && self.last_frame_valid {
                    let lf = &self.last_frame[..frames * ch];
                    for (i, (o, s)) in out.iter_mut().zip(lf).enumerate() {
                        let g = 1.0 - (i / ch) as f32 / frames as f32;
                        *o = s * g;
                    }
                } else {
                    out.fill(0.0);
                }
            }
            self.xfade_len = 0;
            self.conceal_run += 1;
            self.stats.concealed += 1;
            if lost {
                self.next_seq = seq.wrapping_add(1);
                self.push_loss(true);
            }
            self.fifo_end_ts += frames as i64;
        }
        self.stats.produced += 1;

        // 샘플 단위 드리프트 보정(모드가 켜져 있을 때만 패킷당 1샘플)
        let mut out_frames = frames;
        let mut use_shift = false;
        let step = self.corr_step.max(1);
        if frames > step as usize + 2 {
            match self.correction {
                Correction::Drop => {
                    shift_n(&self.dec_buf[..frames * ch], ch, frames, &mut self.shift_buf, -step);
                    out_frames = frames - step as usize;
                    use_shift = true;
                    self.stats.samples_dropped += step as u64;
                    if let Some(e) = self.depth_ewma.as_mut() {
                        *e -= step as f64;
                    }
                }
                Correction::Insert => {
                    shift_n(&self.dec_buf[..frames * ch], ch, frames, &mut self.shift_buf, step);
                    out_frames = frames + step as usize;
                    use_shift = true;
                    self.stats.samples_inserted += step as u64;
                    if let Some(e) = self.depth_ewma.as_mut() {
                        *e += step as f64;
                    }
                }
                Correction::Idle => {}
            }
        }
        let src: &[f32] = if use_shift { &self.shift_buf[..out_frames * ch] } else { &self.dec_buf[..out_frames * ch] };
        // 스테레오로 FIFO에 적재. fifo_end_ts는 원래 프레임 끝 그대로 두므로 1샘플 버리면
        // 재생 위치가 1 앞당겨진다(깊이 -1).
        if ch == 2 {
            self.fifo.extend(src.iter().copied());
        } else {
            for &s in src {
                self.fifo.push_back(s);
                self.fifo.push_back(s);
            }
        }
        true
    }

    pub fn stats(&self) -> JitterStats {
        let spm = Self::samples_per_ms() as f32;
        let mut s = self.stats;
        s.buffer_ms = if self.state == State::Playing {
            self.depth_ewma.unwrap_or(0.0) as f32 / spm
        } else {
            0.0
        };
        s.target_ms = self.target_samples() as f32 / spm;
        let wall = self.wall_cur_min.min(self.wall_prev_min);
        s.wall_buffer_ms = if self.state == State::Playing && wall.is_finite() { wall as f32 / spm } else { 0.0 };
        s.jitter_ms = self.rfc_jitter as f32 / spm;
        s.loss_pct = if self.loss_len > 0 {
            self.loss_lost as f32 * 100.0 / self.loss_len as f32
        } else {
            0.0
        };
        s
    }

    pub fn is_playing(&self) -> bool {
        self.state == State::Playing
    }
}

/// 1샘플 버림(`drop=true`) 또는 삽입. 읽기 위치를 프레임 전체에 걸쳐 선형으로 ±1 시프트한다
/// (선형 보간). 국소 피치 편차가 1/frames로 작다(240샘플이면 약 7센트, 리뷰 M2).
/// `dst`는 (frames±1)×ch 이상이어야 한다. `frames > 2`.
pub fn shift_one(src: &[f32], ch: usize, frames: usize, dst: &mut [f32], drop: bool) {
    shift_n(src, ch, frames, dst, if drop { -1 } else { 1 });
}

/// `delta`샘플만큼 늘이거나(+) 줄인다(−). 출력 길이 = frames + delta. 읽기 위치를 프레임 전체에 걸쳐
/// 선형으로 |delta|만큼 시프트한다. 국소 피치 편차는 |delta|/frames. `frames > |delta| + 1`.
pub fn shift_n(src: &[f32], ch: usize, frames: usize, dst: &mut [f32], delta: i32) {
    let n = delta.unsigned_abs() as usize;
    let drop = delta < 0;
    let out_frames = if drop { frames - n } else { frames + n };
    // 마지막 출력 샘플이 마지막 입력 샘플에 정확히 닿도록 램프 길이를 잡는다.
    let l = if drop { (frames - n - 1) as f32 } else { (frames + n - 1) as f32 };
    let last = (frames - 1) as f32;
    let nf = n as f32;
    for j in 0..out_frames {
        let r = (j as f32 / l).clamp(0.0, 1.0) * nf;
        let pos = if drop { j as f32 + r } else { j as f32 - r }.clamp(0.0, last);
        let i0 = pos.floor() as usize;
        let i1 = (i0 + 1).min(frames - 1);
        let w = pos - i0 as f32;
        for c in 0..ch {
            dst[j * ch + c] = src[i0 * ch + c] * (1.0 - w) + src[i1 * ch + c] * w;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::codec::Encoder;
    use crate::engine::config::{frame_code_from_samples, CodecKind};
    use crate::net::packet::{PacketType, FLAG_OPUS};
    use rand::{Rng, SeedableRng};

    struct Sender {
        enc: Encoder,
        frame: usize,
        seq: u32,
        ts: u32,
        phase: f64,
        opus: bool,
    }

    impl Sender {
        fn new(codec: CodecKind, frame: usize, seq0: u32, ts0: u32) -> Self {
            Self {
                enc: Encoder::new(codec, 1, 96_000).unwrap(),
                frame,
                seq: seq0,
                ts: ts0,
                phase: 0.0,
                opus: codec == CodecKind::Opus,
            }
        }
        fn next(&mut self) -> (Header, Vec<u8>) {
            let mut pcm = vec![0f32; self.frame];
            for s in pcm.iter_mut() {
                *s = (self.phase * 2.0 * std::f64::consts::PI).sin() as f32 * 0.5;
                self.phase = (self.phase + 440.0 / 48000.0).fract();
            }
            let mut buf = vec![0u8; MAX_PAYLOAD_BYTES];
            let n = self.enc.encode(&pcm, &mut buf).unwrap();
            buf.truncate(n);
            let h = Header {
                ptype: PacketType::Audio,
                ssrc: 1,
                seq: self.seq,
                timestamp: self.ts,
                flags: if self.opus { FLAG_OPUS } else { 0 },
                frame_code: frame_code_from_samples(self.frame).unwrap(),
            };
            self.seq = self.seq.wrapping_add(1);
            self.ts = self.ts.wrapping_add(self.frame as u32);
            (h, buf)
        }
    }

    #[derive(Clone, Copy)]
    struct Sim {
        codec: CodecKind,
        /// 송신 패킷 프레임
        frame: usize,
        /// 수신 장치 콜백(버스트) 크기
        burst: usize,
        /// 워커 pop 단위
        chunk: usize,
        drift_ppm: f64,
        jitter_ms: f64,
        loss: f64,
        seconds: f64,
        seed: u64,
        /// 송신측 장치 콜백 크기(패킷이 이 단위로 몰려서 나감)
        tx_burst: usize,
    }

    impl Default for Sim {
        fn default() -> Self {
            Self {
                codec: CodecKind::Pcm16,
                frame: 240,
                burst: 240,
                chunk: 64,
                drift_ppm: 0.0,
                jitter_ms: 0.0,
                loss: 0.0,
                seconds: 5.0,
                seed: 1,
                tx_burst: 0,
            }
        }
    }

    struct SimResult {
        stats: JitterStats,
        /// (초, buffer_ms)
        trace: Vec<(f64, f32)>,
        /// 시작 1초 이후 무음 청크 수
        silent_chunks: usize,
        /// 시작 1초 이후 보정 횟수
        corrections_after_1s: u64,
        /// 5초 시점 통계(정착 후 보정 방향 확인용)
        stats_at_5s: JitterStats,
        /// 인접 샘플 불연속 수(L 채널, 차 > 0.08)
        discontinuities: usize,
    }

    /// 이벤트 시뮬레이션. 송신 클럭은 (1+drift_ppm)배 빠르고, 네트워크 지연은 base + U(0,jitter),
    /// 손실 확률은 loss다. 수신측은 `burst`마다 `chunk` 단위로 연달아 pop한 뒤 `end_burst`를 부른다.
    fn simulate(p: Sim) -> SimResult {
        let mut rng = rand::rngs::StdRng::seed_from_u64(p.seed);
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0 });
        let mut tx = Sender::new(p.codec, p.frame, u32::MAX - 50, u32::MAX - 10_000);
        let send_period_us = p.frame as f64 / 48000.0 * 1e6 / (1.0 + p.drift_ppm * 1e-6);
        let burst_period_us = p.burst as f64 / 48000.0 * 1e6;
        let base_us = 3000.0;

        let mut arrivals: Vec<(f64, Header, Vec<u8>)> = Vec::new();
        let mut t = 0.0;
        let mut k = 0u64;
        while t < p.seconds * 1e6 {
            let (h, pk) = tx.next();
            // 송신측 장치 버스트: tx_burst마다 몰아서 나감
            let send_t = if p.tx_burst > p.frame {
                let per = (p.tx_burst / p.frame) as u64;
                ((k / per + 1) * per) as f64 * send_period_us
            } else {
                t
            };
            if rng.gen::<f64>() >= p.loss {
                let d = base_us + rng.gen::<f64>() * p.jitter_ms * 1000.0;
                arrivals.push((send_t + d, h, pk));
            }
            t += send_period_us;
            k += 1;
        }
        arrivals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        let mut out = vec![0f32; p.chunk * 2];
        let mut ai = 0;
        let mut bt = 0.0;
        let mut trace = Vec::new();
        let mut silent = 0;
        let mut started = false;
        let mut corr_at_1s = None;
        let mut at_5s = None;
        let mut prev = None::<f32>;
        let mut disc = 0;
        let mut next_trace = 0.0;
        while bt < p.seconds * 1e6 {
            while ai < arrivals.len() && arrivals[ai].0 <= bt {
                let (at, h, pk) = &arrivals[ai];
                jb.insert(h, pk, *at as u64);
                ai += 1;
            }
            let mut left = p.burst;
            while left > 0 {
                let n = left.min(p.chunk);
                jb.pop(&mut out[..n * 2]);
                if started && bt > 1e6 {
                    if out[..n * 2].iter().all(|x| x.abs() < 1e-6) {
                        silent += 1;
                    }
                    for fr in out[..n * 2].as_chunks::<2>().0 {
                        if let Some(pv) = prev {
                            if (fr[0] - pv).abs() > 0.08 {
                                disc += 1;
                            }
                        }
                        prev = Some(fr[0]);
                    }
                }
                left -= n;
            }
            jb.end_burst(bt as u64, bt as u64);
            if jb.is_playing() {
                started = true;
            }
            if bt >= 1e6 && corr_at_1s.is_none() {
                corr_at_1s = Some(jb.stats().corrections());
            }
            if bt >= 5e6 && at_5s.is_none() {
                at_5s = Some(jb.stats());
            }
            if bt >= next_trace {
                trace.push((bt / 1e6, jb.stats().buffer_ms));
                next_trace += 250_000.0;
            }
            bt += burst_period_us;
        }
        let stats = jb.stats();
        SimResult {
            corrections_after_1s: stats.corrections() - corr_at_1s.unwrap_or(0),
            stats_at_5s: at_5s.unwrap_or_default(),
            stats,
            trace,
            silent_chunks: silent,
            discontinuities: disc,
        }
    }

    fn mean_buffer(trace: &[(f64, f32)], from: f64, to: f64) -> f32 {
        let v: Vec<f32> = trace.iter().filter(|(t, _)| *t >= from && *t < to).map(|x| x.1).collect();
        v.iter().sum::<f32>() / v.len().max(1) as f32
    }

    #[test]
    fn clean_network_zero_concealment_and_zero_corrections() {
        // 리뷰 매트릭스: 수신 장치 버퍼가 프레임보다 큰 경우 포함(M4), 드리프트 0이면 보정 0(M2)
        for (frame, burst) in [(240, 128), (120, 480), (240, 480), (240, 1024), (480, 1024), (120, 1024), (480, 128)] {
            let r = simulate(Sim { frame, burst, seconds: 6.0, ..Default::default() });
            assert_eq!(r.stats.concealed, 0, "frame {frame} burst {burst}: {:?}", r.stats);
            assert_eq!(r.stats.corrections(), 0, "드리프트 0이면 보정 0: frame {frame} burst {burst}: {:?}", r.stats);
            assert_eq!(r.silent_chunks, 0);
            assert_eq!(r.discontinuities, 0, "frame {frame} burst {burst}");
            assert!(r.stats.buffer_ms < 12.0, "깨끗한 망은 지연이 작아야 함: {:?}", r.stats);
        }
    }

    #[test]
    fn sender_bursts_are_absorbed() {
        // 송신측 장치 버퍼 480(10ms)마다 프레임 2.5ms 4개가 몰려 나가는 경우
        let r = simulate(Sim { frame: 120, burst: 480, tx_burst: 480, seconds: 6.0, ..Default::default() });
        assert_eq!(r.stats.concealed, 0, "{:?}", r.stats);
        assert_eq!(r.corrections_after_1s, 0, "{:?}", r.stats);
        assert_eq!(r.discontinuities, 0);
    }

    #[test]
    fn jitter_and_loss_opus() {
        let r = simulate(Sim {
            codec: CodecKind::Opus,
            jitter_ms: 20.0,
            loss: 0.05,
            seconds: 20.0,
            seed: 2,
            ..Default::default()
        });
        let s = r.stats;
        let ratio = s.concealed as f64 / s.produced as f64;
        assert!(ratio < 0.08, "은닉 비율 {ratio} {s:?}");
        assert!((2.0..9.0).contains(&s.loss_pct), "손실률 추정 {s:?}");
        assert!(s.target_ms >= 15.0 && s.target_ms <= 35.0, "목표 깊이가 지터를 반영해야 함 {s:?}");
        let late = mean_buffer(&r.trace, 10.0, 20.0);
        assert!((late - s.target_ms).abs() < 4.0, "깊이 {late} vs 목표 {}", s.target_ms);
        assert_eq!(s.dropped_frames, 0, "정상 상태에서 패킷 단위 버림 없음: {s:?}");
    }

    #[test]
    fn reordering_handled() {
        let r = simulate(Sim { frame: 120, burst: 128, jitter_ms: 20.0, seconds: 10.0, seed: 3, ..Default::default() });
        let ratio = r.stats.concealed as f64 / r.stats.produced as f64;
        assert!(ratio < 0.03, "순서 뒤바뀜에서 은닉 비율 {ratio} {:?}", r.stats);
    }

    fn drift_case(ppm: f64, burst: usize) {
        let r = simulate(Sim { burst, drift_ppm: ppm, jitter_ms: 5.0, seconds: 60.0, seed: 4, ..Default::default() });
        let s = r.stats;
        let mid = mean_buffer(&r.trace, 20.0, 30.0);
        let end = mean_buffer(&r.trace, 50.0, 60.0);
        println!("drift {ppm}ppm burst {burst}: target {:.2}ms mid {mid:.2}ms end {end:.2}ms stats {s:?}", s.target_ms);
        // 데드밴드(F/2 = 2.5ms) 안에서 수렴
        assert!((end - s.target_ms).abs() < 3.0, "드리프트 {ppm}ppm: end={end} target={}", s.target_ms);
        assert!((end - mid).abs() < 2.0, "지연이 누적되면 안 됨: mid={mid} end={end}");
        // 200ppm × 60s ≈ 576샘플(불감대 안에 남는 몫 제외). 정착(5초) 이후에는 한 방향으로만
        // 보정해야 한다(진동 없음). 시작 직후의 목표 재설정분은 제외.
        let s5 = r.stats_at_5s;
        let (dropped, inserted) = (s.samples_dropped - s5.samples_dropped, s.samples_inserted - s5.samples_inserted);
        if ppm > 0.0 {
            assert!(dropped >= 300 && inserted == 0, "5초 이후 버림 {dropped} 삽입 {inserted}: {s:?}");
        } else {
            assert!(inserted >= 300 && dropped == 0, "5초 이후 삽입 {inserted} 버림 {dropped}: {s:?}");
        }
        assert_eq!(s.dropped_frames, 0, "{s:?}");
        assert_eq!(r.silent_chunks, 0, "드리프트 보정 중 끊김 없음");
    }

    #[test]
    fn drift_plus_200ppm_converges() {
        drift_case(200.0, 240);
        drift_case(200.0, 1024);
    }

    #[test]
    fn drift_minus_200ppm_converges() {
        drift_case(-200.0, 240);
        drift_case(-200.0, 1024);
    }

    #[test]
    fn mismatched_frame_sizes() {
        let r = simulate(Sim { codec: CodecKind::Opus, frame: 120, burst: 480, jitter_ms: 5.0, ..Default::default() });
        assert_eq!(r.silent_chunks, 0, "{:?}", r.stats);
        let r = simulate(Sim { codec: CodecKind::Opus, frame: 480, burst: 128, jitter_ms: 5.0, seed: 6, ..Default::default() });
        assert_eq!(r.silent_chunks, 0, "{:?}", r.stats);
    }

    #[test]
    fn late_packets_discarded() {
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0 });
        let mut tx = Sender::new(CodecKind::Pcm16, 240, 10, 0);
        let pkts: Vec<_> = (0..20).map(|_| tx.next()).collect();
        let mut out = vec![0f32; 480];
        let mut t = 0u64;
        for (h, p) in pkts.iter().take(10) {
            jb.insert(h, p, t);
            jb.pop(&mut out);
            jb.end_burst(t, t);
            t += 5000;
        }
        for _ in 0..5 {
            jb.pop(&mut out);
            jb.end_burst(t, t);
            t += 5000;
        }
        let before = jb.stats().late;
        jb.insert(&pkts[0].0, &pkts[0].1, t);
        assert_eq!(jb.stats().late, before + 1);
    }

    #[test]
    fn replayed_old_packet_cannot_rewind_buffering() {
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0 });
        let mut tx = Sender::new(CodecKind::Pcm16, 240, 1000, 0);
        let pkts: Vec<_> = (0..300).map(|_| tx.next()).collect();
        jb.insert(&pkts[250].0, &pkts[250].1, 0);
        // 200개 이전 패킷 재전송 → 버퍼링 중이라도 시작점을 크게 되돌리지 않음
        jb.insert(&pkts[50].0, &pkts[50].1, 10);
        assert_eq!(jb.stats().late, 1);
    }

    #[test]
    fn pcm_concealment_fades_then_silence() {
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0 });
        let mut tx = Sender::new(CodecKind::Pcm16, 240, 0, 0);
        let mut out = vec![0f32; 480];
        let mut t = 0u64;
        for _ in 0..20 {
            let (h, p) = tx.next();
            jb.insert(&h, &p, t);
            jb.pop(&mut out);
            jb.end_burst(t, t);
            t += 5000;
        }
        let lost: Vec<_> = (0..3).map(|_| tx.next()).collect();
        drop(lost);
        let (h, p) = tx.next();
        jb.insert(&h, &p, t);
        let c0 = jb.stats().concealed;
        let mut energies = vec![];
        for _ in 0..6 {
            jb.pop(&mut out);
            jb.end_burst(t, t);
            energies.push(out.iter().map(|x| x * x).sum::<f32>());
            t += 5000;
            let (h, p) = tx.next();
            jb.insert(&h, &p, t);
        }
        assert!(jb.stats().concealed >= c0 + 3);
        assert!(energies.iter().any(|&e| e < 1e-9), "페이드 1회 후 무음 프레임이 있어야 함: {energies:?}");
    }

    #[test]
    fn backlog_is_trimmed_with_crossfade() {
        // 1초 단절 후 몰려온 패킷(와이파이 버스트) → 패킷 단위 버림 + 크로스페이드, 클릭 없음
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0 });
        let mut tx = Sender::new(CodecKind::Opus, 240, 0, 0);
        let mut out = vec![0f32; 128 * 2];
        let mut t = 0u64;
        let mut prev = None::<f32>;
        let mut max_step = 0f32;
        let mut held: Vec<(Header, Vec<u8>)> = Vec::new();
        let mut send_t = 0u64;
        let mut burst_t = 0u64;
        while t < 6_000_000 {
            while send_t <= t {
                let pkt = tx.next();
                if (2_000_000..2_100_000).contains(&send_t) {
                    held.push(pkt); // 100ms 동안 지연
                } else {
                    if send_t >= 2_100_000 && !held.is_empty() {
                        for (h, p) in held.drain(..) {
                            jb.insert(&h, &p, send_t + 3000);
                        }
                    }
                    jb.insert(&pkt.0, &pkt.1, send_t + 3000);
                }
                send_t += 5000;
            }
            if t >= burst_t {
                jb.pop(&mut out);
                jb.end_burst(t, t);
                if t > 1_000_000 {
                    for fr in out.as_chunks::<2>().0 {
                        if let Some(p) = prev {
                            max_step = max_step.max((fr[0] - p).abs());
                        }
                        prev = Some(fr[0]);
                    }
                }
                burst_t += 128 * 1_000_000 / 48_000;
            }
            t += 500;
        }
        let s = jb.stats();
        println!("backlog: {s:?}, max step {max_step}");
        assert!(s.dropped_frames > 0, "쌓인 지연은 패킷 단위로 줄여야 함: {s:?}");
        assert!(s.buffer_ms < s.target_ms + 3.0, "복귀 후 목표 근처: {s:?}");
    }

    #[test]
    fn shift_one_lengths_and_continuity() {
        let src: Vec<f32> = (0..240).map(|i| i as f32).collect();
        let mut dst = vec![0f32; 241];
        shift_one(&src, 1, 240, &mut dst, true);
        assert_eq!(dst[0], 0.0);
        assert!((dst[238] - 239.0).abs() < 1e-3);
        for w in dst[..239].windows(2) {
            // 프레임 전체 램프: 간격이 1 + 1/238 근처로 고르게
            assert!((w[1] - w[0] - 1.0).abs() < 0.01, "{}", w[1] - w[0]);
        }
        shift_one(&src, 1, 240, &mut dst, false);
        assert_eq!(dst[0], 0.0);
        assert!((dst[240] - 239.0).abs() < 1e-3);
        for w in dst.windows(2) {
            assert!((w[1] - w[0] - 1.0).abs() < 0.01);
        }
    }
}
