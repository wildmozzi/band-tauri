//! 릴레이 패킷 포맷 (docs/overhaul/relay-design.md 2절). 빅엔디안.
//!
//! 엔진 패킷(설계 2절)은 `magic "BN" | version 1 | type | ssrc | seq | timestamp | flags | frame` 18바이트 헤더 +
//! 암호문 + 16B 태그다. 릴레이는 같은 magic/version에 새 type 3개를 쓴다.
//!
//! - `RelayBind`(7): 엔진 헤더 18B(ssrc=토큰 ssrc, seq/timestamp는 클라이언트 임의, flags=0, frame=0) + 토큰
//! - `RelayData`(8): `magic(2) | version(1) | type=8 | dest_ssrc(4)` 8B + 원본 엔진 패킷 전체
//! - `RelayBindAck`(9): 엔진 헤더 18B(ssrc/seq/timestamp는 Bind 그대로, flags=상태) + 본문 16B

use std::net::SocketAddr;

pub const MAGIC: [u8; 2] = [0x42, 0x4E];
pub const VERSION: u8 = 1;
pub const HEADER_BYTES: usize = 18;
pub const TAG_BYTES: usize = 16;

pub const TYPE_RELAY_BIND: u8 = 7;
pub const TYPE_RELAY_DATA: u8 = 8;
pub const TYPE_RELAY_BIND_ACK: u8 = 9;

/// RelayData 접두부 길이
pub const DATA_PREFIX_BYTES: usize = 8;
/// 중계할 수 있는 엔진 패킷 타입 (Audio..Bye)
pub const INNER_TYPE_MIN: u8 = 1;
pub const INNER_TYPE_MAX: u8 = 6;
/// 엔진 패킷 최소 길이 (헤더 + 태그)
pub const INNER_MIN_BYTES: usize = HEADER_BYTES + TAG_BYTES;
/// 엔진 패킷 최대 길이 (설계 1.3: 페이로드 ≤1200B, 여유 포함)
pub const INNER_MAX_BYTES: usize = 1280;
pub const ACK_BODY_BYTES: usize = 16;
pub const ACK_BYTES: usize = HEADER_BYTES + ACK_BODY_BYTES;

/// RelayBindAck flags (상태)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BindStatus {
    Ok = 0,
    RoomFull = 1,
    SsrcConflict = 2,
}

#[inline]
pub fn has_magic(buf: &[u8]) -> bool {
    buf.len() >= 4 && buf[0..2] == MAGIC && buf[2] == VERSION
}

#[inline]
pub fn be32(buf: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]])
}

/// RelayData 생성 (클라이언트/테스트용)
pub fn write_relay_data(dest_ssrc: u32, inner: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(TYPE_RELAY_DATA);
    out.extend_from_slice(&dest_ssrc.to_be_bytes());
    out.extend_from_slice(inner);
}

/// RelayBind 생성 (클라이언트/테스트용)
pub fn write_relay_bind(ssrc: u32, seq: u32, timestamp: u32, token: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(TYPE_RELAY_BIND);
    out.extend_from_slice(&ssrc.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&timestamp.to_be_bytes());
    out.push(0);
    out.push(0);
    out.extend_from_slice(token);
}

/// RelayBindAck 작성. `bind`는 검증된 RelayBind 헤더(앞 18B).
///
/// 본문: `ttl_secs(u16) | family(u8: 4=IPv4, 0=기타) | 0 | port(u16) | ipv4(4) | 0(6)` — 릴레이가 본 출발 주소.
pub fn write_bind_ack(bind: &[u8], status: BindStatus, ttl_secs: u16, observed: SocketAddr, out: &mut [u8; ACK_BYTES]) {
    out[..HEADER_BYTES].copy_from_slice(&bind[..HEADER_BYTES]);
    out[3] = TYPE_RELAY_BIND_ACK;
    out[16] = status as u8;
    out[17] = 0;
    let body = &mut out[HEADER_BYTES..];
    body.fill(0);
    body[0..2].copy_from_slice(&ttl_secs.to_be_bytes());
    body[4..6].copy_from_slice(&observed.port().to_be_bytes());
    if let SocketAddr::V4(v4) = observed {
        body[2] = 4;
        body[6..10].copy_from_slice(&v4.ip().octets());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_data_layout() {
        let mut out = Vec::new();
        write_relay_data(0xAABBCCDD, &[1, 2, 3], &mut out);
        assert_eq!(out, [0x42, 0x4E, 1, 8, 0xAA, 0xBB, 0xCC, 0xDD, 1, 2, 3]);
    }

    #[test]
    fn bind_ack_layout() {
        let mut bind = Vec::new();
        write_relay_bind(0x01020304, 7, 0x0A0B0C0D, &[0xEE; 49], &mut bind);
        assert_eq!(&bind[..4], &[0x42, 0x4E, 1, 7]);
        let mut ack = [0u8; ACK_BYTES];
        write_bind_ack(&bind, BindStatus::RoomFull, 30, "1.2.3.4:5678".parse().unwrap(), &mut ack);
        assert_eq!(&ack[..4], &[0x42, 0x4E, 1, 9]);
        assert_eq!(&ack[4..16], &bind[4..16], "ssrc/seq/timestamp 에코");
        assert_eq!(ack[16], 1);
        assert_eq!(&ack[18..], &[0, 30, 4, 0, 0x16, 0x2E, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0]);
        assert!(ACK_BYTES < bind.len(), "응답이 요청보다 작아야 한다(증폭 없음)");
    }
}
