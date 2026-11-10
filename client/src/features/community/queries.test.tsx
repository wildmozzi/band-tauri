// @vitest-environment jsdom
import { act, renderHook, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createQueryWrapper } from '../../test/queryWrapper'
import type { Comment, Post } from '../../services/communityApi'

const api = vi.hoisted(() => ({
  getPosts: vi.fn(),
  getPost: vi.fn(),
  createPost: vi.fn(),
  updatePost: vi.fn(),
  deletePost: vi.fn(),
  toggleLike: vi.fn(),
  addComment: vi.fn(),
  getTrendingTags: vi.fn(),
}))
vi.mock('../../services/communityApi', () => ({ communityApi: api }))

import {
  communityKeys,
  useAddCommentMutation,
  useCreatePostMutation,
  useDeletePostMutation,
  usePostsQuery,
  useToggleLikeMutation,
  useUpdatePostMutation,
} from './queries'

const post = (id: string, over: Partial<Post> = {}): Post => ({
  id, author_id: 'u', author_name: 'n', author_role: '', title: id, content: '', category: 'general',
  tags: [], images: [], instrument: 'other', likes_count: 0, comments_count: 0, created_at: '', ...over,
})
const comment = (id: string, replies: Comment[] = []): Comment => ({
  id, post_id: 'p', author_id: 'u', content: id, created_at: '', replies,
})
const filters = { instrument: 'all', tag: null, userId: 'u' }

afterEach(() => vi.resetAllMocks())

describe('community queries', () => {
  it('목록 조회는 필터를 API 옵션으로 넘긴다', async () => {
    api.getPosts.mockResolvedValue([post('a')])
    const { wrapper } = createQueryWrapper()
    const { result } = renderHook(() => usePostsQuery({ instrument: 'guitar', tag: 'jazz', userId: null }), { wrapper })
    await waitFor(() => expect(result.current.data).toHaveLength(1))
    expect(api.getPosts).toHaveBeenCalledWith({ instrument: 'guitar', tag: 'jazz', userId: undefined })
  })

  it('좋아요 토글은 목록과 상세 캐시를 재조회 없이 갱신한다', async () => {
    api.toggleLike.mockResolvedValue({ liked: true, likesCount: 7 })
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(communityKeys.postList(filters), [post('p'), post('q')])
    client.setQueryData(communityKeys.post('p', 'u'), post('p'))
    const { result } = renderHook(() => useToggleLikeMutation(), { wrapper })

    await act(() => result.current.mutateAsync({ postId: 'p', userId: 'u' }))

    const list = client.getQueryData<Post[]>(communityKeys.postList(filters))!
    expect(list[0]).toMatchObject({ likes_count: 7, liked_by_user: true })
    expect(list[1].likes_count).toBe(0)
    expect(client.getQueryData<Post>(communityKeys.post('p', 'u'))).toMatchObject({ likes_count: 7, liked_by_user: true })
    expect(api.getPosts).not.toHaveBeenCalled()
  })

  it('글 작성은 목록과 인기 태그를 무효화한다', async () => {
    api.createPost.mockResolvedValue(post('new'))
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(communityKeys.postList(filters), [])
    client.setQueryData(communityKeys.trendingTags(5), [])
    const { result } = renderHook(() => useCreatePostMutation(), { wrapper })

    await act(() => result.current.mutateAsync({ data: { title: 't', content: 'c' }, user: { id: 'u' } }))

    expect(client.getQueryState(communityKeys.postList(filters))?.isInvalidated).toBe(true)
    expect(client.getQueryState(communityKeys.trendingTags(5))?.isInvalidated).toBe(true)
  })

  it('글 수정은 모든 사용자 키의 상세를 무효화한다', async () => {
    api.updatePost.mockResolvedValue(post('p'))
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(communityKeys.post('p', null), post('p'))
    client.setQueryData(communityKeys.post('p', 'u'), post('p'))
    const { result } = renderHook(() => useUpdatePostMutation(), { wrapper })

    await act(() => result.current.mutateAsync({ postId: 'p', data: { title: 'x' } }))

    expect(client.getQueryState(communityKeys.post('p', null))?.isInvalidated).toBe(true)
    expect(client.getQueryState(communityKeys.post('p', 'u'))?.isInvalidated).toBe(true)
  })

  it('글 삭제는 상세·댓글 캐시를 지운다', async () => {
    api.deletePost.mockResolvedValue(undefined)
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(communityKeys.post('p', 'u'), post('p'))
    client.setQueryData(communityKeys.comments('p'), [comment('1')])
    const { result } = renderHook(() => useDeletePostMutation(), { wrapper })

    await act(() => result.current.mutateAsync('p'))

    expect(client.getQueryData(communityKeys.post('p', 'u'))).toBeUndefined()
    expect(client.getQueryData(communityKeys.comments('p'))).toBeUndefined()
  })

  it('답글 작성은 트리에 끼우고 상세·목록의 comments_count를 올린다', async () => {
    api.addComment.mockResolvedValue(comment('1a'))
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(communityKeys.comments('p'), [comment('1')])
    client.setQueryData(communityKeys.post('p', 'u'), post('p', { comments_count: 1 }))
    client.setQueryData(communityKeys.postList(filters), [post('p', { comments_count: 1 })])
    const { result } = renderHook(() => useAddCommentMutation(), { wrapper })

    await act(() => result.current.mutateAsync({ postId: 'p', content: 'r', parentId: '1', user: { id: 'u' } }))

    expect(api.addComment).toHaveBeenCalledWith({ post_id: 'p', content: 'r', parent_id: '1' }, { id: 'u' })
    expect(client.getQueryData<Comment[]>(communityKeys.comments('p'))![0].replies!.map(c => c.id)).toEqual(['1a'])
    expect(client.getQueryData<Post>(communityKeys.post('p', 'u'))!.comments_count).toBe(2)
    expect(client.getQueryData<Post[]>(communityKeys.postList(filters))![0].comments_count).toBe(2)
  })
})
