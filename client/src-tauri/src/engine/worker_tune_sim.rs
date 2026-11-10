//! 지터 버퍼 자동 튜닝(P1-5) 결정적 시나리오: 피어 여럿(최대 6) → 지터 버퍼 → PeerFeed(워커) → OutputRt(콜백).
//!
//! `worker::tests::run_feed_sim`과 같은 이벤트 모델이다(가상 시간, 결정적 의사난수). 피어마다 송신 장치 클럭(드리프트),
//! 망 지터, 손실, 지연 스파이크(와이파이 버스트)를 따로 준다. 지표는 구현과 독립적으로 잰다:
//! - `p95`: 실제 도착 지연(transit = 도착 − ts)의 1초 블록별 p95(블록 최솟값 기준)의 중앙값. GOAL 1절 "p95 지터".
//! - `bufferMs`: 엔진이 보고하는 `PeerStats.bufferMs`(지터 버퍼 깊이, 소스 링 포함)를 100ms마다 표본.
//! - 은닉 비율: 정착 후 구간의 은닉 프레임 / 생산 프레임(손실 포함, GOAL 2절).

use super::*;
use crate::engine::codec::Encoder as SimEncoder;
use crate::engine::config::{frame_code_from_samples, CodecKind, MAX_PAYLOAD_BYTES};
use crate::engine::jitter::{JitterBuffer, JitterSettings, JitterStats};
use crate::engine::rt::{output_path, SLOT_ACTIVE};
use crate::net::packet::{Header, PacketType};
use rand::{Rng, SeedableRng};

#[derive(Clone)]
pub(super) struct NetPeer {
    pub in_buf: usize,
    pub frame: usize,
    /// 송신 장치 클럭 편차(ppm, +면 송신이 빠름 → 수신 버퍼가 차오름).
    pub drift_ppm: f64,
    /// 망 지연 지터 U(0..이 값) µs(기본 망 지연 0.2ms에 더함).
    pub net_jitter_us: u64,
    pub loss: f64,
    /// (시작 µs, 길이 µs, 추가 지연 µs): 이 구간에 보낸 패킷은 그만큼 늦게 도착한다. 끝나면 밀린 패킷과 새 패킷이 몰려온다.
    pub spikes: Vec<(u64, u64, u64)>,
    pub profile: f32,
    pub seed: u64,
}

impl NetPeer {
    pub fn new(frame: usize, seed: u64) -> Self {
        Self { in_buf: 128, frame, drift_ppm: 0.0, net_jitter_us: 0, loss: 0.0, spikes: Vec::new(), profile: 0.5, seed }
    }
}

pub(super) struct MultiSim {
    pub out_buf: usize,
    pub out_phase_us: u64,
    pub seconds: f64,
    pub worker_jitter_us: u64,
    pub out_early_us: u64,
    pub dev_late_us: u64,
    /// (주기 µs, 멈춤 µs, 끝 µs)
    pub out_stall: Option<(u64, u64, u64)>,
    /// 지표를 재기 시작하는 시각(µs).
    pub settle_us: u64,
    pub seed: u64,
    pub peers: Vec<NetPeer>,
}

impl MultiSim {
    pub fn new(seconds: f64, peers: Vec<NetPeer>) -> Self {
        Self {
            out_buf: 128,
            out_phase_us: 700,
            seconds,
            worker_jitter_us: 300,
            out_early_us: 0,
            dev_late_us: 500,
            out_stall: None,
            settle_us: 5_000_000,
            seed: 1,
            peers,
        }
    }
}

#[derive(Default, Clone)]
pub(super) struct PeerOut {
    pub js: JitterStats,
    /// 정착 시점 통계.
    pub js_settle: JitterStats,
    /// (s, bufferMs, targetMs) 100ms마다(재생 중일 때만).
    pub trace: Vec<(f64, f32, f32)>,
    /// 은닉 이벤트 시각(µs).
    pub conceal_us: Vec<u64>,
    /// 1초 블록별 실제 도착 지연 p95(ms, 블록 최솟값 기준).
    pub block_p95: Vec<(f64, f32)>,
    pub margin: usize,
}

impl PeerOut {
    fn in_range(&self, a: f64, b: f64) -> Vec<f32> {
        self.trace.iter().filter(|x| x.0 >= a && x.0 < b).map(|x| x.1).collect()
    }
    /// 구간 [a, b) 초의 bufferMs 중앙값.
    pub fn median_buffer(&self, a: f64, b: f64) -> f32 {
        median(self.in_range(a, b))
    }
    pub fn median_p95(&self, a: f64, b: f64) -> f32 {
        median(self.block_p95.iter().filter(|x| x.0 >= a && x.0 < b).map(|x| x.1).collect())
    }
    /// 정착 후 은닉 비율(%).
    pub fn conceal_pct(&self) -> f64 {
        let c = self.js.concealed - self.js_settle.concealed;
        let p = self.js.produced - self.js_settle.produced;
        c as f64 * 100.0 / p.max(1) as f64
    }
    pub fn corrections(&self) -> u64 {
        self.js.corrections() - self.js_settle.corrections()
    }
    /// 시각 `t`(s) 직전 표본의 목표.
    pub fn target_at(&self, t: f64) -> f32 {
        self.trace.iter().rev().find(|x| x.0 <= t).map(|x| x.2).unwrap_or(0.0)
    }
    /// `from` 이후 처음으로 목표가 `level` 이상이 된 시각(s).
    pub fn first_target_at_least(&self, from: f64, level: f32) -> Option<f64> {
        self.trace.iter().find(|x| x.0 >= from && x.2 >= level).map(|x| x.0)
    }
    /// [from, to) 구간에서 bufferMs가 `level`을 넘은 마지막 표본 시각(s). 없으면 None.
    pub fn last_buffer_above(&self, from: f64, to: f64, level: f32) -> Option<f64> {
        self.trace.iter().filter(|x| x.0 >= from && x.0 < to && x.1 > level).map(|x| x.0).next_back()
    }
    pub fn conceal_between(&self, a_us: u64, b_us: u64) -> usize {
        self.conceal_us.iter().filter(|&&c| c >= a_us && c < b_us).count()
    }
}

pub(super) fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

pub(super) struct MultiOut {
    pub peers: Vec<PeerOut>,
    pub xruns: u64,
    pub xrun_us: Vec<u64>,
}

struct TxState {
    enc: SimEncoder,
    rng: rand::rngs::StdRng,
    seq: u32,
    ts: u32,
    acc: usize,
    phase: f64,
    k_in: u64,
    period_us: f64,
    fcode: u8,
    /// 현재 1초 블록의 transit(µs) 목록과 블록 번호.
    block: Vec<f64>,
    block_no: u64,
}

pub(super) fn run_multi(p: &MultiSim) -> MultiOut {
    const RATE: f64 = 48_000.0;
    let n = p.peers.len();
    assert!((1..=6).contains(&n));
    let mut rng = rand::rngs::StdRng::seed_from_u64(p.seed);
    let counters = Arc::new(AudioCounters::default());
    let controls = Arc::new(MixControls::default());
    let (mut rt, mut feed) = output_path(counters.clone(), controls);
    let mut pfs = Vec::new();
    let mut jbs = Vec::new();
    let mut txs = Vec::new();
    for (i, np) in p.peers.iter().enumerate() {
        let slot = 1 + i;
        let sh = &feed.bank.slots[slot];
        sh.set_gains((0.15, 0.15), (0.15, 0.15));
        sh.state.store(SLOT_ACTIVE, Ordering::Release);
        pfs.push(PeerFeed::new(slot, (i, i as u32), 48_000, sh).unwrap());
        jbs.push(JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: np.profile }));
        txs.push(TxState {
            enc: SimEncoder::new(CodecKind::Pcm16, 1, 0).unwrap(),
            rng: rand::rngs::StdRng::seed_from_u64(np.seed),
            seq: 7 + i as u32 * 1000,
            ts: 1000 + i as u32 * 77_777,
            acc: 0,
            phase: 0.0,
            k_in: 1,
            period_us: np.in_buf as f64 / RATE * 1e6 / (1.0 + np.drift_ppm * 1e-6),
            fcode: frame_code_from_samples(np.frame).unwrap(),
            block: Vec::new(),
            block_no: 0,
        });
    }
    let mut tracker = CallbackTracker::new(48_000);
    let mut chunk = vec![0f32; OUT_CHUNK * 2];
    let mut tmp = vec![0f32; 8192];
    let mut dev = vec![0f32; p.out_buf * 2];
    let mut payload = vec![0u8; MAX_PAYLOAD_BYTES];
    let mut pending: Vec<(u64, usize, Header, Vec<u8>)> = Vec::new();
    let out_period = p.out_buf as f64 / RATE * 1e6;
    let mut k_out = 0u64;
    let mut t_w = 0u64;
    let end_us = (p.seconds * 1e6) as u64;
    let mut outs: Vec<PeerOut> = vec![PeerOut::default(); n];
    let mut seen_conc = vec![0u64; n];
    let mut settled = false;
    let mut next_trace = 0u64;
    let mut xrun_us = Vec::new();
    let mut seen_x = 0u64;
    let jit = |k: u64, salt: u64, max: u64| -> u64 {
        if max == 0 {
            return 0;
        }
        let h = (k ^ p.seed ^ salt).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        (h >> 11) % max
    };
    let out_due = |k: u64| -> u64 {
        let early = jit(k, 0, p.out_early_us.min(out_period as u64 * 9 / 10));
        let late = jit(k, 0x5A5A_0000_0000, p.dev_late_us);
        let nominal = (p.out_phase_us + (k as f64 * out_period) as u64).saturating_sub(early) + late;
        match p.out_stall {
            Some((every, len, until)) if nominal % every < len && nominal > every && nominal < until => nominal - nominal % every + len,
            _ => nominal,
        }
    };
    loop {
        let t_ins: Vec<u64> = txs
            .iter()
            .enumerate()
            .map(|(i, tx)| (tx.k_in as f64 * tx.period_us) as u64 + jit(tx.k_in, 0xA5A5_0000_0000 + i as u64, p.dev_late_us.min(tx.period_us as u64 * 9 / 10)))
            .collect();
        let (pi, t_in) = t_ins.iter().copied().enumerate().min_by_key(|x| x.1).unwrap();
        let t_out = out_due(k_out);
        let t_arr = pending.iter().map(|x| x.0).min().unwrap_or(u64::MAX);
        let t = t_in.min(t_out).min(t_arr).min(t_w);
        if t > end_us {
            break;
        }
        if t == t_arr {
            let i = pending.iter().position(|x| x.0 == t).unwrap();
            let (_, peer, h, pl) = pending.swap_remove(i);
            jbs[peer].insert(&h, &pl, t);
            // 실제 도착 지연(µs): 도착 − ts를 수신 클럭으로 환산(송신 클럭 편차는 블록 최솟값이 흡수)
            let tx = &mut txs[peer];
            let blk = t / 1_000_000;
            if blk != tx.block_no {
                if tx.block.len() > 10 && blk > 0 {
                    let mn = tx.block.iter().cloned().fold(f64::INFINITY, f64::min);
                    let mut rel: Vec<f32> = tx.block.iter().map(|x| ((x - mn) / 1000.0) as f32).collect();
                    rel.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let k = ((rel.len() as f64 * 0.95).ceil() as usize).clamp(1, rel.len()) - 1;
                    outs[peer].block_p95.push((tx.block_no as f64, rel[k]));
                }
                tx.block.clear();
                tx.block_no = blk;
            }
            let ts_us = h.timestamp.wrapping_sub(1000 + peer as u32 * 77_777) as f64 / RATE * 1e6 / (1.0 + p.peers[peer].drift_ppm * 1e-6);
            tx.block.push(t as f64 - ts_us);
        } else if t == t_in {
            let np = &p.peers[pi];
            let tx = &mut txs[pi];
            tx.acc += np.in_buf;
            while tx.acc >= np.frame {
                let f0 = 330.0 + 70.0 * pi as f64;
                let pcm: Vec<f32> = (0..np.frame)
                    .map(|_| {
                        let v = (tx.phase * 2.0 * std::f64::consts::PI).sin() as f32 * 0.5;
                        tx.phase = (tx.phase + f0 / RATE).fract();
                        v
                    })
                    .collect();
                let nb = tx.enc.encode(&pcm, &mut payload).unwrap();
                let h = Header { ptype: PacketType::Audio, ssrc: 1 + pi as u32, seq: tx.seq, timestamp: tx.ts, flags: 0, frame_code: tx.fcode };
                let spike: u64 = np.spikes.iter().filter(|s| t >= s.0 && t < s.0 + s.1).map(|s| s.2).sum();
                let delay = 200 + tx.rng.gen_range(0..np.net_jitter_us.max(100)) + spike;
                if !(np.loss > 0.0 && tx.rng.gen_bool(np.loss)) {
                    pending.push((t + delay, pi, h, payload[..nb].to_vec()));
                }
                tx.seq = tx.seq.wrapping_add(1);
                tx.ts = tx.ts.wrapping_add(np.frame as u32);
                tx.acc -= np.frame;
            }
            tx.k_in += 1;
        } else if t == t_out {
            rt.process(&mut dev, 2);
            k_out += 1;
        } else {
            let timing = tracker.observe(&counters, t, p.out_buf, 48_000);
            let mut urgent = false;
            for i in 0..n {
                let slot = pfs[i].slot();
                let ev = pfs[i].service(&mut jbs[i], &mut feed.prods[slot], &feed.bank.slots[slot], &timing, &|| true, &mut chunk, &mut tmp, None);
                urgent |= ev.urgent;
                let c = jbs[i].stats().concealed;
                if c > seen_conc[i] {
                    outs[i].conceal_us.push(t);
                }
                seen_conc[i] = c;
            }
            let x = counters.xruns.load(Ordering::Relaxed);
            if x > seen_x {
                xrun_us.push(t);
            }
            seen_x = x;
            if !settled && t >= p.settle_us {
                settled = true;
                for i in 0..n {
                    outs[i].js_settle = jbs[i].stats();
                }
            }
            if t >= next_trace {
                for i in 0..n {
                    if jbs[i].is_playing() {
                        let s = jbs[i].stats();
                        outs[i].trace.push((t as f64 / 1e6, s.buffer_ms, s.target_ms));
                    }
                }
                next_trace += 100_000;
            }
            let poll = if urgent { FINE_POLL } else { POLL }.as_micros() as u64;
            t_w = t + poll + if p.worker_jitter_us > 0 { rng.gen_range(0..p.worker_jitter_us) } else { 0 };
        }
    }
    for i in 0..n {
        outs[i].js = jbs[i].stats();
        outs[i].margin = pfs[i].margin();
    }
    MultiOut { peers: outs, xruns: counters.xruns.load(Ordering::Relaxed), xrun_us }
}

fn ms(frame: usize) -> f32 {
    frame as f32 / 48.0
}

/// 시나리오 한 줄 요약(PR 표에 옮기는 값).
fn report(label: &str, p: &PeerOut, frame: usize, a: f64, b: f64) -> (f32, f32) {
    let (buf, p95) = (p.median_buffer(a, b), p.median_p95(a, b));
    println!(
        "TUNE {label}: bufferMs 중앙값 {buf:.2} (1F+p95 = {:.2}), 목표 {:.2}, p95 {p95:.2}ms, 은닉 {:.2}% (손실 {:.1}%), 보정 {}, 여유 {}",
        ms(frame) + p95,
        p.js.target_ms,
        p.conceal_pct(),
        p.js.loss_pct,
        p.corrections(),
        p.margin
    );
    (buf, p95)
}

// ----------------------------------------------------------------------------
// 완료 조건(P1-5)
// ----------------------------------------------------------------------------

/// 안정 회선(망 지터 ≤ 1ms): bufferMs 중앙값 ≤ 1프레임 + p95 지터(GOAL 1절). 2.5/5/10ms 프레임.
#[test]
fn tune_stable_line_buffer_within_goal() {
    for (frame, in_buf, out_buf) in [(120usize, 128usize, 128usize), (240, 128, 128), (240, 480, 480), (480, 256, 256)] {
        let mut np = NetPeer::new(frame, 3);
        np.in_buf = in_buf;
        np.net_jitter_us = 1000;
        let mut sim = MultiSim::new(30.0, vec![np]);
        sim.out_buf = out_buf;
        let r = run_multi(&sim);
        let p = &r.peers[0];
        let (buf, p95) = report(&format!("안정 F {frame} in {in_buf} out {out_buf}"), p, frame, 5.0, 30.0);
        assert!(buf <= ms(frame) + p95, "F {frame}: bufferMs {buf:.2} > 1F + p95 {:.2}", ms(frame) + p95);
        assert_eq!(p.conceal_pct(), 0.0, "안정 회선 은닉");
        assert_eq!(r.xruns, 0);
    }
}

/// 손실 1%(+ 망 지터 0~5ms): 은닉 비율 ≤ 1.5%(GOAL 2절).
#[test]
fn tune_loss_1pct_concealment_within_goal() {
    for (frame, jitter_us) in [(120usize, 5000u64), (240, 5000), (240, 20_000)] {
        let mut np = NetPeer::new(frame, 17);
        np.net_jitter_us = jitter_us;
        np.loss = 0.01;
        let r = run_multi(&MultiSim::new(60.0, vec![np]));
        let p = &r.peers[0];
        report(&format!("손실 1% F {frame} 지터 {jitter_us}µs"), p, frame, 5.0, 60.0);
        assert!(p.conceal_pct() <= 1.5, "F {frame} 지터 {jitter_us}: 은닉 {:.2}%", p.conceal_pct());
    }
}

/// 지터 버스트(10초에 0.3초 동안 +30ms): 상향 시각, 버스트 동안 은닉, 끝난 뒤 복귀 시간.
#[test]
fn tune_jitter_burst_up_and_return() {
    for frame in [120usize, 240] {
        let mut np = NetPeer::new(frame, 5);
        np.net_jitter_us = 1000;
        np.spikes = vec![(10_000_000, 300_000, 30_000)];
        let r = run_multi(&MultiSim::new(40.0, vec![np]));
        let p = &r.peers[0];
        let base_t = p.target_at(9.9);
        let base_b = p.median_buffer(5.0, 9.9);
        let up = p.first_target_at_least(10.0, base_t + 15.0).map(|t| t - 10.0);
        let back = p.last_buffer_above(10.3, 40.0, base_b + 1.0).map(|t| t - 10.3).unwrap_or(0.0);
        let conc = p.conceal_between(10_000_000, 12_000_000);
        let late = p.conceal_between(12_000_000, 40_000_000);
        println!(
            "TUNE 버스트 F {frame}: 기준 목표 {base_t:.2} 버퍼 {base_b:.2}ms, 상향까지 {up:?}s, 버스트~2s 은닉 이벤트 {conc}, 이후 {late}, 버스트 끝 → 버퍼 복귀(기준+1ms) {back:.1}s, 끝 버퍼 {:.2}/{:.2}ms",
            p.median_buffer(35.0, 40.0),
            p.js.target_ms
        );
        assert_eq!(late, 0, "버스트 2초 뒤 은닉");
        assert!(back <= 8.0, "F {frame}: 복귀 {back:.1}s");
        assert!((p.median_buffer(35.0, 40.0) - base_b).abs() < 1.0, "버스트 뒤 원래 지연으로");
    }
}

/// 6피어 동시: 회선이 서로 다른 피어 6명. 피어별 목표가 자기 회선만 따른다.
#[test]
fn tune_six_peers_simultaneous() {
    let mut peers = Vec::new();
    for (i, (jit, loss, drift, spike)) in
        [(1000u64, 0.0, 0.0, false), (1000, 0.0, 200.0, false), (5000, 0.01, -200.0, false), (20_000, 0.01, 0.0, false), (1000, 0.0, 0.0, true), (3000, 0.02, 100.0, false)]
            .into_iter()
            .enumerate()
    {
        let mut np = NetPeer::new(120, 100 + i as u64);
        np.net_jitter_us = jit;
        np.loss = loss;
        np.drift_ppm = drift;
        if spike {
            np.spikes = vec![(12_000_000, 300_000, 30_000)];
        }
        peers.push(np);
    }
    let r = run_multi(&MultiSim::new(40.0, peers.clone()));
    println!("TUNE 6피어: xruns {}", r.xruns);
    for (i, p) in r.peers.iter().enumerate() {
        let np = &peers[i];
        let (buf, p95) = report(
            &format!("6피어 #{i} 지터 {}µs 손실 {:.0}% 드리프트 {}ppm 스파이크 {}", np.net_jitter_us, np.loss * 100.0, np.drift_ppm, !np.spikes.is_empty()),
            p,
            120,
            5.0,
            40.0,
        );
        assert!(p.conceal_pct() <= np.loss * 100.0 + 0.5 + if np.spikes.is_empty() { 0.0 } else { 0.5 }, "#{i} 은닉 {:.2}%", p.conceal_pct());
        if np.net_jitter_us <= 1000 && np.spikes.is_empty() {
            assert!(buf <= ms(120) + p95 + 0.3, "#{i}: bufferMs {buf:.2} vs 1F+p95 {:.2}", ms(120) + p95);
        }
    }
    assert_eq!(r.xruns, 0);
}

/// PR #16 M2: 드리프트(±200ppm)가 있으면 깊이는 삽입 진입 문턱만큼 목표 밑에 머문다. 삽입 문턱을 목표에 넣었으므로
/// 여유를 학습한 뒤(이른 콜백 + 워커 기상 지연 → 언더런) 드리프트 아래에서도 언더런 콜백 밖의 은닉이 없어야 한다.
#[test]
fn tune_margin_with_drift_no_steady_concealment() {
    let (mut runs, mut learned, mut conc) = (0, 0, 0usize);
    for drift in [200.0, -200.0] {
        for (out_buf, wj) in [(144usize, 1300u64), (144, 1700), (128, 1300)] {
            for seed in 0..3u64 {
                let mut np = NetPeer::new(120, 40 + seed);
                np.in_buf = 96;
                np.drift_ppm = drift;
                let mut sim = MultiSim::new(30.0, vec![np]);
                sim.out_buf = out_buf;
                sim.out_phase_us = 900 * seed;
                sim.worker_jitter_us = wj;
                sim.out_early_us = 1700;
                sim.seed = seed * 7 + wj;
                let r = run_multi(&sim);
                let p = &r.peers[0];
                runs += 1;
                learned += usize::from(p.margin > 0 || r.xruns > 0);
                // 언더런 콜백(3ms 창)의 은닉만 빼고 센다.
                let c = p.conceal_us.iter().filter(|&&c| c >= 2_000_000 && !r.xrun_us.iter().any(|&x| c + 1_000 >= x && c < x + 3_000)).count();
                conc += c;
                println!(
                    "TUNE M2 드리프트 {drift}ppm out {out_buf} 기상 {wj} 시드 {seed}: xruns {}, 여유 {}, 은닉(언더런 콜백 제외) {c}, buffer 중앙값 {:.2} / 목표 {:.2}ms, 보정 {}",
                    r.xruns,
                    p.margin,
                    p.median_buffer(5.0, 30.0),
                    p.js.target_ms,
                    p.corrections()
                );
                assert_eq!(c, 0, "드리프트 {drift} out {out_buf} 기상 {wj} 시드 {seed}: 은닉 시각 {:?} xrun {:?}", p.conceal_us, r.xrun_us);
            }
        }
    }
    println!("TUNE M2: {runs}실행 중 여유 학습 {learned}, 언더런 콜백 밖 은닉 {conc}");
    assert!(learned >= 4, "여유 학습이 일어나야 함(전제): {learned}");
}

/// 프로필(저지연 0 ↔ 균형 0.5 ↔ 안정 1): 같은 회선에서 지연은 오르고 은닉은 내려간다.
#[test]
fn tune_profile_tradeoff() {
    let mut rows = Vec::new();
    for profile in [0.0f32, 0.5, 1.0] {
        let mut np = NetPeer::new(120, 77);
        np.net_jitter_us = 8000;
        np.loss = 0.01;
        np.spikes = vec![(10_000_000, 200_000, 20_000), (16_000_000, 200_000, 20_000), (22_000_000, 200_000, 20_000)];
        np.profile = profile;
        let r = run_multi(&MultiSim::new(40.0, vec![np]));
        let p = &r.peers[0];
        let (buf, _) = report(&format!("프로필 {profile}"), p, 120, 5.0, 40.0);
        rows.push((buf, p.conceal_pct()));
    }
    assert!(rows[0].0 < rows[1].0 && rows[1].0 < rows[2].0, "지연은 저지연 < 균형 < 안정: {rows:?}");
    assert!(rows[0].1 >= rows[1].1 && rows[1].1 >= rows[2].1, "은닉은 저지연 ≥ 균형 ≥ 안정: {rows:?}");
}
