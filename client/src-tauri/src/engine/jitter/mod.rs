//! 피어별 적응형 지터 버퍼 + 드리프트 보정 + PLC (설계 문서 1.2절).
//!
//! ## 파일
//! - `mod.rs`: `JitterBuffer` 상태·수명·공개 API, 패킷 삽입(슬롯 링)
//! - `tuning.rs`: 설정과 프로필 → 계수
//! - `target.rs`: 목표 깊이 추정(지연 통계, 짧은 창·버스트·긴 창, 경로 변경, 목표 합성)
//! - `late_budget.rs`: 늦은 패킷 예산(`tail_extra`)
//! - `playout.rs`: 재생 시작, 패킷 생산(디코드·FEC/PLC·PCM 페이드), 버림, 언더런 정산
//! - `correction.rs`: 깊이 측정(`end_burst`)·데드밴드 보정·비율 리샘플러, 샘플 시프트 유틸
//! - `stats.rs`: `JitterStats`, 비율 창, 통계 스냅샷
//! - `tests.rs`: 결정적 단위 시뮬레이션
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

mod correction;
mod late_budget;
mod playout;
mod stats;
mod target;
#[cfg(test)]
mod tests;
mod tuning;

use std::collections::VecDeque;

use super::codec::Decoder;
use super::config::{frame_samples_from_code, INTERNAL_RATE, MAX_PAYLOAD_BYTES};
use crate::net::packet::Header;

pub use correction::{correction_catch_up_secs, correction_exit_ms, shift_n, shift_one, MAX_CORR_RATE};
pub use stats::JitterStats;
pub use tuning::{JitterSettings, JitterTuning};

use correction::RateShifter;
use stats::RateRing;

/// 슬롯 링 크기: 2의 거듭제곱(u32 seq 랩에서도 슬롯 충돌 없음, 리뷰 L1). 2.5ms 기준 2.56초.
const SLOT_COUNT: usize = 1024;
const SLOT_MASK: u32 = SLOT_COUNT as u32 - 1;
/// 지연 통계 창 크기.
const DELAY_WINDOW: usize = 200;
/// 최대 패킷 프레임(10ms).
const MAX_PKT_FRAME: usize = 480;
/// 손실률 창: 최대 2000패킷(2.5ms × 5초).
const LOSS_WINDOW_MAX: usize = 2000;
/// 목표 격자(샘플, 0.25ms). 짧은·버스트·긴 창 추정을 이 단위로 올림한다.
pub const TARGET_GRID: f64 = 12.0;
/// 버스트 감지 창(패킷, `target`).
const BURST_WIN: usize = 16;
/// 긴 창 블록 분위수 링의 최대 블록 수와 블록당 최대 표본(`target`).
const LONG_BLOCKS_MAX: usize = 32;
const BLOCK_SAMPLES_MAX: usize = 1024;
/// 늦은 패킷 예산 블록 링 길이(`late_budget`).
const LATE_BLOCKS: usize = 16;

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
    /// 아직 seq와 짝지어지지 않은 언더런 은닉 프레임 수(P1-6a). 언더런 은닉은 seq를 유지한 채 재생 타임라인을
    /// F만큼 소비한다. 그 seq가 결국 손실로 판정되면(뒤 패킷 도착) 이미 낸 언더런 은닉이 그 자리를 메운 것이므로
    /// 손실 은닉을 또 만들지 않고 seq만 넘긴다(이중 은닉 제거, `settle_underrun_debt`). 실제 패킷을 재생하면 0
    /// (되감긴 타임라인은 샘플 보정이 되돌린다), 패킷을 버리면(H1 늦은 패킷·trimming) 1 줄어든다.
    /// 단위는 프레임 수이고 스킵은 seq 단위다: 언더런 중에 송신 프레임 크기가 바뀌면(2.5 ↔ 5ms) 그 차이만큼
    /// 타임라인이 어긋나며, 이 어긋남은 깊이 보정(샘플 단위)이 흡수한다(드물고 작다, PR #58 L3).
    underrun_debt: usize,

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

    /// 늦은 패킷 예산(P1-6b): 현재 1초 블록의 늦은 도착 수·도착 패킷 수, 블록별 늦은 비율(예산의 `LATE_CLIP`배로 자름)·
    /// 실제 손실률 링. 늦은 도착·손실·도착 패킷 모두 재생 중에만 센다(P1-6c, PR #63 리뷰 L4: 이전에는 도착 패킷만
    /// 버퍼링 중에도 세서 재버퍼가 낀 블록의 비율이 희석됐고, 경로 변경 유예 중 옛 경로 패킷은 늦은 도착만 셌다).
    /// 남은 비대칭: 언더런 은닉이 2번 이상 쌓인 뒤(debt ≥ 2) 도착한 패킷 하나는 늦은 도착 1개로 센다 — 늦게 온
    /// 패킷 수를 세는 것이므로 의도된 것이고, 은닉 수와의 차이는 창 평균에서 작다.
    late_block: u32,
    pkts_block: u32,
    /// 현재 블록에서 손실로 넘긴 seq 수와 그중 나중에 도착한(늦은) 수. 차이가 실제 손실이다.
    lost_block: u32,
    late_seq_block: u32,
    late_rates: [f64; LATE_BLOCKS],
    loss_rates: [f64; LATE_BLOCKS],
    loss_count: usize,
    late_pos: usize,
    /// 늦은 비율이 예산을 넘는 동안 목표에 더하는 깊이(샘플, 0..=F).
    tail_extra: f64,
    /// 마지막 조정 뒤 지난 블록 수(최대 `LATE_BLOCKS`).
    tail_fresh: usize,
    /// 재생 시작 직후 예산 판단에서 뺄 남은 블록 수(`LATE_SKIP_BLOCKS`, PR #63 리뷰 L3).
    tail_skip: u32,

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
            underrun_debt: 0,
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
            late_block: 0,
            pkts_block: 0,
            lost_block: 0,
            late_seq_block: 0,
            late_rates: [0.0; LATE_BLOCKS],
            loss_rates: [0.0; LATE_BLOCKS],
            loss_count: 0,
            late_pos: 0,
            tail_extra: 0.0,
            tail_fresh: 0,
            tail_skip: 0,
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
        self.underrun_debt = 0;
        self.last_frame_valid = false;
        self.xfade_len = 0;
        self.reset_tail(true);
    }

    /// 패킷 프레임 길이(샘플).
    fn frame(&self) -> usize {
        self.pkt_frame.max(1)
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
                    // 블록 경계(`record_delay`) 뒤에 세어 늦은 도착과 분모가 같은 블록에 들어가게 한다.
                    self.record_delay(hdr.timestamp, now_us);
                    self.note_late();
                    self.note_pkt();
                    if self.state == State::Playing {
                        self.late_seq_block = self.late_seq_block.saturating_add(1);
                    }
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
        self.note_pkt();
    }

    /// 실제로 받아 둔(또는 그 사이 빈) 샘플 수: 다음 seq부터 최고 seq까지 + FIFO.
    fn queued_samples(&self) -> f64 {
        let span = seq_diff(self.highest, self.next_seq) + 1;
        span.max(0) as f64 * self.frame() as f64 + (self.fifo.len() / 2) as f64
    }

    fn has_later_packet(&self) -> bool {
        seq_diff(self.highest, self.next_seq) > 0
    }

    fn slot_has(&self, seq: u32) -> bool {
        let s = &self.slots[Self::slot_index(seq)];
        s.valid && s.seq == seq
    }

    /// 녹음 마커용 누적 카운터만(은닉 패킷 수, 버린 패킷 수). `stats()`의 표시용 계산을 건너뛴다(PR #62 L3).
    #[inline]
    pub fn mark_counters(&self) -> (u64, u64) {
        (self.stats.concealed, self.stats.dropped_frames)
    }

    pub fn is_playing(&self) -> bool {
        self.state == State::Playing
    }
}
