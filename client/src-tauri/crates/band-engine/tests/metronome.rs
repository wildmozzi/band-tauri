//! 공유 메트로놈(P2-3) 2피어 시뮬레이션 — 가상 시간, 결정적.
//!
//! 실제 구성요소를 그대로 쓴다: 출력 콜백(`OutputRt` + `ClickRt`), 워커의 콜백 시각 추정(`CallbackTracker`),
//! 시계 오프셋 추정(`ClockSync`), 박 스케줄러(`BeatScheduler`). 가상인 것은 시간·장치·회선뿐이다.
//!
//! 모델(설계 1.4절):
//! - 참 시간 τ. 피어 시스템 시계 = τ·(1+ppm) + 오프셋. 장치 레이트 = 공칭 × (1+장치 ppm)(참 시간 기준).
//! - 장치(하드웨어) 프레임 시계: 프레임 J가 귀에 닿는 참 시각 = 원점 + (J + n)/레이트 + 숨은 출력 지연.
//!   프레임 [b, b+n)을 쓰는 콜백의 공칭 시각은 원점 + b/레이트(직전 버퍼가 울리기 시작할 때)이고,
//!   실제 콜백은 공칭 + 콜백 지터에 불린다(PR #34 M1). 버스트 크기가 교대로 바뀔 수 있다(44.1k의 440/441).
//! - 콜백 공백(PR #34 M3): 장치가 g만큼 멈추면 원점이 g 뒤로 밀린다(그 사이 콜백·재생 없음).
//! - 숨은 출력 지연(PR #34 M2): 드라이버가 보고하면 그 값, 보고가 없으면 루프백 보정값(숨은 입력 + 숨은 출력)을
//!   `measured_hidden_out_us`로 나눈 값을 `OutClock::extra_out_us`로 보정한다(M2(a)).
//! - 워커는 0.5~1.2ms마다 깨어 콜백 카운터를 본다(기상 지연 포함).
//! - 따르는 쪽이 500ms마다 방장에게 Ping, 편도 = 기본 + 지수 큐잉(방향별 독립).
//! - 정렬 오차 = 같은 박의 두 피어 클릭이 귀에 닿는 참 시각 차.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use band_engine::testing::engine::metronome::{measured_hidden_out_us, BeatScheduler, ClickRt, ClickShared, MetronomePlan, OutClock};
use band_dsp::mixer::MixControls;
use band_engine::testing::engine::rt::{output_path, OutputRt};
use band_dsp::stats::AudioCounters;
use band_engine::testing::engine::worker::{CallbackTracker, MetronomeStageProbe};
use band_dsp::clock::ClockSync;
use ringbuf::traits::Consumer;
use ringbuf::HeapCons;

struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn exp(&mut self, mean: f64) -> f64 {
        -mean * (1.0 - self.f()).max(1e-12).ln()
    }
}

#[derive(Clone, Copy)]
struct PeerSpec {
    sys_ppm: f64,
    sys_offset_us: f64,
    rate: u32,
    dev_ppm: f64,
    buffer: usize,
    /// 첫 콜백 참 시각(µs). 시스템 시계는 엔진 시계처럼 0 이상이어야 한다(오프셋 ≥ 0).
    phase_us: f64,
    /// 콜백 지터.
    jitter: Jitter,
    /// 버스트 크기를 n, n−1로 교대(44.1k에서 10ms 주기 441/440).
    alt_burst: bool,
    /// 콜백 공백: (시작 참 시각, 길이) µs.
    gap: Option<(f64, f64)>,
    /// 버퍼 1개 너머의 숨은 출력 지연(µs)과 그 보정 방법.
    hidden_out_us: f64,
    comp: Comp,
}

/// 숨은 출력 지연 보정 방법.
#[derive(Clone, Copy)]
enum Comp {
    /// 보정 안 함(드라이버 보고·루프백 측정 없음).
    None,
    /// 드라이버가 출력 지연을 보고(그 값 그대로).
    Driver,
    /// 드라이버 보고 없음 + 루프백 보정값(PR #34 M2(a)): 그 장치의 숨은 입력 지연과 측정 오차(µs).
    Loopback { hidden_in_us: f64, measure_err_us: f64 },
}

#[derive(Clone, Copy)]
enum Jitter {
    None,
    /// 공칭 ± 균등(µs).
    Uniform(f64),
    /// 공칭 + [0, 최대) 균등(µs). 이벤트 구동 콜백은 공칭보다 이를 수 없다.
    Late(f64),
}

struct SimPeer {
    spec: PeerSpec,
    out: OutputRt,
    tap: HeapCons<f32>,
    counters: Arc<AudioCounters>,
    click: Arc<ClickShared>,
    tracker: CallbackTracker,
    sched: BeatScheduler,
    buf: Vec<f32>,
    rng: Rng,
    /// 하드웨어 프레임 시계 원점(참 시각 µs).
    origin_us: f64,
    gap_done: bool,
    cb_count: u64,
    next_cb_us: f64,
    frames_written: u64,
    next_poll_us: f64,
    last_nonzero: Option<u64>,
    /// 클릭이 귀에 닿은 참 시각들.
    onsets: Vec<f64>,
    tap_nonzero: usize,
}

impl SimPeer {
    fn new(spec: PeerSpec, seed: u64) -> Self {
        Self::with_click(spec, seed, Arc::new(ClickShared::default()))
    }

    /// 클릭 공유 상태를 밖에서 받는다(메트로놈 단계 시험 입구 `MetronomeStageProbe`가 클릭을 올리는 곳).
    fn with_click(spec: PeerSpec, seed: u64, click: Arc<ClickShared>) -> Self {
        let counters = Arc::new(AudioCounters::default());
        let controls = Arc::new(MixControls::default());
        let (mut out, feed) = output_path(counters.clone(), controls);
        click.step.store(48_000.0 / spec.rate as f32);
        out.attach_click(ClickRt::new(click.clone()));
        Self {
            spec,
            out,
            tap: feed.tap,
            counters,
            click,
            tracker: CallbackTracker::new(spec.rate),
            sched: BeatScheduler::new(),
            buf: vec![0.0; spec.buffer * 2],
            rng: Rng(seed),
            origin_us: spec.phase_us,
            gap_done: false,
            cb_count: 0,
            next_cb_us: spec.phase_us,
            frames_written: 0,
            next_poll_us: spec.phase_us,
            last_nonzero: None,
            onsets: Vec::new(),
            tap_nonzero: 0,
        }
    }

    fn sys_us(&self, tau: f64) -> f64 {
        tau * (1.0 + self.spec.sys_ppm * 1e-6) + self.spec.sys_offset_us
    }

    fn true_rate(&self) -> f64 {
        self.spec.rate as f64 * (1.0 + self.spec.dev_ppm * 1e-6)
    }

    /// 출력 콜백(참 시각 `tau`).
    fn callback(&mut self, tau: f64) {
        let n_nom = self.spec.buffer;
        let n = if self.spec.alt_burst && self.cb_count % 2 == 1 { n_nom - 1 } else { n_nom };
        self.cb_count += 1;
        let b = self.frames_written;
        self.out.process(&mut self.buf[..n * 2], 2);
        let r = self.true_rate();
        for i in 0..n {
            let x = self.buf[i * 2];
            if x != 0.0 {
                let j = b + i as u64;
                // 클릭 사이 간격(≥ 200ms)보다 짧은 무음 뒤는 같은 클릭의 꼬리
                if self.last_nonzero.is_none_or(|l| j - l > (self.spec.rate / 20) as u64) {
                    self.onsets.push(self.origin_us + (j + n_nom as u64) as f64 * 1e6 / r + self.spec.hidden_out_us);
                }
                self.last_nonzero = Some(j);
            }
        }
        // 믹스 탭(관람 방송·녹음)에는 클릭이 없어야 한다
        let mut tmp = [0.0f32; 512];
        loop {
            let k = self.tap.pop_slice(&mut tmp);
            if k == 0 {
                break;
            }
            self.tap_nonzero += tmp[..k].iter().filter(|x| **x != 0.0).count();
        }
        self.frames_written += n as u64;
        let mut nominal = self.origin_us + self.frames_written as f64 * 1e6 / r;
        if let Some((gs, gl)) = self.spec.gap {
            if !self.gap_done && nominal >= gs {
                self.gap_done = true;
                self.origin_us += gl;
                nominal += gl;
            }
        }
        let jit = match self.spec.jitter {
            Jitter::None => 0.0,
            Jitter::Uniform(a) => (self.rng.f() * 2.0 - 1.0) * a,
            Jitter::Late(a) => self.rng.f() * a,
        };
        self.next_cb_us = (nominal + jit).max(tau + 1.0);
    }

    /// 워커 반복(참 시각 `tau`): 콜백 시각 추정 → 스케줄.
    fn poll(&mut self, tau: f64, gen: u64, plan: &MetronomePlan, offset: Option<f64>) {
        let wall = self.sys_us(tau).max(0.0) as u64;
        let t = self.tracker.observe(&self.counters, wall, self.spec.buffer, self.spec.rate);
        if t.cb_seq.is_none() {
            return;
        }
        let extra_out_us = match self.spec.comp {
            Comp::None => 0.0,
            Comp::Driver => self.spec.hidden_out_us,
            // 루프백이 재는 보정값 = 숨은 입력 + 숨은 출력(+ 측정 오차). 엔진과 같은 함수로 출력 몫을 나눈다.
            Comp::Loopback { hidden_in_us, measure_err_us } => {
                measured_hidden_out_us((hidden_in_us + self.spec.hidden_out_us + measure_err_us) / 1000.0, None)
            }
        };
        let clock = OutClock { dev_now_us: t.dev_now_us as f64, played: t.played, rate: t.out_rate as f64, burst: t.out_burst as u64, extra_out_us };
        self.sched.tick(gen, plan, offset, &clock, &self.click);
    }
}

struct Scenario {
    host: PeerSpec,
    follower: PeerSpec,
    /// 참 시각 τ에서의 기본 편도 지연(µs).
    one_way: fn(f64) -> f64,
    queue_mean_us: f64,
    bpm: f64,
    secs: f64,
    /// 정렬 오차에서 뺄 참 시각 구간(µs): 콜백 공백 직후 재수렴 중.
    exclude: Option<(f64, f64)>,
}

struct Outcome {
    max_err_ms: f64,
    mean_err_ms: f64,
    beats: usize,
    host_clicks: usize,
    late: u64,
    skipped: u64,
    tap_nonzero: usize,
}

fn run(sc: &Scenario) -> Outcome {
    let mut rng = Rng(0xDEAD_BEEF_1234_5678);
    let mut a = SimPeer::new(sc.host, 0x1234_5678_9ABC_DEF1);
    let mut b = SimPeer::new(sc.follower, 0x0FED_CBA9_8765_4321);
    let mut clock = ClockSync::new();
    // 방장: 2초에 시작을 누름 → 박 0 = 방장 시계로 지금 + 300ms
    let start_tau = 2_000_000.0;
    let plan_a = MetronomePlan { running: true, bpm: sc.bpm, beats_per_bar: 4, start_us: a.sys_us(start_tau) + 300_000.0, ref_peer_id: None };
    let plan_b = MetronomePlan { ref_peer_id: Some("host".into()), ..plan_a.clone() };
    // (도착 참 시각, t0(따르는 쪽 시계), 방장 시계)
    let mut pongs: VecDeque<(f64, f64, f64)> = VecDeque::new();
    let mut next_ping = 100_000.0;
    let end = sc.secs * 1e6;
    let mut tau;
    loop {
        let next_pong = pongs.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
        tau = [a.next_cb_us, b.next_cb_us, a.next_poll_us, b.next_poll_us, next_ping, next_pong].into_iter().fold(f64::INFINITY, f64::min);
        if tau > end {
            break;
        }
        if tau == a.next_cb_us {
            a.callback(tau);
        } else if tau == b.next_cb_us {
            b.callback(tau);
        } else if tau == a.next_poll_us {
            if tau >= start_tau {
                a.poll(tau, 1, &plan_a, Some(0.0));
            } else {
                a.poll(tau, 1, &MetronomePlan::default(), Some(0.0));
            }
            a.next_poll_us = tau + 500.0 + rng.f() * 700.0;
        } else if tau == b.next_poll_us {
            let now_b = b.sys_us(tau);
            let off = clock.estimate(now_b).map(|e| e.offset_us);
            if tau >= start_tau + 150_000.0 {
                // 시그널링이 150ms 뒤 도착
                b.poll(tau, 1, &plan_b, off);
            } else {
                b.poll(tau, 1, &MetronomePlan::default(), off);
            }
            b.next_poll_us = tau + 500.0 + rng.f() * 700.0;
        } else if tau == next_ping {
            let d1 = (sc.one_way)(tau) + rng.exp(sc.queue_mean_us);
            let remote = a.sys_us(tau + d1);
            let d2 = (sc.one_way)(tau + d1) + rng.exp(sc.queue_mean_us);
            pongs.push_back((tau + d1 + d2, b.sys_us(tau), remote));
            next_ping = tau + 500_000.0;
        } else {
            let i = pongs.iter().position(|p| p.0 == next_pong).unwrap();
            let (arr, t0, remote) = pongs.remove(i).unwrap();
            clock.on_sample(t0, b.sys_us(arr), remote);
        }
    }
    // 같은 박끼리 짝짓기(가장 가까운 것, 박 간격의 절반 안)
    let half = 30e6 / sc.bpm;
    let mut errs = Vec::new();
    let excluded = |t: f64| sc.exclude.is_some_and(|(s, e)| t >= s && t < e);
    let mut host_clicks = 0;
    for &ta in a.onsets.iter().filter(|t| !excluded(**t)) {
        host_clicks += 1;
        if let Some(tb) = b.onsets.iter().copied().min_by(|x, y| (x - ta).abs().total_cmp(&(y - ta).abs())) {
            if (tb - ta).abs() < half {
                errs.push((tb - ta) / 1000.0);
            }
        }
    }
    let max_err_ms = errs.iter().fold(0.0f64, |m, e| m.max(e.abs()));
    let mean_err_ms = errs.iter().sum::<f64>() / errs.len().max(1) as f64;
    Outcome {
        max_err_ms,
        mean_err_ms,
        beats: errs.len(),
        host_clicks,
        late: a.click.late.load(Ordering::Relaxed) + b.click.late.load(Ordering::Relaxed),
        skipped: a.click.skipped.load(Ordering::Relaxed) + b.click.skipped.load(Ordering::Relaxed),
        tap_nonzero: a.tap_nonzero + b.tap_nonzero,
    }
}

fn spec(sys_ppm: f64, sys_offset_us: f64, rate: u32, dev_ppm: f64, buffer: usize, phase_us: f64) -> PeerSpec {
    PeerSpec {
        sys_ppm,
        sys_offset_us,
        rate,
        dev_ppm,
        buffer,
        phase_us,
        jitter: Jitter::None,
        alt_burst: false,
        gap: None,
        hidden_out_us: 0.0,
        comp: Comp::None,
    }
}

fn report(name: &str, o: &Outcome) {
    println!(
        "{name}: 박 {} / 방장 클릭 {}, 정렬 오차 최대 {:.3}ms 평균 {:+.3}ms, 늦은 클릭 {}, 건너뜀 {}",
        o.beats, o.host_clicks, o.max_err_ms, o.mean_err_ms, o.late, o.skipped
    );
}

fn check(name: &str, sc: &Scenario) -> Outcome {
    let o = run(sc);
    report(name, &o);
    let excluded_s = sc.exclude.map_or(0.0, |(s, e)| (e - s) / 1e6);
    let expected = ((sc.secs - 2.3 - excluded_s) * sc.bpm / 60.0) as usize;
    assert!(o.host_clicks + 2 >= expected, "{name}: 방장 클릭 {} < 기대 {expected}", o.host_clicks);
    assert!(o.beats + 3 >= o.host_clicks, "{name}: 짝지은 박 {} / {}", o.beats, o.host_clicks);
    assert!(o.max_err_ms <= 2.0, "{name}: 정렬 오차 {:.3}ms > 2ms", o.max_err_ms);
    if sc.exclude.is_none() {
        assert_eq!(o.late, 0, "{name}: 늦은 클릭");
        assert_eq!(o.skipped, 0, "{name}: 건너뛴 클릭");
    }
    assert_eq!(o.tap_nonzero, 0, "{name}: 믹스 탭에 클릭이 섞임");
    o
}

#[test]
fn two_peers_align_within_2ms_same_setup() {
    let _ = check(
        "같은 설정",
        &Scenario {
            host: spec(0.0, 5e9, 48_000, 0.0, 128, 0.0),
            follower: spec(0.0, 3e6, 48_000, 0.0, 128, 1234.0),
            one_way: |_| 5000.0,
            queue_mean_us: 300.0,
            bpm: 120.0,
            secs: 60.0,
            exclude: None,
        },
    );
}

#[test]
fn two_peers_align_within_2ms_with_drift_and_different_devices() {
    // 시스템 시계 ±200ppm(서로 400ppm 차), 장치 클럭도 따로 어긋남, 레이트·버퍼가 다름
    for (sa, sb) in [(200.0, -200.0), (-200.0, 200.0)] {
        let _ = check(
            &format!("드리프트 {sa:+}/{sb:+}ppm"),
            &Scenario {
                host: spec(sa, 1.7e9, 48_000, 80.0, 128, 0.0),
                follower: spec(sb, 42e9, 44_100, -150.0, 441, 777.0),
                one_way: |_| 8000.0,
                queue_mean_us: 800.0,
                bpm: 137.0,
                secs: 180.0,
                exclude: None,
            },
        );
    }
}

#[test]
fn two_peers_align_within_2ms_across_rtt_changes() {
    // 편도 3ms → 30초에 15ms → 80초에 4ms(대칭 경로 변화), 드리프트 200ppm
    let _ = check(
        "RTT 변화",
        &Scenario {
            host: spec(100.0, 0.0, 48_000, 30.0, 64, 0.0),
            follower: spec(-100.0, 9e8, 96_000, -60.0, 256, 3333.0),
            one_way: |t| if t < 30e6 { 3000.0 } else if t < 80e6 { 15_000.0 } else { 4000.0 },
            queue_mean_us: 500.0,
            bpm: 200.0,
            secs: 120.0,
            exclude: None,
        },
    );
}

#[test]
fn two_peers_align_within_2ms_with_callback_jitter() {
    // PR #34 M1: 따르는 쪽만 콜백 시각이 공칭 ±1ms 균등으로 흔들리고, 44.1k 441/440 버스트 교대.
    // DeviceClock은 창 최솟값(가장 이른 콜백)을 쓰므로 대칭 지터는 그 진폭만큼 이르게 치우친다(최악 경우).
    let mut follower = spec(-100.0, 42e9, 44_100, -150.0, 441, 777.0);
    follower.jitter = Jitter::Uniform(1000.0);
    follower.alt_burst = true;
    let _ = check(
        "콜백 지터 ±1ms(한쪽) + 441/440 교대",
        &Scenario {
            host: spec(100.0, 1.7e9, 48_000, 80.0, 128, 0.0),
            follower,
            one_way: |_| 8000.0,
            queue_mean_us: 800.0,
            bpm: 137.0,
            secs: 120.0,
            exclude: None,
        },
    );
    // 이벤트 구동 콜백의 현실적 모양: 공칭보다 이르지 않고 0~2ms 늦음(양쪽 다)
    let mut host = spec(100.0, 1.7e9, 48_000, 80.0, 128, 0.0);
    host.jitter = Jitter::Late(2000.0);
    let mut follower = spec(-100.0, 42e9, 48_000, -150.0, 480, 777.0);
    follower.jitter = Jitter::Late(2000.0);
    let _ = check(
        "콜백 지터 0~2ms 늦음(양쪽)",
        &Scenario { host, follower, one_way: |_| 8000.0, queue_mean_us: 800.0, bpm: 137.0, secs: 120.0, exclude: None },
    );
}

#[test]
fn hidden_output_latency_is_compensated_when_reported() {
    // PR #34 M2: 따르는 쪽 장치에 버퍼 1개 너머 10ms 숨은 출력 지연(WASAPI 공유 엔진 주기 등).
    let sc = |comp: Comp| {
        let mut follower = spec(0.0, 3e6, 48_000, 0.0, 128, 1234.0);
        follower.hidden_out_us = 10_000.0;
        follower.comp = comp;
        Scenario {
            host: spec(0.0, 5e9, 48_000, 0.0, 128, 0.0),
            follower,
            one_way: |_| 5000.0,
            queue_mean_us: 300.0,
            bpm: 120.0,
            secs: 30.0,
            exclude: None,
        }
    };
    let _ = check("숨은 출력 지연 10ms, 드라이버 보고 → 보정", &sc(Comp::Driver));
    // 보고하지 않고 루프백 측정도 없으면(cpal WASAPI) 그 지연이 그대로 정렬 오차가 된다: 통계로 알린다
    let o = run(&sc(Comp::None));
    report("숨은 출력 지연 10ms, 보고 없음(보정 안 됨)", &o);
    assert!((o.mean_err_ms - 10.0).abs() < 1.0, "보정 안 되면 +10ms 어긋남: {:+.3}", o.mean_err_ms);
    // PR #34 M2(a): 보고 없음 + 루프백 보정값(숨은 입력 + 숨은 출력)을 반으로 나눠 출력 쪽에 반영.
    // 측정 오차 0.3ms(루프백 σ 수준)를 더한다.
    let o = check(
        "숨은 출력 지연 10ms, 보고 없음 + 루프백 보정(입력 숨은 10ms, 대칭)",
        &sc(Comp::Loopback { hidden_in_us: 10_000.0, measure_err_us: 300.0 }),
    );
    assert!(o.mean_err_ms.abs() < 0.5, "{:+.3}", o.mean_err_ms);
    let o = check(
        "숨은 출력 지연 10ms, 보고 없음 + 루프백 보정(입력 숨은 8ms, 약한 비대칭)",
        &sc(Comp::Loopback { hidden_in_us: 8_000.0, measure_err_us: 300.0 }),
    );
    assert!((o.mean_err_ms - 0.85).abs() < 0.5, "남는 오차 ≈ (출력 − 입력 − 측정 오차)/2: {:+.3}", o.mean_err_ms);
    // 크게 비대칭이면 (숨은 출력 − 숨은 입력)/2가 남는다(한계, 설계 1.4절). 그래도 보정 안 함(+10ms)보다 작다.
    let o = run(&sc(Comp::Loopback { hidden_in_us: 2_000.0, measure_err_us: 300.0 }));
    report("숨은 출력 지연 10ms, 보고 없음 + 루프백 보정(입력 숨은 2ms, 강한 비대칭)", &o);
    assert!((o.mean_err_ms - 3.85).abs() < 0.5, "{:+.3}", o.mean_err_ms);
}

#[test]
fn both_unreported_peers_align_with_loopback_offsets() {
    // 두 피어 모두 드라이버 보고 없음(cpal WASAPI), 숨은 지연이 서로 다름: 방장 대칭 4+4ms, 따르는 쪽 대칭 12+12ms.
    // 보정 없이는 8ms 어긋나고, 각자 루프백 보정값을 넣으면 측정 오차 차이만 남는다.
    let mut host = spec(80.0, 5e9, 48_000, 20.0, 256, 0.0);
    host.hidden_out_us = 4_000.0;
    let mut follower = spec(-60.0, 7e8, 44_100, -30.0, 441, 2222.0);
    follower.hidden_out_us = 12_000.0;
    let sc = |h: Comp, f: Comp| Scenario {
        host: PeerSpec { comp: h, ..host },
        follower: PeerSpec { comp: f, ..follower },
        one_way: |_| 7000.0,
        queue_mean_us: 500.0,
        bpm: 100.0,
        secs: 60.0,
        exclude: None,
    };
    let o = run(&sc(Comp::None, Comp::None));
    report("둘 다 보고 없음(숨은 4ms ↔ 12ms), 보정 안 됨", &o);
    assert!((o.mean_err_ms - 8.0).abs() < 1.0, "{:+.3}", o.mean_err_ms);
    let _ = check(
        "둘 다 보고 없음(숨은 4ms ↔ 12ms), 각자 루프백 보정",
        &sc(Comp::Loopback { hidden_in_us: 4_000.0, measure_err_us: -200.0 }, Comp::Loopback { hidden_in_us: 12_000.0, measure_err_us: 300.0 }),
    );
}

#[test]
fn follower_recovers_quickly_after_callback_gap() {
    // PR #34 M3: 따르는 쪽 장치가 30초에 300ms 멈췄다가 재개. 공백 감지로 0.6초 안에 다시 2ms 안으로.
    let mut follower = spec(-100.0, 9e8, 48_000, -60.0, 256, 3333.0);
    follower.jitter = Jitter::Late(1000.0);
    follower.gap = Some((30e6, 300_000.0));
    let o = check(
        "콜백 공백 300ms(재개 0.6초 뒤부터)",
        &Scenario {
            host: spec(100.0, 0.0, 48_000, 30.0, 128, 0.0),
            follower,
            one_way: |_| 6000.0,
            queue_mean_us: 500.0,
            bpm: 120.0,
            secs: 60.0,
            exclude: Some((30e6 - 100_000.0, 30e6 + 300_000.0 + 600_000.0)),
        },
    );
    assert!(o.skipped <= 2, "공백 동안 건너뛴 클릭 {}", o.skipped);
}

// ============================================================================
// 계획 세대 변경(#143 리뷰 L-5): 박 직전에 계획이 바뀌어도 새 계획의 박이면 울린다
// ============================================================================

/// 방장 하나(시스템 시계 = 참 시간, 48k/128)를 돌리며 참 시각 `changes[i].0`마다 계획 세대를 올리고 `changes[i].1`로 바꾼다.
/// 반환: (클릭이 귀에 닿은 참 시각들, 늦은 클릭, 건너뛴 클릭).
fn run_host_with_plan_changes(first: MetronomePlan, changes: &[(f64, MetronomePlan)], end_us: f64) -> (Vec<f64>, u64, u64) {
    let mut rng = Rng(0x5EED_0000_0143_0005);
    let mut a = SimPeer::new(spec(0.0, 0.0, 48_000, 0.0, 128, 1_000.0), 0x0143_0005_ABCD_EF01);
    let (mut gen, mut plan) = (1, first);
    let mut next = 0;
    loop {
        let tau = a.next_cb_us.min(a.next_poll_us);
        if tau > end_us {
            break;
        }
        if tau == a.next_cb_us {
            a.callback(tau);
            continue;
        }
        while next < changes.len() && changes[next].0 <= tau {
            gen += 1;
            plan = changes[next].1.clone();
            next += 1;
        }
        a.poll(tau, gen, &plan, Some(0.0));
        a.next_poll_us = tau + 500.0 + rng.f() * 700.0;
    }
    assert_eq!(a.tap_nonzero, 0, "믹스 탭에 클릭");
    (a.onsets, a.click.late.load(Ordering::Relaxed), a.click.skipped.load(Ordering::Relaxed))
}

const L5_START_US: f64 = 1_000_000.0;

fn l5_plan(bpm: f64, beats_per_bar: u32) -> MetronomePlan {
    MetronomePlan { running: true, bpm, beats_per_bar, start_us: L5_START_US, ref_peer_id: None }
}

/// 클릭 시각들이 기대 박 시각들과 하나씩 짝지어지는지(±0.1ms).
fn assert_onsets(name: &str, onsets: &[f64], expected: &[f64]) {
    let got: Vec<String> = onsets.iter().map(|t| format!("{:.1}", (t - L5_START_US) / 1000.0)).collect();
    println!("{name}: 클릭(박 0 기준 ms) {got:?}");
    assert_eq!(onsets.len(), expected.len(), "{name}: 클릭 수 — 받은 {got:?}");
    for (i, (t, e)) in onsets.iter().zip(expected).enumerate() {
        assert!((t - e).abs() < 100.0, "{name}: {i}번째 클릭 {:.3}ms, 기대 {:.3}ms", (t - L5_START_US) / 1000.0, (e - L5_START_US) / 1000.0);
    }
}

#[test]
fn same_plan_resent_just_before_beat_keeps_that_beat() {
    // 박 k(1..=19)의 귀 시각 k ms 전에 같은 계획을 다시 넣는다. 1~12ms 전은 클릭이 이미 가드(콜백 2개 + 2ms ≈ 12.7ms) 안이다.
    // 전에는 세대가 바뀌면 그 클릭을 거두고 다음 박부터 올려 5~12ms 쪽 박(박 5~12)이 빠졌다(1~4ms 전이면 콜백이 이미 가져감).
    let p = l5_plan(120.0, 4);
    let period = p.period_us();
    let changes: Vec<_> = (1..=19u64).map(|k| (L5_START_US + k as f64 * period - k as f64 * 1000.0, p.clone())).collect();
    let (onsets, late, skipped) = run_host_with_plan_changes(p, &changes, L5_START_US + 19.5 * period);
    let expected: Vec<f64> = (0..=19u64).map(|k| L5_START_US + k as f64 * period).collect();
    assert_onsets("같은 계획 재전송", &onsets, &expected);
    assert_eq!((late, skipped), (0, 0));
}

#[test]
fn grid_preserving_plan_change_keeps_beat_and_takes_new_accent() {
    // 박마다 8ms 전(가드 안)에 박자만 4 ↔ 3으로 바꾼다(박 격자는 같음): 박은 빠지지 않는다. 강박 비트는 스케줄러 단위 시험이 본다.
    let period = l5_plan(120.0, 4).period_us();
    let changes: Vec<_> = (1..=9u64).map(|k| (L5_START_US + k as f64 * period - 8000.0, l5_plan(120.0, if k % 2 == 1 { 3 } else { 4 }))).collect();
    let (onsets, late, _) = run_host_with_plan_changes(l5_plan(120.0, 4), &changes, L5_START_US + 9.5 * period);
    let expected: Vec<f64> = (0..=9u64).map(|k| L5_START_US + k as f64 * period).collect();
    assert_onsets("박자 변경", &onsets, &expected);
    assert_eq!(late, 0);
}

#[test]
fn plan_change_just_before_beat_drops_it_only_if_not_a_new_beat() {
    let p120 = l5_plan(120.0, 4);
    // 박 4(2000ms) 5ms 전에 BPM 90(667ms 간격)으로: 2000ms는 새 계획의 박 3 → 울리고 그 뒤는 새 간격.
    let p90 = l5_plan(90.0, 4);
    let at = L5_START_US + 2_000_000.0 - 5000.0;
    let (onsets, late, _) = run_host_with_plan_changes(p120.clone(), &[(at, p90.clone())], L5_START_US + 3_400_000.0);
    let mut expected: Vec<f64> = (0..=4u64).map(|k| L5_START_US + k as f64 * 500_000.0).collect();
    expected.extend((4..=5u64).map(|k| L5_START_US + k as f64 * p90.period_us()));
    assert_onsets("BPM 90(박 겹침)", &onsets, &expected);
    assert_eq!(late, 0);
    // BPM 100(600ms 간격): 2000ms는 새 계획의 박이 아니다 → 거두고 새 계획의 다음 박 2400ms부터.
    let (onsets, late, _) = run_host_with_plan_changes(p120.clone(), &[(at, l5_plan(100.0, 4))], L5_START_US + 3_300_000.0);
    let mut expected: Vec<f64> = (0..=3u64).map(|k| L5_START_US + k as f64 * 500_000.0).collect();
    expected.extend([2_400_000.0, 3_000_000.0].map(|t| L5_START_US + t));
    assert_onsets("BPM 100(박 안 겹침)", &onsets, &expected);
    assert_eq!(late, 0);
    // 정지: 박 4 5ms 전에 멈추면 박 4는 울리지 않는다. 100ms 뒤 같은 계획으로 다시 켜면 박 5부터.
    let stopped = MetronomePlan { running: false, ..p120.clone() };
    let (onsets, _, _) = run_host_with_plan_changes(p120.clone(), &[(at, stopped), (at + 100_000.0, p120.clone())], L5_START_US + 2_700_000.0);
    let expected: Vec<f64> = [0u64, 1, 2, 3, 5].iter().map(|&k| L5_START_US + k as f64 * 500_000.0).collect();
    assert_onsets("정지 후 재시작", &onsets, &expected);
}

// ============================================================================
// 시계 추정 계단(#168 리뷰 L-3): 곧 울릴 박 근처에서 기준 시계 추정이 뛰어도 박이 빠지거나 두 번 울리지 않는다
// ============================================================================

/// 시계 계단 때 박을 제자리에서 어긋나게 울려도 되는 최대 폭(ms) — `sched.rs`의 `MAX_SHIFT_S`(20ms)와 같다.
/// 이보다 늦게밖에 울릴 수 없는 박은 빼고(`skippedClicks`) 다음 박부터 새 시계로 맞춘다.
const MAX_SHIFT_MS: f64 = 20.0;
/// 콜백이 클릭을 가져가는 때는 귀 시각보다 최대 콜백 2개(128프레임 × 2 @48k = 5.33ms) 앞이다. 계단 시각에서 이만큼 안의 박은
/// 계단 전에 이미 가져갔을 수 있다(옛 시계 자리에 울려도 된다).
const TAKEN_BEFORE_MS: f64 = 5.4;

/// 방장 하나(48k/128)가 오프셋 0으로 돌다가 참 시각 `at`에 오프셋을 `jump_us`로 바꾼다. `regen`이면 같은 시각에 계획 세대도 올린다
/// (기준 피어 변경·계획 재전송과 겹친 경우 — 계획 내용은 같다). 반환: (클릭 귀 시각들, 늦은 클릭, 건너뛴 클릭).
fn run_offset_step(at: f64, jump_us: f64, regen: bool, end_us: f64) -> (Vec<f64>, u64, u64) {
    let mut rng = Rng(0x5EED_0000_0168_0003);
    let mut a = SimPeer::new(spec(0.0, 0.0, 48_000, 0.0, 128, 1_000.0), 0x0168_0003_ABCD_EF01);
    let p = l5_plan(120.0, 4);
    loop {
        let tau = a.next_cb_us.min(a.next_poll_us);
        if tau > end_us {
            break;
        }
        if tau == a.next_cb_us {
            a.callback(tau);
            continue;
        }
        let stepped = tau >= at;
        a.poll(tau, if regen && stepped { 2 } else { 1 }, &p, Some(if stepped { jump_us } else { 0.0 }));
        a.next_poll_us = tau + 500.0 + rng.f() * 700.0;
    }
    assert_eq!(a.tap_nonzero, 0, "믹스 탭에 클릭");
    (a.onsets, a.click.late.load(Ordering::Relaxed), a.click.skipped.load(Ordering::Relaxed))
}

/// 계단 하나의 결과를 규칙에 대 본다. 어긋나면 이유 목록.
/// - 박마다 클릭은 많아야 하나(이중 클릭 없음), 클릭 간격 ≥ 박 간격의 절반.
/// - 계단 전에 가져갔을 수 있는 박(옛 귀 시각 ≤ 계단 + 5.4ms)은 옛 자리, 그 밖에는 새 자리(±0.1ms).
/// - 계단 뒤 첫 박(경계 박)만 새 자리에서 min(|점프|, 20ms)까지 어긋나도 된다.
/// - 빠져도 되는 박은 경계 박 하나, 그것도 점프가 20ms를 넘어 박이 당겨질 때(새 자리가 이미 20ms 넘게 지남)뿐이다. 그때 건너뜀 1.
fn step_violations(at: f64, jump_us: f64, onsets: &[f64], late: u64, skipped: u64, last_beat: u64) -> Vec<String> {
    let period = l5_plan(120.0, 4).period_us();
    let old = |k: u64| L5_START_US + k as f64 * period;
    let new = |k: u64| old(k) - jump_us;
    let boundary = (0..).find(|&k| old(k) > at).unwrap();
    let mut bad = Vec::new();
    if late != 0 {
        bad.push(format!("늦은 클릭 {late}"));
    }
    for w in onsets.windows(2) {
        if w[1] - w[0] < period / 2.0 {
            bad.push(format!("이중 클릭: {:.1}ms, {:.1}ms", (w[0] - L5_START_US) / 1000.0, (w[1] - L5_START_US) / 1000.0));
        }
    }
    let mut missing = 0;
    for k in 0..=last_beat {
        let hits: Vec<f64> = onsets.iter().copied().filter(|t| ((t - L5_START_US) / period).round() as i64 == k as i64).collect();
        let Some(&t) = hits.first() else {
            missing += 1;
            if !(k == boundary && jump_us > MAX_SHIFT_MS * 1000.0) {
                bad.push(format!("박 {k} 빠짐"));
            }
            continue;
        };
        let at_old = (t - old(k)).abs() < 100.0 && old(k) <= at + TAKEN_BEFORE_MS * 1000.0;
        let at_new = (t - new(k)).abs() < 100.0 && old(k) > at;
        let shifted = k == boundary && (t - new(k)).abs() <= jump_us.abs().min(MAX_SHIFT_MS * 1000.0) + 100.0;
        if !(at_old || at_new || shifted) {
            bad.push(format!("박 {k}: {:.2}ms(옛 {:.2}, 새 {:.2})", (t - L5_START_US) / 1000.0, (old(k) - L5_START_US) / 1000.0, (new(k) - L5_START_US) / 1000.0));
        }
    }
    if skipped != missing {
        bad.push(format!("건너뜀 {skipped} ≠ 빠진 박 {missing}"));
    }
    bad
}

#[test]
fn clock_step_near_beat_neither_drops_nor_doubles() {
    // 박 4(2000ms)의 d ms 전에 기준 시계 추정이 J만큼 뛴다(J > 0: 박이 당겨짐, J < 0: 밀림). 같은 세대(동기 갱신)와 세대 변경(기준 피어
    // 변경·재전송과 겹침) 둘 다. d는 콜백이 이미 가져갔을 구간(1·3·5ms), 가드(콜백 2개 + 2ms ≈ 7.3ms) 안팎, 먼 박까지 훑는다.
    // 전에는: 같은 세대에서 가드 바로 밖 클릭이 5ms 넘게 당겨지면 빠졌고(+10ms), 가드 안 클릭은 옛 자리에 울렸으며(−50ms면 50ms 일찍),
    // 세대 변경 때는 1ms 넘게 움직인 곧 울릴 박이 빠지거나(+2ms) 이미 울린 박이 다시 울렸다(−10ms).
    let period = l5_plan(120.0, 4).period_us();
    let beat4 = L5_START_US + 4.0 * period;
    let end = L5_START_US + 8.5 * period;
    let mut failures = Vec::new();
    let mut cases = 0;
    for regen in [false, true] {
        for jump_ms in [0.5, -0.5, 2.0, -2.0, 10.0, -10.0, 50.0, -50.0] {
            for d_ms in [1.0, 3.0, 5.0, 6.5, 8.0, 9.5, 12.0, 15.0, 20.0, 30.0, 45.0, 60.0, 100.0] {
                cases += 1;
                let at = beat4 - d_ms * 1000.0;
                let (onsets, late, skipped) = run_offset_step(at, jump_ms * 1000.0, regen, end);
                let bad = step_violations(at, jump_ms * 1000.0, &onsets, late, skipped, 8);
                if !bad.is_empty() {
                    failures.push(format!("{} J={jump_ms:+}ms d={d_ms}ms: {}", if regen { "세대" } else { "동기" }, bad.join("; ")));
                }
            }
        }
    }
    for f in &failures {
        println!("{f}");
    }
    assert!(failures.is_empty(), "{}/{cases} 경우가 규칙을 어김:\n{}", failures.len(), failures.join("\n"));
}

// ============================================================================
// 실제 메트로놈 단계(`MetronomeStage`, #143 후속): 계획 세대·피어 표 복사본·`Peer.clock_estimate` 경로로 같은 규칙
// ============================================================================

/// `MetronomeStageProbe`(test-support)를 타는 방장 아닌 연주자 하나. 내 시스템 시계 = 참 시각, 48k/128.
/// 피어 `host`·`host2`의 시계 = 참 시각 + 40ms(+ `host2`는 5초 + `err_us`). 500ms마다 둘 다에게 Ping(왕복 200µs, 대칭).
/// 기준 피어 `host`로 시작해 참 시각 `at`에 기준 피어를 `host2`로 바꾼다(`startUs`도 host2 시계로 — 의도한 박 격자는 같다).
/// `host2` 시계 추정이 `err_us`만큼 어긋나 있으면 내 시계에서 새 박 자리가 그만큼 당겨진다(점프 J = `err_us`).
fn run_stage_ref_change(at: f64, err_us: f64, end_us: f64) -> (Vec<f64>, u64, u64) {
    const D_US: f64 = 5_000_000.0;
    let mut probe = MetronomeStageProbe::new(&["host", "host2"], 48_000, 128).unwrap();
    let mut a = SimPeer::with_click(spec(0.0, 0.0, 48_000, 0.0, 128, 1_000.0), 0x0168_0003_5747_0001, probe.click());
    let mut rng = Rng(0x5EED_0000_0168_5747);
    let host = |tau: f64| tau + 40_000.0;
    let host2 = |tau: f64| tau + 40_000.0 + D_US + err_us;
    let p1 = MetronomePlan { ref_peer_id: Some("host".into()), start_us: host(L5_START_US), ..l5_plan(120.0, 4) };
    let p2 = MetronomePlan { ref_peer_id: Some("host2".into()), start_us: host(L5_START_US) + D_US, ..p1.clone() };
    probe.ctl().set_plan(p1);
    let mut switched = false;
    let mut next_ping = 0.0;
    loop {
        let tau = a.next_cb_us.min(a.next_poll_us).min(next_ping);
        if tau > end_us {
            break;
        }
        if tau == a.next_cb_us {
            a.callback(tau);
        } else if tau == next_ping {
            // 표본은 바로 넣는다(t3 = t0 + 200µs가 아직 미래여도 추정기는 시각만 본다)
            assert!(probe.clock_sample("host", tau, tau + 200.0, host(tau + 100.0)));
            assert!(probe.clock_sample("host2", tau, tau + 200.0, host2(tau + 100.0)));
            next_ping = tau + 500_000.0;
        } else {
            if !switched && tau >= at {
                probe.ctl().set_plan(p2.clone());
                switched = true;
            }
            let wall = a.sys_us(tau).max(0.0) as u64;
            let t = a.tracker.observe(&a.counters, wall, a.spec.buffer, a.spec.rate);
            probe.service(&t);
            a.next_poll_us = tau + 500.0 + rng.f() * 700.0;
        }
    }
    assert_eq!(a.tap_nonzero, 0, "믹스 탭에 클릭");
    (a.onsets, a.click.late.load(Ordering::Relaxed), a.click.skipped.load(Ordering::Relaxed))
}

#[test]
fn stage_ref_peer_change_with_clock_error_neither_drops_nor_doubles() {
    // 실제 단계: 기준 피어가 바뀌는 순간(새 세대) 새 기준 시계 추정이 J만큼 어긋나 있다. 박 4 d ms 전에 바꾼다.
    // 규칙은 스케줄러 시험(`clock_step_near_beat_neither_drops_nor_doubles`)과 같다.
    let period = l5_plan(120.0, 4).period_us();
    let beat4 = L5_START_US + 4.0 * period;
    let end = L5_START_US + 8.5 * period;
    let mut failures = Vec::new();
    let mut cases = 0;
    for jump_ms in [0.0, 0.5, -0.5, 2.0, -2.0, 10.0, -10.0, 50.0, -50.0] {
        for d_ms in [1.0, 3.0, 5.0, 6.5, 8.0, 9.5, 12.0, 15.0, 20.0, 30.0, 45.0, 60.0, 100.0] {
            cases += 1;
            let at = beat4 - d_ms * 1000.0;
            let (onsets, late, skipped) = run_stage_ref_change(at, jump_ms * 1000.0, end);
            let bad = step_violations(at, jump_ms * 1000.0, &onsets, late, skipped, 8);
            if !bad.is_empty() {
                failures.push(format!("단계 J={jump_ms:+}ms d={d_ms}ms: {}", bad.join("; ")));
            }
        }
    }
    for f in &failures {
        println!("{f}");
    }
    assert!(failures.is_empty(), "{}/{cases} 경우가 규칙을 어김:\n{}", failures.len(), failures.join("\n"));
}

// ============================================================================
// 배선: 실제 엔진 2개(가짜 장치, 로컬 UDP) — Pong 시계 표본 → 오프셋 → 클릭
// ============================================================================

mod engines {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use band_dsp::config::{CodecKind, EngineConfig};
    use band_engine::testing::engine::device::fake::FakeBackend;
    use band_engine::testing::engine::metronome::MetronomeConfig;
    use band_dsp::mixer::MixControls;
    use band_dsp::types::PeerStateKind;
    use band_engine::testing::engine::Engine;
    use band_engine::testing::net::peer::{Candidate, CandidateKind, LocalDescriptor};
    use band_engine::testing::net::{clock_payload, parse_clock_payload, NetOptions};

    fn silent() -> FakeBackend {
        FakeBackend::new(48_000, Arc::new(|_: u64, _: usize, buf: &mut [f32]| buf.fill(0.0)))
    }

    fn start(id: &str, backend: FakeBackend) -> Engine {
        let net = NetOptions {
            port: 0,
            stun_servers: vec![],
            gather_lan: false,
            extra_candidates: vec![],
            allow_loopback_candidates: true,
            cancel: None,
        };
        let cfg = EngineConfig { buffer_size: 128, frame_ms: 2.5, codec: CodecKind::Opus, channels: 1, input_channels: vec![0], ..Default::default() };
        Engine::start(cfg, Arc::new(backend), Arc::new(MixControls::default()), Some((id.into(), net))).unwrap()
    }

    fn desc_at(e: &Engine) -> LocalDescriptor {
        let addr: SocketAddr = format!("127.0.0.1:{}", e.local_port().unwrap()).parse().unwrap();
        LocalDescriptor { candidates: vec![Candidate { kind: CandidateKind::Lan, addr: addr.to_string() }], ..e.descriptor().unwrap().clone() }
    }

    fn wait(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < timeout {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn clock_payload_roundtrip_and_old_engine_compat() {
        let b = clock_payload(0x0102_0304_0506_0708);
        assert_eq!(parse_clock_payload(&b), Some(0x0102_0304_0506_0708));
        assert_eq!(parse_clock_payload(&[]), None, "옛 엔진의 빈 Pong");
        assert_eq!(parse_clock_payload(&b[..8]), None);
        assert_eq!(parse_clock_payload(&[0xEC, 0, 0, 0, 0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn follower_syncs_to_host_clock_and_both_click() {
        let host_dev = silent();
        let fol_dev = silent();
        let fol_sink = fol_dev.sink.clone();
        let host = start("host", host_dev);
        std::thread::sleep(Duration::from_millis(50)); // 엔진 시계(epoch)를 일부러 어긋나게
        let fol = start("fol", fol_dev);
        host.add_peer("fol".into(), &desc_at(&fol)).unwrap();
        fol.add_peer("host".into(), &desc_at(&host)).unwrap();
        assert!(wait(Duration::from_secs(5), || [&host, &fol]
            .iter()
            .all(|e| e.stats().peers.iter().all(|p| p.state == PeerStateKind::Connected))));

        // 기준 피어를 따르면서 startUs를 비우면 에러
        assert!(fol
            .set_metronome(MetronomeConfig { running: true, bpm: 240.0, beats_per_bar: 4, start_us: None, ref_peer_id: Some("host".into()) })
            .is_err());
        let plan = host
            .set_metronome(MetronomeConfig { running: true, bpm: 240.0, beats_per_bar: 4, start_us: None, ref_peer_id: None })
            .unwrap();
        fol.set_metronome(MetronomeConfig {
            running: true,
            bpm: plan.bpm,
            beats_per_bar: plan.beats_per_bar,
            start_us: Some(plan.start_us),
            ref_peer_id: Some("host".into()),
        })
        .unwrap();
        assert!(
            wait(Duration::from_secs(10), || {
                let (h, f) = (host.stats().metronome, fol.stats().metronome);
                h.clicks >= 4 && f.clicks >= 4 && f.synced
            }),
            "클릭 없음: host {:?} fol {:?}",
            host.stats().metronome,
            fol.stats().metronome
        );
        // 오프셋(host − fol 시계)은 두 엔진 시계를 같은 순간에 읽은 차와 같아야 한다(로컬 UDP, 왕복 ≪ 1ms)
        let expected_ms = (host.now_us() - fol.now_us()) / 1000.0;
        let m = fol.stats().metronome;
        let got = m.clock_offset_ms.unwrap();
        assert!((got - expected_ms).abs() < 2.0, "오프셋 {got:.3}ms, 기대 {expected_ms:.3}ms");
        assert!(expected_ms > 40.0, "epoch 차가 반영돼야 함: {expected_ms}");
        assert!(fol_sink.lock().iter().any(|x| *x != 0.0), "따르는 쪽 출력에 클릭이 없음");

        // 정지하면 더 울리지 않는다
        host.set_metronome(MetronomeConfig { running: false, bpm: 240.0, beats_per_bar: 4, start_us: None, ref_peer_id: None }).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let c0 = host.stats().metronome.clicks;
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(host.stats().metronome.clicks, c0);
    }
}
