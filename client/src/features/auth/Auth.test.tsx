// @vitest-environment jsdom
import { StrictMode, useState } from 'react'
import { act, cleanup, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes, useNavigate } from 'react-router-dom'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { Auth } from './Auth'
import { AuthContext, type AuthAlert, type AuthContextType } from './useAuth'

/** AuthProvider 대신 알림 상태만 가진 컨텍스트 — Auth가 알림을 언제 지우는지 본다 */
let current: AuthContextType
let go: (path: string) => void

function Nav() {
  go = useNavigate()
  return null
}

function Harness({ initial }: { initial: AuthAlert | null }) {
  const [alert, setAlert] = useState<AuthAlert | null>(initial)
  current = {
    user: null,
    loading: false,
    alert,
    setAlert,
    signUp: vi.fn(),
    signIn: vi.fn(),
    signOut: vi.fn(),
    isSupabaseReady: true,
  }
  return (
    <AuthContext.Provider value={current}>
      <MemoryRouter initialEntries={['/auth']}>
        <Nav />
        <Routes>
          <Route path="/auth" element={<Auth />} />
          <Route path="/" element={<div>홈</div>} />
        </Routes>
      </MemoryRouter>
    </AuthContext.Provider>
  )
}

const LOGGED_OUT: AuthAlert = { type: 'info', message: '로그아웃되었습니다.' }
const nextTick = () => act(() => new Promise<void>(resolve => setTimeout(resolve, 0)))

afterEach(() => cleanup())

describe('Auth 화면 알림', () => {
  it('"로그아웃되었습니다."는 로그인 화면에서 한 번 보이고, 화면을 떠나면 지워져 다음 방문 때 다시 보이지 않는다', async () => {
    render(<Harness initial={LOGGED_OUT} />)
    expect(screen.getByText('로그아웃되었습니다.')).toBeTruthy()
    await nextTick()
    expect(current.alert).toEqual(LOGGED_OUT) // 보고 있는 동안은 그대로

    act(() => go('/'))
    await nextTick()
    expect(current.alert).toBeNull()

    act(() => go('/auth'))
    await nextTick()
    expect(screen.queryByText('로그아웃되었습니다.')).toBeNull()
  })

  it('로그인 화면에서 난 오류 알림도 떠날 때 지운다', async () => {
    render(<Harness initial={null} />)
    act(() => current.setAlert({ type: 'error', message: 'Invalid login credentials' }))
    expect(screen.getByText('Invalid login credentials')).toBeTruthy()

    act(() => go('/'))
    await nextTick()
    expect(current.alert).toBeNull()
  })

  it('StrictMode(개발)의 마운트→정리→마운트에서는 지우지 않는다', async () => {
    render(
      <StrictMode>
        <Harness initial={LOGGED_OUT} />
      </StrictMode>,
    )
    await nextTick()
    expect(screen.getByText('로그아웃되었습니다.')).toBeTruthy()
    expect(current.alert).toEqual(LOGGED_OUT)

    act(() => go('/'))
    await nextTick()
    expect(current.alert).toBeNull()
  })
})
