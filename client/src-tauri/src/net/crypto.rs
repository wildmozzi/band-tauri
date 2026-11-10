//! ChaCha20-Poly1305 패킷 암호화 (설계 문서 2절).
//!
//! 패킷 = 헤더(18B, AAD) || 암호문 || 태그(16B). 모든 연산은 호출자가 넘긴 버퍼 안에서
//! in-place로 수행되며 할당하지 않는다.

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};

use super::packet::Header;
use crate::engine::config::{HEADER_BYTES, MAX_PACKET_BYTES, TAG_BYTES};

pub const KEY_BYTES: usize = 32;

/// 세션 키쌍(X25519). descriptor의 `key`는 공개키다.
///
/// 피어 쌍마다 ECDH 공유 비밀에서 방향별 키를 HKDF-SHA256으로 유도한다(리뷰 M7). 그래서
/// 같은 방의 제3자(C)는 A의 공개키를 알아도 A→B 키를 만들 수 없어 A를 사칭하지 못한다.
pub struct KeyPair {
    secret: x25519_dalek::StaticSecret,
    public: x25519_dalek::PublicKey,
}

const HKDF_SALT: &[u8] = b"band-native-v1";

impl KeyPair {
    pub fn generate() -> Self {
        let secret = x25519_dalek::StaticSecret::random_from_rng(rand::rngs::OsRng);
        let public = x25519_dalek::PublicKey::from(&secret);
        Self { secret, public }
    }

    pub fn public_bytes(&self) -> [u8; KEY_BYTES] {
        self.public.to_bytes()
    }

    /// (내→상대 송신 암호기, 상대→나 수신 암호기)를 유도한다.
    pub fn pair_ciphers(
        &self,
        my_ssrc: u32,
        their_public: &[u8; KEY_BYTES],
        their_ssrc: u32,
    ) -> Result<(PacketCipher, PacketCipher), String> {
        let their = x25519_dalek::PublicKey::from(*their_public);
        let shared = self.secret.diffie_hellman(&their);
        if !shared.was_contributory() {
            return Err("잘못된 공개키(저위수 점)".into());
        }
        let mine = self.public_bytes();
        let tx = derive_key(shared.as_bytes(), &mine, my_ssrc, their_public, their_ssrc)?;
        let rx = derive_key(shared.as_bytes(), their_public, their_ssrc, &mine, my_ssrc)?;
        Ok((PacketCipher::new(&tx), PacketCipher::new(&rx)))
    }
}

/// 방향별 키: info = "pair" || 송신자 공개키 || 송신자 ssrc || 수신자 공개키 || 수신자 ssrc
fn derive_key(
    shared: &[u8; 32],
    sender_pub: &[u8; 32],
    sender_ssrc: u32,
    receiver_pub: &[u8; 32],
    receiver_ssrc: u32,
) -> Result<[u8; KEY_BYTES], String> {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(HKDF_SALT), shared);
    let mut info = [0u8; 4 + 32 + 4 + 32 + 4];
    info[..4].copy_from_slice(b"pair");
    info[4..36].copy_from_slice(sender_pub);
    info[36..40].copy_from_slice(&sender_ssrc.to_be_bytes());
    info[40..72].copy_from_slice(receiver_pub);
    info[72..76].copy_from_slice(&receiver_ssrc.to_be_bytes());
    let mut okm = [0u8; KEY_BYTES];
    hk.expand(&info, &mut okm).map_err(|_| "HKDF 실패".to_string())?;
    Ok(okm)
}

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
    fn pair_keys_are_directional_and_private() {
        let (a, b, c) = (KeyPair::generate(), KeyPair::generate(), KeyPair::generate());
        let (a_tx, a_rx) = a.pair_ciphers(1, &b.public_bytes(), 2).unwrap();
        let (b_tx, b_rx) = b.pair_ciphers(2, &a.public_bytes(), 1).unwrap();
        let mut buf = [0u8; MAX_PACKET_BYTES];
        let h = Header { ssrc: 1, ..hdr(9) };

        // A→B
        let n = a_tx.seal(&h, b"a2b", &mut buf).unwrap();
        let mut p = buf[..n].to_vec();
        let r = b_rx.open(&h, &mut p).unwrap();
        assert_eq!(&p[r], b"a2b");
        // 방향이 다르면 열리지 않음(B의 rx로 B가 보낸 것, A의 rx로 A가 보낸 것)
        let mut p = buf[..n].to_vec();
        assert!(a_rx.open(&h, &mut p).is_none());
        // B→A
        let h2 = Header { ssrc: 2, ..hdr(9) };
        let n = b_tx.seal(&h2, b"b2a", &mut buf).unwrap();
        let mut p = buf[..n].to_vec();
        assert!(a_rx.open(&h2, &mut p).is_some());

        // 제3자 C는 A의 공개키와 ssrc를 알아도 A→B 키를 만들 수 없다(사칭 불가)
        let (c_tx_to_b, _) = c.pair_ciphers(1, &b.public_bytes(), 2).unwrap();
        let n = c_tx_to_b.seal(&h, b"forged", &mut buf).unwrap();
        let mut p = buf[..n].to_vec();
        assert!(b_rx.open(&h, &mut p).is_none(), "C가 A를 사칭한 패킷은 거부");
    }

    #[test]
    fn rejects_low_order_public_key() {
        let a = KeyPair::generate();
        assert!(a.pair_ciphers(1, &[0u8; 32], 2).is_err());
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
