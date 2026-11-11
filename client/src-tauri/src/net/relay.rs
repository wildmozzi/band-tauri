//! UDP 릴레이 클라이언트 (docs/overhaul/relay-design.md, P1-4b).
//!
//! - 등록: RelayBind(토큰) → CookieRequired(쿠키) → RelayBind(토큰+쿠키) → Ok.
//! - keepalive: 등록 후 10초마다 같은 토큰으로 RelayBind. 릴레이 경로 피어가 1초 넘게 조용하면
//!   1초마다 당겨 보낸다(릴레이 재시작·NAT 재바인딩을 빨리 알아채고 쿠키 절차로 재등록).
//! - 토큰 갱신: 프론트가 서버에서 새 토큰을 받아 `set_token`으로 넘긴다(같은 주소 → 쿠키 없이 갱신).
//! - Ack가 3번 연속 없으면 `Unreachable`(계속 재시도). 토큰이 만료되면 `Expired`(새 토큰 전까지 송신 중지).
//! - 만료는 로컬 벽시계가 아니라 **받은 시각 + 남은 수명**(`deadline`, 서버가 준 `expiresIn`)으로 판단한다
//!   (PR #10 M1: 클라이언트 시계 오차). 서명·만료의 최종 판정은 릴레이가 한다.
//! - 송신량을 약 1초 창으로 재서 릴레이 클라이언트당 상한(2Mbps, 2500pps)의 90%에 닿으면 `over_budget`(M4).
//!
//! - Bind MAC(2.1.1): 서버가 토큰과 함께 WS(TLS)로 준 토큰별 키(`bindKey`)로 RelayBind 끝에
//!   `HMAC-SHA256(bindKey, 앞의 모든 바이트)[..16]`을 붙이고 flags bit0을 켠다. 쿠키까지 덮으므로 경로상 공격자가
//!   캡처한 토큰으로 자기 주소에서 쿠키 왕복을 해도 등록을 가로챌 수 없다. 키가 없으면(옛 서버) MAC 없이 보낸다.
//!
//! 엔진 패킷의 AEAD는 바뀌지 않는다. RelayData 접두부 8바이트는 AAD에 포함하지 않는다.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

use super::packet::{MAGIC, VERSION};

pub const TYPE_RELAY_BIND: u8 = 7;
pub const TYPE_RELAY_DATA: u8 = 8;
pub const TYPE_RELAY_BIND_ACK: u8 = 9;
/// RelayData 접두부 길이: magic(2) | version(1) | type(1) | dest_ssrc(4)
pub const DATA_PREFIX_BYTES: usize = 8;
pub const BIND_HEADER_BYTES: usize = 18;
pub const COOKIE_BYTES: usize = 16;
/// RelayBind flags bit0: 끝에 Bind MAC 16B (relay-design 2.1.1)
pub const BIND_FLAG_MAC: u8 = 0x01;
pub const BIND_MAC_BYTES: usize = 16;
pub const BIND_KEY_BYTES: usize = 32;
pub const ACK_BYTES: usize = 50;
pub const TOKEN_MIN_BYTES: usize = 49;
pub const TOKEN_MAX_BYTES: usize = 257;
const TOKEN_MAC_BYTES: usize = 32;
/// RelayBind 최대 길이(헤더 + 최대 토큰 + 쿠키 + MAC).
pub const BIND_MAX_BYTES: usize = BIND_HEADER_BYTES + TOKEN_MAX_BYTES + COOKIE_BYTES + BIND_MAC_BYTES;

/// 등록 후 keepalive 간격(계약 2.1).
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// 릴레이 경로 피어가 조용할 때 당겨 보내는 keepalive 간격.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(1);
/// 미등록(Binding/Unreachable) 상태의 재시도 간격.
pub const BIND_RETRY: Duration = Duration::from_millis(500);
/// 거절(RoomFull 등) 뒤 재시도 간격.
pub const REJECT_RETRY: Duration = Duration::from_secs(5);
/// CookieRequired에 대한 즉시 재전송 최소 간격(응답 폭주 방지).
pub const COOKIE_RESEND_MIN: Duration = Duration::from_millis(100);
/// 연속 무응답 Bind가 이만큼이면 `Unreachable`.
pub const UNREACHABLE_AFTER: u32 = 3;
/// 릴레이 클라이언트당 송신 상한(relay/src/relay.rs `Limits::default`와 같게 유지).
pub const RELAY_BUDGET_BPS: f32 = 2_000_000.0;
pub const RELAY_BUDGET_PPS: f32 = 2_500.0;
/// 상한의 이 비율을 넘으면 `over_budget`, `BUDGET_CLEAR` 아래로 내려가면 해제(깜박임 방지).
pub const BUDGET_WARN: f32 = 0.9;
pub const BUDGET_CLEAR: f32 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RelayStateKind {
    /// 등록 중(쿠키 왕복 포함).
    Binding,
    /// 등록됨: 릴레이 경로 사용 가능.
    Bound,
    /// Bind에 응답이 없다(릴레이 장애, UDP 차단). 계속 재시도한다.
    Unreachable,
    /// 릴레이가 거절(방 정원, ssrc 충돌, 서버 정원). 5초마다 재시도한다.
    Rejected,
    /// 토큰 만료: 새 토큰(`relay-token`)을 받을 때까지 Bind를 보내지 않는다.
    Expired,
}

/// 프론트로 나가는 릴레이 상태(설계 4절 `EngineStats.relay`).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelayStats {
    pub state: RelayStateKind,
    pub addr: String,
    /// 이 엔진 ↔ 릴레이 왕복(ms, Bind/Ack EWMA).
    pub rtt_ms: Option<f32>,
    /// 릴레이로 보내는 양(kbps, 최근 약 1초, RelayData 접두부 포함·UDP/IP 헤더 제외).
    pub tx_kbps: f32,
    /// 송신량이 릴레이 상한(2Mbps/2500pps)의 90%를 넘음: 릴레이가 패킷을 버리기 시작한다(PCM16 + 릴레이 피어 여럿).
    pub over_budget: bool,
}

/// 검증된 릴레이 토큰(서명은 확인할 수 없다 — 구조, ssrc, 만료만 본다).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayToken {
    pub bytes: Vec<u8>,
    pub expires_at: u64,
    pub ssrc: u32,
    /// 이 토큰의 Bind MAC 키(서버 `relay-token.bindKey`, 2.1.1). None이면 MAC 없이 Bind.
    pub bind_key: Option<[u8; BIND_KEY_BYTES]>,
}

impl RelayToken {
    /// base64url(패딩 유무 무관) 토큰을 해석한다. 토큰의 ssrc는 이 엔진의 ssrc와 같아야 한다.
    pub fn parse(b64: &str, my_ssrc: u32) -> Result<Self, String> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(b64.trim().trim_end_matches('='))
            .map_err(|e| format!("릴레이 토큰 base64 오류: {e}"))?;
        Self::from_bytes(bytes, my_ssrc)
    }

    pub fn from_bytes(bytes: Vec<u8>, my_ssrc: u32) -> Result<Self, String> {
        let len = bytes.len();
        if !(TOKEN_MIN_BYTES..=TOKEN_MAX_BYTES).contains(&len) || bytes[0] != 1 {
            return Err("릴레이 토큰 형식 오류".into());
        }
        let room_len = bytes[13] as usize;
        let member_at = 14 + room_len;
        if !(1..=128).contains(&room_len) || member_at >= len {
            return Err("릴레이 토큰 형식 오류(room)".into());
        }
        let member_len = bytes[member_at] as usize;
        if !(1..=64).contains(&member_len) || member_at + 1 + member_len + TOKEN_MAC_BYTES != len {
            return Err("릴레이 토큰 형식 오류(member)".into());
        }
        let expires_at = u64::from_be_bytes(bytes[1..9].try_into().map_err(|_| "릴레이 토큰 형식 오류")?);
        let ssrc = u32::from_be_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]);
        if ssrc != my_ssrc {
            return Err(format!("릴레이 토큰의 ssrc({ssrc:08x})가 엔진 ssrc({my_ssrc:08x})와 다릅니다"));
        }
        Ok(Self { bytes, expires_at, ssrc, bind_key: None })
    }
}

/// 서버가 준 Bind MAC 키(base64url, 패딩 유무 무관, 32바이트).
pub fn parse_bind_key(b64: &str) -> Result<[u8; BIND_KEY_BYTES], String> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(b64.trim().trim_end_matches('='))
        .map_err(|e| format!("릴레이 bindKey base64 오류: {e}"))?;
    raw.try_into().map_err(|_| "릴레이 bindKey는 32바이트여야 합니다".to_string())
}

/// `HMAC-SHA256(key, msg)[..16]`
fn bind_mac(key: &[u8; BIND_KEY_BYTES], msg: &[u8]) -> [u8; BIND_MAC_BYTES] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("32B 키");
    m.update(msg);
    let full = m.finalize().into_bytes();
    let mut out = [0u8; BIND_MAC_BYTES];
    out.copy_from_slice(&full[..BIND_MAC_BYTES]);
    out
}

/// 토큰 만료 시각(단조 시계). 서버가 준 남은 수명(`expires_in`, 발급 시각 기준)이 있으면 받은 시각 + 그 값을 쓴다
/// (클라이언트 시계 오차와 무관, M1). 없으면(옛 서버) 로컬 시계로 계산하되, 로컬 시계로 이미 만료면 None —
/// 시계가 빠른 PC에서 멀쩡한 토큰을 버리지 않고 판정을 릴레이에 맡긴다.
pub fn token_deadline(token: &RelayToken, expires_in: Option<Duration>, now: Instant, now_unix: u64) -> Option<Instant> {
    match expires_in {
        Some(d) => Some(now + d),
        None => token.expires_at.checked_sub(now_unix).filter(|s| *s > 0).map(|s| now + Duration::from_secs(s)),
    }
}

/// 릴레이 주소 검증: IPv4(소켓이 IPv4), 0.0.0.0/멀티캐스트/브로드캐스트/포트 0 거부, 루프백은 테스트 옵션에서만.
pub fn validate_relay_addr(addr: &SocketAddr, allow_loopback: bool) -> Result<(), String> {
    match addr.ip() {
        IpAddr::V4(v4) if addr.port() != 0 && !v4.is_unspecified() && !v4.is_multicast() && !v4.is_broadcast() => {
            if v4.is_loopback() && !allow_loopback {
                return Err(format!("루프백 릴레이 주소는 허용되지 않음: {addr}"));
            }
            Ok(())
        }
        _ => Err(format!("허용되지 않는 릴레이 주소: {addr}")),
    }
}

/// Ack 처리 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckAction {
    None,
    /// 쿠키를 붙여 즉시 다시 Bind.
    SendBind,
}

#[derive(Debug, Clone)]
pub struct RelayClient {
    /// 프론트가 준 `host:port` 문자열(같은 문자열이면 DNS를 다시 풀지 않는다, M3).
    pub host: String,
    pub addr: SocketAddr,
    token: RelayToken,
    /// 토큰 만료 시각(단조 시계 — 받은 시각 + 남은 수명). None이면 만료 판정을 릴레이에 맡긴다.
    deadline: Option<Instant>,
    cookie: Option<[u8; COOKIE_BYTES]>,
    pub state: RelayStateKind,
    /// 마지막 거절 상태 코드(1=RoomFull 2=SsrcConflict 4=ServerFull).
    pub reject: Option<u8>,
    last_bind: Option<Instant>,
    /// 마지막 Bind 이후 받은 Ack 없이 보낸 Bind 수.
    unacked: u32,
    seq: u32,
    pub rtt_ms: Option<f32>,
    /// 릴레이가 관측한 내 출발 주소(STUN 보조 정보).
    pub observed: Option<SocketAddr>,
    tx_kbps: f32,
    over_budget: bool,
}

impl RelayClient {
    pub fn new(host: String, addr: SocketAddr, token: RelayToken, deadline: Option<Instant>) -> Self {
        Self {
            host,
            addr,
            token,
            deadline,
            cookie: None,
            state: RelayStateKind::Binding,
            reject: None,
            last_bind: None,
            unacked: 0,
            seq: rand::random(),
            rtt_ms: None,
            observed: None,
            tx_kbps: 0.0,
            over_budget: false,
        }
    }

    pub fn token(&self) -> &RelayToken {
        &self.token
    }

    /// 새 토큰(갱신). 같은 주소면 등록을 유지한 채 다음 Bind에서 교체되고, 만료/거절 상태였으면 다시 등록한다.
    pub fn set_token(&mut self, token: RelayToken, deadline: Option<Instant>) {
        self.token = token;
        self.deadline = deadline;
        self.last_bind = None; // 즉시 Bind
        if matches!(self.state, RelayStateKind::Expired | RelayStateKind::Rejected) {
            self.state = RelayStateKind::Binding;
            self.reject = None;
        }
    }

    pub fn ready(&self) -> bool {
        self.state == RelayStateKind::Bound
    }

    pub fn stats(&self) -> RelayStats {
        RelayStats {
            state: self.state,
            addr: self.addr.to_string(),
            rtt_ms: self.rtt_ms,
            tx_kbps: self.tx_kbps,
            over_budget: self.over_budget,
        }
    }

    /// 최근 `elapsed` 동안 릴레이로 보낸 바이트·패킷 수를 반영한다(제어 스레드가 약 1초마다).
    pub fn record_tx(&mut self, bytes: u64, packets: u64, elapsed: Duration) {
        let secs = elapsed.as_secs_f32();
        if secs <= 0.0 {
            return;
        }
        let bps = bytes as f32 * 8.0 / secs;
        let pps = packets as f32 / secs;
        self.tx_kbps = bps / 1000.0;
        let load = (bps / RELAY_BUDGET_BPS).max(pps / RELAY_BUDGET_PPS);
        if load >= BUDGET_WARN {
            self.over_budget = true;
        } else if load < BUDGET_CLEAR {
            self.over_budget = false;
        }
    }

    /// 제어 틱. Bind를 보내야 하면 true. `probe`=릴레이 경로 피어가 조용함(빨리 확인).
    pub fn tick(&mut self, now: Instant, probe: bool) -> bool {
        if self.deadline.is_some_and(|d| now >= d) {
            self.state = RelayStateKind::Expired;
            return false;
        }
        let interval = match self.state {
            RelayStateKind::Bound if probe => PROBE_INTERVAL,
            RelayStateKind::Bound => KEEPALIVE_INTERVAL,
            RelayStateKind::Binding | RelayStateKind::Unreachable => BIND_RETRY,
            RelayStateKind::Rejected => REJECT_RETRY,
            RelayStateKind::Expired => return false, // 새 토큰(set_token)을 기다린다
        };
        let due = self.last_bind.is_none_or(|t| now.duration_since(t) >= interval);
        if due && self.unacked >= UNREACHABLE_AFTER && self.state != RelayStateKind::Rejected {
            self.state = RelayStateKind::Unreachable;
        }
        due
    }

    /// RelayBind를 `out`에 쓰고 길이를 돌려준다. `timestamp`는 Ack에 에코되어 RTT 측정에 쓰인다.
    pub fn write_bind(&mut self, now: Instant, my_ssrc: u32, timestamp: u32, out: &mut [u8; BIND_MAX_BYTES]) -> usize {
        self.seq = self.seq.wrapping_add(1);
        self.last_bind = Some(now);
        self.unacked = self.unacked.saturating_add(1);
        out[0..2].copy_from_slice(&MAGIC);
        out[2] = VERSION;
        out[3] = TYPE_RELAY_BIND;
        out[4..8].copy_from_slice(&my_ssrc.to_be_bytes());
        out[8..12].copy_from_slice(&self.seq.to_be_bytes());
        out[12..16].copy_from_slice(&timestamp.to_be_bytes());
        out[16] = 0;
        out[17] = 0;
        let mut n = BIND_HEADER_BYTES;
        let t = &self.token.bytes;
        out[n..n + t.len()].copy_from_slice(t);
        n += t.len();
        if let Some(c) = self.cookie {
            out[n..n + COOKIE_BYTES].copy_from_slice(&c);
            n += COOKIE_BYTES;
        }
        if let Some(key) = &self.token.bind_key {
            out[16] = BIND_FLAG_MAC;
            let mac = bind_mac(key, &out[..n]);
            out[n..n + BIND_MAC_BYTES].copy_from_slice(&mac);
            n += BIND_MAC_BYTES;
        }
        n
    }

    /// RelayBindAck 처리. 호출자는 출발 주소가 `self.addr`인지 먼저 확인한다.
    pub fn on_ack(&mut self, buf: &[u8], my_ssrc: u32, now: Instant, now_us32: u32) -> AckAction {
        if buf.len() != ACK_BYTES || buf[0..2] != MAGIC || buf[2] != VERSION || buf[3] != TYPE_RELAY_BIND_ACK {
            return AckAction::None;
        }
        let be = |i: usize| u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
        if be(4) != my_ssrc {
            return AckAction::None;
        }
        // 최근에 보낸 Bind의 seq만(옛 Ack·위조 방지: 출발 주소 확인과 함께)
        if self.seq.wrapping_sub(be(8)) >= 8 {
            return AckAction::None;
        }
        let answered_latest = be(8) == self.seq && self.unacked > 0;
        self.unacked = 0;
        let rtt = now_us32.wrapping_sub(be(12)) as f32 / 1000.0;
        if (0.0..10_000.0).contains(&rtt) {
            self.rtt_ms = Some(match self.rtt_ms {
                None => rtt,
                Some(r) => r + (rtt - r) * 0.25,
            });
        }
        if buf[20] == 4 {
            let port = u16::from_be_bytes([buf[22], buf[23]]);
            self.observed = Some(SocketAddr::from(([buf[24], buf[25], buf[26], buf[27]], port)));
        }
        match buf[16] {
            0 => {
                self.state = RelayStateKind::Bound;
                self.cookie = None;
                self.reject = None;
                AckAction::None
            }
            3 => {
                // 새 주소(첫 등록, 릴레이 재시작, NAT 재바인딩): 쿠키를 붙여 다시
                let mut c = [0u8; COOKIE_BYTES];
                c.copy_from_slice(&buf[34..50]);
                self.cookie = Some(c);
                if self.state != RelayStateKind::Rejected {
                    self.state = RelayStateKind::Binding;
                }
                // 마지막 Bind에 대한 첫 응답이면 바로 다시 보낸다(쿠키 왕복 1회). 그 밖의(옛 seq·중복) 쿠키
                // 요청에는 100ms에 한 번까지만 응답한다(PR #10 L1).
                if answered_latest || self.last_bind.is_none_or(|t| now.duration_since(t) >= COOKIE_RESEND_MIN) {
                    AckAction::SendBind
                } else {
                    AckAction::None
                }
            }
            code @ (1 | 2 | 4) => {
                self.state = RelayStateKind::Rejected;
                self.reject = Some(code);
                AckAction::None
            }
            _ => AckAction::None,
        }
    }
}

/// RelayData 접두부를 `out[..8]`에 쓴다(원본 엔진 패킷은 `out[8..]`에 이미 있어야 한다).
pub fn write_data_prefix(dest_ssrc: u32, out: &mut [u8]) {
    out[0..2].copy_from_slice(&MAGIC);
    out[2] = VERSION;
    out[3] = TYPE_RELAY_DATA;
    out[4..8].copy_from_slice(&dest_ssrc.to_be_bytes());
}

/// 릴레이 주소 문자열(`host:port`)을 IPv4 소켓 주소로 해석한다(DNS 조회 — 블로킹).
pub fn resolve_relay_addr(addr: &str) -> Result<SocketAddr, String> {
    use std::net::ToSocketAddrs;
    let addrs = addr.to_socket_addrs().map_err(|e| format!("릴레이 주소 해석 실패({addr}): {e}"))?;
    addrs.into_iter().find(|a| a.is_ipv4()).ok_or_else(|| format!("릴레이 주소에 IPv4가 없습니다: {addr}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 서버 `signRelayToken`과 같은 구조(MAC은 임의 — 엔진은 서명을 검증하지 않는다).
    pub(crate) fn token_bytes(expires_at: u64, ssrc: u32, room: &str, member: &str) -> Vec<u8> {
        let mut b = vec![1u8];
        b.extend_from_slice(&expires_at.to_be_bytes());
        b.extend_from_slice(&ssrc.to_be_bytes());
        b.push(room.len() as u8);
        b.extend_from_slice(room.as_bytes());
        b.push(member.len() as u8);
        b.extend_from_slice(member.as_bytes());
        b.extend_from_slice(&[0xAB; 32]);
        b
    }

    fn ack(bind: &[u8], status: u8, cookie: [u8; 16]) -> [u8; ACK_BYTES] {
        let mut a = [0u8; ACK_BYTES];
        a[..18].copy_from_slice(&bind[..18]);
        a[3] = TYPE_RELAY_BIND_ACK;
        a[16] = status;
        a[18..20].copy_from_slice(&30u16.to_be_bytes());
        a[20] = 4;
        a[22..24].copy_from_slice(&5000u16.to_be_bytes());
        a[24..28].copy_from_slice(&[203, 0, 113, 7]);
        a[34..50].copy_from_slice(&cookie);
        a
    }

    #[test]
    fn token_parse_and_validation() {
        let raw = token_bytes(2_000_000_000, 0xABCD, "room", "user");
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw);
        let t = RelayToken::parse(&b64, 0xABCD).unwrap();
        assert_eq!(t.expires_at, 2_000_000_000);
        assert_eq!(t.bytes, raw);
        // 패딩이 붙어도 받는다
        let padded = base64::engine::general_purpose::URL_SAFE.encode(&raw);
        assert!(RelayToken::parse(&padded, 0xABCD).is_ok());
        assert!(RelayToken::parse(&b64, 0x1234).is_err(), "ssrc 불일치");
        let mut bad = raw.clone();
        bad.push(0);
        assert!(RelayToken::from_bytes(bad, 0xABCD).is_err(), "길이 불일치");
        let mut bad = raw.clone();
        bad[0] = 2;
        assert!(RelayToken::from_bytes(bad, 0xABCD).is_err(), "버전");
        assert!(RelayToken::from_bytes(vec![1; 10], 0xABCD).is_err());
        assert!(RelayToken::parse("!!!", 0xABCD).is_err());
    }

    #[test]
    fn token_matches_shared_vectors() {
        // 서버·릴레이와 같은 공용 벡터의 토큰 구조를 엔진도 해석할 수 있어야 한다
        let json: serde_json::Value =
            serde_json::from_str(include_str!("../../../../docs/overhaul/relay-token-vectors.json")).unwrap();
        let vectors = json["vectors"].as_array().expect("벡터 배열");
        let mut checked = 0;
        for v in vectors {
            let hex = v["hex"].as_str().expect("hex");
            let bytes: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
            let ssrc = v["ssrc"].as_u64().expect("ssrc") as u32;
            let t = RelayToken::from_bytes(bytes.clone(), ssrc).unwrap_or_else(|e| panic!("{}: {e}", v["name"]));
            assert_eq!(t.ssrc, ssrc);
            assert_eq!(t.expires_at, v["expiresAt"].as_u64().unwrap());
            let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes);
            assert_eq!(RelayToken::parse(&b64, ssrc).unwrap(), t);
            checked += 1;
        }
        assert!(checked > 0, "벡터가 하나도 확인되지 않음");
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// 공용 벡터(relay-token-vectors.json)의 MAC 붙은 RelayBind를 바이트 단위로 같게 만든다(서버 키 파생·릴레이 검증과 일치).
    #[test]
    fn bind_mac_matches_shared_vectors() {
        let json: serde_json::Value =
            serde_json::from_str(include_str!("../../../../docs/overhaul/relay-token-vectors.json")).unwrap();
        let t0 = Instant::now();
        let mut checked = 0;
        for v in json["vectors"].as_array().expect("벡터 배열") {
            let name = v["name"].as_str().unwrap();
            let ssrc = v["ssrc"].as_u64().unwrap() as u32;
            let mut tok = RelayToken::from_bytes(unhex(v["hex"].as_str().unwrap()), ssrc).unwrap();
            let key_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(unhex(v["bindKeyHex"].as_str().unwrap()));
            tok.bind_key = Some(parse_bind_key(&key_b64).unwrap());
            let b = &v["bind"];
            let mut c = RelayClient::new("relay.test:4000".into(), "203.0.113.1:4000".parse().unwrap(), tok, None);
            c.seq = (b["seq"].as_u64().unwrap() as u32).wrapping_sub(1); // write_bind가 1 올린다
            c.cookie = b["cookieHex"].as_str().map(|h| unhex(h).try_into().unwrap());
            let mut out = [0u8; BIND_MAX_BYTES];
            let n = c.write_bind(t0, ssrc, b["timestamp"].as_u64().unwrap() as u32, &mut out);
            assert_eq!(out[..n], unhex(b["hex"].as_str().unwrap())[..], "{name}");
            checked += 1;
        }
        assert!(checked >= 3);
        assert!(parse_bind_key("short").is_err());
        assert!(parse_bind_key(&base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 31])).is_err());
    }

    #[test]
    fn bind_cookie_ok_flow() {
        let t0 = Instant::now();
        let tok = RelayToken::from_bytes(token_bytes(1_000, 7, "r", "m"), 7).unwrap();
        let mut c = RelayClient::new("relay.test:4000".into(), "203.0.113.1:4000".parse().unwrap(), tok.clone(), None);
        assert!(c.tick(t0, false));
        let mut out = [0u8; BIND_MAX_BYTES];
        let n = c.write_bind(t0, 7, 0, &mut out);
        assert_eq!(n, BIND_HEADER_BYTES + tok.bytes.len());
        assert_eq!(&out[..4], &[0x42, 0x4E, 1, 7]);
        assert!(!c.tick(t0 + Duration::from_millis(100), false), "재시도 간격 전");

        let a = ack(&out[..n], 3, [0xC0; 16]);
        assert_eq!(c.on_ack(&a, 7, t0, 0), AckAction::SendBind);
        assert_eq!(c.state, RelayStateKind::Binding);
        let n2 = c.write_bind(t0, 7, 0, &mut out);
        assert_eq!(n2, n + COOKIE_BYTES, "쿠키를 붙인다");
        assert_eq!(&out[n..n2], &[0xC0; 16]);

        let a = ack(&out[..n2], 0, [0; 16]);
        assert_eq!(c.on_ack(&a, 7, t0, 2_000), AckAction::None);
        assert_eq!(c.state, RelayStateKind::Bound);
        assert!(c.ready());
        // 쿠키 Ack(0ms)와 이번 Ack(2ms)의 EWMA(α=0.25)
        assert_eq!(c.rtt_ms, Some(0.5));
        assert_eq!(c.observed, Some("203.0.113.7:5000".parse().unwrap()));
        // keepalive: 쿠키 없이, 10초 간격(조용한 릴레이 피어가 있으면 1초)
        assert!(!c.tick(t0 + Duration::from_secs(5), false));
        assert!(c.tick(t0 + Duration::from_secs(5), true));
        assert!(c.tick(t0 + Duration::from_secs(10), false));
        assert_eq!(c.write_bind(t0, 7, 0, &mut out), n, "등록 후에는 쿠키 없음");
    }

    #[test]
    fn ack_filters() {
        let t0 = Instant::now();
        let tok = RelayToken::from_bytes(token_bytes(1_000, 7, "r", "m"), 7).unwrap();
        let mut c = RelayClient::new("relay.test:4000".into(), "203.0.113.1:4000".parse().unwrap(), tok, None);
        let mut out = [0u8; BIND_MAX_BYTES];
        let n = c.write_bind(t0, 7, 0, &mut out);
        let good = ack(&out[..n], 0, [0; 16]);
        let mut other_ssrc = good;
        other_ssrc[4..8].copy_from_slice(&8u32.to_be_bytes());
        assert_eq!(c.on_ack(&other_ssrc, 7, t0, 0), AckAction::None);
        let mut old_seq = good;
        old_seq[8..12].copy_from_slice(&c.seq.wrapping_sub(100).to_be_bytes());
        c.on_ack(&old_seq, 7, t0, 0);
        assert_eq!(c.state, RelayStateKind::Binding, "옛 seq Ack 무시");
        assert_eq!(c.on_ack(&good[..49], 7, t0, 0), AckAction::None, "길이");
        c.on_ack(&good, 7, t0, 0);
        assert_eq!(c.state, RelayStateKind::Bound);
    }

    #[test]
    fn unreachable_rejected_expired_and_refresh() {
        let t0 = Instant::now();
        let tok = RelayToken::from_bytes(token_bytes(1_000, 7, "r", "m"), 7).unwrap();
        let mut c = RelayClient::new("relay.test:4000".into(), "203.0.113.1:4000".parse().unwrap(), tok, None);
        let mut out = [0u8; BIND_MAX_BYTES];
        let mut t = t0;
        for _ in 0..UNREACHABLE_AFTER {
            assert!(c.tick(t, false));
            c.write_bind(t, 7, 0, &mut out);
            t += BIND_RETRY;
        }
        assert!(c.tick(t, false));
        assert_eq!(c.state, RelayStateKind::Unreachable, "Ack 3회 없음");
        let n = c.write_bind(t, 7, 0, &mut out);
        c.on_ack(&ack(&out[..n], 1, [0; 16]), 7, t, 0);
        assert_eq!(c.state, RelayStateKind::Rejected);
        assert_eq!(c.reject, Some(1));
        assert!(!c.tick(t + Duration::from_secs(1), false), "거절 후 5초 간격");
        assert!(c.tick(t + REJECT_RETRY, false));
        // 만료: 받은 시각 + 남은 수명(deadline) 기준
        let tok = RelayToken::from_bytes(token_bytes(1_000, 7, "r", "m"), 7).unwrap();
        c.set_token(tok, Some(t + Duration::from_secs(30)));
        assert!(c.tick(t + Duration::from_secs(29), false));
        assert!(!c.tick(t + Duration::from_secs(30), false));
        assert_eq!(c.state, RelayStateKind::Expired);
        assert!(!c.ready());
        assert!(!c.tick(t + Duration::from_secs(40), false), "만료 뒤에는 새 토큰 전까지 Bind 없음");
        // 새 토큰 → 즉시 재등록
        let t2 = t + Duration::from_secs(40);
        c.set_token(RelayToken::from_bytes(token_bytes(2_000, 7, "r", "m"), 7).unwrap(), Some(t2 + Duration::from_secs(120)));
        assert_eq!(c.state, RelayStateKind::Binding);
        assert!(c.tick(t2, false));
    }

    #[test]
    fn deadline_ignores_local_wall_clock_when_server_gives_expires_in() {
        // M1: 로컬 시계가 토큰의 expiresAt보다 120초 넘게 빨라도(이미 만료로 보여도) 서버가 준 남은 수명을 쓴다
        let now = Instant::now();
        let tok = RelayToken::from_bytes(token_bytes(1_000, 7, "r", "m"), 7).unwrap();
        let skewed_unix = 1_200;
        assert_eq!(token_deadline(&tok, Some(Duration::from_secs(120)), now, skewed_unix), Some(now + Duration::from_secs(120)));
        // 옛 서버(expiresIn 없음): 로컬 시계로 계산, 이미 만료로 보이면 판정을 릴레이에 맡긴다(None)
        assert_eq!(token_deadline(&tok, None, now, skewed_unix), None);
        assert_eq!(token_deadline(&tok, None, now, 900), Some(now + Duration::from_secs(100)));
    }

    #[test]
    fn cookie_resend_rate_limited_for_stale_or_duplicate_requests() {
        // L1: 마지막 Bind에 대한 첫 쿠키 요청에는 즉시, 중복·옛 seq 요청에는 100ms에 한 번만 응답
        let t0 = Instant::now();
        let tok = RelayToken::from_bytes(token_bytes(1_000, 7, "r", "m"), 7).unwrap();
        let mut c = RelayClient::new("relay.test:4000".into(), "203.0.113.1:4000".parse().unwrap(), tok, None);
        let mut out = [0u8; BIND_MAX_BYTES];
        let n = c.write_bind(t0, 7, 0, &mut out);
        let first = ack(&out[..n], 3, [0xC0; 16]);
        assert_eq!(c.on_ack(&first, 7, t0, 0), AckAction::SendBind);
        c.write_bind(t0, 7, 0, &mut out);
        // 같은(이제 옛) seq의 쿠키 요청 반복 → 100ms 안에는 무시
        assert_eq!(c.on_ack(&first, 7, t0 + Duration::from_millis(10), 0), AckAction::None);
        assert_eq!(c.on_ack(&first, 7, t0 + COOKIE_RESEND_MIN, 0), AckAction::SendBind);
    }

    #[test]
    fn tx_budget_warning_with_hysteresis() {
        let tok = RelayToken::from_bytes(token_bytes(1_000, 7, "r", "m"), 7).unwrap();
        let mut c = RelayClient::new("relay.test:4000".into(), "203.0.113.1:4000".parse().unwrap(), tok, None);
        let s = Duration::from_secs(1);
        // PCM16 mono 2.5ms 피어 1명: 282B × 400pps ≈ 0.9Mbps → 정상
        c.record_tx(282 * 400, 400, s);
        assert!(!c.stats().over_budget);
        assert!((c.stats().tx_kbps - 902.4).abs() < 0.1);
        // 피어 2명(PCM16 stereo): 522B × 800pps ≈ 3.3Mbps → 경고
        c.record_tx(522 * 800, 800, s);
        assert!(c.stats().over_budget);
        // 85%(경고와 해제 사이)면 유지, 80% 아래면 해제
        c.record_tx(212_500, 400, s);
        assert!(c.stats().over_budget);
        c.record_tx(150_000, 400, s);
        assert!(!c.stats().over_budget);
        // pps 상한(2500pps)도 본다
        c.record_tx(100_000, 2_400, s);
        assert!(c.stats().over_budget);
    }

    #[test]
    fn relay_addr_validation() {
        assert!(validate_relay_addr(&"203.0.113.1:4000".parse().unwrap(), false).is_ok());
        assert!(validate_relay_addr(&"127.0.0.1:4000".parse().unwrap(), false).is_err());
        assert!(validate_relay_addr(&"127.0.0.1:4000".parse().unwrap(), true).is_ok());
        for bad in ["0.0.0.0:4000", "203.0.113.1:0", "224.0.0.1:4000", "[::1]:4000"] {
            assert!(validate_relay_addr(&bad.parse().unwrap(), true).is_err(), "{bad}");
        }
        assert_eq!(resolve_relay_addr("127.0.0.1:3478").unwrap(), "127.0.0.1:3478".parse().unwrap());
        assert!(resolve_relay_addr("not a host").is_err());
    }

    #[test]
    fn data_prefix_layout() {
        let mut b = [0u8; 12];
        write_data_prefix(0xAABBCCDD, &mut b);
        assert_eq!(&b[..8], &[0x42, 0x4E, 1, 8, 0xAA, 0xBB, 0xCC, 0xDD]);
    }
}
