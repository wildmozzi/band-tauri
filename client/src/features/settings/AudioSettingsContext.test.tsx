// @vitest-environment jsdom
// AudioSettingsContext의 DB 로드/저장이 features/profile 훅으로 옮겨진 뒤에도 저장 게이트 규칙을 지키는지
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
})
