//! 지연 측정 도구(P1-1) 통합 테스트: 가짜 장치 하드웨어 루프백, 신호 판정, 에코 피어(고정 지연 프록시).

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

fn measure(backend: FakeBackend, buffer: u32, runs: u32) -> LoopbackReport {
    let e = Engine::start(config(buffer), Arc::new(backend), Arc::new(MixControls::default()), None).unwrap();
    // 모니터를 켜 둔다: 측정 중에는 출력이 자극만 내야 한다(되먹임 없음).
    e.controls().monitor_enabled.store(true, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(400));
    let cancel = AtomicBool::new(false);
    let rep = e.loopback_job().run(runs, &cancel).unwrap();
    drop(e);
    rep
}

#[test]
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
    // 케이블 + 잡음(SNR 약 15dB): 정상 검출, ±0.5ms
    let rep = measure(mk(Loop { delay_s: 0.0075, gain: 1.0, noise: 0.05, connected: true }), 128, 3);
    let expect = 7.5 + 128.0 / 48.0;
    assert_eq!(rep.verdict, MeasureVerdict::Ok, "{rep:?}");
    assert!((rep.round_trip_ms.unwrap() - expect).abs() <= 0.5, "{rep:?}");
    // 게인 과다: 클리핑 판정(값은 참고로 남음)
    let rep = measure(mk(Loop { delay_s: 0.0075, gain: 4.0, noise: 0.0, connected: true }), 128, 3);
    assert_eq!(rep.verdict, MeasureVerdict::Clipped, "{rep:?}");
    assert!(rep.runs.iter().all(|r| r.clipped));
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
    let m = band_lib::manager::EngineManager::new(Arc::new(FakeBackend::silence(48_000)), test_net());
    m.start(config(128), "alice".into()).unwrap();
    let err = m.measure_loopback(1).unwrap_err();
    assert!(err.contains("합주"), "{err}");
    m.stop();
    // 테스트 모드에서는 된다
    m.test_start(config(128)).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(m.measure_loopback(1).unwrap().verdict, MeasureVerdict::NoSignal);
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
    while t0.elapsed() < Duration::from_secs(5) {
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
    std::thread::sleep(Duration::from_millis(800));
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
        assert!(t0.elapsed() < Duration::from_secs(5), "B 에코가 꺼지지 않음");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!a.stats().peers[0].peer_echoing);
    rep
}

#[test]
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
    std::thread::sleep(Duration::from_millis(300));
    let t0 = Instant::now();
    let err = a.echo_job("bob").unwrap().run(2, Duration::from_millis(1500), &AtomicBool::new(false)).unwrap_err();
    assert!(err.contains("수락"), "{err}");
    assert!(t0.elapsed() < Duration::from_secs(8));
    // B는 요청을 봤지만 동의하지 않았으므로 되돌리지 않았다. 요청이 끝나면 표시도 사라진다.
    let t1 = Instant::now();
    while b.stats().peers[0].echo_requested {
        assert!(t1.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!b.stats().peers[0].echo_active);
    assert!(a.echo_job("nobody").is_err());
}
