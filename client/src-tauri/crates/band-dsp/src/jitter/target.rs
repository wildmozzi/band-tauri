//! 목표 깊이 추정(P1-5): 짧은 창·버스트·긴 창, 경로 변경, 목표 합성.
//!
//! 상태는 `TargetEst`(엔진 ⑧(a))에 있다. `TargetEst`의 메서드는 필요한 값(지연 통계, 계수, 프레임)을 인자로 받고
//! `JitterBuffer`를 모른다(1ms당 샘플 수도 모듈 함수 `samples_per_ms`로, ⑧(b)). 여러 묶음을 잇는 조립(`record_delay`·`target_samples`·`after_target_change` 등)은
//! 아래 `impl JitterBuffer`에 있다.

use super::delay::DelayStats;
use super::{samples_per_ms, JitterBuffer, JitterTuning, State, BLOCK_SAMPLES_MAX, BURST_WIN, LONG_BLOCKS_MAX, TARGET_GRID};

/// 버스트 감지 창 안에서 필요한 초과 패킷 수.
const BURST_COUNT: usize = 3;
/// 버스트 판정의 시간 군집 창(패킷): 이 안에서 BURST_COUNT개가 넘어야 한다.
const BURST_CLUSTER: usize = 4;
/// 긴 창 블록 길이(µs).
const BLOCK_US: u64 = 1_000_000;
/// 긴 창 추정: 최근 블록 분위수 중 이 순위(1 = 최댓값). 버스트 하나는 블록 2개까지 걸치므로 3.
const LONG_RANK: usize = 3;
/// 경로 변경(지연 급감)으로 통계를 다시 모은 뒤, 옛 경로로 늦게 오는 패킷을 통계에서 빼는 시간(µs).
const PATH_GRACE_US: u64 = 200_000;
/// 지연 계단 상승(P2-16b): 모든 패킷이 계단 시작 때의 최소 지연 + 문턱보다 이만큼(µs) 늦게 이어 오면 경로 지연이 오른
/// 것으로 보고 지연 통계를 새 지연에서 다시 모은다. 와이파이 스파이크(0.3초 +30ms, 장시간 하네스 #4·`tune_jitter_burst_up_and_return`)는
/// 버스트로 남도록 그보다 길게 둔다.
const STEP_UP_US: u64 = 500_000;
/// 계단으로 판정한 뒤 이 시간(µs) 안에 지연이 다시 내려오면(경로 변경 하강) 그 "계단"은 긴 스파이크였다(#194 리뷰 M2).
const STEP_RETURN_US: u64 = 3_000_000;
/// 긴 스파이크가 되돌아온 뒤(또는 그 억제 중 다시 하강이 오면) 이 시간(µs) 동안 계단 감지를 끈다. 반복되는 긴 스파이크
/// (2초 +60ms를 6초마다 등)를 매번 계단으로 처리하면 긴 창이 스파이크를 배우지 못해 은닉이 는다(0.90 → 1.38%) — 억제 중에는
/// 계단 감지 전처럼 버스트·긴 창이 맡는다.
const STEP_SUPPRESS_US: u64 = 60_000_000;

/// `track_step_up` 결과.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum StepWatch {
    Idle,
    /// 이 패킷으로 감시를 시작했다(호출한 쪽이 늦은 패킷 예산 스냅숏을 뜬다).
    Started,
    /// 계단으로 판정했다.
    Detected,
}
/// 프로필 변경 때 `tail_extra`까지 비우는 은닉 목표 변화 배율(P1-6c, PR #65 리뷰 L2). 이보다 작은 변화는 늦은 비율만 비운다.
const TAIL_RESET_RATIO: f64 = 1.5;

/// 목표 격자(0.25ms)로 올림.
pub(super) fn grid_up(x: f64) -> f64 {
    (x.max(0.0) / TARGET_GRID).ceil() * TARGET_GRID
}

/// 목표 깊이 추정 상태(샘플).
pub(super) struct TargetEst {
    /// 짧은 창 상한과 하향 대기 시작 시각.
    short_cap: f64,
    short_lower_since: Option<u64>,
    /// 버스트 상향: 절대 transit 기준 수준(최소 지연이 오르면 그만큼 줄어든다)과 마지막 버스트 패킷 시각.
    burst_abs: Option<i64>,
    burst_at_us: u64,
    recent_rel: [i64; BURST_WIN],
    recent_pos: usize,
    /// 긴 창: 현재 블록 표본(용량 `BLOCK_SAMPLES_MAX`, `Buffers`가 회수)과 시작 시각, 블록 분위수 링.
    block_rel: Vec<i64>,
    block_start_us: Option<u64>,
    block_q: [f64; LONG_BLOCKS_MAX],
    block_pos: usize,
    block_count: usize,
    long_cap: f64,
    /// 마지막으로 계산한 목표(샘플). 내려가면 `chase`(호출한 쪽).
    last_target: f64,
    /// 소비자가 은닉 판단 시각에 p95 너머로 요구하는 깊이(샘플, 48k, `set_consumer_lead`, P1-13).
    consumer_lead: f64,
    /// 경로 변경 유예 끝 시각(µs, `PATH_GRACE_US`).
    path_reset_until_us: u64,
    /// 경로 변경 유예 동안의 비교 문턱(샘플, 리셋 전 값).
    path_grace_step: i64,
    /// 지연 계단 상승 감시(P2-16b): 시작 시각(이때부터 모든 패킷이 `step_floor`보다 늦다), 그때의 최소 지연 + 문턱(절대 transit),
    /// 그때의 짧은 창 추정, 그 뒤 쌓은 긴 창 블록 수. 계단으로 판정되면 계단 표본에 오염된 추정을 되돌린다.
    step_since_us: Option<u64>,
    step_floor: i64,
    step_short_cap: f64,
    step_blocks: usize,
    /// 감시 중 받은 패킷 수(#194 리뷰 L1: 공백을 사이에 둔 패킷 몇 개로 판정하지 않게).
    step_pkts: usize,
    /// 마지막 계단 판정 시각과 계단 감지 억제 끝 시각(µs, M2).
    step_detected_us: Option<u64>,
    step_suppress_until_us: u64,
}

impl TargetEst {
    /// 처음 상태. `block_rel`은 비어 있고 용량이 `BLOCK_SAMPLES_MAX` 이상이어야 한다(`Buffers`).
    pub(super) fn new(block_rel: Vec<i64>) -> Self {
        Self {
            short_cap: -1.0,
            short_lower_since: None,
            burst_abs: None,
            burst_at_us: 0,
            recent_rel: [0; BURST_WIN],
            recent_pos: 0,
            block_rel,
            block_start_us: None,
            block_q: [0.0; LONG_BLOCKS_MAX],
            block_pos: 0,
            block_count: 0,
            long_cap: 0.0,
            last_target: 0.0,
            consumer_lead: 0.0,
            path_reset_until_us: 0,
            path_grace_step: 0,
            step_since_us: None,
            step_floor: 0,
            step_short_cap: -1.0,
            step_blocks: 0,
            step_pkts: 0,
            step_detected_us: None,
            step_suppress_until_us: 0,
        }
    }

    /// 재시작(`Buffers::reclaim`): 블록 표본을 비워 꺼낸다(할당·해제 없음).
    pub(super) fn take_block_rel(&mut self) -> Vec<i64> {
        let mut v = std::mem::take(&mut self.block_rel);
        v.clear();
        v
    }

    pub(super) fn consumer_lead(&self) -> f64 {
        self.consumer_lead
    }

    /// 지터 추정(샘플): 짧은 창·버스트·긴 창 중 큰 값.
    pub(super) fn jitter_est(&self, min_transit: i64, long_weight: f64) -> f64 {
        let burst = self.burst_abs.map(|a| (a - min_transit) as f64).unwrap_or(0.0);
        let short = self.short_cap.max(0.0);
        // 긴 창은 짧은 창 너머 초과분의 `long_weight`만 반영한다(PR #20 M4: 프로필에 따라 연속적으로).
        let long = short + (self.long_cap - short).max(0.0) * long_weight;
        short.max(burst).max(long)
    }

    /// 소비자 요구 깊이를 바꾼다. 값이 바뀌었으면 `true`(호출한 쪽이 `chase`를 건다).
    pub(super) fn set_consumer_lead(&mut self, samples: f64) -> bool {
        let before = self.consumer_lead;
        self.consumer_lead = samples.max(0.0);
        self.consumer_lead != before
    }

    /// 마지막 목표를 `t`로 기록한다(비교 없이).
    pub(super) fn set_last_target(&mut self, t: f64) {
        self.last_target = t;
    }

    /// 새 목표 `t`를 기록한다. 지난 목표보다 0.5샘플 넘게 내려갔으면 `true`(호출한 쪽이 `chase`를 건다).
    pub(super) fn note_target(&mut self, t: f64) -> bool {
        let dropped = t < self.last_target - 0.5;
        self.last_target = t;
        dropped
    }

    /// 프로필 변경: 새 분위수가 더 높을 때만 짧은 창 추정을 바로 올린다(PR #20 M3).
    pub(super) fn raise_short_cap(&mut self, q_rel: f64) {
        self.short_cap = self.short_cap.max(grid_up(q_rel));
    }

    /// 짧은 창 추정 갱신: 오르면 바로, 내리는 것은 더 낮은 후보(한 격자 히스테리시스)가 `hold_ms` 이어질 때.
    pub(super) fn update_short_cap(&mut self, q_rel: f64, now_us: u64, hold_ms: f64) {
        let up = grid_up(q_rel);
        if self.short_cap < 0.0 || up > self.short_cap {
            self.short_cap = up;
            self.short_lower_since = None;
            return;
        }
        let cand = grid_up(q_rel + TARGET_GRID);
        if cand < self.short_cap {
            let since = *self.short_lower_since.get_or_insert(now_us);
            if now_us.saturating_sub(since) as f64 >= hold_ms * 1000.0 {
                self.short_cap = cand;
                self.short_lower_since = None;
            }
        } else {
            self.short_lower_since = None;
        }
    }

    /// 버스트 감지(패킷마다): 최근 `BURST_WIN`패킷 중 `BURST_COUNT`개 이상이 현재 추정 + max(F/2, 1ms)를 넘으면
    /// 그 최댓값까지 바로 올린다. 버스트 수준 근처의 패킷이 오는 동안 유지하고, 끝나고 `burst_hold_ms`가 지나면 없앤다.
    /// 버스트 수준이 새로 생기거나 올라가면 `true`(`burst_events`).
    pub(super) fn detect_burst(&mut self, transit: i64, now_us: u64, delay: &DelayStats, frame: usize, tuning: &JitterTuning) -> bool {
        let min_transit = delay.min_transit();
        let rel = (transit - min_transit).max(0);
        self.recent_rel[self.recent_pos] = rel;
        self.recent_pos = (self.recent_pos + 1) % BURST_WIN;
        let spm = samples_per_ms();
        let spike = (frame as f64 / 2.0).max(spm) * tuning.burst_scale;
        // 판정 기준은 j가 아니라 짧은 창 p99와 j 중 큰 값: 꼬리가 긴 지터에서 분위수 너머 패킷은 늘 있다(M1).
        let base = self.jitter_est(min_transit, tuning.long_weight).max(delay.q99_rel());
        let n = delay.count().min(BURST_WIN);
        // 시간 군집: 최근 BURST_CLUSTER개 중 BURST_COUNT개가 넘어야 한다. 버스트는 연속 패킷을 늦추고, 꼬리 패킷은 따로 온다.
        let recent = |i: usize| self.recent_rel[(self.recent_pos + BURST_WIN - 1 - i) % BURST_WIN];
        // 창에 표본이 50개 미만이면(재생 시작·경로 변경 직후) p99 기준이 아직 없어 정상 지터도 버스트로 본다.
        let m = if delay.count() >= 50 { n.min(BURST_CLUSTER) } else { 0 };
        // 넘은 패킷 수와 그 최댓값(할당 없이, #142 리뷰 L2). rel ≥ 0이라 최댓값의 시작값 0은 결과를 바꾸지 않는다.
        let (over, peak) = (0..m).map(recent).filter(|&r| r as f64 > base + spike).fold((0usize, 0i64), |(n, mx), r| (n + 1, mx.max(r)));
        let mut event = false;
        if over >= BURST_COUNT {
            let peak = peak as f64;
            let level = min_transit + grid_up(peak) as i64;
            if self.burst_abs.is_none_or(|a| level > a) {
                event = true;
            }
            self.burst_abs = Some(self.burst_abs.map_or(level, |a| a.max(level)));
            self.burst_at_us = now_us;
        } else if let Some(a) = self.burst_abs {
            // 유지도 판정과 대칭: 최근 BURST_WIN개 중 BURST_COUNT개가 버스트 수준 근처여야 hold를 갱신한다.
            let near = (0..n).map(recent).filter(|&r| (min_transit + r) as f64 + spike >= a as f64).count();
            if near >= BURST_COUNT {
                self.burst_at_us = now_us;
            } else if now_us.saturating_sub(self.burst_at_us) as f64 >= tuning.burst_hold_ms * 1000.0 {
                self.burst_abs = None;
            }
        }
        event
    }

    /// 긴 창 블록 경계: 1초가 지났으면 블록 분위수(`quantile`)를 남기고 새 블록을 시작한 뒤 긴 창 추정을 갱신하고 `true`
    /// (호출한 쪽이 늦은 패킷 예산 블록을 닫는다). 표본은 그다음 `push_block_sample`로 넣는다.
    pub(super) fn roll_block(&mut self, now_us: u64, quantile: f64, long_blocks: usize) -> bool {
        let start = *self.block_start_us.get_or_insert(now_us);
        if now_us.saturating_sub(start) < BLOCK_US {
            return false;
        }
        let n = self.block_rel.len();
        if n >= 20 {
            let k = ((n as f64 * quantile).ceil() as usize).clamp(1, n) - 1;
            let (_, q, _) = self.block_rel.select_nth_unstable(k);
            self.block_q[self.block_pos] = *q as f64;
            self.block_pos = (self.block_pos + 1) % LONG_BLOCKS_MAX;
            self.block_count = (self.block_count + 1).min(LONG_BLOCKS_MAX);
            if self.step_since_us.is_some() {
                self.step_blocks += 1;
            }
        }
        self.block_rel.clear();
        self.block_start_us = Some(now_us);
        self.update_long_cap(long_blocks);
        true
    }

    /// 현재 블록에 상대 지연 표본 하나(블록당 `BLOCK_SAMPLES_MAX`개까지, 할당 없음).
    pub(super) fn push_block_sample(&mut self, rel: i64) {
        if self.block_rel.len() < BLOCK_SAMPLES_MAX {
            self.block_rel.push(rel);
        }
    }

    /// 최근 `long_blocks`개 블록 분위수 중 `LONG_RANK`번째 큰 값을 긴 창 추정으로 쓴다.
    pub(super) fn update_long_cap(&mut self, long_blocks: usize) {
        let n = self.block_count.min(long_blocks);
        let rank = LONG_RANK.max(long_blocks.div_ceil(6));
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
        self.long_cap = grid_up(v[rank - 1]);
    }

    /// 경로 변경 유예를 시작한다: `PATH_GRACE_US` 동안 최소 지연 + `step`(리셋 전 문턱)보다 늦은 패킷은 옛 경로로 본다.
    pub(super) fn start_path_grace(&mut self, now_us: u64, step: i64) {
        self.path_reset_until_us = now_us + PATH_GRACE_US;
        self.path_grace_step = step;
    }

    /// 경로 변경 유예 중 옛 경로로 오던(이미 보낸) 패킷인가.
    pub(super) fn is_old_path(&self, now_us: u64, transit: i64, min_transit: i64) -> bool {
        now_us < self.path_reset_until_us && transit > min_transit + self.path_grace_step
    }

    /// 지연 계단 상승 감시(P2-16b, 패킷마다 `record_delay`에서). `armed`(창이 찼다)일 때 최소 지연 + `step`보다 늦은 패킷이
    /// 오면 감시를 시작하고, 그 뒤 `step_floor` 이하 패킷이 하나라도 오면 그만둔다(버스트·꼬리 지터는 사이사이 바닥 근처 패킷이
    /// 있다). `STEP_UP_US` 동안 끊기지 않으면 `true` — 호출한 쪽이 지연 통계를 다시 모으고 `undo_step`으로 오염을 되돌린다.
    /// 문턱·바닥은 시작 때 값으로 고정한다(계단 표본이 들어오면 분위수가 부풀고, 창이 옛 표본을 다 밀어내면 최솟값이 오른다).
    /// 판정에는 그 동안 받은 패킷이 `STEP_UP_US`어치 패킷 수(`frame` 기준)의 절반 이상이어야 한다(L1). 긴 스파이크가 되돌아온
    /// 뒤의 억제 시간(`note_path_drop`)에는 감시를 시작하지 않는다(M2).
    pub(super) fn track_step_up(&mut self, transit: i64, now_us: u64, min_transit: i64, step: i64, armed: bool, frame: usize) -> StepWatch {
        match self.step_since_us {
            Some(since) if transit > self.step_floor => {
                self.step_pkts += 1;
                let need = (STEP_UP_US as f64 * samples_per_ms() / 1000.0 / frame.max(1) as f64 / 2.0) as usize;
                if now_us.saturating_sub(since) >= STEP_UP_US && self.step_pkts >= need {
                    self.step_since_us = None;
                    self.step_detected_us = Some(now_us);
                    return StepWatch::Detected;
                }
            }
            Some(_) => self.step_since_us = None,
            None if armed && now_us >= self.step_suppress_until_us && transit > min_transit + step => {
                self.step_since_us = Some(now_us);
                self.step_floor = min_transit + step;
                self.step_short_cap = self.short_cap;
                self.step_blocks = 0;
                self.step_pkts = 1;
                return StepWatch::Started;
            }
            None => {}
        }
        StepWatch::Idle
    }

    /// 경로 변경(지연 하강)이 왔다(M2): 계단 판정 뒤 `STEP_RETURN_US` 안이면 그 계단은 긴 스파이크였으므로, 또는 이미 억제
    /// 중이면 반복되는 스파이크가 이어지므로 `STEP_SUPPRESS_US` 동안 계단 감지를 끈다.
    pub(super) fn note_path_drop(&mut self, now_us: u64) {
        let spike = self.step_detected_us.is_some_and(|d| now_us.saturating_sub(d) <= STEP_RETURN_US);
        if spike || now_us < self.step_suppress_until_us {
            self.step_suppress_until_us = now_us + STEP_SUPPRESS_US;
        }
        self.step_detected_us = None;
    }

    /// 지연 계단으로 판정된 뒤: 짧은 창 추정을 계단 전 값으로, 계단 뒤 쌓은 긴 창 블록(옛 최소 지연 기준이라 계단 크기만큼
    /// 부풀었다)을 버린다. 버스트 수준·현재 블록은 `reset_path`가 비운다.
    pub(super) fn undo_step(&mut self, long_blocks: usize) {
        self.short_cap = self.step_short_cap;
        self.short_lower_since = None;
        for _ in 0..self.step_blocks.min(self.block_count) {
            self.block_pos = (self.block_pos + LONG_BLOCKS_MAX - 1) % LONG_BLOCKS_MAX;
            self.block_count -= 1;
        }
        self.step_blocks = 0;
        self.update_long_cap(long_blocks);
    }

    /// 경로 변경: 절대 지연에 묶인 것(버스트 절대 수준, 현재 블록)만 버리고 짧은 창 추정은 정상 hold로 내려가게 한다.
    pub(super) fn reset_path(&mut self, now_us: u64) {
        self.step_since_us = None;
        self.short_lower_since = Some(now_us);
        self.burst_abs = None;
        self.recent_rel = [0; BURST_WIN];
        self.block_rel.clear();
        self.block_start_us = None;
    }

    #[cfg(test)]
    pub(super) fn block_len(&self) -> usize {
        self.block_rel.len()
    }
}

impl JitterBuffer {
    /// 목표 상·하한(샘플). 하한은 `jitter_min_ms`(0이면 없음), 상한은 `jitter_max_ms`(하한 이상).
    fn target_limits(&self) -> (f64, f64) {
        let spm = samples_per_ms();
        let lo = (self.settings.min_ms.max(0.0) as f64 * spm).max(0.0);
        let hi = (self.settings.max_ms as f64 * spm).max(lo);
        (lo, hi)
    }

    /// 지터 추정(샘플): 짧은 창·버스트·긴 창 중 큰 값.
    pub(super) fn jitter_est(&self) -> f64 {
        self.target.jitter_est(self.delay.min_transit(), self.tuning.long_weight)
    }

    /// 목표 깊이(샘플) = 지터 추정 + 소비자 요구 깊이(`lead`) + 삽입 진입 문턱 + 프로필 여유 + 늦은 패킷 예산 여유
    /// (`tail_extra`, ≤ 1프레임). 상한 `max_ms`.
    pub fn target_samples(&self) -> f64 {
        let (lo, hi) = self.target_limits();
        let (_, enter_ins, _, _) = self.deadbands();
        let pad = self.tuning.pad_frames * self.frame() as f64 + self.depth.start_pad();
        (self.jitter_est() + self.target.consumer_lead() + enter_ins + pad + self.late.tail_extra().min(self.frame() as f64)).clamp(lo, hi)
    }

    /// 소비자(워커)가 은닉 판단 시각에 p95 지연 너머로 요구하는 깊이(샘플, 48k): 은닉 마감 여유 + 피어 적응
    /// 여유(M4)만큼 앞당긴 마감과 더 채우는 양(P1-13). 목표가 하나이므로 깊이 제어(보정)·재생 시작·은닉 판단이
    /// 같은 목표 지연을 본다. 값이 바뀌어 목표가 바뀌면 깊이는 보정 램프로 따라간다. 이때는 데드밴드 안의
    /// 오차도 해제 문턱까지 보정한다(`chase`): 여유가 데드밴드보다 작게 줄면 깊이가 내려오지 않아 지연이 남았다.
    pub fn set_consumer_lead(&mut self, samples: f64) {
        if self.target.set_consumer_lead(samples) {
            self.depth.set_chase();
        }
        let t = self.target_samples();
        self.target.set_last_target(t);
    }

    /// '저지연 ↔ 안정' 프로필 변경(0..1). 다음 통계 갱신부터 새 계수로 목표를 계산한다.
    /// 짧은 창 추정은 새 분위수가 더 높을 때만 바로 올리고, 낮으면 정상 hold를 거쳐 내려간다(PR #20 M3: 슬라이더를
    /// 움직일 때마다 hold 중이던 추정이 바로 떨어져 은닉이 날 수 있었다). 같은 값이면 아무것도 하지 않는다.
    pub(crate) fn set_profile(&mut self, profile: f32) {
        let tuning = JitterTuning::from_profile(profile);
        if tuning == self.tuning {
            self.settings.profile = profile;
            return;
        }
        self.settings.profile = profile;
        let ratio = tuning.conceal_target / self.tuning.conceal_target;
        self.tuning = tuning;
        self.delay.recompute(self.tuning.quantile);
        self.target.raise_short_cap(self.delay.q_rel());
        self.target.update_long_cap(self.tuning.long_blocks);
        // 예산(은닉 목표)이 바뀌었으므로 옛 예산으로 잘라 쌓은 늦은 비율(옛 예산 × 4)은 버린다(PR #63 리뷰 L2). 망은 같으므로
        // 예산이 `TAIL_RESET_RATIO`배 넘게 바뀔 때만 `tail_extra`도 비운다 — 슬라이더 한두 칸(0.5 → 0.55, 목표 1.3 → 1.23%)
        // 으로 꼬리 대응을 처음부터 다시 쌓지 않게(PR #65 리뷰 L2). 작은 변화는 새 예산으로 다음 블록들에서 조정된다.
        if ratio.is_finite() && (1.0 / TAIL_RESET_RATIO..=TAIL_RESET_RATIO).contains(&ratio) {
            self.late.clear_late_rates();
        } else {
            self.reset_tail(false);
        }
        self.after_target_change();
    }

    /// 목표가 내려갔으면 해제 문턱까지 따라간다(버림 데드밴드 안에 지연이 남지 않게).
    pub(super) fn after_target_change(&mut self) {
        let t = self.target_samples();
        if self.target.note_target(t) {
            self.depth.set_chase();
        }
    }

    pub(super) fn record_delay(&mut self, ts: u32, now_us: u64) {
        let ts_ext = self.unwrap_ts(ts);
        let transit = Self::now_samples(now_us) - ts_ext;
        // 경로 변경(릴레이 → 직접 승격 등)으로 지연이 한 번에 크게 줄면 이전 경로의 도착 지연이 모두 "늦은" 표본이
        // 되어 창이 빠질 때까지(+ hold) 목표가 옛 경로 지연만큼 남는다. 최소 지연보다 max(5ms, F) 넘게 이르면
        // 지연 통계를 새 경로부터 다시 모은다(깊이 초과분은 패킷 단위 버림이 걷어 낸다). 지터가 큰 회선에서 잘못
        // 걸리지 않도록 문턱은 현재 지터 폭(분위수)보다도 크고, 창에 표본이 50개 이상일 때만 본다.
        let step = (5.0 * samples_per_ms()).max(self.frame() as f64).max(self.delay.q_rel()) as i64;
        let mut stepped_up = false;
        if self.delay.count() >= 50 && transit < self.delay.min_transit() - step {
            self.target.note_path_drop(now_us);
            self.reset_delay_stats(now_us);
            // 유예 비교 문턱은 리셋 전 값으로 고정한다(PR #20 M2: 리셋 뒤 q가 0으로 다시 계산되면 문턱이 max(5ms, F)로
            // 줄어 새 경로의 정상 지터 패킷까지 옛 경로로 보고 빼 버렸다).
            self.target.start_path_grace(now_us, step);
        } else if self.target.is_old_path(now_us, transit, self.delay.min_transit()) {
            // 경로 변경 직후 옛 경로로 오던(이미 보낸) 패킷: 새 경로 통계에 넣지 않는다.
            return;
        } else if let w @ (StepWatch::Started | StepWatch::Detected) =
            self.target.track_step_up(transit, now_us, self.delay.min_transit(), step, self.delay.count() >= 50, self.frame())
        {
            if w == StepWatch::Started {
                self.late_at_step = self.late;
            } else {
                // 지연 계단 상승(P2-16b): 0.5초 동안 모든 패킷이 옛 최소 지연 + 문턱보다 늦었다 — 버스트가 아니라 경로 지연이 올랐다.
                // 계단 표본은 옛 최소 지연 기준으로 계단 크기만큼의 "지터"로 보여 버스트 수준·짧은 창·긴 창 블록을 부풀리고(목표가
                // 상한 80ms에 수 초 머묾), 최소 지연이 창에 남는 동안 깊이를 계단 크기만큼 크게 쟀다. 경로 변경(지연 급감)과 같이
                // 지연 통계를 새 지연에서 다시 모으고, 짧은 창·긴 창은 계단 전 값으로 되돌린다(상대 지터는 그대로라고 본다).
                // 경로 변경과 달리 늦은 패킷 예산(`tail_extra`·손실 링)은 남긴다: 같은 경로의 지연만 올랐으므로 꼬리 모양은 그대로다
                // (비우면 꼬리 긴 회선에서 예산을 다시 쌓는 동안 늦은 패킷 은닉이 는다 — 장시간 하네스 #1). 계단 구간 블록은 판단에서 뺀다.
                // 감시 동안 닫힌 늦은 패킷 블록(계단 공백의 늦은 도착·버림이 든)도 판단에서 되돌린다(#194 리뷰 L2).
                self.delay.reset();
                self.target.reset_path(now_us);
                self.target.undo_step(self.tuning.long_blocks);
                self.late = self.late_at_step;
                self.late.skip_start();
                stepped_up = true;
            }
        }
        if self.delay.push(transit, self.tuning.quantile) {
            self.target.update_short_cap(self.delay.q_rel(), now_us, self.tuning.hold_ms);
        }
        if stepped_up && self.state == State::Playing {
            // 깊이 EWMA를 새 최소 지연(방금 이 표본으로 다시 계산) 기준 깊이로 다시 잡는다(#194 리뷰 M1). EWMA는 옛 최소 지연
            // 기준이라 계단 크기만큼 깊게 남아 있고, 늦은 패킷 버림(H1: EWMA + 되감김 − 목표 ≥ F/2)이 목표가 내려간 뒤에도
            // EWMA가 식을 때까지(+130ms 계단에서 약 0.4초) "버림 → 은닉 → 버림"을 2프레임마다 이어 갔다.
            let d = self.depth_now(now_us);
            self.depth.set_ewma(d);
        }
        if self.target.detect_burst(transit, now_us, &self.delay, self.frame(), &self.tuning) {
            self.stats.burst_events += 1;
        }
        // 긴 창 블록: 경계에서 늦은 패킷 예산 블록도 닫고(블록 분위수·긴 창 추정 갱신 뒤), 그다음 이 표본을 새 블록에 넣는다.
        if self.target.roll_block(now_us, self.tuning.quantile, self.tuning.long_blocks) {
            self.close_late_block();
        }
        self.target.push_block_sample((transit - self.delay.min_transit()).max(0));
        self.after_target_change();
    }

    /// 경로 변경: 절대 지연에 묶인 것(transit 창, 버스트 절대 수준, 현재 블록)만 버린다. 짧은 창 추정과 긴 창 블록
    /// 분위수는 상대 지터라 경로가 바뀌어도 의미가 있으므로 남기고, 짧은 창 추정은 정상 hold로 내려가게 한다(PR #20 M2:
    /// 전부 0으로 두면 목표가 lead + 문턱까지 떨어져 새 경로 지터가 크면 표본이 쌓일 때까지 과소추정했다).
    pub(super) fn reset_delay_stats(&mut self, now_us: u64) {
        self.delay.reset();
        self.target.reset_path(now_us);
        // 늦은 패킷 예산도 새 경로 기준으로 다시 모은다(L2). 목표 변화는 `record_delay` 끝의 `after_target_change`가 반영한다.
        // 재생 시작처럼 창·블록이 비어 수렴 중이므로 처음 블록들은 판단에서 뺀다(PR #65 리뷰 L1).
        self.reset_tail(true);
        self.late.skip_start();
    }
}
