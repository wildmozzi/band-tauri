// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router-dom'
import { afterEach, describe, expect, it, vi } from 'vitest'

const auth = vi.hoisted(() => ({ user: null as { id: string } | null, loading: true }))
vi.mock('./useAuth', () => ({
  useAuth: () => ({ user: auth.user, loading: auth.loading }),
}))

import { useRequireAuth } from './useRequireAuth'

afterEach(() => {
  cleanup()
  auth.user = null
  auth.loading = true
})

function Guarded() {
  const { user, gate } = useRequireAuth()
  if (gate) return gate
  return <div>PAGE {user.id}</div>
}

function AuthPage() {
  const location = useLocation()
  return <div>AUTH from={(location.state as { from?: string } | null)?.from}</div>
}

function renderAt(path: string) {
  const ui = () => (
    <MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route path="/auth" element={<AuthPage />} />
        <Route path="*" element={<Guarded />} />
      </Routes>
    </MemoryRouter>
  )
  const view = render(ui())
  return { rerender: () => view.rerender(ui()) }
}

describe('useRequireAuth', () => {
  it('세션 복원 중(user 없음)에는 /auth로 보내지 않고 확인 중을 보여 준 뒤, 복원되면 페이지를 보여 준다', () => {
    const view = renderAt('/community/create')
    expect(screen.getByRole('status').textContent).toContain('로그인 상태를 확인하는 중')
    expect(screen.queryByText(/^AUTH/)).toBeNull()

    act(() => {
      auth.user = { id: 'u1' }
      auth.loading = false
      view.rerender()
    })
    expect(screen.getByText('PAGE u1')).toBeTruthy()
  })

  it('복원이 끝났는데 사용자가 없으면 /auth로, 현재 경로(쿼리·해시 포함)를 from으로 넘긴다', () => {
    auth.loading = false
    renderAt('/payment?plan=pro#x')
    expect(screen.getByText('AUTH from=/payment?plan=pro#x')).toBeTruthy()
  })

  it('쿼리가 허용 문자 밖이면 경로만 살려 from으로 넘긴다', () => {
    auth.loading = false
    renderAt('/payment?plan=[pro]')
    expect(screen.getByText('AUTH from=/payment')).toBeTruthy()
  })

  it('wrapChecking으로 확인 중 표시를 페이지 레이아웃으로 감싼다', () => {
    function Wrapped() {
      const { gate } = useRequireAuth({ wrapChecking: c => <section data-testid="wrap">{c}</section> })
      return gate ?? <div>PAGE</div>
    }
    render(
      <MemoryRouter initialEntries={['/payment?plan=pro']}>
        <Wrapped />
      </MemoryRouter>,
    )
    expect(screen.getByTestId('wrap').textContent).toContain('로그인 상태를 확인하는 중')
  })

  it('사용자가 있으면 loading이어도(로그아웃 처리 중 등) 페이지를 유지한다', () => {
    auth.user = { id: 'u1' }
    auth.loading = true
    renderAt('/profile')
    expect(screen.getByText('PAGE u1')).toBeTruthy()
  })
})
