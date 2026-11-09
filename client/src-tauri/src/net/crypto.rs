//! ChaCha20-Poly1305 패킷 암호화 (설계 문서 2절).
//!
//! 패킷 = 헤더(18B, AAD) || 암호문 || 태그(16B). 모든 연산은 호출자가 넘긴 버퍼 안에서
//! in-place로 수행되며 할당하지 않는다.

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};

use super::packet::Header;
use crate::engine::config::{HEADER_BYTES, MAX_PACKET_BYTES, TAG_BYTES};

pub const KEY_BYTES: usize = 32;

pub struct PacketCipher {
    aead: ChaCha20Poly1305,
}

impl PacketCipher {
    pub fn new(key: &[u8; KEY_BYTES]) -> Self {
        Self { aead: ChaCha20Poly1305::new(Key::from_slice(key)) }
    }

    pub fn random_key() -> [u8; KEY_BYTES] {
        use rand::RngCore;
        let mut k = [0u8; KEY_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut k);
        k
    }

    /// `out`에 헤더+평문을 쓰고 암호화한다. 패킷 길이를 반환.
    /// 평문이 패킷 한도를 넘거나 `out`이 작으면 `None`.
    pub fn seal(&self, header: &Header, plaintext: &[u8], out: &mut [u8]) -> Option<usize> {
        let total = HEADER_BYTES + plaintext.len() + TAG_BYTES;
        if total > MAX_PACKET_BYTES || out.len() < total {
            return None;
        }
        let mut hb = [0u8; HEADER_BYTES];
        header.write(&mut hb);
        out[..HEADER_BYTES].copy_from_slice(&hb);
        let body_end = HEADER_BYTES + plaintext.len();
        out[HEADER_BYTES..body_end].copy_from_slice(plaintext);
        let nonce = header.nonce();
        let (aad, rest) = out.split_at_mut(HEADER_BYTES);
        let tag = self
            .aead
            .encrypt_in_place_detached(Nonce::from_slice(&nonce), aad, &mut rest[..plaintext.len()])
            .ok()?;
        out[body_end..total].copy_from_slice(&tag);
        Some(total)
    }

    /// 패킷을 in-place 복호화하고 평문 범위(`buf` 내부 오프셋)를 반환한다.
    /// 헤더는 이미 파싱되어 `header`로 전달된다.
    pub fn open(&self, header: &Header, buf: &mut [u8]) -> Option<std::ops::Range<usize>> {
        if buf.len() < HEADER_BYTES + TAG_BYTES {
            return None;
        }
        let body_end = buf.len() - TAG_BYTES;
        let mut tag = [0u8; TAG_BYTES];
        tag.copy_from_slice(&buf[body_end..]);
        let nonce = header.nonce();
        let (aad, rest) = buf.split_at_mut(HEADER_BYTES);
        let body = &mut rest[..body_end - HEADER_BYTES];
        self.aead
            .decrypt_in_place_detached(Nonce::from_slice(&nonce), aad, body, Tag::from_slice(&tag))
            .ok()?;
        Some(HEADER_BYTES..body_end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::packet::{PacketType, FLAG_OPUS};

    fn hdr(seq: u32) -> Header {
        Header { ptype: PacketType::Audio, ssrc: 7, seq, timestamp: 99, flags: FLAG_OPUS, frame_code: 2 }
    }

    #[test]
    fn seal_open_roundtrip() {
        let c = PacketCipher::new(&PacketCipher::random_key());
        let mut buf = [0u8; MAX_PACKET_BYTES];
        let n = c.seal(&hdr(1), b"hello audio", &mut buf).unwrap();
        assert_eq!(n, HEADER_BYTES + 11 + TAG_BYTES);
        assert_ne!(&buf[HEADER_BYTES..HEADER_BYTES + 11], b"hello audio", "암호화되어야 함");
        let mut pkt = buf[..n].to_vec();
        let h = Header::parse(&pkt).unwrap();
        let r = c.open(&h, &mut pkt).unwrap();
        assert_eq!(&pkt[r], b"hello audio");
    }

    #[test]
    fn rejects_tamper_and_wrong_key() {
        let key = PacketCipher::random_key();
        let c = PacketCipher::new(&key);
        let mut buf = [0u8; MAX_PACKET_BYTES];
        let n = c.seal(&hdr(5), &[1, 2, 3, 4], &mut buf).unwrap();
        let pkt = buf[..n].to_vec();

        // 본문 변조
        let mut p = pkt.clone();
        p[HEADER_BYTES] ^= 1;
        assert!(c.open(&Header::parse(&p).unwrap(), &mut p).is_none());
        // 헤더(AAD) 변조: seq 변경 → nonce와 AAD 모두 달라짐
        let mut p = pkt.clone();
        p[11] ^= 1;
        assert!(c.open(&Header::parse(&p).unwrap(), &mut p).is_none());
        // flags 변조
        let mut p = pkt.clone();
        p[16] ^= 2;
        assert!(c.open(&Header::parse(&p).unwrap(), &mut p).is_none());
        // 태그 변조
        let mut p = pkt.clone();
        p[n - 1] ^= 1;
        assert!(c.open(&Header::parse(&p).unwrap(), &mut p).is_none());
        // 다른 키
        let other = PacketCipher::new(&PacketCipher::random_key());
        let mut p = pkt.clone();
        assert!(other.open(&Header::parse(&p).unwrap(), &mut p).is_none());
        // 원본은 통과
        let mut p = pkt;
        assert!(c.open(&Header::parse(&p).unwrap(), &mut p).is_some());
    }

    #[test]
    fn enforces_size_limit() {
        let c = PacketCipher::new(&[3u8; 32]);
        let mut buf = vec![0u8; 4096];
        let max_plain = MAX_PACKET_BYTES - HEADER_BYTES - TAG_BYTES;
        assert!(c.seal(&hdr(1), &vec![0u8; max_plain], &mut buf).is_some());
        assert!(c.seal(&hdr(1), &vec![0u8; max_plain + 1], &mut buf).is_none());
    }
}
