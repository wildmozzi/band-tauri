//! SYNCROOM-style Audio Module
//!
//! Features:
//! - ASIO / WASAPI driver selection
//! - Sample rate: 44100 / 48000 Hz
//! - Buffer size: 64 / 128 / 256 / 512 / 1024 samples
//! - Compression: Uncompressed (PCM16) / Opus
//! - Channels: Mono / Stereo
//!
//! Architecture:
//! - Audio streams run in a dedicated thread (cpal::Stream is not Send)
//! - Communication via channels

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, BufferSize, StreamConfig};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::net::UdpSocket;
use std::thread::{self, JoinHandle};

// ============================================================================
// Types & Settings
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AudioDriver {
    Asio,
    WasapiExclusive,
    WasapiShared,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Compression {
    Uncompressed,  // Raw PCM 16-bit
    Opus,          // Opus codec
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Channels {
    Mono,
    Stereo,
}

/// 모노 모드에서 사용할 입력 채널
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MonoChannel {
    Ch1,  // 오인페 INPUT 1
    Ch2,  // 오인페 INPUT 2
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioConfig {
    pub driver: AudioDriver,           // 기본 드라이버 (하위 호환)
    pub input_driver: Option<AudioDriver>,  // 입력 드라이버 (None이면 driver 사용)
    pub output_driver: Option<AudioDriver>, // 출력 드라이버 (None이면 driver 사용)
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub sample_rate: u32,        // 44100 or 48000
    pub buffer_size: u32,        // 64, 128, 256, 512, 1024
    pub compression: Compression,
    pub channels: Channels,
    pub mono_channel: MonoChannel,     // 모노일 때 사용할 입력 채널 (Ch1/Ch2)
    pub stereo_swap: bool,             // 스테레오일 때 L/R 스왑
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            driver: AudioDriver::WasapiShared,
            input_driver: None,
            output_driver: None,
            input_device: None,
            output_device: None,
            sample_rate: 48000,
            buffer_size: 256,
            compression: Compression::Uncompressed,
            channels: Channels::Mono,
            mono_channel: MonoChannel::Ch1,
            stereo_swap: false,
        }
    }
}

impl AudioConfig {
    /// 실제 입력 드라이버 반환
    pub fn get_input_driver(&self) -> AudioDriver {
        self.input_driver.unwrap_or(self.driver)
    }

    /// 실제 출력 드라이버 반환
    pub fn get_output_driver(&self) -> AudioDriver {
        self.output_driver.unwrap_or(self.driver)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AudioDeviceInfo {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub is_default: bool,
    pub is_input: bool,
    pub sample_rates: Vec<u32>,
}

#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct AudioStats {
    pub is_running: bool,
    pub driver: String,
    pub sample_rate: u32,
    pub buffer_size: u32,
    pub compression: String,
    pub input_level: f32,
    pub output_level: f32,
}

// ============================================================================
// Shared State (Send + Sync)
// ============================================================================

struct SharedState {
    is_running: AtomicBool,
    config: RwLock<AudioConfig>,
    socket: RwLock<Option<Arc<UdpSocket>>>,
    peer_addr: RwLock<Option<std::net::SocketAddr>>,
    output_buffer: Mutex<Vec<i16>>,      // 수신된 원격 오디오
    monitor_buffer: Mutex<Vec<i16>>,     // 로컬 입력 (모니터링용)
    monitoring_enabled: AtomicBool,       // 모니터링 활성화 여부
    input_level: AtomicU32,  // f32 bits
    output_level: AtomicU32, // f32 bits
}

impl SharedState {
    fn new() -> Self {
        Self {
            is_running: AtomicBool::new(false),
            config: RwLock::new(AudioConfig::default()),
            socket: RwLock::new(None),
            peer_addr: RwLock::new(None),
            output_buffer: Mutex::new(Vec::with_capacity(48000)),
            monitor_buffer: Mutex::new(Vec::with_capacity(48000)),
            monitoring_enabled: AtomicBool::new(false),
            input_level: AtomicU32::new(0),
            output_level: AtomicU32::new(0),
        }
    }

    fn set_input_level(&self, level: f32) {
        self.input_level.store(level.to_bits(), Ordering::Relaxed);
    }

    fn set_output_level(&self, level: f32) {
        self.output_level.store(level.to_bits(), Ordering::Relaxed);
    }

    fn get_input_level(&self) -> f32 {
        f32::from_bits(self.input_level.load(Ordering::Relaxed))
    }

    fn get_output_level(&self) -> f32 {
        f32::from_bits(self.output_level.load(Ordering::Relaxed))
    }

    fn is_monitoring(&self) -> bool {
        self.monitoring_enabled.load(Ordering::Relaxed)
    }

    fn set_monitoring(&self, enabled: bool) {
        self.monitoring_enabled.store(enabled, Ordering::Relaxed);
    }
}

// ============================================================================
// Global State
// ============================================================================

lazy_static::lazy_static! {
    static ref SHARED: Arc<SharedState> = Arc::new(SharedState::new());
    static ref AUDIO_THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
    // ASIO는 동시 접근을 지원하지 않으므로 장치 열거를 직렬화
    static ref DEVICE_ENUM_LOCK: Mutex<()> = Mutex::new(());
    // ASIO 장치 이름 캐시 (열거 시 저장, 오디오 시작 시 사용)
    static ref ASIO_DEVICE_NAME: Mutex<Option<String>> = Mutex::new(None);
    // ASIO 장치 목록 캐시 (앱 시작 시 백그라운드에서 초기화)
    static ref CACHED_ASIO_DEVICES: Mutex<Vec<AudioDeviceInfo>> = Mutex::new(Vec::new());
}

// ASIO 캐시 초기화 완료 플래그
static ASIO_CACHE_INITIALIZED: AtomicBool = AtomicBool::new(false);

// ============================================================================
// Public API
// ============================================================================

/// ASIO 캐시 초기화 (백그라운드에서 호출하지 않음 - 싱글톤 문제)
/// 대신 get_audio_devices()에서 필요할 때 직접 열거
pub fn init_audio_cache() {
    // ASIO 싱글톤 문제로 인해 백그라운드 초기화 비활성화
    // 백그라운드에서 ASIO Host를 생성하면 이후 오디오 시작 시 Host가 장치를 못 찾음
    println!("[AUDIO] init_audio_cache: ASIO background init disabled (singleton issue)");
}

/// ASIO 장치 캐시 새로고침
pub fn refresh_audio_cache() {
    println!("[AUDIO] Refreshing ASIO cache...");
    ASIO_CACHE_INITIALIZED.store(false, Ordering::Relaxed);
    init_audio_cache();
}

/// ASIO 장치만 열거 (내부 함수 - Host drop 후 이름만 반환)
fn enumerate_asio_devices_internal() -> Result<Vec<AudioDeviceInfo>> {
    let hosts = cpal::available_hosts();
    let asio_id = hosts.into_iter().find(|h| h.name().to_lowercase().contains("asio"));

    let Some(asio_id) = asio_id else {
        println!("[AUDIO] ASIO host not found");
        return Ok(Vec::new());
    };

    // ASIO Host 생성 (이 함수 끝에서 자동 drop)
    let host = cpal::host_from_id(asio_id).map_err(|e| anyhow!("Failed to get ASIO host: {}", e))?;
    let mut devices = Vec::new();

    println!("[AUDIO] Enumerating ASIO devices...");

    // 기본 장치 이름 가져오기
    let default_input = host.default_input_device().and_then(|d| d.name().ok());
    let default_output = host.default_output_device().and_then(|d| d.name().ok());
    println!("[AUDIO] ASIO default input: {:?}, output: {:?}", default_input, default_output);

    // 입력 장치
    let mut first_input = true;
    if let Ok(inputs) = host.input_devices() {
        for device in inputs {
            if let Ok(name) = device.name() {
                println!("[AUDIO] Found ASIO input: {}", name);
                // 기본 장치가 없으면 첫 번째 장치를 기본으로
                let is_default = default_input.as_ref() == Some(&name) ||
                                 (default_input.is_none() && first_input);
                first_input = false;
                devices.push(AudioDeviceInfo {
                    id: format!("ASIO::{}", name),
                    name: name.clone(),
                    driver: "ASIO".to_string(),
                    is_default,
                    is_input: true,
                    sample_rates: vec![44100, 48000],
                });
            }
        }
    }

    // 출력 장치
    let mut first_output = true;
    if let Ok(outputs) = host.output_devices() {
        for device in outputs {
            if let Ok(name) = device.name() {
                println!("[AUDIO] Found ASIO output: {}", name);
                // 기본 장치가 없으면 첫 번째 장치를 기본으로
                let is_default = default_output.as_ref() == Some(&name) ||
                                 (default_output.is_none() && first_output);
                first_output = false;
                devices.push(AudioDeviceInfo {
                    id: format!("ASIO::{}", name),
                    name: name.clone(),
                    driver: "ASIO".to_string(),
                    is_default,
                    is_input: false,
                    sample_rates: vec![44100, 48000],
                });
            }
        }
    }

    // host는 여기서 자동 drop됨 - ASIO 싱글톤 해제
    Ok(devices)
}

/// Start audio engine
pub fn start_audio(config: AudioConfig, socket: UdpSocket) -> Result<()> {
    println!("[AUDIO] Starting with config: {:?}", config);

    // Stop if already running
    if SHARED.is_running.load(Ordering::SeqCst) {
        stop_audio()?;
    }

    // Update shared state
    {
        let mut cfg = SHARED.config.write().map_err(|e| anyhow!("{}", e))?;
        *cfg = config.clone();
    }
    {
        let mut sock = SHARED.socket.write().map_err(|e| anyhow!("{}", e))?;
        *sock = Some(Arc::new(socket));
    }

    SHARED.is_running.store(true, Ordering::SeqCst);

    // Spawn audio thread
    let shared = SHARED.clone();
    let handle = thread::spawn(move || {
        if let Err(e) = run_audio_thread(shared) {
            eprintln!("[AUDIO] Thread error: {}", e);
        }
    });

    // Store handle
    {
        let mut thread_handle = AUDIO_THREAD.lock().map_err(|e| anyhow!("{}", e))?;
        *thread_handle = Some(handle);
    }

    Ok(())
}

/// Stop audio engine
pub fn stop_audio() -> Result<()> {
    println!("[AUDIO] Stopping...");

    SHARED.is_running.store(false, Ordering::SeqCst);

    // Wait for thread
    if let Ok(mut handle) = AUDIO_THREAD.lock() {
        if let Some(h) = handle.take() {
            let _ = h.join();
        }
    }

    // Clear all buffers
    if let Ok(mut buf) = SHARED.output_buffer.lock() {
        buf.clear();
    }
    if let Ok(mut buf) = SHARED.monitor_buffer.lock() {
        buf.clear();
    }

    println!("[AUDIO] Stopped");
    Ok(())
}

/// Start test audio (monitoring only, no network)
pub fn start_test_audio(config: AudioConfig) -> Result<()> {
    println!("[AUDIO] Starting test mode with config: {:?}", config);

    // Stop if running
    if SHARED.is_running.load(Ordering::SeqCst) {
        stop_audio()?;
    }

    // Update config
    {
        let mut cfg = SHARED.config.write().map_err(|e| anyhow!("{}", e))?;
        *cfg = config;
    }

    // Enable monitoring
    SHARED.set_monitoring(true);
    SHARED.is_running.store(true, Ordering::SeqCst);

    // Start test thread (no network, just monitoring)
    let shared = SHARED.clone();
    let handle = thread::spawn(move || {
        if let Err(e) = run_test_audio_thread(shared) {
            eprintln!("[AUDIO] Test thread error: {}", e);
        }
    });

    // Store handle
    {
        let mut thread_handle = AUDIO_THREAD.lock().map_err(|e| anyhow!("{}", e))?;
        *thread_handle = Some(handle);
    }

    Ok(())
}

/// Stop test audio
pub fn stop_test_audio() -> Result<()> {
    println!("[AUDIO] Stopping test mode...");
    SHARED.set_monitoring(false);
    stop_audio()
}

/// Set peer address
pub fn set_peer(addr: std::net::SocketAddr) {
    println!("[AUDIO] Set peer: {}", addr);
    if let Ok(mut peer) = SHARED.peer_addr.write() {
        *peer = Some(addr);
    }
}

/// Get audio stats
pub fn get_stats() -> AudioStats {
    let config = SHARED.config.read().map(|c| c.clone()).unwrap_or_default();

    AudioStats {
        is_running: SHARED.is_running.load(Ordering::SeqCst),
        driver: format!("{:?}", config.driver),
        sample_rate: config.sample_rate,
        buffer_size: config.buffer_size,
        compression: format!("{:?}", config.compression),
        input_level: SHARED.get_input_level(),
        output_level: SHARED.get_output_level(),
    }
}

/// Enable/disable monitoring (hear your own input)
pub fn set_monitoring(enabled: bool) {
    println!("[AUDIO] Monitoring: {}", enabled);
    SHARED.set_monitoring(enabled);

    // Clear monitor buffer when disabling
    if !enabled {
        if let Ok(mut buf) = SHARED.monitor_buffer.lock() {
            buf.clear();
        }
    }
}

/// Get monitoring state
pub fn get_monitoring() -> bool {
    SHARED.is_monitoring()
}

// ============================================================================
// Audio Thread
// ============================================================================

/// Test audio thread - input to output only (no network)
fn run_test_audio_thread(shared: Arc<SharedState>) -> Result<()> {
    let config = shared.config.read().map_err(|e| anyhow!("{}", e))?.clone();

    println!("[AUDIO] Test thread started");

    // Acquire lock for ASIO compatibility
    let _lock = DEVICE_ENUM_LOCK.lock().map_err(|e| anyhow!("Lock error: {}", e))?;

    // Get hosts for input and output
    let input_driver = config.get_input_driver();
    let output_driver = config.get_output_driver();

    // Check for ASIO full duplex mode (both input and output are ASIO)
    let is_asio_duplex = matches!(input_driver, AudioDriver::Asio) && matches!(output_driver, AudioDriver::Asio);

    if is_asio_duplex {
        println!("[AUDIO] ASIO Full Duplex mode - using synchronized I/O");
        return run_asio_duplex_test(shared, config, _lock);
    }

    let input_host = get_host_for_driver(input_driver)?;
    println!("[AUDIO] Test input host: {}", input_host.id().name());

    let output_host = if output_driver == input_driver {
        input_host.id()
    } else {
        get_host_for_driver(output_driver)?.id()
    };
    let output_host = cpal::host_from_id(output_host)?;
    println!("[AUDIO] Test output host: {}", output_host.id().name());

    // Setup streams (with lock held for ASIO)
    let input_stream = setup_input_stream(&input_host, &config, shared.clone())?;
    let output_stream = setup_output_stream(&output_host, &config, shared.clone())?;

    // Release lock after stream setup
    drop(_lock);

    // Play streams
    input_stream.play()?;
    output_stream.play()?;

    println!("[AUDIO] Test streams playing (monitoring enabled)");

    // Keep thread alive while running
    while shared.is_running.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(100));
    }

    println!("[AUDIO] Test thread exiting");
    Ok(())
}

/// ASIO Full Duplex test mode - synchronized input/output on same device
fn run_asio_duplex_test(shared: Arc<SharedState>, config: AudioConfig, _lock: std::sync::MutexGuard<'_, ()>) -> Result<()> {
    let host = get_host_for_driver(AudioDriver::Asio)?;
    println!("[AUDIO] ASIO Duplex: host = {}", host.id().name());

    // Get ASIO device (bidirectional)
    let device = host.default_output_device()
        .or_else(|| host.default_input_device())
        .ok_or_else(|| anyhow!("No ASIO device available"))?;

    let device_name = device.name()?;
    println!("[AUDIO] ASIO Duplex device: {}", device_name);

    // Get supported configs
    let input_config = device.default_input_config()?;
    let output_config = device.default_output_config()?;

    println!("[AUDIO] ASIO input config: {:?}", input_config);
    println!("[AUDIO] ASIO output config: {:?}", output_config);

    let input_format = input_config.sample_format();
    let output_format = output_config.sample_format();

    // Use fixed buffer size for both (ASIO respects this)
    let stream_config = StreamConfig {
        channels: 2,
        sample_rate: SampleRate(config.sample_rate),
        buffer_size: BufferSize::Fixed(config.buffer_size),
    };

    println!("[AUDIO] ASIO Duplex using config: {:?}", stream_config);

    // Create a direct passthrough buffer for zero-latency monitoring
    // Size = exactly one buffer worth of stereo samples
    let duplex_buffer = Arc::new(Mutex::new(vec![0i16; config.buffer_size as usize * 2]));
    let duplex_buffer_ready = Arc::new(AtomicBool::new(false));

    let channel_mode = config.channels;
    let mono_channel = config.mono_channel;
    let stereo_swap = config.stereo_swap;

    // Build input stream
    let shared_in = shared.clone();
    let buf_in = duplex_buffer.clone();
    let ready_in = duplex_buffer_ready.clone();

    let input_stream = match input_format {
        SampleFormat::I32 => {
            device.build_input_stream(
                &stream_config,
                move |data: &[i32], _: &cpal::InputCallbackInfo| {
                    // Route channels
                    let routed = route_channels_i32(data, 2, channel_mode, mono_channel, stereo_swap);

                    // Convert to i16 and store in duplex buffer
                    let samples: Vec<i16> = routed.iter().map(|&s| (s >> 16) as i16).collect();

                    if let Ok(mut buf) = buf_in.lock() {
                        let copy_len = samples.len().min(buf.len());
                        buf[..copy_len].copy_from_slice(&samples[..copy_len]);
                        ready_in.store(true, Ordering::Release);
                    }

                    shared_in.set_input_level(calculate_level_i16(&samples));
                },
                |err| eprintln!("[ASIO_DUPLEX] Input error: {}", err),
                None,
            )?
        }
        SampleFormat::F32 => {
            device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let routed = route_channels_f32(data, 2, channel_mode, mono_channel, stereo_swap);
                    let samples: Vec<i16> = routed.iter().map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect();

                    if let Ok(mut buf) = buf_in.lock() {
                        let copy_len = samples.len().min(buf.len());
                        buf[..copy_len].copy_from_slice(&samples[..copy_len]);
                        ready_in.store(true, Ordering::Release);
                    }

                    shared_in.set_input_level(calculate_level_i16(&samples));
                },
                |err| eprintln!("[ASIO_DUPLEX] Input error: {}", err),
                None,
            )?
        }
        _ => return Err(anyhow!("Unsupported ASIO input format: {:?}", input_format)),
    };

    // Build output stream
    let shared_out = shared.clone();
    let buf_out = duplex_buffer.clone();
    let ready_out = duplex_buffer_ready.clone();

    let output_stream = match output_format {
        SampleFormat::I32 => {
            device.build_output_stream(
                &stream_config,
                move |data: &mut [i32], _: &cpal::OutputCallbackInfo| {
                    if !shared_out.is_running.load(Ordering::Relaxed) || !shared_out.is_monitoring() {
                        data.fill(0);
                        return;
                    }

                    if ready_out.load(Ordering::Acquire) {
                        if let Ok(buf) = buf_out.lock() {
                            for (i, sample) in buf.iter().enumerate() {
                                if i < data.len() {
                                    data[i] = (*sample as i32) << 16;
                                }
                            }
                        }
                        ready_out.store(false, Ordering::Release);
                    } else {
                        // No new data - output silence (no underrun in duplex mode)
                        data.fill(0);
                    }

                    shared_out.set_output_level(calculate_level_i32(data));
                },
                |err| eprintln!("[ASIO_DUPLEX] Output error: {}", err),
                None,
            )?
        }
        SampleFormat::F32 => {
            device.build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if !shared_out.is_running.load(Ordering::Relaxed) || !shared_out.is_monitoring() {
                        data.fill(0.0);
                        return;
                    }

                    if ready_out.load(Ordering::Acquire) {
                        if let Ok(buf) = buf_out.lock() {
                            for (i, sample) in buf.iter().enumerate() {
                                if i < data.len() {
                                    data[i] = *sample as f32 / 32768.0;
                                }
                            }
                        }
                        ready_out.store(false, Ordering::Release);
                    } else {
                        data.fill(0.0);
                    }

                    shared_out.set_output_level(calculate_level_f32(data));
                },
                |err| eprintln!("[ASIO_DUPLEX] Output error: {}", err),
                None,
            )?
        }
        _ => return Err(anyhow!("Unsupported ASIO output format: {:?}", output_format)),
    };

    // Release ASIO lock after stream setup
    drop(_lock);

    // Play both streams
    input_stream.play()?;
    output_stream.play()?;

    println!("[AUDIO] ASIO Duplex streams playing - latency: ~{:.1}ms",
             config.buffer_size as f32 / config.sample_rate as f32 * 1000.0);

    // Keep thread alive
    while shared.is_running.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(100));
    }

    println!("[AUDIO] ASIO Duplex thread exiting");
    Ok(())
}

fn run_audio_thread(shared: Arc<SharedState>) -> Result<()> {
    let config = shared.config.read().map_err(|e| anyhow!("{}", e))?.clone();

    println!("[AUDIO] Thread started");

    // Acquire lock for ASIO compatibility
    let _lock = DEVICE_ENUM_LOCK.lock().map_err(|e| anyhow!("Lock error: {}", e))?;

    // Get hosts for input and output (may be different)
    let input_driver = config.get_input_driver();
    let output_driver = config.get_output_driver();

    // Check for ASIO full duplex mode (both input and output are ASIO)
    let is_asio_duplex = matches!(input_driver, AudioDriver::Asio) && matches!(output_driver, AudioDriver::Asio);

    if is_asio_duplex {
        println!("[AUDIO] ASIO Full Duplex mode - using synchronized I/O with network");
        return run_asio_duplex_network(shared, config, _lock);
    }

    let input_host = get_host_for_driver(input_driver)?;
    println!("[AUDIO] Input host: {}", input_host.id().name());

    let output_host = if output_driver == input_driver {
        // Same driver - reuse host
        input_host.id()
    } else {
        get_host_for_driver(output_driver)?.id()
    };
    let output_host = cpal::host_from_id(output_host)?;
    println!("[AUDIO] Output host: {}", output_host.id().name());

    // Setup input stream (with lock held for ASIO)
    let input_stream = setup_input_stream(&input_host, &config, shared.clone())?;

    // Setup output stream (with lock held for ASIO)
    let output_stream = setup_output_stream(&output_host, &config, shared.clone())?;

    // Release lock after stream setup
    drop(_lock);

    // Start receive thread
    start_receive_thread(shared.clone(), config.compression, config.channels, config.sample_rate)?;

    // Play streams
    input_stream.play()?;
    output_stream.play()?;

    println!("[AUDIO] Streams playing");

    // Keep thread alive while running
    while shared.is_running.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(100));
    }

    // Streams are dropped here when thread exits
    println!("[AUDIO] Thread exiting");
    Ok(())
}

/// ASIO Full Duplex network mode - synchronized input/output with network transmission
fn run_asio_duplex_network(shared: Arc<SharedState>, config: AudioConfig, _lock: std::sync::MutexGuard<'_, ()>) -> Result<()> {
    let host = get_host_for_driver(AudioDriver::Asio)?;
    println!("[AUDIO] ASIO Duplex Network: host = {}", host.id().name());

    // Get ASIO device (bidirectional)
    let device = host.default_output_device()
        .or_else(|| host.default_input_device())
        .ok_or_else(|| anyhow!("No ASIO device available"))?;

    let device_name = device.name()?;
    println!("[AUDIO] ASIO Duplex device: {}", device_name);

    // Get supported configs
    let input_config = device.default_input_config()?;
    let output_config = device.default_output_config()?;

    let input_format = input_config.sample_format();
    let output_format = output_config.sample_format();

    // Use fixed buffer size for both
    let stream_config = StreamConfig {
        channels: 2,
        sample_rate: SampleRate(config.sample_rate),
        buffer_size: BufferSize::Fixed(config.buffer_size),
    };

    println!("[AUDIO] ASIO Duplex Network using config: {:?}", stream_config);

    // Setup Opus encoder if needed
    let opus_encoder = if config.compression == Compression::Opus {
        let ch = match config.channels {
            Channels::Mono => opus::Channels::Mono,
            Channels::Stereo => opus::Channels::Stereo,
        };
        Some(Arc::new(Mutex::new(
            opus::Encoder::new(config.sample_rate, ch, opus::Application::LowDelay)?
        )))
    } else {
        None
    };

    let compression = config.compression;
    let channel_mode = config.channels;
    let mono_channel = config.mono_channel;
    let stereo_swap = config.stereo_swap;

    // Build input stream (captures and sends over network)
    let shared_in = shared.clone();
    let encoder = opus_encoder.clone();

    let input_stream = match input_format {
        SampleFormat::I32 => {
            device.build_input_stream(
                &stream_config,
                move |data: &[i32], _: &cpal::InputCallbackInfo| {
                    if !shared_in.is_running.load(Ordering::Relaxed) {
                        return;
                    }

                    // Route channels and convert to i16
                    let routed = route_channels_i32(data, 2, channel_mode, mono_channel, stereo_swap);
                    let samples: Vec<i16> = routed.iter().map(|&s| (s >> 16) as i16).collect();

                    shared_in.set_input_level(calculate_level_i16(&samples));

                    // Copy to monitor buffer if enabled
                    if shared_in.is_monitoring() {
                        if let Ok(mut buf) = shared_in.monitor_buffer.try_lock() {
                            buf.extend_from_slice(&samples);
                            let max_size = (samples.len() * 4).max(8192);
                            let buf_len = buf.len();
                            if buf_len > max_size {
                                buf.drain(0..buf_len - max_size);
                            }
                        }
                    }

                    // Send over network
                    send_audio(&shared_in, &samples, compression, &encoder);
                },
                |err| eprintln!("[ASIO_DUPLEX_NET] Input error: {}", err),
                None,
            )?
        }
        SampleFormat::F32 => {
            device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if !shared_in.is_running.load(Ordering::Relaxed) {
                        return;
                    }

                    let routed = route_channels_f32(data, 2, channel_mode, mono_channel, stereo_swap);
                    let samples: Vec<i16> = routed.iter().map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect();

                    shared_in.set_input_level(calculate_level_i16(&samples));

                    if shared_in.is_monitoring() {
                        if let Ok(mut buf) = shared_in.monitor_buffer.try_lock() {
                            buf.extend_from_slice(&samples);
                            let max_size = (samples.len() * 4).max(8192);
                            let buf_len = buf.len();
                            if buf_len > max_size {
                                buf.drain(0..buf_len - max_size);
                            }
                        }
                    }

                    send_audio(&shared_in, &samples, compression, &encoder);
                },
                |err| eprintln!("[ASIO_DUPLEX_NET] Input error: {}", err),
                None,
            )?
        }
        _ => return Err(anyhow!("Unsupported ASIO input format: {:?}", input_format)),
    };

    // Build output stream (plays received network audio + monitor)
    let shared_out = shared.clone();

    let output_stream = match output_format {
        SampleFormat::I32 => {
            device.build_output_stream(
                &stream_config,
                move |data: &mut [i32], _: &cpal::OutputCallbackInfo| {
                    process_output_i32(data, &shared_out);
                },
                |err| eprintln!("[ASIO_DUPLEX_NET] Output error: {}", err),
                None,
            )?
        }
        SampleFormat::F32 => {
            device.build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    process_output_f32(data, &shared_out);
                },
                |err| eprintln!("[ASIO_DUPLEX_NET] Output error: {}", err),
                None,
            )?
        }
        _ => return Err(anyhow!("Unsupported ASIO output format: {:?}", output_format)),
    };

    // Release lock after stream setup
    drop(_lock);

    // Start receive thread for network audio
    start_receive_thread(shared.clone(), config.compression, config.channels, config.sample_rate)?;

    // Play both streams
    input_stream.play()?;
    output_stream.play()?;

    println!("[AUDIO] ASIO Duplex Network streams playing - latency: ~{:.1}ms",
             config.buffer_size as f32 / config.sample_rate as f32 * 1000.0);

    // Keep thread alive
    while shared.is_running.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(100));
    }

    println!("[AUDIO] ASIO Duplex Network thread exiting");
    Ok(())
}

fn get_host_for_driver(driver: AudioDriver) -> Result<cpal::Host> {
    let host_id = match driver {
        AudioDriver::Asio => {
            cpal::available_hosts()
                .into_iter()
                .find(|id| id.name().to_lowercase().contains("asio"))
                .ok_or_else(|| anyhow!("ASIO not available"))?
        }
        AudioDriver::WasapiExclusive | AudioDriver::WasapiShared => {
            cpal::available_hosts()
                .into_iter()
                .find(|id| id.name().to_lowercase().contains("wasapi"))
                .unwrap_or(cpal::default_host().id())
        }
    };

    println!("[AUDIO] Creating host for {:?} (host_id: {})", driver, host_id.name());
    let host = cpal::host_from_id(host_id)?;

    // Debug: list all devices
    let input_count = host.input_devices().map(|d| d.count()).unwrap_or(0);
    let output_count = host.output_devices().map(|d| d.count()).unwrap_or(0);
    let default_in = host.default_input_device().and_then(|d| d.name().ok());
    let default_out = host.default_output_device().and_then(|d| d.name().ok());
    println!("[AUDIO] Host {} - inputs: {}, outputs: {}, default_in: {:?}, default_out: {:?}",
             host_id.name(), input_count, output_count, default_in, default_out);

    Ok(host)
}

fn get_host(config: &AudioConfig) -> Result<cpal::Host> {
    get_host_for_driver(config.driver)
}

fn setup_input_stream(host: &cpal::Host, config: &AudioConfig, shared: Arc<SharedState>) -> Result<cpal::Stream> {
    // Get device - for ASIO, try default device first since input_devices() may return empty
    let is_asio = host.id().name().to_lowercase().contains("asio");

    let device = if is_asio {
        // ASIO: devices are bidirectional - try input first, then output as fallback
        println!("[AUDIO] ASIO mode: looking for input device");
        host.default_input_device()
            .or_else(|| {
                println!("[AUDIO] ASIO: No default input, trying output device (bidirectional)");
                host.default_output_device()
            })
            .ok_or_else(|| anyhow!("No ASIO device available"))?
    } else if let Some(ref name) = config.input_device {
        // Non-ASIO: try to find specified device
        let available: Vec<String> = host.input_devices()?
            .filter_map(|d| d.name().ok())
            .collect();
        println!("[AUDIO] Available input devices in {}: {:?}", host.id().name(), available);

        let found = host.input_devices()?
            .find(|d| d.name().map(|n| n == *name).unwrap_or(false));

        match found {
            Some(d) => d,
            None => {
                println!("[AUDIO] Device '{}' not found in {}, using default", name, host.id().name());
                host.default_input_device()
                    .ok_or_else(|| anyhow!("No default input device in {}", host.id().name()))?
            }
        }
    } else {
        host.default_input_device()
            .ok_or_else(|| anyhow!("No default input device"))?
    };

    println!("[AUDIO] Input device: {}", device.name()?);

    let supported = device.default_input_config()?;
    let sample_format = supported.sample_format();
    println!("[AUDIO] Input format: {:?}", sample_format);
    println!("[AUDIO] Input supported config: {:?}", supported);

    // Configure stream based on driver type
    let input_driver = config.get_input_driver();
    let stream_config = if matches!(input_driver, AudioDriver::WasapiShared) {
        // WASAPI Shared: use device native format (some devices don't support channel conversion)
        StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: BufferSize::Default,
        }
    } else if matches!(input_driver, AudioDriver::WasapiExclusive) {
        // WASAPI Exclusive: use device native format
        StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: BufferSize::Fixed(config.buffer_size),
        }
    } else {
        // ASIO: always request 2 channels (for ch1/ch2 selection), do routing in software
        StreamConfig {
            channels: 2,
            sample_rate: SampleRate(config.sample_rate),
            buffer_size: BufferSize::Fixed(config.buffer_size),
        }
    };
    println!("[AUDIO] Using input config: {:?}", stream_config);

    // Setup Opus encoder if needed
    let opus_encoder = if config.compression == Compression::Opus {
        let ch = match config.channels {
            Channels::Mono => opus::Channels::Mono,
            Channels::Stereo => opus::Channels::Stereo,
        };
        Some(Arc::new(Mutex::new(
            opus::Encoder::new(config.sample_rate, ch, opus::Application::LowDelay)?
        )))
    } else {
        None
    };

    let compression = config.compression;
    let input_channels = stream_config.channels as usize;
    let channel_mode = config.channels;
    let mono_channel = config.mono_channel;
    let stereo_swap = config.stereo_swap;

    let stream = match sample_format {
        SampleFormat::I32 => {
            let shared = shared.clone();
            let encoder = opus_encoder.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[i32], _: &cpal::InputCallbackInfo| {
                    let stereo_data = route_channels_i32(data, input_channels, channel_mode, mono_channel, stereo_swap);
                    process_input_i32(&stereo_data, &shared, compression, &encoder);
                },
                |err| eprintln!("[AUDIO] Input error: {}", err),
                None,
            )?
        }
        SampleFormat::F32 => {
            let shared = shared.clone();
            let encoder = opus_encoder.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let stereo_data = route_channels_f32(data, input_channels, channel_mode, mono_channel, stereo_swap);
                    process_input_f32(&stereo_data, &shared, compression, &encoder);
                },
                |err| eprintln!("[AUDIO] Input error: {}", err),
                None,
            )?
        }
        SampleFormat::I16 => {
            let shared = shared.clone();
            let encoder = opus_encoder.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let stereo_data = route_channels_i16(data, input_channels, channel_mode, mono_channel, stereo_swap);
                    process_input_i16(&stereo_data, &shared, compression, &encoder);
                },
                |err| eprintln!("[AUDIO] Input error: {}", err),
                None,
            )?
        }
        _ => return Err(anyhow!("Unsupported sample format: {:?}", sample_format)),
    };

    Ok(stream)
}

fn setup_output_stream(host: &cpal::Host, config: &AudioConfig, shared: Arc<SharedState>) -> Result<cpal::Stream> {
    // Get device - for ASIO, try default device first
    let is_asio = host.id().name().to_lowercase().contains("asio");

    let device = if is_asio {
        // ASIO: always use default device (devices are bidirectional)
        println!("[AUDIO] ASIO mode: using default output device");
        host.default_output_device()
            .ok_or_else(|| anyhow!("No default ASIO output device"))?
    } else if let Some(ref name) = config.output_device {
        // Non-ASIO: try to find specified device
        let available: Vec<String> = host.output_devices()?
            .filter_map(|d| d.name().ok())
            .collect();
        println!("[AUDIO] Available output devices in {}: {:?}", host.id().name(), available);

        let found = host.output_devices()?
            .find(|d| d.name().map(|n| n == *name).unwrap_or(false));

        match found {
            Some(d) => d,
            None => {
                println!("[AUDIO] Device '{}' not found in {}, using default", name, host.id().name());
                host.default_output_device()
                    .ok_or_else(|| anyhow!("No default output device in {}", host.id().name()))?
            }
        }
    } else {
        host.default_output_device()
            .ok_or_else(|| anyhow!("No default output device"))?
    };

    println!("[AUDIO] Output device: {}", device.name()?);

    let supported = device.default_output_config()?;
    let sample_format = supported.sample_format();
    println!("[AUDIO] Output format: {:?}", sample_format);
    println!("[AUDIO] Output supported config: {:?}", supported);

    // Output stream config - use fixed buffer for lower latency
    // Note: Output always uses device native channels (usually stereo) - mono/stereo conversion happens in software
    let output_driver = config.get_output_driver();
    let stream_config = if matches!(output_driver, AudioDriver::WasapiShared) {
        // WASAPI Shared: use device native format (fixed buffer often ignored in shared mode)
        StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: BufferSize::Fixed(config.buffer_size),
        }
    } else if matches!(output_driver, AudioDriver::WasapiExclusive) {
        // WASAPI Exclusive: use device native channels but can set sample rate and buffer
        StreamConfig {
            channels: supported.channels(),  // Use device native (usually stereo)
            sample_rate: SampleRate(config.sample_rate),
            buffer_size: BufferSize::Fixed(config.buffer_size),
        }
    } else {
        // ASIO: can specify all parameters
        StreamConfig {
            channels: supported.channels(),  // ASIO output also use device native
            sample_rate: SampleRate(config.sample_rate),
            buffer_size: BufferSize::Fixed(config.buffer_size),
        }
    };
    println!("[AUDIO] Using output config: {:?}", stream_config);

    let stream = match sample_format {
        SampleFormat::I32 => {
            let shared = shared.clone();
            device.build_output_stream(
                &stream_config,
                move |data: &mut [i32], _: &cpal::OutputCallbackInfo| {
                    process_output_i32(data, &shared);
                },
                |err| eprintln!("[AUDIO] Output error: {}", err),
                None,
            )?
        }
        SampleFormat::F32 => {
            let shared = shared.clone();
            device.build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    process_output_f32(data, &shared);
                },
                |err| eprintln!("[AUDIO] Output error: {}", err),
                None,
            )?
        }
        SampleFormat::I16 => {
            let shared = shared.clone();
            device.build_output_stream(
                &stream_config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    process_output_i16(data, &shared);
                },
                |err| eprintln!("[AUDIO] Output error: {}", err),
                None,
            )?
        }
        _ => return Err(anyhow!("Unsupported sample format: {:?}", sample_format)),
    };

    Ok(stream)
}

// ============================================================================
// Channel Routing
// ============================================================================

/// Route input channels to stereo output based on settings
/// - Mono mode: Pick ch1 or ch2, duplicate to both L/R
/// - Stereo mode: Map channels with optional L/R swap
fn route_channels_i32(data: &[i32], input_channels: usize, channel_mode: Channels, mono_channel: MonoChannel, stereo_swap: bool) -> Vec<i32> {
    if input_channels == 0 {
        return Vec::new();
    }

    let frames = data.len() / input_channels;
    let mut stereo = Vec::with_capacity(frames * 2);

    for i in 0..frames {
        let frame_start = i * input_channels;
        let ch0 = data[frame_start];
        let ch1 = if input_channels > 1 { data[frame_start + 1] } else { ch0 };

        match channel_mode {
            Channels::Mono => {
                // Pick one channel and duplicate to both L/R
                let sample = match mono_channel {
                    MonoChannel::Ch1 => ch0,
                    MonoChannel::Ch2 => ch1,
                };
                stereo.push(sample);
                stereo.push(sample);
            }
            Channels::Stereo => {
                // Stereo with optional L/R swap
                if stereo_swap {
                    stereo.push(ch1);  // R -> L
                    stereo.push(ch0);  // L -> R
                } else {
                    stereo.push(ch0);  // L
                    stereo.push(ch1);  // R
                }
            }
        }
    }

    stereo
}

fn route_channels_f32(data: &[f32], input_channels: usize, channel_mode: Channels, mono_channel: MonoChannel, stereo_swap: bool) -> Vec<f32> {
    if input_channels == 0 {
        return Vec::new();
    }

    let frames = data.len() / input_channels;
    let mut stereo = Vec::with_capacity(frames * 2);

    for i in 0..frames {
        let frame_start = i * input_channels;
        let ch0 = data[frame_start];
        let ch1 = if input_channels > 1 { data[frame_start + 1] } else { ch0 };

        match channel_mode {
            Channels::Mono => {
                let sample = match mono_channel {
                    MonoChannel::Ch1 => ch0,
                    MonoChannel::Ch2 => ch1,
                };
                stereo.push(sample);
                stereo.push(sample);
            }
            Channels::Stereo => {
                if stereo_swap {
                    stereo.push(ch1);
                    stereo.push(ch0);
                } else {
                    stereo.push(ch0);
                    stereo.push(ch1);
                }
            }
        }
    }

    stereo
}

fn route_channels_i16(data: &[i16], input_channels: usize, channel_mode: Channels, mono_channel: MonoChannel, stereo_swap: bool) -> Vec<i16> {
    if input_channels == 0 {
        return Vec::new();
    }

    let frames = data.len() / input_channels;
    let mut stereo = Vec::with_capacity(frames * 2);

    for i in 0..frames {
        let frame_start = i * input_channels;
        let ch0 = data[frame_start];
        let ch1 = if input_channels > 1 { data[frame_start + 1] } else { ch0 };

        match channel_mode {
            Channels::Mono => {
                let sample = match mono_channel {
                    MonoChannel::Ch1 => ch0,
                    MonoChannel::Ch2 => ch1,
                };
                stereo.push(sample);
                stereo.push(sample);
            }
            Channels::Stereo => {
                if stereo_swap {
                    stereo.push(ch1);
                    stereo.push(ch0);
                } else {
                    stereo.push(ch0);
                    stereo.push(ch1);
                }
            }
        }
    }

    stereo
}

// ============================================================================
// Input Processing
// ============================================================================

fn process_input_i32(data: &[i32], shared: &SharedState, compression: Compression, encoder: &Option<Arc<Mutex<opus::Encoder>>>) {
    if !shared.is_running.load(Ordering::Relaxed) {
        return;
    }

    // Convert I32 to I16
    let samples: Vec<i16> = data.iter().map(|&s| (s >> 16) as i16).collect();

    // Update level
    shared.set_input_level(calculate_level_i16(&samples));

    // Copy to monitor buffer if monitoring enabled (non-blocking to avoid lock contention with output)
    if shared.is_monitoring() {
        if let Ok(mut buf) = shared.monitor_buffer.try_lock() {
            buf.extend_from_slice(&samples);
            // Scale with buffer size but ensure minimum for smooth playback
            let max_size = (samples.len() * 4).max(8192);
            let buf_len = buf.len();
            if buf_len > max_size {
                buf.drain(0..buf_len - max_size);
            }
        }
        // If lock fails, skip this buffer (output is reading - avoid blocking)
    }

    // Send
    send_audio(shared, &samples, compression, encoder);
}

fn process_input_f32(data: &[f32], shared: &SharedState, compression: Compression, encoder: &Option<Arc<Mutex<opus::Encoder>>>) {
    if !shared.is_running.load(Ordering::Relaxed) {
        return;
    }

    // Convert F32 to I16
    let samples: Vec<i16> = data.iter().map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect();

    // Update level and log periodically
    let level = calculate_level_i16(&samples);
    shared.set_input_level(level);

    // Log input level every 500 callbacks (~5 seconds at 48kHz/960 samples)
    static INPUT_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let c = INPUT_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if c % 500 == 0 {
        println!("[INPUT_LEVEL] #{} level={:.4}", c / 500, level);
    }

    // Copy to monitor buffer if monitoring enabled (non-blocking to avoid lock contention with output)
    if shared.is_monitoring() {
        if let Ok(mut buf) = shared.monitor_buffer.try_lock() {
            buf.extend_from_slice(&samples);
            let max_size = (samples.len() * 4).max(8192);
            let buf_len = buf.len();
            if buf_len > max_size {
                buf.drain(0..buf_len - max_size);
            }
        }
        // If lock fails, skip this buffer (output is reading - avoid blocking)
    }

    // Send
    send_audio(shared, &samples, compression, encoder);
}

fn process_input_i16(data: &[i16], shared: &SharedState, compression: Compression, encoder: &Option<Arc<Mutex<opus::Encoder>>>) {
    if !shared.is_running.load(Ordering::Relaxed) {
        return;
    }

    // Update level
    shared.set_input_level(calculate_level_i16(data));

    // Copy to monitor buffer if monitoring enabled (non-blocking to avoid lock contention with output)
    if shared.is_monitoring() {
        if let Ok(mut buf) = shared.monitor_buffer.try_lock() {
            buf.extend_from_slice(data);
            let max_size = (data.len() * 4).max(8192);
            let buf_len = buf.len();
            if buf_len > max_size {
                buf.drain(0..buf_len - max_size);
            }
        }
        // If lock fails, skip this buffer (output is reading - avoid blocking)
    }

    // Send
    send_audio(shared, data, compression, encoder);
}

fn send_audio(shared: &SharedState, samples: &[i16], compression: Compression, encoder: &Option<Arc<Mutex<opus::Encoder>>>) {
    let socket = match shared.socket.read().ok().and_then(|s| s.clone()) {
        Some(s) => s,
        None => return,
    };

    let peer = match shared.peer_addr.read().ok().and_then(|p| *p) {
        Some(p) => p,
        None => return,
    };

    let mut packet = Vec::with_capacity(samples.len() * 2 + 2);
    packet.push(0); // packet type: audio

    match compression {
        Compression::Uncompressed => {
            packet.push(0); // not compressed
            for sample in samples {
                packet.extend_from_slice(&sample.to_le_bytes());
            }
        }
        Compression::Opus => {
            packet.push(1); // compressed
            if let Some(enc) = encoder {
                if let Ok(mut enc) = enc.lock() {
                    let mut opus_buf = vec![0u8; 4000];
                    if let Ok(len) = enc.encode(samples, &mut opus_buf) {
                        packet.extend_from_slice(&opus_buf[..len]);
                    }
                }
            }
        }
    }

    let _ = socket.send_to(&packet, peer);
}

// ============================================================================
// Output Processing
// ============================================================================

fn process_output_i32(data: &mut [i32], shared: &SharedState) {
    if !shared.is_running.load(Ordering::Relaxed) {
        data.fill(0);
        return;
    }

    // Start with zeros
    data.fill(0);

    // Add remote audio from output_buffer
    if let Ok(mut buf) = shared.output_buffer.try_lock() {
        let take = data.len().min(buf.len());
        for (i, sample) in buf.drain(..take).enumerate() {
            data[i] = (sample as i32) << 16;
        }
    }

    // Mix in monitor audio if enabled
    if shared.is_monitoring() {
        if let Ok(mut mon) = shared.monitor_buffer.try_lock() {
            let take = data.len().min(mon.len());
            for (i, sample) in mon.drain(..take).enumerate() {
                // Mix by adding (with clipping protection)
                let mixed = data[i].saturating_add((sample as i32) << 16);
                data[i] = mixed;
            }
        }
    }

    shared.set_output_level(calculate_level_i32(data));
}

fn process_output_f32(data: &mut [f32], shared: &SharedState) {
    if !shared.is_running.load(Ordering::Relaxed) {
        data.fill(0.0);
        return;
    }

    // Start with zeros
    data.fill(0.0);

    // Add remote audio from output_buffer
    if let Ok(mut buf) = shared.output_buffer.try_lock() {
        let take = data.len().min(buf.len());
        for (i, sample) in buf.drain(..take).enumerate() {
            data[i] = sample as f32 / 32768.0;
        }
    }

    // Mix in monitor audio if enabled
    if shared.is_monitoring() {
        match shared.monitor_buffer.try_lock() {
            Ok(mut mon) => {
                let mon_len = mon.len();
                let take = data.len().min(mon_len);

                // Log underrun (when we can't fill the entire output buffer)
                if take < data.len() {
                    static UNDERRUN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                    let c = UNDERRUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    println!("[UNDERRUN] #{} need={}, have={}, missing={}", c, data.len(), mon_len, data.len() - take);
                }

                for (i, sample) in mon.drain(..take).enumerate() {
                    data[i] = (data[i] + sample as f32 / 32768.0).clamp(-1.0, 1.0);
                }
            }
            Err(_) => {
                println!("[OUTPUT_F32] try_lock FAILED!");
            }
        }
    }

    shared.set_output_level(calculate_level_f32(data));
}

fn process_output_i16(data: &mut [i16], shared: &SharedState) {
    if !shared.is_running.load(Ordering::Relaxed) {
        data.fill(0);
        return;
    }

    // Start with zeros
    data.fill(0);

    // Add remote audio from output_buffer
    if let Ok(mut buf) = shared.output_buffer.try_lock() {
        let take = data.len().min(buf.len());
        for (i, sample) in buf.drain(..take).enumerate() {
            data[i] = sample;
        }
    }

    // Mix in monitor audio if enabled
    if shared.is_monitoring() {
        if let Ok(mut mon) = shared.monitor_buffer.try_lock() {
            let take = data.len().min(mon.len());
            for (i, sample) in mon.drain(..take).enumerate() {
                // Mix by adding (with saturation)
                data[i] = data[i].saturating_add(sample);
            }
        }
    }

    shared.set_output_level(calculate_level_i16(data));
}

// ============================================================================
// Receive Thread
// ============================================================================

fn start_receive_thread(shared: Arc<SharedState>, compression: Compression, channels: Channels, sample_rate: u32) -> Result<()> {
    let socket = shared.socket.read()
        .map_err(|e| anyhow!("{}", e))?
        .clone()
        .ok_or_else(|| anyhow!("No socket"))?;

    // Clone socket for receive thread
    let recv_socket = socket.try_clone()?;
    recv_socket.set_read_timeout(Some(std::time::Duration::from_millis(100)))?;

    thread::spawn(move || {
        let mut decoder = if compression == Compression::Opus {
            let ch = match channels {
                Channels::Mono => opus::Channels::Mono,
                Channels::Stereo => opus::Channels::Stereo,
            };
            opus::Decoder::new(sample_rate, ch).ok()
        } else {
            None
        };

        let mut recv_buf = [0u8; 8192];

        while shared.is_running.load(Ordering::Relaxed) {
            match recv_socket.recv_from(&mut recv_buf) {
                Ok((len, _src)) => {
                    if len < 2 {
                        continue;
                    }

                    let packet_type = recv_buf[0];
                    let is_compressed = recv_buf[1] == 1;

                    if packet_type != 0 {
                        continue;
                    }

                    let audio_data = &recv_buf[2..len];

                    let samples: Vec<i16> = if is_compressed {
                        if let Some(ref mut dec) = decoder {
                            let mut output = vec![0i16; 2880];
                            match dec.decode(audio_data, &mut output, false) {
                                Ok(decoded_len) => {
                                    output.truncate(decoded_len);
                                    output
                                }
                                Err(_) => continue,
                            }
                        } else {
                            continue;
                        }
                    } else {
                        audio_data.chunks_exact(2)
                            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                            .collect()
                    };

                    if let Ok(mut buf) = shared.output_buffer.lock() {
                        const MAX_BUFFER: usize = 48000;
                        if buf.len() + samples.len() > MAX_BUFFER {
                            let drop = buf.len() + samples.len() - MAX_BUFFER;
                            buf.drain(..drop);
                        }
                        buf.extend(samples);
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => {}
            }
        }

        println!("[AUDIO] Receive thread stopped");
    });

    Ok(())
}

// ============================================================================
// Utility Functions
// ============================================================================

fn calculate_level_i16(samples: &[i16]) -> f32 {
    if samples.is_empty() { return 0.0; }
    let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
    ((sum / samples.len() as f64).sqrt() / 32768.0) as f32
}

fn calculate_level_i32(samples: &[i32]) -> f32 {
    if samples.is_empty() { return 0.0; }
    let sum: f64 = samples.iter().map(|&s| ((s >> 16) as f64).powi(2)).sum();
    ((sum / samples.len() as f64).sqrt() / 32768.0) as f32
}

fn calculate_level_f32(samples: &[f32]) -> f32 {
    if samples.is_empty() { return 0.0; }
    let sum: f32 = samples.iter().map(|&s| s.powi(2)).sum();
    (sum / samples.len() as f32).sqrt()
}

// ============================================================================
// Device Enumeration
// ============================================================================

pub fn get_audio_devices() -> Result<Vec<AudioDeviceInfo>> {
    // ASIO 동시 접근 방지를 위한 락
    let _lock = DEVICE_ENUM_LOCK.lock().map_err(|e| anyhow!("Lock error: {}", e))?;

    println!("[AUDIO] get_audio_devices: starting enumeration");
    let mut devices = Vec::new();

    let hosts = cpal::available_hosts();
    println!("[AUDIO] Available hosts: {:?}", hosts.iter().map(|h| h.name()).collect::<Vec<_>>());

    for host_id in hosts {
        let host_name = host_id.name();
        let is_asio = host_name.to_lowercase().contains("asio");

        // ASIO: 열거하지 않고 플레이스홀더만 추가
        // ASIO 싱글톤 문제: 여기서 Host를 생성하면 이후 오디오 시작 시 Host가 장치를 못 찾음
        if is_asio {
            println!("[AUDIO] ASIO detected - using placeholder (singleton workaround)");
            devices.push(AudioDeviceInfo {
                id: "ASIO::Default".to_string(),
                name: "ASIO (자동 감지)".to_string(),
                driver: "ASIO".to_string(),
                is_default: true,
                is_input: true,
                sample_rates: vec![44100, 48000],
            });
            devices.push(AudioDeviceInfo {
                id: "ASIO::Default".to_string(),
                name: "ASIO (자동 감지)".to_string(),
                driver: "ASIO".to_string(),
                is_default: true,
                is_input: false,
                sample_rates: vec![44100, 48000],
            });
            continue;
        }

        println!("[AUDIO] Trying host: {}", host_name);
        let host = match cpal::host_from_id(host_id) {
            Ok(h) => h,
            Err(e) => {
                println!("[AUDIO] Failed to get host {}: {}", host_name, e);
                continue;
            }
        };

        let driver_type = if host_name.to_lowercase().contains("wasapi") {
            "WASAPI"
        } else {
            host_name
        };

        let default_input = host.default_input_device().and_then(|d| d.name().ok());
        let default_output = host.default_output_device().and_then(|d| d.name().ok());
        println!("[AUDIO] {} default input: {:?}, default output: {:?}", host_name, default_input, default_output);

        if let Ok(input_devices) = host.input_devices() {
            for device in input_devices {
                if let Ok(name) = device.name() {
                    devices.push(AudioDeviceInfo {
                        id: format!("{}::{}", driver_type, name),
                        name: name.clone(),
                        driver: driver_type.to_string(),
                        is_default: default_input.as_ref() == Some(&name),
                        is_input: true,
                        sample_rates: vec![44100, 48000],
                    });
                }
            }
        }

        if let Ok(output_devices) = host.output_devices() {
            for device in output_devices {
                if let Ok(name) = device.name() {
                    devices.push(AudioDeviceInfo {
                        id: format!("{}::{}", driver_type, name),
                        name: name.clone(),
                        driver: driver_type.to_string(),
                        is_default: default_output.as_ref() == Some(&name),
                        is_input: false,
                        sample_rates: vec![44100, 48000],
                    });
                }
            }
        }
    }

    Ok(devices)
}
