//! Tauri 커맨드 — 얇은 어댑터 (설계 문서 4절). 로직은 `engine`/`net`에 있다.
//!
//! 무거운 작업(장치 열기, STUN)은 `spawn_blocking`으로 UI 스레드 밖에서 수행한다.
//! 장치 열거(`engine_list_devices`)는 기존 동작대로 동기 커맨드로 둔다(ASIO 주의사항은
//! `engine/device.rs` 참고).

use std::sync::Arc;

use parking_lot::Mutex;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::engine::config::EngineConfig;
use crate::engine::device::cpal_backend::CpalBackend;
use crate::engine::device::{AudioBackend, DeviceInfo};
use crate::engine::mixer::{clamp_gain, MixControls};
use crate::engine::stats::EngineStats;
use crate::engine::Engine;
use crate::net::peer::LocalDescriptor;
use crate::net::NetOptions;

struct Inner {
    engine: Mutex<Option<Engine>>,
    controls: Arc<MixControls>,
    backend: Arc<dyn AudioBackend>,
}

#[derive(Clone)]
pub struct EngineState(Arc<Inner>);

impl Default for EngineState {
    fn default() -> Self {
        Self(Arc::new(Inner {
            engine: Mutex::new(None),
            controls: Arc::new(MixControls::default()),
            backend: Arc::new(CpalBackend),
        }))
    }
}

impl EngineState {
    /// 실행 중인 엔진을 정지(모든 스레드 join).
    pub fn shutdown(&self) {
        let old = self.0.engine.lock().take();
        drop(old);
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| format!("작업 실패: {e}"))?
}

#[tauri::command]
pub fn engine_list_devices(state: State<'_, EngineState>) -> Result<Vec<DeviceInfo>, String> {
    state.0.backend.list_devices()
}

#[tauri::command]
pub async fn engine_start(
    state: State<'_, EngineState>,
    config: EngineConfig,
    peer_id: String,
) -> Result<LocalDescriptor, String> {
    let inner = state.0.clone();
    blocking(move || {
        let mut slot = inner.engine.lock();
        // 기존 엔진(테스트 모드 포함) 정지 후 시작
        drop(slot.take());
        let engine = Engine::start(
            config,
            inner.backend.clone(),
            inner.controls.clone(),
            Some((peer_id, NetOptions::default())),
        )?;
        let desc = engine.descriptor().cloned().ok_or("descriptor 없음")?;
        *slot = Some(engine);
        Ok(desc)
    })
    .await
}

#[tauri::command]
pub async fn engine_stop(state: State<'_, EngineState>) -> Result<(), String> {
    let inner = state.0.clone();
    blocking(move || {
        let old = inner.engine.lock().take();
        drop(old);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn engine_test_start(state: State<'_, EngineState>, config: EngineConfig) -> Result<(), String> {
    let inner = state.0.clone();
    blocking(move || {
        let mut slot = inner.engine.lock();
        drop(slot.take());
        inner.controls.monitor_enabled.store(true, std::sync::atomic::Ordering::Relaxed);
        let engine = Engine::start(config, inner.backend.clone(), inner.controls.clone(), None)?;
        *slot = Some(engine);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn engine_test_stop(state: State<'_, EngineState>) -> Result<(), String> {
    let inner = state.0.clone();
    blocking(move || {
        let mut slot = inner.engine.lock();
        if slot.as_ref().is_some_and(|e| !e.has_network()) {
            drop(slot.take());
        }
        Ok(())
    })
    .await
}

fn with_engine<T>(state: &EngineState, f: impl FnOnce(&mut Engine) -> Result<T, String>) -> Result<T, String> {
    let mut slot = state.0.engine.lock();
    let engine = slot.as_mut().ok_or("엔진이 실행 중이 아닙니다")?;
    f(engine)
}

#[tauri::command]
pub fn engine_add_peer(state: State<'_, EngineState>, peer_id: String, descriptor: LocalDescriptor) -> Result<(), String> {
    with_engine(&state, |e| e.add_peer(peer_id, &descriptor))
}

#[tauri::command]
pub fn engine_remove_peer(state: State<'_, EngineState>, peer_id: String) -> Result<(), String> {
    with_engine(&state, |e| e.remove_peer(&peer_id))
}

#[tauri::command]
pub fn engine_set_peer_gain(state: State<'_, EngineState>, peer_id: String, gain: f32, muted: bool) -> Result<(), String> {
    with_engine(&state, |e| e.set_peer_gain(&peer_id, gain, muted))
}

#[tauri::command]
pub fn engine_set_monitor(state: State<'_, EngineState>, enabled: bool, gain: f32) -> Result<(), String> {
    let c = &state.0.controls;
    c.monitor_enabled.store(enabled, std::sync::atomic::Ordering::Relaxed);
    c.monitor_gain.store(clamp_gain(gain));
    Ok(())
}

#[tauri::command]
pub fn engine_set_master_gain(state: State<'_, EngineState>, gain: f32) -> Result<(), String> {
    state.0.controls.master_gain.store(clamp_gain(gain));
    Ok(())
}

#[tauri::command]
pub fn engine_stats(state: State<'_, EngineState>) -> Result<EngineStats, String> {
    Ok(match state.0.engine.lock().as_ref() {
        Some(e) => e.stats(),
        None => EngineStats::stopped(),
    })
}

#[tauri::command]
pub fn engine_subscribe_mix(state: State<'_, EngineState>, on_chunk: Channel<InvokeResponseBody>) -> Result<(), String> {
    with_engine(&state, |e| {
        e.subscribe_mix(Box::new(move |bytes| {
            if let Err(err) = on_chunk.send(InvokeResponseBody::Raw(bytes)) {
                log::warn!("믹스 청크 전송 실패: {err}");
            }
        }))
    })
}

#[tauri::command]
pub fn engine_unsubscribe_mix(state: State<'_, EngineState>) -> Result<(), String> {
    with_engine(&state, |e| {
        e.unsubscribe_mix();
        Ok(())
    })
}
