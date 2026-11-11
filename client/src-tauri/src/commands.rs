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

use crate::diag::Diagnostics;
use crate::engine::config::EngineConfig;
use crate::engine::measure::{EchoReport, LoopbackReport};
use crate::engine::device::system::SystemBackend;
use crate::engine::device::DeviceInfo;
use crate::engine::mixer::clamp_gain;
use crate::engine::stats::EngineStats;
use crate::manager::EngineManager;
use crate::net::RelaySetting;
use crate::net::peer::LocalDescriptor;
use crate::net::NetOptions;

#[derive(Clone)]
pub struct EngineState(Arc<EngineManager>);

impl Default for EngineState {
    fn default() -> Self {
        // 자체 WASAPI(독점·저지연 공유) 우선, 실패 시 cpal(P1-0)
        Self(Arc::new(EngineManager::new(Arc::new(SystemBackend::new()), NetOptions::default())))
    }
}

impl EngineState {
    /// 실행 중인 엔진을 정지(모든 스레드 join).
    pub fn shutdown(&self) {
        self.0.stop();
    }

    pub fn manager(&self) -> Arc<EngineManager> {
        self.0.clone()
    }
}

/// 진단 타임라인(P2-5). 기록 스레드는 `lib.rs` setup에서 띄운다.
#[derive(Clone, Default)]
pub struct DiagState(Arc<Diagnostics>);

impl DiagState {
    pub fn spawn_recorder(&self, engine: Arc<EngineManager>) -> std::io::Result<()> {
        self.0.spawn_recorder(engine).map(|_| ())
    }

    pub fn shutdown(&self) {
        self.0.shutdown();
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
    /// 토큰 남은 수명(초, 서버 발급 시각 기준 = `expiresAt - serverNow`). 엔진은 받은 시각 + 이 값으로 만료를
    /// 판단한다(클라이언트 시계 오차 무관, PR #10 M1). 없으면 로컬 시계로 계산.
    #[serde(default)]
    pub expires_in_sec: Option<u64>,
}

#[tauri::command]
pub async fn engine_set_relay(state: State<'_, EngineState>, relay: Option<RelayArgs>) -> Result<(), String> {
    let m = state.0.clone();
    // DNS 조회는 블로킹이므로 엔진 락 밖, 블로킹 스레드에서. 같은 host:port 문자열이면 처음 해석한 주소를
    // 유지한다(토큰 갱신마다 다시 풀면 라운드로빈 A 레코드에서 릴레이가 바뀌어 끊긴다, M3).
    blocking(move || {
        let Some(r) = relay else {
            return m.with_engine(|e| e.set_relay(None));
        };
        let known = m.with_engine(|e| Ok(e.relay_addr_for_host(&r.addr)))?;
        let addr = match known {
            Some(a) => a,
            None => crate::net::relay::resolve_relay_addr(&r.addr)?,
        };
        let expires_in = r.expires_in_sec.map(std::time::Duration::from_secs);
        m.with_engine(|e| e.set_relay(Some(RelaySetting { host: &r.addr, addr, token: &r.token, expires_in })))
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

/// 하드웨어 루프백 지연 측정(P1-1). 실행 중인 엔진(보통 테스트 모드)에서 출력 → 입력 케이블로 왕복을 잰다.
/// 측정 동안 장치 출력은 자극(처프)만 낸다. `runs` 기본 8(1..=20).
#[tauri::command]
pub async fn engine_measure_loopback(state: State<'_, EngineState>, runs: Option<u32>) -> Result<LoopbackReport, String> {
    let m = state.0.clone();
    blocking(move || m.measure_loopback(runs.unwrap_or(8))).await
}

/// 에코 모드 동의/해제(에코하는 쪽). 켜기는 그 피어가 에코를 요청 중일 때만 된다(양측 합의).
#[tauri::command]
pub async fn engine_set_echo_mode(state: State<'_, EngineState>, peer_id: String, enabled: bool) -> Result<(), String> {
    state.0.with_engine(|e| e.set_echo_mode(&peer_id, enabled))
}

/// 에코 피어 측정(측정하는 쪽). 그 피어에게 에코를 요청하고 동의를 `waitMs`(기본 30000)까지 기다린 뒤
/// 클릭 `runs`개(기본 8)로 네트워크 포함 왕복을 잰다.
#[tauri::command]
pub async fn engine_measure_echo_peer(
    state: State<'_, EngineState>,
    peer_id: String,
    runs: Option<u32>,
    wait_ms: Option<u64>,
) -> Result<EchoReport, String> {
    let m = state.0.clone();
    let wait = std::time::Duration::from_millis(wait_ms.unwrap_or(30_000).min(120_000));
    blocking(move || m.measure_echo(&peer_id, runs.unwrap_or(8), wait)).await
}

/// 진행 중인 측정 취소.
#[tauri::command]
pub async fn engine_cancel_measure(state: State<'_, EngineState>) -> Result<(), String> {
    state.0.cancel_measure();
    Ok(())
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

/// 진단 내보내기(P2-5). 저장 대화상자(zip)를 열고, 고른 경로에 진단 zip을 쓴다. 취소하면 `null`.
/// 경로는 Rust 쪽 대화상자에서만 정한다(프론트가 임의 경로를 넘길 수 없다). 내용·마스킹은 `diag::export`.
#[tauri::command]
pub async fn diag_export(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    engine: State<'_, EngineState>,
    diag: State<'_, DiagState>,
) -> Result<Option<String>, String> {
    use tauri::Manager;
    use tauri_plugin_dialog::DialogExt;
    let m = engine.0.clone();
    let d = diag.0.clone();
    blocking(move || {
        // 설정·믹서 헤더 두 곳에서 동시에 눌러도 대화상자는 하나만
        let Some(_busy) = d.begin_export() else {
            return Err("이미 진단을 내보내는 중입니다".into());
        };
        let now = crate::diag::now_ms();
        let Some(picked) = app
            .dialog()
            .file()
            // 부모 창에 모달로 붙인다(없으면 Windows에서 앱 뒤나 작업 표시줄에 따로 뜰 수 있다)
            .set_parent(&window)
            .set_title("진단 내보내기")
            .set_file_name(crate::diag::export::default_file_name(now))
            .add_filter("zip", &["zip"])
            .blocking_save_file()
        else {
            return Ok(None);
        };
        let path = crate::diag::export::target_path(picked.into_path().map_err(|e| format!("저장 경로 오류: {e}"))?);
        let info = app.package_info();
        let app_info = crate::diag::export::AppInfo { name: info.name.clone(), version: info.version.to_string() };
        let log_dir = app.path().app_log_dir().ok();
        let bytes = crate::diag::export::build(&m, &d, app_info, log_dir.as_deref(), now)?;
        crate::diag::export::write_atomic(&path, &bytes).map_err(|e| format!("진단 파일 저장 실패: {e}"))?;
        log::info!("진단 내보내기 완료");
        Ok(Some(path.to_string_lossy().into_owned()))
    })
    .await
}
