// @vitest-environment jsdom
// P2-12 리뷰 M2: 토큰이 커서 우리 서버가 401인 사용자도 로그인하면 예전 base64 사진이 Supabase에서 직접 지워진다
import { renderHook, waitFor } from '@testing-library/react'
import type { User } from '@supabase/supabase-js'
import { afterEach, describe, expect, it, vi } from 'vitest'

const api = vi.hoisted(() => ({
  updateUserMetadata: vi.fn<(d: Record<string, unknown>) => Promise<void>>(async () => undefined),
  refreshSession: vi.fn<() => Promise<void>>(async () => undefined),
}))
vi.mock('./profileQueries', () => ({ profileApi: api }))

import { hasLegacyProfilePhoto, useLegacyProfilePhotoCleanup } from './legacyProfilePhoto'

const user = (id: string, meta: Record<string, unknown>) => ({ id, user_metadata: meta, app_metadata: {} }) as unknown as User
const BIG = `data:image/jpeg;base64,${'A'.repeat(20_000)}`

afterEach(() => vi.clearAllMocks())

describe('hasLegacyProfilePhoto', () => {
  it('profile_photo 값이 있으면 true', () => {
    expect(hasLegacyProfilePhoto({ profile_photo: BIG })).toBe(true)
    expect(hasLegacyProfilePhoto({ profile_photo: 'https://x/y.png' })).toBe(true)
    expect(hasLegacyProfilePhoto({ profile_photo: null })).toBe(false)
    expect(hasLegacyProfilePhoto({ nickname: 'n' })).toBe(false)
    expect(hasLegacyProfilePhoto(undefined)).toBe(false)
  })
})

describe('useLegacyProfilePhotoCleanup', () => {
  it('예전 사진이 있으면 Supabase user_metadata에서 profile_photo만 null로 지우고 새 토큰을 받는다 (사용자마다 한 번)', async () => {
    const { rerender } = renderHook(({ u }) => useLegacyProfilePhotoCleanup(u), { initialProps: { u: user('u1', { nickname: 'n', profile_photo: BIG }) as User | null } })
    await waitFor(() => expect(api.refreshSession).toHaveBeenCalledTimes(1))
    expect(api.updateUserMetadata).toHaveBeenCalledWith({ profile_photo: null })
    // 같은 사용자 객체가 다시 와도(토큰 갱신 전) 반복하지 않는다
    rerender({ u: user('u1', { nickname: 'n', profile_photo: BIG }) })
    rerender({ u: user('u1', { nickname: 'n' }) })
    expect(api.updateUserMetadata).toHaveBeenCalledTimes(1)
  })

  it('예전 사진이 없거나 로그아웃 상태면 아무것도 하지 않는다', () => {
    renderHook(() => useLegacyProfilePhotoCleanup(user('u2', { nickname: 'n' })))
    renderHook(() => useLegacyProfilePhotoCleanup(null))
    expect(api.updateUserMetadata).not.toHaveBeenCalled()
  })

  it('실패해도 던지지 않는다 (경고만)', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    api.updateUserMetadata.mockRejectedValueOnce(new Error('network'))
    renderHook(() => useLegacyProfilePhotoCleanup(user('u3', { profile_photo: BIG })))
    await waitFor(() => expect(warn).toHaveBeenCalled())
    expect(api.refreshSession).not.toHaveBeenCalled()
    warn.mockRestore()
  })
})
