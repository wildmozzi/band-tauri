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
/** PCM16 설정에서 피어별 Opus 자동 전환 이유: 릴레이 대역 부하(P1-4c) / 상대가 보고한 수신 손실(P1-4e) */
export type TxCodecReason = 'relayLoad' | 'lossFeedback'

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
  /** 지터 버퍼 '저지연(0) ↔ 안정(1)' 프로필, 기본 0.5(균형). 실행 중 변경은 engineSetJitterProfile */
  jitterProfile?: number
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
  /** 코덱이 PCM16인데 릴레이 대역(상한의 90%)에 닿아 릴레이 경로 피어에게 Opus로 자동 전환해 보내는 중(P1-4c) */
  autoOpus?: boolean
}

/**
 * 오디오 UDP DSCP 마킹 상태(P1-3, 설계 3절 "QoS 마킹").
 * - 'ef': EF(46)를 OS에 지정함(비Windows IP_TOS, Windows는 걸린 플로 모두 qWAVE QOSSetFlow 성공 — 관리자 권한 필요).
 *   플로 추가에 실패한 목적지가 섞여 있으면 `failed`로 따로 알린다
 * - 'voice': Windows, 걸린 플로 중 DSCP 직접 지정에 실패한 것이 있음(보통 권한 부족 — 그 플로는 트래픽 유형 Voice, 표시값은 OS 정책)
 * - 'pending': 아직 송신 목적지 없음 / 'none': 표시 못 함(엔진은 그대로 동작)
 */
export type QosMarking = 'pending' | 'ef' | 'voice' | 'none'

export interface QosStats {
  marking: QosMarking
  /** IP_TOS=0xB8 setsockopt 성공(Windows는 성공해도 무시될 수 있다) */
  ipTos: boolean
  /** qWAVE 핸들을 얻음(Windows만) */
  qwave: boolean
  /** qWAVE 플로가 걸린 송신 목적지 수(직접 경로 피어 + 릴레이) */
  flows: number
  /** 그중 DSCP 46 지정에 성공한 수 */
  efFlows: number
  /** 플로 추가 실패 목적지 수(예: 루프백 목적지는 qWAVE가 거부) */
  failed: number
  /** 지금 남은 실패 사유(소켓·핸들 실패 또는 현재 목적지의 실패). 실패한 목적지가 빠지면 지워진다 */
  lastError: string | null
}

/**
 * 송신 큐 지연: 워커 패스 시작(입력 링을 비우기 직전) → 피어별 send_to 완료, 직전 2초 창(P1-3).
 * 같은 패스에서 나온 프레임은 앞 프레임의 인코딩·송신을 기다린 시간까지 포함한다(장치 버퍼 > 프레임일 때)
 */
export interface SendQueueStats {
  p50Ms: number
  p99Ms: number
  maxMs: number
  /** 창에서 잰 오디오 패킷 수(0이면 송신 없음 — 값 무시) */
  packets: number
}

/**
 * CPU 통계(P1-10), 직전 2초 창. %는 코어 1개 = 100.
 * 콜백 실행시간은 출력 콜백(믹스) 1회 기준이고, 사용률은 입력+출력 콜백 실행시간 합 / 창 길이다.
 */
export interface CpuStats {
  /** 워커 스레드 CPU(%, 스레드 사이클 / TSC). 앱 전체 CPU 아님. Windows x64가 아니거나 첫 창 전이면 null */
  workerPct: number | null
  callbackLoadPct: number
  callbackP50Us: number
  callbackP99Us: number
  callbackMaxUs: number
  /** 출력 콜백 p99 / 장치 출력 주기(%). 100에 가까우면 마감을 놓친다 */
  callbackP99PeriodPct: number
  inputCallbackMaxUs: number
  /** 창 안 출력 콜백 수(0이면 콜백 값 무시) */
  callbacks: number
  /** 창 길이(ms). 0이면 아직 첫 창 전 */
  windowMs: number
}

/** engine_set_relay 인자: 서버 relay-token 메시지의 addr/token/expiresIn */
export interface RelayConfig {
  /** 릴레이 host:port */
  addr: string
  /** base64url 토큰 (engine_start가 만든 ssrc에 묶임) */
  token: string
  /** 토큰 남은 수명(초, 서버 발급 시각 기준). 엔진은 받은 시각 + 이 값으로 만료를 판단한다(시계 오차 무관) */
  expiresInSec?: number
  /** 토큰별 Bind MAC 키(base64url 32B, relay-design 2.1.1). 있으면 RelayBind에 MAC을 붙인다 — 경로상 등록 가로채기 방지 */
  bindKey?: string
}

export interface PeerStats {
  peerId: string
  state: PeerState
  /** 'relay' = UDP 릴레이(서버 경유 — 지연 증가). 직접 경로가 열리면 엔진이 자동으로 승격한다 */
  path: PeerPath | null
  rttMs: number | null
  jitterMs: number
  bufferMs: number
  /** 지터 버퍼 목표 깊이(ms): 지터 추정 + 은닉 마감 여유 + 언더런 여유 + 프로필 여유 */
  targetMs: number
  /** 최근 5초 은닉 비율(%, 손실 + 늦은 패킷 + 언더런) */
  concealPct: number
  /** 최근 5초 중 깊이 보정(샘플 버림·삽입) 모드였던 패킷 비율(%) */
  corrTimePct: number
  /** 깊이 보정의 최대 국소 피치 편차(센트, 재생 시작 이후). 상한 약 4.3센트 */
  maxCorrCents: number
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
  /** 이 피어에게 마지막으로 보낸 내 소리의 코덱(아직 안 보냈으면 null) */
  txCodec?: CodecKind | null
  /** txCodec이 PCM16 설정에서 자동 전환한 Opus다(이유는 txCodecReason) */
  txCodecAuto?: boolean
  /** 자동 전환 이유(txCodecAuto일 때). 옛 엔진 빌드에는 없다 */
  txCodecReason?: TxCodecReason | null
  /** 이 피어가 알려 온 내 소리의 수신 손실률·은닉률(%, 그쪽 최근 5초). 보고가 3초 넘게 없으면 null(옛 엔진, 그쪽 재생 전) */
  remoteLossPct?: number | null
  remoteConcealPct?: number | null
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
  /** 스레드 우선순위 설정 결과(MMCSS 등록 여부, 1ms 타이머) */
  threadPriority?: ThreadPriorityStats
  peers: PeerStats[]
  /** 릴레이 등록 상태. 릴레이를 설정하지 않았으면 null/undefined */
  relay?: RelayStats | null
  /** 오디오 UDP DSCP 마킹 상태. 네트워크 미시작이면 null */
  qos?: QosStats | null
  /** 송신 큐 지연(직전 2초 창). 네트워크 미시작이면 null */
  sendQueue?: SendQueueStats | null
  /** 공유 메트로놈 상태(P2-3). 옛 엔진이면 없음 */
  metronome?: MetronomeStats
  /** 워커 CPU·RT 콜백 실행시간(P1-10). 옛 엔진이면 없음 */
  cpu?: CpuStats
  /** 합주 녹음 상태(P2-4). 녹음 중이 아니면 null, 옛 엔진이면 없음 */
  recording?: RecordStats | null
}

/** 녹음 파일 형식: WAV 24비트 정수 PCM(기본) | WAV 32비트 float | FLAC 24비트 무손실(P2-4a, `.flac`) */
export type RecordFormat = 'wav24' | 'wavF32' | 'flac'

/** `engine_record_start` 옵션(설계 1.5절). 모두 선택 */
export interface RecordOptions {
  format?: RecordFormat
  /** 믹스 트랙(내가 들은 피어 게인·팬 + 내 입력, 클릭 없음)도 저장 */
  includeMix?: boolean
  /** 세션 폴더 이름(파일 이름에 못 쓰는 문자는 `_`). 없으면 `band-rec-<UTC 시각>` */
  name?: string
  /** 내 입력 트랙 이름(기본 "me") */
  localName?: string
  /** 피어별 트랙 이름(engine peerId → 표시 이름). 없으면 peerId 가명(id-xxxxxxxx) */
  trackNames?: Record<string, string>
}

export interface RecordStats {
  /** 세션 폴더(절대 경로) */
  folder: string
  format: RecordFormat
  /** 파일 샘플레이트 = 출력 장치 레이트 */
  sampleRate: number
  durationSec: number
  /** 지금까지 쓴 오디오 바이트(모든 트랙) */
  bytes: number
  tracks: number
  /** 녹음 링이 모자라 버린 프레임(그 자리는 무음, 녹음 품질 지표) */
  droppedFrames: number
  /** 쓰기 실패(디스크 가득 등). 있으면 그 뒤로는 저장되지 않는다 */
  error: string | null
}

export interface RecordTrack {
  kind: 'local' | 'peer' | 'mix'
  name: string
  /** 세션 폴더 안 파일 이름 */
  file: string
  /** 피어 id 가명(id-xxxxxxxx, 진단 내보내기와 같은 값 — 원문 아님). 내 입력·믹스는 null */
  peerId: string | null
}

/** `engine_record_stop` 반환(세션 폴더 `session.json`과 같은 내용) */
export interface RecordSummary {
  folder: string
  format: RecordFormat
  sampleRate: number
  channels: number
  durationSec: number
  bytes: number
  droppedFrames: number
  startedAtMs: number
  tracks: RecordTrack[]
  error: string | null
  /** 은닉·버림·언더런 구간 수(모든 트랙 합, P2-4a). 옛 엔진이면 없음 */
  markers?: number
  /** 마커 사이드카 파일 이름(세션 폴더 안, `markers.json`). 쓰지 못했으면 null, 옛 엔진이면 없음 */
  markersFile?: string | null
}

/** `markers.json`의 구간 하나(P2-4a). 위치는 녹음 시작 기준 프레임 = 트랙 파일 샘플 위치 */
export interface RecordMarker {
  kind: 'conceal' | 'drop' | 'underrun'
  start: number
  /** 끝(제외). `drop`은 길이 없는 지점이라 start와 같다 */
  end: number
  startSec: number
  endSec: number
  /** 은닉·0 채움·버린 오디오 양(프레임) */
  frames: number
  /** 합친 이벤트 수 */
  events: number
}

/** `markers.json`(P2-4a) */
export interface RecordMarkersFile {
  sampleRate: number
  mergeGapFrames: number
  accuracyFrames: number
  lostEvents: number
  /** 트랙·종류별 구간 상한(1만)을 넘어 세기만 한 이벤트 수. 옛 엔진에는 없다. */
  overflowEvents?: number
  tracks:{ file: string; name: string; kind: RecordTrack['kind']; peerId: string | null; markers: RecordMarker[] }[]
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
  /** 클릭 시각에 반영한 숨은 출력 지연(ms). null = 드라이버 보고도 루프백 보정값도 없어 보정 안 됨(루프백 측정 안내) */
  outputLatencyMs: number | null
  /** `outputLatencyMs`의 출처: 드라이버 보고 / 루프백 측정 보정값. null = 보정 안 됨 */
  outputLatencySource: MetronomeLatencySource | null
  /** 기준 피어(방장)가 시계를 싣지 않는 옛 엔진이라 동기화할 수 없음(이 계획으로는 synced가 계속 false) */
  refClockUnsupported: boolean
  /** 시계 추정 직선 무게중심에서 지금까지의 외삽 거리(ms, 진단용) */
  clockExtrapolationMs: number | null
}

/** 메트로놈 숨은 출력 지연 보정의 출처 */
export type MetronomeLatencySource = 'driver' | 'loopback'

/** 엔진 스레드 우선순위 설정 결과(설계 문서 4절 `threadPriority`) */
export type ThreadPriority = 'unmanaged' | 'mmcss' | 'elevated' | 'failed' | 'disabled' | 'unsupported'

export interface ThreadPriorityStats {
  worker: ThreadPriority
  netRecv: ThreadPriority
  /** 오디오 I/O 스레드. 자체 WASAPI만 등록하고 cpal·ASIO는 unmanaged(백엔드가 관리) */
  audio: ThreadPriority
  timer1ms: boolean
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

/** 지터 버퍼 프로필(0 저지연 ↔ 1 안정)을 실행 중에 바꾼다. 다음 시작에는 EngineConfig.jitterProfile을 쓴다 */
export function engineSetJitterProfile(profile: number): Promise<null> {
  return invoke<null>('engine_set_jitter_profile', { profile })
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

/**
 * 메트로놈 숨은 출력 지연 보정용 루프백 보정값(`LoopbackReport.offsetMs`, -50..200). null이면 해제.
 * 루프백 측정이 정상 판정이면 엔진이 자동으로 기억해 같은 장치 구성의 합주 엔진에도 쓴다.
 * 앱 재시작 뒤 저장해 둔 값을 다시 넣을 때 부른다. 드라이버가 출력 지연을 보고하는 장치에서는 드라이버 값이 우선이다.
 * - 엔진이 실행 중일 때(`engine_test_start`/`engine_start` 뒤)만 부를 수 있다. 엔진이 없으면 오류다.
 * - 엔진은 기억 슬롯 1개(마지막 장치 구성 + 값)만 둔다. 장치 구성 키 = 드라이버·실제 백엔드·실제로 연 장치(기본 장치면
 *   그때 고른 장치)·버퍼 크기·레이트. 앱이 저장할 때도 이 구성(장치·버퍼·레이트)별로 저장해야 한다.
 * - null은 키와 무관하게 슬롯 전체를 지운다.
 * - 다시 재서 실패(`unstable` 등)하거나 범위 밖이면 이전 값이 그대로 남는다.
 */
export function engineSetLoopbackOffset(offsetMs: number | null): Promise<null> {
  return invoke<null>('engine_set_loopback_offset', { offsetMs })
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

/**
 * 진단 내보내기(P2-5): 저장 대화상자를 열고 진단 zip(엔진 통계 타임라인·설정·장치·앱/OS 정보·마스킹된 로그)을 쓴다.
 * 저장한 경로, 취소하면 null.
 */
export function diagExport(): Promise<string | null> {
  return invoke<string | null>('diag_export')
}

/** 현재 녹음 저장 폴더(고르지 않았으면 기본: 음악 폴더/Band Recordings) */
export function engineRecordFolder(): Promise<string> {
  return invoke<string>('engine_record_folder')
}

/** 녹음 저장 폴더 고르기(폴더 대화상자, 선택은 앱 설정에 남는다). 취소하면 null */
export function engineRecordPickFolder(): Promise<string | null> {
  return invoke<string | null>('engine_record_pick_folder')
}

/** 녹음 시작(P2-4): 저장 폴더 아래 세션 폴더에 트랙별 WAV(내 입력, 피어별, 선택: 믹스) */
export function engineRecordStart(options?: RecordOptions): Promise<RecordStats> {
  return invoke<RecordStats>('engine_record_start', { options: options ?? null })
}

/** 녹음 중 피어 트랙 이름 추가(peerId → 표시 이름). 녹음 중에 들어온 연주자도 닉네임 트랙이 된다 */
export function engineRecordSetTrackNames(trackNames: Record<string, string>): Promise<void> {
  return invoke<void>('engine_record_set_track_names', { trackNames })
}

/** 녹음 정지: 파일을 마무리하고 요약을 돌려준다 */
export function engineRecordStop(): Promise<RecordSummary> {
  return invoke<RecordSummary>('engine_record_stop')
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
