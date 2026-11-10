//! 통합 테스트(설계 문서 7절, 엔진 리뷰 "테스트 신뢰도" 반영).
//!
//! 가짜 장치(`FakeBackend`)는 입력/출력을 별도 스레드·별도 클럭으로 돌리고 콜백 기상 지연(버스트)을
//! 흉내 낸다. 출력 검사는 RMS 창이 아니라 **샘플 단위**로 한다:
//! - 불연속 = L 채널 인접 샘플 차 > 0.08 (440Hz, 진폭 0.5 사인의 최대 차는 0.029)
//! - 무음 구간 = 정확히 0인 샘플이 120개 이상 연속
//!
//! 실시간 테스트끼리 CPU를 다투면 느린 CI에서 흔들리므로 `RT_LOCK`으로 직렬화한다.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use band_lib::engine::config::{CodecKind, EngineConfig, HEADER_BYTES, MAX_PACKET_BYTES};
use band_lib::engine::device::fake::FakeBackend;
use band_lib::engine::mixer::MixControls;
use band_lib::engine::stats::PeerStateKind;
use band_lib::engine::Engine;
use band_lib::manager::EngineManager;
use band_lib::net::crypto::KeyPair;
use band_lib::net::packet::{Header, PacketType};
use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
use band_lib::net::NetOptions;
use parking_lot::Mutex;
use rand::{Rng, SeedableRng};

static RT_LOCK: Mutex<()> = Mutex::new(());

const SINE_HZ: f64 = 440.0;
const SINE_AMP: f32 = 0.5;

fn sine_source() -> Arc<band_lib::engine::device::fake::SourceFn> {
    Arc::new(|start: u64, ch: usize, buf: &mut [f32]| {
        for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
            let t = (start + i as u64) as f64 / 48_000.0;
            frame.fill((t * SINE_HZ * 2.0 * std::f64::consts::PI).sin() as f32 * SINE_AMP);
        }
    })
}

fn sine_backend() -> FakeBackend {
    FakeBackend::new(48_000, sine_source())
}

fn test_net() -> NetOptions {
    NetOptions {
        port: 0,
        stun_servers: vec![],
        gather_lan: false,
        extra_candidates: vec![],
        allow_loopback_candidates: true,
    }
}

fn config(buffer: u32, frame_ms: f32) -> EngineConfig {
    EngineConfig {
        buffer_size: buffer,
        frame_ms,
        codec: CodecKind::Opus,
        channels: 1,
        input_channels: vec![0],
        ..Default::default()
    }
}

fn with_candidate(d: &LocalDescriptor, addr: SocketAddr) -> LocalDescriptor {
    LocalDescriptor { candidates: vec![Candidate { kind: CandidateKind::Lan, addr: addr.to_string() }], ..d.clone() }
}

fn local_addr(e: &Engine) -> SocketAddr {
    format!("127.0.0.1:{}", e.local_port().unwrap()).parse().unwrap()
}

#[derive(Debug, Default)]
struct Continuity {
    discontinuities: usize,
    silent_runs: usize,
    samples: usize,
    /// 처음 몇 개 이벤트의 시각(ms, sink 기준)
    events_ms: Vec<u64>,
}

/// 스테레오 인터리브 sink의 `skip_frames` 이후를 검사한다.
fn analyze(sink: &[f32], skip_frames: usize) -> Continuity {
    let mut c = Continuity::default();
    let mut prev: Option<f32> = None;
    let mut zero_run = 0;
    for (i, f) in sink.as_chunks::<2>().0.iter().enumerate().skip(skip_frames) {
        let x = f[0];
        let mut event = false;
        if let Some(p) = prev {
            if (x - p).abs() > 0.08 {
                c.discontinuities += 1;
                event = true;
            }
        }
        prev = Some(x);
        if x == 0.0 {
            zero_run += 1;
            if zero_run == 120 {
                c.silent_runs += 1;
                event = true;
            }
        } else {
            zero_run = 0;
        }
        if event && c.events_ms.len() < 8 {
            c.events_ms.push(i as u64 / 48);
        }
        c.samples += 1;
    }
    c
}

/// 끊김(무음 구간·불연속)은 셀 수 있는 사건(출력 장치 xrun, 로컬 FIFO 학습 언더런)으로만 설명되어야
/// 한다. 가짜 장치의 콜백 기상 지연이 장치 버퍼보다 크게 몰리면 xrun이 날 수 있는데(느린 CI 포함),
/// 그 한 번은 최대 3개 이벤트(끊김 시작·무음·복귀)를 만든다. 설명되지 않는 끊김은 엔진 내부 경로의
/// 결함이다(리뷰 H1: xrun 0에서 2초에 무음 200개).
fn assert_gaps_explained_by_xruns(c: &Continuity, xruns: u64, label: &str) {
    let gaps = (c.silent_runs + c.discontinuities) as u64;
    assert!(
        gaps <= 3 * xruns,
        "{label}: 끊김 {gaps}개(무음 {} 불연속 {})가 xrun {xruns}회로 설명되지 않음, 시각 {:?}ms",
        c.silent_runs,
        c.discontinuities,
        c.events_ms
    );
}

/// 손실/지연 주입 UDP 프록시. A ↔ pa ... pb ↔ B.
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
            // 프록시는 "네트워크"를 흉내 낸다. 실제 망은 로컬 CPU 부하에 느려지지 않으므로 높은 우선순위로 돈다.
            let _prio = band_lib::engine::prio::promote_current_thread();
            let mut rng = rand::rngs::StdRng::seed_from_u64(42);
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

/// A(사인) → B 연결. B의 sink를 돌려준다.
fn connect_pair(
    cfg: EngineConfig,
    backend_a: FakeBackend,
    backend_b: FakeBackend,
    via_proxy: Option<(f64, f64)>,
) -> (Engine, Engine, Option<Proxy>, Arc<Mutex<Vec<f32>>>) {
    let sink_b = backend_b.sink.clone();
    let a = Engine::start(cfg.clone(), Arc::new(backend_a), Arc::new(MixControls::default()), Some(("alice".into(), test_net()))).unwrap();
    let b = Engine::start(cfg, Arc::new(backend_b), Arc::new(MixControls::default()), Some(("bob".into(), test_net()))).unwrap();
    let (addr_a, addr_b) = (local_addr(&a), local_addr(&b));
    let proxy = via_proxy.map(|(loss, jitter)| Proxy::start(addr_a, addr_b, loss, jitter));
    let (to_b, to_a) = match &proxy {
        Some(p) => (p.pa, p.pb),
        None => (addr_b, addr_a),
    };
    a.add_peer("bob".into(), &with_candidate(b.descriptor().unwrap(), to_b)).unwrap();
    b.add_peer("alice".into(), &with_candidate(a.descriptor().unwrap(), to_a)).unwrap();
    assert!(wait_connected(&a, Duration::from_secs(5)), "A 연결 실패");
    assert!(wait_connected(&b, Duration::from_secs(5)), "B 연결 실패");
    (a, b, proxy, sink_b)
}

// ============================================================================
// 손실/지터/드리프트 루프백 (CI 실패 재현 조건 + 장치 버스트)
// ============================================================================

#[test]
fn loopback_loss5_jitter20_drift() {
    let _g = RT_LOCK.lock();
    // A의 장치 클럭 +200ppm, 양쪽 콜백 기상 지연 0~2ms(느린 러너 흉내)
    let mut backend_a = sine_backend().with_drift(200.0);
    backend_a.wake_jitter_us = 2000;
    let mut backend_b = FakeBackend::silence(48_000);
    backend_b.wake_jitter_us = 2000;
    backend_b.seed = 5;
    let (a, mut b, proxy, sink_b) = connect_pair(config(128, 5.0), backend_a, backend_b, Some((0.05, 20.0)));
    let proxy = proxy.unwrap();
    let t_connected = Instant::now();
    let sink_start = sink_b.lock().len();

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

    let mut trace: Vec<(f64, f32, f32)> = Vec::new();
    let mut corr_at_4s = None;
    while t_connected.elapsed() < Duration::from_secs(12) {
        std::thread::sleep(Duration::from_millis(500));
        let s = b.peer_jitter_stats()[0].1;
        let t = t_connected.elapsed().as_secs_f64();
        if t >= 4.0 && corr_at_4s.is_none() {
            corr_at_4s = Some(s);
        }
        trace.push((t, s.buffer_ms, s.target_ms));
    }
    let stats_b = b.stats();
    let js = b.peer_jitter_stats()[0].1;
    let rtt_a = a.stats().peers[0].rtt_ms;
    b.unsubscribe_mix();
    let mix_chunks = chunks.load(Ordering::Relaxed);

    let out = sink_b.lock().clone();
    let analyzed = &out[sink_start..];
    // 5ms 창 RMS(PLC는 무음이 아니므로 손실이 있어도 소리가 이어져야 함)
    let win = 240 * 2;
    let skip = 48_000 * 2 * 3;
    let (mut windows, mut sound) = (0, 0);
    for w in analyzed.get(skip..).unwrap_or(&[]).chunks_exact(win) {
        let rms = (w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).sqrt();
        windows += 1;
        if rms > 0.15 {
            sound += 1;
        }
    }
    let cont = analyze(analyzed, 48_000 * 3);
    let continuity = sound as f64 / windows.max(1) as f64;
    let conceal_ratio = js.concealed as f64 / js.produced.max(1) as f64;
    let mean = |from: f64, to: f64| {
        let v: Vec<f32> = trace.iter().filter(|t| t.0 >= from && t.0 < to).map(|t| t.1).collect();
        v.iter().sum::<f32>() / v.len().max(1) as f32
    };
    let mid = mean(4.0, 8.0);
    let end = mean(8.0, 12.5);
    let s4 = corr_at_4s.unwrap_or_default();

    println!("=== loopback 결과 (손실 5%, 지터 0..20ms, 송신 장치 +200ppm, 콜백 기상 지연 0..2ms) ===");
    println!("proxy: forwarded {} dropped {}", proxy.forwarded.load(Ordering::Relaxed), proxy.dropped.load(Ordering::Relaxed));
    println!("B←A 지터버퍼: {js:?}");
    println!("버퍼 지연 평균: 4~8s {mid:.1}ms, 8~12s {end:.1}ms, 목표 {:.1}ms", js.target_ms);
    println!("은닉 프레임 비율: {:.2}% (손실 추정 {:.1}%)", conceal_ratio * 100.0, js.loss_pct);
    println!("출력 연속성(5ms 창 중 소리 있음): {:.2}% ({sound}/{windows}), 무음 구간 {}", continuity * 100.0, cont.silent_runs);
    println!(
        "4초 이후 보정: 샘플 버림 {} 삽입 {} 패킷 버림 {}",
        js.samples_dropped - s4.samples_dropped,
        js.samples_inserted - s4.samples_inserted,
        js.dropped_frames - s4.dropped_frames
    );
    println!("B estLatencyMs(로컬): {:.1}, xruns {}, A RTT {:?}ms", stats_b.est_latency_ms, stats_b.xruns, rtt_a);
    println!("믹스 탭 청크: {mix_chunks}");
    println!("trace: {:?}", trace.iter().map(|t| (t.0 as f32, t.1)).collect::<Vec<_>>());

    assert!(continuity > 0.97, "출력 연속성 부족: {continuity}");
    // 손실은 PLC로 메워지므로 무음 구간은 장치 xrun으로만 생겨야 한다(+재버퍼링 여유 2)
    assert!(
        cont.silent_runs as u64 <= 2 + 3 * stats_b.xruns,
        "무음 구간 {} (xruns {}), 시각 {:?}ms",
        cont.silent_runs,
        stats_b.xruns,
        cont.events_ms
    );
    assert!(conceal_ratio < 0.10, "은닉 비율 과다: {conceal_ratio}");
    assert!(end < 50.0, "버퍼 지연 상한 초과: {end}ms");
    assert!((end - js.target_ms).abs() < 6.0, "버퍼 깊이가 목표 근처로 수렴하지 않음: {end} vs {}", js.target_ms);
    assert!((end - mid).abs() < 6.0, "지연 누적/발산: mid {mid} end {end}");
    // 정상 상태에서 패킷 단위 버림이 반복되면 제어가 발산하는 것(CI 실패: 155회)
    assert!(js.dropped_frames - s4.dropped_frames <= 3, "4초 이후 패킷 버림 과다: {js:?}");
    assert!(mix_chunks >= 400, "믹스 탭은 20ms마다(12초 ≈ 600개): {mix_chunks}");
    assert_eq!(bad_chunks.load(Ordering::Relaxed), 0);
    assert!(rtt_a.is_some(), "RTT 측정");

    drop(proxy);
    drop(a);
    b.stop();
}

// ============================================================================
// 리뷰 매트릭스: 모니터(H1) / 링크(M2·M4)
// ============================================================================

#[test]
fn monitor_matrix_no_gaps_with_large_device_buffers() {
    let _g = RT_LOCK.lock();
    // 리뷰에서 무음 200개/불연속 321개가 났던 480/2.5ms, 1024/5ms 포함
    for (buf, frame_ms) in [(128, 5.0), (128, 2.5), (480, 2.5), (480, 5.0), (1024, 5.0), (1024, 10.0), (1024, 2.5)] {
        let controls = Arc::new(MixControls::default());
        controls.monitor_enabled.store(true, Ordering::Relaxed);
        let mut backend = sine_backend();
        backend.wake_jitter_us = 1000;
        let sink = backend.sink.clone();
        let mut e = Engine::start(config(buf, frame_ms), Arc::new(backend), controls, None).unwrap();
        std::thread::sleep(Duration::from_millis(2000));
        let (corr, underruns) = e.local_path_stats();
        let st = e.stats();
        // 정지 과정의 꼬리(워커가 먼저 멈춘 뒤의 0)는 제외하고 정지 직전까지만 검사
        let snap = sink.lock().clone();
        e.stop();
        let c = analyze(&snap, 48_000 * 6 / 10);
        println!(
            "MONITOR buf {buf} / {frame_ms}ms: 불연속 {} 무음 {} (샘플 {}), 로컬 보정 {corr} 언더런 {underruns}, xruns {}, est {:.1}ms, 이벤트(ms) {:?}",
            c.discontinuities, c.silent_runs, c.samples, st.xruns, st.est_latency_ms, c.events_ms
        );
        assert!(c.samples > 48_000, "출력이 충분히 기록되어야 함");
        // 로컬 FIFO는 입·출력 콜백이 같은 시각에 몰리는(위상 고정) 장치에서 순서가 뒤집히면 한 번 패이고,
        // 그때 여유를 입력 방출 하나만큼 늘려 이후 안정된다(학습). 실시간 테스트는 이 학습 2회까지 허용.
        assert!(underruns <= 2, "buf {buf} / {frame_ms}ms: 로컬 FIFO 언더런 {underruns}");
        assert_gaps_explained_by_xruns(&c, st.xruns + underruns, &format!("MONITOR buf {buf} / {frame_ms}ms"));
    }
}

#[test]
fn link_matrix_clean_network_no_concealment_no_corrections() {
    let _g = RT_LOCK.lock();
    for (buf, frame_ms) in [(128, 5.0), (480, 2.5), (480, 5.0), (1024, 5.0)] {
        let mut ba = sine_backend();
        ba.wake_jitter_us = 1000;
        let mut bb = FakeBackend::silence(48_000);
        bb.wake_jitter_us = 1000;
        bb.seed = 9;
        let (a, b, _p, sink_b) = connect_pair(config(buf, frame_ms), ba, bb, None);
        let start = sink_b.lock().len() / 2;
        // 목표 깊이는 내릴 때 2초 유지를 요구하므로, 정착 후(4초~7초) 구간에서 보정을 센다.
        std::thread::sleep(Duration::from_millis(4000));
        let s1 = b.peer_jitter_stats()[0].1;
        std::thread::sleep(Duration::from_millis(3000));
        let s2 = b.peer_jitter_stats()[0].1;
        let xruns = b.stats().xruns;
        let snap = sink_b.lock().clone();
        drop(a);
        drop(b);
        let c = analyze(&snap, start + 48_000 * 3 / 2);
        let corr = s2.corrections() - s1.corrections();
        println!(
            "LINK buf {buf} / {frame_ms}ms: 불연속 {} 무음 {}, concealed {} (4s 이후 {}), 보정(4~7s) {corr}, buffer {:.1}ms target {:.1}ms, xruns {xruns}",
            c.discontinuities,
            c.silent_runs,
            s2.concealed,
            s2.concealed - s1.concealed,
            s2.buffer_ms,
            s2.target_ms
        );
        // 결정적 보장(정확히 0)은 단위 시뮬레이션(jitter::clean_network_zero_concealment_and_zero_corrections)이
        // 맡는다. 실시간 테스트는 스케줄러 지연 1회까지 허용한다(리뷰 M4 회귀는 5초에 6~8회였다).
        assert!(s2.concealed - s1.concealed <= 1, "buf {buf} / {frame_ms}ms: 지터 없는 링크에서 은닉 {}", s2.concealed - s1.concealed);
        assert!(corr <= 2, "buf {buf} / {frame_ms}ms: 드리프트 0에서 보정 {corr}회");
        assert_gaps_explained_by_xruns(&c, xruns, &format!("LINK buf {buf} / {frame_ms}ms"));
    }
}

#[test]
fn monitor_with_separate_device_clocks() {
    let _g = RT_LOCK.lock();
    // 입출력 장치가 다르다: 버퍼 크기·클럭·위상이 모두 다름 (리뷰 M5)
    for (in_ppm, out_ppm) in [(300.0, -300.0), (-300.0, 300.0)] {
        let controls = Arc::new(MixControls::default());
        controls.monitor_enabled.store(true, Ordering::Relaxed);
        let mut backend = sine_backend();
        backend.in_buffer = Some(480);
        backend.out_buffer = Some(128);
        backend.in_drift_ppm = in_ppm;
        backend.out_drift_ppm = out_ppm;
        backend.out_phase_us = 3700;
        backend.wake_jitter_us = 1000;
        let sink = backend.sink.clone();
        let mut e = Engine::start(config(256, 5.0), Arc::new(backend), controls, None).unwrap();
        std::thread::sleep(Duration::from_millis(5000));
        let (corr, underruns) = e.local_path_stats();
        let xruns = e.stats().xruns;
        let snap = sink.lock().clone();
        e.stop();
        let c = analyze(&snap, 48_000);
        println!(
            "MONITOR in 480 {in_ppm}ppm / out 128 {out_ppm}ppm: 불연속 {} 무음 {}, 로컬 보정 {corr}, 언더런 {underruns}, xruns {xruns}, 이벤트(ms) {:?}",
            c.discontinuities, c.silent_runs, c.events_ms
        );
        assert!(underruns <= 2, "로컬 FIFO 언더런 {underruns}");
        assert_gaps_explained_by_xruns(&c, xruns + underruns, &format!("MONITOR in {in_ppm} / out {out_ppm}"));
        assert!(corr > 0, "600ppm 차이면 보정이 있어야 함");
    }
}

#[test]
fn resampled_44k_devices_end_to_end() {
    let _g = RT_LOCK.lock();
    // 44.1kHz 장치: 입력·출력 리샘플 경로(모니터)와 링크(A 44.1k → B 44.1k)
    let mk = |sine: bool| {
        let src = if sine {
            Arc::new(|start: u64, ch: usize, buf: &mut [f32]| {
                for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                    let t = (start + i as u64) as f64 / 44_100.0;
                    frame.fill((t * SINE_HZ * 2.0 * std::f64::consts::PI).sin() as f32 * SINE_AMP);
                }
            }) as Arc<band_lib::engine::device::fake::SourceFn>
        } else {
            Arc::new(|_: u64, _: usize, buf: &mut [f32]| buf.fill(0.0)) as Arc<band_lib::engine::device::fake::SourceFn>
        };
        let mut b = FakeBackend::new(44_100, src);
        b.in_buffer = Some(441);
        b.out_buffer = Some(441);
        b
    };
    let (a, b, _p, sink_b) = connect_pair(config(256, 5.0), mk(true), mk(false), None);
    let start = sink_b.lock().len() / 2;
    std::thread::sleep(Duration::from_millis(3000));
    let js = b.peer_jitter_stats()[0].1;
    let xruns = b.stats().xruns;
    let snap = sink_b.lock().clone();
    drop(a);
    drop(b);
    let c = analyze(&snap, start + 44_100);
    println!("44.1k 링크: 불연속 {} 무음 {} concealed {} buffer {:.1}ms xruns {xruns}", c.discontinuities, c.silent_runs, js.concealed, js.buffer_ms);
    assert_gaps_explained_by_xruns(&c, xruns, "44.1k 링크");
    assert_eq!(js.concealed, 0, "지터 없는 링크에서 은닉");
}

// ============================================================================
// 지연 측정(M12), 장치 분리(M9)
// ============================================================================

#[test]
fn impulse_latency_matches_estimate() {
    let _g = RT_LOCK.lock();
    // 임펄스(값 0.9)를 12007샘플마다 넣어 모니터 경로의 종단 지연을 잰다.
    const PERIOD: u64 = 12_007;
    let src: Arc<band_lib::engine::device::fake::SourceFn> = Arc::new(|start: u64, ch: usize, buf: &mut [f32]| {
        for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
            let idx = start + i as u64;
            frame.fill(if idx % PERIOD == 100 { 0.9 } else { 0.0 });
        }
    });
    for (buf, frame_ms) in [(256, 5.0), (480, 2.5), (128, 10.0)] {
        let controls = Arc::new(MixControls::default());
        controls.monitor_enabled.store(true, Ordering::Relaxed);
        let mut backend = FakeBackend::new(48_000, src.clone());
        backend.out_phase_us = 1300;
        let phase = backend.out_phase_us as f64 / 1e6;
        let sink = backend.sink.clone();
        let mut e = Engine::start(config(buf, frame_ms), Arc::new(backend), controls, None).unwrap();
        std::thread::sleep(Duration::from_millis(3000));
        let est = e.stats().est_latency_ms as f64;
        e.stop();
        let out = sink.lock().clone();
        // 출력의 임펄스 위치 j → 지연 = phase + j/rate − i/rate, i = k·PERIOD + 100
        let mut lat = Vec::new();
        let mut last_k = None;
        for (j, f) in out.as_chunks::<2>().0.iter().enumerate() {
            if f[0] > 0.5 {
                let t_play = phase + j as f64 / 48_000.0;
                // 가장 가까운 이전 임펄스
                let k = ((t_play * 48_000.0 - 100.0) / PERIOD as f64).floor() as i64;
                if k >= 0 && last_k != Some(k) && t_play > 1.0 {
                    let t_cap = (k as f64 * PERIOD as f64 + 100.0) / 48_000.0;
                    lat.push((t_play - t_cap) * 1000.0);
                    last_k = Some(k);
                }
            }
        }
        assert!(lat.len() >= 5, "임펄스 검출 {}", lat.len());
        let mean = lat.iter().sum::<f64>() / lat.len() as f64;
        // est는 첫 샘플 기준(입력 버퍼 전체 + 출력 버퍼 전체). 평균 지연은 그보다 (Bin+Bout)/2 작다.
        let est_mean = est - buf as f64 / 48.0;
        println!("임펄스 지연 buf {buf} / {frame_ms}ms: 평균 {mean:.2}ms (n={}), est {est:.2}ms → 평균 환산 {est_mean:.2}ms, 범위 {:.2}..{:.2}", lat.len(), lat.iter().cloned().fold(f64::MAX, f64::min), lat.iter().cloned().fold(0.0, f64::max));
        assert!((mean - est_mean).abs() < 2.5, "estLatencyMs가 실측과 다름: 실측 {mean:.2} vs 추정 {est_mean:.2}");
    }
}

#[test]
fn device_lost_is_reported() {
    let _g = RT_LOCK.lock();
    let mut backend = FakeBackend::silence(48_000);
    backend.error_after = Some(Duration::from_millis(300));
    let mut e = Engine::start(config(128, 5.0), Arc::new(backend), Arc::new(MixControls::default()), None).unwrap();
    assert!(!e.stats().device_lost);
    std::thread::sleep(Duration::from_millis(700));
    let s = e.stats();
    e.stop();
    assert!(s.device_lost, "{s:?}");
    assert_eq!(s.stream_errors, 1);
    let j = serde_json::to_string(&s).unwrap();
    assert!(j.contains("\"deviceLost\":true") && j.contains("\"streamErrors\":1"));
}

// ============================================================================
// 커맨드 계층(H3), 수명
// ============================================================================

#[test]
fn manager_stays_responsive_while_starting() {
    let _g = RT_LOCK.lock();
    let mut backend = FakeBackend::silence(48_000);
    backend.open_delay = Duration::from_millis(1500);
    let m = Arc::new(EngineManager::new(Arc::new(backend), test_net()));
    let m2 = m.clone();
    let starter = std::thread::spawn(move || m2.start(config(128, 5.0), "me".into()));
    std::thread::sleep(Duration::from_millis(200));
    assert!(m.is_starting());
    // 시작 중에도 다른 커맨드는 즉시 응답(락을 쥔 채 블로킹하지 않음)
    let t0 = Instant::now();
    let s = m.stats();
    let err = m.with_engine(|_| Ok(())).unwrap_err();
    assert!(t0.elapsed() < Duration::from_millis(50), "stats가 막힘: {:?}", t0.elapsed());
    assert!(!s.running);
    assert!(err.contains("시작 중"), "{err}");
    let desc = starter.join().unwrap().unwrap();
    assert!(desc.ssrc != 0 || !desc.key.is_empty());
    assert!(m.stats().running);

    // 시작 중 stop → 그 시작은 취소된다
    let m3 = m.clone();
    let starter = std::thread::spawn(move || m3.start(config(128, 5.0), "me".into()));
    std::thread::sleep(Duration::from_millis(300));
    m.stop();
    let r = starter.join().unwrap();
    assert!(r.is_err(), "취소되어야 함");
    assert!(!m.stats().running, "stop 이후 엔진이 남으면 안 됨");
}

#[test]
fn test_mode_restores_monitor_setting() {
    let _g = RT_LOCK.lock();
    let m = EngineManager::new(Arc::new(FakeBackend::silence(48_000)), test_net());
    assert!(!m.controls().monitor_enabled.load(Ordering::Relaxed));
    m.test_start(config(128, 5.0)).unwrap();
    assert!(m.controls().monitor_enabled.load(Ordering::Relaxed), "테스트 모드는 모니터를 켬");
    m.test_stop();
    assert!(!m.controls().monitor_enabled.load(Ordering::Relaxed), "끝나면 원래 값(꺼짐)으로 복원(리뷰 L4)");
    // 테스트 모드에서 바로 실제 세션으로 넘어가도 복원
    m.test_start(config(128, 5.0)).unwrap();
    m.start(config(128, 5.0), "me".into()).unwrap();
    assert!(!m.controls().monitor_enabled.load(Ordering::Relaxed));
    m.stop();
}

#[test]
fn repeated_start_stop_joins_threads() {
    let _g = RT_LOCK.lock();
    let controls = Arc::new(MixControls::default());
    for i in 0..5 {
        let t0 = Instant::now();
        let mut e = Engine::start(
            config(128, 5.0),
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
    let _g = RT_LOCK.lock();
    let controls = Arc::new(MixControls::default());
    controls.monitor_enabled.store(true, Ordering::Relaxed);
    let backend = sine_backend();
    let sink = backend.sink.clone();
    let mut e = Engine::start(config(128, 5.0), Arc::new(backend), controls, None).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let s = e.stats();
    e.stop();
    let out = sink.lock().clone();
    let tail = &out[out.len().saturating_sub(48_000)..];
    let rms = (tail.iter().map(|x| x * x).sum::<f32>() / tail.len().max(1) as f32).sqrt();
    println!("모니터 모드: rms {rms:.3}, estLatencyMs {:.1}, xruns {}", s.est_latency_ms, s.xruns);
    assert!((rms - SINE_AMP / 2f32.sqrt()).abs() < 0.05, "모니터 출력 rms {rms}");
    assert!(s.est_latency_ms < 25.0, "로컬 지연 {}", s.est_latency_ms);
}

#[test]
fn input_mute_silences_monitor_and_tap_but_keeps_meter() {
    let _g = RT_LOCK.lock();
    let controls = Arc::new(MixControls::default());
    controls.monitor_enabled.store(true, Ordering::Relaxed);
    controls.input_muted.store(true, Ordering::Relaxed);
    let backend = sine_backend();
    let sink = backend.sink.clone();
    let mut e = Engine::start(config(128, 5.0), Arc::new(backend), controls.clone(), None).unwrap();
    let tap_energy = Arc::new(Mutex::new((0usize, 0f64)));
    let te = tap_energy.clone();
    e.subscribe_mix(Box::new(move |bytes| {
        assert_eq!(bytes.len() % 4, 0);
        let mut g = te.lock();
        for b in bytes.as_chunks::<4>().0 {
            let v = f32::from_le_bytes(*b) as f64;
            g.1 += v * v;
        }
        g.0 += bytes.len() / 4;
    }))
    .unwrap();
    std::thread::sleep(Duration::from_millis(600));
    let s = e.stats();
    let muted_len = sink.lock().len();
    let muted_rms = {
        let out = sink.lock();
        let tail = &out[muted_len.saturating_sub(24_000)..muted_len];
        (tail.iter().map(|x| x * x).sum::<f32>() / tail.len().max(1) as f32).sqrt()
    };
    let (n, energy) = *tap_energy.lock();
    controls.input_muted.store(false, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(400));
    e.stop();
    let out = sink.lock().clone();
    let tail = &out[out.len() - 24_000..];
    let unmuted_rms = (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt();
    println!("입력 뮤트: 출력 rms {muted_rms:.4}, 탭 rms {:.4}, inputLevel {:.3} → 해제 후 rms {unmuted_rms:.3}", (energy / n.max(1) as f64).sqrt(), s.input_level);
    assert!(muted_rms < 1e-4, "뮤트 중 모니터 출력은 무음");
    assert!(n > 0 && energy < 1e-6, "뮤트 중 믹스 탭도 무음");
    assert!((s.input_level - SINE_AMP / 2f32.sqrt()).abs() < 0.03, "inputLevel {}", s.input_level);
    assert!(unmuted_rms > 0.3, "뮤트 해제 후 복귀");
}

#[test]
fn peer_pan_and_levels() {
    let _g = RT_LOCK.lock();
    let (a, b, _p, sink_b) = connect_pair(config(128, 5.0), sine_backend(), FakeBackend::silence(48_000), None);
    b.set_peer_gain("alice", 1.0, false, Some(1.0)).unwrap();
    std::thread::sleep(Duration::from_millis(700));
    let st = b.stats();
    let out = sink_b.lock().clone();
    let tail = &out[out.len() - 48_000..];
    let (mut l, mut r) = (0f32, 0f32);
    for f in tail.as_chunks::<2>().0 {
        l += f[0] * f[0];
        r += f[1] * f[1];
    }
    let n = (tail.len() / 2) as f32;
    let (l, r) = ((l / n).sqrt(), (r / n).sqrt());
    let peer_level = st.peers[0].level;
    println!("pan=+1: L rms {l:.4}, R rms {r:.4}, peer level {peer_level:.3}, outputLevel {:.3}", st.output_level);
    assert!(l < 0.01, "오른쪽 끝 팬이면 왼쪽은 무음: {l}");
    assert!(r > 0.4, "오른쪽은 √2 배: {r}");
    assert!((0.0..=1.0).contains(&peer_level) && peer_level > 0.25, "피어 레벨은 0..1 RMS: {peer_level}");
    assert!((0.0..=1.0).contains(&st.output_level));
    assert!(b.set_peer_gain("nobody", 1.0, false, None).is_err());
    drop(a);
}

// ============================================================================
// 보안(M7 사칭·ssrc 충돌, M8 후보 폭주, M6 경로 고정)
// ============================================================================

#[test]
fn ssrc_squatting_and_forged_packets_do_not_hijack_peer() {
    let _g = RT_LOCK.lock();
    let (a, b, _p, sink_b) = connect_pair(config(128, 5.0), sine_backend(), FakeBackend::silence(48_000), None);
    let desc_a = a.descriptor().unwrap().clone();
    let desc_b = b.descriptor().unwrap().clone();

    // 1) 악의적 C가 A의 ssrc로 descriptor를 보낸다 → B는 받아들이되 A 항목을 지우지 않는다.
    let c_keys = KeyPair::generate();
    let squat = LocalDescriptor {
        ssrc: desc_a.ssrc,
        key: LocalDescriptor::encode_key(&c_keys.public_bytes()),
        candidates: vec![Candidate { kind: CandidateKind::Public, addr: "127.0.0.1:9".into() }],
    };
    b.add_peer("carol".into(), &squat).unwrap();
    assert_eq!(b.stats().peers.len(), 2, "A 항목이 남아 있어야 함");

    // 2) C가 A를 사칭한 Hello/Bye를 B에게 보낸다(자기 키로 유도한 A→B 키로 암호화).
    let attacker = UdpSocket::bind("127.0.0.1:0").unwrap();
    let b_pub = desc_b.decode_key().unwrap();
    let (forge_tx, _) = c_keys.pair_ciphers(desc_a.ssrc, &b_pub, desc_b.ssrc).unwrap();
    let mut buf = [0u8; MAX_PACKET_BYTES];
    for (i, t) in [PacketType::Hello, PacketType::Bye, PacketType::Hello, PacketType::Hello, PacketType::Hello, PacketType::Hello, PacketType::Hello]
        .into_iter()
        .enumerate()
    {
        let h = Header { ptype: t, ssrc: desc_a.ssrc, seq: 1_000_000 + i as u32, timestamp: 0, flags: 0, frame_code: 0 };
        let payload: &[u8] = if t == PacketType::Hello { b"alice" } else { b"" };
        let n = forge_tx.seal(&h, payload, &mut buf).unwrap();
        assert!(n >= HEADER_BYTES);
        attacker.send_to(&buf[..n], local_addr(&b)).unwrap();
    }
    std::thread::sleep(Duration::from_millis(600));
    let st = b.stats();
    let alice = st.peers.iter().find(|p| p.peer_id == "alice").unwrap();
    assert_eq!(alice.state, PeerStateKind::Connected, "사칭 Bye로 끊기면 안 됨");
    // B는 여전히 A의 소리를 듣는다
    let out = sink_b.lock().clone();
    let tail = &out[out.len() - 24_000..];
    let rms = (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt();
    assert!(rms > 0.3, "A 오디오가 계속 들려야 함: {rms}");
    drop(a);
}

#[test]
fn malicious_descriptors_rejected() {
    let _g = RT_LOCK.lock();
    let mut strict = test_net();
    strict.allow_loopback_candidates = false;
    let e = Engine::start(config(128, 5.0), Arc::new(FakeBackend::silence(48_000)), Arc::new(MixControls::default()), Some(("me".into(), strict))).unwrap();
    let other = KeyPair::generate();
    let mk = |cands: Vec<String>| LocalDescriptor {
        ssrc: 77,
        key: LocalDescriptor::encode_key(&other.public_bytes()),
        candidates: cands.into_iter().map(|addr| Candidate { kind: CandidateKind::Public, addr }).collect(),
    };
    // 후보 폭주(반사 공격)
    let flood: Vec<String> = (0..1000).map(|i| format!("198.51.{}.{}:4000", i / 250, i % 250)).collect();
    assert!(e.add_peer("x".into(), &mk(flood)).is_err());
    // 특수 주소
    for bad in ["0.0.0.0:4000", "224.0.0.251:5353", "255.255.255.255:4000", "198.51.100.1:0", "127.0.0.1:4000"] {
        assert!(e.add_peer("x".into(), &mk(vec![bad.into()])).is_err(), "{bad}");
    }
    // 자기 자신
    let own = e.descriptor().unwrap().clone();
    assert!(e.add_peer("x".into(), &with_candidate(&own, "198.51.100.1:4000".parse().unwrap())).is_err());
    // 정상
    assert!(e.add_peer("x".into(), &mk(vec!["198.51.100.1:4000".into()])).is_ok());
    // 잘못된 config
    let bad = EngineConfig { codec: CodecKind::Pcm16, channels: 2, input_channels: vec![0, 1], frame_ms: 10.0, ..config(128, 5.0) };
    assert!(Engine::start(bad, Arc::new(FakeBackend::silence(48_000)), Arc::new(MixControls::default()), None).is_err());
}
