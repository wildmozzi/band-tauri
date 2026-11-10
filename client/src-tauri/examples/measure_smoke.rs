//! 지연 측정 스모크(P1-1): 이 PC의 실제 기본 장치로 테스트 엔진을 열고 하드웨어 루프백 측정을 돌린다.
//!
//! 실행: `cargo run --example measure_smoke [드라이버=wasapiShared|wasapiExclusive|asio] [반복=5] [버퍼=128]`
//!
//! 루프백 케이블이 없으면 "신호 미검출"(noSignal)이 나와야 정상이다. 스피커와 마이크가 가까우면 공기 중 경로로
//! 검출될 수도 있다(그 값은 스피커 → 마이크 왕복). **출력에서 짧은 처프 소리가 난다.**

use std::sync::Arc;
use std::time::Duration;

use band_lib::engine::config::{DriverKind, EngineConfig};
use band_lib::engine::device::system::SystemBackend;
use band_lib::manager::EngineManager;
use band_lib::net::NetOptions;

fn main() {
    let mut args = std::env::args().skip(1);
    let driver = match args.next().as_deref() {
        Some("wasapiExclusive") => DriverKind::WasapiExclusive,
        Some("asio") => DriverKind::Asio,
        _ => DriverKind::WasapiShared,
    };
    let runs: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let buffer: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(128);
    let cfg = EngineConfig { input_driver: driver, output_driver: driver, buffer_size: buffer, frame_ms: 2.5, ..Default::default() };
    let m = EngineManager::new(Arc::new(SystemBackend::new()), NetOptions::default());
    println!("# 지연 측정 스모크: {driver:?}, 버퍼 {buffer}, 반복 {runs}");
    if let Err(e) = m.test_start(cfg) {
        println!("테스트 엔진 시작 실패: {e}");
        return;
    }
    std::thread::sleep(Duration::from_millis(800));
    let s = m.stats();
    println!("백엔드 {} {}Hz 장치 버퍼 {} estLatencyMs {:.2}", s.backend, s.sample_rate, s.device_buffer, s.est_latency_ms);
    match m.measure_loopback(runs) {
        Ok(r) => {
            println!("판정: {:?} — {}", r.verdict, r.message);
            for (i, run) in r.runs.iter().enumerate() {
                println!(
                    "  {}. 왕복 {:?} 피크비 {:.1} 상관 {:.3} 입력 최대 {:.4} 클리핑 {}",
                    i + 1,
                    run.round_trip_ms.map(|v| (v * 1000.0).round() / 1000.0),
                    run.peak_ratio,
                    run.correlation,
                    run.input_peak,
                    run.clipped
                );
            }
            println!(
                "장치 버퍼 왕복 {:.2}ms, 드라이버 보고 {:?}/{:?}ms, est {:.2}ms, 보정 {:?}",
                r.buffer_round_trip_ms, r.driver_input_latency_ms, r.driver_output_latency_ms, r.est_latency_ms, r.offset_ms
            );
            println!("JSON: {}", serde_json::to_string(&r).unwrap_or_default());
        }
        Err(e) => println!("측정 실패: {e}"),
    }
    m.stop();
}
