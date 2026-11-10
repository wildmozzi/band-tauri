//! STUN Binding (RFC 5389) — 기존 `network.rs`의 `query_stun` 파서를 이식하고
//! 트랜잭션 ID 검증과 비STUN 패킷 무시를 추가했다.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

pub const DEFAULT_STUN_SERVERS: [&str; 2] = ["stun.l.google.com:19302", "stun.cloudflare.com:3478"];

const MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xa4, 0x42];

pub fn build_request(txn_id: &[u8; 12]) -> [u8; 20] {
    let mut r = [0u8; 20];
    r[0..2].copy_from_slice(&[0x00, 0x01]); // Binding Request
    r[4..8].copy_from_slice(&MAGIC_COOKIE);
    r[8..20].copy_from_slice(txn_id);
    r
}

/// Binding Success Response에서 (XOR-)MAPPED-ADDRESS를 꺼낸다. IPv4만.
pub fn parse_response(buf: &[u8], txn_id: &[u8; 12]) -> Option<SocketAddr> {
    if buf.len() < 20 || buf[0] != 0x01 || buf[1] != 0x01 {
        return None;
    }
    if buf[4..8] != MAGIC_COOKIE || &buf[8..20] != txn_id {
        return None;
    }
    let msg_len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    let end = (20 + msg_len).min(buf.len());
    let mut pos = 20;
    let mut mapped = None;
    while pos + 4 <= end {
        let attr_type = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let attr_len = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) as usize;
        pos += 4;
        if pos + attr_len > end {
            break;
        }
        if (attr_type == 0x0020 || attr_type == 0x0001) && attr_len >= 8 && buf[pos + 1] == 0x01 {
            let a = if attr_type == 0x0020 {
                let port = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) ^ 0x2112;
                let ip = [
                    buf[pos + 4] ^ MAGIC_COOKIE[0],
                    buf[pos + 5] ^ MAGIC_COOKIE[1],
                    buf[pos + 6] ^ MAGIC_COOKIE[2],
                    buf[pos + 7] ^ MAGIC_COOKIE[3],
                ];
                SocketAddr::from((ip, port))
            } else {
                let port = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]);
                SocketAddr::from(([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]], port))
            };
            // XOR-MAPPED-ADDRESS 우선
            if attr_type == 0x0020 {
                return Some(a);
            }
            mapped = Some(a);
        }
        pos += (attr_len + 3) & !3;
    }
    mapped
}

/// 여러 STUN 서버에 동시에 질의한다(리뷰 L7). 응답은 트랜잭션 ID와 출발지(서버 주소)를
/// 모두 확인한다. 전체 대기는 `timeout` 한 번이다. 수신 스레드 시작 전에만 호출한다.
pub fn query_many(
    socket: &UdpSocket,
    servers: &[String],
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Vec<(String, SocketAddr)> {
    let started = Instant::now();
    let deadline = started + timeout;
    // DNS 해석도 서버별 스레드로 동시에, 전체 timeout 안에서만 기다린다(PR #5 L-5: 오프라인에서
    // 직렬 DNS 타임아웃만큼 시작이 늘어나지 않게). 늦게 끝난 해석 스레드는 결과를 버리고 스스로 끝난다.
    let (tx, rx) = std::sync::mpsc::channel::<(String, Option<SocketAddr>)>();
    for s in servers {
        let (tx, s) = (tx.clone(), s.clone());
        let spawned = std::thread::Builder::new().name("band-stun-dns".into()).spawn(move || {
            let addr = s.to_socket_addrs().ok().and_then(|mut it| it.find(|a| a.is_ipv4()));
            let _ = tx.send((s, addr));
        });
        if spawned.is_err() {
            log::warn!("STUN DNS 스레드 생성 실패");
        }
    }
    drop(tx);
    let mut pending: Vec<(String, SocketAddr, [u8; 12])> = Vec::new();
    for _ in 0..servers.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((s, Some(addr))) => {
                let txn: [u8; 12] = rand::random();
                if socket.send_to(&build_request(&txn), addr).is_ok() {
                    pending.push((s, addr, txn));
                }
            }
            Ok((s, None)) => log::warn!("STUN 서버 {s} 이름 해석 실패"),
            Err(_) => break,
        }
    }
    let mut found = Vec::new();
    let resend_at = started + timeout / 2;
    let mut resent = false;
    let mut buf = [0u8; 512];
    while !pending.is_empty() {
        let now = Instant::now();
        if now >= deadline || cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst)) {
            break;
        }
        if !resent && now >= resend_at {
            resent = true;
            for (_, addr, txn) in &pending {
                let _ = socket.send_to(&build_request(txn), *addr);
            }
        }
        let left = deadline.saturating_duration_since(now).min(Duration::from_millis(100));
        if socket.set_read_timeout(Some(left.max(Duration::from_millis(1)))).is_err() {
            break;
        }
        if let Ok((n, src)) = socket.recv_from(&mut buf) {
            if let Some(i) = pending.iter().position(|(_, addr, txn)| *addr == src && parse_response(&buf[..n], txn).is_some()) {
                let (name, _, txn) = pending.swap_remove(i);
                if let Some(a) = parse_response(&buf[..n], &txn) {
                    found.push((name, a));
                }
            }
        }
    }
    found
}

/// 소켓으로 STUN 서버에 질의. 수신 스레드 시작 전에만 호출한다(소켓 타임아웃을 바꾼다).
pub fn query(socket: &UdpSocket, server: &str, timeout: Duration) -> Option<SocketAddr> {
    let server_addr = server.to_socket_addrs().ok()?.find(|a| a.is_ipv4())?;
    let txn_id: [u8; 12] = rand::random();
    let req = build_request(&txn_id);
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 512];
    for _attempt in 0..2 {
        socket.send_to(&req, server_addr).ok()?;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            socket.set_read_timeout(Some(left.min(Duration::from_millis(500)))).ok()?;
            match socket.recv_from(&mut buf) {
                Ok((n, _)) => {
                    if let Some(a) = parse_response(&buf[..n], &txn_id) {
                        return Some(a);
                    }
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => break,
                Err(_) => continue, // Windows WSAECONNRESET 등
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xor_mapped_address() {
        let txn = [7u8; 12];
        let mut r = vec![0x01, 0x01, 0x00, 0x0c];
        r.extend_from_slice(&MAGIC_COOKIE);
        r.extend_from_slice(&txn);
        // XOR-MAPPED-ADDRESS 203.0.113.5:40000
        let port = 40000u16 ^ 0x2112;
        let ip = [203u8 ^ 0x21, 0x12, 113 ^ 0xa4, 5 ^ 0x42];
        r.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
        r.extend_from_slice(&port.to_be_bytes());
        r.extend_from_slice(&ip);
        assert_eq!(parse_response(&r, &txn), Some("203.0.113.5:40000".parse().unwrap()));
        // 다른 트랜잭션 → 거부
        assert_eq!(parse_response(&r, &[8u8; 12]), None);
        // 잘린 패킷
        assert_eq!(parse_response(&r[..25], &txn), None);
    }

    #[test]
    fn query_many_parallel_with_source_check() {
        // 로컬 가짜 STUN 서버 2개(하나는 응답 없음) + 다른 주소에서 보낸 위조 응답
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        let srv = UdpSocket::bind("127.0.0.1:0").unwrap();
        let dead = UdpSocket::bind("127.0.0.1:0").unwrap();
        let forger = UdpSocket::bind("127.0.0.1:0").unwrap();
        let srv_addr = srv.local_addr().unwrap();
        let client_addr = client.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut b = [0u8; 64];
            let (n, from) = srv.recv_from(&mut b).unwrap();
            assert_eq!(n, 20);
            let mut txn = [0u8; 12];
            txn.copy_from_slice(&b[8..20]);
            let mut r = vec![0x01, 0x01, 0x00, 0x0c];
            r.extend_from_slice(&MAGIC_COOKIE);
            r.extend_from_slice(&txn);
            let port = 40000u16 ^ 0x2112;
            r.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
            r.extend_from_slice(&port.to_be_bytes());
            r.extend_from_slice(&[203u8 ^ 0x21, 0x12, 113 ^ 0xa4, 5 ^ 0x42]);
            // 같은 내용을 다른 주소(forger)에서 먼저 보낸다 → 출발지 불일치로 무시되어야 함
            forger.send_to(&r, from).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            srv.send_to(&r, from).unwrap();
        });
        let servers = vec![srv_addr.to_string(), dead.local_addr().unwrap().to_string()];
        let t0 = Instant::now();
        let got = query_many(&client, &servers, Duration::from_millis(600), None);
        t.join().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, "203.0.113.5:40000".parse().unwrap());
        assert!(t0.elapsed() < Duration::from_millis(900), "직렬 대기 없음");
        let _ = client_addr;
    }

    #[test]
    fn request_layout() {
        let r = build_request(&[1u8; 12]);
        assert_eq!(&r[0..8], &[0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42]);
    }
}
