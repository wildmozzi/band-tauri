// @vitest-environment jsdom
// AudioSettingsContext의 DB 로드/저장이 features/auth의 프로필 query 훅으로 옮겨진 뒤에도 저장 게이트 규칙을 지키는지
import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { argsOf, createSupabaseMock } from '../../shared/test/supabaseMock'
import { createQueryWrapper } from '../../shared/test/queryWrapper'

const sb = vi.hoisted(() => ({ mock: null as ReturnType<typeof createSupabaseMock> | null }))
vi.mock('../../shared/lib/supabaseClient', async () => {
  const { createSupabaseMock } = await import('../../shared/test/supabaseMock')
  sb.mock = createSupabaseMock()
  return { supabase: sb.mock.client, isSupabaseConfigured: () => true }
})

const auth = vi.hoisted(() => ({ user: { id: 'u1' } as { id: string } | null }))
vi.mock('../auth/useAuth', () => ({ useAuth: () => ({ user: auth.user }) }))

import type { DeviceInfo, DriverKind, EngineConfig } from '../../native/engineApi'

// 데스크톱 앱 흉내(P2-27): 장치 목록·지원 드라이버·테스트 모니터 시작만 가짜로. 기본은 웹(native.on=false)이라 위 시험에는 영향 없음
const native = vi.hoisted(() => ({
  on: false,
  devices: [] as DeviceInfo[],
  supported: null as DriverKind[] | null,
  testStarts: [] as EngineConfig[],
  /** engine_list_devices 호출 수(정상 경로에 추가 IPC가 없는지) */
  listCalls: 0,
}))
vi.mock('../../native/engineApi', async (importOriginal) => {
  const orig = await importOriginal<typeof import('../../native/engineApi')>()
  return {
    ...orig,
    isNativeApp: () => native.on,
    engineListDevices: async () => {
      native.listCalls += 1
      return native.devices
    },
    engineSupportedDrivers: async () => native.supported,
    engineTestStart: async (c: EngineConfig) => {
      native.testStarts.push(c)
      return null
    },
    engineTestStop: async () => null,
    engineStats: async () => null,
  }
})
vi.mock('../../native/loopbackOffsetStore', () => ({ reapplyLoopbackOffsetInBackground: () => {} }))

import { AudioSettingsProvider, useAudioSettings } from './AudioSettingsContext'

function setup() {
  const { wrapper: QueryWrapper, client } = createQueryWrapper()
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryWrapper><AudioSettingsProvider>{children}</AudioSettingsProvider></QueryWrapper>
  )
  return { client, ...renderHook(() => useAudioSettings(), { wrapper }) }
}

const updates = () => sb.mock!.calls.filter(c => c.chain.some(([m]) => m === 'update'))

beforeEach(() => {
  localStorage.clear()
  vi.spyOn(console, 'warn').mockImplementation(() => {})
  vi.spyOn(console, 'log').mockImplementation(() => {})
})

afterEach(() => {
  cleanup()
  sb.mock!.reset()
  vi.useRealTimers()
  vi.clearAllMocks()
  vi.restoreAllMocks()
  auth.user = { id: 'u1' }
  native.on = false
  native.devices = []
  native.supported = null
  native.testStarts = []
})

describe('AudioSettingsContext + profiles.audio_settings', () => {
  it('로그인 사용자의 v2 설정을 한 번 읽어 적용하고, 변환 저장은 하지 않는다', async () => {
    sb.mock!.setResult('profiles', { data: { audio_settings: { version: 2, engine: { bufferSize: 256 } } } })
    const { result } = setup()

    await waitFor(() => expect(result.current.config.bufferSize).toBe(256))
    const read = sb.mock!.calls[0]
    expect(argsOf(read, 'select')).toEqual([['audio_settings']])
    expect(argsOf(read, 'eq')).toEqual([['id', 'u1']])
    expect(argsOf(read, 'single')).toHaveLength(1)
    expect(updates()).toHaveLength(0)
    expect(result.current.migrated).toBe(false)
  })

  it('구형 설정이면 변환해서 다시 저장하고 migrated를 켠다', async () => {
    sb.mock!.setResult('profiles', { data: { audio_settings: { buffer_size: 512 } } })
    const { result } = setup()

    await waitFor(() => expect(result.current.migrated).toBe(true))
    await waitFor(() => expect(updates()).toHaveLength(1))
    const payload = argsOf(updates()[0], 'update')[0][0] as { audio_settings: { version: number } }
    expect(payload.audio_settings.version).toBe(2)
  })

  it('읽은 뒤 사용자가 바꾸면 디바운스 후 DB에 저장한다', async () => {
    sb.mock!.setResult('profiles', { data: { audio_settings: null } })
    const { result } = setup()
    await waitFor(() => expect(sb.mock!.calls).toHaveLength(1))
    // 로드가 끝나 게이트가 열릴 때까지 한 틱
    await act(async () => {})

    act(() => { result.current.updateConfig({ jitterMaxMs: 60 }) })
    await waitFor(() => expect(updates()).toHaveLength(1), { timeout: 2000 })
    const payload = argsOf(updates()[0], 'update')[0][0] as { audio_settings: { engine: { jitterMaxMs: number } } }
    expect(payload.audio_settings.engine.jitterMaxMs).toBe(60)
    expect(argsOf(updates()[0], 'eq')).toEqual([['id', 'u1']])
  })

  it('DB 읽기에 실패하면(행 없음 포함) 그 사용자 동안 DB에 저장하지 않는다 (재시도도 없음)', async () => {
    sb.mock!.setResult('profiles', { error: { message: 'no rows' } })
    const { result } = setup()
    await waitFor(() => expect(sb.mock!.calls).toHaveLength(1))
    await act(async () => {})

    act(() => { result.current.updateConfig({ jitterMaxMs: 60 }) })
    await act(() => new Promise(r => setTimeout(r, 800)))
    expect(updates()).toHaveLength(0)
    expect(sb.mock!.calls).toHaveLength(1)
    expect(result.current.config.jitterMaxMs).toBe(60) // 로컬에는 반영
  })

  it('비로그인이면 DB를 읽지 않는다', async () => {
    auth.user = null
    setup()
    await act(async () => {})
    expect(sb.mock!.client.from).not.toHaveBeenCalled()
  })

  it('다른 PC에서 로그인: 서버에 옛 threadPolicy가 있어도 이 PC의 기본값(절전 끄기 켬·P 코어 끔)으로 시작하고, 다음 저장 때 서버에서 뺀다(#245 재리뷰 L4)', async () => {
    sb.mock!.setResult('profiles', { data: { audio_settings: { version: 2, engine: { bufferSize: 256, threadPolicy: { highQos: false, preferPCores: true } } } } })
    const { result } = setup()
    await waitFor(() => expect(result.current.config.bufferSize).toBe(256))
    await act(async () => {})
    expect(result.current.config.threadPolicy).toEqual({ highQos: true, preferPCores: false })

    act(() => { result.current.updateConfig({ jitterMaxMs: 60 }) })
    await waitFor(() => expect(updates()).toHaveLength(1), { timeout: 2000 })
    const payload = argsOf(updates()[0], 'update')[0][0] as { audio_settings: { engine: Record<string, unknown> } }
    expect(payload.audio_settings.engine.jitterMaxMs).toBe(60)
    expect('threadPolicy' in payload.audio_settings.engine).toBe(false)
  })

  it('이 PC에서 켠 값은 기기 로컬에만 저장되고 계정 로드 뒤에도 유지된다', async () => {
    localStorage.setItem('band_thread_policy_v2', JSON.stringify({ highQos: false, preferPCores: true }))
    sb.mock!.setResult('profiles', { data: { audio_settings: { version: 2, engine: { bufferSize: 256 } } } })
    const { result } = setup()
    await waitFor(() => expect(result.current.config.bufferSize).toBe(256))
    await act(async () => {})
    expect(result.current.config.threadPolicy).toEqual({ highQos: false, preferPCores: true })

    act(() => { result.current.updateConfig({ threadPolicy: { highQos: true, preferPCores: true } }) })
    await waitFor(() => expect(updates()).toHaveLength(1), { timeout: 2000 })
    expect(JSON.parse(localStorage.getItem('band_thread_policy_v2')!)).toEqual({ highQos: true, preferPCores: true })
    const cache = JSON.parse(localStorage.getItem('band_engine_config_v2')!) as { engine: Record<string, unknown> }
    expect('threadPolicy' in cache.engine).toBe(false)
    const payload = argsOf(updates()[0], 'update')[0][0] as { audio_settings: { engine: Record<string, unknown> } }
    expect('threadPolicy' in payload.audio_settings.engine).toBe(false)
  })
})

const wasapi = (id: string, isInput: boolean, channels = 2): DeviceInfo => ({
  id, name: id.slice('wasapi::'.length), driver: 'wasapiShared', isInput, isDefault: true, channels, sampleRates: [48000],
})
/** 이 PC: 내장 마이크·스피커만(저장된 인터페이스 없음) */
const LAPTOP = [wasapi('wasapi::마이크 배열', true, 1), wasapi('wasapi::스피커', false)]
type Prepared = Awaited<ReturnType<ReturnType<typeof useAudioSettings>['prepareRunConfig']>>

describe('저장된 장치가 이 PC에 없을 때(P2-27): 이번 실행만 기본 장치, 저장값은 그대로', () => {
  const SAVED = { version: 2, engine: { bufferSize: 256, inputDevice: 'wasapi::Focusrite USB', outputDevice: 'wasapi::Focusrite USB', channels: 2, inputChannels: [2, 3] } }

  it('계정에서 받은 장치 id가 없으면 엔진에는 기본 장치로 넘기고, config·계정·로컬 캐시에는 원래 id를 둔다', async () => {
    native.on = true
    native.devices = LAPTOP
    native.supported = ['wasapiExclusive', 'wasapiShared']
    sb.mock!.setResult('profiles', { data: { audio_settings: SAVED } })
    const { result } = setup()
    await waitFor(() => expect(result.current.config.bufferSize).toBe(256))
    await waitFor(() => expect(result.current.devices).toHaveLength(2))
    await act(async () => {})

    // 설정 화면: 저장값 그대로 + 시작 전 안내
    expect(result.current.config.inputDevice).toBe('wasapi::Focusrite USB')
    await waitFor(() => expect(result.current.missingDevices).toEqual({ input: 'wasapi::Focusrite USB', output: 'wasapi::Focusrite USB' }))
    await waitFor(() => expect(result.current.fallbackNotice).toContain("'Focusrite USB'를 찾을 수 없어 기본 장치로 시작합니다"))

    // 엔진 시작 직전: 이번 실행 설정
    let run: Prepared | null = null
    await act(async () => { run = await result.current.prepareRunConfig() })
    expect(run!.issues).toEqual([])
    expect(run!.config).toMatchObject({ inputDevice: null, outputDevice: null, inputChannels: [0, 0], bufferSize: 256 })
    expect(run!.notice).toContain('기본 장치로 시작했습니다')
    // 몇 번 알릴지는 부르는 쪽(합주실: 입장마다, 테스트 모니터: 앱 실행마다)이 정한다 — 여기서는 매번 돌려준다(#251 리뷰 Low-5)
    await act(async () => { run = await result.current.prepareRunConfig() })
    expect(run!.notice).toContain('기본 장치로 시작했습니다')

    // 테스트 모니터도 고친 사본으로 시작
    await act(async () => { await result.current.startTest() })
    expect(result.current.testError).toBeNull()
    expect(native.testStarts[native.testStarts.length - 1]).toMatchObject({ inputDevice: null, outputDevice: null })
    await act(async () => { await result.current.stopTest() })

    // 폴백은 저장하지 않는다: DB 쓰기 없음, 로컬 캐시·상태는 원래 장치
    await act(() => new Promise((r) => setTimeout(r, 800)))
    expect(updates()).toHaveLength(0)
    const cache = JSON.parse(localStorage.getItem('band_engine_config_v2')!) as { engine: EngineConfig }
    expect(cache.engine.inputDevice).toBe('wasapi::Focusrite USB')
    expect(result.current.config.inputChannels).toEqual([2, 3])

    // 사용자가 다른 값을 바꿔 저장해도 장치는 원래 값으로 올라간다(원래 PC에서 그대로 동작)
    act(() => { result.current.updateConfig({ jitterMaxMs: 60 }) })
    await waitFor(() => expect(updates()).toHaveLength(1), { timeout: 2000 })
    const payload = argsOf(updates()[0], 'update')[0][0] as { audio_settings: { engine: EngineConfig } }
    expect(payload.audio_settings.engine).toMatchObject({ inputDevice: 'wasapi::Focusrite USB', outputDevice: 'wasapi::Focusrite USB', inputChannels: [2, 3], jitterMaxMs: 60 })
  })

  it('이 배포본에 없는 드라이버(ASIO)도 저장값은 두고 이번 실행만 WASAPI 공유로 — 다음 편집 때 ASIO가 지워지지 않는다', async () => {
    native.on = true
    native.devices = LAPTOP
    native.supported = ['wasapiExclusive', 'wasapiShared']
    sb.mock!.setResult('profiles', { data: { audio_settings: { version: 2, engine: { bufferSize: 64, inputDriver: 'asio', outputDriver: 'asio', inputDevice: 'asio::X', outputDevice: 'asio::X' } } } })
    const { result } = setup()
    await waitFor(() => expect(result.current.config.bufferSize).toBe(64))
    await waitFor(() => expect(result.current.supportedDrivers).not.toBeNull())
    await act(async () => {})
    expect(result.current.config.inputDriver).toBe('asio')
    await waitFor(() => expect(result.current.fallbackNotice).toContain('WASAPI 공유 기본 장치로 시작합니다'))

    // 장치 select는 "기본 장치"가 아니라 저장된 ASIO 장치를 "(이 PC에 없음)"으로 보인다(#251 리뷰 Low-3)
    expect(result.current.missingDevices).toEqual({ input: 'asio::X', output: 'asio::X' })

    // 드라이버 폴백을 먼저 적용해 판단하므로 시작할 때마다 장치 목록을 다시 받지 않는다(#251 리뷰 Low-2)
    const before = native.listCalls
    let run: Prepared | null = null
    await act(async () => { run = await result.current.prepareRunConfig() })
    expect(run!.config).toMatchObject({ inputDriver: 'wasapiShared', outputDriver: 'wasapiShared', inputDevice: null, outputDevice: null })
    expect(native.listCalls).toBe(before)

    act(() => { result.current.updateConfig({ jitterMaxMs: 60 }) })
    await waitFor(() => expect(updates()).toHaveLength(1), { timeout: 2000 })
    const payload = argsOf(updates()[0], 'update')[0][0] as { audio_settings: { engine: EngineConfig } }
    expect(payload.audio_settings.engine).toMatchObject({ inputDriver: 'asio', outputDriver: 'asio', inputDevice: 'asio::X' })
  })

  it('정상 경로(저장된 장치가 모두 있음): 저장값 객체 그대로, 장치 목록 추가 조회 없음', async () => {
    native.on = true
    native.devices = [...LAPTOP, { ...wasapi('wasapi::Focusrite USB', true, 4), isDefault: false }, { ...wasapi('wasapi::Focusrite USB', false), isDefault: false }]
    native.supported = ['wasapiExclusive', 'wasapiShared']
    localStorage.setItem('band_engine_config_v2', JSON.stringify(SAVED))
    auth.user = null
    const { result } = setup()
    await waitFor(() => expect(result.current.devices).toHaveLength(4))
    await waitFor(() => expect(result.current.supportedDrivers).not.toBeNull())
    await act(async () => {})
    expect(result.current.missingDevices).toEqual({ input: null, output: null })
    const before = native.listCalls
    let run: Prepared | null = null
    await act(async () => { run = await result.current.prepareRunConfig() })
    expect(run!.config).toBe(result.current.config)
    expect(run!.notice).toBeNull()
    expect(run!.issues).toEqual([])
    expect(native.listCalls).toBe(before)
  })

  it('저장값이 "기본 장치"인데 지금 기본 입력 장치의 채널이 모자라도 시작한다 — 입력 채널을 맞추고 저장값은 그대로(#251 리뷰 Low-4)', async () => {
    native.on = true
    native.devices = LAPTOP
    native.supported = ['wasapiExclusive', 'wasapiShared']
    localStorage.setItem('band_engine_config_v2', JSON.stringify({ version: 2, engine: { ...SAVED.engine, inputDevice: null, outputDevice: null } }))
    auth.user = null
    const { result } = setup()
    await waitFor(() => expect(result.current.devices).toHaveLength(2))
    await act(async () => {})
    // 저장값 검증으로는 막히지만(설정 화면 표시) 엔진 시작은 이번 실행 설정으로 검증한다
    expect(result.current.issues[0].message).toContain('입력 장치는 1채널까지만 지원합니다')
    await act(async () => { await result.current.startTest() })
    expect(result.current.testError).toBeNull()
    expect(native.testStarts[native.testStarts.length - 1]).toMatchObject({ inputDevice: null, inputChannels: [0, 0] })
    expect(result.current.fallbackNotice).toContain('입력 채널은 Ch1·Ch1로 맞춥니다.')
    expect(result.current.config.inputChannels).toEqual([2, 3])
    await act(async () => { await result.current.stopTest() })
  })

  it('입력 장치가 하나도 없으면 시작하지 않고 원인을 말한다', async () => {
    native.on = true
    native.devices = [wasapi('wasapi::스피커', false)]
    auth.user = null
    const { result } = setup()
    await waitFor(() => expect(result.current.devices).toHaveLength(1))
    let run: Prepared | null = null
    await act(async () => { run = await result.current.prepareRunConfig() })
    expect(run!.issues[0].message).toContain('입력 장치(WASAPI 공유)를 찾지 못했습니다')
    expect(run!.notice).toBeNull()
    await act(async () => { await result.current.startTest() })
    expect(result.current.testError).toContain('입력 장치(WASAPI 공유)를 찾지 못했습니다')
    expect(native.testStarts).toHaveLength(0)
  })

  it('앱을 켠 뒤 인터페이스를 꽂았으면 시작 전에 목록을 다시 받아 저장된 장치로 시작한다', async () => {
    native.on = true
    native.devices = LAPTOP
    localStorage.setItem('band_engine_config_v2', JSON.stringify(SAVED))
    auth.user = null
    const { result } = setup()
    await waitFor(() => expect(result.current.devices).toHaveLength(2))
    native.devices = [...LAPTOP, { ...wasapi('wasapi::Focusrite USB', true, 4), isDefault: false }, { ...wasapi('wasapi::Focusrite USB', false), isDefault: false }]
    let run: Prepared | null = null
    await act(async () => { run = await result.current.prepareRunConfig() })
    expect(run!.notice).toBeNull()
    expect(run!.config).toMatchObject({ inputDevice: 'wasapi::Focusrite USB', inputChannels: [2, 3] })
  })
})

describe('기기별 스레드 정책: 계정 전환·저장소 예외(#249 리뷰 N2)', () => {
  it('같은 PC에서 계정을 바꿔도 이 기기의 값은 유지되고, 새 계정에도 올라가지 않는다', async () => {
    // 이 PC에서 사용자가 절전 끄기를 명시적으로 끈 상태(기본값과 다른 값이라 유지 여부가 드러난다)
    localStorage.setItem('band_thread_policy_v2', JSON.stringify({ highQos: false, preferPCores: false }))
    sb.mock!.setResult('profiles', { data: { audio_settings: { version: 2, engine: { bufferSize: 256, threadPolicy: { highQos: true, preferPCores: true } } } } })
    const { result, rerender } = setup()
    await waitFor(() => expect(result.current.config.bufferSize).toBe(256))
    expect(result.current.config.threadPolicy).toEqual({ highQos: false, preferPCores: false })

    auth.user = { id: 'u2' }
    sb.mock!.setResult('profiles', { data: { audio_settings: { version: 2, engine: { bufferSize: 512, threadPolicy: { highQos: true, preferPCores: true } } } } })
    rerender()
    await waitFor(() => expect(result.current.config.bufferSize).toBe(512))
    await act(async () => {})
    expect(result.current.config.threadPolicy).toEqual({ highQos: false, preferPCores: false })

    act(() => { result.current.updateConfig({ jitterMaxMs: 60 }) })
    await waitFor(() => expect(updates()).toHaveLength(1), { timeout: 2000 })
    expect(argsOf(updates()[0], 'eq')).toEqual([['id', 'u2']])
    const payload = argsOf(updates()[0], 'update')[0][0] as { audio_settings: { engine: Record<string, unknown> } }
    expect('threadPolicy' in payload.audio_settings.engine).toBe(false)
  })

  it('localStorage 읽기·쓰기가 던져도 기본값(절전 끄기 켬·P 코어 끔)으로 시작하고 설정 변경은 동작한다', async () => {
    auth.user = null
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('denied') })
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('denied') })
    const { result } = setup()
    expect(result.current.config.threadPolicy).toEqual({ highQos: true, preferPCores: false })
    expect(result.current.config.bufferSize).toBe(128)
    act(() => { result.current.updateConfig({ threadPolicy: { highQos: false, preferPCores: false } }) })
    expect(result.current.config.threadPolicy).toEqual({ highQos: false, preferPCores: false })
  })

  it('저장값이 없는 PC: 절전 끄기 켬으로 시작하고, 사용자가 끄면 이 PC에 끈 값이 남는다(P1-21 3단계)', async () => {
    auth.user = null
    const first = setup()
    expect(first.result.current.config.threadPolicy).toEqual({ highQos: true, preferPCores: false })
    act(() => { first.result.current.updateConfig({ threadPolicy: { highQos: false, preferPCores: false } }) })
    await waitFor(() => expect(JSON.parse(localStorage.getItem('band_thread_policy_v2') ?? 'null')).toEqual({ highQos: false, preferPCores: false }))
    first.unmount()
    // 앱을 다시 켠 것: 끈 값을 존중한다
    expect(setup().result.current.config.threadPolicy).toEqual({ highQos: false, preferPCores: false })
  })
})
