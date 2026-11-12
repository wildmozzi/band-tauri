// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { Post } from './communityApi'

const api = vi.hoisted(() => ({ getPost: vi.fn() }))
const auth = vi.hoisted(() => ({ user: null as { id: string } | null }))

vi.mock('./communityApi', () => ({ communityApi: api }))
// 매 렌더 새 객체를 돌려준다(토큰 갱신으로 user 참조만 바뀌는 상황)
vi.mock('../auth/useAuth', () => ({
  useAuth: () => ({ user: auth.user ? { ...auth.user } : null }),
}))

import { CreatePost } from './CreatePost'
import { EditPost } from './EditPost'
import { createQueryWrapper } from '../../shared/test/queryWrapper'

const post: Post = {
  id: 'p1', author_id: 'u1', author_name: 'n', author_role: '', title: '원래 제목', content: '본문',
  category: 'general', tags: [], images: [], instrument: 'other', likes_count: 0, comments_count: 0, created_at: '',
}

afterEach(() => {
  cleanup()
  vi.resetAllMocks()
  auth.user = null
})

function renderAt(path: string) {
  const { wrapper } = createQueryWrapper()
  return render(
    <MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route path="/community/create" element={<CreatePost />} />
        <Route path="/community/edit/:postId" element={<EditPost />} />
        <Route path="/auth" element={<div>AUTH PAGE</div>} />
      </Routes>
    </MemoryRouter>,
    { wrapper },
  )
}

describe('EditPost', () => {
  it('같은 사용자의 user 객체가 바뀌어도 다시 불러오지 않고 편집 중 입력을 유지한다', async () => {
    auth.user = { id: 'u1' }
    api.getPost.mockResolvedValue(post)
    const view = renderAt('/community/edit/p1')

    const title = await screen.findByDisplayValue('원래 제목')
    fireEvent.change(title, { target: { value: '편집 중' } })

    // 다시 렌더 → useAuth가 새 user 객체를 돌려줌
    view.rerender(
      <MemoryRouter initialEntries={['/community/edit/p1']}>
        <Routes>
          <Route path="/community/edit/:postId" element={<EditPost />} />
        </Routes>
      </MemoryRouter>,
    )
    await act(async () => {})

    expect(api.getPost).toHaveBeenCalledTimes(1)
    expect(screen.getByDisplayValue('편집 중')).toBeTruthy()
  })

  it('비로그인이면 /auth로 이동한다', async () => {
    renderAt('/community/edit/p1')
    await screen.findByText('AUTH PAGE')
    expect(api.getPost).not.toHaveBeenCalled()
  })
})

describe('CreatePost', () => {
  it('비로그인이면 effect에서 /auth로 이동한다', async () => {
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {})
    const warns = vi.spyOn(console, 'warn').mockImplementation(() => {})
    renderAt('/community/create')
    await screen.findByText('AUTH PAGE')
    // 렌더 중 navigate 경고(react-router "should call navigate() in a React.useEffect()")가 없어야 한다
    const messages = [...errors.mock.calls, ...warns.mock.calls].flat().join(' ')
    expect(messages).not.toMatch(/useEffect/)
  })

  it('로그인 상태면 작성 폼을 보여준다', async () => {
    auth.user = { id: 'u1' }
    renderAt('/community/create')
    await waitFor(() => expect(screen.getByText('새 게시물 작성')).toBeTruthy())
  })
})
