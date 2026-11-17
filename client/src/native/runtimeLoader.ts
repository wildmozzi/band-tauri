/**
 * 엔진 런타임 지연 로드 (웹 관람 묶음 분리).
 *
 * 연주 세션 본체·관람 방송(nativeRuntime)은 데스크톱 앱에서만 쓴다. 웹 관람 첫 묶음에서 빼려고 동적 import하고,
 * 데스크톱 앱은 첫 그리기 전에 preloadNativeRuntime()을 기다려(main.tsx) 합주실이 처음부터 진짜 세션을 갖는다 —
 * 연주 시작 경로·타이밍은 그대로다. 웹은 엔진 없는 세션(idleSession)을 쓴다.
 * 데스크톱 앱에서 불러오기에 실패했으면(로컬 묶음이라 드물다) 엔진 없는 세션으로 시작하되, 연주 시작 때 다시 불러와
 * 성공하면 그때부터 진짜 세션·방송으로 넘어간다(DeferredPerformSession·deferredMixBridge). 다시 실패하면 앱 재시작 안내.
 */
import { isNativeApp } from './engineApi'
import {
  IdleMixBroadcaster,
  IdleMixSource,
  IdlePerformSession,
  RUNTIME_LOAD_FAILED_MESSAGE,
  type MixBroadcasterApi,
  type MixSourceApi,
  type PerformSessionApi,
} from './idleSession'
import type { PerformState, SessionEnv, SignalOut, StartOptions } from './performSession'

type NativeRuntime = typeof import('./nativeRuntime')

let runtime: NativeRuntime | null = null
let loading: Promise<NativeRuntime | null> | null = null

/** 데스크톱 앱이면 엔진 런타임을 불러온다(한 번). 웹이면 아무것도 받지 않고 null */
export function preloadNativeRuntime(): Promise<NativeRuntime | null> {
  if (!isNativeApp()) return Promise.resolve(null)
  loading ??= import('./nativeRuntime').then(
    (m) => (runtime = m),
    (err) => {
      console.error('[Native] 엔진 런타임을 불러오지 못했습니다:', err)
      loading = null // 다음 preload가 다시 시도한다
      return null
    },
  )
  return loading
}

/** 지금 불러와 있는 엔진 런타임(데스크톱 앱에서 preload가 끝났을 때만) */
export function loadedNativeRuntime(): NativeRuntime | null {
  return runtime
}

/** 연주 세션: 데스크톱 앱이면 진짜 세션, 아니면 엔진 없는 세션 */
export function createPerformSession(env: SessionEnv): PerformSessionApi {
  if (runtime) return runtime.createPerformSession(env)
  return isNativeApp() ? new DeferredPerformSession(env) : new IdlePerformSession()
}

/**
 * 데스크톱 앱에서 런타임을 아직 못 불러온 세션. 그동안은 엔진 없는 세션(idle)이고, 연주 시작 때 런타임을 다시 불러와
 * 성공하면 진짜 세션을 만들어 그 뒤로는 모두 그쪽으로 넘긴다(구독자도 옮긴다). 실패하면 앱 재시작 안내로 끝난다.
 * 진짜 세션으로 넘어갈 때 idle 동안의 상태(오류·원격 메트로놈)는 버린다 — 연주를 시작하면 서버가 메트로놈을 다시 보낸다.
 */
export class DeferredPerformSession implements PerformSessionApi {
  private idle = new IdlePerformSession(RUNTIME_LOAD_FAILED_MESSAGE)
  private real: PerformSessionApi | null = null
  private listeners = new Set<(s: PerformState) => void>()

  constructor(private readonly env: SessionEnv) {
    this.idle.subscribe((st) => this.emit(st))
  }

  private get current(): PerformSessionApi {
    return this.real ?? this.idle
  }

  private emit(st: PerformState) {
    for (const l of this.listeners) l(st)
  }

  getState(): PerformState {
    return this.current.getState()
  }

  subscribe(listener: (s: PerformState) => void): () => void {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  async start(opts: StartOptions): Promise<boolean> {
    if (!this.real) {
      const rt = await preloadNativeRuntime()
      if (rt && !this.real) {
        this.real = rt.createPerformSession(this.env)
        this.real.subscribe((st) => this.emit(st))
        this.idle.dispose()
        this.emit(this.real.getState())
      }
    }
    return this.real ? this.real.start(opts) : this.idle.start()
  }

  stop(opts?: Parameters<PerformSessionApi['stop']>[0]): Promise<void> { return this.current.stop(opts) }
  updateInstrument(instrument: string): void { this.current.updateInstrument(instrument) }
  clearError(): void { this.current.clearError() }
  handleSignal(payload: Record<string, unknown>): boolean { return this.current.handleSignal(payload) }
  performerAppeared(peerId: string): void { this.current.performerAppeared(peerId) }
  peerGone(peerId: string): void { this.current.peerGone(peerId) }
  connectionLost(): void { this.current.connectionLost() }
  welcome(clientId: string, resumed: boolean): Promise<void> { return this.current.welcome(clientId, resumed) }
  setPeerGain(peerId: string, gain: number, muted: boolean, pan?: number): void { this.current.setPeerGain(peerId, gain, muted, pan) }
  setInputMuted(muted: boolean): void { this.current.setInputMuted(muted) }
  setHostMetronome(running: boolean, bpm: number, beatsPerBar: number): Promise<boolean> { return this.current.setHostMetronome(running, bpm, beatsPerBar) }
  pollStats(): Promise<void> { return this.current.pollStats() }
  dispose(): void { this.current.dispose() }
}

type MixBridge = { mixSource: MixSourceApi; broadcaster: MixBroadcasterApi }

/** 관람 방송(연주 중인 방송 담당 → 관람자). 엔진 런타임이 없으면 열리지 않는 빈 방송 */
export function createMixBridge(send: (payload: SignalOut) => void, onViewerCountChange: (connected: number) => void): MixBridge {
  if (runtime) return runtime.createMixBridge(send, onViewerCountChange)
  if (!isNativeApp()) return { mixSource: new IdleMixSource(), broadcaster: new IdleMixBroadcaster() }
  return deferredMixBridge(() => runtime?.createMixBridge(send, onViewerCountChange) ?? null)
}

/**
 * 데스크톱 앱에서 런타임을 아직 못 불러온 방송: 쓸 때마다 런타임을 확인해, 그사이 (연주 시작 때 다시) 불러왔으면 진짜 소스·방송을
 * 한 번 만들어 넘긴다. 방송·엔진 믹스 녹음은 연주 중(= 진짜 세션이 엔진을 연 뒤)에만 쓰이므로 그때는 늘 진짜다.
 */
function deferredMixBridge(make: () => MixBridge | null): MixBridge {
  const idle: MixBridge = { mixSource: new IdleMixSource(), broadcaster: new IdleMixBroadcaster() }
  let real: MixBridge | null = null
  const pick = (): MixBridge => (real ??= make()) ?? idle
  return {
    mixSource: {
      acquire: () => pick().mixSource.acquire(),
      invalidate: () => pick().mixSource.invalidate(),
      resume: () => pick().mixSource.resume(),
    },
    broadcaster: {
      start: (viewers) => pick().broadcaster.start(viewers),
      stop: () => pick().broadcaster.stop(),
      syncViewers: (viewers) => pick().broadcaster.syncViewers(viewers),
      handleAnswer: (from, answer) => pick().broadcaster.handleAnswer(from, answer),
      handleCandidate: (from, candidate) => pick().broadcaster.handleCandidate(from, candidate),
      hasPeer: (viewerId) => pick().broadcaster.hasPeer(viewerId),
      onRelayRateLimited: () => pick().broadcaster.onRelayRateLimited(),
    },
  }
}

/** 테스트 전용: 불러온 런타임을 잊는다 */
export function resetNativeRuntimeForTest(): void {
  runtime = null
  loading = null
}
