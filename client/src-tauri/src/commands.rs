//! Tauri Commands - Frontend API

use crate::audio::{self, AudioConfig, AudioDriver, Compression, Channels, MonoChannel, AudioDeviceInfo, AudioStats};
use crate::network;
use serde::{Deserialize, Serialize};

// ============================================================================
// Types for Frontend
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSettings {
    #[serde(default)]
    pub driver: String,              // "Asio", "WasapiExclusive", "WasapiShared" (기본)
    #[serde(default)]
    pub input_driver: Option<String>,  // 입력 전용 드라이버 (None이면 driver 사용)
    #[serde(default)]
    pub output_driver: Option<String>, // 출력 전용 드라이버 (None이면 driver 사용)
    #[serde(default)]
    pub input_device: Option<String>,
    #[serde(default)]
    pub output_device: Option<String>,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,            // 44100, 48000
    #[serde(default = "default_buffer_size")]
    pub buffer_size: u32,            // 64, 128, 256, 512, 1024
    #[serde(default)]
    pub compression: String,         // "Uncompressed", "Opus"
    #[serde(default)]
    pub channels: String,            // "Mono", "Stereo"
    #[serde(default)]
    pub mono_channel: String,        // "Ch1", "Ch2" (모노일 때 사용할 입력 채널)
    #[serde(default)]
    pub stereo_swap: bool,           // 스테레오일 때 L/R 스왑
}

fn default_sample_rate() -> u32 { 48000 }
fn default_buffer_size() -> u32 { 256 }

fn parse_driver(s: &str) -> Option<AudioDriver> {
    match s {
        "Asio" => Some(AudioDriver::Asio),
        "WasapiExclusive" => Some(AudioDriver::WasapiExclusive),
        "WasapiShared" => Some(AudioDriver::WasapiShared),
        _ => None,
    }
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            driver: "WasapiShared".to_string(),
            input_driver: None,
            output_driver: None,
            input_device: None,
            output_device: None,
            sample_rate: 48000,
            buffer_size: 256,
            compression: "Uncompressed".to_string(),
            channels: "Mono".to_string(),
            mono_channel: "Ch1".to_string(),
            stereo_swap: false,
        }
    }
}

impl From<AudioSettings> for AudioConfig {
    fn from(s: AudioSettings) -> Self {
        let default_driver = parse_driver(&s.driver).unwrap_or(AudioDriver::WasapiShared);

        AudioConfig {
            driver: default_driver,
            input_driver: s.input_driver.as_ref().and_then(|d| parse_driver(d)),
            output_driver: s.output_driver.as_ref().and_then(|d| parse_driver(d)),
            input_device: s.input_device,
            output_device: s.output_device,
            sample_rate: s.sample_rate,
            buffer_size: s.buffer_size,
            compression: match s.compression.as_str() {
                "Opus" => Compression::Opus,
                _ => Compression::Uncompressed,
            },
            channels: match s.channels.as_str() {
                "Stereo" => Channels::Stereo,
                _ => Channels::Mono,
            },
            mono_channel: match s.mono_channel.as_str() {
                "Ch2" => MonoChannel::Ch2,
                _ => MonoChannel::Ch1,
            },
            stereo_swap: s.stereo_swap,
        }
    }
}

// ============================================================================
// Audio Commands
// ============================================================================

/// Start audio engine with settings
#[tauri::command]
pub async fn start_audio(settings: AudioSettings, peer_addr: Option<String>) -> Result<String, String> {
    println!("[CMD] start_audio: {:?}", settings);

    // Get socket from network
    let socket = network::get_socket().map_err(|e| e.to_string())?;

    // Convert settings to config
    let config: AudioConfig = settings.into();

    // Start audio engine
    audio::start_audio(config, socket).map_err(|e| e.to_string())?;

    // Set peer if provided
    if let Some(addr_str) = peer_addr {
        if let Ok(addr) = addr_str.parse() {
            audio::set_peer(addr);
        }
    }

    Ok("Audio started".to_string())
}

/// Stop audio engine
#[tauri::command]
pub async fn stop_audio() -> Result<String, String> {
    println!("[CMD] stop_audio");
    audio::stop_audio().map_err(|e| e.to_string())?;
    Ok("Audio stopped".to_string())
}

/// Set peer address for audio transmission
#[tauri::command]
pub async fn set_audio_peer(addr: String) -> Result<String, String> {
    println!("[CMD] set_audio_peer: {}", addr);
    let peer_addr = addr.parse().map_err(|e| format!("Invalid address: {}", e))?;
    audio::set_peer(peer_addr);
    Ok(format!("Peer set to {}", addr))
}

/// Get audio statistics
#[tauri::command]
pub async fn get_audio_stats() -> Result<AudioStats, String> {
    Ok(audio::get_stats())
}

/// Get available audio devices
#[tauri::command]
pub async fn get_audio_devices() -> Result<Vec<AudioDeviceInfo>, String> {
    println!("[CMD] get_audio_devices called");
    let result = audio::get_audio_devices().map_err(|e| e.to_string());
    println!("[CMD] get_audio_devices result: {:?}", result.as_ref().map(|v| v.len()));
    result
}

/// Refresh audio device cache (re-enumerate ASIO devices)
#[tauri::command]
pub async fn refresh_audio_devices() -> Result<String, String> {
    println!("[CMD] refresh_audio_devices called");
    audio::refresh_audio_cache();
    Ok("Audio cache refresh started".to_string())
}

/// Start test audio (input monitoring only, no network)
#[tauri::command]
pub async fn start_test_audio(settings: AudioSettings) -> Result<String, String> {
    println!("[CMD] start_test_audio: {:?}", settings);
    let config: AudioConfig = settings.into();
    audio::start_test_audio(config).map_err(|e| e.to_string())?;
    Ok("Test audio started".to_string())
}

/// Stop test audio
#[tauri::command]
pub async fn stop_test_audio() -> Result<String, String> {
    println!("[CMD] stop_test_audio");
    audio::stop_test_audio().map_err(|e| e.to_string())?;
    Ok("Test audio stopped".to_string())
}

// ============================================================================
// Network Commands
// ============================================================================

/// Initialize P2P network
#[tauri::command]
pub async fn init_network(my_id: String) -> Result<network::NetworkInfo, String> {
    println!("[CMD] init_network: {}", my_id);
    network::init_network(&my_id).map_err(|e| e.to_string())
}

/// Get network info
#[tauri::command]
pub async fn get_network_info() -> Result<network::NetworkInfo, String> {
    network::get_info().map_err(|e| e.to_string())
}

/// Connect to peer
#[tauri::command]
pub async fn connect_to_peer(peer_id: String, addr: String) -> Result<String, String> {
    println!("[CMD] connect_to_peer: {} at {}", peer_id, addr);

    let peer_addr = network::connect_peer(&peer_id, &addr).map_err(|e| e.to_string())?;

    // Also set audio peer
    audio::set_peer(peer_addr);

    Ok(format!("Connected to {} at {}", peer_id, addr))
}

/// Disconnect from peer
#[tauri::command]
pub async fn disconnect_peer(peer_id: String) -> Result<String, String> {
    println!("[CMD] disconnect_peer: {}", peer_id);
    network::disconnect_peer(&peer_id).map_err(|e| e.to_string())?;
    Ok(format!("Disconnected from {}", peer_id))
}

/// Get peer latencies
#[tauri::command]
pub async fn get_peer_latencies() -> Result<Vec<network::PeerInfo>, String> {
    network::get_peer_latencies().map_err(|e| e.to_string())
}

/// Shutdown network
#[tauri::command]
pub async fn shutdown_network() -> Result<String, String> {
    println!("[CMD] shutdown_network");
    network::shutdown().map_err(|e| e.to_string())?;
    Ok("Network shutdown".to_string())
}

// ============================================================================
// Legacy Commands (for backward compatibility)
// ============================================================================

#[tauri::command]
pub async fn start_native_audio() -> Result<String, String> {
    Ok("Use start_audio instead".to_string())
}

#[tauri::command]
pub async fn stop_native_audio() -> Result<String, String> {
    stop_audio().await
}

#[tauri::command]
pub async fn set_audio_settings(settings: AudioSettings) -> Result<String, String> {
    println!("[CMD] set_audio_settings: {:?}", settings);
    Ok("Settings received (restart audio to apply)".to_string())
}

#[tauri::command]
pub async fn set_monitoring(enabled: bool) -> Result<String, String> {
    println!("[CMD] set_monitoring: {}", enabled);
    audio::set_monitoring(enabled);
    Ok(format!("Monitoring {}", if enabled { "enabled" } else { "disabled" }))
}

#[tauri::command]
pub async fn get_monitoring() -> Result<bool, String> {
    Ok(audio::get_monitoring())
}

#[tauri::command]
pub async fn set_monitoring_volume(_volume: u32) -> Result<String, String> {
    Ok("Monitoring volume not implemented".to_string())
}

#[tauri::command]
pub async fn get_monitoring_volume() -> Result<u32, String> {
    Ok(0)
}

#[tauri::command]
pub async fn start_p2p_network(my_id: String, _signaling_url: String) -> Result<network::NetworkInfo, String> {
    init_network(my_id).await
}

#[tauri::command]
pub async fn stop_p2p_network() -> Result<String, String> {
    shutdown_network().await
}
