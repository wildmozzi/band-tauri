//! 피어별 연결 시도 → 결과 기록(P1-18, GOAL 3-1·3-5). 계층 1: 소켓·스레드 없음, 시각은 부르는 쪽이 준다.
//!
//! net 제어 스레드(`control_loop`)가 20ms 틱마다 피어 상태(`ConnState`의 상태·경로·시작·첫 연결 시각)를 넘기면
//! [`ConnTracker`]가 바뀐 곳에서 [`ConnEvent`]를 만들어 [`ConnLog`](최근 [`LOG_CAPACITY`]개 링)에 넣는다.
//! 진단 기록 스레드가 1초마다 스냅샷으로 읽어 `seq`로 이미 본 것을 거른다(`diag::Diagnostics::record`).
//! RT 콜백·워커·수신 경로에는 아무것도 추가하지 않는다.
//!
//! 기록하는 것(설계 문서 3절 5 "연결 결과 기록"). **시도 하나에 결과는 정확히 하나**다(`connect`·`reconnect`):
//! - `connect`: 피어 항목(add_peer)의 첫 시도. 결과는 처음 `Connected`가 된 경로(lan/public/relay) — 걸린 시간은
//!   `ConnState.connected_at − started`(= `PeerStats.connectMs`) — 또는 `Connecting` 시간 초과(`failed`, 시작부터).
//! - `reconnect`: `Connected`에서 수신이 끊겨 다시 찾는 시도. 시작은 마지막 수신 + `LOST_AFTER`(부르는 쪽이
//!   `ConnView::lost_at`으로 준다, 지금보다 늦으면 지금 — 경로 해제처럼 무수신이 아닌 전환). 즉 실제 끊김 시간보다
//!   `LOST_AFTER`(5초)만큼 짧다. 끝은 제어 틱에서 본 시각(±20ms). 결과는 다시 붙은 경로 또는 `failed`.
//! - `aborted`: 결과가 나기 전에 시도가 끝남 — 피어 제거·교체(Bye를 받지 않음) 또는 엔진(Net) 정지. ms = 시작부터.
//!   빼면 "연결 중"에 사용자가 나간 시도가 분모에서 빠져 성공률이 위로 치우친다(#145 리뷰 M-1).
//! - `recover`: `failed`로 끝난 뒤에도 엔진은 계속 찾는다. 나중에 붙으면 그 경로를 `recover`로 남긴다(ms = 실패부터).
//!   새 시도가 아니다 — 집계는 `connect`·`reconnect`만 시도로 센다. 실패 뒤 찾다가 제거·정지되면 남기지 않는다.
//! - `switch`: `Connected` 중 경로 종류가 바뀜(직접 ↔ 릴레이, LAN ↔ public). `ms` = 직전 경로에 머문 시간.
//! - 상대가 Bye를 보낸 피어(정상 퇴장)는 실패·중단을 남기지 않는다.

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
    /// `failed` 뒤 늦게 붙음(새 시도 아님).
    Recover,
    Switch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnOutcome {
    Lan,
    Public,
    Relay,
    Failed,
    /// 결과 전에 피어 제거·교체 또는 엔진 정지.
    Aborted,
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
    /// 시도 시작 → 결과(ms). `switch`면 직전 경로에 머문 시간, `recover`면 실패부터.
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
    /// 마지막 수신 + `LOST_AFTER`(수신이 없었으면 None). 재연결 시도의 시작.
    pub lost_at: Option<Instant>,
    pub bye: bool,
}

/// 이벤트를 내보낼 곳: 이벤트가 있을 때만 벽시계를 읽고(`unix_ms`) 로그에 넣는다(`log`).
pub struct Sink<'a> {
    pub unix_ms: &'a mut dyn FnMut() -> u64,
    pub log: &'a mut dyn FnMut(u64, &str, ConnPhase, ConnOutcome, u32),
}

impl Sink<'_> {
    fn emit(&mut self, id: &str, phase: ConnPhase, outcome: ConnOutcome, ms: u32) {
        let at = (self.unix_ms)();
        (self.log)(at, id, phase, outcome, ms);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Pending {
    /// 결과가 아직 없는 시도(시작 시각).
    Attempt(ConnPhase, Instant),
    /// `failed`를 남긴 뒤 계속 찾는 중(실패 시각) — 붙으면 `recover`, 끝나면 기록 없음.
    AfterFailure(Instant),
    /// 연결됨(지금 경로로 바뀐 시각).
    Connected(Instant),
}

#[derive(Debug)]
struct Track {
    id: String,
    /// 피어 항목 식별(같은 peerId라도 교체되면 `started`가 다르다).
    started: Instant,
    pending: Pending,
    last_path: Option<PathKind>,
    /// 마지막으로 본 Bye 수신 여부(제거될 때 정상 퇴장인지).
    bye: bool,
    /// 이번 틱에 본 피어(아니면 끝난 것으로 본다).
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

    /// 피어 하나를 본다. 바뀐 곳이 있으면 `sink`로 낸다.
    pub fn observe(&mut self, id: &str, v: ConnView, now: Instant, sink: &mut Sink<'_>) {
        let i = match self.tracks.iter().position(|t| t.started == v.started && t.id == id) {
            Some(i) => i,
            None => {
                self.tracks.push(Track {
                    id: id.to_owned(),
                    started: v.started,
                    pending: Pending::Attempt(ConnPhase::Connect, v.started),
                    last_path: None,
                    bye: false,
                    seen: true,
                });
                self.tracks.len() - 1
            }
        };
        let t = &mut self.tracks[i];
        t.seen = true;
        t.bye = v.bye;
        // 재연결 시도의 시작: 마지막 수신 + LOST_AFTER, 지금보다 늦으면(무수신이 아닌 전환) 지금
        let lost_start = || v.lost_at.filter(|&l| l <= now).unwrap_or(now);
        match (v.state, v.path) {
            (PeerStateKind::Connected, Some(kind)) => match t.pending {
                Pending::Attempt(phase, start) => {
                    // 첫 연결은 수신 스레드가 기록한 정확한 시각(connectMs와 같은 값), 그 뒤는 틱 시각
                    let end = match (phase, v.connected_at) {
                        (ConnPhase::Connect, Some(at)) => at,
                        _ => now,
                    };
                    sink.emit(id, phase, kind.into(), ms_between(start, end));
                    t.pending = Pending::Connected(end);
                }
                Pending::AfterFailure(failed_at) => {
                    sink.emit(id, ConnPhase::Recover, kind.into(), ms_between(failed_at, now));
                    t.pending = Pending::Connected(now);
                }
                Pending::Connected(since) => {
                    if t.last_path.is_some_and(|prev| prev != kind) {
                        sink.emit(id, ConnPhase::Switch, kind.into(), ms_between(since, now));
                        t.pending = Pending::Connected(now);
                    }
                }
            },
            (PeerStateKind::Reconnecting, _) => {
                if let Pending::Connected(_) = t.pending {
                    t.pending = Pending::Attempt(ConnPhase::Reconnect, lost_start());
                }
            }
            (PeerStateKind::Failed, _) => {
                let (phase, start) = match t.pending {
                    Pending::Attempt(phase, start) => (phase, start),
                    // Connected에서 틱 사이에 바로 Failed(틱을 건너뜀): 시작은 끊긴 시점
                    Pending::Connected(_) => (ConnPhase::Reconnect, lost_start()),
                    Pending::AfterFailure(_) => {
                        t.last_path = v.path;
                        return;
                    }
                };
                if v.bye {
                    // 정상 퇴장: 실패가 아니다. 이 항목으로는 더 남기지 않는다.
                    t.pending = Pending::AfterFailure(now);
                } else {
                    sink.emit(id, phase, ConnOutcome::Failed, ms_between(start, now));
                    t.pending = Pending::AfterFailure(now);
                }
            }
            // Connecting, 또는 경로 없는 Connected(없는 상태)
            _ => {}
        }
        t.last_path = v.path;
    }

    /// 틱 끝: 이번 틱에 안 보인(제거·교체된) 피어를 잊는다. 결과 전인 시도는 `aborted`로 남긴다(Bye 받은 피어 제외).
    pub fn end(&mut self, now: Instant, sink: &mut Sink<'_>) {
        for t in self.tracks.iter().filter(|t| !t.seen) {
            if let (Pending::Attempt(phase, start), false) = (t.pending, t.bye) {
                sink.emit(&t.id, phase, ConnOutcome::Aborted, ms_between(start, now));
            }
        }
        self.tracks.retain(|t| t.seen);
    }

    /// Net 정지: 남은 모든 피어를 끝난 것으로 본다(결과 전 시도는 `aborted`).
    pub fn finish(&mut self, now: Instant, sink: &mut Sink<'_>) {
        self.begin();
        self.end(now, sink);
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
        fn with_sink(&mut self, now_ms: u64, f: impl FnOnce(&mut ConnTracker, Instant, &mut Sink<'_>)) {
            let now = self.at(now_ms);
            let log = &mut self.log;
            let mut unix = || 1_000_000 + now_ms;
            let mut push = |at, id: &str, ph, out, ms| log.push(at, id, ph, out, ms);
            let mut sink = Sink { unix_ms: &mut unix, log: &mut push };
            f(&mut self.tr, now, &mut sink);
        }
        fn tick(&mut self, now_ms: u64, peers: &[(&str, ConnView)]) {
            let peers = peers.to_vec();
            self.with_sink(now_ms, |tr, now, sink| {
                tr.begin();
                for (id, v) in &peers {
                    tr.observe(id, *v, now, sink);
                }
                tr.end(now, sink);
            });
        }
        fn stop(&mut self, now_ms: u64) {
            self.with_sink(now_ms, |tr, now, sink| tr.finish(now, sink));
        }
        fn view(&self, state: PeerStateKind, path: Option<PathKind>, connected_ms: Option<u64>) -> ConnView {
            ConnView { state, path, started: self.t0, connected_at: connected_ms.map(|m| self.at(m)), lost_at: None, bye: false }
        }
        /// 마지막 수신이 `rx_ms`였던 상태(lost_at = rx + 5초).
        fn lost(&self, mut v: ConnView, rx_ms: u64) -> ConnView {
            v.lost_at = Some(self.at(rx_ms + 5_000));
            v
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

    /// 시도 하나에 결과 하나: 시간 초과로 `connect failed`가 난 뒤 늦게 붙으면 `recover`(새 시도 아님, 실패부터 ms).
    #[test]
    fn late_success_after_failure_is_recover_not_second_result() {
        let mut h = H::new();
        h.tick(0, &[("bob", h.view(S::Connecting, None, None))]);
        h.tick(10_000, &[("bob", h.view(S::Failed, None, None))]);
        h.tick(10_020, &[("bob", h.view(S::Failed, None, None))]);
        h.tick(12_000, &[("bob", h.view(S::Connected, Some(PathKind::Relay), Some(11_990)))]);
        assert_eq!(h.got(), vec![(P::Connect, O::Failed, 10_000), (P::Recover, O::Relay, 2_000)]);
        let attempts = h.got().iter().filter(|(p, _, _)| matches!(p, P::Connect | P::Reconnect)).count();
        assert_eq!(attempts, 1);
    }

    /// 실패 뒤 계속 찾다가 제거·정지되면 아무것도 더 남기지 않는다(그 시도의 결과는 이미 `failed`).
    #[test]
    fn removed_while_probing_after_failure_adds_nothing() {
        let mut h = H::new();
        h.tick(0, &[("bob", h.view(S::Connecting, None, None))]);
        h.tick(10_000, &[("bob", h.view(S::Failed, None, None))]);
        h.tick(15_000, &[]);
        h.stop(16_000);
        assert_eq!(h.got(), vec![(P::Connect, O::Failed, 10_000)]);
    }

    #[test]
    fn reconnect_starts_at_loss_and_switch() {
        let mut h = H::new();
        h.tick(0, &[("bob", h.view(S::Connected, Some(PathKind::Public), Some(0)))]);
        // 직접 → 릴레이 강등(Connected 유지)
        h.tick(3_000, &[("bob", h.view(S::Connected, Some(PathKind::Relay), Some(0)))]);
        // 마지막 수신 3s → 8s에 Reconnecting(시작 = 3s + 5s) → 10s에 릴레이로 다시 연결
        h.tick(8_020, &[("bob", h.lost(h.view(S::Reconnecting, Some(PathKind::Relay), Some(0)), 3_000))]);
        h.tick(10_000, &[("bob", h.view(S::Connected, Some(PathKind::Relay), Some(0)))]);
        // 다시 잃고(마지막 수신 15s) 실패
        h.tick(20_000, &[("bob", h.lost(h.view(S::Reconnecting, Some(PathKind::Relay), Some(0)), 15_000))]);
        h.tick(30_000, &[("bob", h.lost(h.view(S::Failed, Some(PathKind::Relay), Some(0)), 15_000))]);
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

    /// 틱을 건너뛰어 Connected에서 바로 Failed를 보면 ms는 0이 아니라 끊긴 시점(마지막 수신 + 5초)부터다(#145 L-4).
    #[test]
    fn skipped_tick_connected_to_failed_uses_loss_time() {
        let mut h = H::new();
        h.tick(0, &[("bob", h.view(S::Connected, Some(PathKind::Lan), Some(0)))]);
        // 절전 등으로 20초 동안 틱이 없었다. 마지막 수신 1s.
        h.tick(21_000, &[("bob", h.lost(h.view(S::Failed, Some(PathKind::Lan), Some(0)), 1_000))]);
        assert_eq!(h.got(), vec![(P::Connect, O::Lan, 0), (P::Reconnect, O::Failed, 15_000)]);
    }

    /// 결과 전 시도가 끝나면 `aborted` + 시작부터 ms(#145 M-1): 연결 중 제거, 재연결 중 제거, 교체, 엔진 정지.
    #[test]
    fn unfinished_attempts_are_aborted_on_remove_replace_and_stop() {
        let mut h = H::new();
        let b = |h: &H, ms| {
            let mut v = h.view(S::Connecting, None, None);
            v.started = h.at(ms);
            v
        };
        // alice: 연결 중 4초에 제거
        let alice = b(&h, 0);
        h.tick(0, &[("alice", alice)]);
        h.tick(4_000, &[]);
        // bob: 연결 → 재연결 중(마지막 수신 2s) 9s에 제거
        h.tick(5_000, &[("bob", h.view(S::Connected, Some(PathKind::Lan), Some(5_000)))]);
        h.tick(8_000, &[("bob", h.lost(h.view(S::Reconnecting, Some(PathKind::Lan), Some(5_000)), 2_000))]);
        h.tick(9_000, &[]);
        // carol: 연결 중 같은 id로 교체(새 started) → 옛 시도 aborted, 새 시도는 이어서
        let c1 = b(&h, 10_000);
        h.tick(10_000, &[("carol", c1)]);
        let c2 = b(&h, 11_000);
        h.tick(11_000, &[("carol", c2)]);
        // 엔진 정지 13s: carol 새 시도 aborted(시작 11s부터)
        h.stop(13_000);
        assert_eq!(
            h.got(),
            vec![
                (P::Connect, O::Aborted, 4_000),
                (P::Connect, O::Lan, 5_000),
                (P::Reconnect, O::Aborted, 2_000),
                (P::Connect, O::Aborted, 1_000),
                (P::Connect, O::Aborted, 2_000),
            ]
        );
        // 연결된 피어의 정지는 남기지 않는다
        let mut h = H::new();
        h.tick(0, &[("bob", h.view(S::Connected, Some(PathKind::Lan), Some(0)))]);
        h.stop(1_000);
        assert_eq!(h.got(), vec![(P::Connect, O::Lan, 0)]);
    }

    #[test]
    fn bye_is_not_failure_or_abort_and_replaced_peer_is_new_attempt() {
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
        // 연결 중 Bye를 받고 제거된 피어도 aborted가 아니다
        let mut d = h.view(S::Connecting, None, None);
        d.started = h.at(3_000);
        h.tick(3_000, &[("dave", d)]);
        d.bye = true;
        d.state = S::Failed;
        h.tick(3_500, &[("dave", d)]);
        h.tick(3_520, &[]);
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
