//! Tauri 커맨드 — 얇은 어댑터 (설계 문서 4절). 로직은 `manager`/`engine`/`net`에 있다.
//!
//! 모든 커맨드는 `async`다. Tauri 2에서 동기 커맨드는 메인(UI) 스레드에서 돌기 때문이다.
//! 블로킹 작업(장치 열기/열거, STUN, 스레드 join)은 `spawn_blocking`으로 옮기고, 엔진 락은
//! 짧게만 잡는다(리뷰 H3). **엔진 슬롯 락(`with_engine`·`stats`·`set_loopback_offset`)이나 디스크를 건드리는 커맨드는 모두
//! `blocking` 안에서 부른다**(P2-24b): 슬롯 락은 다른 커맨드의 `with_engine` 클로저(피어 추가의 약 1.4MB 할당·명령 큐 대기,
//! 믹스 구독 해제의 탭 스레드 join)가 쥔 동안 기다리므로, async 런타임 스레드에서 잡으면 그동안 다른 커맨드·IPC가 밀렸다.
//! 원자값만 쓰는 커맨드(뮤트·모니터·게인·측정 취소)는 그대로 둔다. 시험 `commands::tests::lock_and_io_commands_run_on_blocking_pool`.
//!
//! `engine_subscribe_mix`는 JSON이 아닌 raw 바이트(`InvokeResponseBody::Raw`, f32 LE 인터리브)로 보낸다.

use std::sync::Arc;

use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use band_engine::diag::Diagnostics;
use band_dsp::config::{DriverKind, EngineConfig};
use band_engine::engine::measure::{EchoReport, LoopbackReport};
use band_engine::engine::device::select::SystemBackend;
use band_engine::engine::device::types::DeviceInfo;
use band_engine::engine::metronome::plan::{MetronomeConfig, MetronomePlan};
use band_engine::engine::record::sidecar::{RecordOptions, RecordStats, RecordSummary};
use band_engine::engine::report::EngineStats;
use band_engine::manager::EngineManager;
use band_engine::net::peer::LocalDescriptor;
use band_engine::net::NetOptions;
use band_engine::settings::{RecordDir, RECORD_DIR_FILE};

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

    /// 웹뷰 페이지를 새로 읽기 시작했다(`lib.rs` `on_page_load`, 발굴 3차). 처음 열 때는 엔진이 없어 아무것도 하지 않는다.
    /// 다시 읽었으면(F5·Ctrl+R — 릴리스 WebView2도 브라우저 단축키가 켜져 있다, 웹뷰 프로세스 복구) 앞 페이지가 띄운 엔진은
    /// 새 페이지가 모르는 고아다: 마이크 송신·녹음 쓰기·테스트 모드 강제 모니터가 UI 없이 이어진다. 정지해(녹음은 마무리,
    /// 모니터 설정 복원) 새 페이지가 처음 상태에서 시작하게 한다. 블로킹(join)이므로 UI 스레드가 아닌 곳에서 부른다.
    ///
    /// `gen`은 다시 읽기를 시작한 순간의 세대(`reload_generation`, 메인 스레드에서 바로 뜬다). 새 페이지가 그 뒤 이미 start했으면
    /// 그 엔진은 건드리지 않고, 정지가 먼저면 새 start는 옛 엔진이 장치를 놓을 때까지 기다린다(`EngineManager::stop_if_generation`).
    /// 반환: 실행(또는 시작) 중이던 엔진을 정지했다.
    pub fn stop_orphaned(&self, gen: u64) -> bool {
        let stopped = self.0.stop_if_generation(gen);
        if stopped {
            log::warn!("페이지를 다시 읽었습니다: 앞 페이지가 띄운 오디오 엔진을 정지했습니다(녹음 중이었으면 마무리)");
        }
        stopped
    }

    /// 지금 세대(`stop_orphaned`에 넘긴다). 슬롯 락만 잠깐 잡는다 — 메인 스레드에서 불러도 된다.
    pub fn reload_generation(&self) -> u64 {
        self.0.start_generation()
    }
}

/// 진단 타임라인(P2-5). 기록 스레드는 `lib.rs` setup에서 띄운다.
#[derive(Clone, Default)]
pub struct DiagState(Arc<Diagnostics>);

impl DiagState {
    /// 기록 스레드에는 엔진 스냅샷 함수와 정지한 엔진의 연결 이벤트 함수만 넘긴다(`diag`는 `manager`를 모른다, 엔진 ⑤).
    pub fn spawn_recorder(&self, engine: Arc<EngineManager>) -> std::io::Result<()> {
        let e2 = engine.clone();
        self.0.spawn_recorder(move || engine.snapshot(), move || e2.take_ended_conn()).map(|_| ())
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

/// 이 빌드가 지원하는 드라이버(P2-7). 기본 배포본은 `asio` 피처 없이 빌드되어 ASIO가 빠진다.
#[tauri::command]
pub fn engine_supported_drivers() -> Vec<DriverKind> {
    DriverKind::supported()
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
    blocking(move || m.start_monitor_only(config)).await
}

#[tauri::command]
pub async fn engine_test_stop(state: State<'_, EngineState>) -> Result<(), String> {
    let m = state.0.clone();
    blocking(move || {
        m.stop_monitor_only();
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn engine_add_peer(state: State<'_, EngineState>, peer_id: String, descriptor: LocalDescriptor) -> Result<(), String> {
    let m = state.0.clone();
    blocking(move || m.with_engine(|e| e.add_peer(peer_id, &descriptor))).await
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
    /// 토큰별 Bind MAC 키(base64url 32B, relay-design 2.1.1). 있으면 RelayBind에 MAC을 붙인다(옛 서버는 없음).
    #[serde(default)]
    pub bind_key: Option<String>,
}

#[tauri::command]
pub async fn engine_set_relay(state: State<'_, EngineState>, relay: Option<RelayArgs>) -> Result<(), String> {
    let m = state.0.clone();
    // DNS 조회가 블로킹이므로 블로킹 스레드에서(주소 유지 규칙 M3는 `EngineManager::set_relay`)
    blocking(move || match relay {
        None => m.clear_relay(),
        Some(r) => m.set_relay(&r.addr, &r.token, r.expires_in_sec.map(std::time::Duration::from_secs), r.bind_key.as_deref()),
    })
    .await
}

#[tauri::command]
pub async fn engine_remove_peer(state: State<'_, EngineState>, peer_id: String) -> Result<(), String> {
    let m = state.0.clone();
    blocking(move || m.with_engine(|e| e.remove_peer(&peer_id))).await
}

#[tauri::command]
pub async fn engine_set_peer_gain(
    state: State<'_, EngineState>,
    peer_id: String,
    gain: f32,
    muted: bool,
    pan: Option<f32>,
) -> Result<(), String> {
    let m = state.0.clone();
    blocking(move || m.with_engine(|e| e.set_peer_gain(&peer_id, gain, muted, pan))).await
}

/// 피어와의 안전 번호(5자리×4). 두 연주자가 같은 숫자를 보면 시그널링 서버가 키를 바꿔치기하지 않은 것이다.
#[tauri::command]
pub async fn engine_safety_code(state: State<'_, EngineState>, peer_id: String) -> Result<String, String> {
    let m = state.0.clone();
    blocking(move || m.with_engine(|e| e.safety_code(&peer_id))).await
}

/// 지터 버퍼 '저지연(0) ↔ 안정(1)' 프로필을 실행 중에 바꾼다(P1-5). 다음 `engine_start`에는 `EngineConfig.jitterProfile`을 쓴다.
#[tauri::command]
pub async fn engine_set_jitter_profile(state: State<'_, EngineState>, profile: f32) -> Result<(), String> {
    let m = state.0.clone();
    blocking(move || m.with_engine(|e| e.set_jitter_profile(profile))).await
}

#[tauri::command]
pub async fn engine_set_input_muted(state: State<'_, EngineState>, muted: bool) -> Result<(), String> {
    state.0.set_input_muted(muted);
    Ok(())
}

#[tauri::command]
pub async fn engine_set_monitor(state: State<'_, EngineState>, enabled: bool, gain: f32) -> Result<(), String> {
    state.0.set_monitor(enabled, gain);
    Ok(())
}

#[tauri::command]
pub async fn engine_set_master_gain(state: State<'_, EngineState>, gain: f32) -> Result<(), String> {
    state.0.set_master_gain(gain);
    Ok(())
}

/// 공유 메트로놈 계획(P2-3). 방장은 `refPeerId` 없이(시작 시각을 비우면 지금 + 300ms), 다른 연주자는
/// 방장 엔진 peerId와 방장이 알린 `startUs`로 부른다. 적용된 계획을 돌려준다(방장은 이것을 방에 알린다).
#[tauri::command]
pub async fn engine_set_metronome(state: State<'_, EngineState>, metronome: MetronomeConfig) -> Result<MetronomePlan, String> {
    let m = state.0.clone();
    blocking(move || m.with_engine(|e| e.set_metronome(metronome))).await
}

/// 메트로놈 클릭 볼륨·뮤트(로컬 전용, 엔진 재시작 간 유지).
#[tauri::command]
pub async fn engine_set_metronome_gain(state: State<'_, EngineState>, gain: f32, muted: bool) -> Result<(), String> {
    state.0.set_metronome_gain(gain, muted);
    Ok(())
}

/// 메트로놈 숨은 출력 지연 보정용 루프백 보정값(PR #34 M2(a)). `offsetMs` = `engine_measure_loopback` 결과의 `offsetMs`.
/// 루프백 측정이 정상이면 엔진이 자동으로 기억하므로, 앱이 저장해 둔 값을 다시 넣거나 끌 때(null) 쓴다.
#[tauri::command]
pub async fn engine_set_loopback_offset(state: State<'_, EngineState>, offset_ms: Option<f64>) -> Result<(), String> {
    let m = state.0.clone();
    blocking(move || m.set_loopback_offset(offset_ms)).await
}

#[tauri::command]
pub async fn engine_stats(state: State<'_, EngineState>) -> Result<EngineStats, String> {
    let m = state.0.clone();
    blocking(move || Ok(m.stats())).await
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
    let m = state.0.clone();
    blocking(move || m.with_engine(|e| e.set_echo_mode(&peer_id, enabled))).await
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
    let m = state.0.clone();
    blocking(move || {
        m.with_engine(|e| {
            e.subscribe_mix(Box::new(move |bytes| {
                if let Err(err) = on_chunk.send(InvokeResponseBody::Raw(bytes)) {
                    log::warn!("믹스 청크 전송 실패: {err}");
                }
            }))
        })
    })
    .await
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
        let now = band_engine::sys::time::unix_ms();
        let Some(picked) = app
            .dialog()
            .file()
            // 부모 창에 모달로 붙인다(없으면 Windows에서 앱 뒤나 작업 표시줄에 따로 뜰 수 있다)
            .set_parent(&window)
            .set_title("진단 내보내기")
            .set_file_name(band_engine::diag::export::default_file_name(now))
            .add_filter("zip", &["zip"])
            .blocking_save_file()
        else {
            return Ok(None);
        };
        let path = band_engine::diag::export::target_path(picked.into_path().map_err(|e| format!("저장 경로 오류: {e}"))?);
        let info = app.package_info();
        let app_info = band_engine::diag::export::AppInfo { name: info.name.clone(), version: info.version.to_string() };
        let log_dir = app.path().app_log_dir().ok();
        // 장치 열거 → 스냅샷 순서는 ⑤ 이전 `build` 안의 순서 그대로
        let devices = m.backend().list_devices();
        let bytes = band_engine::diag::export::build(m.snapshot(), devices, &d, app_info, log_dir.as_deref(), now)?;
        band_engine::diag::export::write_atomic(&path, &bytes).map_err(|e| format!("진단 파일 저장 실패: {e}"))?;
        log::info!("진단 내보내기 완료");
        Ok(Some(path.to_string_lossy().into_owned()))
    })
    .await
}

/// 녹음 저장 폴더(P2-4). 사용자가 고른 폴더는 앱 설정 폴더의 `recording.json`에 남는다(형식·우선순위는 `settings::RecordDir`).
/// 경로는 Rust 쪽 폴더 대화상자에서만 정한다(프론트가 임의 경로를 넘길 수 없다, `diag_export`와 같은 원칙).
#[derive(Clone)]
pub struct RecordDirState(Arc<RecordDir>);

impl RecordDirState {
    /// 설정 파일 경로(앱 설정 폴더/`recording.json`)를 Tauri에서 찾아 만든다. `lib.rs` setup에서 부른다.
    pub fn new(app: &tauri::AppHandle) -> Self {
        use tauri::Manager;
        Self(Arc::new(RecordDir::new(app.path().app_config_dir().ok().map(|d| d.join(RECORD_DIR_FILE)))))
    }
}

/// 현재 녹음 폴더: 고른 폴더 → 저장된 폴더 → 기본(음악 폴더/Band Recordings).
fn current_record_dir(app: &tauri::AppHandle, dirs: &RecordDirState) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    dirs.0.current(|| {
        let base = app.path().audio_dir().or_else(|_| app.path().document_dir()).map_err(|e| format!("기본 녹음 폴더를 찾을 수 없습니다: {e}"))?;
        Ok(base.join("Band Recordings"))
    })
}

/// 현재 녹음 저장 폴더(고르지 않았으면 기본 폴더).
#[tauri::command]
pub async fn engine_record_folder(app: tauri::AppHandle, dirs: State<'_, RecordDirState>) -> Result<String, String> {
    // 처음 부를 때 설정 파일을 읽는다(디스크 I/O)
    let dirs = dirs.inner().clone();
    blocking(move || current_record_dir(&app, &dirs).map(|p| p.to_string_lossy().into_owned())).await
}

/// `engine_record_pick_folder` 결과. `save_error`: 고른 폴더를 앱 설정에 남기지 못했다(이번 실행 동안은 그 폴더를 쓰지만 다음
/// 실행에는 이전 폴더·기본으로 돌아간다 — 프론트가 안내한다, #212).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordFolderPick {
    pub folder: String,
    pub save_error: Option<String>,
}

/// 녹음 저장 폴더 고르기(폴더 대화상자). 취소하면 `null`.
#[tauri::command]
pub async fn engine_record_pick_folder(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    dirs: State<'_, RecordDirState>,
) -> Result<Option<RecordFolderPick>, String> {
    use tauri_plugin_dialog::DialogExt;
    let dirs = dirs.inner().clone();
    blocking(move || {
        let cur = current_record_dir(&app, &dirs).ok();
        let mut dlg = app.dialog().file().set_parent(&window).set_title("녹음 저장 폴더");
        if let Some(c) = cur.as_ref().filter(|c| c.is_dir()) {
            dlg = dlg.set_directory(c);
        }
        let Some(picked) = dlg.blocking_pick_folder() else {
            return Ok(None);
        };
        let path = picked.into_path().map_err(|e| format!("폴더 경로 오류: {e}"))?;
        let save_error = dirs.0.set(path.clone()).err().map(|e| {
            log::warn!("녹음 폴더 설정 저장 실패: {e}");
            e.to_string()
        });
        Ok(Some(RecordFolderPick { folder: path.to_string_lossy().into_owned(), save_error }))
    })
    .await
}

/// 녹음 시작(P2-4). 현재 녹음 폴더 아래 세션 폴더에 트랙별 WAV(내 입력, 피어별, 선택: 믹스)를 쓴다.
#[tauri::command]
pub async fn engine_record_start(
    app: tauri::AppHandle,
    state: State<'_, EngineState>,
    dirs: State<'_, RecordDirState>,
    options: Option<RecordOptions>,
) -> Result<RecordStats, String> {
    let m = state.0.clone();
    let dirs = dirs.inner().clone();
    blocking(move || {
        let dir = current_record_dir(&app, &dirs)?;
        m.record_start(options.unwrap_or_default(), &dir)
    })
    .await
}

/// 녹음 중 피어 트랙 이름 추가(peerId → 표시 이름). 녹음 중에 들어온 연주자 트랙도 닉네임으로 저장된다.
#[tauri::command]
pub async fn engine_record_set_track_names(
    state: State<'_, EngineState>,
    track_names: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let m = state.0.clone();
    blocking(move || m.record_set_track_names(track_names)).await
}

/// 녹음 정지. 파일을 마무리하고 요약(세션 폴더의 `session.json`과 같은 내용)을 돌려준다.
#[tauri::command]
pub async fn engine_record_stop(state: State<'_, EngineState>) -> Result<RecordSummary, String> {
    let m = state.0.clone();
    blocking(move || m.record_stop()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use band_engine::testing::engine::device::fake::{FakeBackend, SourceFn};
    use std::sync::atomic::Ordering;

    /// 발굴 3차: 페이지를 다시 읽으면 앞 페이지가 띄운 엔진(여기서는 테스트 모드 + 녹음)을 정지한다. 녹음은 마무리되고
    /// (session.json), 테스트 모드가 강제로 켠 모니터는 원래 값으로 돌아간다. 엔진이 없으면(처음 열 때) 아무것도 하지 않는다.
    #[test]
    fn page_reload_stops_orphaned_engine_and_finishes_recording() {
        let src: Arc<SourceFn> = Arc::new(|_: u64, _: usize, b: &mut [f32]| b.fill(0.1));
        let state = EngineState(Arc::new(EngineManager::new(Arc::new(FakeBackend::new(48_000, src)), NetOptions::default())));
        let m = state.manager();
        assert!(!state.stop_orphaned(state.reload_generation()), "엔진이 없으면 아무것도 하지 않는다");
        m.set_monitor(false, 1.0);
        m.start_monitor_only(EngineConfig { buffer_size: 128, ..Default::default() }).unwrap();
        assert!(m.controls().monitor_enabled.load(Ordering::Relaxed), "테스트 모드는 모니터를 켠다");
        let dir = std::env::temp_dir().join(format!("band-app-reload-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let st = m.record_start(RecordOptions::default(), &dir).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));

        assert!(state.stop_orphaned(state.reload_generation()), "실행 중이던 엔진을 정지했다");
        assert!(m.with_engine(|_| Ok(())).is_err() && !m.is_starting(), "엔진이 남았다");
        assert!(!m.controls().monitor_enabled.load(Ordering::Relaxed), "모니터 설정이 복원되지 않았다");
        assert!(std::path::Path::new(&st.folder).join("session.json").exists(), "녹음이 마무리되지 않았다");
        assert!(!state.stop_orphaned(state.reload_generation()), "두 번째는 할 일이 없다");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P2-24b: async 커맨드 본문에서 엔진 슬롯 락·디스크를 건드리는 호출은 `blocking(` 뒤에만 온다(소스 검사 — 새 커맨드가
    /// `state.0.with_engine(..)`을 async 스레드에서 바로 부르면 실패한다).
    #[test]
    fn lock_and_io_commands_run_on_blocking_pool() {
        let src = include_str!("commands.rs").lines().collect::<Vec<_>>().join("\n");
        let body_end = src.find("#[cfg(test)]").unwrap_or(src.len());
        let src = &src[..body_end];
        const GUARDED: [&str; 6] = ["with_engine(", ".stats()", "set_loopback_offset(", "current_record_dir(", "record_start(", "list_devices("];
        let mut bad = Vec::new();
        let mut checked = 0;
        for chunk in src.split("pub async fn ").skip(1) {
            let name = chunk.split(['(', '<']).next().unwrap_or("").to_string();
            let body = chunk.split("\n}\n").next().unwrap_or(chunk);
            // 시그니처(이름에 `list_devices` 등이 들어 있다)는 빼고 본문만 본다
            let body = body.split_once('{').map_or(body, |(_, b)| b);
            let first_blocking = body.find("blocking(").unwrap_or(usize::MAX);
            for g in GUARDED {
                if let Some(at) = body.find(g) {
                    checked += 1;
                    if at < first_blocking {
                        bad.push(format!("{name}: {g}"));
                    }
                }
            }
        }
        assert!(checked >= 15, "검사 대상이 너무 적다({checked}) — 소스 형태가 바뀌었으면 시험을 고칠 것");
        assert!(bad.is_empty(), "async 런타임 스레드에서 엔진 락·디스크를 잡는 커맨드: {bad:?}");
    }
}
