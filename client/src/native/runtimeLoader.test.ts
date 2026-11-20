/**
 * 엔진 런타임 지연 로드(웹 관람 묶음 분리): 데스크톱 앱은 preload 뒤 진짜 세션·방송, 웹은 받지 않고 엔진 없는 것.
 */
import { afterEach, describe, expect, it, vi } from 'vitest'

const native = vi.hoisted(() => ({ app: false }))
vi.mock('./engineApi', async (importOriginal) => ({
  ...(await importOriginal<object>()),
  isNativeApp: () => native.app,
}))

const env = { send: () => {}, getSelfId: () => 'me', getRoomId: () => null, getOtherPerformers: () => [] }

describe('runtimeLoader', () => {
  afterEach(async () => {
    native.app = false
    ;(await import('./runtimeLoader')).resetNativeRuntimeForTest()
    vi.doUnmock('./nativeRuntime')
    vi.resetModules()
  })

  it('웹: 엔진 런타임을 받지 않고 엔진 없는 세션·방송을 만든다', async () => {
    const loader = await import('./runtimeLoader')
    const { IdleMixBroadcaster, IdleMixSource, IdlePerformSession } = await import('./idleSession')
    await expect(loader.preloadNativeRuntime()).resolves.toBeNull()
    expect(loader.loadedNativeRuntime()).toBeNull()
    expect(loader.createPerformSession(env)).toBeInstanceOf(IdlePerformSession)
    const bridge = loader.createMixBridge(() => {}, () => {})
    expect(bridge.mixSource).toBeInstanceOf(IdleMixSource)
    expect(bridge.broadcaster).toBeInstanceOf(IdleMixBroadcaster)
  })

  it('데스크톱 앱: preload 뒤에는 진짜 세션(PerformSession)과 방송(MixBroadcaster)을 만든다. preload는 한 번만 받는다', async () => {
    native.app = true
    const loader = await import('./runtimeLoader')
    const first = loader.preloadNativeRuntime()
    expect(loader.preloadNativeRuntime()).toBe(first)
    const rt = await first
    expect(rt).not.toBeNull()
    const { PerformSession } = await import('./performSession')
    const { MixBroadcaster, MixSource } = await import('./mixBridge')
    expect(loader.createPerformSession(env)).toBeInstanceOf(PerformSession)
    const bridge = loader.createMixBridge(() => {}, () => {})
    expect(bridge.mixSource).toBeInstanceOf(MixSource)
    expect(bridge.broadcaster).toBeInstanceOf(MixBroadcaster)
  })

  it('데스크톱 앱에서 불러오기에 실패하면 엔진 없는 세션으로 시작하고, 연주 시작이 다시 불러와 성공하면 진짜 세션으로 넘어간다', async () => {
    native.app = true
    vi.doMock('./nativeRuntime', () => {
      throw new Error('청크 없음')
    })
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    const loader = await import('./runtimeLoader')
    await expect(loader.preloadNativeRuntime()).resolves.toBeNull()
    expect(error).toHaveBeenCalled()
    const { RUNTIME_LOAD_FAILED_MESSAGE } = await import('./idleSession')
    const session = loader.createPerformSession(env)
    expect(session).toBeInstanceOf(loader.DeferredPerformSession)
    const bridge = loader.createMixBridge(() => {}, () => {})
    const seen: string[] = []
    session.subscribe((st) => seen.push(`${st.phase}:${st.error ?? ''}`))
    const opts = { instrument: 'guitar', config: {} as never, nickname: 'n', isHost: false }

    // 다시 불러와도 실패: 앱 재시작 안내(웹 문구 아님)
    await expect(session.start(opts)).resolves.toBe(false)
    expect(session.getState().error).toBe(RUNTIME_LOAD_FAILED_MESSAGE)
    await expect(bridge.mixSource.acquire()).rejects.toThrow('믹스 구독 실패')

    // 다시 불러와 성공: 그때부터 진짜 세션(가짜 런타임)이 연주를 시작하고, 구독자도 그쪽 상태를 받는다. 방송도 진짜로 넘어간다
    const realStart = vi.fn(async () => true)
    const realState = { phase: 'running', instrument: 'guitar', sessionId: 'me', error: null, stats: null, inputMuted: false }
    const realAcquire = vi.fn(async () => ({ stream: 'mix', release: () => {} }))
    vi.doMock('./nativeRuntime', () => ({
      createPerformSession: () => ({ getState: () => realState, subscribe: () => () => {}, start: realStart, handleSignal: () => true }),
      createMixBridge: () => ({ mixSource: { acquire: realAcquire }, broadcaster: {} }),
    }))
    await expect(session.start(opts)).resolves.toBe(true)
    expect(realStart).toHaveBeenCalledWith(opts)
    expect(session.getState()).toBe(realState)
    expect(seen[seen.length - 1]).toBe('running:')
    expect(session.handleSignal({ type: 'native-descriptor' })).toBe(true)
    await expect(bridge.mixSource.acquire()).resolves.toMatchObject({ stream: 'mix' })
    expect(realAcquire).toHaveBeenCalledOnce()
    error.mockRestore()
  })
})
