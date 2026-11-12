//! 지터 버퍼 통계: 누적 카운터·비율 창과 표시용 스냅샷.

use super::{JitterBuffer, State};

/// 최근 N개 bool 중 true 비율(손실·은닉 창).
pub(super) struct RateRing {
    ring: Vec<bool>,
    pos: usize,
    len: usize,
    hits: usize,
}

impl RateRing {
    pub(super) fn new(cap: usize) -> Self {
        Self { ring: vec![false; cap], pos: 0, len: 0, hits: 0 }
    }

    pub(super) fn push(&mut self, v: bool, win: usize) {
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
    /// 은닉 중 손실 은닉(다음 패킷이 이미 있어 seq를 넘긴 것 — Opus면 FEC/PLC). 언더런 은닉을 낸 seq가 뒤에 손실로
    /// 판정되면 그 언더런 은닉도 손실 은닉으로 다시 센다(P1-6a). 나머지(`concealed - concealed_loss`)는 끝내 늦게 온
    /// 패킷 자리의 언더런 은닉(seq 유지, 깊이 +1F). 측정용(P1-6 리포트)이며 Tauri `PeerStats`에는 노출하지 않는다.
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
    /// 목표 중 늦은 패킷 예산 여유(`tail_extra`, ms). 측정용(P1-6c, PR #65 리뷰).
    pub tail_ms: f32,
    /// `tail_extra`를 바꾼 횟수(올림·내림)와 초기화(경로 변경·스트림 리셋·큰 프로필 변경) 횟수.
    /// 측정용(P1-6c, PR #65 리뷰 M1·L3). Tauri `PeerStats`에는 노출하지 않는다.
    pub tail_adjusts: u64,
    pub tail_resets: u64,
    /// 깊이·지터 추정의 기준인 최소 transit(샘플, 절대값이라 두 시점의 차이만 의미가 있다). 목표의 절대 수준은
    /// `min_transit + target`이다: 창의 가장 이른 표본이 빠져 min이 오르면 깊이는 바로 줄고 짧은 창 추정은 hold로
    /// 남아 절대 목표가 오른다(보정으로 따라감). 측정용(P3-0d, `link_matrix`). Tauri `PeerStats`에는 노출하지 않는다.
    pub min_transit: i64,
}

impl JitterStats {
    /// 샘플/프레임 보정 총 횟수.
    pub fn corrections(&self) -> u64 {
        self.samples_dropped + self.samples_inserted + self.dropped_frames
    }
}

impl JitterBuffer {
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
        s.tail_ms = self.tail_extra as f32 / spm;
        s.min_transit = self.min_transit;
        s
    }
}
