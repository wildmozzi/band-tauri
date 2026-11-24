//! 워커 반복 전체의 할당·재할당·해제 0(엔진 ⑧ 후속, #150 후속 "계수 구간을 워커 루프 전체로").
//!
//! 이 크레이트의 단위 시험 바이너리는 계수 할당자(`sys::alloc::Counting`, `testing::alloc`으로 다시 내보냄)를 글로벌 할당자로 건다. 워커 스레드는 시험
//! 구성에서 반복 하나 전체(에코 프로브 → 피어 표 → 캡처·로컬 → 인코드·송신(자동 코덱 전환 포함) → 시각 → 메트로놈 → 피어 공급
//! (슬롯 배정·제거·녹음 마커·통계 발행) → 로컬 공급 → 장치 감시 → 반복 시간 기록, 수면만 뺌)를 자기 스레드의 계수 구간으로
//! 감싸고 그 누계를 `WorkerStats::iter_allocs`에 더한다(`Worker::run`). 계수는 스레드별이라 제어 평면(이 시험 스레드)·net·RT
//! 스레드와 병렬로 도는 다른 시험의 할당은 섞이지 않는다.
//!
//! 시험은 가짜 장치 + 실제 net(127.0.0.1)으로 엔진을 띄우고, 시작·연결·워밍업이 끝난 뒤 기준값을 잡는다. 그 뒤 정상 구간과
//! 사건(피어 추가·교체·제거, 게인·지터 프로필, 메트로놈 계획 시작·변경·기준 피어 따르기·정지, 녹음 시작·정지, 에코 측정 시작·끝,
//! 자동 코덱 전환 PCM16 ↔ Opus)을 거치며 반복마다 0인지 본다.

use super::*;
use crate::engine::device::fake::FakeBackend;
use crate::net::peer::{Candidate, CandidateKind};
use band_dsp::config::CodecKind;
use band_dsp::types::PeerStateKind;

const HANG: Duration = Duration::from_secs(15);

fn sine() -> FakeBackend {
    FakeBackend::new(
        48_000,
        Arc::new(|start: u64, ch: usize, buf: &mut [f32]| {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let t = (start + i as u64) as f64 / 48_000.0;
                frame.fill((t * 440.0 * 2.0 * std::f64::consts::PI).sin() as f32 * 0.3);
            }
        }),
    )
}

fn start(name: &str, codec: CodecKind) -> Engine {
    start_cfg(name, EngineConfig { buffer_size: 128, codec, ..Default::default() })
}

fn start_cfg(name: &str, cfg: EngineConfig) -> Engine {
    let net = NetOptions { port: 0, stun_servers: vec![], gather_lan: false, extra_candidates: vec![], allow_loopback_candidates: true, cancel: None };
    Engine::start(cfg, Arc::new(sine()), Arc::new(MixControls::default()), Some((name.into(), net))).unwrap()
}

/// `to`의 기술자를 127.0.0.1 후보 하나로.
fn desc_of(to: &Engine) -> LocalDescriptor {
    let addr = format!("127.0.0.1:{}", to.local_port().unwrap());
    LocalDescriptor { candidates: vec![Candidate { kind: CandidateKind::Lan, addr }], ..to.descriptor().unwrap().clone() }
}

fn wait_until(mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(t0.elapsed() < HANG, "시간 초과");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn connected(e: &Engine, n: usize) -> bool {
    let s = e.stats();
    s.peers.len() == n && s.peers.iter().all(|p| p.state == PeerStateKind::Connected)
}

/// 워커 반복 수만큼 기다린다(시간이 아니라 진행량). 그동안 프론트처럼 `stats`를 부른다(반환 큐 회수 포함).
fn run_iters(es: &[&Engine], n: u64) {
    let base: Vec<u64> = es.iter().map(|e| e.worker_stats.iters.load(Ordering::Relaxed)).collect();
    wait_until(|| {
        for e in es {
            let _ = e.stats();
        }
        es.iter().zip(&base).all(|(e, b)| e.worker_stats.iters.load(Ordering::Relaxed) >= b + n)
    });
}

/// 워커 반복 전체 계수 누계(할당·재할당·해제)와 반복 수.
fn allocs(e: &Engine) -> (u64, u64) {
    (e.worker_stats.iter_allocs.load(Ordering::Relaxed), e.worker_stats.iters.load(Ordering::Relaxed))
}

/// 사건 하나를 하고 그 뒤 반복 `n`번 동안 워커 할당이 0인지 본다. 0이 아니면 어느 사건인지 이름으로 실패한다.
fn step(es: &[(&str, &Engine)], what: &str, n: u64, act: impl FnOnce()) {
    let before: Vec<(u64, u64)> = es.iter().map(|(_, e)| allocs(e)).collect();
    act();
    run_iters(&es.iter().map(|(_, e)| *e).collect::<Vec<_>>(), n);
    for ((name, e), (a0, i0)) in es.iter().zip(before) {
        let (a1, i1) = allocs(e);
        assert_eq!(a1 - a0, 0, "{what}: 엔진 {name}의 워커가 반복 {}번 동안 할당·해제를 {}회 했다", i1 - i0, a1 - a0);
    }
}

/// 로그 인자까지 평가되게 한다: 모든 레벨을 켜고, 받은 메시지를 `io::sink`에 포맷한다(할당 없음). 로거가 없으면 `log::*!`는
/// 인자를 평가하지 않아 워커 로그 줄의 `to_string`·`format!`이 계수에 안 잡힌다. 실제 앱 로거 자신의 할당(파일·콘솔 기록)은
/// 이 시험 범위 밖이다 — 워커 로그는 사건·레이트 제한 경로뿐이다(engine-architecture.md 4.3절 4).
fn sink_logger() {
    struct Sink;
    impl log::Log for Sink {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }
        fn log(&self, r: &log::Record) {
            use std::io::Write;
            let _ = std::io::sink().write_fmt(*r.args());
        }
        fn flush(&self) {}
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if log::set_logger(&Sink).is_ok() {
            log::set_max_level(log::LevelFilter::Trace);
        }
    });
}

fn scenario(codec: CodecKind) {
    sink_logger();
    let a = start("alice", codec);
    let b = start("bob", codec);
    let c = start("carol", codec);
    a.add_peer("bob".into(), &desc_of(&b)).unwrap();
    b.add_peer("alice".into(), &desc_of(&a)).unwrap();
    wait_until(|| connected(&a, 1) && connected(&b, 1));
    // 워밍업: 연결 직후의 첫 수신·지터 적응·슬롯 배정이 지나가게(시작 구간은 계수 대상이 아니다).
    run_iters(&[&a, &b], 400);
    let (a_start, a_iters) = allocs(&a);
    let ab = [("alice", &a), ("bob", &b)];

    step(&ab, "정상 구간", 1500, || {});
    step(&ab, "게인·뮤트·팬·지터 프로필", 200, || {
        a.set_peer_gain("bob", 0.5, false, Some(-0.3)).unwrap();
        a.set_peer_gain("bob", 1.0, true, None).unwrap();
        a.set_jitter_profile(0.8).unwrap();
        b.set_jitter_profile(0.2).unwrap();
    });
    let metro = |running, bpm, r: Option<&str>| metronome::MetronomeConfig {
        running,
        bpm,
        beats_per_bar: 4,
        start_us: None,
        ref_peer_id: r.map(String::from),
    };
    step(&ab, "메트로놈 시작", 400, || {
        a.set_metronome(metro(true, 120.0, None)).unwrap();
    });
    // 기준 피어를 따르는 쪽은 방장 계획의 박 0 시각(방장 시계)을 받는다(시그널링이 나르는 값).
    let follow = |bpm, at: f64| metronome::MetronomeConfig { start_us: Some(at), ..metro(true, bpm, Some("alice")) };
    step(&ab, "메트로놈 기준 피어 따르기", 400, || {
        let plan = a.set_metronome(metro(true, 140.0, None)).unwrap();
        b.set_metronome(follow(140.0, plan.start_us)).unwrap();
    });
    step(&ab, "메트로놈 계획 변경·정지", 400, || {
        let plan = a.set_metronome(metro(true, 90.0, None)).unwrap();
        b.set_metronome(follow(90.0, plan.start_us)).unwrap();
        a.set_metronome(metro(false, 90.0, None)).unwrap();
        b.set_metronome(metro(false, 90.0, None)).unwrap();
    });

    let dir = std::env::temp_dir().join(format!("band-worker-alloc-{codec:?}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    step(&ab, "녹음 시작", 600, || {
        a.record_start(record::RecordOptions { include_mix: true, ..Default::default() }, &dir).unwrap();
    });
    step(&ab, "녹음 정지", 200, || {
        let job = a.recorder().lock().begin_stop().unwrap();
        job.finish().unwrap();
        a.recorder().lock().reclaim();
    });
    std::fs::remove_dir_all(&dir).ok();

    let echo = || {
        // B는 요청을 보면 동의한다(사람이 UI에서 허용). 측정 뒤 B는 에코를 스스로 멈춘다.
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    if b.stats().peers.iter().any(|p| p.peer_id == "alice" && p.echo_requested) {
                        let _ = b.set_echo_mode("alice", true);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
            let r = a.echo_job("bob").unwrap().run(2, Duration::from_secs(5), &AtomicBool::new(false));
            stop.store(true, Ordering::Relaxed);
            r.unwrap();
        });
        wait_until(|| b.stats().peers.iter().all(|p| !p.echo_active && !p.echo_requested));
    };
    step(&ab, "에코 측정 시작·끝", 200, echo);

    if codec == CodecKind::Pcm16 {
        // 자동 코덱 전환(PCM16 설정의 피어별 Opus 전환 송신, `AutoCodec`): 손실 피드백과 같은 경로를 강제로 켜고 끈다.
        step(&ab, "자동 코덱 전환 PCM16 → Opus", 600, || {
            a.force_auto_opus(true).unwrap();
            b.force_auto_opus(true).unwrap();
        });
        // 자동 전환 중 에코 측정: 클릭 스트림 대상 피어는 그동안 자동 인코드를 쉬고, 끝나면 새 스트림으로 다시 예열한다.
        step(&ab, "자동 전환 중 에코 측정 시작·끝", 600, echo);
        step(&ab, "자동 코덱 전환 Opus → PCM16", 600, || {
            a.force_auto_opus(false).unwrap();
            b.force_auto_opus(false).unwrap();
        });
    }

    step(&ab, "피어 추가", 600, || {
        a.add_peer("carol".into(), &desc_of(&c)).unwrap();
        c.add_peer("alice".into(), &desc_of(&a)).unwrap();
        wait_until(|| connected(&a, 2) && connected(&c, 1));
    });
    step(&ab, "피어 교체(같은 peerId 다시 추가)", 600, || {
        a.add_peer("carol".into(), &desc_of(&c)).unwrap();
        wait_until(|| connected(&a, 2));
    });
    step(&ab, "피어 제거", 400, || {
        a.remove_peer("carol").unwrap();
        wait_until(|| a.worker_media_peers() == 1);
    });
    step(&ab, "정상 구간(사건 뒤)", 800, || {});

    let (a_end, a_iters_end) = allocs(&a);
    assert_eq!(a_end - a_start, 0);
    // 시험이 실제로 반복을 셌는지(계수 경로가 꺼져 있으면 iters가 늘지 않는다).
    assert!(a_iters_end - a_iters >= 7000, "반복 {}번", a_iters_end - a_iters);
}

#[test]
fn worker_iteration_allocates_nothing_opus() {
    scenario(CodecKind::Opus);
}

#[test]
fn worker_iteration_allocates_nothing_pcm16_with_codec_switch() {
    scenario(CodecKind::Pcm16);
}

/// 계수 경로 자기 검증: 워커 스레드의 계수 구간이 실제로 워커 반복을 감싸고 있고, 제어 평면 스레드의 할당은 세지 않는다.
#[test]
fn worker_iteration_counter_ignores_other_threads() {
    let a = start("solo", CodecKind::Opus);
    run_iters(&[&a], 200);
    let (a0, _) = allocs(&a);
    // 이 스레드(제어 평면)에서 할당이 많아도 워커 계수는 그대로다.
    let v: Vec<Vec<u8>> = (0..1000).map(|i| vec![0u8; 64 + i]).collect();
    std::hint::black_box(&v);
    run_iters(&[&a], 200);
    assert_eq!(allocs(&a).0 - a0, 0);
}

/// 코어 배치(P1-21): 워커 반복·수신 루프가 표본을 세고 `EngineStats.corePlacement`로 나간다(Windows). 비Windows는 null.
#[test]
fn core_placement_is_reported_for_worker_and_recv() {
    let a = start("pa", CodecKind::Opus);
    let b = start("pb", CodecKind::Opus);
    a.add_peer("pb".into(), &desc_of(&b)).unwrap();
    b.add_peer("pa".into(), &desc_of(&a)).unwrap();
    wait_until(|| connected(&a, 1) && connected(&b, 1));
    run_iters(&[&a], 200);
    let s = a.stats();
    if cfg!(windows) {
        let cp = s.core_placement.expect("Windows는 코어 배치를 보고한다");
        assert!(cp.worker.samples >= 200, "워커 표본 {}", cp.worker.samples);
        assert!((cp.worker.p_pct + cp.worker.e_pct + cp.worker.lp_e_pct - 100.0).abs() < 0.01);
        let r = cp.net_recv.expect("네트워크 엔진은 수신 스레드 배치가 있다");
        assert!(r.samples > 0, "수신 표본 0");
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"corePlacement\":{\"worker\":{\"samples\":") && j.contains("\"lpEPct\""), "{j}");
    } else {
        assert!(s.core_placement.is_none());
    }
}

/// 스레드 정책(P1-21): 설정으로 고른 P 코어 정책(고정·선호 힌트·조건부 고정 — 옛 필드 `preferPCores`만 적은 설정은 고정)으로
/// 시작한 엔진은 워커·수신 스레드의 적용 결과와 고른 정책을 `EngineStats.threadPolicy`로 낸다. 비Windows는 null.
/// (환경변수 `BAND_THREAD_POLICY`가 걸린 채로 시험을 돌리면 그 값이 이긴다 — 시험 환경에서는 비워 둔다.)
#[test]
fn thread_policy_from_config_is_applied_and_reported() {
    use crate::sys::placement::{PCoreMode as Mode, PolicyResult};
    use band_dsp::config::{PCoreMode, ThreadPolicyConfig};
    if std::env::var(crate::sys::placement::POLICY_ENV).is_ok() {
        return;
    }
    let legacy_pin = ThreadPolicyConfig { high_qos: true, prefer_p_cores: true, p_core_mode: None };
    let mode = |m| ThreadPolicyConfig { high_qos: true, prefer_p_cores: false, p_core_mode: Some(m) };
    // 계약: 옛 필드만 있으면 고정, 새 필드가 있으면 그것이 이긴다(옛 필드가 true여도)
    assert_eq!(legacy_pin.p_core_mode(), PCoreMode::Pin);
    assert_eq!(ThreadPolicyConfig { prefer_p_cores: true, ..mode(PCoreMode::Off) }.p_core_mode(), PCoreMode::Off);
    assert_eq!(serde_json::from_str::<ThreadPolicyConfig>(r#"{"pCoreMode":"adaptive"}"#).unwrap(), ThreadPolicyConfig { high_qos: true, prefer_p_cores: false, p_core_mode: Some(PCoreMode::Adaptive) });
    assert!(serde_json::from_str::<ThreadPolicyConfig>(r#"{"pCoreMode":"soft"}"#).is_err(), "모르는 정책 이름은 설정 오류");
    for (label, tp, want) in [
        ("옛 preferPCores", legacy_pin, Mode::Pin),
        ("pin", mode(PCoreMode::Pin), Mode::Pin),
        ("prefer", mode(PCoreMode::Prefer), Mode::Prefer),
        ("adaptive", mode(PCoreMode::Adaptive), Mode::Adaptive),
    ] {
        let on = |name: &str| start_cfg(name, EngineConfig { buffer_size: 128, thread_policy: Some(tp), ..Default::default() });
        let a = on("pa");
        let b = on("pb");
        a.add_peer("pb".into(), &desc_of(&b)).unwrap();
        b.add_peer("pa".into(), &desc_of(&a)).unwrap();
        wait_until(|| connected(&a, 1) && connected(&b, 1));
        run_iters(&[&a], 50);
        let s = a.stats();
        if cfg!(windows) {
            let tp = s.thread_policy.expect("Windows는 스레드 정책을 보고한다");
            let r = tp.net_recv.expect("네트워크 엔진은 수신 스레드 정책이 있다");
            assert_eq!(tp.p_core_mode, want, "{label}");
            for t in [tp.worker, r] {
                assert_eq!(t.high_qos, PolicyResult::Applied, "{label}: {tp:?}");
                // 조건부 고정은 시험 기계가 바쁘면 그사이 풀렸을 수 있다(released)
                let ok = matches!(t.p_cores, PolicyResult::Applied | PolicyResult::Skipped) || (want == Mode::Adaptive && t.p_cores == PolicyResult::Released);
                assert!(ok, "{label}: {tp:?}");
            }
            // 조건부 고정만 상태를 낸다
            assert_eq!(tp.adaptive.is_some(), want == Mode::Adaptive, "{label}: {tp:?}");
            let j = serde_json::to_string(&s).unwrap();
            assert!(j.contains("\"threadPolicy\":{\"worker\":{\"highQos\":\"applied\",\"pCores\":"), "{j}");
            assert!(j.contains(&format!("\"pCoreMode\":{}", serde_json::to_string(&want).unwrap())), "{j}");
            // 정책을 걸어도 소리는 흐른다(워커 반복·수신 표본이 계속 는다)
            let n0 = a.stats().core_placement.unwrap().worker.samples;
            run_iters(&[&a], 50);
            assert!(a.stats().core_placement.unwrap().worker.samples >= n0 + 50, "{label}");
        } else {
            assert!(s.thread_policy.is_none());
        }
    }
}

/// 기본값(P1-21 3단계): 정책 필드가 없는 설정(옛 설정 포함)·`ThreadPolicyConfig::default()`·`highQos`가 빠진 JSON은 모두
/// **HighQoS 켬, P 코어 끔**이다 — 워커·수신 스레드가 HighQoS `applied`, P 코어 `off`를 보고한다. 비Windows는 null.
#[test]
fn thread_policy_default_is_high_qos_on_p_cores_off() {
    use crate::sys::placement::{PolicyResult, ThreadPolicy};
    use band_dsp::config::ThreadPolicyConfig;
    if std::env::var(crate::sys::placement::POLICY_ENV).is_ok() {
        return;
    }
    assert_eq!(EngineConfig::default().thread_policy, None);
    assert_eq!(ThreadPolicyConfig::default(), ThreadPolicyConfig { high_qos: true, prefer_p_cores: false, p_core_mode: None });
    assert_eq!(ThreadPolicyConfig::default().p_core_mode(), band_dsp::config::PCoreMode::Off);
    // 계약 JSON: 필드가 빠지면 기본값, 명시한 false는 그대로
    let parse = |j: &str| serde_json::from_str::<ThreadPolicyConfig>(j).unwrap();
    assert_eq!(parse("{}"), ThreadPolicyConfig::default());
    assert_eq!(parse(r#"{"preferPCores":true}"#), ThreadPolicyConfig { high_qos: true, prefer_p_cores: true, p_core_mode: None });
    assert_eq!(parse(r#"{"highQos":false}"#), ThreadPolicyConfig { high_qos: false, prefer_p_cores: false, p_core_mode: None });
    let want = ThreadPolicy { high_qos: PolicyResult::Applied, p_cores: PolicyResult::Off };
    let a = start("pa", CodecKind::Opus);
    let b = start_cfg("pb", EngineConfig { buffer_size: 128, thread_policy: Some(ThreadPolicyConfig::default()), ..Default::default() });
    a.add_peer("pb".into(), &desc_of(&b)).unwrap();
    b.add_peer("pa".into(), &desc_of(&a)).unwrap();
    wait_until(|| connected(&a, 1) && connected(&b, 1));
    run_iters(&[&a, &b], 50);
    for e in [&a, &b] {
        let s = e.stats();
        if cfg!(windows) {
            let tp = s.thread_policy.expect("Windows는 스레드 정책을 보고한다");
            assert_eq!((tp.worker, tp.net_recv), (want, Some(want)));
            assert_eq!((tp.p_core_mode, tp.adaptive), (crate::sys::placement::PCoreMode::Off, None));
        } else {
            assert!(s.thread_policy.is_none());
        }
    }
}

/// 사용자가 명시적으로 끈 설정(`{ highQos: false, preferPCores: false }`)은 존중한다: 두 정책 모두 `off` — 워커·수신 스레드는
/// OS 호출 없이 이 정책이 생기기 전과 같은 경로로 돈다(호출 없음 자체는
/// `sys::placement::tests::policy_results_cover_off_applied_failed_skipped`가 고정한다).
#[test]
fn thread_policy_explicit_off_is_respected() {
    use crate::sys::placement::{PolicyResult, ThreadPolicy};
    use band_dsp::config::ThreadPolicyConfig;
    if std::env::var(crate::sys::placement::POLICY_ENV).is_ok() {
        return;
    }
    let off = ThreadPolicy { high_qos: PolicyResult::Off, p_cores: PolicyResult::Off };
    let cfg = || EngineConfig { buffer_size: 128, thread_policy: Some(ThreadPolicyConfig { high_qos: false, prefer_p_cores: false, p_core_mode: None }), ..Default::default() };
    let a = start_cfg("pa", cfg());
    let b = start_cfg("pb", cfg());
    a.add_peer("pb".into(), &desc_of(&b)).unwrap();
    b.add_peer("pa".into(), &desc_of(&a)).unwrap();
    wait_until(|| connected(&a, 1) && connected(&b, 1));
    run_iters(&[&a, &b], 50);
    for e in [&a, &b] {
        let s = e.stats();
        if cfg!(windows) {
            let tp = s.thread_policy.expect("Windows는 스레드 정책을 보고한다");
            assert_eq!((tp.worker, tp.net_recv), (off, Some(off)));
        } else {
            assert!(s.thread_policy.is_none());
        }
    }
}
