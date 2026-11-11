//! 손실 피드백 코덱 전환 시뮬레이션(P1-4e, 가상 시간, 결정적).
//!
//! 송신(PCM16 설정 + 피어 전용 Opus 인코더 + `LossCodecPolicy` 200ms 점검) → 손실 회선(무작위/버스트, 지연 5ms + 지터
//! U(0,1ms)) → 수신 `JitterBuffer`(실제 코드) → 500ms마다 Ping 평문 손실 보고(`ping_plain` → `LossReport::parse_ping`,
//! 회신 지연 5ms) → 송신 정책. 같은 시드로 피드백을 끈 대조군(PCM16 고정)을 함께 돌려 같은 구간을 비교한다.
//!
//! 지표(출력 L 채널, 100ms 칸):
//! - 은닉률: 은닉 프레임 / 생산 프레임(GOAL 2절 지표). 손실 은닉은 손실을 따라가므로 코덱으로는 거의 안 바뀐다.
//! - 불연속: 인접 샘플 차 > 0.05(신호 최대 기울기 ≈ 0.013). PCM 은닉(직전 프레임 페이드 반복)의 접합부가 여기 걸린다.
//! - 끊김(ms/s): |x| < 1e-3이 1ms 넘게 이어진 구간(신호는 그렇게 오래 0 근처에 있지 않다). PCM은 연속 손실 둘째부터 무음.
//!
//! 결과 표: `cargo test --lib loss_feedback_sim -- --nocapture`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rand::{Rng, SeedableRng};

use super::codec::Encoder;
use super::config::{frame_code_from_samples, CodecKind, MAX_PAYLOAD_BYTES};
use super::jitter::{JitterBuffer, JitterSettings};
use super::relay_codec::{loss_opus_perc, LossCodecPolicy, DEFAULT_OPUS_LOSS_PERC};
use crate::net::packet::{Header, PacketType, FLAG_OPUS};
use crate::net::peer::{ping_plain, LossReport, PING_PLAIN_MAX};

const FRAME: usize = 120;
const TICK_US: f64 = FRAME as f64 / 48.0 * 1000.0;
const BIN_US: f64 = 100_000.0;

#[derive(Clone, Copy, Debug)]
enum LossModel {
    Random(f64),
    /// 길버트-엘리엇: 좋음 → 나쁨 확률, 나쁨 유지 확률(나쁨에서는 모두 손실).
    Burst { enter: f64, stay: f64 },
}

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    model: LossModel,
    /// 손실 구간(0..loss_s) 뒤 clean_s 동안 손실 없음.
    loss_s: f64,
    clean_s: f64,
    seed: u64,
}

#[derive(Default, Clone, Copy)]
struct Bin {
    produced: u64,
    concealed: u64,
    disc: u64,
    gap_samples: u64,
    opus: bool,
}

struct RunOut {
    bins: Vec<Bin>,
    on_s: Option<f64>,
    off_s: Option<f64>,
    sent: u64,
    lost: u64,
    max_report: f32,
    perc_at_on: Option<u8>,
}

struct Signal {
    phase: [f64; 3],
}

impl Signal {
    fn fill(&mut self, out: &mut [f32]) {
        const F: [f64; 3] = [220.0, 330.0, 440.0];
        for s in out.iter_mut() {
            let mut v = 0.0;
            for (p, f) in self.phase.iter_mut().zip(F) {
                v += (*p * std::f64::consts::TAU).sin() * 0.1;
                *p = (*p + f / 48_000.0).fract();
            }
            *s = v as f32;
        }
    }
}

fn run(s: &Scenario, feedback: bool) -> RunOut {
    let mut rng = rand::rngs::StdRng::seed_from_u64(s.seed);
    let t0 = Instant::now();
    let at = |us: f64| t0 + Duration::from_micros(us as u64);
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    jb.set_consumer_lead(72.0);
    let mut pcm = Encoder::new(CodecKind::Pcm16, 1, 96_000).unwrap();
    let mut opus = Encoder::new(CodecKind::Opus, 1, 96_000).unwrap();
    let _ = opus.set_loss_perc(DEFAULT_OPUS_LOSS_PERC);
    let mut policy = LossCodecPolicy::default();
    let mut sig = Signal { phase: [0.0; 3] };
    let mut frame = vec![0f32; FRAME];
    let mut prev = vec![0f32; FRAME];
    let mut payload = vec![0u8; MAX_PAYLOAD_BYTES];
    let mut out = vec![0f32; FRAME * 2];
    let (mut seq, mut ts) = (u32::MAX - 100, 7u32);
    let mut bad = false;
    let mut pending: VecDeque<(f64, Header, Vec<u8>)> = VecDeque::new();
    let mut reports: VecDeque<(f64, LossReport)> = VecDeque::new();
    let mut last_report: Option<(f32, Instant)> = None;
    let mut encoding = false;
    let total_us = (s.loss_s + s.clean_s) * 1e6;
    let mut r = RunOut { bins: vec![], on_s: None, off_s: None, sent: 0, lost: 0, max_report: 0.0, perc_at_on: None };
    let mut bin = Bin::default();
    let mut bin_start = jb.stats();
    let mut next_bin = BIN_US;
    let (mut next_check, mut next_ping) = (0.0, 0.0);
    let (mut prev_out, mut quiet_run) = (0f32, 0u64);
    let mut t = 0.0;
    while t < total_us {
        // ── 송신 ──
        while let Some(&(d, rep)) = reports.front() {
            if d > t {
                break;
            }
            reports.pop_front();
            last_report = Some((rep.loss_pct, at(d)));
            r.max_report = r.max_report.max(rep.loss_pct);
        }
        if feedback && t >= next_check {
            next_check += 200_000.0;
            match policy.update(at(t), true, last_report) {
                Some(true) => {
                    r.on_s.get_or_insert(t / 1e6);
                    let p = loss_opus_perc(last_report.map(|l| l.0));
                    r.perc_at_on.get_or_insert(p);
                    let _ = opus.set_loss_perc(p);
                }
                Some(false) => r.off_s = Some(t / 1e6),
                None => {}
            }
        }
        sig.fill(&mut frame);
        let want = policy.active();
        if want && !encoding {
            opus.reset();
            let _ = opus.encode(&prev, &mut payload);
        }
        encoding = want;
        let (n, flags) = if want { (opus.encode(&frame, &mut payload).unwrap(), FLAG_OPUS) } else { (pcm.encode(&frame, &mut payload).unwrap(), 0) };
        prev.copy_from_slice(&frame);
        let h = Header { ptype: PacketType::Audio, ssrc: 1, seq, timestamp: ts, flags, frame_code: frame_code_from_samples(FRAME).unwrap() };
        seq = seq.wrapping_add(1);
        ts = ts.wrapping_add(FRAME as u32);
        let lossy = t < s.loss_s * 1e6;
        let lost = lossy
            && match s.model {
                LossModel::Random(p) => rng.gen::<f64>() < p,
                LossModel::Burst { enter, stay } => {
                    bad = if bad { rng.gen::<f64>() < stay } else { rng.gen::<f64>() < enter };
                    bad
                }
            };
        r.sent += u64::from(lossy);
        r.lost += u64::from(lost);
        let jitter = rng.gen::<f64>() * 1000.0;
        if !lost {
            let arr = t + 5000.0 + jitter;
            let pos = pending.iter().position(|(a, _, _)| *a > arr).unwrap_or(pending.len());
            pending.insert(pos, (arr, h, payload[..n].to_vec()));
        }
        // ── 수신 ──
        while pending.front().is_some_and(|(a, _, _)| *a <= t) {
            let (a, h, pk) = pending.pop_front().unwrap();
            jb.insert(&h, &pk, a as u64);
        }
        jb.pop(&mut out);
        jb.end_burst(t as u64, t as u64, 0.0);
        if jb.is_playing() {
            for fr in out.as_chunks::<2>().0 {
                let x = fr[0];
                if (x - prev_out).abs() > 0.05 {
                    bin.disc += 1;
                }
                prev_out = x;
                if x.abs() < 1e-3 {
                    quiet_run += 1;
                    if quiet_run > 48 {
                        bin.gap_samples += 1;
                    }
                } else {
                    quiet_run = 0;
                }
            }
        }
        if t >= next_ping {
            next_ping += 500_000.0;
            if jb.is_playing() {
                let st = jb.stats();
                let mut plain = [0u8; PING_PLAIN_MAX];
                let len = ping_plain(None, Some(LossReport { loss_pct: st.loss_pct, conceal_pct: st.conceal_pct }), &mut plain);
                if let Some(rep) = LossReport::parse_ping(&plain[..len]) {
                    reports.push_back((t + 5000.0, rep));
                }
            }
        }
        bin.opus |= want;
        t += TICK_US;
        if t >= next_bin {
            next_bin += BIN_US;
            let st = jb.stats();
            bin.produced = st.produced - bin_start.produced;
            bin.concealed = st.concealed - bin_start.concealed;
            bin_start = st;
            r.bins.push(std::mem::take(&mut bin));
        }
    }
    r
}

#[derive(Debug, Clone, Copy)]
struct Agg {
    conceal_pct: f64,
    disc_per_s: f64,
    gap_ms_per_s: f64,
}

fn agg(bins: &[Bin], from_s: f64, to_s: f64) -> Agg {
    let (a, b) = ((from_s * 10.0) as usize, ((to_s * 10.0) as usize).min(bins.len()));
    let w = &bins[a.min(b)..b];
    let secs = w.len() as f64 / 10.0;
    let (p, c, d, g) = w.iter().fold((0, 0, 0, 0), |(p, c, d, g), x| (p + x.produced, c + x.concealed, d + x.disc, g + x.gap_samples));
    Agg {
        conceal_pct: c as f64 * 100.0 / p.max(1) as f64,
        disc_per_s: d as f64 / secs.max(1e-9),
        gap_ms_per_s: g as f64 / 48.0 / secs.max(1e-9),
    }
}

struct Row {
    s: Scenario,
    fb: RunOut,
    actual_loss: f64,
    win: (f64, f64),
    pcm: Agg,
    auto: Agg,
}

fn scenario_rows() -> Vec<Row> {
    let scenarios = [
        Scenario { name: "무작위 0.5%", model: LossModel::Random(0.005), loss_s: 40.0, clean_s: 20.0, seed: 11 },
        Scenario { name: "무작위 2%", model: LossModel::Random(0.02), loss_s: 40.0, clean_s: 40.0, seed: 12 },
        Scenario { name: "무작위 5%", model: LossModel::Random(0.05), loss_s: 40.0, clean_s: 40.0, seed: 13 },
        // 평균 버스트 길이 1/(1-0.75) = 4프레임(10ms), 정상 손실률 ≈ enter/(enter+1-stay) ≈ 2%
        Scenario { name: "버스트 ≈2%(평균 4프레임)", model: LossModel::Burst { enter: 0.0051, stay: 0.75 }, loss_s: 40.0, clean_s: 40.0, seed: 14 },
    ];
    scenarios
        .iter()
        .map(|s| {
            let fb = run(s, true);
            let ctl = run(s, false);
            // 비교 구간: 전환 1초 뒤부터 손실 끝까지(전환이 없으면 정착 뒤 손실 구간 전체)
            let win = (fb.on_s.map_or(10.0, |o| o + 1.0), s.loss_s);
            Row {
                s: *s,
                actual_loss: fb.lost as f64 * 100.0 / fb.sent.max(1) as f64,
                win,
                pcm: agg(&ctl.bins, win.0, win.1),
                auto: agg(&fb.bins, win.0, win.1),
                fb,
            }
        })
        .collect()
}

fn opt_s(v: Option<f64>) -> String {
    v.map_or("-".into(), |x| format!("{x:.1}s"))
}

#[test]
fn loss_feedback_sim() {
    let rows = scenario_rows();
    println!("| 회선(손실 0..{}s) | 실제 손실 | 보고 최대 | 전환 | 복귀 | 예상 손실 설정 | 비교 구간 | 은닉 PCM→자동 | 불연속/s PCM→자동 | 끊김 ms/s PCM→자동 |", rows[0].s.loss_s);
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for r in &rows {
        println!(
            "| {} | {:.2}% | {:.2}% | {} | {} | {} | {:.1}–{:.0}s | {:.2}% → {:.2}% | {:.1} → {:.1} | {:.2} → {:.2} |",
            r.s.name,
            r.actual_loss,
            r.fb.max_report,
            opt_s(r.fb.on_s),
            opt_s(r.fb.off_s),
            r.fb.perc_at_on.map_or("-".into(), |p| format!("{p}%")),
            r.win.0,
            r.win.1,
            r.pcm.conceal_pct,
            r.auto.conceal_pct,
            r.pcm.disc_per_s,
            r.auto.disc_per_s,
            r.pcm.gap_ms_per_s,
            r.auto.gap_ms_per_s,
        );
    }
    let clean_after = |r: &Row| agg(&r.fb.bins, r.s.loss_s + 1.0, r.s.loss_s + r.s.clean_s);
    for r in &rows {
        let name = r.s.name;
        if r.actual_loss < 1.0 {
            assert!(r.fb.on_s.is_none(), "{name}: 손실 1% 미만에서는 전환하지 않는다");
            continue;
        }
        let on = r.fb.on_s.unwrap_or_else(|| panic!("{name}: 손실 {:.2}%인데 전환 안 됨", r.actual_loss));
        // 무작위 손실은 5초 창이 2%에 닿은 뒤 1초. 버스트는 창 값이 2% 위아래를 오가므로(분산 큼) 더 걸릴 수 있다.
        let limit = if matches!(r.s.model, LossModel::Burst { .. }) { 20.0 } else { 10.0 };
        assert!(on < limit, "{name}: 전환이 늦다({on:.1}s)");
        let off = r.fb.off_s.unwrap_or_else(|| panic!("{name}: 손실이 끝났는데 복귀 안 됨"));
        // 손실이 끝난 뒤: 5초 창이 1% 아래로(≤5초) + hold 10초 + 점검·보고 간격
        assert!(off > r.s.loss_s && off < r.s.loss_s + 17.0, "{name}: 복귀 시각 {off:.1}s");
        assert!(r.fb.bins[(off * 10.0) as usize + 5..].iter().all(|b| !b.opus), "{name}: 복귀 뒤 PCM16 유지");
        // 은닉률은 손실을 따라간다(코덱 무관). 자동 전환이 은닉을 늘리지 않는다.
        assert!(r.auto.conceal_pct <= r.pcm.conceal_pct + 0.3, "{name}: 은닉 {:.2}% > PCM {:.2}%", r.auto.conceal_pct, r.pcm.conceal_pct);
        // 품질: Opus PLC는 PCM 페이드 반복보다 접합 불연속·무음이 적다
        assert!(r.auto.disc_per_s < r.pcm.disc_per_s * 0.5, "{name}: 불연속 {:.1} vs PCM {:.1}", r.auto.disc_per_s, r.pcm.disc_per_s);
        assert!(r.auto.gap_ms_per_s <= r.pcm.gap_ms_per_s, "{name}: 끊김 {:.2} vs PCM {:.2}", r.auto.gap_ms_per_s, r.pcm.gap_ms_per_s);
        let c = clean_after(r);
        assert!(c.conceal_pct < 0.2, "{name}: 손실 끝난 뒤 은닉 {:.2}%(전환·복귀 자체가 은닉을 만들면 안 됨)", c.conceal_pct);
    }
}
