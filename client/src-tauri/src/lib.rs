mod audio;
mod commands;
mod network;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    println!("[BAND] Starting application...");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            // New Audio API
            commands::start_audio,
            commands::stop_audio,
            commands::set_audio_peer,
            commands::get_audio_stats,
            commands::get_audio_devices,
            commands::start_test_audio,
            commands::stop_test_audio,
            commands::refresh_audio_devices,
            // New Network API
            commands::init_network,
            commands::get_network_info,
            commands::connect_to_peer,
            commands::disconnect_peer,
            commands::get_peer_latencies,
            commands::shutdown_network,
            // Legacy (backward compatibility)
            commands::start_native_audio,
            commands::stop_native_audio,
            commands::set_audio_settings,
            commands::set_monitoring,
            commands::get_monitoring,
            commands::set_monitoring_volume,
            commands::get_monitoring_volume,
            commands::start_p2p_network,
            commands::stop_p2p_network,
        ])
        .setup(|_app| {
            println!("[BAND] Setup started...");

            // 백그라운드에서 ASIO 장치 캐시 초기화 (앱 시작 즉시)
            audio::init_audio_cache();

            #[cfg(debug_assertions)]
            {
                use tauri::Manager;
                if let Some(window) = _app.get_webview_window("main") {
                    let _ = window.open_devtools();
                }
            }
            println!("[BAND] Setup completed");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
