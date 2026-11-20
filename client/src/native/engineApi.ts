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

/**
 * 장치 레이트 ↔ 48kHz 리샘플러 품질(P2-17e). 장치가 48kHz면 리샘플러가 돌지 않아 영향이 없다.
 * - `auto`: 프리셋을 따른다 — 프레임 2.5ms(초저지연 프리셋)면 저지연, 아니면 고음질(엔진 `EngineConfig::resample_quality`)
 * - `lowLatency`: 다항식(필터 지연 0.1ms 안, 18kHz −1.5~2.4dB, 23kHz 에일리어싱 큼)
 * - `highQuality`: sinc 128탭(18kHz까지 평탄, 단계당 +1.3~1.5ms — 44.1k 입·출력 합 약 +2.8ms)
 */
export type ResampleQuality = 'auto' | 'lowLatency' | 'highQuality'

/**
 * 엔진 스레드(워커·네트워크 수신) 코어 배치 정책(P1-21 2단계, Windows 전용). 둘 다 **실험 옵션, 기본 꺼짐**.
 * 엔진 시작 때 한 번 적용되므로 바꾸면 엔진을 다시 시작해야 한다. 환경변수 `BAND_THREAD_POLICY`
 * (`off` | `highqos` | `pcores` | `both`)가 있으면 이 설정보다 우선한다. 적용 결과는 `EngineStats.threadPolicy`.
 */
export interface ThreadPolicyConfig {
  /** 실행 속도 전원 조절(EcoQoS)을 끈다. 스케줄러는 여전히 코어를 자유롭게 고른다 */
  highQos: boolean
  /**
   * 고성능(P) 코어 CPU set에 둔다. "선호"가 아니다 — 스레드는 그 집합 안에 머물러, P 코어가 바빠도 다른 코어로 넘어가지 않는다.
   * 하이브리드 CPU가 아니거나 물리 P 코어가 4개 미만이면 적용되지 않는다(`skipped`)
   */
  preferPCores: boolean
}

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
  /** 리샘플러 품질(P2-17e). 없으면 auto */
  resampleQuality?: ResampleQuality
  /** 엔진 스레드 코어 배치 정책(P1-21 2단계, 실험적). 없으면 둘 다 꺼짐 */
  threadPolicy?: ThreadPolicyConfig
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
  /** 워커 수면 overshoot(요청보다 늦게 깬 양) 중앙값·p99·최대(µs, P2-17c). 옛 엔진이면 없음 */
  workerWakeP50Us?: number
  workerWakeP99Us?: number
  workerWakeMaxUs?: number
  /** 은닉 마감 여유(µs, 고정 1500). 대략 150µs + overshoot p99 + 워커 반복 시간을 덮어야 한다 */
  deadlineGuardUs?: number
  /** 직전 창 워커 기다림 중 FINE_POLL 수면·마감 직전 돌기 비율(%, P2-17g). 나머지는 POLL. 옛 엔진이면 없음 */
  workerFineNapPct?: number
  workerSpinPct?: number
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
  /** engine_add_peer → 이 피어와 처음 Connected(ms, P1-20). 아직이면 null. 재연결·경로 전환으로 다시 재지 않는다(옛 엔진에는 없다) */
  connectMs?: number | null
  /** engine_add_peer → 이 피어의 실제 소리가 처음 재생된 순간(ms, P1-20). 아직이면 null. 리셋 규칙은 connectMs와 같다 */
  firstAudioMs?: number | null
  /** 상대 송신 프레임(ms, P2-8b): 이 피어에게서 받은 Audio 헤더 값(2.5 | 5 | 10). 아직 못 받았거나 옛 엔진이면 null/없음 */
  remoteFrameMs?: number | null
  /** 상대 입력 장치 버퍼(ms, P2-8b): 이 피어가 Ping으로 알려 온 그쪽 실측 콜백 크기. 옛 엔진이거나 보고가 3초 넘게 없으면 null/없음 */
  remoteInputBufferMs?: number | null
  /** net 재전송 창에서 버린 이 피어의 수신 패킷(누적, 이 피어 항목 동안). Dup = 이미 받은 seq의 사본·재전송(망 중복·경로 전환 중 두 경로 수신·재생 공격), Stale = Audio seq 창(1024패킷)보다 옛 패킷. 수신 큐 전에 버려 late·lossPct에 없다. 옛 엔진에는 없다 */
  rxDupDropped?: number
  rxStaleDropped?: number
  /** 손실 모양·은닉 방식·도착 여유(P2-17h, 이 피어 항목 동안 누적). FEC/중복 전송 결정용 측정. 옛 엔진에는 없다 */
  lossShape?: LossShapeStats
}

/** 손실 모양 통계(P2-17h, 설계 1.2절 "손실 모양 통계") */
export interface LossShapeStats {
  /** 현재 수신 패킷 프레임(ms). 연속 길이(패킷) × 이 값 ≈ ms */
  frameMs: number
  /** 끝내 안 온 seq의 연속 길이 히스토그램(버킷 1, 2, 3–4, 5–8, 9–16, 17+ 패킷 — engineDiagFormat LOSS_RUN_BUCKETS). 최근 256패킷 안의 손실은 아직 확정 전이라 없다 */
  lossRuns: number[]
  /** lossRuns 17+ 버킷 연속들의 패킷 수 합(긴 버스트 손실 수를 정확히). 옛 엔진에는 없다 */
  lossLongPkts?: number
  /** 도착했지만 재생 마감을 놓친 seq의 연속 길이 히스토그램(같은 버킷) */
  lateRuns: number[]
  /** 도착 여유 = 직접 잰 "은닉 마감 − 도착"(ms, 버킷 <0, 0–0.5, 0.5–1, 1–2, 2–2.5, 2.5–3, 3–5, 5–7.5, 7.5–10, ≥10 — ARRIVAL_MARGIN_BUCKETS) 히스토그램. 꺼낸 패킷마다(제때 재생 ≥ 0) + 마감 놓친 패킷마다(< 0). ≥ F 비율 = 1프레임 늦은 복사본/FEC가 마감 안에 드는 비율 */
  arrivalMargin: number[]
  /** 은닉 방식별 프레임 수(합 = concealed): Opus FEC 디코드 / Opus PLC / PCM 페이드·무음 */
  concealFec: number
  concealPlc: number
  concealPcm: number
  /** 은닉 프레임 중 언더런 은닉(seq 유지) 수(방식과 겹침) */
  concealUnderrun: number
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
  /** 통계·판정에 쓴 클릭 수(= 검출 − 제외) */
  usedRuns: number
  /** 단일 이상치(한 번 튄 클릭)로 뺀 수, 0 또는 1 */
  excludedRuns: number
  /** 뺀 클릭의 runs 인덱스 */
  outlierIndex: number | null
  /** 프레임 + 망 왕복 + 상대 되돌리기 + 내 지터 버퍼. 중앙값·최소·최대·표준편차는 뺀 클릭 제외 */
  roundTripMs: number | null
  minMs: number | null
  maxMs: number | null
  stdDevMs: number | null
  /** 판정에 쓴 흩어짐(최대 − 최소). 5ms 넘으면 unstable */
  spreadMs: number | null
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

/** 리샘플러 상태(P2-17e). 필터 지연은 estLatencyMs·monitorLatencyMs에 들어 있다 */
export interface ResamplerStats {
  /** 실제로 쓰는 품질(auto를 푼 값) */
  quality: Exclude<ResampleQuality, 'auto'>
  /** 입력 단계(장치 → 48k) 필터 지연(ms). 입력 장치가 48kHz면 0 */
  inDelayMs: number
  /** 출력 단계(48k → 장치) 필터 지연(ms). 출력 장치가 48kHz면 0 */
  outDelayMs: number
}

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
  /** 리샘플러(P2-17e). 입·출력 장치가 모두 48kHz(리샘플 없음)거나 정지 상태면 null, 옛 엔진이면 없음 */
  resampler?: ResamplerStats | null
  /** 실제로 열린 오디오 백엔드(`AudioBackendKind`, 입·출력이 다르면 `입력/출력`). 정지 상태면 빈 문자열 */
  backend?: string
  /** 스레드 우선순위 설정 결과(MMCSS 등록 여부, 1ms 타이머) */
  threadPriority?: ThreadPriorityStats
  /** 하이브리드 CPU 코어 배치(P1-21): 워커·수신 스레드가 P/E/LP-E 코어에서 돈 비율(시작부터 누적). 비Windows·옛 엔진은 null/없음 */
  corePlacement?: CorePlacementStats | null
  /** 워커·수신 스레드에 적용한 코어 배치 정책과 결과(P1-21 2단계, 기본은 둘 다 off). 비Windows·정지 상태·옛 엔진은 null/없음 */
  threadPolicy?: ThreadPolicyStats | null
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
  /** 뺀 박 수: 새 자리가 20ms 넘게 지났거나(콜백 공백·시계 점프·계획 변경) 마지막 클릭과 100ms 안(이중 클릭 방지) */
  skippedClicks: number
  /** 새 자리에서 1ms 넘게 어긋나게(최대 약 20ms) 울리기로 한 클릭 수(시계 점프·계획 변경·콜백 공백) */
  shiftedClicks: number
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

/**
 * 스레드 하나의 코어 배치(설계 문서 4절 `corePlacement`). 엔진 시작부터 누적, 비율은 %(합 100).
 * 표본은 깨어남 가중(워커 반복·수신 패킷마다)이라 실행 시간 비율이 아니다. 구간 값은 진단 타임라인(`[P, E, LP-E]` 구간 수)에 있다.
 */
export interface ThreadPlacement {
  samples: number
  pPct: number
  ePct: number
  lpEPct: number
}

export interface CorePlacementStats {
  worker: ThreadPlacement
  /** 네트워크 없이 시작한 엔진은 null */
  netRecv: ThreadPlacement | null
}

/**
 * 스레드 정책 한 항목의 적용 결과(설계 문서 4절 `threadPolicy`): `pending` 스레드가 아직 적용 전(시작 직후 잠깐) ·
 * `off` 옵션 꺼짐(기본) · `applied` OS 호출 성공 · `failed` 실패(무시하고 기본 스케줄링) ·
 * `skipped` P 코어 CPU set만 — 걸 집합이 없어 걸지 않음(하이브리드 아님, 물리 P 코어 4개 미만).
 */
export type ThreadPolicyResult = 'pending' | 'off' | 'applied' | 'failed' | 'skipped'

/** 스레드 하나에 시작 때 한 번 건 정책: HighQoS(EcoQoS 끄기)와 P 코어 CPU set(그 집합 안에 머묾) */
export interface ThreadPolicy {
  highQos: ThreadPolicyResult
  pCores: ThreadPolicyResult
}

export interface ThreadPolicyStats {
  worker: ThreadPolicy
  /** 네트워크 없이 시작한 엔진은 null */
  netRecv: ThreadPolicy | null
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

const DRIVER_KINDS: readonly DriverKind[] = ['asio', 'wasapiShared', 'wasapiExclusive']

/**
 * 이 빌드가 지원하는 드라이버(P2-7, 설계 4절 `engine_supported_drivers`).
 * 옛 엔진(커맨드 없음)·실패·형식이 이상한 응답이면 null — 호출자는 "모두 지원"으로 본다.
 */
export async function engineSupportedDrivers(): Promise<DriverKind[] | null> {
  try {
    const list = await invoke<unknown>('engine_supported_drivers')
    if (!Array.isArray(list)) return null
    const known = list.filter((d): d is DriverKind => DRIVER_KINDS.includes(d as DriverKind))
    return known.length > 0 ? known : null
  } catch {
    return null
  }
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

/** 진행 중인 `engine_stats` 호출(모든 호출자가 나눠 쓴다). */
let statsInFlight: Promise<EngineStats> | null = null

/**
 * 엔진 통계. 앞선 호출이 아직 끝나지 않았으면 새로 보내지 않고 그 응답을 함께 기다린다(#214 리뷰 L4): 커맨드는 Rust
 * blocking 풀에서 엔진 슬롯 락을 기다리므로(피어 추가·탭 join 중), 폴러 여럿(연주 세션 500ms·피어 통계 1초·테스트 모드)이
 * 응답보다 빨리 보내면 블로킹 스레드가 쌓였다. 폴러는 각자 진행 중이면 그 틱을 건너뛴다(같은 표본을 두 번 세지 않게).
 */
export function engineStats(): Promise<EngineStats> {
  if (statsInFlight) return statsInFlight
  const p = invoke<EngineStats>('engine_stats').finally(() => {
    if (statsInFlight === p) statsInFlight = null
  })
  statsInFlight = p
  return p
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

/** `engine_record_pick_folder` 결과. saveError: 고른 폴더를 앱 설정에 남기지 못함(이번 실행만 그 폴더, 다음 실행은 이전 폴더·기본) */
export interface RecordFolderPick {
  folder: string
  saveError?: string | null
}

/** 녹음 저장 폴더 고르기(폴더 대화상자, 선택은 앱 설정에 남는다). 취소하면 null */
export function engineRecordPickFolder(): Promise<RecordFolderPick | null> {
  return invoke<RecordFolderPick | null>('engine_record_pick_folder')
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
