//! 수신 스레드 정지 회귀 테스트(P3-0d).
//!
//! 수신 소켓 읽기 타임아웃을 20ms에서 1초로 늘렸다(Windows에서 `SO_RCVTIMEO` 만료 순간 도착한 데이터그램이
//! 사라져, 패킷 간격이 타임아웃과 겹치는 장치 버퍼 1024에서 깨끗한 루프백도 손실이 났다). 정지는 타임아웃이
//! 아니라 자기 주소로 보내는 깨움 데이터그램으로 알리므로, 타임아웃보다 훨씬 빨리 끝나야 한다.

use std::time::{Duration, Instant};

use band_lib::engine::jitter::JitterSettings;
use band_lib::sys::prio::ThreadPriority;
use band_lib::net::{Net, NetOptions};

#[test]
fn stop_wakes_receiver_without_waiting_for_read_timeout() {
    let opts = NetOptions {
        port: 0,
        stun_servers: vec![],
        gather_lan: false,
        extra_candidates: vec![],
        allow_loopback_candidates: true,
        cancel: None,
    };
    for _ in 0..3 {
        let mut net = Net::start("alice".into(), &opts, JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 }).unwrap();
        // 수신 스레드가 시작해 우선순위를 적은 뒤(곧바로 recv에 들어간다) 정지한다. 시작 전에 정지하면 플래그만
        // 보고 끝나 깨움을 검사하지 못한다. 상한 20초는 행 감지용이다.
        let t0 = Instant::now();
        while net.shared().recv_prio.load() == ThreadPriority::Unmanaged {
            assert!(t0.elapsed() < Duration::from_secs(20), "수신 스레드가 시작하지 않음");
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(50));
        let t0 = Instant::now();
        net.stop();
        // 깨움이 없으면 수신 스레드가 타임아웃(1초)까지 막힌다. 판별선은 그 절반(느린 러너 여유 포함).
        let took = t0.elapsed();
        assert!(took < Duration::from_millis(500), "정지가 수신 타임아웃을 기다림: {took:?}");
    }
}
