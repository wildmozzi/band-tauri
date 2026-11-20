//! 혼돈·장시간 네트워크 시나리오(발굴 작업 d-chaos): `tune::run_multi`(실제 지터 버퍼·`PeerSources`·`OutputRt`)에
//! 기존 시나리오에 없던 악조건을 준다 — 순서 뒤바뀜 폭주, 중복, 수백 ms 버스트 손실, 지연 급변(20→150→20ms), 지터 분포 급변,
//! 피어 반복 이탈·재접속, 큰 클록 드리프트, 6피어 동시 악조건. 측정만 하고 엔진 로직은 바꾸지 않는다.
//!
//! 지표(창별, 100ms 표본 기준):
//! - `bufferMs` 중앙값·최댓값, 목표(`targetMs`).
//! - 은닉 비율 = 은닉 프레임 / 생산 프레임(손실 은닉 + 언더런 은닉), 언더런 은닉만 따로.
//! - 복구 시간: 사건이 끝난 뒤 bufferMs가 사건 전 중앙값 ±3ms 안으로 돌아오고 은닉이 멎은 마지막 시각까지(s).
//! - 출력 언더런(xruns), 재버퍼링 수, 늦은 패킷 수. 메모리·버퍼 증가: 워커 할당 0·참조 누수 없음은 `run_multi`가 단언하고,
//!   여기서는 bufferMs 최댓값과 첫 창·마지막 창 중앙값 차이(누적)를 본다.
//!
//! 기본 `cargo test`에는 짧은 것(합쳐 수 초)만 두고, 10분 6피어 혼돈은 `#[ignore]`(`chaos_six_peers_10min`)다:
//!
//! ```text
//! cargo test -p band-engine --no-default-features --features test-support --test sim chaos_ -- --nocapture
//! cargo test -p band-engine --no-default-features --features test-support --test sim chaos_six_peers_10min -- --ignored --nocapture
//! ```

use super::tune::{median, run_multi, JitDist, MultiOut, MultiSim, NetPeer, PeerOut};

fn us(s: f64) -> u64 {
    (s * 1e6) as u64
}

/// 창 [a, b) 초의 지표.
#[derive(Debug, Clone, Copy)]
struct Row {
    buf_med: f32,
    buf_max: f32,
    tgt_med: f32,
    conceal_pct: f64,
    underrun_pct: f64,
}

fn window(p: &PeerOut, a: f64, b: f64) -> Row {
    let w: Vec<_> = p.samples.iter().filter(|s| s.t >= a && s.t < b).collect();
    let playing: Vec<_> = w.iter().filter(|s| s.playing).collect();
    assert!(!w.is_empty(), "창 [{a}, {b})에 표본이 없다");
    // 누적 계수의 창 안 증가분. 재접속(`rejoins`)하면 새 PeerMedia라 계수가 0부터 다시 센다 — 줄어든 곳은 새 세션의 시작이다.
    let (mut prod, mut conc, mut conc_loss) = (0.0f64, 0.0f64, 0.0f64);
    for pair in w.windows(2) {
        let (x, y) = (pair[0], pair[1]);
        let fresh = y.produced < x.produced;
        let d = |a: u64, b: u64| if fresh { b as f64 } else { b as f64 - a as f64 };
        prod += d(x.produced, y.produced);
        conc += d(x.concealed, y.concealed);
        conc_loss += d(x.concealed_loss, y.concealed_loss);
    }
    let prod = prod.max(1.0);
    Row {
        buf_med: median(playing.iter().map(|s| s.buf).collect()),
        buf_max: playing.iter().map(|s| s.buf).fold(0.0, f32::max),
        tgt_med: median(playing.iter().map(|s| s.tgt).collect()),
        conceal_pct: conc * 100.0 / prod,
        underrun_pct: (conc - conc_loss) * 100.0 / prod,
    }
}

/// 사건 끝(`end` s) 뒤 복구 시간(s): bufferMs가 `base` ±3ms 밖이거나 언더런 은닉이 난 마지막 표본 시각 − end. `horizon` s까지
/// 본다. 손실 은닉(무작위 손실을 따라가는 몫)은 사건과 무관하므로 세지 않는다. 언더런 은닉 수는 단조가 아니다 — 뒤 패킷이
/// 오면 손실로 다시 분류된다(P1-6a) — 그래서 표본마다 그 뒤 1초(10표본)의 최솟값으로 본다.
fn recovery(p: &PeerOut, end: f64, horizon: f64, base: f32) -> f64 {
    let s = &p.samples;
    let under = |j: usize| s[j..(j + 10).min(s.len())].iter().map(|x| x.concealed - x.concealed_loss).min().unwrap_or(0);
    let Some(first) = s.iter().position(|x| x.t >= end) else {
        return 0.0;
    };
    let mut last_bad = end;
    let mut prev = if first > 0 { under(first - 1) } else { 0 };
    for (j, x) in s.iter().enumerate().skip(first) {
        if x.t >= end + horizon {
            break;
        }
        let u = under(j);
        if !x.playing || (x.buf - base).abs() > 3.0 || u > prev {
            last_bad = x.t;
        }
        prev = u;
    }
    last_bad - end
}

fn line(label: &str, r: &Row) {
    println!(
        "CHAOS {label}: bufferMs 중앙 {:.2} 최대 {:.2}, 목표 {:.2}, 은닉 {:.2}% (언더런 {:.2}%)",
        r.buf_med, r.buf_max, r.tgt_med, r.conceal_pct, r.underrun_pct
    );
}

fn base_peer(frame: usize, seed: u64) -> NetPeer {
    let mut np = NetPeer::new(frame, seed);
    np.net_jitter_us = 1000;
    np
}

/// 순서 뒤바뀜 폭주: 패킷 30%가 U(0..10ms) 더 늦게 와 뒤 패킷(2.5ms 간격)에 최대 4개까지 추월당한다. 은닉은 손실이 아니라
/// 늦음에서만 나므로 지터 버퍼가 재배열 깊이만큼 목표를 올려 흡수해야 한다. 순서만 바뀌고 손실은 0이다.
#[test]
fn chaos_reorder_storm() {
    let mut np = base_peer(120, 501);
    np.reorder = Some((0.3, 10_000));
    let r = run_multi(&MultiSim::new(40.0, vec![np]));
    let p = &r.peers[0];
    let w = window(p, 10.0, 40.0);
    line("순서 뒤바뀜 30%·10ms", &w);
    println!("CHAOS 순서 뒤바뀜: 늦은 패킷 {}, 늦어서 버림 {}, 재버퍼링 {}, xruns {}", p.js.late, p.js.late_dropped, p.js.rebuffers, r.xruns);
    // 손실 0인 회선: 은닉은 언더런뿐이고 GOAL 2절의 언더런 예산(0.5%) 안이어야 한다. 지연은 재배열 깊이(10ms) + 1프레임 근처.
    assert!(w.underrun_pct <= 0.5, "순서 뒤바뀜 언더런 은닉 {:.2}%", w.underrun_pct);
    assert!(w.buf_max <= 30.0, "순서 뒤바뀜 bufferMs 최대 {:.2}", w.buf_max);
    assert_eq!(r.xruns, 0);
}

/// 사본이 있는 실행이 사본 없는 같은 회선(같은 시드 — 사본 난수는 따로 뽑으므로 손실·지터 실현이 같다, #185 리뷰 M1)과
/// 지터 버퍼 상태가 같아야 한다. 사본은 원본보다 먼저 오지 않고 받은 seq의 사본은 아무것도 바꾸지 않으므로 정확히 같다.
fn assert_same_line(label: &str, clean: &MultiOut, dupd: &MultiOut) {
    let (c, d) = (&clean.peers[0], &dupd.peers[0]);
    assert_eq!((c.tx_sent, c.tx_lost), (d.tx_sent, d.tx_lost), "{label}: 망 손실 실현이 다르다(사본 난수가 손실 난수열을 밀었다)");
    let key = |p: &PeerOut| (p.js.late, p.js.concealed, p.js.concealed_loss, p.js.produced, p.js.late_dropped, p.js.rebuffers, p.js.samples_dropped, p.js.samples_inserted);
    assert_eq!(key(c), key(d), "{label}: 사본이 지터 버퍼 통계를 바꿨다");
    let bufs = |p: &PeerOut| p.trace.iter().map(|x| x.1.to_bits()).collect::<Vec<_>>();
    assert!(bufs(c) == bufs(d), "{label}: 사본이 bufferMs 궤적을 바꿨다");
    assert_eq!(dupd.xruns, 0);
}

/// 중복: 패킷 20%가 사본 하나를 원본 + U(0..20ms) 뒤에 더 받는다(경로 전환 유예 동안 릴레이·직접 둘 다 받기 등 — 오디오
/// 패킷은 net에 재전송 창이 없어 사본이 지터 버퍼까지 간다). 사본은 소리를 바꾸지 않아야 하고, 지연·은닉이 중복 없는 같은
/// 회선(같은 시드)과 같아야 한다.
#[test]
fn chaos_duplicates_do_not_inflate() {
    let run = |dup: Option<(f64, u64)>| {
        let mut np = base_peer(120, 502);
        np.loss = 0.01;
        np.dup = dup;
        run_multi(&MultiSim::new(40.0, vec![np]))
    };
    let (clean, dupd) = (run(None), run(Some((0.2, 20_000))));
    let (a, b) = (window(&clean.peers[0], 10.0, 40.0), window(&dupd.peers[0], 10.0, 40.0));
    line("중복 없음(기준)", &a);
    line("중복 20%·+20ms", &b);
    let (jc, jd) = (&clean.peers[0].js, &dupd.peers[0].js);
    println!(
        "CHAOS 중복: 사본 {}, 늦은 패킷 {} → {}, 망 손실 {:.2}% → {:.2}%, 손실률 표시(최근 5s) {:.2}% → {:.2}%, 늦은 패킷 예산 {:.2} → {:.2}ms",
        dupd.peers[0].tx_dup,
        jc.late,
        jd.late,
        clean.peers[0].tx_lost as f64 * 100.0 / clean.peers[0].tx_sent as f64,
        dupd.peers[0].tx_lost as f64 * 100.0 / dupd.peers[0].tx_sent as f64,
        jc.loss_pct,
        jd.loss_pct,
        jc.tail_ms,
        jd.tail_ms
    );
    assert_same_line("중복", &clean, &dupd);
}

/// 수백 ms 버스트 손실: 10s·20s·30s에 200·500·800ms 동안 아무것도 오지 않는다(손실 1% 회선). 공백은 은닉(또는 재버퍼링)으로
/// 메우고, 끝나면 사건 전 지연으로 돌아와야 한다(지연을 쌓아 두지 않음).
#[test]
fn chaos_burst_loss_hundreds_ms() {
    for frame in [120usize, 240] {
        let mut np = base_peer(frame, 503);
        np.loss = 0.01;
        np.outages = vec![(us(10.0), 200_000), (us(20.0), 500_000), (us(30.0), 800_000)];
        let r = run_multi(&MultiSim::new(42.0, vec![np]));
        let p = &r.peers[0];
        let pre = window(p, 5.0, 10.0);
        line(&format!("버스트 손실 F {frame} 사건 전"), &pre);
        for (t0, len) in [(10.0, 0.2), (20.0, 0.5), (30.0, 0.8)] {
            let rec = recovery(p, t0 + len, 9.0, pre.buf_med);
            let after = window(p, t0 + len + 3.0, t0 + 10.0);
            println!("CHAOS 버스트 손실 F {frame} {:.0}ms@{t0:.0}s: 복구 {rec:.1}s, 이후 bufferMs {:.2}, 은닉 {:.2}%", len * 1000.0, after.buf_med, after.conceal_pct);
            assert!(rec <= 3.0, "F {frame} {len}s 손실 뒤 복구 {rec:.1}s");
            assert!((after.buf_med - pre.buf_med).abs() <= 1.5, "F {frame} {len}s 손실 뒤 지연이 남았다: {:.2} → {:.2}", pre.buf_med, after.buf_med);
        }
        println!("CHAOS 버스트 손실 F {frame}: 재버퍼링 {}, xruns {}", p.js.rebuffers, r.xruns);
        assert_eq!(r.xruns, 0);
    }
}

/// 지연 급변: 기본 지연 20ms → 10s에 150ms → 20s에 20ms(경로 전환·셀룰러 핸드오버 같은 계단). 올라갈 때 130ms 공백은 피할 수
/// 없다(은닉/재버퍼링). 내려갈 때 130ms어치가 한꺼번에 몰려오는데, 그 묶음을 지연으로 쌓아 두지 말고 목표로 돌아와야 한다.
#[test]
fn chaos_delay_jump_20_150_20() {
    for frame in [120usize, 240] {
        let mut np = base_peer(frame, 504);
        np.steps = vec![(0, 20_000, 1000), (us(10.0), 150_000, 1000), (us(20.0), 20_000, 1000)];
        let r = run_multi(&MultiSim::new(40.0, vec![np]));
        let p = &r.peers[0];
        let pre = window(p, 5.0, 10.0);
        let up = window(p, 10.0, 20.0);
        let rec_up = recovery(p, 10.0, 9.5, pre.buf_med);
        let rec_down = recovery(p, 20.0, 19.5, pre.buf_med);
        let tail = window(p, 30.0, 40.0);
        line(&format!("지연 급변 F {frame} 전"), &pre);
        line(&format!("지연 급변 F {frame} 150ms 구간"), &up);
        line(&format!("지연 급변 F {frame} 20ms 복귀 10s 뒤"), &tail);
        println!(
            "CHAOS 지연 급변 F {frame}: 올라갈 때 복구 {rec_up:.1}s, 내려갈 때 복구 {rec_down:.1}s, 10~12s 은닉 이벤트 {}, 20~22s 은닉 이벤트 {}, 재버퍼링 {}, 늦어서 버림 {}, xruns {}",
            p.conceal_between(us(10.0), us(12.0)),
            p.conceal_between(us(20.0), us(22.0)),
            p.js.rebuffers,
            p.js.late_dropped,
            r.xruns
        );
        // 올라가는 계단(P2-16b): 처음 0.5초(`STEP_UP_US`)는 버스트와 구분할 수 없어 버스트로 본다(목표 상한 80ms, 옛 최소 지연
        // 기준이라 bufferMs ≈ 135ms로 읽힘). 그 뒤 경로 지연 상승으로 판정해 지연 통계를 새 지연에서 다시 모으고 짧은 창·긴 창을
        // 계단 전 값으로 되돌린다. 전에는 목표가 3초쯤 80ms에 머물고 깊이를 키웠다가 보정으로 내려와 복귀 2.7s(F 2.5ms)·
        // 4.9s(F 5ms)였다. 지금 0.8s/0.8s.
        let hot = |a: f64, b: f64| p.trace.iter().filter(|x| x.0 >= a && x.0 < b && x.2 > pre.tgt_med + 3.0).count() as f64 * 0.1;
        let tgt_hot = hot(10.0, 20.0);
        let buf_hot = p.trace.iter().filter(|x| x.0 >= 10.0 && x.0 < 20.0 && x.1 > 80.0).count() as f64 * 0.1;
        println!("CHAOS 지연 급변 F {frame}: 목표 > 기준+3ms {tgt_hot:.1}s, bufferMs > 80ms {buf_hot:.1}s");
        assert!(rec_up <= 1.5, "F {frame}: 150ms로 올라간 뒤 복구 {rec_up:.1}s");
        assert!(tgt_hot <= 1.0, "F {frame}: 계단 뒤 목표가 {tgt_hot:.1}s 동안 올라 있었다(버스트로 해석)");
        assert!(buf_hot <= 1.0, "F {frame}: 계단 뒤 bufferMs > 80ms {buf_hot:.1}s");
        assert!(up.underrun_pct <= 1.5, "F {frame}: 150ms 구간 언더런 은닉 {:.2}%(130ms 공백 몫만)", up.underrun_pct);
        assert!(rec_down <= 1.5, "F {frame}: 20ms로 내려온 뒤 복구 {rec_down:.1}s");
        assert!((tail.buf_med - pre.buf_med).abs() <= 1.5, "F {frame}: 급변 뒤 지연이 남았다 {:.2} → {:.2}", pre.buf_med, tail.buf_med);
        assert_eq!(r.xruns, 0);
    }
}

/// 지연 계단 + 순서 뒤바뀜(P2-16e): 20 → 150ms 계단에 패킷 30%가 U(0..6ms) 더 늦는다(지터 2ms). 계단 판정 전 0.5초에 묶여 오는
/// 패킷이 생기면 늦은 패킷 버림(H1)이 옛 최소 지연 기준 깊이(≈130ms, 목표 상한 80ms)로 늦은 패킷을 버리고 다음 프레임을 은닉했다
/// (P2-16e 전: 늦어서 버림 F 2.5ms 3 / F 5ms 1, 10~11s 은닉 62 / 28). 지금은 감시 중 최소 지연을 잠정 바닥으로 보아 버리지 않는다
/// (0 / 0, 56 / 27 — 남은 은닉은 130ms 공백 몫).
#[test]
fn chaos_delay_jump_reordered() {
    for frame in [120usize, 240] {
        let mut np = base_peer(frame, 504);
        np.reorder = Some((0.3, 6_000));
        np.steps = vec![(0, 20_000, 2000), (us(10.0), 150_000, 2000), (us(20.0), 20_000, 2000)];
        let r = run_multi(&MultiSim::new(25.0, vec![np]));
        let p = &r.peers[0];
        let up = window(p, 10.0, 20.0);
        println!(
            "CHAOS 지연 급변+뒤바뀜 F {frame}: 10~11s 은닉 이벤트 {}, 11~20s {}, 늦어서 버림 {}, 150ms 구간 언더런 {:.2}%, xruns {}",
            p.conceal_between(us(10.0), us(11.0)),
            p.conceal_between(us(11.0), us(20.0)),
            p.js.late_dropped,
            up.underrun_pct,
            r.xruns
        );
        assert_eq!(p.js.late_dropped, 0, "F {frame}: 계단 판정 전 늦은 패킷 버림");
        assert!(up.underrun_pct <= 1.4, "F {frame}: 150ms 구간 언더런 은닉 {:.2}%", up.underrun_pct);
        assert_eq!(r.xruns, 0);
    }
}

/// 지터 분포 급변: 균등 1ms → 15s에 파레토(척도 3ms, 꼬리 김) / 반대로 파레토 3ms → 균등 0.5ms. 나빠지면 목표가 올라가
/// 은닉이 짧게 끝나야 하고, 좋아지면 목표가 다시 내려와야 한다(옛 꼬리를 오래 끌지 않음).
#[test]
fn chaos_jitter_distribution_switch() {
    // 나빠짐
    let mut np = base_peer(120, 505);
    np.dist_switch = Some((us(15.0), JitDist::Pareto));
    np.steps = vec![(us(15.0), 0, 3000)];
    let r = run_multi(&MultiSim::new(45.0, vec![np]));
    let p = &r.peers[0];
    let (pre, post) = (window(p, 5.0, 15.0), window(p, 20.0, 45.0));
    let up_at = p.first_target_at_least(15.0, pre.tgt_med + 3.0).map(|t| t - 15.0);
    line("분포 급변 균등1→파레토3 전", &pre);
    line("분포 급변 균등1→파레토3 뒤(5s~)", &post);
    println!("CHAOS 분포 급변(나빠짐): 목표 +3ms까지 {up_at:?}s, 15~20s 은닉 이벤트 {}, xruns {}", p.conceal_between(us(15.0), us(20.0)), r.xruns);
    assert!(post.underrun_pct <= 0.5, "파레토로 바뀐 뒤 정착 언더런 {:.2}%", post.underrun_pct);
    assert_eq!(r.xruns, 0);
    // 좋아짐
    let mut np = base_peer(120, 506);
    np.dist = JitDist::Pareto;
    np.net_jitter_us = 3000;
    np.dist_switch = Some((us(15.0), JitDist::Uniform));
    np.steps = vec![(us(15.0), 0, 500)];
    let r = run_multi(&MultiSim::new(60.0, vec![np]));
    let p = &r.peers[0];
    let (pre, post) = (window(p, 5.0, 15.0), window(p, 45.0, 60.0));
    let mut fresh = base_peer(120, 506);
    fresh.net_jitter_us = 500;
    let rf = run_multi(&MultiSim::new(30.0, vec![fresh]));
    let ref_w = window(&rf.peers[0], 10.0, 30.0);
    line("분포 급변 파레토3→균등0.5 전", &pre);
    line("분포 급변 파레토3→균등0.5 30s 뒤", &post);
    line("처음부터 균등0.5(기준)", &ref_w);
    let down_at = p.trace.iter().find(|x| x.0 >= 15.0 && x.1 <= ref_w.buf_med + 1.0).map(|x| x.0 - 15.0);
    let tail_at = |t: f64| p.samples.iter().rev().find(|s| s.t <= t).map_or(0.0, |s| s.tail);
    println!("CHAOS 분포 급변(좋아짐): 기준+1ms까지 {down_at:?}s, tail 30s {:.2} / 45s {:.2}ms, xruns {}", tail_at(30.0), tail_at(45.0), r.xruns);
    // P2-16c: 늦은 도착이 없으면 늦은 패킷 예산(`tail_extra`)을 5블록마다 절반씩 내린다. 전에는 10블록마다 0.25ms라 30초 뒤에도
    // +1.3ms(6.61 vs 5.27ms)였다. 지금은 바뀐 지 25초 안에 예산 0, 30초 뒤 기준 +0.5ms 안.
    assert!(post.buf_med <= ref_w.buf_med + 0.5, "좋아진 뒤 30s에도 옛 꼬리 지연: {:.2} vs 기준 {:.2}", post.buf_med, ref_w.buf_med);
    assert_eq!(tail_at(40.0), 0.0, "좋아진 뒤 25초에 늦은 패킷 예산이 남았다");
    assert!(down_at.is_some_and(|d| d <= 20.0), "기준 +1ms까지 {down_at:?}s");
    assert_eq!(r.xruns, 0);
}

/// P2-16c 맞교환: 좋아졌다가(15s) 다시 파레토로 나빠질 때(50s) 빠른 내림으로 비운 예산을 다시 쌓는 동안 은닉이 조금 는다.
/// 측정(3시드 평균, 50~90s): 은닉 2.106 → 2.141%(처음 10s 2.923 → 3.032%), bufferMs 18.65 → 18.47ms. 파레토↔균등 20초마다 바뀌면
/// 1.043 → 1.096%, bufferMs 11.98 → 11.78ms. 상한은 측정값 + 0.05%p(튜닝 변경 시 재측정).
#[test]
fn chaos_jitter_improve_then_worsen_tradeoff() {
    let (mut c, mut c10) = (0.0, 0.0);
    for seed in 0..3u64 {
        let mut np = base_peer(120, 800 + seed);
        np.dist = JitDist::Pareto;
        np.net_jitter_us = 3000;
        np.dist_steps = vec![(us(15.0), JitDist::Uniform), (us(50.0), JitDist::Pareto)];
        np.steps = vec![(us(15.0), 0, 500), (us(50.0), 0, 3000)];
        let r = run_multi(&MultiSim::new(90.0, vec![np]));
        c += window(&r.peers[0], 50.0, 90.0).conceal_pct / 3.0;
        c10 += window(&r.peers[0], 50.0, 60.0).conceal_pct / 3.0;
        assert_eq!(r.xruns, 0);
    }
    println!("CHAOS 좋아짐→다시 파레토: 50~90s 은닉 {c:.3}%, 처음 10s {c10:.3}%");
    assert!(c <= 2.19 && c10 <= 3.08, "다시 나빠질 때 은닉 {c:.3}% / 처음 10s {c10:.3}%");
}

/// 피어 반복 이탈·재접속(churn): 피어 0은 그대로, 피어 1은 3초마다 1초 나갔다 들어오기 8번, 피어 2는 0.5초 세션을 반복한다.
/// 매 세션이 재생을 시작하고, 다른 피어의 이탈·재접속이 남은 피어에 은닉을 만들지 않으며, 슬롯·PeerMedia 누수가 없어야 한다
/// (`run_multi`가 워커 할당 0과 참조 수를 단언한다).
#[test]
fn chaos_churn_rejoin() {
    let mut peers: Vec<NetPeer> = (0..3).map(|i| base_peer(120, 507 + i)).collect();
    peers[1].rejoins = (0..8).map(|k| (us(4.0 + 3.0 * k as f64), us(5.0 + 3.0 * k as f64))).collect();
    peers[2].rejoins = (0..20).map(|k| (us(3.5 + 1.0 * k as f64), us(3.6 + 1.0 * k as f64))).collect();
    let r = run_multi(&MultiSim::new(32.0, peers));
    let joins = |i: usize| r.churn.iter().filter(|e| e.1 == i && e.2 == super::tune::ChurnEv::Join).count();
    let frees = r.churn.iter().filter(|e| matches!(e.2, super::tune::ChurnEv::Free(_))).count();
    println!("CHAOS 재접속: 피어1 재접속 {}회, 피어2 재접속 {}회, 슬롯 해제 {frees}, xruns {}", joins(1), joins(2), r.xruns);
    let steady = window(&r.peers[0], 5.0, 32.0);
    line("재접속 중 고정 피어 0", &steady);
    for i in [1usize, 2] {
        let p = &r.peers[i];
        let sess = &p.sessions;
        let silent = sess.iter().filter(|s| s.produced == 0).count();
        let conc: u64 = sess.iter().map(|s| s.concealed).sum();
        let prod: u64 = sess.iter().map(|s| s.produced).sum();
        println!("CHAOS 재접속 피어 {i}: 세션 {}, 재생 못 한 세션 {silent}, 은닉 {conc}/{prod} 프레임", sess.len());
        assert_eq!(silent, 0, "피어 {i}: 재생을 시작하지 못한 세션");
        // 의도된 엄격함: 무손실·지터 1ms 회선은 세션마다 은닉 0이다. 지터 버퍼 튜닝(시작 여유·목표)을 바꾸는 PR은 재측정해 맞출 것.
        assert_eq!(conc, 0, "피어 {i}: 무손실 회선 세션에서 은닉");
    }
    assert_eq!(joins(1), 8);
    assert_eq!(joins(2), 20);
    // 의도된 엄격함(피어 간 간섭은 0이어야 한다). 튜닝 변경 시 재측정.
    assert_eq!(steady.conceal_pct, 0.0, "다른 피어의 이탈·재접속이 고정 피어에 은닉을 만들었다");
    assert_eq!(r.xruns, 0);
}

/// 큰 클록 드리프트: ±200ppm(GOAL 범위 끝)과 ±1000ppm(값싼 USB 장치·블루투스 스트레스 — 보정 비율 상한 0.25% 안)에서
/// 지연이 쌓이거나 말라 가지 않아야 한다(첫 창·마지막 창 중앙값 차이), 은닉 없이.
#[test]
fn chaos_large_clock_drift() {
    for drift in [200.0, -200.0, 1000.0, -1000.0] {
        let mut np = base_peer(120, 508);
        np.drift_ppm = drift;
        let r = run_multi(&MultiSim::new(90.0, vec![np]));
        let p = &r.peers[0];
        let (first, last) = (window(p, 10.0, 20.0), window(p, 80.0, 90.0));
        line(&format!("드리프트 {drift:+}ppm 마지막 창"), &last);
        println!(
            "CHAOS 드리프트 {drift:+}ppm: 첫 창 {:.2} → 마지막 창 {:.2}ms, 보정 시간 {:.1}%, 최대 {:.1}센트, 버린/넣은 샘플 {}/{}, xruns {}",
            first.buf_med,
            last.buf_med,
            p.corr_time_pct(),
            p.js.max_corr_cents,
            p.js.samples_dropped,
            p.js.samples_inserted,
            r.xruns
        );
        assert!((last.buf_med - first.buf_med).abs() <= 1.0, "드리프트 {drift}: 지연 누적 {:.2} → {:.2}", first.buf_med, last.buf_med);
        // 의도된 엄격함: 무손실 회선에서 드리프트 보정(≤ 0.25%)은 은닉을 만들지 않는다. 보정 규칙·튜닝 변경 시 재측정.
        assert!(last.conceal_pct == 0.0, "드리프트 {drift}: 은닉 {:.2}%", last.conceal_pct);
        assert_eq!(r.xruns, 0);
    }
}

/// 6피어 동시 악조건: 피어마다 다른 혼돈(순서 뒤바뀜+손실, 중복+드리프트, 버스트 손실, 지연 급변, 분포 급변, 재접속)을 한 엔진에
/// 동시에 준다. 피어별 지표가 혼자 겪을 때와 같은 범위여야 하고(서로 새지 않음), 출력 언더런이 없어야 한다.
fn six_peer_chaos(seconds: f64) -> Vec<NetPeer> {
    let at = |f: f64| us(seconds * f);
    let mut v = Vec::new();
    let mut np = base_peer(120, 601);
    np.reorder = Some((0.2, 8000));
    np.loss = 0.01;
    np.drift_ppm = 150.0;
    v.push(np);
    let mut np = base_peer(120, 602);
    np.dup = Some((0.1, 15_000));
    np.drift_ppm = -200.0;
    v.push(np);
    let mut np = base_peer(240, 603);
    np.loss = 0.01;
    np.outages = (1..10).map(|k| (at(k as f64 / 10.0), 300_000 + 50_000 * k as u64)).collect();
    np.drift_ppm = 200.0;
    v.push(np);
    let mut np = base_peer(120, 604);
    np.steps = vec![(0, 20_000, 1000), (at(0.3), 150_000, 1000), (at(0.4), 20_000, 1000), (at(0.7), 150_000, 3000), (at(0.8), 20_000, 1000)];
    v.push(np);
    let mut np = base_peer(120, 605);
    np.dist_switch = Some((at(0.5), JitDist::Pareto));
    np.steps = vec![(at(0.5), 0, 3000)];
    np.loss = 0.005;
    v.push(np);
    let mut np = base_peer(120, 606);
    np.rejoins = (1..10).map(|k| (at(k as f64 / 10.0), at(k as f64 / 10.0) + 1_500_000)).collect();
    np.drift_ppm = -120.0;
    v.push(np);
    v
}

fn run_six(seconds: f64) {
    let peers = six_peer_chaos(seconds);
    let r = run_multi(&MultiSim::new(seconds, peers));
    let names = ["순서+손실1%", "중복+−200ppm", "버스트 손실 9회", "지연 급변 ×2", "분포 급변", "재접속 9회"];
    println!("CHAOS 6피어 {seconds:.0}s: xruns {}", r.xruns);
    for (i, p) in r.peers.iter().enumerate() {
        let (first, last) = (window(p, 5.0, seconds * 0.1), window(p, seconds * 0.9 + 2.0, seconds));
        let whole = window(p, 5.0, seconds);
        println!(
            "CHAOS 6피어 #{i} {}: 전체 은닉 {:.2}% (언더런 {:.2}%), bufferMs 최대 {:.2}, 첫 창 {:.2} → 마지막 창 {:.2}ms, 재버퍼링 {}",
            names[i], whole.conceal_pct, whole.underrun_pct, whole.buf_max, first.buf_med, last.buf_med, p.js.rebuffers
        );
        // 버퍼 증가 없음: 마지막 창 지연이 첫 창보다 크게 늘지 않는다(지터 버퍼 max_ms 80 안). #4는 중간에 회선이 실제로
        // 나빠져(균등 1ms → 파레토 3ms) 목표가 오르는 것이 맞다 — 혼자 겪을 때(`chaos_jitter_distribution_switch`, 18.7ms)와 비교한다.
        let grow = if i == 4 { 15.0 } else { 2.0 };
        assert!(last.buf_med <= first.buf_med + grow, "#{i}: 지연 누적 {:.2} → {:.2}", first.buf_med, last.buf_med);
        // 지연 급변(#3)은 올라가는 계단에서 +130ms를 약 1초 쥔다(`chaos_delay_jump_20_150_20`) — 상한(80ms)을 넘는 유일한 경우.
        // #3 실측 135.8ms(60s)/136.5ms(600s) — 계단 크기(130ms) + 목표 근처라 여유를 15ms 둔다(#185 리뷰 L4). 튜닝 변경 시 재측정.
        let cap = if i == 3 { 150.0 } else { 80.5 };
        assert!(whole.buf_max <= cap, "#{i}: bufferMs 최대 {:.2}", whole.buf_max);
    }
    // 피어 0·1(손실 ≤ 1%, 사건 없음): GOAL 2절 은닉 ≤ 1.5%
    for i in [0usize, 1] {
        let w = window(&r.peers[i], 5.0, seconds);
        assert!(w.conceal_pct <= 1.5, "#{i}: 은닉 {:.2}%", w.conceal_pct);
    }
    assert_eq!(r.xruns, 0);
}

#[test]
fn chaos_six_peers_60s() {
    run_six(60.0);
}

/// 10분 버전(야간·수동): `--ignored`.
#[test]
#[ignore]
fn chaos_six_peers_10min() {
    run_six(600.0);
}

/// 오래된 사본(재생 공격·경로 루프 — 오디오 패킷에는 net 재전송 창이 없다): 패킷 5%가 원본 + U(0..2초) 뒤에 한 번 더 온다. 슬롯 창(1024패킷)
/// 안의 사본은 `JitterBuffer::insert`가 판별해 버린다. 지연·은닉이 오래된 사본 없는 같은 회선과 같은지 본다.
#[test]
fn chaos_stale_replays() {
    let run = |dup: Option<(f64, u64)>| {
        let mut np = base_peer(120, 509);
        np.dup = dup;
        run_multi(&MultiSim::new(40.0, vec![np]))
    };
    let (clean, rep) = (run(None), run(Some((0.05, 2_000_000))));
    let (a, b) = (window(&clean.peers[0], 10.0, 40.0), window(&rep.peers[0], 10.0, 40.0));
    line("오래된 사본 없음(기준)", &a);
    line("오래된 사본 5%·~2s", &b);
    println!("CHAOS 오래된 사본: 사본 {}, 늦은 패킷 {} → {}, 늦은 패킷 예산 {:.2} → {:.2}ms", rep.peers[0].tx_dup, clean.peers[0].js.late, rep.peers[0].js.late, clean.peers[0].js.tail_ms, rep.peers[0].js.tail_ms);
    // 2초 = 800패킷(F 2.5ms) < 슬롯 1024: 사본으로 판별돼 늦은 패킷으로 세지 않는다. 슬롯 창보다 오래된 사본은 여전히 늦은 패킷이다
    // (net에 오디오 재전송 창을 두는 것은 백로그 후보).
    assert_same_line("오래된 사본", &clean, &rep);
}
