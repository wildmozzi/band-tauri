//! 진단(P2-5): 엔진 통계 타임라인 기록과 진단 zip 내보내기.
//!
//! 기록 스레드는 1초마다 받은 스냅샷 함수를 부른다. 앱에서는 어댑터(`commands::DiagState`)가
//! `EngineManager::snapshot()`(엔진 슬롯 락을 잠깐 잡고 `Engine::stats()`)을 넘긴다 — UI가 `engine_stats`를 부르는 것과 같은 경로다.
//! `diag`는 `manager`를 모른다(엔진 ⑤). 입력 타입은 `engine::report::EngineSnapshot`. RT 콜백(`engine/rt.rs`)에는 아무것도 추가하지 않는다.

pub mod export;
pub(crate) mod mask;
pub(crate) mod timeline;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};

use band_dsp::config::EngineConfig;
use crate::engine::report::{EndedConn, EngineSnapshot};
use crate::net::connlog::ConnEvent;
use timeline::{ConnSample, Sample, Timeline, DEFAULT_CAPACITY, SAMPLE_INTERVAL_MS};

struct Inner {
    timeline: Timeline,
    /// 직전 기록 때 엔진이 있었는지(정지 표시 샘플을 한 번만 넣기 위해)
    was_active: bool,
    /// 직전 샘플의 (network, session) — 정지 표시 샘플에 그대로 쓴다
    last_session: (bool, u64),
    /// 현재 또는 마지막 세션의 설정
    last_config: Option<EngineConfig>,
    /// 이미 기록한 연결 이벤트의 (세션, 최대 seq), 최근 세션 몇 개. 스냅샷은 최근 이벤트를 모두 주므로 새 것만 넣는다(P1-18).
    /// 정지한 엔진의 마지막 이벤트(`record_ended`)는 새 세션이 시작된 뒤에 올 수 있어 세션별로 기억한다.
    conn_seen: Vec<(u64, u64)>,
    /// 다음 샘플에 붙일 연결 이벤트(정지한 엔진의 마지막 이벤트).
    pending_conn: Vec<ConnSample>,
    /// 직전 샘플의 (세션, 코어 배치 누적 수) — 다음 샘플을 구간 값으로 만드는 기준(P1-21 후속).
    last_placement: Option<(u64, crate::sys::placement::PlacementWindow)>,
    /// 마지막으로 타임라인에 적은 스레드 정책(세션, 값) — 같은 값이면 줄에 다시 넣지 않는다(P1-21 2단계).
    last_policy: Option<(u64, crate::sys::placement::ThreadPolicyStats)>,
}

/// 기억하는 세션 수(`conn_seen`).
const CONN_SEEN_SESSIONS: usize = 4;

impl Inner {
    /// 그 세션에서 아직 기록하지 않은 이벤트만 꺼내고 기록한 것으로 표시한다.
    fn take_new_conn(&mut self, session: u64, events: &[ConnEvent]) -> Vec<ConnSample> {
        let i = match self.conn_seen.iter().position(|(s, _)| *s == session) {
            Some(i) => i,
            None => {
                if self.conn_seen.len() == CONN_SEEN_SESSIONS {
                    self.conn_seen.remove(0);
                }
                self.conn_seen.push((session, 0));
                self.conn_seen.len() - 1
            }
        };
        let after = self.conn_seen[i].1;
        self.conn_seen[i].1 = events.iter().map(|e| e.seq).max().unwrap_or(0).max(after);
        events.iter().filter(|e| e.seq > after).map(ConnSample::from_event).collect()
    }
}

pub struct Diagnostics {
    inner: Mutex<Inner>,
    stop: AtomicBool,
    wake: (Mutex<()>, Condvar),
    /// 내보내기 진행 중(대화상자가 두 개 열리지 않게)
    busy: AtomicBool,
}

/// 내보내기 진행 표시. 떨어지면 다시 내보낼 수 있다.
pub struct ExportGuard<'a>(&'a AtomicBool);

impl Drop for ExportGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl Diagnostics {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                timeline: Timeline::new(capacity),
                was_active: false,
                last_session: (false, 0),
                last_config: None,
                conn_seen: Vec::new(),
                pending_conn: Vec::new(),
                last_placement: None,
                last_policy: None,
            }),
            busy: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            wake: (Mutex::new(()), Condvar::new()),
        }
    }

    /// 한 번 기록한다. 엔진이 있으면 샘플을 넣고, 엔진이 막 없어졌으면 정지 표시 샘플을 한 번 넣는다.
    /// 엔진이 계속 없으면 아무것도 넣지 않는다(빈 시간은 `t` 간격으로 보인다).
    pub fn record(&self, now_ms: u64, snap: Option<EngineSnapshot>) {
        let mut g = self.inner.lock();
        match snap {
            Some(s) => {
                let mut sample = Sample::from_stats(now_ms, s.network, s.session, &s.stats);
                // 코어 배치는 구간 값으로(#191 리뷰 L1): 같은 세션의 직전 누적과 차분한다. 세션이 바뀌면 카운터가 0부터라 그대로.
                let cum = sample.core_placement;
                if let (Some(c), Some((sess, prev))) = (cum, g.last_placement) {
                    if sess == s.session {
                        sample.core_placement = Some(c.since(&prev));
                    }
                }
                g.last_placement = cum.map(|c| (s.session, c));
                // 스레드 정책은 값이 바뀐 줄에만(세션 첫 줄 포함) 넣는다 — 스레드 시작 때 한 번 정해지는 값이다.
                match sample.thread_policy {
                    Some(p) if g.last_policy == Some((s.session, p)) => sample.thread_policy = None,
                    Some(p) => g.last_policy = Some((s.session, p)),
                    None => {}
                }
                // 세션마다(엔진 재시작 = 새 Net) seq가 1부터 다시 센다
                let fresh = g.take_new_conn(s.session, &s.conn_events);
                sample.conn = std::mem::take(&mut g.pending_conn);
                sample.conn.extend(fresh);
                g.timeline.push_folding(sample, Sample::fold_dropped);
                g.last_config = Some(s.config);
                g.last_session = (s.network, s.session);
                g.was_active = true;
            }
            None if g.was_active => {
                let (network, session) = g.last_session;
                let mut sample = Sample::stopped(now_ms, network, session);
                sample.conn = std::mem::take(&mut g.pending_conn);
                g.timeline.push_folding(sample, Sample::fold_dropped);
                g.was_active = false;
            }
            None => {}
        }
    }

    /// 정지한 엔진의 마지막 연결 이벤트(정지 때 `aborted` 포함, #145 M-1). 아직 기록하지 않은 것만 다음 샘플
    /// (보통 정지 표시 샘플)에 붙인다.
    pub(crate) fn record_ended(&self, ended: Vec<EndedConn>) {
        let mut g = self.inner.lock();
        for e in ended {
            let fresh = g.take_new_conn(e.session, &e.events);
            g.pending_conn.extend(fresh);
        }
    }

    /// 내보내기를 시작한다. 이미 진행 중이면 `None`.
    pub fn begin_export(&self) -> Option<ExportGuard<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ExportGuard(&self.busy))
    }

    /// 내보내기용으로 현재 내용을 읽는다.
    pub fn with_timeline<T>(&self, f: impl FnOnce(&Timeline, Option<&EngineConfig>, bool) -> T) -> T {
        let g = self.inner.lock();
        f(&g.timeline, g.last_config.as_ref(), g.was_active)
    }

    /// 1초마다 `ended()`(정지한 엔진의 마지막 연결 이벤트)와 `snapshot()`을 불러 기록하는 스레드를 띄운다.
    /// `shutdown()`으로 멈춘다.
    pub fn spawn_recorder(
        self: &Arc<Self>,
        snapshot: impl Fn() -> Option<EngineSnapshot> + Send + 'static,
        ended: impl Fn() -> Vec<EndedConn> + Send + 'static,
    ) -> std::io::Result<std::thread::JoinHandle<()>> {
        let me = self.clone();
        std::thread::Builder::new().name("band-diag".into()).spawn(move || {
            let interval = Duration::from_millis(SAMPLE_INTERVAL_MS);
            while !me.stop.load(Ordering::Acquire) {
                me.record_ended(ended());
                me.record(crate::sys::time::unix_ms(), snapshot());
                let mut g = me.wake.0.lock();
                if me.stop.load(Ordering::Acquire) {
                    break;
                }
                me.wake.1.wait_for(&mut g, interval);
            }
        })
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        let _g = self.wake.0.lock();
        self.wake.1.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::report::EngineStats;

    fn snap(running: bool) -> EngineSnapshot {
        let mut stats = EngineStats::stopped();
        stats.running = running;
        EngineSnapshot { stats, config: EngineConfig::default(), network: true, session: 7, conn_events: vec![] }
    }

    /// 스레드 정책(P1-21 2단계): 타임라인에는 세션에서 값이 바뀐 줄에만 들어가고, 세션이 바뀌면 다시 들어간다.
    #[test]
    fn thread_policy_is_recorded_only_when_it_changes() {
        use crate::sys::placement::{PolicyResult, ThreadPolicy, ThreadPolicyStats};
        let pol = |q| ThreadPolicyStats { worker: ThreadPolicy { high_qos: q, p_cores: PolicyResult::Off }, net_recv: None };
        let snap = |session: u64, q| {
            let mut s = super::tests::snap(true);
            s.session = session;
            s.stats.thread_policy = Some(pol(q));
            s
        };
        let d = Diagnostics::new(16);
        d.record(1_000, Some(snap(5, PolicyResult::Off))); // 첫 샘플(적용 전)
        d.record(2_000, Some(snap(5, PolicyResult::Applied))); // 적용 뒤
        d.record(3_000, Some(snap(5, PolicyResult::Applied)));
        d.record(4_000, Some(snap(6, PolicyResult::Applied))); // 새 세션
        d.record(5_000, Some(snap(6, PolicyResult::Applied)));
        d.with_timeline(|tl, _, _| {
            let got: Vec<_> = tl.iter().map(|s| s.thread_policy).collect();
            assert_eq!(got, vec![Some(pol(PolicyResult::Off)), Some(pol(PolicyResult::Applied)), None, Some(pol(PolicyResult::Applied)), None]);
            assert_eq!(timeline::session_policy(tl, 5), Some(pol(PolicyResult::Applied)));
            assert_eq!(timeline::session_policy(tl, 6), Some(pol(PolicyResult::Applied)));
            assert_eq!(timeline::session_policy(tl, 7), None);
            // 줄 형식: 값이 없는 줄에는 키가 없다
            let lines: Vec<String> = timeline::to_jsonl(tl).lines().map(str::to_string).collect();
            assert!(lines[1].contains("\"threadPolicy\":{\"worker\":{\"highQos\":\"applied\",\"pCores\":\"off\"},\"netRecv\":null}"), "{}", lines[1]);
            assert!(!lines[2].contains("threadPolicy"), "{}", lines[2]);
        });
        // 링이 넘쳐 값이 있는 줄을 버리면 같은 세션의 다음 줄이 물려받는다
        let mut dropped = timeline::Sample::from_stats(1, true, 9, &snap(9, PolicyResult::Applied).stats);
        dropped.thread_policy = Some(pol(PolicyResult::Applied));
        let mut next = timeline::Sample::from_stats(2, true, 9, &super::tests::snap(true).stats);
        timeline::Sample::fold_dropped(dropped.clone(), &mut next);
        assert_eq!(next.thread_policy, Some(pol(PolicyResult::Applied)));
        let mut other = timeline::Sample::from_stats(2, true, 10, &super::tests::snap(true).stats);
        timeline::Sample::fold_dropped(dropped, &mut other);
        assert_eq!(other.thread_policy, None, "다른 세션으로는 옮기지 않는다");
    }

    /// 타임라인 `corePlacement`는 같은 세션의 직전 샘플 뒤로 센 구간 수다(#191 리뷰 L1). 세션 첫 샘플·재시작 첫 샘플은 시작부터,
    /// 정지 표시 샘플과 배치가 없는 엔진(비Windows)은 키가 없다. 계약 `EngineStats.corePlacement`(누적 비율)는 그대로다.
    #[test]
    fn core_placement_is_interval_counts_per_session() {
        use crate::sys::placement::{CorePlacementStats, ThreadPlacement};
        let at = |session, w: [u64; 3], r: [u64; 3]| {
            let mut s = snap(true);
            s.session = session;
            s.stats.core_placement = Some(CorePlacementStats {
                worker: ThreadPlacement::from_counts(w[0], w[1], w[2]),
                net_recv: Some(ThreadPlacement::from_counts(r[0], r[1], r[2])),
            });
            s
        };
        let d = Diagnostics::new(10);
        d.record(1_000, Some(at(7, [100, 0, 0], [10, 0, 0])));
        d.record(2_000, Some(at(7, [150, 40, 10], [10, 5, 0])));
        d.record(3_000, None);
        d.record(4_000, Some(at(9, [30, 0, 0], [2, 0, 0])));
        d.record(5_000, Some(snap(true))); // 배치 없음(비Windows) → 키 없음, 다음 차분 기준도 지움
        d.record(6_000, Some(at(9, [60, 0, 0], [4, 0, 0])));
        d.with_timeline(|tl, _, _| {
            let lines: Vec<String> = timeline::to_jsonl(tl).lines().map(str::to_owned).collect();
            let cp = |i: usize| serde_json::from_str::<serde_json::Value>(&lines[i]).unwrap().get("corePlacement").cloned();
            assert_eq!(cp(0), Some(serde_json::json!({"worker": [100, 0, 0], "netRecv": [10, 0, 0]})));
            assert_eq!(cp(1), Some(serde_json::json!({"worker": [50, 40, 10], "netRecv": [0, 5, 0]})));
            assert_eq!(cp(2), None, "정지 표시 샘플");
            assert_eq!(cp(3), Some(serde_json::json!({"worker": [30, 0, 0], "netRecv": [2, 0, 0]})), "재시작은 0부터");
            assert_eq!(cp(4), None);
            assert_eq!(cp(5), Some(serde_json::json!({"worker": [60, 0, 0], "netRecv": [4, 0, 0]})));
        });
    }

    /// 60분 링이 넘쳐 앞 샘플을 버려도 보이는 세션 첫 줄의 corePlacement는 시작부터다 — 버린 구간 값이 같은 세션의 다음 줄에
    /// 합쳐진다(#203 리뷰 L1). 그래서 세션의 줄을 모두 더하면 시작부터의 누적이다. 다른 세션 샘플에는 합치지 않는다.
    #[test]
    fn ring_overflow_keeps_session_first_line_from_start() {
        use crate::sys::placement::{CorePlacementStats, ThreadPlacement};
        let at = |session, w: u64| {
            let mut s = snap(true);
            s.session = session;
            s.stats.core_placement = Some(CorePlacementStats { worker: ThreadPlacement::from_counts(w, 0, w / 10), net_recv: None });
            s
        };
        let d = Diagnostics::new(3);
        // 세션 5: 누적 10, 25, 45, 70, 100 (구간 10, 15, 20, 25, 30)
        for (i, w) in [10u64, 25, 45, 70, 100].into_iter().enumerate() {
            d.record(1_000 * (i as u64 + 1), Some(at(5, w)));
        }
        let worker = |line: &str| serde_json::from_str::<serde_json::Value>(line).unwrap()["corePlacement"]["worker"].clone();
        d.with_timeline(|tl, _, _| {
            assert_eq!(tl.dropped(), 2);
            let jsonl = timeline::to_jsonl(tl);
            let lines: Vec<&str> = jsonl.lines().collect();
            assert_eq!(worker(lines[0]), serde_json::json!([45, 0, 4]), "보이는 첫 줄 = 시작부터(버린 10·15 + 20, LP-E 1+1+2)");
            assert_eq!(worker(lines[1]), serde_json::json!([25, 0, 3]));
            assert_eq!(worker(lines[2]), serde_json::json!([30, 0, 3]));
            assert_eq!(timeline::session_placement(tl, 5).unwrap().worker, [100, 0, 10], "줄 합 = 누적");
        });
        // 세션이 바뀌면 버린 앞 세션 값은 새 세션 줄에 합치지 않는다
        d.record(6_000, None);
        d.record(7_000, Some(at(6, 8)));
        d.record(8_000, Some(at(6, 9)));
        d.with_timeline(|tl, _, _| {
            let jsonl = timeline::to_jsonl(tl);
            let lines: Vec<&str> = jsonl.lines().collect();
            assert_eq!(serde_json::from_str::<serde_json::Value>(lines[0]).unwrap()["running"], false);
            assert_eq!(worker(lines[1]), serde_json::json!([8, 0, 0]));
            assert_eq!(timeline::session_placement(tl, 6).unwrap().worker, [9, 0, 0]);
        });
    }

    #[test]
    fn conn_events_recorded_once_per_session() {
        use crate::net::connlog::{ConnLog, ConnOutcome, ConnPhase};
        let mut log = ConnLog::default();
        log.push(1_500, "peer-bob", ConnPhase::Connect, ConnOutcome::Relay, 1_834);
        let with = |log: &ConnLog, session| EngineSnapshot { session, conn_events: log.events(), ..snap(true) };
        let d = Diagnostics::new(10);
        d.record(2_000, Some(with(&log, 7)));
        d.record(3_000, Some(with(&log, 7))); // 같은 이벤트: 다시 넣지 않음
        log.push(3_200, "peer-bob", ConnPhase::Switch, ConnOutcome::Public, 1_200);
        d.record(4_000, Some(with(&log, 7)));
        // 재시작: 새 Net의 seq는 1부터
        let mut fresh = ConnLog::default();
        fresh.push(4_500, "peer-bob", ConnPhase::Connect, ConnOutcome::Failed, 10_000);
        d.record(5_000, Some(with(&fresh, 9)));
        d.with_timeline(|tl, _, _| {
            let got: Vec<Vec<(u64, ConnPhase, ConnOutcome, u32)>> =
                tl.iter().map(|s| s.conn.iter().map(|c| (c.t, c.phase, c.result, c.ms)).collect()).collect();
            assert_eq!(
                got,
                vec![
                    vec![(1_500, ConnPhase::Connect, ConnOutcome::Relay, 1_834)],
                    vec![],
                    vec![(3_200, ConnPhase::Switch, ConnOutcome::Public, 1_200)],
                    vec![(4_500, ConnPhase::Connect, ConnOutcome::Failed, 10_000)],
                ]
            );
            let jsonl = timeline::to_jsonl(tl);
            let lines: Vec<&str> = jsonl.lines().collect();
            assert!(lines[0].contains(r#""conn":[{"t":1500,"peer":"id-"#), "{}", lines[0]);
            assert!(lines[0].contains(r#""phase":"connect","result":"relay","ms":1834}]"#), "{}", lines[0]);
            assert!(!jsonl.contains("peer-bob"), "피어 id는 가명");
            assert!(!lines[1].contains(r#""conn""#), "이벤트 없는 샘플엔 conn 키가 없다(기존 줄 형식)");
        });
    }

    /// 정지한 엔진의 마지막 이벤트(정지 때 `aborted`)는 정지 표시 샘플에 붙고, 이미 기록한 것은 다시 넣지 않는다.
    /// 새 세션이 시작된 뒤에 와도 옛 세션 기준으로 거른다(#145 M-1).
    #[test]
    fn ended_engine_events_go_to_stop_marker_once() {
        use crate::net::connlog::{ConnLog, ConnOutcome, ConnPhase};
        let mut log = ConnLog::default();
        log.push(1_500, "bob", ConnPhase::Connect, ConnOutcome::Lan, 30);
        let with = |log: &ConnLog, session| EngineSnapshot { session, conn_events: log.events(), ..snap(true) };
        let d = Diagnostics::new(10);
        d.record(2_000, Some(with(&log, 7)));
        // 엔진 정지: carol 연결 중 → aborted
        log.push(2_400, "carol", ConnPhase::Connect, ConnOutcome::Aborted, 3_000);
        d.record_ended(vec![EndedConn { session: 7, events: log.events() }]);
        d.record(3_000, None);
        // 1초 안에 재시작한 경우: 새 세션 샘플 뒤에 옛 세션의 마지막 이벤트가 와도 bob은 다시 넣지 않는다
        d.record(4_000, Some(with(&ConnLog::default(), 9)));
        d.record_ended(vec![EndedConn { session: 7, events: log.events() }]);
        d.record(5_000, Some(with(&ConnLog::default(), 9)));
        d.with_timeline(|tl, _, _| {
            type Row = (u64, bool, Vec<(ConnPhase, ConnOutcome, u32)>);
            let got: Vec<Row> =
                tl.iter().map(|s| (s.t, s.running, s.conn.iter().map(|c| (c.phase, c.result, c.ms)).collect())).collect();
            assert_eq!(
                got,
                vec![
                    (2_000, true, vec![(ConnPhase::Connect, ConnOutcome::Lan, 30)]),
                    (3_000, false, vec![(ConnPhase::Connect, ConnOutcome::Aborted, 3_000)]),
                    (4_000, true, vec![]),
                    (5_000, true, vec![]),
                ]
            );
            assert!(timeline::to_jsonl(tl).contains(r#""result":"aborted""#));
        });
    }

    #[test]
    fn session_marks_fast_restart_and_stop_keeps_network() {
        let d = Diagnostics::new(10);
        d.record(1, Some(snap(true)));
        // 1초 안에 재시작: 정지 표시는 없지만 session이 바뀐다
        d.record(2, Some(EngineSnapshot { session: 9, ..snap(true) }));
        d.record(3, None);
        d.with_timeline(|tl, _, _| {
            let v: Vec<_> = tl.iter().map(|s| (s.t, s.session, s.network, s.running)).collect();
            assert_eq!(v, vec![(1, 7, true, true), (2, 9, true, true), (3, 9, true, false)]);
        });
    }

    #[test]
    fn export_guard_is_exclusive() {
        let d = Diagnostics::new(1);
        let g = d.begin_export().expect("첫 내보내기");
        assert!(d.begin_export().is_none(), "진행 중이면 거절");
        drop(g);
        assert!(d.begin_export().is_some(), "끝나면 다시 가능");
    }

    #[test]
    fn records_active_and_one_stop_marker() {
        let d = Diagnostics::new(10);
        d.record(1, None); // 엔진 없음: 기록 안 함
        d.record(2, Some(snap(true)));
        d.record(3, Some(snap(false))); // 장치 분실 등: 엔진은 있으나 running=false도 기록
        d.record(4, None); // 정지 표시
        d.record(5, None); // 계속 없음: 기록 안 함
        d.with_timeline(|tl, cfg, active| {
            let ts: Vec<_> = tl.iter().map(|s| (s.t, s.running, s.network)).collect();
            // 정지 표시 샘플도 끝난 세션의 network 값을 유지한다
            assert_eq!(ts, vec![(2, true, true), (3, false, true), (4, false, true)]);
            assert!(cfg.is_some(), "마지막 세션 설정은 정지 뒤에도 남는다");
            assert!(!active);
        });
    }

    #[test]
    fn recorder_thread_samples_and_stops() {
        use crate::engine::device::fake::FakeBackend;
        use crate::manager::EngineManager;
        use crate::net::NetOptions;
        let mgr = Arc::new(EngineManager::new(Arc::new(FakeBackend::silence(48_000)), NetOptions::default()));
        mgr.start_monitor_only(EngineConfig::default()).unwrap();
        let d = Arc::new(Diagnostics::new(10));
        let m = mgr.clone();
        let m2 = mgr.clone();
        let h = d.spawn_recorder(move || m.snapshot(), move || m2.take_ended_conn()).unwrap();
        // 첫 기록은 즉시
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while d.with_timeline(|tl, _, _| tl.is_empty()) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        d.shutdown();
        h.join().unwrap();
        mgr.stop();
        d.with_timeline(|tl, cfg, _| {
            assert!(!tl.is_empty());
            let s = tl.iter().next().unwrap();
            assert!(!s.network, "테스트 모드");
            assert_eq!(s.backend, "fake");
            assert_eq!(cfg.unwrap().buffer_size, 128);
        });
    }
}
