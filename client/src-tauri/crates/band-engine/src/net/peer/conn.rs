//! 연결 상태기계: 홀펀칭·경로 선택·릴레이 폴백·승격/강등, keepalive 타이밍(설계 문서 3절).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use band_dsp::packet::PacketType;
use band_dsp::types::{PathKind, PeerStateKind};

use super::descriptor::{CandidateKind, MAX_CANDIDATES};

pub(crate) const HELLO_INTERVAL: Duration = Duration::from_millis(100);
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const PING_INTERVAL: Duration = Duration::from_millis(500);
pub(crate) const LOST_AFTER: Duration = Duration::from_secs(5);
pub(crate) const FAIL_AFTER: Duration = Duration::from_secs(15);
/// 확정 경로가 아닌 같은 주소에서 인증된 Hello가 이만큼 연속으로 오면 경로를 옮긴다(NAT 리바인딩).
pub(crate) const REBIND_HELLOS: u32 = 5;
/// HelloAck 최소 간격(리뷰 L6: 재전송 Hello로 인한 반사 제한).
pub(crate) const HELLO_ACK_MIN_INTERVAL: Duration = Duration::from_millis(50);
/// 직접 연결이 이 시간 안에 안 되면 릴레이로도 Hello를 보낸다(P1-4b, GOAL 3절 "입장 후 3초").
pub(crate) const RELAY_FALLBACK_AFTER: Duration = Duration::from_millis(1500);
/// 직접 경로에서 이 시간 동안 아무것도 못 받으면 릴레이로도 Hello를 보낸다(끊김 전에 대체 경로 확보).
pub(crate) const RELAY_PROBE_AFTER: Duration = Duration::from_millis(1500);
/// 릴레이 경로에서 직접 경로를 백그라운드로 다시 찾는 간격.
pub(crate) const DIRECT_PROBE_INTERVAL: Duration = Duration::from_secs(1);
/// `Failed` 뒤에도 이 간격으로 직접·릴레이 Hello를 계속 보낸다(늦게 열린 릴레이/경로로 회복).
pub(crate) const FAILED_PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// 경로를 바꾼 뒤 이전 경로로 오는 인증 패킷(비행 중)을 받아 주는 시간(무끊김 전환).
pub(crate) const PATH_GRACE: Duration = Duration::from_secs(2);
/// 릴레이 → 직접 승격 조건(PR #10 M2 히스테리시스): 같은 직접 주소에서 HelloAck(왕복 확인)가 이만큼, 간격
/// `PROMOTE_MAX_GAP` 이하로 끊김 없이, `PROMOTE_STABLE` 이상 이어져야 한다(1초 탐색 → 약 2초).
pub(crate) const PROMOTE_ACKS: u32 = 3;
pub(crate) const PROMOTE_STABLE: Duration = Duration::from_secs(2);
pub(crate) const PROMOTE_MAX_GAP: Duration = Duration::from_millis(1500);
/// 직접 경로 RTT가 릴레이 경유 종단 RTT보다 이만큼 넘게 크면 승격하지 않는다(ms).
pub(crate) const PROMOTE_RTT_MARGIN_MS: f32 = 10.0;
/// 직접 → 릴레이 강등 뒤 승격을 미루는 최소 시간. 승격 후 `FLAP_WINDOW` 안에 다시 강등되면(플래핑) 두 배씩
/// 늘려 `RELAY_HOLD_MAX`까지.
pub(crate) const RELAY_HOLD_MIN: Duration = Duration::from_secs(5);
pub(crate) const RELAY_HOLD_MAX: Duration = Duration::from_secs(60);
pub(crate) const FLAP_WINDOW: Duration = Duration::from_secs(30);

fn seq_newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

#[derive(Debug, Clone)]
pub(crate) struct ConnState {
    pub(crate) state: PeerStateKind,
    pub(crate) path: Option<(SocketAddr, PathKind)>,
    pub(crate) started: Instant,
    /// 이 항목(add_peer)에서 처음 Connected가 된 시각(P1-20 `connectMs`). 한 번 정하면 재연결·경로 전환에도 그대로다.
    pub(crate) connected_at: Option<Instant>,
    pub(crate) last_rx: Option<Instant>,
    pub(crate) last_hello: Option<Instant>,
    pub(crate) last_ping: Option<Instant>,
    /// 주소별 마지막 HelloAck 시각(PR #5 L-4: 두 후보 경로가 서로를 굶기지 않게 주소 단위로 제한).
    pub(crate) last_acks: Vec<(SocketAddr, Instant)>,
    pub(crate) rtt_ms: Option<f32>,
    /// 경로가 아닌 주소에서 연속으로 온 인증된 Hello (주소, 횟수).
    pub(crate) rebind: Option<(SocketAddr, u32)>,
    /// 제어 패킷 타입별 최대 seq (재전송 방어, 리뷰 L5).
    ctrl_seq: [Option<u32>; 6],
    /// 이 피어와 릴레이 경유 Hello/HelloAck가 오간 적이 있다(릴레이로 온 인증 패킷 수용).
    pub(crate) relay_confirmed: bool,
    /// 릴레이 경로 중 직접 주소에서 인증된 Hello가 왔다. 그 주소에서 Hello 외 인증 패킷이 오면 상대가
    /// 직접 경로로 승격한 것이므로 따라 승격한다.
    pub(crate) direct_hint: Option<SocketAddr>,
    /// 직전 경로와 바꾼 시각(PATH_GRACE 동안 수신 허용).
    pub(crate) prev_path: Option<(SocketAddr, Instant)>,
    /// 상대가 Bye를 보냈다(더 탐색하지 않음).
    pub(crate) bye: bool,
    last_direct_probe: Option<Instant>,
    /// 릴레이 경로 중 직접 주소의 연속 HelloAck(승격 히스테리시스).
    direct_probe: Option<DirectProbe>,
    /// 이 시각 전에는 직접 경로로 승격하지 않는다(강등 직후·플래핑).
    pub(crate) promote_hold_until: Option<Instant>,
    /// 최근 연속 플래핑 횟수(승격 후 FLAP_WINDOW 안에 강등).
    pub(crate) flaps: u32,
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
pub(crate) enum HandshakeOutcome {
    /// 경로 확정/유지 또는 전환.
    Accepted,
    /// 확정 경로가 아닌 주소: 상태·last_rx는 건드리지 않는다.
    Ignored,
}

impl ConnState {
    pub(crate) fn new(now: Instant) -> Self {
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
    pub(crate) fn relay_moved(&mut self, old: SocketAddr, new: Option<SocketAddr>, now: Instant) {
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

    pub(crate) fn path_kind(&self) -> Option<PathKind> {
        self.path.map(|(_, k)| k)
    }

    /// 같은 세션(같은 키)의 피어를 다시 추가할 때 재전송 창을 이어받는다(PR #5 L-2).
    pub(crate) fn inherit_replay_window(&mut self, old: &ConnState) {
        self.ctrl_seq = old.ctrl_seq;
    }

    /// 제어 패킷 seq가 이전보다 새로우면 기록하고 true. 재전송(같거나 옛 seq)은 false.
    pub(crate) fn accept_ctrl_seq(&mut self, t: PacketType, seq: u32) -> bool {
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
    #[cfg(test)]
    pub(crate) fn on_handshake(
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
    pub(crate) fn on_handshake_rtt(
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
    pub(crate) fn accept_rx(&mut self, from: SocketAddr, cands: &[(SocketAddr, CandidateKind)], relay: Option<SocketAddr>, now: Instant) -> bool {
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
    pub(crate) fn ack_allowed(&mut self, to: SocketAddr, now: Instant) -> bool {
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
    pub(crate) fn on_rx_from_path(&mut self, now: Instant) {
        self.rebind = None;
        self.last_rx = Some(now);
        if self.state == PeerStateKind::Reconnecting {
            self.state = PeerStateKind::Connected;
        }
    }

    pub(crate) fn on_rtt_sample(&mut self, rtt_ms: f32) {
        if !(0.0..10_000.0).contains(&rtt_ms) {
            return;
        }
        self.rtt_ms = Some(match self.rtt_ms {
            None => rtt_ms,
            Some(r) => r + (rtt_ms - r) * 0.125,
        });
    }

    pub(crate) fn is_path(&self, from: SocketAddr) -> bool {
        self.path.is_some_and(|(a, _)| a == from)
    }

    pub(crate) fn can_send_audio(&self) -> bool {
        self.path.is_some() && matches!(self.state, PeerStateKind::Connected | PeerStateKind::Reconnecting)
    }

    /// 제어 틱. 해야 할 송신을 돌려준다. `relay_ready`=이 엔진이 릴레이에 등록되어 있다.
    pub(crate) fn tick(&mut self, now: Instant, relay_ready: bool) -> TickAction {
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
pub(crate) struct TickAction {
    /// 직접 후보(와 현재 직접 경로·리바인딩 후보)로 Hello.
    pub(crate) hello_direct: bool,
    /// 릴레이 경유 Hello(RelayData).
    pub(crate) hello_relay: bool,
    /// 확정 경로로 Ping.
    pub(crate) ping: bool,
}

impl TickAction {
    #[cfg(test)]
    pub(crate) const NONE: TickAction = TickAction { hello_direct: false, hello_relay: false, ping: false };
    #[cfg(test)]
    pub(crate) const HELLO: TickAction = TickAction { hello_direct: true, hello_relay: false, ping: false };
    #[cfg(test)]
    pub(crate) const PING: TickAction = TickAction { hello_direct: false, hello_relay: false, ping: true };
}
