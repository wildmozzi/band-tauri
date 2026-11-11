//! 피어별 적응형 지터 버퍼 + 드리프트 보정 + PLC (설계 문서 1.2절).
//!
//! ## 사용법(워커, 콜백 구동 믹스)
//! 워커는 받은 데이터로 꺼낼 수 있으면(`can_pop_real`) 작은 청크로 미리 `pop`해 피어의 소스 링에
//! 넣고, 데이터가 없으면 출력 콜백 마감 직전에만 `pop`(은닉)한다. 출력 콜백이 돌 때마다
//! `end_burst(now, wall, ahead)`를 한 번 부른다. 깊이 측정·재생 시작·보정 판단은 모두 `end_burst`에서만
//! 한다. `now`는 워커가 깨어난 시각이 아니라 **출력 콜백 시각 추정치**(장치 클럭)다. 워커 기상 지연이
//! 깊이 측정에 섞이면 제어가 발산한다(리뷰 M2/M4, CI 실패 원인). `ahead`는 이미 꺼냈지만 재생 전인
//! 양(소스 링)이며, 깊이는 실제 재생 위치를 기준으로 잰다.
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
//! ## 목표 깊이 (P1-5)
//! 상대 지연 `rel = transit - min_transit`의 분위수(프로필 q, 균형 0.95)를 세 창으로 추정해 큰 값을 쓴다
//! (모두 0.25ms 격자로 올림, `JitterTuning`):
//! - **짧은 창**: 최근 200패킷 분위수. 오르면 바로 올리고, 더 낮은 후보가 `hold_ms` 동안 이어질 때만 내린다.
//! - **버스트**: 최근 4패킷 중 3개가 max(현재 추정, 짧은 창 p99) + max(F/2, 1ms) × 프로필 배율을 넘으면 그 최댓값까지
//!   바로 올린다(짧은 창 분위수는 창의 5%가 늦어야 움직인다. 시간 군집·p99 기준은 꼬리 긴 지터의 상시 발화 방지). 버스트가 끝나고 `burst_hold_ms`가 지나면 없앤다. 절대 지연으로 기억하므로
//!   경로가 바뀌어 최소 지연이 오르면 그만큼 줄어든다.
//! - **긴 창**: 1초 블록마다 블록 분위수를 남기고 최근 `long_blocks`개 중 max(3, ceil(n/6))번째로 큰 값의 초과분 × `long_weight`. 반복되는 스파이크(와이파이
//!   스캔 등)가 지나갈 때마다 목표를 내렸다가 다시 은닉하는 순환을 막는다. 한 번의 버스트(블록 2개 이하)는 무시된다.
//!
//! 최종 목표 = `j + lead + 삽입 진입 문턱 + pad`. `lead`는 소비자(워커)가 은닉 판단 시각에 p95 너머로 요구하는 깊이
//! (`set_consumer_lead`: 은닉 마감 여유 1.5ms + 2·피어 적응 여유, P1-13). 깊이는 삽입 진입 문턱만큼 목표 밑으로
//! 내려가 머물 수 있으므로(드리프트, PR #16 M2) 그만큼 더한다. 그래서 깊이 ≥ `j + lead`가 항상 지켜진다.
//! 프레임 단위로 올리던 이전 공식(`(ceil(p95/F) + 1)·F`)은 안정 회선에서 1F 가까이 더 깊었다(GOAL 1절 위반).
//!
//! ## 보정 (데드밴드 + 히스테리시스, 비대칭)
//! 오차 `e = depth_ewma - target`에 대해
//! - 대기 상태에서 `e > enter_drop`면 버림 모드, `e < -enter_ins`면 삽입 모드로 들어간다.
//! - 버림/삽입 모드에서는 패킷 경계를 넘는 분수 리샘플러로 읽기 간격 1 ± 0.25%(최대 약 4.3센트)로 줄이거나 늘린다
//!   (`RateShifter`, PR #20 H1). 언더런 은닉 뒤 늦게 온 패킷은 깊이가 목표 + F/2를 넘게 되면 손실로 처리한다(디코드 후 버림).
//! - 해제 문턱 안으로 들어오면 대기로 돌아간다. `enter_drop = clamp(F/2, 1ms, 2.5ms)`(지연 쪽, 넓게),
//!   `enter_ins = clamp(F/8, 0.25ms, 1ms)`(은닉 쪽, 좁게: 목표에 포함), 해제 = 진입/4(최소 4샘플).
//! - 목표가 내려가거나 소비자 요구가 바뀌면 해제 문턱까지 따라간다(`chase`). 없으면 깊이가 버림 데드밴드만큼 남았다.
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
/// 목표 격자(샘플, 0.25ms). 짧은·버스트·긴 창 추정을 이 단위로 올림한다.
const TARGET_GRID: f64 = 12.0;
/// 버스트 감지 창(패킷)과 그 안에서 필요한 초과 패킷 수.
const BURST_WIN: usize = 16;
const BURST_COUNT: usize = 3;
/// 버스트 판정의 시간 군집 창(패킷): 이 안에서 BURST_COUNT개가 넘어야 한다.
const BURST_CLUSTER: usize = 4;
/// 긴 창 블록 길이(µs)와 최대 블록 수, 블록당 최대 표본.
const BLOCK_US: u64 = 1_000_000;
const LONG_BLOCKS_MAX: usize = 32;
const BLOCK_SAMPLES_MAX: usize = 1024;
/// 긴 창 추정: 최근 블록 분위수 중 이 순위(1 = 최댓값). 버스트 하나는 블록 2개까지 걸치므로 3.
const LONG_RANK: usize = 3;
/// 경로 변경(지연 급감)으로 통계를 다시 모은 뒤, 옛 경로로 늦게 오는 패킷을 통계에서 빼는 시간(µs).
const PATH_GRACE_US: u64 = 200_000;
/// 재생 시작 직후 목표에 min(1프레임, 2.5ms)을 더해 두는 시간(µs).
const START_PAD_US: u64 = 1_000_000;
/// 패킷 단위 버림 시 크로스페이드 길이(샘플).
const DROP_XFADE: usize = 48;
/// 샘플 단위 보정(버림·삽입)의 최대 속도(비율, PR #20 H1). 0.25% = 1200·log2(1.0025) ≈ 4.3센트: 지속음 피치 JND
/// (약 5–10센트) 아래다. 드리프트(±200ppm = 0.02%)는 이 속도의 1/12로 충분히 덮는다. 이전에는 패킷마다 1–2샘플을
/// 그 패킷 안에서 보정해 2.5ms 프레임에서 14–29센트였다. 더 큰 오차는 패킷 단위 버림(트리밍)과 늦은 패킷 손실 처리가 맡는다.
pub const MAX_CORR_RATE: f64 = 0.0025;

/// 샘플 보정으로 깊이를 `samples`(48k)만큼 옮기는 데 걸리는 이론 시간(초): `samples / (MAX_CORR_RATE × 48000)`.
/// 테스트가 목표 변경 뒤 수렴 시간 상한과 보정 제외 창을 이 값으로 계산한다(엔진 사양을 그대로 따름).
pub fn correction_catch_up_secs(samples: f64) -> f64 {
    samples.abs() / (MAX_CORR_RATE * INTERNAL_RATE as f64)
}

/// 프레임 `frame`(샘플)에서 보정 해제 문턱(ms): (버림 해제, 삽입 해제). 수렴 판정 기준(테스트용).
pub fn correction_exit_ms(frame: usize) -> (f64, f64) {
    let f = frame as f64;
    let spm = INTERNAL_RATE as f64 / 1000.0;
    let enter_drop = (f / 2.0).clamp(spm, 2.5 * spm);
    let enter_ins = (f / 8.0).clamp(0.25 * spm, spm);
    ((enter_drop / 4.0).max(4.0) / spm, (enter_ins / 4.0).max(4.0) / spm)
}
/// 보정이 끝날 때 읽기 위상을 정수로 되돌리는 허용 오차(샘플). 이보다 가까우면 바로 맞춘다.
const PHASE_SNAP: f64 = 0.1;

/// 패킷 경계를 넘어 이어지는 분수 읽기 위치 리샘플러(선형 보간). 보정 중에는 읽기 간격 `step` = 1 ± MAX_CORR_RATE로
/// 출력 길이가 패킷마다 조금씩 달라진다. 보정이 끝나도 위상이 정수가 될 때까지(`PHASE_SNAP`) 같은 방향으로 이어 간 뒤
/// 멈춰, 평상시에는 보간(고역 감쇠) 없이 원 샘플을 그대로 낸다.
#[derive(Default)]
struct RateShifter {
    /// 다음 출력 샘플의 읽기 위치(현재 패킷 시작 기준, −1 이상: −1..0은 직전 패킷 마지막 샘플과의 사이).
    pos: f64,
    /// 직전 패킷의 마지막 입력 샘플(채널별).
    prev: [f32; 2],
    /// 마지막으로 쓴 읽기 간격(위상 정리용).
    last_step: f64,
    /// 위상 정리 중 경과 패킷 수.
    settle: u32,
}

impl RateShifter {
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// 위상이 0이면 원 샘플을 그대로 쓴다(복사 없음).
    fn idle(&self) -> bool {
        self.pos == 0.0
    }

    /// `src`(frames×ch)를 읽기 간격 `step`으로 리샘플해 `dst`에 쓰고 출력 프레임 수를 돌려준다.
    fn process(&mut self, src: &[f32], ch: usize, frames: usize, dst: &mut [f32], step: f64) -> usize {
        let last = (frames - 1) as f64;
        let cap = dst.len() / ch;
        let mut n = 0;
        let mut pos = self.pos;
        while pos <= last && n < cap {
            let i0 = pos.floor();
            let w = (pos - i0) as f32;
            let i0 = i0 as i64;
            for c in 0..ch {
                let a = if i0 < 0 { self.prev[c] } else { src[i0 as usize * ch + c] };
                let b = if w == 0.0 { a } else { src[(i0 + 1) as usize * ch + c] };
                dst[n * ch + c] = a + (b - a) * w;
            }
            n += 1;
            pos += step;
        }
        self.pos = pos - frames as f64;
        for c in 0..ch {
            self.prev[c] = src[(frames - 1) * ch + c];
        }
        self.last_step = step;
        n
    }
}
/// 벽시계 깊이 최솟값 버킷 길이(µs). 보고값은 직전+현재 버킷의 최솟값(0.5~1초).
const WALL_BUCKET_US: u64 = 500_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JitterSettings {
    /// 0이면 자동.
    pub min_ms: f32,
    pub max_ms: f32,
    /// 저지연(0) ↔ 안정(1) 프로필. 0.5 = 균형(기본). `JitterTuning::from_profile`.
    pub profile: f32,
}

/// 사용자 '저지연 ↔ 안정' 프로필(0..1)에서 나온 목표 계산 계수(P1-5, 설계 문서 1.2절).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JitterTuning {
    /// 지연 분위수: 0 → 0.90, 0.5 → 0.95, 1 → 0.99.
    pub quantile: f64,
    /// 목표에 더하는 여유(프레임 배수): 0.5 이하 0, 1 → 0.5F.
    pub pad_frames: f64,
    /// 짧은 창 추정을 내리기 전 유지 시간(ms): 1s / 2s / 4s(균형 = 이전 고정값 2s). 반복 스파이크는 긴 창이 맡는다.
    pub hold_ms: f64,
    /// 버스트 상향 유지 시간(ms, 마지막 버스트 패킷 이후): 0.5s / 2s / 4s(6초 간격 스파이크를 이어 붙이지 않게, M4).
    pub burst_hold_ms: f64,
    /// 긴 창 블록 수(1초 블록): 5 / 12 / 20. 순위는 `max(3, ceil(블록 수/6))`(3 / 3 / 4) — 안정 쪽에서도 창 안 블록의
    /// 1/6 이상이 높아야 덮는다(PR #20 M4: 30블록·3순위에서는 스파이크 3번이 30초 동안 목표를 붙잡아 0.5→1 구간이 절벽).
    pub long_blocks: usize,
    /// 피어 적응 여유(언더런) 유지·상한 배율(워커 `PeerFeed`): 0.5 / 1 / 2.
    pub margin_scale: f64,
    /// 긴 창(반복 스파이크) 초과분 반영 비율: 0.25 이하 0, 0.5 → 1/6, 1 → 1/2(선형). 스파이크 시작은 어차피 은닉되므로
    /// 전부 덮어도 은닉 이득이 작고(M4 시나리오 0.03%p) 지연은 스파이크 높이만큼 는다.
    pub long_weight: f64,
    /// 버스트 판정 문턱 배율(`max(F/2, 1ms)`에 곱함): 1.5 / 1 / 0.5. 저지연은 분위수가 낮아 꼬리 패킷이 문턱을 자주
    /// 넘으므로 문턱을 높여 진짜 스파이크에만 반응한다.
    pub burst_scale: f64,
}

impl JitterTuning {
    pub fn from_profile(profile: f32) -> Self {
        let p = if profile.is_finite() { profile.clamp(0.0, 1.0) as f64 } else { 0.5 };
        let quantile = if p <= 0.5 { 0.90 + 0.1 * p } else { 0.95 + 0.08 * (p - 0.5) };
        Self {
            quantile,
            pad_frames: (p - 0.5).max(0.0),
            hold_ms: if p <= 0.5 { 1000.0 + 2000.0 * p } else { 2000.0 + 4000.0 * (p - 0.5) },
            burst_hold_ms: if p <= 0.5 { 500.0 + 3000.0 * p } else { 2000.0 + 4000.0 * (p - 0.5) },
            long_blocks: (5.0 + 15.0 * p).round() as usize,
            margin_scale: if p <= 0.5 { 0.5 + p } else { 1.0 + 2.0 * (p - 0.5) },
            burst_scale: 1.5 - p,
            long_weight: ((p - 0.25) / 0.75).clamp(0.0, 1.0) * 0.5,
        }
    }
}

/// 최근 N개 bool 중 true 비율(손실·은닉 창).
struct RateRing {
    ring: Vec<bool>,
    pos: usize,
    len: usize,
    hits: usize,
}

impl RateRing {
    fn new(cap: usize) -> Self {
        Self { ring: vec![false; cap], pos: 0, len: 0, hits: 0 }
    }

    fn push(&mut self, v: bool, win: usize) {
        let cap = self.ring.len();
        let win = win.clamp(1, cap);
        while self.len >= win {
            let oldest = (self.pos + cap - self.len) % cap;
            if self.ring[oldest] {
                self.hits -= 1;
            }
            self.len -= 1;
        }
        self.ring[self.pos] = v;
        self.pos = (self.pos + 1) % cap;
        self.len += 1;
        if v {
            self.hits += 1;
        }
    }

    fn pct(&self) -> f32 {
        if self.len == 0 {
            0.0
        } else {
            self.hits as f32 * 100.0 / self.len as f32
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct JitterStats {
    pub buffer_ms: f32,
    pub target_ms: f32,
    pub jitter_ms: f32,
    pub loss_pct: f32,
    pub late: u64,
    pub concealed: u64,
    /// 은닉 중 손실 은닉(다음 패킷이 이미 있어 seq를 넘긴 것 — Opus면 FEC/PLC). 나머지(`concealed - concealed_loss`)는
    /// 언더런 은닉(seq 유지, 깊이 +1F). 측정용(P1-6 리포트)이며 Tauri `PeerStats`에는 노출하지 않는다.
    pub concealed_loss: u64,
    /// 생산된 패킷 프레임 수(정상 디코드 + 은닉).
    pub produced: u64,
    pub dropped_frames: u64,
    pub samples_dropped: u64,
    pub samples_inserted: u64,
    pub rebuffers: u64,
    /// 최근 5초 은닉 비율(%, 은닉 프레임 / 생산 프레임).
    pub conceal_pct: f32,
    /// 목표 중 소비자(워커) 요구 깊이(ms): 은닉 마감 여유 + 2·피어 적응 여유(언더런 원인 상향, P1-13).
    pub lead_ms: f32,
    /// 목표의 지터 추정(ms): 짧은 창·버스트·긴 창 중 큰 값(지터 원인 상향).
    pub jitter_est_ms: f32,
    /// 최근 5초 중 샘플 보정(버림·삽입) 모드였던 패킷 비율(%, H1).
    pub corr_time_pct: f32,
    /// 샘플 보정의 최대 국소 피치 편차(센트, 재생 시작 이후).
    pub max_corr_cents: f32,
    /// 늦게 와서(언더런 은닉 뒤) 손실로 처리해 버린 패킷 수.
    pub late_dropped: u64,
    /// 버스트 감지(즉시 상향) 횟수.
    pub burst_events: u64,
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
    /// 상대 지연의 프로필 분위수(샘플, 짧은 창).
    q_rel: f64,
    /// 짧은 창 p99(샘플). 버스트 판정 기준(꼬리 패킷과 버스트를 가른다, PR #20 M1).
    q99_rel: f64,
    rfc_jitter: f64,
    last_transit: Option<i64>,

    tuning: JitterTuning,
    // 목표 추정(샘플). 짧은 창 상한과 하향 대기 시작 시각.
    short_cap: f64,
    short_lower_since: Option<u64>,
    /// 버스트 상향: 절대 transit 기준 수준(최소 지연이 오르면 그만큼 줄어든다)과 마지막 버스트 패킷 시각.
    burst_abs: Option<i64>,
    burst_at_us: u64,
    recent_rel: [i64; BURST_WIN],
    recent_pos: usize,
    /// 긴 창: 현재 블록 표본과 시작 시각, 블록 분위수 링.
    block_rel: Vec<i64>,
    block_start_us: Option<u64>,
    block_q: [f64; LONG_BLOCKS_MAX],
    block_pos: usize,
    block_count: usize,
    long_cap: f64,
    /// 마지막으로 계산한 목표(샘플). 내려가면 `chase`.
    last_target: f64,
    /// 소비자가 은닉 판단 시각에 p95 너머로 요구하는 깊이(샘플, 48k, `set_consumer_lead`, P1-13).
    consumer_lead: f64,
    /// 소비자 요구로 목표가 바뀐 뒤 해제 문턱까지 보정 중(데드밴드 무시).
    chase: bool,

    depth_ewma: Option<f64>,
    /// 이미 꺼냈지만 아직 재생되지 않은 샘플 수(48k). 콜백 구동 믹스에서 소스 링·리샘플러에 있는 양.
    /// 깊이는 실제 재생 위치(소스 링의 머리)를 기준으로 잰다.
    ahead: f64,
    correction: Correction,
    drop_frames_pending: usize,
    trimming: bool,
    shifter: RateShifter,
    /// 보정 모드였던 패킷 비율 창(H1 통계).
    corr_ring: RateRing,
    max_corr_cents: f32,
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

    // 손실·은닉 창
    loss: RateRing,
    conceal: RateRing,
    /// 재생 시작 직후 여유(샘플)와 그 끝 시각(µs, 장치 클럭). 출력 콜백 시각 추정(`DeviceClock`)이 이른 콜백을
    /// 아직 못 본 첫 콜백들에서 소스 링이 모자라지 않게 한다. 끝나면 `chase`로 목표까지 내려간다.
    start_pad: f64,
    start_pad_until_us: u64,
    /// 경로 변경 유예 끝 시각(µs, `PATH_GRACE_US`).
    path_reset_until_us: u64,
    /// 경로 변경 유예 동안의 비교 문턱(샘플, 리셋 전 값).
    path_grace_step: i64,

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
            q_rel: 0.0,
            q99_rel: 0.0,
            rfc_jitter: 0.0,
            last_transit: None,
            tuning: JitterTuning::from_profile(settings.profile),
            short_cap: -1.0,
            short_lower_since: None,
            burst_abs: None,
            burst_at_us: 0,
            recent_rel: [0; BURST_WIN],
            recent_pos: 0,
            block_rel: Vec::with_capacity(BLOCK_SAMPLES_MAX),
            block_start_us: None,
            block_q: [0.0; LONG_BLOCKS_MAX],
            block_pos: 0,
            block_count: 0,
            long_cap: 0.0,
            last_target: 0.0,
            consumer_lead: 0.0,
            chase: false,
            depth_ewma: None,
            ahead: 0.0,
            correction: Correction::Idle,
            drop_frames_pending: 0,
            trimming: false,
            shifter: RateShifter::default(),
            corr_ring: RateRing::new(LOSS_WINDOW_MAX),
            max_corr_cents: 0.0,
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
            loss: RateRing::new(LOSS_WINDOW_MAX),
            conceal: RateRing::new(LOSS_WINDOW_MAX),
            start_pad: 0.0,
            start_pad_until_us: 0,
            path_reset_until_us: 0,
            path_grace_step: 0,
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

    /// 지연 통계까지 모두 버리고 처음부터 다시 채운다(누적 통계 카운터는 유지). 같은 피어의 스트림 성격이
    /// 바뀔 때(에코 측정 시작/끝: 상대 오디오 ↔ 되돌아온 내 오디오) 이전 도착 시각 분포를 섞지 않으려고 쓴다.
    pub fn restart(&mut self) {
        let stats = self.stats;
        *self = Self::new(self.settings);
        self.stats = stats;
    }

    fn reset_stream(&mut self) {
        self.shifter.reset();
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

    /// 목표 상·하한(샘플). 하한은 `jitter_min_ms`(0이면 없음), 상한은 `jitter_max_ms`(하한 이상).
    fn target_limits(&self) -> (f64, f64) {
        let spm = Self::samples_per_ms();
        let lo = (self.settings.min_ms.max(0.0) as f64 * spm).max(0.0);
        let hi = (self.settings.max_ms as f64 * spm).max(lo);
        (lo, hi)
    }

    /// 보정 데드밴드(샘플): (버림 진입, 삽입 진입, 버림 해제, 삽입 해제). 삽입 쪽(깊이 부족 = 은닉 위험)은 좁게,
    /// 버림 쪽(깊이 초과 = 지연)은 넓게 둔다. 삽입 진입 문턱은 목표에 포함된다(`target_samples`).
    fn deadbands(&self) -> (f64, f64, f64, f64) {
        let f = self.frame() as f64;
        let spm = Self::samples_per_ms();
        let enter_drop = (f / 2.0).clamp(spm, 2.5 * spm);
        let enter_ins = (f / 8.0).clamp(0.25 * spm, spm);
        (enter_drop, enter_ins, (enter_drop / 4.0).max(4.0), (enter_ins / 4.0).max(4.0))
    }

    /// 지터 추정(샘플): 짧은 창·버스트·긴 창 중 큰 값.
    fn jitter_est(&self) -> f64 {
        let burst = self.burst_abs.map(|a| (a - self.min_transit) as f64).unwrap_or(0.0);
        let short = self.short_cap.max(0.0);
        // 긴 창은 짧은 창 너머 초과분의 `long_weight`만 반영한다(PR #20 M4: 프로필에 따라 연속적으로).
        let long = short + (self.long_cap - short).max(0.0) * self.tuning.long_weight;
        short.max(burst).max(long)
    }

    /// 목표 깊이(샘플) = 지터 추정 + 소비자 요구 깊이(`lead`) + 삽입 진입 문턱 + 프로필 여유. 상한 `max_ms`.
    pub fn target_samples(&self) -> f64 {
        let (lo, hi) = self.target_limits();
        let (_, enter_ins, _, _) = self.deadbands();
        let pad = self.tuning.pad_frames * self.frame() as f64 + self.start_pad;
        (self.jitter_est() + self.consumer_lead + enter_ins + pad).clamp(lo, hi)
    }

    /// 소비자(워커)가 은닉 판단 시각에 p95 지연 너머로 요구하는 깊이(샘플, 48k): 은닉 마감 여유 + 피어 적응
    /// 여유(M4)만큼 앞당긴 마감과 더 채우는 양(P1-13). 목표가 하나이므로 깊이 제어(보정)·재생 시작·은닉 판단이
    /// 같은 목표 지연을 본다. 값이 바뀌어 목표가 바뀌면 깊이는 보정 램프로 따라간다. 이때는 데드밴드 안의
    /// 오차도 해제 문턱까지 보정한다(`chase`): 여유가 데드밴드보다 작게 줄면 깊이가 내려오지 않아 지연이 남았다.
    pub fn set_consumer_lead(&mut self, samples: f64) {
        let before = self.consumer_lead;
        self.consumer_lead = samples.max(0.0);
        if self.consumer_lead != before {
            self.chase = true;
        }
        self.last_target = self.target_samples();
    }

    /// 소비자가 재생 위치를 `samples`만큼 옮겼음을 알린다(깊이 추정에 바로 반영). 워커가 언더런 뒤 건너뛰기를
    /// 늘어난 목표만큼 덜 하면 깊이가 그만큼 즉시 커진다. EWMA가 이를 천천히 따라가는 동안 오차를 잘못 읽어
    /// 삽입 보정이 끼지 않게 한다.
    pub fn shift_depth(&mut self, samples: f64) {
        if let Some(e) = self.depth_ewma.as_mut() {
            *e += samples;
        }
    }

    /// 목표 중 소비자 요구 깊이(샘플). 진단·통계용(`lead_ms`).
    pub fn extra_target(&self) -> f64 {
        self.consumer_lead
    }

    /// 패킷 프레임(샘플, 48k).
    pub fn frame_samples(&self) -> usize {
        self.frame()
    }

    pub fn tuning(&self) -> JitterTuning {
        self.tuning
    }

    /// '저지연 ↔ 안정' 프로필 변경(0..1). 다음 통계 갱신부터 새 계수로 목표를 계산한다.
    /// 짧은 창 추정은 새 분위수가 더 높을 때만 바로 올리고, 낮으면 정상 hold를 거쳐 내려간다(PR #20 M3: 슬라이더를
    /// 움직일 때마다 hold 중이던 추정이 바로 떨어져 은닉이 날 수 있었다). 같은 값이면 아무것도 하지 않는다.
    pub fn set_profile(&mut self, profile: f32) {
        let tuning = JitterTuning::from_profile(profile);
        if tuning == self.tuning {
            self.settings.profile = profile;
            return;
        }
        self.settings.profile = profile;
        self.tuning = tuning;
        self.recompute_delay_stats();
        self.short_cap = self.short_cap.max(Self::grid_up(self.q_rel));
        self.update_long_cap();
        self.after_target_change();
    }

    fn grid_up(x: f64) -> f64 {
        (x.max(0.0) / TARGET_GRID).ceil() * TARGET_GRID
    }

    /// 목표가 내려갔으면 해제 문턱까지 따라간다(버림 데드밴드 안에 지연이 남지 않게).
    fn after_target_change(&mut self) {
        let t = self.target_samples();
        if t < self.last_target - 0.5 {
            self.chase = true;
        }
        self.last_target = t;
    }

    /// 짧은 창 추정 갱신: 오르면 바로, 내리는 것은 더 낮은 후보(한 격자 히스테리시스)가 `hold_ms` 이어질 때.
    fn update_short_cap(&mut self, now_us: u64) {
        let up = Self::grid_up(self.q_rel);
        if self.short_cap < 0.0 || up > self.short_cap {
            self.short_cap = up;
            self.short_lower_since = None;
            return;
        }
        let cand = Self::grid_up(self.q_rel + TARGET_GRID);
        if cand < self.short_cap {
            let since = *self.short_lower_since.get_or_insert(now_us);
            if now_us.saturating_sub(since) as f64 >= self.tuning.hold_ms * 1000.0 {
                self.short_cap = cand;
                self.short_lower_since = None;
            }
        } else {
            self.short_lower_since = None;
        }
    }

    /// 버스트 감지(패킷마다): 최근 `BURST_WIN`패킷 중 `BURST_COUNT`개 이상이 현재 추정 + max(F/2, 1ms)를 넘으면
    /// 그 최댓값까지 바로 올린다. 버스트 수준 근처의 패킷이 오는 동안 유지하고, 끝나고 `burst_hold_ms`가 지나면 없앤다.
    fn detect_burst(&mut self, transit: i64, now_us: u64) {
        let rel = (transit - self.min_transit).max(0);
        self.recent_rel[self.recent_pos] = rel;
        self.recent_pos = (self.recent_pos + 1) % BURST_WIN;
        let spm = Self::samples_per_ms();
        let spike = (self.frame() as f64 / 2.0).max(spm) * self.tuning.burst_scale;
        // 판정 기준은 j가 아니라 짧은 창 p99와 j 중 큰 값: 꼬리가 긴 지터에서 분위수 너머 패킷은 늘 있다(M1).
        let base = self.jitter_est().max(self.q99_rel);
        let n = self.transit_count.min(BURST_WIN);
        // 시간 군집: 최근 BURST_CLUSTER개 중 BURST_COUNT개가 넘어야 한다. 버스트는 연속 패킷을 늦추고, 꼬리 패킷은 따로 온다.
        let recent = |i: usize| self.recent_rel[(self.recent_pos + BURST_WIN - 1 - i) % BURST_WIN];
        // 창에 표본이 50개 미만이면(재생 시작·경로 변경 직후) p99 기준이 아직 없어 정상 지터도 버스트로 본다.
        let m = if self.transit_count >= 50 { n.min(BURST_CLUSTER) } else { 0 };
        let over: Vec<i64> = (0..m).map(recent).filter(|&r| r as f64 > base + spike).collect();
        if over.len() >= BURST_COUNT {
            let peak = over.iter().copied().max().unwrap_or(0) as f64;
            let level = self.min_transit + Self::grid_up(peak) as i64;
            if self.burst_abs.is_none_or(|a| level > a) {
                self.stats.burst_events += 1;
            }
            self.burst_abs = Some(self.burst_abs.map_or(level, |a| a.max(level)));
            self.burst_at_us = now_us;
        } else if let Some(a) = self.burst_abs {
            // 유지도 판정과 대칭: 최근 BURST_WIN개 중 BURST_COUNT개가 버스트 수준 근처여야 hold를 갱신한다.
            let near = (0..n).map(recent).filter(|&r| (self.min_transit + r) as f64 + spike >= a as f64).count();
            if near >= BURST_COUNT {
                self.burst_at_us = now_us;
            } else if now_us.saturating_sub(self.burst_at_us) as f64 >= self.tuning.burst_hold_ms * 1000.0 {
                self.burst_abs = None;
            }
        }
    }

    /// 긴 창 블록: 1초마다 블록 분위수를 남기고, 최근 `long_blocks`개 중 `LONG_RANK`번째 큰 값을 긴 창 추정으로 쓴다.
    fn push_block(&mut self, transit: i64, now_us: u64) {
        let start = *self.block_start_us.get_or_insert(now_us);
        if now_us.saturating_sub(start) >= BLOCK_US {
            let n = self.block_rel.len();
            if n >= 20 {
                let k = ((n as f64 * self.tuning.quantile).ceil() as usize).clamp(1, n) - 1;
                let (_, q, _) = self.block_rel.select_nth_unstable(k);
                self.block_q[self.block_pos] = *q as f64;
                self.block_pos = (self.block_pos + 1) % LONG_BLOCKS_MAX;
                self.block_count = (self.block_count + 1).min(LONG_BLOCKS_MAX);
            }
            self.block_rel.clear();
            self.block_start_us = Some(now_us);
            self.update_long_cap();
        }
        if self.block_rel.len() < BLOCK_SAMPLES_MAX {
            self.block_rel.push((transit - self.min_transit).max(0));
        }
    }

    fn update_long_cap(&mut self) {
        let n = self.block_count.min(self.tuning.long_blocks);
        let rank = LONG_RANK.max(self.tuning.long_blocks.div_ceil(6));
        if n < rank {
            self.long_cap = 0.0;
            return;
        }
        // 최근 n개(링 역순)에서 LONG_RANK번째로 큰 값. n ≤ 32라 정렬해도 싸다(1초에 한 번).
        let mut v = [0f64; LONG_BLOCKS_MAX];
        for (i, slot) in v.iter_mut().enumerate().take(n) {
            *slot = self.block_q[(self.block_pos + LONG_BLOCKS_MAX - 1 - i) % LONG_BLOCKS_MAX];
        }
        let v = &mut v[..n];
        v.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        self.long_cap = Self::grid_up(v[rank - 1]);
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
                let back_limit = ((self.target_samples() / self.frame() as f64).ceil() as i32 + 1).clamp(2, 16);
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
        // 경로 변경(릴레이 → 직접 승격 등)으로 지연이 한 번에 크게 줄면 이전 경로의 도착 지연이 모두 "늦은" 표본이
        // 되어 창이 빠질 때까지(+ hold) 목표가 옛 경로 지연만큼 남는다. 최소 지연보다 max(5ms, F) 넘게 이르면
        // 지연 통계를 새 경로부터 다시 모은다(깊이 초과분은 패킷 단위 버림이 걷어 낸다). 지터가 큰 회선에서 잘못
        // 걸리지 않도록 문턱은 현재 지터 폭(분위수)보다도 크고, 창에 표본이 50개 이상일 때만 본다.
        let step = (5.0 * Self::samples_per_ms()).max(self.frame() as f64).max(self.q_rel) as i64;
        if self.transit_count >= 50 && transit < self.min_transit - step {
            self.reset_delay_stats(now_us);
            self.path_reset_until_us = now_us + PATH_GRACE_US;
            // 유예 비교 문턱은 리셋 전 값으로 고정한다(PR #20 M2: 리셋 뒤 q가 0으로 다시 계산되면 문턱이 max(5ms, F)로
            // 줄어 새 경로의 정상 지터 패킷까지 옛 경로로 보고 빼 버렸다).
            self.path_grace_step = step;
        } else if now_us < self.path_reset_until_us && transit > self.min_transit + self.path_grace_step {
            // 경로 변경 직후 옛 경로로 오던(이미 보낸) 패킷: 새 경로 통계에 넣지 않는다.
            return;
        }
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
            self.since_stats = 0;
            self.recompute_delay_stats();
            self.update_short_cap(now_us);
        } else if transit < self.min_transit {
            self.min_transit = transit;
        }
        self.detect_burst(transit, now_us);
        self.push_block(transit, now_us);
        self.after_target_change();
    }

    /// 경로 변경: 절대 지연에 묶인 것(transit 창, 버스트 절대 수준, 현재 블록)만 버린다. 짧은 창 추정과 긴 창 블록
    /// 분위수는 상대 지터라 경로가 바뀌어도 의미가 있으므로 남기고, 짧은 창 추정은 정상 hold로 내려가게 한다(PR #20 M2:
    /// 전부 0으로 두면 목표가 lead + 문턱까지 떨어져 새 경로 지터가 크면 표본이 쌓일 때까지 과소추정했다).
    fn reset_delay_stats(&mut self, now_us: u64) {
        self.transit_count = 0;
        self.transit_pos = 0;
        self.since_stats = 0;
        self.last_transit = None;
        self.short_lower_since = Some(now_us);
        self.burst_abs = None;
        self.recent_rel = [0; BURST_WIN];
        self.block_rel.clear();
        self.block_start_us = None;
    }

    fn recompute_delay_stats(&mut self) {
        let n = self.transit_count;
        if n == 0 {
            return;
        }
        self.scratch.clear();
        self.scratch.extend_from_slice(&self.transits[..n]);
        let min = self.scratch.iter().copied().min().unwrap_or(0);
        let k = ((n as f64 * self.tuning.quantile).ceil() as usize).clamp(1, n) - 1;
        let (_, p, _) = self.scratch.select_nth_unstable(k);
        self.q_rel = (*p - min) as f64;
        let k99 = ((n as f64 * 0.99).ceil() as usize).clamp(1, n) - 1;
        let (_, p99, _) = self.scratch.select_nth_unstable(k99);
        self.q99_rel = (*p99 - min) as f64;
        self.min_transit = min;
    }

    /// 현재 재생 위치 기준 깊이(샘플). 재생 위치(소스 링 머리)의 ts = 다음에 꺼낼 샘플의 ts − 이미
    /// 꺼냈지만 재생 전인 양(`ahead`). 링에 있는 샘플이 더 오래되었으므로 깊이는 그만큼 크다.
    fn depth_now(&self, now_us: u64) -> f64 {
        let next_ts = self.fifo_end_ts - (self.fifo.len() / 2) as i64;
        (Self::now_samples(now_us) - next_ts - self.min_transit) as f64 + self.ahead
    }

    /// 다음 seq의 시작 타임스탬프 기준 깊이(버퍼링 중 판단용). 소스 링에 이전 스트림이 남아 있으면
    /// 그만큼 늦게 재생되므로 깊이에 더한다.
    fn depth_if_start(&self, now_us: u64) -> Option<f64> {
        let slot = &self.slots[Self::slot_index(self.next_seq)];
        if !(slot.valid && slot.seq == self.next_seq) {
            return None;
        }
        Some((Self::now_samples(now_us) - self.ts_ext(slot.ts) - self.min_transit) as f64 + self.ahead)
    }

    /// 은닉 없이(실제 받은 데이터로만) 지금 꺼낼 수 있는 프레임 수(최소 보장치). 콜백 구동 믹스에서 워커는
    /// 이만큼만 미리 꺼내고, 은닉은 출력 콜백 마감 직전에만 한다(늦게 오는 패킷을 기다림).
    /// FIFO에 남은 패킷 꼬리도 포함한다: 청크 단위로만 꺼내면 꼬리(< 청크)가 남아 마감에 은닉이 끼었다.
    pub fn real_frames(&self) -> usize {
        if self.state != State::Playing {
            return 0;
        }
        let fifo = self.fifo.len() / 2;
        // 다음 패킷이 있으면 패킷 하나(보정으로 최대 2샘플 줄어듦)를 더 꺼낼 수 있다. 패킷 단위 버림이
        // 걸려 있으면 그다음 패킷까지 있어야 은닉이 끼지 않는다.
        if self.slot_has(self.next_seq) && (self.drop_frames_pending == 0 || self.slot_has(self.next_seq.wrapping_add(1))) {
            fifo + self.slots[Self::slot_index(self.next_seq)].frame.saturating_sub(2)
        } else {
            fifo
        }
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
    ///
    /// `ahead`: 이미 꺼냈지만 아직 재생되지 않은 샘플 수(48k 환산). 콜백 구동 믹스에서는 소스 링과
    /// 출력 리샘플러에 있는 양이다. 깊이는 실제 재생 위치를 기준으로 재므로 이것을 뺀다. 그래서
    /// `bufferMs`는 소스 링까지 포함한 수신측 버퍼 지연이다.
    pub fn end_burst(&mut self, now_us: u64, wall_us: u64, ahead: f64) {
        self.ahead = ahead.max(0.0);
        match self.state {
            State::Buffering => self.try_start(now_us),
            State::Playing => {
                if self.start_pad > 0.0 && now_us >= self.start_pad_until_us {
                    self.start_pad = 0.0;
                    self.after_target_change();
                }
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
        // 데드밴드(비대칭, P1-5): 버림 진입 F/2를 1ms~2.5ms로 제한(10ms 프레임에서 목표 +5ms까지 머물지 않게, PR #5 L-1),
        // 삽입 진입은 F/8을 0.25~1ms로 제한하고 목표에 포함한다(깊이가 목표 − 삽입 진입까지 내려가도 지터 추정 + lead 이상).
        let (enter_drop, enter_ins, exit_drop, exit_ins) = self.deadbands();
        // 소비자 요구로 목표가 바뀐 직후에는 해제 문턱 밖의 오차부터 따라간다(P1-13, `set_consumer_lead`).
        // 목표가 내려간 직후(`after_target_change`)에도 같은 방식으로 버림 데드밴드 안에 지연을 남기지 않는다(P1-5).
        if self.chase && ((err >= 0.0 && err <= exit_drop) || (err < 0.0 && -err <= exit_ins)) {
            self.chase = false;
        }
        let (enter_drop, enter_ins) = if self.chase { (exit_drop, exit_ins) } else { (enter_drop, enter_ins) };
        self.correction = match self.correction {
            Correction::Idle if err > enter_drop => Correction::Drop,
            Correction::Idle if err < -enter_ins => Correction::Insert,
            Correction::Drop if err <= exit_drop => Correction::Idle,
            Correction::Insert if err >= -exit_ins => Correction::Idle,
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
        self.start_pad = (self.frame() as f64).min(2.5 * Self::samples_per_ms());
        self.start_pad_until_us = now_us + START_PAD_US;
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

    /// 5초 창 크기(패킷).
    fn rate_window(&self) -> usize {
        (5000.0 * Self::samples_per_ms()) as usize / self.frame()
    }

    fn push_loss(&mut self, lost: bool) {
        let win = self.rate_window();
        self.loss.push(lost, win);
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
        let f = self.frame() as f64;
        if let Some(e) = self.depth_ewma.as_mut() {
            *e -= f;
        }
    }

    /// 패킷 하나만큼 FIFO에 생산. 재버퍼링으로 전환하면 false.
    fn produce_packet(&mut self) -> bool {
        // 늦은 패킷을 손실로(H1, NetEQ류): 언더런 은닉이 이미 그 자리를 채워 깊이가 늘었고 지금 목표보다 F/2 이상 깊으면,
        // 늦게 온 패킷을 재생하지 않고 버린다(디코드해 디코더 상태 유지 + 크로스페이드). 은닉은 이미 났으므로 은닉률은
        // 그대로이고, 늘어난 1F를 샘플 버림 램프로 되돌리는 구간(피치 편차)이 없어진다.
        // 깊이는 늦은 패킷을 꺼내는 순간(ts가 은닉한 만큼 되감길 때) 한 번에 늘어나므로 그 되감김 `back`으로 판단한다.
        if self.state == State::Playing && self.slot_has(self.next_seq) && self.slot_has(self.next_seq.wrapping_add(1)) {
            let f = self.frame() as f64;
            let back = (self.fifo_end_ts - self.ts_ext(self.slots[Self::slot_index(self.next_seq)].ts)) as f64;
            if back >= f / 2.0 {
                let err = self.depth_ewma.map(|e| e + back - self.target_samples()).unwrap_or(0.0);
                if err >= f / 2.0 {
                    if let Some(e) = self.depth_ewma.as_mut() {
                        *e += back; // drop_one_packet이 F를 뺀다: 순증 back − F ≈ 0
                    }
                    self.drop_one_packet();
                    self.stats.late_dropped += 1;
                }
            }
        }
        if self.drop_frames_pending > 0 && self.has_later_packet() && self.slot_has(self.next_seq) {
            self.drop_frames_pending -= 1;
            self.drop_one_packet();
            self.stats.dropped_frames += 1;
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
            let win = self.rate_window();
            self.conceal.push(false, win);
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
            let win = self.rate_window();
            self.conceal.push(true, win);
            if lost {
                self.stats.concealed_loss += 1;
                self.next_seq = seq.wrapping_add(1);
                self.push_loss(true);
            }
            self.fifo_end_ts += frames as i64;
        }
        self.stats.produced += 1;

        let win = self.rate_window();
        self.corr_ring.push(self.correction != Correction::Idle, win);
        // 샘플 단위 보정
        let mut out_frames = frames;
        let mut use_shift = false;
        {
            // 비율 상한 리샘플러(H1): 버림 1 + r, 삽입 1 − r. 보정이 끝나면 위상이 정수가 될 때까지 같은 방향으로 이어 간다.
            let rstep = match self.correction {
                Correction::Drop => 1.0 + MAX_CORR_RATE,
                Correction::Insert => 1.0 - MAX_CORR_RATE,
                Correction::Idle => {
                    let fr = self.shifter.pos - self.shifter.pos.round();
                    // 정수 위상(−1 또는 0)이면 보간 없이 원 샘플을 그대로 낸다(−1은 1샘플 지연된 무손실 상태).
                    if fr.abs() < PHASE_SNAP || self.shifter.settle > 64 {
                        self.shifter.pos = self.shifter.pos.round();
                        self.shifter.settle = 0;
                        1.0
                    } else {
                        self.shifter.settle += 1;
                        self.shifter.last_step
                    }
                }
            };
            if rstep != 1.0 || !self.shifter.idle() {
                if self.correction != Correction::Idle {
                    self.shifter.settle = 0;
                }
                out_frames = self.shifter.process(&self.dec_buf[..frames * ch], ch, frames, &mut self.shift_buf, rstep);
                use_shift = true;
                let d = frames as f64 - out_frames as f64;
                if d > 0.0 {
                    self.stats.samples_dropped += d as u64;
                } else {
                    self.stats.samples_inserted += (-d) as u64;
                }
                if let Some(e) = self.depth_ewma.as_mut() {
                    *e -= d;
                }
                let cents = (1200.0 * rstep.log2()).abs() as f32;
                self.max_corr_cents = self.max_corr_cents.max(cents);
            } else {
                for c in 0..ch {
                    self.shifter.prev[c] = self.dec_buf[(frames - 1) * ch + c];
                }
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
        s.loss_pct = self.loss.pct();
        s.conceal_pct = self.conceal.pct();
        s.lead_ms = self.consumer_lead as f32 / spm;
        s.jitter_est_ms = self.jitter_est() as f32 / spm;
        s.corr_time_pct = self.corr_ring.pct();
        s.max_corr_cents = self.max_corr_cents;
        s
    }

    /// 깊이 부족분(샘플): 정상 범위(목표 − 삽입 진입 문턱, 이미 목표에 포함)보다 얕은 만큼. 재생 중이 아니면 0.
    /// 워커의 유효 여유 계산용(PR #20 L4: 정상 데드밴드까지 부족분으로 세면 유효 여유가 늘 조금 깎였다).
    pub fn depth_shortfall(&self) -> f64 {
        let (_, enter_ins, _, _) = self.deadbands();
        (-(self.depth_error() + enter_ins)).max(0.0)
    }

    /// 깊이 오차(샘플, 깊이 EWMA − 목표). 재생 중이 아니면 0.
    pub fn depth_error(&self) -> f64 {
        match (self.state, self.depth_ewma) {
            (State::Playing, Some(e)) => e - self.target_samples(),
            _ => 0.0,
        }
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
        /// 시작 3초 이후 보정 횟수(재생 시작 여유 1프레임을 1초 뒤 걷어 내는 보정 제외)
        corrections_after_3s: u64,
        /// 5초 시점 통계(정착 후 보정 방향 확인용)
        stats_at_5s: JitterStats,
        /// 인접 샘플 불연속 수(L 채널, 차 > 0.08)
        discontinuities: usize,
    }

    /// 이벤트 시뮬레이션. 송신 클럭은 (1+drift_ppm)배 빠르고, 네트워크 지연은 base + U(0,jitter),
    /// 손실 확률은 loss다. 수신측은 `burst`마다 `chunk` 단위로 연달아 pop한 뒤 `end_burst`를 부른다.
    fn simulate(p: Sim) -> SimResult {
        let mut rng = rand::rngs::StdRng::seed_from_u64(p.seed);
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
        // 엔진의 워커는 항상 은닉 마감 여유(1.5ms) 이상을 소비자 요구로 알린다(`PeerFeed::consumer_lead`).
        jb.set_consumer_lead(72.0);
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
        let mut corr_at_3s = None;
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
            jb.end_burst(bt as u64, bt as u64, 0.0);
            if jb.is_playing() {
                started = true;
            }
            if bt >= 3e6 && corr_at_3s.is_none() {
                corr_at_3s = Some(jb.stats().corrections());
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
            corrections_after_3s: stats.corrections() - corr_at_3s.unwrap_or(0),
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
            // 재생 시작 1초 뒤 시작 여유(1프레임)를 걷어 내는 보정 한 번은 빼고 센다(P1-5).
            assert_eq!(r.corrections_after_3s, 0, "드리프트 0이면 보정 0: frame {frame} burst {burst}: {:?}", r.stats);
            assert!(r.stats.corrections() <= frame as u64, "시작 여유 제거 보정은 1프레임 이하: {:?}", r.stats);
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
        assert_eq!(r.corrections_after_3s, 0, "{:?}", r.stats);
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
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
        let mut tx = Sender::new(CodecKind::Pcm16, 240, 10, 0);
        let pkts: Vec<_> = (0..20).map(|_| tx.next()).collect();
        let mut out = vec![0f32; 480];
        let mut t = 0u64;
        for (h, p) in pkts.iter().take(10) {
            jb.insert(h, p, t);
            jb.pop(&mut out);
            jb.end_burst(t, t, 0.0);
            t += 5000;
        }
        for _ in 0..5 {
            jb.pop(&mut out);
            jb.end_burst(t, t, 0.0);
            t += 5000;
        }
        let before = jb.stats().late;
        jb.insert(&pkts[0].0, &pkts[0].1, t);
        assert_eq!(jb.stats().late, before + 1);
    }

    #[test]
    fn replayed_old_packet_cannot_rewind_buffering() {
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
        let mut tx = Sender::new(CodecKind::Pcm16, 240, 1000, 0);
        let pkts: Vec<_> = (0..300).map(|_| tx.next()).collect();
        jb.insert(&pkts[250].0, &pkts[250].1, 0);
        // 200개 이전 패킷 재전송 → 버퍼링 중이라도 시작점을 크게 되돌리지 않음
        jb.insert(&pkts[50].0, &pkts[50].1, 10);
        assert_eq!(jb.stats().late, 1);
    }

    #[test]
    fn pcm_concealment_fades_then_silence() {
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
        let mut tx = Sender::new(CodecKind::Pcm16, 240, 0, 0);
        let mut out = vec![0f32; 480];
        let mut t = 0u64;
        for _ in 0..20 {
            let (h, p) = tx.next();
            jb.insert(&h, &p, t);
            jb.pop(&mut out);
            jb.end_burst(t, t, 0.0);
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
            jb.end_burst(t, t, 0.0);
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
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
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
                jb.end_burst(t, t, 0.0);
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
    fn profile_tuning_and_runtime_change() {
        // 균형(0.5)이 기본 계수: p95, 유지 2초. 저지연 → 안정으로 갈수록 분위수·유지 시간·긴 창이 커진다.
        let (lo, mid, hi) = (JitterTuning::from_profile(0.0), JitterTuning::from_profile(0.5), JitterTuning::from_profile(1.0));
        assert!((mid.quantile - 0.95).abs() < 1e-9 && (mid.hold_ms - 2000.0).abs() < 1e-9 && mid.pad_frames == 0.0);
        assert!(lo.quantile < mid.quantile && mid.quantile < hi.quantile);
        assert!(lo.hold_ms < mid.hold_ms && mid.hold_ms < hi.hold_ms);
        assert!(lo.long_blocks < mid.long_blocks && mid.long_blocks < hi.long_blocks);
        assert!(lo.margin_scale < mid.margin_scale && mid.margin_scale < hi.margin_scale);
        assert_eq!(JitterTuning::from_profile(f32::NAN), mid, "잘못된 값은 균형");
        // 실행 중 변경: 지터 U(0, 10ms) 스트림에서 안정으로 바꾸면 목표가 바로 오르고, 저지연으로 바꾸면 내려간다.
        let mut rng = rand::rngs::StdRng::seed_from_u64(9);
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
        let mut tx = Sender::new(CodecKind::Pcm16, 240, 0, 0);
        for k in 0..400u64 {
            let (h, p) = tx.next();
            jb.insert(&h, &p, k * 5000 + 3000 + rng.gen_range(0..10_000));
        }
        let bal = jb.target_samples();
        jb.set_profile(1.0);
        let stable = jb.target_samples();
        jb.set_profile(1.0);
        assert_eq!(jb.target_samples(), stable, "같은 값이면 변화 없음");
        // 저지연으로 내려도 짧은 창 추정은 hold를 거친다(M3): 바로 내려가는 것은 pad·긴 창 몫뿐이다.
        jb.set_profile(0.0);
        let low_now = jb.target_samples();
        assert!(low_now < stable, "pad 등은 바로 빠짐: {low_now} vs {stable}");
        let (h, p) = tx.next();
        jb.insert(&h, &p, 400 * 5000 + 3000);
        assert!(jb.target_samples() >= low_now - 0.5, "hold 전에는 짧은 창 추정이 내려가지 않음");
        for k in 401..1000u64 {
            let (h, p) = tx.next();
            jb.insert(&h, &p, k * 5000 + 3000 + rng.gen_range(0..10_000));
        }
        let low = jb.target_samples();
        assert!(low < bal && bal < stable, "목표 저지연 {low} < 균형 {bal} < 안정 {stable}");
    }

    #[test]
    fn path_step_down_resets_delay_stats() {
        // 릴레이 → 직접 승격처럼 지연이 18ms 한 번에 줄면 옛 경로 표본을 버리고 목표가 바로 새 경로 기준으로 내려간다.
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
        jb.set_consumer_lead(72.0);
        let mut tx = Sender::new(CodecKind::Pcm16, 240, 0, 0);
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        for k in 0..600u64 {
            let (h, p) = tx.next();
            // 전환 직후 20ms 동안 옛 경로로 보낸 패킷 4개가 새 경로 패킷 사이에 섞여 도착한다.
            let delay = if k < 400 || (402..406).contains(&k) { 21_000 } else { 3000 } + rng.gen_range(0..500);
            jb.insert(&h, &p, k * 5000 + delay);
            if k == 399 {
                assert!(jb.stats().target_ms < 6.0, "전제: 안정 회선 목표 {:?}", jb.stats());
            }
        }
        assert!(jb.stats().target_ms < 6.0, "승격 1초 뒤 목표가 옛 경로 지연(18ms)을 끌지 않음: {:?}", jb.stats());
    }

    #[test]
    fn target_change_converges_within_rate_bound() {
        // PR #20: 보정 속도 상한(0.25%)의 짝 — 목표가 5ms 바뀌면 깊이가 `correction_catch_up_secs` × 1.2 + 1프레임 안에
        // 새 목표의 해제 문턱 안으로 들어와야 한다(실시간 짝: tests/loopback.rs link_target_change_converges).
        for frame in [120usize, 240] {
            let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
            jb.set_consumer_lead(72.0);
            let mut tx = Sender::new(CodecKind::Pcm16, frame, 0, 0);
            let (exit_drop, exit_ins) = correction_exit_ms(frame);
            let mut out = vec![0f32; frame * 2];
            let period = frame as u64 * 1_000_000 / 48_000;
            let mut t = 0u64;
            let mut results = Vec::new();
            for (at, lead) in [(5_000_000u64, 72.0 + 240.0), (15_000_000, 72.0)] {
                let mut changed_at = None;
                let mut conv = None;
                while t < at + 9_000_000 {
                    let (h, p) = tx.next();
                    jb.insert(&h, &p, t + 3000);
                    jb.pop(&mut out);
                    jb.end_burst(t, t, 0.0);
                    if t >= at && changed_at.is_none() {
                        jb.set_consumer_lead(lead);
                        changed_at = Some(t);
                    }
                    if let (Some(c), None) = (changed_at, conv) {
                        let s = jb.stats();
                        let err = (s.buffer_ms - s.target_ms) as f64;
                        // 반 샘플(0.0104ms) 허용: 해제 문턱 경계의 부동소수 비교
                        if t > c && err <= exit_drop + 0.0105 && err >= -exit_ins - 0.0105 {
                            conv = Some((t - c) as f64 / 1e6);
                        }
                    }
                    t += period;
                }
                let conv = conv.expect("수렴하지 않음");
                let bound = correction_catch_up_secs(240.0) * 1.2 + frame as f64 / 48_000.0;
                println!("목표 변경 F {frame} lead {lead}: 수렴 {conv:.2}s (상한 {bound:.2}s, 이론 {:.2}s)", correction_catch_up_secs(240.0));
                assert!(conv <= bound, "F {frame}: 수렴 {conv:.2}s > 상한 {bound:.2}s");
                results.push(conv);
            }
            assert_eq!(jb.stats().concealed, 0);
        }
    }

    #[test]
    fn rate_shifter_caps_pitch_and_is_continuous() {
        // 버림 1 + r로 1초(200패킷 × 240) 램프: 버린 샘플 = r × 48000 ± 1, 패킷 경계 포함 연속(인접 차가 원 신호 수준).
        let mut rs = RateShifter::default();
        let f = 240;
        let mut dst = vec![0f32; (f + 4) * 2];
        let (mut out_total, mut prev) = (0usize, None::<f32>);
        let mut max_step = 0f32;
        for k in 0..200 {
            let src: Vec<f32> = (0..f).map(|i| ((k * f + i) as f32 * 0.01).sin()).collect();
            let n = rs.process(&src, 1, f, &mut dst, 1.0 + MAX_CORR_RATE);
            for &y in &dst[..n] {
                if let Some(p) = prev {
                    max_step = max_step.max((y - p).abs());
                }
                prev = Some(y);
            }
            out_total += n;
        }
        let dropped = 200 * f - out_total;
        assert!((dropped as f64 - MAX_CORR_RATE * 48_000.0).abs() <= 2.0, "버린 샘플 {dropped}");
        assert!(max_step < 0.0102, "패킷 경계 불연속 없음: {max_step}");
        assert!((1200.0 * (1.0 + MAX_CORR_RATE).log2() - 4.33).abs() < 0.01, "상한 약 4.3센트");
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
