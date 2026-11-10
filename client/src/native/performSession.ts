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
 * - 릴레이(P1-4b): 서버 relay-token(내 ssrc)이 오면 engine_set_relay, 만료 30초 전에 relay-token-request.
 *   서버가 ssrc-conflict를 보내면 엔진을 새 ssrc로 다시 시작한다.
 */
import type { EngineConfig, EngineStats, LocalDescriptor, RelayConfig } from './engineApi'
import { parseRelayToken, relayRefreshDelayMs } from './relayLogic'
import { NativeSignalMachine, isValidDescriptor, type SessionAction } from './sessionLogic'

export interface EngineBackend {
  start(config: EngineConfig, peerId: string): Promise<LocalDescriptor>
  stop(): Promise<unknown>
  addPeer(peerId: string, descriptor: LocalDescriptor): Promise<unknown>
  /** 릴레이 설정/토큰 갱신 (engine_set_relay) */
  setRelay(relay: RelayConfig | null): Promise<unknown>
  removePeer(peerId: string): Promise<unknown>
  setPeerGain(peerId: string, gain: number, muted: boolean, pan?: number): Promise<unknown>
  setInputMuted(muted: boolean): Promise<unknown>
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

export type PerformPhase = 'idle' | 'starting' | 'running'

export interface PerformState {
  phase: PerformPhase
  instrument: string | null
  /** 엔진을 시작할 때 쓴 clientId */
  sessionId: string | null
  error: string | null
  stats: EngineStats | null
  inputMuted: boolean
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
}

/** 서버 에러 코드 → 사용자 메시지. 연주 상태 롤백 대상이면 rollback=true */
export function performErrorFromCode(code: unknown, serverMessage?: unknown): { message: string; rollback: boolean } | null {
  const msg = typeof serverMessage === 'string' && serverMessage ? serverMessage : null
  switch (code) {
    case 'perform-not-approved':
      return { message: msg ?? '방장의 승인이 필요합니다. 연주 참여를 요청해주세요.', rollback: true }
    case 'room-full-performers':
      return { message: msg ?? '연주자 자리가 가득 찼습니다.', rollback: true }
    case 'login-required':
      return { message: msg ?? '연주하려면 로그인이 필요합니다.', rollback: true }
    default:
      return null
  }
}

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
  private deadStats = 0
  private readonly retryDelays: number[]
  private readonly reconnectTimeoutMs: number
  private readonly deadStatsThreshold: number
  private readonly relayRetryMs: number

  constructor(private readonly engine: EngineBackend, private readonly env: SessionEnv, opts: PerformSessionOptions = {}) {
    this.retryDelays = opts.retryDelaysMs ?? DEFAULT_RETRY_DELAYS
    this.reconnectTimeoutMs = opts.reconnectTimeoutMs ?? 35_000
    this.deadStatsThreshold = opts.deadStatsThreshold ?? 2
    this.relayRetryMs = opts.relayRetryMs ?? 10_000
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

  async start(opts: StartOptions): Promise<boolean> {
    if (this.state.phase === 'running') return true
    if (this.state.phase === 'starting') return false
    const selfId = this.env.getSelfId()
    const roomId = this.env.getRoomId()
    if (!selfId || !roomId) {
      this.setState({ error: '합주실에 연결되지 않았습니다.' })
      return false
    }

    const gen = ++this.gen
    this.lastStart = opts
    this.setState({ phase: 'starting', error: null, instrument: opts.instrument, sessionId: selfId })

    // 시작·검사·확정을 한 큐 작업 안에서 끝낸다. 그래야 사이에 끼어든 stop/재시작과 순서가 꼬이지 않는다.
    return this.enqueue(async () => {
      let descriptor: LocalDescriptor
      try {
        descriptor = await this.engine.start(opts.config, selfId)
      } catch (err) {
        if (gen === this.gen) this.setState({ phase: 'idle', instrument: null, sessionId: null, error: `오디오 엔진 시작 실패: ${errorText(err)}` })
        return false
      }

      // 시작하는 동안 정지/퇴장/재연결이 있었으면 방금 연 엔진을 다시 닫는다 (H2)
      const stale = gen !== this.gen || this.env.getSelfId() !== selfId || this.env.getRoomId() !== roomId
      if (stale || !isValidDescriptor(descriptor)) {
        await this.engine.stop().catch(() => {})
        if (gen === this.gen) {
          this.gen++
          this.setState({
            phase: 'idle',
            instrument: null,
            sessionId: null,
            error: stale ? null : '엔진이 올바르지 않은 연결 정보를 반환했습니다.',
          })
        }
        return false
      }

      this.deadStats = 0
      this.localSsrc = descriptor.ssrc
      this.setState({ phase: 'running' })
      // 엔진이 뜬 뒤에 연주 시작 알림 → descriptor 브로드캐스트 (서버가 연주자로 인정해야 중계함)
      this.env.send({ type: 'start-performing', instrument: opts.instrument, nickname: opts.nickname, isHost: opts.isHost })
      this.runActions(this.machine.started(selfId, descriptor, this.env.getOtherPerformers()))
      if (this.state.inputMuted) this.enqueue(() => this.engine.setInputMuted(true)).catch(() => {})
      return true
    })
  }

  /** notify=true면 native-leave + stop-performing 전송 */
  async stop(opts: { notify?: boolean; error?: string | null } = {}): Promise<void> {
    const { notify = true, error = null } = opts
    const wasRunning = this.state.phase === 'running'
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
    this.setState({ phase: 'idle', instrument: null, sessionId: null, stats: null, error })
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
    if (payload.type === 'error' && payload.code === 'ssrc-conflict') {
      // 같은 방 다른 연주자가 내 ssrc를 쓴다: 서버는 descriptor를 중계하지 않았다 → 새 ssrc로 재시작
      if (this.state.phase === 'running') void this.restart()
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
    if (resumed && clientId === this.state.sessionId && this.state.phase === 'running') {
      this.runActions(this.machine.reannounce(this.env.getOtherPerformers()))
      return
    }
    await this.restart()
  }

  /** 서버 쪽 세션은 그대로 두고 엔진만 새로 시작(새 clientId 또는 새 ssrc) */
  private async restart(): Promise<void> {
    const last = this.lastStart
    await this.stop({ notify: false })
    if (last) await this.start(last)
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
    this.enqueue(() => (gen === this.gen ? this.engine.setRelay({ addr: msg.addr, token: msg.token }) : Promise.resolve(null)))
      .catch((err) => console.warn('[Native] 릴레이 설정 실패:', errorText(err)))
    this.scheduleRelayRefresh(relayRefreshDelayMs(msg.expiresAt, Date.now()))
  }

  /** 갱신 요청 후 응답이 없으면(WS 순단 등) 다시 요청한다. 새 토큰이 오면 타이머가 다시 잡힌다 */
  private scheduleRelayRefresh(delayMs: number) {
    this.clearRelayTimer()
    this.relayTimer = setTimeout(() => {
      this.relayTimer = null
      if (this.state.phase !== 'running') return
      this.env.send({ type: 'relay-token-request' })
      this.scheduleRelayRefresh(this.relayRetryMs)
    }, delayMs)
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

  /** 주기 호출(500ms). engine_stats.running=false가 이어지면 엔진이 죽은 것으로 보고 정지 (L7) */
  async pollStats(): Promise<void> {
    if (this.state.phase !== 'running') return
    const gen = this.gen
    let stats: EngineStats
    try {
      stats = await this.engine.stats()
    } catch {
      return
    }
    if (gen !== this.gen || this.state.phase !== 'running') return
    if (!stats.running) {
      this.deadStats++
      if (this.deadStats >= this.deadStatsThreshold) {
        await this.stop({ notify: true, error: '오디오 엔진이 멈췄습니다 (장치 분리 등). 연주를 다시 시작해주세요.' })
      }
      return
    }
    this.deadStats = 0
    this.setState({ stats })
  }

  dispose(): void {
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
    this.enqueue(() => this.engine.addPeer(peerId, descriptor))
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
