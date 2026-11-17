//! 피어별 적응형 지터 버퍼 + 드리프트 보정 + PLC (설계 문서 1.2절).
//!
//! ## 파일
//! - `mod.rs`: `JitterBuffer` 상태·수명·공개 API, 패킷 삽입(슬롯 링)
//! - `tuning.rs`: 설정과 프로필 → 계수
//! - `delay.rs`: 지연 통계(`DelayStats`: transit 창, 최소 transit·분위수, RFC 지터)
//! - `target.rs`: 목표 깊이 추정(`TargetEst`: 짧은 창·버스트·긴 창, 경로 변경)과 목표 합성
//! - `late_budget.rs`: 늦은 패킷 예산(`LateBudget`: `tail_extra`)
//! - `playout.rs`: 재생 시작, 패킷 생산(슬롯 → 디코드/은닉 → 보정 → FIFO), 버림, 언더런 정산
//! - `conceal.rs`: 디코드·은닉(`Concealer`: 디코더 풀, FEC/PLC·PCM 페이드, 버림 크로스페이드, 언더런 빚, 손실·은닉 창)
//! - `correction.rs`: 깊이 측정(`end_burst`)·데드밴드 보정(`DepthCtl`)·비율 리샘플러, 샘플 시프트 유틸
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

mod conceal;
mod correction;
mod delay;
mod late_budget;
mod playout;
mod stats;
mod target;
#[cfg(test)]
mod tests;
mod tuning;

use std::collections::VecDeque;

use crate::config::{frame_samples_from_code, INTERNAL_RATE, MAX_PAYLOAD_BYTES};
use crate::packet::Header;

pub use correction::{correction_catch_up_secs, correction_exit_ms, shift_one};
#[cfg(test)]
use correction::MAX_CORR_RATE;
pub use stats::JitterStats;
pub use tuning::{JitterSettings, JitterTuning};

use conceal::{ConcealBufs, Concealer};
use correction::{DepthBufs, DepthCtl};
use delay::DelayStats;
use late_budget::LateBudget;
use target::TargetEst;

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

pub struct JitterBuffer {
    settings: JitterSettings,
    slots: Vec<Slot>,
    state: State,
    next_seq: u32,
    has_stream: bool,
    highest: u32,

    /// 디코드된 스테레오 인터리브 샘플 FIFO.
    fifo: VecDeque<f32>,
    /// FIFO 끝 샘플 다음 위치의 타임스탬프(확장).
    fifo_end_ts: i64,

    // 타임스탬프 언랩
    have_ts: bool,
    last_ts: u32,
    ts_ext_base: i64,

    /// 지연 통계(도착 쪽, `delay.rs`).
    delay: DelayStats,

    tuning: JitterTuning,
    /// 목표 깊이 추정(도착 쪽, `target.rs`).
    target: TargetEst,
    /// 늦은 패킷 예산(도착 쪽, `late_budget.rs`).
    late: LateBudget,
    /// 깊이 측정·보정(재생 쪽, `correction.rs`). `chase`·재생 시작 여유도 여기 있다.
    depth: DepthCtl,
    /// 디코드·은닉(재생 쪽, `conceal.rs`).
    conceal: Concealer,
    /// 마지막으로 받은 패킷의 프레임(샘플).
    pkt_frame: usize,

    stats: JitterStats,
}

/// 1ms당 샘플 수(48k). 하위 묶음도 쓴다(`JitterBuffer`를 거치지 않게, #142 리뷰 L1).
fn samples_per_ms() -> f64 {
    INTERNAL_RATE as f64 / 1000.0
}

fn seq_diff(a: u32, b: u32) -> i32 {
    a.wrapping_sub(b) as i32
}

/// 지터 버퍼의 힙 버퍼 묶음. `new`는 새로 할당하고, `restart`는 지금 것을 꺼내 처음 상태로 되돌려 다시 쓴다
/// (엔진 ③c: 재시작은 워커에서 적용되므로 할당·해제가 없어야 한다). 필드 초기값은 `from_buffers` 한 곳에만 있다.
struct Buffers {
    slots: Vec<Slot>,
    fifo: VecDeque<f32>,
    scratch: Vec<i64>,
    block_rel: Vec<i64>,
    depth: DepthBufs,
    conceal: ConcealBufs,
}

impl Buffers {
    fn alloc() -> Self {
        Self {
            slots: (0..SLOT_COUNT)
                .map(|_| Slot { seq: 0, valid: false, len: 0, flags: 0, frame: 0, ts: 0, data: vec![0u8; MAX_PAYLOAD_BYTES].into_boxed_slice() })
                .collect(),
            fifo: VecDeque::with_capacity(MAX_PKT_FRAME * 16 * 2),
            scratch: Vec::with_capacity(DELAY_WINDOW),
            block_rel: Vec::with_capacity(BLOCK_SAMPLES_MAX),
            depth: DepthBufs::alloc(),
            conceal: ConcealBufs::alloc(),
        }
    }

    /// `jb`의 버퍼를 꺼내 `alloc()`과 같은 내용으로 되돌린다(할당·해제 없음 — 빈 `Vec`/`RateRing`은 할당하지 않는다).
    /// 슬롯 `data`는 지우지 않는다: 읽는 곳은 모두 쓴 길이 `[..len]`까지이고(`insert`가 먼저 쓴다) `len`·`valid`를 0으로 돌린다.
    /// 하위 묶음의 버퍼는 각 묶음의 `take_*`/`reclaim`이 되돌린다(디코더는 풀로 — `Concealer::reclaim`).
    fn reclaim(jb: &mut JitterBuffer) -> Self {
        let mut slots = std::mem::take(&mut jb.slots);
        for s in &mut slots {
            (s.seq, s.valid, s.len, s.flags, s.frame, s.ts) = (0, false, 0, 0, 0, 0);
        }
        let mut fifo = std::mem::take(&mut jb.fifo);
        fifo.clear();
        Self {
            slots,
            fifo,
            scratch: jb.delay.take_scratch(),
            block_rel: jb.target.take_block_rel(),
            depth: jb.depth.reclaim(),
            conceal: jb.conceal.reclaim(),
        }
    }
}

impl JitterBuffer {
    pub fn new(settings: JitterSettings) -> Self {
        Self::from_buffers(settings, Buffers::alloc())
    }

    fn from_buffers(settings: JitterSettings, b: Buffers) -> Self {
        Self {
            settings,
            slots: b.slots,
            state: State::Buffering,
            next_seq: 0,
            has_stream: false,
            highest: 0,
            fifo: b.fifo,
            fifo_end_ts: 0,
            have_ts: false,
            last_ts: 0,
            ts_ext_base: 0,
            delay: DelayStats::new(b.scratch),
            tuning: JitterTuning::from_profile(settings.profile),
            target: TargetEst::new(b.block_rel),
            late: LateBudget::new(),
            depth: DepthCtl::new(b.depth),
            conceal: Concealer::new(b.conceal),
            pkt_frame: 240,
            stats: JitterStats::default(),
        }
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
        // 버퍼를 꺼내 처음 상태로 되돌려 다시 쓴다(엔진 ③c — 워커에서 적용되므로 할당·해제 0, `tests/rt_alloc.rs`).
        let buffers = Buffers::reclaim(self);
        *self = Self::from_buffers(self.settings, buffers);
        self.stats = stats;
    }

    fn reset_stream(&mut self) {
        self.depth.reset_stream();
        for s in &mut self.slots {
            s.valid = false;
        }
        self.state = State::Buffering;
        self.has_stream = false;
        self.fifo.clear();
        self.conceal.reset_stream();
        self.reset_tail(true);
    }

    /// 패킷 프레임 길이(샘플).
    fn frame(&self) -> usize {
        self.pkt_frame.max(1)
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
                        self.late.note_late_seq();
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
