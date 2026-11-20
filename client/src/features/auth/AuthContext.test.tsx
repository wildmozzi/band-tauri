// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

type SessionLike = { user: { id: string } } | null
const h = vi.hoisted(() => ({
  emit: null as ((session: SessionLike) => void) | null,
  signIn: vi.fn(async () => {}),
  signOut: vi.fn(async () => {}),
}))

vi.mock('../../shared/lib/supabaseClient', () => ({ isSupabaseConfigured: () => true }))
vi.mock('./queries', () => ({
  authApi: {
    getUser: async () => null,
    onAuthStateChange: (cb: (session: SessionLike) => void) => {
      h.emit = cb
      return () => {}
    },
    signIn: h.signIn,
    signOut: h.signOut,
  },
  useResetQueryCacheOnUserChange: () => {},
}))
vi.mock('./profileQueries', () => ({ profileApi: {} }))
vi.mock('./legacyProfilePhoto', () => ({ useLegacyProfilePhotoCleanup: () => {} }))

import { AuthProvider } from './AuthContext'
import { useAuth, type AuthContextType } from './useAuth'

let ctx: AuthContextType
function Probe() {
  ctx = useAuth()
  return <div data-testid="alert">{ctx.alert?.message ?? ''}</div>
}

async function mount() {
  render(
    <AuthProvider>
      <Probe />
    </AuthProvider>,
  )
  await act(async () => {})
}
const alertText = () => screen.getByTestId('alert').textContent
const session = { user: { id: 'u1' } }

afterEach(() => {
  cleanup()
  h.emit = null
  h.signIn.mockClear()
  h.signOut.mockClear()
})

describe('AuthProvider 알림', () => {
  it('세션 복원·토큰 갱신·로그인 이벤트로는 알림을 켜지 않는다', async () => {
    await mount()
    act(() => h.emit!(session)) // INITIAL_SESSION
    act(() => h.emit!(session)) // TOKEN_REFRESHED
    expect(alertText()).toBe('')
  })

  it('signIn 성공은 알림을 남기지 않는다', async () => {
    await mount()
    act(() => h.emit!(null))
    await act(async () => {
      await ctx.signIn('a@b.c', 'password1')
      h.emit!(session)
    })
    expect(alertText()).toBe('')
  })

  it('로그인 중 켜진 알림은 다른 탭 로그아웃(이벤트만 옴) 때 지워진다', async () => {
    await mount()
    act(() => h.emit!(session))
    act(() => ctx.setAlert({ type: 'success', message: '회원가입이 완료되었습니다! 로그인해주세요.' }))
    act(() => h.emit!(null))
    expect(alertText()).toBe('')
  })

  it('로그아웃 상태에서 켜진 오류 알림은 로그인되면 지워진다', async () => {
    await mount()
    act(() => h.emit!(null))
    act(() => ctx.setAlert({ type: 'error', message: '오류' }))
    act(() => h.emit!(session))
    expect(alertText()).toBe('')
  })

  it('이 탭에서 signOut하면 "로그아웃되었습니다."가 남는다', async () => {
    h.signOut.mockImplementationOnce(async () => {
      h.emit!(null) // supabase는 signOut이 끝나기 전에 SIGNED_OUT을 알린다
    })
    await mount()
    act(() => h.emit!(session))
    await act(async () => {
      await ctx.signOut()
    })
    expect(alertText()).toBe('로그아웃되었습니다.')
  })

  it('로그인 상태가 그대로면 알림을 건드리지 않는다', async () => {
    await mount()
    act(() => h.emit!(null))
    act(() => ctx.setAlert({ type: 'error', message: '비밀번호가 틀렸습니다' }))
    act(() => h.emit!(null))
    expect(alertText()).toBe('비밀번호가 틀렸습니다')
  })
})
