/**
 * 연주 세션 컨트롤러 (React 비의존, 테스트 가능).
 *
 * 엔진 수명주기 + 시그널링을 한곳에서 관리한다:
 * - start: engine_start(커맨드 큐 안) → 그 사이 stop/퇴장/clientId 변경이 있었으면 엔진을 다시 내리고 실패 처리 (H2)
 *          → start-performing 전송 → descriptor 브로드캐스트
 * - 서버가 start-performing을 거부(perform-not-approved / room-full-performers / login-required)하면 롤백 (H3)
 * - WS 끊김: 엔진은 유지. welcome.resumed면 descriptor만 재전송, 아니면 새 clientId로 재시작 (M6).
 *   재연결이 오래 안 되면 정지.
 * - engine_add_peer 실패는 백오프 재시도 (M1), engine_stats.running=false가 이어지면 정지 (L7)
 * - 모든 엔진 커맨드(start 포함)는 하나의 큐로 직렬화 (L6)
 * - 릴레이(P1-4b): 서버 relay-token(내 ssrc)이 오면 engine_set_relay, 받은 시각 + expiresIn 기준 만료 60초 전에
 *   relay-token-request. 엔진이 릴레이를 expired/unreachable로 보고하면 바로 다시 요청한다.
 *   서버가 ssrc-conflict를 보내면 엔진을 새 ssrc로 다시 시작한다. key-conflict(P1-1c, 다른 사용자가 내 공개키를 먼저 등록)도
 *   새 키로 재시작하고 안내한다(30초 안에 반복되면 정지). descriptor-invalid는 정지하고 안내.
 */
import type { EngineConfig, EngineStats, LocalDescriptor, MetronomeConfig, MetronomePlan, RelayConfig } from './engineApi'
import {
  clampBeats,
  clampBpm,
  followConfig,
  metronomeErrorFromCode,
  parseMetronomeSignal,
  stoppedConfig,
  type MetronomeSignalState,
  type MetronomeView,
} from './metronomeLogic'
import { parseRelayToken, relayNeedsNewToken, relayRefreshDelayMs } from './relayLogic'
import { newFirstAudioTrack, noteAddPeer, notePeerLeft, noteStats, summarizeFirstAudio, type FirstAudioSummary, type FirstAudioTrack } from './firstAudio'
import { NativeSignalMachine, isValidDescriptor, type SessionAction } from './sessionLogic'
import {
  performErrorFromCode,
  KEY_CONFLICT_MESSAGE,
  KEY_CONFLICT_REPEAT_MESSAGE,
  DESCRIPTOR_INVALID_MESSAGE,
  DEVICE_RECOVERING_MESSAGE,
  DEVICE_RECOVERED_MESSAGE,
  DEVICE_RECOVERED_DEFAULT_MESSAGE,
  DEVICE_LOST_FAILED_MESSAGE,
  DEVICE_LOST_REPEAT_MESSAGE,
} from './performSignals'
// 연주 신호 해석·안내 문구는 웹 첫 묶음에도 실리는 가벼운 모듈에 있다(웹 관람 묶음 분리). 기존 import 경로를 위해 다시 내보낸다
export {
  performErrorFromCode,
  isEngineDescriptorErrorCode,
  KEY_CONFLICT_MESSAGE,
  KEY_CONFLICT_REPEAT_MESSAGE,
  DESCRIPTOR_INVALID_MESSAGE,
  DEVICE_RECOVERING_MESSAGE,
  DEVICE_RECOVERED_MESSAGE,
  DEVICE_RECOVERED_DEFAULT_MESSAGE,
  DEVICE_LOST_FAILED_MESSAGE,
  DEVICE_LOST_REPEAT_MESSAGE,
} from './performSignals'

export interface EngineBackend {
  start(config: EngineConfig, peerId: string): Promise<LocalDescriptor>
  stop(): Promise<unknown>
  addPeer(peerId: string, descriptor: LocalDescriptor): Promise<unknown>
  /** 릴레이 설정/토큰 갱신 (engine_set_relay) */
  setRelay(relay: RelayConfig | null): Promise<unknown>
  removePeer(peerId: string): Promise<unknown>
  setPeerGain(peerId: string, gain: number, muted: boolean, pan?: number): Promise<unknown>
  setInputMuted(muted: boolean): Promise<unknown>
  /** 공유 메트로놈 계획 (engine_set_metronome, P2-3) */
  setMetronome(config: MetronomeConfig): Promise<MetronomePlan>
  stats(): Promise<EngineStats>
}

export type SignalOut = Record<string, unknown> & { type: string }

export interface SessionEnv {
  send(payload: SignalOut): void
  getSelfId(): string | null
  getRoomId(): string | null
  /** 지금 알고 있는 다른 연주자 id */
  getOtherPerformers(): string[]
}

/**
 * restarting: 서버 세션은 그대로 두고 엔진만 다시 여는 중(장치 분실 자동 재시작, ssrc/key 충돌·새 clientId 재시작).
 * 화면은 연주자 상태를 유지하고(악기·연주자 항목), 사용자의 연주 시작은 무시, 연주 중단은 재시작을 취소하고 서버에 알린다 (#208 리뷰 M-1)
 */
export type PerformPhase = 'idle' | 'starting' | 'restarting' | 'running'

export interface PerformState {
  phase: PerformPhase
  instrument: string | null
  /** 엔진을 시작할 때 쓴 clientId */
  sessionId: string | null
  error: string | null
  stats: EngineStats | null
  inputMuted: boolean
  /** 마지막 engine_set_relay 실패 사유(성공하면 null). 'failed' 안내에서 "릴레이 서버 없음"과 구분한다 */
  relayError?: string | null
  /** 공유 메트로놈(P2-3). 켜진 적 없거나 엔진이 꺼지면 null */
  metronome?: MetronomeView | null
  /** 연주 시작 → 첫 피어 소리(P1-20, 가장 빠른/가장 늦은 피어). 연주 중이 아니거나 피어가 없으면 null */
  firstAudio?: FirstAudioSummary | null
  /**
   * 오디오 장치 분실(deviceLost) 뒤 자동 재시작 안내(#206 후보). 재시작하는 동안·직후 잠시 보인다. 없으면 null.
   * error와 달리 엔진 정지·시작으로 지워지지 않는다(재시작 중에도 띠를 유지)
   */
  deviceNotice?: string | null
}

export interface StartOptions {
  instrument: string
  config: EngineConfig
  nickname: string
  isHost: boolean
}

export interface PerformSessionOptions {
  /** engine_add_peer 재시도 간격 */
  retryDelaysMs?: number[]
  /** WS가 끊긴 뒤 이 시간 안에 welcome이 없으면 연주 정지 (서버 재개 유예 30초 + 여유) */
  reconnectTimeoutMs?: number
  /** engine_stats.running=false가 몇 번 연속이면 엔진이 죽은 것으로 볼지 */
  deadStatsThreshold?: number
  /** relay-token-request에 응답이 없을 때 다시 요청하는 간격 */
  relayRetryMs?: number
  /** 단조 시계(ms). 테스트용 — 기본 performance.now */
  now?: () => number
  /** 장치 분실 뒤 재시작 시도 전 대기(ms). 시도 횟수 = 길이 (기본 2회: 0.5초, 1.5초) */
  deviceRecoveryDelaysMs?: number[]
  /** 이 시간(ms) 안에 장치 분실이 deviceLostMaxRecoveries번을 넘으면 자동 재시작 대신 정지 (장치가 계속 흔들림) */
  deviceLostWindowMs?: number
  deviceLostMaxRecoveries?: number
  /** 재시작 성공 안내를 보여 줄 시간(ms) */
  deviceNoticeMs?: number
}

/** 같은 드라이버의 기본 장치(장치 id null, 입력 채널 처음부터) — 쓰던 장치가 사라졌을 때 */
export function withDefaultDevices(config: EngineConfig): EngineConfig {
  return {
    ...config,
    inputDevice: null,
    outputDevice: null,
    inputChannels: config.channels === 2 ? [0, 1] : [0],
  }
}

/** 이 시간 안에 key-conflict가 또 오면 재시작 반복 대신 정지 */
const KEY_CONFLICT_REPEAT_MS = 30_000

const DEFAULT_RETRY_DELAYS = [500, 1000, 2000, 4000, 8000]

export class PerformSession {
  private machine = new NativeSignalMachine()
  private state: PerformState = { phase: 'idle', instrument: null, sessionId: null, error: null, stats: null, inputMuted: false }
  private listeners = new Set<(s: PerformState) => void>()
  private queue: Promise<unknown> = Promise.resolve()
  /** start/stop 세대. stop이나 새 start가 오면 이전 start 결과는 버린다 */
  private gen = 0
  private lastStart: StartOptions | null = null
  private retryTimers = new Map<string, ReturnType<typeof setTimeout>>()
  private retryAttempts = new Map<string, number>()
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null
  /** 지금 엔진의 ssrc (relay-token 대조용) */
  private localSsrc: number | null = null
  private relayTimer: ReturnType<typeof setTimeout> | null = null
  /** 마지막 relay-token-request 시각(ms) — 엔진 상태로 인한 즉시 요청의 간격 제한 */
  private lastRelayRequestAt = -Infinity
  private deadStats = 0
  /** 통계 요청이 진행 중(느린 응답): 다음 틱은 건너뛴다 — 같은 표본으로 deadStats·deviceLostPolls를 두 번 세지 않고 블로킹 풀에 쌓지 않는다(#214 리뷰 L4) */
  private statsInFlight = false
  /** 마지막 key-conflict 재시작 시각(ms) — 반복 재시작 방지 */
  private lastKeyConflictAt = -Infinity
  /** 따르는 방장 메트로놈(from = 방장 엔진 peerId) */
  private remoteMetronome: { from: string; state: MetronomeSignalState } | null = null
  private readonly retryDelays: number[]
  private readonly reconnectTimeoutMs: number
  private readonly deadStatsThreshold: number
  private readonly relayRetryMs: number
  private readonly now: () => number
  /** 연주 시작 → 첫 피어 소리 추적(P1-20). 엔진을 시작할 때마다 새로, 정지하면 null */
  private firstAudio: FirstAudioTrack | null = null
  /** 장치 분실 자동 재시작 중. 바깥(사용자·퇴장)에서 stop하면 false가 되어 재시작을 그만둔다 */
  private recoveringDevice = false
  /** deviceLost를 연속으로 본 폴링 수(디바운스) */
  private deviceLostPolls = 0
  /** 최근 장치 분실 시각들(반복 판정) */
  private deviceLostTimes: number[] = []
  private deviceNoticeTimer: ReturnType<typeof setTimeout> | null = null
  /** 재시작 앞 정지에서 기억해 둔 방장 메트로놈(새 엔진에서 같은 BPM·박자로 다시 켠다) */
  private hostedMetronomeForRestart: { bpm: number; beatsPerBar: number } | null = null
  private readonly deviceRecoveryDelays: number[]
  private readonly deviceLostWindowMs: number
  private readonly deviceLostMaxRecoveries: number
  private readonly deviceNoticeMs: number

  constructor(private readonly engine: EngineBackend, private readonly env: SessionEnv, opts: PerformSessionOptions = {}) {
    this.retryDelays = opts.retryDelaysMs ?? DEFAULT_RETRY_DELAYS
    this.reconnectTimeoutMs = opts.reconnectTimeoutMs ?? 35_000
    this.deadStatsThreshold = opts.deadStatsThreshold ?? 2
    this.relayRetryMs = opts.relayRetryMs ?? 10_000
    this.now = opts.now ?? (() => performance.now())
    this.deviceRecoveryDelays = opts.deviceRecoveryDelaysMs ?? [500, 1500]
    this.deviceLostWindowMs = opts.deviceLostWindowMs ?? 60_000
    this.deviceLostMaxRecoveries = opts.deviceLostMaxRecoveries ?? 3
    this.deviceNoticeMs = opts.deviceNoticeMs ?? 8000
  }

  // ---------------------------------------------------------------- 상태

  getState(): PerformState {
    return this.state
  }

  subscribe(listener: (s: PerformState) => void): () => void {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  private setState(patch: Partial<PerformState>) {
    this.state = { ...this.state, ...patch }
    for (const l of this.listeners) l(this.state)
  }

  get peers(): string[] {
    return this.machine.peers
  }

  /** 엔진 커맨드 직렬화. 실패는 호출자에게 전달하되 큐는 계속 진행 */
  private enqueue<T>(fn: () => Promise<T>): Promise<T> {
    const run = this.queue.then(fn, fn)
    this.queue = run.catch(() => {})
    return run
  }

  // ---------------------------------------------------------------- 시작/정지

  /** 사용자가 시작하는 경로. 새 세션이므로 key-conflict 반복 판정을 초기화한다(PR #48 L2) */
  async start(opts: StartOptions): Promise<boolean> {
    return this.startEngine(opts, true)
  }

  /** userInitiated=false는 restart()(ssrc/key 충돌, 새 clientId) 경로: key-conflict 반복 판정을 이어 간다 */
  private async startEngine(opts: StartOptions, userInitiated: boolean): Promise<boolean> {
    if (this.state.phase === 'running') return true
    if (this.state.phase === 'starting') return false
    // 재시작 중에는 사용자의 시작을 받지 않는다(재시작이 그 엔진을 끄고 옛 설정으로 다시 열던 경합, #208 리뷰 M-1)
    if (this.state.phase === 'restarting' && userInitiated) return false
    if (userInitiated) {
      this.lastKeyConflictAt = -Infinity
      this.hostedMetronomeForRestart = null // 옛 재시작이 남긴 방장 메트로놈을 새 세션에 켜지 않는다 (#208 리뷰 L-2)
    }
    const selfId = this.env.getSelfId()
    const roomId = this.env.getRoomId()
    if (!selfId || !roomId) {
      this.setState({ error: '합주실에 연결되지 않았습니다.' })
      return false
    }

    const gen = ++this.gen
    this.lastStart = opts
    // 재시작 중이면 'restarting'을 유지한다(연주자 화면 유지). 사용자 시작은 'starting'
    const restarting = this.state.phase === 'restarting'
    this.setState({ phase: restarting ? 'restarting' : 'starting', error: null, instrument: opts.instrument, sessionId: selfId })
    // 연주 시작(P1-20): 지금 알고 있는 연주자는 이 시각부터, 뒤에 들어오는 연주자는 처음 추가한 시각부터 잰다
    this.firstAudio = newFirstAudioTrack(this.now(), this.env.getOtherPerformers())

    // 시작·검사·확정을 한 큐 작업 안에서 끝낸다. 그래야 사이에 끼어든 stop/재시작과 순서가 꼬이지 않는다.
    return this.enqueue(async () => {
      let descriptor: LocalDescriptor
      try {
        descriptor = await this.engine.start(opts.config, selfId)
      } catch (err) {
        if (gen === this.gen) {
          // 장치 분실 재시작의 시도 실패는 오류로 보이지 않는다 — 다음 시도(기본 장치)를 하거나 전부 실패하면 안내한다 (#208 리뷰 L-4)
          if (this.recoveringDevice) this.setState({ phase: 'restarting' })
          else {
            if (!userInitiated) this.restartFailedUnannounced = true
            this.setState({ phase: 'idle', instrument: null, sessionId: null, error: `오디오 엔진 시작 실패: ${errorText(err)}` })
          }
        }
        return false
      }

      // 시작하는 동안 정지/퇴장/재연결이 있었으면 방금 연 엔진을 다시 닫는다 (H2)
      const stale = gen !== this.gen || this.env.getSelfId() !== selfId || this.env.getRoomId() !== roomId
      if (stale || !isValidDescriptor(descriptor)) {
        await this.engine.stop().catch(() => {})
        if (gen === this.gen) {
          this.gen++
          // 장치 재시작 중이고 아직 같은 방이면(새 clientId로 바뀐 것뿐) 다음 시도가 새 id로 연다
          if (this.recoveringDevice && this.env.getRoomId() === roomId) {
            this.setState({ phase: 'restarting' })
          } else {
            if (!userInitiated && !stale) this.restartFailedUnannounced = true
            this.setState({
              phase: 'idle',
              instrument: null,
              sessionId: null,
              error: stale ? null : '엔진이 올바르지 않은 연결 정보를 반환했습니다.',
            })
          }
        }
        return false
      }

      this.deadStats = 0
      this.deviceLostPolls = 0
      this.localSsrc = descriptor.ssrc
      this.setState({ phase: 'running' })
      // 엔진이 뜬 뒤에 연주 시작 알림 → descriptor 브로드캐스트 (서버가 연주자로 인정해야 중계함)
      this.env.send({ type: 'start-performing', instrument: opts.instrument, nickname: opts.nickname, isHost: opts.isHost })
      this.runActions(this.machine.started(selfId, descriptor, this.env.getOtherPerformers()))
      if (this.state.inputMuted) this.enqueue(() => this.engine.setInputMuted(true)).catch(() => {})
      // 엔진이 뜨기 전에 받은 방장 메트로놈이 있으면 적용(서버도 descriptor를 받으면 다시 보내 준다)
      if (this.remoteMetronome) this.applyRemoteMetronome()
      return true
    })
  }

  /**
   * notify=true면 native-leave + stop-performing 전송.
   * forRestart는 재시작(장치 분실·ssrc/key 충돌·새 clientId)이 안에서 부르는 정지만 쓴다: phase를 'restarting'으로 두고(악기 유지)
   * 서버에 알리지 않는다. 그 밖의 정지(사용자·퇴장·다른 오류)는 진행 중인 재시작을 그만두게 하고, 재시작 중이었으면 서버에도 알린다
   */
  async stop(opts: { notify?: boolean; error?: string | null; forRestart?: boolean } = {}): Promise<void> {
    const { notify = true, error = null, forRestart = false } = opts
    if (!forRestart) {
      if (this.recoveringDevice) {
        this.recoveringDevice = false
        this.setDeviceNotice(null)
      }
      this.hostedMetronomeForRestart = null // (#208 리뷰 L-2)
    }
    // 재시작 중(서버는 아직 연주자로 안다)에 멈추면 서버에도 연주 중단을 알린다
    const wasRunning = this.state.phase === 'running' || (this.state.phase === 'restarting' && !forRestart)
    if (this.state.phase === 'idle') {
      if (error) this.setState({ error })
      return
    }
    this.gen++ // 진행 중인 start는 완료 시 엔진을 닫고 실패 처리한다
    this.machine.stopped()
    this.clearRetries()
    this.clearReconnectTimer()
    this.clearRelayTimer()
    this.localSsrc = null
    this.lastRelayRequestAt = -Infinity
    // 연주자가 아니면 서버가 메트로놈을 보내지 않으므로 잊는다(다시 시작하면 서버가 현재 상태를 보내 준다)
    this.firstAudio = null
    this.remoteMetronome = null
    if (forRestart) this.setState({ phase: 'restarting', stats: null, error, relayError: null, metronome: null, firstAudio: null })
    else this.setState({ phase: 'idle', instrument: null, sessionId: null, stats: null, error, relayError: null, metronome: null, firstAudio: null })
    if (wasRunning) {
      if (notify) {
        this.env.send({ type: 'native-leave' })
        this.env.send({ type: 'stop-performing' })
      }
      await this.enqueue(() => this.engine.stop()).catch((err) => console.warn('[Native] engine_stop 실패:', errorText(err)))
    }
  }

  /** 악기 변경이 승인됨 (재시작 시에도 새 악기로) */
  updateInstrument(instrument: string): void {
    if (this.lastStart) this.lastStart = { ...this.lastStart, instrument }
    if (this.state.phase !== 'idle') this.setState({ instrument })
  }

  clearError(): void {
    if (this.state.error) this.setState({ error: null })
  }

  // ---------------------------------------------------------------- 시그널링

  /** native-descriptor / native-leave / 연주 거부 에러를 처리했으면 true */
  handleSignal(payload: Record<string, unknown>): boolean {
    const from = typeof payload.from === 'string' ? payload.from : null
    if (payload.type === 'native-descriptor') {
      if (!from || this.state.phase !== 'running') return true
      if (!isValidDescriptor(payload.descriptor)) {
        console.warn('[Native] 잘못된 descriptor 무시:', from.slice(0, 8))
        return true
      }
      this.runActions(this.machine.descriptorReceived(from, payload.descriptor))
      return true
    }
    if (payload.type === 'native-leave') {
      if (from) this.peerGone(from)
      return true
    }
    if (payload.type === 'relay-token') {
      this.relayTokenReceived(payload)
      return true
    }
    if (payload.type === 'metronome') {
      this.metronomeReceived(payload)
      return true
    }
    if (payload.type === 'error') {
      const metroError = metronomeErrorFromCode(payload.code, payload.message)
      if (metroError) {
        // 서버가 내 메트로놈을 받지 않았다: 내 클릭도 끈다(방 안에서 나만 울리지 않게)
        const m = this.state.metronome
        if (m && m.leaderId === null) {
          this.setState({ metronome: null })
          if (this.state.phase === 'running') {
            this.enqueue(() => this.engine.setMetronome(stoppedConfig(m.bpm, m.beatsPerBar))).catch(() => {})
          }
        }
        this.setState({ error: metroError })
        return true
      }
    }
    if (payload.type === 'error' && payload.code === 'ssrc-conflict') {
      // 같은 방 다른 연주자가 내 ssrc를 쓴다: 서버는 descriptor를 중계하지 않았다 → 새 ssrc로 재시작
      if (this.state.phase === 'running') void this.restart()
      return true
    }
    if (payload.type === 'error' && payload.code === 'key-conflict') {
      // 같은 방 다른 사용자가 내 엔진 공개키를 먼저 등록했다(위장·선점, P1-1c): 서버는 중계하지 않았다 → 새 키로 재시작.
      // 새 키는 엔진이 매번 새로 만들므로 다시 충돌할 수 없다. 짧은 시간에 또 오면 반복하지 않고 멈춘다.
      if (this.state.phase !== 'running') return true
      const now = Date.now()
      if (now - this.lastKeyConflictAt < KEY_CONFLICT_REPEAT_MS) {
        void this.stop({ notify: true, error: KEY_CONFLICT_REPEAT_MESSAGE })
        return true
      }
      this.lastKeyConflictAt = now
      void this.restart().then(() => {
        if (this.state.phase === 'running') this.setState({ error: KEY_CONFLICT_MESSAGE })
      })
      return true
    }
    if (payload.type === 'error' && payload.code === 'descriptor-invalid') {
      // 서버가 중계하지 않았다 → 아무와도 연결되지 않은 채 '연주 중'으로 남지 않게 정지하고 안내(PR #48 L3)
      void this.stop({ notify: true, error: DESCRIPTOR_INVALID_MESSAGE })
      return true
    }
    if (payload.type === 'error') {
      const mapped = performErrorFromCode(payload.code, payload.message)
      if (!mapped) return false
      if (this.state.phase !== 'idle' && mapped.rollback) {
        // 서버가 연주자로 인정하지 않았으므로 stop-performing은 보내지 않는다
        void this.stop({ notify: false, error: mapped.message })
      } else {
        this.setState({ error: mapped.message })
      }
      return true
    }
    return false
  }

  performerAppeared(peerId: string): void {
    if (this.state.phase !== 'running') return
    this.runActions(this.machine.performerAppeared(peerId))
  }

  peerGone(peerId: string): void {
    this.clearRetry(peerId)
    // 방장(메트로놈 기준)이 사라지면 시각 기준이 없다 → 멈춤
    if (this.remoteMetronome?.from === peerId) {
      const { state } = this.remoteMetronome
      this.remoteMetronome = null
      this.setState({ metronome: null })
      if (this.state.phase === 'running') {
        this.enqueue(() => this.engine.setMetronome(stoppedConfig(state.bpm, state.beatsPerBar))).catch(() => {})
      }
    }
    // 들리기 전에 떠난 연주자는 다시 들어오면 그 재입장부터 잰다(#120 L-5). 큐에 넣어 앞서 대기 중인 add_peer 뒤에 적용한다
    const gen = this.gen
    this.enqueue(async () => {
      if (gen === this.gen && this.firstAudio) this.firstAudio = notePeerLeft(this.firstAudio, peerId)
    }).catch(() => {})
    this.runActions(this.machine.peerGone(peerId))
  }

  // ---------------------------------------------------------------- WS 연결

  /** WS 끊김: 엔진은 유지(UDP 메시는 계속 동작). 일정 시간 안에 재연결이 없으면 정지 */
  connectionLost(): void {
    if (this.state.phase === 'idle') return
    this.clearReconnectTimer()
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null
      void this.stop({ notify: false, error: '시그널링 서버 연결이 끊겨 연주를 중단했습니다.' })
    }, this.reconnectTimeoutMs)
  }

  /**
   * welcome 수신. resumed && 같은 id → 엔진 유지, descriptor만 다시 브로드캐스트(끊긴 동안 놓친 교환 보완).
   * 그 외(새 clientId) → 이전 세션은 서버에서 사라졌으므로 새 id로 엔진 재시작.
   * 호출자는 이 전에 join(rejoin)을 보내 둬야 한다(서버는 클라이언트별로 순서대로 처리).
   */
  async welcome(clientId: string, resumed: boolean): Promise<void> {
    this.clearReconnectTimer()
    if (this.state.phase === 'idle') return
    // 장치 분실 재시작 중이면 그 재시작에 맡긴다: 재시작은 엔진을 열 때 지금 clientId를 읽어 start-performing·descriptor를
    // 다시 알리므로 재개(같은 id)든 새 id든 따로 다시 열 필요가 없다. 여기서 또 열면 재시작이 그 엔진을 끄고 다시 열어
    // 피어 재연결이 한 번 더 일어났다 (#208 재리뷰 L-6)
    if (this.recoveringDevice) return
    if (resumed && clientId === this.state.sessionId && this.state.phase === 'running') {
      this.runActions(this.machine.reannounce(this.env.getOtherPerformers()))
      return
    }
    await this.restart()
  }

  /** 서버 쪽 세션은 그대로 두고 엔진만 새로 시작(새 clientId 또는 새 ssrc) */
  private async restart(): Promise<void> {
    const last = this.lastStart
    if (!last) {
      await this.stop({ notify: false })
      return
    }
    const ok = await this.restartWith(last)
    // 다시 열지 못해 멈췄다(엔진 시작 실패 등): 재시작 앞 정지는 서버에 알리지 않았으므로 지금 알린다 —
    // 아니면 서버는 계속 연주자로 알고 다른 사람에게 이 사람이 연주 중으로 보였다 (#208 재리뷰 L-7).
    // 그사이 사용자가 멈췄거나(이미 알림) 장치 재시작이 이어받았으면 보내지 않는다
    if (!ok && this.state.phase === 'idle' && !this.recoveringDevice && this.restartFailedUnannounced) {
      this.env.send({ type: 'native-leave' })
      this.env.send({ type: 'stop-performing' })
    }
    this.restartFailedUnannounced = false
  }

  /** startEngine이 재시작 경로(userInitiated=false)에서 실패해 idle로 끝났다 — restart()가 서버에 알릴지 판단 */
  private restartFailedUnannounced = false

  /** 방장 메트로놈을 기억해 둔 채 알림 없이 엔진을 멈춘다(재시작 앞 단계) */
  private async stopForRestart(): Promise<void> {
    // 방장이 켠 메트로놈: startUs는 엔진 시계 기준이라 새 엔진에서는 의미가 없다(PR #36 M1).
    // 먼저 정지를 알리고(따르는 쪽이 옛 startUs로 계속 돌지 않게), 새 엔진이 뜨면 같은 BPM·박자로 새 박 0을 알린다.
    const m = this.state.metronome
    const hosted = m && m.leaderId === null && m.running ? { bpm: m.bpm, beatsPerBar: m.beatsPerBar } : null
    if (hosted) {
      this.hostedMetronomeForRestart = hosted
      this.env.send({ type: 'metronome', state: { running: false, bpm: hosted.bpm, beatsPerBar: hosted.beatsPerBar, startUs: 0 } })
    }
    await this.stop({ notify: false, forRestart: true })
  }

  /** 엔진을 (멈추고) opts로 다시 시작. 성공하면 true */
  private async restartWith(opts: StartOptions): Promise<boolean> {
    if (this.state.phase === 'running' || this.state.phase === 'starting') await this.stopForRestart()
    const ok = await this.startEngine(opts, false)
    const hosted = this.hostedMetronomeForRestart
    if (ok) this.hostedMetronomeForRestart = null
    if (ok && hosted) await this.setHostMetronome(true, hosted.bpm, hosted.beatsPerBar)
    return ok
  }

  // ---------------------------------------------------------------- 릴레이 (P1-4b)

  private relayTokenReceived(payload: Record<string, unknown>): void {
    const msg = parseRelayToken(payload)
    if (!msg) {
      console.warn('[Native] 잘못된 relay-token 무시')
      return
    }
    // 이전 엔진(재시작 전 ssrc)에 대한 토큰은 버린다. 엔진도 ssrc를 다시 확인한다.
    if (this.state.phase !== 'running' || msg.ssrc !== this.localSsrc) return
    const gen = this.gen
    const config = { addr: msg.addr, token: msg.token, expiresInSec: msg.expiresIn, ...(msg.bindKey ? { bindKey: msg.bindKey } : {}) }
    this.enqueue(async () => {
      if (gen !== this.gen) return null
      try {
        const r = await this.engine.setRelay(config)
        if (gen === this.gen && this.state.relayError) this.setState({ relayError: null })
        return r
      } catch (err) {
        if (gen === this.gen) this.setState({ relayError: errorText(err) })
        throw err
      }
    }).catch((err) => console.warn('[Native] 릴레이 설정 실패:', errorText(err)))
    // 내 시계가 아니라 받은 시각 + 남은 수명 기준(시계 오차, M1)
    this.scheduleRelayRefresh(relayRefreshDelayMs(msg.expiresIn))
  }

  /** 갱신 요청 후 응답이 없으면(WS 순단 등) 다시 요청한다. 새 토큰이 오면 타이머가 다시 잡힌다 */
  private scheduleRelayRefresh(delayMs: number) {
    this.clearRelayTimer()
    this.relayTimer = setTimeout(() => {
      this.relayTimer = null
      if (this.state.phase !== 'running') return
      this.requestRelayToken()
      this.scheduleRelayRefresh(this.relayRetryMs)
    }, delayMs)
  }

  private requestRelayToken() {
    this.lastRelayRequestAt = Date.now()
    this.env.send({ type: 'relay-token-request' })
  }

  /**
   * 엔진이 릴레이를 expired/unreachable로 보고하면(만료, 릴레이가 토큰을 거부해 응답 없음) 갱신 시각을 기다리지 않고
   * 바로 새 토큰을 요청한다(relayRetryMs에 한 번까지). 응답이 없으면 기존 재시도 타이머가 이어 받는다.
   */
  private maybeRequestRelayTokenFor(stats: EngineStats) {
    if (!relayNeedsNewToken(stats.relay)) return
    if (Date.now() - this.lastRelayRequestAt < this.relayRetryMs) return
    this.requestRelayToken()
    this.scheduleRelayRefresh(this.relayRetryMs)
  }

  private clearRelayTimer() {
    if (this.relayTimer) clearTimeout(this.relayTimer)
    this.relayTimer = null
  }

  // ---------------------------------------------------------------- 엔진 제어

  setPeerGain(peerId: string, gain: number, muted: boolean, pan?: number): void {
    if (this.state.phase !== 'running') return
    this.enqueue(() => this.engine.setPeerGain(peerId, gain, muted, pan)).catch((err) => console.warn('[Native] gain 실패:', errorText(err)))
  }

  setInputMuted(muted: boolean): void {
    this.setState({ inputMuted: muted })
    if (this.state.phase !== 'running') return
    this.enqueue(() => this.engine.setInputMuted(muted)).catch((err) => console.warn('[Native] 입력 뮤트 실패:', errorText(err)))
  }

  // ---------------------------------------------------------------- 공유 메트로놈 (P2-3)

  private metronomeReceived(payload: Record<string, unknown>): void {
    const msg = parseMetronomeSignal(payload)
    if (!msg) {
      console.warn('[Native] 잘못된 metronome 무시')
      return
    }
    this.remoteMetronome = msg
    this.setState({
      metronome: msg.state.running ? { running: true, bpm: msg.state.bpm, beatsPerBar: msg.state.beatsPerBar, leaderId: msg.from } : null,
    })
    if (this.state.phase === 'running') this.applyRemoteMetronome()
  }

  private applyRemoteMetronome(): void {
    const m = this.remoteMetronome
    if (!m) return
    const gen = this.gen
    this.enqueue(async () => {
      if (gen !== this.gen) return null
      return this.engine.setMetronome(followConfig(m.from, m.state))
    }).catch((err) => console.warn('[Native] 메트로놈 적용 실패:', errorText(err)))
  }

  /**
   * 방장: 메트로놈 시작(또는 BPM·박자 변경 = 새 박 0으로 다시 시작)·정지. 엔진이 정한 startUs를 방에 알린다.
   * 서버가 방장 여부를 DB로 확인한다(아니면 metronome-host-only 에러 → 내 클릭도 끈다).
   */
  async setHostMetronome(running: boolean, bpm: number, beatsPerBar: number): Promise<boolean> {
    if (this.state.phase !== 'running') return false
    const gen = this.gen
    const config: MetronomeConfig = { running, bpm: clampBpm(bpm), beatsPerBar: clampBeats(beatsPerBar) }
    let plan: MetronomePlan
    try {
      plan = await this.enqueue(() => this.engine.setMetronome(config))
    } catch (err) {
      if (gen === this.gen) this.setState({ error: `메트로놈 설정 실패: ${errorText(err)}` })
      return false
    }
    if (gen !== this.gen) return false
    this.remoteMetronome = null
    this.setState({ metronome: running ? { running, bpm: plan.bpm, beatsPerBar: plan.beatsPerBar, leaderId: null } : null })
    this.env.send({ type: 'metronome', state: { running: plan.running, bpm: plan.bpm, beatsPerBar: plan.beatsPerBar, startUs: plan.startUs } })
    return true
  }

  /** 주기 호출(500ms). engine_stats.running=false가 이어지면 엔진이 죽은 것으로 보고 정지 (L7) */
  async pollStats(): Promise<void> {
    if (this.state.phase !== 'running') return
    const gen = this.gen
    // 통계 요청은 명령 큐 밖이라 그사이 재추가와 겹칠 수 있다: 요청 순간의 add 시각과 비교한다(#120 L-4)
    const addedAtAtRequest = this.firstAudio?.addedAt
    if (this.statsInFlight) return
    let stats: EngineStats
    this.statsInFlight = true
    try {
      stats = await this.engine.stats()
    } catch {
      return
    } finally {
      this.statsInFlight = false
    }
    if (gen !== this.gen || this.state.phase !== 'running') return
    if (!stats.running) {
      // 장치 분실(헤드폰 연결로 기본 장치 변경, 공유 모드 포맷 변경, USB 분리): 합주를 끝내지 않고 엔진만 다시 연다.
      // 복구되지 않는 스트림 오류(streamFailed)와 원인 모를 정지는 아래처럼 정지한다 (#206 후보, 엔진 계약 native-engine-design.md deviceLost)
      // 한 번의 통계로는 재시작하지 않는다: 콜백이 잠깐(1초) 멈춘 것도 deviceLost로 보이고 곧 풀린다 —
      // 재시작은 피어 재연결·메트로놈 새 박 0·녹음 종료를 부르므로 deadStatsThreshold번 연속일 때만 (#208 리뷰 L-1)
      if (stats.deviceLost && !stats.streamFailed && this.lastStart) {
        this.deadStats = 0
        this.deviceLostPolls++
        if (this.deviceLostPolls >= this.deadStatsThreshold) {
          this.deviceLostPolls = 0
          void this.recoverFromDeviceLoss()
        }
        return
      }
      this.deviceLostPolls = 0
      this.deadStats++
      if (this.deadStats >= this.deadStatsThreshold) {
        await this.stop({ notify: true, error: '오디오 엔진이 멈췄습니다 (장치 분리 등). 연주를 다시 시작해주세요.' })
      }
      return
    }
    this.deadStats = 0
    this.deviceLostPolls = 0
    if (this.firstAudio) this.firstAudio = noteStats(this.firstAudio, stats.peers, addedAtAtRequest)
    this.setState({ stats, firstAudio: summarizeFirstAudio(this.firstAudio, stats.peers) })
    this.maybeRequestRelayTokenFor(stats)
  }

  /**
   * 장치 분실 자동 재시작. 서버 세션·연주자 상태는 그대로 두고(stop-performing을 보내지 않음) 엔진만 다시 연다 — 같은 clientId라
   * 다른 연주자는 새 descriptor로 다시 붙고, 관람 방송은 엔진이 다시 뜨면 방송 담당이 다시 offer한다.
   * 1) 같은 설정으로 다시 시도, 2) 장치를 지정했으면 마지막 시도는 기본 장치로(쓰던 장치가 사라졌을 수 있다), 3) 모두 실패하면 정지·안내.
   * 짧은 시간에 반복되면(장치가 계속 흔들림) 재시작하지 않고 정지한다.
   */
  private async recoverFromDeviceLoss(): Promise<void> {
    const last = this.lastStart
    if (this.recoveringDevice || !last || this.state.phase !== 'running') return
    const now = this.now()
    this.deviceLostTimes = this.deviceLostTimes.filter((t) => now - t < this.deviceLostWindowMs)
    this.deviceLostTimes.push(now)
    if (this.deviceLostTimes.length > this.deviceLostMaxRecoveries) {
      await this.stop({ notify: true, error: DEVICE_LOST_REPEAT_MESSAGE })
      return
    }

    this.recoveringDevice = true
    this.setDeviceNotice(DEVICE_RECOVERING_MESSAGE)
    // 참고(#208 리뷰 L-5): 기본 장치로 대체해 성공하면 lastStart가 기본 장치 설정이 된다 — 이후 재시작(ssrc/clientId)도
    // 지금 실제로 쓰는 장치로 연다(사라진 장치를 다시 고르지 않음). 사용자가 다시 시작하면 그때 설정을 쓴다
    const hasExplicitDevice = last.config.inputDevice !== null || last.config.outputDevice !== null
    let retriedForNewId = false
    for (let i = 0; i < this.deviceRecoveryDelays.length; i++) {
      const useDefault = hasExplicitDevice && i === this.deviceRecoveryDelays.length - 1
      const opts: StartOptions = useDefault ? { ...last, config: withDefaultDevices(last.config) } : last
      if (this.getState().phase === 'running') await this.stopForRestart() // getState: 위 guard의 좁힘을 피해 await 뒤 실제 값
      await new Promise((r) => setTimeout(r, this.deviceRecoveryDelays[i]))
      if (!this.recoveringDevice) return // 그 사이 사용자가 멈추거나 나갔다
      const idBefore = this.env.getSelfId()
      const ok = await this.restartWith(opts)
      if (!this.recoveringDevice) return
      // 시도 중에 clientId가 바뀌어(재연결) 버려진 시도는 세지 않고 같은 시도를 한 번 더 한다(새 id로)
      if (!ok && this.env.getSelfId() !== idBefore && !retriedForNewId) {
        retriedForNewId = true
        i--
        continue
      }
      if (ok) {
        this.recoveringDevice = false
        this.setDeviceNotice(useDefault ? DEVICE_RECOVERED_DEFAULT_MESSAGE : DEVICE_RECOVERED_MESSAGE, this.deviceNoticeMs)
        return
      }
    }
    // 모두 실패: 서버에 연주 중단을 알린다(재시작 앞 정지는 알리지 않았다)
    this.recoveringDevice = false
    this.hostedMetronomeForRestart = null
    this.setDeviceNotice(null)
    this.env.send({ type: 'native-leave' })
    this.env.send({ type: 'stop-performing' })
    this.setState({ phase: 'idle', instrument: null, sessionId: null, error: DEVICE_LOST_FAILED_MESSAGE })
  }

  private setDeviceNotice(notice: string | null, clearAfterMs?: number) {
    if (this.deviceNoticeTimer) clearTimeout(this.deviceNoticeTimer)
    this.deviceNoticeTimer = null
    if ((this.state.deviceNotice ?? null) !== notice) this.setState({ deviceNotice: notice })
    if (notice && clearAfterMs !== undefined) {
      this.deviceNoticeTimer = setTimeout(() => {
        this.deviceNoticeTimer = null
        if (this.state.deviceNotice === notice) this.setState({ deviceNotice: null })
      }, clearAfterMs)
    }
  }

  dispose(): void {
    if (this.deviceNoticeTimer) clearTimeout(this.deviceNoticeTimer)
    this.clearRetries()
    this.clearReconnectTimer()
    this.clearRelayTimer()
    if (this.state.phase !== 'idle') void this.stop({ notify: true })
  }

  // ---------------------------------------------------------------- 내부

  private runActions(actions: SessionAction[]) {
    for (const action of actions) {
      switch (action.kind) {
        case 'broadcast':
          this.env.send({ type: 'native-descriptor', descriptor: action.descriptor })
          break
        case 'sendTo':
          this.env.send({ type: 'native-descriptor', to: action.peerId, descriptor: action.descriptor })
          break
        case 'addPeer':
          this.addPeer(action.peerId, action.descriptor)
          break
        case 'removePeer':
          this.enqueue(() => this.engine.removePeer(action.peerId)).catch((err) => console.warn('[Native] remove_peer 실패:', errorText(err)))
          break
        case 'rejectPeer':
          console.warn('[Native] 최대 연주자 수 초과 — 자리가 나면 연결:', action.peerId.slice(0, 8))
          break
      }
    }
  }

  private addPeer(peerId: string, descriptor: LocalDescriptor) {
    const gen = this.gen
    this.enqueue(() => {
      // 엔진 firstAudioMs의 기준(engine_add_peer 호출 시각). 큐에서 실제로 부르는 순간에 잰다
      if (gen === this.gen && this.firstAudio) this.firstAudio = noteAddPeer(this.firstAudio, peerId, this.now())
      return this.engine.addPeer(peerId, descriptor)
    })
      .then(() => {
        if (gen !== this.gen) return
        this.machine.addPeerSucceeded(peerId)
        this.retryAttempts.delete(peerId)
      })
      .catch((err) => {
        if (gen !== this.gen) return
        console.warn(`[Native] add_peer(${peerId.slice(0, 8)}) 실패:`, errorText(err))
        this.machine.addPeerFailed(peerId)
        this.scheduleRetry(peerId)
      })
  }

  private scheduleRetry(peerId: string) {
    const attempt = this.retryAttempts.get(peerId) ?? 0
    if (attempt >= this.retryDelays.length) {
      this.retryAttempts.delete(peerId)
      return
    }
    this.retryAttempts.set(peerId, attempt + 1)
    this.clearRetry(peerId, false)
    this.retryTimers.set(peerId, setTimeout(() => {
      this.retryTimers.delete(peerId)
      this.runActions(this.machine.retryAddPeer(peerId))
    }, this.retryDelays[attempt]))
  }

  private clearRetry(peerId: string, resetAttempts = true) {
    const t = this.retryTimers.get(peerId)
    if (t) clearTimeout(t)
    this.retryTimers.delete(peerId)
    if (resetAttempts) this.retryAttempts.delete(peerId)
  }

  private clearRetries() {
    for (const t of this.retryTimers.values()) clearTimeout(t)
    this.retryTimers.clear()
    this.retryAttempts.clear()
  }

  private clearReconnectTimer() {
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer)
    this.reconnectTimer = null
  }
}

function errorText(err: unknown): string {
  if (typeof err === 'string') return err
  if (err instanceof Error) return err.message
  try {
    return JSON.stringify(err)
  } catch {
    return String(err)
  }
}
