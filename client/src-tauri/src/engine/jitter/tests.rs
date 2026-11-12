use super::*;
use super::late_budget::{LATE_BUDGET_LOSSY_MAX, LATE_SKIP_BLOCKS};
use crate::engine::codec::Encoder;
use crate::engine::config::{frame_code_from_samples, CodecKind};
use crate::net::packet::{PacketType, FLAG_OPUS};
use rand::{Rng, SeedableRng};

struct Sender {
    enc: Encoder,
    frame: usize,
    seq: u32,
    ts: u32,
    phase: f64,
    opus: bool,
}

impl Sender {
    fn new(codec: CodecKind, frame: usize, seq0: u32, ts0: u32) -> Self {
        Self {
            enc: Encoder::new(codec, 1, 96_000).unwrap(),
            frame,
            seq: seq0,
            ts: ts0,
            phase: 0.0,
            opus: codec == CodecKind::Opus,
        }
    }
    fn next(&mut self) -> (Header, Vec<u8>) {
        let mut pcm = vec![0f32; self.frame];
        for s in pcm.iter_mut() {
            *s = (self.phase * 2.0 * std::f64::consts::PI).sin() as f32 * 0.5;
            self.phase = (self.phase + 440.0 / 48000.0).fract();
        }
        let mut buf = vec![0u8; MAX_PAYLOAD_BYTES];
        let n = self.enc.encode(&pcm, &mut buf).unwrap();
        buf.truncate(n);
        let h = Header {
            ptype: PacketType::Audio,
            ssrc: 1,
            seq: self.seq,
            timestamp: self.ts,
            flags: if self.opus { FLAG_OPUS } else { 0 },
            frame_code: frame_code_from_samples(self.frame).unwrap(),
        };
        self.seq = self.seq.wrapping_add(1);
        self.ts = self.ts.wrapping_add(self.frame as u32);
        (h, buf)
    }
}

#[derive(Clone, Copy)]
struct Sim {
    codec: CodecKind,
    /// 송신 패킷 프레임
    frame: usize,
    /// 수신 장치 콜백(버스트) 크기
    burst: usize,
    /// 워커 pop 단위
    chunk: usize,
    drift_ppm: f64,
    jitter_ms: f64,
    loss: f64,
    seconds: f64,
    seed: u64,
    /// 송신측 장치 콜백 크기(패킷이 이 단위로 몰려서 나감)
    tx_burst: usize,
    /// 망 지터를 지수 분포(평균 `jitter_ms`, 꼬리 김)로. 아니면 균등 U(0, jitter_ms).
    exp_jitter: bool,
    /// 늦은 패킷 예산을 끈다(은닉 목표 100% → 예산이 늘 늦은 비율보다 커서 `tail_extra` 0). 비교용.
    late_budget_off: bool,
}

impl Default for Sim {
    fn default() -> Self {
        Self {
            codec: CodecKind::Pcm16,
            frame: 240,
            burst: 240,
            chunk: 64,
            drift_ppm: 0.0,
            jitter_ms: 0.0,
            loss: 0.0,
            seconds: 5.0,
            seed: 1,
            tx_burst: 0,
            exp_jitter: false,
            late_budget_off: false,
        }
    }
}

struct SimResult {
    stats: JitterStats,
    /// (초, buffer_ms)
    trace: Vec<(f64, f32)>,
    /// 시작 1초 이후 무음 청크 수
    silent_chunks: usize,
    /// 시작 3초 이후 보정 횟수(재생 시작 여유 1프레임을 1초 뒤 걷어 내는 보정 제외)
    corrections_after_3s: u64,
    /// 5초 시점 통계(정착 후 보정 방향 확인용)
    stats_at_5s: JitterStats,
    /// 인접 샘플 불연속 수(L 채널, 차 > 0.08)
    discontinuities: usize,
    /// 끝 시점 늦은 패킷 예산 여유(ms)와 5초 시점 이후 은닉 비율(%).
    tail_extra_ms: f64,
    conceal_after_5s: f64,
    /// 망 손실률(%, 송신 패킷 중 버린 것).
    net_loss_pct: f64,
}

/// 이벤트 시뮬레이션. 송신 클럭은 (1+drift_ppm)배 빠르고, 네트워크 지연은 base + U(0,jitter),
/// 손실 확률은 loss다. 수신측은 `burst`마다 `chunk` 단위로 연달아 pop한 뒤 `end_burst`를 부른다.
fn simulate(p: Sim) -> SimResult {
    let mut rng = rand::rngs::StdRng::seed_from_u64(p.seed);
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    // 엔진의 워커는 항상 은닉 마감 여유(1.5ms) 이상을 소비자 요구로 알린다(`PeerFeed::consumer_lead`).
    jb.set_consumer_lead(72.0);
    if p.late_budget_off {
        jb.tuning.conceal_target = 1.0;
    }
    let mut tx = Sender::new(p.codec, p.frame, u32::MAX - 50, u32::MAX - 10_000);
    let send_period_us = p.frame as f64 / 48000.0 * 1e6 / (1.0 + p.drift_ppm * 1e-6);
    let burst_period_us = p.burst as f64 / 48000.0 * 1e6;
    let base_us = 3000.0;

    let mut arrivals: Vec<(f64, Header, Vec<u8>)> = Vec::new();
    let mut t = 0.0;
    let mut k = 0u64;
    let mut net_lost = 0u64;
    while t < p.seconds * 1e6 {
        let (h, pk) = tx.next();
        // 송신측 장치 버스트: tx_burst마다 몰아서 나감
        let send_t = if p.tx_burst > p.frame {
            let per = (p.tx_burst / p.frame) as u64;
            ((k / per + 1) * per) as f64 * send_period_us
        } else {
            t
        };
        if rng.gen::<f64>() < p.loss {
            net_lost += 1;
        } else {
            let u = rng.gen::<f64>();
            let j = if p.exp_jitter { -(1.0 - u).ln() } else { u };
            let d = base_us + j * p.jitter_ms * 1000.0;
            arrivals.push((send_t + d, h, pk));
        }
        t += send_period_us;
        k += 1;
    }
    arrivals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    let mut out = vec![0f32; p.chunk * 2];
    let mut ai = 0;
    let mut bt = 0.0;
    let mut trace = Vec::new();
    let mut silent = 0;
    let mut started = false;
    let mut corr_at_3s = None;
    let mut at_5s = None;
    let mut prev = None::<f32>;
    let mut disc = 0;
    let mut next_trace = 0.0;
    while bt < p.seconds * 1e6 {
        while ai < arrivals.len() && arrivals[ai].0 <= bt {
            let (at, h, pk) = &arrivals[ai];
            jb.insert(h, pk, *at as u64);
            ai += 1;
        }
        let mut left = p.burst;
        while left > 0 {
            let n = left.min(p.chunk);
            jb.pop(&mut out[..n * 2]);
            if started && bt > 1e6 {
                if out[..n * 2].iter().all(|x| x.abs() < 1e-6) {
                    silent += 1;
                }
                for fr in out[..n * 2].as_chunks::<2>().0 {
                    if let Some(pv) = prev {
                        if (fr[0] - pv).abs() > 0.08 {
                            disc += 1;
                        }
                    }
                    prev = Some(fr[0]);
                }
            }
            left -= n;
        }
        jb.end_burst(bt as u64, bt as u64, 0.0);
        if jb.is_playing() {
            started = true;
        }
        if bt >= 3e6 && corr_at_3s.is_none() {
            corr_at_3s = Some(jb.stats().corrections());
        }
        if bt >= 5e6 && at_5s.is_none() {
            at_5s = Some(jb.stats());
        }
        if bt >= next_trace {
            trace.push((bt / 1e6, jb.stats().buffer_ms));
            next_trace += 250_000.0;
        }
        bt += burst_period_us;
    }
    let stats = jb.stats();
    SimResult {
        corrections_after_3s: stats.corrections() - corr_at_3s.unwrap_or(0),
        stats_at_5s: at_5s.unwrap_or_default(),
        stats,
        trace,
        silent_chunks: silent,
        discontinuities: disc,
        tail_extra_ms: jb.tail_extra / 48.0,
        net_loss_pct: net_lost as f64 * 100.0 / k.max(1) as f64,
        conceal_after_5s: {
            let a = at_5s.unwrap_or_default();
            (stats.concealed - a.concealed) as f64 * 100.0 / (stats.produced - a.produced).max(1) as f64
        },
    }
}

fn mean_buffer(trace: &[(f64, f32)], from: f64, to: f64) -> f32 {
    let v: Vec<f32> = trace.iter().filter(|(t, _)| *t >= from && *t < to).map(|x| x.1).collect();
    v.iter().sum::<f32>() / v.len().max(1) as f32
}

#[test]
fn clean_network_zero_concealment_and_zero_corrections() {
    // 리뷰 매트릭스: 수신 장치 버퍼가 프레임보다 큰 경우 포함(M4), 드리프트 0이면 보정 0(M2)
    for (frame, burst) in [(240, 128), (120, 480), (240, 480), (240, 1024), (480, 1024), (120, 1024), (480, 128)] {
        let r = simulate(Sim { frame, burst, seconds: 6.0, ..Default::default() });
        assert_eq!(r.stats.concealed, 0, "frame {frame} burst {burst}: {:?}", r.stats);
        // 재생 시작 1초 뒤 시작 여유(1프레임)를 걷어 내는 보정 한 번은 빼고 센다(P1-5).
        assert_eq!(r.corrections_after_3s, 0, "드리프트 0이면 보정 0: frame {frame} burst {burst}: {:?}", r.stats);
        assert!(r.stats.corrections() <= frame as u64, "시작 여유 제거 보정은 1프레임 이하: {:?}", r.stats);
        assert_eq!(r.silent_chunks, 0);
        assert_eq!(r.discontinuities, 0, "frame {frame} burst {burst}");
        assert!(r.stats.buffer_ms < 12.0, "깨끗한 망은 지연이 작아야 함: {:?}", r.stats);
    }
}

#[test]
fn sender_bursts_are_absorbed() {
    // 송신측 장치 버퍼 480(10ms)마다 프레임 2.5ms 4개가 몰려 나가는 경우
    let r = simulate(Sim { frame: 120, burst: 480, tx_burst: 480, seconds: 6.0, ..Default::default() });
    assert_eq!(r.stats.concealed, 0, "{:?}", r.stats);
    assert_eq!(r.corrections_after_3s, 0, "{:?}", r.stats);
    assert_eq!(r.discontinuities, 0);
}

#[test]
fn jitter_and_loss_opus() {
    let r = simulate(Sim {
        codec: CodecKind::Opus,
        jitter_ms: 20.0,
        loss: 0.05,
        seconds: 20.0,
        seed: 2,
        ..Default::default()
    });
    let s = r.stats;
    let ratio = s.concealed as f64 / s.produced as f64;
    assert!(ratio < 0.08, "은닉 비율 {ratio} {s:?}");
    assert!((2.0..9.0).contains(&s.loss_pct), "손실률 추정 {s:?}");
    assert!(s.target_ms >= 15.0 && s.target_ms <= 35.0, "목표 깊이가 지터를 반영해야 함 {s:?}");
    let late = mean_buffer(&r.trace, 10.0, 20.0);
    assert!((late - s.target_ms).abs() < 4.0, "깊이 {late} vs 목표 {}", s.target_ms);
    assert_eq!(s.dropped_frames, 0, "정상 상태에서 패킷 단위 버림 없음: {s:?}");
}

#[test]
fn reordering_handled() {
    let r = simulate(Sim { frame: 120, burst: 128, jitter_ms: 20.0, seconds: 10.0, seed: 3, ..Default::default() });
    let ratio = r.stats.concealed as f64 / r.stats.produced as f64;
    assert!(ratio < 0.03, "순서 뒤바뀜에서 은닉 비율 {ratio} {:?}", r.stats);
}

fn drift_case(ppm: f64, burst: usize) {
    let r = simulate(Sim { burst, drift_ppm: ppm, jitter_ms: 5.0, seconds: 60.0, seed: 4, ..Default::default() });
    let s = r.stats;
    let mid = mean_buffer(&r.trace, 20.0, 30.0);
    let end = mean_buffer(&r.trace, 50.0, 60.0);
    println!("drift {ppm}ppm burst {burst}: target {:.2}ms mid {mid:.2}ms end {end:.2}ms stats {s:?}", s.target_ms);
    // 데드밴드(F/2 = 2.5ms) 안에서 수렴
    assert!((end - s.target_ms).abs() < 3.0, "드리프트 {ppm}ppm: end={end} target={}", s.target_ms);
    assert!((end - mid).abs() < 2.0, "지연이 누적되면 안 됨: mid={mid} end={end}");
    // 200ppm × 60s ≈ 576샘플(불감대 안에 남는 몫 제외). 정착(5초) 이후에는 한 방향으로만
    // 보정해야 한다(진동 없음). 시작 직후의 목표 재설정분은 제외.
    let s5 = r.stats_at_5s;
    let (dropped, inserted) = (s.samples_dropped - s5.samples_dropped, s.samples_inserted - s5.samples_inserted);
    if ppm > 0.0 {
        assert!(dropped >= 300 && inserted == 0, "5초 이후 버림 {dropped} 삽입 {inserted}: {s:?}");
    } else {
        assert!(inserted >= 300 && dropped == 0, "5초 이후 삽입 {inserted} 버림 {dropped}: {s:?}");
    }
    assert_eq!(s.dropped_frames, 0, "{s:?}");
    assert_eq!(r.silent_chunks, 0, "드리프트 보정 중 끊김 없음");
}

#[test]
fn drift_plus_200ppm_converges() {
    drift_case(200.0, 240);
    drift_case(200.0, 1024);
}

#[test]
fn drift_minus_200ppm_converges() {
    drift_case(-200.0, 240);
    drift_case(-200.0, 1024);
}

#[test]
fn mismatched_frame_sizes() {
    let r = simulate(Sim { codec: CodecKind::Opus, frame: 120, burst: 480, jitter_ms: 5.0, ..Default::default() });
    assert_eq!(r.silent_chunks, 0, "{:?}", r.stats);
    let r = simulate(Sim { codec: CodecKind::Opus, frame: 480, burst: 128, jitter_ms: 5.0, seed: 6, ..Default::default() });
    assert_eq!(r.silent_chunks, 0, "{:?}", r.stats);
}

#[test]
fn late_packets_discarded() {
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    let mut tx = Sender::new(CodecKind::Pcm16, 240, 10, 0);
    let pkts: Vec<_> = (0..20).map(|_| tx.next()).collect();
    let mut out = vec![0f32; 480];
    let mut t = 0u64;
    for (h, p) in pkts.iter().take(10) {
        jb.insert(h, p, t);
        jb.pop(&mut out);
        jb.end_burst(t, t, 0.0);
        t += 5000;
    }
    for _ in 0..5 {
        jb.pop(&mut out);
        jb.end_burst(t, t, 0.0);
        t += 5000;
    }
    let before = jb.stats().late;
    jb.insert(&pkts[0].0, &pkts[0].1, t);
    assert_eq!(jb.stats().late, before + 1);
}

/// P1-6a: 손실된 k의 마감에 k+1이 아직 없으면 언더런 은닉 1프레임만 나가고, k+1이 도착하면 그 은닉을 k의 손실
/// 은닉으로 재해석한다(손실 1건 = 은닉 1프레임, 깊이 증가 없음). k가 손실이 아니라 늦게 오면 언더런 은닉으로 남는다.
#[test]
fn loss_after_underrun_is_concealed_once() {
    // (k를 잃음, k를 k+1과 함께 늦게 보냄)
    for late_k in [false, true] {
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
        let mut tx = Sender::new(CodecKind::Pcm16, 240, 100, 0);
        let pkts: Vec<_> = (0..60).map(|_| tx.next()).collect();
        let mut out = vec![0f32; 480];
        let mut t = 0u64;
        let k = 30;
        let mut depth_before = 0.0;
        let mut s_before = JitterStats::default();
        for (i, (h, p)) in pkts.iter().enumerate() {
            // k와 k+1이 한 틱씩 늦어 k의 마감에는 뒤 패킷이 없다(언더런 은닉). k+1은 k+2와 함께 도착한다.
            if i == k {
                depth_before = jb.depth_error();
                s_before = jb.stats();
            } else if i == k + 1 {
            } else {
                if i == k + 2 {
                    if late_k {
                        jb.insert(&pkts[k].0, &pkts[k].1, t);
                    }
                    jb.insert(&pkts[k + 1].0, &pkts[k + 1].1, t);
                }
                jb.insert(h, p, t);
            }
            jb.pop(&mut out);
            jb.end_burst(t, t, 0.0);
            t += 5000;
        }
        let s = jb.stats();
        assert!(jb.is_playing());
        assert_eq!(s.concealed - s_before.concealed, 1, "late_k {late_k}: {s:?}");
        assert_eq!(s.concealed_loss - s_before.concealed_loss, u64::from(!late_k), "late_k {late_k}: {s:?}");
        if !late_k {
            // 손실이면 타임라인이 그대로라 깊이가 늘지 않는다(이전에는 은닉 2프레임 + 깊이 +1F).
            assert_eq!(s.samples_dropped, s_before.samples_dropped, "{s:?}");
            assert_eq!(s.dropped_frames + s.late_dropped, s_before.dropped_frames + s_before.late_dropped);
            assert!((jb.depth_error() - depth_before).abs() < 1.0, "깊이 {depth_before} → {}", jb.depth_error());
        }
    }
}

/// `underrun_debt` 경계 사례(PR #58 M1)의 결과: 사건 틱부터 끝까지의 통계 변화.
#[derive(Debug)]
struct DebtOut {
    concealed: u64,
    loss: u64,
    late: u64,
    late_dropped: u64,
    dropped_frames: u64,
    samples_dropped: u64,
    rebuffers: u64,
    debt: usize,
    depth_change: f64,
}

/// 5ms 프레임, 틱(5ms)마다 insert → pop 1프레임 → end_burst. 패킷 i는 보통 틱 i에 도착하고, `arrive(i)`가
/// Some(틱)이면 그 틱에, None이면 손실이다. 사건은 틱 `EV`부터. `hook`은 사건 틱마다 pop 직전에 불린다.
const EV: usize = 40;
fn debt_case(codec: CodecKind, seq0: u32, arrive: &dyn Fn(usize) -> Option<usize>, hook: &mut dyn FnMut(usize, &mut JitterBuffer)) -> DebtOut {
    const N: usize = 90;
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    let mut tx = Sender::new(codec, 240, seq0, 0);
    let pkts: Vec<_> = (0..N).map(|_| tx.next()).collect();
    let mut by_tick: Vec<Vec<usize>> = vec![Vec::new(); N + 1];
    for i in 0..N {
        if let Some(t) = arrive(i) {
            by_tick[t.min(N)].push(i);
        }
    }
    let mut out = vec![0f32; 480];
    let (mut s0, mut d0) = (JitterStats::default(), 0.0);
    for (tick, arrivals) in by_tick.iter().enumerate().take(N) {
        if tick == EV {
            s0 = jb.stats();
            d0 = jb.depth_error();
        }
        for &i in arrivals {
            jb.insert(&pkts[i].0, &pkts[i].1, tick as u64 * 5000);
        }
        if tick >= EV {
            hook(tick, &mut jb);
        }
        jb.pop(&mut out);
        jb.end_burst(tick as u64 * 5000, tick as u64 * 5000, 0.0);
        let s = jb.stats();
        assert!(s.concealed_loss <= s.concealed, "{s:?}");
    }
    assert!(jb.is_playing());
    let s = jb.stats();
    DebtOut {
        concealed: s.concealed - s0.concealed,
        loss: s.concealed_loss - s0.concealed_loss,
        late: s.late - s0.late,
        late_dropped: s.late_dropped - s0.late_dropped,
        dropped_frames: s.dropped_frames - s0.dropped_frames,
        samples_dropped: s.samples_dropped - s0.samples_dropped,
        rebuffers: s.rebuffers - s0.rebuffers,
        debt: jb.underrun_debt,
        depth_change: jb.depth_error() - d0,
    }
}

/// PR #58 M1: 언더런 은닉 재해석(`underrun_debt`)의 경계 사례. PCM·Opus × seq 랩어라운드 여부로 같은 결과여야 한다.
/// 마지막 패킷은 마지막 틱에 도착하므로 끝에 꼬리 언더런은 없다.
#[test]
fn underrun_debt_edge_cases() {
    let k = EV;
    for codec in [CodecKind::Pcm16, CodecKind::Opus] {
        // 사건 seq가 u32::MAX → 0을 넘어가도록(랩어라운드) 또는 평범한 시작값.
        for seq0 in [100u32, u32::MAX - k as u32 - 1] {
            let ctx = format!("{codec:?} seq0 {seq0}");
            let none = &mut |_: usize, _: &mut JitterBuffer| {};
            // 1. 버스트: k..k+2 손실, k+3이 1틱 늦음 → 언더런 은닉 3 = 손실 3, 새 은닉·샘플 버림 없음.
            let r = debt_case(codec, seq0, &|i| match i {
                _ if (k..k + 3).contains(&i) => None,
                _ if i == k + 3 => Some(k + 4),
                _ => Some(i),
            }, none);
            assert_eq!((r.concealed, r.loss, r.samples_dropped, r.dropped_frames + r.late_dropped), (3, 3, 0, 0), "버스트 {ctx}: {r:?}");
            assert!(r.depth_change.abs() < 1.0, "버스트 {ctx}: {r:?}");
            // 2. k,k+1 손실, k+2가 2틱 늦음: 타임라인이 이미 3F를 소비했으므로 k+2도 손실로 세고 도착하면 `late`.
            let r = debt_case(codec, seq0, &|i| match i {
                _ if i == k || i == k + 1 => None,
                _ if i == k + 2 => Some(k + 4),
                _ => Some(i),
            }, none);
            assert_eq!((r.concealed, r.loss, r.late, r.samples_dropped), (3, 3, 1, 0), "k+2 늦음 {ctx}: {r:?}");
            // 3. debt > 빈 seq: k 손실, k+1·k+2가 늦게(k+3 틱에 k+3과 함께) 도착. 언더런 2번(debt 2, 빈 seq는 k 하나)
            //    중 1번은 k의 손실로 정산하고, 늦은 k+1을 버리면(H1) 나머지가 정산되어 k+2를 재생한다. 누수 없음.
            let r = debt_case(codec, seq0, &|i| match i {
                _ if i == k => None,
                _ if i == k + 1 || i == k + 2 => Some(k + 3),
                _ => Some(i),
            }, none);
            assert_eq!((r.concealed, r.loss, r.late_dropped, r.debt, r.rebuffers), (2, 1, 1, 0, 0), "debt > 빈 seq {ctx}: {r:?}");
            assert!(r.depth_change.abs() < 1.0, "debt > 빈 seq {ctx}: {r:?}");
            // 4. 재버퍼 전 단절 150ms(30패킷 손실): 은닉 = 손실 = 30, 패킷 단위 버림·샘플 버림 없음(이전: 은닉 58).
            let r = debt_case(codec, seq0, &|i| if (k..k + 30).contains(&i) { None } else { Some(i) }, none);
            assert_eq!((r.concealed, r.loss, r.dropped_frames, r.samples_dropped, r.rebuffers), (30, 30, 0, 0, 0), "150ms 단절 {ctx}: {r:?}");
            assert!(r.depth_change.abs() < 1.0, "150ms 단절 {ctx}: {r:?}");
            // 5. 재정렬: 1틱 뒤바뀜은 버퍼 깊이가 흡수한다. k가 k+1보다 2틱 늦으면 k는 마감에 손실 은닉 1(언더런
            //    아님, debt 0)이고 도착하면 `late`.
            let r = debt_case(codec, seq0, &|i| match i {
                _ if i == k => Some(k + 1),
                _ if i == k + 1 => Some(k),
                _ => Some(i),
            }, none);
            assert_eq!((r.concealed, r.loss, r.late), (0, 0, 0), "재정렬 1틱 {ctx}: {r:?}");
            let r = debt_case(codec, seq0, &|i| if i == k { Some(k + 2) } else { Some(i) }, none);
            assert_eq!((r.concealed, r.loss, r.late, r.debt, r.samples_dropped), (1, 1, 1, 0, 0), "재정렬 2틱 {ctx}: {r:?}");
            // 6. trimming 버림 뒤 남은 debt(PR #58 L1): k가 늦어 언더런 2번(debt 2). k+3 틱에 k·k+2·k+3이 오고
            //    k+1은 손실. 그 틱에 패킷 단위 버림이 걸려 있으면 k를 버린 뒤(debt 1) 빈 k+1은 남은 debt로 정산해야
            //    한다 — 새 손실 은닉 프레임을 만들지 않는다(정산이 맨 앞에만 있으면 은닉 3).
            let r = debt_case(codec, seq0, &|i| match i {
                _ if i == k || i == k + 2 => Some(k + 3),
                _ if i == k + 1 => None,
                _ => Some(i),
            }, &mut |tick, jb| {
                if tick == k + 3 {
                    assert_eq!(jb.underrun_debt, 2);
                    jb.drop_frames_pending = 1;
                }
            });
            assert_eq!((r.concealed, r.loss, r.dropped_frames, r.debt), (2, 1, 1, 0), "trimming 뒤 debt {ctx}: {r:?}");
        }
    }
}

/// P1-6b 늦은 패킷 예산: 꼬리 긴(지수) 지터에서는 늦은 도착이 예산을 넘어 `tail_extra`가 오르고(≤ 1프레임) 은닉이 준다.
/// 균등 지터 + 손실 1%에서는 늦은 도착이 없으므로 0에 머문다(손실 은닉은 예산이 아니라 실제 손실로 센다).
#[test]
fn late_budget_tracks_tail_jitter_only() {
    let run = |exp_jitter: bool, jitter_ms: f64, loss: f64, late_budget_off: bool| {
        simulate(Sim { exp_jitter, jitter_ms, loss, late_budget_off, seconds: 60.0, seed: 7, ..Default::default() })
    };
    let on = run(true, 4.0, 0.0, false);
    let off = run(true, 4.0, 0.0, true);
    println!(
        "지수 지터 4ms: 예산 끔 은닉 {:.2}% 버퍼 {:.2}ms → 켬 은닉 {:.2}% 버퍼 {:.2}ms (tail_extra {:.2}ms)",
        off.conceal_after_5s,
        mean_buffer(&off.trace, 10.0, 60.0),
        on.conceal_after_5s,
        mean_buffer(&on.trace, 10.0, 60.0),
        on.tail_extra_ms
    );
    assert_eq!(off.tail_extra_ms, 0.0);
    assert!(on.tail_extra_ms > 0.0 && on.tail_extra_ms <= 5.0, "지수 지터: tail_extra {:.2}ms", on.tail_extra_ms);
    // 손실 0이면 늦은 패킷 예산 = 은닉 목표 1.3% × 3/4.
    assert!(off.conceal_after_5s > 0.975, "전제: 예산 없이 늦은 은닉이 예산을 넘는 회선 {:.2}%", off.conceal_after_5s);
    assert!(on.conceal_after_5s < off.conceal_after_5s && on.conceal_after_5s <= 0.975 + 0.2, "은닉 {:.2}% → {:.2}%", off.conceal_after_5s, on.conceal_after_5s);
    // 지연 대가는 1프레임 이내.
    assert!(mean_buffer(&on.trace, 10.0, 60.0) - mean_buffer(&off.trace, 10.0, 60.0) <= 5.0);
    let u = run(false, 3.0, 0.01, false);
    assert_eq!(u.tail_extra_ms, 0.0, "균등 지터 + 손실 1%: tail_extra {:.2}ms", u.tail_extra_ms);
    assert!(u.conceal_after_5s < 1.5, "균등 지터 + 손실 1%: 은닉 {:.2}%", u.conceal_after_5s);
}

/// P1-6c(PR #65 리뷰 M1·M2): 예산은 손실의 연속 함수다. 손실 < 0.975%는 P1-6b와 같고(손실 1% → 0.325%), 그 위는
/// 0.4%까지 선형으로 풀리며, 어디서도 하네스 GOAL 밖 회선 기준의 손실 너머 몫(0.5%p)에 닿지 않는다.
#[test]
fn late_budget_is_continuous_in_loss() {
    let lb = JitterBuffer::late_budget;
    for &target in &[0.030, 0.013, 0.006, JitterTuning::from_profile(0.25).conceal_target, JitterTuning::from_profile(0.75).conceal_target] {
        let mut prev = lb(target, 0.0);
        assert!((prev - target * 0.75).abs() < 1e-12);
        for k in 1..=10_000 {
            let r = k as f64 * 1e-5; // 0 ~ 10%, 0.001%p 간격
            let b = lb(target, r);
            // 기울기 ≤ 1(이음매에 계단 없음): 손실 0.001%p 변화에 예산도 그 이상 움직이지 않는다.
            assert!((b - prev).abs() <= 1e-5 + 1e-12, "target {target} r {r}: {prev} → {b}");
            assert!(b >= target / 4.0 - 1e-12 && b <= target * 0.75 + 1e-12);
            if r >= target * 0.75 + 1e-5 {
                // 손실이 목표에 가까워지면 예산은 줄지 않고(역전 없음), 균형 이하 프로필에서도 0.4%를 넘지 않는다.
                assert!(b >= prev - 1e-12, "target {target} r {r}: 손실이 늘었는데 예산이 줄었다");
                assert!(b <= LATE_BUDGET_LOSSY_MAX.max(target / 4.0) + 1e-12);
            }
            prev = b;
        }
    }
    // 균형: 손실 1%·1.2% → 0.325%(이전과 같음), 1.4% → 0.337%(첫 커밋 0.975%), 1.95% 이상 → 0.4%.
    let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
    assert!(near(lb(0.013, 0.010), 0.00325) && near(lb(0.013, 0.012), 0.00325));
    assert!(lb(0.013, 0.014) - lb(0.013, 0.012) < 0.0002);
    assert!(near(lb(0.013, 0.02), 0.004) && near(lb(0.013, 0.05), 0.004));
    // 저지연(목표 3%)은 풀지 않는다(하한 0.75%가 이미 0.4%보다 큼).
    assert!(near(lb(0.03, 0.05), 0.0075));
}

/// 시드 여러 개 평균(PR #65 리뷰 L4: 단일 시드 단언은 다음 엔진 변경에서 의미 없이 깨질 수 있다).
struct TailRun {
    tail_ms: f64,
    conceal: f64,
    net_loss: f64,
    adjusts: f64,
}

fn tail_runs(jitter_ms: f64, loss: f64) -> TailRun {
    let seeds = [7u64, 11, 23];
    let rs: Vec<SimResult> =
        seeds.iter().map(|&seed| simulate(Sim { exp_jitter: true, jitter_ms, loss, seconds: 60.0, seed, ..Default::default() })).collect();
    let mean = |f: &dyn Fn(&SimResult) -> f64| rs.iter().map(f).sum::<f64>() / rs.len() as f64;
    TailRun {
        tail_ms: mean(&|r| r.tail_extra_ms),
        conceal: mean(&|r| r.conceal_after_5s),
        net_loss: mean(&|r| r.net_loss_pct),
        adjusts: mean(&|r| r.stats.tail_adjusts as f64),
    }
}

/// 진단용 격자(지수 지터 2/3/4ms × 손실 0~3%, 시드 3개 평균). 예산 정책을 바꿀 때 PR 본문 표를 만드는 데 쓴다.
/// `cargo test --lib tail_matrix_diag -- --ignored --nocapture`
#[test]
#[ignore]
fn tail_matrix_diag() {
    for j in [2.0, 3.0, 4.0] {
        for l in [0.0, 0.005, 0.01, 0.012, 0.014, 0.02, 0.03] {
            let r = tail_runs(j, l);
            println!(
                "지수 지터 {j}ms 손실 {:.1}%: tail {:.2}ms 은닉 {:.2}% 망 손실 {:.2}% (은닉 − 손실 {:+.2}%p) 조정 {:.1}회",
                l * 100.0,
                r.tail_ms,
                r.conceal,
                r.net_loss,
                r.conceal - r.net_loss,
                r.adjusts
            );
        }
    }
}

/// P1-6c(PR #63 리뷰 L1, PR #65 리뷰 M1·M2·L4): 손실이 목표를 넘는 꼬리 지터 회선. 시드 3개 평균.
/// - 문턱(균형 1.3%) 양쪽 손실 1.2% / 1.4%에서 `tail_extra`가 격자 2칸(0.5ms) 안이고 조정 횟수도 비슷하다. #65 첫 커밋(계단
///   0.325% → 0.975%)은 지수 지터 3ms에서 2.42 / 1.58ms, 4ms에서 4.83 / 3.83ms로 갈렸다.
/// - 1F 상한에 닿지 않는 꼬리(지수 3ms)에서 손실 ≥ 1.4%의 은닉은 하네스 GOAL 밖 회선 기준(실제 손실 + 0.5%p) 안이다.
///   첫 커밋은 손실 2%에서 +0.53%p였다.
/// - 손실 2%는 손실 1%보다 여유를 더 쓰지 않는다(예산 0.4% ≥ 0.325%, 격자 1칸 허용).
/// - 지수 4ms는 `tail_extra`가 1F 상한(5ms)에 닿아 예산과 무관하게 늦은 패킷이 남는다(손실 0%에서도 은닉 0.74%). 이 영역의
///   은닉 초과는 GOAL 1절 지연 상한과의 교환이므로 여기서는 상한만 확인한다(PR #65 본문 M2).
#[test]
fn late_budget_relaxes_when_loss_exceeds_target() {
    for j in [3.0, 4.0] {
        let (a, b) = (tail_runs(j, 0.012), tail_runs(j, 0.014));
        println!("지수 지터 {j}ms 손실 1.2% / 1.4%: tail {:.2} / {:.2}ms, 조정 {:.1} / {:.1}회", a.tail_ms, b.tail_ms, a.adjusts, b.adjusts);
        assert!((a.tail_ms - b.tail_ms).abs() <= 0.5 + 1e-9, "지터 {j}ms: tail {:.2} vs {:.2}", a.tail_ms, b.tail_ms);
        assert!((a.adjusts - b.adjusts).abs() <= 2.0, "지터 {j}ms: 조정 {:.1} vs {:.1}", a.adjusts, b.adjusts);
    }
    let l1 = tail_runs(3.0, 0.01);
    for loss in [0.014, 0.02, 0.03] {
        let r = tail_runs(3.0, loss);
        println!("지수 지터 3ms 손실 {:.1}%: tail {:.2}ms 은닉 {:.2}% = 손실 {:.2}% {:+.2}%p", loss * 100.0, r.tail_ms, r.conceal, r.net_loss, r.conceal - r.net_loss);
        assert!(r.conceal <= r.net_loss + 0.5, "손실 {loss}: 은닉 {:.2}% > 실제 손실 {:.2}% + 0.5%p", r.conceal, r.net_loss);
        if loss == 0.02 {
            assert!(r.tail_ms <= l1.tail_ms + 0.25 + 1e-9, "손실 2% tail {:.2} vs 1% {:.2}", r.tail_ms, l1.tail_ms);
        }
    }
    let r = tail_runs(4.0, 0.02);
    assert!(r.tail_ms <= 5.0 + 1e-9, "1F 상한: {:.2}ms", r.tail_ms);
}

/// P1-6c(PR #63 리뷰 L4): 늦은 패킷 예산의 분모(도착 패킷)는 재생 중에만 센다.
#[test]
fn late_budget_counts_packets_only_while_playing() {
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    jb.set_consumer_lead(72.0);
    let mut tx = Sender::new(CodecKind::Pcm16, 240, 1, 0);
    let mut out = vec![0f32; 240 * 2];
    let mut t = 0u64;
    let (h, pk) = tx.next();
    jb.insert(&h, &pk, t);
    assert!(!jb.is_playing());
    assert_eq!(jb.pkts_block, 0, "버퍼링 중 도착은 분모에 넣지 않는다");
    while !jb.is_playing() {
        t += 5000;
        let (h, pk) = tx.next();
        jb.insert(&h, &pk, t);
        jb.pop(&mut out);
        jb.end_burst(t, t, 0.0);
        assert!(t < 2_000_000, "재생 시작 안 됨");
    }
    let before = jb.pkts_block;
    t += 5000;
    let (h, pk) = tx.next();
    jb.insert(&h, &pk, t);
    assert_eq!(jb.pkts_block, before + 1, "재생 중 도착은 센다");
}

/// P1-6c(PR #63 리뷰 L2·L3): 경로 변경·스트림 리셋·프로필 변경은 늦은 패킷 예산을 초기화하고(프로필은 손실 링을 남김),
/// 재생 시작은 처음 `LATE_SKIP_BLOCKS`개 블록을 판단에서 뺀다.
#[test]
fn late_budget_resets_on_path_stream_profile_and_skips_start() {
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    let dirty = |jb: &mut JitterBuffer| {
        jb.tail_extra = 96.0;
        jb.tail_fresh = 3;
        jb.late_rates = [0.01; LATE_BLOCKS];
        jb.loss_rates = [0.02; LATE_BLOCKS];
        jb.loss_count = LATE_BLOCKS;
        jb.late_block = 5;
        jb.pkts_block = 100;
    };
    dirty(&mut jb);
    jb.reset_delay_stats(0);
    assert_eq!((jb.tail_extra, jb.tail_fresh, jb.loss_count, jb.pkts_block, jb.late_block), (0.0, 0, 0, 0, 0), "경로 변경");
    // 경로 변경 뒤에도 수렴 구간은 판단에서 뺀다(PR #65 리뷰 L1).
    assert_eq!(jb.tail_skip, LATE_SKIP_BLOCKS);
    assert_eq!(jb.stats().tail_resets, 1);
    dirty(&mut jb);
    jb.reset_stream();
    assert_eq!((jb.tail_extra, jb.loss_count), (0.0, 0), "스트림 리셋");
    dirty(&mut jb);
    jb.set_profile(1.0);
    assert_eq!(jb.tail_extra, 0.0, "큰 프로필 변경(목표 1.3% → 0.6%)");
    assert_eq!(jb.late_rates, [0.0; LATE_BLOCKS]);
    assert_eq!(jb.loss_count, LATE_BLOCKS, "프로필 변경은 손실 링을 남긴다");
    assert_eq!(jb.stats().tail_resets, 3);
    // 같은 프로필이면 아무것도 바꾸지 않는다.
    dirty(&mut jb);
    jb.set_profile(1.0);
    assert_eq!(jb.tail_extra, 96.0);
    assert_eq!(jb.late_rates, [0.01; LATE_BLOCKS]);
    // 작은 프로필 변경(1.0 → 0.9, 목표 0.6% → 0.74%)은 늦은 비율만 비우고 `tail_extra`는 남긴다(PR #65 리뷰 L2).
    jb.set_profile(0.9);
    assert_eq!((jb.tail_extra, jb.tail_fresh), (96.0, 0));
    assert_eq!(jb.late_rates, [0.0; LATE_BLOCKS]);
    assert_eq!(jb.stats().tail_resets, 3);

    // 재생 시작 → 처음 블록 2개는 건너뛰고 세 번째부터 센다(재생 중 도착만 분모에 들어감).
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    jb.set_consumer_lead(72.0);
    let mut tx = Sender::new(CodecKind::Pcm16, 240, 1, 0);
    let mut out = vec![0f32; 240 * 2];
    let mut t = 0u64;
    while !jb.is_playing() {
        let (h, pk) = tx.next();
        jb.insert(&h, &pk, t);
        jb.pop(&mut out);
        jb.end_burst(t, t, 0.0);
        t += 5000;
        assert!(t < 2_000_000, "재생 시작 안 됨");
    }
    assert_eq!(jb.tail_skip, LATE_SKIP_BLOCKS);
    assert_eq!(jb.loss_count, 0);
    while t < 6_000_000 {
        let (h, pk) = tx.next();
        jb.insert(&h, &pk, t);
        jb.pop(&mut out);
        jb.end_burst(t, t, 0.0);
        t += 5000;
    }
    assert_eq!(jb.tail_skip, 0);
    // 시작 뒤 약 6초 = 블록 5~6개 중 2개는 건너뜀.
    assert!((3..=4).contains(&jb.loss_count), "센 블록 {}", jb.loss_count);
}

#[test]
fn replayed_old_packet_cannot_rewind_buffering() {
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    let mut tx = Sender::new(CodecKind::Pcm16, 240, 1000, 0);
    let pkts: Vec<_> = (0..300).map(|_| tx.next()).collect();
    jb.insert(&pkts[250].0, &pkts[250].1, 0);
    // 200개 이전 패킷 재전송 → 버퍼링 중이라도 시작점을 크게 되돌리지 않음
    jb.insert(&pkts[50].0, &pkts[50].1, 10);
    assert_eq!(jb.stats().late, 1);
}

#[test]
fn pcm_concealment_fades_then_silence() {
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    let mut tx = Sender::new(CodecKind::Pcm16, 240, 0, 0);
    let mut out = vec![0f32; 480];
    let mut t = 0u64;
    for _ in 0..20 {
        let (h, p) = tx.next();
        jb.insert(&h, &p, t);
        jb.pop(&mut out);
        jb.end_burst(t, t, 0.0);
        t += 5000;
    }
    let lost: Vec<_> = (0..3).map(|_| tx.next()).collect();
    drop(lost);
    let (h, p) = tx.next();
    jb.insert(&h, &p, t);
    let c0 = jb.stats().concealed;
    let mut energies = vec![];
    for _ in 0..6 {
        jb.pop(&mut out);
        jb.end_burst(t, t, 0.0);
        energies.push(out.iter().map(|x| x * x).sum::<f32>());
        t += 5000;
        let (h, p) = tx.next();
        jb.insert(&h, &p, t);
    }
    assert!(jb.stats().concealed >= c0 + 3);
    assert!(energies.iter().any(|&e| e < 1e-9), "페이드 1회 후 무음 프레임이 있어야 함: {energies:?}");
}

#[test]
fn backlog_is_trimmed_with_crossfade() {
    // 1초 단절 후 몰려온 패킷(와이파이 버스트) → 패킷 단위 버림 + 크로스페이드, 클릭 없음
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    let mut tx = Sender::new(CodecKind::Opus, 240, 0, 0);
    let mut out = vec![0f32; 128 * 2];
    let mut t = 0u64;
    let mut prev = None::<f32>;
    let mut max_step = 0f32;
    let mut held: Vec<(Header, Vec<u8>)> = Vec::new();
    let mut send_t = 0u64;
    let mut burst_t = 0u64;
    while t < 6_000_000 {
        while send_t <= t {
            let pkt = tx.next();
            if (2_000_000..2_100_000).contains(&send_t) {
                held.push(pkt); // 100ms 동안 지연
            } else {
                if send_t >= 2_100_000 && !held.is_empty() {
                    for (h, p) in held.drain(..) {
                        jb.insert(&h, &p, send_t + 3000);
                    }
                }
                jb.insert(&pkt.0, &pkt.1, send_t + 3000);
            }
            send_t += 5000;
        }
        if t >= burst_t {
            jb.pop(&mut out);
            jb.end_burst(t, t, 0.0);
            if t > 1_000_000 {
                for fr in out.as_chunks::<2>().0 {
                    if let Some(p) = prev {
                        max_step = max_step.max((fr[0] - p).abs());
                    }
                    prev = Some(fr[0]);
                }
            }
            burst_t += 128 * 1_000_000 / 48_000;
        }
        t += 500;
    }
    let s = jb.stats();
    println!("backlog: {s:?}, max step {max_step}");
    assert!(s.dropped_frames > 0, "쌓인 지연은 패킷 단위로 줄여야 함: {s:?}");
    assert!(s.buffer_ms < s.target_ms + 3.0, "복귀 후 목표 근처: {s:?}");
}

#[test]
fn profile_tuning_and_runtime_change() {
    // 균형(0.5)이 기본 계수: p95, 유지 2초. 저지연 → 안정으로 갈수록 분위수·유지 시간·긴 창이 커진다.
    let (lo, mid, hi) = (JitterTuning::from_profile(0.0), JitterTuning::from_profile(0.5), JitterTuning::from_profile(1.0));
    assert!((mid.quantile - 0.95).abs() < 1e-9 && (mid.hold_ms - 2000.0).abs() < 1e-9 && mid.pad_frames == 0.0);
    assert!(lo.quantile < mid.quantile && mid.quantile < hi.quantile);
    assert!(lo.hold_ms < mid.hold_ms && mid.hold_ms < hi.hold_ms);
    assert!(lo.long_blocks < mid.long_blocks && mid.long_blocks < hi.long_blocks);
    assert!(lo.margin_scale < mid.margin_scale && mid.margin_scale < hi.margin_scale);
    // P1-6b 은닉 목표: 저지연 3% · 균형 1.3% · 안정 0.6%(안정 쪽일수록 늦은 패킷 예산이 작아 깊이를 더 쓴다).
    assert!((lo.conceal_target - 0.03).abs() < 1e-9 && (mid.conceal_target - 0.013).abs() < 1e-9 && (hi.conceal_target - 0.006).abs() < 1e-9);
    assert_eq!(JitterTuning::from_profile(f32::NAN), mid, "잘못된 값은 균형");
    // 실행 중 변경: 지터 U(0, 10ms) 스트림에서 안정으로 바꾸면 목표가 바로 오르고, 저지연으로 바꾸면 내려간다.
    let mut rng = rand::rngs::StdRng::seed_from_u64(9);
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    let mut tx = Sender::new(CodecKind::Pcm16, 240, 0, 0);
    for k in 0..400u64 {
        let (h, p) = tx.next();
        jb.insert(&h, &p, k * 5000 + 3000 + rng.gen_range(0..10_000));
    }
    let bal = jb.target_samples();
    jb.set_profile(1.0);
    let stable = jb.target_samples();
    jb.set_profile(1.0);
    assert_eq!(jb.target_samples(), stable, "같은 값이면 변화 없음");
    // 저지연으로 내려도 짧은 창 추정은 hold를 거친다(M3): 바로 내려가는 것은 pad·긴 창 몫뿐이다.
    jb.set_profile(0.0);
    let low_now = jb.target_samples();
    assert!(low_now < stable, "pad 등은 바로 빠짐: {low_now} vs {stable}");
    let (h, p) = tx.next();
    jb.insert(&h, &p, 400 * 5000 + 3000);
    assert!(jb.target_samples() >= low_now - 0.5, "hold 전에는 짧은 창 추정이 내려가지 않음");
    for k in 401..1000u64 {
        let (h, p) = tx.next();
        jb.insert(&h, &p, k * 5000 + 3000 + rng.gen_range(0..10_000));
    }
    let low = jb.target_samples();
    assert!(low < bal && bal < stable, "목표 저지연 {low} < 균형 {bal} < 안정 {stable}");
}

#[test]
fn path_step_down_resets_delay_stats() {
    // 릴레이 → 직접 승격처럼 지연이 18ms 한 번에 줄면 옛 경로 표본을 버리고 목표가 바로 새 경로 기준으로 내려간다.
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    jb.set_consumer_lead(72.0);
    let mut tx = Sender::new(CodecKind::Pcm16, 240, 0, 0);
    let mut rng = rand::rngs::StdRng::seed_from_u64(3);
    for k in 0..600u64 {
        let (h, p) = tx.next();
        // 전환 직후 20ms 동안 옛 경로로 보낸 패킷 4개가 새 경로 패킷 사이에 섞여 도착한다.
        let delay = if k < 400 || (402..406).contains(&k) { 21_000 } else { 3000 } + rng.gen_range(0..500);
        jb.insert(&h, &p, k * 5000 + delay);
        if k == 399 {
            assert!(jb.stats().target_ms < 6.0, "전제: 안정 회선 목표 {:?}", jb.stats());
        }
    }
    assert!(jb.stats().target_ms < 6.0, "승격 1초 뒤 목표가 옛 경로 지연(18ms)을 끌지 않음: {:?}", jb.stats());
}

#[test]
fn target_change_converges_within_rate_bound() {
    // PR #20: 보정 속도 상한(0.25%)의 짝 — 목표가 5ms 바뀌면 깊이가 `correction_catch_up_secs` × 1.2 + 1프레임 안에
    // 새 목표의 해제 문턱 안으로 들어와야 한다(실시간 짝: tests/loopback.rs link_target_change_converges).
    for frame in [120usize, 240] {
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
        jb.set_consumer_lead(72.0);
        let mut tx = Sender::new(CodecKind::Pcm16, frame, 0, 0);
        let (exit_drop, exit_ins) = correction_exit_ms(frame);
        let mut out = vec![0f32; frame * 2];
        let period = frame as u64 * 1_000_000 / 48_000;
        let mut t = 0u64;
        let mut results = Vec::new();
        for (at, lead) in [(5_000_000u64, 72.0 + 240.0), (15_000_000, 72.0)] {
            let mut changed_at = None;
            let mut conv = None;
            while t < at + 9_000_000 {
                let (h, p) = tx.next();
                jb.insert(&h, &p, t + 3000);
                jb.pop(&mut out);
                jb.end_burst(t, t, 0.0);
                if t >= at && changed_at.is_none() {
                    jb.set_consumer_lead(lead);
                    changed_at = Some(t);
                }
                if let (Some(c), None) = (changed_at, conv) {
                    let s = jb.stats();
                    let err = (s.buffer_ms - s.target_ms) as f64;
                    // 반 샘플(0.0104ms) 허용: 해제 문턱 경계의 부동소수 비교
                    if t > c && err <= exit_drop + 0.0105 && err >= -exit_ins - 0.0105 {
                        conv = Some((t - c) as f64 / 1e6);
                    }
                }
                t += period;
            }
            let conv = conv.expect("수렴하지 않음");
            let bound = correction_catch_up_secs(240.0) * 1.2 + frame as f64 / 48_000.0;
            println!("목표 변경 F {frame} lead {lead}: 수렴 {conv:.2}s (상한 {bound:.2}s, 이론 {:.2}s)", correction_catch_up_secs(240.0));
            assert!(conv <= bound, "F {frame}: 수렴 {conv:.2}s > 상한 {bound:.2}s");
            results.push(conv);
        }
        assert_eq!(jb.stats().concealed, 0);
    }
}

#[test]
fn rate_shifter_caps_pitch_and_is_continuous() {
    // 버림 1 + r로 1초(200패킷 × 240) 램프: 버린 샘플 = r × 48000 ± 1, 패킷 경계 포함 연속(인접 차가 원 신호 수준).
    let mut rs = RateShifter::default();
    let f = 240;
    let mut dst = vec![0f32; (f + 4) * 2];
    let (mut out_total, mut prev) = (0usize, None::<f32>);
    let mut max_step = 0f32;
    for k in 0..200 {
        let src: Vec<f32> = (0..f).map(|i| ((k * f + i) as f32 * 0.01).sin()).collect();
        let n = rs.process(&src, 1, f, &mut dst, 1.0 + MAX_CORR_RATE);
        for &y in &dst[..n] {
            if let Some(p) = prev {
                max_step = max_step.max((y - p).abs());
            }
            prev = Some(y);
        }
        out_total += n;
    }
    let dropped = 200 * f - out_total;
    assert!((dropped as f64 - MAX_CORR_RATE * 48_000.0).abs() <= 2.0, "버린 샘플 {dropped}");
    assert!(max_step < 0.0102, "패킷 경계 불연속 없음: {max_step}");
    assert!((1200.0 * (1.0 + MAX_CORR_RATE).log2() - 4.33).abs() < 0.01, "상한 약 4.3센트");
}

#[test]
fn shift_one_lengths_and_continuity() {
    let src: Vec<f32> = (0..240).map(|i| i as f32).collect();
    let mut dst = vec![0f32; 241];
    shift_one(&src, 1, 240, &mut dst, true);
    assert_eq!(dst[0], 0.0);
    assert!((dst[238] - 239.0).abs() < 1e-3);
    for w in dst[..239].windows(2) {
        // 프레임 전체 램프: 간격이 1 + 1/238 근처로 고르게
        assert!((w[1] - w[0] - 1.0).abs() < 0.01, "{}", w[1] - w[0]);
    }
    shift_one(&src, 1, 240, &mut dst, false);
    assert_eq!(dst[0], 0.0);
    assert!((dst[240] - 239.0).abs() < 1e-3);
    for w in dst.windows(2) {
        assert!((w[1] - w[0] - 1.0).abs() < 0.01);
    }
}
