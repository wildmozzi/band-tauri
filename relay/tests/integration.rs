//! 통합 테스트: 실제 UDP 소켓으로 A → 릴레이 → B 왕복, 미인증 폐기, 메트릭 엔드포인트.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use band_relay::packet::{self, ack_cookie, write_relay_bind, write_relay_data, ACK_BYTES, TYPE_RELAY_BIND_ACK};
use band_relay::token::{self, Claims};
use band_relay::{server, Limits, Relay, Stats};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::timeout;

const SECRET: &[u8] = b"integration-secret-0123456789abcdef";

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn engine_pkt(ssrc: u32, seq: u32, payload: usize) -> Vec<u8> {
    let mut p = vec![0xA5u8; packet::HEADER_BYTES + payload + packet::TAG_BYTES];
    p[0..4].copy_from_slice(&[0x42, 0x4E, 1, 1]);
    p[4..8].copy_from_slice(&ssrc.to_be_bytes());
    p[8..12].copy_from_slice(&seq.to_be_bytes());
    p
}

async fn send_bind(sock: &UdpSocket, relay: std::net::SocketAddr, t: &[u8], ssrc: u32, cookie: Option<&[u8; 16]>) -> [u8; ACK_BYTES] {
    let mut b = Vec::new();
    write_relay_bind(ssrc, 1, 0xCAFE, t, cookie, &mut b);
    sock.send_to(&b, relay).await.unwrap();
    let mut buf = [0u8; 64];
    let (n, from) = timeout(Duration::from_secs(2), sock.recv_from(&mut buf)).await.expect("ack 타임아웃").unwrap();
    assert_eq!(from, relay);
    assert_eq!(n, ACK_BYTES);
    assert!(n < b.len(), "Ack가 요청보다 작아야 한다");
    let mut ack = [0u8; ACK_BYTES];
    ack.copy_from_slice(&buf[..n]);
    ack
}

/// RelayBind → CookieRequired → 쿠키 포함 RelayBind → Ok
async fn bind_client(sock: &UdpSocket, relay: std::net::SocketAddr, room: &str, client: &str, ssrc: u32) -> [u8; ACK_BYTES] {
    let t = token::sign(SECRET, &Claims { expires_at: unix_now() + 600, ssrc, room, client });
    let first = send_bind(sock, relay, &t, ssrc, None).await;
    assert_eq!(first[16], 3, "CookieRequired");
    send_bind(sock, relay, &t, ssrc, Some(&ack_cookie(&first))).await
}

async fn start_relay(limits: Limits) -> (std::net::SocketAddr, Arc<Stats>) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    let stats = Arc::new(Stats::default());
    let relay = Relay::new(SECRET.to_vec(), limits, stats.clone());
    tokio::spawn(server::run_udp(sock, relay));
    (addr, stats)
}

#[tokio::test]
async fn roundtrip_through_relay_with_latency() {
    let (relay, stats) = start_relay(Limits::default()).await;
    let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    let ack = bind_client(&a, relay, "room", "client-a", 0xA).await;
    assert_eq!(ack[3], TYPE_RELAY_BIND_ACK);
    assert_eq!(ack[16], 0, "Ok");
    assert_eq!(&ack[12..16], &0xCAFEu32.to_be_bytes(), "timestamp 에코");
    assert_eq!(u16::from_be_bytes([ack[22], ack[23]]), a.local_addr().unwrap().port(), "관측 주소");
    bind_client(&b, relay, "room", "client-b", 0xB).await;

    // A → 릴레이 → B → 릴레이 → A 왕복 N회
    const N: usize = 300;
    let mut rtts = Vec::with_capacity(N);
    let mut out = Vec::new();
    let mut buf = [0u8; 2048];
    for seq in 0..N as u32 {
        let pkt = engine_pkt(0xA, seq, 60);
        write_relay_data(0xB, &pkt, &mut out);
        let t0 = Instant::now();
        a.send_to(&out, relay).await.unwrap();
        let (n, from) = timeout(Duration::from_secs(2), b.recv_from(&mut buf)).await.expect("B 수신 타임아웃").unwrap();
        assert_eq!(from, relay);
        assert_eq!(&buf[..n], &pkt[..], "원본 패킷이 그대로 전달");
        // B가 응답
        let reply = engine_pkt(0xB, seq, 60);
        write_relay_data(0xA, &reply, &mut out);
        b.send_to(&out, relay).await.unwrap();
        let (n, _) = timeout(Duration::from_secs(2), a.recv_from(&mut buf)).await.expect("A 수신 타임아웃").unwrap();
        assert_eq!(&buf[..n], &reply[..]);
        rtts.push(t0.elapsed());
    }
    rtts.sort();
    let p50 = rtts[N / 2];
    let p99 = rtts[N * 99 / 100];
    let relay_p50 = stats.latency_quantile_us(0.5).unwrap();
    let relay_p99 = stats.latency_quantile_us(0.99).unwrap();
    println!(
        "loopback RTT via relay (2 hops): p50={:?} p99={:?} max={:?}; relay processing p50<={relay_p50}us p99<={relay_p99}us",
        p50,
        p99,
        rtts[N - 1]
    );
    assert!(p99 < Duration::from_millis(50), "p99 RTT {p99:?}");
    assert_eq!(stats.forwarded_packets.load(std::sync::atomic::Ordering::Relaxed), 2 * N as u64);
}

#[tokio::test]
async fn unauthenticated_sender_gets_nothing() {
    let (relay, stats) = start_relay(Limits::default()).await;
    let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    bind_client(&a, relay, "room", "client-a", 0xA).await;
    bind_client(&b, relay, "other-room", "client-b", 0xB).await;

    let mut out = Vec::new();
    // C(미등록)가 A의 ssrc를 사칭해 B에게
    write_relay_data(0xB, &engine_pkt(0xA, 1, 60), &mut out);
    c.send_to(&out, relay).await.unwrap();
    // C가 위조 토큰으로 Bind
    let t = token::sign(b"not-the-secret-not-the-secret-not-the", &Claims { expires_at: unix_now() + 600, ssrc: 0xC, room: "room", client: "c" });
    let mut bind = Vec::new();
    write_relay_bind(0xC, 1, 1, &t, None, &mut bind);
    c.send_to(&bind, relay).await.unwrap();
    // A(다른 방)가 B에게
    write_relay_data(0xB, &engine_pkt(0xA, 2, 60), &mut out);
    a.send_to(&out, relay).await.unwrap();

    let mut buf = [0u8; 2048];
    assert!(timeout(Duration::from_millis(300), c.recv_from(&mut buf)).await.is_err(), "C에게 응답이 없어야 한다");
    assert!(timeout(Duration::from_millis(100), b.recv_from(&mut buf)).await.is_err(), "B에게 전달되지 않아야 한다");
    use band_relay::DropReason::*;
    assert_eq!(stats.drops(Unregistered), 1);
    assert_eq!(stats.drops(BadToken), 1);
    assert_eq!(stats.drops(UnknownDest), 1);
}

#[tokio::test]
async fn bandwidth_cap_over_socket() {
    // 클라이언트당 80kbit/s(10KB/s), 버킷 0.2초 = 2KB → 1KB 패킷 연속 10개 중 2개만 통과
    let limits = Limits { max_bits_per_sec: 80_000, ..Limits::default() };
    let (relay, stats) = start_relay(limits).await;
    let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    bind_client(&a, relay, "room", "client-a", 0xA).await;
    bind_client(&b, relay, "room", "client-b", 0xB).await;
    let mut out = Vec::new();
    write_relay_data(0xB, &engine_pkt(0xA, 0, 958), &mut out);
    assert_eq!(out.len(), 1000);
    for _ in 0..10 {
        a.send_to(&out, relay).await.unwrap();
    }
    let mut buf = [0u8; 2048];
    let mut got = 0;
    while timeout(Duration::from_millis(200), b.recv_from(&mut buf)).await.is_ok() {
        got += 1;
    }
    assert_eq!(got, 2);
    assert_eq!(stats.drops(band_relay::DropReason::RateLimited), 8);
}

#[tokio::test]
async fn metrics_endpoint() {
    let stats = Arc::new(Stats::default());
    stats.record_latency(12_000);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server::serve_metrics(listener, stats));
    for (path, expect) in [("/metrics", "relay_processing_seconds_count 1"), ("/healthz", "ok"), ("/nope", "404")] {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes()).await.unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).await.unwrap();
        assert!(resp.contains(expect), "{path}: {resp}");
    }
}

/// H1: 캡처한 Bind(쿠키 포함)를 다른 소켓에서 리플레이해도 등록이 옮겨지지 않는다
#[tokio::test]
async fn replay_from_other_address_does_not_hijack() {
    let (relay, _stats) = start_relay(Limits::default()).await;
    let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let x = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let t = token::sign(SECRET, &Claims { expires_at: unix_now() + 600, ssrc: 0xA, room: "room", client: "client-a" });
    let first = send_bind(&a, relay, &t, 0xA, None).await;
    let cookie = ack_cookie(&first);
    assert_eq!(send_bind(&a, relay, &t, 0xA, Some(&cookie)).await[16], 0);
    bind_client(&b, relay, "room", "client-b", 0xB).await;

    // 공격자 X가 A의 Bind(쿠키 포함)를 그대로 재전송 → 쿠키 요구만 받음
    assert_eq!(send_bind(&x, relay, &t, 0xA, Some(&cookie)).await[16], 3);
    // B → A 데이터는 여전히 A로
    let mut out = Vec::new();
    let pkt = engine_pkt(0xB, 1, 60);
    write_relay_data(0xA, &pkt, &mut out);
    b.send_to(&out, relay).await.unwrap();
    let mut buf = [0u8; 2048];
    let (n, _) = timeout(Duration::from_secs(2), a.recv_from(&mut buf)).await.expect("A 수신").unwrap();
    assert_eq!(&buf[..n], &pkt[..]);
    assert!(timeout(Duration::from_millis(200), x.recv_from(&mut buf)).await.is_err(), "X는 오디오를 받지 않는다");
}
