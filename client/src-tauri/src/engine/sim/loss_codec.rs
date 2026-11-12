//! 손실 피드백 코덱 전환 시뮬레이션(P1-4e, 가상 시간, 결정적).
//!
//! 송신(PCM16 설정 + 피어 전용 Opus 인코더 + `LossCodecPolicy` 200ms 점검) → 손실 회선(무작위/버스트, 지연 5ms + 지터
//! U(0,1ms)) → 수신 `JitterBuffer`(실제 코드) → 500ms마다 Ping 평문 손실 보고(`ping_plain` → `LossReport::parse_ping`,
//! 회신 지연 5ms) → 송신 정책. 같은 시드로 피드백을 끈 대조군(PCM16 고정)을 함께 돌려 같은 구간을 비교한다.
//!
//! 지표(출력 L 채널, 100ms 칸):
//! - 은닉률: 은닉 프레임 / 생산 프레임(GOAL 2절 지표). 손실 은닉은 손실을 따라가므로 코덱으로는 거의 안 바뀐다.
//! - 불연속: 인접 샘플 차 > 문턱(원신호 최대 기울기 × 2, 최소 0.05). PCM 은닉(직전 프레임 페이드 반복)의 접합부가 여기 걸린다.
//! - 끊김(ms/s): |x| < 1e-3이 1ms 넘게 이어진 구간(원신호에는 없다 — `music_signal_has_transients_and_no_gaps`).
//!   PCM은 연속 손실 둘째부터 무음.
//!
//! 신호는 두 가지다(P1-4f M2). 정현파 3개는 CELT PLC(피치 반복)에 매우 유리해 이득을 부풀린다. 음악 신호(화음 변화,
//! 베이스, 킥·스네어·하이햇 과도음, 결정적 잡음)는 예측이 어려운 구간이 있어 PLC 이득이 작아지는 쪽의 기준이다.
//!
//! 결과 표: `cargo test --lib loss_feedback_sim -- --nocapture` (두 신호 모두).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rand::{Rng, SeedableRng};

use crate::engine::codec::Encoder;
use crate::engine::config::{frame_code_from_samples, CodecKind, MAX_PAYLOAD_BYTES};
use crate::engine::jitter::{JitterBuffer, JitterSettings};
use crate::engine::relay_codec::{loss_opus_perc, LossCodecPolicy, DEFAULT_OPUS_LOSS_PERC};
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

#[derive(Clone, Copy, Debug, PartialEq)]
enum SignalKind {
    /// 정현파 3개(220·330·440Hz) 정상 신호. Opus PLC(피치 반복)에 매우 유리하다(#66 리뷰 M2).
    Sines,
    /// 음악 같은 신호(P1-4f M2): 120bpm, 1초마다 바뀌는 화음(배음 4개, 어택·감쇠·크로스페이드) + 박마다 베이스 +
    /// 킥(1·3박, 피치 하강)·스네어(2·4박, 저역 통과 잡음 + 톤)·하이햇(8분, 잡음) 과도음. 잡음은 xorshift라 결정적.
    Music,
}

struct Signal {
    kind: SignalKind,
    n: u64,
    phase: [f64; 3],
    noise: u32,
    snare_lp: f64,
    hat_lp: f64,
}

const SR: f64 = 48_000.0;
/// 화음 진행(1초씩): C, Am, F, G.
const CHORDS: [[f64; 3]; 4] = [[261.63, 329.63, 392.0], [220.0, 261.63, 329.63], [174.61, 220.0, 261.63], [196.0, 246.94, 293.66]];

impl Signal {
    fn new(kind: SignalKind) -> Self {
        Self { kind, n: 0, phase: [0.0; 3], noise: 0x9E37_79B9, snare_lp: 0.0, hat_lp: 0.0 }
    }

    fn noise(&mut self) -> f64 {
        let mut x = self.noise;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.noise = x;
        x as f64 / u32::MAX as f64 * 2.0 - 1.0
    }

    fn fill(&mut self, out: &mut [f32]) {
        for s in out.iter_mut() {
            *s = match self.kind {
                SignalKind::Sines => {
                    const F: [f64; 3] = [220.0, 330.0, 440.0];
                    let mut v = 0.0;
                    for (p, f) in self.phase.iter_mut().zip(F) {
                        v += (*p * std::f64::consts::TAU).sin() * 0.1;
                        *p = (*p + f / SR).fract();
                    }
                    v as f32
                }
                SignalKind::Music => self.music_sample() as f32,
            };
            self.n += 1;
        }
    }

    fn music_sample(&mut self) -> f64 {
        use std::f64::consts::TAU;
        let t = self.n as f64 / SR;
        let ramp = |x: f64, w: f64| (x / w).clamp(0.0, 1.0);
        // 화음: 지금 구간과 직전 구간(릴레이스 20ms)을 겹쳐 무음 틈 없이 바꾼다
        let seg = (t / 1.0).floor() as i64;
        let mut pad = 0.0;
        for k in [seg - 1, seg] {
            if k < 0 {
                continue;
            }
            let ts = t - k as f64;
            let env = ramp(ts, 0.008) * (0.55 + 0.45 * (-ts / 0.25).exp()) * (1.0 - ramp(ts - 1.0, 0.02));
            if env <= 0.0 {
                continue;
            }
            let chord = CHORDS[(k as usize) % CHORDS.len()];
            let mut v = 0.0;
            for f in chord {
                for (h, a) in [(1.0, 1.0), (2.0, 0.5), (3.0, 0.33), (4.0, 0.2)] {
                    v += (TAU * f * h * t).sin() * a;
                }
            }
            pad += v * env * 0.035;
        }
        // 베이스: 박마다(0.5초) 화음 근음의 한 옥타브 아래, 뜯는 엔벨로프
        let beat = (t / 0.5).floor();
        let tb = t - beat * 0.5;
        let root = CHORDS[((t / 1.0).floor() as usize) % CHORDS.len()][0] / 2.0;
        let bass = (TAU * root * t).sin() * 0.12 * ramp(tb, 0.004) * (0.35 + 0.65 * (-tb / 0.12).exp()) * (1.0 - ramp(tb - 0.49, 0.01));
        // 드럼: 1·3박 킥, 2·4박 스네어, 8분 하이햇
        let beat_in_bar = (beat as i64) % 4;
        let kick = if beat_in_bar % 2 == 0 {
            let ph = 50.0 * tb + 100.0 * 0.03 * (1.0 - (-tb / 0.03).exp());
            (TAU * ph).sin() * 0.35 * (-tb / 0.12).exp() * ramp(tb, 0.001)
        } else {
            0.0
        };
        let nz = self.noise();
        // 잡음은 저역 통과해 샘플 간 기울기를 제한한다(불연속 문턱이 PCM 접합 점프보다 충분히 낮게)
        self.snare_lp += 0.12 * (nz - self.snare_lp);
        self.hat_lp += 0.5 * (nz - self.hat_lp);
        let snare = if beat_in_bar % 2 == 1 {
            ((self.snare_lp * 0.3 + (TAU * 185.0 * tb).sin() * 0.08) * (-tb / 0.07).exp()) * ramp(tb, 0.0007)
        } else {
            0.0
        };
        let te = t - (t / 0.25).floor() * 0.25;
        let hat_src = self.hat_lp - self.snare_lp;
        let hat = hat_src * 0.04 * (-te / 0.025).exp() * ramp(te, 0.0005);
        pad + bass + kick + snare + hat
    }
}

/// 원신호의 인접 샘플 차 최대와, |x| < 1e-3이 1ms 넘게 이어진 샘플 수(끊김 지표가 원신호에서 0인지).
fn source_profile(kind: SignalKind, secs: f64) -> (f32, u64) {
    let mut s = Signal::new(kind);
    let mut buf = vec![0f32; FRAME];
    let (mut prev, mut max_d, mut quiet, mut gap) = (0f32, 0f32, 0u64, 0u64);
    for _ in 0..(secs * SR / FRAME as f64) as usize {
        s.fill(&mut buf);
        for &x in &buf {
            max_d = max_d.max((x - prev).abs());
            prev = x;
            if x.abs() < 1e-3 {
                quiet += 1;
                if quiet > 48 {
                    gap += 1;
                }
            } else {
                quiet = 0;
            }
        }
    }
    (max_d, gap)
}

/// 불연속 문턱: 원신호 최대 기울기의 2배(최소 0.05). 정현파는 0.05(최대 기울기 ≈ 0.013), 음악은 과도음 때문에 더 높다.
fn disc_threshold(kind: SignalKind, secs: f64) -> f32 {
    (source_profile(kind, secs).0 * 2.0).max(0.05)
}

fn run(s: &Scenario, feedback: bool, kind: SignalKind, disc_thr: f32) -> RunOut {
    let mut rng = rand::rngs::StdRng::seed_from_u64(s.seed);
    let t0 = Instant::now();
    let at = |us: f64| t0 + Duration::from_micros(us as u64);
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    // 수신은 실제처럼 피어별 수신 큐를 거친다(엔진 ③-a3): 도착 → push, pop 직전에 비워 삽입.
    let (mut mtx, mut mrx) = crate::net::media_link::media_link(crate::net::media_link::PeerKey::from_raw(1));
    jb.set_consumer_lead(72.0);
    let mut pcm = Encoder::new(CodecKind::Pcm16, 1, 96_000).unwrap();
    let mut opus = Encoder::new(CodecKind::Opus, 1, 96_000).unwrap();
    let _ = opus.set_loss_perc(DEFAULT_OPUS_LOSS_PERC);
    let mut policy = LossCodecPolicy::default();
    let mut sig = Signal::new(kind);
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
            mtx.push(&h, &pk, a as u64);
        }
        mrx.drain(|p| jb.insert(&p.hdr, p.payload(), p.arrival_us));
        jb.pop(&mut out);
        jb.end_burst(t as u64, t as u64, 0.0);
        if jb.is_playing() {
            for fr in out.as_chunks::<2>().0 {
                let x = fr[0];
                if (x - prev_out).abs() > disc_thr {
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

fn scenario_rows(kind: SignalKind, disc_thr: f32) -> Vec<Row> {
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
            let fb = run(s, true, kind, disc_thr);
            let ctl = run(s, false, kind, disc_thr);
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

/// 손실이 끝난 뒤 복귀 전까지(Opus, 무손실) 불연속/s — 코덱 자체가 만드는 기준선(음악 신호에서 의미가 있다).
fn opus_clean_disc(r: &Row) -> Option<f64> {
    let off = r.fb.off_s?;
    (off > r.s.loss_s + 2.0).then(|| agg(&r.fb.bins, r.s.loss_s + 1.0, off - 0.5).disc_per_s)
}

fn print_table(kind: SignalKind, disc_thr: f32, rows: &[Row]) {
    println!();
    println!("신호: {kind:?}, 불연속 문턱 |Δx| > {disc_thr:.3}");
    println!(
        "| 회선(손실 0..{}s) | 실제 손실 | 보고 최대 | 전환 | 복귀 | 예상 손실 설정 | 비교 구간 | 은닉 PCM→자동 | 불연속/s PCM→자동 | 끊김 ms/s PCM→자동 | Opus 무손실 불연속/s |",
        rows[0].s.loss_s
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for r in rows {
        println!(
            "| {} | {:.2}% | {:.2}% | {} | {} | {} | {:.1}–{:.0}s | {:.2}% → {:.2}% | {:.1} → {:.1} | {:.2} → {:.2} | {} |",
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
            opus_clean_disc(r).map_or("-".into(), |d| format!("{d:.1}")),
        );
    }
}

/// 전환·복귀·은닉 규칙(신호와 무관).
fn check_policy(rows: &[Row]) {
    let clean_after = |r: &Row| agg(&r.fb.bins, r.s.loss_s + 1.0, r.s.loss_s + r.s.clean_s);
    for r in rows {
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
        let c = clean_after(r);
        assert!(c.conceal_pct < 0.2, "{name}: 손실 끝난 뒤 은닉 {:.2}%(전환·복귀 자체가 은닉을 만들면 안 됨)", c.conceal_pct);
    }
}

#[test]
fn music_signal_has_transients_and_no_gaps() {
    let (sine_d, sine_gap) = source_profile(SignalKind::Sines, 4.0);
    let (music_d, music_gap) = source_profile(SignalKind::Music, 8.0);
    assert_eq!((sine_gap, music_gap), (0, 0), "원신호에는 끊김 지표에 걸리는 무음이 없어야 한다");
    assert!(sine_d < 0.02, "{sine_d}");
    assert!(music_d > sine_d * 2.0, "음악 신호는 과도음 때문에 기울기가 더 크다: {music_d} vs {sine_d}");
    assert!(music_d < 0.08, "불연속 문턱(×2)이 PCM 접합 점프(신호 진폭급 0.1~0.4)보다 충분히 낮아야 한다: {music_d}");
    // 결정적
    let mut a = Signal::new(SignalKind::Music);
    let mut b = Signal::new(SignalKind::Music);
    let (mut x, mut y) = (vec![0f32; 4800], vec![0f32; 4800]);
    a.fill(&mut x);
    b.fill(&mut y);
    assert_eq!(x, y);
}

#[test]
fn loss_feedback_sim() {
    // 정현파 3개(#66 기준선, Opus PLC에 유리)
    let thr = disc_threshold(SignalKind::Sines, 80.0);
    let rows = scenario_rows(SignalKind::Sines, thr);
    print_table(SignalKind::Sines, thr, &rows);
    check_policy(&rows);
    for r in rows.iter().filter(|r| r.fb.on_s.is_some()) {
        let name = r.s.name;
        // 품질: 정상 톤에서는 Opus PLC가 PCM 페이드 반복보다 접합 불연속·무음이 훨씬 적다
        assert!(r.auto.disc_per_s < r.pcm.disc_per_s * 0.5, "{name}: 불연속 {:.1} vs PCM {:.1}", r.auto.disc_per_s, r.pcm.disc_per_s);
        assert!(r.auto.gap_ms_per_s <= r.pcm.gap_ms_per_s, "{name}: 끊김 {:.2} vs PCM {:.2}", r.auto.gap_ms_per_s, r.pcm.gap_ms_per_s);
    }
}

#[test]
fn loss_feedback_sim_music() {
    // 음악 같은 신호(P1-4f M2): 과도음·화음 변화가 있어 PLC 이득이 작아진다. 전환 규칙은 같아야 하고, 품질은
    // 적어도 PCM16보다 나빠지지 않아야 한다.
    let thr = disc_threshold(SignalKind::Music, 80.0);
    let rows = scenario_rows(SignalKind::Music, thr);
    print_table(SignalKind::Music, thr, &rows);
    check_policy(&rows);
    for r in rows.iter().filter(|r| r.fb.on_s.is_some()) {
        let name = r.s.name;
        assert!(r.auto.disc_per_s <= r.pcm.disc_per_s, "{name}: 불연속 {:.1} vs PCM {:.1}", r.auto.disc_per_s, r.pcm.disc_per_s);
        assert!(r.auto.gap_ms_per_s <= r.pcm.gap_ms_per_s, "{name}: 끊김 {:.2} vs PCM {:.2}", r.auto.gap_ms_per_s, r.pcm.gap_ms_per_s);
    }
}
