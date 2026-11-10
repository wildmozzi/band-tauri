//! WASAPI 스모크 테스트(P1-0): 이 PC의 실제 장치로 독점/저지연 공유 스트림을 열어 본다.
//!
//! 실행: `cargo run --example wasapi_probe [초=5]`
//!
//! 출력: 장치 목록(믹스 포맷, 장치 주기, IAudioClient3 엔진 주기, 독점 지원 포맷), 기본 입·출력 장치를
//! 독점 / 저지연 공유로 열었을 때의 성공 여부·주기·실측 콜백 간격 히스토그램.
//! 출력 스트림은 **무음**이다(버퍼를 0으로 채우고 AUDCLNT_BUFFERFLAGS_SILENT 플래그). 소리를 내지 않는다.

#[cfg(windows)]
fn main() {
    use band_lib::engine::wasapi::{enumerate_endpoints, probe_stream, ShareMode};

    let seconds: f64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5.0);
    println!("# WASAPI 프로브 ({seconds}초씩, 출력은 무음)\n");
    let eps = match enumerate_endpoints() {
        Ok(e) => e,
        Err(e) => {
            println!("장치 열거 실패: {e}");
            return;
        }
    };
    println!("## 장치");
    for ep in &eps {
        let (d, m) = ep.device_period_hns;
        println!(
            "- [{}{}] {} | 믹스 {:?} | GetDevicePeriod 기본 {:.2}ms 최소 {:.2}ms | IAudioClient3 {}",
            if ep.is_input { "입력" } else { "출력" },
            if ep.is_default { ", 기본" } else { "" },
            ep.name,
            ep.mix.map(|f| format!("{:?} {}ch {}Hz", f.kind, f.channels, f.rate)),
            d as f64 / 10_000.0,
            m as f64 / 10_000.0,
            match ep.shared3 {
                Some(p) => format!(
                    "기본 {} / 단위 {} / 최소 {} / 최대 {} 프레임",
                    p.default, p.fundamental, p.min, p.max
                ),
                None => "없음".into(),
            },
        );
        let fmts: Vec<String> = ep
            .exclusive_formats
            .iter()
            .map(|f| format!("{}Hz {:?}/{} {}ch", f.rate, f.kind, f.valid_bits, f.channels))
            .collect();
        println!("  독점 지원: {}", if fmts.is_empty() { "없음".to_string() } else { fmts.join(", ") });
    }

    println!("\n## 기본 장치 스트림");
    for is_input in [false, true] {
        for mode in [ShareMode::Exclusive, ShareMode::Shared3] {
            let label = format!("{} {:?}", if is_input { "입력" } else { "출력" }, mode);
            match probe_stream(is_input, None, mode, seconds) {
                Err(e) => println!("\n### {label}: 열기 실패 — {e}"),
                Ok(r) => {
                    let period_ms = r.period_frames as f64 * 1000.0 / r.format.rate as f64;
                    let n = r.intervals_us.len();
                    let mut v = r.intervals_us.clone();
                    v.sort_unstable();
                    let pct = |p: f64| v.get(((n as f64 - 1.0) * p).round() as usize).copied().unwrap_or(0) as f64 / 1000.0;
                    let mean = if n > 0 { v.iter().map(|&x| x as f64).sum::<f64>() / n as f64 / 1000.0 } else { 0.0 };
                    let rate = r.frames as f64 / seconds;
                    println!(
                        "\n### {label}: 열림 — {} | {:?} {}ch {}Hz | 주기 {} 프레임({period_ms:.2}ms), 버퍼 {} 프레임",
                        r.name, r.format.kind, r.format.channels, r.format.rate, r.period_frames, r.buffer_frames
                    );
                    println!(
                        "콜백 {n}회, 처리 {:.0} 프레임/s(장치 레이트 {}), 간격 평균 {mean:.3}ms, p1 {:.3} / p50 {:.3} / p99 {:.3} / 최대 {:.3}ms{}",
                        rate,
                        r.format.rate,
                        pct(0.01),
                        pct(0.5),
                        pct(0.99),
                        pct(1.0),
                        r.error.as_deref().map(|e| format!(" | 오류: {e}")).unwrap_or_default()
                    );
                    println!(
                        "GetStreamLatency {:.2}ms | DATA_DISCONTINUITY {} | SILENT 패킷 {} | 일시 오류 {} | 재시작 {}",
                        r.stream_latency_ms, r.discontinuities, r.silent_packets, r.stream_errors, r.restarts
                    );
                    // 히스토그램: 주기 기준 0.25ms 칸
                    let bin = 250u32;
                    let mut hist: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
                    for &x in &r.intervals_us {
                        *hist.entry(x / bin).or_default() += 1;
                    }
                    let max = hist.values().copied().max().unwrap_or(1);
                    for (b, c) in hist {
                        let lo = b * bin;
                        let bar = "#".repeat(((c as f64 / max as f64) * 40.0).ceil() as usize);
                        println!("  {:>6.2}~{:<6.2}ms {:>6} {bar}", lo as f64 / 1000.0, (lo + bin) as f64 / 1000.0, c);
                    }
                }
            }
        }
    }

    engine_runs(seconds);
    peer_runs(seconds);
}

/// 엔진 전체(SystemBackend: 자체 WASAPI 우선, cpal 폴백)를 기본 장치로 열어 통계를 본다.
/// 모니터 꺼짐·피어 없음이라 출력은 0(무음)이다.
#[cfg(windows)]
fn engine_runs(seconds: f64) {
    use band_lib::engine::config::{CodecKind, DriverKind, EngineConfig};
    use band_lib::engine::device::system::SystemBackend;
    use band_lib::engine::mixer::MixControls;
    use band_lib::engine::Engine;
    use std::sync::Arc;
    use std::time::Duration;

    println!("\n## 엔진(SystemBackend, 모니터 끔 = 무음 출력, 128 / 2.5ms 요청)");
    for driver in [DriverKind::WasapiExclusive, DriverKind::WasapiShared] {
        let cfg = EngineConfig {
            input_driver: driver,
            output_driver: driver,
            buffer_size: 128,
            frame_ms: 2.5,
            codec: CodecKind::Opus,
            channels: 1,
            input_channels: vec![0],
            ..Default::default()
        };
        let controls = Arc::new(MixControls::default());
        match Engine::start(cfg, Arc::new(SystemBackend::new()), controls, None) {
            Err(e) => println!("- {driver:?}: 시작 실패 — {e}"),
            Ok(mut e) => {
                std::thread::sleep(Duration::from_secs_f64(seconds));
                let s = e.stats();
                let info = e.stream_info();
                e.stop();
                println!(
                    "- {driver:?}: backend {} | in {}Hz 주기 {} / out {}Hz 주기 {} 프레임 | devicePeriodMs {:.2} | estLatencyMs {:.2} | monitorLatencyMs {:.2} | xruns {} | streamErrors {} | deviceLost {} | streamFailed {} | 재시작 {} | inputDiscontinuities {} | SILENT {}",
                    s.backend,
                    info.in_rate,
                    info.in_period_frames,
                    info.out_rate,
                    info.out_period_frames,
                    s.device_period_ms,
                    s.est_latency_ms,
                    s.monitor_latency_ms,
                    s.xruns,
                    s.stream_errors,
                    s.device_lost,
                    s.stream_failed,
                    s.stream_restarts,
                    s.input_discontinuities,
                    s.input_silent_packets
                );
            }
        }
    }
}

/// 실장비 피어 경로(PR #11 M3): B = 실제 장치(SystemBackend), A = 가짜 장치(무음 입력) → 127.0.0.1 → B.
/// B는 피어를 뮤트하고 마스터 게인 0이라 출력은 무음이지만, 콜백 구동 믹스의 마감 경로(소스 링 선행량,
/// 마감 직전 은닉, 언더런 건너뛰기)는 실제 장치 콜백 타이밍으로 그대로 돈다.
#[cfg(windows)]
fn peer_runs(seconds: f64) {
    use band_lib::engine::config::{CodecKind, DriverKind, EngineConfig};
    use band_lib::engine::device::fake::FakeBackend;
    use band_lib::engine::device::system::SystemBackend;
    use band_lib::engine::mixer::MixControls;
    use band_lib::engine::Engine;
    use band_lib::net::peer::{Candidate, CandidateKind, LocalDescriptor};
    use band_lib::net::NetOptions;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let net = || NetOptions { port: 0, stun_servers: vec![], gather_lan: false, extra_candidates: vec![], allow_loopback_candidates: true, cancel: None };
    let addr = |e: &Engine| -> SocketAddr { format!("127.0.0.1:{}", e.local_port().unwrap_or(0)).parse().unwrap() };
    let with = |d: &LocalDescriptor, a: SocketAddr| LocalDescriptor { candidates: vec![Candidate { kind: CandidateKind::Lan, addr: a.to_string() }], ..d.clone() };
    println!("\n## 피어 경로(실제 장치 B ← 가짜 A 무음, 127.0.0.1, Opus 모노 2.5ms, 피어 뮤트 + 마스터 0 = 무음)");
    for driver in [DriverKind::WasapiExclusive, DriverKind::WasapiShared] {
        let cfg = EngineConfig {
            input_driver: driver,
            output_driver: driver,
            buffer_size: 128,
            frame_ms: 2.5,
            codec: CodecKind::Opus,
            channels: 1,
            input_channels: vec![0],
            ..Default::default()
        };
        let mut fake = FakeBackend::silence(48_000);
        fake.record_output = false;
        let a = match Engine::start(cfg.clone(), Arc::new(fake), Arc::new(MixControls::default()), Some(("a".into(), net()))) {
            Ok(e) => e,
            Err(e) => {
                println!("- {driver:?}: 가짜 송신 시작 실패 — {e}");
                continue;
            }
        };
        let controls = Arc::new(MixControls::default());
        controls.master_gain.store(0.0);
        let b = match Engine::start(cfg, Arc::new(SystemBackend::new()), controls, Some(("b".into(), net()))) {
            Ok(e) => e,
            Err(e) => {
                println!("- {driver:?}: 시작 실패 — {e}");
                continue;
            }
        };
        let (Some(da), Some(db)) = (a.descriptor().cloned(), b.descriptor().cloned()) else {
            println!("- {driver:?}: 디스크립터 없음");
            continue;
        };
        let _ = a.add_peer("b".into(), &with(&db, addr(&b)));
        let _ = b.add_peer("a".into(), &with(&da, addr(&a)));
        let _ = b.set_peer_gain("a", 0.0, true, None);
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(5) && b.stats().peers.first().is_none_or(|p| p.buffer_ms <= 0.0) {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(1000));
        let (s0, f0) = (b.stats(), b.feed_stats());
        let c0 = s0.peers.first().map(|p| p.concealed).unwrap_or(0);
        std::thread::sleep(Duration::from_secs_f64(seconds));
        let (s, f) = (b.stats(), b.feed_stats());
        let p = s.peers.first();
        println!(
            "- {driver:?}: backend {} | devicePeriodMs {:.2} | estLatencyMs {:.2} | {seconds:.0}초: 은닉 {} | 마감 채움 {} | 건너뜀 {} 프레임 | xruns {} | bufferMs {:.2} | playoutMs {:.2} | inputDiscontinuities {} | 재시작 {} | streamErrors {}",
            s.backend,
            s.device_period_ms,
            s.est_latency_ms,
            p.map(|p| p.concealed).unwrap_or(0) - c0,
            f.0 - f0.0,
            f.1 - f0.1,
            s.xruns - s0.xruns,
            p.map(|p| p.buffer_ms).unwrap_or(0.0),
            p.map(|p| p.playout_ms).unwrap_or(0.0),
            s.input_discontinuities,
            s.stream_restarts,
            s.stream_errors
        );
        drop(b);
        drop(a);
    }
}

#[cfg(not(windows))]
fn main() {
    println!("WASAPI 프로브는 Windows 전용입니다.");
}
