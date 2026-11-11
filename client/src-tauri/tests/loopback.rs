//! 통합 테스트(설계 문서 7절, 엔진 리뷰 "테스트 신뢰도" 반영).
//!
//! 가짜 장치(`FakeBackend`)는 입력/출력을 별도 스레드·별도 클럭으로 돌리고 콜백 기상 지연(버스트)을
//! 흉내 낸다. 출력 검사는 RMS 창이 아니라 **샘플 단위**로 한다:
//! - 불연속 = L 채널 인접 샘플 차 > 0.08 (440Hz, 진폭 0.5 사인의 최대 차는 0.029)
//! - 무음 구간 = 정확히 0인 샘플이 120개 이상 연속
//!
//! 실시간 테스트끼리 CPU를 다투면 느린 CI에서 흔들리므로 `RT_LOCK`으로 직렬화한다.
//!
//! ## 두 단계(P3-0)
//! - **결정적**(기본 `cargo test`): 기능 검사(연결, 뮤트, 팬, 장치 분리, 시작/정지 직렬화, 악성 입력 등).
//!   벽시계 품질 임계(은닉 수·끊김·지연 ms)가 없어 러너 속도와 무관하다. 고정 sleep도 쓰지 않는다(P3-0a):
//!   기다림은 조건(`wait_until`: 상태·출력 진행량·받은 패킷 수)이나 동기화(`OpenGate`, 시작 세대)로 하고,
//!   벽시계는 행 감지 상한(`HANG` 20초)에만 쓴다. 출력 검사는 "조건을 만족하는 창이 나온다"로 보아 xrun 구멍과 무관하다.
//! - **실시간**(`#[ignore = "realtime: ..."]`): 실제 스레드·실제 시계로 은닉·끊김·지연 임계를 재는 테스트.
//!   러너 스케줄링(워커가 은닉 마감 1.5ms 넘게 못 깨어남 등)에 흔들리므로 CI `realtime` 잡이 nextest로 하나씩,
//!   실패 시 총 3회까지 돌린다. 각 테스트의 ignore 사유에 같은 시나리오를 가상 시간으로 검증하는 **결정적 짝**을
//!   적었다(엔진 단위 테스트, 허용치 0). 판별: 짝이 깨지면 엔진 회귀, 짝은 녹색인데 실시간만 3회 연속 깨지면
//!   환경 또는 시뮬레이션이 놓친 경로 → 로그의 수치(은닉/xrun/이벤트 시각)로 조사한다.
//!   로컬 실행: `cargo test --test loopback -- --ignored --test-threads 1` 또는
//!   `cargo nextest run --run-ignored only --retries 2 --test-threads 1`.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use band_lib::engine::config::{CodecKind, EngineConfig, HEADER_BYTES, MAX_PACKET_BYTES};
use band_lib::engine::device::fake::{FakeBackend, OpenGate};
use band_lib::engine::jitter as jitter_stats;
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
        cancel: None,
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

/// 결정적 테스트의 대기 상한. 판정은 조건 자체로 하고, 벽시계는 행(hang) 감지에만 쓴다(정상 소요의 수십 배).
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

/// `f`를 다른 스레드에서 돌려 상한 안에 끝나면 결과를 돌려준다(막힘 = None, 테스트가 영영 멈추지 않게).
fn within<T: Send + 'static>(timeout: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).ok()
}

/// 스테레오 인터리브 sink에서 샘플 인덱스 `from` 이후 마지막 `frames` 프레임의 (L, R) RMS. 아직 모자라면 None.
/// 시작 지연·xrun과 무관하게 "출력이 이렇게 나온 구간이 있다"를 보려고 짧은 창을 반복해서 본다.
fn tail_rms_after(sink: &Mutex<Vec<f32>>, from: usize, frames: usize) -> Option<(f32, f32)> {
    let out = sink.lock();
    let start = out.len().checked_sub(frames * 2)?;
    if start < from {
        return None;
    }
    let (mut l, mut r) = (0f32, 0f32);
    for f in out[start..].as_chunks::<2>().0 {
        l += f[0] * f[0];
        r += f[1] * f[1];
    }
    Some(((l / frames as f32).sqrt(), (r / frames as f32).sqrt()))
}

/// 지터 버퍼가 실제 패킷으로 만든 프레임 수(은닉 제외). 벽시계 대신 "그 뒤로 패킷을 이만큼 받았다"는 진행량 기준.
fn real_frames(e: &Engine, peer: &str) -> u64 {
    e.peer_jitter_stats().iter().find(|(id, _)| id == peer).map(|(_, js)| js.produced.saturating_sub(js.concealed)).unwrap_or(0)
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
    assert!(wait_connected(&a, HANG), "A 연결 실패");
    assert!(wait_connected(&b, HANG), "B 연결 실패");
    (a, b, proxy, sink_b)
}

// ============================================================================
// 손실/지터/드리프트 루프백 (CI 실패 재현 조건 + 장치 버스트)
// ============================================================================

#[test]
#[ignore = "realtime: 결정적 짝 engine::jitter::tests::{jitter_and_loss_opus, drift_plus_200ppm_converges, drift_minus_200ppm_converges}"]
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
    let (fills, skipped) = b.feed_stats();
    let (_, local_underruns) = b.local_path_stats();
    println!(
        "B estLatencyMs(로컬): {:.1}, xruns {} (피어 마감 채움 {fills}, 콜백 언더런으로 건너뜀 {skipped}프레임, 로컬 FIFO 언더런 {local_underruns}), A RTT {:?}ms",
        stats_b.est_latency_ms, stats_b.xruns, rtt_a
    );
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
#[ignore = "realtime: 결정적 짝 engine::worker::tests::local_fifo_handles_bursts_matrix"]
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
#[ignore = "realtime: 결정적 짝 engine::jitter::tests::clean_network_zero_concealment_and_zero_corrections, engine::worker::tests::callback_mix_sim_phase_and_buffer_matrix"]
fn link_matrix_clean_network_no_concealment_no_corrections() {
    let _g = RT_LOCK.lock();
    // (장치 버퍼, 프레임, 목표 고정 중 보정 0 판정). 1024 / 5ms는 송신이 21.3ms마다 4~5패킷으로 몰려 최소 transit 후보가
    // 버스트당 하나(프레임 경계가 콜백 끝과 맞는 패킷)뿐이고, 콜백 기상 지연(1ms) 때문에 창의 가장 이른 표본이 들고 날 때마다
    // 기준 min이 0.5초에 ~1ms씩 오르내린다(P3-0d 실측: 6초에 −1.25 ~ +1.17ms). 깊이는 min 기준이라 바로 따라 움직이고 짧은 창
    // 추정은 hold(2초)로 남으므로, 목표(상대)가 그대로인 동안에도 절대 목표(min + 목표)가 움직여 보정이 생긴다. 그래서 이
    // 조합은 "보정은 절대 목표가 움직인 만큼만"으로 본다(아래). 결정적 짝(기상 지연 없는 256/1024 시뮬레이션)은 보정 0이다.
    let cases: [(u32, f32, bool); 4] = [(128, 5.0, true), (480, 2.5, true), (480, 5.0, true), (1024, 5.0, false)];
    for (buf, frame_ms, strict) in cases {
        let mut ba = sine_backend();
        ba.wake_jitter_us = 1000;
        let mut bb = FakeBackend::silence(48_000);
        bb.wake_jitter_us = 1000;
        bb.seed = 9;
        let (a, b, _p, sink_b) = connect_pair(config(buf, frame_ms), ba, bb, None);
        let start = sink_b.lock().len() / 2;
        // 3초~9초를 0.25초 간격으로 관찰한다. 콜백 기상 지연(1ms)과 송신 버스트 때문에 p95가 프레임 경계를
        // 넘나들면 목표가 한 단계 바뀔 수 있다(목표 재설정 → 정당한 보정). 이 테스트가 확인하는 것은
        // "드리프트 0이면 목표가 고정된 동안 보정이 없다"는 것이므로, 목표가 바뀐 구간과 그 뒤 따라잡기
        // 구간(목표 차이 ÷ 패킷당 최소 보정 속도 + 여유)은 세지 않는다(PR #5 M-1).
        std::thread::sleep(Duration::from_millis(3000));
        let mut snaps: Vec<(Instant, jitter_stats::JitterStats)> = Vec::new();
        for _ in 0..24 {
            snaps.push((Instant::now(), b.peer_jitter_stats()[0].1));
            std::thread::sleep(Duration::from_millis(250));
        }
        let xruns = b.stats().xruns;
        let snap = sink_b.lock().clone();
        drop(a);
        drop(b);
        let c = analyze(&snap, start + 48_000 * 3 / 2);
        let (first, last) = (snaps[0].1, snaps[snaps.len() - 1].1);
        let mut stable_corr = 0;
        let mut stable_secs = 0.0;
        let mut retarget_until: Option<Instant> = None;
        // 절대 목표(샘플) = 최소 transit + 목표. 관찰 구간에서 움직인 경로 길이(오르내림 합)를 함께 잰다.
        let abs_of = |s: &jitter_stats::JitterStats| s.min_transit as f64 + s.target_ms as f64 * 48.0;
        let mut abs_path = 0.0;
        for w in snaps.windows(2) {
            let ((t0, s0), (t1, s1)) = (w[0], w[1]);
            abs_path += (abs_of(&s1) - abs_of(&s0)).abs();
            if (s1.target_ms - s0.target_ms).abs() > 0.01 {
                // 목표가 바뀌었다: 깊이를 새 목표까지 옮기는 데 걸리는 시간 + 0.5초는 제외한다.
                // 따라잡는 시간은 엔진 사양(보정 속도 상한 `jitter::MAX_CORR_RATE` = 0.25%, PR #20 H1)에서 그대로 계산하고,
                // 보정이 끝날 때 위상 정리(최대 1패킷)를 위해 1프레임을 더한다. 이전 식(패킷당 1샘플)은 상한 이전의 보정 속도였다.
                // 단언(목표 고정 중 보정 0)과 0.5초 여유는 그대로다. 사양 변경 근거: 국소 피치 편차 14.5 → 4.3센트, 지터 회선 보정
                // 시간 51.6 → 15.7%, 은닉 1.11 → 1.21%(engine-notes "PR #20 리뷰 반영"). 따라잡기가 무한히 느려지는 회귀는
                // `link_target_change_converges`가 상한으로 잡는다.
                let samples = ((s1.target_ms - s0.target_ms).abs() * 48.0) as f64;
                let catch_up = Duration::from_secs_f64(jitter_stats::correction_catch_up_secs(samples) + frame_ms as f64 / 1000.0 + 0.5);
                retarget_until = Some(t1 + catch_up);
                continue;
            }
            if retarget_until.is_some_and(|u| t1 <= u) {
                continue;
            }
            stable_corr += s1.corrections() - s0.corrections();
            stable_secs += (t1 - t0).as_secs_f64();
        }
        let corr = last.corrections() - first.corrections();
        println!(
            "LINK buf {buf} / {frame_ms}ms: 불연속 {} 무음 {}, concealed(3~9s) {}, 목표 {:.1}→{:.1}ms, 목표 고정 {stable_secs:.1}s 동안 보정 {stable_corr}, 전체 보정 {corr}(절대 목표 이동 {:.0}샘플, min {:+.2}ms, 최대 {:.1}센트), buffer {:.1}ms, xruns {xruns}",
            c.discontinuities,
            c.silent_runs,
            last.concealed - first.concealed,
            first.target_ms,
            last.target_ms,
            abs_path,
            (last.min_transit - first.min_transit) as f64 / 48.0,
            last.max_corr_cents,
            last.buffer_ms,
        );
        if strict {
            assert!(stable_secs >= 3.0, "buf {buf} / {frame_ms}ms: 목표가 고정된 구간이 3초 이상 있어야 함({stable_secs:.1}s)");
            assert_eq!(stable_corr, 0, "buf {buf} / {frame_ms}ms: 드리프트 0, 목표 고정 중 보정 {stable_corr}회");
        } else {
            // 드리프트 0이면 깊이를 옮길 이유는 기준(절대 목표)의 이동뿐이다: 보정량은 관찰 구간의 절대 목표 경로 길이를 넘지
            // 않는다(+ 관찰 시작 전 이동의 따라잡기 몫 1격자). 리뷰 M4류 회귀(목표·기준이 그대로인데 계속 보정)는 이 상한을 넘는다.
            let bound = abs_path + jitter_stats::TARGET_GRID;
            assert!(corr as f64 <= bound, "buf {buf} / {frame_ms}ms: 보정 {corr}샘플이 절대 목표 이동 {abs_path:.0}샘플(+격자)로 설명되지 않음");
        }
        // 보정은 속도 상한 리샘플러로만 한다(국소 피치 ≤ 4.3센트, 지속음 JND 5~10센트 아래).
        assert!(last.max_corr_cents <= 4.4, "buf {buf} / {frame_ms}ms: 보정 피치 {:.1}센트", last.max_corr_cents);
        // 결정적 보장(정확히 0)은 단위 시뮬레이션(jitter::clean_network_zero_concealment_and_zero_corrections)이
        // 맡는다. 실시간 테스트는 스케줄러 지연 1회까지 허용한다(리뷰 M4 회귀는 5초에 6~8회였다).
        assert!(last.concealed - first.concealed <= 1, "buf {buf} / {frame_ms}ms: 지터 없는 링크에서 은닉 {}", last.concealed - first.concealed);
        assert_gaps_explained_by_xruns(&c, xruns, &format!("LINK buf {buf} / {frame_ms}ms"));
    }
}

#[test]
#[ignore = "realtime: 결정적 짝 engine::jitter::tests::target_change_converges_within_rate_bound"]
fn link_target_change_converges() {
    let _g = RT_LOCK.lock();
    // PR #20: 보정 속도에 상한(0.25%)을 둔 대신, 목표가 바뀐 뒤 깊이가 새 목표로 수렴하는 시간에 상한을 단언한다.
    // 상한 = 엔진 상수로 계산한 이론값(`correction_catch_up_secs`) × 1.2 + 1프레임 + 관찰 주기(50ms). 프로필 균형 → 안정으로
    // 바꾸면 목표가 오르고(pad 0.5F + 분위수), 다시 균형으로 바꾸면 hold 뒤 내려간다. 따라잡기가 느려지는 회귀를 잡는다.
    let frame_ms = 5.0f32;
    let mut ba = sine_backend();
    ba.wake_jitter_us = 500;
    let mut bb = FakeBackend::silence(48_000);
    bb.wake_jitter_us = 500;
    bb.seed = 9;
    let (a, b, _p, _sink_b) = connect_pair(config(128, frame_ms), ba, bb, None);
    std::thread::sleep(Duration::from_millis(3000));
    let (exit_drop, exit_ins) = jitter_stats::correction_exit_ms(240);
    let poll = Duration::from_millis(50);
    // `target0`는 프로필을 바꾸기 직전 목표, `t0`는 바꾼 시각.
    let measure = |b: &Engine, label: &str, target0: f32, t0: Instant| -> (f64, f64, f64) {
        // 목표가 정해질 때까지(설정 직후 바로 바뀌거나 다음 통계 갱신에서 바뀜) 잠깐 본다.
        std::thread::sleep(Duration::from_millis(300));
        let mut target;
        let deadline = Instant::now() + Duration::from_secs(15);
        let converged = loop {
            let s = b.peer_jitter_stats()[0].1;
            let err = s.buffer_ms - s.target_ms;
            target = s.target_ms;
            if err <= exit_drop as f32 + 0.05 && err >= -(exit_ins as f32) - 0.05 {
                break t0.elapsed().as_secs_f64();
            }
            assert!(Instant::now() < deadline, "{label}: 15초 안에 수렴하지 않음: {s:?}");
            std::thread::sleep(poll);
        };
        let diff = (target - target0) as f64;
        println!("TARGET-CHANGE {label}: 목표 {target0:.2} → {target:.2}ms, 수렴 {converged:.2}s");
        (diff, converged, target as f64)
    };
    let before = b.peer_jitter_stats()[0].1.target_ms;
    let t_set = Instant::now();
    b.set_jitter_profile(1.0).unwrap();
    let (diff_up, t_up, _) = measure(&b, "균형→안정", before, t_set);
    // 내려가는 쪽: 안정 → 균형. pad(0.5F)는 바로 빠지고 깊이는 chase로 따라 내려간다(짧은 창 분위수 몫은 hold 뒤에 내려가므로
    // 이 측정 창 밖이다).
    let before = b.peer_jitter_stats()[0].1.target_ms;
    let t_set = Instant::now();
    b.set_jitter_profile(0.5).unwrap();
    let (diff_down, t_down, _) = measure(&b, "안정→균형", before, t_set);
    drop(a);
    drop(b);
    let frame_s = frame_ms as f64 / 1000.0;
    let slack = poll.as_secs_f64() + 0.3; // 관찰 주기 + 목표 확정 대기(위 300ms)
    let bound = |diff: f64| jitter_stats::correction_catch_up_secs(diff * 48.0) * 1.2 + frame_s + slack;
    assert!(diff_up > 1.0, "전제: 안정 프로필로 목표가 1ms 넘게 올라야 함({diff_up:.2})");
    assert!(t_up <= bound(diff_up), "상향 수렴 {t_up:.2}s > 상한 {:.2}s", bound(diff_up));
    assert!(diff_down < -1.0, "전제: 균형으로 돌아가면 목표가 1ms 넘게 내려가야 함({diff_down:.2})");
    assert!(t_down <= bound(diff_down), "하향 수렴 {t_down:.2}s > 상한 {:.2}s", bound(diff_down));
}

#[test]
#[ignore = "realtime: 결정적 짝 engine::worker::tests::local_fifo_tracks_device_clock_difference"]
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
#[ignore = "realtime: 결정적 짝 engine::resample::tests::*, engine::worker::tests::callback_mix_sim_phase_and_buffer_matrix"]
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
    let fs = b.feed_stats();
    let xruns = b.stats().xruns;
    let snap = sink_b.lock().clone();
    drop(a);
    drop(b);
    let c = analyze(&snap, start + 44_100);
    println!("44.1k 링크: 불연속 {} 무음 {} concealed {} buffer {:.1}ms xruns {xruns}, 마감 채움/건너뜀 {:?}", c.discontinuities, c.silent_runs, js.concealed, js.buffer_ms, fs);
    assert_gaps_explained_by_xruns(&c, xruns, "44.1k 링크");
    assert_eq!(js.concealed, 0, "지터 없는 링크에서 은닉");
}

// ============================================================================
// 지연 측정(M12), 장치 분리(M9)
// ============================================================================

#[test]
#[ignore = "realtime: 결정적 짝 est_latency_budget_table(추정 공식), engine::worker::tests::local_fifo_*"]
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
        let est = e.stats().monitor_latency_ms as f64;
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

/// A(입력 임펄스) → B(출력) 종단 간 지연을 공유 클럭으로 잰다. 반환: (지연 목록 ms, A stats, B stats, B 지터 통계).
fn link_impulse_latency(
    cfg: EngineConfig,
    seconds: u64,
) -> (Vec<f64>, band_lib::engine::stats::EngineStats, band_lib::engine::stats::EngineStats, jitter_stats::JitterStats) {
    const PERIOD: u64 = 9_601; // 0.2초, 장치 버퍼·프레임과 서로소
    let src: Arc<band_lib::engine::device::fake::SourceFn> = Arc::new(|start: u64, ch: usize, buf: &mut [f32]| {
        for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
            frame.fill(if (start + i as u64) % PERIOD == 100 { 0.9 } else { 0.0 });
        }
    });
    let t0 = Instant::now();
    let mut ba = FakeBackend::new(48_000, src);
    ba.t0 = Some(t0);
    ba.wake_jitter_us = 300;
    let mut bb = FakeBackend::silence(48_000);
    bb.t0 = Some(t0);
    bb.out_phase_us = 700;
    bb.wake_jitter_us = 300;
    bb.seed = 3;
    let phase = bb.out_phase_us as f64 / 1e6;
    let origin = bb.sink_origin.clone();
    let (a, b, _p, sink_b) = connect_pair(cfg, ba, bb, None);
    std::thread::sleep(Duration::from_secs(seconds));
    let (sa, sb) = (a.stats(), b.stats());
    let js = b.peer_jitter_stats()[0].1;
    let out = sink_b.lock().clone();
    drop(a);
    drop(b);
    let origin = origin.load(Ordering::SeqCst) as f64;
    let mut lat = Vec::new();
    let mut last_k = None;
    for (j, f) in out.as_chunks::<2>().0.iter().enumerate() {
        if f[0] > 0.5 {
            let t_play = phase + (origin + j as f64) / 48_000.0;
            let k = ((t_play * 48_000.0 - 100.0) / PERIOD as f64).floor() as i64;
            // 연결·버퍼링 안정화 뒤(3초 이후)만
            if k >= 0 && last_k != Some(k) && t_play > 3.0 {
                let t_cap = (k as f64 * PERIOD as f64 + 100.0) / 48_000.0;
                lat.push((t_play - t_cap) * 1000.0);
                last_k = Some(k);
            }
        }
    }
    (lat, sa, sb, js)
}

#[test]
#[ignore = "realtime: 결정적 짝 est_latency_budget_table(estLatencyMs ≤ 8), engine::worker::tests::callback_mix_sim_mirrors_realtime_*"]
fn link_end_to_end_latency_128_2_5ms() {
    let _g = RT_LOCK.lock();
    // GOAL 1절 ASIO 기준 설정(128샘플 / 2.5ms 프레임)을 가짜 장치로. PCM16이라 임펄스가 코덱에 뭉개지지 않는다.
    let cfg = EngineConfig { codec: CodecKind::Pcm16, ..config(128, 2.5) };
    let (lat, sa, sb, js) = link_impulse_latency(cfg, 8);
    assert!(lat.len() >= 10, "임펄스 검출 {}", lat.len());
    let mean = lat.iter().sum::<f64>() / lat.len() as f64;
    let (lo, hi) = (lat.iter().cloned().fold(f64::MAX, f64::min), lat.iter().cloned().fold(0.0, f64::max));
    println!(
        "종단 지연 128/2.5ms: 평균 {mean:.2}ms (범위 {lo:.2}..{hi:.2}, n={}), A estLatencyMs {:.2}, B estLatencyMs {:.2}, B bufferMs {:.2} (목표 {:.2}), B playoutMs {:.2}, B concealed {}, xruns A {} B {}",
        lat.len(),
        sa.est_latency_ms,
        sb.est_latency_ms,
        js.buffer_ms,
        js.target_ms,
        sb.peers[0].playout_ms,
        js.concealed,
        sa.xruns,
        sb.xruns
    );
    // 송신 큐 지연(P1-3): 워커 경로(프레임 생성 → PCM16 인코딩 → 암호화 → send_to)가 통계로 나온다
    let sq = sa.send_queue.expect("네트워크가 있으면 sendQueue가 있다");
    println!("A sendQueue: p50 {:.3}ms p99 {:.3}ms max {:.3}ms ({}패킷), A qos {:?}", sq.p50_ms, sq.p99_ms, sq.max_ms, sq.packets, sa.qos);
    assert!(sq.packets > 0 && sq.p50_ms < 2.5, "{sq:?}");
    assert!(sa.qos.as_ref().is_some_and(|q| q.ip_tos), "{:?}", sa.qos);
    // GOAL 1절: ASIO 128샘플 / 2.5ms 프레임에서 estLatencyMs ≤ 8ms(입력 2.67 + 프레임 2.5 + 출력 2.67 = 7.83).
    // 콜백 구동 믹스 전에는 출력 링(≈ 장치 버퍼 + 청크) 때문에 11.8ms였다.
    assert!(sb.est_latency_ms <= 8.0, "estLatencyMs {:.2} > 8", sb.est_latency_ms);
    assert!(sb.device_period_ms > 2.6 && sb.device_period_ms < 2.7, "devicePeriodMs {}", sb.device_period_ms);
    assert_eq!(sb.backend, "fake");
    // 지연이 통계 밖으로 숨지 않는다: 실측 종단 지연(공유 클럭) ≤ estLatencyMs(송신 입력+프레임, 수신 출력)
    // + bufferMs(소스 링 포함 수신 버퍼) + 1ms(루프백 망·스케줄링). 소스 링이 bufferMs 밖에 있으면 이 검사가 깨진다.
    assert!(
        mean <= sb.est_latency_ms as f64 + js.buffer_ms as f64 + 1.0,
        "종단 지연 {mean:.2}ms > est {:.2} + bufferMs {:.2} + 1",
        sb.est_latency_ms,
        js.buffer_ms
    );
    // playoutMs(벽시계 버퍼 + 출력 버퍼)는 종단 지연의 수신측 몫이다: 종단 − playout = 송신측(입력 대기 + 프레임) 이하.
    let send_side = mean - sb.peers[0].playout_ms as f64;
    assert!(send_side > 0.0 && send_side <= 2.67 + 2.5 + 1.0, "송신측 몫 {send_side:.2}ms");
    assert!(js.concealed <= 1, "지터 없는 링크에서 은닉 {}", js.concealed);
}

#[test]
fn device_lost_is_reported() {
    let _g = RT_LOCK.lock();
    // 1) 비치명 스트림 에러만으로는 장치 분실이 아니다(콜백은 계속 돈다)
    let mut backend = FakeBackend::silence(48_000);
    backend.error_after = Some(Duration::from_millis(300));
    let mut e = Engine::start(config(128, 5.0), Arc::new(backend), Arc::new(MixControls::default()), None).unwrap();
    assert!(wait_until(HANG, || e.stats().stream_errors >= 1), "스트림 에러가 보고되지 않음");
    let s = e.stats();
    e.stop();
    assert_eq!(s.stream_errors, 1);
    assert!(!s.device_lost && s.running, "{s:?}");

    // 2) 콜백이 멈추면(장치 분리) deviceLost=true, running=false → 프론트의 기존 엔진 정지 감지가 동작(PR #5 M-2)
    let mut backend = FakeBackend::silence(48_000);
    backend.halt_after = Some(Duration::from_millis(400));
    let mut e = Engine::start(config(128, 5.0), Arc::new(backend), Arc::new(MixControls::default()), None).unwrap();
    assert!(e.stats().running);
    assert!(wait_until(HANG, || e.stats().device_lost), "콜백이 멈췄는데 deviceLost가 서지 않음");
    let s = e.stats();
    e.stop();
    assert!(s.device_lost, "{s:?}");
    assert!(!s.running, "장치를 잃으면 running=false");
    let j = serde_json::to_string(&s).unwrap();
    assert!(j.contains("\"deviceLost\":true") && j.contains("\"running\":false"));
}

#[test]
fn worker_init_failure_fails_start() {
    let _g = RT_LOCK.lock();
    // 장치가 지원 범위 밖 레이트를 보고하면 Worker::new가 실패하고 engine_start가 에러를 돌려준다(PR #5 L-6)
    let backend = FakeBackend::new(4_000, Arc::new(|_, _, buf: &mut [f32]| buf.fill(0.0)));
    let r = Engine::start(config(128, 5.0), Arc::new(backend), Arc::new(MixControls::default()), None);
    let err = r.err().unwrap_or_default();
    assert!(err.contains("레이트"), "{err}");
}

#[test]
#[ignore = "realtime: 결정적 짝 engine::worker::tests::callback_mix_sim_output_stalls_do_not_accumulate_latency"]
fn output_underruns_do_not_accumulate_latency() {
    let _g = RT_LOCK.lock();
    // PR #5 H-1 회귀: 수신측 출력 콜백이 1초마다 8ms 멈췄다 몰아서 따라잡는다(CPU 스파이크) → 매번 출력 언더런.
    // 이전 구현은 소비 카운터 결손이 0.5ms/s로만 회복되어 실제 지연이 30초에 +54ms까지 몰래 늘었다.
    let ba = sine_backend();
    let mut bb = FakeBackend::silence(48_000);
    bb.out_stall = Some((Duration::from_secs(1), Duration::from_millis(8)));
    let (a, b, _p, _sink) = connect_pair(config(128, 5.0), ba, bb, None);
    let mut trace = Vec::new();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(500));
        let s = b.stats();
        let js = b.peer_jitter_stats()[0].1;
        trace.push((t0.elapsed().as_secs_f64(), js.buffer_ms, js.wall_buffer_ms, s.peers[0].playout_ms, js.target_ms));
    }
    let st = b.stats();
    let js = b.peer_jitter_stats()[0].1;
    drop(a);
    drop(b);
    let mean = |from: f64, to: f64, f: fn(&(f64, f32, f32, f32, f32)) -> f32| {
        let v: Vec<f32> = trace.iter().filter(|t| t.0 >= from && t.0 < to).map(f).collect();
        v.iter().sum::<f32>() / v.len().max(1) as f32
    };
    let (wall_early, wall_late) = (mean(4.0, 8.0, |t| t.2), mean(15.0, 20.5, |t| t.2));
    let (clk_late, play_late) = (mean(15.0, 20.5, |t| t.1), mean(15.0, 20.5, |t| t.3));
    println!(
        "출력 언더런 반복: xruns {}, 벽시계 버퍼 4~8s {wall_early:.1}ms → 15~20s {wall_late:.1}ms, 클럭 기준 bufferMs {clk_late:.1}ms, playoutMs {play_late:.1}ms, 목표 {:.1}ms, 순증 보정 {} 샘플",
        st.xruns,
        js.target_ms,
        js.samples_inserted as i64 - js.samples_dropped as i64
    );
    assert!(st.xruns >= 10, "테스트가 언더런을 실제로 일으켜야 함: {}", st.xruns);
    assert!((wall_late - wall_early).abs() < 3.0, "실제 지연 누적: {wall_early:.1} → {wall_late:.1}ms");
    assert!((wall_late - js.target_ms).abs() < 5.0, "실제 지연이 목표 근처: {wall_late:.1} vs {:.1}", js.target_ms);
    // 클럭 기준 깊이(bufferMs)와 벽시계 실측이 어긋나지 않아야 숨은 누적이 없다
    assert!((clk_late - wall_late).abs() < 3.0, "bufferMs {clk_late:.1} vs 실측 {wall_late:.1}");
    assert!(play_late > wall_late, "playoutMs는 출력 경로를 포함");
}

// ============================================================================
// 커맨드 계층(H3), 수명
// ============================================================================

#[test]
fn manager_stays_responsive_while_starting() {
    let _g = RT_LOCK.lock();
    // 장치 열기를 문으로 붙잡는다(벽시계 open_delay 대신): 문이 닫혀 있는 동안은 확실히 "시작 중"이다.
    let gate = OpenGate::closed();
    let mut backend = FakeBackend::silence(48_000);
    backend.open_gate = Some(gate.clone());
    let m = Arc::new(EngineManager::new(Arc::new(backend), test_net()));
    let m2 = m.clone();
    let starter = std::thread::spawn(move || m2.start(config(128, 5.0), "me".into()));
    assert!(gate.wait_entered(1, HANG), "장치 열기에 들어가지 않음");
    assert!(m.is_starting());
    // 시작 중에도 다른 커맨드는 응답한다(락을 쥔 채 블로킹하지 않음). 막히면 문이 닫혀 있어 끝나지 않으므로 상한으로 판정.
    let m3 = m.clone();
    let (s, err) = within(HANG, move || (m3.stats(), m3.with_engine(|_| Ok(())).unwrap_err())).expect("시작 중 stats/with_engine이 막힘");
    assert!(m.is_starting(), "문이 닫혀 있으므로 아직 시작 중");
    assert!(!s.running);
    assert!(err.contains("시작 중"), "{err}");
    gate.release();
    let desc = starter.join().unwrap().unwrap();
    assert!(desc.ssrc != 0 || !desc.key.is_empty());
    assert!(m.stats().running);

    // 시작 중 stop → 그 시작은 취소된다
    gate.close();
    let m3 = m.clone();
    let starter = std::thread::spawn(move || m3.start(config(128, 5.0), "me".into()));
    assert!(gate.wait_entered(2, HANG), "두 번째 시작이 장치 열기에 들어가지 않음");
    m.stop();
    gate.release();
    let r = starter.join().unwrap();
    assert!(r.is_err(), "취소되어야 함");
    assert!(!m.stats().running, "stop 이후 엔진이 남으면 안 됨");
}

#[test]
fn stop_then_start_during_slow_start_is_serialized() {
    let _g = RT_LOCK.lock();
    // PR #5 M-4: 장치 열기가 느린 시작 중에 stop → 곧바로 start. 장치는 배타라 동시에 열면 실패한다.
    // 새 시작은 이전 시작이 끝나 장치를 놓을 때까지 기다려야 하고, 결국 엔진이 하나 살아 있어야 한다.
    // 느린 장치 열기는 문(OpenGate)으로 만든다: 순서를 벽시계가 아니라 문과 시작 세대로 고정한다.
    let gate = OpenGate::closed();
    let mut backend = FakeBackend::silence(48_000);
    backend.open_gate = Some(gate.clone());
    backend.exclusive = Some(Arc::new(AtomicBool::new(false)));
    let m = Arc::new(EngineManager::new(Arc::new(backend), test_net()));
    let m1 = m.clone();
    let first = std::thread::spawn(move || m1.start(config(128, 5.0), "me".into()));
    assert!(gate.wait_entered(1, HANG), "첫 시작이 장치 열기에 들어가지 않음");
    m.stop();
    let g0 = m.start_generation();
    let m2 = m.clone();
    let second = std::thread::spawn(move || m2.start(config(128, 5.0), "me".into()));
    // 두 번째 start가 들어왔다(세대 증가). 첫 시작은 아직 장치를 쥔 채 문에 붙잡혀 있다 — 직렬화가 깨졌다면
    // 두 번째는 배타 장치 열기에 실패한다.
    assert!(wait_until(HANG, || m.start_generation() > g0), "두 번째 start가 들어오지 않음");
    gate.release();
    let r1 = first.join().unwrap();
    let r2 = second.join().unwrap();
    assert!(r1.is_err(), "첫 시작은 취소");
    assert!(r2.is_ok(), "두 번째 시작은 장치 사용 중 없이 성공해야 함: {r2:?}");
    assert!(m.stats().running);
    // 연속 start 두 번(설정 변경 연타)도 마지막 하나만 남는다
    gate.close();
    let n = gate.entered();
    let m3 = m.clone();
    let a = std::thread::spawn(move || m3.start(config(256, 5.0), "me".into()));
    assert!(gate.wait_entered(n + 1, HANG), "a가 장치 열기에 들어가지 않음");
    let g1 = m.start_generation();
    let m4 = m.clone();
    let b = std::thread::spawn(move || m4.start(config(128, 2.5), "me".into()));
    assert!(wait_until(HANG, || m.start_generation() > g1), "b가 들어오지 않음");
    gate.release();
    let (ra, rb) = (a.join().unwrap(), b.join().unwrap());
    assert!(ra.is_err() && rb.is_ok(), "{ra:?} / {rb:?}");
    assert!(m.stats().running);
    m.stop();
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
        let backend = FakeBackend::silence(48_000);
        let sink = backend.sink.clone();
        let mut e = Engine::start(
            config(128, 5.0),
            Arc::new(backend),
            controls.clone(),
            if i % 2 == 0 { Some((format!("p{i}"), test_net())) } else { None },
        )
        .unwrap();
        e.subscribe_mix(Box::new(|_| {})).unwrap();
        // 콜백이 실제로 돈 뒤에 정지한다(고정 sleep 대신 출력 진행량)
        assert!(wait_until(HANG, || sink.lock().len() >= 128 * 2 * 4), "콜백이 돌지 않음");
        // 정지(join)가 끝나야 한다. 상한은 행 감지용.
        within(HANG, move || {
            e.stop();
            e.stop(); // 멱등
        })
        .expect("정지가 끝나지 않음(스레드 join 행)");
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
    // 모니터 출력에 입력 사인이 원래 크기로 나온다: 50ms 창 RMS가 진폭/√2 근처인 구간이 나올 때까지
    // (고정 sleep·마지막 0.5초 RMS 대신 — 시작 지연·xrun 구멍과 무관).
    let want = SINE_AMP / 2f32.sqrt();
    let mut rms = 0.0;
    let ok = wait_until(HANG, || {
        rms = tail_rms_after(&sink, 0, 2400).map_or(0.0, |(l, _)| l);
        (rms - want).abs() < 0.05
    });
    let s = e.stats();
    e.stop();
    println!("모니터 모드: rms {rms:.3}, estLatencyMs {:.1}, xruns {}", s.est_latency_ms, s.xruns);
    assert!(ok, "모니터 출력 rms {rms}");
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
    // 고정 sleep 대신 진행량으로 기다린다(P3-0a): 뮤트 상태로 출력 0.5초분(24000프레임)과 탭 샘플이 쌓이고,
    // 입력 미터가 입력 사인 RMS를 보일 때까지. 처음부터 뮤트였으므로 그동안의 출력·탭 전체가 무음이어야 한다.
    let want = SINE_AMP / 2f32.sqrt();
    assert!(wait_until(HANG, || sink.lock().len() >= 48_000 && tap_energy.lock().0 >= 24_000), "출력/탭이 쌓이지 않음");
    let mut level = 0.0;
    let level_ok = wait_until(HANG, || {
        level = e.stats().input_level;
        (level - want).abs() < 0.03
    });
    let muted_len = sink.lock().len();
    let muted_rms = {
        let out = sink.lock();
        let all = &out[..muted_len];
        (all.iter().map(|x| x * x).sum::<f32>() / all.len().max(1) as f32).sqrt()
    };
    let (n, energy) = *tap_energy.lock();
    controls.input_muted.store(false, Ordering::Relaxed);
    let mut unmuted_rms = 0.0;
    let unmuted = wait_until(HANG, || {
        unmuted_rms = tail_rms_after(&sink, muted_len, 2400).map_or(0.0, |(l, _)| l);
        unmuted_rms > 0.3
    });
    e.stop();
    println!("입력 뮤트: 출력 rms {muted_rms:.4}, 탭 rms {:.4}, inputLevel {level:.3} → 해제 후 rms {unmuted_rms:.3}", (energy / n.max(1) as f64).sqrt());
    assert!(muted_rms < 1e-4, "뮤트 중 모니터 출력은 무음");
    assert!(n > 0 && energy < 1e-6, "뮤트 중 믹스 탭도 무음");
    assert!(level_ok, "뮤트 중에도 입력 미터는 동작: inputLevel {level}");
    assert!(unmuted, "뮤트 해제 후 복귀: rms {unmuted_rms}");
}

#[test]
fn peer_pan_and_levels() {
    let _g = RT_LOCK.lock();
    let (a, b, _p, sink_b) = connect_pair(config(128, 5.0), sine_backend(), FakeBackend::silence(48_000), None);
    b.set_peer_gain("alice", 1.0, false, Some(1.0)).unwrap();
    // 고정 sleep 대신(P3-0a): 팬이 적용된 소리(오른쪽 √2 배, 50ms 창)가 나오고 피어 레벨이 설 때까지 기다린다.
    let from = sink_b.lock().len();
    let (mut r, mut peer_level) = (0f32, 0f32);
    let panned = wait_until(HANG, || {
        r = tail_rms_after(&sink_b, from, 2400).map_or(0.0, |(_, r)| r);
        peer_level = b.stats().peers[0].level;
        r > 0.4 && peer_level > 0.25
    });
    assert!(panned, "오른쪽은 √2 배: R rms {r}, 피어 레벨 {peer_level}");
    // 그 뒤 출력 0.5초분(진행량) 동안 왼쪽은 한 번도 새지 않는다(은닉 프레임도 같은 팬).
    let settled = sink_b.lock().len();
    assert!(wait_until(HANG, || sink_b.lock().len() >= settled + 48_000), "출력이 진행하지 않음");
    let st = b.stats();
    let l = {
        let out = sink_b.lock();
        let seg = &out[settled..];
        let n = (seg.len() / 2).max(1) as f32;
        (seg.as_chunks::<2>().0.iter().map(|f| f[0] * f[0]).sum::<f32>() / n).sqrt()
    };
    println!("pan=+1: L rms {l:.4}, R rms {r:.4}, peer level {peer_level:.3}, outputLevel {:.3}", st.output_level);
    assert!(l < 0.01, "오른쪽 끝 팬이면 왼쪽은 무음: {l}");
    assert!((0.0..=1.0).contains(&peer_level), "피어 레벨은 0..1 RMS: {peer_level}");
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
    // 고정 sleep 대신 진행량(P3-0a): 위조 패킷 뒤에 도착한 A의 패킷으로 프레임 200개(1초분)를 더 만들 때까지.
    // 같은 수신 소켓에서 순서대로 처리하므로 그때는 위조 패킷도 처리가 끝났다. 사칭 Bye가 먹혔다면 A의 프레임이 멈춰 실패한다.
    let real0 = real_frames(&b, "alice");
    assert!(wait_until(HANG, || real_frames(&b, "alice") >= real0 + 200), "위조 뒤 A의 패킷이 계속 처리되지 않음");
    let st = b.stats();
    let alice = st.peers.iter().find(|p| p.peer_id == "alice").unwrap();
    assert_eq!(alice.state, PeerStateKind::Connected, "사칭 Bye로 끊기면 안 됨");
    // B는 여전히 A의 소리를 듣는다(50ms 창이 소리로 찬 구간이 나온다)
    let from = sink_b.lock().len();
    let mut rms = 0.0;
    assert!(
        wait_until(HANG, || {
            rms = tail_rms_after(&sink_b, from, 2400).map_or(0.0, |(l, _)| l);
            rms > 0.3
        }),
        "A 오디오가 계속 들려야 함: {rms}"
    );
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

// ============================================================================
// 콜백 구동 믹스(P1-0) 회귀
// ============================================================================

#[test]
#[ignore = "realtime: 결정적 짝 engine::worker::tests::callback_mix_sim_mirrors_realtime_phase_sweep"]
fn callback_mix_phase_sweep_no_premature_concealment() {
    let _g = RT_LOCK.lock();
    // 송신 입력과 수신 출력의 위상을 바꿔 가며 지터 없는 링크에서 은닉·마감 채움·보정이 0인지 본다.
    // 회귀 대상 두 가지(engine-notes "콜백 구동 믹스"):
    // 1) 청크(64) 단위로만 꺼내 FIFO에 남은 패킷 꼬리를 못 써서 마감에 은닉
    // 2) 워커가 콜백 직전 타이밍과 콜백 직후 링 점유량을 짝지어 한 주기 이르게 은닉
    // 수정 전에는 위상에 따라 3초에 1~13회 은닉 + 수백~천여 회 보정이 났다.
    for (buf, frame_ms) in [(480u32, 2.5f32), (441, 5.0), (128, 2.5)] {
        for phase_us in [0u64, 3000, 7500] {
            let mut ba = sine_backend();
            ba.wake_jitter_us = 1000;
            let mut bb = FakeBackend::silence(48_000);
            bb.wake_jitter_us = 1000;
            bb.seed = 9;
            bb.out_phase_us = phase_us;
            let (a, b, _p, sink_b) = connect_pair(config(buf, frame_ms), ba, bb, None);
            std::thread::sleep(Duration::from_millis(1500));
            let j0 = b.peer_jitter_stats()[0].1;
            let f0 = b.feed_stats();
            let start = sink_b.lock().len() / 2;
            std::thread::sleep(Duration::from_millis(2500));
            let js = b.peer_jitter_stats()[0].1;
            let f1 = b.feed_stats();
            let xruns = b.stats().xruns;
            let snap = sink_b.lock().clone();
            drop(a);
            drop(b);
            let c = analyze(&snap, start);
            let (conc, fills, corr) = (js.concealed - j0.concealed, f1.0 - f0.0, js.corrections() - j0.corrections());
            println!(
                "CB-MIX buf {buf} / {frame_ms}ms, 출력 위상 {phase_us}µs: 은닉 {conc}, 마감 채움 {fills}, 건너뜀 {}, 보정 {corr}, buffer {:.2}/{:.2}ms, 불연속 {} 무음 {}, xruns {xruns}",
                f1.1 - f0.1,
                js.buffer_ms,
                js.target_ms,
                c.discontinuities,
                c.silent_runs
            );
            // 실시간 테스트는 스케줄러 지연 1회까지 허용(link_matrix와 같은 기준)
            assert!(conc <= 1, "buf {buf} / {frame_ms}ms 위상 {phase_us}: 은닉 {conc}");
            assert!(fills <= 1, "buf {buf} / {frame_ms}ms 위상 {phase_us}: 마감 채움 {fills}");
            assert_gaps_explained_by_xruns(&c, xruns, &format!("CB-MIX buf {buf} / {frame_ms}ms 위상 {phase_us}"));
        }
    }
}

#[test]
#[ignore = "realtime: 결정적 짝 engine::worker::tests::{callback_mix_sim_mirrors_realtime_early_callbacks, callback_mix_sim_margin_after_underrun_no_steady_concealment, callback_mix_sim_margin_sweep_no_concealment_after_underrun}"]
fn callback_mix_early_callbacks_real_time() {
    let _g = RT_LOCK.lock();
    // 실장비 WASAPI 독점 프로브에서 출력 콜백은 격자보다 최대 ≈1.7ms 이르게 왔다(PR #11 M3: 간격 p1 2.3ms,
    // 최소 1.25~1.5ms, 공칭 3ms). 가짜 장치 출력에 이른 콜백(0..1.7ms) + 늦은 기상(0..0.5ms)을 주고,
    // 피어 경로(마감 직전 은닉)가 0 채움·조기 은닉 없이 도는지 본다.
    for (out_buf, phase_us) in [(144usize, 0u64), (144, 1800), (128, 900)] {
        let mut ba = sine_backend();
        ba.wake_jitter_us = 500;
        ba.in_buffer = Some(96);
        let mut bb = FakeBackend::silence(48_000);
        bb.wake_jitter_us = 500;
        bb.out_early_us = 1700;
        bb.out_buffer = Some(out_buf);
        bb.seed = 21;
        bb.out_phase_us = phase_us;
        let (a, b, _p, sink_b) = connect_pair(config(128, 2.5), ba, bb, None);
        std::thread::sleep(Duration::from_millis(1500));
        let j0 = b.peer_jitter_stats()[0].1;
        let f0 = b.feed_stats();
        let start = sink_b.lock().len() / 2;
        std::thread::sleep(Duration::from_millis(3000));
        let js = b.peer_jitter_stats()[0].1;
        let f1 = b.feed_stats();
        let st = b.stats();
        let snap = sink_b.lock().clone();
        drop(a);
        drop(b);
        let c = analyze(&snap, start);
        let (conc, fills, skipped) = (js.concealed - j0.concealed, f1.0 - f0.0, f1.1 - f0.1);
        println!(
            "EARLY-CB out {out_buf} 위상 {phase_us}µs: 은닉 {conc}, 마감 채움 {fills}, 건너뜀 {skipped}, buffer {:.2}/{:.2}ms, est {:.2}ms, 불연속 {} 무음 {}, xruns {}",
            js.buffer_ms,
            js.target_ms,
            st.est_latency_ms,
            c.discontinuities,
            c.silent_runs,
            st.xruns
        );
        // 실시간 테스트는 스케줄러 지연 1회까지 허용(link_matrix와 같은 기준)
        assert!(conc <= 1, "out {out_buf} 위상 {phase_us}: 은닉 {conc}");
        assert!(fills <= 1, "out {out_buf} 위상 {phase_us}: 마감 채움 {fills}");
        assert_gaps_explained_by_xruns(&c, st.xruns, &format!("EARLY-CB out {out_buf} 위상 {phase_us}"));
    }
}

#[test]
fn est_latency_budget_table() {
    let _g = RT_LOCK.lock();
    // GOAL 1절 estLatencyMs = 입력 장치 버퍼 + 프레임 + 출력 링 + 출력 장치 버퍼. 콜백 구동 믹스에는 출력 링이 없다.
    // 장치 주기는 가짜 장치로 흉내 낸다: ASIO 128, WASAPI 독점 최소 주기(이 PC 실측: 입력 96 = 2.0ms, 출력 144 = 3.0ms;
    // 일반적인 3ms = 144), IAudioClient3 저지연 공유(최소 주기 128 = 2.67ms 가정), 독점 기본 주기 10ms(참고).
    struct Row {
        label: &'static str,
        in_buf: usize,
        out_buf: usize,
        frame_ms: f32,
        goal_ms: Option<f32>,
    }
    let rows = [
        Row { label: "ASIO 128 / 2.5ms", in_buf: 128, out_buf: 128, frame_ms: 2.5, goal_ms: Some(8.0) },
        Row { label: "ASIO 128 / 5ms", in_buf: 128, out_buf: 128, frame_ms: 5.0, goal_ms: None },
        Row { label: "WASAPI 독점 최소 3ms(144) / 2.5ms", in_buf: 144, out_buf: 144, frame_ms: 2.5, goal_ms: Some(15.0) },
        Row { label: "WASAPI 독점 최소 3ms(144) / 5ms", in_buf: 144, out_buf: 144, frame_ms: 5.0, goal_ms: Some(15.0) },
        Row { label: "WASAPI 독점 이 PC(in 96 / out 144) / 2.5ms", in_buf: 96, out_buf: 144, frame_ms: 2.5, goal_ms: Some(15.0) },
        Row { label: "WASAPI 독점 이 PC(in 96 / out 144) / 5ms", in_buf: 96, out_buf: 144, frame_ms: 5.0, goal_ms: Some(15.0) },
        Row { label: "IAudioClient3 공유 2.67ms(128) / 5ms", in_buf: 128, out_buf: 128, frame_ms: 5.0, goal_ms: Some(30.0) },
        Row { label: "(참고) 독점 기본 주기 10ms(480) / 5ms", in_buf: 480, out_buf: 480, frame_ms: 5.0, goal_ms: None },
    ];
    println!("| 설정 | 입력 | 프레임 | 출력 링 | 출력 | estLatencyMs | 목표 |");
    println!("|---|---|---|---|---|---|---|");
    for r in rows {
        let mut backend = FakeBackend::silence(48_000);
        backend.in_buffer = Some(r.in_buf);
        backend.out_buffer = Some(r.out_buf);
        let mut e = Engine::start(config(r.in_buf as u32, r.frame_ms), Arc::new(backend), Arc::new(MixControls::default()), None).unwrap();
        let (i, o) = (r.in_buf as f32 / 48.0, r.out_buf as f32 / 48.0);
        let calc = i + r.frame_ms + o;
        // 실측 콜백 크기가 잡힐 때까지(고정 sleep 대신). 끝내 안 맞으면 아래 단언이 값을 보여 준다.
        wait_until(HANG, || (e.stats().est_latency_ms - calc).abs() < 0.05);
        let s = e.stats();
        e.stop();
        println!(
            "| {} | {i:.2} | {:.2} | 0 | {o:.2} | **{:.2}** (계산 {calc:.2}) | {} |",
            r.label,
            r.frame_ms,
            s.est_latency_ms,
            r.goal_ms.map(|g| format!("≤ {g}")).unwrap_or_else(|| "—".into())
        );
        assert!((s.est_latency_ms - calc).abs() < 0.05, "{}: 실측 콜백 크기 기반 est {:.2} ≠ 계산 {calc:.2}", r.label, s.est_latency_ms);
        assert!((s.device_period_ms - o).abs() < 0.05, "{}: devicePeriodMs {}", r.label, s.device_period_ms);
        if let Some(g) = r.goal_ms {
            assert!(s.est_latency_ms <= g, "{}: estLatencyMs {:.2} > {g}", r.label, s.est_latency_ms);
        }
    }
}

// ============================================================================
// 스레드 우선순위(P1-2): MMCSS 등록 노출, 타이머 짝, 적용 전/후 측정
// ============================================================================

#[test]
fn thread_priority_reported_and_timer_released_on_stop() {
    use band_lib::engine::prio::ThreadPriority;
    let _g = RT_LOCK.lock();
    for _ in 0..3 {
        let (mut a, b, _p, _sink) = connect_pair(config(480, 5.0), sine_backend(), FakeBackend::silence(48_000), None);
        // 워커·수신 스레드는 시작 직후 우선순위를 적는다
        let t0 = Instant::now();
        let mut tp = a.stats().thread_priority;
        while (tp.worker == ThreadPriority::Unmanaged || tp.net_recv == ThreadPriority::Unmanaged) && t0.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
            tp = a.stats().thread_priority;
        }
        println!("threadPriority: {tp:?}");
        if cfg!(windows) {
            for p in [tp.worker, tp.net_recv] {
                assert!(matches!(p, ThreadPriority::Mmcss | ThreadPriority::Elevated | ThreadPriority::Failed), "{tp:?}");
            }
            assert!(tp.timer_1ms, "Windows 엔진 실행 중에는 1ms 타이머를 쥔다");
        } else {
            assert_eq!(tp.worker, ThreadPriority::Unsupported);
            assert!(!tp.timer_1ms);
        }
        // 가짜 장치는 우리가 관리하지 않는 장치 스레드로 보고된다
        assert_eq!(tp.audio, ThreadPriority::Unmanaged);
        let json = serde_json::to_value(a.stats()).unwrap();
        assert!(json["threadPriority"]["timer1ms"].is_boolean());
        // stop을 두 번 불러도 안전(타이머 짝은 prio 단위 테스트가 검증)
        a.stop();
        a.stop();
        drop(b);
    }
}

/// 적용 전/후 비교: 엔진 스레드 승격(MMCSS, 1ms 타이머)을 끔/MMCSS만/타이머만/둘 다로 바꿔 같은 링크를 돌린다.
/// 지표: 워커와 같은 방식으로 승격된 프로브 스레드의 FINE_POLL(150µs) 수면 초과량, B의 은닉·마감 채움·건너뜀,
/// 지터버퍼 지연/지터. 부하 단계는 논리 코어 수만큼 바쁜 스레드(보통 우선순위)를 돌린다.
/// 실행: `cargo test --release --test loopback rt_priority_before_after -- --ignored --nocapture`
#[test]
#[ignore = "realtime: 측정용(임계 없음). 결정적 짝 engine::prio::tests"]
fn rt_priority_before_after() {
    use band_lib::engine::prio;
    let _g = RT_LOCK.lock();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    println!("| 부하 | 승격 | 프로브 수면 초과 p50/p99/max (µs) | 은닉/s | 마감 채움 | 건너뜀(프레임) | bufferMs 평균 | jitterMs | xruns B |");
    println!("|---|---|---|---|---|---|---|---|---|");
    for load in [false, true] {
        for _round in 0..2 {
            for (mmcss, timer) in [(false, false), (true, false), (false, true), (true, true)] {
                prio::set_engine_promotion_parts(mmcss, timer);
                let stop_load = Arc::new(AtomicBool::new(false));
                let hogs: Vec<_> = (0..if load { cores } else { 0 })
                    .map(|_| {
                        let s = stop_load.clone();
                        std::thread::spawn(move || {
                            let mut x = 0u64;
                            while !s.load(Ordering::Relaxed) {
                                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                            }
                        })
                    })
                    .collect();
                let mut ba = sine_backend();
                ba.wake_jitter_us = 0;
                let bb = FakeBackend::silence(48_000);
                let (a, b, _p, _sink) = connect_pair(config(128, 2.5), ba, bb, None);
                // 프로브: 워커와 같은 승격 경로 + 같은 수면 길이
                let stop_probe = Arc::new(AtomicBool::new(false));
                let sp = stop_probe.clone();
                let probe = std::thread::spawn(move || {
                    let slot = prio::PrioSlot::default();
                    let _g = prio::promote_engine_thread(&slot);
                    let mut over = Vec::with_capacity(100_000);
                    while !sp.load(Ordering::Relaxed) {
                        let t = Instant::now();
                        std::thread::sleep(Duration::from_micros(150));
                        over.push(t.elapsed().as_micros().saturating_sub(150) as u64);
                    }
                    over
                });
                std::thread::sleep(Duration::from_secs(2));
                let s0 = b.peer_jitter_stats()[0].1;
                let f0 = b.feed_stats();
                let x0 = b.stats().xruns;
                let mut buf_sum = 0.0;
                let n = 40;
                for _ in 0..n {
                    std::thread::sleep(Duration::from_millis(250));
                    buf_sum += b.peer_jitter_stats()[0].1.buffer_ms as f64;
                }
                let s1 = b.peer_jitter_stats()[0].1;
                let f1 = b.feed_stats();
                let x1 = b.stats().xruns;
                stop_probe.store(true, Ordering::Relaxed);
                let mut over = probe.join().unwrap();
                drop(a);
                drop(b);
                stop_load.store(true, Ordering::Relaxed);
                for h in hogs {
                    let _ = h.join();
                }
                over.sort_unstable();
                let pct = |q: f64| over[((over.len() - 1) as f64 * q) as usize];
                println!(
                    "| {} | {} | {}/{}/{} | {:.2} | {} | {} | {:.2} | {:.2} | {} |",
                    if load { format!("{cores}코어 점유") } else { "없음".into() },
                    match (mmcss, timer) {
                        (false, false) => "끔",
                        (true, false) => "MMCSS만",
                        (false, true) => "타이머만",
                        (true, true) => "둘 다",
                    },
                    pct(0.5),
                    pct(0.99),
                    over[over.len() - 1],
                    (s1.concealed - s0.concealed) as f64 / (n as f64 * 0.25),
                    f1.0 - f0.0,
                    f1.1 - f0.1,
                    buf_sum / n as f64,
                    s1.jitter_ms,
                    x1 - x0,
                );
            }
        }
    }
    prio::set_engine_promotion(true);
}
