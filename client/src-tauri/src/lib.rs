pub mod commands;
pub mod diag;
pub mod engine;
pub mod manager;
pub mod net;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                // 진단 내보내기(P2-5)에 담을 만큼 남긴다(기본 40KB·1개는 몇 분 분량뿐)
                .max_file_size(1_000_000)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(2))
                .build(),
        )
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::EngineState::default())
        .manage(commands::DiagState::default())
        .manage(commands::RecordDirState::default())
        .invoke_handler(tauri::generate_handler![
            commands::engine_list_devices,
            commands::engine_start,
            commands::engine_stop,
            commands::engine_test_start,
            commands::engine_test_stop,
            commands::engine_add_peer,
            commands::engine_remove_peer,
            commands::engine_set_relay,
            commands::engine_set_peer_gain,
            commands::engine_set_input_muted,
            commands::engine_set_jitter_profile,
            commands::engine_safety_code,
            commands::engine_set_monitor,
            commands::engine_set_master_gain,
            commands::engine_set_metronome,
            commands::engine_set_metronome_gain,
            commands::engine_stats,
            commands::engine_subscribe_mix,
            commands::engine_unsubscribe_mix,
            commands::engine_measure_loopback,
            commands::engine_set_echo_mode,
            commands::engine_measure_echo_peer,
            commands::engine_cancel_measure,
            commands::engine_record_folder,
            commands::engine_record_pick_folder,
            commands::engine_record_start,
            commands::engine_record_stop,
            commands::engine_record_set_track_names,
            commands::diag_export,
        ])
        .setup(|_app| {
            // 엔진 통계 타임라인 기록(P2-5): 1초 간격, 최근 60분
            let engine = _app.state::<commands::EngineState>().manager();
            if let Err(e) = _app.state::<commands::DiagState>().spawn_recorder(engine) {
                log::warn!("진단 기록 스레드 시작 실패: {e}");
            }
            // 자동 업데이트 (P2-1): 엔드포인트·공개키는 tauri.conf.json plugins.updater, 절차는 docs/RELEASE.md
            #[cfg(desktop)]
            _app.handle().plugin(tauri_plugin_updater::Builder::new().build())?;
            // ASIO Host는 여기서(백그라운드로) 미리 만들지 않는다 — engine/device.rs 참고.
            #[cfg(debug_assertions)]
            {
                if let Some(window) = _app.get_webview_window("main") {
                    window.open_devtools();
                }
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Tauri 앱 초기화 실패")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // 종료 시 Bye 송신과 스레드 join
                app.state::<commands::DiagState>().shutdown();
                app.state::<commands::EngineState>().shutdown();
            }
        });
}
