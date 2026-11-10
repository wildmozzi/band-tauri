//! 피어 테이블, 홀펀칭 상태기계, keepalive, RTT (설계 문서 3절).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::crypto::{KeyPair, PacketCipher, KEY_BYTES};
use super::packet::PacketType;
use crate::engine::jitter::{JitterBuffer, JitterSettings};
use crate::engine::mixer::PeerMixCtl;
use crate::engine::stats::{PathKind, PeerStateKind};

pub const HELLO_INTERVAL: Duration = Duration::from_millis(100);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const PING_INTERVAL: Duration = Duration::from_millis(500);
pub const LOST_AFTER: Duration = Duration::from_secs(5);
pub const FAIL_AFTER: Duration = Duration::from_secs(15);
pub const MAX_PEER_ID_BYTES: usize = 64;
/// descriptor 후보 수 상한(리뷰 M8: Hello 반사 폭주 방지).
pub const MAX_CANDIDATES: usize = 8;
/// 확정 경로가 아닌 같은 주소에서 인증된 Hello가 이만큼 연속으로 오면 경로를 옮긴다(NAT 리바인딩).
pub const REBIND_HELLOS: u32 = 5;
/// HelloAck 최소 간격(리뷰 L6: 재전송 Hello로 인한 반사 제한).
pub const HELLO_ACK_MIN_INTERVAL: Duration = Duration::from_millis(50);

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
pub fn validate_candidate(addr: &SocketAddr, allow_loopback: bool) -> Result<(), String> {
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
    pub fn validated_candidates(&self, allow_loopback: bool) -> Result<Vec<(SocketAddr, CandidateKind)>, String> {
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

fn seq_newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

#[derive(Debug, Clone)]
pub struct ConnState {
    pub state: PeerStateKind,
    pub path: Option<(SocketAddr, PathKind)>,
    pub started: Instant,
    pub last_rx: Option<Instant>,
    pub last_hello: Option<Instant>,
    pub last_ping: Option<Instant>,
    pub last_ack: Option<Instant>,
    pub rtt_ms: Option<f32>,
    /// 경로가 아닌 주소에서 연속으로 온 인증된 Hello (주소, 횟수).
    pub rebind: Option<(SocketAddr, u32)>,
    /// 제어 패킷 타입별 최대 seq (재전송 방어, 리뷰 L5).
    ctrl_seq: [Option<u32>; 6],
}

/// Hello/HelloAck 처리 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeOutcome {
    /// 경로 확정/유지 또는 전환.
    Accepted,
    /// 확정 경로가 아닌 주소: 상태·last_rx는 건드리지 않는다.
    Ignored,
}

impl ConnState {
    pub fn new(now: Instant) -> Self {
        Self {
            state: PeerStateKind::Connecting,
            path: None,
            started: now,
            last_rx: None,
            last_hello: None,
            last_ping: None,
            last_ack: None,
            rtt_ms: None,
            rebind: None,
            ctrl_seq: [None; 6],
        }
    }

    /// 제어 패킷 seq가 이전보다 새로우면 기록하고 true. 재전송(같거나 옛 seq)은 false.
    pub fn accept_ctrl_seq(&mut self, t: PacketType, seq: u32) -> bool {
        let slot = &mut self.ctrl_seq[(t as usize - 1).min(5)];
        match *slot {
            Some(m) if !seq_newer(seq, m) => false,
            _ => {
                *slot = Some(seq);
                true
            }
        }
    }

    /// Hello/HelloAck를 `from`에서 받음(리뷰 M6 규칙).
    /// - 경로 없음 / 같은 경로 / public→LAN 승격 / 끊김 상태 → 경로 확정, Connected.
    /// - Connected인데 다른 주소 → 같은 주소에서 `REBIND_HELLOS`회 연속이면 전환, 아니면 무시.
    pub fn on_handshake(&mut self, from: SocketAddr, cands: &[(SocketAddr, CandidateKind)], now: Instant) -> HandshakeOutcome {
        let kind = match cands.iter().find(|(a, _)| *a == from) {
            Some((_, CandidateKind::Lan)) => PathKind::Lan,
            _ => PathKind::Public,
        };
        let switch = match self.path {
            None => true,
            Some((addr, _)) if addr == from => {
                self.rebind = None;
                false
            }
            Some((_, PathKind::Public)) if kind == PathKind::Lan => true,
            Some(_) if matches!(self.state, PeerStateKind::Reconnecting | PeerStateKind::Failed) => true,
            Some(_) => {
                let count = match self.rebind {
                    Some((a, n)) if a == from => n + 1,
                    _ => 1,
                };
                self.rebind = Some((from, count));
                if count >= REBIND_HELLOS {
                    true
                } else {
                    return HandshakeOutcome::Ignored;
                }
            }
        };
        if switch {
            self.path = Some((from, kind));
            self.rebind = None;
        }
        self.state = PeerStateKind::Connected;
        self.last_rx = Some(now);
        HandshakeOutcome::Accepted
    }

    /// HelloAck를 보내도 되는지(간격 제한).
    pub fn ack_allowed(&mut self, now: Instant) -> bool {
        if self.last_ack.is_some_and(|t| now.duration_since(t) < HELLO_ACK_MIN_INTERVAL) {
            return false;
        }
        self.last_ack = Some(now);
        true
    }

    /// 확정 경로에서 인증된 패킷 수신.
    pub fn on_rx_from_path(&mut self, now: Instant) {
        self.last_rx = Some(now);
        if self.state == PeerStateKind::Reconnecting {
            self.state = PeerStateKind::Connected;
        }
    }

    pub fn on_rtt_sample(&mut self, rtt_ms: f32) {
        if !(0.0..10_000.0).contains(&rtt_ms) {
            return;
        }
        self.rtt_ms = Some(match self.rtt_ms {
            None => rtt_ms,
            Some(r) => r + (rtt_ms - r) * 0.125,
        });
    }

    pub fn is_path(&self, from: SocketAddr) -> bool {
        self.path.is_some_and(|(a, _)| a == from)
    }

    pub fn can_send_audio(&self) -> bool {
        self.path.is_some() && matches!(self.state, PeerStateKind::Connected | PeerStateKind::Reconnecting)
    }

    /// 제어 틱. 해야 할 송신을 돌려준다.
    pub fn tick(&mut self, now: Instant) -> TickAction {
        match self.state {
            PeerStateKind::Connecting => {
                if now.duration_since(self.started) >= CONNECT_TIMEOUT {
                    self.state = PeerStateKind::Failed;
                    return TickAction::None;
                }
                self.hello_due(now)
            }
            PeerStateKind::Connected => {
                let last = self.last_rx.unwrap_or(self.started);
                if now.duration_since(last) >= LOST_AFTER {
                    self.state = PeerStateKind::Reconnecting;
                    return self.hello_due(now);
                }
                // 500ms 핑이 1초 keepalive를 겸한다.
                let due = self.last_ping.is_none_or(|t| now.duration_since(t) >= PING_INTERVAL);
                if due {
                    self.last_ping = Some(now);
                    TickAction::Ping
                } else {
                    TickAction::None
                }
            }
            PeerStateKind::Reconnecting => {
                let last = self.last_rx.unwrap_or(self.started);
                if now.duration_since(last) >= FAIL_AFTER {
                    self.state = PeerStateKind::Failed;
                    return TickAction::None;
                }
                self.hello_due(now)
            }
            PeerStateKind::Failed => TickAction::None,
        }
    }

    fn hello_due(&mut self, now: Instant) -> TickAction {
        if self.last_hello.is_none_or(|t| now.duration_since(t) >= HELLO_INTERVAL) {
            self.last_hello = Some(now);
            TickAction::Hello
        } else {
            TickAction::None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickAction {
    None,
    Hello,
    Ping,
}

pub struct Peer {
    pub id: String,
    pub ssrc: u32,
    pub public_key: [u8; KEY_BYTES],
    /// 내 → 피어 송신 암호기.
    pub tx: PacketCipher,
    /// 피어 → 나 수신 암호기.
    pub rx: PacketCipher,
    pub candidates: Vec<(SocketAddr, CandidateKind)>,
    pub conn: Mutex<ConnState>,
    pub jitter: Mutex<JitterBuffer>,
    pub mix: Arc<PeerMixCtl>,
}

pub struct PeerSetup<'a> {
    pub keys: &'a KeyPair,
    pub my_ssrc: u32,
    pub jitter: JitterSettings,
    pub allow_loopback: bool,
}

impl Peer {
    pub fn new(id: String, desc: &LocalDescriptor, setup: &PeerSetup<'_>, now: Instant) -> Result<Self, String> {
        if id.is_empty() || id.len() > MAX_PEER_ID_BYTES {
            return Err("peerId는 1..64바이트여야 합니다".into());
        }
        let public_key = desc.decode_key()?;
        if public_key == setup.keys.public_bytes() {
            return Err("자기 자신의 descriptor입니다".into());
        }
        let candidates = desc.validated_candidates(setup.allow_loopback)?;
        let (tx, rx) = setup.keys.pair_ciphers(setup.my_ssrc, &public_key, desc.ssrc)?;
        Ok(Self {
            id,
            ssrc: desc.ssrc,
            public_key,
            tx,
            rx,
            candidates,
            conn: Mutex::new(ConnState::new(now)),
            jitter: Mutex::new(JitterBuffer::new(setup.jitter)),
            mix: Arc::new(PeerMixCtl::default()),
        })
    }

    /// Hello를 보낼 주소들: 모든 후보 + (다르면) 현재 경로 + 리바인딩 후보 주소. 최대 10개.
    pub fn hello_targets(&self) -> Vec<SocketAddr> {
        let mut v: Vec<SocketAddr> = self.candidates.iter().map(|(a, _)| *a).collect();
        let c = self.conn.lock();
        for extra in [c.path.map(|(a, _)| a), c.rebind.map(|(a, _)| a)].into_iter().flatten() {
            if !v.contains(&extra) {
                v.push(extra);
            }
        }
        v.truncate(MAX_CANDIDATES + 2);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cands() -> Vec<(SocketAddr, CandidateKind)> {
        vec![
            ("192.168.0.5:4000".parse().unwrap(), CandidateKind::Lan),
            ("203.0.113.9:5000".parse().unwrap(), CandidateKind::Public),
        ]
    }

    #[test]
    fn lan_preferred_over_public() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        c.on_handshake("203.0.113.9:5000".parse().unwrap(), &cands(), t0);
        assert_eq!(c.path.unwrap().1, PathKind::Public);
        assert_eq!(c.state, PeerStateKind::Connected);
        c.on_handshake("192.168.0.5:4000".parse().unwrap(), &cands(), t0);
        assert_eq!(c.path.unwrap().1, PathKind::Lan);
        // LAN 확정 후 public 응답은 무시
        assert_eq!(c.on_handshake("203.0.113.9:5000".parse().unwrap(), &cands(), t0), HandshakeOutcome::Ignored);
        assert_eq!(c.path.unwrap().1, PathKind::Lan);
        // 후보에 없는 주소(peer-reflexive)는 public 취급
        let mut c = ConnState::new(t0);
        c.on_handshake("198.51.100.1:7".parse().unwrap(), &cands(), t0);
        assert_eq!(c.path.unwrap().1, PathKind::Public);
    }

    #[test]
    fn hello_from_other_address_does_not_refresh_liveness() {
        // 리뷰 M6: 경로가 아닌 주소의 Hello는 last_rx/상태를 갱신하지 않는다
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let path: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        let other: SocketAddr = "198.51.100.1:7".parse().unwrap();
        c.on_handshake(path, &cands(), t0);
        let t1 = t0 + Duration::from_secs(3);
        assert_eq!(c.on_handshake(other, &cands(), t1), HandshakeOutcome::Ignored);
        assert_eq!(c.last_rx, Some(t0), "last_rx 유지");
        // 5초가 지나면 끊김으로 판정되어야 한다(가짜 Hello로 살아 있는 척 불가)
        c.tick(t0 + Duration::from_secs(5));
        assert_eq!(c.state, PeerStateKind::Reconnecting);
    }

    #[test]
    fn nat_rebinding_switches_after_consecutive_hellos() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let old: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        let new: SocketAddr = "203.0.113.9:6000".parse().unwrap();
        c.on_handshake(old, &cands(), t0);
        for i in 1..REBIND_HELLOS {
            assert_eq!(c.on_handshake(new, &cands(), t0), HandshakeOutcome::Ignored, "{i}");
        }
        assert_eq!(c.on_handshake(new, &cands(), t0), HandshakeOutcome::Accepted);
        assert_eq!(c.path.unwrap().0, new);
        // 중간에 원래 경로에서 오면 카운트 초기화
        let mut c = ConnState::new(t0);
        c.on_handshake(old, &cands(), t0);
        for _ in 0..REBIND_HELLOS - 1 {
            c.on_handshake(new, &cands(), t0);
        }
        c.on_handshake(old, &cands(), t0);
        assert_eq!(c.on_handshake(new, &cands(), t0), HandshakeOutcome::Ignored);
    }

    #[test]
    fn ctrl_replay_rejected() {
        let mut c = ConnState::new(Instant::now());
        assert!(c.accept_ctrl_seq(PacketType::Bye, 10));
        assert!(!c.accept_ctrl_seq(PacketType::Bye, 10), "같은 seq 재전송");
        assert!(!c.accept_ctrl_seq(PacketType::Bye, 3), "옛 seq");
        assert!(c.accept_ctrl_seq(PacketType::Bye, 11));
        assert!(c.accept_ctrl_seq(PacketType::Ping, 3), "타입별 독립");
        // u32 랩
        let mut c = ConnState::new(Instant::now());
        assert!(c.accept_ctrl_seq(PacketType::Hello, u32::MAX));
        assert!(c.accept_ctrl_seq(PacketType::Hello, 0));
    }

    #[test]
    fn hello_ack_rate_limited() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        assert!(c.ack_allowed(t0));
        assert!(!c.ack_allowed(t0 + Duration::from_millis(10)));
        assert!(c.ack_allowed(t0 + Duration::from_millis(60)));
    }

    #[test]
    fn state_machine_timeouts() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        assert_eq!(c.tick(t0), TickAction::Hello);
        assert_eq!(c.tick(t0 + Duration::from_millis(50)), TickAction::None);
        assert_eq!(c.tick(t0 + Duration::from_millis(100)), TickAction::Hello);
        assert_eq!(c.tick(t0 + Duration::from_secs(10)), TickAction::None);
        assert_eq!(c.state, PeerStateKind::Failed);

        let mut c = ConnState::new(t0);
        c.on_handshake("192.168.0.5:4000".parse().unwrap(), &cands(), t0);
        assert_eq!(c.tick(t0), TickAction::Ping);
        assert_eq!(c.tick(t0 + Duration::from_millis(400)), TickAction::None);
        assert_eq!(c.tick(t0 + Duration::from_millis(500)), TickAction::Ping);
        let t5 = t0 + Duration::from_secs(5);
        assert_eq!(c.tick(t5), TickAction::Hello);
        assert_eq!(c.state, PeerStateKind::Reconnecting);
        assert!(c.can_send_audio());
        c.on_rx_from_path(t5 + Duration::from_secs(1));
        assert_eq!(c.state, PeerStateKind::Connected);
        let t_lost = t5 + Duration::from_secs(6);
        c.tick(t_lost);
        assert_eq!(c.state, PeerStateKind::Reconnecting);
        c.tick(t5 + Duration::from_secs(16));
        assert_eq!(c.state, PeerStateKind::Failed);
    }

    #[test]
    fn rtt_ewma() {
        let mut c = ConnState::new(Instant::now());
        c.on_rtt_sample(10.0);
        assert_eq!(c.rtt_ms, Some(10.0));
        c.on_rtt_sample(18.0);
        assert_eq!(c.rtt_ms, Some(11.0));
        c.on_rtt_sample(-5.0);
        assert_eq!(c.rtt_ms, Some(11.0));
    }

    #[test]
    fn descriptor_roundtrip() {
        let key = [9u8; 32];
        let d = LocalDescriptor {
            ssrc: 42,
            key: LocalDescriptor::encode_key(&key),
            candidates: vec![Candidate { kind: CandidateKind::Lan, addr: "10.0.0.2:5555".into() }],
        };
        let j = serde_json::to_string(&d).unwrap();
        assert!(j.contains("\"kind\":\"lan\""));
        let back: LocalDescriptor = serde_json::from_str(&j).unwrap();
        assert_eq!(back.decode_key().unwrap(), key);
        assert!(LocalDescriptor { key: "AAAA".into(), ..d }.decode_key().is_err());
    }

    #[test]
    fn candidate_validation() {
        let mk = |addrs: &[&str]| LocalDescriptor {
            ssrc: 1,
            key: LocalDescriptor::encode_key(&[1u8; 32]),
            candidates: addrs.iter().map(|a| Candidate { kind: CandidateKind::Public, addr: a.to_string() }).collect(),
        };
        assert!(mk(&["203.0.113.1:5000"]).validated_candidates(false).is_ok());
        for bad in ["0.0.0.0:5000", "224.0.0.1:5000", "255.255.255.255:5000", "203.0.113.1:0", "[::1]:5000", "nope"] {
            assert!(mk(&[bad]).validated_candidates(false).is_err(), "{bad}");
        }
        assert!(mk(&["127.0.0.1:5000"]).validated_candidates(false).is_err());
        assert!(mk(&["127.0.0.1:5000"]).validated_candidates(true).is_ok());
        let many: Vec<String> = (0..9).map(|i| format!("203.0.113.{i}:5000")).collect();
        let refs: Vec<&str> = many.iter().map(|s| s.as_str()).collect();
        assert!(mk(&refs).validated_candidates(false).is_err(), "후보 9개는 거부");
        // 중복 제거
        assert_eq!(mk(&["203.0.113.1:5000", "203.0.113.1:5000"]).validated_candidates(false).unwrap().len(), 1);
    }
}
