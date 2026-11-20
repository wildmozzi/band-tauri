//! 피어 디스크립터(계약 DTO): 시그널링으로 주고받는 후보 주소·공개키와 그 검증(설계 문서 3절).
//!
//! `net::peer`의 다른 항목을 쓰지 않는다(엔진 R-1 ⑥-7 — 어댑터가 가리키는 DTO 노드).

use std::net::{IpAddr, SocketAddr};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::net::crypto::KEY_BYTES;

/// descriptor 후보 수 상한(리뷰 M8: Hello 반사 폭주 방지).
pub(crate) const MAX_CANDIDATES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CandidateKind {
    Lan,
    Public,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub kind: CandidateKind,
    pub addr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalDescriptor {
    pub ssrc: u32,
    /// X25519 공개키(base64, 32바이트).
    pub key: String,
    pub candidates: Vec<Candidate>,
}

/// 후보 주소 검증(리뷰 M8). 0.0.0.0, 멀티캐스트, 브로드캐스트, 포트 0, IPv6(소켓이 IPv4),
/// 그리고 (허용하지 않으면) 루프백을 거부한다.
pub(crate) fn validate_candidate(addr: &SocketAddr, allow_loopback: bool) -> Result<(), String> {
    if addr.port() == 0 {
        return Err(format!("후보 포트 0: {addr}"));
    }
    match addr.ip() {
        IpAddr::V4(v4) => {
            if v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast() {
                return Err(format!("허용되지 않는 후보 주소: {addr}"));
            }
            if v4.is_loopback() && !allow_loopback {
                return Err(format!("루프백 후보는 허용되지 않음: {addr}"));
            }
            Ok(())
        }
        IpAddr::V6(_) => Err(format!("IPv6 후보는 지원하지 않음: {addr}")),
    }
}

impl LocalDescriptor {
    pub fn decode_key(&self) -> Result<[u8; KEY_BYTES], String> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(self.key.trim())
            .map_err(|e| format!("descriptor key base64 오류: {e}"))?;
        raw.try_into().map_err(|_| "descriptor key는 32바이트여야 합니다".to_string())
    }

    pub fn encode_key(key: &[u8; KEY_BYTES]) -> String {
        base64::engine::general_purpose::STANDARD.encode(key)
    }

    /// 후보 파싱과 검증. 상한 초과나 잘못된 주소가 있으면 descriptor 전체를 거부한다.
    pub(crate) fn validated_candidates(&self, allow_loopback: bool) -> Result<Vec<(SocketAddr, CandidateKind)>, String> {
        if self.candidates.len() > MAX_CANDIDATES {
            return Err(format!("후보가 너무 많습니다({}개 > {MAX_CANDIDATES})", self.candidates.len()));
        }
        let mut out = Vec::with_capacity(self.candidates.len());
        for c in &self.candidates {
            let a: SocketAddr = c.addr.parse().map_err(|_| format!("잘못된 후보 주소: {}", c.addr))?;
            validate_candidate(&a, allow_loopback)?;
            if !out.iter().any(|(x, _)| *x == a) {
                out.push((a, c.kind));
            }
        }
        if out.is_empty() {
            return Err("descriptor에 유효한 후보 주소가 없습니다".into());
        }
        Ok(out)
    }
}
