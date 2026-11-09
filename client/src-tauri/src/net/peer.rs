//! 피어 테이블, 홀펀칭 상태기계, keepalive, RTT (설계 문서 3절).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::crypto::{PacketCipher, KEY_BYTES};
use crate::engine::jitter::{JitterBuffer, JitterSettings};
use crate::engine::mixer::PeerMixCtl;
use crate::engine::stats::{PathKind, PeerStateKind};

pub const HELLO_INTERVAL: Duration = Duration::from_millis(100);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const PING_INTERVAL: Duration = Duration::from_millis(500);
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);
pub const LOST_AFTER: Duration = Duration::from_secs(5);
pub const FAIL_AFTER: Duration = Duration::from_secs(15);
pub const MAX_PEER_ID_BYTES: usize = 64;

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
    pub key: String,
    pub candidates: Vec<Candidate>,
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

    pub fn parsed_candidates(&self) -> Vec<(SocketAddr, CandidateKind)> {
        self.candidates
            .iter()
            .filter_map(|c| c.addr.parse::<SocketAddr>().ok().map(|a| (a, c.kind)))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ConnState {
    pub state: PeerStateKind,
    pub path: Option<(SocketAddr, PathKind)>,
    pub started: Instant,
    pub last_rx: Option<Instant>,
    pub last_hello: Option<Instant>,
    pub last_ping: Option<Instant>,
    pub rtt_ms: Option<f32>,
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
            rtt_ms: None,
        }
    }

    /// Hello/HelloAck를 `from`에서 받음. 경로 확정(LAN 우선) 후 Connected.
    pub fn on_handshake(&mut self, from: SocketAddr, cands: &[(SocketAddr, CandidateKind)], now: Instant) {
        let kind = match cands.iter().find(|(a, _)| *a == from) {
            Some((_, CandidateKind::Lan)) => PathKind::Lan,
            _ => PathKind::Public,
        };
        let replace = match self.path {
            None => true,
            Some((addr, _)) if addr == from => false,
            Some((_, PathKind::Public)) if kind == PathKind::Lan => true,
            // 끊김 상태에서는 새로 응답한 주소로 갈아탄다
            Some(_) => matches!(self.state, PeerStateKind::Reconnecting | PeerStateKind::Failed),
        };
        if replace {
            self.path = Some((from, kind));
        }
        self.state = PeerStateKind::Connected;
        self.last_rx = Some(now);
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
    pub rx: PacketCipher,
    pub candidates: Vec<(SocketAddr, CandidateKind)>,
    pub conn: Mutex<ConnState>,
    pub jitter: Mutex<JitterBuffer>,
    pub mix: Arc<PeerMixCtl>,
}

impl Peer {
    pub fn new(id: String, desc: &LocalDescriptor, jitter: JitterSettings, now: Instant) -> Result<Self, String> {
        if id.is_empty() || id.len() > MAX_PEER_ID_BYTES {
            return Err("peerId는 1..64바이트여야 합니다".into());
        }
        let key = desc.decode_key()?;
        let candidates = desc.parsed_candidates();
        if candidates.is_empty() {
            return Err("descriptor에 유효한 후보 주소가 없습니다".into());
        }
        Ok(Self {
            id,
            ssrc: desc.ssrc,
            rx: PacketCipher::new(&key),
            candidates,
            conn: Mutex::new(ConnState::new(now)),
            jitter: Mutex::new(JitterBuffer::new(jitter)),
            mix: Arc::new(PeerMixCtl::default()),
        })
    }

    /// Hello를 보낼 주소들: 모든 후보 + (다르면) 현재 경로.
    pub fn hello_targets(&self) -> Vec<SocketAddr> {
        let mut v: Vec<SocketAddr> = self.candidates.iter().map(|(a, _)| *a).collect();
        if let Some((p, _)) = self.conn.lock().path {
            if !v.contains(&p) {
                v.push(p);
            }
        }
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
        c.on_handshake("203.0.113.9:5000".parse().unwrap(), &cands(), t0);
        assert_eq!(c.path.unwrap().1, PathKind::Lan);
        // 후보에 없는 주소(peer-reflexive)는 public 취급
        let mut c = ConnState::new(t0);
        c.on_handshake("198.51.100.1:7".parse().unwrap(), &cands(), t0);
        assert_eq!(c.path.unwrap().1, PathKind::Public);
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
}
