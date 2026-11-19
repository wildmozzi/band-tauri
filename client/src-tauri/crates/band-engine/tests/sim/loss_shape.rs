//! 손실 모양 통계(P2-17h) 하네스: 무작위 손실과 Gilbert–Elliott 버스트 손실을 같은 평균 손실로 돌려 `LossShapeStats`
//! (손실·마감 놓침 연속 길이, 도착 여유, 은닉 방식) 분포 표를 찍고, 그 통계만으로 "바로 복사 / 1프레임 늦춘 복사·FEC가
//! 막았을 비율"을 추정한다(P2-17b 결정용). 추정식은 `LossShapeStats::estimate`(band-dsp `jitter/shape.rs` 머리말)이고, 실합주
//! 진단 내보내기에도 같은 식을 쓴다(`examples/loss_shape_estimate.rs`).
//!
//! 비율은 두 분모로 낸다(#230 리뷰 M1): 손실 대비(끝내 안 온 패킷)와 은닉 원인 대비(손실 + 마감 놓침). 마감 놓침은 어느
//! 방식으로도 못 살린다(사본도 같은 경로로 늦는다).
//!
//! 도착 여유는 워커의 실제 은닉 마감 기준 "마감 − 도착"을 패킷마다 직접 잰 값이다(#230 리뷰 H1) — 여기 sim은 실제 워커
//! (`PeerFeed`)를 돌리므로 마감 시계도 엔진과 같다.

use super::tune::{run_multi, JitDist, MultiSim, NetPeer};
use band_dsp::jitter::LossShapeStats;

fn pct(v: Option<f64>) -> String {
    v.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or("-".into())
}

fn row(label: &str, sh: &LossShapeStats, net_lost: u64, sent: u64) -> band_dsp::jitter::ShapeEstimate {
    let e = sh.estimate();
    let total: u64 = sh.arrival_margin.iter().sum();
    let m: Vec<String> = sh.arrival_margin.iter().map(|&n| format!("{:.1}", n as f64 * 100.0 / total.max(1) as f64)).collect();
    println!(
        "| {label} | {:.2}% | {:?} | {:?} | {} | FEC {} PLC {} PCM {} (언더런 {}) | {} | {} / {} | {} / {} |",
        net_lost as f64 * 100.0 / sent.max(1) as f64,
        sh.loss_runs,
        sh.late_runs,
        m.join(" / "),
        sh.conceal_fec,
        sh.conceal_plc,
        sh.conceal_pcm,
        sh.conceal_underrun,
        pct(e.margin_ge_f),
        pct(Some(e.dup_now_of_loss)),
        pct(Some(e.dup_now_of_causes)),
        pct(e.dup_1f_of_loss),
        pct(e.dup_1f_of_causes),
    );
    e
}

/// 무작위 1% vs Gilbert–Elliott(평균 연속 4패킷, 평균 손실 ≈ 1%) × F 2.5·5ms × 지터(균등 1ms / 지수 평균 2ms — 와이파이 비슷).
/// 표는 PR 본문(P2-17h)에 옮긴다. 단언은 통계가 손실 모양을 구분하는지와 도착 여유가 정의대로(제때 재생 ≥ 0) 나오는지만 본다.
#[test]
fn loss_shape_random_vs_burst() {
    println!("| 시나리오 | 망 손실 | 손실 연속 1/2/3–4/5–8/9–16/17+ | 마감 놓침 연속 | 도착 여유 % (<0/0–.5/.5–1/1–2/2–2.5/2.5–3/3–5/5–7.5/7.5–10/≥10ms) | 은닉 방식 | 여유≥F | 바로 복사* 손실/원인 | 1F 복사·FEC* 손실/원인 |");
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
                // 도착 여유는 재생한 패킷마다 하나 + 늦은 패킷: 재생 패킷 수와 같은 규모이고, 늦음(<0)은 마감 놓침보다 적지 않다.
                let total: u64 = sh.arrival_margin.iter().sum();
                assert!(total * 10 > p.js.produced * 9, "여유 표본 {total} / 생산 {}", p.js.produced);
                let late_pkts: u64 = sh.late_runs.iter().sum();
                assert!(sh.arrival_margin[0] >= late_pkts, "{sh:?}");
                let label = format!("F {} {dist_label} {}", frame as f64 / 48.0, if burst { "GE 버스트" } else { "무작위" });
                ests.push(row(&label, &sh, p.tx_lost, p.tx_sent));
            }
            let (rand, ge) = (&ests[0], &ests[1]);
            // 같은 평균 손실이면 버스트 쪽 연속 수가 훨씬 적고(연속이 길다), 바로 복사 어림(단발 비율)도 낮다.
            assert!(rand.lost > 50.0 && ge.lost > 50.0, "손실 표본 부족: {} / {}", rand.lost, ge.lost);
            assert!(ge.runs * 2 < rand.runs, "버스트 연속 수 {} vs 무작위 {}", ge.runs, rand.runs);
            assert!(rand.dup_now_of_loss > 0.9 && ge.dup_now_of_loss < 0.5, "단발 비율 무작위 {:.2} / 버스트 {:.2}", rand.dup_now_of_loss, ge.dup_now_of_loss);
        }
    }
    println!("* 추정(`LossShapeStats::estimate`): 바로 복사 = 단발 손실 수(상한 쪽), 1F 복사·FEC = 손실 연속 수 × P(여유 ≥ F). 손실 대비 / 은닉 원인(손실 + 마감 놓침) 대비.");
}

/// 도착 여유 정의 검증(#230 리뷰 H1): 패킷 2%를 정확히 1F 늦춰 보내 실제로 마감을 놓친 비율과, 같은 실행의 `1 − P(여유 ≥ F)`
/// (통계가 예측한 놓침 비율)를 비교한다. 직접 잰 여유라면 두 값이 맞아야 한다. 늦은 패킷은 늦은 패킷 예산·H1로 버퍼를 깊게
/// 하므로(늦춤 없는 실행과 버퍼가 다르다) 예측은 같은 실행의 분포로 한다 — 늦춤 없는 실행의 값도 함께 찍는다.
#[test]
fn loss_shape_margin_predicts_1f_late_misses() {
    println!("| 시나리오 | 버퍼 ms(늦춤 없음 → 늦춤) | 여유≥F 늦춤 없음 | 여유≥F 같은 실행 | 예측 놓침 | 1F 늦춘 패킷 | 실제 놓침 | 실제 놓침 비율 |");
    println!("|---|---|---|---|---|---|---|---|");
    for frame in [120usize, 240] {
        for (dist_label, dist, jit_us) in [("균등 1ms", JitDist::Uniform, 1000u64), ("균등 3ms", JitDist::Uniform, 3000), ("지수 2ms", JitDist::Exp, 2000)] {
            let mk = |late: Option<f64>| {
                let mut np = NetPeer::new(frame, 4200 + frame as u64 + jit_us);
                np.net_jitter_us = jit_us;
                np.dist = dist;
                np.late_1f = late;
                np
            };
            let f_ms = frame as f64 / 48.0;
            let base = run_multi(&MultiSim::new(30.0, vec![mk(None)]));
            let b = &base.peers[0].js;
            let base_ge_f = b.shape.margin_at_least(f_ms).expect("F 2.5·5ms는 버킷 경계");
            let base_late: u64 = b.shape.late_runs.iter().sum();
            let r = run_multi(&MultiSim::new(30.0, vec![mk(Some(0.02))]));
            let js = &r.peers[0].js;
            let ge_f = js.shape.margin_at_least(f_ms).unwrap();
            // 근사(#230 재리뷰 L4): `late_runs` 합은 연속 **수**(패킷 수 아님)이고, 늦춤 없는 놓침(`base_late`)은 **다른 실행**에서
            // 빼 쓴다. 지금 시나리오는 1F 늦춤이 거의 단발이고 `base_late`가 0이라 영향이 없다 — 지터가 큰 시나리오를 더하면
            // 늦춘 seq만 따로 세는 쪽으로 바꿀 것.
            let late: u64 = js.shape.late_runs.iter().sum::<u64>().saturating_sub(base_late);
            let delayed = r.peers[0].tx_late_1f;
            let miss = late as f64 / delayed.max(1) as f64;
            println!(
                "| F {f_ms} {dist_label} | {:.1} → {:.1} | {:.1}% | {:.1}% | {:.1}% | {delayed} | {late} | {:.1}% |",
                b.buffer_ms,
                js.buffer_ms,
                base_ge_f * 100.0,
                ge_f * 100.0,
                (1.0 - ge_f) * 100.0,
                miss * 100.0
            );
            assert!(delayed > 100, "늦춘 표본 부족 {delayed}");
            // 늦춘 패킷 자체(2%)가 분포의 낮은 쪽에 섞이고 목표가 실행 중에 움직이므로 정확히 같지는 않다 — 12%p 안이면 정의가 맞다.
            assert!((miss - (1.0 - ge_f)).abs() < 0.12, "F {frame} {dist_label}: 예측 {:.3} vs 실제 {miss:.3}", 1.0 - ge_f);
            // 마감 놓침은 <0 버킷에 든다(제때 재생된 패킷은 ≥0).
            let late_all: u64 = js.shape.late_runs.iter().sum();
            assert!(js.shape.arrival_margin[0] >= late_all, "{:?}", js.shape);
        }
    }
}
