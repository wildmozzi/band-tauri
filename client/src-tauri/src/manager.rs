//! 엔진 수명 관리(커맨드 계층과 분리, 리뷰 H3).
//!
//! - 시작(STUN, 장치 열기)과 정지(스레드 join)는 **락 밖**에서 한다. 락은 슬롯을 바꿀 때만
//!   짧게 잡는다. 그래서 시작이 수 초 걸려도 `stats`/`add_peer` 같은 커맨드가 막히지 않는다.
//! - 세대 번호로 시작 중에 들어온 `stop`/다른 `start`를 처리한다. 늦게 끝난 시작은 버린다.
//! - 테스트 모드는 시작할 때 모니터를 켜고, 끝나면 원래 값으로 되돌린다(리뷰 L4).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::engine::config::EngineConfig;
use crate::engine::measure::{EchoReport, LoopbackReport};
use crate::engine::device::AudioBackend;
use crate::engine::mixer::MixControls;
use crate::engine::stats::EngineStats;
use crate::engine::Engine;
use crate::net::peer::LocalDescriptor;
use crate::net::NetOptions;

#[derive(Default)]
struct Slot {
    engine: Option<Engine>,
    generation: u64,
    starting: bool,
    monitor_before_test: Option<bool>,
    /// 진행 중인 시작의 취소 플래그.
    cancel: Option<Arc<AtomicBool>>,
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
        let job = self.with_engine(|e| {
            if e.has_network() {
                return Err("합주 중에는 루프백 측정을 할 수 없습니다. 연주를 멈추고 테스트 모드에서 재세요".to_string());
            }
            Ok(e.loopback_job())
        })?;
        let (_g, cancel) = self.begin_measure()?;
        job.run(runs, &cancel)
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
        let (gen, old) = {
            let mut s = self.slot.lock();
            s.generation += 1;
            s.starting = true;
            // 진행 중인 이전 시작은 취소(장치 열기 뒤·STUN 중에 일찍 포기하고 장치를 놓는다)
            if let Some(c) = s.cancel.replace(cancel.clone()) {
                c.store(true, Ordering::SeqCst);
            }
            if peer_id.is_some() {
                self.restore_monitor(&mut s);
            } else {
                if s.monitor_before_test.is_none() {
                    s.monitor_before_test = Some(self.controls.monitor_enabled.load(Ordering::Relaxed));
                }
                self.controls.monitor_enabled.store(true, Ordering::Relaxed);
            }
            (s.generation, s.engine.take())
        };
        drop(old); // 슬롯 락 밖에서 정지(join)

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
        s.starting = false;
        s.cancel = None;
        let engine = result?;
        let desc = engine.descriptor().cloned().unwrap_or(LocalDescriptor {
            ssrc: 0,
            key: String::new(),
            candidates: Vec::new(),
        });
        s.engine = Some(engine);
        Ok(desc)
    }

    /// 엔진 정지(시작 중이면 그 시작을 취소). 블로킹(join)이므로 비UI 스레드에서 부른다.
    pub fn stop(&self) {
        let old = {
            let mut s = self.slot.lock();
            s.generation += 1;
            s.starting = false;
            if let Some(c) = s.cancel.take() {
                c.store(true, Ordering::SeqCst);
            }
            self.restore_monitor(&mut s);
            s.engine.take()
        };
        drop(old);
    }

    /// 테스트 모드 엔진만 정지한다.
    pub fn test_stop(&self) {
        let old = {
            let mut s = self.slot.lock();
            if s.engine.as_ref().is_some_and(|e| !e.has_network()) {
                self.restore_monitor(&mut s);
                s.engine.take()
            } else {
                None
            }
        };
        drop(old);
    }

    pub fn is_starting(&self) -> bool {
        self.slot.lock().starting
    }

    /// start/stop이 불릴 때마다 오르는 세대. 테스트가 "다음 start가 이전 시작을 취소했다"를 벽시계 없이 기다리는 데 쓴다.
    pub fn start_generation(&self) -> u64 {
        self.slot.lock().generation
    }

    pub fn with_engine<T>(&self, f: impl FnOnce(&mut Engine) -> Result<T, String>) -> Result<T, String> {
        let mut s = self.slot.lock();
        if s.starting && s.engine.is_none() {
            return Err("엔진 시작 중입니다".into());
        }
        let engine = s.engine.as_mut().ok_or("엔진이 실행 중이 아닙니다")?;
        f(engine)
    }

    /// 진단 기록용(P2-5): 엔진이 있으면 통계·설정·네트워크 여부. 슬롯 락은 `stats()`만큼만 잡는다.
    pub fn snapshot(&self) -> Option<crate::diag::Snapshot> {
        let s = self.slot.lock();
        s.engine.as_ref().map(|e| crate::diag::Snapshot {
            stats: e.stats(),
            config: e.config().clone(),
            network: e.has_network(),
            session: s.generation,
        })
    }

    pub fn stats(&self) -> EngineStats {
        match self.slot.lock().engine.as_ref() {
            Some(e) => e.stats(),
            None => EngineStats::stopped(),
        }
    }
}
