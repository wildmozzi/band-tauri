//! 수신 손실 피드백 Opus 전환(P1-4e) 종단 확인: 실제 엔진 둘(A 사인 PCM16 → B), 직접 경로 사이 프록시가 A→B Audio만
//! N개 중 1개씩 버린다. B가 Ping으로 보고한 손실을 A가 받아 B에게만 Opus로 바꾸고, 손실이 멈추면 hold 뒤 PCM16으로
//! 돌아오는지 본다. 정책·은닉 수치의 결정적 짝은 `engine::relay_codec::tests::loss_feedback_*`,
//! `engine::loss_codec_sim::loss_feedback_sim`(가상 시간), 보고 형식은 `net::peer::tests::loss_report_*`.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use band_lib::engine::config::{CodecKind, EngineConfig};
use band_lib::engine::device::fake::FakeBackend;
use band_lib::engine::mixer::MixControls;
use band_lib::engine::relay_codec::CodecReason;
use band_lib::engine::stats::PeerStats;
use band_lib::engine::Engine;
use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
use band_lib::net::NetOptions;

/// A ↔ B 직접 경로 프록시. A가 pa로 보내면 pb에서 B로, B가 pb로 보내면 pa에서 A로. A→B Audio(type 1)는
/// `drop_every`(0 = 끔)개마다 1개 버린다.
struct LossyProxy {
    pa: SocketAddr,
    pb: SocketAddr,
    drop_every: Arc<AtomicU32>,
    dropped: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LossyProxy {
    fn start(a: SocketAddr, b: SocketAddr) -> Self {
        let sa = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sb = UdpSocket::bind("127.0.0.1:0").unwrap();
        sa.set_nonblocking(true).unwrap();
        sb.set_nonblocking(true).unwrap();
        let (pa, pb) = (sa.local_addr().unwrap(), sb.local_addr().unwrap());
        let drop_every = Arc::new(AtomicU32::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (de, dr, st) = (drop_every.clone(), dropped.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let _prio = band_lib::engine::prio::promote_current_thread();
            let mut buf = [0u8; 2048];
            let mut audio = 0u32;
            while !st.load(Ordering::Relaxed) {
                while let Ok((n, _)) = sa.recv_from(&mut buf) {
                    let every = de.load(Ordering::Relaxed);
                    if n > 3 && buf[..2] == *b"BN" && buf[3] == 1 && every > 0 {
                        audio = audio.wrapping_add(1);
                        if audio.is_multiple_of(every) {
                            dr.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    }
                    let _ = sb.send_to(&buf[..n], b);
                }
                while let Ok((n, _)) = sb.recv_from(&mut buf) {
                    let _ = sa.send_to(&buf[..n], a);
                }
                std::thread::sleep(Duration::from_micros(200));
            }
        });
        Self { pa, pb, drop_every, dropped, stop, thread: Some(thread) }
    }
}

impl Drop for LossyProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn sine_backend() -> FakeBackend {
    FakeBackend::new(
        48_000,
        Arc::new(|start: u64, ch: usize, buf: &mut [f32]| {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let t = (start + i as u64) as f64 / 48_000.0;
                frame.fill((t * 440.0 * 2.0 * std::f64::consts::PI).sin() as f32 * 0.5);
            }
        }),
    )
}

fn test_net() -> NetOptions {
    NetOptions { port: 0, stun_servers: vec![], gather_lan: false, extra_candidates: vec![], allow_loopback_candidates: true, cancel: None }
}

fn local_addr(e: &Engine) -> SocketAddr {
    format!("127.0.0.1:{}", e.local_port().unwrap()).parse().unwrap()
}

fn peer(e: &Engine, id: &str) -> Option<PeerStats> {
    e.stats().peers.into_iter().find(|p| p.peer_id == id)
}

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> Option<Duration> {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if f() {
            return Some(t0.elapsed());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
#[ignore = "realtime: 결정적 짝 engine::loss_codec_sim::loss_feedback_sim, engine::relay_codec::tests::loss_feedback_*"]
fn direct_path_loss_feedback_switches_to_opus_and_back() {
    let cfg = EngineConfig { buffer_size: 128, frame_ms: 2.5, codec: CodecKind::Pcm16, channels: 1, input_channels: vec![0], ..Default::default() };
    let be_b = FakeBackend::silence(48_000);
    let a = Engine::start(cfg.clone(), Arc::new(sine_backend()), Arc::new(MixControls::default()), Some(("alice".into(), test_net()))).unwrap();
    let b = Engine::start(cfg, Arc::new(be_b), Arc::new(MixControls::default()), Some(("bob".into(), test_net()))).unwrap();
    let proxy = LossyProxy::start(local_addr(&a), local_addr(&b));
    let cand = |d: &LocalDescriptor, addr: SocketAddr| LocalDescriptor {
        candidates: vec![Candidate { kind: CandidateKind::Lan, addr: addr.to_string() }],
        ..d.clone()
    };
    a.add_peer("bob".into(), &cand(b.descriptor().unwrap(), proxy.pa)).unwrap();
    b.add_peer("alice".into(), &cand(a.descriptor().unwrap(), proxy.pb)).unwrap();

    // 손실 없음: A는 B에게 PCM16, B는 손실 0을 보고
    assert!(
        wait_until(Duration::from_secs(10), || peer(&a, "bob").is_some_and(|p| p.remote_loss_pct.is_some() && p.tx_codec == Some(CodecKind::Pcm16))).is_some(),
        "연결·첫 보고: {:?}",
        peer(&a, "bob")
    );
    std::thread::sleep(Duration::from_secs(2));
    let p = peer(&a, "bob").unwrap();
    assert_eq!(p.tx_codec_reason, None);
    assert!(p.remote_loss_pct.unwrap() < 0.5, "{p:?}");

    // A→B Audio 20개 중 1개(5%) 손실
    proxy.drop_every.store(20, Ordering::Relaxed);
    let t_on = wait_until(Duration::from_secs(15), || peer(&a, "bob").is_some_and(|p| p.tx_codec_reason == Some(CodecReason::LossFeedback)));
    let p = peer(&a, "bob").unwrap();
    let pb = peer(&b, "alice").unwrap();
    println!("=== 손실 5% === 전환까지 {t_on:?}, A 보기 {p:?}\nB 보기: 손실 {:.2}% 은닉 {:.2}%", pb.loss_pct, pb.conceal_pct);
    assert!(t_on.is_some(), "손실 피드백으로 Opus 전환: {p:?}");
    assert_eq!(p.tx_codec, Some(CodecKind::Opus));
    assert!(p.tx_codec_auto);
    assert!((2.0..10.0).contains(&p.remote_loss_pct.unwrap()), "보고 손실 {p:?}");
    // 전환 뒤 몇 초: 은닉은 손실을 크게 넘지 않는다(전환 자체가 은닉을 만들지 않음)
    std::thread::sleep(Duration::from_secs(5));
    let pb = peer(&b, "alice").unwrap();
    println!("전환 5초 뒤 B: 손실 {:.2}% 은닉 {:.2}% 버퍼 {:.1}ms", pb.loss_pct, pb.conceal_pct, pb.buffer_ms);
    assert!(pb.conceal_pct < pb.loss_pct + 1.5, "{pb:?}");

    // 손실 멈춤: 5초 창이 1% 아래로 + hold 10초 → PCM16
    proxy.drop_every.store(0, Ordering::Relaxed);
    let t_off = wait_until(Duration::from_secs(30), || peer(&a, "bob").is_some_and(|p| p.tx_codec == Some(CodecKind::Pcm16)));
    let p = peer(&a, "bob").unwrap();
    println!("=== 손실 멈춤 === 복귀까지 {t_off:?}, A 보기 {p:?}, 프록시 버림 {}", proxy.dropped.load(Ordering::Relaxed));
    assert!(t_off.is_some_and(|t| t >= Duration::from_secs(10)), "hold 뒤 PCM16 복귀: {t_off:?}");
    assert_eq!(p.tx_codec_reason, None);
}

/// 손실을 겪는 피어에게만 전환되는지(#66 리뷰 L4): A가 B(손실 프록시 경유)와 C(직접, 손실 없음)에게 보낸다.
/// 실제 워커·`send_audio` 경로에서 B만 Opus(`LossFeedback`)로 바뀌고 C는 PCM16을 유지해야 한다.
#[test]
#[ignore = "realtime: 결정적 짝 engine::relay_codec::tests::overlapping_reasons_*"]
fn loss_feedback_switches_only_the_lossy_peer() {
    let cfg = EngineConfig { buffer_size: 128, frame_ms: 2.5, codec: CodecKind::Pcm16, channels: 1, input_channels: vec![0], ..Default::default() };
    let start = |id: &str, be: FakeBackend| Engine::start(cfg.clone(), Arc::new(be), Arc::new(MixControls::default()), Some((id.into(), test_net()))).unwrap();
    let a = start("alice", sine_backend());
    let b = start("bob", FakeBackend::silence(48_000));
    let c = start("carol", FakeBackend::silence(48_000));
    let proxy = LossyProxy::start(local_addr(&a), local_addr(&b));
    let cand = |d: &LocalDescriptor, addr: SocketAddr| LocalDescriptor {
        candidates: vec![Candidate { kind: CandidateKind::Lan, addr: addr.to_string() }],
        ..d.clone()
    };
    a.add_peer("bob".into(), &cand(b.descriptor().unwrap(), proxy.pa)).unwrap();
    b.add_peer("alice".into(), &cand(a.descriptor().unwrap(), proxy.pb)).unwrap();
    a.add_peer("carol".into(), &cand(c.descriptor().unwrap(), local_addr(&c))).unwrap();
    c.add_peer("alice".into(), &cand(a.descriptor().unwrap(), local_addr(&a))).unwrap();
    let ready = |id: &str| peer(&a, id).is_some_and(|p| p.remote_loss_pct.is_some() && p.tx_codec == Some(CodecKind::Pcm16));
    assert!(wait_until(Duration::from_secs(10), || ready("bob") && ready("carol")).is_some(), "연결·첫 보고: {:?}", a.stats().peers);

    proxy.drop_every.store(20, Ordering::Relaxed);
    let t_on = wait_until(Duration::from_secs(15), || peer(&a, "bob").is_some_and(|p| p.tx_codec_reason == Some(CodecReason::LossFeedback)));
    assert!(t_on.is_some(), "B 전환: {:?}", peer(&a, "bob"));
    // 전환 뒤 3초 동안 C는 계속 PCM16(이유 없음)
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(3) {
        let pb = peer(&a, "bob").unwrap();
        let pc = peer(&a, "carol").unwrap();
        assert_eq!((pb.tx_codec, pb.tx_codec_reason), (Some(CodecKind::Opus), Some(CodecReason::LossFeedback)), "{pb:?}");
        assert_eq!((pc.tx_codec, pc.tx_codec_reason, pc.tx_codec_auto), (Some(CodecKind::Pcm16), None, false), "{pc:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let pc_rx = peer(&c, "alice").unwrap();
    println!("B 전환 {t_on:?}; C 수신 손실 {:.2}% 은닉 {:.2}%", pc_rx.loss_pct, pc_rx.conceal_pct);
    assert!(pc_rx.loss_pct < 0.5, "{pc_rx:?}");
}
