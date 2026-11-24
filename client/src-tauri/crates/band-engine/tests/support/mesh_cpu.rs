//! 6엔진 풀메시 CPU 측정의 본체 — 시험 `tests/cpu_profile.rs`(P1-10·P1-21b)와 예제 `examples/policy_matrix.rs`(P1-21 3단계 ②
//! 정책 × 환경 교차 측정)가 `#[path]`로 함께 쓴다. 한 프로세스에 가짜 장치(사인 입력) 엔진 6개를 띄워 127.0.0.1 풀메시로
//! 잇고 측정 창 동안의 워커 CPU·반복 본문 시간·xrun·은닉·코어 배치를 낸다(엔진 수는 `run_n`으로 바꾼다 — `policy_matrix --engines`).
//! 수치 판정은 없다.
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
    /// 측정 창 워커 반복 본문(`iter_body`, 수면 제외) p50·p99(엔진 평균, µs)·최대(엔진 최대, µs)·반복 수(엔진 평균),
    /// 반복 간격 p50(µs). ADR D6 비교 측정의 보조 지표(P1-21b — `cpu-quiet-2026-11-17.md` 측정 패치를 정식으로).
    pub body: [f32; 5],
    /// 측정 창 동안 엔진 합 코어 배치(P1-21): [워커, 수신] × [P, E, LP-E] 표본 수. 비Windows는 None.
    pub placement: Option<[[f64; 3]; 2]>,
    /// 엔진 0이 보고한 스레드 정책 적용 결과(`EngineStats.threadPolicy` JSON, P1-21 2단계). 비Windows는 빈 문자열.
    pub policy_applied: String,
    /// 예열·안정화 구간(풀메시 연결 뒤 ~ 측정 창 시작)의 xrun(엔진 합). **집계(`xruns`)에서 뺀 값**이다 — 정책마다 같은 규칙.
    /// 조건부 고정은 엔진 시작마다의 감지 구간(풀기 전 나쁜 창)이 여기 들어간다(#258 재리뷰 M1).
    pub settle_xruns: u64,
    /// 조건부 고정의 엔진 전체 상태(#258 재리뷰 M1·L2). 그 정책이 아니거나 비Windows면 None.
    pub adaptive: Option<AdaptiveAgg>,
}

/// 조건부 고정(3안)의 엔진 전체 상태: 측정 창 시작·끝 두 시점의 엔진별 값을 모은 것.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AdaptiveAgg {
    /// 조건부 고정 상태를 보고한 엔진 수(처음 묶기가 되지 않은 엔진은 빠진다).
    pub engines: u32,
    /// 측정 창 시작 시점에 풀려 있던 엔진 수 — 감지 구간이 예열 안에서 끝난 엔진.
    pub released_at_start: u32,
    /// 측정 창 끝 시점에 풀려 있던 엔진 수.
    pub released_at_end: u32,
    /// 예열 구간(엔진 시작 ~ 측정 창 시작)에 푼 횟수(엔진 합).
    pub settle_releases: u32,
    /// 측정 창 안에서 푼 횟수·다시 묶은 횟수(엔진 합). 0이 아니면 창 안에 감지 구간이나 다시 묶기가 들어 있다.
    pub window_releases: u32,
    pub window_rebinds: u32,
    /// 처음 묶인 채로 측정 창에 들어가 창 안에서 처음 푼 엔진 수 — 감지 구간(풀기 전 나쁜 창)이 창 안에 들어간 엔진.
    pub first_release_in_window: u32,
    /// 엔진 시작부터 첫 풀림을 본 시각(초, 예열 중 250ms 간격으로 확인 — 예열 안에서 푼 엔진만)의 중앙값.
    pub first_release_s_median: Option<f32>,
}

/// `EngineStats.threadPolicy.adaptive`의 (풀려 있음, 푼 횟수, 다시 묶은 횟수). 계약 JSON으로 읽는다.
pub fn adaptive_state(s: &band_engine::testing::engine::report::EngineStats) -> Option<(bool, u32, u32)> {
    let v = serde_json::to_value(s).ok()?;
    let a = v.get("threadPolicy")?.get("adaptive")?;
    Some((a.get("released")?.as_bool()?, a.get("releases")?.as_u64()? as u32, a.get("rebinds")?.as_u64()? as u32))
}

/// 측정 창 시작·끝의 엔진별 상태와 예열 중 본 첫 풀림 시각으로 `AdaptiveAgg`를 만든다. 상태가 하나도 없으면 None.
pub fn aggregate_adaptive(start: &[Option<(bool, u32, u32)>], end: &[Option<(bool, u32, u32)>], first_release_s: &[Option<f32>]) -> Option<AdaptiveAgg> {
    let mut a = AdaptiveAgg::default();
    for (s, e) in start.iter().zip(end) {
        let (Some((s_rel, s_n, s_rb)), Some((e_rel, e_n, e_rb))) = (s, e) else { continue };
        a.engines += 1;
        a.released_at_start += u32::from(*s_rel);
        a.released_at_end += u32::from(*e_rel);
        a.settle_releases += s_n;
        a.window_releases += e_n.saturating_sub(*s_n);
        a.window_rebinds += e_rb.saturating_sub(*s_rb);
        a.first_release_in_window += u32::from(*s_n == 0 && *e_n > 0);
    }
    let mut firsts: Vec<f32> = first_release_s.iter().flatten().copied().collect();
    firsts.sort_by(f32::total_cmp);
    a.first_release_s_median = (!firsts.is_empty()).then(|| {
        let k = firsts.len();
        if k % 2 == 1 { firsts[k / 2] } else { (firsts[k / 2 - 1] + firsts[k / 2]) / 2.0 }
    });
    (a.engines > 0).then_some(a)
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
    run_n(PEERS, codec, frame_ms, force_auto, warmup, measure)
}

/// `run`과 같되 엔진 수를 정한다(2 이상). 2면 1:1 루프백 — 한 프로세스의 엔진 스레드가 워커·수신 2개씩이라, 엔진 1개만 도는
/// 제품에 가깝다(6엔진은 스레드 12개가 P 코어를 두고 스스로 다툰다 — #258 리뷰 M2). 평균·합은 이 엔진 수 기준이다.
pub fn run_n(n: usize, codec: CodecKind, frame_ms: f32, force_auto: bool, warmup: Duration, measure: Duration) -> Row {
    assert!(n >= 2, "엔진은 2개 이상(풀메시)");
    let cfg = EngineConfig { buffer_size: 128, frame_ms, codec, channels: 1, input_channels: vec![0], ..Default::default() };
    let started = Instant::now();
    let engines: Vec<Engine> = (0..n)
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
        s.peers.len() == n - 1 && s.peers.iter().all(|p| p.state == PeerStateKind::Connected)
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
    // 예열·안정화(지터 버퍼 수렴, 조건부 고정의 감지 구간) 후 창을 한 번 회전해 기준을 맞춘다. 예열 구간의 xrun은 집계에서
    // 빼고 따로 낸다(정책마다 같은 규칙 — #258 재리뷰 M1). 예열 중에는 250ms마다 조건부 고정의 첫 풀림을 본다(측정 창 안에서는
    // 보지 않는다 — 창의 CPU 값에 섞이지 않게).
    let settle_x0: u64 = engines.iter().map(|e| e.stats().xruns).sum();
    let mut first_release_s: Vec<Option<f32>> = vec![None; n];
    let settle_end = Instant::now() + warmup;
    loop {
        for (slot, e) in first_release_s.iter_mut().zip(&engines) {
            if slot.is_none() && adaptive_state(&e.stats()).is_some_and(|(_, releases, _)| releases > 0) {
                *slot = Some(started.elapsed().as_secs_f32());
            }
        }
        let left = settle_end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(Duration::from_millis(250)));
    }
    if force_auto {
        for (i, e) in engines.iter().enumerate() {
            for p in e.stats().peers {
                assert_eq!(p.tx_codec_reason, Some(CodecReason::LossFeedback), "엔진 {i} → {}: 강제 전환이 안 됨", p.peer_id);
                assert_eq!(p.tx_codec, Some(CodecKind::Opus));
            }
        }
    }
    // 창 회전과 xrun 기준(연결·예열 구간 제외)
    let stats0: Vec<_> = engines.iter().map(|e| e.stats()).collect();
    let x0: u64 = stats0.iter().map(|s| s.xruns).sum();
    let pl0 = sum_placement(&stats0);
    let ad0: Vec<_> = stats0.iter().map(adaptive_state).collect();
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
        label: if force_auto { format!("{codec:?} {frame_ms}ms + {}피어 Opus 개별 인코딩(손실 피드백 강제)", n - 1) } else { format!("{codec:?} {frame_ms}ms") },
        me: cpus[0],
        worker_avg: workers.iter().sum::<f32>() / n as f32,
        worker_max: workers.iter().cloned().fold(0.0, f32::max),
        load_avg: cpus.iter().map(|c| c.callback_load_pct).sum::<f32>() / n as f32,
        p99_max: cpus.iter().map(|c| c.callback_p99_us).fold(0.0, f32::max),
        per_node_pct: cycle_pct(p1, p0, k1, k0).unwrap_or(f32::NAN) / n as f32,
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
        settle_xruns: x0.saturating_sub(settle_x0),
        adaptive: aggregate_adaptive(&ad0, &stats.iter().map(adaptive_state).collect::<Vec<_>>(), &first_release_s),
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
