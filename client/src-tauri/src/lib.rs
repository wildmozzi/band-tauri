pub mod commands;
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
                .build(),
        )
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .manage(commands::EngineState::default())
        .invoke_handler(tauri::generate_handler![
            commands::engine_list_devices,
            commands::engine_start,
            commands::engine_stop,
            commands::engine_test_start,
            commands::engine_test_stop,
            commands::engine_add_peer,
            commands::engine_remove_peer,
            commands::engine_set_peer_gain,
            commands::engine_set_input_muted,
            commands::engine_set_monitor,
            commands::engine_set_master_gain,
            commands::engine_stats,
            commands::engine_subscribe_mix,
            commands::engine_unsubscribe_mix,
        ])
        .setup(|_app| {
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
                app.state::<commands::EngineState>().shutdown();
            }
        });
}
