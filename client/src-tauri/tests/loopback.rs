//! 통합 테스트(설계 문서 7절): 엔진 2개를 가짜 오디오 장치로 띄우고, 손실 5%·지터 20ms를
//! 주입하는 UDP 프록시를 거쳐 연결한다. A가 사인파를 보내고 B의 출력에서 연속성/지연을 검증한다.
//!
//! 실시간으로 돌기 때문에 수 초~십수 초 걸린다. 측정값은 `--nocapture`로 볼 수 있다.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use band_lib::engine::config::{CodecKind, EngineConfig};
use band_lib::engine::device::fake::FakeBackend;
use band_lib::engine::mixer::MixControls;
use band_lib::engine::stats::PeerStateKind;
use band_lib::engine::Engine;
use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
use band_lib::net::NetOptions;
use rand::{Rng, SeedableRng};

const SINE_HZ: f64 = 440.0;
const SINE_AMP: f32 = 0.5;

fn sine_backend(drift_ppm: f64) -> FakeBackend {
    let mut b = FakeBackend::new(
        48_000,
        Arc::new(|start: u64, ch: usize, buf: &mut [f32]| {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let t = (start + i as u64) as f64 / 48_000.0;
                let v = (t * SINE_HZ * 2.0 * std::f64::consts::PI).sin() as f32 * SINE_AMP;
                frame.fill(v);
            }
        }),
    );
    b.drift_ppm = drift_ppm;
    b
}

fn test_net() -> NetOptions {
    NetOptions { port: 0, stun_servers: vec![], gather_lan: false, extra_candidates: vec![] }
}

fn config() -> EngineConfig {
    EngineConfig {
        buffer_size: 128,
        frame_ms: 5.0,
        codec: CodecKind::Opus,
        channels: 1,
        input_channels: vec![0],
        ..Default::default()
    }
}

fn with_candidate(d: &LocalDescriptor, addr: SocketAddr) -> LocalDescriptor {
    LocalDescriptor {
        candidates: vec![Candidate { kind: CandidateKind::Lan, addr: addr.to_string() }],
        ..d.clone()
    }
}

/// 손실/지연 주입 UDP 프록시.
/// A ↔ pa ... pb ↔ B. A는 pa로 보내고(B로 보임), B는 pb로 보낸다(A로 보임).
struct Proxy {
    pa: SocketAddr,
    pb: SocketAddr,
    stop: Arc<AtomicBool>,
    forwarded: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl Proxy {
    fn start(a: SocketAddr, b: SocketAddr, loss: f64, jitter_ms: f64) -> Proxy {
        let sa = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sb = UdpSocket::bind("127.0.0.1:0").unwrap();
        sa.set_nonblocking(true).unwrap();
        sb.set_nonblocking(true).unwrap();
        let pa = sa.local_addr().unwrap();
        let pb = sb.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let forwarded = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let (st, fw, dr) = (stop.clone(), forwarded.clone(), dropped.clone());
        let thread = std::thread::spawn(move || {
            let mut rng = rand::rngs::StdRng::seed_from_u64(42);
            // (전송 시각, from_a, 데이터)
            let mut queue: Vec<(Instant, bool, Vec<u8>)> = Vec::new();
            let mut buf = [0u8; 2048];
            while !st.load(Ordering::Relaxed) {
                for (sock, from_a) in [(&sa, true), (&sb, false)] {
                    while let Ok((n, _src)) = sock.recv_from(&mut buf) {
                        if rng.gen::<f64>() < loss {
                            dr.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        let delay = Duration::from_secs_f64(rng.gen::<f64>() * jitter_ms / 1000.0);
                        queue.push((Instant::now() + delay, from_a, buf[..n].to_vec()));
                    }
                }
                let now = Instant::now();
                let mut i = 0;
                while i < queue.len() {
                    if queue[i].0 <= now {
                        let (_, from_a, data) = queue.swap_remove(i);
                        if from_a {
                            let _ = sb.send_to(&data, b);
                        } else {
                            let _ = sa.send_to(&data, a);
                        }
                        fw.fetch_add(1, Ordering::Relaxed);
                    } else {
                        i += 1;
                    }
                }
                std::thread::sleep(Duration::from_micros(200));
            }
        });
        Proxy { pa, pb, stop, forwarded, dropped, thread: Some(thread) }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn wait_connected(e: &Engine, timeout: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        let s = e.stats();
        if !s.peers.is_empty() && s.peers.iter().all(|p| p.state == PeerStateKind::Connected) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn loopback_loss5_jitter20_drift() {
    let controls_a = Arc::new(MixControls::default());
    let controls_b = Arc::new(MixControls::default());
    // A의 장치 클럭은 +200ppm 빠르게 → B는 드리프트를 보정해야 한다.
    let backend_a = sine_backend(200.0);
    let backend_b = FakeBackend::silence(48_000);
    let sink_b = backend_b.sink.clone();

    let a = Engine::start(config(), Arc::new(backend_a), controls_a, Some(("alice".into(), test_net()))).unwrap();
    let mut b = Engine::start(config(), Arc::new(backend_b), controls_b, Some(("bob".into(), test_net()))).unwrap();
    let addr_a: SocketAddr = format!("127.0.0.1:{}", a.local_port().unwrap()).parse().unwrap();
    let addr_b: SocketAddr = format!("127.0.0.1:{}", b.local_port().unwrap()).parse().unwrap();
    let proxy = Proxy::start(addr_a, addr_b, 0.05, 20.0);

    // A는 B를 pa로, B는 A를 pb로 본다.
    a.add_peer("bob".into(), &with_candidate(b.descriptor().unwrap(), proxy.pa)).unwrap();
    b.add_peer("alice".into(), &with_candidate(a.descriptor().unwrap(), proxy.pb)).unwrap();
    assert!(wait_connected(&a, Duration::from_secs(5)), "A 연결 실패");
    assert!(wait_connected(&b, Duration::from_secs(5)), "B 연결 실패");
    let t_connected = Instant::now();
    let sink_start = sink_b.lock().len();

    // 믹스 구독 검증용
    let chunks = Arc::new(AtomicUsize::new(0));
    let bad_chunks = Arc::new(AtomicUsize::new(0));
    let (c2, bc2) = (chunks.clone(), bad_chunks.clone());
    b.subscribe_mix(Box::new(move |bytes| {
        c2.fetch_add(1, Ordering::Relaxed);
        if bytes.len() != 960 * 2 * 4 {
            bc2.fetch_add(1, Ordering::Relaxed);
        }
    }))
    .unwrap();

    // 12초 동안 0.5초 간격으로 B의 버퍼 깊이 기록
    let mut trace: Vec<(f64, f32, f32)> = Vec::new();
    while t_connected.elapsed() < Duration::from_secs(12) {
        std::thread::sleep(Duration::from_millis(500));
        let js = b.peer_jitter_stats();
        let (_, s) = &js[0];
        trace.push((t_connected.elapsed().as_secs_f64(), s.buffer_ms, s.target_ms));
    }

    let stats_b = b.stats();
    let js = b.peer_jitter_stats()[0].1;
    let rtt_a = a.stats().peers[0].rtt_ms;
    b.unsubscribe_mix();
    let mix_chunks = chunks.load(Ordering::Relaxed);

    // 출력 분석: 연결 후 3초부터 10ms 창의 RMS
    let out = sink_b.lock().clone();
    let analyzed = &out[sink_start..];
    let win = 480 * 2; // 10ms 스테레오
    let skip = 48_000 * 2 * 3;
    let mut windows = 0;
    let mut sound = 0;
    for w in analyzed.get(skip..).unwrap_or(&[]).chunks_exact(win) {
        let rms = (w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).sqrt();
        windows += 1;
        if rms > 0.15 {
            sound += 1;
        }
    }
    let continuity = sound as f64 / windows.max(1) as f64;
    let conceal_ratio = js.concealed as f64 / js.produced.max(1) as f64;
    let mean = |from: f64, to: f64| {
        let v: Vec<f32> = trace.iter().filter(|t| t.0 >= from && t.0 < to).map(|t| t.1).collect();
        v.iter().sum::<f32>() / v.len().max(1) as f32
    };
    let mid = mean(4.0, 8.0);
    let end = mean(8.0, 12.5);

    println!("=== loopback 결과 (손실 5%, 지터 0..20ms, 송신 장치 +200ppm) ===");
    println!("proxy: forwarded {} dropped {}", proxy.forwarded.load(Ordering::Relaxed), proxy.dropped.load(Ordering::Relaxed));
    println!("B←A 지터버퍼: {js:?}");
    println!("버퍼 지연 평균: 4~8s {mid:.1}ms, 8~12s {end:.1}ms, 목표 {:.1}ms", js.target_ms);
    println!("은닉 프레임 비율: {:.2}% (손실 추정 {:.1}%)", conceal_ratio * 100.0, js.loss_pct);
    println!("출력 연속성(10ms 창 중 소리 있음): {:.2}% ({sound}/{windows})", continuity * 100.0);
    println!("B estLatencyMs(로컬): {:.1}, xruns {}, A RTT {:?}ms", stats_b.est_latency_ms, stats_b.xruns, rtt_a);
    println!("믹스 탭 청크: {mix_chunks}");
    println!("trace: {:?}", trace.iter().map(|t| (t.0 as f32, t.1)).collect::<Vec<_>>());

    assert!(continuity > 0.95, "출력 연속성 부족: {continuity}");
    assert!(conceal_ratio < 0.15, "은닉 비율 과다: {conceal_ratio}");
    assert!(end < 60.0, "버퍼 지연 상한 초과: {end}ms");
    assert!((end - js.target_ms).abs() < 10.0, "버퍼 깊이가 목표 근처로 수렴하지 않음: {end} vs {}", js.target_ms);
    assert!((end - mid).abs() < 8.0, "지연 누적(드리프트 미보정): mid {mid} end {end}");
    assert!(js.samples_dropped + js.dropped_frames > 0, "드리프트 보정이 동작해야 함: {js:?}");
    assert!(mix_chunks >= 400, "믹스 탭은 20ms마다(12초 ≈ 600개): {mix_chunks}");
    assert_eq!(bad_chunks.load(Ordering::Relaxed), 0);
    assert!(rtt_a.is_some(), "RTT 측정");

    drop(proxy);
    drop(a);
    b.stop();
}

#[test]
fn repeated_start_stop_joins_threads() {
    let controls = Arc::new(MixControls::default());
    for i in 0..5 {
        let t0 = Instant::now();
        let mut e = Engine::start(
            config(),
            Arc::new(FakeBackend::silence(48_000)),
            controls.clone(),
            if i % 2 == 0 { Some((format!("p{i}"), test_net())) } else { None },
        )
        .unwrap();
        e.subscribe_mix(Box::new(|_| {})).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        e.stop();
        e.stop(); // 멱등
        assert!(t0.elapsed() < Duration::from_secs(3), "정지가 지연됨");
    }
}

#[test]
fn test_mode_monitor_passthrough() {
    let controls = Arc::new(MixControls::default());
    controls.monitor_enabled.store(true, Ordering::Relaxed);
    let backend = sine_backend(0.0);
    let sink = backend.sink.clone();
    let mut e = Engine::start(config(), Arc::new(backend), controls, None).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let s = e.stats();
    e.stop();
    let out = sink.lock().clone();
    let tail = &out[out.len().saturating_sub(48_000)..];
    let rms = (tail.iter().map(|x| x * x).sum::<f32>() / tail.len().max(1) as f32).sqrt();
    println!("모니터 모드: rms {rms:.3}, estLatencyMs {:.1}, xruns {}", s.est_latency_ms, s.xruns);
    assert!((rms - SINE_AMP / 2f32.sqrt()).abs() < 0.05, "모니터 출력 rms {rms}");
    assert!(s.est_latency_ms < 20.0, "로컬 지연 {}", s.est_latency_ms);
}

#[test]
fn rejects_bad_config_and_descriptor() {
    let controls = Arc::new(MixControls::default());
    let bad = EngineConfig { codec: CodecKind::Pcm16, channels: 2, input_channels: vec![0, 1], frame_ms: 10.0, ..config() };
    assert!(Engine::start(bad, Arc::new(FakeBackend::silence(48_000)), controls.clone(), None).is_err());

    let e = Engine::start(config(), Arc::new(FakeBackend::silence(48_000)), controls, Some(("me".into(), test_net()))).unwrap();
    let own = e.descriptor().unwrap().clone();
    let mut d = with_candidate(&own, "127.0.0.1:9".parse().unwrap());
    assert!(e.add_peer("x".into(), &d).is_err(), "자기 자신 ssrc 거부");
    d.ssrc ^= 1;
    d.key = "bad".into();
    assert!(e.add_peer("x".into(), &d).is_err(), "잘못된 키 거부");
}
