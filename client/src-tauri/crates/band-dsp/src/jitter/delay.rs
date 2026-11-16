//! 지연 통계(`DelayStats`, 엔진 ⑧(a)): 최근 `DELAY_WINDOW`개 transit 창, 최소 transit·상대 지연 분위수, RFC 3550 지터.
//!
//! 도착 쪽(`insert` → `record_delay`)만 갱신한다. 다른 묶음은 읽기 메서드(`min_transit`·`q_rel`·`q99_rel`·`count`·
//! `rfc_jitter`)로만 본다. 메서드는 필요한 값(분위수)을 인자로 받고 `JitterBuffer`를 모른다.

use super::DELAY_WINDOW;

/// p95/최솟값 재계산 주기(패킷 수).
const STATS_EVERY: usize = 10;

pub(super) struct DelayStats {
    transits: [i64; DELAY_WINDOW],
    transit_count: usize,
    transit_pos: usize,
    since_stats: usize,
    /// 분위수 계산용 스크래치(용량 `DELAY_WINDOW`, `Buffers`가 회수해 다시 쓴다).
    scratch: Vec<i64>,
    min_transit: i64,
    /// 상대 지연의 프로필 분위수(샘플, 짧은 창).
    q_rel: f64,
    /// 짧은 창 p99(샘플). 버스트 판정 기준(꼬리 패킷과 버스트를 가른다, PR #20 M1).
    q99_rel: f64,
    rfc_jitter: f64,
    last_transit: Option<i64>,
}

impl DelayStats {
    /// 처음 상태. `scratch`는 비어 있고 용량이 `DELAY_WINDOW` 이상이어야 한다(`Buffers`).
    pub(super) fn new(scratch: Vec<i64>) -> Self {
        Self {
            transits: [0; DELAY_WINDOW],
            transit_count: 0,
            transit_pos: 0,
            since_stats: 0,
            scratch,
            min_transit: 0,
            q_rel: 0.0,
            q99_rel: 0.0,
            rfc_jitter: 0.0,
            last_transit: None,
        }
    }

    /// 재시작(`Buffers::reclaim`): 스크래치를 비워 꺼낸다(할당·해제 없음).
    pub(super) fn take_scratch(&mut self) -> Vec<i64> {
        let mut v = std::mem::take(&mut self.scratch);
        v.clear();
        v
    }

    /// 깊이·지터 추정의 기준인 최소 transit(샘플).
    pub(super) fn min_transit(&self) -> i64 {
        self.min_transit
    }

    pub(super) fn q_rel(&self) -> f64 {
        self.q_rel
    }

    pub(super) fn q99_rel(&self) -> f64 {
        self.q99_rel
    }

    /// 창에 든 표본 수(≤ `DELAY_WINDOW`).
    pub(super) fn count(&self) -> usize {
        self.transit_count
    }

    pub(super) fn rfc_jitter(&self) -> f64 {
        self.rfc_jitter
    }

    /// 표본 하나를 넣는다. 재계산 주기면 분위수·최솟값을 다시 계산하고 `true`(호출한 쪽이 짧은 창 추정을 갱신한다),
    /// 아니면 최솟값만 낮추고 `false`.
    pub(super) fn push(&mut self, transit: i64, quantile: f64) -> bool {
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
            self.recompute(quantile);
            true
        } else {
            if transit < self.min_transit {
                self.min_transit = transit;
            }
            false
        }
    }

    /// 경로 변경: transit 창만 버린다(분위수·최솟값·RFC 지터는 다음 재계산까지 남는다).
    pub(super) fn reset(&mut self) {
        self.transit_count = 0;
        self.transit_pos = 0;
        self.since_stats = 0;
        self.last_transit = None;
    }

    /// 창에서 최솟값·프로필 분위수(`quantile`)·p99를 다시 계산한다. 창이 비었으면 그대로.
    pub(super) fn recompute(&mut self, quantile: f64) {
        let n = self.transit_count;
        if n == 0 {
            return;
        }
        self.scratch.clear();
        self.scratch.extend_from_slice(&self.transits[..n]);
        let min = self.scratch.iter().copied().min().unwrap_or(0);
        let k = ((n as f64 * quantile).ceil() as usize).clamp(1, n) - 1;
        let (_, p, _) = self.scratch.select_nth_unstable(k);
        self.q_rel = (*p - min) as f64;
        let k99 = ((n as f64 * 0.99).ceil() as usize).clamp(1, n) - 1;
        let (_, p99, _) = self.scratch.select_nth_unstable(k99);
        self.q99_rel = (*p99 - min) as f64;
        self.min_transit = min;
    }

    #[cfg(test)]
    pub(super) fn scratch_is_empty(&self) -> bool {
        self.scratch.is_empty()
    }
}
