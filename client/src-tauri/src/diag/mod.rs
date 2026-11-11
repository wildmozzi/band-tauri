//! 진단(P2-5): 엔진 통계 타임라인 기록과 진단 zip 내보내기.
//!
//! 기록 스레드는 1초마다 `EngineManager::snapshot()`(엔진 슬롯 락을 잠깐 잡고 `Engine::stats()`)을 부른다.
//! UI가 `engine_stats`를 부르는 것과 같은 경로다. RT 콜백(`engine/rt.rs`)에는 아무것도 추가하지 않는다.

pub mod export;
pub mod mask;
pub mod timeline;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::{Condvar, Mutex};

use crate::engine::config::EngineConfig;
use crate::engine::stats::EngineStats;
use crate::manager::EngineManager;
use timeline::{Sample, Timeline, DEFAULT_CAPACITY, SAMPLE_INTERVAL_MS};

/// 엔진 한 순간의 상태(기록 스레드 입력).
pub struct Snapshot {
    pub stats: EngineStats,
    pub config: EngineConfig,
    /// 합주(네트워크) 세션인지
    pub network: bool,
}

struct Inner {
    timeline: Timeline,
    /// 직전 기록 때 엔진이 있었는지(정지 표시 샘플을 한 번만 넣기 위해)
    was_active: bool,
    /// 현재 또는 마지막 세션의 설정
    last_config: Option<EngineConfig>,
}

pub struct Diagnostics {
    inner: Mutex<Inner>,
    stop: AtomicBool,
    wake: (Mutex<()>, Condvar),
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl Diagnostics {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner { timeline: Timeline::new(capacity), was_active: false, last_config: None }),
            stop: AtomicBool::new(false),
            wake: (Mutex::new(()), Condvar::new()),
        }
    }

    /// 한 번 기록한다. 엔진이 있으면 샘플을 넣고, 엔진이 막 없어졌으면 정지 표시 샘플을 한 번 넣는다.
    /// 엔진이 계속 없으면 아무것도 넣지 않는다(빈 시간은 `t` 간격으로 보인다).
    pub fn record(&self, now_ms: u64, snap: Option<Snapshot>) {
        let mut g = self.inner.lock();
        match snap {
            Some(s) => {
                g.timeline.push(Sample::from_stats(now_ms, s.network, &s.stats));
                g.last_config = Some(s.config);
                g.was_active = true;
            }
            None if g.was_active => {
                g.timeline.push(Sample::stopped(now_ms));
                g.was_active = false;
            }
            None => {}
        }
    }

    /// 내보내기용으로 현재 내용을 읽는다.
    pub fn with_timeline<T>(&self, f: impl FnOnce(&Timeline, Option<&EngineConfig>, bool) -> T) -> T {
        let g = self.inner.lock();
        f(&g.timeline, g.last_config.as_ref(), g.was_active)
    }

    /// 1초마다 기록하는 스레드를 띄운다. `shutdown()`으로 멈춘다.
    pub fn spawn_recorder(self: &Arc<Self>, manager: Arc<EngineManager>) -> std::io::Result<std::thread::JoinHandle<()>> {
        let me = self.clone();
        std::thread::Builder::new().name("band-diag".into()).spawn(move || {
            let interval = Duration::from_millis(SAMPLE_INTERVAL_MS);
            while !me.stop.load(Ordering::Acquire) {
                me.record(now_ms(), manager.snapshot());
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

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(running: bool) -> Snapshot {
        let mut stats = EngineStats::stopped();
        stats.running = running;
        Snapshot { stats, config: EngineConfig::default(), network: true }
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
            assert_eq!(ts, vec![(2, true, true), (3, false, true), (4, false, false)]);
            assert!(cfg.is_some(), "마지막 세션 설정은 정지 뒤에도 남는다");
            assert!(!active);
        });
    }

    #[test]
    fn recorder_thread_samples_and_stops() {
        use crate::engine::device::fake::FakeBackend;
        use crate::net::NetOptions;
        let mgr = Arc::new(EngineManager::new(Arc::new(FakeBackend::silence(48_000)), NetOptions::default()));
        mgr.test_start(EngineConfig::default()).unwrap();
        let d = Arc::new(Diagnostics::new(10));
        let h = d.spawn_recorder(mgr.clone()).unwrap();
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
