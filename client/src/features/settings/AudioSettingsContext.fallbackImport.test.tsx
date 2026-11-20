// @vitest-environment jsdom
// 장치 폴백 모듈(deviceFallback, 지연 로드 청크)을 불러오지 못해도 엔진 시작이 막히지 않는다(#251 리뷰 Low-1):
// 저장된 설정 그대로(폴백 전 동작) 시작하고 콘솔에 경고를 남긴다.
import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createQueryWrapper } from '../../shared/test/queryWrapper'
import type { DeviceInfo, EngineConfig } from '../../native/engineApi'

vi.mock('../../shared/lib/supabaseClient', async () => {
  const { createSupabaseMock } = await import('../../shared/test/supabaseMock')
  return { supabase: createSupabaseMock().client, isSupabaseConfigured: () => true }
})
vi.mock('../auth/useAuth', () => ({ useAuth: () => ({ user: null }) }))

const native = vi.hoisted(() => ({ testStarts: [] as EngineConfig[] }))
const MIC: DeviceInfo = { id: 'wasapi::마이크', name: '마이크', driver: 'wasapiShared', isInput: true, isDefault: true, channels: 2, sampleRates: [48000] }
const SPK: DeviceInfo = { id: 'wasapi::스피커', name: '스피커', driver: 'wasapiShared', isInput: false, isDefault: true, channels: 2, sampleRates: [48000] }
vi.mock('../../native/engineApi', async (importOriginal) => {
  const orig = await importOriginal<typeof import('../../native/engineApi')>()
  return {
    ...orig,
    isNativeApp: () => true,
    engineListDevices: async () => [MIC, SPK],
    engineSupportedDrivers: async () => ['wasapiExclusive', 'wasapiShared'],
    engineTestStart: async (c: EngineConfig) => {
      native.testStarts.push(c)
      return null
    },
    engineTestStop: async () => null,
    engineStats: async () => null,
  }
})
vi.mock('../../native/loopbackOffsetStore', () => ({ reapplyLoopbackOffsetInBackground: () => {} }))
// 청크 로드 실패 흉내: 이 모듈을 import하면 던진다
vi.mock('../../native/deviceFallback', () => {
  throw new Error('Failed to fetch dynamically imported module')
})

import { AudioSettingsProvider, useAudioSettings } from './AudioSettingsContext'

function setup() {
  const { wrapper: QueryWrapper } = createQueryWrapper()
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryWrapper><AudioSettingsProvider>{children}</AudioSettingsProvider></QueryWrapper>
  )
  return renderHook(() => useAudioSettings(), { wrapper })
}

let warn: ReturnType<typeof vi.spyOn>
beforeEach(() => {
  localStorage.clear()
  warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
  vi.spyOn(console, 'log').mockImplementation(() => {})
})
afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
  native.testStarts = []
})

describe('장치 폴백 모듈 로드 실패(#251 리뷰 Low-1)', () => {
  it('저장된 설정 그대로 시작하고(알림 없음) 콘솔에 경고한다', async () => {
    const { result } = setup()
    await waitFor(() => expect(result.current.devices).toHaveLength(2))
    let run: Awaited<ReturnType<typeof result.current.prepareRunConfig>> | null = null
    await act(async () => { run = await result.current.prepareRunConfig() })
    expect(run!.config).toBe(result.current.config)
    expect(run!.notice).toBeNull()
    expect(run!.issues).toEqual([])
    expect(warn.mock.calls.some((c: unknown[]) => String(c[0]).includes('저장된 설정 그대로 시작합니다'))).toBe(true)

    // 테스트 모니터도 막히지 않는다
    await act(async () => { await result.current.startTest() })
    expect(result.current.testError).toBeNull()
    expect(native.testStarts).toHaveLength(1)
    expect(native.testStarts[0]).toBe(result.current.config)
    await act(async () => { await result.current.stopTest() })
  })

  it('저장값에 문제가 있으면 그 문제로 막는다(폴백 전과 같은 검증)', async () => {
    localStorage.setItem('band_engine_config_v2', JSON.stringify({ version: 2, engine: { channels: 2, inputChannels: [2, 3] } }))
    const { result } = setup()
    await waitFor(() => expect(result.current.devices).toHaveLength(2))
    let run: Awaited<ReturnType<typeof result.current.prepareRunConfig>> | null = null
    await act(async () => { run = await result.current.prepareRunConfig() })
    expect(run!.issues[0].message).toContain('입력 장치는 2채널까지만 지원합니다')
  })
})
