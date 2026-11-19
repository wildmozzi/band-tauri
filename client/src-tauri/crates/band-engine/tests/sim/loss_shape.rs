//! 손실 모양 통계(P2-17h) 하네스: 무작위 손실과 Gilbert–Elliott 버스트 손실을 같은 평균 손실로 돌려 `LossShapeStats`
//! (손실·마감 놓침 연속 길이, 도착 여유, 은닉 방식) 분포 표를 찍고, 그 통계만으로 "바로 복사 / 1프레임 늦춘 복사 / FEC가
//! 막았을 손실 비율"을 추정한다(P2-17b 결정용). 실합주에서 모은 `PeerStats.lossShape`에도 같은 계산(`estimate`)을 쓴다.
//!
//! 추정(모두 **추정**이다 — 통계에는 개별 패킷의 운명이 없다):
//! - 1프레임 늦춘 복사(k의 사본을 k+1과 함께 보냄)·FEC(k+1에 k의 중복 정보): 손실 연속 하나마다 그 마지막 seq만 살릴 수 있다
//!   (연속 안의 다른 seq는 실어 나를 다음 패킷도 잃었다). 실어 나른 패킷은 F 늦은 셈이므로 도착 여유 ≥ F일 때만 마감 안에 든다.
//!   → 살린 수 ≈ 연속 수 × P(여유 ≥ F). 여유는 보수적(하한) 추정이라 이 값도 하한 쪽이다.
//! - 바로 복사(같은 순간 두 번 보냄): 손실이 패킷마다 독립이면 거의 다 살리고, 상태에 묶인 버스트면 사본도 같이 잃는다.
//!   단발 손실(연속 1)만 살린다고 보면 상한 쪽 어림이다 → 살린 수 ≈ 연속 1의 수.
//! - 마감 놓침(늦음)은 어느 방식으로도 못 살린다(사본도 같은 경로로 늦는다) — 분모(은닉 원인)에만 넣는다.

use super::tune::{run_multi, JitDist, MultiSim, NetPeer};
use band_dsp::jitter::{LossShapeStats, MARGIN_EDGES_MS, RUN_BUCKETS};

/// 연속 길이 버킷의 대표 길이(평균 손실 패킷 수 어림 — 17+는 17로 센다).
const RUN_LEN: [f64; RUN_BUCKETS] = [1.0, 2.0, 3.5, 6.5, 12.5, 17.0];

pub(super) struct Estimate {
    /// 손실로 확정한 패킷 수(어림).
    pub lost: f64,
    /// 손실 연속 수.
    pub runs: u64,
    /// 도착 중 여유 ≥ F 비율(0..1). F가 버킷 경계가 아니면 None.
    pub margin_ge_f: Option<f64>,
    /// 바로 복사·1프레임 늦춘 복사·FEC가 막았을 손실의 비율(손실 대비, 0..1).
    pub dup_now: f64,
    pub dup_1f: Option<f64>,
}

pub(super) fn estimate(sh: &LossShapeStats) -> Estimate {
    let lost: f64 = sh.loss_runs.iter().zip(RUN_LEN).map(|(&n, l)| n as f64 * l).sum();
    let runs: u64 = sh.loss_runs.iter().sum();
    let total: u64 = sh.arrival_margin.iter().sum();
    let f = sh.frame_ms as f64;
    // 버킷 i(≥1)의 아래 경계는 MARGIN_EDGES_MS[i-1]
    let margin_ge_f = MARGIN_EDGES_MS
        .iter()
        .position(|&e| (e - f).abs() < 1e-6)
        .map(|i| sh.arrival_margin[i + 1..].iter().sum::<u64>() as f64 / total.max(1) as f64);
    let dup_now = sh.loss_runs[0] as f64 / lost.max(1.0);
    let dup_1f = margin_ge_f.map(|m| runs as f64 * m / lost.max(1.0));
    Estimate { lost, runs, margin_ge_f, dup_now, dup_1f }
}

fn row(label: &str, sh: &LossShapeStats, net_lost: u64, sent: u64) -> Estimate {
    let e = estimate(sh);
    let total: u64 = sh.arrival_margin.iter().sum();
    let pct = |n: u64| n as f64 * 100.0 / total.max(1) as f64;
    let m: Vec<String> = sh.arrival_margin.iter().map(|&n| format!("{:.1}", pct(n))).collect();
    println!(
        "| {label} | {:.2}% | {:?} | {:?} | {} | FEC {} PLC {} PCM {} (언더런 {}) | {} | {:.0}% | {} |",
        net_lost as f64 * 100.0 / sent.max(1) as f64,
        sh.loss_runs,
        sh.late_runs,
        m.join(" / "),
        sh.conceal_fec,
        sh.conceal_plc,
        sh.conceal_pcm,
        sh.conceal_underrun,
        e.margin_ge_f.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or("-".into()),
        e.dup_now * 100.0,
        e.dup_1f.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or("-".into()),
    );
    e
}

/// 무작위 1% vs Gilbert–Elliott(평균 연속 4패킷, 평균 손실 ≈ 1%) × F 2.5·5ms × 지터(균등 1ms / 지수 평균 2ms — 와이파이 비슷).
/// 표는 PR 본문(P2-17h)에 옮긴다. 단언은 통계가 손실 모양을 구분하는지만 본다.
#[test]
fn loss_shape_random_vs_burst() {
    println!("| 시나리오 | 망 손실 | 손실 연속 1/2/3–4/5–8/9–16/17+ | 마감 놓침 연속 | 도착 여유 % (<0/0–.5/.5–1/1–2/2–2.5/2.5–3/3–5/5–7.5/7.5–10/≥10ms) | 은닉 방식 | 여유≥F | 바로 복사* | 1F 복사·FEC* |");
    println!("|---|---|---|---|---|---|---|---|---|");
    // GE: 나쁨→좋음 0.25(평균 연속 4), 좋음→나쁨 p로 정상 손실 p/(p+0.25) ≈ 1%.
    let p_gb = 0.01 * 0.25 / 0.99;
    for frame in [120usize, 240] {
        for (dist_label, dist, jit_us) in [("균등 1ms", JitDist::Uniform, 1000u64), ("지수 2ms", JitDist::Exp, 2000)] {
            let mut ests = Vec::new();
            for burst in [false, true] {
                let mut np = NetPeer::new(frame, 1700 + frame as u64 + jit_us);
                np.net_jitter_us = jit_us;
                np.dist = dist;
                if burst {
                    np.burst = Some((p_gb, 0.25));
                } else {
                    np.loss = 0.01;
                }
                let r = run_multi(&MultiSim::new(40.0, vec![np]));
                let p = &r.peers[0];
                let sh = p.js.shape;
                // 은닉 방식 합 = 은닉 프레임(기존 카운터 의미 그대로), PCM 송신이라 모두 PCM 은닉.
                assert_eq!(sh.conceal_fec + sh.conceal_plc + sh.conceal_pcm, p.js.concealed, "{sh:?}");
                assert_eq!(sh.conceal_pcm, p.js.concealed);
                let label = format!("F {} {dist_label} {}", frame as f64 / 48.0, if burst { "GE 버스트" } else { "무작위" });
                ests.push(row(&label, &sh, p.tx_lost, p.tx_sent));
            }
            let (rand, ge) = (&ests[0], &ests[1]);
            // 같은 평균 손실이면 버스트 쪽 연속 수가 훨씬 적고(연속이 길다), 바로 복사 어림(단발 비율)도 낮다.
            assert!(rand.lost > 50.0 && ge.lost > 50.0, "손실 표본 부족: {} / {}", rand.lost, ge.lost);
            assert!(ge.runs * 2 < rand.runs, "버스트 연속 수 {} vs 무작위 {}", ge.runs, rand.runs);
            assert!(rand.dup_now > 0.9 && ge.dup_now < 0.5, "단발 비율 무작위 {:.2} / 버스트 {:.2}", rand.dup_now, ge.dup_now);
        }
    }
    println!("* 추정(`loss_shape.rs` 머리말): 바로 복사 = 단발 손실 / 손실(상한 쪽), 1F 복사·FEC = 손실 연속 수 × P(여유 ≥ F) / 손실(하한 쪽).");
}
