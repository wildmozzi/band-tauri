//! 피어 테이블, 홀펀칭 상태기계, keepalive, RTT (설계 문서 3절).

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use parking_lot::{Mutex, MutexGuard};
use serde::{Deserialize, Serialize};

use super::crypto::{KeyPair, PacketCipher, KEY_BYTES};
use super::packet::PacketType;
use super::media_link::{LossCell, MediaTx, PeerKey};
use super::types::{PathKind, PeerStateKind};

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
/// 직접 연결이 이 시간 안에 안 되면 릴레이로도 Hello를 보낸다(P1-4b, GOAL 3절 "입장 후 3초").
pub const RELAY_FALLBACK_AFTER: Duration = Duration::from_millis(1500);
/// 직접 경로에서 이 시간 동안 아무것도 못 받으면 릴레이로도 Hello를 보낸다(끊김 전에 대체 경로 확보).
pub const RELAY_PROBE_AFTER: Duration = Duration::from_millis(1500);
/// 릴레이 경로에서 직접 경로를 백그라운드로 다시 찾는 간격.
pub const DIRECT_PROBE_INTERVAL: Duration = Duration::from_secs(1);
/// `Failed` 뒤에도 이 간격으로 직접·릴레이 Hello를 계속 보낸다(늦게 열린 릴레이/경로로 회복).
pub const FAILED_PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// 경로를 바꾼 뒤 이전 경로로 오는 인증 패킷(비행 중)을 받아 주는 시간(무끊김 전환).
pub const PATH_GRACE: Duration = Duration::from_secs(2);
/// 릴레이 → 직접 승격 조건(PR #10 M2 히스테리시스): 같은 직접 주소에서 HelloAck(왕복 확인)가 이만큼, 간격
/// `PROMOTE_MAX_GAP` 이하로 끊김 없이, `PROMOTE_STABLE` 이상 이어져야 한다(1초 탐색 → 약 2초).
pub const PROMOTE_ACKS: u32 = 3;
pub const PROMOTE_STABLE: Duration = Duration::from_secs(2);
pub const PROMOTE_MAX_GAP: Duration = Duration::from_millis(1500);
/// 직접 경로 RTT가 릴레이 경유 종단 RTT보다 이만큼 넘게 크면 승격하지 않는다(ms).
pub const PROMOTE_RTT_MARGIN_MS: f32 = 10.0;
/// 직접 → 릴레이 강등 뒤 승격을 미루는 최소 시간. 승격 후 `FLAP_WINDOW` 안에 다시 강등되면(플래핑) 두 배씩
/// 늘려 `RELAY_HOLD_MAX`까지.
pub const RELAY_HOLD_MIN: Duration = Duration::from_secs(5);
pub const RELAY_HOLD_MAX: Duration = Duration::from_secs(60);
pub const FLAP_WINDOW: Duration = Duration::from_secs(30);

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
    /// 이 항목(add_peer)에서 처음 Connected가 된 시각(P1-20 `connectMs`). 한 번 정하면 재연결·경로 전환에도 그대로다.
    pub connected_at: Option<Instant>,
    pub last_rx: Option<Instant>,
    pub last_hello: Option<Instant>,
    pub last_ping: Option<Instant>,
    /// 주소별 마지막 HelloAck 시각(PR #5 L-4: 두 후보 경로가 서로를 굶기지 않게 주소 단위로 제한).
    pub last_acks: Vec<(SocketAddr, Instant)>,
    pub rtt_ms: Option<f32>,
    /// 경로가 아닌 주소에서 연속으로 온 인증된 Hello (주소, 횟수).
    pub rebind: Option<(SocketAddr, u32)>,
    /// 제어 패킷 타입별 최대 seq (재전송 방어, 리뷰 L5).
    ctrl_seq: [Option<u32>; 6],
    /// 이 피어와 릴레이 경유 Hello/HelloAck가 오간 적이 있다(릴레이로 온 인증 패킷 수용).
    pub relay_confirmed: bool,
    /// 릴레이 경로 중 직접 주소에서 인증된 Hello가 왔다. 그 주소에서 Hello 외 인증 패킷이 오면 상대가
    /// 직접 경로로 승격한 것이므로 따라 승격한다.
    pub direct_hint: Option<SocketAddr>,
    /// 직전 경로와 바꾼 시각(PATH_GRACE 동안 수신 허용).
    pub prev_path: Option<(SocketAddr, Instant)>,
    /// 상대가 Bye를 보냈다(더 탐색하지 않음).
    pub bye: bool,
    last_direct_probe: Option<Instant>,
    /// 릴레이 경로 중 직접 주소의 연속 HelloAck(승격 히스테리시스).
    direct_probe: Option<DirectProbe>,
    /// 이 시각 전에는 직접 경로로 승격하지 않는다(강등 직후·플래핑).
    pub promote_hold_until: Option<Instant>,
    /// 최근 연속 플래핑 횟수(승격 후 FLAP_WINDOW 안에 강등).
    pub flaps: u32,
    last_promote: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
struct DirectProbe {
    addr: SocketAddr,
    first: Instant,
    last: Instant,
    count: u32,
    rtt_ms: Option<f32>,
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
            connected_at: None,
            last_rx: None,
            last_hello: None,
            last_ping: None,
            last_acks: Vec::new(),
            rtt_ms: None,
            rebind: None,
            ctrl_seq: [None; 6],
            relay_confirmed: false,
            direct_hint: None,
            prev_path: None,
            bye: false,
            last_direct_probe: None,
            direct_probe: None,
            promote_hold_until: None,
            flaps: 0,
            last_promote: None,
        }
    }

    /// 직접 → 릴레이 강등: 승격을 잠시 미룬다(플래핑이면 점점 길게).
    fn on_demote(&mut self, now: Instant) {
        self.flaps = if self.last_promote.is_some_and(|t| now.duration_since(t) < FLAP_WINDOW) { self.flaps + 1 } else { 0 };
        let hold = RELAY_HOLD_MIN.saturating_mul(1 << self.flaps.min(4)).min(RELAY_HOLD_MAX);
        self.promote_hold_until = Some(now + hold);
        self.direct_probe = None;
    }

    fn on_promote(&mut self, now: Instant) {
        self.last_promote = Some(now);
        self.direct_probe = None;
    }

    /// 릴레이 경로 중 직접 주소 `from`의 HelloAck. 승격 조건(연속 N회·안정 시간·RTT·강등 유예)을 채우면 true.
    fn direct_ack_promotes(&mut self, from: SocketAddr, now: Instant, rtt: Option<f32>) -> bool {
        let mut p = match self.direct_probe {
            Some(p) if p.addr == from && now.duration_since(p.last) <= PROMOTE_MAX_GAP => DirectProbe { count: p.count + 1, last: now, ..p },
            _ => DirectProbe { addr: from, first: now, last: now, count: 1, rtt_ms: None },
        };
        if let Some(r) = rtt {
            p.rtt_ms = Some(p.rtt_ms.map_or(r, |x| x + (r - x) * 0.5));
        }
        self.direct_probe = Some(p);
        let held = self.promote_hold_until.is_some_and(|t| now < t);
        let rtt_ok = match (p.rtt_ms, self.rtt_ms) {
            (Some(direct), Some(relay)) => direct <= relay + PROMOTE_RTT_MARGIN_MS,
            _ => true,
        };
        p.count >= PROMOTE_ACKS && now.duration_since(p.first) >= PROMOTE_STABLE && !held && rtt_ok
    }

    /// 릴레이 주소가 `old` → `new`로 바뀜(None = 해제, M3). 릴레이 경로면 새 주소로 옮기고(해제면 경로를 비우고
    /// 직접 경로를 다시 찾는다), 옛 주소에서 비행 중인 패킷은 PATH_GRACE 동안 받는다.
    pub fn relay_moved(&mut self, old: SocketAddr, new: Option<SocketAddr>, now: Instant) {
        if self.path != Some((old, PathKind::Relay)) {
            if new.is_none() {
                self.relay_confirmed = false;
            }
            return;
        }
        self.prev_path = Some((old, now));
        match new {
            Some(n) => self.path = Some((n, PathKind::Relay)),
            None => {
                self.path = None;
                self.relay_confirmed = false;
                if self.state == PeerStateKind::Connected {
                    self.state = PeerStateKind::Reconnecting;
                }
            }
        }
    }

    fn set_path(&mut self, addr: SocketAddr, kind: PathKind, now: Instant) {
        if let Some((old, _)) = self.path {
            if old != addr {
                self.prev_path = Some((old, now));
            }
        }
        self.path = Some((addr, kind));
        self.rebind = None;
        self.direct_hint = None;
    }

    pub fn path_kind(&self) -> Option<PathKind> {
        self.path.map(|(_, k)| k)
    }

    /// 같은 세션(같은 키)의 피어를 다시 추가할 때 재전송 창을 이어받는다(PR #5 L-2).
    pub fn inherit_replay_window(&mut self, old: &ConnState) {
        self.ctrl_seq = old.ctrl_seq;
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

    /// Hello/HelloAck를 `from`에서 받음(리뷰 M6 규칙 + 릴레이, P1-4b).
    /// - 경로 없음 / 같은 경로 / public→LAN 승격 / 끊김 상태 → 경로 확정, Connected.
    /// - 릴레이(`from == relay`)로 왔다: 상대가 직접 경로로 우리 소리를 못 받는다는 뜻이므로 릴레이 경로로 바꾼다.
    /// - 릴레이 경로인데 직접 주소: HelloAck(왕복 확인)가 승격 조건(`direct_ack_promotes`: 연속 3회·2초 안정·
    ///   RTT·강등 유예)을 채우면 직접 경로로 승격. 그 전까지(와 Hello)는 힌트만 남기고 무시.
    /// - Connected인데 다른 직접 주소 → 같은 주소에서 `REBIND_HELLOS`회 연속이면 전환, 아니면 무시.
    pub fn on_handshake(
        &mut self,
        from: SocketAddr,
        is_ack: bool,
        cands: &[(SocketAddr, CandidateKind)],
        relay: Option<SocketAddr>,
        now: Instant,
    ) -> HandshakeOutcome {
        self.on_handshake_rtt(from, is_ack, cands, relay, now, None)
    }

    /// `on_handshake` + HelloAck로 잰 그 경로의 왕복 시간(`ack_rtt`, Hello timestamp 에코).
    pub fn on_handshake_rtt(
        &mut self,
        from: SocketAddr,
        is_ack: bool,
        cands: &[(SocketAddr, CandidateKind)],
        relay: Option<SocketAddr>,
        now: Instant,
        ack_rtt: Option<f32>,
    ) -> HandshakeOutcome {
        let via_relay = relay == Some(from);
        let kind = if via_relay {
            self.relay_confirmed = true;
            PathKind::Relay
        } else {
            direct_kind(from, cands)
        };
        let switch = match self.path {
            None => true,
            Some((addr, _)) if addr == from => {
                self.rebind = None;
                false
            }
            Some(_) if via_relay => true,
            Some(_) if matches!(self.state, PeerStateKind::Reconnecting | PeerStateKind::Failed) => true,
            Some((_, PathKind::Relay)) => {
                self.direct_hint = Some(from);
                if is_ack && self.direct_ack_promotes(from, now, ack_rtt) {
                    true
                } else {
                    return HandshakeOutcome::Ignored;
                }
            }
            Some((_, PathKind::Public)) if kind == PathKind::Lan => true,
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
            let old_kind = self.path_kind();
            let probe_rtt = self.direct_probe.and_then(|p| p.rtt_ms);
            match (old_kind, kind) {
                (Some(PathKind::Lan | PathKind::Public), PathKind::Relay) => self.on_demote(now),
                (Some(PathKind::Relay), PathKind::Lan | PathKind::Public) => self.on_promote(now),
                _ => {}
            }
            if old_kind != Some(kind) {
                // 경로 종류가 바뀌면 RTT도 그 경로 기준으로(이 HelloAck나 승격 탐색에서 잰 값, 없으면 핑으로 다시)
                self.rtt_ms = ack_rtt.or(probe_rtt);
            }
            self.set_path(from, kind, now);
        } else if let (true, Some(r)) = (self.is_path(from), ack_rtt) {
            self.on_rtt_sample(r);
        }
        self.state = PeerStateKind::Connected;
        self.connected_at.get_or_insert(now);
        self.bye = false;
        self.last_rx = Some(now);
        HandshakeOutcome::Accepted
    }

    /// Hello/HelloAck 외 인증 패킷을 `from`에서 받음. 받아들이면 true(처리 계속), 아니면 폐기.
    /// - 확정 경로 → 생존 갱신.
    /// - 릴레이 경로인데 `direct_hint` 주소 → 상대가 직접 경로로 승격했다(우리 HelloAck가 닿았다): 따라 승격.
    /// - 릴레이가 확인된 피어의 릴레이 주소, 직전 경로(PATH_GRACE 이내) → 데이터만 받는다(생존은 갱신하지 않음).
    pub fn accept_rx(&mut self, from: SocketAddr, cands: &[(SocketAddr, CandidateKind)], relay: Option<SocketAddr>, now: Instant) -> bool {
        if self.is_path(from) {
            self.on_rx_from_path(now);
            return true;
        }
        if self.path_kind() == Some(PathKind::Relay) && self.direct_hint == Some(from) {
            // 상대가 자기 승격 조건을 채우고 직접 경로로 보내기 시작했다: 따라가지 않으면 소리가 끊기므로 유예와 무관하게
            self.on_promote(now);
            self.rtt_ms = None;
            self.set_path(from, direct_kind(from, cands), now);
            self.on_rx_from_path(now);
            return true;
        }
        if relay == Some(from) && self.relay_confirmed {
            return true;
        }
        self.prev_path.is_some_and(|(a, t)| a == from && now.duration_since(t) < PATH_GRACE)
    }

    /// `to`로 HelloAck를 보내도 되는지(주소별 간격 제한). 기억하는 주소는 후보 수만큼으로 제한한다.
    pub fn ack_allowed(&mut self, to: SocketAddr, now: Instant) -> bool {
        if let Some(e) = self.last_acks.iter_mut().find(|(a, _)| *a == to) {
            if now.duration_since(e.1) < HELLO_ACK_MIN_INTERVAL {
                return false;
            }
            e.1 = now;
            return true;
        }
        if self.last_acks.len() >= MAX_CANDIDATES + 2 {
            // 가장 오래된 항목을 교체
            if let Some(i) = self.last_acks.iter().enumerate().min_by_key(|(_, (_, t))| *t).map(|(i, _)| i) {
                self.last_acks.swap_remove(i);
            }
        }
        self.last_acks.push((to, now));
        true
    }

    /// 확정 경로에서 인증된 패킷 수신. 경로가 살아 있으므로 리바인딩 카운트를 지운다(PR #5 L-3:
    /// "연속 5회"가 Ping/Audio에 끼어든 Hello로 누적되지 않게).
    pub fn on_rx_from_path(&mut self, now: Instant) {
        self.rebind = None;
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

    /// 제어 틱. 해야 할 송신을 돌려준다. `relay_ready`=이 엔진이 릴레이에 등록되어 있다.
    pub fn tick(&mut self, now: Instant, relay_ready: bool) -> TickAction {
        let mut a = TickAction::default();
        let since_rx = now.duration_since(self.last_rx.unwrap_or(self.started));
        if self.state == PeerStateKind::Connected && since_rx >= LOST_AFTER {
            self.state = PeerStateKind::Reconnecting;
        }
        match self.state {
            PeerStateKind::Connecting => {
                let elapsed = now.duration_since(self.started);
                if elapsed >= CONNECT_TIMEOUT {
                    self.state = PeerStateKind::Failed;
                    return a;
                }
                if self.hello_due(now, HELLO_INTERVAL) {
                    a.hello_direct = true;
                    a.hello_relay = relay_ready && elapsed >= RELAY_FALLBACK_AFTER;
                }
            }
            PeerStateKind::Connected => {
                // 500ms 핑이 1초 keepalive를 겸한다.
                if self.last_ping.is_none_or(|t| now.duration_since(t) >= PING_INTERVAL) {
                    self.last_ping = Some(now);
                    a.ping = true;
                }
                if self.path_kind() == Some(PathKind::Relay) {
                    // 릴레이 중에도 직접 경로를 계속 찾는다(그 주소의 HelloAck가 오면 승격)
                    if self.last_direct_probe.is_none_or(|t| now.duration_since(t) >= DIRECT_PROBE_INTERVAL) {
                        self.last_direct_probe = Some(now);
                        a.hello_direct = true;
                    }
                } else if relay_ready && since_rx >= RELAY_PROBE_AFTER && self.hello_due(now, HELLO_INTERVAL * 2) {
                    a.hello_relay = true;
                }
            }
            PeerStateKind::Reconnecting => {
                if since_rx >= FAIL_AFTER {
                    self.state = PeerStateKind::Failed;
                    return a;
                }
                if self.hello_due(now, HELLO_INTERVAL) {
                    a.hello_direct = true;
                    a.hello_relay = relay_ready;
                }
            }
            PeerStateKind::Failed => {
                if !self.bye && self.hello_due(now, FAILED_PROBE_INTERVAL) {
                    a.hello_direct = true;
                    a.hello_relay = relay_ready;
                }
            }
        }
        a
    }

    fn hello_due(&mut self, now: Instant, interval: Duration) -> bool {
        if self.last_hello.is_none_or(|t| now.duration_since(t) >= interval) {
            self.last_hello = Some(now);
            true
        } else {
            false
        }
    }
}

fn direct_kind(from: SocketAddr, cands: &[(SocketAddr, CandidateKind)]) -> PathKind {
    match cands.iter().find(|(a, _)| *a == from) {
        Some((_, CandidateKind::Lan)) => PathKind::Lan,
        _ => PathKind::Public,
    }
}

/// 이번 틱에 보낼 것.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TickAction {
    /// 직접 후보(와 현재 직접 경로·리바인딩 후보)로 Hello.
    pub hello_direct: bool,
    /// 릴레이 경유 Hello(RelayData).
    pub hello_relay: bool,
    /// 확정 경로로 Ping.
    pub ping: bool,
}

impl TickAction {
    pub const NONE: TickAction = TickAction { hello_direct: false, hello_relay: false, ping: false };
    pub const HELLO: TickAction = TickAction { hello_direct: true, hello_relay: false, ping: false };
    pub const PING: TickAction = TickAction { hello_direct: false, hello_relay: false, ping: true };
}

// ============================================================================
// 에코 피어 모드(P1-1, 설계 2절 "Ping 평문", 4절 `engine_set_echo_mode`)
// ============================================================================

/// Ping 평문의 수신 손실 보고 태그(P1-4e, 설계 2절). 블록 = `[LOSS_TAG, loss(u16 BE), conceal(u16 BE)]`(5바이트),
/// 단위 0.01%. 에코 블록이 있으면 그 **뒤**에 붙는다. 옛 엔진은 `[ECHO_TAG, f, d×4, ..]`만 보고 나머지는 무시하며,
/// 에코 블록 없이 이 블록만 오면 첫 바이트가 `ECHO_TAG`가 아니므로 빈 평문(flags 0)과 같게 처리한다.
pub const LOSS_TAG: u8 = 0x4C;
pub const LOSS_REPORT_BYTES: usize = 5;
/// Ping 평문 최대 길이(에코 6 + 손실 5).
pub const PING_PLAIN_MAX: usize = ECHO_PAYLOAD_BYTES + LOSS_REPORT_BYTES;

/// 받는 쪽이 본 그 피어 스트림의 최근 5초 손실률·은닉률(%, 지터 버퍼 `loss_pct`·`conceal_pct`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LossReport {
    pub loss_pct: f32,
    pub conceal_pct: f32,
}

impl LossReport {
    fn to_units(pct: f32) -> u16 {
        if pct.is_finite() { (pct.clamp(0.0, 100.0) * 100.0).round() as u16 } else { 0 }
    }

    pub fn encode(&self) -> [u8; LOSS_REPORT_BYTES] {
        let l = Self::to_units(self.loss_pct).to_be_bytes();
        let c = Self::to_units(self.conceal_pct).to_be_bytes();
        [LOSS_TAG, l[0], l[1], c[0], c[1]]
    }

    /// Ping 평문에서 손실 보고(없으면 None). 에코 블록이 앞에 있으면 건너뛴다. 100% 넘는 값은 100%로 자른다.
    pub fn parse_ping(plain: &[u8]) -> Option<Self> {
        let rest = match plain {
            [ECHO_TAG, _, _, _, _, _, rest @ ..] => rest,
            _ => plain,
        };
        match rest {
            [LOSS_TAG, a, b, c, d, ..] => Some(Self {
                loss_pct: (u16::from_be_bytes([*a, *b]) as f32 / 100.0).min(100.0),
                conceal_pct: (u16::from_be_bytes([*c, *d]) as f32 / 100.0).min(100.0),
            }),
            _ => None,
        }
    }
}

/// Ping 평문 조립: 에코 블록(있으면) + 손실 보고(있으면). 반환 = 길이(0 = 빈 평문).
pub fn ping_plain(echo: Option<[u8; ECHO_PAYLOAD_BYTES]>, loss: Option<LossReport>, out: &mut [u8; PING_PLAIN_MAX]) -> usize {
    let mut n = 0;
    if let Some(e) = echo {
        out[..ECHO_PAYLOAD_BYTES].copy_from_slice(&e);
        n = ECHO_PAYLOAD_BYTES;
    }
    if let Some(l) = loss {
        out[n..n + LOSS_REPORT_BYTES].copy_from_slice(&l.encode());
        n += LOSS_REPORT_BYTES;
    }
    n
}

/// Ping 평문의 에코 제어 태그. 평문 = `[ECHO_TAG, flags, delay_us(u32 BE)]`(6바이트). 빈 평문 = flags 0.
pub const ECHO_TAG: u8 = 0xEC;
pub const ECHO_PAYLOAD_BYTES: usize = 6;
/// flags bit0: 보낸 쪽이 받는 쪽에게 "내 오디오를 되돌려 달라"고 요청 중(측정 중).
pub const ECHO_FLAG_REQUEST: u8 = 0b01;
/// flags bit1: 보낸 쪽이 받는 쪽의 오디오를 되돌려 보내는 중(동의함). `delay_us` = 되돌리는 처리 지연.
pub const ECHO_FLAG_ACTIVE: u8 = 0b10;
/// 요청/활성 표시가 이 시간 넘게 갱신되지 않으면 끝난 것으로 본다(Ping 500ms 간격 × 6).
pub const ECHO_LAPSE: Duration = Duration::from_secs(3);

/// 피어 하나와의 에코 상태. 요청·동의는 Ping에 실어 수준(level)으로 알린다(손실에 강함).
#[derive(Debug, Default)]
pub struct EchoState {
    /// 내가 이 피어에게 에코를 요청 중(내 쪽 측정 중).
    pub requesting: bool,
    /// 사용자가 이 피어의 에코 요청에 동의했다. 피어의 요청이 살아 있는 동안만 실제로 되돌린다.
    pub enabled: bool,
    /// 피어가 마지막으로 요청 플래그를 보낸 시각.
    pub remote_request_at: Option<Instant>,
    /// 피어가 마지막으로 활성 플래그(나를 에코 중)를 보낸 시각과 보고한 처리 지연(µs).
    pub remote_active_at: Option<Instant>,
    pub remote_delay_us: u32,
    /// 되돌리는 패킷의 타임스탬프 오프셋(에코 시작 시 내 송신 타임스탬프에 이어지게 정한다).
    pub ts_offset: Option<u32>,
    /// 되돌리기 처리 시간(수신 → 송신) EWMA(µs). Ping으로 보고한다.
    pub reflect_us: f32,
}

impl EchoState {
    pub fn remote_requesting(&self, now: Instant) -> bool {
        self.remote_request_at.is_some_and(|t| now.saturating_duration_since(t) < ECHO_LAPSE)
    }

    /// 지금 이 피어의 오디오를 되돌려 보내야 하는가(동의 + 피어 요청 유효).
    pub fn reflecting(&self, now: Instant) -> bool {
        self.enabled && self.remote_requesting(now)
    }

    /// 피어가 나를 에코 중인가.
    pub fn remote_echoing(&self, now: Instant) -> bool {
        self.remote_active_at.is_some_and(|t| now.saturating_duration_since(t) < ECHO_LAPSE)
    }

    /// Ping 평문(플래그가 없으면 None = 빈 평문).
    pub fn ping_payload(&self, now: Instant) -> Option<[u8; ECHO_PAYLOAD_BYTES]> {
        let mut flags = 0;
        if self.requesting {
            flags |= ECHO_FLAG_REQUEST;
        }
        if self.reflecting(now) {
            flags |= ECHO_FLAG_ACTIVE;
        }
        if flags == 0 {
            return None;
        }
        let d = (self.reflect_us.max(0.0).round() as u32).to_be_bytes();
        Some([ECHO_TAG, flags, d[0], d[1], d[2], d[3]])
    }

    /// 피어의 Ping 평문 처리. 요청이 없으면 동의도 끝난다(측정 끝 = 에코 끝).
    pub fn on_ping(&mut self, plain: &[u8], now: Instant) {
        let (flags, delay) = match plain {
            [ECHO_TAG, f, a, b, c, d, ..] => (*f, u32::from_be_bytes([*a, *b, *c, *d])),
            _ => (0, 0),
        };
        if flags & ECHO_FLAG_REQUEST != 0 {
            self.remote_request_at = Some(now);
        } else {
            self.remote_request_at = None;
            self.stop_reflecting();
        }
        if flags & ECHO_FLAG_ACTIVE != 0 {
            self.remote_active_at = Some(now);
            self.remote_delay_us = delay;
        } else {
            self.remote_active_at = None;
        }
    }

    pub fn stop_reflecting(&mut self) {
        self.enabled = false;
        self.ts_offset = None;
    }

    /// 제어 스레드 틱: 피어 요청이 끊긴 지 오래면 동의를 거둔다.
    pub fn expire(&mut self, now: Instant) {
        if self.enabled && !self.remote_requesting(now) {
            self.stop_reflecting();
        }
    }
}

// ============================================================================
// 송신 경로 스냅샷(엔진 ③c, ADR D1): 워커가 `conn`·`echo` 락 없이 읽는 값
// ============================================================================
//
// 측정(engine-stage3-design.md 10절): 워커가 `conn`·`echo`를 잡으면, 그 락을 쥔 기본 우선순위 스레드(통계·커맨드·
// `band-net-ctrl`)가 선점될 때 워커가 수~수십 ms 멈춘다(`parking_lot`은 우선순위 상속이 없다). 그래서 두 상태를 바꾸는
// 쪽이 락을 쥔 채 원자값을 발행하고(가드의 `Drop` — 락 해제 **전**에 저장하므로 발행 순서가 락 순서와 같다), 워커는
// 원자값만 읽는다.

/// 경로 스냅샷 비트: 주소 IPv4 32 | 포트 16 | 종류 2 | 보낼 수 있음 1 | … | 유효 1(최상위).
const ROUTE_VALID: u64 = 1 << 63;
const ROUTE_SENDABLE: u64 = 1 << 50;

fn kind_bits(k: PathKind) -> u64 {
    match k {
        PathKind::Lan => 0,
        PathKind::Public => 1,
        PathKind::Relay => 2,
    }
}

/// `ConnState`의 송신 경로를 64비트 하나로. 경로가 IPv6이면(지금은 후보·릴레이·STUN 모두 IPv4만 받아 생기지 않는다)
/// 담을 수 없으므로 유효 비트 없이 낸다 — 그때 워커는 `try_lock`으로 직접 보고, 못 잡으면 그 프레임을 그 피어에게 건너뛴다.
pub(crate) fn encode_route(c: &ConnState) -> u64 {
    match (c.can_send_audio(), c.path) {
        (true, Some((SocketAddr::V4(a), kind))) => {
            ROUTE_VALID | ROUTE_SENDABLE | (kind_bits(kind) << 48) | ((a.port() as u64) << 32) | u32::from(*a.ip()) as u64
        }
        (true, Some((SocketAddr::V6(_), _))) => 0,
        _ => ROUTE_VALID,
    }
}

/// 스냅샷 해석: `Some(경로 또는 없음)` = 유효, `None` = 담을 수 없는 경로(락으로 직접 봐야 함).
pub(crate) fn decode_route(v: u64) -> Option<Option<(SocketAddr, PathKind)>> {
    if v & ROUTE_VALID == 0 {
        return None;
    }
    if v & ROUTE_SENDABLE == 0 {
        return Some(None);
    }
    let ip = std::net::Ipv4Addr::from(v as u32);
    let port = (v >> 32) as u16;
    let kind = match (v >> 48) & 3 {
        0 => PathKind::Lan,
        1 => PathKind::Public,
        _ => PathKind::Relay,
    };
    Some(Some((SocketAddr::from((ip, port)), kind)))
}

/// 반사 마감(피어 시계 기준 ns, 0 = 반사 안 함). `EchoState::reflecting(now)` = `enabled && now - t < ECHO_LAPSE`
/// = `now < t + ECHO_LAPSE`이므로 `enabled`일 때 `t + ECHO_LAPSE`를 내면 시각 비교 하나로 같은 답이 나온다
/// (`Instant` 차이는 ns 단위로 정확하다). 시간이 지나 요청이 끊기는 것도 틱 없이 반영된다.
pub(crate) fn echo_deadline_ns(e: &EchoState, born: Instant) -> u64 {
    match (e.enabled, e.remote_request_at) {
        (true, Some(t)) => ((t + ECHO_LAPSE).saturating_duration_since(born).as_nanos() as u64).max(1),
        _ => 0,
    }
}

/// `now`가 반사 마감 전인가(`echo_deadline_ns`의 짝).
pub(crate) fn before_deadline(deadline_ns: u64, now: Instant, born: Instant) -> bool {
    deadline_ns != 0 && (now.saturating_duration_since(born).as_nanos() as u64) < deadline_ns
}

/// `ConnState` 가드: 놓을 때(락 해제 전) 송신 경로 스냅샷을 발행한다. 상태를 바꾸는 모든 곳이 `Peer::conn()`을 거치므로
/// 빠뜨릴 수 없다.
pub(crate) struct ConnGuard<'a> {
    g: MutexGuard<'a, ConnState>,
    snap: &'a AtomicU64,
}

impl std::ops::Deref for ConnGuard<'_> {
    type Target = ConnState;
    fn deref(&self) -> &ConnState {
        &self.g
    }
}

impl std::ops::DerefMut for ConnGuard<'_> {
    fn deref_mut(&mut self) -> &mut ConnState {
        &mut self.g
    }
}

impl Drop for ConnGuard<'_> {
    fn drop(&mut self) {
        // 필드(`g`, 락)는 이 함수가 끝난 뒤에 놓인다 → 락을 쥔 채 저장.
        self.snap.store(encode_route(&self.g), Ordering::Release);
    }
}

/// `EchoState` 가드: 놓을 때(락 해제 전) 반사 마감을 발행한다.
pub(crate) struct EchoGuard<'a> {
    g: MutexGuard<'a, EchoState>,
    snap: &'a AtomicU64,
    born: Instant,
}

impl std::ops::Deref for EchoGuard<'_> {
    type Target = EchoState;
    fn deref(&self) -> &EchoState {
        &self.g
    }
}

impl std::ops::DerefMut for EchoGuard<'_> {
    fn deref_mut(&mut self) -> &mut EchoState {
        &mut self.g
    }
}

impl Drop for EchoGuard<'_> {
    fn drop(&mut self) {
        self.snap.store(echo_deadline_ns(&self.g, self.born), Ordering::Release);
    }
}

/// 원격 손실 보고 셀(엔진 ③c): 쓰는 쪽은 수신 스레드 하나(`set_remote_loss`), 읽는 쪽(워커 200ms, 통계)은 락을 잡지 않는다.
/// seqlock(#116과 같은 꼴): 홀수 버전 → fence(Release) → 필드 → 짝수 Release / 읽기 Acquire → 필드 → fence(Acquire) → 재확인.
#[derive(Default)]
struct RemoteLossCell {
    ver: AtomicU32,
    /// `loss_pct` 비트 << 32 | `conceal_pct` 비트.
    report: AtomicU64,
    /// 받은 시각(피어 시계 ns + 1, 0 = 없음).
    at: AtomicU64,
}

impl RemoteLossCell {
    fn write(&self, r: LossReport, at_ns: u64) {
        let v = self.ver.load(Ordering::Relaxed);
        self.ver.store(v.wrapping_add(1), Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        self.report.store(((r.loss_pct.to_bits() as u64) << 32) | r.conceal_pct.to_bits() as u64, Ordering::Relaxed);
        self.at.store(at_ns.saturating_add(1), Ordering::Relaxed);
        self.ver.store(v.wrapping_add(2), Ordering::Release);
    }

    /// 성공할 때까지 다시 읽는다. 쓰기 구간은 저장 두 번이고 쓰는 쪽(`band-net-recv`)은 MMCSS라 사실상 한두 번이다.
    fn read(&self) -> Option<(LossReport, u64)> {
        let mut tries = 0u32;
        loop {
            let v1 = self.ver.load(Ordering::Acquire);
            if v1 & 1 == 0 {
                let (r, at) = (self.report.load(Ordering::Relaxed), self.at.load(Ordering::Relaxed));
                std::sync::atomic::fence(Ordering::Acquire);
                if self.ver.load(Ordering::Relaxed) == v1 {
                    if at == 0 {
                        return None;
                    }
                    let rep = LossReport { loss_pct: f32::from_bits((r >> 32) as u32), conceal_pct: f32::from_bits(r as u32) };
                    return Some((rep, at - 1));
                }
            }
            tries += 1;
            if tries.is_multiple_of(64) {
                std::thread::yield_now();
            } else {
                std::hint::spin_loop();
            }
        }
    }
}

pub struct Peer {
    id: String,
    ssrc: u32,
    public_key: [u8; KEY_BYTES],
    /// 내 → 피어 송신 암호기.
    tx: PacketCipher,
    /// 피어 → 나 수신 암호기.
    rx: PacketCipher,
    candidates: Vec<(SocketAddr, CandidateKind)>,
    conn: Mutex<ConnState>,
    /// 송信 경로 스냅샷(`encode_route`, `ConnGuard`가 발행). 워커는 이것만 읽는다(엔진 ③c).
    route_snap: AtomicU64,
    /// 반사 마감(`echo_deadline_ns`, `EchoGuard`가 발행). 워커는 이것만 읽는다(엔진 ③c).
    echo_snap: AtomicU64,
    /// 스냅샷 시각의 기준(이 항목을 만든 시각).
    born: Instant,
    /// 피어별 수신 큐 생산자(엔진 ③-a3). `ringbuf` 생산자는 `&mut`가 필요하고 `Peer`는 `Arc`로 공유되므로 Mutex로
    /// 감싼다. 넣는 스레드는 `band-net-recv` 하나뿐이라(`push_audio`는 `recv_loop` 경로에서만) 경쟁자가 없다(설계 1.2절).
    media: Mutex<MediaTx>,
    /// `media.key()`의 복사본(락 없이 읽게).
    key: PeerKey,
    /// 워커가 발행하는 이 피어 스트림의 수신 손실 보고(Ping, P1-4e). `media` 락을 잡지 않고 읽게 따로 쥔다.
    rx_report: Arc<LossCell>,
    /// 이 피어로 보내는 Audio 패킷 seq(송신 키별). 에코로 되돌리는 패킷도 같은 카운터를 써서 nonce가 겹치지 않는다.
    /// 같은 키를 쓰는 살아 있는 항목(다른 peerId가 같은 descriptor를 보냄)끼리는 이 카운터를 같이 쓴다(`Net::add_peer`).
    tx_seq: Arc<AtomicU32>,
    /// 이 피어로 마지막에 보낸 Audio 타임스탬프(에코 시작 시 타임스탬프를 이어 붙이는 데 쓴다).
    tx_last_ts: AtomicU32,
    /// 이 피어로 마지막에 보낸 내 오디오의 코덱(`TX_CODEC_*` 비트, 0 = 아직 안 보냄). PeerStats.txCodec.
    tx_codec: AtomicU8,
    echo: Mutex<EchoState>,
    /// 이 피어 시계와 내 시계의 오프셋 추정(P2-3, Pong 평문의 응답자 시계로 갱신).
    clock: Mutex<super::clock::ClockSync>,
    /// 이 피어가 Ping으로 알려 온, 내 소리의 수신 손실 보고와 받은 시각(P1-4e). 손실 피드백 Opus 전환의 입력.
    remote_loss: RemoteLossCell,
}

/// `Peer::tx_codec` 비트: 보낸 적 있음 / Opus / 릴레이 대역 때문에 자동 전환한 Opus(P1-4c) /
/// 수신 손실 피드백으로 자동 전환한 Opus(P1-4e). AUTO와 AUTO_LOSS가 둘 다 서면 이유는 릴레이 부하로 보인다.
pub const TX_CODEC_SENT: u8 = 0x80;
pub const TX_CODEC_OPUS: u8 = 0x01;
pub const TX_CODEC_AUTO: u8 = 0x02;
pub const TX_CODEC_AUTO_LOSS: u8 = 0x04;

/// `PeerStats`의 net 몫(상태·경로·RTT·에코·송신 코덱 비트·원격 손실 보고)을 값으로 담는다.
#[derive(Clone, Copy, Debug)]
pub struct PeerNetStats {
    pub state: PeerStateKind,
    pub path: Option<PathKind>,
    pub rtt_ms: Option<f32>,
    pub echo_requested: bool,
    pub echo_active: bool,
    pub peer_echoing: bool,
    /// `TX_CODEC_*` 비트.
    pub tx_codec: u8,
    pub remote_loss: Option<(LossReport, Instant)>,
    /// add_peer → 처음 Connected(ms, P1-20). 아직 연결 전이면 None.
    pub connect_ms: Option<f32>,
}

pub struct PeerSetup<'a> {
    pub keys: &'a KeyPair,
    pub my_ssrc: u32,
    pub allow_loopback: bool,
}

impl Peer {
    /// `media`: 엔진이 만든 이 피어의 수신 큐 생산자(`media_link`).
    pub fn new(id: String, desc: &LocalDescriptor, setup: &PeerSetup<'_>, media: MediaTx, now: Instant) -> Result<Self, String> {
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
            route_snap: AtomicU64::new(encode_route(&ConnState::new(now))),
            echo_snap: AtomicU64::new(0),
            born: now,
            conn: Mutex::new(ConnState::new(now)),
            rx_report: media.loss_cell().clone(),
            key: media.key(),
            media: Mutex::new(media),
            tx_seq: Arc::new(AtomicU32::new(rand::random())),
            tx_last_ts: AtomicU32::new(0),
            tx_codec: AtomicU8::new(0),
            echo: Mutex::new(EchoState::default()),
            clock: Mutex::new(super::clock::ClockSync::new()),
            remote_loss: RemoteLossCell::default(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    pub fn public_key(&self) -> &[u8; KEY_BYTES] {
        &self.public_key
    }

    /// 오디오를 보낼 수 있으면 지금 경로(주소, 종류). 락 없이 스냅샷에서 읽는다(엔진 ③c — 워커 송신 경로·자동 코덱).
    /// 스냅샷에 담을 수 없는 경로(IPv6, 지금은 생기지 않음)면 `try_lock`으로 직접 보고, 락이 바쁘면 기다리지 않고 None.
    pub fn route(&self) -> Option<(SocketAddr, PathKind)> {
        self.send_route().unwrap_or(None)
    }

    /// `route`와 같되, 담을 수 없는 경로인데 `conn` 락이 바빠 못 본 경우를 `Err`로 구분한다(송신 경로가 건너뛴 수를 센다).
    pub(super) fn send_route(&self) -> Result<Option<(SocketAddr, PathKind)>, ()> {
        match decode_route(self.route_snap.load(Ordering::Acquire)) {
            Some(r) => Ok(r),
            None => self.conn.try_lock().map(|c| if c.can_send_audio() { c.path } else { None }).ok_or(()),
        }
    }

    /// 지금 오디오를 릴레이로 보내는 중인가(자동 코덱·릴레이 부하 계산).
    pub fn sends_audio_via_relay(&self) -> bool {
        self.route().is_some_and(|(_, k)| k == PathKind::Relay)
    }

    /// 이 피어가 Ping으로 알려 온 내 소리의 수신 손실 보고와 받은 시각(락 없음, 엔진 ③c).
    pub fn remote_loss(&self) -> Option<(LossReport, Instant)> {
        self.remote_loss.read().map(|(r, ns)| (r, self.born + Duration::from_nanos(ns)))
    }

    /// 지금 이 피어의 오디오를 되돌려 보내는 중인가(에코 반사). 락 없이 반사 마감과 비교한다(엔진 ③c) —
    /// `EchoState::reflecting(now)`과 같은 답(`echo_deadline_ns`).
    pub fn reflecting(&self, now: Instant) -> bool {
        before_deadline(self.echo_snap.load(Ordering::Acquire), now, self.born)
    }

    /// `PeerStats`의 net 몫을 값으로.
    pub fn net_stats(&self, now: Instant) -> PeerNetStats {
        let (state, path, rtt_ms, connect_ms) = {
            let c = self.conn.lock();
            let connect_ms = c.connected_at.map(|t| t.saturating_duration_since(c.started).as_secs_f32() * 1000.0);
            (c.state, c.path.map(|(_, k)| k), c.rtt_ms, connect_ms)
        };
        let (echo_requested, echo_active, peer_echoing) = {
            let e = self.echo.lock();
            (e.remote_requesting(now) && !e.enabled, e.reflecting(now), e.remote_echoing(now))
        };
        PeerNetStats {
            state,
            path,
            rtt_ms,
            echo_requested,
            echo_active,
            peer_echoing,
            tx_codec: self.tx_codec.load(Ordering::Relaxed),
            remote_loss: self.remote_loss(),
            connect_ms,
        }
    }

    /// 이 피어 미디어 상태의 키(엔진 등록부·워커와 짝짓는 값).
    pub fn key(&self) -> PeerKey {
        self.key
    }

    /// 수신 오디오를 피어별 수신 큐에 넣는다(가득 차면 버리고 센다). `recv_loop` 경로(`handle_packet`)에서만 부른다 —
    /// 다른 스레드가 부르면 SPSC가 Mutex로 직렬화될 뿐 깨지지는 않지만 무경쟁 가정이 깨진다(③-a3 리뷰 항목).
    pub(super) fn push_audio(&self, hdr: &super::packet::Header, payload: &[u8], arrival_us: u64) {
        self.media.lock().push(hdr, payload, arrival_us);
    }

    /// 이 피어 스트림의 수신 손실 보고(재생 중이 아니면 None). Ping에 싣는다(P1-4e).
    pub(super) fn loss_report(&self) -> Option<LossReport> {
        self.rx_report.get().map(|(loss_pct, conceal_pct)| LossReport { loss_pct, conceal_pct })
    }

    pub(super) fn tx(&self) -> &PacketCipher {
        &self.tx
    }

    pub(super) fn rx(&self) -> &PacketCipher {
        &self.rx
    }

    pub(super) fn candidates(&self) -> &[(SocketAddr, CandidateKind)] {
        &self.candidates
    }

    /// 상태를 바꾸는 곳은 모두 이 가드를 거친다 — 놓을 때 송신 경로 스냅샷을 발행한다(엔진 ③c).
    pub(super) fn conn(&self) -> ConnGuard<'_> {
        ConnGuard { g: self.conn.lock(), snap: &self.route_snap }
    }

    /// 상태를 바꾸는 곳은 모두 이 가드를 거친다 — 놓을 때 반사 마감을 발행한다(엔진 ③c).
    pub(super) fn echo(&self) -> EchoGuard<'_> {
        EchoGuard { g: self.echo.lock(), snap: &self.echo_snap, born: self.born }
    }

    pub(super) fn clock(&self) -> MutexGuard<'_, super::clock::ClockSync> {
        self.clock.lock()
    }

    pub(super) fn set_remote_loss(&self, r: LossReport, at: Instant) {
        self.remote_loss.write(r, at.saturating_duration_since(self.born).as_nanos() as u64);
    }

    pub(super) fn tx_seq(&self) -> &AtomicU32 {
        &self.tx_seq
    }

    pub(super) fn tx_last_ts(&self) -> &AtomicU32 {
        &self.tx_last_ts
    }

    pub(super) fn tx_codec(&self) -> &AtomicU8 {
        &self.tx_codec
    }

    /// 같은 송신 키를 쓰는 살아 있는 항목과 seq 카운터를 같이 쓴다(`Net::add_peer`, 공유 전 단계).
    pub(super) fn share_tx_seq(&mut self, live: &Peer) {
        self.tx_seq = live.tx_seq.clone();
    }

    /// 테스트용: 경로를 강제로 세운다(핸드셰이크 없이).
    #[doc(hidden)]
    pub fn force_path(&self, addr: SocketAddr, kind: PathKind) {
        let mut c = self.conn();
        c.state = PeerStateKind::Connected;
        c.path = Some((addr, kind));
        c.last_rx = Some(Instant::now());
    }

    /// 테스트용: 두 항목이 송신 seq 카운터를 같이 쓰는가.
    #[doc(hidden)]
    pub fn shares_tx_seq_with(&self, other: &Peer) -> bool {
        Arc::ptr_eq(&self.tx_seq, &other.tx_seq)
    }

    /// 직접 Hello를 보낼 주소들: 모든 후보 + (다르면) 현재 직접 경로 + 리바인딩 후보 주소. 최대 10개.
    /// 릴레이 주소는 빠진다(릴레이로는 RelayData로 따로 보낸다).
    pub fn hello_targets(&self, relay: Option<SocketAddr>) -> Vec<SocketAddr> {
        let mut v: Vec<SocketAddr> = self.candidates.iter().map(|(a, _)| *a).collect();
        let c = self.conn.lock();
        let direct_path = c.path.filter(|(_, k)| *k != PathKind::Relay).map(|(a, _)| a);
        for extra in [direct_path, c.rebind.map(|(a, _)| a)].into_iter().flatten() {
            if !v.contains(&extra) {
                v.push(extra);
            }
        }
        v.retain(|a| Some(*a) != relay);
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
        c.on_handshake("203.0.113.9:5000".parse().unwrap(), false, &cands(), None, t0);
        assert_eq!(c.path.unwrap().1, PathKind::Public);
        assert_eq!(c.state, PeerStateKind::Connected);
        c.on_handshake("192.168.0.5:4000".parse().unwrap(), false, &cands(), None, t0);
        assert_eq!(c.path.unwrap().1, PathKind::Lan);
        // LAN 확정 후 public 응답은 무시
        assert_eq!(c.on_handshake("203.0.113.9:5000".parse().unwrap(), false, &cands(), None, t0), HandshakeOutcome::Ignored);
        assert_eq!(c.path.unwrap().1, PathKind::Lan);
        // 후보에 없는 주소(peer-reflexive)는 public 취급
        let mut c = ConnState::new(t0);
        c.on_handshake("198.51.100.1:7".parse().unwrap(), false, &cands(), None, t0);
        assert_eq!(c.path.unwrap().1, PathKind::Public);
    }

    /// P1-20 `connectMs`: 처음 Connected가 된 시각만 남고, 끊김 → 재연결·경로 승격에도 바뀌지 않는다.
    #[test]
    fn connected_at_is_first_connect_only() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        assert_eq!(c.connected_at, None);
        let public: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        let t1 = t0 + Duration::from_millis(120);
        c.on_handshake(public, false, &cands(), None, t1);
        assert_eq!(c.connected_at, Some(t1));
        // LAN 승격
        c.on_handshake("192.168.0.5:4000".parse().unwrap(), false, &cands(), None, t1 + Duration::from_millis(50));
        assert_eq!(c.connected_at, Some(t1));
        // 끊김 → 재연결
        c.tick(t1 + Duration::from_secs(6), false);
        assert_eq!(c.state, PeerStateKind::Reconnecting);
        c.on_handshake("192.168.0.5:4000".parse().unwrap(), false, &cands(), None, t1 + Duration::from_secs(7));
        assert_eq!(c.state, PeerStateKind::Connected);
        assert_eq!(c.connected_at, Some(t1));
    }

    #[test]
    fn hello_from_other_address_does_not_refresh_liveness() {
        // 리뷰 M6: 경로가 아닌 주소의 Hello는 last_rx/상태를 갱신하지 않는다
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let path: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        let other: SocketAddr = "198.51.100.1:7".parse().unwrap();
        c.on_handshake(path, false, &cands(), None, t0);
        let t1 = t0 + Duration::from_secs(3);
        assert_eq!(c.on_handshake(other, false, &cands(), None, t1), HandshakeOutcome::Ignored);
        assert_eq!(c.last_rx, Some(t0), "last_rx 유지");
        // 5초가 지나면 끊김으로 판정되어야 한다(가짜 Hello로 살아 있는 척 불가)
        c.tick(t0 + Duration::from_secs(5), false);
        assert_eq!(c.state, PeerStateKind::Reconnecting);
    }

    #[test]
    fn nat_rebinding_switches_after_consecutive_hellos() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let old: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        let new: SocketAddr = "203.0.113.9:6000".parse().unwrap();
        c.on_handshake(old, false, &cands(), None, t0);
        for i in 1..REBIND_HELLOS {
            assert_eq!(c.on_handshake(new, false, &cands(), None, t0), HandshakeOutcome::Ignored, "{i}");
        }
        assert_eq!(c.on_handshake(new, false, &cands(), None, t0), HandshakeOutcome::Accepted);
        assert_eq!(c.path.unwrap().0, new);
        // 중간에 원래 경로에서 오면 카운트 초기화
        let mut c = ConnState::new(t0);
        c.on_handshake(old, false, &cands(), None, t0);
        for _ in 0..REBIND_HELLOS - 1 {
            c.on_handshake(new, false, &cands(), None, t0);
        }
        c.on_handshake(old, false, &cands(), None, t0);
        assert_eq!(c.on_handshake(new, false, &cands(), None, t0), HandshakeOutcome::Ignored);
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
        let (a, b): (SocketAddr, SocketAddr) = ("192.168.0.5:4000".parse().unwrap(), "203.0.113.9:5000".parse().unwrap());
        assert!(c.ack_allowed(a, t0));
        assert!(!c.ack_allowed(a, t0 + Duration::from_millis(10)));
        // 다른 주소(두 번째 후보 경로)는 굶지 않는다(PR #5 L-4)
        assert!(c.ack_allowed(b, t0 + Duration::from_millis(10)));
        assert!(c.ack_allowed(a, t0 + Duration::from_millis(60)));
        // 주소 수가 많아도 기억 공간은 제한된다
        for i in 0..50u16 {
            c.ack_allowed(SocketAddr::from(([198, 51, 100, 1], 1000 + i)), t0);
        }
        assert!(c.last_acks.len() <= MAX_CANDIDATES + 2);
    }

    #[test]
    fn rebind_count_reset_by_path_traffic() {
        // PR #5 L-3: 경로에서 Ping/Audio가 오면 리바인딩 카운트가 지워져 "연속 5회"가 유지된다
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let old: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        let new: SocketAddr = "203.0.113.9:6000".parse().unwrap();
        c.on_handshake(old, false, &cands(), None, t0);
        for _ in 0..REBIND_HELLOS - 1 {
            c.on_handshake(new, false, &cands(), None, t0);
            c.on_rx_from_path(t0);
        }
        assert_eq!(c.on_handshake(new, false, &cands(), None, t0), HandshakeOutcome::Ignored, "누적이 아니라 연속이어야 함");
        assert_eq!(c.path.unwrap().0, old);
    }

    #[test]
    fn replay_window_inherited() {
        let t0 = Instant::now();
        let mut old = ConnState::new(t0);
        assert!(old.accept_ctrl_seq(PacketType::Bye, 100));
        let mut fresh = ConnState::new(t0);
        fresh.inherit_replay_window(&old);
        assert!(!fresh.accept_ctrl_seq(PacketType::Bye, 100), "재추가 전에 캡처한 Bye 재전송 거부(PR #5 L-2)");
        assert!(fresh.accept_ctrl_seq(PacketType::Bye, 101));
    }

    #[test]
    fn state_machine_timeouts() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        assert_eq!(c.tick(t0, false), TickAction::HELLO);
        assert_eq!(c.tick(t0 + Duration::from_millis(50), false), TickAction::NONE);
        assert_eq!(c.tick(t0 + Duration::from_millis(100), false), TickAction::HELLO);
        assert_eq!(c.tick(t0 + Duration::from_secs(10), false), TickAction::NONE);
        assert_eq!(c.state, PeerStateKind::Failed);

        let mut c = ConnState::new(t0);
        c.on_handshake("192.168.0.5:4000".parse().unwrap(), false, &cands(), None, t0);
        assert_eq!(c.tick(t0, false), TickAction::PING);
        assert_eq!(c.tick(t0 + Duration::from_millis(400), false), TickAction::NONE);
        assert_eq!(c.tick(t0 + Duration::from_millis(500), false), TickAction::PING);
        let t5 = t0 + Duration::from_secs(5);
        assert_eq!(c.tick(t5, false), TickAction::HELLO);
        assert_eq!(c.state, PeerStateKind::Reconnecting);
        assert!(c.can_send_audio());
        c.on_rx_from_path(t5 + Duration::from_secs(1));
        assert_eq!(c.state, PeerStateKind::Connected);
        let t_lost = t5 + Duration::from_secs(6);
        c.tick(t_lost, false);
        assert_eq!(c.state, PeerStateKind::Reconnecting);
        c.tick(t5 + Duration::from_secs(16), false);
        assert_eq!(c.state, PeerStateKind::Failed);
        // Failed 뒤에도 2초마다 탐색(늦게 열린 릴레이/경로로 회복). Bye를 받았으면 하지 않는다.
        let tf = t5 + Duration::from_secs(16);
        assert_eq!(c.tick(tf + FAILED_PROBE_INTERVAL, true), TickAction { hello_direct: true, hello_relay: true, ping: false });
        c.bye = true;
        assert_eq!(c.tick(tf + FAILED_PROBE_INTERVAL * 3, true), TickAction::NONE);
    }

    const RELAY: &str = "198.51.100.200:3478";

    fn relay() -> Option<SocketAddr> {
        Some(RELAY.parse().unwrap())
    }

    #[test]
    fn relay_hello_after_fallback_delay_only_when_ready() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        assert_eq!(c.tick(t0, true), TickAction::HELLO, "처음에는 직접만");
        let tf = t0 + RELAY_FALLBACK_AFTER;
        assert_eq!(c.tick(tf, false), TickAction::HELLO, "릴레이 미등록이면 직접만");
        let a = c.tick(tf + HELLO_INTERVAL, true);
        assert!(a.hello_direct && a.hello_relay, "{a:?}");
    }

    #[test]
    fn relay_path_established_and_direct_promoted_by_ack() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let r: SocketAddr = RELAY.parse().unwrap();
        let lan: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        assert_eq!(c.on_handshake(r, false, &cands(), relay(), t0), HandshakeOutcome::Accepted);
        assert_eq!(c.path, Some((r, PathKind::Relay)));
        assert!(c.relay_confirmed);
        // 릴레이 경로에서는 직접 경로를 1초마다 계속 찾는다
        let a = c.tick(t0, true);
        assert!(a.ping && a.hello_direct && !a.hello_relay, "{a:?}");
        assert!(!c.tick(t0 + Duration::from_millis(500), true).hello_direct);
        assert!(c.tick(t0 + DIRECT_PROBE_INTERVAL, true).hello_direct);
        // 직접 주소의 Hello만으로는 승격하지 않는다(왕복 미확인) — 힌트만
        assert_eq!(c.on_handshake(lan, false, &cands(), relay(), t0), HandshakeOutcome::Ignored);
        assert_eq!(c.path_kind(), Some(PathKind::Relay));
        assert_eq!(c.direct_hint, Some(lan));
        // HelloAck(왕복 확인)가 1초 간격으로 3번, 2초 이상 이어져야 승격(M2 히스테리시스)
        assert_eq!(c.on_handshake(lan, true, &cands(), relay(), t0), HandshakeOutcome::Ignored);
        assert_eq!(c.on_handshake(lan, true, &cands(), relay(), t0 + Duration::from_secs(1)), HandshakeOutcome::Ignored);
        assert_eq!(c.path_kind(), Some(PathKind::Relay));
        let tp = t0 + Duration::from_secs(2);
        assert_eq!(c.on_handshake(lan, true, &cands(), relay(), tp), HandshakeOutcome::Accepted);
        assert_eq!(c.path, Some((lan, PathKind::Lan)));
        // 비행 중인 릴레이 패킷은 계속 받는다(무끊김), 생존은 경로 기준
        assert!(c.accept_rx(r, &cands(), relay(), tp + Duration::from_millis(10)));
        assert!(!c.accept_rx("198.51.100.9:1".parse().unwrap(), &cands(), relay(), tp), "모르는 주소");
    }

    #[test]
    fn promotion_requires_unbroken_acks_and_acceptable_rtt() {
        let t0 = Instant::now();
        let r: SocketAddr = RELAY.parse().unwrap();
        let lan: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        let s = Duration::from_secs(1);
        // 중간에 2초 끊기면(손실) 처음부터 다시 센다
        let mut c = ConnState::new(t0);
        c.on_handshake(r, false, &cands(), relay(), t0);
        c.on_handshake(lan, true, &cands(), relay(), t0);
        c.on_handshake(lan, true, &cands(), relay(), t0 + s);
        assert_eq!(c.on_handshake(lan, true, &cands(), relay(), t0 + s * 3), HandshakeOutcome::Ignored, "간격 2초 → 리셋");
        c.on_handshake(lan, true, &cands(), relay(), t0 + s * 4);
        assert_eq!(c.on_handshake(lan, true, &cands(), relay(), t0 + s * 5), HandshakeOutcome::Accepted);
        // 직접 RTT(80ms)가 릴레이 종단 RTT(40ms) + 10ms보다 크면 승격하지 않는다
        let mut c = ConnState::new(t0);
        c.on_handshake(r, false, &cands(), relay(), t0);
        c.rtt_ms = Some(40.0);
        for i in 0..5 {
            assert_eq!(c.on_handshake_rtt(lan, true, &cands(), relay(), t0 + s * i, Some(80.0)), HandshakeOutcome::Ignored);
        }
        // 직접이 더 빠르면 승격하고, RTT는 직접 경로 값으로 바뀐다
        let mut c = ConnState::new(t0);
        c.on_handshake(r, false, &cands(), relay(), t0);
        c.rtt_ms = Some(40.0);
        for i in 0..2 {
            c.on_handshake_rtt(lan, true, &cands(), relay(), t0 + s * i, Some(4.0));
        }
        assert_eq!(c.on_handshake_rtt(lan, true, &cands(), relay(), t0 + s * 2, Some(4.0)), HandshakeOutcome::Accepted);
        assert_eq!(c.rtt_ms, Some(4.0));
    }

    /// 직접 경로 승격까지(1초 간격 HelloAck 3회). 승격 시각을 돌려준다.
    fn promote(c: &mut ConnState, addr: SocketAddr, from: Instant) -> Instant {
        let mut t = from;
        for _ in 0..PROMOTE_ACKS {
            c.on_handshake(addr, true, &cands(), relay(), t);
            t += Duration::from_secs(1);
        }
        t - Duration::from_secs(1)
    }

    #[test]
    fn demotion_holds_promotion_and_backs_off_on_flapping() {
        let t0 = Instant::now();
        let r: SocketAddr = RELAY.parse().unwrap();
        let lan: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        let mut c = ConnState::new(t0);
        c.on_handshake(lan, false, &cands(), relay(), t0);
        // 처음 강등(직접 → 릴레이): 5초 동안 승격 보류
        let td = t0 + Duration::from_secs(60);
        c.on_handshake(r, false, &cands(), relay(), td);
        assert_eq!(c.path_kind(), Some(PathKind::Relay));
        assert_eq!(c.promote_hold_until, Some(td + RELAY_HOLD_MIN));
        let tp = promote(&mut c, lan, td + Duration::from_secs(1));
        assert_eq!(c.path_kind(), Some(PathKind::Relay), "유예 중에는 HelloAck가 이어져도 승격 안 함 ({tp:?})");
        let tp = promote(&mut c, lan, td + RELAY_HOLD_MIN);
        assert_eq!(c.path_kind(), Some(PathKind::Lan), "유예 뒤 승격");
        // 승격 후 30초 안에 다시 강등(플래핑) → 유예 10초, 다음은 20초 … 최대 60초
        let mut t = tp + Duration::from_secs(3);
        let mut holds = vec![];
        for _ in 0..5 {
            c.on_handshake(r, false, &cands(), relay(), t);
            holds.push(c.promote_hold_until.unwrap() - t);
            let hold_end = c.promote_hold_until.unwrap();
            let tp = promote(&mut c, lan, hold_end);
            assert_eq!(c.path_kind(), Some(PathKind::Lan));
            t = tp + Duration::from_secs(3);
        }
        let secs: Vec<u64> = holds.iter().map(|d| d.as_secs()).collect();
        assert_eq!(secs, vec![10, 20, 40, 60, 60]);
        // 승격이 30초 넘게 안정적이었으면 처음(5초)부터
        let t_late = t + FLAP_WINDOW;
        c.on_handshake(r, false, &cands(), relay(), t_late);
        assert_eq!(c.promote_hold_until, Some(t_late + RELAY_HOLD_MIN));
        assert_eq!(c.flaps, 0);
    }

    #[test]
    fn follows_peer_promotion_even_during_hold() {
        // 상대가 먼저 승격해 직접 경로로 보내면 유예 중이어도 따라간다(안 그러면 그 소리를 버린다)
        let t0 = Instant::now();
        let r: SocketAddr = RELAY.parse().unwrap();
        let lan: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        let mut c = ConnState::new(t0);
        c.on_handshake(lan, false, &cands(), relay(), t0);
        c.on_handshake(r, false, &cands(), relay(), t0 + Duration::from_secs(1));
        assert!(c.promote_hold_until.is_some());
        c.on_handshake(lan, false, &cands(), relay(), t0 + Duration::from_secs(2));
        assert!(c.accept_rx(lan, &cands(), relay(), t0 + Duration::from_secs(2)));
        assert_eq!(c.path_kind(), Some(PathKind::Lan));
    }

    #[test]
    fn relay_address_change_moves_relay_path() {
        let t0 = Instant::now();
        let r: SocketAddr = RELAY.parse().unwrap();
        let r2: SocketAddr = "198.51.100.201:3478".parse().unwrap();
        let mut c = ConnState::new(t0);
        c.on_handshake(r, false, &cands(), relay(), t0);
        c.relay_moved(r, Some(r2), t0);
        assert_eq!(c.path, Some((r2, PathKind::Relay)), "새 릴레이로 바로 옮긴다(Reconnecting 없이)");
        assert_eq!(c.state, PeerStateKind::Connected);
        assert!(c.accept_rx(r2, &cands(), Some(r2), t0));
        assert!(c.accept_rx(r, &cands(), Some(r2), t0 + Duration::from_secs(1)), "옛 릴레이의 비행 중 패킷");
        // 해제: 경로를 비우고 직접 경로를 다시 찾는다
        c.relay_moved(r2, None, t0);
        assert_eq!(c.path, None);
        assert_eq!(c.state, PeerStateKind::Reconnecting);
        assert!(!c.relay_confirmed);
        assert!(c.tick(t0 + Duration::from_millis(200), false).hello_direct);
        // 직접 경로 피어는 그대로
        let lan: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        let mut d = ConnState::new(t0);
        d.on_handshake(lan, false, &cands(), relay(), t0);
        d.relay_moved(r, Some(r2), t0);
        assert_eq!(d.path, Some((lan, PathKind::Lan)));
    }

    #[test]
    fn follows_peer_promotion_via_direct_hint() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let r: SocketAddr = RELAY.parse().unwrap();
        let public: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        c.on_handshake(r, true, &cands(), relay(), t0);
        assert!(!c.accept_rx(public, &cands(), relay(), t0), "힌트 없으면 직접 주소 패킷 폐기");
        c.on_handshake(public, false, &cands(), relay(), t0);
        // 상대가 승격해 그 주소로 Ping/Audio를 보내면 따라 승격
        assert!(c.accept_rx(public, &cands(), relay(), t0));
        assert_eq!(c.path, Some((public, PathKind::Public)));
        assert_eq!(c.prev_path.map(|(a, _)| a), Some(r));
    }

    #[test]
    fn falls_back_to_relay_when_direct_goes_quiet() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        let r: SocketAddr = RELAY.parse().unwrap();
        let lan: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        c.on_handshake(lan, false, &cands(), relay(), t0);
        assert!(!c.tick(t0 + Duration::from_secs(1), true).hello_relay, "직접 경로가 살아 있으면 릴레이 안 씀");
        let tq = t0 + RELAY_PROBE_AFTER;
        assert!(c.tick(tq, true).hello_relay, "조용해지면 릴레이로도 Hello");
        assert!(!c.tick(tq, false).hello_relay);
        // 상대의 릴레이 Hello/HelloAck → 릴레이 경로로 전환, 직전 직접 경로는 잠시 수용
        assert_eq!(c.on_handshake(r, false, &cands(), relay(), tq), HandshakeOutcome::Accepted);
        assert_eq!(c.path_kind(), Some(PathKind::Relay));
        assert!(c.accept_rx(lan, &cands(), relay(), tq + Duration::from_secs(1)));
        assert!(!c.accept_rx(lan, &cands(), relay(), tq + PATH_GRACE), "유예 이후 폐기");
    }

    #[test]
    fn relay_data_without_relay_handshake_rejected() {
        // 릴레이를 거친 Hello/HelloAck가 없었던 피어는 릴레이 주소에서 온 데이터를 받지 않는다
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        c.on_handshake("192.168.0.5:4000".parse().unwrap(), false, &cands(), relay(), t0);
        assert!(!c.accept_rx(RELAY.parse().unwrap(), &cands(), relay(), t0));
    }

    fn test_peer() -> Peer {
        let keys = KeyPair::generate();
        let other = KeyPair::generate();
        let desc = LocalDescriptor {
            ssrc: 9,
            key: LocalDescriptor::encode_key(&other.public_bytes()),
            candidates: vec![Candidate { kind: CandidateKind::Lan, addr: "192.168.0.5:4000".into() }],
        };
        let setup = PeerSetup { keys: &keys, my_ssrc: 1, allow_loopback: false };
        let (media, _rx) = super::super::media_link::media_link(PeerKey::from_raw(1));
        Peer::new("p".into(), &desc, &setup, media, Instant::now()).unwrap()
    }

    /// 엔진 ③c: 경로 스냅샷 왕복 — IPv4 경로는 그대로, 보낼 수 없으면 없음, IPv6이면 유효 비트 없음.
    #[test]
    fn route_snapshot_roundtrip() {
        let t0 = Instant::now();
        let mut c = ConnState::new(t0);
        assert_eq!(decode_route(encode_route(&c)), Some(None), "처음엔 경로 없음(유효)");
        for (addr, kind) in [("192.168.0.5:4000", PathKind::Lan), ("203.0.113.9:65535", PathKind::Public), ("198.51.100.1:1", PathKind::Relay)] {
            c.state = PeerStateKind::Connected;
            c.path = Some((addr.parse().unwrap(), kind));
            assert_eq!(decode_route(encode_route(&c)), Some(Some((addr.parse().unwrap(), kind))));
            c.state = PeerStateKind::Reconnecting;
            assert_eq!(decode_route(encode_route(&c)), Some(Some((addr.parse().unwrap(), kind))), "재연결 중에도 보낸다");
            c.state = PeerStateKind::Failed;
            assert_eq!(decode_route(encode_route(&c)), Some(None), "실패면 안 보낸다");
        }
        c.state = PeerStateKind::Connected;
        c.path = Some(("[2001:db8::1]:4000".parse().unwrap(), PathKind::Public));
        assert_eq!(decode_route(encode_route(&c)), None, "IPv6은 담을 수 없다 → 락으로 직접");
    }

    /// 엔진 ③c: `conn()`·`echo()` 가드가 놓을 때 스냅샷을 발행한다 — 락 없는 `route()`·`reflecting()`이 바로 본다.
    /// 담을 수 없는 경로(IPv6)는 `try_lock`으로 보고, 락이 바쁘면 기다리지 않고 `Err`(건너뜀).
    #[test]
    fn guards_publish_snapshots() {
        let p = test_peer();
        assert_eq!(p.route(), None);
        let a: SocketAddr = "192.168.0.5:4000".parse().unwrap();
        p.force_path(a, PathKind::Lan);
        assert_eq!(p.route(), Some((a, PathKind::Lan)));
        p.conn().state = PeerStateKind::Failed;
        assert_eq!(p.route(), None);
        // IPv6: 스냅샷 무효 → try_lock 경로
        let v6: SocketAddr = "[2001:db8::1]:4000".parse().unwrap();
        p.force_path(v6, PathKind::Public);
        assert_eq!(p.send_route(), Ok(Some((v6, PathKind::Public))));
        let held = p.conn.lock();
        assert_eq!(p.send_route(), Err(()), "락이 바쁘면 기다리지 않는다");
        assert_eq!(p.route(), None);
        drop(held);
        // 에코: 요청 + 동의 → 반사, 동의 거두면 즉시 아님
        let now = Instant::now();
        assert!(!p.reflecting(now));
        {
            let mut e = p.echo();
            e.remote_request_at = Some(now);
            e.enabled = true;
        }
        assert!(p.reflecting(now));
        assert!(p.reflecting(now + ECHO_LAPSE - Duration::from_nanos(1)));
        assert!(!p.reflecting(now + ECHO_LAPSE), "요청이 끊기면 틱 없이도 반사가 멈춘다");
        p.echo().stop_reflecting();
        assert!(!p.reflecting(now));
    }

    /// 엔진 ③c: 반사 마감 비교가 `EchoState::reflecting(now)`과 같은 답 — 동의 여부, 요청 시각(기준 전·후), 확인 시각을
    /// 경계(마감 −1ns, 마감, +1ns)와 그 사이에서 모두 비교한다.
    #[test]
    fn echo_deadline_matches_reflecting() {
        let born = Instant::now();
        let ns = Duration::from_nanos;
        for enabled in [false, true] {
            for req_off in [None, Some(0u64), Some(1), Some(1_500_000_000), Some(7_000_000_123)] {
                let e = EchoState { enabled, remote_request_at: req_off.map(|o| born + ns(o)), ..EchoState::default() };
                let dl = echo_deadline_ns(&e, born);
                let mut probes = vec![0u64, 1, 2_999_999_999, 3_000_000_000, 10_000_000_000];
                if let Some(o) = req_off {
                    let edge = o + ECHO_LAPSE.as_nanos() as u64;
                    probes.extend([o.saturating_sub(5), o, edge - 1, edge, edge + 1]);
                }
                for q in probes {
                    let now = born + ns(q);
                    assert_eq!(before_deadline(dl, now, born), e.reflecting(now), "enabled {enabled} req {req_off:?} now +{q}ns");
                }
            }
        }
    }

    /// 엔진 ③c: 원격 손실 보고 셀 — 없음 → 쓴 값·시각 그대로, 덮어쓰기.
    #[test]
    fn remote_loss_cell_roundtrip() {
        let p = test_peer();
        assert_eq!(p.remote_loss(), None);
        let at = p.born + Duration::from_millis(1234);
        p.set_remote_loss(LossReport { loss_pct: 2.5, conceal_pct: 1.25 }, at);
        assert_eq!(p.remote_loss(), Some((LossReport { loss_pct: 2.5, conceal_pct: 1.25 }, at)));
        let at2 = at + Duration::from_nanos(7);
        p.set_remote_loss(LossReport { loss_pct: 0.0, conceal_pct: 9.0 }, at2);
        assert_eq!(p.remote_loss(), Some((LossReport { loss_pct: 0.0, conceal_pct: 9.0 }, at2)));
    }

    #[test]
    fn hello_targets_exclude_relay() {
        let keys = KeyPair::generate();
        let other = KeyPair::generate();
        let desc = LocalDescriptor {
            ssrc: 9,
            key: LocalDescriptor::encode_key(&other.public_bytes()),
            candidates: vec![
                Candidate { kind: CandidateKind::Lan, addr: "192.168.0.5:4000".into() },
                Candidate { kind: CandidateKind::Public, addr: RELAY.into() },
            ],
        };
        let setup = PeerSetup { keys: &keys, my_ssrc: 1, allow_loopback: false };
        let (media, _rx) = super::super::media_link::media_link(PeerKey::from_raw(1));
        let p = Peer::new("p".into(), &desc, &setup, media, Instant::now()).unwrap();
        p.conn.lock().on_handshake(RELAY.parse().unwrap(), false, &p.candidates, relay(), Instant::now());
        assert_eq!(p.hello_targets(relay()), vec!["192.168.0.5:4000".parse::<SocketAddr>().unwrap()]);
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

    #[test]
    fn loss_report_roundtrip_and_old_engine_compat() {
        let now = Instant::now();
        let rep = LossReport { loss_pct: 2.37, conceal_pct: 2.5 };
        let mut buf = [0u8; PING_PLAIN_MAX];
        // 손실 보고만(에코 없음): 옛 엔진의 on_ping은 빈 평문(flags 0)과 같게 처리한다
        let n = ping_plain(None, Some(rep), &mut buf);
        assert_eq!(n, LOSS_REPORT_BYTES);
        assert_eq!(LossReport::parse_ping(&buf[..n]), Some(rep));
        let mut e = EchoState { enabled: true, remote_request_at: Some(now), ..Default::default() };
        e.on_ping(&buf[..n], now);
        assert!(!e.remote_requesting(now) && !e.enabled, "에코 플래그 0으로 해석");
        // 에코 블록 + 손실 보고: 에코는 그대로(옛 엔진은 뒤 바이트 무시), 손실 보고도 읽힌다
        let echo = [ECHO_TAG, ECHO_FLAG_REQUEST, 0, 0, 0, 7];
        let n = ping_plain(Some(echo), Some(rep), &mut buf);
        assert_eq!(n, PING_PLAIN_MAX);
        let mut e = EchoState::default();
        e.on_ping(&buf[..n], now);
        assert!(e.remote_requesting(now));
        assert_eq!(LossReport::parse_ping(&buf[..n]), Some(rep));
        // 옛 엔진 Ping(빈 평문·에코만)에는 보고가 없다
        assert_eq!(LossReport::parse_ping(&[]), None);
        assert_eq!(LossReport::parse_ping(&echo), None);
        assert_eq!(ping_plain(None, None, &mut buf), 0);
        // 잘린 블록·모르는 태그는 무시, 뒤에 붙는 확장 바이트는 허용
        assert_eq!(LossReport::parse_ping(&[LOSS_TAG, 0, 1, 0]), None);
        assert_eq!(LossReport::parse_ping(&[0x55, 0, 1, 0, 1]), None);
        assert!(LossReport::parse_ping(&[LOSS_TAG, 0, 100, 0, 100, 0xAA, 0xBB]).is_some_and(|r| r.loss_pct == 1.0));
        // 범위 밖 값: 보낼 때 0..100%로 자르고, 받을 때도 100%를 넘지 않는다
        let wild = LossReport { loss_pct: f32::NAN, conceal_pct: 400.0 }.encode();
        assert_eq!(LossReport::parse_ping(&wild), Some(LossReport { loss_pct: 0.0, conceal_pct: 100.0 }));
        assert_eq!(LossReport::parse_ping(&[LOSS_TAG, 0xFF, 0xFF, 0, 0]).unwrap().loss_pct, 100.0);
    }
}
