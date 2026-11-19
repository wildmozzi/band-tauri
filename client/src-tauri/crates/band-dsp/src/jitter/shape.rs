//! 손실 모양·은닉 방식·도착 여유 진단 통계(P2-17h, FEC/중복 전송 결정용 측정).
//!
//! 고정 배열 카운터만 쓴다(워커에서 돈다 — 할당·로그 없음). 정의는 설계 문서 1.2절 "손실 모양 통계"와 같다.
//!
//! - **손실 연속 길이**(`loss_runs`): 재생 쪽이 패킷 없이 넘긴 seq(손실 은닉, 언더런 은닉의 손실 재해석) 중 그 뒤
//!   256패킷(`SHAPE_HORIZON`) 안에 끝내 도착하지 않은 것의 연속 길이. 버킷 1, 2, 3–4, 5–8, 9–16, 17+ 패킷.
//! - **마감 놓침 연속 길이**(`late_runs`): 도착은 했지만 재생 마감을 놓친 seq의 연속 길이(같은 버킷). 마감을 놓친
//!   seq = 손실로 넘긴 뒤 늦게 온 것(`late`), 언더런 은닉 뒤 늦게 와 버린 것(`late_dropped`), 언더런 은닉 뒤 늦게 와
//!   재생한 것(깊이 +F). 셋 다 그 자리에서 은닉이 났다.
//! - **도착 여유**(`arrival_margin`): 재생 중 패킷이 도착한 순간 그 패킷의 재생 마감까지 남은 시간의 추정(ms, 음수 = 늦음).
//!   "도착 시 깊이 여유"로 정의한다: `margin = depth_ewma − (transit − min_transit) − consumer_lead`. 깊이 정의(`mod.rs`)로
//!   재생 위치의 ts는 `now − min_transit − depth`이므로 `depth − rel`은 도착 순간 그 패킷 ts가 재생 위치보다 앞선 양이고,
//!   목표 깊이가 지키는 `j + lead`와 같은 기준이다(`consumer_lead` = 은닉 마감 여유 1.5ms + 2·피어 적응 여유).
//!   **보수적(하한) 추정**이다: 깊이는 한 주기 중 가장 얕은 버스트 직후 값이라 실제 마감은 패킷 머리가 다음 출력 버스트의
//!   어디에 있는지에 따라 0..출력 콜백 주기만큼 더 늦고, 적응 여유(언더런 뒤 올림)도 마감이 아니라 쿠션이다. 그래서 0 바로
//!   밑(콜백 주기 이내)의 패킷은 은닉 없이 재생될 수 있다(`tests.rs` `shape_late_arrivals_are_deadline_misses_not_losses`).
//!   한 프레임(F) 늦게 보내는 복사본이나 다음 패킷에 실린 FEC는 같은 경로라면 여유가 `margin − F`이므로, `margin ≥ F`인
//!   비율이 "1프레임 늦춘 복사본/FEC가 마감 안에 들어오는 비율"의 추정치다. 경계에 2.5·5·10ms가 있어 F = 2.5·5ms의
//!   1F·2F를 버킷 합으로 바로 읽는다.
//! - **은닉 방식**(`conceal_fec`·`conceal_plc`·`conceal_pcm`): 은닉 프레임마다 실제로 쓴 방식(Opus FEC 디코드 / Opus PLC /
//!   PCM 페이드·무음 — PCM 코덱, 디코더 없음·실패). 셋의 합은 `JitterStats.concealed`다. `conceal_underrun`은 그중 언더런
//!   은닉(seq 유지)으로 낸 프레임 수(방식과 겹친다). 저지연 Opus(RESTRICTED_LOWDELAY, CELT 전용)에는 in-band FEC가 없어
//!   `decode_fec`가 PLC와 같은 소리를 낸다(P2-17b 측정) — `conceal_fec`는 "다음 패킷이 있어 FEC 디코드를 불렀다"는 뜻이다.

use serde::Serialize;

use super::seq_diff;

/// 연속 길이 버킷 수: 1, 2, 3–4, 5–8, 9–16, 17+ 패킷.
pub const RUN_BUCKETS: usize = 6;
/// 도착 여유 버킷 경계(ms). 버킷: <0, 0–0.5, 0.5–1, 1–2, 2–2.5, 2.5–3, 3–5, 5–7.5, 7.5–10, ≥10.
pub const MARGIN_EDGES_MS: [f64; 9] = [0.0, 0.5, 1.0, 2.0, 2.5, 3.0, 5.0, 7.5, 10.0];
pub const MARGIN_BUCKETS: usize = MARGIN_EDGES_MS.len() + 1;
/// 손실로 넘긴 seq가 이 패킷 수 안에 도착하면 마감 놓침(늦음), 끝내 안 오면 손실로 확정한다(2.5ms 기준 640ms).
pub(super) const SHAPE_HORIZON: usize = 256;
const HORIZON_MASK: u32 = SHAPE_HORIZON as u32 - 1;

/// 손실 모양 통계 스냅숏(누적, 지터 버퍼 `restart`에도 유지). 엔진 `PeerStats.lossShape`(설계 문서 4절)로 그대로 나간다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LossShapeStats {
    /// 현재 수신 패킷 프레임(ms). 연속 길이(패킷) × 이 값 ≈ ms.
    pub frame_ms: f32,
    /// 끝내 안 온 seq의 연속 길이 히스토그램(버킷 1, 2, 3–4, 5–8, 9–16, 17+ 패킷).
    pub loss_runs: [u64; RUN_BUCKETS],
    /// 도착했지만 마감을 놓친 seq의 연속 길이 히스토그램(같은 버킷).
    pub late_runs: [u64; RUN_BUCKETS],
    /// 도착 여유 히스토그램(ms 버킷 <0, 0–0.5, 0.5–1, 1–2, 2–2.5, 2.5–3, 3–5, 5–7.5, 7.5–10, ≥10). 재생 중 도착만.
    pub arrival_margin: [u64; MARGIN_BUCKETS],
    /// 은닉 프레임의 방식별 수(합 = `concealed`).
    pub conceal_fec: u64,
    pub conceal_plc: u64,
    pub conceal_pcm: u64,
    /// 은닉 프레임 중 언더런 은닉(seq 유지) 수.
    pub conceal_underrun: u64,
}

/// 은닉 프레임 하나를 만든 방식.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConcealKind {
    Fec,
    Plc,
    Pcm,
}

/// 재생 쪽이 seq 하나를 지나간 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SeqOutcome {
    /// 기록 없음(시작 건너뜀 등) — 연속을 끊는다.
    None = 0,
    /// 제때 재생(또는 깊이 보정으로 버림).
    Played = 1,
    /// 패킷 없이 넘김(아직 늦게 올 수 있다).
    Missing = 2,
    /// 도착했지만 마감을 놓침.
    Late = 3,
}

fn run_bucket(n: u32) -> usize {
    match n {
        0 | 1 => 0,
        2 => 1,
        3..=4 => 2,
        5..=8 => 3,
        9..=16 => 4,
        _ => 5,
    }
}

/// 도착 여유(ms) → 버킷.
pub(super) fn margin_bucket(ms: f64) -> usize {
    MARGIN_EDGES_MS.iter().position(|&e| ms < e).unwrap_or(MARGIN_EDGES_MS.len())
}

/// seq별 결과 링(최근 `SHAPE_HORIZON`개)과 진행 중인 연속 길이. 링을 벗어나는 seq를 확정해 히스토그램에 더한다.
pub(super) struct ShapeTracker {
    outcome: [u8; SHAPE_HORIZON],
    /// 아직 확정하지 않은 가장 옛 seq.
    base: u32,
    /// 다음에 기록할 seq(마지막 기록 + 1).
    head: u32,
    active: bool,
    loss_run: u32,
    late_run: u32,
}

impl ShapeTracker {
    pub(super) fn new() -> Self {
        Self { outcome: [0; SHAPE_HORIZON], base: 0, head: 0, active: false, loss_run: 0, late_run: 0 }
    }

    fn finalize(&mut self, o: u8, out: &mut LossShapeStats) {
        if o == SeqOutcome::Missing as u8 {
            self.loss_run += 1;
        } else if self.loss_run > 0 {
            out.loss_runs[run_bucket(self.loss_run)] += 1;
            self.loss_run = 0;
        }
        if o == SeqOutcome::Late as u8 {
            self.late_run += 1;
        } else if self.late_run > 0 {
            out.late_runs[run_bucket(self.late_run)] += 1;
            self.late_run = 0;
        }
    }

    fn finalize_base(&mut self, out: &mut LossShapeStats) {
        let i = (self.base & HORIZON_MASK) as usize;
        let o = self.outcome[i];
        self.outcome[i] = 0;
        self.finalize(o, out);
        self.base = self.base.wrapping_add(1);
    }

    /// 재생 쪽이 `seq`를 `o`로 지나갔다. seq는 오름차순으로 온다(건너뛴 seq는 `None`).
    pub(super) fn pass(&mut self, seq: u32, o: SeqOutcome, out: &mut LossShapeStats) {
        if !self.active {
            self.active = true;
            (self.base, self.head) = (seq, seq);
        }
        let gap = seq_diff(seq, self.head);
        if gap < 0 {
            return;
        }
        if gap as usize >= SHAPE_HORIZON {
            self.flush(out);
            self.active = true;
            (self.base, self.head) = (seq, seq);
        }
        // 기록할 자리를 비운다: seq + 1 − base ≤ HORIZON(건너뛴 seq는 이미 0 — 확정 때 지웠다).
        while seq_diff(seq, self.base) >= SHAPE_HORIZON as i32 {
            self.finalize_base(out);
        }
        self.outcome[(seq & HORIZON_MASK) as usize] = o as u8;
        self.head = seq.wrapping_add(1);
    }

    /// 넘긴 seq가 늦게 도착했다: 아직 확정 전이고 `Missing`이면 `Late`로 바꾼다.
    pub(super) fn arrived_late(&mut self, seq: u32) {
        if self.active && seq_diff(seq, self.base) >= 0 && seq_diff(self.head, seq) > 0 {
            let i = (seq & HORIZON_MASK) as usize;
            if self.outcome[i] == SeqOutcome::Missing as u8 {
                self.outcome[i] = SeqOutcome::Late as u8;
            }
        }
    }

    /// 남은 seq를 모두 확정하고 진행 중인 연속을 닫는다(스트림 리셋·재시작·시험).
    pub(super) fn flush(&mut self, out: &mut LossShapeStats) {
        if self.active {
            while seq_diff(self.head, self.base) > 0 {
                self.finalize_base(out);
            }
        }
        self.finalize(SeqOutcome::None as u8, out);
        self.active = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets() {
        assert_eq!([1, 2, 3, 4, 5, 8, 9, 16, 17, 900].map(run_bucket), [0, 1, 2, 2, 3, 3, 4, 4, 5, 5]);
        assert_eq!([-0.1, 0.0, 0.49, 0.5, 1.9, 2.0, 2.49, 2.5, 2.99, 3.0, 4.9, 5.0, 7.5, 9.9, 10.0, 50.0].map(margin_bucket), [0, 1, 1, 2, 3, 4, 4, 5, 5, 6, 6, 7, 8, 8, 9, 9]);
    }

    #[test]
    fn tracker_runs_and_late_reclassify() {
        let mut t = ShapeTracker::new();
        let mut s = LossShapeStats::default();
        // seq 0..: 재생 3, 손실 2(그중 하나는 늦게 옴), 재생, 손실 1, 재생 …
        let seqs = [SeqOutcome::Played, SeqOutcome::Played, SeqOutcome::Missing, SeqOutcome::Missing, SeqOutcome::Played, SeqOutcome::Missing, SeqOutcome::Played];
        for (i, o) in seqs.iter().enumerate() {
            t.pass((u32::MAX - 3).wrapping_add(i as u32), *o, &mut s); // u32 랩을 지나간다
        }
        t.arrived_late(u32::MAX);
        t.arrived_late(0); // 재생한 seq — 바뀌지 않음
        t.flush(&mut s);
        assert_eq!(s.loss_runs, [2, 0, 0, 0, 0, 0]);
        assert_eq!(s.late_runs, [1, 0, 0, 0, 0, 0]);
        // 확정(링 밖) 뒤 늦게 온 것은 손실로 남는다
        let mut t = ShapeTracker::new();
        let mut s = LossShapeStats::default();
        t.pass(0, SeqOutcome::Missing, &mut s);
        for k in 1..=SHAPE_HORIZON as u32 {
            t.pass(k, SeqOutcome::Played, &mut s);
        }
        t.arrived_late(0);
        t.flush(&mut s);
        assert_eq!((s.loss_runs[0], s.late_runs[0]), (1, 0));
    }
}
