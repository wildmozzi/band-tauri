//! STUN 질의의 벽시계 성질(P3-0a: 단위 테스트에서 옮김).
//!
//! 단위 테스트의 `#[ignore]`는 known-bug 재현용이라 CI `realtime` 잡(`kind(test)` 필터)에 잡히지 않는다.
//! 그래서 벽시계 임계가 있는 검사는 통합 테스트로 두고 `#[ignore = "realtime: ..."]`로 realtime 잡에서 돈다.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use band_lib::net::stun::query_many;

/// `txn`에 대한 XOR-MAPPED-ADDRESS 응답(203.0.113.5:40000).
fn response(txn: &[u8]) -> Vec<u8> {
    let mut r = vec![0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42];
    r.extend_from_slice(txn);
    r.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
    r.extend_from_slice(&(40000u16 ^ 0x2112).to_be_bytes());
    r.extend_from_slice(&[203u8 ^ 0x21, 0x12, 113 ^ 0xa4, 5 ^ 0x42]);
    r
}

/// 응답 없는 서버가 섞여 있어도 서버마다 timeout을 직렬로 기다리지 않는다(전체 마감 하나).
#[test]
#[ignore = "realtime: 결정적 짝 net::stun::tests::query_many_parallel_with_source_check(병렬 송신·출발지 검사)"]
fn query_many_does_not_wait_serially() {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let srv = UdpSocket::bind("127.0.0.1:0").unwrap();
    let dead = UdpSocket::bind("127.0.0.1:0").unwrap();
    let servers = vec![dead.local_addr().unwrap().to_string(), srv.local_addr().unwrap().to_string()];
    let t = std::thread::spawn(move || {
        let mut b = [0u8; 64];
        let (n, from) = srv.recv_from(&mut b).unwrap();
        assert_eq!(n, 20);
        srv.send_to(&response(&b[8..20]), from).unwrap();
    });
    let t0 = Instant::now();
    let got = query_many(&client, &servers, Duration::from_millis(600), None);
    let took = t0.elapsed();
    t.join().unwrap();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].1, "203.0.113.5:40000".parse().unwrap());
    // 직렬이면 2 × 600ms. 전체 마감 하나면 약 600ms.
    assert!(took < Duration::from_millis(900), "직렬 대기 없음: {took:?}");
}
