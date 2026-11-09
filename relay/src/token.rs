//! 릴레이 토큰 (docs/overhaul/relay-design.md 3절). 시그널링 서버가 발급하고 릴레이가 검증한다.
//!
//! ```text
//! off  len  필드 (빅엔디안)
//! 0    1    token version = 1
//! 1    8    expires_at (unix 초, u64)
//! 9    4    ssrc (u32)
//! 13   1    room_len (1..=128)
//! 14   n    room_id (UTF-8)
//! ..   1    client_len (1..=64)
//! ..   m    client_id (UTF-8)
//! ..   32   HMAC-SHA256(RELAY_SECRET, 앞의 모든 바이트)
//! ```

use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const TOKEN_VERSION: u8 = 1;
pub const MAC_BYTES: usize = 32;
pub const MAX_ROOM_BYTES: usize = 128;
pub const MAX_CLIENT_BYTES: usize = 64;
/// 최소 토큰: 고정부 14 + room 1 + client_len 1 + client 1 + MAC
pub const MIN_TOKEN_BYTES: usize = 14 + 1 + 1 + 1 + MAC_BYTES;
pub const MAX_TOKEN_BYTES: usize = 14 + MAX_ROOM_BYTES + 1 + MAX_CLIENT_BYTES + MAC_BYTES;

type HmacSha256 = Hmac<Sha256>;

/// 검증된 토큰 내용. room/client는 입력 버퍼를 빌린다(할당 없음).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claims<'a> {
    pub expires_at: u64,
    pub ssrc: u32,
    pub room: &'a str,
    pub client: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    BadMac,
    Expired,
}

/// 토큰 서명 (테스트·도구용. 운영 발급은 시그널링 서버 `relayToken.ts`)
pub fn sign(secret: &[u8], claims: &Claims<'_>) -> Vec<u8> {
    assert!(!claims.room.is_empty() && claims.room.len() <= MAX_ROOM_BYTES);
    assert!(!claims.client.is_empty() && claims.client.len() <= MAX_CLIENT_BYTES);
    let mut out = Vec::with_capacity(MAX_TOKEN_BYTES);
    out.push(TOKEN_VERSION);
    out.extend_from_slice(&claims.expires_at.to_be_bytes());
    out.extend_from_slice(&claims.ssrc.to_be_bytes());
    out.push(claims.room.len() as u8);
    out.extend_from_slice(claims.room.as_bytes());
    out.push(claims.client.len() as u8);
    out.extend_from_slice(claims.client.as_bytes());
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC은 모든 키 길이를 받는다");
    mac.update(&out);
    out.extend_from_slice(&mac.finalize().into_bytes());
    out
}

/// 토큰 검증: 구조 → MAC(상수 시간 비교) → 만료 순서. `now_unix`는 현재 unix 초.
pub fn verify<'a>(secret: &[u8], token: &'a [u8], now_unix: u64) -> Result<Claims<'a>, TokenError> {
    if token.len() < MIN_TOKEN_BYTES || token.len() > MAX_TOKEN_BYTES || token[0] != TOKEN_VERSION {
        return Err(TokenError::Malformed);
    }
    let body_len = token.len() - MAC_BYTES;
    let (body, tag) = token.split_at(body_len);
    let room_len = body[13] as usize;
    if room_len == 0 || room_len > MAX_ROOM_BYTES || 14 + room_len + 1 > body_len {
        return Err(TokenError::Malformed);
    }
    let client_len = body[14 + room_len] as usize;
    let client_start = 15 + room_len;
    if client_len == 0 || client_len > MAX_CLIENT_BYTES || client_start + client_len != body_len {
        return Err(TokenError::Malformed);
    }
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC은 모든 키 길이를 받는다");
    mac.update(body);
    mac.verify_slice(tag).map_err(|_| TokenError::BadMac)?;

    let room = std::str::from_utf8(&body[14..14 + room_len]).map_err(|_| TokenError::Malformed)?;
    let client = std::str::from_utf8(&body[client_start..body_len]).map_err(|_| TokenError::Malformed)?;
    let mut exp = [0u8; 8];
    exp.copy_from_slice(&body[1..9]);
    let expires_at = u64::from_be_bytes(exp);
    if expires_at <= now_unix {
        return Err(TokenError::Expired);
    }
    let ssrc = u32::from_be_bytes([body[9], body[10], body[11], body[12]]);
    Ok(Claims { expires_at, ssrc, room, client })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-secret-0123456789abcdef0123456789";

    fn claims() -> Claims<'static> {
        Claims { expires_at: 1_900_000_000, ssrc: 0xDEADBEEF, room: "room-1", client: "client-a" }
    }

    #[test]
    fn roundtrip() {
        let t = sign(SECRET, &claims());
        assert_eq!(verify(SECRET, &t, 1_800_000_000), Ok(claims()));
    }

    #[test]
    fn expired() {
        let t = sign(SECRET, &claims());
        assert_eq!(verify(SECRET, &t, 1_900_000_000), Err(TokenError::Expired));
        assert_eq!(verify(SECRET, &t, 2_000_000_000), Err(TokenError::Expired));
    }

    #[test]
    fn forged() {
        let t = sign(SECRET, &claims());
        // 다른 비밀로 서명
        assert_eq!(verify(b"other-secret-xxxxxxxxxxxxxxxxxxxxxxxx", &t, 0), Err(TokenError::BadMac));
        // 본문 변조: ssrc, room, 만료 연장
        for idx in [5usize, 9, 15, t.len() - 33] {
            let mut f = t.clone();
            f[idx] ^= 1;
            assert_eq!(verify(SECRET, &f, 0), Err(TokenError::BadMac), "byte {idx}");
        }
        // 태그 변조
        let mut f = t.clone();
        *f.last_mut().unwrap() ^= 0x80;
        assert_eq!(verify(SECRET, &f, 0), Err(TokenError::BadMac));
    }

    #[test]
    fn malformed() {
        let t = sign(SECRET, &claims());
        assert_eq!(verify(SECRET, &t[..t.len() - 1], 0), Err(TokenError::Malformed));
        assert_eq!(verify(SECRET, &[], 0), Err(TokenError::Malformed));
        let mut f = t.clone();
        f[0] = 2;
        assert_eq!(verify(SECRET, &f, 0), Err(TokenError::Malformed));
        let mut f = t.clone();
        f[13] = 200; // room_len 범위 밖
        assert_eq!(verify(SECRET, &f, 0), Err(TokenError::Malformed));
        let mut long = t.clone();
        long.extend_from_slice(&[0u8; 300]);
        assert_eq!(verify(SECRET, &long, 0), Err(TokenError::Malformed));
    }

    /// 시그널링 서버(server/test/relay-token.test.ts)와 같은 벡터. 두 구현이 바이트 단위로 일치해야 한다.
    #[test]
    fn cross_language_vector() {
        let secret = b"band-relay-test-vector-secret-32bytes!!";
        let c = Claims {
            expires_at: 1_700_000_000,
            ssrc: 0x01020304,
            room: "room-abc",
            client: "11111111-2222-3333-4444-555555555555",
        };
        let t = sign(secret, &c);
        let hex: String = t.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, TEST_VECTOR_HEX);
        assert_eq!(verify(secret, &t, 1_600_000_000), Ok(c));
    }

    const TEST_VECTOR_HEX: &str = "01000000006553f1000102030408726f6f6d2d6162632431313131313131312d323232322d333333332d343434342d35353535353535353535353537cb2b9be3db194a0018849fd9df099457dbdc9f39aef51c5f621ba02d8f05b4";
}
