//! 엔진 수명 관리(커맨드 계층과 분리, 리뷰 H3).
//!
//! - 시작(STUN, 장치 열기)과 정지(스레드 join)는 **락 밖**에서 한다. 락은 슬롯을 바꿀 때만
//!   짧게 잡는다. 그래서 시작이 수 초 걸려도 `stats`/`add_peer` 같은 커맨드가 막히지 않는다.
//! - 세대 번호로 시작 중에 들어온 `stop`/다른 `start`를 처리한다. 늦게 끝난 시작은 버린다.
//! - 테스트 모드는 시작할 때 모니터를 켜고, 끝나면 원래 값으로 되돌린다(리뷰 L4).
//! - 수명 상태는 `Phase`(engine-architecture.md 11절). `manager`가 전이시키는 것은 Idle·Starting·Running 셋이다.
//!   Degraded는 엔진이 원자값으로 보고하는 값(`EngineStats::device_lost`)이고, Stopping은 슬롯에서 꺼내
//!   락 밖에서 정지(`retire`)하는 지역 변수다.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::engine::config::EngineConfig;
use crate::engine::measure::{EchoReport, LoopbackReport, MeasureVerdict};
use crate::engine::metronome::{MAX_LOOPBACK_OFFSET_MS, MIN_LOOPBACK_OFFSET_MS};
use crate::engine::device::AudioBackend;
use crate::engine::mixer::{clamp_gain, MixControls};
use crate::engine::report::{EndedConn, EngineSnapshot, EngineStats};
use crate::engine::Engine;
use crate::net::peer::LocalDescriptor;
use crate::net::relay::resolve_relay_addr;
use crate::net::{NetOptions, RelaySetting};

/// `manager`가 전이시키는 엔진 수명 상태(engine-architecture.md 11절).
///
/// - Degraded(장치 분실)는 여기 없다: 엔진 스레드가 원자값으로 보고하고(`EngineSnapshot.stats.device_lost`)
///   `manager`는 전이시키지 않는다. 그동안에도 `Running`이다.
/// - Stopping도 여기 없다: `Phase::leave`로 꺼낸 엔진을 호출자가 **슬롯 락 밖**에서 정지한다(`retire`: join, 합주 엔진이면 마지막 연결 이벤트 기록).
#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    /// 장치 열기·STUN·스레드 시작 중(블로킹, `start_lock` 직렬화). `cancel`은 그 시작의 취소 플래그.
    Starting { cancel: Arc<AtomicBool> },
    Running(Box<Engine>),
}

impl Phase {
    /// Idle로 바꾸고 나가는 것을 돌려준다. 진행 중인 시작은 취소하고(장치 열기 뒤·STUN 중에 일찍 포기하고 장치를
    /// 놓는다), 실행 중이던 엔진은 돌려준다 — 호출자가 슬롯 락 밖에서 정지한다(Stopping, `retire`).
    fn leave(&mut self) -> Option<Engine> {
        match std::mem::take(self) {
            Phase::Idle => None,
            Phase::Starting { cancel } => {
                cancel.store(true, Ordering::SeqCst);
                None
            }
            Phase::Running(e) => Some(*e),
        }
    }

    fn engine(&self) -> Option<&Engine> {
        match self {
            Phase::Running(e) => Some(e),
            _ => None,
        }
    }
}

#[derive(Default)]
struct Slot {
    phase: Phase,
    /// start/stop마다 오른다. 블로킹 시작이 끝났을 때 다르면 그 결과를 버린다.
    generation: u64,
    monitor_before_test: Option<bool>,
}

pub struct EngineManager {
    slot: Mutex<Slot>,
    /// 시작(장치 열기~설치)을 직렬화한다. 이전 시작이 장치·포트를 놓기 전에 새 시작이 같은 장치를
    /// 열지 않게 한다(PR #5 M-4). 슬롯 락과 달리 블로킹 작업 동안 쥔다(다른 커맨드는 막지 않음).
    start_lock: Mutex<()>,
    controls: Arc<MixControls>,
    backend: Arc<dyn AudioBackend>,
    net_options: NetOptions,
    /// 진행 중인 측정(루프백/에코)의 취소 플래그. 한 번에 하나만 돈다.
    measure: Mutex<Option<Arc<AtomicBool>>>,
    /// 마지막 루프백 보정값(`Engine::device_key`, `offsetMs`). 테스트 모드에서 잰 값을 같은 장치 구성의
    /// 합주 엔진에도 적용한다(메트로놈 숨은 출력 지연 보정, PR #34 M2(a)). 장치 구성이 바뀌면 쓰지 않는다.
    loopback_cal: Mutex<Option<(String, f64)>>,
    /// 정지한 합주 엔진의 마지막 연결 이벤트(P1-18). 진단 기록이 `take_ended_conn`으로 가져간다.
    ended_conn: Mutex<Vec<EndedConn>>,
}

/// 측정 진행 표시를 지우는 가드.
struct MeasureGuard<'a>(&'a Mutex<Option<Arc<AtomicBool>>>);

impl Drop for MeasureGuard<'_> {
    fn drop(&mut self) {
        *self.0.lock() = None;
    }
}

impl EngineManager {
    pub fn new(backend: Arc<dyn AudioBackend>, net_options: NetOptions) -> Self {
        Self {
            slot: Mutex::new(Slot::default()),
            start_lock: Mutex::new(()),
            controls: Arc::new(MixControls::default()),
            backend,
            net_options,
            measure: Mutex::new(None),
            loopback_cal: Mutex::new(None),
            ended_conn: Mutex::new(Vec::new()),
        }
    }

    fn begin_measure(&self) -> Result<(MeasureGuard<'_>, Arc<AtomicBool>), String> {
        let mut m = self.measure.lock();
        if m.is_some() {
            return Err("이미 측정 중입니다".into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *m = Some(cancel.clone());
        Ok((MeasureGuard(&self.measure), cancel))
    }

    /// 하드웨어 루프백 측정(설계 4절). 엔진 락은 작업을 꺼낼 때만 잡는다(측정 중에도 다른 커맨드가 돈다).
    /// 블로킹(반복당 약 0.7초)이므로 비UI 스레드에서 부른다.
    pub fn measure_loopback(&self, runs: u32) -> Result<LoopbackReport, String> {
        // 합주 중에는 거부한다: 측정 동안 장치 출력이 처프로 바뀌어 모니터·피어 소리가 끊긴다(PR #13 L5).
        let (job, key) = self.with_engine(|e| {
            if e.has_network() {
                return Err("합주 중에는 루프백 측정을 할 수 없습니다. 연주를 멈추고 테스트 모드에서 재세요".to_string());
            }
            Ok((e.loopback_job(), e.device_key()))
        })?;
        let (_g, cancel) = self.begin_measure()?;
        let report = job.run(runs, &cancel)?;
        // 판정이 정상이면 보정값을 이 장치 구성의 메트로놈 출력 지연 보정으로 기억·적용한다(PR #34 M2(a)).
        // 범위(−50..200ms) 밖이면 잘라서 "부분 보정"으로 쓰지 않고 기억하지 않는다 — `engine_set_loopback_offset`이
        // 같은 값을 거부하는 것과 맞춘다(PR #57 L1). 실패·범위 밖이면 이전 값이 그대로 남는다.
        if let (MeasureVerdict::Ok, Some(o)) = (report.verdict, report.offset_ms) {
            if !(MIN_LOOPBACK_OFFSET_MS..=MAX_LOOPBACK_OFFSET_MS).contains(&o) {
                return Ok(report);
            }
            *self.loopback_cal.lock() = Some((key.clone(), o));
            let _ = self.with_engine(|e| if e.device_key() == key { e.set_loopback_offset(Some(o)) } else { Ok(()) });
        }
        Ok(report)
    }

    /// 루프백 보정값 직접 설정·해제(설계 4절 `engine_set_loopback_offset`). 실행 중인 엔진의 장치 구성에 묶어 기억한다.
    /// 앱이 장치별로 저장해 둔 값을 다시 넣거나(앱 재시작), 사용자가 보정을 끌 때(None) 쓴다.
    pub fn set_loopback_offset(&self, offset_ms: Option<f64>) -> Result<(), String> {
        let key = self.with_engine(|e| {
            e.set_loopback_offset(offset_ms)?;
            Ok(e.device_key())
        })?;
        *self.loopback_cal.lock() = offset_ms.map(|o| (key, o));
        Ok(())
    }

    /// 에코 피어 측정(측정하는 쪽). 상대 동의를 `wait`까지 기다린다. 블로킹.
    pub fn measure_echo(&self, peer_id: &str, runs: u32, wait: std::time::Duration) -> Result<EchoReport, String> {
        let job = self.with_engine(|e| e.echo_job(peer_id))?;
        let (_g, cancel) = self.begin_measure()?;
        job.run(runs, wait, &cancel)
    }

    /// 진행 중인 측정을 취소한다(없으면 아무것도 안 함).
    pub fn cancel_measure(&self) {
        if let Some(c) = self.measure.lock().as_ref() {
            c.store(true, Ordering::SeqCst);
        }
    }

    pub fn controls(&self) -> &Arc<MixControls> {
        &self.controls
    }

    pub fn backend(&self) -> &Arc<dyn AudioBackend> {
        &self.backend
    }

    /// 릴레이 설정(설계 4절). 서버의 `relay-token`을 받을 때마다(최초·갱신) 부른다. 블로킹(DNS)이므로 비UI 스레드에서.
    /// DNS 조회는 엔진 락 밖에서 한다. 같은 host:port 문자열이면 처음 해석한 주소를 유지한다(토큰 갱신마다 다시
    /// 풀면 라운드로빈 A 레코드에서 릴레이가 바뀌어 끊긴다, M3).
    pub fn set_relay(&self, host: &str, token: &str, expires_in: Option<std::time::Duration>, bind_key: Option<&str>) -> Result<(), String> {
        let known = self.with_engine(|e| Ok(e.relay_addr_for_host(host)))?;
        let addr = match known {
            Some(a) => a,
            None => resolve_relay_addr(host)?,
        };
        self.with_engine(|e| e.set_relay(Some(RelaySetting { host, addr, token, expires_in, bind_key })))
    }

    /// 릴레이 해제.
    pub fn clear_relay(&self) -> Result<(), String> {
        self.with_engine(|e| e.set_relay(None))
    }

    /// 마이크 입력 뮤트(로컬 송신·모니터).
    pub fn set_input_muted(&self, muted: bool) {
        self.controls.input_muted.store(muted, Ordering::Relaxed);
    }

    /// 내 입력 모니터. 게인은 믹서 범위로 자른다.
    pub fn set_monitor(&self, enabled: bool, gain: f32) {
        self.controls.monitor_enabled.store(enabled, Ordering::Relaxed);
        self.controls.monitor_gain.store(clamp_gain(gain));
    }

    /// 마스터 게인(믹서 범위로 자른다).
    pub fn set_master_gain(&self, gain: f32) {
        self.controls.master_gain.store(clamp_gain(gain));
    }

    /// 메트로놈 클릭 볼륨·뮤트(로컬 전용, 엔진 재시작 간 유지). 게인은 믹서 범위로 자른다.
    pub fn set_metronome_gain(&self, gain: f32, muted: bool) {
        self.controls.click_gain.store(clamp_gain(gain));
        self.controls.click_muted.store(muted, Ordering::Relaxed);
    }

    fn restore_monitor(&self, s: &mut Slot) {
        if let Some(v) = s.monitor_before_test.take() {
            self.controls.monitor_enabled.store(v, Ordering::Relaxed);
        }
    }

    /// 실제 세션 시작. 기존 엔진(테스트 모드 포함)은 정지한다. 블로킹이므로 비UI 스레드에서 부른다.
    pub fn start(&self, config: EngineConfig, peer_id: String) -> Result<LocalDescriptor, String> {
        self.start_inner(config, Some(peer_id))
    }

    /// 네트워크 없이 모니터만(테스트 모드).
    pub fn test_start(&self, config: EngineConfig) -> Result<(), String> {
        self.start_inner(config, None).map(|_| ())
    }

    fn start_inner(&self, config: EngineConfig, peer_id: Option<String>) -> Result<LocalDescriptor, String> {
        let cancel = Arc::new(AtomicBool::new(false));
        let (gen, old_session, old) = {
            let mut s = self.slot.lock();
            // 꺼낸 엔진의 진단 세션은 증가 전 세대(P1-18)
            let old_session = s.generation;
            s.generation += 1;
            // 진행 중인 이전 시작은 취소하고, 실행 중이던 엔진은 꺼내 락 밖에서 정지한다(retire)
            let old = s.phase.leave();
            s.phase = Phase::Starting { cancel: cancel.clone() };
            if peer_id.is_some() {
                self.restore_monitor(&mut s);
            } else {
                if s.monitor_before_test.is_none() {
                    s.monitor_before_test = Some(self.controls.monitor_enabled.load(Ordering::Relaxed));
                }
                self.controls.monitor_enabled.store(true, Ordering::Relaxed);
            }
            (s.generation, old_session, old)
        };
        self.retire(old, old_session); // 슬롯 락 밖에서 정지(join)

        // 이전 시작이 끝나(그리고 취소된 결과를 버려 장치를 놓을 때까지) 기다린다.
        let _serial = self.start_lock.lock();
        if self.slot.lock().generation != gen {
            return Err("엔진 시작이 취소되었습니다".into());
        }

        let network = peer_id.map(|id| (id, self.net_options.clone()));
        let result = Engine::start_cancellable(config, self.backend.clone(), self.controls.clone(), network, Some(cancel));

        let mut s = self.slot.lock();
        if s.generation != gen {
            drop(s);
            // 그 사이 stop/start가 들어왔다: 이 결과는 버린다. start_lock을 쥔 채 정지하므로
            // 다음 시작은 장치가 닫힌 뒤에 연다.
            drop(result);
            return Err("엔진 시작이 취소되었습니다".into());
        }
        // 세대가 같으면 그 사이 아무도 슬롯을 바꾸지 않았다: 아직 이 시작의 Starting이다
        debug_assert!(matches!(s.phase, Phase::Starting { .. }));
        s.phase = Phase::Idle;
        let engine = result?;
        // 같은 장치 구성에서 잰 루프백 보정값이 있으면 메트로놈 출력 지연 보정에 쓴다(PR #34 M2(a))
        if let Some((key, o)) = self.loopback_cal.lock().clone() {
            if engine.device_key() == key {
                let _ = engine.set_loopback_offset(Some(o));
            }
        }
        let desc = engine.descriptor().cloned().unwrap_or(LocalDescriptor {
            ssrc: 0,
            key: String::new(),
            candidates: Vec::new(),
        });
        s.phase = Phase::Running(Box::new(engine));
        Ok(desc)
    }

    /// 엔진 정지(시작 중이면 그 시작을 취소). 블로킹(join)이므로 비UI 스레드에서 부른다.
    pub fn stop(&self) {
        let (old_session, old) = {
            let mut s = self.slot.lock();
            let old_session = s.generation;
            s.generation += 1;
            self.restore_monitor(&mut s);
            (old_session, s.phase.leave())
        };
        self.retire(old, old_session);
    }

    /// 꺼낸 엔진을 정지(join)하고, 합주 엔진이면 마지막 연결 이벤트(정지 때 `aborted` 포함)를 진단용으로 남긴다
    /// (P1-18, #145 M-1). 슬롯 락 밖에서 부른다.
    fn retire(&self, old: Option<Engine>, session: u64) {
        let Some(mut e) = old else { return };
        let network = e.has_network();
        e.stop();
        if network {
            self.ended_conn.lock().push(EndedConn { session, events: e.conn_events() });
        }
    }

    /// 정지한 합주 엔진의 마지막 연결 이벤트를 가져간다(진단 기록 스레드가 1초마다, 한 번만).
    pub fn take_ended_conn(&self) -> Vec<EndedConn> {
        std::mem::take(&mut *self.ended_conn.lock())
    }

    /// 테스트 모드 엔진만 정지한다.
    pub fn test_stop(&self) {
        let old = {
            let mut s = self.slot.lock();
            if s.phase.engine().is_some_and(|e| !e.has_network()) {
                self.restore_monitor(&mut s);
                s.phase.leave()
            } else {
                None
            }
        };
        drop(old);
    }

    pub fn is_starting(&self) -> bool {
        matches!(self.slot.lock().phase, Phase::Starting { .. })
    }

    /// start/stop이 불릴 때마다 오르는 세대. 테스트가 "다음 start가 이전 시작을 취소했다"를 벽시계 없이 기다리는 데 쓴다.
    pub fn start_generation(&self) -> u64 {
        self.slot.lock().generation
    }

    pub fn with_engine<T>(&self, f: impl FnOnce(&mut Engine) -> Result<T, String>) -> Result<T, String> {
        match &mut self.slot.lock().phase {
            Phase::Idle => Err("엔진이 실행 중이 아닙니다".into()),
            Phase::Starting { .. } => Err("엔진 시작 중입니다".into()),
            Phase::Running(engine) => f(engine),
        }
    }

    /// 녹음 시작(P2-4). 실행 중인 엔진(합주·테스트 모드)에서.
    pub fn record_start(
        &self,
        opts: crate::engine::record::RecordOptions,
        dir: &std::path::Path,
    ) -> Result<crate::engine::record::RecordStats, String> {
        self.with_engine(|e| e.record_start(opts, dir))
    }

    /// 녹음 중 피어 트랙 이름 추가(peerId → 표시 이름).
    pub fn record_set_track_names(&self, names: std::collections::HashMap<String, String>) -> Result<(), String> {
        self.with_engine(|e| e.record_set_track_names(names))
    }

    /// 녹음 정지. 콜백 반환 대기·파일 마무리는 엔진 락 밖에서 한다(그동안 다른 커맨드가 막히지 않음). 블로킹.
    pub fn record_stop(&self) -> Result<crate::engine::record::RecordSummary, String> {
        let rec = self.with_engine(|e| Ok(e.recorder()))?;
        let job = rec.lock().begin_stop()?;
        let r = job.finish();
        rec.lock().reclaim();
        r
    }

    /// 진단 기록용(P2-5): 엔진이 있으면 통계·설정·네트워크 여부. 슬롯 락은 `stats()`만큼만 잡는다.
    pub fn snapshot(&self) -> Option<EngineSnapshot> {
        let s = self.slot.lock();
        s.phase.engine().map(|e| EngineSnapshot {
            stats: e.stats(),
            config: e.config().clone(),
            network: e.has_network(),
            session: s.generation,
            conn_events: e.conn_events(),
        })
    }

    pub fn stats(&self) -> EngineStats {
        match self.slot.lock().phase.engine() {
            Some(e) => e.stats(),
            None => EngineStats::stopped(),
        }
    }
}

/// 11절 사례 표의 전이를 가짜 백엔드로 돈다(⑧). 순서는 벽시계가 아니라 열기 문(`OpenGate`)·세대로 고정하고,
/// 시간 상한(`HANG`)은 행 감지용이다.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::device::fake::{FakeBackend, OpenGate};
    use std::time::{Duration, Instant};

    const HANG: Duration = Duration::from_secs(10);

    #[derive(Debug, PartialEq)]
    enum P {
        Idle,
        Starting,
        Running,
    }

    fn phase(m: &EngineManager) -> P {
        match &m.slot.lock().phase {
            Phase::Idle => P::Idle,
            Phase::Starting { .. } => P::Starting,
            Phase::Running(_) => P::Running,
        }
    }

    /// 지금 `Starting`인 시작의 취소 플래그(같은 Arc). Starting이 아니면 실패.
    fn starting_cancel(m: &EngineManager) -> Arc<AtomicBool> {
        match &m.slot.lock().phase {
            Phase::Starting { cancel } => cancel.clone(),
            _ => panic!("Starting이 아님"),
        }
    }

    fn net() -> NetOptions {
        NetOptions {
            port: 0,
            stun_servers: vec![],
            gather_lan: false,
            extra_candidates: vec![],
            allow_loopback_candidates: true,
            cancel: None,
        }
    }

    fn cfg() -> EngineConfig {
        EngineConfig { buffer_size: 128, ..Default::default() }
    }

    fn wait_until(mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + HANG;
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        f()
    }

    /// 문으로 장치 열기를 붙잡을 수 있고, 배타 장치(동시에 한 번만 열림)인 가짜 백엔드.
    fn gated() -> (Arc<OpenGate>, Arc<AtomicBool>, FakeBackend) {
        let gate = OpenGate::closed();
        let held = Arc::new(AtomicBool::new(false));
        let mut b = FakeBackend::silence(48_000);
        b.open_gate = Some(gate.clone());
        b.exclusive = Some(held.clone());
        (gate, held, b)
    }

    #[test]
    fn leave_cancels_starting_and_returns_engine() {
        let mut p = Phase::Idle;
        assert!(p.leave().is_none());
        let cancel = Arc::new(AtomicBool::new(false));
        p = Phase::Starting { cancel: cancel.clone() };
        assert!(p.leave().is_none());
        assert!(cancel.load(Ordering::SeqCst), "Starting을 떠나면 그 시작을 취소");
        assert!(matches!(p, Phase::Idle));
        let controls = Arc::new(MixControls::default());
        let e = Engine::start(cfg(), Arc::new(FakeBackend::silence(48_000)), controls, None).unwrap();
        p = Phase::Running(Box::new(e));
        assert!(p.leave().is_some());
        assert!(matches!(p, Phase::Idle));
    }

    #[test]
    fn idle_starting_running_idle() {
        // 연주 시작 / 테스트 모드: Idle → Starting → Running, 정지: Running → (Stopping) → Idle
        let (gate, held, b) = gated();
        let m = Arc::new(EngineManager::new(Arc::new(b), net()));
        assert_eq!(phase(&m), P::Idle);
        assert_eq!(m.with_engine(|_| Ok(())).unwrap_err(), "엔진이 실행 중이 아닙니다");
        assert!(m.snapshot().is_none());

        let m2 = m.clone();
        let t = std::thread::spawn(move || m2.test_start(cfg()));
        assert!(gate.wait_entered(1, HANG));
        assert_eq!(phase(&m), P::Starting);
        assert!(m.is_starting());
        assert_eq!(m.with_engine(|_| Ok(())).unwrap_err(), "엔진 시작 중입니다");
        assert!(m.snapshot().is_none() && !m.stats().running);
        assert!(m.controls().monitor_enabled.load(Ordering::Relaxed), "테스트 모드는 모니터를 켬");
        gate.release();
        t.join().unwrap().unwrap();
        assert_eq!(phase(&m), P::Running);
        assert!(!m.is_starting());
        assert!(m.with_engine(|e| Ok(!e.has_network())).unwrap());
        assert!(m.snapshot().is_some_and(|s| !s.network));

        // test_stop은 테스트 모드 엔진만 멈춘다
        m.test_stop();
        assert_eq!(phase(&m), P::Idle);
        assert!(!held.load(Ordering::SeqCst), "정지하면 장치를 놓는다");
        assert!(!m.controls().monitor_enabled.load(Ordering::Relaxed), "모니터 복원");

        // 연주(net 있음)는 test_stop으로 멈추지 않는다
        m.start(cfg(), "me".into()).unwrap();
        assert_eq!(phase(&m), P::Running);
        m.test_stop();
        assert_eq!(phase(&m), P::Running);
        assert!(m.snapshot().is_some_and(|s| s.network));
        m.stop();
        assert_eq!(phase(&m), P::Idle);
        assert!(!held.load(Ordering::SeqCst));
    }

    #[test]
    fn restart_or_stop_while_starting() {
        // 시작 중 다시 시작: Starting → (세대 불일치, 이전 결과 버림) → 새 Starting → Running
        let (gate, held, b) = gated();
        let m = Arc::new(EngineManager::new(Arc::new(b), net()));
        let m1 = m.clone();
        let a = std::thread::spawn(move || m1.start(cfg(), "me".into()));
        assert!(gate.wait_entered(1, HANG));
        let cancel_a = starting_cancel(&m);
        assert!(!cancel_a.load(Ordering::SeqCst));
        let g0 = m.start_generation();
        let m2 = m.clone();
        let b = std::thread::spawn(move || m2.start(cfg(), "me".into()));
        assert!(wait_until(|| m.start_generation() > g0), "두 번째 start가 들어오지 않음");
        // 두 번째 시작은 start_lock에서 기다린다: 여전히 Starting(새 cancel)
        assert_eq!(phase(&m), P::Starting);
        // 재시작은 이전 시작에 취소를 보낸다(장치 열기 뒤·STUN 중에 일찍 포기 — 세대 불일치만으로는 늦게 버린다)
        assert!(cancel_a.load(Ordering::SeqCst), "재시작이 이전 시작의 cancel을 세우지 않음");
        let cancel_b = starting_cancel(&m);
        assert!(!Arc::ptr_eq(&cancel_a, &cancel_b) && !cancel_b.load(Ordering::SeqCst), "새 시작은 새 cancel");
        gate.release();
        let (ra, rb) = (a.join().unwrap(), b.join().unwrap());
        assert_eq!(ra.unwrap_err(), "엔진 시작이 취소되었습니다");
        assert!(rb.is_ok(), "{rb:?}");
        assert_eq!(phase(&m), P::Running);

        // 시작 중 정지: Starting → Idle(그 시작은 결과를 버리고 장치를 놓는다)
        m.stop();
        gate.close();
        let n = gate.entered();
        let m3 = m.clone();
        let c = std::thread::spawn(move || m3.start(cfg(), "me".into()));
        assert!(gate.wait_entered(n + 1, HANG));
        assert_eq!(phase(&m), P::Starting);
        let cancel_c = starting_cancel(&m);
        m.stop();
        assert!(cancel_c.load(Ordering::SeqCst), "stop이 진행 중인 시작의 cancel을 세우지 않음");
        assert_eq!(phase(&m), P::Idle, "stop은 시작을 기다리지 않고 곧바로 Idle");
        assert!(!m.is_starting());
        gate.release();
        assert_eq!(c.join().unwrap().unwrap_err(), "엔진 시작이 취소되었습니다");
        assert_eq!(phase(&m), P::Idle, "늦게 끝난 시작이 슬롯을 되살리지 않는다");
        assert!(!held.load(Ordering::SeqCst), "버린 결과의 장치는 닫혔다");
    }

    #[test]
    fn failed_start_returns_to_idle() {
        // Starting → 실패 → Idle
        let mut b = FakeBackend::silence(48_000);
        b.exclusive = Some(Arc::new(AtomicBool::new(true))); // 이미 다른 곳이 쥔 장치
        let m = EngineManager::new(Arc::new(b), net());
        assert!(m.test_start(cfg()).is_err());
        assert_eq!(phase(&m), P::Idle);
        assert!(!m.is_starting());
        assert!(!m.stats().running);
    }

    #[test]
    fn running_restart_stops_old_engine_first() {
        // ssrc/key-conflict 재시작: Running → Stopping → Starting → Running. 배타 장치라 이전 엔진이 장치를
        // 놓지 않으면 새 열기가 실패한다.
        let (gate, held, b) = gated();
        gate.release();
        let m = EngineManager::new(Arc::new(b), net());
        let d1 = m.start(cfg(), "me".into()).unwrap();
        let d2 = m.start(cfg(), "me".into()).unwrap();
        assert_eq!(phase(&m), P::Running);
        assert_ne!((d1.ssrc, d1.key), (d2.ssrc, d2.key), "새 ssrc·키");
        // 앱 종료: 모든 상태 → Stopping → Idle(Drop)
        drop(m);
        assert!(!held.load(Ordering::SeqCst), "manager drop이 엔진을 정지");
    }

    #[test]
    fn degraded_is_reported_not_transitioned() {
        // 장치 분리: 엔진이 deviceLost를 보고해도 manager는 Running 그대로(전이는 프론트의 stop).
        // 전이 시험과 달리 Degraded 감지는 문·세대가 아니라 폴링이다(엔진의 장치 감시 주기 약 1초, 상한 HANG).
        let mut b = FakeBackend::silence(48_000);
        b.halt_after = Some(Duration::from_millis(200));
        let m = EngineManager::new(Arc::new(b), net());
        m.test_start(cfg()).unwrap();
        assert!(wait_until(|| m.snapshot().is_some_and(|s| s.stats.device_lost)), "deviceLost가 서지 않음");
        assert_eq!(phase(&m), P::Running);
        let s = m.snapshot().unwrap();
        assert!(!s.stats.running);
        assert!(m.with_engine(|_| Ok(())).is_ok(), "Degraded 동안에도 커맨드는 엔진에 닿는다");
        m.stop();
        assert_eq!(phase(&m), P::Idle);
    }
}
