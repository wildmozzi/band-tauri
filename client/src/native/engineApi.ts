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
export type PeerPath = 'lan' | 'public' | 'relay'

/** 릴레이 등록 상태 (relay-design.md, P1-4b) */
export type RelayState = 'binding' | 'bound' | 'unreachable' | 'rejected' | 'expired'

export interface RelayStats {
  state: RelayState
  /** 해석된 릴레이 주소 ip:port */
  addr: string
  /** 이 엔진 ↔ 릴레이 왕복(ms) */
  rttMs: number | null
  /** 릴레이로 보내는 양(kbps, 최근 1초) */
  txKbps: number
  /** 릴레이 상한(2Mbps/2500pps)의 90% 초과: 릴레이가 패킷을 버리기 시작한다(PCM16 + 릴레이 피어 여럿) */
  overBudget: boolean
}

/** engine_set_relay 인자: 서버 relay-token 메시지의 addr/token/expiresIn */
export interface RelayConfig {
  /** 릴레이 host:port */
  addr: string
  /** base64url 토큰 (engine_start가 만든 ssrc에 묶임) */
  token: string
  /** 토큰 남은 수명(초, 서버 발급 시각 기준). 엔진은 받은 시각 + 이 값으로 만료를 판단한다(시계 오차 무관) */
  expiresInSec?: number
}

export interface PeerStats {
  peerId: string
  state: PeerState
  /** 'relay' = UDP 릴레이(서버 경유 — 지연 증가). 직접 경로가 열리면 엔진이 자동으로 승격한다 */
  path: PeerPath | null
  rttMs: number | null
  jitterMs: number
  bufferMs: number
  lossPct: number
  late: number
  concealed: number
  level: number
  /** 실제 재생 지연(ms): 벽시계로 잰 버퍼 지연(소스 링 포함) + 장치 출력 버퍼. bufferMs와 크게 벌어지면 숨은 지연 누적 */
  playoutMs?: number
  /** 이 피어가 나에게 에코 측정을 요청 중(동의를 물어야 함) */
  echoRequested?: boolean
  /** 내가 이 피어의 소리를 되돌려 보내는 중(동의함) */
  echoActive?: boolean
  /** 이 피어가 내 소리를 되돌려 보내는 중(내 에코 측정 진행 중) */
  peerEchoing?: boolean
}

// ============================================================================
// 지연 측정 (P1-1, 설계 문서 4절)
// ============================================================================

/** ok: 정상, noSignal: 신호 미검출, clipped: 입력 클리핑, unstable: 검출 부족·흩어짐 큼 */
export type MeasureVerdict = 'ok' | 'noSignal' | 'clipped' | 'unstable'

export interface LoopbackRun {
  roundTripMs: number | null
  peakRatio: number
  correlation: number
  clipped: boolean
  inverted: boolean
  inputPeak: number
}

/** `engine_measure_loopback` 결과(하드웨어 루프백 왕복) */
export interface LoopbackReport {
  verdict: MeasureVerdict
  message: string
  runs: LoopbackRun[]
  detectedRuns: number
  /** 실측 왕복(아날로그 출력 → 입력, 소프트웨어 패스스루 기준) 중앙값·최소·최대·표준편차 */
  roundTripMs: number | null
  minMs: number | null
  maxMs: number | null
  stdDevMs: number | null
  sampleRate: number
  inputBufferFrames: number
  outputBufferFrames: number
  /** 입력 버퍼 + 출력 버퍼 */
  bufferRoundTripMs: number
  /** 드라이버 보고(ASIO ASIOGetLatencies, 자체 WASAPI GetStreamLatency). 모르면 null */
  driverInputLatencyMs: number | null
  driverOutputLatencyMs: number | null
  driverRoundTripMs: number | null
  estLatencyMs: number
  /** 보정값 = 실측 − (입력 버퍼 + 출력 버퍼): estLatencyMs에 없는 숨은 지연 */
  offsetMs: number | null
  /** 실측 − 드라이버 보고 */
  driverErrorMs: number | null
  /** estLatencyMs + 보정값 */
  correctedEstLatencyMs: number | null
  backend: string
}

/** `engine_measure_echo_peer` 결과(네트워크 포함 왕복) */
export interface EchoReport {
  verdict: MeasureVerdict
  message: string
  peerId: string
  runs: (number | null)[]
  detectedRuns: number
  /** 프레임 + 망 왕복 + 상대 되돌리기 + 내 지터 버퍼 */
  roundTripMs: number | null
  minMs: number | null
  maxMs: number | null
  stdDevMs: number | null
  /** 상대가 보고한 로컬 처리 지연(되돌리기) */
  peerEchoDelayMs: number
  /** (왕복 − 상대 처리) / 2 */
  oneWayMs: number | null
  networkRttMs: number | null
  jitterBufferMs: number
  frameMs: number
  localLatencyMs: number
  inputBufferMs: number
  outputBufferMs: number
  /** 입→귀 추정 = 왕복 − 상대 처리 − 핑/2 + 입력 버퍼 + 출력 버퍼 */
  mouthToEarMs: number | null
  /** 왕복 − (프레임 + 핑 + 상대 처리 + 지터 버퍼): 0에 가까우면 통계가 맞음 */
  residualMs: number | null
  path: PeerPath | null
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
  /** 스트림이 복구되지 않는 오류로 멈춤(치명 오류, 재시작 반복 실패). 장치는 남아 있을 수 있다. true면 running=false */
  streamFailed?: boolean
  /** 일시 오류 반복으로 자체 WASAPI 스트림을 재시작한 횟수 */
  streamRestarts?: number
  /** 캡처 DATA_DISCONTINUITY 횟수(입력 오버런, xruns에도 포함). 자체 WASAPI만 센다 */
  inputDiscontinuities?: number
  /** 장치가 SILENT로 표시한 캡처 패킷 수(자체 WASAPI) */
  inputSilentPackets?: number
  /** 실제 출력 장치 주기(ms). 자체 WASAPI는 장치에 설정된 주기, cpal은 실측 콜백 크기 */
  devicePeriodMs?: number
  /** 실제로 열린 오디오 백엔드(`AudioBackendKind`, 입·출력이 다르면 `입력/출력`). 정지 상태면 빈 문자열 */
  backend?: string
  peers: PeerStats[]
  /** 릴레이 등록 상태. 릴레이를 설정하지 않았으면 null/undefined */
  relay?: RelayStats | null
  /** 공유 메트로놈 상태(P2-3). 옛 엔진이면 없음 */
  metronome?: MetronomeStats
}

/**
 * 공유 메트로놈 계획(P2-3, 설계 1.4절). 방장은 refPeerId 없이 부르고(startUs를 비우면 지금 + 300ms),
 * 다른 연주자는 방장 엔진 peerId와 방장이 알린 startUs로 부른다. startUs는 기준 엔진 시계(µs).
 */
export interface MetronomeConfig {
  running: boolean
  /** 20..300 */
  bpm: number
  /** 1..16 */
  beatsPerBar: number
  startUs?: number | null
  refPeerId?: string | null
}

/** `engine_set_metronome`이 적용한 계획. 방장은 이 값을 방에 알린다. */
export interface MetronomePlan {
  running: boolean
  bpm: number
  beatsPerBar: number
  startUs: number
  refPeerId: string | null
}

export interface MetronomeStats extends MetronomePlan {
  /** 시간축이 잡힘(방장이거나 기준 피어 시계 표본 있음). false면 클릭을 내지 않는다 */
  synced: boolean
  /** 기준 피어 시계 − 내 시계(ms). 방장이면 0 */
  clockOffsetMs: number | null
  clockRttMs: number | null
  driftPpm: number | null
  /** 마지막으로 예약한 박 번호(0부터) */
  beat: number | null
  clicks: number
  lateClicks: number
  /** 콜백 공백 뒤 제 시각이 지나 건너뛴 클릭 수 */
  skippedClicks: number
  /** 클릭 시각에 반영한 숨은 출력 지연(ms). null = 드라이버가 알려 주지 않아 보정 안 됨 */
  outputLatencyMs: number | null
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

/**
 * 릴레이 설정/토큰 갱신(서버 relay-token을 받을 때마다), null이면 해제.
 * 토큰은 engine_start가 만든 ssrc에 묶이므로 시작 인자가 아니라 별도 커맨드다(설계 4절).
 */
export function engineSetRelay(relay: RelayConfig | null): Promise<null> {
  return invoke<null>('engine_set_relay', { relay })
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

/** 공유 메트로놈 계획 적용(P2-3). 적용된 계획을 돌려준다 */
export function engineSetMetronome(metronome: MetronomeConfig): Promise<MetronomePlan> {
  return invoke<MetronomePlan>('engine_set_metronome', { metronome })
}

/** 메트로놈 클릭 볼륨(0..4)·뮤트. 로컬 전용(내 귀에만), 엔진 재시작 간 유지 */
export function engineSetMetronomeGain(gain: number, muted: boolean): Promise<null> {
  return invoke<null>('engine_set_metronome_gain', { gain, muted })
}

export function engineStats(): Promise<EngineStats> {
  return invoke<EngineStats>('engine_stats')
}

/**
 * 믹스 구독. 엔진이 20ms마다 48kHz stereo f32 인터리브 청크를 보낸다.
 * Rust가 raw 바이트로 보내면 ArrayBuffer, JSON 배열로 보내면 number[]가 오므로 둘 다 Float32Array로 맞춘다.
 */
/**
 * 하드웨어 루프백 지연 측정. 엔진이 실행 중이어야 한다(보통 테스트 모드). 측정 동안 출력은 처프만 낸다.
 * 반복당 약 0.7초 걸린다.
 */
export function engineMeasureLoopback(runs?: number): Promise<LoopbackReport> {
  return invoke<LoopbackReport>('engine_measure_loopback', runs === undefined ? {} : { runs })
}

/** 에코 모드 동의/해제(에코하는 쪽). 그 피어가 요청 중일 때만 켤 수 있다 */
export function engineSetEchoMode(peerId: string, enabled: boolean): Promise<null> {
  return invoke<null>('engine_set_echo_mode', { peerId, enabled })
}

/** 에코 피어 측정(측정하는 쪽). 상대 동의를 waitMs(기본 30초)까지 기다린다 */
export function engineMeasureEchoPeer(peerId: string, runs?: number, waitMs?: number): Promise<EchoReport> {
  const args: Record<string, unknown> = { peerId }
  if (runs !== undefined) args.runs = runs
  if (waitMs !== undefined) args.waitMs = waitMs
  return invoke<EchoReport>('engine_measure_echo_peer', args)
}

/** 진행 중인 측정 취소 */
export function engineCancelMeasure(): Promise<null> {
  return invoke<null>('engine_cancel_measure')
}

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
