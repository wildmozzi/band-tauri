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
  /** engine_stats가 돌려줄 값(테스트 모니터 폴링) */
  stats: null as { deviceReset?: boolean } | null,
  /** engine_test_start를 이 횟수만큼 실패시킨다 */
  testStartFailures: 0,
  testStops: 0,
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
      if (native.testStartFailures > 0) {
        native.testStartFailures -= 1
        throw new Error('ASIO 장치를 찾을 수 없습니다')
      }
      native.stats = null // 새 엔진은 리셋 요청이 없는 상태로 시작한다
      return null
    },
    engineTestStop: async () => {
      native.testStops += 1
      return null
    },
    engineStats: async () => native.stats,
  }
})
vi.mock('../../native/loopbackOffsetStore', () => ({ reapplyLoopbackOffsetInBackground: () => {} }))

import { AudioSettingsProvider, useAudioSettings } from './AudioSettingsContext'
import { TEST_RESET_RESTARTED_MESSAGE, TEST_RESET_RESTARTING_MESSAGE } from '../../native/performSignals'

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
  native.stats = null
  native.testStartFailures = 0
  native.testStops = 0
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

  it('계정에서 받은(이행) 장치 id가 없으면 엔진에는 기본 장치로 넘기고, config·이 PC 저장값에는 원래 id를 둔다', async () => {
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

    // 폴백은 저장하지 않는다: DB 쓰기 없음, 이 PC 저장값·상태는 원래 장치(계정에서 한 번 가져온 값 — P2-27 후속 이행)
    await act(() => new Promise((r) => setTimeout(r, 800)))
    expect(updates()).toHaveLength(0)
    const local = JSON.parse(localStorage.getItem('band_device_settings_v1')!) as EngineConfig
    expect(local).toMatchObject({ inputDevice: 'wasapi::Focusrite USB', inputChannels: [2, 3], bufferSize: 256 })
    expect(result.current.config.inputChannels).toEqual([2, 3])

    // 사용자가 다른 값을 바꿔 저장해도 장치 쪽 값은 계정에 올리지 않는다(PC별) — 이 PC 저장값은 원래 장치 그대로
    act(() => { result.current.updateConfig({ jitterMaxMs: 60 }) })
    await waitFor(() => expect(updates()).toHaveLength(1), { timeout: 2000 })
    const payload = argsOf(updates()[0], 'update')[0][0] as { audio_settings: { engine: Record<string, unknown> } }
    expect(payload.audio_settings.engine.jitterMaxMs).toBe(60)
    for (const k of ['inputDevice', 'outputDevice', 'inputDriver', 'outputDriver', 'inputChannels', 'bufferSize']) expect(k in payload.audio_settings.engine).toBe(false)
    expect(JSON.parse(localStorage.getItem('band_device_settings_v1')!)).toMatchObject({ inputDevice: 'wasapi::Focusrite USB', outputDevice: 'wasapi::Focusrite USB' })
  })

  it('이 배포본에 없는 드라이버(ASIO)도 저장값은 두고 이번 실행만 WASAPI 공유로 — 다음 편집 때 이 PC의 ASIO가 지워지지 않는다', async () => {
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
    expect(JSON.parse(localStorage.getItem('band_device_settings_v1')!)).toMatchObject({ inputDriver: 'asio', outputDriver: 'asio', inputDevice: 'asio::X' })
    expect(result.current.config).toMatchObject({ inputDriver: 'asio', inputDevice: 'asio::X' })
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

  describe('테스트 모니터의 드라이버 리셋 요청(#261 재리뷰 L4)', () => {
  it('deviceReset이면 같은 설정으로 다시 열고 알린다 — 첫 시도가 실패해도 같은 장치로 다시 시도한다', async () => {
    native.on = true
    native.devices = LAPTOP
    native.supported = ['wasapiExclusive', 'wasapiShared']
    localStorage.setItem('band_engine_config_v2', JSON.stringify({ version: 2, engine: { ...SAVED.engine, inputDevice: null, outputDevice: null, channels: 1, inputChannels: [0] } }))
    auth.user = null
    const { result } = setup()
    await waitFor(() => expect(result.current.devices).toHaveLength(2))
    await act(async () => { await result.current.startTest() })
    expect(result.current.isTesting).toBe(true)
    const first = native.testStarts[native.testStarts.length - 1]
    const startsBefore = native.testStarts.length
    native.testStartFailures = 1
    native.stats = { deviceReset: true }
    await waitFor(() => expect(result.current.fallbackNotice).toBe(TEST_RESET_RESTARTING_MESSAGE))
    await waitFor(() => expect(result.current.fallbackNotice).toBe(TEST_RESET_RESTARTED_MESSAGE), { timeout: 4000 })
    // 정지 1번, 시작 2번(실패 1 + 성공 1) — 둘 다 처음과 같은 설정
    expect(native.testStops).toBe(1)
    expect(native.testStarts.slice(startsBefore)).toEqual([first, first])
    expect(result.current.isTesting).toBe(true)
    expect(result.current.testError).toBeNull()
    await act(async () => { await result.current.stopTest() })
  })
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
    sb.mock!.setResult('profiles', { data: { audio_settings: { version: 2, engine: { bufferSize: 512, jitterMaxMs: 120, threadPolicy: { highQos: true, preferPCores: true } } } } })
    rerender()
    await waitFor(() => expect(result.current.config.jitterMaxMs).toBe(120))
    expect(result.current.config.bufferSize).toBe(256) // 버퍼는 이 PC의 값(P2-27 후속)
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

describe('장치 설정은 PC별 저장(P2-27 후속): 저장 분리·이행·덮어쓰지 않음', () => {
  const DEVICE_KEY = 'band_device_settings_v1'
  const HERE = { inputDriver: 'wasapiExclusive', outputDriver: 'wasapiExclusive', inputDevice: 'wasapi::이 PC 마이크', outputDevice: 'wasapi::이 PC 스피커', inputChannels: [1], bufferSize: 64 }
  const OTHER_PC_ACCOUNT = {
    version: 2,
    engine: { inputDriver: 'asio', outputDriver: 'asio', inputDevice: 'asio::다른 PC', outputDevice: 'asio::다른 PC', inputChannels: [4], bufferSize: 512, codec: 'pcm16', frameMs: 2.5, jitterMaxMs: 40 },
  }
  const localDevice = () => JSON.parse(localStorage.getItem(DEVICE_KEY) ?? 'null') as Record<string, unknown> | null
  const DEVICE_FIELDS = ['inputDevice', 'outputDevice', 'inputDriver', 'outputDriver', 'inputChannels', 'bufferSize']

  it('이 PC에 저장값이 있으면 계정의 장치 값이 있어도 이 PC 값을 쓰고, 코덱·프레임·지터는 계정 값을 쓴다', async () => {
    localStorage.setItem(DEVICE_KEY, JSON.stringify(HERE))
    sb.mock!.setResult('profiles', { data: { audio_settings: OTHER_PC_ACCOUNT } })
    const { result } = setup()
    await waitFor(() => expect(result.current.config.codec).toBe('pcm16'))
    await act(async () => {})
    expect(result.current.config).toMatchObject({ ...HERE, frameMs: 2.5, jitterMaxMs: 40 })
    expect(localDevice()).toMatchObject(HERE)
    expect(updates()).toHaveLength(0)
  })

  it('저장값 없는 새 PC: 계정의 옛 장치 값을 한 번 초기값으로 가져와 이 PC에 저장하고, 이후엔 계정 값으로 덮어쓰지 않는다', async () => {
    sb.mock!.setResult('profiles', { data: { audio_settings: OTHER_PC_ACCOUNT } })
    const first = setup()
    await waitFor(() => expect(first.result.current.config.inputDevice).toBe('asio::다른 PC'))
    await act(async () => {})
    expect(localDevice()).toMatchObject({ inputDriver: 'asio', inputDevice: 'asio::다른 PC', bufferSize: 512 })

    // 이 PC에서 장치를 바꾼다 → 이 PC에만 저장, 계정에는 장치 필드가 올라가지 않는다
    act(() => { first.result.current.updateConfig({ inputDriver: 'wasapiShared', inputDevice: 'wasapi::이 PC 마이크', bufferSize: 128 }) })
    await waitFor(() => expect(updates()).toHaveLength(1), { timeout: 2000 })
    const payload = argsOf(updates()[0], 'update')[0][0] as { audio_settings: { engine: Record<string, unknown> } }
    for (const k of DEVICE_FIELDS) expect(k in payload.audio_settings.engine).toBe(false)
    expect(payload.audio_settings.engine).toMatchObject({ codec: 'pcm16', frameMs: 2.5 })
    expect(localDevice()).toMatchObject({ inputDriver: 'wasapiShared', inputDevice: 'wasapi::이 PC 마이크', bufferSize: 128 })
    cleanup()
    sb.mock!.reset()

    // 앱을 다시 켜도(계정에 옛 장치 값이 그대로 남아 있어도) 이 PC 값
    sb.mock!.setResult('profiles', { data: { audio_settings: OTHER_PC_ACCOUNT } })
    const second = setup()
    await waitFor(() => expect(sb.mock!.calls).toHaveLength(1))
    await act(async () => {})
    expect(second.result.current.config).toMatchObject({ inputDriver: 'wasapiShared', inputDevice: 'wasapi::이 PC 마이크', bufferSize: 128, codec: 'pcm16' })
  })

  it('계정에 장치 값도 없는 새 PC: 기본 장치로 시작하고 그 값을 이 PC 값으로 굳힌다', async () => {
    sb.mock!.setResult('profiles', { data: { audio_settings: { version: 2, engine: { codec: 'pcm16' } } } })
    const { result } = setup()
    await waitFor(() => expect(result.current.config.codec).toBe('pcm16'))
    await act(async () => {})
    expect(result.current.config).toMatchObject({ inputDevice: null, outputDevice: null, inputDriver: 'wasapiShared', bufferSize: 128 })
    expect(localDevice()).toMatchObject({ inputDevice: null, inputDriver: 'wasapiShared', bufferSize: 128 })
  })

  it('계정 설정이 아예 없는(새 계정) 경우에도 지금 장치 값을 이 PC 값으로 굳힌다', async () => {
    sb.mock!.setResult('profiles', { data: { audio_settings: null } })
    setup()
    await waitFor(() => expect(sb.mock!.calls).toHaveLength(1))
    await waitFor(() => expect(localDevice()).toMatchObject({ inputDevice: null, bufferSize: 128 }))
  })

  it('같은 PC에서 계정을 바꿔 로그인해도 장치 설정은 그대로(PC의 것), 새 계정의 코덱·지터만 적용', async () => {
    sb.mock!.setResult('profiles', { data: { audio_settings: { version: 2, engine: { ...HERE, jitterMaxMs: 60 } } } })
    const { result, rerender } = setup()
    await waitFor(() => expect(result.current.config.inputDevice).toBe('wasapi::이 PC 마이크'))
    await act(async () => {})

    auth.user = { id: 'u2' }
    sb.mock!.setResult('profiles', { data: { audio_settings: OTHER_PC_ACCOUNT } })
    rerender()
    await waitFor(() => expect(result.current.config.codec).toBe('pcm16'))
    await act(async () => {})
    expect(result.current.config).toMatchObject({ ...HERE, jitterMaxMs: 40, frameMs: 2.5 })
    expect(localDevice()).toMatchObject(HERE)
  })

  it('이행: 이 PC의 옛 캐시(이 PC 장치)와 계정(다른 PC 장치)이 다르면 캐시가 이긴다 — 업데이트 직후 다른 PC 장치로 바뀌지 않는다(#256 리뷰 M1)', async () => {
    // 옛 앱: 이 PC 캐시 = 이 PC에서 마지막으로 고른 장치, 계정 = 다른 PC에서 마지막으로 저장한 장치
    localStorage.setItem('band_engine_config_v2', JSON.stringify({ version: 2, engine: { ...HERE, codec: 'opus', jitterMaxMs: 80 } }))
    sb.mock!.setResult('profiles', { data: { audio_settings: OTHER_PC_ACCOUNT } })
    const { result } = setup()
    expect(result.current.config).toMatchObject(HERE) // 계정을 읽기 전
    await waitFor(() => expect(result.current.config.codec).toBe('pcm16'))
    await act(async () => {})
    // 계정을 읽은 뒤에도 장치 쪽은 이 PC 캐시 값, 나머지는 계정 값
    expect(result.current.config).toMatchObject({ ...HERE, frameMs: 2.5, jitterMaxMs: 40 })
    expect(localDevice()).toMatchObject(HERE)
    const cache = JSON.parse(localStorage.getItem('band_engine_config_v2')!) as { engine: Record<string, unknown> }
    for (const k of DEVICE_FIELDS) expect(k in cache.engine).toBe(false)
    cleanup()
    sb.mock!.reset()

    // 다시 켜도(캐시에서 장치 필드가 빠진 뒤) 이 PC 저장값 그대로
    sb.mock!.setResult('profiles', { data: { audio_settings: OTHER_PC_ACCOUNT } })
    const second = setup()
    await waitFor(() => expect(second.result.current.config.codec).toBe('pcm16'))
    await act(async () => {})
    expect(second.result.current.config).toMatchObject(HERE)
  })

  it('장치 저장값 읽기가 던지면(사생활 모드 등) 죽지 않고, 캐시도 못 읽으니 계정의 옛 장치 값을 이번 실행에 쓴다(#256 리뷰 L3)', async () => {
    const realGet = Storage.prototype.getItem
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(function (this: Storage, key: string) {
      if (key === DEVICE_KEY || key === 'band_engine_config_v2') throw new Error('denied')
      return realGet.call(this, key)
    })
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('denied') })
    sb.mock!.setResult('profiles', { data: { audio_settings: OTHER_PC_ACCOUNT } })
    const { result } = setup()
    await waitFor(() => expect(result.current.config.codec).toBe('pcm16'))
    await act(async () => {})
    expect(result.current.config).toMatchObject({ inputDriver: 'asio', inputDevice: 'asio::다른 PC', bufferSize: 512, jitterMaxMs: 40 })
    // 편집도 동작하고 계정에는 장치 필드가 올라가지 않는다
    act(() => { result.current.updateConfig({ inputDevice: 'wasapi::이 PC 마이크', inputDriver: 'wasapiShared' }) })
    await waitFor(() => expect(updates()).toHaveLength(1), { timeout: 2000 })
    const payload = argsOf(updates()[0], 'update')[0][0] as { audio_settings: { engine: Record<string, unknown> } }
    for (const k of DEVICE_FIELDS) expect(k in payload.audio_settings.engine).toBe(false)
    expect(result.current.config).toMatchObject({ inputDriver: 'wasapiShared', inputDevice: 'wasapi::이 PC 마이크' })
  })

  it('옛 로컬 캐시(장치 값 포함)만 있고 계정을 못 읽으면 그 값을 임시로 쓰고, 사용자가 바꾸면 이 PC에 저장한다', async () => {
    localStorage.setItem('band_engine_config_v2', JSON.stringify({ version: 2, engine: { ...HERE, codec: 'pcm16' } }))
    auth.user = null
    const { result } = setup()
    await act(async () => {})
    expect(result.current.config).toMatchObject({ ...HERE, codec: 'pcm16' })
    expect(localDevice()).toBeNull()

    act(() => { result.current.updateConfig({ jitterMaxMs: 70 }) })
    await act(async () => {})
    expect(localDevice()).toMatchObject(HERE)
    const cache = JSON.parse(localStorage.getItem('band_engine_config_v2')!) as { engine: Record<string, unknown> }
    expect('inputDevice' in cache.engine).toBe(false)
    expect(cache.engine).toMatchObject({ codec: 'pcm16', jitterMaxMs: 70 })
  })
})
