//! 목표 깊이 추정(P1-5): 지연 통계, 짧은 창·버스트·긴 창, 경로 변경, 목표 합성.

use super::late_budget::LATE_SKIP_BLOCKS;
use super::{JitterBuffer, JitterTuning, BLOCK_SAMPLES_MAX, BURST_WIN, DELAY_WINDOW, LONG_BLOCKS_MAX, TARGET_GRID};

/// p95/최솟값 재계산 주기(패킷 수).
const STATS_EVERY: usize = 10;
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
/// 프로필 변경 때 `tail_extra`까지 비우는 은닉 목표 변화 배율(P1-6c, PR #65 리뷰 L2). 이보다 작은 변화는 늦은 비율만 비운다.
const TAIL_RESET_RATIO: f64 = 1.5;

impl JitterBuffer {
    /// 목표 상·하한(샘플). 하한은 `jitter_min_ms`(0이면 없음), 상한은 `jitter_max_ms`(하한 이상).
    fn target_limits(&self) -> (f64, f64) {
        let spm = Self::samples_per_ms();
        let lo = (self.settings.min_ms.max(0.0) as f64 * spm).max(0.0);
        let hi = (self.settings.max_ms as f64 * spm).max(lo);
        (lo, hi)
    }

    /// 지터 추정(샘플): 짧은 창·버스트·긴 창 중 큰 값.
    pub(super) fn jitter_est(&self) -> f64 {
        let burst = self.burst_abs.map(|a| (a - self.min_transit) as f64).unwrap_or(0.0);
        let short = self.short_cap.max(0.0);
        // 긴 창은 짧은 창 너머 초과분의 `long_weight`만 반영한다(PR #20 M4: 프로필에 따라 연속적으로).
        let long = short + (self.long_cap - short).max(0.0) * self.tuning.long_weight;
        short.max(burst).max(long)
    }

    /// 목표 깊이(샘플) = 지터 추정 + 소비자 요구 깊이(`lead`) + 삽입 진입 문턱 + 프로필 여유 + 늦은 패킷 예산 여유
    /// (`tail_extra`, ≤ 1프레임). 상한 `max_ms`.
    pub fn target_samples(&self) -> f64 {
        let (lo, hi) = self.target_limits();
        let (_, enter_ins, _, _) = self.deadbands();
        let pad = self.tuning.pad_frames * self.frame() as f64 + self.start_pad;
        (self.jitter_est() + self.consumer_lead + enter_ins + pad + self.tail_extra.min(self.frame() as f64)).clamp(lo, hi)
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
        let ratio = tuning.conceal_target / self.tuning.conceal_target;
        self.tuning = tuning;
        self.recompute_delay_stats();
        self.short_cap = self.short_cap.max(Self::grid_up(self.q_rel));
        self.update_long_cap();
        // 예산(은닉 목표)이 바뀌었으므로 옛 예산으로 잘라 쌓은 늦은 비율(옛 예산 × 4)은 버린다(PR #63 리뷰 L2). 망은 같으므로
        // 예산이 `TAIL_RESET_RATIO`배 넘게 바뀔 때만 `tail_extra`도 비운다 — 슬라이더 한두 칸(0.5 → 0.55, 목표 1.3 → 1.23%)
        // 으로 꼬리 대응을 처음부터 다시 쌓지 않게(PR #65 리뷰 L2). 작은 변화는 새 예산으로 다음 블록들에서 조정된다.
        if ratio.is_finite() && (1.0 / TAIL_RESET_RATIO..=TAIL_RESET_RATIO).contains(&ratio) {
            self.clear_late_rates();
        } else {
            self.reset_tail(false);
        }
        self.after_target_change();
    }

    pub(super) fn grid_up(x: f64) -> f64 {
        (x.max(0.0) / TARGET_GRID).ceil() * TARGET_GRID
    }

    /// 목표가 내려갔으면 해제 문턱까지 따라간다(버림 데드밴드 안에 지연이 남지 않게).
    pub(super) fn after_target_change(&mut self) {
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
            self.close_late_block();
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

    pub(super) fn record_delay(&mut self, ts: u32, now_us: u64) {
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
    pub(super) fn reset_delay_stats(&mut self, now_us: u64) {
        self.transit_count = 0;
        self.transit_pos = 0;
        self.since_stats = 0;
        self.last_transit = None;
        self.short_lower_since = Some(now_us);
        self.burst_abs = None;
        self.recent_rel = [0; BURST_WIN];
        self.block_rel.clear();
        self.block_start_us = None;
        // 늦은 패킷 예산도 새 경로 기준으로 다시 모은다(L2). 목표 변화는 `record_delay` 끝의 `after_target_change`가 반영한다.
        // 재생 시작처럼 창·블록이 비어 수렴 중이므로 처음 블록들은 판단에서 뺀다(PR #65 리뷰 L1).
        self.reset_tail(true);
        self.tail_skip = LATE_SKIP_BLOCKS;
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
}
