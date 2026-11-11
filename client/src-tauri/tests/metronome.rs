//! 공유 메트로놈(P2-3) 2피어 시뮬레이션 — 가상 시간, 결정적.
//!
//! 실제 구성요소를 그대로 쓴다: 출력 콜백(`OutputRt` + `ClickRt`), 워커의 콜백 시각 추정(`CallbackTracker`),
//! 시계 오프셋 추정(`ClockSync`), 박 스케줄러(`BeatScheduler`). 가상인 것은 시간·장치·회선뿐이다.
//!
//! 모델(설계 1.4절):
//! - 참 시간 τ. 피어 시스템 시계 = τ·(1+ppm) + 오프셋. 장치 레이트 = 공칭 × (1+장치 ppm)(참 시간 기준).
//! - 장치는 프레임 [b, b+n)을 쓰는 콜백을 직전 버퍼가 울리기 시작할 때 부르고, 그 버퍼는 n 프레임 뒤에 울린다.
//!   → 프레임 J가 귀에 닿는 참 시각 = τ_cb + (n + J − b)/레이트.
//! - 워커는 0.5~1.2ms마다 깨어 콜백 카운터를 본다(기상 지연 포함).
//! - 따르는 쪽이 500ms마다 방장에게 Ping, 편도 = 기본 + 지수 큐잉(방향별 독립).
//! - 정렬 오차 = 같은 박의 두 피어 클릭이 귀에 닿는 참 시각 차.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use band_lib::engine::metronome::{BeatScheduler, ClickRt, ClickShared, MetronomePlan, OutClock};
use band_lib::engine::mixer::MixControls;
use band_lib::engine::rt::{output_path, OutputRt};
use band_lib::engine::stats::AudioCounters;
use band_lib::engine::worker::CallbackTracker;
use band_lib::net::clock::ClockSync;
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
    next_cb_us: f64,
    frames_written: u64,
    next_poll_us: f64,
    last_nonzero: Option<u64>,
    /// 클릭이 귀에 닿은 참 시각들.
    onsets: Vec<f64>,
    tap_nonzero: usize,
}

impl SimPeer {
    fn new(spec: PeerSpec) -> Self {
        let counters = Arc::new(AudioCounters::default());
        let controls = Arc::new(MixControls::default());
        let (mut out, feed) = output_path(counters.clone(), controls);
        let click = Arc::new(ClickShared::default());
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
        let n = self.spec.buffer;
        let b = self.frames_written;
        self.out.process(&mut self.buf[..], 2);
        let r = self.true_rate();
        for i in 0..n {
            let x = self.buf[i * 2];
            if x != 0.0 {
                let j = b + i as u64;
                // 클릭 사이 간격(≥ 200ms)보다 짧은 무음 뒤는 같은 클릭의 꼬리
                if self.last_nonzero.is_none_or(|l| j - l > (self.spec.rate / 20) as u64) {
                    self.onsets.push(tau + (n + i) as f64 * 1e6 / r);
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
        self.next_cb_us = tau + n as f64 * 1e6 / r;
    }

    /// 워커 반복(참 시각 `tau`): 콜백 시각 추정 → 스케줄.
    fn poll(&mut self, tau: f64, plan: &MetronomePlan, offset: Option<f64>) {
        let wall = self.sys_us(tau).max(0.0) as u64;
        let t = self.tracker.observe(&self.counters, wall, self.spec.buffer, self.spec.rate);
        if t.cb_seq.is_none() {
            return;
        }
        let clock = OutClock { dev_now_us: t.dev_now_us as f64, played: t.played, rate: t.out_rate as f64, burst: t.out_burst as u64 };
        self.sched.tick(1, plan, offset, &clock, &self.click);
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
}

struct Outcome {
    max_err_ms: f64,
    mean_err_ms: f64,
    beats: usize,
    host_clicks: usize,
    late: u64,
    tap_nonzero: usize,
}

fn run(sc: &Scenario) -> Outcome {
    let mut rng = Rng(0xDEAD_BEEF_1234_5678);
    let mut a = SimPeer::new(sc.host);
    let mut b = SimPeer::new(sc.follower);
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
                a.poll(tau, &plan_a, Some(0.0));
            } else {
                a.poll(tau, &MetronomePlan::default(), Some(0.0));
            }
            a.next_poll_us = tau + 500.0 + rng.f() * 700.0;
        } else if tau == b.next_poll_us {
            let now_b = b.sys_us(tau);
            let off = clock.estimate(now_b).map(|e| e.offset_us);
            if tau >= start_tau + 150_000.0 {
                // 시그널링이 150ms 뒤 도착
                b.poll(tau, &plan_b, off);
            } else {
                b.poll(tau, &MetronomePlan::default(), off);
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
    for &ta in &a.onsets {
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
        host_clicks: a.onsets.len(),
        late: a.click.late.load(Ordering::Relaxed) + b.click.late.load(Ordering::Relaxed),
        tap_nonzero: a.tap_nonzero + b.tap_nonzero,
    }
}

fn spec(sys_ppm: f64, sys_offset_us: f64, rate: u32, dev_ppm: f64, buffer: usize, phase_us: f64) -> PeerSpec {
    PeerSpec { sys_ppm, sys_offset_us, rate, dev_ppm, buffer, phase_us }
}

fn check(name: &str, sc: &Scenario) {
    let o = run(sc);
    println!(
        "{name}: 박 {} / 방장 클릭 {}, 정렬 오차 최대 {:.3}ms 평균 {:+.3}ms, 늦은 클릭 {}",
        o.beats, o.host_clicks, o.max_err_ms, o.mean_err_ms, o.late
    );
    let expected = ((sc.secs - 2.3) * sc.bpm / 60.0) as usize;
    assert!(o.host_clicks + 2 >= expected, "{name}: 방장 클릭 {} < 기대 {expected}", o.host_clicks);
    assert!(o.beats + 3 >= o.host_clicks, "{name}: 짝지은 박 {} / {}", o.beats, o.host_clicks);
    assert!(o.max_err_ms <= 2.0, "{name}: 정렬 오차 {:.3}ms > 2ms", o.max_err_ms);
    assert_eq!(o.late, 0, "{name}: 늦은 클릭");
    assert_eq!(o.tap_nonzero, 0, "{name}: 믹스 탭에 클릭이 섞임");
}

#[test]
fn two_peers_align_within_2ms_same_setup() {
    check(
        "같은 설정",
        &Scenario {
            host: spec(0.0, 5e9, 48_000, 0.0, 128, 0.0),
            follower: spec(0.0, 3e6, 48_000, 0.0, 128, 1234.0),
            one_way: |_| 5000.0,
            queue_mean_us: 300.0,
            bpm: 120.0,
            secs: 60.0,
        },
    );
}

#[test]
fn two_peers_align_within_2ms_with_drift_and_different_devices() {
    // 시스템 시계 ±200ppm(서로 400ppm 차), 장치 클럭도 따로 어긋남, 레이트·버퍼가 다름
    for (sa, sb) in [(200.0, -200.0), (-200.0, 200.0)] {
        check(
            &format!("드리프트 {sa:+}/{sb:+}ppm"),
            &Scenario {
                host: spec(sa, 1.7e9, 48_000, 80.0, 128, 0.0),
                follower: spec(sb, 42e9, 44_100, -150.0, 441, 777.0),
                one_way: |_| 8000.0,
                queue_mean_us: 800.0,
                bpm: 137.0,
                secs: 180.0,
            },
        );
    }
}

#[test]
fn two_peers_align_within_2ms_across_rtt_changes() {
    // 편도 3ms → 30초에 15ms → 80초에 4ms(대칭 경로 변화), 드리프트 200ppm
    check(
        "RTT 변화",
        &Scenario {
            host: spec(100.0, 0.0, 48_000, 30.0, 64, 0.0),
            follower: spec(-100.0, 9e8, 96_000, -60.0, 256, 3333.0),
            one_way: |t| if t < 30e6 { 3000.0 } else if t < 80e6 { 15_000.0 } else { 4000.0 },
            queue_mean_us: 500.0,
            bpm: 200.0,
            secs: 120.0,
        },
    );
}

// ============================================================================
// 배선: 실제 엔진 2개(가짜 장치, 로컬 UDP) — Pong 시계 표본 → 오프셋 → 클릭
// ============================================================================

mod engines {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use band_lib::engine::config::{CodecKind, EngineConfig};
    use band_lib::engine::device::fake::FakeBackend;
    use band_lib::engine::metronome::MetronomeConfig;
    use band_lib::engine::mixer::MixControls;
    use band_lib::engine::stats::PeerStateKind;
    use band_lib::engine::Engine;
    use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
    use band_lib::net::{clock_payload, parse_clock_payload, NetOptions};

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
