// @vitest-environment jsdom
// Community에서 분리한 목록 훅: 필터가 조회 조건으로 가는지, 인기 글, 좋아요(로그인 필요)
import { act, renderHook } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

const state = vi.hoisted(() => ({
  user: { id: 'u1' } as { id: string } | null,
  posts: [] as { id: string; likes_count: number }[],
  postsQuery: vi.fn(),
  toggleLike: vi.fn<(v: { postId: string; userId: string }) => Promise<void>>(async () => undefined),
}))
vi.mock('../auth', () => ({ useAuth: () => ({ user: state.user, loading: false }) }))
vi.mock('./queries', () => ({
  usePostsQuery: (filter: unknown) => {
    state.postsQuery(filter)
    return { data: state.posts, isPending: false, error: null }
  },
  useTrendingTagsQuery: () => ({ data: [{ tag: '재즈', count: 3 }], error: null }),
  useToggleLikeMutation: () => ({ mutateAsync: state.toggleLike }),
}))

import { useCommunityFeed } from './useCommunityFeed'

afterEach(() => {
  vi.restoreAllMocks()
  vi.clearAllMocks()
  state.user = { id: 'u1' }
  state.posts = []
})

describe('useCommunityFeed', () => {
  it('악기·태그 필터가 글 조회 조건이 되고, 같은 태그를 다시 누르면 해제', () => {
    const { result } = renderHook(() => useCommunityFeed())
    expect(state.postsQuery).toHaveBeenLastCalledWith({ instrument: 'all', tag: null, userId: 'u1' })
    act(() => result.current.setSelectedInstrument('bass'))
    act(() => result.current.handleTagClick('재즈'))
    expect(state.postsQuery).toHaveBeenLastCalledWith({ instrument: 'bass', tag: '재즈', userId: 'u1' })
    act(() => result.current.handleTagClick('재즈'))
    expect(result.current.selectedTag).toBeNull()
    expect(result.current.trendingTags).toEqual([{ tag: '재즈', count: 3 }])
  })

  it('인기 글은 좋아요 10개 이상', () => {
    state.posts = [{ id: 'a', likes_count: 3 }, { id: 'b', likes_count: 11 }]
    const { result } = renderHook(() => useCommunityFeed())
    expect(result.current.popularPosts.map(p => p.id)).toEqual(['b'])
  })

  it('좋아요: 로그인하면 토글, 아니면 안내만', async () => {
    const { result, rerender } = renderHook(() => useCommunityFeed())
    const stopPropagation = vi.fn()
    await act(() => result.current.handleLike('p1', { stopPropagation } as unknown as React.MouseEvent))
    expect(stopPropagation).toHaveBeenCalled()
    expect(state.toggleLike).toHaveBeenCalledWith({ postId: 'p1', userId: 'u1' })

    state.user = null
    rerender()
    const alert = vi.spyOn(window, 'alert').mockImplementation(() => {})
    await act(() => result.current.handleLike('p1'))
    expect(alert).toHaveBeenCalledWith('좋아요를 누르려면 로그인이 필요합니다.')
    expect(state.toggleLike).toHaveBeenCalledTimes(1)
  })
})
