// communityApi 체인 테스트: 쿼리 조건과 행 정규화(toPost/toComment), 좋아요 토글 분기
import { afterEach, describe, expect, it, vi } from 'vitest'
import { argsOf, createSupabaseMock, type Call } from '../test/supabaseMock'

const sb = vi.hoisted(() => ({ mock: null as ReturnType<typeof createSupabaseMock> | null }))
vi.mock('../lib/supabaseClient', async () => {
  const { createSupabaseMock } = await import('../test/supabaseMock')
  sb.mock = createSupabaseMock()
  return { supabase: sb.mock.client, isSupabaseConfigured: () => true }
})

import { communityApi } from './communityApi'

const has = (call: Call, method: string) => call.chain.some(([m]) => m === method)
const row = (id: string, over: Record<string, unknown> = {}) => ({
  id, author_id: 'a', author_name: null, author_role: null, title: id, content: 'c', category: null,
  instrument: null, tags: null, images: null, likes_count: null, comments_count: null, views_count: null,
  created_at: '2026-01-01', updated_at: null, ...over,
})

afterEach(() => {
  sb.mock!.reset()
  vi.clearAllMocks()
})

describe('communityApi', () => {
  it('getPosts: 악기·태그 필터를 걸고, 로그인 사용자의 좋아요 여부를 붙이며 null 컬럼을 채운다', async () => {
    sb.mock!.setResult('posts', { data: [row('p1'), row('p2', { likes_count: 4, tags: ['jazz'] })] })
    sb.mock!.setResult('post_likes', { data: [{ post_id: 'p2' }] })

    const posts = await communityApi.getPosts({ instrument: 'guitar', tag: 'jazz', userId: 'u' })

    const postsCall = sb.mock!.calls.find(c => c.table === 'posts')!
    expect(argsOf(postsCall, 'order')).toEqual([['created_at', { ascending: false }]])
    expect(argsOf(postsCall, 'eq')).toEqual([['instrument', 'guitar']])
    expect(argsOf(postsCall, 'contains')).toEqual([['tags', ['jazz']]])
    expect(argsOf(sb.mock!.calls.find(c => c.table === 'post_likes')!, 'eq')).toEqual([['user_id', 'u']])

    expect(posts[0]).toMatchObject({
      author_name: 'User', author_role: '', category: 'general', instrument: 'other',
      tags: [], images: [], likes_count: 0, comments_count: 0, liked_by_user: false,
    })
    expect(posts[1]).toMatchObject({ likes_count: 4, tags: ['jazz'], liked_by_user: true })
  })

  it("getPosts: 'all' 필터와 비로그인이면 조건·좋아요 조회 없이 liked_by_user도 붙이지 않는다", async () => {
    sb.mock!.setResult('posts', { data: [row('p1')] })

    const posts = await communityApi.getPosts({ instrument: 'all' })

    expect(sb.mock!.calls.map(c => c.table)).toEqual(['posts'])
    expect(argsOf(sb.mock!.calls[0], 'eq')).toEqual([])
    expect(posts[0].liked_by_user).toBeUndefined()
  })

  it('toggleLike: 좋아요가 없으면 insert 후 개수를 다시 세어 posts.likes_count를 갱신한다', async () => {
    sb.mock!.setResult('post_likes', call =>
      has(call, 'maybeSingle') ? { data: null } : has(call, 'insert') ? { data: null } : { count: 5 },
    )
    sb.mock!.setResult('posts', { data: { likes_count: 5 } })

    expect(await communityApi.toggleLike('p', 'u')).toEqual({ liked: true, likesCount: 5 })

    const insert = sb.mock!.calls.find(c => has(c, 'insert'))!
    expect(argsOf(insert, 'insert')).toEqual([[{ post_id: 'p', user_id: 'u' }]])
    const update = sb.mock!.calls.find(c => c.table === 'posts')!
    expect(argsOf(update, 'update')).toEqual([[{ likes_count: 5 }]])
    expect(argsOf(update, 'eq')).toEqual([['id', 'p']])
  })

  it('toggleLike: 이미 좋아요면 (post_id, user_id)로 delete한다', async () => {
    sb.mock!.setResult('post_likes', call =>
      has(call, 'maybeSingle') ? { data: { id: 'l1' } } : has(call, 'delete') ? { data: null } : { count: 2 },
    )
    sb.mock!.setResult('posts', { data: { likes_count: 2 } })

    expect(await communityApi.toggleLike('p', 'u')).toEqual({ liked: false, likesCount: 2 })

    const del = sb.mock!.calls.find(c => has(c, 'delete'))!
    expect(argsOf(del, 'eq')).toEqual([['post_id', 'p'], ['user_id', 'u']])
    expect(sb.mock!.calls.some(c => has(c, 'insert'))).toBe(false)
  })

  it('getComments: 프로필 닉네임 조인(없으면 User)으로 작성자 이름을 정하고 답글을 트리로 묶는다', async () => {
    sb.mock!.setResult('comments', {
      data: [
        { id: '1', post_id: 'p', author_id: 'a', content: 'x', parent_id: null, created_at: 't1', updated_at: null, profiles: { nickname: '닉' } },
        { id: '1a', post_id: 'p', author_id: 'b', content: 'y', parent_id: '1', created_at: 't2', updated_at: 't3', profiles: { nickname: null } },
        { id: '2', post_id: 'p', author_id: 'c', content: 'z', parent_id: null, created_at: null, updated_at: null, profiles: null },
      ],
    })

    const tree = await communityApi.getComments('p')

    expect(argsOf(sb.mock!.calls[0], 'eq')).toEqual([['post_id', 'p']])
    expect(tree.map(c => [c.id, c.author_name])).toEqual([['1', '닉'], ['2', 'User']])
    expect(tree[0].replies!.map(c => [c.id, c.author_name, c.updated_at])).toEqual([['1a', 'User', 't3']])
    // 011 이후 profiles.email은 클라이언트가 읽을 수 없다 — 임베드는 nickname만
    expect(argsOf(sb.mock!.calls[0], 'select')[0][0]).toContain('profiles:author_id (nickname)')
    expect(tree[1].created_at).toBe('')
  })
})
