//! 지연 측정 도구(P1-1) 통합 테스트: 가짜 장치 하드웨어 루프백, 신호 판정, 에코 피어(고정 지연 프록시).
//!
//! 두 단계(P3-0, tests/loopback.rs와 같은 규칙):
//! - **결정적**(기본 `cargo test`): 판정 종류, 거부·취소·동의 흐름, 에코 자동 종료처럼 벽시계 품질 임계가 없는 검사.
//!   예열·연결 대기는 고정 sleep이 아니라 진행량(출력 샘플 수, 받은 프레임 수)으로 하고 벽시계는 행 감지 상한뿐이다(P3-0a).
//! - **실시간**(`#[ignore = "realtime: ..."]`): 실제 스레드·실제 시계로 측정 정확도(ms 오차, 흩어짐 판정)를 재는 검사.
//!   CI `realtime` 잡이 하나씩 3회까지 돌린다. ignore 사유에 같은 계산을 합성 신호로 검증하는 결정적 짝을 적었다.
//!   로컬: `cargo test --test measure -- --ignored --test-threads 1`.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use band_lib::engine::config::{CodecKind, EngineConfig};
use band_lib::engine::device::fake::{FakeBackend, SourceFn};
use band_lib::engine::measure::{LoopbackReport, MeasureVerdict};
use band_lib::engine::mixer::MixControls;
use band_lib::engine::stats::PeerStateKind;
use band_lib::engine::Engine;
use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
use band_lib::net::NetOptions;
use parking_lot::Mutex;
use rand::{Rng, SeedableRng};

/// 실시간 테스트끼리 CPU를 다투지 않게 직렬화한다.
static RT_LOCK: Mutex<()> = Mutex::new(());

fn config(buffer: u32) -> EngineConfig {
    EngineConfig {
        buffer_size: buffer,
        frame_ms: 2.5,
        codec: CodecKind::Opus,
        channels: 1,
        input_channels: vec![0],
        ..Default::default()
    }
}

struct Loop {
    delay_s: f64,
    gain: f32,
    noise: f32,
    connected: bool,
}

/// 출력 sink를 `delay_s` 뒤 입력으로 되돌리는 가짜 "케이블"(+ 잡음, 게인, 클리핑).
/// 시간 모델(device.rs `fake`): 입력 샘플 i는 t0 + i/r에 캡처, sink 프레임 j는 t0 + phase + (origin + j)/r에 재생.
fn loopback_backend(rate: u32, in_buf: usize, out_buf: usize, phase_us: u64, jitter_us: u64, l: Loop) -> FakeBackend {
    let sink: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let origin = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (s2, o2) = (sink.clone(), origin.clone());
    let rng = Arc::new(Mutex::new(rand::rngs::StdRng::seed_from_u64(11)));
    let r = rate as f64;
    let phase = phase_us as f64 / 1e6;
    let src: Arc<SourceFn> = Arc::new(move |start: u64, ch: usize, buf: &mut [f32]| {
        let sink = s2.lock();
        let origin = o2.load(Ordering::SeqCst) as f64;
        let mut rng = rng.lock();
        for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
            let t = (start + i as u64) as f64 / r - l.delay_s;
            let j = (t - phase) * r - origin;
            let mut v = 0.0f32;
            if l.connected && j >= 0.0 {
                let j0 = j.floor() as usize;
                let f = (j - j0 as f64) as f32;
                if let (Some(a), Some(b)) = (sink.get(j0 * 2), sink.get((j0 + 1) * 2)) {
                    v = a * (1.0 - f) + b * f;
                }
            }
            v = v * l.gain + if l.noise > 0.0 { rng.gen_range(-l.noise..l.noise) } else { 0.0 };
            frame.fill(v.clamp(-1.0, 1.0));
        }
    });
    let mut b = FakeBackend::new(rate, src);
    b.sink = sink;
    b.sink_origin = origin;
    b.in_buffer = Some(in_buf);
    b.out_buffer = Some(out_buf);
    b.out_phase_us = phase_us;
    b.wake_jitter_us = jitter_us;
    b
}

/// 결정적 테스트의 대기 상한. 판정은 조건 자체로 하고, 벽시계는 행(hang) 감지에만 쓴다.
const HANG: Duration = Duration::from_secs(20);

/// 조건이 참이 될 때까지 기다린다(고정 sleep 대신). 상한 안에 참이 되면 true.
fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let t0 = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if t0.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 출력 콜백이 `ms`만큼(장치 시간) 돌 때까지 기다린다 — 고정 sleep의 "예열" 대신 진행량.
fn wait_output(sink: &Mutex<Vec<f32>>, channels: usize, rate: u32, ms: u32) {
    let want = rate as usize * ms as usize / 1000 * channels;
    assert!(wait_until(HANG, || sink.lock().len() >= want), "출력 콜백이 돌지 않음");
}

/// 지터 버퍼가 실제 패킷으로 만든 프레임 수(은닉 제외, 첫 피어). 벽시계 대신 "패킷을 이만큼 받았다"는 진행량 기준.
fn real_frames(e: &Engine) -> u64 {
    e.peer_jitter_stats().first().map(|(_, js)| js.produced.saturating_sub(js.concealed)).unwrap_or(0)
}

fn measure(backend: FakeBackend, buffer: u32, runs: u32) -> LoopbackReport {
    let (sink, ch, rate) = (backend.sink.clone(), backend.channels, backend.out_rate);
    let e = Engine::start(config(buffer), Arc::new(backend), Arc::new(MixControls::default()), None).unwrap();
    // 모니터를 켜 둔다: 측정 중에는 출력이 자극만 내야 한다(되먹임 없음).
    e.controls().monitor_enabled.store(true, Ordering::Relaxed);
    wait_output(&sink, ch, rate, 400);
    let cancel = AtomicBool::new(false);
    let rep = e.loopback_job().run(runs, &cancel).unwrap();
    drop(e);
    rep
}

#[test]
#[ignore = "realtime: 결정적 짝 engine::measure::tests::{loopback_math_matches_time_model, xcorr_finds_known_offset_with_subsample_precision, noisy_loopback_detects_within_tolerance}"]
fn loopback_known_delay_within_half_ms() {
    let _g = RT_LOCK.lock();
    // (레이트, 입력 버퍼, 출력 버퍼, 출력 위상 µs, 기상 지터 µs)
    let cases = [(48_000u32, 128usize, 128usize, 0u64, 0u64), (48_000, 64, 256, 700, 300), (44_100, 128, 128, 350, 200)];
    for (rate, ib, ob, phase, jit) in cases {
        let delay = 0.0075;
        let mut b = loopback_backend(rate, ib, ob, phase, jit, Loop { delay_s: delay, gain: 1.0, noise: 0.0, connected: true });
        b.reported_latency = (ib as u32 + 20, ob as u32 + 30);
        let rep = measure(b, ib as u32, 6);
        let expect = (delay + ib as f64 / rate as f64) * 1000.0;
        let rt = rep.round_trip_ms.unwrap_or(f64::NAN);
        eprintln!("rate {rate} in {ib} out {ob}: 측정 {rt:.3}ms 기대 {expect:.3}ms {:?} {}", rep.verdict, rep.message);
        assert_eq!(rep.verdict, MeasureVerdict::Ok, "{rep:?}");
        // 요구는 ±0.5ms. 정밀도 회귀를 잡으려고 더 좁게 본다(이 PC 실측 오차 ≤ 0.06ms, PR #13 L7d).
        assert!((rt - expect).abs() <= 0.2, "왕복 {rt} 기대 {expect}: {rep:?}");
        assert_eq!(rep.detected_runs, 6);
        // 보정값 = 실측 − 장치 버퍼 왕복, 보정 estLatency = est + 보정값
        let buf_rt = (ib + ob) as f64 * 1000.0 / rate as f64;
        assert!((rep.buffer_round_trip_ms - buf_rt).abs() < 1e-6);
        assert!((rep.offset_ms.unwrap() - (rt - buf_rt)).abs() < 1e-6);
        assert!((rep.corrected_est_latency_ms.unwrap() - (rep.est_latency_ms as f64 + rt - buf_rt)).abs() < 1e-3);
        // 드라이버 보고 지연이 그대로 실린다
        let drt = (ib + ob + 50) as f64 * 1000.0 / rate as f64;
        assert!((rep.driver_round_trip_ms.unwrap() - drt).abs() < 1e-6);
        assert!((rep.driver_error_ms.unwrap() - (rt - drt)).abs() < 1e-6);
        assert!(rep.runs.iter().all(|r| !r.clipped && r.peak_ratio > 10.0));
    }
}

#[test]
fn loopback_noise_clipping_and_no_signal_verdicts() {
    let _g = RT_LOCK.lock();
    let mk = |l: Loop| loopback_backend(48_000, 128, 128, 0, 0, l);
    // 케이블 없음(무음): 미검출
    let rep = measure(mk(Loop { delay_s: 0.0075, gain: 1.0, noise: 0.0, connected: false }), 128, 3);
    assert_eq!(rep.verdict, MeasureVerdict::NoSignal, "{rep:?}");
    assert_eq!(rep.detected_runs, 0);
    assert!(rep.round_trip_ms.is_none());
    // 잡음만(케이블 없음): 미검출
    let rep = measure(mk(Loop { delay_s: 0.0075, gain: 1.0, noise: 0.3, connected: false }), 128, 3);
    assert_eq!(rep.verdict, MeasureVerdict::NoSignal, "{rep:?}");
    // 케이블 + 잡음(SNR 약 15dB): 검출된다(값의 정확도·흩어짐 판정은 실시간 테스트 `loopback_with_noise_within_half_ms`).
    let rep = measure(mk(Loop { delay_s: 0.0075, gain: 1.0, noise: 0.05, connected: true }), 128, 3);
    assert!(rep.detected_runs > 0 && rep.round_trip_ms.is_some(), "{rep:?}");
    assert!(matches!(rep.verdict, MeasureVerdict::Ok | MeasureVerdict::Unstable), "{rep:?}");
    // 게인 과다: 클리핑 판정(값은 참고로 남음)
    let rep = measure(mk(Loop { delay_s: 0.0075, gain: 4.0, noise: 0.0, connected: true }), 128, 3);
    assert_eq!(rep.verdict, MeasureVerdict::Clipped, "{rep:?}");
    assert!(rep.runs.iter().all(|r| r.clipped));
}

#[test]
#[ignore = "realtime: 결정적 짝 engine::measure::tests::noisy_loopback_detects_within_tolerance"]
fn loopback_with_noise_within_half_ms() {
    let _g = RT_LOCK.lock();
    let b = loopback_backend(48_000, 128, 128, 0, 0, Loop { delay_s: 0.0075, gain: 1.0, noise: 0.05, connected: true });
    let rep = measure(b, 128, 3);
    let expect = 7.5 + 128.0 / 48.0;
    assert_eq!(rep.verdict, MeasureVerdict::Ok, "{rep:?}");
    assert!((rep.round_trip_ms.unwrap() - expect).abs() <= 0.5, "{rep:?}");
}

#[test]
fn loopback_rejects_mismatched_rates_and_concurrent_runs() {
    let _g = RT_LOCK.lock();
    let mut b = FakeBackend::silence(48_000);
    b.out_rate = 44_100;
    let e = Engine::start(config(128), Arc::new(b), Arc::new(MixControls::default()), None).unwrap();
    let err = e.loopback_job().run(1, &AtomicBool::new(false)).unwrap_err();
    assert!(err.contains("레이트"), "{err}");
    drop(e);
    // 취소
    let e = Engine::start(config(128), Arc::new(FakeBackend::silence(48_000)), Arc::new(MixControls::default()), None).unwrap();
    let cancel = AtomicBool::new(true);
    let err = e.loopback_job().run(3, &cancel).unwrap_err();
    assert!(err.contains("취소"), "{err}");
    // 취소 뒤 다시 잴 수 있다(끝점 반환)
    let rep = e.loopback_job().run(1, &AtomicBool::new(false)).unwrap();
    assert_eq!(rep.verdict, MeasureVerdict::NoSignal);
}

#[test]
fn loopback_refused_during_session() {
    let _g = RT_LOCK.lock();
    let backend = FakeBackend::silence(48_000);
    let sink = backend.sink.clone();
    let m = band_lib::manager::EngineManager::new(Arc::new(backend), test_net());
    m.start(config(128), "alice".into()).unwrap();
    let err = m.measure_loopback(1).unwrap_err();
    assert!(err.contains("합주"), "{err}");
    m.stop();
    // 테스트 모드에서는 된다
    let before = sink.lock().len();
    m.test_start(config(128)).unwrap();
    assert!(wait_until(HANG, || sink.lock().len() >= before + 48_000 * 2 * 3 / 10), "테스트 모드 출력이 돌지 않음");
    assert_eq!(m.measure_loopback(1).unwrap().verdict, MeasureVerdict::NoSignal);
    m.stop();
}

#[test]
fn loopback_offset_carries_to_session_engine_on_same_device() {
    // PR #34 M2(a): 테스트 모드에서 넣은(또는 잰) 루프백 보정값이 같은 장치 구성의 합주 엔진 메트로놈에 이어진다.
    use band_lib::engine::metronome::OutputLatencySource;
    let _g = RT_LOCK.lock();
    let backend = FakeBackend::silence(48_000); // 드라이버 보고 지연 없음(cpal WASAPI처럼)
    let m = band_lib::manager::EngineManager::new(Arc::new(backend), test_net());
    let metro = |m: &band_lib::manager::EngineManager| m.stats().metronome;
    let wait_src = |m: &band_lib::manager::EngineManager, want: Option<OutputLatencySource>| {
        wait_until(HANG, || metro(m).output_latency_source == want)
    };
    // 엔진이 없으면 거부
    assert!(m.set_loopback_offset(Some(12.0)).is_err());
    m.test_start(config(128)).unwrap();
    assert!(wait_src(&m, None), "처음에는 보정 안 됨");
    assert!(m.set_loopback_offset(Some(f64::NAN)).is_err());
    m.set_loopback_offset(Some(12.0)).unwrap();
    assert!(wait_src(&m, Some(OutputLatencySource::Loopback)));
    assert_eq!(metro(&m).output_latency_ms, Some(6.0), "입력 쪽 보고가 없으면 반");
    // 같은 장치로 합주 시작 → 이어서 적용
    m.start(config(128), "alice".into()).unwrap();
    assert!(wait_src(&m, Some(OutputLatencySource::Loopback)));
    assert_eq!(metro(&m).output_latency_ms, Some(6.0));
    m.stop();
    // 다른 장치 구성이면 적용하지 않는다
    let other = EngineConfig { output_device: Some("wasapi::다른 장치".into()), ..config(128) };
    m.start(other, "alice".into()).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(metro(&m).output_latency_source, None);
    m.stop();
    // 해제하면 같은 장치에서도 쓰지 않는다
    m.test_start(config(128)).unwrap();
    assert!(wait_src(&m, Some(OutputLatencySource::Loopback)));
    m.set_loopback_offset(None).unwrap();
    assert!(wait_src(&m, None));
    m.stop();
    m.start(config(128), "alice".into()).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(metro(&m).output_latency_source, None);
    m.stop();
}

// ============================================================================
// 에코 피어
// ============================================================================

fn test_net() -> NetOptions {
    NetOptions {
        port: 0,
        stun_servers: vec![],
        gather_lan: false,
        extra_candidates: vec![],
        allow_loopback_candidates: true,
        cancel: None,
    }
}

fn local_addr(e: &Engine) -> SocketAddr {
    format!("127.0.0.1:{}", e.local_port().unwrap()).parse().unwrap()
}

fn with_candidate(d: &LocalDescriptor, addr: SocketAddr) -> LocalDescriptor {
    LocalDescriptor { candidates: vec![Candidate { kind: CandidateKind::Lan, addr: addr.to_string() }], ..d.clone() }
}

/// 방향마다 고정 지연을 넣는 UDP 프록시.
struct DelayProxy {
    pa: SocketAddr,
    pb: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DelayProxy {
    fn start(a: SocketAddr, b: SocketAddr, delay: Duration) -> DelayProxy {
        let sa = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sb = UdpSocket::bind("127.0.0.1:0").unwrap();
        sa.set_nonblocking(true).unwrap();
        sb.set_nonblocking(true).unwrap();
        let (pa, pb) = (sa.local_addr().unwrap(), sb.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let thread = std::thread::spawn(move || {
            let _prio = band_lib::engine::prio::promote_current_thread();
            let mut queue: std::collections::VecDeque<(Instant, bool, Vec<u8>)> = Default::default();
            let mut buf = [0u8; 2048];
            while !st.load(Ordering::Relaxed) {
                for (sock, from_a) in [(&sa, true), (&sb, false)] {
                    while let Ok((n, _)) = sock.recv_from(&mut buf) {
                        queue.push_back((Instant::now() + delay, from_a, buf[..n].to_vec()));
                    }
                }
                let now = Instant::now();
                while queue.front().is_some_and(|q| q.0 <= now) {
                    let (_, from_a, data) = queue.pop_front().unwrap();
                    if from_a {
                        let _ = sb.send_to(&data, b);
                    } else {
                        let _ = sa.send_to(&data, a);
                    }
                }
                std::thread::sleep(Duration::from_micros(100));
            }
        });
        DelayProxy { pa, pb, stop, thread: Some(thread) }
    }
}

impl Drop for DelayProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn wait_connected(e: &Engine) {
    let t0 = Instant::now();
    while t0.elapsed() < HANG {
        let s = e.stats();
        if !s.peers.is_empty() && s.peers.iter().all(|p| p.state == PeerStateKind::Connected) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("연결 실패");
}

fn pair(delay: Duration) -> (Engine, Engine, DelayProxy) {
    let mk = |seed| {
        let mut b = FakeBackend::silence(48_000);
        b.seed = seed;
        b
    };
    let a = Engine::start(config(128), Arc::new(mk(1)), Arc::new(MixControls::default()), Some(("alice".into(), test_net()))).unwrap();
    let b = Engine::start(config(128), Arc::new(mk(2)), Arc::new(MixControls::default()), Some(("bob".into(), test_net()))).unwrap();
    let p = DelayProxy::start(local_addr(&a), local_addr(&b), delay);
    a.add_peer("bob".into(), &with_candidate(b.descriptor().unwrap(), p.pa)).unwrap();
    b.add_peer("alice".into(), &with_candidate(a.descriptor().unwrap(), p.pb)).unwrap();
    wait_connected(&a);
    wait_connected(&b);
    (a, b, p)
}

/// B가 A의 요청을 보면 동의한다(사람이 UI에서 "허용"을 누르는 것). B 쪽 요청 표시도 확인한다.
fn auto_accept(b: Arc<Mutex<Engine>>, stop: Arc<AtomicBool>) -> JoinHandle<bool> {
    std::thread::spawn(move || {
        let mut saw_request = false;
        while !stop.load(Ordering::Relaxed) {
            {
                let e = b.lock();
                let s = e.stats();
                if let Some(p) = s.peers.iter().find(|p| p.peer_id == "alice") {
                    if p.echo_requested {
                        saw_request = true;
                        e.set_echo_mode("alice", true).unwrap();
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        saw_request
    })
}

fn echo_once(one_way: Duration) -> band_lib::engine::measure::EchoReport {
    let (a, b, _p) = pair(one_way);
    // 동의 전에는 켤 수 없다(양측 합의)
    let err = b.set_echo_mode("alice", true).unwrap_err();
    assert!(err.contains("요청"), "{err}");
    // 고정 sleep(800ms) 대신 진행량(P3-0a): 양쪽이 상대 패킷으로 프레임 320개(2.5ms × 320 = 0.8초분)를 만들 때까지.
    assert!(wait_until(HANG, || real_frames(&a) >= 320 && real_frames(&b) >= 320), "양방향 오디오가 흐르지 않음");
    let b = Arc::new(Mutex::new(b));
    let stop = Arc::new(AtomicBool::new(false));
    let acc = auto_accept(b.clone(), stop.clone());
    let rep = a.echo_job("bob").unwrap().run(6, Duration::from_secs(5), &AtomicBool::new(false)).unwrap();
    stop.store(true, Ordering::Relaxed);
    assert!(acc.join().unwrap(), "B가 요청 표시(echoRequested)를 봐야 한다");
    // 측정이 끝나면 B는 에코를 스스로 멈추고, A는 다시 B의 소리를 받는다.
    let t0 = Instant::now();
    loop {
        let s = b.lock().stats();
        let pa = s.peers.iter().find(|p| p.peer_id == "alice").unwrap();
        if !pa.echo_active && !pa.echo_requested {
            break;
        }
        assert!(t0.elapsed() < HANG, "B 에코가 꺼지지 않음");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!a.stats().peers[0].peer_echoing);
    rep
}

/// 결정적: 동의 → 측정 → 자동 종료 흐름과 클릭 검출(정확도·흩어짐 판정은 실시간 짝에서).
#[test]
fn echo_flow_consent_measure_auto_stop() {
    let _g = RT_LOCK.lock();
    let r = echo_once(Duration::from_millis(5));
    assert!(r.detected_runs >= 1, "{r:?}");
    assert!(r.round_trip_ms.is_some_and(|v| v > 10.0), "왕복은 최소 프록시 왕복 10ms보다 길다: {r:?}");
    assert!(r.peer_echo_delay_ms < 50.0, "{r:?}");
}

#[test]
#[ignore = "realtime: 결정적 짝 engine::measure::tests::echo_probe_emits_frame_aligned_clicks_and_detects_them(클릭 송신·검출 시각 계산), echo_flow_consent_measure_auto_stop(흐름)"]
fn echo_round_trip_reflects_proxy_delay() {
    let _g = RT_LOCK.lock();
    let near = echo_once(Duration::from_millis(5));
    let far = echo_once(Duration::from_millis(25));
    for r in [&near, &far] {
        eprintln!(
            "에코: 왕복 {:?} 편도 {:?} 핑 {:?} 지터 {} 상대 {:.3} 잔차 {:?} 입→귀 {:?} {:?}",
            r.round_trip_ms, r.one_way_ms, r.network_rtt_ms, r.jitter_buffer_ms, r.peer_echo_delay_ms, r.residual_ms, r.mouth_to_ear_ms, r.runs
        );
        assert_eq!(r.verdict, MeasureVerdict::Ok, "{r:?}");
        assert!(r.detected_runs >= 5, "{r:?}");
        assert!(r.peer_echo_delay_ms < 1.0, "되돌리기는 디코드 없이 바로: {r:?}");
        // 왕복 = 프레임 + 망 왕복 + 상대 처리 + 지터 버퍼(± 측정 오차)
        // 잔차에는 클릭이 놓인 입력 버스트 위상(≤ 입력 버퍼)과 지터 버퍼 평균·순간값 차이가 남는다.
        // 잔차 = 왕복 − (프레임 + 핑 RTT + 상대 처리 + 지터 버퍼 평균). 클릭이 놓인 입력 버스트 위상(≤ 입력 버퍼)과
        // 지터 버퍼 순간값·평균 차이가 남는다.
        assert!(r.residual_ms.unwrap().abs() < 5.0, "{r:?}");
    }
    // 프록시 편도 지연 20ms 차이 → 왕복 40ms, 편도 추정 20ms 차이
    let d_rt = far.round_trip_ms.unwrap() - near.round_trip_ms.unwrap();
    let d_ow = far.one_way_ms.unwrap() - near.one_way_ms.unwrap();
    eprintln!("왕복 차 {d_rt:.2}ms, 편도 차 {d_ow:.2}ms");
    assert!((d_rt - 40.0).abs() < 2.5, "왕복 차 {d_rt}");
    assert!((d_ow - 20.0).abs() < 1.5, "편도 차 {d_ow}");
}

#[test]
fn echo_requires_consent_and_times_out() {
    let _g = RT_LOCK.lock();
    let (a, b, _p) = pair(Duration::from_millis(2));
    assert!(wait_until(HANG, || real_frames(&a) >= 120 && real_frames(&b) >= 120), "양방향 오디오가 흐르지 않음");
    let t0 = Instant::now();
    let err = a.echo_job("bob").unwrap().run(2, Duration::from_millis(1500), &AtomicBool::new(false)).unwrap_err();
    assert!(err.contains("수락"), "{err}");
    assert!(t0.elapsed() < HANG, "동의 대기 1.5초가 끝나지 않음(행)");
    // B는 요청을 봤지만 동의하지 않았으므로 되돌리지 않았다. 요청이 끝나면 표시도 사라진다.
    let t1 = Instant::now();
    while b.stats().peers[0].echo_requested {
        assert!(t1.elapsed() < HANG, "요청 표시가 사라지지 않음");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!b.stats().peers[0].echo_active);
    assert!(a.echo_job("nobody").is_err());
}
