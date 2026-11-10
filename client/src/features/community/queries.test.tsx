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
  forgetPost,
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

  it('좋아요 여부는 누른 사용자의 키에만 쓰고, 다른 사용자·비로그인 키는 개수만 바뀐다', async () => {
    api.toggleLike.mockResolvedValue({ liked: true, likesCount: 3 })
    const { wrapper, client } = createQueryWrapper()
    const anon = { ...filters, userId: null }
    const other = { ...filters, userId: 'a' }
    client.setQueryData(communityKeys.postList(filters), [post('p')])
    client.setQueryData(communityKeys.postList(anon), [post('p')])
    client.setQueryData(communityKeys.postList(other), [post('p', { liked_by_user: false })])
    client.setQueryData(communityKeys.post('p', 'a'), post('p', { liked_by_user: false }))
    client.setQueryData(communityKeys.post('p', null), post('p'))
    const { result } = renderHook(() => useToggleLikeMutation(), { wrapper })

    await act(() => result.current.mutateAsync({ postId: 'p', userId: 'u' }))

    expect(client.getQueryData<Post[]>(communityKeys.postList(filters))![0].liked_by_user).toBe(true)
    expect(client.getQueryData<Post[]>(communityKeys.postList(anon))![0]).toMatchObject({ likes_count: 3 })
    expect(client.getQueryData<Post[]>(communityKeys.postList(anon))![0].liked_by_user).toBeUndefined()
    expect(client.getQueryData<Post[]>(communityKeys.postList(other))![0]).toMatchObject({ likes_count: 3, liked_by_user: false })
    expect(client.getQueryData<Post>(communityKeys.post('p', 'a'))).toMatchObject({ likes_count: 3, liked_by_user: false })
    expect(client.getQueryData<Post>(communityKeys.post('p', null))!.liked_by_user).toBeUndefined()
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

  it('글 수정은 응답으로 모든 사용자 키의 상세를 덮어쓰고(좋아요 여부 유지) 목록을 무효화한다', async () => {
    api.updatePost.mockResolvedValue(post('p', { title: '새 제목' }))
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(communityKeys.post('p', null), post('p'))
    client.setQueryData(communityKeys.post('p', 'u'), post('p', { liked_by_user: true }))
    client.setQueryData(communityKeys.postList(filters), [post('p')])
    const { result } = renderHook(() => useUpdatePostMutation(), { wrapper })

    await act(() => result.current.mutateAsync({ postId: 'p', data: { title: '새 제목' } }))

    expect(client.getQueryData<Post>(communityKeys.post('p', null))!.title).toBe('새 제목')
    expect(client.getQueryData<Post>(communityKeys.post('p', 'u'))).toMatchObject({ title: '새 제목', liked_by_user: true })
    expect(client.getQueryState(communityKeys.post('p', 'u'))?.isInvalidated).toBe(false)
    expect(client.getQueryState(communityKeys.postList(filters))?.isInvalidated).toBe(true)
  })

  it('글 삭제는 목록을 무효화하고, forgetPost가 상세·댓글 캐시를 지운다', async () => {
    api.deletePost.mockResolvedValue(undefined)
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(communityKeys.post('p', 'u'), post('p'))
    client.setQueryData(communityKeys.comments('p'), [comment('1')])
    client.setQueryData(communityKeys.postList(filters), [post('p')])
    const { result } = renderHook(() => useDeletePostMutation(), { wrapper })

    await act(() => result.current.mutateAsync('p'))
    expect(client.getQueryState(communityKeys.postList(filters))?.isInvalidated).toBe(true)

    forgetPost(client, 'p')
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
