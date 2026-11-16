//! 피어별 연결 시도 → 결과 기록(P1-18, GOAL 3-1·3-5). 계층 1: 소켓·스레드 없음, 시각은 부르는 쪽이 준다.
//!
//! net 제어 스레드(`control_loop`)가 20ms 틱마다 피어 상태(`ConnState`의 상태·경로·시작·첫 연결 시각)를 넘기면
//! [`ConnTracker`]가 바뀐 곳에서 [`ConnEvent`]를 만들어 [`ConnLog`](최근 [`LOG_CAPACITY`]개 링)에 넣는다.
//! 진단 기록 스레드가 1초마다 스냅샷으로 읽어 `seq`로 이미 본 것을 거른다(`diag::Diagnostics::record`).
//! RT 콜백·워커·수신 경로에는 아무것도 추가하지 않는다.
//!
//! 기록하는 것(설계 문서 4절 "진단 내보내기"):
//! - `connect`: 피어 항목(add_peer)의 첫 시도. 결과는 처음 `Connected`가 된 경로(lan/public/relay) — 걸린 시간은
//!   `ConnState.connected_at − started`(= `PeerStats.connectMs`) — 또는 `Connecting` 시간 초과(`failed`, 시작부터).
//! - `reconnect`: `Connected` → `Reconnecting`(무수신)에서 시작한 시도. 다시 `Connected`가 된 경로 또는 `failed`.
//!   시작·끝은 제어 틱에서 본 시각이라 ±1틱(20ms).
//! - `failed` 뒤에도 엔진은 계속 찾는다. 나중에 붙으면 같은 시도의 결과를 한 번 더 남긴다(걸린 시간은 같은 시작부터).
//! - `switch`: `Connected` 중 경로 종류가 바뀜(직접 ↔ 릴레이, LAN ↔ public). `ms` = 직전 경로에 머문 시간.
//! - 상대가 Bye를 보내 `Failed`가 된 것은 연결 실패가 아니라 남기지 않는다. 연결 전에 제거된 피어도 남기지 않는다.

use std::collections::VecDeque;
use std::time::Instant;

use serde::Serialize;

use super::types::{PathKind, PeerStateKind};

/// 링 용량(최근 이벤트 수). 피어 5명이 1초에 여러 번 바뀌어도 진단 기록(1초 간격)이 놓치지 않을 만큼.
pub const LOG_CAPACITY: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnPhase {
    Connect,
    Reconnect,
    Switch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnOutcome {
    Lan,
    Public,
    Relay,
    Failed,
}

impl From<PathKind> for ConnOutcome {
    fn from(k: PathKind) -> Self {
        match k {
            PathKind::Lan => ConnOutcome::Lan,
            PathKind::Public => ConnOutcome::Public,
            PathKind::Relay => ConnOutcome::Relay,
        }
    }
}

/// 연결 이벤트 하나. `peer_id`는 원문(진단 기록 때 가명화한다).
#[derive(Debug, Clone, PartialEq)]
pub struct ConnEvent {
    /// Net 수명 동안 1부터 늘어나는 번호(진단 기록이 이미 본 것을 거르는 데 쓴다).
    pub seq: u64,
    /// 유닉스 시각(ms).
    pub at_ms: u64,
    pub peer_id: String,
    pub phase: ConnPhase,
    pub outcome: ConnOutcome,
    /// 시도 시작 → 결과(ms). `switch`면 직전 경로에 머문 시간.
    pub ms: u32,
}

/// 최근 이벤트 링.
#[derive(Debug, Default)]
pub struct ConnLog {
    buf: VecDeque<ConnEvent>,
    next_seq: u64,
}

impl ConnLog {
    pub fn push(&mut self, at_ms: u64, peer_id: &str, phase: ConnPhase, outcome: ConnOutcome, ms: u32) {
        if self.buf.len() == LOG_CAPACITY {
            self.buf.pop_front();
        }
        self.next_seq += 1;
        self.buf.push_back(ConnEvent { seq: self.next_seq, at_ms, peer_id: peer_id.to_owned(), phase, outcome, ms });
    }

    /// 지금 링에 있는 이벤트(오래된 것부터).
    pub fn events(&self) -> Vec<ConnEvent> {
        self.buf.iter().cloned().collect()
    }
}

/// 제어 틱마다 넘기는 피어 상태(`ConnState`에서 읽은 값).
#[derive(Debug, Clone, Copy)]
pub struct ConnView {
    pub state: PeerStateKind,
    pub path: Option<PathKind>,
    pub started: Instant,
    pub connected_at: Option<Instant>,
    pub bye: bool,
}

#[derive(Debug)]
struct Track {
    id: String,
    /// 피어 항목 식별(같은 peerId라도 교체되면 `started`가 다르다).
    started: Instant,
    phase: ConnPhase,
    /// 진행 중인 시도의 시작(연결됐으면 None).
    attempt: Option<Instant>,
    /// 이 시도에서 `failed`를 이미 남겼다.
    failed_logged: bool,
    last_state: PeerStateKind,
    last_path: Option<PathKind>,
    /// 지금 경로 종류로 바뀐 시각(`switch`의 `ms`).
    path_since: Option<Instant>,
    /// 이번 틱에 본 피어(아니면 지운다).
    seen: bool,
}

fn ms_between(from: Instant, to: Instant) -> u32 {
    to.saturating_duration_since(from).as_millis().min(u32::MAX as u128) as u32
}

/// 피어 상태 변화를 이벤트로 바꾼다. 제어 스레드 하나가 소유한다(락 없음).
#[derive(Debug, Default)]
pub struct ConnTracker {
    tracks: Vec<Track>,
}

impl ConnTracker {
    /// 틱 시작.
    pub fn begin(&mut self) {
        for t in &mut self.tracks {
            t.seen = false;
        }
    }

    /// 피어 하나를 본다. 바뀐 곳이 있으면 `log`에 넣는다. `unix_ms`는 이벤트가 있을 때만 부른다.
    pub fn observe(&mut self, id: &str, v: ConnView, now: Instant, unix_ms: &mut impl FnMut() -> u64, log: &mut impl FnMut(u64, &str, ConnPhase, ConnOutcome, u32)) {
        let i = match self.tracks.iter().position(|t| t.started == v.started && t.id == id) {
            Some(i) => i,
            None => {
                self.tracks.push(Track {
                    id: id.to_owned(),
                    started: v.started,
                    phase: ConnPhase::Connect,
                    attempt: Some(v.started),
                    failed_logged: false,
                    last_state: PeerStateKind::Connecting,
                    last_path: None,
                    path_since: None,
                    seen: true,
                });
                self.tracks.len() - 1
            }
        };
        let t = &mut self.tracks[i];
        t.seen = true;
        let mut emit = |phase, outcome, ms| log(unix_ms(), id, phase, outcome, ms);
        match v.state {
            PeerStateKind::Connected => {
                if let (Some(start), Some(kind)) = (t.attempt, v.path) {
                    // 첫 연결은 수신 스레드가 기록한 정확한 시각(connectMs와 같은 값), 그 뒤는 틱 시각
                    let end = match (t.phase, v.connected_at) {
                        (ConnPhase::Connect, Some(at)) => at,
                        _ => now,
                    };
                    emit(t.phase, kind.into(), ms_between(start, end));
                    t.attempt = None;
                    t.failed_logged = false;
                    t.path_since = Some(end);
                } else if let (Some(prev), Some(kind)) = (t.last_path, v.path) {
                    if prev != kind && t.last_state == PeerStateKind::Connected {
                        emit(ConnPhase::Switch, kind.into(), t.path_since.map_or(0, |s| ms_between(s, now)));
                        t.path_since = Some(now);
                    }
                }
            }
            PeerStateKind::Reconnecting => {
                if t.attempt.is_none() {
                    t.phase = ConnPhase::Reconnect;
                    t.attempt = Some(now);
                }
            }
            PeerStateKind::Failed => {
                if t.attempt.is_none() {
                    // Connected에서 틱 사이에 바로 Failed(틱을 건너뛴 경우)
                    t.phase = ConnPhase::Reconnect;
                    t.attempt = Some(now);
                }
                if !t.failed_logged && !v.bye {
                    let start = t.attempt.unwrap_or(now);
                    emit(t.phase, ConnOutcome::Failed, ms_between(start, now));
                    t.failed_logged = true;
                }
            }
            PeerStateKind::Connecting => {}
        }
        t.last_state = v.state;
        t.last_path = v.path;
    }

    /// 틱 끝: 이번 틱에 안 보인(제거·교체된) 피어를 잊는다.
    pub fn end(&mut self) {
        self.tracks.retain(|t| t.seen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct H {
        tr: ConnTracker,
        log: ConnLog,
        t0: Instant,
    }

    impl H {
        fn new() -> Self {
            Self { tr: ConnTracker::default(), log: ConnLog::default(), t0: Instant::now() }
        }
        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }
        fn tick(&mut self, now_ms: u64, peers: &[(&str, ConnView)]) {
            let now = self.at(now_ms);
            self.tr.begin();
            let log = &mut self.log;
            for (id, v) in peers {
                self.tr.observe(id, *v, now, &mut || 1_000_000 + now_ms, &mut |at, id, ph, out, ms| log.push(at, id, ph, out, ms));
            }
            self.tr.end();
        }
        fn view(&self, state: PeerStateKind, path: Option<PathKind>, connected_ms: Option<u64>) -> ConnView {
            ConnView { state, path, started: self.t0, connected_at: connected_ms.map(|m| self.at(m)), bye: false }
        }
        fn got(&self) -> Vec<(ConnPhase, ConnOutcome, u32)> {
            self.log.events().iter().map(|e| (e.phase, e.outcome, e.ms)).collect()
        }
    }

    use ConnOutcome as O;
    use ConnPhase as P;
    use PeerStateKind as S;

    #[test]
    fn first_connect_uses_exact_connected_at_and_records_once() {
        let mut h = H::new();
        h.tick(0, &[("bob", h.view(S::Connecting, None, None))]);
        h.tick(20, &[("bob", h.view(S::Connecting, None, None))]);
        // 수신 스레드가 13ms에 연결, 제어 틱은 40ms에 본다
        h.tick(40, &[("bob", h.view(S::Connected, Some(PathKind::Lan), Some(13)))]);
        h.tick(60, &[("bob", h.view(S::Connected, Some(PathKind::Lan), Some(13)))]);
        assert_eq!(h.got(), vec![(P::Connect, O::Lan, 13)]);
        let e = &h.log.events()[0];
        assert_eq!((e.seq, e.at_ms, e.peer_id.as_str()), (1, 1_000_040, "bob"));
    }

    #[test]
    fn connect_timeout_is_failed_then_late_relay_success() {
        let mut h = H::new();
        h.tick(0, &[("bob", h.view(S::Connecting, None, None))]);
        h.tick(10_000, &[("bob", h.view(S::Failed, None, None))]);
        h.tick(10_020, &[("bob", h.view(S::Failed, None, None))]);
        h.tick(12_000, &[("bob", h.view(S::Connected, Some(PathKind::Relay), Some(11_990)))]);
        assert_eq!(h.got(), vec![(P::Connect, O::Failed, 10_000), (P::Connect, O::Relay, 11_990)]);
    }

    #[test]
    fn reconnect_and_switch() {
        let mut h = H::new();
        h.tick(0, &[("bob", h.view(S::Connected, Some(PathKind::Public), Some(0)))]);
        // 직접 → 릴레이 강등(Connected 유지)
        h.tick(3_000, &[("bob", h.view(S::Connected, Some(PathKind::Relay), Some(0)))]);
        // 무수신 → Reconnecting → 2초 뒤 릴레이로 다시 연결
        h.tick(8_000, &[("bob", h.view(S::Reconnecting, Some(PathKind::Relay), Some(0)))]);
        h.tick(10_000, &[("bob", h.view(S::Connected, Some(PathKind::Relay), Some(0)))]);
        // 다시 잃고 실패
        h.tick(20_000, &[("bob", h.view(S::Reconnecting, Some(PathKind::Relay), Some(0)))]);
        h.tick(30_000, &[("bob", h.view(S::Failed, Some(PathKind::Relay), Some(0)))]);
        assert_eq!(
            h.got(),
            vec![
                (P::Connect, O::Public, 0),
                (P::Switch, O::Relay, 3_000),
                (P::Reconnect, O::Relay, 2_000),
                (P::Reconnect, O::Failed, 10_000),
            ]
        );
    }

    #[test]
    fn bye_failure_and_removed_peer_not_logged_replaced_peer_is_new_attempt() {
        let mut h = H::new();
        h.tick(0, &[("bob", h.view(S::Connected, Some(PathKind::Lan), Some(0)))]);
        let mut bye = h.view(S::Failed, Some(PathKind::Lan), Some(0));
        bye.bye = true;
        h.tick(1_000, &[("bob", bye)]);
        // 제거(안 보임) → 같은 id로 다시 추가(새 started)
        h.tick(1_020, &[]);
        let mut again = h.view(S::Connecting, None, None);
        again.started = h.at(2_000);
        h.tick(2_000, &[("bob", again)]);
        again.state = S::Connected;
        again.path = Some(PathKind::Public);
        again.connected_at = Some(h.at(2_150));
        h.tick(2_160, &[("bob", again)]);
        assert_eq!(h.got(), vec![(P::Connect, O::Lan, 0), (P::Connect, O::Public, 150)]);
    }

    #[test]
    fn log_ring_keeps_latest_with_increasing_seq() {
        let mut l = ConnLog::default();
        for i in 0..(LOG_CAPACITY as u32 + 5) {
            l.push(0, "p", ConnPhase::Connect, ConnOutcome::Lan, i);
        }
        let ev = l.events();
        assert_eq!(ev.len(), LOG_CAPACITY);
        assert_eq!(ev[0].seq, 6);
        assert_eq!(ev.last().unwrap().seq, LOG_CAPACITY as u64 + 5);
    }
}
