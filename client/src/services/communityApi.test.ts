// communityApi 체인 테스트: 쿼리 조건과 행 정규화(toPost/toComment), 좋아요 토글 분기
import { afterEach, describe, expect, it, vi } from 'vitest'
import { argsOf, createSupabaseMock, type Call } from '../test/supabaseMock'

const sb = vi.hoisted(() => ({ mock: null as ReturnType<typeof createSupabaseMock> | null }))
vi.mock('../lib/supabaseClient', async () => {
  const { createSupabaseMock } = await import('../test/supabaseMock')
  sb.mock = createSupabaseMock()
  return { supabase: sb.mock.client, isSupabaseConfigured: () => true }
})

import { PostSaveError, communityApi, toPost } from './communityApi'

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

  it('toggleLike: 좋아요가 없으면 insert하고 post_likes 개수를 돌려준다 — posts는 UPDATE하지 않는다(남의 글 0 표시 버그)', async () => {
    sb.mock!.setResult('post_likes', call =>
      has(call, 'maybeSingle') ? { data: null } : has(call, 'insert') ? { data: null } : { count: 5 },
    )

    expect(await communityApi.toggleLike('p', 'u')).toEqual({ liked: true, likesCount: 5 })

    const insert = sb.mock!.calls.find(c => has(c, 'insert'))!
    expect(argsOf(insert, 'insert')).toEqual([[{ post_id: 'p', user_id: 'u' }]])
    const count = sb.mock!.calls[sb.mock!.calls.length - 1]
    expect(argsOf(count, 'select')).toEqual([['*', { count: 'exact', head: true }]])
    expect(argsOf(count, 'eq')).toEqual([['post_id', 'p']])
    expect(sb.mock!.calls.some(c => c.table === 'posts')).toBe(false)
  })

  it('toggleLike: 이미 좋아요면 (post_id, user_id)로 delete한다', async () => {
    sb.mock!.setResult('post_likes', call =>
      has(call, 'maybeSingle') ? { data: { id: 'l1' } } : has(call, 'delete') ? { data: null } : { count: 2 },
    )

    expect(await communityApi.toggleLike('p', 'u')).toEqual({ liked: false, likesCount: 2 })

    const del = sb.mock!.calls.find(c => has(c, 'delete'))!
    expect(argsOf(del, 'eq')).toEqual([['post_id', 'p'], ['user_id', 'u']])
    expect(sb.mock!.calls.some(c => has(c, 'insert'))).toBe(false)
    expect(sb.mock!.calls.some(c => c.table === 'posts')).toBe(false)
  })

  it('toggleLike: 중복 좋아요(23505)는 좋아요 성공으로 본다', async () => {
    sb.mock!.setResult('post_likes', call =>
      has(call, 'maybeSingle') ? { data: null } : has(call, 'insert') ? { error: { code: '23505' } } : { count: 3 },
    )

    expect(await communityApi.toggleLike('p', 'u')).toEqual({ liked: true, likesCount: 3 })
  })

  it('toggleLike: 다른 insert 오류는 던진다', async () => {
    sb.mock!.setResult('post_likes', call =>
      has(call, 'maybeSingle') ? { data: null } : { error: { code: '42501', message: 'rls' } },
    )

    await expect(communityApi.toggleLike('p', 'u')).rejects.toMatchObject({ code: '42501' })
  })

  it('toggleLike: 개수 조회가 실패하면 likesCount는 null(호출 측이 재조회)', async () => {
    sb.mock!.setResult('post_likes', call =>
      has(call, 'maybeSingle') ? { data: null } : has(call, 'insert') ? { data: null } : { error: { message: 'timeout' } },
    )

    expect(await communityApi.toggleLike('p', 'u')).toEqual({ liked: true, likesCount: null })
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

describe('communityApi 글 작성·수정 (P3-10d: 서버 API)', () => {
  const json = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })
  afterEach(() => { vi.unstubAllEnvs() })

  it('createPost/updatePost는 서버 API를 부르고 posts를 직접 쓰지 않는다', async () => {
    vi.stubEnv('VITE_API_URL', 'https://api.example')
    const f = vi.fn(async () => json(201, { success: true, post: row('p1', { images: ['https://api.example/api/storage/post-images/u/1.png'] }) }))
    const created = await communityApi.createPost({ title: 't', content: 'c', images: ['x'] }, f as unknown as typeof fetch)
    expect(created).toMatchObject({ id: 'p1', images: ['https://api.example/api/storage/post-images/u/1.png'] })
    const [url, init] = f.mock.calls[0] as unknown as [string, RequestInit]
    expect(url).toBe('https://api.example/api/posts')
    expect(init.method).toBe('POST')
    expect(JSON.parse(String(init.body))).toEqual({ title: 't', content: 'c', images: ['x'] })

    const g = vi.fn(async () => json(200, { success: true, post: row('p 1') }))
    await communityApi.updatePost('p 1', { title: 'n' }, g as unknown as typeof fetch)
    const [url2, init2] = g.mock.calls[0] as unknown as [string, RequestInit]
    expect(url2).toBe('https://api.example/api/posts/p%201')
    expect(init2.method).toBe('PUT')
    expect(sb.mock!.calls.filter(c => c.table === 'posts')).toEqual([])
  })

  it('toPost: 서버 상대 조회 경로는 API 서버 기준 절대 URL로 (PUBLIC_API_URL 미설정 대비)', () => {
    vi.stubEnv('VITE_API_URL', 'https://api.example/')
    const p = toPost(row('p', { images: ['/api/storage/post-images/u/1.png', 'https://cdn/x.png'] }) as never)
    expect(p.images).toEqual(['https://api.example/api/storage/post-images/u/1.png', 'https://cdn/x.png'])
  })

  it('서버가 거절하면 이유를 담은 PostSaveError', async () => {
    vi.stubEnv('VITE_API_URL', 'https://api.example')
    const f = vi.fn(async () => json(422, { success: false, error: '저장할 수 없는 이미지가 있습니다.', rejected: [{ url: 'https://evil/x.png', reason: '이 서비스에 올린 이미지만 붙일 수 있습니다.' }] }))
    const err = await communityApi.createPost({ title: 't', content: 'c' }, f as unknown as typeof fetch).catch(e => e)
    expect(err).toBeInstanceOf(PostSaveError)
    expect(err.status).toBe(422)
    expect(err.message).toBe('저장할 수 없는 이미지가 있습니다. (이 서비스에 올린 이미지만 붙일 수 있습니다.)')
    expect(err.rejected).toHaveLength(1)
    // 400: 필드별 이유를 보여 준다 (PR #25 M2)
    const invalid = vi.fn(async () => json(400, { success: false, error: '입력값이 올바르지 않습니다.', issues: [
      { field: 'title', message: '제목은(는) 최대 200자까지입니다.' }, { field: 'instrument', message: '악기: 선택할 수 없는 값입니다.' },
    ] }))
    const err400 = await communityApi.updatePost('p', { title: 'x' }, invalid as unknown as typeof fetch).catch(e => e)
    expect(err400).toBeInstanceOf(PostSaveError)
    expect(err400.message).toBe('입력값이 올바르지 않습니다. (제목은(는) 최대 200자까지입니다., 악기: 선택할 수 없는 값입니다.)')
    expect(err400.issues).toHaveLength(2)
    const down = vi.fn(async () => { throw new TypeError('fetch failed') })
    await expect(communityApi.updatePost('p', {}, down as unknown as typeof fetch)).rejects.toThrow('서버에 연결할 수 없습니다')
  })
})
