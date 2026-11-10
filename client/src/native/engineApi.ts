/**
 * 네이티브 오디오 엔진 Tauri 커맨드 래퍼.
 * 계약: docs/overhaul/native-engine-design.md 4절. 타입/커맨드 이름을 바꾸려면 설계 문서부터 바꾼다.
 *
 * 웹(비Tauri) 빌드에서도 이 파일은 import만으로 에러가 나지 않는다.
 * `@tauri-apps/api/core`는 import 시점에 Tauri 전역을 건드리지 않고,
 * 모든 커맨드는 `isNativeApp()` 가드 뒤에서만 invoke 한다.
 */
import { Channel, invoke as tauriInvoke, isTauri } from '@tauri-apps/api/core'

// ============================================================================
// 타입 (설계 4절)
// ============================================================================

export type DriverKind = 'asio' | 'wasapiShared' | 'wasapiExclusive'

export interface DeviceInfo {
  id: string
  name: string
  driver: DriverKind
  isInput: boolean
  isDefault: boolean
  channels: number
  sampleRates: number[]
}

export type FrameMs = 2.5 | 5 | 10
export type CodecKind = 'opus' | 'pcm16'

export interface EngineConfig {
  inputDriver: DriverKind
  outputDriver: DriverKind
  /** DeviceInfo.id, null이면 기본 장치 */
  inputDevice: string | null
  outputDevice: string | null
  /** 32..1024, 장치 요청값 */
  bufferSize: number
  frameMs: FrameMs
  codec: CodecKind
  channels: 1 | 2
  /** 0-based 장치 입력 채널 */
  inputChannels: [number] | [number, number]
  opusBitrate?: number
  jitterMinMs?: number
  jitterMaxMs?: number
  udpPort?: number
}

export interface Candidate {
  kind: 'lan' | 'public'
  addr: string
}

export interface LocalDescriptor {
  ssrc: number
  key: string
  candidates: Candidate[]
}

export type PeerState = 'connecting' | 'connected' | 'reconnecting' | 'failed'

export interface PeerStats {
  peerId: string
  state: PeerState
  path: 'lan' | 'public' | null
  rttMs: number | null
  jitterMs: number
  bufferMs: number
  lossPct: number
  late: number
  concealed: number
  level: number
  /** 실제 재생 지연(ms): 벽시계로 잰 버퍼 지연(소스 링 포함) + 장치 출력 버퍼. bufferMs와 크게 벌어지면 숨은 지연 누적 */
  playoutMs?: number
}

/**
 * 실제로 열린 오디오 백엔드. 입·출력이 다르면 `입력/출력`(예: `wasapiExclusive/wasapiShared3`).
 * - `wasapiExclusive`: 자체 WASAPI 독점(이벤트 구동, 장치 최소 주기)
 * - `wasapiShared3`: 자체 WASAPI IAudioClient3 저지연 공유
 * - `cpalWasapi`: cpal 공유 모드(≈10ms, 자체 백엔드를 못 열었을 때의 폴백)
 * - `cpalAsio`, `fake`(테스트)
 */
export type AudioBackendKind = 'wasapiExclusive' | 'wasapiShared3' | 'cpalWasapi' | 'cpalAsio' | 'fake'

export interface EngineStats {
  /** 오디오 장치를 잃으면(콜백 정지) false */
  running: boolean
  sampleRate: number
  deviceBuffer: number
  inputLevel: number
  outputLevel: number
  xruns: number
  /** 로컬 처리 지연(GOAL 1절 정의: 입력 버퍼+프레임+출력 링+출력 버퍼). 콜백 구동 믹스라 출력 링은 0 */
  estLatencyMs: number
  /** 모니터 경로(내 입력 → 내 출력) 지연 */
  monitorLatencyMs?: number
  /** 오디오 스트림 에러 누적(비치명 포함) */
  streamErrors?: number
  /** 장치를 잃음(콜백 1초 정지, 장치 분리·무효화, 기본 장치 변경). true면 running=false */
  deviceLost?: boolean
  /** 실제 출력 장치 주기(ms). 자체 WASAPI는 장치에 설정된 주기, cpal은 실측 콜백 크기 */
  devicePeriodMs?: number
  /** 실제로 열린 오디오 백엔드(`AudioBackendKind`, 입·출력이 다르면 `입력/출력`). 정지 상태면 빈 문자열 */
  backend?: string
  peers: PeerStats[]
}

// ============================================================================
// 환경 감지
// ============================================================================

/** Tauri 데스크톱 앱에서 실행 중인지 (연주는 앱에서만 가능) */
export function isNativeApp(): boolean {
  if (typeof window === 'undefined') return false
  try {
    return isTauri()
  } catch {
    return false
  }
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isNativeApp()) {
    throw new Error(`네이티브 엔진은 데스크톱 앱에서만 사용할 수 있습니다 (${cmd})`)
  }
  return tauriInvoke<T>(cmd, args)
}

/** invoke 에러(Rust `Result<T, String>`의 Err)를 사람이 읽을 문자열로 */
export function engineErrorMessage(err: unknown): string {
  if (typeof err === 'string') return err
  if (err instanceof Error) return err.message
  try {
    return JSON.stringify(err)
  } catch {
    return String(err)
  }
}

// ============================================================================
// 커맨드 (설계 4절 표)
// ============================================================================

export function engineListDevices(): Promise<DeviceInfo[]> {
  return invoke<DeviceInfo[]>('engine_list_devices')
}

export function engineStart(config: EngineConfig, peerId: string): Promise<LocalDescriptor> {
  return invoke<LocalDescriptor>('engine_start', { config, peerId })
}

export function engineStop(): Promise<null> {
  return invoke<null>('engine_stop')
}

export function engineTestStart(config: EngineConfig): Promise<null> {
  return invoke<null>('engine_test_start', { config })
}

export function engineTestStop(): Promise<null> {
  return invoke<null>('engine_test_stop')
}

export function engineAddPeer(peerId: string, descriptor: LocalDescriptor): Promise<null> {
  return invoke<null>('engine_add_peer', { peerId, descriptor })
}

export function engineRemovePeer(peerId: string): Promise<null> {
  return invoke<null>('engine_remove_peer', { peerId })
}

/** pan: -1(왼쪽)..1(오른쪽), equal-power. 생략하면 엔진 기본(0) */
export function engineSetPeerGain(peerId: string, gain: number, muted: boolean, pan?: number): Promise<null> {
  return invoke<null>('engine_set_peer_gain', pan === undefined ? { peerId, gain, muted } : { peerId, gain, muted, pan })
}

/** 내 입력을 송신/모니터/믹스 구독에서 무음 처리 (레벨 미터는 계속 표시) */
export function engineSetInputMuted(muted: boolean): Promise<null> {
  return invoke<null>('engine_set_input_muted', { muted })
}

/**
 * 피어와의 안전 번호("12345 67890 ..."). 두 연주자가 같은 숫자를 보면 시그널링 서버가
 * 암호 키를 바꿔치기하지 않은 것이다(키 확인용, 표시 UI는 별도 작업).
 */
export function engineSafetyCode(peerId: string): Promise<string> {
  return invoke<string>('engine_safety_code', { peerId })
}

export function engineSetMonitor(enabled: boolean, gain: number): Promise<null> {
  return invoke<null>('engine_set_monitor', { enabled, gain })
}

export function engineSetMasterGain(gain: number): Promise<null> {
  return invoke<null>('engine_set_master_gain', { gain })
}

export function engineStats(): Promise<EngineStats> {
  return invoke<EngineStats>('engine_stats')
}

/**
 * 믹스 구독. 엔진이 20ms마다 48kHz stereo f32 인터리브 청크를 보낸다.
 * Rust가 raw 바이트로 보내면 ArrayBuffer, JSON 배열로 보내면 number[]가 오므로 둘 다 Float32Array로 맞춘다.
 */
export async function engineSubscribeMix(onChunk: (samples: Float32Array) => void): Promise<void> {
  const channel = new Channel<ArrayBuffer | number[]>()
  channel.onmessage = (data) => {
    const samples = toFloat32(data)
    if (samples) onChunk(samples)
  }
  await invoke<null>('engine_subscribe_mix', { onChunk: channel })
}

export function engineUnsubscribeMix(): Promise<null> {
  return invoke<null>('engine_unsubscribe_mix')
}

function toFloat32(data: unknown): Float32Array | null {
  if (data instanceof ArrayBuffer) {
    return data.byteLength % 4 === 0 ? new Float32Array(data) : null
  }
  if (ArrayBuffer.isView(data)) {
    const view = data as ArrayBufferView
    if (view.byteLength % 4 !== 0) return null
    // 정렬되지 않은 뷰일 수 있으므로 복사
    return new Float32Array(view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength))
  }
  if (Array.isArray(data)) {
    // Vec<u8> 직렬화(바이트 배열)
    const bytes = Uint8Array.from(data as number[])
    return bytes.byteLength % 4 === 0 ? new Float32Array(bytes.buffer) : null
  }
  return null
}

// ============================================================================
// 상수
// ============================================================================

export const ENGINE_SAMPLE_RATE = 48000
export const MIX_CHUNK_MS = 20
export const MAX_PERFORMERS = 6
/** 패킷 오디오 페이로드 상한 (설계 1.3) */
export const MAX_PAYLOAD_BYTES = 1200
