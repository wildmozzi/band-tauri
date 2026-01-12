/**
 * Tauri Integration - SYNCROOM-style Audio
 */

declare global {
  interface Window {
    __TAURI__?: {
      core: {
        invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>
      }
    }
  }
}

// ============================================================================
// Types
// ============================================================================

/** 오디오 드라이버 타입 */
export type AudioDriver = 'Asio' | 'WasapiExclusive' | 'WasapiShared'

/** 압축 방식 */
export type CompressionType = 'Uncompressed' | 'Opus'

/** 채널 */
export type ChannelType = 'Mono' | 'Stereo'

/** 모노 입력 채널 선택 */
export type MonoChannelType = 'Ch1' | 'Ch2'

/** 오디오 설정 (새 API) */
export interface AudioSettings {
  driver: AudioDriver
  input_driver?: AudioDriver | null  // 입력 전용 드라이버 (null이면 driver 사용)
  output_driver?: AudioDriver | null // 출력 전용 드라이버 (null이면 driver 사용)
  input_device?: string | null
  output_device?: string | null
  sample_rate: number        // 44100, 48000
  buffer_size: number        // 64, 128, 256, 512, 1024
  compression: CompressionType
  channels: ChannelType
  mono_channel?: MonoChannelType  // 모노일 때 사용할 입력 채널 (Ch1/Ch2)
  stereo_swap?: boolean           // 스테레오일 때 L/R 스왑
}

/** 오디오 통계 */
export interface AudioStats {
  is_running: boolean
  driver: string
  sample_rate: number
  buffer_size: number
  compression: string
  input_level: number   // 0.0 ~ 1.0
  output_level: number  // 0.0 ~ 1.0
}

/** 오디오 장치 정보 */
export interface AudioDeviceInfo {
  id: string           // "ASIO::DeviceName" or "WASAPI::DeviceName"
  name: string
  driver: string       // "ASIO", "WASAPI"
  is_default: boolean
  is_input: boolean
  sample_rates: number[]
}

/** 네트워크 정보 */
export interface NetworkInfo {
  local_addr: string
  lan_addr: string | null
  public_addr: string | null
  is_running: boolean
  connected_peers: string[]
}

/** 피어 정보 */
export interface PeerInfo {
  id: string
  addr: string
  rtt_ms: number | null
  is_connected: boolean
}

/** 피어 레이턴시 (PeerInfo 별칭) */
export type PeerLatency = PeerInfo

// ============================================================================
// Utility
// ============================================================================

export function isTauri(): boolean {
  return typeof window !== 'undefined' && window.__TAURI__ !== undefined
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) {
    throw new Error('Not running in Tauri environment')
  }
  return window.__TAURI__!.core.invoke<T>(cmd, args)
}

export function supportsNativeAudio(): boolean {
  return isTauri()
}

export function getAudioMode(): 'native' | 'web' {
  return isTauri() ? 'native' : 'web'
}

// ============================================================================
// Network API
// ============================================================================

/**
 * 네트워크 초기화
 */
export async function initNetwork(myId: string): Promise<NetworkInfo> {
  return invoke<NetworkInfo>('init_network', { myId })
}

/**
 * 네트워크 정보 조회
 */
export async function getNetworkInfo(): Promise<NetworkInfo> {
  return invoke<NetworkInfo>('get_network_info')
}

/**
 * 피어 연결
 */
export async function connectToPeer(peerId: string, addr: string): Promise<string> {
  return invoke<string>('connect_to_peer', { peerId, addr })
}

/**
 * 피어 연결 해제
 */
export async function disconnectPeer(peerId: string): Promise<string> {
  return invoke<string>('disconnect_peer', { peerId })
}

/**
 * 피어 레이턴시 조회
 */
export async function getPeerLatencies(): Promise<PeerInfo[]> {
  return invoke<PeerInfo[]>('get_peer_latencies')
}

/**
 * 네트워크 종료
 */
export async function shutdownNetwork(): Promise<string> {
  return invoke<string>('shutdown_network')
}

// ============================================================================
// Audio API
// ============================================================================

/**
 * 오디오 시작
 */
export async function startAudio(settings: AudioSettings, peerAddr?: string | null): Promise<string> {
  return invoke<string>('start_audio', { settings, peerAddr })
}

/**
 * 오디오 중지
 */
export async function stopAudio(): Promise<string> {
  return invoke<string>('stop_audio')
}

/**
 * 피어 주소 설정
 */
export async function setAudioPeer(addr: string): Promise<string> {
  return invoke<string>('set_audio_peer', { addr })
}

/**
 * 오디오 통계 조회
 */
export async function getAudioStats(): Promise<AudioStats> {
  return invoke<AudioStats>('get_audio_stats')
}

/**
 * 오디오 장치 목록 조회
 */
export async function getAudioDevices(): Promise<AudioDeviceInfo[]> {
  return invoke<AudioDeviceInfo[]>('get_audio_devices')
}

/**
 * 입력 테스트 오디오 시작 (네트워크 없이 입력 모니터링만)
 */
export async function startTestAudio(settings: AudioSettings): Promise<string> {
  return invoke<string>('start_test_audio', { settings })
}

/**
 * 입력 테스트 오디오 중지
 */
export async function stopTestAudio(): Promise<string> {
  return invoke<string>('stop_test_audio')
}

// ============================================================================
// Legacy API (backward compatibility)
// ============================================================================

export async function startNativeAudio(): Promise<string> {
  return invoke<string>('start_native_audio')
}

export async function stopNativeAudio(): Promise<string> {
  return invoke<string>('stop_native_audio')
}

export async function startP2PNetwork(myId: string, signalingUrl: string): Promise<NetworkInfo> {
  return invoke<NetworkInfo>('start_p2p_network', { myId, signalingUrl })
}

export async function stopP2PNetwork(): Promise<string> {
  return invoke<string>('stop_p2p_network')
}

// Legacy types for compatibility
export interface LegacyAudioSettings {
  sample_rate: number
  buffer_size: number
  opus_bitrate: number
  monitor_buffer_ms?: number
  jitter_buffer_ms?: number
  host_id?: string | null
  input_device_id?: string | null
  output_device_id?: string | null
}

// Alias for backward compatibility with AudioSettingsContext
export type { LegacyAudioSettings as AudioSettings_Legacy }

/** 레거시 호스트 정보 (AudioSettingsContext 호환) */
export interface AudioHostInfo {
  id: string
  name: string
}

/** 레거시 장치 목록 (AudioSettingsContext 호환) */
export interface AudioDeviceList {
  hosts: AudioHostInfo[]
  input_devices: AudioDeviceInfo[]
  output_devices: AudioDeviceInfo[]
}

export async function setAudioSettings(settings: LegacyAudioSettings): Promise<string> {
  return invoke<string>('set_audio_settings', { settings })
}

export async function setMonitoring(enabled: boolean): Promise<string> {
  return invoke<string>('set_monitoring', { enabled })
}

export async function getMonitoring(): Promise<boolean> {
  return invoke<boolean>('get_monitoring')
}

export async function setMonitoringVolume(volume: number): Promise<string> {
  return invoke<string>('set_monitoring_volume', { volume: Math.round(volume) })
}

export async function getMonitoringVolume(): Promise<number> {
  return invoke<number>('get_monitoring_volume')
}

// ============================================================================
// Default Settings
// ============================================================================

export const DEFAULT_AUDIO_SETTINGS: AudioSettings = {
  driver: 'WasapiShared',
  input_driver: null,
  output_driver: null,
  input_device: null,
  output_device: null,
  sample_rate: 48000,
  buffer_size: 256,
  compression: 'Uncompressed',
  channels: 'Mono',
  mono_channel: 'Ch1',
  stereo_swap: false,
}

/** 음질 프리셋 */
export const AUDIO_PRESETS = {
  'high-quality': {
    sample_rate: 48000,
    buffer_size: 256,
    compression: 'Uncompressed' as CompressionType,
    channels: 'Stereo' as ChannelType,
  },
  'standard': {
    sample_rate: 48000,
    buffer_size: 256,
    compression: 'Opus' as CompressionType,
    channels: 'Mono' as ChannelType,
  },
  'low-latency': {
    sample_rate: 48000,
    buffer_size: 64,
    compression: 'Uncompressed' as CompressionType,
    channels: 'Mono' as ChannelType,
  },
  'bandwidth-priority': {
    sample_rate: 44100,
    buffer_size: 512,
    compression: 'Opus' as CompressionType,
    channels: 'Mono' as ChannelType,
  },
}
