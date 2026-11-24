//! 6엔진 풀메시 CPU 측정의 본체 — 시험 `tests/cpu_profile.rs`(P1-10·P1-21b)와 예제 `examples/policy_matrix.rs`(P1-21 3단계 ②
//! 정책 × 환경 교차 측정)가 `#[path]`로 함께 쓴다. 한 프로세스에 가짜 장치(사인 입력) 엔진 6개를 띄워 127.0.0.1 풀메시로
//! 잇고 측정 창 동안의 워커 CPU·반복 본문 시간·xrun·은닉·코어 배치를 낸다. 수치 판정은 없다.
#![allow(dead_code)] // 쓰는 쪽마다 쓰는 항목이 다르다

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use band_dsp::config::{CodecKind, EngineConfig};
use band_engine::testing::engine::cpuprof::CpuStats;
use band_engine::testing::sys::cpu::{cycle_pct, process_cycles, tsc_now};
use band_engine::testing::engine::device::fake::FakeBackend;
use band_dsp::mixer::MixControls;
use band_dsp::relay_codec::CodecReason;
use band_dsp::types::PeerStateKind;
use band_engine::testing::engine::Engine;
use band_engine::testing::net::peer::{Candidate, CandidateKind, LocalDescriptor};
use band_engine::testing::net::NetOptions;

pub const PEERS: usize = 6;

pub fn test_net() -> NetOptions {
    NetOptions { port: 0, stun_servers: vec![], gather_lan: false, extra_candidates: vec![], allow_loopback_candidates: true, cancel: None }
}

pub fn sine_backend(hz: f64) -> FakeBackend {
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

pub fn addr(e: &Engine) -> SocketAddr {
    format!("127.0.0.1:{}", e.local_port().unwrap()).parse().unwrap()
}

pub fn with_candidate(d: &LocalDescriptor, a: SocketAddr) -> LocalDescriptor {
    LocalDescriptor { candidates: vec![Candidate { kind: CandidateKind::Lan, addr: a.to_string() }], ..d.clone() }
}

pub struct Row {
    pub label: String,
    pub me: CpuStats,
    pub worker_avg: f32,
    pub worker_max: f32,
    pub load_avg: f32,
    pub p99_max: f32,
    pub per_node_pct: f32,
    pub conceal_max: f32,
    pub xruns: u64,
    /// 측정 창 워커 반복 본문(`iter_body`, 수면 제외) p50·p99(6엔진 평균, µs)·최대(6엔진 최대, µs)·반복 수(6엔진 평균),
    /// 반복 간격 p50(µs). ADR D6 비교 측정의 보조 지표(P1-21b — `cpu-quiet-2026-11-17.md` 측정 패치를 정식으로).
    pub body: [f32; 5],
    /// 측정 창 동안 6엔진 합 코어 배치(P1-21): [워커, 수신] × [P, E, LP-E] 표본 수. 비Windows는 None.
    pub placement: Option<[[f64; 3]; 2]>,
    /// 엔진 0이 보고한 스레드 정책 적용 결과(`EngineStats.threadPolicy` JSON, P1-21 2단계). 비Windows는 빈 문자열.
    pub policy_applied: String,
}

/// `EngineStats.corePlacement`(누적)에서 [워커, 수신] × [P, E, LP-E] 표본 수. 계약 JSON으로 읽는다(공개 표면을 늘리지 않음).
pub fn placement_counts(s: &band_engine::testing::engine::report::EngineStats) -> Option<[[f64; 3]; 2]> {
    let v = serde_json::to_value(s).ok()?;
    let cp = v.get("corePlacement")?;
    let one = |t: &serde_json::Value| -> [f64; 3] {
        let n = t.get("samples").and_then(|x| x.as_f64()).unwrap_or(0.0);
        ["pPct", "ePct", "lpEPct"].map(|k| n * t.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) / 100.0)
    };
    Some([one(cp.get("worker")?), cp.get("netRecv").map(one).unwrap_or([0.0; 3])])
}

pub fn sum_placement(stats: &[band_engine::testing::engine::report::EngineStats]) -> Option<[[f64; 3]; 2]> {
    let mut acc = [[0.0; 3]; 2];
    for s in stats {
        let p = placement_counts(s)?;
        for (a, b) in acc.iter_mut().flatten().zip(p.iter().flatten()) {
            *a += b;
        }
    }
    Some(acc)
}

/// 구성 하나를 잰다: 엔진 6개를 띄워 풀메시로 잇고, `warmup` 예열 뒤 `measure` 창을 잰다(끝나면 엔진을 멈춘다).
pub fn run(codec: CodecKind, frame_ms: f32, force_auto: bool, warmup: Duration, measure: Duration) -> Row {
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
    std::thread::sleep(warmup);
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
    let pl0 = sum_placement(&engines.iter().map(|e| e.stats()).collect::<Vec<_>>());
    for e in &engines {
        let _ = e.take_worker_timing();
    }
    let (p0, k0) = (process_cycles(), tsc_now());
    std::thread::sleep(measure);
    let stats: Vec<_> = engines.iter().map(|e| e.stats()).collect();
    let (p1, k1) = (process_cycles(), tsc_now());
    let wts: Vec<_> = engines.iter().map(|e| e.take_worker_timing()).collect();
    let cpus: Vec<CpuStats> = stats.iter().map(|s| s.cpu).collect();
    for c in &cpus {
        assert!(c.window_ms >= measure.as_millis() as f32 * 0.9, "창 회전: {c:?}");
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
        body: {
            let avg = |v: Vec<f32>| v.iter().sum::<f32>() / v.len() as f32;
            [
                avg(wts.iter().map(|w| w.body.p50_ms * 1000.0).collect()),
                avg(wts.iter().map(|w| w.body.p99_ms * 1000.0).collect()),
                wts.iter().map(|w| w.body.max_ms * 1000.0).fold(0.0, f32::max),
                avg(wts.iter().map(|w| w.body.packets as f32).collect()),
                avg(wts.iter().map(|w| w.period.p50_ms * 1000.0).collect()),
            ]
        },
        policy_applied: serde_json::to_value(&stats[0]).ok().and_then(|v| v.get("threadPolicy").filter(|p| !p.is_null()).map(|p| p.to_string())).unwrap_or_default(),
        placement: sum_placement(&stats).zip(pl0).map(|(a, b)| {
            let mut d = [[0.0; 3]; 2];
            for ((x, a), b) in d.iter_mut().flatten().zip(a.iter().flatten()).zip(b.iter().flatten()) {
                *x = a - b;
            }
            d
        }),
    };
    for mut e in engines {
        e.stop();
    }
    row
}
