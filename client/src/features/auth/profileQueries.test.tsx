// @vitest-environment jsdom
import { act, renderHook, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { argsOf, createSupabaseMock } from '../../test/supabaseMock'
import { createQueryWrapper } from '../../test/queryWrapper'

const sb = vi.hoisted(() => ({ mock: null as ReturnType<typeof createSupabaseMock> | null }))
vi.mock('../../lib/supabaseClient', async () => {
  const { createSupabaseMock } = await import('../../test/supabaseMock')
  sb.mock = createSupabaseMock()
  return { supabase: sb.mock.client, isSupabaseConfigured: () => true }
})

import { profileApi, profileKeys, useProfileNicknameQuery, useSaveProfileMutation } from './queries'

const form = { nickname: '새닉', bio: '', instagram: '', youtube: '', twitter: '' }

afterEach(() => {
  sb.mock!.reset()
  vi.clearAllMocks()
  vi.restoreAllMocks()
})

describe('profile queries', () => {
  it('enabled=false면 조회하지 않고, 켜지면 profiles.nickname을 읽는다', async () => {
    sb.mock!.setResult('profiles', { data: { nickname: '닉' } })
    const { wrapper } = createQueryWrapper()
    const { result, rerender } = renderHook(({ on }) => useProfileNicknameQuery('u1', on), {
      wrapper,
      initialProps: { on: false },
    })
    expect(sb.mock!.client.from).not.toHaveBeenCalled()

    rerender({ on: true })
    await waitFor(() => expect(result.current.data).toBe('닉'))
    expect(argsOf(sb.mock!.calls[0], 'eq')).toEqual([['id', 'u1']])
  })

  it('profiles 행이 없으면 오류 없이 null (maybeSingle, 재시도 없음)', async () => {
    sb.mock!.setResult('profiles', { data: null })
    const { wrapper } = createQueryWrapper()
    const { result } = renderHook(() => useProfileNicknameQuery('u1'), { wrapper })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))
    expect(result.current.data).toBeNull()
    expect(argsOf(sb.mock!.calls[0], 'maybeSingle')).toHaveLength(1)
    expect(sb.mock!.calls).toHaveLength(1)
  })

  it('저장: 메타데이터 갱신 후 profiles 갱신, 닉네임 캐시를 바꾼다', async () => {
    sb.mock!.setResult('profiles', { data: null })
    const { wrapper, client } = createQueryWrapper()
    const { result } = renderHook(() => useSaveProfileMutation(), { wrapper })

    await act(() => result.current.mutateAsync({ userId: 'u1', form }))

    expect(sb.mock!.client.auth.updateUser).toHaveBeenCalledWith({ data: form })
    expect(argsOf(sb.mock!.calls[0], 'update')).toEqual([[{ nickname: '새닉' }]])
    expect(client.getQueryData(profileKeys.nickname('u1'))).toBe('새닉')
  })

  it('profiles 갱신 실패는 로그만 남기고 성공으로 본다(기존 동작)', async () => {
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {})
    sb.mock!.setResult('profiles', { error: { message: 'rls' } })
    const { wrapper } = createQueryWrapper()
    const { result } = renderHook(() => useSaveProfileMutation(), { wrapper })

    await act(() => result.current.mutateAsync({ userId: 'u1', form }))
    expect(errors).toHaveBeenCalledWith('Failed to update profiles table:', { message: 'rls' })
  })

  it('메타데이터 갱신이 실패하면 throw하고 profiles는 건드리지 않는다', async () => {
    sb.mock!.client.auth.updateUser.mockResolvedValueOnce({ data: {}, error: { message: 'auth' } })
    const { wrapper } = createQueryWrapper()
    const { result } = renderHook(() => useSaveProfileMutation(), { wrapper })

    await expect(result.current.mutateAsync({ userId: 'u1', form })).rejects.toEqual({ message: 'auth' })
    expect(sb.mock!.client.from).not.toHaveBeenCalled()
  })
})

describe('profileApi (contexts에서 옮긴 profiles 접근)', () => {
  it('audio_settings 저장 실패는 throw', async () => {
    sb.mock!.setResult('profiles', { error: { message: 'rls' } })
    await expect(profileApi.saveAudioSettings('u1', { version: 2 })).rejects.toEqual({ message: 'rls' })
    expect(argsOf(sb.mock!.calls[0], 'update')).toEqual([[{ audio_settings: { version: 2 } }]])
    expect(argsOf(sb.mock!.calls[0], 'eq')).toEqual([['id', 'u1']])
  })

  it('가입 프로필 생성: 오류는 던지지 않고 돌려준다', async () => {
    sb.mock!.setResult('profiles', { data: null })
    await expect(profileApi.createProfile({ id: 'u1', nickname: 'n' })).resolves.toBeNull()
    expect(argsOf(sb.mock!.calls[0], 'insert')).toEqual([[{ id: 'u1', nickname: 'n' }]])

    sb.mock!.setResult('profiles', { error: { message: 'dup' } })
    await expect(profileApi.createProfile({ id: 'u1' })).resolves.toEqual({ message: 'dup' })
  })
})
