//! 패킷 헤더 직렬화/역직렬화 (설계 문서 2절). 빅엔디안.
//!
//! ```text
//! off len 필드
//! 0   2   magic "BN"
//! 2   1   version = 1
//! 3   1   type
//! 4   4   sender_ssrc
//! 8   4   seq
//! 12  4   timestamp
//! 16  1   flags: Audio bit0=opus, bit1=stereo / Hello·HelloAck bit7=경로 태그 있음, bit6=릴레이 경유
//! 17  1   frame_samples / 120
//! 18  ..  암호문 + 16B 태그
//! ```

use crate::engine::config::{HEADER_BYTES, TAG_BYTES};

pub const MAGIC: [u8; 2] = [0x42, 0x4E];
pub const VERSION: u8 = 1;

pub const FLAG_OPUS: u8 = 0b01;
pub const FLAG_STEREO: u8 = 0b10;
/// Hello/HelloAck: 보낸 쪽이 경로 종류를 표시했다(새 엔진). 헤더는 AAD라 경로상 공격자가 바꿀 수 없다.
pub const FLAG_PATH_TAGGED: u8 = 0x80;
/// Hello/HelloAck: 릴레이(RelayData)로 보냈다. 태그가 있으면 수신 경로 종류(출발 주소 = 릴레이 주소)와 같아야 한다.
pub const FLAG_VIA_RELAY: u8 = 0x40;

/// Hello/HelloAck 헤더 flags (relay-design 4.1 경로 종류 위장 대응).
pub fn path_tag(via_relay: bool) -> u8 {
    FLAG_PATH_TAGGED | if via_relay { FLAG_VIA_RELAY } else { 0 }
}

/// 받은 Hello/HelloAck의 경로 태그가 실제 수신 경로와 맞는가. 태그 없는 패킷(옛 엔진)은 출발 주소만으로 판단한다.
pub fn path_tag_matches(flags: u8, via_relay: bool) -> bool {
    flags & FLAG_PATH_TAGGED == 0 || (flags & FLAG_VIA_RELAY != 0) == via_relay
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Audio = 1,
    Hello = 2,
    HelloAck = 3,
    Ping = 4,
    Pong = 5,
    Bye = 6,
}

impl PacketType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Audio,
            2 => Self::Hello,
            3 => Self::HelloAck,
            4 => Self::Ping,
            5 => Self::Pong,
            6 => Self::Bye,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub ptype: PacketType,
    pub ssrc: u32,
    pub seq: u32,
    pub timestamp: u32,
    pub flags: u8,
    pub frame_code: u8,
}

impl Header {
    pub fn write(&self, out: &mut [u8; HEADER_BYTES]) {
        out[0..2].copy_from_slice(&MAGIC);
        out[2] = VERSION;
        out[3] = self.ptype as u8;
        out[4..8].copy_from_slice(&self.ssrc.to_be_bytes());
        out[8..12].copy_from_slice(&self.seq.to_be_bytes());
        out[12..16].copy_from_slice(&self.timestamp.to_be_bytes());
        out[16] = self.flags;
        out[17] = self.frame_code;
    }

    /// 헤더 파싱. 길이(헤더+태그 이상), magic, version, type을 검사한다.
    pub fn parse(buf: &[u8]) -> Option<Header> {
        if buf.len() < HEADER_BYTES + TAG_BYTES {
            return None;
        }
        if buf[0..2] != MAGIC || buf[2] != VERSION {
            return None;
        }
        let ptype = PacketType::from_u8(buf[3])?;
        let be = |i: usize| u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
        Some(Header {
            ptype,
            ssrc: be(4),
            seq: be(8),
            timestamp: be(12),
            flags: buf[16],
            frame_code: buf[17],
        })
    }

    pub fn is_opus(&self) -> bool {
        self.flags & FLAG_OPUS != 0
    }

    pub fn is_stereo(&self) -> bool {
        self.flags & FLAG_STEREO != 0
    }

    /// AEAD nonce = ssrc(4) || type(1) || 0(3) || seq(4)
    pub fn nonce(&self) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[0..4].copy_from_slice(&self.ssrc.to_be_bytes());
        n[4] = self.ptype as u8;
        n[8..12].copy_from_slice(&self.seq.to_be_bytes());
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Header {
        Header {
            ptype: PacketType::Audio,
            ssrc: 0xDEADBEEF,
            seq: 0x01020304,
            timestamp: 0xA0B0C0D0,
            flags: FLAG_OPUS | FLAG_STEREO,
            frame_code: 2,
        }
    }

    #[test]
    fn roundtrip() {
        let h = sample();
        let mut buf = [0u8; HEADER_BYTES + TAG_BYTES];
        let mut hb = [0u8; HEADER_BYTES];
        h.write(&mut hb);
        buf[..HEADER_BYTES].copy_from_slice(&hb);
        assert_eq!(&buf[0..4], &[0x42, 0x4E, 1, 1]);
        assert_eq!(&buf[4..8], &[0xDE, 0xAD, 0xBE, 0xEF]);
        let p = Header::parse(&buf).unwrap();
        assert_eq!(p, h);
        assert!(p.is_opus() && p.is_stereo());
    }

    #[test]
    fn rejects_garbage() {
        let mut hb = [0u8; HEADER_BYTES];
        sample().write(&mut hb);
        let mut buf = vec![0u8; HEADER_BYTES + TAG_BYTES];
        buf[..HEADER_BYTES].copy_from_slice(&hb);
        assert!(Header::parse(&buf[..HEADER_BYTES + TAG_BYTES - 1]).is_none(), "짧은 패킷");
        let mut b = buf.clone();
        b[0] = b'X';
        assert!(Header::parse(&b).is_none(), "magic");
        let mut b = buf.clone();
        b[2] = 2;
        assert!(Header::parse(&b).is_none(), "version");
        let mut b = buf.clone();
        b[3] = 9;
        assert!(Header::parse(&b).is_none(), "type");
        // STUN 응답(0x0101...)은 magic 불일치로 무시
        let stun = [0x01u8, 0x01, 0, 0, 0x21, 0x12, 0xa4, 0x42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(Header::parse(&stun).is_none());
    }

    #[test]
    fn nonce_layout() {
        let n = sample().nonce();
        assert_eq!(n, [0xDE, 0xAD, 0xBE, 0xEF, 1, 0, 0, 0, 1, 2, 3, 4]);
    }

    #[test]
    fn path_tag_rules() {
        assert_eq!(path_tag(false), 0x80);
        assert_eq!(path_tag(true), 0xC0);
        for via in [false, true] {
            assert!(path_tag_matches(path_tag(via), via));
            assert!(!path_tag_matches(path_tag(via), !via), "경로 위장");
            assert!(path_tag_matches(0, via), "태그 없는 옛 엔진은 출발 주소로");
        }
    }
}
