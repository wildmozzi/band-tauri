//! 6인 풀메시 CPU 측정(P1-10, GOAL 2절 "6인 풀메시 60분, CPU ≤ 15%(4코어 기준)").
//!
//! 한 프로세스에 가짜 장치(사인 입력) 엔진 6개를 띄우고 127.0.0.1로 풀메시 연결한다(각 엔진 = 자신 + 원격 5명).
//! 엔진마다 `EngineStats.cpu`(워커 CPU%, RT 콜백 사용률·p99)를 읽고, 프로세스 전체 CPU(`QueryProcessCycleTime`
//! 증분 / TSC 증분) / 6으로
//! 연주자 1명 몫(워커 + 네트워크 수신·제어 + 가짜 장치 스레드)을 낸다. 수치 판정 없이 표만 출력한다(측정 도구).
//!
//! 실행(수치는 release 기준이 의미 있다):
//! `cargo test --release --features test-support --test cpu_profile -- --ignored --nocapture`

#[cfg(not(any(debug_assertions, feature = "test-support")))]
compile_error!("릴리스 측정은 가짜 장치가 필요하다: cargo test --release --features test-support --test cpu_profile");

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use band_lib::engine::codec::Encoder;
use band_lib::engine::config::{CodecKind, EngineConfig};
use band_lib::engine::cpuprof::CpuStats;
use band_lib::sys::cpu::{cycle_pct, process_cycles, tsc_now};
use band_lib::engine::device::fake::FakeBackend;
use band_lib::engine::mixer::MixControls;
use band_lib::engine::relay_codec::CodecReason;
use band_lib::net::types::PeerStateKind;
use band_lib::engine::Engine;
use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
use band_lib::net::NetOptions;

const PEERS: usize = 6;
const MEASURE: Duration = Duration::from_secs(12);

fn test_net() -> NetOptions {
    NetOptions { port: 0, stun_servers: vec![], gather_lan: false, extra_candidates: vec![], allow_loopback_candidates: true, cancel: None }
}

fn sine_backend(hz: f64) -> FakeBackend {
    let mut b = FakeBackend::new(
        48_000,
        Arc::new(move |start: u64, ch: usize, buf: &mut [f32]| {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let t = (start + i as u64) as f64 / 48_000.0;
                frame.fill((t * hz * 2.0 * std::f64::consts::PI).sin() as f32 * 0.3);
            }
        }),
    );
    b.record_output = false;
    b
}

fn addr(e: &Engine) -> SocketAddr {
    format!("127.0.0.1:{}", e.local_port().unwrap()).parse().unwrap()
}

fn with_candidate(d: &LocalDescriptor, a: SocketAddr) -> LocalDescriptor {
    LocalDescriptor { candidates: vec![Candidate { kind: CandidateKind::Lan, addr: a.to_string() }], ..d.clone() }
}

struct Row {
    label: String,
    me: CpuStats,
    worker_avg: f32,
    worker_max: f32,
    load_avg: f32,
    p99_max: f32,
    per_node_pct: f32,
    conceal_max: f32,
    xruns: u64,
}

fn run(codec: CodecKind, frame_ms: f32, force_auto: bool) -> Row {
    let cfg = EngineConfig { buffer_size: 128, frame_ms, codec, channels: 1, input_channels: vec![0], ..Default::default() };
    let engines: Vec<Engine> = (0..PEERS)
        .map(|i| {
            Engine::start(cfg.clone(), Arc::new(sine_backend(220.0 * (i + 1) as f64)), Arc::new(MixControls::default()), Some((format!("p{i}"), test_net())))
                .unwrap()
        })
        .collect();
    for (i, e) in engines.iter().enumerate() {
        for (j, o) in engines.iter().enumerate() {
            if i != j {
                e.add_peer(format!("p{j}"), &with_candidate(o.descriptor().unwrap(), addr(o))).unwrap();
            }
        }
    }
    let t0 = Instant::now();
    while !engines.iter().all(|e| {
        let s = e.stats();
        s.peers.len() == PEERS - 1 && s.peers.iter().all(|p| p.state == PeerStateKind::Connected)
    }) {
        assert!(t0.elapsed() < Duration::from_secs(30), "풀메시 연결 실패");
        std::thread::sleep(Duration::from_millis(50));
    }
    if force_auto {
        // P1-4f M1: PCM16 설정에서 손실 피드백이 모든 피어에게 켜진 최악 조건(엔진마다 프레임당 Opus 인코딩 5회)
        for e in &engines {
            e.force_auto_opus(true).unwrap();
        }
    }
    // 예열(지터 버퍼 수렴) 후 창을 한 번 회전해 기준을 맞춘다
    std::thread::sleep(Duration::from_secs(3));
    if force_auto {
        for (i, e) in engines.iter().enumerate() {
            for p in e.stats().peers {
                assert_eq!(p.tx_codec_reason, Some(CodecReason::LossFeedback), "엔진 {i} → {}: 강제 전환이 안 됨", p.peer_id);
                assert_eq!(p.tx_codec, Some(CodecKind::Opus));
            }
        }
    }
    // 창 회전과 xrun 기준(연결·예열 구간 제외)
    let x0: u64 = engines.iter().map(|e| e.stats().xruns).sum();
    let (p0, k0) = (process_cycles(), tsc_now());
    std::thread::sleep(MEASURE);
    let stats: Vec<_> = engines.iter().map(|e| e.stats()).collect();
    let (p1, k1) = (process_cycles(), tsc_now());
    let cpus: Vec<CpuStats> = stats.iter().map(|s| s.cpu).collect();
    for c in &cpus {
        assert!(c.window_ms >= MEASURE.as_millis() as f32 * 0.9, "창 회전: {c:?}");
        assert!(c.callbacks > 0, "{c:?}");
    }
    let workers: Vec<f32> = cpus.iter().map(|c| c.worker_pct.unwrap_or(f32::NAN)).collect();
    let row = Row {
        label: if force_auto { format!("{codec:?} {frame_ms}ms + 5피어 Opus 개별 인코딩(손실 피드백 강제)") } else { format!("{codec:?} {frame_ms}ms") },
        me: cpus[0],
        worker_avg: workers.iter().sum::<f32>() / PEERS as f32,
        worker_max: workers.iter().cloned().fold(0.0, f32::max),
        load_avg: cpus.iter().map(|c| c.callback_load_pct).sum::<f32>() / PEERS as f32,
        p99_max: cpus.iter().map(|c| c.callback_p99_us).fold(0.0, f32::max),
        per_node_pct: cycle_pct(p1, p0, k1, k0).unwrap_or(f32::NAN) / PEERS as f32,
        conceal_max: stats.iter().flat_map(|s| s.peers.iter().map(|p| p.conceal_pct)).fold(0.0, f32::max),
        // 측정 창 안의 xrun만(연결·예열 구간 제외, 리뷰 L1)
        xruns: stats.iter().map(|s| s.xruns).sum::<u64>().saturating_sub(x0),
    };
    for mut e in engines {
        e.stop();
    }
    row
}

#[test]
#[ignore = "측정 도구: cargo test --release --features test-support --test cpu_profile -- --ignored --nocapture"]
fn six_peer_full_mesh_cpu() {
    // RT 콜백 타이머 비용(QPC)
    let n = 1_000_000;
    let t = Instant::now();
    let mut acc = Duration::ZERO;
    for _ in 0..n {
        acc += std::hint::black_box(Instant::now()).elapsed();
    }
    let per_call_ns = t.elapsed().as_nanos() as f64 / (2 * n) as f64;
    std::hint::black_box(acc);
    println!("Instant::now() 1회 ≈ {per_call_ns:.1}ns (논리 코어 {})", std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0));

    // P1-4f M1: Opus 인코딩 1회 비용(단일 스레드, 음악 같은 화음+잡음 신호). 피어별 인코더 5개 × 프레임률이 1코어의 몇 %인지.
    for (ch, frame) in [(1usize, 120usize), (2, 120), (1, 240), (2, 240)] {
        let bitrate = if ch == 2 { 160_000 } else { 96_000 };
        let mut enc = Encoder::new(CodecKind::Opus, ch, bitrate).unwrap();
        let _ = enc.set_loss_perc(5);
        let mut out = vec![0u8; 1500];
        let frames = 8000usize;
        let mut seed = 0x1234_5678u32;
        let sig: Vec<f32> = (0..frames * frame * ch)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let t = (i / ch) as f32 / 48_000.0;
                let tone: f32 = [196.0f32, 247.0, 294.0, 392.0].iter().map(|f| (t * f * std::f32::consts::TAU).sin()).sum();
                tone * 0.08 + (seed as f32 / u32::MAX as f32 - 0.5) * 0.05
            })
            .collect();
        let t = Instant::now();
        for f in sig.chunks_exact(frame * ch) {
            std::hint::black_box(enc.encode(f, &mut out).unwrap());
        }
        let us = t.elapsed().as_secs_f64() * 1e6 / frames as f64;
        let fps = 48_000.0 / frame as f64;
        println!(
            "Opus 인코딩 1회({}, {:.1}ms): {us:.1}µs → 피어 5명 개별 = 1코어의 {:.2}% (4코어 기준 {:.2}%)",
            if ch == 2 { "stereo 160k" } else { "mono 96k" },
            frame as f64 / 48.0,
            us * fps * 5.0 / 1e4,
            us * fps * 5.0 / 4e4
        );
    }

    let rows: Vec<Row> = [
        (CodecKind::Opus, 2.5, false),
        (CodecKind::Pcm16, 2.5, false),
        (CodecKind::Pcm16, 2.5, true),
        (CodecKind::Opus, 5.0, false),
        (CodecKind::Pcm16, 5.0, false),
        (CodecKind::Pcm16, 5.0, true),
    ]
        .into_iter()
        .map(|(c, f, force)| run(c, f, force))
        .collect();
    println!();
    println!("| 구성(128샘플 버퍼, 모노) | 워커 CPU 내 엔진 / 평균 / 최대 (%, 1코어) | 콜백 사용률 평균 (%) | 콜백 p50 / p99 / 최대 (µs, 내 엔진) | p99/주기 (%) | 6피어 최악 p99 (µs) | 연주자 1명 몫 프로세스 CPU (%, 1코어 → 4코어 기준) | 최대 은닉 (%) | xrun(측정 창, 6엔진 합) |");
    println!("|---|---|---|---|---|---|---|---|---|");
    for r in &rows {
        println!(
            "| {} | {:.2} / {:.2} / {:.2} | {:.3} | {:.0} / {:.0} / {:.0} | {:.2} | {:.0} | {:.2} → **{:.2}** | {:.2} | {} |",
            r.label,
            r.me.worker_pct.unwrap_or(f32::NAN),
            r.worker_avg,
            r.worker_max,
            r.load_avg,
            r.me.callback_p50_us,
            r.me.callback_p99_us,
            r.me.callback_max_us,
            r.me.callback_p99_period_pct,
            r.p99_max,
            r.per_node_pct,
            r.per_node_pct / 4.0,
            r.conceal_max,
            r.xruns
        );
    }
}

/// 엔진 ③-a2부터 지터 재시작(에코 측정 시작·끝)은 워커가 적용하고, `JitterBuffer::restart`는 버퍼를 새로 만든다
/// (≈1.19MB 할당·해제). 그 비용이 워커 반복을 얼마나 늘리고 다른 일(송신·다른 피어 공급)을 늦추는지 잰다.
/// (1) `restart` 1회 시간 분포(= 재시작을 적용하는 반복에 더해지는 시간). (2) 6엔진 풀메시에서 엔진 0이 피어 5명 모두를
/// 50ms마다 재시작(실제는 측정당 2회 — 수백 배 거센 조건)할 때와 조용할 때, 엔진 0의 건너뛴 프레임·xrun과
/// **다른 엔진들이 받는 엔진 0 소리의 은닉**(엔진 0 워커가 늦어 송신이 밀리면 오른다)을 비교한다. 판정 없이 표만 낸다.
/// `cargo test --release --features test-support --test cpu_profile restart_cost -- --ignored --nocapture`
#[test]
#[ignore = "측정: release 권장(약 40초)"]
fn restart_cost_on_worker() {
    use band_lib::engine::jitter::{JitterBuffer, JitterSettings};
    // (1) restart 1회 시간
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    let mut us: Vec<f64> = (0..300)
        .map(|_| {
            let t = Instant::now();
            jb.restart();
            t.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| us[((us.len() as f64 * p) as usize).min(us.len() - 1)];
    println!("JitterBuffer::restart 1회(µs, 300회): p50 {:.1} / p99 {:.1} / 최대 {:.1}", q(0.5), q(0.99), us[us.len() - 1]);

    // (2) 풀메시에서 재시작 폭주 전/중
    let cfg = EngineConfig { buffer_size: 128, frame_ms: 2.5, codec: CodecKind::Pcm16, channels: 1, input_channels: vec![0], ..Default::default() };
    let engines: Vec<Engine> = (0..PEERS)
        .map(|i| Engine::start(cfg.clone(), Arc::new(sine_backend(220.0 * (i + 1) as f64)), Arc::new(MixControls::default()), Some((format!("p{i}"), test_net()))).unwrap())
        .collect();
    for (i, e) in engines.iter().enumerate() {
        for (j, o) in engines.iter().enumerate() {
            if i != j {
                e.add_peer(format!("p{j}"), &with_candidate(o.descriptor().unwrap(), addr(o))).unwrap();
            }
        }
    }
    let t0 = Instant::now();
    while !engines.iter().all(|e| e.stats().peers.iter().filter(|p| p.state == PeerStateKind::Connected).count() == PEERS - 1) {
        assert!(t0.elapsed() < Duration::from_secs(30), "풀메시 연결 실패");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_secs(3));
    let medias: Vec<_> = (1..PEERS).map(|j| engines[0].echo_job(&format!("p{j}")).unwrap().media).collect();
    // 다른 엔진들이 받는 p0 소리의 (은닉, 생산) 합
    let p0_stream = |es: &[Engine]| -> (u64, u64) {
        es[1..].iter().flat_map(|e| e.peer_jitter_stats()).filter(|(id, _)| id == "p0").fold((0, 0), |a, (_, s)| (a.0 + s.concealed, a.1 + s.produced))
    };
    let window = |storm: bool| -> (u64, u64, f64, f32) {
        let (f0, x0, (c0, n0)) = (engines[0].feed_stats().1, engines[0].stats().xruns, p0_stream(&engines));
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(8) {
            if storm {
                for m in &medias {
                    m.request_restart();
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let (f1, s1, (c1, n1)) = (engines[0].feed_stats().1, engines[0].stats(), p0_stream(&engines));
        let pct = (c1 - c0) as f64 * 100.0 / (n1 - n0).max(1) as f64;
        (f1 - f0, s1.xruns - x0, pct, s1.cpu.worker_pct.unwrap_or(f32::NAN))
    };
    let quiet = window(false);
    let storm = window(true);
    let quiet2 = window(false);
    println!("| 창(8s, PCM16 2.5ms, 6엔진) | 엔진0 건너뛴 프레임 | 엔진0 xrun | 다른 엔진이 받는 p0 은닉(%) | 엔진0 워커 CPU(%) |");
    println!("|---|---|---|---|---|");
    for (name, w) in [("조용함", quiet), ("재시작 폭주(피어 5명 × 20회/s)", storm), ("조용함(다시)", quiet2)] {
        println!("| {name} | {} | {} | {:.3} | {:.2} |", w.0, w.1, w.2, w.3);
    }
    for mut e in engines {
        e.stop();
    }
}
