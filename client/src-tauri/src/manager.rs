//! 엔진 수명 관리(커맨드 계층과 분리, 리뷰 H3).
//!
//! - 시작(STUN, 장치 열기)과 정지(스레드 join)는 **락 밖**에서 한다. 락은 슬롯을 바꿀 때만
//!   짧게 잡는다. 그래서 시작이 수 초 걸려도 `stats`/`add_peer` 같은 커맨드가 막히지 않는다.
//! - 세대 번호로 시작 중에 들어온 `stop`/다른 `start`를 처리한다. 늦게 끝난 시작은 버린다.
//! - 테스트 모드는 시작할 때 모니터를 켜고, 끝나면 원래 값으로 되돌린다(리뷰 L4).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::engine::config::EngineConfig;
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
}

pub struct EngineManager {
    slot: Mutex<Slot>,
    controls: Arc<MixControls>,
    backend: Arc<dyn AudioBackend>,
    net_options: NetOptions,
}

impl EngineManager {
    pub fn new(backend: Arc<dyn AudioBackend>, net_options: NetOptions) -> Self {
        Self { slot: Mutex::new(Slot::default()), controls: Arc::new(MixControls::default()), backend, net_options }
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
        let (gen, old) = {
            let mut s = self.slot.lock();
            s.generation += 1;
            s.starting = true;
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
        drop(old); // 락 밖에서 정지(join)

        let network = peer_id.map(|id| (id, self.net_options.clone()));
        let result = Engine::start(config, self.backend.clone(), self.controls.clone(), network);

        let mut s = self.slot.lock();
        if s.generation != gen {
            drop(s);
            drop(result); // 그 사이 stop/start가 들어왔다: 이 결과는 버린다(락 밖에서 정지)
            return Err("엔진 시작이 취소되었습니다".into());
        }
        s.starting = false;
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

    pub fn with_engine<T>(&self, f: impl FnOnce(&mut Engine) -> Result<T, String>) -> Result<T, String> {
        let mut s = self.slot.lock();
        if s.starting && s.engine.is_none() {
            return Err("엔진 시작 중입니다".into());
        }
        let engine = s.engine.as_mut().ok_or("엔진이 실행 중이 아닙니다")?;
        f(engine)
    }

    pub fn stats(&self) -> EngineStats {
        match self.slot.lock().engine.as_ref() {
            Some(e) => e.stats(),
            None => EngineStats::stopped(),
        }
    }
}
