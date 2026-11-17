// @vitest-environment jsdom
// Profile에서 분리한 '내 정보' 폼 훅: 닉네임 출처(user_metadata → profiles), 저장·취소
import { act, renderHook } from '@testing-library/react'
import type { User } from '@supabase/supabase-js'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const auth = vi.hoisted(() => ({
  profileNickname: undefined as string | undefined,
  nicknameQuery: vi.fn(),
  saveProfile: vi.fn<(v: unknown) => Promise<void>>(async () => undefined),
  syncMetadata: vi.fn(),
  updateMetadata: vi.fn<(v: unknown) => Promise<void>>(async () => undefined),
  refreshSession: vi.fn<() => Promise<void>>(async () => undefined),
}))
const avatar = vi.hoisted(() => ({ upload: vi.fn<(f: File) => Promise<string>>(async () => 'https://api.example/api/storage/avatars/u1/1-x.webp') }))
vi.mock('./avatarApi', async (orig) => ({ ...(await orig<typeof import('./avatarApi')>()), uploadAvatar: avatar.upload }))
vi.mock('../auth', () => ({
  useProfileNicknameQuery: (userId: string | undefined, enabled: boolean) => {
    auth.nicknameQuery(userId, enabled)
    return { data: enabled ? auth.profileNickname : undefined }
  },
  useSaveProfileMutation: () => ({ isPending: false, mutateAsync: auth.saveProfile }),
  useUpdateUserMetadataMutation: () => ({ mutate: auth.syncMetadata, mutateAsync: auth.updateMetadata }),
  profileApi: { refreshSession: auth.refreshSession },
}))
vi.mock('../../shared/lib/supabaseClient', () => ({ isSupabaseConfigured: () => true }))

import { useProfileForm } from './useProfileForm'

const makeUser = (meta: Record<string, unknown>) => ({ id: 'u1', email: 'a@b.c', user_metadata: meta }) as unknown as User

beforeEach(() => {
  vi.spyOn(window, 'alert').mockImplementation(() => {})
})
afterEach(() => {
  vi.restoreAllMocks()
  vi.clearAllMocks()
  auth.profileNickname = undefined
})

describe('useProfileForm', () => {
  it('user_metadata 닉네임이 있으면 그것을 쓰고 profiles는 조회하지 않는다', () => {
    const { result } = renderHook(() => useProfileForm(makeUser({ nickname: '메타닉', bio: '소개', instagram: 'ig' })))
    expect(result.current.nickname).toBe('메타닉')
    expect(result.current.bio).toBe('소개')
    expect(result.current.snsLinks).toEqual({ instagram: 'ig', youtube: '', twitter: '' })
    expect(auth.nicknameQuery).toHaveBeenCalledWith('u1', false)
    expect(auth.syncMetadata).not.toHaveBeenCalled()
  })

  it('없으면 profiles 닉네임을 쓰고 user_metadata에도 맞춘다', () => {
    auth.profileNickname = '테이블닉'
    const { result } = renderHook(() => useProfileForm(makeUser({})))
    expect(result.current.nickname).toBe('테이블닉')
    expect(auth.syncMetadata).toHaveBeenCalledWith({ nickname: '테이블닉' })
  })

  it('저장하면 폼을 보내고 편집을 끝낸다, 취소하면 원래 값으로', async () => {
    const { result } = renderHook(() => useProfileForm(makeUser({ nickname: '원래', bio: '원래 소개' })))
    act(() => {
      result.current.setIsEditing(true)
      result.current.setNickname('새닉')
      result.current.setBio('새 소개')
    })
    act(() => result.current.handleCancel())
    expect(result.current.nickname).toBe('원래')
    expect(result.current.bio).toBe('원래 소개')
    expect(result.current.isEditing).toBe(false)

    act(() => {
      result.current.setIsEditing(true)
      result.current.setNickname('새닉')
    })
    await act(() => result.current.handleSave())
    expect(auth.saveProfile).toHaveBeenCalledWith({
      userId: 'u1',
      form: { nickname: '새닉', bio: '원래 소개', instagram: '', youtube: '', twitter: '' },
    })
    expect(result.current.isEditing).toBe(false)
    // 저장한 닉네임이 새 기준: 취소해도 유지
    act(() => result.current.handleCancel())
    expect(result.current.nickname).toBe('새닉')
  })

  const pick = (file: File) => ({ target: { files: [file], value: 'C:/fake/' + file.name } }) as unknown as React.ChangeEvent<HTMLInputElement>

  it('사진 변경: Storage 업로드 후 서버가 준 URL을 쓰고 세션을 새로 받는다 — metadata에 사진을 직접 넣지 않는다 (P2-12)', async () => {
    const { result } = renderHook(() => useProfileForm(makeUser({ nickname: 'n', profile_photo: 'data:image/png;base64,OLD' })))
    expect(result.current.profilePhoto).toBe('')
    const ev = pick(new File([new Uint8Array(8)], 'me.webp', { type: 'image/webp' }))
    await act(() => result.current.handlePhotoChange(ev))
    expect(avatar.upload).toHaveBeenCalledTimes(1)
    expect(result.current.profilePhoto).toBe('https://api.example/api/storage/avatars/u1/1-x.webp')
    expect(auth.refreshSession).toHaveBeenCalledTimes(1)
    expect(auth.updateMetadata).not.toHaveBeenCalled()
    expect(auth.syncMetadata).not.toHaveBeenCalled()
    expect(ev.target.value).toBe('')
    expect(result.current.isUploadingPhoto).toBe(false)
  })

  it('형식이 맞지 않으면 올리지 않고 알린다, 업로드 실패도 알린다', async () => {
    const { result } = renderHook(() => useProfileForm(makeUser({ nickname: 'n' })))
    await act(() => result.current.handlePhotoChange(pick(new File(['GIF89a'], 'a.gif', { type: 'image/gif' }))))
    expect(avatar.upload).not.toHaveBeenCalled()
    expect(window.alert).toHaveBeenCalledWith(expect.stringMatching(/jpg, png, webp/))

    avatar.upload.mockRejectedValueOnce(new Error('저장소가 설정되지 않았습니다.'))
    await act(() => result.current.handlePhotoChange(pick(new File([new Uint8Array(8)], 'me.png', { type: 'image/png' }))))
    expect(window.alert).toHaveBeenLastCalledWith('저장소가 설정되지 않았습니다.')
    expect(result.current.profilePhoto).toBe('')
    expect(auth.refreshSession).not.toHaveBeenCalled()
  })
})
