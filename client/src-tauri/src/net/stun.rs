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
    fn request_layout() {
        let r = build_request(&[1u8; 12]);
        assert_eq!(&r[0..8], &[0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42]);
    }
}
