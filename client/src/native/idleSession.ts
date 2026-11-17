/**
 * 엔진 없는 연주 세션·관람 방송 (웹 브라우저용, 가벼움).
 *
 * 웹에서는 연주(엔진)를 시작할 수 없어 PerformSession은 늘 idle이다. 그래서 웹 관람 첫 묶음에는 엔진 세션 본체
 * (PerformSession·sessionLogic·mixBridge, 지연 로드 — runtimeLoader)를 싣지 않고, 시작한 적 없는 PerformSession과
 * 똑같이 동작하는 이 클래스를 쓴다. 같은 동작인지는 idleSession.test.ts가 같은 입력을 둘 다에 넣어 비교한다.
 * 데스크톱 앱이 엔진 런타임을 불러오지 못했을 때(그럴 일은 없어야 한다)는 runtimeLoader가 이것을 감싸 연주 시작 때 다시 불러온다.
 */
import type { MixBroadcaster, MixLease, MixSource } from './mixBridge'
import { metronomeErrorFromCode, parseMetronomeSignal, type MetronomeSignalState } from './metronomeLogic'
import { DESCRIPTOR_INVALID_MESSAGE, performErrorFromCode } from './performSignals'
import type { PerformSession, PerformState } from './performSession'

/** 합주실(RoomProvider)이 쓰는 연주 세션 표면 — PerformSession과 IdlePerformSession이 같이 맞춘다 */
export type PerformSessionApi = Pick<
  PerformSession,
  | 'getState'
  | 'subscribe'
  | 'start'
  | 'stop'
  | 'updateInstrument'
  | 'clearError'
  | 'handleSignal'
  | 'performerAppeared'
  | 'peerGone'
  | 'connectionLost'
  | 'welcome'
  | 'setPeerGain'
  | 'setInputMuted'
  | 'setHostMetronome'
  | 'pollStats'
  | 'dispose'
>
export type MixSourceApi = Pick<MixSource, 'acquire' | 'invalidate' | 'resume'>
export type MixBroadcasterApi = Pick<MixBroadcaster, 'start' | 'stop' | 'syncViewers' | 'handleAnswer' | 'handleCandidate' | 'hasPeer'>

export const NO_ENGINE_MESSAGE = '오디오 엔진 시작 실패: 이 환경에서는 오디오 엔진을 쓸 수 없습니다(데스크톱 앱 전용).'
/** 데스크톱 앱인데 엔진 런타임 묶음을 불러오지 못했다(연주 시작 때 다시 불러와도 실패) */
export const RUNTIME_LOAD_FAILED_MESSAGE = '오디오 엔진 시작 실패: 엔진 모듈을 불러오지 못했습니다. 다시 시도하고, 계속되면 앱을 다시 시작해주세요.'

export class IdlePerformSession implements PerformSessionApi {
  private state: PerformState = { phase: 'idle', instrument: null, sessionId: null, error: null, stats: null, inputMuted: false }
  private listeners = new Set<(s: PerformState) => void>()
  private remoteMetronome: { from: string; state: MetronomeSignalState } | null = null

  /** startError: 연주 시작이 끝나는 오류 문구(웹은 NO_ENGINE_MESSAGE, 데스크톱 로드 실패는 RUNTIME_LOAD_FAILED_MESSAGE) */
  constructor(private readonly startError: string = NO_ENGINE_MESSAGE) {}

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

  async start(): Promise<boolean> {
    this.setState({ error: this.startError })
    return false
  }

  async stop(opts: { notify?: boolean; error?: string | null; forRestart?: boolean } = {}): Promise<void> {
    if (opts.error) this.setState({ error: opts.error })
  }

  updateInstrument(): void {}

  clearError(): void {
    if (this.state.error) this.setState({ error: null })
  }

  /** PerformSession.handleSignal의 idle 경로와 같다 */
  handleSignal(payload: Record<string, unknown>): boolean {
    const from = typeof payload.from === 'string' ? payload.from : null
    if (payload.type === 'native-descriptor' || payload.type === 'relay-token') return true
    if (payload.type === 'native-leave') {
      if (from) this.peerGone(from)
      return true
    }
    if (payload.type === 'metronome') {
      const msg = parseMetronomeSignal(payload)
      if (!msg) return true
      this.remoteMetronome = msg
      this.setState({
        metronome: msg.state.running ? { running: true, bpm: msg.state.bpm, beatsPerBar: msg.state.beatsPerBar, leaderId: msg.from } : null,
      })
      return true
    }
    if (payload.type !== 'error') return false
    const metroError = metronomeErrorFromCode(payload.code, payload.message)
    if (metroError) {
      // 내 메트로놈(leaderId null)은 연주 중에만 생기므로(setHostMetronome) 여기서는 문구만
      this.setState({ error: metroError })
      return true
    }
    if (payload.code === 'ssrc-conflict' || payload.code === 'key-conflict') return true
    if (payload.code === 'descriptor-invalid') {
      this.setState({ error: DESCRIPTOR_INVALID_MESSAGE })
      return true
    }
    const mapped = performErrorFromCode(payload.code, payload.message)
    if (!mapped) return false
    this.setState({ error: mapped.message })
    return true
  }

  performerAppeared(): void {}

  peerGone(peerId: string): void {
    if (this.remoteMetronome?.from === peerId) {
      this.remoteMetronome = null
      this.setState({ metronome: null })
    }
  }

  connectionLost(): void {}

  async welcome(): Promise<void> {}

  setPeerGain(): void {}

  setInputMuted(muted: boolean): void {
    this.setState({ inputMuted: muted })
  }

  async setHostMetronome(): Promise<boolean> {
    return false
  }

  async pollStats(): Promise<void> {}

  dispose(): void {}
}

/** 엔진 없는 믹스 소스: 열 수 없다(웹은 연주 중이 아니라 엔진 믹스로 녹음·방송하지 않는다) */
export class IdleMixSource implements MixSourceApi {
  async acquire(): Promise<MixLease> {
    throw new Error('믹스 구독 실패: 오디오 엔진이 없습니다(데스크톱 앱 전용).')
  }
  invalidate(): void {}
  resume(): void {}
}

/** 엔진 없는 관람 방송: 방송 담당은 연주자(데스크톱 앱)만 된다 */
export class IdleMixBroadcaster implements MixBroadcasterApi {
  async start(): Promise<void> {
    throw new Error('믹스 구독 실패: 오디오 엔진이 없습니다(데스크톱 앱 전용).')
  }
  stop(): void {}
  syncViewers(): void {}
  async handleAnswer(): Promise<void> {}
  async handleCandidate(): Promise<void> {}
  hasPeer(): boolean {
    return false
  }
}
