// @vitest-environment jsdom
import { act, renderHook, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Comment, Post } from './communityApi'

const api = vi.hoisted(() => ({
  getPost: vi.fn(),
  getComments: vi.fn(),
  getTrendingTags: vi.fn(),
  deleteComment: vi.fn(),
  deletePost: vi.fn(),
}))
const navigate = vi.hoisted(() => vi.fn())

vi.mock('./communityApi', () => ({ communityApi: api }))
vi.mock('react-router-dom', () => ({ useNavigate: () => navigate }))

import { usePostDetail } from './usePostDetail'
import { createQueryWrapper } from '../../shared/test/queryWrapper'

const post = (id: string, over: Partial<Post> = {}): Post => ({
  id, author_id: 'u', author_name: 'n', author_role: '', title: id, content: '', category: 'general',
  tags: [], images: [], instrument: 'other', likes_count: 0, comments_count: 0, created_at: '', ...over,
})
const comment = (id: string, replies: Comment[] = []): Comment => ({
  id, post_id: 'p', author_id: 'u', content: id, created_at: '', replies,
})

function deferred<T>() {
  let resolve!: (v: T) => void
  const promise = new Promise<T>(r => { resolve = r })
  return { promise, resolve }
}

beforeEach(() => {
  api.getTrendingTags.mockResolvedValue([])
})
afterEach(() => vi.resetAllMocks())

describe('usePostDetail', () => {
  it('postId가 바뀐 뒤 늦게 도착한 이전 게시물 응답은 무시한다', async () => {
    const postA = deferred<Post>()
    const commentsA = deferred<Comment[]>()
    api.getPost.mockImplementation((id: string) => (id === 'A' ? postA.promise : Promise.resolve(post('B'))))
    api.getComments.mockImplementation((id: string) => (id === 'A' ? commentsA.promise : Promise.resolve([comment('b1')])))

    const { result, rerender } = renderHook(({ id }) => usePostDetail(id, null), { initialProps: { id: 'A' }, wrapper: createQueryWrapper().wrapper })
    rerender({ id: 'B' })
    await waitFor(() => expect(result.current.post?.id).toBe('B'))

    await act(async () => {
      postA.resolve(post('A'))
      commentsA.resolve([comment('a1')])
    })

    expect(result.current.post?.id).toBe('B')
    expect(result.current.comments.map(c => c.id)).toEqual(['b1'])
    expect(result.current.loading).toBe(false)
  })

  it('댓글 삭제는 손자 답글까지 포함해 comments_count를 줄인다', async () => {
    api.getPost.mockResolvedValue(post('P', { comments_count: 4 }))
    api.getComments.mockResolvedValue([comment('1', [comment('1a', [comment('1a-i')])]), comment('2')])
    api.deleteComment.mockResolvedValue(undefined)

    const { result } = renderHook(() => usePostDetail('P', { id: 'u' }), { wrapper: createQueryWrapper().wrapper })
    await waitFor(() => expect(result.current.comments).toHaveLength(2))

    await act(() => result.current.handleDeleteComment('1'))

    // 캐시 변경 알림은 다음 틱에 배치되므로 waitFor로 기다린다
    await waitFor(() => expect(result.current.comments.map(c => c.id)).toEqual(['2']))
    expect(result.current.post?.comments_count).toBe(1)
  })

  it('연속 삭제 시 두 번째 삭제도 최신 트리 기준으로 센다', async () => {
    api.getPost.mockResolvedValue(post('P', { comments_count: 3 }))
    api.getComments.mockResolvedValue([comment('1', [comment('1a')]), comment('2')])
    api.deleteComment.mockResolvedValue(undefined)

    const { result } = renderHook(() => usePostDetail('P', { id: 'u' }), { wrapper: createQueryWrapper().wrapper })
    await waitFor(() => expect(result.current.comments).toHaveLength(2))

    // 같은 렌더의 핸들러로 부모 → 자식을 연달아 삭제(렌더 사이 클로저가 갱신되지 않는 상황).
    // 오래된 트리로 세면 이미 사라진 1a를 한 번 더 빼서 0이 된다.
    const { handleDeleteComment } = result.current
    await act(async () => {
      await Promise.all([handleDeleteComment('1'), handleDeleteComment('1a')])
    })

    // 캐시 변경 알림은 다음 틱에 배치되므로 waitFor로 기다린다
    await waitFor(() => expect(result.current.comments.map(c => c.id)).toEqual(['2']))
    expect(result.current.post?.comments_count).toBe(1)
  })

  it('글 삭제 뒤 리렌더가 있어도 지운 글·댓글을 다시 조회하지 않고 목록으로 한 번만 이동한다', async () => {
    api.getPost.mockResolvedValue(post('P'))
    api.getComments.mockResolvedValue([])
    api.deletePost.mockResolvedValue(undefined)

    const { result, rerender } = renderHook(() => usePostDetail('P', { id: 'u' }), { wrapper: createQueryWrapper().wrapper })
    await waitFor(() => expect(result.current.post?.id).toBe('P'))

    await act(() => result.current.deletePost())
    rerender()
    await act(async () => {})

    expect(api.deletePost).toHaveBeenCalledWith('P')
    expect(api.getPost).toHaveBeenCalledTimes(1)
    expect(api.getComments).toHaveBeenCalledTimes(1)
    expect(navigate.mock.calls).toEqual([['/community']])
  })
})
