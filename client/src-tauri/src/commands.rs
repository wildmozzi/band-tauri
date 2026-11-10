//! Tauri 커맨드 — 얇은 어댑터 (설계 문서 4절). 로직은 `manager`/`engine`/`net`에 있다.
//!
//! 모든 커맨드는 `async`다. Tauri 2에서 동기 커맨드는 메인(UI) 스레드에서 돌기 때문이다.
//! 블로킹 작업(장치 열기/열거, STUN, 스레드 join)은 `spawn_blocking`으로 옮기고, 엔진 락은
//! 짧게만 잡는다(리뷰 H3).
//!
//! `engine_subscribe_mix`는 JSON이 아닌 raw 바이트(`InvokeResponseBody::Raw`, f32 LE 인터리브)로 보낸다.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::engine::config::EngineConfig;
use crate::engine::device::cpal_backend::CpalBackend;
use crate::engine::device::DeviceInfo;
use crate::engine::mixer::clamp_gain;
use crate::engine::stats::EngineStats;
use crate::manager::EngineManager;
use crate::net::peer::LocalDescriptor;
use crate::net::NetOptions;

#[derive(Clone)]
pub struct EngineState(Arc<EngineManager>);

impl Default for EngineState {
    fn default() -> Self {
        Self(Arc::new(EngineManager::new(Arc::new(CpalBackend), NetOptions::default())))
    }
}

impl EngineState {
    /// 실행 중인 엔진을 정지(모든 스레드 join).
    pub fn shutdown(&self) {
        self.0.stop();
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| format!("작업 실패: {e}"))?
}

#[tauri::command]
pub async fn engine_list_devices(state: State<'_, EngineState>) -> Result<Vec<DeviceInfo>, String> {
    let m = state.0.clone();
    blocking(move || m.backend().list_devices()).await
}

#[tauri::command]
pub async fn engine_start(
    state: State<'_, EngineState>,
    config: EngineConfig,
    peer_id: String,
) -> Result<LocalDescriptor, String> {
    let m = state.0.clone();
    blocking(move || m.start(config, peer_id)).await
}

#[tauri::command]
pub async fn engine_stop(state: State<'_, EngineState>) -> Result<(), String> {
    let m = state.0.clone();
    blocking(move || {
        m.stop();
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn engine_test_start(state: State<'_, EngineState>, config: EngineConfig) -> Result<(), String> {
    let m = state.0.clone();
    blocking(move || m.test_start(config)).await
}

#[tauri::command]
pub async fn engine_test_stop(state: State<'_, EngineState>) -> Result<(), String> {
    let m = state.0.clone();
    blocking(move || {
        m.test_stop();
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn engine_add_peer(state: State<'_, EngineState>, peer_id: String, descriptor: LocalDescriptor) -> Result<(), String> {
    state.0.with_engine(|e| e.add_peer(peer_id, &descriptor))
}

/// 릴레이 설정(설계 4절). 서버의 `relay-token { token, addr }`을 받을 때마다(최초·갱신) 부른다. `null`이면 해제.
/// 토큰은 `engine_start`가 만든 ssrc에 묶이므로 시작 인자가 아니라 별도 커맨드다.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayArgs {
    /// 릴레이 `host:port`(DNS 이름 가능, IPv4로 해석)
    pub addr: String,
    /// base64url 릴레이 토큰
    pub token: String,
}

#[tauri::command]
pub async fn engine_set_relay(state: State<'_, EngineState>, relay: Option<RelayArgs>) -> Result<(), String> {
    let m = state.0.clone();
    // DNS 조회는 블로킹이므로 엔진 락 밖, 블로킹 스레드에서
    blocking(move || {
        let resolved = match relay {
            Some(r) => Some((crate::net::relay::resolve_relay_addr(&r.addr)?, r.token)),
            None => None,
        };
        m.with_engine(|e| e.set_relay(resolved.as_ref().map(|(a, t)| (*a, t.as_str()))))
    })
    .await
}

#[tauri::command]
pub async fn engine_remove_peer(state: State<'_, EngineState>, peer_id: String) -> Result<(), String> {
    state.0.with_engine(|e| e.remove_peer(&peer_id))
}

#[tauri::command]
pub async fn engine_set_peer_gain(
    state: State<'_, EngineState>,
    peer_id: String,
    gain: f32,
    muted: bool,
    pan: Option<f32>,
) -> Result<(), String> {
    state.0.with_engine(|e| e.set_peer_gain(&peer_id, gain, muted, pan))
}

/// 피어와의 안전 번호(5자리×4). 두 연주자가 같은 숫자를 보면 시그널링 서버가 키를 바꿔치기하지 않은 것이다.
#[tauri::command]
pub async fn engine_safety_code(state: State<'_, EngineState>, peer_id: String) -> Result<String, String> {
    state.0.with_engine(|e| e.safety_code(&peer_id))
}

#[tauri::command]
pub async fn engine_set_input_muted(state: State<'_, EngineState>, muted: bool) -> Result<(), String> {
    state.0.controls().input_muted.store(muted, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub async fn engine_set_monitor(state: State<'_, EngineState>, enabled: bool, gain: f32) -> Result<(), String> {
    let c = state.0.controls();
    c.monitor_enabled.store(enabled, Ordering::Relaxed);
    c.monitor_gain.store(clamp_gain(gain));
    Ok(())
}

#[tauri::command]
pub async fn engine_set_master_gain(state: State<'_, EngineState>, gain: f32) -> Result<(), String> {
    state.0.controls().master_gain.store(clamp_gain(gain));
    Ok(())
}

#[tauri::command]
pub async fn engine_stats(state: State<'_, EngineState>) -> Result<EngineStats, String> {
    Ok(state.0.stats())
}

#[tauri::command]
pub async fn engine_subscribe_mix(state: State<'_, EngineState>, on_chunk: Channel<InvokeResponseBody>) -> Result<(), String> {
    state.0.with_engine(|e| {
        e.subscribe_mix(Box::new(move |bytes| {
            if let Err(err) = on_chunk.send(InvokeResponseBody::Raw(bytes)) {
                log::warn!("믹스 청크 전송 실패: {err}");
            }
        }))
    })
}

#[tauri::command]
pub async fn engine_unsubscribe_mix(state: State<'_, EngineState>) -> Result<(), String> {
    // 탭 스레드 join이 있으므로 블로킹 스레드에서
    let m = state.0.clone();
    blocking(move || {
        m.with_engine(|e| {
            e.unsubscribe_mix();
            Ok(())
        })
    })
    .await
}
