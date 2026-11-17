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

  it('데스크톱 앱에서 불러오기에 실패하면 null(엔진 없는 세션으로 떨어짐)이고 다음 preload가 다시 시도한다', async () => {
    native.app = true
    vi.doMock('./nativeRuntime', () => {
      throw new Error('청크 없음')
    })
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    const loader = await import('./runtimeLoader')
    await expect(loader.preloadNativeRuntime()).resolves.toBeNull()
    expect(error).toHaveBeenCalled()
    const { IdlePerformSession } = await import('./idleSession')
    expect(loader.createPerformSession(env)).toBeInstanceOf(IdlePerformSession)
    vi.doUnmock('./nativeRuntime')
    await expect(loader.preloadNativeRuntime()).resolves.not.toBeNull()
    error.mockRestore()
  })
})
