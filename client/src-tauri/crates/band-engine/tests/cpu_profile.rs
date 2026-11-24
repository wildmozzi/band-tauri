//! 6인 풀메시 CPU 측정(P1-10, GOAL 2절 "6인 풀메시 60분, CPU ≤ 15%(4코어 기준)").
//!
//! 한 프로세스에 가짜 장치(사인 입력) 엔진 6개를 띄우고 127.0.0.1로 풀메시 연결한다(각 엔진 = 자신 + 원격 5명).
//! 엔진마다 `EngineStats.cpu`(워커 CPU%, RT 콜백 사용률·p99)를 읽고, 프로세스 전체 CPU(`QueryProcessCycleTime`
//! 증분 / TSC 증분) / 6으로
//! 연주자 1명 몫(워커 + 네트워크 수신·제어 + 가짜 장치 스레드)을 낸다. 수치 판정 없이 표만 출력한다(측정 도구).
//!
//! 실행(수치는 release 기준이 의미 있다):
//! `cargo test --release --test cpu_profile -- --ignored --nocapture`
//!
//! 전·후 비교(ADR D6, P1-21b): `BAND_CPU_PIN=pe`(LP-E를 뺀 P+E에 고정 — 반복당 비용) / 비움(고정 없음 — 배치 확률)으로 돌리고,
//! 구성마다 찍는 `CSV|` 줄(열 이름 `CSVHEAD|`)을 `scripts/cpu-compare.mjs`가 모아 통계를 낸다. 방법: `docs/overhaul/cpu-measure.md`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use band_dsp::codec::Encoder;
use band_dsp::config::{CodecKind, EngineConfig};
use band_dsp::mixer::MixControls;
use band_dsp::types::PeerStateKind;
use band_engine::testing::engine::Engine;
use band_engine::testing::sys::placement::{describe_cores, pin_process};

// 측정 본체(엔진 6개 풀메시·측정 창·`Row`)는 예제 `policy_matrix`와 함께 쓴다.
#[path = "support/mesh_cpu.rs"]
mod mesh_cpu;
use mesh_cpu::{addr, run, sine_backend, test_net, with_candidate, Row, PEERS};

/// 구성마다 예열 3초 + 측정 창 12초(`cpu-measure.md` — 2026-11-17·11-20 측정과 같은 값).
const WARMUP: Duration = Duration::from_secs(3);
const MEASURE: Duration = Duration::from_secs(12);

#[test]
#[ignore = "측정 도구: cargo test --release --test cpu_profile -- --ignored --nocapture"]
fn six_peer_full_mesh_cpu() {
    // P1-21 2단계 비교: 엔진이 환경변수 `BAND_THREAD_POLICY` = off | highqos | pcores | both를 직접 읽는다(제품 빌드와 같은
    // 경로, 옛 이름 qos·qos+pcores도 받음). 비우면 엔진 설정 기본값(HighQoS 켬, P 코어 끔)이라 `default`로 적는다. 실제 적용 결과는
    // 아래 `배치 |` 줄 끝에 찍힌다.
    let policy = match std::env::var("BAND_THREAD_POLICY").unwrap_or_default().trim().to_ascii_lowercase().as_str() {
        "" => "default".to_string(),
        p => p.to_string(),
    };
    println!("스레드 정책: {policy}");
    println!("코어 구성: {}", describe_cores());
    // P1-21b(ADR D6): `BAND_CPU_PIN=pe`면 프로세스를 LP-E를 뺀 P+E 코어에 고정한다(전·후 비교 측정용 — 반복당 비용만 본다).
    // `0x…`이면 그 마스크(그룹 0). 비우거나 `none`이면 고정 없음(배치 확률 쪽 회귀를 보는 반복 측정용). 엔진 시작 전에 정한다.
    let pin = match std::env::var("BAND_CPU_PIN").unwrap_or_default().trim().to_ascii_lowercase().as_str() {
        "" | "none" => "none".to_string(),
        "pe" => format!("0x{:X}", pin_process(None).expect("BAND_CPU_PIN=pe")),
        m => {
            let mask = u64::from_str_radix(m.trim_start_matches("0x"), 16).expect("BAND_CPU_PIN: pe | none | 0x<16진 마스크>");
            format!("0x{:X}", pin_process(Some(mask)).expect("BAND_CPU_PIN 마스크"))
        }
    };
    println!("코어 고정: {pin}");
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
    // mono 2.5ms 값은 실행 사이 기계 속도 기준값으로 `CSV|` 줄에도 싣는다(P1-21b).
    let mut opus_ref_us = f64::NAN;
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
        if (ch, frame) == (1, 120) {
            opus_ref_us = us;
        }
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
        .map(|(c, f, force)| run(c, f, force, WARMUP, MEASURE))
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
    // 코어 배치(P1-21): 측정 창 동안 6엔진 합, 스레드별 P / E / LP-E 비율(%)과 표본 수. 비Windows는 줄 없음.
    for r in &rows {
        if let Some(p) = r.placement {
            let pct = |t: [f64; 3]| {
                let n: f64 = t.iter().sum();
                let f = |x: f64| if n > 0.0 { (x * 100.0 / n).max(0.0) } else { 0.0 };
                format!("P {:.1} / E {:.1} / LP-E {:.1} (n {:.0})", f(t[0]), f(t[1]), f(t[2]), n)
            };
            println!("배치 | {} | 워커 {} | 수신 {} | 정책 {}", r.label, pct(p[0]), pct(p[1]), r.policy_applied);
        }
    }
    // 기계가 읽는 줄(P1-21b, ADR D6): `scripts/cpu-compare.mjs`가 실행마다 모아 전·후 쌍별 차를 낸다. 열 이름은 `CSVHEAD|`.
    // 워커 LP-E 비율은 고정 없는 반복에서 "느린 코어에 놓인 실행"을 가르는 데 쓴다(비Windows는 빈 값).
    // 워커·수신 P 비율과 최대 은닉은 스레드 정책 비교(P1-21 2단계)에 쓴다.
    println!("CSVHEAD|config|pin|policy|opus_ref_us|worker_pct_e0|worker_pct_avg6|worker_pct_max6|cb_load_avg_pct|cb_p50_us_e0|cb_p99_us_e0|cb_p99_us_max6|per_node_pct|iter_body_p50_us_avg6|iter_body_p99_us_avg6|iter_body_max_us_max6|iters_avg6|iter_period_p50_us_avg6|worker_lpe_pct|netrecv_lpe_pct|worker_p_pct|netrecv_p_pct|conceal_max_pct|xruns");
    for r in &rows {
        let share = |t: [f64; 3], i: usize| {
            let n: f64 = t.iter().sum();
            if n > 0.0 { format!("{:.1}", (t[i] * 100.0 / n).max(0.0)) } else { String::new() }
        };
        let (wl, nl, wp, np) = r.placement.map(|p| (share(p[0], 2), share(p[1], 2), share(p[0], 0), share(p[1], 0))).unwrap_or_default();
        println!(
            "CSV|{}|{pin}|{policy}|{opus_ref_us:.1}|{:.3}|{:.3}|{:.3}|{:.4}|{:.1}|{:.1}|{:.1}|{:.3}|{:.1}|{:.1}|{:.1}|{:.0}|{:.1}|{wl}|{nl}|{wp}|{np}|{:.3}|{}",
            r.label,
            r.me.worker_pct.unwrap_or(f32::NAN),
            r.worker_avg,
            r.worker_max,
            r.load_avg,
            r.me.callback_p50_us,
            r.me.callback_p99_us,
            r.p99_max,
            r.per_node_pct,
            r.body[0],
            r.body[1],
            r.body[2],
            r.body[3],
            r.body[4],
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
/// `cargo test --release --test cpu_profile restart_cost -- --ignored --nocapture`
#[test]
#[ignore = "측정: release 권장(약 40초)"]
fn restart_cost_on_worker() {
    use band_dsp::jitter::{JitterBuffer, JitterSettings};
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
