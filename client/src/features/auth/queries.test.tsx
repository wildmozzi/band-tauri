// @vitest-environment jsdom
// P3-3d: 로그아웃·계정 전환 시 Query 캐시 초기화
import { act, cleanup, render, renderHook, waitFor } from '@testing-library/react'
import { useQuery } from '@tanstack/react-query'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createQueryWrapper } from '../../shared/test/queryWrapper'

type AuthCb = (event: string, session: { user: { id: string } } | null) => void
const sb = vi.hoisted(() => {
  const state = { cb: null as null | ((event: string, session: { user: { id: string } } | null) => void), user: null as null | { id: string } }
  const auth = {
    getUser: vi.fn(async () => ({ data: { user: state.user } })),
    onAuthStateChange: vi.fn((cb: AuthCb) => {
      state.cb = cb
      return { data: { subscription: { unsubscribe: vi.fn() } } }
    }),
    signInWithPassword: vi.fn(async () => ({ error: null as unknown })),
    signOut: vi.fn(async () => ({ error: null as unknown })),
    signUp: vi.fn(async () => ({ data: { user: { id: 'new' } }, error: null as unknown })),
  }
  return { state, client: { auth, from: vi.fn() } }
})
vi.mock('../../shared/lib/supabaseClient', () => ({ supabase: sb.client, isSupabaseConfigured: () => true }))

import { authApi, isUserAgnosticKey, resetUserScopedCache, shouldResetUserCache, useResetQueryCacheOnUserChange } from './queries'
import { AuthProvider } from './AuthContext'
import { useAuth } from './useAuth'

beforeEach(() => {
  sb.state.user = null
  sb.state.cb = null
})

afterEach(() => {
  cleanup()
  vi.clearAllMocks()
})

describe('shouldResetUserCache', () => {
  it('첫 확인 전(undefined)은 기준점일 뿐, 이후 id가 바뀔 때만 true', () => {
    expect(shouldResetUserCache(undefined, 'a')).toBe(false)
    expect(shouldResetUserCache(undefined, null)).toBe(false)
    expect(shouldResetUserCache('a', 'a')).toBe(false)
    expect(shouldResetUserCache(null, null)).toBe(false)
    expect(shouldResetUserCache('a', null)).toBe(true) // 로그아웃
    expect(shouldResetUserCache(null, 'b')).toBe(true) // 로그인
    expect(shouldResetUserCache('a', 'b')).toBe(true) // 계정 전환
  })
})

describe('resetUserScopedCache', () => {
  it('비활성 쿼리는 지우고, 활성 쿼리는 데이터를 둔 채 다시 읽으며(로딩으로 안 돌아감), mutation 캐시를 비운다', async () => {
    const { client, wrapper } = createQueryWrapper()
    client.setQueryData(['rooms', 'mine', 'a'], ['m1'])
    let n = 0
    const fetcher = vi.fn(async () => `v${++n}`)
    const { result } = renderHook(() => useQuery({ queryKey: ['community', 'posts', {}], queryFn: fetcher }), { wrapper })
    await waitFor(() => expect(result.current.data).toBe('v1'))
    client.getMutationCache().build(client, { mutationFn: async () => 1 })

    let reset!: Promise<void>
    act(() => { reset = resetUserScopedCache(client) })
    expect(result.current.data).toBe('v1') // invalidate: 데이터 유지
    expect(result.current.isLoading).toBe(false)
    await act(() => reset)

    expect(client.getQueryCache().find({ queryKey: ['rooms', 'mine', 'a'] })).toBeUndefined()
    await waitFor(() => expect(result.current.data).toBe('v2'))
    expect(fetcher).toHaveBeenCalledTimes(2)
    expect(client.getMutationCache().getAll()).toHaveLength(0)
  })

  it('사용자와 무관한 키(방 상세·공개 목록)는 활성이든 비활성이든 건드리지 않는다', async () => {
    const { client, wrapper } = createQueryWrapper()
    client.setQueryData(['rooms', 'detail', 'r2'], { id: 'r2' })
    client.setQueryData(['rooms', 'list'], [])
    const fetcher = vi.fn(async () => ({ id: 'r1' }))
    const { result } = renderHook(() => useQuery({ queryKey: ['rooms', 'detail', 'r1'], queryFn: fetcher }), { wrapper })
    await waitFor(() => expect(result.current.data).toEqual({ id: 'r1' }))

    await act(() => resetUserScopedCache(client))

    expect(client.getQueryData(['rooms', 'detail', 'r2'])).toEqual({ id: 'r2' })
    expect(client.getQueryData(['rooms', 'list'])).toEqual([])
    expect(client.getQueryState(['rooms', 'detail', 'r1'])?.isInvalidated).toBe(false)
    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it('진행 중인 조회는 취소·재시작하지 않는다(로그인 직후 이중 조회 방지)', async () => {
    const { client, wrapper } = createQueryWrapper()
    let finish!: (v: string) => void
    const fetcher = vi.fn(() => new Promise<string>(r => { finish = r }))
    const { result } = renderHook(() => useQuery({ queryKey: ['profile', 'audioSettings', 'b'], queryFn: fetcher }), { wrapper })
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1))

    let reset!: Promise<void>
    act(() => { reset = resetUserScopedCache(client) })
    await act(async () => { finish('ok'); await reset })

    await waitFor(() => expect(result.current.data).toBe('ok'))
    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it('isUserAgnosticKey', () => {
    expect(isUserAgnosticKey(['rooms', 'detail', 'r1'])).toBe(true)
    expect(isUserAgnosticKey(['rooms', 'list'])).toBe(true)
    expect(isUserAgnosticKey(['rooms', 'mine', 'a'])).toBe(false)
    expect(isUserAgnosticKey(['profile', 'nickname', 'a'])).toBe(false)
    expect(isUserAgnosticKey(['rooms'])).toBe(false)
  })
})

describe('useResetQueryCacheOnUserChange', () => {
  it('첫 인증 확인(로딩 끝)에서는 비우지 않고, 그 뒤 사용자 변경마다 비운다', () => {
    const { client, wrapper } = createQueryWrapper()
    const seed = () => client.setQueryData(['profile', 'nickname', 'x'], 'n')
    const has = () => client.getQueryData(['profile', 'nickname', 'x']) !== undefined
    seed()
    const { rerender } = renderHook(({ id, loading }) => useResetQueryCacheOnUserChange(id, loading), {
      wrapper,
      initialProps: { id: null as string | null, loading: true },
    })

    rerender({ id: 'a', loading: true }) // 로딩 중 INITIAL_SESSION
    rerender({ id: 'a', loading: false }) // 첫 확인 끝 → 기준점 a
    expect(has()).toBe(true)

    rerender({ id: 'a', loading: false }) // 토큰 갱신 등 같은 사용자
    expect(has()).toBe(true)

    rerender({ id: null, loading: false }) // 로그아웃
    expect(has()).toBe(false)

    seed()
    rerender({ id: 'b', loading: true }) // 로그인 (signIn 중 로딩이어도)
    expect(has()).toBe(false)

    seed()
    rerender({ id: 'c', loading: false }) // 계정 전환
    expect(has()).toBe(false)
  })
})

function Probe() {
  const { user, loading, signOut } = useAuth()
  return (
    <div>
      <span data-testid="user">{loading ? 'loading' : (user?.id ?? 'none')}</span>
      <button onClick={() => void signOut()}>out</button>
    </div>
  )
}

describe('AuthProvider + Query 캐시 (P3-3d)', () => {
  it('로그아웃하면 이전 사용자 캐시가 비워지고, 다른 계정 로그인 때도 비워진다', async () => {
    sb.state.user = { id: 'a' }
    const { client, wrapper: Wrapper } = createQueryWrapper()
    client.setQueryData(['community', 'post', 'p1', { userId: 'a' }], { liked_by_user: true })
    const view = render(<Wrapper><AuthProvider><Probe /></AuthProvider></Wrapper>)
    await waitFor(() => expect(view.getByTestId('user').textContent).toBe('a'))
    // 첫 로그인 확인은 캐시를 지우지 않는다
    expect(client.getQueryData(['community', 'post', 'p1', { userId: 'a' }])).toBeDefined()

    // 로그아웃 (Supabase가 SIGNED_OUT 이벤트를 보냄)
    await act(async () => {
      view.getByText('out').click()
      sb.state.cb!('SIGNED_OUT', null)
    })
    expect(sb.client.auth.signOut).toHaveBeenCalled()
    await waitFor(() => expect(view.getByTestId('user').textContent).toBe('none'))
    expect(client.getQueryData(['community', 'post', 'p1', { userId: 'a' }])).toBeUndefined()

    // 다른 계정 로그인
    client.setQueryData(['profile', 'audioSettings', null], null)
    act(() => sb.state.cb!('SIGNED_IN', { user: { id: 'b' } }))
    await waitFor(() => expect(view.getByTestId('user').textContent).toBe('b'))
    expect(client.getQueryCache().find({ queryKey: ['profile', 'audioSettings', null] })).toBeUndefined()
  })

  it('세션 복원(getUser)이 reject해도 loading이 풀리고 로그아웃 상태가 된다 (PR #22 리뷰 L-3)', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    sb.client.auth.getUser.mockRejectedValueOnce(new Error('storage blocked'))
    const { wrapper: Wrapper } = createQueryWrapper()
    const view = render(<Wrapper><AuthProvider><Probe /></AuthProvider></Wrapper>)
    await waitFor(() => expect(view.getByTestId('user').textContent).toBe('none'))
    expect(warn).toHaveBeenCalled()
    warn.mockRestore()
  })

  it('같은 사용자의 토큰 갱신은 캐시를 유지한다', async () => {
    sb.state.user = { id: 'a' }
    const { client, wrapper: Wrapper } = createQueryWrapper()
    client.setQueryData(['rooms', 'mine', 'a'], [])
    const view = render(<Wrapper><AuthProvider><Probe /></AuthProvider></Wrapper>)
    await waitFor(() => expect(view.getByTestId('user').textContent).toBe('a'))

    act(() => sb.state.cb!('TOKEN_REFRESHED', { user: { id: 'a' } }))
    expect(client.getQueryData(['rooms', 'mine', 'a'])).toEqual([])
  })
})

describe('authApi', () => {
  it('signIn/signOut 오류는 던진다', async () => {
    sb.client.auth.signInWithPassword.mockResolvedValueOnce({ error: new Error('bad') })
    await expect(authApi.signIn('e', 'p')).rejects.toThrow('bad')
    sb.client.auth.signOut.mockResolvedValueOnce({ error: new Error('nope') })
    await expect(authApi.signOut()).rejects.toThrow('nope')
  })

  it('signUp은 메타데이터를 options.data로 보내고 사용자가 없으면 오류', async () => {
    const meta = { name: 'n', nickname: 'k', birth_date: '2000-01-01', phone: null }
    await expect(authApi.signUp('e@x', 'pw', meta)).resolves.toEqual({ id: 'new' })
    expect(sb.client.auth.signUp).toHaveBeenCalledWith({ email: 'e@x', password: 'pw', options: { data: meta } })
    sb.client.auth.signUp.mockResolvedValueOnce({ data: { user: null as unknown as { id: string } }, error: null })
    await expect(authApi.signUp('e@x', 'pw', meta)).rejects.toThrow('회원가입에 실패했습니다.')
  })

  it('onAuthStateChange 해제 함수는 구독을 끊는다', () => {
    const off = authApi.onAuthStateChange(() => {})
    const sub = sb.client.auth.onAuthStateChange.mock.results[0].value.data.subscription
    off()
    expect(sub.unsubscribe).toHaveBeenCalled()
  })
})
