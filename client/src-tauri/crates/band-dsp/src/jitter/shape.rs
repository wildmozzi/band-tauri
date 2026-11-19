//! 손실 모양·은닉 방식·도착 여유 진단 통계(P2-17h, FEC/중복 전송 결정용 측정).
//!
//! 고정 배열 카운터만 쓴다(워커에서 돈다 — 할당·로그 없음). 정의는 설계 문서 1.2절 "손실 모양 통계"와 같다.
//!
//! - **손실 연속 길이**(`loss_runs`): 재생 쪽이 패킷 없이 넘긴 seq(손실 은닉, 언더런 은닉의 손실 재해석) 중 그 뒤
//!   256패킷(`SHAPE_HORIZON`) 안에 끝내 도착하지 않은 것의 연속 길이. 버킷 1, 2, 3–4, 5–8, 9–16, 17+ 패킷. 17+ 연속은
//!   패킷 수 합(`loss_long_pkts`)도 센다.
//! - **마감 놓침 연속 길이**(`late_runs`): 도착은 했지만 재생 마감을 놓친 seq의 연속 길이(같은 버킷). 마감을 놓친
//!   seq = 손실로 넘긴 뒤 늦게 온 것(`late`), 언더런 은닉 뒤 늦게 와 버린 것(`late_dropped`), 언더런 은닉 뒤 늦게 와
//!   재생한 것(깊이 +F). 셋 다 그 자리에서 은닉이 났다.
//! - **도착 여유**(`arrival_margin`): 패킷마다 **직접 잰** "마감 − 도착"(ms, 음수 = 늦음). 슬롯에 도착 시각을 적어 두고,
//!   재생 쪽이 그 seq를 실제로 꺼낼(재생·버림) 때 그 패킷의 은닉 마감을 소비자 시계(`DeadlineClock`)로 계산해 뺀다.
//!   마감 = 그 seq가 없었다면 소비자가 은닉했을 시각: 패킷 머리가 소스 링의 `o`(48k 샘플, 재생 위치 기준)에 놓이면 콜백
//!   j번 뒤 링 점유가 `o − j·burst`이고, 이것이 `need`보다 적어지는 첫 콜백 `C_j`의 `guard` 전이다(`DeadlineClock::deadline_at`).
//!   그래서 워커가 마감보다 일찍 미리 꺼낸 패킷도 실제 마감까지의 여유로 센다. 제때 재생된 패킷은 정의상 0 이상이다(명목
//!   마감과 워커가 실제로 깨어난 시각 사이에 온 패킷은 0으로 센다). 마감을 놓친 패킷(손실로 넘긴 뒤 늦게 온 것, 언더런 은닉
//!   뒤 버리거나 재생한 것)은 그 seq를 넘기거나 처음 은닉한 시각을 마감으로 `−(도착 − 마감)`이다. 소비자 시계가 없으면(재생
//!   시작·언더런 뒤 건너뛰기·장치 클럭 추정 전) 세지 않는다. 남은 오차는 콜백 시각 추정(장치 클럭)과 워커 기상 지연뿐이다.
//!   같은 경로로 한 프레임(F) 늦게 오는 복사본이나 다음 패킷에 실린 FEC는 여유가 `margin − F`이므로 `margin ≥ F`인 비율이
//!   "1프레임 늦춘 복사본/FEC가 마감 안에 드는 비율"이다. 경계에 2.5·5·10ms가 있어 F = 2.5·5ms의 1F·2F를 버킷 합으로 읽는다.
//! - **은닉 방식**(`conceal_fec`·`conceal_plc`·`conceal_pcm`): 은닉 프레임마다 실제로 쓴 방식(Opus FEC 디코드 / Opus PLC /
//!   PCM 페이드·무음 — PCM 코덱, 디코더 없음·실패). 셋의 합은 `JitterStats.concealed`다. `conceal_underrun`은 그중 언더런
//!   은닉(seq 유지)으로 낸 프레임 수(방식과 겹친다). 저지연 Opus(RESTRICTED_LOWDELAY, CELT 전용)에는 in-band FEC가 없어
//!   `decode_fec`가 PLC와 같은 소리를 낸다(P2-17b 측정) — `conceal_fec`는 "다음 패킷이 있어 FEC 디코드를 불렀다"는 뜻이다.

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LossShapeStats {
    /// 현재 수신 패킷 프레임(ms). 연속 길이(패킷) × 이 값 ≈ ms.
    pub frame_ms: f32,
    /// 끝내 안 온 seq의 연속 길이 히스토그램(버킷 1, 2, 3–4, 5–8, 9–16, 17+ 패킷).
    pub loss_runs: [u64; RUN_BUCKETS],
    /// 17+ 버킷 연속들의 패킷 수 합(긴 버스트의 손실 수를 대표 길이로 어림하지 않게).
    #[serde(default)]
    pub loss_long_pkts: u64,
    /// 도착했지만 마감을 놓친 seq의 연속 길이 히스토그램(같은 버킷).
    pub late_runs: [u64; RUN_BUCKETS],
    /// 도착 여유(마감 − 도착) 히스토그램(ms 버킷 <0, 0–0.5, 0.5–1, 1–2, 2–2.5, 2.5–3, 3–5, 5–7.5, 7.5–10, ≥10). 재생 쪽이
    /// 꺼낸 패킷과 마감을 놓친 패킷마다 하나.
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

/// 소비자(워커)의 은닉 마감 시계(도착 여유 직접 측정, P2-17h). 워커가 `pop` 직전에 `JitterBuffer::set_deadline_clock`으로
/// 알린다. 시각은 `insert`의 `now_us`와 같은 시계(µs), 양은 48k 샘플이다. 재생 동작에는 쓰지 않는다(통계 전용).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeadlineClock {
    /// 다음 출력 콜백 예상 시각(µs).
    pub next_cb_us: f64,
    /// 출력 콜백 주기(µs)와 콜백 하나가 꺼내는 양(48k 샘플).
    pub period_us: f64,
    pub burst: f64,
    /// 알린 순간 소스 링(+출력 리샘플러)에 이미 있는 양(48k 샘플). 이후 `pop`한 양은 지터 버퍼가 더한다.
    pub ahead: f64,
    /// 은닉 판단: 콜백 `guard_us` 전에 링이 `need`(48k 샘플)보다 적으면 그 콜백에 모자란 만큼 은닉한다.
    pub need: f64,
    pub guard_us: f64,
}

impl DeadlineClock {
    /// 머리가 소스 링의 `offset`(48k 샘플, 재생 위치 기준)에 놓이는 데이터의 은닉 마감(µs): 링 점유가 `offset − j·burst`로
    /// 줄어 `need`보다 적어지는 첫 콜백 j의 `guard` 전.
    pub(crate) fn deadline_at(&self, offset: f64) -> f64 {
        let j = if offset < self.need { 0.0 } else { ((offset - self.need) / self.burst.max(1.0)).floor() + 1.0 };
        self.next_cb_us + j * self.period_us - self.guard_us
    }
}

/// 연속 길이 버킷의 대표 길이(17+는 `loss_long_pkts`가 있으면 그것으로 정확히 센다).
const RUN_LEN: [f64; RUN_BUCKETS] = [1.0, 2.0, 3.5, 6.5, 12.5, 17.0];

/// `LossShapeStats`만으로 낸 복사·FEC 효과 **추정**(통계에 개별 패킷의 운명은 없다). sim 표(`tests/sim/loss_shape.rs`)와
/// 실합주 진단 내보내기(`examples/loss_shape_estimate.rs`)가 같은 식을 쓴다.
///
/// - 1프레임 늦춘 복사(k의 사본을 k+1과 함께)·FEC(k+1에 k의 중복 정보): 손실 연속마다 마지막 seq만 살릴 수 있고(연속 안의
///   다른 seq는 실어 나를 다음 패킷도 잃었다), 실어 나른 패킷은 F 늦은 셈이라 도착 여유 ≥ F일 때만 마감 안에 든다
///   → 살린 수 ≈ 연속 수 × P(여유 ≥ F).
/// - 바로 복사(같은 순간 두 번): 단발 손실(연속 1)만 살린다고 본다 — 상관 손실이면 사본도 같이 잃으므로 상한 쪽 어림이다.
/// - 마감 놓침(늦음)은 어느 방식으로도 못 살린다(사본도 같은 경로로 늦는다). 그래서 비율을 두 분모로 낸다: 손실 대비
///   (`*_of_loss`)와 은닉 원인(손실 + 마감 놓침) 대비(`*_of_causes`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShapeEstimate {
    /// 손실로 확정한 패킷 수(1–16 패킷 연속은 버킷 대표 길이 어림, 17+는 `loss_long_pkts`).
    pub lost: f64,
    /// 마감 놓침 패킷 수(버킷 대표 길이 어림).
    pub late: f64,
    /// 손실 연속 수.
    pub runs: u64,
    /// 도착 여유 ≥ F 비율(0..1). F가 버킷 경계(2.5·5·10ms 등)가 아니면 None.
    pub margin_ge_f: Option<f64>,
    pub dup_now_of_loss: f64,
    pub dup_now_of_causes: f64,
    pub dup_1f_of_loss: Option<f64>,
    pub dup_1f_of_causes: Option<f64>,
}

impl LossShapeStats {
    /// 도착 여유가 `ms` 이상인 비율. `ms`가 버킷 경계가 아니면 None.
    pub fn margin_at_least(&self, ms: f64) -> Option<f64> {
        let total: u64 = self.arrival_margin.iter().sum();
        MARGIN_EDGES_MS
            .iter()
            .position(|&e| (e - ms).abs() < 1e-3)
            .map(|i| self.arrival_margin[i + 1..].iter().sum::<u64>() as f64 / total.max(1) as f64)
    }

    /// 복사·FEC 효과 추정(`ShapeEstimate`).
    pub fn estimate(&self) -> ShapeEstimate {
        let count = |r: &[u64; RUN_BUCKETS], long: Option<u64>| -> f64 {
            let short: f64 = r[..RUN_BUCKETS - 1].iter().zip(RUN_LEN).map(|(&n, l)| n as f64 * l).sum();
            short + long.map(|p| p as f64).unwrap_or(r[RUN_BUCKETS - 1] as f64 * RUN_LEN[RUN_BUCKETS - 1])
        };
        // 옛 기록(17+ 패킷 합 없음 — 0인데 17+ 연속이 있음)이면 대표 길이 17로 어림한다.
        let long = (self.loss_long_pkts > 0 || self.loss_runs[RUN_BUCKETS - 1] == 0).then_some(self.loss_long_pkts);
        let lost = count(&self.loss_runs, long);
        let late = count(&self.late_runs, None);
        let runs: u64 = self.loss_runs.iter().sum();
        let margin_ge_f = self.margin_at_least(self.frame_ms as f64);
        let causes = lost + late;
        let saved_now = self.loss_runs[0] as f64;
        let saved_1f = margin_ge_f.map(|m| runs as f64 * m);
        ShapeEstimate {
            lost,
            late,
            runs,
            margin_ge_f,
            dup_now_of_loss: saved_now / lost.max(1.0),
            dup_now_of_causes: saved_now / causes.max(1.0),
            dup_1f_of_loss: saved_1f.map(|v| v / lost.max(1.0)),
            dup_1f_of_causes: saved_1f.map(|v| v / causes.max(1.0)),
        }
    }
}

/// seq별 결과 링(최근 `SHAPE_HORIZON`개)과 진행 중인 연속 길이. 링을 벗어나는 seq를 확정해 히스토그램에 더한다.
pub(super) struct ShapeTracker {
    outcome: [u8; SHAPE_HORIZON],
    /// `Missing`으로 넘긴 seq의 은닉 마감(µs, 모르면 NaN). 늦게 오면 `마감 − 도착`(음수)을 센다.
    deadline: [f64; SHAPE_HORIZON],
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
        Self { outcome: [0; SHAPE_HORIZON], deadline: [f64::NAN; SHAPE_HORIZON], base: 0, head: 0, active: false, loss_run: 0, late_run: 0 }
    }

    fn finalize(&mut self, o: u8, out: &mut LossShapeStats) {
        if o == SeqOutcome::Missing as u8 {
            self.loss_run += 1;
        } else if self.loss_run > 0 {
            let b = run_bucket(self.loss_run);
            out.loss_runs[b] += 1;
            if b == RUN_BUCKETS - 1 {
                out.loss_long_pkts += self.loss_run as u64;
            }
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

    /// 재생 쪽이 `seq`를 `o`로 지나갔다. seq는 오름차순으로 온다(건너뛴 seq는 `None`). `deadline_us`: `Missing`이면 그 seq의
    /// 은닉 마감(모르면 None).
    pub(super) fn pass(&mut self, seq: u32, o: SeqOutcome, deadline_us: Option<f64>, out: &mut LossShapeStats) {
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
        let i = (seq & HORIZON_MASK) as usize;
        self.outcome[i] = o as u8;
        self.deadline[i] = deadline_us.unwrap_or(f64::NAN);
        self.head = seq.wrapping_add(1);
    }

    /// 넘긴 seq가 늦게 도착했다: 아직 확정 전이고 `Missing`이면 `Late`로 바꾸고 그 seq의 은닉 마감(알면)을 돌려준다.
    pub(super) fn arrived_late(&mut self, seq: u32) -> Option<f64> {
        if self.active && seq_diff(seq, self.base) >= 0 && seq_diff(self.head, seq) > 0 {
            let i = (seq & HORIZON_MASK) as usize;
            if self.outcome[i] == SeqOutcome::Missing as u8 {
                self.outcome[i] = SeqOutcome::Late as u8;
                let d = self.deadline[i];
                return (!d.is_nan()).then_some(d);
            }
        }
        None
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
            t.pass((u32::MAX - 3).wrapping_add(i as u32), *o, Some(i as f64), &mut s); // u32 랩을 지나간다
        }
        assert_eq!(t.arrived_late(u32::MAX), Some(3.0), "넘긴 seq의 마감을 돌려준다");
        assert_eq!(t.arrived_late(0), None); // 재생한 seq — 바뀌지 않음
        t.flush(&mut s);
        assert_eq!(s.loss_runs, [2, 0, 0, 0, 0, 0]);
        assert_eq!(s.late_runs, [1, 0, 0, 0, 0, 0]);
        // 확정(링 밖) 뒤 늦게 온 것은 손실로 남는다
        let mut t = ShapeTracker::new();
        let mut s = LossShapeStats::default();
        t.pass(0, SeqOutcome::Missing, None, &mut s);
        for k in 1..=SHAPE_HORIZON as u32 {
            t.pass(k, SeqOutcome::Played, None, &mut s);
        }
        assert_eq!(t.arrived_late(0), None);
        t.flush(&mut s);
        assert_eq!((s.loss_runs[0], s.late_runs[0]), (1, 0));
    }

    #[test]
    fn long_runs_count_packets_exactly() {
        let mut t = ShapeTracker::new();
        let mut s = LossShapeStats::default();
        for k in 0..40u32 {
            t.pass(k, if (5..35).contains(&k) { SeqOutcome::Missing } else { SeqOutcome::Played }, None, &mut s);
        }
        t.flush(&mut s);
        assert_eq!((s.loss_runs[5], s.loss_long_pkts), (1, 30));
        s.frame_ms = 2.5;
        assert_eq!(s.estimate().lost, 30.0, "17+ 연속은 대표 길이(17)가 아니라 실제 패킷 수로");
    }

    #[test]
    fn deadline_follows_callback_grid() {
        // 256프레임 콜백(5.333ms), need = 256(여유 0), guard 1.5ms
        let c = DeadlineClock { next_cb_us: 10_000.0, period_us: 5_333.0, burst: 256.0, ahead: 0.0, need: 256.0, guard_us: 1_500.0 };
        assert_eq!(c.deadline_at(0.0), 8_500.0); // 다음 콜백이 꺼낼 자리 → 그 콜백 직전 마감
        assert_eq!(c.deadline_at(255.0), 8_500.0);
        assert_eq!(c.deadline_at(256.0), 13_833.0); // 그다음 콜백
        assert_eq!(c.deadline_at(600.0), 19_166.0);
    }

    #[test]
    fn estimate_denominators() {
        // 단발 손실 10, 2연속 5개(10패킷), 마감 놓침 단발 20. F 5ms, 여유 ≥5ms 50%.
        let mut s = LossShapeStats { frame_ms: 5.0, ..Default::default() };
        s.loss_runs[0] = 10;
        s.loss_runs[1] = 5;
        s.late_runs[0] = 20;
        s.arrival_margin[margin_bucket(1.0)] = 50;
        s.arrival_margin[margin_bucket(6.0)] = 50;
        let e = s.estimate();
        assert_eq!((e.lost, e.late, e.runs, e.margin_ge_f), (20.0, 20.0, 15, Some(0.5)));
        assert_eq!((e.dup_now_of_loss, e.dup_now_of_causes), (0.5, 0.25));
        assert_eq!((e.dup_1f_of_loss, e.dup_1f_of_causes), (Some(0.375), Some(0.1875)));
        // F가 버킷 경계가 아니면(128샘플 2.667ms) ≥F 비율과 1F 추정이 없다.
        s.frame_ms = 128.0 / 48.0;
        assert_eq!((s.estimate().margin_ge_f, s.estimate().dup_1f_of_loss), (None, None));
    }
}
