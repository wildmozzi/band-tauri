//! 늦은 패킷 예산(P1-6b/c): 마감을 놓친 뒤 도착한 패킷 비율로 목표에 `tail_extra`를 더한다.
//!
//! 상태는 `LateBudget`(엔진 ⑧(a))에 있다. 메서드는 필요한 값(재생 중 여부, 은닉 목표, RFC 지터, 프레임)을 인자로 받고
//! `JitterBuffer`를 모른다. 통계 카운터(`tail_resets`·`tail_adjusts`)와 목표 재계산(`after_target_change`)은 아래
//! `impl JitterBuffer`가 한다.

use super::target::grid_up;
use super::{JitterBuffer, State, LATE_BLOCKS, TARGET_GRID};

/// 늦은 패킷 예산(P1-6b, `close_late_block`): 올림·내림 판단에 쓰는 (조정 뒤) 블록 수, 내림 문턱
/// (예산 배수), 블록 비율 자름 배수(스파이크가 예산을 움직이지 않게).
const LATE_UP_BLOCKS: usize = 5;
const LATE_DOWN_BLOCKS: usize = 10;
const LATE_DOWN_FRAC: f64 = 0.6;
const LATE_CLIP: f64 = 4.0;
/// 한 번에 바꾸는 최대 깊이(샘플, 1ms).
const LATE_STEP_MAX: f64 = 48.0;
/// 재생 시작 뒤 예산 판단에서 빼는 블록 수: 시작이 걸친 블록 + START_PAD(1초)·짧은 창 수렴 구간(PR #63 리뷰 L3).
pub(super) const LATE_SKIP_BLOCKS: u32 = 2;
/// 손실만으로 은닉 목표를 넘는 회선에서 푸는 예산의 상한(비율, P1-6c PR #65 리뷰 M1·M2). 하네스의 GOAL 밖 회선 기준
/// "은닉 ≤ 실제 손실 + 0.5%p"(GOAL 2절 손실 1% → 1.5%의 손실 너머 몫)에서 언더런·잡음 몫 0.1%p를 남긴 값이다.
pub(super) const LATE_BUDGET_LOSSY_MAX: f64 = 0.004;

/// 늦은 패킷 예산(P1-6b): 현재 1초 블록의 늦은 도착 수·도착 패킷 수, 블록별 늦은 비율(예산의 `LATE_CLIP`배로 자름)·
/// 실제 손실률 링. 늦은 도착·손실·도착 패킷 모두 재생 중에만 센다(P1-6c, PR #63 리뷰 L4: 이전에는 도착 패킷만
/// 버퍼링 중에도 세서 재버퍼가 낀 블록의 비율이 희석됐고, 경로 변경 유예 중 옛 경로 패킷은 늦은 도착만 셌다).
/// 남은 비대칭: 언더런 은닉이 2번 이상 쌓인 뒤(debt ≥ 2) 도착한 패킷 하나는 늦은 도착 1개로 센다 — 늦게 온
/// 패킷 수를 세는 것이므로 의도된 것이고, 은닉 수와의 차이는 창 평균에서 작다.
/// 고정 크기라 복사로 스냅숏을 뜬다(지연 계단 감시 시작 때, #194 리뷰 L2 — 할당 없음).
#[derive(Clone, Copy)]
pub(super) struct LateBudget {
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
}

impl LateBudget {
    pub(super) fn new() -> Self {
        Self {
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
        }
    }

    /// 목표에 더하는 깊이(샘플, 0..=F).
    pub(super) fn tail_extra(&self) -> f64 {
        self.tail_extra
    }

    /// `tail_extra`와 늦은 비율 링, 현재 블록 카운터를 비운다. `with_loss`면 실제 손실률 링도(`JitterBuffer::reset_tail`).
    pub(super) fn reset(&mut self, with_loss: bool) {
        self.tail_extra = 0.0;
        self.clear_late_rates();
        self.late_block = 0;
        self.pkts_block = 0;
        self.lost_block = 0;
        self.late_seq_block = 0;
        if with_loss {
            self.loss_rates = [0.0; LATE_BLOCKS];
            self.loss_count = 0;
        }
    }

    /// 늦은 비율 링만 비우고 조정 창을 새로 시작한다(`tail_extra`는 유지).
    pub(super) fn clear_late_rates(&mut self) {
        self.tail_fresh = 0;
        self.late_rates = [0.0; LATE_BLOCKS];
    }

    /// 재생 시작·경로 변경: 처음 `LATE_SKIP_BLOCKS`개 블록을 판단에서 뺀다(L3, PR #65 리뷰 L1).
    pub(super) fn skip_start(&mut self) {
        self.tail_skip = LATE_SKIP_BLOCKS;
    }

    /// 마감을 놓친 뒤 도착한 패킷 1개(재생 중일 때만 센다).
    pub(super) fn note_late(&mut self, playing: bool) {
        if playing {
            self.late_block = self.late_block.saturating_add(1);
        }
    }

    /// 분모: 도착한 패킷 1개(재생 중일 때만 센다).
    pub(super) fn note_pkt(&mut self, playing: bool) {
        if playing {
            self.pkts_block = self.pkts_block.saturating_add(1);
        }
    }

    /// 재생 중 손실로 넘긴 seq 1개.
    pub(super) fn note_lost(&mut self) {
        self.lost_block = self.lost_block.saturating_add(1);
    }

    /// 재생 중 손실로 넘긴 seq가 나중에 도착했다(실제 손실에서 뺀다).
    pub(super) fn note_late_seq(&mut self) {
        self.late_seq_block = self.late_seq_block.saturating_add(1);
    }

    /// 1초 블록을 닫고 `tail_extra`를 조정한다(규칙은 `JitterBuffer::close_late_block`). `conceal_target`은 은닉 목표,
    /// `rfc_jitter`는 꼬리 척도(샘플), `frame`은 상한(샘플). 조정했으면 `true`.
    pub(super) fn close_block(&mut self, conceal_target: f64, rfc_jitter: f64, frame: f64) -> bool {
        let (late, pkts, lost, late_seq) = (self.late_block, self.pkts_block, self.lost_block, self.late_seq_block);
        self.late_block = 0;
        self.pkts_block = 0;
        self.lost_block = 0;
        self.late_seq_block = 0;
        if pkts < 20 {
            return false;
        }
        // 재생 시작 직후(시작이 걸친 블록, START_PAD·짧은 창 수렴 구간)의 늦은 도착은 초기 과조정을 만들고 내림이 느려
        // 오래 남으므로 판단에서 뺀다(L3). 재버퍼 뒤 재시작에도 같다.
        if self.tail_skip > 0 {
            self.tail_skip -= 1;
            return false;
        }
        // 실제 손실 = 손실로 넘긴 seq − 그중 나중에 도착한 것. 늦은 도착이 다음 블록에 걸쳐도 창 평균에서 상쇄된다.
        self.loss_rates[self.late_pos] = lost as f64 / pkts as f64 - late_seq as f64 / pkts as f64;
        self.loss_count = (self.loss_count + 1).min(LATE_BLOCKS);
        let real_loss = (self.loss_rates[..self.loss_count].iter().sum::<f64>() / self.loss_count as f64).max(0.0);
        let budget = Self::budget(conceal_target, real_loss);
        self.late_rates[self.late_pos] = (late as f64 / pkts as f64).min(LATE_CLIP * budget);
        self.late_pos = (self.late_pos + 1) % LATE_BLOCKS;
        self.tail_fresh = (self.tail_fresh + 1).min(LATE_BLOCKS);
        let avg = |n: usize| (0..n).map(|i| self.late_rates[(self.late_pos + LATE_BLOCKS - 1 - i) % LATE_BLOCKS]).sum::<f64>() / n as f64;
        let before = self.tail_extra;
        // 꼬리 척도 μ: RFC 3550 지터(연속 지연 차 절댓값의 평균)는 지수 분포에서 평균 μ와 같다.
        let mu = rfc_jitter.max(TARGET_GRID);
        if self.tail_fresh >= LATE_UP_BLOCKS && avg(LATE_UP_BLOCKS) > budget {
            let d = grid_up(mu * (avg(LATE_UP_BLOCKS) / budget).ln()).clamp(TARGET_GRID, LATE_STEP_MAX);
            self.tail_extra = (self.tail_extra + d).min(frame);
        } else if self.tail_extra > 0.0 && self.tail_fresh >= LATE_UP_BLOCKS && avg(LATE_UP_BLOCKS) == 0.0 {
            // 빠른 내림(P2-16c): 올림과 같은 블록 수(5) 동안 늦은 도착이 하나도 없으면 예산을 전혀 쓰지 않은 것이다(지터가
            // 좋아짐 — 파레토 3ms → 균등 0.5ms 뒤 `tail_extra` 2.25ms가 10블록마다 0.25ms씩 내려 1분 넘게 남았다). 절반씩
            // 내린다(격자로 올림, 1ms 상한). 늦은 도착이 조금이라도 있는 회선은 아래 느린 내림 그대로다.
            let d = grid_up(self.tail_extra / 2.0).clamp(TARGET_GRID, LATE_STEP_MAX);
            self.tail_extra = (self.tail_extra - d).max(0.0);
        } else if self.tail_fresh >= LATE_DOWN_BLOCKS && avg(LATE_DOWN_BLOCKS) < budget * LATE_DOWN_FRAC {
            // 내림은 절반 걸음: 예산 아래로 얼마나 내려왔는지(최소 예산/4로 잘라) 기준.
            let r = (budget * LATE_DOWN_FRAC / avg(LATE_DOWN_BLOCKS).max(budget / 4.0)).ln();
            let d = grid_up(mu * r / 2.0).clamp(TARGET_GRID, LATE_STEP_MAX);
            self.tail_extra = (self.tail_extra - d).max(0.0);
        }
        if self.tail_extra != before {
            self.tail_fresh = 0;
            return true;
        }
        false
    }

    /// 늦은 패킷 예산(비율): 은닉 목표 `target`, 실제 손실률 `real_loss`의 연속 함수(P1-6c, PR #65 리뷰 M1).
    /// - 손실 < 목표(균형 1.3%): `target − real_loss`를 목표의 1/4 ~ 3/4로 제한(P1-6b와 같음, 손실 1%면 0.325%).
    /// - 목표 ~ 목표 × 1.5(균형 1.3 ~ 1.95%): 하한(목표/4)에서 `LATE_BUDGET_LOSSY_MAX`(0.4%, 목표 1/4 ~ 3/4 안으로)까지
    ///   선형으로 푼다. 손실만으로 목표에 닿는 회선에서 늦은 패킷을 가장 세게 줄이면 지연(≤ 1F)만 크고 이득이 작다(PR #63
    ///   리뷰 L1). #65 첫 커밋은 손실 1.3%에서 0.325% → 0.975% 계단이라 손실 1.2~1.5% 회선에서 블록마다 뒤집혔고(M1),
    ///   0.975%는 하네스의 GOAL 밖 회선 기준(은닉 ≤ 실제 손실 + 0.5%p)을 넘었다(M2).
    /// - 그 위: 0.4%(균형). 저지연(목표 3%)은 하한 0.75%가 이미 0.4%보다 커서 풀지 않는다(하한 그대로).
    pub(super) fn budget(target: f64, real_loss: f64) -> f64 {
        let (lo, hi) = (target / 4.0, target * 0.75);
        let base = (target - real_loss).clamp(lo, hi);
        let relaxed = LATE_BUDGET_LOSSY_MAX.clamp(lo, hi);
        // 손실이 목표를 넘는 만큼(목표/2 폭) 푼다. 목표 아래면 w = 0이라 base 그대로(목표 − 목표/4 ~ 목표 구간은 이미 하한).
        let w = ((real_loss - target) / (target / 2.0)).clamp(0.0, 1.0);
        base + (relaxed - lo) * w
    }
}

/// 시험용 들여다보기(`jitter::tests`).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct LateProbe {
    pub(crate) tail_extra: f64,
    pub(crate) tail_fresh: usize,
    pub(crate) loss_count: usize,
    pub(crate) pkts_block: u32,
    pub(crate) late_block: u32,
    pub(crate) tail_skip: u32,
    pub(crate) late_rates: [f64; LATE_BLOCKS],
}

/// 시험용 들여다보기·조작(`jitter::tests`).
#[cfg(test)]
impl LateBudget {
    pub(super) fn probe(&self) -> LateProbe {
        LateProbe {
            tail_extra: self.tail_extra,
            tail_fresh: self.tail_fresh,
            loss_count: self.loss_count,
            pkts_block: self.pkts_block,
            late_block: self.late_block,
            tail_skip: self.tail_skip,
            late_rates: self.late_rates,
        }
    }

    /// 모든 링·카운터를 0이 아닌 값으로 채운다(초기화 시험).
    pub(super) fn dirty(&mut self) {
        self.tail_extra = 96.0;
        self.tail_fresh = 3;
        self.late_rates = [0.01; LATE_BLOCKS];
        self.loss_rates = [0.02; LATE_BLOCKS];
        self.loss_count = LATE_BLOCKS;
        self.late_block = 5;
        self.pkts_block = 100;
    }
}

impl JitterBuffer {
    /// 늦은 패킷 예산 초기화(P1-6c, PR #63 리뷰 L2): `tail_extra`와 늦은 비율 링을 비운다. 내림은 10블록마다 절반 걸음이라
    /// 그대로 두면 새 조건(경로·스트림·프로필)이 꼬리 없는 회선이어도 1프레임이 빠지는 데 수십~200초가 걸렸다. 다시 필요하면
    /// 5블록 뒤 μ·ln 걸음(≤ 1ms)으로 올라온다. `with_loss`면 실제 손실률 링도 비운다(경로·스트림이 바뀐 경우; 프로필
    /// 변경은 망 손실과 무관하므로 남긴다). 목표가 바뀌므로 호출한 쪽이 `after_target_change`를 거친다.
    pub(super) fn reset_tail(&mut self, with_loss: bool) {
        self.stats.tail_resets += 1;
        self.late.reset(with_loss);
    }

    /// 마감을 놓친 뒤 도착한 패킷 1개(P1-6b): 손실로 넘긴 seq가 도착(`late`)했거나, 언더런 은닉 뒤 도착해 재생·버림됐다.
    pub(super) fn note_late(&mut self) {
        self.late.note_late(self.state == State::Playing);
    }

    /// 늦은 패킷 예산의 분모: 재생 중 도착한 패킷 1개(늦은 도착 포함, 경로 변경 유예로 통계에서 뺀 것 포함). 늦은 도착·
    /// 손실과 같은 조건(재생 중)으로 센다(L4). `record_delay`(블록 경계) 뒤에 불러 이 패킷이 새 블록에 들어가게 한다.
    pub(super) fn note_pkt(&mut self) {
        self.late.note_pkt(self.state == State::Playing);
    }

    /// 늦은 패킷 예산(P1-6b): 1초 블록이 끝날 때 마지막 조정 이후 블록들의 늦은 도착 비율을 예산과 비교해 `tail_extra`를
    /// 조정한다. 조정 직후의 블록만 보므로(창을 새로 시작) 창 지연으로 계속 올라가는 과조정이 없다.
    /// - 블록 비율은 예산의 `LATE_CLIP`배로 자른다. 스파이크(블록 1~2개에 늦은 패킷이 몰림)는 긴 창·버스트 감지의
    ///   몫이고 이 예산을 크게 움직이지 않는다. 꼬리 긴 지터처럼 늦은 패킷이 고르게 퍼진 회선에서만 오른다.
    /// - 예산 = 은닉 목표(`conceal_target`, 균형 1.3%) − 실제 손실률(손실로 넘긴 seq − 그중 늦게 도착한 것), 목표의
    ///   1/4 ~ 3/4로 제한. 손실 은닉은 지터 버퍼가 줄일 수 없으므로 남은 몫만 늦은 패킷에 쓴다(GOAL 2절). 손실만으로
    ///   목표에 닿는 회선은 0.4%까지 연속으로 푼다(`LateBudget::budget`, P1-6c).
    /// - 재생 시작·경로 변경 뒤 `LATE_SKIP_BLOCKS`개 블록은 판단에서 뺀다(L3). 경로 변경·스트림 리셋·큰 프로필 변경은 초기화(L2).
    /// - 올림: 조정 뒤 `LATE_UP_BLOCKS`개 평균이 예산을 넘으면 μ·ln(평균/예산)(격자 0.25ms ~ 1ms). μ는 꼬리 척도로
    ///   RFC 3550 지터를 쓴다(지수 꼬리면 늦은 비율 ∝ exp(−여유/μ)이라 한두 번에 수렴).
    /// - 내림: 조정 뒤 `LATE_DOWN_BLOCKS`개 평균이 예산 × `LATE_DOWN_FRAC` 아래면 그 절반 걸음.
    /// - 상한은 1프레임(지터 버퍼 추가 지연 증가 ≤ 1프레임).
    pub(super) fn close_late_block(&mut self) {
        if self.late.close_block(self.tuning.conceal_target, self.delay.rfc_jitter(), self.frame() as f64) {
            self.stats.tail_adjusts += 1;
            self.after_target_change();
        }
    }
}
